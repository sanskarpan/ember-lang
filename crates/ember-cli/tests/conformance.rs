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
fn both_backends_produce_identical_output_matching_every_captured_fixture() {
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

        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
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

        let (tree_result, tree_err) = ember_tree::interpret(&ast, &interner, &stmts);
        assert!(
            tree_err.is_none(),
            "{path:?}: tree-walker runtime error: {tree_err:?}"
        );
        let tree_actual = match tree_result {
            Some(v) => ember_tree::display_value(&v, &interner),
            None => String::new(),
        };
        assert_eq!(
            tree_actual.trim(),
            expected.trim(),
            "{path:?}: tree-walker output mismatch"
        );

        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let mut vm = ember_vm::vm::Vm::new(proto);
        let vm_actual = match vm.run() {
            Ok(v) => ember_vm::value::display_value(&v),
            Err(e) => panic!(
                "{path:?}: VM runtime error: {}",
                e.to_diagnostic(&interner).message
            ),
        };
        assert_eq!(
            vm_actual.trim(),
            expected.trim(),
            "{path:?}: VM output mismatch"
        );
        assert_eq!(
            tree_actual.trim(),
            vm_actual.trim(),
            "{path:?}: the two backends disagree with each other"
        );

        checked += 1;
    }
    assert!(
        checked >= 6,
        "expected at least 6 conformance fixtures, found {checked} in {dir:?}"
    );
}
