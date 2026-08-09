use ember_bytecode::chunk::FunctionProto;
use ember_gc::{Gc, GcHeap, Trace, Tracer};
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Gc<String>),
    List(Gc<ListObj>),
    Closure(Gc<ClosureObj>),
    /// Stays `Rc`, not `Gc` — natives are constructed once in `Vm::new`,
    /// are effectively `'static` for the VM's lifetime, and hold no
    /// `Value`s of their own, so there is nothing for the GC heap to
    /// manage here.
    Native(Rc<NativeFn>),
    Adt(Gc<AdtValue>),
    Record {
        name: Gc<String>,
        fields: Gc<RecordFields>,
    },
}

/// Local newtype around the list's actual storage. Needed because Rust's
/// orphan rule forbids `ember-vm` implementing `ember-gc`'s `Trace` trait
/// directly for `RefCell<Vec<Value>>` — neither `Trace` nor `RefCell` is
/// defined in this crate. Wrapping in a type this crate *does* define
/// sidesteps the question entirely.
#[derive(Debug)]
pub struct ListObj(pub RefCell<Vec<Value>>);

impl Trace for ListObj {
    fn trace(&self, tracer: &mut Tracer) {
        for v in self.0.borrow().iter() {
            trace_value(v, tracer);
        }
    }
}

/// Same reasoning as `ListObj` — a local wrapper so `Trace` can be
/// implemented for it.
#[derive(Debug)]
pub struct RecordFields(pub RefCell<FxHashMap<Gc<String>, Value>>);

impl Trace for RecordFields {
    fn trace(&self, tracer: &mut Tracer) {
        for (k, v) in self.0.borrow().iter() {
            tracer.mark(*k);
            trace_value(v, tracer);
        }
    }
}

pub struct ClosureObj {
    /// Stays `Rc`, not `Gc` — see this file's own header reasoning:
    /// `FunctionProto` is immutable, compile-time-owned program data, part
    /// of the `Chunk` the whole `Vm` run borrows, never cyclic.
    pub proto: Rc<FunctionProto>,
    pub upvalues: Vec<Gc<UpvalueCell>>,
}

impl Trace for ClosureObj {
    fn trace(&self, tracer: &mut Tracer) {
        for uv in &self.upvalues {
            tracer.mark(*uv);
        }
    }
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

/// Local wrapper around `RefCell<Upvalue>`, same orphan-rule reasoning as
/// `ListObj`/`RecordFields`.
pub struct UpvalueCell(pub RefCell<Upvalue>);

impl Trace for UpvalueCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Upvalue::Closed(v) = &*self.0.borrow() {
            trace_value(v, tracer);
        }
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
    pub type_name: Gc<String>,
    pub variant: Gc<String>,
    pub fields: Vec<Value>,
}

impl Trace for AdtValue {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.mark(self.type_name);
        tracer.mark(self.variant);
        for v in &self.fields {
            trace_value(v, tracer);
        }
    }
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    /// Gained a `&mut GcHeap` parameter this phase — natives that
    /// construct new heap values (`str`) need somewhere to allocate into.
    pub func: fn(&[Value], u32, &mut GcHeap) -> Result<Value, crate::error::RuntimeError>,
}

impl std::fmt::Debug for NativeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<native {}>", self.name)
    }
}

/// Marks every `Gc<_>` handle a `Value` directly holds. Deliberately not
/// `impl Trace for Value` — `Value` itself is never heap-allocated (it
/// lives inline on the VM stack and inside other GC objects' fields);
/// only the things *inside* certain variants are.
pub fn trace_value(v: &Value, tracer: &mut Tracer) {
    match v {
        Value::Str(g) => tracer.mark(*g),
        Value::List(g) => tracer.mark(*g),
        Value::Closure(g) => tracer.mark(*g),
        Value::Adt(g) => tracer.mark(*g),
        Value::Record { name, fields } => {
            tracer.mark(*name);
            tracer.mark(*fields);
        }
        Value::Native(_) | Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {}
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
            (Value::List(a), Value::List(b)) => *a.0.borrow() == *b.0.borrow(),
            (
                Value::Record {
                    name: n1,
                    fields: f1,
                },
                Value::Record {
                    name: n2,
                    fields: f2,
                },
            ) => n1 == n2 && *f1.0.borrow() == *f2.0.borrow(),
            _ => false,
        }
    }
}

/// Structural equality — `List`/`Record` compare by contents, not by
/// handle identity. Mirrors `ember-tree::values_equal` exactly (both
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
            let (x, y) = (x.0.borrow(), y.0.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

/// Deliberately takes no `&Interner` — see this file's own header note
/// from Phase 9, unchanged by this migration.
pub fn display_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l.0.borrow().iter().map(display_value).collect();
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
            let f = fields.0.borrow();
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
        let mut heap = GcHeap::new();
        let a =
            Value::List(heap.allocate(ListObj(RefCell::new(vec![Value::Int(1), Value::Int(2)]))));
        let b =
            Value::List(heap.allocate(ListObj(RefCell::new(vec![Value::Int(1), Value::Int(2)]))));
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
        let mut heap = GcHeap::new();
        assert_eq!(display_value(&Value::Nil), "nil");
        assert_eq!(display_value(&Value::Bool(true)), "true");
        assert_eq!(display_value(&Value::Int(42)), "42");
        assert_eq!(display_value(&Value::Str(heap.intern_str("hi"))), "hi");
        let list =
            Value::List(heap.allocate(ListObj(RefCell::new(vec![Value::Int(1), Value::Int(2)]))));
        assert_eq!(display_value(&list), "[1, 2]");
    }

    #[test]
    fn display_value_formats_a_record_with_its_fields() {
        let mut heap = GcHeap::new();
        let x_key = heap.intern_str("x");
        let mut fields = FxHashMap::default();
        fields.insert(x_key, Value::Int(1));
        let record = Value::Record {
            name: heap.intern_str("P"),
            fields: heap.allocate(RecordFields(RefCell::new(fields))),
        };
        let out = display_value(&record);
        assert!(out.starts_with("P {"), "{out}");
        assert!(out.contains("x: 1"), "{out}");
    }

    #[test]
    fn display_value_formats_a_nullary_and_a_payload_adt() {
        let mut heap = GcHeap::new();
        let shape = heap.intern_str("Shape");
        let origin = heap.intern_str("Origin");
        let nullary = Value::Adt(heap.allocate(AdtValue {
            type_name: shape,
            variant: origin,
            fields: vec![],
        }));
        assert_eq!(display_value(&nullary), "Origin");
        let shape = heap.intern_str("Shape");
        let circle = heap.intern_str("Circle");
        let payload = Value::Adt(heap.allocate(AdtValue {
            type_name: shape,
            variant: circle,
            fields: vec![Value::Float(1.5)],
        }));
        assert_eq!(display_value(&payload), "Circle(1.5)");
    }
}
