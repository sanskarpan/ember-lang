use crate::value::Value;
use ember_ast::Symbol;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Env {
    pub values: FxHashMap<Symbol, Value>,
    pub parent: Option<Rc<RefCell<Env>>>,
}

impl Env {
    pub fn new() -> Rc<RefCell<Env>> {
        Rc::new(RefCell::new(Env {
            values: FxHashMap::default(),
            parent: None,
        }))
    }

    pub fn child(parent: &Rc<RefCell<Env>>) -> Rc<RefCell<Env>> {
        Rc::new(RefCell::new(Env {
            values: FxHashMap::default(),
            parent: Some(Rc::clone(parent)),
        }))
    }

    pub fn declare(env: &Rc<RefCell<Env>>, name: Symbol, value: Value) {
        env.borrow_mut().values.insert(name, value);
    }

    /// Innermost-first lookup, walking the parent chain.
    pub fn get(env: &Rc<RefCell<Env>>, name: Symbol) -> Option<Value> {
        let e = env.borrow();
        if let Some(v) = e.values.get(&name) {
            return Some(v.clone());
        }
        let parent = e.parent.clone();
        drop(e);
        match parent {
            Some(p) => Env::get(&p, name),
            None => None,
        }
    }

    /// Mutates the NEAREST enclosing declaration of `name`, walking
    /// outward. Returns `false` if `name` was never declared anywhere in
    /// the chain (the resolver should have already caught this upstream
    /// in a well-formed pipeline — this is a defensive fallback, not the
    /// primary correctness mechanism).
    pub fn set(env: &Rc<RefCell<Env>>, name: Symbol, value: Value) -> bool {
        let mut e = env.borrow_mut();
        if let std::collections::hash_map::Entry::Occupied(mut entry) = e.values.entry(name) {
            entry.insert(value);
            return true;
        }
        let parent = e.parent.clone();
        drop(e);
        match parent {
            Some(p) => Env::set(&p, name, value),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use ember_ast::Interner;

    #[test]
    fn declare_and_get_within_one_env() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let env = Env::new();
        Env::declare(&env, x, Value::Int(42));
        assert!(matches!(Env::get(&env, x), Some(Value::Int(42))));
    }

    #[test]
    fn child_env_sees_parent_bindings() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let parent = Env::new();
        Env::declare(&parent, x, Value::Int(1));
        let child = Env::child(&parent);
        assert!(matches!(Env::get(&child, x), Some(Value::Int(1))));
    }

    #[test]
    fn set_mutates_through_the_parent_chain() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let parent = Env::new();
        Env::declare(&parent, x, Value::Int(1));
        let child = Env::child(&parent);
        assert!(Env::set(&child, x, Value::Int(2)));
        assert!(
            matches!(Env::get(&parent, x), Some(Value::Int(2))),
            "mutation through a child must be visible in the parent"
        );
    }

    #[test]
    fn set_on_an_undeclared_name_returns_false() {
        let mut interner = Interner::new();
        let y = interner.intern("y");
        let env = Env::new();
        assert!(!Env::set(&env, y, Value::Int(1)));
    }
}
