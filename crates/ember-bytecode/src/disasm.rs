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
pub fn disassemble_instruction(
    chunk: &Chunk,
    offset: usize,
    interner: &Interner,
) -> (u32, String, usize) {
    let line = chunk.line_at(offset);
    let op = Op::from_u8(chunk.code[offset]).expect("invalid opcode byte in chunk");
    let read_u16 = |at: usize| u16::from_be_bytes([chunk.code[at], chunk.code[at + 1]]);

    let (text, len) = match op {
        Op::Constant => {
            let idx = read_u16(offset + 1);
            (
                format!("OP_CONSTANT {idx} ({:?})", chunk.constants[idx as usize]),
                3,
            )
        }
        Op::GetGlobal
        | Op::SetGlobal
        | Op::DefineGlobal
        | Op::GetField
        | Op::SetField
        | Op::TestVariant => {
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
            (format!("OP_LOOP -> {target:04}"), 3)
        }
        Op::Closure => {
            let idx = read_u16(offset + 1);
            let proto = &chunk.functions[idx as usize];
            (
                format!(
                    "OP_CLOSURE {idx} <fn {} / arity {}>",
                    interner.resolve(proto.name),
                    proto.arity
                ),
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
            (
                format!("OP_MAKE_ADT {type_name:?}::{variant_name:?} arity={arity}"),
                7,
            )
        }
        other => (op_name(other).to_string(), 1),
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
        assert!(
            out.contains(&format!("{target:04}")),
            "expected the RESOLVED target address {target:04} in output: {out}"
        );
    }

    #[test]
    fn disassemble_chunk_lists_every_instruction_on_its_own_line() {
        let mut chunk = Chunk::new();
        chunk.write_op(Op::Nil, 1);
        chunk.write_op(Op::Pop, 1);
        chunk.write_op(Op::Return, 2);
        let interner = Interner::new();
        let out = disassemble_chunk(&chunk, "test", &interner);
        assert_eq!(
            out.lines()
                .filter(|l| !l.is_empty() && !l.starts_with("=="))
                .count(),
            3
        );
    }
}
