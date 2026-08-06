#[test]
fn check_exhaustiveness_is_reachable_from_the_crate_root() {
    let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    assert!(infer_diags.is_empty());
    let diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    assert_eq!(diags.len(), 1);
}
