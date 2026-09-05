//! The error type shared by the store and by every front end.
//!
//! See the crate-level docs for the rule that keeps this type and
//! `ctadl_ascent::error::Error` from colliding: a file imports one [`ErrorContext`] or the
//! other, never both, and the `#[from]` on `ctadl_ascent::Error::Import` bridges them at the `?`.

use std::path::PathBuf;
use thiserror::Error;

/// Errors raised while reading an artifact, or while reading and writing the CTADL store.
///
/// One enum rather than one per language, because the variants are already shared vocabulary:
/// the container formats in `ctadl-frontends` construct [`Error::Dex`] without being the Dex
/// front end, and [`ErrorContext`] is hardwired to *this* type, so every `.err_context(…)` in
/// every front end keeps compiling with no per-language wrapping.
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

    /// The artifact was read fine and simply held no code -- a split APK carrying only
    /// resources, say. Distinct from a decoding error: nothing is malformed, there is
    /// just nothing here to analyze, and the message says where to look instead.
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
    /// No index has been written for the project yet. Distinct from
    /// [`Error::IncompatibleIndex`], which is about an index that exists and cannot be read.
    /// With model files in hand `ctadl query` does not raise this at all: it checks them
    /// against the imports instead (see `ctadl_ascent::cli::query`).
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

/// Inspired by `anyhow`'s `Context`, this trait provides a method to attach context to a CTADL
/// error. Unlike anyhow, it just uses our error types to do so.
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
