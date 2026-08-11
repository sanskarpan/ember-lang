# Phase 12 — Conformance & Test Infrastructure Design

## Goal

Grow the existing conformance/property-test foundation (6 `.em` fixtures on tree+VM, lexer span-tiling, parser round-trip) into the project's full spine: broader conformance coverage including error paths, diagnostic/AST/disassembly snapshots, a gc-stress CI pass, real fuzz targets, and criterion benchmarks with allocation counting and a CI regression gate.

## Scope

Full 🔴 + 🟡 checklist scope (coverage reporting, 🟢, deferred).

## 1. Conformance suite expansion

`tests/conformance/*.em` + `.expected` grows from 6 to ~14 fixtures. New fixtures, one topic each:

- `strings.em` — concatenation, interpolation if supported, length/indexing per SPEC.
- `recursion.em` — plain self-recursion (factorial or similar).
- `mutual_recursion.em` — two functions calling each other (e.g. is_even/is_odd).
- `deep_recursion.em` — a recursion depth deep enough to exercise stack handling on both backends without overflowing either (empirically determined during implementation, not hardcoded blind).
- `generics.em` — a generic function or ADT instantiated at ≥2 different types in one program.
- `shadowing.em` — same name rebound in nested scopes, each read observing the innermost binding.
- `higher_order.em` — functions passed as values and returned from functions (e.g. a `compose` or `map`-style helper written in ember itself, not a native).
- `loops.em` — `while` and `loop`+`break` (the existing `list_and_for.em` only covers `for`).

Each ships with a hand-computed `.expected` file, same convention as existing fixtures.

**Error-output parity.** New `tests/conformance_errors/*.em` + `.expected` (expected file holds the rendered diagnostic text) for programs that fail at runtime: division by zero, non-exhaustive match reaching an unhandled case, unbound variable. The harness in `crates/ember-cli/tests/conformance.rs` gets a second test function that runs each of these on both backends, asserts both fail, and asserts the rendered error text is identical between backends and matches the `.expected` file. This is distinct from the existing success-path test, which continues to assert `parse_diags`/`resolve_diags`/`infer_diags` are empty and both backends' *values* match.

**gc-stress.** No code changes — the `gc-stress` cargo feature already propagates `ember-cli → ember-vm → ember-gc` and forces a collection before every VM instruction. A new CI job runs `cargo test -p ember-cli --features gc-stress` so the full conformance suite (success + error paths) runs a third way, for free, on every push.

## 2. Diagnostics, AST, and disassembly snapshots

New dev-dependency: `insta = "1"` (matches the existing `proptest = "1"` unpinned-minor convention) in `ember-cli`.

- `tests/diagnostics/*.em` — one fixture per distinct diagnostic-producing path found in `ember-parser`/`ember-resolve`/`ember-types` (parse error, unresolved name, type mismatch, non-exhaustive match warning, etc.). New `crates/ember-cli/tests/diagnostics.rs` renders each through the CLI's actual diagnostic renderer (not a debug dump) and snapshots it with `insta::assert_snapshot!`.
- `crates/ember-cli/tests/snapshots_ast.rs` — snapshots a pretty-printed AST (`{:#?}` on the parsed `Ast`/statement list) for each `tests/conformance/*.em` fixture.
- `crates/ember-cli/tests/snapshots_disasm.rs` — snapshots `ember_bytecode::disasm::disassemble_chunk` output for the same fixtures, compiled via the existing `ember-compile` pipeline.

Snapshots live in `crates/ember-cli/tests/snapshots/` (insta's default), committed to git. `cargo insta review` is the update workflow; I will not hand-edit `.snap` files.

## 3. Property tests

Lexer span-tiling and parser round-trip already exist and are unchanged.

New: formatter idempotence, `crates/ember-fmt/tests/proptest_idempotence.rs`. Rather than building a full arbitrary-AST generator (large, separate undertaking), this generates random whitespace/blank-line/comment-placement perturbations of the existing conformance corpus source files and asserts `format(format(s)) == format(s)` for every perturbation — real coverage of the blank-line/comment-attachment logic (the trickiest part of the formatter) without a second formatter-testing subsystem.

## 4. Fuzz targets

`cargo-fuzz` installed (nightly + libFuzzer). Three fuzz crates, one target each:

- `crates/ember-lexer/fuzz/fuzz_targets/lex.rs` — raw arbitrary bytes (as `&str` via `Arbitrary`/lossy conversion) into `ember_lexer::lex`, asserting no panic.
- `crates/ember-parser/fuzz/fuzz_targets/parse.rs` — raw arbitrary bytes into `ember_parser::parse`, asserting no panic (parse errors are fine, panics are not).
- `crates/ember-types/fuzz/fuzz_targets/infer.rs` — arbitrary bytes piped through `parse` → `resolve` → `infer`, asserting no panic. Most random byte strings won't parse; this still exercises `infer`'s robustness on whatever *does* parse (which libFuzzer's coverage-guided search will bias toward parseable-ish inputs over time).

Each fuzz crate is its own `Cargo.toml` (cargo-fuzz's standard layout, not a workspace member — cargo-fuzz projects are intentionally excluded from the main workspace to avoid nightly bleeding into the stable build).

New CI job (`nightly` toolchain, only this job): builds each target and runs it for a fixed `-max_total_time=30` via `cargo fuzz run`, on every push. Bounded so CI stays fast; not a substitute for longer local fuzzing runs.

## 5. Benchmarks, allocation counting, regression gate

`crates/ember-cli/benches/backends.rs` using `criterion` (new dev-dependency, `criterion = "0.5"`), with `harness = false` wired in `Cargo.toml`. Benchmark groups: `fib`, `loop`, `closures`, `list_ops`, `string_ops` — each group has a `tree` and `vm` benchmark, sharing the same source snippet, run through each backend's own execution path.

Allocation counting: new module `crates/ember-vm/src/alloc_counter.rs` — a `GlobalAlloc` wrapper (`CountingAlloc`) tracking total bytes and allocation count via `AtomicUsize`, with a `reset()`/`snapshot() -> AllocStats { bytes, count }` API. Installed as the global allocator only behind a `count-allocs` cargo feature (so normal builds pay zero cost) — this phase only builds and unit-tests the wrapper; CLI/playground surfacing is Phase 13/16.

CI regression gate: new job that runs `cargo bench -p ember-cli -- --save-baseline pr` on the PR branch, and (via a separate checkout step) `--save-baseline main` on `main` at the PR's merge-base, then a small script comparing criterion's `estimates.json` per benchmark and failing if any PR benchmark's mean regresses >10% over the corresponding main baseline. No off-the-shelf action fits this repo's two-backend benchmark layout, so this is a small checked-in script (`scripts/check_bench_regression.py` or a shell script — decided during implementation based on what's cleanest) rather than a third-party action.

## Non-goals

- Coverage reporting (🟢, deferred).
- Phase 13's `bench` CLI command and Phase 16's allocation comparison panel (this phase only builds the allocator wrapper they'll consume).
- A general arbitrary-AST generator (the formatter idempotence test uses corpus perturbation instead, as justified above).
