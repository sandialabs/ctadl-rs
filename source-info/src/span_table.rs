//! Span table with interning. Each (start, len) pair maps to exactly one SpanId.

use crate::ids::SpanId;
use crate::span::{Span, SpanLen};
use crate::store::{KvStore, SpansStore};

fn span_key_bytes(start: u32, len: SpanLen) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9);
    buf.extend_from_slice(&start.to_be_bytes());
    match len {
        SpanLen::Empty => buf.push(0),
        SpanLen::ToLineEnd => buf.push(1),
        SpanLen::ByteLen(n) => {
            buf.push(2);
            buf.extend_from_slice(&n.to_be_bytes());
        }
    }
    buf
}

/// Table of raw spans with interning. IDs are 1-based.
#[derive(Debug)]
pub struct SpanTable {
    spans: Vec<Span>,
    store: Box<dyn KvStore>,
}

impl SpanTable {
    pub fn new(store: SpansStore) -> Self {
        Self {
            spans: Vec::new(),
            store: store.0,
        }
    }

    /// Returns the SpanId for the given (start, len), interning if needed.
    pub fn get_or_intern(&mut self, start: u32, len: SpanLen) -> SpanId {
        let key = span_key_bytes(start, len);
        if let Some(val) = self.store.get(&key) {
            return SpanId(u32::from_be_bytes(val));
        }
        let id = SpanId(self.spans.len() as u32 + 1);
        self.spans.push(Span { start, len });
        self.store.insert(&key, id.0.to_be_bytes());
        id
    }

    pub fn get(&self, id: SpanId) -> Option<&Span> {
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

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn finish(self) -> Vec<Span> {
        self.spans
    }

    /// Builds a table from a list of spans (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_spans(spans: Vec<Span>, store: SpansStore) -> Self {
        for (i, span) in spans.iter().enumerate() {
            let id = SpanId((i + 1) as u32);
            store
                .0
                .insert(&span_key_bytes(span.start, span.len), id.0.to_be_bytes());
        }
        Self {
            spans,
            store: store.0,
        }
    }
}
