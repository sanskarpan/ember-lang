use ember_lexer::TokenKind;

use crate::idx::Idx;
use crate::interner::Symbol;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(Symbol),
    Bool(bool),
    Nil,
    Var(Symbol),
    Unary {
        op: TokenKind,
        operand: Idx<Expr>,
    },
    Binary {
        op: TokenKind,
        lhs: Idx<Expr>,
        rhs: Idx<Expr>,
    },
    Assign {
        target: Idx<Expr>,
        value: Idx<Expr>,
    },
    Call {
        callee: Idx<Expr>,
        args: Vec<Idx<Expr>>,
    },
    Index {
        base: Idx<Expr>,
        index: Idx<Expr>,
    },
    Field {
        base: Idx<Expr>,
        name: Symbol,
    },
    Lambda {
        params: Vec<crate::stmt::Param>,
        body: Idx<Expr>,
    },
    If {
        cond: Idx<Expr>,
        then_: Idx<Expr>,
        else_: Option<Idx<Expr>>,
    },
    Match {
        scrutinee: Idx<Expr>,
        arms: Vec<crate::pattern::MatchArm>,
    },
    Block {
        stmts: Vec<Idx<crate::stmt::Stmt>>,
        tail: Option<Idx<Expr>>,
    },
    List {
        items: Vec<Idx<Expr>>,
    },
    Struct {
        name: Symbol,
        fields: Vec<(Symbol, Idx<Expr>)>,
    },
    Error,
}
