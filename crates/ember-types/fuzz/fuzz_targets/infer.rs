#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let (ast, mut interner, stmts, diags) = ember_parser::parse(s);
    if !diags.is_empty() {
        return;
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return;
    }
    let _ = ember_types::infer(&ast, &mut interner, &stmts);
});
