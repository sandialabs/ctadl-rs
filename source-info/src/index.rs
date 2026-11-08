//! Hash index that interns the rows of a table.
//!
//! A table keeps its rows in a `Vec`, and a row's ID is its 1-based position there. The index maps
//! a row's key to that ID, so interning a row that is already present returns its first ID and IDs
//! stay in first-seen order.
//!
//! The index stores only the `u32` ID of each row. It hashes and compares keys by looking at the
//! row in the table's `Vec`, so the key is not stored a second time; that costs about five bytes
//! per row instead of a copy of the key.

use hashbrown::{DefaultHashBuilder, HashTable, hash_table::Entry};
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
    fn key_eq(a: &Self::Key<'_>, b: &Self::Key<'_>) -> bool;
}

/// Maps the key of each row in a table to the row's 1-based ID. See the module docs.
pub(crate) struct Index<T: Row> {
    table: HashTable<u32>,
    hasher: DefaultHashBuilder,
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
fn row<T>(rows: &[T], id: u32) -> &T {
    &rows[id as usize - 1]
}

impl<T: Row> Index<T> {
    pub(crate) fn new() -> Self {
        Self {
            table: HashTable::new(),
            hasher: DefaultHashBuilder::default(),
            row: PhantomData,
        }
    }

    /// Indexes `rows`, which already hold IDs 1, 2, ... in order and have distinct keys, as an
    /// interned table's rows do.
    pub(crate) fn from_rows(rows: &[T]) -> Self {
        let mut index = Self::new();
        let hasher = &index.hasher;
        index
            .table
            .reserve(rows.len(), |&id| hasher.hash_one(row(rows, id).key()));
        for (i, r) in rows.iter().enumerate() {
            index.table.insert_unique(hasher.hash_one(r.key()), i as u32 + 1, |&id| {
                hasher.hash_one(row(rows, id).key())
            });
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
}
