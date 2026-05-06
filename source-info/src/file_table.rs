//! File table: maps FileId to ArtifactId. One file per artifact; interning by artifact.

use crate::ids::{ArtifactId, FileId};
use crate::store::{FilesStore, KvStore};

/// A file entry references a single artifact.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileEntry {
    pub artifact: ArtifactId,
}

/// Table of files with interning by artifact. IDs are 1-based.
#[derive(Debug)]
pub struct FileTable {
    pub files: Vec<FileEntry>,
    store: Box<dyn KvStore>,
}

impl FileTable {
    pub fn new(store: FilesStore) -> Self {
        Self {
            files: Vec::new(),
            store: store.0,
        }
    }

    /// Returns the file ID for the given artifact, interning a new entry if needed.
    pub fn get_or_intern_file(&mut self, artifact_id: ArtifactId) -> FileId {
        let key = artifact_id.0.to_be_bytes();
        if let Some(val) = self.store.get(&key) {
            return FileId(u32::from_be_bytes(val));
        }
        let id = FileId(self.files.len() as u32 + 1);
        self.files.push(FileEntry {
            artifact: artifact_id,
        });
        self.store.insert(&key, id.0.to_be_bytes());
        id
    }

    pub fn get(&self, id: FileId) -> Option<&FileEntry> {
        let idx = id.0 as usize;
        if idx == 0 || idx > self.files.len() {
            return None;
        }
        self.files.get(idx - 1)
    }

    /// Builds a table from a list of entries (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_entries(files: Vec<FileEntry>, store: FilesStore) -> Self {
        for (i, e) in files.iter().enumerate() {
            let id = FileId((i + 1) as u32);
            store
                .0
                .insert(&e.artifact.0.to_be_bytes(), id.0.to_be_bytes());
        }
        Self {
            files,
            store: store.0,
        }
    }
}
