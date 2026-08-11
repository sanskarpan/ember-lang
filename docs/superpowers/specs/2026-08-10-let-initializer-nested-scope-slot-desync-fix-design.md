# `let`-initializer nested-scope slot desync: bug fix design

## Discovery

Found while writing new Phase 12 conformance fixtures (a `shadowing.em` fixture using a block-expression `let` initializer with its own internal `let`s failed to resolve for an unrelated reason — a `let x = x + 1;`-style self-reference — and the workaround fixture surfaced this separate, more serious bug during manual both-backends verification).

## Root cause

The resolver (`crates/ember-resolve/src/scope.rs`'s `FunctionCtx::declare`) declares a `let`'s own name **before** resolving its initializer, specifically so a self-referential initializer (`let x = x + 1;`) can be rejected: the name is declared `initialized: false`, the initializer is resolved (where a same-scope lookup of the not-yet-initialized name is caught), then it's marked initialized.

The compiler (`crates/ember-compile/src/compiler.rs:608-610`, `Stmt::Let` arm) does the reverse: it compiles the initializer expression *then* declares the name's local (`compile_expr(init); declare_named_local(name, line);`). `declare_named_local` recovers the resolver's slot number for the local it's about to declare via `local_count - total_shift()` — a formula whose own doc comment says it's "valid because nothing hidden has been pushed since the resolver's own counter last matched this compiler's."

That precondition is false whenever `init` contains its own nested scope that declares locals (a block, an `if`/`else` branch, a match arm — anything compiled via `compile_block`). The resolver has already incremented its slot counter past the outer `let`'s own (not-yet-compiler-declared) slot before resolving anything inside `init`, so every local declared inside `init` gets a resolver slot number one higher than where the compiler's `local_count` (not yet bumped for the outer `let`) says it should physically be. Every `Expr::Var` read inside `init` uses the resolver's true slot number directly (translated only for active `for`-loop shifts via `physical_slot()`, confirmed by reading `compiler.rs:364,985,1073` — not recomputed by the compiler), so those reads now target the wrong physical stack position.

## Confirmed impact (empirical, both backends compared)

- `let y = { let a = 1; a }; y;` — VM panics: `index out of bounds` (`vm.rs:359`, `Op::GetLocal`). Tree-walker: `1` (correct).
- `let y = { let a = 1; let b = 2; a + b }; y;` — VM **silently prints `4`** instead of `3` — a wrong answer, not a crash, because the misaddressed read happens to land on another live stack slot instead of past the end of the stack. Tree-walker: `3` (correct).
- `let y = if true { let a = 10; a } else { 0 }; y;` — VM panics the same way (confirms `if`/`else` branches, not just bare blocks, trigger it — anything routed through `compile_block`).
- Reproduces identically whether the outer `let` is at top level or inside a function body.
- Plain (non-nested-scope) initializers are unaffected: `let y = 1 + 2; y;` works correctly on both backends.

This is a real, general miscompilation bug in the VM backend — not covered by the pre-Phase-12 conformance suite (all 6 original fixtures happen to avoid this exact shape) or by any existing unit test.

## Fix

Mirror the technique this project already uses for exactly this class of problem (the Or-pattern shared-slot fix's `compile_pattern_bind_into_reserved`): **reserve the physical slot before compiling the value that goes into it**, matching the resolver's own declare-before-resolve order, instead of declaring the slot after the fact and hoping the initializer happened to leave its result in the right place.

Concretely, for `Stmt::Let`:

1. Emit `Op::Nil` (a real placeholder push, physically reserving the slot).
2. Reserve the local's bookkeeping (`push_local`, recovering the resolver slot the same way `declare_named_local` already does) — but do **not** dual-register it yet, since the placeholder isn't the real value.
3. Compile `init` (now correctly sees `local_count` already bumped past the reserved slot, so any nested scope inside `init` gets resolver-matching slot numbers — this is the actual fix).
4. Emit `Op::SetLocal <reserved slot>` (copies `init`'s result, now sitting on top of the stack, down into the reserved slot — leaving a duplicate on top per `SetLocal`'s existing semantics) then `Op::Pop` (discards that duplicate) — the exact same "write into a pre-reserved slot, then pop the leftover duplicate" pattern `emit_tail_scope_exit` and `compile_for`'s binding-slot write already use elsewhere in this file.
5. Dual-register (`maybe_dual_register`) now that the reserved slot holds the real value.

This requires splitting `declare_named_local` into a `reserve_named_local` (steps 1-2's bookkeeping half, reusable) and keeping `maybe_dual_register` as a separate, explicit final step for `Stmt::Let` — `StructDecl`/`Fn` keep calling the combined `declare_named_local` unchanged, since neither of them compiles a sub-expression capable of containing nested scopes between reserving their name's slot and being ready to dual-register it.

Verified by hand that the stack-depth bookkeeping stays balanced (`Op::Nil` +1, `init`'s own net effect +1, `Op::SetLocal` is a fixed `Some(0)`-effect op per `static_stack_effect`, `Op::Pop` -1 — nets to the same `+1` `Stmt::Let` already asserts via `compile_stmt`'s `permanent_locals` check) and that this doesn't disturb `for`-loop slot-shift handling (`reserve_named_local` uses the identical `local_count - total_shift()` recovery `declare_named_local` already used).

## Known ripple: existing compiler unit tests encode the old (buggy-adjacent) bytecode shape

Several existing tests in `crates/ember-compile/src/compiler.rs`'s test module assert on the *exact* bytecode `let`-with-a-block-initializer used to produce, as a way of isolating the block's own scope-exit behavior from the `let`'s (`a_block_with_two_locals_and_a_tail_pops_both_and_keeps_the_tail`, `a_block_with_no_locals_emits_no_pops`) or asserting no `OP_POP` appears for a bare top-level `let` at all (`a_top_level_let_gets_dual_registration`). Under this fix, **every** `Stmt::Let` now unconditionally emits its own `OP_SET_LOCAL` + `OP_POP` pair (writing into its reserved slot), regardless of whether its initializer is itself a block. These tests' assertions were checking an old invariant that this fix intentionally changes; the implementation plan updates each to assert the new, correct shape (with a comment explaining why), not silently relaxes them to hide a regression. Confirming exactly which tests are affected, and how each assertion should change, happens during implementation (run the full suite, read every failure, update only the ones whose assertion encoded the old bytecode shape — leave everything else untouched).

## Non-goals

- Does not touch `Stmt::StructDecl`/`Stmt::Fn` (unaffected, per the analysis above).
- Does not revisit the resolver's declare-before-resolve design for `let` (that's correct and necessary for self-reference detection — the compiler was the one out of step).
- Does not attempt a broader audit of every other place `local_count`/`declared_slots` bookkeeping might assume compiler/resolver ordering matches; scoped strictly to `Stmt::Let`, the only place this specific pre-vs-post ordering mismatch exists (`StructDecl` and `Fn` both declare via `declare_named_local` immediately, with no intervening sub-compilation of a value expression that could itself open nested scopes).
