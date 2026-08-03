use crate::idx::Idx;
use crate::interner::Symbol;
use ember_span::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wild,
    Bind(Symbol),
    Int(i64),
    Float(f64),
    Str(Symbol),
    Bool(bool),
    Ctor {
        name: Symbol,
        args: Vec<Idx<Pattern>>,
    },
    Tuple(Vec<Idx<Pattern>>),
    List {
        items: Vec<Idx<Pattern>>,
        rest: Option<Idx<Pattern>>,
    },
    Record {
        name: Symbol,
        fields: Vec<(Symbol, Idx<Pattern>)>,
    },
    Or(Vec<Idx<Pattern>>),
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pat: Idx<Pattern>,
    pub guard: Option<Idx<crate::expr::Expr>>,
    pub body: Idx<crate::expr::Expr>,
    pub span: Span,
}
