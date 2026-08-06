use crate::error::RuntimeError;
use crate::value::{NativeFn, Value};
use ember_ast::Interner;
use ember_span::Span;
use std::rc::Rc;

pub fn print(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    println!("{}", display_value(&args[0], interner));
    Ok(Value::Nil)
}

pub fn len(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Int(l.borrow().len() as i64)),
        other => Err(RuntimeError::new(
            format!("len expects a list, found {other:?}"),
            span,
        )),
    }
}

pub fn push(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => {
            l.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(
            format!("push expects a list, found {other:?}"),
            span,
        )),
    }
}

pub fn clock(_args: &[Value], _span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Value::Float(now.as_secs_f64()))
}

pub fn str_fn(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    Ok(Value::Str(Rc::new(display_value(&args[0], interner))))
}

pub fn int_fn(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Int"), span)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Int"),
            span,
        )),
    }
}

pub fn float_fn(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Float"), span)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Float"),
            span,
        )),
    }
}

pub fn type_of(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    let name = match &args[0] {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::Nil => "Nil".to_string(),
        Value::Str(_) => "String".to_string(),
        Value::List(_) => "List".to_string(),
        Value::Closure(_) | Value::Native(_) | Value::AdtCtor { .. } => "Function".to_string(),
        Value::Adt(a) => interner.resolve(a.type_name).to_string(),
        Value::Record { name, .. } => interner.resolve(*name).to_string(),
    };
    Ok(Value::Str(Rc::new(name)))
}

pub fn display_value(v: &Value, interner: &Interner) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l
                .borrow()
                .iter()
                .map(|v| display_value(v, interner))
                .collect();
            format!("[{}]", items.join(", "))
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::AdtCtor { variant, .. } => format!("<constructor {}>", interner.resolve(*variant)),
        Value::Adt(a) => {
            let name = interner.resolve(a.variant);
            if a.fields.is_empty() {
                name.to_string()
            } else {
                let parts: Vec<String> = a
                    .fields
                    .iter()
                    .map(|v| display_value(v, interner))
                    .collect();
                format!("{name}({})", parts.join(", "))
            }
        }
        Value::Record { name, fields } => {
            let name_str = interner.resolve(*name);
            let f = fields.borrow();
            let parts: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{}: {}", interner.resolve(*k), display_value(v, interner)))
                .collect();
            format!("{name_str} {{ {} }}", parts.join(", "))
        }
    }
}

type NativeImpl = fn(&[Value], Span, &Interner) -> Result<Value, RuntimeError>;

const NATIVES: &[(&str, usize, NativeImpl)] = &[
    ("print", 1, print),
    ("len", 1, len),
    ("push", 2, push),
    ("clock", 0, clock),
    ("str", 1, str_fn),
    ("int", 1, int_fn),
    ("float", 1, float_fn),
    ("type_of", 1, type_of),
];

/// Looks up a native by name, constructing a fresh `NativeFn` (cheap — a
/// few fields, no meaningful allocation cost worth caching for this
/// deliberately-simple reference backend).
pub fn lookup(name: &str) -> Option<NativeFn> {
    NATIVES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|&(name, arity, func)| NativeFn { name, arity, func })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;
    use ember_span::Span;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn span() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn len_and_push_operate_on_lists() {
        let interner = Interner::new();
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert!(matches!(
            len(std::slice::from_ref(&list), span(), &interner),
            Ok(Value::Int(2))
        ));
        push(&[list.clone(), Value::Int(3)], span(), &interner).unwrap();
        assert!(matches!(len(&[list], span(), &interner), Ok(Value::Int(3))));
    }

    #[test]
    fn int_and_float_convert_between_each_other_and_from_strings() {
        let interner = Interner::new();
        assert!(matches!(
            int_fn(&[Value::Float(3.9)], span(), &interner),
            Ok(Value::Int(3))
        ));
        assert!(
            matches!(float_fn(&[Value::Int(3)], span(), &interner), Ok(Value::Float(f)) if f == 3.0)
        );
        assert!(matches!(
            int_fn(&[Value::Str(Rc::new("42".to_string()))], span(), &interner),
            Ok(Value::Int(42))
        ));
        assert!(int_fn(&[Value::Str(Rc::new("abc".to_string()))], span(), &interner).is_err());
    }

    #[test]
    fn type_of_names_every_kind_of_value() {
        let interner = Interner::new();
        assert!(
            matches!(type_of(&[Value::Int(1)], span(), &interner), Ok(Value::Str(s)) if s.as_str() == "Int")
        );
        assert!(
            matches!(type_of(&[Value::Bool(true)], span(), &interner), Ok(Value::Str(s)) if s.as_str() == "Bool")
        );
    }

    #[test]
    fn lookup_finds_every_native_by_name_with_the_right_arity() {
        let names_and_arities = [
            ("print", 1),
            ("len", 1),
            ("push", 2),
            ("clock", 0),
            ("str", 1),
            ("int", 1),
            ("float", 1),
            ("type_of", 1),
        ];
        for (name, arity) in names_and_arities {
            let native = lookup(name).unwrap_or_else(|| panic!("expected a native named {name}"));
            assert_eq!(native.arity, arity, "wrong arity for {name}");
        }
        assert!(lookup("not_a_real_native").is_none());
    }
}
