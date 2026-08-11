use crate::error::RuntimeError;
use crate::value::{trace_value, ClosureObj, UpvalueCell, Value};
use ember_bytecode::chunk::{Chunk, FunctionProto};
use ember_bytecode::op::Op;
use ember_gc::{Gc, GcHeap};
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
///
const MAX_FRAMES: usize = 1000;

/// Mirrors `ember_resolve::resolver::NATIVE_GLOBALS` (private to that
/// crate) and `ember_compile::compiler::NATIVE_GLOBAL_COUNT` — the
/// resolver seeds 8 native names into the top-level function's own scope
/// *before* any user code is resolved, via the same slot counter ordinary
/// `let`s use, so the physical VM stack needs 8 real positions reserved
/// for them too, or every top-level `Resolution::Local` slot (including
/// the user's own first top-level `let`) would be off by 8 relative to
/// what `ember-compile` assumed when it emitted `OP_GET_LOCAL`/`OP_SET_LOCAL`.
/// `Vm::new` fills these 8 slots with the real native function values (see
/// `crate::natives::NATIVES`), pushed in that same order, rather than
/// placeholders — this constant just names how many there are.
const NATIVE_GLOBAL_COUNT: usize = 8;

pub struct CallFrame {
    pub closure: Gc<ClosureObj>,
    pub ip: usize,
    pub slot_base: usize,
}

pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    /// Keyed by an interned `Gc<String>` name now, not `Rc<String>` — see
    /// `read_global_name` (a later task). Note the *keys* are marked as
    /// roots in `mark_roots` too, not just the values: a global's name
    /// string must stay alive exactly as long as the global itself does,
    /// and nothing else roots it once it's off the constant pool's own
    /// `Rc` and onto the GC heap via interning.
    globals: FxHashMap<Gc<String>, Value>,
    open_upvalues: Vec<Gc<UpvalueCell>>,
    gc: GcHeap,
}

#[derive(Debug)]
pub enum StepOutcome {
    Running,
    Done(Value),
}

impl Vm {
    pub fn new(script: FunctionProto) -> Self {
        let mut gc = GcHeap::new();
        let proto = Rc::new(script);
        let closure = gc.allocate(ClosureObj {
            proto,
            upvalues: Vec::new(),
        });
        let frame = CallFrame {
            closure,
            ip: 0,
            slot_base: 0,
        };
        // The 8 native function values, pushed in the exact order
        // `ember-resolve::NATIVE_GLOBALS` seeded their names — this both
        // pads the physical stack so ordinary top-level locals land where
        // the compiler assumed (see `NATIVE_GLOBAL_COUNT`'s doc comment)
        // and pre-registers them in `globals`, so a direct top-level
        // reference (`Resolution::Local`, reads straight off the stack)
        // and a nested-function reference (`Resolution::Global`, via
        // `globals`) both resolve to the same native.
        let mut stack = Vec::with_capacity(NATIVE_GLOBAL_COUNT);
        let mut globals = FxHashMap::default();
        for &(name, arity, func) in crate::natives::NATIVES {
            let native = Value::Native(Rc::new(crate::value::NativeFn { name, arity, func }));
            stack.push(native.clone());
            let key = gc.intern_str(name);
            globals.insert(key, native);
        }
        Vm {
            stack,
            frames: vec![frame],
            globals,
            open_upvalues: Vec::new(),
            gc,
        }
    }

    #[cfg(test)]
    pub(crate) fn stack_len_for_test(&self) -> usize {
        self.stack.len()
    }

    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, v: Value) {
        self.push(v);
    }

    #[cfg(test)]
    pub(crate) fn gc_mut_for_test(&mut self) -> &mut GcHeap {
        &mut self.gc
    }

    fn frame(&self) -> &CallFrame {
        self.frames
            .last()
            .expect("at least one frame while running")
    }

    fn frame_mut(&mut self) -> &mut CallFrame {
        self.frames
            .last_mut()
            .expect("at least one frame while running")
    }

    fn chunk(&self) -> &Chunk {
        &self.frame().closure.proto.chunk
    }

    fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    fn pop(&mut self) -> Value {
        self.stack
            .pop()
            .expect("stack underflow — a compiler bug, not a user error")
    }

    /// Inspects the stack without popping — used by `SetLocal`/`SetGlobal`,
    /// whose net-zero stack effect requires leaving the assigned value on
    /// top (assignment is an expression), and by future short-circuiting
    /// `and`/`or` handlers.
    fn peek(&self, distance_from_top: usize) -> &Value {
        let len = self.stack.len();
        &self.stack[len - 1 - distance_from_top]
    }

    /// A collection is checked for once, here, at the very top of every
    /// `step()` call — never inside `allocate`/`intern_str` themselves.
    /// Checking only at instruction boundaries means every opcode
    /// handler's whole body runs with the stack in a fully-rooted state
    /// from start to finish, so no handler needs its own temporary-root
    /// protection even when it does more than one allocation in a row
    /// (OP_CLOSURE's upvalue-capture loop, OP_MAKE_ADT's two interned-name
    /// lookups, both added in later tasks).
    fn maybe_collect(&mut self) {
        if !self.gc.should_collect() {
            return;
        }
        let stack = &self.stack;
        let frames = &self.frames;
        let open_upvalues = &self.open_upvalues;
        let globals = &self.globals;
        self.gc.collect(|tracer| {
            for v in stack {
                trace_value(v, tracer);
            }
            for f in frames {
                tracer.mark(f.closure);
            }
            for uv in open_upvalues {
                tracer.mark(*uv);
            }
            for (k, v) in globals.iter() {
                tracer.mark(*k);
                trace_value(v, tracer);
            }
        });
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
    /// interns into the GC heap rather than sharing the pooled `Rc`
    /// directly — the two `Value` types (compile-time constant pool vs.
    /// runtime heap) are no longer the same representation for strings.
    fn read_constant(&mut self) -> Value {
        let idx = self.read_u16();
        let pooled = self.chunk().constants[idx as usize].clone();
        self.const_to_value(pooled)
    }

    /// Reads a `u16` constant-pool index and expects the pooled value to
    /// be a `Str`, interning it — every global/field/type/variant name
    /// operand in the whole `Op` set is read this way. Panics on a
    /// non-`Str` constant: that would mean `ember-compile` emitted a name
    /// operand pointing at the wrong kind of constant, a compiler bug this
    /// crate has no responsibility to recover from.
    fn read_global_name(&mut self) -> Gc<String> {
        let idx = self.read_u16();
        self.str_constant(idx)
    }

    /// Resolves a *known* constant-pool index to its pooled string,
    /// interned — unlike `read_global_name`, which reads the index off
    /// `ip` itself, this is for opcodes (`MakeAdt`, `TestVariant`) that
    /// already have the index in hand and just need it turned into a
    /// name.
    fn str_constant(&mut self, idx: u16) -> Gc<String> {
        let s = match &self.chunk().constants[idx as usize] {
            ember_bytecode::value::Value::Str(s) => Rc::clone(s),
            other => panic!("name constant must be a string, found {other:?}"),
        };
        self.gc.intern_str(&s)
    }

    fn const_to_value(&mut self, c: ember_bytecode::value::Value) -> Value {
        match c {
            ember_bytecode::value::Value::Nil => Value::Nil,
            ember_bytecode::value::Value::Bool(b) => Value::Bool(b),
            ember_bytecode::value::Value::Int(n) => Value::Int(n),
            ember_bytecode::value::Value::Float(f) => Value::Float(f),
            ember_bytecode::value::Value::Str(s) => Value::Str(self.gc.intern_str(&s)),
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
    ///
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
        RuntimeError {
            message: message.into(),
            line,
            trace,
        }
    }

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

    pub fn step(&mut self) -> Result<StepOutcome, RuntimeError> {
        self.maybe_collect();
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
                // started) along with every arg/local it pushed — a
                // future task adds OP_CALL and explains this `-1` fully.
                self.stack.truncate(frame.slot_base - 1);
                self.push(result);
            }
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
                    other => {
                        return Err(self.runtime_error(format!("expected Bool, found {other:?}")))
                    }
                }
            }
            Op::Negate => {
                let a = self.pop();
                match a {
                    Value::Int(n) => match n.checked_neg() {
                        Some(r) => self.push(Value::Int(r)),
                        None => {
                            return Err(self.runtime_error(format!("integer overflow negating {n}")))
                        }
                    },
                    Value::Float(f) => self.push(Value::Float(-f)),
                    other => {
                        return Err(
                            self.runtime_error(format!("invalid operand for unary -: {other:?}"))
                        )
                    }
                }
            }
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
                        self.frames.push(CallFrame {
                            closure: c,
                            ip: 0,
                            slot_base,
                        });
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
                        let result = (n.func)(&args, line, &mut self.gc)
                            .map_err(|e| self.attach_trace(e))?;
                        self.push(result);
                    }
                    other => return Err(self.runtime_error(format!("cannot call {other:?}"))),
                }
            }
            Op::Closure => {
                let idx = self.read_u16();
                let proto = Rc::clone(&self.chunk().functions[idx as usize]);
                let mut upvalues = Vec::with_capacity(proto.upvalues.len());
                for desc in &proto.upvalues {
                    let uv = if desc.is_local {
                        let slot = self.frame().slot_base + desc.index as usize;
                        self.capture_upvalue(slot)
                    } else {
                        self.frame().closure.upvalues[desc.index as usize]
                    };
                    upvalues.push(uv);
                }
                let closure = self.gc.allocate(ClosureObj { proto, upvalues });
                self.push(Value::Closure(closure));
            }
            Op::GetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = self.frame().closure.upvalues[idx];
                let v = match &*uv.0.borrow() {
                    crate::value::Upvalue::Open(slot) => self.stack[*slot].clone(),
                    crate::value::Upvalue::Closed(v) => v.clone(),
                };
                self.push(v);
            }
            Op::SetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = self.frame().closure.upvalues[idx];
                let v = self.peek(0).clone();
                let open_slot = match &*uv.0.borrow() {
                    crate::value::Upvalue::Open(slot) => Some(*slot),
                    crate::value::Upvalue::Closed(_) => None,
                };
                match open_slot {
                    Some(slot) => self.stack[slot] = v,
                    None => *uv.0.borrow_mut() = crate::value::Upvalue::Closed(v),
                }
            }
            Op::CloseUpvalue => {
                let slot = self.stack.len() - 1;
                self.close_upvalues(slot);
                self.pop();
            }
            Op::CloseUpvaluesFrom => {
                let local_slot = self.read_u8() as usize;
                let physical = self.frame().slot_base + local_slot;
                self.close_upvalues(physical);
            }
            Op::MakeList => {
                let count = self.read_u16() as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.pop());
                }
                items.reverse(); // popped in reverse of push order
                let list = self.gc.allocate(crate::value::ListObj(RefCell::new(items)));
                self.push(Value::List(list));
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
                        let v = fields.0.borrow().get(&name).cloned();
                        match v {
                            Some(v) => self.push(v),
                            None => return Err(self.runtime_error(format!("no field `{name}`"))),
                        }
                    }
                    other => {
                        return Err(self
                            .runtime_error(format!("cannot access field `{name}` on {other:?}")))
                    }
                }
            }
            Op::SetField => {
                let name = self.read_global_name();
                let value = self.pop();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        fields.0.borrow_mut().insert(name, value.clone());
                        self.push(value);
                    }
                    other => {
                        return Err(
                            self.runtime_error(format!("cannot set field `{name}` on {other:?}"))
                        )
                    }
                }
            }
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
                let fields: FxHashMap<Gc<String>, Value> = pairs.into_iter().collect();
                let fields = self
                    .gc
                    .allocate(crate::value::RecordFields(RefCell::new(fields)));
                self.push(Value::Record {
                    name: type_name,
                    fields,
                });
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
                fields.reverse(); // positional order matters
                let adt = self.gc.allocate(crate::value::AdtValue {
                    type_name,
                    variant,
                    fields,
                });
                self.push(Value::Adt(adt));
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
                    other => {
                        return Err(self.runtime_error(format!("cannot destructure {other:?}")))
                    }
                }
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

    /// Hoists every open upvalue at or above `from` from the stack to the
    /// heap. Not kept sorted by slot (unlike some VM designs' intrusive
    /// linked-list sketches, which rely on that ordering for an early-exit
    /// scan) — this does a full drain-and-filter of `open_upvalues` on
    /// every close regardless of order, so sorting would add bookkeeping
    /// without buying anything.
    ///
    /// Entries that are already `Closed` (from an earlier call — e.g. one
    /// `OP_CLOSE_UPVALUES_FROM` pre-closing something this call's own
    /// `OP_RETURN`-driven close later walks over again) are dropped here
    /// too, not just left alone: `open_upvalues` exists solely so
    /// `capture_upvalue` can find and reuse still-*open* entries, and so
    /// this method can find slots that still need closing. A `Closed`
    /// entry serves neither purpose ever again once its `Rc` is held by
    /// whatever closure captured it — keeping it in this list forever
    /// would just leak one `Vec` slot per capture for the life of the VM.
    fn close_upvalues(&mut self, from: usize) {
        let mut keep = Vec::with_capacity(self.open_upvalues.len());
        for uv in self.open_upvalues.drain(..) {
            let open_slot = match &*uv.0.borrow() {
                crate::value::Upvalue::Open(s) => Some(*s),
                crate::value::Upvalue::Closed(_) => None,
            };
            match open_slot {
                Some(s) if s >= from => {
                    let value = self.stack[s].clone();
                    *uv.0.borrow_mut() = crate::value::Upvalue::Closed(value);
                }
                Some(_) => keep.push(uv),
                None => {}
            }
        }
        self.open_upvalues = keep;
    }

    /// Searches `open_upvalues` for an existing entry at `slot` and reuses
    /// it — the mechanism that makes shared capture work: two closures
    /// over the same variable end up holding `Rc::clone`s of the identical
    /// `RefCell`, so a write through one is visible through the other.
    fn capture_upvalue(&mut self, slot: usize) -> Gc<crate::value::UpvalueCell> {
        for uv in &self.open_upvalues {
            if let crate::value::Upvalue::Open(s) = &*uv.0.borrow() {
                if *s == slot {
                    return *uv;
                }
            }
        }
        let uv = self.gc.allocate(crate::value::UpvalueCell(RefCell::new(
            crate::value::Upvalue::Open(slot),
        )));
        self.open_upvalues.push(uv);
        uv
    }

    fn index_get(&self, base: Value, index: Value) -> Result<Value, RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let l = l.0.borrow();
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
                let mut l = l.0.borrow_mut();
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

    fn binary_arith(&mut self, op: Op, l: Value, r: Value) -> Result<Value, RuntimeError> {
        let overflow = |op_name: &str, a: i64, b: i64| {
            self.runtime_error(format!("integer overflow: {a} {op_name} {b}"))
        };
        match (op, l, r) {
            (Op::Add, Value::Int(a), Value::Int(b)) => a
                .checked_add(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("+", a, b)),
            (Op::Sub, Value::Int(a), Value::Int(b)) => a
                .checked_sub(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("-", a, b)),
            (Op::Mul, Value::Int(a), Value::Int(b)) => a
                .checked_mul(b)
                .map(Value::Int)
                .ok_or_else(|| overflow("*", a, b)),
            (Op::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(self.runtime_error("division by zero"));
                }
                a.checked_div(b)
                    .map(Value::Int)
                    .ok_or_else(|| overflow("/", a, b))
            }
            (Op::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err(self.runtime_error("division by zero"));
                }
                a.checked_rem(b)
                    .map(Value::Int)
                    .ok_or_else(|| overflow("%", a, b))
            }
            (Op::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Op::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Op::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Op::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Op::Add, Value::Str(a), Value::Str(b)) => {
                Ok(Value::Str(self.gc.intern_str(&format!("{a}{b}"))))
            }
            (op, l, r) => {
                Err(self.runtime_error(format!("invalid operands for {op:?}: {l:?}, {r:?}")))
            }
        }
    }

    fn compare(&self, op: Op, l: Value, r: Value) -> Result<Value, RuntimeError> {
        match (op, l, r) {
            (Op::Less, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (Op::Greater, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (Op::Less, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            (Op::Greater, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            (op, l, r) => {
                Err(self.runtime_error(format!("invalid operands for {op:?}: {l:?}, {r:?}")))
            }
        }
    }
}

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
        // Baseline, not 0: `Vm::new` seeds `NATIVE_GLOBAL_COUNT` placeholder
        // slots onto the physical stack before any user code runs — see
        // that constant's doc comment.
        let base = vm.stack_len_for_test();
        assert!(matches!(vm.step(), Ok(StepOutcome::Running)));
        assert_eq!(vm.stack_len_for_test(), base + 1);
        assert!(matches!(vm.step(), Ok(StepOutcome::Running)));
        assert_eq!(vm.stack_len_for_test(), base + 2);
        match vm.step() {
            Ok(StepOutcome::Done(Value::Bool(true))) => {}
            other => panic!("expected Done(Bool(true)), got {other:?}"),
        }
    }

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
            let a = c.add_constant(ember_bytecode::value::Value::Str(Rc::new(
                "foo".to_string(),
            )));
            let b = c.add_constant(ember_bytecode::value::Value::Str(Rc::new(
                "bar".to_string(),
            )));
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

    #[test]
    fn get_local_and_set_local_read_and_write_the_frame_relative_slot() {
        // Slot 8, not 0: `Vm::new` reserves physical slots 0-7 for
        // `NATIVE_GLOBAL_COUNT` placeholders, so the top-level frame's
        // first *real* local — which is all this hand-built chunk is
        // standing in for — lives at slot 8, exactly like the real
        // resolver/compiler would assign it. Slot 0 would silently read
        // and write a native placeholder instead, defeating the test.
        let result = run_ops(|c| {
            let a = int_const(c, 1);
            c.write_op(Op::Constant, 1); // slot 8
            c.write_u16(a, 1);
            let b = int_const(c, 99);
            c.write_op(Op::Constant, 1); // pushes the new value to assign
            c.write_u16(b, 1);
            c.write_op(Op::SetLocal, 1); // slot 8 = 99, leaves 99 on top too
            c.write_u8(8, 1);
            c.write_op(Op::Pop, 1); // discard the SetLocal duplicate
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
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
            let name = c.add_constant(ember_bytecode::value::Value::Str(Rc::new(
                "nope".to_string(),
            )));
            c.write_op(Op::GetGlobal, 1);
            c.write_u16(name, 1);
        });
        assert!(result.is_err());
    }

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
        // using slot 8 as the counter, exercising OP_LOOP's backward jump.
        // Slot 8, not 0: `Vm::new` reserves physical slots 0-7 for
        // `NATIVE_GLOBAL_COUNT` placeholders, so the top-level frame's
        // first real local lives at slot 8, matching what the real
        // resolver/compiler would assign.
        let result = run_ops(|c| {
            let zero = int_const(c, 0);
            c.write_op(Op::Constant, 1); // slot 8 = 0
            c.write_u16(zero, 1);

            let loop_start = c.code.len();
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
            let three = int_const(c, 3);
            c.write_op(Op::Constant, 1);
            c.write_u16(three, 1);
            c.write_op(Op::Less, 1);
            let to_end = c.code.len();
            c.write_op(Op::JumpIfFalse, 1);
            c.write_u16(0xFFFF, 1);

            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
            let one = int_const(c, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(one, 1);
            c.write_op(Op::Add, 1);
            c.write_op(Op::SetLocal, 1);
            c.write_u8(8, 1);
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
            c.write_u8(8, 1);
        });
        assert!(matches!(result, Ok(Value::Int(3))));
    }

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
        FunctionProto {
            chunk,
            arity: 1,
            upvalues: vec![],
            name: interner.intern("callee"),
        }
    }

    #[test]
    fn calling_a_closure_pushes_a_frame_and_the_result_replaces_the_whole_call() {
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner));

        let proto = script(|c| {
            let five = c.add_constant(ember_bytecode::value::Value::Int(5));
            c.write_op(Op::Constant, 1);
            c.write_u16(five, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(1, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        let closure = Value::Closure(vm.gc_mut_for_test().allocate(ClosureObj {
            proto: callee,
            upvalues: vec![],
        }));
        // Test-only: seed the stack with the callee BEFORE execution starts,
        // exactly where the script's own bytecode expects to find it (this
        // sidesteps needing OP_CLOSURE, built in a future task, just to
        // exercise OP_CALL's own frame mechanics in isolation).
        // Baseline, not 0: `Vm::new` seeds `NATIVE_GLOBAL_COUNT` permanent
        // placeholder slots before any script code runs — that floor
        // persists for the VM's whole lifetime, so "nothing else" means
        // nothing beyond it, not a literally empty stack.
        let base = vm.stack_len_for_test();
        vm.push_for_test(closure);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(6)));
        assert_eq!(
            vm.stack_len_for_test(),
            base,
            "the call must leave exactly its result and nothing else"
        );
    }

    #[test]
    fn calling_with_the_wrong_arity_is_a_runtime_error() {
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner)); // expects 1 arg
        let proto = script(|c| {
            c.write_op(Op::Call, 1);
            c.write_u8(0, 1); // called with 0 args instead of 1
        });
        let mut vm = Vm::new(proto);
        let closure = Value::Closure(vm.gc_mut_for_test().allocate(ClosureObj {
            proto: callee,
            upvalues: vec![],
        }));
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
        fn double(
            args: &[Value],
            _line: u32,
            _gc: &mut ember_gc::GcHeap,
        ) -> Result<Value, RuntimeError> {
            match args[0] {
                Value::Int(n) => Ok(Value::Int(n * 2)),
                _ => unreachable!(),
            }
        }
        let native = Value::Native(Rc::new(crate::value::NativeFn {
            name: "double",
            arity: 1,
            func: double,
        }));
        let proto = script(|c| {
            let five = c.add_constant(ember_bytecode::value::Value::Int(5));
            c.write_op(Op::Constant, 1);
            c.write_u16(five, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(1, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        vm.push_for_test(native);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(10)));
    }

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
    fn native_functions_are_reachable_through_the_whole_pipeline() {
        let result = compile_and_run("let xs = [1, 2]; push(xs, 3); len(xs);").unwrap();
        assert!(matches!(result, Value::Int(3)));
    }

    #[test]
    fn a_bare_top_level_native_call_resolves_as_a_local_not_a_global() {
        // Unlike the previous test, this calls `len` directly from a bare
        // top-level expression — `ember-resolve` resolves that as
        // `Resolution::Local` (reading straight off the physical stack
        // slot `Vm::new` pre-seeds), not `Resolution::Global` via the
        // `globals` map. This is the exact case the stack-padding fix in
        // `Vm::new` exists for.
        let result = compile_and_run("len([1, 2, 3]);").unwrap();
        assert!(matches!(result, Value::Int(3)));
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
        assert!(
            matches!(result, Value::Int(1)),
            "b's own counter must start fresh at 1, not inherit a's 2"
        );
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
        // `OP_MAKE_RECORD` (struct-literal construction) is a later task's
        // opcode, so — exactly like
        // `calling_a_closure_pushes_a_frame_and_the_result_replaces_the_whole_call`
        // seeds a closure directly ahead of `OP_CLOSURE` existing — this
        // seeds a `Value::Record` straight onto the stack, to exercise
        // `OP_GET_FIELD`/`OP_SET_FIELD` (this task's actual scope) in
        // isolation from record construction.
        let proto = script(|c| {
            let x_name =
                c.add_constant(ember_bytecode::value::Value::Str(Rc::new("x".to_string())));
            let forty_two = int_const(c, 42);

            // p.x = 42; (leaves 42 on top, per assignment-as-expression)
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1); // slot 8 — see the `base` assertion below
            c.write_op(Op::Constant, 1);
            c.write_u16(forty_two, 1);
            c.write_op(Op::SetField, 1);
            c.write_u16(x_name, 1);
            c.write_op(Op::Pop, 1); // discard the statement's value

            // p.x;
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
            c.write_op(Op::GetField, 1);
            c.write_u16(x_name, 1);
            c.write_op(Op::Return, 1);
        });

        let mut vm = Vm::new(proto);
        let gc = vm.gc_mut_for_test();
        let mut fields = FxHashMap::default();
        fields.insert(gc.intern_str("x"), Value::Int(1));
        let record = Value::Record {
            name: gc.intern_str("P"),
            fields: gc.allocate(crate::value::RecordFields(RefCell::new(fields))),
        };
        // Slot 8, not 0: `Vm::new` reserves physical slots 0-7 for
        // `NATIVE_GLOBAL_COUNT` placeholders (see that constant's doc
        // comment), so the record seeded right after them lands at slot 8,
        // matching the `GetLocal` operand hardcoded above.
        let base = vm.stack_len_for_test();
        assert_eq!(
            base, 8,
            "record must land where the GetLocal operand above expects it"
        );
        vm.push_for_test(record);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(42)));
    }

    #[test]
    fn struct_literal_construction_and_field_read() {
        // `_P`, not `P`: `Expr::Struct` never marks its type name as used
        // against the declared struct binding (a pre-existing resolver
        // gap — see `ember-compile`'s
        // `assigning_to_a_field_target_compiles_base_value_then_set_field`
        // for the same workaround), so an un-prefixed name would leave the
        // declaration looking unused and trip `compile_and_run`'s
        // `resolve_diags.is_empty()` assertion with a warning.
        let src = "struct _P { x: Int, y: Int } let p = _P { x: 3, y: 4 }; p.x + p.y;";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(7)));
    }

    #[test]
    fn nullary_adt_variant_constructs_via_match() {
        // `_Shape`/`_Circle`, not `Shape`/`Circle`: neither the enclosing
        // type name nor a variant that's only ever referenced from pattern
        // position (never constructed as an expression) gets marked
        // "used" by the resolver — the same pre-existing gap as
        // `Expr::Struct` never marking its type name used (see
        // `struct_literal_construction_and_field_read`). `Origin`, in
        // contrast, needs no prefix: the scrutinee `match Origin { .. }`
        // really does read the global, real use. Also note the first arm's
        // pattern is `_Origin`, a fresh (shadowing, unused, hence
        // underscore-prefixed) *binding* rather than a tag test: a bare
        // capitalized identifier with no trailing `(`/`{` always parses as
        // `Pattern::Bind`, never `Pattern::Ctor` — see
        // `ember-parser`'s `pattern_primary` — so this arm matches
        // unconditionally regardless of the scrutinee's actual variant,
        // exactly like `_` would. That's fine: this test's job is proving
        // `OP_MAKE_ADT` correctly constructs and stores a nullary
        // singleton at declaration time; a real `OP_TEST_VARIANT` tag
        // test on an ADT is exercised below by the payload-carrying
        // `Pair`/`Circle`/`Square` tests, whose parenthesized ctor
        // patterns do parse as `Pattern::Ctor`.
        let src = "
            type _Shape = _Circle(Float) | Origin;
            match Origin {
                _Origin => 1,
                _Circle(_) => 2,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(1)));
    }

    #[test]
    fn payload_adt_variant_constructs_via_a_callable_ctor_with_fields_in_order() {
        // `_Pair`, not `Pair`, for the *type* name only — see
        // `nullary_adt_variant_constructs_via_match`'s comment for why an
        // ADT type name is never marked used. The `Pair` *variant*/ctor
        // itself needs no prefix: `Pair(10, 20)` really does call it as an
        // expression.
        let src = "
            type _Pair = Pair(Int, Int);
            match Pair(10, 20) {
                Pair(a, b) => a - b,
            }
        ";
        let result = compile_and_run(src).unwrap();
        assert!(
            matches!(result, Value::Int(-10)),
            "must be 10 - 20, not 20 - 10 (payload order matters)"
        );
    }

    #[test]
    fn ctor_pattern_tests_the_tag_and_destructures_by_position() {
        // `_Shape`/`_Circle`, not `Shape`/`Circle` — see
        // `nullary_adt_variant_constructs_via_match`'s comment: the type
        // name is never marked used, and `Circle` here is only ever
        // referenced from pattern position (never constructed), unlike
        // `Square`, which is genuinely called via `Square(4.0)`. Both
        // patterns here (`_Circle(r)`, `Square(side)`) parse as real
        // `Pattern::Ctor`s (parenthesized), so this test does exercise a
        // genuine `OP_TEST_VARIANT` tag check plus `OP_DESTRUCTURE`.
        let src = "
            type _Shape = _Circle(Float) | Square(Float);
            fn area(s) {
                match s {
                    _Circle(r) => 3.14 * r * r,
                    Square(side) => side * side,
                }
            }
            area(Square(4.0));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Float(f) if (f - 16.0).abs() < 1e-9));
    }

    // Naming convention throughout this task's tests (already established
    // elsewhere in this file — see the existing
    // `nullary_adt_variant_constructs_via_match`/`struct_literal_construction_and_field_read`
    // tests): `compile_and_run`'s own `assert!(resolve_diags.is_empty())` is
    // strict, catching warnings too, and this resolver never marks a type
    // name "used" from ANY reference, or a constructor "used" from PATTERN
    // position specifically (only from being actually constructed as an
    // expression) — so every variant that's only ever pattern-matched in a
    // given test, and every type name, needs a `_` prefix to avoid an
    // "unused" warning tripping that assertion. Each test below has already
    // been checked against which constructors it actually CONSTRUCTS (via a
    // real call like `f(Circle(5))`) vs. only pattern-matches.

    #[test]
    fn or_pattern_repeated_name_matches_correctly_via_the_first_alternative() {
        // The original reported reproduction: `Circle(r) | Square(r) => r`,
        // matched via Circle. Before the fix, this panicked with an
        // out-of-bounds VM stack read (Circle's bind wrote into a slot the
        // arm body never read, since the resolver's `lookup` returned
        // whichever alternative's `declare` ran LAST in source order).
        // `Square` is only ever pattern-matched here (never constructed), so
        // it needs the `_` prefix; `Circle` is genuinely constructed below.
        let src = "
            type _Shape = Circle(Int) | _Square(Int);
            fn area(s) {
                match s {
                    Circle(r) | _Square(r) => r,
                }
            }
            area(Circle(5));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(5)));
    }

    #[test]
    fn or_pattern_repeated_name_matches_correctly_via_the_second_alternative() {
        // Same pattern, matched via the OTHER alternative — must independently
        // confirm both branches write into the slot the arm body actually
        // reads. `Circle` is only ever pattern-matched here (never
        // constructed), so IT needs the `_` prefix this time — the reverse of
        // the previous test.
        let src = "
            type _Shape = _Circle(Int) | Square(Int);
            fn area(s) {
                match s {
                    _Circle(r) | Square(r) => r,
                }
            }
            area(Square(7));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(7)));
    }

    #[test]
    fn or_pattern_with_a_nested_binding_at_different_depths_per_alternative() {
        // `r` sits at a different structural nesting depth in each
        // alternative (direct child of Circle, nested one level inside
        // Square's own Pair payload) — proves the fix is name-keyed, not
        // positional, since a "reset local_count per alternative" fix
        // (simpler, but insufficient) would put these two `r`s in different
        // slots. `Circle` is only pattern-matched (never constructed); `Pair`
        // and `Square` both ARE constructed below.
        let src = "
            type _Pair = Pair(Int, Int);
            type _Shape = _Circle(Int) | Square(_Pair);
            fn f(s) {
                match s {
                    _Circle(r) | Square(Pair(_, r)) => r,
                }
            }
            f(Square(Pair(1, 9)));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(9)));
    }

    #[test]
    fn or_pattern_with_no_repeated_name_still_compiles_and_runs_correctly() {
        // The broader (non-repeated-name) half of the original bug:
        // `local_count` drifted once per alternative that bound ANY name, not
        // just once per repeated occurrence — this exercises TWO distinct
        // reserved slots (`_r`, `_s`) rather than one shared one.
        //
        // The body deliberately does NOT read either bound name: this
        // language doesn't check that every alternative of an `Or`-pattern
        // binds the same names (a documented, deferred usability nicety, not
        // a soundness issue — see `declare_pattern_bindings`'s own comment in
        // `ember-resolve`), so whichever name the NON-matching alternative
        // would have bound holds a meaningless placeholder value on any given
        // run — reading it wouldn't test anything well-defined. Both `_r` and
        // `_s` are underscore-prefixed since they're genuinely unused by
        // design here (and `_s`, not `s`, specifically to avoid shadowing the
        // outer parameter `s` with a same-named, differently-meant binding).
        // Instead, this test proves the SLOT ACCOUNTING itself is correct by
        // checking that a local declared AFTER the match (`extra`) still
        // reads back correctly — the real proof that no slot leaked into
        // subsequent code. `Square` is only pattern-matched here, so it needs
        // the `_` prefix; `Circle` is constructed below.
        let src = "
            type _Shape = Circle(Int) | _Square(Int);
            fn f(s) {
                let extra = 42;
                match s {
                    Circle(_r) | _Square(_s) => 1,
                };
                extra
            }
            f(Circle(3));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(42)));
    }

    #[test]
    fn an_or_pattern_arm_that_does_not_match_leaves_the_stack_correct_for_the_next_arm() {
        // Proves Task 2's leaked-reservation fix: this Or-pattern's
        // alternatives never match `Triangle`, so control must fall through
        // to the wildcard arm with a clean stack. Before the fix, the
        // reserved (never-bound) slot for `r` was never popped on this path,
        // corrupting the function's return value. `Circle`/`Square` are only
        // ever pattern-matched (never constructed); `Triangle` is constructed
        // below.
        let src = "
            type _Shape = _Circle(Int) | _Square(Int) | Triangle(Int);
            fn f(s) {
                match s {
                    _Circle(r) | _Square(r) => r,
                    _ => -1,
                }
            }
            f(Triangle(9));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(-1)));
    }

    #[test]
    fn a_guards_failure_after_a_non_or_bind_falls_through_with_a_clean_stack() {
        // Proves Task 3's fix: the ORIGINAL reproduction found while
        // investigating Task 2 — `r` is really bound (Circle matched), but
        // the guard is false, so control must fall through to the wildcard
        // arm with `r` popped, not leaked. `Circle` is constructed below, no
        // prefix needed.
        let src = "
            type _Shape = Circle(Int);
            fn f(s) {
                match s {
                    Circle(r) if r > 100 => r,
                    _ => -1,
                }
            }
            f(Circle(5));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(-1)));
    }

    #[test]
    fn a_guards_failure_after_an_or_pattern_bind_falls_through_with_a_clean_stack() {
        // The combined case: Task 2's reserved-slot bind AND Task 3's
        // guard-failure cleanup must compose correctly. `Square` is only
        // ever pattern-matched here (never constructed), so it needs the `_`
        // prefix; `Circle` is constructed below.
        let src = "
            type _Shape = Circle(Int) | _Square(Int);
            fn f(s) {
                match s {
                    Circle(r) | _Square(r) if r > 100 => r,
                    _ => -1,
                }
            }
            f(Circle(5));
        ";
        let result = compile_and_run(src).unwrap();
        assert!(matches!(result, Value::Int(-1)));
    }

    #[test]
    fn record_pattern_tests_by_name() {
        // `_P`, not `P` — see `struct_literal_construction_and_field_read`
        // for why the struct type name needs the underscore prefix. The
        // scrutinee is bound to `p` first rather than written inline
        // (`match _P { x: 5 } { ... }`) because `match_expr` parses its
        // scrutinee with struct literals disambiguated away from the
        // match's own opening `{`, so there's no way to write a struct
        // literal directly as a match scrutinee (mirrors
        // `ember-compile`'s `a_record_pattern_uses_get_field_by_name_not_destructure`).
        let src = "
            struct _P { x: Int }
            let p = _P { x: 5 };
            match p {
                _P { x } => x,
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

    #[test]
    fn heap_size_stays_bounded_in_a_long_running_allocation_loop() {
        let src = "
            let mut i = 0;
            let mut last = \"\";
            while i < 5000 {
                last = str(i);
                i = i + 1;
            }
            last;
        ";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "resolve diags: {resolve_diags:?}");
        let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
        let mut vm = Vm::new(proto);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Str(s) if *s == "4999"));
        assert!(
            vm.gc_mut_for_test().bytes_allocated() < 200_000,
            "5000 iterations each discarding the previous `last` must not retain all 5000 \
             strings — got {} bytes allocated, collection is not reclaiming discarded ones",
            vm.gc_mut_for_test().bytes_allocated()
        );
    }

    #[test]
    fn let_with_a_block_initializer_containing_one_inner_let_works() {
        let v = compile_and_run("let y = { let a = 1; a }; y;").unwrap();
        assert_eq!(v, Value::Int(1));
    }

    #[test]
    fn let_with_a_block_initializer_containing_two_inner_lets_works() {
        let v = compile_and_run("let y = { let a = 1; let b = 2; a + b }; y;").unwrap();
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn let_with_an_if_else_initializer_containing_an_inner_let_works() {
        let v = compile_and_run("let y = if true { let a = 10; a } else { 0 }; y;").unwrap();
        assert_eq!(v, Value::Int(10));
    }

    #[test]
    fn let_with_a_nested_scope_initializer_inside_a_for_loop_body_works() {
        // Exercises the fix under active `slot_shifts` (for-loop desugaring
        // shifts physical slot numbers) — no pre-existing test puts a `let`
        // inside a `for`-loop body at all, so this is new coverage, not just
        // a regression check.
        let v = compile_and_run(
            "let xs = [1, 2, 3]; let mut total = 0; for x in xs { let y = { let a = x; a + 1 }; total = total + y; } total;",
        )
        .unwrap();
        assert_eq!(v, Value::Int(9));
    }
}
