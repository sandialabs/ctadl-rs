/*! Source lines: where an instruction's span points.

An instruction's span is its source when the assembly's portable PDB maps it to some, and its own
bytes in the assembly otherwise. The source is the sequence point that covers the instruction's IL
offset (the last at or before it), as a span of the document's bytes from the point's start to its
end, so that SARIF regions are `.cs` lines and columns.

A document is used only if its file is on disk under the path the PDB gives and hashes to the hash
the PDB records: a span into a file that has changed since the build would point at the wrong
line. Without a PDB, before the first sequence point, under a hidden one (compiler-generated code
with no line), and in a document that is missing or changed, the span is the instruction's bytes.

Lines are counted at `\n`, as the SARIF formatter's line map counts them; columns count UTF-16
code units, as the compiler wrote them, and are converted to UTF-8 bytes here.

Methods are lowered on several threads, so a method's spans are not interned as they are met:
[`MethodSpans`] lists them, and a statement's span id is, until the method is placed in the
program, 1 + its span's position in that list ([`SpanList::intern`]).
*/

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cil_reader::model::pdb::hash_algorithm;
use cil_reader::model::{PortablePdb, SequencePoint};
use ctadl_ir::SourceInfo;
use hashbrown::HashMap;
use source_info::{ArtifactEncoding, ArtifactKey, FileId, FileSpanId, SourceInfoBuilder, SpanLen};

/// A document, loaded by the first method that needs it; `None` where unusable.
type Document = Arc<OnceLock<Option<SourceFile>>>;

/// Spans for one assembly's instructions, shared by the threads lowering its methods.
pub struct ModuleSpans<'p> {
    /// The assembly's own bytes.
    pub key: ArtifactKey,
    pdb: Option<PortablePdb<'p>>,
    /// Documents by rid.
    docs: Mutex<HashMap<u32, Document>>,
    stats: Mutex<LineStats>,
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
            docs: Mutex::new(HashMap::new()),
            stats: Mutex::new(stats),
        }
    }

    /// What this assembly's methods found.
    pub fn into_stats(self) -> LineStats {
        self.stats
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn stats(&self) -> std::sync::MutexGuard<'_, LineStats> {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The spans of the method with MethodDef `rid`, whose code starts at `code_file_offset`.
    pub fn method(&self, rid: u32, code_file_offset: u32) -> MethodSpans<'_, 'p> {
        let points = match &self.pdb {
            Some(pdb) => pdb.sequence_points(rid).unwrap_or_else(|e| {
                log::debug!("cil: sequence points of method 0x{rid:x}: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        };
        if !points.is_empty() {
            self.stats().methods_with_lines += 1;
        }
        MethodSpans {
            module: self,
            points,
            code_file_offset,
            docs: Vec::new(),
            list: SpanList::default(),
        }
    }

    fn document(&self, rid: u32) -> Document {
        let doc = Arc::clone(
            self.docs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(rid)
                .or_default(),
        );
        // Loaded outside the map's lock; a thread that wants it meanwhile waits for it here.
        doc.get_or_init(|| self.load(rid));
        doc
    }

    fn load(&self, rid: u32) -> Option<SourceFile> {
        let doc = self.pdb.as_ref()?.document(rid).ok()?;
        let unusable = |why| {
            log::debug!("cil: no source lines from {}: {why}", doc.name);
            self.stats()
                .unusable_documents
                .push((doc.name.clone(), why));
            None
        };
        let Ok(text) = std::fs::read(Path::new(&doc.name)) else {
            return unusable("not found");
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
            return unusable("changed since the build");
        }
        self.stats().documents.push(doc.name.clone());
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

/// The spans one method's statements use, in the order lowering met them, not yet interned.
#[derive(Default)]
pub struct SpanList {
    /// The documents the spans are in, after the assembly itself.
    files: Vec<ArtifactKey>,
    /// Each span: its file (0 for the assembly, `n` for `files[n - 1]`), start and length.
    spans: Vec<(u32, u32, SpanLen)>,
}

impl SpanList {
    /// Interns the spans into `builder`, in order, the assembly's under `module`: the id each
    /// gets, by position.
    pub fn intern(&self, builder: &mut SourceInfoBuilder, module: &ArtifactKey) -> Vec<FileSpanId> {
        // A file is looked up at its first span: that is when interning a span in it numbers it.
        let mut files: Vec<Option<FileId>> = vec![None; self.files.len() + 1];
        self.spans
            .iter()
            .map(|&(f, start, len)| {
                let file = *files[f as usize].get_or_insert_with(|| {
                    builder.file_for(match f {
                        0 => module,
                        n => &self.files[n as usize - 1],
                    })
                });
                builder.span_in_file(file, start, len)
            })
            .collect()
    }
}

/// Spans for one method body's instructions.
pub struct MethodSpans<'m, 'p> {
    module: &'m ModuleSpans<'p>,
    /// In IL order.
    points: Vec<SequencePoint>,
    code_file_offset: u32,
    /// The documents this method's points are in, by rid, with their file in `list`.
    docs: Vec<(u32, Document, u32)>,
    list: SpanList,
}

impl MethodSpans<'_, '_> {
    /// The span of the `size` bytes of IL at `offset`, as an id into [`Self::finish`]'s list.
    pub fn span(&mut self, offset: u32, size: u32) -> SourceInfo {
        let covering = self.points.partition_point(|p| p.offset <= offset);
        let span = covering
            .checked_sub(1)
            .map(|i| self.points[i])
            .filter(|point| !point.is_hidden())
            .and_then(|point| self.in_document(&point));
        let span =
            span.unwrap_or_else(|| (0, self.code_file_offset + offset, SpanLen::ByteLen(size)));
        self.list.spans.push(span);
        SourceInfo::new(FileSpanId(self.list.spans.len() as u32))
    }

    /// The span of `point` in its document, if that is usable.
    fn in_document(&mut self, point: &SequencePoint) -> Option<(u32, u32, SpanLen)> {
        let at = match self
            .docs
            .iter()
            .position(|(rid, ..)| *rid == point.document)
        {
            Some(at) => at,
            None => {
                let doc = self.module.document(point.document);
                self.docs.push((point.document, doc, 0));
                self.docs.len() - 1
            }
        };
        let (_, doc, file) = &mut self.docs[at];
        let source = doc.get()?.as_ref()?;
        let (start, end) = source.byte_range(point)?;
        if *file == 0 {
            self.list.files.push(source.key.clone());
            *file = self.list.files.len() as u32;
        }
        let len = if end > start {
            SpanLen::ByteLen(end - start)
        } else {
            SpanLen::Empty
        };
        Some((*file, start, len))
    }

    /// The spans [`Self::span`] gave out.
    pub fn finish(self) -> SpanList {
        self.list
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
