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
        let s = Scheme {
            vars: vec![],
            ty: Ty::Int,
        };
        assert!(s.vars.is_empty());
    }
}
