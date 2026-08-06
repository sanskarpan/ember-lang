# Phase 6 — Exhaustiveness Checking: Design

**Goal:** Implement match exhaustiveness and unreachable-arm checking for `ember` via Maranget's usefulness algorithm on a pattern matrix, living inside the existing `ember-types` crate (per `SPEC.md §17`: "ember-types/ — Ty, Scheme, constraints, unify, exhaustiveness"). Non-exhaustive matches become compile errors naming the missing patterns; unreachable arms fall out of the same algorithm as warnings.

**Architecture:** Runs as its own pass, after `ember_types::infer()` fully completes — walks the AST for every `Expr::Match`, looks up each scrutinee's fully-resolved type from `TypeInfo.expr_types`, and runs the usefulness algorithm against its arms. Kept separate from Phase 5's eager-unification inference walk rather than woven into `infer_match`, since exhaustiveness needs a scrutinee type that's *already* fully solved, not one still being pinned down. `ember-cli typecheck` is extended to also run this pass and print its diagnostics alongside inference diagnostics, rather than gaining a new subcommand.

**Tech Stack:** Rust, reuses `ember-types`'s existing `Ty`/`AdtRegistry`/`Subst` and `ember-ast`'s `Pattern`/`MatchArm`.

---

## A small fix to already-shipped Phase 5 code

`AdtRegistry`'s `AdtDecl::Struct { fields: FxHashMap<Symbol, Ty> }` doesn't preserve field declaration order (`AdtDecl::Enum`'s `variants: Vec<(Symbol, Vec<Ty>)>` already does, with a comment anticipating exactly this need: "Ordered so error messages... can report in declaration order — Phase 6's job, but the data is here now"). Exhaustiveness needs a stable, deterministic field order for record patterns: a `Point { x, y } => ...` pattern's lowered form must have "unmentioned fields become wildcard" line up positionally the same way across every arm that matches the same struct. Fixed by changing `AdtDecl::Struct` to also carry an ordered field list (or restructuring its storage to preserve insertion order outright), with `struct_fields()`'s existing return type/signature unchanged — it just becomes deterministic where it previously wasn't specified. No existing Phase 5 call site depends on the old unordered behavior, so this is a strict improvement, not a breaking change.

## New files in `ember-types`

- **`pat.rs`** — the algorithm's internal normalized pattern shape, deliberately simpler than `ember_ast::Pattern`:
  ```rust
  enum Pat { Wild, Ctor(CtorId, Vec<Pat>) }
  enum CtorId {
      Variant(AdtId, Symbol), Struct(AdtId), Bool(bool),
      Int(i64), Float(u64) /* bit pattern, for Eq/Hash */, Str(Symbol),
      Nil, Cons, Tuple(usize),
  }
  ```
  No `Or` variant — **or-patterns expand into multiple matrix rows at lowering time** (`A | B` becomes two rows sharing one arm), matching the checklist's literal wording rather than threading Or-handling through the algorithm itself. `lower_pattern(ast, idx) -> Vec<Pat>` does this conversion; a `Bind` pattern lowers to `Wild` (a binding matches anything — its name is irrelevant to usefulness); a `Record` pattern lowers using the struct's full ordered field list, filling any field the pattern doesn't mention with `Wild`.

- **`ctor_set.rs`** — given a resolved `Ty`, either the complete enumerable list of `CtorId`s that type can produce (ADT enum → its variants; struct → one `CtorId::Struct`; `Bool` → `{true, false}`; `List` → `{Nil, Cons}`, the standard two-constructor treatment regardless of concrete length) or "not enumerable" (`Int`, `Float`, `String`, `Fun`, `Var`, `Unit` — always require a wildcard to close, since there's no finite constructor set, or in `Unit`'s case there's nothing meaningful to pattern-match structurally beyond a single trivial value which a bare `Wild`/`Bind` already covers).

- **`matrix.rs`** — `PatMatrix` (rows of `Vec<Pat>`, one column per scrutinee "field" at the current specialization depth), `specialize(ctor, matrix)` (keep rows whose first pattern is `ctor` or `Wild`, replace the first column with that constructor's sub-patterns), `default_matrix(matrix)` (keep rows whose first pattern is `Wild`, drop the first column).

- **`exhaustive.rs`** — `is_useful(matrix, query_row, ty) -> Usefulness` (recursive on column count; `Usefulness::Useful` carries witness pattern(s) so a positive exhaustiveness-check result names the concrete missing case, not just "yes/no"). `check_exhaustive(ast, interner, adts, arms, scrutinee_ty) -> Vec<Diagnostic>`:
  - Walks arms in order, lowering each into one or more rows.
  - For each arm: checks reachability of every one of its rows against the matrix accumulated so far (including earlier alternatives of the *same* arm, checked in sequence — so in `A | B`, `B` sees `A`). The arm is unreachable (warning, at the arm's span) only if *all* its rows are simultaneously not-useful.
  - **Guards**: an arm with a guard is still checked for reachability, but none of its rows are added to the running matrix afterward — a guarded arm can never be assumed to cover anything for later arms or the final check, since the guard may be false at runtime.
  - After all arms: checks whether `Wild` is still useful against the final matrix. If so, the match is non-exhaustive — the witnesses from that check become the "missing: ..." error, rendered via a witness-printing helper (reusing the `AdtRegistry`/`Interner`-passing style already established in `display.rs`) naming concrete patterns like `Rect(_, _)` and `Point`, matching the `SPEC.md §9` example output.
  - A program-level driver walks every `Expr::Match` node reachable from the top-level statements, looks up each match's scrutinee type via `TypeInfo.expr_types` (resolved through `TypeInfo.subst`), and skips any scrutinee whose type never got pinned down to anything concrete (an unconstrained `Ty::Var` — nothing meaningful to check).

## Nested patterns, tuples

Nested patterns (e.g. `Circle(Rect(_, _))`-shaped payloads, or a `List` pattern containing `Ctor` sub-patterns) are handled automatically by the matrix algorithm's own recursion — `specialize`/`default_matrix` operate column-by-column regardless of nesting depth, so no special-casing is needed beyond `lower_pattern` correctly recursing into sub-patterns.

`Pattern::Tuple` — the pre-existing inert-pattern gap from Phase 5 (no `Ty::Tuple`/`Expr::Tuple` exist in the grammar) — lowers to `CtorId::Tuple(arity)` treated as a single, always-complete constructor of whatever arity first appears in a given matrix. This means it never produces a spurious "non-exhaustive" diagnostic, but a tuple pattern remains fundamentally unreachable at runtime for the same reason it was inert in Phase 5 (nothing can ever construct a value for a scrutinee that would concretely be a tuple type) — this phase doesn't change that underlying gap, just declines to report false positives about it.

## CLI

`ember-cli typecheck <file>` gains one more step after `ember_types::infer()`: run the exhaustiveness pass over the resulting `TypeInfo` and append its diagnostics to what's printed, using the same `print_diagnostics` path already in place.

## Tests

Every test explicitly listed in the checklist (missing one ADT variant named in the error, a `_` arm makes any match exhaustive, an arm after `_` reported unreachable), plus coverage this design adds: nested-pattern exhaustiveness, list-pattern (`[]`/`[_, ..]`) exhaustiveness, struct/record-pattern exhaustiveness (including partial field patterns correctly treating unmentioned fields as covered), guard-suppressed exhaustiveness contribution, or-pattern row expansion and internal reachability (`A | B` where `B` is redundant given `A`), and the ordered-struct-fields fix itself.

## Non-goals (this phase)

- Both execution backends, GC, formatter, LSP, WASM bindings, playground — later phases, each gets its own cycle.
- Fixing `Pattern::Tuple`'s underlying inertness (no `Ty::Tuple`/`Expr::Tuple`) — a grammar-level gap from Phase 5, not this phase's to close.
- Fixing the bare-nullary-variant-vs-bind-pattern ambiguity from Phase 5 (`Point` with no parens always lowers as `Wild` via `Pattern::Bind`, same as before) — same reasoning, a parser-level gap outside this phase's scope.
