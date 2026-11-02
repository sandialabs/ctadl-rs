//! A BYODS store for a relation of five columns and a [`Cond`], kept as a lattice on the
//! condition: `locals`, `assign_like`, `edge_split` and `ext_dst`.
//!
//! Ascent refuses a custom store on a `lattice`, so these are declared as plain `relation`s and
//! the store keeps the lattice semantics itself. A row is a fact `k` (the five columns) and its
//! condition `c`, and there is at most one row per fact, holding the join of every condition it
//! was derived under:
//!
//! - **Existence is coverage.** Ascent adds a row only if neither `total` nor `delta`
//!   `contains_key` it. Here that means the condition already held for `k` covers `c` (joining
//!   `c` would change nothing), so a row derived under a condition it already holds under is
//!   dropped exactly as a duplicate is.
//! - **Insertion is a join.** `insert_if_not_present` on `new` joins `c` into what `new` holds for
//!   `k`, and reports whether that grew.
//! - **The merge joins across versions.** [`CondTrie`]'s `merge_delta_to_total_new_to_delta` joins
//!   `delta` into `total`, then joins each conditional row of `new` with what `total` now holds for
//!   its fact, dropping the rows `total` already covers. So `delta` carries each changed fact's
//!   joined condition, not just the part that is new, which matters where a join widens (to ⊤
//!   under `Collapse`, or to `Always` under `Spill`).
//!
//! One difference from an Ascent lattice: Ascent joins into the row of `total` in place, while
//! here `total` keeps a fact's old condition until the merge. Within one iteration a rule can
//! therefore also derive from the old condition. Whatever it derives is covered by what the new
//! condition derives, so the fixpoint is the same.
//!
//! ## Layout
//!
//! Nearly every row is `Always`. Those live in a [`LocalsIndCommon`] of the five columns, the
//! trie the relations had before they carried a condition, unchanged, and the condition is not
//! stored. The conditional rows live beside it in a map keyed the same way, `(K1, K2)` then `P`,
//! with the condition per leaf. A fact is in at most one of the two after a merge; between merges
//! `new` can hold a fact in both, and the merge resolves it.
//!
//! The store is generic over the five columns in its own order, `(K1, K2, P, A, B)`: the group
//! key, the probed path, and the rest. `locals` and `edge_split` use that order ([`locals`]);
//! `assign_like` and `ext_dst` are keyed on the edge's source, columns 0 and 3 ([`assign`]).
//!
//! The only real merge is on the ind_common, as in [`super::locals_trie`]: every index write
//! target's merge is a no-op.

use std::hash::Hash;
use std::marker::PhantomData;

use ascent::Lattice;
use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use streaming_iterator::StreamingIterator;

use super::Rows;
use super::assign_like_trie::FromRows;
use super::decision::Cond;
use super::locals_trie::{DynIter, LocalsIndCommon, NoopWrite, hb_bytes};
use super::path_group::Get;

pub(crate) type Map<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;

pub(crate) const ALWAYS: &Cond = &Cond::Always;

/// Whether a fact held under `held` already holds under `c`: joining `c` changes nothing.
#[inline]
pub fn covers(held: Cond, c: Cond) -> bool {
    match (held, c) {
        (Cond::Always, _) => true,
        (_, Cond::Always) => false,
        (mut held, c) => !held.join_mut(c),
    }
}

/// The conditional leaves of one group, by path.
pub(crate) type CondLeaves<P, A, B> = Map<P, Map<(A, B), Cond>>;

/// The shared store. See the module docs.
pub struct CondTrie<K1, K2, P, A, B>
where
    K1: Clone + Eq + Hash,
    K2: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    always: LocalsIndCommon<K1, K2, P, A, B>,
    cond: Map<(K1, K2), CondLeaves<P, A, B>>,
    cond_len: usize,
}

impl<K1, K2, P, A, B> Default for CondTrie<K1, K2, P, A, B>
where
    K1: Clone + Eq + Hash,
    K2: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            always: LocalsIndCommon::default(),
            cond: Map::default(),
            cond_len: 0,
        }
    }
}

impl<K1, K2, P, A, B> CondTrie<K1, K2, P, A, B>
where
    K1: Clone + Eq + Hash,
    K2: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    /// Number of facts. Exact after a merge; `new` can count a fact twice before one.
    #[inline]
    pub fn len(&self) -> usize {
        self.always.len() + self.cond_len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `Always` rows, without their condition.
    #[inline]
    pub fn always(&self) -> &LocalsIndCommon<K1, K2, P, A, B> {
        &self.always
    }

    /// Number of conditional rows.
    #[inline]
    pub fn conditional_len(&self) -> usize {
        self.cond_len
    }

    /// The condition a fact holds under here, if any.
    fn cond_at(&self, k1: &K1, k2: &K2, p: &P, a: &A, b: &B) -> Option<Cond> {
        if self.always.contains(k1, k2, p, a, b) {
            return Some(Cond::Always);
        }
        if self.cond.is_empty() {
            return None;
        }
        self.cond
            .get(&(k1.clone(), k2.clone()))?
            .get(p)?
            .get(&(a.clone(), b.clone()))
            .copied()
    }

    /// Joins `c` into the condition of the fact. Returns true if this store's condition for it
    /// grew.
    pub(crate) fn insert(&mut self, k1: &K1, k2: &K2, p: &P, a: &A, b: &B, c: Cond) -> bool {
        if c.is_always() {
            // A conditional row for the fact, if any, is dropped at the next merge.
            return self
                .always
                .insert(&(k1.clone(), k2.clone(), p.clone(), a.clone(), b.clone()));
        }
        if self.always.contains(k1, k2, p, a, b) {
            return false;
        }
        let leaves = self
            .cond
            .entry((k1.clone(), k2.clone()))
            .or_default()
            .entry(p.clone())
            .or_default();
        let joined = match leaves.entry((a.clone(), b.clone())) {
            hashbrown::hash_map::Entry::Occupied(mut e) => {
                if !e.get_mut().join_mut(c) {
                    return false;
                }
                *e.get()
            }
            hashbrown::hash_map::Entry::Vacant(e) => {
                e.insert(c);
                self.cond_len += 1;
                return true;
            }
        };
        if joined.is_always() {
            // A join that spills to `Always` moves the fact into the trie.
            self.remove_cond(k1, k2, p, a, b);
            self.always
                .insert(&(k1.clone(), k2.clone(), p.clone(), a.clone(), b.clone()));
        }
        true
    }

    fn remove_cond(&mut self, k1: &K1, k2: &K2, p: &P, a: &A, b: &B) {
        let g = (k1.clone(), k2.clone());
        let Some(leaves) = self.cond.get_mut(&g) else {
            return;
        };
        let Some(abs) = leaves.get_mut(p) else {
            return;
        };
        if abs.remove(&(a.clone(), b.clone())).is_some() {
            self.cond_len -= 1;
        }
        if abs.is_empty() {
            leaves.remove(p);
            if leaves.is_empty() {
                self.cond.remove(&g);
            }
        }
    }

    /// Joins `from` into `self`, emptying `from`. The delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        // The conditional rows that `from` makes unconditional.
        if !self.cond.is_empty() && !from.always.is_empty() {
            let mut removed = 0;
            let fwd = &from.always.fwd;
            self.cond.retain(|g, leaves| {
                if let Some(group) = fwd.get(g) {
                    leaves.retain(|p, abs| {
                        abs.retain(|(a, b), _| {
                            let gone = group.contains(p, a, b);
                            removed += usize::from(gone);
                            !gone
                        });
                        !abs.is_empty()
                    });
                }
                !leaves.is_empty()
            });
            self.cond_len -= removed;
        }
        self.always.absorb(&mut from.always);
        for ((k1, k2), leaves) in from.cond.drain() {
            for (p, abs) in leaves {
                for ((a, b), c) in abs {
                    self.insert(&k1, &k2, &p, &a, &b, c);
                }
            }
        }
        from.cond_len = 0;
    }

    /// Makes `self`, the rows found this iteration, the next `delta` against `total`, which has
    /// already absorbed the old `delta`: a conditional row `total` covers is dropped, and the
    /// rest carry their join with what `total` holds.
    fn refine(&mut self, total: &Self) {
        if self.cond.is_empty() {
            return;
        }
        let mut removed = 0;
        let mut spilled = Vec::new();
        let always = &self.always;
        self.cond.retain(|(k1, k2), leaves| {
            leaves.retain(|p, abs| {
                abs.retain(|(a, b), c| {
                    let held = if always.contains(k1, k2, p, a, b) {
                        Some(Cond::Always)
                    } else {
                        total.cond_at(k1, k2, p, a, b)
                    };
                    match held {
                        Some(held) if covers(held, *c) => {
                            removed += 1;
                            false
                        }
                        Some(held) => {
                            c.join_mut(held);
                            if c.is_always() {
                                spilled.push((
                                    k1.clone(),
                                    k2.clone(),
                                    p.clone(),
                                    a.clone(),
                                    b.clone(),
                                ));
                                removed += 1;
                                false
                            } else {
                                true
                            }
                        }
                        None => true,
                    }
                });
                !abs.is_empty()
            });
            !leaves.is_empty()
        });
        self.cond_len -= removed;
        for row in spilled {
            self.always.insert(&row);
        }
    }

    /// Every row, in the store's column order.
    #[allow(clippy::type_complexity)]
    pub fn rows(&self) -> impl Iterator<Item = (&K1, &K2, &P, &A, &B, &Cond)> + Clone + '_ {
        let always = self.always.fwd.iter().flat_map(|((k1, k2), group)| {
            group.iter().map(move |(p, a, b)| (k1, k2, p, a, b, ALWAYS))
        });
        let cond = self.cond.iter().flat_map(|((k1, k2), leaves)| {
            leaves
                .iter()
                .flat_map(move |(p, abs)| abs.iter().map(move |((a, b), c)| (k1, k2, p, a, b, c)))
        });
        always.chain(cond)
    }

    /// Heap bytes of the conditional rows, load-factor slack included. The `Always` rows'
    /// are [`LocalsIndCommon::heap_report`]'s.
    pub fn cond_heap_bytes(&self) -> usize {
        let leaves_sz = std::mem::size_of::<((K1, K2), CondLeaves<P, A, B>)>();
        let abs_sz = std::mem::size_of::<(P, Map<(A, B), Cond>)>();
        let leaf_sz = std::mem::size_of::<((A, B), Cond)>();
        let mut bytes = hb_bytes(self.cond.capacity(), leaves_sz);
        for leaves in self.cond.values() {
            bytes += hb_bytes(leaves.capacity(), abs_sz);
            for abs in leaves.values() {
                bytes += hb_bytes(abs.capacity(), leaf_sz);
            }
        }
        bytes
    }

    /// The rows of group `(k1, k2)`, as `(&P, &A, &B, &Cond)`; `None` if it has none.
    fn group(&self, key: &(K1, K2)) -> Option<DynIter<'_, (&P, &A, &B, &Cond)>> {
        let always = self.always.fwd.get(key);
        let cond = if self.cond.is_empty() {
            None
        } else {
            self.cond.get(key)
        };
        if always.is_none() && cond.is_none() {
            return None;
        }
        Some(DynIter::new(move || {
            always
                .into_iter()
                .flat_map(|g| g.iter().map(|(p, a, b)| (p, a, b, ALWAYS)))
                .chain(cond.into_iter().flat_map(|leaves| {
                    leaves
                        .iter()
                        .flat_map(|(p, abs)| abs.iter().map(move |((a, b), c)| (p, a, b, c)))
                }))
        }))
    }

    /// The rows at `(k1, k2, p)`; `None` if it has none, which cuts the caller's join.
    #[inline]
    fn get(&self, key: &(K1, K2, P)) -> Option<CondGet<'_, P, A, B>> {
        let g = (key.0.clone(), key.1.clone());
        let always = self.always.fwd.get(&g).and_then(|group| group.get(&key.2));
        let cond = if self.cond.is_empty() {
            None
        } else {
            self.cond.get(&g).and_then(|leaves| leaves.get(&key.2))
        };
        if always.is_none() && cond.is_none() {
            return None;
        }
        Some(CondGet {
            always,
            cond: cond.map(|abs| abs.iter()),
        })
    }

    /// Every `(k1, k2, p)` with its rows, for a planner that drives a join off a probe index.
    /// Not a hot path.
    #[allow(clippy::type_complexity)]
    fn iter_by_path(&self) -> Vec<((&K1, &K2, &P), Vec<(&A, &B, &Cond)>)> {
        let mut by: Map<(&K1, &K2, &P), Vec<(&A, &B, &Cond)>> = Map::default();
        for (k1, k2, p, a, b, c) in self.rows() {
            by.entry((k1, k2, p)).or_default().push((a, b, c));
        }
        by.into_iter().collect()
    }
}

impl<K1, K2, P, A, B> RelIndexMerge for CondTrie<K1, K2, P, A, B>
where
    K1: Clone + Eq + Hash,
    K2: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.absorb(from);
    }

    fn merge_delta_to_total_new_to_delta(new: &mut Self, delta: &mut Self, total: &mut Self) {
        total.absorb(delta);
        new.refine(total);
        std::mem::swap(new, delta);
    }
}

/// The rows at one `(k1, k2, p)`: the `Always` ones, then the conditional ones.
pub struct CondGet<'a, P, A, B> {
    pub(crate) always: Option<Get<'a, P, A, B>>,
    pub(crate) cond: Option<hashbrown::hash_map::Iter<'a, (A, B), Cond>>,
}

impl<P, A, B> Clone for CondGet<'_, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        Self {
            always: self.always.clone(),
            cond: self.cond.clone(),
        }
    }
}

impl<'a, P, A, B> Iterator for CondGet<'a, P, A, B>
where
    P: Clone + Eq + Hash,
    A: Clone + Eq + Hash,
    B: Clone + Eq + Hash,
{
    type Item = (&'a A, &'a B, &'a Cond);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(it) = &mut self.always {
            if let Some((a, b)) = it.next() {
                return Some((a, b, ALWAYS));
            }
            self.always = None;
        }
        self.cond.as_mut()?.next().map(|((a, b), c)| (a, b, c))
    }
}

/// The bound every column of the store meets.
pub trait Col: Clone + Eq + Hash {}
impl<T: Clone + Eq + Hash> Col for T {}

/// The write target of the full index: the one place rows enter the store. Its merge is a
/// no-op; the ind_common merges.
pub struct FullWrite<'a, S, Ix>(&'a mut S, PhantomData<Ix>);

impl<S, Ix> RelIndexMerge for FullWrite<'_, S, Ix> {
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

/// A read view over the store. Each column order's index markers hand one out; the index it
/// serves is the marker's.
pub struct View<'a, S, Ix>(&'a S, PhantomData<Ix>);

/// Defines a zero-sized `ToRelIndex` marker `$to` for the store type `$store`, whose view is
/// `View<_, $to>`; the full index's marker writes through [`FullWrite`].
macro_rules! marker {
    ($to:ident, $store:ty, $wrty:ty, $rel:ident => $wrbody:expr) => {
        pub struct $to<T0, T1, T2, T3, T4>(PhantomData<(T0, T1, T2, T3, T4)>);
        impl<T0, T1, T2, T3, T4> Default for $to<T0, T1, T2, T3, T4> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> ToRelIndex<$store>
            for $to<T0, T1, T2, T3, T4>
        {
            type RelIndex<'a>
                = View<'a, $store, $to<T0, T1, T2, T3, T4>>
            where
                Self: 'a,
                $store: 'a;
            #[inline]
            fn to_rel_index<'a>(&'a self, rel: &'a $store) -> Self::RelIndex<'a> {
                View(rel, PhantomData)
            }
            type RelIndexWrite<'a>
                = $wrty
            where
                Self: 'a,
                $store: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                $rel: &'a mut $store,
            ) -> Self::RelIndexWrite<'a> {
                $wrbody
            }
        }
    };
}

/// `locals(F, V, P, M, Fp, Cond)` and `edge_split`: the store's own column order, grouped on
/// columns 0 and 1 and probed on 2. Indices `none`, `0_1`, `0_1_2` and the full index.
pub mod locals {
    use super::*;

    pub use crate::cond_trie_locals_rel_full_ind as rel_full_ind;
    pub use crate::cond_trie_locals_rel_ind as rel_ind;
    pub use crate::cond_trie_locals_rel_ind_common as rel_ind_common;
    pub use crate::cond_trie_rel as rel;
    pub use crate::cond_trie_rel_codegen as rel_codegen;

    pub type Store<T0, T1, T2, T3, T4> = CondTrie<T0, T1, T2, T3, T4>;
    type Row<T0, T1, T2, T3, T4> = (T0, T1, T2, T3, T4, Cond);

    marker!(ToNone, Store<T0, T1, T2, T3, T4>, NoopWrite<(), Row<T0, T1, T2, T3, T4>>, _rel => NoopWrite::default());
    marker!(To01, Store<T0, T1, T2, T3, T4>, NoopWrite<(T0, T1), (T2, T3, T4, Cond)>, _rel => NoopWrite::default());
    marker!(To012, Store<T0, T1, T2, T3, T4>, NoopWrite<(T0, T1, T2), (T3, T4, Cond)>, _rel => NoopWrite::default());
    marker!(ToFull, Store<T0, T1, T2, T3, T4>, FullWrite<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>, rel => FullWrite(rel, PhantomData));

    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelFullIndexWrite
        for FullWrite<'_, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = ();
        #[inline]
        fn insert_if_not_present(&mut self, r: &Self::Key, _v: ()) -> bool {
            self.0.insert(&r.0, &r.1, &r.2, &r.3, &r.4, r.5)
        }
    }
    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexWrite
        for FullWrite<'_, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = ();
        #[inline]
        fn index_insert(&mut self, r: Self::Key, _v: ()) {
            self.0.insert(&r.0, &r.1, &r.2, &r.3, &r.4, r.5);
        }
    }

    // ---- none: every row ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToNone<T0, T1, T2, T3, T4>>
    {
        type Key = ();
        type Value = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type IteratorType = DynIter<'a, Self::Value>;
        #[inline]
        fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
            let s = self.0;
            Some(DynIter::new(move || s.rows()))
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            1
        }
        #[inline]
        fn is_empty(&'a self) -> bool {
            self.0.is_empty()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToNone<T0, T1, T2, T3, T4>>
    {
        type Key = ();
        type Value = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type ValueIteratorType = DynIter<'a, Self::Value>;
        type AllIteratorType = std::iter::Once<((), Self::ValueIteratorType)>;
        #[inline]
        fn iter_all(&'a self) -> Self::AllIteratorType {
            std::iter::once(((), RelIndexRead::index_get(self, &()).unwrap()))
        }
    }

    // ---- 0_1: (T0, T1) -> (T2, T3, T4, Cond) ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To01<T0, T1, T2, T3, T4>>
    {
        type Key = (T0, T1);
        type Value = (&'a T2, &'a T3, &'a T4, &'a Cond);
        type IteratorType = DynIter<'a, Self::Value>;
        #[inline]
        fn index_get(&'a self, key: &(T0, T1)) -> Option<Self::IteratorType> {
            self.0.group(key)
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.always.fwd.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To01<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T1);
        type Value = (&'a T2, &'a T3, &'a T4, &'a Cond);
        type ValueIteratorType = DynIter<'a, Self::Value>;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            let s = self.0;
            let keys: Map<&'a (T0, T1), ()> = s
                .always
                .fwd
                .keys()
                .chain(s.cond.keys())
                .map(|k| (k, ()))
                .collect();
            Box::new(
                keys.into_iter()
                    .map(move |(k, ())| ((&k.0, &k.1), s.group(k).unwrap())),
            )
        }
    }

    // ---- 0_1_2: (T0, T1, T2) -> (T3, T4, Cond), the exact-path probe ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To012<T0, T1, T2, T3, T4>>
    {
        type Key = (T0, T1, T2);
        type Value = (&'a T3, &'a T4, &'a Cond);
        type IteratorType = CondGet<'a, T2, T3, T4>;
        #[inline]
        fn index_get(&'a self, key: &(T0, T1, T2)) -> Option<Self::IteratorType> {
            self.0.get(key)
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.always.fwd.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To012<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T1, &'a T2);
        type Value = (&'a T3, &'a T4, &'a Cond);
        type ValueIteratorType = std::vec::IntoIter<Self::Value>;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            Box::new(
                self.0
                    .iter_by_path()
                    .into_iter()
                    .map(|(k, vs)| (k, vs.into_iter())),
            )
        }
    }

    // ---- full: coverage ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelFullIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        #[inline]
        fn contains_key(&'a self, r: &Self::Key) -> bool {
            self.0
                .cond_at(&r.0, &r.1, &r.2, &r.3, &r.4)
                .is_some_and(|held| covers(held, r.5))
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = &'a ();
        type IteratorType = std::iter::Once<&'a ()>;
        #[inline]
        fn index_get(&'a self, r: &Self::Key) -> Option<Self::IteratorType> {
            (self.0.cond_at(&r.0, &r.1, &r.2, &r.3, &r.4) == Some(r.5))
                .then(|| std::iter::once(&()))
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type Value = &'a ();
        type ValueIteratorType = std::iter::Once<&'a ()>;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            Box::new(self.0.rows().map(|r| (r, std::iter::once(&()))))
        }
    }

    /// The store's rows as [`Rows`].
    pub struct AsRows<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col>(
        pub &'a Store<T0, T1, T2, T3, T4>,
    );
    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> Rows<Row<T0, T1, T2, T3, T4>>
        for AsRows<'_, T0, T1, T2, T3, T4>
    {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn stream(&self) -> Box<dyn StreamingIterator<Item = Row<T0, T1, T2, T3, T4>> + '_> {
            Box::new(streaming_iterator::convert(self.0.rows().map(
                |(a, b, c, d, e, f)| (a.clone(), b.clone(), c.clone(), d.clone(), e.clone(), *f),
            )))
        }
    }
}

/// `assign_like(F, Vd, Pd, Vs, Ps, Cond)` and `ext_dst`: an edge `Vd.Pd = Vs.Ps`, grouped on its
/// source `(F, Vs)` and probed on `Ps`, so the store's order is `(F, Vs, Ps, Vd, Pd)`. Indices
/// `none`, `0_3`, `0_3_4` and the full index.
pub mod assign {
    use super::*;

    pub use crate::cond_trie_assign_rel_full_ind as rel_full_ind;
    pub use crate::cond_trie_assign_rel_ind as rel_ind;
    pub use crate::cond_trie_assign_rel_ind_common as rel_ind_common;
    pub use crate::cond_trie_rel as rel;
    pub use crate::cond_trie_rel_codegen as rel_codegen;

    pub type Store<T0, T1, T2, T3, T4> = CondTrie<T0, T3, T4, T1, T2>;
    type Row<T0, T1, T2, T3, T4> = (T0, T1, T2, T3, T4, Cond);

    marker!(ToNone, Store<T0, T1, T2, T3, T4>, NoopWrite<(), Row<T0, T1, T2, T3, T4>>, _rel => NoopWrite::default());
    marker!(To03, Store<T0, T1, T2, T3, T4>, NoopWrite<(T0, T3), (T1, T2, T4, Cond)>, _rel => NoopWrite::default());
    marker!(To034, Store<T0, T1, T2, T3, T4>, NoopWrite<(T0, T3, T4), (T1, T2, Cond)>, _rel => NoopWrite::default());
    marker!(ToFull, Store<T0, T1, T2, T3, T4>, FullWrite<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>, rel => FullWrite(rel, PhantomData));

    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelFullIndexWrite
        for FullWrite<'_, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = ();
        #[inline]
        fn insert_if_not_present(&mut self, r: &Self::Key, _v: ()) -> bool {
            self.0.insert(&r.0, &r.3, &r.4, &r.1, &r.2, r.5)
        }
    }
    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexWrite
        for FullWrite<'_, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = ();
        #[inline]
        fn index_insert(&mut self, r: Self::Key, _v: ()) {
            self.0.insert(&r.0, &r.3, &r.4, &r.1, &r.2, r.5);
        }
    }

    #[allow(clippy::type_complexity)]
    fn rows<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col>(
        s: &'a Store<T0, T1, T2, T3, T4>,
    ) -> impl Iterator<Item = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond)> + Clone + 'a {
        s.rows()
            .map(|(f, vs, ps, vd, pd, c)| (f, vd, pd, vs, ps, c))
    }

    // ---- none: every row ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToNone<T0, T1, T2, T3, T4>>
    {
        type Key = ();
        type Value = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type IteratorType = DynIter<'a, Self::Value>;
        #[inline]
        fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
            let s = self.0;
            Some(DynIter::new(move || rows(s)))
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            1
        }
        #[inline]
        fn is_empty(&'a self) -> bool {
            self.0.is_empty()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToNone<T0, T1, T2, T3, T4>>
    {
        type Key = ();
        type Value = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type ValueIteratorType = DynIter<'a, Self::Value>;
        type AllIteratorType = std::iter::Once<((), Self::ValueIteratorType)>;
        #[inline]
        fn iter_all(&'a self) -> Self::AllIteratorType {
            std::iter::once(((), RelIndexRead::index_get(self, &()).unwrap()))
        }
    }

    // ---- 0_3: (F, Vs) -> (Vd, Pd, Ps, Cond) ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To03<T0, T1, T2, T3, T4>>
    {
        type Key = (T0, T3);
        type Value = (&'a T1, &'a T2, &'a T4, &'a Cond);
        type IteratorType = std::iter::Map<
            DynIter<'a, (&'a T4, &'a T1, &'a T2, &'a Cond)>,
            fn((&'a T4, &'a T1, &'a T2, &'a Cond)) -> Self::Value,
        >;
        #[inline]
        fn index_get(&'a self, key: &(T0, T3)) -> Option<Self::IteratorType> {
            Some(self.0.group(key)?.map(|(ps, vd, pd, c)| (vd, pd, ps, c)))
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.always.fwd.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To03<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T3);
        type Value = (&'a T1, &'a T2, &'a T4, &'a Cond);
        type ValueIteratorType = <Self as RelIndexRead<'a>>::IteratorType;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            let s = self.0;
            let keys: Map<&'a (T0, T3), ()> = s
                .always
                .fwd
                .keys()
                .chain(s.cond.keys())
                .map(|k| (k, ()))
                .collect();
            Box::new(keys.into_iter().map(move |(k, ())| {
                let it: Self::ValueIteratorType =
                    s.group(k).unwrap().map(|(ps, vd, pd, c)| (vd, pd, ps, c));
                ((&k.0, &k.1), it)
            }))
        }
    }

    // ---- 0_3_4: (F, Vs, Ps) -> (Vd, Pd, Cond), the exact-path probe ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To034<T0, T1, T2, T3, T4>>
    {
        type Key = (T0, T3, T4);
        type Value = (&'a T1, &'a T2, &'a Cond);
        type IteratorType = CondGet<'a, T4, T1, T2>;
        #[inline]
        fn index_get(&'a self, key: &(T0, T3, T4)) -> Option<Self::IteratorType> {
            self.0.get(key)
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, To034<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T3, &'a T4);
        type Value = (&'a T1, &'a T2, &'a Cond);
        type ValueIteratorType = std::vec::IntoIter<Self::Value>;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            Box::new(
                self.0
                    .iter_by_path()
                    .into_iter()
                    .map(|(k, vs)| (k, vs.into_iter())),
            )
        }
    }

    // ---- full: coverage ----
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelFullIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        #[inline]
        fn contains_key(&'a self, r: &Self::Key) -> bool {
            self.0
                .cond_at(&r.0, &r.3, &r.4, &r.1, &r.2)
                .is_some_and(|held| covers(held, r.5))
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexRead<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = Row<T0, T1, T2, T3, T4>;
        type Value = &'a ();
        type IteratorType = std::iter::Once<&'a ()>;
        #[inline]
        fn index_get(&'a self, r: &Self::Key) -> Option<Self::IteratorType> {
            (self.0.cond_at(&r.0, &r.3, &r.4, &r.1, &r.2) == Some(r.5))
                .then(|| std::iter::once(&()))
        }
        #[inline]
        fn len_estimate(&self) -> usize {
            self.0.len()
        }
    }
    impl<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> RelIndexReadAll<'a>
        for View<'a, Store<T0, T1, T2, T3, T4>, ToFull<T0, T1, T2, T3, T4>>
    {
        type Key = (&'a T0, &'a T1, &'a T2, &'a T3, &'a T4, &'a Cond);
        type Value = &'a ();
        type ValueIteratorType = std::iter::Once<&'a ()>;
        type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
        fn iter_all(&'a self) -> Self::AllIteratorType {
            Box::new(rows(self.0).map(|r| (r, std::iter::once(&()))))
        }
    }

    /// The seed rows, all `Always`.
    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> FromRows<(T0, T1, T2, T3, T4)>
        for Store<T0, T1, T2, T3, T4>
    {
        fn from_rows(rows: Vec<(T0, T1, T2, T3, T4)>) -> Self {
            let mut store = Self::default();
            for (f, vd, pd, vs, ps) in rows {
                store.always.insert(&(f, vs, ps, vd, pd));
            }
            store
        }
    }

    /// The `Always` rows as `(F, Vd, Pd, Vs, Ps)`, consuming the store, which frees as the output
    /// fills: this is the run's peak.
    pub fn into_always_vec<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col>(
        store: Store<T0, T1, T2, T3, T4>,
    ) -> Vec<(T0, T1, T2, T3, T4)> {
        let mut always = store.always;
        let mut out = Vec::with_capacity(always.len());
        for ((f, vs), group) in always.fwd.drain() {
            for (ps, vd, pd) in group {
                out.push((f.clone(), vd, pd, vs.clone(), ps));
            }
        }
        out
    }

    /// The store's rows as [`Rows`], in the relation's column order.
    pub struct AsRows<'a, T0: Col, T1: Col, T2: Col, T3: Col, T4: Col>(
        pub &'a Store<T0, T1, T2, T3, T4>,
    );
    impl<T0: Col, T1: Col, T2: Col, T3: Col, T4: Col> Rows<Row<T0, T1, T2, T3, T4>>
        for AsRows<'_, T0, T1, T2, T3, T4>
    {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn stream(&self) -> Box<dyn StreamingIterator<Item = Row<T0, T1, T2, T3, T4>> + '_> {
            Box::new(streaming_iterator::convert(rows(self.0).map(
                |(a, b, c, d, e, f)| (a.clone(), b.clone(), c.clone(), d.clone(), e.clone(), *f),
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// The BYODS provider macros, `#[ds(cond_trie::locals)]` and `#[ds(cond_trie::assign)]`. As in
// [`super::locals_trie`], the fourth argument is `ser` under `ascent!` and `par` under
// `ascent_par!`, and selects the serial store here or the concurrent one in
// [`super::c_cond_trie`]. The physical relation counts pushes and holds no rows.
// ---------------------------------------------------------------------------

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_rel_codegen {
    ($($tt:tt)*) => {};
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_rel {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt) => {
        $crate::index_engine::locals_trie::CountingVec<($t0, $t1, $t2, $t3, $t4, $c)>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt) => {
        $crate::index_engine::c_locals_trie::CCountingVec<($t0, $t1, $t2, $t3, $t4, $c)>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_locals_rel_ind_common {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt) => {
        $crate::index_engine::cond_trie::locals::Store<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt) => {
        $crate::index_engine::c_cond_trie::locals::Store<$t0, $t1, $t2, $t3, $t4>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_locals_rel_full_ind {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::locals::ToFull<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::locals::ToFull<$t0, $t1, $t2, $t3, $t4>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_locals_rel_ind {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::locals::ToNone<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [0, 1], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::locals::To01<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [0, 1, 2], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::locals::To012<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::locals::ToNone<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [0, 1], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::locals::To01<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [0, 1, 2], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::locals::To012<$t0, $t1, $t2, $t3, $t4>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_assign_rel_ind_common {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt) => {
        $crate::index_engine::cond_trie::assign::Store<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt) => {
        $crate::index_engine::c_cond_trie::assign::Store<$t0, $t1, $t2, $t3, $t4>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_assign_rel_full_ind {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::assign::ToFull<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::assign::ToFull<$t0, $t1, $t2, $t3, $t4>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! cond_trie_assign_rel_ind {
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::assign::ToNone<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [0, 3], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::assign::To03<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, ser, $args:tt, [0, 3, 4], $key:ty, $val:ty) => {
        $crate::index_engine::cond_trie::assign::To034<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::assign::ToNone<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [0, 3], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::assign::To03<$t0, $t1, $t2, $t3, $t4>
    };
    ($name:ident, ($t0:ty, $t1:ty, $t2:ty, $t3:ty, $t4:ty, $c:ty), $inds:tt, par, $args:tt, [0, 3, 4], $key:ty, $val:ty) => {
        $crate::index_engine::c_cond_trie::assign::To034<$t0, $t1, $t2, $t3, $t4>
    };
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    // Not `super::*`: the store's `ToRelIndex` in scope makes Ascent's generated calls ambiguous.
    use ascent::Lattice;
    use streaming_iterator::StreamingIterator;

    use super::{assign, covers, locals};
    use crate::index_engine::Rows;
    use crate::index_engine::decision::{Cond, DecisionId, DecisionSet};

    type Row = (u32, u32, u32, u32, u32, Cond);

    // The same rules over the store and over an Ascent lattice. `step` probes `0_1` and `link`
    // probes `0_1_2`; in `assign` order, `0_3` and `0_3_4`.
    ascent::ascent! {
        struct StoreLocals;
        relation seed(u32, u32, u32, u32, u32, Cond);
        relation step(u32, u32, u32, Cond);
        relation link(u32, u32, u32, u32, Cond);
        #[ds(crate::index_engine::cond_trie::locals)]
        relation r(u32, u32, u32, u32, u32, Cond);
        r(a, b, p, m, fp, *c) <-- seed(a, b, p, m, fp, c);
        r(a, b2, p, m, fp, c) <-- step(a, b, b2, c2), r(a, b, p, m, fp, c1), if let Some(c) = c1.and(*c2);
        r(a, b, q, m, fp, c) <-- link(a, b, p, q, c2), r(a, b, p, m, fp, c1), if let Some(c) = c1.and(*c2);
    }
    ascent::ascent! {
        struct LatticeLocals;
        relation seed(u32, u32, u32, u32, u32, Cond);
        relation step(u32, u32, u32, Cond);
        relation link(u32, u32, u32, u32, Cond);
        lattice r(u32, u32, u32, u32, u32, Cond);
        r(a, b, p, m, fp, *c) <-- seed(a, b, p, m, fp, c);
        r(a, b2, p, m, fp, c) <-- step(a, b, b2, c2), r(a, b, p, m, fp, c1), if let Some(c) = c1.and(*c2);
        r(a, b, q, m, fp, c) <-- link(a, b, p, q, c2), r(a, b, p, m, fp, c1), if let Some(c) = c1.and(*c2);
    }
    ascent::ascent! {
        struct StoreAssign;
        relation seed(u32, u32, u32, u32, u32, Cond);
        relation step(u32, u32, u32, Cond);
        relation link(u32, u32, u32, u32, Cond);
        #[ds(crate::index_engine::cond_trie::assign)]
        relation r(u32, u32, u32, u32, u32, Cond);
        r(a, m, fp, b, p, *c) <-- seed(a, b, p, m, fp, c);
        r(a, m, fp, b2, p, c) <-- step(a, b, b2, c2), r(a, m, fp, b, p, c1), if let Some(c) = c1.and(*c2);
        r(a, m, fp, b, q, c) <-- link(a, b, p, q, c2), r(a, m, fp, b, p, c1), if let Some(c) = c1.and(*c2);
    }
    ascent::ascent! {
        struct LatticeAssign;
        relation seed(u32, u32, u32, u32, u32, Cond);
        relation step(u32, u32, u32, Cond);
        relation link(u32, u32, u32, u32, Cond);
        lattice r(u32, u32, u32, u32, u32, Cond);
        r(a, m, fp, b, p, *c) <-- seed(a, b, p, m, fp, c);
        r(a, m, fp, b2, p, c) <-- step(a, b, b2, c2), r(a, m, fp, b, p, c1), if let Some(c) = c1.and(*c2);
        r(a, m, fp, b, q, c) <-- link(a, b, p, q, c2), r(a, m, fp, b, p, c1), if let Some(c) = c1.and(*c2);
    }

    /// A small deterministic generator, so a failure reproduces.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % u64::from(n)) as u32
        }
        fn cond(&mut self, collapse: bool) -> Cond {
            match self.next(4) {
                0 => Cond::Always,
                _ => {
                    let ids: Vec<DecisionId> = (0..1 + self.next(2))
                        .map(|_| DecisionId::from_raw(self.next(4)))
                        .collect();
                    let mut ds = DecisionSet::singleton(ids[0], collapse);
                    for d in &ids[1..] {
                        ds = ds.union(DecisionSet::singleton(*d, collapse));
                    }
                    Cond::Under(ds)
                }
            }
        }
    }

    struct Input {
        seed: Vec<Row>,
        step: Vec<(u32, u32, u32, Cond)>,
        link: Vec<(u32, u32, u32, u32, Cond)>,
    }

    fn input(rng: &mut Lcg, collapse: bool, vars: u32) -> Input {
        // Few columns' values, so facts are re-derived under many conditions and groups grow
        // past the small regime.
        let seed = (0..40)
            .map(|_| {
                (
                    rng.next(2),
                    rng.next(vars),
                    rng.next(4),
                    rng.next(3),
                    rng.next(3),
                    rng.cond(collapse),
                )
            })
            .collect();
        let step = (0..60)
            .map(|_| {
                (
                    rng.next(2),
                    rng.next(vars),
                    rng.next(vars),
                    rng.cond(collapse),
                )
            })
            .collect();
        let link = (0..30)
            .map(|_| {
                (
                    rng.next(2),
                    rng.next(vars),
                    rng.next(4),
                    rng.next(4),
                    rng.cond(collapse),
                )
            })
            .collect();
        Input { seed, step, link }
    }

    macro_rules! run {
        ($prog:ident, $input:expr) => {{
            let mut prog = $prog::default();
            prog.seed = $input.seed.clone();
            prog.step = $input.step.clone();
            prog.link = $input.link.clone();
            prog.run();
            prog
        }};
    }

    /// The rows in a canonical order, each condition as `None` for `Always` or its set's members
    /// and whether it is ⊤.
    #[allow(clippy::type_complexity)]
    fn sorted(
        rows: impl IntoIterator<Item = Row>,
    ) -> Vec<(u32, u32, u32, u32, u32, Option<(bool, Vec<DecisionId>)>)> {
        let set: BTreeSet<_> = rows
            .into_iter()
            .map(|(a, b, c, d, e, f)| {
                (
                    a,
                    b,
                    c,
                    d,
                    e,
                    f.set().map(|ds| (ds.is_top(), ds.ids().collect())),
                )
            })
            .collect();
        set.into_iter().collect()
    }

    #[test]
    fn the_store_reaches_the_lattice_fixpoint() {
        for collapse in [false, true] {
            for case in 0..60u64 {
                let mut rng = Lcg(case + 1);
                let vars = if case % 3 == 0 { 2 } else { 6 };
                let input = input(&mut rng, collapse, vars);

                let want = sorted(run!(LatticeLocals, input).r);
                let store = run!(StoreLocals, input);
                let got = sorted(locals::AsRows(&store.__r_ind_common).stream().cloned());
                assert_eq!(got, want, "locals order, case {case}, collapse {collapse}");
                assert_eq!(store.__r_ind_common.len(), want.len());

                let want = sorted(run!(LatticeAssign, input).r);
                let store = run!(StoreAssign, input);
                let got = sorted(assign::AsRows(&store.__r_ind_common).stream().cloned());
                assert_eq!(got, want, "assign order, case {case}, collapse {collapse}");
                assert_eq!(store.__r_ind_common.len(), want.len());
            }
        }
    }

    #[test]
    fn coverage_is_the_join_order() {
        let d = |i| Cond::Under(DecisionSet::singleton(DecisionId::from_raw(i), false));
        let d01 = {
            let mut c = d(0);
            c.join_mut(d(1));
            c
        };
        assert!(covers(Cond::Always, Cond::Always));
        assert!(covers(Cond::Always, d(0)));
        assert!(!covers(d(0), Cond::Always));
        assert!(covers(d(0), d(0)));
        assert!(!covers(d(0), d(1)));
        assert!(covers(d01, d(1)));
        assert!(!covers(d(1), d01));
    }
}
