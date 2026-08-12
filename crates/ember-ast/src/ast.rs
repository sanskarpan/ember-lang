use ember_span::Span;

use crate::expr::Expr;
use crate::idx::Idx;
use crate::pattern::Pattern;
use crate::stmt::Stmt;
use crate::ty::TypeExpr;

#[derive(Default, serde::Serialize)]
pub struct Ast {
    exprs: Vec<Expr>,
    expr_spans: Vec<Span>,
    stmts: Vec<Stmt>,
    stmt_spans: Vec<Span>,
    pats: Vec<Pattern>,
    pat_spans: Vec<Span>,
    type_exprs: Vec<TypeExpr>,
    type_expr_spans: Vec<Span>,
}

impl Ast {
    pub fn new() -> Self {
        Ast::default()
    }

    pub fn alloc_expr(&mut self, expr: Expr, span: Span) -> Idx<Expr> {
        let idx = Idx::new(self.exprs.len() as u32);
        self.exprs.push(expr);
        self.expr_spans.push(span);
        idx
    }

    pub fn expr(&self, idx: Idx<Expr>) -> &Expr {
        &self.exprs[idx.index()]
    }

    pub fn span_of_expr(&self, idx: Idx<Expr>) -> Span {
        self.expr_spans[idx.index()]
    }

    pub fn alloc_stmt(&mut self, stmt: Stmt, span: Span) -> Idx<Stmt> {
        let idx = Idx::new(self.stmts.len() as u32);
        self.stmts.push(stmt);
        self.stmt_spans.push(span);
        idx
    }

    pub fn stmt(&self, idx: Idx<Stmt>) -> &Stmt {
        &self.stmts[idx.index()]
    }

    pub fn span_of_stmt(&self, idx: Idx<Stmt>) -> Span {
        self.stmt_spans[idx.index()]
    }

    pub fn alloc_pat(&mut self, pat: Pattern, span: Span) -> Idx<Pattern> {
        let idx = Idx::new(self.pats.len() as u32);
        self.pats.push(pat);
        self.pat_spans.push(span);
        idx
    }

    pub fn pat(&self, idx: Idx<Pattern>) -> &Pattern {
        &self.pats[idx.index()]
    }

    pub fn span_of_pat(&self, idx: Idx<Pattern>) -> Span {
        self.pat_spans[idx.index()]
    }

    pub fn alloc_type_expr(&mut self, ty: TypeExpr, span: Span) -> Idx<TypeExpr> {
        let idx = Idx::new(self.type_exprs.len() as u32);
        self.type_exprs.push(ty);
        self.type_expr_spans.push(span);
        idx
    }

    pub fn type_expr(&self, idx: Idx<TypeExpr>) -> &TypeExpr {
        &self.type_exprs[idx.index()]
    }

    pub fn span_of_type_expr(&self, idx: Idx<TypeExpr>) -> Span {
        self.type_expr_spans[idx.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::Pattern;
    use crate::stmt::Stmt;
    use ember_span::Span;

    #[test]
    fn idx_is_4_bytes_and_copy() {
        assert_eq!(std::mem::size_of::<Idx<Expr>>(), 4);
    }

    #[test]
    fn alloc_expr_records_span_and_is_retrievable() {
        let mut ast = Ast::new();
        let idx = ast.alloc_expr(Expr::Int(42), Span::new(0, 2));
        assert_eq!(ast.expr(idx), &Expr::Int(42));
        assert_eq!(ast.span_of_expr(idx), Span::new(0, 2));
    }

    #[test]
    fn distinct_allocations_get_distinct_indices() {
        let mut ast = Ast::new();
        let a = ast.alloc_expr(Expr::Int(1), Span::new(0, 1));
        let b = ast.alloc_expr(Expr::Int(2), Span::new(2, 3));
        assert_ne!(a, b);
        assert_eq!(ast.expr(a), &Expr::Int(1));
        assert_eq!(ast.expr(b), &Expr::Int(2));
    }

    #[test]
    fn alloc_stmt_and_alloc_pat_record_spans() {
        let mut ast = Ast::new();
        let e = ast.alloc_expr(Expr::Int(1), Span::new(0, 1));
        let s = ast.alloc_stmt(Stmt::ExprStmt(e), Span::new(0, 2));
        assert_eq!(ast.span_of_stmt(s), Span::new(0, 2));

        let p = ast.alloc_pat(Pattern::Wild, Span::new(3, 4));
        assert_eq!(ast.span_of_pat(p), Span::new(3, 4));
        assert_eq!(ast.pat(p), &Pattern::Wild);
    }
}
