use crate::env::Env;
use crate::error::RuntimeError;
use crate::value::{AdtValue, Closure, Value};
use ember_ast::{Ast, Expr, Idx, Interner, MatchArm, Stmt, Symbol};
use ember_lexer::TokenKind;
use ember_span::Span;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Flow {
    Normal(Value),
    Return(Value),
    Break,
    Continue,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "Int({n})"),
            Value::Float(n) => write!(f, "Float({n})"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Nil => write!(f, "Nil"),
            Value::Str(s) => write!(f, "Str({s:?})"),
            Value::List(_) => write!(f, "List(..)"),
            Value::Closure(_) => write!(f, "Closure(..)"),
            Value::Native(n) => write!(f, "Native({})", n.name),
            Value::Adt(_) => write!(f, "Adt(..)"),
            Value::Record { .. } => write!(f, "Record(..)"),
            Value::AdtCtor { .. } => write!(f, "AdtCtor(..)"),
        }
    }
}

pub type EvalResult = Result<Flow, RuntimeError>;

/// Evaluates `$e` (an `EvalResult`-returning call), extracting the `Value`
/// from `Flow::Normal` — any OTHER `Flow` (`Return`/`Break`/`Continue`)
/// immediately returns out of the CALLING function, propagating the
/// non-local control flow upward instead of treating it as a plain value.
/// Used everywhere a node evaluates a sub-node.
macro_rules! propagate {
    ($e:expr) => {
        match $e? {
            Flow::Normal(v) => v,
            other => return Ok(other),
        }
    };
}
// Not yet used outside this module — later tasks (calls, control flow) pull
// `propagate!` into their own modules via `crate::interp::propagate`.
#[allow(unused_imports)]
pub(crate) use propagate;

// Deliberately well below what a genuinely deep call chain could tolerate:
// each interpreted call now recurses through several native stack frames
// (`eval_expr` -> `eval_expr_uninstrumented` -> `eval_block` -> `exec_stmt`
// -> `exec_stmt_uninstrumented` -> `eval_call` -> `call_closure` -> ...) —
// the step-mode wrappers add one extra frame per `eval_expr`/`exec_stmt`
// call on top of that — and this limit must trip *before* that exhausts
// the real OS thread stack (observed to abort the process around a native
// recursion depth in the low hundreds in an unoptimized debug build) —
// otherwise `call_depth >= MAX_CALL_DEPTH` never gets a chance to turn an
// unbounded recursive program into a clean `RuntimeError` instead of a
// crash.
const MAX_CALL_DEPTH: usize = 64;

pub struct Interp<'a> {
    pub(crate) ast: &'a Ast,
    pub(crate) interner: &'a Interner,
    // `call_depth`/`call_stack` are maintained by `eval_call` to enforce
    // `MAX_CALL_DEPTH` and to build `RuntimeError::call_stack` on overflow;
    // `step_hook` fires once per evaluated node via the `eval_expr`/
    // `exec_stmt` wrappers.
    pub(crate) call_depth: usize,
    pub(crate) call_stack: Vec<Span>,
    pub(crate) step_hook: Option<Box<dyn FnMut(crate::interp::StepEvent)>>,
}

#[derive(Debug, Clone)]
pub struct StepEvent {
    pub node_span: Span,
    /// The whole `Env` chain flattened, innermost binding wins on a name
    /// present at multiple levels. `Value` clones are cheap — every
    /// heap-backed variant is `Rc`.
    pub env_snapshot: Vec<(Symbol, Value)>,
    /// `Some` for an expression that produced a `Flow::Normal` value;
    /// `None` for a statement, or for a non-local `Flow` (the debugger
    /// sees the CONTROL FLOW itself via subsequent events, not a value).
    pub result: Option<Value>,
}

impl<'a> Interp<'a> {
    pub fn new(ast: &'a Ast, interner: &'a Interner) -> Self {
        Interp {
            ast,
            interner,
            call_depth: 0,
            call_stack: Vec::new(),
            step_hook: None,
        }
    }

    pub fn set_step_hook(&mut self, hook: Box<dyn FnMut(StepEvent)>) {
        self.step_hook = Some(hook);
    }

    fn snapshot_env(env: &Rc<RefCell<Env>>) -> Vec<(Symbol, Value)> {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut out = Vec::new();
        let mut cur = Some(Rc::clone(env));
        while let Some(e) = cur {
            let borrowed = e.borrow();
            for (k, v) in borrowed.values.iter() {
                if seen.insert(*k) {
                    out.push((*k, v.clone()));
                }
            }
            cur = borrowed.parent.clone();
        }
        out
    }

    pub fn eval_expr(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult {
        let result = self.eval_expr_uninstrumented(idx, env);
        self.fire_step_hook(self.ast.span_of_expr(idx), env, &result);
        result
    }

    fn fire_step_hook(&mut self, node_span: Span, env: &Rc<RefCell<Env>>, result: &EvalResult) {
        if self.step_hook.is_none() {
            return;
        }
        let env_snapshot = Self::snapshot_env(env);
        let result_value = match result {
            Ok(Flow::Normal(v)) => Some(v.clone()),
            _ => None,
        };
        if let Some(hook) = &mut self.step_hook {
            hook(StepEvent {
                node_span,
                env_snapshot,
                result: result_value,
            });
        }
    }

    fn eval_expr_uninstrumented(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult {
        let span = self.ast.span_of_expr(idx);
        match self.ast.expr(idx).clone() {
            Expr::Int(n) => Ok(Flow::Normal(Value::Int(n))),
            Expr::Float(n) => Ok(Flow::Normal(Value::Float(n))),
            Expr::Str(s) => Ok(Flow::Normal(Value::Str(Rc::new(
                self.interner.resolve(s).to_string(),
            )))),
            Expr::Bool(b) => Ok(Flow::Normal(Value::Bool(b))),
            Expr::Nil => Ok(Flow::Normal(Value::Nil)),
            Expr::Var(sym) => self.eval_var(sym, env, span),
            Expr::Unary { op, operand } => self.eval_unary(op, operand, env, span),
            Expr::Binary { op, lhs, rhs } => self.eval_binary(op, lhs, rhs, env, span),
            Expr::Lambda { params, body } => {
                let closure = Closure {
                    params: params.iter().map(|p| p.name).collect(),
                    body,
                    env: Rc::clone(env),
                };
                Ok(Flow::Normal(Value::Closure(Rc::new(closure))))
            }
            Expr::Call { callee, args } => self.eval_call(callee, &args, env, span),
            Expr::If { cond, then_, else_ } => {
                let c = propagate!(self.eval_expr(cond, env));
                match c {
                    Value::Bool(true) => self.eval_expr(then_, env),
                    Value::Bool(false) => match else_ {
                        Some(e) => self.eval_expr(e, env),
                        None => Ok(Flow::Normal(Value::Nil)),
                    },
                    other => Err(RuntimeError::new(
                        format!("expected Bool, found {other:?}"),
                        span,
                    )),
                }
            }
            Expr::Block { stmts, tail } => self.eval_block(&stmts, tail, env),
            Expr::Assign { target, value } => self.eval_assign(target, value, env, span),
            Expr::List { items } => {
                let mut vals = Vec::with_capacity(items.len());
                for &it in &items {
                    vals.push(propagate!(self.eval_expr(it, env)));
                }
                Ok(Flow::Normal(Value::List(Rc::new(RefCell::new(vals)))))
            }
            Expr::Index { base, index } => {
                let b = propagate!(self.eval_expr(base, env));
                let i = propagate!(self.eval_expr(index, env));
                match (b, i) {
                    (Value::List(l), Value::Int(idx)) => {
                        let list = l.borrow();
                        if idx < 0 || idx as usize >= list.len() {
                            Err(RuntimeError::new(
                                format!("index {idx} out of bounds (length {})", list.len()),
                                span,
                            ))
                        } else {
                            Ok(Flow::Normal(list[idx as usize].clone()))
                        }
                    }
                    (other, _) => Err(RuntimeError::new(
                        format!("cannot index into {other:?}"),
                        span,
                    )),
                }
            }
            Expr::Field { base, name } => {
                let b = propagate!(self.eval_expr(base, env));
                match b {
                    Value::Record { fields, .. } => match fields.borrow().get(&name) {
                        Some(v) => Ok(Flow::Normal(v.clone())),
                        None => {
                            let field_str = self.interner.resolve(name).to_string();
                            Err(RuntimeError::new(
                                format!("no field `{field_str}` on this record"),
                                span,
                            ))
                        }
                    },
                    other => Err(RuntimeError::new(
                        format!("cannot access a field on {other:?}"),
                        span,
                    )),
                }
            }
            Expr::Struct { name, fields } => {
                let mut map = rustc_hash::FxHashMap::default();
                for (field_name, value_expr) in fields {
                    let v = propagate!(self.eval_expr(value_expr, env));
                    map.insert(field_name, v);
                }
                Ok(Flow::Normal(Value::Record {
                    name,
                    fields: Rc::new(RefCell::new(map)),
                }))
            }
            Expr::Match { scrutinee, arms } => self.eval_match(scrutinee, &arms, env, span),
            Expr::Error => Ok(Flow::Normal(Value::Nil)),
        }
    }

    fn eval_match(
        &mut self,
        scrutinee: Idx<Expr>,
        arms: &[MatchArm],
        env: &Rc<RefCell<Env>>,
        span: Span,
    ) -> EvalResult {
        let value = propagate!(self.eval_expr(scrutinee, env));
        for arm in arms {
            let arm_env = Env::child(env);
            if crate::pattern::match_pattern(self.ast, self.interner, arm.pat, &value, &arm_env) {
                if let Some(guard) = arm.guard {
                    match propagate!(self.eval_expr(guard, &arm_env)) {
                        Value::Bool(true) => {}
                        Value::Bool(false) => continue,
                        other => {
                            return Err(RuntimeError::new(
                                format!("expected Bool, found {other:?}"),
                                span,
                            ))
                        }
                    }
                }
                return self.eval_expr(arm.body, &arm_env);
            }
        }
        Err(RuntimeError::new(
            "no pattern matched (this should have been caught by exhaustiveness checking upstream)",
            span,
        ))
    }

    fn eval_block(
        &mut self,
        stmts: &[Idx<Stmt>],
        tail: Option<Idx<Expr>>,
        env: &Rc<RefCell<Env>>,
    ) -> EvalResult {
        let block_env = Env::child(env);
        for &s in stmts {
            propagate!(self.exec_stmt(s, &block_env));
        }
        match tail {
            Some(t) => self.eval_expr(t, &block_env),
            None => Ok(Flow::Normal(Value::Nil)),
        }
    }

    fn eval_assign(
        &mut self,
        target: Idx<Expr>,
        value: Idx<Expr>,
        env: &Rc<RefCell<Env>>,
        span: Span,
    ) -> EvalResult {
        let v = propagate!(self.eval_expr(value, env));
        match self.ast.expr(target).clone() {
            Expr::Var(sym) => {
                if Env::set(env, sym, v.clone()) {
                    Ok(Flow::Normal(v))
                } else {
                    let name = self.interner.resolve(sym).to_string();
                    Err(RuntimeError::new(
                        format!("undefined variable `{name}`"),
                        span,
                    ))
                }
            }
            Expr::Index { base, index } => {
                let b = propagate!(self.eval_expr(base, env));
                let i = propagate!(self.eval_expr(index, env));
                match (b, i) {
                    (Value::List(l), Value::Int(idx)) => {
                        let mut list = l.borrow_mut();
                        if idx < 0 || idx as usize >= list.len() {
                            Err(RuntimeError::new(
                                format!("index {idx} out of bounds (length {})", list.len()),
                                span,
                            ))
                        } else {
                            list[idx as usize] = v.clone();
                            Ok(Flow::Normal(v))
                        }
                    }
                    (other, _) => Err(RuntimeError::new(
                        format!("cannot index into {other:?}"),
                        span,
                    )),
                }
            }
            Expr::Field { base, name } => {
                let b = propagate!(self.eval_expr(base, env));
                match b {
                    Value::Record { fields, .. } => {
                        fields.borrow_mut().insert(name, v.clone());
                        Ok(Flow::Normal(v))
                    }
                    other => Err(RuntimeError::new(
                        format!("cannot assign to a field on {other:?}"),
                        span,
                    )),
                }
            }
            _ => Err(RuntimeError::new(
                "only variable assignment is supported so far",
                span,
            )),
        }
    }

    /// Minimal statement driver — handles just enough (`ExprStmt`/`Let`/`Fn`)
    /// for this task's tests to exercise closures and calls via a sequence
    /// of top-level statements. A later task replaces this with the full
    /// two-pass version (mutual recursion between top-level `fn`s, etc.).
    pub fn exec_stmt(&mut self, idx: Idx<Stmt>, env: &Rc<RefCell<Env>>) -> EvalResult {
        let result = self.exec_stmt_uninstrumented(idx, env);
        self.fire_step_hook(self.ast.span_of_stmt(idx), env, &result);
        result
    }

    fn exec_stmt_uninstrumented(&mut self, idx: Idx<Stmt>, env: &Rc<RefCell<Env>>) -> EvalResult {
        match self.ast.stmt(idx).clone() {
            Stmt::ExprStmt(e) => self.eval_expr(e, env),
            Stmt::Let { name, init, .. } => {
                let v = propagate!(self.eval_expr(init, env));
                Env::declare(env, name, v);
                Ok(Flow::Normal(Value::Nil))
            }
            Stmt::Fn {
                name, params, body, ..
            } => {
                let closure = Closure {
                    params: params.iter().map(|p| p.name).collect(),
                    body,
                    env: Rc::clone(env),
                };
                Env::declare(env, name, Value::Closure(Rc::new(closure)));
                Ok(Flow::Normal(Value::Nil))
            }
            Stmt::While { cond, body } => {
                loop {
                    let c = propagate!(self.eval_expr(cond, env));
                    match c {
                        Value::Bool(true) => {}
                        Value::Bool(false) => break,
                        other => {
                            return Err(RuntimeError::new(
                                format!("expected Bool, found {other:?}"),
                                self.ast.span_of_expr(cond),
                            ))
                        }
                    }
                    match self.eval_expr(body, env)? {
                        Flow::Normal(_) => {}
                        Flow::Break => break,
                        Flow::Continue => continue,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Normal(Value::Nil))
            }
            Stmt::For {
                binding,
                iter,
                body,
            } => {
                let iter_val = propagate!(self.eval_expr(iter, env));
                let items = match iter_val {
                    Value::List(l) => l.borrow().clone(),
                    other => {
                        return Err(RuntimeError::new(
                            format!("expected a list to iterate, found {other:?}"),
                            self.ast.span_of_expr(iter),
                        ))
                    }
                };
                let loop_env = Env::child(env);
                for item in items {
                    Env::declare(&loop_env, binding, item);
                    match self.eval_expr(body, &loop_env)? {
                        Flow::Normal(_) => {}
                        Flow::Break => break,
                        Flow::Continue => continue,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Normal(Value::Nil))
            }
            Stmt::Loop { body } => {
                loop {
                    match self.eval_expr(body, env)? {
                        Flow::Normal(_) => {}
                        Flow::Break => break,
                        Flow::Continue => continue,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Normal(Value::Nil))
            }
            Stmt::Return(value) => {
                let v = match value {
                    Some(e) => propagate!(self.eval_expr(e, env)),
                    None => Value::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::TypeDecl { name, variants } => {
                for variant in variants {
                    if variant.payload.is_empty() {
                        let adt = crate::value::AdtValue {
                            type_name: name,
                            variant: variant.name,
                            fields: Vec::new(),
                        };
                        Env::declare(env, variant.name, Value::Adt(Rc::new(adt)));
                    } else {
                        Env::declare(
                            env,
                            variant.name,
                            Value::AdtCtor {
                                type_name: name,
                                variant: variant.name,
                                arity: variant.payload.len(),
                            },
                        );
                    }
                }
                Ok(Flow::Normal(Value::Nil))
            }
            _ => Ok(Flow::Normal(Value::Nil)), // StructDecl/Error need no runtime work
        }
    }

    fn eval_call(
        &mut self,
        callee: Idx<Expr>,
        args: &[Idx<Expr>],
        env: &Rc<RefCell<Env>>,
        call_span: Span,
    ) -> EvalResult {
        let callee_val = propagate!(self.eval_expr(callee, env));
        let mut arg_vals = Vec::with_capacity(args.len());
        for &a in args {
            arg_vals.push(propagate!(self.eval_expr(a, env)));
        }
        match callee_val {
            Value::Closure(closure) => self.call_closure(&closure, arg_vals, call_span),
            Value::Native(native) => {
                if native.arity != arg_vals.len() {
                    return Err(RuntimeError::new(
                        format!(
                            "{} expects {} argument(s), found {}",
                            native.name,
                            native.arity,
                            arg_vals.len()
                        ),
                        call_span,
                    ));
                }
                (native.func)(&arg_vals, call_span, self.interner).map(Flow::Normal)
            }
            Value::AdtCtor {
                type_name,
                variant,
                arity,
            } => {
                if arity != arg_vals.len() {
                    let name = self.interner.resolve(variant).to_string();
                    return Err(RuntimeError::new(
                        format!(
                            "`{name}` expects {arity} argument(s), found {}",
                            arg_vals.len()
                        ),
                        call_span,
                    ));
                }
                Ok(Flow::Normal(Value::Adt(Rc::new(AdtValue {
                    type_name,
                    variant,
                    fields: arg_vals,
                }))))
            }
            other => Err(RuntimeError::new(
                format!("cannot call a non-function value: {other:?}"),
                call_span,
            )),
        }
    }

    fn call_closure(&mut self, closure: &Closure, args: Vec<Value>, call_span: Span) -> EvalResult {
        if closure.params.len() != args.len() {
            return Err(RuntimeError::new(
                format!(
                    "expected {} argument(s), found {}",
                    closure.params.len(),
                    args.len()
                ),
                call_span,
            ));
        }
        if self.call_depth >= MAX_CALL_DEPTH {
            let mut err = RuntimeError::new("stack overflow", call_span);
            err.call_stack = self.call_stack.clone();
            return Err(err);
        }
        let call_env = Env::child(&closure.env);
        for (p, v) in closure.params.iter().zip(args) {
            Env::declare(&call_env, *p, v);
        }
        self.call_depth += 1;
        self.call_stack.push(call_span);
        let result = self.eval_expr(closure.body, &call_env);
        self.call_depth -= 1;
        self.call_stack.pop();
        match result? {
            Flow::Normal(v) => Ok(Flow::Normal(v)),
            Flow::Return(v) => Ok(Flow::Normal(v)),
            Flow::Break | Flow::Continue => Err(RuntimeError::new(
                "break/continue used outside a loop",
                call_span,
            )),
        }
    }

    fn eval_var(&mut self, sym: Symbol, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
        if let Some(v) = Env::get(env, sym) {
            return Ok(Flow::Normal(v));
        }
        let name = self.interner.resolve(sym);
        if let Some(native) = crate::natives::lookup(name) {
            return Ok(Flow::Normal(Value::Native(Rc::new(native))));
        }
        Err(RuntimeError::new(
            format!("undefined variable `{name}`"),
            span,
        ))
    }

    fn eval_unary(
        &mut self,
        op: TokenKind,
        operand: Idx<Expr>,
        env: &Rc<RefCell<Env>>,
        span: Span,
    ) -> EvalResult {
        let v = propagate!(self.eval_expr(operand, env));
        match (op, v) {
            (TokenKind::Bang, Value::Bool(b)) => Ok(Flow::Normal(Value::Bool(!b))),
            (TokenKind::Minus, Value::Int(n)) => match n.checked_neg() {
                Some(r) => Ok(Flow::Normal(Value::Int(r))),
                None => Err(RuntimeError::new(
                    format!("integer overflow negating {n}"),
                    span,
                )),
            },
            (TokenKind::Minus, Value::Float(n)) => Ok(Flow::Normal(Value::Float(-n))),
            (_, other) => Err(RuntimeError::new(
                format!("invalid operand for unary operator: {other:?}"),
                span,
            )),
        }
    }

    fn eval_binary(
        &mut self,
        op: TokenKind,
        lhs: Idx<Expr>,
        rhs: Idx<Expr>,
        env: &Rc<RefCell<Env>>,
        span: Span,
    ) -> EvalResult {
        // Logical operators short-circuit: the right side must not even be
        // evaluated when the left side already determines the result.
        if op == TokenKind::AndAnd {
            let l = propagate!(self.eval_expr(lhs, env));
            return match l {
                Value::Bool(false) => Ok(Flow::Normal(Value::Bool(false))),
                Value::Bool(true) => self.eval_expr(rhs, env),
                other => Err(RuntimeError::new(
                    format!("expected Bool, found {other:?}"),
                    span,
                )),
            };
        }
        if op == TokenKind::OrOr {
            let l = propagate!(self.eval_expr(lhs, env));
            return match l {
                Value::Bool(true) => Ok(Flow::Normal(Value::Bool(true))),
                Value::Bool(false) => self.eval_expr(rhs, env),
                other => Err(RuntimeError::new(
                    format!("expected Bool, found {other:?}"),
                    span,
                )),
            };
        }

        let l = propagate!(self.eval_expr(lhs, env));
        let r = propagate!(self.eval_expr(rhs, env));
        self.apply_binary(op, l, r, span)
    }

    fn apply_binary(&self, op: TokenKind, l: Value, r: Value, span: Span) -> EvalResult {
        use TokenKind::*;
        let overflow = |op_name: &str, a: i64, b: i64| {
            RuntimeError::new(format!("integer overflow: {a} {op_name} {b}"), span)
        };
        match (op, l, r) {
            (Plus, Value::Int(a), Value::Int(b)) => a
                .checked_add(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("+", a, b)),
            (Minus, Value::Int(a), Value::Int(b)) => a
                .checked_sub(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("-", a, b)),
            (Star, Value::Int(a), Value::Int(b)) => a
                .checked_mul(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("*", a, b)),
            (Slash, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::new("division by zero", span))
                } else {
                    a.checked_div(b)
                        .map(Value::Int)
                        .ok_or_else(|| overflow("/", a, b))
                }
            }
            (Percent, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::new("division by zero", span))
                } else {
                    a.checked_rem(b)
                        .map(Value::Int)
                        .ok_or_else(|| overflow("%", a, b))
                }
            }
            (Plus, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Minus, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Star, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Slash, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Plus, Value::Str(a), Value::Str(b)) => Ok(Value::Str(Rc::new(format!("{a}{b}")))),
            (EqEq, a, b) => Ok(Value::Bool(values_equal(&a, &b))),
            (BangEq, a, b) => Ok(Value::Bool(!values_equal(&a, &b))),
            (Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (LtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (GtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (Lt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            (LtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
            (Gt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            (GtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
            (op, l, r) => Err(RuntimeError::new(
                format!("invalid operands for {op:?}: {l:?}, {r:?}"),
                span,
            )),
        }
        .map(Flow::Normal)
    }
}

/// Structural value equality — `List`/`Record` compare by contents, not by
/// `Rc` pointer identity (two separately-built lists with the same
/// elements must compare equal, matching ordinary language semantics).
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

/// Runs a whole program: hoists top-level `fn`s (so mutual recursion
/// works, mirroring the resolver's and type checker's own two-pass hoist),
/// then executes every statement in order. Returns the last statement's
/// value on success, or the first `RuntimeError` encountered — unlike the
/// earlier compile-time passes, execution genuinely stops at the first
/// runtime failure rather than accumulating diagnostics, since there's no
/// meaningful way to "keep going" after one.
pub fn interpret(
    ast: &Ast,
    interner: &Interner,
    stmts: &[Idx<Stmt>],
) -> (Option<Value>, Option<RuntimeError>) {
    let mut interp = Interp::new(ast, interner);
    let env = Env::new();

    for &s in stmts {
        if let Stmt::Fn {
            name, params, body, ..
        } = ast.stmt(s).clone()
        {
            let closure = Closure {
                params: params.iter().map(|p| p.name).collect(),
                body,
                env: Rc::clone(&env),
            };
            Env::declare(&env, name, Value::Closure(Rc::new(closure)));
        }
    }

    let mut last = Value::Nil;
    for &s in stmts {
        if matches!(ast.stmt(s), Stmt::Fn { .. }) {
            continue;
        }
        match interp.exec_stmt(s, &env) {
            Ok(Flow::Normal(v)) => last = v,
            Ok(Flow::Return(v)) => return (Some(v), None), // a bare top-level `return` ends the program early
            Ok(Flow::Break | Flow::Continue) => {
                return (
                    None,
                    Some(RuntimeError::new(
                        "break/continue used outside a loop",
                        ast.span_of_stmt(s),
                    )),
                );
            }
            Err(e) => return (None, Some(e)),
        }
    }
    (Some(last), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_expr(src: &str) -> Result<Value, RuntimeError> {
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty(), "parse diags: {diags:?}");
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let last = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected an ExprStmt, got {other:?}"),
        };
        match interp.eval_expr(last, &env)? {
            Flow::Normal(v) => Ok(v),
            other => panic!("expected Normal flow, got a non-local flow: {other:?}"),
        }
    }

    #[test]
    fn int_literal_evaluates_to_itself() {
        let v = run_expr("42;").unwrap();
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn arithmetic_and_comparison_work() {
        assert!(matches!(run_expr("1 + 2;").unwrap(), Value::Int(3)));
        assert!(matches!(run_expr("5 - 2 * 2;").unwrap(), Value::Int(1)));
        assert!(matches!(run_expr("3 < 4;").unwrap(), Value::Bool(true)));
    }

    #[test]
    fn logical_short_circuit_never_evaluates_the_right_side() {
        // `1 / 0` on the right would error if evaluated — short-circuit
        // must prevent that.
        assert!(matches!(
            run_expr("false && (1 / 0 == 0);").unwrap(),
            Value::Bool(false)
        ));
        assert!(matches!(
            run_expr("true || (1 / 0 == 0);").unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn integer_overflow_is_a_diagnostic_not_a_panic() {
        let err = run_expr("9223372036854775807 + 1;").unwrap_err();
        assert!(err.message.to_lowercase().contains("overflow"));
    }

    #[test]
    fn division_by_zero_is_a_diagnostic() {
        let err = run_expr("1 / 0;").unwrap_err();
        assert!(err.message.to_lowercase().contains("zero"));
    }

    #[test]
    fn runtime_error_span_points_at_the_failing_subexpression() {
        // `1 + (2 / 0)` — the error must point at `2 / 0` specifically,
        // not the outer `+` expression or the whole statement. This falls
        // out naturally from `eval_binary` always using the span of the
        // exact node it's currently evaluating (`self.ast.span_of_expr`
        // computed fresh at each recursive call), not a span threaded
        // down from an outer caller.
        let src = "1 + (2 / 0);";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let last = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            _ => unreachable!(),
        };
        let err = interp.eval_expr(last, &env).unwrap_err();
        let failing_text = &src[err.span.start as usize..err.span.end as usize];
        assert_eq!(
            failing_text, "2 / 0",
            "error span should cover just the failing subexpression, got {failing_text:?}"
        );
    }

    #[test]
    fn var_looks_up_the_environment() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse("x;");
        assert!(diags.is_empty());
        let env = crate::env::Env::new();
        let x = interner.intern("x");
        crate::env::Env::declare(&env, x, Value::Int(7));
        let mut interp = Interp::new(&ast, &interner);
        let last = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            _ => unreachable!(),
        };
        match interp.eval_expr(last, &env).unwrap() {
            Flow::Normal(Value::Int(7)) => {}
            other => panic!("expected Int(7), got {other:?}"),
        }
    }

    #[test]
    fn a_lambda_captures_its_environment() {
        let (ast, mut interner, stmts, diags) =
            ember_parser::parse("let x = 10;\nlet f = |y| x + y;\nf(5);");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(15))));
        let _ = &mut interner;
    }

    #[test]
    fn two_closures_share_a_mutable_capture() {
        // `_ignored` params sidestep any question of whether zero-parameter
        // lambda syntax is supported — this test's whole point is closure
        // ENVIRONMENT SHARING, not arity.
        let src = "let mut counter = 0;\nlet inc = |_ignored| { counter = counter + 1; };\nlet get = |_ignored| counter;\ninc(0);\ninc(0);\nget(0);";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(
            matches!(result, Some(Value::Int(2))),
            "expected the second closure to see both mutations from the first, got {result:?}"
        );
    }

    #[test]
    fn recursive_calls_work() {
        let src = "fn fact(n) { if n == 0 { 1 } else { n * fact(n - 1) } }\nfact(5);";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(120))));
    }

    #[test]
    fn wrong_argument_count_errors() {
        let (ast, interner, stmts, diags) = ember_parser::parse("let f = |x| x;\nf(1, 2);");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut last_err = None;
        for &s in &stmts {
            if let Err(e) = interp.exec_stmt(s, &env) {
                last_err = Some(e);
            }
        }
        assert!(last_err.is_some());
    }

    #[test]
    fn deep_recursion_reports_stack_overflow_not_a_crash() {
        let src = "fn loop_forever(n) { loop_forever(n + 1) }\nloop_forever(0);";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut last_err = None;
        for &s in &stmts {
            if let Err(e) = interp.exec_stmt(s, &env) {
                last_err = Some(e);
            }
        }
        let err = last_err.expect("expected a stack overflow error");
        assert!(err.message.to_lowercase().contains("stack overflow"));
        assert!(
            !err.call_stack.is_empty(),
            "expected a non-empty call chain"
        );
    }

    #[test]
    fn if_else_selects_the_right_branch() {
        let (ast, interner, stmts, diags) = ember_parser::parse("if true { 1 } else { 2 };");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let last = last_expr(&ast, &stmts);
        assert!(matches!(
            interp.eval_expr(last, &env).unwrap(),
            Flow::Normal(Value::Int(1))
        ));
    }

    #[test]
    fn while_loop_with_break_and_continue() {
        let src = "let mut i = 0;\nlet mut sum = 0;\nwhile i < 10 {\n  i = i + 1;\n  if i == 5 { continue; }\n  if i == 8 { break; }\n  sum = sum + i;\n}\nsum;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        // 1+2+3+4 (skip 5) +6+7 (stop before 8) = 23
        assert!(matches!(result, Some(Value::Int(23))));
    }

    #[test]
    fn for_loop_iterates_a_list() {
        let src = "let mut sum = 0;\nfor x in [1, 2, 3] {\n  sum = sum + x;\n}\nsum;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(6))));
    }

    #[test]
    fn return_short_circuits_out_of_nested_blocks() {
        let src = "fn f() {\n  if true {\n    return 42;\n  }\n  99\n}\nf();";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(42))));
    }

    #[test]
    fn list_literal_and_index() {
        let (ast, interner, stmts, diags) = ember_parser::parse("let xs = [1, 2, 3];\nxs[1];");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(2))));
    }

    #[test]
    fn index_out_of_bounds_is_a_diagnostic() {
        let (ast, interner, stmts, diags) = ember_parser::parse("let xs = [1];\nxs[5];");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut last_err = None;
        for &s in &stmts {
            if let Err(e) = interp.exec_stmt(s, &env) {
                last_err = Some(e);
            }
        }
        let err = last_err.expect("expected an out-of-bounds error");
        assert!(
            err.message.to_lowercase().contains("bounds")
                || err.message.to_lowercase().contains("index")
        );
    }

    #[test]
    fn assignment_mutates_the_binding() {
        let (ast, interner, stmts, diags) = ember_parser::parse("let mut x = 1;\nx = 2;\nx;");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(2))));
    }

    #[test]
    fn index_assignment_mutates_the_list_in_place() {
        let (ast, interner, stmts, diags) =
            ember_parser::parse("let xs = [1, 2, 3];\nxs[1] = 99;\nxs[1];");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Int(99))));
    }

    fn last_expr(
        ast: &ember_ast::Ast,
        stmts: &[ember_ast::Idx<ember_ast::Stmt>],
    ) -> ember_ast::Idx<ember_ast::Expr> {
        match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected an ExprStmt, got {other:?}"),
        }
    }

    #[test]
    fn struct_literal_constructs_a_record_and_field_access_reads_it() {
        let src = "struct Point { x: Float, y: Float }\nlet p = Point { x: 1.0, y: 2.0 };\np.x;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Float(f)) if f == 1.0));
    }

    #[test]
    fn payload_ful_variant_constructs_an_adt_value() {
        let src = "type Shape = | Circle(Float);\nlet c = Circle(3.0);\nc;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        match result {
            Some(Value::Adt(adt)) => {
                assert_eq!(interner.resolve(adt.variant), "Circle");
                assert!(matches!(adt.fields.as_slice(), [Value::Float(f)] if *f == 3.0));
            }
            other => panic!("expected an Adt value, got {other:?}"),
        }
    }

    #[test]
    fn mutual_recursion_between_top_level_functions_works() {
        let src = "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }\nis_even(10);";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let (result, err) = interpret(&ast, &interner, &stmts);
        assert!(err.is_none(), "{err:?}");
        assert!(matches!(result, Some(Value::Bool(true))));
    }

    #[test]
    fn interpret_runs_a_whole_program_and_returns_its_final_value() {
        let src = "let x = 1;\nlet y = 2;\nx + y;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let (result, err) = interpret(&ast, &interner, &stmts);
        assert!(err.is_none(), "{err:?}");
        assert!(matches!(result, Some(Value::Int(3))));
    }

    #[test]
    fn match_dispatches_to_the_first_matching_arm_and_binds_payload() {
        let src = "type Shape = | Circle(Float) | Rect(Float, Float);\nlet s = Circle(2.0);\nmatch s {\n  Circle(r) => r,\n  Rect(w, h) => w,\n};";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        assert!(matches!(result, Some(Value::Float(f)) if f == 2.0));
    }

    #[test]
    fn a_false_guard_falls_through_to_the_next_arm() {
        let src = "let x = 5;\nmatch x {\n  n if n > 10 => \"big\",\n  _ => \"small\",\n};";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        match result {
            Some(Value::Str(s)) => assert_eq!(s.as_str(), "small"),
            other => panic!("expected \"small\", got {other:?}"),
        }
    }

    #[test]
    fn step_hook_receives_one_event_per_evaluated_node() {
        let (ast, interner, stmts, diags) = ember_parser::parse("1 + 2;");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_clone = std::rc::Rc::clone(&events);
        interp.set_step_hook(Box::new(move |e| events_clone.borrow_mut().push(e)));
        let env = crate::env::Env::new();
        let last = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            _ => unreachable!(),
        };
        interp.eval_expr(last, &env).unwrap();
        // 3 nodes: the literal `1`, the literal `2`, and the `1 + 2` binary expr itself.
        assert_eq!(events.borrow().len(), 3);
    }

    #[test]
    fn step_hook_env_snapshot_reflects_current_bindings() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse("let x = 5;\nx;");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let events: std::rc::Rc<std::cell::RefCell<Vec<StepEvent>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_clone = std::rc::Rc::clone(&events);
        interp.set_step_hook(Box::new(move |e| events_clone.borrow_mut().push(e)));
        let env = crate::env::Env::new();
        for &s in &stmts {
            interp.exec_stmt(s, &env).unwrap();
        }
        let x = interner.intern("x");
        let last_event = events
            .borrow()
            .last()
            .cloned()
            .expect("expected at least one step event");
        assert!(last_event.env_snapshot.iter().any(|(sym, _)| *sym == x));
    }

    #[test]
    fn no_hook_installed_means_no_overhead_path_still_works() {
        let (ast, interner, stmts, diags) = ember_parser::parse("1 + 2;");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let last = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            _ => unreachable!(),
        };
        assert!(matches!(
            interp.eval_expr(last, &env).unwrap(),
            Flow::Normal(Value::Int(3))
        ));
    }

    #[test]
    fn nullary_variant_is_already_a_value_no_call_needed() {
        let src = "type Shape = | Point;\nPoint;";
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let mut result = None;
        for &s in &stmts {
            match interp.exec_stmt(s, &env).unwrap() {
                Flow::Normal(v) => result = Some(v),
                other => panic!("unexpected flow: {other:?}"),
            }
        }
        match result {
            Some(Value::Adt(adt)) => assert_eq!(interner.resolve(adt.variant), "Point"),
            other => panic!("expected an Adt value, got {other:?}"),
        }
    }
}
