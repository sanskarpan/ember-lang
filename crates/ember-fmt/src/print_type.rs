use crate::doc::Doc;
use crate::print_expr::print_comma_list;
use ember_ast::{Ast, Idx, Interner, TypeExpr};

pub fn print_type(ast: &Ast, interner: &Interner, t: Idx<TypeExpr>) -> Doc {
    match ast.type_expr(t).clone() {
        TypeExpr::Name(s) => Doc::text(interner.resolve(s).to_string()),
        TypeExpr::Generic { name, args } => Doc::concat(vec![
            Doc::text(interner.resolve(name).to_string()),
            Doc::text("<"),
            print_comma_list(args.iter().map(|&a| print_type(ast, interner, a))),
            Doc::text(">"),
        ]),
        TypeExpr::List(elem) => Doc::concat(vec![
            Doc::text("["),
            print_type(ast, interner, elem),
            Doc::text("]"),
        ]),
        TypeExpr::Fun { params, ret } => Doc::concat(vec![
            Doc::text("("),
            print_comma_list(params.iter().map(|&p| print_type(ast, interner, p))),
            Doc::text(") -> "),
            print_type(ast, interner, ret),
        ]),
        TypeExpr::Error => Doc::text("<error-type>"),
    }
}
