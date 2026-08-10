use crate::doc::Doc;
use crate::print_expr::print_comma_list;
use ember_ast::{Ast, Idx, Interner, Pattern};

pub fn print_pattern(ast: &Ast, interner: &Interner, p: Idx<Pattern>) -> Doc {
    match ast.pat(p).clone() {
        Pattern::Wild => Doc::text("_"),
        Pattern::Bind(s) => Doc::text(interner.resolve(s).to_string()),
        Pattern::Int(n) => Doc::text(n.to_string()),
        Pattern::Float(f) => Doc::text(f.to_string()),
        Pattern::Str(s) => Doc::text(format!("{:?}", interner.resolve(s))),
        Pattern::Bool(b) => Doc::text(b.to_string()),
        Pattern::Ctor { name, args } => {
            if args.is_empty() {
                Doc::text(interner.resolve(name).to_string())
            } else {
                Doc::concat(vec![
                    Doc::text(interner.resolve(name).to_string()),
                    Doc::text("("),
                    print_comma_list(args.iter().map(|&a| print_pattern(ast, interner, a))),
                    Doc::text(")"),
                ])
            }
        }
        Pattern::Tuple(items) => Doc::concat(vec![
            Doc::text("("),
            print_comma_list(items.iter().map(|&i| print_pattern(ast, interner, i))),
            Doc::text(")"),
        ]),
        Pattern::List { items, rest } => {
            let mut parts: Vec<Doc> = items
                .iter()
                .map(|&i| print_pattern(ast, interner, i))
                .collect();
            if let Some(r) = rest {
                parts.push(Doc::concat(vec![
                    Doc::text(".."),
                    print_pattern(ast, interner, r),
                ]));
            }
            Doc::concat(vec![
                Doc::text("["),
                print_comma_list(parts.into_iter()),
                Doc::text("]"),
            ])
        }
        Pattern::Record { name, fields } => {
            let field_docs = fields.iter().map(|(fname, fpat)| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(*fname).to_string()),
                    Doc::text(": "),
                    print_pattern(ast, interner, *fpat),
                ])
            });
            Doc::concat(vec![
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Pattern::Or(alts) => {
            let mut out = Vec::new();
            for (i, &a) in alts.iter().enumerate() {
                if i > 0 {
                    out.push(Doc::text(" | "));
                }
                out.push(print_pattern(ast, interner, a));
            }
            Doc::concat(out)
        }
        Pattern::Error => Doc::text("<error>"),
    }
}
