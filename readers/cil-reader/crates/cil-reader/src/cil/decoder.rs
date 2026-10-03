//! Streaming IL instruction decoder: no allocation, raw operands, branch
//! targets as absolute offsets.
//!
//! Port of the operand-decoding half of `CodeReader.ReadCode`/`ReadOperand`
//! (`Mono.Cecil.Cil/CodeReader.cs:141-221`).

use crate::cil::opcodes::{self, Code, OpCode, OperandType};
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

/// One decoded instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct Ins<'a> {
    pub offset: u32,
    pub opcode: OpCode,
    pub operand: RawOperand<'a>,
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

    fn decode_one(&mut self) -> Result<Ins<'a>> {
        let offset = self.cur.pos() as u32;
        let b = self.cur.u8()?;
        let opcode = if b != 0xfe {
            opcodes::ONE_BYTE
                .get(b as usize)
                .copied()
                .flatten()
                .ok_or(Error::bad(offset as usize, "invalid opcode"))?
        } else {
            let b2 = self.cur.u8()?;
            opcodes::TWO_BYTE
                .get(b2 as usize)
                .copied()
                .flatten()
                .ok_or(Error::bad(offset as usize, "invalid two-byte opcode"))?
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
