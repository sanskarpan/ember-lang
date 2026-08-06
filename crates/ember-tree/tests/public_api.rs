#[test]
fn interpret_and_display_value_are_reachable_from_the_crate_root() {
    let src = "let x = 1;\nlet y = 2;\nx + y;";
    let (ast, interner, stmts, diags) = ember_parser::parse(src);
    assert!(diags.is_empty());
    let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
    assert!(err.is_none());
    let value = result.expect("expected a final value");
    assert_eq!(ember_tree::display_value(&value, &interner), "3");
}
