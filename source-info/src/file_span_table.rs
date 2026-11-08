//! File span table with interning. Each (file_id, span_id) maps to exactly one FileSpanId.

use crate::ids::{FileId, FileSpanId, SpanId};
use crate::index::{Index, Row};
use crate::span::FileSpan;

impl Row for FileSpan {
    type Key<'a> = (FileId, SpanId);

    fn key(&self) -> (FileId, SpanId) {
        (self.file, self.span)
    }

    fn key_eq(a: &(FileId, SpanId), b: &(FileId, SpanId)) -> bool {
        a == b
    }

    #[cfg(feature = "sled")]
    fn key_bytes(&(file, span): &(FileId, SpanId)) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&file.0.to_be_bytes());
        buf.extend_from_slice(&span.0.to_be_bytes());
        buf
    }
}

/// Table of file spans with interning. IDs are 1-based.
#[derive(Debug, Default)]
pub struct FileSpanTable {
    spans: Vec<FileSpan>,
    index: Index<FileSpan>,
}

impl FileSpanTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the FileSpanId for the given (file, span_id), interning if needed.
    pub fn get_or_intern(&mut self, file_id: FileId, span_id: SpanId) -> FileSpanId {
        FileSpanId(
            self.index
                .get_or_insert_with(&mut self.spans, (file_id, span_id), || FileSpan {
                    file: file_id,
                    span: span_id,
                }),
        )
    }

    pub fn get(&self, id: FileSpanId) -> Option<&FileSpan> {
        self.spans.get((id.0 as usize).checked_sub(1)?)
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn spans(&self) -> &[FileSpan] {
        &self.spans
    }

    pub fn finish(self) -> Vec<FileSpan> {
        self.spans
    }

    /// Builds a table from a list of spans (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_spans(spans: Vec<FileSpan>) -> Self {
        let index = Index::from_rows(&spans);
        Self { spans, index }
    }
}
