//! File table: maps FileId to ArtifactId. One file per artifact; interning by artifact.

use crate::ids::{ArtifactId, FileId};
use crate::index::{Index, Row};

/// A file entry references a single artifact.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileEntry {
    pub artifact: ArtifactId,
}

impl Row for FileEntry {
    type Key<'a> = ArtifactId;

    fn key(&self) -> ArtifactId {
        self.artifact
    }

    fn key_eq(a: &ArtifactId, b: &ArtifactId) -> bool {
        a == b
    }

    #[cfg(feature = "sled")]
    fn key_bytes(artifact: &ArtifactId) -> Vec<u8> {
        artifact.0.to_be_bytes().to_vec()
    }
}

/// Table of files with interning by artifact. IDs are 1-based.
#[derive(Debug, Default)]
pub struct FileTable {
    pub files: Vec<FileEntry>,
    index: Index<FileEntry>,
}

impl FileTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the file ID for the given artifact, interning a new entry if needed.
    pub fn get_or_intern_file(&mut self, artifact_id: ArtifactId) -> FileId {
        FileId(
            self.index
                .get_or_insert_with(&mut self.files, artifact_id, || FileEntry {
                    artifact: artifact_id,
                }),
        )
    }

    pub fn get(&self, id: FileId) -> Option<&FileEntry> {
        self.files.get((id.0 as usize).checked_sub(1)?)
    }

    /// Builds a table from a list of entries (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_entries(files: Vec<FileEntry>) -> Self {
        let index = Index::from_rows(&files);
        Self { files, index }
    }
}
