use crate::ty::{Ty, TyVarId};

/// The union-find-style substitution store: `bindings[v.0]` is `Some(ty)`
/// once `v` has been bound to a concrete (or partially concrete) type, or
/// `None` while it's still an open hole.
#[derive(Default, Clone)]
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
        let Ty::Var(id) = a else {
            panic!("fresh() must return Ty::Var")
        };
        s.bind(id, Ty::Int);
        assert_eq!(s.resolve(&a), Ty::Int);
    }

    #[test]
    fn resolve_follows_a_chain_of_bound_vars() {
        let mut s = Subst::new();
        let a = s.fresh();
        let b = s.fresh();
        let (Ty::Var(a_id), Ty::Var(_)) = (&a, &b) else {
            unreachable!()
        };
        s.bind(*a_id, b.clone());
        let Ty::Var(b_id) = b else { unreachable!() };
        s.bind(b_id, Ty::Bool);
        assert_eq!(s.resolve(&a), Ty::Bool);
    }

    #[test]
    fn resolve_recurses_into_compound_types() {
        let mut s = Subst::new();
        let a = s.fresh();
        let Ty::Var(id) = a.clone() else {
            unreachable!()
        };
        s.bind(id, Ty::Int);
        let list = Ty::List(Box::new(a));
        assert_eq!(s.resolve(&list), Ty::List(Box::new(Ty::Int)));
    }

    #[test]
    fn occurs_check_detects_self_reference_through_a_fun_type() {
        let mut s = Subst::new();
        let a = s.fresh();
        let Ty::Var(id) = a.clone() else {
            unreachable!()
        };
        let self_referential = Ty::Fun(vec![a.clone()], Box::new(Ty::Bool));
        assert!(s.occurs_in(id, &self_referential));
        assert!(!s.occurs_in(id, &Ty::Int));
    }
}
