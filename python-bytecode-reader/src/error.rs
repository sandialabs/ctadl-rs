//! Errors for parsing the stable bytecode text and for running the serializer.

use thiserror::Error;

use crate::parse::Rule;

/// A failure to parse or extract the stable bytecode text.
///
/// Syntactic failures surface as [`ParseError::Pest`] (which carries line/col);
/// semantic failures (integer overflow, unsupported version, bad structure) as
/// [`ParseError::Extract`], located via `pair.line_col()`. `pest` has no partial
/// recovery, so both are fatal — but neither ever panics.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("bytecode text syntax error")]
    Pest(#[from] Box<pest::error::Error<Rule>>),
    #[error("{line}:{col}: {message}")]
    Extract {
        message: String,
        line: usize,
        col: usize,
    },
}

impl ParseError {
    /// Build a located extraction error from a pest pair's position.
    pub(crate) fn extract(
        pair: &pest::iterators::Pair<'_, Rule>,
        message: impl Into<String>,
    ) -> Self {
        let (line, col) = pair.line_col();
        ParseError::Extract {
            message: message.into(),
            line,
            col,
        }
    }
}

/// A failure to run the embedded Python serializer.
#[derive(Debug, Error)]
pub enum SerializeError {
    #[error("i/o error running the python serializer")]
    Io(#[from] std::io::Error),
    #[error(
        "no python interpreter found: set the PYTHON environment variable or add `python3` to PATH"
    )]
    NoInterpreter,
    #[error("python serializer failed (exit {code}):\n{stderr}")]
    Python { code: i32, stderr: String },
    #[error("python serializer produced non-utf8 output")]
    NonUtf8,
}
