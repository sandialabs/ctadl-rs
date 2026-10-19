//! Metadata heap accessors: `#Strings`, `#Blob`, `#US`, `#GUID`.
//!
//! Port of `Mono.Cecil.Metadata/{StringHeap,BlobHeap,UserStringHeap,GuidHeap}.cs`,
//! but returning borrowed subslices of the image instead of copies.

use crate::error::{Error, Result};
use crate::raw::cursor::Cur;
use std::borrow::Cow;
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
            b[3],
            b[2],
            b[1],
            b[0],
            b[5],
            b[4],
            b[7],
            b[6],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl Guid {
    /// Builds a GUID from its fields, as .NET's `Guid(int, short, short,
    /// byte[])` constructor does.
    pub fn from_fields(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
        let mut bytes = [0; 16];
        bytes[0..4].copy_from_slice(&a.to_le_bytes());
        bytes[4..6].copy_from_slice(&b.to_le_bytes());
        bytes[6..8].copy_from_slice(&c.to_le_bytes());
        bytes[8..].copy_from_slice(&d);
        Guid(bytes)
    }

    /// Parses `s` as .NET's `new Guid(string)` does, or `None` where it
    /// throws. It accepts the "N", "D", "B", "P" and "X" formats with
    /// surrounding whitespace, and keeps .NET's leniencies: a group of a
    /// "D"-shaped GUID may start with `+` and then `0x`, and an "X" GUID may
    /// contain whitespace anywhere and truncates its 16-bit fields.
    pub fn parse(s: &str) -> Option<Guid> {
        let s = s.trim();
        // Dispatch as Guid.TryParseGuid does: on the first character and
        // whether there is a dash. Each format then checks its own length.
        match s.as_bytes().first()? {
            b'(' => parse_braced(s.as_bytes(), b'(', b')'),
            b'{' if s.contains('-') => parse_braced(s.as_bytes(), b'{', b'}'),
            b'{' => {
                let s: Vec<u8> = s
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    .into_bytes();
                parse_x(&s)
            }
            _ if s.contains('-') => parse_d(s.as_bytes()),
            _ => parse_n(s.as_bytes()),
        }
    }
}

/// .NET `Guid.TryParseHex`: an optional `+`, then an optional `0x`, then hex
/// digits, of which at most `max_digits` may be significant. Like .NET, it
/// accepts nothing at all after the prefixes as zero.
fn guid_hex(mut s: &[u8], max_digits: usize) -> Option<u64> {
    if let [b'+', rest @ ..] = s {
        s = rest;
    }
    if let [b'0', x, rest @ ..] = s
        && x | 0x20 == b'x'
    {
        s = rest;
    }
    let digits = &s[s.iter().take_while(|&&c| c == b'0').count()..];
    if digits.len() > max_digits {
        return None;
    }
    digits.iter().try_fold(0u64, |acc, &c| {
        Some(acc << 4 | (c as char).to_digit(16)? as u64)
    })
}

/// The 32 hex digits of the "N" format; no prefixes.
fn parse_n(s: &[u8]) -> Option<Guid> {
    if s.len() != 32 || !s.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let hex = std::str::from_utf8(s).ok()?;
    let v = u128::from_str_radix(hex, 16).ok()?;
    let d = (v as u64).to_be_bytes();
    Some(Guid::from_fields(
        (v >> 96) as u32,
        (v >> 80) as u16,
        (v >> 64) as u16,
        d,
    ))
}

/// The "D" format, `dddddddd-dddd-dddd-dddd-dddddddddddd`; each group goes
/// through [`guid_hex`], so it may carry `+` and `0x` prefixes.
fn parse_d(s: &[u8]) -> Option<Guid> {
    if s.len() != 36 || [8, 13, 18, 23].iter().any(|&i| s[i] != b'-') {
        return None;
    }
    let a = guid_hex(&s[0..8], 8)? as u32;
    let b = guid_hex(&s[9..13], 8)? as u16;
    let c = guid_hex(&s[14..18], 8)? as u16;
    let d_hi = guid_hex(&s[19..23], 8)? as u16;
    let d_lo = guid_hex(&s[24..36], 16)?;
    let mut d = [0; 8];
    d[0..2].copy_from_slice(&d_hi.to_be_bytes());
    d[2..].copy_from_slice(&d_lo.to_be_bytes()[2..]);
    Some(Guid::from_fields(a, b, c, d))
}

/// The "B" and "P" formats: a "D" GUID between `open` and `close`.
fn parse_braced(s: &[u8], open: u8, close: u8) -> Option<Guid> {
    if s.len() != 38 || s[0] != open || s[37] != close {
        return None;
    }
    parse_d(&s[1..37])
}

/// The "X" format, `{0xa,0xb,0xc,{0xd,0xd,0xd,0xd,0xd,0xd,0xd,0xd}}`, with
/// whitespace already removed. Mirrors `Guid.TryParseExactX`: every number
/// needs its `0x`, and goes through [`guid_hex`] after it (so `0x+1` and
/// `0x0x1` pass); the 16-bit fields are truncated, not range-checked.
fn parse_x(s: &[u8]) -> Option<Guid> {
    let is_hex_prefix = |i: usize| s.len() > i + 1 && s[i] == b'0' && s[i + 1] | 0x20 == b'x';
    // The number after the `0x` at `prefix`, up to `end`; returns it and the
    // position of `end`.
    let number = |prefix: usize, end: u8| -> Option<(u64, usize)> {
        if !is_hex_prefix(prefix) {
            return None;
        }
        let start = prefix + 2;
        let len = s[start..].iter().position(|&c| c == end)?;
        if len == 0 {
            return None;
        }
        Some((guid_hex(&s[start..start + len], 8)?, start + len))
    };
    if s.first() != Some(&b'{') {
        return None;
    }
    let (a, comma) = number(1, b',')?;
    let (b, comma) = number(comma + 1, b',')?;
    let (c, comma) = number(comma + 1, b',')?;
    if s.get(comma + 1) != Some(&b'{') {
        return None;
    }
    let mut d = [0; 8];
    let mut sep = comma + 1;
    for (i, byte) in d.iter_mut().enumerate() {
        let (v, end) = number(sep + 1, if i < 7 { b',' } else { b'}' })?;
        *byte = u8::try_from(v).ok()?;
        sep = end;
    }
    // `sep` is the inner `}`; exactly one more `}` must end the string.
    if s.len() != sep + 2 || s[sep + 1] != b'}' {
        return None;
    }
    Some(Guid::from_fields(a as u32, b as u16, c as u16, d))
}

#[derive(Clone, Copy)]
pub struct StringsHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> StringsHeap<'a> {
    /// Reads the zero-terminated UTF-8 string at `index`. Invalid UTF-8
    /// decodes as Cecil's `Encoding.UTF8.GetString` decodes it, each maximal
    /// invalid subsequence becoming U+FFFD.
    ///
    /// Index 0 and out-of-range indices yield the empty string, as in Cecil's
    /// `StringHeap.Read`.
    pub fn get(&self, index: StringIndex) -> Result<Cow<'a, str>> {
        let idx = index.0 as usize;
        if idx == 0 || idx >= self.data.len() {
            return Ok(Cow::Borrowed(""));
        }
        let rest = &self.data[idx..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Ok(String::from_utf8_lossy(&rest[..end]))
    }
}

#[derive(Clone, Copy)]
pub struct BlobHeap<'a> {
    pub(crate) data: &'a [u8],
}

impl<'a> BlobHeap<'a> {
    /// Returns the blob at `index` as a subslice, as Cecil's `BlobHeap.Read`
    /// does: index 0, an out-of-range index, or a length that runs past the
    /// heap yields the empty blob. Cecil reads raw blobs (keys, hashes, and
    /// `GetBlob` of attributes and security declarations) this way.
    pub fn get(&self, index: BlobIndex) -> Result<&'a [u8]> {
        let (start, len) = self.span(index)?;
        Ok(self.data[start..].get(..len).unwrap_or(&[]))
    }

    /// Returns the blob at `index` for a signature reader. Cecil's
    /// `SignatureReader` reads the whole heap from the blob's start, so a
    /// length that runs past the heap gives the rest of the heap.
    pub fn view(&self, index: BlobIndex) -> Result<&'a [u8]> {
        let (start, len) = self.span(index)?;
        let rest = &self.data[start..];
        Ok(&rest[..len.min(rest.len())])
    }

    /// Returns the blob at `index`, or an error when its length runs past
    /// the heap, where Cecil's readers that slice by the length
    /// (`ReadConstantString`, `ReadUnicodeStringBlob`) throw.
    pub fn get_exact(&self, index: BlobIndex) -> Result<&'a [u8]> {
        let (start, len) = self.span(index)?;
        self.data[start..]
            .get(..len)
            .ok_or(Error::bad(start, "blob extends past the #Blob heap"))
    }

    /// The start of the blob at `index` and its length as written, which may
    /// run past the heap. Index 0 and out-of-range indices give an empty
    /// blob, as in Cecil's `BlobHeap.Read` and `GetView`.
    fn span(&self, index: BlobIndex) -> Result<(usize, usize)> {
        let idx = index.0 as usize;
        if idx == 0 || idx >= self.data.len() {
            return Ok((self.data.len(), 0));
        }
        let mut cur = Cur::at(self.data, idx);
        let len = cur.compressed_u32()? as usize;
        Ok((cur.pos(), len))
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
            f.write_str(
                c.unwrap_or(char::REPLACEMENT_CHARACTER)
                    .encode_utf8(&mut [0; 4]),
            )?;
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
    /// Reads the 1-based GUID at `index`, as Cecil's `GuidHeap.Read` does.
    /// Index 0 yields the zero GUID, and so does an index past the heap by
    /// Cecil's bounds check, which adds `index - 1` where it means
    /// `(index - 1) * 16`. An index that passes that check but not the real
    /// one is an error, where Cecil's copy throws.
    pub fn get(&self, index: GuidIndex) -> Result<Guid> {
        if index.0 == 0 || (index.0 as usize - 1) + 16 > self.data.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_decode_invalid_utf8_as_dotnet_does() {
        // "Caf\u{e9}", then an encoded surrogate.
        let data = b"\0Caf\xc3\xa9\0T\xed\xa0\x80x\0";
        let heap = StringsHeap { data };
        let get = |i| heap.get(StringIndex(i)).unwrap();
        assert!(matches!(get(1), Cow::Borrowed("Caf\u{e9}")));
        // An index into the middle of a sequence starts at its continuation.
        assert_eq!(get(5), "\u{fffd}");
        assert_eq!(get(7), "T\u{fffd}\u{fffd}\u{fffd}x");
    }

    #[test]
    fn strings_out_of_range_read_empty() {
        let heap = StringsHeap { data: b"\0A\0" };
        assert_eq!(heap.get(StringIndex(1)).unwrap(), "A");
        assert_eq!(heap.get(StringIndex(3)).unwrap(), "");
        assert_eq!(heap.get(StringIndex(u32::MAX)).unwrap(), "");
        assert_eq!(StringsHeap { data: b"" }.get(StringIndex(1)).unwrap(), "");
    }

    // The blob at 2 claims 0x7f bytes, where the heap has 2 left.
    #[test]
    fn overlong_blobs() {
        let heap = BlobHeap {
            data: b"\0\x03\x7f\x06\x08",
        };
        assert_eq!(heap.get(BlobIndex(1)).unwrap(), b"\x7f\x06\x08");
        assert_eq!(heap.view(BlobIndex(1)).unwrap(), b"\x7f\x06\x08");
        assert_eq!(heap.get(BlobIndex(2)).unwrap(), b"");
        assert_eq!(heap.view(BlobIndex(2)).unwrap(), b"\x06\x08");
        assert!(heap.get_exact(BlobIndex(2)).is_err());
        assert_eq!(heap.get_exact(BlobIndex(1)).unwrap(), b"\x7f\x06\x08");
        for get in [BlobHeap::get, BlobHeap::view, BlobHeap::get_exact] {
            assert_eq!(get(&heap, BlobIndex(0)).unwrap(), b"");
            assert_eq!(get(&heap, BlobIndex(5)).unwrap(), b"");
        }
        // A length prefix cut off by the heap's end still fails.
        assert!(BlobHeap { data: b"\0\xc0" }.view(BlobIndex(1)).is_err());
    }

    // Cecil's bounds check is `(index - 1) + 16 > length`.
    #[test]
    fn guid_indexes_past_the_heap() {
        let data: Vec<u8> = (1..=32).collect();
        let heap = GuidHeap { data: &data };
        assert_eq!(heap.get(GuidIndex(2)).unwrap().0[0], 17);
        assert_eq!(heap.get(GuidIndex(0)).unwrap(), Guid([0; 16]));
        // Past the heap by Cecil's check: the zero GUID.
        assert_eq!(heap.get(GuidIndex(18)).unwrap(), Guid([0; 16]));
        assert_eq!(heap.get(GuidIndex(u32::MAX)).unwrap(), Guid([0; 16]));
        // Within it by Cecil's check, past it in fact: Cecil's copy throws.
        assert!(heap.get(GuidIndex(3)).is_err());
        assert!(heap.get(GuidIndex(17)).is_err());
        assert_eq!(
            GuidHeap { data: &[] }.get(GuidIndex(1)).unwrap(),
            Guid([0; 16])
        );
    }

    /// `new Guid(s).ToString("d")` on .NET 8, or `None` where it throws.
    const DOTNET: &[(&str, Option<&str>)] = &[
        (
            "11111111-2222-3333-4444-555555555555",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "AABBCCDD-EEFF-0011-2233-445566778899",
            Some("aabbccdd-eeff-0011-2233-445566778899"),
        ),
        (
            "{11111111-2222-3333-4444-555555555555}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "(11111111-2222-3333-4444-555555555555)",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "11111111222233334444555555555555",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "{0x11111111,0x2222,0x3333,{0x44,0x44,0x55,0x55,0x55,0x55,0x55,0x55}}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{ 0x11111111 , 0x2222,0x3333,{0x44,0x44,0x55,0x55,0x55,0x55,0x55,0x55}}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "{0X11111111,0x2222,0x3333,{0x44,0x44,0x55,0x55,0x55,0x55,0x55,0x55}}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "{0x011111111,0x2222,0x3333,{0x44,0x44,0x55,0x55,0x55,0x55,0x55,0x55}}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "{0x,0x2222,0x3333,{0x44,0x44,0x55,0x55,0x55,0x55,0x55,0x55}}",
            None,
        ),
        (
            "  11111111-2222-3333-4444-555555555555  ",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "\t11111111-2222-3333-4444-555555555555\n",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        ("11111111-2222-3333-4444-55555555555", None),
        ("11111111-2222-3333-4444-5555555555555", None),
        ("1111111-2222-3333-4444-555555555555", None),
        (
            "+1111111-2222-3333-4444-555555555555",
            Some("01111111-2222-3333-4444-555555555555"),
        ),
        (
            "0x111111-2222-3333-4444-555555555555",
            Some("00111111-2222-3333-4444-555555555555"),
        ),
        (
            "11111111-+222-3333-4444-555555555555",
            Some("11111111-0222-3333-4444-555555555555"),
        ),
        (
            "11111111-2222-3333-+444-555555555555",
            Some("11111111-2222-3333-0444-555555555555"),
        ),
        ("{11111111-2222-3333-4444-555555555555)", None),
        ("{11111111-2222-3333-4444-555555555555", None),
        ("1111111122223333444455555555555", None),
        ("111111112222333344445555555555555", None),
        ("+1111111222233334444555555555555", None),
        ("0x111111222233334444555555555555", None),
        ("1111111 -2222-3333-4444-555555555555", None),
        ("11111111-2222-3333-4444-55555555555g", None),
        ("garbage", None),
        ("x", None),
        (" ", None),
        ("{}", None),
        (
            "11111111-0x22-3333-4444-555555555555",
            Some("11111111-0022-3333-4444-555555555555"),
        ),
        (
            "11111111-2222-3333-4444-+55555555555",
            Some("11111111-2222-3333-4444-055555555555"),
        ),
        (
            "11111111-2222-3333-4444-0x5555555555",
            Some("11111111-2222-3333-4444-005555555555"),
        ),
        (
            "11111111-2222-3333-0x44-555555555555",
            Some("11111111-2222-3333-0044-555555555555"),
        ),
        ("11111111-2222-3333-44+4-555555555555", None),
        (
            "+0x11111-2222-3333-4444-555555555555",
            Some("00011111-2222-3333-4444-555555555555"),
        ),
        ("0x+11111-2222-3333-4444-555555555555", None),
        (
            "11111111-2222-3333-4444-555555555555\u{a0}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "\u{a0}11111111-2222-3333-4444-555555555555",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        ("{11111111-2222-3333-4444-555555555555}x", None),
        ("{11111111222233334444555555555555}", None),
        ("{+0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}", None),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x1ff}}", None),
        (
            "{0x1,0x22222,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-2222-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}} ",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}x", None),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa}}", None),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb,0xc}}", None),
        (
            "{0x1 1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000011-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x0000000000001,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x0000000000000b}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x0b}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        ("{0x1,0x2,0x3,0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}", None),
        ("(0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb})", None),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}", None),
        ("{0xg,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}", None),
        ("{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x}}", None),
        ("{1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}", None),
        ("11111111-2222-3333-4444-55555555555\0", None),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}\u{a0}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,\u{a0}0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x0}}",
            Some("00000001-0002-0003-0405-060708090a00"),
        ),
        (
            "{0x+1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x+2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x+4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x1,0x2,0x3,{0x0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            Some("00000001-0002-0003-0405-060708090a0b"),
        ),
        (
            "{0x111111111,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            None,
        ),
        (
            "{0x1,0x222222222,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0xb}}",
            None,
        ),
        (
            "{0x1,0x2,0x3,{0x4,0x5,0x6,0x7,0x8,0x9,0xa,0x100000000}}",
            None,
        ),
        ("11111111-0x+2-3333-4444-555555555555", None),
        (
            "11111111-+0x2-3333-4444-555555555555",
            Some("11111111-0002-3333-4444-555555555555"),
        ),
        (
            "11111111-2222-3333-4444-+0x555555555",
            Some("11111111-2222-3333-4444-000555555555"),
        ),
        ("11111111-2222-3333-++44-555555555555", None),
        ("11111111-2222-3333-+-44-555555555555", None),
        ("11111111-2222-3333-4444-0x", None),
        ("11111111-2222-3333-4444-+", None),
        (
            "11111111-2222-3333-4444-+00000000000",
            Some("11111111-2222-3333-4444-000000000000"),
        ),
        ("0x-2222-3333-4444-555555555555", None),
        (
            "11111111-2222-3333-4444-555555555555",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        ("11111111 2222 3333 4444 555555555555", None),
        (
            "{11111111-2222-3333-4444-555555555555}",
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "(+1111111-2222-3333-4444-555555555555)",
            Some("01111111-2222-3333-4444-555555555555"),
        ),
    ];

    #[test]
    fn parse_matches_dotnet() {
        for &(input, expected) in DOTNET {
            let actual = Guid::parse(input).map(|g| g.to_string());
            assert_eq!(actual.as_deref(), expected, "{input:?}");
        }
    }
}
