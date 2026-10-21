//! Streaming IL instruction decoder: no allocation, raw operands, branch
//! targets as absolute offsets.
//!
//! Port of the operand-decoding half of `CodeReader.ReadCode`/`ReadOperand`
//! (`Mono.Cecil.Cil/CodeReader.cs:141-221`).

use crate::cil::opcodes::{
    self, Code, FlowControl, OpCode, OpCodeType, OperandType, StackBehaviour,
};
use crate::error::{Error, Result};
use crate::raw::cursor::Cur;
use crate::token::MetadataToken;

/// A raw decoded operand. Branch targets are absolute offsets into the method
/// body (Cecil resolves relative targets the same way before its second pass).
#[derive(Debug, Clone, PartialEq)]
pub enum RawOperand<'a> {
    None,
    I8(i8),
    U8(u8),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    /// Absolute branch target offset.
    Target(u32),
    /// Switch targets, decoded lazily from the operand bytes.
    Switch(SwitchTargets<'a>),
    /// InlineTok / InlineType / InlineMethod / InlineField.
    Token(MetadataToken),
    /// InlineString: a `#US` heap token.
    String(MetadataToken),
    /// ShortInlineVar / InlineVar.
    Var(u16),
    /// ShortInlineArg / InlineArg.
    Arg(u16),
    /// InlineSig: a StandAloneSig token for a `calli` call site.
    CallSite(MetadataToken),
}

/// The undecoded target list of a `switch` instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchTargets<'a> {
    data: &'a [u8],
    /// Offset of the first instruction after the switch (targets are relative
    /// to it).
    base_offset: u32,
}

impl SwitchTargets<'_> {
    pub fn len(&self) -> usize {
        self.data.len() / 4
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Absolute target offsets.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        let base = self.base_offset;
        self.data.chunks_exact(4).map(move |c| {
            (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i64 + base as i64) as u32
        })
    }
}

/// What Cecil reads an undefined opcode inside its opcode tables as: the
/// `default(OpCode)` its `OneByteOpCode`/`TwoBytesOpCode` arrays hold there.
/// Every field is 0, so it is `nop` with an `InlineBrTarget` operand, and
/// decoding carries on after that operand. No real opcode has an `op1` of 0,
/// so [`OpCode::is_undefined`] tells it apart.
pub const UNDEFINED: OpCode = OpCode {
    code: Code::Nop,
    op1: 0,
    op2: 0,
    flow_control: FlowControl::Branch,
    opcode_type: OpCodeType::Annotation,
    operand_type: OperandType::InlineBrTarget,
    stack_pop: StackBehaviour::Pop0,
    stack_push: StackBehaviour::Pop0,
};

impl OpCode {
    /// Whether this is [`UNDEFINED`], read from a byte that names no opcode.
    pub fn is_undefined(self) -> bool {
        self.op1 == 0
    }
}

/// One decoded instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct Ins<'a> {
    pub offset: u32,
    pub opcode: OpCode,
    pub operand: RawOperand<'a>,
    size: u32,
}

impl Ins<'_> {
    /// Encoded length in bytes: the opcode (one or two bytes, also for an
    /// [`UNDEFINED`] one) plus its operand.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Offset of the byte after this instruction.
    pub fn end_offset(&self) -> u32 {
        self.offset + self.size
    }
}

/// Iterator over the instructions of a method body's code bytes.
pub struct InstructionDecoder<'a> {
    cur: Cur<'a>,
}

impl<'a> InstructionDecoder<'a> {
    pub fn new(code: &'a [u8]) -> InstructionDecoder<'a> {
        InstructionDecoder {
            cur: Cur::new(code),
        }
    }

    /// Offset of the next instruction to decode.
    pub fn offset(&self) -> u32 {
        self.cur.pos() as u32
    }

    fn decode_one(&mut self) -> Result<Ins<'a>> {
        let offset = self.cur.pos() as u32;
        let b = self.cur.u8()?;
        // CodeReader.ReadOpCode indexes Cecil's tables directly: a hole reads
        // as UNDEFINED, and a byte past the end throws.
        let opcode = if b != 0xfe {
            opcodes::ONE_BYTE
                .get(b as usize)
                .ok_or(Error::bad(offset as usize, "invalid opcode"))?
                .unwrap_or(UNDEFINED)
        } else {
            let b2 = self.cur.u8()?;
            opcodes::TWO_BYTE
                .get(b2 as usize)
                .ok_or(Error::bad(offset as usize, "invalid two-byte opcode"))?
                .unwrap_or(UNDEFINED)
        };

        let operand = match opcode.operand_type {
            OperandType::InlineNone => RawOperand::None,
            OperandType::InlineSwitch => {
                let length = self.cur.u32()? as usize;
                let data = self.cur.bytes(length.saturating_mul(4))?;
                RawOperand::Switch(SwitchTargets {
                    data,
                    base_offset: self.cur.pos() as u32,
                })
            }
            OperandType::ShortInlineBrTarget => {
                let delta = self.cur.i8()? as i64;
                RawOperand::Target((delta + self.cur.pos() as i64) as u32)
            }
            OperandType::InlineBrTarget => {
                let delta = self.cur.i32()? as i64;
                RawOperand::Target((delta + self.cur.pos() as i64) as u32)
            }
            OperandType::ShortInlineI => {
                // CodeReader: Ldc_I4_S is signed, unaligned.'s operand is not.
                if opcode.code == Code::Ldc_I4_S {
                    RawOperand::I8(self.cur.i8()?)
                } else {
                    RawOperand::U8(self.cur.u8()?)
                }
            }
            OperandType::InlineI => RawOperand::I32(self.cur.i32()?),
            OperandType::InlineI8 => RawOperand::I64(self.cur.i64()?),
            OperandType::ShortInlineR => RawOperand::F32(self.cur.f32()?),
            OperandType::InlineR => RawOperand::F64(self.cur.f64()?),
            OperandType::ShortInlineVar => RawOperand::Var(self.cur.u8()? as u16),
            OperandType::InlineVar => RawOperand::Var(self.cur.u16()?),
            OperandType::ShortInlineArg => RawOperand::Arg(self.cur.u8()? as u16),
            OperandType::InlineArg => RawOperand::Arg(self.cur.u16()?),
            OperandType::InlineSig => RawOperand::CallSite(MetadataToken(self.cur.u32()?)),
            OperandType::InlineString => RawOperand::String(MetadataToken(self.cur.u32()?)),
            OperandType::InlineTok
            | OperandType::InlineType
            | OperandType::InlineMethod
            | OperandType::InlineField => RawOperand::Token(MetadataToken(self.cur.u32()?)),
            OperandType::InlinePhi => {
                return Err(Error::Unsupported("InlinePhi operand"));
            }
        };

        Ok(Ins {
            offset,
            opcode,
            operand,
            size: self.cur.pos() as u32 - offset,
        })
    }
}

impl<'a> Iterator for InstructionDecoder<'a> {
    type Item = Result<Ins<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cur.remaining() == 0 {
            return None;
        }
        Some(self.decode_one())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(code: &[u8]) -> Vec<Result<Ins<'_>>> {
        InstructionDecoder::new(code).collect()
    }

    #[test]
    fn table_holes_read_as_nop_with_a_branch_operand() {
        // 0x24 and FE 08 are holes, each followed by ret: one branches back
        // to itself, the other to the ret.
        for (code, target) in [
            (&[0x24, 0xfb, 0xff, 0xff, 0xff, 0x2a][..], 0),
            (&[0xfe, 0x08, 0, 0, 0, 0, 0x2a], 6),
        ] {
            let ins: Vec<_> = decode(code).into_iter().map(|i| i.unwrap()).collect();
            assert_eq!(ins.len(), 2);
            assert!(ins[0].opcode.is_undefined());
            assert_eq!(ins[0].opcode.name(), "nop");
            assert_eq!(ins[0].operand, RawOperand::Target(target));
            assert_eq!(ins[1].opcode.code, Code::Ret);
            assert!(!ins[1].opcode.is_undefined());
            assert_eq!(ins[0].end_offset(), ins[1].offset);
        }
    }

    #[test]
    fn sizes_cover_opcode_and_operand() {
        // ldc.i4.s 5; ldarg 0x0102 (fe 09); switch (2 targets); ldc.r8; ret
        let code = [
            0x1f, 0x05, //
            0xfe, 0x09, 0x02, 0x01, //
            0x45, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
            0x23, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f, //
            0x2a,
        ];
        let ins: Vec<_> = decode(&code).into_iter().map(|i| i.unwrap()).collect();
        let sizes: Vec<u32> = ins.iter().map(|i| i.size()).collect();
        assert_eq!(sizes, [2, 4, 13, 9, 1]);
        for pair in ins.windows(2) {
            assert_eq!(pair[0].end_offset(), pair[1].offset);
        }
        assert_eq!(ins[4].end_offset(), code.len() as u32);
    }

    #[test]
    fn no_defined_opcode_is_undefined() {
        for op in opcodes::ONE_BYTE.iter().chain(&opcodes::TWO_BYTE).flatten() {
            assert!(!op.is_undefined(), "{}", op.name());
        }
    }

    #[test]
    fn bytes_past_the_tables_are_errors() {
        for code in [&[0xe1][..], &[0xfd], &[0xff], &[0xfe, 0x1f], &[0xfe, 0xff]] {
            assert!(decode(code)[0].is_err(), "{code:x?}");
        }
    }
}
