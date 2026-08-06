use crate::env::Env;
use crate::error::RuntimeError;
use ember_ast::{Expr, Idx, Symbol};
use ember_span::Span;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<Closure>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    /// Beyond the literal `SPEC.md` sketch: named, so `type_of()` on a
    /// struct instance can report something more useful than "Record".
    Record {
        name: Symbol,
        fields: Rc<RefCell<FxHashMap<Symbol, Value>>>,
    },
    /// A payload-ful ADT variant constructor referenced but not yet
    /// called (e.g. evaluating the bare name `Circle`). A nullary variant
    /// skips this entirely and is bound directly to a `Value::Adt`.
    AdtCtor {
        type_name: Symbol,
        variant: Symbol,
        arity: usize,
    },
}

pub struct AdtValue {
    pub type_name: Symbol,
    pub variant: Symbol,
    pub fields: Vec<Value>,
}

pub struct Closure {
    pub params: Vec<Symbol>,
    pub body: Idx<Expr>,
    pub env: Rc<RefCell<Env>>,
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value], Span, &ember_ast::Interner) -> Result<Value, RuntimeError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_clone_cheaply_via_rc() {
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1)])));
        let cloned = list.clone();
        if let (Value::List(a), Value::List(b)) = (&list, &cloned) {
            assert!(
                Rc::ptr_eq(a, b),
                "clone should share the same backing Rc, not deep-copy"
            );
        } else {
            panic!("expected List");
        }
    }
}
