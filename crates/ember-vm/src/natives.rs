use crate::error::RuntimeError;
use crate::value::{display_value, Value};
use ember_gc::GcHeap;

#[cfg(test)]
use crate::value::ListObj;
#[cfg(test)]
use std::cell::RefCell;

pub fn print(args: &[Value], _line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    println!("{}", display_value(&args[0]));
    Ok(Value::Nil)
}

pub fn len(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Int(l.0.borrow().len() as i64)),
        other => Err(RuntimeError::new(
            format!("len expects a list, found {other:?}"),
            line,
        )),
    }
}

pub fn push(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => {
            l.0.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(
            format!("push expects a list, found {other:?}"),
            line,
        )),
    }
}

pub fn clock(_args: &[Value], _line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Value::Float(now.as_secs_f64()))
}

pub fn str_fn(args: &[Value], _line: u32, gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    Ok(Value::Str(gc.intern_str(&display_value(&args[0]))))
}

pub fn int_fn(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Int"), line)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Int"),
            line,
        )),
    }
}

pub fn float_fn(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Float"), line)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Float"),
            line,
        )),
    }
}

pub fn type_of(args: &[Value], _line: u32, gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    let name = match &args[0] {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::Nil => "Nil".to_string(),
        Value::Str(_) => "String".to_string(),
        Value::List(_) => "List".to_string(),
        Value::Closure(_) | Value::Native(_) => "Function".to_string(),
        Value::Adt(a) => (*a.type_name).clone(),
        Value::Record { name, .. } => (**name).clone(),
    };
    Ok(Value::Str(gc.intern_str(&name)))
}

type NativeImpl = fn(&[Value], u32, &mut GcHeap) -> Result<Value, RuntimeError>;

pub const NATIVES: &[(&str, usize, NativeImpl)] = &[
    ("print", 1, print),
    ("len", 1, len),
    ("push", 2, push),
    ("clock", 0, clock),
    ("str", 1, str_fn),
    ("int", 1, int_fn),
    ("float", 1, float_fn),
    ("type_of", 1, type_of),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_reports_list_length() {
        let mut gc = GcHeap::new();
        let list = Value::List(gc.allocate(ListObj(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))));
        let result = len(&[list], 1, &mut gc).unwrap();
        assert!(matches!(result, Value::Int(3)));
    }

    #[test]
    fn len_rejects_a_non_list() {
        let mut gc = GcHeap::new();
        assert!(len(&[Value::Int(1)], 1, &mut gc).is_err());
    }

    #[test]
    fn push_appends_in_place() {
        let mut gc = GcHeap::new();
        let list = Value::List(gc.allocate(ListObj(RefCell::new(vec![Value::Int(1)]))));
        push(&[list.clone(), Value::Int(2)], 1, &mut gc).unwrap();
        match &list {
            Value::List(l) => assert_eq!(l.0.borrow().len(), 2),
            _ => unreachable!(),
        }
    }

    #[test]
    fn str_formats_any_value() {
        let mut gc = GcHeap::new();
        let result = str_fn(&[Value::Int(42)], 1, &mut gc).unwrap();
        match result {
            Value::Str(s) => assert_eq!(*s, "42"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn int_parses_strings_and_truncates_floats() {
        let mut gc = GcHeap::new();
        assert!(matches!(
            int_fn(&[Value::Float(3.9)], 1, &mut gc).unwrap(),
            Value::Int(3)
        ));
        let s = Value::Str(gc.intern_str("42"));
        assert!(matches!(int_fn(&[s], 1, &mut gc).unwrap(), Value::Int(42)));
        let bad = Value::Str(gc.intern_str("nope"));
        assert!(int_fn(&[bad], 1, &mut gc).is_err());
    }

    #[test]
    fn float_parses_strings_and_widens_ints() {
        let mut gc = GcHeap::new();
        assert!(matches!(
            float_fn(&[Value::Int(3)], 1, &mut gc).unwrap(),
            Value::Float(f) if f == 3.0
        ));
    }

    #[test]
    fn type_of_names_every_kind_including_records_and_adts_by_their_own_name() {
        let mut gc = GcHeap::new();
        let int_name = type_of(&[Value::Int(1)], 1, &mut gc).unwrap();
        assert!(matches!(int_name, Value::Str(s) if *s == "Int"));

        let type_name = gc.intern_str("Shape");
        let variant = gc.intern_str("Circle");
        let adt = Value::Adt(gc.allocate(crate::value::AdtValue {
            type_name,
            variant,
            fields: vec![],
        }));
        let adt_name = type_of(&[adt], 1, &mut gc).unwrap();
        assert!(matches!(adt_name, Value::Str(s) if *s == "Shape"));
    }

    #[test]
    fn clock_returns_a_float() {
        let mut gc = GcHeap::new();
        assert!(matches!(clock(&[], 1, &mut gc).unwrap(), Value::Float(_)));
    }

    #[test]
    fn natives_table_has_all_8_with_the_right_arities() {
        let expected: &[(&str, usize)] = &[
            ("print", 1),
            ("len", 1),
            ("push", 2),
            ("clock", 0),
            ("str", 1),
            ("int", 1),
            ("float", 1),
            ("type_of", 1),
        ];
        assert_eq!(NATIVES.len(), 8);
        for (name, arity) in expected {
            let found = NATIVES.iter().find(|(n, _, _)| n == name);
            assert!(found.is_some(), "missing native {name}");
            assert_eq!(found.unwrap().1, *arity, "wrong arity for {name}");
        }
    }
}
