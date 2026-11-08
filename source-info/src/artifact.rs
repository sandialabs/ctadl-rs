//! Artifact table: immutable source artifacts with interning by
//! (canonical_path, sub_artifact_id, content_hash, encoding).

use crate::ids::ArtifactId;
use crate::index::{Index, Row};

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

/// Computes the SHA-256 digest of `bytes`, returning the raw 32-byte hash.
///
/// This is the canonical content hash used throughout source info (see
/// [`HashAlgorithm::Sha256`]).
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// Incremental SHA-256 hasher for computing the content hash of an artifact that
/// is read in pieces (e.g. a directory of files, or a large file streamed in
/// chunks). Feed bytes with [`ContentHasher::update`] and call
/// [`ContentHasher::finalize`] to obtain the raw digest.
pub struct ContentHasher {
    inner: sha2::Sha256,
}

impl ContentHasher {
    /// Creates a new SHA-256 content hasher.
    pub fn new() -> Self {
        use sha2::Digest;
        Self {
            inner: sha2::Sha256::new(),
        }
    }

    /// Feeds more bytes into the running hash.
    pub fn update(&mut self, bytes: &[u8]) {
        use sha2::Digest;
        self.inner.update(bytes);
    }

    /// Consumes the hasher and returns the raw digest.
    pub fn finalize(self) -> Vec<u8> {
        use sha2::Digest;
        self.inner.finalize().to_vec()
    }

    /// The hash algorithm this hasher implements.
    pub fn algorithm(&self) -> HashAlgorithm {
        HashAlgorithm::Sha256
    }
}

impl Default for ContentHasher {
    fn default() -> Self {
        Self::new()
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

/// The fields of an artifact that identify it, borrowed from an [`ArtifactKey`] or an
/// [`ArtifactRecord`], so that a key can be looked up without copying it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ArtifactKeyRef<'a> {
    path: &'a str,
    sub_artifact_id: u32,
    hash: &'a [u8],
    encoding: ArtifactEncoding,
}

impl ArtifactKey {
    pub(crate) fn as_key_ref(&self) -> ArtifactKeyRef<'_> {
        ArtifactKeyRef {
            path: &self.path,
            sub_artifact_id: self.sub_artifact_id,
            hash: &self.hash,
            encoding: self.encoding,
        }
    }
}

impl Row for ArtifactRecord {
    type Key<'a> = ArtifactKeyRef<'a>;

    fn key(&self) -> ArtifactKeyRef<'_> {
        ArtifactKeyRef {
            path: &self.canonical_path,
            sub_artifact_id: self.sub_artifact_id,
            hash: &self.content_hash,
            encoding: self.encoding,
        }
    }

    fn key_eq(a: &ArtifactKeyRef<'_>, b: &ArtifactKeyRef<'_>) -> bool {
        a == b
    }
}

/// Table of artifacts with interning. IDs are 1-based; index 0 is reserved.
#[derive(Debug)]
pub struct ArtifactTable {
    pub metadata: ArtifactMetadata,
    pub artifacts: Vec<ArtifactRecord>,
    index: Index<ArtifactRecord>,
}

impl ArtifactTable {
    pub fn new(metadata: ArtifactMetadata) -> Self {
        Self {
            metadata,
            artifacts: Vec::new(),
            index: Index::new(),
        }
    }

    /// Returns the artifact ID for the given key, interning a new record if needed, and copies
    /// `key` only when the artifact is new. IDs are 1-based.
    pub fn get_or_intern_artifact(&mut self, key: &ArtifactKey) -> ArtifactId {
        let id = self
            .index
            .get_or_insert_with(&mut self.artifacts, key.as_key_ref(), || ArtifactRecord {
                canonical_path: key.path.clone(),
                sub_artifact_id: key.sub_artifact_id,
                encoding: key.encoding,
                content_hash: key.hash.clone(),
            });
        ArtifactId(id)
    }

    pub fn get(&self, id: ArtifactId) -> Option<&ArtifactRecord> {
        self.artifacts.get((id.0 as usize).checked_sub(1)?)
    }

    /// Builds a table from metadata and records (e.g. when deserializing). Assigns IDs 1, 2, ...
    pub fn from_records(metadata: ArtifactMetadata, artifacts: Vec<ArtifactRecord>) -> Self {
        let index = Index::from_rows(&artifacts);
        Self {
            metadata,
            artifacts,
            index,
        }
    }
}
