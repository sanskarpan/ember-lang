use ember_diag::Diagnostic;
use std::fs;
use std::path::PathBuf;

fn diagnostics_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/diagnostics")
}

fn collect_diagnostics(src: &str) -> Vec<Diagnostic> {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    if !parse_diags.is_empty() {
        return parse_diags;
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return resolve_diags;
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return infer_diags;
    }
    ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts)
}

#[test]
fn diagnostic_rendering_matches_snapshots() {
    let dir = diagnostics_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();
    assert!(
        entries.len() >= 4,
        "expected at least 4 diagnostics fixtures, found {}",
        entries.len()
    );

    for path in entries {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        let diags = collect_diagnostics(&src);
        assert!(
            !diags.is_empty(),
            "{path:?}: expected at least one diagnostic, got none"
        );

        let mut rendered = String::new();
        for d in &diags {
            rendered.push_str(&ember_diag::render::render(d, &name, &src, false));
            rendered.push('\n');
        }
        insta::assert_snapshot!(name, rendered);
    }
}
