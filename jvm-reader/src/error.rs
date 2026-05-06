use core::fmt;

#[derive(Debug)]
pub enum ClassFileError {
    InvalidMagic,
    OutOfBounds {
        offset: usize,
        size: usize,
        len: usize,
    },
    InvalidUtf8,
    InvalidClassFile(&'static str),
    Io(std::io::Error),
    InvalidZip(String),
}

impl fmt::Display for ClassFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for ClassFileError {}

impl From<std::io::Error> for ClassFileError {
    fn from(e: std::io::Error) -> Self {
        ClassFileError::Io(e)
    }
}

pub type ClassFileResult<T> = Result<T, ClassFileError>;
