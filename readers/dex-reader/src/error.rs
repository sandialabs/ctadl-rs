use thiserror::Error;

#[derive(Debug, Error)]
pub enum DexError {
    #[error("out of bounds: at offset {offset}, size {size}, len {len}")]
    OutOfBounds {
        offset: usize,
        size: usize,
        len: usize,
    },
    #[error("invalid leb128")]
    InvalidLeb128,
    /// A `string_data_item` whose bytes are not well-formed modified UTF-8.
    ///
    /// `string_index` is the index into the string table, filled in once the
    /// entry being decoded is known; `offset` and `byte` locate the first byte
    /// that does not fit the encoding, relative to the item's data.
    #[error(
        "malformed modified UTF-8{}: byte 0x{byte:02x} at offset {offset}",
        string_index_suffix(.string_index)
    )]
    MalformedUtf8 {
        string_index: Option<usize>,
        offset: usize,
        byte: u8,
    },
    /// A well-formed modified-UTF-8 string that holds unpaired UTF-16 surrogate
    /// code units, used where a Rust `str` is required.
    ///
    /// Such a string is legal in a DEX file -- generated lexers use string
    /// constants as packed UTF-16 tables -- but it is not a Unicode scalar
    /// sequence, so `str` cannot hold it. Type, method and field names can
    /// never legally contain one, so this is a hard error at those call sites;
    /// string *constants* keep their code units and go through
    /// [`crate::types::DexString::to_string_lossy`] instead.
    #[error(
        "modified UTF-8 string{} holds an unpaired surrogate \
         (code unit U+{code_unit:04X} at index {index}), which is not a Unicode scalar value",
        string_index_suffix(.string_index)
    )]
    UnpairedSurrogate {
        string_index: Option<usize>,
        /// Index of the offending code unit within the string.
        index: usize,
        code_unit: u16,
    },
    #[error("invalid dex: {0}")]
    InvalidDex(&'static str),
}

impl DexError {
    /// Record the string-table index a decoding failure belongs to.
    ///
    /// The decoder works on one item's bytes alone and so cannot know it; the
    /// string table calls this once the index is in hand.
    pub fn with_string_index(self, index: usize) -> Self {
        match self {
            DexError::MalformedUtf8 { offset, byte, .. } => DexError::MalformedUtf8 {
                string_index: Some(index),
                offset,
                byte,
            },
            DexError::UnpairedSurrogate {
                index: unit_index,
                code_unit,
                ..
            } => DexError::UnpairedSurrogate {
                string_index: Some(index),
                index: unit_index,
                code_unit,
            },
            other => other,
        }
    }
}

fn string_index_suffix(string_index: &Option<usize>) -> String {
    match string_index {
        Some(i) => format!(" (string #{i})"),
        None => String::new(),
    }
}

pub type DexResult<T> = Result<T, DexError>;

#[cfg(test)]
mod tests {
    use super::DexError;

    /// A decoding failure must say which string table entry it came from;
    /// without it the only clue is the error kind, across tens of thousands of
    /// strings in a real APK.
    #[test]
    fn string_index_is_attached_to_decoding_errors() {
        let err = DexError::MalformedUtf8 {
            string_index: None,
            offset: 12,
            byte: 0xFF,
        }
        .with_string_index(4211);
        let msg = err.to_string();
        assert!(msg.contains("#4211"), "{msg}");
        assert!(msg.contains("0xff"), "{msg}");
    }

    #[test]
    fn unpaired_surrogate_error_names_the_code_unit() {
        let err = DexError::UnpairedSurrogate {
            string_index: Some(7),
            index: 3,
            code_unit: 0xD801,
        };
        let msg = err.to_string();
        for needle in ["#7", "U+D801", "index 3"] {
            assert!(msg.contains(needle), "{needle:?} missing from {msg:?}");
        }
    }
}
