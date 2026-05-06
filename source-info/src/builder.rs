//! Source info builder: single entry point to create FileSpanIds from artifact key + start + len.

use std::path::Path;

use crate::artifact::{ArtifactKey, ArtifactMetadata, ArtifactTable};
use crate::file_span_table::FileSpanTable;
use crate::file_table::FileTable;
use crate::ids::{ArtifactId, FileId, FileSpanId};
use crate::parquet_io;
use crate::source_info::SourceInfo;
use crate::span::SpanLen;
use crate::span_table::SpanTable;

/// Builds source info by interning artifacts, files, and spans.
#[derive(Debug)]
pub struct SourceInfoBuilder {
    pub artifacts: ArtifactTable,
    pub files: FileTable,
    pub span_table: SpanTable,
    pub spans: FileSpanTable,
}

impl SourceInfoBuilder {
    pub fn new(metadata: ArtifactMetadata) -> Self {
        let (art_tree, file_tree, span_tree, fspan_tree) = crate::db::open_session_trees();
        let artifacts = ArtifactTable::new(metadata, art_tree);
        let files = FileTable::new(file_tree);
        let span_table = SpanTable::new(span_tree);
        let spans = FileSpanTable::new(fspan_tree);
        Self {
            artifacts,
            files,
            span_table,
            spans,
        }
    }

    pub fn finish(self) -> SourceInfo {
        let SourceInfoBuilder {
            artifacts,
            files,
            span_table,
            spans,
        } = self;
        SourceInfo {
            metadata: artifacts.metadata,
            artifacts: artifacts.artifacts,
            files: files.files,
            spans: span_table.finish(),
            file_spans: spans.finish(),
        }
    }

    /// Serialize current builder state to a directory of Parquet files without calling `finish()`.
    pub fn write_parquet(&self, dir: &Path) -> parquet_io::Result<()> {
        parquet_io::write_parquet(
            dir,
            &self.artifacts.metadata,
            &self.artifacts.artifacts,
            &self.files.files,
            self.span_table.spans(),
            self.spans.spans(),
        )
    }

    /// Returns a FileSpanId for the given artifact key and byte range.
    /// Interns artifact → file → span and returns the span ID.
    pub fn span_for(&mut self, key: ArtifactKey, start: u32, len: SpanLen) -> FileSpanId {
        let artifact_id: ArtifactId = self.artifacts.get_or_intern_artifact(key);
        let file_id: FileId = self.files.get_or_intern_file(artifact_id);
        let span_id = self.span_table.get_or_intern(start, len);
        self.spans.get_or_intern(file_id, span_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactEncoding, ArtifactMetadata, HashAlgorithm};

    fn make_builder() -> SourceInfoBuilder {
        let metadata = ArtifactMetadata {
            hash_algorithm: HashAlgorithm::Sha256,
            hash_len: 32,
            version: 1,
        };
        SourceInfoBuilder::new(metadata)
    }

    #[test]
    fn same_key_same_span_dedup() {
        let mut b = make_builder();
        let key = ArtifactKey {
            path: "/foo.rs".to_string(),
            sub_artifact_id: 0,
            hash: vec![0; 32],
            encoding: ArtifactEncoding::Utf8,
        };
        let id1 = b.span_for(key.clone(), 10, SpanLen::ByteLen(5));
        let id2 = b.span_for(key, 10, SpanLen::ByteLen(5));
        assert_eq!(id1, id2);
    }

    #[test]
    fn same_artifact_different_spans() {
        let mut b = make_builder();
        let key = ArtifactKey {
            path: "/bar.rs".to_string(),
            sub_artifact_id: 0,
            hash: vec![1; 32],
            encoding: ArtifactEncoding::Utf8,
        };
        let id1 = b.span_for(key.clone(), 0, SpanLen::Empty);
        let id2 = b.span_for(key, 20, SpanLen::ByteLen(3));
        assert_ne!(id1, id2);
        let span1 = b.spans.get(id1).unwrap();
        let span2 = b.spans.get(id2).unwrap();
        assert_eq!(span1.file, span2.file);
    }
}
