use crate::doc::Doc;
use crate::print_expr::{print_comma_list, print_expr};
use crate::print_type::print_type;
use ember_ast::{Ast, Idx, Interner, Stmt};
use ember_parser::Prec;

pub(crate) fn print_decl_stmt(ast: &Ast, interner: &Interner, s: Idx<Stmt>) -> Doc {
    match ast.stmt(s).clone() {
        Stmt::Fn {
            name,
            params,
            ret_ty,
            body,
        } => {
            let param_docs = params.iter().map(|p| match p.ty {
                Some(ty) => Doc::concat(vec![
                    Doc::text(interner.resolve(p.name).to_string()),
                    Doc::text(": "),
                    print_type(ast, interner, ty),
                ]),
                None => Doc::text(interner.resolve(p.name).to_string()),
            });
            let mut out = vec![
                Doc::text("fn "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text("("),
                print_comma_list(param_docs),
                Doc::text(")"),
            ];
            if let Some(rt) = ret_ty {
                out.push(Doc::text(" -> "));
                out.push(print_type(ast, interner, rt));
            }
            out.push(Doc::text(" "));
            out.push(print_expr(ast, interner, body, Prec::None));
            Doc::concat(out)
        }
        Stmt::StructDecl { name, fields } => {
            let field_docs = fields.iter().map(|f| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(f.name).to_string()),
                    Doc::text(": "),
                    print_type(ast, interner, f.ty),
                ])
            });
            Doc::concat(vec![
                Doc::text("struct "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Stmt::TypeDecl { name, variants } => {
            let mut out = vec![
                Doc::text("type "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" = "),
            ];
            for (i, v) in variants.iter().enumerate() {
                if i > 0 {
                    out.push(Doc::text(" | "));
                }
                out.push(Doc::text(interner.resolve(v.name).to_string()));
                if !v.payload.is_empty() {
                    out.push(Doc::text("("));
                    out.push(print_comma_list(
                        v.payload.iter().map(|&p| print_type(ast, interner, p)),
                    ));
                    out.push(Doc::text(")"));
                }
            }
            out.push(Doc::text(";"));
            Doc::concat(out)
        }
        other => unreachable!("print_decl_stmt called with non-decl {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_program(src: &str) -> String {
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty(), "parse diags for {src:?}: {diags:?}");
        let docs: Vec<Doc> = stmts
            .iter()
            .map(|&s| crate::print_stmt::print_stmt(&ast, &interner, s))
            .collect();
        let mut out = Vec::new();
        for (i, d) in docs.into_iter().enumerate() {
            if i > 0 {
                out.push(Doc::HardLine);
            }
            out.push(d);
        }
        crate::render(Doc::concat(out), 100)
    }

    #[test]
    fn fn_decl_with_params_and_return_type() {
        assert_eq!(
            fmt_program("fn add(a: Int, b: Int) -> Int { a + b }"),
            "fn add(a: Int, b: Int) -> Int {\n    a + b\n}"
        );
    }

    #[test]
    fn fn_decl_no_types() {
        assert_eq!(fmt_program("fn f(x) { x }"), "fn f(x) {\n    x\n}");
    }

    #[test]
    fn struct_decl() {
        assert_eq!(
            fmt_program("struct Point { x: Int, y: Int }"),
            "struct Point { x: Int, y: Int }"
        );
    }

    #[test]
    fn type_decl_with_payload_and_nullary_variants() {
        assert_eq!(
            fmt_program("type Shape = Circle(Float) | Origin;"),
            "type Shape = Circle(Float) | Origin;"
        );
    }

    #[test]
    fn let_with_type_annotation() {
        assert_eq!(fmt_program("let x: Int = 1;"), "let x: Int = 1;");
    }
}
