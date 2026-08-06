use clap::{Parser as ClapParser, Subcommand};
use std::fs;
use std::process::ExitCode;

#[derive(ClapParser)]
#[command(name = "ember")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the token stream with spans.
    Tokens { file: String },
    /// Print the parsed AST (pretty-printed source form).
    Ast {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// Print each Var's resolution (local/upvalue/global), per-function
    /// upvalue counts, and any resolver diagnostics.
    Resolve { file: String },
    /// Print each expression's inferred type, each top-level fn's generalized
    /// scheme, and any type diagnostics.
    Typecheck { file: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Tokens { file } => run_tokens(&file),
        Command::Ast { file, json } => run_ast(&file, json),
        Command::Resolve { file } => run_resolve(&file),
        Command::Typecheck { file } => run_typecheck(&file),
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

fn run_tokens(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (tokens, diags) = ember_lexer::lex(&src);
    for t in &tokens {
        let text = &src[t.span.start as usize..t.span.end as usize];
        println!("{:?}\t{}..{}\t{:?}", t.kind, t.span.start, t.span.end, text);
    }
    print_diagnostics(&diags, path, &src)
}

fn run_ast(path: &str, json: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, interner, stmts, diags) = ember_parser::parse(&src);
    if json {
        // Ast itself doesn't derive Serialize yet (its arenas are private
        // fields on a struct with no #[derive(Serialize)]) — that wiring is
        // future playground work. Fall back to the pretty-printed form.
        eprintln!("note: --json is not yet implemented; showing pretty-printed form");
    }
    for s in &stmts {
        println!("{}", ember_ast::print_stmt(&ast, &interner, *s));
    }
    print_diagnostics(&diags, path, &src)
}

fn run_resolve(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);

    let mut resolutions: Vec<_> = bindings.resolutions.iter().collect();
    resolutions.sort_by_key(|(idx, _)| ast.span_of_expr(**idx).start);
    for (idx, res) in resolutions {
        let span = ast.span_of_expr(*idx);
        let desc = match res {
            ember_resolve::Resolution::Local { slot } => format!("local[{slot}]"),
            ember_resolve::Resolution::Upvalue { index } => format!("upvalue[{index}]"),
            ember_resolve::Resolution::Global { symbol } => {
                format!("global({})", interner.resolve(*symbol))
            }
        };
        println!("{}..{}\t{}", span.start, span.end, desc);
    }

    let mut upvalue_entries: Vec<_> = bindings
        .upvalues
        .iter()
        .filter(|(_, ups)| !ups.is_empty())
        .collect();
    upvalue_entries.sort_by_key(|(id, _)| format!("{id:?}"));
    for (id, ups) in upvalue_entries {
        println!("{id:?}: {} upvalue(s) -> {ups:?}", ups.len());
    }

    print_diagnostics(&diags, path, &src)
}

fn run_typecheck(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let resolve_diags = resolver.diagnostics();
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(resolve_diags, path, &src);
    }

    let (mut info, mut diags) = ember_types::infer(&ast, &mut interner, &stmts);

    let mut typed: Vec<_> = info.expr_types.iter().collect();
    typed.sort_by_key(|(idx, _)| ast.span_of_expr(**idx).start);
    for (idx, ty) in typed {
        let span = ast.span_of_expr(*idx);
        let ty_str = ember_types::display_ty(ty, &mut info.subst, &info.adts, &interner);
        println!("{}..{}\t{}", span.start, span.end, ty_str);
    }

    let mut schemes: Vec<_> = info.fn_schemes.iter().collect();
    schemes.sort_by_key(|(name, _)| interner.resolve(**name).to_string());
    for (name, scheme) in schemes {
        let scheme_str =
            ember_types::display_scheme(scheme, &mut info.subst, &info.adts, &interner);
        println!("{}: {}", interner.resolve(*name), scheme_str);
    }

    diags.extend(ember_types::check_exhaustiveness(
        &ast, &interner, &info, &stmts,
    ));

    print_diagnostics(&diags, path, &src)
}

fn print_diagnostics(diags: &[ember_diag::Diagnostic], path: &str, src: &str) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    let use_color = std::env::var_os("NO_COLOR").is_none();
    for d in diags {
        println!("{}", ember_diag::render::render(d, path, src, use_color));
    }
    ExitCode::from(2)
}
