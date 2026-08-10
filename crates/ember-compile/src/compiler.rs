use ember_ast::{Ast, Expr, Idx, Interner, Pattern, Symbol};
use ember_bytecode::chunk::Chunk;
use ember_bytecode::op::Op;
use ember_bytecode::value::Value;
use ember_lexer::TokenKind;
use ember_resolve::{Bindings, FunctionId, Resolution};
use rustc_hash::FxHashMap;
use std::rc::Rc;

/// Fixed, unconditional runtime stack effect for ops whose effect doesn't
/// depend on an operand. `None` means "operand-dependent — adjust
/// `stack_depth` manually at the call site" (future tasks handle those).
pub(crate) fn static_stack_effect(op: Op) -> Option<i32> {
    use Op::*;
    match op {
        Constant | Nil | True | False => Some(1),
        Pop => Some(-1),
        GetLocal | GetGlobal | GetUpvalue => Some(1),
        SetLocal | SetGlobal | SetUpvalue => Some(0),
        DefineGlobal => Some(-1),
        CloseUpvalue => Some(-1),
        CloseUpvaluesFrom => Some(0), // closes in place; doesn't touch the stack
        GetField => Some(0), // consumes [base] (index is a compile-time constant operand, not a stack value), produces [value]
        GetIndex => Some(-1), // consumes [base, index] (both runtime stack values), produces [value]
        SetField => Some(-1),
        SetIndex => Some(-2),
        Equal | Greater | Less | Add | Sub | Mul | Div | Mod => Some(-1),
        Not | Negate => Some(0),
        Jump | Loop => Some(0),
        JumpIfFalse | JumpIfTrue => Some(-1),
        Closure => Some(1),
        TestVariant => Some(0), // pops the scrutinee, pushes a Bool
        Destructure => Some(0), // pops the base (an Adt), pushes the extracted positional field — its operand is an INDEX, not a count
        Return => Some(-1),     // pops the return value; the frame ends immediately after
        Print => Some(-1),
        Call | MakeList | MakeRecord | MakeAdt => None, // these alone have a genuinely operand-*count*-dependent effect
    }
}

/// One loop's patch targets and cleanup bookkeeping (used by future loop-
/// compiling tasks — `while`/`for`/`loop` each push one of these before
/// compiling their body and pop it after).
///
/// - `body_base_local_count`: `FunctionCompiler.local_count` at the moment
///   the loop body starts compiling. A `break`/`continue` found anywhere
///   inside the body — even nested several `Block`s deep — must pop
///   `local_count - body_base_local_count` values before jumping, since
///   jumping out bypasses the normal per-`Block` scope-exit cleanup that
///   would otherwise run for every block the jump escapes.
/// - `continue_jumps`: placeholder addresses of every `continue`'s forward
///   jump, patched once compilation reaches "the next iteration step" —
///   for `while`/`loop` that's immediately before the backward `OP_LOOP`;
///   for `for` (a future desugaring task) that's the counter-increment
///   code, which comes *after* the body, making a continue-from-inside-body
///   a forward jump even though it's semantically "go to the next lap."
/// - `break_jumps`: same idea, patched once the loop's end address (after
///   its own cleanup pops) is known.
pub(crate) struct LoopCtx {
    pub body_base_local_count: u32,
    pub continue_jumps: Vec<usize>,
    pub break_jumps: Vec<usize>,
}

/// A local-slot addressing correction, needed only for `for`-loop
/// desugaring (a future task). The resolver assigns each declared name an
/// absolute, frame-relative slot number assuming every declaration
/// corresponds to exactly one physical stack push — but the compiler's
/// `for`-loop desugaring will push two compiler-only hidden locals (the
/// iterated list, the index counter) that the resolver has no idea exist.
/// Every resolver slot number `>= base` — the loop binding itself, and
/// anything declared inside the loop body afterward — physically lands
/// `extra` positions further up the stack than the resolver assumed, so
/// `FunctionCompiler::physical_slot` adds `extra` to any resolver slot
/// `>= base` before it's written into a `GetLocal`/`SetLocal` operand
/// byte. Unused (empty `slot_shifts`, `physical_slot` is the identity
/// function) until that future task.
pub(crate) struct SlotShift {
    pub base: u32,
    pub extra: u32,
}

/// Where one `Ctor`/`Record`/`List` sub-pattern's value comes from, for
/// `compile_destructured_bind` (used only by `compile_pattern_bind`, which
/// never branches — see its own doc comment).
enum DestructureSource {
    Positional(u8),
    Named(Symbol),
    Indexed(i64),
}

/// One function's compilation-in-progress state: its own chunk, a running
/// local-slot counter (bookkeeping only — the resolver already assigned
/// real slot numbers; this just tracks how many are live for scope-exit
/// `OP_POP` counts), the loop-context stack for break/continue, the
/// active `for`-loop slot-shift stack, and the debug-mode running stack
/// depth.
pub struct FunctionCompiler {
    pub(crate) function_id: FunctionId,
    pub(crate) chunk: Chunk,
    pub(crate) local_count: u32,
    // 0 at the top level, incremented on `Block` entry, decremented on
    // exit — lets `maybe_dual_register` tell whether a `let` sits directly
    // in the top-level function's outermost scope.
    pub(crate) scope_depth: u32,
    // Pushed by `while`/`for`/`loop` compilation before compiling their
    // body, popped after; read by `break`/`continue` handling.
    pub(crate) loops: Vec<LoopCtx>,
    pub(crate) slot_shifts: Vec<SlotShift>,
    pub(crate) declared_slots: Vec<Option<u32>>,
    pub(crate) stack_depth: i32,
}

/// Mirrors `ember_resolve::resolver::NATIVE_GLOBALS` (private to that crate,
/// so its length can't be imported directly). The resolver's
/// `seed_native_globals` declares all 8 native names into the top-level
/// function's outermost scope *before* any user code is resolved, via the
/// same `FunctionCtx::declare` call — and therefore the same per-function
/// monotonic slot counter — that ordinary `let`s use. So the first
/// user-declared top-level local isn't slot 0; it's slot 8. `local_count`
/// must start there too, or `compile_block`'s scope-exit `OP_SET_LOCAL`
/// target (written as a raw slot number, not translated through
/// `physical_slot`) would point at whatever physical stack slot `local_count`
/// happened to reach, not the block's real first local.
const NATIVE_GLOBAL_COUNT: u32 = 8;

impl FunctionCompiler {
    pub(crate) fn new(function_id: FunctionId) -> Self {
        FunctionCompiler {
            function_id,
            chunk: Chunk::new(),
            local_count: if function_id == FunctionId::TopLevel {
                NATIVE_GLOBAL_COUNT
            } else {
                0
            },
            scope_depth: 0,
            loops: Vec::new(),
            slot_shifts: Vec::new(),
            declared_slots: Vec::new(),
            stack_depth: 0,
        }
    }

    /// Translates a resolver-assigned slot number into the real physical
    /// stack position, applying every active `for`-loop shift whose `base`
    /// is at or below `resolver_slot`. Nested `for` loops compound
    /// correctly: each shift only ever applies to slots at or after its
    /// own `base`, and `base`s strictly increase with nesting depth.
    pub(crate) fn physical_slot(&self, resolver_slot: u32) -> u32 {
        resolver_slot
            + self
                .slot_shifts
                .iter()
                .filter(|s| resolver_slot >= s.base)
                .map(|s| s.extra)
                .sum::<u32>()
    }

    /// Sum of every active `for`-loop shift's `extra` — used to recover
    /// the resolver's original, unshifted slot number for a local being
    /// declared *right now* (its physical push position minus however much
    /// shifting is currently in effect).
    pub(crate) fn total_shift(&self) -> u32 {
        self.slot_shifts.iter().map(|s| s.extra).sum()
    }

    /// Emits a fixed-effect opcode and updates the tracked depth. Panics
    /// (via `expect`) if called with an operand-dependent op — those go
    /// through `chunk.write_op` directly plus a manual `adjust_depth` call,
    /// so a wrong use here is a compiler bug caught immediately in debug
    /// builds.
    pub(crate) fn emit_op(&mut self, op: Op, line: u32) {
        self.chunk.write_op(op, line);
        let effect = static_stack_effect(op)
            .unwrap_or_else(|| panic!("{op:?} has an operand-dependent stack effect; use chunk.write_op + adjust_depth directly"));
        self.stack_depth += effect;
    }

    /// For operand-dependent ops (`Call`, `MakeList`, etc.): write the op
    /// and operand bytes via `self.chunk` directly, then call this with the
    /// real net effect for that specific call site.
    pub(crate) fn adjust_depth(&mut self, delta: i32) {
        self.stack_depth += delta;
    }

    /// Writes `op` followed by a 2-byte `0xFFFF` placeholder; returns the
    /// placeholder's start offset for `patch_jump` to backfill later.
    pub(crate) fn emit_jump(&mut self, op: Op, line: u32) -> usize {
        self.emit_op(op, line);
        let operand_start = self.chunk.code.len();
        self.chunk.write_u16(0xFFFF, line);
        operand_start
    }

    /// Backfills the placeholder at `operand_start` (as returned by
    /// `emit_jump`) with the real forward-jump distance to the current end
    /// of the chunk. Panics if the distance doesn't fit in `u16`.
    pub(crate) fn patch_jump(&mut self, operand_start: usize) {
        let target = self.chunk.code.len();
        let jump = target - operand_start - 2;
        let jump: u16 = jump.try_into().expect("jump distance exceeds u16::MAX");
        self.chunk.code[operand_start] = (jump >> 8) as u8;
        self.chunk.code[operand_start + 1] = jump as u8;
    }

    /// Emits a backward `OP_LOOP` from the current position to `loop_start`.
    pub(crate) fn emit_loop(&mut self, loop_start: usize, line: u32) {
        self.emit_op(Op::Loop, line);
        let operand_start = self.chunk.code.len();
        let after_operand = operand_start + 2;
        let offset: u16 = (after_operand - loop_start)
            .try_into()
            .expect("loop body exceeds u16::MAX bytes");
        self.chunk.write_u16(offset, line);
    }
}

/// The compiler's full state: the resolver's output (read-only — every
/// slot/upvalue/global decision was already made by the resolver) plus a
/// stack of function-compilers-in-progress, innermost (currently being
/// compiled) last. `interner` is `&mut`, not `&`, even though the compiler
/// mostly only *reads* names: a future `for`-loop desugaring task needs to
/// look up the pre-interned `len` native's `Symbol` to synthesize a length
/// check, and `Interner` (`crates/ember-ast/src/interner.rs`) only exposes
/// `intern(&mut self, &str) -> Symbol` (get-or-intern) — there's no
/// read-only "look up an existing symbol" method. Since `seed_native_globals`
/// (`ember-resolve`) unconditionally interns all 8 native names during
/// resolution, calling `interner.intern("len")` later is guaranteed to
/// return the already-existing symbol, never allocate a new one — but it
/// still needs `&mut` to make that call at all.
///
/// There is deliberately no public `compile()` free function yet, and
/// `Compiler` has no statement-walking methods yet either — `Stmt::Let`,
/// `Expr::Block`, `Expr::If`, and everything after arrive in future tasks.
/// This task adds `new` plus `compile_expr` for the AST shapes that need no
/// scope-shape changes: literals, `Var`, and arithmetic/comparison/logical
/// `Unary`/`Binary`.
///
/// `compile_expr` and its helpers below are exercised only by this file's
/// own `#[cfg(test)]` module for now: nothing outside tests calls into a
/// `Compiler` yet, since the public `compile()` entry point that will
/// (Task 15) doesn't exist until statement compilation (`Stmt::Let`,
/// `Expr::Block`, `Expr::If`, ...) is far enough along to drive it. A plain
/// `cargo build`/`cargo clippy --lib` (which excludes `#[cfg(test)]`) sees
/// every `pub(crate)`/private item here as unreachable from `new` alone,
/// which is genuinely true today, not a mistake — hence the scoped
pub struct Compiler<'a> {
    pub(crate) ast: &'a ember_ast::Ast,
    pub(crate) interner: &'a mut ember_ast::Interner,
    pub(crate) bindings: &'a ember_resolve::Bindings,
    pub(crate) functions: Vec<FunctionCompiler>,
}

impl<'a> Compiler<'a> {
    pub fn new(ast: &'a Ast, interner: &'a mut Interner, bindings: &'a Bindings) -> Self {
        Compiler {
            ast,
            interner,
            bindings,
            functions: vec![FunctionCompiler::new(FunctionId::TopLevel)],
        }
    }

    fn current(&mut self) -> &mut FunctionCompiler {
        self.functions
            .last_mut()
            .expect("at least one FunctionCompiler is always on the stack")
    }

    /// Pools `sym`'s resolved string as a `Value::Str` constant — used for
    /// global names, field names, and (later tasks) variant/type names, so
    /// the disassembler and the future VM see a real name, not a bare index.
    fn name_constant(&mut self, sym: Symbol) -> u16 {
        let text = self.interner.resolve(sym).to_string();
        self.current().chunk.add_constant(Value::Str(Rc::new(text)))
    }

    pub(crate) fn compile_expr(&mut self, idx: Idx<Expr>) {
        let line = self.ast.span_of_expr(idx).start; // placeholder line numbering: byte offset stands in for a line number until a line table exists upstream
        match self.ast.expr(idx).clone() {
            Expr::Int(n) => {
                let c = self.current().chunk.add_constant(Value::Int(n));
                self.emit_constant(c, line);
            }
            Expr::Float(n) => {
                let c = self.current().chunk.add_constant(Value::Float(n));
                self.emit_constant(c, line);
            }
            Expr::Str(sym) => {
                let text = self.interner.resolve(sym).to_string();
                let c = self.current().chunk.add_constant(Value::Str(Rc::new(text)));
                self.emit_constant(c, line);
            }
            Expr::Bool(true) => self.current().emit_op(Op::True, line),
            Expr::Bool(false) => self.current().emit_op(Op::False, line),
            Expr::Nil => self.current().emit_op(Op::Nil, line),
            Expr::Var(_) => self.compile_var(idx, line),
            Expr::Unary { op, operand } => self.compile_unary(op, operand, line),
            Expr::Binary { op, lhs, rhs } => self.compile_binary(op, lhs, rhs, line),
            Expr::If { cond, then_, else_ } => {
                self.compile_expr(cond);
                let to_else = self.current().emit_jump(Op::JumpIfFalse, line);
                // `stack_depth` is a straight-line bookkeeping counter: it
                // has no notion that `then_` and `else_` are alternatives,
                // not a sequence, so it would otherwise sum both branches'
                // pushes as if they both ran. Snapshot the depth here (right
                // where both branches start from, post-condition) and
                // restore it before compiling the `else_`/`Nil` branch, so
                // `stack_depth` after the whole `If` reflects reality:
                // exactly one branch's value landed on the stack.
                let branch_entry_depth = self.current().stack_depth;
                self.compile_expr(then_);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_else);
                self.current().stack_depth = branch_entry_depth;
                match else_ {
                    Some(e) => self.compile_expr(e),
                    None => self.current().emit_op(Op::Nil, line),
                }
                self.current().patch_jump(to_end);
            }
            Expr::Block { stmts, tail } => self.compile_block(&stmts, tail, line),
            Expr::Assign { target, value } => self.compile_assign(target, value, line),
            Expr::List { items } => self.compile_list(items, line),
            Expr::Index { base, index } => {
                self.compile_expr(base);
                self.compile_expr(index);
                self.current().emit_op(Op::GetIndex, line);
            }
            Expr::Field { base, name } => {
                self.compile_expr(base);
                let c = self.name_constant(name);
                self.current().emit_op(Op::GetField, line);
                self.current().chunk.write_u16(c, line);
            }
            Expr::Struct { name, fields } => self.compile_struct(name, fields, line),
            Expr::Lambda { params, body } => {
                let anon_name = self.interner.intern("<lambda>");
                self.compile_function(FunctionId::Lambda(idx), &params, body, anon_name, line);
            }
            Expr::Call { callee, args } => {
                let argc = args.len();
                self.compile_expr(callee);
                for a in args {
                    self.compile_expr(a);
                }
                self.current().chunk.write_op(Op::Call, line);
                self.current().chunk.write_u8(argc as u8, line);
                self.current().adjust_depth(-(argc as i32)); // pops callee+argc args, pushes 1 result: net -argc
            }
            Expr::Match { scrutinee, arms } => self.compile_match(scrutinee, arms, line),
            Expr::Error => panic!("cannot compile an Expr::Error node — the pipeline must reject programs with parse/resolve errors before compilation"),
        }
    }

    fn emit_constant(&mut self, const_idx: u16, line: u32) {
        self.current().emit_op(Op::Constant, line);
        self.current().chunk.write_u16(const_idx, line);
    }

    fn compile_var(&mut self, idx: Idx<Expr>, line: u32) {
        match self.bindings.resolutions.get(&idx) {
            Some(Resolution::Local { slot }) => {
                let physical = self.current().physical_slot(*slot);
                self.current().emit_op(Op::GetLocal, line);
                self.current().chunk.write_u8(physical as u8, line);
            }
            Some(Resolution::Upvalue { index }) => {
                self.current().emit_op(Op::GetUpvalue, line);
                self.current().chunk.write_u8(*index as u8, line);
            }
            Some(Resolution::Global { symbol }) => {
                let c = self.name_constant(*symbol);
                self.current().emit_op(Op::GetGlobal, line);
                self.current().chunk.write_u16(c, line);
            }
            None => panic!("Var node at {idx:?} has no recorded Resolution — the resolver must run (and succeed) before compilation"),
        }
    }

    fn compile_unary(&mut self, op: TokenKind, operand: Idx<Expr>, line: u32) {
        self.compile_expr(operand);
        match op {
            TokenKind::Bang => self.current().emit_op(Op::Not, line),
            TokenKind::Minus => self.current().emit_op(Op::Negate, line),
            other => panic!("unsupported unary operator token: {other:?}"),
        }
    }

    fn compile_binary(&mut self, op: TokenKind, lhs: Idx<Expr>, rhs: Idx<Expr>, line: u32) {
        match op {
            TokenKind::AndAnd => {
                self.compile_expr(lhs);
                let to_false = self.current().emit_jump(Op::JumpIfFalse, line);
                // Same straight-line-vs-alternative-branches reconciliation
                // as `Expr::If` below: `rhs` and the `False` fallback are
                // alternatives (exactly one runs), not a sequence, so
                // `stack_depth` must be reset between them.
                let branch_entry_depth = self.current().stack_depth;
                self.compile_expr(rhs);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_false);
                self.current().stack_depth = branch_entry_depth;
                self.current().emit_op(Op::False, line);
                self.current().patch_jump(to_end);
                return;
            }
            TokenKind::OrOr => {
                self.compile_expr(lhs);
                let to_true = self.current().emit_jump(Op::JumpIfTrue, line);
                let branch_entry_depth = self.current().stack_depth;
                self.compile_expr(rhs);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_true);
                self.current().stack_depth = branch_entry_depth;
                self.current().emit_op(Op::True, line);
                self.current().patch_jump(to_end);
                return;
            }
            _ => {}
        }
        self.compile_expr(lhs);
        self.compile_expr(rhs);
        match op {
            TokenKind::Plus => self.current().emit_op(Op::Add, line),
            TokenKind::Minus => self.current().emit_op(Op::Sub, line),
            TokenKind::Star => self.current().emit_op(Op::Mul, line),
            TokenKind::Slash => self.current().emit_op(Op::Div, line),
            TokenKind::Percent => self.current().emit_op(Op::Mod, line),
            TokenKind::EqEq => self.current().emit_op(Op::Equal, line),
            TokenKind::BangEq => {
                self.current().emit_op(Op::Equal, line);
                self.current().emit_op(Op::Not, line);
            }
            TokenKind::Lt => self.current().emit_op(Op::Less, line),
            TokenKind::Gt => self.current().emit_op(Op::Greater, line),
            TokenKind::LtEq => {
                self.current().emit_op(Op::Greater, line);
                self.current().emit_op(Op::Not, line);
            }
            TokenKind::GtEq => {
                self.current().emit_op(Op::Less, line);
                self.current().emit_op(Op::Not, line);
            }
            other => panic!("unsupported binary operator token: {other:?}"),
        }
    }

    /// Compiles a block's statements, then its tail (or `Nil` if absent),
    /// then — if the block declared any locals — relocates the tail value
    /// down past them and pops the rest. See the module-level explanation
    /// in the task plan for why `OP_SET_LOCAL` (write without popping) is
    /// the right primitive here: the tail is computed on top of the `n`
    /// locals the block declared; `OP_SET_LOCAL` overwrites the first of
    /// those `n` slots with the tail value (that local is dying anyway),
    /// and the `n` plain `OP_POP`s that follow remove the other `n - 1`
    /// dead locals plus the now-duplicate tail value on top, leaving
    /// exactly the tail sitting where the first local used to be.
    fn compile_block(
        &mut self,
        stmts: &[Idx<ember_ast::Stmt>],
        tail: Option<Idx<Expr>>,
        line: u32,
    ) {
        let entry_local_count = self.current().local_count;
        self.current().scope_depth += 1;
        for &s in stmts {
            self.compile_stmt(s);
        }
        match tail {
            Some(t) => self.compile_expr(t),
            None => self.current().emit_op(Op::Nil, line),
        }
        self.emit_tail_scope_exit(entry_local_count, line);
        self.current().scope_depth -= 1;
    }

    /// Given that `declared_slots.len() - entry_local_count`'s worth of
    /// locals were pushed since `entry_local_count` and a single "tail"
    /// value now sits on top of all of them, tears the locals down
    /// (substituting `OP_CLOSE_UPVALUE` for `OP_POP` per `slot_is_captured`,
    /// with every captured local in range pre-closed in place first) while
    /// leaving exactly the tail value at `entry_local_count`'s physical
    /// stack position. Shared by `compile_block`, and (this task) by every
    /// match arm's own bindings plus the whole match's hidden scrutinee
    /// local.
    ///
    /// The upfront `OP_CLOSE_UPVALUES_FROM entry_local_count` pre-close
    /// step exists only for `entry_local_count` itself (the first-declared
    /// local of this scope): its slot is the one that *survives* this
    /// teardown — `OP_SET_LOCAL` below overwrites it in place with the
    /// tail value rather than popping it — so if it's captured, its
    /// upvalue must be closed, with its real value, before that overwrite.
    /// `OP_CLOSE_UPVALUE` can't do this: it's zero-operand and always
    /// targets whatever's physically on top of the stack, which is never
    /// this slot (the tail value, and every other local declared after
    /// it, still sit above it here) — no reordering of `OP_GET_LOCAL` +
    /// `OP_CLOSE_UPVALUE` fixes that, since duplicating the value onto a
    /// new top slot doesn't move the *already-recorded* open upvalue,
    /// which still points at the original slot. `OP_CLOSE_UPVALUES_FROM`
    /// closes in place, by slot, without touching the stack, sidestepping
    /// the problem entirely. Emitting it with a threshold of
    /// `entry_local_count` also harmlessly closes any *other* local in
    /// this same range that happens to be captured too — safe, since none
    /// of their slots are touched before the loop below removes them, and
    /// idempotent, since the loop's own `OP_CLOSE_UPVALUE`/`OP_POP` choice
    /// per slot doesn't care whether the upvalue was already closed here.
    fn emit_tail_scope_exit(&mut self, entry_local_count: u32, line: u32) {
        let declared = (self.current().local_count - entry_local_count) as usize;
        if declared == 0 {
            return;
        }
        let base_idx = self.current().declared_slots.len() - declared;
        let any_captured = (base_idx..base_idx + declared).any(|i| {
            self.current().declared_slots[i]
                .map(|s| self.slot_is_captured(s))
                .unwrap_or(false)
        });
        if any_captured {
            self.current().emit_op(Op::CloseUpvaluesFrom, line);
            self.current().chunk.write_u8(entry_local_count as u8, line);
        }
        self.current().emit_op(Op::SetLocal, line);
        self.current().chunk.write_u8(entry_local_count as u8, line);
        self.current().emit_op(Op::Pop, line); // SetLocal's leftover tail-duplicate — never a real local
        for i in (1..declared).rev() {
            let slot = self.current().declared_slots[base_idx + i];
            let captured = slot.map(|s| self.slot_is_captured(s)).unwrap_or(false);
            self.current()
                .emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
        }
        self.current().declared_slots.truncate(base_idx);
        self.current().local_count = entry_local_count;
    }

    fn slot_is_captured(&self, resolver_slot: u32) -> bool {
        let function_id = self
            .functions
            .last()
            .expect("at least one FunctionCompiler")
            .function_id;
        self.bindings
            .captured_slots
            .get(&function_id)
            .map(|v| v.contains(&resolver_slot))
            .unwrap_or(false)
    }

    /// Pushes bookkeeping for one new local: `resolver_slot` is `Some(n)`
    /// for anything the resolver itself declared (so `slot_is_captured`
    /// can find it later), `None` for compiler-only hidden locals (`for`'s
    /// iterable/counter) that no `Var` node could ever reference and so
    /// can never be captured.
    fn push_local(&mut self, resolver_slot: Option<u32>) {
        self.current().local_count += 1;
        self.current().declared_slots.push(resolver_slot);
    }

    /// Pops `count` locals with `OP_CLOSE_UPVALUE` substituted for `OP_POP`
    /// wherever `slot_is_captured`. Used everywhere a scope-exit has no
    /// tail value to protect (loop cleanup, `break`/`continue` unwinding)
    /// — `compile_block` doesn't use this, since its tail value needs the
    /// more careful treatment above.
    fn emit_scope_pops(&mut self, count: u32, line: u32) {
        for _ in 0..count {
            let resolver_slot = self
                .current()
                .declared_slots
                .pop()
                .expect("emit_scope_pops: declared_slots underflow");
            self.current().local_count -= 1;
            let captured = resolver_slot
                .map(|s| self.slot_is_captured(s))
                .unwrap_or(false);
            self.current()
                .emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
        }
    }

    /// Shared by `Stmt::Let` and `Stmt::Fn`: declares one new named local
    /// (recovering its resolver slot number as `local_count - total_shift`,
    /// valid because nothing hidden has been pushed since the resolver's
    /// own counter last matched this compiler's), then applies top-level
    /// dual registration if applicable.
    fn declare_named_local(&mut self, name: Symbol, line: u32) {
        let resolver_slot = self.current().local_count - self.current().total_shift();
        self.push_local(Some(resolver_slot));
        self.maybe_dual_register(name, line);
    }

    pub(crate) fn compile_stmt(&mut self, idx: Idx<ember_ast::Stmt>) {
        let entry_depth = self.current().stack_depth;
        // How many permanent locals this statement leaves behind, i.e. how
        // much `stack_depth` should have grown by once it's done. `Let`,
        // `StructDecl`, and `Fn` each declare exactly one. `TypeDecl` is the
        // odd one out: it declares its own (unread) name PLUS one local per
        // variant constructor — `1 + variants.len()`, not a fixed `1`.
        // Everything else nets to zero.
        let permanent_locals: i32 = match self.ast.stmt(idx) {
            ember_ast::Stmt::Let { .. }
            | ember_ast::Stmt::StructDecl { .. }
            | ember_ast::Stmt::Fn { .. } => 1,
            ember_ast::Stmt::TypeDecl { variants, .. } => 1 + variants.len() as i32,
            _ => 0,
        };
        let line = self.ast.span_of_stmt(idx).start;
        match self.ast.stmt(idx).clone() {
            ember_ast::Stmt::Let { name, init, .. } => {
                self.compile_expr(init);
                self.declare_named_local(name, line);
            }
            ember_ast::Stmt::StructDecl { name, .. } => {
                // The struct's own name consumes a resolver slot (hoisted
                // alongside `fn`/ADT names) but is never read via any Var
                // node — `Expr::Struct` is its own AST node, constructed
                // directly at each literal site, not by calling a value
                // bound to the struct's name. A Nil placeholder just
                // reserves the slot to keep local_count aligned with the
                // resolver's. Routed through `declare_named_local` (rather
                // than the raw `local_count += 1` this arm used before this
                // task): `compile_block`'s scope-exit now indexes
                // `declared_slots` in lockstep with `local_count` for EVERY
                // local, hidden or not — a struct declared inside a nested
                // block (legal: `struct` is an ordinary keyword-led `stmt`,
                // parseable anywhere a block accepts one) would otherwise
                // desync the two counters and panic with a subtraction
                // underflow the next time that block computes its own
                // `declared`/`base_idx`.
                self.current().emit_op(Op::Nil, line);
                self.declare_named_local(name, line);
            }
            ember_ast::Stmt::ExprStmt(e) => {
                self.compile_expr(e);
                self.current().emit_op(Op::Pop, line);
            }
            ember_ast::Stmt::While { cond, body } => self.compile_while(cond, body, line),
            ember_ast::Stmt::Loop { body } => self.compile_loop(body, line),
            ember_ast::Stmt::For {
                binding,
                iter,
                body,
            } => self.compile_for(binding, iter, body, line),
            ember_ast::Stmt::Break => self.compile_break(line),
            ember_ast::Stmt::Continue => self.compile_continue(line),
            ember_ast::Stmt::Fn {
                name, params, body, ..
            } => {
                self.compile_function(FunctionId::Fn(idx), &params, body, name, line);
                self.declare_named_local(name, line);
            }
            ember_ast::Stmt::TypeDecl { name, variants } => {
                self.compile_type_decl(name, variants, line)
            }
            other => unimplemented!("compile_stmt: {other:?} — added in a later task"),
        }
        // CHECKLIST.md's required per-statement stack-balance assertion.
        // Every statement kind but `Let`/`StructDecl`/`Fn`/`TypeDecl` must
        // return to its entry depth — `Let`'s pushed initializer,
        // `StructDecl`'s Nil placeholder for its hoisted name slot, `Fn`'s
        // `OP_CLOSURE` result, and `TypeDecl`'s own Nil placeholder plus one
        // `OP_MAKE_ADT`/`OP_CLOSURE` result per variant constructor all
        // deliberately stay, permanently occupying their new locals' slots
        // — `permanent_locals` (computed above, before the match, from the
        // statement's own shape) is exactly how many. This wrapping
        // structure (entry snapshot before the match, assert after) stays
        // correct as later tasks add more arms to the match above — no
        // per-arm assertion needed at each addition. `break`/`continue`
        // also satisfy this: they restore
        // `stack_depth` to their own entry depth right after emitting
        // their (real) cleanup pops and jump — see the comment in
        // `compile_break`.
        debug_assert_eq!(
            self.current().stack_depth,
            entry_depth + permanent_locals,
            "stack imbalance compiling statement {idx:?}"
        );
    }

    /// If we're directly at the top level's outermost scope, duplicates the
    /// just-declared local (which stays on the stack as its permanent
    /// storage) and also stores it into the runtime globals table under
    /// `name`, so nested functions — which see this binding only via
    /// `Resolution::Global`, never `Upvalue` — can read/write it too.
    fn maybe_dual_register(&mut self, name: Symbol, line: u32) {
        if self.current().function_id != FunctionId::TopLevel || self.current().scope_depth != 0 {
            return;
        }
        let slot = self.current().local_count - 1;
        self.current().emit_op(Op::GetLocal, line);
        self.current().chunk.write_u8(slot as u8, line);
        let c = self.name_constant(name);
        self.current().emit_op(Op::DefineGlobal, line);
        self.current().chunk.write_u16(c, line);
    }

    /// Registers an ADT type's constructors as named locals — one per
    /// variant, plus the type's own (never-read-via-`Var`) name, mirroring
    /// `Stmt::StructDecl`'s Nil-placeholder treatment above. A nullary
    /// variant (`Origin`) compiles straight to `OP_MAKE_ADT`, no closure
    /// needed — constructing it takes no arguments. A variant with a
    /// payload (`Circle(Float)`) compiles to a tiny synthetic function
    /// (built directly as a standalone `Chunk`, not through
    /// `compile_function`/`FunctionCompiler`, since it needs none of that
    /// machinery: no locals beyond its own params, no upvalues, no nested
    /// scopes) that reads back its `arity` params and wraps them into an
    /// `OP_MAKE_ADT` call — so `Circle(1.0)` compiles as an ordinary
    /// `Expr::Call` against this closure, exactly like calling any other
    /// function value.
    fn compile_type_decl(&mut self, name: Symbol, variants: Vec<ember_ast::AdtVariant>, line: u32) {
        // Same reasoning as StructDecl: the type's own name is never read
        // via a Var node, only its variants' constructors are.
        self.current().emit_op(Op::Nil, line);
        self.declare_named_local(name, line);

        for variant in variants {
            if variant.payload.is_empty() {
                let type_c = self.name_constant(name);
                let variant_c = self.name_constant(variant.name);
                self.current().chunk.write_op(Op::MakeAdt, line);
                self.current().chunk.write_u16(type_c, line);
                self.current().chunk.write_u16(variant_c, line);
                self.current().chunk.write_u16(0, line);
                self.current().adjust_depth(1);
                self.declare_named_local(variant.name, line);
            } else {
                let arity = variant.payload.len();
                let mut ctor_chunk = Chunk::new();
                for i in 0..arity {
                    ctor_chunk.write_op(Op::GetLocal, line);
                    ctor_chunk.write_u8(i as u8, line);
                }
                let type_text = self.interner.resolve(name).to_string();
                let type_c = ctor_chunk.add_constant(Value::Str(Rc::new(type_text)));
                let variant_text = self.interner.resolve(variant.name).to_string();
                let variant_c = ctor_chunk.add_constant(Value::Str(Rc::new(variant_text)));
                ctor_chunk.write_op(Op::MakeAdt, line);
                ctor_chunk.write_u16(type_c, line);
                ctor_chunk.write_u16(variant_c, line);
                ctor_chunk.write_u16(arity as u16, line);
                ctor_chunk.write_op(Op::Return, line);
                let proto = ember_bytecode::chunk::FunctionProto {
                    chunk: ctor_chunk,
                    arity,
                    upvalues: Vec::new(),
                    name: variant.name,
                };
                let idx = self.current().chunk.add_function(proto);
                self.current().emit_op(Op::Closure, line);
                self.current().chunk.write_u16(idx, line);
                self.declare_named_local(variant.name, line);
            }
        }
    }

    fn compile_while(&mut self, cond: Idx<Expr>, body: Idx<Expr>, line: u32) {
        let loop_start = self.current().chunk.code.len();
        self.compile_expr(cond);
        let to_end = self.current().emit_jump(Op::JumpIfFalse, line);
        let body_base_local_count = self.current().local_count;
        self.current().loops.push(LoopCtx {
            body_base_local_count,
            continue_jumps: Vec::new(),
            break_jumps: Vec::new(),
        });
        self.compile_expr(body);
        self.current().emit_op(Op::Pop, line); // body is a statement; discard its Block value
        let ctx = self.current().loops.pop().expect("just pushed");
        for j in ctx.continue_jumps {
            self.current().patch_jump(j);
        }
        self.current().emit_loop(loop_start, line);
        self.current().patch_jump(to_end);
        for j in ctx.break_jumps {
            self.current().patch_jump(j);
        }
    }

    fn compile_loop(&mut self, body: Idx<Expr>, line: u32) {
        let loop_start = self.current().chunk.code.len();
        let body_base_local_count = self.current().local_count;
        self.current().loops.push(LoopCtx {
            body_base_local_count,
            continue_jumps: Vec::new(),
            break_jumps: Vec::new(),
        });
        self.compile_expr(body);
        self.current().emit_op(Op::Pop, line);
        let ctx = self.current().loops.pop().expect("just pushed");
        for j in ctx.continue_jumps {
            self.current().patch_jump(j);
        }
        self.current().emit_loop(loop_start, line);
        for j in ctx.break_jumps {
            self.current().patch_jump(j);
        }
    }

    fn compile_for(&mut self, _binding: Symbol, iter: Idx<Expr>, body: Idx<Expr>, line: u32) {
        let base = self.current().local_count; // == the resolver's slot number for `binding`

        self.compile_expr(iter); // hidden local: the iterable, physical slot `base`
        self.push_local(None);
        let zero_c = self.current().chunk.add_constant(Value::Int(0));
        self.emit_constant(zero_c, line); // hidden local: the counter, physical slot `base + 1`
        self.push_local(None);
        self.current().emit_op(Op::Nil, line); // `binding` placeholder, physical slot `base + 2`
        self.push_local(Some(base)); // `binding` — its real resolver slot is `base`
        self.current()
            .slot_shifts
            .push(SlotShift { base, extra: 2 });

        let xs_slot = base;
        let counter_slot = base + 1;
        let binding_slot = base + 2;

        let loop_start = self.current().chunk.code.len();
        self.emit_get_local(counter_slot, line);
        self.emit_len_call(xs_slot, line);
        self.current().emit_op(Op::Less, line); // counter < len(xs)
        let to_end = self.current().emit_jump(Op::JumpIfFalse, line);

        self.emit_get_local(xs_slot, line);
        self.emit_get_local(counter_slot, line);
        self.current().emit_op(Op::GetIndex, line); // xs[counter]
        self.emit_set_local(binding_slot, line);
        self.current().emit_op(Op::Pop, line); // discard SetLocal's duplicate

        let body_base_local_count = self.current().local_count;
        self.current().loops.push(LoopCtx {
            body_base_local_count,
            continue_jumps: Vec::new(),
            break_jumps: Vec::new(),
        });
        self.compile_expr(body);
        self.current().emit_op(Op::Pop, line);
        let ctx = self.current().loops.pop().expect("just pushed");
        for j in ctx.continue_jumps {
            self.current().patch_jump(j);
        }

        // counter = counter + 1
        self.emit_get_local(counter_slot, line);
        let one_c = self.current().chunk.add_constant(Value::Int(1));
        self.emit_constant(one_c, line);
        self.current().emit_op(Op::Add, line);
        self.emit_set_local(counter_slot, line);
        self.current().emit_op(Op::Pop, line);

        self.current().emit_loop(loop_start, line);
        self.current().patch_jump(to_end);
        for j in ctx.break_jumps {
            self.current().patch_jump(j);
        }

        // Tear down the 3 hidden/visible locals this loop introduced. This
        // is the loop's own genuine, permanent scope exit (not an
        // alternate/jumped-away path), so — unlike `emit_loop_cleanup_pops`
        // below — it's correct for this to actually shrink `local_count`/
        // `declared_slots`.
        self.emit_scope_pops(3, line);
        self.current().slot_shifts.pop();
    }

    fn compile_break(&mut self, line: u32) {
        let base = self
            .current()
            .loops
            .last()
            .expect("`break` outside a loop — the resolver must reject this before compilation")
            .body_base_local_count;
        // Whatever bytecode comes next in program order (more statements
        // in this block, then the block's own tail + scope-exit pops, all
        // the way out to the enclosing loop's body) is only reachable when
        // this `break` *isn't* taken — an alternative to the jump-away
        // path, not a continuation of it, even though the compiler emits
        // both into the same linear stream. Same reconciliation `Expr::If`
        // needs between `then_`/`else_`: snapshot `stack_depth` before the
        // cleanup pops (which are real and must stay, for the jump-taken
        // path to be correct) and restore it after, so the "break not
        // taken" continuation's bookkeeping isn't thrown off by pops that,
        // from its perspective, never happened.
        let entry_depth = self.current().stack_depth;
        self.emit_loop_cleanup_pops(base, line);
        let j = self.current().emit_jump(Op::Jump, line);
        self.current().stack_depth = entry_depth;
        self.current()
            .loops
            .last_mut()
            .expect("checked above")
            .break_jumps
            .push(j);
    }

    fn compile_continue(&mut self, line: u32) {
        let base = self
            .current()
            .loops
            .last()
            .expect("`continue` outside a loop — the resolver must reject this before compilation")
            .body_base_local_count;
        // Same stack_depth snapshot/restore as `compile_break`, and for
        // the same reason — see its comment.
        let entry_depth = self.current().stack_depth;
        self.emit_loop_cleanup_pops(base, line);
        let j = self.current().emit_jump(Op::Jump, line);
        self.current().stack_depth = entry_depth;
        self.current()
            .loops
            .last_mut()
            .expect("checked above")
            .continue_jumps
            .push(j);
    }

    /// Emits real cleanup ops (`OP_POP`/`OP_CLOSE_UPVALUE` per live local's
    /// captured-ness) for a `break`/`continue` jumping out of everything
    /// down to `body_base_local_count`. Deliberately does NOT go through
    /// `emit_scope_pops` (which permanently pops `declared_slots` and
    /// decrements `local_count`): a break/continue's cleanup describes an
    /// ALTERNATE control-flow path, exactly like the `stack_depth`
    /// snapshot/restore right below each call site, not this (still-open)
    /// block's own real exit. Mutating `local_count`/`declared_slots` here
    /// would desync them from the enclosing `compile_block` call still in
    /// progress — e.g. `while true { let a = 1; if done { break; } let b =
    /// a + 1; }`: the `break` sits inside a nested block, and permanently
    /// popping `a`'s bookkeeping there would make the outer block's own
    /// eventual `local_count - entry_local_count` computation underflow,
    /// since further code (`let b = ...`) still declares more locals
    /// afterward along the (real, fallthrough) not-taken path. So this
    /// peeks the live slice of `declared_slots` (top of stack first, to
    /// match real LIFO pop order) instead of popping it.
    fn emit_loop_cleanup_pops(&mut self, body_base_local_count: u32, line: u32) {
        let live = (self.current().local_count - body_base_local_count) as usize;
        let len = self.current().declared_slots.len();
        let start = len - live;
        for i in (start..len).rev() {
            let slot = self.current().declared_slots[i];
            let captured = slot.map(|s| self.slot_is_captured(s)).unwrap_or(false);
            self.current()
                .emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
        }
    }

    fn emit_get_local(&mut self, slot: u32, line: u32) {
        self.current().emit_op(Op::GetLocal, line);
        self.current().chunk.write_u8(slot as u8, line);
    }

    fn emit_set_local(&mut self, slot: u32, line: u32) {
        self.current().emit_op(Op::SetLocal, line);
        self.current().chunk.write_u8(slot as u8, line);
    }

    /// Emits `len(<value already at `arg_slot`>)` — pushes the `len`
    /// native (a `Resolution::Global`, always seeded by the resolver),
    /// pushes one argument by reading it back from its local slot, and
    /// calls with `argc = 1`. Narrow and `for`-loop-specific: a future
    /// task's general `Expr::Call` compiler does not call this, since it
    /// compiles an arbitrary AST `callee`/`args` list rather than a fixed
    /// native.
    fn emit_len_call(&mut self, arg_slot: u32, line: u32) {
        let len_sym = self.interner.intern("len");
        let len_const = self.name_constant(len_sym);
        self.current().emit_op(Op::GetGlobal, line);
        self.current().chunk.write_u16(len_const, line);
        self.emit_get_local(arg_slot, line);
        self.current().chunk.write_op(Op::Call, line);
        self.current().chunk.write_u8(1, line); // argc = 1
        self.current().adjust_depth(-1); // pops callee + 1 arg (2), pushes 1 result: net -1
    }

    /// `ember-resolve`'s `resolve_assign_target` only records a `Resolution`
    /// for a bare `Var` target — `Index`/`Field` targets instead resolve
    /// their `base` (and, for `Index`, `index`) as ordinary sub-expressions,
    /// leaving the target node itself unresolved. So a `Var` target mirrors
    /// `compile_var`'s three-way `Local`/`Upvalue`/`Global` dispatch, while
    /// `Index`/`Field` targets just compile `base`/`index`/`value` in order
    /// and let `OP_SET_INDEX`/`OP_SET_FIELD` do the write.
    fn compile_assign(&mut self, target: Idx<Expr>, value: Idx<Expr>, line: u32) {
        match self.ast.expr(target).clone() {
            Expr::Var(_) => {
                self.compile_expr(value);
                match self.bindings.resolutions.get(&target) {
                    Some(Resolution::Local { slot }) => {
                        let physical = self.current().physical_slot(*slot);
                        self.emit_set_local(physical, line);
                    }
                    Some(Resolution::Upvalue { index }) => {
                        self.current().emit_op(Op::SetUpvalue, line);
                        self.current().chunk.write_u8(*index as u8, line);
                    }
                    Some(Resolution::Global { symbol }) => {
                        let c = self.name_constant(*symbol);
                        self.current().emit_op(Op::SetGlobal, line);
                        self.current().chunk.write_u16(c, line);
                    }
                    None => panic!("Assign target Var at {target:?} has no recorded Resolution"),
                }
            }
            Expr::Index { base, index } => {
                self.compile_expr(base);
                self.compile_expr(index);
                self.compile_expr(value);
                self.current().emit_op(Op::SetIndex, line);
            }
            Expr::Field { base, name } => {
                self.compile_expr(base);
                self.compile_expr(value);
                let c = self.name_constant(name);
                self.current().emit_op(Op::SetField, line);
                self.current().chunk.write_u16(c, line);
            }
            other => panic!("invalid assignment target: {other:?} — the resolver must reject this before compilation"),
        }
    }

    fn compile_list(&mut self, items: Vec<Idx<Expr>>, line: u32) {
        let count = items.len();
        for item in items {
            self.compile_expr(item);
        }
        self.current().chunk.write_op(Op::MakeList, line);
        self.current().chunk.write_u16(count as u16, line);
        self.current().adjust_depth(1 - count as i32);
    }

    fn compile_struct(&mut self, name: Symbol, fields: Vec<(Symbol, Idx<Expr>)>, line: u32) {
        let field_count = fields.len();
        for (fname, fexpr) in fields {
            let fname_const = self.name_constant(fname);
            self.emit_constant(fname_const, line);
            self.compile_expr(fexpr);
        }
        let type_name_const = self.name_constant(name);
        self.current().chunk.write_op(Op::MakeRecord, line);
        self.current().chunk.write_u16(type_name_const, line);
        self.current().chunk.write_u16(field_count as u16, line);
        self.current().adjust_depth(1 - 2 * field_count as i32);
    }

    /// Compiles a function-introducing node (`Expr::Lambda` or `Stmt::Fn`)
    /// into its own `FunctionProto`, emitting `OP_CLOSURE` (plus its
    /// resolved upvalue list) into the ENCLOSING chunk. `function_id` keys
    /// `self.bindings.upvalues`/`captured_slots` — it's how the resolver's
    /// per-function output gets matched back up with the AST node currently
    /// being compiled.
    fn compile_function(
        &mut self,
        function_id: FunctionId,
        params: &[ember_ast::Param],
        body: Idx<Expr>,
        name: Symbol,
        line: u32,
    ) {
        // Translate every `is_local: true` upvalue's raw resolver slot
        // number into a real physical stack position, evaluated in the
        // ENCLOSING frame (before pushing the new one) — a capture from
        // inside a `for`-loop's shifted region needs this. `is_local: false`
        // entries (chained capture from the enclosing closure's own upvalue
        // array) are a plain `Vec` index, never a stack slot, so they need
        // no translation.
        let raw_upvalues = self
            .bindings
            .upvalues
            .get(&function_id)
            .cloned()
            .unwrap_or_default();
        let upvalues: Vec<ember_resolve::UpvalueDesc> = raw_upvalues
            .into_iter()
            .map(|uv| {
                if uv.is_local {
                    ember_resolve::UpvalueDesc {
                        index: self.current().physical_slot(uv.index),
                        is_local: true,
                    }
                } else {
                    uv
                }
            })
            .collect();

        self.functions.push(FunctionCompiler::new(function_id));
        for i in 0..params.len() {
            self.push_local(Some(i as u32));
        }
        self.compile_expr(body);
        self.current().chunk.write_op(Op::Return, line);
        self.current().adjust_depth(-1);

        let fc = self.functions.pop().expect("just pushed");
        debug_assert_eq!(
            fc.stack_depth, 0,
            "a function body must leave exactly its one return value, which OP_RETURN then consumes"
        );

        let proto = ember_bytecode::chunk::FunctionProto {
            chunk: fc.chunk,
            arity: params.len(),
            upvalues,
            name,
        };
        let const_idx = self.current().chunk.add_function(proto);
        self.current().emit_op(Op::Closure, line);
        self.current().chunk.write_u16(const_idx, line);
    }

    fn pattern_needs_value(&self, pat: Idx<ember_ast::Pattern>) -> bool {
        !matches!(
            self.ast.pat(pat),
            Pattern::Wild | Pattern::Bind(_) | Pattern::Error
        )
    }

    /// Pushes exactly one `Bool`. No bindings, no other side effects.
    fn compile_pattern_test(
        &mut self,
        pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        line: u32,
    ) {
        match self.ast.pat(pat).clone() {
            Pattern::Wild | Pattern::Bind(_) | Pattern::Error => {
                self.current().emit_op(Op::True, line);
            }
            Pattern::Int(n) => {
                self.emit_get_local(scrutinee_slot, line);
                let c = self.current().chunk.add_constant(Value::Int(n));
                self.emit_constant(c, line);
                self.current().emit_op(Op::Equal, line);
            }
            Pattern::Float(f) => {
                self.emit_get_local(scrutinee_slot, line);
                let c = self.current().chunk.add_constant(Value::Float(f));
                self.emit_constant(c, line);
                self.current().emit_op(Op::Equal, line);
            }
            Pattern::Bool(b) => {
                self.emit_get_local(scrutinee_slot, line);
                let c = self.current().chunk.add_constant(Value::Bool(b));
                self.emit_constant(c, line);
                self.current().emit_op(Op::Equal, line);
            }
            Pattern::Str(sym) => {
                self.emit_get_local(scrutinee_slot, line);
                let text = self.interner.resolve(sym).to_string();
                let c = self.current().chunk.add_constant(Value::Str(Rc::new(text)));
                self.emit_constant(c, line);
                self.current().emit_op(Op::Equal, line);
            }
            Pattern::Ctor { name, args } => {
                self.emit_get_local(scrutinee_slot, line);
                let variant_c = self.name_constant(name);
                self.current().emit_op(Op::TestVariant, line);
                self.current().chunk.write_u16(variant_c, line);
                let mut fail_jumps = vec![self.current().emit_jump(Op::JumpIfFalse, line)];
                for (i, &arg_pat) in args.iter().enumerate() {
                    if !self.pattern_needs_value(arg_pat) {
                        continue;
                    }
                    self.emit_get_local(scrutinee_slot, line);
                    self.current().chunk.write_op(Op::Destructure, line);
                    self.current().chunk.write_u8(i as u8, line);
                    self.current().adjust_depth(0);
                    self.push_local(None);
                    let temp_slot = self.current().local_count - 1;
                    self.compile_pattern_test(arg_pat, temp_slot, line);
                    self.emit_tail_scope_exit(temp_slot, line);
                    fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
                }
                self.finish_and_chain(fail_jumps, line);
            }
            Pattern::Record { name, fields } => {
                self.emit_get_local(scrutinee_slot, line);
                let name_c = self.name_constant(name);
                self.current().emit_op(Op::TestVariant, line);
                self.current().chunk.write_u16(name_c, line);
                let mut fail_jumps = vec![self.current().emit_jump(Op::JumpIfFalse, line)];
                for (fname, fpat) in fields {
                    if !self.pattern_needs_value(fpat) {
                        continue;
                    }
                    self.emit_get_local(scrutinee_slot, line);
                    let fname_c = self.name_constant(fname);
                    self.current().emit_op(Op::GetField, line);
                    self.current().chunk.write_u16(fname_c, line);
                    self.push_local(None);
                    let temp_slot = self.current().local_count - 1;
                    self.compile_pattern_test(fpat, temp_slot, line);
                    self.emit_tail_scope_exit(temp_slot, line);
                    fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
                }
                self.finish_and_chain(fail_jumps, line);
            }
            Pattern::List { items, rest } => {
                self.emit_len_call(scrutinee_slot, line);
                let n_c = self.current().chunk.add_constant(Value::Int(items.len() as i64));
                self.emit_constant(n_c, line);
                if rest.is_some() {
                    self.current().emit_op(Op::Less, line);
                    self.current().emit_op(Op::Not, line); // len >= items.len()
                } else {
                    self.current().emit_op(Op::Equal, line);
                }
                let mut fail_jumps = vec![self.current().emit_jump(Op::JumpIfFalse, line)];
                for (i, &item_pat) in items.iter().enumerate() {
                    if !self.pattern_needs_value(item_pat) {
                        continue;
                    }
                    self.emit_get_local(scrutinee_slot, line);
                    let idx_c = self.current().chunk.add_constant(Value::Int(i as i64));
                    self.emit_constant(idx_c, line);
                    self.current().emit_op(Op::GetIndex, line);
                    self.push_local(None);
                    let temp_slot = self.current().local_count - 1;
                    self.compile_pattern_test(item_pat, temp_slot, line);
                    self.emit_tail_scope_exit(temp_slot, line);
                    fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
                }
                self.finish_and_chain(fail_jumps, line);
            }
            Pattern::Tuple(_) => {
                self.current().emit_op(Op::False, line);
            }
            Pattern::Or(_) => unreachable!("Or is compiled by compile_pattern_match, never nested inside another pattern's test per this language's grammar"),
        }
    }

    /// Shared tail of every AND-chain test above: `True` if every fail
    /// jump was avoided, `False` if any of them landed here. `True` and
    /// `False` are mutually-exclusive alternatives (only one runs per
    /// pass) reached via a jump, not a sequence — exactly the
    /// `stack_depth` hazard `Expr::If` already reconciles: snapshot the
    /// depth right after the fail jumps converge (both branches start
    /// from the same real depth there) and restore it before compiling
    /// `False`, so `stack_depth` after this function reflects "exactly
    /// one Bool landed here," not "both landed here."
    fn finish_and_chain(&mut self, fail_jumps: Vec<usize>, line: u32) {
        let branch_entry_depth = self.current().stack_depth;
        self.current().emit_op(Op::True, line);
        let to_end = self.current().emit_jump(Op::Jump, line);
        for j in fail_jumps {
            self.current().patch_jump(j);
        }
        self.current().stack_depth = branch_entry_depth;
        self.current().emit_op(Op::False, line);
        self.current().patch_jump(to_end);
    }

    /// Collects every name a pattern would bind, in traversal order, WITH
    /// duplicates — mirrors `ember-resolve`'s own `collect_pattern_bind_names`
    /// exactly (both crates need the identical distinct-name set for the
    /// same `Or`-pattern, computed the same way, to stay in lockstep). Not
    /// shared as a crate dependency between the two — `ember-compile` reads
    /// `Pattern` directly off its own `&Ast`, same as `ember-resolve` does off
    /// its own, and duplicating this ~20-line traversal is simpler than
    /// inventing a shared abstraction for one function.
    fn collect_pattern_bind_names(&self, pat: Idx<ember_ast::Pattern>, out: &mut Vec<Symbol>) {
        match self.ast.pat(pat) {
            Pattern::Wild
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Error => {}
            Pattern::Bind(sym) => out.push(*sym),
            Pattern::Ctor { args, .. } => {
                for a in args {
                    self.collect_pattern_bind_names(*a, out);
                }
            }
            Pattern::Tuple(items) => {
                for i in items {
                    self.collect_pattern_bind_names(*i, out);
                }
            }
            Pattern::List { items, rest } => {
                for i in items {
                    self.collect_pattern_bind_names(*i, out);
                }
                if let Some(r) = rest {
                    self.collect_pattern_bind_names(*r, out);
                }
            }
            Pattern::Record { fields, .. } => {
                for (_, p) in fields {
                    self.collect_pattern_bind_names(*p, out);
                }
            }
            Pattern::Or(alts) => {
                for a in alts {
                    self.collect_pattern_bind_names(*a, out);
                }
            }
        }
    }

    /// One "extract a sub-value and bind (or recurse into) its pattern"
    /// step, shared by `Ctor`/`Record`/`List` binding below.
    fn compile_destructured_bind(
        &mut self,
        sub_pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        source: DestructureSource,
        line: u32,
    ) {
        if matches!(self.ast.pat(sub_pat), Pattern::Wild | Pattern::Error) {
            return;
        }
        self.emit_get_local(scrutinee_slot, line);
        match source {
            DestructureSource::Positional(i) => {
                self.current().chunk.write_op(Op::Destructure, line);
                self.current().chunk.write_u8(i, line);
                self.current().adjust_depth(0);
            }
            DestructureSource::Named(sym) => {
                let c = self.name_constant(sym);
                self.current().emit_op(Op::GetField, line);
                self.current().chunk.write_u16(c, line);
            }
            DestructureSource::Indexed(i) => {
                let c = self.current().chunk.add_constant(Value::Int(i));
                self.emit_constant(c, line);
                self.current().emit_op(Op::GetIndex, line);
            }
        }
        match self.ast.pat(sub_pat).clone() {
            Pattern::Bind(sym) => self.declare_named_local(sym, line),
            _ => {
                self.push_local(None);
                let temp_slot = self.current().local_count - 1;
                self.compile_pattern_bind(sub_pat, temp_slot, line);
            }
        }
    }

    /// Like `compile_destructured_bind`, but used only from
    /// `compile_pattern_bind_into_reserved` (see that function's doc
    /// comment). Any intermediate temp needed to hold a nested sub-pattern's
    /// own destructured value (e.g. unpacking `Pair` before reaching a bind
    /// two levels deep) is self-cleaning: pushed, used, then popped again
    /// before returning — unlike `compile_destructured_bind`'s temps, which
    /// are deliberately left for the enclosing scope-exit to sweep up, these
    /// must not persist, since they don't correspond to any slot the
    /// resolver counted (only the FINAL bound names do, via `reserved`).
    fn compile_destructured_bind_into_reserved(
        &mut self,
        sub_pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        source: DestructureSource,
        reserved: &FxHashMap<Symbol, u32>,
        line: u32,
    ) {
        if matches!(self.ast.pat(sub_pat), Pattern::Wild | Pattern::Error) {
            return;
        }
        self.emit_get_local(scrutinee_slot, line);
        match source {
            DestructureSource::Positional(i) => {
                self.current().chunk.write_op(Op::Destructure, line);
                self.current().chunk.write_u8(i, line);
                self.current().adjust_depth(0);
            }
            DestructureSource::Named(sym) => {
                let c = self.name_constant(sym);
                self.current().emit_op(Op::GetField, line);
                self.current().chunk.write_u16(c, line);
            }
            DestructureSource::Indexed(i) => {
                let c = self.current().chunk.add_constant(Value::Int(i));
                self.emit_constant(c, line);
                self.current().emit_op(Op::GetIndex, line);
            }
        }
        match self.ast.pat(sub_pat).clone() {
            Pattern::Bind(sym) => {
                let slot = reserved[&sym];
                self.emit_set_local(slot, line);
                self.current().emit_op(Op::Pop, line);
            }
            _ => {
                self.push_local(None);
                let temp_slot = self.current().local_count - 1;
                self.compile_pattern_bind_into_reserved(sub_pat, temp_slot, reserved, line);
                self.emit_scope_pops(1, line);
            }
        }
    }

    /// Like `compile_pattern_bind`, but used only while compiling one
    /// alternative of an `Or` pattern (see `compile_pattern_match`): every
    /// name this alternative's pattern binds must write into a slot some
    /// OTHER, unconditionally-run code has already reserved, because only
    /// one alternative's bind ever actually executes at runtime — a plain
    /// push-and-declare here would leave the slot's physical stack position
    /// undefined whenever a DIFFERENT alternative is the one that matched.
    /// `reserved` maps every name any alternative of this same `Or` binds to
    /// its one shared physical slot.
    fn compile_pattern_bind_into_reserved(
        &mut self,
        pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        reserved: &FxHashMap<Symbol, u32>,
        line: u32,
    ) {
        match self.ast.pat(pat).clone() {
            Pattern::Wild
            | Pattern::Error
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Bool(_)
            | Pattern::Str(_)
            | Pattern::Tuple(_) => {}
            Pattern::Bind(sym) => {
                let slot = reserved[&sym];
                self.emit_get_local(scrutinee_slot, line);
                self.emit_set_local(slot, line);
                self.current().emit_op(Op::Pop, line);
            }
            Pattern::Ctor { args, .. } => {
                for (i, &arg_pat) in args.iter().enumerate() {
                    self.compile_destructured_bind_into_reserved(
                        arg_pat,
                        scrutinee_slot,
                        DestructureSource::Positional(i as u8),
                        reserved,
                        line,
                    );
                }
            }
            Pattern::Record { fields, .. } => {
                for (fname, fpat) in fields {
                    self.compile_destructured_bind_into_reserved(
                        fpat,
                        scrutinee_slot,
                        DestructureSource::Named(fname),
                        reserved,
                        line,
                    );
                }
            }
            Pattern::List { items, rest } => {
                for (i, &item_pat) in items.iter().enumerate() {
                    self.compile_destructured_bind_into_reserved(
                        item_pat,
                        scrutinee_slot,
                        DestructureSource::Indexed(i as i64),
                        reserved,
                        line,
                    );
                }
                if let Some(rest_pat) = rest {
                    if let Pattern::Bind(sym) = self.ast.pat(rest_pat) {
                        let sym = *sym;
                        let slot = reserved[&sym];
                        // Known gap (matches compile_pattern_bind's own): a
                        // Nil placeholder, not the real remaining sublist.
                        self.current().emit_op(Op::Nil, line);
                        self.emit_set_local(slot, line);
                        self.current().emit_op(Op::Pop, line);
                    }
                }
            }
            Pattern::Or(_) => {
                unreachable!("nested Or inside an Or alternative is not produced by this grammar")
            }
        }
    }

    /// Declares a new local for every `Bind` in `pat` — only ever called
    /// once `compile_pattern_test` of this same `pat` has already
    /// returned `true` on this control-flow path. No branching here at
    /// all, so hidden temps are simply left live for the enclosing scope's
    /// own `emit_tail_scope_exit` to sweep up.
    fn compile_pattern_bind(
        &mut self,
        pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        line: u32,
    ) {
        match self.ast.pat(pat).clone() {
            Pattern::Wild
            | Pattern::Error
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Bool(_)
            | Pattern::Str(_)
            | Pattern::Tuple(_) => {}
            Pattern::Bind(sym) => {
                self.emit_get_local(scrutinee_slot, line);
                self.declare_named_local(sym, line);
            }
            Pattern::Ctor { args, .. } => {
                for (i, &arg_pat) in args.iter().enumerate() {
                    self.compile_destructured_bind(
                        arg_pat,
                        scrutinee_slot,
                        DestructureSource::Positional(i as u8),
                        line,
                    );
                }
            }
            Pattern::Record { fields, .. } => {
                for (fname, fpat) in fields {
                    self.compile_destructured_bind(
                        fpat,
                        scrutinee_slot,
                        DestructureSource::Named(fname),
                        line,
                    );
                }
            }
            Pattern::List { items, rest } => {
                for (i, &item_pat) in items.iter().enumerate() {
                    self.compile_destructured_bind(
                        item_pat,
                        scrutinee_slot,
                        DestructureSource::Indexed(i as i64),
                        line,
                    );
                }
                if let Some(rest_pat) = rest {
                    if let Pattern::Bind(sym) = self.ast.pat(rest_pat) {
                        let sym = *sym;
                        // Known gap: a Nil placeholder, not the real
                        // remaining sublist — only the slot is reserved.
                        self.current().emit_op(Op::Nil, line);
                        self.declare_named_local(sym, line);
                    }
                }
            }
            Pattern::Or(_) => {
                unreachable!("bound by whichever alternative matched — see compile_pattern_match")
            }
        }
    }

    /// Tests, then (only on success) binds, one match arm's pattern. On
    /// failure, pushes a jump address into `fail_jumps` for the caller to
    /// patch once it knows where "try the next arm" begins. `Or` gets its
    /// own control flow here — each alternative is an independent
    /// test/bind/jump-to-success unit.
    ///
    /// Each alternative's `test` genuinely runs in sequence (alt 2's test
    /// is only reached once alt 1's has already failed — a real chain).
    /// But each alternative's `bind` is mutually exclusive with every
    /// OTHER alternative's: only one alternative's bind ever executes per
    /// pass, yet the compiler emits (and, without correction, tallies)
    /// every alternative's bind into the same linear `stack_depth` count.
    /// Snapshot the depth right where the alternatives diverge — after the
    /// scrutinee is in place and before the first alternative's test, the
    /// same real depth every subsequent alternative's test is ALSO
    /// reached at (only via the previous alternative's `this_fails` jump)
    /// — and restore it before each subsequent alternative, exactly like
    /// `Expr::If` reconciles `then_`/`else_`.
    fn compile_pattern_match(
        &mut self,
        pat: Idx<ember_ast::Pattern>,
        scrutinee_slot: u32,
        fail_jumps: &mut Vec<usize>,
        line: u32,
    ) {
        if let Pattern::Or(alts) = self.ast.pat(pat).clone() {
            let mut end_jumps = Vec::new();

            // Every alternative could bind the same name (that's the
            // whole point of `Circle(r) | Square(r) => r`) — but only
            // one alternative's bind ever executes at runtime, since
            // they're mutually exclusive branches reached only via the
            // previous alternative's failed-test jump. Reserving each
            // distinct bound name's slot UNCONDITIONALLY here, before any
            // alternative's test runs, guarantees the slot physically
            // exists on the stack regardless of which alternative later
            // matches — a plain push-and-declare inside one alternative's
            // own (conditionally executed) branch would leave the slot's
            // physical position undefined whenever a DIFFERENT
            // alternative is the one that actually matched. Every
            // alternative then writes into these reserved slots via
            // `compile_pattern_bind_into_reserved` instead of the normal
            // `compile_pattern_bind`.
            let mut names = Vec::new();
            for &alt in &alts {
                self.collect_pattern_bind_names(alt, &mut names);
            }
            let mut reserved: FxHashMap<Symbol, u32> = FxHashMap::default();
            for name in names {
                if let std::collections::hash_map::Entry::Vacant(e) = reserved.entry(name) {
                    self.current().emit_op(Op::Nil, line);
                    self.declare_named_local(name, line);
                    let slot = self.current().local_count - 1;
                    e.insert(slot);
                }
            }

            // Captured AFTER pre-reservation, not before: the
            // reservation's Nil-pushes are real, unconditional stack
            // growth that every alternative's test is reached "on top
            // of" — resetting to a depth from BEFORE reservation would
            // undercount by one per distinct reserved name.
            let alts_entry_depth = self.current().stack_depth;
            for &alt in &alts {
                self.current().stack_depth = alts_entry_depth;
                self.compile_pattern_test(alt, scrutinee_slot, line);
                let this_fails = self.current().emit_jump(Op::JumpIfFalse, line);
                self.compile_pattern_bind_into_reserved(alt, scrutinee_slot, &reserved, line);
                end_jumps.push(self.current().emit_jump(Op::Jump, line));
                self.current().patch_jump(this_fails);
                // falls through to the next alternative's test (or, after
                // the last alternative, straight into the line below) —
                // reached at exactly `alts_entry_depth`, the same real
                // depth every alternative's test starts from.
            }
            self.current().stack_depth = alts_entry_depth;
            // Every alternative's test failed. The `reserved.len()` slots
            // reserved above were pushed unconditionally before any test
            // ran — nothing bound them (no alternative matched), and
            // nothing else on this path will ever pop them (the
            // success-path body/scope-exit code below is never reached
            // via this jump), so they must be cleaned up explicitly here.
            // Raw `Op::Pop`, not `emit_scope_pops`: this must NOT touch
            // `local_count`/`declared_slots`, which the success-path body
            // compiled immediately after this function returns still
            // needs to correctly describe.
            for _ in 0..reserved.len() {
                self.current().emit_op(Op::Pop, line);
            }
            fail_jumps.push(self.current().emit_jump(Op::Jump, line));
            for j in end_jumps {
                self.current().patch_jump(j);
            }
            return;
        }
        self.compile_pattern_test(pat, scrutinee_slot, line);
        fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
        self.compile_pattern_bind(pat, scrutinee_slot, line);
    }

    /// Compiles a `match` expression: the scrutinee into a hidden local,
    /// then each arm as an independent test/bind/guard/body unit chained
    /// by fail-jumps, converging on a shared end.
    ///
    /// Each arm's full `test+bind+guard+body+scope_exit+Jump-to-end` is
    /// mutually exclusive with every OTHER arm's (and with the final
    /// unreachable-fallback `Nil`) — only one ever actually executes per
    /// pass, exactly the same hazard as `compile_pattern_match`'s `Or`
    /// loop above and `Expr::If`'s branches. `local_count`/`declared_slots`
    /// don't need the same treatment: each arm's own `emit_tail_scope_exit`
    /// call already unconditionally restores those back down to
    /// `match_entry_local_count`'s equivalent (`arm_entry_local_count`,
    /// freshly re-read at the top of every loop iteration) regardless of
    /// this bug, since that's real, always-correct teardown bytecode, not
    /// alternate-path bookkeeping. `stack_depth` alone needs the
    /// snapshot/restore, at the same "arms entry" point — right after the
    /// scrutinee's hidden local is in place, before any arm's test runs —
    /// restored before every arm after the first, and before the final
    /// fallback `Nil`.
    fn compile_match(&mut self, scrutinee: Idx<Expr>, arms: Vec<ember_ast::MatchArm>, line: u32) {
        self.compile_expr(scrutinee);
        let match_entry_local_count = self.current().local_count;
        self.push_local(None);
        let scrutinee_slot = match_entry_local_count;
        let arms_entry_depth = self.current().stack_depth;

        let mut end_jumps = Vec::new();
        let mut prev_fail_jumps: Vec<usize> = Vec::new();

        for arm in &arms {
            for j in prev_fail_jumps.drain(..) {
                self.current().patch_jump(j);
            }
            self.current().stack_depth = arms_entry_depth;
            let arm_entry_local_count = self.current().local_count;
            let mut fail_jumps = Vec::new();
            self.compile_pattern_match(arm.pat, scrutinee_slot, &mut fail_jumps, line);

            // `bound_count` is captured HERE, not after
            // `emit_tail_scope_exit` runs below — that call
            // unconditionally resets `local_count` back down to
            // `arm_entry_local_count` as part of the compiler's own
            // linear code generation (regardless of which runtime path
            // is actually taken), so this is the only point where
            // `local_count` still reflects how many locals this arm's
            // pattern really bound.
            let guard_fail = arm.guard.map(|guard| {
                self.compile_expr(guard);
                let bound_count = self.current().local_count - arm_entry_local_count;
                let jump = self.current().emit_jump(Op::JumpIfFalse, line);
                (jump, bound_count)
            });

            self.compile_expr(arm.body);
            self.emit_tail_scope_exit(arm_entry_local_count, line);
            end_jumps.push(self.current().emit_jump(Op::Jump, line));

            if let Some((guard_fail_jump, bound_count)) = guard_fail {
                self.current().patch_jump(guard_fail_jump);
                // The pattern DID match (bound_count locals were really
                // pushed by its bind) but the guard failed — nothing else
                // on this path will ever pop them, since we're not taking
                // the body/scope-exit route just above (that code is only
                // reached by falling through from a successful guard, not
                // by jumping here). Raw Op::Pop, not `emit_scope_pops`,
                // for the same reason as `compile_pattern_match`'s own
                // matching fix in Task 2: must not touch `local_count`/
                // `declared_slots`, which the body code just above still
                // needs to have correctly described.
                for _ in 0..bound_count {
                    self.current().emit_op(Op::Pop, line);
                }
                fail_jumps.push(self.current().emit_jump(Op::Jump, line));
            }

            prev_fail_jumps = fail_jumps;
        }
        for j in prev_fail_jumps {
            self.current().patch_jump(j);
        }
        self.current().stack_depth = arms_entry_depth;
        // Unreachable in a program that passed the type-checker's
        // exhaustiveness check — a defined fallback keeps the chunk
        // well-formed rather than falling off the end of it.
        self.current().emit_op(Op::Nil, line);

        for j in end_jumps {
            self.current().patch_jump(j);
        }
        self.emit_tail_scope_exit(match_entry_local_count, line);
    }
}

/// The public "AST in, bytecode out" entry point. Mirrors
/// `ember-resolve`'s `resolve_program` two-pass hoist of top-level
/// `Fn`/`TypeDecl`/`StructDecl` names (pass 1) ahead of everything else
/// (pass 2), in source order within each pass — see this task's module
/// doc for why that specific ordering (not plain top-to-bottom source
/// order) is required to keep this compiler's `local_count` bookkeeping
/// aligned with the resolver's actual slot numbers.
pub fn compile(
    ast: &Ast,
    interner: &mut Interner,
    bindings: &Bindings,
    stmts: &[Idx<ember_ast::Stmt>],
) -> ember_bytecode::chunk::FunctionProto {
    let mut compiler = Compiler::new(ast, interner, bindings);

    for &s in stmts {
        if matches!(
            compiler.ast.stmt(s),
            ember_ast::Stmt::Fn { .. }
                | ember_ast::Stmt::TypeDecl { .. }
                | ember_ast::Stmt::StructDecl { .. }
        ) {
            compiler.compile_stmt(s);
        }
    }

    let non_hoisted: Vec<Idx<ember_ast::Stmt>> = stmts
        .iter()
        .copied()
        .filter(|&s| {
            !matches!(
                compiler.ast.stmt(s),
                ember_ast::Stmt::Fn { .. }
                    | ember_ast::Stmt::TypeDecl { .. }
                    | ember_ast::Stmt::StructDecl { .. }
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
                if let ember_ast::Stmt::ExprStmt(e) = compiler.ast.stmt(s) {
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

    compiler.current().chunk.write_op(Op::Return, 0);
    compiler.current().adjust_depth(-1);

    let fc = compiler
        .functions
        .pop()
        .expect("top-level FunctionCompiler always present");
    let script_name = compiler.interner.intern("<script>");
    ember_bytecode::chunk::FunctionProto {
        chunk: fc.chunk,
        arity: 0,
        upvalues: Vec::new(),
        name: script_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::{Interner, Stmt};
    use ember_bytecode::op::Op;
    use ember_resolve::FunctionId;

    fn empty_fc() -> FunctionCompiler {
        FunctionCompiler::new(FunctionId::TopLevel)
    }

    /// A "compiles cleanly" check for the test helpers below that actually
    /// matches what that phrase means: no ERROR-severity diagnostic. A
    /// plain `resolve_diags.is_empty()` (this file's original helper
    /// design) conflates warnings with errors — and the resolver's
    /// unused-variable check is a warning that a type's own name (or an
    /// ADT variant referenced only from a pattern, never a value
    /// position) can trip even in a source snippet that's otherwise
    /// completely valid, since neither `Expr::Struct`/`TypeDecl`'s own
    /// name nor a `Ctor`/`Record` pattern's `name` field is ever resolved
    /// against the declared binding (a documented, pre-existing resolver
    /// gap, not something this task's compiler code can or should paper
    /// over by renaming every ADT/struct type used in a test).
    fn assert_no_errors(diags: &[ember_diag::Diagnostic]) {
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "resolve errors: {errors:?}");
    }

    #[test]
    fn static_stack_effect_covers_every_fixed_effect_op() {
        assert_eq!(static_stack_effect(Op::Constant), Some(1));
        assert_eq!(static_stack_effect(Op::Pop), Some(-1));
        assert_eq!(static_stack_effect(Op::Add), Some(-1));
        assert_eq!(static_stack_effect(Op::Not), Some(0));
        assert_eq!(static_stack_effect(Op::JumpIfFalse), Some(-1));
        assert_eq!(static_stack_effect(Op::DefineGlobal), Some(-1));
        assert_eq!(static_stack_effect(Op::Closure), Some(1));
        assert_eq!(
            static_stack_effect(Op::Call),
            None,
            "operand-dependent, not in the static table"
        );
        assert_eq!(static_stack_effect(Op::MakeList), None);
        assert_eq!(
            static_stack_effect(Op::Destructure),
            Some(0),
            "its operand is an index, not a count — pop base, push field, always"
        );
        assert_eq!(static_stack_effect(Op::TestVariant), Some(0));
        assert_eq!(static_stack_effect(Op::Return), Some(-1));
    }

    #[test]
    fn emit_op_updates_stack_depth_by_the_static_effect() {
        let mut fc = empty_fc();
        fc.emit_op(Op::Constant, 1);
        assert_eq!(fc.stack_depth, 1);
        fc.emit_op(Op::Constant, 1);
        assert_eq!(fc.stack_depth, 2);
        fc.emit_op(Op::Add, 1);
        assert_eq!(fc.stack_depth, 1);
    }

    #[test]
    fn emit_jump_writes_placeholder_and_patch_jump_backfills_correct_offset() {
        let mut fc = empty_fc();
        let jump_at = fc.emit_jump(Op::Jump, 1);
        fc.chunk.write_op(Op::Nil, 1); // 1 byte of "loop body"
        fc.chunk.write_op(Op::Pop, 1); // another byte
        fc.patch_jump(jump_at);
        let bytes = [fc.chunk.code[jump_at], fc.chunk.code[jump_at + 1]];
        let patched = u16::from_be_bytes(bytes);
        assert_eq!(
            patched, 2,
            "jump should skip exactly the 2 one-byte instructions emitted after it"
        );
    }

    #[test]
    fn emit_loop_backpatches_a_negative_style_offset_to_the_loop_start() {
        let mut fc = empty_fc();
        let loop_start = fc.chunk.code.len();
        fc.chunk.write_op(Op::Nil, 1);
        fc.emit_loop(loop_start, 1);
        // disassemble to confirm the resolved target is loop_start
        let interner = Interner::new();
        let out = ember_bytecode::disasm::disassemble_chunk(&fc.chunk, "test", &interner);
        assert!(out.contains(&format!("-> {loop_start:04}")), "{out}");
    }

    #[test]
    #[should_panic]
    fn patch_jump_panics_if_the_jump_distance_does_not_fit_in_u16() {
        let mut fc = empty_fc();
        let jump_at = fc.emit_jump(Op::Jump, 1);
        fc.chunk.code.resize(fc.chunk.code.len() + 70_000, 0);
        fc.patch_jump(jump_at);
    }

    #[test]
    fn physical_slot_is_identity_with_no_active_shifts() {
        let fc = empty_fc();
        assert_eq!(fc.physical_slot(0), 0);
        assert_eq!(fc.physical_slot(5), 5);
    }

    #[test]
    fn physical_slot_shifts_only_slots_at_or_above_base() {
        let mut fc = empty_fc();
        fc.slot_shifts.push(SlotShift { base: 3, extra: 2 });
        assert_eq!(fc.physical_slot(0), 0, "below base: untouched");
        assert_eq!(fc.physical_slot(2), 2, "below base: untouched");
        assert_eq!(fc.physical_slot(3), 5, "at base: shifted by extra");
        assert_eq!(fc.physical_slot(4), 6, "above base: shifted by extra");
    }

    #[test]
    fn physical_slot_compounds_nested_shifts() {
        let mut fc = empty_fc();
        fc.slot_shifts.push(SlotShift { base: 2, extra: 2 });
        fc.slot_shifts.push(SlotShift { base: 5, extra: 2 });
        assert_eq!(fc.physical_slot(1), 1, "below both bases");
        assert_eq!(
            fc.physical_slot(3),
            5,
            "only the outer shift applies (3 < 5)"
        );
        assert_eq!(
            fc.physical_slot(6),
            10,
            "both shifts apply (6 >= 2 and 6 >= 5)"
        );
    }

    fn compile_expr_str(src: &str) -> String {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert_no_errors(&resolve_diags);
        let mut compiler = Compiler::new(&ast, &mut interner, &bindings);
        // Compile the LAST statement's expression, not necessarily the
        // first: `a_local_variable_reference_compiles_to_get_local` feeds
        // in `"let x = 1; x;"`, two statements, where only the second is
        // the `ExprStmt` under test — `Stmt::Let` compilation isn't part of
        // this task, but resolution already ran over the whole program, so
        // compiling the trailing `Var` reference directly still exercises
        // real `Resolution::Local` data.
        let last = *stmts
            .last()
            .expect("compile_expr_str needs at least one statement");
        let Stmt::ExprStmt(expr_idx) = ast.stmt(last) else {
            panic!("expected the last statement to be an expression statement");
        };
        compiler.compile_expr(*expr_idx);
        let fc = compiler.functions.pop().unwrap();
        ember_bytecode::disasm::disassemble_chunk(&fc.chunk, "test", &interner)
    }

    #[test]
    fn compiles_an_integer_literal_to_a_pooled_constant() {
        let out = compile_expr_str("42;");
        assert!(out.contains("OP_CONSTANT"), "{out}");
        assert!(out.contains("Int(42)"), "{out}");
    }

    #[test]
    fn compiles_arithmetic_with_correct_op_order() {
        let out = compile_expr_str("1 + 2 * 3;");
        let lines: Vec<&str> = out.lines().collect();
        let mul_line = lines.iter().position(|l| l.contains("OP_MUL")).unwrap();
        let add_line = lines.iter().position(|l| l.contains("OP_ADD")).unwrap();
        assert!(
            mul_line < add_line,
            "* must be emitted (and thus executed) before +: {out}"
        );
    }

    #[test]
    fn not_equal_desugars_to_equal_then_not() {
        let out = compile_expr_str("1 != 2;");
        assert!(out.contains("OP_EQUAL"), "{out}");
        assert!(out.contains("OP_NOT"), "{out}");
    }

    #[test]
    fn and_and_short_circuits_via_jump_not_an_opcode() {
        let out = compile_expr_str("true && false;");
        assert!(out.contains("OP_JUMP_IF_FALSE"), "{out}");
        assert!(
            !out.contains("OP_AND"),
            "there is no OP_AND — && must compile to jumps: {out}"
        );
    }

    #[test]
    fn a_local_variable_reference_compiles_to_get_local() {
        let out = compile_expr_str("let x = 1; x;");
        assert!(out.contains("OP_GET_LOCAL"), "{out}");
    }

    fn compile_program_str(src: &str) -> (String, ember_ast::Interner) {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert_no_errors(&resolve_diags);
        let mut compiler = Compiler::new(&ast, &mut interner, &bindings);
        for &s in &stmts {
            compiler.compile_stmt(s);
        }
        let fc = compiler.functions.pop().unwrap();
        let out = ember_bytecode::disasm::disassemble_chunk(&fc.chunk, "test", &interner);
        (out, interner)
    }

    /// `ember_bytecode::disasm::disassemble_chunk` (Task 5, already shipped
    /// and out of this task's scope to change) only prints ONE chunk's own
    /// instructions — it does not recurse into `chunk.functions[i].chunk`,
    /// the nested chunk each `OP_CLOSURE` instantiates. Counting
    /// `OP_CLOSURE` occurrences across an entire nested-closures program
    /// therefore needs its own recursive walk, done here in the test
    /// module rather than by changing production disassembly code.
    fn disassemble_recursively(
        chunk: &Chunk,
        name: &str,
        interner: &ember_ast::Interner,
    ) -> String {
        let mut out = ember_bytecode::disasm::disassemble_chunk(chunk, name, interner);
        for (i, proto) in chunk.functions.iter().enumerate() {
            let nested_name = format!("{name}::fn{i}");
            out.push_str(&disassemble_recursively(
                &proto.chunk,
                &nested_name,
                interner,
            ));
        }
        out
    }

    fn compile_program_chunk(src: &str) -> (Chunk, ember_ast::Interner) {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert_no_errors(&resolve_diags);
        let mut compiler = Compiler::new(&ast, &mut interner, &bindings);
        for &s in &stmts {
            compiler.compile_stmt(s);
        }
        let fc = compiler.functions.pop().unwrap();
        (fc.chunk, interner)
    }

    #[test]
    fn a_top_level_let_gets_dual_registration() {
        // Named `_x`, not `x`: `x` is otherwise never read, and the
        // resolver's (pre-existing, unrelated to this task) unused-variable
        // check would turn that into a warning that trips this test
        // helper's `resolve_diags.is_empty()` assertion. The underscore
        // prefix suppresses that check without changing what's being
        // tested here — dual registration of a top-level local.
        let (out, _) = compile_program_str("let _x = 1;");
        assert!(out.contains("OP_CONSTANT"), "{out}");
        assert!(
            out.contains("OP_DEFINE_GLOBAL"),
            "top-level let must also define a global: {out}"
        );
        assert!(
            !out.contains("OP_POP"),
            "the top-level local itself is never popped: {out}"
        );
    }

    #[test]
    fn a_let_inside_a_block_does_not_get_dual_registration() {
        let (out, _) = compile_program_str("{ let y = 1; y };");
        assert!(
            !out.contains("OP_DEFINE_GLOBAL"),
            "a block-local let is never a global: {out}"
        );
    }

    #[test]
    fn a_block_with_two_locals_and_a_tail_pops_both_and_keeps_the_tail() {
        // Wrapped in `let _r = { ... };` rather than compiled as a bare
        // top-level statement `{ ... };`: a bare block-as-statement is
        // itself an `ExprStmt`, which (correctly, per `compile_stmt`)
        // unconditionally pops the statement's value — an extra pop
        // unrelated to the block's *own* scope-exit cleanup that this test
        // is isolating. Binding it to a `let` instead means only the
        // block's own cleanup pops show up in the count.
        let (out, _) = compile_program_str("let _r = { let a = 1; let b = 2; a + b };");
        let pop_count = out.matches("OP_POP").count();
        assert_eq!(pop_count, 2, "exactly 2 locals to clean up: {out}");
        assert!(out.contains("OP_SET_LOCAL"), "{out}");
    }

    #[test]
    fn a_block_with_no_locals_emits_no_pops() {
        // See the note on the previous test: `let _r = { ... };` avoids the
        // bare-statement's own unconditional `ExprStmt` pop so this test
        // isolates exactly what it's named for — the block's own scope-exit
        // logic (a no-op here, since it declared zero locals).
        let (out, _) = compile_program_str("let _r = { 1 + 2 };");
        assert!(!out.contains("OP_POP"), "{out}");
        assert!(!out.contains("OP_SET_LOCAL"), "{out}");
    }

    #[test]
    fn a_block_with_no_tail_pushes_nil() {
        // `_z`, not `z`: see `a_top_level_let_gets_dual_registration` above
        // for why — `z` is declared but never read, which the resolver
        // (correctly, pre-existing behavior unrelated to this task) flags
        // as an unused-variable warning.
        let (out, _) = compile_program_str("{ let _z = 1; };");
        assert!(out.contains("OP_NIL"), "missing tail must push Nil: {out}");
    }

    #[test]
    fn if_else_both_branches_leave_one_value_and_jump_correctly() {
        let (out, _) = compile_program_str("if true { 1 } else { 2 };");
        assert!(out.contains("OP_JUMP_IF_FALSE"), "{out}");
        assert!(out.contains("OP_JUMP "), "{out}");
    }

    #[test]
    fn if_without_else_pushes_nil_in_the_false_branch() {
        let (out, _) = compile_program_str("if true { 1 };");
        assert!(out.contains("OP_NIL"), "{out}");
    }

    #[test]
    fn while_loop_jumps_back_to_the_condition_check() {
        let (out, _) = compile_program_str("while true { 1; }");
        assert!(out.contains("OP_LOOP"), "{out}");
        assert!(out.contains("OP_JUMP_IF_FALSE"), "{out}");
    }

    #[test]
    fn for_loop_compiles_to_a_get_local_get_index_pair_for_each_element_access() {
        // `compile_program_str` panics (via its internal `resolve_diags.is_empty()`
        // assertion) on ANY diagnostic, including plain errors — so, unlike the
        // task-plan comment suggests, we can't actually call it with an
        // undeclared `xs` and inspect the output afterward; declare `xs`
        // first. Also, list literals (`[1, 2]`) aren't compilable yet —
        // `Expr::List` compilation is a later task (list/struct literals) —
        // and this compiler does no type checking, so any already-supported
        // expression works fine as the (compile-time-only) iterable here.
        let (out2, _) = compile_program_str("let xs = 1; for x in xs { x; }");
        assert!(out2.contains("OP_GET_INDEX"), "{out2}");
        assert!(out2.contains("OP_LOOP"), "{out2}");
        assert!(out2.contains("\"len\""), "must call the len native: {out2}");
    }

    #[test]
    fn break_in_a_nested_loop_targets_the_innermost_loop() {
        // `_i`, not `i`: `i` is otherwise never read, which the resolver's
        // (pre-existing, unrelated to this task) unused-variable check would
        // turn into a warning that trips this test helper's
        // `resolve_diags.is_empty()` assertion.
        let src = "let mut _i = 0; while true { while true { break; } break; }";
        let (out, _) = compile_program_str(src);
        // Two independent break jumps, one per loop — not one shared target.
        let jump_count = out.lines().filter(|l| l.contains("OP_JUMP ")).count();
        assert!(jump_count >= 2, "{out}");
    }

    #[test]
    fn break_inside_a_nested_block_pops_the_blocks_locals_before_jumping() {
        // `_a`, not `a`: see the note on `break_in_a_nested_loop_targets_the_innermost_loop`.
        let src = "while true { let _a = 1; break; }";
        let (out, _) = compile_program_str(src);
        // Expect at least one OP_POP emitted right before break's OP_JUMP
        // for the still-live `a`, on top of the loop's own eventual
        // cleanup pop.
        let pop_count = out.matches("OP_POP").count();
        assert!(pop_count >= 2, "{out}");
    }

    #[test]
    fn plain_loop_has_no_condition_check() {
        let (out, _) = compile_program_str("loop { break; }");
        assert!(
            !out.contains("OP_JUMP_IF_FALSE"),
            "an unconditional loop has no condition to test: {out}"
        );
        assert!(out.contains("OP_LOOP"), "{out}");
    }

    #[test]
    fn nested_if_inside_while_has_correctly_resolved_jump_targets() {
        // CHECKLIST.md: "jump offsets correct for nested if/while". The
        // disassembler resolves every jump to its real, absolute target
        // address — a wrong offset would show up as a `->` target that
        // either points at the wrong instruction or falls outside the
        // chunk entirely. Cross-checking that every jump target in this
        // nested program is a real instruction boundary (one of the
        // `NNNN` offsets the disassembler itself printed a line for) is a
        // strong, mechanical way to catch a miscomputed offset.
        //
        // Trailing `3;` deliberately follows the `while`: the disassembler
        // (`disassemble_chunk`) only prints a line for an offset that has
        // an actual instruction starting there, so a jump target sitting
        // exactly at the end of the whole chunk — which is where the
        // while-loop's own exit jump lands when it's the last thing
        // compiled — would never appear in `instruction_offsets` even
        // when correct. Something after the loop gives that exit jump a
        // real, printed instruction to land on, so this test's boundary
        // check is actually meaningful rather than failing on every
        // trailing loop regardless of correctness.
        let src = "while true { if true { 1; } else { 2; } } 3;";
        let (out, _) = compile_program_str(src);
        let instruction_offsets: std::collections::HashSet<&str> = out
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        let jump_target_count = out.matches("->").count();
        assert!(
            jump_target_count >= 2,
            "expects at least the if's JumpIfFalse and Jump: {out}"
        );
        for line in out.lines().filter(|l| l.contains("->")) {
            let target = line.rsplit("-> ").next().unwrap().trim();
            assert!(
                instruction_offsets.contains(target),
                "jump target {target} in {line:?} doesn't land on a real instruction boundary: {out}"
            );
        }
    }

    #[test]
    fn assigning_to_a_local_variable_compiles_to_set_local() {
        let (out, _) = compile_program_str("let mut x = 1; x = 2;");
        assert!(out.contains("OP_SET_LOCAL"), "{out}");
    }

    #[test]
    fn assigning_to_an_index_target_compiles_base_index_value_then_set_index() {
        let (out, _) = compile_program_str("let mut xs = [1]; xs[0] = 2;");
        assert!(out.contains("OP_SET_INDEX"), "{out}");
    }

    #[test]
    fn assigning_to_a_field_target_compiles_base_value_then_set_field() {
        // Struct type named `_P`, not `P`: `Expr::Struct` never marks its
        // type name as used against the declared struct binding (a
        // pre-existing, documented resolver gap — see
        // ember-resolve's `unary_binary_index_field_and_list_struct_all_resolve_their_subexpressions`),
        // so an un-prefixed name would leave the declaration looking
        // unused and trip this test helper's `resolve_diags.is_empty()`
        // assertion with an "unused variable" warning.
        let src = "struct _P { x: Int } let mut p = _P { x: 1 }; p.x = 2;";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_SET_FIELD"), "{out}");
    }

    #[test]
    fn list_literal_compiles_every_item_then_make_list_with_the_right_count() {
        let (out, _) = compile_program_str("[1, 2, 3];");
        assert!(out.contains("OP_MAKE_LIST count=3"), "{out}");
    }

    #[test]
    fn index_read_compiles_to_get_index() {
        let (out, _) = compile_program_str("let xs = [1]; xs[0];");
        assert!(out.contains("OP_GET_INDEX"), "{out}");
    }

    #[test]
    fn field_read_compiles_to_get_field_with_the_resolved_name() {
        // `_P`, not `P` — see the comment in
        // `assigning_to_a_field_target_compiles_base_value_then_set_field`
        // for why the struct type name needs the underscore prefix.
        let src = "struct _P { x: Int } let p = _P { x: 1 }; p.x;";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_GET_FIELD"), "{out}");
        assert!(out.contains("\"x\""), "{out}");
    }

    #[test]
    fn struct_literal_pushes_interleaved_name_value_pairs_then_make_record() {
        // `_P`, not `P` — see the comment in
        // `assigning_to_a_field_target_compiles_base_value_then_set_field`
        // for why the struct type name needs the underscore prefix.
        let src = "struct _P { x: Int, y: Int } _P { x: 1, y: 2 };";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_MAKE_RECORD \"_P\" fields=2"), "{out}");
    }

    #[test]
    fn a_lambda_compiles_to_op_closure_referencing_a_function_proto() {
        // `_f`, not `f`: see `a_top_level_let_gets_dual_registration` above
        // for why an otherwise-never-read top-level binding needs the
        // underscore prefix to dodge the resolver's unused-variable warning.
        let (out, _) = compile_program_str("let _f = || 1;");
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    #[test]
    fn a_named_fn_compiles_to_op_closure_and_becomes_a_named_local() {
        // `_f`, not `f` — same reason as the lambda test above.
        let (out, _) = compile_program_str("fn _f() { 1 }");
        assert!(out.contains("OP_CLOSURE"), "{out}");
        assert!(
            out.contains("OP_DEFINE_GLOBAL"),
            "top-level fn also gets dual registration: {out}"
        );
    }

    #[test]
    fn a_captured_block_local_gets_close_upvalue_not_plain_pop_at_scope_exit() {
        let src = "let mut fns = []; { let counter = 0; let inc = || counter; fns = [inc]; }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_CLOSE_UPVALUE"), "{out}");
    }

    #[test]
    fn an_uncaptured_block_local_still_gets_a_plain_pop() {
        let (out, _) = compile_program_str("{ let a = 1; a };");
        assert!(out.contains("OP_POP"), "{out}");
        assert!(!out.contains("OP_CLOSE_UPVALUE"), "{out}");
    }

    #[test]
    fn nested_closures_compile_without_panicking_and_each_get_their_own_closure_op() {
        // Deviates from the task-plan source (`inner()`/`innermost()`) by
        // referencing each nested fn by NAME as its enclosing body's tail,
        // rather than calling it: `Expr::Call` isn't compilable yet
        // (that's Task 13, still `unimplemented!` in `compile_expr`), so a
        // literal call here would panic on something unrelated to what
        // this test is actually checking. Referencing (not calling) each
        // still exercises the same three-nested-closures shape — and, as a
        // bonus, `innermost`'s tail `x` now genuinely chains an upvalue
        // capture two levels deep (innermost captures `x` via inner's own
        // upvalue list; inner captures it directly from outer's locals).
        // `_outer`, not `outer`: the outermost fn itself is still never
        // referenced by anything, so it needs the usual underscore prefix
        // to dodge the resolver's unused-variable warning — see
        // `a_top_level_let_gets_dual_registration` above.
        let src = "fn _outer() { let x = 1; fn inner() { fn innermost() { x } innermost } inner }";
        let (chunk, interner) = compile_program_chunk(src);
        let out = disassemble_recursively(&chunk, "test", &interner);
        let closure_count = out.matches("OP_CLOSURE").count();
        assert_eq!(
            closure_count, 3,
            "outer, inner, and innermost each get one: {out}"
        );
    }

    #[test]
    fn break_inside_a_conditional_does_not_corrupt_the_enclosing_blocks_scope_bookkeeping() {
        // Regression test for a scope-accounting hazard this task's own
        // retrofit could introduce. `emit_loop_cleanup_pops` (used by
        // `break`/`continue`) must NOT permanently pop `declared_slots`/
        // decrement `local_count` when it emits its cleanup ops — those
        // describe the ALTERNATE (jumped-away) control-flow path, not this
        // still-open block's own real, linear exit, exactly like the
        // existing `stack_depth` snapshot/restore right around it.
        //
        // A version that instead routed through the mutating
        // `emit_scope_pops` (correct for a loop's OWN final teardown, e.g.
        // `compile_for`'s hidden locals — but not for break/continue,
        // which sit inside a scope that isn't actually ending) makes THIS
        // exact program panic with a `local_count` subtraction underflow:
        // `break` sits inside a nested `if`-block, and the `let _b = ...`
        // right after the `if` keeps declaring locals in the SAME
        // still-open outer block along the real, fallthrough (not-taken)
        // path — so that outer block's own eventual `local_count -
        // entry_local_count` bookkeeping must still see `a` as live right
        // up until the outer block's own genuine scope-exit cleanup runs.
        let src = "while true { let a = 1; if true { break; } let _b = a + 1; }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_LOOP"), "{out}");
        assert!(
            out.contains("OP_GET_LOCAL"),
            "`a` must still be readable in `a + 1`: {out}"
        );
    }

    #[test]
    fn a_struct_decl_nested_inside_a_block_does_not_corrupt_scope_bookkeeping() {
        // Regression test: `struct` is an ordinary keyword-led `stmt`,
        // legal anywhere a block accepts one (not just at top level), so
        // `compile_block`'s scope-exit accounting must stay in sync with
        // it too. Before routing `Stmt::StructDecl` through
        // `declare_named_local`, this exact program panicked with a
        // `local_count`/`declared_slots` subtraction underflow the moment
        // the enclosing block tried to compute its own cleanup.
        let src = "{ struct _Q { x: Int } let _q = _Q { x: 1 }; };";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_MAKE_RECORD"), "{out}");
    }

    #[test]
    fn a_call_compiles_callee_then_every_arg_then_op_call_with_argc() {
        let (out, _) = compile_program_str("let f = || 1; f();");
        assert!(out.contains("OP_CALL argc=0"), "{out}");
    }

    #[test]
    fn a_call_with_arguments_compiles_each_one_before_op_call() {
        let (out, _) = compile_program_str("let add = |a, b| a + b; add(1, 2);");
        assert!(out.contains("OP_CALL argc=2"), "{out}");
        let call_line = out.lines().position(|l| l.contains("OP_CALL")).unwrap();
        let constant_lines = out
            .lines()
            .take(call_line)
            .filter(|l| l.contains("OP_CONSTANT"))
            .count();
        assert!(
            constant_lines >= 2,
            "both 1 and 2 must be pushed before the call: {out}"
        );
    }

    #[test]
    fn a_nested_call_compiles_the_inner_call_as_part_of_computing_the_callee_or_an_arg() {
        let src = "let f = || || 1; f()();";
        let (out, _) = compile_program_str(src);
        assert_eq!(out.matches("OP_CALL argc=0").count(), 2, "{out}");
    }

    #[test]
    fn a_nullary_adt_variant_compiles_to_make_adt_and_becomes_a_named_local() {
        let src = "type Shape = Circle(Float) | Origin;";
        let (out, _) = compile_program_str(src);
        assert!(
            out.contains("OP_MAKE_ADT \"Shape\"::\"Origin\" arity=0"),
            "{out}"
        );
    }

    #[test]
    fn a_payload_adt_variant_compiles_to_a_synthetic_closure() {
        let src = "type Shape = Circle(Float);";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    #[test]
    fn a_ctor_pattern_tests_the_variant_tag_and_destructures_its_payload() {
        let src = "type Shape = Circle(Float); match Circle(1.0) { Circle(r) => r, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_TEST_VARIANT"), "{out}");
        assert!(out.contains("OP_DESTRUCTURE"), "{out}");
    }

    #[test]
    fn a_wildcard_fallback_arm_always_matches() {
        let src = "match 1 { 2 => 20, _ => 0, }";
        let (out, _) = compile_program_str(src);
        assert!(
            out.contains("OP_EQUAL"),
            "the literal arm still tests: {out}"
        );
    }

    #[test]
    fn a_guard_expression_adds_its_own_failure_jump() {
        let src = "match 1 { x if x > 0 => x, _ => 0, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_GREATER"), "{out}");
    }

    #[test]
    fn an_or_pattern_tries_each_alternative_without_panicking() {
        let src = "match 1 { 1 | 2 | 3 => 10, _ => 0, }";
        let (out, _) = compile_program_str(src);
        let equal_count = out.matches("OP_EQUAL").count();
        assert_eq!(equal_count, 3, "one Equal test per Or alternative: {out}");
    }

    #[test]
    fn a_record_pattern_uses_get_field_by_name_not_destructure() {
        // `_P`, not `P` — see the comment in
        // `assigning_to_a_field_target_compiles_base_value_then_set_field`
        // for why the struct type name needs the underscore prefix. The
        // scrutinee is bound to `p` first rather than written inline
        // (`match _P { x: 1 } { ... }`) because `match_expr` parses its
        // scrutinee with `no_struct_literal = true` (to disambiguate the
        // match's own opening `{` from a struct literal's), and that flag
        // isn't paren-scoped either, so there's no way to write a struct
        // literal directly as a match scrutinee.
        let src = "struct _P { x: Int } let p = _P { x: 1 }; match p { _P { x } => x, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_GET_FIELD"), "{out}");
    }

    #[test]
    fn a_tuple_pattern_still_never_matches() {
        // `_a`/`_b`, not `a`/`b`: both are bound (by a pattern in dead code
        // that never actually matches, per `Pattern::Tuple`'s documented
        // gap) but never read, which would otherwise trip the resolver's
        // unused-variable warning.
        let src = "match 1 { (_a, _b) => 1, _ => 0, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_FALSE"), "{out}");
    }

    #[test]
    fn an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping()
    {
        // Probes the stack_depth bug class described in this task's plan:
        // `compile_pattern_match`'s `Or` handling compiles EVERY
        // alternative's `bind` into the linear stream, even though only
        // one alternative's bind ever actually runs per pass. If
        // `stack_depth` isn't reset between alternatives, this program's
        // enclosing `fn`'s debug_assert (`fc.stack_depth == 0` in
        // `compile_function`) — or the per-statement debug_assert in
        // `compile_stmt` — should fire. It must not.
        let src = "type Shape = Circle(Int) | Square(Int); fn _f(s) { match s { Circle(r) | Square(r) => r, _ => 0, } }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    #[test]
    fn an_or_pattern_binding_the_same_name_writes_both_alternatives_into_the_same_slot() {
        // Complements the existing
        // `an_or_pattern_binding_the_same_name_in_every_alternative_does_not_corrupt_stack_bookkeeping`
        // test (which only proves compilation doesn't panic) by asserting the
        // emitted bytecode is actually consistent: whichever alternative's
        // bind runs, it must write `r` into the SAME physical slot, since the
        // arm body has only one `GetLocal` for `r` and it can't know at
        // compile time which alternative will have matched.
        //
        // Uses `compile_program_chunk` + `disassemble_recursively` (not
        // `compile_program_str`, which only disassembles the top-level chunk
        // and would show nothing for code inside `fn _f`'s own nested chunk —
        // see `disassemble_recursively`'s own doc comment for why).
        let src = "type Shape = Circle(Int) | Square(Int); fn _f(s) { match s { Circle(r) | Square(r) => r, _ => 0, } }";
        let (chunk, interner) = compile_program_chunk(src);
        let out = disassemble_recursively(&chunk, "test", &interner);
        let set_local_lines: Vec<&str> =
            out.lines().filter(|l| l.contains("OP_SET_LOCAL")).collect();
        // Not an exact-length assertion: `emit_tail_scope_exit` (unrelated,
        // pre-existing, untouched by this fix) also emits its own
        // `OP_SET_LOCAL` — once for this arm's own scope exit and once for
        // the whole `match`'s final scope exit — so the full disassembly
        // legitimately contains more than just the two alternatives' binds.
        // What's actually under test here is program order: each
        // alternative's own bind is compiled (and so disassembles) strictly
        // before either scope-exit's `OP_SET_LOCAL`, so the first two
        // `OP_SET_LOCAL` lines in the whole disassembly are always exactly
        // the two alternatives' binds of `r` — that's what the equality
        // check below actually pins down.
        assert!(
            set_local_lines.len() >= 2,
            "expected at least one OP_SET_LOCAL per alternative's bind of `r`: {out}"
        );
        assert_eq!(
            set_local_lines[0].split_whitespace().last(),
            set_local_lines[1].split_whitespace().last(),
            "both alternatives must write `r` into the same slot: {out}"
        );
    }

    #[test]
    fn an_or_pattern_with_a_binding_that_does_not_match_pops_its_reserved_slot() {
        // The specific bug a subagent review caught in an earlier version of
        // this fix: the reserved slot(s) are pushed unconditionally BEFORE
        // any alternative's test runs. If every alternative's test fails,
        // nothing pops them — `fail_jumps` jumps straight past the
        // success-path body and its `emit_tail_scope_exit` cleanup. This
        // can't be caught by a compile-time-only assertion (it's a genuine
        // stack-height/value bug, only observable by running the bytecode —
        // see Task 4's VM-level regression test for the real proof), but this
        // at least confirms the expected `OP_POP` is present in the
        // disassembly on the "all alternatives failed" path, right before the
        // jump to the next arm.
        let src = "type Shape = Circle(Int) | Square(Int) | Triangle(Int); fn _f(s) { match s { Circle(r) | Square(r) => r, _ => -1, } }";
        let (chunk, interner) = compile_program_chunk(src);
        let out = disassemble_recursively(&chunk, "test", &interner);
        assert!(
            out.contains("OP_POP"),
            "expected at least one OP_POP cleaning up the reserved (but never bound) \
             slot on the all-alternatives-failed path: {out}"
        );
    }

    #[test]
    fn a_match_arms_captured_local_flowing_into_a_nested_closure_does_not_corrupt_stack_bookkeeping(
    ) {
        // Probes the compile_match per-arm stack_depth bug class: a local
        // bound by one arm's pattern, captured by a lambda built in that
        // arm's body, must not leave `stack_depth` miscounted by the
        // (unreachable, per-pass) alternative arms that were also
        // compiled into the same linear stream.
        let src = "fn _f(x) { match x { n => { let g = || n; g }, } }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    // `compile` is defined in this same module (outer scope), not
    // imported from the crate root: `use ember_compile::compile;` (as
    // written in the task plan) doesn't resolve here because this test
    // module is compiled as part of the `ember-compile` crate itself,
    // not as an external consumer of it — self-referential `use`s by
    // crate name aren't available without declaring the crate as its
    // own dev-dependency. `use super::*;` above already brings `compile`
    // into scope, which is what every other helper in this module relies
    // on for `Compiler` et al.

    #[test]
    fn compile_hoists_fn_declarations_before_other_top_level_code() {
        // Task-plan literal test used `assert!(resolve_diags.is_empty())`,
        // but `x` here is a top-level `let` that's never read, which (per
        // `a_top_level_let_gets_dual_registration`'s comment above) the
        // resolver flags as an unused-variable *warning* — not an error.
        // `assert_no_errors` is this file's established way to check
        // "resolves cleanly" without also demanding zero warnings.
        let src = "let x = a(); fn a() { 1 }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert_no_errors(&resolve_diags);
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        let closure_line = out.lines().position(|l| l.contains("OP_CLOSURE")).unwrap();
        let call_line = out.lines().position(|l| l.contains("OP_CALL")).unwrap();
        assert!(
            closure_line < call_line,
            "a's OP_CLOSURE must be emitted before x's initializer calls it: {out}"
        );
    }

    #[test]
    fn compile_ends_the_top_level_chunk_with_return() {
        // See the comment in `compile_hoists_fn_declarations_before_other_top_level_code`:
        // `x` is an unread top-level `let`, which only warns, not errors.
        let src = "let x = 1;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert_no_errors(&resolve_diags);
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        assert!(
            out.trim_end().lines().last().unwrap().contains("OP_RETURN"),
            "{out}"
        );
    }

    #[test]
    fn compile_handles_mutual_recursion_between_top_level_fns() {
        let src = "fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } } fn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "{resolve_diags:?}");
        let proto = compile(&ast, &mut interner, &bindings, &stmts); // must not panic
        assert_eq!(proto.chunk.functions.len(), 2);
    }

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
        assert!(
            last_two[1].contains("OP_ADD"),
            "the last computed value must flow straight into Return, not through a Pop/Nil: {out}"
        );
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

    #[test]
    fn a_guards_failure_after_a_bound_pattern_pops_the_bound_local() {
        // Confirmed via the VM: before this fix, `Circle(r) if r > 100 => r,
        // _ => -1` matched via `Circle(5)` (guard false) returns `5`, not
        // `-1` — the bound-but-then-abandoned `r` leaks onto the stack and
        // becomes whatever the next arm's `Return` picks up. This is a
        // compile-time-only check that the expected cleanup `OP_POP` is
        // present; the VM-level test in Task 4 is the real proof.
        let src = "type Shape = Circle(Int); fn _f(s) { match s { Circle(r) if r > 100 => r, _ => -1, } }";
        let (chunk, interner) = compile_program_chunk(src);
        let out = disassemble_recursively(&chunk, "test", &interner);
        assert!(
            out.contains("OP_POP"),
            "expected an OP_POP cleaning up `r` on the guard-failure path: {out}"
        );
    }
}
