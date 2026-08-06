# Phase 5 — Type Inference: Design

**Goal:** Implement Hindley-Milner type inference for `ember` as a new `ember-types` crate: constraint generation separated from constraint solving (for good error messages), unification with the occurs check, let-polymorphism via generalize/instantiate, the value restriction, nominal ADT/struct typing, and pattern typing. Add an `ember-cli typecheck` subcommand.

**Architecture:** `ember-types` is self-contained — it builds its own `Symbol -> Scheme` environment by walking scopes during inference, independent of `ember-resolve`'s `Bindings`. It depends only on `ember-span`, `ember-diag`, `ember-ast`, and `rustc-hash`. The CLI chains parse → resolve (bail on resolver errors) → infer, the same bail-early pattern `resolve` already uses for parse errors.

**Tech Stack:** Rust, `rustc-hash::FxHashMap`, union-find-style substitution store.

---

## A pre-existing resolver gap this phase surfaces

`Circle(3.0)` (constructing an ADT variant) parses as `Expr::Call{callee: Expr::Var(Circle), args}`. The resolver's two-pass top-level hoisting (`resolve_program`) only declares the *type* name (`Shape`) from `Stmt::TypeDecl`, never each `AdtVariant`'s own name. Verified directly: `ember-cli resolve` on a program constructing `Circle(3.0)` reports "undeclared name `Circle`". This blocks this phase's own checklist requirement ("ADT declarations register constructors as functions"), so it's fixed as this plan's first task — a small, isolated addition to the resolver's hoisting pass, with its own test, the same pattern as the Task 11 upvalue bug found mid-Phase-4.

## Crate structure — `crates/ember-types/`

- `ty.rs` — `TyVarId(u32)`; `Ty::{Int, Float, Bool, String, Unit, Var(TyVarId), Fun(Vec<Ty>, Box<Ty>), List(Box<Ty>), Adt(AdtId, Vec<Ty>), Record(BTreeMap<Symbol, Ty>)}`; `Scheme{vars: Vec<TyVarId>, ty: Ty}`.
- `adt.rs` — `AdtId(u32)`; `AdtRegistry` mapping a declared type `Symbol` to an `AdtId`, and each `AdtId` to `AdtDecl::Enum{variants: Vec<(Symbol, Vec<Ty>)>}` (from `Stmt::TypeDecl`) or `AdtDecl::Struct{fields: FxHashMap<Symbol, Ty>}` (from `Stmt::StructDecl`).
- `env.rs` — `TyEnv`: a scope stack of `FxHashMap<Symbol, Scheme>`, own push/pop, independent of the resolver's scope stack (different data needed: schemes, not slots).
- `constraint.rs` — `Constraint{lhs: Ty, rhs: Ty, origin: Origin}`; `Origin::{IfBranches, CallArgument, BinaryOp, Annotation, MatchArms, Return, ListElement, WhileCond, IndexTarget}`, each carrying the relevant spans.
- `subst.rs` — the union-find-style substitution store: `Vec<Option<Ty>>` indexed by `TyVarId`, `fresh() -> Ty::Var`, `resolve(ty)` following the chain with path compression, `occurs_in(var, ty)`.
- `unify.rs` — `unify(&mut self, a: &Ty, b: &Ty, origin: &Origin) -> Result<(), Diagnostic>`: resolve both sides, then match structurally (var-var, var-type with occurs check, `Fun`/`List`/`Adt`/`Record` structural recursion, arity mismatch, else mismatch-from-origin).
- `infer.rs` — the constraint-generation walk over every `Expr`/`Stmt`/`Pattern` variant; `generalize`/`instantiate`; the value restriction (`should_generalize`/`is_syntactic_value`); two-pass top-level handling (mirroring the resolver's own two-pass hoist, but for monomorphic-then-generalized function types); deferred field-access resolution; the public `infer()` entry point.
- `trace.rs` — `InferenceTrace{constraints: Vec<(Constraint, Span)>, steps: Vec<UnifyStep>, final_env}`; `UnifyStep{lhs, rhs, result_substitution, origin}` — built even without a playground consumer yet, per explicit approval this round.
- `display.rs` — pretty-printing `Ty`/`Scheme` with minimal parens and readable variable names (`a`, `b`, `c`, … not `t47`).

## Nominal ADT/struct typing

`Ty::Adt(AdtId, Vec<Ty>)` represents **both** enums and structs. `Vec<Ty>` is always empty this phase — `Stmt::TypeDecl`/`Stmt::StructDecl` have no generic type-parameter list in the grammar, so there is nothing to parameterize over. `Ty::Record` stays in the `Ty` enum (matching the checklist's literal variant list) but nothing in the current grammar produces one; it's inert until/unless an anonymous record type is ever added to the language. Keeping structs nominal (identified by `AdtId`, not structural field-shape) matches `Expr::Struct{name, ..}` requiring a name at construction — two structs with identical fields are not interchangeable.

## Constraint generation

- **Two-pass top level**, mirroring the resolver: pass 1 binds every top-level `fn` to a *monomorphic* function type (fresh param/return type variables, or the annotated types where present) in the top-level `TyEnv`, so mutual recursion between top-level functions type-checks. Pass 2 infers each function's body against that binding (enabling self- and mutually-recursive calls to unify consistently), unifies the body's result with the declared/inferred return type, then **generalizes** the solved type and rebinds it polymorphically in the top-level env before inferring the remaining top-level `let`s and expression statements.
- The same monomorphic-bind-then-infer-body pattern applies to any `Stmt::Fn` (so a nested function can still recurse correctly), but **only top-level** `fn`s get generalized afterward — matches the checklist's literal "generalize at `let` and top-level `fn` only." A nested `fn`'s type stays monomorphic at its own use sites within the enclosing function.
- Value restriction (`should_generalize(is_mut, init) = !is_mut && is_syntactic_value(init)`) implemented exactly as sketched in `PROMPT.md`/`SPEC.md`: `is_syntactic_value` covers `Expr::Int|Float|Str|Bool|Nil|Lambda{..}|Var(_)` only — not extended to list/struct literals or call expressions, staying faithful to the given reference rather than inventing a broader rule.
- ADT variant construction (`Circle(3.0)`) types as an ordinary `Call` against the constructor's function type in `AdtRegistry` (`Circle : Float -> Shape`). A nullary variant (`Point`) registers as a plain value binding of the ADT type — not a 0-arg function — since it's referenced as a bare `Var`, not called.
- Struct literals (`Expr::Struct{name, fields}`) resolve immediately at generation time (the name is a literal `Symbol`, no deferral needed): unify each provided field's expression type against the struct's declared field type; error on an unknown field name and on any missing required field, naming it.
- Field access (`Expr::Field{base, name}`) **cannot** resolve immediately under generate-then-solve, since `base`'s type may still be an unresolved variable at generation time. Handled as a deferred obligation: `infer.rs` collects `FieldObligation{base_ty: Ty, field: Symbol, result_ty: Ty::Var, span}` during generation; after solving all ordinary equality constraints to a fixpoint, each obligation is resolved against the final substitution. If `base_ty` is a concrete `Ty::Adt` whose registry entry is a `Struct`, look up the field's type and unify it with `result_ty`. If `base_ty` is still unresolved at this point, error "cannot infer the type of this field access; try adding a type annotation" (the standard HM limitation without row polymorphism/type classes). If `base_ty` resolves to a non-struct concrete type, error naming that type has no such field.
- Pattern typing (constrain the scrutinee, bind variables at the right types — **not** exhaustiveness, that's Phase 6):
  - `Ctor{name, args}` — look up the variant in `AdtRegistry`, unify each `args[i]`'s bound type against the variant's `i`-th payload type, unify the pattern's own type with `Ty::Adt(id, [])`.
  - `Record{name, fields}` — look up the struct, unify each field pattern's type against the declared field type, pattern's type is `Ty::Adt(id, [])`.
  - `Or(alts)` — type each alternative against the same scrutinee type. Bindings with the same name across different alternatives follow last-write-wins into the arm's single scope, the same simplification the resolver already applies to or-patterns, for consistency rather than inventing a stricter cross-check here.
  - `Tuple(items)` — there is no `Ty::Tuple` and no `Expr::Tuple` to ever produce a matchable value (a pre-existing AST gap, not something this phase's scope covers). Each sub-pattern binding gets a fresh, unconstrained type variable; the pattern is inert by construction. Documented, not silently "fixed" by inventing an unspec'd tuple type.
  - `Wild`/literal patterns — no bindings; literal patterns unify their own literal type against the scrutinee.

Exhaustiveness/unreachable-arm checking is explicitly out of scope (Phase 6).

## Diagnostics

- Mismatch errors formatted **from the `Origin`**, not raw type names — both contributing spans labeled (e.g. "these two `if` branches disagree", labeling both branches).
- Infinite-type error from the occurs check, with a readable explanation.
- `Fun` arity mismatch → dedicated "expected N arguments, found M".
- Struct literal: unknown-field and missing-field errors, each naming the field.
- Field access on an unresolved or non-struct type, as above.
- 🟡 "expected `Int`, found `Float`" mismatches get an extra help suggesting an explicit conversion (`int(..)`/`float(..)`, the native conversion functions already seeded as globals in the resolver).

## Public API + CLI

`pub fn infer(ast: &Ast, interner: &mut Interner, stmts: &[Idx<Stmt>]) -> (TypeInfo, Vec<Diagnostic>)` in `infer.rs`, re-exported from `lib.rs` alongside `Ty`, `Scheme`, `TyEnv`, `AdtId`, `AdtRegistry`. `TypeInfo` bundles per-expression types, per-top-level-fn schemes, the `AdtRegistry`, and (per this round's scope) the `InferenceTrace`.

`ember-cli typecheck <file>`: parse → resolve (bail and print resolver diagnostics on error, matching `run_resolve`'s bail-on-parse-error pattern) → `ember_types::infer`. Prints each expression's inferred type (sorted by span, matching `resolve`'s output style), each top-level fn's generalized scheme, and any type diagnostics.

## Tests

Every test explicitly listed in the checklist:
- `let x = 42` infers `Int`
- `fn identity(x) { x }` infers `∀a. a -> a`
- `identity(1)` and `identity("s")` both typecheck (let-polymorphism)
- Occurs check: `let f = |x| f(x)` → infinite type error, no hang
- `if` branch mismatch → error labeling both branches
- Arity mismatch → correct message
- Value restriction: mutable binding does not generalize

Plus, driven by this design's scope:
- ADT variant construction typing (`Circle(3.0) : Shape`, `Point : Shape`)
- Struct literal + field access, including missing-field and unknown-field errors
- Mutual recursion at the top level types correctly
- Field access on an unresolved type produces the "needs annotation" error
- The resolver fix: `Circle(3.0)` resolves without an undeclared-name error

## Non-goals (this phase)

- Exhaustiveness/unreachable-arm checking (Phase 6).
- Generic user-defined ADTs/structs (`type Option<T>`) — the grammar has no type-parameter syntax for declarations this phase.
- Row polymorphism / structural typing for field access on a still-unresolved type.
- A working `Ty::Tuple`/`Expr::Tuple` (pre-existing AST gap, not introduced or fixed here).
- Consuming `InferenceTrace` from an actual playground panel — Phase 16's job.
