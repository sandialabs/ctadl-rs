//! Artifact table: immutable source artifacts with interning by
//! (canonical_path, sub_artifact_id, content_hash).

use crate::ids::ArtifactId;
use crate::store::{ArtifactsStore, KvStore};

/// Algorithm used for content hashing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HashAlgorithm {
    Sha256,
    Other(u8),
}

impl HashAlgorithm {
    pub fn to_u8(self) -> u8 {
        match self {
            HashAlgorithm::Sha256 => 0,
            HashAlgorithm::Other(n) => n,
        }
    }

    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => HashAlgorithm::Sha256,
            n => HashAlgorithm::Other(n),
        }
    }
}

/// How artifact content is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ArtifactEncoding {
    Utf8,
    Utf16,
    Binary,
}

impl ArtifactEncoding {
    pub fn to_u8(self) -> u8 {
        match self {
            ArtifactEncoding::Utf8 => 0,
            ArtifactEncoding::Utf16 => 1,
            ArtifactEncoding::Binary => 2,
        }
    }

    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => ArtifactEncoding::Utf8,
            1 => ArtifactEncoding::Utf16,
            _ => ArtifactEncoding::Binary,
        }
    }
}

/// Metadata for the artifact table.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactMetadata {
    pub hash_algorithm: HashAlgorithm,
    pub hash_len: u8,
    pub version: u16,
}

impl ArtifactMetadata {
    /// Uses SHA256 hash algorithm
    pub fn new() -> Self {
        Self {
            hash_algorithm: HashAlgorithm::Sha256,
            hash_len: 32,
            version: 1,
        }
    }
}

impl Default for ArtifactMetadata {
    fn default() -> Self {
        ArtifactMetadata::new()
    }
}

/// A single artifact record.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactRecord {
    pub canonical_path: String,
    pub sub_artifact_id: u32,
    pub encoding: ArtifactEncoding,
    pub content_hash: Vec<u8>,
}

/// Key for deduplication: (canonical_path, sub_artifact_id, content_hash, encoding).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactKey {
    pub path: String,
    pub sub_artifact_id: u32,
    pub hash: Vec<u8>,
    pub encoding: ArtifactEncoding,
}

/// Table of artifacts with interning. IDs are 1-based; index 0 is reserved.
#[derive(Debug)]
pub struct ArtifactTable {
    pub metadata: ArtifactMetadata,
    pub artifacts: Vec<ArtifactRecord>,
    store: Box<dyn KvStore>,
}

fn artifact_key_bytes(key: &ArtifactKey) -> Vec<u8> {
    let path_bytes = key.path.as_bytes();
    let path_len = path_bytes.len() as u32;
    let mut buf = Vec::with_capacity(4 + path_bytes.len() + 4 + 1 + key.hash.len());
    buf.extend_from_slice(&path_len.to_be_bytes());
    buf.extend_from_slice(path_bytes);
    buf.extend_from_slice(&key.sub_artifact_id.to_be_bytes());
    buf.push(key.encoding.to_u8());
    buf.extend_from_slice(&key.hash);
    buf
}

impl ArtifactTable {
    pub fn new(metadata: ArtifactMetadata, store: ArtifactsStore) -> Self {
        Self {
            metadata,
            artifacts: Vec::new(),
            store: store.0,
        }
    }

    /// Returns the artifact ID for the given key, interning a new record if needed.
    /// IDs are 1-based.
    pub fn get_or_intern_artifact(&mut self, key: ArtifactKey) -> ArtifactId {
        let key_bytes = artifact_key_bytes(&key);
        if let Some(val) = self.store.get(&key_bytes) {
            return ArtifactId(u32::from_be_bytes(val));
        }
        let id = ArtifactId(self.artifacts.len() as u32 + 1);
        self.artifacts.push(ArtifactRecord {
            canonical_path: key.path.clone(),
            sub_artifact_id: key.sub_artifact_id,
            encoding: key.encoding,
            content_hash: key.hash.clone(),
        });
        self.store.insert(&key_bytes, id.0.to_be_bytes());
        id
    }

    pub fn get(&self, id: ArtifactId) -> Option<&ArtifactRecord> {
        let idx = id.0 as usize;
        if idx == 0 || idx > self.artifacts.len() {
            return None;
        }
        self.artifacts.get(idx - 1)
    }

    /// Builds a table from metadata and records (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_records(
        metadata: ArtifactMetadata,
        artifacts: Vec<ArtifactRecord>,
        store: ArtifactsStore,
    ) -> Self {
        for (i, rec) in artifacts.iter().enumerate() {
            let key = ArtifactKey {
                path: rec.canonical_path.clone(),
                sub_artifact_id: rec.sub_artifact_id,
                hash: rec.content_hash.clone(),
                encoding: rec.encoding,
            };
            let id = ArtifactId((i + 1) as u32);
            store
                .0
                .insert(&artifact_key_bytes(&key), id.0.to_be_bytes());
        }
        Self {
            metadata,
            artifacts,
            store: store.0,
        }
    }
}
