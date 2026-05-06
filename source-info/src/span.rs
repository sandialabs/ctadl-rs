//! Span representation: FileSpan and SpanLen relative to a file.

use crate::ids::{FileId, SpanId};

/// Length of a span.
/// - Empty: caret position (zero-length)
/// - ByteLen(n): span [start, start+n)
/// - ToLineEnd: from start until next newline
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpanLen {
    Empty,
    ByteLen(u32),
    ToLineEnd,
}

/// A raw span: byte offset and length. Stored in `SpanTable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    pub start: u32,
    pub len: SpanLen,
}

/// A span within a file: references a file and a `SpanId` into the `SpanTable`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileSpan {
    pub file: FileId,
    pub span: SpanId,
}
