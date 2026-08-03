# ember: Phase 0-3 Design — Bootstrap, Lexer, AST, Parser

Status: approved
Date: 2026-08-03
Scope: first buildable slice of the `ember` language project (see `PROMPT.md`, `CHECKLIST.md`, `SPEC.md` at repo root). Covers Phase 0 (Bootstrap), Phase 1 (Lexer), Phase 2 (AST), Phase 3 (Parser) only. Everything from Phase 4 (Resolver) onward is out of scope for this spec and will get its own design cycle.

## Context

`ember` is a from-scratch language implementation: hand-written lexer, Pratt parser, resolver, Hindley-Milner inference, two execution backends (tree-walker + bytecode VM), a GC, formatter, LSP, and a React/WASM playground — 368 tasks across 17 phases per `CHECKLIST.md`. The three project-level rules that override everything else:

1. No parser generator (no lalrpop/pest/chumsky/nom).
2. The conformance suite (tree-walk vs VM byte-identical output) is the spine — starts in Phase 8, not relevant yet.
3. The lexer and parser never fail — `lex` always returns a full token stream, `parse` always returns a complete tree.

This project directory is not currently its own git repository — the user's home directory has a `.git` tracking everything under it. Part of Phase 0 is `git init`-ing this directory properly so commits don't land in the home-dir repo.

The machine has no Rust toolchain installed (`cargo`/`rustc` both absent). Bun is present. Installing Rust via `rustup` is in scope for this slice; frontend/playground scaffolding is explicitly deferred (nothing in Phases 0-3 needs WASM or the browser — that infrastructure doesn't exist until Phase 15).

## Non-goals for this slice

- Resolver, type inference, exhaustiveness checking, both execution backends, GC, formatter, LSP, WASM bindings, playground — all later phases with their own specs.
- Full CLI (`ember run/repl/fmt/lsp/trace/bench/debug/explain`) — Phase 13. This slice only needs a minimal `ember-cli` with `tokens` and `ast` subcommands for manual sanity-checking during development.
- Fuzzing, criterion benchmarks, `logos`-based lexer variant — all marked 🟡 (important, not blocking) in `CHECKLIST.md`; skipped this round, tracked as deferred work.

## Workspace structure

Full 16-crate skeleton per `SPEC.md` §17, satisfying Phase 0 checklist item 1 literally:

```
ember/
├── Cargo.toml                # workspace
├── crates/
│   ├── ember-span/           # IMPLEMENTED this slice
│   ├── ember-diag/           # IMPLEMENTED this slice
│   ├── ember-lexer/          # IMPLEMENTED this slice
│   ├── ember-ast/            # IMPLEMENTED this slice
│   ├── ember-parser/         # IMPLEMENTED this slice
│   ├── ember-resolve/        # stub — Phase 4
│   ├── ember-types/          # stub — Phase 5
│   ├── ember-tree/           # stub — Phase 7
│   ├── ember-bytecode/       # stub — Phase 8
│   ├── ember-compile/        # stub — Phase 8
│   ├── ember-vm/             # stub — Phase 9
│   ├── ember-gc/             # stub — Phase 10
│   ├── ember-fmt/            # stub — Phase 11
│   ├── ember-lsp/            # stub — Phase 14
│   ├── ember-wasm/           # stub — Phase 15
│   └── ember-cli/            # MINIMAL this slice (tokens, ast subcommands only)
├── tests/
│   └── snapshots/            # insta snapshots for parser
└── docs/superpowers/specs/   # this design doc and future ones
```

Dependency layering is enforced by construction: `ember-span` has no internal deps; `ember-diag` depends on `ember-span`; `ember-lexer` depends on `ember-span` + `ember-diag`; `ember-ast` depends on `ember-span`; `ember-parser` depends on all four. No back-edges.

## `ember-span`

- `Span { start: u32, end: u32 }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, 8 bytes.
- `SourceMap`: precomputed `Vec<u32>` of line-start byte offsets built once from the source text; `line_col(offset: u32) -> (u32, u32)` via binary search — O(log n), per Phase 0 checklist item 4.

## `ember-diag`

- `Diagnostic { severity: Severity, code: Option<&'static str>, message: String, labels: Vec<Label>, notes: Vec<String>, help: Vec<Help> }`.
- `Label { span: Span, message: String, primary: bool }`, `Help { message: String, suggestion: Option<Suggestion> }`, `Suggestion(Span, String)` (span + replacement text, for future LSP code actions), `Severity::{Error, Warning, Note, Help}`.
- Builder methods matching the sketches in `PROMPT.md`/`SPEC.md`: `Diagnostic::error(msg)`, `.with_code()`, `.with_primary()`/`.with_secondary()` (both push into `labels` with `primary` set accordingly), `.with_note()`, `.with_help()`.
- `ariadne`-backed renderer: multi-span labels, notes, help text, color output, with a `NO_COLOR` env var and non-TTY fallback to plain ASCII (no unicode box-drawing) per Phase 0 checklist item 6.

## `ember-lexer`

Hand-rolled, matching `PROMPT.md` Phase 1 and `SPEC.md` §4 line-for-line — no `logos` in this slice.

- `Token { kind: TokenKind, span: Span }` — `#[derive(Debug, Clone, Copy, PartialEq)]`, 12 bytes, owns no text; text recovered via `&src[span.start as usize..span.end as usize]`.
- `TokenKind`: all literals (`Int`, `Float`, `Str`, `True`, `False`, `Ident`), 18 keywords (`let mut fn if else while for in loop break continue return match type struct import nil` — per `SPEC.md` §4, plus any implied by Phase 3's statement grammar), 25 operators/delimiters, `Comment`, `Whitespace`, `Newline`, `Eof`, `Error`.
- `lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>)` — **total function**: never panics, never returns early, always terminates with `Eof`. Unrecognized characters produce `TokenKind::Error` + a diagnostic and lexing continues.
- Maximal munch, longest-operator-first: `==` before `=`, `=>` before `=`, `..` before `.`, `::` before `:`, `->` before `-`, etc.
- Number lexing: decimal/`0x`/`0b`/`0o` integers with `_` separators; the `1..10` vs `1.5` rule (a `.` after digits is only a float if the *next* char is also a digit); `1e10`/`1.5e-3` exponents; overflow on integer literals is a diagnostic, not a panic; bare `1.` / `.5` rejected with a targeted message rather than silently accepted.
- String literals with escapes `\n \t \r \\ \" \0 \u{XXXX}`; unterminated string reports at the **opening quote**, not EOF.
- Line comments `//`; nested block comments `/* /* */ */` via a depth counter (naive scan-to-first-`*/` is a known-wrong shortcut this must avoid).
- Keyword recognition via direct `match` on the identifier slice, not a `HashMap` lookup.
- `string-interner`-backed `Symbol(u32)` for every identifier; scope/lookup code downstream compares integers, never strings.
- Trivia (whitespace, comments) retained in a side channel rather than discarded — unused by anything in this slice, but required later by the formatter/LSP, and much cheaper to plumb through now than to retrofit.
- UTF-8 correctness: all spans are byte offsets; the lexer never splits a multi-byte character.

Deferred (🟡, not blocking): `logos`-derive variant behind a feature flag, benchmarked against the hand-rolled lexer; fuzz target; >50MB/s throughput benchmark.

## `ember-ast`

- Arena-based, not `Box<Expr>`: `Ast { exprs: Vec<Expr>, stmts: Vec<Stmt>, pats: Vec<Pattern>, spans: Vec<Span> }`.
- `Idx<T> { raw: u32, _marker: PhantomData<T> }` — `#[derive(Clone, Copy, PartialEq, Eq, Hash)]`, 4 bytes. Rationale (per `SPEC.md` §5): cache-local contiguous storage, no recursive `Drop` (a 100k-node tree must not blow the stack when freed), and `Idx` being `Copy` means tree transforms don't fight the borrow checker.
- `Expr` variants: `Int(i64)`, `Float(f64)`, `Str(Symbol)`, `Bool(bool)`, `Nil`, `Var(Symbol)`, `Unary{op, operand}`, `Binary{op, lhs, rhs}`, `Assign{target, value}`, `Call{callee, args}`, `Index{base, index}`, `Field{base, name}`, `Lambda{params, body}`, `If{cond, then_, else_}`, `Match{scrutinee, arms}`, `Block{stmts, tail}`, `List{items}`, `Struct{name, fields}`, `Error`.
- `Stmt` variants: `Let{name, mutable, ty, init}`, `ExprStmt`, `Fn{name, params, ret_ty, body}`, `TypeDecl{name, variants}`, `StructDecl{name, fields}`, `While{cond, body}`, `For{binding, iter, body}`, `Loop{body}`, `Return{value}`, `Break`, `Continue`, `Error`.
- `Pattern` variants: `Wild`, `Bind(Symbol)`, `Literal(...)`, `Ctor{name, args}`, `Tuple(Vec<Idx<Pattern>>)`, `List{items, rest: Option<...>}` (rest = `..tail`), `Record{fields}`, `Or(Vec<Idx<Pattern>>)`.
- `Ast::alloc_expr/alloc_stmt/alloc_pat` allocate and record the span in the same call; `Ast::span_of(idx)` covers every node kind across all three arenas.
- A visitor trait (or explicit `walk_*` functions) usable later by the resolver/typer/both backends — defined now so later phases don't each hand-roll traversal.
- A pretty-printer that emits valid `ember` source text from an `Ast` — needed for the parser round-trip property test in this same slice.
- `serde::Serialize` derives on the AST types (including per-node spans) — cheap to add alongside the struct definitions now; the playground's AST panel (Phase 16) will consume this later, but adding it retroactively across 30+ variants is not cheap.
- `Error` variants exist on `Expr`, `Stmt`, and `Pattern` — required by the parser's recovery mechanism below.

## `ember-parser`

Pratt / precedence-climbing core, per `SPEC.md` §5:

- `Prec` enum: `None < Assign < Or < And < Equality < Comparison < Term < Factor < Unary < Call < Primary` (`#[repr(u8)]`, `PartialOrd`).
- `TokenKind::infix_prec() -> Prec` table.
- `expr(min_prec: Prec) -> Idx<Expr>`: NUD (`prefix()`) then a loop absorbing LED (`infix()`) while `peek().infix_prec() > min_prec`.
- **The associativity rule**: left-associative operators recurse into `self.expr(prec)` (same precedence — an equal-precedence operator fails the caller's `> min_prec` check and gets absorbed by the *outer* loop, producing left-nesting); right-associative operators (currently just `=`) recurse into `self.expr(prec.lower())` (one step lower — an equal-precedence operator now passes the inner call's check and nests right). This one `.lower()` call is the entire left/right distinction.
- Prefix parsers: int/float/string/bool/nil literals, identifier, `(` grouping, `-`/`!` unary, `[` list literal, `|params|`/`||` lambda, `if`/`else if`/`else`, `match`, `{` block, struct literal.
- Infix parsers: all binary operators, assignment (validates the LHS is `Var`/`Index`/`Field` — anything else is a targeted "invalid assignment target" error, not a generic parse failure), `(` call, `[` index, `.` field access.

Statements: `let` (optional `mut`, optional `: Type`, required initializer), `fn` (params with optional types, optional `-> Type`, block body), `type` ADT declarations (`|`-separated variants with payload types), `struct` declarations (typed fields), `while`, `for .. in`, `loop`, `break`/`continue`/`return`, block expressions (`{ stmts…; tail_expr? }` where the tail expression, if present, is the block's value), and the semicolon rule (expression statements need `;` unless in tail position or the expression itself ends in `}`, e.g. `if`/`match`/block).

Patterns: all forms including list rest (`[head, ..tail]`) and or-patterns (`A | B`); match arms `pat => expr,` with optional `if cond` guard.

Types: `Int`, `[T]`, `(A, B) -> C`, `Name<Args>`, bare type variables.

**Error recovery** — built in from day one, not retrofitted, per `SPEC.md` §5 and the Phase 3 checklist:

- `panicking: bool` on the parser; `error_at()` records a suppressed placeholder diagnostic and is a no-op for surfaced errors while `panicking` is true — this is cascade suppression, the mechanism that turns "one missing semicolon" into one diagnostic instead of a cascade of ~15.
- `Expr::Error`/`Stmt::Error` placeholders keep the tree whole around a parse failure, so the resolver/typer (later phases) can still run on the good parts of a file that has one bad function.
- `synchronize()`: clears `panicking`, then skips tokens until a statement boundary — either just past a `;`, or at a token that can only start a new statement (`let fn if while for loop return match type struct` or `}`).
- `expect_close(open, close)`: unclosed `(`/`{`/`[` reports at the **opening** delimiter with a secondary label at the point where the closer was expected — not "unexpected EOF", which is useless in a multi-hundred-line file.
- A recursion-depth limit on `expr()` producing a clean "expression nested too deeply" diagnostic instead of a native stack overflow on pathological/adversarial input.

## Testing strategy for this slice

- Unit tests per lexer/parser feature, matching the named tests in `PROMPT.md`/`CHECKLIST.md`: spans tile the source exactly (no gaps/overlaps, first span starts at 0, last ends at `src.len()`); unterminated string reports at the opening quote; nested block comments close correctly at depth 3; `1..10` lexes as `Int DotDot Int`; `a.0.1` field-vs-float disambiguation; precedence (`1 + 2 * 3`); left-assoc (`1 - 2 - 3`); right-assoc (`a = b = c`); unary-binds-tighter (`-a + b`); call-binds-tightest (`-f(x)`); one-missing-semicolon-is-one-diagnostic; recovery preserves surrounding good code; unclosed-brace reports at the opener.
- `insta` snapshot tests over 20 representative `.em` programs (parser output shape).
- `proptest` property tests: lexer span-tiling holds for arbitrary generated source; `parse(pretty_print(ast))` is structurally equal to `ast`.
- Deferred (🟡): fuzz targets, `criterion` throughput benchmarks.

## Minimal `ember-cli`

Just enough to manually exercise the pipeline against real files during development — not the Phase 13 CLI:

```
ember tokens FILE            # print token stream with spans
ember ast FILE [--json]      # pretty-printed or JSON tree
```

Built on `clap` (already in the dependency list per `PROMPT.md` Phase 0), rendering diagnostics via `ember-diag`'s ariadne integration.

## CI

GitHub Actions: `cargo test` (workspace), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. No WASM build step yet (nothing to build) — that's added when `ember-wasm` gets real content in Phase 15.

## Git

This directory (`Interpreter-Lang/`) becomes its own git repository, separate from the pre-existing `~/.git`. Local git identity is set explicitly on this repo (`sanskarpandey2004@gmail.com` / `sanskarpan`) rather than inherited.

## Open items for the implementation plan

- Exact keyword list: cross-check the 18 keywords implied across `SPEC.md` §4 and the `for`/`struct`/`type` grammar in Phase 3 to make sure nothing is missing before writing `TokenKind`.
- `insta` snapshot review workflow (`cargo insta review`) should be documented in a short CONTRIBUTING note once snapshots exist.
