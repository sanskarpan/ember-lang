use ember_ast::{Interner, Symbol};
use ember_diag::Diagnostic;
use ember_span::Span;

#[derive(Debug, Clone)]
pub struct TraceFrame {
    pub function_name: Symbol,
    pub line: u32,
}

/// A VM-local runtime error. `line` is a byte-offset stand-in (see this
/// file's own header note), not a real 1-based source line — it becomes a
/// zero-width `Span` only when rendered.
#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub line: u32,
    /// Enclosing call frames at the point of failure, innermost first —
    /// does NOT include the frame the error itself occurred in (that's
    /// `message`/`line`), matching `ember-tree::RuntimeError.call_stack`'s
    /// own "innermost first, callers only" convention.
    pub trace: Vec<TraceFrame>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, line: u32) -> Self {
        RuntimeError {
            message: message.into(),
            line,
            trace: Vec::new(),
        }
    }

    pub fn to_diagnostic(&self, interner: &Interner) -> Diagnostic {
        let mut diag = Diagnostic::error(self.message.clone())
            .with_primary(Span::new(self.line, self.line), "here");
        for frame in &self.trace {
            diag = diag.with_secondary(
                Span::new(frame.line, frame.line),
                format!("in {}", interner.resolve(frame.function_name)),
            );
        }
        diag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;

    #[test]
    fn to_diagnostic_carries_the_primary_span() {
        let err = RuntimeError::new("division by zero", 6);
        let interner = Interner::new();
        let diag = err.to_diagnostic(&interner);
        assert_eq!(diag.message, "division by zero");
        assert_eq!(diag.labels[0].span, ember_span::Span::new(6, 6));
    }

    #[test]
    fn trace_frames_become_secondary_labels_with_resolved_names() {
        let mut interner = Interner::new();
        let f = interner.intern("f");
        let g = interner.intern("g");
        let mut err = RuntimeError::new("stack overflow", 1);
        err.trace.push(TraceFrame {
            function_name: f,
            line: 10,
        });
        err.trace.push(TraceFrame {
            function_name: g,
            line: 20,
        });
        let diag = err.to_diagnostic(&interner);
        assert_eq!(diag.labels.len(), 3); // 1 primary + 2 trace frames
        assert!(diag.labels[1].message.contains('f'));
        assert!(diag.labels[2].message.contains('g'));
    }
}
