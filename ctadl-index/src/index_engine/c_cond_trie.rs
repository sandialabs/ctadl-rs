//! The parallel twin of [`super::cond_trie`], for `ascent_par!`.
//!
//! Same lattice semantics, same layout: the `Always` rows in a [`CLocalsIndCommon`], the trie of
//! [`super::c_locals_trie`], and the conditional rows beside it in a map keyed the same way, here
//! a [`CMap`] whose group values are the serial module's [`CondLeaves`]. What changes is what
//! [`super::c_locals_trie`] explains parallel Ascent asks for: `total` and `delta` are frozen
//! while rules run and only read, `new` is unfrozen and only written, through a shared reference,
//! and the merge runs single-threaded on `&mut`. Both halves freeze and unfreeze together, so a
//! frozen store hands out plain references into either.
//!
//! ## Joining into `new`
//!
//! `insert` must report true only if it grew `new`'s condition for the fact, and for at most one
//! of several threads racing on it, since that is what Ascent counts and what sets `changed`.
//!
//! - An `Always` row goes into the trie, whose insert already has exactly that property. It does
//!   not look at the side map: a conditional row it makes redundant is dropped at the merge.
//! - A conditional row takes the side map's entry for its group, one shard write lock, and does
//!   everything else under it: checks the trie (an `Always` row for the fact means no growth),
//!   then joins into the leaf. Two conditional rows for one fact are therefore serialized. An
//!   `Always` row racing with it is not, but the trie check and the join happen under one lock
//!   that no other conditional insert can enter, so the outcome is the same as if the two had run
//!   in some order.
//! - A join that spills to `Always` inserts into the trie first, still under the lock, and only
//!   then removes the leaf, so no other thread sees the fact missing from both halves. Its result
//!   is the trie insert's: if a racing `Always` row got there first, this join grew nothing.
//!
//! Lock order is always side map, then trie; an `Always` insert takes only the trie's, so the two
//! cannot deadlock. `new` can end an iteration holding a fact in both halves, which the merge
//! resolves exactly as the serial one does.

use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::dashmap::ReadOnlyView;
use ascent::internal::{DashMapViewParIter, Freezable, RelIndexMerge};
use ascent::rayon::iter::plumbing::UnindexedConsumer;
use ascent::rayon::iter::{IntoParallelIterator, ParallelIterator};
use ascent_base::Lattice;

use super::c_locals_trie::{CLocalsIndCommon, CMap, CollectedParIter, Hasher};
use super::cond_trie::{ALWAYS, Col, CondGet, CondLeaves, Map, covers};
use super::decision::Cond;
use super::locals_trie::hb_bytes;
use super::path_group::PathGroup;

/// The bound every column of the concurrent store meets.
pub trait CCol: Col + Send + Sync {}
impl<T: Col + Send + Sync> CCol for T {}

/// A row in the store's column order, borrowed.
type SRow<'a, K1, K2, P, A, B> = (&'a K1, &'a K2, &'a P, &'a A, &'a B, &'a Cond);

/// The conditional leaves of one group, as `(&P, &A, &B, &Cond)`.
fn leaf_rows<P, A, B>(
    leaves: &CondLeaves<P, A, B>,
) -> impl Iterator<Item = (&P, &A, &B, &Cond)> + Clone {
    leaves
        .iter()
        .flat_map(|(p, abs)| abs.iter().map(move |((a, b), c)| (p, a, b, c)))
}

/// The rows of one group, the `Always` ones then the conditional ones.
fn group_rows<'a, P: Col, A: Col, B: Col>(
    always: Option<&'a PathGroup<P, A, B>>,
    cond: Option<&'a CondLeaves<P, A, B>>,
) -> impl Iterator<Item = (&'a P, &'a A, &'a B, &'a Cond)> + Clone + 'a {
    always
        .into_iter()
        .flat_map(|g| g.iter().map(|(p, a, b)| (p, a, b, ALWAYS)))
        .chain(cond.into_iter().flat_map(leaf_rows))
}

/// Every entry of `m` in either freeze state, each mapped through `f` while its shard is locked.
fn map_entries<'a, K: Clone + Eq + Hash, V, R: 'a>(
    m: &'a CMap<K, V>,
    f: impl Fn(&K, &V) -> Vec<R> + 'a,
) -> Box<dyn Iterator<Item = R> + 'a> {
    match m {
        CMap::Frozen(v) => Box::new(v.iter().flat_map(move |(k, v)| f(k, v))),
        CMap::Unfrozen(dm) => Box::new(dm.iter().flat_map(move |e| f(e.key(), e.value()))),
    }
}

/// The concurrent store. See the module docs.
pub struct CCondTrie<K1, K2, P, A, B>
where
    K1: Col,
    K2: Col,
    P: Col,
    A: Col,
    B: Col,
{
    always: CLocalsIndCommon<K1, K2, P, A, B>,
    cond: CMap<(K1, K2), CondLeaves<P, A, B>>,
    cond_len: AtomicUsize,
}

impl<K1: Col, K2: Col, P: Col, A: Col, B: Col> Default for CCondTrie<K1, K2, P, A, B> {
    fn default() -> Self {
        Self {
            always: CLocalsIndCommon::default(),
            cond: CMap::default(),
            cond_len: AtomicUsize::new(0),
        }
    }
}

impl<K1: Col, K2: Col, P: Col, A: Col, B: Col> Clone for CCondTrie<K1, K2, P, A, B> {
    fn clone(&self) -> Self {
        Self {
            always: self.always.clone(),
            cond: self.cond.clone(),
            cond_len: AtomicUsize::new(self.conditional_len()),
        }
    }
}

impl<K1: Col, K2: Col, P: Col, A: Col, B: Col> Freezable for CCondTrie<K1, K2, P, A, B> {
    fn freeze(&mut self) {
        self.always.freeze();
        self.cond.freeze();
    }
    fn unfreeze(&mut self) {
        self.always.unfreeze();
        self.cond.unfreeze();
    }
}

impl<K1: Col, K2: Col, P: Col, A: Col, B: Col> CCondTrie<K1, K2, P, A, B> {
    /// Number of facts. Exact after a merge; `new` can count a fact twice before one.
    #[inline]
    pub fn len(&self) -> usize {
        self.always.len() + self.conditional_len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `Always` rows, without their condition.
    #[inline]
    pub fn always(&self) -> &CLocalsIndCommon<K1, K2, P, A, B> {
        &self.always
    }

    /// Number of conditional rows.
    #[inline]
    pub fn conditional_len(&self) -> usize {
        self.cond_len.load(Ordering::Relaxed)
    }

    /// The condition a fact holds under here, if any, in either freeze state.
    fn cond_at(&self, k1: &K1, k2: &K2, p: &P, a: &A, b: &B) -> Option<Cond> {
        if self.always.contains(k1, k2, p, a, b) {
            return Some(Cond::Always);
        }
        if self.conditional_len() == 0 {
            return None;
        }
        self.cond
            .get_with(&(k1.clone(), k2.clone()), |leaves| {
                leaves.get(p)?.get(&(a.clone(), b.clone())).copied()
            })
            .flatten()
    }

    /// Joins `c` into the condition of the fact, through a shared reference to the unfrozen
    /// store. Returns true if this call grew the store's condition for it. See the module docs.
    pub(crate) fn insert(&self, k1: &K1, k2: &K2, p: &P, a: &A, b: &B, c: Cond) -> bool {
        use ascent::dashmap::mapref::entry::Entry;
        let full = || (k1.clone(), k2.clone(), p.clone(), a.clone(), b.clone());
        if c.is_always() {
            return self.always.c_insert(&full());
        }
        match self.cond.unfrozen().entry((k1.clone(), k2.clone())) {
            Entry::Vacant(e) => {
                if self.always.contains(k1, k2, p, a, b) {
                    return false;
                }
                let mut leaves = CondLeaves::default();
                leaves
                    .entry(p.clone())
                    .or_default()
                    .insert((a.clone(), b.clone()), c);
                e.insert(leaves);
                self.cond_len.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(mut e) => {
                if self.always.contains(k1, k2, p, a, b) {
                    return false;
                }
                let leaves = e.get_mut();
                let abs = leaves.entry(p.clone()).or_default();
                match abs.entry((a.clone(), b.clone())) {
                    hashbrown::hash_map::Entry::Vacant(v) => {
                        v.insert(c);
                        self.cond_len.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                    hashbrown::hash_map::Entry::Occupied(mut o) => {
                        if !o.get_mut().join_mut(c) {
                            return false;
                        }
                        if !o.get().is_always() {
                            return true;
                        }
                        o.remove();
                    }
                }
                // The join spilled to `Always`.
                let grew = self.always.c_insert(&full());
                self.cond_len.fetch_sub(1, Ordering::Relaxed);
                if abs.is_empty() {
                    leaves.remove(p);
                    if leaves.is_empty() {
                        e.remove();
                    }
                }
                grew
            }
        }
    }

    /// Joins `from` into `self`, emptying `from`. The delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        // The conditional rows that `from` makes unconditional.
        if self.conditional_len() != 0 && !from.always.is_empty() {
            let mut removed = 0;
            let fwd = &from.always.fwd;
            self.cond.unfrozen().retain(|g, leaves| {
                fwd.get_with(g, |group| {
                    leaves.retain(|p, abs| {
                        abs.retain(|(a, b), _| {
                            let gone = group.contains(p, a, b);
                            removed += usize::from(gone);
                            !gone
                        });
                        !abs.is_empty()
                    });
                });
                !leaves.is_empty()
            });
            *self.cond_len.get_mut() -= removed;
        }
        self.always.absorb(&mut from.always);
        // Drained in place, as the trie's absorb does: `from` becomes the next `new`.
        for shard in from.cond.unfrozen_mut().shards_mut() {
            for ((k1, k2), leaves) in shard.get_mut().drain() {
                for (p, abs) in leaves.into_inner() {
                    for ((a, b), c) in abs {
                        self.insert(&k1, &k2, &p, &a, &b, c);
                    }
                }
            }
        }
        *from.cond_len.get_mut() = 0;
    }

    /// Makes `self`, the rows found this iteration, the next `delta` against `total`, which has
    /// already absorbed the old `delta`: a conditional row `total` covers is dropped, and the
    /// rest carry their join with what `total` holds.
    fn refine(&mut self, total: &Self) {
        if self.conditional_len() == 0 {
            return;
        }
        let mut removed = 0;
        let mut spilled = Vec::new();
        let always = &self.always;
        self.cond.unfrozen().retain(|(k1, k2), leaves| {
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
        *self.cond_len.get_mut() -= removed;
        for row in spilled {
            self.always.c_insert(&row);
        }
    }

    /// Every row, in the store's column order, cloned, in either freeze state. An unfrozen store
    /// holds a shard's read lock while the iterator is inside it.
    #[allow(clippy::type_complexity)]
    pub fn rows(&self) -> impl Iterator<Item = (K1, K2, P, A, B, Cond)> + '_ {
        let always = map_entries(&self.always.fwd, |(k1, k2), group| {
            group
                .iter()
                .map(|(p, a, b)| {
                    (
                        k1.clone(),
                        k2.clone(),
                        p.clone(),
                        a.clone(),
                        b.clone(),
                        Cond::Always,
                    )
                })
                .collect()
        });
        let cond = map_entries(&self.cond, |(k1, k2), leaves| {
            leaf_rows(leaves)
                .map(|(p, a, b, c)| (k1.clone(), k2.clone(), p.clone(), a.clone(), b.clone(), *c))
                .collect()
        });
        always.chain(cond)
    }

    /// Visits every row, in the store's column order, in either freeze state.
    pub fn for_each_row(&self, mut f: impl FnMut(SRow<'_, K1, K2, P, A, B>)) {
        self.always
            .for_each_row(|(k1, k2, p, a, b)| f((k1, k2, p, a, b, ALWAYS)));
        self.cond.for_each(|(k1, k2), leaves| {
            for (p, a, b, c) in leaf_rows(leaves) {
                f((k1, k2, p, a, b, c));
            }
        });
    }

    /// Heap bytes of the conditional rows, load-factor slack included. The `Always` rows'
    /// are [`CLocalsIndCommon::heap_report`]'s.
    pub fn cond_heap_bytes(&self) -> usize {
        let leaves_sz = std::mem::size_of::<((K1, K2), CondLeaves<P, A, B>)>();
        let abs_sz = std::mem::size_of::<(P, Map<(A, B), Cond>)>();
        let leaf_sz = std::mem::size_of::<((A, B), Cond)>();
        let mut bytes = self.cond.table_bytes(leaves_sz);
        self.cond.for_each(|_, leaves| {
            bytes += hb_bytes(leaves.capacity(), abs_sz);
            for abs in leaves.values() {
                bytes += hb_bytes(abs.capacity(), leaf_sz);
            }
        });
        bytes
    }

    // ---- Reads of the frozen store, which is all a rule sees ----

    fn frozen_always(&self) -> &ReadOnlyView<(K1, K2), PathGroup<P, A, B>, Hasher> {
        self.always.fwd.frozen()
    }

    /// The frozen side map, or `None` when it is empty, which spares a probe of it.
    fn frozen_cond(&self) -> Option<&ReadOnlyView<(K1, K2), CondLeaves<P, A, B>, Hasher>> {
        (self.conditional_len() != 0).then(|| self.cond.frozen())
    }

    /// Every row, in the store's column order.
    fn frozen_rows(&self) -> impl Iterator<Item = SRow<'_, K1, K2, P, A, B>> + '_ {
        let always = self.frozen_always().iter().flat_map(|((k1, k2), group)| {
            group.iter().map(move |(p, a, b)| (k1, k2, p, a, b, ALWAYS))
        });
        let cond = self.frozen_cond().into_iter().flat_map(|cond| {
            cond.iter().flat_map(|((k1, k2), leaves)| {
                leaf_rows(leaves).map(move |(p, a, b, c)| (k1, k2, p, a, b, c))
            })
        });
        always.chain(cond)
    }

    /// Group `(k1, k2)`'s two halves; `None` if it has no rows, which cuts the caller's join.
    #[allow(clippy::type_complexity)]
    fn group(
        &self,
        key: &(K1, K2),
    ) -> Option<(Option<&PathGroup<P, A, B>>, Option<&CondLeaves<P, A, B>>)> {
        let always = self.frozen_always().get(key);
        let cond = self.frozen_cond().and_then(|cond| cond.get(key));
        (always.is_some() || cond.is_some()).then_some((always, cond))
    }

    /// Every group key, each once.
    fn group_keys(&self) -> Vec<&(K1, K2)> {
        let mut keys: Map<&(K1, K2), ()> = self.frozen_always().keys().map(|k| (k, ())).collect();
        if let Some(cond) = self.frozen_cond() {
            keys.extend(cond.keys().map(|k| (k, ())));
        }
        keys.into_keys().collect()
    }

    /// The rows at `(k1, k2, p)`; `None` if it has none, which cuts the caller's join.
    #[inline]
    fn get(&self, key: &(K1, K2, P)) -> Option<CondGet<'_, P, A, B>> {
        let g = (key.0.clone(), key.1.clone());
        let always = self
            .frozen_always()
            .get(&g)
            .and_then(|group| group.get(&key.2));
        let cond = self
            .frozen_cond()
            .and_then(|cond| cond.get(&g))
            .and_then(|leaves| leaves.get(&key.2));
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
        for (k1, k2, p, a, b, c) in self.frozen_rows() {
            by.entry((k1, k2, p)).or_default().push((a, b, c));
        }
        by.into_iter().collect()
    }

    fn rows_par<'a, R>(
        &'a self,
        f: fn(SRow<'a, K1, K2, P, A, B>) -> R,
    ) -> CRowsParIter<'a, K1, K2, P, A, B, R> {
        CRowsParIter {
            always: self.frozen_always(),
            cond: self.frozen_cond(),
            f,
        }
    }

    fn groups_par<'a, V>(
        &'a self,
        f: fn((&'a P, &'a A, &'a B, &'a Cond)) -> V,
    ) -> CGroupsParIter<'a, K1, K2, P, A, B, V> {
        CGroupsParIter {
            always: self.frozen_always(),
            cond: self.frozen_cond(),
            f,
        }
    }
}

impl<K1: Col, K2: Col, P: Col, A: Col, B: Col> RelIndexMerge for CCondTrie<K1, K2, P, A, B> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.absorb(from);
    }

    fn merge_delta_to_total_new_to_delta(new: &mut Self, delta: &mut Self, total: &mut Self) {
        total.absorb(delta);
        new.refine(total);
        std::mem::swap(new, delta);
    }
}

// ---------------------------------------------------------------------------
// Parallel iterators over the frozen store. Each splits over `DashMap`'s shards, the `Always`
// half's then the side map's, and maps a row through a column reordering `f`.
// ---------------------------------------------------------------------------

/// Every row, as `f` of it.
pub struct CRowsParIter<'a, K1: Col, K2: Col, P, A, B, R> {
    always: &'a ReadOnlyView<(K1, K2), PathGroup<P, A, B>, Hasher>,
    cond: Option<&'a ReadOnlyView<(K1, K2), CondLeaves<P, A, B>, Hasher>>,
    f: fn(SRow<'a, K1, K2, P, A, B>) -> R,
}

impl<K1: Col, K2: Col, P, A, B, R> Clone for CRowsParIter<'_, K1, K2, P, A, B, R> {
    fn clone(&self) -> Self {
        Self {
            always: self.always,
            cond: self.cond,
            f: self.f,
        }
    }
}

impl<'a, K1: CCol, K2: CCol, P: CCol, A: CCol, B: CCol, R: Send> ParallelIterator
    for CRowsParIter<'a, K1, K2, P, A, B, R>
{
    type Item = R;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        let f = self.f;
        let always =
            DashMapViewParIter::new(self.always).flat_map_iter(move |((k1, k2), group)| {
                group
                    .iter()
                    .map(move |(p, a, b)| f((k1, k2, p, a, b, ALWAYS)))
            });
        let cond = self.cond.into_par_iter().flat_map(move |cond| {
            DashMapViewParIter::new(cond).flat_map_iter(move |((k1, k2), leaves)| {
                leaf_rows(leaves).map(move |(p, a, b, c)| f((k1, k2, p, a, b, c)))
            })
        });
        always.chain(cond).drive_unindexed(consumer)
    }
}

/// One item per group, its key and its rows, each as `f` of `(&P, &A, &B, &Cond)`.
pub struct CGroupsParIter<'a, K1: Col, K2: Col, P, A, B, V> {
    always: &'a ReadOnlyView<(K1, K2), PathGroup<P, A, B>, Hasher>,
    cond: Option<&'a ReadOnlyView<(K1, K2), CondLeaves<P, A, B>, Hasher>>,
    f: fn((&'a P, &'a A, &'a B, &'a Cond)) -> V,
}

impl<'a, K1: CCol, K2: CCol, P: CCol, A: CCol, B: CCol, V: Send> ParallelIterator
    for CGroupsParIter<'a, K1, K2, P, A, B, V>
{
    type Item = ((&'a K1, &'a K2), CollectedParIter<V>);

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item>,
    {
        let (always, cond, f) = (self.always, self.cond, self.f);
        // A group in the trie, with its conditional rows if it has any.
        let with_always = DashMapViewParIter::new(always).map(move |(k, group)| {
            let leaves = cond.and_then(|cond| cond.get(k));
            let rows = group_rows(Some(group), leaves).map(f).collect();
            ((&k.0, &k.1), CollectedParIter(rows))
        });
        // A group with only conditional rows.
        let cond_only = cond.into_par_iter().flat_map(move |cond| {
            DashMapViewParIter::new(cond)
                .filter(move |(k, _)| !always.contains_key(*k))
                .map(move |(k, leaves)| {
                    let rows = group_rows(None, Some(leaves)).map(f).collect();
                    ((&k.0, &k.1), CollectedParIter(rows))
                })
        });
        with_always.chain(cond_only).drive_unindexed(consumer)
    }
}

// ---------------------------------------------------------------------------
// Views and their `ToRelIndex` markers.
//
// As in [`super::c_locals_trie`], `to_c_rel_index_write` hands back the read view, so the view is
// also every index's write target: the full index's inserts, the rest no-ops. None merges; the
// ind_common does.
// ---------------------------------------------------------------------------

/// A view over the store. The index it serves is the marker's.
pub struct View<'a, S, Ix>(&'a S, PhantomData<Ix>);

impl<S, Ix> RelIndexMerge for View<'_, S, Ix> {
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

/// Defines a zero-sized `ToRelIndex` marker `$to` over the enclosing module's `Store`.
macro_rules! marker {
    ($to:ident) => {
        pub struct $to<T0, T1, T2, T3, T4>(PhantomData<(T0, T1, T2, T3, T4)>);
        impl<T0, T1, T2, T3, T4> Default for $to<T0, T1, T2, T3, T4> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<T0, T1, T2, T3, T4> Freezable for $to<T0, T1, T2, T3, T4> {}
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> ToRelIndex<Store<T0, T1, T2, T3, T4>>
            for $to<T0, T1, T2, T3, T4>
        {
            type RelIndex<'a>
                = View<'a, Store<T0, T1, T2, T3, T4>, $to<T0, T1, T2, T3, T4>>
            where
                Self: 'a,
                Store<T0, T1, T2, T3, T4>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a Store<T0, T1, T2, T3, T4>,
            ) -> Self::RelIndex<'a> {
                View(rel, PhantomData)
            }
            type RelIndexWrite<'a>
                = View<'a, Store<T0, T1, T2, T3, T4>, $to<T0, T1, T2, T3, T4>>
            where
                Self: 'a,
                Store<T0, T1, T2, T3, T4>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                rel: &'a mut Store<T0, T1, T2, T3, T4>,
            ) -> Self::RelIndexWrite<'a> {
                View(rel, PhantomData)
            }
        }
    };
}

/// Both write traits as no-ops, for a view whose rows the full index already put in the store.
macro_rules! noop_writes {
    ($to:ident, $key:ty, $val:ty) => {
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $to<T0, T1, T2, T3, T4>>
        {
            type Key = $key;
            type Value = $val;
            #[inline(always)]
            fn index_insert(&mut self, _key: Self::Key, _value: Self::Value) {}
        }
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $to<T0, T1, T2, T3, T4>>
        {
            type Key = $key;
            type Value = $val;
            #[inline(always)]
            fn index_insert(&self, _key: Self::Key, _value: Self::Value) {}
        }
    };
}

/// The four indices of a column order over the enclosing module's `Store`: every row (`$none`),
/// a group (`$group`), a group's path (`$path`) and the full index (`$full`). `store` names the
/// relation's columns in the store's order `(K1, K2, P, A, B)`, and `group_val` the group index's
/// value columns. The module supplies the reorderings as three functions: `row_ref` (a store row
/// to the relation's order), `store_key` (a relation row to the store's columns) and `group_val`
/// (a group's `(P, A, B, Cond)` to the group index's value), plus `row_owned` for [`Rows`].
macro_rules! views {
    ($none:ident, $group:ident, $path:ident, $full:ident;
     store [$k1:ident, $k2:ident, $p:ident, $a:ident, $b:ident];
     group_val [$g0:ident, $g1:ident, $g2:ident]) => {
        marker!($none);
        marker!($group);
        marker!($path);
        marker!($full);

        noop_writes!($none, (), Row<T0, T1, T2, T3, T4>);
        noop_writes!($group, ($k1, $k2), ($g0, $g1, $g2, Cond));
        noop_writes!($path, ($k1, $k2, $p), ($a, $b, Cond));

        // ---- none: every row ----
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $none<T0, T1, T2, T3, T4>>
        {
            type Key = ();
            type Value = SRow<'a, T0, T1, T2, T3, T4>;
            type IteratorType = DynIter<'a, Self::Value>;
            #[inline]
            fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
                let s = self.0;
                Some(DynIter::new(move || s.frozen_rows().map(row_ref)))
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
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $none<T0, T1, T2, T3, T4>>
        {
            type Key = ();
            type Value = SRow<'a, T0, T1, T2, T3, T4>;
            type ValueIteratorType = DynIter<'a, Self::Value>;
            type AllIteratorType = std::iter::Once<((), Self::ValueIteratorType)>;
            #[inline]
            fn iter_all(&'a self) -> Self::AllIteratorType {
                std::iter::once(((), RelIndexRead::index_get(self, &()).unwrap()))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $none<T0, T1, T2, T3, T4>>
        {
            type Key = ();
            type Value = SRow<'a, T0, T1, T2, T3, T4>;
            type IteratorType = CRowsParIter<'a, $k1, $k2, $p, $a, $b, Self::Value>;
            #[inline]
            fn c_index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
                Some(self.0.rows_par(row_ref))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $none<T0, T1, T2, T3, T4>>
        {
            type Key = ();
            type Value = SRow<'a, T0, T1, T2, T3, T4>;
            type ValueIteratorType = CRowsParIter<'a, $k1, $k2, $p, $a, $b, Self::Value>;
            type AllIteratorType = ascent::rayon::iter::Once<((), Self::ValueIteratorType)>;
            #[inline]
            fn c_iter_all(&'a self) -> Self::AllIteratorType {
                ascent::rayon::iter::once(((), self.0.rows_par(row_ref)))
            }
        }

        // ---- group: one group's rows ----
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $group<T0, T1, T2, T3, T4>>
        {
            type Key = ($k1, $k2);
            type Value = (&'a $g0, &'a $g1, &'a $g2, &'a Cond);
            type IteratorType = DynIter<'a, Self::Value>;
            #[inline]
            fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
                let (always, cond) = self.0.group(key)?;
                Some(DynIter::new(move || group_rows(always, cond).map(group_val)))
            }
            #[inline]
            fn len_estimate(&self) -> usize {
                self.0.always.fwd.len_estimate()
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $group<T0, T1, T2, T3, T4>>
        {
            type Key = (&'a $k1, &'a $k2);
            type Value = (&'a $g0, &'a $g1, &'a $g2, &'a Cond);
            type ValueIteratorType = DynIter<'a, Self::Value>;
            type AllIteratorType =
                Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
            fn iter_all(&'a self) -> Self::AllIteratorType {
                let s = self.0;
                Box::new(s.group_keys().into_iter().map(move |k| {
                    let (always, cond) = s.group(k).unwrap();
                    let it = DynIter::new(move || group_rows(always, cond).map(group_val));
                    ((&k.0, &k.1), it)
                }))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $group<T0, T1, T2, T3, T4>>
        {
            type Key = ($k1, $k2);
            type Value = (&'a $g0, &'a $g1, &'a $g2, &'a Cond);
            type IteratorType = CollectedParIter<Self::Value>;
            #[inline]
            fn c_index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
                let (always, cond) = self.0.group(key)?;
                Some(CollectedParIter(group_rows(always, cond).map(group_val).collect()))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $group<T0, T1, T2, T3, T4>>
        {
            type Key = (&'a $k1, &'a $k2);
            type Value = (&'a $g0, &'a $g1, &'a $g2, &'a Cond);
            type ValueIteratorType = CollectedParIter<Self::Value>;
            type AllIteratorType = CGroupsParIter<'a, $k1, $k2, $p, $a, $b, Self::Value>;
            #[inline]
            fn c_iter_all(&'a self) -> Self::AllIteratorType {
                self.0.groups_par(group_val)
            }
        }

        // ---- path: the exact-path probe ----
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $path<T0, T1, T2, T3, T4>>
        {
            type Key = ($k1, $k2, $p);
            type Value = (&'a $a, &'a $b, &'a Cond);
            type IteratorType = CondGet<'a, $p, $a, $b>;
            #[inline]
            fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
                self.0.get(key)
            }
            #[inline]
            fn len_estimate(&self) -> usize {
                self.0.always.fwd.len_estimate()
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $path<T0, T1, T2, T3, T4>>
        {
            type Key = (&'a $k1, &'a $k2, &'a $p);
            type Value = (&'a $a, &'a $b, &'a Cond);
            type ValueIteratorType = std::vec::IntoIter<Self::Value>;
            type AllIteratorType =
                Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
            fn iter_all(&'a self) -> Self::AllIteratorType {
                Box::new(
                    self.0
                        .iter_by_path()
                        .into_iter()
                        .map(|(k, vs)| (k, vs.into_iter())),
                )
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $path<T0, T1, T2, T3, T4>>
        {
            type Key = ($k1, $k2, $p);
            type Value = (&'a $a, &'a $b, &'a Cond);
            type IteratorType = CollectedParIter<Self::Value>;
            #[inline]
            fn c_index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
                Some(CollectedParIter(self.0.get(key)?.collect()))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $path<T0, T1, T2, T3, T4>>
        {
            type Key = (&'a $k1, &'a $k2, &'a $p);
            type Value = (&'a $a, &'a $b, &'a Cond);
            type ValueIteratorType = CollectedParIter<Self::Value>;
            type AllIteratorType = CollectedParIter<(Self::Key, Self::ValueIteratorType)>;
            fn c_iter_all(&'a self) -> Self::AllIteratorType {
                CollectedParIter(
                    self.0
                        .iter_by_path()
                        .into_iter()
                        .map(|(k, vs)| (k, CollectedParIter(vs)))
                        .collect(),
                )
            }
        }

        // ---- full: coverage, and the one real writer ----
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelFullIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            #[inline]
            fn contains_key(&'a self, r: &Self::Key) -> bool {
                let (k1, k2, p, a, b) = store_key(r);
                self.0
                    .cond_at(k1, k2, p, a, b)
                    .is_some_and(|held| covers(held, r.5))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = &'a ();
            type IteratorType = std::iter::Once<&'a ()>;
            #[inline]
            fn index_get(&'a self, r: &Self::Key) -> Option<Self::IteratorType> {
                let (k1, k2, p, a, b) = store_key(r);
                (self.0.cond_at(k1, k2, p, a, b) == Some(r.5)).then(|| std::iter::once(&()))
            }
            #[inline]
            fn len_estimate(&self) -> usize {
                self.0.len()
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = SRow<'a, T0, T1, T2, T3, T4>;
            type Value = &'a ();
            type ValueIteratorType = std::iter::Once<&'a ()>;
            type AllIteratorType =
                Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
            fn iter_all(&'a self) -> Self::AllIteratorType {
                Box::new(
                    self.0
                        .frozen_rows()
                        .map(|r| (row_ref(r), std::iter::once(&()))),
                )
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexRead<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = &'a ();
            type IteratorType = ascent::rayon::iter::Once<&'a ()>;
            #[inline]
            fn c_index_get(&'a self, r: &Self::Key) -> Option<Self::IteratorType> {
                let (k1, k2, p, a, b) = store_key(r);
                (self.0.cond_at(k1, k2, p, a, b) == Some(r.5))
                    .then(|| ascent::rayon::iter::once(&()))
            }
        }
        impl<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexReadAll<'a>
            for View<'a, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = SRow<'a, T0, T1, T2, T3, T4>;
            type Value = &'a ();
            type ValueIteratorType = ascent::rayon::iter::Once<&'a ()>;
            type AllIteratorType =
                CRowsParIter<'a, $k1, $k2, $p, $a, $b, (Self::Key, Self::ValueIteratorType)>;
            fn c_iter_all(&'a self) -> Self::AllIteratorType {
                self.0
                    .rows_par(|r| (row_ref(r), ascent::rayon::iter::once(&())))
            }
        }
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelFullIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = ();
            #[inline]
            fn insert_if_not_present(&self, r: &Self::Key, _v: ()) -> bool {
                let (k1, k2, p, a, b) = store_key(r);
                self.0.insert(k1, k2, p, a, b, r.5)
            }
        }
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelFullIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = ();
            #[inline]
            fn insert_if_not_present(&mut self, r: &Self::Key, _v: ()) -> bool {
                let (k1, k2, p, a, b) = store_key(r);
                self.0.insert(k1, k2, p, a, b, r.5)
            }
        }
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> CRelIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = ();
            #[inline]
            fn index_insert(&self, r: Self::Key, _v: ()) {
                let (k1, k2, p, a, b) = store_key(&r);
                self.0.insert(k1, k2, p, a, b, r.5);
            }
        }
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> RelIndexWrite
            for View<'_, Store<T0, T1, T2, T3, T4>, $full<T0, T1, T2, T3, T4>>
        {
            type Key = Row<T0, T1, T2, T3, T4>;
            type Value = ();
            #[inline]
            fn index_insert(&mut self, r: Self::Key, _v: ()) {
                let (k1, k2, p, a, b) = store_key(&r);
                self.0.insert(k1, k2, p, a, b, r.5);
            }
        }

        /// The store's rows as [`Rows`], in the relation's column order, in either freeze state.
        pub struct AsRows<'a, T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol>(
            pub &'a Store<T0, T1, T2, T3, T4>,
        );
        impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> Rows<Row<T0, T1, T2, T3, T4>>
            for AsRows<'_, T0, T1, T2, T3, T4>
        {
            fn len(&self) -> usize {
                self.0.len()
            }
            fn stream(&self) -> Box<dyn StreamingIterator<Item = Row<T0, T1, T2, T3, T4>> + '_> {
                Box::new(streaming_iterator::convert(self.0.rows().map(row_owned)))
            }
        }
    };
}

/// `locals(F, V, P, M, Fp, Cond)` and `edge_split`: the store's own column order, grouped on
/// columns 0 and 1 and probed on 2. Indices `none`, `0_1`, `0_1_2` and the full index.
pub mod locals {
    use std::marker::PhantomData;

    use ascent::internal::{
        CRelFullIndexWrite, CRelIndexRead, CRelIndexReadAll, CRelIndexWrite, Freezable,
        RelFullIndexRead, RelFullIndexWrite, RelIndexRead, RelIndexReadAll, RelIndexWrite,
        ToRelIndex,
    };
    use streaming_iterator::StreamingIterator;

    use super::{CCol, CCondTrie, CGroupsParIter, CRowsParIter, CollectedParIter, SRow, View};
    use super::{covers, group_rows};
    use crate::index_engine::Rows;
    use crate::index_engine::cond_trie::CondGet;
    use crate::index_engine::decision::Cond;
    use crate::index_engine::locals_trie::DynIter;

    pub type Store<T0, T1, T2, T3, T4> = CCondTrie<T0, T1, T2, T3, T4>;
    type Row<T0, T1, T2, T3, T4> = (T0, T1, T2, T3, T4, Cond);

    #[inline(always)]
    fn row_ref<'a, T0, T1, T2, T3, T4>(
        r: SRow<'a, T0, T1, T2, T3, T4>,
    ) -> SRow<'a, T0, T1, T2, T3, T4> {
        r
    }
    #[inline(always)]
    fn row_owned<T0, T1, T2, T3, T4>(r: Row<T0, T1, T2, T3, T4>) -> Row<T0, T1, T2, T3, T4> {
        r
    }
    #[inline(always)]
    fn store_key<T0, T1, T2, T3, T4>(r: &Row<T0, T1, T2, T3, T4>) -> (&T0, &T1, &T2, &T3, &T4) {
        (&r.0, &r.1, &r.2, &r.3, &r.4)
    }
    #[inline(always)]
    fn group_val<'a, P, A, B>(
        r: (&'a P, &'a A, &'a B, &'a Cond),
    ) -> (&'a P, &'a A, &'a B, &'a Cond) {
        r
    }

    views!(ToNone, To01, To012, ToFull; store [T0, T1, T2, T3, T4]; group_val [T2, T3, T4]);
}

/// `assign_like(F, Vd, Pd, Vs, Ps, Cond)` and `ext_dst`: an edge `Vd.Pd = Vs.Ps`, grouped on its
/// source `(F, Vs)` and probed on `Ps`, so the store's order is `(F, Vs, Ps, Vd, Pd)`. Indices
/// `none`, `0_3`, `0_3_4` and the full index.
pub mod assign {
    use std::marker::PhantomData;

    use ascent::internal::{
        CRelFullIndexWrite, CRelIndexRead, CRelIndexReadAll, CRelIndexWrite, Freezable,
        RelFullIndexRead, RelFullIndexWrite, RelIndexRead, RelIndexReadAll, RelIndexWrite,
        ToRelIndex,
    };
    use streaming_iterator::StreamingIterator;

    use super::{CCol, CCondTrie, CGroupsParIter, CRowsParIter, CollectedParIter, SRow, View};
    use super::{covers, group_rows};
    use crate::index_engine::Rows;
    use crate::index_engine::assign_like_trie::FromRows;
    use crate::index_engine::cond_trie::CondGet;
    use crate::index_engine::decision::Cond;
    use crate::index_engine::locals_trie::DynIter;

    pub type Store<T0, T1, T2, T3, T4> = CCondTrie<T0, T3, T4, T1, T2>;
    type Row<T0, T1, T2, T3, T4> = (T0, T1, T2, T3, T4, Cond);

    /// `(F, Vs, Ps, Vd, Pd)` to `(F, Vd, Pd, Vs, Ps)`.
    #[inline(always)]
    fn row_ref<'a, F, Vs, Ps, Vd, Pd>(
        (f, vs, ps, vd, pd, c): SRow<'a, F, Vs, Ps, Vd, Pd>,
    ) -> SRow<'a, F, Vd, Pd, Vs, Ps> {
        (f, vd, pd, vs, ps, c)
    }
    #[inline(always)]
    fn row_owned<F, Vs, Ps, Vd, Pd>(
        (f, vs, ps, vd, pd, c): (F, Vs, Ps, Vd, Pd, Cond),
    ) -> (F, Vd, Pd, Vs, Ps, Cond) {
        (f, vd, pd, vs, ps, c)
    }
    #[inline(always)]
    fn store_key<T0, T1, T2, T3, T4>(r: &Row<T0, T1, T2, T3, T4>) -> (&T0, &T3, &T4, &T1, &T2) {
        (&r.0, &r.3, &r.4, &r.1, &r.2)
    }
    /// A group's `(Ps, Vd, Pd, Cond)` to `(Vd, Pd, Ps, Cond)`.
    #[inline(always)]
    fn group_val<'a, Ps, Vd, Pd>(
        (ps, vd, pd, c): (&'a Ps, &'a Vd, &'a Pd, &'a Cond),
    ) -> (&'a Vd, &'a Pd, &'a Ps, &'a Cond) {
        (vd, pd, ps, c)
    }

    views!(ToNone, To03, To034, ToFull; store [T0, T3, T4, T1, T2]; group_val [T1, T2, T4]);

    /// The seed rows, all `Always`.
    impl<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol> FromRows<(T0, T1, T2, T3, T4)>
        for Store<T0, T1, T2, T3, T4>
    {
        fn from_rows(rows: Vec<(T0, T1, T2, T3, T4)>) -> Self {
            let store = Self::default();
            for (f, vd, pd, vs, ps) in rows {
                store.always.c_insert(&(f, vs, ps, vd, pd));
            }
            store
        }
    }

    /// The `Always` rows as `(F, Vd, Pd, Vs, Ps)`, consuming the store, which frees as the output
    /// fills: this is the run's peak.
    pub fn into_always_vec<T0: CCol, T1: CCol, T2: CCol, T3: CCol, T4: CCol>(
        store: Store<T0, T1, T2, T3, T4>,
    ) -> Vec<(T0, T1, T2, T3, T4)> {
        let mut always = store.always;
        always.fwd.unfreeze();
        let mut out = Vec::with_capacity(always.len());
        for shard in always.fwd.unfrozen_mut().shards_mut() {
            for ((f, vs), group) in shard.get_mut().drain() {
                for (ps, vd, pd) in group.into_inner() {
                    out.push((f.clone(), vd, pd, vs.clone(), ps));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    // Not `super::*`: the store's `ToRelIndex` in scope makes Ascent's generated calls ambiguous.
    use streaming_iterator::StreamingIterator;

    use super::{assign, locals};
    use crate::index_engine::Rows;
    use crate::index_engine::decision::{Cond, DecisionId, DecisionSet};

    type Row = (u32, u32, u32, u32, u32, Cond);

    // The serial store's test rules, with the stores under `ascent_par!` and the reference
    // lattices under `ascent!`. `step` probes `0_1` and `link` probes `0_1_2`; in `assign` order,
    // `0_3` and `0_3_4`.
    ascent::ascent_par! {
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
    ascent::ascent_par! {
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

    /// Few columns' values, so facts are re-derived under many conditions, often by several
    /// threads in one iteration.
    fn input(rng: &mut Lcg, collapse: bool, vars: u32) -> Input {
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
            prog.seed = $input.seed.iter().copied().collect();
            prog.step = $input.step.iter().copied().collect();
            prog.link = $input.link.iter().copied().collect();
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
    fn the_parallel_store_reaches_the_lattice_fixpoint() {
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
}
