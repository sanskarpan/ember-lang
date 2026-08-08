use crate::op::Op;
use crate::value::Value;
use ember_ast::Symbol;
use ember_resolve::UpvalueDesc;
use std::rc::Rc;

#[derive(Default)]
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    /// One compiled function per `OP_CLOSURE` this chunk emits, referenced
    /// by index (a SEPARATE pool from `constants` — a `FunctionProto`
    /// isn't a `Value`, it's a whole nested `Chunk` plus metadata).
    pub functions: Vec<Rc<FunctionProto>>,
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
        self.functions.push(Rc::new(proto));
        (self.functions.len() - 1) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::Op;
    use crate::value::Value;
    use std::rc::Rc;

    #[test]
    fn write_op_and_write_u8_append_bytes_and_track_lines() {
        let mut chunk = Chunk::new();
        chunk.write_op(Op::Nil, 1);
        chunk.write_op(Op::Pop, 1);
        chunk.write_op(Op::Return, 2);
        assert_eq!(
            chunk.code,
            vec![Op::Nil.as_u8(), Op::Pop.as_u8(), Op::Return.as_u8()]
        );
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
        let mut interner = ember_ast::Interner::new();
        let proto1 = FunctionProto {
            chunk: Chunk::new(),
            arity: 0,
            upvalues: vec![],
            name: interner.intern("f"),
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
        assert!(
            Rc::ptr_eq(&a, &b),
            "both handles must point at the same allocation"
        );
    }
}
