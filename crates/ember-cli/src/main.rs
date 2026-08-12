use clap::{CommandFactory, Parser as ClapParser, Subcommand};
use std::fs;
use std::process::ExitCode;

mod debug_tui;
mod repl;

#[derive(ClapParser)]
#[command(name = "ember")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Which execution backend `run` should use.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Backend {
    /// The tree-walking interpreter.
    Tree,
    /// Compile to bytecode and run it on the register/stack VM.
    Vm,
}

#[derive(Subcommand)]
enum Command {
    /// Print the token stream with spans.
    Tokens { file: String },
    /// Print the parsed AST (pretty-printed source form).
    Ast {
        file: String,
        /// Serialize the full parsed AST as JSON instead of printing its
        /// pretty-printed source form.
        #[arg(long)]
        json: bool,
        /// Annotate each top-level expression statement with its inferred
        /// type (`<stmt> : <type>`), running full type inference first.
        #[arg(long)]
        typed: bool,
    },
    /// Print each top-level fn's inferred, generalized type scheme, and any
    /// type diagnostics.
    Types { file: String },
    /// Parse, resolve, typecheck, check exhaustiveness, then actually run the
    /// program on the chosen backend, printing its final value or a
    /// rendered diagnostic.
    Run {
        file: String,
        /// Which backend to execute on.
        #[arg(long, value_enum, default_value_t = Backend::Tree)]
        backend: Backend,
        /// Print elapsed wall-clock execution time to stderr.
        #[arg(long)]
        time: bool,
        /// Force a GC collection on every allocation (VM backend only).
        #[arg(long)]
        gc_stress: bool,
    },
    /// Format a file. Rewrites it in place by default; with `--check`,
    /// reports whether it's already formatted without writing, exiting
    /// non-zero if not.
    Fmt {
        file: String,
        #[arg(long)]
        check: bool,
    },
    /// Parse, resolve, typecheck, and check exhaustiveness, printing any
    /// diagnostics without ever executing the program.
    Check { file: String },
    /// Compile a file and print the disassembled bytecode for the
    /// top-level chunk and every nested function's chunk, recursively.
    Disasm { file: String },
    /// Generate a shell completion script.
    #[command(hide = true)]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print a long-form explanation of a diagnostic code, e.g. `E0301`.
    Explain { code: String },
    /// Print the full inference derivation: every unification attempted
    /// during type inference, in the order it was attempted, with its
    /// origin and pass/fail outcome.
    Trace { file: String },
    /// Run a program on both backends, printing elapsed time for each, the
    /// VM's allocation stats, and the tree-walker/VM speedup ratio.
    Bench { file: String },
    /// Start an interactive REPL: entries are parsed/resolved/type-checked
    /// incrementally against a persistent session and executed one at a
    /// time.
    Repl {
        /// Which backend to execute entries on.
        #[arg(long, value_enum, default_value_t = Backend::Tree)]
        backend: Backend,
        /// Print each printed value's inferred type alongside it.
        #[arg(long)]
        show_types: bool,
    },
    /// Compile a file and step through its bytecode interactively in a
    /// `ratatui` TUI: source/stack/locals/next-instruction panels driven by
    /// `Vm::step`.
    Debug { file: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Tokens { file } => run_tokens(&file),
        Command::Ast { file, json, typed } => run_ast(&file, json, typed),
        Command::Types { file } => run_types(&file),
        Command::Run {
            file,
            backend,
            time,
            gc_stress,
        } => run_run(&file, backend, time, gc_stress),
        Command::Fmt { file, check } => run_fmt(&file, check),
        Command::Check { file } => run_check(&file),
        Command::Disasm { file } => run_disasm(&file),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            ExitCode::SUCCESS
        }
        Command::Explain { code } => run_explain(&code),
        Command::Trace { file } => run_trace(&file),
        Command::Bench { file } => run_bench(&file),
        Command::Repl {
            backend,
            show_types,
        } => run_repl(backend, show_types),
        Command::Debug { file } => run_debug(&file),
    }
}

fn read_source(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("error: could not read {path}: {e}");
            None
        }
    }
}

/// Whether diagnostic/output rendering should use ANSI color: respects
/// `NO_COLOR` and falls back to no color when stdout isn't a terminal (e.g.
/// piped output, CI logs).
fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout())
}

fn run_tokens(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (tokens, _trivia, diags) = ember_lexer::lex(&src);
    for t in &tokens {
        let text = &src[t.span.start as usize..t.span.end as usize];
        println!("{:?}\t{}..{}\t{:?}", t.kind, t.span.start, t.span.end, text);
    }
    print_diagnostics(&diags, path, &src)
}

fn run_ast(path: &str, json: bool, typed: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, diags) = ember_parser::parse(&src);

    if json {
        // Serialize the whole `Ast` (every arena: exprs, stmts, pats,
        // type_exprs, plus their spans) rather than projecting just the
        // requested top-level `stmts`. A `Stmt`/`Expr` only ever references
        // its children by `Idx<T>` (an arena index), never inline — so
        // serializing a `Vec<&Stmt>` alone would show e.g. a `Let`'s
        // initializer as a bare integer index with no way to look up what it
        // points to. Serializing the whole `Ast` keeps every arena entry
        // available (cross-referenced by index, same trade-off `Idx<T>` and
        // `Symbol` already make: valid, complete JSON, not fully "inlined"
        // human prose) so nothing is silently missing from the output.
        let rendered = serde_json::to_string_pretty(&ast)
            .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
        println!("{rendered}");
        return print_diagnostics(&diags, path, &src);
    }

    if typed {
        if !diags.is_empty() {
            return print_diagnostics(&diags, path, &src);
        }
        let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        if resolve_diags
            .iter()
            .any(|d| d.severity == ember_diag::Severity::Error)
        {
            return print_diagnostics(&resolve_diags, path, &src);
        }
        let (mut info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        for s in &stmts {
            println!("{}", print_stmt_typed(&ast, &interner, &mut info, *s));
        }
        return print_diagnostics(&infer_diags, path, &src);
    }

    for s in &stmts {
        println!("{}", ember_ast::print_stmt(&ast, &interner, *s));
    }
    print_diagnostics(&diags, path, &src)
}

/// Pretty-prints a top-level statement the same way `ember_ast::print_stmt`
/// does, appending `: <type>` when the statement is a bare expression
/// statement whose expression got an inferred type recorded in `info`
/// (`ast --typed`). Other statement kinds (`let`, `fn`, `type`, ...) print
/// unannotated, matching the plan's "per top-level expression statement"
/// scope — `ember_ast::print_stmt` has no type-annotation-aware variant of
/// its own to reuse.
fn print_stmt_typed(
    ast: &ember_ast::Ast,
    interner: &ember_ast::Interner,
    info: &mut ember_types::TypeInfo,
    idx: ember_ast::Idx<ember_ast::Stmt>,
) -> String {
    let text = ember_ast::print_stmt(ast, interner, idx);
    let ember_ast::Stmt::ExprStmt(e) = ast.stmt(idx) else {
        return text;
    };
    let Some(ty) = info.expr_types.get(e).cloned() else {
        return text;
    };
    let ty_str = ember_types::display_ty(&ty, &mut info.subst, &info.adts, interner);
    format!("{text} : {ty_str}")
}

fn run_types(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (mut info, diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&diags, path, &src);
    }
    let mut schemes: Vec<_> = info.fn_schemes.iter().collect();
    schemes.sort_by_key(|(name, _)| interner.resolve(**name).to_string());
    for (name, scheme) in schemes {
        let scheme_str =
            ember_types::display_scheme(scheme, &mut info.subst, &info.adts, &interner);
        println!("{}: {}", interner.resolve(*name), scheme_str);
    }
    print_diagnostics(&diags, path, &src)
}

fn run_run(path: &str, backend: Backend, time: bool, gc_stress: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }

    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }

    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&infer_diags, path, &src);
    }

    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    if exhaustive_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&exhaustive_diags, path, &src);
    }

    let start = std::time::Instant::now();
    let outcome: Result<String, String> = match backend {
        Backend::Tree => {
            if gc_stress {
                eprintln!("note: --gc-stress has no effect on the tree-walker backend (no GC)");
            }
            let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
            match err {
                Some(e) => Err(ember_diag::render::render(
                    &e.to_diagnostic(),
                    path,
                    &src,
                    use_color(),
                )),
                None => Ok(result
                    .map(|v| ember_tree::display_value(&v, &interner))
                    .unwrap_or_default()),
            }
        }
        Backend::Vm => {
            let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
            let mut vm = ember_vm::vm::Vm::new(proto);
            if gc_stress {
                vm.set_gc_stress(true);
            }
            match vm.run() {
                Ok(v) => Ok(ember_vm::value::display_value(&v)),
                Err(e) => Err(ember_diag::render::render(
                    &e.to_diagnostic(&interner),
                    path,
                    &src,
                    use_color(),
                )),
            }
        }
    };
    if time {
        eprintln!("time: {:?}", start.elapsed());
    }
    match outcome {
        Ok(s) => {
            if !s.is_empty() {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Err(rendered) => {
            println!("{rendered}");
            ExitCode::from(1)
        }
    }
}

fn run_fmt(path: &str, check: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let formatted = ember_fmt::format(&src);
    if check {
        if formatted == src {
            ExitCode::SUCCESS
        } else {
            eprintln!("{path} is not formatted");
            ExitCode::from(2)
        }
    } else {
        match fs::write(path, &formatted) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: could not write {path}: {e}");
                ExitCode::from(3)
            }
        }
    }
}

fn run_check(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&infer_diags, path, &src);
    }
    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    print_diagnostics(&exhaustive_diags, path, &src)
}

fn run_disasm(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    print!(
        "{}",
        disassemble_recursively(&proto.chunk, "script", &interner)
    );
    ExitCode::SUCCESS
}

/// Disassembles `chunk`, then recurses into every nested `FunctionProto` it
/// references (one per `OP_CLOSURE` it emits), each named `<name>::fn<i>` —
/// mirrors the pattern `ember-compile`'s own test helper of the same name
/// uses, and what `ember-cli/tests/snapshots_disasm.rs` verifies against
/// snapshots (there only for the top-level chunk; here also recursing into
/// nested closures, since `disasm` is meant to show the whole program).
pub(crate) fn disassemble_recursively(
    chunk: &ember_bytecode::chunk::Chunk,
    name: &str,
    interner: &ember_ast::Interner,
) -> String {
    let mut out = ember_bytecode::disasm::disassemble_chunk(chunk, name, interner);
    for (i, proto) in chunk.functions.iter().enumerate() {
        let nested_name = format!("{name}::fn{i}");
        out.push_str(&disassemble_recursively(
            &proto.chunk,
            &nested_name,
            interner,
        ));
    }
    out
}

/// Prints the registered long-form explanation for a diagnostic code, e.g.
/// `E0301`. Exits `3` (matching `read_source`'s own "couldn't proceed"
/// exit code) when the code isn't in `ember_diag::explain::REGISTRY`.
fn run_explain(code: &str) -> ExitCode {
    match ember_diag::explain::lookup(code) {
        Some(entry) => {
            println!("{} — {}\n\n{}", entry.code, entry.title, entry.body);
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("no explanation available for {code}");
            ExitCode::from(3)
        }
    }
}

/// Prints the full inference derivation: every unification attempted during
/// type inference, in the order it was attempted, with its origin and
/// pass/fail outcome. Types on both sides are displayed against the *final*
/// substitution (inference doesn't keep a per-step snapshot), so a variable
/// shown as resolved in an early step may not have been resolved yet at the
/// time that step actually ran.
fn run_trace(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (mut info, diags) = ember_types::infer(&ast, &mut interner, &stmts);
    // `display_ty` needs `&mut info.subst` (it resolves through the substitution,
    // compressing paths as it goes), which conflicts with iterating
    // `info.trace.steps` immutably in the same loop. Clone the steps up front
    // to break the borrow.
    let steps = info.trace.steps.clone();
    for (i, step) in steps.iter().enumerate() {
        let lhs = ember_types::display_ty(&step.lhs, &mut info.subst, &info.adts, &interner);
        let rhs = ember_types::display_ty(&step.rhs, &mut info.subst, &info.adts, &interner);
        let verdict = if step.succeeded { "ok" } else { "FAILED" };
        println!("{i:>4}  {lhs} ~ {rhs}   [{:?}]   {verdict}", step.origin);
    }
    print_diagnostics(&diags, path, &src)
}

/// Runs a program on both backends, printing elapsed wall-clock time for
/// each, the VM's allocation stats (relies on `ember-cli` always building
/// `ember-vm` with its `count-allocs` feature enabled), and the
/// tree-walker/VM speedup ratio.
fn run_bench(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (_info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&infer_diags, path, &src);
    }

    let tree_start = std::time::Instant::now();
    let (_result, tree_err) = ember_tree::interpret(&ast, &interner, &stmts);
    let tree_elapsed = tree_start.elapsed();
    if let Some(e) = tree_err {
        println!(
            "{}",
            ember_diag::render::render(&e.to_diagnostic(), path, &src, use_color())
        );
        return ExitCode::from(1);
    }

    ember_vm::reset_alloc_stats();
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let vm_start = std::time::Instant::now();
    let mut vm = ember_vm::vm::Vm::new(proto);
    let vm_result = vm.run();
    let vm_elapsed = vm_start.elapsed();
    let alloc_stats = ember_vm::alloc_stats();
    if let Err(e) = vm_result {
        println!(
            "{}",
            ember_diag::render::render(&e.to_diagnostic(&interner), path, &src, use_color())
        );
        return ExitCode::from(1);
    }

    println!("tree-walker: {tree_elapsed:?}");
    println!(
        "vm:          {vm_elapsed:?}  ({} allocations, {} bytes)",
        alloc_stats.count, alloc_stats.bytes
    );
    let ratio = tree_elapsed.as_secs_f64() / vm_elapsed.as_secs_f64().max(f64::EPSILON);
    println!("speedup:     {ratio:.2}x");
    ExitCode::SUCCESS
}

/// Runs the interactive REPL: reads entries with `rustyline`, transparently
/// continuing a read across multiple lines while brackets are unbalanced,
/// then hands each complete entry to a `repl::ReplSession`.
fn run_repl(backend: Backend, show_types: bool) -> ExitCode {
    let mut rl = rustyline::DefaultEditor::new().expect("failed to initialize line editor");
    let mut session = repl::ReplSession::new(backend, show_types);
    loop {
        let mut input = match rl.readline("ember> ") {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Eof)
            | Err(rustyline::error::ReadlineError::Interrupted) => break,
            Err(e) => {
                eprintln!("error: {e}");
                break;
            }
        };
        // Multi-line continuation: keep reading further lines while this
        // entry's brackets are unbalanced, so e.g. `fn f(x) {` prompts for
        // more input instead of being handed to the parser (and rejected)
        // as an incomplete entry.
        while !brackets_balanced(&input) {
            match rl.readline("...    ") {
                Ok(more) => {
                    input.push('\n');
                    input.push_str(&more);
                }
                Err(_) => break,
            }
        }
        let _ = rl.add_history_entry(input.as_str());
        if let Some(output) = session.handle_entry(&input) {
            println!("{output}");
        }
    }
    ExitCode::SUCCESS
}

/// Whether `src`'s brace/paren/bracket nesting is balanced (or over-closed,
/// e.g. a stray `}` typed alone) — used to decide whether the REPL should
/// keep reading more lines for the current entry. Lexes rather than parses:
/// an incomplete entry is often not valid enough to parse at all (that's the
/// whole point of needing more input), but its token stream is still fine to
/// scan for bracket depth. Lex diagnostics (e.g. an unterminated string) are
/// deliberately ignored here — they'll surface properly once the entry is
/// actually parsed.
fn brackets_balanced(src: &str) -> bool {
    let (tokens, _trivia, _diags) = ember_lexer::lex(src);
    let mut depth: i32 = 0;
    for t in &tokens {
        match t.kind {
            ember_lexer::TokenKind::LBrace
            | ember_lexer::TokenKind::LParen
            | ember_lexer::TokenKind::LBracket => depth += 1,
            ember_lexer::TokenKind::RBrace
            | ember_lexer::TokenKind::RParen
            | ember_lexer::TokenKind::RBracket => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// Compiles `path` (same parse/resolve/infer/exhaustiveness/compile
/// pipeline every other command uses, stopping with the usual
/// diagnostic-printing exit codes on any failure) and hands the resulting
/// `Vm` to the interactive `ratatui` debug TUI.
fn run_debug(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&infer_diags, path, &src);
    }
    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    if exhaustive_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&exhaustive_diags, path, &src);
    }

    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let vm = ember_vm::vm::Vm::new(proto);
    let state = debug_tui::DebugState::new(vm, interner, src);
    match debug_tui::run(state) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: debug TUI failed: {e}");
            ExitCode::from(3)
        }
    }
}

fn print_diagnostics(diags: &[ember_diag::Diagnostic], path: &str, src: &str) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    for d in diags {
        println!("{}", ember_diag::render::render(d, path, src, use_color()));
    }
    ExitCode::from(2)
}
