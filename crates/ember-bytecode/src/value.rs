use std::rc::Rc;

/// The constant pool's value type — deliberately minimal. Only literal,
/// immutable, poolable values ever belong in a constant pool; closures,
/// lists, and records are built at runtime via opcodes (`Closure`,
/// `MakeList`, `MakeRecord`, `MakeAdt`), never pooled. Phase 9/10 extend
/// this with a GC-backed `Obj` variant once `ember-gc` exists — they
/// don't replace it; every constant this phase ever pools stays
/// representable exactly as-is.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_compare_structurally() {
        assert_eq!(Value::Int(1), Value::Int(1));
        assert_ne!(Value::Int(1), Value::Int(2));
        assert_eq!(
            Value::Str(std::rc::Rc::new("x".to_string())),
            Value::Str(std::rc::Rc::new("x".to_string()))
        );
    }
}
