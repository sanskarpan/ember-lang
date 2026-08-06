use crate::adt::AdtRegistry;
use crate::subst::Subst;
use crate::ty::{Scheme, Ty, TyVarId};
use ember_ast::Interner;
use rustc_hash::FxHashMap;

pub fn display_ty(ty: &Ty, subst: &mut Subst, adts: &AdtRegistry, interner: &Interner) -> String {
    let resolved = subst.resolve(ty);
    let mut names = FxHashMap::default();
    fmt_ty(&resolved, &mut names, adts, interner)
}

pub fn display_scheme(
    scheme: &Scheme,
    subst: &mut Subst,
    adts: &AdtRegistry,
    interner: &Interner,
) -> String {
    let resolved = subst.resolve(&scheme.ty);
    if scheme.vars.is_empty() {
        return fmt_ty(&resolved, &mut FxHashMap::default(), adts, interner);
    }
    let mut names = FxHashMap::default();
    let quantifier: Vec<String> = scheme
        .vars
        .iter()
        .map(|v| var_name(*v, &mut names))
        .collect();
    format!(
        "forall {}. {}",
        quantifier.join(" "),
        fmt_ty(&resolved, &mut names, adts, interner)
    )
}

fn var_name(v: TyVarId, names: &mut FxHashMap<TyVarId, String>) -> String {
    let next = names.len();
    names
        .entry(v)
        .or_insert_with(|| {
            let letter = (b'a' + (next % 26) as u8) as char;
            let suffix = next / 26;
            if suffix == 0 {
                letter.to_string()
            } else {
                format!("{letter}{suffix}")
            }
        })
        .clone()
}

fn fmt_ty(
    ty: &Ty,
    names: &mut FxHashMap<TyVarId, String>,
    adts: &AdtRegistry,
    interner: &Interner,
) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Var(v) => var_name(*v, names),
        Ty::List(t) => format!("[{}]", fmt_ty(t, names, adts, interner)),
        Ty::Fun(params, ret) => {
            let params_str: Vec<String> = params
                .iter()
                .map(|p| fmt_ty(p, names, adts, interner))
                .collect();
            let ret_str = fmt_ty(ret, names, adts, interner);
            match params_str.as_slice() {
                // A single-parameter function reads naturally without
                // parens (`a -> a`); zero or multiple parameters need them
                // to disambiguate (`() -> a`, `(a, b) -> b`).
                [single] => format!("{single} -> {ret_str}"),
                _ => format!("({}) -> {ret_str}", params_str.join(", ")),
            }
        }
        Ty::Adt(id, _args) => interner.resolve(adts.name_of(*id)).to_string(),
        Ty::Record(fields) => {
            let mut entries: Vec<(String, String)> = fields
                .iter()
                .map(|(k, v)| {
                    (
                        interner.resolve(*k).to_string(),
                        fmt_ty(v, names, adts, interner),
                    )
                })
                .collect();
            entries.sort();
            let body: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            format!("{{{}}}", body.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adt::AdtRegistry;
    use crate::subst::Subst;
    use crate::ty::{Scheme, Ty};
    use ember_ast::Interner;

    #[test]
    fn concrete_types_print_plainly() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        assert_eq!(display_ty(&Ty::Int, &mut subst, &adts, &interner), "Int");
        assert_eq!(
            display_ty(&Ty::List(Box::new(Ty::Bool)), &mut subst, &adts, &interner),
            "[Bool]"
        );
        assert_eq!(
            display_ty(
                &Ty::Fun(vec![Ty::Int, Ty::Int], Box::new(Ty::Bool)),
                &mut subst,
                &adts,
                &interner
            ),
            "(Int, Int) -> Bool"
        );
    }

    #[test]
    fn unbound_vars_get_readable_letter_names() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let a = subst.fresh();
        let b = subst.fresh();
        let fun = Ty::Fun(vec![a, b.clone()], Box::new(b));
        assert_eq!(
            display_ty(&fun, &mut subst, &adts, &interner),
            "(a, b) -> b"
        );
    }

    #[test]
    fn named_adt_prints_its_declared_name() {
        let mut subst = Subst::new();
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let mut adts = AdtRegistry::new();
        let id = adts.register_enum(shape, vec![]);
        assert_eq!(
            display_ty(&Ty::Adt(id, vec![]), &mut subst, &adts, &interner),
            "Shape"
        );
    }

    #[test]
    fn scheme_prints_its_quantifier() {
        let mut subst = Subst::new();
        let adts = AdtRegistry::new();
        let interner = Interner::new();
        let a = subst.fresh();
        let crate::ty::Ty::Var(id) = a.clone() else {
            unreachable!()
        };
        let scheme = Scheme {
            vars: vec![id],
            ty: Ty::Fun(vec![a.clone()], Box::new(a)),
        };
        assert_eq!(
            display_scheme(&scheme, &mut subst, &adts, &interner),
            "forall a. a -> a"
        );
    }
}
