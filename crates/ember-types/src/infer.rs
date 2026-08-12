use crate::adt::AdtRegistry;
use crate::constraint::Origin;
use crate::env::TyEnv;
use crate::subst::Subst;
use crate::trace::InferenceTrace;
use crate::ty::{Scheme, Ty};
use ember_ast::{Ast, Expr, Idx, Interner, Stmt, Symbol};
use ember_diag::Diagnostic;
use ember_lexer::TokenKind;
use ember_span::Span;
use rustc_hash::FxHashMap;

type NativeGlobalCtor = fn(&mut Subst) -> Ty;

const NATIVE_GLOBALS: &[(&str, NativeGlobalCtor)] = &[
    ("print", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Unit))),
    ("len", |s| {
        Ty::Fun(vec![Ty::List(Box::new(s.fresh()))], Box::new(Ty::Int))
    }),
    ("push", |s| {
        let elem = s.fresh();
        Ty::Fun(
            vec![Ty::List(Box::new(elem.clone())), elem],
            Box::new(Ty::Unit),
        )
    }),
    ("clock", |_| Ty::Fun(vec![], Box::new(Ty::Float))),
    ("str", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::String))),
    ("int", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Int))),
    ("float", |s| Ty::Fun(vec![s.fresh()], Box::new(Ty::Float))),
    ("type_of", |s| {
        Ty::Fun(vec![s.fresh()], Box::new(Ty::String))
    }),
];

/// A deferred field-access obligation collected during the walk. `base`'s
/// type may still be an open variable at the point a `.field` expression is
/// visited (e.g. an un-annotated function parameter whose struct type is
/// only implied by how it's used elsewhere), so field access can't resolve
/// eagerly — every obligation gets replayed by `resolve_field_obligations`
/// once the whole program has been walked and `base_ty` has had every
/// chance to get pinned down.
struct FieldObligation {
    base_ty: Ty,
    field: Symbol,
    result_ty: Ty,
    span: Span,
}

pub struct Infer<'a> {
    ast: &'a Ast,
    interner: &'a mut Interner,
    pub subst: Subst,
    env: TyEnv,
    adts: AdtRegistry,
    pub diagnostics: Vec<Diagnostic>,
    pub trace: InferenceTrace,
    pub expr_types: FxHashMap<Idx<Expr>, Ty>,
    pub fn_schemes: FxHashMap<Symbol, Scheme>,
    field_obligations: Vec<FieldObligation>,
}

impl<'a> Infer<'a> {
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        let mut subst = Subst::new();
        let mut env = TyEnv::new();
        for (name, make_ty) in NATIVE_GLOBALS {
            let sym = interner.intern(name);
            let ty = make_ty(&mut subst);
            // Every var `make_ty` just created via `s.fresh()` is free at
            // this point (nothing else references it yet), so all of them
            // are safe to quantify — this is what makes `print`/`push`/etc.
            // usable at different types across different call sites,
            // rather than sharing one global type variable for the whole
            // program's lifetime.
            let mut vars = rustc_hash::FxHashSet::default();
            collect_ty_vars(&ty, &mut vars);
            env.declare(
                sym,
                Scheme {
                    vars: vars.into_iter().collect(),
                    ty,
                },
            );
        }
        Infer {
            ast,
            interner,
            subst,
            env,
            adts: AdtRegistry::new(),
            diagnostics: Vec::new(),
            trace: InferenceTrace::default(),
            expr_types: FxHashMap::default(),
            fn_schemes: FxHashMap::default(),
            field_obligations: Vec::new(),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty, origin: Origin) -> bool {
        crate::unify::unify(
            a,
            b,
            origin,
            &mut self.subst,
            &self.adts,
            self.interner,
            &mut self.trace,
            &mut self.diagnostics,
        )
    }

    fn display(&mut self, ty: &Ty) -> String {
        crate::display::display_ty(ty, &mut self.subst, &self.adts, self.interner)
    }

    pub fn resolve_program(&mut self, stmts: &[Idx<Stmt>]) {
        self.register_adts(stmts);

        // Pass 1: bind every top-level fn to a monomorphic function type so
        // mutual recursion between top-level functions type-checks.
        let mut fn_stmts = Vec::new();
        for &s in stmts {
            if let Stmt::Fn {
                name,
                params,
                ret_ty,
                ..
            } = self.ast.stmt(s).clone()
            {
                let param_types: Vec<Ty> = params
                    .iter()
                    .map(|p| match p.ty {
                        Some(annot) => self.type_expr_to_ty(annot),
                        None => self.subst.fresh(),
                    })
                    .collect();
                let ret = match ret_ty {
                    Some(annot) => self.type_expr_to_ty(annot),
                    None => self.subst.fresh(),
                };
                let fn_ty = Ty::Fun(param_types, Box::new(ret));
                self.env.declare(
                    name,
                    Scheme {
                        vars: vec![],
                        ty: fn_ty,
                    },
                );
                fn_stmts.push(s);
            }
        }

        // Pass 2: infer each function's body against its own (and its
        // siblings') monomorphic binding, then generalize and rebind.
        for &s in &fn_stmts {
            self.infer_fn_decl(s, true);
        }

        // Pass 3: everything else (let, expression statements, decls).
        for &s in stmts {
            if !matches!(self.ast.stmt(s), Stmt::Fn { .. }) {
                self.infer_stmt(s);
            }
        }

        self.resolve_field_obligations();
    }

    /// Populates `self.adts` from every top-level `type`/`struct` decl, and
    /// declares each enum variant's constructor in `self.env` — a payload-ful
    /// variant (`Circle(Float)`) as a function `Float -> Shape`, a nullary
    /// one (`Point`) as a plain value of type `Shape`. Run before the
    /// two-pass fn walk below: a fn body or any other top-level statement may
    /// reference a type or constructor declared anywhere else in the file.
    fn register_adts(&mut self, stmts: &[Idx<Stmt>]) {
        for &s in stmts {
            match self.ast.stmt(s).clone() {
                Stmt::TypeDecl { name, variants } => {
                    let resolved_variants: Vec<(Symbol, Vec<Ty>)> = variants
                        .iter()
                        .map(|v| {
                            let payload =
                                v.payload.iter().map(|&t| self.type_expr_to_ty(t)).collect();
                            (v.name, payload)
                        })
                        .collect();
                    let id = self.adts.register_enum(name, resolved_variants.clone());
                    for (variant_name, payload) in resolved_variants {
                        let scheme = if payload.is_empty() {
                            Scheme {
                                vars: vec![],
                                ty: Ty::Adt(id, vec![]),
                            }
                        } else {
                            Scheme {
                                vars: vec![],
                                ty: Ty::Fun(payload, Box::new(Ty::Adt(id, vec![]))),
                            }
                        };
                        self.env.declare(variant_name, scheme);
                    }
                }
                Stmt::StructDecl { name, fields } => {
                    let resolved_fields: Vec<(Symbol, Ty)> = fields
                        .iter()
                        .map(|f| (f.name, self.type_expr_to_ty(f.ty)))
                        .collect();
                    self.adts.register_struct(name, resolved_fields);
                }
                _ => {}
            }
        }
    }

    /// Infers one `fn`'s body against its already-bound monomorphic type
    /// (bound either by `resolve_program`'s pass 1, for top-level fns, or by
    /// this function itself for a nested one), unifies the body with the
    /// return type, then — only when `top_level` — generalizes the solved
    /// type and rebinds it polymorphically. A nested `fn`'s type stays
    /// monomorphic at its own use sites, matching "generalize at let and
    /// top-level fn only."
    fn infer_fn_decl(&mut self, idx: Idx<Stmt>, top_level: bool) {
        let Stmt::Fn {
            name,
            params,
            ret_ty,
            body,
        } = self.ast.stmt(idx).clone()
        else {
            unreachable!("infer_fn_decl called on a non-Fn statement");
        };

        // A nested fn isn't pre-declared by any hoisting pass, so bind its own
        // monomorphic type here before inferring its body (so it can recurse).
        let existing = self.env.lookup(name).cloned();
        let fn_scheme = existing.unwrap_or_else(|| {
            let param_types: Vec<Ty> = params
                .iter()
                .map(|p| match p.ty {
                    Some(annot) => self.type_expr_to_ty(annot),
                    None => self.subst.fresh(),
                })
                .collect();
            let ret = match ret_ty {
                Some(annot) => self.type_expr_to_ty(annot),
                None => self.subst.fresh(),
            };
            let scheme = Scheme {
                vars: vec![],
                ty: Ty::Fun(param_types, Box::new(ret)),
            };
            self.env.declare(name, scheme.clone());
            scheme
        });

        let Ty::Fun(param_types, ret_ty_boxed) = fn_scheme.ty.clone() else {
            unreachable!("a fn's own bound type must be Ty::Fun")
        };

        self.env.push_scope();
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.env.declare(
                p.name,
                Scheme {
                    vars: vec![],
                    ty: ty.clone(),
                },
            );
        }
        let body_ty = self.infer_expr(body);
        let fn_span = self.ast.span_of_stmt(idx);
        let body_span = self.ast.span_of_expr(body);
        self.unify(
            &body_ty,
            &ret_ty_boxed,
            Origin::Return {
                fn_span,
                expr_span: body_span,
            },
        );
        self.env.pop_scope();

        if top_level {
            let final_ty = self.subst.resolve(&Ty::Fun(param_types, ret_ty_boxed));
            // `name`'s own pre-generalization binding (from pass 1, or from
            // the `unwrap_or_else` above) is still sitting in the enclosing
            // scope, monomorphic (`vars: []`), and `generalize` treats every
            // env-resident scheme's unquantified vars as "still owned by an
            // enclosing binding" — so without neutralizing it first, a
            // self-recursive fn's own param/return vars would always appear
            // "already owned" (by itself) and it could never generalize.
            self.env.declare(
                name,
                Scheme {
                    vars: vec![],
                    ty: Ty::Unit,
                },
            );
            let scheme = self.generalize(&final_ty);
            self.env.declare(name, scheme.clone());
            self.fn_schemes.insert(name, scheme);
        }
    }

    fn infer_stmt(&mut self, idx: Idx<Stmt>) {
        match self.ast.stmt(idx).clone() {
            Stmt::ExprStmt(e) => {
                self.infer_expr(e);
            }
            Stmt::Let {
                name,
                mutable,
                ty: annot,
                init,
            } => {
                let init_ty = self.infer_expr(init);
                if let Some(annot_idx) = annot {
                    let annot_ty = self.type_expr_to_ty(annot_idx);
                    let origin = Origin::Annotation {
                        annot_span: self.ast.span_of_type_expr(annot_idx),
                        value_span: self.ast.span_of_expr(init),
                    };
                    self.unify(&annot_ty, &init_ty, origin);
                }
                let scheme = if self.should_generalize(mutable, init) {
                    self.generalize(&init_ty)
                } else {
                    Scheme {
                        vars: vec![],
                        ty: init_ty,
                    }
                };
                self.env.declare(name, scheme);
            }
            Stmt::Fn { .. } => self.infer_fn_decl(idx, false),
            Stmt::While { cond, body } => {
                let cond_ty = self.infer_expr(cond);
                let span = self.ast.span_of_expr(cond);
                self.unify(&cond_ty, &Ty::Bool, Origin::WhileCond { span });
                self.infer_expr(body);
            }
            Stmt::For {
                binding,
                iter,
                body,
            } => {
                let iter_ty = self.infer_expr(iter);
                let elem_ty = self.subst.fresh();
                let span = self.ast.span_of_expr(iter);
                self.unify(
                    &iter_ty,
                    &Ty::List(Box::new(elem_ty.clone())),
                    Origin::IndexTarget { span },
                );
                self.env.push_scope();
                self.env.declare(
                    binding,
                    Scheme {
                        vars: vec![],
                        ty: elem_ty,
                    },
                );
                self.infer_expr(body);
                self.env.pop_scope();
            }
            Stmt::Loop { body } => {
                self.infer_expr(body);
            }
            // The return TYPE itself is unified against the enclosing fn's
            // declared return type in `infer_fn_decl` via the function's tail
            // expression/body type, not per-`return`-statement here — a narrower
            // scope than full branch-level dataflow typing, matching how Phase 4
            // similarly scoped its unreachable-code analysis to direct-successor
            // only. A mismatched early `return`'s value is still visited and
            // typed above so its OWN subexpressions get checked.
            Stmt::Return(Some(v)) => {
                self.infer_expr(v);
            }
            Stmt::Return(None) => {}
            Stmt::Break | Stmt::Continue => {}
            _ => {
                // Every other Stmt form is added in later tasks (decls in
                // Task 14). Left unhandled here deliberately so the match
                // stays a visible TODO surface rather than a silent no-op —
                // revisit if this arm still exists after Task 14.
            }
        }
    }

    fn infer_expr(&mut self, idx: Idx<Expr>) -> Ty {
        let ty = match self.ast.expr(idx).clone() {
            Expr::Int(_) => Ty::Int,
            Expr::Float(_) => Ty::Float,
            Expr::Str(_) => Ty::String,
            Expr::Bool(_) => Ty::Bool,
            Expr::Nil => Ty::Unit,
            Expr::Var(sym) => self.infer_var(sym, idx),
            Expr::Unary { op, operand } => self.infer_unary(op, operand, idx),
            Expr::Binary { op, lhs, rhs } => self.infer_binary(op, lhs, rhs, idx),
            Expr::Call { callee, args } => self.infer_call(callee, &args, idx),
            Expr::Lambda { params, body } => self.infer_lambda(&params, body),
            Expr::If { cond, then_, else_ } => self.infer_if(cond, then_, else_, idx),
            Expr::Block { stmts, tail } => self.infer_block(&stmts, tail),
            Expr::List { items } => self.infer_list(&items, idx),
            Expr::Index { base, index } => self.infer_index(base, index, idx),
            Expr::Assign { target, value } => self.infer_assign(target, value, idx),
            Expr::Struct { name, fields } => self.infer_struct_literal(name, &fields, idx),
            Expr::Field { base, name } => self.infer_field(base, name, idx),
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, &arms),
            _ => self.subst.fresh(), // remaining forms land in Tasks 14-18
        };
        self.expr_types.insert(idx, ty.clone());
        ty
    }

    fn infer_var(&mut self, sym: Symbol, idx: Idx<Expr>) -> Ty {
        match self.env.lookup(sym).cloned() {
            Some(scheme) => self.instantiate(&scheme),
            None => {
                let span = self.ast.span_of_expr(idx);
                let name = self.interner.resolve(sym).to_string();
                self.diagnostics.push(
                    Diagnostic::error(format!("undeclared name `{name}`"))
                        .with_code("E0401")
                        .with_primary(span, "not found in this scope"),
                );
                self.subst.fresh()
            }
        }
    }

    /// Replaces each of a scheme's quantified variables with a fresh one —
    /// why `identity(1)` and `identity("x")` can coexist: `identity` is
    /// stored as `∀a. a -> a`; the first call instantiates `a := t1` and
    /// unifies `t1 = Int`, the second instantiates `a := t2` and unifies
    /// `t2 = String`. No conflict, because they're different variables.
    pub(crate) fn instantiate(&mut self, scheme: &Scheme) -> Ty {
        let sub: FxHashMap<crate::ty::TyVarId, Ty> = scheme
            .vars
            .iter()
            .map(|&v| (v, self.subst.fresh()))
            .collect();
        substitute(&scheme.ty, &sub)
    }

    fn infer_unary(&mut self, op: TokenKind, operand: Idx<Expr>, idx: Idx<Expr>) -> Ty {
        let operand_ty = self.infer_expr(operand);
        let span = self.ast.span_of_expr(idx);
        match op {
            TokenKind::Bang => {
                self.unify(&operand_ty, &Ty::Bool, Origin::WhileCond { span });
                Ty::Bool
            }
            // Minus: permissive (see the numeric-operator scope note above)
            // — the operand's own type is also the result's type.
            _ => operand_ty,
        }
    }

    fn infer_binary(
        &mut self,
        op: TokenKind,
        lhs: Idx<Expr>,
        rhs: Idx<Expr>,
        idx: Idx<Expr>,
    ) -> Ty {
        let lhs_ty = self.infer_expr(lhs);
        let rhs_ty = self.infer_expr(rhs);
        let op_span = self.ast.span_of_expr(idx);
        let lhs_span = self.ast.span_of_expr(lhs);
        let rhs_span = self.ast.span_of_expr(rhs);
        let origin = Origin::BinaryOp {
            op_span,
            lhs_span,
            rhs_span,
        };
        match op {
            TokenKind::AndAnd | TokenKind::OrOr => {
                self.unify(&lhs_ty, &Ty::Bool, origin.clone());
                self.unify(&rhs_ty, &Ty::Bool, origin);
                Ty::Bool
            }
            TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::LtEq
            | TokenKind::Gt
            | TokenKind::GtEq => {
                self.unify(&lhs_ty, &rhs_ty, origin);
                Ty::Bool
            }
            // Arithmetic: permissive, see the numeric-operator scope note.
            _ => {
                self.unify(&lhs_ty, &rhs_ty, origin);
                lhs_ty
            }
        }
    }

    fn infer_call(&mut self, callee: Idx<Expr>, args: &[Idx<Expr>], idx: Idx<Expr>) -> Ty {
        let call_span = self.ast.span_of_expr(idx);
        let callee_ty = self.infer_expr(callee);
        let resolved_callee = self.subst.resolve(&callee_ty);
        let fn_name = match self.ast.expr(callee) {
            Expr::Var(s) => Some(*s),
            _ => None,
        };
        match resolved_callee {
            Ty::Fun(params, ret) => {
                if params.len() != args.len() {
                    let expected = params.len();
                    let found = args.len();
                    self.diagnostics.push(
                        Diagnostic::error(format!(
                            "expected {expected} argument(s), found {found}"
                        ))
                        .with_code("E0402")
                        .with_primary(call_span, format!("expected {expected}, found {found}")),
                    );
                    for &a in args {
                        self.infer_expr(a);
                    }
                    self.subst.fresh()
                } else {
                    for (i, (&a, p)) in args.iter().zip(params.iter()).enumerate() {
                        let arg_ty = self.infer_expr(a);
                        let origin = Origin::CallArgument {
                            call_span,
                            arg_span: self.ast.span_of_expr(a),
                            param_idx: i,
                            fn_name,
                        };
                        self.unify(&arg_ty, p, origin);
                    }
                    *ret
                }
            }
            Ty::Var(_) => {
                let arg_types: Vec<Ty> = args.iter().map(|&a| self.infer_expr(a)).collect();
                let ret_ty = self.subst.fresh();
                let expected = Ty::Fun(arg_types, Box::new(ret_ty.clone()));
                self.unify(
                    &resolved_callee,
                    &expected,
                    Origin::CallArgument {
                        call_span,
                        arg_span: call_span,
                        param_idx: 0,
                        fn_name,
                    },
                );
                ret_ty
            }
            other => {
                let other_str = self.display(&other);
                self.diagnostics.push(
                    Diagnostic::error(format!("`{other_str}` is not callable"))
                        .with_code("E0403")
                        .with_primary(call_span, "attempted call here"),
                );
                for &a in args {
                    self.infer_expr(a);
                }
                self.subst.fresh()
            }
        }
    }

    /// THE VALUE RESTRICTION. Generalizing a mutable binding is unsound: `let
    /// mut r = [];` would generalize to `∀a. [a]`, letting a later `push`
    /// pin one element type while a later read expects another. Only
    /// syntactic values may generalize — literals, lambdas, variables. Never
    /// mutable bindings, never the result of a general application (a plain
    /// function call).
    fn should_generalize(&self, is_mut: bool, init: Idx<Expr>) -> bool {
        !is_mut && self.is_syntactic_value(init)
    }

    fn is_syntactic_value(&self, e: Idx<Expr>) -> bool {
        matches!(
            self.ast.expr(e),
            Expr::Int(_)
                | Expr::Float(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Nil
                | Expr::Lambda { .. }
                | Expr::Var(_)
        )
    }

    /// Generalize ONLY at `let` and top-level `fn` (a later task reuses this
    /// for the latter). Quantifies every free type variable that is NOT free
    /// in the surrounding environment — a variable still referenced by an
    /// enclosing binding is not this binding's to quantify.
    fn generalize(&mut self, ty: &Ty) -> Scheme {
        let resolved = self.subst.resolve(ty);
        let mut env_free = self.env.free_vars();
        // A variable that's still the subject of a pending field obligation
        // isn't this binding's to quantify either, exactly like a variable
        // still free in the environment: `resolve_field_obligations` needs
        // to unify against it later using its ORIGINAL identity, and
        // `instantiate` mints an unrelated fresh copy at every use site of a
        // generalized scheme — so generalizing it now would sever the
        // obligation from whatever eventually pins the real type down (e.g.
        // a later call site's argument).
        for ob in &self.field_obligations {
            collect_ty_vars(&ob.base_ty, &mut env_free);
            collect_ty_vars(&ob.result_ty, &mut env_free);
        }
        let mut ty_free = rustc_hash::FxHashSet::default();
        collect_ty_vars(&resolved, &mut ty_free);
        let vars: Vec<crate::ty::TyVarId> = ty_free.difference(&env_free).copied().collect();
        Scheme { vars, ty: resolved }
    }

    fn type_expr_to_ty(&mut self, idx: Idx<ember_ast::TypeExpr>) -> Ty {
        match self.ast.type_expr(idx).clone() {
            ember_ast::TypeExpr::Name(sym) => {
                let name = self.interner.resolve(sym);
                match name {
                    "Int" => Ty::Int,
                    "Float" => Ty::Float,
                    "Bool" => Ty::Bool,
                    "String" => Ty::String,
                    "Unit" => Ty::Unit,
                    _ => match self.adts.id_of(sym) {
                        Some(id) => Ty::Adt(id, vec![]),
                        None => {
                            let span = self.ast.span_of_type_expr(idx);
                            self.diagnostics.push(
                                Diagnostic::error(format!("unknown type `{name}`"))
                                    .with_code("E0404")
                                    .with_primary(span, "not found"),
                            );
                            self.subst.fresh()
                        }
                    },
                }
            }
            ember_ast::TypeExpr::List(inner) => Ty::List(Box::new(self.type_expr_to_ty(inner))),
            ember_ast::TypeExpr::Generic { name, args } => {
                let text = self.interner.resolve(name).to_string();
                if text == "List" && args.len() == 1 {
                    Ty::List(Box::new(self.type_expr_to_ty(args[0])))
                } else {
                    let span = self.ast.span_of_type_expr(idx);
                    self.diagnostics.push(
                        Diagnostic::error(format!("type `{text}` does not take type arguments"))
                            .with_code("E0405")
                            .with_primary(
                                span,
                                "user-declared types have no generic parameters this phase",
                            ),
                    );
                    self.subst.fresh()
                }
            }
            ember_ast::TypeExpr::Fun { params, ret } => {
                let param_tys = params.iter().map(|p| self.type_expr_to_ty(*p)).collect();
                Ty::Fun(param_tys, Box::new(self.type_expr_to_ty(ret)))
            }
            ember_ast::TypeExpr::Error => self.subst.fresh(),
        }
    }

    fn infer_if(
        &mut self,
        cond: Idx<Expr>,
        then_: Idx<Expr>,
        else_: Option<Idx<Expr>>,
        idx: Idx<Expr>,
    ) -> Ty {
        let cond_ty = self.infer_expr(cond);
        let cond_span = self.ast.span_of_expr(cond);
        self.unify(&cond_ty, &Ty::Bool, Origin::WhileCond { span: cond_span });
        let then_ty = self.infer_expr(then_);
        match else_ {
            Some(e) => {
                let else_ty = self.infer_expr(e);
                let if_span = self.ast.span_of_expr(idx);
                let then_span = self.ast.span_of_expr(then_);
                let else_span = self.ast.span_of_expr(e);
                self.unify(
                    &then_ty,
                    &else_ty,
                    Origin::IfBranches {
                        if_span,
                        then_span,
                        else_span,
                    },
                );
                then_ty
            }
            None => Ty::Unit,
        }
    }

    fn infer_block(&mut self, stmts: &[Idx<Stmt>], tail: Option<Idx<Expr>>) -> Ty {
        self.env.push_scope();
        for &s in stmts {
            self.infer_stmt(s);
        }
        let ty = match tail {
            Some(t) => self.infer_expr(t),
            None => Ty::Unit,
        };
        self.env.pop_scope();
        ty
    }

    fn infer_list(&mut self, items: &[Idx<Expr>], idx: Idx<Expr>) -> Ty {
        let elem_ty = self.subst.fresh();
        let list_span = self.ast.span_of_expr(idx);
        for (i, &item) in items.iter().enumerate() {
            let item_ty = self.infer_expr(item);
            let elem_span = self.ast.span_of_expr(item);
            self.unify(
                &elem_ty,
                &item_ty,
                Origin::ListElement {
                    list_span,
                    elem_span,
                    index: i,
                },
            );
        }
        Ty::List(Box::new(elem_ty))
    }

    fn infer_index(&mut self, base: Idx<Expr>, index: Idx<Expr>, idx: Idx<Expr>) -> Ty {
        let base_ty = self.infer_expr(base);
        let index_ty = self.infer_expr(index);
        let span = self.ast.span_of_expr(idx);
        self.unify(&index_ty, &Ty::Int, Origin::IndexTarget { span });
        let elem_ty = self.subst.fresh();
        self.unify(
            &base_ty,
            &Ty::List(Box::new(elem_ty.clone())),
            Origin::IndexTarget { span },
        );
        elem_ty
    }

    fn infer_assign(&mut self, target: Idx<Expr>, value: Idx<Expr>, idx: Idx<Expr>) -> Ty {
        let target_ty = self.infer_expr(target);
        let value_ty = self.infer_expr(value);
        let span = self.ast.span_of_expr(idx);
        self.unify(&target_ty, &value_ty, Origin::IndexTarget { span });
        Ty::Unit
    }

    fn infer_struct_literal(
        &mut self,
        name: Symbol,
        fields: &[(Symbol, Idx<Expr>)],
        idx: Idx<Expr>,
    ) -> Ty {
        let span = self.ast.span_of_expr(idx);
        let Some(id) = self.adts.id_of(name) else {
            let name_str = self.interner.resolve(name).to_string();
            self.diagnostics.push(
                Diagnostic::error(format!("unknown type `{name_str}`"))
                    .with_code("E0404")
                    .with_primary(span, "not found"),
            );
            for &(_, v) in fields {
                self.infer_expr(v);
            }
            return self.subst.fresh();
        };
        if !self.adts.is_struct(id) {
            let name_str = self.interner.resolve(name).to_string();
            self.diagnostics.push(
                Diagnostic::error(format!("`{name_str}` is not a struct"))
                    .with_code("E0406")
                    .with_primary(span, "struct literal syntax used here"),
            );
            for &(_, v) in fields {
                self.infer_expr(v);
            }
            return self.subst.fresh();
        }

        let declared_fields: Vec<Symbol> = self.adts.struct_fields(id).collect();
        let mut provided = rustc_hash::FxHashSet::default();
        for &(field_name, value) in fields {
            let value_ty = self.infer_expr(value);
            provided.insert(field_name);
            match self.adts.field_ty(id, field_name).cloned() {
                Some(declared_ty) => {
                    let value_span = self.ast.span_of_expr(value);
                    self.unify(
                        &declared_ty,
                        &value_ty,
                        Origin::Annotation {
                            annot_span: span,
                            value_span,
                        },
                    );
                }
                None => {
                    let field_str = self.interner.resolve(field_name).to_string();
                    let name_str = self.interner.resolve(name).to_string();
                    self.diagnostics.push(
                        Diagnostic::error(format!("no field `{field_str}` on struct `{name_str}`"))
                            .with_code("E0407")
                            .with_primary(span, "unknown field in this struct literal"),
                    );
                }
            }
        }
        for declared in declared_fields {
            if !provided.contains(&declared) {
                let field_str = self.interner.resolve(declared).to_string();
                let name_str = self.interner.resolve(name).to_string();
                self.diagnostics.push(
                    Diagnostic::error(format!(
                        "missing field `{field_str}` in struct `{name_str}`"
                    ))
                    .with_code("E0408")
                    .with_primary(span, format!("`{name_str}` requires a `{field_str}` field")),
                );
            }
        }
        Ty::Adt(id, vec![])
    }

    fn infer_field(&mut self, base: Idx<Expr>, name: Symbol, idx: Idx<Expr>) -> Ty {
        let base_ty = self.infer_expr(base);
        let result_ty = self.subst.fresh();
        let span = self.ast.span_of_expr(idx);
        self.field_obligations.push(FieldObligation {
            base_ty,
            field: name,
            result_ty: result_ty.clone(),
            span,
        });
        result_ty
    }

    /// Resolves every deferred field-access obligation against the FINAL
    /// substitution, once the whole program has been walked. Field access
    /// can't resolve at generation time in general — `base`'s type may still
    /// be an open variable then (e.g. an un-annotated function parameter whose
    /// struct type is only implied by how it's used) — so it's deferred to
    /// here, the one place after eager unification has had every chance to
    /// pin `base`'s type down.
    fn resolve_field_obligations(&mut self) {
        let obligations = std::mem::take(&mut self.field_obligations);
        for ob in obligations {
            let resolved_base = self.subst.resolve(&ob.base_ty);
            match resolved_base {
                Ty::Adt(id, _) if self.adts.is_struct(id) => {
                    match self.adts.field_ty(id, ob.field).cloned() {
                        Some(field_ty) => {
                            self.unify(
                                &ob.result_ty,
                                &field_ty,
                                Origin::IndexTarget { span: ob.span },
                            );
                        }
                        None => {
                            let field_str = self.interner.resolve(ob.field).to_string();
                            let type_str = self.display(&resolved_base);
                            self.diagnostics.push(
                                Diagnostic::error(format!(
                                    "no field `{field_str}` on type `{type_str}`"
                                ))
                                .with_code("E0407")
                                .with_primary(ob.span, "unknown field"),
                            );
                        }
                    }
                }
                Ty::Var(_) => {
                    self.diagnostics.push(
                        Diagnostic::error("cannot infer the type of this field access")
                            .with_code("E0409")
                            .with_primary(ob.span, "add a type annotation to resolve this")
                            .with_help(
                                "field access needs a known struct type; ember doesn't infer field types structurally across unrelated declarations",
                            ),
                    );
                }
                other => {
                    let type_str = self.display(&other);
                    self.diagnostics.push(
                        Diagnostic::error(format!("type `{type_str}` has no fields"))
                            .with_code("E0410")
                            .with_primary(ob.span, "attempted field access here"),
                    );
                }
            }
        }
    }

    fn infer_match(&mut self, scrutinee: Idx<Expr>, arms: &[ember_ast::MatchArm]) -> Ty {
        let scrutinee_ty = self.infer_expr(scrutinee);
        let result_ty = self.subst.fresh();
        let mut first_arm_span: Option<Span> = None;
        for arm in arms {
            self.env.push_scope();
            self.infer_pattern(arm.pat, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                let guard_ty = self.infer_expr(guard);
                let guard_span = self.ast.span_of_expr(guard);
                self.unify(&guard_ty, &Ty::Bool, Origin::WhileCond { span: guard_span });
            }
            let body_ty = self.infer_expr(arm.body);
            match first_arm_span {
                None => {
                    first_arm_span = Some(arm.span);
                    self.unify(
                        &result_ty,
                        &body_ty,
                        Origin::MatchArms {
                            first_span: arm.span,
                            this_span: arm.span,
                        },
                    );
                }
                Some(first_span) => {
                    self.unify(
                        &result_ty,
                        &body_ty,
                        Origin::MatchArms {
                            first_span,
                            this_span: arm.span,
                        },
                    );
                }
            }
            self.env.pop_scope();
        }
        result_ty
    }

    /// Constrains `scrutinee_ty` and declares every name this pattern binds,
    /// into the CURRENT (already-pushed) scope. Exhaustiveness is explicitly
    /// a separate future phase's job — this only types.
    fn infer_pattern(&mut self, pat: Idx<ember_ast::Pattern>, scrutinee_ty: &Ty) {
        use ember_ast::Pattern;
        let span = self.ast.span_of_pat(pat);
        match self.ast.pat(pat).clone() {
            Pattern::Wild | Pattern::Error => {}
            Pattern::Int(_) => {
                self.unify(scrutinee_ty, &Ty::Int, Origin::IndexTarget { span });
            }
            Pattern::Float(_) => {
                self.unify(scrutinee_ty, &Ty::Float, Origin::IndexTarget { span });
            }
            Pattern::Str(_) => {
                self.unify(scrutinee_ty, &Ty::String, Origin::IndexTarget { span });
            }
            Pattern::Bool(_) => {
                self.unify(scrutinee_ty, &Ty::Bool, Origin::IndexTarget { span });
            }
            Pattern::Bind(sym) => {
                self.env.declare(
                    sym,
                    Scheme {
                        vars: vec![],
                        ty: scrutinee_ty.clone(),
                    },
                );
            }
            Pattern::Ctor { name, args } => {
                // `pattern_primary` in the parser only ever produces `Ctor`
                // when the identifier is followed by `(...)` — a bare
                // identifier with no parens always parses as `Pattern::Bind`
                // instead. `Ctor`'s `args` can still legitimately be empty
                // here (`Circle()` written explicitly), so the arity check
                // below handles that correctly regardless.
                match self.adts.variant(name) {
                    Some((id, payload)) => {
                        let payload = payload.to_vec();
                        self.unify(
                            scrutinee_ty,
                            &Ty::Adt(id, vec![]),
                            Origin::IndexTarget { span },
                        );
                        if payload.len() != args.len() {
                            let name_str = self.interner.resolve(name).to_string();
                            self.diagnostics.push(
                                Diagnostic::error(format!(
                                    "`{name_str}` takes {} argument(s), found {}",
                                    payload.len(),
                                    args.len()
                                ))
                                .with_code("E0411")
                                .with_primary(span, "pattern here"),
                            );
                        } else {
                            for (&arg_pat, arg_ty) in args.iter().zip(payload.iter()) {
                                self.infer_pattern(arg_pat, arg_ty);
                            }
                        }
                    }
                    None => {
                        let name_str = self.interner.resolve(name).to_string();
                        self.diagnostics.push(
                            Diagnostic::error(format!("unknown constructor `{name_str}`"))
                                .with_code("E0412")
                                .with_primary(span, "not found"),
                        );
                    }
                }
            }
            Pattern::Record { name, fields } => match self.adts.id_of(name) {
                Some(id) if self.adts.is_struct(id) => {
                    self.unify(
                        scrutinee_ty,
                        &Ty::Adt(id, vec![]),
                        Origin::IndexTarget { span },
                    );
                    for (field_name, field_pat) in fields {
                        match self.adts.field_ty(id, field_name).cloned() {
                            Some(field_ty) => self.infer_pattern(field_pat, &field_ty),
                            None => {
                                let field_str = self.interner.resolve(field_name).to_string();
                                self.diagnostics.push(
                                    Diagnostic::error(format!(
                                        "no field `{field_str}` on this struct"
                                    ))
                                    .with_code("E0407")
                                    .with_primary(span, "in this pattern"),
                                );
                            }
                        }
                    }
                }
                _ => {
                    let name_str = self.interner.resolve(name).to_string();
                    self.diagnostics.push(
                        Diagnostic::error(format!("`{name_str}` is not a struct"))
                            .with_code("E0406")
                            .with_primary(span, "record pattern used here"),
                    );
                }
            },
            // No Ty::Tuple and no Expr::Tuple exist in this grammar — a
            // pre-existing gap. Each binding gets a fresh, unconstrained type;
            // the pattern is inert by construction since nothing can ever
            // produce a matching value.
            Pattern::Tuple(items) => {
                for item in items {
                    let fresh = self.subst.fresh();
                    self.infer_pattern(item, &fresh);
                }
            }
            Pattern::List { items, rest } => {
                let elem_ty = self.subst.fresh();
                self.unify(
                    scrutinee_ty,
                    &Ty::List(Box::new(elem_ty.clone())),
                    Origin::IndexTarget { span },
                );
                for item in items {
                    self.infer_pattern(item, &elem_ty);
                }
                if let Some(rest_pat) = rest {
                    self.infer_pattern(rest_pat, &Ty::List(Box::new(elem_ty)));
                }
            }
            Pattern::Or(alts) => {
                // Each alternative types against the same scrutinee. Bindings
                // sharing a name across alternatives follow last-write-wins
                // into this arm's single scope — the same simplification the
                // resolver already applies to or-patterns, kept consistent
                // rather than adding a stricter cross-check only here.
                for alt in alts {
                    self.infer_pattern(alt, scrutinee_ty);
                }
            }
        }
    }

    fn infer_lambda(&mut self, params: &[ember_ast::Param], body: Idx<Expr>) -> Ty {
        self.env.push_scope();
        let param_types: Vec<Ty> = params
            .iter()
            .map(|p| {
                let ty = match p.ty {
                    Some(annot_idx) => self.type_expr_to_ty(annot_idx),
                    None => self.subst.fresh(),
                };
                self.env.declare(
                    p.name,
                    Scheme {
                        vars: vec![],
                        ty: ty.clone(),
                    },
                );
                ty
            })
            .collect();
        let body_ty = self.infer_expr(body);
        self.env.pop_scope();
        Ty::Fun(param_types, Box::new(body_ty))
    }
}

/// Replaces every `TyVarId` present in `sub` throughout `ty`, leaving
/// anything not in `sub` untouched. Used by `instantiate`.
pub(crate) fn substitute(ty: &Ty, sub: &FxHashMap<crate::ty::TyVarId, Ty>) -> Ty {
    match ty {
        Ty::Var(v) => sub.get(v).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Fun(params, ret) => Ty::Fun(
            params.iter().map(|p| substitute(p, sub)).collect(),
            Box::new(substitute(ret, sub)),
        ),
        Ty::List(t) => Ty::List(Box::new(substitute(t, sub))),
        Ty::Adt(id, args) => Ty::Adt(*id, args.iter().map(|a| substitute(a, sub)).collect()),
        Ty::Record(fields) => Ty::Record(
            fields
                .iter()
                .map(|(k, v)| (*k, substitute(v, sub)))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

/// Collects every `TyVarId` present in a bare `Ty`, with nothing
/// pre-quantified — distinct from `env.rs`'s `collect_free_vars`, which
/// excludes a scheme's own quantified vars. Used by `generalize` to find
/// every variable a candidate binding's type mentions, before subtracting
/// the ones still owned by the surrounding environment.
fn collect_ty_vars(ty: &Ty, out: &mut rustc_hash::FxHashSet<crate::ty::TyVarId>) {
    match ty {
        Ty::Var(v) => {
            out.insert(*v);
        }
        Ty::Fun(params, ret) => {
            for p in params {
                collect_ty_vars(p, out);
            }
            collect_ty_vars(ret, out);
        }
        Ty::List(t) => collect_ty_vars(t, out),
        Ty::Adt(_, args) => {
            for a in args {
                collect_ty_vars(a, out);
            }
        }
        Ty::Record(fields) => {
            for v in fields.values() {
                collect_ty_vars(v, out);
            }
        }
        _ => {}
    }
}

pub struct TypeInfo {
    pub expr_types: FxHashMap<Idx<Expr>, Ty>,
    pub fn_schemes: FxHashMap<Symbol, Scheme>,
    pub adts: AdtRegistry,
    pub subst: Subst,
    pub trace: InferenceTrace,
}

pub fn infer(
    ast: &Ast,
    interner: &mut Interner,
    stmts: &[Idx<Stmt>],
) -> (TypeInfo, Vec<Diagnostic>) {
    let mut checker = Infer::new(ast, interner);
    checker.resolve_program(stmts);
    let info = TypeInfo {
        expr_types: checker.expr_types,
        fn_schemes: checker.fn_schemes,
        adts: checker.adts,
        subst: checker.subst,
        trace: checker.trace,
    };
    (info, checker.diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> (crate::ty::Ty, Vec<ember_diag::Diagnostic>) {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let mut infer = Infer::new(&ast, &mut interner);
        let last_expr = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => {
                panic!("expected the last top-level statement to be an ExprStmt, got {other:?}")
            }
        };
        infer.resolve_program(&stmts);
        let ty = infer
            .subst
            .resolve(infer.expr_types.get(&last_expr).unwrap());
        (ty, infer.diagnostics)
    }

    #[test]
    fn int_literal_infers_int() {
        let (ty, diags) = run("42;");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(ty, crate::ty::Ty::Int);
    }

    #[test]
    fn native_global_print_is_seeded() {
        let (_ty, diags) = run("print(1);");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn native_globals_are_polymorphic_across_call_sites() {
        // `print`'s parameter type must generalize (∀a. a -> Unit) — if it
        // didn't, every call site in the whole program would share ONE
        // underlying type variable, and using `print` at two different
        // types in the same program would wrongly conflict.
        let src = "print(1);\nprint(\"hello\");";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn binary_arithmetic_requires_matching_operands() {
        let (_ty, diags) = run("1 + true;");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn logical_and_requires_bool_operands() {
        let (_ty, diags) = run("1 && true;");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn comparison_produces_bool() {
        let (ty, diags) = run("1 < 2;");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(ty, crate::ty::Ty::Bool);
    }

    #[test]
    fn let_binding_infers_from_its_initializer() {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse("let x = 42;\nprint(x);");
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn let_polymorphism_lets_identity_be_used_at_two_types() {
        let src = "let identity = |x| x;\nlet a = identity(1);\nlet b = identity(\"hello\");";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn occurs_check_terminates_instead_of_hanging() {
        let src = "let f = |x| f(x);";
        let result = std::thread::spawn(move || {
            let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
            assert!(parse_diags.is_empty());
            let mut infer = Infer::new(&ast, &mut interner);
            infer.resolve_program(&stmts);
            infer.diagnostics.len()
        })
        .join()
        .expect("inference panicked or hung");
        assert!(result > 0, "expected an infinite-type diagnostic");
    }

    #[test]
    fn identity_generalizes_to_a_polymorphic_scheme() {
        let src = "fn identity(x) { x }\nlet a = identity(1);\nlet b = identity(\"hello\");";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        // Interned up front (idempotent lookup — "identity" is already
        // interned by the parser) so the borrow below doesn't overlap with
        // `Infer`'s `&mut Interner` borrow, which must stay live through
        // `infer`'s last use further down.
        let identity = interner.intern("identity");
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
        let scheme = infer
            .fn_schemes
            .get(&identity)
            .expect("identity should have a recorded scheme");
        assert_eq!(
            scheme.vars.len(),
            1,
            "identity should generalize over exactly one type variable"
        );
    }

    #[test]
    fn call_with_the_wrong_number_of_arguments_errors() {
        let src = "fn add(a, b) { a }\nlet x = add(1, 2, 3);\nprint(x);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1, "{:?}", infer.diagnostics);
        assert!(
            infer.diagnostics[0].message.contains('2')
                && infer.diagnostics[0].message.contains('3')
        );
    }

    #[test]
    fn mutual_recursion_between_top_level_functions_typechecks() {
        // Deliberately no `if`/base case — control flow (`if`) isn't
        // implemented yet at this point in the plan; this test only needs to
        // exercise the two-pass hoisting itself (forward reference from
        // `is_even` to the not-yet-declared `is_odd`), not realistic
        // termination semantics.
        let src = "fn is_even(n) { is_odd(n) }\nfn is_odd(n) { is_even(n) }\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn recursive_function_self_calls_typecheck() {
        // Same reasoning as above: no `if`/base case needed to exercise
        // letrec-style monomorphic self-binding during body inference.
        let src = "fn loop_forever(n) { loop_forever(n) }\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn value_restriction_prevents_a_mutable_binding_from_generalizing() {
        // `r` is `mut`, so it must NOT generalize to `forall a. [a]` — if it
        // did, `push(r, true)` (a Bool) and `1 + r[0]` (expecting Int) would
        // each separately instantiate their own fresh `a` and both typecheck,
        // silently masking the unsoundness. Since it stays monomorphic, `r`'s
        // element type gets pinned to Bool by the push, and `1 + r[0]` then
        // conflicts. (`r[0]`/list indexing itself lands in Task 13 — this test
        // only needs `push`'s effect on `r`'s element type, via the second
        // `push` call below, not indexing.)
        let src = "let mut r = [];\npush(r, true);\npush(r, 1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(
            !infer.diagnostics.is_empty(),
            "expected a type error: mutable bindings must not generalize"
        );
    }

    #[test]
    fn if_branch_type_mismatch_labels_both_branches() {
        let src = "if true { 1 } else { \"x\" };";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
        assert!(
            infer.diagnostics[0].labels.len() >= 2,
            "expected both branches labeled: {:?}",
            infer.diagnostics[0]
        );
    }

    #[test]
    fn if_without_else_is_unit() {
        let src = "let x = if true { print(1); };";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn while_condition_must_be_bool() {
        let src = "while 1 { }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn block_tail_expression_is_the_blocks_type() {
        let src = "let x = { let y = 1; y };\nlet z = x + 1;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn list_elements_must_share_one_type() {
        let src = "[1, \"two\", 3];";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn index_into_a_list_yields_its_element_type() {
        let src = "let xs = [1, 2, 3];\nlet y = xs[0] + 1;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn assigning_a_wrong_type_to_a_mutable_binding_errors() {
        let src = "let mut x = 1;\nx = \"nope\";";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn for_loop_and_control_flow_statements_all_typecheck() {
        let src = "fn f(xs) {\n  for x in xs {\n    if x == 0 { break; }\n    if x == 1 { continue; }\n  }\n  0\n}\nprint(f([1, 2]));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn adt_variant_with_payload_types_as_a_function() {
        let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nlet c = Circle(3.0);\nprint(c);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn nullary_variant_is_a_plain_value_not_a_function() {
        let src = "type Shape = | Circle(Float) | Point;\nlet p = Point;\nprint(p);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn wrong_payload_type_to_a_constructor_errors() {
        let src = "type Shape = | Circle(Float);\nlet c = Circle(\"nope\");";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn struct_literal_with_all_fields_typechecks() {
        let src =
            "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };\nprint(p);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn struct_literal_missing_a_field_names_it() {
        let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0 };";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
        assert!(infer.diagnostics[0].message.contains('y'));
    }

    #[test]
    fn struct_literal_with_an_unknown_field_names_it() {
        let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0, z: 3.0 };";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
        assert!(infer.diagnostics[0].message.contains('z'));
    }

    #[test]
    fn struct_literal_with_a_field_type_mismatch_errors() {
        let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1, y: 2.0 };";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn field_access_on_a_known_struct_type_resolves() {
        let src = "struct Point { x: Float, y: Float }\nfn get_x(p) {\n  let q = p;\n  q.x\n}\nlet p = Point { x: 1.0, y: 2.0 };\nprint(get_x(p) + 1.0);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn field_access_on_an_unresolved_type_needs_an_annotation() {
        let src = "fn get_x(p) { p.x }\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
        assert!(
            infer.diagnostics[0]
                .message
                .to_lowercase()
                .contains("annotation")
                || infer.diagnostics[0]
                    .message
                    .to_lowercase()
                    .contains("infer")
        );
    }

    #[test]
    fn field_access_on_a_non_struct_type_errors() {
        let src = "let x = 1;\nlet y = x.foo;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn field_access_with_an_unknown_field_name_errors() {
        let src = "struct Point { x: Float }\nlet p = Point { x: 1.0 };\nlet z = p.bogus;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn match_on_an_adt_binds_variant_payloads_at_the_right_type() {
        // The last arm uses `_`, not the bare nullary variant `Point`: per the
        // grammar note above, a bare identifier always parses as
        // `Pattern::Bind`, never `Pattern::Ctor` — so `Point` here would just
        // be a fresh catch-all bind, not a genuine constructor match. Using
        // `_` keeps this test's intent (Circle/Rect payload binding) honest.
        let src = "type Shape = | Circle(Float) | Rect(Float, Float) | Point;\nfn area(s) {\n  match s {\n    Circle(r) => r * r,\n    Rect(w, h) => w * h,\n    _ => 0.0,\n  }\n}\nprint(area(Circle(2.0)));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn match_arms_must_agree_on_their_result_type() {
        let src = "type Shape = | Circle(Float) | Point;\nfn describe(s) {\n  match s {\n    Circle(r) => r,\n    _ => \"origin\",\n  }\n}\nprint(1);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert_eq!(infer.diagnostics.len(), 1);
    }

    #[test]
    fn record_pattern_destructures_struct_fields_at_the_right_types() {
        let src = "struct Point { x: Float, y: Float }\nfn get_x(p) {\n  match p {\n    Point { x, y } => x,\n  }\n}\nlet p = Point { x: 1.0, y: 2.0 };\nprint(get_x(p) + 1.0);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn wildcard_and_literal_patterns_typecheck() {
        let src =
            "fn f(n) {\n  match n {\n    0 => \"zero\",\n    _ => \"other\",\n  }\n}\nprint(f(1));";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let mut infer = Infer::new(&ast, &mut interner);
        infer.resolve_program(&stmts);
        assert!(infer.diagnostics.is_empty(), "{:?}", infer.diagnostics);
    }

    #[test]
    fn infer_entry_point_ties_everything_together() {
        let src = "fn identity(x) { x }\nlet a = identity(1);\nprint(a);";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty());
        let (info, diags) = infer(&ast, &mut interner, &stmts);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(!info.expr_types.is_empty());
        assert!(info.fn_schemes.contains_key(&interner.intern("identity")));
    }
}
