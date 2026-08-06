use crate::adt::AdtRegistry;
use crate::pat::CtorId;
use crate::ty::Ty;

/// A type's complete constructor set, or `Infinite` if it has none
/// (`Int`/`Float`/`String`, or anything else without a finite pattern
/// vocabulary — a wildcard is always required to close such a type).
#[derive(Debug, Clone, PartialEq)]
pub enum CtorSet {
    Finite(Vec<(CtorId, usize)>),
    Infinite,
}

pub fn ctor_set(ty: &Ty, adts: &AdtRegistry) -> CtorSet {
    match ty {
        Ty::Bool => CtorSet::Finite(vec![(CtorId::Bool(true), 0), (CtorId::Bool(false), 0)]),
        Ty::List(_) => CtorSet::Finite(vec![(CtorId::Nil, 0), (CtorId::Cons, 2)]),
        Ty::Adt(id, _) if adts.is_struct(*id) => {
            let arity = adts.struct_fields(*id).count();
            CtorSet::Finite(vec![(CtorId::Struct(*id), arity)])
        }
        Ty::Adt(id, _) => CtorSet::Finite(
            adts.enum_variants(*id)
                .map(|(name, arity)| (CtorId::Variant(*id, name), arity))
                .collect(),
        ),
        _ => CtorSet::Infinite,
    }
}

/// The types of a constructor's own sub-fields, needed to keep type
/// context correct when the algorithm recurses one level deeper (e.g. a
/// `Cons`'s two fields are the list's element type and the list type
/// itself). `Tuple`'s slots use `Ty::Unit` as a placeholder — there's no
/// real per-slot type to give them (no `Ty::Tuple` exists), but since
/// `Tuple` is always treated as a single complete constructor, its own
/// slot types are never load-bearing beyond not panicking.
pub fn ctor_arg_types(ctor: &CtorId, ty: &Ty, adts: &AdtRegistry) -> Vec<Ty> {
    match ctor {
        CtorId::Variant(_, name) => adts
            .variant(*name)
            .map(|(_, payload)| payload.to_vec())
            .unwrap_or_default(),
        CtorId::Struct(id) => adts
            .struct_fields(*id)
            .filter_map(|f| adts.field_ty(*id, f).cloned())
            .collect(),
        CtorId::Cons => {
            if let Ty::List(elem) = ty {
                vec![(**elem).clone(), ty.clone()]
            } else {
                vec![]
            }
        }
        CtorId::Nil | CtorId::Bool(_) | CtorId::Int(_) | CtorId::Float(_) | CtorId::Str(_) => {
            vec![]
        }
        CtorId::Tuple(n) => vec![Ty::Unit; *n],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::pat::CtorId;
    use crate::ty::Ty;
    use ember_ast::Interner;

    #[test]
    fn bool_has_two_constructors() {
        let adts = AdtRegistry::new();
        match ctor_set(&Ty::Bool, &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(ctors.len(), 2);
                assert!(ctors.contains(&(CtorId::Bool(true), 0)));
                assert!(ctors.contains(&(CtorId::Bool(false), 0)));
            }
            CtorSet::Infinite => panic!("Bool should be finite"),
        }
    }

    #[test]
    fn list_has_nil_and_cons() {
        let adts = AdtRegistry::new();
        match ctor_set(&Ty::List(Box::new(Ty::Int)), &adts) {
            CtorSet::Finite(ctors) => {
                assert!(ctors.contains(&(CtorId::Nil, 0)));
                assert!(ctors.contains(&(CtorId::Cons, 2)));
            }
            CtorSet::Infinite => panic!("List should be finite"),
        }
    }

    #[test]
    fn int_float_string_are_infinite() {
        let adts = AdtRegistry::new();
        assert!(matches!(ctor_set(&Ty::Int, &adts), CtorSet::Infinite));
        assert!(matches!(ctor_set(&Ty::Float, &adts), CtorSet::Infinite));
        assert!(matches!(ctor_set(&Ty::String, &adts), CtorSet::Infinite));
    }

    #[test]
    fn struct_has_one_constructor_with_field_arity() {
        let mut interner = Interner::new();
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let y = interner.intern("y");
        let mut adts = AdtRegistry::new();
        let id = adts.register_struct(point, vec![(x, Ty::Float), (y, Ty::Float)]);
        match ctor_set(&Ty::Adt(id, vec![]), &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(ctors, vec![(CtorId::Struct(id), 2)]);
            }
            CtorSet::Infinite => panic!("struct should be finite"),
        }
    }

    #[test]
    fn enum_has_one_constructor_per_variant() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let point = interner.intern("Point");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![(circle, vec![Ty::Float]), (point, vec![])]);
        match ctor_set(&Ty::Adt(id, vec![]), &adts) {
            CtorSet::Finite(ctors) => {
                assert_eq!(
                    ctors,
                    vec![
                        (CtorId::Variant(id, circle), 1),
                        (CtorId::Variant(id, point), 0)
                    ]
                );
            }
            CtorSet::Infinite => panic!("enum should be finite"),
        }
    }
}
