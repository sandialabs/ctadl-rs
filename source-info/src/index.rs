//! Hash index that interns the rows of a table.
//!
//! A table keeps its rows in a `Vec`, and a row's ID is its 1-based position there. The index maps
//! a row's key to that ID, so interning a row that is already present returns its first ID and IDs
//! stay in first-seen order.
//!
//! In memory, the index stores only the `u32` ID of each row. It hashes and compares keys by
//! looking at the row in the table's `Vec`, so the key is not stored a second time; that costs
//! about five bytes per row instead of a copy of the key. With the `sled` feature the index is
//! instead a sled tree keyed by a byte encoding of the key, which lets it live on disk.

#[cfg(not(feature = "sled"))]
use hashbrown::{DefaultHashBuilder, HashTable, hash_table::Entry};
#[cfg(not(feature = "sled"))]
use std::hash::BuildHasher;
use std::marker::PhantomData;

/// A row of an interned table, and the key that identifies it.
pub(crate) trait Row {
    /// The fields that identify the row, borrowed from it.
    type Key<'a>: std::hash::Hash + Eq
    where
        Self: 'a;

    fn key(&self) -> Self::Key<'_>;

    /// Whether two keys are equal. Keys borrowed for different lifetimes are different types to
    /// generic code, so each row type compares them itself.
    #[cfg_attr(feature = "sled", allow(dead_code))]
    fn key_eq(a: &Self::Key<'_>, b: &Self::Key<'_>) -> bool;

    /// Encodes `key` as the bytes the sled index stores it under.
    #[cfg(feature = "sled")]
    fn key_bytes(key: &Self::Key<'_>) -> Vec<u8>;
}

/// Maps the key of each row in a table to the row's 1-based ID. See the module docs.
pub(crate) struct Index<T: Row> {
    #[cfg(not(feature = "sled"))]
    table: HashTable<u32>,
    #[cfg(not(feature = "sled"))]
    hasher: DefaultHashBuilder,
    #[cfg(feature = "sled")]
    tree: sled::Tree,
    row: PhantomData<fn(&T)>,
}

impl<T: Row> std::fmt::Debug for Index<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").finish_non_exhaustive()
    }
}

impl<T: Row> Default for Index<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// The row with 1-based `id`.
#[cfg(not(feature = "sled"))]
fn row<T>(rows: &[T], id: u32) -> &T {
    &rows[id as usize - 1]
}

impl<T: Row> Index<T> {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(not(feature = "sled"))]
            table: HashTable::new(),
            #[cfg(not(feature = "sled"))]
            hasher: DefaultHashBuilder::default(),
            #[cfg(feature = "sled")]
            tree: crate::db::open_tree(),
            row: PhantomData,
        }
    }

    /// Indexes `rows`, which already hold IDs 1, 2, ... in order and have distinct keys, as an
    /// interned table's rows do.
    pub(crate) fn from_rows(rows: &[T]) -> Self {
        let mut index = Self::new();
        #[cfg(not(feature = "sled"))]
        {
            let hasher = &index.hasher;
            index
                .table
                .reserve(rows.len(), |&id| hasher.hash_one(row(rows, id).key()));
        }
        for (i, r) in rows.iter().enumerate() {
            index.insert_new(rows, &r.key(), i as u32 + 1);
        }
        index
    }

    /// Returns the ID of the row in `rows` whose key is `key`. If there is none, pushes `make()`,
    /// whose key must be `key`, and returns its ID.
    pub(crate) fn get_or_insert_with(
        &mut self,
        rows: &mut Vec<T>,
        key: T::Key<'_>,
        make: impl FnOnce() -> T,
    ) -> u32 {
        #[cfg(not(feature = "sled"))]
        {
            let hasher = &self.hasher;
            let hash = hasher.hash_one(&key);
            let entry = self.table.entry(
                hash,
                |&id| T::key_eq(&row(rows, id).key(), &key),
                |&id| hasher.hash_one(row(rows, id).key()),
            );
            match entry {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => {
                    rows.push(make());
                    let id = rows.len() as u32;
                    e.insert(id);
                    id
                }
            }
        }
        #[cfg(feature = "sled")]
        {
            let bytes = T::key_bytes(&key);
            if let Some(v) = self.tree.get(&bytes).expect("sled error") {
                return u32::from_be_bytes(v.as_ref().try_into().expect("sled value is a u32"));
            }
            rows.push(make());
            let id = rows.len() as u32;
            self.tree
                .insert(bytes, &id.to_be_bytes())
                .expect("sled error");
            id
        }
    }

    /// Records that the row with `key` has `id`, which is not yet in the index.
    fn insert_new(&mut self, rows: &[T], key: &T::Key<'_>, id: u32) {
        #[cfg(not(feature = "sled"))]
        {
            let hasher = &self.hasher;
            self.table.insert_unique(hasher.hash_one(key), id, |&id| {
                hasher.hash_one(row(rows, id).key())
            });
        }
        #[cfg(feature = "sled")]
        {
            let _ = rows;
            self.tree
                .insert(T::key_bytes(key), &id.to_be_bytes())
                .expect("sled error");
        }
    }
}
