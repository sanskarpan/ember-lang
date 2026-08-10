# Phase 11: Formatter — Design

## Goal

Build `ember fmt`, a source-code formatter for `ember`, in the currently-stub `ember-fmt` crate. Comment preservation is a hard requirement of this phase's own checklist items, which requires retrofitting `ember-lexer` with a comment side-channel first (currently discarded during lexing, explicitly flagged in `CHECKLIST.md` as deferred to this phase).

## Prerequisite: trivia retention in `ember-lexer`

`Lexer::skip_trivia` currently advances past whitespace and comments with no record kept. Add:

```rust
pub enum TriviaKind { Line, Block }
pub struct Trivia { pub kind: TriviaKind, pub span: Span }
```

`lex()`'s signature changes from `(Vec<Token>, Vec<Diagnostic>)` to `(Vec<Token>, Vec<Trivia>, Vec<Diagnostic>)` — a genuine side channel (Vec pushed to alongside the existing token loop), not interleaved into the token stream, so `Token`/`TokenKind` and every consumer's assumptions about the shape of `Vec<Token>` are completely unaffected. Blank-line preservation does not need its own trivia entry — `ember-span::SourceMap::line_col` already turns two adjacent nodes' byte offsets into line numbers, and the formatter already has every AST node's `Span`.

**Blast radius**: `ember_lexer::lex()` has 6 call sites total (`ember-cli/src/main.rs`: 1, `ember-parser/src/parser.rs`: 3 — `parse`, and the two `#[cfg(test)]` helpers `parse_expr_from_str`/`parse_stmt_from_str`, `ember-lexer/tests/proptest_lexer.rs`: 2). All are small, mechanical updates (destructure one more tuple element, most discarding it with `_`).

**Deliberate non-change**: `ember_parser::parse()`'s own public signature (`(Ast, Interner, Vec<Idx<Stmt>>, Vec<Diagnostic>)`) stays exactly as it is. `parse()` has dozens of existing callers across the whole workspace (every crate's own test suite), none of which need trivia — growing its return tuple would ripple everywhere for zero benefit to those callers. Internally, `parse()` now discards the trivia `lex()` returns (`let (tokens, _trivia, lex_diags) = ember_lexer::lex(src);`), functionally identical to today. `ember-fmt` is the one caller that needs trivia; it gets it by calling `ember_lexer::lex(src)` a second time, directly, alongside its own call to `ember_parser::parse(src)` for the AST. This means formatting re-lexes the source once — a trivial, isolated cost paid only by the one caller that needs it, in exchange for not touching a wide, workspace-spanning blast radius.

## `ember-fmt`: Wadler-style Doc IR + layout

```rust
pub enum Doc {
    Text(String),
    Line,            // space if the enclosing Group fits, newline+indent otherwise
    HardLine,         // always a newline+indent, regardless of fit
    Nest(usize, Box<Doc>),
    Concat(Vec<Doc>),
    Group(Box<Doc>),  // the unit the fits-check operates on
}
```

Layout: a single-pass renderer that, on entering a `Group`, checks whether the flattened (`Line` → space) rendering of its contents fits within the remaining budget on the current line (target width 100, matching this project's own conformance-fixture style and the checklist's stated default); if so renders flat, otherwise renders with every `Line` inside it as a real break, propagating the current indent (tracked via `Nest`).

## AST → `Doc`

One lowering function per node kind (expressions, statements, patterns, declarations), matching the style already consistently used throughout this project's own `tests/conformance/*.em` fixtures and every inline test-source string across the whole codebase: K&R braces (`{` on the same line), 4-space indent via `Nest`, space around binary operators, trailing commas in any construct that breaks across multiple lines (`match` arms, struct/record literals, list literals, function parameters), semicolon-terminated statements.

Binary operator chains: grouped by precedence level, so a chain that doesn't fit breaks consistently at every operator of that level (not just the first one that overflows) — the standard Wadler `Group`-per-precedence-level technique.

## Comment attachment

The formatter walks the AST in source order (every node already carries a `Span` from `ember-span`) alongside the trivia list (sorted by position, obtained via the redundant re-lex above). For each node about to be formatted:
- Any not-yet-consumed comment whose span starts before the node's span becomes a **leading** comment: emitted on its own line, before the node.
- After formatting a node, any not-yet-consumed comment on the same source line as the node's end (before the next newline) becomes a **trailing** comment: appended after, same line.

No fancier heuristic than that — matches the checklist's "land in sensible places" bar (not a prettier-grade attachment algorithm). Both cases are testable directly against fixture-style source.

## Blank lines

Between top-level items only (not inside blocks), derived from `SourceMap::line_col` on adjacent items' spans, capped at 1 regardless of how many blank lines existed in the source.

## CLI

`ember fmt FILE [--check]` — default mode rewrites the file in place; `--check` exits non-zero on any diff without writing, printing nothing on stdout (matches standard formatter CLI conventions, e.g. `rustfmt --check`, `prettier --check`).

## Testing strategy

- Idempotence (`fmt(fmt(x)) == fmt(x)`) and semantics (`run(x) == run(fmt(x))`, checked via the tree-walking interpreter, the cheaper of the two backends to invoke for this purpose) run directly against the existing `tests/conformance/*.em` fixtures — no new fixture corpus needed for either property.
- Comment attachment: dedicated small source snippets (leading, trailing, inline-between-arguments) with expected output asserted directly.
- Blank-line preservation: dedicated snippets with 0, 1, and 2+ blank lines between top-level items, asserting the output always has at most 1.
- Snapshot tests over ~20 files (🟢 enhancement, not required): reuse the conformance corpus plus a handful of formatter-specific edge-case files.

## Non-goals

- A configurable style (line width, indent size, brace style) — one fixed style, matching the project's own existing convention, no config file.
- Comment attachment beyond leading/same-line-trailing (e.g. comments attached mid-expression between operators) — deferred, not required by the checklist.
- NaN-boxing/perf work — unrelated to this phase.
