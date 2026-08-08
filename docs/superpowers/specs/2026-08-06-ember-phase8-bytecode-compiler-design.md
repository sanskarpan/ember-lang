# Phase 8 — Bytecode & Compiler: Design

**Goal:** Implement `ember`'s bytecode representation and the single-pass AST-to-bytecode compiler, per `SPEC.md §11` and `CHECKLIST.md`'s Phase 8 — `Op`, `Chunk`, a disassembler, and a compiler that finally *uses* `ember-resolve`'s slot allocation (unused since Phase 4, since Phase 7's tree-walker deliberately did its own dynamic lookup). Execution-free: no VM exists until Phase 9, so this phase is entirely testable via disassembly.

**Architecture:** Two crates, matching `SPEC.md §17` exactly: `ember-bytecode` (`Op`, `Chunk`, `FunctionProto`, disassembler, a minimal constant-pool `Value`) and `ember-compile` (the compiler, depending on `ember-ast` and `ember-resolve`, deliberately not `ember-types` — opcodes are generic/untyped, checked at runtime by the VM).

**Tech Stack:** Rust, `#[repr(u8)]` opcodes, run-length-encoded line info, `rustc_hash::FxHashMap` where needed.

---

## `ember-bytecode`

### `Value` — a minimal, GC-free constant-pool type

`SPEC.md`'s full sketch (`Value::{Nil, Bool, Int, Float, Obj(Gc<Obj>)}`) needs a working GC, which is Phase 10's job. Only literal, immutable, poolable values ever belong in a constant pool — closures/lists/records are built at runtime via opcodes (`Closure`, `MakeList`, `MakeRecord`, `MakeAdt`), never pooled. So this phase defines:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
}
```

Phase 9/10 extend this (adding `Obj(Gc<Obj>)` and friends), they don't replace it — every constant this phase ever pools stays representable exactly as-is.

### `Op`

`SPEC.md`'s ~35-opcode list, adopted almost verbatim, with one deliberate omission: **no `Op::Match`**. The checklist's own compiler task list only calls for `TestVariant` + jump chains + `Destructure` to compile pattern matching; a composite "Match" opcode adds nothing on top of those three primitives.

```rust
#[repr(u8)]
pub enum Op {
    Constant, Nil, True, False, Pop,
    GetLocal, SetLocal, GetGlobal, SetGlobal, DefineGlobal,
    GetUpvalue, SetUpvalue, CloseUpvalue,
    GetField, SetField, GetIndex, SetIndex,
    Equal, Greater, Less, Add, Sub, Mul, Div, Mod, Not, Negate,
    Jump, JumpIfFalse, JumpIfTrue, Loop,
    Call, Closure, Return,
    MakeList, MakeRecord, MakeAdt,
    TestVariant, Destructure,
    Print,
}
```

### `Chunk` and `FunctionProto`

```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    /// Run-length encoded: one `u32` per byte doubles chunk size for
    /// nothing, and consecutive instructions almost always share a line.
    pub lines: Vec<(u32 /* line */, u32 /* run_len */)>,
}

/// One compiled function — the unit `OP_CLOSURE` instantiates at runtime.
/// `upvalues` is `ember_resolve::binding::UpvalueDesc` reused directly,
/// another direct Phase 4 payoff: the descriptor list `OP_CLOSURE` needs
/// (capture from the enclosing frame's locals vs. the enclosing closure's
/// own upvalues) was already computed by the resolver.
pub struct FunctionProto {
    pub chunk: Chunk,
    pub arity: usize,
    pub upvalues: Vec<ember_resolve::UpvalueDesc>,
    pub name: Symbol,
}
```

Constant pool deduplication: before pushing a new constant, check for an existing equal one (`Value: PartialEq`) and reuse its index.

### Disassembler

`disassemble_chunk`/`disassemble_instruction` — human-readable text with operand names resolved (a local's slot number, a jump's *target address* computed from its offset, a constant's actual value), not raw byte indices. This is the phase's primary testing mechanism.

---

## `ember-compile`

### The "shadow" scope tracker

The compiler doesn't *compute* local slots — the resolver already did (`Resolution::Local{slot}` is looked up directly from `Bindings.resolutions[var_idx]` and turned straight into `OP_GET_LOCAL <slot>`). But a `let` declaration needs **no explicit store opcode**: the initializer's evaluated value simply ends up sitting at the correct stack slot, *provided* the compiler walks the AST in the same order and with the same scoping the resolver used (which it does, by construction — same AST, same traversal). What the compiler *does* need to track itself: a running local count per function-in-progress, incremented on each declaration, with `OP_POP`s emitted at scope exit to release those slots — a drastically simplified mirror of `ember-resolve`'s `FunctionCtx::declare`/`pop_scope` (no upvalue capture logic, no diagnostics, no "did you mean" — all of that already happened and is sitting in `Bindings`).

### Function compilation

Each function-introducing node (`Stmt::Fn`, `Expr::Lambda`) compiles into its own `Chunk`/`FunctionProto`. The compiler keeps a stack of in-progress functions (mirroring the resolver's `functions: Vec<FunctionCtx>` stack), each with its own chunk, local counter, and loop-context stack for `break`/`continue` patching. `OP_CLOSURE` is emitted with the function's upvalue descriptor list (from `Bindings.upvalues[function_id]`) inline.

### Control flow

- `if/else` → `JumpIfFalse` + `Jump`; both arms leave exactly one value on the stack (an absent `else` pushes `Nil` in the false branch, matching the tree-walker's `Expr::If` semantics).
- `while` → condition, `JumpIfFalse` past the loop, body, `Loop` back to the condition.
- `&&`/`||` → short-circuit via jumps directly, never a function call.
- `break`/`continue` → forward/backward jumps recorded against a loop-context stack, patched once the loop's start/end addresses are known, so `break` inside a nested loop targets the *innermost* enclosing loop — verified directly by a dedicated test.
- **`for` loop desugaring**: the tree-walker (Phase 7) only ever iterates `Value::List` — there is no `Value::Range` and no range-literal evaluation anywhere in this pipeline. For future byte-identical output vs. the tree-walker (Phase 9's conformance requirement), `for x in xs { body }` desugars to an index-counter `while`: evaluate `xs` once into a hidden local, a hidden counter starting at 0, loop while the counter is less than the list's length, binding `x` to the indexed element each iteration and incrementing after. "Desugared to a while loop with a hidden counter local" read as counting over the *iterable's index range*, not literal range syntax (which doesn't exist in the language yet).

### Pattern compilation

Recursively mirrors Phase 7's `match_pattern` structure (the same `Pattern` variant walk), but emits opcodes instead of directly testing Rust values: `TestVariant` checks a runtime value's tag/name (for `Ctor`/`Record` patterns) with a conditional jump to the next arm on failure; `Destructure` extracts and binds sub-pattern values recursively (list rest-binding, record field-by-name, constructor payload-by-position). `Pattern::Tuple` still compiles to "never matches" — no `Value::Tuple` exists anywhere in this pipeline (Phase 5/6/7's shared, carried-over gap), not newly introduced or fixed here.

### Debug-build stack-balance assertions

Every `Op` has a known static stack effect (`Constant`: +1, `Pop`/`Add`/`Sub`/.../`Equal`: -1, `Return`: consumes the frame, etc.). The compiler tracks a running would-be stack depth as it emits, `debug_assert!`ing it returns to the expected value after compiling each statement — catches an entire class of codegen bugs (an emitted sequence that doesn't balance push/pop) immediately rather than as a mysterious runtime stack corruption two phases from now.

## Conformance suite (started, not completed, this phase)

The actual cross-backend comparison (tree-walker vs. bytecode+VM, byte-identical output) needs Phase 9's VM to exist — this phase can't run that check yet. What this phase does: establishes the `tests/conformance/` directory convention and adds a first batch of representative `.em` programs with their expected output, captured by running each through the already-working `ember-cli run` (tree-walker). Phase 9 extends this same directory to also run each program through compile+VM and assert identical output against what's captured here.

## Tests

Every one `CHECKLIST.md` names: disassembly snapshots for 15 programs, jump offsets correct for nested `if`/`while`, `break` inside a nested loop targets the right loop, `OP_CLOSE_UPVALUE` emitted exactly where a captured local dies. Plus, driven by this design's scope: constant-pool deduplication, and the debug-mode stack-balance assertion catching a deliberately-introduced imbalance.

## Non-goals (this phase)

- The VM itself (Phase 9) — nothing in this phase executes bytecode, only emits and disassembles it.
- The garbage collector (Phase 10) — `ember-bytecode::Value` stays primitive-only (`Nil`/`Bool`/`Int`/`Float`/`Str`) until then.
- Constant folding and peephole optimization (both 🟡) — deferred; no measured performance need yet, and this phase's job is correctness first.
- Fixing `Pattern::Tuple`'s underlying inertness — carried from Phase 5/6/7, still out of scope.
- The full conformance cross-check against the VM — infrastructure only this phase; the actual comparison is Phase 9's.
