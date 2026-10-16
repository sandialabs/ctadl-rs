/*! Source lines: where an instruction's span points.

An instruction's span is its source when the assembly's portable PDB maps it to some, and its own
bytes in the assembly otherwise. The source is the sequence point that covers the instruction's IL
offset (the last at or before it), as a span of the document's bytes from the point's start to its
end, so that SARIF regions are `.cs` lines and columns.

A document is used only if its file is on disk under the path the PDB gives and hashes to the hash
the PDB records: a span into a file that has changed since the build would point at the wrong
line. Without a PDB, before the first sequence point, under a hidden one (compiler-generated code
with no line), and in a document that is missing or changed, the span is the instruction's bytes,
as it was before PDBs were read.

Lines are counted at `\n`, as the SARIF formatter's line map counts them; columns count UTF-16
code units, as the compiler wrote them, and are converted to UTF-8 bytes here.
*/

use std::path::Path;

use cil_reader::model::pdb::hash_algorithm;
use cil_reader::model::{PortablePdb, SequencePoint};
use ctadl_ir::SourceInfo;
use hashbrown::HashMap;
use source_info::{ArtifactEncoding, ArtifactKey, SourceInfoBuilder, SpanLen};

/// Spans for one assembly's instructions.
pub struct ModuleSpans<'p> {
    /// The assembly's own bytes.
    pub key: ArtifactKey,
    pdb: Option<PortablePdb<'p>>,
    /// Documents by rid, loaded on first use; `None` where unusable.
    docs: HashMap<u32, Option<SourceFile>>,
    pub stats: LineStats,
}

/// What source lines an import found.
#[derive(Debug, Default, Clone)]
pub struct LineStats {
    /// Assemblies with a portable PDB.
    pub pdbs: usize,
    /// Method bodies with at least one sequence point.
    pub methods_with_lines: usize,
    /// Documents read, by path.
    pub documents: Vec<String>,
    /// Documents not used, by path, with why.
    pub unusable_documents: Vec<(String, &'static str)>,
}

impl LineStats {
    /// Adds another assembly's.
    pub fn add(&mut self, other: LineStats) {
        self.pdbs += other.pdbs;
        self.methods_with_lines += other.methods_with_lines;
        self.documents.extend(other.documents);
        self.unusable_documents.extend(other.unusable_documents);
    }
}

struct SourceFile {
    key: ArtifactKey,
    text: Vec<u8>,
    /// The byte offset of each line's start; line `n` (1-based) starts at `line_starts[n - 1]`.
    line_starts: Vec<u32>,
}

impl<'p> ModuleSpans<'p> {
    pub fn new(key: ArtifactKey, pdb: Option<PortablePdb<'p>>) -> Self {
        let stats = LineStats {
            pdbs: usize::from(pdb.is_some()),
            ..LineStats::default()
        };
        ModuleSpans {
            key,
            pdb,
            docs: HashMap::new(),
            stats,
        }
    }

    /// The spans of the method with MethodDef `rid`, whose code starts at `code_file_offset`.
    pub fn method(&mut self, rid: u32, code_file_offset: u32) -> MethodSpans<'_, 'p> {
        let points = match &self.pdb {
            Some(pdb) => pdb.sequence_points(rid).unwrap_or_else(|e| {
                log::debug!("cil: sequence points of method 0x{rid:x}: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        if !points.is_empty() {
            self.stats.methods_with_lines += 1;
        }
        MethodSpans {
            module: self,
            points,
            code_file_offset,
        }
    }

    fn document(&mut self, rid: u32) -> Option<&SourceFile> {
        if !self.docs.contains_key(&rid) {
            let loaded = self.load(rid);
            self.docs.insert(rid, loaded);
        }
        self.docs.get(&rid).and_then(Option::as_ref)
    }

    fn load(&mut self, rid: u32) -> Option<SourceFile> {
        let doc = self.pdb.as_ref()?.document(rid).ok()?;
        let unusable = |stats: &mut LineStats, why| {
            log::debug!("cil: no source lines from {}: {why}", doc.name);
            stats.unusable_documents.push((doc.name.clone(), why));
            None
        };
        let Ok(text) = std::fs::read(Path::new(&doc.name)) else {
            return unusable(&mut self.stats, "not found");
        };
        let matches = if doc.hash_algorithm == hash_algorithm::SHA1 {
            use sha1::Digest;
            sha1::Sha1::digest(&text).as_slice() == doc.hash
        } else if doc.hash_algorithm == hash_algorithm::SHA256 {
            source_info::sha256(&text) == doc.hash
        } else {
            // No hash, or one of an algorithm the PDB format does not name: nothing to check.
            true
        };
        if !matches {
            return unusable(&mut self.stats, "changed since the build");
        }
        self.stats.documents.push(doc.name.clone());
        let mut line_starts = vec![0u32];
        line_starts.extend(
            text.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i as u32 + 1),
        );
        Some(SourceFile {
            key: ArtifactKey {
                path: doc.name.clone(),
                sub_artifact_id: 0,
                hash: source_info::sha256(&text),
                encoding: ArtifactEncoding::Utf8,
            },
            text,
            line_starts,
        })
    }
}

/// Spans for one method body's instructions.
pub struct MethodSpans<'m, 'p> {
    module: &'m mut ModuleSpans<'p>,
    /// In IL order.
    points: Vec<SequencePoint>,
    code_file_offset: u32,
}

impl MethodSpans<'_, '_> {
    /// The span of the `size` bytes of IL at `offset`.
    pub fn span(&mut self, builder: &mut SourceInfoBuilder, offset: u32, size: u32) -> SourceInfo {
        let covering = self.points.partition_point(|p| p.offset <= offset);
        if let Some(point) = covering.checked_sub(1).map(|i| self.points[i])
            && !point.is_hidden()
            && let Some(file) = self.module.document(point.document)
            && let Some((start, end)) = file.byte_range(&point)
        {
            let len = if end > start {
                SpanLen::ByteLen(end - start)
            } else {
                SpanLen::Empty
            };
            return SourceInfo::new(builder.span_for(file.key.clone(), start, len));
        }
        SourceInfo::new(builder.span_for(
            self.module.key.clone(),
            self.code_file_offset + offset,
            SpanLen::ByteLen(size),
        ))
    }
}

impl SourceFile {
    /// The bytes from a point's start to its end; `None` if the start is not in the file.
    fn byte_range(&self, point: &SequencePoint) -> Option<(u32, u32)> {
        let start = self.offset(point.start_line, point.start_column)?;
        let end = self
            .offset(point.end_line, point.end_column)
            .unwrap_or(start)
            .max(start);
        Some((start, end))
    }

    /// The byte offset of 1-based `line` and `column`, the column in UTF-16 code units and
    /// clamped to the line's end.
    fn offset(&self, line: i32, column: i32) -> Option<u32> {
        let index = usize::try_from(line).ok()?.checked_sub(1)?;
        let line_start = *self.line_starts.get(index)?;
        let line_end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |&next| next as usize - 1);
        let line_text = &self.text[line_start as usize..line_end];
        let mut units = usize::try_from(column).ok()?.saturating_sub(1);
        let mut bytes = 0usize;
        for ch in String::from_utf8_lossy(line_text).chars() {
            if units == 0 {
                break;
            }
            units = units.saturating_sub(ch.len_utf16());
            bytes += ch.len_utf8();
        }
        Some(line_start + bytes.min(line_text.len()) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> SourceFile {
        let text = text.as_bytes().to_vec();
        let mut line_starts = vec![0u32];
        line_starts.extend(
            text.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i as u32 + 1),
        );
        SourceFile {
            key: ArtifactKey {
                path: "x.cs".into(),
                sub_artifact_id: 0,
                hash: Vec::new(),
                encoding: ArtifactEncoding::Utf8,
            },
            text,
            line_starts,
        }
    }

    #[test]
    fn columns_are_utf16_units() {
        // `é` is one UTF-16 unit and two UTF-8 bytes; `𝄞` is two units and four bytes.
        let f = file("ab\n\té𝄞x = 1;\r\n");
        assert_eq!(f.offset(1, 1), Some(0));
        assert_eq!(f.offset(2, 1), Some(3));
        assert_eq!(f.offset(2, 3), Some(3 + 1 + 2));
        assert_eq!(f.offset(2, 5), Some(3 + 1 + 2 + 4));
        // Past the line's end: clamped to it; a CRLF's `\r` is part of the line.
        assert_eq!(f.offset(2, 99), Some(3 + 14));
        assert_eq!(f.offset(3, 1), Some(f.text.len() as u32));
        assert_eq!(f.offset(4, 1), None);
        assert_eq!(f.offset(0, 1), None);
    }
}
