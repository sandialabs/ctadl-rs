//! Core ID types for the source span system.
//!
//! Table indices start at 1; ID value 0 is reserved for "none" where applicable
//! (e.g. `FileSpanId(0)` = no source info).

/// Identifier for an artifact in the artifact table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactId(pub u32);

/// Identifier for a file in the file table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileId(pub u32);

/// Identifier for a raw span (start, len) in the span table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanId(pub u32);

/// Identifier for a span in the file span table.
/// Use [`NO_SPAN`] when an instruction has no source location.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileSpanId(pub u32);

/// No source info. Table indices start at 1, so 0 means "none".
pub const NO_SPAN: FileSpanId = FileSpanId(0);
