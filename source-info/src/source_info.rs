use std::path::Path;

use crate::artifact::{ArtifactEncoding, ArtifactMetadata, ArtifactRecord, HashAlgorithm};
use crate::file_table::FileEntry;
use crate::parquet_io;
use crate::span::{FileSpan, Span, SpanLen};

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceInfo {
    pub metadata: ArtifactMetadata,
    pub artifacts: Vec<ArtifactRecord>,
    pub files: Vec<FileEntry>,
    pub spans: Vec<Span>,
    pub file_spans: Vec<FileSpan>,
}

impl SourceInfo {
    /// Serialize to a directory of Parquet files.
    pub fn to_parquet(&self, dir: &Path) -> parquet_io::Result<()> {
        parquet_io::write_parquet_source_info(dir, self)
    }

    /// Deserialize from a directory of Parquet files.
    pub fn from_parquet(dir: &Path) -> parquet_io::Result<Self> {
        parquet_io::read_parquet_source_info(dir)
    }
}

impl std::fmt::Display for SourceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let algo = match self.metadata.hash_algorithm {
            HashAlgorithm::Sha256 => "sha256".to_string(),
            HashAlgorithm::Other(n) => format!("algo{n}"),
        };
        writeln!(
            f,
            "SourceInfo v{} [{}/{}]",
            self.metadata.version, algo, self.metadata.hash_len
        )?;
        writeln!(
            f,
            "  {} artifact(s), {} file(s), {} span(s), {} file_span(s)",
            self.artifacts.len(),
            self.files.len(),
            self.spans.len(),
            self.file_spans.len(),
        )?;

        // Artifacts
        writeln!(f, "\nArtifacts:")?;
        for (i, a) in self.artifacts.iter().enumerate() {
            let enc = match a.encoding {
                ArtifactEncoding::Utf8 => "utf8",
                ArtifactEncoding::Utf16 => "utf16",
                ArtifactEncoding::Binary => "binary",
            };
            let hash_hex: String = a
                .content_hash
                .iter()
                .take(8)
                .map(|b| format!("{b:02x}"))
                .collect();
            let hash_str = if a.content_hash.len() > 8 {
                format!("{algo}:{hash_hex}...")
            } else {
                format!("{algo}:{hash_hex}")
            };
            writeln!(
                f,
                "  [{}] {}  sub={}  {}  {}",
                i + 1,
                a.canonical_path,
                a.sub_artifact_id,
                enc,
                hash_str
            )?;
        }

        // Files
        writeln!(f, "\nFiles:")?;
        for (i, file) in self.files.iter().enumerate() {
            let path = self
                .artifacts
                .get(file.artifact.0 as usize - 1)
                .map(|a| a.canonical_path.as_str())
                .unwrap_or("<unknown>");
            writeln!(f, "  [{}] artifact[{}] {}", i + 1, file.artifact.0, path)?;
        }

        // Spans
        writeln!(f, "\nSpans:")?;
        for (i, span) in self.spans.iter().enumerate() {
            let len_str = match span.len {
                SpanLen::Empty => "(empty)".to_string(),
                SpanLen::ByteLen(n) => format!("+{n}"),
                SpanLen::ToLineEnd => "to-line-end".to_string(),
            };
            writeln!(f, "  [{}] @{} {}", i + 1, span.start, len_str)?;
        }

        // FileSpans
        write!(f, "\nFileSpans:")?;
        for (i, fs) in self.file_spans.iter().enumerate() {
            let path = self
                .files
                .get(fs.file.0 as usize - 1)
                .and_then(|fe| self.artifacts.get(fe.artifact.0 as usize - 1))
                .map(|a| a.canonical_path.as_str())
                .unwrap_or("<unknown>");
            let span_str = self
                .spans
                .get(fs.span.0 as usize - 1)
                .map(|s| {
                    let len_str = match s.len {
                        SpanLen::Empty => "(empty)".to_string(),
                        SpanLen::ByteLen(n) => format!("+{n}"),
                        SpanLen::ToLineEnd => "to-line-end".to_string(),
                    };
                    format!("@{} {}", s.start, len_str)
                })
                .unwrap_or_else(|| "<unknown>".to_string());
            write!(
                f,
                "\n  [{}] file[{}] span[{}]  {} {}",
                i + 1,
                fs.file.0,
                fs.span.0,
                path,
                span_str
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactMetadata, HashAlgorithm};

    #[test]
    fn display_smoke() {
        let info = SourceInfo {
            metadata: ArtifactMetadata {
                hash_algorithm: HashAlgorithm::Sha256,
                hash_len: 32,
                version: 1,
            },
            artifacts: vec![],
            files: vec![],
            spans: vec![],
            file_spans: vec![],
        };
        let s = info.to_string();
        assert!(s.contains("SourceInfo"));
        assert!(s.contains("0 artifact"));
    }
}
