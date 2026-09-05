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
    /// A port's trailing access path is not a path in the canonical grammar -- e.g.
    /// `Argument(0).[*]`, or a field name beginning with `[` that was not written `\[`.
    InvalidAccessPath {
        index: usize,
        text: String,
        source: ctadl_ir::mir::PathSyntaxError,
    },
    InvalidInteger {
        index: usize,
        source: std::num::ParseIntError,
    },
    UnexpectedConstraint {
        index: usize,
        constraint_type: String,
    },
    UnexpectedField {
        index: usize,
        field_name: String,
        message: String,
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
            JsonModelError::InvalidAccessPath {
                index,
                text,
                source,
            } => {
                write!(
                    f,
                    "invalid access path '{text}' ({source}) in model generator at index {index}"
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
            JsonModelError::UnexpectedField {
                index,
                field_name,
                message,
            } => {
                write!(
                    f,
                    "unexpected field '{field_name}' in model generator at index {index}: {message}"
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

/// CTADL's engine-side error.
///
/// The import-side half of what used to be one enum now lives in [`ctadl_import::Error`] --
/// everything a front end or the store can raise -- and arrives here through
/// [`Error::Import`]. The split is what lets a consumer read an import without building
/// datafusion, ascent and tree-sitter; see `ctadl_import`'s module docs for the rule that keeps
/// the two [`ErrorContext`] traits from colliding.
#[derive(Debug, Error)]
pub enum Error {
    /// Anything raised while reading an artifact or the store. `transparent` so the chain a
    /// caller renders is the same one it saw before the split.
    #[error(transparent)]
    Import(#[from] ctadl_import::Error),
    #[error("parquet error")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow error")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("flowy error")]
    Flowy(#[from] ctadl_flowy::FlowyError),
    #[error("datafusion error")]
    DataFusion(#[from] datafusion::error::DataFusionError),
    #[error("error converting facts: {0}")]
    FactsConvert(String),
    #[error("JSON model parsing error")]
    JsonModel(#[from] JsonModelErrors),
    /// A model was well-formed but could not be applied: a bridge side that matched nothing
    /// under `on-unmatched: error`, or an ambiguous pairing under `on-ambiguous: error`.
    /// Distinct from [`Error::JsonModel`], which is about the file's *syntax*; this one needs a
    /// program to detect.
    #[error("model error: {message}")]
    Model { message: String },
    #[error("{context}")]
    Context {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Routes a leaf error type through [`Error::Import`].
///
/// These are the types `ctadl_import::Error` takes a `#[from]` on. Without the bridge, every
/// `?` and every `.err_context(...)` in the engine half that starts from one of them would have
/// to name `ctadl_import::Error` explicitly -- which is churn that says nothing, since there is
/// exactly one way for such an error to become an [`Error`].
macro_rules! from_import {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for Error {
            #[inline]
            fn from(e: $t) -> Self {
                Error::Import(ctadl_import::Error::from(e))
            }
        })*
    };
}

from_import!(
    std::io::Error,
    serde_json::Error,
    json5::Error,
    bitcode::Error,
    ctadl_ir::mir::VerifyErrors,
    source_info::parquet_io::ParquetError,
    dex_reader::error::DexError,
    jvm_reader::error::ClassFileError,
);

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
