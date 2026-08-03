use crate::expr::Expr;
use crate::idx::Idx;
use crate::interner::Symbol;
use crate::ty::TypeExpr;

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Symbol,
    pub ty: Option<Idx<TypeExpr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdtVariant {
    pub name: Symbol,
    pub payload: Vec<Idx<TypeExpr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: Symbol,
    pub ty: Idx<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: Symbol,
        mutable: bool,
        ty: Option<Idx<TypeExpr>>,
        init: Idx<Expr>,
    },
    ExprStmt(Idx<Expr>),
    Fn {
        name: Symbol,
        params: Vec<Param>,
        ret_ty: Option<Idx<TypeExpr>>,
        body: Idx<Expr>,
    },
    TypeDecl {
        name: Symbol,
        variants: Vec<AdtVariant>,
    },
    StructDecl {
        name: Symbol,
        fields: Vec<FieldDecl>,
    },
    While {
        cond: Idx<Expr>,
        body: Idx<Expr>,
    },
    For {
        binding: Symbol,
        iter: Idx<Expr>,
        body: Idx<Expr>,
    },
    Loop {
        body: Idx<Expr>,
    },
    Return(Option<Idx<Expr>>),
    Break,
    Continue,
    Error,
}
