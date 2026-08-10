use crate::doc::Doc;
use crate::print_expr::print_expr;
use ember_ast::{Ast, Idx, Interner, Stmt};
use ember_parser::Prec;

pub fn print_stmt(ast: &Ast, interner: &Interner, s: Idx<Stmt>) -> Doc {
    match ast.stmt(s).clone() {
        Stmt::Let {
            name,
            mutable,
            ty,
            init,
        } => {
            let kw = if mutable { "let mut " } else { "let " };
            let mut out = vec![Doc::text(kw), Doc::text(interner.resolve(name).to_string())];
            if let Some(t) = ty {
                out.push(Doc::text(": "));
                out.push(crate::print_type::print_type(ast, interner, t));
            }
            out.push(Doc::text(" = "));
            out.push(print_expr(ast, interner, init, Prec::None));
            out.push(Doc::text(";"));
            Doc::concat(out)
        }
        Stmt::ExprStmt(e) => Doc::concat(vec![
            print_expr(ast, interner, e, Prec::None),
            Doc::text(";"),
        ]),
        Stmt::While { cond, body } => Doc::concat(vec![
            Doc::text("while "),
            print_expr(ast, interner, cond, Prec::None),
            Doc::text(" "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::For {
            binding,
            iter,
            body,
        } => Doc::concat(vec![
            Doc::text("for "),
            Doc::text(interner.resolve(binding).to_string()),
            Doc::text(" in "),
            print_expr(ast, interner, iter, Prec::None),
            Doc::text(" "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::Loop { body } => Doc::concat(vec![
            Doc::text("loop "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::Return(value) => match value {
            Some(v) => Doc::concat(vec![
                Doc::text("return "),
                print_expr(ast, interner, v, Prec::None),
                Doc::text(";"),
            ]),
            None => Doc::text("return;"),
        },
        Stmt::Break => Doc::text("break;"),
        Stmt::Continue => Doc::text("continue;"),
        Stmt::Error => Doc::text("<error-stmt>"),
        Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. } => {
            crate::print_decl::print_decl_stmt(ast, interner, s)
        }
    }
}
