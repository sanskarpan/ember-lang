# ember Phase 9 Implementation Plan — Virtual Machine

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `ember`'s bytecode virtual machine (`ember-vm`), executing the `Chunk`/`Op` stream `ember-compile` produces, per `SPEC.md §11`/`§12` and `CHECKLIST.md`'s Phase 9. Closes the loop the conformance suite (Phase 8) exists to check — every fixture must produce identical output on both backends.

**Architecture:** One crate, `ember-vm`, depending on `ember-bytecode`/`ember-ast`. One retroactive one-line-of-substance fix to `ember-bytecode` first (below). No garbage collector until Phase 10 — every heap value is `Rc`/`RefCell`, mirroring `ember-tree`.

**Tech Stack:** Rust, `FxHashMap`, no `unsafe`.

---

## A key implementation-level refinement over the design doc: names are strings at runtime, not `Symbol`s

The design doc sketched `Value::Record { name: Symbol, .. }` and `AdtValue { type_name: Symbol, variant: Symbol, .. }`, matching `ember-tree`'s shape. Working through `OP_GET_GLOBAL`/`OP_GET_FIELD`/`OP_MAKE_RECORD`/`OP_MAKE_ADT`'s actual operands reveals this doesn't fit: every one of those opcodes' name operands is a **constant-pool index into a pooled `Value::Str`** (`ember-compile`'s `name_constant` helper resolved every name to its literal string *at compile time*, precisely so the runtime side never needs a shared `Interner` to make sense of them). So this plan uses `Rc<String>` for every name `ember_vm::Value` carries (`Record.name`, `Record.fields`' keys, `AdtValue.type_name`/`.variant`) — never a `Symbol`. This is a real simplification, not just a rename: it means `display_value`, `values_equal`, and every native function become fully `Interner`-independent. The **one** place a `Symbol` survives into the VM at all is `FunctionProto.name` (baked in at compile time, from `ember-ast::Symbol`) — needed solely to name a function in a stack trace, which is the *only* place the VM ever needs an `Interner` (to resolve that one `Symbol` when rendering a `Diagnostic`).

---

## Task 1: Retroactive `ember-bytecode` fix — `Chunk.functions` becomes `Vec<Rc<FunctionProto>>`

**Files:**
- Modify: `crates/ember-bytecode/src/chunk.rs`

A `Value::Closure` must be able to outlive the function that created it (returned, stored in a list, etc.) — but today a nested function's `FunctionProto` is owned inline inside its parent's `chunk.functions: Vec<FunctionProto>`, with no way to hand out an independently-owned, cheaply-clonable reference to it. Wrapping the pool in `Rc` fixes this with no ripple effect: `add_function`'s signature and every call site in `ember-compile` are unaffected (they only ever use the returned `u16` index), and `ember-bytecode`'s own disassembler (`chunk.functions[idx]`) keeps compiling unchanged via auto-deref from `&Rc<FunctionProto>` to `&FunctionProto`. Verified by grepping every `.functions` reference across both crates before writing this task — the only production-code line that needs to change is the one shown below.

- [ ] **Step 1: Write the failing test**

Add to `crates/ember-bytecode/src/chunk.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn add_function_returns_a_shareable_rc() {
        let mut chunk = Chunk::new();
        let mut interner = ember_ast::Interner::new();
        let proto = FunctionProto {
            chunk: Chunk::new(),
            arity: 0,
            upvalues: vec![],
            name: interner.intern("f"),
        };
        let idx = chunk.add_function(proto);
        let a = Rc::clone(&chunk.functions[idx as usize]);
        let b = Rc::clone(&chunk.functions[idx as usize]);
        assert!(Rc::ptr_eq(&a, &b), "both handles must point at the same allocation");
    }
```

(Add `use std::rc::Rc;` to the test module's `use` list if not already present via `use super::*;` — check what's already imported before duplicating.)

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p ember-bytecode add_function_returns_a_shareable_rc`
Expected: FAIL to compile — `chunk.functions[idx]` is currently a `FunctionProto`, not an `Rc<FunctionProto>`, so `Rc::clone(&chunk.functions[idx])` produces an `Rc<&FunctionProto>`/type mismatch, not what the test expects.

- [ ] **Step 3: Implement**

In `chunk.rs`, add `use std::rc::Rc;` to the top-level imports, change the field:

```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    pub functions: Vec<Rc<FunctionProto>>,
    pub lines: Vec<(u32, u32)>,
}
```

and `add_function`:

```rust
    pub fn add_function(&mut self, proto: FunctionProto) -> u16 {
        self.functions.push(Rc::new(proto));
        (self.functions.len() - 1) as u16
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-bytecode`
Expected: PASS — including every pre-existing test, unchanged. Run `cargo test -p ember-compile` too (a different crate, but one that depends on `ember-bytecode` and touches `chunk.functions` in its own test helpers) to confirm nothing there broke either. Run `cargo clippy -p ember-bytecode -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-bytecode -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-bytecode
git commit -m "Make Chunk's function pool Rc-shareable for the upcoming VM"
```

---

## Task 2: Retroactive `ember-compile` fix — the top-level program must return its last statement's value

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

A second bug in already-merged Phase 8 code, found only now because Phase 8 had no way to *execute* a compiled program and notice: `compile()`'s top-level driver unconditionally emits `OP_NIL` before the final `OP_RETURN`, regardless of what the program's statements actually computed — because `Stmt::ExprStmt` (`compile_stmt`'s own arm) always pops its expression's value, with no special case for "this happens to be the last statement." `ember-tree::interpret` does the opposite: it tracks `last = v` across every top-level statement's result and returns `Some(last)` — meaning a program like `let a = 3; let b = 4; a * a + b * b;` returns `25` on the tree-walker but would silently return `Nil` on the (currently unfixed) VM, breaking conformance for nearly every realistic program. Every later task in this plan that runs a real, compiled program through the VM (starting with Task 11) depends on this being fixed first.

`ember-tree::interp::exec_stmt_uninstrumented` shows exactly what "last statement's value" means for every statement kind: `Stmt::ExprStmt(e)` evaluates to `e`'s own value; every other kind (`Let`, `Fn`, `While`, ...) evaluates to `Value::Nil`. So the fix only needs to special-case `ExprStmt`: if the last non-hoisted top-level statement is an `ExprStmt`, compile its expression *without* popping (bypassing `compile_stmt`'s own `Stmt::ExprStmt` arm entirely, which would pop); for every other kind, compile it normally (through `compile_stmt`, which already leaves the frame's stack net-zero, or net-`+1` for a permanent `Let`/hoisted-declaration slot) and then push an explicit `Nil` alongside it — matching `ember-tree`'s "this statement's own value is `Nil`" for those kinds exactly, while still leaving whatever permanent local it declared alone.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `compiler.rs`, below the existing tests (these call the public `compile()` free function directly, not the `compile_stmt`-driven `compile_program_str` helper other tasks use — this bug lives specifically in `compile()`'s own top-level driver, not in `compile_stmt`):

```rust
    #[test]
    fn the_last_top_level_expression_statements_value_is_the_programs_result() {
        let src = "let a = 3; let b = 4; a * a + b * b;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "{resolve_diags:?}");
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        // Only 0 OP_POPs for the whole program: `a`/`b`'s own Lets don't pop
        // (their value permanently occupies their slot, as always), and the
        // final `a * a + b * b` must NOT be popped either now, unlike an
        // ordinary (non-last) ExprStmt.
        assert!(!out.contains("OP_POP"), "{out}");
        let last_two: Vec<&str> = out.lines().rev().take(2).collect();
        assert!(last_two[0].contains("OP_RETURN"), "{out}");
        assert!(last_two[1].contains("OP_ADD"), "the last computed value must flow straight into Return, not through a Pop/Nil: {out}");
    }

    #[test]
    fn a_program_ending_in_a_non_expression_statement_still_returns_nil() {
        let src = "let mut _x = 1;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "{resolve_diags:?}");
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        let last_two: Vec<&str> = out.lines().rev().take(2).collect();
        assert!(last_two[0].contains("OP_RETURN"), "{out}");
        assert!(last_two[1].contains("OP_NIL"), "a Let as the final statement must still push an explicit Nil result, matching ember-tree's Stmt::Let => Flow::Normal(Value::Nil): {out}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile the_last_top_level a_program_ending_in_a_non_expression`
Expected: FAIL — the first test currently sees an `OP_POP` right after the final `OP_ADD` (the unconditional pop `Stmt::ExprStmt`'s arm always emits) followed by `OP_NIL`/`OP_RETURN`, not `OP_ADD` flowing directly into `OP_RETURN`.

- [ ] **Step 3: Implement**

Replace the two `for &s in stmts { ... }` loops in `compile()` (the current implementation pops every hoisted-vs-not statement into two flat passes) with:

```rust
    for &s in stmts {
        if matches!(
            compiler.ast.stmt(s),
            Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. }
        ) {
            compiler.compile_stmt(s);
        }
    }

    let non_hoisted: Vec<Idx<Stmt>> = stmts
        .iter()
        .copied()
        .filter(|&s| {
            !matches!(
                compiler.ast.stmt(s),
                Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. }
            )
        })
        .collect();

    if non_hoisted.is_empty() {
        compiler.current().emit_op(Op::Nil, 0);
    } else {
        let last_index = non_hoisted.len() - 1;
        for (i, &s) in non_hoisted.iter().enumerate() {
            if i == last_index {
                // The program's result: an ExprStmt's own value flows
                // straight through (compile_expr only, deliberately
                // bypassing compile_stmt's own Stmt::ExprStmt arm, which
                // would pop it) — anything else's "value" is Nil, matching
                // ember-tree::exec_stmt_uninstrumented's own behavior for
                // every non-ExprStmt statement kind.
                if let Stmt::ExprStmt(e) = compiler.ast.stmt(s) {
                    let e = *e;
                    compiler.compile_expr(e);
                } else {
                    compiler.compile_stmt(s);
                    compiler.current().emit_op(Op::Nil, 0);
                }
            } else {
                compiler.compile_stmt(s);
            }
        }
    }
```

(This replaces the existing two-loop body; the hoisting pass above it is unchanged — only the second, non-hoisted pass changes shape. The `Op::Nil`/`Op::Return`/`adjust_depth(-1)` lines immediately after stay exactly as they are: whatever the loop above left on top of the stack — the last `ExprStmt`'s value, an explicit `Nil`, or the empty-program `Nil` — is exactly the one value `Return` expects to pop.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS — including every pre-existing test (in particular, re-run `compile_ends_the_top_level_chunk_with_return` and `compile_handles_mutual_recursion_between_top_level_fns` from the earlier top-level-hoisting task specifically; both should still pass, since neither depends on a *popped* final value). Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Make the top-level program return its last statement's value, matching the tree-walker"
```

---

## Task 3: Scaffold the `ember-vm` crate

**Files:**
- Modify: `crates/ember-vm/Cargo.toml`
- Modify: `crates/ember-vm/src/lib.rs`
- Create: `crates/ember-vm/src/error.rs`, `crates/ember-vm/src/value.rs`, `crates/ember-vm/src/vm.rs`, `crates/ember-vm/src/natives.rs` (empty stubs)

- [ ] **Step 1: Write the manifest**

```toml
[package]
name = "ember-vm"
version.workspace = true
edition.workspace = true

[dependencies]
ember-span = { path = "../ember-span" }
ember-diag = { path = "../ember-diag" }
ember-ast = { path = "../ember-ast" }
ember-bytecode = { path = "../ember-bytecode" }
rustc-hash = "2"

[dev-dependencies]
ember-parser = { path = "../ember-parser" }
ember-resolve = { path = "../ember-resolve" }
ember-compile = { path = "../ember-compile" }
```

(`ember-parser`/`ember-resolve`/`ember-compile` are dev-only — `ember-vm` itself only ever consumes an already-compiled `ember_bytecode::chunk::FunctionProto`; the full pipeline is only needed to build test fixtures.)

- [ ] **Step 2: Declare the module layout**

```rust
pub mod error;
pub mod natives;
pub mod value;
pub mod vm;
```

- [ ] **Step 3: Create empty stubs and verify the build**

```bash
touch crates/ember-vm/src/error.rs crates/ember-vm/src/value.rs crates/ember-vm/src/vm.rs crates/ember-vm/src/natives.rs
```

Run: `cargo build -p ember-vm`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm Cargo.lock
git commit -m "Scaffold ember-vm crate module layout"
```

---

## Task 4: `error.rs` — `RuntimeError` and stack traces

**Files:**
- Modify: `crates/ember-vm/src/error.rs`

Mirrors `ember-tree::RuntimeError`'s shape (message + trace + `to_diagnostic`), adapted for the VM: a `Span` needs a start *and* end byte offset, but `Chunk::line_at` only ever gives back a single `u32` (itself a byte-offset stand-in, per `ember-compile`'s own established convention of using `span_of_expr(idx).start` as an instruction's "line" — there is no real line-mapping table anywhere in this pipeline yet). So this `RuntimeError` stores a raw `u32` position instead of a `Span`, and builds a zero-width `Span::new(line, line)` only at the point it's rendered as a `Diagnostic`. Each trace frame remembers a `Symbol` (the failing call chain's function names) — resolving it to a real string is deferred to `to_diagnostic`, the only place in the whole VM that ever needs an `&Interner`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;

    #[test]
    fn to_diagnostic_carries_the_primary_span() {
        let err = RuntimeError::new("division by zero", 6);
        let interner = Interner::new();
        let diag = err.to_diagnostic(&interner);
        assert_eq!(diag.message, "division by zero");
        assert_eq!(diag.labels[0].span, ember_span::Span::new(6, 6));
    }

    #[test]
    fn trace_frames_become_secondary_labels_with_resolved_names() {
        let mut interner = Interner::new();
        let f = interner.intern("f");
        let g = interner.intern("g");
        let mut err = RuntimeError::new("stack overflow", 1);
        err.trace.push(TraceFrame { function_name: f, line: 10 });
        err.trace.push(TraceFrame { function_name: g, line: 20 });
        let diag = err.to_diagnostic(&interner);
        assert_eq!(diag.labels.len(), 3); // 1 primary + 2 trace frames
        assert!(diag.labels[1].message.contains('f'));
        assert!(diag.labels[2].message.contains('g'));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm to_diagnostic_carries trace_frames_become`
Expected: FAIL to compile — `RuntimeError`/`TraceFrame` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::{Interner, Symbol};
use ember_diag::Diagnostic;
use ember_span::Span;

#[derive(Debug, Clone)]
pub struct TraceFrame {
    pub function_name: Symbol,
    pub line: u32,
}

/// A VM-local runtime error. `line` is a byte-offset stand-in (see this
/// task's own note), not a real 1-based source line — it becomes a
/// zero-width `Span` only when rendered.
#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub message: String,
    pub line: u32,
    /// Enclosing call frames at the point of failure, innermost first —
    /// does NOT include the frame the error itself occurred in (that's
    /// `message`/`line`), matching `ember-tree::RuntimeError.call_stack`'s
    /// own "innermost first, callers only" convention.
    pub trace: Vec<TraceFrame>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, line: u32) -> Self {
        RuntimeError {
            message: message.into(),
            line,
            trace: Vec::new(),
        }
    }

    pub fn to_diagnostic(&self, interner: &Interner) -> Diagnostic {
        let mut diag = Diagnostic::error(self.message.clone())
            .with_primary(Span::new(self.line, self.line), "here");
        for frame in &self.trace {
            diag = diag.with_secondary(
                Span::new(frame.line, frame.line),
                format!("in {}", interner.resolve(frame.function_name)),
            );
        }
        diag
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged. Clippy will likely flag the rest of the still-empty stub files (`value.rs`/`vm.rs`/`natives.rs`) or nothing at all since they're empty — that's expected, they're filled in by later tasks.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Add RuntimeError with stack traces"
```

---

## Task 5: `value.rs` — the runtime `Value`

**Files:**
- Modify: `crates/ember-vm/src/value.rs`

Every name-bearing variant (`Record`'s type name and field keys, `AdtValue`'s type/variant names) uses `Rc<String>`, not `Symbol` — see this plan's header note on why. `ClosureObj` holds an `Rc<FunctionProto>` (Task 1's retroactive fix is what makes this possible) plus its captured upvalues. `NativeFn`'s signature deliberately has **no** `&Interner` parameter — nothing in `display_value`/`values_equal`/a native's own logic needs to resolve a `Symbol`, since every name already arrived pre-resolved to a string from the constant pool.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_equal_compares_structurally_not_by_identity() {
        let a = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        let b = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert!(values_equal(&a, &b), "two separately-built lists with equal contents must compare equal");
    }

    #[test]
    fn values_equal_rejects_different_types() {
        assert!(!values_equal(&Value::Int(1), &Value::Bool(true)));
        assert!(!values_equal(&Value::Nil, &Value::Int(0)));
    }

    #[test]
    fn display_value_formats_every_variant() {
        assert_eq!(display_value(&Value::Nil), "nil");
        assert_eq!(display_value(&Value::Bool(true)), "true");
        assert_eq!(display_value(&Value::Int(42)), "42");
        assert_eq!(display_value(&Value::Str(Rc::new("hi".to_string()))), "hi");
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        assert_eq!(display_value(&list), "[1, 2]");
    }

    #[test]
    fn display_value_formats_a_record_with_its_fields() {
        let mut fields = FxHashMap::default();
        fields.insert(Rc::new("x".to_string()), Value::Int(1));
        let record = Value::Record {
            name: Rc::new("P".to_string()),
            fields: Rc::new(RefCell::new(fields)),
        };
        let out = display_value(&record);
        assert!(out.starts_with("P {"), "{out}");
        assert!(out.contains("x: 1"), "{out}");
    }

    #[test]
    fn display_value_formats_a_nullary_and_a_payload_adt() {
        let nullary = Value::Adt(Rc::new(AdtValue {
            type_name: Rc::new("Shape".to_string()),
            variant: Rc::new("Origin".to_string()),
            fields: vec![],
        }));
        assert_eq!(display_value(&nullary), "Origin");
        let payload = Value::Adt(Rc::new(AdtValue {
            type_name: Rc::new("Shape".to_string()),
            variant: Rc::new("Circle".to_string()),
            fields: vec![Value::Float(1.5)],
        }));
        assert_eq!(display_value(&payload), "Circle(1.5)");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm values_equal display_value`
Expected: FAIL to compile — `Value`/`values_equal`/`display_value`/`AdtValue` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_ast::Symbol;
use ember_bytecode::chunk::FunctionProto;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<ClosureObj>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    Record {
        name: Rc<String>,
        fields: Rc<RefCell<FxHashMap<Rc<String>, Value>>>,
    },
}

pub struct ClosureObj {
    pub proto: Rc<FunctionProto>,
    pub upvalues: Vec<Rc<RefCell<Upvalue>>>,
}

impl std::fmt::Debug for ClosureObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<closure arity={} upvalues={}>", self.proto.arity, self.upvalues.len())
    }
}

#[derive(Debug, Clone)]
pub enum Upvalue {
    /// Still live on the VM stack, at this index.
    Open(usize),
    /// Hoisted to the heap — the stack slot it used to occupy is gone.
    Closed(Value),
}

#[derive(Debug)]
pub struct AdtValue {
    pub type_name: Rc<String>,
    pub variant: Rc<String>,
    pub fields: Vec<Value>,
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value], u32) -> Result<Value, crate::error::RuntimeError>,
}

impl std::fmt::Debug for NativeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<native {}>", self.name)
    }
}

/// Structural equality — `List`/`Record` compare by contents, not by `Rc`
/// pointer identity. Mirrors `ember-tree::values_equal` exactly (both
/// backends must agree on what `==` means for the conformance suite to be
/// meaningful).
pub fn values_equal(a: &Value, b: &Value) -> bool {
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

/// Deliberately takes no `&Interner` — see this file's own header note.
pub fn display_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(display_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::Adt(a) => {
            if a.fields.is_empty() {
                a.variant.to_string()
            } else {
                let parts: Vec<String> = a.fields.iter().map(display_value).collect();
                format!("{}({})", a.variant, parts.join(", "))
            }
        }
        Value::Record { name, fields } => {
            let f = fields.borrow();
            let parts: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{k}: {}", display_value(v)))
                .collect();
            format!("{name} {{ {} }}", parts.join(", "))
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Add the runtime Value type"
```

---

## Task 6: `Vm`/`CallFrame` skeleton — stack ops, read helpers, and the dispatch loop

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

This is the foundational scaffolding every later task adds opcode handlers to: `CallFrame`/`Vm`, the byte-reading helpers, and a `step`/`run` pair covering just enough opcodes (`Constant`/`Nil`/`True`/`False`/`Pop`/`Return`) to execute a genuinely hand-built chunk end to end — including a real function return, which is what makes this the right place to also nail down **step mode**, since `step()` is the one place the whole dispatch loop lives.

**`step`/`run` shape:** `step(&mut self) -> Result<StepOutcome, RuntimeError>`, where `StepOutcome` is `Running` or `Done(Value)` — folding the error case into the `Result` (rather than a three-way `Running`/`Done`/`Error` enum) is what lets every opcode handler use `?` for its own fallible logic instead of manually matching and re-wrapping at every call site. `run` is then just:

```rust
pub fn run(&mut self) -> Result<Value, RuntimeError> {
    loop {
        match self.step()? {
            StepOutcome::Running => continue,
            StepOutcome::Done(v) => return Ok(v),
        }
    }
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;
    use ember_bytecode::chunk::{Chunk, FunctionProto};
    use ember_bytecode::op::Op;

    fn script(build: impl FnOnce(&mut Chunk)) -> FunctionProto {
        let mut chunk = Chunk::new();
        build(&mut chunk);
        let mut interner = Interner::new();
        FunctionProto {
            chunk,
            arity: 0,
            upvalues: vec![],
            name: interner.intern("<script>"),
        }
    }

    #[test]
    fn a_constant_pushed_then_returned_is_the_programs_result() {
        let proto = script(|c| {
            let idx = c.add_constant(ember_bytecode::value::Value::Int(42));
            c.write_op(Op::Constant, 1);
            c.write_u16(idx, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(42)));
    }

    #[test]
    fn nil_true_false_and_pop_all_execute() {
        let proto = script(|c| {
            c.write_op(Op::Nil, 1);
            c.write_op(Op::Pop, 1);
            c.write_op(Op::True, 1);
            c.write_op(Op::Pop, 1);
            c.write_op(Op::False, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Bool(false)));
    }

    #[test]
    fn step_executes_exactly_one_instruction_at_a_time() {
        let proto = script(|c| {
            c.write_op(Op::Nil, 1);
            c.write_op(Op::True, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        assert!(matches!(vm.step(), Ok(StepOutcome::Running)));
        assert_eq!(vm.stack_len_for_test(), 1);
        assert!(matches!(vm.step(), Ok(StepOutcome::Running)));
        assert_eq!(vm.stack_len_for_test(), 2);
        match vm.step() {
            Ok(StepOutcome::Done(Value::Bool(true))) => {}
            other => panic!("expected Done(Bool(true)), got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL to compile — `Vm`, `CallFrame`, `StepOutcome`, `Value` (re-exported), `stack_len_for_test` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::error::RuntimeError;
use crate::value::{ClosureObj, Value};
use ember_bytecode::chunk::{Chunk, FunctionProto};
use ember_bytecode::op::Op;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

/// Frame-depth cap — a sanity limit against runaway/infinite recursion,
/// not a native-stack-overflow guard. The dispatch loop below is
/// genuinely iterative (a `while` loop reading one opcode at a time), not
/// recursive, so unlike `ember-tree`'s `MAX_CALL_DEPTH`, deep `ember`
/// recursion never threatens the *host* Rust stack — this cap exists only
/// to turn an infinitely-recursive `ember` program into a diagnostic
/// instead of unbounded `frames`/`stack` growth.
const MAX_FRAMES: usize = 1000;

pub struct CallFrame {
    pub closure: Rc<ClosureObj>,
    pub ip: usize,
    pub slot_base: usize,
}

pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    globals: FxHashMap<Rc<String>, Value>,
    open_upvalues: Vec<Rc<RefCell<crate::value::Upvalue>>>,
}

pub enum StepOutcome {
    Running,
    Done(Value),
}

impl Vm {
    pub fn new(script: FunctionProto) -> Self {
        let proto = Rc::new(script);
        let closure = Rc::new(ClosureObj { proto, upvalues: Vec::new() });
        let frame = CallFrame { closure, ip: 0, slot_base: 0 };
        Vm {
            stack: Vec::new(),
            frames: vec![frame],
            globals: FxHashMap::default(),
            open_upvalues: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn stack_len_for_test(&self) -> usize {
        self.stack.len()
    }

    fn frame(&self) -> &CallFrame {
        self.frames.last().expect("at least one frame while running")
    }

    fn frame_mut(&mut self) -> &mut CallFrame {
        self.frames.last_mut().expect("at least one frame while running")
    }

    fn chunk(&self) -> &Chunk {
        &self.frame().closure.proto.chunk
    }

    fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    fn pop(&mut self) -> Value {
        self.stack.pop().expect("stack underflow — a compiler bug, not a user error")
    }

    fn peek(&self, distance_from_top: usize) -> &Value {
        let len = self.stack.len();
        &self.stack[len - 1 - distance_from_top]
    }

    fn read_u8(&mut self) -> u8 {
        let ip = self.frame().ip;
        let byte = self.chunk().code[ip];
        self.frame_mut().ip += 1;
        byte
    }

    fn read_op(&mut self) -> Op {
        let byte = self.read_u8();
        Op::from_u8(byte).expect("invalid opcode byte in compiled chunk")
    }

    fn read_u16(&mut self) -> u16 {
        let hi = self.read_u8();
        let lo = self.read_u8();
        u16::from_be_bytes([hi, lo])
    }

    /// Reads a `u16` constant-pool index and converts the pooled
    /// `ember_bytecode::value::Value` into a runtime `Value`. `Str`
    /// shares its `Rc<String>` directly (both `Value` types use the exact
    /// same representation for strings) rather than re-allocating.
    fn read_constant(&mut self) -> Value {
        let idx = self.read_u16();
        Self::const_to_value(&self.chunk().constants[idx as usize])
    }

    fn const_to_value(c: &ember_bytecode::value::Value) -> Value {
        match c {
            ember_bytecode::value::Value::Nil => Value::Nil,
            ember_bytecode::value::Value::Bool(b) => Value::Bool(*b),
            ember_bytecode::value::Value::Int(n) => Value::Int(*n),
            ember_bytecode::value::Value::Float(f) => Value::Float(*f),
            ember_bytecode::value::Value::Str(s) => Value::Str(Rc::clone(s)),
        }
    }

    /// Builds a `RuntimeError` at the current position, with a trace of
    /// every *enclosing* frame (innermost first) — not including the
    /// frame the error itself occurred in, matching `error.rs`'s own
    /// documented convention. `ip.saturating_sub(1)` accounts for
    /// `read_op`/`read_u8`/`read_u16` already having advanced `ip` past
    /// the current instruction's opcode (and any operands) by the time an
    /// error is detected — every byte of one instruction shares the same
    /// recorded line, so pointing at any of them resolves correctly.
    fn runtime_error(&self, message: impl Into<String>) -> RuntimeError {
        let line = self.chunk().line_at(self.frame().ip.saturating_sub(1));
        let trace = self.frames[..self.frames.len() - 1]
            .iter()
            .rev()
            .map(|f| crate::error::TraceFrame {
                function_name: f.closure.proto.name,
                line: f.closure.proto.chunk.line_at(f.ip.saturating_sub(1)),
            })
            .collect();
        RuntimeError { message: message.into(), line, trace }
    }

    pub fn step(&mut self) -> Result<StepOutcome, RuntimeError> {
        let op = self.read_op();
        match op {
            Op::Constant => {
                let v = self.read_constant();
                self.push(v);
            }
            Op::Nil => self.push(Value::Nil),
            Op::True => self.push(Value::Bool(true)),
            Op::False => self.push(Value::Bool(false)),
            Op::Pop => {
                self.pop();
            }
            Op::Return => {
                let result = self.pop();
                let frame = self.frames.pop().expect("frame present");
                self.close_upvalues(frame.slot_base);
                if self.frames.is_empty() {
                    return Ok(StepOutcome::Done(result));
                }
                // `frame.slot_base - 1` removes the callee itself (which
                // sits one slot below where the new frame's own locals
                // started) along with every arg/local it pushed — see
                // Task 10's own note for the full reasoning behind the `-1`.
                self.stack.truncate(frame.slot_base - 1);
                self.push(result);
            }
            other => unimplemented!("Vm::step: {other:?} — added in a later task"),
        }
        Ok(StepOutcome::Running)
    }

    pub fn run(&mut self) -> Result<Value, RuntimeError> {
        loop {
            match self.step()? {
                StepOutcome::Running => continue,
                StepOutcome::Done(v) => return Ok(v),
            }
        }
    }

    /// Placeholder until Task 11 builds the real version — needed only so
    /// `Op::Return` (Task 6) and `Op::CloseUpvalue`/`OP_CALL`'s cleanup
    /// (later tasks) share one implementation from the start rather than
    /// duplicating the close logic when upvalues are added.
    fn close_upvalues(&mut self, _from: usize) {}
}
```

Note the `other => unimplemented!(...)` catch-all and the temporary no-op `close_upvalues` stub: every remaining `Op` variant is added to `step`'s `match` by later tasks (mirroring exactly how `ember-compile`'s own `compile_expr`/`compile_stmt` catch-alls were filled in incrementally across Phase 8) — this is a deliberate, temporary "not yet reached" arm, not a silent placeholder. `close_upvalues` becomes a real implementation in Task 11; until then, the `Return` path above already calls it (with nothing to do yet, since no opcode can create an open upvalue before Task 11 exists).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged (the `#[cfg(test)] pub(crate) fn stack_len_for_test` may need `#[allow(dead_code)]` removed/added depending on whether clippy sees it as reachable from the test module in the same file — check what's actually flagged rather than guessing).

Also add `pub use value::Value;` and `pub use vm::{StepOutcome, Vm};` (and `pub use error::RuntimeError;`) to `crates/ember-vm/src/lib.rs`'s module declarations, so downstream code (Task 14's CLI wiring, this crate's own future integration tests) doesn't need to write `ember_vm::value::Value` everywhere.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Add the Vm skeleton with stack ops, read helpers, and step/run"
```

---

## Task 7: Arithmetic, comparison, equality, and unary operators

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

Mirrors `ember-tree::interp::apply_binary`/`eval_unary` exactly — same checked-arithmetic overflow/div-by-zero handling, same type-error rejection — since both backends must reject (and accept) identical programs for the conformance suite to mean anything. `ember-compile`'s `!=`/`<=`/`>=` desugaring (`Equal`+`Not`, `Greater`+`Not`, `Less`+`Not`) means the VM itself only ever needs to implement `Equal`/`Greater`/`Less` directly — there's no `OP_NOT_EQUAL` etc. to handle.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    fn run_ops(build: impl FnOnce(&mut Chunk)) -> Result<Value, RuntimeError> {
        let proto = script(|c| {
            build(c);
            c.write_op(Op::Return, 1);
        });
        Vm::new(proto).run()
    }

    fn int_const(c: &mut Chunk, n: i64) -> u16 {
        c.add_constant(ember_bytecode::value::Value::Int(n))
    }

    #[test]
    fn integer_addition() {
        let result = run_ops(|c| {
            let a = int_const(c, 3);
            let b = int_const(c, 4);
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(b, 1);
            c.write_op(Op::Add, 1);
        });
        assert!(matches!(result, Ok(Value::Int(7))));
    }

    #[test]
    fn integer_division_by_zero_is_a_runtime_error() {
        let result = run_ops(|c| {
            let a = int_const(c, 1);
            let b = int_const(c, 0);
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(b, 1);
            c.write_op(Op::Div, 1);
        });
        assert!(result.is_err());
    }

    #[test]
    fn integer_overflow_is_a_runtime_error_not_a_panic() {
        let result = run_ops(|c| {
            let a = int_const(c, i64::MAX);
            let b = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(b, 1);
            c.write_op(Op::Add, 1);
        });
        assert!(result.is_err());
    }

    #[test]
    fn adding_two_strings_concatenates() {
        let result = run_ops(|c| {
            let a = c.add_constant(ember_bytecode::value::Value::Str(Rc::new("foo".to_string())));
            let b = c.add_constant(ember_bytecode::value::Value::Str(Rc::new("bar".to_string())));
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(b, 1);
            c.write_op(Op::Add, 1);
        });
        match result {
            Ok(Value::Str(s)) => assert_eq!(*s, "foobar"),
            other => panic!("expected Str(\"foobar\"), got {other:?}"),
        }
    }

    #[test]
    fn mismatched_operand_types_are_a_runtime_error() {
        let result = run_ops(|c| {
            let a = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::True, 1);
            c.write_op(Op::Add, 1);
        });
        assert!(result.is_err());
    }

    #[test]
    fn equal_compares_structurally_and_never_type_errors() {
        let result = run_ops(|c| {
            c.write_op(Op::True, 1);
            let s = c.add_constant(ember_bytecode::value::Value::Str(Rc::new("x".to_string())));
            c.write_op(Op::Constant, 1);
            c.write_u16(s, 1);
            c.write_op(Op::Equal, 1);
        });
        assert!(matches!(result, Ok(Value::Bool(false))));
    }

    #[test]
    fn comparison_and_negation() {
        let result = run_ops(|c| {
            let a = int_const(c, 5);
            c.write_op(Op::Constant, 1);
            c.write_u16(a, 1);
            c.write_op(Op::Negate, 1);
            let b = int_const(c, 3);
            c.write_op(Op::Constant, 1);
            c.write_u16(b, 1);
            c.write_op(Op::Less, 1);
        });
        assert!(matches!(result, Ok(Value::Bool(true)))); // -5 < 3
    }

    #[test]
    fn logical_not() {
        let result = run_ops(|c| {
            c.write_op(Op::True, 1);
            c.write_op(Op::Not, 1);
        });
        assert!(matches!(result, Ok(Value::Bool(false))));
    }
}
```

(`use std::rc::Rc;` should already be reachable via `use super::*;`, matching the module's own top-level import — check what's already imported into the test module before adding a duplicate.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — `Op::Add`/`Sub`/`Mul`/`Div`/`Mod`/`Equal`/`Greater`/`Less`/`Not`/`Negate` all fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match` (find the `other => unimplemented!(...)` arm and add these named arms right before it):

```rust
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod => {
                let b = self.pop();
                let a = self.pop();
                let v = self.binary_arith(op, a, b)?;
                self.push(v);
            }
            Op::Equal => {
                let b = self.pop();
                let a = self.pop();
                self.push(Value::Bool(crate::value::values_equal(&a, &b)));
            }
            Op::Greater | Op::Less => {
                let b = self.pop();
                let a = self.pop();
                let v = self.compare(op, a, b)?;
                self.push(v);
            }
            Op::Not => {
                let a = self.pop();
                match a {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => return Err(self.runtime_error(format!("expected Bool, found {other:?}"))),
                }
            }
            Op::Negate => {
                let a = self.pop();
                match a {
                    Value::Int(n) => match n.checked_neg() {
                        Some(r) => self.push(Value::Int(r)),
                        None => return Err(self.runtime_error(format!("integer overflow negating {n}"))),
                    },
                    Value::Float(f) => self.push(Value::Float(-f)),
                    other => return Err(self.runtime_error(format!("invalid operand for unary -: {other:?}"))),
                }
            }
```

Add these methods to `impl Vm`:

```rust
    fn binary_arith(&self, op: Op, l: Value, r: Value) -> Result<Value, RuntimeError> {
        let overflow = |op_name: &str, a: i64, b: i64| {
            self.runtime_error(format!("integer overflow: {a} {op_name} {b}"))
        };
        match (op, l, r) {
            (Op::Add, Value::Int(a), Value::Int(b)) => {
                a.checked_add(b).map(Value::Int).ok_or_else(|| overflow("+", a, b))
            }
            (Op::Sub, Value::Int(a), Value::Int(b)) => {
                a.checked_sub(b).map(Value::Int).ok_or_else(|| overflow("-", a, b))
            }
            (Op::Mul, Value::Int(a), Value::Int(b)) => {
                a.checked_mul(b).map(Value::Int).ok_or_else(|| overflow("*", a, b))
            }
            (Op::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(self.runtime_error("division by zero"));
                }
                a.checked_div(b).map(Value::Int).ok_or_else(|| overflow("/", a, b))
            }
            (Op::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(self.runtime_error("division by zero"));
                }
                a.checked_rem(b).map(Value::Int).ok_or_else(|| overflow("%", a, b))
            }
            (Op::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Op::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Op::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Op::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Op::Add, Value::Str(a), Value::Str(b)) => Ok(Value::Str(Rc::new(format!("{a}{b}")))),
            (op, l, r) => Err(self.runtime_error(format!("invalid operands for {op:?}: {l:?}, {r:?}"))),
        }
    }

    fn compare(&self, op: Op, l: Value, r: Value) -> Result<Value, RuntimeError> {
        match (op, l, r) {
            (Op::Less, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (Op::Greater, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (Op::Less, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            (Op::Greater, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            (op, l, r) => Err(self.runtime_error(format!("invalid operands for {op:?}: {l:?}, {r:?}"))),
        }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute arithmetic, comparison, equality, and unary operators"
```

---

## Task 8: Locals and globals — `OP_GET_LOCAL`/`OP_SET_LOCAL`/`OP_GET_GLOBAL`/`OP_SET_GLOBAL`/`OP_DEFINE_GLOBAL`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

Locals are a direct indexed array access into `self.stack[frame.slot_base + slot]` — CHECKLIST's own "the whole speed story vs. the tree-walker's hash-map-in-a-chain `Env`". Globals are keyed by `Rc<String>` (the pooled name, read straight from the constant pool — see this plan's header note), never re-interned into a `Symbol`.

`OP_SET_LOCAL`/`OP_SET_GLOBAL` **peek, don't pop** — matching `ember-compile`'s own static-effect table (`SetLocal`/`SetGlobal` are both `Some(0)`), since assignment is an expression that evaluates to the assigned value. `OP_DEFINE_GLOBAL` **does** pop (`Some(-1)`) — it's not an assignment expression, it's the one-time act of publishing a top-level declaration's already-permanently-stored local value into the globals table too (`ember-compile`'s top-level dual-registration design).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn get_local_and_set_local_read_and_write_the_frame_relative_slot() {
        let result = run_ops(|c| {
            let a = int_const(c, 1);
            c.write_op(Op::Constant, 1); // slot 0
            c.write_u16(a, 1);
            let b = int_const(c, 99);
            c.write_op(Op::Constant, 1); // pushes the new value to assign
            c.write_u16(b, 1);
            c.write_op(Op::SetLocal, 1); // slot 0 = 99, leaves 99 on top too
            c.write_u8(0, 1);
            c.write_op(Op::Pop, 1); // discard the SetLocal duplicate
            c.write_op(Op::GetLocal, 1);
            c.write_u8(0, 1);
        });
        assert!(matches!(result, Ok(Value::Int(99))));
    }

    #[test]
    fn define_get_and_set_global_round_trip_by_pooled_name() {
        let result = run_ops(|c| {
            let name = c.add_constant(ember_bytecode::value::Value::Str(Rc::new("x".to_string())));
            let one = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(one, 1);
            c.write_op(Op::DefineGlobal, 1);
            c.write_u16(name, 1);

            let two = int_const(c, 2);
            c.write_op(Op::Constant, 1);
            c.write_u16(two, 1);
            c.write_op(Op::SetGlobal, 1);
            c.write_u16(name, 1);
            c.write_op(Op::Pop, 1); // discard the SetGlobal duplicate

            c.write_op(Op::GetGlobal, 1);
            c.write_u16(name, 1);
        });
        assert!(matches!(result, Ok(Value::Int(2))));
    }

    #[test]
    fn reading_an_undefined_global_is_a_runtime_error() {
        let result = run_ops(|c| {
            let name = c.add_constant(ember_bytecode::value::Value::Str(Rc::new("nope".to_string())));
            c.write_op(Op::GetGlobal, 1);
            c.write_u16(name, 1);
        });
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — these five opcodes fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match`:

```rust
            Op::GetLocal => {
                let slot = self.read_u8() as usize;
                let base = self.frame().slot_base;
                self.push(self.stack[base + slot].clone());
            }
            Op::SetLocal => {
                let slot = self.read_u8() as usize;
                let base = self.frame().slot_base;
                let v = self.peek(0).clone();
                self.stack[base + slot] = v;
            }
            Op::GetGlobal => {
                let name = self.read_global_name();
                match self.globals.get(&name) {
                    Some(v) => {
                        let v = v.clone();
                        self.push(v);
                    }
                    None => return Err(self.runtime_error(format!("undefined global `{name}`"))),
                }
            }
            Op::SetGlobal => {
                let name = self.read_global_name();
                if !self.globals.contains_key(&name) {
                    return Err(self.runtime_error(format!("undefined global `{name}`")));
                }
                let v = self.peek(0).clone();
                self.globals.insert(name, v);
            }
            Op::DefineGlobal => {
                let name = self.read_global_name();
                let v = self.pop();
                self.globals.insert(name, v);
            }
```

Add this helper to `impl Vm`:

```rust
    /// Reads a `u16` constant-pool index and expects the pooled value to
    /// be a `Str` — every global/field/type/variant name operand in the
    /// whole `Op` set is pooled this way (see this plan's header note).
    /// Panics on a non-`Str` constant: that would mean `ember-compile`
    /// emitted a name operand pointing at the wrong kind of constant, a
    /// compiler bug this crate has no responsibility to recover from.
    fn read_global_name(&mut self) -> Rc<String> {
        let idx = self.read_u16();
        match &self.chunk().constants[idx as usize] {
            ember_bytecode::value::Value::Str(s) => Rc::clone(s),
            other => panic!("name constant must be a string, found {other:?}"),
        }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute local and global variable access"
```

---

## Task 9: Jump instructions — `OP_JUMP`/`OP_JUMP_IF_FALSE`/`OP_JUMP_IF_TRUE`/`OP_LOOP`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

`ember-compile`'s disassembler already resolves jump offsets to absolute target addresses for humans to read (Phase 8); the VM does the identical arithmetic at runtime to actually move `ip`. `JumpIfFalse`/`JumpIfTrue` always pop the value they test, unconditionally — matching `ember-compile`'s own static-effect table (`Some(-1)` for both, regardless of which way the branch goes).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn jump_if_false_skips_the_true_branch_bytes() {
        // Equivalent to: if false { 1 } else { 2 }
        let result = run_ops(|c| {
            c.write_op(Op::False, 1);
            let to_else = c.code.len();
            c.write_op(Op::JumpIfFalse, 1);
            c.write_u16(0xFFFF, 1);
            let one = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(one, 1);
            let to_end = c.code.len();
            c.write_op(Op::Jump, 1);
            c.write_u16(0xFFFF, 1);
            let else_start = c.code.len();
            let two = int_const(c, 2);
            c.write_op(Op::Constant, 1);
            c.write_u16(two, 1);
            let end = c.code.len();
            let else_offset = (else_start - to_else - 3) as u16;
            c.code[to_else + 1] = (else_offset >> 8) as u8;
            c.code[to_else + 2] = else_offset as u8;
            let end_offset = (end - to_end - 3) as u16;
            c.code[to_end + 1] = (end_offset >> 8) as u8;
            c.code[to_end + 2] = end_offset as u8;
        });
        assert!(matches!(result, Ok(Value::Int(2))));
    }

    #[test]
    fn loop_jumps_backward() {
        // A hand-built "while counter < 3 { counter = counter + 1 }; counter"
        // using slot 0 as the counter, exercising OP_LOOP's backward jump.
        let result = run_ops(|c| {
            let zero = int_const(c, 0);
            c.write_op(Op::Constant, 1); // slot 0 = 0
            c.write_u16(zero, 1);

            let loop_start = c.code.len();
            c.write_op(Op::GetLocal, 1);
            c.write_u8(0, 1);
            let three = int_const(c, 3);
            c.write_op(Op::Constant, 1);
            c.write_u16(three, 1);
            c.write_op(Op::Less, 1);
            let to_end = c.code.len();
            c.write_op(Op::JumpIfFalse, 1);
            c.write_u16(0xFFFF, 1);

            c.write_op(Op::GetLocal, 1);
            c.write_u8(0, 1);
            let one = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(one, 1);
            c.write_op(Op::Add, 1);
            c.write_op(Op::SetLocal, 1);
            c.write_u8(0, 1);
            c.write_op(Op::Pop, 1);

            c.write_op(Op::Loop, 1);
            let after_loop_operand = c.code.len() + 2;
            let loop_offset = (after_loop_operand - loop_start) as u16;
            c.write_u16(loop_offset, 1);

            let end = c.code.len();
            let end_offset = (end - to_end - 3) as u16;
            c.code[to_end + 1] = (end_offset >> 8) as u8;
            c.code[to_end + 2] = end_offset as u8;

            c.write_op(Op::GetLocal, 1);
            c.write_u8(0, 1);
        });
        assert!(matches!(result, Ok(Value::Int(3))));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — `Jump`/`JumpIfFalse`/`JumpIfTrue`/`Loop` fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match`:

```rust
            Op::Jump => {
                let offset = self.read_u16();
                self.frame_mut().ip += offset as usize;
            }
            Op::JumpIfFalse => {
                let offset = self.read_u16();
                let cond = self.pop();
                let is_false = matches!(cond, Value::Bool(false));
                if is_false {
                    self.frame_mut().ip += offset as usize;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_u16();
                let cond = self.pop();
                let is_true = matches!(cond, Value::Bool(true));
                if is_true {
                    self.frame_mut().ip += offset as usize;
                }
            }
            Op::Loop => {
                let offset = self.read_u16();
                self.frame_mut().ip -= offset as usize;
            }
```

`JumpIfFalse`/`JumpIfTrue` deliberately treat a non-`Bool` condition as simply "not the tested value" rather than a type error — by the time this phase runs, `ember-types`' checker has already rejected any program where a condition isn't a `Bool` (matching `ember-tree::eval_if`'s own trust in the type-checked pipeline having already ruled this out); a genuinely malformed condition here would indicate a compiler bug upstream, not a user-facing runtime error to construct a nice diagnostic for.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute jump instructions"
```

---

## Task 10: Function calls — `OP_CALL`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

**The callee-cleanup subtlety, worked through concretely because it's easy to get subtly wrong:** before `OP_CALL` executes, the stack looks like `[..., callee, arg0, arg1, ..., arg(argc-1)]`. `ember-compile`'s params start at local slot 0 (`compile_function` does `push_local(Some(i))` for each param starting at `i = 0`), so `arg0` must land at `stack[frame.slot_base + 0]` — meaning **`frame.slot_base` must equal `arg0`'s own stack position**, `stack.len() - argc`, not the callee's position. That leaves the callee sitting at `slot_base - 1`, one slot *below* the new frame's own locals — outside anything the new frame's addressing ever touches. Task 6's `Op::Return` already accounts for this: when a non-top-level frame returns, it truncates to `frame.slot_base - 1`, not `frame.slot_base` — removing the callee along with every arg/local the call pushed, so exactly one value (the result) remains where the whole `f(a, b)` call expression started. Get the `- 1` wrong (or omit it) and every call leaves a stray leftover closure value sitting on the stack forever.

Calling a `Value::Native` is simpler — natives execute synchronously with no `CallFrame`/`ip` of their own, so `OP_CALL` does the stack cleanup itself (same `args_start - 1` truncate-then-push-result shape) rather than relying on a future `OP_RETURN`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    fn callee_proto(interner: &mut Interner) -> FunctionProto {
        // fn(n) { n + 1 } — param `n` is local slot 0.
        let mut chunk = Chunk::new();
        chunk.write_op(Op::GetLocal, 1);
        chunk.write_u8(0, 1);
        let one = chunk.add_constant(ember_bytecode::value::Value::Int(1));
        chunk.write_op(Op::Constant, 1);
        chunk.write_u16(one, 1);
        chunk.write_op(Op::Add, 1);
        chunk.write_op(Op::Return, 1);
        FunctionProto { chunk, arity: 1, upvalues: vec![], name: interner.intern("callee") }
    }

    #[test]
    fn calling_a_closure_pushes_a_frame_and_the_result_replaces_the_whole_call() {
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner));
        let closure = Value::Closure(Rc::new(ClosureObj { proto: callee, upvalues: vec![] }));

        let proto = script(|c| {
            let five = c.add_constant(ember_bytecode::value::Value::Int(5));
            c.write_op(Op::Constant, 1);
            c.write_u16(five, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(1, 1);
        });
        let mut vm = Vm::new(proto);
        // Test-only: seed the stack with the callee BEFORE execution starts,
        // exactly where the script's own bytecode expects to find it (this
        // sidesteps needing OP_CLOSURE, built in the next task, just to
        // exercise OP_CALL's own frame mechanics in isolation).
        vm.push_for_test(closure);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(6)));
        assert_eq!(vm.stack_len_for_test(), 0, "the call must leave exactly its result and nothing else");
    }

    #[test]
    fn calling_with_the_wrong_arity_is_a_runtime_error() {
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner)); // expects 1 arg
        let closure = Value::Closure(Rc::new(ClosureObj { proto: callee, upvalues: vec![] }));
        let proto = script(|c| {
            c.write_op(Op::Call, 1);
            c.write_u8(0, 1); // called with 0 args instead of 1
        });
        let mut vm = Vm::new(proto);
        vm.push_for_test(closure);
        assert!(vm.run().is_err());
    }

    #[test]
    fn calling_a_non_callable_value_is_a_runtime_error() {
        let result = run_ops(|c| {
            let n = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(n, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(0, 1);
        });
        assert!(result.is_err());
    }

    #[test]
    fn calling_a_native_dispatches_immediately_with_no_extra_frame() {
        fn double(args: &[Value], _line: u32) -> Result<Value, RuntimeError> {
            match args[0] {
                Value::Int(n) => Ok(Value::Int(n * 2)),
                _ => unreachable!(),
            }
        }
        let native = Value::Native(Rc::new(crate::value::NativeFn { name: "double", arity: 1, func: double }));
        let proto = script(|c| {
            let five = c.add_constant(ember_bytecode::value::Value::Int(5));
            c.write_op(Op::Constant, 1);
            c.write_u16(five, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(1, 1);
        });
        let mut vm = Vm::new(proto);
        vm.push_for_test(native);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(10)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL to compile — `push_for_test` doesn't exist yet, and `Op::Call` falls through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add the test-only stack-seeding helper (near `stack_len_for_test` in `impl Vm`):

```rust
    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, v: Value) {
        self.push(v);
    }
```

Add this arm to `step`'s `match`:

```rust
            Op::Call => {
                let argc = self.read_u8() as usize;
                let callee = self.peek(argc).clone();
                match callee {
                    Value::Closure(c) => {
                        if argc != c.proto.arity {
                            return Err(self.runtime_error(format!(
                                "expected {} argument(s), got {argc}",
                                c.proto.arity
                            )));
                        }
                        if self.frames.len() >= MAX_FRAMES {
                            return Err(self.runtime_error("stack overflow"));
                        }
                        let slot_base = self.stack.len() - argc;
                        self.frames.push(CallFrame { closure: c, ip: 0, slot_base });
                    }
                    Value::Native(n) => {
                        if argc != n.arity {
                            return Err(self.runtime_error(format!(
                                "expected {} argument(s), got {argc}",
                                n.arity
                            )));
                        }
                        let args_start = self.stack.len() - argc;
                        let args: Vec<Value> = self.stack[args_start..].to_vec();
                        self.stack.truncate(args_start - 1); // removes the native callee + its args
                        let line = self.chunk().line_at(self.frame().ip.saturating_sub(1));
                        let result = (n.func)(&args, line).map_err(|e| self.attach_trace(e))?;
                        self.push(result);
                    }
                    other => return Err(self.runtime_error(format!("cannot call {other:?}"))),
                }
            }
```

Add this helper to `impl Vm` (used by natives, which have no `&Vm` access of their own to build a trace with):

```rust
    /// Native functions return a bare `RuntimeError` with no knowledge of
    /// the caller's frame stack — this fills in the real trace before the
    /// error propagates further.
    fn attach_trace(&self, mut err: RuntimeError) -> RuntimeError {
        err.trace = self
            .frames
            .iter()
            .rev()
            .map(|f| crate::error::TraceFrame {
                function_name: f.closure.proto.name,
                line: f.closure.proto.chunk.line_at(f.ip.saturating_sub(1)),
            })
            .collect();
        err
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute function calls, closures and natives, with correct multi-frame cleanup"
```

---

## Task 11: `OP_CLOSURE` and upvalues — capture, sharing, and closing

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

With real function calls working (Task 10), this task can finally test through the **actual compiler pipeline** instead of hand-built chunks — `ember-vm`'s `Cargo.toml` already has `ember-parser`/`ember-resolve`/`ember-compile` as dev-dependencies for exactly this. This is also where `close_upvalues` stops being Task 6's no-op stub.

`capture_upvalue(slot)` searches `open_upvalues` for an existing entry at that slot and reuses it — this is *the* mechanism that makes shared capture work (two closures over the same variable end up holding `Rc::clone`s of the identical `RefCell`, so a write through one is visible through the other). `close_upvalues(from)` hoists every open upvalue at or above `from` from the stack to the heap; `OP_CLOSE_UPVALUE` calls it with `from = stack.len() - 1` (the position of the value it's about to discard — the same slot `OP_POP` would have targeted, had this local not been captured), then pops. Deliberately **not** kept sorted by slot (unlike `SPEC.md`'s own intrusive-linked-list sketch) — that ordering exists there to support an early-exit scan, but this implementation does a full drain-and-filter of `open_upvalues` on every close regardless of order, so sorting would add bookkeeping without buying anything.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    fn compile_and_run(src: &str) -> Result<Value, RuntimeError> {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "resolve diags: {resolve_diags:?}");
        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        Vm::new(proto).run()
    }

    #[test]
    fn a_closure_with_no_captures_compiles_and_calls_correctly() {
        let result = compile_and_run("let f = || 42; f();").unwrap();
        assert!(matches!(result, Value::Int(42)));
    }

    #[test]
    fn shared_capture_two_closures_see_each_others_mutations() {
        let src = "
            let mut counter = 0;
            let inc = || { counter = counter + 1; counter };
            let get = || counter;
            inc();
            inc();
            get();
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(2)));
    }

    #[test]
    fn upvalue_closed_at_scope_exit_survives_and_state_persists_across_calls() {
        let src = "
            fn make_counter() {
                let mut count = 0;
                || { count = count + 1; count }
            }
            let counter = make_counter();
            counter();
            counter();
            counter();
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(3)));
    }

    #[test]
    fn two_independently_made_counters_do_not_share_state() {
        let src = "
            fn make_counter() {
                let mut count = 0;
                || { count = count + 1; count }
            }
            let a = make_counter();
            let b = make_counter();
            a();
            a();
            b();
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(1)), "b's own counter must start fresh at 1, not inherit a's 2");
    }

    #[test]
    fn recursive_function_calls_compute_the_correct_value() {
        // Real recursion (a top-level fn calling itself by name, via
        // OP_GET_GLOBAL — ember-compile's top-level dual registration) is
        // only testable once OP_CLOSURE exists to actually produce the
        // callable in the first place, hence this test living here rather
        // than in the OP_CALL task.
        let src = "
            fn fact(n) {
                if n == 0 { 1 } else { n * fact(n - 1) }
            }
            fact(5);
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(120)));
    }

    #[test]
    fn runaway_recursion_is_a_runtime_error_not_a_crash_or_hang() {
        let src = "
            fn forever(n) { forever(n + 1) }
            forever(0);
        ";
        let result = compile_and_run(src);
        assert!(result.is_err(), "unbounded recursion must hit MAX_FRAMES and produce a RuntimeError, not overflow the native stack or loop forever");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — `Closure`/`GetUpvalue`/`SetUpvalue`/`CloseUpvalue` fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match`:

```rust
            Op::Closure => {
                let idx = self.read_u16();
                let proto = Rc::clone(&self.chunk().functions[idx as usize]);
                let mut upvalues = Vec::with_capacity(proto.upvalues.len());
                for desc in &proto.upvalues {
                    let uv = if desc.is_local {
                        let slot = self.frame().slot_base + desc.index as usize;
                        self.capture_upvalue(slot)
                    } else {
                        Rc::clone(&self.frame().closure.upvalues[desc.index as usize])
                    };
                    upvalues.push(uv);
                }
                self.push(Value::Closure(Rc::new(ClosureObj { proto, upvalues })));
            }
            Op::GetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = Rc::clone(&self.frame().closure.upvalues[idx]);
                let v = match &*uv.borrow() {
                    crate::value::Upvalue::Open(slot) => self.stack[*slot].clone(),
                    crate::value::Upvalue::Closed(v) => v.clone(),
                };
                self.push(v);
            }
            Op::SetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = Rc::clone(&self.frame().closure.upvalues[idx]);
                let v = self.peek(0).clone();
                let open_slot = match &*uv.borrow() {
                    crate::value::Upvalue::Open(slot) => Some(*slot),
                    crate::value::Upvalue::Closed(_) => None,
                };
                match open_slot {
                    Some(slot) => self.stack[slot] = v,
                    None => *uv.borrow_mut() = crate::value::Upvalue::Closed(v),
                }
            }
            Op::CloseUpvalue => {
                let slot = self.stack.len() - 1;
                self.close_upvalues(slot);
                self.pop();
            }
```

Replace Task 6's temporary no-op `close_upvalues` stub with the real implementation, and add `capture_upvalue` alongside it:

```rust
    fn close_upvalues(&mut self, from: usize) {
        let mut keep = Vec::with_capacity(self.open_upvalues.len());
        for uv in self.open_upvalues.drain(..) {
            let slot = match &*uv.borrow() {
                crate::value::Upvalue::Open(s) => Some(*s),
                crate::value::Upvalue::Closed(_) => None,
            };
            match slot {
                Some(s) if s >= from => {
                    let value = self.stack[s].clone();
                    *uv.borrow_mut() = crate::value::Upvalue::Closed(value);
                }
                _ => keep.push(uv),
            }
        }
        self.open_upvalues = keep;
    }

    fn capture_upvalue(&mut self, slot: usize) -> Rc<RefCell<crate::value::Upvalue>> {
        for uv in &self.open_upvalues {
            if let crate::value::Upvalue::Open(s) = &*uv.borrow() {
                if *s == slot {
                    return Rc::clone(uv);
                }
            }
        }
        let uv = Rc::new(RefCell::new(crate::value::Upvalue::Open(slot)));
        self.open_upvalues.push(Rc::clone(&uv));
        uv
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute closures with real upvalue capture, sharing, and closing"
```

---

## Task 12: Lists, indexing, and field access — `OP_MAKE_LIST`/`OP_GET_INDEX`/`OP_SET_INDEX`/`OP_GET_FIELD`/`OP_SET_FIELD`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

Every one of these is a straight-line stack operation — no branching, so none of the `stack_depth`-style bugs Phase 8 kept finding apply here (those were a *compile-time* bookkeeping problem specific to `ember-compile`, not something the VM's own runtime execution has an equivalent of). `OP_SET_FIELD`/`OP_SET_INDEX` push the assigned value back (assignment-as-expression), matching `ember-compile`'s own static-effect table (`Some(-1)`/`Some(-2)` respectively — consuming one more than they produce).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn list_literal_and_index_read() {
        let result = compile_and_run("let xs = [10, 20, 30]; xs[1];").unwrap();
        assert!(matches!(result, Value::Int(20)));
    }

    #[test]
    fn index_out_of_bounds_is_a_runtime_error() {
        let result = compile_and_run("let xs = [1]; xs[5];");
        assert!(result.is_err());
    }

    #[test]
    fn index_assignment_mutates_the_list_in_place() {
        let result = compile_and_run("let mut xs = [1, 2, 3]; xs[0] = 99; xs[0];").unwrap();
        assert!(matches!(result, Value::Int(99)));
    }

    #[test]
    fn field_read_and_write_on_a_struct() {
        let src = "
            struct P { x: Int }
            let mut p = P { x: 1 };
            p.x = 42;
            p.x;
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(42)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — `MakeList`/`GetIndex`/`SetIndex`/`GetField`/`SetField` fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match`:

```rust
            Op::MakeList => {
                let count = self.read_u16() as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.pop());
                }
                items.reverse(); // popped in reverse of push order
                self.push(Value::List(Rc::new(RefCell::new(items))));
            }
            Op::GetIndex => {
                let index = self.pop();
                let base = self.pop();
                let v = self.index_get(base, index)?;
                self.push(v);
            }
            Op::SetIndex => {
                let value = self.pop();
                let index = self.pop();
                let base = self.pop();
                self.index_set(base, index, value.clone())?;
                self.push(value);
            }
            Op::GetField => {
                let name = self.read_global_name();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        let v = fields.borrow().get(&name).cloned();
                        match v {
                            Some(v) => self.push(v),
                            None => return Err(self.runtime_error(format!("no field `{name}`"))),
                        }
                    }
                    other => return Err(self.runtime_error(format!("cannot access field `{name}` on {other:?}"))),
                }
            }
            Op::SetField => {
                let name = self.read_global_name();
                let value = self.pop();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        fields.borrow_mut().insert(name, value.clone());
                        self.push(value);
                    }
                    other => return Err(self.runtime_error(format!("cannot set field `{name}` on {other:?}"))),
                }
            }
```

Add these helpers to `impl Vm`:

```rust
    fn index_get(&self, base: Value, index: Value) -> Result<Value, RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let l = l.borrow();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                Ok(l[*i as usize].clone())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }

    fn index_set(&self, base: Value, index: Value, value: Value) -> Result<(), RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let mut l = l.borrow_mut();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                l[*i as usize] = value;
                Ok(())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute list construction, indexing, and field access"
```

---

## Task 13: Struct/ADT construction and pattern matching — `OP_MAKE_RECORD`/`OP_MAKE_ADT`/`OP_TEST_VARIANT`/`OP_DESTRUCTURE`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

**`OP_MAKE_RECORD`'s field encoding:** per `ember-compile`'s design, field names travel on the stack as ordinary pushed values (interleaved `name1, value1, name2, value2, ...`), not as instruction operands — the instruction's own two `u16` operands are just the record's type-name constant and the field count. Popping gives the pairs in **reverse** of push order (`valueN, nameN, ..., value1, name1`) — get the pop order backward within a pair (name before value) or forget to reverse the pair sequence and every field ends up matched to the wrong value.

**`OP_MAKE_ADT`'s positional order matters for correctness, not just cosmetics**, unlike `MakeRecord`'s pair order: `payload[0]` must be the constructor's *first* argument. Since args were pushed `arg0, arg1, ..., arg(n-1)` and popped in reverse, the popped `Vec` must be `.reverse()`d before becoming `AdtValue.fields` — skip it and every payload field ends up permuted backward.

**`OP_TEST_VARIANT`** pops the scrutinee and pushes a `Bool` — checking either `Value::Adt`'s `.variant` or `Value::Record`'s `.name` against the given pooled string (both compare by value, `Rc<String>: PartialEq` derefs to plain string comparison, not pointer identity — needed since the same logical name can be pooled as separate `Rc` allocations across different constant pools). **`OP_DESTRUCTURE`** pops an `Adt` and pushes its payload field at the given positional index.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn struct_literal_construction_and_field_read() {
        let src = "struct P { x: Int, y: Int } let p = P { x: 3, y: 4 }; p.x + p.y;";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(7)));
    }

    #[test]
    fn nullary_adt_variant_constructs_directly() {
        let src = "type Shape = Circle(Float) | Origin type_of(Origin);";
        // type_of isn't wired until a later task's natives — use a match instead:
        let src = "
            type Shape = Circle(Float) | Origin
            match Origin {
                Origin => 1,
                Circle(_) => 2,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(1)));
    }

    #[test]
    fn payload_adt_variant_constructs_via_a_callable_ctor_with_fields_in_order() {
        let src = "
            type Pair = Pair(Int, Int)
            match Pair(10, 20) {
                Pair(a, b) => a - b,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(-10)), "must be 10 - 20, not 20 - 10 (payload order matters)");
    }

    #[test]
    fn ctor_pattern_tests_the_tag_and_destructures_by_position() {
        let src = "
            type Shape = Circle(Float) | Square(Float)
            fn area(s) {
                match s {
                    Circle(r) => 3.14 * r * r,
                    Square(side) => side * side,
                }
            }
            area(Square(4.0));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Float(f) if (f - 16.0).abs() < 1e-9));
    }

    #[test]
    fn record_pattern_tests_by_name() {
        let src = "
            struct P { x: Int }
            match P { x: 5 } {
                P { x } => x,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(5)));
    }

    #[test]
    fn guard_and_or_pattern_both_work() {
        let src = "
            match 2 {
                x if x > 10 => 1,
                1 | 2 | 3 => 2,
                _ => 3,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(2)));
    }
}
```

(The `nullary_adt_variant_constructs_directly` test above shows the throwaway-then-real-source pattern deliberately — leave only the second `let src = ...` assignment and its following code; delete the unused first `let src` line and dead `type_of` reference, they're just scratch showing why a `match`-based test was chosen over a native call that doesn't exist yet.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL — `MakeRecord`/`MakeAdt`/`TestVariant`/`Destructure` fall through `step`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add these arms to `step`'s `match`:

```rust
            Op::MakeRecord => {
                let name_idx = self.read_u16();
                let type_name = self.str_constant(name_idx);
                let field_count = self.read_u16() as usize;
                let mut pairs = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let value = self.pop();
                    let name = match self.pop() {
                        Value::Str(s) => s,
                        other => panic!("record field name must be a string, found {other:?}"),
                    };
                    pairs.push((name, value));
                }
                pairs.reverse();
                let fields: FxHashMap<Rc<String>, Value> = pairs.into_iter().collect();
                self.push(Value::Record { name: type_name, fields: Rc::new(RefCell::new(fields)) });
            }
            Op::MakeAdt => {
                let type_idx = self.read_u16();
                let type_name = self.str_constant(type_idx);
                let variant_idx = self.read_u16();
                let variant = self.str_constant(variant_idx);
                let arity = self.read_u16() as usize;
                let mut fields = Vec::with_capacity(arity);
                for _ in 0..arity {
                    fields.push(self.pop());
                }
                fields.reverse(); // positional order matters — see this task's own note
                self.push(Value::Adt(Rc::new(crate::value::AdtValue { type_name, variant, fields })));
            }
            Op::TestVariant => {
                let idx = self.read_u16();
                let name = self.str_constant(idx);
                let v = self.pop();
                let matches_name = match &v {
                    Value::Adt(a) => a.variant == name,
                    Value::Record { name: rname, .. } => *rname == name,
                    _ => false,
                };
                self.push(Value::Bool(matches_name));
            }
            Op::Destructure => {
                let index = self.read_u8() as usize;
                let base = self.pop();
                match base {
                    Value::Adt(a) => self.push(a.fields[index].clone()),
                    other => return Err(self.runtime_error(format!("cannot destructure {other:?}"))),
                }
            }
```

Add this helper to `impl Vm` (parallel to `read_global_name`, but for a *known* constant-pool index rather than one just read off `ip` — `MakeAdt`/`TestVariant` need to resolve a string constant at an index they already have in hand, not the next one in the stream):

```rust
    fn str_constant(&self, idx: u16) -> Rc<String> {
        match &self.chunk().constants[idx as usize] {
            ember_bytecode::value::Value::Str(s) => Rc::clone(s),
            other => panic!("name constant must be a string, found {other:?}"),
        }
    }
```

While in there: `read_global_name` (Task 8) duplicates this same string-constant-resolution logic inline. Feel free to simplify it to `let idx = self.read_u16(); self.str_constant(idx)` for the small DRY win — not required, purely a nice-to-have, since both versions behave identically.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Execute struct/ADT construction and pattern matching"
```

---

## Task 14: Native functions

**Files:**
- Modify: `crates/ember-vm/src/natives.rs`
- Modify: `crates/ember-vm/src/vm.rs` (`Vm::new` pre-seeds `globals`)

The same 8 functions as `ember-tree::natives` (`print`/`len`/`push`/`clock`/`str`/`int`/`float`/`type_of`), reimplemented against `ember_vm::Value` — the two `Value` types are structurally similar but not the same Rust type, so the logic is duplicated rather than shared (the same situation `ember-bytecode::Value` was already in relative to `ember-tree::Value`). Every native's signature is `fn(&[Value], u32) -> Result<Value, RuntimeError>` — **no `&Interner`**, since `display_value` doesn't need one (this plan's header note) and every `RuntimeError` here just needs the current `line`.

Unlike `ember-tree`'s dynamic fallback-lookup inside `eval_var` (Phase 7's `eval_var` only ever looks up a native the moment a `Var` reference actually needs one), `Vm::new` **pre-seeds** `globals` with all 8 up front — `OP_GET_GLOBAL` is an unconditional hash lookup with no fallback path, mirroring exactly how `ember-resolve::seed_native_globals` and `ember-compile`'s top-level dual registration already treat natives as pre-existing globals rather than lazily-discovered ones.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/ember-vm/src/natives.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_reports_list_length() {
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)])));
        let result = len(&[list], 1).unwrap();
        assert!(matches!(result, Value::Int(3)));
    }

    #[test]
    fn len_rejects_a_non_list() {
        assert!(len(&[Value::Int(1)], 1).is_err());
    }

    #[test]
    fn push_appends_in_place() {
        let list = Value::List(Rc::new(RefCell::new(vec![Value::Int(1)])));
        push(&[list.clone(), Value::Int(2)], 1).unwrap();
        match &list {
            Value::List(l) => assert_eq!(l.borrow().len(), 2),
            _ => unreachable!(),
        }
    }

    #[test]
    fn str_formats_any_value() {
        let result = str_fn(&[Value::Int(42)], 1).unwrap();
        match result {
            Value::Str(s) => assert_eq!(*s, "42"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn int_parses_strings_and_truncates_floats() {
        assert!(matches!(int_fn(&[Value::Float(3.9)], 1).unwrap(), Value::Int(3)));
        assert!(matches!(int_fn(&[Value::Str(Rc::new("42".to_string()))], 1).unwrap(), Value::Int(42)));
        assert!(int_fn(&[Value::Str(Rc::new("nope".to_string()))], 1).is_err());
    }

    #[test]
    fn float_parses_strings_and_widens_ints() {
        assert!(matches!(float_fn(&[Value::Int(3)], 1).unwrap(), Value::Float(f) if f == 3.0));
    }

    #[test]
    fn type_of_names_every_kind_including_records_and_adts_by_their_own_name() {
        assert_eq!(type_of(&[Value::Int(1)], 1).unwrap(), Value::Str(Rc::new("Int".to_string())));
        let adt = Value::Adt(Rc::new(crate::value::AdtValue {
            type_name: Rc::new("Shape".to_string()),
            variant: Rc::new("Circle".to_string()),
            fields: vec![],
        }));
        assert_eq!(type_of(&[adt], 1).unwrap(), Value::Str(Rc::new("Shape".to_string())));
    }

    #[test]
    fn clock_returns_a_float() {
        assert!(matches!(clock(&[], 1).unwrap(), Value::Float(_)));
    }

    #[test]
    fn natives_table_has_all_8_with_the_right_arities() {
        let expected: &[(&str, usize)] = &[
            ("print", 1), ("len", 1), ("push", 2), ("clock", 0),
            ("str", 1), ("int", 1), ("float", 1), ("type_of", 1),
        ];
        assert_eq!(NATIVES.len(), 8);
        for (name, arity) in expected {
            let found = NATIVES.iter().find(|(n, _, _)| n == name);
            assert!(found.is_some(), "missing native {name}");
            assert_eq!(found.unwrap().1, *arity, "wrong arity for {name}");
        }
    }
}
```

`assert_eq!(result, Value::Str(...))`/`assert_eq!(type_of(...), ...)` above needs `Value: PartialEq`, which it doesn't have yet (Task 5 only derived `Debug`/`Clone`). **Don't** `#[derive(PartialEq)]` on `Value` — it would transitively require `ClosureObj`/`NativeFn`/`FunctionProto: PartialEq`, and `ember_bytecode::chunk::FunctionProto` has no such derive (nor should it gain one just for this). Implement it by hand in `value.rs` instead, added as part of this task, treating `Closure`/`Native` as never equal to anything — mirroring `values_equal` (Task 5), which already has no case for them and falls through to `_ => false`:

```rust
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => *a.borrow() == *b.borrow(),
            (Value::Record { name: n1, fields: f1 }, Value::Record { name: n2, fields: f2 }) => {
                n1 == n2 && *f1.borrow() == *f2.borrow()
            }
            _ => false,
        }
    }
}
```

(`Value::List`'s and `Value::Record`'s comparisons above lean on `Vec<Value>: PartialEq`/`FxHashMap<Rc<String>, Value>: PartialEq`, which this same `impl` makes available recursively — Rust resolves that fine since the outer `impl` is already in scope by the time the inner comparisons need it.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-vm`
Expected: FAIL to compile — `len`/`push`/`str_fn`/`int_fn`/`float_fn`/`type_of`/`clock`/`NATIVES` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::error::RuntimeError;
use crate::value::{display_value, NativeFn, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub fn print(args: &[Value], _line: u32) -> Result<Value, RuntimeError> {
    println!("{}", display_value(&args[0]));
    Ok(Value::Nil)
}

pub fn len(args: &[Value], line: u32) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Int(l.borrow().len() as i64)),
        other => Err(RuntimeError::new(format!("len expects a list, found {other:?}"), line)),
    }
}

pub fn push(args: &[Value], line: u32) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => {
            l.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(format!("push expects a list, found {other:?}"), line)),
    }
}

pub fn clock(_args: &[Value], _line: u32) -> Result<Value, RuntimeError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Value::Float(now.as_secs_f64()))
}

pub fn str_fn(args: &[Value], _line: u32) -> Result<Value, RuntimeError> {
    Ok(Value::Str(Rc::new(display_value(&args[0]))))
}

pub fn int_fn(args: &[Value], line: u32) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Int"), line)),
        other => Err(RuntimeError::new(format!("cannot convert {other:?} to Int"), line)),
    }
}

pub fn float_fn(args: &[Value], line: u32) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Float"), line)),
        other => Err(RuntimeError::new(format!("cannot convert {other:?} to Float"), line)),
    }
}

pub fn type_of(args: &[Value], _line: u32) -> Result<Value, RuntimeError> {
    let name = match &args[0] {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::Nil => "Nil".to_string(),
        Value::Str(_) => "String".to_string(),
        Value::List(_) => "List".to_string(),
        Value::Closure(_) | Value::Native(_) => "Function".to_string(),
        Value::Adt(a) => a.type_name.to_string(),
        Value::Record { name, .. } => name.to_string(),
    };
    Ok(Value::Str(Rc::new(name)))
}

type NativeImpl = fn(&[Value], u32) -> Result<Value, RuntimeError>;

pub const NATIVES: &[(&str, usize, NativeImpl)] = &[
    ("print", 1, print),
    ("len", 1, len),
    ("push", 2, push),
    ("clock", 0, clock),
    ("str", 1, str_fn),
    ("int", 1, int_fn),
    ("float", 1, float_fn),
    ("type_of", 1, type_of),
];
```

(This matches `ember-tree::natives::NATIVES`'s own `(&str, usize, NativeImpl)` shape exactly — reuse that layout rather than inventing a new one, since Task 1's test table above expects a 3-tuple.)

Then, in `vm.rs`, update `Vm::new` to pre-seed `globals`:

```rust
    pub fn new(script: FunctionProto) -> Self {
        let proto = Rc::new(script);
        let closure = Rc::new(ClosureObj { proto, upvalues: Vec::new() });
        let frame = CallFrame { closure, ip: 0, slot_base: 0 };
        let mut globals = FxHashMap::default();
        for &(name, arity, func) in crate::natives::NATIVES {
            globals.insert(
                Rc::new(name.to_string()),
                Value::Native(Rc::new(crate::value::NativeFn { name, arity, func })),
            );
        }
        Vm {
            stack: Vec::new(),
            frames: vec![frame],
            globals,
            open_upvalues: Vec::new(),
        }
    }
```

(Replacing the earlier `globals: FxHashMap::default()` initializer from Task 6 — find and update that one line, keep everything else in `new` unchanged.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-vm`
Expected: PASS. Run `cargo clippy -p ember-vm --all-targets -- -D warnings` and `cargo fmt -p ember-vm -- --check`, fix anything flagged.

Also add a quick end-to-end check via `compile_and_run` in `vm.rs`'s own test module confirming a real program can reach a native through the whole pipeline, e.g. `compile_and_run("let xs = [1, 2]; push(xs, 3); len(xs);")` should yield `Value::Int(3)` — natives are globals exactly like any top-level `fn`, so no special compiler support is needed, but it's worth confirming end to end now that both sides exist.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm
git commit -m "Add native functions and pre-seed them as globals"
```

---

## Task 15: `ember-cli`'s `vm` subcommand, and the conformance cross-check

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-cli/tests/conformance.rs`

A new `vm` subcommand runs the identical pipeline `run` (tree-walker) already does, swapping the last step for compile-then-execute — for manual side-by-side comparison from the command line. The conformance harness (Phase 8) gains its second, closing assertion: every fixture now runs through *both* backends in the same test, checked against `.expected` independently and against each other directly — this is CHECKLIST's own "every conformance program produces identical output to the tree-walker" test, and the actual payoff of the whole two-backend architecture this project has been building toward since `SPEC.md §3`.

- [ ] **Step 1: Add dependencies**

In `crates/ember-cli/Cargo.toml`, add to `[dependencies]`:

```toml
ember-bytecode = { path = "../ember-bytecode" }
ember-compile = { path = "../ember-compile" }
ember-vm = { path = "../ember-vm" }
```

- [ ] **Step 2: Write the failing test**

Add to `crates/ember-cli/tests/conformance.rs`, replacing the whole file's single test function (keep `conformance_dir`/`has_errors` as they are):

```rust
#[test]
fn both_backends_produce_identical_output_matching_every_captured_fixture() {
    let dir = conformance_dir();
    let mut checked = 0;
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("em"))
        .collect();
    entries.sort();

    for path in entries {
        let expected_path = path.with_extension("expected");
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("missing {expected_path:?} for {path:?}: {e}"));

        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
        assert!(parse_diags.is_empty(), "{path:?}: parse diags: {parse_diags:?}");

        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(!has_errors(&resolve_diags), "{path:?}: resolve diags: {resolve_diags:?}");

        let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        assert!(!has_errors(&infer_diags), "{path:?}: infer diags: {infer_diags:?}");

        let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert!(!has_errors(&exhaustive_diags), "{path:?}: exhaustiveness diags: {exhaustive_diags:?}");

        let (tree_result, tree_err) = ember_tree::interpret(&ast, &interner, &stmts);
        assert!(tree_err.is_none(), "{path:?}: tree-walker runtime error: {tree_err:?}");
        let tree_actual = match tree_result {
            Some(v) => ember_tree::display_value(&v, &interner),
            None => String::new(),
        };
        assert_eq!(tree_actual.trim(), expected.trim(), "{path:?}: tree-walker output mismatch");

        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let mut vm = ember_vm::vm::Vm::new(proto);
        let vm_actual = match vm.run() {
            Ok(v) => ember_vm::value::display_value(&v),
            Err(e) => panic!(
                "{path:?}: VM runtime error: {}",
                e.to_diagnostic(&interner).message
            ),
        };
        assert_eq!(vm_actual.trim(), expected.trim(), "{path:?}: VM output mismatch");
        assert_eq!(
            tree_actual.trim(),
            vm_actual.trim(),
            "{path:?}: the two backends disagree with each other"
        );

        checked += 1;
    }
    assert!(checked >= 6, "expected at least 6 conformance fixtures, found {checked} in {dir:?}");
}
```

- [ ] **Step 3: Run to verify pass**

Run: `cargo test -p ember-cli --test conformance`
Expected: PASS, `checked == 6`. If the VM disagrees with the tree-walker on any fixture, that's a real bug somewhere in Tasks 1-14 (or an as-yet-undiscovered gap in `ember-compile`) — debug it via `cargo run -p ember-cli -- vm <fixture path>` and `cargo run -p ember-cli -- run <fixture path>` side by side (built in the next step) rather than guessing; don't paper over a mismatch by editing the fixture.

- [ ] **Step 4: Add the `vm` subcommand**

In `main.rs`, add a new `Vm` variant to the `Command` enum (alongside `Run`):

```rust
    /// Parse, resolve, typecheck, check exhaustiveness, then compile to
    /// bytecode and run it on the VM, printing its final value or a
    /// rendered runtime-error diagnostic — same pipeline as `run`, but the
    /// bytecode backend instead of the tree-walker.
    Vm { file: String },
```

Add the matching arm to `main`'s dispatch:

```rust
        Command::Vm { file } => run_vm(&file),
```

Add the `run_vm` function (place it right after `run_run`, matching that function's shape closely):

```rust
fn run_vm(path: &str) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(&src);
    if !parse_diags.is_empty() {
        return print_diagnostics(&parse_diags, path, &src);
    }

    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    if resolve_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&resolve_diags, path, &src);
    }

    let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
    if infer_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&infer_diags, path, &src);
    }

    let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
    if exhaustive_diags
        .iter()
        .any(|d| d.severity == ember_diag::Severity::Error)
    {
        return print_diagnostics(&exhaustive_diags, path, &src);
    }

    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let mut vm = ember_vm::vm::Vm::new(proto);
    match vm.run() {
        Ok(v) => {
            println!("{}", ember_vm::value::display_value(&v));
            ExitCode::SUCCESS
        }
        Err(e) => {
            let use_color = std::env::var_os("NO_COLOR").is_none();
            println!(
                "{}",
                ember_diag::render::render(&e.to_diagnostic(&interner), path, &src, use_color)
            );
            ExitCode::from(2)
        }
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo build -p ember-cli` then manually sanity-check both backends agree on a real file:

```bash
cargo run -p ember-cli -- run tests/conformance/adt_and_match.em
cargo run -p ember-cli -- vm tests/conformance/adt_and_match.em
```

Expected: identical output (`16`) from both. Run `cargo test -p ember-cli`, `cargo clippy -p ember-cli --all-targets -- -D warnings`, `cargo fmt -p ember-cli -- --check`, fix anything flagged.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-cli Cargo.lock
git commit -m "Add the vm subcommand and complete the conformance cross-check"
```

---

## Task 16: Final verification and `CHECKLIST.md` reconciliation

**Not delegated to a subagent** — done directly, same as every phase's final task so far.

- [ ] Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all -- --check`; fix anything that surfaces.
- [ ] Read `CHECKLIST.md`'s Phase 9 section and check off every item that's genuinely done, item by item — not a blanket check.
- [ ] Add honest notes for every deliberate scope decision and every bug found and fixed along the way, at minimum:
  - The retroactive `ember-bytecode` fix (`Chunk.functions` → `Vec<Rc<FunctionProto>>`) and the retroactive `ember-compile` fix (the top-level program returning its last statement's value) — both bugs in already-merged Phase 8 code, only surfaced once something could actually *execute* a compiled program.
  - Names are `Rc<String>` at runtime, never `Symbol`, except `FunctionProto.name` (used solely for stack traces) — a real simplification over the design doc's initial sketch, not just an implementation detail.
  - The callee-cleanup subtlety in `OP_CALL`/`OP_RETURN` (`slot_base - 1`, not `slot_base`) and why it's needed.
  - `open_upvalues` is an unsorted `Vec`, not sorted-by-slot-descending like `SPEC.md`'s own sketch — a deliberate, explained deviation (the drain-and-filter `close_upvalues` never needed the ordering).
  - `NativeFn.arity`, checked before dispatch (mirroring `ember-tree`'s own native-arity check) — prevents an out-of-bounds `args[]` panic on a wrong-arity native call.
  - `Vm` has no `gc` field, unlike `SPEC.md`'s literal struct sketch — there's no `GcHeap` until Phase 10, so nothing to hold a handle to yet; this isn't an oversight, it's this phase's whole "no GC" premise made concrete in the one place the checklist's sketch mentions it directly.
  - "Stack push/pop/peek with a depth limit" is satisfied by `MAX_FRAMES` (a *frame*-count cap), not a separate raw value-stack size cap — the dispatch loop is iterative, so unlike a native recursive interpreter there's no equivalent risk of unbounded plain-value-stack growth independent of call depth; capping frames already bounds the value stack too, since every frame's own locals/temporaries are bounded by what that function's own compiled chunk can push.
  - Non-goals reconfirmed: NaN boxing and computed-goto dispatch (both 🟡/🔵, deferred, no measured performance need yet); the real garbage collector (Phase 10) — `ember_vm::Value` stays `Rc`/`RefCell`-based until then.
- [ ] Verify the final `git log` for this phase reads as a clean, coherent history (no leftover WIP-sounding messages).

---
