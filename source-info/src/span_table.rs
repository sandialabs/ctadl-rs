//! Span table with interning. Each (start, len) pair maps to exactly one SpanId.

use crate::ids::SpanId;
use crate::index::{Index, Row};
use crate::span::{Span, SpanLen};

impl Row for Span {
    type Key<'a> = Span;

    fn key(&self) -> Span {
        *self
    }

    fn key_eq(a: &Span, b: &Span) -> bool {
        a == b
    }

    #[cfg(feature = "sled")]
    fn key_bytes(span: &Span) -> Vec<u8> {
        let mut buf = Vec::with_capacity(9);
        buf.extend_from_slice(&span.start.to_be_bytes());
        match span.len {
            SpanLen::Empty => buf.push(0),
            SpanLen::ToLineEnd => buf.push(1),
            SpanLen::ByteLen(n) => {
                buf.push(2);
                buf.extend_from_slice(&n.to_be_bytes());
            }
        }
        buf
    }
}

/// Table of raw spans with interning. IDs are 1-based.
#[derive(Debug, Default)]
pub struct SpanTable {
    spans: Vec<Span>,
    index: Index<Span>,
}

impl SpanTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the SpanId for the given (start, len), interning if needed.
    pub fn get_or_intern(&mut self, start: u32, len: SpanLen) -> SpanId {
        let span = Span { start, len };
        SpanId(
            self.index
                .get_or_insert_with(&mut self.spans, span, || span),
        )
    }

    pub fn get(&self, id: SpanId) -> Option<&Span> {
        self.spans.get((id.0 as usize).checked_sub(1)?)
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn finish(self) -> Vec<Span> {
        self.spans
    }

    /// Builds a table from a list of spans (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_spans(spans: Vec<Span>) -> Self {
        let index = Index::from_rows(&spans);
        Self { spans, index }
    }
}
