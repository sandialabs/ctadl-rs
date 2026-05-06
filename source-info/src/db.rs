#[cfg(not(feature = "sled"))]
use crate::store::MemStore;
use crate::store::{ArtifactsStore, FileSpansStore, FilesStore, SpansStore};

#[cfg(feature = "sled")]
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

#[cfg(feature = "sled")]
use crate::store::SledStore;

#[cfg(feature = "sled")]
static DB: OnceLock<sled::Db> = OnceLock::new();

#[cfg(feature = "sled")]
static SESSION: AtomicU64 = AtomicU64::new(0);

/// Initialize the global sled database. First call wins; subsequent calls are ignored.
#[cfg(feature = "sled")]
pub fn init(db: sled::Db) {
    let _ = DB.set(db);
}

#[cfg(feature = "sled")]
pub(crate) fn get() -> &'static sled::Db {
    DB.get_or_init(|| {
        sled::Config::new()
            .temporary(true)
            .open()
            .expect("sled error")
    })
}

pub(crate) fn open_session_trees() -> (ArtifactsStore, FilesStore, SpansStore, FileSpansStore) {
    #[cfg(feature = "sled")]
    {
        let id = SESSION.fetch_add(1, Ordering::Relaxed);
        let db = get();
        return (
            ArtifactsStore(Box::new(SledStore(
                db.open_tree(format!("{id}/artifacts")).expect("sled error"),
            ))),
            FilesStore(Box::new(SledStore(
                db.open_tree(format!("{id}/files")).expect("sled error"),
            ))),
            SpansStore(Box::new(SledStore(
                db.open_tree(format!("{id}/spans")).expect("sled error"),
            ))),
            FileSpansStore(Box::new(SledStore(
                db.open_tree(format!("{id}/file_spans"))
                    .expect("sled error"),
            ))),
        );
    }
    #[cfg(not(feature = "sled"))]
    (
        ArtifactsStore(Box::new(MemStore::new())),
        FilesStore(Box::new(MemStore::new())),
        SpansStore(Box::new(MemStore::new())),
        FileSpansStore(Box::new(MemStore::new())),
    )
}
