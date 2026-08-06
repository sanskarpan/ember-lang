# Phase 7 — Tree-Walking Interpreter: Design

**Goal:** Implement `ember`'s reference execution backend — a simple, obviously-correct tree-walking interpreter, per `SPEC.md §10`: "Simple, obviously correct, and slow. Its job: be the reference implementation and the debugger-friendly backend." Lives in the already-stubbed `ember-tree` crate. Adds an `ember-cli run` subcommand.

**Architecture:** `ember-tree` is decoupled from `ember-resolve`/`ember-types` — it does plain dynamic name lookup via an `Env` chain (`FxHashMap<Symbol, Value>` with a parent pointer), matching `SPEC.md`'s sketch exactly rather than consuming the resolver's slot-based `Resolution`. In the full pipeline, `ember-cli run` chains parse → resolve (bail on errors) → infer (bail on errors) → interpret, but the interpreter crate itself has no dependency on that having happened — a deliberate simplicity/speed tradeoff that's the whole point of this backend (Phase 8/9's bytecode VM is where the resolver's slot allocation actually gets used).

**Tech Stack:** Rust, `Rc<RefCell<..>>` for shared mutable heap state (environments, lists, records — exactly what makes closures work and what makes this backend slow, per the spec's own framing), `rustc_hash::FxHashMap`.

---

## `Value`

Adapted from `SPEC.md §10`'s sketch, with two additions beyond the literal text:

```rust
pub enum Value {
    Int(i64), Float(f64), Bool(bool), Nil,
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<Closure>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    Record { name: Symbol, fields: Rc<RefCell<FxHashMap<Symbol, Value>>> },
    AdtCtor { type_name: Symbol, variant: Symbol, arity: usize },
}
```

- **`Record` gains a `name: Symbol` field** the spec's bare sketch doesn't have — needed for a meaningful `type_of()` on a struct instance (the fields map alone can't say "Point"). The fields themselves stay behind `Rc<RefCell<..>>` for interior mutability, since field assignment (`p.x = 5.0`) type-checks generically in Phase 5 and needs real runtime support.
- **`AdtCtor` is new.** The spec's sketch only shows the shape of an already-*constructed* `AdtValue`; it doesn't say how a payload-ful variant constructor (`Circle`) is represented as a callable value before it's invoked. Evaluating `Circle` as a bare `Var` yields `Value::AdtCtor{type_name: Shape, variant: Circle, arity: 1}`; calling it builds the real `Value::Adt(Rc::new(AdtValue{type_name, variant, fields}))`. A *nullary* variant (`Point`) skips this entirely — bound directly to an already-constructed `Value::Adt` with no fields, mirroring exactly how Phase 5 typed it (`Ty::Fun` for payload-ful, a plain `Ty::Adt` value for nullary).

```rust
pub struct AdtValue { pub type_name: Symbol, pub variant: Symbol, pub fields: Vec<Value> }
pub struct Closure { pub params: Vec<Symbol>, pub body: Idx<Expr>, pub env: Rc<RefCell<Env>> }
pub struct NativeFn { pub name: &'static str, pub arity: usize, pub func: fn(&[Value], Span) -> Result<Value, RuntimeError> }
pub struct Env { pub values: FxHashMap<Symbol, Value>, pub parent: Option<Rc<RefCell<Env>>> }
```

`Closure` doesn't store a reference to the `Ast` — it stores only `body: Idx<Expr>`; the whole interpretation session shares one `&Ast` threaded through every eval call, which is simpler than wrapping the AST in an `Rc` and avoids a lifetime fight for no real benefit (the AST outlives the interpreter call by construction).

## Control flow: `Flow`, never `panic!`

```rust
pub enum Flow { Normal(Value), Return(Value), Break, Continue }
pub type EvalResult = Result<Flow, RuntimeError>;
```

Exactly as sketched. `return`/`break`/`continue` thread through the return type; using `panic!`/`catch_unwind` would break WASM (Phase 15) and make single-stepping impossible — the spec calls this out explicitly as a shortcut not to take.

## Runtime errors

`RuntimeError{message: String, span: Span, call_stack: Vec<Span>}` with a `to_diagnostic()` conversion into `ember_diag::Diagnostic` (so `ember-cli run` renders it exactly like every other diagnostic in the pipeline). Categories, per the checklist plus one addition:

- Stack overflow — a call-depth counter on the interpreter, checked at every function call; exceeding it produces a diagnostic **naming the call chain** (the accumulated `call_stack` of call-site spans), not a process crash from a real Rust stack overflow.
- Integer overflow — every `Int` arithmetic operation uses checked arithmetic (`checked_add`/`checked_sub`/`checked_mul`/`checked_div`/`checked_rem`), reporting the operand values on overflow.
- Division by zero — checked explicitly before division, for a clearer message than a generic overflow report.
- **Index out of bounds** (addition beyond the checklist's explicit list) — necessary once list indexing exists with dynamic (not statically known) indices; the same class of "genuinely runtime-only failure" as the other three, not something the resolver/type-checker could have caught upstream.

Everything else — undeclared names, assignment to an immutable binding, type mismatches — is the resolver/type-checker's job upstream; the interpreter trusts a well-formed pipeline already checked those and doesn't re-verify them.

## Pattern matching at runtime

`match_pattern(ast, interner, pat, value, env) -> bool`, binding names into `env` as it recursively walks. Mirrors `ember_ast::Pattern`'s shape directly (unlike Phase 6's exhaustiveness checker, which needed its own normalized `Pat`/`CtorId` — matching here is a straightforward recursive walk with no matrix algorithm involved). `Pattern::Tuple` can never successfully match — consistent with it being inert since Phase 5/6 (no `Ty::Tuple`/`Expr::Tuple`, and now no `Value::Tuple` either — nothing can construct a value for one to match against). Documented as a carried-over gap, not newly introduced or newly "fixed" here.

## Step-mode

Per explicit scope decision to include it this round: implemented as a callback hook stored on the `Interp` struct itself —

```rust
pub struct StepEvent { pub node_span: Span, pub env_snapshot: Vec<(Symbol, Value)>, pub result: Option<Value> }
```

`Interp` carries `step_hook: Option<Box<dyn FnMut(StepEvent)>>`, invoked once per `Expr`/`Stmt` node evaluated (after the node's own evaluation completes, so `result` is populated for expressions; `None` for statements). `env_snapshot` flattens the whole `Env` chain (innermost shadows outermost) — `Value` clones are cheap since every heap-backed variant is `Rc`. This is a **synchronous callback**, not true async pause/resume: a real interactive debugger would run interpretation on a background thread and have the callback block on a channel waiting for a "continue" signal — that's the *consumer's* job (a future LSP/playground phase), not this crate's. With no hook installed, the check is a single `if`, effectively zero overhead on the common path.

## Native functions

Real runtime implementations of the 8 functions already type-signature-registered in `ember-resolve`/`ember-types`: `print` (writes to stdout, returns `Nil`), `len` (list length), `push` (mutates the list in place via its `RefCell`, returns `Nil`), `clock` (wall-clock seconds as `Float`), `str`/`int`/`float` (value-to-value conversions, erroring on nonsensical ones like `int("abc")` rather than panicking), `type_of` (returns a `String` naming the value's runtime type — `"Int"`, `"Shape"` for an ADT value via its `type_name`, `"Point"` for a struct via its `name`, etc.).

## CLI

`ember-cli run <file>`: parse → resolve (bail and print resolver diagnostics on error) → infer (bail and print type diagnostics on error) → interpret, printing the program's final value, or a rendered runtime-error diagnostic on failure. The natural culmination of every prior subcommand (`tokens`/`ast`/`resolve`/`typecheck`) — this one actually *runs* the program.

## Tests

Every test explicitly listed in the checklist: arithmetic/comparison/logical short-circuit, closures capture-and-mutate correctly, recursion (`fib`/`fact`), all loop forms with `break`/`continue`, pattern matching with destructuring, shared mutable capture between two closures, runtime error spans pointing at the right expression. Plus, driven by this design's scope: ADT/struct construction and field access at runtime, each native function's real behavior, integer-overflow and division-by-zero diagnostics, stack-overflow reporting a real call chain, index-out-of-bounds, and step-mode event sequencing.

## Non-goals (this phase)

- The bytecode compiler/VM backend (Phase 8/9) — a different, faster execution path that *does* consume the resolver's slot allocation; this phase is the reference implementation only.
- Fixing `Pattern::Tuple`'s underlying inertness — carried over from Phase 5/6, still out of scope (a grammar/AST-level change).
- True interactive/async step-through debugging — the synchronous callback hook exists; wiring it to an actual pausable debugger UI is later-phase work (LSP/playground).
- Garbage collection — `Rc`-based reference counting is this backend's whole memory story; the mark-sweep GC (Phase 10) belongs to the bytecode VM.
