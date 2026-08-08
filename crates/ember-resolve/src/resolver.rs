use ember_ast::{Ast, Expr, Idx, Interner, Stmt};
use ember_diag::Diagnostic;

use crate::binding::{Bindings, FunctionId};
use crate::edit_distance::closest_match;
use crate::scope::FunctionCtx;

const NATIVE_GLOBALS: &[&str] = &[
    "print", "len", "push", "clock", "str", "int", "float", "type_of",
];

pub struct Resolver<'a> {
    ast: &'a Ast,
    interner: &'a mut Interner,
    functions: Vec<FunctionCtx>,
    diagnostics: Vec<Diagnostic>,
    bindings: Bindings,
}

impl<'a> Resolver<'a> {
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        let mut r = Resolver {
            ast,
            interner,
            functions: vec![FunctionCtx::new(FunctionId::TopLevel)],
            diagnostics: Vec::new(),
            bindings: Bindings::new(),
        };
        r.seed_native_globals();
        r
    }

    fn seed_native_globals(&mut self) {
        for name in NATIVE_GLOBALS {
            let sym = self.interner.intern(name);
            self.functions[0].declare(sym, false, true, ember_span::Span::new(0, 0));
            // Natives are never "unused" — nothing to warn about even if a
            // program never calls e.g. `clock`.
            if let Some(info) = self.functions[0].lookup_mut(sym) {
                info.used = true;
            }
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_bindings(self) -> (Bindings, Vec<Diagnostic>) {
        (self.bindings, self.diagnostics)
    }

    fn emit(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    /// All names currently reachable, for "did you mean?" — every scope of
    /// every function currently on the stack (order doesn't matter for the
    /// suggestion itself, only that everything visible right now is
    /// included).
    fn reachable_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for fc in &self.functions {
            for scope in &fc.scopes {
                for sym in scope.bindings.keys() {
                    names.push(self.interner.resolve(*sym).to_string());
                }
            }
        }
        names
    }

    /// Resolves a single name reference by walking the current function's
    /// scopes innermost-first, then (for now, until later tasks add real
    /// upvalue-chain support) falling back to the top-level's outermost
    /// scope for globals. Marks the binding used on a local hit. Returns
    /// the `Resolution` to record, or `None` if an "undeclared name"
    /// diagnostic was emitted instead.
    fn resolve_name(
        &mut self,
        name_sym: ember_ast::Symbol,
        name_text: &str,
        span: ember_span::Span,
    ) -> Option<crate::binding::Resolution> {
        let current = self.functions.len() - 1;
        if let Some(info) = self.functions[current].lookup_mut(name_sym) {
            if !info.initialized {
                let init_span = info.span;
                self.emit(
                    Diagnostic::error(format!("cannot use `{name_text}` in its own initializer"))
                        .with_primary(span, "used here")
                        .with_secondary(init_span, "while initializing this binding"),
                );
                return None;
            }
            info.used = true;
            return Some(crate::binding::Resolution::Local { slot: info.slot });
        }
        if let Some((up_idx, _mutable)) = self.resolve_upvalue(current, name_sym) {
            return Some(crate::binding::Resolution::Upvalue { index: up_idx });
        }
        if let Some(info) = self.functions[0]
            .scopes
            .first_mut()
            .and_then(|s| s.bindings.get_mut(&name_sym))
        {
            // A reference resolved as a global (e.g. one top-level `fn`
            // calling another) IS a use of that binding, even though it
            // isn't reached through the normal scope-stack `lookup_mut`
            // path above.
            info.used = true;
            return Some(crate::binding::Resolution::Global { symbol: name_sym });
        }
        let suggestion = {
            let names = self.reachable_names();
            closest_match(name_text, names.iter().map(|s| s.as_str())).map(|s| s.to_string())
        };
        let mut diag = Diagnostic::error(format!("undeclared name `{name_text}`"))
            .with_primary(span, "not found in this scope");
        if let Some(sugg) = suggestion {
            diag = diag.with_help(format!("did you mean `{sugg}`?"));
        }
        self.emit(diag);
        None
    }

    /// Walk outward through enclosing functions looking for `name`. Each
    /// level that gets passed through must ALSO capture it, forming a
    /// chain, so a variable captured three levels deep threads an upvalue
    /// through every intermediate closure. Returns the new upvalue's index
    /// in `functions[fn_idx]`'s own upvalue list, plus whether the
    /// ORIGINAL captured binding was mutable (needed by assignment-target
    /// resolution, which can't otherwise tell without a second walk).
    fn resolve_upvalue(&mut self, fn_idx: usize, name: ember_ast::Symbol) -> Option<(u32, bool)> {
        if fn_idx == 0 {
            return None; // no enclosing function — must be resolved as a global instead
        }
        let enclosing = fn_idx - 1;
        // The top level's outermost (script) scope holds natives, hoisted
        // `fn`/`type`/`struct` names, and already-resolved top-level `let`s.
        // Structurally those live in `functions[0]`'s scope stack just like
        // any other binding, but semantically they're Globals, not
        // capturable locals — `resolve_name`/`resolve_assign_target` already
        // have a dedicated Global check for `functions[0]`'s outermost
        // scope, so skip case 1 here for exactly that scope and let it fall
        // through (case 2 below recurses into `resolve_upvalue(0, ..)`,
        // which immediately returns `None` via the base case above). A
        // *block*-scoped top-level local (nested inside `{ }` directly at
        // top level, not the outermost scope) is still a real capturable
        // local and must not be skipped.
        let found_in_toplevel_script_scope = enclosing == 0
            && self.functions[0]
                .scopes
                .iter()
                .rposition(|s| s.bindings.contains_key(&name))
                == Some(0);
        // Case 1: a local of the IMMEDIATELY enclosing function.
        if !found_in_toplevel_script_scope {
            if let Some(info) = self.functions[enclosing].lookup_mut(name) {
                info.captured = true;
                // A variable captured by an inner closure IS consumed — by
                // that closure — even though this function's own body never
                // reads it directly, so it must not be flagged "unused".
                info.used = true;
                let slot = info.slot;
                let mutable = info.mutable;
                let enclosing_id = self.functions[enclosing].id;
                self.bindings
                    .captured_slots
                    .entry(enclosing_id)
                    .or_default()
                    .push(slot);
                return Some((self.add_upvalue(fn_idx, slot, true), mutable));
            }
        }
        // Case 2: further out — recurse, and thread the result through this
        // level too via add_upvalue(is_local: false).
        let (outer_index, mutable) = self.resolve_upvalue(enclosing, name)?;
        Some((self.add_upvalue(fn_idx, outer_index, false), mutable))
    }

    /// Deduplicates: capturing the same variable twice (or two sibling
    /// closures capturing the same slot) reuses one index rather than
    /// allocating a second, separate upvalue entry.
    fn add_upvalue(&mut self, fn_idx: usize, index: u32, is_local: bool) -> u32 {
        let ups = &mut self.functions[fn_idx].upvalues;
        if let Some(i) = ups
            .iter()
            .position(|u| u.index == index && u.is_local == is_local)
        {
            return i as u32;
        }
        ups.push(crate::binding::UpvalueDesc { index, is_local });
        (ups.len() - 1) as u32
    }

    /// Checks a set of just-departed bindings for anything never marked
    /// `used`, skipping `_`-prefixed names. Takes ownership of the bindings
    /// (rather than borrowing) since every call site already has them by
    /// value — either freshly popped off a scope stack, or drained out of the
    /// top-level scope at the very end of resolution.
    fn check_unused(
        &mut self,
        bindings: rustc_hash::FxHashMap<ember_ast::Symbol, crate::binding::BindingInfo>,
    ) {
        for (sym, info) in bindings {
            if info.used {
                continue;
            }
            let name = self.interner.resolve(sym).to_string();
            if name.starts_with('_') {
                continue;
            }
            self.diagnostics.push(
                Diagnostic::warning(format!("unused variable `{name}`"))
                    .with_primary(info.span, "never used")
                    .with_help(format!(
                        "prefix with an underscore (`_{name}`) if this is intentional"
                    )),
            );
        }
    }
}

impl<'a> Resolver<'a> {
    pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
        for &s in stmts {
            match self.ast.stmt(s) {
                Stmt::Fn { name, .. } | Stmt::StructDecl { name, .. } => {
                    let span = self.ast.span_of_stmt(s);
                    self.functions[0].declare(*name, false, true, span);
                }
                Stmt::TypeDecl { name, variants } => {
                    let span = self.ast.span_of_stmt(s);
                    self.functions[0].declare(*name, false, true, span);
                    for variant in variants {
                        self.functions[0].declare(variant.name, false, true, span);
                    }
                }
                _ => {}
            }
        }
        for &s in stmts {
            self.resolve_stmt(s);
        }
        let top_level_bindings = std::mem::take(&mut self.functions[0].scopes[0].bindings);
        self.check_unused(top_level_bindings);
    }

    /// Resolves the body of a function-introducing node (`fn` or lambda) in
    /// a fresh `FunctionCtx`, binds its params first, then records the
    /// resulting frame size and upvalue list in `Bindings` once the body is
    /// fully resolved.
    fn resolve_function_body(
        &mut self,
        id: crate::binding::FunctionId,
        params: &[ember_ast::Param],
        body: Idx<Expr>,
    ) {
        self.functions.push(FunctionCtx::new(id));
        for p in params {
            self.functions
                .last_mut()
                .unwrap()
                .declare(p.name, false, true, p.span);
        }
        self.resolve_expr(body);
        let fc = self
            .functions
            .pop()
            .expect("just pushed a function context");
        self.bindings.frame_sizes.insert(fc.id, fc.high_water);
        self.bindings.upvalues.insert(fc.id, fc.upvalues);
        // Guarantees an entry exists even for a function that captures nothing —
        // a LATER task's resolve_upvalue pushes into this map directly at the
        // moment of capture, so this call must not overwrite anything it
        // already added during this function's own body resolution (e.g. if a
        // closure nested inside THIS function captured one of THIS function's
        // locals). `entry().or_default()` only inserts if absent, so it's safe.
        self.bindings.captured_slots.entry(fc.id).or_default();
        for scope in fc.scopes {
            self.check_unused(scope.bindings);
        }
    }

    fn resolve_stmt(&mut self, idx: Idx<Stmt>) {
        match self.ast.stmt(idx) {
            Stmt::ExprStmt(e) => self.resolve_expr(*e),
            Stmt::Let {
                name,
                mutable,
                init,
                ..
            } => {
                let span = self.ast.span_of_stmt(idx);
                let current = self.functions.len() - 1;
                // Declare BEFORE resolving the initializer, uninitialized. This
                // is what makes `let x = x;` see its own not-yet-ready binding
                // (and therefore error) instead of silently falling through to
                // an outer `x` of the same name.
                self.functions[current].declare(*name, *mutable, false, span);
                self.resolve_expr(*init);
                self.functions[current].mark_initialized(*name);
            }
            Stmt::Fn {
                name, params, body, ..
            } => {
                let current = self.functions.len() - 1;
                let already_hoisted =
                    current == 0 && self.functions[0].lookup_in_outermost(*name).is_some();
                if !already_hoisted {
                    let span = self.ast.span_of_stmt(idx);
                    self.functions[current].declare(*name, false, true, span);
                }
                self.resolve_function_body(crate::binding::FunctionId::Fn(idx), params, *body);
            }
            Stmt::While { cond, body } => {
                self.resolve_expr(*cond);
                self.resolve_expr(*body);
            }
            Stmt::For {
                binding,
                iter,
                body,
            } => {
                self.resolve_expr(*iter);
                let current = self.functions.len() - 1;
                self.functions[current].push_scope(crate::scope::ScopeKind::Loop);
                // `Stmt::For` doesn't carry a dedicated span for just the loop
                // variable, only for the whole statement — using the whole-statement
                // span here means a future unused-loop-variable warning would point at
                // the entire `for` line rather than just the binding identifier. A
                // minor precision gap, not a correctness one.
                let span = self.ast.span_of_stmt(idx);
                self.functions[current].declare(*binding, false, true, span);
                self.resolve_expr(*body);
                let popped = self.functions[current].pop_scope();
                self.check_unused(popped.bindings);
            }
            Stmt::Loop { body } => self.resolve_expr(*body),
            Stmt::Return(value) => {
                if let Some(v) = value {
                    self.resolve_expr(*v);
                }
            }
            Stmt::Break
            | Stmt::Continue
            | Stmt::TypeDecl { .. }
            | Stmt::StructDecl { .. }
            | Stmt::Error => {}
        }
    }

    fn resolve_expr(&mut self, idx: Idx<Expr>) {
        match self.ast.expr(idx) {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Nil
            | Expr::Str(_)
            | Expr::Error => {}
            Expr::Var(sym) => {
                let span = self.ast.span_of_expr(idx);
                let text = self.interner.resolve(*sym).to_string();
                if let Some(res) = self.resolve_name(*sym, &text, span) {
                    self.bindings.resolutions.insert(idx, res);
                }
            }
            Expr::Call { callee, args } => {
                self.resolve_expr(*callee);
                for a in args {
                    self.resolve_expr(*a);
                }
            }
            Expr::Assign { target, value } => {
                self.resolve_expr(*value);
                self.resolve_assign_target(*target);
            }
            Expr::Block { stmts, tail } => {
                let current = self.functions.len() - 1;
                self.functions[current].push_scope(crate::scope::ScopeKind::Block);
                let mut unreachable = false;
                for s in stmts {
                    if unreachable {
                        let span = self.ast.span_of_stmt(*s);
                        self.emit(
                            Diagnostic::warning("unreachable code")
                                .with_primary(span, "this code can never run"),
                        );
                    }
                    self.resolve_stmt(*s);
                    if matches!(
                        self.ast.stmt(*s),
                        Stmt::Return(_) | Stmt::Break | Stmt::Continue
                    ) {
                        unreachable = true;
                    }
                }
                if let Some(t) = tail {
                    if unreachable {
                        let span = self.ast.span_of_expr(*t);
                        self.emit(
                            Diagnostic::warning("unreachable code")
                                .with_primary(span, "this code can never run"),
                        );
                    }
                    self.resolve_expr(*t);
                }
                let popped = self.functions[current].pop_scope();
                self.check_unused(popped.bindings);
            }
            Expr::Lambda { params, body } => {
                self.resolve_function_body(crate::binding::FunctionId::Lambda(idx), params, *body);
            }
            Expr::Unary { operand, .. } => self.resolve_expr(*operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(*lhs);
                self.resolve_expr(*rhs);
            }
            Expr::Index { base, index } => {
                self.resolve_expr(*base);
                self.resolve_expr(*index);
            }
            Expr::Field { base, .. } => self.resolve_expr(*base),
            Expr::If { cond, then_, else_ } => {
                self.resolve_expr(*cond);
                self.resolve_expr(*then_);
                if let Some(e) = else_ {
                    self.resolve_expr(*e);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.resolve_expr(*scrutinee);
                // Reserve a hidden local slot for the match's own
                // scrutinee, for the whole match, before resolving any
                // arm's pattern bindings — mirrors `ember-compile`'s
                // `compile_match`, which evaluates the scrutinee once into
                // its own local slot and keeps it pinned there across
                // every arm's `compile_pattern_test`/`compile_pattern_bind`
                // (each arm's `Op::Destructure`/`Op::GetField` reads it
                // back via `scrutinee_slot`), releasing it only once via a
                // single trailing `emit_tail_scope_exit` after every arm.
                // Without this reservation here, `declare_pattern_bindings`
                // hands out slot numbers as though the scrutinee occupied
                // no slot at all, one lower than where the compiler
                // actually places each binding — so `Expr::Var` reads for
                // a pattern-bound name (e.g. `r` in `Circle(r) => r`) come
                // out one slot short, silently reading whatever sits right
                // before the real binding (the scrutinee itself, for a
                // single-binding pattern) instead.
                let current = self.functions.len() - 1;
                self.functions[current].next_slot += 1;
                if self.functions[current].next_slot > self.functions[current].high_water {
                    self.functions[current].high_water = self.functions[current].next_slot;
                }
                for arm in arms {
                    self.resolve_match_arm(arm);
                }
                self.functions[current].next_slot -= 1;
            }
            Expr::List { items } => {
                for i in items {
                    self.resolve_expr(*i);
                }
            }
            Expr::Struct { fields, .. } => {
                for (_, v) in fields {
                    self.resolve_expr(*v);
                }
            }
        }
    }

    fn resolve_match_arm(&mut self, arm: &ember_ast::MatchArm) {
        let current = self.functions.len() - 1;
        self.functions[current].push_scope(crate::scope::ScopeKind::Match);
        self.declare_pattern_bindings(arm.pat);
        if let Some(g) = arm.guard {
            self.resolve_expr(g);
        }
        self.resolve_expr(arm.body);
        let popped = self.functions[current].pop_scope();
        self.check_unused(popped.bindings);
    }

    /// Walks a pattern, declaring every name it binds into the CURRENT (already
    /// pushed) scope. Simplification: for or-patterns (`A | B`), every
    /// alternative's bindings are declared into the same scope rather than
    /// requiring — and cross-checking — that every alternative binds exactly
    /// the same names; that fuller check belongs to a later phase with more
    /// context (it's a usability nicety, not a soundness issue at this phase).
    fn declare_pattern_bindings(&mut self, pat: Idx<ember_ast::Pattern>) {
        use ember_ast::Pattern;
        let span = self.ast.span_of_pat(pat);
        let current = self.functions.len() - 1;
        match self.ast.pat(pat) {
            Pattern::Wild
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Error => {}
            Pattern::Bind(sym) => {
                self.functions[current].declare(*sym, false, true, span);
            }
            Pattern::Ctor { args, .. } => {
                for a in args {
                    self.declare_pattern_bindings(*a);
                }
            }
            Pattern::Tuple(items) => {
                for i in items {
                    self.declare_pattern_bindings(*i);
                }
            }
            Pattern::List { items, rest } => {
                for i in items {
                    self.declare_pattern_bindings(*i);
                }
                if let Some(r) = rest {
                    self.declare_pattern_bindings(*r);
                }
            }
            Pattern::Record { fields, .. } => {
                for (_, p) in fields {
                    self.declare_pattern_bindings(*p);
                }
            }
            Pattern::Or(alts) => {
                for a in alts {
                    self.declare_pattern_bindings(*a);
                }
            }
        }
    }

    fn resolve_assign_target(&mut self, idx: Idx<Expr>) {
        match self.ast.expr(idx) {
            Expr::Var(sym) => {
                let span = self.ast.span_of_expr(idx);
                let text = self.interner.resolve(*sym).to_string();
                let current = self.functions.len() - 1;
                if let Some(info) = self.functions[current].lookup_mut(*sym) {
                    if !info.initialized {
                        let init_span = info.span;
                        self.emit(
                            Diagnostic::error(format!(
                                "cannot use `{text}` in its own initializer"
                            ))
                            .with_primary(span, "used here")
                            .with_secondary(init_span, "while initializing this binding"),
                        );
                        return;
                    }
                    if !info.mutable {
                        let decl_span = info.span;
                        self.emit(
                            Diagnostic::error(format!(
                                "cannot assign to immutable variable `{text}`"
                            ))
                            .with_primary(span, "assigned here")
                            .with_secondary(decl_span, "declared here")
                            .with_help(format!("consider changing to `let mut {text}`")),
                        );
                        return;
                    }
                    info.used = true;
                    let slot = info.slot;
                    self.bindings
                        .resolutions
                        .insert(idx, crate::binding::Resolution::Local { slot });
                    return;
                }
                if let Some((up_idx, mutable)) = self.resolve_upvalue(current, *sym) {
                    if !mutable {
                        self.emit(
                            Diagnostic::error(format!(
                                "cannot assign to immutable captured variable `{text}`"
                            ))
                            .with_primary(span, "assigned here")
                            .with_help(format!(
                                "consider changing the outer binding to `let mut {text}`"
                            )),
                        );
                        return;
                    }
                    self.bindings
                        .resolutions
                        .insert(idx, crate::binding::Resolution::Upvalue { index: up_idx });
                    return;
                }
                if let Some(info) = self.functions[0]
                    .scopes
                    .first_mut()
                    .and_then(|s| s.bindings.get_mut(sym))
                {
                    info.used = true;
                    self.bindings
                        .resolutions
                        .insert(idx, crate::binding::Resolution::Global { symbol: *sym });
                    return;
                }
                self.resolve_name(*sym, &text, span);
            }
            Expr::Index { base, index } => {
                self.resolve_expr(*base);
                self.resolve_expr(*index);
            }
            Expr::Field { base, .. } => {
                self.resolve_expr(*base);
            }
            _ => self.resolve_expr(idx),
        }
    }
}

pub fn resolve(
    ast: &Ast,
    interner: &mut Interner,
    stmts: &[Idx<Stmt>],
) -> (Bindings, Vec<Diagnostic>) {
    let mut resolver = Resolver::new(ast, interner);
    resolver.resolve_program(stmts);
    resolver.into_bindings()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_globals_resolve_without_error() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("print(1);");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn undeclared_name_is_an_error_with_a_suggestion() {
        let (ast, mut interner, stmts, parse_diags) =
            ember_parser::parse("let count = 1;\nprin(count);");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(errors[0].message.contains("prin"));
        assert!(
            errors[0].help.iter().any(|h| h.message.contains("print")),
            "expected a did-you-mean suggestion toward `print`, got: {:?}",
            errors[0].help
        );
    }

    #[test]
    fn let_binding_resolves_its_initializer_then_becomes_usable() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\nprint(x);");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn let_x_equals_x_is_an_error() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = x;");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(
            errors[0].message.contains("its own initializer"),
            "message: {}",
            errors[0].message
        );
    }

    #[test]
    fn assignment_to_mutable_binding_is_fine() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let mut x = 1;\nx = 2;");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn assignment_to_immutable_binding_errors_with_a_mut_suggestion() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 1;\nx = 2;");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(
            errors[0].message.contains("immutable"),
            "message: {}",
            errors[0].message
        );
        assert!(
            errors[0].help.iter().any(|h| h.message.contains("let mut")),
            "expected a `let mut` suggestion, got: {:?}",
            errors[0].help
        );
    }

    #[test]
    fn nested_scope_shadowing_resolves_to_the_innermost_binding() {
        let src = "let x = 1;\nlet y = { let x = 2; x };\nprint(y);\nprint(x);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn let_x_equals_x_is_an_error_even_with_an_outer_x_in_scope() {
        let (ast, mut interner, stmts, parse_diags) =
            ember_parser::parse("let x = 1;\n{ let x = x; }");
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(
            errors[0].message.contains("its own initializer"),
            "message: {}",
            errors[0].message
        );
    }

    #[test]
    fn sibling_blocks_can_each_declare_their_own_locals_without_slot_conflicts() {
        let src = "{ let a = 1; print(a); }\n{ let b = 2; print(b); }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn mutual_recursion_works_regardless_of_declaration_order() {
        let src = "fn is_even(n) { is_odd(n) }\nfn is_odd(n) { is_even(n) }\nprint(is_even(4));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn fn_params_resolve_inside_the_body() {
        let src = "fn add(a, b) { print(a); print(b); a }\nprint(add(1, 2));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn frame_size_and_upvalues_are_recorded_per_function() {
        let src = "fn add(a, b) { print(a); b }\nprint(add(1, 2));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let (bindings, diags) = resolver.into_bindings();
        assert!(diags.is_empty(), "diags: {diags:?}");
        let fn_id = crate::binding::FunctionId::Fn(stmts[0]);
        assert_eq!(
            bindings.frame_sizes.get(&fn_id),
            Some(&2),
            "two params, both alive at once"
        );
        assert_eq!(
            bindings.upvalues.get(&fn_id),
            Some(&vec![]),
            "non-capturing function has zero upvalues"
        );
    }

    #[test]
    fn non_capturing_lambda_resolves_its_own_params() {
        let src = "let f = |x, y| { print(y); x };\nprint(f(1, 2));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn counter_closure_captures_exactly_one_upvalue() {
        let src = "fn make_counter() {\n  let mut n = 0;\n  |x| { n = x; n }\n}\nlet c = make_counter();\nprint(c(1));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
        let (bindings, _) = resolver.into_bindings();
        let lambda_upvalues: Vec<_> = bindings
            .upvalues
            .iter()
            .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
            .collect();
        assert_eq!(
            lambda_upvalues.len(),
            1,
            "exactly one lambda in this program"
        );
        let (_, ups) = lambda_upvalues[0];
        assert_eq!(ups.len(), 1, "the lambda captures exactly one upvalue (n)");
        assert!(
            ups[0].is_local,
            "n is captured directly from make_counter's own locals"
        );
    }

    #[test]
    fn triple_nested_capture_threads_through_every_level() {
        let src =
            "fn outer() {\n  let x = 1;\n  || {\n    || {\n      || { x }\n    }\n  }\n}\nouter();";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
        let (bindings, _) = resolver.into_bindings();
        let lambda_upvalue_counts: Vec<usize> = bindings
            .upvalues
            .iter()
            .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
            .map(|(_, ups)| ups.len())
            .collect();
        assert_eq!(lambda_upvalue_counts.len(), 3, "three nested lambdas");
        assert!(
            lambda_upvalue_counts.iter().all(|&n| n == 1),
            "every level threads exactly one upvalue for x: {lambda_upvalue_counts:?}"
        );
    }

    #[test]
    fn capturing_the_same_variable_twice_deduplicates_to_one_upvalue() {
        let src = "fn outer() {\n  let x = 1;\n  || { print(x); x }\n}\nouter();";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
        let (bindings, _) = resolver.into_bindings();
        let lambda_ups: Vec<_> = bindings
            .upvalues
            .values()
            .filter(|v| !v.is_empty())
            .collect();
        assert_eq!(lambda_ups.len(), 1);
        assert_eq!(
            lambda_ups[0].len(),
            1,
            "x is referenced twice in the body but must dedupe to exactly one upvalue entry"
        );
    }

    #[test]
    fn two_sibling_closures_capturing_the_same_variable_reference_the_same_slot() {
        let src = "fn make_pair() {\n  let mut n = 0;\n  let inc = || { n };\n  let get = || { n };\n  inc\n}";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        // `make_pair` and its local `get` are intentionally never called/used
        // in this source — the test is about upvalue slot-sharing between
        // sibling closures, not about unused-binding warnings — so only
        // assert there are no errors, not that diagnostics are empty.
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
        let (bindings, _) = resolver.into_bindings();
        let lambda_ups: Vec<_> = bindings
            .upvalues
            .iter()
            .filter(|(id, _)| matches!(id, crate::binding::FunctionId::Lambda(_)))
            .map(|(_, ups)| ups.clone())
            .collect();
        assert_eq!(lambda_ups.len(), 2, "two closures");
        assert_eq!(
            lambda_ups[0], lambda_ups[1],
            "both closures capture the same variable, so their upvalue descriptors (same slot index, is_local) must be identical — this is what lets a future VM's capture_upvalue recognize and merge them into one shared cell"
        );
    }

    #[test]
    fn assigning_to_a_mutable_captured_variable_is_fine() {
        let src = "fn make_setter() {\n  let mut n = 0;\n  |v| { n = v; n }\n}\nmake_setter();";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn assigning_to_an_immutable_captured_variable_errors() {
        let src = "fn make_setter() {\n  let n = 0;\n  |v| { n = v; n }\n}";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(
            errors[0].message.contains("immutable"),
            "message: {}",
            errors[0].message
        );
    }

    #[test]
    fn unary_binary_index_field_and_list_struct_all_resolve_their_subexpressions() {
        let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };\nlet xs = [1, 2, 3];\nlet a = -xs[0];\nlet b = p.x;\nprint(a);\nprint(b);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        // `Expr::Struct` doesn't resolve/mark-used its type name (`Point`)
        // against the declared struct binding — a separate, pre-existing
        // gap unrelated to what this test covers (that every subexpression
        // form gets visited) — so assert no errors rather than zero
        // diagnostics of any kind.
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn full_mutual_recursion_with_real_if_and_binary_operators() {
        let src = "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }\nprint(is_even(4));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn match_arm_patterns_introduce_scoped_bindings() {
        let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r,\n    Rect(w, h) => w,\n    Point => 0.0,\n  }\n}";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        // `h`, `Point`, `area`, and `Shape` are all genuinely unused in this
        // source — the test's focus is that match-arm patterns introduce
        // correctly SCOPED bindings (no resolution errors), not that every
        // binding gets consumed — so only assert no errors.
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn list_pattern_rest_binding_is_usable_in_the_arm_body() {
        let src =
            "fn describe(xs) {\n  match xs {\n    [head, ..tail] => head,\n    [] => 0,\n  }\n}";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn unused_local_variable_produces_a_warning() {
        let src = "fn f() { let x = 1; 2 }\nprint(f());";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(warnings[0].message.contains('x'));
    }

    #[test]
    fn underscore_prefixed_names_suppress_the_unused_warning() {
        let src = "fn f() { let _x = 1; 2 }\nprint(f());";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert!(warnings.is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn unused_function_parameter_produces_a_warning() {
        let src = "fn f(a, b) { a }\nprint(f(1, 2));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(warnings[0].message.contains('b'));
    }

    #[test]
    fn unused_top_level_function_produces_a_warning() {
        let src = "fn never_called() { 1 }\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(warnings[0].message.contains("never_called"));
    }

    #[test]
    fn used_variable_produces_no_warning() {
        let src = "fn f() { let x = 1; x }\nprint(f());";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn loop_forms_and_control_flow_statements_all_resolve() {
        let src = "let mut i = 0;\nwhile i == 0 { i = 1; }\nfor x in xs { print(x); }\nloop { break; }\nfn f() { return 1; }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        // `xs` in the `for` loop is intentionally undeclared here — this test is
        // about every statement FORM being visited (so `i`, `x`, `break`,
        // `return 1` all get resolved without panicking), not about every name
        // in it being declared. Assert specifically that it's the ONLY error.
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(errors[0].message.contains("xs"));
    }

    #[test]
    fn code_after_return_is_flagged_unreachable() {
        let src = "fn f() { return 1; print(2); print(3) }\nprint(f());";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert_eq!(
            warnings.len(),
            2,
            "both print(2) and the tail print(3) after the return should be flagged: {:?}",
            resolver.diagnostics()
        );
        assert!(warnings.iter().all(|w| w.message.contains("unreachable")));
    }

    #[test]
    fn code_before_return_is_not_flagged() {
        let src = "fn f() { print(1); return 2; }\nprint(f());";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        assert!(
            resolver.diagnostics().is_empty(),
            "diags: {:?}",
            resolver.diagnostics()
        );
    }

    #[test]
    fn break_marks_following_code_in_the_same_block_unreachable() {
        let src = "loop { break; print(1); }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let warnings: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "diags: {:?}", resolver.diagnostics());
        assert!(warnings[0].message.contains("unreachable"));
    }

    #[test]
    fn adt_variant_constructor_names_resolve_as_globals() {
        let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nlet c = Circle(3.0);\nlet p = Point;\nprint(c);\nprint(p);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let mut resolver = Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let errors: Vec<_> = resolver
            .diagnostics()
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "diags: {:?}", resolver.diagnostics());
    }

    #[test]
    fn resolve_entry_point_ties_everything_together() {
        let src = "fn add(a, b) { print(a); b }\nprint(add(1, 2));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "diags: {parse_diags:?}");
        let (bindings, diags) = resolve(&ast, &mut interner, &stmts);
        assert!(diags.is_empty(), "diags: {diags:?}");
        assert!(!bindings.resolutions.is_empty());
    }
}
