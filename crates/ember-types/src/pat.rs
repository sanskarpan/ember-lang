use crate::adt::AdtRegistry;
use crate::ty::AdtId;
use ember_ast::{Ast, Idx, Pattern, Symbol};

/// The exhaustiveness algorithm's own normalized pattern shape — simpler
/// than `ember_ast::Pattern`. No `Or` variant: or-patterns expand into
/// multiple `Pat` rows at lowering time (a later task adds `lower_pattern`
/// to do this), not threaded through the algorithm itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Wild,
    Ctor(CtorId, Vec<Pat>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CtorId {
    Variant(AdtId, Symbol),
    Struct(AdtId),
    Bool(bool),
    Int(i64),
    /// `f64`'s bit pattern, not the float itself — `f64` isn't `Eq`/`Hash`,
    /// and this algorithm needs both to track "which constructors are
    /// already present in a matrix column" via a set.
    Float(u64),
    Str(Symbol),
    Nil,
    Cons,
    /// Pre-existing gap from Phase 5: no `Ty::Tuple`/`Expr::Tuple` exist in
    /// the grammar, so a tuple pattern is treated as a single,
    /// always-complete constructor of whatever arity first appears —
    /// inert by construction, matching how Phase 5 already left it.
    Tuple(usize),
}

pub fn lower_pattern(ast: &Ast, adts: &AdtRegistry, idx: Idx<Pattern>) -> Vec<Pat> {
    match ast.pat(idx).clone() {
        Pattern::Wild | Pattern::Bind(_) | Pattern::Error => vec![Pat::Wild],
        Pattern::Int(n) => vec![Pat::Ctor(CtorId::Int(n), vec![])],
        Pattern::Float(f) => vec![Pat::Ctor(CtorId::Float(f.to_bits()), vec![])],
        Pattern::Str(s) => vec![Pat::Ctor(CtorId::Str(s), vec![])],
        Pattern::Bool(b) => vec![Pat::Ctor(CtorId::Bool(b), vec![])],
        Pattern::Ctor { name, args } => match adts.variant(name) {
            Some((id, _payload)) => {
                let arg_alts: Vec<Vec<Pat>> =
                    args.iter().map(|&a| lower_pattern(ast, adts, a)).collect();
                cartesian_ctor(CtorId::Variant(id, name), arg_alts)
            }
            // Unknown constructor — already reported elsewhere (type
            // inference, or this is unreachable in practice). Don't
            // cascade a second diagnostic; treat as inert.
            None => vec![Pat::Wild],
        },
        Pattern::Tuple(items) => {
            let arg_alts: Vec<Vec<Pat>> =
                items.iter().map(|&i| lower_pattern(ast, adts, i)).collect();
            let arity = items.len();
            cartesian_ctor(CtorId::Tuple(arity), arg_alts)
        }
        Pattern::List { items, rest } => lower_list(ast, adts, &items, rest),
        Pattern::Record { name, fields } => match adts.id_of(name) {
            Some(id) if adts.is_struct(id) => {
                let declared: Vec<Symbol> = adts.struct_fields(id).collect();
                let arg_alts: Vec<Vec<Pat>> = declared
                    .iter()
                    .map(|field_name| {
                        fields
                            .iter()
                            .find(|(f, _)| f == field_name)
                            .map(|(_, pat_idx)| lower_pattern(ast, adts, *pat_idx))
                            .unwrap_or_else(|| vec![Pat::Wild])
                    })
                    .collect();
                cartesian_ctor(CtorId::Struct(id), arg_alts)
            }
            _ => vec![Pat::Wild],
        },
        Pattern::Or(alts) => alts
            .iter()
            .flat_map(|&a| lower_pattern(ast, adts, a))
            .collect(),
    }
}

/// Lowers a list pattern into a Cons-chain: `[]` → `Nil`; `[a]` (no rest)
/// → `Cons(a, Nil)` (exact length 1); `[a, ..rest]` → `Cons(a, rest)`
/// where `rest`'s own lowering (typically `Wild`, from a plain binding)
/// represents "any remaining tail".
fn lower_list(
    ast: &Ast,
    adts: &AdtRegistry,
    items: &[Idx<Pattern>],
    rest: Option<Idx<Pattern>>,
) -> Vec<Pat> {
    match items.split_first() {
        None => match rest {
            None => vec![Pat::Ctor(CtorId::Nil, vec![])],
            Some(r) => lower_pattern(ast, adts, r),
        },
        Some((&head, tail_items)) => {
            let head_alts = lower_pattern(ast, adts, head);
            let tail_alts = lower_list(ast, adts, tail_items, rest);
            cartesian_ctor(CtorId::Cons, vec![head_alts, tail_alts])
        }
    }
}

/// The cartesian product of every argument's own lowered alternatives,
/// each combination wrapped as one `Ctor(ctor, combo)`. Needed because an
/// or-pattern can appear NESTED inside a constructor's arguments (e.g.
/// `Circle(1.0 | 2.0)`), not just at an arm's top level — every level must
/// expand fully.
fn cartesian_ctor(ctor: CtorId, arg_alts: Vec<Vec<Pat>>) -> Vec<Pat> {
    let mut combos: Vec<Vec<Pat>> = vec![vec![]];
    for alts in arg_alts {
        let mut next = Vec::new();
        for prefix in &combos {
            for alt in &alts {
                let mut combo = prefix.clone();
                combo.push(alt.clone());
                next.push(combo);
            }
        }
        combos = next;
    }
    combos
        .into_iter()
        .map(|args| Pat::Ctor(ctor.clone(), args))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::AdtId;

    #[test]
    fn ctor_id_equality_is_by_value() {
        let mut interner = ember_ast::Interner::new();
        let sym = interner.intern("Circle");
        assert_eq!(CtorId::Bool(true), CtorId::Bool(true));
        assert_ne!(CtorId::Bool(true), CtorId::Bool(false));
        assert_eq!(
            CtorId::Variant(AdtId(0), sym),
            CtorId::Variant(AdtId(0), sym)
        );
    }

    #[test]
    fn wild_and_ctor_pats_construct() {
        let p = Pat::Ctor(CtorId::Nil, vec![]);
        assert!(matches!(p, Pat::Ctor(CtorId::Nil, _)));
        assert!(matches!(Pat::Wild, Pat::Wild));
    }
}

#[cfg(test)]
mod lower_tests {
    use super::*;
    use crate::adt::AdtRegistry;

    #[test]
    fn wild_and_bind_lower_to_a_single_wild() {
        let (ast, interner, stmts, diags) = ember_parser::parse("match 1 { _ => 1, x => x, }");
        assert!(diags.is_empty());
        let arms = match_arms(&ast, &stmts);
        let adts = AdtRegistry::new();
        assert_eq!(lower_pattern(&ast, &adts, arms[0].pat), vec![Pat::Wild]);
        assert_eq!(lower_pattern(&ast, &adts, arms[1].pat), vec![Pat::Wild]);
        let _ = interner;
    }

    #[test]
    fn literals_lower_to_their_own_ctor() {
        let (ast, interner, stmts, diags) = ember_parser::parse("match 1 { 0 => 1, _ => 2, }");
        assert!(diags.is_empty());
        let arms = match_arms(&ast, &stmts);
        let adts = AdtRegistry::new();
        assert_eq!(
            lower_pattern(&ast, &adts, arms[0].pat),
            vec![Pat::Ctor(CtorId::Int(0), vec![])]
        );
        let _ = interner;
    }

    #[test]
    fn ctor_pattern_lowers_using_the_registry() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse(
            "type Shape = | Circle(Float);\nmatch s { Circle(r) => r, _ => 0.0, }",
        );
        assert!(diags.is_empty());
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(circle, vec![crate::ty::Ty::Float])]);
        let arms = match_arms(&ast, &stmts);
        assert_eq!(
            lower_pattern(&ast, &adts, arms[0].pat),
            vec![Pat::Ctor(CtorId::Variant(id, circle), vec![Pat::Wild])]
        );
    }

    #[test]
    fn tuple_pattern_lowers_to_a_single_always_complete_ctor() {
        // No Ty::Tuple/Expr::Tuple exist — this is inert by construction, but
        // must still lower without panicking and without spuriously reporting
        // gaps for a type nothing can construct.
        let (ast, interner, stmts, diags) = ember_parser::parse("match s { (a, b) => a, _ => 1, }");
        assert!(diags.is_empty());
        let adts = AdtRegistry::new();
        let arms = match_arms(&ast, &stmts);
        let lowered = lower_pattern(&ast, &adts, arms[0].pat);
        assert_eq!(lowered.len(), 1);
        assert!(matches!(&lowered[0], Pat::Ctor(CtorId::Tuple(2), _)));
        let _ = interner;
    }

    #[test]
    fn empty_list_pattern_lowers_to_nil() {
        let (ast, interner, stmts, diags) = ember_parser::parse("match xs { [] => 1, _ => 2, }");
        assert!(diags.is_empty());
        let adts = AdtRegistry::new();
        let arms = match_arms(&ast, &stmts);
        assert_eq!(
            lower_pattern(&ast, &adts, arms[0].pat),
            vec![Pat::Ctor(CtorId::Nil, vec![])]
        );
        let _ = interner;
    }

    #[test]
    fn list_pattern_with_rest_lowers_to_a_cons_chain() {
        let (ast, interner, stmts, diags) =
            ember_parser::parse("match xs { [a, ..rest] => a, _ => 2, }");
        assert!(diags.is_empty());
        let adts = AdtRegistry::new();
        let arms = match_arms(&ast, &stmts);
        let lowered = lower_pattern(&ast, &adts, arms[0].pat);
        assert_eq!(lowered.len(), 1);
        // Cons(Wild, Wild) — the head binding and the rest binding both lower
        // to Wild (a Bind matches anything; its NAME doesn't matter here).
        assert_eq!(
            lowered[0],
            Pat::Ctor(CtorId::Cons, vec![Pat::Wild, Pat::Wild])
        );
        let _ = interner;
    }

    #[test]
    fn record_pattern_fills_unmentioned_fields_with_wild() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse(
            "struct Point { x: Float, y: Float }\nmatch p { Point { x } => x, _ => 0.0, }",
        );
        assert!(diags.is_empty());
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let y = interner.intern("y");
        let mut adts = AdtRegistry::new();
        let id = adts.register_struct(
            point,
            vec![(x, crate::ty::Ty::Float), (y, crate::ty::Ty::Float)],
        );
        let arms = match_arms(&ast, &stmts);
        let lowered = lower_pattern(&ast, &adts, arms[0].pat);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered[0],
            Pat::Ctor(CtorId::Struct(id), vec![Pat::Wild, Pat::Wild])
        );
    }

    #[test]
    fn or_pattern_expands_into_multiple_rows() {
        let (ast, interner, stmts, diags) =
            ember_parser::parse("match n { 0 | 1 => \"small\", _ => \"other\", }");
        assert!(diags.is_empty());
        let adts = AdtRegistry::new();
        let arms = match_arms(&ast, &stmts);
        let lowered = lower_pattern(&ast, &adts, arms[0].pat);
        assert_eq!(
            lowered,
            vec![
                Pat::Ctor(CtorId::Int(0), vec![]),
                Pat::Ctor(CtorId::Int(1), vec![]),
            ]
        );
        let _ = interner;
    }

    /// Test helper: find the first `match` expression among the parsed
    /// top-level statements and return its arms. Handles the source
    /// possibly having other statements before it (e.g. a `type` decl).
    fn match_arms<'a>(
        ast: &'a ember_ast::Ast,
        stmts: &[ember_ast::Idx<ember_ast::Stmt>],
    ) -> &'a [ember_ast::MatchArm] {
        for &s in stmts {
            if let ember_ast::Stmt::ExprStmt(e) = ast.stmt(s) {
                if let ember_ast::Expr::Match { arms, .. } = ast.expr(*e) {
                    return arms;
                }
            }
        }
        panic!("no match expression found in parsed statements")
    }
}
