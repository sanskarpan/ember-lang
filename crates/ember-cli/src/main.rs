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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Tokens { file } => run_tokens(&file),
        Command::Ast { file, json } => run_ast(&file, json),
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
