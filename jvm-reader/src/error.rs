use core::fmt;

#[derive(Debug)]
pub enum ClassFileError {
    InvalidMagic,
    OutOfBounds {
        offset: usize,
        size: usize,
        len: usize,
    },
    /// A CONSTANT_Utf8 entry whose bytes are not well-formed modified UTF-8.
    ///
    /// `cp_index` is the 1-based constant-pool index, filled in once the entry
    /// being parsed is known; `offset` and `byte` locate the first byte that
    /// does not fit the encoding, relative to the entry's `bytes` array.
    MalformedUtf8 {
        cp_index: Option<u16>,
        offset: usize,
        byte: u8,
    },
    /// A well-formed modified-UTF-8 constant that holds unpaired UTF-16
    /// surrogate code units, used where a Rust `str` is required.
    ///
    /// Such a constant is legal in a class file -- generated lexers use string
    /// constants as packed UTF-16 tables -- but it is not a Unicode scalar
    /// sequence, so `str` cannot hold it. Class, method, field and descriptor
    /// names can never legally contain one, so this is a hard error at those
    /// call sites; string *constants* keep their code units and go through
    /// [`crate::types::JvmString::to_string_lossy`] instead.
    UnpairedSurrogate {
        cp_index: Option<u16>,
        /// Index of the offending code unit within the entry.
        index: usize,
        code_unit: u16,
    },
    /// Two predecessors reach a basic block with different operand-stack
    /// heights.
    ///
    /// Structured rather than a formatted message so callers can recognize it
    /// without matching on text (`xtask regression` counts these as skips).
    StackHeightMismatch {
        class_name: String,
        method_name: String,
        method_descriptor: String,
        /// Index and start pc of the block being entered.
        block: usize,
        block_pc: u32,
        /// Index and start pc of the predecessor supplying the new height.
        pred_block: usize,
        pred_pc: u32,
        /// Height already recorded for `block` from an earlier predecessor.
        existing_len: usize,
        /// Height `pred_block` arrives with.
        new_len: usize,
    },
    InvalidClassFile(&'static str),
    InvalidClassFileMessage(String),
    /// A failure attributable to one named entry of a JAR.
    InEntry {
        entry: String,
        source: Box<ClassFileError>,
    },
    Io(std::io::Error),
    InvalidZip(String),
}

impl ClassFileError {
    /// Attach the JAR entry name a failure came from, so a whole-JAR error says
    /// which class it choked on.
    pub fn in_entry(self, entry: impl Into<String>) -> Self {
        ClassFileError::InEntry {
            entry: entry.into(),
            source: Box::new(self),
        }
    }

    /// Record the constant-pool index a UTF-8 decoding failure belongs to.
    ///
    /// The decoder works on the entry's bytes alone and so cannot know it; the
    /// constant-pool parser calls this once the index is in hand.
    pub fn with_cp_index(self, index: u16) -> Self {
        match self {
            ClassFileError::MalformedUtf8 { offset, byte, .. } => ClassFileError::MalformedUtf8 {
                cp_index: Some(index),
                offset,
                byte,
            },
            ClassFileError::UnpairedSurrogate {
                index: unit_index,
                code_unit,
                ..
            } => ClassFileError::UnpairedSurrogate {
                cp_index: Some(index),
                index: unit_index,
                code_unit,
            },
            other => other,
        }
    }
}

fn cp_index_suffix(cp_index: &Option<u16>) -> String {
    match cp_index {
        Some(i) => format!(" (constant pool #{i})"),
        None => String::new(),
    }
}

impl fmt::Display for ClassFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClassFileError::InvalidClassFileMessage(msg) => write!(f, "{}", msg),
            ClassFileError::MalformedUtf8 {
                cp_index,
                offset,
                byte,
            } => write!(
                f,
                "malformed modified UTF-8{}: byte 0x{byte:02x} at offset {offset}",
                cp_index_suffix(cp_index)
            ),
            ClassFileError::UnpairedSurrogate {
                cp_index,
                index,
                code_unit,
            } => write!(
                f,
                "modified UTF-8 constant{} holds an unpaired surrogate \
                 (code unit U+{code_unit:04X} at index {index}), which is not a Unicode \
                 scalar value",
                cp_index_suffix(cp_index)
            ),
            ClassFileError::StackHeightMismatch {
                class_name,
                method_name,
                method_descriptor,
                block,
                block_pc,
                pred_block,
                pred_pc,
                existing_len,
                new_len,
            } => write!(
                f,
                "inconsistent operand stack height at basic-block join: \
                 class={class_name} method={method_name}{method_descriptor} \
                 block {block} (pc {block_pc}) <- block {pred_block} (pc {pred_pc}), \
                 existing_len={existing_len}, new_len={new_len}"
            ),
            ClassFileError::InEntry { entry, source } => write!(f, "in entry {entry}: {source}"),
            _ => write!(f, "{:?}", self),
        }
    }
}

impl std::error::Error for ClassFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClassFileError::InEntry { source, .. } => Some(source.as_ref()),
            ClassFileError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ClassFileError {
    fn from(e: std::io::Error) -> Self {
        ClassFileError::Io(e)
    }
}

pub type ClassFileResult<T> = Result<T, ClassFileError>;

#[cfg(test)]
mod tests {
    use super::ClassFileError;

    /// A whole-JAR failure must say which entry it came from; without it the
    /// only clue is the error kind, across thousands of classes.
    #[test]
    fn entry_context_names_the_class() {
        let err = ClassFileError::MalformedUtf8 {
            cp_index: Some(338),
            offset: 12,
            byte: 0xFF,
        }
        .in_entry("com/android/tools/smali/smali/smaliFlexLexer.class");
        let msg = err.to_string();
        assert!(msg.contains("smaliFlexLexer.class"), "{msg}");
        assert!(msg.contains("#338"), "{msg}");
        assert!(msg.contains("0xff"), "{msg}");
    }

    /// The join error names class, method and both sides of the edge, and is a
    /// structured variant so `xtask` can count it without matching on text.
    #[test]
    fn stack_height_mismatch_names_the_edge() {
        let err = ClassFileError::StackHeightMismatch {
            class_name: "ResFileDecoder".to_string(),
            method_name: "decode".to_string(),
            method_descriptor: "(Ljava/lang/String;)V".to_string(),
            block: 17,
            block_pc: 221,
            pred_block: 14,
            pred_pc: 181,
            existing_len: 0,
            new_len: 2,
        };
        let msg = err.to_string();
        for needle in [
            "ResFileDecoder",
            "decode(Ljava/lang/String;)V",
            "block 17 (pc 221)",
            "block 14 (pc 181)",
            "existing_len=0",
            "new_len=2",
        ] {
            assert!(msg.contains(needle), "{needle:?} missing from {msg:?}");
        }
    }
}
