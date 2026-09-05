//! The error type that the store and every front end share.
//!
//! There is a second error type, `ctadl_ascent::error::Error`. The crate-level docs explain the
//! rule that keeps the two apart: a file imports one [`ErrorContext`] trait or the other, never
//! both, and the `#[from]` on `ctadl_ascent::Error::Import` converts between the two error types
//! wherever `?` is used.

use std::path::PathBuf;
use thiserror::Error;

/// Errors raised while reading an artifact, or while reading or writing the CTADL store.
///
/// This is one enum for all languages, rather than one per language, for two reasons. The
/// variants are already shared: the container formats in `ctadl-frontends` build an
/// [`Error::Dex`] even though they are not the Dex front end. And [`ErrorContext`] is tied to
/// this one type, so every `.err_context(…)` call in every front end works without wrapping the
/// error in a per-language type first.
#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error")]
    Io(#[from] std::io::Error),
    #[error("path error: {message}")]
    Path { message: String },
    #[error("json serialization error")]
    Json(#[from] serde_json::Error),
    #[error("json5 serialization error")]
    Json5(#[from] json5::Error),
    #[error("bitcode error")]
    Bitcode(#[from] bitcode::Error),
    #[error("IR verify error")]
    Verify(#[from] ctadl_ir::mir::VerifyErrors),
    #[error("source-info serialization error")]
    SourceInfoParquet(#[from] source_info::parquet_io::ParquetError),

    #[cfg(feature = "dex")]
    #[error("dex decoding error")]
    Dex(#[from] dex_reader::error::DexError),
    #[cfg(feature = "jvm")]
    #[error("jvm decoding error")]
    Jvm(#[from] jvm_reader::error::ClassFileError),
    #[cfg(feature = "flowy")]
    #[error("flowy error")]
    Flowy(#[from] ctadl_flowy::FlowyError),
    #[cfg(feature = "pcode")]
    #[error("pcode fact reading error: {0}")]
    PcodeFactRead(String),
    #[cfg(feature = "pcode")]
    #[error("pcode conversion error: {0}")]
    PcodeConversion(String),
    #[cfg(feature = "ts")]
    #[error("error loading tree-sitter language")]
    TreeSitterLanguage(tree_sitter::LanguageError),
    #[cfg(feature = "ts")]
    #[error("error running tree-sitter query")]
    TreeSitterQuery(tree_sitter::QueryError),
    #[cfg(feature = "ts")]
    #[error("tree-sitter parse error: {0}")]
    TreeSitterParse(String),

    /// The artifact was read without trouble and simply contained no code. A split APK that
    /// holds only resources is one example. This is not a decoding error: nothing is
    /// malformed, there is just nothing here to analyze. The message says where to look
    /// instead.
    #[error("nothing to import: {message}")]
    NothingToImport { message: String },
    #[error(
        "import '{name}' was created by an incompatible version of ctadl \
         (import format {found}, this build expects {expected}); the original \
         artifact was imported from '{}'; re-import it", .artifact_path.display()
    )]
    IncompatibleImport {
        name: String,
        found: String,
        expected: String,
        artifact_path: PathBuf,
    },
    /// No index has been written for the project yet. This is different from
    /// [`Error::IncompatibleIndex`], which means an index exists but cannot be read. `ctadl
    /// query` never raises this error when it is given model files, because it checks those
    /// files against the imports instead. See `ctadl_ascent::cli::query`.
    #[error(
        "project '{project}' has no index; run `ctadl index {project}` first. \
         With `--models` given, `ctadl query {project}` reports what those files match in the \
         imported programs without one."
    )]
    MissingIndex { project: String },
    #[error(
        "the index for project '{project}' was created by an incompatible version of ctadl \
         (index format {found}, this build expects {expected}); \
         re-run `ctadl index {project}`"
    )]
    IncompatibleIndex {
        project: String,
        found: String,
        expected: String,
    },
    #[error("{context}")]
    Context {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Adds context to a CTADL error. The idea comes from `anyhow`'s `Context` trait, but this one
/// works with our own error types instead of `anyhow`'s.
///
/// ```
/// use ctadl_import::error::{Error, ErrorContext};
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
