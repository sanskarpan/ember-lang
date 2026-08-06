use crate::constraint::Constraint;
use crate::constraint::Origin;
use crate::ty::Ty;

/// One attempted unification, recorded in execution order — the data
/// behind the playground's Panel 4 (no consumer exists yet; built now per
/// explicit scope decision for this phase).
#[derive(Debug, Clone)]
pub struct UnifyStep {
    pub lhs: Ty,
    pub rhs: Ty,
    pub origin: Origin,
    pub succeeded: bool,
}

#[derive(Default)]
pub struct InferenceTrace {
    pub steps: Vec<UnifyStep>,
}

impl InferenceTrace {
    pub fn record(&mut self, constraint: Constraint, succeeded: bool) {
        self.steps.push(UnifyStep {
            lhs: constraint.lhs,
            rhs: constraint.rhs,
            origin: constraint.origin,
            succeeded,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraint::{Constraint, Origin};
    use crate::ty::Ty;
    use ember_span::Span;

    #[test]
    fn trace_accumulates_steps_in_order() {
        let mut trace = InferenceTrace::default();
        trace.record(
            Constraint {
                lhs: Ty::Int,
                rhs: Ty::Int,
                origin: Origin::WhileCond {
                    span: Span::new(0, 1),
                },
            },
            true,
        );
        trace.record(
            Constraint {
                lhs: Ty::Bool,
                rhs: Ty::Int,
                origin: Origin::WhileCond {
                    span: Span::new(2, 3),
                },
            },
            false,
        );
        assert_eq!(trace.steps.len(), 2);
        assert!(trace.steps[0].succeeded);
        assert!(!trace.steps[1].succeeded);
    }
}
