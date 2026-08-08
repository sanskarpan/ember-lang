# ember Phase 8 Implementation Plan — Bytecode & Compiler

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `Op`/`Chunk`/`FunctionProto`/disassembler in `ember-bytecode`, and the single-pass AST-to-bytecode compiler in `ember-compile`, per `SPEC.md §11` and `CHECKLIST.md`'s Phase 8. Execution-free — everything is tested via disassembly, since no VM exists until Phase 9.

**Architecture:** `ember-bytecode` owns the bytecode format and a minimal, GC-free constant-pool `Value` (`Nil`/`Bool`/`Int`/`Float`/`Str` only — `Obj`/GC-backed variants arrive in Phase 9/10). `ember-compile` depends on `ember-ast` and `ember-resolve` (not `ember-types` — opcodes are generic/untyped, checked at runtime later) and is the first phase to actually use `ember-resolve`'s slot allocation, sitting unused since Phase 4.

**A key design decision, spelled out precisely since nothing in `SPEC.md`/`CHECKLIST.md` addresses it directly:** the top-level script compiles as its own function (matching `ember_resolve::FunctionId::TopLevel`) with **real local slots** for its own declarations, exactly like any other function — `Resolution::Local{slot}` always means `OP_GET_LOCAL`/`OP_SET_LOCAL`, unconditionally, everywhere, with zero special-casing based on which function is being compiled. `Resolution::Global{symbol}` only ever arises (per the resolver's own design) when a *nested* function references a name living in the top-level script's own outermost scope — reaching across frames needs a name-based lookup, not a slot offset, since a nested function's frame doesn't share stack space with the script's frame. To make that reachable, every top-level `Stmt::Fn`/`Stmt::Let` declaration **also** emits `OP_DEFINE_GLOBAL` right after establishing its value (peeking, not popping, so the value still becomes that declaration's own local slot too) — top-level bindings are **dual-registered**: once as an ordinary local slot for the script's own same-frame references, once in the VM's global name table for nested functions' cross-frame references. This requires no "is this the top-level frame" branching anywhere in the compiler — it's a fixed, uniform rule (every `Resolution` variant maps to exactly one opcode family, always; every top-level declaration additionally emits one `OP_DEFINE_GLOBAL`).

**Tech Stack:** Rust, `#[repr(u8)]` opcodes, run-length-encoded line info.

---

## Task 1: Scaffold the `ember-bytecode` crate

**Files:**
- Modify: `crates/ember-bytecode/Cargo.toml`
- Modify: `crates/ember-bytecode/src/lib.rs`

- [ ] **Step 1: Write the manifest**

```toml
[package]
name = "ember-bytecode"
version.workspace = true
edition.workspace = true

[dependencies]
ember-ast = { path = "../ember-ast" }
ember-resolve = { path = "../ember-resolve" }
```

- [ ] **Step 2: Declare the module layout**

```rust
pub mod chunk;
pub mod disasm;
pub mod op;
pub mod value;
```

- [ ] **Step 3: Create empty stub files and verify the build**

```bash
touch crates/ember-bytecode/src/chunk.rs crates/ember-bytecode/src/disasm.rs crates/ember-bytecode/src/op.rs crates/ember-bytecode/src/value.rs
```

Run: `source "$HOME/.cargo/env" && cargo build -p ember-bytecode`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-bytecode Cargo.lock
git commit -m "Scaffold ember-bytecode crate module layout"
```

---

## Task 2: `value.rs` — the constant-pool `Value`

**Files:**
- Modify: `crates/ember-bytecode/src/value.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_compare_structurally() {
        assert_eq!(Value::Int(1), Value::Int(1));
        assert_ne!(Value::Int(1), Value::Int(2));
        assert_eq!(Value::Str(std::rc::Rc::new("x".to_string())), Value::Str(std::rc::Rc::new("x".to_string())));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-bytecode values_compare_structurally`
Expected: FAIL to compile — `Value` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
use std::rc::Rc;

/// The constant pool's value type — deliberately minimal. Only literal,
/// immutable, poolable values ever belong in a constant pool; closures,
/// lists, and records are built at runtime via opcodes (`Closure`,
/// `MakeList`, `MakeRecord`, `MakeAdt`), never pooled. Phase 9/10 extend
/// this with a GC-backed `Obj` variant once `ember-gc` exists — they
/// don't replace it; every constant this phase ever pools stays
/// representable exactly as-is.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-bytecode`
Expected: PASS. Run `cargo clippy -p ember-bytecode --all-targets -- -D warnings` and `cargo fmt -p ember-bytecode -- --check` too.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-bytecode
git commit -m "Add the constant-pool Value type"
```

---

## Task 3: `op.rs` — `Op`

**Files:**
- Modify: `crates/ember-bytecode/src/op.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_round_trips_through_as_u8_and_from_u8() {
        let all = [
            Op::Constant, Op::Nil, Op::True, Op::False, Op::Pop,
            Op::GetLocal, Op::SetLocal, Op::GetGlobal, Op::SetGlobal, Op::DefineGlobal,
            Op::GetUpvalue, Op::SetUpvalue, Op::CloseUpvalue,
            Op::GetField, Op::SetField, Op::GetIndex, Op::SetIndex,
            Op::Equal, Op::Greater, Op::Less, Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Mod, Op::Not, Op::Negate,
            Op::Jump, Op::JumpIfFalse, Op::JumpIfTrue, Op::Loop,
            Op::Call, Op::Closure, Op::Return,
            Op::MakeList, Op::MakeRecord, Op::MakeAdt,
            Op::TestVariant, Op::Destructure,
            Op::Print,
        ];
        for op in all {
            assert_eq!(Op::from_u8(op.as_u8()), Some(op), "round-trip failed for {op:?}");
        }
    }

    #[test]
    fn an_invalid_byte_is_not_a_valid_op() {
        assert_eq!(Op::from_u8(255), None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-bytecode every_op_round_trips an_invalid_byte`
Expected: FAIL to compile — `Op` doesn't exist yet.

- [ ] **Step 3: Implement**

```rust
/// No `Op::Match` — pattern matching compiles to `TestVariant` + jump
/// chains + `Destructure` (per `CHECKLIST.md`'s own compiler task list);
/// a composite "Match" opcode adds nothing on top of those three.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Constant = 0,
    Nil = 1,
    True = 2,
    False = 3,
    Pop = 4,
    GetLocal = 5,
    SetLocal = 6,
    GetGlobal = 7,
    SetGlobal = 8,
    DefineGlobal = 9,
    GetUpvalue = 10,
    SetUpvalue = 11,
    CloseUpvalue = 12,
    GetField = 13,
    SetField = 14,
    GetIndex = 15,
    SetIndex = 16,
    Equal = 17,
    Greater = 18,
    Less = 19,
    Add = 20,
    Sub = 21,
    Mul = 22,
    Div = 23,
    Mod = 24,
    Not = 25,
    Negate = 26,
    Jump = 27,
    JumpIfFalse = 28,
    JumpIfTrue = 29,
    Loop = 30,
    Call = 31,
    Closure = 32,
    Return = 33,
    MakeList = 34,
    MakeRecord = 35,
    MakeAdt = 36,
    TestVariant = 37,
    Destructure = 38,
    Print = 39,
}

impl Op {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(b: u8) -> Option<Op> {
        use Op::*;
        Some(match b {
            0 => Constant, 1 => Nil, 2 => True, 3 => False, 4 => Pop,
            5 => GetLocal, 6 => SetLocal, 7 => GetGlobal, 8 => SetGlobal, 9 => DefineGlobal,
            10 => GetUpvalue, 11 => SetUpvalue, 12 => CloseUpvalue,
            13 => GetField, 14 => SetField, 15 => GetIndex, 16 => SetIndex,
            17 => Equal, 18 => Greater, 19 => Less, 20 => Add, 21 => Sub, 22 => Mul, 23 => Div, 24 => Mod, 25 => Not, 26 => Negate,
            27 => Jump, 28 => JumpIfFalse, 29 => JumpIfTrue, 30 => Loop,
            31 => Call, 32 => Closure, 33 => Return,
            34 => MakeList, 35 => MakeRecord, 36 => MakeAdt,
            37 => TestVariant, 38 => Destructure,
            39 => Print,
            _ => return None,
        })
    }

    /// The number of operand bytes following this opcode's own byte, for
    /// opcodes with a FIXED operand width. Opcodes with a variable/compound
    /// operand shape (`MakeRecord`, `MakeAdt`) are handled specially by
    /// the disassembler and are not covered by this table — see Task 5.
    pub fn fixed_operand_len(self) -> Option<usize> {
        use Op::*;
        Some(match self {
            Nil | True | False | Pop | CloseUpvalue
            | Equal | Greater | Less | Add | Sub | Mul | Div | Mod | Not | Negate
            | Return | GetIndex | SetIndex | Print => 0,
            GetLocal | SetLocal | GetUpvalue | SetUpvalue | Call | Destructure => 1,
            Constant | GetGlobal | SetGlobal | DefineGlobal | GetField | SetField
            | Jump | JumpIfFalse | JumpIfTrue | Loop | Closure | MakeList | TestVariant => 2,
            MakeRecord | MakeAdt => return None,
        })
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-bytecode`
Expected: PASS. Run `cargo clippy -p ember-bytecode --all-targets -- -D warnings` and `cargo fmt -p ember-bytecode -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-bytecode
git commit -m "Add the Op enum with safe u8 round-tripping"
```

---

## Task 4: `chunk.rs` — `Chunk` and `FunctionProto`

**Files:**
- Modify: `crates/ember-bytecode/src/chunk.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::Op;
    use crate::value::Value;

    #[test]
    fn write_op_and_write_u8_append_bytes_and_track_lines() {
        let mut chunk = Chunk::new();
        chunk.write_op(Op::Nil, 1);
        chunk.write_op(Op::Pop, 1);
        chunk.write_op(Op::Return, 2);
        assert_eq!(chunk.code, vec![Op::Nil.as_u8(), Op::Pop.as_u8(), Op::Return.as_u8()]);
        assert_eq!(chunk.line_at(0), 1);
        assert_eq!(chunk.line_at(1), 1);
        assert_eq!(chunk.line_at(2), 2);
    }

    #[test]
    fn line_info_is_run_length_encoded() {
        let mut chunk = Chunk::new();
        for _ in 0..5 {
            chunk.write_op(Op::Nil, 7);
        }
        // 5 bytes all on line 7 should collapse to ONE (line, run_len) entry,
        // not five — this is the whole point of RLE.
        assert_eq!(chunk.lines, vec![(7, 5)]);
    }

    #[test]
    fn write_u16_writes_big_endian() {
        let mut chunk = Chunk::new();
        chunk.write_u16(0x1234, 1);
        assert_eq!(chunk.code, vec![0x12, 0x34]);
    }

    #[test]
    fn add_constant_deduplicates_equal_values() {
        let mut chunk = Chunk::new();
        let a = chunk.add_constant(Value::Int(42));
        let b = chunk.add_constant(Value::Int(42));
        let c = chunk.add_constant(Value::Int(43));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(chunk.constants.len(), 2);
    }

    #[test]
    fn add_function_does_not_deduplicate() {
        let mut chunk = Chunk::new();
        let proto1 = FunctionProto {
            chunk: Chunk::new(),
            arity: 0,
            upvalues: vec![],
            name: ember_ast::Interner::new().intern("f"),
        };
        let proto2 = FunctionProto {
            chunk: Chunk::new(),
            arity: 0,
            upvalues: vec![],
            name: proto1.name,
        };
        let a = chunk.add_function(proto1);
        let b = chunk.add_function(proto2);
        assert_ne!(a, b, "two distinct FunctionProtos must never share an index, even if superficially identical");
        assert_eq!(chunk.functions.len(), 2);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-bytecode write_op_and_write_u8 line_info_is_run_length write_u16_writes add_constant_deduplicates add_function_does_not`
Expected: FAIL to compile — `Chunk`/`FunctionProto` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::op::Op;
use crate::value::Value;
use ember_ast::Symbol;
use ember_resolve::UpvalueDesc;

#[derive(Default)]
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    /// One compiled function per `OP_CLOSURE` this chunk emits, referenced
    /// by index (a SEPARATE pool from `constants` — a `FunctionProto`
    /// isn't a `Value`, it's a whole nested `Chunk` plus metadata).
    pub functions: Vec<FunctionProto>,
    /// Run-length encoded: one `u32` per byte doubles chunk size for
    /// nothing, and consecutive instructions almost always share a line.
    pub lines: Vec<(u32, u32)>,
}

/// One compiled function — the unit `OP_CLOSURE` instantiates at runtime.
/// `upvalues` is `ember_resolve::UpvalueDesc` reused directly: the
/// descriptor list `OP_CLOSURE` needs (capture from the enclosing frame's
/// locals vs. the enclosing closure's own upvalues) was already computed
/// by the resolver back in Phase 4.
pub struct FunctionProto {
    pub chunk: Chunk,
    pub arity: usize,
    pub upvalues: Vec<UpvalueDesc>,
    pub name: Symbol,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk::default()
    }

    pub fn write_u8(&mut self, byte: u8, line: u32) {
        self.code.push(byte);
        self.record_line(line);
    }

    pub fn write_op(&mut self, op: Op, line: u32) {
        self.write_u8(op.as_u8(), line);
    }

    pub fn write_u16(&mut self, value: u16, line: u32) {
        self.write_u8((value >> 8) as u8, line);
        self.write_u8(value as u8, line);
    }

    fn record_line(&mut self, line: u32) {
        match self.lines.last_mut() {
            Some((last_line, run_len)) if *last_line == line => *run_len += 1,
            _ => self.lines.push((line, 1)),
        }
    }

    pub fn line_at(&self, offset: usize) -> u32 {
        let mut remaining = offset;
        for &(line, run_len) in &self.lines {
            if remaining < run_len as usize {
                return line;
            }
            remaining -= run_len as usize;
        }
        0
    }

    /// Reuses an existing equal constant's index if one exists.
    pub fn add_constant(&mut self, value: Value) -> u16 {
        if let Some(idx) = self.constants.iter().position(|v| v == &value) {
            return idx as u16;
        }
        self.constants.push(value);
        (self.constants.len() - 1) as u16
    }

    /// No deduplication — every compiled function is unique by
    /// definition, even two textually-identical closures compiled from
    /// different source locations.
    pub fn add_function(&mut self, proto: FunctionProto) -> u16 {
        self.functions.push(proto);
        (self.functions.len() - 1) as u16
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-bytecode`
Expected: PASS. Run `cargo clippy -p ember-bytecode --all-targets -- -D warnings` and `cargo fmt -p ember-bytecode -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-bytecode
git commit -m "Add Chunk and FunctionProto with run-length-encoded line info"
```

---

## Task 5: `disasm.rs` — the disassembler

**Files:**
- Modify: `crates/ember-bytecode/src/disasm.rs`

This is the phase's primary testing mechanism — human-readable text with operand names *resolved* (a jump's computed target address, a constant's actual value, a global/field's actual name string), not raw indices.

**Compound-operand opcodes**, not covered by `Op::fixed_operand_len` (Task 3): `MakeRecord` is `<name_const_idx: u16> <field_count: u16>` (4 operand bytes); `MakeAdt` is `<type_const_idx: u16> <variant_const_idx: u16> <arity: u16>` (6 operand bytes).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::op::Op;
    use crate::value::Value;
    use ember_ast::Interner;

    #[test]
    fn disassembles_a_constant_with_its_value_shown() {
        let mut chunk = Chunk::new();
        let idx = chunk.add_constant(Value::Int(42));
        chunk.write_op(Op::Constant, 1);
        chunk.write_u16(idx, 1);
        let interner = Interner::new();
        let out = disassemble_chunk(&chunk, "test", &interner);
        assert!(out.contains("OP_CONSTANT"), "{out}");
        assert!(out.contains("42"), "{out}");
    }

    #[test]
    fn disassembles_a_local_slot_operand() {
        let mut chunk = Chunk::new();
        chunk.write_op(Op::GetLocal, 1);
        chunk.write_u8(3, 1);
        let interner = Interner::new();
        let out = disassemble_chunk(&chunk, "test", &interner);
        assert!(out.contains("OP_GET_LOCAL"), "{out}");
        assert!(out.contains("slot=3") || out.contains('3'), "{out}");
    }

    #[test]
    fn disassembles_a_jump_with_its_computed_target_not_its_raw_offset() {
        let mut chunk = Chunk::new();
        let at = chunk.code.len();
        chunk.write_op(Op::JumpIfFalse, 1);
        chunk.write_u16(0xFFFF, 1); // placeholder, patched below
        chunk.write_op(Op::Pop, 1);
        chunk.write_op(Op::Nil, 1);
        let target = chunk.code.len();
        let jump_offset = (target - at - 3) as u16;
        chunk.code[at + 1] = (jump_offset >> 8) as u8;
        chunk.code[at + 2] = jump_offset as u8;
        let interner = Interner::new();
        let out = disassemble_chunk(&chunk, "test", &interner);
        assert!(out.contains(&format!("{target:04}")), "expected the RESOLVED target address {target:04} in output: {out}");
    }

    #[test]
    fn disassemble_chunk_lists_every_instruction_on_its_own_line() {
        let mut chunk = Chunk::new();
        chunk.write_op(Op::Nil, 1);
        chunk.write_op(Op::Pop, 1);
        chunk.write_op(Op::Return, 2);
        let interner = Interner::new();
        let out = disassemble_chunk(&chunk, "test", &interner);
        assert_eq!(out.lines().filter(|l| !l.is_empty() && !l.starts_with("==")).count(), 3);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-bytecode disassembles_a_constant disassembles_a_local_slot disassembles_a_jump disassemble_chunk_lists`
Expected: FAIL to compile — `disassemble_chunk`/`disassemble_instruction` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use crate::chunk::Chunk;
use crate::op::Op;
use crate::value::Value;
use ember_ast::Interner;

pub fn disassemble_chunk(chunk: &Chunk, name: &str, interner: &Interner) -> String {
    let mut out = format!("== {name} ==\n");
    let mut offset = 0;
    while offset < chunk.code.len() {
        let (line, text, next_offset) = disassemble_instruction(chunk, offset, interner);
        out.push_str(&format!("{offset:04} {line:>4} {text}\n"));
        offset = next_offset;
    }
    out
}

/// Returns `(line, human-readable text, offset of the NEXT instruction)`.
pub fn disassemble_instruction(chunk: &Chunk, offset: usize, interner: &Interner) -> (u32, String, usize) {
    let line = chunk.line_at(offset);
    let op = Op::from_u8(chunk.code[offset]).expect("invalid opcode byte in chunk");
    let read_u16 = |at: usize| u16::from_be_bytes([chunk.code[at], chunk.code[at + 1]]);

    let (text, len) = match op {
        Op::Constant => {
            let idx = read_u16(offset + 1);
            (format!("OP_CONSTANT {idx} ({:?})", chunk.constants[idx as usize]), 3)
        }
        Op::GetGlobal | Op::SetGlobal | Op::DefineGlobal | Op::GetField | Op::SetField | Op::TestVariant => {
            let idx = read_u16(offset + 1);
            let name = match &chunk.constants[idx as usize] {
                Value::Str(s) => s.to_string(),
                other => format!("{other:?}"),
            };
            (format!("{}({idx}) {name:?}", op_name(op)), 3)
        }
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue => {
            let jump_offset = read_u16(offset + 1);
            let target = offset + 3 + jump_offset as usize;
            (format!("{} -> {target:04}", op_name(op)), 3)
        }
        Op::Loop => {
            let jump_offset = read_u16(offset + 1);
            let target = (offset + 3).saturating_sub(jump_offset as usize);
            (format!("OP_LOOP -> {target:04}", ), 3)
        }
        Op::Closure => {
            let idx = read_u16(offset + 1);
            let proto = &chunk.functions[idx as usize];
            (
                format!("OP_CLOSURE {idx} <fn {} / arity {}>", interner.resolve(proto.name), proto.arity),
                3,
            )
        }
        Op::MakeList => {
            let count = read_u16(offset + 1);
            (format!("OP_MAKE_LIST count={count}"), 3)
        }
        Op::GetLocal | Op::SetLocal | Op::GetUpvalue | Op::SetUpvalue => {
            let slot = chunk.code[offset + 1];
            (format!("{} slot={slot}", op_name(op)), 2)
        }
        Op::Call => {
            let argc = chunk.code[offset + 1];
            (format!("OP_CALL argc={argc}"), 2)
        }
        Op::Destructure => {
            let field = chunk.code[offset + 1];
            (format!("OP_DESTRUCTURE field={field}"), 2)
        }
        Op::MakeRecord => {
            let name_idx = read_u16(offset + 1);
            let field_count = read_u16(offset + 3);
            let name = match &chunk.constants[name_idx as usize] {
                Value::Str(s) => s.to_string(),
                other => format!("{other:?}"),
            };
            (format!("OP_MAKE_RECORD {name:?} fields={field_count}"), 5)
        }
        Op::MakeAdt => {
            let type_idx = read_u16(offset + 1);
            let variant_idx = read_u16(offset + 3);
            let arity = read_u16(offset + 5);
            let type_name = match &chunk.constants[type_idx as usize] {
                Value::Str(s) => s.to_string(),
                other => format!("{other:?}"),
            };
            let variant_name = match &chunk.constants[variant_idx as usize] {
                Value::Str(s) => s.to_string(),
                other => format!("{other:?}"),
            };
            (format!("OP_MAKE_ADT {type_name:?}::{variant_name:?} arity={arity}"), 7)
        }
        Nil_and_the_rest @ (Op::Nil | Op::True | Op::False | Op::Pop | Op::CloseUpvalue | Op::GetIndex | Op::SetIndex
        | Op::Equal | Op::Greater | Op::Less | Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Not | Op::Negate
        | Op::Return | Op::Print) => (op_name(Nil_and_the_rest).to_string(), 1),
    };
    (line, text, offset + len)
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Constant => "OP_CONSTANT",
        Op::Nil => "OP_NIL",
        Op::True => "OP_TRUE",
        Op::False => "OP_FALSE",
        Op::Pop => "OP_POP",
        Op::GetLocal => "OP_GET_LOCAL",
        Op::SetLocal => "OP_SET_LOCAL",
        Op::GetGlobal => "OP_GET_GLOBAL",
        Op::SetGlobal => "OP_SET_GLOBAL",
        Op::DefineGlobal => "OP_DEFINE_GLOBAL",
        Op::GetUpvalue => "OP_GET_UPVALUE",
        Op::SetUpvalue => "OP_SET_UPVALUE",
        Op::CloseUpvalue => "OP_CLOSE_UPVALUE",
        Op::GetField => "OP_GET_FIELD",
        Op::SetField => "OP_SET_FIELD",
        Op::GetIndex => "OP_GET_INDEX",
        Op::SetIndex => "OP_SET_INDEX",
        Op::Equal => "OP_EQUAL",
        Op::Greater => "OP_GREATER",
        Op::Less => "OP_LESS",
        Op::Add => "OP_ADD",
        Op::Sub => "OP_SUB",
        Op::Mul => "OP_MUL",
        Op::Div => "OP_DIV",
        Op::Mod => "OP_MOD",
        Op::Not => "OP_NOT",
        Op::Negate => "OP_NEGATE",
        Op::Jump => "OP_JUMP",
        Op::JumpIfFalse => "OP_JUMP_IF_FALSE",
        Op::JumpIfTrue => "OP_JUMP_IF_TRUE",
        Op::Loop => "OP_LOOP",
        Op::Call => "OP_CALL",
        Op::Closure => "OP_CLOSURE",
        Op::Return => "OP_RETURN",
        Op::MakeList => "OP_MAKE_LIST",
        Op::MakeRecord => "OP_MAKE_RECORD",
        Op::MakeAdt => "OP_MAKE_ADT",
        Op::TestVariant => "OP_TEST_VARIANT",
        Op::Destructure => "OP_DESTRUCTURE",
        Op::Print => "OP_PRINT",
    }
}
```

The `Nil_and_the_rest @ (...)` binding pattern in the big no-operand match arm is a slightly unusual construct — if it doesn't compile cleanly or `cargo fmt`/`clippy` flags it, replace that one arm with a plain `other => (op_name(other).to_string(), 1),` catch-all instead (functionally identical, just without naming every variant explicitly); either is fine, prefer whichever compiles cleanly and passes clippy.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-bytecode`
Expected: PASS. Run `cargo clippy -p ember-bytecode --all-targets -- -D warnings` and `cargo fmt -p ember-bytecode -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-bytecode
git commit -m "Add the disassembler with resolved operand names"
```

---

## Task 6: Scaffold `ember-compile`

**Files:**
- Modify: `crates/ember-compile/Cargo.toml`
- Modify: `crates/ember-compile/src/lib.rs`
- Create: `crates/ember-compile/src/compiler.rs` (empty stub, filled in Task 7+)

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "ember-compile"
version.workspace = true
edition.workspace = true

[dependencies]
ember-span = { path = "../ember-span" }
ember-ast = { path = "../ember-ast" }
ember-lexer = { path = "../ember-lexer" }
ember-resolve = { path = "../ember-resolve" }
ember-bytecode = { path = "../ember-bytecode" }
rustc-hash = "2"

[dev-dependencies]
ember-parser = { path = "../ember-parser" }
```

- [ ] **Step 2: Write `lib.rs`**

```rust
//! AST-to-bytecode compiler.

pub mod compiler;

pub use compiler::Compiler;
```

(`compile`, the public free-function entry point, is added to this re-export list in Task 15 once `Compiler` actually has something to compile.)

- [ ] **Step 3: Create the empty stub and verify it builds**

```bash
touch crates/ember-compile/src/compiler.rs
```

`compiler.rs` is empty right now, so `lib.rs`'s `pub use compiler::{compile, Compiler}` will fail to compile — that's expected and gets fixed in Task 7's first step. Do not run `cargo build` yet; go straight to Task 7.

- [ ] **Step 4: Commit** (combined with Task 7's commit, since an empty `compiler.rs` doesn't build on its own — see Task 7 Step 5)

---

## Task 7: Compiler skeleton — `Compiler`, stack-effect tracking, jump-patching

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

**Files:**

This task builds the scaffolding every later task emits code through: the `Compiler`/`FunctionCompiler` state, a static per-`Op` stack-effect table (used for the debug-build balance assertions the design calls for), and the `emit_jump`/`patch_jump`/`emit_loop` helpers every control-flow task (9, 10) depends on. No AST-walking yet — that starts in Task 8.

**Design notes carried from the spec doc, made concrete here:**
- Every `Stmt` compiles to **net-zero** stack effect: `ExprStmt` pushes its expression then immediately `Pop`s it; `Let` pushes its initializer and *deliberately leaves it* — that pushed value permanently occupies the new local's stack slot, which is why `Stmt::Let` is the one exception tracked separately (`FunctionCompiler::local_count` goes up by one, and the balance assertion after a `Let` statement expects depth `+1`, not `0`).
- `Expr::Block`'s `tail` (if present) contributes exactly one net value; its own `Let`-introduced locals are popped via bulk `OP_POP` emission at scope exit (Task 9), so a whole block containing three `let`s and a tail expression returns to `entry_depth + 1`, not `entry_depth + 4`.
- Jump-family opcodes always pop the value they test **unconditionally at runtime**, regardless of which way control ends up going — `JumpIfFalse`/`JumpIfTrue` are `-1` in the static table even though whether the jump is taken depends on the popped value. This is what lets a purely-linear compile-time running sum stay accurate without simulating actual branches (both arms of any `if`/`&&`/`||`/`while` this compiler emits are constructed to balance identically — verified per-task as those constructs are added).
- `Call`, `MakeList`, `MakeRecord`, and `MakeAdt` have genuinely operand-*count*-dependent effects — `static_stack_effect` returns `None` for these, and the emitting code (Tasks 11-14) adjusts `stack_depth` manually with an explicit comment at each call site. `Return`, `TestVariant`, and `Destructure` are *not* in that group even though they carry operands: `Return` always pops exactly one value (the operand-less case is just "which value", not "how many"), and `TestVariant`/`Destructure`'s single operand is an *index*, not a count — both always pop one base and push one result, so they get fixed entries (`-1`, `0`, `0` respectively) like everything else.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ember_bytecode::chunk::{Chunk, FunctionProto};
    use ember_bytecode::op::Op;
    use ember_resolve::{Bindings, FunctionId};
    use ember_ast::{Ast, Interner};

    fn empty_fc() -> FunctionCompiler {
        FunctionCompiler::new(FunctionId::TopLevel)
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
        assert_eq!(static_stack_effect(Op::Call), None, "operand-dependent, not in the static table");
        assert_eq!(static_stack_effect(Op::MakeList), None);
        assert_eq!(static_stack_effect(Op::Destructure), Some(0), "its operand is an index, not a count — pop base, push field, always");
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
        assert_eq!(patched, 2, "jump should skip exactly the 2 one-byte instructions emitted after it");
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
        assert_eq!(fc.physical_slot(3), 5, "only the outer shift applies (3 < 5)");
        assert_eq!(fc.physical_slot(6), 10, "both shifts apply (6 >= 2 and 6 >= 5)");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL to compile — `FunctionCompiler`, `static_stack_effect`, `emit_op`, `emit_jump`, `patch_jump`, `emit_loop` don't exist yet.

- [ ] **Step 3: Implement**

```rust
use ember_bytecode::chunk::Chunk;
use ember_bytecode::op::Op;
use ember_resolve::FunctionId;

/// Fixed, unconditional runtime stack effect for ops whose effect doesn't
/// depend on an operand. `None` means "operand-dependent — adjust
/// `stack_depth` manually at the call site" (see Tasks 11-14).
pub(crate) fn static_stack_effect(op: Op) -> Option<i32> {
    use Op::*;
    match op {
        Constant | Nil | True | False => Some(1),
        Pop => Some(-1),
        GetLocal | GetGlobal | GetUpvalue => Some(1),
        SetLocal | SetGlobal | SetUpvalue => Some(0),
        DefineGlobal => Some(-1),
        CloseUpvalue => Some(-1),
        GetField => Some(0), // consumes [base] (index is a compile-time constant operand, not a stack value), produces [value]
        GetIndex => Some(-1), // consumes [base, index] (both runtime stack values), produces [value]
        SetField => Some(-1),
        SetIndex => Some(-2),
        Equal | Greater | Less | Add | Sub | Mul | Div | Mod => Some(-1),
        Not | Negate => Some(0),
        Jump | Loop => Some(0),
        JumpIfFalse | JumpIfTrue => Some(-1),
        Closure => Some(1),
        TestVariant => Some(0), // pops the scrutinee, pushes a Bool — Task 14 always re-fetches the scrutinee fresh via GetLocal beforehand rather than chaining off a peeked value, so a plain pop-then-push is enough
        Destructure => Some(0), // pops the base (an Adt), pushes the extracted positional field — its operand is an INDEX, not a count, so unlike Call/MakeList/etc. this effect is NOT operand-dependent
        Return => Some(-1), // pops the return value; the frame ends immediately after, so nothing downstream in this chunk observes the resulting depth
        Print => Some(-1),
        Call | MakeList | MakeRecord | MakeAdt => None, // these alone have a genuinely operand-*count*-dependent effect
    }
}

/// One loop's patch targets and cleanup bookkeeping (Task 10 is where this
/// is actually used — `while`/`for`/`loop` each push one of these before
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
///   for `for` (Task 10's desugaring) that's the counter-increment code,
///   which comes *after* the body, making a continue-from-inside-body a
///   forward jump even though it's semantically "go to the next lap."
/// - `break_jumps`: same idea, patched once the loop's end address (after
///   its own cleanup pops) is known.
pub(crate) struct LoopCtx {
    pub body_base_local_count: u32,
    pub continue_jumps: Vec<usize>,
    pub break_jumps: Vec<usize>,
}

/// A local-slot addressing correction, needed only for `for`-loop
/// desugaring (Task 10). The resolver assigns each declared name an
/// absolute, frame-relative slot number assuming every declaration
/// corresponds to exactly one physical stack push — but the compiler's
/// `for`-loop desugaring pushes two compiler-only hidden locals (the
/// iterated list, the index counter) that the resolver has no idea exist.
/// Every resolver slot number `>= base` — the loop binding itself, and
/// anything declared inside the loop body afterward — physically lands
/// `extra` positions further up the stack than the resolver assumed, so
/// `FunctionCompiler::physical_slot` adds `extra` to any resolver slot
/// `>= base` before it's written into a `GetLocal`/`SetLocal` operand
/// byte. Unused (empty `slot_shifts`, `physical_slot` is the identity
/// function) until Task 10.
pub(crate) struct SlotShift {
    pub base: u32,
    pub extra: u32,
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
    pub(crate) loops: Vec<LoopCtx>,
    pub(crate) slot_shifts: Vec<SlotShift>,
    pub(crate) stack_depth: i32,
}

impl FunctionCompiler {
    pub(crate) fn new(function_id: FunctionId) -> Self {
        FunctionCompiler {
            function_id,
            chunk: Chunk::new(),
            local_count: 0,
            loops: Vec::new(),
            slot_shifts: Vec::new(),
            stack_depth: 0,
        }
    }

    /// Translates a resolver-assigned slot number into the real physical
    /// stack position, applying every active `for`-loop shift whose `base`
    /// is at or below `resolver_slot`. Nested `for` loops compound
    /// correctly: each shift only ever applies to slots at or after its
    /// own `base`, and `base`s strictly increase with nesting depth (a
    /// resolver slot can never be reused at a shallower nesting level
    /// while an outer shift is still active, since the resolver only
    /// frees a slot once its owning scope has fully popped).
    pub(crate) fn physical_slot(&self, resolver_slot: u32) -> u32 {
        resolver_slot
            + self
                .slot_shifts
                .iter()
                .filter(|s| resolver_slot >= s.base)
                .map(|s| s.extra)
                .sum::<u32>()
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
    /// Always has stack effect `-1` (`JumpIfFalse`/`JumpIfTrue`) or `0`
    /// (`Jump`), both from the static table.
    pub(crate) fn emit_jump(&mut self, op: Op, line: u32) -> usize {
        self.emit_op(op, line);
        let operand_start = self.chunk.code.len();
        self.chunk.write_u16(0xFFFF, line);
        operand_start
    }

    /// Backfills the placeholder at `operand_start` (as returned by
    /// `emit_jump`) with the real forward-jump distance to the current end
    /// of the chunk. Panics if the distance doesn't fit in `u16` — a
    /// single function body over ~64KB of bytecode is already pathological
    /// and should fail loudly at compile time, not silently corrupt.
    pub(crate) fn patch_jump(&mut self, operand_start: usize) {
        let target = self.chunk.code.len();
        let jump = target - operand_start - 2;
        let jump: u16 = jump.try_into().expect("jump distance exceeds u16::MAX");
        self.chunk.code[operand_start] = (jump >> 8) as u8;
        self.chunk.code[operand_start + 1] = jump as u8;
    }

    /// Emits a backward `OP_LOOP` from the current position to `loop_start`.
    /// Stack effect `0` (from the static table) — a loop back-edge never
    /// changes the stack itself.
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
/// slot/upvalue/global decision was already made in Phase 4/8-design) plus
/// a stack of function-compilers-in-progress, innermost (currently being
/// compiled) last. Filled in starting Task 8.
pub struct Compiler<'a> {
    pub(crate) ast: &'a ember_ast::Ast,
    pub(crate) interner: &'a mut ember_ast::Interner,
    pub(crate) bindings: &'a ember_resolve::Bindings,
    pub(crate) functions: Vec<FunctionCompiler>,
}
```

`interner` is `&mut`, not `&`, even though the compiler mostly only *reads* names: Task 10's `for`-loop desugaring needs to look up the pre-interned `len` native's `Symbol` to synthesize a length check, and `Interner` (`crates/ember-ast/src/interner.rs`) only exposes `intern(&mut self, &str) -> Symbol` (get-or-intern) — there's no read-only "look up an existing symbol" method. Since `seed_native_globals` (`ember-resolve`) unconditionally interns all 8 native names during resolution, calling `interner.intern("len")` during compilation is guaranteed to return the *already-existing* symbol, never allocate a new one — but it still needs `&mut` to make that call at all.

There is deliberately no public `compile()` free function yet, and `Compiler` has no `new`/statement-walking methods yet either — those start in Task 8 (`Compiler::new` plus literal/`Var` compilation) and build up through Task 15, which adds the public entry point once there's a real top-level function body to produce. `#[allow(dead_code)]` is not needed on `Compiler`'s fields for this to compile cleanly, since the test module in this same file already constructs and reads `FunctionCompiler` directly; `Compiler` itself goes unconstructed until Task 8's tests exercise it, which is expected for one task's gap and not a placeholder needing a fix.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: the 5 new tests PASS.

Run `cargo clippy -p ember-compile --all-targets -- -D warnings`. If clippy flags `Compiler` as dead code (unused struct, since nothing constructs one yet), add `#[allow(dead_code)]` directly above `pub struct Compiler<'a> {` with a comment noting it's constructed starting Task 8 — remove the allow once Task 8 lands.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Scaffold ember-compile with stack-effect tracking and jump-patching"
```

---

## Task 8: `Compiler::new` + literals + `Var` (Local/Upvalue/Global dispatch) + arithmetic/comparison/logical `Binary`/`Unary`

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

`Compiler` gets its first real AST-walking methods. This task covers every `Expr` variant that needs no scope-shape changes: literals, `Var` (reading `Bindings.resolutions`, the direct Phase 4 payoff), and `Unary`/`Binary` (arithmetic, comparison, `&&`/`||` short-circuit). `If`/`Block`/`Let` (Task 9) and beyond build on `compile_expr` established here.

**`TokenKind` → `Op` mapping** (from `ember_lexer::TokenKind`, matching exactly what Phase 7's `apply_binary`/`eval_unary` handle): `Plus`→`Add`, `Minus`→`Sub` (binary) or `Negate` (unary), `Star`→`Mul`, `Slash`→`Div`, `Percent`→`Mod`, `Bang`→`Not`, `EqEq`→`Equal`, `BangEq`→`Equal`+`Not` (no dedicated not-equal opcode — SPEC's minimal set omits it), `Lt`→`Less`, `Gt`→`Greater`, `LtEq`→`Greater`+`Not` (`a <= b` ⇔ `!(a > b)`), `GtEq`→`Less`+`Not` (`a >= b` ⇔ `!(a < b)`). `AndAnd`/`OrOr` compile via jumps, not an opcode.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    // ... (existing Task 7 tests stay above this line)

    fn compile_expr_str(src: &str) -> String {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "resolve diags: {resolve_diags:?}");
        let mut compiler = Compiler::new(&ast, &mut interner, &bindings);
        let Stmt::ExprStmt(expr_idx) = ast.stmt(stmts[0]) else {
            panic!("expected a single expression statement");
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
        assert!(mul_line < add_line, "* must be emitted (and thus executed) before +: {out}");
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
        assert!(!out.contains("OP_AND"), "there is no OP_AND — && must compile to jumps: {out}");
    }

    #[test]
    fn a_local_variable_reference_compiles_to_get_local() {
        let out = compile_expr_str("let x = 1; x;");
        assert!(out.contains("OP_GET_LOCAL"), "{out}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL to compile — `Compiler::new` and `compile_expr` don't exist yet.

- [ ] **Step 3: Implement**

Add to `compiler.rs` (imports at the top of the file, extended):

```rust
use ember_ast::{Ast, Expr, Idx, Interner, Stmt, Symbol};
use ember_bytecode::chunk::Chunk;
use ember_bytecode::op::Op;
use ember_bytecode::value::Value;
use ember_lexer::TokenKind;
use ember_resolve::{Bindings, FunctionId, Resolution};
use std::rc::Rc;
```

Then, on `impl<'a> Compiler<'a>`:

```rust
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
        self.functions.last_mut().expect("at least one FunctionCompiler is always on the stack")
    }

    /// Pools `sym`'s resolved string as a `Value::Str` constant — used for
    /// global names, field names, and (Task 14) variant/type names, so
    /// the disassembler and the future VM see a real name, not a bare index.
    fn name_constant(&mut self, sym: Symbol) -> u16 {
        let text = self.interner.resolve(sym).to_string();
        self.current().chunk.add_constant(Value::Str(Rc::new(text)))
    }

    pub(crate) fn compile_expr(&mut self, idx: Idx<Expr>) {
        let line = self.ast.span_of_expr(idx).start; // placeholder line numbering: byte offset stands in for a line number until a line table exists upstream (matches Phase 6/7 precedent of using raw span offsets where no line-mapping pass exists yet)
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
            Expr::Error => panic!("cannot compile an Expr::Error node — the pipeline must reject programs with parse/resolve errors before compilation"),
            other => unimplemented!("compile_expr: {other:?} — added in a later task"),
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
                self.compile_expr(rhs);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_false);
                self.current().emit_op(Op::False, line);
                self.current().patch_jump(to_end);
                return;
            }
            TokenKind::OrOr => {
                self.compile_expr(lhs);
                let to_true = self.current().emit_jump(Op::JumpIfTrue, line);
                self.compile_expr(rhs);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_true);
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
}
```

`compile_var`'s `Local` arm routes the resolver's slot number through `self.current().physical_slot(*slot)` rather than using it directly — with `slot_shifts` empty (as it is for every program that doesn't yet involve a `for` loop), `physical_slot` is the identity function, so this has no observable effect until Task 10. Getting this indirection right here, before any code depends on the un-shifted value, avoids a much larger retrofit later — anywhere a resolver slot number is about to become a `GetLocal`/`SetLocal`/`CloseUpvalue` operand byte (this file, Task 11's assignment target, Task 12's `OP_CLOSE_UPVALUE` emission), it must go through `physical_slot` first.

Note the `unimplemented!("compile_expr: {other:?} — added in a later task")` catch-all: this is a deliberate, temporary "not yet reached" arm, not a silent placeholder — every remaining `Expr` variant (`Assign`, `Call`, `Index`, `Field`, `Lambda`, `If`, `Match`, `Block`, `List`, `Struct`) is added to this same `match` by name in Tasks 9-14, each removing its case from the catch-all. By Task 14 the catch-all arm itself is deleted (every variant has an explicit arm) — if you reach Task 14 and the catch-all is still needed for some variant, that variant was missed and must be added, not left to panic.

`self.ast.span_of_expr(idx).start` standing in for a "line number": `ember_span::Span` carries byte offsets, not line numbers, and no line-mapping pass exists anywhere in the pipeline yet (Phase 7's diagnostics render byte-offset spans directly via `ariadne`, which handles the line lookup itself at render time). `Chunk::lines` faithfully records whatever `u32` it's given — using the byte offset there is not "wrong," it just means the disassembler's line column shows byte offsets rather than 1-based line numbers, exactly matching how every other diagnostic in this codebase already displays position information. If a real line-mapping table is wanted later, that's a follow-up outside this phase's scope, not a defect introduced here.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged. Remove the `#[allow(dead_code)]` added on `Compiler` in Task 7 now that `Compiler::new` is a real, tested constructor.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile literals, Var resolution dispatch, and arithmetic/logical operators"
```

---

## Task 9: `Stmt::Let` + `Expr::Block` (scope-exit `OP_POP`s) + `Expr::If` + top-level dual registration

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

**The block scope-exit trick:** a block like `{ let a = 1; let b = 2; a + b }` must leave *only* the tail value on the stack once it's done, but the tail is computed *after* (i.e. on top of) `a` and `b`'s slots — a plain stack only lets you pop from the top, so popping `a`/`b` first isn't directly possible without first getting the tail value out of the way. This plan uses `OP_SET_LOCAL` for that: it writes the top-of-stack value into an arbitrary local slot **without popping** (matching assignment-as-expression semantics — the written value stays on top too). So: compute the tail (or `Nil` if none) on top of the `n` locals this block declared, `OP_SET_LOCAL` it down into the first of those `n` slots (overwriting that local, which is fine — it's dying anyway), then emit `n` plain `OP_POP`s. Those `n` pops remove the (n-1) other now-dead locals plus the duplicate tail value sitting on top, leaving exactly the tail's value sitting where the first local used to be — which is exactly the depth the block should end at. When a block declares zero locals (`n == 0`), skip this entirely — there is no slot to write into, and the tail is already correctly positioned.

**Top-level dual registration**, exactly as decided in the design doc: a `let`/`fn`/`type`/`struct` declared directly at the top level (not nested inside any `Block`) is *both* a `Resolution::Local` for same-frame references *and* visible as `Resolution::Global` to nested functions (mirroring the resolver's own `functions[0].scopes.first()` fallback, which only ever looks at the **outermost** scope of the top-level function — a `let` inside a top-level `if { }` block does NOT get this treatment, matching the resolver exactly). `FunctionCompiler` gets a `scope_depth: u32` counter (0 at the top level, incremented on `Block` entry, decremented on exit) so the compiler can tell whether it's currently at that outermost scope: dual registration applies exactly when `function_id == FunctionId::TopLevel && scope_depth == 0`.

- [ ] **Step 1: Modify `FunctionCompiler` — add `scope_depth`**

In `FunctionCompiler`'s definition (Task 7), add the field and initialize it:

```rust
pub struct FunctionCompiler {
    pub(crate) function_id: FunctionId,
    pub(crate) chunk: Chunk,
    pub(crate) local_count: u32,
    pub(crate) scope_depth: u32,
    pub(crate) loops: Vec<LoopCtx>,
    pub(crate) stack_depth: i32,
}
```

And in `FunctionCompiler::new`, add `scope_depth: 0,` to the struct literal.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    fn compile_program_str(src: &str) -> (String, ember_ast::Interner) {
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "resolve diags: {resolve_diags:?}");
        let mut compiler = Compiler::new(&ast, &mut interner, &bindings);
        for &s in &stmts {
            compiler.compile_stmt(s);
        }
        let fc = compiler.functions.pop().unwrap();
        let out = ember_bytecode::disasm::disassemble_chunk(&fc.chunk, "test", &interner);
        (out, interner)
    }

    #[test]
    fn a_top_level_let_gets_dual_registration() {
        let (out, _) = compile_program_str("let x = 1;");
        assert!(out.contains("OP_CONSTANT"), "{out}");
        assert!(out.contains("OP_DEFINE_GLOBAL"), "top-level let must also define a global: {out}");
        assert!(!out.contains("OP_POP"), "the top-level local itself is never popped: {out}");
    }

    #[test]
    fn a_let_inside_a_block_does_not_get_dual_registration() {
        let (out, _) = compile_program_str("{ let y = 1; y };");
        assert!(!out.contains("OP_DEFINE_GLOBAL"), "a block-local let is never a global: {out}");
    }

    #[test]
    fn a_block_with_two_locals_and_a_tail_pops_both_and_keeps_the_tail() {
        let (out, _) = compile_program_str("{ let a = 1; let b = 2; a + b };");
        let pop_count = out.matches("OP_POP").count();
        assert_eq!(pop_count, 2, "exactly 2 locals to clean up: {out}");
        assert!(out.contains("OP_SET_LOCAL"), "{out}");
    }

    #[test]
    fn a_block_with_no_locals_emits_no_pops() {
        let (out, _) = compile_program_str("{ 1 + 2 };");
        assert!(!out.contains("OP_POP"), "{out}");
        assert!(!out.contains("OP_SET_LOCAL"), "{out}");
    }

    #[test]
    fn a_block_with_no_tail_pushes_nil() {
        let (out, _) = compile_program_str("{ let z = 1; };");
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
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL to compile — `compile_stmt` doesn't exist yet, and `compile_expr`'s catch-all doesn't handle `Block`/`If`.

- [ ] **Step 4: Implement**

Add `Block` and `If` arms to `compile_expr`'s `match` (replacing the catch-all's coverage of just these two variants — leave the catch-all in place for the rest):

```rust
            Expr::If { cond, then_, else_ } => {
                self.compile_expr(cond);
                let to_else = self.current().emit_jump(Op::JumpIfFalse, line);
                self.compile_expr(then_);
                let to_end = self.current().emit_jump(Op::Jump, line);
                self.current().patch_jump(to_else);
                match else_ {
                    Some(e) => self.compile_expr(e),
                    None => self.current().emit_op(Op::Nil, line),
                }
                self.current().patch_jump(to_end);
            }
            Expr::Block { stmts, tail } => self.compile_block(&stmts, tail, line),
```

Add the new methods (and `compile_stmt`) to `impl<'a> Compiler<'a>`:

```rust
    fn compile_block(&mut self, stmts: &[Idx<ember_ast::Stmt>], tail: Option<Idx<Expr>>, line: u32) {
        let entry_local_count = self.current().local_count;
        self.current().scope_depth += 1;
        for &s in stmts {
            self.compile_stmt(s);
        }
        match tail {
            Some(t) => self.compile_expr(t),
            None => self.current().emit_op(Op::Nil, line),
        }
        let declared = self.current().local_count - entry_local_count;
        if declared > 0 {
            self.current().emit_op(Op::SetLocal, line);
            self.current().chunk.write_u8(entry_local_count as u8, line);
            for _ in 0..declared {
                self.current().emit_op(Op::Pop, line);
            }
        }
        self.current().local_count = entry_local_count;
        self.current().scope_depth -= 1;
    }

    pub(crate) fn compile_stmt(&mut self, idx: Idx<ember_ast::Stmt>) {
        let entry_depth = self.current().stack_depth;
        let is_let = matches!(self.ast.stmt(idx), Stmt::Let { .. });
        let line = self.ast.span_of_stmt(idx).start;
        match self.ast.stmt(idx).clone() {
            Stmt::Let { name, init, .. } => {
                self.compile_expr(init);
                self.current().local_count += 1;
                self.maybe_dual_register(name, line);
            }
            Stmt::ExprStmt(e) => {
                self.compile_expr(e);
                self.current().emit_op(Op::Pop, line);
            }
            other => unimplemented!("compile_stmt: {other:?} — added in a later task"),
        }
        // CHECKLIST.md's required per-statement stack-balance assertion.
        // Every statement kind but `Let` must return to its entry depth —
        // `Let`'s pushed initializer deliberately stays, permanently
        // occupying the new local's slot, so it alone expects `+1`. This
        // wrapping structure (entry snapshot before the match, assert
        // after) stays correct as later tasks add more arms to the match
        // above — no per-arm assertion needed at each addition.
        let expected = if is_let { entry_depth + 1 } else { entry_depth };
        debug_assert_eq!(
            self.current().stack_depth,
            expected,
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
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile Let, Block scope-exit pops, If, and top-level dual registration"
```

---

## Task 10: `While`/`Loop`/`Break`/`Continue`, and `For` desugaring

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

**Why `for` needs the slot-shift mechanism (Task 7's `SlotShift`/`physical_slot`, unused until now):** `for x in xs { body }` desugars to an index-counter loop over two compiler-only hidden locals — the evaluated iterable and an index counter — that live on the stack for the loop's duration but that `ember-resolve` has no idea exist (it modeled `for`'s only declared name as `binding`, resolving every reference to it inside `body` as `Resolution::Local { slot: K }` for whatever `K` its own `next_slot` counter was at when it resolved `Stmt::For`). If this compiler pushed the hidden iterable and counter locals *before* `binding`'s own value, `binding` would physically land at stack position `K + 2`, not `K` — silently reading the wrong slot for every reference to the loop variable. The fix (already built in Task 7): before pushing anything hidden, snapshot `base = self.current().local_count` — since nothing hidden has been pushed yet, this still faithfully equals the resolver's `K` — then push `SlotShift { base, extra: 2 }` so `physical_slot` correctly redirects `K` (and anything resolver-declared afterward, e.g. a `let` inside `body`) two slots further up, exactly where they now physically live.

**`break`/`continue` cleanup:** jumping out of a loop body bypasses the normal per-`Block` scope-exit `OP_POP`s (Task 9) for every block the jump escapes — a `break` three blocks deep would otherwise leave those blocks' now-dead locals sitting on the stack forever. Each `LoopCtx` (Task 7) records `body_base_local_count`; `break`/`continue` emit `local_count - body_base_local_count` plain `OP_POP`s immediately before their jump, unwinding exactly what's accumulated since the loop body started, regardless of how many nested blocks contributed it.

**`len(xs)` without a general `Call` compiler yet:** Task 13 adds full `Expr::Call` compilation; this task needs to call the `len` native *now*, for the loop condition. It adds one narrow, self-contained helper, `emit_native_call`, that Task 13 does not need to change (it compiles user-written calls a different way, walking `Expr::Call`'s own `callee`/`args`).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn while_loop_jumps_back_to_the_condition_check() {
        let (out, _) = compile_program_str("while true { 1; }");
        assert!(out.contains("OP_LOOP"), "{out}");
        assert!(out.contains("OP_JUMP_IF_FALSE"), "{out}");
    }

    #[test]
    fn for_loop_compiles_to_a_get_local_get_index_pair_for_each_element_access() {
        let (out, _) = compile_program_str("for x in xs { x; }");
        // resolve() will error on an undeclared `xs`, so declare it first:
        let (out2, _) = compile_program_str("let xs = [1, 2]; for x in xs { x; }");
        let _ = out;
        assert!(out2.contains("OP_GET_INDEX"), "{out2}");
        assert!(out2.contains("OP_LOOP"), "{out2}");
        assert!(out2.contains("\"len\""), "must call the len native: {out2}");
    }

    #[test]
    fn break_in_a_nested_loop_targets_the_innermost_loop() {
        let src = "let mut i = 0; while true { while true { break; } break; }";
        let (out, _) = compile_program_str(src);
        // Two independent break jumps, one per loop — not one shared target.
        let jump_count = out.lines().filter(|l| l.contains("OP_JUMP ")).count();
        assert!(jump_count >= 2, "{out}");
    }

    #[test]
    fn break_inside_a_nested_block_pops_the_blocks_locals_before_jumping() {
        let src = "while true { let a = 1; break; }";
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
        assert!(!out.contains("OP_JUMP_IF_FALSE"), "an unconditional loop has no condition to test: {out}");
        assert!(out.contains("OP_LOOP"), "{out}");
    }

    #[test]
    fn nested_if_inside_while_has_correctly_resolved_jump_targets() {
        // CHECKLIST.md: "jump offsets correct for nested if/while". The
        // disassembler (Task 5) resolves every jump to its real, absolute
        // target address — a wrong offset would show up as a `->` target
        // that either points at the wrong instruction or falls outside
        // the chunk entirely. Cross-checking that every jump target in
        // this nested program is a real instruction boundary (one of the
        // `NNNN` offsets the disassembler itself printed a line for) is a
        // strong, mechanical way to catch a miscomputed offset.
        let src = "while true { if true { 1; } else { 2; } }";
        let (out, _) = compile_program_str(src);
        let instruction_offsets: std::collections::HashSet<&str> = out
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        let jump_target_count = out.matches("->").count();
        assert!(jump_target_count >= 2, "expects at least the if's JumpIfFalse and Jump: {out}");
        for line in out.lines().filter(|l| l.contains("->")) {
            let target = line.rsplit("-> ").next().unwrap().trim();
            assert!(
                instruction_offsets.contains(target),
                "jump target {target} in {line:?} doesn't land on a real instruction boundary: {out}"
            );
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL to compile — `Stmt::While`/`For`/`Loop`/`Break`/`Continue` fall through `compile_stmt`'s catch-all `unimplemented!`.

- [ ] **Step 3: Implement**

Replace `compile_stmt`'s catch-all coverage of these five variants (leave the catch-all for the rest — `Return`, `Fn`, `TypeDecl`, `StructDecl`, `Error` are still Task 11+/14's job):

```rust
            Stmt::While { cond, body } => self.compile_while(cond, body, line),
            Stmt::Loop { body } => self.compile_loop(body, line),
            Stmt::For { binding, iter, body } => self.compile_for(binding, iter, body, line),
            Stmt::Break => self.compile_break(line),
            Stmt::Continue => self.compile_continue(line),
```

(`binding` is unused in `compile_for`'s own signature beyond documentation purposes — every reference to it inside `body` was already resolved by `ember-resolve` to `Resolution::Local { slot: K }`, and `physical_slot` handles the rest. It's kept as a parameter only because `Stmt::For` carries it and Rust's `match` destructure requires binding it to something; prefix with `_` if clippy flags it unused.)

Add the new methods to `impl<'a> Compiler<'a>`:

```rust
    fn compile_while(&mut self, cond: Idx<Expr>, body: Idx<Expr>, line: u32) {
        let loop_start = self.current().chunk.code.len();
        self.compile_expr(cond);
        let to_end = self.current().emit_jump(Op::JumpIfFalse, line);
        self.current().loops.push(LoopCtx {
            body_base_local_count: self.current().local_count,
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
        self.current().loops.push(LoopCtx {
            body_base_local_count: self.current().local_count,
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
        self.current().local_count += 1;
        let zero_c = self.current().chunk.add_constant(Value::Int(0));
        self.emit_constant(zero_c, line); // hidden local: the counter, physical slot `base + 1`
        self.current().local_count += 1;
        self.current().emit_op(Op::Nil, line); // `binding` placeholder, physical slot `base + 2`
        self.current().local_count += 1;
        self.current().slot_shifts.push(SlotShift { base, extra: 2 });

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

        self.current().loops.push(LoopCtx {
            body_base_local_count: self.current().local_count,
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

        // Tear down the 3 hidden/visible locals this loop introduced.
        self.current().emit_op(Op::Pop, line);
        self.current().emit_op(Op::Pop, line);
        self.current().emit_op(Op::Pop, line);
        self.current().local_count -= 3;
        self.current().slot_shifts.pop();
    }

    fn compile_break(&mut self, line: u32) {
        let base = self
            .current()
            .loops
            .last()
            .expect("`break` outside a loop — the resolver must reject this before compilation")
            .body_base_local_count;
        self.emit_loop_cleanup_pops(base, line);
        let j = self.current().emit_jump(Op::Jump, line);
        self.current().loops.last_mut().expect("checked above").break_jumps.push(j);
    }

    fn compile_continue(&mut self, line: u32) {
        let base = self
            .current()
            .loops
            .last()
            .expect("`continue` outside a loop — the resolver must reject this before compilation")
            .body_base_local_count;
        self.emit_loop_cleanup_pops(base, line);
        let j = self.current().emit_jump(Op::Jump, line);
        self.current().loops.last_mut().expect("checked above").continue_jumps.push(j);
    }

    fn emit_loop_cleanup_pops(&mut self, body_base_local_count: u32, line: u32) {
        let live = self.current().local_count - body_base_local_count;
        for _ in 0..live {
            self.current().emit_op(Op::Pop, line);
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
    /// calls with `argc = 1`. Narrow and `for`-loop-specific: Task 13's
    /// general `Expr::Call` compiler does not call this, since it compiles
    /// an arbitrary AST `callee`/`args` list rather than a fixed native.
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged (likely candidates: the unused `_binding` parameter, and `compile_for`'s length — if clippy's `too_many_lines` fires, that's an acceptable, explicitly-justified exception for this one function given the desugaring it documents; do not split it against the grain just to silence the lint).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile while/loop/break/continue and desugar for-loops with slot-shift addressing"
```

---

## Task 11: `Assign` (Var/Index/Field targets), `List`, `Index`/`Field` reads, `Struct` → `MakeRecord`

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

`ember-resolve`'s `resolve_assign_target` only ever inserts a `Resolution` into `Bindings.resolutions` for a bare `Var` target — `Index`/`Field` targets instead resolve their `base` (and, for `Index`, `index`) as ordinary sub-expressions, leaving the target node itself unresolved. That asymmetry carries straight through to the compiler: assigning to a variable is a `GetLocal`/`Upvalue`/`Global`-style three-way dispatch (mirroring `compile_var`), while assigning to `a.b` or `a[i]` just compiles `base`/`index` normally and lets `OP_SET_FIELD`/`OP_SET_INDEX` do the write.

**`MakeRecord`'s field encoding:** Task 5's disassembler fixed the instruction's own operands at `<name_const_idx: u16> <field_count: u16>` — no room for per-field name operands. So field names travel as ordinary pushed values instead: for each field, push its name (a pooled `Value::Str` constant, exactly like `OP_GET_GLOBAL`'s name operand) immediately followed by its value expression, interleaved — `name1, value1, name2, value2, ...` — then `OP_MAKE_RECORD` pops `2 * field_count` stack values in those pairs. No bytecode format change needed; this was always compatible with what Task 5 already committed to.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

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
        let src = "struct P { x: Int } let mut p = P { x: 1 }; p.x = 2;";
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
        let src = "struct P { x: Int } let p = P { x: 1 }; p.x;";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_GET_FIELD"), "{out}");
        assert!(out.contains("\"x\""), "{out}");
    }

    #[test]
    fn struct_literal_pushes_interleaved_name_value_pairs_then_make_record() {
        let src = "struct P { x: Int, y: Int } P { x: 1, y: 2 };";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_MAKE_RECORD \"P\" fields=2"), "{out}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL — these `Expr` variants still fall through `compile_expr`'s `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add to `compile_expr`'s `match` (replacing the catch-all's coverage of these variants):

```rust
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
```

Add the new methods:

```rust
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile assignment targets, list literals, index/field reads, and struct literals"
```

---

## Task 12: Function/lambda compilation, upvalue capture, and `OP_CLOSE_UPVALUE`

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

This is the phase's most intricate task — it's where `Expr::Lambda`/`Stmt::Fn` become real `FunctionProto`s wrapped in `OP_CLOSURE`, and where captured locals finally get `OP_CLOSE_UPVALUE` instead of a plain `OP_POP` at scope exit. Three things make this harder than it looks, each addressed below: (1) `OP_CLOSE_UPVALUE` only makes sense once closures exist, so it's deferred to this task rather than Tasks 9/10, which are revised here; (2) the resolver's upvalue-capture `index` for a local capture is a raw, unshifted slot number that needs `physical_slot` translation if the capture happens inside a `for`-loop's shifted region; (3) `ember`'s blocks are expressions (they leave a tail value), which conflicts with `OP_CLOSE_UPVALUE`'s "operates on whatever's on top of the stack" semantics for the *first*-declared local in a scope specifically.

### Why Return doesn't need to emit `OP_CLOSE_UPVALUE` itself

`SPEC.md`'s own VM sketch (§11, `Vm::run`'s `Op::Return` arm) has the *VM* call `self.close_upvalues(frame.slot_base)` unconditionally whenever a frame returns — closing everything still open in that whole frame before truncating the stack. That's runtime behavior Phase 9 builds, not something this compiler needs to emit instructions for: a function's own parameters (and any of its own top-level body locals) get closed automatically for free when `OP_RETURN` runs, regardless of whether this compiler ever emits an explicit `OP_CLOSE_UPVALUE` for them. **`OP_CLOSE_UPVALUE` is therefore only needed for scope exits *short of* a full function return** — a `Block`, loop body, or `break`/`continue` unwind where the enclosing function keeps running. That's exactly what CHECKLIST.md's "`OP_CLOSE_UPVALUE` emitted at scope exit for every captured local" is asking for, and exactly the set of sites this task revises.

### The block-tail problem, and its fix

Task 9's block scope-exit trick (`OP_SET_LOCAL` the tail down into the first local's slot, then plain-pop the rest) *silently overwrites* that first local's slot rather than popping it — fine for a dead value nobody will read again, wrong if it's captured: an open upvalue needs to be closed using the local's *real* value, and by the time `OP_SET_LOCAL` has run, that value is gone. The fix: if the block's *first*-declared local is captured, close it explicitly *before* computing the tail at all, via a harmless duplicate-and-discard — `OP_GET_LOCAL <entry>` (push a copy), `OP_CLOSE_UPVALUE` (close using that copy, pop it back off). The original slot's stack *storage* still holds the value after this — that's fine, it's about to be overwritten by the tail anyway, only the closing itself had to happen while the value was still correct. Every *other* declared local in the block (i.e. every one but the first) genuinely does get popped from the true top of the stack in the existing trick's cleanup loop, with its real value intact — those substitute `OP_CLOSE_UPVALUE` for `OP_POP` directly, no special-casing needed. (The very first pop in that cleanup loop, immediately after `OP_SET_LOCAL`, is never a real local either way — it's discarding `OP_SET_LOCAL`'s own leftover duplicate of the tail — so it's always a plain `OP_POP`.)

### Upvalue index translation for `for`-loop captures

`Bindings.upvalues[function_id]` entries with `is_local: true` mean "capture slot `index` from the *immediately enclosing* function's own locals" — but if that capture happens from inside a `for`-loop's desugared body, `index` is the resolver's raw, unshifted slot number, and the physical stack position is `index` plus whatever `SlotShift`s are active in the *enclosing* `FunctionCompiler` at the point the closure is created. `compile_function` translates every `is_local: true` entry through `self.current().physical_slot(...)` (evaluated in the enclosing frame, before pushing the new one) before baking it into the new `FunctionProto`. `is_local: false` entries (chained capture from the enclosing closure's own upvalue array) need no translation — they're a plain `Vec` index, never a stack slot.

- [ ] **Step 1: Retrofit `FunctionCompiler` — add `declared_slots` and `total_shift`**

In `FunctionCompiler`'s definition (Task 7), add the field:

```rust
pub struct FunctionCompiler {
    pub(crate) function_id: FunctionId,
    pub(crate) chunk: Chunk,
    pub(crate) local_count: u32,
    pub(crate) scope_depth: u32,
    pub(crate) loops: Vec<LoopCtx>,
    pub(crate) slot_shifts: Vec<SlotShift>,
    pub(crate) declared_slots: Vec<Option<u32>>,
    pub(crate) stack_depth: i32,
}
```

Add `declared_slots: Vec::new(),` to `FunctionCompiler::new`'s struct literal. And add a method alongside `physical_slot`:

```rust
    /// Sum of every active `for`-loop shift's `extra` — used to recover
    /// the resolver's original, unshifted slot number for a local being
    /// declared *right now* (its physical push position minus however much
    /// shifting is currently in effect).
    pub(crate) fn total_shift(&self) -> u32 {
        self.slot_shifts.iter().map(|s| s.extra).sum()
    }
```

- [ ] **Step 2: Retrofit Task 9's `compile_block` and `Stmt::Let` arm — captured-local-aware scope exit**

Replace `compile_block` entirely with:

```rust
    fn compile_block(&mut self, stmts: &[Idx<ember_ast::Stmt>], tail: Option<Idx<Expr>>, line: u32) {
        let entry_local_count = self.current().local_count;
        self.current().scope_depth += 1;
        for &s in stmts {
            self.compile_stmt(s);
        }
        let declared = (self.current().local_count - entry_local_count) as usize;
        let base_idx = self.current().declared_slots.len() - declared;

        if declared > 0 {
            if let Some(first_slot) = self.current().declared_slots[base_idx] {
                if self.slot_is_captured(first_slot) {
                    self.emit_get_local(entry_local_count, line);
                    self.current().emit_op(Op::CloseUpvalue, line);
                }
            }
        }

        match tail {
            Some(t) => self.compile_expr(t),
            None => self.current().emit_op(Op::Nil, line),
        }

        if declared > 0 {
            self.current().emit_op(Op::SetLocal, line);
            self.current().chunk.write_u8(entry_local_count as u8, line);
            self.current().emit_op(Op::Pop, line); // SetLocal's leftover tail-duplicate — never a real local
            for i in (1..declared).rev() {
                let slot = self.current().declared_slots[base_idx + i];
                let captured = slot.map(|s| self.slot_is_captured(s)).unwrap_or(false);
                self.current().emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
            }
            self.current().declared_slots.truncate(base_idx);
        }
        self.current().local_count = entry_local_count;
        self.current().scope_depth -= 1;
    }

    fn slot_is_captured(&self, resolver_slot: u32) -> bool {
        let function_id = self.functions.last().expect("at least one FunctionCompiler").function_id;
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
            let captured = resolver_slot.map(|s| self.slot_is_captured(s)).unwrap_or(false);
            self.current().emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
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
```

Replace `Stmt::Let`'s arm in `compile_stmt` (Task 9) to use the shared helper:

```rust
            Stmt::Let { name, init, .. } => {
                self.compile_expr(init);
                self.declare_named_local(name, line);
            }
```

Delete the old free-standing `maybe_dual_register` slot computation (`let slot = self.current().local_count - 1;`) — it stays as its own method, just no longer duplicated inline; `declare_named_local` is now the only caller that increments `local_count` before invoking it, so `maybe_dual_register` itself is unchanged.

- [ ] **Step 3: Retrofit Task 10's `for`/`while`/`loop`/`break`/`continue` — route through `push_local`/`emit_scope_pops`**

In `compile_for`, replace the three hidden-local pushes:

```rust
        self.compile_expr(iter);
        self.push_local(None); // hidden: the iterable
        let zero_c = self.current().chunk.add_constant(Value::Int(0));
        self.emit_constant(zero_c, line);
        self.push_local(None); // hidden: the counter
        self.current().emit_op(Op::Nil, line);
        self.push_local(Some(base)); // `binding` — its real resolver slot is `base`
        self.current().slot_shifts.push(SlotShift { base, extra: 2 });
```

(replacing the three `local_count += 1` lines). And replace the teardown at the very end of `compile_for`:

```rust
        self.emit_scope_pops(3, line);
        self.current().slot_shifts.pop();
```

(replacing the three explicit `Op::Pop` emissions and the `local_count -= 3` line).

In `compile_while`, `compile_loop`, and `compile_for`'s own post-body cleanup, `Op::Pop, line);` immediately after `self.compile_expr(body);` (discarding the body-as-statement's `Block` value) is unrelated to this substitution — that's popping the block's *own* already-self-contained tail, not a loop-owned local, and stays a plain `Op::Pop` exactly as Task 10 wrote it.

Replace `emit_loop_cleanup_pops` (used by `compile_break`/`compile_continue`) to substitute correctly:

```rust
    fn emit_loop_cleanup_pops(&mut self, body_base_local_count: u32, line: u32) {
        let live = self.current().local_count - body_base_local_count;
        self.emit_scope_pops(live, line);
    }
```

- [ ] **Step 4: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)
    use ember_resolve::UpvalueDesc;

    #[test]
    fn a_lambda_compiles_to_op_closure_referencing_a_function_proto() {
        let (out, _) = compile_program_str("let f = || 1;");
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    #[test]
    fn a_named_fn_compiles_to_op_closure_and_becomes_a_named_local() {
        let (out, _) = compile_program_str("fn f() { 1 }");
        assert!(out.contains("OP_CLOSURE"), "{out}");
        assert!(out.contains("OP_DEFINE_GLOBAL"), "top-level fn also gets dual registration: {out}");
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
        let src = "fn outer() { let x = 1; fn inner() { fn innermost() { x } innermost() } inner() }";
        let (out, _) = compile_program_str(src);
        let closure_count = out.matches("OP_CLOSURE").count();
        assert_eq!(closure_count, 3, "outer, inner, and innermost each get one: {out}");
    }
}
```

- [ ] **Step 5: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL — `Expr::Lambda`/`Stmt::Fn` still fall through the `unimplemented!` catch-alls, and the new helper methods don't exist yet.

- [ ] **Step 6: Implement `compile_function` and wire up `Lambda`/`Fn`**

Add to `compile_expr`'s `match` (replacing the catch-all's coverage of `Lambda`):

```rust
            Expr::Lambda { params, body } => {
                let anon_name = self.interner.intern("<lambda>");
                self.compile_function(FunctionId::Lambda(idx), &params, body, anon_name, line);
            }
```

Add to `compile_stmt`'s `match` (replacing the catch-all's coverage of `Fn`):

```rust
            Stmt::Fn { name, params, body, .. } => {
                self.compile_function(FunctionId::Fn(idx), &params, body, name, line);
                self.declare_named_local(name, line);
            }
```

Add `compile_function` itself:

```rust
    fn compile_function(
        &mut self,
        function_id: FunctionId,
        params: &[ember_ast::Param],
        body: Idx<Expr>,
        name: Symbol,
        line: u32,
    ) {
        let raw_upvalues = self.bindings.upvalues.get(&function_id).cloned().unwrap_or_default();
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
```

- [ ] **Step 7: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 8: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile functions/lambdas into FunctionProto+OP_CLOSURE with upvalue capture and OP_CLOSE_UPVALUE"
```

---

## Task 13: `Expr::Call` compilation

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`

The general case Task 10's `emit_len_call` deliberately didn't try to be: compile an arbitrary callee expression (a `Var`, a nested call, any expression that evaluates to a callable), then each argument in order, then `OP_CALL <argc>`. `emit_len_call` stays exactly as Task 10 wrote it — it's a fixed, narrow "call a known native by name" helper the `for`-loop desugaring still uses directly; this task doesn't touch it.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

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
        let constant_lines = out.lines().take(call_line).filter(|l| l.contains("OP_CONSTANT")).count();
        assert!(constant_lines >= 2, "both 1 and 2 must be pushed before the call: {out}");
    }

    #[test]
    fn a_nested_call_compiles_the_inner_call_as_part_of_computing_the_callee_or_an_arg() {
        let src = "let f = || || 1; f()();";
        let (out, _) = compile_program_str(src);
        assert_eq!(out.matches("OP_CALL argc=0").count(), 2, "{out}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL — `Expr::Call` still falls through the `unimplemented!` catch-all.

- [ ] **Step 3: Implement**

Add to `compile_expr`'s `match` (replacing the catch-all's coverage of `Call`):

```rust
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile function calls"
```

---

## Task 14: ADT/`struct` declarations and pattern-match compilation

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`
- Modify: `crates/ember-compile/src/compiler.rs` (retrofit `compile_block`, see Step 1)

This is the phase's largest remaining task: `Stmt::TypeDecl`/`Stmt::StructDecl` (registering ADT constructors), and `Expr::Match` (compiling every `Pattern` variant into `TestVariant`/`Destructure`/`GetField`/`GetIndex` chains). It also extracts a shared helper (`emit_tail_scope_exit`) out of `compile_block` — needed twice more here (once per match arm, once for the whole match) — and fixes an unavoidable ordering hazard in `OP_CLOSE_UPVALUE`'s interaction with pattern-matching temporaries.

### Two-pass pattern compilation: separating "does it match" from "bind the names"

A naive single-pass compiler that tests and binds a pattern together runs into a real problem with backtracking constructs (`Or`, and generally any pattern whose test can fail *after* a sibling sub-pattern already bound something): if a partial match fails, whatever bindings it already made are sitting on the stack in slots the compiler now needs to either keep or discard *consistently*, and a naive design leaves that inconsistent across different failure points.

The fix used throughout this task: every pattern compiles through **two separate passes**.

- **`compile_pattern_test`** — pushes exactly one `Bool`, has **no other side effects** (in particular, no `Bind` pattern declares anything here — `Bind`/`Wild`/`Error` all just push `True`, trivially). Because nothing is bound, a failed test needs no cleanup beyond popping that one `Bool` — which `OP_JUMP_IF_FALSE` already does as part of testing it.
- **`compile_pattern_bind`** — only ever invoked on a control-flow path that a `compile_pattern_test` of the *same* pattern has already confirmed `true`. It's where every `Bind` becomes a real local (via `declare_named_local`, same as `let`).

This is what makes `Or`-patterns tractable: each alternative is tried as an independent `test → (if true) bind → jump to success` unit; a failed alternative's `test` never bound anything, so trying the next alternative needs no rollback at all.

### The `OP_CLOSE_UPVALUE` / short-circuit ordering hazard

`compile_pattern_test`'s `Ctor`/`Record`/`List` cases need a hidden temp local per value-needing sub-pattern (to destructure into and recurse on) — but that temp's own test result (a `Bool`, from the recursive call) ends up sitting *on top of* the temp, and `OP_JUMP_IF_FALSE` needs to test that `Bool`, not the temp underneath it. Each such site closes this gap immediately with `emit_tail_scope_exit(temp_slot, line)` — the exact same "relocate-then-pop" trick `compile_block` already uses for its own tail value — collapsing `[temp, bool]` down to just `[bool]` at `temp_slot`'s position *before* the `JumpIfFalse` that tests it. Doing this **before** every single-arg `JumpIfFalse` (not batched at the end) is what keeps every one of a `Ctor`/`Record`/`List` pattern's several possible failure jumps landing at the *same* stack depth — a prerequisite for them sharing one `fail_jumps` list. `compile_pattern_bind`, by contrast, never branches at all (it's pure straight-line binding once a match is confirmed), so its own hidden temps are simply left live and swept up later by the arm's enclosing `emit_tail_scope_exit` call — no per-temp cleanup needed there.

### Known, explicitly out-of-scope gaps in this task

- **`Pattern::Tuple`** still compiles to "never matches" (`OP_FALSE`) — the inertness carried since Phase 5/6/7 (no `Value::Tuple` exists anywhere in this pipeline). Not newly introduced here.
- **`Pattern::List`'s `rest` binding** does *not* bind the real remaining sublist — this is a genuinely **new** gap (the tree-walker, Phase 7, supports it correctly). Building a real sublist at runtime needs some way to construct a list of a *runtime-determined* length, and `Op::MakeList`'s count operand is fixed at compile time — there is no `slice`/`tail` opcode or native in this pipeline to fall back on either. This task still tests the length/prefix correctly (`len(xs) >= items.len()`, plus each fixed-position item), but a `rest` binding (if it's a plain `Bind`) is declared as `Nil` — wrong value, but the resolver slot is still correctly reserved, so nothing declared afterward in the same scope misaligns. A future phase should add either opcode. This is flagged here, not silently dropped, and should be carried into this task's own honest note in `CHECKLIST.md`'s final reconciliation (Task 17).

- [ ] **Step 1: Extract `emit_tail_scope_exit` from `compile_block` (Task 9/12)**

Replace `compile_block` (as it stands after Task 12's captured-local retrofit) with:

```rust
    fn compile_block(&mut self, stmts: &[Idx<ember_ast::Stmt>], tail: Option<Idx<Expr>>, line: u32) {
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
    /// with the first-declared local pre-closed separately if needed —
    /// see this task's own note on why) while leaving exactly the tail
    /// value at `entry_local_count`'s physical stack position. Shared by
    /// `compile_block`, and (this task) by every match arm's own bindings
    /// plus the whole match's hidden scrutinee local.
    fn emit_tail_scope_exit(&mut self, entry_local_count: u32, line: u32) {
        let declared = (self.current().local_count - entry_local_count) as usize;
        if declared == 0 {
            return;
        }
        let base_idx = self.current().declared_slots.len() - declared;
        if let Some(first_slot) = self.current().declared_slots[base_idx] {
            if self.slot_is_captured(first_slot) {
                self.emit_get_local(entry_local_count, line);
                self.current().emit_op(Op::CloseUpvalue, line);
            }
        }
        self.current().emit_op(Op::SetLocal, line);
        self.current().chunk.write_u8(entry_local_count as u8, line);
        self.current().emit_op(Op::Pop, line); // SetLocal's leftover tail-duplicate — never a real local
        for i in (1..declared).rev() {
            let slot = self.current().declared_slots[base_idx + i];
            let captured = slot.map(|s| self.slot_is_captured(s)).unwrap_or(false);
            self.current().emit_op(if captured { Op::CloseUpvalue } else { Op::Pop }, line);
        }
        self.current().declared_slots.truncate(base_idx);
        self.current().local_count = entry_local_count;
    }
```

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)

    #[test]
    fn a_nullary_adt_variant_compiles_to_make_adt_and_becomes_a_named_local() {
        let src = "type Shape = Circle(Float) | Origin";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_MAKE_ADT \"Shape\"::\"Origin\" arity=0"), "{out}");
    }

    #[test]
    fn a_payload_adt_variant_compiles_to_a_synthetic_closure() {
        let src = "type Shape = Circle(Float)";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_CLOSURE"), "{out}");
    }

    #[test]
    fn a_ctor_pattern_tests_the_variant_tag_and_destructures_its_payload() {
        let src = "type Shape = Circle(Float) match Circle(1.0) { Circle(r) => r, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_TEST_VARIANT"), "{out}");
        assert!(out.contains("OP_DESTRUCTURE"), "{out}");
    }

    #[test]
    fn a_wildcard_fallback_arm_always_matches() {
        let src = "match 1 { 2 => 20, _ => 0, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_EQUAL"), "the literal arm still tests: {out}");
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
        let src = "struct P { x: Int } match P { x: 1 } { P { x } => x, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_GET_FIELD"), "{out}");
    }

    #[test]
    fn a_tuple_pattern_still_never_matches() {
        let src = "match 1 { (a, b) => 1, _ => 0, }";
        let (out, _) = compile_program_str(src);
        assert!(out.contains("OP_FALSE"), "{out}");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL — `Stmt::TypeDecl`/`StructDecl` and `Expr::Match` still fall through their `unimplemented!` catch-alls, and `emit_tail_scope_exit`/pattern-compiling methods don't exist yet.

- [ ] **Step 4: Implement — ADT/struct declarations**

Add to `compile_stmt`'s `match` (replacing the catch-all's coverage of `TypeDecl`/`StructDecl`):

```rust
            Stmt::TypeDecl { name, variants } => self.compile_type_decl(name, variants, line),
            Stmt::StructDecl { name, .. } => {
                // The struct's own name consumes a resolver slot (hoisted
                // alongside `fn`/ADT names) but is never read via any Var
                // node — `Expr::Struct` is its own AST node, constructed
                // directly at each literal site (Task 11), not by calling
                // a value bound to the struct's name. A Nil placeholder
                // just reserves the slot to keep local_count aligned.
                self.current().emit_op(Op::Nil, line);
                self.declare_named_local(name, line);
            }
```

Add `compile_type_decl`:

```rust
    fn compile_type_decl(&mut self, name: Symbol, variants: Vec<ember_ast::AdtVariant>, line: u32) {
        // Same reasoning as StructDecl above: the type's own name is never
        // read via a Var node, only its variants' constructors are.
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
```

- [ ] **Step 5: Implement — pattern compilation**

Add to `compile_expr`'s `match` (replacing the catch-all's coverage of `Match` — this is the last variant it covers, so the catch-all arm can be deleted entirely once this lands):

```rust
            Expr::Match { scrutinee, arms } => self.compile_match(scrutinee, arms, line),
```

Add the pattern-compiling methods:

```rust
    fn pattern_needs_value(&self, pat: Idx<ember_ast::Pattern>) -> bool {
        !matches!(self.ast.pat(pat), Pattern::Wild | Pattern::Bind(_) | Pattern::Error)
    }

    /// Pushes exactly one `Bool`. No bindings, no other side effects.
    fn compile_pattern_test(&mut self, pat: Idx<ember_ast::Pattern>, scrutinee_slot: u32, line: u32) {
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
    /// jump was avoided, `False` if any of them landed here — all landing
    /// at the same depth, by construction (see this task's own note on
    /// why each `JumpIfFalse` is preceded by an immediate `emit_tail_scope_exit`).
    fn finish_and_chain(&mut self, fail_jumps: Vec<usize>, line: u32) {
        self.current().emit_op(Op::True, line);
        let to_end = self.current().emit_jump(Op::Jump, line);
        for j in fail_jumps {
            self.current().patch_jump(j);
        }
        self.current().emit_op(Op::False, line);
        self.current().patch_jump(to_end);
    }

    /// One "extract a sub-value and bind (or recurse into) its pattern"
    /// step, shared by `Ctor`/`Record`/`List` binding below.
    fn compile_destructured_bind(&mut self, sub_pat: Idx<ember_ast::Pattern>, scrutinee_slot: u32, source: DestructureSource, line: u32) {
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

    /// Declares a new local for every `Bind` in `pat` — only ever called
    /// once `compile_pattern_test` of this same `pat` has already
    /// returned `true` on this control-flow path. No branching here at
    /// all, so hidden temps are simply left live for the enclosing scope's
    /// own `emit_tail_scope_exit` to sweep up.
    fn compile_pattern_bind(&mut self, pat: Idx<ember_ast::Pattern>, scrutinee_slot: u32, line: u32) {
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
                    self.compile_destructured_bind(arg_pat, scrutinee_slot, DestructureSource::Positional(i as u8), line);
                }
            }
            Pattern::Record { fields, .. } => {
                for (fname, fpat) in fields {
                    self.compile_destructured_bind(fpat, scrutinee_slot, DestructureSource::Named(fname), line);
                }
            }
            Pattern::List { items, rest } => {
                for (i, &item_pat) in items.iter().enumerate() {
                    self.compile_destructured_bind(item_pat, scrutinee_slot, DestructureSource::Indexed(i as i64), line);
                }
                if let Some(rest_pat) = rest {
                    if let Pattern::Bind(sym) = self.ast.pat(rest_pat) {
                        let sym = *sym;
                        // Known gap (see this task's header note): a Nil
                        // placeholder, not the real remaining sublist —
                        // only the slot is reserved.
                        self.current().emit_op(Op::Nil, line);
                        self.declare_named_local(sym, line);
                    }
                }
            }
            Pattern::Or(_) => unreachable!("bound by whichever alternative matched — see compile_pattern_match"),
        }
    }

    /// Tests, then (only on success) binds, one match arm's pattern. On
    /// failure, pushes a jump address into `fail_jumps` for the caller to
    /// patch once it knows where "try the next arm" begins. `Or` gets its
    /// own control flow here — each alternative is an independent
    /// test/bind/jump-to-success unit, so a failed alternative (which
    /// never bound anything, by the test/bind split) costs nothing to
    /// abandon before trying the next one.
    fn compile_pattern_match(&mut self, pat: Idx<ember_ast::Pattern>, scrutinee_slot: u32, fail_jumps: &mut Vec<usize>, line: u32) {
        if let Pattern::Or(alts) = self.ast.pat(pat).clone() {
            let mut end_jumps = Vec::new();
            for &alt in &alts {
                self.compile_pattern_test(alt, scrutinee_slot, line);
                let this_fails = self.current().emit_jump(Op::JumpIfFalse, line);
                self.compile_pattern_bind(alt, scrutinee_slot, line);
                end_jumps.push(self.current().emit_jump(Op::Jump, line));
                self.current().patch_jump(this_fails);
                // falls through to the next alternative's test (or, after
                // the last alternative, straight into the line below).
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

    fn compile_match(&mut self, scrutinee: Idx<Expr>, arms: Vec<ember_ast::MatchArm>, line: u32) {
        self.compile_expr(scrutinee);
        let match_entry_local_count = self.current().local_count;
        self.push_local(None);
        let scrutinee_slot = match_entry_local_count;

        let mut end_jumps = Vec::new();
        let mut prev_fail_jumps: Vec<usize> = Vec::new();

        for arm in &arms {
            for j in prev_fail_jumps.drain(..) {
                self.current().patch_jump(j);
            }
            let arm_entry_local_count = self.current().local_count;
            let mut fail_jumps = Vec::new();
            self.compile_pattern_match(arm.pat, scrutinee_slot, &mut fail_jumps, line);

            if let Some(guard) = arm.guard {
                self.compile_expr(guard);
                fail_jumps.push(self.current().emit_jump(Op::JumpIfFalse, line));
            }

            self.compile_expr(arm.body);
            self.emit_tail_scope_exit(arm_entry_local_count, line);
            end_jumps.push(self.current().emit_jump(Op::Jump, line));

            prev_fail_jumps = fail_jumps;
        }
        for j in prev_fail_jumps {
            self.current().patch_jump(j);
        }
        // Unreachable in a program that passed Phase 6's exhaustiveness
        // check — a defined fallback keeps the chunk well-formed rather
        // than falling off the end of it.
        self.current().emit_op(Op::Nil, line);

        for j in end_jumps {
            self.current().patch_jump(j);
        }
        self.emit_tail_scope_exit(match_entry_local_count, line);
    }
```

Add the small `DestructureSource` enum near the top of the file (alongside the other type definitions):

```rust
enum DestructureSource {
    Positional(u8),
    Named(Symbol),
    Indexed(i64),
}
```

Delete `compile_expr`'s `unimplemented!` catch-all arm entirely — by this point every `Expr` variant has an explicit arm.

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged (a `too_many_lines`/`too_many_arguments`-style lint on `compile_match` or the `compile_pattern_test` match is an acceptable, explicitly-justified exception here — do not fragment this against the grain of the design just to silence it).

- [ ] **Step 7: Commit**

```bash
git add crates/ember-compile
git commit -m "Compile ADT/struct declarations and full pattern-match with two-pass test/bind"
```

---

## Task 15: Public `compile()` entry point, top-level two-pass hoisting, and crate exports

**Files:**
- Modify: `crates/ember-compile/src/compiler.rs`
- Modify: `crates/ember-compile/src/lib.rs`

### Why the top-level needs its own two-pass hoisting — this isn't just a forward-reference nicety, it's required for slot alignment

`ember-resolve`'s `resolve_program` (`crates/ember-resolve/src/resolver.rs`) hoists every top-level `Fn`/`TypeDecl`(+its variants)/`StructDecl` **name** in a dedicated first pass — calling `declare()` on each, which consumes a resolver slot — *before* its second pass walks every statement (including the now-already-declared `Fn`/`TypeDecl`/`StructDecl` ones, which it skips re-declaring) in source order. That means the resolver's real slot-assignment order is "every hoisted name, in source order among themselves, first — then every other declaration (`let`), in source order, afterward" — which is **not** the same as literal top-to-bottom source order whenever a `let` appears before a `fn`/`type`/`struct` in the file. `compile()` mirrors this exactly: pass 1 compiles every top-level `Fn`/`TypeDecl`/`StructDecl` (their `OP_CLOSURE`/`OP_MAKE_ADT` + `OP_DEFINE_GLOBAL`), pass 2 compiles everything else, in source order. Anything less would misalign this compiler's `local_count` bookkeeping against the resolver's actual slot numbers for the very next `let` after a hoisted declaration.

This also happens to be exactly what makes mutual recursion between top-level functions work at runtime: `ember-tree::interpret` (Phase 7) does the identical two-pass hoist for the identical reason (`crates/ember-tree/src/interp.rs`'s own top-level loop), so both backends agree on when a forward-referenced top-level `fn` becomes callable.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    // ... (existing tests stay above this line)
    use ember_compile::compile;

    #[test]
    fn compile_hoists_fn_declarations_before_other_top_level_code() {
        let src = "let x = a(); fn a() { 1 }";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "{resolve_diags:?}");
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        let closure_line = out.lines().position(|l| l.contains("OP_CLOSURE")).unwrap();
        let call_line = out.lines().position(|l| l.contains("OP_CALL")).unwrap();
        assert!(closure_line < call_line, "a's OP_CLOSURE must be emitted before x's initializer calls it: {out}");
    }

    #[test]
    fn compile_ends_the_top_level_chunk_with_return() {
        let src = "let x = 1;";
        let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
        assert!(parse_diags.is_empty(), "{parse_diags:?}");
        let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
        assert!(resolve_diags.is_empty(), "{resolve_diags:?}");
        let proto = compile(&ast, &mut interner, &bindings, &stmts);
        let out = ember_bytecode::disasm::disassemble_chunk(&proto.chunk, "test", &interner);
        assert!(out.trim_end().lines().last().unwrap().contains("OP_RETURN"), "{out}");
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
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ember-compile`
Expected: FAIL to compile — `compile` isn't exported from `ember-compile`'s crate root yet, and doesn't exist as a public free function in `compiler.rs` yet.

- [ ] **Step 3: Implement**

Add to `compiler.rs`:

```rust
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
            Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. }
        ) {
            compiler.compile_stmt(s);
        }
    }
    for &s in stmts {
        if !matches!(
            compiler.ast.stmt(s),
            Stmt::Fn { .. } | Stmt::TypeDecl { .. } | Stmt::StructDecl { .. }
        ) {
            compiler.compile_stmt(s);
        }
    }

    compiler.current().emit_op(Op::Nil, 0);
    compiler.current().chunk.write_op(Op::Return, 0);
    compiler.current().adjust_depth(-1);

    let fc = compiler.functions.pop().expect("top-level FunctionCompiler always present");
    let script_name = compiler.interner.intern("<script>");
    ember_bytecode::chunk::FunctionProto {
        chunk: fc.chunk,
        arity: 0,
        upvalues: Vec::new(),
        name: script_name,
    }
}
```

Update `lib.rs`:

```rust
//! AST-to-bytecode compiler.

pub mod compiler;

pub use compiler::{compile, Compiler};
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ember-compile`
Expected: PASS. Run `cargo clippy -p ember-compile --all-targets -- -D warnings` and `cargo fmt -p ember-compile -- --check`, fix anything flagged.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-compile
git commit -m "Add the public compile() entry point with top-level two-pass hoisting"
```

---

## Task 16: Conformance suite infrastructure

**Files:**
- Create: `tests/conformance/arithmetic.em`, `tests/conformance/arithmetic.expected`
- Create: `tests/conformance/control_flow.em`, `tests/conformance/control_flow.expected`
- Create: `tests/conformance/list_and_for.em`, `tests/conformance/list_and_for.expected`
- Create: `tests/conformance/structs.em`, `tests/conformance/structs.expected`
- Create: `tests/conformance/adt_and_match.em`, `tests/conformance/adt_and_match.expected`
- Create: `tests/conformance/closures.em`, `tests/conformance/closures.expected`
- Create: `crates/ember-cli/tests/conformance.rs`

This phase can't yet run the actual cross-backend comparison the design doc describes (tree-walker vs. compile+VM) — there's no VM until Phase 9. What this task builds is the **convention and the tree-walker side of it**: a `tests/conformance/` directory at the workspace root holding `.em`/`.expected` pairs, and a harness (living in `ember-cli`'s own integration tests, since it already depends on every crate needed to run the full parse→resolve→infer→exhaustiveness→interpret pipeline) that runs every pair through the tree-walker and asserts the output matches. Phase 9 extends this exact same harness to also run each program through `ember-compile`+the new VM and assert *that* output matches too — this task's fixtures and `.expected` files don't need to change for that, only the harness gains a second assertion.

- [ ] **Step 1: Write the fixtures**

`tests/conformance/arithmetic.em`:
```
let a = 3;
let b = 4;
a * a + b * b;
```

`tests/conformance/arithmetic.expected`:
```
25
```

`tests/conformance/control_flow.em`:
```
let x = 10;
if x > 5 { "big" } else { "small" };
```

`tests/conformance/control_flow.expected`:
```
big
```

`tests/conformance/list_and_for.em`:
```
let xs = [1, 2, 3, 4, 5];
let mut total = 0;
for x in xs { total = total + x; }
total;
```

`tests/conformance/list_and_for.expected`:
```
15
```

`tests/conformance/structs.em`:
```
struct Point { x: Int, y: Int }
let p = Point { x: 3, y: 4 };
p.x + p.y;
```

`tests/conformance/structs.expected`:
```
7
```

`tests/conformance/adt_and_match.em`:
```
type Shape = Circle(Float) | Square(Float)

fn area(s) {
    match s {
        Circle(r) => 3.14 * r * r,
        Square(side) => side * side,
    }
}

area(Square(4.0));
```

`tests/conformance/adt_and_match.expected`:
```
16
```

`tests/conformance/closures.em`:
```
fn make_counter() {
    let mut count = 0;
    || { count = count + 1; count }
}

let counter = make_counter();
counter();
counter();
counter();
```

`tests/conformance/closures.expected`:
```
3
```

- [ ] **Step 2: Write the harness**

```rust
// crates/ember-cli/tests/conformance.rs
use ember_diag::Severity;
use std::fs;
use std::path::PathBuf;

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

fn has_errors(diags: &[ember_diag::Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

#[test]
fn tree_walker_output_matches_every_captured_fixture() {
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

        let mut resolver = ember_resolve::Resolver::new(&ast, &mut interner);
        resolver.resolve_program(&stmts);
        let resolve_diags = resolver.diagnostics().to_vec();
        assert!(!has_errors(&resolve_diags), "{path:?}: resolve diags: {resolve_diags:?}");

        let (info, infer_diags) = ember_types::infer(&ast, &mut interner, &stmts);
        assert!(!has_errors(&infer_diags), "{path:?}: infer diags: {infer_diags:?}");

        let exhaustive_diags = ember_types::check_exhaustiveness(&ast, &interner, &info, &stmts);
        assert!(!has_errors(&exhaustive_diags), "{path:?}: exhaustiveness diags: {exhaustive_diags:?}");

        let (result, err) = ember_tree::interpret(&ast, &interner, &stmts);
        assert!(err.is_none(), "{path:?}: unexpected runtime error: {err:?}");
        let actual = match result {
            Some(v) => ember_tree::display_value(&v, &interner),
            None => String::new(),
        };
        assert_eq!(actual.trim(), expected.trim(), "{path:?}: output mismatch");
        checked += 1;
    }
    assert!(checked >= 6, "expected at least 6 conformance fixtures, found {checked} in {dir:?}");
}
```

- [ ] **Step 3: Run to verify pass**

Run: `cargo test -p ember-cli --test conformance`
Expected: PASS, with `checked == 6`. If any fixture's actual output doesn't match its `.expected` file (most likely culprit: a hand-computed `.expected` value doesn't match `ember_tree::display_value`'s real formatting, e.g. a float rendering), fix the `.expected` file to match the tree-walker's real, verified-correct output — the fixtures document actual current behavior, not a hoped-for one.

- [ ] **Step 4: Commit**

```bash
git add tests/conformance crates/ember-cli/tests/conformance.rs
git commit -m "Add conformance suite infrastructure with tree-walker fixtures"
```

---

## Task 17: Final verification and `CHECKLIST.md` reconciliation

**Not delegated to a subagent** — done directly, same as every phase's final task so far.

- [ ] Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all -- --check`; fix anything that surfaces.
- [ ] Read `CHECKLIST.md`'s Phase 8 section and check off every item that's genuinely done, item by item — not a blanket check.
- [ ] Add honest notes for every deliberate scope decision this plan made beyond (or short of) the checklist's literal wording, at minimum:
  - The top-level dual-registration design (every top-level declaration is both a `Local` and a `Global`).
  - The `for`-loop desugaring to an index-counter `while`, and the `SlotShift`/`physical_slot` mechanism it required.
  - `OP_CLOSE_UPVALUE` is emitted only for scope exits short of a full function return (`Block`, loop cleanup, `break`/`continue`) — a function's own parameters/locals are closed for free by the VM's own `OP_RETURN` handling (per `SPEC.md`'s own sketch), not by anything this compiler emits.
  - Two-pass (test/bind) pattern compilation, and why it was needed for `Or`-patterns.
  - `Pattern::Tuple` inertness (carried forward, not new).
  - `Pattern::List`'s `rest` binding gap (**new**, not carried forward — flag this distinctly from Tuple's).
  - The `len` native called directly by the `for`-loop/list-pattern compiler via a narrow `emit_len_call` helper, bypassing the general `Expr::Call` path.
  - `ember-compile` walks the *resolved* AST (`ember-resolve::Bindings`), not the *typed* AST (`ember-types::TypeInfo`) — a deliberate deviation from `CHECKLIST.md`'s literal "walk the typed AST" wording, decided and approved during this phase's own design doc (`docs/superpowers/specs/2026-08-06-ember-phase8-bytecode-compiler-design.md`): opcodes are generic/untyped, checked at runtime by the VM, so nothing this phase compiles needs `ember-types`' output.
  - `OP_CLOSURE`'s upvalue descriptor list lives in `FunctionProto.upvalues` (a Rust-level field on the constant-pooled function, set once at compile time), not inline in the bytecode stream as `CHECKLIST.md`'s literal wording suggests — functionally equivalent (the VM reads the same descriptors either way) but a real deviation worth naming explicitly, not silently matching the checklist's letter while diverging from its intent.
  - Implicit `nil` return when a function falls off the end: confirmed satisfied by construction, not a special case — every function body compiles as an `Expr::Block` (Task 9's `compile_block` always pushes `Nil` when there's no `tail`), so `compile_function` (Task 12) never needs to special-case an empty/fall-through body.
  - "Assert stack effect balance per statement" is wired up as a `debug_assert_eq!` wrapping every `compile_stmt` call (added in Task 9, automatically covering every statement kind later tasks add), not a scattered set of per-construct checks.
  - "Disassembly snapshots for 15 programs" is satisfied cumulatively — every task from 8 onward asserts against real disassembler output for its own new constructs; count the actual test total across `ember-compile` here rather than expecting one dedicated 15-program batch.
- [ ] Verify the final `git log` for this phase reads as a clean, coherent history (no leftover WIP-sounding messages).

---
