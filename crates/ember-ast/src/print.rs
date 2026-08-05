use ember_lexer::TokenKind;

use crate::ast::Ast;
use crate::expr::Expr;
use crate::idx::Idx;
use crate::interner::Interner;
use crate::stmt::Stmt;

fn op_text(op: TokenKind) -> &'static str {
    match op {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::EqEq => "==",
        TokenKind::BangEq => "!=",
        TokenKind::Lt => "<",
        TokenKind::LtEq => "<=",
        TokenKind::Gt => ">",
        TokenKind::GtEq => ">=",
        TokenKind::AndAnd => "&&",
        TokenKind::OrOr => "||",
        TokenKind::Bang => "!",
        TokenKind::DotDot => "..",
        _ => unreachable!("op_text called on non-operator TokenKind {op:?}"),
    }
}

/// Prints an expression as a fully-parenthesized S-expression-like form —
/// used by the parser's own tests to assert precedence/associativity, and
/// as the basis for the round-trip property test in a later task.
pub fn print_expr(ast: &Ast, interner: &Interner, idx: Idx<Expr>) -> String {
    match ast.expr(idx) {
        Expr::Int(n) => n.to_string(),
        Expr::Float(f) => f.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Nil => "nil".to_string(),
        Expr::Str(sym) => format!("{:?}", interner.resolve(*sym)),
        Expr::Var(sym) => interner.resolve(*sym).to_string(),
        Expr::Unary { op, operand } => {
            format!("({}{})", op_text(*op), print_expr(ast, interner, *operand))
        }
        Expr::Binary { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                print_expr(ast, interner, *lhs),
                op_text(*op),
                print_expr(ast, interner, *rhs)
            )
        }
        Expr::Assign { target, value } => {
            format!(
                "({} = {})",
                print_expr(ast, interner, *target),
                print_expr(ast, interner, *value)
            )
        }
        Expr::Call { callee, args } => {
            let args_str: Vec<_> = args.iter().map(|a| print_expr(ast, interner, *a)).collect();
            format!(
                "(call {} {})",
                print_expr(ast, interner, *callee),
                args_str.join(" ")
            )
        }
        Expr::Index { base, index } => {
            format!(
                "(index {} {})",
                print_expr(ast, interner, *base),
                print_expr(ast, interner, *index)
            )
        }
        Expr::Field { base, name } => format!(
            "(field {} {})",
            print_expr(ast, interner, *base),
            interner.resolve(*name)
        ),
        Expr::If { cond, then_, else_ } => {
            let else_str = match else_ {
                Some(e) => print_expr(ast, interner, *e),
                None => "()".to_string(),
            };
            format!(
                "(if {} {} {})",
                print_expr(ast, interner, *cond),
                print_expr(ast, interner, *then_),
                else_str
            )
        }
        Expr::Block { stmts, tail } => {
            let mut parts: Vec<String> = stmts
                .iter()
                .map(|s| print_stmt(ast, interner, *s))
                .collect();
            if let Some(t) = tail {
                parts.push(print_expr(ast, interner, *t));
            }
            format!("(block {})", parts.join(" "))
        }
        Expr::List { items } => {
            let items_str: Vec<_> = items
                .iter()
                .map(|i| print_expr(ast, interner, *i))
                .collect();
            format!("[{}]", items_str.join(", "))
        }
        Expr::Struct { name, fields } => {
            let fields_str: Vec<_> = fields
                .iter()
                .map(|(n, v)| {
                    format!(
                        "{}: {}",
                        interner.resolve(*n),
                        print_expr(ast, interner, *v)
                    )
                })
                .collect();
            format!(
                "{} {{ {} }}",
                interner.resolve(*name),
                fields_str.join(", ")
            )
        }
        Expr::Lambda { params, body } => {
            let params_str: Vec<_> = params
                .iter()
                .map(|p| interner.resolve(p.name).to_string())
                .collect();
            format!(
                "|{}| {}",
                params_str.join(", "),
                print_expr(ast, interner, *body)
            )
        }
        Expr::Match { scrutinee, arms } => {
            let arms_str: Vec<_> = arms
                .iter()
                .map(|a| {
                    format!(
                        "{} => {}",
                        print_pattern(ast, interner, a.pat),
                        print_expr(ast, interner, a.body)
                    )
                })
                .collect();
            format!(
                "match {} {{ {} }}",
                print_expr(ast, interner, *scrutinee),
                arms_str.join(", ")
            )
        }
        Expr::Error => "<error>".to_string(),
    }
}

/// Prints a pattern as ember source text.
pub fn print_pattern(ast: &Ast, interner: &Interner, idx: Idx<crate::pattern::Pattern>) -> String {
    use crate::pattern::Pattern;
    match ast.pat(idx) {
        Pattern::Wild => "_".to_string(),
        Pattern::Bind(s) => interner.resolve(*s).to_string(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Float(f) => f.to_string(),
        Pattern::Str(s) => format!("{:?}", interner.resolve(*s)),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, args } if args.is_empty() => interner.resolve(*name).to_string(),
        Pattern::Ctor { name, args } => {
            let args_str: Vec<_> = args
                .iter()
                .map(|a| print_pattern(ast, interner, *a))
                .collect();
            format!("{}({})", interner.resolve(*name), args_str.join(", "))
        }
        Pattern::Tuple(items) => {
            let items_str: Vec<_> = items
                .iter()
                .map(|i| print_pattern(ast, interner, *i))
                .collect();
            format!("({})", items_str.join(", "))
        }
        Pattern::List { items, rest } => {
            let mut parts: Vec<_> = items
                .iter()
                .map(|i| print_pattern(ast, interner, *i))
                .collect();
            if let Some(r) = rest {
                parts.push(format!("..{}", print_pattern(ast, interner, *r)));
            }
            format!("[{}]", parts.join(", "))
        }
        Pattern::Record { name, fields } => {
            let fields_str: Vec<_> = fields
                .iter()
                .map(|(n, p)| {
                    format!(
                        "{}: {}",
                        interner.resolve(*n),
                        print_pattern(ast, interner, *p)
                    )
                })
                .collect();
            format!(
                "{} {{ {} }}",
                interner.resolve(*name),
                fields_str.join(", ")
            )
        }
        Pattern::Or(alts) => {
            let alts_str: Vec<_> = alts
                .iter()
                .map(|a| print_pattern(ast, interner, *a))
                .collect();
            alts_str.join(" | ")
        }
        Pattern::Error => "<error-pattern>".to_string(),
    }
}

/// Prints a statement as ember source text.
pub fn print_stmt(ast: &Ast, interner: &Interner, idx: Idx<Stmt>) -> String {
    match ast.stmt(idx) {
        Stmt::Let {
            name,
            mutable,
            init,
            ..
        } => {
            let kw = if *mutable { "let mut" } else { "let" };
            format!(
                "{} {} = {};",
                kw,
                interner.resolve(*name),
                print_expr(ast, interner, *init)
            )
        }
        Stmt::ExprStmt(e) => format!("{};", print_expr(ast, interner, *e)),
        Stmt::Break => "break;".to_string(),
        Stmt::Continue => "continue;".to_string(),
        Stmt::Fn {
            name, params, body, ..
        } => {
            let params_str: Vec<_> = params
                .iter()
                .map(|p| interner.resolve(p.name).to_string())
                .collect();
            format!(
                "fn {}({}) {}",
                interner.resolve(*name),
                params_str.join(", "),
                print_expr(ast, interner, *body)
            )
        }
        Stmt::TypeDecl { name, variants } => {
            let variants_str: Vec<_> = variants
                .iter()
                .map(|v| interner.resolve(v.name).to_string())
                .collect();
            format!(
                "type {} = {}",
                interner.resolve(*name),
                variants_str.join(" | ")
            )
        }
        Stmt::StructDecl { name, fields } => {
            let fields_str: Vec<_> = fields
                .iter()
                .map(|f| interner.resolve(f.name).to_string())
                .collect();
            format!(
                "struct {} {{ {} }}",
                interner.resolve(*name),
                fields_str.join(", ")
            )
        }
        Stmt::While { cond, body } => format!(
            "while {} {}",
            print_expr(ast, interner, *cond),
            print_expr(ast, interner, *body)
        ),
        Stmt::For {
            binding,
            iter,
            body,
        } => {
            format!(
                "for {} in {} {}",
                interner.resolve(*binding),
                print_expr(ast, interner, *iter),
                print_expr(ast, interner, *body)
            )
        }
        Stmt::Loop { body } => format!("loop {}", print_expr(ast, interner, *body)),
        Stmt::Return(Some(e)) => format!("return {};", print_expr(ast, interner, *e)),
        Stmt::Return(None) => "return;".to_string(),
        Stmt::Error => "<error-stmt>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Ast;
    use crate::expr::Expr;
    use crate::stmt::Stmt;
    use ember_span::Span;

    #[test]
    fn prints_binary_expression() {
        let mut ast = Ast::new();
        let interner = crate::interner::Interner::new();
        let one = ast.alloc_expr(Expr::Int(1), Span::new(0, 1));
        let two = ast.alloc_expr(Expr::Int(2), Span::new(0, 1));
        let bin = ast.alloc_expr(
            Expr::Binary {
                op: ember_lexer::TokenKind::Plus,
                lhs: one,
                rhs: two,
            },
            Span::new(0, 5),
        );
        assert_eq!(print_expr(&ast, &interner, bin), "(1 + 2)");
    }

    #[test]
    fn prints_variable_by_its_interned_name() {
        let mut ast = Ast::new();
        let mut interner = crate::interner::Interner::new();
        let sym = interner.intern("count");
        let v = ast.alloc_expr(Expr::Var(sym), Span::new(0, 5));
        assert_eq!(print_expr(&ast, &interner, v), "count");
    }

    #[test]
    fn prints_let_statement() {
        let mut ast = Ast::new();
        let mut interner = crate::interner::Interner::new();
        let sym = interner.intern("x");
        let init = ast.alloc_expr(Expr::Int(42), Span::new(0, 2));
        let stmt = ast.alloc_stmt(
            Stmt::Let {
                name: sym,
                mutable: false,
                ty: None,
                init,
            },
            Span::new(0, 10),
        );
        assert_eq!(print_stmt(&ast, &interner, stmt), "let x = 42;");
    }
}
