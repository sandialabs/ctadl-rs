//! File span table with interning. Each (file_id, span_id) maps to exactly one FileSpanId.

use crate::ids::{FileId, FileSpanId, SpanId};
use crate::span::FileSpan;
use crate::store::{FileSpansStore, KvStore};

fn file_span_key_bytes(file_id: FileId, span_id: SpanId) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&file_id.0.to_be_bytes());
    buf[4..].copy_from_slice(&span_id.0.to_be_bytes());
    buf
}

/// Table of file spans with interning. IDs are 1-based.
#[derive(Debug)]
pub struct FileSpanTable {
    spans: Vec<FileSpan>,
    store: Box<dyn KvStore>,
}

impl FileSpanTable {
    pub fn new(store: FileSpansStore) -> Self {
        Self {
            spans: Vec::new(),
            store: store.0,
        }
    }

    /// Returns the FileSpanId for the given (file, span_id), interning if needed.
    pub fn get_or_intern(&mut self, file_id: FileId, span_id: SpanId) -> FileSpanId {
        let key = file_span_key_bytes(file_id, span_id);
        if let Some(val) = self.store.get(&key) {
            return FileSpanId(u32::from_be_bytes(val));
        }
        let id = FileSpanId(self.spans.len() as u32 + 1);
        self.spans.push(FileSpan {
            file: file_id,
            span: span_id,
        });
        self.store.insert(&key, id.0.to_be_bytes());
        id
    }

    pub fn get(&self, id: FileSpanId) -> Option<&FileSpan> {
        let idx = id.0 as usize;
        if idx == 0 || idx > self.spans.len() {
            return None;
        }
        self.spans.get(idx - 1)
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
    pub fn from_spans(spans: Vec<FileSpan>, store: FileSpansStore) -> Self {
        for (i, span) in spans.iter().enumerate() {
            let id = FileSpanId((i + 1) as u32);
            store.0.insert(
                &file_span_key_bytes(span.file, span.span),
                id.0.to_be_bytes(),
            );
        }
        Self {
            spans,
            store: store.0,
        }
    }
}
