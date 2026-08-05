use ember_ast::Symbol;
use ember_span::Span;
use rustc_hash::FxHashMap;

use crate::binding::{BindingInfo, FunctionId, UpvalueDesc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Block,
    Function,
    Loop,
    Match,
}

pub struct Scope {
    pub bindings: FxHashMap<Symbol, BindingInfo>,
    pub kind: ScopeKind,
}

impl Scope {
    pub fn new(kind: ScopeKind) -> Self {
        Scope {
            bindings: FxHashMap::default(),
            kind,
        }
    }
}

pub struct FunctionCtx {
    pub id: FunctionId,
    pub scopes: Vec<Scope>,
    pub upvalues: Vec<UpvalueDesc>,
    pub next_slot: u32,
    pub high_water: u32,
}

impl FunctionCtx {
    pub fn new(id: FunctionId) -> Self {
        FunctionCtx {
            id,
            scopes: vec![Scope::new(ScopeKind::Function)],
            upvalues: Vec::new(),
            next_slot: 0,
            high_water: 0,
        }
    }

    pub fn push_scope(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope::new(kind));
    }

    /// Pops the current scope, releasing its slots back for reuse by later
    /// sibling scopes, and returns the popped scope so the caller can
    /// inspect its bindings (e.g. to emit unused-variable warnings) before
    /// they're gone.
    pub fn pop_scope(&mut self) -> Scope {
        let scope = self
            .scopes
            .pop()
            .expect("pop_scope called with no scope open");
        self.next_slot -= scope.bindings.len() as u32;
        scope
    }

    /// Declares a new binding in the current (innermost) scope, allocating
    /// the next free slot. `initialized` is `false` for a `let` whose
    /// initializer hasn't been resolved yet (so a self-reference in that
    /// initializer can be caught), `true` for parameters and hoisted
    /// top-level declarations.
    pub fn declare(&mut self, name: Symbol, mutable: bool, initialized: bool, span: Span) -> u32 {
        let slot = self.next_slot;
        self.next_slot += 1;
        if self.next_slot > self.high_water {
            self.high_water = self.next_slot;
        }
        let info = BindingInfo {
            slot,
            mutable,
            initialized,
            span,
            used: false,
            captured: false,
        };
        self.scopes
            .last_mut()
            .expect("no scope open")
            .bindings
            .insert(name, info);
        slot
    }

    pub fn mark_initialized(&mut self, name: Symbol) {
        if let Some(info) = self.lookup_mut(name) {
            info.initialized = true;
        }
    }

    /// Innermost-first lookup, so shadowing resolves to the nearest declaration.
    pub fn lookup(&self, name: Symbol) -> Option<&BindingInfo> {
        self.scopes.iter().rev().find_map(|s| s.bindings.get(&name))
    }

    pub fn lookup_mut(&mut self, name: Symbol) -> Option<&mut BindingInfo> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|s| s.bindings.get_mut(&name))
    }

    /// Looks up a binding **only in the outermost (function-level) scope**
    /// — used for the top-level's hoisted `fn`/`type`/`struct` names and
    /// native globals, which must resolve to `Global` regardless of how
    /// deeply nested the reference is within nested blocks of the same
    /// function-index-0 context.
    pub fn lookup_in_outermost(&self, name: Symbol) -> Option<&BindingInfo> {
        self.scopes.first().and_then(|s| s.bindings.get(&name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;
    use ember_span::Span;

    #[test]
    fn declare_and_lookup_within_one_scope() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        let slot = fc.declare(x, false, true, Span::new(0, 1));
        assert_eq!(slot, 0);
        assert_eq!(fc.lookup(x).unwrap().slot, 0);
    }

    #[test]
    fn slots_are_reused_after_scope_pop() {
        let mut interner = Interner::new();
        let a = interner.intern("a");
        let b = interner.intern("b");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.push_scope(ScopeKind::Block);
        fc.declare(a, false, true, Span::new(0, 1));
        fc.pop_scope();
        fc.push_scope(ScopeKind::Block);
        let slot = fc.declare(b, false, true, Span::new(2, 3));
        fc.pop_scope();
        assert_eq!(slot, 0, "b should reuse a's slot after a's scope exited");
    }

    #[test]
    fn shadowing_in_nested_scope_hides_outer_binding() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.declare(x, false, true, Span::new(0, 1));
        fc.push_scope(ScopeKind::Block);
        let inner_slot = fc.declare(x, false, true, Span::new(2, 3));
        assert_eq!(fc.lookup(x).unwrap().slot, inner_slot);
        fc.pop_scope();
        assert_eq!(
            fc.lookup(x).unwrap().slot,
            0,
            "outer x visible again after inner scope pops"
        );
    }

    #[test]
    fn high_water_mark_tracks_the_deepest_slot_usage() {
        let mut interner = Interner::new();
        let a = interner.intern("a");
        let b = interner.intern("b");
        let mut fc = FunctionCtx::new(FunctionId::TopLevel);
        fc.declare(a, false, true, Span::new(0, 1));
        fc.push_scope(ScopeKind::Block);
        fc.declare(b, false, true, Span::new(2, 3));
        fc.pop_scope();
        assert_eq!(
            fc.high_water, 2,
            "both a and b were alive at once, even though b's slot is now free again"
        );
    }
}
