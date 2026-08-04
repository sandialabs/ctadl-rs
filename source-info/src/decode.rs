//! Decoding source text from bytes that are not required to be valid UTF-8.
//!
//! Source code on disk is not always valid UTF-8, and a few stray bytes should not sink a whole
//! file. `String::from_utf8_lossy` is the obvious remedy, but it is the wrong one here: it
//! substitutes U+FFFD, three bytes, for an invalid sequence of one to three bytes, so every offset
//! past the first bad byte shifts. That matters because spans recorded against the decoded text are
//! later resolved against the raw bytes on disk (see [`crate::line_map`]), and the two would
//! disagree.
//!
//! [`decode_source`] substitutes one ASCII byte per invalid byte instead, so the decoded string
//! has exactly the same length as the input and every byte offset, line number, and column number
//! still names the same place in the file.

/// Stands in for a byte that is not valid UTF-8.
///
/// A space is the safest choice: it is one byte, so offsets hold, and it is a token separator in
/// every language we parse, so it can only ever split a token — never join two that the file kept
/// apart. It is not a line break, so line and column numbers hold too.
const REPLACEMENT: char = ' ';

/// Decodes `bytes` as UTF-8, replacing each invalid byte with a space.
///
/// The result is always exactly `bytes.len()` bytes long, so offsets into it are offsets into
/// `bytes`. Input that is already valid UTF-8 comes back unchanged.
pub fn decode_source(bytes: &[u8]) -> String {
    // The overwhelmingly common case: no substitution needed, one pass to validate.
    if let Ok(s) = str::from_utf8(bytes) {
        return s.to_string();
    }

    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                out.push_str(
                    str::from_utf8(&rest[..good]).expect("prefix validated by valid_up_to"),
                );
                // `error_len` is `None` when the input ends mid-character, in which case every
                // remaining byte is unusable.
                let bad = e.error_len().unwrap_or(rest.len() - good);
                for _ in 0..bad {
                    out.push(REPLACEMENT);
                }
                rest = &rest[good + bad..];
            }
        }
    }
    out
}

/// Reads `path` and decodes it with [`decode_source`].
pub fn read_source(path: &std::path::Path) -> std::io::Result<String> {
    Ok(decode_source(&std::fs::read(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_returned_unchanged() {
        let src = "local M = {}\n-- café ☕\nreturn M\n";
        assert_eq!(decode_source(src.as_bytes()), src);
    }

    #[test]
    fn every_offset_still_names_the_same_byte() {
        // Latin-1 `é` in a comment, then a byte pair no encoding of ours accepts.
        let mut bytes = b"local M = {}\n-- caf".to_vec();
        bytes.extend_from_slice(&[0xe9, b' ', 0xff, 0xfe]);
        bytes.extend_from_slice(b"\nreturn M\n");

        let text = decode_source(&bytes);
        assert_eq!(text.len(), bytes.len());
        assert_eq!(text, "local M = {}\n-- caf    \nreturn M\n");
        // The bytes that were already valid are untouched, at their original offsets.
        for (i, b) in bytes.iter().enumerate() {
            if b.is_ascii() {
                assert_eq!(text.as_bytes()[i], *b, "byte {i} moved");
            }
        }
    }

    #[test]
    fn a_truncated_character_is_replaced_byte_for_byte() {
        // A three-byte sequence cut short by the end of the file.
        let bytes = [b'x', 0xe2, 0x98];
        let text = decode_source(&bytes);
        assert_eq!(text, "x  ");
        assert_eq!(text.len(), bytes.len());
    }

    #[test]
    fn multibyte_characters_keep_their_length() {
        // A valid multi-byte character next to an invalid byte: only the invalid byte changes.
        let mut bytes = "☕".as_bytes().to_vec();
        bytes.push(0xff);
        let text = decode_source(&bytes);
        assert_eq!(text, "☕ ");
        assert_eq!(text.len(), bytes.len());
    }

    #[test]
    fn line_and_column_numbers_are_preserved() {
        let mut bytes = b"one\n".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"two\nthree\n");
        let text = decode_source(&bytes);
        assert_eq!(
            text.lines().count(),
            bytes.split(|b| *b == b'\n').count() - 1
        );
        assert_eq!(text.lines().nth(1), Some(" two"));
    }
}
