//! The sled database that holds the indexes of interned tables when the `sled` feature is on.

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

static DB: OnceLock<sled::Db> = OnceLock::new();

static TREES: AtomicU64 = AtomicU64::new(0);

/// Initialize the global sled database. First call wins; subsequent calls are ignored.
pub fn init(db: sled::Db) {
    let _ = DB.set(db);
}

pub(crate) fn get() -> &'static sled::Db {
    DB.get_or_init(|| {
        sled::Config::new()
            .temporary(true)
            .open()
            .expect("sled error")
    })
}

/// Opens a tree no other index uses.
pub(crate) fn open_tree() -> sled::Tree {
    let id = TREES.fetch_add(1, Ordering::Relaxed);
    get().open_tree(format!("index/{id}")).expect("sled error")
}
