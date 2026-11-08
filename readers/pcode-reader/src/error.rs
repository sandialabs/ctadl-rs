//! Error types for pcode-reader crate

use std::path::PathBuf;
use thiserror::Error;

/// Pcode reader error types
#[derive(Debug, Error)]
pub enum PcodeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("CSV parsing error in {file}: {source}")]
    CsvParse { file: String, source: csv::Error },

    #[error("Missing required fact file: {0}")]
    MissingFactFile(PathBuf),

    #[error("Invalid fact format in {file}: {message}")]
    InvalidFactFormat { file: String, message: String },

    #[error("Fact consistency error: {0}")]
    FactConsistency(String),

    #[error("Unsupported pcode mnemonic: {0}")]
    UnsupportedMnemonic(String),
}

impl PcodeError {
    pub fn csv_parse_error(file: impl Into<String>, source: csv::Error) -> Self {
        Self::CsvParse {
            file: file.into(),
            source,
        }
    }

    pub fn missing_fact_file<P: Into<PathBuf>>(path: P) -> Self {
        Self::MissingFactFile(path.into())
    }

    pub fn invalid_fact_format(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidFactFormat {
            file: file.into(),
            message: message.into(),
        }
    }

    pub fn fact_consistency_error(message: impl Into<String>) -> Self {
        Self::FactConsistency(message.into())
    }

    pub fn unsupported_mnemonic(mnemonic: impl Into<String>) -> Self {
        Self::UnsupportedMnemonic(mnemonic.into())
    }
}
