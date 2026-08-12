use ember_span::Span;

pub mod explain;
pub mod render;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub span: Span,
    pub replacement: String,
}

#[derive(Debug, Clone)]
pub struct Help {
    pub message: String,
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub help: Vec<Help>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(message)
        }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_primary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
            primary: true,
        });
        self
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
            primary: false,
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.help.push(Help {
            message: message.into(),
            suggestion: None,
        });
        self
    }

    pub fn with_suggestion(
        mut self,
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        self.help.push(Help {
            message: message.into(),
            suggestion: Some(Suggestion {
                span,
                replacement: replacement.into(),
            }),
        });
        self
    }

    /// The first primary-labeled span, or the first label of any kind if none is primary.
    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.primary)
            .map(|l| l.span)
            .or_else(|| self.labels.first().map(|l| l.span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_span::Span;

    #[test]
    fn builder_chains_labels_notes_help() {
        let d = Diagnostic::error("type mismatch")
            .with_code("E0308")
            .with_primary(Span::new(10, 12), "expected Int")
            .with_secondary(Span::new(0, 3), "because of this")
            .with_note("if is an expression")
            .with_help("add a conversion");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, Some("E0308"));
        assert_eq!(d.labels.len(), 2);
        assert!(d.labels[0].primary);
        assert!(!d.labels[1].primary);
        assert_eq!(d.notes.len(), 1);
        assert_eq!(d.help.len(), 1);
    }

    #[test]
    fn primary_span_prefers_primary_label() {
        let d = Diagnostic::error("x")
            .with_secondary(Span::new(0, 1), "secondary")
            .with_primary(Span::new(5, 6), "primary");
        assert_eq!(d.primary_span(), Some(Span::new(5, 6)));
    }

    #[test]
    fn warning_has_warning_severity() {
        let d = Diagnostic::warning("unused variable");
        assert_eq!(d.severity, Severity::Warning);
    }
}
