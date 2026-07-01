// ---------------------------------------------------------------------------
// Macros

pub use ascent::rel;
pub use ascent::rel_codegen;
pub use ascent::rel_ind_common;

#[doc(hidden)]
#[macro_export]
macro_rules! lmdbrel_full_ind {
    ($name: ident, $field_types: ty, $indices: expr, ser, (), $key: ty, $val: ty) => {
        $crate::lmdbrel::LmdbIndexType<$key, $val>
    };
}

pub use lmdbrel_full_ind as rel_full_ind;

#[doc(hidden)]
#[macro_export]
macro_rules! lmdbrel_ind {
    ($name: ident, $field_types: ty, $indices: expr, ser, (), $ind: expr, $key: ty, $val: ty) => {
        $crate::lmdbrel::LmdbIndexType<$key, $val>
    };
}
pub use lmdbrel_ind as rel_ind;

// ---------------------------------------------------------------------------
// LMDB Index Type

use crate::LmdbWrapper;
use lmdb::{Error, RoCursor, RwTransaction, WriteFlags};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);
const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

#[derive(Clone)]
pub struct LmdbIndexType<K, V> {
    /// We will be using one database to store multiple indexes. We distinguish
    /// between the indexes using an 'ident'.
    ident: u64,
    ident_serialized: Vec<u8>,
    lmdb: Arc<LmdbWrapper>,
    // the database maps keys of type K to (possibly multiple) values of type V;
    // keys and values must be serialized to &[u8] to be stored in the database
    _unused_1: PhantomData<K>,
    _unused_2: PhantomData<V>,
}

impl<K, V> Default for LmdbIndexType<K, V> {
    fn default() -> Self {
        let ident = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            ident,
            ident_serialized: bincode::encode_to_vec(ident, BINCODE_CONFIG)
                .expect("failed to serialize key"),
            lmdb: crate::LMDB_ROOT.clone(),
            _unused_1: PhantomData,
            _unused_2: PhantomData,
        }
    }
}

#[derive(Clone)]
pub struct LmdbIndexTypeValueIterator<'a, K, V> {
    index: &'a LmdbIndexType<K, V>,
    key_u8: Vec<u8>,
    last_value_index: usize,
}

impl<'a, K, V: Decode<()>> Iterator for LmdbIndexTypeValueIterator<'a, K, V> {
    type Item = V;

    fn next(&mut self) -> Option<Self::Item> {
        let ro_txn: RoTransaction = self
            .index
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let mut ro_cursor = ro_txn
            .open_ro_cursor(self.index.lmdb.db)
            .expect("failed to open ro cursor");
        match ro_cursor
            .iter_dup_of(&self.key_u8)
            .nth(self.last_value_index)
        {
            None => None,
            Some(elem) => {
                let (_, value_u8) = elem.expect("failed to get element from database");
                self.last_value_index += 1;
                if value_u8.is_empty() {
                    None
                } else {
                    Some(
                        bincode::decode_from_slice::<V, _>(value_u8, BINCODE_CONFIG)
                            .expect("failed to deserialize value")
                            .0,
                    )
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct LmdbIndexTypeIterator<'a, K, V> {
    index: &'a LmdbIndexType<K, V>,
    last_key_u8: Option<Vec<u8>>,
}

impl<'a, K: Decode<()>, V: Decode<()>> Iterator for LmdbIndexTypeIterator<'a, K, V> {
    type Item = (K, LmdbIndexTypeValueIterator<'a, K, V>);

    fn next(&mut self) -> Option<Self::Item> {
        let ro_txn: RoTransaction = self
            .index
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let mut ro_cursor = ro_txn
            .open_ro_cursor(self.index.lmdb.db)
            .expect("failed to open ro cursor");

        let opt_it = match &self.last_key_u8 {
            None => ro_cursor.iter_dup_from(&self.index.ident_serialized).next(),
            Some(last_key_u8) => ro_cursor.iter_dup_from(last_key_u8).nth(1),
        };

        if let Some(mut it) = opt_it {
            let elem = it
                .next()
                .expect("expected iter_dup to return a non-empty iterator");
            let key_u8 = elem.expect("failed to get element from database").0;
            if !key_u8.starts_with(&self.index.ident_serialized) {
                return None;
            }
            let key = bincode::decode_from_slice::<(u64, K), _>(key_u8, BINCODE_CONFIG)
                .expect("failed to deserialize value")
                .0
                .1;
            self.last_key_u8 = Some(key_u8.to_vec());
            Some((
                key,
                LmdbIndexTypeValueIterator {
                    index: self.index,
                    key_u8: key_u8.to_vec(),
                    last_value_index: 0,
                },
            ))
        } else {
            None
        }
    }
}

impl<K, V> LmdbIndexType<K, V> {
    /// Iterate over the values for a given key
    fn iter_values<'a>(&'a self, key_u8: Vec<u8>) -> LmdbIndexTypeValueIterator<'a, K, V> {
        LmdbIndexTypeValueIterator {
            index: self,
            key_u8,
            last_value_index: 0,
        }
    }

    /// Iterate over all elements in the database
    fn iter<'a>(&'a self) -> LmdbIndexTypeIterator<'a, K, V> {
        LmdbIndexTypeIterator {
            index: self,
            last_key_u8: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Trait Implementations

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use bincode::{Decode, Encode};
use lmdb::{Cursor, RoTransaction, Transaction};
use std::fmt::Debug;

impl<K: Encode + Decode<()>, V> RelIndexMerge for LmdbIndexType<K, V> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        // Copy new entries
        let mut to_rw_txn: RwTransaction = to
            .lmdb
            .env
            .begin_rw_txn()
            .expect("failed to begin rw transaction");
        let from_ro_txn: RoTransaction = from
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let mut from_ro_cursor: RoCursor = from_ro_txn
            .open_ro_cursor(from.lmdb.db)
            .expect("failed to open ro cursor");
        let mut added_keys = Vec::new();
        'outer: for it in from_ro_cursor.iter_dup_from(&from.ident_serialized) {
            let mut new_key_u8 = Vec::<u8>::new();
            for elem in it {
                let (key_u8, value_u8) = elem.expect("failed to get element from database");
                if new_key_u8.is_empty() {
                    if !key_u8.starts_with(&from.ident_serialized) {
                        // we've stepped past the end of the current index, so quit
                        break 'outer;
                    }
                    // Note: within a given 'it' the key is fixed, so we only
                    // need to run this code once
                    added_keys.push(key_u8);
                    let (_, key) =
                        bincode::decode_from_slice::<(u64, K), _>(key_u8, BINCODE_CONFIG)
                            .expect("failed to deserialize key")
                            .0;
                    new_key_u8 = bincode::encode_to_vec((to.ident, key), BINCODE_CONFIG)
                        .expect("failed to serialize key");
                }
                to_rw_txn
                    .put(to.lmdb.db, &new_key_u8, &value_u8, WriteFlags::empty())
                    .expect("failed to write to database");
            }
        }
        to_rw_txn.commit().expect("failed to commit rw transaction");
        drop(from_ro_cursor);
        drop(from_ro_txn);

        // Delete old entries
        let mut from_rw_txn: RwTransaction = from
            .lmdb
            .env
            .begin_rw_txn()
            .expect("failed to begin rw transaction");
        added_keys.iter().for_each(|key_u8| {
            from_rw_txn
                .del(from.lmdb.db, key_u8, None)
                .expect("failed to delete key from database")
        });
        from_rw_txn
            .commit()
            .expect("failed to commit rw transaction");
    }
}

impl<'a, K: 'a + Clone + Encode + Decode<()> + Debug, V: 'a + Clone + Decode<()>> RelIndexRead<'a>
    for LmdbIndexType<K, V>
{
    type Key = K;
    type Value = V;
    type IteratorType = LmdbIndexTypeValueIterator<'a, K, V>;

    #[inline(always)]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        let ro_txn: RoTransaction = self
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let key_u8 = bincode::encode_to_vec((self.ident, key), BINCODE_CONFIG)
            .expect("failed to serialize key");
        let contains_key = match ro_txn.get(self.lmdb.db, &key_u8) {
            Ok(_) => true,
            Err(Error::NotFound) => false,
            Err(e) => panic!("failed to lookup key {key:?}: {e:?}"),
        };
        drop(ro_txn);
        if contains_key {
            Some(self.iter_values(key_u8))
        } else {
            None
        }
    }

    #[inline(always)]
    fn len_estimate(&self) -> usize {
        self.iter().count()
    }

    #[inline(always)]
    fn is_empty(&'a self) -> bool {
        self.iter().count() == 0
    }
}

impl<'a, K: Ord + 'a + Decode<()>, V: 'a + Decode<()>> RelIndexReadAll<'a> for LmdbIndexType<K, V> {
    type Key = K;
    type Value = V;
    type ValueIteratorType = LmdbIndexTypeValueIterator<'a, K, V>;
    type AllIteratorType = LmdbIndexTypeIterator<'a, K, V>;

    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        self.iter()
    }
}

impl<K: Ord + Encode, V: Encode> RelIndexWrite for LmdbIndexType<K, V> {
    type Key = K;
    type Value = V;

    fn index_insert(&mut self, key: K, value: V) {
        let mut rw_txn: RwTransaction = self
            .lmdb
            .env
            .begin_rw_txn()
            .expect("failed to begin rw transaction");
        let key_u8 = bincode::encode_to_vec((self.ident, &key), BINCODE_CONFIG)
            .expect("failed to serialize key");
        let value_u8 =
            bincode::encode_to_vec(&value, BINCODE_CONFIG).expect("failed to serialize value");
        rw_txn
            .put(self.lmdb.db, &key_u8, &value_u8, WriteFlags::empty())
            .expect("failed to write to database");
        rw_txn.commit().expect("failed to commit rw transaction");
    }
}

impl<'a, K: Debug + Encode, V> RelFullIndexRead<'a> for LmdbIndexType<K, V> {
    type Key = K;

    #[inline(always)]
    fn contains_key(&'a self, key: &Self::Key) -> bool {
        let ro_txn: RoTransaction = self
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let key_u8 = bincode::encode_to_vec((self.ident, &key), BINCODE_CONFIG)
            .expect("failed to serialize key");
        match ro_txn.get(self.lmdb.db, &key_u8) {
            Ok(_) => true,
            Err(Error::NotFound) => false,
            Err(e) => panic!("failed to lookup key {key:?}: {e:?}"),
        }
    }
}

impl<K: Clone + Debug + Encode, V: Encode> RelFullIndexWrite for LmdbIndexType<K, V> {
    type Key = K;
    type Value = V;

    fn insert_if_not_present(&mut self, key: &Self::Key, value: Self::Value) -> bool {
        let mut rw_txn: RwTransaction = self
            .lmdb
            .env
            .begin_rw_txn()
            .expect("failed to begin rw transaction");
        let key_u8 = bincode::encode_to_vec((self.ident, &key), BINCODE_CONFIG)
            .expect("failed to serialize key");
        match rw_txn.get(self.lmdb.db, &key_u8) {
            Ok(_) => false,
            Err(Error::NotFound) => {
                let value_u8 = bincode::encode_to_vec(value, BINCODE_CONFIG)
                    .expect("failed to serialize value");
                rw_txn
                    .put(self.lmdb.db, &key_u8, &value_u8, WriteFlags::empty())
                    .expect("failed to write to database");
                rw_txn.commit().expect("failed to commit rw transaction");
                true
            }
            Err(e) => panic!("failed to lookup key {key:?}: {e:?}"),
        }
    }
}

impl<K, V, Rel> ToRelIndex<Rel> for LmdbIndexType<K, V> {
    type RelIndex<'a>
        = &'a Self
    where
        Self: 'a,
        Rel: 'a;

    #[inline(always)]
    fn to_rel_index<'a>(&'a self, _rel: &'a Rel) -> Self::RelIndex<'a> {
        self
    }

    type RelIndexWrite<'a>
        = &'a mut Self
    where
        Self: 'a,
        Rel: 'a;

    #[inline(always)]
    fn to_rel_index_write<'a>(&'a mut self, _rel: &'a mut Rel) -> Self::RelIndexWrite<'a> {
        self
    }
}

// ---------------------------------------------------------------------------
// Extra (unused) code

// Database printer for debugging
impl<K: Debug + Decode<()>, V: Debug + Decode<()>> LmdbIndexType<K, V> {
    pub fn print_contents(&self) {
        println!("Printing contents of table {:?}", self.ident);
        let ro_txn: RoTransaction = self
            .lmdb
            .env
            .begin_ro_txn()
            .expect("failed to begin ro transaction");
        let mut ro_cursor: RoCursor = ro_txn
            .open_ro_cursor(self.lmdb.db)
            .expect("failed to open ro cursor");
        'outer: for it in ro_cursor.iter_dup_from(&self.ident_serialized) {
            let mut key: Option<K> = None;
            for elem in it {
                let (key_u8, value_u8) = elem.expect("failed to get element from database");
                if key.is_none() {
                    if !key_u8.starts_with(&self.ident_serialized) {
                        break 'outer;
                    }
                    let inner_key =
                        bincode::decode_from_slice::<(u64, K), _>(key_u8, BINCODE_CONFIG)
                            .expect("failed to deserialize key")
                            .0
                            .1;
                    println!("K: {:?}", inner_key);
                    key = Some(inner_key);
                }
                let value = bincode::decode_from_slice::<V, _>(value_u8, BINCODE_CONFIG)
                    .expect("failed to deserialize value")
                    .0;
                println!("\tV: {:?}", value);
            }
        }
    }
}
