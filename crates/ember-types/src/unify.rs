use crate::adt::AdtRegistry;
use crate::constraint::{Constraint, Origin};
use crate::display::display_ty;
use crate::subst::Subst;
use crate::trace::InferenceTrace;
use crate::ty::Ty;
use ember_ast::Interner;
use ember_diag::Diagnostic;

/// Eagerly unifies `a` and `b`, resolving both through `subst` first and
/// binding immediately on success. Every call is self-contained (no
/// deferred queue) — "constraint generation separate from solving" here
/// means every comparison carries an `Origin`, not that solving is batched
/// to the end. Returns `true` on success; on failure, pushes exactly one
/// diagnostic and returns `false` so the caller can substitute a recovery
/// type and keep going rather than aborting the whole inference pass.
#[allow(clippy::too_many_arguments)]
pub fn unify(
    a: &Ty,
    b: &Ty,
    origin: Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
    trace: &mut InferenceTrace,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let ra = subst.resolve(a);
    let rb = subst.resolve(b);
    let constraint = Constraint {
        lhs: ra.clone(),
        rhs: rb.clone(),
        origin: origin.clone(),
    };

    let ok = unify_resolved(&ra, &rb, &origin, subst, adts, interner, diagnostics);
    trace.record(constraint, ok);
    ok
}

#[allow(clippy::too_many_arguments)]
fn unify_resolved(
    a: &Ty,
    b: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match (a, b) {
        (Ty::Var(v1), Ty::Var(v2)) if v1 == v2 => true,

        (Ty::Var(v), t) | (t, Ty::Var(v)) => {
            if subst.occurs_in(*v, t) {
                diagnostics.push(infinite_type_error(*v, t, origin, subst, adts, interner));
                return false;
            }
            subst.bind(*v, t.clone());
            true
        }

        (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
            if p1.len() != p2.len() {
                diagnostics.push(arity_error(p1.len(), p2.len(), origin));
                return false;
            }
            let mut ok = true;
            for (x, y) in p1.iter().zip(p2) {
                ok &= unify_resolved(x, y, origin, subst, adts, interner, diagnostics);
            }
            ok & unify_resolved(r1, r2, origin, subst, adts, interner, diagnostics)
        }

        (Ty::List(x), Ty::List(y)) => {
            unify_resolved(x, y, origin, subst, adts, interner, diagnostics)
        }

        (Ty::Adt(id1, a1), Ty::Adt(id2, a2)) if id1 == id2 => {
            let mut ok = true;
            for (x, y) in a1.iter().zip(a2) {
                ok &= unify_resolved(x, y, origin, subst, adts, interner, diagnostics);
            }
            ok
        }

        (Ty::Record(f1), Ty::Record(f2)) if f1.len() == f2.len() => {
            let mut ok = true;
            for (k, v1) in f1 {
                match f2.get(k) {
                    Some(v2) => {
                        ok &= unify_resolved(v1, v2, origin, subst, adts, interner, diagnostics)
                    }
                    None => ok = false,
                }
            }
            if !ok {
                diagnostics.push(mismatch_error(a, b, origin, subst, adts, interner));
            }
            ok
        }

        (x, y) if x == y => true,

        // Int/Float specifically get a conversion hint, since it's the
        // single most common near-miss a numeric-literal-heavy language
        // produces.
        (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
            let diag = mismatch_error(a, b, origin, subst, adts, interner)
                .with_help("convert explicitly with `int(..)` or `float(..)`".to_string());
            diagnostics.push(diag);
            false
        }

        _ => {
            diagnostics.push(mismatch_error(a, b, origin, subst, adts, interner));
            false
        }
    }
}

fn infinite_type_error(
    v: crate::ty::TyVarId,
    t: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> Diagnostic {
    let var_str = display_ty(&Ty::Var(v), subst, adts, interner);
    let t_str = display_ty(t, subst, adts, interner);
    Diagnostic::error(format!("infinite type: `{var_str}` occurs in `{t_str}`"))
        .with_primary(origin.primary_span(), "while trying to unify these types")
        .with_help("a type cannot contain itself — this usually comes from a function whose return type depends on calling itself with its own type")
}

fn arity_error(expected: usize, found: usize, origin: &Origin) -> Diagnostic {
    Diagnostic::error(format!("expected {expected} argument(s), found {found}")).with_primary(
        origin.primary_span(),
        format!("expected {expected}, found {found}"),
    )
}

fn mismatch_error(
    a: &Ty,
    b: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> Diagnostic {
    let a_str = display_ty(a, subst, adts, interner);
    let b_str = display_ty(b, subst, adts, interner);
    match origin {
        Origin::IfBranches {
            if_span,
            then_span,
            else_span,
        } => Diagnostic::error("type mismatch in `if` branches")
            .with_secondary(*if_span, "this `if` expression must have a single type")
            .with_primary(*then_span, format!("this branch has type `{a_str}`"))
            .with_primary(*else_span, format!("this branch has type `{b_str}`"))
            .with_help("both branches of an `if` must produce the same type"),
        Origin::CallArgument {
            call_span,
            arg_span,
            param_idx,
            fn_name,
        } => Diagnostic::error("argument type mismatch")
            .with_primary(*arg_span, format!("expected `{a_str}`, found `{b_str}`"))
            .with_secondary(
                *call_span,
                match fn_name {
                    Some(n) => format!(
                        "in this call to `{}` (argument {})",
                        interner.resolve(*n),
                        param_idx + 1
                    ),
                    None => format!("in this call (argument {})", param_idx + 1),
                },
            ),
        Origin::BinaryOp {
            op_span,
            lhs_span,
            rhs_span,
        } => Diagnostic::error("operand type mismatch")
            .with_primary(*lhs_span, format!("this has type `{a_str}`"))
            .with_primary(*rhs_span, format!("this has type `{b_str}`"))
            .with_secondary(*op_span, "in this operator expression"),
        Origin::Annotation {
            annot_span,
            value_span,
        } => Diagnostic::error("type does not match its annotation")
            .with_primary(*annot_span, format!("annotated as `{a_str}`"))
            .with_primary(*value_span, format!("but this has type `{b_str}`")),
        Origin::MatchArms {
            first_span,
            this_span,
        } => Diagnostic::error("match arms have different types")
            .with_primary(*first_span, format!("first arm has type `{a_str}`"))
            .with_primary(*this_span, format!("this arm has type `{b_str}`")),
        Origin::Return { fn_span, expr_span } => Diagnostic::error("return type mismatch")
            .with_secondary(*fn_span, format!("this function should return `{a_str}`"))
            .with_primary(*expr_span, format!("but this returns `{b_str}`")),
        Origin::ListElement {
            list_span,
            elem_span,
            index,
        } => Diagnostic::error("list elements have different types")
            .with_secondary(
                *list_span,
                format!("this list's elements have type `{a_str}`"),
            )
            .with_primary(*elem_span, format!("element {index} has type `{b_str}`")),
        Origin::WhileCond { span } => {
            Diagnostic::error(format!("expected `Bool`, found `{b_str}`"))
                .with_primary(*span, "a `while` condition must be a Bool")
        }
        Origin::IndexTarget { span } => {
            Diagnostic::error(format!("expected `{a_str}`, found `{b_str}`"))
                .with_primary(*span, "type mismatch here")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::constraint::Origin;
    use crate::subst::Subst;
    use crate::trace::InferenceTrace;
    use crate::ty::Ty;
    use ember_ast::Interner;
    use ember_span::Span;

    fn origin() -> Origin {
        Origin::WhileCond {
            span: Span::new(0, 1),
        }
    }

    #[test]
    fn identical_concrete_types_unify() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(unify(
            &Ty::Int,
            &Ty::Int,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert!(diags.is_empty());
    }

    #[test]
    fn mismatched_concrete_types_fail_with_a_diagnostic() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(!unify(
            &Ty::Int,
            &Ty::Bool,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn a_var_binds_to_a_concrete_type() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = subst.fresh();
        assert!(unify(
            &a,
            &Ty::Int,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert_eq!(subst.resolve(&a), Ty::Int);
    }

    #[test]
    fn occurs_check_rejects_an_infinite_type() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = subst.fresh();
        let self_fun = Ty::Fun(vec![a.clone()], Box::new(Ty::Bool));
        assert!(!unify(
            &a,
            &self_fun,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert!(diags[0].message.to_lowercase().contains("infinite"));
    }

    #[test]
    fn fun_arity_mismatch_is_a_dedicated_error() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = Ty::Fun(vec![Ty::Int, Ty::Int], Box::new(Ty::Bool));
        let b = Ty::Fun(vec![Ty::Int], Box::new(Ty::Bool));
        assert!(!unify(
            &a,
            &b,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert!(diags[0].message.contains('2') && diags[0].message.contains('1'));
    }

    #[test]
    fn list_and_adt_unify_structurally() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![]);
        let mut subst = Subst::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(unify(
            &Ty::List(Box::new(Ty::Int)),
            &Ty::List(Box::new(Ty::Int)),
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert!(unify(
            &Ty::Adt(id, vec![]),
            &Ty::Adt(id, vec![]),
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
    }

    #[test]
    fn int_float_mismatch_suggests_a_conversion() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(!unify(
            &Ty::Int,
            &Ty::Float,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        let help = diags[0].help.first().expect("expected a help suggestion");
        assert!(
            help.message.to_lowercase().contains("convert")
                || help.message.to_lowercase().contains("int(")
                || help.message.to_lowercase().contains("float(")
        );
    }

    #[test]
    fn every_attempt_is_recorded_in_the_trace() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        unify(
            &Ty::Int,
            &Ty::Int,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags,
        );
        unify(
            &Ty::Int,
            &Ty::Bool,
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags,
        );
        assert_eq!(trace.steps.len(), 2);
    }
}
