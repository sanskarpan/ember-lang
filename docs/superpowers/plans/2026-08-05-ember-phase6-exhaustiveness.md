# ember Phase 6 Implementation Plan — Exhaustiveness Checking

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement match exhaustiveness and unreachable-arm checking via Maranget's usefulness algorithm on a pattern matrix, inside the existing `ember-types` crate (per `SPEC.md §17`). Extend `ember-cli typecheck` to run it.

**Architecture:** A normalized internal pattern representation (`Pat`/`CtorId`) decouples the algorithm from `ember_ast::Pattern`'s AST-index-based shape and lets or-patterns expand into multiple matrix rows at lowering time. The core `is_useful` function is witness-carrying (a positive result names the concrete missing pattern, not just a bool). Runs as a separate pass after `ember_types::infer()`, walking every `Expr::Match` and using `TypeInfo.expr_types` for each scrutinee's already-resolved type.

**Tech Stack:** Rust, reuses `ember-types`'s `Ty`/`AdtRegistry`/`Subst`/`TypeInfo` and `ember-ast`'s `Pattern`/`MatchArm`/`Ast`.

---

## Task 1: `AdtRegistry` — ordered struct fields and enum-variant enumeration

**Files:**
- Modify: `crates/ember-types/src/adt.rs`

`AdtDecl::Struct { fields: FxHashMap<Symbol, Ty> }` doesn't preserve declaration order, and there's no way to enumerate an enum's variants by `AdtId` (only `variant(name)`, a lookup by the variant's own name). Both are needed for exhaustiveness: record patterns need a stable field order so "unmentioned fields are wildcards" lines up positionally across arms, and the algorithm needs to enumerate a whole enum's variant set to check completeness.

- [ ] **Step 1: Write the failing tests**

Add to `crates/ember-types/src/adt.rs`'s test module:

```rust
#[test]
fn struct_fields_preserves_declaration_order() {
    let mut interner = Interner::new();
    let point = interner.intern("Point");
    let x = interner.intern("x");
    let y = interner.intern("y");
    let z = interner.intern("z");
    let mut reg = AdtRegistry::new();
    let id = reg.register_struct(
        point,
        vec![(x, Ty::Float), (y, Ty::Float), (z, Ty::Float)],
    );
    let order: Vec<_> = reg.struct_fields(id).collect();
    assert_eq!(order, vec![x, y, z]);
}

#[test]
fn enum_variants_lists_every_variant_with_its_arity() {
    let mut interner = Interner::new();
    let shape = interner.intern("Shape");
    let circle = interner.intern("Circle");
    let rect = interner.intern("Rect");
    let point = interner.intern("Point");
    let mut reg = AdtRegistry::new();
    let id = reg.register_enum(
        shape,
        vec![
            (circle, vec![Ty::Float]),
            (rect, vec![Ty::Float, Ty::Float]),
            (point, vec![]),
        ],
    );
    let variants: Vec<_> = reg.enum_variants(id).collect();
    assert_eq!(variants, vec![(circle, 1), (rect, 2), (point, 0)]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p ember-types struct_fields_preserves enum_variants_lists`
Expected: `struct_fields_preserves_declaration_order` may already pass by coincidence of `FxHashMap`'s current iteration order for a 3-entry map, or may fail — either way, `enum_variants_lists_every_variant_with_its_arity` FAILS to compile (`enum_variants` doesn't exist).

- [ ] **Step 3: Implement**

Change `AdtDecl::Struct` to also carry field order:

```rust
pub enum AdtDecl {
    Enum {
        variants: Vec<(Symbol, Vec<Ty>)>,
    },
    Struct {
        field_order: Vec<Symbol>,
        fields: FxHashMap<Symbol, Ty>,
    },
}
```

Update `register_struct`:

```rust
pub fn register_struct(&mut self, name: Symbol, fields: Vec<(Symbol, Ty)>) -> AdtId {
    let id = AdtId(self.decls.len() as u32);
    let field_order: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
    self.decls.push(AdtDecl::Struct {
        field_order,
        fields: fields.into_iter().collect(),
    });
    self.names.push(name);
    self.by_name.insert(name, id);
    id
}
```

Update `struct_fields` to iterate `field_order` instead of `fields.keys()`:

```rust
pub fn struct_fields(&self, id: AdtId) -> impl Iterator<Item = Symbol> + '_ {
    match &self.decls[id.0 as usize] {
        AdtDecl::Struct { field_order, .. } => Some(field_order.iter().copied()),
        AdtDecl::Enum { .. } => None,
    }
    .into_iter()
    .flatten()
}
```

Add a new method:

```rust
/// Every variant of an enum type, in declaration order, with each one's
/// payload arity. `None` (empty iterator) for a struct `id`.
pub fn enum_variants(&self, id: AdtId) -> impl Iterator<Item = (Symbol, usize)> + '_ {
    match &self.decls[id.0 as usize] {
        AdtDecl::Enum { variants } => Some(variants.iter().map(|(name, payload)| (*name, payload.len()))),
        AdtDecl::Struct { .. } => None,
    }
    .into_iter()
    .flatten()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS, all tests green (existing Phase 5 tests unaffected — `struct_fields`'s signature is unchanged, only its ordering guarantee improved). Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Give AdtRegistry ordered struct fields and enum-variant enumeration"
```

---

## Task 2: `pat.rs` — `Pat`, `CtorId`

**Files:**
- Modify: `crates/ember-types/src/pat.rs` (new file — create it)
- Modify: `crates/ember-types/src/lib.rs` — add `pub mod pat;`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::AdtId;

    #[test]
    fn ctor_id_equality_is_by_value() {
        let mut interner = ember_ast::Interner::new();
        let sym = interner.intern("Circle");
        assert_eq!(CtorId::Bool(true), CtorId::Bool(true));
        assert_ne!(CtorId::Bool(true), CtorId::Bool(false));
        assert_eq!(CtorId::Variant(AdtId(0), sym), CtorId::Variant(AdtId(0), sym));
    }

    #[test]
    fn wild_and_ctor_pats_construct() {
        let p = Pat::Ctor(CtorId::Nil, vec![]);
        assert!(matches!(p, Pat::Ctor(CtorId::Nil, _)));
        assert!(matches!(Pat::Wild, Pat::Wild));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types ctor_id_equality wild_and_ctor_pats`
Expected: FAIL to compile — `Pat`/`CtorId` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::ty::AdtId;
use ember_ast::Symbol;

/// The exhaustiveness algorithm's own normalized pattern shape — simpler
/// than `ember_ast::Pattern`. No `Or` variant: or-patterns expand into
/// multiple `Pat` rows at lowering time (see `lower_pattern`), not
/// threaded through the algorithm itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Wild,
    Ctor(CtorId, Vec<Pat>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CtorId {
    Variant(AdtId, Symbol),
    Struct(AdtId),
    Bool(bool),
    Int(i64),
    /// `f64`'s bit pattern, not the float itself — `f64` isn't `Eq`/`Hash`,
    /// and this algorithm needs both to track "which constructors are
    /// already present in a matrix column" via a set.
    Float(u64),
    Str(Symbol),
    Nil,
    Cons,
    /// Pre-existing gap from Phase 5: no `Ty::Tuple`/`Expr::Tuple` exist in
    /// the grammar, so a tuple pattern is treated as a single,
    /// always-complete constructor of whatever arity first appears —
    /// inert by construction, matching how Phase 5 already left it.
    Tuple(usize),
}
```

- [ ] **Step 4: Add `pub mod pat;` to `crates/ember-types/src/lib.rs`** (alongside the other `pub mod` lines — don't add a `pub use` yet, later tasks add real exports).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check` too.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-types
git commit -m "Add Pat and CtorId, the exhaustiveness algorithm's normalized pattern shape"
```

---

## Task 3: `pat.rs` — `lower_pattern` for literals, `Wild`/`Bind`, and `Ctor`

**Files:**
- Modify: `crates/ember-types/src/pat.rs`

`lower_pattern` converts an `ember_ast::Pattern` (via its `Idx<Pattern>`) into one or more `Pat`s (plural, since a nested or-pattern anywhere inside — not just at the top — must expand into a full cartesian product of alternatives). This task covers the base cases and constructor patterns; Task 4 covers `Tuple`/`List`/`Record`/`Or`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod lower_tests {
    use super::*;
    use crate::adt::AdtRegistry;

    #[test]
    fn wild_and_bind_lower_to_a_single_wild() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse("match 1 { _ => 1, x => x, }");
        assert!(diags.is_empty());
        let arms = match_arms(&ast, &stmts);
        let adts = AdtRegistry::new();
        assert_eq!(lower_pattern(&ast, &adts, arms[0].pat), vec![Pat::Wild]);
        assert_eq!(lower_pattern(&ast, &adts, arms[1].pat), vec![Pat::Wild]);
        let _ = interner;
    }

    #[test]
    fn literals_lower_to_their_own_ctor() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse("match 1 { 0 => 1, _ => 2, }");
        assert!(diags.is_empty());
        let arms = match_arms(&ast, &stmts);
        let adts = AdtRegistry::new();
        assert_eq!(lower_pattern(&ast, &adts, arms[0].pat), vec![Pat::Ctor(CtorId::Int(0), vec![])]);
        let _ = interner;
    }

    #[test]
    fn ctor_pattern_lowers_using_the_registry() {
        let (ast, mut interner, stmts, diags) =
            ember_parser::parse("type Shape = | Circle(Float);\nmatch s { Circle(r) => r, _ => 0.0, }");
        assert!(diags.is_empty());
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(circle, vec![crate::ty::Ty::Float])]);
        let arms = match_arms(&ast, &stmts);
        assert_eq!(
            lower_pattern(&ast, &adts, arms[0].pat),
            vec![Pat::Ctor(CtorId::Variant(id, circle), vec![Pat::Wild])]
        );
    }

    /// Test helper: parse produces one top-level `ExprStmt` wrapping a
    /// `match`, possibly preceded by other statements (e.g. a `type`
    /// decl) — find the `Expr::Match` and return its arms.
    fn match_arms<'a>(ast: &'a ember_ast::Ast, stmts: &[ember_ast::Idx<ember_ast::Stmt>]) -> &'a [ember_ast::MatchArm] {
        for &s in stmts {
            if let ember_ast::Stmt::ExprStmt(e) = ast.stmt(s) {
                if let ember_ast::Expr::Match { arms, .. } = ast.expr(*e) {
                    return arms;
                }
            }
        }
        panic!("no match expression found in parsed statements")
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types wild_and_bind_lower literals_lower ctor_pattern_lowers`
Expected: FAIL to compile — `lower_pattern` doesn't exist yet.

- [ ] **Step 3: Implement**

Add `ember-parser` is already a dev-dependency of `ember-types` (from Phase 5) — no `Cargo.toml` change needed.

```rust
use crate::adt::AdtRegistry;
use ember_ast::{Ast, Expr as AstExpr, Idx, Pattern};

pub fn lower_pattern(ast: &Ast, adts: &AdtRegistry, idx: Idx<Pattern>) -> Vec<Pat> {
    match ast.pat(idx).clone() {
        Pattern::Wild | Pattern::Bind(_) | Pattern::Error => vec![Pat::Wild],
        Pattern::Int(n) => vec![Pat::Ctor(CtorId::Int(n), vec![])],
        Pattern::Float(f) => vec![Pat::Ctor(CtorId::Float(f.to_bits()), vec![])],
        Pattern::Str(s) => vec![Pat::Ctor(CtorId::Str(s), vec![])],
        Pattern::Bool(b) => vec![Pat::Ctor(CtorId::Bool(b), vec![])],
        Pattern::Ctor { name, args } => match adts.variant(name) {
            Some((id, _payload)) => {
                let arg_alts: Vec<Vec<Pat>> = args.iter().map(|&a| lower_pattern(ast, adts, a)).collect();
                cartesian_ctor(CtorId::Variant(id, name), arg_alts)
            }
            // Unknown constructor — already reported by type inference
            // (or exhaustiveness runs on code with no scrutinee type at
            // all and this is unreachable in practice). Don't cascade a
            // second diagnostic; treat as inert.
            None => vec![Pat::Wild],
        },
        // Tuple/List/Record/Or land in Task 4.
        _ => vec![Pat::Wild],
    }
}

/// The cartesian product of every argument's own lowered alternatives,
/// each combination wrapped as one `Ctor(ctor, combo)`. Needed because an
/// or-pattern can appear NESTED inside a constructor's arguments (e.g.
/// `Circle(1.0 | 2.0)`), not just at an arm's top level — every level must
/// expand fully.
fn cartesian_ctor(ctor: CtorId, arg_alts: Vec<Vec<Pat>>) -> Vec<Pat> {
    let mut combos: Vec<Vec<Pat>> = vec![vec![]];
    for alts in arg_alts {
        let mut next = Vec::new();
        for prefix in &combos {
            for alt in &alts {
                let mut combo = prefix.clone();
                combo.push(alt.clone());
                next.push(combo);
            }
        }
        combos = next;
    }
    combos.into_iter().map(|args| Pat::Ctor(ctor.clone(), args)).collect()
}
```

Note `AstExpr` is imported but unused by this task's code — that's fine, Task 4 uses it (or remove the import now and let Task 4 re-add it; either is acceptable, but if `cargo clippy` flags an unused import, remove it here and note it needs re-adding in Task 4).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged (including the possibly-unused `AstExpr` import noted above).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Lower literal, wildcard/bind, and constructor patterns for exhaustiveness"
```

---

## Task 4: `pat.rs` — `lower_pattern` for `Tuple`, `List`, `Record`, `Or`

**Files:**
- Modify: `crates/ember-types/src/pat.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn tuple_pattern_lowers_to_a_single_always_complete_ctor() {
    // No Ty::Tuple/Expr::Tuple exist — this is inert by construction, but
    // must still lower without panicking and without spuriously reporting
    // gaps for a type nothing can construct.
    let (ast, mut interner, stmts, diags) = ember_parser::parse("match s { (a, b) => a, _ => 1, }");
    assert!(diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = match_arms(&ast, &stmts);
    let lowered = lower_pattern(&ast, &adts, arms[0].pat);
    assert_eq!(lowered.len(), 1);
    assert!(matches!(&lowered[0], Pat::Ctor(CtorId::Tuple(2), _)));
    let _ = interner;
}

#[test]
fn empty_list_pattern_lowers_to_nil() {
    let (ast, mut interner, stmts, diags) = ember_parser::parse("match xs { [] => 1, _ => 2, }");
    assert!(diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = match_arms(&ast, &stmts);
    assert_eq!(lower_pattern(&ast, &adts, arms[0].pat), vec![Pat::Ctor(CtorId::Nil, vec![])]);
    let _ = interner;
}

#[test]
fn list_pattern_with_rest_lowers_to_a_cons_chain() {
    let (ast, mut interner, stmts, diags) = ember_parser::parse("match xs { [a, ..rest] => a, _ => 2, }");
    assert!(diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = match_arms(&ast, &stmts);
    let lowered = lower_pattern(&ast, &adts, arms[0].pat);
    assert_eq!(lowered.len(), 1);
    // Cons(Wild, Wild) — the head binding and the rest binding both lower
    // to Wild (a Bind matches anything; its NAME doesn't matter here).
    assert_eq!(
        lowered[0],
        Pat::Ctor(CtorId::Cons, vec![Pat::Wild, Pat::Wild])
    );
    let _ = interner;
}

#[test]
fn record_pattern_fills_unmentioned_fields_with_wild() {
    let (ast, mut interner, stmts, diags) =
        ember_parser::parse("struct Point { x: Float, y: Float }\nmatch p { Point { x } => x, _ => 0.0, }");
    assert!(diags.is_empty());
    let point = interner.intern("Point");
    let x = interner.intern("x");
    let y = interner.intern("y");
    let mut adts = AdtRegistry::new();
    let id = adts.register_struct(point, vec![(x, crate::ty::Ty::Float), (y, crate::ty::Ty::Float)]);
    let arms = match_arms(&ast, &stmts);
    let lowered = lower_pattern(&ast, &adts, arms[0].pat);
    assert_eq!(lowered.len(), 1);
    assert_eq!(
        lowered[0],
        Pat::Ctor(CtorId::Struct(id), vec![Pat::Wild, Pat::Wild])
    );
}

#[test]
fn or_pattern_expands_into_multiple_rows() {
    let (ast, mut interner, stmts, diags) = ember_parser::parse("match n { 0 | 1 => \"small\", _ => \"other\", }");
    assert!(diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = match_arms(&ast, &stmts);
    let lowered = lower_pattern(&ast, &adts, arms[0].pat);
    assert_eq!(
        lowered,
        vec![
            Pat::Ctor(CtorId::Int(0), vec![]),
            Pat::Ctor(CtorId::Int(1), vec![]),
        ]
    );
    let _ = interner;
}
```

Add these to the existing `mod lower_tests` from Task 3, reusing its `match_arms` helper.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types tuple_pattern_lowers empty_list_pattern list_pattern_with_rest record_pattern_fills or_pattern_expands`
Expected: FAIL — `Tuple`/`List`/`Record`/`Or` all currently fall into the placeholder `_ => vec![Pat::Wild]` catch-all from Task 3, so none of the specific-shape assertions match.

- [ ] **Step 3: Implement**

Replace the `_ => vec![Pat::Wild],` catch-all in `lower_pattern`'s match with real arms for the remaining `Pattern` variants:

```rust
Pattern::Tuple(items) => {
    let arg_alts: Vec<Vec<Pat>> = items.iter().map(|&i| lower_pattern(ast, adts, i)).collect();
    let arity = items.len();
    cartesian_ctor(CtorId::Tuple(arity), arg_alts)
}
Pattern::List { items, rest } => lower_list(ast, adts, &items, rest),
Pattern::Record { name, fields } => match adts.id_of(name) {
    Some(id) if adts.is_struct(id) => {
        let declared: Vec<ember_ast::Symbol> = adts.struct_fields(id).collect();
        let arg_alts: Vec<Vec<Pat>> = declared
            .iter()
            .map(|field_name| {
                fields
                    .iter()
                    .find(|(f, _)| f == field_name)
                    .map(|(_, pat_idx)| lower_pattern(ast, adts, *pat_idx))
                    .unwrap_or_else(|| vec![Pat::Wild])
            })
            .collect();
        cartesian_ctor(CtorId::Struct(id), arg_alts)
    }
    _ => vec![Pat::Wild],
},
Pattern::Or(alts) => alts.iter().flat_map(|&a| lower_pattern(ast, adts, a)).collect(),
```

Add the `lower_list` helper (handles the recursive `[a, b, ..rest]` → `Cons(a, Cons(b, rest))` chain):

```rust
/// Lowers a list pattern into a Cons-chain: `[]` → `Nil`; `[a]` (no rest)
/// → `Cons(a, Nil)` (exact length 1); `[a, ..rest]` → `Cons(a, rest)`
/// where `rest`'s own lowering (typically `Wild`, from a plain binding)
/// represents "any remaining tail".
fn lower_list(
    ast: &Ast,
    adts: &AdtRegistry,
    items: &[Idx<Pattern>],
    rest: Option<Idx<Pattern>>,
) -> Vec<Pat> {
    match items.split_first() {
        None => match rest {
            None => vec![Pat::Ctor(CtorId::Nil, vec![])],
            Some(r) => lower_pattern(ast, adts, r),
        },
        Some((&head, tail_items)) => {
            let head_alts = lower_pattern(ast, adts, head);
            let tail_alts = lower_list(ast, adts, tail_items, rest);
            cartesian_ctor(CtorId::Cons, vec![head_alts, tail_alts])
        }
    }
}
```

Now that `Tuple`/`List`/`Record`/`Or` are handled, the `Pattern::Ctor` unknown-constructor case remains the only fallback returning `vec![Pat::Wild]` explicitly, and there's no more unreachable-catch-all — the whole `lower_pattern` match should now be exhaustive against every `Pattern` variant without a trailing `_`. Remove the `Pattern::` import qualifier issues if any arise, and make sure `AstExpr`/`Idx` imports are actually used (if `AstExpr` ends up unused after this task too, remove it — Task 3's note about it was speculative, not a promise it'd be needed here).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Lower tuple, list, record, and or-patterns for exhaustiveness"
```

---

## Task 5: `ctor_set.rs` — per-type constructor-set completeness

**Files:**
- Modify: `crates/ember-types/src/ctor_set.rs` (new file — create it)
- Modify: `crates/ember-types/src/lib.rs` — add `pub mod ctor_set;`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::pat::CtorId;
    use crate::ty::Ty;
    use ember_ast::Interner;

    #[test]
    fn bool_has_two_constructors() {
        let adts = AdtRegistry::new();
        match ctor_set(&Ty::Bool, &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(ctors.len(), 2);
                assert!(ctors.contains(&(CtorId::Bool(true), 0)));
                assert!(ctors.contains(&(CtorId::Bool(false), 0)));
            }
            CtorSet::Infinite => panic!("Bool should be finite"),
        }
    }

    #[test]
    fn list_has_nil_and_cons() {
        let adts = AdtRegistry::new();
        match ctor_set(&Ty::List(Box::new(Ty::Int)), &adts) {
            CtorSet::Finite(ctors) => {
                assert!(ctors.contains(&(CtorId::Nil, 0)));
                assert!(ctors.contains(&(CtorId::Cons, 2)));
            }
            CtorSet::Infinite => panic!("List should be finite"),
        }
    }

    #[test]
    fn int_float_string_are_infinite() {
        let adts = AdtRegistry::new();
        assert!(matches!(ctor_set(&Ty::Int, &adts), CtorSet::Infinite));
        assert!(matches!(ctor_set(&Ty::Float, &adts), CtorSet::Infinite));
        assert!(matches!(ctor_set(&Ty::String, &adts), CtorSet::Infinite));
    }

    #[test]
    fn struct_has_one_constructor_with_field_arity() {
        let mut interner = Interner::new();
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let y = interner.intern("y");
        let mut adts = AdtRegistry::new();
        let id = adts.register_struct(point, vec![(x, Ty::Float), (y, Ty::Float)]);
        match ctor_set(&Ty::Adt(id, vec![]), &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(ctors, vec![(CtorId::Struct(id), 2)]);
            }
            CtorSet::Infinite => panic!("struct should be finite"),
        }
    }

    #[test]
    fn enum_has_one_constructor_per_variant() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(circle, vec![Ty::Float]), (point, vec![])]);
        match ctor_set(&Ty::Adt(id, vec![]), &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(ctors, vec![(CtorId::Variant(id, circle), 1), (CtorId::Variant(id, point), 0)]);
            }
            CtorSet::Infinite => panic!("enum should be finite"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types bool_has_two list_has_nil int_float_string struct_has_one enum_has_one`
Expected: FAIL to compile — `ctor_set`/`CtorSet` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::adt::AdtRegistry;
use crate::pat::CtorId;
use crate::ty::Ty;

/// A type's complete constructor set, or `Infinite` if it has none
/// (`Int`/`Float`/`String`, or anything else without a finite pattern
/// vocabulary — a wildcard is always required to close such a type).
#[derive(Debug, Clone, PartialEq)]
pub enum CtorSet {
    Finite(Vec<(CtorId, usize)>),
    Infinite,
}

pub fn ctor_set(ty: &Ty, adts: &AdtRegistry) -> CtorSet {
    match ty {
        Ty::Bool => CtorSet::Finite(vec![(CtorId::Bool(true), 0), (CtorId::Bool(false), 0)]),
        Ty::List(_) => CtorSet::Finite(vec![(CtorId::Nil, 0), (CtorId::Cons, 2)]),
        Ty::Adt(id, _) if adts.is_struct(*id) => {
            let arity = adts.struct_fields(*id).count();
            CtorSet::Finite(vec![(CtorId::Struct(*id), arity)])
        }
        Ty::Adt(id, _) => CtorSet::Finite(
            adts.enum_variants(*id)
                .map(|(name, arity)| (CtorId::Variant(*id, name), arity))
                .collect(),
        ),
        _ => CtorSet::Infinite,
    }
}

/// The types of a constructor's own sub-fields, needed to keep type
/// context correct when the algorithm recurses one level deeper (e.g. a
/// `Cons`'s two fields are the list's element type and the list type
/// itself). `Tuple`'s slots use `Ty::Unit` as a placeholder — there's no
/// real per-slot type to give them (no `Ty::Tuple` exists), but since
/// `Tuple` is always treated as a single complete constructor, its own
/// slot types are never load-bearing beyond not panicking.
pub fn ctor_arg_types(ctor: &CtorId, ty: &Ty, adts: &AdtRegistry) -> Vec<Ty> {
    match ctor {
        CtorId::Variant(_, name) => adts
            .variant(*name)
            .map(|(_, payload)| payload.to_vec())
            .unwrap_or_default(),
        CtorId::Struct(id) => adts
            .struct_fields(*id)
            .filter_map(|f| adts.field_ty(*id, f).cloned())
            .collect(),
        CtorId::Cons => {
            if let Ty::List(elem) = ty {
                vec![(**elem).clone(), ty.clone()]
            } else {
                vec![]
            }
        }
        CtorId::Nil | CtorId::Bool(_) | CtorId::Int(_) | CtorId::Float(_) | CtorId::Str(_) => vec![],
        CtorId::Tuple(n) => vec![Ty::Unit; *n],
    }
}
```

- [ ] **Step 4: Add `pub mod ctor_set;` to `crates/ember-types/src/lib.rs`.**

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-types
git commit -m "Add per-type constructor-set completeness and sub-field type lookup"
```

---

## Task 6: `matrix.rs` — `specialize` and `default_matrix`

**Files:**
- Modify: `crates/ember-types/src/matrix.rs` (new file — create it)
- Modify: `crates/ember-types/src/lib.rs` — add `pub mod matrix;`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pat::{CtorId, Pat};

    #[test]
    fn specialize_keeps_matching_ctor_rows_and_expands_wild_rows() {
        let matrix = vec![
            vec![Pat::Ctor(CtorId::Int(0), vec![]), Pat::Wild],
            vec![Pat::Wild, Pat::Ctor(CtorId::Int(9), vec![])],
            vec![Pat::Ctor(CtorId::Int(1), vec![]), Pat::Wild],
        ];
        let out = specialize(&CtorId::Int(0), 0, &matrix);
        // Row 0 matches Int(0) exactly (arity 0, so no columns added) -> [Wild]
        // Row 1 is Wild-first -> expands to 0 wildcard columns + its rest -> [Int(9)]
        // Row 2 doesn't match Int(0) -> dropped
        assert_eq!(
            out,
            vec![
                vec![Pat::Wild],
                vec![Pat::Ctor(CtorId::Int(9), vec![])],
            ]
        );
    }

    #[test]
    fn specialize_expands_a_wild_row_to_the_ctors_own_arity() {
        let matrix = vec![vec![Pat::Wild, Pat::Ctor(CtorId::Bool(true), vec![])]];
        let out = specialize(&CtorId::Cons, 2, &matrix);
        assert_eq!(
            out,
            vec![vec![Pat::Wild, Pat::Wild, Pat::Ctor(CtorId::Bool(true), vec![])]]
        );
    }

    #[test]
    fn default_matrix_keeps_only_wild_first_rows_and_drops_the_column() {
        let matrix = vec![
            vec![Pat::Wild, Pat::Ctor(CtorId::Int(1), vec![])],
            vec![Pat::Ctor(CtorId::Int(0), vec![]), Pat::Wild],
            vec![Pat::Wild, Pat::Wild],
        ];
        let out = default_matrix(&matrix);
        assert_eq!(
            out,
            vec![
                vec![Pat::Ctor(CtorId::Int(1), vec![])],
                vec![Pat::Wild],
            ]
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types specialize_keeps specialize_expands default_matrix_keeps`
Expected: FAIL to compile — `specialize`/`default_matrix` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::pat::{CtorId, Pat};

pub type PatMatrix = Vec<Vec<Pat>>;

/// `S(c, matrix)`: keep rows whose first pattern is `c` itself or `Wild`
/// (matches anything), replacing that first column with `c`'s own
/// sub-patterns (or `arity` wildcards, for a `Wild` row).
pub fn specialize(ctor: &CtorId, arity: usize, matrix: &[Vec<Pat>]) -> PatMatrix {
    matrix
        .iter()
        .filter_map(|row| {
            let (first, rest) = row.split_first()?;
            match first {
                Pat::Wild => {
                    let mut new_row = vec![Pat::Wild; arity];
                    new_row.extend_from_slice(rest);
                    Some(new_row)
                }
                Pat::Ctor(c, args) if c == ctor => {
                    let mut new_row = args.clone();
                    new_row.extend_from_slice(rest);
                    Some(new_row)
                }
                Pat::Ctor(_, _) => None,
            }
        })
        .collect()
}

/// `D(matrix)`: keep rows whose first pattern is `Wild`, dropping that
/// column — the rows that say nothing about which constructor is used.
pub fn default_matrix(matrix: &[Vec<Pat>]) -> PatMatrix {
    matrix
        .iter()
        .filter_map(|row| {
            let (first, rest) = row.split_first()?;
            match first {
                Pat::Wild => Some(rest.to_vec()),
                Pat::Ctor(_, _) => None,
            }
        })
        .collect()
}
```

- [ ] **Step 4: Add `pub mod matrix;` to `crates/ember-types/src/lib.rs`.**

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-types
git commit -m "Add PatMatrix specialize and default_matrix operations"
```

---

## Task 7: `exhaustive.rs` — `is_useful`, the witness-carrying core algorithm

**Files:**
- Modify: `crates/ember-types/src/exhaustive.rs` (new file — create it)
- Modify: `crates/ember-types/src/lib.rs` — add `pub mod exhaustive;`

This is the heart of the phase. Read this task's implementation carefully before starting — the recursion structure and witness reconstruction are exact, not illustrative; deviating from the given code risks subtle correctness bugs in an algorithm that's hard to spot-check by eye.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::pat::{CtorId, Pat};
    use crate::ty::Ty;

    #[test]
    fn empty_matrix_makes_the_empty_query_useful() {
        let adts = AdtRegistry::new();
        let result = is_useful(&[], &[], &[], &adts);
        assert!(matches!(result, Usefulness::Useful(_)));
    }

    #[test]
    fn a_matrix_with_a_row_makes_the_empty_query_not_useful() {
        let adts = AdtRegistry::new();
        let matrix: Vec<Vec<Pat>> = vec![vec![]];
        let result = is_useful(&matrix, &[], &[], &adts);
        assert!(matches!(result, Usefulness::NotUseful));
    }

    #[test]
    fn wild_is_not_useful_once_bool_is_fully_covered() {
        let adts = AdtRegistry::new();
        let matrix = vec![
            vec![Pat::Ctor(CtorId::Bool(true), vec![])],
            vec![Pat::Ctor(CtorId::Bool(false), vec![])],
        ];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Bool], &adts);
        assert!(matches!(result, Usefulness::NotUseful));
    }

    #[test]
    fn wild_is_useful_when_bool_is_only_partially_covered() {
        let adts = AdtRegistry::new();
        let matrix = vec![vec![Pat::Ctor(CtorId::Bool(true), vec![])]];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Bool], &adts);
        match result {
            Usefulness::Useful(witnesses) => {
                assert_eq!(witnesses, vec![vec![Pat::Ctor(CtorId::Bool(false), vec![])]]);
            }
            Usefulness::NotUseful => panic!("expected useful"),
        }
    }

    #[test]
    fn wild_is_useful_against_an_infinite_domain_with_no_rows() {
        let adts = AdtRegistry::new();
        let result = is_useful(&[], &[Pat::Wild], &[Ty::Int], &adts);
        assert!(matches!(result, Usefulness::Useful(_)));
    }

    #[test]
    fn multiple_missing_variants_all_appear_as_witnesses() {
        let mut interner = ember_ast::Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let rect = interner.intern("Rect");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(
            shape,
            vec![(circle, vec![Ty::Float]), (rect, vec![Ty::Float, Ty::Float]), (point, vec![])],
        );
        let matrix = vec![vec![Pat::Ctor(CtorId::Variant(id, circle), vec![Pat::Wild])]];
        let result = is_useful(&matrix, &[Pat::Wild], &[Ty::Adt(id, vec![])], &adts);
        match result {
            Usefulness::Useful(witnesses) => {
                assert_eq!(witnesses.len(), 2, "Rect and Point should both be missing: {witnesses:?}");
                let has_rect = witnesses.iter().any(|w| matches!(&w[0], Pat::Ctor(CtorId::Variant(_, n), _) if *n == rect));
                let has_point = witnesses.iter().any(|w| matches!(&w[0], Pat::Ctor(CtorId::Variant(_, n), _) if *n == point));
                assert!(has_rect && has_point);
            }
            Usefulness::NotUseful => panic!("expected useful"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types empty_matrix_makes a_matrix_with_a_row wild_is_not_useful wild_is_useful multiple_missing_variants`
Expected: FAIL to compile — `is_useful`/`Usefulness` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::adt::AdtRegistry;
use crate::ctor_set::{ctor_arg_types, ctor_set, CtorSet};
use crate::matrix::{default_matrix, specialize};
use crate::pat::{CtorId, Pat};
use crate::ty::Ty;
use rustc_hash::FxHashSet;

/// `NotUseful`: the query row adds nothing the matrix doesn't already
/// cover. `Useful`: it does, and each `Vec<Pat>` is one concrete witness
/// row (same column count as the query) demonstrating a value the matrix
/// misses — there can be more than one (e.g. two different missing ADT
/// variants at once).
#[derive(Debug, Clone, PartialEq)]
pub enum Usefulness {
    NotUseful,
    Useful(Vec<Vec<Pat>>),
}

pub fn is_useful(matrix: &[Vec<Pat>], q: &[Pat], col_types: &[Ty], adts: &AdtRegistry) -> Usefulness {
    if q.is_empty() {
        return if matrix.is_empty() {
            Usefulness::Useful(vec![vec![]])
        } else {
            Usefulness::NotUseful
        };
    }

    let (q_first, q_rest) = q.split_first().expect("checked non-empty above");
    let (ty_first, ty_rest) = col_types.split_first().expect("q and col_types must have matching length");

    match q_first {
        Pat::Ctor(ctor, args) => {
            let arity = args.len();
            let specialized = specialize(ctor, arity, matrix);
            let mut new_q = args.clone();
            new_q.extend_from_slice(q_rest);
            let mut new_types = ctor_arg_types(ctor, ty_first, adts);
            new_types.extend_from_slice(ty_rest);
            match is_useful(&specialized, &new_q, &new_types, adts) {
                Usefulness::NotUseful => Usefulness::NotUseful,
                Usefulness::Useful(witnesses) => Usefulness::Useful(
                    witnesses
                        .into_iter()
                        .map(|w| {
                            let (sub, rest) = w.split_at(arity);
                            let mut row = vec![Pat::Ctor(ctor.clone(), sub.to_vec())];
                            row.extend_from_slice(rest);
                            row
                        })
                        .collect(),
                ),
            }
        }
        Pat::Wild => match ctor_set(ty_first, adts) {
            CtorSet::Finite(ctors) => {
                let present: FxHashSet<CtorId> = matrix
                    .iter()
                    .filter_map(|row| match row.first() {
                        Some(Pat::Ctor(c, _)) => Some(c.clone()),
                        _ => None,
                    })
                    .collect();
                let missing: Vec<&(CtorId, usize)> = ctors.iter().filter(|(c, _)| !present.contains(c)).collect();

                if missing.is_empty() {
                    // Every constructor is present — try each one; useful if ANY branch is useful.
                    let mut all_witnesses = Vec::new();
                    for (ctor, arity) in &ctors {
                        let specialized = specialize(ctor, *arity, matrix);
                        let mut new_q = vec![Pat::Wild; *arity];
                        new_q.extend_from_slice(q_rest);
                        let mut new_types = ctor_arg_types(ctor, ty_first, adts);
                        new_types.extend_from_slice(ty_rest);
                        if let Usefulness::Useful(witnesses) = is_useful(&specialized, &new_q, &new_types, adts) {
                            for w in witnesses {
                                let (sub, rest) = w.split_at(*arity);
                                let mut row = vec![Pat::Ctor(ctor.clone(), sub.to_vec())];
                                row.extend_from_slice(rest);
                                all_witnesses.push(row);
                            }
                        }
                    }
                    if all_witnesses.is_empty() {
                        Usefulness::NotUseful
                    } else {
                        Usefulness::Useful(all_witnesses)
                    }
                } else {
                    // Some constructors are missing entirely. Whether the
                    // REST of the row is satisfiable doesn't depend on
                    // WHICH missing constructor we pick (a constructor
                    // with zero rows contributes nothing but wildcard
                    // columns to specialize()), so compute it once via
                    // the default matrix, then pair every missing
                    // constructor with every rest-witness.
                    let def = default_matrix(matrix);
                    match is_useful(&def, q_rest, ty_rest, adts) {
                        Usefulness::NotUseful => Usefulness::NotUseful,
                        Usefulness::Useful(rest_witnesses) => {
                            let mut all_witnesses = Vec::new();
                            for (ctor, arity) in &missing {
                                for w in &rest_witnesses {
                                    let mut row = vec![Pat::Ctor((*ctor).clone(), vec![Pat::Wild; *arity])];
                                    row.extend_from_slice(w);
                                    all_witnesses.push(row);
                                }
                            }
                            Usefulness::Useful(all_witnesses)
                        }
                    }
                }
            }
            CtorSet::Infinite => {
                let def = default_matrix(matrix);
                match is_useful(&def, q_rest, ty_rest, adts) {
                    Usefulness::NotUseful => Usefulness::NotUseful,
                    Usefulness::Useful(rest_witnesses) => Usefulness::Useful(
                        rest_witnesses
                            .into_iter()
                            .map(|w| {
                                let mut row = vec![Pat::Wild];
                                row.extend_from_slice(&w);
                                row
                            })
                            .collect(),
                    ),
                }
            }
        },
    }
}
```

- [ ] **Step 4: Add `pub mod exhaustive;` to `crates/ember-types/src/lib.rs`.**

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged (e.g. `CtorId` needs `Clone`/`PartialEq`/`Eq`/`Hash`, already derived in Task 2 — if `FxHashSet<CtorId>` doesn't compile, double check those derives are present).

- [ ] **Step 6: Commit**

```bash
git add crates/ember-types
git commit -m "Implement is_useful: Maranget's witness-carrying usefulness algorithm"
```

---

## Task 8: `exhaustive.rs` — witness pattern rendering

**Files:**
- Modify: `crates/ember-types/src/exhaustive.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn wild_renders_as_underscore() {
    let adts = AdtRegistry::new();
    let interner = ember_ast::Interner::new();
    assert_eq!(fmt_pat(&Pat::Wild, &adts, &interner), "_");
}

#[test]
fn nullary_variant_renders_bare() {
    let mut interner = ember_ast::Interner::new();
    let shape = interner.intern("Shape");
    let point = interner.intern("Point");
    let mut adts = AdtRegistry::new();
    let id = adts.register_enum(shape, vec![(point, vec![])]);
    assert_eq!(fmt_pat(&Pat::Ctor(CtorId::Variant(id, point), vec![]), &adts, &interner), "Point");
}

#[test]
fn variant_with_payload_renders_wildcarded_args() {
    let mut interner = ember_ast::Interner::new();
    let shape = interner.intern("Shape");
    let rect = interner.intern("Rect");
    let mut adts = AdtRegistry::new();
    let id = adts.register_enum(shape, vec![(rect, vec![Ty::Float, Ty::Float])]);
    let pat = Pat::Ctor(CtorId::Variant(id, rect), vec![Pat::Wild, Pat::Wild]);
    assert_eq!(fmt_pat(&pat, &adts, &interner), "Rect(_, _)");
}

#[test]
fn list_renders_bracketed() {
    let adts = AdtRegistry::new();
    let interner = ember_ast::Interner::new();
    let nil = Pat::Ctor(CtorId::Nil, vec![]);
    assert_eq!(fmt_pat(&nil, &adts, &interner), "[]");
    let one = Pat::Ctor(CtorId::Cons, vec![Pat::Wild, Pat::Ctor(CtorId::Nil, vec![])]);
    assert_eq!(fmt_pat(&one, &adts, &interner), "[_]");
    let open = Pat::Ctor(CtorId::Cons, vec![Pat::Wild, Pat::Wild]);
    assert_eq!(fmt_pat(&open, &adts, &interner), "[_, ..]");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types wild_renders nullary_variant_renders variant_with_payload_renders list_renders`
Expected: FAIL to compile — `fmt_pat` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::Interner;

pub fn fmt_pat(pat: &Pat, adts: &AdtRegistry, interner: &Interner) -> String {
    match pat {
        Pat::Wild => "_".to_string(),
        Pat::Ctor(ctor, args) => match ctor {
            CtorId::Variant(_, name) => {
                let name_str = interner.resolve(*name);
                if args.is_empty() {
                    name_str.to_string()
                } else {
                    let args_str: Vec<String> = args.iter().map(|a| fmt_pat(a, adts, interner)).collect();
                    format!("{name_str}({})", args_str.join(", "))
                }
            }
            CtorId::Struct(id) => {
                let name_str = interner.resolve(adts.name_of(*id));
                let fields: Vec<_> = adts.struct_fields(*id).collect();
                let parts: Vec<String> = fields
                    .iter()
                    .zip(args.iter())
                    .map(|(f, a)| format!("{}: {}", interner.resolve(*f), fmt_pat(a, adts, interner)))
                    .collect();
                format!("{name_str} {{ {} }}", parts.join(", "))
            }
            CtorId::Bool(b) => b.to_string(),
            CtorId::Int(n) => n.to_string(),
            CtorId::Float(bits) => f64::from_bits(*bits).to_string(),
            CtorId::Str(s) => format!("{:?}", interner.resolve(*s)),
            CtorId::Nil => "[]".to_string(),
            CtorId::Cons => fmt_list_chain(pat, adts, interner),
            CtorId::Tuple(_) => {
                let args_str: Vec<String> = args.iter().map(|a| fmt_pat(a, adts, interner)).collect();
                format!("({})", args_str.join(", "))
            }
        },
    }
}

/// Renders a `Cons`-chain witness as `[a, b, ..]`-style list syntax,
/// walking the chain until it hits `Nil` (closed list) or anything else
/// (`Wild`, meaning an open-ended tail — rendered as `..`).
fn fmt_list_chain(pat: &Pat, adts: &AdtRegistry, interner: &Interner) -> String {
    let mut items = Vec::new();
    let mut cur = pat;
    loop {
        match cur {
            Pat::Ctor(CtorId::Cons, args) => {
                items.push(fmt_pat(&args[0], adts, interner));
                cur = &args[1];
            }
            Pat::Ctor(CtorId::Nil, _) => {
                return format!("[{}]", items.join(", "));
            }
            _ => {
                items.push("..".to_string());
                break;
            }
        }
    }
    format!("[{}]", items.join(", "))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Render exhaustiveness witnesses as ember-like pattern syntax"
```

---

## Task 9: `exhaustive.rs` — `check_exhaustive`: per-arm reachability, guards, final diagnostic

**Files:**
- Modify: `crates/ember-types/src/exhaustive.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn missing_one_adt_variant_is_named_in_the_error() {
    let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    Rect(w, h) => w,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let shape = interner.intern("Shape");
    let circle = interner.intern("Circle");
    let rect = interner.intern("Rect");
    let point = interner.intern("Point");
    let mut adts = AdtRegistry::new();
    let id = adts.register_enum(
        shape,
        vec![(circle, vec![Ty::Float]), (rect, vec![Ty::Float, Ty::Float]), (point, vec![])],
    );
    let arms = find_match_arms(&ast, &stmts);
    let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Adt(id, vec![]));
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("non-exhaustive"));
    let note = &diags[0].notes[0];
    assert!(note.contains("Point"), "note was: {note}");
}

#[test]
fn wildcard_arm_makes_the_match_exhaustive() {
    let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    _ => 0.0,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let shape = interner.intern("Shape");
    let circle = interner.intern("Circle");
    let point = interner.intern("Point");
    let mut adts = AdtRegistry::new();
    let id = adts.register_enum(shape, vec![(circle, vec![Ty::Float]), (point, vec![])]);
    let arms = find_match_arms(&ast, &stmts);
    let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Adt(id, vec![]));
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn arm_after_wildcard_is_reported_unreachable() {
    let src = "fn f(n) {\n  match n {\n    _ => 1,\n    0 => 2,\n  }\n}\nprint(1);";
    let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = find_match_arms(&ast, &stmts);
    let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Int);
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("unreachable"));
}

#[test]
fn guarded_arm_never_counts_toward_exhaustiveness() {
    let src = "fn f(b) {\n  match b {\n    true if false => 1,\n  }\n}\nprint(1);";
    let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let adts = AdtRegistry::new();
    let arms = find_match_arms(&ast, &stmts);
    let diags = check_exhaustive(&ast, &interner, &adts, arms, &Ty::Bool);
    // Even though `true` is literally present, the guard means it can't be
    // assumed to cover `true` — both `true` and `false` should be reported
    // missing (a guarded arm contributes nothing to exhaustiveness).
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("non-exhaustive"));
}

fn find_match_arms<'a>(ast: &'a ember_ast::Ast, stmts: &[ember_ast::Idx<ember_ast::Stmt>]) -> &'a [ember_ast::MatchArm] {
    for &s in stmts {
        if let ember_ast::Stmt::Fn { body, .. } = ast.stmt(s) {
            if let ember_ast::Expr::Block { tail: Some(t), .. } = ast.expr(*body) {
                if let ember_ast::Expr::Match { arms, .. } = ast.expr(*t) {
                    return arms;
                }
            }
        }
        if let ember_ast::Stmt::ExprStmt(e) = ast.stmt(s) {
            if let ember_ast::Expr::Match { arms, .. } = ast.expr(*e) {
                return arms;
            }
        }
    }
    panic!("no match expression found")
}
```

Note the `find_match_arms` test helper handles TWO shapes: a bare top-level `match` expression statement, and a `match` as the sole tail expression of a `fn`'s block body (needed since `fn area(s) { match s { ... } }` parses the `match` as the fn body block's tail, not a top-level statement directly) — check this actually matches how the parser shapes a `fn` whose entire body is one `match` expression; adjust the helper if the real shape differs (e.g. if `match` needs a trailing statement before it can be a tail, or if it's wrapped differently).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types missing_one_adt wildcard_arm_makes arm_after_wildcard guarded_arm_never`
Expected: FAIL to compile — `check_exhaustive` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::{Ast, MatchArm};
use ember_diag::Diagnostic;

pub fn check_exhaustive(
    ast: &Ast,
    interner: &Interner,
    adts: &AdtRegistry,
    arms: &[MatchArm],
    scrutinee_ty: &Ty,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut matrix: Vec<Vec<Pat>> = Vec::new();
    let col_types = [scrutinee_ty.clone()];

    for arm in arms {
        let rows = crate::pat::lower_pattern(ast, adts, arm.pat);
        let mut any_reachable = false;
        for row in &rows {
            let q = [row.clone()];
            let reachable = !matches!(is_useful(&matrix, &q, &col_types, adts), Usefulness::NotUseful);
            if reachable {
                any_reachable = true;
            }
            // Tentatively add — lets a LATER alternative of the SAME
            // or-pattern arm see this one (`A | B`: B sees A). Popped
            // back out below if this arm turns out to have a guard.
            matrix.push(vec![row.clone()]);
        }
        if !any_reachable {
            diags.push(
                Diagnostic::warning("unreachable pattern").with_primary(arm.span, "this pattern can never match"),
            );
        }
        if arm.guard.is_some() {
            for _ in &rows {
                matrix.pop();
            }
        }
    }

    if let Usefulness::Useful(witnesses) = is_useful(&matrix, &[Pat::Wild], &col_types, adts) {
        let rendered: Vec<String> = witnesses.iter().map(|w| fmt_pat(&w[0], adts, interner)).collect();
        diags.push(
            Diagnostic::error("non-exhaustive patterns")
                .with_note(format!("missing: {}", rendered.join(", ")))
                .with_help("add a `_ => ...` arm to cover the remaining cases"),
        );
    }

    diags
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo clippy -p ember-types --all-targets -- -D warnings` and `cargo fmt -p ember-types -- --check`, fix anything flagged. If `Diagnostic`'s `.notes` field access in the test doesn't compile, check `crates/ember-diag/src/lib.rs` for the exact field name/shape (`with_note` should push to a `notes: Vec<String>` field, matching the pattern already established in Phases 4/5) and adjust the test's assertion accordingly.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Implement check_exhaustive: per-arm reachability, guards, and the final diagnostic"
```

---

## Task 10: `exhaustive.rs` — program-level driver

**Files:**
- Modify: `crates/ember-types/src/exhaustive.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn check_exhaustiveness_finds_a_non_exhaustive_match_anywhere_in_the_program() {
    let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, infer_diags) = crate::infer::infer(&ast, &mut interner, &stmts);
    assert!(infer_diags.is_empty(), "{infer_diags:?}");
    let diags = check_exhaustiveness(&ast, &interner, &info, &stmts);
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("non-exhaustive"));
}

#[test]
fn an_unconstrained_scrutinee_type_is_skipped_without_panicking() {
    // A match whose scrutinee type never gets pinned to anything concrete
    // shouldn't crash the checker — just nothing meaningful to report.
    let src = "fn f(x) {\n  match x {\n    _ => 1,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, infer_diags) = crate::infer::infer(&ast, &mut interner, &stmts);
    assert!(infer_diags.is_empty(), "{infer_diags:?}");
    let diags = check_exhaustiveness(&ast, &interner, &info, &stmts);
    assert!(diags.is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types check_exhaustiveness_finds an_unconstrained_scrutinee`
Expected: FAIL to compile — `check_exhaustiveness` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::infer::TypeInfo;
use ember_ast::{Expr, Idx, Stmt};

/// Walks every statement/expression reachable from the top level, calling
/// `check_exhaustive` on each `Expr::Match` found, using its
/// already-inferred (and now fully solved) scrutinee type from
/// `TypeInfo.expr_types`. A scrutinee whose type never got pinned down to
/// anything concrete (still an unresolved `Ty::Var`) is skipped — nothing
/// meaningful to check without a known constructor set.
pub fn check_exhaustiveness(ast: &Ast, interner: &Interner, info: &TypeInfo, stmts: &[Idx<Stmt>]) -> Vec<Diagnostic> {
    let mut match_exprs = Vec::new();
    walk_stmts(ast, stmts, &mut match_exprs);

    let mut subst = info.subst.clone();
    let mut diags = Vec::new();
    for idx in match_exprs {
        let Expr::Match { scrutinee, arms } = ast.expr(idx).clone() else {
            unreachable!("walk_stmts only ever collects Expr::Match indices")
        };
        let Some(scrutinee_ty) = info.expr_types.get(&scrutinee) else {
            continue;
        };
        let resolved = subst.resolve(scrutinee_ty);
        if matches!(resolved, Ty::Var(_)) {
            continue;
        }
        diags.extend(check_exhaustive(ast, interner, &info.adts, &arms, &resolved));
    }
    diags
}

fn walk_stmts(ast: &Ast, stmts: &[Idx<Stmt>], out: &mut Vec<Idx<Expr>>) {
    for &s in stmts {
        walk_stmt(ast, s, out);
    }
}

fn walk_stmt(ast: &Ast, idx: Idx<Stmt>, out: &mut Vec<Idx<Expr>>) {
    match ast.stmt(idx).clone() {
        Stmt::Let { init, .. } => walk_expr(ast, init, out),
        Stmt::ExprStmt(e) => walk_expr(ast, e, out),
        Stmt::Fn { body, .. } => walk_expr(ast, body, out),
        Stmt::While { cond, body } => {
            walk_expr(ast, cond, out);
            walk_expr(ast, body, out);
        }
        Stmt::For { iter, body, .. } => {
            walk_expr(ast, iter, out);
            walk_expr(ast, body, out);
        }
        Stmt::Loop { body } => walk_expr(ast, body, out),
        Stmt::Return(Some(e)) => walk_expr(ast, e, out),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. } | Stmt::Error => {}
    }
}

fn walk_expr(ast: &Ast, idx: Idx<Expr>, out: &mut Vec<Idx<Expr>>) {
    match ast.expr(idx).clone() {
        Expr::Match { scrutinee, arms } => {
            out.push(idx);
            walk_expr(ast, scrutinee, out);
            for arm in &arms {
                if let Some(g) = arm.guard {
                    walk_expr(ast, g, out);
                }
                walk_expr(ast, arm.body, out);
            }
        }
        Expr::Unary { operand, .. } => walk_expr(ast, operand, out),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(ast, lhs, out);
            walk_expr(ast, rhs, out);
        }
        Expr::Assign { target, value } => {
            walk_expr(ast, target, out);
            walk_expr(ast, value, out);
        }
        Expr::Call { callee, args } => {
            walk_expr(ast, callee, out);
            for a in args {
                walk_expr(ast, a, out);
            }
        }
        Expr::Index { base, index } => {
            walk_expr(ast, base, out);
            walk_expr(ast, index, out);
        }
        Expr::Field { base, .. } => walk_expr(ast, base, out),
        Expr::Lambda { body, .. } => walk_expr(ast, body, out),
        Expr::If { cond, then_, else_ } => {
            walk_expr(ast, cond, out);
            walk_expr(ast, then_, out);
            if let Some(e) = else_ {
                walk_expr(ast, e, out);
            }
        }
        Expr::Block { stmts, tail } => {
            walk_stmts(ast, &stmts, out);
            if let Some(t) = tail {
                walk_expr(ast, t, out);
            }
        }
        Expr::List { items } => {
            for i in items {
                walk_expr(ast, i, out);
            }
        }
        Expr::Struct { fields, .. } => {
            for (_, v) in fields {
                walk_expr(ast, v, out);
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil | Expr::Var(_) | Expr::Error => {}
    }
}
```

This requires `Subst` to derive `Clone` — check `crates/ember-types/src/subst.rs`; if `#[derive(Default)]` is the only derive on `Subst`, add `Clone` to it (`#[derive(Default, Clone)]`) so `info.subst.clone()` compiles. This is a safe, backward-compatible addition (doesn't change any existing behavior, just adds a capability).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` — all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Add the program-level exhaustiveness-checking driver"
```

---

## Task 11: Crate exports

**Files:**
- Modify: `crates/ember-types/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/ember-types/src/exhaustive.rs`'s test module:

```rust
#[test]
fn check_exhaustiveness_is_reachable_from_the_crate_root() {
    let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    assert!(infer_diags.is_empty());
    let diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    assert_eq!(diags.len(), 1);
}
```

Wait — this test lives INSIDE the `ember-types` crate itself (in `exhaustive.rs`'s own test module), so it can't refer to `ember_types::` as an external crate. Write it instead as an integration-style test using the crate's own already-in-scope items, OR — simpler and consistent with how Phase 5's entry-point test worked — just add this as a new test in `crates/ember-types/tests/` (a new integration test file, which DOES see the crate as external `ember_types::`) if one doesn't already exist. Check whether `crates/ember-types/tests/` exists; if not, create `crates/ember-types/tests/public_api.rs`:

```rust
#[test]
fn check_exhaustiveness_is_reachable_from_the_crate_root() {
    let src = "type Shape = | Circle(Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n  }\n}\nprint(1);";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty());
    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    assert!(infer_diags.is_empty());
    let diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    assert_eq!(diags.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-types --test public_api`
Expected: FAIL to compile — `ember_types::check_exhaustiveness` isn't re-exported yet.

- [ ] **Step 3: Implement**

Add to `crates/ember-types/src/lib.rs`'s existing `pub use` block:

```rust
pub use ctor_set::{ctor_arg_types, ctor_set, CtorSet};
pub use exhaustive::{check_exhaustive, check_exhaustiveness, fmt_pat, is_useful, Usefulness};
pub use matrix::{default_matrix, specialize, PatMatrix};
pub use pat::{lower_pattern, CtorId, Pat};
```

(Alongside the existing re-exports from Phase 5 — don't remove those.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-types`
Expected: PASS, including the new integration test. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` — all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-types
git commit -m "Re-export exhaustiveness-checking API from the crate root"
```

---

## Task 12: `ember-cli typecheck` — run exhaustiveness checking too

**Files:**
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Read the current `run_typecheck` function** in `crates/ember-cli/src/main.rs` to see its exact current shape (it parses, resolves, bails on resolver errors, calls `ember_types::infer`, prints types/schemes, then calls `print_diagnostics(&diags, path, &src)` as its final line).

- [ ] **Step 2: Implement**

Change `run_typecheck` so that after printing types/schemes, it also runs exhaustiveness checking and merges its diagnostics in before the final `print_diagnostics` call:

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

    let (mut info, mut diags) = ember_types::infer(&ast, &mut interner, &stmts);

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

    diags.extend(ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts));

    print_diagnostics(&diags, path, &src)
}
```

The only changes from the current version: `let (mut info, diags)` becomes `let (mut info, mut diags)` (needs to be mutable to `.extend()`), and one new line before the final `print_diagnostics` call.

- [ ] **Step 3: Build and manually verify**

Run: `source "$HOME/.cargo/env" && cargo build -p ember-cli` — expect clean build.

Write a small scratch file exercising a non-exhaustive match, e.g.:
```
type Shape = | Circle(Float) | Rect(Float, Float) | Point;
fn area(s) {
  match s {
    Circle(r) => r,
    Rect(w, h) => w,
  }
}
print(1);
```
Run `cargo run -p ember-cli -- typecheck <that file>` and confirm it reports a "non-exhaustive patterns" error naming `Point` as missing. Then run it again against `examples/hello.em` (no `match` at all) and confirm it still reports zero diagnostics, unaffected.

- [ ] **Step 4: Run the full verification suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-cli
git commit -m "Run exhaustiveness checking as part of ember typecheck"
```

---

## Task 13: Final wrap-up — full verification and CHECKLIST.md update

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full verification suite**

Run: `cargo test --workspace`
Expected: PASS across all 16 crates.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Update `CHECKLIST.md`'s Phase 6 section**

Open `CHECKLIST.md` and go through Phase 6's 14 items line by line, checking `- [x]` for everything this plan actually implemented, following the same honesty standard as every prior phase's wrap-up. Verify each line against the real code rather than block-checking. Specifically account for:
- `PatMatrix` — implemented as a type alias `Vec<Vec<Pat>>` over the algorithm's own `Pat`, not `ember_ast::Pattern` directly.
- Witness generation — implemented; note the "complete set" branch reconstructs witnesses per-constructor via recursion, and the "incomplete set" branch reuses one `default_matrix`-derived result across every missing constructor (a deliberate simplification justified by the two being structurally equivalent for a constructor with zero matrix rows — verified correct against the multi-missing-variant test, not a shortfall).
- Guards — verified via `guarded_arm_never_counts_toward_exhaustiveness`.
- Or-patterns — expand into multiple rows at lowering time, verified via `or_pattern_expands_into_multiple_rows` and via internal reachability between alternatives of the same arm.
- Nested patterns — handled by the matrix algorithm's own recursion (no special-casing needed); note this isn't separately unit-tested beyond what the ADT/list/struct pattern tests already exercise structurally.
- `Pattern::Tuple` — inert by design (pre-existing Phase 5 gap), not a new gap introduced here.
- The bare-nullary-variant-vs-bind-pattern ambiguity from Phase 5 — unaffected by this phase; a bare `Point` still lowers as `Wild` (via `Pattern::Bind`) since it's still parsed as `Pattern::Bind`, meaning it silently satisfies exhaustiveness rather than being checked as a genuine `Point`-variant match. Worth flagging clearly: this means a match using bare nullary variants can currently pass exhaustiveness even when other variants aren't handled — an honest limitation, not silently swept under.

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Mark Phase 6 checklist items complete"
```

- [ ] **Step 4: Final confirmation**

Run: `git log --oneline` and confirm a clean, incremental commit history from the `AdtRegistry` field-ordering fix through this final checklist update.

---

## Summary of what this plan does NOT cover (by design)

- Both execution backends, GC, formatter, LSP, WASM bindings, playground — Phases 7-17, each gets its own design/plan cycle.
- Fixing `Pattern::Tuple`'s underlying inertness or the bare-nullary-variant-vs-bind ambiguity — both pre-existing grammar/AST gaps from earlier phases, explicitly out of scope here (see the design doc's non-goals).
- Full precision witness generation for deeply nested missing cases beyond what the algorithm's own recursion naturally produces (e.g. this doesn't attempt to bound witness output size or deduplicate structurally-equivalent witnesses beyond what the algorithm already avoids by construction).
