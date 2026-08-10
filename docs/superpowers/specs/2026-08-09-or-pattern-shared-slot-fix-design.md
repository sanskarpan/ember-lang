# Or-pattern shared-name binding — bug fix design

## Background

`CHECKLIST.md` carried a standing flagged gap since Phase 4/8: for a match arm like `Circle(r) | Square(r) => r`, the resolver allocates a separate slot per occurrence of `r` across the two alternatives, and the note speculated this could cause the arm body to read the wrong slot at runtime — but it was explicitly unverifiable until a VM existed to actually execute compiled bytecode against.

Phase 9/10 built that VM and the cross-backend conformance suite. This fix was scoped and verified now that it's checkable.

## Confirmed bug (broader than the original note)

A direct reproduction (`Circle(r) | Square(r) => r`, matched via `Circle(5)`) panics with an **out-of-bounds VM stack read**, not just a wrong value. Root-caused to two independent, compounding issues:

1. **Resolver** (`ember-resolve::resolver::declare_pattern_bindings`, `Pattern::Or` arm): calls `declare()` once *per occurrence* of a name across alternatives, not once per *distinct* name. Each call advances `next_slot`, but `pop_scope` only releases slots by counting distinct `HashMap` keys — so every repeated occurrence permanently leaks one slot, desyncing every local declared afterward in that scope. Separately, `lookup()` (used to resolve the arm body's read of `r`) returns whichever alternative's `declare()` call happened *last* in source order — not "whichever alternative actually matched," which is impossible to know at resolve time.

2. **Compiler** (`ember-compile::compiler::compile_pattern_match`/`compile_pattern_bind`): resets `stack_depth` between alternatives (already handled, documented) but never resets `local_count`. Since `compile_pattern_match`'s loop compiles *every* alternative's bind code (even though only one ever executes at runtime — they're mutually exclusive branches reached only via the previous alternative's failed-test jump), `local_count` drifts upward once per alternative that binds a name, not once per distinct name. **This is not limited to repeated names** — even `Circle(r) | Square(s)` (different names, no repetition) hits the same drift, since each alternative's `compile_pattern_bind` call unconditionally increments `local_count` regardless of whether its own branch is the one that runs.

## Fix

The only generally-correct design — sound even when alternatives have *different* nesting shapes on the path to a bound name (e.g. `Circle(r) | Square(Pair(x, r))`, where `r` sits at a different structural depth in each alternative) — is pre-reservation:

1. **Resolver**: `Pattern::Or`'s handling collects every name bound across *all* alternatives (via a new read-only recursive collector mirroring `declare_pattern_bindings`'s existing traversal shape), then declares each **distinct** name exactly once. This is the one place `next_slot` advances for the whole `Or`-pattern, matching what the compiler must mirror.

2. **Compiler**: before compiling any alternative's test, `compile_pattern_match`'s `Or` handling collects the same distinct-name set (mirroring the resolver's traversal) and unconditionally emits `Op::Nil` + `declare_named_local` once per distinct name — reserving one real physical stack slot per name, executed regardless of which alternative later matches, so the slot genuinely exists by the time any alternative's bind code runs. Each name's reserved slot is recorded in a map.

3. A **new, parallel bind-compilation path** — `compile_pattern_bind_into_reserved` and a matching `compile_destructured_bind_into_reserved` — used *only* while compiling an `Or` alternative's bind. Instead of push-and-declare-a-new-local (the existing, unchanged behavior for every non-`Or` pattern), every bound name writes into its already-reserved slot via `GetLocal` (scrutinee/sub-value) + `SetLocal` (reserved slot) + `Pop` (discard `SetLocal`'s duplicate). Any intermediate anonymous temp needed for nested destructuring (e.g. unpacking `Pair` before reaching `r`) is self-cleaning: pushed, used to compute the next destructured value, then popped again before the recursive call returns — so each alternative's own bind code is net-zero on `local_count`. The pre-reservation step is the *only* thing that permanently grows `local_count`, by exactly the distinct-name count — matching the resolver exactly.

The existing `compile_pattern_bind`/`compile_destructured_bind` — used for every non-`Or` pattern, the overwhelming majority of match arms in any real program — are **not modified**. This is additive: a new path used only for `Or`-alternative binds, isolated from already-tested, working code.

## A second, independently-discovered bug (same root cause class)

A subagent review of the initial version of this design (and this plan's implementation) found that the pre-reservation fix above, as first written, was itself unsound: it pushes `Op::Nil` unconditionally *before* any alternative's test runs, but nothing popped those slots on the "every alternative's test failed, fall through to the next arm" path — `fail_jumps` jumps straight past both the success-path body *and* its `emit_tail_scope_exit` cleanup. Hand-tracing confirmed this leaks the reserved `Nil`s onto the stack permanently, corrupting whatever the next arm (or the function's `Return`) reads from the top of the stack.

Chasing that trace surfaced a **second, pre-existing bug with the same root cause, unrelated to `Or`-patterns entirely**: `compile_match`'s own guard-handling has the identical problem. Any pattern (Or or not) that binds a name, guarded by a condition that can fail with another arm following, leaks the bound value the same way — `fail_jumps` (populated by both the pattern-test-failure jump *and* the guard's own `JumpIfFalse`) jumps past the scope-exit cleanup regardless of which one fired, but only the guard-failure case has already-bound locals sitting on the stack when it fires. Confirmed via direct reproduction:

```
match s { Circle(r) if r > 100 => r, _ => -1 }   // s = Circle(5), guard is false
```

returns `5`, not `-1`. Tracing further showed the *existing, currently-passing* `guard_and_or_pattern_both_work` VM test is subject to this exact bug too — it only passes because the leaked value happens to numerically equal the correct answer (`2` either way), not because the underlying bytecode is correct. This was scoped into the same fix (user-approved) rather than deferred, since it's the same architectural gap, found while already deep in this exact code path.

### Fix, part 2: pop leaked locals on every failure path, without disturbing the success path's own bookkeeping

Both leaks are fixed the same way: emit *raw* `Op::Pop` (never `emit_scope_pops`, which also mutates `local_count`/`declared_slots`) for however many locals were really pushed, right before falling through on the failure path in question. The raw-Pop approach is essential — `local_count`/`declared_slots` must stay exactly as pre-reservation/pattern-bind left them for the success-path body code that's compiled immediately afterward in the same linear stream (which needs to correctly reference those same slots); only the *failure* branch's own bytecode needs the extra cleanup, and it doesn't touch the ongoing compile-time counters at all.

- `compile_pattern_match`'s `Or` handling: after the alternatives loop, if every alternative's test failed, emit `reserved.len()` raw pops before the "jump to next arm" instruction.
- `compile_match`'s guard handling: capture `bound_count = local_count - arm_entry_local_count` at the point the guard's `JumpIfFalse` is emitted (before `emit_tail_scope_exit` resets `local_count`), then on the guard-failure path (patched to land after the success-path body/scope-exit code, never reached by fallthrough from it), emit `bound_count` raw pops before jumping to `fail_jumps`' target.

Hand-traced against three cases (`Or`-pattern-not-matched-falls-through, guard-failure-after-a-non-Or-bind, and the combined case — an `Or`-pattern's alternative matches but its shared guard then fails) to confirm both fixes compose correctly and produce the exact expected stack state in each.

## Non-goals

- Cross-alternative name-set consistency checking (rejecting `Circle(r) | Square(s)` for binding different names, or requiring identical types) stays deferred, exactly as the resolver's existing comment already documents. That's a usability nicety; this fix is about soundness — an alternative that *did* bind a name must be readable correctly by the arm body, regardless of what other alternatives do or don't bind.
- No change to guard compilation, exhaustiveness checking, or any non-`Or` pattern kind.

## Testing strategy

- Resolver: unit tests confirming `next_slot`/scope bookkeeping is correct (no leak) after resolving an `Or`-pattern with a repeated name, and that both occurrences resolve to the same slot.
- Compiler: unit tests on the emitted bytecode shape for the pre-reservation + reserved-slot-write pattern, including a nested-destructuring case (`Circle(r) | Square(Pair(x, r))`-shaped) proving the two differently-positioned occurrences of `r` land in the same slot.
- VM (end-to-end, the real regression tests): the original reproduction (`Circle(r) | Square(r) => r`, matched via each alternative in turn), a nested-depth case proving the fix is name-keyed not positional, an `Or`-pattern-with-a-binding that *isn't* the matching arm (proving the pre-reservation leak fix), a guard-failure-after-a-bound-pattern case for both a non-`Or` and an `Or` pattern (proving the guard leak fix), plus the full existing conformance suite re-verified.
