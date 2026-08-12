use crate::idx::Idx;
use crate::interner::Symbol;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum TypeExpr {
    Name(Symbol),
    Generic {
        name: Symbol,
        args: Vec<Idx<TypeExpr>>,
    },
    List(Idx<TypeExpr>),
    Fun {
        params: Vec<Idx<TypeExpr>>,
        ret: Idx<TypeExpr>,
    },
    Error,
}
