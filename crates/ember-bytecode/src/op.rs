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
            0 => Constant,
            1 => Nil,
            2 => True,
            3 => False,
            4 => Pop,
            5 => GetLocal,
            6 => SetLocal,
            7 => GetGlobal,
            8 => SetGlobal,
            9 => DefineGlobal,
            10 => GetUpvalue,
            11 => SetUpvalue,
            12 => CloseUpvalue,
            13 => GetField,
            14 => SetField,
            15 => GetIndex,
            16 => SetIndex,
            17 => Equal,
            18 => Greater,
            19 => Less,
            20 => Add,
            21 => Sub,
            22 => Mul,
            23 => Div,
            24 => Mod,
            25 => Not,
            26 => Negate,
            27 => Jump,
            28 => JumpIfFalse,
            29 => JumpIfTrue,
            30 => Loop,
            31 => Call,
            32 => Closure,
            33 => Return,
            34 => MakeList,
            35 => MakeRecord,
            36 => MakeAdt,
            37 => TestVariant,
            38 => Destructure,
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
            Nil | True | False | Pop | CloseUpvalue | Equal | Greater | Less | Add | Sub | Mul
            | Div | Mod | Not | Negate | Return | GetIndex | SetIndex | Print => 0,
            GetLocal | SetLocal | GetUpvalue | SetUpvalue | Call | Destructure => 1,
            Constant | GetGlobal | SetGlobal | DefineGlobal | GetField | SetField | Jump
            | JumpIfFalse | JumpIfTrue | Loop | Closure | MakeList | TestVariant => 2,
            MakeRecord | MakeAdt => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_round_trips_through_as_u8_and_from_u8() {
        let all = [
            Op::Constant,
            Op::Nil,
            Op::True,
            Op::False,
            Op::Pop,
            Op::GetLocal,
            Op::SetLocal,
            Op::GetGlobal,
            Op::SetGlobal,
            Op::DefineGlobal,
            Op::GetUpvalue,
            Op::SetUpvalue,
            Op::CloseUpvalue,
            Op::GetField,
            Op::SetField,
            Op::GetIndex,
            Op::SetIndex,
            Op::Equal,
            Op::Greater,
            Op::Less,
            Op::Add,
            Op::Sub,
            Op::Mul,
            Op::Div,
            Op::Mod,
            Op::Not,
            Op::Negate,
            Op::Jump,
            Op::JumpIfFalse,
            Op::JumpIfTrue,
            Op::Loop,
            Op::Call,
            Op::Closure,
            Op::Return,
            Op::MakeList,
            Op::MakeRecord,
            Op::MakeAdt,
            Op::TestVariant,
            Op::Destructure,
            Op::Print,
        ];
        for op in all {
            assert_eq!(
                Op::from_u8(op.as_u8()),
                Some(op),
                "round-trip failed for {op:?}"
            );
        }
    }

    #[test]
    fn an_invalid_byte_is_not_a_valid_op() {
        assert_eq!(Op::from_u8(255), None);
    }
}
