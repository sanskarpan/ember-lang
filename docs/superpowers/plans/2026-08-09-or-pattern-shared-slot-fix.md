# Or-pattern shared-slot binding fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two confirmed, related correctness bugs in `match` compilation: (1) an `Or`-pattern that binds a name in more than one alternative (e.g. `Circle(r) | Square(r) => r`) resolves and compiles that name to inconsistent slots — an out-of-bounds panic in the worst case; (2) any guarded pattern that binds a name, where the guard can fail with another arm following, leaks the bound value onto the stack, silently corrupting whatever code runs next (confirmed to affect even the currently-passing `guard_and_or_pattern_both_work` test, which only passes by numeric coincidence).

**Architecture:** Three coordinated fixes, all in `ember-resolve`/`ember-compile`:
1. Resolver declares each distinct `Or`-pattern-bound name exactly once, not once per occurrence.
2. Compiler pre-reserves one physical stack slot per distinct name *before* any alternative's test runs, and every alternative writes into that reserved slot via a new, parallel bind-compilation path — **plus** pops those reserved slots explicitly if every alternative's test fails (a gap found by subagent review of an earlier version of this plan; without it, the pre-reservation itself leaks).
3. `compile_match`'s own guard-handling pops however many locals a pattern actually bound if its guard then fails, before falling through to the next arm (the second, independently-discovered bug — same root cause, no `Or` involved).

See `docs/superpowers/specs/2026-08-09-or-pattern-shared-slot-fix-design.md` for the full root-cause analysis, including the hand-traced bytecode walkthroughs that verify all three fixes compose correctly.

**Tech Stack:** Rust 2021, existing `ember-resolve`/`ember-compile`/`ember-vm` crates. `rustc-hash` is already a dependency of both `ember-resolve` and `ember-compile` (not yet imported in either file this plan touches).

---

## Before you start (context every task needs)

- **Both fixes share one technique**: on a failure path that jumps past a scope's normal cleanup (`emit_tail_scope_exit`) but where real values were genuinely pushed onto the stack first (reserved-but-unbound `Nil` placeholders, or a really-matched-and-bound pattern whose guard then failed), emit **raw `Op::Pop`** — via `self.current().emit_op(Op::Pop, line)`, in a loop, the exact count needed — **never** `emit_scope_pops` (which also mutates `local_count`/`declared_slots`). Those counters must stay exactly as they were left for the success-path code compiled immediately afterward in the same linear stream, which still needs to correctly reference the same slots. The failure-path cleanup only needs to emit *bytecode*, not touch the compiler's ongoing bookkeeping.
- **Why `emit_tail_scope_exit` resetting `local_count` unconditionally matters**: it runs as part of the compiler's own linear code generation, regardless of whether that particular bytecode is runtime-reachable on a given path. This is *why* the next arm's `arm_entry_local_count` capture is already correct without any extra work on your part — but it's also *why* a "before scope-exit runs" vs. "after scope-exit runs" distinction matters when capturing counts like `bound_count` for the guard fix (capture before, since after it's always zero).
- Do not modify `compile_pattern_bind`/`compile_destructured_bind` (used for every non-`Or` pattern). This fix adds new, parallel functions used only for `Or`-alternative binds, plus small, additive changes to `compile_pattern_match`'s `Or` branch and `compile_match`'s guard handling.
- An existing compiler test, `an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping` (`crates/ember-compile/src/compiler.rs`), already proves compilation doesn't panic — but only asserts `stack_depth` bookkeeping is sound, not that the emitted bytecode is *semantically correct*. Do not remove or weaken it; this plan adds new tests alongside it.

---

### Task 1: Resolver fix — declare each distinct name once, not once per occurrence

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

- [ ] **Step 1: Write the failing test first**

Add to the `mod tests` block at the bottom of `crates/ember-resolve/src/resolver.rs`:

```rust
#[test]
fn or_pattern_repeated_name_declares_one_slot_not_one_per_occurrence() {
    // Every name is `_`-prefixed (type `_Shape`, both variants, and `_f`
    // itself) because this test only cares about slot bookkeeping, not
    // program behavior — nothing here is ever called or constructed, and
    // this resolver test's own `assert!(diags.is_empty())` below is
    // strict (unlike `ember-compile`'s `assert_no_errors`, it catches
    // warnings too), so every declared name that's never read/called
    // needs the underscore convention already established elsewhere in
    // this codebase (constructors only ever referenced from pattern
    // position, and the enclosing type name, are never marked "used" —
    // see `ember-vm`'s `nullary_adt_variant_constructs_via_match` for the
    // same convention applied at the VM-test level).
    let src = "
        type _Shape = _Circle(Int) | _Square(Int);
        fn _f(s) {
            match s {
                _Circle(r) | _Square(r) => r,
                _ => 0,
            }
        }
    ";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let (bindings, diags) = resolver.into_bindings();
    assert!(diags.is_empty(), "diags: {diags:?}");
    // stmts[0] is the `type` decl, stmts[1] is `fn f`.
    let fn_id = crate::binding::FunctionId::Fn(stmts[1]);
    // `f`'s own frame: param `s` (slot 0), the match's own hidden
    // scrutinee slot (slot 1, reserved by `Expr::Match`'s resolution —
    // see CHECKLIST.md's Phase 9 retroactive fixes section), and `r`
    // (slot 2) — bound identically by both alternatives, so it must
    // consume exactly ONE slot, not two. High water = 3.
    assert_eq!(
        bindings.frame_sizes.get(&fn_id),
        Some(&3),
        "Circle(r) | Square(r) must declare `r` once, not once per alternative \
         (got {:?} — a value of 4 means the pre-fix double-declare bug is still \
         present)",
        bindings.frame_sizes.get(&fn_id)
    );
}
```

- [ ] **Step 2: Run test to verify it fails, and confirms the bug**

Run: `cargo test -p ember-resolve or_pattern_repeated_name_declares_one_slot`
Expected: FAILS. The assertion should report `Some(&4)` instead of the expected `Some(&3)` — this is the bug, empirically confirmed and precisely quantified, before you write a single line of fix.

If it fails differently (e.g. a parse/resolve diagnostic instead of a slot-count mismatch), stop and investigate before proceeding.

- [ ] **Step 3: Add a read-only pattern-name collector**

Add this new method to `Resolver`'s `impl` block in `crates/ember-resolve/src/resolver.rs`, near `declare_pattern_bindings`:

```rust
/// Collects every name a pattern would bind, in traversal order, WITH
/// duplicates if the same name is bound more than once (e.g. across two
/// alternatives of an `Or`) — the caller decides how to dedupe. A
/// read-only mirror of `declare_pattern_bindings`'s own traversal shape.
fn collect_pattern_bind_names(&self, pat: Idx<ember_ast::Pattern>, out: &mut Vec<ember_ast::Symbol>) {
    use ember_ast::Pattern;
    match self.ast.pat(pat) {
        Pattern::Wild
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Error => {}
        Pattern::Bind(sym) => out.push(*sym),
        Pattern::Ctor { args, .. } => {
            for a in args {
                self.collect_pattern_bind_names(*a, out);
            }
        }
        Pattern::Tuple(items) => {
            for i in items {
                self.collect_pattern_bind_names(*i, out);
            }
        }
        Pattern::List { items, rest } => {
            for i in items {
                self.collect_pattern_bind_names(*i, out);
            }
            if let Some(r) = rest {
                self.collect_pattern_bind_names(*r, out);
            }
        }
        Pattern::Record { fields, .. } => {
            for (_, p) in fields {
                self.collect_pattern_bind_names(*p, out);
            }
        }
        Pattern::Or(alts) => {
            for a in alts {
                self.collect_pattern_bind_names(*a, out);
            }
        }
    }
}
```

- [ ] **Step 4: Fix `declare_pattern_bindings`'s `Pattern::Or` arm**

In the same file, find `declare_pattern_bindings`'s existing `Pattern::Or(alts)` arm:

```rust
            Pattern::Or(alts) => {
                for a in alts {
                    self.declare_pattern_bindings(*a);
                }
            }
```

Replace with:

```rust
            Pattern::Or(alts) => {
                // Every alternative could bind the same name (that's the
                // whole point of `Circle(r) | Square(r) => r`) — but only
                // one alternative's bind ever executes at runtime, so
                // this must declare each DISTINCT name exactly once, not
                // once per occurrence. Declaring once per occurrence
                // advances `next_slot` once per occurrence while only
                // one physical slot is ever reserved for it (see
                // `ember-compile`'s matching fix in
                // `compile_pattern_match`), permanently desyncing every
                // local declared afterward in this scope — `pop_scope`
                // only releases as many slots as there are DISTINCT keys
                // in the scope's binding map, not one per `declare` call.
                let mut names = Vec::new();
                for a in alts {
                    self.collect_pattern_bind_names(*a, &mut names);
                }
                let mut declared = rustc_hash::FxHashSet::default();
                for name in names {
                    if declared.insert(name) {
                        self.functions[current].declare(name, false, true, span);
                    }
                }
            }
```

Add `use rustc_hash::FxHashSet;` to the top-level `use` block of `crates/ember-resolve/src/resolver.rs` if not already present (it isn't, as of this plan).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ember-resolve or_pattern_repeated_name_declares_one_slot`
Expected: PASS.

Run: `cargo test -p ember-resolve`
Expected: full crate suite passes (no regressions).

- [ ] **Step 6: Commit**

```bash
git add crates/ember-resolve/src/resolver.rs
git commit -m "Resolver: declare each Or-pattern alternative's bound name once, not once per occurrence"
```

---

### Task 2: Compiler fix — pre-reserve shared slots, bind into them, clean up on total failure

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

- [ ] **Step 1: Write the failing tests first**

Add to the `mod tests` block in `crates/ember-compile/src/compiler.rs`, near the existing `an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping` test:

```rust
#[test]
fn an_or_pattern_binding_the_same_name_writes_both_alternatives_into_the_same_slot() {
    // Complements the existing
    // `an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping`
    // test (which only proves compilation doesn't panic) by asserting the
    // emitted bytecode is actually consistent: whichever alternative's
    // bind runs, it must write `r` into the SAME physical slot, since the
    // arm body has only one `GetLocal` for `r` and it can't know at
    // compile time which alternative will have matched.
    //
    // Uses `compile_program_chunk` + `disassemble_recursively` (not
    // `compile_program_str`, which only disassembles the top-level chunk
    // and would show nothing for code inside `fn _f`'s own nested chunk —
    // see `disassemble_recursively`'s own doc comment for why).
    let src = "type Shape = Circle(Int) | Square(Int); fn _f(s) { match s { Circle(r) | Square(r) => r, _ => 0, } }";
    let (chunk, interner) = compile_program_chunk(src);
    let out = disassemble_recursively(&chunk, "test", &interner);
    let set_local_lines: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("OP_SET_LOCAL"))
        .collect();
    assert_eq!(
        set_local_lines.len(),
        2,
        "one OP_SET_LOCAL per alternative's bind of `r`: {out}"
    );
    assert_eq!(
        set_local_lines[0].split_whitespace().last(),
        set_local_lines[1].split_whitespace().last(),
        "both alternatives must write `r` into the same slot: {out}"
    );
}

#[test]
fn an_or_pattern_with_a_binding_that_does_not_match_pops_its_reserved_slot() {
    // The specific bug a subagent review caught in an earlier version of
    // this fix: the reserved slot(s) are pushed unconditionally BEFORE
    // any alternative's test runs. If every alternative's test fails,
    // nothing pops them — `fail_jumps` jumps straight past the
    // success-path body and its `emit_tail_scope_exit` cleanup. This
    // can't be caught by a compile-time-only assertion (it's a genuine
    // stack-height/value bug, only observable by running the bytecode —
    // see Task 4's VM-level regression test for the real proof), but this
    // at least confirms the expected `OP_POP` is present in the
    // disassembly on the "all alternatives failed" path, right before the
    // jump to the next arm.
    let src = "type Shape = Circle(Int) | Square(Int) | Triangle(Int); fn _f(s) { match s { Circle(r) | Square(r) => r, _ => -1, } }";
    let (chunk, interner) = compile_program_chunk(src);
    let out = disassemble_recursively(&chunk, "test", &interner);
    assert!(
        out.contains("OP_POP"),
        "expected at least one OP_POP cleaning up the reserved (but never bound) \
         slot on the all-alternatives-failed path: {out}"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ember-compile an_or_pattern_binding_the_same_name_writes_both_alternatives_into_the_same_slot`
Expected: FAILS — before this fix, `compile_pattern_match`'s `Or` handling never emits `OP_SET_LOCAL` for pattern binds at all (it uses push-and-declare), so `set_local_lines.len()` would be `0`, not `2`.

(The second new test, `an_or_pattern_with_a_binding_that_does_not_match_pops_its_reserved_slot`, may or may not fail at this exact point depending on how much of Step 1 vs. Step 5 below has landed when you run it — that's fine, just confirm it passes once the full fix (through Step 5) is in place, in Step 6.)

- [ ] **Step 3: Add `FxHashMap` import**

Add `use rustc_hash::FxHashMap;` to the top-level `use` block of `crates/ember-compile/src/compiler.rs`.

- [ ] **Step 4: Add the read-only pattern-name collector (compiler's own copy)**

Add this new method to the `impl` block containing `compile_pattern_bind` etc. in `crates/ember-compile/src/compiler.rs`:

```rust
/// Collects every name a pattern would bind, in traversal order, WITH
/// duplicates — mirrors `ember-resolve`'s own `collect_pattern_bind_names`
/// exactly (both crates need the identical distinct-name set for the
/// same `Or`-pattern, computed the same way, to stay in lockstep). Not
/// shared as a crate dependency between the two — `ember-compile` reads
/// `Pattern` directly off its own `&Ast`, same as `ember-resolve` does off
/// its own, and duplicating this ~20-line traversal is simpler than
/// inventing a shared abstraction for one function.
fn collect_pattern_bind_names(&self, pat: Idx<ember_ast::Pattern>, out: &mut Vec<Symbol>) {
    match self.ast.pat(pat) {
        Pattern::Wild
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::Error => {}
        Pattern::Bind(sym) => out.push(*sym),
        Pattern::Ctor { args, .. } => {
            for a in args {
                self.collect_pattern_bind_names(*a, out);
            }
        }
        Pattern::Tuple(items) => {
            for i in items {
                self.collect_pattern_bind_names(*i, out);
            }
        }
        Pattern::List { items, rest } => {
            for i in items {
                self.collect_pattern_bind_names(*i, out);
            }
            if let Some(r) = rest {
                self.collect_pattern_bind_names(*r, out);
            }
        }
        Pattern::Record { fields, .. } => {
            for (_, p) in fields {
                self.collect_pattern_bind_names(*p, out);
            }
        }
        Pattern::Or(alts) => {
            for a in alts {
                self.collect_pattern_bind_names(*a, out);
            }
        }
    }
}
```

- [ ] **Step 5: Add the two parallel "bind into reserved slot" functions**

Add these two new methods to the same `impl` block, right after `compile_destructured_bind` (which stays completely unmodified):

```rust
/// Like `compile_destructured_bind`, but used only from
/// `compile_pattern_bind_into_reserved` (see that function's doc
/// comment). Any intermediate temp needed to hold a nested sub-pattern's
/// own destructured value (e.g. unpacking `Pair` before reaching a bind
/// two levels deep) is self-cleaning: pushed, used, then popped again
/// before returning — unlike `compile_destructured_bind`'s temps, which
/// are deliberately left for the enclosing scope-exit to sweep up, these
/// must not persist, since they don't correspond to any slot the
/// resolver counted (only the FINAL bound names do, via `reserved`).
fn compile_destructured_bind_into_reserved(
    &mut self,
    sub_pat: Idx<ember_ast::Pattern>,
    scrutinee_slot: u32,
    source: DestructureSource,
    reserved: &FxHashMap<Symbol, u32>,
    line: u32,
) {
    if matches!(self.ast.pat(sub_pat), Pattern::Wild | Pattern::Error) {
        return;
    }
    self.emit_get_local(scrutinee_slot, line);
    match source {
        DestructureSource::Positional(i) => {
            self.current().chunk.write_op(Op::Destructure, line);
            self.current().chunk.write_u8(i, line);
            self.current().adjust_depth(0);
        }
        DestructureSource::Named(sym) => {
            let c = self.name_constant(sym);
            self.current().emit_op(Op::GetField, line);
            self.current().chunk.write_u16(c, line);
        }
        DestructureSource::Indexed(i) => {
            let c = self.current().chunk.add_constant(Value::Int(i));
            self.emit_constant(c, line);
            self.current().emit_op(Op::GetIndex, line);
        }
    }
    match self.ast.pat(sub_pat).clone() {
        Pattern::Bind(sym) => {
            let slot = reserved[&sym];
            self.emit_set_local(slot, line);
            self.current().emit_op(Op::Pop, line);
        }
        _ => {
            self.push_local(None);
            let temp_slot = self.current().local_count - 1;
            self.compile_pattern_bind_into_reserved(sub_pat, temp_slot, reserved, line);
            self.emit_scope_pops(1, line);
        }
    }
}

/// Like `compile_pattern_bind`, but used only while compiling one
/// alternative of an `Or` pattern (see `compile_pattern_match`): every
/// name this alternative's pattern binds must write into a slot some
/// OTHER, unconditionally-run code has already reserved, because only
/// one alternative's bind ever actually executes at runtime — a plain
/// push-and-declare here would leave the slot's physical stack position
/// undefined whenever a DIFFERENT alternative is the one that matched.
/// `reserved` maps every name any alternative of this same `Or` binds to
/// its one shared physical slot.
fn compile_pattern_bind_into_reserved(
    &mut self,
    pat: Idx<ember_ast::Pattern>,
    scrutinee_slot: u32,
    reserved: &FxHashMap<Symbol, u32>,
    line: u32,
) {
    match self.ast.pat(pat).clone() {
        Pattern::Wild
        | Pattern::Error
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Bool(_)
        | Pattern::Str(_)
        | Pattern::Tuple(_) => {}
        Pattern::Bind(sym) => {
            let slot = reserved[&sym];
            self.emit_get_local(scrutinee_slot, line);
            self.emit_set_local(slot, line);
            self.current().emit_op(Op::Pop, line);
        }
        Pattern::Ctor { args, .. } => {
            for (i, &arg_pat) in args.iter().enumerate() {
                self.compile_destructured_bind_into_reserved(
                    arg_pat,
                    scrutinee_slot,
                    DestructureSource::Positional(i as u8),
                    reserved,
                    line,
                );
            }
        }
        Pattern::Record { fields, .. } => {
            for (fname, fpat) in fields {
                self.compile_destructured_bind_into_reserved(
                    fpat,
                    scrutinee_slot,
                    DestructureSource::Named(fname),
                    reserved,
                    line,
                );
            }
        }
        Pattern::List { items, rest } => {
            for (i, &item_pat) in items.iter().enumerate() {
                self.compile_destructured_bind_into_reserved(
                    item_pat,
                    scrutinee_slot,
                    DestructureSource::Indexed(i as i64),
                    reserved,
                    line,
                );
            }
            if let Some(rest_pat) = rest {
                if let Pattern::Bind(sym) = self.ast.pat(rest_pat) {
                    let sym = *sym;
                    let slot = reserved[&sym];
                    // Known gap (matches compile_pattern_bind's own): a
                    // Nil placeholder, not the real remaining sublist.
                    self.current().emit_op(Op::Nil, line);
                    self.emit_set_local(slot, line);
                    self.current().emit_op(Op::Pop, line);
                }
            }
        }
        Pattern::Or(_) => {
            unreachable!("nested Or inside an Or alternative is not produced by this grammar")
        }
    }
}
```

- [ ] **Step 6: Rewrite `compile_pattern_match`'s `Or` handling to pre-reserve, bind into reserved slots, and clean up on total failure**

Find the existing `compile_pattern_match` function:

```rust
    fn compile_pattern_match(
        &mut self,
        pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        fail_jumps: &mut Vec<usize>,
        line: u32,
    ) {
        if let Pattern::Or(alts) = self.ast.pat(pat).clone() {
            let mut end_jumps = Vec::new();
            let alts_entry_depth = self.current().stack_depth;
            for &alt in &alts {
                self.current().stack_depth = alts_entry_depth;
                self.compile_pattern_test(alt, scrutinee_slot, line);
                let this_fails = self.current().emit_jump(Op::JumpIfFalse, line);
                self.compile_pattern_bind(alt, scrutinee_slot, line);
                end_jumps.push(self.current().emit_jump(Op::Jump, line));
                self.current().patch_jump(this_fails);
                // falls through to the next alternative's test (or, after
                // the last alternative, straight into the line below) —
                // reached at exactly `alts_entry_depth`, the same real
                // depth every alternative's test starts from.
            }
            self.current().stack_depth = alts_entry_depth;
            fail_jumps.push(self.current().emit_jump(Op::Jump, line));
            for j in end_jumps {
                self.current().patch_jump(j);
            }
            return;
        }
        self.compile_pattern_test(pat, scrutinee_slot, line);
        fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
        self.compile_pattern_bind(pat, scrutinee_slot, line);
    }
```

Replace the `if let Pattern::Or(alts) = ...` block with:

```rust
        if let Pattern::Or(alts) = self.ast.pat(pat).clone() {
            let mut end_jumps = Vec::new();

            // Every alternative could bind the same name (that's the
            // whole point of `Circle(r) | Square(r) => r`) — but only
            // one alternative's bind ever executes at runtime, since
            // they're mutually exclusive branches reached only via the
            // previous alternative's failed-test jump. Reserving each
            // distinct bound name's slot UNCONDITIONALLY here, before any
            // alternative's test runs, guarantees the slot physically
            // exists on the stack regardless of which alternative later
            // matches — a plain push-and-declare inside one alternative's
            // own (conditionally executed) branch would leave the slot's
            // physical position undefined whenever a DIFFERENT
            // alternative is the one that actually matched. Every
            // alternative then writes into these reserved slots via
            // `compile_pattern_bind_into_reserved` instead of the normal
            // `compile_pattern_bind`.
            let mut names = Vec::new();
            for &alt in &alts {
                self.collect_pattern_bind_names(alt, &mut names);
            }
            let mut reserved: FxHashMap<Symbol, u32> = FxHashMap::default();
            for name in names {
                if !reserved.contains_key(&name) {
                    self.current().emit_op(Op::Nil, line);
                    self.declare_named_local(name, line);
                    let slot = self.current().local_count - 1;
                    reserved.insert(name, slot);
                }
            }

            // Captured AFTER pre-reservation, not before: the
            // reservation's Nil-pushes are real, unconditional stack
            // growth that every alternative's test is reached "on top
            // of" — resetting to a depth from BEFORE reservation would
            // undercount by one per distinct reserved name.
            let alts_entry_depth = self.current().stack_depth;
            for &alt in &alts {
                self.current().stack_depth = alts_entry_depth;
                self.compile_pattern_test(alt, scrutinee_slot, line);
                let this_fails = self.current().emit_jump(Op::JumpIfFalse, line);
                self.compile_pattern_bind_into_reserved(alt, scrutinee_slot, &reserved, line);
                end_jumps.push(self.current().emit_jump(Op::Jump, line));
                self.current().patch_jump(this_fails);
                // falls through to the next alternative's test (or, after
                // the last alternative, straight into the line below) —
                // reached at exactly `alts_entry_depth`, the same real
                // depth every alternative's test starts from.
            }
            self.current().stack_depth = alts_entry_depth;
            // Every alternative's test failed. The `reserved.len()` slots
            // reserved above were pushed unconditionally before any test
            // ran — nothing bound them (no alternative matched), and
            // nothing else on this path will ever pop them (the
            // success-path body/scope-exit code below is never reached
            // via this jump), so they must be cleaned up explicitly here.
            // Raw `Op::Pop`, not `emit_scope_pops`: this must NOT touch
            // `local_count`/`declared_slots`, which the success-path body
            // compiled immediately after this function returns still
            // needs to correctly describe.
            for _ in 0..reserved.len() {
                self.current().emit_op(Op::Pop, line);
            }
            fail_jumps.push(self.current().emit_jump(Op::Jump, line));
            for j in end_jumps {
                self.current().patch_jump(j);
            }
            return;
        }
```

Leave the non-`Or` fallback path (`self.compile_pattern_test(pat, ...)` etc.) at the bottom of the function completely unchanged.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ember-compile an_or_pattern_binding_the_same_name_writes_both_alternatives_into_the_same_slot an_or_pattern_with_a_binding_that_does_not_match_pops_its_reserved_slot`
Expected: PASS, both.

Run: `cargo test -p ember-compile`
Expected: full crate suite passes, including the pre-existing `an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping` and `an_or_pattern_tries_each_alternative_without_panicking` tests.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-compile/src/compiler.rs
git commit -m "Compiler: pre-reserve Or-pattern bound-name slots, bind into them, clean up on total failure"
```

---

### Task 3: Compiler fix — pop leaked locals when a bound pattern's guard fails

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

This is a separate bug from Task 2's — same root cause (a `fail_jumps` path that bypasses `emit_tail_scope_exit`, leaking whatever was really pushed), different trigger (a guard failing after ANY pattern, `Or` or not, has already bound a name), found while investigating Task 2's bug. It lives in `compile_match`, not `compile_pattern_match`.

- [ ] **Step 1: Write the failing test first**

Add to the `mod tests` block in `crates/ember-compile/src/compiler.rs`:

```rust
#[test]
fn a_guards_failure_after_a_bound_pattern_pops_the_bound_local() {
    // Confirmed via the VM (see this task's own design doc): before this
    // fix, `Circle(r) if r > 100 => r, _ => -1` matched via `Circle(5)`
    // (guard false) returns `5`, not `-1` — the bound-but-then-abandoned
    // `r` leaks onto the stack and becomes whatever the next arm's
    // `Return` picks up. This is a compile-time-only check that the
    // expected cleanup `OP_POP` is present; the VM-level test in Task 4
    // is the real proof.
    let src = "type Shape = Circle(Int); fn _f(s) { match s { Circle(r) if r > 100 => r, _ => -1, } }";
    let (chunk, interner) = compile_program_chunk(src);
    let out = disassemble_recursively(&chunk, "test", &interner);
    assert!(
        out.contains("OP_POP"),
        "expected an OP_POP cleaning up `r` on the guard-failure path: {out}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails or passes vacuously**

Run: `cargo test -p ember-compile a_guards_failure_after_a_bound_pattern_pops_the_bound_local`
This assertion alone (`out.contains("OP_POP")`) may pass even pre-fix, since `emit_tail_scope_exit`'s own success-path cleanup also emits `OP_POP`. That's fine — this compile-time check is a weak signal by design (the real proof is Task 4's VM-level test); don't try to make it stronger by counting occurrences, since the exact count depends on unrelated bytecode shape. Proceed to the fix regardless of this step's outcome.

- [ ] **Step 3: Rewrite `compile_match`'s guard handling**

Find the existing `compile_match` function's arms loop:

```rust
        for arm in &arms {
            for j in prev_fail_jumps.drain(..) {
                self.current().patch_jump(j);
            }
            self.current().stack_depth = arms_entry_depth;
            let arm_entry_local_count = self.current().local_count;
            let mut fail_jumps = Vec::new();
            self.compile_pattern_match(arm.pat, scrutinee_slot, &mut fail_jumps, line);

            if let Some(guard) = arm.guard {
                self.compile_expr(guard);
                fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
            }

            self.compile_expr(arm.body);
            self.emit_tail_scope_exit(arm_entry_local_count, line);
            end_jumps.push(self.current().emit_jump(Op::Jump, line));

            prev_fail_jumps = fail_jumps;
        }
```

Replace with:

```rust
        for arm in &arms {
            for j in prev_fail_jumps.drain(..) {
                self.current().patch_jump(j);
            }
            self.current().stack_depth = arms_entry_depth;
            let arm_entry_local_count = self.current().local_count;
            let mut fail_jumps = Vec::new();
            self.compile_pattern_match(arm.pat, scrutinee_slot, &mut fail_jumps, line);

            // `bound_count` is captured HERE, not after
            // `emit_tail_scope_exit` runs below — that call
            // unconditionally resets `local_count` back down to
            // `arm_entry_local_count` as part of the compiler's own
            // linear code generation (regardless of which runtime path
            // is actually taken), so this is the only point where
            // `local_count` still reflects how many locals this arm's
            // pattern really bound.
            let guard_fail = arm.guard.map(|guard| {
                self.compile_expr(guard);
                let bound_count = self.current().local_count - arm_entry_local_count;
                let jump = self.current().emit_jump(Op::JumpIfFalse, line);
                (jump, bound_count)
            });

            self.compile_expr(arm.body);
            self.emit_tail_scope_exit(arm_entry_local_count, line);
            end_jumps.push(self.current().emit_jump(Op::Jump, line));

            if let Some((guard_fail_jump, bound_count)) = guard_fail {
                self.current().patch_jump(guard_fail_jump);
                // The pattern DID match (bound_count locals were really
                // pushed by its bind) but the guard failed — nothing else
                // on this path will ever pop them, since we're not taking
                // the body/scope-exit route just above (that code is only
                // reached by falling through from a successful guard, not
                // by jumping here). Raw Op::Pop, not `emit_scope_pops`,
                // for the same reason as `compile_pattern_match`'s own
                // matching fix in Task 2: must not touch `local_count`/
                // `declared_slots`, which the body code just above still
                // needs to have correctly described.
                for _ in 0..bound_count {
                    self.current().emit_op(Op::Pop, line);
                }
                fail_jumps.push(self.current().emit_jump(Op::Jump, line));
            }

            prev_fail_jumps = fail_jumps;
        }
```

- [ ] **Step 4: Run tests to verify no regressions**

Run: `cargo test -p ember-compile`
Expected: full crate suite passes, including the existing `guard_and_or_pattern_both_work`-style tests (search for tests with `guard` in the name) and `a_match_arms_captured_local_flowing_into_a_nested_closure_does_not_corrupt_stack_bookkeeping`.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile/src/compiler.rs
git commit -m "Compiler: pop leaked locals when a bound pattern's guard fails"
```

---

### Task 4: End-to-end VM regression tests

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/ember-vm/src/vm.rs`, near the other ADT/pattern-matching tests (e.g. `ctor_pattern_tests_the_tag_and_destructures_by_position`):

```rust
// Naming convention throughout this task's tests (already established
// elsewhere in this file — see `ember-vm`'s existing
// `nullary_adt_variant_constructs_via_match`/`struct_literal_construction_and_field_read`
// tests): `compile_and_run`'s own `assert!(resolve_diags.is_empty())` is
// strict, catching warnings too, and this resolver never marks a type
// name "used" from ANY reference, or a constructor "used" from PATTERN
// position specifically (only from being actually constructed as an
// expression) — so every variant that's only ever pattern-matched in a
// given test, and every type name, needs a `_` prefix to avoid an
// "unused" warning tripping that assertion. Check each test below
// against which constructors it actually CONSTRUCTS (via a real call
// like `f(Circle(5))`) vs. only pattern-matches.

#[test]
fn or_pattern_repeated_name_matches_correctly_via_the_first_alternative() {
    // The original reported reproduction: `Circle(r) | Square(r) => r`,
    // matched via Circle. Before the fix, this panicked with an
    // out-of-bounds VM stack read (Circle's bind wrote into a slot the
    // arm body never read, since the resolver's `lookup` returned
    // whichever alternative's `declare` ran LAST in source order).
    // `Square` is only ever pattern-matched here (never constructed), so
    // it needs the `_` prefix; `Circle` is genuinely constructed below.
    let src = "
        type _Shape = Circle(Int) | _Square(Int);
        fn area(s) {
            match s {
                Circle(r) | _Square(r) => r,
            }
        }
        area(Circle(5));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(5)));
}

#[test]
fn or_pattern_repeated_name_matches_correctly_via_the_second_alternative() {
    // Same pattern, matched via the OTHER alternative — must independently
    // confirm both branches write into the slot the arm body actually
    // reads. `Circle` is only ever pattern-matched here (never
    // constructed), so IT needs the `_` prefix this time — the reverse of
    // the previous test.
    let src = "
        type _Shape = _Circle(Int) | Square(Int);
        fn area(s) {
            match s {
                _Circle(r) | Square(r) => r,
            }
        }
        area(Square(7));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(7)));
}

#[test]
fn or_pattern_with_a_nested_binding_at_different_depths_per_alternative() {
    // `r` sits at a different structural nesting depth in each
    // alternative (direct child of Circle, nested one level inside
    // Square's own Pair payload) — proves the fix is name-keyed, not
    // positional, since a "reset local_count per alternative" fix
    // (simpler, but insufficient) would put these two `r`s in different
    // slots. `Circle` is only pattern-matched (never constructed); `Pair`
    // and `Square` both ARE constructed below.
    let src = "
        type _Pair = Pair(Int, Int);
        type _Shape = _Circle(Int) | Square(_Pair);
        fn f(s) {
            match s {
                _Circle(r) | Square(Pair(_, r)) => r,
            }
        }
        f(Square(Pair(1, 9)));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(9)));
}

#[test]
fn or_pattern_with_no_repeated_name_still_compiles_and_runs_correctly() {
    // The broader (non-repeated-name) half of the original bug:
    // `local_count` drifted once per alternative that bound ANY name, not
    // just once per repeated occurrence — this exercises TWO distinct
    // reserved slots (`_r`, `_s`) rather than one shared one.
    //
    // The body deliberately does NOT read either bound name: this
    // language doesn't check that every alternative of an `Or`-pattern
    // binds the same names (a documented, deferred usability nicety, not
    // a soundness issue — see `declare_pattern_bindings`'s own comment in
    // `ember-resolve`), so whichever name the NON-matching alternative
    // would have bound holds a meaningless placeholder value on any given
    // run — reading it wouldn't test anything well-defined. Both `_r` and
    // `_s` are underscore-prefixed since they're genuinely unused by
    // design here (and `_s`, not `s`, specifically to avoid shadowing the
    // outer parameter `s` with a same-named, differently-meant binding).
    // Instead, this test proves the SLOT ACCOUNTING itself is correct by
    // checking that a local declared AFTER the match (`extra`) still
    // reads back correctly — the real proof that no slot leaked into
    // subsequent code. `Square` is only pattern-matched here, so it needs
    // the `_` prefix; `Circle` is constructed below.
    let src = "
        type _Shape = Circle(Int) | _Square(Int);
        fn f(s) {
            let extra = 42;
            match s {
                Circle(_r) | _Square(_s) => 1,
            };
            extra
        }
        f(Circle(3));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn an_or_pattern_arm_that_does_not_match_leaves_the_stack_correct_for_the_next_arm() {
    // Proves Task 2's leaked-reservation fix: this Or-pattern's
    // alternatives never match `Triangle`, so control must fall through
    // to the wildcard arm with a clean stack. Before the fix, the
    // reserved (never-bound) slot for `r` was never popped on this path,
    // corrupting the function's return value. `Circle`/`Square` are only
    // ever pattern-matched (never constructed); `Triangle` is constructed
    // below.
    let src = "
        type _Shape = _Circle(Int) | _Square(Int) | Triangle(Int);
        fn f(s) {
            match s {
                _Circle(r) | _Square(r) => r,
                _ => -1,
            }
        }
        f(Triangle(9));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(-1)));
}

#[test]
fn a_guards_failure_after_a_non_or_bind_falls_through_with_a_clean_stack() {
    // Proves Task 3's fix: the ORIGINAL reproduction found while
    // investigating Task 2 — `r` is really bound (Circle matched), but
    // the guard is false, so control must fall through to the wildcard
    // arm with `r` popped, not leaked. `Circle` is constructed below, no
    // prefix needed.
    let src = "
        type _Shape = Circle(Int);
        fn f(s) {
            match s {
                Circle(r) if r > 100 => r,
                _ => -1,
            }
        }
        f(Circle(5));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(-1)));
}

#[test]
fn a_guards_failure_after_an_or_pattern_bind_falls_through_with_a_clean_stack() {
    // The combined case: Task 2's reserved-slot bind AND Task 3's
    // guard-failure cleanup must compose correctly. `Square` is only
    // ever pattern-matched here (never constructed), so it needs the `_`
    // prefix; `Circle` is constructed below.
    let src = "
        type _Shape = Circle(Int) | _Square(Int);
        fn f(s) {
            match s {
                Circle(r) | _Square(r) if r > 100 => r,
                _ => -1,
            }
        }
        f(Circle(5));
    ";
    let result = compile_and_run(src).unwrap();
    assert!(matches!(result, Value::Int(-1)));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ember-vm --lib or_pattern`
Expected: PASS, 5/5 (the five `or_pattern_*`-named tests).

Run: `cargo test -p ember-vm --lib guard`
Expected: PASS, including the two new `a_guards_failure_*` tests and the pre-existing `guard_and_or_pattern_both_work` (which was passing by coincidence before this fix — confirm it still passes now, for the right reason).

Run: `cargo test -p ember-vm --lib`
Expected: full crate suite passes (the original ~55 tests plus these 7 new ones).

If any new test fails, the fix from Tasks 1-3 is incomplete — do not weaken these tests to make them pass; investigate and fix the underlying compiler/resolver logic. Re-read the design doc's hand-traced bytecode walkthroughs if you need to re-derive where a specific case should land.

- [ ] **Step 3: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Add end-to-end regression tests for the Or-pattern and guard-failure slot-leak fixes"
```

---

### Task 5: Full workspace verification and `CHECKLIST.md` reconciliation

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Full workspace verification**

Run: `cargo test --workspace`
Expected: PASS, 0 failures.

Run: `cargo test -p ember-cli --features gc-stress --test conformance` (if the `gc-stress` feature from prior work is present)
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 2: Update `CHECKLIST.md`'s existing flagged note**

Find the existing note (in the Phase 8 section, added during Phase 8's own reconciliation):

> **`Or`-pattern alternatives that bind the same name to different resolver slots** — ... Flagged for whoever builds the VM and starts real conformance cross-checking — a `Match` arm with a repeated-name `Or`-pattern is exactly the kind of program that would silently misbehave.

Replace it with (keep it in the same location in the document):

```markdown
- **`Or`-pattern alternatives binding a name, and guard-failure leaking a bound local — both fixed.** Confirmed via the VM (once one existed): the `Or`-pattern bug was broader than originally suspected here — not just repeated names across alternatives, but *any* `Or`-pattern where at least one alternative binds a name, since the compiler's `compile_pattern_match` never reset `local_count` between alternatives even though only one alternative's bind ever executes at runtime. Reproduced as a genuine out-of-bounds VM stack panic, not just a wrong value. Fixed by having the resolver declare each distinct bound name exactly once and having the compiler pre-reserve one physical slot per distinct name before any alternative's test runs, with every alternative writing into that reserved slot via a new parallel bind-compilation path. A second, independently-discovered, unrelated-to-`Or` bug was found and fixed alongside it: any guarded pattern that binds a name, whose guard can fail with another arm following, leaked the bound value onto the stack — confirmed to silently corrupt the *existing, previously-passing* `guard_and_or_pattern_both_work` test, which only happened to pass because the leaked value numerically equalled the correct answer. Both fixes share one root cause (a `fail_jumps` path bypassing `emit_tail_scope_exit`'s cleanup) and one technique (raw `Op::Pop` on the failure path, without touching the compiler's ongoing `local_count`/`declared_slots` bookkeeping, which the success path compiled immediately afterward still needs intact). See `docs/superpowers/specs/2026-08-09-or-pattern-shared-slot-fix-design.md`.
```

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Reconcile the Or-pattern and guard-leak slot fixes against CHECKLIST.md"
```
