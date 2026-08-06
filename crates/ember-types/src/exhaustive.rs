use crate::adt::AdtRegistry;
use crate::ctor_set::{ctor_arg_types, ctor_set, CtorSet};
use crate::matrix::{default_matrix, specialize};
use crate::pat::{CtorId, Pat};
use crate::ty::Ty;
use ember_ast::{Ast, Interner, MatchArm};
use ember_diag::Diagnostic;
use ember_span::Span;
use rustc_hash::FxHashSet;

/// `NotUseful`: the query row adds nothing the matrix doesn't already
/// cover. `Useful`: it does, and each `Vec<Pat>` is one concrete witness
/// row (same column count as the query) demonstrating a value the matrix
/// misses — there can be more than one (e.g. two different missing ADT
/// variants at once).
#[derive(Debug, Clone, PartialEq)]
pub enum Usefulness {
    NotUseful,
    Useful(Vec<Vec<Pat>>),
}

pub fn is_useful(
    matrix: &[Vec<Pat>],
    q: &[Pat],
    col_types: &[Ty],
    adts: &AdtRegistry,
) -> Usefulness {
    if q.is_empty() {
        return if matrix.is_empty() {
            Usefulness::Useful(vec![vec![]])
        } else {
            Usefulness::NotUseful
        };
    }

    let (q_first, q_rest) = q.split_first().expect("checked non-empty above");
    let (ty_first, ty_rest) = col_types
        .split_first()
        .expect("q and col_types must have matching length");

    match q_first {
        Pat::Ctor(ctor, args) => {
            let arity = args.len();
            let specialized = specialize(ctor, arity, matrix);
            let mut new_q = args.clone();
            new_q.extend_from_slice(q_rest);
            let mut new_types = ctor_arg_types(ctor, ty_first, adts);
            new_types.extend_from_slice(ty_rest);
            match is_useful(&specialized, &new_q, &new_types, adts) {
                Usefulness::NotUseful => Usefulness::NotUseful,
                Usefulness::Useful(witnesses) => Usefulness::Useful(
                    witnesses
                        .into_iter()
                        .map(|w| {
                            let (sub, rest) = w.split_at(arity);
                            let mut row = vec![Pat::Ctor(ctor.clone(), sub.to_vec())];
                            row.extend_from_slice(rest);
                            row
                        })
                        .collect(),
                ),
            }
        }
        Pat::Wild => match ctor_set(ty_first, adts) {
            CtorSet::Finite(ctors) => {
                let present: FxHashSet<CtorId> = matrix
                    .iter()
                    .filter_map(|row| match row.first() {
                        Some(Pat::Ctor(c, _)) => Some(c.clone()),
                        _ => None,
                    })
                    .collect();
                let missing: Vec<&(CtorId, usize)> =
                    ctors.iter().filter(|(c, _)| !present.contains(c)).collect();

                if missing.is_empty() {
                    // Every constructor is present — try each one; useful if ANY branch is useful.
                    let mut all_witnesses = Vec::new();
                    for (ctor, arity) in &ctors {
                        let specialized = specialize(ctor, *arity, matrix);
                        let mut new_q = vec![Pat::Wild; *arity];
                        new_q.extend_from_slice(q_rest);
                        let mut new_types = ctor_arg_types(ctor, ty_first, adts);
                        new_types.extend_from_slice(ty_rest);
                        if let Usefulness::Useful(witnesses) =
                            is_useful(&specialized, &new_q, &new_types, adts)
                        {
                            for w in witnesses {
                                let (sub, rest) = w.split_at(*arity);
                                let mut row = vec![Pat::Ctor(ctor.clone(), sub.to_vec())];
                                row.extend_from_slice(rest);
                                all_witnesses.push(row);
                            }
                        }
                    }
                    if all_witnesses.is_empty() {
                        Usefulness::NotUseful
                    } else {
                        Usefulness::Useful(all_witnesses)
                    }
                } else {
                    // Some constructors are missing entirely. Whether the
                    // REST of the row is satisfiable doesn't depend on
                    // WHICH missing constructor we pick (a constructor
                    // with zero rows contributes nothing but wildcard
                    // columns to specialize()), so compute it once via
                    // the default matrix, then pair every missing
                    // constructor with every rest-witness.
                    let def = default_matrix(matrix);
                    match is_useful(&def, q_rest, ty_rest, adts) {
                        Usefulness::NotUseful => Usefulness::NotUseful,
                        Usefulness::Useful(rest_witnesses) => {
                            let mut all_witnesses = Vec::new();
                            for (ctor, arity) in &missing {
                                for w in &rest_witnesses {
                                    let mut row =
                                        vec![Pat::Ctor((*ctor).clone(), vec![Pat::Wild; *arity])];
                                    row.extend_from_slice(w);
                                    all_witnesses.push(row);
                                }
                            }
                            Usefulness::Useful(all_witnesses)
                        }
                    }
                }
            }
            CtorSet::Infinite => {
                let def = default_matrix(matrix);
                match is_useful(&def, q_rest, ty_rest, adts) {
                    Usefulness::NotUseful => Usefulness::NotUseful,
                    Usefulness::Useful(rest_witnesses) => Usefulness::Useful(
                        rest_witnesses
                            .into_iter()
                            .map(|w| {
                                let mut row = vec![Pat::Wild];
                                row.extend_from_slice(&w);
                                row
                            })
                            .collect(),
                    ),
                }
            }
        },
    }
}

pub fn fmt_pat(pat: &Pat, adts: &AdtRegistry, interner: &Interner) -> String {
    match pat {
        Pat::Wild => "_".to_string(),
        Pat::Ctor(ctor, args) => match ctor {
            CtorId::Variant(_, name) => {
                let name_str = interner.resolve(*name);
                if args.is_empty() {
                    name_str.to_string()
                } else {
                    let args_str: Vec<String> =
                        args.iter().map(|a| fmt_pat(a, adts, interner)).collect();
                    format!("{name_str}({})", args_str.join(", "))
                }
            }
            CtorId::Struct(id) => {
                let name_str = interner.resolve(adts.name_of(*id));
                let fields: Vec<_> = adts.struct_fields(*id).collect();
                let parts: Vec<String> = fields
                    .iter()
                    .zip(args.iter())
                    .map(|(f, a)| {
                        format!("{}: {}", interner.resolve(*f), fmt_pat(a, adts, interner))
                    })
                    .collect();
                format!("{name_str} {{ {} }}", parts.join(", "))
            }
            CtorId::Bool(b) => b.to_string(),
            CtorId::Int(n) => n.to_string(),
            CtorId::Float(bits) => f64::from_bits(*bits).to_string(),
            CtorId::Str(s) => format!("{:?}", interner.resolve(*s)),
            CtorId::Nil => "[]".to_string(),
            CtorId::Cons => fmt_list_chain(pat, adts, interner),
            CtorId::Tuple(_) => {
                let args_str: Vec<String> =
                    args.iter().map(|a| fmt_pat(a, adts, interner)).collect();
                format!("({})", args_str.join(", "))
            }
        },
    }
}

/// Renders a `Cons`-chain witness as `[a, b, ..]`-style list syntax,
/// walking the chain until it hits `Nil` (closed list) or anything else
/// (`Wild`, meaning an open-ended tail — rendered as `..`).
fn fmt_list_chain(pat: &Pat, adts: &AdtRegistry, interner: &Interner) -> String {
    let mut items = Vec::new();
    let mut cur = pat;
    loop {
        match cur {
            Pat::Ctor(CtorId::Cons, args) => {
                items.push(fmt_pat(&args[0], adts, interner));
                cur = &args[1];
            }
            Pat::Ctor(CtorId::Nil, _) => {
                return format!("[{}]", items.join(", "));
            }
            _ => {
                items.push("..".to_string());
                break;
            }
        }
    }
    format!("[{}]", items.join(", "))
}

pub fn check_exhaustive(
    ast: &Ast,
    interner: &Interner,
    adts: &AdtRegistry,
    arms: &[MatchArm],
    scrutinee_ty: &Ty,
    match_span: Span,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut matrix: Vec<Vec<Pat>> = Vec::new();
    let col_types = [scrutinee_ty.clone()];

    for arm in arms {
        let rows = crate::pat::lower_pattern(ast, adts, arm.pat);
        let mut any_reachable = false;
        for row in &rows {
            let q = [row.clone()];
            let reachable = !matches!(
                is_useful(&matrix, &q, &col_types, adts),
                Usefulness::NotUseful
            );
            if reachable {
                any_reachable = true;
            }
            // Tentatively add — lets a LATER alternative of the SAME
            // or-pattern arm see this one (`A | B`: B sees A). Popped
            // back out below if this arm turns out to have a guard.
            matrix.push(vec![row.clone()]);
        }
        if !any_reachable {
            diags.push(
                Diagnostic::warning("unreachable pattern")
                    .with_primary(arm.span, "this pattern can never match"),
            );
        }
        if arm.guard.is_some() {
            for _ in &rows {
                matrix.pop();
            }
        }
    }

    if let Usefulness::Useful(witnesses) = is_useful(&matrix, &[Pat::Wild], &col_types, adts) {
        let rendered: Vec<String> = witnesses
            .iter()
            .map(|w| fmt_pat(&w[0], adts, interner))
            .collect();
        diags.push(
            Diagnostic::error("non-exhaustive patterns")
                .with_primary(match_span, "this match is not exhaustive")
                .with_note(format!("missing: {}", rendered.join(", ")))
                .with_help("add a `_ => ...` arm to cover the remaining cases"),
        );
    }

    diags
}

use crate::infer::TypeInfo;
use ember_ast::{Expr, Idx, Stmt};

/// Walks every statement/expression reachable from the top level, calling
/// `check_exhaustive` on each `Expr::Match` found, using its
/// already-inferred (and now fully solved) scrutinee type from
/// `TypeInfo.expr_types`. A scrutinee whose type never got pinned down to
/// anything concrete (still an unresolved `Ty::Var`) is skipped — nothing
/// meaningful to check without a known constructor set.
pub fn check_exhaustiveness(
    ast: &Ast,
    interner: &Interner,
    info: &TypeInfo,
    stmts: &[Idx<Stmt>],
) -> Vec<Diagnostic> {
    let mut match_exprs = Vec::new();
    walk_stmts(ast, stmts, &mut match_exprs);

    let mut subst = info.subst.clone();
    let mut diags = Vec::new();
    for idx in match_exprs {
        let Expr::Match { scrutinee, arms } = ast.expr(idx).clone() else {
            unreachable!("walk_stmts only ever collects Expr::Match indices")
        };
        let Some(scrutinee_ty) = info.expr_types.get(&scrutinee) else {
            continue;
        };
        let resolved = subst.resolve(scrutinee_ty);
        if matches!(resolved, Ty::Var(_)) {
            continue;
        }
        diags.extend(check_exhaustive(
            ast,
            interner,
            &info.adts,
            &arms,
            &resolved,
            ast.span_of_expr(idx),
        ));
    }
    diags
}

fn walk_stmts(ast: &Ast, stmts: &[Idx<Stmt>], out: &mut Vec<Idx<Expr>>) {
    for &s in stmts {
        walk_stmt(ast, s, out);
    }
}

fn walk_stmt(ast: &Ast, idx: Idx<Stmt>, out: &mut Vec<Idx<Expr>>) {
    match ast.stmt(idx).clone() {
        Stmt::Let { init, .. } => walk_expr(ast, init, out),
        Stmt::ExprStmt(e) => walk_expr(ast, e, out),
        Stmt::Fn { body, .. } => walk_expr(ast, body, out),
        Stmt::While { cond, body } => {
            walk_expr(ast, cond, out);
            walk_expr(ast, body, out);
        }
        Stmt::For { iter, body, .. } => {
            walk_expr(ast, iter, out);
            walk_expr(ast, body, out);
        }
        Stmt::Loop { body } => walk_expr(ast, body, out),
        Stmt::Return(Some(e)) => walk_expr(ast, e, out),
        Stmt::Return(None)
        | Stmt::Break
        | Stmt::Continue
        | Stmt::TypeDecl { .. }
        | Stmt::StructDecl { .. }
        | Stmt::Error => {}
    }
}

fn walk_expr(ast: &Ast, idx: Idx<Expr>, out: &mut Vec<Idx<Expr>>) {
    match ast.expr(idx).clone() {
        Expr::Match { scrutinee, arms } => {
            out.push(idx);
            walk_expr(ast, scrutinee, out);
            for arm in &arms {
                if let Some(g) = arm.guard {
                    walk_expr(ast, g, out);
                }
                walk_expr(ast, arm.body, out);
            }
        }
        Expr::Unary { operand, .. } => walk_expr(ast, operand, out),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(ast, lhs, out);
            walk_expr(ast, rhs, out);
        }
        Expr::Assign { target, value } => {
            walk_expr(ast, target, out);
            walk_expr(ast, value, out);
        }
        Expr::Call { callee, args } => {
            walk_expr(ast, callee, out);
            for a in args {
                walk_expr(ast, a, out);
            }
        }
        Expr::Index { base, index } => {
            walk_expr(ast, base, out);
            walk_expr(ast, index, out);
        }
        Expr::Field { base, .. } => walk_expr(ast, base, out),
        Expr::Lambda { body, .. } => walk_expr(ast, body, out),
        Expr::If { cond, then_, else_ } => {
            walk_expr(ast, cond, out);
            walk_expr(ast, then_, out);
            if let Some(e) = else_ {
                walk_expr(ast, e, out);
            }
        }
        Expr::Block { stmts, tail } => {
            walk_stmts(ast, &stmts, out);
            if let Some(t) = tail {
                walk_expr(ast, t, out);
            }
        }
        Expr::List { items } => {
            for i in items {
                walk_expr(ast, i, out);
            }
        }
        Expr::Struct { fields, .. } => {
            for (_, v) in fields {
                walk_expr(ast, v, out);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Nil
        | Expr::Var(_)
        | Expr::Error => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::pat::{CtorId, Pat};
    use crate::ty::Ty;

    #[test]
    fn empty_matrix_makes_the_empty_query_useful() {
        let adts = AdtRegistry::new();
        let result = is_useful(&[], &[], &[], &adts);
        assert!(matches!(result, Usefulness::Useful(_)));
    }

    #[test]
    fn a_matrix_with_a_row_makes_the_empty_query_not_useful() {
        let adts = AdtRegistry::new();
        let matrix: Vec<Vec<Pat>> = vec![vec![]];
        let result = is_useful(&matrix, &[], &[], &adts);
        assert!(matches!(result, Usefulness::NotUseful));
    }

    #[test]
    fn wild_is_not_useful_once_bool_is_fully_covered() {
        let adts = AdtRegistry::new();
        let matrix = vec![
            vec![Pat::Ctor(CtorId::Bool(true), vec![])],
            vec![Pat::Ctor(CtorId::Bool(false), vec![])],
        ];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Bool], &adts);
        assert!(matches!(result, Usefulness::NotUseful));
    }

    #[test]
    fn wild_is_useful_when_bool_is_only_partially_covered() {
        let adts = AdtRegistry::new();
        let matrix = vec![vec![Pat::Ctor(CtorId::Bool(true), vec![])]];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Bool], &adts);
        match result {
            Usefulness::Useful(witnesses) => {
                assert_eq!(
                    witnesses,
                    vec![vec![Pat::Ctor(CtorId::Bool(false), vec![])]]
                );
            }
            Usefulness::NotUseful => panic!("expected useful"),
        }
    }

    #[test]
    fn wild_is_useful_against_an_infinite_domain_with_no_rows() {
        let adts = AdtRegistry::new();
        let result = is_useful(&[], &[Pat::Wild], &[Ty::Int], &adts);
        assert!(matches!(result, Usefulness::Useful(_)));
    }

    #[test]
    fn multiple_missing_variants_all_appear_as_witnesses() {
        let mut interner = ember_ast::Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let rect = interner.intern("Rect");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(
            shape,
            vec![
                (circle, vec![Ty::Float]),
                (rect, vec![Ty::Float, Ty::Float]),
                (point, vec![]),
            ],
        );
        let matrix = vec![vec![Pat::Ctor(
            CtorId::Variant(id, circle),
            vec![Pat::Wild],
        )]];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Adt(id, vec![])], &adts);
        match result {
            Usefulness::Useful(witnesses) => {
                assert_eq!(
                    witnesses.len(),
                    2,
                    "Rect and Point should both be missing: {witnesses:?}"
                );
                let has_rect = witnesses
                    .iter()
                    .any(|w| matches!(&w[0], Pat::Ctor(CtorId::Variant(_, n), _) if *n == rect));
                let has_point = witnesses
                    .iter()
                    .any(|w| matches!(&w[0], Pat::Ctor(CtorId::Variant(_, n), _) if *n == point));
                assert!(has_rect && has_point);
            }
            Usefulness::NotUseful => panic!("expected useful"),
        }
    }

    #[test]
    fn wild_renders_as_underscore() {
        let adts = AdtRegistry::new();
        let interner = ember_ast::Interner::new();
        assert_eq!(fmt_pat(&Pat::Wild, &adts, &interner), "_");
    }

    #[test]
    fn nullary_variant_renders_bare() {
        let mut interner = ember_ast::Interner::new();
        let shape = interner.intern("Shape");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(point, vec![])]);
        assert_eq!(
            fmt_pat(
                &Pat::Ctor(CtorId::Variant(id, point), vec![]),
                &adts,
                &interner
            ),
            "Point"
        );
    }

    #[test]
    fn variant_with_payload_renders_wildcarded_args() {
        let mut interner = ember_ast::Interner::new();
        let shape = interner.intern("Shape");
        let rect = interner.intern("Rect");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(rect, vec![Ty::Float, Ty::Float])]);
        let pat = Pat::Ctor(CtorId::Variant(id, rect), vec![Pat::Wild, Pat::Wild]);
        assert_eq!(fmt_pat(&pat, &adts, &interner), "Rect(_, _)");
    }

    #[test]
    fn list_renders_bracketed() {
        let adts = AdtRegistry::new();
        let interner = ember_ast::Interner::new();
        let nil = Pat::Ctor(CtorId::Nil, vec![]);
        assert_eq!(fmt_pat(&nil, &adts, &interner), "[]");
        let one = Pat::Ctor(
            CtorId::Cons,
            vec![Pat::Wild, Pat::Ctor(CtorId::Nil, vec![])],
        );
        assert_eq!(fmt_pat(&one, &adts, &interner), "[_]");
        let open = Pat::Ctor(CtorId::Cons, vec![Pat::Wild, Pat::Wild]);
        assert_eq!(fmt_pat(&open, &adts, &interner), "[_, ..]");
    }

    #[test]
    fn missing_one_adt_variant_is_named_in_the_error() {
        let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    Rect(w, h) => w,\n  }\n}\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let rect = interner.intern("Rect");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(
            shape,
            vec![
                (circle, vec![Ty::Float]),
                (rect, vec![Ty::Float, Ty::Float]),
                (point, vec![]),
            ],
        );
        let (arms, span) = find_match_arms(&ast, &stmts);
        let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Adt(id, vec![]), span);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("non-exhaustive"));
        let note = &diags[0].notes[0];
        assert!(note.contains("Point"), "note was: {note}");
    }

    #[test]
    fn wildcard_arm_makes_the_match_exhaustive() {
        let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    _ => 0.0,\n  }\n}\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(circle, vec![Ty::Float]), (point, vec![])]);
        let (arms, span) = find_match_arms(&ast, &stmts);
        let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Adt(id, vec![]), span);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn arm_after_wildcard_is_reported_unreachable() {
        let src = "fn f(n) {\n  match n {\n    _ => 1,\n    0 => 2,\n  }\n}\nprint(1);";
        let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let adts = AdtRegistry::new();
        let (arms, span) = find_match_arms(&ast, &stmts);
        let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Int, span);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unreachable"));
    }

    #[test]
    fn guarded_arm_never_counts_toward_exhaustiveness() {
        let src = "fn f(b) {\n  match b {\n    true if false => 1,\n  }\n}\nprint(1);";
        let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let adts = AdtRegistry::new();
        let (arms, span) = find_match_arms(&ast, &stmts);
        let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Bool, span);
        // Even though `true` is literally present, the guard means it can't be
        // assumed to cover `true` — both `true` and `false` should be reported
        // missing (a guarded arm contributes nothing to exhaustiveness).
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("non-exhaustive"));
    }

    #[test]
    fn check_exhaustiveness_finds_a_non_exhaustive_match_anywhere_in_the_program() {
        let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n  }\n}\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let (info, infer_diags) = crate::infer::infer(&ast, &mut interner, &stmts);
        assert!(infer_diags.is_empty(), "{infer_diags:?}");
        let diags = check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("non-exhaustive"));
    }

    #[test]
    fn an_unconstrained_scrutinee_type_is_skipped_without_panicking() {
        // A match whose scrutinee type never gets pinned to anything concrete
        // shouldn't crash the checker — just nothing meaningful to report.
        let src = "fn f(x) {\n  match x {\n    _ => 1,\n  }\n}\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let (info, infer_diags) = crate::infer::infer(&ast, &mut interner, &stmts);
        assert!(infer_diags.is_empty(), "{infer_diags:?}");
        let diags = check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert!(diags.is_empty());
    }

    fn find_match_arms<'a>(
        ast: &'a ember_ast::Ast,
        stmts: &[ember_ast::Idx<ember_ast::Stmt>],
    ) -> (&'a [ember_ast::MatchArm], Span) {
        for &s in stmts {
            if let ember_ast::Stmt::Fn { body, .. } = ast.stmt(s) {
                if let ember_ast::Expr::Block { tail: Some(t), .. } = ast.expr(*body) {
                    if let ember_ast::Expr::Match { arms, .. } = ast.expr(*t) {
                        return (arms, ast.span_of_expr(*t));
                    }
                }
            }
            if let ember_ast::Stmt::ExprStmt(e) = ast.stmt(s) {
                if let ember_ast::Expr::Match { arms, .. } = ast.expr(*e) {
                    return (arms, ast.span_of_expr(*e));
                }
            }
        }
        panic!("no match expression found")
    }
}
