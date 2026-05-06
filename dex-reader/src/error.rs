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
    #[error("invalid utf-8")]
    InvalidUtf8,
    #[error("invalid dex: {0}")]
    InvalidDex(&'static str),
}

pub type DexResult<T> = Result<T, DexError>;
