# ember Phase 5 Implementation Plan — Type Inference

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Hindley-Milner type inference for `ember` as a new `ember-types` crate — constraint generation with immediate (eager) unification carrying provenance (`Origin`), the occurs check, let-polymorphism via generalize/instantiate, the value restriction, nominal ADT/struct typing, pattern typing, and an `ember-cli typecheck` subcommand. Fixes a pre-existing resolver gap (ADT variant constructors never being declared as resolvable names) surfaced while scoping this phase.

**Architecture:** `ember-types` is self-contained: it builds its own `Symbol -> Scheme` environment (`TyEnv`) by walking scopes during inference, independent of `ember-resolve`'s `Bindings`. Unification is eager — each `unify(a, b, origin)` call resolves and binds immediately, matching the literal function signature given in `SPEC.md`/`PROMPT.md`; "generation separate from solving" is achieved by every unify call carrying an `Origin`, not by batching all constraints to the end. Field access is the one exception requiring deferral (its base type may still be unresolved when the `Field` node is visited) — those obligations are collected during the walk and resolved in one pass immediately after the whole program has been walked.

**Tech Stack:** Rust, `rustc-hash::FxHashMap`, a `Vec<Option<Ty>>`-backed substitution store with path compression.

---

## Task 1: Resolver fix — declare ADT variant constructor names as globals

**Files:**
- Modify: `crates/ember-resolve/src/resolver.rs`

`Circle(3.0)` parses as `Expr::Call{callee: Expr::Var(Circle), args}`. `resolve_program`'s two-pass hoisting currently only declares the *type* name (`Shape`) from `Stmt::TypeDecl`, never each `AdtVariant`'s own name — so constructing any ADT variant via a call expression currently fails with "undeclared name". Patterns never hit this (they don't route constructor names through `resolve_name`), which is why no earlier test caught it. This blocks this phase's own requirement that ADT constructors type as functions, so it's fixed first, in isolation.

- [ ] **Step 1: Write the failing test**

Add to `crates/ember-resolve/src/resolver.rs`'s test module:

```rust
#[test]
fn adt_variant_constructor_names_resolve_as_globals() {
    let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nlet c = Circle(3.0);\nlet p = Point;\nprint(c);\nprint(p);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
    let mut resolver = Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let errors: Vec<_> = resolver
        .diagnostics()
        .iter()
        .filter(|d| d.severity == ember_diag::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p ember-resolve adt_variant_constructor`
Expected: FAIL with an "undeclared name `Circle`" diagnostic.

- [ ] **Step 3: Implement**

In `resolve_program`, split the `Stmt::TypeDecl` arm out of the combined match so its variants can be walked too:

```rust
pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
    for &s in stmts {
        match self.ast.stmt(s) {
            Stmt::Fn { name, .. } | Stmt::StructDecl { name, .. } => {
                let span = self.ast.span_of_stmt(s);
                self.functions[0].declare(*name, false, true, span);
            }
            Stmt::TypeDecl { name, variants } => {
                let span = self.ast.span_of_stmt(s);
                self.functions[0].declare(*name, false, true, span);
                for variant in variants {
                    self.functions[0].declare(variant.name, false, true, span);
                }
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

Each variant is declared using the `TypeDecl` statement's own span, since `AdtVariant` doesn't carry a separate span of its own (the same span-precision simplification already used elsewhere, e.g. `Stmt::For`'s loop-variable binding).

Note this is a source of new (harmless) unused-variable warnings: a program that only ever uses a constructor in *pattern* position (never constructs it) will now see it flagged unused, since pattern matching still doesn't mark names used. This is the same class of false positive the project already tolerates for type/struct names (see `match_arm_patterns_introduce_scoped_bindings`, which already filters for errors only) — not a new problem this fix introduces, just newly-visible for variant names specifically.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-resolve`
Expected: PASS, all tests green, including the new one. Run `cargo clippy -p ember-resolve --all-targets -- -D warnings` and `cargo fmt -p ember-resolve -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-resolve
git commit -m "Declare ADT variant constructor names during top-level hoisting"
```

---

## Task 2: Scaffold the `ember-types` crate

**Files:**
- Modify: `crates/ember-types/Cargo.toml`
- Modify: `crates/ember-types/src/lib.rs`

- [ ] **Step 1: Write the manifest**

Replace `crates/ember-types/Cargo.toml`:

```toml
[package]
name = "ember-types"
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

- [ ] **Step 2: Declare the module layout**

Replace `crates/ember-types/src/lib.rs`:

```rust
pub mod adt;
pub mod constraint;
pub mod display;
pub mod env;
pub mod infer;
pub mod subst;
pub mod trace;
pub mod ty;
```

(`pub use` re-exports land in Task 18, once every module actually has something worth re-exporting.)

- [ ] **Step 3: Verify the crate builds**

Run: `source "$HOME/.cargo/env" && cargo build -p ember-types`
Expected: fails to compile (the `pub mod` lines reference files that don't exist yet as anything but empty stubs) — create empty files so it builds clean before moving on:

```bash
touch crates/ember-types/src/adt.rs crates/ember-types/src/constraint.rs crates/ember-types/src/display.rs crates/ember-types/src/env.rs crates/ember-types/src/infer.rs crates/ember-types/src/subst.rs crates/ember-types/src/trace.rs crates/ember-types/src/ty.rs
```

Run: `cargo build -p ember-types`
Expected: builds cleanly (empty modules).

- [ ] **Step 4: Commit**

```bash
git add crates/ember-types Cargo.lock
git commit -m "Scaffold ember-types crate module layout"
```

---

## Task 3: `ty.rs` — `Ty`, `TyVarId`, `Scheme`

**Files:**
- Modify: `crates/ember-types/src/ty.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_var_equality_is_by_id() {
        assert_eq!(Ty::Var(TyVarId(0)), Ty::Var(TyVarId(0)));
        assert_ne!(Ty::Var(TyVarId(0)), Ty::Var(TyVarId(1)));
    }

    #[test]
    fn fun_and_list_compare_structurally() {
        let a = Ty::Fun(vec![Ty::Int], Box::new(Ty::Bool));
        let b = Ty::Fun(vec![Ty::Int], Box::new(Ty::Bool));
        let c = Ty::Fun(vec![Ty::Float], Box::new(Ty::Bool));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(Ty::List(Box::new(Ty::Int)), Ty::List(Box::new(Ty::Int)));
    }

    #[test]
    fn scheme_with_no_vars_is_monomorphic() {
        let s = Scheme { vars: vec![], ty: Ty::Int };
        assert!(s.vars.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p ember-types ty_var_equality fun_and_list scheme_with_no_vars`
Expected: FAIL to compile — `Ty`/`TyVarId`/`Scheme` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::Symbol;
use rustc_hash::FxHashMap;

/// An index into the substitution store's `Vec<Option<Ty>>` — a unification
/// variable, i.e. a "hole" that inference will progressively pin down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyVarId(pub u32);

/// `AdtId` identifies one user-declared type (either an ADT/enum from
/// `Stmt::TypeDecl` or a struct from `Stmt::StructDecl`) — both are nominal,
/// registered in `adt::AdtRegistry`. Defined here (not in `adt.rs`) since
/// `Ty` needs it and `adt.rs` needs `Ty`; breaking the cycle by keeping the
/// identifier itself in `ty.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AdtId(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    String,
    Unit,
    /// A unification variable — a hole inference will fill in.
    Var(TyVarId),
    Fun(Vec<Ty>, Box<Ty>),
    List(Box<Ty>),
    /// A user-declared ADT or struct. `Vec<Ty>` is always empty this phase —
    /// the grammar has no generic type-parameter syntax on `Stmt::TypeDecl`
    /// or `Stmt::StructDecl` to parameterize over. Kept as `Vec<Ty>` (not
    /// dropped) so a future phase can add generics without reshaping `Ty`.
    Adt(AdtId, Vec<Ty>),
    /// An anonymous structural record. Present to match the type this
    /// phase's checklist literally enumerates, but nothing in the current
    /// grammar produces one — named struct types go through `Ty::Adt`
    /// instead, so instances are nominal, not structural. Reserved for a
    /// future anonymous-record-type feature. `FxHashMap`, not `BTreeMap`,
    /// to avoid depending on `Symbol: Ord` (not guaranteed by
    /// `string-interner`'s `DefaultSymbol`).
    Record(FxHashMap<Symbol, Ty>),
}

/// A type SCHEME is a type with universally quantified variables — what
/// makes let-polymorphism work. `identity` is stored as `∀a. a -> a`, and
/// each use instantiates fresh variables, so `identity(1)` and
/// `identity("x")` don't conflict.
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<TyVarId>,
    pub ty: Ty,
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add Ty, TyVarId, AdtId, and Scheme types"
```

---

## Task 4: `adt.rs` — `AdtRegistry`

**Files:**
- Modify: `crates/ember-types/src/adt.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::Ty;
    use ember_ast::Interner;

    #[test]
    fn register_enum_and_look_up_a_variant() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let point = interner.intern("Point");
        let mut reg = AdtRegistry::new();
        let id = reg.register_enum(
            shape,
            vec![(circle, vec![Ty::Float]), (point, vec![])],
        );
        assert_eq!(reg.id_of(shape), Some(id));
        assert_eq!(reg.name_of(id), shape);
        let (owner, payload) = reg.variant(circle).expect("Circle should be registered");
        assert_eq!(owner, id);
        assert_eq!(payload, &[Ty::Float]);
    }

    #[test]
    fn register_struct_and_look_up_a_field() {
        let mut interner = Interner::new();
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let mut reg = AdtRegistry::new();
        let id = reg.register_struct(point, vec![(x, Ty::Float)]);
        assert_eq!(reg.field_ty(id, x), Some(&Ty::Float));
        assert_eq!(reg.field_ty(id, interner.intern("y")), None);
    }

    #[test]
    fn is_struct_distinguishes_declaration_kinds() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let point = interner.intern("Point");
        let mut reg = AdtRegistry::new();
        let enum_id = reg.register_enum(shape, vec![]);
        let struct_id = reg.register_struct(point, vec![]);
        assert!(!reg.is_struct(enum_id));
        assert!(reg.is_struct(struct_id));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types register_enum register_struct is_struct`
Expected: FAIL to compile — `AdtRegistry` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::ty::{AdtId, Ty};
use ember_ast::Symbol;
use rustc_hash::FxHashMap;

pub enum AdtDecl {
    Enum {
        /// Ordered so error messages ("missing variant Circle, Rect")
        /// can report in declaration order — Phase 6's job, but the data
        /// is here now.
        variants: Vec<(Symbol, Vec<Ty>)>,
    },
    Struct {
        fields: FxHashMap<Symbol, Ty>,
    },
}

#[derive(Default)]
pub struct AdtRegistry {
    by_name: FxHashMap<Symbol, AdtId>,
    /// Maps a variant's own name straight to (owning AdtId, its payload
    /// types) — variant names are unique across the whole program, same as
    /// top-level fn/type/struct names, so this flat map is enough without
    /// needing to know the owning type up front.
    variants: FxHashMap<Symbol, (AdtId, Vec<Ty>)>,
    decls: Vec<AdtDecl>,
    names: Vec<Symbol>,
}

impl AdtRegistry {
    pub fn new() -> Self {
        AdtRegistry::default()
    }

    pub fn register_enum(&mut self, name: Symbol, variants: Vec<(Symbol, Vec<Ty>)>) -> AdtId {
        let id = AdtId(self.decls.len() as u32);
        for (variant_name, payload) in &variants {
            self.variants.insert(*variant_name, (id, payload.clone()));
        }
        self.decls.push(AdtDecl::Enum { variants });
        self.names.push(name);
        self.by_name.insert(name, id);
        id
    }

    pub fn register_struct(&mut self, name: Symbol, fields: Vec<(Symbol, Ty)>) -> AdtId {
        let id = AdtId(self.decls.len() as u32);
        self.decls.push(AdtDecl::Struct {
            fields: fields.into_iter().collect(),
        });
        self.names.push(name);
        self.by_name.insert(name, id);
        id
    }

    pub fn id_of(&self, name: Symbol) -> Option<AdtId> {
        self.by_name.get(&name).copied()
    }

    pub fn name_of(&self, id: AdtId) -> Symbol {
        self.names[id.0 as usize]
    }

    pub fn is_struct(&self, id: AdtId) -> bool {
        matches!(self.decls[id.0 as usize], AdtDecl::Struct { .. })
    }

    /// Looks up a variant BY ITS OWN NAME (e.g. `Circle`), returning the
    /// enum type it belongs to and its declared payload types.
    pub fn variant(&self, name: Symbol) -> Option<(AdtId, &[Ty])> {
        self.variants.get(&name).map(|(id, payload)| (*id, payload.as_slice()))
    }

    pub fn field_ty(&self, id: AdtId, field: Symbol) -> Option<&Ty> {
        match &self.decls[id.0 as usize] {
            AdtDecl::Struct { fields } => fields.get(&field),
            AdtDecl::Enum { .. } => None,
        }
    }

    /// All field names of a struct, for detecting an unknown field name in
    /// a struct literal and for listing missing ones.
    pub fn struct_fields(&self, id: AdtId) -> impl Iterator<Item = Symbol> + '_ {
        match &self.decls[id.0 as usize] {
            AdtDecl::Struct { fields } => Some(fields.keys().copied()),
            AdtDecl::Enum { .. } => None,
        }
        .into_iter()
        .flatten()
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add AdtRegistry for nominal enum and struct types"
```

---

## Task 5: `subst.rs` — the substitution store

**Files:**
- Modify: `crates/ember-types/src/subst.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::Ty;

    #[test]
    fn fresh_vars_are_distinct_and_unresolved() {
        let mut s = Subst::new();
        let a = s.fresh();
        let b = s.fresh();
        assert_ne!(a, b);
        assert_eq!(s.resolve(&a), a);
    }

    #[test]
    fn binding_a_var_makes_resolve_follow_it() {
        let mut s = Subst::new();
        let a = s.fresh();
        let Ty::Var(id) = a else { panic!("fresh() must return Ty::Var") };
        s.bind(id, Ty::Int);
        assert_eq!(s.resolve(&a), Ty::Int);
    }

    #[test]
    fn resolve_follows_a_chain_of_bound_vars() {
        let mut s = Subst::new();
        let a = s.fresh();
        let b = s.fresh();
        let (Ty::Var(a_id), Ty::Var(_)) = (&a, &b) else { unreachable!() };
        s.bind(*a_id, b.clone());
        let Ty::Var(b_id) = b else { unreachable!() };
        s.bind(b_id, Ty::Bool);
        assert_eq!(s.resolve(&a), Ty::Bool);
    }

    #[test]
    fn resolve_recurses_into_compound_types() {
        let mut s = Subst::new();
        let a = s.fresh();
        let Ty::Var(id) = a.clone() else { unreachable!() };
        s.bind(id, Ty::Int);
        let list = Ty::List(Box::new(a));
        assert_eq!(s.resolve(&list), Ty::List(Box::new(Ty::Int)));
    }

    #[test]
    fn occurs_check_detects_self_reference_through_a_fun_type() {
        let mut s = Subst::new();
        let a = s.fresh();
        let Ty::Var(id) = a.clone() else { unreachable!() };
        let self_referential = Ty::Fun(vec![a.clone()], Box::new(Ty::Bool));
        assert!(s.occurs_in(id, &self_referential));
        assert!(!s.occurs_in(id, &Ty::Int));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types fresh_vars binding_a_var resolve_follows resolve_recurses occurs_check`
Expected: FAIL to compile — `Subst` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::ty::{Ty, TyVarId};

/// The union-find-style substitution store: `bindings[v.0]` is `Some(ty)`
/// once `v` has been bound to a concrete (or partially concrete) type, or
/// `None` while it's still an open hole.
#[derive(Default)]
pub struct Subst {
    bindings: Vec<Option<Ty>>,
}

impl Subst {
    pub fn new() -> Self {
        Subst::default()
    }

    pub fn fresh(&mut self) -> Ty {
        let id = TyVarId(self.bindings.len() as u32);
        self.bindings.push(None);
        Ty::Var(id)
    }

    pub fn bind(&mut self, v: TyVarId, ty: Ty) {
        self.bindings[v.0 as usize] = Some(ty);
    }

    /// Follows a chain of bound variables to the current representative
    /// type, recursing into compound types so nested vars resolve too.
    /// Compresses the chain as it walks (classic union-find path
    /// compression) so repeated resolves of the same var are O(1) after
    /// the first.
    pub fn resolve(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(v) => match self.bindings[v.0 as usize].clone() {
                Some(inner) => {
                    let resolved = self.resolve(&inner);
                    self.bindings[v.0 as usize] = Some(resolved.clone());
                    resolved
                }
                None => ty.clone(),
            },
            Ty::Fun(params, ret) => Ty::Fun(
                params.iter().map(|p| self.resolve(p)).collect(),
                Box::new(self.resolve(ret)),
            ),
            Ty::List(t) => Ty::List(Box::new(self.resolve(t))),
            Ty::Adt(id, args) => Ty::Adt(*id, args.iter().map(|a| self.resolve(a)).collect()),
            Ty::Record(fields) => {
                Ty::Record(fields.iter().map(|(k, v)| (*k, self.resolve(v))).collect())
            }
            _ => ty.clone(),
        }
    }

    /// True if `var` appears anywhere inside `ty` (after following bound
    /// variables). Must run before every var-to-type binding: without it,
    /// `a := a -> b` (from `let f = |x| f(x)`) creates an infinite type
    /// that every later substitution expands further, hanging the
    /// compiler. Three lines. Non-optional.
    pub fn occurs_in(&mut self, var: TyVarId, ty: &Ty) -> bool {
        match self.resolve(ty) {
            Ty::Var(v) => v == var,
            Ty::Fun(ps, r) => ps.iter().any(|p| self.occurs_in(var, p)) || self.occurs_in(var, &r),
            Ty::List(t) => self.occurs_in(var, &t),
            Ty::Adt(_, args) => args.iter().any(|a| self.occurs_in(var, a)),
            Ty::Record(fs) => fs.values().any(|t| self.occurs_in(var, t)),
            _ => false,
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add the substitution store: fresh vars, resolve with path compression, occurs check"
```

---

## Task 6: `display.rs` — pretty-printing types and schemes

**Files:**
- Modify: `crates/ember-types/src/display.rs`

Free functions, not `impl Display`, since rendering needs the substitution (to resolve remaining vars), the `AdtRegistry` (to print a user type's name), and the `Interner` (to resolve `Symbol`s) — none of which a bare `&self` method can access. Every free variable gets a fresh, message-local `a`, `b`, `c`, … name (not a global numbering), matching how compilers usually render inference errors.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::subst::Subst;
    use crate::ty::{Scheme, Ty};
    use ember_ast::Interner;

    #[test]
    fn concrete_types_print_plainly() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        assert_eq!(display_ty(&Ty::Int, &mut subst, &adts, &interner), "Int");
        assert_eq!(
            display_ty(&Ty::List(Box::new(Ty::Bool)), &mut subst, &adts, &interner),
            "[Bool]"
        );
        assert_eq!(
            display_ty(
                &Ty::Fun(vec![Ty::Int, Ty::Int], Box::new(Ty::Bool)),
                &mut subst,
                &adts,
                &interner
            ),
            "(Int, Int) -> Bool"
        );
    }

    #[test]
    fn unbound_vars_get_readable_letter_names() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let a = subst.fresh();
        let b = subst.fresh();
        let fun = Ty::Fun(vec![a, b.clone()], Box::new(b));
        assert_eq!(display_ty(&fun, &mut subst, &adts, &interner), "(a, b) -> b");
    }

    #[test]
    fn named_adt_prints_its_declared_name() {
        let mut subst = Subst::new();
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![]);
        assert_eq!(
            display_ty(&Ty::Adt(id, vec![]), &mut subst, &adts, &interner),
            "Shape"
        );
    }

    #[test]
    fn scheme_prints_its_quantifier() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let a = subst.fresh();
        let crate::ty::Ty::Var(id) = a.clone() else { unreachable!() };
        let scheme = Scheme {
            vars: vec![id],
            ty: Ty::Fun(vec![a.clone()], Box::new(a)),
        };
        assert_eq!(
            display_scheme(&scheme, &mut subst, &adts, &interner),
            "forall a. a -> a"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types concrete_types_print unbound_vars named_adt scheme_prints`
Expected: FAIL to compile — `display_ty`/`display_scheme` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::adt::AdtRegistry;
use crate::subst::Subst;
use crate::ty::{Scheme, Ty, TyVarId};
use ember_ast::Interner;
use rustc_hash::FxHashMap;

pub fn display_ty(ty: &Ty, subst: &mut Subst, adts: &AdtRegistry, interner: &Interner) -> String {
    let resolved = subst.resolve(ty);
    let mut names = FxHashMap::default();
    fmt_ty(&resolved, &mut names, adts, interner)
}

pub fn display_scheme(
    scheme: &Scheme,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> String {
    let resolved = subst.resolve(&scheme.ty);
    if scheme.vars.is_empty() {
        return fmt_ty(&resolved, &mut FxHashMap::default(), adts, interner);
    }
    let mut names = FxHashMap::default();
    let quantifier: Vec<String> = scheme
        .vars
        .iter()
        .map(|v| var_name(*v, &mut names))
        .collect();
    format!(
        "forall {}. {}",
        quantifier.join(" "),
        fmt_ty(&resolved, &mut names, adts, interner)
    )
}

fn var_name(v: TyVarId, names: &mut FxHashMap<TyVarId, String>) -> String {
    let next = names.len();
    names
        .entry(v)
        .or_insert_with(|| {
            let letter = (b'a' + (next % 26) as u8) as char;
            let suffix = next / 26;
            if suffix == 0 {
                letter.to_string()
            } else {
                format!("{letter}{suffix}")
            }
        })
        .clone()
}

fn fmt_ty(ty: &Ty, names: &mut FxHashMap<TyVarId, String>, adts: &AdtRegistry, interner: &Interner) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Var(v) => var_name(*v, names),
        Ty::List(t) => format!("[{}]", fmt_ty(t, names, adts, interner)),
        Ty::Fun(params, ret) => {
            let params_str: Vec<String> = params.iter().map(|p| fmt_ty(p, names, adts, interner)).collect();
            format!("({}) -> {}", params_str.join(", "), fmt_ty(ret, names, adts, interner))
        }
        Ty::Adt(id, _args) => interner.resolve(adts.name_of(*id)).to_string(),
        Ty::Record(fields) => {
            let mut entries: Vec<(String, String)> = fields
                .iter()
                .map(|(k, v)| (interner.resolve(*k).to_string(), fmt_ty(v, names, adts, interner)))
                .collect();
            entries.sort();
            let body: Vec<String> = entries.into_iter().map(|(k, v)| format!("{k}: {v}")).collect();
            format!("{{{}}}", body.join(", "))
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add type and scheme pretty-printing with readable variable names"
```

---

## Task 7: `constraint.rs` and `trace.rs` — `Origin` and the inference trace

**Files:**
- Modify: `crates/ember-types/src/constraint.rs`
- Modify: `crates/ember-types/src/trace.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/ember-types/src/constraint.rs
#[cfg(test)]
mod tests {
    use super::*;
    use ember_span::Span;

    #[test]
    fn origin_variants_carry_their_spans() {
        let o = Origin::IfBranches {
            if_span: Span::new(0, 2),
            then_span: Span::new(3, 4),
            else_span: Span::new(5, 6),
        };
        match o {
            Origin::IfBranches { if_span, .. } => assert_eq!(if_span, Span::new(0, 2)),
            _ => unreachable!(),
        }
    }
}
```

```rust
// crates/ember-types/src/trace.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraint::{Constraint, Origin};
    use crate::ty::Ty;
    use ember_span::Span;

    #[test]
    fn trace_accumulates_steps_in_order() {
        let mut trace = InferenceTrace::default();
        trace.record(
            Constraint { lhs: Ty::Int, rhs: Ty::Int, origin: Origin::WhileCond { span: Span::new(0, 1) } },
            true,
        );
        trace.record(
            Constraint { lhs: Ty::Bool, rhs: Ty::Int, origin: Origin::WhileCond { span: Span::new(2, 3) } },
            false,
        );
        assert_eq!(trace.steps.len(), 2);
        assert!(trace.steps[0].succeeded);
        assert!(!trace.steps[1].succeeded);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types origin_variants trace_accumulates`
Expected: FAIL to compile — `Origin`, `Constraint`, `InferenceTrace`, `UnifyStep` don't exist yet.

- [ ] **Step 3: Implement**

```rust
// crates/ember-types/src/constraint.rs
use crate::ty::Ty;
use ember_ast::Symbol;
use ember_span::Span;

/// Every comparison unify() is asked to make carries WHY the two types are
/// expected to match — this is what lets error messages say "these two
/// `if` branches disagree" instead of "Int != String" with no context.
#[derive(Debug, Clone)]
pub struct Constraint {
    pub lhs: Ty,
    pub rhs: Ty,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub enum Origin {
    IfBranches { if_span: Span, then_span: Span, else_span: Span },
    CallArgument { call_span: Span, arg_span: Span, param_idx: usize, fn_name: Option<Symbol> },
    BinaryOp { op_span: Span, lhs_span: Span, rhs_span: Span },
    Annotation { annot_span: Span, value_span: Span },
    MatchArms { first_span: Span, this_span: Span },
    Return { fn_span: Span, expr_span: Span },
    ListElement { list_span: Span, elem_span: Span, index: usize },
    WhileCond { span: Span },
    IndexTarget { span: Span },
}

impl Origin {
    /// A best-effort single span for diagnostics that don't need every
    /// contributing span individually labeled (e.g. the field-access
    /// "needs annotation" error, which has no dedicated Origin variant).
    pub fn primary_span(&self) -> Span {
        match self {
            Origin::IfBranches { if_span, .. } => *if_span,
            Origin::CallArgument { arg_span, .. } => *arg_span,
            Origin::BinaryOp { op_span, .. } => *op_span,
            Origin::Annotation { value_span, .. } => *value_span,
            Origin::MatchArms { this_span, .. } => *this_span,
            Origin::Return { expr_span, .. } => *expr_span,
            Origin::ListElement { elem_span, .. } => *elem_span,
            Origin::WhileCond { span } => *span,
            Origin::IndexTarget { span } => *span,
        }
    }
}
```

```rust
// crates/ember-types/src/trace.rs
use crate::constraint::{Constraint, Origin};
use crate::ty::Ty;

/// One attempted unification, recorded in execution order — the data
/// behind the playground's Panel 4 (no consumer exists yet; built now per
/// explicit scope decision for this phase).
#[derive(Debug, Clone)]
pub struct UnifyStep {
    pub lhs: Ty,
    pub rhs: Ty,
    pub origin: Origin,
    pub succeeded: bool,
}

#[derive(Default)]
pub struct InferenceTrace {
    pub steps: Vec<UnifyStep>,
}

impl InferenceTrace {
    pub fn record(&mut self, constraint: Constraint, succeeded: bool) {
        self.steps.push(UnifyStep {
            lhs: constraint.lhs,
            rhs: constraint.rhs,
            origin: constraint.origin,
            succeeded,
        });
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add Constraint, Origin, and the inference trace"
```

---

## Task 8: `unify.rs` — unification with the occurs check

**Files:**
- Modify: `crates/ember-types/src/unify.rs`
- Modify: `crates/ember-types/src/lib.rs`

**Step 0:** Add `pub mod unify;` to `crates/ember-types/src/lib.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::constraint::Origin;
    use crate::subst::Subst;
    use crate::trace::InferenceTrace;
    use crate::ty::Ty;
    use ember_ast::Interner;
    use ember_span::Span;

    fn origin() -> Origin {
        Origin::WhileCond { span: Span::new(0, 1) }
    }

    #[test]
    fn identical_concrete_types_unify() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(unify(&Ty::Int, &Ty::Int, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        assert!(diags.is_empty());
    }

    #[test]
    fn mismatched_concrete_types_fail_with_a_diagnostic() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(!unify(&Ty::Int, &Ty::Bool, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn a_var_binds_to_a_concrete_type() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = subst.fresh();
        assert!(unify(&a, &Ty::Int, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        assert_eq!(subst.resolve(&a), Ty::Int);
    }

    #[test]
    fn occurs_check_rejects_an_infinite_type() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = subst.fresh();
        let self_fun = Ty::Fun(vec![a.clone()], Box::new(Ty::Bool));
        assert!(!unify(&a, &self_fun, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        assert!(diags[0].message.to_lowercase().contains("infinite"));
    }

    #[test]
    fn fun_arity_mismatch_is_a_dedicated_error() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        let a = Ty::Fun(vec![Ty::Int, Ty::Int], Box::new(Ty::Bool));
        let b = Ty::Fun(vec![Ty::Int], Box::new(Ty::Bool));
        assert!(!unify(&a, &b, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        assert!(diags[0].message.contains("2") && diags[0].message.contains("1"));
    }

    #[test]
    fn list_and_adt_unify_structurally() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![]);
        let mut subst = Subst::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(unify(
            &Ty::List(Box::new(Ty::Int)),
            &Ty::List(Box::new(Ty::Int)),
            origin(),
            &mut subst,
            &adts,
            &interner,
            &mut trace,
            &mut diags
        ));
        assert!(unify(&Ty::Adt(id, vec![]), &Ty::Adt(id, vec![]), origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
    }

    #[test]
    fn int_float_mismatch_suggests_a_conversion() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        assert!(!unify(&Ty::Int, &Ty::Float, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags));
        let help = diags[0].help.as_ref().expect("expected a help suggestion");
        assert!(help.to_lowercase().contains("convert") || help.to_lowercase().contains("int(") || help.to_lowercase().contains("float("));
    }

    #[test]
    fn every_attempt_is_recorded_in_the_trace() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let mut trace = InferenceTrace::default();
        let mut diags = Vec::new();
        unify(&Ty::Int, &Ty::Int, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags);
        unify(&Ty::Int, &Ty::Bool, origin(), &mut subst, &adts, &interner, &mut trace, &mut diags);
        assert_eq!(trace.steps.len(), 2);
    }
}
```

Check `ember_diag::Diagnostic`'s exact field names for `message`/`help` before writing these assertions (`grep -n "pub struct Diagnostic" -A 20 crates/ember-diag/src/lib.rs`) — adjust field access if the real struct differs (e.g. `help` might be `Option<String>` behind a different field name or require a getter).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types identical_concrete mismatched_concrete a_var_binds occurs_check fun_arity list_and_adt int_float every_attempt`
Expected: FAIL to compile — `unify` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::adt::AdtRegistry;
use crate::constraint::{Constraint, Origin};
use crate::display::display_ty;
use crate::subst::Subst;
use crate::trace::InferenceTrace;
use crate::ty::Ty;
use ember_ast::Interner;
use ember_diag::Diagnostic;

/// Eagerly unifies `a` and `b`, resolving both through `subst` first and
/// binding immediately on success. Every call is self-contained (no
/// deferred queue) — "constraint generation separate from solving" here
/// means every comparison carries an `Origin`, not that solving is batched
/// to the end. Returns `true` on success; on failure, pushes exactly one
/// diagnostic and returns `false` so the caller can substitute a recovery
/// type and keep going rather than aborting the whole inference pass.
#[allow(clippy::too_many_arguments)]
pub fn unify(
    a: &Ty,
    b: &Ty,
    origin: Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
    trace: &mut InferenceTrace,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let ra = subst.resolve(a);
    let rb = subst.resolve(b);
    let constraint = Constraint { lhs: ra.clone(), rhs: rb.clone(), origin: origin.clone() };

    let ok = unify_resolved(&ra, &rb, &origin, subst, adts, interner, diagnostics);
    trace.record(constraint, ok);
    ok
}

#[allow(clippy::too_many_arguments)]
fn unify_resolved(
    a: &Ty,
    b: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match (a, b) {
        (Ty::Var(v1), Ty::Var(v2)) if v1 == v2 => true,

        (Ty::Var(v), t) | (t, Ty::Var(v)) => {
            if subst.occurs_in(*v, t) {
                diagnostics.push(infinite_type_error(*v, t, origin, subst, adts, interner));
                return false;
            }
            subst.bind(*v, t.clone());
            true
        }

        (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
            if p1.len() != p2.len() {
                diagnostics.push(arity_error(p1.len(), p2.len(), origin));
                return false;
            }
            let mut ok = true;
            for (x, y) in p1.iter().zip(p2) {
                ok &= unify_resolved(x, y, origin, subst, adts, interner, diagnostics);
            }
            ok & unify_resolved(r1, r2, origin, subst, adts, interner, diagnostics)
        }

        (Ty::List(x), Ty::List(y)) => unify_resolved(x, y, origin, subst, adts, interner, diagnostics),

        (Ty::Adt(id1, a1), Ty::Adt(id2, a2)) if id1 == id2 => {
            let mut ok = true;
            for (x, y) in a1.iter().zip(a2) {
                ok &= unify_resolved(x, y, origin, subst, adts, interner, diagnostics);
            }
            ok
        }

        (Ty::Record(f1), Ty::Record(f2)) if f1.len() == f2.len() => {
            let mut ok = true;
            for (k, v1) in f1 {
                match f2.get(k) {
                    Some(v2) => ok &= unify_resolved(v1, v2, origin, subst, adts, interner, diagnostics),
                    None => ok = false,
                }
            }
            if !ok {
                diagnostics.push(mismatch_error(a, b, origin, subst, adts, interner));
            }
            ok
        }

        (x, y) if x == y => true,

        // Int/Float specifically get a conversion hint, since it's the
        // single most common near-miss a numeric-literal-heavy language
        // produces.
        (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
            let mut diag = mismatch_error(a, b, origin, subst, adts, interner);
            diag = diag.with_help("convert explicitly with `int(..)` or `float(..)`".to_string());
            diagnostics.push(diag);
            false
        }

        _ => {
            diagnostics.push(mismatch_error(a, b, origin, subst, adts, interner));
            false
        }
    }
}

fn infinite_type_error(
    v: crate::ty::TyVarId,
    t: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> Diagnostic {
    let var_str = display_ty(&Ty::Var(v), subst, adts, interner);
    let t_str = display_ty(t, subst, adts, interner);
    Diagnostic::error(format!("infinite type: `{var_str}` occurs in `{t_str}`"))
        .with_primary(origin.primary_span(), "while trying to unify these types")
        .with_help("a type cannot contain itself — this usually comes from a function whose return type depends on calling itself with its own type")
}

fn arity_error(expected: usize, found: usize, origin: &Origin) -> Diagnostic {
    Diagnostic::error(format!("expected {expected} argument(s), found {found}"))
        .with_primary(origin.primary_span(), format!("expected {expected}, found {found}"))
}

fn mismatch_error(
    a: &Ty,
    b: &Ty,
    origin: &Origin,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> Diagnostic {
    let a_str = display_ty(a, subst, adts, interner);
    let b_str = display_ty(b, subst, adts, interner);
    match origin {
        Origin::IfBranches { if_span, then_span, else_span } => Diagnostic::error("type mismatch in `if` branches")
            .with_secondary(*if_span, "this `if` expression must have a single type")
            .with_primary(*then_span, format!("this branch has type `{a_str}`"))
            .with_primary(*else_span, format!("this branch has type `{b_str}`"))
            .with_help("both branches of an `if` must produce the same type"),
        Origin::CallArgument { call_span, arg_span, param_idx, fn_name } => Diagnostic::error("argument type mismatch")
            .with_primary(*arg_span, format!("expected `{a_str}`, found `{b_str}`"))
            .with_secondary(*call_span, match fn_name {
                Some(n) => format!("in this call to `{}` (argument {})", interner.resolve(*n), param_idx + 1),
                None => format!("in this call (argument {})", param_idx + 1),
            }),
        Origin::BinaryOp { op_span, lhs_span, rhs_span } => Diagnostic::error("operand type mismatch")
            .with_primary(*lhs_span, format!("this has type `{a_str}`"))
            .with_primary(*rhs_span, format!("this has type `{b_str}`"))
            .with_secondary(*op_span, "in this operator expression"),
        Origin::Annotation { annot_span, value_span } => Diagnostic::error("type does not match its annotation")
            .with_primary(*annot_span, format!("annotated as `{a_str}`"))
            .with_primary(*value_span, format!("but this has type `{b_str}`")),
        Origin::MatchArms { first_span, this_span } => Diagnostic::error("match arms have different types")
            .with_primary(*first_span, format!("first arm has type `{a_str}`"))
            .with_primary(*this_span, format!("this arm has type `{b_str}`")),
        Origin::Return { fn_span, expr_span } => Diagnostic::error("return type mismatch")
            .with_secondary(*fn_span, format!("this function should return `{a_str}`"))
            .with_primary(*expr_span, format!("but this returns `{b_str}`")),
        Origin::ListElement { list_span, elem_span, index } => Diagnostic::error("list elements have different types")
            .with_secondary(*list_span, format!("this list's elements have type `{a_str}`"))
            .with_primary(*elem_span, format!("element {index} has type `{b_str}`")),
        Origin::WhileCond { span } => Diagnostic::error(format!("expected `Bool`, found `{b_str}`"))
            .with_primary(*span, "a `while` condition must be a Bool"),
        Origin::IndexTarget { span } => Diagnostic::error(format!("expected `{a_str}`, found `{b_str}`"))
            .with_primary(*span, "type mismatch here"),
    }
}
```

If `Diagnostic`'s builder methods don't take owned `String`/`&str` exactly as sketched here, adjust to match the real signatures found in `crates/ember-diag/src/lib.rs` (already used correctly throughout `ember-resolve`'s `resolver.rs` — follow that file's exact call patterns if these differ).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Implement unification with the occurs check and origin-driven diagnostics"
```

---

## Task 9: `env.rs` — `TyEnv`

**Files:**
- Modify: `crates/ember-types/src/env.rs`
- Modify: `crates/ember-types/src/lib.rs`

**Step 0:** Add `pub mod env;` — wait, already declared in Task 2's `lib.rs`. Skip.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{Scheme, Ty};
    use ember_ast::Interner;

    #[test]
    fn declare_and_lookup_within_one_scope() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut env = TyEnv::new();
        env.declare(x, Scheme { vars: vec![], ty: Ty::Int });
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Int);
    }

    #[test]
    fn nested_scope_shadowing_resolves_to_the_innermost() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut env = TyEnv::new();
        env.declare(x, Scheme { vars: vec![], ty: Ty::Int });
        env.push_scope();
        env.declare(x, Scheme { vars: vec![], ty: Ty::Bool });
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Bool);
        env.pop_scope();
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Int);
    }

    #[test]
    fn free_vars_collects_every_var_across_the_whole_scope_stack() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut env = TyEnv::new();
        env.declare(x, Scheme { vars: vec![], ty: Ty::Var(crate::ty::TyVarId(7)) });
        let free = env.free_vars();
        assert!(free.contains(&crate::ty::TyVarId(7)));
    }

    #[test]
    fn free_vars_excludes_a_schemes_own_quantified_vars() {
        let mut interner = Interner::new();
        let f = interner.intern("f");
        let mut env = TyEnv::new();
        let id = crate::ty::TyVarId(3);
        env.declare(f, Scheme { vars: vec![id], ty: Ty::Var(id) });
        assert!(!env.free_vars().contains(&id));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types declare_and_lookup nested_scope_shadowing free_vars`
Expected: FAIL to compile — `TyEnv` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::ty::{Scheme, Ty, TyVarId};
use ember_ast::Symbol;
use rustc_hash::{FxHashMap, FxHashSet};

/// A symbol-scoped environment mapping names to type schemes, with its own
/// push/pop scope stack — independent of `ember-resolve`'s scope stack
/// (that one tracks stack SLOTS, this one tracks SCHEMES; different data,
/// so a separate walk).
pub struct TyEnv {
    scopes: Vec<FxHashMap<Symbol, Scheme>>,
}

impl TyEnv {
    pub fn new() -> Self {
        TyEnv { scopes: vec![FxHashMap::default()] }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(FxHashMap::default());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn declare(&mut self, name: Symbol, scheme: Scheme) {
        self.scopes.last_mut().expect("no scope open").insert(name, scheme);
    }

    /// Innermost-first, so shadowing resolves to the nearest declaration.
    pub fn lookup(&self, name: Symbol) -> Option<&Scheme> {
        self.scopes.iter().rev().find_map(|s| s.get(&name))
    }

    /// Every free (non-quantified) type variable across every scheme
    /// currently in scope — used by `generalize` to know which variables
    /// are still "owned" by an enclosing binding and so must NOT be
    /// quantified away.
    pub fn free_vars(&self) -> FxHashSet<TyVarId> {
        let mut set = FxHashSet::default();
        for scope in &self.scopes {
            for scheme in scope.values() {
                collect_free_vars(&scheme.ty, &scheme.vars, &mut set);
            }
        }
        set
    }
}

impl Default for TyEnv {
    fn default() -> Self {
        TyEnv::new()
    }
}

fn collect_free_vars(ty: &Ty, quantified: &[TyVarId], out: &mut FxHashSet<TyVarId>) {
    match ty {
        Ty::Var(v) => {
            if !quantified.contains(v) {
                out.insert(*v);
            }
        }
        Ty::Fun(params, ret) => {
            for p in params {
                collect_free_vars(p, quantified, out);
            }
            collect_free_vars(ret, quantified, out);
        }
        Ty::List(t) => collect_free_vars(t, quantified, out),
        Ty::Adt(_, args) => {
            for a in args {
                collect_free_vars(a, quantified, out);
            }
        }
        Ty::Record(fields) => {
            for v in fields.values() {
                collect_free_vars(v, quantified, out);
            }
        }
        _ => {}
    }
}
```

Note `free_vars` walks the raw (unresolved) `Scheme.ty` — schemes stored in the env are the already-generalized, already-substituted result at the point they were declared, so this doesn't need `&mut Subst` to resolve anything further.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add TyEnv scope stack with free-variable collection"
```

---

## Task 10: `infer.rs` skeleton — native globals, literals, `Var`, `Unary`/`Binary`, `Call`

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

This is the first task touching `infer.rs`; it establishes the `Infer` struct every later task adds to, and the arithmetic/comparison/logical-operator typing rules.

**Numeric-operator scope decision:** without a typeclass system (explicitly out of scope for this phase — not mentioned anywhere in the checklist), arithmetic (`+ - * / %`) and comparison (`< <= > >=`) operators unify their two operands with each other and use that shared type as the result (arithmetic) or `Bool` (comparison), **without** additionally constraining that shared type to be `Int`/`Float` specifically. This is permissive (e.g. it would let two same-typed non-numeric values unify through `+` at the type level) but sound, and matches every example given in `SPEC.md`/`PROMPT.md`, none of which specify a numeric constraint mechanism. Documented as a known scope simplification in Task 20's checklist reconciliation. Logical operators (`&& ||`) and unary `!` DO get a real `Bool` constraint (unambiguous, no reason to be permissive there).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> (crate::ty::Ty, Vec<ember_diag::Diagnostic>) {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let mut infer = Infer::new(&ast, &mut interner);
        let last_expr = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected the last top-level statement to be an ExprStmt, got {other:?}"),
        };
        infer.resolve_program(&stmts);
        let ty = infer.subst.resolve(infer.expr_types.get(&last_expr).unwrap());
        (ty, infer.diagnostics)
    }

    #[test]
    fn int_literal_infers_int() {
        let (ty, diags) = run("42;");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(ty, crate::ty::Ty::Int);
    }

    #[test]
    fn native_global_print_is_seeded() {
        let (_ty, diags) = run("print(1);");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn binary_arithmetic_requires_matching_operands() {
        let (_ty, diags) = run("1 + true;");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn logical_and_requires_bool_operands() {
        let (_ty, diags) = run("1 && true;");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn comparison_produces_bool() {
        let (ty, diags) = run("1 < 2;");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(ty, crate::ty::Ty::Bool);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types int_literal native_global binary_arithmetic logical_and comparison_produces`
Expected: FAIL to compile — `Infer` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::adt::AdtRegistry;
use crate::constraint::Origin;
use crate::env::TyEnv;
use crate::subst::Subst;
use crate::trace::InferenceTrace;
use crate::ty::{Scheme, Ty};
use ember_ast::{Ast, Expr, Idx, Interner, Stmt, Symbol};
use ember_diag::Diagnostic;
use ember_lexer::TokenKind;
use rustc_hash::FxHashMap;

const NATIVE_GLOBALS: &[(&str, fn(&mut Subst) -> Ty)] = &[
    ("print", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Unit))),
    ("len", |s| Ty::Fun(vec![Ty::List(Box::new(s.fresh()))], Box::new(Ty::Int))),
    ("push", |s| {
        let elem = s.fresh();
        Ty::Fun(vec![Ty::List(Box::new(elem.clone())), elem], Box::new(Ty::Unit))
    }),
    ("clock", |_| Ty::Fun(vec![], Box::new(Ty::Float))),
    ("str", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::String))),
    ("int", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Int))),
    ("float", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Float))),
    ("type_of", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::String))),
];

pub struct Infer<'a> {
    ast: &'a Ast,
    interner: &'a mut Interner,
    pub subst: Subst,
    env: TyEnv,
    adts: AdtRegistry,
    pub diagnostics: Vec<Diagnostic>,
    pub trace: InferenceTrace,
    pub expr_types: FxHashMap<Idx<Expr>, Ty>,
}

impl<'a> Infer<'a> {
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        let mut subst = Subst::new();
        let mut env = TyEnv::new();
        for (name, make_ty) in NATIVE_GLOBALS {
            let sym = interner.intern(name);
            let ty = make_ty(&mut subst);
            env.declare(sym, Scheme { vars: vec![], ty });
        }
        Infer {
            ast,
            interner,
            subst,
            env,
            adts: AdtRegistry::new(),
            diagnostics: Vec::new(),
            trace: InferenceTrace::default(),
            expr_types: FxHashMap::default(),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty, origin: Origin) -> bool {
        crate::unify::unify(a, b, origin, &mut self.subst, &self.adts, self.interner, &mut self.trace, &mut self.diagnostics)
    }

    fn display(&mut self, ty: &Ty) -> String {
        crate::display::display_ty(ty, &mut self.subst, &self.adts, self.interner)
    }

    /// Minimal driver for this task: infers every top-level statement in
    /// order, no hoisting/generalization yet (Task 12 replaces this).
    pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
        for &s in stmts {
            self.infer_stmt(s);
        }
    }

    fn infer_stmt(&mut self, idx: Idx<Stmt>) {
        match self.ast.stmt(idx).clone() {
            Stmt::ExprStmt(e) => {
                self.infer_expr(e);
            }
            _ => {
                // Every other Stmt form is added in later tasks (Let in
                // Task 11, Fn in Task 12, control flow in Task 13, decls in
                // Task 14). Left unhandled here deliberately so the match
                // stays a visible TODO surface rather than a silent no-op —
                // revisit if this arm still exists after Task 14.
            }
        }
    }

    fn infer_expr(&mut self, idx: Idx<Expr>) -> Ty {
        let ty = match self.ast.expr(idx).clone() {
            Expr::Int(_) => Ty::Int,
            Expr::Float(_) => Ty::Float,
            Expr::Str(_) => Ty::String,
            Expr::Bool(_) => Ty::Bool,
            Expr::Nil => Ty::Unit,
            Expr::Var(sym) => self.infer_var(sym, idx),
            Expr::Unary { op, operand } => self.infer_unary(op, operand, idx),
            Expr::Binary { op, lhs, rhs } => self.infer_binary(op, lhs, rhs, idx),
            Expr::Call { callee, args } => self.infer_call(callee, &args, idx),
            _ => self.subst.fresh(), // remaining forms land in Tasks 12-18
        };
        self.expr_types.insert(idx, ty.clone());
        ty
    }

    fn infer_var(&mut self, sym: Symbol, idx: Idx<Expr>) -> Ty {
        match self.env.lookup(sym).cloned() {
            Some(scheme) => self.instantiate(&scheme),
            None => {
                let span = self.ast.span_of_expr(idx);
                let name = self.interner.resolve(sym).to_string();
                self.diagnostics.push(
                    Diagnostic::error(format!("undeclared name `{name}`")).with_primary(span, "not found in this scope"),
                );
                self.subst.fresh()
            }
        }
    }

    /// Replaces each of a scheme's quantified variables with a fresh one —
    /// why `identity(1)` and `identity("x")` can coexist: `identity` is
    /// stored as `∀a. a -> a`; the first call instantiates `a := t1` and
    /// unifies `t1 = Int`, the second instantiates `a := t2` and unifies
    /// `t2 = String`. No conflict, because they're different variables.
    pub(crate) fn instantiate(&mut self, scheme: &Scheme) -> Ty {
        let sub: FxHashMap<crate::ty::TyVarId, Ty> =
            scheme.vars.iter().map(|&v| (v, self.subst.fresh())).collect();
        substitute(&scheme.ty, &sub)
    }

    fn infer_unary(&mut self, op: TokenKind, operand: Idx<Expr>, idx: Idx<Expr>) -> Ty {
        let operand_ty = self.infer_expr(operand);
        let span = self.ast.span_of_expr(idx);
        match op {
            TokenKind::Bang => {
                self.unify(&operand_ty, &Ty::Bool, Origin::WhileCond { span });
                Ty::Bool
            }
            // Minus: permissive (see the numeric-operator scope note above)
            // — the operand's own type is also the result's type.
            _ => operand_ty,
        }
    }

    fn infer_binary(&mut self, op: TokenKind, lhs: Idx<Expr>, rhs: Idx<Expr>, idx: Idx<Expr>) -> Ty {
        let lhs_ty = self.infer_expr(lhs);
        let rhs_ty = self.infer_expr(rhs);
        let op_span = self.ast.span_of_expr(idx);
        let lhs_span = self.ast.span_of_expr(lhs);
        let rhs_span = self.ast.span_of_expr(rhs);
        let origin = Origin::BinaryOp { op_span, lhs_span, rhs_span };
        match op {
            TokenKind::AndAnd | TokenKind::OrOr => {
                self.unify(&lhs_ty, &Ty::Bool, origin.clone());
                self.unify(&rhs_ty, &Ty::Bool, origin);
                Ty::Bool
            }
            TokenKind::EqEq | TokenKind::BangEq | TokenKind::Lt | TokenKind::LtEq | TokenKind::Gt | TokenKind::GtEq => {
                self.unify(&lhs_ty, &rhs_ty, origin);
                Ty::Bool
            }
            // Arithmetic: permissive, see the numeric-operator scope note.
            _ => {
                self.unify(&lhs_ty, &rhs_ty, origin);
                lhs_ty
            }
        }
    }

    fn infer_call(&mut self, callee: Idx<Expr>, args: &[Idx<Expr>], idx: Idx<Expr>) -> Ty {
        let call_span = self.ast.span_of_expr(idx);
        let callee_ty = self.infer_expr(callee);
        let resolved_callee = self.subst.resolve(&callee_ty);
        let fn_name = match self.ast.expr(callee) {
            Expr::Var(s) => Some(*s),
            _ => None,
        };
        match resolved_callee {
            Ty::Fun(params, ret) => {
                if params.len() != args.len() {
                    let expected = params.len();
                    let found = args.len();
                    self.diagnostics.push(
                        Diagnostic::error(format!("expected {expected} argument(s), found {found}"))
                            .with_primary(call_span, format!("expected {expected}, found {found}")),
                    );
                    for &a in args {
                        self.infer_expr(a);
                    }
                    self.subst.fresh()
                } else {
                    for (i, (&a, p)) in args.iter().zip(params.iter()).enumerate() {
                        let arg_ty = self.infer_expr(a);
                        let origin = Origin::CallArgument {
                            call_span,
                            arg_span: self.ast.span_of_expr(a),
                            param_idx: i,
                            fn_name,
                        };
                        self.unify(&arg_ty, p, origin);
                    }
                    *ret
                }
            }
            Ty::Var(_) => {
                let arg_types: Vec<Ty> = args.iter().map(|&a| self.infer_expr(a)).collect();
                let ret_ty = self.subst.fresh();
                let expected = Ty::Fun(arg_types, Box::new(ret_ty.clone()));
                self.unify(
                    &resolved_callee,
                    &expected,
                    Origin::CallArgument { call_span, arg_span: call_span, param_idx: 0, fn_name },
                );
                ret_ty
            }
            other => {
                let other_str = self.display(&other);
                self.diagnostics.push(
                    Diagnostic::error(format!("`{other_str}` is not callable")).with_primary(call_span, "attempted call here"),
                );
                for &a in args {
                    self.infer_expr(a);
                }
                self.subst.fresh()
            }
        }
    }
}

/// Replaces every `TyVarId` present in `sub` throughout `ty`, leaving
/// anything not in `sub` untouched. Used by `instantiate` (and later,
/// `generalize`'s callers indirectly via schemes built from it).
pub(crate) fn substitute(ty: &Ty, sub: &FxHashMap<crate::ty::TyVarId, Ty>) -> Ty {
    match ty {
        Ty::Var(v) => sub.get(v).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Fun(params, ret) => Ty::Fun(params.iter().map(|p| substitute(p, sub)).collect(), Box::new(substitute(ret, sub))),
        Ty::List(t) => Ty::List(Box::new(substitute(t, sub))),
        Ty::Adt(id, args) => Ty::Adt(*id, args.iter().map(|a| substitute(a, sub)).collect()),
        Ty::Record(fields) => Ty::Record(fields.iter().map(|(k, v)| (*k, substitute(v, sub))).collect()),
        _ => ty.clone(),
    }
}
```

Also add `pub mod unify;` and `pub mod infer;` presence check to `lib.rs` — both already declared in Task 2, no change needed there.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too — the `_ => self.subst.fresh()` catch-all in `infer_expr` and the `_ => {}` in `infer_stmt` are expected and intentional at this point; later tasks narrow them.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add Infer skeleton: native globals, literals, Var, Unary/Binary, Call"
```

---

## Task 11: `let` statements, the value restriction, generalize/instantiate

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn let_binding_infers_from_its_initializer() {
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 42;\nprint(x);");
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn let_polymorphism_lets_identity_be_used_at_two_types() {
    let src = "let identity = |x| x;\nlet a = identity(1);\nlet b = identity(\"hello\");";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn occurs_check_terminates_instead_of_hanging() {
    let src = "let f = |x| f(x);";
    let result = std::thread::spawn(move || {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        infer.diagnostics.len()
    })
    .join()
    .expect("inference panicked or hung");
    assert!(result > 0, "expected an infinite-type diagnostic");
}

#[test]
fn value_restriction_prevents_a_mutable_binding_from_generalizing() {
    let src = "let mut r = [];\npush(r, 1);\nlet s = int(r[0]) + float(r[0]);";
    // Deliberately provoke a conflict: if `r` had generalized to `forall a.
    // [a]`, both `int(r[0])` and `float(r[0])` would separately typecheck
    // by instantiating `a` differently, masking the bug this test exists
    // to catch. Instead, force a DIRECT type conflict on the monomorphic
    // list element type by using it as two different concrete types in the
    // same expression without generalization ever being able to paper over
    // it.
    let src2 = "let mut r = [];\npush(r, true);\nlet s = 1 + r[0];";
    for s in [src, src2] {
        let _ = s;
    }
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src2);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(!infer.diagnostics.is_empty(), "expected a type error: mutable bindings must not generalize");
}
```

The first `src`/loop scaffolding in `value_restriction_prevents_a_mutable_binding_from_generalizing` is dead weight left from drafting — delete the `let src = ...` line and the `for` loop, keeping only `src2` (renamed to `src`) and the direct call. Write the test as:

```rust
#[test]
fn value_restriction_prevents_a_mutable_binding_from_generalizing() {
    // `r` is `mut`, so it must NOT generalize to `forall a. [a]` — if it
    // did, `push(r, true)` (a Bool) and `1 + r[0]` (expecting Int) would
    // each separately instantiate their own fresh `a` and both typecheck,
    // silently masking the unsoundness. Since it stays monomorphic, `r`'s
    // element type gets pinned to Bool by the push, and `1 + r[0]` then
    // conflicts.
    let src = "let mut r = [];\npush(r, true);\nlet s = 1 + r[0];";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(!infer.diagnostics.is_empty(), "expected a type error: mutable bindings must not generalize");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types let_binding_infers let_polymorphism occurs_check_terminates value_restriction`
Expected: FAIL — `Stmt::Let` isn't handled by `infer_stmt` yet (falls into the `_ => {}` arm), so `x`/`identity`/etc. are never declared and subsequent `Var` lookups error as undeclared instead of the specific behaviors under test.

- [ ] **Step 3: Implement**

Add to `infer_stmt`'s match (replacing the `_ => {}` catch-all's role for `Let`, keeping the catch-all for the forms still unhandled):

```rust
fn infer_stmt(&mut self, idx: Idx<Stmt>) {
    match self.ast.stmt(idx).clone() {
        Stmt::ExprStmt(e) => {
            self.infer_expr(e);
        }
        Stmt::Let { name, mutable, ty: annot, init } => {
            let init_ty = self.infer_expr(init);
            if let Some(annot_idx) = annot {
                let annot_ty = self.type_expr_to_ty(annot_idx);
                let origin = Origin::Annotation {
                    annot_span: self.ast.span_of_type_expr(annot_idx),
                    value_span: self.ast.span_of_expr(init),
                };
                self.unify(&annot_ty, &init_ty, origin);
            }
            let scheme = if self.should_generalize(mutable, init) {
                self.generalize(&init_ty)
            } else {
                Scheme { vars: vec![], ty: init_ty }
            };
            self.env.declare(name, scheme);
        }
        _ => {}
    }
}

/// THE VALUE RESTRICTION. Generalizing a mutable binding is unsound: `let
/// mut r = [];` would generalize to `∀a. [a]`, letting a later `push`
/// pin one element type while a later read expects another. Only
/// syntactic values may generalize — literals, lambdas, variables. Never
/// mutable bindings, never the result of a general application (a plain
/// function call).
fn should_generalize(&self, is_mut: bool, init: Idx<Expr>) -> bool {
    !is_mut && self.is_syntactic_value(init)
}

fn is_syntactic_value(&self, e: Idx<Expr>) -> bool {
    matches!(
        self.ast.expr(e),
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil | Expr::Lambda { .. } | Expr::Var(_)
    )
}

/// Generalize ONLY at `let` and top-level `fn` (Task 12 reuses this for
/// the latter). Quantifies every free type variable that is NOT free in
/// the surrounding environment — a variable still referenced by an
/// enclosing binding is not this binding's to quantify.
fn generalize(&mut self, ty: &Ty) -> Scheme {
    let resolved = self.subst.resolve(ty);
    let env_free = self.env.free_vars();
    let mut ty_free = rustc_hash::FxHashSet::default();
    collect_ty_vars(&resolved, &mut ty_free);
    let vars: Vec<crate::ty::TyVarId> = ty_free.difference(&env_free).copied().collect();
    Scheme { vars, ty: resolved }
}
```

Add the small `collect_ty_vars` helper (used by `generalize`; distinct from `env.rs`'s `collect_free_vars`, which excludes a scheme's own quantified vars — this one wants every var in a bare `Ty` with nothing pre-quantified):

```rust
fn collect_ty_vars(ty: &Ty, out: &mut rustc_hash::FxHashSet<crate::ty::TyVarId>) {
    match ty {
        Ty::Var(v) => {
            out.insert(*v);
        }
        Ty::Fun(params, ret) => {
            for p in params {
                collect_ty_vars(p, out);
            }
            collect_ty_vars(ret, out);
        }
        Ty::List(t) => collect_ty_vars(t, out),
        Ty::Adt(_, args) => {
            for a in args {
                collect_ty_vars(a, out);
            }
        }
        Ty::Record(fields) => {
            for v in fields.values() {
                collect_ty_vars(v, out);
            }
        }
        _ => {}
    }
}
```

Add `Expr::Lambda` to `infer_expr`'s match (needed for `let identity = |x| x;` to work at all):

```rust
Expr::Lambda { params, body } => self.infer_lambda(&params, body),
```

```rust
fn infer_lambda(&mut self, params: &[ember_ast::Param], body: Idx<Expr>) -> Ty {
    self.env.push_scope();
    let param_types: Vec<Ty> = params
        .iter()
        .map(|p| {
            let ty = match p.ty {
                Some(annot_idx) => self.type_expr_to_ty(annot_idx),
                None => self.subst.fresh(),
            };
            self.env.declare(p.name, Scheme { vars: vec![], ty: ty.clone() });
            ty
        })
        .collect();
    let body_ty = self.infer_expr(body);
    self.env.pop_scope();
    Ty::Fun(param_types, Box::new(body_ty))
}
```

Add the `type_expr_to_ty` helper, translating a parsed `TypeExpr` annotation into a `Ty` (used by both `Let`'s optional annotation and `Lambda`/`Fn` parameter annotations — this task only needs the built-in-name and list cases; the `Generic`/ADT-name cases are exercised once Task 14 registers user types, but the function is written completely now):

```rust
fn type_expr_to_ty(&mut self, idx: Idx<ember_ast::TypeExpr>) -> Ty {
    match self.ast.type_expr(idx).clone() {
        ember_ast::TypeExpr::Name(sym) => {
            let name = self.interner.resolve(sym);
            match name {
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Bool" => Ty::Bool,
                "String" => Ty::String,
                "Unit" => Ty::Unit,
                _ => match self.adts.id_of(sym) {
                    Some(id) => Ty::Adt(id, vec![]),
                    None => {
                        let span = self.ast.span_of_type_expr(idx);
                        self.diagnostics.push(
                            Diagnostic::error(format!("unknown type `{name}`")).with_primary(span, "not found"),
                        );
                        self.subst.fresh()
                    }
                },
            }
        }
        ember_ast::TypeExpr::List(inner) => Ty::List(Box::new(self.type_expr_to_ty(inner))),
        ember_ast::TypeExpr::Generic { name, args } => {
            let text = self.interner.resolve(name).to_string();
            if text == "List" && args.len() == 1 {
                Ty::List(Box::new(self.type_expr_to_ty(args[0])))
            } else {
                let span = self.ast.span_of_type_expr(idx);
                self.diagnostics.push(
                    Diagnostic::error(format!("type `{text}` does not take type arguments"))
                        .with_primary(span, "user-declared types have no generic parameters this phase"),
                );
                self.subst.fresh()
            }
        }
        ember_ast::TypeExpr::Fun { params, ret } => {
            let param_tys = params.iter().map(|p| self.type_expr_to_ty(*p)).collect();
            Ty::Fun(param_tys, Box::new(self.type_expr_to_ty(ret)))
        }
        ember_ast::TypeExpr::Error => self.subst.fresh(),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Infer let statements with generalize/instantiate and the value restriction"
```

---

## Task 12: Top-level two-pass function typing, mutual recursion, nested `fn`

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn identity_generalizes_to_a_polymorphic_scheme() {
    let src = "fn identity(x) { x }\nlet a = identity(1);\nlet b = identity(\"hello\");";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    let identity = interner.intern("identity");
    let scheme = infer.fn_schemes.get(&identity).expect("identity should have a recorded scheme");
    assert_eq!(scheme.vars.len(), 1, "identity should generalize over exactly one type variable");
}

#[test]
fn mutual_recursion_between_top_level_functions_typechecks() {
    // Deliberately no `if`/base case — Task 13 (control flow) hasn't
    // landed yet at this point in the plan, and this test only needs to
    // exercise the two-pass hoisting itself (forward reference from
    // `is_even` to the not-yet-declared `is_odd`), not realistic
    // termination semantics.
    let src = "fn is_even(n) { is_odd(n) }\nfn is_odd(n) { is_even(n) }\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn recursive_function_self_calls_typecheck() {
    // Same reasoning as above: no `if`/base case needed to exercise
    // letrec-style monomorphic self-binding during body inference.
    let src = "fn loop_forever(n) { loop_forever(n) }\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types identity_generalizes mutual_recursion recursive_function`
Expected: FAIL to compile (`infer.fn_schemes` doesn't exist) or fail at runtime with undeclared-name diagnostics (`Stmt::Fn` isn't handled by `infer_stmt` yet).

- [ ] **Step 3: Implement**

Add `pub fn_schemes: FxHashMap<Symbol, Scheme>` to the `Infer` struct and initialize it empty in `new`.

Replace `resolve_program` with the two-pass version (mirrors the resolver's own two-pass hoist, but binding monomorphic function types instead of allocating slots):

```rust
pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
    // Pass 1: bind every top-level fn to a monomorphic function type so
    // mutual recursion between top-level functions type-checks.
    let mut fn_stmts = Vec::new();
    for &s in stmts {
        if let Stmt::Fn { name, params, ret_ty, .. } = self.ast.stmt(s).clone() {
            let param_types: Vec<Ty> = params
                .iter()
                .map(|p| match p.ty {
                    Some(annot) => self.type_expr_to_ty(annot),
                    None => self.subst.fresh(),
                })
                .collect();
            let ret = match ret_ty {
                Some(annot) => self.type_expr_to_ty(annot),
                None => self.subst.fresh(),
            };
            let fn_ty = Ty::Fun(param_types, Box::new(ret));
            self.env.declare(name, Scheme { vars: vec![], ty: fn_ty });
            fn_stmts.push(s);
        }
    }

    // Pass 2: infer each function's body against its own (and its
    // siblings') monomorphic binding, then generalize and rebind.
    for &s in &fn_stmts {
        self.infer_fn_decl(s, true);
    }

    // Pass 3: everything else (let, expression statements, decls).
    for &s in stmts {
        if !matches!(self.ast.stmt(s), Stmt::Fn { .. }) {
            self.infer_stmt(s);
        }
    }
}

/// Infers one `fn`'s body against its already-bound monomorphic type
/// (bound either by `resolve_program`'s pass 1, for top-level fns, or by
/// this function itself for a nested one), unifies the body with the
/// return type, then — only when `top_level` — generalizes the solved
/// type and rebinds it polymorphically. A nested `fn`'s type stays
/// monomorphic at its own use sites, matching "generalize at let and
/// top-level fn only."
fn infer_fn_decl(&mut self, idx: Idx<Stmt>, top_level: bool) {
    let Stmt::Fn { name, params, ret_ty, body } = self.ast.stmt(idx).clone() else {
        unreachable!("infer_fn_decl called on a non-Fn statement");
    };

    // A nested fn isn't pre-declared by any hoisting pass, so bind its own
    // monomorphic type here before inferring its body (so it can recurse).
    let existing = self.env.lookup(name).cloned();
    let fn_scheme = existing.unwrap_or_else(|| {
        let param_types: Vec<Ty> = params
            .iter()
            .map(|p| match p.ty {
                Some(annot) => self.type_expr_to_ty(annot),
                None => self.subst.fresh(),
            })
            .collect();
        let ret = match ret_ty {
            Some(annot) => self.type_expr_to_ty(annot),
            None => self.subst.fresh(),
        };
        let scheme = Scheme { vars: vec![], ty: Ty::Fun(param_types, Box::new(ret)) };
        self.env.declare(name, scheme.clone());
        scheme
    });

    let Ty::Fun(param_types, ret_ty_boxed) = fn_scheme.ty.clone() else {
        unreachable!("a fn's own bound type must be Ty::Fun")
    };

    self.env.push_scope();
    for (p, ty) in params.iter().zip(param_types.iter()) {
        self.env.declare(p.name, Scheme { vars: vec![], ty: ty.clone() });
    }
    let body_ty = self.infer_expr(body);
    let fn_span = self.ast.span_of_stmt(idx);
    let body_span = self.ast.span_of_expr(body);
    self.unify(&body_ty, &ret_ty_boxed, Origin::Return { fn_span, expr_span: body_span });
    self.env.pop_scope();

    if top_level {
        let final_ty = self.subst.resolve(&Ty::Fun(param_types, ret_ty_boxed));
        let scheme = self.generalize(&final_ty);
        self.env.declare(name, scheme.clone());
        self.fn_schemes.insert(name, scheme);
    }
}
```

Add `Stmt::Fn { .. } => self.infer_fn_decl(idx, false),` to `infer_stmt`'s match, for the nested case (a `fn` encountered inside a block, not via `resolve_program`'s top-level pass).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add two-pass top-level function typing with generalization and mutual recursion"
```

---

## Task 13: Control flow — `If`, `Block`, `While`, `For`, `Loop`, `Return`, `Assign`, `List`, `Index`

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn if_branch_type_mismatch_labels_both_branches() {
    let src = "if true { 1 } else { \"x\" };";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
    assert!(infer.diagnostics[0].labels.len() >= 2, "expected both branches labeled: {:?}", infer.diagnostics[0]);
}

#[test]
fn if_without_else_is_unit() {
    let src = "let x = if true { print(1); };";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn while_condition_must_be_bool() {
    let src = "while 1 { }";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}

#[test]
fn block_tail_expression_is_the_blocks_type() {
    let src = "let x = { let y = 1; y };\nlet z = x + 1;";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn list_elements_must_share_one_type() {
    let src = "[1, \"two\", 3];";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}

#[test]
fn index_into_a_list_yields_its_element_type() {
    let src = "let xs = [1, 2, 3];\nlet y = xs[0] + 1;";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn assigning_a_wrong_type_to_a_mutable_binding_errors() {
    let src = "let mut x = 1;\nx = \"nope\";";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}

#[test]
fn for_loop_and_return_and_break_continue_all_typecheck() {
    let src = "fn f(xs) {\n  for x in xs {\n    if x == 0 { break; }\n    if x == 1 { continue; }\n    return x;\n  }\n  0\n}\nprint(f([1, 2]));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}
```

Check `ember_diag::Diagnostic`'s exact `labels` field name/shape before relying on `.labels.len()` in `if_branch_type_mismatch_labels_both_branches` — adjust to whatever the real introspection surface is (`grep -n "pub struct Diagnostic" -A 20 crates/ember-diag/src/lib.rs`; it may be `primary`/`secondary` fields rather than one combined `labels` vec).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types if_branch if_without while_condition block_tail list_elements index_into assigning_a_wrong for_loop_and`
Expected: FAIL — none of these `Expr`/`Stmt` forms are handled yet (all fall into the `_ => self.subst.fresh()` / `_ => {}` catch-alls, producing zero diagnostics where errors are expected, or undeclared-name errors where clean typechecking is expected).

- [ ] **Step 3: Implement**

Add to `infer_expr`'s match:

```rust
Expr::If { cond, then_, else_ } => self.infer_if(cond, then_, else_, idx),
Expr::Block { stmts, tail } => self.infer_block(&stmts, tail),
Expr::List { items } => self.infer_list(&items, idx),
Expr::Index { base, index } => self.infer_index(base, index, idx),
Expr::Assign { target, value } => self.infer_assign(target, value, idx),
```

```rust
fn infer_if(&mut self, cond: Idx<Expr>, then_: Idx<Expr>, else_: Option<Idx<Expr>>, idx: Idx<Expr>) -> Ty {
    let cond_ty = self.infer_expr(cond);
    let cond_span = self.ast.span_of_expr(cond);
    self.unify(&cond_ty, &Ty::Bool, Origin::WhileCond { span: cond_span });
    let then_ty = self.infer_expr(then_);
    match else_ {
        Some(e) => {
            let else_ty = self.infer_expr(e);
            let if_span = self.ast.span_of_expr(idx);
            let then_span = self.ast.span_of_expr(then_);
            let else_span = self.ast.span_of_expr(e);
            self.unify(&then_ty, &else_ty, Origin::IfBranches { if_span, then_span, else_span });
            then_ty
        }
        None => {
            // No else: the `if` is only ever used for its side effects, so
            // its own type is Unit (its "then" branch's type is not
            // constrained to Unit — that's the branch author's business,
            // matching how a Block's own tail-less form works).
            Ty::Unit
        }
    }
}

fn infer_block(&mut self, stmts: &[Idx<Stmt>], tail: Option<Idx<Expr>>) -> Ty {
    self.env.push_scope();
    for &s in stmts {
        self.infer_stmt(s);
    }
    let ty = match tail {
        Some(t) => self.infer_expr(t),
        None => Ty::Unit,
    };
    self.env.pop_scope();
    ty
}

fn infer_list(&mut self, items: &[Idx<Expr>], idx: Idx<Expr>) -> Ty {
    let elem_ty = self.subst.fresh();
    let list_span = self.ast.span_of_expr(idx);
    for (i, &item) in items.iter().enumerate() {
        let item_ty = self.infer_expr(item);
        let elem_span = self.ast.span_of_expr(item);
        self.unify(&elem_ty, &item_ty, Origin::ListElement { list_span, elem_span, index: i });
    }
    Ty::List(Box::new(elem_ty))
}

fn infer_index(&mut self, base: Idx<Expr>, index: Idx<Expr>, idx: Idx<Expr>) -> Ty {
    let base_ty = self.infer_expr(base);
    let index_ty = self.infer_expr(index);
    let span = self.ast.span_of_expr(idx);
    self.unify(&index_ty, &Ty::Int, Origin::IndexTarget { span });
    let elem_ty = self.subst.fresh();
    self.unify(&base_ty, &Ty::List(Box::new(elem_ty.clone())), Origin::IndexTarget { span });
    elem_ty
}

fn infer_assign(&mut self, target: Idx<Expr>, value: Idx<Expr>, idx: Idx<Expr>) -> Ty {
    let target_ty = self.infer_expr(target);
    let value_ty = self.infer_expr(value);
    let span = self.ast.span_of_expr(idx);
    self.unify(&target_ty, &value_ty, Origin::IndexTarget { span });
    Ty::Unit
}
```

(`Assign`/`Index` reuse `Origin::IndexTarget` rather than gaining their own dedicated `Origin` variants — both are simple "these two must match" checks without multiple contributing spans worth labeling separately, unlike `IfBranches`/`ListElement`. `Assign`'s mutability is already enforced by the resolver; type inference only needs to check the value's type matches.)

Add to `infer_stmt`'s match:

```rust
Stmt::While { cond, body } => {
    let cond_ty = self.infer_expr(cond);
    let span = self.ast.span_of_expr(cond);
    self.unify(&cond_ty, &Ty::Bool, Origin::WhileCond { span });
    self.infer_expr(body);
}
Stmt::For { binding, iter, body } => {
    let iter_ty = self.infer_expr(iter);
    let elem_ty = self.subst.fresh();
    let span = self.ast.span_of_expr(iter);
    self.unify(&iter_ty, &Ty::List(Box::new(elem_ty.clone())), Origin::IndexTarget { span });
    self.env.push_scope();
    self.env.declare(binding, Scheme { vars: vec![], ty: elem_ty });
    self.infer_expr(body);
    self.env.pop_scope();
}
Stmt::Loop { body } => {
    self.infer_expr(body);
}
Stmt::Return(value) => {
    if let Some(v) = value {
        self.infer_expr(v);
    }
    // The return TYPE itself is unified against the enclosing fn's
    // declared return type in `infer_fn_decl` via the function's tail
    // expression/body type, not per-`return`-statement here — an early
    // `return` inside a loop or `if` is a narrower dataflow-typing problem
    // (which control paths are reachable) that this phase's simplified,
    // non-branch-level approach doesn't attempt. Matches Phase 4's
    // similarly-scoped unreachable-code analysis: direct-successor only,
    // not full flow typing. A mismatched early `return`'s value is still
    // visited and typed above so its OWN subexpressions get checked.
}
Stmt::Break | Stmt::Continue => {}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Infer control-flow forms: if/block/while/for/loop/return/list/index/assign"
```

---

## Task 14: ADT registry construction and variant constructor typing

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn adt_variant_with_payload_types_as_a_function() {
    let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nlet c = Circle(3.0);\nprint(c);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn nullary_variant_is_a_plain_value_not_a_function() {
    let src = "type Shape = | Circle(Float) | Point;\nlet p = Point;\nprint(p);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn wrong_payload_type_to_a_constructor_errors() {
    let src = "type Shape = | Circle(Float);\nlet c = Circle(\"nope\");";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types adt_variant_with nullary_variant wrong_payload`
Expected: FAIL — `Stmt::TypeDecl` isn't handled yet, so `Circle`/`Point` are never declared and every use is an undeclared-name error.

- [ ] **Step 3: Implement**

`AdtRegistry` needs to be populated *before* the two-pass function walk (a function might reference a type declared anywhere in the file, and constructors must be callable/referenceable from any top-level statement). Add a registration pass as the very first step of `resolve_program`, before pass 1:

```rust
pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
    self.register_adts(stmts);

    // Pass 1: bind every top-level fn to a monomorphic function type ...
    let mut fn_stmts = Vec::new();
    // ... (unchanged from Task 12)
}

fn register_adts(&mut self, stmts: &[Idx<Stmt>]) {
    for &s in stmts {
        match self.ast.stmt(s).clone() {
            Stmt::TypeDecl { name, variants } => {
                let resolved_variants: Vec<(Symbol, Vec<Ty>)> = variants
                    .iter()
                    .map(|v| {
                        let payload = v.payload.iter().map(|&t| self.type_expr_to_ty(t)).collect();
                        (v.name, payload)
                    })
                    .collect();
                let id = self.adts.register_enum(name, resolved_variants.clone());
                for (variant_name, payload) in resolved_variants {
                    let scheme = if payload.is_empty() {
                        Scheme { vars: vec![], ty: Ty::Adt(id, vec![]) }
                    } else {
                        Scheme { vars: vec![], ty: Ty::Fun(payload, Box::new(Ty::Adt(id, vec![]))) }
                    };
                    self.env.declare(variant_name, scheme);
                }
            }
            Stmt::StructDecl { name, fields } => {
                let resolved_fields: Vec<(Symbol, Ty)> =
                    fields.iter().map(|f| (f.name, self.type_expr_to_ty(f.ty))).collect();
                self.adts.register_struct(name, resolved_fields);
            }
            _ => {}
        }
    }
}
```

Note this pass calls `self.type_expr_to_ty`, which for an ADT/struct field annotation referencing ANOTHER user type looks it up via `self.adts.id_of` — this means forward references between type declarations (`type A` referencing `type B` declared later in the file) won't resolve within this single linear pass. Not exercised by this phase's tests (mutual ADT references aren't in the checklist), so left as a known, narrow limitation rather than building a second two-pass mechanism for it — noted in Task 20's checklist reconciliation.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Register ADT declarations and type variant constructors as functions"
```

---

## Task 15: Struct literal typing

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn struct_literal_with_all_fields_typechecks() {
    let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };\nprint(p);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn struct_literal_missing_a_field_names_it() {
    let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0 };";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
    assert!(infer.diagnostics[0].message.contains('y'));
}

#[test]
fn struct_literal_with_an_unknown_field_names_it() {
    let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0, z: 3.0 };";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
    assert!(infer.diagnostics[0].message.contains('z'));
}

#[test]
fn struct_literal_with_a_field_type_mismatch_errors() {
    let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1, y: 2.0 };";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types struct_literal`
Expected: FAIL — `Expr::Struct` isn't handled yet (falls into the fresh-var catch-all), so no field-completeness checking happens and every one of these tests' diagnostic-count assertions fails.

- [ ] **Step 3: Implement**

Add to `infer_expr`'s match: `Expr::Struct { name, fields } => self.infer_struct_literal(name, &fields, idx),`

```rust
fn infer_struct_literal(&mut self, name: Symbol, fields: &[(Symbol, Idx<Expr>)], idx: Idx<Expr>) -> Ty {
    let span = self.ast.span_of_expr(idx);
    let Some(id) = self.adts.id_of(name) else {
        let name_str = self.interner.resolve(name).to_string();
        self.diagnostics.push(Diagnostic::error(format!("unknown type `{name_str}`")).with_primary(span, "not found"));
        for &(_, v) in fields {
            self.infer_expr(v);
        }
        return self.subst.fresh();
    };
    if !self.adts.is_struct(id) {
        let name_str = self.interner.resolve(name).to_string();
        self.diagnostics.push(
            Diagnostic::error(format!("`{name_str}` is not a struct")).with_primary(span, "struct literal syntax used here"),
        );
        for &(_, v) in fields {
            self.infer_expr(v);
        }
        return self.subst.fresh();
    }

    let declared_fields: Vec<Symbol> = self.adts.struct_fields(id).collect();
    let mut provided = rustc_hash::FxHashSet::default();
    for &(field_name, value) in fields {
        let value_ty = self.infer_expr(value);
        provided.insert(field_name);
        match self.adts.field_ty(id, field_name).cloned() {
            Some(declared_ty) => {
                let value_span = self.ast.span_of_expr(value);
                self.unify(&declared_ty, &value_ty, Origin::Annotation { annot_span: span, value_span });
            }
            None => {
                let field_str = self.interner.resolve(field_name).to_string();
                let name_str = self.interner.resolve(name).to_string();
                self.diagnostics.push(
                    Diagnostic::error(format!("no field `{field_str}` on struct `{name_str}`"))
                        .with_primary(span, "unknown field in this struct literal"),
                );
            }
        }
    }
    for declared in declared_fields {
        if !provided.contains(&declared) {
            let field_str = self.interner.resolve(declared).to_string();
            let name_str = self.interner.resolve(name).to_string();
            self.diagnostics.push(
                Diagnostic::error(format!("missing field `{field_str}` in struct `{name_str}`"))
                    .with_primary(span, format!("`{name_str}` requires a `{field_str}` field")),
            );
        }
    }
    Ty::Adt(id, vec![])
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Infer struct literals: field-type checking, missing/unknown field errors"
```

---

## Task 16: Field access via deferred obligations

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn field_access_on_a_known_struct_type_resolves() {
    let src = "struct Point { x: Float, y: Float }\nfn get_x(p) {\n  let q = p;\n  q.x\n}\nlet p = Point { x: 1.0, y: 2.0 };\nprint(get_x(p) + 1.0);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn field_access_on_an_unresolved_type_needs_an_annotation() {
    let src = "fn get_x(p) { p.x }\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
    assert!(infer.diagnostics[0].message.to_lowercase().contains("annotation") || infer.diagnostics[0].message.to_lowercase().contains("infer"));
}

#[test]
fn field_access_on_a_non_struct_type_errors() {
    let src = "let x = 1;\nlet y = x.foo;";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}

#[test]
fn field_access_with_an_unknown_field_name_errors() {
    let src = "struct Point { x: Float }\nlet p = Point { x: 1.0 };\nlet z = p.bogus;";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types field_access_on field_access_with`
Expected: FAIL — `Expr::Field` isn't handled yet (falls into the fresh-var catch-all): no errors are produced where expected, and legitimate field accesses don't type-check the way the test asserts.

- [ ] **Step 3: Implement**

Add a `FieldObligation` type and a `field_obligations: Vec<FieldObligation>` field on `Infer`, initialized to `Vec::new()` alongside `Infer::new`'s other field initializers (`expr_types`, `fn_schemes`, etc.):

```rust
struct FieldObligation {
    base_ty: Ty,
    field: Symbol,
    result_ty: Ty,
    span: Span,
}
```

(`use ember_span::Span;` at the top of `infer.rs`.)

Add to `infer_expr`'s match: `Expr::Field { base, name } => self.infer_field(base, name, idx),`

```rust
fn infer_field(&mut self, base: Idx<Expr>, name: Symbol, idx: Idx<Expr>) -> Ty {
    let base_ty = self.infer_expr(base);
    let result_ty = self.subst.fresh();
    let span = self.ast.span_of_expr(idx);
    self.field_obligations.push(FieldObligation { base_ty, field: name, result_ty: result_ty.clone(), span });
    result_ty
}

/// Resolves every deferred field-access obligation against the FINAL
/// substitution, once the whole program has been walked. Field access
/// can't resolve at generation time in general — `base`'s type may still
/// be an open variable then (e.g. an un-annotated function parameter whose
/// struct type is only implied by how it's used) — so it's deferred to
/// here, the one place after eager unification has had every chance to
/// pin `base`'s type down.
fn resolve_field_obligations(&mut self) {
    let obligations = std::mem::take(&mut self.field_obligations);
    for ob in obligations {
        let resolved_base = self.subst.resolve(&ob.base_ty);
        match resolved_base {
            Ty::Adt(id, _) if self.adts.is_struct(id) => match self.adts.field_ty(id, ob.field).cloned() {
                Some(field_ty) => {
                    self.unify(&ob.result_ty, &field_ty, Origin::IndexTarget { span: ob.span });
                }
                None => {
                    let field_str = self.interner.resolve(ob.field).to_string();
                    let type_str = self.display(&resolved_base);
                    self.diagnostics.push(
                        Diagnostic::error(format!("no field `{field_str}` on type `{type_str}`"))
                            .with_primary(ob.span, "unknown field"),
                    );
                }
            },
            Ty::Var(_) => {
                self.diagnostics.push(
                    Diagnostic::error("cannot infer the type of this field access")
                        .with_primary(ob.span, "add a type annotation to resolve this")
                        .with_help("field access needs a known struct type; ember doesn't infer field types structurally across unrelated declarations"),
                );
            }
            other => {
                let type_str = self.display(&other);
                self.diagnostics.push(
                    Diagnostic::error(format!("type `{type_str}` has no fields")).with_primary(ob.span, "attempted field access here"),
                );
            }
        }
    }
}
```

Call `self.resolve_field_obligations();` at the very end of `resolve_program` (after pass 3).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Resolve field access as a deferred obligation against the final substitution"
```

---

## Task 17: Pattern typing and `match` arms

**Files:**
- Modify: `crates/ember-types/src/infer.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn match_on_an_adt_binds_variant_payloads_at_the_right_type() {
    // The last arm uses `_`, not the bare nullary variant `Point`: the
    // parser has no syntax distinguishing "match the nullary constructor
    // Point" from "bind a fresh local named Point" (both are a bare
    // identifier with no following `(`/`{`, and `pattern_primary` always
    // produces `Pattern::Bind` for that shape) — a pre-existing grammar
    // gap noted in Task 20's checklist reconciliation, not something to
    // route around inside a single test.
    let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r * r,\n    Rect(w, h) => w * h,\n    _ => 0.0,\n  }\n}\nprint(area(Circle(2.0)));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn match_arms_must_agree_on_their_result_type() {
    let src = "type Shape = | Circle(Float) | Point;\nfn describe(s) {\n  match s {\n    Circle(r) => r,\n    Point => \"origin\",\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert_eq!(infer.diagnostics.len(), 1);
}

#[test]
fn record_pattern_destructures_struct_fields_at_the_right_types() {
    let src = "struct Point { x: Float, y: Float }\nfn get_x(p) {\n  match p {\n    Point { x, y } => x,\n  }\n}\nlet p = Point { x: 1.0, y: 2.0 };\nprint(get_x(p) + 1.0);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}

#[test]
fn wildcard_and_literal_patterns_typecheck() {
    let src = "fn f(n) {\n  match n {\n    0 => \"zero\",\n    _ => \"other\",\n  }\n}\nprint(f(1));";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let mut infer = Infer::new(&ast, &mut interner);
    infer.resolve_program(&stmts);
    assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types match_on_an_adt match_arms_must record_pattern wildcard_and_literal`
Expected: FAIL — `Expr::Match` isn't handled yet (falls into the fresh-var catch-all): no pattern bindings are ever declared, so arm bodies referencing them fail as undeclared names.

- [ ] **Step 3: Implement**

Add to `infer_expr`'s match: `Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, &arms, idx),`

```rust
fn infer_match(&mut self, scrutinee: Idx<Expr>, arms: &[ember_ast::MatchArm], idx: Idx<Expr>) -> Ty {
    let scrutinee_ty = self.infer_expr(scrutinee);
    let result_ty = self.subst.fresh();
    let mut first_arm_span: Option<ember_span::Span> = None;
    for arm in arms {
        self.env.push_scope();
        self.infer_pattern(arm.pat, &scrutinee_ty);
        if let Some(guard) = arm.guard {
            let guard_ty = self.infer_expr(guard);
            let guard_span = self.ast.span_of_expr(guard);
            self.unify(&guard_ty, &Ty::Bool, Origin::WhileCond { span: guard_span });
        }
        let body_ty = self.infer_expr(arm.body);
        match first_arm_span {
            None => {
                first_arm_span = Some(arm.span);
                self.unify(&result_ty, &body_ty, Origin::MatchArms { first_span: arm.span, this_span: arm.span });
            }
            Some(first_span) => {
                self.unify(&result_ty, &body_ty, Origin::MatchArms { first_span, this_span: arm.span });
            }
        }
        self.env.pop_scope();
    }
    let _ = idx;
    result_ty
}

/// Constrains `scrutinee_ty` and declares every name this pattern binds,
/// into the CURRENT (already-pushed) scope. Exhaustiveness is explicitly
/// Phase 6's job — this only types.
fn infer_pattern(&mut self, pat: Idx<ember_ast::Pattern>, scrutinee_ty: &Ty) {
    use ember_ast::Pattern;
    let span = self.ast.span_of_pat(pat);
    match self.ast.pat(pat).clone() {
        Pattern::Wild | Pattern::Error => {}
        Pattern::Int(_) => {
            self.unify(scrutinee_ty, &Ty::Int, Origin::IndexTarget { span });
        }
        Pattern::Float(_) => {
            self.unify(scrutinee_ty, &Ty::Float, Origin::IndexTarget { span });
        }
        Pattern::Str(_) => {
            self.unify(scrutinee_ty, &Ty::String, Origin::IndexTarget { span });
        }
        Pattern::Bool(_) => {
            self.unify(scrutinee_ty, &Ty::Bool, Origin::IndexTarget { span });
        }
        Pattern::Bind(sym) => {
            self.env.declare(sym, Scheme { vars: vec![], ty: scrutinee_ty.clone() });
        }
        Pattern::Ctor { name, args } => {
            // `pattern_primary` in the parser only ever produces `Ctor`
            // when the identifier is followed by `(...)` — a bare
            // identifier with no parens (including a bare nullary variant
            // like `Point`) always parses as `Pattern::Bind` instead, with
            // no way to distinguish "match the Point constructor" from
            // "bind a fresh local named Point". That's a pre-existing
            // grammar gap (see Task 20's checklist note), not something
            // this arm needs to special-case — `Ctor`'s `args` can still
            // legitimately be empty here (`Circle()` written explicitly),
            // so the arity check below handles that correctly regardless.
            match self.adts.variant(name) {
                Some((id, payload)) => {
                    let payload = payload.to_vec();
                    self.unify(scrutinee_ty, &Ty::Adt(id, vec![]), Origin::IndexTarget { span });
                    if payload.len() != args.len() {
                        let name_str = self.interner.resolve(name).to_string();
                        self.diagnostics.push(
                            Diagnostic::error(format!(
                                "`{name_str}` takes {} argument(s), found {}",
                                payload.len(),
                                args.len()
                            ))
                            .with_primary(span, "pattern here"),
                        );
                    } else {
                        for (&arg_pat, arg_ty) in args.iter().zip(payload.iter()) {
                            self.infer_pattern(arg_pat, arg_ty);
                        }
                    }
                }
                None => {
                    let name_str = self.interner.resolve(name).to_string();
                    self.diagnostics
                        .push(Diagnostic::error(format!("unknown constructor `{name_str}`")).with_primary(span, "not found"));
                }
            }
        }
        Pattern::Record { name, fields } => match self.adts.id_of(name) {
            Some(id) if self.adts.is_struct(id) => {
                self.unify(scrutinee_ty, &Ty::Adt(id, vec![]), Origin::IndexTarget { span });
                for (field_name, field_pat) in fields {
                    match self.adts.field_ty(id, field_name).cloned() {
                        Some(field_ty) => self.infer_pattern(field_pat, &field_ty),
                        None => {
                            let field_str = self.interner.resolve(field_name).to_string();
                            self.diagnostics.push(
                                Diagnostic::error(format!("no field `{field_str}` on this struct")).with_primary(span, "in this pattern"),
                            );
                        }
                    }
                }
            }
            _ => {
                let name_str = self.interner.resolve(name).to_string();
                self.diagnostics
                    .push(Diagnostic::error(format!("`{name_str}` is not a struct")).with_primary(span, "record pattern used here"));
            }
        },
        // No Ty::Tuple and no Expr::Tuple exist in this grammar — a
        // pre-existing gap, not this phase's to fix. Each binding gets a
        // fresh, unconstrained type; the pattern is inert by construction
        // since nothing can ever produce a matching value.
        Pattern::Tuple(items) => {
            for item in items {
                let fresh = self.subst.fresh();
                self.infer_pattern(item, &fresh);
            }
        }
        Pattern::List { items, rest } => {
            let elem_ty = self.subst.fresh();
            self.unify(scrutinee_ty, &Ty::List(Box::new(elem_ty.clone())), Origin::IndexTarget { span });
            for item in items {
                self.infer_pattern(item, &elem_ty);
            }
            if let Some(rest_pat) = rest {
                self.infer_pattern(rest_pat, &Ty::List(Box::new(elem_ty)));
            }
        }
        Pattern::Or(alts) => {
            // Each alternative types against the same scrutinee. Bindings
            // sharing a name across alternatives follow last-write-wins
            // into this arm's single scope — the same simplification the
            // resolver already applies to or-patterns, kept consistent
            // rather than adding a stricter cross-check only here.
            for alt in alts {
                self.infer_pattern(alt, scrutinee_ty);
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too. If clippy flags the unused `idx` parameter in `infer_match` (only used for the now-removed `let _ = idx;` placeholder), remove the parameter and its call site's argument instead of suppressing the warning.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Infer pattern typing for match arms: Ctor, Record, List, Or, literals"
```

---

## Task 18: Public `infer()` entry point, `TypeInfo`, and crate exports

**Files:**
- Modify: `crates/ember-types/src/infer.rs`
- Modify: `crates/ember-types/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn infer_entry_point_ties_everything_together() {
    let src = "fn identity(x) { x }\nlet a = identity(1);\nprint(a);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, diags) = infer(&ast, &mut interner, &stmts);
    assert!(diags.is_empty(), "{diags:?}");
    assert!(!info.expr_types.is_empty());
    assert!(info.fn_schemes.contains_key(&interner.intern("identity")));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types infer_entry_point`
Expected: FAIL to compile — the free function `infer` and `TypeInfo` don't exist yet.

- [ ] **Step 3: Implement**

Add `TypeInfo` and the entry point to `infer.rs`:

```rust
pub struct TypeInfo {
    pub expr_types: FxHashMap<Idx<Expr>, Ty>,
    pub fn_schemes: FxHashMap<Symbol, Scheme>,
    pub adts: AdtRegistry,
    pub subst: Subst,
    pub trace: InferenceTrace,
}

pub fn infer(ast: &Ast, interner: &mut Interner, stmts: &[Idx<Stmt>]) -> (TypeInfo, Vec<Diagnostic>) {
    let mut checker = Infer::new(ast, interner);
    checker.resolve_program(stmts);
    let info = TypeInfo {
        expr_types: checker.expr_types,
        fn_schemes: checker.fn_schemes,
        adts: checker.adts,
        subst: checker.subst,
        trace: checker.trace,
    };
    (info, checker.diagnostics)
}
```

Replace `crates/ember-types/src/lib.rs`:

```rust
pub mod adt;
pub mod constraint;
pub mod display;
pub mod env;
pub mod infer;
pub mod subst;
pub mod trace;
pub mod ty;
pub mod unify;

pub use adt::{AdtDecl, AdtRegistry};
pub use constraint::{Constraint, Origin};
pub use display::{display_scheme, display_ty};
pub use env::TyEnv;
pub use infer::{infer, Infer, TypeInfo};
pub use subst::Subst;
pub use trace::{InferenceTrace, UnifyStep};
pub use ty::{AdtId, Scheme, Ty, TyVarId};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` too — all must be clean. Fix any unused-import or visibility issue the new re-exports surface.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add public infer() entry point, TypeInfo, and finalize crate exports"
```

---

## Task 19: `ember-cli typecheck` subcommand

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the dependency**

Add to `crates/ember-cli/Cargo.toml`'s `[dependencies]` (matching the existing entries' style):

```toml
ember-types = { path = "../ember-types" }
```

- [ ] **Step 2: Implement the subcommand**

Add a `Typecheck` variant to the `Command` enum:

```rust
/// Print each expression's inferred type, each top-level fn's generalized
/// scheme, and any type diagnostics.
Typecheck { file: String },
```

Add its dispatch arm in `main`'s match: `Command::Typecheck { file } => run_typecheck(&file),`

Add the handler:

```rust
fn run_typecheck(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }
    let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let resolve_diags = resolver.diagnostics();
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(resolve_diags, path, &src);
    }

    let (mut info, diags) = ember_types::infer(&ast, &mut interner, &stmts);

    let mut typed: Vec<_> = info.expr_types.iter().collect();
    typed.sort_by_key(|(idx, _)| ast.span_of_expr(**idx).start);
    for (idx, ty) in typed {
        let span = ast.span_of_expr(*idx);
        let ty_str = ember_types::display_ty(ty, &mut info.subst, &info.adts, &interner);
        println!("{}..{}\t{}", span.start, span.end, ty_str);
    }

    let mut schemes: Vec<_> = info.fn_schemes.iter().collect();
    schemes.sort_by_key(|(name, _)| interner.resolve(**name).to_string());
    for (name, scheme) in schemes {
        let scheme_str = ember_types::display_scheme(scheme, &mut info.subst, &info.adts, &interner);
        println!("{}: {}", interner.resolve(*name), scheme_str);
    }

    print_diagnostics(&diags, path, &src)
}
```

Since `resolver.diagnostics()` needs `Resolver::new(&ast, &mut interner)`, check `crates/ember-resolve/src/resolver.rs`'s exact public constructor/method signatures (`Resolver::new`, `resolve_program`, `diagnostics()`) match this call shape — they should, since `run_resolve` already uses this exact pattern; mirror it precisely rather than re-deriving it.

- [ ] **Step 3: Build and manually verify**

Run: `source "$HOME/.cargo/env" && cargo build -p ember-cli`
Expected: builds cleanly.

Run: `cargo run -p ember-cli -- typecheck examples/hello.em` (read the file first if you don't remember its exact contents — it's the `fact`/recursion example from Phase 4's CLI verification). Expected: every subexpression's type printed (the recursive `fact` calls should show `Int`, the top-level `let x = fact(5);` should show `Int`, `print(x)`'s call should show `Unit`), `fact`'s scheme printed as a monomorphic `(Int) -> Int` (it's a *recursive* function using a genuinely numeric operation via `*` — even though `*` itself is permissively typed per this phase's numeric-operator scope decision, `n == 0` and the base case `1` pin `fact`'s parameter and return type to concrete `Int` through ordinary unification, so it will NOT show as polymorphic), and no diagnostics.

- [ ] **Step 4: Run the full verification suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-cli
git commit -m "Add ember typecheck subcommand for manual inference inspection"
```

---

## Task 20: Final wrap-up — full verification and CHECKLIST.md update

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full verification suite**

Run: `cargo test --workspace`
Expected: PASS across all 16 crates.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Update `CHECKLIST.md`'s Phase 5 section**

Open `CHECKLIST.md` and go through Phase 5's items line by line, checking `- [x]` for everything this plan actually implemented, following the same honesty standard as every prior phase's wrap-up — verify each line against the real code rather than block-checking. Specifically leave notes (not blanket checks) for:
- The numeric-operator scope decision (Task 10): arithmetic/comparison operators unify operands with each other but don't independently constrain the shared type to `Int`/`Float` — no typeclass mechanism this phase, documented as a known permissiveness, not a soundness bug.
- Forward references between type declarations (Task 14): a `type`/`struct` referencing another declared later in the same file won't resolve — narrow, undocumented-until-now limitation of the single-pass `register_adts`.
- Field access requiring a resolvable concrete type (Task 16): no row polymorphism/structural inference across unrelated declarations — matches the design doc's stated non-goal, so this is expected, not a gap.
- `Pattern::Tuple` typing is inert (Task 17) — pre-existing AST gap (no `Ty::Tuple`/`Expr::Tuple`), not something this phase introduces or fixes.
- A bare nullary-variant pattern (e.g. `Point` with no parens) is indistinguishable from a fresh bind pattern at the grammar level (Task 17) — `pattern_primary` in the Phase 3 parser only produces `Pattern::Ctor` when parens follow the identifier, so `Point` in a match arm always parses as `Pattern::Bind(Point)`, silently shadowing rather than matching the constructor. Pre-existing parser/grammar gap, not introduced by this phase; fixing it would mean either resolver/parser changes (name-based disambiguation against declared constructors) or a grammar change (mandatory `Point()` / a case-sensitivity convention) — out of scope here.
- `InferenceTrace`/`UnifyStep` (Task 7) exist and are populated but have no consumer yet (no playground) — built per this round's explicit scope decision, not because Phase 16 has arrived.

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Mark Phase 5 checklist items complete"
```

- [ ] **Step 4: Final confirmation**

Run: `git log --oneline` and confirm a clean, incremental commit history from the resolver fix through this final checklist update.

---

## Summary of what this plan does NOT cover (by design)

- Exhaustiveness/unreachable-arm checking for `match` — Phase 6.
- Both execution backends, GC, formatter, LSP, WASM bindings, playground — Phases 7-17, each gets its own design/plan cycle.
- Generic user-defined ADTs/structs (`type Option<T>`) — no type-parameter syntax exists on `Stmt::TypeDecl`/`Stmt::StructDecl` this phase.
- Row polymorphism / structural inference for field access on a still-unresolved type.
- A numeric typeclass constraining arithmetic/comparison operators to `Int`/`Float` specifically (see the Task 10 scope note).
- Forward references between mutually-referencing type declarations (see the Task 14 scope note).
- Consuming `InferenceTrace` from an actual playground panel — Phase 16's job; the data shape exists now, nothing renders it yet.
