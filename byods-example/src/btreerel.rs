/*
Example of using ascent's BYODS infrastructure to change the underlying data
structure from a HashMap to a BTree. Currently only handles the serial case,
I *believe* it will fail if you try to run it with `par`.
*/

// ---------------------------------------------------------------------------
// Macros

// By default, rel is a Vec, rel_ind_common is unit, and rel_codegen is a no-op.
pub use ascent::rel;
pub use ascent::rel_codegen;
pub use ascent::rel_ind_common;

#[doc(hidden)]
#[macro_export]
macro_rules! btreerel_full_ind {
    ($name: ident, $field_types: ty, $indices: expr, ser, (), $key: ty, $val: ty) => {
        $crate::btreerel::BTreeFullIndexType<$key, $val>
    };
}
pub use btreerel_full_ind as rel_full_ind;

#[doc(hidden)]
#[macro_export]
macro_rules! btreerel_ind {
   ($name: ident, $field_types: ty, $indices: expr, ser, (), $ind: expr, $key: ty, $val: ty) => {
      $crate::btreerel::BTreeToRelIndexType<$key, $val>
   };
}
pub use btreerel_ind as rel_ind;

// ---------------------------------------------------------------------------
// BTree Index Types

use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;
use std::iter::Iterator;

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};

// Original type from ascent: `hashbrown::HashMap<K, V, BuildHasherDefault<FxHasher>>`
#[derive(Default, Debug)]
pub struct BTreeFullIndexType<K, V>(BTreeMap<K, V>);

// Original type from ascent: `HashMap<K, Vec<V>, BuildHasherDefault<FxHasher>>`
#[derive(Default, Debug)]
pub struct BTreeToRelIndexType<K, V>(BTreeMap<K, HashSet<V>>);

// ---------------------------------------------------------------------------
// Trait Implementations

use std::fmt::Debug;

impl<K: Ord + Clone + Debug, V: Eq + Hash + Debug> RelIndexMerge for BTreeToRelIndexType<K, V> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        if from.0.len() > to.0.len() {
            std::mem::swap(from, to);
        }
        let keys: Vec<K> = from.0.keys().cloned().collect();
        for key in keys {
            let mut from_vals = from.0.remove(&key).unwrap_or_default();
            match to.0.entry(key) {
                std::collections::btree_map::Entry::Occupied(mut vals) => {
                    vals.get_mut().extend(from_vals.drain())
                }
                std::collections::btree_map::Entry::Vacant(vacant) => {
                    let mut vals = HashSet::new();
                    vals.extend(from_vals.drain());
                    vacant.insert(vals);
                }
            }
        }
    }
}

impl<K: Eq + Hash + Ord + Debug, V: Debug> RelIndexMerge for BTreeFullIndexType<K, V> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        if from.0.len() > to.0.len() {
            std::mem::swap(from, to);
        }
        while let Some((key, val)) = from.0.pop_first() {
            to.0.insert(key, val);
        }
    }
}

impl<'a, K: Ord, V: 'a> RelIndexRead<'a> for BTreeToRelIndexType<K, V> {
    type Key = K;
    type Value = &'a V;
    type IteratorType = std::collections::hash_set::Iter<'a, V>;

    #[inline(always)]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        let BTreeToRelIndexType(btree) = self;
        btree.get(key).map(|v| v.iter())
    }

    #[inline(always)]
    fn len_estimate(&self) -> usize {
        let BTreeToRelIndexType(btree) = self;
        btree.len()
    }

    #[inline(always)]
    fn is_empty(&'a self) -> bool {
        let BTreeToRelIndexType(btree) = self;
        btree.is_empty()
    }
}

impl<'a, K: Ord + 'a, V: 'a> RelIndexReadAll<'a> for BTreeToRelIndexType<K, V> {
    type Key = &'a K;
    type Value = &'a V;
    type ValueIteratorType = std::collections::hash_set::Iter<'a, V>;
    type AllIteratorType = std::iter::Map<
        std::collections::btree_map::Iter<'a, K, HashSet<V>>,
        for<'aa, 'bb> fn(
            (&'aa K, &'bb std::collections::HashSet<V>),
        ) -> (&'aa K, std::collections::hash_set::Iter<'bb, V>),
    >;

    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        let res: Self::AllIteratorType = self.0.iter().map(|(k, v)| (k, v.iter()));
        res
    }
}

impl<K: Hash + Eq + Ord, V> RelFullIndexRead<'_> for BTreeFullIndexType<K, V> {
    type Key = K;

    fn contains_key(&self, key: &Self::Key) -> bool {
        self.0.contains_key(key)
    }
}

impl<K: Ord + Debug, V: Eq + Hash + Debug> RelIndexWrite for BTreeToRelIndexType<K, V> {
    type Key = K;
    type Value = V;

    fn index_insert(&mut self, key: K, value: V) {
        match self.0.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut vals) => {
                vals.get_mut().insert(value);
            }
            std::collections::btree_map::Entry::Vacant(vacant) => {
                let mut vals = HashSet::new();
                vals.insert(value);
                vacant.insert(vals);
            }
        }
    }
}

impl<K: Eq + Hash + Ord + Debug, V: Debug> RelIndexWrite for BTreeFullIndexType<K, V> {
    type Key = K;
    type Value = V;

    #[inline(always)]
    fn index_insert(&mut self, key: Self::Key, value: V) {
        self.0.insert(key, value);
    }
}

impl<K: Clone + Hash + Eq + Ord + Debug, V: Debug> RelFullIndexWrite for BTreeFullIndexType<K, V> {
    type Key = K;
    type Value = V;
    #[inline]
    fn insert_if_not_present(&mut self, key: &K, v: V) -> bool {
        match self.0.entry(key.clone()) {
            std::collections::btree_map::Entry::Occupied(_) => false,
            std::collections::btree_map::Entry::Vacant(vacant) => {
                vacant.insert(v);
                true
            }
        }
    }
}

impl<K, V, Rel> ToRelIndex<Rel> for BTreeToRelIndexType<K, V> {
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

impl<K, V, Rel> ToRelIndex<Rel> for BTreeFullIndexType<K, V> {
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
