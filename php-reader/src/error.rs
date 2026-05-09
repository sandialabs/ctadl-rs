use thiserror::Error;

#[derive(Error, Debug)]
pub enum PhpReaderError {
    #[error("Parse error at byte offset {offset}")]
    ParseError { offset: usize },

    #[error("Unsupported syntax: {message}")]
    UnsupportedSyntax { message: String },

    #[error("Lowering failure: {message}")]
    LoweringFailure { message: String },
}
