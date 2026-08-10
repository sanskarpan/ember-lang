use crate::doc::Doc;
use ember_span::{SourceMap, Span};

/// Formats a whole program: parses `src`, lowers every top-level
/// statement to a `Doc`, interleaves comments (obtained via a second,
/// independent lex pass) and blank-line preservation between top-level
/// items, and renders the result. Always returns a string ending in
/// exactly one `\n`.
///
/// Comments and statements are merged into ONE source-ordered sequence
/// of "items," and blank-line-gap detection runs uniformly over
/// consecutive items in that sequence — not as two separate passes (one
/// for comments, one for statement-to-statement gaps) that would
/// otherwise disagree about where a gap "really" is whenever a comment
/// sits between two statements. A trailing (same-source-line) comment is
/// the one exception: it's glued directly onto its statement's own
/// output with no gap check at all, since by definition it shares that
/// statement's line.
pub fn format(src: &str) -> String {
    let (ast, interner, stmts, _parse_diags) = ember_parser::parse(src);
    let (_tokens, trivia, _lex_diags) = ember_lexer::lex(src);
    let source_map = SourceMap::new(src);

    // Classify each trivia as either "trailing" (glued onto the
    // immediately preceding statement, same source line, no blank-line
    // gap logic) or "standalone" (leading/freestanding — takes part in
    // the same ordered, gap-checked sequence statements do). A trivia is
    // trailing-of-statement-i if it starts after that statement's own
    // span ends, on the SAME source line as that end, and before the
    // next statement's span begins.
    let mut trailing_of: Vec<Option<usize>> = vec![None; trivia.len()];
    for (ti, t) in trivia.iter().enumerate() {
        for (i, &s) in stmts.iter().enumerate() {
            let stmt_span = ast.span_of_stmt(s);
            if t.span.start < stmt_span.end {
                continue;
            }
            let next_start = stmts
                .get(i + 1)
                .map(|&next| ast.span_of_stmt(next).start)
                .unwrap_or(u32::MAX);
            if t.span.start >= next_start {
                continue;
            }
            let (stmt_end_line, _) = source_map.line_col(stmt_span.end);
            let (trivia_line, _) = source_map.line_col(t.span.start);
            if trivia_line == stmt_end_line {
                trailing_of[ti] = Some(i);
            }
            break;
        }
    }

    // The merged, source-ordered sequence of top-level items: every
    // statement, plus every NON-trailing trivia.
    enum Item {
        Stmt(usize),
        Trivia(usize),
    }
    let mut items: Vec<Item> = Vec::new();
    for i in 0..stmts.len() {
        items.push(Item::Stmt(i));
    }
    for (ti, trailing) in trailing_of.iter().enumerate() {
        if trailing.is_none() {
            items.push(Item::Trivia(ti));
        }
    }
    let item_span = |item: &Item| -> Span {
        match item {
            Item::Stmt(i) => ast.span_of_stmt(stmts[*i]),
            Item::Trivia(ti) => trivia[*ti].span,
        }
    };
    items.sort_by_key(|item| item_span(item).start);

    let mut out = Vec::new();
    let mut prev_end_line: Option<u32> = None;
    for item in &items {
        let span = item_span(item);
        let (start_line, _) = source_map.line_col(span.start);
        if let Some(prev_line) = prev_end_line {
            out.push(Doc::HardLine);
            if start_line > prev_line + 1 {
                out.push(Doc::HardLine);
            }
        }
        match item {
            Item::Stmt(i) => {
                out.push(crate::print_stmt::print_stmt(&ast, &interner, stmts[*i]));
                for (ti, t) in trivia.iter().enumerate() {
                    if trailing_of[ti] == Some(*i) {
                        out.push(Doc::text(" "));
                        out.push(Doc::text(
                            src[t.span.start as usize..t.span.end as usize].to_string(),
                        ));
                    }
                }
            }
            Item::Trivia(ti) => {
                let t = &trivia[*ti];
                out.push(Doc::text(
                    src[t.span.start as usize..t.span.end as usize].to_string(),
                ));
            }
        }
        let (end_line, _) = source_map.line_col(span.end);
        prev_end_line = Some(end_line);
    }

    let mut rendered = crate::render(Doc::concat(out), 100);
    while rendered.ends_with('\n') {
        rendered.pop();
    }
    rendered.push('\n');
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_every_conformance_fixture_produces_stable_non_empty_output() {
        // A lightweight stand-in for full snapshot testing (no snapshot
        // fixtures are checked in for this — the idempotence/semantics
        // tests elsewhere in this module are the real correctness proof):
        // confirms every fixture in the corpus formats to non-empty,
        // `\n`-terminated output without panicking.
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("em") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let formatted = format(&src);
            assert!(!formatted.is_empty(), "{path:?} formatted to empty output");
            assert!(
                formatted.ends_with('\n'),
                "{path:?} formatted output must end in \\n"
            );
            checked += 1;
        }
        assert!(
            checked >= 6,
            "expected at least 6 conformance fixtures, found {checked}"
        );
    }

    #[test]
    fn two_statements_get_a_hardline_between_them() {
        assert_eq!(format("let x = 1;\nx;\n"), "let x = 1;\nx;\n");
    }

    #[test]
    fn blank_line_between_top_level_items_is_preserved_capped_at_one() {
        assert_eq!(
            format("let x = 1;\n\n\n\nlet y = 2;\n"),
            "let x = 1;\n\nlet y = 2;\n"
        );
        assert_eq!(
            format("let x = 1;\nlet y = 2;\n"),
            "let x = 1;\nlet y = 2;\n"
        );
        assert_eq!(
            format("let x = 1;\n\nlet y = 2;\n"),
            "let x = 1;\n\nlet y = 2;\n"
        );
    }

    #[test]
    fn leading_line_comment_is_preserved_before_its_statement() {
        assert_eq!(format("// hello\nlet x = 1;\n"), "// hello\nlet x = 1;\n");
    }

    #[test]
    fn trailing_same_line_comment_is_preserved_after_its_statement() {
        assert_eq!(format("let x = 1; // hi\n"), "let x = 1; // hi\n");
    }

    #[test]
    fn a_leading_comment_with_no_blank_line_gains_none() {
        assert_eq!(
            format("let x = 1;\n// comment\nlet y = 2;\n"),
            "let x = 1;\n// comment\nlet y = 2;\n"
        );
    }

    #[test]
    fn a_standalone_comment_after_a_real_blank_line_keeps_the_blank_line_before_it() {
        assert_eq!(
            format("let x = 1;\n\n// standalone\nlet y = 2;\n"),
            "let x = 1;\n\n// standalone\nlet y = 2;\n"
        );
    }

    #[test]
    fn output_always_ends_with_exactly_one_trailing_newline() {
        assert_eq!(format("let x = 1;"), "let x = 1;\n");
        assert_eq!(format("let x = 1;\n\n\n"), "let x = 1;\n");
    }

    #[test]
    fn idempotent_on_the_full_conformance_suite() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("em") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let once = format(&src);
            let twice = format(&once);
            assert_eq!(once, twice, "not idempotent on {path:?}");
            checked += 1;
        }
        assert!(
            checked >= 6,
            "expected at least 6 conformance fixtures, found {checked}"
        );
    }

    #[test]
    fn semantics_preserved_on_the_full_conformance_suite() {
        // run(x) == run(fmt(x)) via the tree-walking interpreter (cheaper
        // to invoke here than standing up the VM's own pipeline).
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("em") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let formatted = format(&src);
            let before = run_via_tree_walker(&src);
            let after = run_via_tree_walker(&formatted);
            assert_eq!(before, after, "semantics changed for {path:?}");
            checked += 1;
        }
        assert!(
            checked >= 6,
            "expected at least 6 conformance fixtures, found {checked}"
        );
    }

    fn run_via_tree_walker(src: &str) -> String {
        let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        // `ember_tree::interpret`'s real signature is `(ast: &Ast, interner:
        // &Interner, stmts: &[Idx<Stmt>]) -> (Option<Value>,
        // Option<RuntimeError>)` — no `Bindings` parameter (the tree-walker
        // does dynamic environment lookup, not resolved binding).
        let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
        format!("{result:?} {err:?}")
    }
}
