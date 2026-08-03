use ember_parser::parse;

fn dump(src: &str) -> String {
    let (ast, interner, stmts, diags) = parse(src);
    let mut out = String::new();
    for s in &stmts {
        out.push_str(&ember_ast::print_stmt(&ast, &interner, *s));
        out.push('\n');
    }
    if !diags.is_empty() {
        out.push_str("-- diagnostics --\n");
        for d in &diags {
            out.push_str(&format!("{:?}: {}\n", d.severity, d.message));
        }
    }
    out
}

#[test]
fn snapshot_representative_programs() {
    let programs: &[(&str, &str)] = &[
        ("arithmetic", "let x = 1 + 2 * 3 - 4 / 2;"),
        ("string_literal", "let s = \"hello\\nworld\";"),
        ("list_literal", "let xs = [1, 2, 3];"),
        ("list_with_rest_pattern", "match xs { [head, ..tail] => head, [] => 0 }"),
        ("closure_capture", "fn make_counter() { let mut n = 0; || { n = n + 1; n } }"),
        ("higher_order_function", "fn apply(f, x) { f(x) }"),
        ("simple_recursion", "fn fact(n) { if n == 0 { 1 } else { n * fact(n - 1) } }"),
        (
            "mutual_recursion",
            "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }",
        ),
        (
            "adt_and_match",
            "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) { match s { Circle(r) => r, Rect(w, h) => w, Point => 0.0 } }",
        ),
        ("struct_decl_and_literal", "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };"),
        ("generic_identity", "fn identity(x) { x }"),
        ("while_loop", "while count < 10 { count = count + 1; }"),
        ("for_loop_range", "for i in 0..10 { print(i); }"),
        ("bare_loop_with_break", "loop { if done { break; } }"),
        ("shadowing", "let x = 1;\nlet x = x + 1;"),
        (
            "nested_if_else",
            "let sign = if x > 0 { \"pos\" } else if x < 0 { \"neg\" } else { \"zero\" };",
        ),
        ("or_pattern_and_guard", "match n { 1 | 2 => 0, n if n > 10 => 1, _ => 2 }"),
        ("function_types", "fn f(g: (Int) -> Int, x: Int) -> Int { g(x) }"),
        ("syntax_error_missing_semicolon", "let a = 1\nlet b = 2;"),
        ("nested_lambda", "let compose = |f, g| { |x| f(g(x)) };"),
    ];
    for (name, src) in programs {
        insta::assert_snapshot!(*name, dump(src));
    }
}
