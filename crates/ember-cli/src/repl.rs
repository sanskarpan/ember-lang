//! The REPL session: incremental parse/resolve/infer against a growing
//! source buffer, executed one entry at a time on either backend.
//!
//! Each entry is handled by re-`parse_into`-ing the *whole* accumulated
//! buffer against the session's single, persistent `Interner` (so the same
//! source identifier always interns to the same `Symbol` across entries),
//! then running only the newly-appended statements (`stmts[stmt_count..]`).
//! On any parse/resolve/infer diagnostic at error severity, the entry is
//! rejected and the buffer append is rolled back, so a bad entry never
//! leaves the session in a half-committed state.
//!
//! `handle_entry` returns what it would print (`Option<String>`) rather than
//! printing directly, so both backends are testable without capturing
//! stdout; the real interactive loop (`run_repl` in `main.rs`) prints
//! whatever it returns.
//!
//! The tree-walker backend (`Backend::Tree`) runs each entry's new
//! statements against a persistent `ember_tree::env::Env`. The VM backend
//! (`Backend::Vm`) takes a different path: each entry is *independently*
//! resolved (seeding prior entries' top-level names as known globals via
//! `Resolver::seed_repl_globals`) and compiled, then executed against a
//! persistent `ember_vm::vm::Vm` via `run_incremental`, which reuses the
//! VM's existing `globals`/`gc` state but resets its physical stack/frames
//! per entry.
//!
//! Reusing the whole-buffer resolve's `Bindings` (the broad resolve already
//! run earlier in `handle_entry` for diagnostic validation) to compile just
//! the new statements was tried and produces a real panic: in a single
//! whole-buffer resolve pass, an earlier statement's name resolves as
//! `Resolution::Local` (continuous slot numbering across the whole buffer)
//! rather than `Resolution::Global`, so the compiled tail emits
//! `OP_GET_LOCAL` for a slot that only ever existed in a different,
//! already-finished `run()` call — `run_incremental`'s freshly-reset
//! physical stack doesn't have it. The narrow, second resolve of only the
//! new statements (below) is the only correct design: it lets
//! `seed_repl_globals` do its job and everything resolve as `Global`.

use ember_ast::{Interner, Stmt};

use crate::{use_color, Backend};

/// A REPL session's persistent state: everything that must survive across
/// entries. `interner`/`buffer`/`stmt_count` are backend-agnostic; `tree_env`
/// is the tree-walker's persistent top-level environment; `vm` and
/// `repl_global_symbols` are the VM backend's persistent state.
///
/// Deliberately does NOT hold an `ember_tree::interp::Interp` field: `Interp<'a>`
/// borrows `&'a Ast`, and every entry produces a brand new `Ast` (from
/// re-parsing the growing buffer). An `Interp` is therefore constructed fresh
/// inside `handle_entry`, borrowing that entry's own `Ast` for exactly as
/// long as that entry's `exec_stmt` calls need it.
pub struct ReplSession {
    interner: Interner,
    buffer: String,
    stmt_count: usize,
    tree_env: std::rc::Rc<std::cell::RefCell<ember_tree::env::Env>>,
    /// The VM backend's persistent `Vm`, created lazily on the first entry
    /// handled with `Backend::Vm` (there's nothing to construct it from
    /// until then — `Vm::new` needs an initial `FunctionProto`, which only
    /// exists once the first entry has been compiled).
    vm: Option<ember_vm::vm::Vm>,
    /// Every name ever top-level-declared (`let`/`fn`) by a prior VM-backend
    /// entry, so a later entry's independent resolve can seed them as known
    /// globals via `Resolver::seed_repl_globals`. Unused by `Backend::Tree`.
    repl_global_symbols: Vec<ember_ast::Symbol>,
    backend: Backend,
    show_types: bool,
}

impl ReplSession {
    pub fn new(backend: Backend, show_types: bool) -> Self {
        ReplSession {
            interner: Interner::new(),
            buffer: String::new(),
            stmt_count: 0,
            tree_env: ember_tree::env::Env::new(),
            vm: None,
            repl_global_symbols: Vec::new(),
            backend,
            show_types,
        }
    }

    /// Renders each diagnostic against the session's accumulated buffer
    /// (there's no real file on disk, so `"<repl>"` stands in for a
    /// filename) and joins them into one string, mirroring the top-level
    /// CLI's `print_diagnostics` but reused here for the REPL's virtual
    /// buffer and its return-value-based `handle_entry`.
    fn render_diags(&self, diags: &[ember_diag::Diagnostic]) -> String {
        Self::render_diags_against(diags, &self.buffer)
    }

    /// Same rendering `render_diags` does, but against an explicit buffer
    /// rather than `self.buffer` — needed by the `:type`/`:ast`/`:disasm`
    /// meta-commands, which parse against a *copy* of the buffer (see
    /// `parse_in_context`) and must render any diagnostic against that same
    /// copy, since a diagnostic's span is an offset into whichever source
    /// text it was produced from.
    fn render_diags_against(diags: &[ember_diag::Diagnostic], buffer: &str) -> String {
        diags
            .iter()
            .map(|d| ember_diag::render::render(d, "<repl>", buffer, use_color()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Undoes this entry's append to `self.buffer` after a stage rejected it
    /// (via a diagnostic at error severity) before anything from this entry
    /// ran or was committed to `self.stmt_count`.
    fn rollback(&mut self, input: &str) {
        self.buffer.truncate(self.buffer.len() - input.len() - 1);
    }

    /// Handles one REPL entry: a single (possibly multi-line) piece of
    /// source text already assembled by the REPL loop's bracket-balance
    /// continuation logic. Returns what should be printed for this entry
    /// (a rendered diagnostic, or the last statement's resulting value),
    /// or `None` if there's nothing to print.
    pub fn handle_entry(&mut self, input: &str) -> Option<String> {
        if let Some(rest) = input.trim().strip_prefix(':') {
            return self.handle_meta_command(rest);
        }

        self.buffer.push_str(input);
        self.buffer.push('\n');

        let (ast, stmts, parse_diags) = ember_parser::parse_into(&self.buffer, &mut self.interner);
        if !parse_diags.is_empty() {
            let out = self.render_diags(&parse_diags);
            self.rollback(input);
            return Some(out);
        }

        let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut self.interner, &stmts);
        if resolve_diags
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error)
        {
            let out = self.render_diags(&resolve_diags);
            self.rollback(input);
            return Some(out);
        }

        let (mut info, infer_diags) = ember_types::infer(&ast, &mut self.interner, &stmts);
        if infer_diags
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error)
        {
            let out = self.render_diags(&infer_diags);
            self.rollback(input);
            return Some(out);
        }

        let new_stmts = &stmts[self.stmt_count..];
        if new_stmts.is_empty() {
            // Parsed cleanly but produced no new top-level statements (e.g.
            // a blank line, or a line that was pure comment/whitespace) —
            // nothing to execute, but the buffer append is real source text
            // that should stay, so commit and stop, no rollback.
            self.stmt_count = stmts.len();
            return None;
        }

        match self.backend {
            Backend::Tree => self.run_tree(&ast, new_stmts, &stmts, &mut info),
            Backend::Vm => self.run_vm(&ast, new_stmts, &stmts, input),
        }
    }

    /// Runs `new_stmts` (this entry's newly-parsed statements) against the
    /// session's persistent `tree_env`, then returns the last statement's
    /// resulting value (with its inferred type appended, if `show_types` is
    /// set and the last statement is a bare expression statement with a
    /// recorded type) as a printable string.
    fn run_tree(
        &mut self,
        ast: &ember_ast::Ast,
        new_stmts: &[ember_ast::Idx<Stmt>],
        all_stmts: &[ember_ast::Idx<Stmt>],
        info: &mut ember_types::TypeInfo,
    ) -> Option<String> {
        let mut interp = ember_tree::interp::Interp::new(ast, &self.interner);
        let mut last = None;
        for &s in new_stmts {
            match interp.exec_stmt(s, &self.tree_env) {
                Ok(ember_tree::Flow::Normal(v)) => last = Some(v),
                // A top-level `return`/`break`/`continue` is unusual (there's
                // no enclosing function/loop to return from or break out of
                // at the REPL's top level) but not a reason to crash the
                // session: treat a bare top-level `return <expr>` as
                // producing that expr's value, same as an ordinary
                // expression statement would, and a stray `break`/`continue`
                // as producing nothing printable.
                Ok(ember_tree::Flow::Return(v)) => last = Some(v),
                Ok(ember_tree::Flow::Break) | Ok(ember_tree::Flow::Continue) => last = None,
                Err(e) => {
                    // The statements that already ran before this error may
                    // have declared names into `self.tree_env` — commit the
                    // buffer/stmt_count regardless, matching what running a
                    // real top-level script partway would leave behind, so a
                    // later entry referencing those names still works.
                    self.stmt_count = all_stmts.len();
                    return Some(self.render_diags(&[e.to_diagnostic()]));
                }
            }
        }
        self.stmt_count = all_stmts.len();

        let v = last?;
        let mut out = ember_tree::display_value(&v, &self.interner);
        if self.show_types {
            if let Some(&last_idx) = new_stmts.last() {
                if let Stmt::ExprStmt(e) = ast.stmt(last_idx) {
                    if let Some(ty) = info.expr_types.get(e).cloned() {
                        let ty_str = ember_types::display_ty(
                            &ty,
                            &mut info.subst,
                            &info.adts,
                            &self.interner,
                        );
                        out.push_str(&format!(" : {ty_str}"));
                    }
                }
            }
        }
        Some(out)
    }

    /// Runs `new_stmts` on the VM backend: independently resolves just
    /// these statements (seeding every name a prior entry declared as a
    /// known global via `seed_repl_globals`, so they resolve as `Global`
    /// rather than clashing with this entry's own fresh local slots),
    /// compiles them alone, then executes the result against the session's
    /// persistent `Vm` via `run_incremental` (or, on the very first VM
    /// entry, constructs that `Vm` fresh via `Vm::new` and runs it directly
    /// — equivalent to `run_incremental` for a brand new `Vm`, since a
    /// fresh `Vm` already has empty stack/frames/open_upvalues, but avoids
    /// needing `FunctionProto: Clone`, which it isn't).
    ///
    /// See this module's doc comment for why this entry must be resolved on
    /// its own, separately from the whole-buffer resolve already run in
    /// `handle_entry` for diagnostic validation, rather than reusing that
    /// resolve's `Bindings` to compile `new_stmts` — the latter panics.
    fn run_vm(
        &mut self,
        ast: &ember_ast::Ast,
        new_stmts: &[ember_ast::Idx<Stmt>],
        all_stmts: &[ember_ast::Idx<Stmt>],
        input: &str,
    ) -> Option<String> {
        let mut resolver = ember_resolve::Resolver::new(ast, &mut self.interner);
        resolver.seed_repl_globals(&self.repl_global_symbols);
        resolver.resolve_program(new_stmts);
        let has_error = resolver
            .diagnostics()
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error);
        if has_error {
            // Clone the diagnostics and drop `resolver` before calling
            // `self.render_diags`: `resolver` holds `&mut self.interner`
            // for its whole lifetime, which would otherwise still be
            // borrowed here, conflicting with `render_diags`'s `&self`.
            let diags = resolver.diagnostics().to_vec();
            drop(resolver);
            let out = self.render_diags(&diags);
            self.rollback(input);
            return Some(out);
        }
        let (new_bindings, _) = resolver.into_bindings();
        let proto = ember_compile::compile(ast, &mut self.interner, &new_bindings, new_stmts);

        let result = if let Some(vm) = self.vm.as_mut() {
            vm.run_incremental(proto)
        } else {
            let mut vm = ember_vm::vm::Vm::new(proto);
            let result = vm.run();
            self.vm = Some(vm);
            result
        };

        self.stmt_count = all_stmts.len();

        match result {
            Ok(v) => {
                for &s in new_stmts {
                    match ast.stmt(s) {
                        Stmt::Let { name, .. } | Stmt::Fn { name, .. } => {
                            self.repl_global_symbols.push(*name);
                        }
                        _ => {}
                    }
                }
                Some(ember_vm::value::display_value(&v))
            }
            Err(e) => Some(self.render_diags(&[e.to_diagnostic(&self.interner)])),
        }
    }

    /// Dispatches a `:`-prefixed REPL meta-command (the leading `:` already
    /// stripped by `handle_entry`). `command` is `"<name> <arg>"` or just
    /// `"<name>"` with no argument.
    ///
    /// `:reset` and `:load` are the two "real" commands — they replace or
    /// extend the session's actual state, `:load` by feeding a file's
    /// contents through the exact same `handle_entry` path a typed entry
    /// takes (so it's not a special case: declarations in a loaded file
    /// become visible to later entries exactly as if they'd been typed).
    ///
    /// `:type`/`:ast`/`:disasm` are the three "don't commit" commands: each
    /// parses `arg` via `parse_in_context`, which operates on *copies* of
    /// `self.buffer`/`self.interner` and never writes anything back —
    /// `self.buffer`/`self.stmt_count`/`self.interner` are left completely
    /// unaffected by running any of them.
    fn handle_meta_command(&mut self, command: &str) -> Option<String> {
        let (name, arg) = command.split_once(' ').unwrap_or((command, ""));
        let arg = arg.trim();
        match name {
            "reset" => {
                *self = ReplSession::new(self.backend, self.show_types);
                Some("session reset".to_string())
            }
            "load" => match std::fs::read_to_string(arg) {
                Ok(contents) => self.handle_entry(&contents),
                Err(e) => Some(format!("error: could not read {arg}: {e}")),
            },
            "type" => match self.parse_in_context(arg) {
                Ok(mut ctx) => {
                    let &last = ctx
                        .new_stmts
                        .last()
                        .expect("parse_in_context guarantees new_stmts is non-empty");
                    match ctx.ast.stmt(last) {
                        Stmt::ExprStmt(e) => match ctx.info.expr_types.get(e).cloned() {
                            Some(ty) => Some(ember_types::display_ty(
                                &ty,
                                &mut ctx.info.subst,
                                &ctx.info.adts,
                                &ctx.interner,
                            )),
                            None => Some("<no type recorded for this expression>".to_string()),
                        },
                        _ => Some(":type only works on an expression".to_string()),
                    }
                }
                Err(out) => Some(out),
            },
            "ast" => match self.parse_in_context(arg) {
                Ok(ctx) => Some(
                    ctx.new_stmts
                        .iter()
                        .map(|&s| ember_ast::print_stmt(&ctx.ast, &ctx.interner, s))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(out) => Some(out),
            },
            "disasm" => match self.parse_in_context(arg) {
                Ok(ctx) => {
                    let ParsedInContext {
                        ast,
                        mut interner,
                        prior_stmts,
                        new_stmts,
                        buffer,
                        ..
                    } = ctx;
                    // Compiling requires the same narrow, independent resolve
                    // `run_vm` uses (seeding prior entries' top-level names as
                    // known globals) rather than the whole-buffer `Bindings`
                    // `parse_in_context` already computed for `:type`/`:ast`
                    // — see this module's doc comment for why reusing a
                    // whole-buffer resolve to compile only a tail slice
                    // panics (stale local slot numbers).
                    //
                    // The seed list is derived directly from `prior_stmts`
                    // (every top-level `let`/`fn` already committed to
                    // `self.buffer`) rather than reused from
                    // `self.repl_global_symbols`, which the VM backend's
                    // `run_vm` populates but the tree-walker backend's
                    // `run_tree` never touches — deriving it here keeps
                    // `:disasm` correct for a tree-walker-backend session
                    // too, where a name like `x` from an earlier `let x = 5;`
                    // entry is only ever recorded in `self.tree_env`, never
                    // in `self.repl_global_symbols`.
                    let mut prior_global_symbols = Vec::new();
                    for &s in &prior_stmts {
                        match ast.stmt(s) {
                            Stmt::Let { name, .. } | Stmt::Fn { name, .. } => {
                                prior_global_symbols.push(*name);
                            }
                            _ => {}
                        }
                    }
                    let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
                    resolver.seed_repl_globals(&prior_global_symbols);
                    resolver.resolve_program(&new_stmts);
                    let has_error = resolver
                        .diagnostics()
                        .iter()
                        .any(|d| d.severity == ember_diag::Severity::Error);
                    if has_error {
                        let diags = resolver.diagnostics().to_vec();
                        drop(resolver);
                        return Some(Self::render_diags_against(&diags, &buffer));
                    }
                    let (bindings, _) = resolver.into_bindings();
                    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &new_stmts);
                    Some(crate::disassemble_recursively(
                        &proto.chunk,
                        "repl",
                        &interner,
                    ))
                }
                Err(out) => Some(out),
            },
            other => Some(format!("unknown command: :{other}")),
        }
    }

    /// Parses `arg` as if it were a normal entry appended after everything
    /// already in the session, but against *copies* of `self.buffer` and
    /// `self.interner` — nothing this produces is ever written back to
    /// `self`. Backs the three "don't commit" meta-commands (`:type`,
    /// `:ast`, `:disasm`).
    ///
    /// Cloning `self.interner` is safe: `Interner::clone` duplicates its
    /// backing arena and dedup map, and interning is idempotent/append-only,
    /// so any symbol the clone interns while parsing `arg` (and the real
    /// session `Interner` never sees, since the clone is discarded
    /// afterward) can never invalidate a `Symbol` the real session already
    /// handed out.
    ///
    /// Returns `Err(rendered diagnostics)` on any parse/resolve/infer error
    /// (or if `arg` produced no new statement at all — an empty argument, or
    /// pure whitespace/comment), otherwise `Ok` of everything the callers
    /// project from: the parsed `Ast`, the `Interner` it's indexed against,
    /// the inferred types, the new statement(s) `arg` produced, and the
    /// exact buffer text they were parsed against (needed to render any
    /// *later* diagnostic, e.g. from `:disasm`'s own extra resolve, at the
    /// right span).
    fn parse_in_context(&self, arg: &str) -> Result<ParsedInContext, String> {
        let mut interner = self.interner.clone();
        let mut buffer = self.buffer.clone();
        buffer.push_str(arg);
        // `:type`/`:ast`/`:disasm` take a bare expression (`:type x`, not
        // `:type x;`) — the parser only accepts statements, so make sure
        // one terminates the buffer regardless of what the user typed.
        if !arg.trim_end().ends_with(';') {
            buffer.push(';');
        }
        buffer.push('\n');

        let (ast, stmts, parse_diags) = ember_parser::parse_into(&buffer, &mut interner);
        if !parse_diags.is_empty() {
            return Err(Self::render_diags_against(&parse_diags, &buffer));
        }

        let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        if resolve_diags
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error)
        {
            return Err(Self::render_diags_against(&resolve_diags, &buffer));
        }

        let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        if infer_diags
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error)
        {
            return Err(Self::render_diags_against(&infer_diags, &buffer));
        }

        let prior_stmts = stmts[..self.stmt_count].to_vec();
        let new_stmts = stmts[self.stmt_count..].to_vec();
        if new_stmts.is_empty() {
            return Err("nothing to show".to_string());
        }

        Ok(ParsedInContext {
            ast,
            interner,
            info,
            prior_stmts,
            new_stmts,
            buffer,
        })
    }
}

/// Everything `parse_in_context` produces: the AST parsed from a *copy* of
/// the session's buffer plus the meta-command's argument, the `Interner`
/// copy it's indexed against, the inferred types, the previously-committed
/// statements (everything before `arg`) and the new statement(s) `arg`
/// itself produced, and the exact buffer text it was parsed against.
struct ParsedInContext {
    ast: ember_ast::Ast,
    interner: Interner,
    info: ember_types::TypeInfo,
    prior_stmts: Vec<ember_ast::Idx<Stmt>>,
    new_stmts: Vec<ember_ast::Idx<Stmt>>,
    buffer: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_backend_persists_state_across_entries_without_replaying_output() {
        let mut session = ReplSession::new(Backend::Vm, false);
        // A bare `let` statement's "value" is `Nil` (mirrors the compiler's
        // and the tree-walker's own existing, pre-established convention —
        // see `compile`'s non-hoisted-statement loop and
        // `exec_stmt_uninstrumented`'s `Stmt::Let` arm), so these first two
        // entries each print `nil`, not nothing.
        assert_eq!(session.handle_entry("let x = 5;").as_deref(), Some("nil"));
        assert_eq!(
            session.handle_entry("let y = x + 1;").as_deref(),
            Some("nil")
        );
        let out = session.handle_entry("x + y;");
        assert_eq!(
            out.as_deref(),
            Some("11"),
            "the third entry should print only its own result (5 + 6 = 11), \
             not a re-print of either prior entry's output"
        );
    }

    #[test]
    fn tree_backend_persists_state_across_entries_without_replaying_output() {
        let mut session = ReplSession::new(Backend::Tree, false);
        assert_eq!(session.handle_entry("let x = 5;").as_deref(), Some("nil"));
        assert_eq!(
            session.handle_entry("let y = x + 1;").as_deref(),
            Some("nil")
        );
        let out = session.handle_entry("x + y;");
        assert_eq!(out.as_deref(), Some("11"));
    }

    #[test]
    fn reset_clears_prior_session_state() {
        let mut session = ReplSession::new(Backend::Tree, false);
        session.handle_entry("let x = 5;");
        assert_eq!(session.stmt_count, 1);
        assert!(!session.buffer.is_empty());

        let msg = session.handle_entry(":reset");
        assert_eq!(msg.as_deref(), Some("session reset"));
        assert_eq!(session.stmt_count, 0);
        assert_eq!(session.buffer, "");

        // `x` was only ever declared before the reset — referencing it now
        // must be an undeclared-name diagnostic, not a resolution to the
        // pre-reset value.
        let out = session
            .handle_entry("x;")
            .expect("referencing a name only declared before :reset should print a diagnostic");
        assert!(
            !out.contains('5'),
            "must not still see the pre-reset value of x; got: {out}"
        );
    }

    #[test]
    fn load_executes_a_files_contents_through_the_same_path_as_typed_input() {
        let mut session = ReplSession::new(Backend::Tree, false);
        let path = std::env::temp_dir().join(format!(
            "ember_repl_load_test_{}_{}.em",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "let loaded = 42;\n").expect("failed to write temp file");

        let out = session.handle_entry(&format!(":load {}", path.display()));
        assert_eq!(
            out.as_deref(),
            Some("nil"),
            ":load should run the file's contents through handle_entry, whose \
             own last-statement-value convention prints a bare `let` as `nil`"
        );

        // The loaded file's declaration is now visible to a subsequent
        // ordinary entry — exactly as if `let loaded = 42;` had been typed
        // directly.
        let out2 = session.handle_entry("loaded;");
        assert_eq!(out2.as_deref(), Some("42"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_reports_an_error_for_a_missing_file() {
        let mut session = ReplSession::new(Backend::Tree, false);
        let out = session
            .handle_entry(":load /this/path/almost/certainly/does/not/exist.em")
            .expect("a missing file should still print something");
        assert!(out.starts_with("error:"), "got: {out}");
    }

    #[test]
    fn type_ast_disasm_do_not_mutate_session_state() {
        let mut session = ReplSession::new(Backend::Tree, false);
        session.handle_entry("let x = 5;");
        let buffer_before = session.buffer.clone();
        let stmt_count_before = session.stmt_count;

        let type_out = session.handle_entry(":type x + 1;");
        assert_eq!(type_out.as_deref(), Some("Int"));
        assert_eq!(session.buffer, buffer_before, ":type must not touch buffer");
        assert_eq!(
            session.stmt_count, stmt_count_before,
            ":type must not touch stmt_count"
        );

        let ast_out = session.handle_entry(":ast x + 1;");
        assert!(ast_out.is_some());
        assert_eq!(session.buffer, buffer_before, ":ast must not touch buffer");
        assert_eq!(
            session.stmt_count, stmt_count_before,
            ":ast must not touch stmt_count"
        );

        let disasm_out = session.handle_entry(":disasm x + 1;");
        assert!(disasm_out.is_some());
        let disasm_text = disasm_out.unwrap();
        assert!(
            disasm_text.starts_with("== repl =="),
            "disassembly should start with disassemble_chunk's own '== <name> ==' \
             header, not a diagnostic (a prior regression here: :disasm failed to \
             resolve `x`, a name declared by this Tree-backend session, because it \
             looked it up in `self.repl_global_symbols`, which only the VM backend's \
             run_vm ever populates); got: {disasm_text}"
        );
        assert!(
            disasm_text.contains("OP_"),
            "disassembly should contain real opcodes; got: {disasm_text}"
        );
        assert_eq!(
            session.buffer, buffer_before,
            ":disasm must not touch buffer"
        );
        assert_eq!(
            session.stmt_count, stmt_count_before,
            ":disasm must not touch stmt_count"
        );

        // A subsequent normal entry still sees exactly the pre-meta-command
        // state — `x` is still 5, undisturbed by any of the three discarded
        // parses above (which each interned/parsed against their own throwaway
        // copies of the buffer and interner, not the real session state).
        let out = session.handle_entry("x;");
        assert_eq!(out.as_deref(), Some("5"));
    }

    #[test]
    fn type_ast_disasm_on_the_vm_backend_also_do_not_mutate_session_state() {
        let mut session = ReplSession::new(Backend::Vm, false);
        session.handle_entry("let x = 5;");
        let buffer_before = session.buffer.clone();
        let stmt_count_before = session.stmt_count;
        let globals_before = session.repl_global_symbols.clone();

        session.handle_entry(":type x + 1;");
        session.handle_entry(":ast x + 1;");
        session.handle_entry(":disasm x + 1;");

        assert_eq!(session.buffer, buffer_before);
        assert_eq!(session.stmt_count, stmt_count_before);
        assert_eq!(session.repl_global_symbols, globals_before);

        let out = session.handle_entry("x;");
        assert_eq!(out.as_deref(), Some("5"));
    }

    #[test]
    fn unknown_meta_command_reports_an_error_without_mutating_state() {
        let mut session = ReplSession::new(Backend::Tree, false);
        session.handle_entry("let x = 5;");
        let buffer_before = session.buffer.clone();
        let stmt_count_before = session.stmt_count;

        let out = session
            .handle_entry(":nonsense")
            .expect("an unknown meta-command should still print something");
        assert!(out.starts_with("unknown command"), "got: {out}");
        assert_eq!(session.buffer, buffer_before);
        assert_eq!(session.stmt_count, stmt_count_before);
    }
}
