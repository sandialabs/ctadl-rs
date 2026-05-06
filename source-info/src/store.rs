use std::collections::BTreeMap;
use std::sync::Mutex;

pub trait KvStore: Send + Sync + std::fmt::Debug {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]>;
    fn insert(&self, key: &[u8], val: [u8; 4]);
}

/// In-memory BTreeMap store (default, always available).
#[derive(Debug)]
pub struct MemStore(Mutex<BTreeMap<Box<[u8]>, [u8; 4]>>);

impl MemStore {
    pub fn new() -> Self {
        MemStore(Mutex::new(BTreeMap::new()))
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0.lock().unwrap().get(key).copied()
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.lock().unwrap().insert(key.into(), val);
    }
}

/// Typed wrapper for the artifacts KV store.
#[derive(Debug)]
pub struct ArtifactsStore(pub Box<dyn KvStore>);

/// Typed wrapper for the files KV store.
#[derive(Debug)]
pub struct FilesStore(pub Box<dyn KvStore>);

/// Typed wrapper for the spans KV store.
#[derive(Debug)]
pub struct SpansStore(pub Box<dyn KvStore>);

/// Typed wrapper for the file-spans KV store.
#[derive(Debug)]
pub struct FileSpansStore(pub Box<dyn KvStore>);

impl KvStore for ArtifactsStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0.get(key)
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.insert(key, val)
    }
}
impl KvStore for FilesStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0.get(key)
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.insert(key, val)
    }
}
impl KvStore for SpansStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0.get(key)
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.insert(key, val)
    }
}
impl KvStore for FileSpansStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0.get(key)
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.insert(key, val)
    }
}

/// sled::Tree-backed store (optional, requires "sled" feature).
#[cfg(feature = "sled")]
pub struct SledStore(pub sled::Tree);

#[cfg(feature = "sled")]
impl std::fmt::Debug for SledStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SledStore").field(&"<sled::Tree>").finish()
    }
}

#[cfg(feature = "sled")]
impl KvStore for SledStore {
    fn get(&self, key: &[u8]) -> Option<[u8; 4]> {
        self.0
            .get(key)
            .expect("sled error")
            .map(|v| v.as_ref().try_into().unwrap())
    }
    fn insert(&self, key: &[u8], val: [u8; 4]) {
        self.0.insert(key, &val).expect("sled error");
    }
}
