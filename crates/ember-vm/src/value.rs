use ember_bytecode::chunk::FunctionProto;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<ClosureObj>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    Record {
        name: Rc<String>,
        fields: Rc<RefCell<FxHashMap<Rc<String>, Value>>>,
    },
}

pub struct ClosureObj {
    pub proto: Rc<FunctionProto>,
    pub upvalues: Vec<Rc<RefCell<Upvalue>>>,
}

impl std::fmt::Debug for ClosureObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<closure arity={} upvalues={}>",
            self.proto.arity,
            self.upvalues.len()
        )
    }
}

#[derive(Debug, Clone)]
pub enum Upvalue {
    /// Still live on the VM stack, at this index.
    Open(usize),
    /// Hoisted to the heap — the stack slot it used to occupy is gone.
    Closed(Value),
}

#[derive(Debug)]
pub struct AdtValue {
    pub type_name: Rc<String>,
    pub variant: Rc<String>,
    pub fields: Vec<Value>,
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value], u32) -> Result<Value, crate::error::RuntimeError>,
}

impl std::fmt::Debug for NativeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<native {}>", self.name)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => *a.borrow() == *b.borrow(),
            (
                Value::Record {
                    name: n1,
                    fields: f1,
                },
                Value::Record {
                    name: n2,
                    fields: f2,
                },
            ) => n1 == n2 && *f1.borrow() == *f2.borrow(),
            _ => false,
        }
    }
}

/// Structural equality — `List`/`Record` compare by contents, not by `Rc`
/// pointer identity. Mirrors `ember-tree::values_equal` exactly (both
/// backends must agree on what `==` means for the conformance suite to be
/// meaningful).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

/// Deliberately takes no `&Interner` — see this file's own header note.
pub fn display_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(display_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::Adt(a) => {
            if a.fields.is_empty() {
                a.variant.to_string()
            } else {
                let parts: Vec<String> = a.fields.iter().map(display_value).collect();
                format!("{}({})", a.variant, parts.join(", "))
            }
        }
        Value::Record { name, fields } => {
            let f = fields.borrow();
            let parts: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{k}: {}", display_value(v)))
                .collect();
            format!("{name} {{ {} }}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_equal_compares_structurally_not_by_identity() {
        let a = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        let b = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert!(
            values_equal(&a, &b),
            "two separately-built lists with equal contents must compare equal"
        );
    }

    #[test]
    fn values_equal_rejects_different_types() {
        assert!(!values_equal(&Value::Int(1), &Value::Bool(true)));
        assert!(!values_equal(&Value::Nil, &Value::Int(0)));
    }

    #[test]
    fn display_value_formats_every_variant() {
        assert_eq!(display_value(&Value::Nil), "nil");
        assert_eq!(display_value(&Value::Bool(true)), "true");
        assert_eq!(display_value(&Value::Int(42)), "42");
        assert_eq!(display_value(&Value::Str(Rc::new("hi".to_string()))), "hi");
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert_eq!(display_value(&list), "[1, 2]");
    }

    #[test]
    fn display_value_formats_a_record_with_its_fields() {
        let mut fields = FxHashMap::default();
        fields.insert(Rc::new("x".to_string()), Value::Int(1));
        let record = Value::Record {
            name: Rc::new("P".to_string()),
            fields: Rc::new(RefCell::new(fields)),
        };
        let out = display_value(&record);
        assert!(out.starts_with("P {"), "{out}");
        assert!(out.contains("x: 1"), "{out}");
    }

    #[test]
    fn display_value_formats_a_nullary_and_a_payload_adt() {
        let nullary = Value::Adt(Rc::new(AdtValue {
            type_name: Rc::new("Shape".to_string()),
            variant: Rc::new("Origin".to_string()),
            fields: vec![],
        }));
        assert_eq!(display_value(&nullary), "Origin");
        let payload = Value::Adt(Rc::new(AdtValue {
            type_name: Rc::new("Shape".to_string()),
            variant: Rc::new("Circle".to_string()),
            fields: vec![Value::Float(1.5)],
        }));
        assert_eq!(display_value(&payload), "Circle(1.5)");
    }
}
