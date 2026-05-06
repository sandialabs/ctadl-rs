use regex::Error as RegexError;
use thiserror::Error;

/// CTADL error. This is used for interface functions in this crate.
/// Errors that can occur during JSON model parsing
#[derive(Debug)]
pub enum JsonModelError {
    MissingField {
        index: usize,
        field_name: String,
    },
    FieldNotString {
        index: usize,
        field_name: String,
    },
    FieldNotArray {
        index: usize,
        field_name: String,
    },
    InvalidRegex {
        index: usize,
        pattern: String,
        source: RegexError,
    },
    InvalidArgumentFormat {
        index: usize,
        text: String,
    },
    InvalidInteger {
        index: usize,
        source: std::num::ParseIntError,
    },
    UnexpectedConstraint {
        index: usize,
        constraint_type: String,
    },
}

impl std::fmt::Display for JsonModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonModelError::MissingField { index, field_name } => {
                write!(
                    f,
                    "missing required field '{field_name}' in model generator at index {index}"
                )
            }
            JsonModelError::FieldNotString { index, field_name } => {
                write!(
                    f,
                    "field '{field_name}' must be a string in model generator at index {index}"
                )
            }
            JsonModelError::FieldNotArray { index, field_name } => {
                write!(
                    f,
                    "field '{field_name}' must be an array in model generator at index {index}"
                )
            }
            JsonModelError::InvalidRegex {
                index,
                pattern,
                source,
            } => {
                write!(
                    f,
                    "invalid regex pattern '{pattern}' in model generator at index {index}: {source}"
                )
            }
            JsonModelError::InvalidArgumentFormat { index, text } => {
                write!(
                    f,
                    "invalid argument format '{text}' in model generator at index {index}"
                )
            }
            JsonModelError::InvalidInteger { index, source } => {
                write!(
                    f,
                    "invalid integer in argument index in model generator at index {index}: {source}"
                )
            }
            JsonModelError::UnexpectedConstraint {
                index,
                constraint_type,
            } => {
                write!(
                    f,
                    "unexpected constraint type '{constraint_type}' in model generator at index {index}"
                )
            }
        }
    }
}

impl std::error::Error for JsonModelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JsonModelError::InvalidRegex { source, .. } => Some(source),
            JsonModelError::InvalidInteger { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A collection of JSON model parsing errors
#[derive(Debug, Default)]
pub struct JsonModelErrors {
    errors: Vec<JsonModelError>,
}

impl std::error::Error for JsonModelErrors {}

impl std::ops::Deref for JsonModelErrors {
    type Target = Vec<JsonModelError>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.errors
    }
}

impl std::ops::DerefMut for JsonModelErrors {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.errors
    }
}

impl std::fmt::Display for JsonModelErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.errors.len() > 1 {
            writeln!(f, "found {} JSON model parsing errors", self.errors.len())?;
        }
        for err in &self.errors {
            writeln!(f, "> {err}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error")]
    Io(#[from] std::io::Error),
    #[error("parquet error")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow error")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("path error: {message}")]
    Path { message: String },
    #[error("json serialization error")]
    Json(#[from] serde_json::Error),
    #[error("json5 serialization error")]
    Json5(#[from] json5::Error),
    #[error("bitcode error")]
    Bitcode(#[from] bitcode::Error),
    #[error("dex decoding error")]
    Dex(#[from] dex_reader::error::DexError),
    #[error("jvm decoding error")]
    Jvm(#[from] jvm_reader::error::ClassFileError),
    #[error("flowy error")]
    Flowy(#[from] ctadl_flowy::FlowyError),
    #[error("IR verify error")]
    Verify(#[from] ctadl_ir::mir::VerifyErrors),
    #[error("source-info serialization error")]
    SourceInfoParquet(#[from] source_info::parquet_io::ParquetError),
    #[error("datafusion error")]
    DataFusion(#[from] datafusion::error::DataFusionError),
    #[error("error converting facts: {0}")]
    FactsConvert(String),
    #[error("pcode fact reading error: {0}")]
    PcodeFactRead(String),
    #[error("pcode conversion error: {0}")]
    PcodeConversion(String),
    #[error("error loading tree-sitter language")]
    TreeSitterLanguage(tree_sitter::LanguageError),
    #[error("error running tree-sitter query")]
    TreeSitterQuery(tree_sitter::QueryError),
    #[error("tree-sitter parse error: {0}")]
    TreeSitterParse(String),
    #[error("JSON model parsing error")]
    JsonModel(#[from] JsonModelErrors),
    #[error("{context}")]
    Context {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Inspired by `anyhow`'s `Context`, this trait provides a method to attach context to a CTADL
/// error. Unlike anyhow, it just uses our error types to do so.
///
/// ```
/// use ctadl_ascent::error::{Error, ErrorContext};
/// fn example() -> Result<u32, Error> {
///   // Imagine this produced an error
///   let result: Result<u32, Error> = Err(todo!());
///   // Add context in the message
///   result.err_context(|| format!("producing difficult u32"))
/// }
/// ```
pub trait ErrorContext<T> {
    fn err_context<C, F>(self, f: F) -> Result<T, Error>
    where
        C: std::fmt::Display + Send + Sync + 'static,
        F: FnOnce() -> C;
}

impl<T, E> ErrorContext<T> for Result<T, E>
where
    E: Into<Error>,
{
    #[inline]
    fn err_context<C, F>(self, f: F) -> Result<T, Error>
    where
        C: std::fmt::Display + Send + Sync + 'static,
        F: FnOnce() -> C,
    {
        self.map_err(|e| Error::Context {
            context: f().to_string(),
            source: Box::new(e.into()),
        })
    }
}
