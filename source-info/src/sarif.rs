//! SARIF-style export: resolve FileSpanId to physical location (line/column or byte offset).
//!
//! When artifact content and line map are available: emit startLine, startColumn, endLine, endColumn.
//! When artifact is missing: emit byteOffset, byteLength and a warning.

use crate::artifact::ArtifactTable;

/// Function that provides artifact content by path (e.g. from a file loader).
pub type ContentProvider = dyn Fn(&str) -> Option<Vec<u8>>;
use crate::error::{ArtifactMissing, HashMismatch};
use crate::file_span_table::FileSpanTable;
use crate::file_table::FileTable;
use crate::ids::FileSpanId;
use crate::line_map::LineMap;
use crate::offset_to_line::offset_to_line_column;
use crate::span::SpanLen;
use crate::span_table::SpanTable;

/// Resolved location for a diagnostic. Either line/column (when artifact content is available)
/// or byte offset/length (fallback when artifact is missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticLocation {
    LineColumn {
        path: String,
        start_line: u32,
        start_column: u32,
        end_line: u32,
        end_column: u32,
    },
    ByteOffset {
        path: String,
        byte_offset: u32,
        byte_length: u32,
        warning: Option<ExportWarning>,
    },
}

/// Warning when exporting (e.g. artifact missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportWarning {
    ArtifactMissing(ArtifactMissing),
    HashMismatch(HashMismatch),
}

/// Resolves a FileSpanId to a diagnostic location.
///
/// - `content_provider`: if provided, called with artifact path to get content for line mapping.
///   If it returns Some(bytes), line/column are computed. If None or artifact missing, byte offset is used and a warning is set.
pub fn resolve_span(
    span_id: FileSpanId,
    artifacts: &ArtifactTable,
    files: &FileTable,
    file_spans: &FileSpanTable,
    span_table: &SpanTable,
    content_provider: Option<&ContentProvider>,
) -> Option<DiagnosticLocation> {
    let file_span = file_spans.get(span_id)?;
    let span = span_table.get(file_span.span)?;
    let file = files.get(file_span.file)?;
    let artifact = artifacts.get(file.artifact)?;
    let path = artifact.canonical_path.clone();

    let content = content_provider.and_then(|f| f(&path));

    let (start_byte, end_byte) = match span.len {
        SpanLen::Empty => (span.start, span.start),
        SpanLen::ByteLen(n) => (span.start, span.start.saturating_add(n)),
        SpanLen::ToLineEnd => {
            if let Some(ref bytes) = content {
                let end = bytes[span.start as usize..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| span.start + p as u32 + 1)
                    .unwrap_or(bytes.len() as u32);
                (span.start, end)
            } else {
                (span.start, span.start)
            }
        }
    };

    if let Some(ref bytes) = content {
        let line_map = LineMap::from_bytes(bytes);
        let start_lc = offset_to_line_column(&line_map, start_byte);
        let end_lc = offset_to_line_column(&line_map, end_byte.saturating_sub(1).max(start_byte));
        return Some(DiagnosticLocation::LineColumn {
            path,
            start_line: start_lc.line,
            start_column: start_lc.column,
            end_line: end_lc.line,
            end_column: end_lc.column,
        });
    }

    Some(DiagnosticLocation::ByteOffset {
        path,
        byte_offset: start_byte,
        byte_length: end_byte.saturating_sub(start_byte),
        warning: Some(ExportWarning::ArtifactMissing(ArtifactMissing {
            path: artifact.canonical_path.clone(),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        ArtifactEncoding, ArtifactMetadata, ArtifactRecord, ArtifactTable, HashAlgorithm,
    };
    use crate::file_span_table::FileSpanTable;
    use crate::file_table::FileTable;
    use crate::ids::{ArtifactId, FileSpanId};
    use crate::span_table::SpanTable;

    fn minimal_tables(
        span_start: u32,
        span_len: SpanLen,
    ) -> (ArtifactTable, FileTable, FileSpanTable, SpanTable) {
        let metadata = ArtifactMetadata {
            hash_algorithm: HashAlgorithm::Sha256,
            hash_len: 32,
            version: 1,
        };
        let (art_tree, file_tree, span_tree, fspan_tree) = crate::db::open_session_trees();
        let mut artifacts = ArtifactTable::new(metadata, art_tree);
        artifacts.artifacts.push(ArtifactRecord {
            canonical_path: "/x.rs".to_string(),
            sub_artifact_id: 0,
            encoding: ArtifactEncoding::Utf8,
            content_hash: vec![0; 32],
        });
        let mut files = FileTable::new(file_tree);
        let fid = files.get_or_intern_file(ArtifactId(1));
        let mut span_table = SpanTable::new(span_tree);
        let sid = span_table.get_or_intern(span_start, span_len);
        let mut file_spans = FileSpanTable::new(fspan_tree);
        let _ = file_spans.get_or_intern(fid, sid);
        (artifacts, files, file_spans, span_table)
    }

    #[test]
    fn resolve_span_with_content() {
        // Content "line1\nline2\n": bytes 0-4 = "line1", 5 = \n, 6-11 = "line2", 12 = \n.
        // Span start=0, len=5 highlights "line1" -> line 1, col 0 to line 1, col 4.
        let (artifacts, files, file_spans, span_table) = minimal_tables(0, SpanLen::ByteLen(5));
        let span_id = FileSpanId(1);
        let content = b"line1\nline2\n".to_vec();
        let provider = move |path: &str| {
            if path == "/x.rs" {
                Some(content.clone())
            } else {
                None
            }
        };
        let loc = resolve_span(
            span_id,
            &artifacts,
            &files,
            &file_spans,
            &span_table,
            Some(&provider),
        )
        .unwrap();
        match &loc {
            DiagnosticLocation::LineColumn {
                path: p,
                start_line,
                start_column,
                end_line,
                end_column,
            } => {
                assert_eq!(p, "/x.rs");
                assert_eq!(*start_line, 1);
                assert_eq!(*start_column, 0);
                assert_eq!(*end_line, 1);
                assert_eq!(*end_column, 4);
            }
            _ => panic!("expected LineColumn"),
        }
    }

    #[test]
    fn resolve_span_without_content() {
        let (artifacts, files, file_spans, span_table) = minimal_tables(10, SpanLen::ByteLen(5));
        let span_id = FileSpanId(1);
        let loc =
            resolve_span(span_id, &artifacts, &files, &file_spans, &span_table, None).unwrap();
        match &loc {
            DiagnosticLocation::ByteOffset {
                path,
                byte_offset,
                byte_length,
                warning,
            } => {
                assert_eq!(path, "/x.rs");
                assert_eq!(*byte_offset, 10);
                assert_eq!(*byte_length, 5);
                assert!(matches!(warning, Some(ExportWarning::ArtifactMissing(_))));
            }
            _ => panic!("expected ByteOffset"),
        }
    }
}
