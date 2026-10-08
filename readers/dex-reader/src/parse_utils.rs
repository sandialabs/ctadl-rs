use crate::error::*;
use crate::types::DexString;

#[inline]
pub fn check_range(data: &[u8], offset: usize, size: usize) -> DexResult<()> {
    if offset
        .checked_add(size)
        .map_or(true, |end| end > data.len())
    {
        Err(DexError::OutOfBounds {
            offset,
            size,
            len: data.len(),
        })
    } else {
        Ok(())
    }
}

#[inline]
pub fn read_u8(data: &[u8], offset: usize) -> DexResult<u8> {
    check_range(data, offset, 1)?;
    Ok(data[offset])
}

#[inline]
pub fn read_u16_le(data: &[u8], offset: usize) -> DexResult<u16> {
    check_range(data, offset, 2)?;
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

#[inline]
pub fn read_u32_le(data: &[u8], offset: usize) -> DexResult<u32> {
    check_range(data, offset, 4)?;
    Ok(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

#[inline]
pub fn read_i32_le(data: &[u8], offset: usize) -> DexResult<i32> {
    read_u32_le(data, offset).map(|v| v as i32)
}

#[inline]
pub fn read_slice<'a>(data: &'a [u8], offset: usize, size: usize) -> DexResult<&'a [u8]> {
    check_range(data, offset, size)?;
    Ok(&data[offset..offset + size])
}

pub fn read_uleb128(data: &[u8], offset: usize) -> DexResult<(u32, usize)> {
    let mut result = 0u32;
    let mut shift = 0;
    let mut pos = offset;

    for _ in 0..5 {
        let byte = *data.get(pos).ok_or(DexError::InvalidLeb128)?;
        pos += 1;

        result |= ((byte & 0x7f) as u32) << shift;

        if byte & 0x80 == 0 {
            return Ok((result, pos));
        }

        shift += 7;
    }

    Err(DexError::InvalidLeb128)
}

pub fn read_sleb128(data: &[u8], offset: usize) -> DexResult<(i32, usize)> {
    let mut result = 0i32;
    let mut shift = 0;
    let mut pos = offset;
    let mut byte;

    for _ in 0..5 {
        byte = *data.get(pos).ok_or(DexError::InvalidLeb128)?;
        pos += 1;

        result |= ((byte & 0x7f) as i32) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            if shift < 32 && (byte & 0x40) != 0 {
                result |= !0 << shift;
            }
            return Ok((result, pos));
        }
    }

    Err(DexError::InvalidLeb128)
}

#[inline]
pub fn validate_offset(off: u32, data_len: usize) -> DexResult<()> {
    if off == 0 {
        return Ok(());
    }
    if off as usize >= data_len {
        Err(DexError::InvalidDex("offset out of range"))
    } else {
        Ok(())
    }
}

/// Decode DEX modified UTF-8 (the bytes of a `string_data_item`) into UTF-16
/// code units.
///
/// Modified UTF-8 is a **UTF-16** transport, not a UTF-8 one: every one-, two-
/// or three-byte sequence encodes exactly one UTF-16 code unit, and a
/// supplementary character therefore arrives as a *pair* of three-byte
/// sequences (CESU-8), not as a four-byte UTF-8 sequence. Null is `0xC0 0x80`
/// and no raw `0x00` appears in the stream, which is what lets the item be
/// NUL-terminated.
///
/// The code units are returned verbatim -- surrogates included, paired or not.
/// Recombining pairs into scalar values, and deciding what to do about the
/// unpaired ones a DEX file may legally contain, belongs to
/// [`crate::types::DexString`].
pub fn decode_modified_utf8_code_units(data: &[u8]) -> DexResult<Vec<u16>> {
    let mut units: Vec<u16> = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];

        if byte & 0x80 == 0 {
            // 1-byte ASCII. Modified UTF-8 encodes NUL as 0xC0 0x80, so a raw
            // zero byte here is a terminator that leaked into the payload.
            if byte == 0 {
                return Err(DexError::MalformedUtf8 {
                    string_index: None,
                    offset: i,
                    byte,
                });
            }
            units.push(byte as u16);
            i += 1;
        } else if byte & 0xE0 == 0xC0 {
            // 2-byte sequence
            if i + 1 >= data.len() {
                return Err(DexError::MalformedUtf8 {
                    string_index: None,
                    offset: i,
                    byte,
                });
            }
            let b1 = data[i];
            let b2 = data[i + 1];
            if b1 == 0xC0 && b2 == 0x80 {
                units.push(0);
            } else {
                let c = (((b1 & 0x1F) as u16) << 6) | ((b2 & 0x3F) as u16);
                units.push(c);
            }
            i += 2;
        } else if byte & 0xF0 == 0xE0 {
            // 3-byte sequence: one UTF-16 code unit, which may be a surrogate.
            if i + 2 >= data.len() {
                return Err(DexError::MalformedUtf8 {
                    string_index: None,
                    offset: i,
                    byte,
                });
            }
            let b1 = data[i];
            let b2 = data[i + 1];
            let b3 = data[i + 2];
            let c =
                (((b1 & 0x0F) as u16) << 12) | (((b2 & 0x3F) as u16) << 6) | ((b3 & 0x3F) as u16);
            units.push(c);
            i += 3;
        } else {
            return Err(DexError::MalformedUtf8 {
                string_index: None,
                offset: i,
                byte,
            });
        }
    }

    Ok(units)
}

/// Read one `string_data_item` at `offset`: a ULEB128 count of UTF-16 code
/// units, then NUL-terminated modified UTF-8. Returns the decoded string and
/// the offset just past the terminator.
///
/// The ULEB128 count is not used to bound the scan -- the terminator does that,
/// and the two agree in every well-formed file -- but it is returned so callers
/// that want to cross-check have it.
pub fn read_string_data_item(data: &[u8], offset: usize) -> DexResult<(DexString, u32, usize)> {
    let (utf16_len, start) = read_uleb128(data, offset)?;

    let mut end = start;
    while *data.get(end).ok_or(DexError::OutOfBounds {
        offset: end,
        size: 1,
        len: data.len(),
    })? != 0
    {
        end += 1;
    }

    let units = decode_modified_utf8_code_units(&data[start..end])?;
    Ok((DexString::from_code_units(units), utf16_len, end + 1))
}

#[cfg(test)]
mod tests {
    use super::{decode_modified_utf8_code_units, read_string_data_item};
    use crate::error::DexError;
    use crate::types::DexString;

    /// Decode raw modified-UTF-8 bytes the way the string table does.
    fn decode(bytes: &[u8]) -> DexString {
        DexString::from_code_units(
            decode_modified_utf8_code_units(bytes).expect("well-formed modified UTF-8"),
        )
    }

    /// A `string_data_item`: ULEB128 UTF-16 length, payload, NUL terminator.
    fn string_data_item(utf16_len: u8, payload: &[u8]) -> Vec<u8> {
        let mut item = vec![utf16_len];
        item.extend_from_slice(payload);
        item.push(0);
        item
    }

    #[test]
    fn ascii_round_trips() {
        let s = decode(b"HelloWorld");
        assert_eq!(s.as_str(), Some("HelloWorld"));
        assert_eq!(s.len_utf16(), 10);
    }

    /// Modified UTF-8 encodes NUL as 0xC0 0x80 and never as a raw zero byte --
    /// which is exactly what lets a `string_data_item` be NUL-terminated.
    #[test]
    fn embedded_null_uses_the_two_byte_form() {
        assert_eq!(decode(&[0xC0, 0x80]).as_str(), Some("\0"));
        assert!(matches!(
            decode_modified_utf8_code_units(&[0x00]),
            Err(DexError::MalformedUtf8 {
                offset: 0,
                byte: 0x00,
                ..
            })
        ));
    }

    /// A supplementary character arrives as a CESU-8 pair of three-byte
    /// sequences, not as a four-byte UTF-8 sequence. Recombining them is what
    /// makes an ordinary DEX with an emoji or a CJK extension character in a
    /// literal parse at all.
    #[test]
    fn well_formed_surrogate_pairs_recombine() {
        // U+10000: ED A0 80 ED B0 80
        assert_eq!(
            decode(&[0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80]).as_str(),
            Some("\u{10000}")
        );
        // U+1F600 (grinning face): ED A0 BD ED B8 80
        let emoji = decode(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        assert_eq!(emoji.as_str(), Some("\u{1F600}"));
        // One scalar value, but still two UTF-16 code units, as the item's
        // ULEB128 prefix counts them.
        assert_eq!(emoji.len_utf16(), 2);
    }

    /// A four-byte UTF-8 sequence is not modified UTF-8 and must be rejected
    /// rather than silently accepted.
    #[test]
    fn four_byte_utf8_is_not_modified_utf8() {
        assert!(matches!(
            decode_modified_utf8_code_units(&[0xF0, 0x9F, 0x98, 0x80]),
            Err(DexError::MalformedUtf8 {
                offset: 0,
                byte: 0xF0,
                ..
            })
        ));
    }

    #[test]
    fn unpaired_high_surrogate_is_kept_as_a_code_unit() {
        let s = decode(&[0xED, 0xA0, 0x80]); // U+D800
        assert_eq!(s.as_str(), None);
        assert_eq!(s.code_units().collect::<Vec<u16>>(), vec![0xD800]);
        assert_eq!(s.to_string_lossy(), "\u{FFFD}");
        assert!(matches!(
            s.as_str_or_err(),
            Err(DexError::UnpairedSurrogate {
                index: 0,
                code_unit: 0xD800,
                ..
            })
        ));
    }

    #[test]
    fn unpaired_low_surrogate_is_kept_as_a_code_unit() {
        let s = decode(&[0xED, 0xB0, 0x80]); // U+DC00
        assert_eq!(s.as_str(), None);
        assert_eq!(s.code_units().collect::<Vec<u16>>(), vec![0xDC00]);
        assert!(matches!(
            s.as_str_or_err(),
            Err(DexError::UnpairedSurrogate {
                index: 0,
                code_unit: 0xDC00,
                ..
            })
        ));
    }

    /// The shape a generated lexer table has, and the one `SurrogateConstants`
    /// compiles to: a run of code units mixing well-formed pairs with
    /// deliberately unpaired surrogates. Every unit must survive exactly.
    #[test]
    fn packed_table_keeps_every_code_unit() {
        let bytes = [
            0x20, // ' '
            0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80, // U+10000, a well-formed pair
            0xED, 0xA0, 0x81, // U+D801, high, and the next unit is not a low
            0xED, 0xAF, 0xBF, 0xED, 0xBF, 0xBF, // U+10FFFF, another pair
            0xED, 0xB0, 0x82, // U+DC02, a low with no high before it
        ];
        let s = decode(&bytes);
        assert_eq!(s.as_str(), None);
        assert_eq!(
            s.code_units().collect::<Vec<u16>>(),
            vec![0x0020, 0xD800, 0xDC00, 0xD801, 0xDBFF, 0xDFFF, 0xDC02]
        );
        assert_eq!(s.len_utf16(), 7);
        // Both unpaired units become U+FFFD; the two pairs do not.
        assert_eq!(s.to_string_lossy(), " \u{10000}\u{FFFD}\u{10FFFF}\u{FFFD}");
        assert!(matches!(
            s.as_str_or_err(),
            Err(DexError::UnpairedSurrogate {
                index: 3,
                code_unit: 0xD801,
                ..
            })
        ));
    }

    /// A high surrogate followed by something that is not a low surrogate is
    /// unpaired, and the reported index is that of the high surrogate.
    #[test]
    fn first_unpaired_surrogate_is_reported() {
        let bytes = [
            0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80, // U+10000, fine
            0xED, 0xA0, 0x81, // U+D801, high
            0x41, // 'A' -- so the high surrogate above is unpaired
        ];
        let s = decode(&bytes);
        assert_eq!(
            s.code_units().collect::<Vec<u16>>(),
            vec![0xD800, 0xDC00, 0xD801, 0x0041]
        );
        assert!(matches!(
            s.as_str_or_err(),
            Err(DexError::UnpairedSurrogate {
                index: 2,
                code_unit: 0xD801,
                ..
            })
        ));
    }

    #[test]
    fn truncated_sequences_are_rejected() {
        assert!(matches!(
            decode_modified_utf8_code_units(&[0xC2]),
            Err(DexError::MalformedUtf8 { offset: 0, .. })
        ));
        assert!(matches!(
            decode_modified_utf8_code_units(&[0x41, 0xE0, 0x80]),
            Err(DexError::MalformedUtf8 { offset: 1, .. })
        ));
    }

    /// `read_string_data_item` stops at the terminator, reports the ULEB128
    /// length verbatim, and hands back the offset of the next item.
    #[test]
    fn string_data_item_is_bounded_by_its_terminator() {
        // Two items back to back; the second must not bleed into the first.
        let mut data = string_data_item(2, &[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        let second_at = data.len();
        data.extend(string_data_item(3, b"abc"));

        let (first, first_len, next) = read_string_data_item(&data, 0).expect("first item");
        assert_eq!(first.as_str(), Some("\u{1F600}"));
        assert_eq!(first_len, 2);
        assert_eq!(next, second_at);

        let (second, second_len, end) = read_string_data_item(&data, next).expect("second item");
        assert_eq!(second.as_str(), Some("abc"));
        assert_eq!(second_len, 3);
        assert_eq!(end, data.len());
    }

    /// An item with no terminator runs off the end rather than reading past it.
    #[test]
    fn unterminated_string_data_item_is_out_of_bounds() {
        assert!(matches!(
            read_string_data_item(&[0x03, b'a', b'b', b'c'], 0),
            Err(DexError::OutOfBounds { .. })
        ));
    }

    /// The string table attaches the index a failure belongs to, so an error
    /// across a real APK's tens of thousands of strings says which one.
    #[test]
    fn string_index_is_attached_to_decoding_errors() {
        let err = decode_modified_utf8_code_units(&[0xFF])
            .unwrap_err()
            .with_string_index(4211);
        assert!(matches!(
            err,
            DexError::MalformedUtf8 {
                string_index: Some(4211),
                ..
            }
        ));
        assert!(err.to_string().contains("#4211"));
    }
}
