use ember_ast::{Expr, Idx, Stmt, Symbol};
use ember_span::Span;
use rustc_hash::FxHashMap;

/// Everything the resolver knows about one declared name at one point in
/// the scope stack.
#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub slot: u32,
    pub mutable: bool,
    pub initialized: bool,
    pub span: Span,
    pub used: bool,
    /// Set once any inner function captures this binding as an upvalue.
    pub captured: bool,
}

/// Where a `Var` node's name actually lives at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Local { slot: u32 },
    Upvalue { index: u32 },
    Global { symbol: Symbol },
}

/// One entry in a function's upvalue list: either "capture slot `index`
/// from the immediately enclosing function's locals" (`is_local: true`) or
/// "capture upvalue `index` from the immediately enclosing function's own
/// upvalue list" (`is_local: false`) — the two cases `resolve_upvalue`
/// distinguishes when threading a capture through intermediate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    pub index: u32,
    pub is_local: bool,
}

/// Identifies one function-introducing AST node. `Expr::Lambda` isn't the
/// only thing that introduces a function scope — `Stmt::Fn` can appear
/// anywhere a statement can, including nested inside another function's
/// body, so both need their own identity here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionId {
    TopLevel,
    Lambda(Idx<Expr>),
    Fn(Idx<Stmt>),
}

/// The resolver's full output: everything Phase 5 (type inference) and
/// Phase 7-9 (both backends) need to know about names and closures.
#[derive(Debug, Default)]
pub struct Bindings {
    pub resolutions: FxHashMap<Idx<Expr>, Resolution>,
    pub upvalues: FxHashMap<FunctionId, Vec<UpvalueDesc>>,
    pub frame_sizes: FxHashMap<FunctionId, u32>,
    /// Which slot numbers were ever captured somewhere in a function.
    /// Coarser than `BindingInfo.captured` — a provisional shape a future
    /// bytecode compiler may want to refine once it knows exactly what it
    /// needs at each scope-exit point.
    pub captured_slots: FxHashMap<FunctionId, Vec<u32>>,
}

impl Bindings {
    pub fn new() -> Self {
        Bindings::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Idx;

    #[test]
    fn function_id_hashes_and_compares_correctly() {
        use std::collections::HashSet;
        let a = FunctionId::TopLevel;
        let b = FunctionId::Lambda(Idx::new(0));
        let c = FunctionId::Lambda(Idx::new(0));
        let d = FunctionId::Lambda(Idx::new(1));
        assert_eq!(b, c);
        assert_ne!(b, d);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c); // duplicate of b
        set.insert(d);
        assert_eq!(
            set.len(),
            3,
            "FunctionId must hash/compare correctly to dedupe in a HashSet/HashMap"
        );
    }

    #[test]
    fn bindings_starts_empty() {
        let b = Bindings::new();
        assert!(b.resolutions.is_empty());
        assert!(b.upvalues.is_empty());
        assert!(b.frame_sizes.is_empty());
        assert!(b.captured_slots.is_empty());
    }
}
