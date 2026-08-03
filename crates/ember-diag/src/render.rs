use ariadne::{Color, Config, Label as AriadneLabel, Report, ReportKind, Source};

use crate::{Diagnostic, Severity};

/// Render a diagnostic to a string via ariadne. `use_color` is decided by the
/// caller (CLI checks `NO_COLOR` and whether stdout is a TTY); this function
/// just honors whatever it's told.
pub fn render(diag: &Diagnostic, filename: &str, src: &str, use_color: bool) -> String {
    let kind = match diag.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note | Severity::Help => ReportKind::Advice,
    };
    let offset = diag.primary_span().map(|s| s.start as usize).unwrap_or(0);
    let mut builder = Report::build(kind, filename, offset)
        .with_config(Config::default().with_color(use_color))
        .with_message(&diag.message);
    if let Some(code) = diag.code {
        builder = builder.with_code(code);
    }
    for label in &diag.labels {
        let color = if label.primary {
            Color::Red
        } else {
            Color::Blue
        };
        builder = builder.with_label(
            AriadneLabel::new((filename, label.span.start as usize..label.span.end as usize))
                .with_message(&label.message)
                .with_color(color),
        );
    }
    // ariadne 0.4.1's `ReportBuilder` holds a single `Option<String>` for
    // `note` and one for `help` (not a `Vec`), so calling `with_note`/
    // `with_help` per entry would silently keep only the last one. Join
    // multiple entries into one message instead of dropping data.
    if !diag.notes.is_empty() {
        builder = builder.with_note(diag.notes.join("\n"));
    }
    if !diag.help.is_empty() {
        let help_text = diag
            .help
            .iter()
            .map(|h| h.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        builder = builder.with_help(help_text);
    }
    let report = builder.finish();
    let mut buf = Vec::new();
    // Caller must ensure every span on `diag` is in-bounds for `src` — ariadne
    // indexes into the source text directly and can panic on a span from a
    // stale or mismatched source string, not just return an error.
    report
        .write((filename, Source::from(src)), &mut buf)
        .expect("ariadne render should not fail");
    String::from_utf8(buf).expect("ariadne output is valid utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Diagnostic;
    use ember_span::Span;

    #[test]
    fn render_includes_message_code_and_notes() {
        let diag = Diagnostic::error("type mismatch")
            .with_code("E0308")
            .with_primary(Span::new(0, 3), "expected Int, found String")
            .with_note("if is an expression");
        let out = render(&diag, "main.em", "abc", false);
        assert!(out.contains("type mismatch"));
        assert!(out.contains("E0308"));
        assert!(out.contains("expected Int, found String"));
        assert!(out.contains("if is an expression"));
    }

    #[test]
    fn render_without_color_has_no_ansi_escapes() {
        let diag = Diagnostic::error("oops").with_primary(Span::new(0, 1), "here");
        let out = render(&diag, "main.em", "x", false);
        assert!(
            !out.contains('\u{1b}'),
            "no-color output must contain no ANSI escapes"
        );
    }
}
