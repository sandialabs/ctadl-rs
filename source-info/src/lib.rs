//! Compact source span system: associate IR instructions with source locations
//! with minimal memory and serialized size.
//!
//! Instructions store only a `FileSpanId`. Source data lives in:
//! Artifacts → Files → FileSpans → Instructions.

#[cfg(all(feature = "sled", feature = "serde"))]
compile_error!("features \"sled\" and \"serde\" are mutually exclusive");

pub mod artifact;
pub mod artifact_cache;
pub mod builder;
pub mod db;
pub mod error;
pub mod file_span_table;
pub mod file_table;
pub mod ids;
pub mod line_map;
pub mod offset_to_line;
pub mod parquet_io;
pub mod sarif;
pub mod serialize;
pub mod source_info;
pub mod span;
pub mod span_table;
pub mod store;
pub mod validation;

pub use artifact::{
    ArtifactEncoding, ArtifactKey, ArtifactMetadata, ArtifactRecord, ArtifactTable, HashAlgorithm,
};
pub use artifact_cache::ArtifactCache;
pub use builder::SourceInfoBuilder;
#[cfg(feature = "sled")]
pub use db::init as init_db;
pub use error::{ArtifactMissing, HashMismatch, ValidationError};
pub use file_span_table::FileSpanTable;
pub use file_table::{FileEntry, FileTable};
pub use ids::{ArtifactId, FileId, FileSpanId, NO_SPAN, SpanId};
pub use line_map::LineMap;
pub use offset_to_line::{LineColumn, offset_to_line_column};
pub use parquet_io::{read_parquet_source_info, write_parquet_source_info};
pub use sarif::{ContentProvider, DiagnosticLocation, ExportWarning, resolve_span};
pub use serialize::{read_tables, write_tables};
pub use source_info::SourceInfo;
pub use span::{FileSpan, Span, SpanLen};
pub use span_table::SpanTable;
pub use validation::validate_span;
