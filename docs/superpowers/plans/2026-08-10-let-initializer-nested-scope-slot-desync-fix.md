# `let`-initializer nested-scope slot desync: bug fix implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix a real VM miscompilation bug — `let NAME = <expr containing its own nested scope, e.g. a block or if/else>` desyncs the resolver's and compiler's local-slot numbering, causing either a VM panic or (worse) a silently wrong value, while the tree-walker is unaffected.

**Architecture:** See `docs/superpowers/specs/2026-08-10-let-initializer-nested-scope-slot-desync-fix-design.md` for full root-cause analysis. Fix: split `declare_named_local` (`crates/ember-compile/src/compiler.rs`) into a `reserve_named_local` (slot bookkeeping only) called *before* compiling `Stmt::Let`'s initializer, and an explicit `maybe_dual_register` call after the initializer's result is written into that reserved slot via `Op::SetLocal` + `Op::Pop` — mirroring the `emit_tail_scope_exit`/`compile_for`'s already-established "reserve a slot, write into it later, pop the leftover duplicate" pattern in the same file.

**Tech Stack:** Pure Rust, `ember-compile` crate. No new dependencies.

---

### Task 1: VM regression tests reproducing the bug (written first, confirmed failing)

**Files:**
- Modify: `crates/ember-vm/src/vm.rs` (new `#[cfg(test)]` tests, following the file's existing end-to-end test conventions — search for an existing test that parses+resolves+compiles+runs a source string and asserts on the resulting `Value`, and match its exact helper-function usage)

- [ ] **Step 1: Use the existing end-to-end test helper**

`crates/ember-vm/src/vm.rs` (line 1224) already has the helper existing tests use to go from an ember source string to a run result: `fn compile_and_run(src: &str) -> Result<Value, RuntimeError>` (parse → resolve → compile → `Vm::new(proto).run()`, panicking via `assert!` on any parse/resolve diagnostics). Use it directly — do not write a new one.

- [ ] **Step 2: Write three failing regression tests, plus a fourth for the `for`-loop case**

```rust
#[test]
fn let_with_a_block_initializer_containing_one_inner_let_works() {
    let v = compile_and_run("let y = { let a = 1; a }; y;").unwrap();
    assert_eq!(v, Value::Int(1));
}

#[test]
fn let_with_a_block_initializer_containing_two_inner_lets_works() {
    let v = compile_and_run("let y = { let a = 1; let b = 2; a + b }; y;").unwrap();
    assert_eq!(v, Value::Int(3));
}

#[test]
fn let_with_an_if_else_initializer_containing_an_inner_let_works() {
    let v = compile_and_run("let y = if true { let a = 10; a } else { 0 }; y;").unwrap();
    assert_eq!(v, Value::Int(10));
}

#[test]
fn let_with_a_nested_scope_initializer_inside_a_for_loop_body_works() {
    // Exercises the fix under active `slot_shifts` (for-loop desugaring
    // shifts physical slot numbers) — no pre-existing test puts a `let`
    // inside a `for`-loop body at all, so this is new coverage, not just
    // a regression check.
    let v = compile_and_run(
        "let xs = [1, 2, 3]; let mut total = 0; for x in xs { let y = { let a = x; a + 1 }; total = total + y; } total;",
    )
    .unwrap();
    assert_eq!(v, Value::Int(9));
}
```

- [ ] **Step 3: Run the tests and confirm they fail**

Run: `cargo test -p ember-vm let_with_a_block_initializer let_with_an_if_else let_with_a_nested_scope_initializer_inside_a_for_loop_body`
Expected: the block/if-else tests FAIL with a panic (`index out of bounds`) or a wrong-value assertion mismatch (`4 != 3`); the `for`-loop test FAILS with the same panic — confirming the bug reproduces under active slot shifts too, exactly as documented in the design doc, before any fix is applied.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Add failing regression tests for let-initializer nested-scope slot desync"
```

---

### Task 2: The compiler fix

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

- [ ] **Step 1: Split `declare_named_local` into a reusable reservation step**

Find the current `declare_named_local` (around line 584):
```rust
    fn declare_named_local(&mut self, name: Symbol, line: u32) {
        let resolver_slot = self.current().local_count - self.current().total_shift();
        self.push_local(Some(resolver_slot));
        self.maybe_dual_register(name, line);
    }
```
Replace with:
```rust
    /// Reserves one new physical local slot — bookkeeping only, no dual
    /// registration. The caller must already have pushed the value (or a
    /// placeholder) that will live in this slot; returns the slot's real
    /// physical position (for `Op::SetLocal`/`Op::GetLocal` operands),
    /// distinct from the resolver slot number recorded internally for
    /// `slot_is_captured` lookups.
    ///
    /// Split out of the old combined `declare_named_local` for `Stmt::Let`
    /// (see this crate's design doc, 2026-08-10), which must reserve its
    /// name's slot *before* compiling its initializer — matching the
    /// resolver's own declare-before-resolve order (needed so `let x = x
    /// + 1;` can be rejected as a self-reference) — but must not
    /// dual-register a placeholder value as the name's global.
    fn reserve_named_local(&mut self) -> u32 {
        let resolver_slot = self.current().local_count - self.current().total_shift();
        self.push_local(Some(resolver_slot));
        self.current().local_count - 1
    }

    fn declare_named_local(&mut self, name: Symbol, line: u32) {
        self.reserve_named_local();
        self.maybe_dual_register(name, line);
    }
```

- [ ] **Step 2: Run the existing test suite to confirm this refactor alone changes nothing**

Run: `cargo test -p ember-compile`
Expected: PASS, identical to before this step — `declare_named_local`'s observable behavior for its existing `StructDecl`/`Fn` callers is unchanged (same two calls happen in the same order), only `Stmt::Let` (Step 3) will actually use the new split.

- [ ] **Step 3: Rewrite `Stmt::Let`'s compilation**

Find (around line 608):
```rust
            ember_ast::Stmt::Let { name, init, .. } => {
                self.compile_expr(init);
                self.declare_named_local(name, line);
            }
```
Replace with:
```rust
            ember_ast::Stmt::Let { name, init, .. } => {
                // Reserve `name`'s slot BEFORE compiling `init`, matching
                // the resolver's own declare-before-resolve order (see
                // this crate's design doc, 2026-08-10) — otherwise any
                // nested scope inside `init` (a block, an if/else branch)
                // gets resolver slot numbers one higher than where this
                // compiler's `local_count` actually puts them, since the
                // resolver already "spent" a slot on `name` that this
                // compiler hasn't reserved yet.
                self.current().emit_op(Op::Nil, line);
                let slot = self.reserve_named_local();
                self.compile_expr(init);
                self.emit_set_local(slot, line);
                self.current().emit_op(Op::Pop, line); // discard SetLocal's duplicate
                self.maybe_dual_register(name, line);
            }
```

- [ ] **Step 4: Run the VM regression tests from Task 1**

Run: `cargo test -p ember-vm let_with_a_block_initializer let_with_an_if_else`
Expected: all 3 PASS now.

- [ ] **Step 5: Run the full `ember-compile` test suite and fix assertions that encoded the old bytecode shape**

Run: `cargo test -p ember-compile`

Expect exactly 4 failures (verified empirically against the real fix before this plan was finalized — trust this list and these counts over re-deriving them from scratch, though still read each failure's actual output to confirm) — every `Stmt::Let` now unconditionally emits its own `OP_SET_LOCAL` + `OP_POP` pair, which several existing tests assert does NOT happen (they were isolating a block's own scope-exit behavior by wrapping it in a `let`, or asserting no pop for a bare top-level `let`). For each failure, read the test, confirm the failure matches what's described below, and update the assertion with a comment explaining why it changed:

- `a_top_level_let_gets_dual_registration` (asserts `!out.contains("OP_POP")` for `let _x = 1;`) — this is now false; every `let` emits one `OP_POP` for its own `SetLocal` duplicate. Change to assert `out.contains("OP_POP")` instead, with a comment: this pop is `Stmt::Let`'s own `SetLocal`-duplicate discard (present for every `let`, not evidence of the local being torn down).

- `a_block_with_two_locals_and_a_tail_pops_both_and_keeps_the_tail` (source: `let _r = { let a = 1; let b = 2; a + b };`, currently asserts exactly 2 `OP_POP`) — the real new count is **5, not a naively-expected 3**. Breakdown (confirmed via disassembly): the inner `let a = 1;` and `let b = 2;` are themselves now-fixed `Stmt::Let`s and each gets its own new `SetLocal`+`Pop` pair (2 pops), THEN the block's own pre-existing `emit_tail_scope_exit` cleanup contributes its usual 2 pops (unchanged by this fix), THEN the outer `let _r`'s own new `SetLocal`+`Pop` pair contributes 1 more pop — 2 + 2 + 1 = 5. Update the assertion to `assert_eq!(pop_count, 5, ...)` with a comment giving this breakdown (don't just change the number without explaining it, since a future reader hitting this test after another compiler change needs to be able to tell which of the three sources shifted).

- `a_block_with_no_locals_emits_no_pops` (asserts NO `OP_POP`/`OP_SET_LOCAL` for `let _r = { 1 + 2 };`) — this test's whole premise (bind to `let` to avoid `ExprStmt`'s own pop, isolating the block's behavior) no longer isolates what it claims to, since `let` itself now always emits `SetLocal`+`Pop`. Rewrite it to assert on the *count* instead: exactly 1 `OP_SET_LOCAL` and exactly 1 `OP_POP` (both attributable to the outer `let`, none to the block, since the block itself declares zero locals) — update the test name/comment to describe what it now actually isolates (the outer let's own pair being the ONLY pair present), or rename it if `a_block_with_no_locals_emits_no_pops` no longer accurately describes the assertion.

- `the_last_top_level_expression_statements_value_is_the_programs_result` (source: `let a = 3; let b = 4; a * a + b * b;`, around line 2674-2694, asserts `!out.contains("OP_POP")` on the theory that top-level `let`s and the final tail expression never pop) — now false: the two top-level `let`s each emit their own `SetLocal`+`Pop` pair (2 `OP_POP`s total), while the final expression statement's value still correctly flows straight into `OP_RETURN` with no extra pop. Update to assert exactly 2 `OP_POP`, keeping this test's other existing assertions (about `OP_RETURN`/`OP_ADD` ordering in `last_two[...]`) unchanged — those aren't affected by this fix.

Do not weaken any assertion beyond what's needed to reflect the new, intentionally-changed bytecode shape — if a test fails for a reason other than the four described above, stop and investigate rather than adjusting it.

- [ ] **Step 6: Re-run until green**

Run: `cargo test -p ember-compile`
Expected: PASS, all tests (including the ones updated in Step 5).

- [ ] **Step 7: Run clippy and fmt**

Run: `cargo clippy -p ember-compile --all-targets -- -D warnings`
Run: `cargo fmt -p ember-compile -- --check`
Expected: both clean.

- [ ] **Step 8: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS — this is the broadest available check that the fix doesn't regress anything elsewhere (resolver, tree-walker, GC, formatter, etc. are all unaffected by this change in principle, but confirm).

- [ ] **Step 9: Commit**

```bash
git add crates/ember-compile/src/compiler.rs
git commit -m "Fix let-initializer nested-scope slot desync: reserve the name's slot before compiling its initializer"
```

---

### Task 3: Restore the natural `shadowing.em` conformance fixture

**Files:**
- Modify: `tests/conformance/shadowing.em`, `tests/conformance/shadowing.expected`

Phase 12's Task 1 originally wanted `let y = { let x = x + 1; let x = x * 10; x };`-style shadowing but hit an unrelated resolver rule (self-reference rejection, not this bug) and worked around it with a function-local helper instead. This task is about the pattern this bug fix specifically targets — a block-expression `let`-initializer with its own internal `let`s — so use that shape here instead (still avoiding the unrelated self-reference-in-own-initializer rule, which is correct resolver behavior, not a bug).

- [ ] **Step 1: Rewrite the fixture**

```
// tests/conformance/shadowing.em
let x = 1;
let y = {
    let inner = x + 1;
    let inner = inner * 10;
    inner
};
x + y;
```

- [ ] **Step 2: Verify on both backends**

```bash
cargo run -p ember-cli -- run tests/conformance/shadowing.em
cargo run -p ember-cli -- vm tests/conformance/shadowing.em
```
Both should print the same value; compute it by hand first (`x=1`, `inner=2`, `inner=20`, `y=20`, `x+y=21`) and write that to `.expected`, then confirm the real runs match.

```
// tests/conformance/shadowing.expected
21
```

- [ ] **Step 3: Run the full conformance suite**

Run: `cargo test -p ember-cli --test conformance`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/conformance/shadowing.em tests/conformance/shadowing.expected
git commit -m "Restore natural nested-let shadowing fixture now that the slot desync bug is fixed"
```

---

### Task 4: CHECKLIST.md note

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Add a note to Phase 9's compiler section**

Find Phase 9's (bytecode compiler) checklist section and add a brief note (matching the style of the existing Or-pattern fix note) documenting: a `let`-initializer/nested-scope local-slot desync bug was found (Phase 12, while writing conformance fixtures) and fixed — reference the design doc filename. Keep it to 2-3 sentences, matching the existing note style already in the file for the Or-pattern fix.

- [ ] **Step 2: Commit**

```bash
git add CHECKLIST.md
git commit -m "Note the let-initializer nested-scope slot desync fix in CHECKLIST.md"
```
