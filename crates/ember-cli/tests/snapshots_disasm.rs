use std::fs;
use std::path::PathBuf;

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

#[test]
fn disassembly_matches_snapshots() {
    let dir = conformance_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "expected conformance fixtures in {dir:?}"
    );

    for path in entries {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));

        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
        assert!(
            parse_diags.is_empty(),
            "{path:?}: parse diags: {parse_diags:?}"
        );
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(
            !resolve_diags
                .iter()
                .any(|d| d.severity == ember_diag::Severity::Error),
            "{path:?}: resolve diags: {resolve_diags:?}"
        );

        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let disasm = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, &name, &interner);
        insta::assert_snapshot!(name, disasm);
    }
}
