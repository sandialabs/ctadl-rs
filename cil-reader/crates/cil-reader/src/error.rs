use std::fmt;

/// Errors produced while reading a CIL image.
///
/// Malformed input must never panic; every slice access goes through checked
/// helpers that produce `BadImage` with the offending offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input is not a valid PE/CLI image. `offset` is the position in the
    /// input (or in the sub-stream named by the context) where reading failed.
    BadImage { offset: usize, reason: &'static str },
    /// The image is structurally valid but uses a feature this reader does not
    /// support.
    Unsupported(&'static str),
    /// An index (rid, heap offset, ...) is out of range for its target.
    IndexOutOfRange {
        kind: &'static str,
        index: u32,
        max: u32,
    },
}

impl Error {
    pub(crate) fn bad(offset: usize, reason: &'static str) -> Error {
        Error::BadImage { offset, reason }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadImage { offset, reason } => {
                write!(f, "bad image format at offset {offset:#x}: {reason}")
            }
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::IndexOutOfRange { kind, index, max } => {
                write!(f, "{kind} index {index} out of range (max {max})")
            }
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
