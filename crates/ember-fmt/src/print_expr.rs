use crate::doc::Doc;
use ember_ast::{Ast, Expr, Idx, Interner};
use ember_lexer::TokenKind;
use ember_parser::prec::InfixPrec;
use ember_parser::Prec;

/// Renders a `Float` literal so it always re-lexes as `TokenKind::Float`
/// rather than `TokenKind::Int`. `f64::to_string()` drops the fractional
/// part for whole-number floats (`4.0.to_string() == "4"`), which would
/// silently turn a float literal into an int literal on the next parse —
/// append `.0` whenever the default rendering has no `.` to make the
/// float-ness explicit again.
fn format_float_literal(f: f64) -> String {
    let s = f.to_string();
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

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

/// Every `Expr`'s own intrinsic precedence, for paren-elision decisions —
/// NOT the same table as `InfixPrec` (that's only for binary operator
/// tokens); this covers every expression KIND.
fn expr_prec(ast: &Ast, e: Idx<Expr>) -> Prec {
    match ast.expr(e) {
        Expr::Binary { op, .. } => op.infix_prec(),
        Expr::Unary { .. } => Prec::Unary,
        Expr::Assign { .. } => Prec::Assign,
        Expr::Call { .. } | Expr::Index { .. } | Expr::Field { .. } => Prec::Call,
        _ => Prec::Primary,
    }
}

/// Prints `e`, parenthesizing it if its own precedence is lower than
/// `min_prec` requires (i.e. it wouldn't parse back to the same tree
/// without parens in this position).
pub fn print_expr(ast: &Ast, interner: &Interner, e: Idx<Expr>, min_prec: Prec) -> Doc {
    let inner = print_expr_inner(ast, interner, e);
    if expr_prec(ast, e) < min_prec {
        Doc::concat(vec![Doc::text("("), inner, Doc::text(")")])
    } else {
        inner
    }
}

fn print_expr_inner(ast: &Ast, interner: &Interner, e: Idx<Expr>) -> Doc {
    match ast.expr(e).clone() {
        Expr::Int(n) => Doc::text(n.to_string()),
        Expr::Float(f) => Doc::text(format_float_literal(f)),
        Expr::Str(s) => Doc::text(format!("{:?}", interner.resolve(s))),
        Expr::Bool(b) => Doc::text(b.to_string()),
        Expr::Nil => Doc::text("nil"),
        Expr::Var(s) => Doc::text(interner.resolve(s).to_string()),
        Expr::Unary { op, operand } => Doc::concat(vec![
            Doc::text(op_text(op)),
            print_expr(ast, interner, operand, Prec::Unary),
        ]),
        Expr::Binary { op, lhs, rhs } => {
            let p = op.infix_prec();
            // Left-associative: LHS may be equal precedence (no parens),
            // RHS must be STRICTLY tighter (equal precedence on the right
            // needs parens).
            let lhs_doc = print_expr(ast, interner, lhs, p);
            let rhs_min = match p {
                Prec::Primary => Prec::Primary,
                _ => next_tighter(p),
            };
            let rhs_doc = print_expr(ast, interner, rhs, rhs_min);
            Doc::group(Doc::concat(vec![
                lhs_doc,
                Doc::text(" "),
                Doc::text(op_text(op)),
                Doc::Line,
                rhs_doc,
            ]))
        }
        Expr::Assign { target, value } => Doc::concat(vec![
            print_expr(ast, interner, target, Prec::None),
            Doc::text(" = "),
            print_expr(ast, interner, value, Prec::Assign),
        ]),
        Expr::Call { callee, args } => Doc::concat(vec![
            print_expr(ast, interner, callee, Prec::Call),
            Doc::text("("),
            print_comma_list(
                args.iter()
                    .map(|&a| print_expr(ast, interner, a, Prec::None)),
            ),
            Doc::text(")"),
        ]),
        Expr::Index { base, index } => Doc::concat(vec![
            print_expr(ast, interner, base, Prec::Call),
            Doc::text("["),
            print_expr(ast, interner, index, Prec::None),
            Doc::text("]"),
        ]),
        Expr::Field { base, name } => Doc::concat(vec![
            print_expr(ast, interner, base, Prec::Call),
            Doc::text("."),
            Doc::text(interner.resolve(name).to_string()),
        ]),
        Expr::Block { stmts, tail } => {
            if stmts.is_empty() && tail.is_none() {
                return Doc::text("{}");
            }
            let mut body = Vec::new();
            for &s in &stmts {
                body.push(Doc::HardLine);
                body.push(crate::print_stmt::print_stmt(ast, interner, s));
            }
            if let Some(t) = tail {
                body.push(Doc::HardLine);
                body.push(print_expr(ast, interner, t, Prec::None));
            }
            Doc::concat(vec![
                Doc::text("{"),
                Doc::nest(4, Doc::concat(body)),
                Doc::HardLine,
                Doc::text("}"),
            ])
        }
        Expr::If { cond, then_, else_ } => {
            let mut out = vec![
                Doc::text("if "),
                print_expr(ast, interner, cond, Prec::None),
                Doc::text(" "),
                print_expr(ast, interner, then_, Prec::None),
            ];
            if let Some(e) = else_ {
                out.push(Doc::text(" else "));
                out.push(print_expr(ast, interner, e, Prec::None));
            }
            Doc::concat(out)
        }
        Expr::Lambda { params, body } => {
            let param_docs = params
                .iter()
                .map(|p| Doc::text(interner.resolve(p.name).to_string()));
            Doc::concat(vec![
                Doc::text("|"),
                print_comma_list(param_docs),
                Doc::text("| "),
                print_expr(ast, interner, body, Prec::Assign.lower()),
            ])
        }
        Expr::List { items } => Doc::concat(vec![
            Doc::text("["),
            print_comma_list(
                items
                    .iter()
                    .map(|&i| print_expr(ast, interner, i, Prec::None)),
            ),
            Doc::text("]"),
        ]),
        Expr::Struct { name, fields } => {
            let field_docs = fields.iter().map(|(fname, fval)| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(*fname).to_string()),
                    Doc::text(": "),
                    print_expr(ast, interner, *fval, Prec::None),
                ])
            });
            Doc::concat(vec![
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Expr::Match { scrutinee, arms } => {
            let mut body = Vec::new();
            for arm in &arms {
                body.push(Doc::HardLine);
                body.push(crate::print_pattern::print_pattern(ast, interner, arm.pat));
                if let Some(guard) = arm.guard {
                    body.push(Doc::text(" if "));
                    body.push(print_expr(ast, interner, guard, Prec::None));
                }
                body.push(Doc::text(" => "));
                body.push(print_expr(ast, interner, arm.body, Prec::None));
                body.push(Doc::text(","));
            }
            Doc::concat(vec![
                Doc::text("match "),
                print_expr(ast, interner, scrutinee, Prec::None),
                Doc::text(" {"),
                Doc::nest(4, Doc::concat(body)),
                Doc::HardLine,
                Doc::text("}"),
            ])
        }
        Expr::Error => Doc::text("<error>"),
    }
}

/// One step tighter than `p` — the opposite direction of `Prec::lower`,
/// needed for right-operand paren rules. `Prec` has no built-in "raise by
/// one" (only `lower`), so this is a small manual inverse table.
fn next_tighter(p: Prec) -> Prec {
    match p {
        Prec::None => Prec::Assign,
        Prec::Assign => Prec::Or,
        Prec::Or => Prec::And,
        Prec::And => Prec::Equality,
        Prec::Equality => Prec::Comparison,
        Prec::Comparison => Prec::Term,
        Prec::Term => Prec::Factor,
        Prec::Factor => Prec::Unary,
        Prec::Unary => Prec::Call,
        Prec::Call => Prec::Primary,
        Prec::Primary => Prec::Primary,
    }
}

/// Joins `docs` with `", "` — used for call args and, in later tasks,
/// every other comma-separated construct (list/struct literals, params).
pub fn print_comma_list(docs: impl Iterator<Item = Doc>) -> Doc {
    let items: Vec<Doc> = docs.collect();
    let mut out = Vec::new();
    for (i, d) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(Doc::text(", "));
        }
        out.push(d);
    }
    Doc::concat(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_expr(src: &str) -> String {
        // Parses `src` as a standalone expression statement, formats just
        // that expression, ignoring the trailing `;` this wraps it in.
        let wrapped = format!("{src};");
        let (ast, interner, stmts, diags) = ember_parser::parse(&wrapped);
        assert!(diags.is_empty(), "parse diags for {src:?}: {diags:?}");
        let e = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected ExprStmt, got {other:?}"),
        };
        let doc = print_expr(&ast, &interner, e, ember_parser::Prec::None);
        ember_fmt_doc_render(doc)
    }

    fn ember_fmt_doc_render(doc: crate::Doc) -> String {
        crate::render(doc, 100)
    }

    #[test]
    fn literals() {
        assert_eq!(fmt_expr("42"), "42");
        assert_eq!(fmt_expr("3.14"), "3.14");
        assert_eq!(fmt_expr("true"), "true");
        assert_eq!(fmt_expr("false"), "false");
        assert_eq!(fmt_expr("nil"), "nil");
        assert_eq!(fmt_expr("\"hi\""), "\"hi\"");
        assert_eq!(fmt_expr("x"), "x");
    }

    #[test]
    fn binary_operators_get_spaces_and_no_redundant_parens() {
        assert_eq!(fmt_expr("1 + 2"), "1 + 2");
        assert_eq!(fmt_expr("1 + 2 * 3"), "1 + 2 * 3");
        assert_eq!(fmt_expr("(1 + 2) * 3"), "(1 + 2) * 3");
        assert_eq!(fmt_expr("1 - 2 - 3"), "1 - 2 - 3");
        assert_eq!(fmt_expr("1 - (2 - 3)"), "1 - (2 - 3)");
        assert_eq!(fmt_expr("a && b || c"), "a && b || c");
    }

    #[test]
    fn unary_and_call_precedence() {
        assert_eq!(fmt_expr("-x"), "-x");
        assert_eq!(fmt_expr("-(x + 1)"), "-(x + 1)");
        assert_eq!(fmt_expr("!x"), "!x");
        assert_eq!(fmt_expr("f(1, 2)"), "f(1, 2)");
        assert_eq!(fmt_expr("f()"), "f()");
        assert_eq!(fmt_expr("a.b"), "a.b");
        assert_eq!(fmt_expr("a[0]"), "a[0]");
        assert_eq!(fmt_expr("a.b.c"), "a.b.c");
    }

    #[test]
    fn assignment() {
        assert_eq!(fmt_expr("x = 1"), "x = 1");
    }

    #[test]
    fn block_with_tail() {
        assert_eq!(fmt_expr("{ let x = 1; x }"), "{\n    let x = 1;\n    x\n}");
    }

    #[test]
    fn empty_block() {
        assert_eq!(fmt_expr("{ }"), "{}");
    }

    #[test]
    fn if_else() {
        assert_eq!(
            fmt_expr("if x { 1 } else { 2 }"),
            "if x {\n    1\n} else {\n    2\n}"
        );
    }

    #[test]
    fn if_no_else() {
        assert_eq!(fmt_expr("if x { 1 }"), "if x {\n    1\n}");
    }

    #[test]
    fn else_if_chains_stay_on_one_line_at_each_link() {
        assert_eq!(
            fmt_expr("if a { 1 } else if b { 2 } else { 3 }"),
            "if a {\n    1\n} else if b {\n    2\n} else {\n    3\n}"
        );
    }

    #[test]
    fn lambda_with_bare_expr_body_stays_inline() {
        assert_eq!(fmt_expr("|| 42"), "|| 42");
        assert_eq!(fmt_expr("|x| x + 1"), "|x| x + 1");
    }

    #[test]
    fn lambda_with_block_body() {
        assert_eq!(fmt_expr("|| { 1; 2 }"), "|| {\n    1;\n    2\n}");
    }

    #[test]
    fn list_literal() {
        assert_eq!(fmt_expr("[1, 2, 3]"), "[1, 2, 3]");
        assert_eq!(fmt_expr("[]"), "[]");
    }

    #[test]
    fn struct_literal() {
        assert_eq!(fmt_expr("_P { x: 1, y: 2 }"), "_P { x: 1, y: 2 }");
    }

    #[test]
    fn match_with_guard_is_not_dropped() {
        assert_eq!(
            fmt_expr("match x { n if n > 0 => 1, _ => 0, }"),
            "match x {\n    n if n > 0 => 1,\n    _ => 0,\n}"
        );
    }
}
