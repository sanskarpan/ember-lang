# CLAUDE CODE PROMPT — `ember`: A Programming Language, End to End

## Project Mission

Build a complete programming language implementation from scratch:

- **Backend: Rust** — hand-written lexer, Pratt parser with real error recovery, resolver with upvalue capture, Hindley-Milner type inference with provenance-carrying constraints, match exhaustiveness checking, **two execution backends** (tree-walking interpreter + bytecode VM), a mark-sweep garbage collector, a formatter, and an LSP server
- **Frontend: React + TypeScript + Vite + CodeMirror 6 + Tailwind + shadcn/ui + D3** — a playground where the compiler *is* the app, compiled to WASM. Eight panels exposing every pipeline stage.

**Read `lang-SPEC.md` and `lang-CHECKLIST.md` before writing any code.**

### Three rules that override everything

1. **No parser generator.** No `lalrpop`, `pest`, `chumsky`, or `nom`. The hand-written Pratt parser is ~400 lines and is the single most valuable thing in this project to understand. A generator hides exactly what you're here to learn.

2. **The conformance suite is the spine.** Every program in `tests/conformance/` must produce **byte-identical output on both backends**, including error messages, and must pass again under `gc-stress`. Start writing conformance tests in Phase 8, not at the end. This one check is what turns "I wrote two interpreters" into "I understand what an interpreter is."

3. **The lexer and parser never fail.** `lex` always returns a full token stream. `parse` always returns a complete tree. Malformed input produces `Error` nodes and diagnostics, never an early return. Everything downstream — error recovery, the LSP, the playground's live feedback — depends on this.

---

## Phase 0 — Bootstrap

```bash
cargo new --lib ember && cd ember
# workspace with 16 members per SPEC §17

cargo add -p ember-lexer  logos string-interner rustc-hash
cargo add -p ember-diag   ariadne
cargo add -p ember-cli    clap rustyline anyhow
cargo add -p ember-lsp    tower-lsp tokio --features tokio/full
cargo add -p ember-wasm   wasm-bindgen serde-wasm-bindgen serde --features serde/derive
cargo add --dev insta criterion proptest

cd playground
bun create vite . --template react-ts
bun add @codemirror/state @codemirror/view @codemirror/language \
        @codemirror/commands @codemirror/lint @codemirror/autocomplete \
        @lezer/highlight d3 recharts zustand clsx lucide-react
bun add -d tailwindcss postcss autoprefixer @types/d3 vite-plugin-wasm vite-plugin-top-level-await
bunx tailwindcss init -p && bunx shadcn@latest init
```

---

## Phase 1 — Lexer: tokens are spans, not strings

```rust
// crates/ember-lexer/src/token.rs

/// A Token is 12 bytes and Copy. It owns nothing and borrows nothing.
/// Text is recovered on demand as &src[span.start..span.end].
///
/// This is why the entire front end can be zero-copy: no String allocation
/// per token, and the parser can freely copy tokens around without lifetimes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token { pub kind: TokenKind, pub span: Span }
```

```rust
// crates/ember-lexer/src/lex.rs

/// CRITICAL DESIGN DECISION: this returns diagnostics ALONGSIDE tokens, never
/// Result<Vec<Token>, Error>.
///
/// A lexer that stops at the first bad character cannot power an editor, where
/// the buffer is malformed 100% of the time you are typing. Unrecognised input
/// becomes TokenKind::Error with a span and lexing continues.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer::new(src);
    let mut tokens = Vec::with_capacity(src.len() / 4);  // ~4 bytes per token
    loop {
        let tok = lx.next_token();
        let done = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if done { break; }
    }
    (tokens, lx.diags)
}

impl<'src> Lexer<'src> {
    fn next_token(&mut self) -> Token {
        self.skip_trivia();
        let start = self.pos;
        let Some(c) = self.advance() else {
            return Token { kind: TokenKind::Eof, span: Span::new(start, start) };
        };

        let kind = match c {
            '0'..='9' => self.number(start),
            'a'..='z' | 'A'..='Z' | '_' => self.ident_or_keyword(start),
            '"' => self.string(start),

            // MAXIMAL MUNCH: always try the longest operator first.
            // Getting this order wrong means `a == b` lexes as `a = = b`,
            // and `1..10` lexes as `1. . 10` (a float, a dot, an int).
            '=' if self.eat('=') => TokenKind::EqEq,
            '=' if self.eat('>') => TokenKind::FatArrow,
            '=' => TokenKind::Eq,
            '!' if self.eat('=') => TokenKind::BangEq,
            '!' => TokenKind::Bang,
            '<' if self.eat('=') => TokenKind::LtEq,
            '<' => TokenKind::Lt,
            '>' if self.eat('=') => TokenKind::GtEq,
            '>' => TokenKind::Gt,
            '-' if self.eat('>') => TokenKind::Arrow,
            '-' => TokenKind::Minus,
            '&' if self.eat('&') => TokenKind::AndAnd,
            '|' if self.eat('|') => TokenKind::OrOr,
            '|' => TokenKind::Pipe,
            ':' if self.eat(':') => TokenKind::ColonColon,
            ':' => TokenKind::Colon,
            '.' if self.eat('.') => TokenKind::DotDot,
            '.' => TokenKind::Dot,

            '+' => TokenKind::Plus, '*' => TokenKind::Star, '%' => TokenKind::Percent,
            '/' => TokenKind::Slash,
            '(' => TokenKind::LParen, ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace, '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket, ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma, ';' => TokenKind::Semi,

            other => {
                // Never panic. Record and continue.
                self.error(Span::new(start, self.pos),
                           format!("unexpected character `{other}`"));
                TokenKind::Error
            }
        };
        Token { kind, span: Span::new(start, self.pos) }
    }

    /// `1..10` must lex as Int DotDot Int, NOT Float Dot Int.
    /// A digit followed by `.` is only a float if the NEXT char is also a digit.
    fn number(&mut self, start: u32) -> TokenKind {
        while self.peek().is_ascii_digit() || self.peek() == '_' { self.advance(); }

        if self.peek() == '.' && self.peek_at(1).is_ascii_digit() {
            self.advance();                                   // consume '.'
            while self.peek().is_ascii_digit() || self.peek() == '_' { self.advance(); }
            self.maybe_exponent();
            return TokenKind::Float;
        }
        if matches!(self.peek(), 'e' | 'E') { self.maybe_exponent(); return TokenKind::Float; }
        TokenKind::Int
    }

    /// Nested block comments: `/* /* */ */` must close correctly.
    /// A naive "scan to the first */" gets this wrong and swallows the rest
    /// of the file.
    fn block_comment(&mut self) {
        let start = self.pos;
        let mut depth = 1usize;
        while depth > 0 {
            match (self.advance(), self.peek()) {
                (None, _) => {
                    self.error(Span::new(start, self.pos), "unterminated block comment");
                    return;
                }
                (Some('/'), '*') => { self.advance(); depth += 1; }
                (Some('*'), '/') => { self.advance(); depth -= 1; }
                _ => {}
            }
        }
    }
}
```

**The test that catches almost every lexer bug:**

```rust
#[test]
fn spans_tile_the_source_exactly() {
    // If spans have gaps or overlaps, every downstream diagnostic points at
    // the wrong place — and you will not notice until you are debugging the
    // type checker and the caret is three characters off.
    for src in CORPUS {
        let (tokens, _) = lex(src);
        let mut cursor = 0u32;
        for t in &tokens {
            assert!(t.span.start >= cursor, "overlapping span at {:?}", t);
            // gap is only allowed where trivia was skipped
            assert!(src[cursor as usize..t.span.start as usize]
                        .chars().all(|c| c.is_whitespace())
                    || src[cursor as usize..t.span.start as usize].contains("//")
                    || src[cursor as usize..t.span.start as usize].contains("/*"),
                    "non-trivia gap before {:?}", t);
            cursor = t.span.end;
        }
        assert_eq!(cursor as usize, src.len());
    }
}
```

---

## Phase 3 — Pratt Parser

### The whole expression grammar in one loop

```rust
// crates/ember-parser/src/pratt.rs

impl<'src> Parser<'src> {
    /// Pratt parsing / precedence climbing.
    ///
    /// Recursive descent handles expressions badly: twelve precedence levels
    /// become twelve mutually-recursive functions, so every leaf costs a
    /// twelve-deep call chain, and adding an operator means editing several
    /// functions. Pratt collapses all of it into this one loop plus a table.
    pub fn expr(&mut self, min_prec: Prec) -> Idx<Expr> {
        // NUD — "null denotation": this token can START an expression
        let mut lhs = self.prefix();

        // LED — "left denotation": absorb operators that bind more tightly
        // than our caller's precedence
        while self.peek().kind.infix_prec() > min_prec {
            lhs = self.infix(lhs);
        }
        lhs
    }

    fn infix(&mut self, lhs: Idx<Expr>) -> Idx<Expr> {
        let op = self.advance();
        let prec = op.kind.infix_prec();

        match op.kind {
            // ── LEFT-ASSOCIATIVE ────────────────────────────────────────
            // Recurse with THIS precedence. An operator of equal precedence
            // fails the `> min_prec` test in the inner loop, so it returns
            // and gets absorbed by OUR loop instead → left nesting.
            //   1 - 2 - 3  →  ((1 - 2) - 3)
            TokenKind::Plus | TokenKind::Minus | TokenKind::Star | TokenKind::Slash
            | TokenKind::Percent | TokenKind::EqEq | TokenKind::BangEq
            | TokenKind::Lt | TokenKind::LtEq | TokenKind::Gt | TokenKind::GtEq
            | TokenKind::AndAnd | TokenKind::OrOr => {
                let rhs = self.expr(prec);
                self.alloc_expr(Expr::Binary { op, lhs, rhs }, self.span_from(lhs))
            }

            // ── RIGHT-ASSOCIATIVE ───────────────────────────────────────
            // Recurse with prec - 1. Now an equal-precedence operator DOES
            // pass the inner test, so the inner call absorbs it → right nesting.
            //   a = b = c  →  (a = (b = c))
            //
            // That single `.lower()` is the entire difference between left and
            // right associativity. It is worth staring at until it is obvious.
            TokenKind::Eq => {
                let rhs = self.expr(prec.lower());
                if !self.is_valid_assign_target(lhs) {
                    self.error_at(self.span_of(lhs),
                        "invalid assignment target")
                        .with_help("only variables, fields, and index expressions can be assigned to");
                }
                self.alloc_expr(Expr::Assign { target: lhs, value: rhs }, self.span_from(lhs))
            }

            TokenKind::LParen   => self.finish_call(lhs),
            TokenKind::LBracket => self.finish_index(lhs),
            TokenKind::Dot      => self.finish_field(lhs),
            _ => unreachable!("infix_prec() returned > None for {:?}", op.kind),
        }
    }
}
```

### Error recovery — the feature that makes this a tool, not a toy

```rust
/// Panic-mode recovery with synchronization points.
///
/// Three mechanics working together:
///   1. `panicking` suppresses cascading errors
///   2. `Expr::Error` / `Stmt::Error` placeholders keep the tree WHOLE, so the
///      resolver and type checker can still run on the good parts
///   3. `synchronize()` skips to a plausible restart point
///
/// Result: one missing semicolon produces ONE diagnostic, not forty.
impl<'src> Parser<'src> {
    fn error_at(&mut self, span: Span, msg: impl Into<String>) -> &mut Diagnostic {
        // Cascade suppression. Without this, a single bad token generates an
        // error at every subsequent parse step and the real problem is buried.
        if self.panicking {
            self.diags.push(Diagnostic::suppressed());
            return self.diags.last_mut().unwrap();
        }
        self.panicking = true;
        self.diags.push(Diagnostic::error(msg).with_primary(span, "here"));
        self.diags.last_mut().unwrap()
    }

    fn synchronize(&mut self) {
        self.panicking = false;
        while !self.at_end() {
            // Just consumed a `;` — a statement boundary. Good place to resume.
            if self.previous().kind == TokenKind::Semi { return; }
            match self.peek().kind {
                // These tokens can only start a new statement, so whatever
                // garbage preceded them is over.
                TokenKind::Let | TokenKind::Fn | TokenKind::If | TokenKind::While
                | TokenKind::For | TokenKind::Loop | TokenKind::Return
                | TokenKind::Match | TokenKind::Type | TokenKind::Struct
                | TokenKind::RBrace => return,
                _ => { self.advance(); }
            }
        }
    }

    /// Unclosed delimiters must report at the OPENING delimiter. Reporting at
    /// EOF ("unexpected end of file") is technically true and completely
    /// useless in a 2000-line file.
    fn expect_close(&mut self, open: Token, close: TokenKind) -> Token {
        if self.check(close) { return self.advance(); }
        self.error_at(open.span, format!("unclosed `{}`", open.kind.text()))
            .with_secondary(self.peek().span, "expected the matching close here");
        Token { kind: close, span: self.peek().span }   // synthesize and continue
    }
}
```

**Tests that gate Phase 4:**

```rust
#[test]
fn one_missing_semicolon_is_one_error() {
    // The single most important recovery property. Without cascade
    // suppression this produces ~15 diagnostics and the user gives up.
    let src = "let a = 1\nlet b = 2;\nlet c = 3;";
    let (_, diags) = parse(src);
    assert_eq!(diags.iter().filter(|d| d.severity == Severity::Error).count(), 1);
}

#[test]
fn recovery_preserves_surrounding_code() {
    let src = "fn good1() { 1 }\nfn @@@ bad\nfn good2() { 2 }";
    let (ast, diags) = parse(src);
    assert!(!diags.is_empty());
    // Both good functions must still be in the tree — that's what makes the
    // LSP usable while you're mid-edit.
    assert_eq!(ast.functions().count(), 2);
}

#[test]
fn associativity() {
    assert_eq!(sexpr("1 - 2 - 3"),  "(- (- 1 2) 3)");   // left
    assert_eq!(sexpr("a = b = c"),  "(= a (= b c))");   // right
    assert_eq!(sexpr("1 + 2 * 3"),  "(+ 1 (* 2 3))");   // precedence
    assert_eq!(sexpr("-a + b"),     "(+ (- a) b)");     // unary binds tighter
    assert_eq!(sexpr("-f(x)"),      "(- (call f x))");  // call binds tightest
}
```

---

## Phase 4 — Resolver: upvalue capture

```rust
// crates/ember-resolve/src/upvalue.rs

/// THE hardest part of implementing closures.
///
/// When an inner function references an outer function's local, the compiler
/// must arrange for that variable to outlive the outer function's stack frame.
/// The mechanism is an "upvalue": a level of indirection that starts pointing
/// at a stack slot and gets promoted to the heap when the slot dies.
///
/// The subtlety: a variable captured three levels deep must be threaded
/// through EVERY intermediate function, because each closure can only capture
/// from its immediate enclosing frame.
///
///     fn outer() {
///         let x = 1;
///         || {                    // must capture x as a LOCAL upvalue
///             || {                // must capture x as an UPVALUE upvalue
///                 || { x }        // ...and again
///             }
///         }
///     }
fn resolve_upvalue(&mut self, fn_idx: usize, name: Symbol) -> Option<u32> {
    if fn_idx == 0 { return None; }   // top level — must be a global

    // Case 1: it's a local of the IMMEDIATELY enclosing function.
    if let Some(slot) = self.local_slot_in(fn_idx - 1, name) {
        // Mark it captured so the compiler emits OP_CLOSE_UPVALUE when this
        // local goes out of scope. Forgetting this leaves the closure holding
        // a stack slot that has been reused by something else — the bug
        // manifests as a closure whose captured variable mysteriously changes.
        self.functions[fn_idx - 1].locals[slot as usize].captured = true;
        return Some(self.add_upvalue(fn_idx, slot, /* is_local */ true));
    }

    // Case 2: it's further out. Recurse — and thread the result through THIS
    // level as well, so the chain is complete.
    let outer_index = self.resolve_upvalue(fn_idx - 1, name)?;
    Some(self.add_upvalue(fn_idx, outer_index, /* is_local */ false))
}

/// Deduplicate: capturing the same variable twice must reuse one index, or
/// two closures over the same variable end up with SEPARATE cells and stop
/// seeing each other's mutations.
fn add_upvalue(&mut self, fn_idx: usize, index: u32, is_local: bool) -> u32 {
    let ups = &mut self.functions[fn_idx].upvalues;
    if let Some(i) = ups.iter().position(|u| u.index == index && u.is_local == is_local) {
        return i as u32;
    }
    ups.push(UpvalueDesc { index, is_local });
    (ups.len() - 1) as u32
}
```

---

## Phase 5 — Type Inference

### Constraints carry provenance — this is the whole trick

```rust
// crates/ember-types/src/constraint.rs

/// Textbook Algorithm W interleaves substitution with traversal, which makes
/// it compact but produces terrible errors: it reports "Int != String" at
/// whatever point unification happened to fail, which is often nowhere near
/// the user's actual mistake.
///
/// Separating constraint GENERATION from constraint SOLVING lets every
/// constraint carry an Origin, so the error message can say
/// "these two `if` branches disagree" and label BOTH branches.
pub struct Constraint { pub lhs: Ty, pub rhs: Ty, pub origin: Origin }

pub enum Origin {
    IfBranches   { if_span: Span, then_span: Span, else_span: Span },
    CallArgument { call_span: Span, arg_span: Span, param_idx: usize, fn_name: Option<Symbol> },
    BinaryOp     { op_span: Span, lhs_span: Span, rhs_span: Span, op: TokenKind },
    Annotation   { annot_span: Span, value_span: Span },
    MatchArms    { first_span: Span, this_span: Span },
    Return       { fn_span: Span, expr_span: Span },
    ListElement  { list_span: Span, elem_span: Span, index: usize },
    WhileCond    { span: Span },
}
```

### Unification with the occurs check

```rust
pub fn unify(&mut self, a: &Ty, b: &Ty, origin: &Origin) -> Result<(), Diagnostic> {
    let a = self.resolve(a);
    let b = self.resolve(b);

    match (&a, &b) {
        (Ty::Var(v1), Ty::Var(v2)) if v1 == v2 => Ok(()),

        (Ty::Var(v), t) | (t, Ty::Var(v)) => {
            // ── THE OCCURS CHECK ────────────────────────────────────────
            // Without it, `let f = |x| f(x)` generates the constraint
            //     a = a -> b
            // and naively binding a := (a -> b) creates an INFINITE TYPE.
            // Every subsequent substitution expands it further and the
            // compiler hangs, allocating until the OOM killer arrives.
            //
            // Three lines. Absolutely non-optional.
            if self.occurs_in(*v, t) {
                return Err(self.infinite_type_error(*v, t, origin));
            }
            self.bind(*v, t.clone());
            Ok(())
        }

        (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
            if p1.len() != p2.len() {
                return Err(self.arity_error(p1.len(), p2.len(), origin));
            }
            for (x, y) in p1.iter().zip(p2.iter()) { self.unify(x, y, origin)?; }
            self.unify(r1, r2, origin)
        }

        (Ty::List(x), Ty::List(y)) => self.unify(x, y, origin),

        (Ty::Adt(id1, args1), Ty::Adt(id2, args2)) if id1 == id2 => {
            for (x, y) in args1.iter().zip(args2.iter()) { self.unify(x, y, origin)?; }
            Ok(())
        }

        (x, y) if x == y => Ok(()),

        // Format the message FROM THE ORIGIN, not from the raw types.
        _ => Err(self.mismatch_error(&a, &b, origin)),
    }
}

fn occurs_in(&self, var: TyVarId, ty: &Ty) -> bool {
    match self.resolve(ty) {
        Ty::Var(v) => v == var,
        Ty::Fun(ps, r) => ps.iter().any(|p| self.occurs_in(var, p)) || self.occurs_in(var, &r),
        Ty::List(t) => self.occurs_in(var, &t),
        Ty::Adt(_, args) => args.iter().any(|a| self.occurs_in(var, a)),
        Ty::Record(fs) => fs.values().any(|t| self.occurs_in(var, t)),
        _ => false,
    }
}

/// Error rendering driven by Origin — this is what the whole provenance
/// machinery buys.
fn mismatch_error(&self, a: &Ty, b: &Ty, origin: &Origin) -> Diagnostic {
    match origin {
        Origin::IfBranches { if_span, then_span, else_span } =>
            Diagnostic::error("type mismatch in `if` branches")
                .with_code("E0308")
                .with_secondary(*if_span, "this `if` expression must have a single type")
                .with_primary(*then_span, format!("this branch has type `{}`", self.display(a)))
                .with_primary(*else_span, format!("this branch has type `{}`", self.display(b)))
                .with_help("both branches of an `if` must produce the same type")
                .with_note("`if` is an expression in ember, so its branches must agree"),

        Origin::CallArgument { call_span, arg_span, param_idx, fn_name } =>
            Diagnostic::error("argument type mismatch")
                .with_code("E0308")
                .with_primary(*arg_span,
                    format!("expected `{}`, found `{}`", self.display(a), self.display(b)))
                .with_secondary(*call_span,
                    match fn_name {
                        Some(n) => format!("in this call to `{n}` (argument {})", param_idx + 1),
                        None    => format!("in this call (argument {})", param_idx + 1),
                    }),

        // … one arm per Origin variant
    }
}
```

### Generalization, instantiation, and the value restriction

```rust
/// Generalize ONLY at let bindings. Quantify every free type variable that is
/// not free in the surrounding environment — a variable still referenced by an
/// enclosing binding is not ours to quantify.
fn generalize(&self, env: &TyEnv, ty: &Ty) -> Scheme {
    let env_free = env.free_vars(self);
    let vars: Vec<_> = self.free_vars(ty).difference(&env_free).copied().collect();
    Scheme { vars, ty: ty.clone() }
}

/// Instantiate at every USE with fresh variables.
///
/// THIS is why `identity(1)` and `identity("x")` coexist. `identity` is stored
/// as ∀a. a -> a; the first call instantiates a := t1 and unifies t1 = Int,
/// the second instantiates a := t2 and unifies t2 = String. No conflict,
/// because they are different variables.
fn instantiate(&mut self, s: &Scheme) -> Ty {
    let sub: FxHashMap<TyVarId, Ty> =
        s.vars.iter().map(|&v| (v, self.fresh())).collect();
    self.substitute(&s.ty, &sub)
}

/// THE VALUE RESTRICTION.
///
/// Generalizing a mutable binding is UNSOUND:
///     let mut r = [];        // would generalize to ∀a. [a]
///     push(r, 1);            // instantiate a := Int
///     let s: String = r[0];  // instantiate a := String — accepted! CRASH.
///
/// Only syntactic values (literals, lambdas, variables, constructor
/// applications) may be generalized. Never mutable bindings, never general
/// applications. This is exactly the bug that forced the value restriction
/// into ML in the first place.
fn should_generalize(&self, is_mut: bool, init: Idx<Expr>) -> bool {
    !is_mut && self.is_syntactic_value(init)
}

fn is_syntactic_value(&self, e: Idx<Expr>) -> bool {
    matches!(self.ast.expr(e),
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil
        | Expr::Lambda { .. } | Expr::Var(_))
}
```

**The test that proves let-polymorphism works:**

```rust
#[test]
fn let_polymorphism() {
    let src = r#"
        fn identity(x) { x }
        let a = identity(1);
        let b = identity("hello");
    "#;
    let types = infer(src).unwrap();
    // Without generalize/instantiate this fails: the first call would bind
    // identity's type variable to Int and the second would be a type error.
    assert_eq!(types.scheme_of("identity").to_string(), "forall a. a -> a");
    assert_eq!(types.type_of("a").to_string(), "Int");
    assert_eq!(types.type_of("b").to_string(), "String");
}

#[test]
fn occurs_check_terminates() {
    // Without the occurs check this HANGS, allocating until OOM.
    let result = std::thread::spawn(|| infer("let f = |x| f(x);"))
        .join().expect("inference panicked or hung");
    let err = result.unwrap_err();
    assert!(err.message.contains("infinite type"));
}

#[test]
fn value_restriction_prevents_unsoundness() {
    let src = r#"
        let mut r = [];
        push(r, 1);
        let s: String = r[0];
    "#;
    // Must be a type error. If this compiles, generalization is unsound.
    assert!(infer(src).is_err());
}
```

---

## Phase 9 — VM: upvalues at runtime

```rust
// crates/ember-vm/src/upvalue.rs

pub enum Upvalue {
    /// Still pointing at a live VM stack slot.
    Open(usize),
    /// Stack slot is gone; the value now lives on the heap.
    Closed(Value),
}

impl Vm {
    /// Capture a stack slot as an upvalue.
    ///
    /// CRITICAL: search the open list first and REUSE an existing upvalue for
    /// the same slot. If two closures capture the same variable and each gets
    /// its own cell, they stop seeing each other's mutations — and this is
    /// exactly the semantics people write closures to get.
    ///
    /// The list is kept sorted by slot DESCENDING, so the search stops early
    /// and close_upvalues can walk a prefix.
    fn capture_upvalue(&mut self, slot: usize) -> Gc<Upvalue> {
        let mut prev: Option<Gc<Upvalue>> = None;
        let mut cur = self.open_upvalues;

        while let Some(uv) = cur {
            match *uv.borrow() {
                Upvalue::Open(s) if s > slot => { prev = Some(uv); cur = uv.next; }
                Upvalue::Open(s) if s == slot => return uv,   // REUSE
                _ => break,
            }
        }

        let created = self.gc.alloc(Upvalue::Open(slot));
        created.next = cur;
        match prev {
            Some(p) => p.next = Some(created),
            None    => self.open_upvalues = Some(created),
        }
        created
    }

    /// Close every upvalue at or above `from`, moving values from stack to heap.
    ///
    /// This is the crux of the entire closure implementation: a captured
    /// variable must transparently migrate from stack to heap at exactly the
    /// moment its stack slot dies, and every closure holding it must observe
    /// the same cell before and after.
    ///
    /// Watching this happen in the playground's debugger is the moment
    /// closures stop being magic.
    fn close_upvalues(&mut self, from: usize) {
        while let Some(uv) = self.open_upvalues {
            let slot = match *uv.borrow() {
                Upvalue::Open(s) if s >= from => s,
                _ => break,
            };
            let value = self.stack[slot].clone();
            *uv.borrow_mut() = Upvalue::Closed(value);
            self.open_upvalues = uv.next;
        }
    }
}
```

### `OP_RETURN` — order matters

```rust
Op::Return => {
    let result = self.pop();
    let frame = self.frames.pop().unwrap();

    // CLOSE UPVALUES BEFORE TRUNCATING THE STACK.
    //
    // Any upvalue still pointing into this frame must be promoted to the heap
    // now. Truncate first and the closure holds an index into a region that
    // is about to be overwritten by the next call — the captured variable
    // silently changes value, at some unrelated point later in the program.
    // This is one of the nastiest bugs in the whole project.
    self.close_upvalues(frame.slot_base);

    if self.frames.is_empty() { return Ok(result); }
    self.stack.truncate(frame.slot_base);
    self.push(result);
}
```

---

## Phase 10 — Garbage Collector

```rust
// crates/ember-gc/src/collect.rs

impl GcHeap {
    fn mark_roots(&mut self, vm: &Vm, compiler: Option<&Compiler>) {
        // 1. Value stack
        for v in &vm.stack { self.mark_value(v); }
        // 2. Closures in call frames
        for f in &vm.frames { self.mark_object(f.closure.as_obj()); }
        // 3. Open upvalues
        let mut uv = vm.open_upvalues;
        while let Some(u) = uv { self.mark_object(u.as_obj()); uv = u.next; }
        // 4. Globals
        for v in vm.globals.values() { self.mark_value(v); }

        // 5. ── COMPILER ROOTS ────────────────────────────────────────────
        // THE classic GC bug. During compilation, function objects exist that
        // the VM cannot reach — they are held only by the compiler's own
        // locals. If a collection triggers mid-compilation and we skip these,
        // they are swept while the compiler still holds pointers.
        //
        // The symptom is memory corruption at a point unrelated to the cause,
        // typically hours of debugging later.
        if let Some(c) = compiler {
            let mut f = Some(c.current_function());
            while let Some(func) = f {
                self.mark_object(func.as_obj());
                f = c.enclosing_of(func);
            }
        }
    }

    /// Tri-color marking. Gray = discovered but children not yet traced.
    fn trace_references(&mut self) {
        while let Some(obj) = self.gray_stack.pop() {
            self.blacken_object(obj);
        }
    }
}

/// GC bugs are nondeterministic by nature: whether a collection happens at the
/// exact wrong moment depends on allocation timing. They can hide for months.
///
/// Stress mode collects on EVERY allocation, which makes the bug fire
/// immediately and reproducibly. The entire conformance suite runs under it.
#[cfg(feature = "gc-stress")]
#[inline]
fn should_collect(&self) -> bool { true }

#[cfg(not(feature = "gc-stress"))]
#[inline]
fn should_collect(&self) -> bool { self.bytes_allocated > self.next_gc }
```

---

## Phase 12 — The Conformance Suite

**This is the single most important piece of test infrastructure in the project.**

```rust
// tests/conformance.rs

/// THE CENTRAL CLAIM OF THIS PROJECT:
/// the tree-walking interpreter and the bytecode VM are two implementations
/// of ONE language, and are observationally indistinguishable.
///
/// Every program runs three times:
///   1. tree-walker
///   2. VM
///   3. VM under gc-stress
/// All three must produce byte-identical output. Any divergence is a bug in
/// one of them, and finding out WHICH is most of the value of this suite.
#[test]
fn backends_agree() {
    let mut failures = vec![];

    for entry in glob("tests/conformance/**/*.em").unwrap() {
        let path = entry.unwrap();
        let src = fs::read_to_string(&path).unwrap();
        let expected = fs::read_to_string(path.with_extension("expected")).unwrap();

        let tree = run_capture(&src, Backend::TreeWalk);
        let vm   = run_capture(&src, Backend::Vm);

        if tree != vm {
            failures.push(format!(
                "\n{}\n  BACKENDS DIVERGED\n  tree-walk: {:?}\n  vm:        {:?}",
                path.display(), tree, vm));
            continue;
        }
        if tree != expected {
            failures.push(format!(
                "\n{}\n  output mismatch\n  expected: {:?}\n  actual:   {:?}",
                path.display(), expected, tree));
        }
    }

    assert!(failures.is_empty(), "{} conformance failures:{}",
            failures.len(), failures.join(""));
}

#[test]
#[cfg(feature = "gc-stress")]
fn conformance_under_gc_stress() {
    // Same suite, collecting on every allocation. This is the real GC test —
    // far more effective than any hand-written GC unit test, because it
    // exercises the collector against every object graph the language can build.
    for entry in glob("tests/conformance/**/*.em").unwrap() {
        let path = entry.unwrap();
        let src = fs::read_to_string(&path).unwrap();
        let expected = fs::read_to_string(path.with_extension("expected")).unwrap();
        assert_eq!(run_capture(&src, Backend::Vm), expected,
                   "gc-stress divergence in {}", path.display());
    }
}
```

**The conformance program that catches the most bugs:**

```rust
// tests/conformance/closures_shared_capture.em
fn make_pair() {
    let mut n = 0;
    let inc = || { n = n + 1; n };
    let get = || { n };
    [inc, get]
}

let p = make_pair();
let inc = p[0];
let get = p[1];

print(get());   // 0
inc();
inc();
print(get());   // 2   ← requires ONE shared upvalue cell, not two
print(inc());   // 3
```

```
// tests/conformance/closures_shared_capture.expected
0
2
3
```

If `capture_upvalue` fails to deduplicate, this prints `0 / 0 / 1` and the bug is caught immediately rather than in some subtle program six weeks later.

---

## Frontend — the two panels that justify a browser

### CodeMirror language mode driven by the real lexer

```typescript
// playground/src/editor/emberLanguage.ts
import { StreamLanguage, LanguageSupport } from '@codemirror/language';
import { tokenize } from '../wasm';

/// The editor's tokenizer IS the compiler's lexer, via WASM.
/// This means the syntax highlighting can never disagree with the parser —
/// a class of bug that plagues every editor using a hand-written TextMate
/// grammar alongside a separate real lexer.
export const emberLanguage = StreamLanguage.define<{ pos: number }>({
  name: 'ember',
  startState: () => ({ pos: 0 }),
  token(stream, state) {
    const line = stream.string;
    const toks: TokenView[] = tokenize(line);
    const tok = toks.find(t => t.start === stream.pos);
    if (!tok) { stream.next(); return null; }
    stream.pos = tok.end;
    return TAG_FOR_KIND[tok.kind] ?? null;
  },
});
```

### The debugger's upvalue view

```tsx
// playground/src/components/runtime/UpvaluePanel.tsx

/// The single clearest explanation of closures anyone will encounter.
///
/// An OPEN upvalue is drawn as an arrow pointing at a live stack slot.
/// A CLOSED upvalue is drawn holding a heap value.
///
/// Step past the end of the enclosing function's scope and watch the arrow
/// detach and the value migrate onto the heap. That single animation replaces
/// several pages of prose.
export function UpvaluePanel({ state }: { state: VmState }) {
  return (
    <div className="space-y-2">
      {state.upvalues.map((uv, i) => (
        <div key={i} className="flex items-center gap-2 font-mono text-sm">
          <Badge variant={uv.kind === 'open' ? 'default' : 'secondary'}>
            {uv.kind}
          </Badge>
          <span className="text-muted-foreground">upvalue[{i}]</span>
          {uv.kind === 'open' ? (
            <>
              <ArrowRight className="h-4 w-4 text-blue-500" />
              <span className="text-blue-500">stack[{uv.slot}]</span>
              <span className="text-muted-foreground">= {fmtValue(state.stack[uv.slot])}</span>
            </>
          ) : (
            <>
              <span className="text-green-500">heap</span>
              <span>= {fmtValue(uv.value)}</span>
            </>
          )}
        </div>
      ))}
    </div>
  );
}
```

### Backend comparison with the equality assertion

```tsx
// playground/src/components/compare/ComparisonPanel.tsx

export function ComparisonPanel({ source }: { source: string }) {
  const tree = useRun(source, 'tree');
  const vm   = useRun(source, 'vm');
  const agree = tree.output === vm.output;

  return (
    <div className="space-y-4">
      {/* This is the conformance suite, running live in the browser.
          Green means the two backends are observationally identical —
          which is the entire thesis of the project. */}
      <Alert variant={agree ? 'default' : 'destructive'}>
        {agree ? <Check className="h-4 w-4" /> : <X className="h-4 w-4" />}
        <AlertTitle>
          {agree ? 'Backends agree' : 'BACKENDS DIVERGED — this is a bug'}
        </AlertTitle>
      </Alert>

      <table className="w-full font-mono text-sm tabular-nums">
        <thead><tr><th/><th>Tree-walk</th><th>Bytecode VM</th><th>Speedup</th></tr></thead>
        <tbody>
          <Row label="Time"        a={`${tree.ms} ms`} b={`${vm.ms} ms`}
               ratio={`${(tree.ms / vm.ms).toFixed(1)}×`} />
          <Row label="Allocations" a={tree.allocs}     b={vm.allocs}
               ratio={`${(tree.allocs / Math.max(vm.allocs,1)).toFixed(0)}×`} />
          <Row label="Peak heap"   a={fmtBytes(tree.peakHeap)} b={fmtBytes(vm.peakHeap)} />
          <Row label="Instructions" a="—" b={vm.instructions.toLocaleString()} />
        </tbody>
      </table>
    </div>
  );
}
```

---

## Correctness Invariants

1. **Backend equivalence** — `backends_agree` over the whole conformance suite
2. **GC soundness** — same suite under `gc-stress`
3. **Lexer totality** — never panics; spans tile the source exactly
4. **Parser totality** — always returns a tree; one syntax error → one diagnostic
5. **Let-polymorphism** — `identity : ∀a. a → a`, usable at `Int` and `String`
6. **Occurs check** — `let f = |x| f(x)` errors, never hangs
7. **Value restriction** — mutable bindings are not generalized
8. **Exhaustiveness** — non-exhaustive matches rejected with the missing patterns named
9. **Shared capture** — two closures over one variable share one cell
10. **Upvalue closing** — closing happens before stack truncation on return
11. **Span accuracy** — every diagnostic points at the exact responsible range
12. **Formatter** — idempotent and semantics-preserving

---

## Code Standards

**Rust**
- **No parser generator.** The Pratt parser is hand-written.
- Lexer and parser return `(result, Vec<Diagnostic>)`, never `Result<_, Error>`. Totality is a load-bearing property.
- AST nodes live in arenas referenced by `Idx<T>`, never `Box<Expr>`. No recursive `Drop`; `Idx` is `Copy` so tree transformations don't fight the borrow checker.
- Every constraint carries an `Origin`. A `unify` call without provenance produces an unreadable error and should not compile — make `origin` a required parameter.
- Never use `panic!` + `catch_unwind` for `return`/`break`/`continue`. Thread `Flow` through return types: panics break WASM and make single-stepping impossible.
- Every new language feature gets a conformance test in the same commit.
- `gc-stress` is a default feature in debug builds.
- `#[deny(clippy::all)]`; `unsafe` only in NaN boxing, with `// SAFETY:` comments.

**Frontend**
- The editor's tokenizer is the compiler's lexer via WASM — never a second grammar that can drift.
- Debounce recompilation at 200 ms; run the pipeline once and fan the artifacts out to all panels.
- Every panel must handle the "source has errors" state gracefully — that is the *common* case while typing.
- Execution has a step budget so an infinite loop can't hang the tab.

---

## Startup

```bash
cargo test                                   # unit tests
cargo test --test conformance                # THE test
cargo test --features gc-stress              # the real GC test
cargo bench                                  # tree-walk vs VM

cargo run -- run examples/fib.em --backend tree --time
cargo run -- run examples/fib.em --backend vm   --time
cargo run -- trace examples/generics.em      # the inference derivation
cargo run -- disasm examples/closures.em

wasm-pack build crates/ember-wasm --target web --release
cd playground && bun run dev                 # http://localhost:5173
```

**First thing to run:**

```bash
cargo run -- bench examples/fib.em
```

```
fib(30) = 832040

              tree-walk      bytecode VM     speedup
time            4,118 ms          147 ms       28.0×
allocations   2,891,443               31       ~93,000×
peak heap        184 MB           0.4 MB          460×
instructions          —      24,157,817
output           832040           832040    ✓ identical
```

That table is the project in one screen. Same language, same source, same answer — and a 28× difference that comes entirely from replacing a hash-map-in-a-pointer-chain with an indexed array access.

**Then open the playground and run this:**

```rust
fn make_counter() {
    let mut n = 0;
    || { n = n + 1; n }
}
let c = make_counter();
print(c()); print(c()); print(c());
```

Step through it in Panel 6. Watch the upvalue start as an arrow pointing at `stack[1]`. Step past the end of `make_counter` and watch `OP_CLOSE_UPVALUE` fire: the arrow detaches, the value moves to the heap, and the closure keeps working. That is the entire mechanism of closures, made visible in about eight steps.

**Then open Panel 4 with this:**

```rust
fn identity(x) { x }
let a = identity(1);
let b = identity("hello");
```

Step the unification stepper. Watch `identity` infer as `t1 -> t1`, generalize to `∀a. a → a`, then instantiate *twice* — once to `Int → Int`, once to `String → String`. Let-polymorphism, which is genuinely hard to explain in prose, becomes obvious in about fifteen seconds of stepping.
