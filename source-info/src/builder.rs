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
///
/// Every table numbers its rows from 1 in the order they are first interned, so the same
/// sequence of calls always builds the same tables.
#[derive(Debug)]
pub struct SourceInfoBuilder {
    pub artifacts: ArtifactTable,
    pub files: FileTable,
    pub span_table: SpanTable,
    pub spans: FileSpanTable,
    /// The artifact key most recently looked up, and its file. A frontend usually asks for many
    /// spans in one artifact in a row, and comparing against this key is much cheaper than
    /// looking the key up.
    last_file: Option<(ArtifactKey, FileId)>,
}

impl SourceInfoBuilder {
    pub fn new(metadata: ArtifactMetadata) -> Self {
        Self {
            artifacts: ArtifactTable::new(metadata),
            files: FileTable::new(),
            span_table: SpanTable::new(),
            spans: FileSpanTable::new(),
            last_file: None,
        }
    }

    pub fn finish(self) -> SourceInfo {
        let SourceInfoBuilder {
            artifacts,
            files,
            span_table,
            spans,
            last_file: _,
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
    ///
    /// The same as [`Self::file_for`] followed by [`Self::span_in_file`].
    pub fn span_for(&mut self, key: ArtifactKey, start: u32, len: SpanLen) -> FileSpanId {
        let file = match &self.last_file {
            Some((last, file)) if *last == key => *file,
            _ => {
                let file = self.intern_file(&key);
                self.last_file = Some((key, file));
                file
            }
        };
        self.span_in_file(file, start, len)
    }

    /// Returns the file for the artifact `key` names, interning the artifact and the file if
    /// they are new. `key` is copied only when the artifact is new or differs from the last one
    /// looked up.
    ///
    /// Pass the result to [`Self::span_in_file`] to intern spans in that artifact without
    /// looking the key up again. Interning the artifact here gives it the next artifact ID, just
    /// as the first [`Self::span_for`] with `key` would; calling this for an artifact before or
    /// instead of that first `span_for` numbers the artifacts and files differently.
    pub fn file_for(&mut self, key: &ArtifactKey) -> FileId {
        if let Some((last, file)) = &self.last_file
            && last == key
        {
            return *file;
        }
        let file = self.intern_file(key);
        self.last_file = Some((key.clone(), file));
        file
    }

    /// Returns a FileSpanId for the byte range in `file`, interning the span if it is new.
    ///
    /// `file` must come from [`Self::file_for`] on this builder.
    pub fn span_in_file(&mut self, file: FileId, start: u32, len: SpanLen) -> FileSpanId {
        debug_assert!(
            self.files.get(file).is_some(),
            "{file:?} is not a file of this builder"
        );
        let span_id = self.span_table.get_or_intern(start, len);
        self.spans.get_or_intern(file, span_id)
    }

    fn intern_file(&mut self, key: &ArtifactKey) -> FileId {
        let artifact_id: ArtifactId = self.artifacts.get_or_intern_artifact_ref(key);
        self.files.get_or_intern_file(artifact_id)
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

    fn key(n: u32) -> ArtifactKey {
        ArtifactKey {
            path: format!("/f{}.rs", n % 3),
            sub_artifact_id: n / 3,
            hash: vec![n as u8; 32],
            encoding: ArtifactEncoding::Utf8,
        }
    }

    /// A pseudo-random sequence of `(artifact, start, len)` requests that revisits artifacts and
    /// spans, both right away and after others.
    fn requests() -> Vec<(u32, u32, SpanLen)> {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        (0..20_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let artifact = if x % 4 == 0 { (x >> 8) as u32 % 7 } else { 0 };
                let start = (x >> 16) as u32 % 3000;
                let len = match (x >> 40) % 3 {
                    0 => SpanLen::Empty,
                    1 => SpanLen::ToLineEnd,
                    _ => SpanLen::ByteLen((x >> 48) as u32 % 4),
                };
                (artifact, start, len)
            })
            .collect()
    }

    /// Numbers every table the way the builder must: each new row gets the next ID, in the order
    /// [`SourceInfoBuilder::span_for`] first sees it.
    fn reference(requests: &[(u32, u32, SpanLen)]) -> (SourceInfo, Vec<FileSpanId>) {
        use std::collections::BTreeMap;
        fn intern<K: Ord>(map: &mut BTreeMap<K, u32>, k: K, push: impl FnOnce()) -> u32 {
            let next = map.len() as u32 + 1;
            *map.entry(k).or_insert_with(|| {
                push();
                next
            })
        }
        let mut info = SourceInfo::default();
        let (mut arts, mut files, mut spans, mut fspans) = Default::default();
        let mut ids = Vec::new();
        for &(n, start, len) in requests {
            let k = key(n);
            let a = intern(&mut arts, k.clone(), || {
                info.artifacts.push(crate::ArtifactRecord {
                    canonical_path: k.path.clone(),
                    sub_artifact_id: k.sub_artifact_id,
                    encoding: k.encoding,
                    content_hash: k.hash.clone(),
                })
            });
            let f = intern(&mut files, a, || {
                info.files.push(crate::FileEntry {
                    artifact: ArtifactId(a),
                })
            });
            let s = intern(&mut spans, (start, len), || {
                info.spans.push(crate::Span { start, len })
            });
            let fs = intern(&mut fspans, (f, s), || {
                info.file_spans.push(crate::FileSpan {
                    file: FileId(f),
                    span: crate::SpanId(s),
                })
            });
            ids.push(FileSpanId(fs));
        }
        (info, ids)
    }

    fn assert_same(got: &SourceInfo, want: &SourceInfo) {
        let paths = |i: &SourceInfo| {
            i.artifacts
                .iter()
                .map(|a| {
                    (
                        a.canonical_path.clone(),
                        a.sub_artifact_id,
                        a.content_hash.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(got), paths(want));
        let files = |i: &SourceInfo| i.files.iter().map(|f| f.artifact).collect::<Vec<_>>();
        assert_eq!(files(got), files(want));
        assert_eq!(got.spans, want.spans);
        assert_eq!(got.file_spans, want.file_spans);
    }

    #[test]
    fn span_for_numbers_rows_in_first_seen_order() {
        let requests = requests();
        let (want, want_ids) = reference(&requests);
        let mut b = make_builder();
        let ids: Vec<_> = requests
            .iter()
            .map(|&(n, start, len)| b.span_for(key(n), start, len))
            .collect();
        assert_eq!(ids, want_ids);
        assert_same(&b.finish(), &want);
    }

    #[test]
    fn file_for_and_span_in_file_match_span_for() {
        let requests = requests();
        let (want, want_ids) = reference(&requests);
        let mut b = make_builder();
        let ids: Vec<_> = requests
            .iter()
            .map(|&(n, start, len)| {
                let file = b.file_for(&key(n));
                b.span_in_file(file, start, len)
            })
            .collect();
        assert_eq!(ids, want_ids);
        assert_same(&b.finish(), &want);
    }

    #[test]
    fn tables_rebuilt_from_rows_find_existing_ids() {
        let requests = requests();
        let (want, _) = reference(&requests);
        let mut spans = SpanTable::from_spans(want.spans.clone());
        let mut file_spans = FileSpanTable::from_spans(want.file_spans.clone());
        for (i, s) in want.spans.iter().enumerate() {
            assert_eq!(spans.get_or_intern(s.start, s.len).0 as usize, i + 1);
        }
        for (i, fs) in want.file_spans.iter().enumerate() {
            assert_eq!(file_spans.get_or_intern(fs.file, fs.span).0 as usize, i + 1);
        }
        assert_eq!(spans.len(), want.spans.len());
        assert_eq!(file_spans.len(), want.file_spans.len());
    }
}
