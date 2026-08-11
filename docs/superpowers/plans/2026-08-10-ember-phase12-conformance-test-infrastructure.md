# Phase 12 — Conformance & Test Infrastructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow the conformance/property-test foundation into the project's full spine: broader conformance coverage (including a real error-path parity check), a gc-stress CI pass, diagnostic/disassembly snapshots, a formatter-idempotence property test, real fuzz targets, and criterion benchmarks with allocation counting and a CI regression gate.

**Architecture:** Everything builds on the existing `crates/ember-cli/tests/conformance.rs` harness pattern (parse → resolve → infer → exhaustiveness-check → interpret-both-backends → compare) and the existing `insta`/`proptest` conventions already used in `ember-parser`/`ember-lexer`. No new workspace crates except two small ones (fuzz harnesses, which are cargo-fuzz's own non-member layout).

**Tech Stack:** `insta = "1"`, `proptest = "1"` (both already used elsewhere in the workspace at these versions), `criterion = "0.5"`, `cargo-fuzz` (nightly-only, not a workspace dependency).

**Pre-existing infrastructure this plan builds on (verified in the repo, not re-built):**
- `tests/conformance/*.em` + `.expected` (6 fixtures) and `crates/ember-cli/tests/conformance.rs` — runs both backends, asserts identical values, on success paths only.
- `crates/ember-lexer/tests/proptest_lexer.rs` — no-panic + span-tiling property tests. Unchanged.
- `crates/ember-parser/tests/proptest_roundtrip.rs` — round-trip property test. Unchanged.
- `crates/ember-parser/tests/snapshot_programs.rs` — **this already is the project's AST-snapshot coverage** (20 programs including recursion, mutual recursion, generics, shadowing, higher-order functions, loops, ADTs — snapshotted via `ember_ast::print_stmt`, not raw `Debug`). The checklist's "insta snapshots for AST" item is satisfied by this existing suite; this plan does not duplicate it.
- `gc-stress` / `gc-log` cargo features already propagate `ember-cli → ember-vm → ember-gc` with zero code changes needed to activate.
- `ember_diag::render::render(diag: &Diagnostic, path: &str, src: &str, use_color: bool) -> String` — the CLI's real diagnostic renderer, used for diagnostics snapshots.
- `ember_bytecode::disasm::disassemble_chunk(chunk: &Chunk, name: &str, interner: &Interner) -> String` — exists, unused by any test yet.

**A real architectural finding that scopes Task 2:** the tree-walker's `RuntimeError` carries a full `Span` (start+end) taken directly from the failing AST node, while the VM's `RuntimeError` carries only a single `line: u32` derived from the bytecode's line table (`chunk.line_at(ip)`) — its own doc comment calls this "a byte-offset stand-in, not a real 1-based source line." These render to visibly different diagnostics (different underline widths, different snippet framing) even for the identical logical error, by design of the current bytecode line-table architecture. So "identical error output" in this plan means **identical error message text**, not byte-identical rendered diagnostics — verified achievable only for error paths where both backends use the exact same format string. Grepping both backends' `RuntimeError::new`/`runtime_error(...)` call sites confirms exactly three such paths: `"division by zero"`, `format!("integer overflow: {a} {op_name} {b}")` (identical format string, same variable names, in both `ember-tree/src/interp.rs` and `ember-vm/src/vm.rs`), and `"stack overflow"`. Every other runtime error message embeds a backend-specific `Value`'s `{:?}` (the two backends have different `Value` enums with different `Debug` output), so those are excluded from this parity check — Task 2 covers the three matching paths only.

---

### Task 1: New conformance fixtures

**Files:**
- Create: `tests/conformance/strings.em`, `tests/conformance/strings.expected`
- Create: `tests/conformance/recursion.em`, `tests/conformance/recursion.expected`
- Create: `tests/conformance/mutual_recursion.em`, `tests/conformance/mutual_recursion.expected`
- Create: `tests/conformance/deep_recursion.em`, `tests/conformance/deep_recursion.expected`
- Create: `tests/conformance/generics.em`, `tests/conformance/generics.expected`
- Create: `tests/conformance/shadowing.em`, `tests/conformance/shadowing.expected`
- Create: `tests/conformance/higher_order.em`, `tests/conformance/higher_order.expected`
- Create: `tests/conformance/loops.em`, `tests/conformance/loops.expected`
- Modify: `crates/ember-cli/tests/conformance.rs:91-94` (the `checked >= 6` assertion)

This task has no "failing test first" step in the usual TDD sense — the existing harness test (`both_backends_produce_identical_output_matching_every_captured_fixture`) already iterates every `.em` file in the directory, so adding fixtures the test doesn't yet know about *is* the change, and the test fails naturally if a fixture's `.expected` is wrong.

- [ ] **Step 1: Write `strings.em` / `strings.expected`**

```
// tests/conformance/strings.em
let a = "hello";
let b = "world";
a + " " + b;
```

```
// tests/conformance/strings.expected
hello world
```

Run `cargo run -p ember-cli -- run tests/conformance/strings.em` first to confirm the actual output, then write `.expected` to match exactly (do not guess — string concatenation syntax must match SPEC.md; if `+` is not the concatenation operator, check `SPEC.md` for the correct one before writing the fixture).

- [ ] **Step 2: Write `recursion.em` / `recursion.expected`**

```
// tests/conformance/recursion.em
fn fact(n) {
    if n == 0 { 1 } else { n * fact(n - 1) }
}

fact(10);
```

```
// tests/conformance/recursion.expected
3628800
```

- [ ] **Step 3: Write `mutual_recursion.em` / `mutual_recursion.expected`**

```
// tests/conformance/mutual_recursion.em
fn is_even(n) {
    if n == 0 { true } else { is_odd(n - 1) }
}
fn is_odd(n) {
    if n == 0 { false } else { is_even(n - 1) }
}

is_even(20);
```

```
// tests/conformance/mutual_recursion.expected
true
```

- [ ] **Step 4: Write `deep_recursion.em` / `deep_recursion.expected`**

```
// tests/conformance/deep_recursion.em
fn sum_to(n) {
    if n == 0 { 0 } else { n + sum_to(n - 1) }
}

sum_to(50);
```

```
// tests/conformance/deep_recursion.expected
1275
```

The tree-walker enforces a deliberately low `MAX_CALL_DEPTH = 64` (`crates/ember-tree/src/interp.rs:67`, chosen to trip before the native Rust stack overflows) — so `sum_to(1000)` as shown above WILL fail on the tree-walker. Before finalizing, run this through both `cargo run -p ember-cli -- run tests/conformance/deep_recursion.em` and `cargo run -p ember-cli -- vm tests/conformance/deep_recursion.em`. Start at `n = 50` (known to work: `sum_to(50)` = 1275) rather than 1000, and adjust `.expected` to match. The point of this fixture is proving deep-but-legal recursion works identically, not finding the overflow boundary (that's covered separately by the `stack_overflow` fixture in Task 2). Record whatever depth actually works as the final fixture; do not leave a depth that fails either backend.

- [ ] **Step 5: Write `generics.em` / `generics.expected`**

```
// tests/conformance/generics.em
fn identity(x) { x }

let a = identity(5);
let b = identity("hi");
a;
```

Ember has no explicit generic-parameter annotation syntax — `fn identity(x) { x }` is itself the generic function (HM inference generalizes it); the snippet above is already correct as written. The point is one function instantiated at two different concrete types in the same program.

```
// tests/conformance/generics.expected
5
```

- [ ] **Step 6: Write `shadowing.em` / `shadowing.expected`**

```
// tests/conformance/shadowing.em
let x = 1;
let y = { let x = x + 1; let x = x * 10; x };
x + y;
```

```
// tests/conformance/shadowing.expected
21
```

- [ ] **Step 7: Write `higher_order.em` / `higher_order.expected`**

```
// tests/conformance/higher_order.em
fn apply_twice(f, x) { f(f(x)) }
fn add_one(n) { n + 1 }

apply_twice(add_one, 5);
```

```
// tests/conformance/higher_order.expected
7
```

- [ ] **Step 8: Write `loops.em` / `loops.expected`**

```
// tests/conformance/loops.em
let mut i = 0;
let mut total = 0;
while i < 5 {
    total = total + i;
    i = i + 1;
}

let mut count = 0;
loop {
    if count >= 3 { break; }
    count = count + 1;
}

total + count;
```

```
// tests/conformance/loops.expected
13
```

- [ ] **Step 9: Verify every fixture's `.expected` against BOTH backends manually**

For each of the 8 new `.em` files:
```bash
cargo run -p ember-cli -- run tests/conformance/<name>.em
cargo run -p ember-cli -- vm tests/conformance/<name>.em
```
Both must print the value in `.expected` (whitespace-trimmed). If any fixture doesn't parse/typecheck due to a wrong guess about ember syntax (e.g. generics syntax), fix the `.em` source based on the actual error message and `SPEC.md`, then re-verify — do not fix `.expected` to match an unintended program.

- [ ] **Step 10: Update the fixture-count assertion**

In `crates/ember-cli/tests/conformance.rs`, change:
```rust
    assert!(
        checked >= 6,
        "expected at least 6 conformance fixtures, found {checked} in {dir:?}"
    );
```
to:
```rust
    assert!(
        checked >= 14,
        "expected at least 14 conformance fixtures, found {checked} in {dir:?}"
    );
```

- [ ] **Step 11: Run the full conformance test**

Run: `cargo test -p ember-cli --test conformance`
Expected: PASS, all 14 fixtures checked.

- [ ] **Step 12: Commit**

```bash
git add tests/conformance/ crates/ember-cli/tests/conformance.rs
git commit -m "Add 8 conformance fixtures: strings, recursion, mutual/deep recursion, generics, shadowing, higher-order functions, loops"
```

---

### Task 2: Error-output parity harness

**Files:**
- Create: `tests/conformance_errors/division_by_zero.em`, `tests/conformance_errors/division_by_zero.expected`
- Create: `tests/conformance_errors/integer_overflow.em`, `tests/conformance_errors/integer_overflow.expected`
- Create: `tests/conformance_errors/stack_overflow.em`, `tests/conformance_errors/stack_overflow.expected`
- Modify: `crates/ember-cli/tests/conformance.rs` (new test function)

- [ ] **Step 1: Write the three error fixtures**

```
// tests/conformance_errors/division_by_zero.em
let a = 10;
let b = 0;
a / b;
```
```
// tests/conformance_errors/division_by_zero.expected
division by zero
```

```
// tests/conformance_errors/integer_overflow.em
let max = 9223372036854775807;
max + 1;
```
```
// tests/conformance_errors/integer_overflow.expected
integer overflow: 9223372036854775807 + 1
```
Run this through both backends manually first — if `Int` overflow detection uses a different message shape than assumed here (check `ember-tree/src/interp.rs:656`'s exact `format!` args: `"integer overflow: {a} {op_name} {b}"` — confirm what `op_name` renders as for `+`, e.g. `"+"` literally), fix `.expected` to match the ACTUAL rendered text from a real run, not a guess.

```
// tests/conformance_errors/stack_overflow.em
fn recurse(n) { n + recurse(n + 1) }
recurse(0);
```
```
// tests/conformance_errors/stack_overflow.expected
stack overflow
```

- [ ] **Step 2: Verify each fixture actually fails on both backends with the expected message**

```bash
cargo run -p ember-cli -- run tests/conformance_errors/division_by_zero.em
cargo run -p ember-cli -- vm tests/conformance_errors/division_by_zero.em
```
(repeat for the other two). Each should exit non-zero and print a diagnostic whose message line matches the `.expected` file. Adjust `.expected` to the real observed message text if it differs from the guess above.

- [ ] **Step 3: Add the error-parity test function to `crates/ember-cli/tests/conformance.rs`**

Add at the end of the file, after the existing test:
```rust
fn conformance_errors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance_errors")
}

#[test]
fn both_backends_fail_with_identical_error_messages() {
    let dir = conformance_errors_dir();
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

        let (_tree_result, tree_err) = ember_tree::interpret(&ast, &interner, &stmts);
        let tree_message = tree_err
            .unwrap_or_else(|| panic!("{path:?}: tree-walker did not fail, expected an error"))
            .to_diagnostic()
            .message;
        assert_eq!(
            tree_message.trim(),
            expected.trim(),
            "{path:?}: tree-walker error message mismatch"
        );

        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let mut vm = ember_vm::vm::Vm::new(proto);
        let vm_message = match vm.run() {
            Ok(v) => panic!(
                "{path:?}: VM did not fail, expected an error, got {}",
                ember_vm::value::display_value(&v)
            ),
            Err(e) => e.to_diagnostic(&interner).message,
        };
        assert_eq!(
            vm_message.trim(),
            expected.trim(),
            "{path:?}: VM error message mismatch"
        );
        assert_eq!(
            tree_message.trim(),
            vm_message.trim(),
            "{path:?}: the two backends' error messages disagree"
        );

        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected at least 3 conformance-error fixtures, found {checked} in {dir:?}"
    );
}
```

- [ ] **Step 4: Run the new test**

Run: `cargo test -p ember-cli --test conformance both_backends_fail_with_identical_error_messages`
Expected: PASS, `checked` reported as 3 (visible only on failure, but confirm no failure).

- [ ] **Step 5: Run the full conformance test file**

Run: `cargo test -p ember-cli --test conformance`
Expected: both test functions PASS.

- [ ] **Step 6: Commit**

```bash
git add tests/conformance_errors/ crates/ember-cli/tests/conformance.rs
git commit -m "Add error-output parity check: division by zero, integer overflow, stack overflow fail identically on both backends"
```

---

### Task 3: gc-stress CI job

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add a second job to the workflow**

Full new content of `.github/workflows/ci.yml`:
```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - name: fmt check
        run: cargo fmt --all -- --check
      - name: clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: test
        run: cargo test --workspace

  conformance-gc-stress:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: conformance suite under gc-stress
        run: cargo test -p ember-cli --features gc-stress --test conformance
```

- [ ] **Step 2: Verify locally (the closest available proxy for the CI job)**

Run: `cargo test -p ember-cli --features gc-stress --test conformance`
Expected: PASS. This forces a GC collection before every VM instruction across all 17 conformance fixtures (14 success-path + 3 error-path), the strongest available regression signal for the GC.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "Add CI job running the conformance suite under gc-stress"
```

---

### Task 4: Diagnostics snapshots

**Files:**
- Create: `tests/diagnostics/parse_error.em`
- Create: `tests/diagnostics/unresolved_name.em`
- Create: `tests/diagnostics/type_mismatch.em`
- Create: `tests/diagnostics/non_exhaustive_match.em`
- Create: `crates/ember-cli/tests/diagnostics.rs`
- Modify: `crates/ember-cli/Cargo.toml` (add `[dev-dependencies]` section)

- [ ] **Step 1: Add `insta` as a dev-dependency**

In `crates/ember-cli/Cargo.toml`, add after the `[features]` section:
```toml

[dev-dependencies]
insta = "1"
```

- [ ] **Step 2: Write the four diagnostic-triggering fixtures**

```
// tests/diagnostics/parse_error.em
let x = ;
```

```
// tests/diagnostics/unresolved_name.em
let x = totally_undefined_name;
```

```
// tests/diagnostics/type_mismatch.em
let x: Int = "not an int";
```
Check `SPEC.md` for the actual type-annotation syntax before finalizing — if `let x: Int = ...` isn't valid syntax, use whatever construct produces a genuine type-mismatch diagnostic (e.g. `1 + "s"`).

```
// tests/diagnostics/non_exhaustive_match.em
type Shape = Circle(Float) | Square(Float);
fn area(s) {
    match s {
        Circle(r) => 3.14 * r * r,
    }
}
```

- [ ] **Step 3: Write the failing test first**

`crates/ember-cli/tests/diagnostics.rs`:
```rust
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
        assert!(!diags.is_empty(), "{path:?}: expected at least one diagnostic, got none");

        let mut rendered = String::new();
        for d in &diags {
            rendered.push_str(&ember_diag::render::render(d, &name, &src, false));
            rendered.push('\n');
        }
        insta::assert_snapshot!(name, rendered);
    }
}
```

- [ ] **Step 4: Run it to generate initial snapshots**

Run: `cargo test -p ember-cli --test diagnostics`
Expected: FAIL first time (insta reports new/pending snapshots — this is insta's normal first-run behavior, not a bug).

- [ ] **Step 5: Review and accept the snapshots**

Run: `cargo insta review` (or `cargo insta accept` if the rendered diagnostics look correct on manual inspection first — read each `.snap.new` file and confirm it's a real, well-formed diagnostic before accepting, don't blindly accept).

- [ ] **Step 6: Run again to confirm green**

Run: `cargo test -p ember-cli --test diagnostics`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/tests/diagnostics.rs crates/ember-cli/tests/snapshots/ tests/diagnostics/
git commit -m "Add insta snapshot tests for rendered diagnostics"
```

---

### Task 5: Disassembly snapshots

**Files:**
- Create: `crates/ember-cli/tests/snapshots_disasm.rs`

- [ ] **Step 1: Write the test**

Reuses the existing `tests/conformance/*.em` corpus (14 fixtures from Task 1) — no new fixtures needed, disassembly snapshots ride on the same programs the conformance suite already validates for correctness.

```rust
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
    assert!(!entries.is_empty(), "expected conformance fixtures in {dir:?}");

    for path in entries {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));

        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
        assert!(parse_diags.is_empty(), "{path:?}: parse diags: {parse_diags:?}");
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
```

- [ ] **Step 2: Run to generate initial snapshots**

Run: `cargo test -p ember-cli --test snapshots_disasm`
Expected: FAIL first time (pending snapshots).

- [ ] **Step 3: Review and accept**

Run: `cargo insta review`. Read at least 2-3 of the generated `.snap.new` files to confirm the disassembly output looks like real, sane bytecode (recognizable opcodes, plausible constant pool entries) before accepting all.

- [ ] **Step 4: Run again to confirm green**

Run: `cargo test -p ember-cli --test snapshots_disasm`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-cli/tests/snapshots_disasm.rs crates/ember-cli/tests/snapshots/
git commit -m "Add insta snapshot tests for bytecode disassembly over the conformance corpus"
```

---

### Task 6: Formatter idempotence property test

**Files:**
- Create: `crates/ember-fmt/tests/proptest_idempotence.rs`
- Modify: `crates/ember-fmt/Cargo.toml` (add `proptest` dev-dependency)

- [ ] **Step 1: Add `proptest` as a dev-dependency**

`crates/ember-fmt/Cargo.toml` already has `[dev-dependencies]` with `ember-tree = { path = "../ember-tree" }` present (used by `format.rs`'s own semantics-preservation test). Add `proptest = "1"` as the one new line under that existing section:
```toml
[dev-dependencies]
ember-tree = { path = "../ember-tree" }
proptest = "1"
```

- [ ] **Step 2: Write the property test**

`crates/ember-fmt/tests/proptest_idempotence.rs`:
```rust
use ember_fmt::format;
use proptest::prelude::*;
use std::fs;
use std::path::PathBuf;

fn conformance_sources() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|p| fs::read_to_string(&p).unwrap())
        .collect()
}

/// Inserts `extra` blank lines / spaces at pseudo-random positions that are
/// always safe (only ever widens existing whitespace runs, or duplicates an
/// existing blank line, never touches non-whitespace bytes) so the result
/// is still valid ember source with the exact same token stream.
fn perturb_whitespace(src: &str, seed: u64) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let mut counter = seed;
    for line in src.lines() {
        out.push_str(line);
        out.push('\n');
        counter = counter.wrapping_mul(6364136223846793005).wrapping_add(1);
        if line.trim().is_empty() && (counter % 3 == 0) {
            out.push('\n');
        }
    }
    out
}

proptest! {
    #[test]
    fn formatting_is_idempotent_over_perturbed_conformance_corpus(
        idx in 0..conformance_sources().len(),
        seed in any::<u64>(),
    ) {
        let sources = conformance_sources();
        let perturbed = perturb_whitespace(&sources[idx], seed);
        let once = format(&perturbed);
        let twice = format(&once);
        prop_assert_eq!(once, twice);
    }
}
```

- [ ] **Step 3: Run it**

Run: `cargo test -p ember-fmt --test proptest_idempotence`
Expected: PASS. If it fails, the failure is a real formatter bug (a case where reformatting already-formatted output changes it again) — investigate `crates/ember-fmt/src/format.rs`'s blank-line logic rather than weakening the test.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-fmt/Cargo.toml crates/ember-fmt/tests/proptest_idempotence.rs
git commit -m "Add formatter idempotence property test over perturbed conformance corpus"
```

---

### Task 7: Fuzz targets

**Files:**
- Create: `crates/ember-lexer/fuzz/Cargo.toml`, `crates/ember-lexer/fuzz/fuzz_targets/lex.rs`
- Create: `crates/ember-parser/fuzz/Cargo.toml`, `crates/ember-parser/fuzz/fuzz_targets/parse.rs`
- Create: `crates/ember-types/fuzz/Cargo.toml`, `crates/ember-types/fuzz/fuzz_targets/infer.rs`
- Modify: `.github/workflows/ci.yml` (new nightly fuzz job)

- [ ] **Step 1: Install cargo-fuzz**

Run: `source "$HOME/.cargo/env" && cargo install cargo-fuzz`
Expected: installs successfully (needs a C/C++ compiler for libFuzzer, already present via Xcode CLT on this machine — confirm with `cargo fuzz --version` after install).

- [ ] **Step 2: Initialize the lexer fuzz target**

Run: `cd crates/ember-lexer && cargo fuzz init && cd ../..`
This scaffolds `crates/ember-lexer/fuzz/Cargo.toml` and `crates/ember-lexer/fuzz/fuzz_targets/fuzz_target_1.rs`. Rename the target file to `lex.rs` and update `fuzz/Cargo.toml`'s `[[bin]]` section's `name` and `path` accordingly (cargo-fuzz names the target after the file by default; check the generated `Cargo.toml` and fix the `name`/`path` pair to `lex`/`fuzz_targets/lex.rs` if `init` didn't already match).

Contents of `crates/ember-lexer/fuzz/fuzz_targets/lex.rs`:
```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = ember_lexer::lex(s);
    }
});
```

Ensure `crates/ember-lexer/fuzz/Cargo.toml` has `ember-lexer` as a path dependency (`{ path = ".." }`) — `cargo fuzz init` sets this up automatically from the crate it was run in; verify it's there.

- [ ] **Step 3: Build and smoke-test the lexer fuzz target**

Run: `cd crates/ember-lexer && cargo +nightly fuzz run lex -- -max_total_time=10 && cd ../..`
Expected: builds, runs for 10 seconds, no crashes reported (exits 0, prints a summary of executions).

- [ ] **Step 4: Initialize the parser fuzz target**

Run: `cd crates/ember-parser && cargo fuzz init && cd ../..`
Rename to `parse.rs`, contents:
```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = ember_parser::parse(s);
    }
});
```

- [ ] **Step 5: Build and smoke-test the parser fuzz target**

Run: `cd crates/ember-parser && cargo +nightly fuzz run parse -- -max_total_time=10 && cd ../..`
Expected: no crashes.

- [ ] **Step 6: Initialize the type-checker fuzz target**

Run: `cd crates/ember-types && cargo fuzz init && cd ../..`
This needs `ember-parser` and `ember-resolve` as additional path dependencies in `crates/ember-types/fuzz/Cargo.toml` (add both under `[dependencies]`, `{ path = "../../ember-parser" }` and `{ path = "../../ember-resolve" }`, alongside the auto-added `ember-types = { path = ".." }`).

Contents of `crates/ember-types/fuzz/fuzz_targets/infer.rs`:
```rust
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
```
Also add `ember-diag = { path = "../../ember-diag" }` to `crates/ember-types/fuzz/Cargo.toml`'s dependencies for the `Severity` import.

- [ ] **Step 7: Build and smoke-test the type-checker fuzz target**

Run: `cd crates/ember-types && cargo +nightly fuzz run infer -- -max_total_time=10 && cd ../..`
Expected: no crashes. If a real panic is found by any of the three targets during these smoke tests, that's a genuine bug — capture the crashing input cargo-fuzz saves under `fuzz/artifacts/`, minimize it (`cargo fuzz tmin`), and either fix the panic now (if small and clearly a robustness bug, e.g. an unwrap that should be a graceful diagnostic) or record it as a new tracked issue rather than silently deleting the crash artifact.

- [ ] **Step 8: Add the nightly fuzz CI job**

Append to `.github/workflows/ci.yml`:
```yaml

  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - name: install cargo-fuzz
        run: cargo install cargo-fuzz
      - name: fuzz lexer
        run: cd crates/ember-lexer && cargo fuzz run lex -- -max_total_time=30
      - name: fuzz parser
        run: cd crates/ember-parser && cargo fuzz run parse -- -max_total_time=30
      - name: fuzz type checker
        run: cd crates/ember-types && cargo fuzz run infer -- -max_total_time=30
```

- [ ] **Step 9: Ensure fuzz crates don't break the main workspace build**

cargo-fuzz projects are standalone (not workspace members) by convention specifically to keep nightly-only dependencies out of the stable build. Confirm this holds:
Run: `cargo build --workspace` (from repo root, stable toolchain)
Expected: succeeds, unaffected by the new `*/fuzz/` directories (cargo only considers them if explicitly listed in `[workspace] members`, which they are not).

- [ ] **Step 10: Add fuzz artifacts to `.gitignore`**

Check if `.gitignore` already has a `target/` or `**/target/` entry (fuzz builds also produce a `fuzz/target/` under each fuzz dir); if not covered, add `**/fuzz/target/` and `**/fuzz/corpus/` and `**/fuzz/artifacts/` to `.gitignore` (corpus/artifacts are runtime-generated, not source).

- [ ] **Step 11: Commit**

```bash
git add crates/ember-lexer/fuzz crates/ember-parser/fuzz crates/ember-types/fuzz .github/workflows/ci.yml .gitignore
git commit -m "Add cargo-fuzz targets for lexer, parser, and type checker with a nightly CI job"
```

---

### Task 8: Criterion benchmarks

**Files:**
- Create: `crates/ember-cli/benches/backends.rs`
- Modify: `crates/ember-cli/Cargo.toml` (add `criterion` dev-dependency, `[[bench]]` section)

- [ ] **Step 1: Add criterion**

In `crates/ember-cli/Cargo.toml`, update `[dev-dependencies]`:
```toml
[dev-dependencies]
insta = "1"
criterion = "0.5"
```
And add after `[features]`:
```toml

[[bench]]
name = "backends"
harness = false
```

- [ ] **Step 2: Write the benchmark file**

`crates/ember-cli/benches/backends.rs`:
```rust
use criterion::{criterion_group, criterion_main, Criterion};

const FIB: &str = "fn fib(n) { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } } fib(20);";
const LOOP: &str = "let mut i = 0; let mut total = 0; while i < 100000 { total = total + i; i = i + 1; } total;";
const CLOSURES: &str = "fn make_adder(n) { |x| x + n } let add5 = make_adder(5); let mut total = 0; let mut i = 0; while i < 10000 { total = add5(total); i = i + 1; } total;";
const LIST_OPS: &str = "let mut xs = []; let mut i = 0; while i < 5000 { xs = xs + [i]; i = i + 1; } xs;";
const STRING_OPS: &str = "let mut s = \"\"; let mut i = 0; while i < 2000 { s = s + \"x\"; i = i + 1; } s;";

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
```

Note: parsing/resolving/compiling happen inside the timed closure for the VM path but the tree-walker path also re-parses/re-resolves inside its timed closure — this is intentional symmetry (both measure "source to result", not "already-compiled bytecode to result"), since the whole point of this benchmark is comparing the two backends' end-to-end cost, matching how `ember run` actually invokes each.

- [ ] **Step 3: Run the benchmarks once to confirm they execute**

Run: `cargo bench -p ember-cli --bench backends -- --test`
(the `--test` flag makes criterion run each benchmark exactly once instead of its full statistical sampling, fast enough for verification)
Expected: all 10 benchmarks (5 groups × 2 backends) run without panicking, exit 0.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/benches/backends.rs
git commit -m "Add criterion benchmarks comparing tree-walker and VM backends"
```

---

### Task 9: Allocation counting

**Files:**
- Create: `crates/ember-vm/src/alloc_counter.rs`
- Modify: `crates/ember-vm/src/lib.rs` (register module + conditional `#[global_alloc]`)
- Modify: `crates/ember-vm/Cargo.toml` (new `count-allocs` feature)

- [ ] **Step 1: Write the failing test**

`crates/ember-vm/src/alloc_counter.rs`, test module first:
```rust
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct CountingAlloc {
    bytes: AtomicUsize,
    count: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocStats {
    pub bytes: usize,
    pub count: usize,
}

impl CountingAlloc {
    pub const fn new() -> Self {
        CountingAlloc {
            bytes: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
        }
    }

    pub fn reset(&self) {
        self.bytes.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> AllocStats {
        AllocStats {
            bytes: self.bytes.load(Ordering::Relaxed),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.bytes.fetch_add(layout.size(), Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_zeroes_both_counters() {
        let alloc = CountingAlloc::new();
        alloc.bytes.store(100, Ordering::Relaxed);
        alloc.count.store(5, Ordering::Relaxed);
        alloc.reset();
        assert_eq!(alloc.snapshot(), AllocStats { bytes: 0, count: 0 });
    }

    #[test]
    fn snapshot_reflects_recorded_activity() {
        let alloc = CountingAlloc::new();
        alloc.bytes.fetch_add(64, Ordering::Relaxed);
        alloc.count.fetch_add(1, Ordering::Relaxed);
        assert_eq!(alloc.snapshot(), AllocStats { bytes: 64, count: 1 });
    }
}
```

- [ ] **Step 2: Register the module and the conditional global allocator**

In `crates/ember-vm/src/lib.rs`, add:
```rust
pub mod alloc_counter;

#[cfg(feature = "count-allocs")]
#[global_allocator]
static GLOBAL_ALLOC_COUNTER: alloc_counter::CountingAlloc = alloc_counter::CountingAlloc::new();

#[cfg(feature = "count-allocs")]
pub fn alloc_stats() -> alloc_counter::AllocStats {
    GLOBAL_ALLOC_COUNTER.snapshot()
}

#[cfg(feature = "count-allocs")]
pub fn reset_alloc_stats() {
    GLOBAL_ALLOC_COUNTER.reset()
}
```
(Find the exact insertion point by reading the current top of `crates/ember-vm/src/lib.rs` first — insert alongside the existing `pub mod` declarations, don't disturb existing content.)

- [ ] **Step 3: Add the feature**

In `crates/ember-vm/Cargo.toml`'s `[features]` section:
```toml
[features]
gc-stress = ["ember-gc/gc-stress"]
gc-log = ["ember-gc/gc-log"]
count-allocs = []
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p ember-vm alloc_counter`
Expected: PASS, both tests green.

- [ ] **Step 5: Verify the feature builds and the global allocator installs correctly**

Run: `cargo build -p ember-vm --features count-allocs`
Expected: builds cleanly. Run: `cargo build -p ember-vm` (feature off)
Expected: also builds cleanly, confirming the `#[global_allocator]` attribute is truly conditional and doesn't affect default builds.

- [ ] **Step 6: Run clippy on the new module**

Run: `cargo clippy -p ember-vm --all-targets --features count-allocs -- -D warnings`
Expected: clean. Pay attention to any `missing_safety_doc` or similar lint on the `unsafe impl GlobalAlloc` — add a `// SAFETY:` comment above the impl if clippy flags it, explaining that `alloc`/`dealloc` delegate directly to `System`, the counting is side-effect-only bookkeeping with no aliasing/lifetime implications.

- [ ] **Step 7: Commit**

```bash
git add crates/ember-vm/src/alloc_counter.rs crates/ember-vm/src/lib.rs crates/ember-vm/Cargo.toml
git commit -m "Add allocation-counting GlobalAlloc wrapper behind a count-allocs feature"
```

---

### Task 10: CI benchmark regression gate

**Files:**
- Create: `scripts/check_bench_regression.py`
- Modify: `.github/workflows/ci.yml` (new `bench-regression` job)

- [ ] **Step 1: Write the comparison script**

`scripts/check_bench_regression.py`:
```python
#!/usr/bin/env python3
"""Compares two criterion baselines and fails if any benchmark's mean
estimate regressed by more than THRESHOLD (10%) from `base` to `pr`.
Reads criterion's own `estimates.json` per benchmark, under
target/criterion/<group>/<bench>/{base,pr}/estimates.json.
"""
import json
import sys
from pathlib import Path

THRESHOLD = 0.10
CRITERION_DIR = Path("target/criterion")


def mean_estimate(baseline_dir: Path) -> float | None:
    estimates_path = baseline_dir / "estimates.json"
    if not estimates_path.exists():
        return None
    with open(estimates_path) as f:
        data = json.load(f)
    return data["mean"]["point_estimate"]


def main() -> int:
    if not CRITERION_DIR.exists():
        print(f"no criterion output found at {CRITERION_DIR}, nothing to check")
        return 0

    regressions = []
    for bench_dir in sorted(CRITERION_DIR.glob("*/*/")):
        base_dir = bench_dir / "base"
        pr_dir = bench_dir / "pr"
        base_mean = mean_estimate(base_dir)
        pr_mean = mean_estimate(pr_dir)
        if base_mean is None or pr_mean is None:
            continue
        change = (pr_mean - base_mean) / base_mean
        label = f"{bench_dir.parent.name}/{bench_dir.name}"
        if change > THRESHOLD:
            regressions.append((label, change))
            print(f"REGRESSION  {label}: {change:+.1%} (base={base_mean:.0f}ns, pr={pr_mean:.0f}ns)")
        else:
            print(f"ok          {label}: {change:+.1%}")

    if regressions:
        print(f"\n{len(regressions)} benchmark(s) regressed by more than {THRESHOLD:.0%}")
        return 1
    print("\nno regressions over threshold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Test the script locally against synthetic data**

Create a throwaway fixture to confirm the script's logic before wiring it into CI:
```bash
mkdir -p /tmp/criterion_test/fib/tree/base /tmp/criterion_test/fib/tree/pr
echo '{"mean":{"point_estimate":1000.0}}' > /tmp/criterion_test/fib/tree/base/estimates.json
echo '{"mean":{"point_estimate":1200.0}}' > /tmp/criterion_test/fib/tree/pr/estimates.json
cd /tmp && python3 /Users/sanskar/dev/Research/Projects/Interpreter-Lang/scripts/check_bench_regression.py
```
(Temporarily edit `CRITERION_DIR` in the script or `cd` such that `target/criterion` resolves to `/tmp/criterion_test` for this one-off check — e.g. run from a directory where `target/criterion` symlinks there, or simplest: `cd /tmp && ln -s criterion_test target && python3 <script>`, then delete the symlink.)
Expected: script reports a `REGRESSION` for `fib/tree` (1000 → 1200 is +20%, over the 10% threshold), exits 1. Clean up `/tmp/criterion_test` and any symlink afterward.

- [ ] **Step 3: Make the script executable**

Run: `chmod +x scripts/check_bench_regression.py`

- [ ] **Step 4: Add the CI job**

Append to `.github/workflows/ci.yml`:
```yaml

  bench-regression:
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - name: benchmark base (merge-base with main)
        run: |
          git checkout $(git merge-base origin/main HEAD)
          cargo bench -p ember-cli --bench backends -- --save-baseline base
      - name: benchmark PR head
        run: |
          git checkout ${{ github.event.pull_request.head.sha }}
          cargo bench -p ember-cli --bench backends -- --save-baseline pr
      - name: check for regressions
        run: python3 scripts/check_bench_regression.py
```

Note: on the PR that introduces `benches/backends.rs` itself (Task 8), `git merge-base origin/main HEAD` lands on a commit predating the bench file's existence, so the "benchmark base" step will fail with "no such bench target" on that one PR — expected and fine, since there's no prior baseline to compare against yet. It will work correctly for every PR after Task 8 is merged to `main`.

- [ ] **Step 5: Verify the workflow YAML is well-formed**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"` (or any available YAML linter) to catch indentation errors before pushing — this job can't be fully exercised locally since it depends on `github.event.pull_request.head.sha`, so static validation is the best available local check.

- [ ] **Step 6: Commit**

```bash
git add scripts/check_bench_regression.py .github/workflows/ci.yml
git commit -m "Add CI benchmark regression gate: fails PRs with >10% slowdown vs main"
```

---

### Final Task: Full workspace verification and CHECKLIST.md reconciliation

**Files:**
- Modify: `CHECKLIST.md` (Phase 12 section)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all green, including all new tests from Tasks 1-9 (Task 10's CI job isn't locally runnable end-to-end, already covered by Task 10's own local script test).

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: both clean.

- [ ] **Step 3: Reconcile `CHECKLIST.md`'s Phase 12 section**

Check every item against what was actually built. Mark 🔴 items done. For 🟡 items, mark done and add an honest note for any that were scoped down during implementation (e.g. if the error-parity check ended up covering exactly 3 message-matching paths rather than a broader set, say so and explain why, matching the precedent set in Phase 11's reconciliation for the formatter's own honestly-documented gaps). Leave 🟢 coverage reporting unchecked with a note that it was deferred.

- [ ] **Step 4: Commit**

```bash
git add CHECKLIST.md
git commit -m "Reconcile CHECKLIST.md Phase 12: Conformance & Test Infrastructure"
```
