use crate::ty::Ty;
use ember_ast::Symbol;
use ember_span::Span;

/// Every comparison unify() is asked to make carries WHY the two types are
/// expected to match — this is what lets error messages say "these two
/// `if` branches disagree" instead of "Int != String" with no context.
#[derive(Debug, Clone)]
pub struct Constraint {
    pub lhs: Ty,
    pub rhs: Ty,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub enum Origin {
    IfBranches {
        if_span: Span,
        then_span: Span,
        else_span: Span,
    },
    CallArgument {
        call_span: Span,
        arg_span: Span,
        param_idx: usize,
        fn_name: Option<Symbol>,
    },
    BinaryOp {
        op_span: Span,
        lhs_span: Span,
        rhs_span: Span,
    },
    Annotation {
        annot_span: Span,
        value_span: Span,
    },
    MatchArms {
        first_span: Span,
        this_span: Span,
    },
    Return {
        fn_span: Span,
        expr_span: Span,
    },
    ListElement {
        list_span: Span,
        elem_span: Span,
        index: usize,
    },
    WhileCond {
        span: Span,
    },
    IndexTarget {
        span: Span,
    },
}

impl Origin {
    /// A best-effort single span for diagnostics that don't need every
    /// contributing span individually labeled (e.g. the field-access
    /// "needs annotation" error, which has no dedicated Origin variant).
    pub fn primary_span(&self) -> Span {
        match self {
            Origin::IfBranches { if_span, .. } => *if_span,
            Origin::CallArgument { arg_span, .. } => *arg_span,
            Origin::BinaryOp { op_span, .. } => *op_span,
            Origin::Annotation { value_span, .. } => *value_span,
            Origin::MatchArms { this_span, .. } => *this_span,
            Origin::Return { expr_span, .. } => *expr_span,
            Origin::ListElement { elem_span, .. } => *elem_span,
            Origin::WhileCond { span } => *span,
            Origin::IndexTarget { span } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_span::Span;

    #[test]
    fn origin_variants_carry_their_spans() {
        let o = Origin::IfBranches {
            if_span: Span::new(0, 2),
            then_span: Span::new(3, 4),
            else_span: Span::new(5, 6),
        };
        match o {
            Origin::IfBranches { if_span, .. } => assert_eq!(if_span, Span::new(0, 2)),
            _ => unreachable!(),
        }
    }
}
