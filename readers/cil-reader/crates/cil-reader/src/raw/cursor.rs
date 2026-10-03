//! Immutable-data cursor over `&[u8]` with checked little-endian reads.
//!
//! Unlike Cecil's single mutable `MetadataReader` cursor (which forces every
//! reentrant read to save/restore `position` under a module lock,
//! `ModuleDefinition.cs:963-1007`), cursors here are cheap per-call values.

use crate::error::{Error, Result};

#[derive(Clone, Copy)]
pub struct Cur<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    pub fn new(data: &'a [u8]) -> Cur<'a> {
        Cur { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Cur<'a> {
        Cur { data, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or(Error::bad(self.pos, "unexpected end of data"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64> {
        let b = self.bytes(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// Reads a column value of width 1, 2 or 4 bytes, widened to `u32`.
    pub fn col(&mut self, size: u8) -> Result<u32> {
        match size {
            1 => Ok(self.u8()? as u32),
            2 => Ok(self.u16()? as u32),
            4 => self.u32(),
            _ => unreachable!("column size {size}"),
        }
    }

    /// ECMA-335 II.23.2 compressed unsigned integer.
    pub fn compressed_u32(&mut self) -> Result<u32> {
        let first = self.u8()?;
        if first & 0x80 == 0 {
            Ok(first as u32)
        } else if first & 0x40 == 0 {
            let b1 = self.u8()?;
            Ok(((first as u32 & !0x80) << 8) | b1 as u32)
        } else {
            let b = self.bytes(3)?;
            Ok(((first as u32 & !0xc0) << 24)
                | ((b[0] as u32) << 16)
                | ((b[1] as u32) << 8)
                | b[2] as u32)
        }
    }

    /// ECMA-335 II.23.2 compressed signed integer.
    ///
    /// Mirrors `ByteBuffer.ReadCompressedInt32` (`Mono.Cecil.PE/ByteBuffer.cs:117`),
    /// including the width-dependent bias for negative values.
    pub fn compressed_i32(&mut self) -> Result<i32> {
        let first = self.data.get(self.pos).copied().ok_or(Error::bad(
            self.pos,
            "unexpected end of data reading compressed int",
        ))?;
        let u = self.compressed_u32()? as i32;
        let v = u >> 1;
        if u & 1 == 0 {
            Ok(v)
        } else {
            Ok(match first & 0xc0 {
                0 | 0x40 => v - 0x40,
                0x80 => v - 0x2000,
                _ => v - 0x1000_0000,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test/Mono.Cecil.Tests/ByteBufferTests.cs:7 (TestLargeIntegerCompressed):
    // -9076 must round-trip; here we check decoding of its known encoding.
    #[test]
    fn compressed_int_sign_case() {
        // Cecil's WriteCompressedInt32 stores ((value + bias) << 1) | 1; for
        // -9076 (< -0x2000) the bias is 0x10000000 and the 4-byte form is used.
        let v: i32 = -9076;
        let u = (((v + 0x1000_0000) as u32) << 1) | 1;
        // 4-byte compressed encoding of u
        let enc = [
            0xc0 | ((u >> 24) as u8),
            (u >> 16) as u8,
            (u >> 8) as u8,
            u as u8,
        ];
        let mut cur = Cur::new(&enc);
        assert_eq!(cur.compressed_i32().unwrap(), -9076);
    }

    #[test]
    fn compressed_uint_forms() {
        let mut c = Cur::new(&[0x03]);
        assert_eq!(c.compressed_u32().unwrap(), 3);
        let mut c = Cur::new(&[0x80, 0x80]);
        assert_eq!(c.compressed_u32().unwrap(), 0x80);
        let mut c = Cur::new(&[0xc0, 0x00, 0x40, 0x00]);
        assert_eq!(c.compressed_u32().unwrap(), 0x4000);
    }

    #[test]
    fn truncated_read_errors() {
        let mut c = Cur::new(&[0x01]);
        assert!(c.u32().is_err());
    }
}
