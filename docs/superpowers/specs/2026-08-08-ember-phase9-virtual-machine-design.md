# Phase 9 — Virtual Machine: Design

**Goal:** Implement `ember`'s bytecode virtual machine per `SPEC.md §11`/`§12` and `CHECKLIST.md`'s Phase 9 — a `Vm` that actually executes the `Chunk`/`Op` stream `ember-compile` produces, closing the loop the conformance suite (started in Phase 8) exists to check: every program must produce identical output on both backends.

**Architecture:** One crate, `ember-vm`, depending on `ember-bytecode` (for `Chunk`/`Op`/`FunctionProto`) and `ember-ast` (for `Symbol`/`Interner`). A single retroactive change to `ember-bytecode` (below) is needed first. No garbage collector exists until Phase 10, so every heap-allocated runtime value is `Rc`/`RefCell`-based, mirroring `ember-tree`'s own approach — this phase is entirely about correct execution semantics, not memory management.

**Tech Stack:** Rust, `FxHashMap` for globals, no `unsafe`.

---

## Retroactive fix to `ember-bytecode`: `Chunk.functions` becomes `Vec<Rc<FunctionProto>>`

Currently `Chunk { functions: Vec<FunctionProto>, .. }` owns every nested function inline. That's fine for compiling and disassembling, but the VM needs to construct a `Value::Closure` that can *outlive* the function that created it (a closure returned from a function, stored in a list, etc.) — and there's no way to hand out an independently-owned handle to a `FunctionProto` sitting inside someone else's `Vec` without either cloning the whole nested chunk (wasteful, and wrong for shared-upvalue semantics if compilation ever produces genuinely shared protos) or restructuring.

The fix: `pub functions: Vec<Rc<FunctionProto>>`. `Chunk::add_function` wraps the passed `FunctionProto` in `Rc::new` before pushing — its signature and every call site in `ember-compile` (which only ever uses the returned `u16` index, never reads the proto back) are unaffected. `ember-bytecode`'s disassembler (`chunk.functions[idx]`) keeps working unchanged via auto-deref. This is a small, mechanical, low-risk change to a crate with no consumers yet outside its own tests and `ember-compile` — exactly the kind of adjustment a later phase revealing a real need is expected to make to an earlier one (Phase 6 similarly added parenthesized-tuple-pattern parsing that Phase 5 needed but the parser didn't yet support).

---

## Runtime `Value`

```rust
#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<ClosureObj>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    Record { name: Symbol, fields: Rc<RefCell<FxHashMap<Symbol, Value>>> },
}

pub struct ClosureObj {
    pub proto: Rc<FunctionProto>,
    pub upvalues: Vec<Rc<RefCell<Upvalue>>>,
}

pub struct AdtValue {
    pub type_name: Symbol,
    pub variant: Symbol,
    pub fields: Vec<Value>,
}

pub struct NativeFn {
    pub name: &'static str,
    pub func: fn(&[Value], &Interner) -> Result<Value, RuntimeError>,
}
```

Deliberately structurally identical to `ember-tree::Value` (same variant shapes, same `Rc<RefCell<..>>` pattern for mutable shared state) — this is what makes the conformance cross-check meaningful: both backends model the same runtime semantics, just reached by different execution strategies. `display_value`-equivalent formatting is reimplemented here matching `ember-tree::display_value`'s exact output shape, since conformance comparison is string-based.

## Upvalues

```rust
pub enum Upvalue {
    Open(usize),   // index into Vm.stack — the variable is still live there
    Closed(Value), // hoisted to the heap — the stack slot is gone
}
```

`Vm.open_upvalues: Vec<Rc<RefCell<Upvalue>>>`, kept sorted by slot descending (matching `SPEC.md`'s own choice, for a fast early-exit scan — though a `Vec` replaces the spec's intrusive linked list, since that's the natural safe-Rust shape and the search/reuse/close semantics are identical either way).

- `capture_upvalue(&mut self, slot: usize) -> Rc<RefCell<Upvalue>>`: scans `open_upvalues` for an existing entry at `slot`; if found, returns `Rc::clone` of it (so two closures capturing the same variable share one cell — required by CHECKLIST's own test); otherwise creates `Rc::new(RefCell::new(Upvalue::Open(slot)))`, inserts it in sorted position, returns it.
- `close_upvalues(&mut self, from: usize)`: for every entry in `open_upvalues` with `Open(slot) if slot >= from`, replaces its contents with `Closed(self.stack[slot].clone())` and removes it from the open list. Called from `OP_RETURN` **before** truncating the stack — get this order wrong and a closure ends up holding a stale/dangling slot index, per `SPEC.md`'s own explicit warning.

## `CallFrame` and the dispatch loop

```rust
pub struct CallFrame {
    pub closure: Rc<ClosureObj>,
    pub ip: usize,
    pub slot_base: usize,
}

pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    globals: FxHashMap<Symbol, Value>,
    open_upvalues: Vec<Rc<RefCell<Upvalue>>>,
}
```

The loop is a `while` over `self.read_op()` — genuinely iterative, not recursive, unlike the tree-walker's `eval_expr`/`exec_stmt`. This means the native Rust call stack is never at risk from a deep `ember` call chain; the frame-depth cap (`frames.len()` capped around 1000) exists purely to turn runaway/infinite recursion into a diagnostic rather than unbounded heap growth from an ever-growing `frames`/`stack`, not to prevent a native stack overflow the way `ember-tree::MAX_CALL_DEPTH` had to.

`OP_GET_LOCAL`/`OP_SET_LOCAL` index directly into `self.stack[frame.slot_base + slot]` — the whole performance story versus the tree-walker's hash-map-in-a-chain `Env`.

`OP_CALL`: pops `argc` args plus the callee off the stack, checks the callee is `Value::Closure` (arity-checked against `proto.arity`) or `Value::Native` (called immediately, no frame pushed), pushes a new `CallFrame` with `slot_base = stack.len() - argc` (so the already-pushed args become the new frame's parameter slots 0..argc, with no extra copying).

`OP_RETURN`: pops the return value, calls `close_upvalues(frame.slot_base)`, pops the frame; if `frames` is now empty, that popped value **is** the program's result (returned to the caller of `run`/`step`); otherwise truncates `stack` to `frame.slot_base` and pushes the return value, exactly restoring the caller's stack shape plus one new value — matching `ember-compile`'s own compile-time stack-depth accounting for `OP_CALL`.

`OP_CLOSURE`: reads the `FunctionProto` at the given constant-pool-adjacent function index (`Rc<FunctionProto>`, per the retroactive fix above — a cheap clone), then for each of `proto.upvalues`' `UpvalueDesc { index, is_local }` entries: `is_local: true` calls `capture_upvalue(frame.slot_base + index)` (capturing the *enclosing* frame's local); `is_local: false` clones `frame.closure.upvalues[index]` directly (chaining through an already-captured upvalue from the enclosing closure). Builds a `ClosureObj`, pushes `Value::Closure(Rc::new(..))`.

## Natives

The same 8 functions as `ember-tree::natives` (`print`/`len`/`push`/`clock`/`str`/`int`/`float`/`type_of`), reimplemented against `ember_vm::Value` — can't share code directly across the two `Value` types (the same situation `ember-bytecode::Value` is already in relative to `ember-tree::Value`). Unlike the tree-walker's dynamic fallback-on-lookup inside `eval_var`, `Vm::new()` pre-seeds `globals` with all 8 (interning each name and inserting a `Value::Native`), since `OP_GET_GLOBAL` is a real, unconditional hash lookup with no fallback path — this mirrors exactly how `ember-resolve::seed_native_globals` and `ember-compile`'s top-level dual registration already treat natives as pre-existing globals.

## Errors and stack traces

A VM-local `RuntimeError { message: String, trace: Vec<TraceFrame> }` (`TraceFrame { function_name: Symbol, line: u32 }`), built by walking `frames` top-down at the point of failure (function name from `proto.name`, line from `proto.chunk.line_at(frame.ip)`). Converts to `ember_diag::Diagnostic` via a `to_diagnostic` method mirroring `ember-tree::RuntimeError`'s own — the primary label on the innermost frame's line, secondary labels for each enclosing frame, matching the tree-walker's established diagnostic shape so both backends' runtime errors *look* the same to a user, even though the underlying failure detection differs (VM: an opcode's own type check; tree-walker: `eval_binary`/`apply_binary`'s match arms).

Arithmetic (`Add`/`Sub`/`Mul`/`Div`/`Mod`/unary `Negate`) and comparisons (`Greater`/`Less`) type-check their operands per-op, exactly mirroring `ember-tree::interp::apply_binary`'s own `(op, l, r)` match — including checked-arithmetic overflow/div-by-zero handling, so both backends reject the same programs the same way. `Equal` handles cross-type comparison by structural equality (mirroring `ember-tree::values_equal`), never a type error.

## Step mode

`Vm::step(&mut self) -> StepResult` executes exactly one dispatch-loop iteration (one opcode) and returns whether the program is still running, has finished (with the final value), or hit a `RuntimeError`. `Vm::run(&mut self) -> Result<Value, RuntimeError>` is a thin loop calling `step` until it's no longer `Running` — non-invasive in the same spirit as Phase 7's step-mode wrapper (the bulk of the dispatch logic lives in one place; `run` doesn't duplicate it).

## CLI and conformance integration

A new `vm` subcommand on `ember-cli`, running the full pipeline (parse → resolve → infer → exhaustiveness → **compile → `Vm::run`**) and printing output in the same format as the existing `run` subcommand (tree-walker), for manual side-by-side comparison.

`crates/ember-cli/tests/conformance.rs` (Phase 8) gains a second assertion per fixture: compile it and run it through `Vm::run`, asserting that output matches the tree-walker's actual output for that same fixture (not just each backend independently matching the `.expected` file) — this is CHECKLIST's own closing test for the phase and the first time the two-backend conformance promise is actually checked end to end.

## Tests

Arithmetic/comparison/logic (mirroring the op-level type-check behavior); function calls, recursion, and correct return values; a closure-based counter incrementing correctly across repeated calls; an upvalue closed at scope exit with its value surviving after the enclosing frame is gone; shared capture (two closures over the same variable observe each other's mutations); stack overflow (runaway recursion) produces a `RuntimeError`/diagnostic with a trace, not a native crash or hang; and the full conformance cross-check — every fixture in `tests/conformance/` produces byte-identical output on both backends.

## Non-goals

- NaN boxing (🔵) and computed-goto-style dispatch (🔵) — both explicitly deferred, no measured performance need yet.
- The real garbage collector (Phase 10) — `ember_vm::Value` stays `Rc`/`RefCell`-based until then; Phase 10 does a mechanical swap, not a redesign.
- Any new opcodes or compiler changes beyond the one `Chunk.functions` fix above — this phase executes what Phase 8 already emits.
