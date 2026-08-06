use ember_diag::Diagnostic;
use ember_span::Span;

#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
    /// Call-site spans accumulated as a "stack overflow" error unwinds
    /// through `eval_call`, innermost first — lets the diagnostic show the
    /// real call chain instead of just where the limit was hit.
    pub call_stack: Vec<Span>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        RuntimeError {
            message: message.into(),
            span,
            call_stack: Vec::new(),
        }
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        let mut diag = Diagnostic::error(self.message.clone()).with_primary(self.span, "here");
        for (i, frame) in self.call_stack.iter().enumerate() {
            diag = diag.with_secondary(*frame, format!("in call frame {}", i + 1));
        }
        diag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_span::Span;

    #[test]
    fn to_diagnostic_carries_the_primary_span() {
        let err = RuntimeError::new("division by zero", Span::new(3, 6));
        let diag = err.to_diagnostic();
        assert_eq!(diag.message, "division by zero");
        assert_eq!(diag.labels[0].span, Span::new(3, 6));
    }

    #[test]
    fn call_stack_frames_become_secondary_labels() {
        let mut err = RuntimeError::new("stack overflow", Span::new(0, 1));
        err.call_stack.push(Span::new(10, 20));
        err.call_stack.push(Span::new(30, 40));
        let diag = err.to_diagnostic();
        assert_eq!(diag.labels.len(), 3); // 1 primary + 2 call-stack frames
    }
}
