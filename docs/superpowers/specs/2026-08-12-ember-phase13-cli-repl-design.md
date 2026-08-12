# Phase 13 — CLI & REPL Design

## Goal

Grow `ember-cli` from its current 7 ad hoc subcommands (`tokens`, `ast`, `resolve`, `typecheck`, `run`, `vm`, `fmt`) into the full command surface SPEC.md §16 defines, plus a real REPL and an interactive TUI debugger — matching the checklist's full 🔴+🟡 scope.

## Pre-existing infrastructure this design builds on

- `Vm::step() -> Result<StepOutcome, RuntimeError>` already exists — the VM already supports single-instruction stepping.
- `ember_tree::interp::Interp` already exposes `pub fn exec_stmt`/`eval_expr` plus a `set_step_hook(Box<dyn FnMut(StepEvent)>)` mechanism whose own doc comment anticipates "the debugger."
- `ember_types::infer` already returns a populated `TypeInfo.trace: InferenceTrace` (`Vec<UnifyStep>`), built in an earlier phase specifically anticipating this one, with no consumer yet.
- `ember_bytecode::disasm::disassemble_chunk` already exists, exercised by Phase 12's snapshot tests.
- `ember-vm`'s `CountingAlloc` (Phase 12) already exists behind a `count-allocs` feature.
- `Diagnostic.code: Option<&'static str>` already exists on the type but is set nowhere in production code.
- `gc-stress` is currently a compile-time-only `cfg!(feature = "gc-stress")` check inside `GcHeap::should_collect`.

## 1. Command restructuring

- `run FILE [--backend tree|vm] [--time] [--gc-stress]` replaces the current separate `run`/`vm` subcommands. Default backend `tree` (preserves current `ember run`'s meaning). `--time` prints wall-clock elapsed (via `std::time::Instant`) after execution, to stderr so it doesn't pollute piped stdout. `--gc-stress` requires `GcHeap` to gain a runtime `stress: bool` field (constructor defaults it to `cfg!(feature = "gc-stress")`, preserving Phase 12's CI job and existing tests unchanged) plus a setter `GcHeap::set_stress(bool)` the CLI calls when the flag is passed; harmless no-op (with a one-line stderr note) when `--backend tree` is used, since the tree-walker has no GC.
- `check FILE`: runs parse → resolve → infer → exhaustiveness, prints every diagnostic, never executes. Exit `2` if any error-severity diagnostic, `0` otherwise.
- `tokens FILE` unchanged.
- `ast FILE [--json] [--typed]`: `--typed` becomes real — reuses `ember_types::infer` and annotates each printed statement's expressions with their inferred type (via `display_ty`). `--json` becomes real by adding `serde`/`Serialize` derives to `ember-ast`'s `Ast`/`Expr`/`Stmt`/`Pattern`/`TypeExpr` types (mechanical, no behavior change) and a small serializable projection (spans + kind + children), since the raw arena-of-indices representation isn't itself meaningful JSON.
- `types FILE` (new): every top-level binding's inferred, generalized scheme — the scheme-printing half of the current `Typecheck` command, without its exhaustiveness-diagnostic and per-expression-type output (those move to `--typed`/`check` respectively). The old `Typecheck`/`Resolve` commands are retired in favor of `types`/`ast --typed`/`check`; nothing in SPEC.md's command list calls for a standalone `resolve`/`typecheck` subcommand, and keeping them alongside the new ones would just be redundant surface.
- `disasm FILE` (new): compiles via the existing pipeline, prints `disassemble_chunk` output for the top-level chunk (nested closures' chunks too, recursively, matching the recursive helper Phase 12's own compiler tests already use as a pattern).
- **Exit code fix**: `run`'s runtime-error path changes from `2` to `1` (compile-time diagnostic paths — `parse`/`resolve`/`infer`/`exhaustiveness` failures — stay `2`; missing/unreadable file stays `3`), matching the checklist's explicit 0/1/2/3 contract, which the current code doesn't honor (it returns `2` for both compile and runtime errors).
- Colored output: `NO_COLOR` is already checked via `std::env::var_os`. Add real non-TTY detection via `std::io::IsTerminal::is_terminal()` (stable since Rust 1.70, no new dependency) on stdout, so redirected/piped output never carries ANSI codes even without `NO_COLOR` set.
- Shell completions: a hidden `ember completions <shell>` subcommand using `clap_complete`, printing the completion script to stdout for the user's shell to source — standard practice, no committed generated files to keep in sync.

## 2. Error-code registry + `explain`

- Every `Diagnostic::error`/`::warning` construction site across `ember-lexer`, `ember-parser`, `ember-resolve`, `ember-types` (infer/exhaustive/unify) — 51 call sites surveyed — gets `.with_code("E0NNN")`, grouped by logical error *kind* rather than 1:1 per call site (e.g. every parser "expected X, found Y" shares one code). Expected to land around 20-30 distinct codes.
- New `ember-diag::explain` module: a static `&[ExplainEntry]` registry (`ExplainEntry { code: &'static str, title: &'static str, body: &'static str }`), one entry per code assigned above, each with a longer explanation and a minimal illustrative example beyond what the one-line diagnostic message carries.
- `ember explain E0308` looks up and prints the entry (via the same colored-diagnostic-adjacent rendering style, not raw text) — unknown/unassigned codes print a clean "no explanation available for E0NNN" (exit `3`, usage-class error), never a panic or `unwrap`.

## 3. `trace` and `bench`

- `trace FILE`: parses/resolves, then calls `infer` and prints `TypeInfo.trace.steps` in order — each step's `Origin` (why the constraint was generated), both sides' types (rendered via `display_ty` against the *final* `Subst`, since per-step incremental substitution snapshots aren't stored by `InferenceTrace` today — documented in the command's own `--help` and in this design as a real, intentional simplification rather than a silently-missing "substitution evolution" claim), and pass/fail.
- `bench FILE`: runs both backends back to back, timing each via `Instant`, reading `ember_vm::alloc_stats()`/`reset_alloc_stats()` (Phase 12) around the VM run for allocation counts, and printing a speedup ratio (tree time / vm time). `ember-cli`'s `Cargo.toml` changes its `ember-vm` dependency to unconditionally enable `count-allocs` — the right call specifically for the CLI binary, whose `bench` command's whole purpose is measuring allocations, while every other `ember-vm` consumer (`ember-lsp`, `ember-wasm`, library tests) stays opt-in/zero-cost since the feature is additive per-dependency-edge, not workspace-wide.

## 4. REPL

- `rustyline` for line editing/history. Multi-line continuation: after each `Enter`, re-lex the accumulated input and check whether every `{`/`(`/`[` has a matching close (via the existing lexer's token stream — no new lexer work); if not, keep prompting with a continuation marker instead of submitting.
- **Incremental execution, real state persistence, no replay, for both backends:**
  - One `Interner` and one growing source-text buffer live for the REPL session. Each submitted entry is appended to the buffer and parsed via a **new** `ember_parser::parse_into(src: &str, interner: &mut Interner) -> (Ast, Vec<Idx<Stmt>>, Vec<Diagnostic>)` — `parse()` becomes a thin wrapper (`let mut interner = Interner::new(); let (ast, stmts, diags) = parse_into(src, &mut interner); (ast, interner, stmts, diags)`), so every existing caller (~100 call sites across the workspace) is unaffected. Re-parsing the whole buffer each entry is cheap and keeps `Symbol`s consistent across entries (same `Interner` instance throughout the session) without needing incremental-parser surgery.
  - Resolve/infer/exhaustiveness re-run on the whole buffer each entry too (cheap, side-effect-free) — but only to *validate* the new entry in context; only the **newly added** statements (the tail past the previous entry's statement count) are actually executed.
  - **Tree-walker backend**: a persistent `Interp` + `Env` (both already public) live for the session; each entry calls `exec_stmt`/`eval_expr` only on the new statements. No new tree-walker plumbing needed.
  - **VM backend** (two real additions, per the "build it properly" scope decision):
    1. `ember-resolve` gains a way to seed the top-level `FunctionCtx`'s outermost scope with "already-declared global" names carried over from prior REPL entries — extending the exact pattern `seed_native_globals` already uses for the 8 native functions, generalized to take an arbitrary list of pre-existing global names rather than a hardcoded native list.
    2. `Vm` gains a method (name TBD during planning, e.g. `run_incremental`) that compiles the new statements alone (via the seeded resolver above) into a small `FunctionProto` and executes it as a nested call against the **existing** `Vm`'s globals/stack, rather than constructing a fresh `Vm`. Its own `globals` map persists naturally since it's just read/written by `OP_DEFINE_GLOBAL`/`OP_GET_GLOBAL` as normal.
  - `:reset` discards the `Interner`/buffer/`Env`/`Vm` and starts a fresh session.
  - `:load file` reads a file's contents and feeds it through the exact same "append to buffer, parse, resolve, execute the new tail" path as typed input — not a special case.
  - `:type expr` / `:ast expr` / `:disasm expr`: parse the expression in the context of the accumulated buffer (so it can reference prior bindings) but only *analyze*, never execute or persist it into the buffer.
  - `--show-types`: after each entry's value prints, also print its inferred type (from the same `infer` call already run for validation).

## 5. `debug` TUI

- `ratatui` + `crossterm` (ratatui's default backend). Defaults to the VM backend, since `Vm::step()` gives real single-instruction granularity; the tree-walker's `StepEvent` hook can drive an analogous AST-level stepper as a secondary mode, not required by the checklist bullet itself.
- `Vm` gains new `pub` read-only introspection accessors (its `stack`/`frames`/`open_upvalues`/`globals` fields are currently private) — e.g. `pub fn stack_snapshot(&self) -> &[Value]`, `pub fn frames_snapshot(&self) -> &[CallFrame]` (or a purpose-built `VmSnapshot` struct bundling everything the TUI needs in one call) — read-only, no new mutation surface.
- Panels: source (current line/span highlighted, computed from the current frame's chunk line table), value stack, current frame's locals (mapped back to names via the resolver's `Bindings`), upvalues (open vs. closed), and the next instruction (one line of `disassemble_instruction` output). Keybindings: step, step-over (skip into a call and run it to completion), run-to-line (set a breakpoint at a source line, run until hit or program end), quit.

## Non-goals

- `--emit tokens|ast|hir|bytecode` pipeline dumping and a `--time` per-phase breakdown (both 🟢) are deferred, matching how prior phases scoped optional/nice-to-have items when the required+useful (🔴/🟡) scope was already large.
- The tree-walker's AST-level stepping mode for `debug` is a secondary, not-required stretch — the checklist's bullet is satisfied by the VM-backed stepper alone.
- No attempt to make `trace`'s substitution display show *per-step* historical substitution state — documented above as a real, acknowledged simplification (final-substitution rendering only).
