//! The `debug` TUI: a ratatui-based single-step debugger driven by
//! `Vm::step`.
//!
//! `DebugState` wraps a `Vm` plus the pieces of context the render function
//! needs but `Vm` itself doesn't track: the source text (for the source
//! panel) and the `Interner` (to turn `Symbol`s back into names for
//! disassembly and to render the compiled function's constant pool).
//! `step` is a thin, testable wrapper around `Vm::step` that turns a
//! `RuntimeError` into UI state (`error`) instead of propagating it, so the
//! interactive loop and its tests never have to handle a `Result` — a
//! finished debugger (successful return or runtime error) is just a state
//! to render, not a failure to unwind past.
//!
//! Known simplification: the locals panel shows `slot N: <value>` rather
//! than real source-level variable names. `ember_resolve::Bindings` (the
//! resolver's output, already available where `DebugState` is constructed)
//! only records a *use* site's resolution (`Resolution::Local { slot }` at
//! each `Var` expression) — the name -> slot mapping itself lives in
//! `FunctionCtx`/`Scope`, which are transient per-function structures the
//! resolver pops and discards once it finishes resolving that function, and
//! never surfaces on `Bindings`. Threading a persistent slot -> name table
//! out of the resolver would be a real (and separately reviewable) resolver
//! change, not something to sneak into a CLI-only task, so this first
//! version ships the honest `slot N` fallback and leaves real names as
//! follow-up work.

use std::io::{self, Stdout};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use ember_ast::Interner;
use ember_bytecode::disasm::disassemble_instruction;
use ember_vm::value::display_value;
use ember_vm::vm::{StepOutcome, Vm};

/// Debugger state: a `Vm` plus everything the render function needs but the
/// `Vm` doesn't track itself. Deliberately holds no terminal/ratatui state
/// of its own, so it's constructible and steppable in a unit test without
/// ever touching a real terminal (see the tests below).
pub struct DebugState {
    vm: Vm,
    interner: Interner,
    src: String,
    /// Set once the program has returned or a runtime error occurred.
    /// `step` is a no-op once this is set.
    finished: bool,
    /// The program's final value, once it has returned successfully.
    result: Option<String>,
    /// Set once `step` surfaces a `RuntimeError` — rendered in the status
    /// line rather than silently stopping the debugger.
    error: Option<String>,
}

impl DebugState {
    pub fn new(vm: Vm, interner: Interner, src: String) -> Self {
        DebugState {
            vm,
            interner,
            src,
            finished: false,
            result: None,
            error: None,
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }

    /// Advances the VM by exactly one instruction. A no-op once `finished`
    /// is set. Surfaces a `RuntimeError` into `self.error` rather than
    /// swallowing it — the caller (the render loop, or a test) can inspect
    /// it via `error()`.
    pub fn step(&mut self) {
        if self.finished {
            return;
        }
        match self.vm.step() {
            Ok(StepOutcome::Running) => {}
            Ok(StepOutcome::Done(v)) => {
                self.finished = true;
                self.result = Some(display_value(&v));
            }
            Err(e) => {
                self.finished = true;
                self.error = Some(format!("runtime error at line {}: {}", e.line, e.message));
            }
        }
    }
}

/// Restores the terminal to normal (non-raw, non-alternate-screen) state
/// when dropped — covers every exit path out of `run` below, including a
/// panic mid-render, since `run` unwinds `catch_unwind` around the whole
/// event loop and this guard's `Drop` fires as that unwind passes through
/// its stack frame.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort on the way out: nothing left to report a failure to,
        // and leaving raw mode/alternate screen half-restored would be
        // worse than continuing.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Runs the interactive debug TUI to completion (until the user quits).
/// Enters raw mode + the alternate screen, loops on `crossterm::event::read`
/// stepping the VM on key presses, and unconditionally restores the
/// terminal before returning — including when the render loop panics.
pub fn run(mut state: DebugState) -> io::Result<()> {
    let guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = catch_unwind(AssertUnwindSafe(|| event_loop(&mut terminal, &mut state)));

    // Drop the guard (restoring the terminal) before propagating either
    // outcome, so a panic doesn't leave the user's shell stuck in raw mode
    // even for the duration of unwinding back through this frame.
    drop(guard);

    match result {
        Ok(r) => r,
        Err(payload) => resume_unwind(payload),
    }
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &mut DebugState,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, state))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        // On some terminals a single physical key press is reported as both
        // a `Press` and a `Release` event; only act on `Press` (`Repeat` —
        // held-key auto-repeat — is treated the same as a fresh press) so a
        // key isn't actioned twice.
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('s') | KeyCode::Enter | KeyCode::Char(' ') => state.step(),
            // Run to completion (or the next unhandled error) — a
            // convenience on top of the plan's minimum bar of step + quit.
            KeyCode::Char('r') => {
                // Cap iterations so a runaway/infinite program can't hang
                // the UI forever on one keypress; MAX_FRAMES in ember-vm
                // already bounds recursion depth, not instruction count.
                let mut budget = 10_000_000u64;
                while !state.finished() && budget > 0 {
                    state.step();
                    budget -= 1;
                }
            }
            _ => {}
        }
    }
}

fn render(frame: &mut Frame, state: &DebugState) {
    let area = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[0]);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(columns[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(columns[1]);

    let current_line = current_source_line(state);
    render_source(frame, left[0], state, current_line);
    render_locals(frame, left[1], state);
    render_stack(frame, right[0], state);
    render_next_instruction(frame, right[1], state);
    render_status(frame, outer[1], state);
}

/// The current frame's source line (1-based), from the chunk's own
/// line table — `None` once the program has finished (no current frame).
fn current_source_line(state: &DebugState) -> Option<u32> {
    let frame = state.vm.current_frame()?;
    let chunk = &frame.closure.proto.chunk;
    if frame.ip >= chunk.code.len() {
        return None;
    }
    Some(chunk.line_at(frame.ip))
}

fn render_source(frame: &mut Frame, area: Rect, state: &DebugState, current_line: Option<u32>) {
    let lines: Vec<Line> = state
        .src
        .lines()
        .enumerate()
        .map(|(i, text)| {
            let line_no = (i + 1) as u32;
            let content = format!("{line_no:>4} | {text}");
            if Some(line_no) == current_line {
                Line::from(content).style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(content)
            }
        })
        .collect();

    // Keep the highlighted line roughly centered rather than always
    // scrolling to the top, so stepping through a long function doesn't
    // require manual scrolling to see what's currently executing.
    let height = area.height.saturating_sub(2) as usize; // minus the block's borders
    let scroll = current_line
        .map(|l| (l as usize).saturating_sub(1))
        .map(|idx| {
            idx.saturating_sub(height / 2)
                .min(lines.len().saturating_sub(height.max(1)))
        })
        .unwrap_or(0) as u16;

    let widget = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Source"))
        .scroll((scroll, 0));
    frame.render_widget(widget, area);
}

fn render_stack(frame: &mut Frame, area: Rect, state: &DebugState) {
    let items: Vec<ListItem> = state
        .vm
        .stack()
        .iter()
        .enumerate()
        .rev()
        .map(|(i, v)| ListItem::new(format!("[{i}] {}", display_value(v))))
        .collect();
    let widget = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Value Stack (top first)"),
    );
    frame.render_widget(widget, area);
}

fn render_locals(frame: &mut Frame, area: Rect, state: &DebugState) {
    let items: Vec<ListItem> = match state.vm.current_frame() {
        Some(f) => state
            .vm
            .stack()
            .iter()
            .enumerate()
            .skip(f.slot_base)
            .map(|(i, v)| ListItem::new(format!("slot {}: {}", i - f.slot_base, display_value(v))))
            .collect(),
        None => Vec::new(),
    };
    let widget = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Locals (current frame, by slot — names not tracked past resolve time)"),
    );
    frame.render_widget(widget, area);
}

fn render_next_instruction(frame: &mut Frame, area: Rect, state: &DebugState) {
    let text = match state.vm.current_frame() {
        Some(f) => {
            let chunk = &f.closure.proto.chunk;
            if f.ip < chunk.code.len() {
                let (line, instr, _next) = disassemble_instruction(chunk, f.ip, &state.interner);
                format!("ip={:04}  line {line}\n{instr}", f.ip)
            } else {
                "(frame about to return)".to_string()
            }
        }
        None => "(no active frame)".to_string(),
    };
    let widget = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Next Instruction"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn render_status(frame: &mut Frame, area: Rect, state: &DebugState) {
    let (text, style) = if let Some(err) = state.error() {
        (
            format!("ERROR: {err}   [q] quit"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else if let Some(result) = state.result() {
        (
            format!("FINISHED: {result}   [q] quit"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "[s/Enter/Space] step   [r] run to completion   [q/Esc] quit".to_string(),
            Style::default(),
        )
    };
    let widget = Paragraph::new(Span::styled(text, style))
        .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `DebugState` from source via the same
    /// parse/resolve/infer/exhaustiveness/compile pipeline `main.rs` uses
    /// for every other command — exercised directly here (no terminal, no
    /// event loop) so `DebugState::step`'s VM-driving logic is covered by
    /// a fast, non-interactive test.
    fn debug_state_for(src: &str) -> DebugState {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(
            resolve_diags.is_empty(),
            "resolve errors: {resolve_diags:?}"
        );
        let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        assert!(infer_diags.is_empty(), "infer errors: {infer_diags:?}");
        let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert!(
            exhaustive_diags.is_empty(),
            "exhaustiveness errors: {exhaustive_diags:?}"
        );
        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let vm = Vm::new(proto);
        DebugState::new(vm, interner, src.to_string())
    }

    #[test]
    fn steps_a_simple_arithmetic_program_to_completion() {
        let mut state = debug_state_for("1 + 2 * 3;");
        assert!(!state.finished());
        let mut steps = 0;
        while !state.finished() {
            state.step();
            steps += 1;
            assert!(steps < 100, "runaway stepping, never finished");
        }
        assert_eq!(state.error(), None);
        assert_eq!(state.result(), Some("7"));
    }

    #[test]
    fn stepping_after_finished_is_a_no_op() {
        let mut state = debug_state_for("42;");
        while !state.finished() {
            state.step();
        }
        let result_before = state.result().map(str::to_string);
        state.step();
        state.step();
        assert_eq!(state.result().map(str::to_string), result_before);
    }

    #[test]
    fn stack_and_locals_reflect_intermediate_state_mid_program() {
        let mut state = debug_state_for("let x = 10;\nlet y = 20;\nx + y;");
        // `Vm::new` pre-pushes the 8 native globals onto the physical stack
        // (see `ember_vm::vm::NATIVE_GLOBAL_COUNT`'s doc comment) before any
        // user code runs, so "two more locals landed" means "stack grew by
        // two from wherever it started", not "stack length is 2".
        let initial_len = state.vm.stack().len();
        // Step until both `let`s have pushed their values onto the stack —
        // exercises `Vm::stack`/`Vm::current_frame` through `DebugState`
        // the same way the locals/stack panels read them.
        let mut steps = 0;
        while state.vm.stack().len() < initial_len + 2 && !state.finished() {
            state.step();
            steps += 1;
            assert!(steps < 100, "never reached two locals on the stack");
        }
        assert!(!state.finished());
        let frame = state.vm.current_frame().expect("frame active mid-program");
        assert_eq!(frame.slot_base, 0);
        assert_eq!(state.vm.stack().len(), initial_len + 2);

        while !state.finished() {
            state.step();
        }
        assert_eq!(state.error(), None);
        assert_eq!(state.result(), Some("30"));
    }

    #[test]
    fn runtime_error_is_surfaced_not_swallowed() {
        // Division by zero — a genuine `RuntimeError` from the VM, not a
        // compile-time diagnostic, so this only surfaces once stepping
        // reaches the `Div` instruction.
        let mut state = debug_state_for("1 / 0;");
        let mut steps = 0;
        while !state.finished() {
            state.step();
            steps += 1;
            assert!(steps < 100, "runaway stepping, never finished");
        }
        assert_eq!(state.result(), None);
        assert!(state.error().is_some(), "expected a surfaced runtime error");
    }
}
