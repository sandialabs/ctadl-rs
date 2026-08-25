use core::fmt;

/// The method a stack-slot simulation failure happened in.
///
/// Boxed by the variants that carry it: three `String`s are more than half of
/// `ClassFileError`'s size budget, and that type sits in the `Err` of every
/// `Result` the reader returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodContext {
    pub class_name: String,
    pub method_name: String,
    pub method_descriptor: String,
}

impl fmt::Display for MethodContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "class={} method={}{}",
            self.class_name, self.method_name, self.method_descriptor
        )
    }
}

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
        method: Box<MethodContext>,
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
    /// Two predecessors reach a basic block at the same operand-stack height
    /// but with different slot identities.
    ///
    /// Slot ids are positional -- a slot's id is its depth -- so equal heights
    /// normally imply equal layouts and this cannot fire. It is kept as a
    /// consistency check on that invariant, and structured for the same reason
    /// as [`ClassFileError::StackHeightMismatch`]: the layout, the edge and
    /// whether the edge is an exception edge are what identify the cause.
    StackLayoutMismatch {
        method: Box<MethodContext>,
        /// Index and start pc of the block being entered.
        block: usize,
        block_pc: u32,
        /// Index and start pc of the predecessor supplying the new layout.
        pred_block: usize,
        pred_pc: u32,
        /// Whether `pred_block` reaches `block` as an exception handler edge.
        exception_edge: bool,
        /// Layout already recorded for `block` from an earlier predecessor.
        existing_slots: Vec<u32>,
        /// Layout `pred_block` arrives with.
        new_slots: Vec<u32>,
    },
    /// An instruction consumes more operand-stack words than the simulated
    /// frame holds.
    ///
    /// Unlike the underflows reported while rewriting a specific `StackInput`,
    /// this one is raised on the instruction's aggregate stack effect, so it
    /// names no operand -- the opcode and the two depths are what locate it.
    StackUnderflow {
        method: Box<MethodContext>,
        pc: u32,
        opcode: u8,
        mnemonic: &'static str,
        /// Words the instruction's stack effect says it consumes.
        consumed: usize,
        /// Words the frame holds at that point.
        stack_len: usize,
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
                method,
                block,
                block_pc,
                pred_block,
                pred_pc,
                existing_len,
                new_len,
            } => write!(
                f,
                "inconsistent operand stack height at basic-block join: {method} \
                 block {block} (pc {block_pc}) <- block {pred_block} (pc {pred_pc}), \
                 existing_len={existing_len}, new_len={new_len}"
            ),
            ClassFileError::StackLayoutMismatch {
                method,
                block,
                block_pc,
                pred_block,
                pred_pc,
                exception_edge,
                existing_slots,
                new_slots,
            } => write!(
                f,
                "inconsistent operand stack layout at basic-block join: {method} \
                 block {block} (pc {block_pc}) <- block {pred_block} (pc {pred_pc}){}, \
                 existing={existing_slots:?}, new={new_slots:?}",
                if *exception_edge {
                    " [exception edge]"
                } else {
                    ""
                }
            ),
            ClassFileError::StackUnderflow {
                method,
                pc,
                opcode,
                mnemonic,
                consumed,
                stack_len,
            } => write!(
                f,
                "stack underflow in stack-slot simulation: {method} \
                 pc={pc} opcode=0x{opcode:02x} mnem={mnemonic} \
                 consumed={consumed} stack_len={stack_len}"
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
    use super::{ClassFileError, MethodContext};

    fn method(class_name: &str, method_name: &str, method_descriptor: &str) -> Box<MethodContext> {
        Box::new(MethodContext {
            class_name: class_name.to_string(),
            method_name: method_name.to_string(),
            method_descriptor: method_descriptor.to_string(),
        })
    }

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
            method: method("ResFileDecoder", "decode", "(Ljava/lang/String;)V"),
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

    /// The layout join error used to be a bare `&'static str`, so the one shape
    /// it can report -- a block reached from both a handler and a normal edge --
    /// was the one thing it did not say.
    #[test]
    fn stack_layout_mismatch_names_the_edge_and_the_layouts() {
        let err = ClassFileError::StackLayoutMismatch {
            method: method(
                "AbstractFilterExpressionConverter",
                "convert",
                "(Ljava/lang/String;)V",
            ),
            block: 5,
            block_pc: 61,
            pred_block: 3,
            pred_pc: 40,
            exception_edge: true,
            existing_slots: vec![0],
            new_slots: vec![7],
        };
        let msg = err.to_string();
        for needle in [
            "AbstractFilterExpressionConverter",
            "convert(Ljava/lang/String;)V",
            "block 5 (pc 61)",
            "block 3 (pc 40)",
            "[exception edge]",
            "existing=[0]",
            "new=[7]",
        ] {
            assert!(msg.contains(needle), "{needle:?} missing from {msg:?}");
        }
    }

    /// The aggregate underflow error used to be a bare `&'static str` too --
    /// the message the CVE report quotes for Yamcs and could not attribute to
    /// any instruction.
    #[test]
    fn stack_underflow_names_the_instruction() {
        let err = ClassFileError::StackUnderflow {
            method: method("org/yamcs/Processor", "start", "()V"),
            pc: 94,
            opcode: 0xad,
            mnemonic: "lreturn",
            consumed: 2,
            stack_len: 1,
        };
        let msg = err.to_string();
        for needle in [
            "org/yamcs/Processor",
            "start()V",
            "pc=94",
            "opcode=0xad",
            "mnem=lreturn",
            "consumed=2",
            "stack_len=1",
        ] {
            assert!(msg.contains(needle), "{needle:?} missing from {msg:?}");
        }
    }
}
