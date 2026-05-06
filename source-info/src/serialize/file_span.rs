//! Span table and file span table binary serialization.
//!
//! SpanTable: count (ULEB128), then for each span: start_delta (SLEB128 from previous),
//! len_tag (u8). Length tag: 0=Empty, 1=len 1, 2=len 2, 3=len 4, 4=ByteLen(ULEB128), 5=ToLineEnd.
//!
//! FileSpanTable: count (ULEB128), then for each entry: file_id (ULEB128), span_id (ULEB128).

use std::io::{Read, Write};

use crate::file_span_table::FileSpanTable;
use crate::ids::{FileId, SpanId};
use crate::serialize::leb128;
use crate::span::{FileSpan, Span, SpanLen};
use crate::span_table::SpanTable;
use crate::store::{FileSpansStore, SpansStore};

const TAG_EMPTY: u8 = 0;
const TAG_LEN_1: u8 = 1;
const TAG_LEN_2: u8 = 2;
const TAG_LEN_4: u8 = 3;
const TAG_BYTE_LEN: u8 = 4;
const TAG_TO_LINE_END: u8 = 5;

fn span_len_to_tag(len: SpanLen) -> (u8, Option<u32>) {
    match len {
        SpanLen::Empty => (TAG_EMPTY, None),
        SpanLen::ByteLen(1) => (TAG_LEN_1, None),
        SpanLen::ByteLen(2) => (TAG_LEN_2, None),
        SpanLen::ByteLen(4) => (TAG_LEN_4, None),
        SpanLen::ByteLen(n) => (TAG_BYTE_LEN, Some(n)),
        SpanLen::ToLineEnd => (TAG_TO_LINE_END, None),
    }
}

fn tag_to_span_len(tag: u8, r: &mut impl Read) -> std::io::Result<SpanLen> {
    match tag {
        TAG_EMPTY => Ok(SpanLen::Empty),
        TAG_LEN_1 => Ok(SpanLen::ByteLen(1)),
        TAG_LEN_2 => Ok(SpanLen::ByteLen(2)),
        TAG_LEN_4 => Ok(SpanLen::ByteLen(4)),
        TAG_BYTE_LEN => {
            let (n, _) = leb128::read_uleb128(r)?;
            Ok(SpanLen::ByteLen(n))
        }
        TAG_TO_LINE_END => Ok(SpanLen::ToLineEnd),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid span len tag",
        )),
    }
}

pub fn write_span_table(buf: &mut impl Write, table: &SpanTable) -> std::io::Result<()> {
    leb128::write_uleb128(buf, table.len() as u32)?;
    let mut previous_start: i32 = 0;
    for span in table.spans() {
        let delta = span.start as i32 - previous_start;
        leb128::write_sleb128(buf, delta)?;
        previous_start = span.start as i32;
        let (tag, extra) = span_len_to_tag(span.len);
        buf.write_all(&[tag])?;
        if let Some(n) = extra {
            leb128::write_uleb128(buf, n)?;
        }
    }
    Ok(())
}

pub fn read_span_table(r: &mut impl Read, store: SpansStore) -> std::io::Result<SpanTable> {
    let (count, _) = leb128::read_uleb128(r)?;
    let mut spans = Vec::with_capacity(count as usize);
    let mut previous_start: i32 = 0;
    for _ in 0..count {
        let (delta, _) = leb128::read_sleb128(r)?;
        previous_start += delta;
        let start = previous_start as u32;
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)?;
        let len = tag_to_span_len(tag[0], r)?;
        spans.push(Span { start, len });
    }
    Ok(SpanTable::from_spans(spans, store))
}

pub fn write_file_span_table(buf: &mut impl Write, table: &FileSpanTable) -> std::io::Result<()> {
    leb128::write_uleb128(buf, table.len() as u32)?;
    for span in table.spans() {
        leb128::write_uleb128(buf, span.file.0)?;
        leb128::write_uleb128(buf, span.span.0)?;
    }
    Ok(())
}

pub fn read_file_span_table(
    r: &mut impl Read,
    store: FileSpansStore,
) -> std::io::Result<FileSpanTable> {
    let (count, _) = leb128::read_uleb128(r)?;
    let mut spans = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (file_idx, _) = leb128::read_uleb128(r)?;
        let (span_idx, _) = leb128::read_uleb128(r)?;
        spans.push(FileSpan {
            file: FileId(file_idx),
            span: SpanId(span_idx),
        });
    }
    Ok(FileSpanTable::from_spans(spans, store))
}
