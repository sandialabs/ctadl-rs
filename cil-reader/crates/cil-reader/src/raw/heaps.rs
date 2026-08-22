//! Metadata heap accessors: `#Strings`, `#Blob`, `#US`, `#GUID`.
//!
//! Port of `Mono.Cecil.Metadata/{StringHeap,BlobHeap,UserStringHeap,GuidHeap}.cs`,
//! but returning borrowed subslices of the image instead of copies.

use crate::error::{Error, Result};
use crate::raw::cursor::Cur;
use std::fmt;

/// Index into the `#Strings` heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StringIndex(pub u32);

/// Index into the `#Blob` heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlobIndex(pub u32);

/// 1-based index into the `#GUID` heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GuidIndex(pub u32);

/// A 16-byte GUID, with `Display` matching .NET's `Guid.ToString()`
/// ("dddddddd-dddd-dddd-dddd-dddddddddddd", lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Guid(pub [u8; 16]);

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        // .NET Guid layout: first three groups are little-endian.
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15]
        )
    }
}

#[derive(Clone, Copy)]
pub struct StringsHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> StringsHeap<'a> {
    /// Reads the zero-terminated UTF-8 string at `index`.
    ///
    /// Cecil (`StringHeap.Read`) returns the empty string for index 0 and for
    /// out-of-range indices; we mirror the former but report the latter.
    pub fn get(&self, index: StringIndex) -> Result<&'a str> {
        let idx = index.0 as usize;
        if idx == 0 {
            return Ok("");
        }
        if idx >= self.data.len() {
            return Err(Error::IndexOutOfRange {
                kind: "#Strings",
                index: index.0,
                max: self.data.len() as u32,
            });
        }
        let rest = &self.data[idx..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        std::str::from_utf8(&rest[..end]).map_err(|_| Error::bad(idx, "invalid UTF-8 in #Strings"))
    }
}

#[derive(Clone, Copy)]
pub struct BlobHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> BlobHeap<'a> {
    /// Returns the blob at `index` as a subslice (Cecil `BlobHeap.GetView`).
    ///
    /// Index 0 (and, like Cecil, out-of-range indices) yields the empty blob.
    pub fn get(&self, index: BlobIndex) -> Result<&'a [u8]> {
        let idx = index.0 as usize;
        if idx == 0 || idx >= self.data.len() {
            return Ok(&[]);
        }
        let mut cur = Cur::at(self.data, idx);
        let len = cur.compressed_u32()? as usize;
        cur.bytes(len)
    }
}

/// A borrowed UTF-16LE string from the `#US` heap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Utf16Str<'a> {
    /// Raw bytes; length is always even.
    pub bytes: &'a [u8],
}

impl<'a> Utf16Str<'a> {
    pub fn units(&self) -> impl ExactSizeIterator<Item = u16> + 'a {
        self.bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
    }

    pub fn to_string_lossy(&self) -> String {
        char::decode_utf16(self.units())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

impl fmt::Display for Utf16Str<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in char::decode_utf16(self.units()) {
            f.write_str(c.unwrap_or(char::REPLACEMENT_CHARACTER).encode_utf8(&mut [0; 4]))?;
        }
        Ok(())
    }
}

impl fmt::Debug for Utf16Str<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_string_lossy())
    }
}

#[derive(Clone, Copy)]
pub struct UserStringsHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> UserStringsHeap<'a> {
    /// Reads the user string at byte offset `index`.
    ///
    /// Mirrors `UserStringHeap.ReadStringAt`: the compressed length includes a
    /// trailing terminal byte flag; the character data is `length & ~1` bytes.
    pub fn get(&self, index: u32) -> Result<Utf16Str<'a>> {
        let idx = index as usize;
        if idx == 0 || idx >= self.data.len() {
            return Ok(Utf16Str { bytes: &[] });
        }
        let mut cur = Cur::at(self.data, idx);
        let len = (cur.compressed_u32()? & !1) as usize;
        Ok(Utf16Str {
            bytes: cur.bytes(len)?,
        })
    }
}

#[derive(Clone, Copy)]
pub struct GuidHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> GuidHeap<'a> {
    /// Reads the 1-based GUID at `index`; index 0 yields the zero GUID
    /// (mirroring `GuidHeap.Read`).
    pub fn get(&self, index: GuidIndex) -> Result<Guid> {
        if index.0 == 0 {
            return Ok(Guid([0; 16]));
        }
        let start = (index.0 as usize - 1) * 16;
        if start + 16 > self.data.len() {
            return Err(Error::IndexOutOfRange {
                kind: "#GUID",
                index: index.0,
                max: (self.data.len() / 16) as u32,
            });
        }
        let mut g = [0u8; 16];
        g.copy_from_slice(&self.data[start..start + 16]);
        Ok(Guid(g))
    }
}
