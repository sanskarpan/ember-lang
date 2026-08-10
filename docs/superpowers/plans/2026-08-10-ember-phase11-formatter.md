# Phase 11: Formatter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `ember fmt`, a source formatter, in the currently-stub `ember-fmt` crate — including the lexer-level comment retention this phase's own checklist items require and which doesn't exist anywhere in the codebase yet.

**Architecture:** A Wadler-style pretty-printer (`Doc` IR + a fits-check layout algorithm) with one lowering function per AST node kind, matching the style already consistently used throughout this project's own `tests/conformance/*.em` fixtures (4-space indent, K&R braces, trailing commas on multi-line constructs). Comments are attached by walking the AST in source order alongside a separately-obtained, sorted trivia list (re-lexing the source once more, isolated to `ember-fmt` itself — see the design doc for why this doesn't touch `ember_parser::parse()`'s widely-used public signature).

**Tech Stack:** Rust 2021, new dependencies for `ember-fmt`: `ember-ast`, `ember-lexer`, `ember-parser`, `ember-span`.

See `docs/superpowers/specs/2026-08-10-ember-phase11-formatter-design.md` for the full design rationale.

---

## Before you start (context every task needs)

- **The complete AST inventory this plan is built against** (verified directly against `crates/ember-ast/src/*.rs` and the relevant parts of `crates/ember-parser/src/parser.rs` — do not rediscover this, it's already precise):
  - `Expr` (`crates/ember-ast/src/expr.rs`): `Int(i64)`, `Float(f64)`, `Str(Symbol)`, `Bool(bool)`, `Nil`, `Var(Symbol)`, `Unary { op: TokenKind, operand: Idx<Expr> }`, `Binary { op: TokenKind, lhs: Idx<Expr>, rhs: Idx<Expr> }`, `Assign { target: Idx<Expr>, value: Idx<Expr> }`, `Call { callee: Idx<Expr>, args: Vec<Idx<Expr>> }`, `Index { base: Idx<Expr>, index: Idx<Expr> }`, `Field { base: Idx<Expr>, name: Symbol }`, `Lambda { params: Vec<Param>, body: Idx<Expr> }`, `If { cond: Idx<Expr>, then_: Idx<Expr>, else_: Option<Idx<Expr>> }`, `Match { scrutinee: Idx<Expr>, arms: Vec<MatchArm> }`, `Block { stmts: Vec<Idx<Stmt>>, tail: Option<Idx<Expr>> }`, `List { items: Vec<Idx<Expr>> }`, `Struct { name: Symbol, fields: Vec<(Symbol, Idx<Expr>)> }`, `Error`.
  - `Stmt` (`crates/ember-ast/src/stmt.rs`): `Let { name: Symbol, mutable: bool, ty: Option<Idx<TypeExpr>>, init: Idx<Expr> }`, `ExprStmt(Idx<Expr>)`, `Fn { name: Symbol, params: Vec<Param>, ret_ty: Option<Idx<TypeExpr>>, body: Idx<Expr> }`, `TypeDecl { name: Symbol, variants: Vec<AdtVariant> }`, `StructDecl { name: Symbol, fields: Vec<FieldDecl> }`, `While { cond: Idx<Expr>, body: Idx<Expr> }`, `For { binding: Symbol, iter: Idx<Expr>, body: Idx<Expr> }`, `Loop { body: Idx<Expr> }`, `Return(Option<Idx<Expr>>)`, `Break`, `Continue`, `Error`. `Param { name: Symbol, ty: Option<Idx<TypeExpr>>, span: Span }`, `AdtVariant { name: Symbol, payload: Vec<Idx<TypeExpr>> }`, `FieldDecl { name: Symbol, ty: Idx<TypeExpr> }`.
  - `Pattern` (`crates/ember-ast/src/pattern.rs`): `Wild`, `Bind(Symbol)`, `Int(i64)`, `Float(f64)`, `Str(Symbol)`, `Bool(bool)`, `Ctor { name: Symbol, args: Vec<Idx<Pattern>> }`, `Tuple(Vec<Idx<Pattern>>)`, `List { items: Vec<Idx<Pattern>>, rest: Option<Idx<Pattern>> }`, `Record { name: Symbol, fields: Vec<(Symbol, Idx<Pattern>)> }`, `Or(Vec<Idx<Pattern>>)`, `Error`. `MatchArm { pat: Idx<Pattern>, guard: Option<Idx<Expr>>, body: Idx<Expr>, span: Span }`.
  - `TypeExpr` (`crates/ember-ast/src/ty.rs`): `Name(Symbol)`, `Generic { name: Symbol, args: Vec<Idx<TypeExpr>> }`, `List(Idx<TypeExpr>)`, `Fun { params: Vec<Idx<TypeExpr>>, ret: Idx<TypeExpr> }`, `Error`. Surface syntax (confirmed in `crates/ember-parser/src/parser.rs:466-529`): bare name `Int`, generic `Option<Int>`, list `[Int]`, function `(Int, Int) -> Int`.
  - `Ast` accessors: `ast.expr(idx) -> &Expr`, `ast.span_of_expr(idx) -> Span`, `ast.stmt(idx) -> &Stmt`, `ast.span_of_stmt(idx) -> Span`, `ast.pat(idx) -> &Pattern`, `ast.span_of_pat(idx) -> Span`, `ast.type_expr(idx) -> &TypeExpr`, `ast.span_of_type_expr(idx) -> Span`.
  - **Only `Expr::Block` carries a `Vec<Idx<Stmt>>`.** `if`/`while`/`for`/`loop` bodies are *grammar-guaranteed* to be `Expr::Block` (parsed via `block_or_error`, which requires `{`, confirmed in `parser.rs:392-446,686-773`) — the formatter can always print these with braces. **Lambda bodies are not brace-guaranteed** (parsed via a general `self.expr(Prec::Assign.lower())`, confirmed at `parser.rs:163-168`) — a lambda body might be a bare expression (`|| 42`) or an `Expr::Block` (`|| { ... }`); the formatter's lambda-printing must handle both by just recursing into the general expression printer, which naturally does the right thing for either.
  - `Symbol` (`crates/ember-ast/src/interner.rs`) is `Copy`; text comes from `interner.resolve(sym) -> &str` (panics on an unknown symbol — always use the *same* `Interner` the `Ast` was built with).
  - There is no dedicated `BinOp`/`UnOp` enum — `Expr::Unary.op`/`Expr::Binary.op` are `ember_lexer::TokenKind` directly. Operator text mapping (mirror, don't duplicate divergently, `crates/ember-ast/src/print.rs`'s existing `op_text`): `Plus"+"`, `Minus"-"`, `Star"*"`, `Slash"/"`, `Percent"%"`, `EqEq"=="`, `BangEq"!="`, `Lt"<"`, `LtEq"<="`, `Gt">"`, `GtEq">="`, `AndAnd"&&"`, `OrOr"||"`, `Bang"!"`, `DotDot".."`.
  - Precedence table lives in `ember_parser::prec` (already `pub`, re-exported as `ember_parser::Prec`): `None < Assign < Or < And < Equality < Comparison < Term < Factor < Unary < Call < Primary`, and `ember_parser::prec::InfixPrec::infix_prec(TokenKind) -> Prec` gives each binary operator's precedence. **This project's existing `ember-ast/src/print.rs` debug-printer fully parenthesizes everything and is explicitly documented as unfit to model a real formatter's paren-elision — do not copy its approach.** This plan's own precedence-aware paren rule (Task 3) is the one to implement.
  - `crates/ember-ast/src/print.rs`'s `Match` printing ignores `MatchArm.guard` entirely (a known, pre-existing gap in that debug-only printer) — the real formatter must not repeat this; every arm with `Some(guard)` needs `if <guard>` emitted before `=>`.
- **`ember-fmt`'s `Cargo.toml` currently has zero dependencies** — Task 2 adds `ember-ast`, `ember-lexer`, `ember-parser`, `ember-span`.
- This project enforces `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` cleanly at every commit — verify both after each task, not just at the end.

---

### Task 1: Trivia retention in `ember-lexer`

**Files:**
- Modify: `crates/ember-lexer/src/lib.rs`
- Modify: `crates/ember-lexer/src/lex.rs`
- Modify: `crates/ember-cli/src/main.rs`
- Modify: `crates/ember-parser/src/parser.rs`
- Modify: `crates/ember-lexer/tests/proptest_lexer.rs`

- [ ] **Step 1: Write the failing test first**

Add to the `mod tests` block in `crates/ember-lexer/src/lex.rs`:

```rust
#[test]
fn line_and_block_comments_are_recorded_as_trivia() {
    let (tokens, trivia, diags) = lex("// leading\nlet x = 1; /* trailing */");
    assert!(diags.is_empty());
    assert_eq!(trivia.len(), 2, "{trivia:?}");
    assert_eq!(trivia[0].kind, TriviaKind::Line);
    assert_eq!(trivia[0].span, Span::new(0, 10)); // "// leading" (no trailing \n in the span)
    assert_eq!(trivia[1].kind, TriviaKind::Block);
    assert_eq!(trivia[1].span, Span::new(23, 39)); // "/* trailing */"
    // The token stream itself must be completely unaffected by this
    // change — same tokens, same spans, as before.
    assert_eq!(tokens[0].kind, TokenKind::Let);
}

#[test]
fn no_comments_yields_empty_trivia() {
    let (_tokens, trivia, _diags) = lex("let x = 1;");
    assert!(trivia.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-lexer line_and_block_comments_are_recorded_as_trivia`
Expected: FAILS to compile — `lex` currently returns a 2-tuple, `TriviaKind`/`Trivia` don't exist.

- [ ] **Step 3: Add `Trivia`/`TriviaKind` and thread them through the lexer**

In `crates/ember-lexer/src/lex.rs`, add near the top (after the existing `use` block):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaKind {
    Line,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub span: Span,
}
```

Change `pub fn lex`'s signature and body:

```rust
pub fn lex(src: &str) -> (Vec<Token>, Vec<Trivia>, Vec<Diagnostic>) {
    let mut lx = Lexer::new(src);
    let mut tokens = Vec::with_capacity(src.len() / 4 + 1);
    loop {
        let tok = lx.next_token();
        let done = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if done {
            break;
        }
    }
    (tokens, lx.trivia, lx.diagnostics)
}
```

Add a `trivia: Vec<Trivia>` field to the `Lexer` struct and its `new` constructor:

```rust
struct Lexer<'src> {
    src: &'src str,
    pos: u32,
    trivia: Vec<Trivia>,
    diagnostics: Vec<Diagnostic>,
}

impl<'src> Lexer<'src> {
    fn new(src: &'src str) -> Self {
        Lexer {
            src,
            pos: 0,
            trivia: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
```

Rewrite `skip_trivia` to record each comment before/while skipping it:

```rust
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                ' ' | '\t' | '\r' | '\n' => {
                    self.advance();
                }
                '/' if self.peek_at(1) == '/' => {
                    let start = self.pos;
                    while !self.at_end() && self.peek() != '\n' {
                        self.advance();
                    }
                    self.trivia.push(Trivia {
                        kind: TriviaKind::Line,
                        span: Span::new(start, self.pos),
                    });
                }
                '/' if self.peek_at(1) == '*' => {
                    let start = self.pos;
                    self.advance();
                    self.advance();
                    self.block_comment();
                    self.trivia.push(Trivia {
                        kind: TriviaKind::Block,
                        span: Span::new(start, self.pos),
                    });
                }
                _ => break,
            }
        }
    }
```

Note: `block_comment()`'s own error path (unterminated block comment) already returns early without consuming a matching `*/` — in that case the trivia span still gets pushed (covering from `/*` to wherever the lexer gave up), which is harmless (the accompanying diagnostic is the actually-important signal for that case, and no formatter will ever run successfully on a program with lex errors).

- [ ] **Step 4: Update `crates/ember-lexer/src/lib.rs`'s re-exports**

Find the existing `pub use` line(s) re-exporting from `lex`/`token` and add `Trivia`/`TriviaKind`:

```rust
pub use lex::{lex, Trivia, TriviaKind};
```

(Match whatever the existing re-export line's exact style is — if `lex` isn't currently re-exported by name this way, adjust to fit; the goal is `ember_lexer::Trivia`/`ember_lexer::TriviaKind`/`ember_lexer::lex` all resolve from the crate root, same convention as the existing `Token`/`TokenKind`.)

- [ ] **Step 5: Update the 5 remaining call sites**

`crates/ember-cli/src/main.rs` (1 site) — find:
```rust
    let (tokens, diags) = ember_lexer::lex(&src);
```
Replace with:
```rust
    let (tokens, _trivia, diags) = ember_lexer::lex(&src);
```

`crates/ember-parser/src/parser.rs` (3 sites — `parse`, `parse_expr_from_str`, `parse_stmt_from_str`) — each currently:
```rust
    let (tokens, lex_diags) = ember_lexer::lex(src);
```
Replace each with:
```rust
    let (tokens, _trivia, lex_diags) = ember_lexer::lex(src);
```

`crates/ember-lexer/tests/proptest_lexer.rs` (2 sites) — find:
```rust
        let (_tokens, _diags) = lex(&s);
```
and
```rust
        let (tokens, _diags) = lex(&s);
```
Replace with:
```rust
        let (_tokens, _trivia, _diags) = lex(&s);
```
and
```rust
        let (tokens, _trivia, _diags) = lex(&s);
```

- [ ] **Step 6: Run tests to verify everything passes**

Run: `cargo test -p ember-lexer`
Expected: PASS, including both new tests.

Run: `cargo build --workspace`
Expected: succeeds — confirms all 6 call sites were found and fixed (a missed site would be a compile error here).

Run: `cargo test --workspace`
Expected: PASS, no regressions anywhere (this change is purely additive to the lexer's return shape).

- [ ] **Step 7: Clippy and fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` — both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-lexer/src/lex.rs crates/ember-lexer/src/lib.rs crates/ember-cli/src/main.rs crates/ember-parser/src/parser.rs crates/ember-lexer/tests/proptest_lexer.rs
git commit -m "Retain comments as trivia during lexing"
```

## Important process notes for this task

- No AI/Claude attribution anywhere.
- Do NOT change `Token`/`TokenKind` — trivia is a side channel, the token stream itself is byte-for-byte unaffected.
- Do NOT touch `ember_parser::parse()`'s public *return signature* — only its internal call to `ember_lexer::lex` changes (discarding the new trivia element), its own 4-tuple return stays exactly as-is.

---

### Task 2: `ember-fmt` scaffold + Doc IR + layout algorithm

**Files:**
- Modify: `crates/ember-fmt/Cargo.toml`
- Modify: `crates/ember-fmt/src/lib.rs`
- Create: `crates/ember-fmt/src/doc.rs`

- [ ] **Step 1: Add dependencies**

```toml
[package]
name = "ember-fmt"
version.workspace = true
edition.workspace = true

[dependencies]
ember-ast = { path = "../ember-ast" }
ember-lexer = { path = "../ember-lexer" }
ember-parser = { path = "../ember-parser" }
ember-span = { path = "../ember-span" }
```

- [ ] **Step 2: Write the failing tests first**

Create `crates/ember-fmt/src/doc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_verbatim() {
        assert_eq!(render(&Doc::Text("hi".to_string()), 100), "hi");
    }

    #[test]
    fn a_group_that_fits_renders_flat_with_lines_as_spaces() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("a".to_string()),
            Doc::Line,
            Doc::Text("b".to_string()),
        ])));
        assert_eq!(render(&doc, 100), "a b");
    }

    #[test]
    fn a_group_that_does_not_fit_breaks_every_line_and_respects_nesting() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("aaaaaaaaaa".to_string()),
            Doc::Nest(
                4,
                Box::new(Doc::Concat(vec![
                    Doc::Line,
                    Doc::Text("bbbbbbbbbb".to_string()),
                    Doc::Line,
                    Doc::Text("cccccccccc".to_string()),
                ])),
            ),
        ])));
        // width 5 forces a break; each Line becomes a newline + 4-space indent.
        assert_eq!(render(&doc, 5), "aaaaaaaaaa\n    bbbbbbbbbb\n    cccccccccc");
    }

    #[test]
    fn hard_line_always_breaks_even_inside_a_fitting_group() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("a".to_string()),
            Doc::HardLine,
            Doc::Text("b".to_string()),
        ])));
        assert_eq!(render(&doc, 100), "a\nb");
    }

    #[test]
    fn nested_groups_fit_independently() {
        // The outer group doesn't fit at width 8, so it breaks — but the
        // inner group ("x y", 3 chars) still fits on its own line and stays
        // flat.
        let inner = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("x".to_string()),
            Doc::Line,
            Doc::Text("y".to_string()),
        ])));
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("aaaaaaaa".to_string()),
            Doc::Line,
            inner,
        ])));
        assert_eq!(render(&doc, 8), "aaaaaaaa\nx y");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ember-fmt`
Expected: FAILS to compile — `Doc`/`render` don't exist yet.

- [ ] **Step 4: Implement the `Doc` IR and layout algorithm**

Add above the test module in `crates/ember-fmt/src/doc.rs`:

```rust
use ember_span::Span;

/// A Wadler-style pretty-printing IR. `Group` is the unit the layout
/// algorithm's fits-check operates on: everything inside one `Group` is
/// rendered either fully flat (every `Line` becomes a space) or fully
/// broken (every `Line` becomes a newline at the current indent),
/// decided once per `Group` by whether the flat rendering fits in the
/// remaining width on the current line.
#[derive(Debug, Clone)]
pub enum Doc {
    Text(String),
    /// A space if the enclosing `Group` fits; a newline + current indent
    /// otherwise.
    Line,
    /// Always a newline + current indent, regardless of any enclosing
    /// `Group`'s fit decision — used after `;`, between top-level items,
    /// and anywhere a real line break is mandatory rather than a style
    /// choice.
    HardLine,
    Nest(usize, Box<Doc>),
    Concat(Vec<Doc>),
    Group(Box<Doc>),
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Doc {
        Doc::Text(s.into())
    }

    pub fn concat(docs: Vec<Doc>) -> Doc {
        Doc::Concat(docs)
    }

    pub fn nest(indent: usize, doc: Doc) -> Doc {
        Doc::Nest(indent, Box::new(doc))
    }

    pub fn group(doc: Doc) -> Doc {
        Doc::Group(Box::new(doc))
    }
}

/// True if `doc`, rendered with every `Line` as a space (never breaking),
/// fits within `remaining` columns. `HardLine` always fails a flat fit —
/// anything containing a mandatory break can never be flattened.
fn fits(doc: &Doc, remaining: i64) -> bool {
    if remaining < 0 {
        return false;
    }
    match doc {
        Doc::Text(s) => remaining - (s.chars().count() as i64) >= 0,
        Doc::Line => remaining - 1 >= 0,
        Doc::HardLine => false,
        Doc::Nest(_, d) => fits(d, remaining),
        Doc::Group(d) => fits(d, remaining),
        Doc::Concat(docs) => {
            let mut r = remaining;
            for d in docs {
                if !fits(d, r) {
                    return false;
                }
                r -= flat_width(d);
            }
            true
        }
    }
}

/// Width of `doc` if rendered fully flat (every `Line` as one space).
/// Only ever called on subtrees already confirmed to contain no
/// `HardLine` (via `fits`'s own early return), so it never needs to
/// handle "infinite width" — every `Doc` here has a well-defined flat
/// width.
fn flat_width(doc: &Doc) -> i64 {
    match doc {
        Doc::Text(s) => s.chars().count() as i64,
        Doc::Line => 1,
        Doc::HardLine => 0, // unreachable in practice, see doc comment above
        Doc::Nest(_, d) => flat_width(d),
        Doc::Group(d) => flat_width(d),
        Doc::Concat(docs) => docs.iter().map(flat_width).sum(),
    }
}

/// Renders `doc` at `width` columns.
pub fn render(doc: Doc, width: usize) -> String {
    let mut out = String::new();
    let mut col: i64 = 0;
    render_doc(&doc, 0, false, width as i64, &mut col, &mut out);
    out
}

/// `flat`: true if an enclosing `Group` already decided to render flat —
/// propagates down so nested non-`Group` structure (`Concat`/`Nest`)
/// inherits the decision, and a NESTED `Group` gets its OWN independent
/// fits-check only when not already forced flat by an ancestor.
fn render_doc(doc: &Doc, indent: usize, flat: bool, width: i64, col: &mut i64, out: &mut String) {
    match doc {
        Doc::Text(s) => {
            out.push_str(s);
            *col += s.chars().count() as i64;
        }
        Doc::Line => {
            if flat {
                out.push(' ');
                *col += 1;
            } else {
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                *col = indent as i64;
            }
        }
        Doc::HardLine => {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            *col = indent as i64;
        }
        Doc::Nest(n, d) => render_doc(d, indent + n, flat, width, col, out),
        Doc::Concat(docs) => {
            for d in docs {
                render_doc(d, indent, flat, width, col, out);
            }
        }
        Doc::Group(d) => {
            let should_flatten = !flat && fits(d, width - *col);
            render_doc(d, indent, flat || should_flatten, width, col, out);
        }
    }
}
```

Note: `render`'s public signature above takes `doc` by value (not `&Doc`) for ergonomics at call sites, but the tests in Step 2 call `render(&Doc::..., width)`. Reconcile by making the public `render` accept `impl Into<Doc>`-style ownership OR (simpler, do this) just change the tests to pass owned `Doc` values instead of references — update Step 2's tests to call `render(doc, N)` (no `&`) once you've written `render`'s real signature as `pub fn render(doc: Doc, width: usize) -> String`. Keep whichever direction you pick internally consistent; the exact ownership shape doesn't matter, only that it compiles and the behavior described in each test holds.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ember-fmt`
Expected: PASS, all 5.

- [ ] **Step 6: Wire up `lib.rs`**

Replace `crates/ember-fmt/src/lib.rs`'s content:

```rust
pub mod doc;

pub use doc::{render, Doc};
```

- [ ] **Step 7: Clippy and fmt**

Run: `cargo clippy -p ember-fmt --all-targets -- -D warnings` and `cargo fmt -p ember-fmt -- --check` — both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-fmt/Cargo.toml crates/ember-fmt/src/lib.rs crates/ember-fmt/src/doc.rs
git commit -m "Add the Doc IR and Wadler-style layout algorithm to ember-fmt"
```

---

### Task 3: AST → `Doc` — expressions (literals, operators with precedence-aware parens, calls)

**Files:**
- Create: `crates/ember-fmt/src/print_expr.rs`
- Modify: `crates/ember-fmt/src/lib.rs`

This task covers `Expr::{Int, Float, Str, Bool, Nil, Var, Unary, Binary, Assign, Call, Index, Field}`. `Block`/`If`/`Match`/`Lambda`/`List`/`Struct`/`Error` are Task 4.

- [ ] **Step 1: Write the failing tests first**

Create `crates/ember-fmt/src/print_expr.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Interner;

    fn fmt_expr(src: &str) -> String {
        // Parses `src` as a standalone expression statement, formats just
        // that expression, ignoring the trailing `;` this wraps it in.
        let wrapped = format!("{src};");
        let (ast, interner, stmts, diags) = ember_parser::parse(&wrapped);
        assert!(diags.is_empty(), "parse diags for {src:?}: {diags:?}");
        let e = match ast.stmt(stmts[0]) {
            ember_ast::Stmt::ExprStmt(e) => *e,
            other => panic!("expected ExprStmt, got {other:?}"),
        };
        let doc = print_expr(&ast, &interner, e, ember_parser::Prec::None);
        ember_fmt_doc_render(doc)
    }

    fn ember_fmt_doc_render(doc: crate::Doc) -> String {
        crate::render(doc, 100)
    }

    #[test]
    fn literals() {
        assert_eq!(fmt_expr("42"), "42");
        assert_eq!(fmt_expr("3.14"), "3.14");
        assert_eq!(fmt_expr("true"), "true");
        assert_eq!(fmt_expr("false"), "false");
        assert_eq!(fmt_expr("nil"), "nil");
        assert_eq!(fmt_expr("\"hi\""), "\"hi\"");
        assert_eq!(fmt_expr("x"), "x");
    }

    #[test]
    fn binary_operators_get_spaces_and_no_redundant_parens() {
        assert_eq!(fmt_expr("1 + 2"), "1 + 2");
        assert_eq!(fmt_expr("1 + 2 * 3"), "1 + 2 * 3");
        assert_eq!(fmt_expr("(1 + 2) * 3"), "(1 + 2) * 3");
        assert_eq!(fmt_expr("1 - 2 - 3"), "1 - 2 - 3");
        assert_eq!(fmt_expr("1 - (2 - 3)"), "1 - (2 - 3)");
        assert_eq!(fmt_expr("a && b || c"), "a && b || c");
    }

    #[test]
    fn unary_and_call_precedence() {
        assert_eq!(fmt_expr("-x"), "-x");
        assert_eq!(fmt_expr("-(x + 1)"), "-(x + 1)");
        assert_eq!(fmt_expr("!x"), "!x");
        assert_eq!(fmt_expr("f(1, 2)"), "f(1, 2)");
        assert_eq!(fmt_expr("f()"), "f()");
        assert_eq!(fmt_expr("a.b"), "a.b");
        assert_eq!(fmt_expr("a[0]"), "a[0]");
        assert_eq!(fmt_expr("a.b.c"), "a.b.c");
    }

    #[test]
    fn assignment() {
        assert_eq!(fmt_expr("x = 1"), "x = 1");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-fmt print_expr`
Expected: FAILS to compile — `print_expr` doesn't exist.

- [ ] **Step 3: Implement `print_expr`**

Add above the test module in `crates/ember-fmt/src/print_expr.rs`:

```rust
use crate::doc::Doc;
use ember_ast::{Ast, Expr, Idx, Interner};
use ember_lexer::TokenKind;
use ember_parser::prec::InfixPrec;
use ember_parser::Prec;

fn op_text(op: TokenKind) -> &'static str {
    match op {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::EqEq => "==",
        TokenKind::BangEq => "!=",
        TokenKind::Lt => "<",
        TokenKind::LtEq => "<=",
        TokenKind::Gt => ">",
        TokenKind::GtEq => ">=",
        TokenKind::AndAnd => "&&",
        TokenKind::OrOr => "||",
        TokenKind::Bang => "!",
        TokenKind::DotDot => "..",
        _ => unreachable!("op_text called on non-operator TokenKind {op:?}"),
    }
}

/// Every `Expr`'s own intrinsic precedence, for paren-elision decisions —
/// NOT the same table as `InfixPrec` (that's only for binary operator
/// tokens); this covers every expression KIND.
fn expr_prec(ast: &Ast, e: Idx<Expr>) -> Prec {
    match ast.expr(e) {
        Expr::Binary { op, .. } => op.infix_prec(),
        Expr::Unary { .. } => Prec::Unary,
        Expr::Assign { .. } => Prec::Assign,
        Expr::Call { .. } | Expr::Index { .. } | Expr::Field { .. } => Prec::Call,
        _ => Prec::Primary,
    }
}

/// Prints `e`, parenthesizing it if its own precedence is lower than
/// `min_prec` requires (i.e. it wouldn't parse back to the same tree
/// without parens in this position).
pub fn print_expr(ast: &Ast, interner: &Interner, e: Idx<Expr>, min_prec: Prec) -> Doc {
    let inner = print_expr_inner(ast, interner, e);
    if expr_prec(ast, e) < min_prec {
        Doc::concat(vec![Doc::text("("), inner, Doc::text(")")])
    } else {
        inner
    }
}

fn print_expr_inner(ast: &Ast, interner: &Interner, e: Idx<Expr>) -> Doc {
    match ast.expr(e).clone() {
        Expr::Int(n) => Doc::text(n.to_string()),
        Expr::Float(f) => Doc::text(f.to_string()),
        Expr::Str(s) => Doc::text(format!("{:?}", interner.resolve(s))),
        Expr::Bool(b) => Doc::text(b.to_string()),
        Expr::Nil => Doc::text("nil"),
        Expr::Var(s) => Doc::text(interner.resolve(s).to_string()),
        Expr::Unary { op, operand } => Doc::concat(vec![
            Doc::text(op_text(op)),
            print_expr(ast, interner, operand, Prec::Unary),
        ]),
        Expr::Binary { op, lhs, rhs } => {
            let p = op.infix_prec();
            // Left-associative: LHS may be equal precedence (no parens),
            // RHS must be STRICTLY tighter (equal precedence on the right
            // needs parens) — see this plan's own header note on why
            // `< p` vs `<= p` is the whole difference.
            let lhs_doc = print_expr(ast, interner, lhs, p);
            let rhs_min = match p {
                Prec::Primary => Prec::Primary,
                _ => next_tighter(p),
            };
            let rhs_doc = print_expr(ast, interner, rhs, rhs_min);
            Doc::group(Doc::concat(vec![
                lhs_doc,
                Doc::text(" "),
                Doc::text(op_text(op)),
                Doc::Line,
                rhs_doc,
            ]))
        }
        Expr::Assign { target, value } => Doc::concat(vec![
            print_expr(ast, interner, target, Prec::None),
            Doc::text(" = "),
            print_expr(ast, interner, value, Prec::Assign),
        ]),
        Expr::Call { callee, args } => Doc::concat(vec![
            print_expr(ast, interner, callee, Prec::Call),
            Doc::text("("),
            print_comma_list(args.iter().map(|&a| print_expr(ast, interner, a, Prec::None))),
            Doc::text(")"),
        ]),
        Expr::Index { base, index } => Doc::concat(vec![
            print_expr(ast, interner, base, Prec::Call),
            Doc::text("["),
            print_expr(ast, interner, index, Prec::None),
            Doc::text("]"),
        ]),
        Expr::Field { base, name } => Doc::concat(vec![
            print_expr(ast, interner, base, Prec::Call),
            Doc::text("."),
            Doc::text(interner.resolve(name).to_string()),
        ]),
        other => unimplemented!("print_expr_inner: {other:?} — a later task"),
    }
}

/// One step tighter than `p` — the opposite direction of `Prec::lower`,
/// needed for right-operand paren rules. `Prec` has no built-in "raise by
/// one" (only `lower`), so this is a small manual inverse table.
fn next_tighter(p: Prec) -> Prec {
    match p {
        Prec::None => Prec::Assign,
        Prec::Assign => Prec::Or,
        Prec::Or => Prec::And,
        Prec::And => Prec::Equality,
        Prec::Equality => Prec::Comparison,
        Prec::Comparison => Prec::Term,
        Prec::Term => Prec::Factor,
        Prec::Factor => Prec::Unary,
        Prec::Unary => Prec::Call,
        Prec::Call => Prec::Primary,
        Prec::Primary => Prec::Primary,
    }
}

/// Joins `docs` with `", "` — used for call args and, in later tasks,
/// every other comma-separated construct (list/struct literals, params).
pub fn print_comma_list(docs: impl Iterator<Item = Doc>) -> Doc {
    let items: Vec<Doc> = docs.collect();
    let mut out = Vec::new();
    for (i, d) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(Doc::text(", "));
        }
        out.push(d);
    }
    Doc::concat(out)
}
```

Note on `Expr::Binary`'s `Doc::group(...)` with a `Doc::Line` before the RHS: this makes a long binary chain able to break (RHS moves to the next line, indented) when it doesn't fit — Task 4/5 may need to revisit this once real multi-line contexts (e.g. inside a `Block`) exist to properly test the *chain-breaks-consistently-at-one-precedence-level* checklist requirement; for this task, the flat-fits-on-one-line behavior (exercised by the tests above, which all fit in 100 columns) is what's being verified. Leave a comment to that effect if you adjust this in a later task rather than silently changing behavior this task's own tests rely on.

- [ ] **Step 4: Wire up `lib.rs`**

Add to `crates/ember-fmt/src/lib.rs`:

```rust
pub mod print_expr;

pub use print_expr::print_expr;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ember-fmt`
Expected: PASS, including all of Task 2's `doc` tests plus this task's new `print_expr` tests.

- [ ] **Step 6: Clippy and fmt**

Run: `cargo clippy -p ember-fmt --all-targets -- -D warnings` and `cargo fmt -p ember-fmt -- --check` — both clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ember-fmt/src/print_expr.rs crates/ember-fmt/src/lib.rs
git commit -m "AST to Doc: literals, operators with precedence-aware parens, calls"
```

---

### Task 4: AST → `Doc` — blocks, control flow, match, lambda, collections

**Files:**
- Modify: `crates/ember-fmt/src/print_expr.rs`

This task fills in `print_expr_inner`'s remaining `Expr` variants: `Block`, `If`, `Match`, `Lambda`, `List`, `Struct`, `Error`. It also needs `print_pattern` (for `Match` arms) and `print_stmt` (for `Block`'s statements) — since those are mutually recursive with expression printing (a `Block`'s statements can contain expressions, which can contain nested blocks via `if`/lambdas/etc.), write minimal versions of `print_pattern`/`print_stmt` here (covering every variant with real, non-placeholder output) rather than deferring them to Task 5, and Task 5 will REUSE (not replace) them, only adding the top-level-declaration-specific pieces (`Fn`, `TypeDecl`, `StructDecl`, `Let`'s type-annotation printing, etc.) it needs beyond what this task already covers for statements-inside-a-block.

**Files (revised):**
- Modify: `crates/ember-fmt/src/print_expr.rs`
- Create: `crates/ember-fmt/src/print_pattern.rs`
- Create: `crates/ember-fmt/src/print_stmt.rs`
- Modify: `crates/ember-fmt/src/lib.rs`

- [ ] **Step 1: Write the failing tests first**

Add to `print_expr.rs`'s test module (extend the existing `mod tests`, reusing its `fmt_expr` helper):

```rust
#[test]
fn block_with_tail() {
    assert_eq!(fmt_expr("{ let x = 1; x }"), "{\n    let x = 1;\n    x\n}");
}

#[test]
fn empty_block() {
    assert_eq!(fmt_expr("{ }"), "{}");
}

#[test]
fn if_else() {
    assert_eq!(
        fmt_expr("if x { 1 } else { 2 }"),
        "if x {\n    1\n} else {\n    2\n}"
    );
}

#[test]
fn if_no_else() {
    assert_eq!(fmt_expr("if x { 1 }"), "if x {\n    1\n}");
}

#[test]
fn else_if_chains_stay_on_one_line_at_each_link() {
    assert_eq!(
        fmt_expr("if a { 1 } else if b { 2 } else { 3 }"),
        "if a {\n    1\n} else if b {\n    2\n} else {\n    3\n}"
    );
}

#[test]
fn lambda_with_bare_expr_body_stays_inline() {
    assert_eq!(fmt_expr("|| 42"), "|| 42");
    assert_eq!(fmt_expr("|x| x + 1"), "|x| x + 1");
}

#[test]
fn lambda_with_block_body() {
    assert_eq!(fmt_expr("|| { 1; 2 }"), "|| {\n    1;\n    2\n}");
}

#[test]
fn list_literal() {
    assert_eq!(fmt_expr("[1, 2, 3]"), "[1, 2, 3]");
    assert_eq!(fmt_expr("[]"), "[]");
}

#[test]
fn struct_literal() {
    assert_eq!(fmt_expr("_P { x: 1, y: 2 }"), "_P { x: 1, y: 2 }");
}

#[test]
fn match_with_guard_is_not_dropped() {
    assert_eq!(
        fmt_expr("match x { n if n > 0 => 1, _ => 0, }"),
        "match x {\n    n if n > 0 => 1,\n    _ => 0,\n}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-fmt`
Expected: FAILS — `unimplemented!` panics for `Block`/`If`/`Lambda`/`List`/`Struct`/`Match`.

- [ ] **Step 3: Write `print_pattern.rs`**

```rust
use crate::doc::Doc;
use crate::print_expr::{print_comma_list, print_expr};
use ember_ast::{Ast, Interner, Pattern, Idx};
use ember_parser::Prec;

pub fn print_pattern(ast: &Ast, interner: &Interner, p: Idx<Pattern>) -> Doc {
    match ast.pat(p).clone() {
        Pattern::Wild => Doc::text("_"),
        Pattern::Bind(s) => Doc::text(interner.resolve(s).to_string()),
        Pattern::Int(n) => Doc::text(n.to_string()),
        Pattern::Float(f) => Doc::text(f.to_string()),
        Pattern::Str(s) => Doc::text(format!("{:?}", interner.resolve(s))),
        Pattern::Bool(b) => Doc::text(b.to_string()),
        Pattern::Ctor { name, args } => {
            if args.is_empty() {
                Doc::text(interner.resolve(name).to_string())
            } else {
                Doc::concat(vec![
                    Doc::text(interner.resolve(name).to_string()),
                    Doc::text("("),
                    print_comma_list(args.iter().map(|&a| print_pattern(ast, interner, a))),
                    Doc::text(")"),
                ])
            }
        }
        Pattern::Tuple(items) => Doc::concat(vec![
            Doc::text("("),
            print_comma_list(items.iter().map(|&i| print_pattern(ast, interner, i))),
            Doc::text(")"),
        ]),
        Pattern::List { items, rest } => {
            let mut parts: Vec<Doc> = items
                .iter()
                .map(|&i| print_pattern(ast, interner, i))
                .collect();
            if let Some(r) = rest {
                parts.push(Doc::concat(vec![
                    Doc::text(".."),
                    print_pattern(ast, interner, r),
                ]));
            }
            Doc::concat(vec![
                Doc::text("["),
                print_comma_list(parts.into_iter()),
                Doc::text("]"),
            ])
        }
        Pattern::Record { name, fields } => {
            let field_docs = fields.iter().map(|(fname, fpat)| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(*fname).to_string()),
                    Doc::text(": "),
                    print_pattern(ast, interner, *fpat),
                ])
            });
            Doc::concat(vec![
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Pattern::Or(alts) => {
            let mut out = Vec::new();
            for (i, &a) in alts.iter().enumerate() {
                if i > 0 {
                    out.push(Doc::text(" | "));
                }
                out.push(print_pattern(ast, interner, a));
            }
            Doc::concat(out)
        }
        Pattern::Error => Doc::text("<error>"),
    }
}

// Referenced here only to keep `Prec`/`print_expr` imports honest if a
// later edit needs them for guard printing done elsewhere — remove this
// line if it ends up unused after Task 4's `print_expr.rs` changes land
// (guards are printed from `print_expr.rs`'s own `Match` arm, not here).
#[allow(unused_imports)]
use print_expr as _;
```

Delete that last trailing `#[allow(unused_imports)] use print_expr as _;` stub line — it was a placeholder reminder, not real code; `print_pattern.rs` as written above doesn't actually need `print_expr`/`Prec` imported (guards live in `print_expr.rs`'s `Match` handling, not here) — remove the unused `use crate::print_expr::{print_comma_list, print_expr};`'s `print_expr` half too, keeping only `print_comma_list`:

```rust
use crate::print_expr::print_comma_list;
```

- [ ] **Step 4: Write `print_stmt.rs`** (statements as they appear inside a `Block` — `Let`, `ExprStmt`, `Break`, `Continue`, `Return`; top-level-only declarations `Fn`/`TypeDecl`/`StructDecl` are stubbed here with `unimplemented!` and filled in for real by Task 5, since a `Block` can't actually contain them per the grammar — `starts_keyword_stmt`/`block`'s own dispatch only routes `let/fn/type/struct/while/for/loop/return/break/continue` keyword-led statements into a block body, so *all* of `Stmt`'s variants CAN appear inside a block; keep `Fn`/`TypeDecl`/`StructDecl`/`While`/`For`/`Loop` real here too, not stubbed, since they're valid block contents)

```rust
use crate::doc::Doc;
use crate::print_expr::print_expr;
use ember_ast::{Ast, Interner, Stmt, Idx, Expr};
use ember_parser::Prec;

pub fn print_stmt(ast: &Ast, interner: &Interner, s: Idx<Stmt>) -> Doc {
    match ast.stmt(s).clone() {
        Stmt::Let { name, mutable, init, .. } => {
            let kw = if mutable { "let mut " } else { "let " };
            Doc::concat(vec![
                Doc::text(kw),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" = "),
                print_expr(ast, interner, init, Prec::None),
                Doc::text(";"),
            ])
        }
        Stmt::ExprStmt(e) => Doc::concat(vec![
            print_expr(ast, interner, e, Prec::None),
            Doc::text(";"),
        ]),
        Stmt::While { cond, body } => Doc::concat(vec![
            Doc::text("while "),
            print_expr(ast, interner, cond, Prec::None),
            Doc::text(" "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::For { binding, iter, body } => Doc::concat(vec![
            Doc::text("for "),
            Doc::text(interner.resolve(binding).to_string()),
            Doc::text(" in "),
            print_expr(ast, interner, iter, Prec::None),
            Doc::text(" "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::Loop { body } => Doc::concat(vec![
            Doc::text("loop "),
            print_expr(ast, interner, body, Prec::None),
        ]),
        Stmt::Return(value) => match value {
            Some(v) => Doc::concat(vec![
                Doc::text("return "),
                print_expr(ast, interner, v, Prec::None),
                Doc::text(";"),
            ]),
            None => Doc::text("return;"),
        },
        Stmt::Break => Doc::text("break;"),
        Stmt::Continue => Doc::text("continue;"),
        Stmt::Error => Doc::text("<error-stmt>"),
        // Fn/TypeDecl/StructDecl: valid inside a block (grammar allows
        // any keyword-led statement there), but their full printing
        // (params, type annotations, variants, fields) is written once,
        // for real, in Task 5 — this task only needs them to not panic
        // if the checklist's own test corpus happens to nest one inside
        // a block; delegate to Task 5's function by forward-declaring it
        // here and implementing it there.
        Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. } => {
            crate::print_decl::print_decl_stmt(ast, interner, s)
        }
    }
}

// Placeholder for the `Expr` import so this file compiles standalone
// before Task 5 adds `print_decl.rs` — remove if `Expr` ends up unused.
#[allow(unused_imports)]
use Expr as _;
```

Remove that last placeholder `use Expr as _;` line too — `Expr` isn't actually referenced in this file's real code above (only `Idx<Stmt>` and the `Stmt` variants are), so drop the unused import entirely; the real `use` list at the top of the file should just be:

```rust
use crate::doc::Doc;
use crate::print_expr::print_expr;
use ember_ast::{Ast, Interner, Stmt, Idx};
use ember_parser::Prec;
```

Note this task forward-references `crate::print_decl::print_decl_stmt`, which doesn't exist until Task 5 — this is intentional and means Task 4's build will NOT compile clean until Task 5 lands `print_decl.rs`. Do not attempt to work around this by stubbing `print_decl_stmt` here; Task 5 owns that module. (If your TDD workflow requires each task to leave the crate compiling, add a temporary `pub(crate) fn print_decl_stmt(_: &Ast, _: &Interner, _: Idx<Stmt>) -> Doc { unimplemented!("Task 5") }` directly in this file for now, and delete it once Task 5's real `print_decl.rs` module exists and is wired into `lib.rs` — prefer this temporary-stub approach so `cargo build`/`cargo test` keep working after this task, matching how every other phase in this project has kept the crate buildable after each task.)

- [ ] **Step 5: Fill in `print_expr_inner`'s remaining variants**

In `print_expr.rs`, replace the `other => unimplemented!(...)` catch-all with real arms for `Block`, `If`, `Lambda`, `List`, `Struct`, `Error`, and `Match`:

```rust
        Expr::Block { stmts, tail } => {
            if stmts.is_empty() && tail.is_none() {
                return Doc::text("{}");
            }
            let mut body = Vec::new();
            for &s in &stmts {
                body.push(Doc::HardLine);
                body.push(crate::print_stmt::print_stmt(ast, interner, s));
            }
            if let Some(t) = tail {
                body.push(Doc::HardLine);
                body.push(print_expr(ast, interner, t, Prec::None));
            }
            Doc::concat(vec![
                Doc::text("{"),
                Doc::nest(4, Doc::concat(body)),
                Doc::HardLine,
                Doc::text("}"),
            ])
        }
        Expr::If { cond, then_, else_ } => {
            let mut out = vec![
                Doc::text("if "),
                print_expr(ast, interner, cond, Prec::None),
                Doc::text(" "),
                print_expr(ast, interner, then_, Prec::None),
            ];
            if let Some(e) = else_ {
                out.push(Doc::text(" else "));
                // `else if` recurses into another `If` here naturally —
                // `print_expr_inner`'s own `If` arm handles it, keeping
                // the chain on one logical line-group per link, matching
                // this task's `else_if_chains_stay_on_one_line_at_each_link`
                // test.
                out.push(print_expr(ast, interner, e, Prec::None));
            }
            Doc::concat(out)
        }
        Expr::Lambda { params, body } => {
            let param_docs = params.iter().map(|p| Doc::text(interner.resolve(p.name).to_string()));
            Doc::concat(vec![
                Doc::text("|"),
                print_comma_list(param_docs),
                Doc::text("| "),
                print_expr(ast, interner, body, Prec::Assign.lower()),
            ])
        }
        Expr::List { items } => Doc::concat(vec![
            Doc::text("["),
            print_comma_list(items.iter().map(|&i| print_expr(ast, interner, i, Prec::None))),
            Doc::text("]"),
        ]),
        Expr::Struct { name, fields } => {
            let field_docs = fields.iter().map(|(fname, fval)| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(*fname).to_string()),
                    Doc::text(": "),
                    print_expr(ast, interner, *fval, Prec::None),
                ])
            });
            Doc::concat(vec![
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Expr::Match { scrutinee, arms } => {
            let mut body = Vec::new();
            for arm in &arms {
                body.push(Doc::HardLine);
                body.push(crate::print_pattern::print_pattern(ast, interner, arm.pat));
                if let Some(guard) = arm.guard {
                    body.push(Doc::text(" if "));
                    body.push(print_expr(ast, interner, guard, Prec::None));
                }
                body.push(Doc::text(" => "));
                body.push(print_expr(ast, interner, arm.body, Prec::None));
                body.push(Doc::text(","));
            }
            Doc::concat(vec![
                Doc::text("match "),
                print_expr(ast, interner, scrutinee, Prec::None),
                Doc::text(" {"),
                Doc::nest(4, Doc::concat(body)),
                Doc::HardLine,
                Doc::text("}"),
            ])
        }
        Expr::Error => Doc::text("<error>"),
```

`Expr::Lambda`'s use of `Prec::Assign.lower()` mirrors the parser's own `parser.rs:165,172` (`self.expr(Prec::Assign.lower())`) — the tightest precedence a lambda's un-braced bare-expression body can bind at without a caller misreading where the lambda ends.

Add `pub mod print_pattern; pub mod print_stmt;` to `crates/ember-fmt/src/lib.rs`, alongside a temporary `pub mod print_decl;` with a minimal stub file `crates/ember-fmt/src/print_decl.rs` containing just:
```rust
use crate::doc::Doc;
use ember_ast::{Ast, Interner, Idx, Stmt};

pub(crate) fn print_decl_stmt(_ast: &Ast, _interner: &Interner, _s: Idx<Stmt>) -> Doc {
    unimplemented!("filled in by Task 5")
}
```
(Task 5 replaces this stub file's contents for real — don't skip creating it now, or Task 4 won't compile per the note in Step 4.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ember-fmt`
Expected: PASS, all tests from Tasks 2-4.

- [ ] **Step 7: Clippy and fmt**

Run: `cargo clippy -p ember-fmt --all-targets -- -D warnings` and `cargo fmt -p ember-fmt -- --check` — both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-fmt/src/print_expr.rs crates/ember-fmt/src/print_pattern.rs crates/ember-fmt/src/print_stmt.rs crates/ember-fmt/src/print_decl.rs crates/ember-fmt/src/lib.rs
git commit -m "AST to Doc: blocks, if/else, match with guards, lambda, list/struct literals"
```

---

### Task 5: AST → `Doc` — top-level declarations and type expressions

**Files:**
- Modify: `crates/ember-fmt/src/print_decl.rs` (replacing Task 4's stub)
- Create: `crates/ember-fmt/src/print_type.rs`
- Modify: `crates/ember-fmt/src/print_stmt.rs` (add `Let`'s type-annotation printing)
- Modify: `crates/ember-fmt/src/lib.rs`

- [ ] **Step 1: Write the failing tests first**

Add a new test module — create `crates/ember-fmt/src/print_decl.rs`'s own `#[cfg(test)] mod tests` (this REPLACES the Task 4 stub file entirely, so write the whole file fresh, tests included, per Step 3 below) with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_program(src: &str) -> String {
        let (ast, interner, stmts, diags) = ember_parser::parse(src);
        assert!(diags.is_empty(), "parse diags for {src:?}: {diags:?}");
        let docs: Vec<Doc> = stmts
            .iter()
            .map(|&s| crate::print_stmt::print_stmt(&ast, &interner, s))
            .collect();
        let mut out = Vec::new();
        for (i, d) in docs.into_iter().enumerate() {
            if i > 0 {
                out.push(Doc::HardLine);
            }
            out.push(d);
        }
        crate::render(Doc::concat(out), 100)
    }

    #[test]
    fn fn_decl_with_params_and_return_type() {
        assert_eq!(
            fmt_program("fn add(a: Int, b: Int) -> Int { a + b }"),
            "fn add(a: Int, b: Int) -> Int {\n    a + b\n}"
        );
    }

    #[test]
    fn fn_decl_no_types() {
        assert_eq!(fmt_program("fn f(x) { x }"), "fn f(x) {\n    x\n}");
    }

    #[test]
    fn struct_decl() {
        assert_eq!(
            fmt_program("struct Point { x: Int, y: Int }"),
            "struct Point { x: Int, y: Int }"
        );
    }

    #[test]
    fn type_decl_with_payload_and_nullary_variants() {
        assert_eq!(
            fmt_program("type Shape = Circle(Float) | Origin;"),
            "type Shape = Circle(Float) | Origin;"
        );
    }

    #[test]
    fn let_with_type_annotation() {
        assert_eq!(fmt_program("let x: Int = 1;"), "let x: Int = 1;");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-fmt print_decl`
Expected: FAILS — `print_decl_stmt` is still Task 4's `unimplemented!` stub, and `Let`'s type-annotation isn't printed yet (dropped by `..` in Task 4's `Stmt::Let` arm).

- [ ] **Step 3: Write `print_type.rs`**

```rust
use crate::doc::Doc;
use crate::print_expr::print_comma_list;
use ember_ast::{Ast, Interner, TypeExpr, Idx};

pub fn print_type(ast: &Ast, interner: &Interner, t: Idx<TypeExpr>) -> Doc {
    match ast.type_expr(t).clone() {
        TypeExpr::Name(s) => Doc::text(interner.resolve(s).to_string()),
        TypeExpr::Generic { name, args } => Doc::concat(vec![
            Doc::text(interner.resolve(name).to_string()),
            Doc::text("<"),
            print_comma_list(args.iter().map(|&a| print_type(ast, interner, a))),
            Doc::text(">"),
        ]),
        TypeExpr::List(elem) => Doc::concat(vec![
            Doc::text("["),
            print_type(ast, interner, elem),
            Doc::text("]"),
        ]),
        TypeExpr::Fun { params, ret } => Doc::concat(vec![
            Doc::text("("),
            print_comma_list(params.iter().map(|&p| print_type(ast, interner, p))),
            Doc::text(") -> "),
            print_type(ast, interner, ret),
        ]),
        TypeExpr::Error => Doc::text("<error-type>"),
    }
}
```

- [ ] **Step 4: Replace `print_decl.rs`'s stub with the real implementation**

```rust
use crate::doc::Doc;
use crate::print_expr::{print_comma_list, print_expr};
use crate::print_type::print_type;
use ember_ast::{Ast, Interner, Stmt, Idx};
use ember_parser::Prec;

pub(crate) fn print_decl_stmt(ast: &Ast, interner: &Interner, s: Idx<Stmt>) -> Doc {
    match ast.stmt(s).clone() {
        Stmt::Fn { name, params, ret_ty, body } => {
            let param_docs = params.iter().map(|p| match p.ty {
                Some(ty) => Doc::concat(vec![
                    Doc::text(interner.resolve(p.name).to_string()),
                    Doc::text(": "),
                    print_type(ast, interner, ty),
                ]),
                None => Doc::text(interner.resolve(p.name).to_string()),
            });
            let mut out = vec![
                Doc::text("fn "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text("("),
                print_comma_list(param_docs),
                Doc::text(")"),
            ];
            if let Some(rt) = ret_ty {
                out.push(Doc::text(" -> "));
                out.push(print_type(ast, interner, rt));
            }
            out.push(Doc::text(" "));
            out.push(print_expr(ast, interner, body, Prec::None));
            Doc::concat(out)
        }
        Stmt::StructDecl { name, fields } => {
            let field_docs = fields.iter().map(|f| {
                Doc::concat(vec![
                    Doc::text(interner.resolve(f.name).to_string()),
                    Doc::text(": "),
                    print_type(ast, interner, f.ty),
                ])
            });
            Doc::concat(vec![
                Doc::text("struct "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" { "),
                print_comma_list(field_docs),
                Doc::text(" }"),
            ])
        }
        Stmt::TypeDecl { name, variants } => {
            let mut out = vec![
                Doc::text("type "),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" = "),
            ];
            for (i, v) in variants.iter().enumerate() {
                if i > 0 {
                    out.push(Doc::text(" | "));
                }
                out.push(Doc::text(interner.resolve(v.name).to_string()));
                if !v.payload.is_empty() {
                    out.push(Doc::text("("));
                    out.push(print_comma_list(
                        v.payload.iter().map(|&p| print_type(ast, interner, p)),
                    ));
                    out.push(Doc::text(")"));
                }
            }
            out.push(Doc::text(";"));
            Doc::concat(out)
        }
        other => unreachable!("print_decl_stmt called with non-decl {other:?}"),
    }
}
```

- [ ] **Step 5: Fill in `Stmt::Let`'s type annotation in `print_stmt.rs`**

Find (from Task 4):
```rust
        Stmt::Let { name, mutable, init, .. } => {
            let kw = if mutable { "let mut " } else { "let " };
            Doc::concat(vec![
                Doc::text(kw),
                Doc::text(interner.resolve(name).to_string()),
                Doc::text(" = "),
                print_expr(ast, interner, init, Prec::None),
                Doc::text(";"),
            ])
        }
```
Replace with:
```rust
        Stmt::Let { name, mutable, ty, init } => {
            let kw = if mutable { "let mut " } else { "let " };
            let mut out = vec![
                Doc::text(kw),
                Doc::text(interner.resolve(name).to_string()),
            ];
            if let Some(t) = ty {
                out.push(Doc::text(": "));
                out.push(crate::print_type::print_type(ast, interner, t));
            }
            out.push(Doc::text(" = "));
            out.push(print_expr(ast, interner, init, Prec::None));
            out.push(Doc::text(";"));
            Doc::concat(out)
        }
```

- [ ] **Step 6: Wire up `lib.rs`**

Add `pub mod print_type;` to `crates/ember-fmt/src/lib.rs` (it should already have `pub mod print_decl;` from Task 4's stub — leave that line as-is, only its file contents changed).

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ember-fmt`
Expected: PASS, everything from Tasks 2-5.

- [ ] **Step 8: Clippy and fmt**

Run: `cargo clippy -p ember-fmt --all-targets -- -D warnings` and `cargo fmt -p ember-fmt -- --check` — both clean.

- [ ] **Step 9: Commit**

```bash
git add crates/ember-fmt/src/print_decl.rs crates/ember-fmt/src/print_type.rs crates/ember-fmt/src/print_stmt.rs crates/ember-fmt/src/lib.rs
git commit -m "AST to Doc: top-level declarations (fn/struct/type) and type expressions"
```

---

### Task 6: Top-level driver — `format(src)`, blank-line preservation, comment attachment

**Files:**
- Create: `crates/ember-fmt/src/format.rs`
- Modify: `crates/ember-fmt/src/lib.rs`

- [ ] **Step 1: Write the failing tests first**

Create `crates/ember-fmt/src/format.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_statements_get_a_hardline_between_them() {
        assert_eq!(format("let x = 1;\nx;\n"), "let x = 1;\nx;\n");
    }

    #[test]
    fn blank_line_between_top_level_items_is_preserved_capped_at_one() {
        assert_eq!(format("let x = 1;\n\n\n\nlet y = 2;\n"), "let x = 1;\n\nlet y = 2;\n");
        assert_eq!(format("let x = 1;\nlet y = 2;\n"), "let x = 1;\nlet y = 2;\n");
        assert_eq!(format("let x = 1;\n\nlet y = 2;\n"), "let x = 1;\n\nlet y = 2;\n");
    }

    #[test]
    fn leading_line_comment_is_preserved_before_its_statement() {
        assert_eq!(
            format("// hello\nlet x = 1;\n"),
            "// hello\nlet x = 1;\n"
        );
    }

    #[test]
    fn trailing_same_line_comment_is_preserved_after_its_statement() {
        assert_eq!(
            format("let x = 1; // hi\n"),
            "let x = 1; // hi\n"
        );
    }

    #[test]
    fn output_always_ends_with_exactly_one_trailing_newline() {
        assert_eq!(format("let x = 1;"), "let x = 1;\n");
        assert_eq!(format("let x = 1;\n\n\n"), "let x = 1;\n");
    }

    #[test]
    fn idempotent_on_the_full_conformance_suite() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("em") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let once = format(&src);
            let twice = format(&once);
            assert_eq!(once, twice, "not idempotent on {path:?}");
            checked += 1;
        }
        assert!(checked >= 6, "expected at least 6 conformance fixtures, found {checked}");
    }

    #[test]
    fn semantics_preserved_on_the_full_conformance_suite() {
        // run(x) == run(fmt(x)) via the tree-walking interpreter (cheaper
        // to invoke here than standing up the VM's own pipeline).
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("em") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let formatted = format(&src);
            let before = run_via_tree_walker(&src);
            let after = run_via_tree_walker(&formatted);
            assert_eq!(before, after, "semantics changed for {path:?}");
            checked += 1;
        }
        assert!(checked >= 6, "expected at least 6 conformance fixtures, found {checked}");
    }

    fn run_via_tree_walker(src: &str) -> String {
        let (ast, interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        // `ember_tree::interpret`'s real signature (`ember-tree/src/interp.rs:738`)
        // is `(ast: &Ast, interner: &Interner, stmts: &[Idx<Stmt>]) ->
        // (Option<Value>, Option<RuntimeError>)` — no `Bindings` parameter
        // at all (the tree-walker does dynamic environment lookup, not
        // resolved binding), so no resolve pass is needed here.
        let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
        format!("{result:?} {err:?}")
    }
}
```

Add `ember-tree` as a **dev-dependency** (not a regular dependency — only test code needs it) to `crates/ember-fmt/Cargo.toml`:
```toml
[dev-dependencies]
ember-tree = { path = "../ember-tree" }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-fmt format`
Expected: FAILS to compile — `format` doesn't exist.

- [ ] **Step 3: Add two more tests covering comment/blank-line interaction**

These specifically pin down cases that a naive "handle comments in one loop, blank lines in a separate loop" implementation gets wrong (a leading comment with no blank line before its statement would spuriously gain one from the statement-to-statement gap calculation counting the comment's own line as part of the gap; a standalone comment genuinely separated by a blank line would have that blank line misplaced relative to the comment). Add to the test module, alongside `leading_line_comment_is_preserved_before_its_statement`:

```rust
    #[test]
    fn a_leading_comment_with_no_blank_line_gains_none() {
        assert_eq!(
            format("let x = 1;\n// comment\nlet y = 2;\n"),
            "let x = 1;\n// comment\nlet y = 2;\n"
        );
    }

    #[test]
    fn a_standalone_comment_after_a_real_blank_line_keeps_the_blank_line_before_it() {
        assert_eq!(
            format("let x = 1;\n\n// standalone\nlet y = 2;\n"),
            "let x = 1;\n\n// standalone\nlet y = 2;\n"
        );
    }
```

- [ ] **Step 4: Implement `format`**

Add above the test module in `crates/ember-fmt/src/format.rs`. The key design point: comments and statements are merged into ONE source-ordered sequence of "items," and blank-line-gap detection runs uniformly over consecutive items in that sequence — not as two separate passes (one for comments, one for statement-to-statement gaps) that would otherwise disagree about where a gap "really" is whenever a comment sits between two statements. A trailing (same-source-line) comment is the one exception: it's glued directly onto its statement's own output with no gap check at all, since by definition it shares that statement's line.

```rust
use crate::doc::Doc;
use ember_span::{SourceMap, Span};

/// Formats a whole program: parses `src`, lowers every top-level
/// statement to a `Doc`, interleaves comments (obtained via a second,
/// independent lex pass — see this crate's own design doc for why this
/// doesn't touch `ember_parser::parse`'s public signature) and blank-line
/// preservation between top-level items, and renders the result. Always
/// returns a string ending in exactly one `\n`.
pub fn format(src: &str) -> String {
    let (ast, interner, stmts, _parse_diags) = ember_parser::parse(src);
    let (_tokens, trivia, _lex_diags) = ember_lexer::lex(src);
    let source_map = SourceMap::new(src);

    // Classify each trivia as either "trailing" (glued onto the
    // immediately preceding statement, same source line, no blank-line
    // gap logic) or "standalone" (leading/freestanding — takes part in
    // the same ordered, gap-checked sequence statements do). A trivia is
    // trailing-of-statement-i if it starts after that statement's own
    // span ends, on the SAME source line as that end, and before the
    // next statement's span begins.
    let mut trailing_of: Vec<Option<usize>> = vec![None; trivia.len()];
    for (ti, t) in trivia.iter().enumerate() {
        for (i, &s) in stmts.iter().enumerate() {
            let stmt_span = ast.span_of_stmt(s);
            if t.span.start < stmt_span.end {
                continue;
            }
            let next_start = stmts
                .get(i + 1)
                .map(|&next| ast.span_of_stmt(next).start)
                .unwrap_or(u32::MAX);
            if t.span.start >= next_start {
                continue;
            }
            let (stmt_end_line, _) = source_map.line_col(stmt_span.end);
            let (trivia_line, _) = source_map.line_col(t.span.start);
            if trivia_line == stmt_end_line {
                trailing_of[ti] = Some(i);
            }
            break;
        }
    }

    // The merged, source-ordered sequence of top-level items: every
    // statement, plus every NON-trailing trivia.
    enum Item {
        Stmt(usize),
        Trivia(usize),
    }
    let mut items: Vec<Item> = Vec::new();
    for i in 0..stmts.len() {
        items.push(Item::Stmt(i));
    }
    for ti in 0..trivia.len() {
        if trailing_of[ti].is_none() {
            items.push(Item::Trivia(ti));
        }
    }
    let item_span = |item: &Item| -> Span {
        match item {
            Item::Stmt(i) => ast.span_of_stmt(stmts[*i]),
            Item::Trivia(ti) => trivia[*ti].span,
        }
    };
    items.sort_by_key(|item| item_span(item).start);

    let mut out = Vec::new();
    let mut prev_end_line: Option<u32> = None;
    for item in &items {
        let span = item_span(item);
        let (start_line, _) = source_map.line_col(span.start);
        if let Some(prev_line) = prev_end_line {
            out.push(Doc::HardLine);
            if start_line > prev_line + 1 {
                out.push(Doc::HardLine);
            }
        }
        match item {
            Item::Stmt(i) => {
                out.push(crate::print_stmt::print_stmt(&ast, &interner, stmts[*i]));
                for (ti, t) in trivia.iter().enumerate() {
                    if trailing_of[ti] == Some(*i) {
                        out.push(Doc::text(" "));
                        out.push(Doc::text(
                            src[t.span.start as usize..t.span.end as usize].to_string(),
                        ));
                    }
                }
            }
            Item::Trivia(ti) => {
                let t = &trivia[*ti];
                out.push(Doc::text(
                    src[t.span.start as usize..t.span.end as usize].to_string(),
                ));
            }
        }
        let (end_line, _) = source_map.line_col(span.end);
        prev_end_line = Some(end_line);
    }

    let mut rendered = crate::render(Doc::concat(out), 100);
    while rendered.ends_with('\n') {
        rendered.pop();
    }
    rendered.push('\n');
    rendered
}
```

Note this version needs no "strip the artifact leading HardLine" hack (unlike an earlier, incorrect draft of this function) — since `prev_end_line` starts as `None`, the very first item in `items` never has a `HardLine` pushed before it in the first place.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ember-fmt format`
Expected: PASS, all of them including the two conformance-suite-driven tests.

If the idempotence or semantics tests fail on a specific fixture, investigate the exact diff (add a `eprintln!("{once:?}")`-style temporary debug print if needed, remove before committing) — do not weaken the assertion; a real failure here means a real formatter bug (most likely in comment/blank-line handling, or a precedence/paren mistake from Task 3 that changes what a fixture's next-format-pass reparses to).

- [ ] **Step 6: Wire up `lib.rs`**

Add to `crates/ember-fmt/src/lib.rs`:
```rust
pub mod format;
pub use format::format;
```

- [ ] **Step 7: Run full test suite, clippy, fmt**

Run: `cargo test -p ember-fmt`, `cargo clippy -p ember-fmt --all-targets -- -D warnings`, `cargo fmt -p ember-fmt -- --check` — all clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-fmt/Cargo.toml crates/ember-fmt/src/format.rs crates/ember-fmt/src/lib.rs
git commit -m "Add the top-level format() driver: blank-line preservation and comment attachment"
```

---

### Task 7: `ember fmt` CLI command

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/src/main.rs`

- [ ] **Step 1: Add the dependency**

Add to `crates/ember-cli/Cargo.toml`'s `[dependencies]`:
```toml
ember-fmt = { path = "../ember-fmt" }
```

- [ ] **Step 2: Add the `Fmt` variant to the `Command` enum**

`crates/ember-cli/src/main.rs`'s existing `Command` enum (verified directly, this is its real current shape) ends with:

```rust
    /// Parse, resolve, typecheck, check exhaustiveness, then compile to
    /// bytecode and run it on the VM, printing its final value or a
    /// rendered runtime-error diagnostic — same pipeline as `run`, but the
    /// bytecode backend instead of the tree-walker.
    Vm { file: String },
}
```

Add a new variant right after `Vm`, before the closing `}`:

```rust
    /// Format a file. Rewrites it in place by default; with `--check`,
    /// reports whether it's already formatted without writing, exiting
    /// non-zero if not.
    Fmt {
        file: String,
        #[arg(long)]
        check: bool,
    },
}
```

- [ ] **Step 3: Add the dispatch arm**

`main()`'s existing dispatch `match` ends with:
```rust
        Command::Vm { file } => run_vm(&file),
    }
}
```
Add:
```rust
        Command::Vm { file } => run_vm(&file),
        Command::Fmt { file, check } => run_fmt(&file, check),
    }
}
```

- [ ] **Step 4: Add `run_fmt`**

Add this function near the other `run_*` functions (e.g. right after `run_vm`), reusing the existing `read_source` helper exactly as every other `run_*` function does:

```rust
fn run_fmt(path: &str, check: bool) -> ExitCode {
    let Some(src) = read_source(path) else {
        return ExitCode::from(3);
    };
    let formatted = ember_fmt::format(&src);
    if check {
        if formatted == src {
            ExitCode::SUCCESS
        } else {
            eprintln!("{path} is not formatted");
            ExitCode::from(2)
        }
    } else {
        match fs::write(path, &formatted) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: could not write {path}: {e}");
                ExitCode::from(3)
            }
        }
    }
}
```

(`fs` is already imported at the top of this file via `use std::fs;` — no new import needed. `ExitCode::from(3)` matches `read_source`'s own established convention for I/O failures; `ExitCode::from(2)` matches every other `run_*` function's convention for "ran successfully but found something to report" — see `print_diagnostics`'s own `ExitCode::from(2)` return for un-formatted/erroring input.)

- [ ] **Step 6: Manual verification**

Run: `cargo run -p ember-cli -- fmt tests/conformance/arithmetic.em --check`
Expected: since the conformance fixtures are already close to this formatter's own style (matching the style this plan deliberately mirrored), this may already report "formatted" — either outcome (already formatted, or a clean diff-and-rewrite) is fine; what matters is the command runs without panicking and produces sensible output. Also run without `--check` against a scratch copy of a fixture (not the real fixture file, to avoid perturbing the checked-in test corpus) and inspect the output looks like well-formed `ember` source.

- [ ] **Step 7: Clippy and fmt**

Run: `cargo clippy -p ember-cli --all-targets -- -D warnings` and `cargo fmt -p ember-cli -- --check` — both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-cli/Cargo.toml crates/ember-cli/src/main.rs
git commit -m "Add the ember fmt CLI command"
```

---

### Task 8: Snapshot tests (🟢 enhancement)

**Files:**
- Modify: `crates/ember-fmt/src/format.rs`

- [ ] **Step 1: Add a snapshot-style test over the conformance corpus**

Add to `format.rs`'s test module:

```rust
#[test]
fn formatting_every_conformance_fixture_produces_stable_non_empty_output() {
    // A lightweight stand-in for full snapshot testing (no snapshot
    // fixtures are checked in for this — the idempotence/semantics tests
    // above are the real correctness proof): confirms every fixture in
    // the corpus formats to non-empty, `\n`-terminated output without
    // panicking, across every file in the existing conformance suite
    // (already checked at >= 6 by the other tests in this module).
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("em") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let formatted = format(&src);
        assert!(!formatted.is_empty(), "{path:?} formatted to empty output");
        assert!(formatted.ends_with('\n'), "{path:?} formatted output must end in \\n");
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p ember-fmt formatting_every_conformance_fixture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/ember-fmt/src/format.rs
git commit -m "Add a lightweight snapshot-style test over the conformance corpus"
```

---

### Task 9: Full workspace verification and `CHECKLIST.md` reconciliation

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Full workspace verification**

Run: `cargo test --workspace` — PASS, 0 failures.
Run: `cargo clippy --workspace --all-targets -- -D warnings` — clean.
Run: `cargo fmt --all -- --check` — clean.

- [ ] **Step 2: Reconcile the Phase 11 section of `CHECKLIST.md`**

Check off every genuinely-completed item (Wadler-style pretty printer, layout algorithm, format every AST node, preserve comments, preserve blank lines capped at 1, group binary chains, `ember fmt --check`, idempotence test, semantics test, comment attachment, snapshot tests). Document any real deviations found during implementation (e.g. if the subagent implementing this plan found and fixed a bug in the plan's own sketched code, or a case this plan didn't anticipate).

- [ ] **Step 3: Final re-verification and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check` — all clean.

```bash
git add CHECKLIST.md
git commit -m "Reconcile Phase 11 (Formatter) against CHECKLIST.md"
```
