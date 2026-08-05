# ember Phase 4 Implementation Plan — Resolver

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `ember-resolve` crate: scope resolution, slot assignment, mutability/initialization checks, upvalue capture, "did you mean?" suggestions, and unused-binding/unreachable-code warnings — plus a `resolve` subcommand on `ember-cli`.

**Architecture:** A `Resolver` walks the already-parsed `Ast` once, maintaining a stack of `FunctionCtx` (one per lexical function nesting level, each with its own stack of block/loop/match scopes), producing a `Bindings` structure — a `Var`-node → `Resolution` map plus per-function upvalue/frame-size/captured-slot data — and a `Vec<Diagnostic>` for errors and warnings.

**Tech Stack:** Rust, `rustc-hash` (`FxHashMap`), building on the existing `ember-span`/`ember-diag`/`ember-ast` crates from Phase 0-3.

---

## Task 1: Give parameters real spans and unify Lambda/Fn parameter representation

**Files:**
- Modify: `crates/ember-ast/src/stmt.rs`
- Modify: `crates/ember-ast/src/expr.rs`
- Modify: `crates/ember-ast/src/print.rs`
- Modify: `crates/ember-parser/src/parser.rs`

This is a small, necessary fix surfaced by this phase's design, not a new feature: `Param` (used by `Stmt::Fn`) currently has no `span` field, and `Expr::Lambda`'s params are a bare `Vec<Symbol>` with no span at all — so there's nowhere to anchor an "unused parameter" diagnostic. Fixing both now, before the resolver needs them, avoids threading spans through twice.

- [ ] **Step 1: Write the failing tests**

Add to `crates/ember-parser/src/parser.rs`'s test module:

```rust
#[test]
fn fn_param_has_a_span_covering_its_name() {
    let (ast, _interner, stmt, diags) = parse_stmt_from_str("fn f(x: Int) { x }");
    assert!(diags.is_empty(), "diags: {diags:?}");
    match ast.stmt(stmt) {
        Stmt::Fn { params, .. } => {
            assert_eq!(params[0].span, Span::new(5, 6)); // the "x" in "fn f(x: Int)"
        }
        other => panic!("expected Fn, got {other:?}"),
    }
}

#[test]
fn lambda_params_are_now_full_params_with_spans() {
    let (ast, interner, e, diags) = parse_expr_from_str("|x, y| x");
    assert!(diags.is_empty(), "diags: {diags:?}");
    match ast.expr(e) {
        Expr::Lambda { params, .. } => {
            assert_eq!(params.len(), 2);
            assert_eq!(interner.resolve(params[0].name), "x");
            assert_eq!(params[0].span, Span::new(1, 2));
            assert!(params[0].ty.is_none(), "lambda params never have type annotations");
        }
        other => panic!("expected Lambda, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p ember-parser param_has_a_span lambda_params_are_now`
Expected: FAIL — `Param` has no `span` field yet; `Expr::Lambda.params` is `Vec<Symbol>`, not `Vec<Param>`, so `.name`/`.span` don't exist on its elements.

- [ ] **Step 3: Implement**

In `crates/ember-ast/src/stmt.rs`, add `span` to `Param`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Symbol,
    pub ty: Option<Idx<TypeExpr>>,
    pub span: Span,
}
```

(Add `use ember_span::Span;` to the top of the file if not already imported.)

In `crates/ember-ast/src/expr.rs`, change `Lambda`'s field type:

```rust
Lambda { params: Vec<crate::stmt::Param>, body: Idx<Expr> },
```

In `crates/ember-ast/src/print.rs`, update the `Lambda` arm in `print_expr` (it currently does `params.iter().map(|p| interner.resolve(*p).to_string())`):

```rust
Expr::Lambda { params, body } => {
    let params_str: Vec<_> = params.iter().map(|p| interner.resolve(p.name).to_string()).collect();
    format!("|{}| {}", params_str.join(", "), print_expr(ast, interner, *body))
}
```

In `crates/ember-parser/src/parser.rs`:

1. In `fn_stmt`, the params loop currently does `params.push(Param { name: p_name, ty: p_ty });` — change to include the span:
```rust
params.push(Param { name: p_name, ty: p_ty, span: p_tok.span });
```
(`p_tok` is already the name token in that loop — confirm it's still in scope at the push site; it is, since it's bound earlier in the same loop iteration.)

2. `lambda_params` currently returns `Vec<ember_ast::Symbol>` and does `params.push(self.interner.intern(&text))`. Change its signature and body to build `Param`s instead:
```rust
fn lambda_params(&mut self, open: Token) -> Vec<Param> {
    let mut params = Vec::new();
    if self.peek().kind != TokenKind::Pipe {
        loop {
            let p = self.advance();
            if p.kind != TokenKind::Ident {
                self.emit(Diagnostic::error("expected a parameter name").with_primary(p.span, "here"));
            } else {
                let text = self.text(p.span).to_string();
                let sym = self.interner.intern(&text);
                params.push(Param { name: sym, ty: None, span: p.span });
            }
            if self.peek().kind == TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
    }
    self.expect_close(open, TokenKind::Pipe);
    params
}
```
(`Param` must already be in scope via the crate's `use ember_ast::{..., Param, ...}` import line from Task 23 of the Phase 0-3 plan — confirm it's there; if the import list doesn't include `Param` for some reason, add it.)

3. The two `prefix` arms that build `Expr::Lambda` (for `Pipe` and `OrOr`) call `self.lambda_params(tok)` and `Vec::new()` respectively — no change needed at the call sites themselves since the return type change is transparent to them, but double check the `OrOr` arm's `params: Vec::new()` still type-checks as `Vec<Param>` (it does — an empty vec infers its element type from context).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --workspace`
Expected: PASS across the whole workspace (this changes a public type used only within `ember-ast`/`ember-parser`, so no other crate is affected). Run `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` too — both clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-ast crates/ember-parser
git commit -m "Give Param a span and unify Lambda params to Vec<Param>"
```

---

## Task 2: Scaffold the `ember-resolve` crate

**Files:**
- Modify: `crates/ember-resolve/Cargo.toml`
- Modify: `crates/ember-resolve/src/lib.rs`

- [ ] **Step 1: Update the manifest**

`crates/ember-resolve/Cargo.toml`:
```toml
[package]
name = "ember-resolve"
version.workspace = true
edition.workspace = true

[dependencies]
ember-span = { path = "../ember-span" }
ember-diag = { path = "../ember-diag" }
ember-ast = { path = "../ember-ast" }
rustc-hash = "2"

[dev-dependencies]
ember-parser = { path = "../ember-parser" }
```

`ember-resolve` doesn't depend on `ember-parser` in its real (non-test) code — a resolver has no business knowing how to parse — but its own tests are much easier to write against real parsed programs via `ember_parser::parse` than by hand-building `Ast` nodes for every case, so it's a dev-dependency only.

- [ ] **Step 2: Stub the module layout**

`crates/ember-resolve/src/lib.rs`:
```rust
pub mod binding;
pub mod edit_distance;
pub mod resolver;
pub mod scope;
```

Create empty placeholder files `crates/ember-resolve/src/binding.rs`, `crates/ember-resolve/src/edit_distance.rs`, `crates/ember-resolve/src/resolver.rs`, `crates/ember-resolve/src/scope.rs`, each containing just `// implemented in Task N` (matching the task that fills each one in: binding.rs → Task 3, scope.rs → Task 4, edit_distance.rs → Task 5, resolver.rs → Task 6).

- [ ] **Step 3: Verify the workspace still builds**

Run: `source "$HOME/.cargo/env" && cargo build --workspace`
Expected: builds cleanly (empty modules with only comments compile fine).

- [ ] **Step 4: Commit**

```bash
git add crates/ember-resolve
git commit -m "Scaffold ember-resolve crate module layout"
```

---

## Task 3: `binding.rs` — BindingInfo, Resolution, UpvalueDesc, FunctionId, Bindings

**Files:**
- Modify: `crates/ember-resolve/src/binding.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Idx;

    #[test]
    fn function_id_hashes_and_compares_correctly() {
        use std::collections::HashSet;
        let a = FunctionId::TopLevel;
        let b = FunctionId::Lambda(Idx::new(0));
        let c = FunctionId::Lambda(Idx::new(0));
        let d = FunctionId::Lambda(Idx::new(1));
        assert_eq!(b, c);
        assert_ne!(b, d);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c); // duplicate of b
        set.insert(d);
        assert_eq!(set.len(), 3, "FunctionId must hash/compare correctly to dedupe in a HashSet/HashMap");
    }

    #[test]
    fn bindings_starts_empty() {
        let b = Bindings::new();
        assert!(b.resolutions.is_empty());
        assert!(b.upvalues.is_empty());
        assert!(b.frame_sizes.is_empty());
        assert!(b.captured_slots.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve`
Expected: FAIL — nothing defined yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::{Expr, Idx, Stmt, Symbol};
use ember_span::Span;
use rustc_hash::FxHashMap;

/// Everything the resolver knows about one declared name at one point in
/// the scope stack.
#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub slot: u32,
    pub mutable: bool,
    pub initialized: bool,
    pub span: Span,
    pub used: bool,
    /// Set once any inner function captures this binding as an upvalue.
    pub captured: bool,
}

/// Where a `Var` node's name actually lives at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Local { slot: u32 },
    Upvalue { index: u32 },
    Global { symbol: Symbol },
}

/// One entry in a function's upvalue list: either "capture slot `index`
/// from the immediately enclosing function's locals" (`is_local: true`) or
/// "capture upvalue `index` from the immediately enclosing function's own
/// upvalue list" (`is_local: false`) — the two cases `resolve_upvalue`
/// distinguishes when threading a capture through intermediate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    pub index: u32,
    pub is_local: bool,
}

/// Identifies one function-introducing AST node. `Expr::Lambda` isn't the
/// only thing that introduces a function scope — `Stmt::Fn` can appear
/// anywhere a statement can, including nested inside another function's
/// body, so both need their own identity here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionId {
    TopLevel,
    Lambda(Idx<Expr>),
    Fn(Idx<Stmt>),
}

/// The resolver's full output: everything Phase 5 (type inference) and
/// Phase 7-9 (both backends) need to know about names and closures.
#[derive(Debug, Default)]
pub struct Bindings {
    pub resolutions: FxHashMap<Idx<Expr>, Resolution>,
    pub upvalues: FxHashMap<FunctionId, Vec<UpvalueDesc>>,
    pub frame_sizes: FxHashMap<FunctionId, u32>,
    /// Which slot numbers were ever captured somewhere in a function.
    /// Coarser than `BindingInfo.captured` (see the Phase 4 design doc's
    /// "Ambiguity resolved" note) — a provisional shape a future bytecode
    /// compiler may want to refine once it knows exactly what it needs at
    /// each scope-exit point.
    pub captured_slots: FxHashMap<FunctionId, Vec<u32>>,
}

impl Bindings {
    pub fn new() -> Self {
        Bindings::default()
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, both tests green. Run `cargo clippy -p ember-resolve --all-targets -- -D warnings` and `cargo fmt -p ember-resolve -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add BindingInfo, Resolution, UpvalueDesc, FunctionId, and Bindings types"
```

---

## Task 4: `scope.rs` — Scope, ScopeKind, FunctionCtx with slot reuse

**Files:**
- Modify: `crates/ember-resolve/src/scope.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;
    use ember_span::Span;

    #[test]
    fn declare_and_lookup_within_one_scope() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        let slot = fc.declare(x, false, true, Span::new(0, 1));
        assert_eq!(slot, 0);
        assert_eq!(fc.lookup(x).unwrap().slot, 0);
    }

    #[test]
    fn slots_are_reused_after_scope_pop() {
        let mut interner = Interner::new();
        let a = interner.intern("a");
        let b = interner.intern("b");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.push_scope(ScopeKind::Block);
        fc.declare(a, false, true, Span::new(0, 1));
        fc.pop_scope();
        fc.push_scope(ScopeKind::Block);
        let slot = fc.declare(b, false, true, Span::new(2, 3));
        fc.pop_scope();
        assert_eq!(slot, 0, "b should reuse a's slot after a's scope exited");
    }

    #[test]
    fn shadowing_in_nested_scope_hides_outer_binding() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.declare(x, false, true, Span::new(0, 1));
        fc.push_scope(ScopeKind::Block);
        let inner_slot = fc.declare(x, false, true, Span::new(2, 3));
        assert_eq!(fc.lookup(x).unwrap().slot, inner_slot);
        fc.pop_scope();
        assert_eq!(fc.lookup(x).unwrap().slot, 0, "outer x visible again after inner scope pops");
    }

    #[test]
    fn high_water_mark_tracks_the_deepest_slot_usage() {
        let mut interner = Interner::new();
        let a = interner.intern("a");
        let b = interner.intern("b");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.declare(a, false, true, Span::new(0, 1));
        fc.push_scope(ScopeKind::Block);
        fc.declare(b, false, true, Span::new(2, 3));
        fc.pop_scope();
        assert_eq!(fc.high_water, 2, "both a and b were alive at once, even though b's slot is now free again");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve scope`
Expected: FAIL — nothing defined.

- [ ] **Step 3: Implement**

```rust
use ember_ast::Symbol;
use ember_span::Span;
use rustc_hash::FxHashMap;

use crate::binding::{BindingInfo, FunctionId, UpvalueDesc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Block,
    Function,
    Loop,
    Match,
}

pub struct Scope {
    pub bindings: FxHashMap<Symbol, BindingInfo>,
    pub kind: ScopeKind,
}

impl Scope {
    pub fn new(kind: ScopeKind) -> Self {
        Scope { bindings: FxHashMap::default(), kind }
    }
}

pub struct FunctionCtx {
    pub id: FunctionId,
    pub scopes: Vec<Scope>,
    pub upvalues: Vec<UpvalueDesc>,
    pub next_slot: u32,
    pub high_water: u32,
}

impl FunctionCtx {
    pub fn new(id: FunctionId) -> Self {
        FunctionCtx {
            id,
            scopes: vec![Scope::new(ScopeKind::Function)],
            upvalues: Vec::new(),
            next_slot: 0,
            high_water: 0,
        }
    }

    pub fn push_scope(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope::new(kind));
    }

    /// Pops the current scope, releasing its slots back for reuse by later
    /// sibling scopes, and returns the popped scope so the caller can
    /// inspect its bindings (e.g. to emit unused-variable warnings) before
    /// they're gone.
    pub fn pop_scope(&mut self) -> Scope {
        let scope = self.scopes.pop().expect("pop_scope called with no scope open");
        self.next_slot -= scope.bindings.len() as u32;
        scope
    }

    /// Declares a new binding in the current (innermost) scope, allocating
    /// the next free slot. `initialized` is `false` for a `let` whose
    /// initializer hasn't been resolved yet (so a self-reference in that
    /// initializer can be caught), `true` for parameters and hoisted
    /// top-level declarations.
    pub fn declare(&mut self, name: Symbol, mutable: bool, initialized: bool, span: Span) -> u32 {
        let slot = self.next_slot;
        self.next_slot += 1;
        if self.next_slot > self.high_water {
            self.high_water = self.next_slot;
        }
        let info = BindingInfo { slot, mutable, initialized, span, used: false, captured: false };
        self.scopes.last_mut().expect("no scope open").bindings.insert(name, info);
        slot
    }

    pub fn mark_initialized(&mut self, name: Symbol) {
        if let Some(info) = self.lookup_mut(name) {
            info.initialized = true;
        }
    }

    /// Innermost-first lookup, so shadowing resolves to the nearest declaration.
    pub fn lookup(&self, name: Symbol) -> Option<&BindingInfo> {
        self.scopes.iter().rev().find_map(|s| s.bindings.get(&name))
    }

    pub fn lookup_mut(&mut self, name: Symbol) -> Option<&mut BindingInfo> {
        self.scopes.iter_mut().rev().find_map(|s| s.bindings.get_mut(&name))
    }

    /// Looks up a binding **only in the outermost (function-level) scope**
    /// — used for the top-level's hoisted `fn`/`type`/`struct` names and
    /// native globals, which must resolve to `Global` regardless of how
    /// deeply nested the reference is within nested blocks of the same
    /// function-index-0 context.
    pub fn lookup_in_outermost(&self, name: Symbol) -> Option<&BindingInfo> {
        self.scopes.first().and_then(|s| s.bindings.get(&name))
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Implement FunctionCtx scope stack with slot allocation and reuse"
```

---

## Task 5: `edit_distance.rs` — Levenshtein distance and "did you mean?"

**Files:**
- Modify: `crates/ember-resolve/src/edit_distance.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_have_zero_distance() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn one_substitution_has_distance_one() {
        assert_eq!(levenshtein("cat", "bat"), 1);
    }

    #[test]
    fn completely_different_strings_have_a_larger_distance() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn closest_match_finds_a_near_typo() {
        let candidates = ["count", "counter", "total"];
        assert_eq!(closest_match("cout", candidates.into_iter()), Some("count"));
    }

    #[test]
    fn closest_match_returns_none_when_nothing_is_close() {
        let candidates = ["apple", "banana"];
        assert_eq!(closest_match("xyz", candidates.into_iter()), None);
    }

    #[test]
    fn closest_match_excludes_exact_matches() {
        let candidates = ["count"];
        assert_eq!(closest_match("count", candidates.into_iter()), None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve edit_distance`
Expected: FAIL — `levenshtein`/`closest_match` don't exist.

- [ ] **Step 3: Implement**

```rust
/// Standard Levenshtein edit distance via the two-row dynamic-programming
/// algorithm — O(len(a) * len(b)) time, O(min(len(a), len(b))) space.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Finds the closest candidate to `target` among `candidates` by
/// Levenshtein distance, only if it's close enough to plausibly be a typo
/// rather than an unrelated name — and never suggests `target` itself.
pub fn closest_match<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let threshold = (target.chars().count() / 3).max(2);
    candidates
        .map(|c| (c, levenshtein(target, c)))
        .filter(|(_, d)| *d <= threshold && *d > 0)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add Levenshtein distance and closest_match for did-you-mean suggestions"
```

---

## Task 6: `resolver.rs` skeleton — native globals, literals, single-scope Var resolution, undeclared-name error

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_globals_resolve_without_error() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("print(1);");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn undeclared_name_is_an_error_with_a_suggestion() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let count = 1;\nprin(count);");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(errors[0].message.contains("prin"));
        assert!(errors[0].help.iter().any(|h| h.message.contains("print")), "expected a did-you-mean suggestion toward `print`, got: {:?}", errors[0].help);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve resolver`
Expected: FAIL — `Resolver` doesn't exist.

- [ ] **Step 3: Implement**

```rust
use ember_ast::{Ast, Expr, Idx, Interner, Stmt};
use ember_diag::Diagnostic;

use crate::binding::{Bindings, FunctionId};
use crate::edit_distance::closest_match;
use crate::scope::FunctionCtx;

const NATIVE_GLOBALS: &[&str] = &["print", "len", "push", "clock", "str", "int", "float", "type_of"];

pub struct Resolver<'a> {
    ast: &'a Ast,
    interner: &'a mut Interner,
    functions: Vec<FunctionCtx>,
    diagnostics: Vec<Diagnostic>,
    bindings: Bindings,
}

impl<'a> Resolver<'a> {
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        let mut r = Resolver {
            ast,
            interner,
            functions: vec![FunctionCtx::new(FunctionId::TopLevel)],
            diagnostics: Vec::new(),
            bindings: Bindings::new(),
        };
        r.seed_native_globals();
        r
    }

    fn seed_native_globals(&mut self) {
        for name in NATIVE_GLOBALS {
            let sym = self.interner.intern(name);
            self.functions[0].declare(sym, false, true, ember_span::Span::new(0, 0));
            // Natives are never "unused" — nothing to warn about even if a
            // program never calls e.g. `clock`.
            if let Some(info) = self.functions[0].lookup_mut(sym) {
                info.used = true;
            }
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_bindings(self) -> (Bindings, Vec<Diagnostic>) {
        (self.bindings, self.diagnostics)
    }

    fn emit(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    /// All names currently reachable, for "did you mean?" — every scope of
    /// every function currently on the stack (order doesn't matter for the
    /// suggestion itself, only that everything visible right now is
    /// included).
    fn reachable_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for fc in &self.functions {
            for scope in &fc.scopes {
                for sym in scope.bindings.keys() {
                    names.push(self.interner.resolve(*sym).to_string());
                }
            }
        }
        names
    }

    /// Resolves a single name reference by walking the current function's
    /// scopes innermost-first, then (for now, until Task 12 adds real
    /// upvalue-chain support) falling back to the top-level's outermost
    /// scope for globals. Marks the binding used on a local hit. Returns
    /// the `Resolution` to record, or `None` if an "undeclared name"
    /// diagnostic was emitted instead.
    fn resolve_name(&mut self, name_sym: ember_ast::Symbol, name_text: &str, span: ember_span::Span) -> Option<crate::binding::Resolution> {
        let current = self.functions.len() - 1;
        if let Some(info) = self.functions[current].lookup_mut(name_sym) {
            info.used = true;
            return Some(crate::binding::Resolution::Local { slot: info.slot });
        }
        if self.functions[0].lookup_in_outermost(name_sym).is_some() {
            return Some(crate::binding::Resolution::Global { symbol: name_sym });
        }
        let suggestion = {
            let names = self.reachable_names();
            closest_match(name_text, names.iter().map(|s| s.as_str())).map(|s| s.to_string())
        };
        let mut diag = Diagnostic::error(format!("undeclared name `{name_text}`")).with_primary(span, "not found in this scope");
        if let Some(sugg) = suggestion {
            diag = diag.with_help(format!("did you mean `{sugg}`?"));
        }
        self.emit(diag);
        None
    }
}
```

Note: `resolve_name` mutably borrows `self.functions[current]` and, in the undeclared-name branch, calls `self.reachable_names()` / `self.emit(...)` — both of which also touch `self`. Since the local-hit branch `return`s before ever reaching the undeclared-name code, and `self.functions[current].lookup_mut(...)`'s borrow ends at that `return`, there's no overlapping-borrow conflict here; the borrow checker accepts this as written because Rust's non-lexical lifetimes end the `lookup_mut` borrow at the last use inside that `if let` block, before the fallthrough path runs.

Now add `resolve_expr` (only the `Var`/literal cases for this task — everything else gets a `_ => {}` catch-all filled in across Tasks 7-13) and the `resolve_program`/`resolve_stmt` entry points needed for the tests above to compile and pass:

```rust
impl<'a> Resolver<'a> {
    pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
        for &s in stmts {
            self.resolve_stmt(s);
        }
    }

    fn resolve_stmt(&mut self, idx: Idx<Stmt>) {
        match self.ast.stmt(idx) {
            Stmt::ExprStmt(e) => self.resolve_expr(*e),
            _ => {} // every other statement kind is filled in across Tasks 7-13
        }
    }

    fn resolve_expr(&mut self, idx: Idx<Expr>) {
        match self.ast.expr(idx) {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil | Expr::Str(_) | Expr::Error => {}
            Expr::Var(sym) => {
                let span = self.ast.span_of_expr(idx);
                let text = self.interner.resolve(*sym).to_string();
                if let Some(res) = self.resolve_name(*sym, &text, span) {
                    self.bindings.resolutions.insert(idx, res);
                }
            }
            Expr::Call { callee, args } => {
                self.resolve_expr(*callee);
                for a in args {
                    self.resolve_expr(*a);
                }
            }
            _ => {} // every other expression kind is filled in across Tasks 7-13
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, both tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add Resolver skeleton: native globals, literal/Var/Call resolution, undeclared-name error"
```

---

## Task 7: `let` statement resolution — the `initialized` flag and `let x = x;`

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn let_binding_resolves_its_initializer_then_becomes_usable() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\nprint(x);");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn let_x_equals_x_is_an_error_even_with_an_outer_x_in_scope() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\n{ let x = x; }");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("its own initializer"), "message: {}", errors[0].message);
}
```

Note: `{ let x = x; }` here is a **block expression used as a statement**. Since `ember-resolve`'s `resolve_stmt` doesn't handle bare block-expression statements yet at this point in the plan (that's Task 9), and top-level `parse()` requires every non-keyword-led statement to go through `expr_stmt`, this specific test needs `resolve_expr` to already dispatch into `Expr::Block` — which isn't implemented until Task 9 either. **Reorder:** move this second test into Task 9 instead (where block resolution exists), and replace it here with a version that doesn't need block nesting:

```rust
#[test]
fn let_x_equals_x_is_an_error() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = x;");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("its own initializer"), "message: {}", errors[0].message);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve let_`
Expected: FAIL — `Stmt::Let` isn't handled in `resolve_stmt` yet, so `x` in `print(x)` is undeclared, and `let x = x;` doesn't produce the specific self-reference error (it either resolves cleanly or errors as plain-undeclared, not with the right message).

- [ ] **Step 3: Implement**

Replace `resolve_stmt`'s catch-all with a `Let` arm:

```rust
fn resolve_stmt(&mut self, idx: Idx<Stmt>) {
    match self.ast.stmt(idx) {
        Stmt::ExprStmt(e) => self.resolve_expr(*e),
        Stmt::Let { name, mutable, init, .. } => {
            let span = self.ast.span_of_stmt(idx);
            let current = self.functions.len() - 1;
            // Declare BEFORE resolving the initializer, uninitialized. This
            // is what makes `let x = x;` see its own not-yet-ready binding
            // (and therefore error) instead of silently falling through to
            // an outer `x` of the same name.
            self.functions[current].declare(*name, *mutable, false, span);
            self.resolve_expr(*init);
            self.functions[current].mark_initialized(*name);
        }
        _ => {} // remaining statement kinds filled in across Tasks 9-13
    }
}
```

And update `resolve_name` to check `initialized` before returning a `Local` resolution:

```rust
fn resolve_name(&mut self, name_sym: ember_ast::Symbol, name_text: &str, span: ember_span::Span) -> Option<crate::binding::Resolution> {
    let current = self.functions.len() - 1;
    if let Some(info) = self.functions[current].lookup_mut(name_sym) {
        if !info.initialized {
            let init_span = info.span;
            self.emit(
                Diagnostic::error(format!("cannot use `{name_text}` in its own initializer"))
                    .with_primary(span, "used here")
                    .with_secondary(init_span, "while initializing this binding"),
            );
            return None;
        }
        info.used = true;
        return Some(crate::binding::Resolution::Local { slot: info.slot });
    }
    if self.functions[0].lookup_in_outermost(name_sym).is_some() {
        return Some(crate::binding::Resolution::Global { symbol: name_sym });
    }
    let suggestion = {
        let names = self.reachable_names();
        closest_match(name_text, names.iter().map(|s| s.as_str())).map(|s| s.to_string())
    };
    let mut diag = Diagnostic::error(format!("undeclared name `{name_text}`")).with_primary(span, "not found in this scope");
    if let Some(sugg) = suggestion {
        diag = diag.with_help(format!("did you mean `{sugg}`?"));
    }
    self.emit(diag);
    None
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Resolve let statements with the initialized-flag self-reference check"
```

---

## Task 8: Assignment resolution — mutability check

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn assignment_to_mutable_binding_is_fine() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let mut x = 1;\nx = 2;");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn assignment_to_immutable_binding_errors_with_a_mut_suggestion() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\nx = 2;");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("immutable"), "message: {}", errors[0].message);
    assert!(errors[0].help.iter().any(|h| h.message.contains("let mut")), "expected a `let mut` suggestion, got: {:?}", errors[0].help);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve assignment`
Expected: FAIL — `Expr::Assign` isn't handled in `resolve_expr` yet.

- [ ] **Step 3: Implement**

Add an `Assign` arm to `resolve_expr` and a new `resolve_assign_target` method:

```rust
// in resolve_expr's match:
Expr::Assign { target, value } => {
    self.resolve_expr(*value);
    self.resolve_assign_target(*target);
}
```

```rust
fn resolve_assign_target(&mut self, idx: Idx<Expr>) {
    match self.ast.expr(idx) {
        Expr::Var(sym) => {
            let span = self.ast.span_of_expr(idx);
            let text = self.interner.resolve(*sym).to_string();
            let current = self.functions.len() - 1;
            if let Some(info) = self.functions[current].lookup_mut(*sym) {
                if !info.initialized {
                    let init_span = info.span;
                    self.emit(
                        Diagnostic::error(format!("cannot use `{text}` in its own initializer"))
                            .with_primary(span, "used here")
                            .with_secondary(init_span, "while initializing this binding"),
                    );
                    return;
                }
                if !info.mutable {
                    let decl_span = info.span;
                    self.emit(
                        Diagnostic::error(format!("cannot assign to immutable variable `{text}`"))
                            .with_primary(span, "assigned here")
                            .with_secondary(decl_span, "declared here")
                            .with_help(format!("consider changing to `let mut {text}`")),
                    );
                    return;
                }
                info.used = true;
                let slot = info.slot;
                self.bindings.resolutions.insert(idx, crate::binding::Resolution::Local { slot });
                return;
            }
            if self.functions[0].lookup_in_outermost(*sym).is_some() {
                self.bindings.resolutions.insert(idx, crate::binding::Resolution::Global { symbol: *sym });
                return;
            }
            self.resolve_name(*sym, &text, span);
        }
        Expr::Index { base, index } => {
            self.resolve_expr(*base);
            self.resolve_expr(*index);
        }
        Expr::Field { base, .. } => {
            self.resolve_expr(*base);
        }
        _ => self.resolve_expr(idx),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Resolve assignment targets with mutability checking"
```

---

## Task 9: Block expressions — nested scopes, shadowing, and the deferred self-reference test

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn nested_scope_shadowing_resolves_to_the_innermost_binding() {
    let src = "let x = 1;\nlet y = { let x = 2; x };\nprint(y);\nprint(x);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn let_x_equals_x_is_an_error_even_with_an_outer_x_in_scope() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\n{ let x = x; }");
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("its own initializer"), "message: {}", errors[0].message);
}

#[test]
fn sibling_blocks_can_each_declare_their_own_locals_without_slot_conflicts() {
    let src = "{ let a = 1; print(a); }\n{ let b = 2; print(b); }";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve nested_scope shadowing sibling_blocks let_x_equals_x_is_an_error_even_with`
Expected: FAIL — `Expr::Block` isn't handled in `resolve_expr` yet, so every `x`/`a`/`b`/`y` reference inside a block is undeclared.

- [ ] **Step 3: Implement**

Add a `Block` arm to `resolve_expr`:

```rust
// in resolve_expr's match:
Expr::Block { stmts, tail } => {
    let current = self.functions.len() - 1;
    self.functions[current].push_scope(crate::scope::ScopeKind::Block);
    for s in stmts {
        self.resolve_stmt(*s);
    }
    if let Some(t) = tail {
        self.resolve_expr(*t);
    }
    self.functions[current].pop_scope();
}
```

(Unused-variable warnings on the popped scope's bindings are added in Task 14 — this task is purely about correct scoping/shadowing/slot behavior.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Resolve block expressions with proper nested-scope shadowing"
```

---

## Task 10: Two-pass top level (forward references) and function/lambda body resolution

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

This is the task that makes `fn`/`type`/`struct` declarations visible regardless of source order (mutual recursion), and makes function/lambda bodies resolve at all — entering a new `FunctionCtx` per function, binding parameters, and recording `frame_sizes`/`upvalues` in `Bindings` once the body is done. It deliberately does **not** yet support a nested function capturing a variable from an *enclosing* function's locals — referencing an outer local from inside a nested function/lambda still resolves as "undeclared" until Task 11 adds real upvalue-chain support. Keep this task's own tests to non-capturing cases only.

- [ ] **Step 1: Write the failing tests**

Note: none of these test programs use `if`/binary operators (`==`, `-`, `+`) on purpose — `Expr::If` and `Expr::Binary` aren't resolved until Task 12, so a test relying on them here would trivially pass without exercising anything (the unhandled node would just be silently skipped by `resolve_expr`'s catch-all). These use only `Var`/`Call`/`Block`/`Lambda`/literals, all already wired up.

```rust
#[test]
fn mutual_recursion_works_regardless_of_declaration_order() {
    let src = "fn is_even(n) { is_odd(n) }\nfn is_odd(n) { is_even(n) }\nprint(is_even(4));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn fn_params_resolve_inside_the_body() {
    let src = "fn add(a, b) { print(a); print(b); a }\nprint(add(1, 2));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn frame_size_and_upvalues_are_recorded_per_function() {
    let src = "fn add(a, b) { print(a); b }";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let (bindings, diags) = resolver.into_bindings();
    assert!(diags.is_empty(), "diags: {diags:?}");
    let fn_id = crate::binding::FunctionId::Fn(stmts[0]);
    assert_eq!(bindings.frame_sizes.get(&fn_id), Some(&2), "two params, both alive at once");
    assert_eq!(bindings.upvalues.get(&fn_id), Some(&vec![]), "non-capturing function has zero upvalues");
}

#[test]
fn non_capturing_lambda_resolves_its_own_params() {
    let src = "let f = |x, y| x;\nprint(f(1, 2));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve mutual_recursion fn_params frame_size non_capturing_lambda`
Expected: FAIL — `is_odd`/`is_even`/`add`/`f` are all undeclared right now, since neither hoisting nor function-body resolution exists yet.

- [ ] **Step 3: Implement**

Add the hoisting pass to `resolve_program`:

```rust
pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
    for &s in stmts {
        match self.ast.stmt(s) {
            Stmt::Fn { name, .. } | Stmt::TypeDecl { name, .. } | Stmt::StructDecl { name, .. } => {
                let span = self.ast.span_of_stmt(s);
                self.functions[0].declare(*name, false, true, span);
            }
            _ => {}
        }
    }
    for &s in stmts {
        self.resolve_stmt(s);
    }
}
```

Add `resolve_function_body` and wire `Stmt::Fn` into `resolve_stmt` and `Expr::Lambda` into `resolve_expr`:

```rust
fn resolve_function_body(&mut self, id: crate::binding::FunctionId, params: &[ember_ast::Param], body: Idx<Expr>) {
    self.functions.push(FunctionCtx::new(id));
    for p in params {
        self.functions.last_mut().unwrap().declare(p.name, false, true, p.span);
    }
    self.resolve_expr(body);
    let fc = self.functions.pop().expect("just pushed a function context");
    self.bindings.frame_sizes.insert(fc.id, fc.high_water);
    self.bindings.upvalues.insert(fc.id, fc.upvalues);
    // Guarantees an entry exists even for a function that captures nothing —
    // Task 11's resolve_upvalue pushes into this map directly at the moment
    // of capture, so this call must not overwrite anything it already added.
    self.bindings.captured_slots.entry(fc.id).or_default();
}
```

In `resolve_stmt`, replace the `Let` arm's sibling catch-all with a `Fn` arm:

```rust
Stmt::Fn { name, params, body, .. } => {
    let current = self.functions.len() - 1;
    let already_hoisted = current == 0 && self.functions[0].lookup_in_outermost(*name).is_some();
    if !already_hoisted {
        let span = self.ast.span_of_stmt(idx);
        self.functions[current].declare(*name, false, true, span);
    }
    self.resolve_function_body(crate::binding::FunctionId::Fn(idx), params, *body);
}
```

(`already_hoisted` is `true` exactly when this `fn` is a genuine top-level declaration whose name Pass 1 already put in `functions[0]`'s outermost scope; a `fn` nested inside a block is never hoisted, so it gets declared fresh in whatever scope it's being resolved in — including correctly handling a `fn` nested in a block at the top level, since `lookup_in_outermost` only ever checks `scopes[0]`, not whichever scope is currently innermost.)

In `resolve_expr`, add:

```rust
Expr::Lambda { params, body } => {
    self.resolve_function_body(crate::binding::FunctionId::Lambda(idx), params, *body);
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add two-pass top-level hoisting and function/lambda body resolution"
```

---

## Task 11: Upvalue resolution — `resolve_upvalue`/`add_upvalue`, threaded capture, deduplication

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

The hardest part of the resolver. Ports `PROMPT.md`'s `resolve_upvalue`/`add_upvalue` algorithm directly, with one addition the reference sketch doesn't need: since assignment targets also have to be upvalues (a closure mutating a captured variable, e.g. a counter), `resolve_upvalue` here returns `(index, was_mutable)` instead of just `index`, so the assignment path can check mutability without a second outward walk.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn counter_closure_captures_exactly_one_upvalue() {
    let src = "fn make_counter() {\n  let mut n = 0;\n  |x| { n = x; n }\n}\nlet c = make_counter();\nprint(c(1));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings, _) = resolver.into_bindings();
    let lambda_upvalues: Vec<_> = bindings
        .upvalues
        .iter()
        .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
        .collect();
    assert_eq!(lambda_upvalues.len(), 1, "exactly one lambda in this program");
    let (_, ups) = lambda_upvalues[0];
    assert_eq!(ups.len(), 1, "the lambda captures exactly one upvalue (n)");
    assert!(ups[0].is_local, "n is captured directly from make_counter's own locals");
}

#[test]
fn triple_nested_capture_threads_through_every_level() {
    let src = "fn outer() {\n  let x = 1;\n  || {\n    || {\n      || { x }\n    }\n  }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings, _) = resolver.into_bindings();
    let lambda_upvalue_counts: Vec<usize> = bindings
        .upvalues
        .iter()
        .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
        .map(|(_, ups)| ups.len())
        .collect();
    assert_eq!(lambda_upvalue_counts.len(), 3, "three nested lambdas");
    assert!(lambda_upvalue_counts.iter().all(|&n| n == 1), "every level threads exactly one upvalue for x: {lambda_upvalue_counts:?}");
}

#[test]
fn capturing_the_same_variable_twice_deduplicates_to_one_upvalue() {
    let src = "fn outer() {\n  let x = 1;\n  || { print(x); x }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings, _) = resolver.into_bindings();
    let lambda_ups: Vec<_> = bindings.upvalues.values().filter(|v| !v.is_empty()).collect();
    assert_eq!(lambda_ups.len(), 1);
    assert_eq!(lambda_ups[0].len(), 1, "x is referenced twice in the body but must dedupe to exactly one upvalue entry");
}

#[test]
fn two_sibling_closures_capturing_the_same_variable_reference_the_same_slot() {
    let src = "fn make_pair() {\n  let mut n = 0;\n  let inc = || { n };\n  let get = || { n };\n  inc\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
    let (bindings, _) = resolver.into_bindings();
    let lambda_ups: Vec<_> = bindings
        .upvalues
        .iter()
        .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
        .map(|(_, ups)| ups.clone())
        .collect();
    assert_eq!(lambda_ups.len(), 2, "two closures");
    assert_eq!(
        lambda_ups[0], lambda_ups[1],
        "both closures capture the same variable, so their upvalue descriptors (same slot index, is_local) must be identical — this is what lets a future VM's capture_upvalue recognize and merge them into one shared cell"
    );
}

#[test]
fn assigning_to_a_mutable_captured_variable_is_fine() {
    let src = "fn make_setter() {\n  let mut n = 0;\n  |v| { n = v; n }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn assigning_to_an_immutable_captured_variable_errors() {
    let src = "fn make_setter() {\n  let n = 0;\n  |v| { n = v; n }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("immutable"), "message: {}", errors[0].message);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve upvalue capture nested_capture deduplicat sibling_closures mutable_captured immutable_captured`
Expected: FAIL — right now a reference to an outer function's local is treated as plain "undeclared" (no upvalue mechanism exists yet), so every one of these programs currently produces an undeclared-name error instead of resolving cleanly.

- [ ] **Step 3: Implement**

Add `resolve_upvalue` and `add_upvalue`:

```rust
impl<'a> Resolver<'a> {
    /// Walk outward through enclosing functions looking for `name`. Each
    /// level that gets passed through must ALSO capture it, forming a
    /// chain, so a variable captured three levels deep threads an upvalue
    /// through every intermediate closure. Returns the new upvalue's index
    /// in `functions[fn_idx]`'s own upvalue list, plus whether the
    /// ORIGINAL captured binding was mutable (needed by assignment-target
    /// resolution, which can't otherwise tell without a second walk).
    fn resolve_upvalue(&mut self, fn_idx: usize, name: ember_ast::Symbol) -> Option<(u32, bool)> {
        if fn_idx == 0 {
            return None; // no enclosing function — must be resolved as a global instead
        }
        // Case 1: a local of the IMMEDIATELY enclosing function.
        if let Some(info) = self.functions[fn_idx - 1].lookup_mut(name) {
            info.captured = true;
            let slot = info.slot;
            let mutable = info.mutable;
            let enclosing_id = self.functions[fn_idx - 1].id;
            self.bindings.captured_slots.entry(enclosing_id).or_default().push(slot);
            return Some((self.add_upvalue(fn_idx, slot, true), mutable));
        }
        // Case 2: further out — recurse, and thread the result through this
        // level too via add_upvalue(is_local: false).
        let (outer_index, mutable) = self.resolve_upvalue(fn_idx - 1, name)?;
        Some((self.add_upvalue(fn_idx, outer_index, false), mutable))
    }

    /// Deduplicates: capturing the same variable twice (or two sibling
    /// closures capturing the same slot) reuses one index rather than
    /// allocating a second, separate upvalue entry.
    fn add_upvalue(&mut self, fn_idx: usize, index: u32, is_local: bool) -> u32 {
        let ups = &mut self.functions[fn_idx].upvalues;
        if let Some(i) = ups.iter().position(|u| u.index == index && u.is_local == is_local) {
            return i as u32;
        }
        ups.push(crate::binding::UpvalueDesc { index, is_local });
        (ups.len() - 1) as u32
    }
}
```

Wire it into `resolve_name` (insert the upvalue check between the local-scope check and the global check):

```rust
fn resolve_name(&mut self, name_sym: ember_ast::Symbol, name_text: &str, span: ember_span::Span) -> Option<crate::binding::Resolution> {
    let current = self.functions.len() - 1;
    if let Some(info) = self.functions[current].lookup_mut(name_sym) {
        if !info.initialized {
            let init_span = info.span;
            self.emit(
                Diagnostic::error(format!("cannot use `{name_text}` in its own initializer"))
                    .with_primary(span, "used here")
                    .with_secondary(init_span, "while initializing this binding"),
            );
            return None;
        }
        info.used = true;
        return Some(crate::binding::Resolution::Local { slot: info.slot });
    }
    if let Some((up_idx, _mutable)) = self.resolve_upvalue(current, name_sym) {
        return Some(crate::binding::Resolution::Upvalue { index: up_idx });
    }
    if self.functions[0].lookup_in_outermost(name_sym).is_some() {
        return Some(crate::binding::Resolution::Global { symbol: name_sym });
    }
    let suggestion = {
        let names = self.reachable_names();
        closest_match(name_text, names.iter().map(|s| s.as_str())).map(|s| s.to_string())
    };
    let mut diag = Diagnostic::error(format!("undeclared name `{name_text}`")).with_primary(span, "not found in this scope");
    if let Some(sugg) = suggestion {
        diag = diag.with_help(format!("did you mean `{sugg}`?"));
    }
    self.emit(diag);
    None
}
```

And into `resolve_assign_target`'s `Var` arm (insert between the local check and the global check):

```rust
fn resolve_assign_target(&mut self, idx: Idx<Expr>) {
    match self.ast.expr(idx) {
        Expr::Var(sym) => {
            let span = self.ast.span_of_expr(idx);
            let text = self.interner.resolve(*sym).to_string();
            let current = self.functions.len() - 1;
            if let Some(info) = self.functions[current].lookup_mut(*sym) {
                if !info.initialized {
                    let init_span = info.span;
                    self.emit(
                        Diagnostic::error(format!("cannot use `{text}` in its own initializer"))
                            .with_primary(span, "used here")
                            .with_secondary(init_span, "while initializing this binding"),
                    );
                    return;
                }
                if !info.mutable {
                    let decl_span = info.span;
                    self.emit(
                        Diagnostic::error(format!("cannot assign to immutable variable `{text}`"))
                            .with_primary(span, "assigned here")
                            .with_secondary(decl_span, "declared here")
                            .with_help(format!("consider changing to `let mut {text}`")),
                    );
                    return;
                }
                info.used = true;
                let slot = info.slot;
                self.bindings.resolutions.insert(idx, crate::binding::Resolution::Local { slot });
                return;
            }
            if let Some((up_idx, mutable)) = self.resolve_upvalue(current, *sym) {
                if !mutable {
                    self.emit(
                        Diagnostic::error(format!("cannot assign to immutable captured variable `{text}`"))
                            .with_primary(span, "assigned here")
                            .with_help(format!("consider changing the outer binding to `let mut {text}`")),
                    );
                    return;
                }
                self.bindings.resolutions.insert(idx, crate::binding::Resolution::Upvalue { index: up_idx });
                return;
            }
            if self.functions[0].lookup_in_outermost(*sym).is_some() {
                self.bindings.resolutions.insert(idx, crate::binding::Resolution::Global { symbol: *sym });
                return;
            }
            self.resolve_name(*sym, &text, span);
        }
        Expr::Index { base, index } => {
            self.resolve_expr(*base);
            self.resolve_expr(*index);
        }
        Expr::Field { base, .. } => {
            self.resolve_expr(*base);
        }
        _ => self.resolve_expr(idx),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green.

Before moving on, hand-trace `triple_nested_capture_threads_through_every_level` against your implementation: confirm that resolving `x` inside the innermost `|| { x }` triggers `resolve_upvalue(4, x)` → recurse to `resolve_upvalue(3, x)` → recurse to `resolve_upvalue(2, x)` → finds `x` as `outer`'s (fn_idx 1) local, marks it captured, and the result threads back out through three `add_upvalue` calls (one per level), each getting its own single-entry upvalue list.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Implement upvalue resolution: resolve_upvalue/add_upvalue with threaded capture and dedup"
```

---

## Task 12: Remaining expression/statement forms and pattern-binding resolution

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

Everything still falling through `resolve_expr`/`resolve_stmt`'s catch-alls: unary/binary operators, index/field reads, `if`, `match` (including pattern-introduced bindings — a match arm is its own little scope), list/struct literals, and the loop/control-flow statements.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn unary_binary_index_field_and_list_struct_all_resolve_their_subexpressions() {
    let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };\nlet xs = [1, 2, 3];\nlet a = -xs[0];\nlet b = p.x;\nprint(a);\nprint(b);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn full_mutual_recursion_with_real_if_and_binary_operators() {
    let src = "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }\nprint(is_even(4));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn match_arm_patterns_introduce_scoped_bindings() {
    let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Point => 0.0,\n  }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn list_pattern_rest_binding_is_usable_in_the_arm_body() {
    let src = "fn describe(xs) {\n  match xs {\n    [head, ..tail] => head,\n    [] => 0,\n  }\n}";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn loop_forms_and_control_flow_statements_all_resolve() {
    let src = "let mut i = 0;\nwhile i == 0 { i = 1; }\nfor x in xs { print(x); }\nloop { break; }\nfn f() { return 1; }";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    // `xs` in the `for` loop is intentionally undeclared here — this test is
    // about every statement FORM being visited (so `i`, `x`, `break`,
    // `return 1` all get resolved without panicking), not about every name
    // in it being declared. Assert specifically that it's the ONLY error.
    let errors: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Error).collect();
    assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(errors[0].message.contains("xs"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve unary_binary full_mutual_recursion match_arm list_pattern_rest loop_forms`
Expected: FAIL — none of these expression/statement forms are resolved yet, so most of these programs currently produce spurious undeclared-name errors (or, for the ones with no name errors at all possible, simply don't exercise anything since the nodes are silently skipped).

- [ ] **Step 3: Implement**

Add these arms to `resolve_expr`'s match, and then delete the trailing `_ => {}` catch-all entirely — between the literal arm (`Int | Float | Bool | Nil | Str | Error`) from Task 6, `Var`/`Call` (Task 6), `Assign` (Task 8), `Block` (Task 9), `Lambda` (Task 10), and these new arms, every `Expr` variant now has an explicit arm, so the match should compile as exhaustive without a catch-all:

```rust
Expr::Unary { operand, .. } => self.resolve_expr(*operand),
Expr::Binary { lhs, rhs, .. } => {
    self.resolve_expr(*lhs);
    self.resolve_expr(*rhs);
}
Expr::Index { base, index } => {
    self.resolve_expr(*base);
    self.resolve_expr(*index);
}
Expr::Field { base, .. } => self.resolve_expr(*base),
Expr::If { cond, then_, else_ } => {
    self.resolve_expr(*cond);
    self.resolve_expr(*then_);
    if let Some(e) = else_ {
        self.resolve_expr(*e);
    }
}
Expr::Match { scrutinee, arms } => {
    self.resolve_expr(*scrutinee);
    for arm in arms {
        self.resolve_match_arm(arm);
    }
}
Expr::List { items } => {
    for i in items {
        self.resolve_expr(*i);
    }
}
Expr::Struct { fields, .. } => {
    for (_, v) in fields {
        self.resolve_expr(*v);
    }
}
```

Add the match-arm and pattern-binding helpers:

```rust
fn resolve_match_arm(&mut self, arm: &ember_ast::MatchArm) {
    let current = self.functions.len() - 1;
    self.functions[current].push_scope(crate::scope::ScopeKind::Match);
    self.declare_pattern_bindings(arm.pat);
    if let Some(g) = arm.guard {
        self.resolve_expr(g);
    }
    self.resolve_expr(arm.body);
    self.functions[current].pop_scope();
}

/// Walks a pattern, declaring every name it binds into the CURRENT (already
/// pushed) scope. Simplification: for or-patterns (`A | B`), every
/// alternative's bindings are declared into the same scope rather than
/// requiring — and cross-checking — that every alternative binds exactly
/// the same names; that fuller check belongs to a later phase with more
/// context (it's a usability nicety, not a soundness issue at this phase).
fn declare_pattern_bindings(&mut self, pat: Idx<ember_ast::Pattern>) {
    use ember_ast::Pattern;
    let span = self.ast.span_of_pat(pat);
    let current = self.functions.len() - 1;
    match self.ast.pat(pat) {
        Pattern::Wild | Pattern::Int(_) | Pattern::Float(_) | Pattern::Str(_) | Pattern::Bool(_) | Pattern::Error => {}
        Pattern::Bind(sym) => {
            self.functions[current].declare(*sym, false, true, span);
        }
        Pattern::Ctor { args, .. } => {
            for a in args {
                self.declare_pattern_bindings(*a);
            }
        }
        Pattern::Tuple(items) => {
            for i in items {
                self.declare_pattern_bindings(*i);
            }
        }
        Pattern::List { items, rest } => {
            for i in items {
                self.declare_pattern_bindings(*i);
            }
            if let Some(r) = rest {
                self.declare_pattern_bindings(*r);
            }
        }
        Pattern::Record { fields, .. } => {
            for (_, p) in fields {
                self.declare_pattern_bindings(*p);
            }
        }
        Pattern::Or(alts) => {
            for a in alts {
                self.declare_pattern_bindings(*a);
            }
        }
    }
}
```

Add these arms to `resolve_stmt`'s match and delete its trailing `_ => {}` catch-all too — combined with `ExprStmt`/`Let` (Task 6-7) and `Fn` (Task 10), every `Stmt` variant now has an explicit arm:

```rust
Stmt::While { cond, body } => {
    self.resolve_expr(*cond);
    self.resolve_expr(*body);
}
Stmt::For { binding, iter, body } => {
    self.resolve_expr(*iter);
    let current = self.functions.len() - 1;
    self.functions[current].push_scope(crate::scope::ScopeKind::Loop);
    // `Stmt::For` doesn't carry a dedicated span for just the loop
    // variable, only for the whole statement — using the whole-statement
    // span here means an unused-loop-variable warning (Task 13) would
    // point at the entire `for` line rather than just the binding
    // identifier. A minor precision gap, not a correctness one.
    let span = self.ast.span_of_stmt(idx);
    self.functions[current].declare(*binding, false, true, span);
    self.resolve_expr(*body);
    self.functions[current].pop_scope();
}
Stmt::Loop { body } => self.resolve_expr(*body),
Stmt::Return(value) => {
    if let Some(v) = value {
        self.resolve_expr(*v);
    }
}
Stmt::Break | Stmt::Continue | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. } | Stmt::Error => {}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green. Run `cargo clippy -p ember-resolve --all-targets -- -D warnings` and `cargo fmt -p ember-resolve -- --check` too — fix anything flagged (the compiler should now also flag if `resolve_expr`/`resolve_stmt`'s `match` is non-exhaustive against every `Expr`/`Stmt` variant — if it complains about a missing arm, that means a variant was missed above; add it).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Resolve remaining expression/statement forms and match-arm pattern bindings"
```

---

## Task 13: Unused-variable/parameter/function warnings

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

Every scope-pop site (block exit, function exit, match-arm exit, for-loop exit) and the end of top-level resolution now needs to check its bindings' `used` flag before they're gone. This is one check reused everywhere — `BindingInfo.used` doesn't distinguish "local variable" from "function name" from "parameter", so one warning message covers all three (matching how `CHECKLIST.md` lists them as one 🟡 group, not three distinct diagnostics).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn unused_local_variable_produces_a_warning() {
    let src = "fn f() { let x = 1; 2 }\nprint(f());";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(warnings[0].message.contains('x'));
}

#[test]
fn underscore_prefixed_names_suppress_the_unused_warning() {
    let src = "fn f() { let _x = 1; 2 }\nprint(f());";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert!(warnings.is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn unused_function_parameter_produces_a_warning() {
    let src = "fn f(a, b) { a }\nprint(f(1, 2));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(warnings[0].message.contains('b'));
}

#[test]
fn unused_top_level_function_produces_a_warning() {
    let src = "fn never_called() { 1 }\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(warnings[0].message.contains("never_called"));
}

#[test]
fn used_variable_produces_no_warning() {
    let src = "fn f() { let x = 1; x }\nprint(f());";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve unused used_variable underscore`
Expected: FAIL — no unused-binding warnings are ever emitted yet, so every test expecting one sees zero.

- [ ] **Step 3: Implement**

Add the shared check:

```rust
/// Checks a set of just-departed bindings for anything never marked
/// `used`, skipping `_`-prefixed names. Takes ownership of the bindings
/// (rather than borrowing) since every call site already has them by
/// value — either freshly popped off a scope stack, or drained out of the
/// top-level scope at the very end of resolution.
fn check_unused(&mut self, bindings: rustc_hash::FxHashMap<ember_ast::Symbol, crate::binding::BindingInfo>) {
    for (sym, info) in bindings {
        if info.used {
            continue;
        }
        let name = self.interner.resolve(sym).to_string();
        if name.starts_with('_') {
            continue;
        }
        self.diagnostics.push(
            Diagnostic::warning(format!("unused variable `{name}`"))
                .with_primary(info.span, "never used")
                .with_help(format!("prefix with an underscore (`_{name}`) if this is intentional")),
        );
    }
}
```

Update the four scope-pop call sites to route through it:

1. In `Expr::Block`'s arm (from Task 9), replace `self.functions[current].pop_scope();` with:
```rust
let popped = self.functions[current].pop_scope();
self.check_unused(popped.bindings);
```

2. In `resolve_function_body` (from Task 10), after popping the `FunctionCtx`, check every scope it still holds (in practice just the outermost parameter scope, since the body's own block scope already popped and got checked via (1) during `self.resolve_expr(body)`):
```rust
fn resolve_function_body(&mut self, id: crate::binding::FunctionId, params: &[ember_ast::Param], body: Idx<Expr>) {
    self.functions.push(FunctionCtx::new(id));
    for p in params {
        self.functions.last_mut().unwrap().declare(p.name, false, true, p.span);
    }
    self.resolve_expr(body);
    let fc = self.functions.pop().expect("just pushed a function context");
    self.bindings.frame_sizes.insert(fc.id, fc.high_water);
    self.bindings.upvalues.insert(fc.id, fc.upvalues);
    self.bindings.captured_slots.entry(fc.id).or_default();
    for scope in fc.scopes {
        self.check_unused(scope.bindings);
    }
}
```

3. In `resolve_match_arm` (from Task 12), replace `self.functions[current].pop_scope();` with the same two-line pattern as (1).

4. In `Stmt::For`'s arm (from Task 12), replace `self.functions[current].pop_scope();` with the same two-line pattern as (1).

Finally, check the top-level scope at the very end of `resolve_program` (top-level `fn`/`let` names live in `functions[0]`'s outermost scope, which is never popped through the normal scope-stack mechanism since function index 0 itself is never popped):

```rust
pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
    for &s in stmts {
        match self.ast.stmt(s) {
            Stmt::Fn { name, .. } | Stmt::TypeDecl { name, .. } | Stmt::StructDecl { name, .. } => {
                let span = self.ast.span_of_stmt(s);
                self.functions[0].declare(*name, false, true, span);
            }
            _ => {}
        }
    }
    for &s in stmts {
        self.resolve_stmt(s);
    }
    let top_level_bindings = std::mem::take(&mut self.functions[0].scopes[0].bindings);
    self.check_unused(top_level_bindings);
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green. Also re-run every EARLIER test in this crate — several used a bare `fn` declaration without calling it (e.g. `frame_size_and_upvalues_are_recorded_per_function`'s `"fn add(a, b) { print(a); b }"`), which will now ALSO emit an unused-function warning for `add` in addition to whatever it was already checking. Any earlier test that asserted `resolver.diagnostics().is_empty()` on a program with an uncalled top-level `fn` will now fail — go back and either call the function in the test's source, or change the assertion to filter for `Severity::Error` specifically (matching the pattern already used by tests that expect warnings alongside a clean bill of health elsewhere in this file) rather than requiring zero diagnostics of any kind. Fix each one you find this way; there's no way to enumerate them all in advance without re-running the full suite.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add unused-variable/parameter/function warnings"
```

---

## Task 14: Unreachable-code-after-return/break/continue warnings

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

Scoped narrowly, matching `CHECKLIST.md`: any statement (or the tail expression) that comes immediately after a `return`/`break`/`continue` **within the same block** is flagged. Not full branch-level dataflow analysis (e.g. detecting that both arms of an `if` return, making code after the `if` unreachable) — see the design doc's Non-goals.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn code_after_return_is_flagged_unreachable() {
    let src = "fn f() { return 1; print(2); print(3) }\nprint(f());";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert_eq!(warnings.len(), 2, "both print(2) and the tail print(3) after the return should be flagged: {:?}", resolver.diagnostics());
    assert!(warnings.iter().all(|w| w.message.contains("unreachable")));
}

#[test]
fn code_before_return_is_not_flagged() {
    let src = "fn f() { print(1); return 2; }\nprint(f());";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    assert!(resolver.diagnostics().is_empty(), "diags: {:?}", resolver.diagnostics());
}

#[test]
fn break_marks_following_code_in_the_same_block_unreachable() {
    let src = "loop { break; print(1); }";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let warnings: Vec<_> = resolver.diagnostics().iter().filter(|d| d.severity == ember_diag::Severity::Warning).collect();
    assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
    assert!(warnings[0].message.contains("unreachable"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve unreachable`
Expected: FAIL — no unreachable-code diagnostics are ever emitted yet.

- [ ] **Step 3: Implement**

Replace the `Expr::Block` arm (from Task 9, already modified once in Task 13 to route through `check_unused`) with a version that also tracks reachability across the statement list:

```rust
Expr::Block { stmts, tail } => {
    let current = self.functions.len() - 1;
    self.functions[current].push_scope(crate::scope::ScopeKind::Block);
    let mut unreachable = false;
    for s in stmts {
        if unreachable {
            let span = self.ast.span_of_stmt(*s);
            self.emit(Diagnostic::warning("unreachable code").with_primary(span, "this code can never run"));
        }
        self.resolve_stmt(*s);
        if matches!(self.ast.stmt(*s), Stmt::Return(_) | Stmt::Break | Stmt::Continue) {
            unreachable = true;
        }
    }
    if let Some(t) = tail {
        if unreachable {
            let span = self.ast.span_of_expr(*t);
            self.emit(Diagnostic::warning("unreachable code").with_primary(span, "this code can never run"));
        }
        self.resolve_expr(*t);
    }
    let popped = self.functions[current].pop_scope();
    self.check_unused(popped.bindings);
}
```

Note this still *resolves* every statement even when flagged unreachable (so nested `Var`s inside dead code are still checked, matching the design doc) — the warning is additive, not a skip.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green. Run `cargo clippy -p ember-resolve --all-targets -- -D warnings` and `cargo fmt -p ember-resolve -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Flag statements after return/break/continue as unreachable"
```

---

## Task 15: Public `resolve()` entry point and crate exports

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`
- Modify: `crates/ember-resolve/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolve_entry_point_ties_everything_together() {
    let src = "fn add(a, b) { print(a); b }\nprint(add(1, 2));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let (bindings, diags) = resolve(&ast, &mut interner, &stmts);
    assert!(diags.is_empty(), "diags: {diags:?}");
    assert!(!bindings.resolutions.is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-resolve resolve_entry_point`
Expected: FAIL — `resolve` (the free function) doesn't exist yet, only `Resolver::new`/`resolve_program`.

- [ ] **Step 3: Implement**

Add to `crates/ember-resolve/src/resolver.rs` (this is the crate's main pipeline entry point, the `ember-resolve` equivalent of `ember_parser::parse`):

```rust
pub fn resolve(ast: &Ast, interner: &mut Interner, stmts: &[Idx<Stmt>]) -> (Bindings, Vec<Diagnostic>) {
    let mut resolver = Resolver::new(ast, interner);
    resolver.resolve_program(stmts);
    resolver.into_bindings()
}
```

`crates/ember-resolve/src/lib.rs`:
```rust
pub mod binding;
pub mod edit_distance;
pub mod resolver;
pub mod scope;

pub use binding::{Bindings, BindingInfo, FunctionId, Resolution, UpvalueDesc};
pub use resolver::{resolve, Resolver};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` too — all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Add public resolve() entry point and finalize crate exports"
```

---

## Task 16: `ember-cli` — `resolve` subcommand

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the dependency**

Add to `crates/ember-cli/Cargo.toml`'s `[dependencies]`:
```toml
ember-resolve = { path = "../ember-resolve" }
```

- [ ] **Step 2: Implement the subcommand**

Add a `Resolve` variant to the `Command` enum:
```rust
/// Print each Var's resolution (local/upvalue/global), per-function
/// upvalue counts, and any resolver diagnostics.
Resolve { file: String },
```

Add its dispatch arm in `main`:
```rust
Command::Resolve { file } => run_resolve(&file),
```

Add the handler function:
```rust
fn run_resolve(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let (bindings, diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);

    let mut resolutions: Vec<_> = bindings.resolutions.iter().collect();
    resolutions.sort_by_key(|(idx, _)| ast.span_of_expr(**idx).start);
    for (idx, res) in resolutions {
        let span = ast.span_of_expr(*idx);
        let desc = match res {
            ember_resolve::Resolution::Local { slot } => format!("local[{slot}]"),
            ember_resolve::Resolution::Upvalue { index } => format!("upvalue[{index}]"),
            ember_resolve::Resolution::Global { symbol } => format!("global({})", interner.resolve(*symbol)),
        };
        println!("{}..{}\t{}", span.start, span.end, desc);
    }

    let mut upvalue_entries: Vec<_> = bindings.upvalues.iter().filter(|(_, ups)| !ups.is_empty()).collect();
    upvalue_entries.sort_by_key(|(id, _)| format!("{id:?}"));
    for (id, ups) in upvalue_entries {
        println!("{id:?}: {} upvalue(s) -> {ups:?}", ups.len());
    }

    print_diagnostics(&diags, path, &src)
}
```

- [ ] **Step 3: Build and manually verify**

Run: `source "$HOME/.cargo/env" && cargo build -p ember-cli`
Expected: builds cleanly.

Run: `cargo run -p ember-cli -- resolve examples/hello.em`
Expected: a line per `Var` resolution (all `local[N]` in this file, since `hello.em` has no closures), plus any diagnostics — `fact`'s recursive self-call inside its own body should resolve as `global(fact)` (top-level `fn` names resolve as globals), and everything else should be `local[N]`. No errors expected; check whether `fact`'s own unused-parameter or unused-variable warnings fire and confirm they're correct given the file's actual content (read `examples/hello.em` first if you don't remember its exact contents).

- [ ] **Step 4: Run the full verification suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-cli
git commit -m "Add ember resolve subcommand for manual resolver inspection"
```

---

## Task 17: Final wrap-up — full verification and CHECKLIST.md update

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full verification suite**

Run: `cargo test --workspace`
Expected: PASS across all 16 crates.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Update `CHECKLIST.md`'s Phase 4 section**

Open `CHECKLIST.md` and go through Phase 4's 22 items line by line, checking `- [x]` for everything this plan actually implemented (which, per the design doc, is all 22 — this phase has no deferred 🟡 items, unlike Phase 0-3). Also double-check the "did you mean?" line explicitly references Levenshtein-distance edit distance (it does, matching the design). If you find any item whose behavior doesn't actually match what's implemented (e.g. re-reading the exact wording turns up a mismatch), leave it unchecked and add a short note explaining the gap, following the same honesty standard the Phase 0-3 wrap-up used — don't block-check the whole phase without verifying each line against the actual code.

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Mark Phase 4 checklist items complete"
```

- [ ] **Step 4: Final confirmation**

Run: `git log --oneline` and confirm a clean, incremental commit history from the `Param`-span fix through to this final checklist update.

---

## Summary of what this plan does NOT cover (by design)

- Type inference, exhaustiveness checking, both execution backends, GC, formatter, LSP, WASM bindings, playground — Phases 5-17, each gets its own design/plan cycle.
- Full branch-level dataflow analysis for unreachable code (only the direct-successor-of-return/break/continue case).
- Cross-checking that every alternative of an or-pattern binds the same names.
- Consuming `Bindings` from an actual interpreter or compiler — Phase 7/8 will define exactly what shape they need from `captured_slots` in particular (see the design doc's "Ambiguity resolved" note).

