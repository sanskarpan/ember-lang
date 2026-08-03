# SPEC.md — `ember`: A Programming Language, End to End

> **Backend: Rust 2021 (MSRV 1.80+)** — lexer, parser, resolver, type inference, tree-walking interpreter, bytecode compiler + VM, GC, LSP server, WASM build
> **Frontend: React 18 + TypeScript + Vite + CodeMirror 6 + Tailwind + shadcn/ui + D3** — a browser playground where the compiler *is* the app, compiled to WASM
> **Two execution backends, one language** — that comparison is the pedagogical spine of the project

---

## §1 Language Decision — Rust

### The workload

| Component | Requirement | Why Rust wins |
|---|---|---|
| AST / IR representation | Sum types with exhaustive matching | `enum` + `match` is *the* algebraic data type story; the compiler catches every unhandled node |
| Parser | Zero-copy tokens, arena-allocated nodes | `&'src str` slices + `Vec<Node>` arena with `u32` indices; no per-node heap allocation |
| Bytecode VM | Tight dispatch loop, cache-friendly value repr | No GC pauses, no boxing you didn't ask for, `#[repr(u8)]` opcodes, NaN-boxing possible |
| Garbage collector | Manual object graph, precise root tracking | You *must* write it yourself — a host GC would hide the entire lesson |
| Browser playground | Ship the real compiler, not a reimplementation | `wasm-bindgen` + `wasm-pack` → the exact same code runs in the browser |
| LSP server | Concurrent request handling, incremental state | `tower-lsp` + `tokio`, `Arc<RwLock<Analysis>>` |

### Why not the alternatives

- **Go** — no sum types. An AST becomes `interface{}` + type switches with no exhaustiveness checking. Adding a node variant silently compiles and panics at runtime. For a project whose core data structure *is* a sum type, this is disqualifying.
- **C/C++** — the traditional choice (clox is C), and you'd fight memory bugs in your own GC forever. The irony of a segfaulting garbage collector is not worth it.
- **OCaml/Haskell** — genuinely excellent for the *front end* (pattern matching, HM inference is idiomatic), but you inherit their GC, which kills the "write your own GC" chapter, and the WASM story is far worse.
- **TypeScript** — one language for compiler and playground, but you cannot write a real GC or a fast VM, and NaN-boxing is meaningless in a language where every number is already a double.

**Rust is the only option that does the front end elegantly *and* lets you write a real VM and a real GC *and* ships to the browser as the actual artifact.**

### Crates

| Crate | Role |
|---|---|
| `logos` | derive-macro lexer (DFA-generated, extremely fast) — or hand-rolled; spec covers both |
| `ariadne` | rustc-quality diagnostics with multi-span labels, colors, notes |
| `la-arena` / hand-rolled | AST arena with `Idx<T>` handles |
| `rustc-hash` (`FxHashMap`) | fast non-cryptographic maps for interning, scopes, globals |
| `string-interner` | symbol interning — compare identifiers with `u32` equality |
| `tower-lsp` + `tokio` | LSP server |
| `wasm-bindgen`, `serde-wasm-bindgen` | browser build |
| `clap`, `rustyline` | CLI + REPL with history and multi-line input |
| `insta` | snapshot testing for ASTs, bytecode disassembly, diagnostics |
| `criterion` | benchmarks (tree-walk vs VM) |
| `proptest` | property tests (parser round-trip, GC soundness) |

**No parser generator.** No `lalrpop`, no `pest`, no `chumsky`. A hand-written Pratt parser is ~400 lines, gives total control over error recovery, and is the single most valuable thing to understand in this project. A generator hides exactly what you're here to learn.

### Frontend: React + **CodeMirror 6** (not Monaco)

| | CodeMirror 6 | Monaco |
|---|---|---|
| Bundle | ~50–300 KB tree-shaken | 2–5 MB |
| Custom language | Designed for it — Lezer grammar or a stream tokenizer | TextMate grammar + Monarch; workable but bolted on |
| Extension model | Everything is an extension; decorations, gutters, tooltips, panels are first-class | Fixed extension points; people monkey-patch |
| Our needs | Custom highlighting, inline type hints, AST-node hover linking, step-debugger line markers, error squiggles from *our* diagnostics | Same, but fighting the framework |

We're building a playground for a language nobody has ever seen, with heavy custom decoration (highlight the AST node under the cursor, show inferred types inline, mark the currently-executing line). **CodeMirror 6 is the correct tool**; Monaco's value is its built-in TypeScript service, which is irrelevant here.

Plus **D3** for the AST tree and the environment/heap graph, and **Recharts** for benchmark comparison charts.

---

## §2 The Language: `ember`

Designed so that every major implementation technique has a reason to exist.

```rust
// ── Bindings & types ────────────────────────────────────────────
let x = 42;                    // inferred: Int
let name: String = "ember";    // annotated
let mut count = 0;             // mutable
count = count + 1;

// ── Functions, closures, first-class ────────────────────────────
fn add(a: Int, b: Int) -> Int { a + b }

let make_counter = || {        // closure capturing by upvalue
    let mut n = 0;
    || { n = n + 1; n }        // nested closure mutating captured var
};

// ── Expression-oriented: if/match/block all yield values ────────
let sign = if x > 0 { "pos" } else if x < 0 { "neg" } else { "zero" };

// ── Algebraic data types + exhaustive matching ──────────────────
type Shape =
  | Circle(Float)
  | Rect(Float, Float)
  | Point;

fn area(s: Shape) -> Float {
    match s {
        Circle(r)  => 3.14159 * r * r,
        Rect(w, h) => w * h,
        Point      => 0.0,
    }                          // non-exhaustive match is a COMPILE ERROR
}

// ── Generics via Hindley-Milner (no annotations needed) ─────────
fn identity(x) { x }           // inferred: forall a. a -> a
fn map(list, f) {              // forall a b. [a] -> (a -> b) -> [b]
    match list {
        []          => [],
        [head, ..t] => [f(head), ..map(t, f)],
    }
}

// ── Structs & records ───────────────────────────────────────────
struct Point { x: Float, y: Float }
let p = Point { x: 1.0, y: 2.0 };
let Point { x, y } = p;        // destructuring

// ── Control flow ────────────────────────────────────────────────
while count < 10 { count = count + 1; }
for i in 0..10 { print(i); }
loop { if done { break; } }

// ── Errors as values ────────────────────────────────────────────
type Result = | Ok(a) | Err(e);
fn safe_div(a, b) { if b == 0 { Err("div by zero") } else { Ok(a / b) } }
```

### Design decisions and why each earns its place

| Feature | Forces you to implement |
|---|---|
| Closures over mutable captures | **Upvalues** — the hardest part of a VM. Cannot be faked with copies. |
| Everything is an expression | Uniform AST; no statement/expression duality hacks |
| Algebraic data types + match | Pattern compilation, **exhaustiveness checking** (a real decision-tree algorithm) |
| HM type inference, annotations optional | **Unification**, let-polymorphism, occurs check, and the hardest problem: *good error messages from inference* |
| Immutable by default, explicit `mut` | Resolver must track mutability; enables optimizations |
| First-class functions | Calling convention, call frames, tail position analysis |
| Recursive data (lists) | **Garbage collection** becomes mandatory, not decorative |
| No `null` | Option-as-ADT; the type system actually buys you something |

---

## §3 Architecture — The Two-Backend Spine

```
                        source: &str
                             │
                    ┌────────▼────────┐
                    │     Lexer       │  logos DFA or hand-rolled
                    │  → Vec<Token>   │  zero-copy: tokens hold Span, not String
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  Pratt Parser   │  recursive descent + precedence climbing
                    │  → AST arena    │  ERROR RECOVERY: never stop at first error
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │    Resolver     │  scopes, slot assignment, upvalue capture,
                    │  → Bindings     │  mutability check, unused-variable warnings
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  Type Inference │  Algorithm W: constraint gen → unification
                    │  → TypedAST     │  let-polymorphism, occurs check
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │  Exhaustiveness │  pattern matrix / usefulness algorithm
                    └────────┬────────┘
                             │
            ┌────────────────┴────────────────┐
            │                                 │
   ┌────────▼────────┐              ┌─────────▼─────────┐
   │  TREE-WALKING   │              │  BYTECODE COMPILER │
   │   INTERPRETER   │              │   → Chunk (Vec<u8>)│
   │                 │              └─────────┬─────────┘
   │ Env chain,      │                        │
   │ Rc<RefCell<>>   │              ┌─────────▼─────────┐
   │ ~100× slower    │              │    STACK VM       │
   │ Simple, correct │              │  dispatch loop,   │
   │ Great for       │              │  call frames,     │
   │ stepping/debug  │              │  upvalues, GC     │
   └────────┬────────┘              └─────────┬─────────┘
            │                                 │
            └────────────────┬────────────────┘
                             │
                    IDENTICAL OBSERVABLE BEHAVIOR
                    (enforced by a shared conformance suite)
```

**The invariant that makes this project work:** for every program in `tests/conformance/`, the tree-walker and the VM must produce byte-identical output, including error messages. Any divergence fails CI. This is what turns "I built two interpreters" into "I understand what an interpreter *is*, independent of implementation strategy."

---

## §4 Lexer

### Tokens are spans, not strings

```rust
// crates/ember-lexer/src/token.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span { pub start: u32, pub end: u32 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
// A Token is 12 bytes and Copy. It borrows nothing and owns nothing.
// Text is recovered on demand: &source[span.start as usize .. span.end as usize]
// This is why the whole pipeline can be zero-copy.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // literals
    Int, Float, Str, True, False,
    Ident,
    // keywords
    Let, Mut, Fn, If, Else, While, For, In, Loop, Break, Continue,
    Return, Match, Type, Struct, Import, Nil,
    // operators
    Plus, Minus, Star, Slash, Percent,
    Eq, EqEq, BangEq, Lt, LtEq, Gt, GtEq,
    AndAnd, OrOr, Bang, Pipe, Arrow, FatArrow, DotDot, Dot, Colon, ColonColon,
    // delimiters
    LParen, RParen, LBrace, RBrace, LBracket, RBracket, Comma, Semi,
    // trivia & control
    Comment, Whitespace, Newline,
    Eof,
    Error,           // the lexer NEVER panics; it emits Error and continues
}
```

### The lexer never fails

```rust
/// A lexer that returns Result<Vec<Token>, Error> is a lexer you cannot build
/// an editor on. Ours always produces a full token stream; unrecognised input
/// becomes TokenKind::Error with a span, and lexing continues. The LSP needs
/// tokens for text that is mid-edit and therefore always malformed.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) { … }
```

**String interning** happens at lex time for identifiers: each unique identifier gets a `Symbol(u32)`. Scope lookups then compare integers, not strings — this is worth ~15% on the front end and makes hash maps trivially fast.

**Trivia** (whitespace, comments) is retained in a side channel rather than discarded, because the formatter and the LSP's semantic-tokens response both need it.

---

## §5 Parser — Pratt / Precedence Climbing

### Why Pratt

Recursive descent handles statements beautifully and expressions badly: encoding 12 precedence levels as 12 mutually-recursive functions (`parse_equality` → `parse_comparison` → `parse_term` → …) means a deep call chain for every leaf, and adding an operator means editing several functions.

Pratt parsing collapses all of it into **one loop plus a precedence table**:

```rust
// crates/ember-parser/src/pratt.rs

#[derive(PartialEq, PartialOrd, Clone, Copy)]
#[repr(u8)]
pub enum Prec {
    None = 0, Assign, Or, And, Equality, Comparison,
    Term, Factor, Unary, Call, Primary,
}

impl TokenKind {
    /// Binding power when this token appears in INFIX position.
    fn infix_prec(self) -> Prec {
        match self {
            Eq                                  => Prec::Assign,
            OrOr                                => Prec::Or,
            AndAnd                              => Prec::And,
            EqEq | BangEq                       => Prec::Equality,
            Lt | LtEq | Gt | GtEq               => Prec::Comparison,
            Plus | Minus                        => Prec::Term,
            Star | Slash | Percent              => Prec::Factor,
            LParen | LBracket | Dot             => Prec::Call,
            _                                   => Prec::None,
        }
    }
}

impl<'src> Parser<'src> {
    /// The whole expression grammar. `min_prec` is the caller's binding power.
    pub fn expr(&mut self, min_prec: Prec) -> Idx<Expr> {
        // NUD ("null denotation"): the token can start an expression
        let mut lhs = self.prefix();

        // LED ("left denotation"): keep absorbing operators that bind tighter
        while self.peek().kind.infix_prec() > min_prec {
            lhs = self.infix(lhs);
        }
        lhs
    }

    fn infix(&mut self, lhs: Idx<Expr>) -> Idx<Expr> {
        let op = self.advance();
        let prec = op.kind.infix_prec();
        match op.kind {
            // LEFT-associative: recurse with THIS precedence, so an operator of
            // equal precedence terminates the inner loop and stays left-nested.
            Plus | Minus | Star | Slash | EqEq | Lt /* … */ => {
                let rhs = self.expr(prec);
                self.alloc(Expr::Binary { op, lhs, rhs })
            }
            // RIGHT-associative: recurse with prec - 1, so an equal-precedence
            // operator is absorbed by the inner call and nests to the right.
            Eq => {
                let rhs = self.expr(prec.lower());
                self.alloc(Expr::Assign { target: lhs, value: rhs })
            }
            LParen   => self.finish_call(lhs),
            LBracket => self.finish_index(lhs),
            Dot      => self.finish_field(lhs),
            _        => unreachable!(),
        }
    }
}
```

**Associativity is one line.** Left-assoc recurses with the same precedence; right-assoc recurses with one less. That single `.lower()` is the entire difference between `a - b - c` parsing as `(a-b)-c` and as `a-(b-c)`.

### AST arena

```rust
// Nodes live in flat Vecs and reference each other by index, not Box<Expr>.
// Three reasons: (1) cache locality — the whole AST is contiguous;
// (2) no recursive Drop, so a 100k-node AST doesn't blow the stack when freed;
// (3) Idx<T> is Copy, so tree transformations don't fight the borrow checker.

pub struct Ast {
    exprs: Vec<Expr>,
    stmts: Vec<Stmt>,
    pats:  Vec<Pattern>,
    pub spans: Vec<Span>,     // parallel to exprs — every node is locatable
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Idx<T> { raw: u32, _m: PhantomData<T> }

pub enum Expr {
    Int(i64), Float(f64), Str(Symbol), Bool(bool), Nil,
    Var(Symbol),
    Unary  { op: Token, operand: Idx<Expr> },
    Binary { op: Token, lhs: Idx<Expr>, rhs: Idx<Expr> },
    Assign { target: Idx<Expr>, value: Idx<Expr> },
    Call   { callee: Idx<Expr>, args: Vec<Idx<Expr>> },
    Index  { base: Idx<Expr>, index: Idx<Expr> },
    Field  { base: Idx<Expr>, name: Symbol },
    Lambda { params: Vec<Param>, body: Idx<Expr> },
    If     { cond: Idx<Expr>, then_: Idx<Expr>, else_: Option<Idx<Expr>> },
    Match  { scrutinee: Idx<Expr>, arms: Vec<MatchArm> },
    Block  { stmts: Vec<Idx<Stmt>>, tail: Option<Idx<Expr>> },
    List   { items: Vec<Idx<Expr>> },
    Struct { name: Symbol, fields: Vec<(Symbol, Idx<Expr>)> },
    Error,                     // ← the recovery node
}
```

### Error recovery — the feature that separates a toy from a tool

A parser that stops at the first error is useless for an editor, where the code is malformed 100% of the time you're typing.

```rust
/// Panic-mode recovery with synchronization points.
/// On a parse error we:
///   1. record a diagnostic
///   2. emit Expr::Error / Stmt::Error as a placeholder so the tree stays whole
///   3. skip tokens until we reach something that plausibly starts a new
///      statement — then resume as if nothing happened
///
/// Result: one missing semicolon produces ONE error, not forty.
fn synchronize(&mut self) {
    self.panicking = false;
    while !self.at_end() {
        if self.previous().kind == Semi { return; }
        match self.peek().kind {
            Let | Fn | If | While | For | Loop | Return | Match | Type | Struct | RBrace
                => return,
            _   => { self.advance(); }
        }
    }
}

/// Cascade suppression: while `panicking` is true we record no further
/// diagnostics. Without this, one bad token generates an error at every
/// subsequent parse step and the user sees noise instead of the real problem.
fn error_at(&mut self, span: Span, msg: impl Into<String>) {
    if self.panicking { return; }
    self.panicking = true;
    self.diags.push(Diagnostic::error(msg).with_span(span));
}
```

---

## §6 Diagnostics

Modelled on rustc; rendered with `ariadne`.

```rust
pub struct Diagnostic {
    pub severity: Severity,          // Error | Warning | Note | Help
    pub code: Option<&'static str>,  // "E0308"
    pub message: String,             // must stand alone, out of context
    pub labels: Vec<Label>,          // primary ^^^^ and secondary ---- spans
    pub notes: Vec<String>,          // "= note: …"
    pub help: Vec<Help>,             // machine-applicable suggestions
}

pub struct Label { pub span: Span, pub message: String, pub primary: bool }

pub struct Help {
    pub message: String,
    pub suggestion: Option<Suggestion>,  // (span, replacement) — the LSP turns
}                                        // this into a one-click code action
```

Target output:

```
error[E0308]: type mismatch in `if` branches
   ╭─[main.em:7:5]
   │
 6 │     let x = if flag {
   │             ── this `if` expression must have a single type
 7 │         42
   │         ─┬
   │          ╰── this branch has type `Int`
 8 │     } else {
 9 │         "hello"
   │         ───┬───
   │            ╰── this branch has type `String`
   │
   │ Help: both branches of an `if` must produce the same type
   │
   │ Note: `if` is an expression in ember, so its branches must agree
───╯
```

**Type errors are the hardest diagnostics to make good.** HM unification naturally reports *where the constraint failed*, which is often far from where the user made the mistake. Mitigation:
- every constraint carries a **provenance** (`FromIfBranches`, `FromCallArg { fn_span, arg_idx }`, `FromAnnotation`, …)
- unification failure formats using provenance, not raw types
- both contributing spans are labeled, never just one

---

## §7 Resolver

Runs between parsing and type checking. Answers: *which declaration does each name refer to, and where does it live at runtime?*

```rust
pub struct Resolver {
    scopes: Vec<Scope>,
    functions: Vec<FunctionCtx>,
    diags: Vec<Diagnostic>,
}

struct Scope { bindings: FxHashMap<Symbol, BindingInfo>, kind: ScopeKind }

struct BindingInfo {
    slot: u32,            // stack slot index within the enclosing frame
    mutable: bool,
    initialized: bool,    // `let x = x;` must error, not silently self-reference
    span: Span,
    used: bool,           // drives the unused-variable warning
}

pub struct Bindings {
    /// For every Var expression: where does it actually live?
    pub resolutions: FxHashMap<Idx<Expr>, Resolution>,
    /// Per function: how many locals, and which upvalues to capture.
    pub upvalues: FxHashMap<Idx<Expr>, Vec<UpvalueDesc>>,
    pub frame_sizes: FxHashMap<Idx<Expr>, u32>,
}

pub enum Resolution {
    Local  { slot: u32 },       // stack slot in the current frame — O(1)
    Upvalue{ index: u32 },      // captured from an enclosing function
    Global { symbol: Symbol },  // hash lookup, only for top level
}
```

### Upvalue capture

The core difficulty. When an inner function references a variable from an outer one, the compiler must arrange for that variable to outlive the outer function's stack frame.

```rust
/// Walk outward through enclosing functions looking for `name`.
/// Each level that we pass through must ALSO capture it, forming a chain, so
/// that a variable captured three levels deep is threaded through every
/// intermediate closure.
fn resolve_upvalue(&mut self, fn_idx: usize, name: Symbol) -> Option<u32> {
    if fn_idx == 0 { return None; }              // no enclosing function

    // Is it a local of the immediately enclosing function?
    if let Some(local_slot) = self.local_in(fn_idx - 1, name) {
        // Mark it captured: the compiler must emit OP_CLOSE_UPVALUE when this
        // local goes out of scope, moving it from stack to heap.
        self.mark_captured(fn_idx - 1, local_slot);
        return Some(self.add_upvalue(fn_idx, local_slot, /*is_local*/ true));
    }

    // Otherwise recurse — and thread the result through this level too.
    let outer = self.resolve_upvalue(fn_idx - 1, name)?;
    Some(self.add_upvalue(fn_idx, outer, /*is_local*/ false))
}
```

The resolver also produces: use-before-definition errors, assignment-to-immutable errors, unused-variable and unused-function warnings, and shadowing notes.

---

## §8 Type System — Hindley-Milner

Inference is **constraint generation followed by unification**, rather than Algorithm W's interleaved substitution. Separating the phases makes error messages dramatically better, because constraints carry provenance.

```rust
// crates/ember-types/src/ty.rs

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int, Float, Bool, String, Unit,
    Var(TyVarId),                     // unification variable — a hole
    Fun(Vec<Ty>, Box<Ty>),
    List(Box<Ty>),
    Adt(AdtId, Vec<Ty>),              // Shape, Option<Int>, …
    Record(BTreeMap<Symbol, Ty>),
}

/// A type SCHEME is a type with universally quantified variables.
/// This is what makes let-polymorphism work: `identity` is stored as
/// ∀a. a -> a, and each USE instantiates fresh variables, so
/// `identity(1)` and `identity("x")` don't conflict.
pub struct Scheme { pub vars: Vec<TyVarId>, pub ty: Ty }
```

### Constraints carry provenance

```rust
pub struct Constraint {
    pub lhs: Ty,
    pub rhs: Ty,
    pub origin: Origin,   // ← this is what makes errors readable
}

pub enum Origin {
    IfBranches   { if_span: Span, then_span: Span, else_span: Span },
    CallArgument { call_span: Span, arg_span: Span, param_idx: usize, fn_name: Option<Symbol> },
    BinaryOp     { op_span: Span, lhs_span: Span, rhs_span: Span, op: TokenKind },
    Annotation   { annot_span: Span, value_span: Span },
    MatchArms    { first_span: Span, this_span: Span },
    Return       { fn_span: Span, expr_span: Span },
    ListElement  { list_span: Span, elem_span: Span, index: usize },
}
```

### Unification

```rust
pub fn unify(&mut self, a: &Ty, b: &Ty, origin: &Origin) -> Result<(), Diagnostic> {
    let (a, b) = (self.resolve(a), self.resolve(b));   // follow existing bindings
    match (&a, &b) {
        (Ty::Var(v1), Ty::Var(v2)) if v1 == v2 => Ok(()),

        (Ty::Var(v), t) | (t, Ty::Var(v)) => {
            // OCCURS CHECK. Without it, `let f = |x| f(x)` produces the
            // constraint a = a -> b, and naively binding a to (a -> b) creates
            // an infinite type. Every substitution would then expand forever
            // and the compiler hangs. This check is 3 lines and non-optional.
            if self.occurs(*v, t) {
                return Err(self.infinite_type_error(*v, t, origin));
            }
            self.bind(*v, t.clone());
            Ok(())
        }

        (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
            if p1.len() != p2.len() { return Err(self.arity_error(p1.len(), p2.len(), origin)); }
            for (x, y) in p1.iter().zip(p2) { self.unify(x, y, origin)?; }
            self.unify(r1, r2, origin)
        }

        (Ty::List(x), Ty::List(y)) => self.unify(x, y, origin),

        (Ty::Adt(id1, a1), Ty::Adt(id2, a2)) if id1 == id2 => {
            for (x, y) in a1.iter().zip(a2) { self.unify(x, y, origin)?; }
            Ok(())
        }

        (x, y) if x == y => Ok(()),

        // Mismatch — format the message USING the origin, so the user sees
        // "these two `if` branches disagree", not "Int != String".
        _ => Err(self.mismatch_error(&a, &b, origin)),
    }
}
```

### Generalization and instantiation

```rust
/// Generalize at LET bindings only. Quantify every free type variable that is
/// NOT free in the surrounding environment — a variable still referenced by an
/// enclosing binding is not ours to quantify.
fn generalize(&self, env: &TyEnv, ty: &Ty) -> Scheme {
    let env_free = env.free_vars();
    let vars: Vec<_> = self.free_vars(ty).difference(&env_free).copied().collect();
    Scheme { vars, ty: ty.clone() }
}

/// Instantiate at every USE: replace each quantified variable with a fresh one.
/// This is precisely why `identity(1)` and `identity("x")` can coexist.
fn instantiate(&mut self, s: &Scheme) -> Ty {
    let sub: FxHashMap<_, _> = s.vars.iter().map(|&v| (v, self.fresh())).collect();
    self.substitute(&s.ty, &sub)
}
```

### The value restriction

Naïve generalization of mutable bindings is unsound: `let mut r = [];` would generalize to `∀a. [a]`, letting you push an `Int` and read a `String`. **Only generalize syntactic values** (literals, lambdas, variables, constructor applications) — never mutable bindings or general applications.

---

## §9 Exhaustiveness Checking

A `match` that doesn't cover every case must be a compile error, and the error must *name the missing case*. This is the usefulness algorithm (Maranget) on a pattern matrix.

```rust
/// A pattern is USEFUL relative to a matrix if some value matches it and no
/// row above. A match is EXHAUSTIVE iff the wildcard `_` is NOT useful against
/// the matrix of its arms — i.e. every value is already covered.
fn is_useful(matrix: &PatMatrix, v: &[Pattern], ty: &Ty) -> Usefulness { … }

pub fn check_exhaustive(arms: &[MatchArm], scrutinee_ty: &Ty) -> Vec<Diagnostic> {
    let mut diags = vec![];
    let mut matrix = PatMatrix::new();

    for arm in arms {
        // Unreachable-arm detection falls out for free: if this arm's pattern
        // is not useful against everything above it, it can never fire.
        if !is_useful(&matrix, &[arm.pat.clone()], scrutinee_ty).is_useful() {
            diags.push(Diagnostic::warning("unreachable pattern").with_span(arm.span));
        }
        matrix.push_row(vec![arm.pat.clone()]);
    }

    // Now ask: is `_` still useful? If so, the match misses something, and the
    // witnesses tell us exactly what.
    if let Usefulness::Useful(witnesses) = is_useful(&matrix, &[Pattern::Wild], scrutinee_ty) {
        diags.push(
            Diagnostic::error("non-exhaustive patterns")
                .with_note(format!("missing: {}",
                    witnesses.iter().map(fmt_pat).collect::<Vec<_>>().join(", ")))
                .with_help("add a `_ => …` arm to cover the remaining cases")
        );
    }
    diags
}
```

Output:

```
error[E0004]: non-exhaustive patterns
   ╭─[shapes.em:12:5]
   │
12 │     match s {
   │           ┬
   │           ╰── patterns `Rect(_, _)` and `Point` not covered
   │
   │ Note: `Shape` has 3 variants; 1 is covered
   │ Help: add a `_ => …` arm, or handle the remaining variants
───╯
```

---

## §10 Backend A — Tree-Walking Interpreter

Simple, obviously correct, and slow. Its job: be the reference implementation and the debugger-friendly backend.

```rust
#[derive(Clone)]
pub enum Value {
    Int(i64), Float(f64), Bool(bool), Nil,
    Str(Rc<String>),
    List(Rc<RefCell<Vec<Value>>>),
    Closure(Rc<Closure>),
    Native(Rc<NativeFn>),
    Adt(Rc<AdtValue>),
    Record(Rc<RefCell<FxHashMap<Symbol, Value>>>),
}

/// Environments form a chain. Rc<RefCell<…>> because closures share and mutate
/// them. This is exactly what makes the tree-walker slow: every variable access
/// is a pointer chase plus a runtime borrow check, and the values themselves
/// are scattered across the heap.
pub struct Env {
    values: FxHashMap<Symbol, Value>,
    parent: Option<Rc<RefCell<Env>>>,
}
```

Non-local control flow (`return`, `break`, `continue`) is threaded through the return type rather than via panics:

```rust
pub enum Flow { Normal(Value), Return(Value), Break, Continue }
type EvalResult = Result<Flow, RuntimeError>;
```

Using `panic!`/`catch_unwind` for `return` is a common shortcut that makes the interpreter unusable from WASM and impossible to single-step. Don't.

---

## §11 Backend B — Bytecode Compiler + VM

### Chunk

```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    /// Line info per instruction, run-length encoded. Naïvely storing one u32
    /// per byte doubles chunk size; RLE costs nothing because consecutive
    /// instructions almost always share a line.
    pub lines: Vec<(u32 /*line*/, u32 /*run_len*/)>,
}

#[repr(u8)]
pub enum Op {
    Constant, Nil, True, False, Pop,
    GetLocal, SetLocal, GetGlobal, SetGlobal, DefineGlobal,
    GetUpvalue, SetUpvalue, CloseUpvalue,
    GetField, SetField, GetIndex, SetIndex,
    Equal, Greater, Less, Add, Sub, Mul, Div, Mod, Not, Negate,
    Jump, JumpIfFalse, JumpIfTrue, Loop,
    Call, Closure, Return,
    MakeList, MakeRecord, MakeAdt,
    Match, TestVariant, Destructure,
    Print,
}
```

### Single-pass compilation

The compiler walks the typed AST and emits bytecode directly — no separate IR. Jump patching uses the standard backpatch:

```rust
/// Emit a jump with a placeholder operand; return its address for patching.
fn emit_jump(&mut self, op: Op) -> usize {
    self.emit(op);
    self.emit_u16(0xFFFF);            // placeholder
    self.chunk.code.len() - 2
}

/// Fill in the real offset once we know where the jump lands.
fn patch_jump(&mut self, at: usize) {
    let offset = self.chunk.code.len() - at - 2;
    assert!(offset <= u16::MAX as usize, "jump too large");
    self.chunk.code[at]     = (offset >> 8) as u8;
    self.chunk.code[at + 1] =  offset       as u8;
}
```

### The VM

```rust
pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    globals: FxHashMap<Symbol, Value>,
    /// Linked list of upvalues pointing at still-live stack slots, sorted by
    /// slot descending. When a scope ends we walk it and CLOSE every upvalue
    /// at or above the departing slot, moving the value onto the heap.
    open_upvalues: Option<Gc<Upvalue>>,
    gc: GcHeap,
}

pub struct CallFrame {
    closure: Gc<Closure>,
    ip: usize,            // index into closure.function.chunk.code
    slot_base: usize,     // where this frame's locals start in `stack`
}

impl Vm {
    fn run(&mut self) -> Result<Value, RuntimeError> {
        loop {
            let op = self.read_op();
            match op {
                // Locals are an INDEXED ARRAY ACCESS. This is the single
                // biggest win over the tree-walker's hash-map-in-a-chain.
                Op::GetLocal => {
                    let slot = self.read_u8() as usize;
                    let base = self.frame().slot_base;
                    self.push(self.stack[base + slot].clone());
                }
                Op::Add => {
                    let b = self.pop();
                    let a = self.pop();
                    self.push(self.add(a, b)?);
                }
                Op::Return => {
                    let result = self.pop();
                    let frame = self.frames.pop().unwrap();
                    // Any upvalues still pointing into this frame must be
                    // closed BEFORE the stack is truncated, or the closure
                    // ends up with a dangling reference.
                    self.close_upvalues(frame.slot_base);
                    if self.frames.is_empty() { return Ok(result); }
                    self.stack.truncate(frame.slot_base);
                    self.push(result);
                }
                // …
            }
        }
    }
}
```

### Upvalues at runtime

```rust
pub enum Upvalue {
    Open(usize),      // index into the VM stack — variable is still live there
    Closed(Value),    // moved to the heap — the stack slot is gone
}

/// Called when a scope ends. Walks the open-upvalue list, closing every one at
/// or above `from`, moving each value from the stack onto the heap.
///
/// This is the crux of the whole closure implementation: a captured variable
/// must transparently migrate from stack to heap at exactly the moment its
/// stack slot dies, with every closure holding it seeing the same cell.
fn close_upvalues(&mut self, from: usize) {
    while let Some(uv) = self.open_upvalues {
        match *uv.borrow() {
            Upvalue::Open(slot) if slot >= from => {
                let value = self.stack[slot].clone();
                *uv.borrow_mut() = Upvalue::Closed(value);
                self.open_upvalues = uv.next;
            }
            _ => break,
        }
    }
}
```

### Value representation & NaN boxing (stretch)

```rust
/// Baseline: a tagged enum. 16 bytes with the discriminant.
pub enum Value { Nil, Bool(bool), Int(i64), Float(f64), Obj(Gc<Obj>) }

/// Stretch goal: NaN boxing packs everything into one u64.
/// IEEE-754 doubles have 2^52 distinct NaN bit patterns that no arithmetic
/// produces. We use the spare bits to encode pointers and small immediates.
/// Halves Value size, doubles stack density, measurably speeds the VM.
///
///   quiet NaN: 0x7FF8_0000_0000_0000
///   pointer:   QNAN | SIGN | (ptr & 0x0000_FFFF_FFFF_FFFF)
///   nil:       QNAN | 1
///   false:     QNAN | 2
///   true:      QNAN | 3
///   any other bit pattern is a genuine f64
#[repr(transparent)]
pub struct NanValue(u64);
```

---

## §12 Garbage Collector

Mark-and-sweep, precise, with tri-color marking.

```rust
pub struct GcHeap {
    objects: Option<Gc<Obj>>,     // intrusive linked list of every allocation
    gray_stack: Vec<Gc<Obj>>,     // worklist for the mark phase
    bytes_allocated: usize,
    next_gc: usize,               // threshold; doubles after each collection
}

pub struct ObjHeader {
    marked: bool,
    next: Option<Gc<Obj>>,
    kind: ObjKind,
}
```

### Roots

Getting the root set wrong is the classic GC bug: an object is collected while still reachable, and the program crashes somewhere entirely unrelated.

```rust
fn mark_roots(&mut self, vm: &Vm) {
    // 1. Everything on the value stack
    for v in &vm.stack { self.mark_value(v); }
    // 2. Every closure in every call frame
    for f in &vm.frames { self.mark_object(f.closure); }
    // 3. Every open upvalue
    let mut uv = vm.open_upvalues;
    while let Some(u) = uv { self.mark_object(u); uv = u.next; }
    // 4. All globals
    for v in vm.globals.values() { self.mark_value(v); }
    // 5. Compiler roots — CRITICAL and easy to forget. If a collection is
    //    triggered mid-compilation, function objects the compiler has built
    //    but not yet attached to anything are unreachable from the VM and get
    //    swept, leaving the compiler holding freed memory.
    self.mark_compiler_roots();
}
```

### Stress mode

```rust
/// Debug builds collect on EVERY allocation. GC bugs are nondeterministic by
/// nature and can hide for months; stress mode makes them fire immediately and
/// reproducibly. The conformance suite runs entirely under stress mode.
#[cfg(feature = "gc-stress")]
const STRESS_GC: bool = true;
```

---

## §13 LSP Server

```rust
// crates/ember-lsp/src/lib.rs — built on tower-lsp

impl LanguageServer for EmberLsp {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncKind::INCREMENTAL.into()),
                completion_provider: Some(Default::default()),
                hover_provider: Some(true.into()),
                definition_provider: Some(true.into()),
                references_provider: Some(true.into()),
                document_symbol_provider: Some(true.into()),
                rename_provider: Some(true.into()),
                inlay_hint_provider: Some(true.into()),          // inferred types
                semantic_tokens_provider: Some(/* … */),
                code_action_provider: Some(true.into()),         // apply Help suggestions
                document_formatting_provider: Some(true.into()),
                ..Default::default()
            },
            ..Default::default()
        })
    }
}
```

| LSP feature | Backed by |
|---|---|
| `publishDiagnostics` | the same `Diagnostic` type the CLI renders with ariadne |
| `hover` | inferred `Ty` at the span, pretty-printed |
| `inlayHint` | inferred types on un-annotated `let`s and params — this is where HM *shows off* |
| `definition` / `references` | resolver's `Resolution` map |
| `rename` | resolver + span table |
| `completion` | in-scope bindings from the resolver + keywords + ADT variants |
| `semanticTokens` | lexer + resolver (distinguish local / global / param / type) |
| `codeAction` | `Help.suggestion` → `TextEdit` |

**Everything reuses the compiler.** The LSP is a thin protocol shim over the exact code the CLI runs — which is only possible because the lexer and parser never fail and always produce a full tree.

---

## §14 The Playground — Compiler in the Browser

The compiler compiles to WASM and runs entirely client-side. No backend. The playground *is* the compiler.

```rust
// crates/ember-wasm/src/lib.rs

#[wasm_bindgen]
pub fn compile_and_run(src: &str, backend: &str, opts: JsValue) -> JsValue {
    let opts: RunOptions = serde_wasm_bindgen::from_value(opts).unwrap();
    let result = ember::pipeline::run(src, backend.parse().unwrap(), opts);
    serde_wasm_bindgen::to_value(&result).unwrap()
}

/// Every intermediate stage is exposed, because seeing them IS the product.
#[wasm_bindgen] pub fn tokenize(src: &str)   -> JsValue { … }  // Vec<TokenView>
#[wasm_bindgen] pub fn parse_ast(src: &str)  -> JsValue { … }  // serialized tree
#[wasm_bindgen] pub fn type_info(src: &str)  -> JsValue { … }  // per-span types
#[wasm_bindgen] pub fn disassemble(src: &str)-> JsValue { … }  // annotated bytecode

/// Debugger: step one instruction (VM) or one AST node (tree-walker) and
/// return the complete machine state.
#[wasm_bindgen]
pub struct Debugger { /* … */ }

#[wasm_bindgen]
impl Debugger {
    #[wasm_bindgen(constructor)]
    pub fn new(src: &str, backend: &str) -> Result<Debugger, JsValue> { … }
    pub fn step(&mut self)      -> JsValue { … }  // VmState | EvalState
    pub fn step_over(&mut self) -> JsValue { … }
    pub fn run_to(&mut self, line: u32) -> JsValue { … }
    pub fn state(&self)         -> JsValue { … }  // stack, frames, heap, ip, line
}
```

---

## §15 Frontend — Eight Panels

Stack: `react` · `vite` · `typescript` · `@codemirror/*` · `tailwindcss` · `shadcn/ui` · `d3` · `recharts` · `zustand`

### Panel 1 · Editor (CodeMirror 6)
- Custom `ember` language mode via a StreamLanguage tokenizer driven by **our WASM lexer** — the editor and the compiler literally agree on what a token is
- Error squiggles from our diagnostics, with hover showing the full ariadne-style message
- **Inlay hints**: inferred types shown inline on un-annotated bindings
- Current-line highlight during debugging
- **AST linking**: selecting a node in Panel 3 highlights its exact span here, and vice versa
- Example gallery, share-via-URL (source compressed into the fragment)

### Panel 2 · Token Stream
Horizontal scrolling strip of token chips, colored by kind, each showing text + span. Hovering a chip highlights the source range. Makes lexing tangible in about three seconds.

### Panel 3 · AST Viewer ⭐
D3 collapsible tree. Node label = variant name; expanding shows fields. Clicking a node highlights its source span in the editor. A toggle switches between the raw AST and the **typed** AST, where every node is annotated with its inferred type — which is the clearest possible demonstration of what inference actually did.

### Panel 4 · Type Inference Trace ⭐
The panel that makes HM comprehensible:
1. **Constraints** generated, in order, each with its `Origin` and source span
2. **Unification steps** — a stepper: at each step, which two types are being unified, and what substitution results
3. **Substitution map** evolving live: `t3 ↦ Int`, `t7 ↦ t3 -> Bool`, …
4. **Final scheme** per binding, with quantifiers: `identity : ∀a. a → a`

Nobody explains let-polymorphism better than watching `identity` generalize to `∀a. a → a` and then instantiate to `Int → Int` and `String → String` at two different call sites, on screen.

### Panel 5 · Bytecode Disassembler
```
== main ==
0000    1 OP_CONSTANT         0 '42'
0002    | OP_DEFINE_GLOBAL    1 'x'
0004    2 OP_GET_GLOBAL       1 'x'
0006    | OP_CONSTANT         2 '0'
0008    | OP_GREATER
0009    | OP_JUMP_IF_FALSE    9 -> 18
0012    3 OP_CONSTANT         3 'pos'
0014    | OP_JUMP            14 -> 21
```
Each line links to its source line. During debugging the current instruction is highlighted and the stack effect is annotated.

### Panel 6 · Runtime State (Debugger) ⭐
Split view, updating on every step:
- **Value stack** — bottom to top, with frame boundaries marked
- **Call frames** — function name, ip, slot base
- **Locals** per frame, by slot
- **Upvalues** — open (pointing at a stack slot, drawn as an arrow) vs closed (holding a heap value). *Watching an upvalue close when its scope ends is the moment closures stop being magic.*
- **Heap** — D3 force graph of live objects with reference edges; GC roots outlined
- **GC** — bytes allocated, next threshold, collection count; mark and sweep phases animate

### Panel 7 · Backend Comparison ⭐
Run the same program on both backends side by side:

| | Tree-walk | Bytecode VM |
|---|---|---|
| `fib(30)` | 4,120 ms | 148 ms |
| Allocations | 2,891,443 | 31 |
| Peak heap | 184 MB | 0.4 MB |
| Instructions | — | 24,157,817 |

Plus a Recharts chart of runtime vs input size for both. **And an output-equality assertion**: a green check confirming both produced identical results, red if they diverged. That assertion is the conformance suite running live.

### Panel 8 · Pipeline Explorer
A horizontal strip of stages — Source → Tokens → AST → Resolved → Typed → Bytecode → Output — with timing for each. Clicking a stage jumps to its panel. Total compile time broken down by phase.

---

## §16 CLI

```
ember run FILE [--backend tree|vm] [--time] [--gc-stress]
ember repl [--backend vm] [--show-types]
ember check FILE                    # diagnostics only, no execution
ember fmt FILE [--check]
ember lsp                           # stdio language server

# The teaching subcommands
ember tokens FILE                   # token stream with spans
ember ast FILE [--json] [--typed]   # pretty-printed tree
ember types FILE                    # every binding with its inferred scheme
ember trace FILE                    # constraint generation + unification steps
ember disasm FILE                   # annotated bytecode
ember bench FILE                    # both backends, timing + allocation counts
ember debug FILE                    # interactive stepping TUI
ember explain E0308                 # extended error documentation
```

`ember trace` is the flagship: it prints the entire inference derivation, constraint by constraint, with the substitution after each unification step.

---

## §17 File Structure

```
ember/
├── Cargo.toml                          # workspace
├── crates/
│   ├── ember-span/                     # Span, SourceMap, line/col
│   ├── ember-diag/                     # Diagnostic, Label, Help, ariadne render
│   ├── ember-lexer/                    # Token, TokenKind, lex(), interner
│   ├── ember-ast/                      # Ast arena, Expr, Stmt, Pattern, Idx<T>
│   ├── ember-parser/                   # Pratt parser, error recovery
│   ├── ember-resolve/                  # scopes, slots, upvalues, mutability
│   ├── ember-types/                    # Ty, Scheme, constraints, unify, exhaustiveness
│   ├── ember-tree/                     # tree-walking interpreter
│   ├── ember-bytecode/                 # Chunk, Op, disassembler
│   ├── ember-compile/                  # AST → bytecode
│   ├── ember-vm/                       # VM, frames, upvalues, NaN boxing
│   ├── ember-gc/                       # mark-sweep heap, Gc<T>, stress mode
│   ├── ember-fmt/                      # formatter
│   ├── ember-lsp/                      # tower-lsp server
│   ├── ember-wasm/                     # wasm-bindgen surface
│   └── ember-cli/                      # clap CLI + REPL + debug TUI
├── playground/                         # React app
│   └── src/
│       ├── wasm/                       # generated bindings
│       ├── components/{editor,tokens,ast,types,bytecode,runtime,compare,pipeline}/
│       ├── store/                      # zustand
│       └── lib/
├── tests/
│   ├── conformance/                    # .em + .expected — BOTH backends must match
│   ├── diagnostics/                    # .em + .stderr snapshots
│   └── snapshots/                      # insta: ASTs, disassembly
├── examples/                           # fib, closures, adts, generics, sorting, brainfuck
└── docs/                               # LANGUAGE.md, IMPLEMENTATION.md, ERRORS.md
```

---

## §18 Testing

| Suite | What it guarantees |
|---|---|
| `tests/conformance/` | **The core invariant**: tree-walker and VM produce identical output for every program, under GC stress |
| `tests/diagnostics/` | Error messages are exact — snapshot-tested, so improving one is a deliberate act |
| `insta` snapshots | AST shape and bytecode disassembly don't change silently |
| `proptest` | Parser: `parse(print(ast)) == ast`. Lexer: spans tile the source with no gaps or overlaps. |
| Fuzzing | Random bytes into the lexer and parser: never panic, always terminate |
| GC stress | Every conformance program under collect-on-every-allocation |
| `criterion` | fib, loops, closures, list ops — both backends |
| Type suite | Well-typed programs infer expected schemes; ill-typed programs produce the expected error at the expected span |

---

## §19 Correctness Properties

1. **Backend equivalence.** For every conformance program, tree-walker and VM produce byte-identical stdout and identical error messages. This is the project's central claim.
2. **Lexer totality.** `lex` never panics and never returns early. Spans tile the input exactly.
3. **Parser totality.** `parse` always returns a tree, however malformed the input. Errors become `Error` nodes.
4. **Recovery quality.** One syntax error produces one diagnostic, not a cascade.
5. **Soundness.** A well-typed program does not fail with a type error at runtime.
6. **Principal types.** Inference produces the most general type; `identity` is `∀a. a → a`, not `Int → Int`.
7. **Occurs check.** Self-referential constraints are rejected as infinite types, never looped on.
8. **Value restriction.** Mutable bindings are not generalized.
9. **Exhaustiveness.** Every non-exhaustive match is rejected with the missing patterns named.
10. **Closure semantics.** A closure over a mutable variable observes later mutations; two closures over the same variable share one cell.
11. **GC soundness.** No reachable object is ever collected. Verified under stress mode across the whole conformance suite.
12. **Span accuracy.** Every diagnostic points at the exact source range responsible.
13. **Formatter idempotence.** `fmt(fmt(x)) == fmt(x)`, and formatting never changes program meaning.

---

## §20 Performance Targets

| Metric | Target |
|---|---|
| Lexing | > 50 MB/s |
| Parsing | > 15 MB/s |
| Type inference (10k-line file) | < 150 ms |
| Full pipeline to bytecode (1k lines) | < 20 ms |
| `fib(30)` — tree-walker | ~4 s (the honest baseline) |
| `fib(30)` — VM | < 250 ms (**> 15× faster**) |
| `fib(30)` — VM with NaN boxing | < 180 ms |
| VM dispatch overhead | < 6 ns/instruction |
| GC pause (1 MB live) | < 3 ms |
| LSP `didChange` → diagnostics | < 40 ms (10k lines) |
| WASM bundle (gzipped) | < 900 KB |
| Playground cold start | < 1.2 s |
