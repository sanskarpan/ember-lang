use ember_diag::Severity;
use std::fs;
use std::path::PathBuf;

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

fn has_errors(diags: &[ember_diag::Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

#[test]
fn tree_walker_output_matches_every_captured_fixture() {
    let dir = conformance_dir();
    let mut checked = 0;
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();

    for path in entries {
        let expected_path = path.with_extension("expected");
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("missing {expected_path:?} for {path:?}: {e}"));

        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
        assert!(
            parse_diags.is_empty(),
            "{path:?}: parse diags: {parse_diags:?}"
        );

        let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let resolve_diags = resolver.diagnostics().to_vec();
        assert!(
            !has_errors(&resolve_diags),
            "{path:?}: resolve diags: {resolve_diags:?}"
        );

        let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        assert!(
            !has_errors(&infer_diags),
            "{path:?}: infer diags: {infer_diags:?}"
        );

        let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert!(
            !has_errors(&exhaustive_diags),
            "{path:?}: exhaustiveness diags: {exhaustive_diags:?}"
        );

        let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
        assert!(err.is_none(), "{path:?}: unexpected runtime error: {err:?}");
        let actual = match result {
            Some(v) => ember_tree::display_value(&v, &interner),
            None => String::new(),
        };
        assert_eq!(actual.trim(), expected.trim(), "{path:?}: output mismatch");
        checked += 1;
    }
    assert!(
        checked >= 6,
        "expected at least 6 conformance fixtures, found {checked} in {dir:?}"
    );
}
