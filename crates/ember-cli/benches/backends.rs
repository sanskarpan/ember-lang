use criterion::{criterion_group, criterion_main, Criterion};

const FIB: &str = "fn fib(n) { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } } fib(20);";
const LOOP: &str =
    "let mut i = 0; let mut total = 0; while i < 100000 { total = total + i; i = i + 1; } total;";
const CLOSURES: &str = "fn make_adder(n) { |x| x + n } let add5 = make_adder(5); let mut total = 0; let mut i = 0; while i < 10000 { total = add5(total); i = i + 1; } total;";
const LIST_OPS: &str =
    "let mut xs = []; let mut i = 0; while i < 5000 { xs = xs + [i]; i = i + 1; } xs;";
const STRING_OPS: &str =
    "let mut s = \"\"; let mut i = 0; while i < 2000 { s = s + \"x\"; i = i + 1; } s;";

fn run_tree(src: &str) {
    let (ast, mut interner, stmts, _) = ember_parser::parse(src);
    let (_bindings, _) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    let (_result, _err) = ember_tree::interpret(&ast, &interner, &stmts);
}

fn run_vm(src: &str) {
    let (ast, mut interner, stmts, _) = ember_parser::parse(src);
    let (bindings, _) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let mut vm = ember_vm::vm::Vm::new(proto);
    let _ = vm.run();
}

fn bench_group(c: &mut Criterion, name: &str, src: &str) {
    let mut group = c.benchmark_group(name);
    group.bench_function("tree", |b| b.iter(|| run_tree(src)));
    group.bench_function("vm", |b| b.iter(|| run_vm(src)));
    group.finish();
}

fn benches(c: &mut Criterion) {
    bench_group(c, "fib", FIB);
    bench_group(c, "loop", LOOP);
    bench_group(c, "closures", CLOSURES);
    bench_group(c, "list_ops", LIST_OPS);
    bench_group(c, "string_ops", STRING_OPS);
}

criterion_group!(backend_benches, benches);
criterion_main!(backend_benches);
