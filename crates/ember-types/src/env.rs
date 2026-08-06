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
        TyEnv {
            scopes: vec![FxHashMap::default()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(FxHashMap::default());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn declare(&mut self, name: Symbol, scheme: Scheme) {
        self.scopes
            .last_mut()
            .expect("no scope open")
            .insert(name, scheme);
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
        env.declare(
            x,
            Scheme {
                vars: vec![],
                ty: Ty::Int,
            },
        );
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Int);
    }

    #[test]
    fn nested_scope_shadowing_resolves_to_the_innermost() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut env = TyEnv::new();
        env.declare(
            x,
            Scheme {
                vars: vec![],
                ty: Ty::Int,
            },
        );
        env.push_scope();
        env.declare(
            x,
            Scheme {
                vars: vec![],
                ty: Ty::Bool,
            },
        );
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Bool);
        env.pop_scope();
        assert_eq!(env.lookup(x).unwrap().ty, Ty::Int);
    }

    #[test]
    fn free_vars_collects_every_var_across_the_whole_scope_stack() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut env = TyEnv::new();
        env.declare(
            x,
            Scheme {
                vars: vec![],
                ty: Ty::Var(crate::ty::TyVarId(7)),
            },
        );
        let free = env.free_vars();
        assert!(free.contains(&crate::ty::TyVarId(7)));
    }

    #[test]
    fn free_vars_excludes_a_schemes_own_quantified_vars() {
        let mut interner = Interner::new();
        let f = interner.intern("f");
        let mut env = TyEnv::new();
        let id = crate::ty::TyVarId(3);
        env.declare(
            f,
            Scheme {
                vars: vec![id],
                ty: Ty::Var(id),
            },
        );
        assert!(!env.free_vars().contains(&id));
    }
}
