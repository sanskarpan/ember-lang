# ember Phase 7 Implementation Plan — Tree-Walking Interpreter

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the tree-walking interpreter in `ember-tree` per `SPEC.md §10` — `Value`, `Env`, `Flow`-threaded control flow, `eval_expr`/`exec_stmt` for every AST form, runtime pattern matching, native functions, runtime-error diagnostics, and step-mode. Add `ember-cli run`.

**Architecture:** Self-contained — dynamic `Env`-chain name lookup, no dependency on `ember-resolve`'s slot allocation. Every `eval_expr`/`exec_stmt` call returns `EvalResult = Result<Flow, RuntimeError>` so `return`/`break`/`continue` thread through the return type and propagate out of nested expressions (e.g. a `Block` expression containing a `return` statement) without ever using `panic!`/`catch_unwind`. A `propagate!` macro extracts `Flow::Normal(v) -> v` or short-circuits the enclosing function on any other `Flow` variant — used pervasively any time one node evaluates a sub-node.

**Tech Stack:** Rust, `Rc<RefCell<..>>` for shared mutable heap state, `rustc_hash::FxHashMap`.

---

## Task 1: Scaffold the `ember-tree` crate

**Files:**
- Modify: `crates/ember-tree/Cargo.toml`
- Modify: `crates/ember-tree/src/lib.rs`

- [ ] **Step 1: Write the manifest**

```toml
[package]
name = "ember-tree"
version.workspace = true
edition.workspace = true

[dependencies]
ember-span = { path = "../ember-span" }
ember-diag = { path = "../ember-diag" }
ember-ast = { path = "../ember-ast" }
rustc-hash = "2"

[dev-dependencies]
ember-parser = { path = "../ember-parser" }
```

- [ ] **Step 2: Declare the module layout**

```rust
pub mod env;
pub mod error;
pub mod interp;
pub mod natives;
pub mod pattern;
pub mod value;
```

- [ ] **Step 3: Create empty stub files and verify the build**

```bash
touch crates/ember-tree/src/env.rs crates/ember-tree/src/error.rs crates/ember-tree/src/interp.rs crates/ember-tree/src/natives.rs crates/ember-tree/src/pattern.rs crates/ember-tree/src/value.rs
```

Run: `source "$HOME/.cargo/env" && cargo build -p ember-tree`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-tree Cargo.lock
git commit -m "Scaffold ember-tree crate module layout"
```

---

## Task 2: `error.rs` — `RuntimeError`

**Files:**
- Modify: `crates/ember-tree/src/error.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_span::Span;

    #[test]
    fn to_diagnostic_carries_the_primary_span() {
        let err = RuntimeError::new("division by zero", Span::new(3, 6));
        let diag = err.to_diagnostic();
        assert_eq!(diag.message, "division by zero");
        assert_eq!(diag.labels[0].span, Span::new(3, 6));
    }

    #[test]
    fn call_stack_frames_become_secondary_labels() {
        let mut err = RuntimeError::new("stack overflow", Span::new(0, 1));
        err.call_stack.push(Span::new(10, 20));
        err.call_stack.push(Span::new(30, 40));
        let diag = err.to_diagnostic();
        assert_eq!(diag.labels.len(), 3); // 1 primary + 2 call-stack frames
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree to_diagnostic_carries call_stack_frames`
Expected: FAIL to compile — `RuntimeError` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_diag::Diagnostic;
use ember_span::Span;

#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
    /// Call-site spans accumulated as a "stack overflow" error unwinds
    /// through `eval_call`, innermost first — lets the diagnostic show the
    /// real call chain instead of just where the limit was hit.
    pub call_stack: Vec<Span>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        RuntimeError { message: message.into(), span, call_stack: Vec::new() }
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        let mut diag = Diagnostic::error(self.message.clone()).with_primary(self.span, "here");
        for (i, frame) in self.call_stack.iter().enumerate() {
            diag = diag.with_secondary(*frame, format!("in call frame {}", i + 1));
        }
        diag
    }
}
```

Check `ember_diag::Diagnostic`'s exact field names (`message`, `labels`) match — already established throughout `ember-resolve`/`ember-types`; adjust only if genuinely different.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Add RuntimeError with call-chain-aware diagnostic conversion"
```

---

## Task 3: `value.rs` and `env.rs` — `Value`, `Env`

**Files:**
- Modify: `crates/ember-tree/src/value.rs`
- Modify: `crates/ember-tree/src/env.rs`

`Value` and `Env` reference each other (`Closure` holds an `Env`; `Env` holds `Value`s) — this is fine in Rust across two modules in the same crate, no special handling needed.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/ember-tree/src/value.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_clone_cheaply_via_rc() {
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1)])));
        let cloned = list.clone();
        if let (Value::List(a), Value::List(b)) = (&list, &cloned) {
            assert!(Rc::ptr_eq(a, b), "clone should share the same backing Rc, not deep-copy");
        } else {
            panic!("expected List");
        }
    }
}
```

```rust
// crates/ember-tree/src/env.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use ember_ast::Interner;

    #[test]
    fn declare_and_get_within_one_env() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let env = Env::new();
        Env::declare(&env, x, Value::Int(42));
        assert!(matches!(Env::get(&env, x), Some(Value::Int(42))));
    }

    #[test]
    fn child_env_sees_parent_bindings() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let parent = Env::new();
        Env::declare(&parent, x, Value::Int(1));
        let child = Env::child(&parent);
        assert!(matches!(Env::get(&child, x), Some(Value::Int(1))));
    }

    #[test]
    fn set_mutates_through_the_parent_chain() {
        let mut interner = Interner::new();
        let x = interner.intern("x");
        let parent = Env::new();
        Env::declare(&parent, x, Value::Int(1));
        let child = Env::child(&parent);
        assert!(Env::set(&child, x, Value::Int(2)));
        assert!(matches!(Env::get(&parent, x), Some(Value::Int(2))), "mutation through a child must be visible in the parent");
    }

    #[test]
    fn set_on_an_undeclared_name_returns_false() {
        let mut interner = Interner::new();
        let y = interner.intern("y");
        let env = Env::new();
        assert!(!Env::set(&env, y, Value::Int(1)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree values_clone declare_and_get child_env_sees set_mutates set_on_an_undeclared`
Expected: FAIL to compile — `Value`/`Env` don't exist yet.

- [ ] **Step 3: Implement**

`value.rs`:

```rust
use crate::env::Env;
use crate::error::RuntimeError;
use ember_ast::{Expr, Idx, Symbol};
use ember_span::Span;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<Closure>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    /// Beyond the literal `SPEC.md` sketch: named, so `type_of()` on a
    /// struct instance can report something more useful than "Record".
    Record {
        name: Symbol,
        fields: Rc<RefCell<FxHashMap<Symbol, Value>>>,
    },
    /// A payload-ful ADT variant constructor referenced but not yet
    /// called (e.g. evaluating the bare name `Circle`). A nullary variant
    /// skips this entirely and is bound directly to a `Value::Adt`.
    AdtCtor {
        type_name: Symbol,
        variant: Symbol,
        arity: usize,
    },
}

pub struct AdtValue {
    pub type_name: Symbol,
    pub variant: Symbol,
    pub fields: Vec<Value>,
}

pub struct Closure {
    pub params: Vec<Symbol>,
    pub body: Idx<Expr>,
    pub env: Rc<RefCell<Env>>,
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value], Span, &ember_ast::Interner) -> Result<Value, RuntimeError>,
}
```

(`NativeFn.func` takes `&Interner` — needed for e.g. `type_of` to resolve a struct/ADT's name symbol to text. A plain `fn` pointer, not `Box<dyn Fn>`, since the native set is a small fixed table, not user-extensible.)

`env.rs`:

```rust
use crate::value::Value;
use ember_ast::Symbol;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Env {
    pub values: FxHashMap<Symbol, Value>,
    pub parent: Option<Rc<RefCell<Env>>>,
}

impl Env {
    pub fn new() -> Rc<RefCell<Env>> {
        Rc::new(RefCell::new(Env { values: FxHashMap::default(), parent: None }))
    }

    pub fn child(parent: &Rc<RefCell<Env>>) -> Rc<RefCell<Env>> {
        Rc::new(RefCell::new(Env { values: FxHashMap::default(), parent: Some(Rc::clone(parent)) }))
    }

    pub fn declare(env: &Rc<RefCell<Env>>, name: Symbol, value: Value) {
        env.borrow_mut().values.insert(name, value);
    }

    /// Innermost-first lookup, walking the parent chain.
    pub fn get(env: &Rc<RefCell<Env>>, name: Symbol) -> Option<Value> {
        let e = env.borrow();
        if let Some(v) = e.values.get(&name) {
            return Some(v.clone());
        }
        let parent = e.parent.clone();
        drop(e);
        match parent {
            Some(p) => Env::get(&p, name),
            None => None,
        }
    }

    /// Mutates the NEAREST enclosing declaration of `name`, walking
    /// outward. Returns `false` if `name` was never declared anywhere in
    /// the chain (the resolver should have already caught this upstream
    /// in a well-formed pipeline — this is a defensive fallback, not the
    /// primary correctness mechanism).
    pub fn set(env: &Rc<RefCell<Env>>, name: Symbol, value: Value) -> bool {
        let mut e = env.borrow_mut();
        if e.values.contains_key(&name) {
            e.values.insert(name, value);
            return true;
        }
        let parent = e.parent.clone();
        drop(e);
        match parent {
            Some(p) => Env::set(&p, name, value),
            None => false,
        }
    }
}
```

- [ ] **Step 4: Add `pub mod` lines** — already present from Task 1; no `lib.rs` change needed here.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-tree
git commit -m "Add Value and Env: the interpreter's runtime data model"
```

---

## Task 4: `interp.rs` skeleton — `Flow`, `propagate!`, literals, `Var`, `Unary`/`Binary` with checked arithmetic

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

This task establishes `Interp`, `Flow`, `EvalResult`, and the `propagate!` macro every later task relies on.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn run_expr(src: &str) -> Result<Value, RuntimeError> {
        let (ast, mut interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty(), "parse diags: {diags:?}");
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let last = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected an ExprStmt, got {other:?}"),
        };
        let _ = &mut interner;
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
        assert!(matches!(run_expr("false && (1 / 0 == 0);").unwrap(), Value::Bool(false)));
        assert!(matches!(run_expr("true || (1 / 0 == 0);").unwrap(), Value::Bool(true)));
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
        assert_eq!(failing_text, "2 / 0", "error span should cover just the failing subexpression, got {failing_text:?}");
    }

    #[test]
    fn var_looks_up_the_environment() {
        let (ast, mut interner, stmts, diags) = ember_parser::parse("x;");
        assert!(diags.is_empty());
        let mut interp = Interp::new(&ast, &interner);
        let env = crate::env::Env::new();
        let x = interner.intern("x");
        crate::env::Env::declare(&env, x, Value::Int(7));
        let last = match ast.stmt(*stmts.last().unwrap()) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            _ => unreachable!(),
        };
        match interp.eval_expr(last, &env).unwrap() {
            Flow::Normal(Value::Int(7)) => {}
            other => panic!("expected Int(7), got {other:?}"),
        }
    }
}
```

Note `Flow` needs `Debug` for the `panic!("... {other:?}")` calls above — derive it.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree int_literal arithmetic_and_comparison logical_short_circuit integer_overflow division_by_zero runtime_error_span var_looks_up`
Expected: FAIL to compile — `Interp`/`Flow`/`EvalResult` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::env::Env;
use crate::error::RuntimeError;
use crate::value::Value;
use ember_ast::{Ast, Expr, Idx, Interner, Symbol};
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
pub(crate) use propagate;

const MAX_CALL_DEPTH: usize = 512;

pub struct Interp<'a> {
    pub(crate) ast: &'a Ast,
    pub(crate) interner: &'a Interner,
    pub(crate) call_depth: usize,
    pub(crate) call_stack: Vec<Span>,
    pub(crate) step_hook: Option<Box<dyn FnMut(crate::interp::StepEvent)>>,
}

/// Placeholder so the skeleton compiles before Task 15 (step-mode) adds
/// the real definition — replaced there, not duplicated.
pub struct StepEvent;

impl<'a> Interp<'a> {
    pub fn new(ast: &'a Ast, interner: &'a Interner) -> Self {
        Interp { ast, interner, call_depth: 0, call_stack: Vec::new(), step_hook: None }
    }

    pub fn eval_expr(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult {
        let span = self.ast.span_of_expr(idx);
        match self.ast.expr(idx).clone() {
            Expr::Int(n) => Ok(Flow::Normal(Value::Int(n))),
            Expr::Float(n) => Ok(Flow::Normal(Value::Float(n))),
            Expr::Str(s) => Ok(Flow::Normal(Value::Str(Rc::new(self.interner.resolve(s).to_string())))),
            Expr::Bool(b) => Ok(Flow::Normal(Value::Bool(b))),
            Expr::Nil => Ok(Flow::Normal(Value::Nil)),
            Expr::Var(sym) => self.eval_var(sym, env, span),
            Expr::Unary { op, operand } => self.eval_unary(op, operand, env, span),
            Expr::Binary { op, lhs, rhs } => self.eval_binary(op, lhs, rhs, env, span),
            _ => Ok(Flow::Normal(Value::Nil)), // remaining forms land in later tasks
        }
    }

    fn eval_var(&mut self, sym: Symbol, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
        match Env::get(env, sym) {
            Some(v) => Ok(Flow::Normal(v)),
            None => {
                let name = self.interner.resolve(sym).to_string();
                Err(RuntimeError::new(format!("undefined variable `{name}`"), span))
            }
        }
    }

    fn eval_unary(&mut self, op: TokenKind, operand: Idx<Expr>, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
        let v = propagate!(self.eval_expr(operand, env));
        match (op, v) {
            (TokenKind::Bang, Value::Bool(b)) => Ok(Flow::Normal(Value::Bool(!b))),
            (TokenKind::Minus, Value::Int(n)) => match n.checked_neg() {
                Some(r) => Ok(Flow::Normal(Value::Int(r))),
                None => Err(RuntimeError::new(format!("integer overflow negating {n}"), span)),
            },
            (TokenKind::Minus, Value::Float(n)) => Ok(Flow::Normal(Value::Float(-n))),
            (_, other) => Err(RuntimeError::new(format!("invalid operand for unary operator: {other:?}"), span)),
        }
    }

    fn eval_binary(&mut self, op: TokenKind, lhs: Idx<Expr>, rhs: Idx<Expr>, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
        // Logical operators short-circuit: the right side must not even be
        // evaluated when the left side already determines the result.
        if op == TokenKind::AndAnd {
            let l = propagate!(self.eval_expr(lhs, env));
            return match l {
                Value::Bool(false) => Ok(Flow::Normal(Value::Bool(false))),
                Value::Bool(true) => self.eval_expr(rhs, env),
                other => Err(RuntimeError::new(format!("expected Bool, found {other:?}"), span)),
            };
        }
        if op == TokenKind::OrOr {
            let l = propagate!(self.eval_expr(lhs, env));
            return match l {
                Value::Bool(true) => Ok(Flow::Normal(Value::Bool(true))),
                Value::Bool(false) => self.eval_expr(rhs, env),
                other => Err(RuntimeError::new(format!("expected Bool, found {other:?}"), span)),
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
            (Plus, Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int).ok_or_else(|| overflow("+", a, b)),
            (Minus, Value::Int(a), Value::Int(b)) => a.checked_sub(b).map(Value::Int).ok_or_else(|| overflow("-", a, b)),
            (Star, Value::Int(a), Value::Int(b)) => a.checked_mul(b).map(Value::Int).ok_or_else(|| overflow("*", a, b)),
            (Slash, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::new("division by zero", span))
                } else {
                    a.checked_div(b).map(Value::Int).ok_or_else(|| overflow("/", a, b))
                }
            }
            (Percent, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    Err(RuntimeError::new("division by zero", span))
                } else {
                    a.checked_rem(b).map(Value::Int).ok_or_else(|| overflow("%", a, b))
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
            (op, l, r) => Err(RuntimeError::new(format!("invalid operands for {op:?}: {l:?}, {r:?}"), span)),
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
```

`Value` needs a hand-written `Debug` impl (shown above) since `Closure`/`NativeFn`/`AdtValue` don't derive it — used by the tests' `{:?}` formatting and by error messages.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged. The `_ => Ok(Flow::Normal(Value::Nil))` catch-all in `eval_expr` is expected and intentional at this point — later tasks narrow it.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Add Interp skeleton: Flow, propagate!, literals, Var, checked arithmetic"
```

---

## Task 5: Closures and function calls

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_lambda_captures_its_environment() {
    let (ast, mut interner, stmts, diags) = ember_parser::parse("let x = 10;\nlet f = |y| x + y;\nf(5);");
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
    assert!(matches!(result, Some(Value::Int(2))), "expected the second closure to see both mutations from the first, got {result:?}");
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
    assert!(!err.call_stack.is_empty(), "expected a non-empty call chain");
}
```

These tests call `exec_stmt`, which doesn't exist until Task 9's driver — that's fine, this task ADDS `eval_call`/`call_closure` to `eval_expr`, and a minimal `exec_stmt` handling just `Stmt::Fn`/`Stmt::Let`/`Stmt::ExprStmt` is needed for these tests specifically to run at all. Add this minimal version now (Task 9 replaces it with the full two-pass driver — don't worry about mutual recursion between top-level `fn`s yet, single self-recursion is enough for this task's tests):

```rust
pub fn exec_stmt(&mut self, idx: Idx<Stmt>, env: &Rc<RefCell<Env>>) -> EvalResult {
    match self.ast.stmt(idx).clone() {
        Stmt::ExprStmt(e) => self.eval_expr(e, env),
        Stmt::Let { name, init, .. } => {
            let v = propagate!(self.eval_expr(init, env));
            Env::declare(env, name, v);
            Ok(Flow::Normal(Value::Nil))
        }
        Stmt::Fn { name, params, body, .. } => {
            let closure = Closure { params: params.iter().map(|p| p.name).collect(), body, env: Rc::clone(env) };
            Env::declare(env, name, Value::Closure(Rc::new(closure)));
            Ok(Flow::Normal(Value::Nil))
        }
        _ => Ok(Flow::Normal(Value::Nil)), // remaining forms land in later tasks
    }
}
```

Note this minimal `Stmt::Fn` doesn't yet support recursion via a name declared BEFORE the closure captures the env (needed for `fact` to call itself) — since `Env::declare` happens AFTER building the closure, but the closure's captured `env` Rc is the SAME env `declare` then mutates afterward (interior mutability via `RefCell`), so the closure's captured env DOES see `fact` once declared — this already works correctly as written, since `Closure.env` is a shared `Rc<RefCell<Env>>` pointing at the same environment `declare` inserts into, not a snapshot.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree a_lambda_captures recursive_calls two_closures_share wrong_argument_count deep_recursion`
Expected: FAIL — `eval_call`/`call_closure` don't exist; `Expr::Lambda`/`Expr::Call` fall into the `_ => Ok(Flow::Normal(Value::Nil))` catch-all.

- [ ] **Step 3: Implement**

Add to `eval_expr`'s match:

```rust
Expr::Lambda { params, body } => {
    let closure = Closure { params: params.iter().map(|p| p.name).collect(), body, env: Rc::clone(env) };
    Ok(Flow::Normal(Value::Closure(Rc::new(closure))))
}
Expr::Call { callee, args } => self.eval_call(callee, &args, env, span),
```

Add these methods (needs `use crate::value::Closure;` at the top of the file):

```rust
fn eval_call(&mut self, callee: Idx<Expr>, args: &[Idx<Expr>], env: &Rc<RefCell<Env>>, call_span: Span) -> EvalResult {
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
                    format!("{} expects {} argument(s), found {}", native.name, native.arity, arg_vals.len()),
                    call_span,
                ));
            }
            (native.func)(&arg_vals, call_span, self.interner).map(Flow::Normal)
        }
        Value::AdtCtor { type_name, variant, arity } => {
            if arity != arg_vals.len() {
                let name = self.interner.resolve(variant).to_string();
                return Err(RuntimeError::new(
                    format!("`{name}` expects {arity} argument(s), found {}", arg_vals.len()),
                    call_span,
                ));
            }
            Ok(Flow::Normal(Value::Adt(Rc::new(crate::value::AdtValue { type_name, variant, fields: arg_vals }))))
        }
        other => Err(RuntimeError::new(format!("cannot call a non-function value: {other:?}"), call_span)),
    }
}

fn call_closure(&mut self, closure: &Closure, args: Vec<Value>, call_span: Span) -> EvalResult {
    if closure.params.len() != args.len() {
        return Err(RuntimeError::new(
            format!("expected {} argument(s), found {}", closure.params.len(), args.len()),
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
        Flow::Break | Flow::Continue => Err(RuntimeError::new("break/continue used outside a loop", call_span)),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Evaluate closures and function calls, with a real call-depth stack-overflow limit"
```

---

## Task 6: Control flow — `If`, `Block`, `While`, `For`, `Loop`, `Return`, `Break`, `Continue`

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn if_else_selects_the_right_branch() {
    let (ast, interner, stmts, diags) = ember_parser::parse("if true { 1 } else { 2 };");
    assert!(diags.is_empty());
    let mut interp = Interp::new(&ast, &interner);
    let env = crate::env::Env::new();
    let last = last_expr(&ast, &stmts);
    assert!(matches!(interp.eval_expr(last, &env).unwrap(), Flow::Normal(Value::Int(1))));
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

fn last_expr(ast: &ember_ast::Ast, stmts: &[ember_ast::Idx<ember_ast::Stmt>]) -> ember_ast::Idx<ember_ast::Expr> {
    match ast.stmt(*stmts.last().unwrap()) {
        ember_ast::Stmt::ExprStmt(e) => *e,
        other => panic!("expected an ExprStmt, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree if_else_selects while_loop_with for_loop_iterates return_short_circuits`
Expected: FAIL — `If`/`Block` fall into the catch-all; `While`/`For`/`Loop`/`Return`/`Break`/`Continue` aren't in `exec_stmt` yet.

- [ ] **Step 3: Implement**

Add to `eval_expr`'s match:

```rust
Expr::If { cond, then_, else_ } => {
    let c = propagate!(self.eval_expr(cond, env));
    match c {
        Value::Bool(true) => self.eval_expr(then_, env),
        Value::Bool(false) => match else_ {
            Some(e) => self.eval_expr(e, env),
            None => Ok(Flow::Normal(Value::Nil)),
        },
        other => Err(RuntimeError::new(format!("expected Bool, found {other:?}"), span)),
    }
}
Expr::Block { stmts, tail } => self.eval_block(&stmts, tail, env),
```

Add this method:

```rust
fn eval_block(&mut self, stmts: &[Idx<Stmt>], tail: Option<Idx<Expr>>, env: &Rc<RefCell<Env>>) -> EvalResult {
    let block_env = Env::child(env);
    for &s in stmts {
        propagate!(self.exec_stmt(s, &block_env));
    }
    match tail {
        Some(t) => self.eval_expr(t, &block_env),
        None => Ok(Flow::Normal(Value::Nil)),
    }
}
```

Replace `exec_stmt`'s catch-all (`_ => Ok(Flow::Normal(Value::Nil))`) with these new arms, keeping the catch-all ONLY for `TypeDecl`/`StructDecl`/`Error` (later tasks handle those):

```rust
Stmt::While { cond, body } => {
    loop {
        let c = propagate!(self.eval_expr(cond, env));
        match c {
            Value::Bool(true) => {}
            Value::Bool(false) => break,
            other => return Err(RuntimeError::new(format!("expected Bool, found {other:?}"), self.ast.span_of_expr(cond))),
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
Stmt::For { binding, iter, body } => {
    let iter_val = propagate!(self.eval_expr(iter, env));
    let items = match iter_val {
        Value::List(l) => l.borrow().clone(),
        other => return Err(RuntimeError::new(format!("expected a list to iterate, found {other:?}"), self.ast.span_of_expr(iter))),
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Evaluate control flow: if/block/while/for/loop/return/break/continue"
```

---

## Task 7: `List`, `Index`, `Assign`, `Field`

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

- [ ] **Step 1: Write the failing tests**

```rust
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
    assert!(err.message.to_lowercase().contains("bounds") || err.message.to_lowercase().contains("index"));
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
fn push_mutates_the_list_in_place_visible_through_another_reference() {
    let (ast, interner, stmts, diags) = ember_parser::parse("let xs = [1];\nlet ys = xs;\npush(xs, 2);\nlen(ys);");
    assert!(diags.is_empty());
    let mut interp = Interp::new(&ast, &interner);
    let env = crate::env::Env::new();
    // `push`/`len` aren't wired as natives until Task 12 — this test is
    // moved there instead; DO NOT include it in this task's test set. Use
    // the two tests above (list index, assignment) only for this task.
    let _ = (interp, env);
}
```

Delete the `push_mutates_the_list_in_place_visible_through_another_reference` test above — it's a forward-reference note, not a real test for this task (natives don't exist until Task 12). Only add `list_literal_and_index`, `index_out_of_bounds_is_a_diagnostic`, and `assignment_mutates_the_binding` in this task.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree list_literal_and_index index_out_of_bounds assignment_mutates`
Expected: FAIL — `List`/`Index`/`Assign` fall into the `eval_expr` catch-all.

- [ ] **Step 3: Implement**

Add to `eval_expr`'s match:

```rust
Expr::List { items } => {
    let mut vals = Vec::with_capacity(items.len());
    for &i in &items {
        vals.push(propagate!(self.eval_expr(i, env)));
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
                Err(RuntimeError::new(format!("index {idx} out of bounds (length {})", list.len()), span))
            } else {
                Ok(Flow::Normal(list[idx as usize].clone()))
            }
        }
        (other, _) => Err(RuntimeError::new(format!("cannot index into {other:?}"), span)),
    }
}
Expr::Assign { target, value } => self.eval_assign(target, value, env, span),
Expr::Field { base, name } => {
    let b = propagate!(self.eval_expr(base, env));
    match b {
        Value::Record { fields, .. } => match fields.borrow().get(&name) {
            Some(v) => Ok(Flow::Normal(v.clone())),
            None => {
                let field_str = self.interner.resolve(name).to_string();
                Err(RuntimeError::new(format!("no field `{field_str}` on this record"), span))
            }
        },
        other => Err(RuntimeError::new(format!("cannot access a field on {other:?}"), span)),
    }
}
```

Add this method:

```rust
fn eval_assign(&mut self, target: Idx<Expr>, value: Idx<Expr>, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
    let v = propagate!(self.eval_expr(value, env));
    match self.ast.expr(target).clone() {
        Expr::Var(sym) => {
            if !Env::set(env, sym, v.clone()) {
                let name = self.interner.resolve(sym).to_string();
                return Err(RuntimeError::new(format!("undefined variable `{name}`"), span));
            }
            Ok(Flow::Normal(v))
        }
        Expr::Index { base, index } => {
            let b = propagate!(self.eval_expr(base, env));
            let i = propagate!(self.eval_expr(index, env));
            match (b, i) {
                (Value::List(l), Value::Int(idx)) => {
                    let mut list = l.borrow_mut();
                    if idx < 0 || idx as usize >= list.len() {
                        Err(RuntimeError::new(format!("index {idx} out of bounds (length {})", list.len()), span))
                    } else {
                        list[idx as usize] = v.clone();
                        Ok(Flow::Normal(v))
                    }
                }
                (other, _) => Err(RuntimeError::new(format!("cannot index into {other:?}"), span)),
            }
        }
        Expr::Field { base, name } => {
            let b = propagate!(self.eval_expr(base, env));
            match b {
                Value::Record { fields, .. } => {
                    fields.borrow_mut().insert(name, v.clone());
                    Ok(Flow::Normal(v))
                }
                other => Err(RuntimeError::new(format!("cannot assign to a field on {other:?}"), span)),
            }
        }
        other => Err(RuntimeError::new(format!("invalid assignment target: {other:?}"), span)),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Evaluate list literals, indexing, assignment, and field access"
```

---

## Task 8: Struct literal and ADT variant construction at runtime

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree struct_literal_constructs payload_ful_variant nullary_variant_is`
Expected: FAIL — `Expr::Struct` falls into the `eval_expr` catch-all; `Stmt::TypeDecl` falls into the `exec_stmt` catch-all, so `Circle`/`Point` are never declared and evaluating them errors as undefined variables.

- [ ] **Step 3: Implement**

Add to `eval_expr`'s match:

```rust
Expr::Struct { name, fields } => {
    let mut map = rustc_hash::FxHashMap::default();
    for (field_name, value_expr) in fields {
        let v = propagate!(self.eval_expr(value_expr, env));
        map.insert(field_name, v);
    }
    Ok(Flow::Normal(Value::Record { name, fields: Rc::new(RefCell::new(map)) }))
}
```

Replace `exec_stmt`'s `Stmt::TypeDecl { .. }` catch-all arm with real handling (keep the catch-all for `StructDecl`/`Error` only — a struct DECLARATION needs no runtime registration of its own; struct VALUES are built directly by `Expr::Struct`, not via a stored constructor):

```rust
Stmt::TypeDecl { name, variants } => {
    for variant in variants {
        if variant.payload.is_empty() {
            let adt = crate::value::AdtValue { type_name: name, variant: variant.name, fields: Vec::new() };
            Env::declare(env, variant.name, Value::Adt(Rc::new(adt)));
        } else {
            Env::declare(
                env,
                variant.name,
                Value::AdtCtor { type_name: name, variant: variant.name, arity: variant.payload.len() },
            );
        }
    }
    Ok(Flow::Normal(Value::Nil))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Construct struct and ADT variant values at runtime"
```

---

## Task 9: Top-level driver — two-pass `fn` hoisting and the public `interpret()` entry point

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

Task 5 added a minimal `exec_stmt` handling `Stmt::Fn` inline (declare-then-run, no forward-reference support). This task replaces the driver with a proper two-pass version mirroring the resolver's and type checker's own two-pass hoist, so mutually-recursive top-level functions work — matching the established pattern from Phase 4/5.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn mutual_recursion_between_top_level_functions_works() {
    let src = "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }\nis_even(10);";
    let (ast, mut interner, stmts, diags) = ember_parser::parse(src);
    assert!(diags.is_empty());
    let (result, err) = interpret(&ast, &mut interner, &stmts);
    assert!(err.is_none(), "{err:?}");
    assert!(matches!(result, Some(Value::Bool(true))));
}

#[test]
fn interpret_runs_a_whole_program_and_returns_its_final_value() {
    let src = "let x = 1;\nlet y = 2;\nx + y;";
    let (ast, mut interner, stmts, diags) = ember_parser::parse(src);
    assert!(diags.is_empty());
    let (result, err) = interpret(&ast, &mut interner, &stmts);
    assert!(err.is_none(), "{err:?}");
    assert!(matches!(result, Some(Value::Int(3))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree mutual_recursion_between_top_level interpret_runs_a_whole_program`
Expected: FAIL to compile — `interpret` doesn't exist yet.

- [ ] **Step 3: Implement**

Add this free function at the bottom of `interp.rs` (this is `ember-tree`'s equivalent of `ember_types::infer`/`ember_resolve::resolve` — the crate's main pipeline entry point):

```rust
/// Runs a whole program: hoists top-level `fn`s (so mutual recursion
/// works, mirroring the resolver's and type checker's own two-pass hoist),
/// then executes every statement in order. Returns the last statement's
/// value on success, or the first `RuntimeError` encountered — unlike the
/// earlier compile-time passes, execution genuinely stops at the first
/// runtime failure rather than accumulating diagnostics, since there's no
/// meaningful way to "keep going" after one.
pub fn interpret(ast: &Ast, interner: &Interner, stmts: &[Idx<Stmt>]) -> (Option<Value>, Option<RuntimeError>) {
    let mut interp = Interp::new(ast, interner);
    let env = Env::new();

    for &s in stmts {
        if let Stmt::Fn { name, params, body, .. } = ast.stmt(s).clone() {
            let closure = Closure { params: params.iter().map(|p| p.name).collect(), body, env: Rc::clone(&env) };
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
                return (None, Some(RuntimeError::new("break/continue used outside a loop", ast.span_of_stmt(s))));
            }
            Err(e) => return (None, Some(e)),
        }
    }
    (Some(last), None)
}
```

Note top-level `fn` hoisting here pre-declares each closure into the SHARED `env` up front (same pattern as Task 5's minimal version, just now done for ALL top-level fns before any of them run, not one at a time) — since every closure's captured `env` is the same `Rc<RefCell<Env>>` that gets `Env::declare`d into for every OTHER top-level fn too, `is_even`'s closure can see `is_odd` once `is_odd` is declared, even though `is_even` was declared first. This works because closures capture the environment by reference (`Rc`), not by value.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Add two-pass top-level fn hoisting and the public interpret() entry point"
```

---

## Task 10: `pattern.rs` — runtime pattern matching

**Files:**
- Modify: `crates/ember-tree/src/pattern.rs`

Unlike Phase 6's exhaustiveness checker, matching a single concrete value against a single pattern is a straightforward recursive walk — no matrix algorithm needed, since there's no "is this useful against everything above it" question at runtime, only "does this one pattern match this one value."

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use crate::value::Value;
    use ember_ast::Interner;

    fn parse_pattern_in_a_match(src: &str) -> (ember_ast::Ast, Interner, Idx<Pattern>) {
        let full = format!("match x {{ {src} => 1, _ => 2, }}");
        let (ast, interner, stmts, diags) = ember_parser::parse(&full);
        assert!(diags.is_empty(), "diags: {diags:?}");
        let pat = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => match ast.expr(*e) {
                ember_ast::Expr::Match { arms, .. } => arms[0].pat,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        (ast, interner, pat)
    }

    #[test]
    fn wildcard_and_bind_always_match() {
        let (ast, interner, pat) = parse_pattern_in_a_match("_");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
    }

    #[test]
    fn bind_pattern_declares_the_name() {
        let (ast, mut interner, pat) = parse_pattern_in_a_match("y");
        let y = interner.intern("y");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(7), &env));
        assert!(matches!(Env::get(&env, y), Some(Value::Int(7))));
    }

    #[test]
    fn literal_patterns_match_by_value() {
        let (ast, interner, pat) = parse_pattern_in_a_match("0");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(0), &env));
        assert!(!match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
    }

    #[test]
    fn list_pattern_with_rest_destructures() {
        let (ast, mut interner, pat) = parse_pattern_in_a_match("[a, ..rest]");
        let a = interner.intern("a");
        let rest = interner.intern("rest");
        let env = Env::new();
        let list = Value::List(std::rc::Rc::new(std::cell::RefCell::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)])));
        assert!(match_pattern(&ast, &interner, pat, &list, &env));
        assert!(matches!(Env::get(&env, a), Some(Value::Int(1))));
        match Env::get(&env, rest) {
            Some(Value::List(l)) => assert_eq!(l.borrow().len(), 2),
            other => panic!("expected a List, got {other:?}"),
        }
    }

    #[test]
    fn empty_list_pattern_does_not_match_a_nonempty_list() {
        let (ast, interner, pat) = parse_pattern_in_a_match("[]");
        let env = Env::new();
        let list = Value::List(std::rc::Rc::new(std::cell::RefCell::new(vec![Value::Int(1)])));
        assert!(!match_pattern(&ast, &interner, pat, &list, &env));
    }

    #[test]
    fn or_pattern_matches_if_any_alternative_matches() {
        let (ast, interner, pat) = parse_pattern_in_a_match("0 | 1");
        let env = Env::new();
        assert!(match_pattern(&ast, &interner, pat, &Value::Int(1), &env));
        assert!(!match_pattern(&ast, &interner, pat, &Value::Int(2), &env));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree wildcard_and_bind bind_pattern_declares literal_patterns list_pattern_with empty_list_pattern or_pattern_matches`
Expected: FAIL to compile — `match_pattern` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::env::Env;
use crate::value::Value;
use ember_ast::{Ast, Idx, Interner, Pattern};
use std::cell::RefCell;
use std::rc::Rc;

/// Attempts to match `value` against `pat`, binding any names the pattern
/// introduces into `env` as it goes. `Pattern::Tuple` can never succeed —
/// there's no `Value::Tuple` (no way to construct one), the same
/// inertness carried since Phase 5/6, not newly introduced here.
pub fn match_pattern(ast: &Ast, interner: &Interner, pat: Idx<Pattern>, value: &Value, env: &Rc<RefCell<Env>>) -> bool {
    match ast.pat(pat).clone() {
        Pattern::Wild | Pattern::Error => true,
        Pattern::Bind(sym) => {
            Env::declare(env, sym, value.clone());
            true
        }
        Pattern::Int(n) => matches!(value, Value::Int(v) if *v == n),
        Pattern::Float(f) => matches!(value, Value::Float(v) if *v == f),
        Pattern::Bool(b) => matches!(value, Value::Bool(v) if *v == b),
        Pattern::Str(s) => matches!(value, Value::Str(v) if interner.resolve(s) == v.as_str()),
        Pattern::Ctor { name, args } => match value {
            Value::Adt(adt) if adt.variant == name && adt.fields.len() == args.len() => args
                .iter()
                .zip(adt.fields.iter())
                .all(|(&p, v)| match_pattern(ast, interner, p, v, env)),
            _ => false,
        },
        Pattern::Record { name, fields } => match value {
            Value::Record { name: rname, fields: value_fields } if *rname == name => {
                let vf = value_fields.borrow();
                fields.iter().all(|(field_name, pat_idx)| match vf.get(field_name) {
                    Some(v) => match_pattern(ast, interner, *pat_idx, v, env),
                    None => false,
                })
            }
            _ => false,
        },
        Pattern::List { items, rest } => match value {
            Value::List(l) => {
                let list = l.borrow();
                if rest.is_none() {
                    if list.len() != items.len() {
                        return false;
                    }
                } else if list.len() < items.len() {
                    return false;
                }
                for (i, &item_pat) in items.iter().enumerate() {
                    if !match_pattern(ast, interner, item_pat, &list[i], env) {
                        return false;
                    }
                }
                match rest {
                    Some(rest_pat) => {
                        let remaining: Vec<Value> = list[items.len()..].to_vec();
                        let rest_value = Value::List(Rc::new(RefCell::new(remaining)));
                        match_pattern(ast, interner, rest_pat, &rest_value, env)
                    }
                    None => true,
                }
            }
            _ => false,
        },
        Pattern::Or(alts) => alts.iter().any(|&a| match_pattern(ast, interner, a, value, env)),
        Pattern::Tuple(_) => false,
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Add runtime pattern matching with binding extraction"
```

---

## Task 11: `Match` expression evaluation

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree match_dispatches a_false_guard`
Expected: FAIL — `Expr::Match` falls into the `eval_expr` catch-all.

- [ ] **Step 3: Implement**

Add to `eval_expr`'s match. `Expr::Match` is the LAST remaining catch-all case — once this lands, `eval_expr`'s match should be exhaustive against every `Expr` variant with no `_ =>` left (except `Expr::Error`, which stays a deliberate `Ok(Flow::Normal(Value::Nil))` no-op, matching how earlier phases treat `Error` nodes leniently):

```rust
Expr::Match { scrutinee, arms } => self.eval_match(scrutinee, &arms, env, span),
Expr::Error => Ok(Flow::Normal(Value::Nil)),
```

Add this method (needs `use ember_ast::MatchArm;` at the top of the file):

```rust
fn eval_match(&mut self, scrutinee: Idx<Expr>, arms: &[MatchArm], env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
    let value = propagate!(self.eval_expr(scrutinee, env));
    for arm in arms {
        let arm_env = Env::child(env);
        if crate::pattern::match_pattern(self.ast, self.interner, arm.pat, &value, &arm_env) {
            if let Some(guard) = arm.guard {
                match propagate!(self.eval_expr(guard, &arm_env)) {
                    Value::Bool(true) => {}
                    Value::Bool(false) => continue,
                    other => return Err(RuntimeError::new(format!("expected Bool, found {other:?}"), span)),
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
```

Remove the old `_ => Ok(Flow::Normal(Value::Nil))` catch-all from `eval_expr`'s match entirely now that every real variant has its own arm.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged. If the compiler reports `eval_expr`'s match isn't actually exhaustive without a catch-all, that means some `Expr` variant was missed somewhere in Tasks 4-11 — add whatever arm is missing rather than reintroducing a catch-all.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Evaluate match expressions with guard support"
```

---

## Task 12: `natives.rs` — the 8 native functions

**Files:**
- Modify: `crates/ember-tree/src/natives.rs`
- Modify: `crates/ember-tree/src/interp.rs`

**Design note on how natives get looked up:** rather than pre-seeding `Value::Native` bindings into the top-level `Env` (which would need to *intern* each native's name — requiring `&mut Interner` throughout the whole crate just for this one thing), natives are resolved as a **fallback inside `eval_var`**: if `Env::get` finds nothing, check the (already-interned-by-parsing, since any use of `print` in source text is interned by the parser regardless of whether resolve/infer ran) symbol's text against the native name table, using only `&Interner`'s `resolve` — no mutation needed anywhere. This keeps every earlier task's `&Interner` (not `&mut Interner`) signature intact.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/ember-tree/src/natives.rs
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;
    use ember_span::Span;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn span() -> Span {
        Span::new(0, 1)
    }

    #[test]
    fn len_and_push_operate_on_lists() {
        let interner = Interner::new();
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert!(matches!(len(&[list.clone()], span(), &interner), Ok(Value::Int(2))));
        push(&[list.clone(), Value::Int(3)], span(), &interner).unwrap();
        assert!(matches!(len(&[list], span(), &interner), Ok(Value::Int(3))));
    }

    #[test]
    fn int_and_float_convert_between_each_other_and_from_strings() {
        let interner = Interner::new();
        assert!(matches!(int_fn(&[Value::Float(3.9)], span(), &interner), Ok(Value::Int(3))));
        assert!(matches!(float_fn(&[Value::Int(3)], span(), &interner), Ok(Value::Float(f)) if f == 3.0));
        assert!(matches!(int_fn(&[Value::Str(Rc::new("42".to_string()))], span(), &interner), Ok(Value::Int(42))));
        assert!(int_fn(&[Value::Str(Rc::new("abc".to_string()))], span(), &interner).is_err());
    }

    #[test]
    fn type_of_names_every_kind_of_value() {
        let interner = Interner::new();
        assert!(matches!(type_of(&[Value::Int(1)], span(), &interner), Ok(Value::Str(s)) if s.as_str() == "Int"));
        assert!(matches!(type_of(&[Value::Bool(true)], span(), &interner), Ok(Value::Str(s)) if s.as_str() == "Bool"));
    }

    #[test]
    fn lookup_finds_every_native_by_name_with_the_right_arity() {
        let names_and_arities = [
            ("print", 1),
            ("len", 1),
            ("push", 2),
            ("clock", 0),
            ("str", 1),
            ("int", 1),
            ("float", 1),
            ("type_of", 1),
        ];
        for (name, arity) in names_and_arities {
            let native = lookup(name).unwrap_or_else(|| panic!("expected a native named {name}"));
            assert_eq!(native.arity, arity, "wrong arity for {name}");
        }
        assert!(lookup("not_a_real_native").is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree len_and_push int_and_float type_of_names lookup_finds`
Expected: FAIL to compile — none of these functions exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::error::RuntimeError;
use crate::value::{NativeFn, Value};
use ember_ast::Interner;
use ember_span::Span;
use std::rc::Rc;

pub fn print(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    println!("{}", display_value(&args[0], interner));
    Ok(Value::Nil)
}

pub fn len(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Int(l.borrow().len() as i64)),
        other => Err(RuntimeError::new(format!("len expects a list, found {other:?}"), span)),
    }
}

pub fn push(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => {
            l.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(format!("push expects a list, found {other:?}"), span)),
    }
}

pub fn clock(_args: &[Value], _span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Value::Float(now.as_secs_f64()))
}

pub fn str_fn(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    Ok(Value::Str(Rc::new(display_value(&args[0], interner))))
}

pub fn int_fn(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Int"), span)),
        other => Err(RuntimeError::new(format!("cannot convert {other:?} to Int"), span)),
    }
}

pub fn float_fn(args: &[Value], span: Span, _interner: &Interner) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Float"), span)),
        other => Err(RuntimeError::new(format!("cannot convert {other:?} to Float"), span)),
    }
}

pub fn type_of(args: &[Value], _span: Span, interner: &Interner) -> Result<Value, RuntimeError> {
    let name = match &args[0] {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::Nil => "Nil".to_string(),
        Value::Str(_) => "String".to_string(),
        Value::List(_) => "List".to_string(),
        Value::Closure(_) | Value::Native(_) | Value::AdtCtor { .. } => "Function".to_string(),
        Value::Adt(a) => interner.resolve(a.type_name).to_string(),
        Value::Record { name, .. } => interner.resolve(*name).to_string(),
    };
    Ok(Value::Str(Rc::new(name)))
}

fn display_value(v: &Value, interner: &Interner) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(|v| display_value(v, interner)).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::AdtCtor { variant, .. } => format!("<constructor {}>", interner.resolve(*variant)),
        Value::Adt(a) => {
            let name = interner.resolve(a.variant);
            if a.fields.is_empty() {
                name.to_string()
            } else {
                let parts: Vec<String> = a.fields.iter().map(|v| display_value(v, interner)).collect();
                format!("{name}({})", parts.join(", "))
            }
        }
        Value::Record { name, fields } => {
            let name_str = interner.resolve(*name);
            let f = fields.borrow();
            let parts: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{}: {}", interner.resolve(*k), display_value(v, interner)))
                .collect();
            format!("{name_str} {{ {} }}", parts.join(", "))
        }
    }
}

type NativeImpl = fn(&[Value], Span, &Interner) -> Result<Value, RuntimeError>;

const NATIVES: &[(&str, usize, NativeImpl)] = &[
    ("print", 1, print),
    ("len", 1, len),
    ("push", 2, push),
    ("clock", 0, clock),
    ("str", 1, str_fn),
    ("int", 1, int_fn),
    ("float", 1, float_fn),
    ("type_of", 1, type_of),
];

/// Looks up a native by name, constructing a fresh `NativeFn` (cheap — a
/// few fields, no meaningful allocation cost worth caching for this
/// deliberately-simple reference backend).
pub fn lookup(name: &str) -> Option<NativeFn> {
    NATIVES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|&(name, arity, func)| NativeFn { name, arity, func })
}
```

Now update `eval_var` in `interp.rs` to fall back to native lookup when the environment has nothing:

```rust
fn eval_var(&mut self, sym: Symbol, env: &Rc<RefCell<Env>>, span: Span) -> EvalResult {
    if let Some(v) = Env::get(env, sym) {
        return Ok(Flow::Normal(v));
    }
    let name = self.interner.resolve(sym);
    if let Some(native) = crate::natives::lookup(name) {
        return Ok(Flow::Normal(Value::Native(Rc::new(native))));
    }
    Err(RuntimeError::new(format!("undefined variable `{name}`"), span))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged.

- [ ] **Step 5: Manually verify `print`/`push` end-to-end**

Run a quick scratch check (not a unit test, just confirming the fallback-lookup wiring works through a real program):
```bash
cat > /tmp/natives_check.em << 'EOF'
let xs = [1, 2];
push(xs, 3);
print(len(xs));
EOF
```
There's no CLI `run` subcommand yet (that's Task 15) — instead, add a temporary `#[test]` in `interp.rs` calling `interpret()` directly on this source, confirming it runs without error and `len(xs)` evaluates to `Value::Int(3)`, then delete the scratch file and keep (or discard) the test at your discretion — it's redundant with `len_and_push_operate_on_lists` once Task 15's CLI exists, so it's fine to leave it out of the permanent test suite if it's just a manual sanity check.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-tree
git commit -m "Implement the 8 native functions with fallback-lookup dispatch"
```

---

## Task 13: Step-mode — `eval_step`/`StepEvent`

**Files:**
- Modify: `crates/ember-tree/src/interp.rs`

Per the explicit scope decision to include this now. Implemented as a non-invasive wrapper: the existing `eval_expr`/`exec_stmt` (containing the full match logic built up across Tasks 4-11) are renamed to private `_uninstrumented` methods; the public `eval_expr`/`exec_stmt` become thin wrappers that call through and fire the hook — since every recursive call within the existing match arms already just says `self.eval_expr(...)`/`self.exec_stmt(...)`, they automatically route through the new wrapper (and thus get instrumented) without any of Tasks 4-11's code needing to change.

- [ ] **Step 1: Write the failing tests**

```rust
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
    let events: std::rc::Rc<std::cell::RefCell<Vec<StepEvent>>> = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_clone = std::rc::Rc::clone(&events);
    interp.set_step_hook(Box::new(move |e| events_clone.borrow_mut().push(e)));
    let env = crate::env::Env::new();
    for &s in &stmts {
        interp.exec_stmt(s, &env).unwrap();
    }
    let x = interner.intern("x");
    let last_event = events.borrow().last().cloned().expect("expected at least one step event");
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
    assert!(matches!(interp.eval_expr(last, &env).unwrap(), Flow::Normal(Value::Int(3))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree step_hook_receives step_hook_env_snapshot no_hook_installed`
Expected: FAIL to compile — `StepEvent`'s real fields and `set_step_hook` don't exist yet (only Task 4's placeholder `pub struct StepEvent;` does).

- [ ] **Step 3: Implement**

Replace the placeholder `pub struct StepEvent;` from Task 4 with the real definition:

```rust
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
```

Add a setter and the snapshot helper to `impl<'a> Interp<'a>`:

```rust
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
```

Rename the EXISTING `pub fn eval_expr(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult { match self.ast.expr(idx).clone() { ... } }` (the full method built up across Tasks 4-11, containing every `Expr` arm) to `fn eval_expr_uninstrumented(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult` — same body, just the name and visibility change (drop `pub`). Do the same for `exec_stmt`: rename it to `fn exec_stmt_uninstrumented(&mut self, idx: Idx<Stmt>, env: &Rc<RefCell<Env>>) -> EvalResult`.

Add new `pub fn eval_expr`/`pub fn exec_stmt` wrappers in their place:

```rust
pub fn eval_expr(&mut self, idx: Idx<Expr>, env: &Rc<RefCell<Env>>) -> EvalResult {
    let result = self.eval_expr_uninstrumented(idx, env);
    self.fire_step_hook(self.ast.span_of_expr(idx), env, &result);
    result
}

pub fn exec_stmt(&mut self, idx: Idx<Stmt>, env: &Rc<RefCell<Env>>) -> EvalResult {
    let result = self.exec_stmt_uninstrumented(idx, env);
    self.fire_step_hook(self.ast.span_of_stmt(idx), env, &result);
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
        hook(StepEvent { node_span, env_snapshot, result: result_value });
    }
}
```

Since every recursive call inside `eval_expr_uninstrumented`/`exec_stmt_uninstrumented` (built across Tasks 4-11) already just calls `self.eval_expr(...)`/`self.exec_stmt(...)` by name — not a hardcoded `_uninstrumented` suffix — they now automatically route through the new wrappers with no further code changes needed anywhere else in the file.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS. Run `cargo clippy -p ember-tree --all-targets -- -D warnings` and `cargo fmt -p ember-tree -- --check`, fix anything flagged. If `eval_block`, `eval_match`, `call_closure`, or any other helper method calls `self.eval_expr_uninstrumented`/`self.exec_stmt_uninstrumented` directly instead of the public wrapper name, that's a bug — every internal call site should say `self.eval_expr`/`self.exec_stmt` so nested nodes get instrumented too; fix any direct `_uninstrumented` call you find outside the two wrapper functions themselves.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Add step-mode: a StepEvent hook firing after every evaluated node"
```

---

## Task 14: Crate exports

**Files:**
- Modify: `crates/ember-tree/src/natives.rs`
- Modify: `crates/ember-tree/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`display_value` in `natives.rs` (added in Task 12) is currently a private `fn` — the CLI (Task 15) needs to print a program's final value, so it needs to become part of the public surface. Create `crates/ember-tree/tests/public_api.rs`:

```rust
#[test]
fn interpret_and_display_value_are_reachable_from_the_crate_root() {
    let src = "let x = 1;\nlet y = 2;\nx + y;";
    let (ast, mut interner, stmts, diags) = ember_parser::parse(src);
    assert!(diags.is_empty());
    let (result, err) = ember_tree::interpret(&ast, &mut interner, &stmts);
    assert!(err.is_none());
    let value = result.expect("expected a final value");
    assert_eq!(ember_tree::display_value(&value, &interner), "3");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-tree --test public_api`
Expected: FAIL to compile — `ember_tree::display_value` isn't public/re-exported yet (and `interpret`/`Value` may not be either, depending on what Task 9 already exported — check first).

- [ ] **Step 3: Implement**

In `natives.rs`, change `fn display_value(...)` to `pub fn display_value(...)` — no other change to its body.

Replace the contents of `crates/ember-tree/src/lib.rs`:

```rust
pub mod env;
pub mod error;
pub mod interp;
pub mod natives;
pub mod pattern;
pub mod value;

pub use env::Env;
pub use error::RuntimeError;
pub use interp::{interpret, EvalResult, Flow, Interp, StepEvent};
pub use natives::display_value;
pub use value::{AdtValue, Closure, NativeFn, Value};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-tree`
Expected: PASS, including the new integration test. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` — all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-tree
git commit -m "Re-export the interpreter's public API from the crate root"
```

---

## Task 15: `ember-cli run` — the full pipeline, actually executing a program

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the dependency**

Add to `crates/ember-cli/Cargo.toml`'s `[dependencies]` (matching the existing formatting style):
```toml
ember-tree = { path = "../ember-tree" }
```

- [ ] **Step 2: Read `crates/ember-cli/src/main.rs`'s current `run_typecheck` function** to see its exact current shape — it parses, resolves (bail on error), infers (does NOT currently bail on type errors before printing — check this precisely), runs exhaustiveness checking, and prints diagnostics. `run_run` follows the same bail-early chain but stops one step earlier on any error category, since there's no point interpreting a program that didn't even type-check.

- [ ] **Step 3: Implement**

Add a `Run` variant to the `Command` enum:
```rust
/// Parse, resolve, typecheck, check exhaustiveness, then actually run the
/// program, printing its final value or a rendered runtime-error
/// diagnostic.
Run { file: String },
```

Add its dispatch arm in `main`'s match:
```rust
Command::Run { file } => run_run(&file),
```

Add the handler:
```rust
fn run_run(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }

    let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
    resolver.resolve_program(&stmts);
    let resolve_diags = resolver.diagnostics();
    if resolve_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(resolve_diags, path, &src);
    }

    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&infer_diags, path, &src);
    }

    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    if exhaustive_diags.iter().any(|d| d.severity == ember_diag::Severity::Error) {
        return print_diagnostics(&exhaustive_diags, path, &src);
    }

    let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
    if let Some(e) = err {
        let use_color = std::env::var_os("NO_COLOR").is_none();
        println!("{}", ember_diag::render::render(&e.to_diagnostic(), path, &src, use_color));
        return ExitCode::from(2);
    }
    if let Some(v) = result {
        println!("{}", ember_tree::display_value(&v, &interner));
    }
    ExitCode::SUCCESS
}
```

Check `ember_types::infer`'s exact return type (`(TypeInfo, Vec<Diagnostic>)`) and `ember_tree::interpret`'s exact signature (`(ast: &Ast, interner: &Interner, stmts: &[Idx<Stmt>]) -> (Option<Value>, Option<RuntimeError>)`) match what's actually shipped by the time this task runs — both were established in earlier phases/tasks, mirror them exactly rather than re-deriving.

- [ ] **Step 4: Build and manually verify**

Run: `source "$HOME/.cargo/env" && cargo build -p ember-cli` — expect clean build.

Run `cargo run -p ember-cli -- run examples/hello.em` (the `fact`/recursion example) — expect it prints the program's final value with no diagnostics.

Write a small scratch program exercising more of the language (ADT construction, pattern matching, a loop, `push`/`len`) and confirm it runs correctly end-to-end, e.g.:
```
type Shape = | Circle(Float) | Rect(Float, Float);
fn area(s) {
  match s {
    Circle(r) => r * r,
    Rect(w, h) => w * h,
  }
}
let shapes = [Circle(2.0), Rect(3.0, 4.0)];
let mut total = 0.0;
for s in shapes {
  total = total + area(s);
}
print(total);
total;
```
Confirm it prints `16` (4.0 + 12.0) and exits 0. Also verify a program with a runtime error (e.g. `1 / 0;`) prints a rendered diagnostic and exits with code 2, not a Rust panic/backtrace.

- [ ] **Step 5: Run the full verification suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-cli
git commit -m "Add ember run: the full pipeline, actually executing a program"
```

---

## Task 16: Final wrap-up — full verification and CHECKLIST.md update

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Run the full verification suite**

Run: `cargo test --workspace`
Expected: PASS across all 16 crates.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Update `CHECKLIST.md`'s Phase 7 section**

Open `CHECKLIST.md` and go through Phase 7's 20 items line by line, checking `- [x]` for everything this plan actually implemented, following the same honesty standard as every prior phase's wrap-up — verify each line against the real code rather than block-checking. Specifically account for:
- `Value`'s real shape has two additions beyond the literal `SPEC.md` sketch (`Record` gained a `name` field; `AdtCtor` is new) — both justified in the design doc, not silent deviations.
- Index-out-of-bounds as a runtime error category — an addition beyond the checklist's explicit list (stack overflow, integer overflow, division by zero), necessary once list indexing exists with dynamic indices.
- Native functions are dispatched via a fallback lookup inside `eval_var` (checking the resolved name text against a static table) rather than pre-seeded `Value::Native` bindings in the environment — a deliberate design choice to avoid needing `&mut Interner` throughout the crate; functionally equivalent from a program's perspective.
- Step-mode (`eval_step`) was included this round per explicit scope decision, implemented as a synchronous `StepEvent` callback hook on `Interp`, not true async pause/resume — wiring it to an actual interactive debugger UI is later-phase work (LSP/playground), noted as such in the design doc's non-goals.
- `Pattern::Tuple` still can never match (no `Value::Tuple` exists, mirroring the still-inert `Ty::Tuple`/`Expr::Tuple` gap carried since Phase 5/6) — re-confirmed unaffected, not newly introduced or newly fixed here.

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Mark Phase 7 checklist items complete"
```

- [ ] **Step 4: Final confirmation**

Run: `git log --oneline` and confirm a clean, incremental commit history from the crate scaffold through this final checklist update.

---

## Summary of what this plan does NOT cover (by design)

- The bytecode compiler/VM backend (Phase 8/9) — a separate, faster execution path that consumes the resolver's slot allocation; this phase is the reference implementation only.
- Fixing `Pattern::Tuple`'s underlying inertness — carried over from Phase 5/6, still out of scope (a grammar/AST-level change).
- True interactive/async step-through debugging — the synchronous callback hook exists; wiring it to an actual pausable debugger UI is later-phase work (LSP/playground).
- Garbage collection — `Rc`-based reference counting is this backend's whole memory story; the mark-sweep GC (Phase 10) belongs to the bytecode VM.

