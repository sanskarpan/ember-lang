# ember: Phase 4 — Resolver Design

Status: approved
Date: 2026-08-04
Scope: Phase 4 of `CHECKLIST.md` (22 tasks) — scope resolution, slot assignment, upvalue capture, mutability/initialization checks, "did you mean?" suggestions, and the 🟡 unused-binding / unreachable-code warnings. Builds on the completed Phase 0-3 work (`ember-span`, `ember-diag`, `ember-lexer`, `ember-ast`, `ember-parser`).

## Context

The resolver runs between parsing and type inference. It answers: for every `Var` node in the AST, which declaration does it refer to, and where does it live at runtime (a stack slot, a captured upvalue, or a global)? Its output — `Bindings` — is the data structure that Phase 5 (type inference), Phase 7 (tree-walking interpreter), and Phase 8-9 (bytecode compiler/VM) all consume: none of them exist yet, so this phase's job is producing a correct, well-typed `Bindings` structure and proving it correct via tests, not wiring it into a downstream consumer.

`ember-resolve`'s `Cargo.toml`/`src/lib.rs` currently exist as a stub crate (Phase 0). This design fills it in for real, and adds a dependency on `ember-ast` (and `ember-diag`, `ember-span`).

## Non-goals

- Type inference, exhaustiveness checking, both execution backends, GC, formatter, LSP, WASM, playground — all later phases.
- Full branch-level dataflow analysis for unreachable code (e.g. detecting that an `if`/`else` where both arms return makes code after the `if` unreachable). `CHECKLIST.md`'s ask is narrower: code immediately following a `return`/`break`/`continue` *within the same block* is unreachable — that's what's implemented.
- Consuming `Bindings` from an actual compiler/interpreter — that wiring happens when Phase 7/8 exist and can specify exactly what shape they need.

## Data model

```rust
// crates/ember-resolve/src/binding.rs
pub struct BindingInfo {
    pub slot: u32,
    pub mutable: bool,
    pub initialized: bool,
    pub span: Span,
    pub used: bool,
    pub captured: bool,   // true once any inner function captures this binding as an upvalue
}

pub enum Resolution {
    Local { slot: u32 },
    Upvalue { index: u32 },
    Global { symbol: Symbol },
}

pub struct UpvalueDesc {
    pub index: u32,      // slot in the enclosing function's frame, or upvalue index in the enclosing function's own upvalue list
    pub is_local: bool,  // true: capture from the immediately enclosing function's locals. false: capture from that function's own upvalues.
}
```

`FunctionId` — the fix for the reference sketch's `Idx<Expr>`-only keying (see Context above): both `Expr::Lambda` and `Stmt::Fn` introduce a function scope, since `fn` can appear anywhere a statement can (including nested inside another function's body), not just at top level.

```rust
// crates/ember-resolve/src/lib.rs
pub enum FunctionId {
    TopLevel,
    Lambda(Idx<Expr>),
    Fn(Idx<Stmt>),
}

pub struct Bindings {
    pub resolutions: FxHashMap<Idx<Expr>, Resolution>,        // one entry per Var node
    pub upvalues: FxHashMap<FunctionId, Vec<UpvalueDesc>>,
    pub frame_sizes: FxHashMap<FunctionId, u32>,               // high-water mark of live locals
    pub captured_slots: FxHashMap<FunctionId, Vec<u32>>,       // which slots need OP_CLOSE_UPVALUE at scope exit (Phase 8 consumes this)
}
```

## Resolver internals

```rust
struct Scope {
    bindings: FxHashMap<Symbol, BindingInfo>,
    kind: ScopeKind,   // Block | Function | Loop | Match — Loop/Match don't change resolution semantics yet, but later warnings (e.g. break-outside-loop) will want to know
}

struct FunctionCtx {
    id: FunctionId,
    scopes: Vec<Scope>,     // this function's own lexical scope stack
    upvalues: Vec<UpvalueDesc>,
    next_slot: u32,         // next free slot — increments on `let`, decrements back on scope pop, so slots are genuinely reused
    high_water: u32,        // max next_slot ever reached — becomes frame_sizes[id]
}

pub struct Resolver {
    functions: Vec<FunctionCtx>,   // functions[0] is TopLevel; resolve_upvalue's fn_idx indexes into this
    diags: Vec<Diagnostic>,
    bindings: Bindings,
}
```

**Slot allocation and reuse.** Entering a block pushes a `Scope`; each `let` inside it takes `next_slot` and increments; leaving the block pops the scope and decrements `next_slot` by however many bindings that scope introduced (so a later sibling block's `let`s reuse those same slot numbers) while `high_water` only ever grows. This is what makes the eventual VM's stack layout correct — two sibling blocks don't need disjoint slot ranges.

**Two-pass top level.** Before resolving any top-level statement body, the resolver walks the flat top-level statement list once and inserts every `fn`/`type`/`struct` name into `functions[0]`'s outermost scope as an (uninitialized-irrelevant, since these aren't `let`-style values) global binding. *Then* it resolves every statement's body in order. This is what makes `is_even`/`is_odd` mutual recursion work regardless of declaration order. Top-level `let` is **not** hoisted — it resolves strictly sequentially, same as `let` anywhere else, so referencing a not-yet-declared `let` at top level is still the ordinary undeclared-name error.

**Native globals.** Before the two-pass walk, `functions[0]`'s scope is seeded with `print, len, push, clock, str, int, float, type_of` — the native functions `PROMPT.md`'s tree-walking interpreter section names — each as an already-initialized, immutable, "used" global binding, so calling them never trips undeclared-name detection.

**`resolve_upvalue`** — ports `PROMPT.md`'s algorithm directly onto `functions: Vec<FunctionCtx>`: check the immediately enclosing function's locals first (marking the found binding `captured = true` and appending to `captured_slots`), else recurse outward and thread the result through every intermediate function via `add_upvalue`, which deduplicates by `(index, is_local)` so two closures capturing the same variable share one upvalue index — this is the property the "counter closure" and "two closures share one cell" tests exist to catch.

**Resolution::Global.** A `Var` resolves to `Global` when the name is found in `functions[0]`'s scope (native, or a hoisted top-level `fn`/`type`/`struct`, or an already-resolved top-level `let`) — not as a fallback for "not found anywhere." A name found nowhere (not local, not upvalue, not global) is the undeclared-name error path, which includes a Levenshtein-distance "did you mean?" suggestion computed over every name currently reachable (locals in the enclosing scope chain, upvalue-reachable names, and globals), surfaced only when the closest match is within a small distance threshold.

**Diagnostics reused from `ember-diag`**, matching Phase 0-3's established pattern: `let x = x;` (self-reference in initializer, checked via the `initialized` flag being false until the initializer finishes resolving), assignment to a non-`mut` binding (with a `with_help` suggesting `let mut`), and the undeclared-name error.

**Warnings** (the 🟡 items): at each scope's pop, any `BindingInfo` with `used == false` and a name not starting with `_` produces an unused-variable warning at its declaration span; the same mechanism covers unused function names and unused parameters (parameters are just bindings in the function's outermost scope). Unreachable code: while resolving a block's statement list, once a `Return`/`Break`/`Continue` statement is resolved, every subsequent statement in that same list produces an "unreachable code" warning instead of being resolved as reachable code (still resolved for its own internal correctness — e.g. so nested `Var`s inside it are still checked — just flagged).

**Ambiguity resolved: `captured_slots` granularity.** `FxHashMap<FunctionId, Vec<u32>>` records *which slot numbers were ever captured anywhere in a function*, not *which specific binding at a given scope's exit needs `OP_CLOSE_UPVALUE`* — since slots are reused across sibling scopes, a slot number alone doesn't disambiguate "the captured binding from block A" from "the uncaptured binding from sibling block B that happens to reuse the same slot." This is intentionally coarse: nothing downstream consumes `captured_slots` yet (Phase 8's compiler will be the first real consumer), and the actual shape a bytecode compiler needs (e.g. captured-flag attached per scope-exit event, not a flat per-function set) is Phase 8's call to make with full context. `BindingInfo.captured` itself is precise (set on the exact `BindingInfo` that was captured); `captured_slots` is a provisional convenience view over it, documented here as such rather than left silently ambiguous.

## CLI

`ember resolve FILE`: runs lex → parse → resolve, then prints, per top-level item: each `Var`'s resolution (`local[N]` / `upvalue[N]` / `global(name)`), each function's upvalue descriptor list, and any diagnostics (errors and warnings, since warnings don't block resolution from completing).

## Testing strategy

Matches `CHECKLIST.md`'s named tests directly: local resolves to the correct slot with nested-scope shadowing; `let x = x;` errors; assignment to an immutable binding errors (with the `let mut` help); a counter closure produces exactly one upvalue; a triple-nested capture produces an upvalue chain at every intermediate level; two closures over the same variable share one upvalue index. Plus: mutual recursion via forward-referenced top-level `fn`, native-function calls not tripping undeclared-name detection, unused-variable/parameter warnings (including `_`-suppression), and unreachable-code-after-`return` warnings.
