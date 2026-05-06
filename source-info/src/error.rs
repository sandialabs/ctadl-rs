//! Error types for the source span system.
//!
//! - Artifact missing / hash mismatch: typically surfaced as warnings (e.g. when loading or in SARIF).
//! - Invalid span / span overflow: validation errors when building or deserializing.

use crate::ids::FileId;

/// Artifact could not be found (e.g. when resolving a diagnostic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactMissing {
    pub path: String,
}

/// Content hash did not match the expected hash for the artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashMismatch {
    pub path: String,
}

/// Span start or end is out of file bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    SpanStartOutOfBounds {
        file: FileId,
        start: u32,
        file_size: u32,
    },
    SpanEndOutOfBounds {
        file: FileId,
        start: u32,
        len: u32,
        file_size: u32,
    },
    SpanOverflow {
        file: FileId,
        start: u32,
        len: u32,
    },
}
