# Phase 13 — CLI & REPL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build out `ember-cli`'s full SPEC.md §16 command surface, a real REPL with incremental execution on both backends, and a `ratatui` debug TUI.

**Architecture:** See `docs/superpowers/specs/2026-08-12-ember-phase13-cli-repl-design.md` for full rationale. Tasks are ordered so foundational cross-crate additions (GcHeap runtime stress, `parse_into`, resolver REPL-global seeding, Vm incremental execution, Vm introspection) land before the CLI-side features that consume them.

**Tech Stack:** `rustyline` (REPL line editing), `ratatui` + `crossterm` (debug TUI), `clap_complete` (shell completions), `std::io::IsTerminal` (stable, no new dependency, non-TTY detection), `serde`/`serde_json` (AST `--json`).

**A note on precision:** several tasks touch delicate, previously-hand-verified slot/scope bookkeeping (the resolver's `FunctionCtx`, the compiler's `local_count`/`NATIVE_GLOBAL_COUNT`, the VM's stack/globals setup). This project has twice already shipped and then had to fix subtle bugs in exactly this kind of code (the Or-pattern shared-slot bug, the let-initializer nested-scope slot desync bug) — both times because a change didn't fully account for how resolver and compiler bookkeeping must stay in lockstep. Every task below that touches this territory is designed to avoid the compiler's existing slot-arithmetic entirely rather than extend it (see Task 12's design note) — if an implementer finds themselves needing to change `NATIVE_GLOBAL_COUNT`, `declare_named_local`, or any `local_count` arithmetic to make a task work, STOP and re-read the design doc's Section 4 rather than pushing through, since that's a sign the task is being implemented differently than designed.

---

### Task 1: `GcHeap` runtime stress flag

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Write the failing test**

Read the current `GcHeap` struct and `should_collect` method first (`crates/ember-gc/src/heap.rs`) to find the exact insertion point. `GcHeap::new()` is `pub fn new() -> Self { Self::default() }` — it delegates to a separate `impl Default for GcHeap`, so the new field's initializer goes in that `Default` impl's struct literal, not in `new()` itself. Add a field `stress: bool` to `GcHeap`, initialized in the `Default` impl to `cfg!(feature = "gc-stress")` (preserving current behavior exactly). Add:
```rust
pub fn set_stress(&mut self, on: bool) {
    self.stress = on;
}
```
Change `should_collect`'s existing `if cfg!(feature = "gc-stress")` check to `if self.stress`.

Add a test:
```rust
#[test]
fn set_stress_forces_collection_regardless_of_the_cargo_feature() {
    let mut heap = GcHeap::new();
    heap.set_stress(true);
    assert!(heap.should_collect());
}

#[test]
fn set_stress_false_restores_normal_threshold_based_collection() {
    let mut heap = GcHeap::new();
    heap.set_stress(false);
    assert!(!heap.should_collect(), "a fresh empty heap should not want to collect yet");
}
```
(Adjust `should_collect`'s exact signature/call convention by reading the real method first — it may take `&self` or need other context; match what's actually there.)

- [ ] **Step 2: Run the tests, confirm they pass**

Run: `cargo test -p ember-gc set_stress`

- [ ] **Step 3: Confirm the existing `gc-stress`-feature-gated tests still pass unchanged**

Run: `cargo test -p ember-gc --features gc-stress`
Run: `cargo test -p ember-gc` (feature off)
Expected: both green — `GcHeap::new()`'s `stress` field defaulting to `cfg!(feature = "gc-stress")` must keep both existing test suites passing exactly as before.

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy -p ember-gc --all-targets -- -D warnings`
Run: `cargo fmt -p ember-gc -- --check`

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Add a runtime GcHeap stress flag alongside the existing compile-time feature"
```

---

### Task 2: `run` command restructuring + exit code fix + TTY detection

**Files:**
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-vm/Cargo.toml` (only if `set_stress` needs a new public re-export path — check first)

- [ ] **Step 1: Read the current `main.rs` in full**

The current `Command` enum has separate `Run { file }` and `Vm { file }` variants, each with its own `run_run`/`run_vm` function — read both in full from the real `crates/ember-cli/src/main.rs` before editing (they are NOT reproduced in the design doc; the design doc's "pre-existing infrastructure" section is bullet points only). Confirmed by running both subcommands against `tests/conformance_errors/division_by_zero.em`: both currently exit `2` on a runtime error, exactly as this task assumes — the exit-code fix in Step 3 below is a real, verified bug fix, not a guess.

- [ ] **Step 2: Replace `Run`/`Vm` with one `Run` variant**

```rust
Run {
    file: String,
    #[arg(long, default_value = "tree")]
    backend: String,
    #[arg(long)]
    time: bool,
    #[arg(long)]
    gc_stress: bool,
},
```
(Using a plain `String` with manual validation below rather than a `clap::ValueEnum` two-variant enum is a legitimate choice either way — if using `ValueEnum`, define `enum Backend { Tree, Vm }` with `#[derive(clap::ValueEnum, Clone)]` instead, which gets free validation/error messages from clap. Prefer the `ValueEnum` approach — check clap's version already in use (`clap = { version = "4", features = ["derive"] }`) supports it, which it does.)

- [ ] **Step 3: Rewrite `run_run` to dispatch on backend, fix exit codes, add `--time`**

The current `run_run` (tree-walker) and `run_vm` (VM) bodies are almost identical up through the shared parse/resolve/infer/exhaustiveness pipeline — factor that shared pipeline into a small helper returning early with the right exit code on any diagnostic-stage failure, then branch only the final execute-and-print step by backend. Example shape (adapt exactly to the real current code, not this sketch):

```rust
fn run_run(path: &str, backend: Backend, time: bool, gc_stress: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&infer_diags, path, &src);
    }
    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    if exhaustive_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&exhaustive_diags, path, &src);
    }

    let start = std::time::Instant::now();
    let outcome = match backend {
        Backend::Tree => {
            if gc_stress {
                eprintln!("note: --gc-stress has no effect on the tree-walker backend (no GC)");
            }
            let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
            match err {
                Some(e) => Err(ember_diag::render::render(&e.to_diagnostic(), path, &src, use_color())),
                None => Ok(result.map(|v| ember_tree::display_value(&v, &interner)).unwrap_or_default()),
            }
        }
        Backend::Vm => {
            let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
            let mut vm = ember_vm::vm::Vm::new(proto);
            if gc_stress {
                vm.set_gc_stress(true); // exact accessor name TBD — Vm needs to forward to its GcHeap; check whether Vm already exposes its GcHeap or needs a new pass-through method
            }
            match vm.run() {
                Ok(v) => Ok(ember_vm::value::display_value(&v)),
                Err(e) => Err(ember_diag::render::render(&e.to_diagnostic(&interner), path, &src, use_color())),
            }
        }
    };
    if time {
        eprintln!("time: {:?}", start.elapsed());
    }
    match outcome {
        Ok(s) => {
            if !s.is_empty() {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Err(rendered) => {
            println!("{rendered}");
            ExitCode::from(1) // runtime error — was incorrectly 2 before this task
        }
    }
}
```

Note: `Vm` needs a way to reach its private `GcHeap` to call `set_stress` (Task 1). Add a small pass-through, e.g. `pub fn set_gc_stress(&mut self, on: bool) { self.gc.set_stress(on); }` on `Vm` in `crates/ember-vm/src/vm.rs` as part of this task (not Task 1, since Task 1 only touches `ember-gc`).

- [ ] **Step 4: Add a `use_color()` helper**

```rust
fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout())
}
```
Replace every existing `let use_color = std::env::var_os("NO_COLOR").is_none();` call site (there are several, in `print_diagnostics` and the old `run_run`/`run_vm`) with `use_color()`.

- [ ] **Step 5: Update `main()`'s dispatch**

```rust
Command::Run { file, backend, time, gc_stress } => run_run(&file, backend.into(), time, gc_stress),
```
(adjust based on whether `Backend` is the clap `ValueEnum` type directly or needs a `.into()`/match conversion — if using `#[derive(clap::ValueEnum)]` directly on an internal `Backend` enum, no conversion is needed, simplify accordingly).

- [ ] **Step 6: Manual verification**

```bash
cargo run -p ember-cli -- run tests/conformance/arithmetic.em
cargo run -p ember-cli -- run tests/conformance/arithmetic.em --backend vm
cargo run -p ember-cli -- run tests/conformance/arithmetic.em --backend vm --time
cargo run -p ember-cli -- run tests/conformance_errors/division_by_zero.em; echo "exit: $?"
```
Expected: last command exits `1` (not `2`) — confirm this explicitly, it's the exact bug this task fixes.

- [ ] **Step 7: Update `crates/ember-cli/tests/conformance.rs`**

This test file calls the old two-function pipeline directly (not through the CLI binary), so it's unaffected by the `Command` enum change — but re-run it to confirm: `cargo test -p ember-cli --test conformance`. If it fails, the failure means something about the shared library-level pipeline (not the CLI restructuring) broke — investigate rather than assume it's expected.

- [ ] **Step 8: Run full checks**

Run: `cargo test -p ember-cli`, `cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo clippy -p ember-vm --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

- [ ] **Step 9: Commit**

```bash
git add crates/ember-cli/src/main.rs crates/ember-vm/src/vm.rs
git commit -m "Merge run/vm into one run --backend command; fix runtime-error exit code to 1; add non-TTY color detection"
```

---

### Task 3: `check` command

**Files:**
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the `Check` variant and handler**

```rust
Check { file: String },
```
```rust
fn run_check(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&infer_diags, path, &src);
    }
    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    print_diagnostics(&exhaustive_diags, path, &src)
}
```
Note this prints warnings too (via `print_diagnostics`'s existing behavior of printing every diagnostic regardless of severity) but only treats error-severity ones as pipeline-stopping at each stage above — matches the existing convention used everywhere else in this file. Wire into `main()`'s match.

- [ ] **Step 2: Manual verification**

```bash
cargo run -p ember-cli -- check tests/conformance/arithmetic.em; echo "exit: $?"
cargo run -p ember-cli -- check tests/diagnostics/type_mismatch.em; echo "exit: $?"
```
Expected: `0` then `2`.

- [ ] **Step 3: Run checks and commit**

`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/src/main.rs
git commit -m "Add the check command: diagnostics only, no execution"
```

---

### Task 4: `ast --typed` and `--json`

**Files:**
- Modify: `crates/ember-ast/src/expr.rs`, `stmt.rs`, `pattern.rs`, `ty.rs`, `ast.rs` (serde derives)
- Modify: `crates/ember-ast/Cargo.toml` (add `serde`)
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-cli/Cargo.toml` (add `serde_json`)

- [ ] **Step 1: Add `serde` as a dependency**

`crates/ember-ast/Cargo.toml`:
```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
```
(check the current file first for what's already there — `ember-span`'s `Symbol`/`Span` types are likely dependencies too and may ALSO need `Serialize` derives if they don't already have them; check `crates/ember-span/src/lib.rs` and `crates/ember-ast/src/interner.rs`'s `Symbol` type before assuming this task is contained to just the 5 files listed above.)

- [ ] **Step 2: Add `#[derive(serde::Serialize)]` to `Expr`, `Stmt`, `Pattern`, `TypeExpr`, and `Ast`**

Read each type's current derive list first (`#[derive(Debug, Clone, PartialEq)]` per earlier investigation) and add `serde::Serialize` to each. `Ast`'s own fields are private (`exprs: Vec<Expr>`, etc.) — adding `#[derive(serde::Serialize)]` to it directly will serialize the raw arena layout, which is valid JSON but not especially readable (flat parallel arrays of indices). If any field type doesn't derive cleanly (e.g. `Idx<T>` — check `crates/ember-ast/src/idx.rs` or wherever it's defined for whether it already derives `Serialize`-compatible traits), add `Serialize` there too.

- [ ] **Step 3: Wire `--json` and `--typed` into `run_ast`**

Read the current `run_ast` function in full first. Replace the `--json` stub (`eprintln!("note: --json is not yet implemented...")`) with real serialization:
```rust
if json {
    let json = serde_json::to_string_pretty(&stmts.iter().map(|&s| /* project each stmt or the whole Ast */).collect::<Vec<_>>())
        .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
    println!("{json}");
    return print_diagnostics(&diags, path, &src);
}
```
(Decide during implementation whether to serialize the whole `Ast` struct once, or project just the requested `stmts` — prefer whichever produces cleaner, more useful JSON; verify by actually running it and reading the output, don't guess blind.)

For `--typed`, reuse `ember_types::infer` (same pattern as `run_typecheck`/the future `types` command in Task 5) and print each statement via `ember_ast::print_stmt` with type annotations appended — check whether `ember_ast::print_stmt` has any existing type-annotation-aware variant, or whether this needs a new small helper printing `<stmt> : <type>` per top-level expression statement.

- [ ] **Step 4: Add `serde_json` to `ember-cli`**

```toml
serde_json = "1"
```

- [ ] **Step 5: Manual verification**

```bash
cargo run -p ember-cli -- ast tests/conformance/arithmetic.em --json
cargo run -p ember-cli -- ast tests/conformance/arithmetic.em --typed
```
Confirm the JSON output is well-formed (pipe through `python3 -m json.tool` or similar to validate) and the typed output shows real inferred types, not placeholders.

- [ ] **Step 6: Run checks and commit**

`cargo test --workspace` (confirm the new `Serialize` derives don't break anything elsewhere), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
```bash
git add crates/ember-ast crates/ember-cli
git commit -m "Implement ast --json and --typed"
```

---

### Task 5: `types` command, retire `Typecheck`/`Resolve`

**Files:**
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add `Types` variant, remove `Resolve`/`Typecheck`**

Per the design doc, `types FILE` prints only the scheme-printing half of the current `run_typecheck` (the `schemes` loop), not per-expression types (moved to `ast --typed`) or exhaustiveness diagnostics (covered by `check`):
```rust
fn run_types(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (mut info, diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&diags, path, &src);
    }
    let mut schemes: Vec<_> = info.fn_schemes.iter().collect();
    schemes.sort_by_key(|(name, _)| interner.resolve(**name).to_string());
    for (name, scheme) in schemes {
        let scheme_str = ember_types::display_scheme(scheme, &mut info.subst, &info.adts, &interner);
        println!("{}: {}", interner.resolve(*name), scheme_str);
    }
    print_diagnostics(&diags, path, &src)
}
```
Remove `Command::Resolve`/`Command::Typecheck` and their `run_resolve`/`run_typecheck` functions entirely (they're superseded — `run_resolve`'s output isn't in SPEC.md's command list at all, and nothing else in the codebase depends on these as library functions, only as CLI subcommands; confirm with a workspace-wide grep for `run_resolve`/`run_typecheck` before deleting, to be sure).

- [ ] **Step 2: Manual verification and commit**

```bash
cargo run -p ember-cli -- types tests/conformance/higher_order.em
```
`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/src/main.rs
git commit -m "Add the types command; retire resolve/typecheck in favor of types, check, and ast --typed"
```

---

### Task 6: `disasm` command

**Files:**
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the `Disasm` variant and handler**

Reuse the exact recursive-disassembly pattern Phase 12's `crates/ember-cli/tests/snapshots_disasm.rs` and `crates/ember-compile/src/compiler.rs`'s own test helper (`disassemble_recursively`) already established — read both for the exact pattern before writing this (don't duplicate a subtly different version).
```rust
fn run_disasm(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    print!("{}", disassemble_recursively(&proto.chunk, "script", &interner));
    ExitCode::SUCCESS
}

fn disassemble_recursively(chunk: &ember_bytecode::chunk::Chunk, name: &str, interner: &ember_ast::Interner) -> String {
    let mut out = ember_bytecode::disasm::disassemble_chunk(chunk, name, interner);
    for (i, proto) in chunk.functions.iter().enumerate() {
        out.push_str(&disassemble_recursively(&proto.chunk, &format!("{name}::fn{i}"), interner));
    }
    out
}
```
(Verify `Chunk.functions` is a public field with this exact name/shape by reading `crates/ember-bytecode/src/chunk.rs` — the compiler test helper this is modeled on already relies on it, so it should be public, but confirm.)

- [ ] **Step 2: Manual verification and commit**

```bash
cargo run -p ember-cli -- disasm tests/conformance/closures.em
```
`cargo clippy`/`cargo fmt` checks.
```bash
git add crates/ember-cli/src/main.rs
git commit -m "Add the disasm command"
```

---

### Task 7: Shell completions

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`, `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add `clap_complete`**

```toml
clap_complete = "4"
```

- [ ] **Step 2: Add a hidden `Completions` subcommand**

```rust
/// Generate a shell completion script.
#[command(hide = true)]
Completions {
    #[arg(value_enum)]
    shell: clap_complete::Shell,
},
```
```rust
Command::Completions { shell } => {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    ExitCode::SUCCESS
}
```
(`Cli::command()` requires `clap::CommandFactory` in scope — add `use clap::CommandFactory;`. Confirm the derive macro already provides this via `#[derive(ClapParser)]`.)

- [ ] **Step 3: Manual verification**

```bash
cargo run -p ember-cli -- completions bash | head -5
cargo run -p ember-cli -- completions zsh | head -5
```
Expected: real shell-completion script output, not an error.

- [ ] **Step 4: Run checks and commit**

`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/src/main.rs
git commit -m "Add shell completions via a hidden completions subcommand"
```

---

### Task 8: Error-code registry — assign codes to every diagnostic site

**Files:**
- Modify: `crates/ember-lexer/src/lex.rs`, `crates/ember-parser/src/parser.rs`, `crates/ember-resolve/src/resolver.rs`, `crates/ember-types/src/infer.rs`, `crates/ember-types/src/exhaustive.rs`, `crates/ember-types/src/unify.rs`

- [ ] **Step 1: Survey every diagnostic construction site**

Run: `grep -rn "Diagnostic::error(\|Diagnostic::warning(" crates/ember-lexer/src crates/ember-parser/src crates/ember-resolve/src crates/ember-types/src` and read each one in context. Group them by logical error *kind* (e.g. every "expected X, found Y" parse error is one code; every "undeclared name" is one code; every "type mismatch" from unification is one code, etc.) — expect roughly 20-30 distinct groups from the 51 call sites. Write this grouping down (as a comment block or scratch note) before touching any code, so the registry (Task 9) and the codes assigned here stay consistent.

- [ ] **Step 2: Assign codes, lowest-numbered first by pipeline stage**

Convention: `E01xx` lexer, `E02xx` parser, `E03xx` resolver, `E04xx` type inference, `E05xx` exhaustiveness (adjust ranges if the real distribution doesn't fit evenly — the point is stage-grouped numbering, not exact ranges). For each call site in a group, append `.with_code("E0NNN")` to the existing `Diagnostic::error(...)`/`::warning(...)` builder chain.

- [ ] **Step 3: Update any test that asserts on `.code` being `None`, if any exist**

Run: `grep -rn "\.code, None\|code: None" crates/ember-lexer/src crates/ember-parser/src crates/ember-resolve/src crates/ember-types/src` — if any test explicitly asserts a diagnostic's code is absent, update it now that codes are assigned (this would be a real, intentional behavior change, not a bug — update with a comment, don't just delete the assertion).

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --workspace`. Diagnostic codes are additive metadata — no test should fail from adding them unless it specifically asserted `code.is_none()` (handled in Step 3). If anything else fails, investigate before proceeding.

- [ ] **Step 5: Run checks and commit**

`cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
```bash
git add crates/ember-lexer/src crates/ember-parser/src crates/ember-resolve/src crates/ember-types/src
git commit -m "Assign error codes to every diagnostic-producing call site"
```

---

### Task 9: `explain` registry and command

**Files:**
- Create: `crates/ember-diag/src/explain.rs`
- Modify: `crates/ember-diag/src/lib.rs`
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Write the registry**

```rust
// crates/ember-diag/src/explain.rs

pub struct ExplainEntry {
    pub code: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

pub static REGISTRY: &[ExplainEntry] = &[
    ExplainEntry {
        code: "E0301",
        title: "undeclared name",
        body: "A name was referenced that isn't declared anywhere visible from this point in the program.\n\nExample:\n\n    print(totally_undefined_name);\n\nCheck for typos, or declare the name with `let` before using it.",
    },
    // ... one entry per code from Task 8, matching the exact code strings assigned there
];

pub fn lookup(code: &str) -> Option<&'static ExplainEntry> {
    REGISTRY.iter().find(|e| e.code == code)
}
```
Write one real entry per code assigned in Task 8 — read each diagnostic's actual message text and the code producing it to write an accurate title/body, not a placeholder. This is the single largest step in this task; budget real time for it rather than rushing generic-sounding bodies.

- [ ] **Step 2: Export from `ember-diag`**

```rust
pub mod explain;
```

- [ ] **Step 3: Add the `Explain` CLI command**

```rust
Explain { code: String },
```
```rust
fn run_explain(code: &str) -> ExitCode {
    match ember_diag::explain::lookup(code) {
        Some(entry) => {
            println!("{} — {}\n\n{}", entry.code, entry.title, entry.body);
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("no explanation available for {code}");
            ExitCode::from(3)
        }
    }
}
```

- [ ] **Step 4: Write a test confirming every code from Task 8 has a registry entry**

In `crates/ember-diag/src/explain.rs`'s own test module, or a new integration test — this is the important correctness check for this task (not just "does explain print something," but "does every code that can actually appear in a real diagnostic have a registry entry"). One reasonable approach: a test that greps the workspace for every `.with_code("E0NNN")` literal (via `include_str!` over the relevant source files, or simply a hardcoded list mirrored from Task 8's own grouping) and asserts `explain::lookup` finds each one. Design this test to actually catch a missing entry, not just exercise the happy path.

- [ ] **Step 5: Manual verification and commit**

```bash
cargo run -p ember-cli -- explain E0301
cargo run -p ember-cli -- explain E9999; echo "exit: $?"
```
`cargo test -p ember-diag`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
```bash
git add crates/ember-diag crates/ember-cli/src/main.rs
git commit -m "Add the explain command and its error-code registry"
```

---

### Task 10: `trace` command

**Files:**
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the `Trace` variant and handler**

```rust
fn run_trace(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (_bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (mut info, diags) = ember_types::infer(&ast, &mut interner, &stmts);
    for (i, step) in info.trace.steps.iter().enumerate() {
        let lhs = ember_types::display_ty(&step.lhs, &mut info.subst, &info.adts, &interner);
        let rhs = ember_types::display_ty(&step.rhs, &mut info.subst, &info.adts, &interner);
        let verdict = if step.succeeded { "ok" } else { "FAILED" };
        println!("{i:>4}  {lhs} ~ {rhs}   [{:?}]   {verdict}", step.origin);
    }
    print_diagnostics(&diags, path, &src)
}
```
(Verify `TypeInfo.trace`/`UnifyStep`'s exact field names by reading `crates/ember-types/src/trace.rs` and `infer.rs` fresh — confirmed present as of Phase 8 per the design doc, but confirm the exact borrow-checker shape works: `display_ty` takes `&mut info.subst`, called in a loop borrowing `info.trace.steps` immutably at the same time — this may need `info.trace.steps.clone()` or restructuring to avoid a double-borrow; resolve whatever the compiler actually complains about, don't guess blind.)

- [ ] **Step 2: Manual verification and commit**

```bash
cargo run -p ember-cli -- trace tests/conformance/generics.em
```
`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/src/main.rs
git commit -m "Add the trace command: print the full inference derivation"
```

---

### Task 11: `bench` command + allocation counting wired into the CLI

**Files:**
- Modify: `crates/ember-cli/Cargo.toml` (enable `ember-vm/count-allocs` unconditionally)
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Enable `count-allocs` unconditionally for the CLI binary**

In `crates/ember-cli/Cargo.toml`, change the `ember-vm` dependency line to:
```toml
ember-vm = { path = "../ember-vm", features = ["count-allocs"] }
```
Verified safe: `ember-vm` is the only crate in the workspace with a `#[global_allocator]` (behind this feature), and today only `ember-cli` depends on `ember-vm` at all — `ember-lsp` and `ember-wasm` currently have no dependency on it. Forward-looking note for whoever touches this later: with `resolver = "2"` (the workspace's actual resolver, confirmed in the root `Cargo.toml`), feature unification still applies across workspace members built in the same `cargo build/test --workspace` invocation (v2 only isolates dev-deps/build-deps and cross-target-triple cases) — if a future phase gives `ember-lsp` or `ember-wasm` a normal dependency on `ember-vm`, this unconditional feature would silently install the counting global allocator for them too whenever built workspace-wide. Not a problem today; worth remembering if that changes.

- [ ] **Step 2: Add the `Bench` variant and handler**

```rust
fn run_bench(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&resolve_diags, path, &src);
    }
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&infer_diags, path, &src);
    }

    let tree_start = std::time::Instant::now();
    let (_result, tree_err) = ember_tree::interpret(&ast, &interner, &stmts);
    let tree_elapsed = tree_start.elapsed();
    if let Some(e) = tree_err {
        println!("{}", ember_diag::render::render(&e.to_diagnostic(), path, &src, use_color()));
        return ExitCode::from(1);
    }

    ember_vm::reset_alloc_stats();
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let vm_start = std::time::Instant::now();
    let mut vm = ember_vm::vm::Vm::new(proto);
    let vm_result = vm.run();
    let vm_elapsed = vm_start.elapsed();
    let alloc_stats = ember_vm::alloc_stats();
    if let Err(e) = vm_result {
        println!("{}", ember_diag::render::render(&e.to_diagnostic(&interner), path, &src, use_color()));
        return ExitCode::from(1);
    }

    println!("tree-walker: {tree_elapsed:?}");
    println!("vm:          {vm_elapsed:?}  ({} allocations, {} bytes)", alloc_stats.count, alloc_stats.bytes);
    let ratio = tree_elapsed.as_secs_f64() / vm_elapsed.as_secs_f64().max(f64::EPSILON);
    println!("speedup:     {ratio:.2}x");
    ExitCode::SUCCESS
}
```
Note `ember_vm::reset_alloc_stats`/`alloc_stats` are `#[cfg(feature = "count-allocs")]`-gated per Phase 12's implementation — since Step 1 makes `ember-cli` always build with that feature on, these calls always compile and work for this crate; no `#[cfg]` needed on the CLI side.

- [ ] **Step 3: Manual verification and commit**

```bash
cargo run -p ember-cli -- bench tests/conformance/recursion.em
```
Confirm real, non-zero, plausible timing/allocation numbers print (not zeros from a miswired feature).
`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`, `cargo test --workspace` (confirm nothing else regresses from the `count-allocs` feature now always being on for `ember-cli`'s own build).
```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/src/main.rs
git commit -m "Add the bench command: both backends, timing, allocations, speedup ratio"
```

---

### Task 12: `parse_into` — parser entry point taking an external Interner

**Files:**
- Modify: `crates/ember-parser/src/parser.rs`

**Design note:** this is purely additive — `parse()`'s existing public signature and behavior are completely unchanged; every one of its ~100 existing callers across the workspace needs zero changes.

- [ ] **Step 1: Write the failing test**

Read `pub fn parse(src: &str) -> (Ast, Interner, Vec<Idx<Stmt>>, Vec<Diagnostic>)`'s current body in full first. Add:
```rust
#[test]
fn parse_into_reuses_the_given_interner_across_two_calls() {
    let mut interner = Interner::new();
    let (_ast1, _stmts1, diags1) = parse_into("let x = 1;", &mut interner);
    assert!(diags1.is_empty());
    let x_symbol_after_first = interner.intern("x");

    let (_ast2, _stmts2, diags2) = parse_into("x + 1;", &mut interner);
    assert!(diags2.is_empty());
    let x_symbol_after_second = interner.intern("x");

    assert_eq!(
        x_symbol_after_first, x_symbol_after_second,
        "the same source identifier interned across two parse_into calls on the same Interner must produce the same Symbol"
    );
}
```

- [ ] **Step 2: Extract `parse_into` — this is a constructor/signature change, not a one-line body swap**

There is no standalone `let mut interner = Interner::new();` line inside `parse()`'s body to simply replace — the `Interner` is constructed inside `Parser::new`'s own struct-literal construction. The real shape of this change: give `Parser` a second lifetime parameter and an `interner: &mut Interner` field (was previously `Parser<'src>` owning its own `Interner`, becomes `Parser<'src, 'i>` borrowing one), update `Parser::new`'s signature to take `&'i mut Interner`, and rewire `parse`/`parse_into` plus this file's own two test helpers (`parse_expr_from_str`, `parse_stmt_from_str` — check for their exact current names) accordingly:
```rust
pub fn parse_into(src: &str, interner: &mut Interner) -> (Ast, Vec<Idx<Stmt>>, Vec<Diagnostic>) {
    let (tokens, _trivia, lex_diags) = ember_lexer::lex(src);
    let mut parser = Parser::new(&tokens, src, interner);
    // ... the rest of parse()'s existing body, adjusted for Parser now
    // borrowing interner instead of owning one — read the real current
    // body to get this exactly right, the lexing/token-feeding shape
    // above is illustrative, not necessarily exact.
}

pub fn parse(src: &str) -> (Ast, Interner, Vec<Idx<Stmt>>, Vec<Diagnostic>) {
    let mut interner = Interner::new();
    let (ast, stmts, diags) = parse_into(src, &mut interner);
    (ast, interner, stmts, diags)
}
```
`Parser::new`'s 3 call sites are all internal to `parser.rs` (confirmed by a workspace-wide grep before writing this plan), so this is contained to one file.

- [ ] **Step 3: Export `parse_into` from the crate root**

`crates/ember-parser/src/lib.rs` needs `pub use parser::parse_into;` alongside whatever already re-exports `parse` — without this, `ember_parser::parse_into` (the path every later task in this plan calls it by) doesn't resolve; only the fully-qualified `ember_parser::parser::parse_into` would. Easy to miss since `parse_into` compiles fine as a function without it — the failure only shows up as a confusing "unresolved import" in a LATER task (14+) that tries to call `ember_parser::parse_into`, so add this export now rather than debugging it later.

- [ ] **Step 4: Run the new test and the full parser test suite**

Run: `cargo test -p ember-parser parse_into`
Run: `cargo test -p ember-parser` (confirm the restructuring didn't change `parse`'s own observable behavior — every existing test should still pass unchanged; expect 54 tests total — 53 pre-existing plus this task's new one)

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace` — `parse`'s signature is unchanged so this should be a no-op check, but confirm nothing anywhere else broke.

- [ ] **Step 6: Run checks and commit**

`cargo clippy -p ember-parser --all-targets -- -D warnings`, `cargo fmt -p ember-parser -- --check`.
```bash
git add crates/ember-parser/src/parser.rs crates/ember-parser/src/lib.rs
git commit -m "Add parse_into: parse against an existing Interner, for the REPL's incremental parsing"
```

---

### Task 13: Resolver REPL-global seeding

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

**Design note — read this before writing any code:** do NOT implement this by mimicking `seed_native_globals`'s `declare()` call. `declare()` allocates a *local* resolver slot, which only makes sense for names physically present in the *same* compiled chunk. REPL entries are compiled as **separate, independent `compile()` calls** (Task 15) — a name from an earlier entry has no local slot in a *later* entry's freshly-compiled chunk at all. The correct design (from the spec doc, Section 4) is a **separate, unconditional-Global lookup table**, checked in `resolve_name` as a distinct branch — never producing `Resolution::Local`, regardless of nesting depth. This deliberately avoids touching `NATIVE_GLOBAL_COUNT` or any compiler-side slot-count baseline.

- [ ] **Step 1: Write the failing test**

Read `Resolver`'s current struct fields and `resolve_name`'s current full body first (both shown in the design investigation, but re-read the real file — line numbers may have shifted since).
```rust
#[test]
fn a_seeded_repl_global_resolves_from_top_level_code_in_a_separate_resolve_call() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("x + 1;");
    assert!(parse_diags.is_empty());
    let x_symbol = interner.intern("x");

    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.seed_repl_globals(&[x_symbol]);
    resolver.resolve_program(&stmts);
    assert!(
        resolver.diagnostics().is_empty(),
        "diags: {:?}", resolver.diagnostics()
    );
    let (bindings, _) = resolver.into_bindings();
    let var_expr = /* find the Idx<Expr> for the `x` reference in `ast` — walk `stmts`/`ast.stmt(...)` to locate it, matching how other tests in this file already do this */;
    assert!(matches!(
        bindings.resolutions.get(&var_expr),
        Some(crate::binding::Resolution::Global { .. })
    ), "a seeded REPL global must resolve as Global, never Local, even when referenced from this resolve call's own top level");
}

#[test]
fn a_new_entrys_own_declaration_shadows_a_seeded_repl_global_of_the_same_name() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 2; x;");
    assert!(parse_diags.is_empty());
    let x_symbol = interner.intern("x");

    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.seed_repl_globals(&[x_symbol]);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings, _) = resolver.into_bindings();
    let var_expr = /* the `x` reference in the second statement */;
    assert!(matches!(
        bindings.resolutions.get(&var_expr),
        Some(crate::binding::Resolution::Local { .. })
    ), "this entry's own `let x` must shadow the seeded REPL global, resolving as Local like any ordinary top-level let");
}
```
(Fill in the `/* ... */` placeholders with a small local helper, since no existing test in this file already does this — verified no such pattern exists in `resolver.rs`'s test module. Write a `find_var_expr(ast: &Ast, bindings: &Bindings, name: Symbol) -> Idx<Expr>` that filters `bindings.resolutions.keys()` for the entry whose `ast.expr(*idx)` matches `Expr::Var(sym) if *sym == name`, e.g.:
```rust
fn find_var_expr(ast: &Ast, bindings: &crate::binding::Bindings, name: ember_ast::Symbol) -> Idx<Expr> {
    *bindings
        .resolutions
        .keys()
        .find(|idx| matches!(ast.expr(**idx), Expr::Var(s) if *s == name))
        .expect("no Var reference to this name found")
}
```
)

- [ ] **Step 2: Add the `repl_globals` field and `seed_repl_globals` method**

```rust
pub struct Resolver<'a> {
    ast: &'a Ast,
    interner: &'a mut Interner,
    functions: Vec<FunctionCtx>,
    diagnostics: Vec<Diagnostic>,
    bindings: Bindings,
    repl_globals: FxHashSet<ember_ast::Symbol>, // new field
}
```
(Update `Resolver::new` to initialize it empty; `FxHashSet` is already imported in this file per the existing `use rustc_hash::FxHashSet;` at the top — confirm.)
```rust
pub fn seed_repl_globals(&mut self, names: &[ember_ast::Symbol]) {
    self.repl_globals.extend(names.iter().copied());
}
```

- [ ] **Step 3: Add the new branch in `resolve_name`**

Insert a new check right before the final "undeclared name" error path (after the existing local/upvalue/top-level-outermost-scope checks all fail):
```rust
if self.repl_globals.contains(&name_sym) {
    return Some(crate::binding::Resolution::Global { symbol: name_sym });
}
```
(No `used`-marking needed here the way the existing Global-fallback branch does it — REPL globals aren't subject to the unused-variable warning check the way a same-entry local declaration is, since they were already used-or-not in whatever prior entry declared them. Confirm this assumption doesn't break anything by running the full test suite in Step 4.)

- [ ] **Step 4: Run the new tests and the full resolver test suite**

Run: `cargo test -p ember-resolve seed_repl_globals`
Run: `cargo test -p ember-resolve`

- [ ] **Step 5: Run checks and commit**

`cargo clippy -p ember-resolve --all-targets -- -D warnings`, `cargo fmt -p ember-resolve -- --check`.
```bash
git add crates/ember-resolve/src/resolver.rs
git commit -m "Add resolver support for seeding known-global names from prior REPL entries"
```

---

### Task 14: Vm incremental execution

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

**Design note — the real `Vm::new` push order, verified byte-for-byte (`crates/ember-vm/src/vm.rs:60-95`), corrects an earlier guess:** `Vm::new` allocates the script's closure via `gc.allocate(...)` and builds its `CallFrame`, but **the closure itself is never pushed onto the physical `stack` Vec at all** — it lives only in `frame.closure`. The loop over `NATIVES` is what pushes values onto `stack`, filling physical slots 0-7 with the 8 native values, and those are the *only* things on the stack when execution begins (`frame.slot_base = 0`). There is no "closure vs. natives push order" question — the closure is simply never a stack entry. `run_incremental` must mirror this exactly: do NOT push the new closure onto `self.stack`. Reuse the **existing** `globals` map and `gc` heap (so cross-entry name lookups and any GC-allocated values already referenced by persisted globals stay alive and consistent) — only `stack`/`open_upvalues`/`frames` get reset fresh per entry.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn run_incremental_persists_globals_across_two_calls() {
    // Entry 1: `let x = 5;` compiled and run alone.
    let src1 = "let x = 5;";
    let (ast1, mut interner, stmts1, parse_diags1) = ember_parser::parse(src1);
    assert!(parse_diags1.is_empty());
    let (bindings1, resolve_diags1) = ember_resolve::resolve(&ast1, &mut interner, &stmts1);
    // NOT `.is_empty()` — a bare `let x = 5;` resolved on its own always
    // produces an "unused variable `x`" WARNING (nothing in this lone
    // resolve pass knows a later entry will read it). Check only
    // error-severity diagnostics, matching the convention used everywhere
    // else in this codebase.
    assert!(!resolve_diags1.iter().any(|d| d.severity == ember_diag::Severity::Error));
    let proto1 = ember_compile::compile(&ast1, &mut interner, &bindings1, &stmts1);
    let mut vm = Vm::new(proto1);
    // Vm::new already runs the "first entry" as its own initial script —
    // for a true multi-entry test, either call vm.run() once here to
    // execute entry 1's own let, OR (if run_incremental is designed to
    // handle the very first entry too, not just subsequent ones) skip
    // straight to using run_incremental for both — resolve this design
    // question during implementation by checking which shape makes the
    // REPL's own calling code (Task 16) simplest, then keep this test
    // consistent with that decision.
    vm.run().unwrap();

    // Entry 2: `x + 1;`, resolved with `x` seeded as a known REPL global,
    // compiled alone, run via run_incremental against the SAME vm.
    let (ast2, stmts2, parse_diags2) = ember_parser::parse_into("x + 1;", &mut interner);
    assert!(parse_diags2.is_empty());
    let x_symbol = interner.intern("x");
    let mut resolver = ember_resolve::Resolver::new(&ast2, &mut interner);
    resolver.seed_repl_globals(&[x_symbol]);
    resolver.resolve_program(&stmts2);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings2, _) = resolver.into_bindings();
    let proto2 = ember_compile::compile(&ast2, &mut interner, &bindings2, &stmts2);

    let result = vm.run_incremental(proto2).unwrap();
    // `Value` derives only `Debug, Clone`, not `PartialEq` — every existing
    // VM test uses `matches!` for this reason, not `assert_eq!`.
    assert!(matches!(result, Value::Int(6)));
}
```
(This test needs `ember-resolve`'s `Resolver`/`seed_repl_globals` from Task 13, and `ember-parser`'s `parse_into` from Task 12 — both already merged by the time this task runs, per the plan's task ordering. Check `ember-vm`'s existing `[dev-dependencies]` already includes `ember-parser`/`ember-resolve`/`ember-compile` — confirmed present per this crate's `Cargo.toml`.)

- [ ] **Step 2: Implement `run_incremental`**

```rust
pub fn run_incremental(&mut self, script: ember_bytecode::chunk::FunctionProto) -> Result<Value, RuntimeError> {
    self.stack.clear();
    self.open_upvalues.clear();
    let proto = std::rc::Rc::new(script);
    let closure = self.gc.allocate(crate::value::ClosureObj {
        proto,
        upvalues: Vec::new(),
    });
    // The closure itself is NEVER pushed onto the physical stack — verified
    // against Vm::new's real body. Only the natives loop below populates
    // `stack` (filling physical slots 0-7), exactly matching what a fresh
    // Vm::new(...) does; the closure lives solely in `frame.closure`.
    for &(name, arity, func) in crate::natives::NATIVES {
        let native = Value::Native(std::rc::Rc::new(crate::value::NativeFn { name, arity, func }));
        self.stack.push(native.clone());
        let key = self.gc.intern_str(name);
        self.globals.insert(key, native); // re-insert is a harmless idempotent overwrite for names already present from a prior entry
    }
    self.frames = vec![CallFrame {
        closure,
        ip: 0,
        slot_base: 0,
    }];
    self.run()
}
```
This exact shape (no closure push, natives-only stack population) was verified by implementing it against the real `Vm::new` body and running this task's own regression test end-to-end successfully — no further reconciliation needed, just confirm the real current `Vm::new`/`CallFrame`/`ClosureObj` shapes haven't changed since this plan was written.

- [ ] **Step 3: Run the new test**

Run: `cargo test -p ember-vm run_incremental`

- [ ] **Step 4: Run the full ember-vm test suite and the full workspace suite**

Run: `cargo test -p ember-vm`
Run: `cargo test --workspace`

- [ ] **Step 5: Run checks and commit**

`cargo clippy -p ember-vm --all-targets -- -D warnings`, `cargo fmt -p ember-vm -- --check`.
```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Add Vm::run_incremental: run a new chunk against existing globals/gc state, for the REPL"
```

---

### Task 15: REPL — rustyline scaffold, multi-line continuation, tree-walker backend

**Files:**
- Create: `crates/ember-cli/src/repl.rs`
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-cli/Cargo.toml` (add `rustyline`)

- [ ] **Step 1: Add `rustyline`**

```toml
rustyline = "14"
```

- [ ] **Step 2: Write the REPL session struct and tree-walker path**

```rust
// crates/ember-cli/src/repl.rs

use ember_ast::Interner;
use std::cell::RefCell;
use std::rc::Rc;

pub struct ReplSession {
    interner: Interner,
    buffer: String,
    stmt_count: usize,
    tree_env: Rc<RefCell<ember_tree::env::Env>>,
    tree_interp_ast_placeholder: (), // see Step 3's design note below
    backend: Backend,
    show_types: bool,
}

impl ReplSession {
    pub fn new(backend: Backend, show_types: bool) -> Self {
        ReplSession {
            interner: Interner::new(),
            buffer: String::new(),
            stmt_count: 0,
            tree_env: ember_tree::env::Env::new(),
            tree_interp_ast_placeholder: (),
            backend,
            show_types,
        }
    }
}
```

**Design note on the tree-walker path's `Ast` lifetime:** `Interp<'a>` borrows `&'a Ast`, and each REPL entry produces a *new* `Ast` (from re-parsing the growing buffer via `parse_into`). This means `Interp` can't be stored as a long-lived field on `ReplSession` the way `tree_env`/`interner` are — it must be constructed fresh *within* each entry's handling function, borrowing that entry's freshly-parsed `Ast` for exactly as long as that entry's `exec_stmt` calls need it, then dropped before the next entry re-parses. Restructure `ReplSession` to NOT hold `tree_interp_ast_placeholder`/any `Interp` field at all — only `interner`, `buffer`, `stmt_count`, `tree_env`, `backend`, `show_types` (remove the placeholder field above, it was scaffolding to make this note concrete, not real design).

- [ ] **Step 3: Implement `handle_entry` for the tree-walker backend**

```rust
impl ReplSession {
    pub fn handle_entry(&mut self, input: &str) {
        self.buffer.push_str(input);
        self.buffer.push('\n');
        let (ast, stmts, diags) = ember_parser::parse_into(&self.buffer, &mut self.interner);
        if !diags.is_empty() {
            for d in &diags {
                println!("{}", ember_diag::render::render(d, "<repl>", &self.buffer, crate::use_color()));
            }
            // roll back the buffer append — this entry didn't parse, don't keep it
            self.buffer.truncate(self.buffer.len() - input.len() - 1);
            return;
        }
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut self.interner, &stmts);
        if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
            for d in &resolve_diags {
                println!("{}", ember_diag::render::render(d, "<repl>", &self.buffer, crate::use_color()));
            }
            self.buffer.truncate(self.buffer.len() - input.len() - 1);
            return;
        }
        let (info, infer_diags) = ember_types::infer(&ast, &mut self.interner, &stmts);
        // (print any infer diagnostics the same way; on error, roll back and return — same pattern as above, elided here for brevity but must be written out in full in the real implementation)

        let new_stmts = &stmts[self.stmt_count..];
        if new_stmts.is_empty() {
            return;
        }
        match self.backend {
            Backend::Tree => {
                let mut interp = ember_tree::interp::Interp::new(&ast, &self.interner);
                let mut last = None;
                for &s in new_stmts {
                    match interp.exec_stmt(s, &self.tree_env) {
                        Ok(flow) => last = /* extract a printable Value from `flow` — check ember_tree's `EvalResult`/`Flow` type shape first, this sketch doesn't know its exact variants */,
                        Err(e) => {
                            println!("{}", ember_diag::render::render(&e.to_diagnostic(), "<repl>", &self.buffer, crate::use_color()));
                            self.stmt_count = stmts.len(); // still commit: the statement ran partway, its declared names may already be in tree_env
                            return;
                        }
                    }
                }
                if let Some(v) = last {
                    print!("{}", ember_tree::display_value(&v, &self.interner));
                    if self.show_types {
                        // look up this last statement's expression type from `info` and print it too
                    }
                    println!();
                }
            }
            Backend::Vm => { /* Task 16 */ }
        }
        self.stmt_count = stmts.len();
        let _ = bindings; // used by the Vm arm in Task 16; silence unused warning on this arm's own build until Task 16 lands, or restructure so this isn't needed
    }
}
```
This step's code has two deliberate open questions marked with comments (`EvalResult`/`Flow`'s exact shape, and the infer-diagnostic-rollback repetition) — resolve both by reading `crates/ember-tree/src/interp.rs`'s real `EvalResult`/`Flow` type definitions and by writing out the repeated diagnostic-print-and-rollback pattern in full (don't leave a comment placeholder in the actual committed code — the comment above is plan guidance, not permitted final code, per this project's own no-placeholders convention).

- [ ] **Step 4: Wire a `Repl` subcommand into `main.rs`**

```rust
Repl {
    #[arg(long, default_value = "tree")]
    backend: String, // or the Backend ValueEnum from Task 2
    #[arg(long)]
    show_types: bool,
},
```
```rust
Command::Repl { backend, show_types } => run_repl(backend.into(), show_types),
```
```rust
fn run_repl(backend: Backend, show_types: bool) -> ExitCode {
    let mut rl = rustyline::DefaultEditor::new().expect("failed to initialize line editor");
    let mut session = repl::ReplSession::new(backend, show_types);
    loop {
        let mut input = match rl.readline("ember> ") {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Eof) | Err(rustyline::error::ReadlineError::Interrupted) => break,
            Err(e) => {
                eprintln!("error: {e}");
                break;
            }
        };
        // multi-line continuation: keep reading while brackets are unbalanced
        while !brackets_balanced(&input) {
            match rl.readline("...    ") {
                Ok(more) => {
                    input.push('\n');
                    input.push_str(&more);
                }
                Err(_) => break,
            }
        }
        let _ = rl.add_history_entry(input.as_str());
        session.handle_entry(&input);
    }
    ExitCode::SUCCESS
}

fn brackets_balanced(src: &str) -> bool {
    let (tokens, _trivia, _diags) = ember_lexer::lex(src);
    let mut depth: i32 = 0;
    for t in &tokens {
        match t.kind {
            ember_lexer::TokenKind::LBrace | ember_lexer::TokenKind::LParen | ember_lexer::TokenKind::LBracket => depth += 1,
            ember_lexer::TokenKind::RBrace | ember_lexer::TokenKind::RParen | ember_lexer::TokenKind::RBracket => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}
```
(Verify `ember_lexer::TokenKind`'s real variant names for braces/parens/brackets by reading `crates/ember-lexer/src/lex.rs` — don't guess the exact identifiers.)

- [ ] **Step 5: Manual verification**

```bash
cargo run -p ember-cli -- repl
```
Interactively (or by piping input) confirm: `let x = 5;` then `x + 1;` on a second line prints `6`, not a re-print of the first entry, and not an "undeclared name" error.

- [ ] **Step 6: Run checks and commit**

`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/src/main.rs crates/ember-cli/src/repl.rs
git commit -m "Add the REPL: rustyline, multi-line continuation, incremental tree-walker execution"
```

---

### Task 16: REPL — VM backend integration

**Files:**
- Modify: `crates/ember-cli/src/repl.rs`

- [ ] **Step 1: Add a `Vm` field to `ReplSession`, initialized lazily or eagerly**

Add `vm: Option<ember_vm::vm::Vm>` (or eagerly construct one with an empty initial `FunctionProto` — decide based on whichever makes `run_incremental`'s "first call" story cleaner, per the open design question flagged in Task 14 Step 1) and a `repl_global_symbols: Vec<ember_ast::Symbol>` tracking every name ever top-level-declared across entries, for seeding future entries' resolvers.

- [ ] **Step 2: Implement the `Backend::Vm` arm of `handle_entry`**

**This step's design was an open question when this plan was first drafted — it has since been settled empirically and is no longer open.** Reusing the whole-buffer resolve's `Bindings` (from the broad resolve already run earlier in `handle_entry` for diagnostic validation) to compile just `new_stmts` was tried directly and **produces a real panic**: `index out of bounds: the len is 8 but the index is 8` at `vm.rs`'s `Op::GetLocal` handler. Root cause: in a single whole-buffer resolve pass, both the old and new statements share the same function context, so a name declared by an earlier statement resolves as `Resolution::Local` (continuous slot numbering across the whole buffer) rather than `Resolution::Global` — the same-function local lookup succeeds before the dedicated global-fallback path is ever reached. The compiled tail then emits `OP_GET_LOCAL` for a slot number that only existed in a *different, already-finished* `run()` call; `run_incremental`'s freshly-reset physical stack (8 native slots only) doesn't have it.

**The narrow/second-resolve shown below is the only correct design, confirmed working end-to-end** (verified as part of this plan's own review, using this exact scenario: `let x = 5;` then `x + 1;` two-entry sequence) — `x` correctly resolves as `Global` and round-trips through `vm.globals`, populated by the compiler's existing top-level dual-registration (`OP_DEFINE_GLOBAL`) mechanism:
```rust
Backend::Vm => {
    let mut resolver = ember_resolve::Resolver::new(&ast, &mut self.interner);
    resolver.seed_repl_globals(&self.repl_global_symbols);
    resolver.resolve_program(new_stmts); // resolve ONLY the new statements — required, see above; do not reuse the whole-buffer resolve's Bindings here
    if resolver.diagnostics().iter().any(|d| d.severity == ember_diag::Severity::Error) {
        for d in resolver.diagnostics() {
            println!("{}", ember_diag::render::render(d, "<repl>", &self.buffer, crate::use_color()));
        }
        return;
    }
    let (new_bindings, _) = resolver.into_bindings();
    let proto = ember_compile::compile(&ast, &mut self.interner, &new_bindings, new_stmts);
    let vm = self.vm.get_or_insert_with(|| ember_vm::vm::Vm::new(proto.clone() /* or however the "first ever entry" case is resolved, per Task 14 Step 1's own open note */));
    match vm.run_incremental(proto) {
        Ok(v) => {
            print!("{}", ember_vm::value::display_value(&v));
            println!();
        }
        Err(e) => {
            println!("{}", ember_diag::render::render(&e.to_diagnostic(&self.interner), "<repl>", &self.buffer, crate::use_color()));
        }
    }
    // record any newly top-level-declared names from this entry into self.repl_global_symbols for future entries to see as globals
}
```

- [ ] **Step 3: Extend the regression test from Task 14 into a REPL-level test**

Add to `crates/ember-cli/src/repl.rs`'s own test module (or a new `crates/ember-cli/tests/repl.rs` integration test):
```rust
#[test]
fn vm_backend_persists_state_across_entries_without_replaying_output() {
    let mut session = ReplSession::new(Backend::Vm, false);
    session.handle_entry("let x = 5;");
    session.handle_entry("let y = x + 1;");
    session.handle_entry("x + y;");
    // Assert the THIRD entry's result is 11, not a re-print of prior entries.
    // This will need handle_entry (or a variant) to return its result rather
    // than only printing it, for the test to assert on — consider adding a
    // `handle_entry_for_test(&mut self, input: &str) -> Option<String>` that
    // captures what would have been printed, or restructure `handle_entry`
    // to return `Option<Value>`/`Option<String>` and have the real REPL loop
    // print it, rather than printing directly inside `handle_entry` — the
    // latter is cleaner and testable; prefer it, adjusting Task 15's Step 3
    // design retroactively if needed (a legitimate, expected refactor at
    // this point, not scope creep — session-return-values are more testable
    // and just as simple to wire into the print-loop).
}
```

- [ ] **Step 4: Run the new test and the full test suite**

Run: `cargo test -p ember-cli`
Run: `cargo test --workspace`

- [ ] **Step 5: Manual end-to-end verification**

```bash
cargo run -p ember-cli -- repl --backend vm
```
Type `let x = 5;`, then `let y = x + 1;`, then `x + y;` — confirm `11` prints once, with no replayed output from earlier entries.

- [ ] **Step 6: Run checks and commit**

`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/src/repl.rs crates/ember-cli/tests/
git commit -m "Wire the VM backend into the REPL: real incremental execution via run_incremental"
```

---

### Task 17: REPL meta-commands (`:type`, `:ast`, `:disasm`, `:reset`, `:load`)

**Files:**
- Modify: `crates/ember-cli/src/repl.rs`

- [ ] **Step 1: Detect and dispatch meta-commands before normal entry handling**

In the REPL loop (or at the top of `handle_entry`), check if `input.trim()` starts with `:`; if so, parse the command name and argument, and dispatch instead of treating it as ember source:
```rust
if let Some(rest) = input.trim().strip_prefix(':') {
    self.handle_meta_command(rest);
    return;
}
```
```rust
fn handle_meta_command(&mut self, command: &str) {
    let (name, arg) = command.split_once(' ').unwrap_or((command, ""));
    match name {
        "reset" => {
            *self = ReplSession::new(self.backend, self.show_types);
            println!("session reset");
        }
        "load" => {
            match std::fs::read_to_string(arg.trim()) {
                Ok(contents) => self.handle_entry(&contents),
                Err(e) => eprintln!("error: could not read {}: {e}", arg.trim()),
            }
        }
        "type" => {
            // parse `arg` in the context of self.buffer + self.interner (append temporarily, parse, infer, print the expression's type, then discard — do NOT commit it to self.buffer/self.stmt_count, unlike :load)
        }
        "ast" => {
            // similar: parse-in-context, print via ember_ast::print_stmt or print_expr, discard
        }
        "disasm" => {
            // similar: parse-in-context, resolve, compile just this expression wrapped as a statement, disassemble, discard
        }
        other => eprintln!("unknown command: :{other}"),
    }
}
```
The three "parse in context, don't commit" commands (`:type`/`:ast`/`:disasm`) share a lot of structure — factor out a small helper that appends `arg` to a *copy* of `self.buffer` (not the real one), parses/resolves/infers against a *copy* of `self.interner` (since committing symbols from a discarded expression into the real session interner is harmless — interning is idempotent and append-only — but re-parsing against a real, uncommitted-buffer copy keeps `self.stmt_count`/`self.buffer` correctly unaffected), and returns whatever each command needs (a type, an AST, a chunk) for its own printing.

- [ ] **Step 2: Write tests for each meta-command**

At minimum: `:reset` clears prior state (a name declared before `:reset` is undeclared after), `:load` executes a file's contents through the same path as typed input (declares are visible afterward), `:type`/`:ast`/`:disasm` don't affect `self.stmt_count`/`self.buffer` (run one, then confirm a subsequent normal entry still sees exactly the state from before the meta-command, not polluted by it).

- [ ] **Step 3: Manual verification**

```bash
cargo run -p ember-cli -- repl
```
Try each meta-command interactively.

- [ ] **Step 4: Run checks and commit**

`cargo test -p ember-cli`, `cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`.
```bash
git add crates/ember-cli/src/repl.rs
git commit -m "Add REPL meta-commands: :type, :ast, :disasm, :reset, :load"
```

---

### Task 18: `Vm` introspection accessors for the debug TUI

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Add read-only snapshot accessors**

Read `Vm`'s real current private fields first (`stack: Vec<Value>`, `frames: Vec<CallFrame>`, `open_upvalues: Vec<Gc<UpvalueCell>>`, `globals: FxHashMap<Gc<String>, Value>`, `gc: GcHeap` per earlier investigation — confirm unchanged). Add:
```rust
pub fn stack(&self) -> &[Value] {
    &self.stack
}

pub fn frames(&self) -> &[CallFrame] {
    &self.frames
}

pub fn current_frame(&self) -> Option<&CallFrame> {
    self.frames.last()
}
```
`CallFrame` is already `pub struct CallFrame { pub closure: Gc<ClosureObj>, pub ip: usize, pub slot_base: usize }` (all fields already `pub`) — confirm this by reading the real struct, and if any field is private, decide whether the debug TUI genuinely needs it exposed before making it `pub` (don't blanket-expose fields the TUI won't use).

- [ ] **Step 2: Write tests confirming the accessors reflect real VM state**

```rust
#[test]
fn stack_snapshot_reflects_pushed_values() {
    let vm = compile_and_run_and_return_vm_before_completion(/* ... */); // design this test helper to run a program up to a known point (e.g. via `step()` a fixed number of times) and inspect `vm.stack()` — exact shape depends on what's easiest given the real Vm/StepOutcome API; don't guess, write this against the real types.
    assert!(!vm.stack().is_empty());
}

#[test]
fn frames_snapshot_shows_the_current_call_frame() {
    // similar — step into a function call, confirm frames().len() > 1
}
```

- [ ] **Step 3: Run tests and checks**

`cargo test -p ember-vm`, `cargo clippy -p ember-vm --all-targets -- -D warnings`, `cargo fmt -p ember-vm -- --check`.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Add read-only Vm introspection accessors for the debug TUI"
```

---

### Task 19: `debug` TUI

**Files:**
- Create: `crates/ember-cli/src/debug_tui.rs`
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-cli/Cargo.toml` (add `ratatui`, `crossterm`)

- [ ] **Step 1: Add dependencies**

```toml
ratatui = "0.28"
crossterm = "0.28"
```
(Check crates.io for the current latest 0.x line at implementation time — pin to whatever `cargo add ratatui` resolves, don't hardcode a version that might already be stale.)

- [ ] **Step 2: Build the debugger state**

```rust
// crates/ember-cli/src/debug_tui.rs

pub struct DebugState {
    vm: ember_vm::vm::Vm,
    interner: ember_ast::Interner,
    src: String,
    finished: bool,
}

impl DebugState {
    pub fn step(&mut self) {
        if self.finished {
            return;
        }
        match self.vm.step() {
            Ok(ember_vm::vm::StepOutcome::Running) => {}
            Ok(ember_vm::vm::StepOutcome::Done(_)) => self.finished = true,
            Err(_) => self.finished = true, // surface the error in the UI, don't just swallow it — render it in the status area rather than silently stopping
        }
    }
}
```

- [ ] **Step 3: Build the ratatui render function**

Four panels via a `ratatui::layout::Layout`: source (top-left, current-line highlighted using the current frame's chunk line table — reuse `Chunk`'s existing line-tracking, the same data `disassemble_chunk` already prints per-instruction), stack (top-right, one line per `Value` via `display_value`), locals (bottom-left, current frame's slots mapped to names — this needs the resolver's `Bindings`/a name-per-slot map threaded through from compile time, since `Vm` itself has no notion of names, only slot numbers; decide whether to thread this through `DebugState` at construction time from the same `Bindings` the compile step already produced), next instruction (bottom-right, one line from `disassemble_instruction` at the current frame's `ip`).

- [ ] **Step 4: Build the event loop**

Standard `ratatui` + `crossterm` pattern: enter raw mode, alternate screen, loop on `crossterm::event::read()`, dispatch key presses (`s` step, `o` step-over, `b` set a breakpoint at the current source line, `r` run-to-next-breakpoint, `q` quit), restore terminal state on exit (including on panic — use a guard/`Drop` impl or a `std::panic::catch_unwind` wrapper so a mid-render panic doesn't leave the user's terminal in raw mode).

- [ ] **Step 5: Wire the `Debug` subcommand**

```rust
Debug { file: String },
```
Build the initial `DebugState` from the same parse/resolve/infer/exhaustiveness/compile pipeline every other command uses, stopping with the usual diagnostic-printing exit codes on any failure before ever entering the TUI.

- [ ] **Step 6: Manual verification**

```bash
cargo run -p ember-cli -- debug tests/conformance/recursion.em
```
Interactively step through a few instructions, confirm the stack/locals/next-instruction panels update correctly and match what `ember disasm` shows for the same program. Quit cleanly and confirm the terminal is left in a normal (non-raw, non-alternate-screen) state.

- [ ] **Step 7: Run checks and commit**

`cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`, `cargo test -p ember-cli`.
```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/src/main.rs crates/ember-cli/src/debug_tui.rs
git commit -m "Add the debug TUI: ratatui stepper over Vm::step with source/stack/locals/next-instruction panels"
```

---

### Final Task: Full workspace verification and CHECKLIST.md reconciliation

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`. Expected all green, including every new test from Tasks 1-19.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`

- [ ] **Step 3: End-to-end manual smoke test of every new command**

```bash
cargo run -p ember-cli -- run tests/conformance/arithmetic.em --backend vm --time
cargo run -p ember-cli -- check tests/conformance/arithmetic.em
cargo run -p ember-cli -- types tests/conformance/higher_order.em
cargo run -p ember-cli -- trace tests/conformance/generics.em
cargo run -p ember-cli -- disasm tests/conformance/closures.em
cargo run -p ember-cli -- bench tests/conformance/recursion.em
cargo run -p ember-cli -- explain E0301
cargo run -p ember-cli -- completions bash | head -3
```
Confirm every one produces sane, real output (not a panic, not an "unimplemented" stub).

- [ ] **Step 4: Reconcile CHECKLIST.md's Phase 13 section**

Check every item against what was actually built. Mark 🔴/🟡 items done with honest notes on any scope decisions made during implementation (matching the precedent set in every prior phase's own reconciliation) — in particular: `trace`'s final-substitution-only rendering (not per-step snapshots), the error-code registry's actual final code count versus the ~20-30 estimated, and any REPL/VM-incremental design detail that ended up different from this plan's sketch once real compiler-error-driven verification happened during Tasks 15-17. Leave the two 🟢 items (`--emit` pipeline dumping, per-phase `--time` breakdown) unchecked with a note that they were deferred, per the design doc's own non-goals.

- [ ] **Step 5: Commit**

```bash
git add CHECKLIST.md
git commit -m "Reconcile CHECKLIST.md Phase 13: CLI & REPL"
```
