# Phase 10: Garbage Collector — Design

## Goal

Replace `ember-vm`'s Phase 9 `Rc`/`RefCell`-based `Value` heap representation with a real mark-and-sweep, tri-color tracing garbage collector, implemented in the (currently stub) `ember-gc` crate. This closes the "swap for `Gc<T>` in Phase 10" deferral made explicit in the Phase 9 design doc, and is the one part of the whole project that `Rc` fundamentally cannot solve correctly: `Rc` cannot collect reference cycles, and the checklist requires a test proving a cyclic structure is collected once unreachable.

## Non-goals

- NaN boxing (🔵, deferred — no measured performance need yet).
- Computed-goto-style dispatch (🔵, deferred, unrelated to this phase's actual scope but listed alongside it in the checklist).
- GC pause-duration stats (🟡 — tracked as collections/bytes-freed/live-objects only; wall-clock timing isn't meaningful without a timing dependency the project hasn't taken elsewhere).
- Any change to `ember-compile`, `ember-bytecode`, or `ember-resolve` — this phase is scoped entirely to `ember-gc` (new) and `ember-vm` (migrated), plus `ember-cli`'s conformance harness gaining a `gc-stress` variant.
- Concurrent or incremental collection — stop-the-world only, matching SPEC.md's design and the project's scope.

## Architecture

### `Gc<T>` — the handle

A `Copy` wrapper around `NonNull<GcBox<T>>`:

```rust
pub struct Gc<T> {
    ptr: NonNull<GcBox<T>>,
}

struct GcBox<T> {
    header: ObjHeader,
    data: T,
}
```

Embedding the header directly before the data in one allocation means a type-erased `NonNull<ObjHeader>` in the heap's intrusive list can walk every allocation regardless of `T`, while a typed `Gc<T>` still derefs straight to `&T`/`&mut T` with no extra indirection (`impl Deref for Gc<T>`, `impl DerefMut for Gc<T>`). `Gc<T>` derives `Copy`/`Clone` (a bitwise pointer copy) and does not implement `Drop` — freeing happens only during sweep, driven by the header's `marked` bit, not by handle lifetime. This is genuinely `unsafe` under the hood (raw pointer deref, manual allocation/free via `Box::into_raw`/`Box::from_raw`) — the project's first unsafe code, deliberately, per SPEC.md's own rationale for choosing Rust over a GC'd host language ("you must write it yourself — a host GC would hide the entire lesson").

### `ObjHeader` and `ObjKind`

```rust
pub struct ObjHeader {
    marked: bool,
    next: Option<NonNull<ObjHeader>>,
    kind: ObjKind,
}

enum ObjKind {
    Str,
    List,
    Closure,
    Adt,
    Record,
    Upvalue,
}
```

`ObjKind` exists **only** for the heap's own internal mark/sweep/free dispatch (an `unsafe` downcast from the type-erased `NonNull<ObjHeader>` back to the concrete `GcBox<T>`, keyed off the tag). It is never exposed in `ember-vm`'s `Value` enum — `Value` keeps its Phase 9 per-kind shape (`Value::Str(Gc<String>)`, `Value::List(Gc<RefCell<Vec<Value>>>)`, etc.) rather than unifying into a single `Value::Obj(Gc<Obj>)` the way SPEC.md's pseudocode sketches it. This is a deliberate deviation, consistent with Phase 9's own precedent of adapting SPEC.md pseudocode to the real implementation (e.g. `Rc<String>` instead of `Symbol` for runtime names): unifying into one `Obj` type would force every existing, already-tested `vm.rs` match arm on `Value::Str`/`List`/`Closure`/etc. into a second-level match on an internal kind tag, for no behavioral gain — Rust's own enum already gives the tagged-union benefit SPEC.md's C-flavored pseudocode is reaching for.

### `GcHeap`

```rust
pub struct GcHeap {
    objects: Option<NonNull<ObjHeader>>,   // intrusive linked list of every allocation
    gray_stack: Vec<NonNull<ObjHeader>>,   // tri-color worklist for the mark phase
    bytes_allocated: usize,
    next_gc: usize,                        // threshold; doubles after each collection
    strings: FxHashMap<String, NonNull<GcBox<String>>>,  // intern table, see below
    stats: GcStats,
}

pub struct GcStats {
    pub collections: usize,
    pub bytes_freed: usize,
    pub live_objects: usize,
}
```

`allocate<T>(&mut self, kind: ObjKind, data: T, vm: &Vm) -> Gc<T>`:
1. If `next_gc` has been exceeded, or the `gc-stress` feature is enabled, run a full collection first (passing `vm` so `mark_roots` can walk its stack/frames/globals/upvalues).
2. Box `GcBox { header: ObjHeader { marked: false, next: <old head>, kind }, data }`, leak it via `Box::into_raw`, link it in as the new list head.
3. Add `size_of::<GcBox<T>>()` to `bytes_allocated`, increment `stats.live_objects`.
4. Return the typed `Gc<T>` handle.

**Collection**, `collect(&mut self, vm: &Vm)`:
1. `mark_roots(vm)`: mark every `Value` on `vm.stack`; mark every `frame.closure` in `vm.frames`; walk `vm.open_upvalues` and mark each; mark every `Value` in `vm.globals`. Marking an object pushes it onto `gray_stack` if it wasn't already marked (tri-color: white = unmarked/unvisited, gray = marked-but-children-not-yet-traced (on the worklist), black = marked and fully traced).
2. Drain `gray_stack`: pop each gray object, call `blacken_object` to trace its children (marking them gray in turn, pushing onto the stack), and consider it black once its children are all pushed.
   - `blacken_object` dispatch per `ObjKind`: `List`/`Record` trace every `Value` they hold (recursively marking any that are themselves `Gc` handles); `Closure` traces each of its captured `upvalues` (not its `proto` — `FunctionProto` is plain `Rc` program data, never GC-owned, see below); `Upvalue::Closed(v)` traces `v`; `Str` has no children (leaf).
3. Sweep: walk the intrusive `objects` list; for each node, if unmarked, unlink it and free it via `Box::from_raw` (subtracting its size from `bytes_allocated`, decrementing `live_objects`, incrementing `stats.bytes_freed`), running any kind-specific cleanup (e.g. `Str` nodes also get pruned from the `strings` intern table at this point, since the table holds non-owning, non-marking raw pointers — see below); if marked, unmark it (reset to white for the next cycle) and keep it.
4. `next_gc = bytes_allocated * 2`. Increment `stats.collections`.

**`mark_compiler_roots` — architectural note, not a literal port.** clox needs this because its single-pass compiler interleaves bytecode emission with GC-heap allocation on the *same* heap the VM later runs on, so an in-progress function object under construction can be swept mid-compile if a collection is triggered by, e.g., a string constant allocation. Ember's architecture has no such interleaving: `ember-compile` (Phase 8) is a fully separate, already-completed, already-tested pass that produces plain `Chunk`/`FunctionProto` data using compile-time `Symbol`s from `ember-lex`'s `Interner` — it never touches `ember-gc` at all, and no `Vm`/`GcHeap` exists yet while it runs. This checklist item is therefore satisfied by this documented architectural note rather than a ported function; `CHECKLIST.md`'s Phase 10 section will record this the same way Phase 9 recorded its own deliberate deviations from SPEC.md pseudocode.

### String interning

`GcHeap.strings: FxHashMap<String, NonNull<GcBox<String>>>` — raw, non-owning pointers, not `Gc<String>` handles, and specifically **not marked** during `mark_roots`/`blacken_object`. `intern_str(&mut self, s: &str, vm: &Vm) -> Gc<String>`:
- If `s` is already a key, return a `Gc<String>` wrapping the stored pointer (no new allocation — this also gives structural string equality "for free" as pointer equality post-interning, matching clox's own `vm.strings` design).
- Otherwise `allocate(ObjKind::Str, s.to_string(), vm)`, insert the pointer into the table, return it.

Because the table's entries are non-owning and non-marking, an interned string with zero other live references (no `Value::Str` anywhere reachable) is collected exactly like any other unreachable object — sweep frees it and **also removes its `strings` table entry in the same pass** (the table is keyed by content, so sweep needs to know which content-string is being freed; this is why `ObjKind::Str`'s per-kind sweep cleanup does a table removal, not a separate GC pass). Every op that materializes a `Value::Str` (string literal load, `+` concatenation, `str()`/`type_of`-style native conversions) goes through `intern_str` instead of a bare `allocate`.

`values_equal`'s existing content-comparison fallback for `Value::Str` is unchanged — interning is a performance/collectability property, not a correctness dependency; two `Value::Str`s must compare equal by content regardless of whether interning happened to unify their handles.

## `ember-vm` migration

`Value`'s shape is unchanged from Phase 9 except every `Rc`/`Rc<RefCell<...>>` becomes `Gc`/`Gc<RefCell<...>>`:

```rust
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Gc<String>),
    List(Gc<RefCell<Vec<Value>>>),
    Closure(Gc<ClosureObj>),
    Native(Rc<NativeFn>),           // UNCHANGED — see below
    Adt(Gc<AdtValue>),
    Record {
        name: Gc<String>,
        fields: Gc<RefCell<FxHashMap<Gc<String>, Value>>>,
    },
}

pub struct ClosureObj {
    pub proto: Rc<FunctionProto>,   // UNCHANGED — see below
    pub upvalues: Vec<Gc<RefCell<Upvalue>>>,
}
```

Two fields deliberately **stay** `Rc`, not `Gc`:
- `ClosureObj.proto: Rc<FunctionProto>` — immutable, compile-time-owned program data (part of the `Chunk` the whole `Vm` run borrows), never cyclic, never needs collecting mid-run. Only genuinely runtime-allocated, potentially-cyclic data belongs on the GC heap.
- `Value::Native(Rc<NativeFn>)` — natives are constructed once in `Vm::new`, are effectively `'static` for the VM's lifetime, and never hold references to other `Value`s (no cycles, nothing to collect). Moving them onto the GC heap would add marking overhead for zero benefit.

`Vm` gains a `gc: GcHeap` field, filling in the placeholder Phase 9's own design doc named explicitly ("no `gc` field — there's no `GcHeap` until Phase 10, so nothing to hold a handle to yet"). Every existing `Rc::new(...)` / `Rc::new(RefCell::new(...))` call site in `vm.rs`/`natives.rs` becomes `self.gc.allocate(...)` (or `self.gc.intern_str(...)` for the string-producing ones). `RuntimeError`/`error.rs` needs no shape changes — it never holds `Value`s directly (`TraceFrame` carries `Symbol`/`u32`, not runtime values).

`mark_roots` is called from inside `GcHeap::collect`, which needs read access to `Vm`'s `stack`/`frames`/`globals`/`open_upvalues` — this requires either passing `&Vm` into `gc.allocate`/`collect`, or restructuring so `GcHeap` is collected via a method on `Vm` that has access to both `self.gc` and the rest of `self`'s fields simultaneously (an ordinary Rust split-borrow, no unsafe needed for that part). The implementation plan will pin down the exact call shape as its first task.

## Feature flags and stats

- `gc-stress` (Cargo feature on `ember-gc`): when enabled, `allocate` always runs a full collection before returning, regardless of `next_gc` — makes GC bugs deterministic instead of load-bearing on allocation timing. `ember-vm` gains a stress-mode dev-dependency path; `ember-cli`'s conformance test gains a `gc-stress`-enabled variant so "the entire conformance suite passes under `gc-stress`" is an automated, repeatable check.
- `gc-log` (Cargo feature on `ember-gc`): traces allocate/mark/sweep events (kind, size, pointer) to stderr behind `#[cfg(feature = "gc-log")]` — a debugging aid, no runtime cost when disabled.
- `GcStats` (collections, bytes_freed, live_objects) exposed via a getter on `GcHeap` — no pause-duration tracking (🟡, out of scope per Non-goals).

## Testing strategy

Mirrors Phase 9's own precedent of keeping lower layers unit-testable in isolation before the layer that consumes them exists to exercise them end-to-end (Phase 8 tested `ember-bytecode`/`ember-compile` via disassembly alone, before `ember-vm` could execute anything).

**`ember-gc` (new, isolated tests against small synthetic object graphs — no `Vm` involved):**
- Unreachable object is collected.
- Reachable object (rooted directly, standing in for a `Vm` root) survives 100 collections.
- A cyclic structure (two `List`s each holding a `Value` pointing back at the other, `Rc` could never collect this) is collected once nothing roots either side of the cycle.
- A closure keeps its captured upvalue alive across a collection (the upvalue is reachable only via the closure's `upvalues`, not directly rooted).

**`ember-vm` / `ember-cli` (existing 54-test suite plus conformance, re-verified against the new heap):**
- Every existing Phase 9 test continues to pass unchanged in behavior (only the underlying handle type changed).
- New: heap size stays bounded in a long-running allocation loop (🟡).
- The full conformance suite (`ember-cli`), re-run with `gc-stress` enabled — this is, per the checklist's own framing, "the real GC test": every conformance fixture must still produce identical, correct output when a collection happens after literally every single allocation.

## CHECKLIST.md reconciliation

As with every prior phase, `CHECKLIST.md`'s Phase 10 section will be reconciled item-by-item on completion, including a note on the `mark_compiler_roots` architectural deviation described above, and any other real deviations discovered during implementation (Phase 9 found five previously-unknown bugs in already-merged code this same end-to-end-execution testing style flushed out; Phase 10's `gc-stress` conformance run is exactly the kind of test likely to surface anything Phase 9 missed in root-tracking terms).
