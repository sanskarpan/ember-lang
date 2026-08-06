use crate::env::Env;
use crate::value::Value;
use ember_ast::{Ast, Idx, Interner, Pattern};
use std::cell::RefCell;
use std::rc::Rc;

/// Attempts to match `value` against `pat`, binding any names the pattern
/// introduces into `env` as it goes. `Pattern::Tuple` can never succeed —
/// there's no `Value::Tuple` (no way to construct one), the same
/// inertness carried since Phase 5/6, not newly introduced here.
pub fn match_pattern(
    ast: &Ast,
    interner: &Interner,
    pat: Idx<Pattern>,
    value: &Value,
    env: &Rc<RefCell<Env>>,
) -> bool {
    match ast.pat(pat).clone() {
        Pattern::Wild | Pattern::Error => true,
        Pattern::Bind(sym) => {
            Env::declare(env, sym, value.clone());
            true
        }
        Pattern::Int(n) => matches!(value, Value::Int(v) if *v == n),
        Pattern::Float(f) => matches!(value, Value::Float(v) if *v == f),
        Pattern::Bool(b) => matches!(value, Value::Bool(v) if *v == b),
        Pattern::Str(s) => matches!(value, Value::Str(v) if interner.resolve(s) == v.as_str()),
        Pattern::Ctor { name, args } => match value {
            Value::Adt(adt) if adt.variant == name && adt.fields.len() == args.len() => args
                .iter()
                .zip(adt.fields.iter())
                .all(|(&p, v)| match_pattern(ast, interner, p, v, env)),
            _ => false,
        },
        Pattern::Record { name, fields } => match value {
            Value::Record {
                name: rname,
                fields: value_fields,
            } if *rname == name => {
                let vf = value_fields.borrow();
                fields
                    .iter()
                    .all(|(field_name, pat_idx)| match vf.get(field_name) {
                        Some(v) => match_pattern(ast, interner, *pat_idx, v, env),
                        None => false,
                    })
            }
            _ => false,
        },
        Pattern::List { items, rest } => match value {
            Value::List(l) => {
                let list = l.borrow();
                if rest.is_none() {
                    if list.len() != items.len() {
                        return false;
                    }
                } else if list.len() < items.len() {
                    return false;
                }
                for (i, &item_pat) in items.iter().enumerate() {
                    if !match_pattern(ast, interner, item_pat, &list[i], env) {
                        return false;
                    }
                }
                match rest {
                    Some(rest_pat) => {
                        let remaining: Vec<Value> = list[items.len()..].to_vec();
                        let rest_value = Value::List(Rc::new(RefCell::new(remaining)));
                        match_pattern(ast, interner, rest_pat, &rest_value, env)
                    }
                    None => true,
                }
            }
            _ => false,
        },
        Pattern::Or(alts) => alts
            .iter()
            .any(|&a| match_pattern(ast, interner, a, value, env)),
        Pattern::Tuple(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use crate::value::Value;
    use ember_ast::Interner;

    fn parse_pattern_in_a_match(src: &str) -> (ember_ast::Ast, Interner, Idx<Pattern>) {
        let full = format!("match x {{ {src} => 1, _ => 2, }}");
        let (ast, interner, stmts, diags) = ember_parser::parse(&full);
        assert!(diags.is_empty(), "diags: {diags:?}");
        let pat = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => match ast.expr(*e) {
                ember_ast::Expr::Match { arms, .. } => arms[0].pat,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        (ast, interner, pat)
    }

    #[test]
    fn wildcard_and_bind_always_match() {
        let (ast, interner, pat) = parse_pattern_in_a_match("_");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
    }

    #[test]
    fn bind_pattern_declares_the_name() {
        let (ast, mut interner, pat) = parse_pattern_in_a_match("y");
        let y = interner.intern("y");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(7), &env));
        assert!(matches!(Env::get(&env, y), Some(Value::Int(7))));
    }

    #[test]
    fn literal_patterns_match_by_value() {
        let (ast, interner, pat) = parse_pattern_in_a_match("0");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(0), &env));
        assert!(!match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
    }

    #[test]
    fn list_pattern_with_rest_destructures() {
        let (ast, mut interner, pat) = parse_pattern_in_a_match("[a, ..rest]");
        let a = interner.intern("a");
        let rest = interner.intern("rest");
        let env = Env::new();
        let list = Value::List(std::rc::Rc::new(std::cell::RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])));
        assert!(match_pattern(&ast, &interner, pat, &list, &env));
        assert!(matches!(Env::get(&env, a), Some(Value::Int(1))));
        match Env::get(&env, rest) {
            Some(Value::List(l)) => assert_eq!(l.borrow().len(), 2),
            other => panic!("expected a List, got {other:?}"),
        }
    }

    #[test]
    fn empty_list_pattern_does_not_match_a_nonempty_list() {
        let (ast, interner, pat) = parse_pattern_in_a_match("[]");
        let env = Env::new();
        let list = Value::List(std::rc::Rc::new(std::cell::RefCell::new(vec![Value::Int(
            1,
        )])));
        assert!(!match_pattern(&ast, &interner, pat, &list, &env));
    }

    #[test]
    fn or_pattern_matches_if_any_alternative_matches() {
        let (ast, interner, pat) = parse_pattern_in_a_match("0 | 1");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
        assert!(!match_pattern(&ast, &interner, pat, &Value::Int(2), &env));
    }
}
