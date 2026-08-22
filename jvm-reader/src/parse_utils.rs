use crate::error::*;

#[inline]
pub fn check_range(data: &[u8], offset: usize, size: usize) -> ClassFileResult<()> {
    match offset.checked_add(size) {
        None => Err(ClassFileError::OutOfBounds {
            offset,
            size,
            len: data.len(),
        }),
        Some(end) if end > data.len() => Err(ClassFileError::OutOfBounds {
            offset,
            size,
            len: data.len(),
        }),
        Some(_) => Ok(()),
    }
}

#[inline]
pub fn read_u8(data: &[u8], offset: usize) -> ClassFileResult<u8> {
    check_range(data, offset, 1)?;
    Ok(data[offset])
}

#[inline]
pub fn read_u16_be(data: &[u8], offset: usize) -> ClassFileResult<u16> {
    check_range(data, offset, 2)?;
    Ok(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

#[inline]
pub fn read_u32_be(data: &[u8], offset: usize) -> ClassFileResult<u32> {
    check_range(data, offset, 4)?;
    Ok(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

#[inline]
pub fn read_i32_be(data: &[u8], offset: usize) -> ClassFileResult<i32> {
    read_u32_be(data, offset).map(|v| v as i32)
}

#[inline]
pub fn read_slice(data: &[u8], offset: usize, size: usize) -> ClassFileResult<&[u8]> {
    check_range(data, offset, size)?;
    Ok(&data[offset..offset + size])
}

#[inline]
pub fn validate_offset(off: u32, data_len: usize) -> ClassFileResult<()> {
    if off == 0 {
        return Ok(());
    }
    if off as usize >= data_len {
        Err(ClassFileError::InvalidClassFile("offset out of range"))
    } else {
        Ok(())
    }
}

/// Decode JVM modified UTF-8 (length-prefixed bytes in Utf8_info) into UTF-16
/// code units.
///
/// Modified UTF-8 (JVMS §4.4.7) is a UTF-16 transport, not a UTF-8 one: every
/// one-, two- or three-byte sequence encodes exactly one UTF-16 code unit, and
/// a supplementary character therefore arrives as a *pair* of three-byte
/// sequences (CESU-8), not as a four-byte UTF-8 sequence. Null is `0xC0 0x80`
/// and no raw `0x00` appears in the stream.
///
/// The code units are returned verbatim -- surrogates included, paired or not.
/// Recombining pairs into scalar values, and deciding what to do about the
/// unpaired ones a class file may legally contain, belongs to
/// [`crate::types::JvmString`].
pub fn decode_modified_utf8_code_units(data: &[u8]) -> ClassFileResult<Vec<u16>> {
    let mut units: Vec<u16> = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];

        if byte & 0x80 == 0 {
            if byte == 0 {
                return Err(ClassFileError::MalformedUtf8 {
                    cp_index: None,
                    offset: i,
                    byte,
                });
            }
            units.push(byte as u16);
            i += 1;
        } else if byte & 0xE0 == 0xC0 {
            if i + 1 >= data.len() {
                return Err(ClassFileError::MalformedUtf8 {
                    cp_index: None,
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
            if i + 2 >= data.len() {
                return Err(ClassFileError::MalformedUtf8 {
                    cp_index: None,
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
            return Err(ClassFileError::MalformedUtf8 {
                cp_index: None,
                offset: i,
                byte,
            });
        }
    }

    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::decode_modified_utf8_code_units;
    use crate::error::ClassFileError;
    use crate::types::JvmString;

    /// Decode raw modified-UTF-8 bytes the way the constant-pool parser does.
    fn decode(bytes: &[u8]) -> JvmString {
        JvmString::from_code_units(
            decode_modified_utf8_code_units(bytes).expect("well-formed modified UTF-8"),
        )
    }

    #[test]
    fn ascii_round_trips() {
        let s = decode(b"HelloWorld");
        assert_eq!(s.as_str(), Some("HelloWorld"));
        assert_eq!(s.len_utf16(), 10);
    }

    /// Modified UTF-8 encodes NUL as 0xC0 0x80 and never as a raw zero byte.
    #[test]
    fn embedded_null_uses_the_two_byte_form() {
        assert_eq!(decode(&[0xC0, 0x80]).as_str(), Some("\0"));
        assert!(matches!(
            decode_modified_utf8_code_units(&[0x00]),
            Err(ClassFileError::MalformedUtf8 {
                offset: 0,
                byte: 0x00,
                ..
            })
        ));
    }

    /// A supplementary character arrives as a CESU-8 pair of three-byte
    /// sequences, not as a four-byte UTF-8 sequence. Recombining them is what
    /// makes an ordinary class with an emoji or a CJK extension character
    /// parse at all.
    #[test]
    fn well_formed_surrogate_pairs_recombine() {
        // U+10000: ED A0 80 ED B0 80
        assert_eq!(decode(&[0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80]).as_str(), Some("\u{10000}"));
        // U+1F600 (grinning face): ED A0 BD ED B8 80
        let emoji = decode(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        assert_eq!(emoji.as_str(), Some("\u{1F600}"));
        // One scalar value, but still two UTF-16 code units, as Java counts it.
        assert_eq!(emoji.len_utf16(), 2);
    }

    /// A four-byte UTF-8 sequence is not modified UTF-8 and must be rejected
    /// rather than silently accepted.
    #[test]
    fn four_byte_utf8_is_not_modified_utf8() {
        assert!(matches!(
            decode_modified_utf8_code_units(&[0xF0, 0x9F, 0x98, 0x80]),
            Err(ClassFileError::MalformedUtf8 {
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
            Err(ClassFileError::UnpairedSurrogate {
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
            Err(ClassFileError::UnpairedSurrogate {
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
        assert_eq!(
            s.to_string_lossy(),
            " \u{10000}\u{FFFD}\u{10FFFF}\u{FFFD}"
        );
        assert!(matches!(
            s.as_str_or_err(),
            Err(ClassFileError::UnpairedSurrogate {
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
            Err(ClassFileError::UnpairedSurrogate {
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
            Err(ClassFileError::MalformedUtf8 { offset: 0, .. })
        ));
        assert!(matches!(
            decode_modified_utf8_code_units(&[0x41, 0xE0, 0x80]),
            Err(ClassFileError::MalformedUtf8 { offset: 1, .. })
        ));
    }

    /// The constant-pool parser attaches the index a failure belongs to, so a
    /// whole-JAR error can say which constant it choked on.
    #[test]
    fn cp_index_is_attached_to_decoding_errors() {
        let err = decode_modified_utf8_code_units(&[0xFF])
            .unwrap_err()
            .with_cp_index(381);
        assert!(matches!(
            err,
            ClassFileError::MalformedUtf8 {
                cp_index: Some(381),
                ..
            }
        ));
        assert!(err.to_string().contains("#381"));
    }
}
