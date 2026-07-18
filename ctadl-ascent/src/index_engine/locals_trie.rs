//! A flat, CSR-style BYODS data structure for the `locals` relation.
//!
//! `locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is the dominant memory
//! consumer of the index phase. As a normal Ascent relation it is stored ~6× over: the
//! physical `Vec`, plus the indices `none`, `0_1`, `0_1_2`, the full existence index
//! `0_1_2_3_4`, and the inverse `0_3_4`. Every index stores its value columns *inline*, so
//! the full 5-column tuple is replicated many times.
//!
//! This module replaces all of that with a single shared store (the Ascent BYODS
//! "ind_common"). Every logical index becomes a lightweight *view* over that one store.
//!
//! ## Why flat, not a nested trie
//!
//! The earlier design stored `(F,V) -> P -> {(M,Fp)}` as `HashMap<(F,V), HashMap<P,
//! HashSet<(M,Fp)>>>`. Measured on a representative workload the groups are *tiny* — ~5
//! leaves per `(F,V)` group spread over ~2 distinct `P` — so the two inner hash levels are
//! nearly empty: each pays hashbrown's 8-bucket minimum allocation to hold ~2 elements.
//! That structural slack, not the payload, was ~91% of the store (36k inner maps ≈ 53%,
//! 70k leaf sets ≈ 38%, only ~13% real data + outer map).
//!
//! We collapse both inner levels into one **sorted `Vec<(P,M,Fp)>` per `(F,V)` group**:
//!   - one heap allocation per group instead of `1 + (#distinct P)` tiny hash tables;
//!   - leaves packed contiguously (24 B each) with no per-element control bytes;
//!   - the vec is kept sorted by `(P,M,Fp)`, so existence is a `binary_search`, the
//!     `0_1_2` view is a `partition_point` range over the contiguous `P`-run, and inserts
//!     keep order with a single shift (groups are ~5 elements, so O(group) is negligible).
//!
//! ## Large groups: promote to a hash set
//!
//! A sorted `Vec` is wrong for the *rare* dense-aliasing function whose `(F,V)` group grows
//! to tens of thousands of leaves. The per-iteration delta->total merge re-copies the whole
//! accumulated group every fixpoint round, so building a group of size `G` costs O(G^2) —
//! the dominant cost on such inputs (profiled at ~55% of index time; see
//! memory-investigation.md §7/§8). Once a group exceeds [`GROUP_HASHSET_THRESHOLD`] it
//! switches to a `HashSet`, making the merge O(delta) (insert only the new leaves) and
//! existence O(1). Small groups keep the `Vec`: one compact allocation, direct sorted
//! `0_1_2` range probe, and the quadratic never bites because they stay tiny.
//!
//! The forward map `(F,V) -> [ (P,M,Fp) ]` serves `none`, `0_1`, `0_1_2`, existence, and
//! iteration. The `0_3_4` view is *derived* by scanning (as before) rather than
//! materializing a full inverse; a small side-index `fidx: F -> {V}` narrows each `0_3_4`
//! probe to the flow-variables of the probed function.
//!
//! Correctness note (differs from `ascent_byods_rels::eqrel`): eqrel tolerates Ascent
//! merging the shared store twice per iteration (once via the ind_common, once via the
//! full-index write target) because union-find merge is idempotent. `locals` is a plain
//! relation, so a double merge would corrupt semi-naive evaluation. We therefore make the
//! *only* real merge live on the ind_common ([`LocalsIndCommon`]); every index write target
//! ([`FullWrite`], [`NoopWrite`]) has a no-op `RelIndexMerge`.

use std::cmp::Ordering;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::Index;
use std::rc::Rc;

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};

type Map<K, V> = hashbrown::HashMap<K, V>;
type Set<T> = hashbrown::HashSet<T>;

/// Merge a sorted, deduplicated `src` into a sorted, deduplicated `dst` (both ascending),
/// keeping `dst` sorted and deduplicated. Returns the number of elements that were *newly*
/// added to `dst` (i.e. present in `src` but not already in `dst`). Linear two-way merge.
fn merge_sorted<T: Ord>(dst: &mut Vec<T>, src: Vec<T>) -> usize {
    if src.is_empty() {
        return 0;
    }
    if dst.is_empty() {
        let n = src.len();
        *dst = src;
        return n;
    }
    let old = std::mem::take(dst);
    let mut merged = Vec::with_capacity(old.len() + src.len());
    let mut oi = old.into_iter().peekable();
    let mut si = src.into_iter().peekable();
    let mut added = 0usize;
    loop {
        match (oi.peek(), si.peek()) {
            (Some(a), Some(b)) => match a.cmp(b) {
                Ordering::Less => merged.push(oi.next().unwrap()),
                Ordering::Greater => {
                    merged.push(si.next().unwrap());
                    added += 1;
                }
                Ordering::Equal => {
                    merged.push(oi.next().unwrap());
                    si.next();
                }
            },
            (Some(_), None) => merged.push(oi.next().unwrap()),
            (None, Some(_)) => {
                merged.push(si.next().unwrap());
                added += 1;
            }
            (None, None) => break,
        }
    }
    *dst = merged;
    added
}

/// Number of distinct elements in the union of two sorted, deduplicated ascending slices
/// (i.e. the length `merge_sorted(dst, src)` would produce). O(m+n), allocation-free.
/// Lets `absorb_group` decide, *before* merging, whether a Small+Small union would exceed
/// `GROUP_HASHSET_THRESHOLD` — so it can build straight into a `HashSet` instead of merging
/// into a `Vec` that `promote()` would immediately deallocate.
fn merge_size<T: Ord>(dst: &[T], src: &[T]) -> usize {
    let mut oi = dst.iter().peekable();
    let mut si = src.iter().peekable();
    let mut count = 0usize;
    loop {
        match (oi.peek(), si.peek()) {
            (Some(a), Some(b)) => {
                match a.cmp(b) {
                    Ordering::Less => {
                        oi.next();
                    }
                    Ordering::Greater => {
                        si.next();
                    }
                    Ordering::Equal => {
                        oi.next();
                        si.next();
                    }
                }
                count += 1;
            }
            (Some(_), None) => {
                count += oi.count();
                break;
            }
            (None, Some(_)) => {
                count += si.count();
                break;
            }
            (None, None) => break,
        }
    }
    count
}

// ---------------------------------------------------------------------------
// A single `(F,V)` group's leaves. `Small` (the overwhelmingly common case, ~5 leaves)
// stays a sorted, deduplicated `Vec`; a group that grows past `GROUP_HASHSET_THRESHOLD`
// switches to a `HashSet` so the per-iteration delta->total merge is O(delta) instead of
// re-copying the whole accumulated group each round (which made large groups O(N^2)).
// ---------------------------------------------------------------------------

/// Groups with more than this many leaves switch from a sorted `Vec` to a `HashSet`.
const GROUP_HASHSET_THRESHOLD: usize = 64;

#[derive(Clone)]
enum Group<P, M, Fp> {
    /// Sorted ascending, deduplicated. Existence = `binary_search`; `0_1_2` = `partition_point`.
    Small(Vec<(P, M, Fp)>),
    /// Unordered, deduplicated. Existence = hash lookup; merge = per-element insert (O(delta)).
    Large(Set<(P, M, Fp)>),
}

/// Iterator over a group's leaves regardless of representation. Order is unspecified for
/// `Large`; every consumer that uses `iter()` is order-independent (the sorted-only `0_1_2`
/// range probe reaches into the variants directly instead).
enum GroupIter<'a, P, M, Fp> {
    Small(std::slice::Iter<'a, (P, M, Fp)>),
    Large(hashbrown::hash_set::Iter<'a, (P, M, Fp)>),
}
impl<'a, P, M, Fp> Iterator for GroupIter<'a, P, M, Fp> {
    type Item = &'a (P, M, Fp);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            GroupIter::Small(i) => i.next(),
            GroupIter::Large(i) => i.next(),
        }
    }
}

// Bounds-light group ops (no `Ord`): iteration and size, used by the read views.
impl<P, M, Fp> Group<P, M, Fp>
where
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline]
    fn len(&self) -> usize {
        match self {
            Group::Small(v) => v.len(),
            Group::Large(s) => s.len(),
        }
    }
    #[inline]
    fn iter(&self) -> GroupIter<'_, P, M, Fp> {
        match self {
            Group::Small(v) => GroupIter::Small(v.iter()),
            Group::Large(s) => GroupIter::Large(s.iter()),
        }
    }
}

// Group ops that maintain / query order need `Ord` on the leaf columns.
impl<P, M, Fp> Group<P, M, Fp>
where
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    #[inline]
    fn new() -> Self {
        Group::Small(Vec::new())
    }

    #[inline]
    fn contains(&self, leaf: &(P, M, Fp)) -> bool {
        match self {
            Group::Small(v) => v.binary_search(leaf).is_ok(),
            Group::Large(s) => s.contains(leaf),
        }
    }

    /// Convert a `Small` group in place to `Large`. No-op if already `Large`.
    fn promote(&mut self) {
        if let Group::Small(v) = self {
            *self = Group::Large(std::mem::take(v).into_iter().collect());
        }
    }

    /// Insert one leaf; returns true if newly added. Promotes to `Large` when a `Small`
    /// group grows past the threshold.
    fn insert(&mut self, leaf: (P, M, Fp)) -> bool {
        match self {
            Group::Small(v) => match v.binary_search(&leaf) {
                Ok(_) => false,
                Err(pos) => {
                    v.insert(pos, leaf);
                    if v.len() > GROUP_HASHSET_THRESHOLD {
                        self.promote();
                    }
                    true
                }
            },
            Group::Large(s) => s.insert(leaf),
        }
    }

    /// Merge `other` into `self` (union). Returns how many leaves were *newly* added to
    /// `self`. Small+Small stays a sorted linear merge (may promote); any case touching a
    /// `Large` side builds the union in a `HashSet` (O(other) inserts, no whole-group
    /// re-copy — this is what removes the O(N^2) merge on dense groups).
    fn absorb_group(&mut self, other: Group<P, M, Fp>) -> usize {
        match (std::mem::replace(self, Group::Small(Vec::new())), other) {
            (Group::Small(mut dst), Group::Small(src)) => {
                if merge_size(&dst, &src) > GROUP_HASHSET_THRESHOLD {
                    // Union would become a Large group: build it straight into a HashSet
                    // rather than merge into a throwaway Vec that promote() would
                    // immediately deallocate.
                    let before = dst.len();
                    let mut set: Set<(P, M, Fp)> = dst.into_iter().collect();
                    for leaf in src {
                        set.insert(leaf);
                    }
                    let added = set.len() - before;
                    *self = Group::Large(set);
                    added
                } else {
                    let added = merge_sorted(&mut dst, src);
                    *self = Group::Small(dst);
                    added
                }
            }
            (this, other) => {
                let (mut set, before) = match this {
                    Group::Large(s) => {
                        let n = s.len();
                        (s, n)
                    }
                    Group::Small(v) => {
                        let s: Set<(P, M, Fp)> = v.into_iter().collect();
                        let n = s.len();
                        (s, n)
                    }
                };
                match other {
                    Group::Small(v) => {
                        for leaf in v {
                            set.insert(leaf);
                        }
                    }
                    Group::Large(s) => {
                        for leaf in s {
                            set.insert(leaf);
                        }
                    }
                }
                let added = set.len() - before;
                *self = Group::Large(set);
                added
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Physical `rel!` storage. Stores no tuples (all data lives in the shared
// ind_common), but tracks the row count so `prog.locals.len()` — the only
// post-run consumer of the physical relation — still reports the true size.
// `push` is invoked exactly once per newly-inserted row by the generated code.
// ---------------------------------------------------------------------------
pub struct CountingVec<T> {
    len: usize,
    _p: PhantomData<T>,
}
impl<T> Default for CountingVec<T> {
    fn default() -> Self {
        Self {
            len: 0,
            _p: PhantomData,
        }
    }
}
impl<T> CountingVec<T> {
    #[inline(always)]
    pub fn push(&mut self, _v: T) {
        self.len += 1;
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline(always)]
    pub fn iter(&self) -> std::iter::Empty<&T> {
        std::iter::empty()
    }
}
impl<T> Index<usize> for CountingVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("locals_trie::CountingVec stores no tuples")
    }
}

// ---------------------------------------------------------------------------
// Clone-able boxed iterator (a local copy of byods' private IteratorFromDyn).
// ---------------------------------------------------------------------------
pub struct DynIter<'a, T> {
    iter: Box<dyn Iterator<Item = T> + 'a>,
    producer: Rc<dyn Fn() -> Box<dyn Iterator<Item = T> + 'a> + 'a>,
}
impl<'a, T> DynIter<'a, T> {
    pub fn new<F, I>(producer: F) -> Self
    where
        F: Fn() -> I + 'a,
        I: Iterator<Item = T> + 'a,
    {
        let producer = Rc::new(move || Box::new(producer()) as Box<dyn Iterator<Item = T> + 'a>);
        let iter = producer();
        Self { iter, producer }
    }
}
impl<T> Iterator for DynIter<'_, T> {
    type Item = T;
    #[inline(always)]
    fn next(&mut self) -> Option<T> {
        self.iter.next()
    }
}
impl<T> Clone for DynIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            iter: (self.producer)(),
            producer: self.producer.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The shared store (Ascent's `ind_common`).
// ---------------------------------------------------------------------------
pub struct LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    /// forward: (F,V) -> sorted `Vec` of `(P, M, Fp)` (ascending, deduplicated). Serves
    /// none / 0_1 / 0_1_2 / existence, and the `0_3_4` view by *scanning*. The vec is kept
    /// sorted so existence is a `binary_search` and `0_1_2` is a `partition_point` range over
    /// the contiguous `P`-run. One heap allocation per group replaces the old nested
    /// `HashMap<P, HashSet<(M,Fp)>>` (which was ~91% hashbrown slack on tiny groups).
    fwd: Map<(F, V), Group<P, M, Fp>>,
    /// side-index: F -> set of V present for that function. Lets a `0_3_4` probe restrict its
    /// scan to the flow-variables of the probed function instead of walking every `(F,V)` group
    /// in `fwd`. Maintained in lockstep with `fwd`'s outer keys (exactly one V per `(F,V)`
    /// group), so it is cheap: one V (8 B + hashbrown slack) per group vs. the multi-GB store.
    fidx: Map<F, Set<V>>,
    len: usize,
}

// Bounds-light methods (no `Ord` needed): callable from the read views, which are only
// bounded `Clone + Eq + Hash`.
impl<F, V, P, M, Fp> LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of distinct `(F, V)` groups — i.e. distinct variables reached by some formal,
    /// since `fwd`'s outer key is exactly the subject of a `locals` row. O(1): the trie
    /// already keys on the prefix a scan of the rows would have to rediscover.
    #[inline]
    pub fn num_reached_variables(&self) -> usize {
        self.fwd.len()
    }

    /// Phase-0 instrumentation: estimate the heap bytes held by the forward store vs. the
    /// `fidx` side-index, so we can see *which* structure dominates before optimizing (external
    /// `phys_footprint` can't attribute bytes to a sub-structure). Estimates are
    /// allocation-size approximations that include hashbrown load-factor slack; they are for
    /// *relative* comparison (fwd vs fidx), not exact accounting. O(rows), one pass.
    pub fn heap_report(&self) -> HeapReport {
        // hashbrown allocates `buckets` slots (a power of two sized so that 7/8*buckets >=
        // capacity), each `size_of::<T>()` bytes, plus one control byte per bucket (+ a
        // group-width mirror). Approximate that from the map's reported `capacity()`.
        fn hb_bytes(capacity: usize, elem: usize) -> usize {
            if capacity == 0 {
                return 0;
            }
            let buckets = (capacity * 8).div_ceil(7).next_power_of_two().max(8);
            buckets * (elem + 1) + 16
        }

        let sz_outer = std::mem::size_of::<((F, V), Group<P, M, Fp>)>();
        let sz_leaf = std::mem::size_of::<(P, M, Fp)>();
        let sz_fidx_outer = std::mem::size_of::<(F, Set<V>)>();
        let sz_fidx_val = std::mem::size_of::<V>();

        let mut r = HeapReport {
            rows: self.len,
            fv_groups: self.fwd.len(),
            p_entries: 0,
            leaf_elems: 0,
            fwd_bytes: hb_bytes(self.fwd.capacity(), sz_outer),
            fidx_funcs: self.fidx.len(),
            fidx_vs: 0,
            fidx_bytes: hb_bytes(self.fidx.capacity(), sz_fidx_outer),
            elem_sizes: (sz_outer, 0, sz_leaf, sz_fidx_outer, sz_fidx_val),
        };
        for group in self.fwd.values() {
            r.leaf_elems += group.len();
            match group {
                Group::Small(vec) => {
                    // Group vec heap buffer: capacity slots of the leaf tuple (no control bytes).
                    r.fwd_bytes += vec.capacity() * sz_leaf;
                    // Distinct P = run boundaries in the sorted vec.
                    let mut last: Option<&P> = None;
                    for (p, _, _) in vec.iter() {
                        if last != Some(p) {
                            r.p_entries += 1;
                            last = Some(p);
                        }
                    }
                }
                Group::Large(set) => {
                    // Hash set buffer: control bytes + leaf slots (with load-factor slack).
                    r.fwd_bytes += hb_bytes(set.capacity(), sz_leaf);
                    // Unordered, so count distinct P with a scratch set.
                    let mut ps: Set<&P> = Set::default();
                    for (p, _, _) in set.iter() {
                        ps.insert(p);
                    }
                    r.p_entries += ps.len();
                }
            }
        }
        for vs in self.fidx.values() {
            r.fidx_vs += vs.len();
            r.fidx_bytes += hb_bytes(vs.capacity(), sz_fidx_val);
        }
        r
    }
}

// Methods that maintain / query the sorted group vecs need `Ord` on the leaf columns.
impl<F, V, P, M, Fp> LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    #[inline]
    fn contains(&self, f: &F, v: &V, p: &P, m: &M, fp: &Fp) -> bool {
        // (P,M,Fp) are cheap to clone (8-byte handles + i16); avoids a borrow-key helper.
        self.fwd
            .get(&(f.clone(), v.clone()))
            .is_some_and(|group| group.contains(&(p.clone(), m.clone(), fp.clone())))
    }

    /// Insert a full tuple; returns true if newly added to *this* store.
    fn insert(&mut self, key: &(F, V, P, M, Fp)) -> bool {
        use hashbrown::hash_map::Entry;
        let (f, v, p, m, fp) = key;
        // Fetch the (F,V) group, recording V in the side-index the first time the group appears.
        let group = match self.fwd.entry((f.clone(), v.clone())) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                self.fidx.entry(f.clone()).or_default().insert(v.clone());
                e.insert(Group::new())
            }
        };
        if group.insert((p.clone(), m.clone(), fp.clone())) {
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Merge `from` into `self` (union). Used by the delta->total move.
    fn absorb(&mut self, from: &mut Self) {
        use hashbrown::hash_map::Entry;
        for ((f, v), fromgroup) in from.fwd.drain() {
            match self.fwd.entry((f.clone(), v.clone())) {
                Entry::Occupied(e) => {
                    self.len += e.into_mut().absorb_group(fromgroup);
                }
                Entry::Vacant(e) => {
                    self.fidx.entry(f).or_default().insert(v);
                    self.len += fromgroup.len();
                    e.insert(fromgroup);
                }
            }
        }
        from.len = 0;
        from.fidx.clear();
    }
}

/// Per-structure byte estimate for the `locals` store (see [`LocalsIndCommon::heap_report`]).
#[derive(Debug, Clone)]
pub struct HeapReport {
    /// Logical row count (== `locals` size).
    pub rows: usize,
    /// Number of distinct `(F,V)` groups (outer forward keys).
    pub fv_groups: usize,
    /// Number of distinct `(F,V,P)` entries across all groups.
    pub p_entries: usize,
    /// Number of `(P,M,Fp)` leaf elements across all groups (== rows).
    pub leaf_elems: usize,
    /// Estimated bytes for the whole forward store (outer map + per-group leaf vecs).
    pub fwd_bytes: usize,
    /// Number of distinct functions in the `fidx` side-index (outer keys).
    pub fidx_funcs: usize,
    /// Number of `V` entries across all `fidx` sets (== distinct `(F,V)` groups == `fv_groups`).
    pub fidx_vs: usize,
    /// Estimated bytes for the `fidx` side-index (outer map + per-function V sets).
    pub fidx_bytes: usize,
    /// Element sizes `(outer, _unused, leaf, fidx_outer, fidx_val)` for reference.
    pub elem_sizes: (usize, usize, usize, usize, usize),
}

impl std::fmt::Display for HeapReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        let total = self.fwd_bytes + self.fidx_bytes;
        let pct = |b: usize| {
            if total == 0 {
                0.0
            } else {
                100.0 * b as f64 / total as f64
            }
        };
        let (o, _i, l, fo, fv) = self.elem_sizes;
        write!(
            f,
            "locals store estimate: total {:.1} MB over {} rows ({:.0} B/row) | \
             fwd {:.1} MB ({:.0}%): {} (F,V) groups, {} (F,V,P) entries, {} leaves | \
             fidx {:.1} MB ({:.0}%): {} funcs, {} V entries | \
             elem sizes o={} l={} fo={} fv={} B",
            mb(total),
            self.rows,
            if self.rows == 0 {
                0.0
            } else {
                total as f64 / self.rows as f64
            },
            mb(self.fwd_bytes),
            pct(self.fwd_bytes),
            self.fv_groups,
            self.p_entries,
            self.leaf_elems,
            mb(self.fidx_bytes),
            pct(self.fidx_bytes),
            self.fidx_funcs,
            self.fidx_vs,
            o,
            l,
            fo,
            fv,
        )
    }
}

impl<F, V, P, M, Fp> Default for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            fwd: Map::default(),
            fidx: Map::default(),
            len: 0,
        }
    }
}

impl<F, V, P, M, Fp> Clone for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    fn clone(&self) -> Self {
        Self {
            fwd: self.fwd.clone(),
            fidx: self.fidx.clone(),
            len: self.len,
        }
    }
}

// The ONE real merge lives here (on the ind_common).
impl<F, V, P, M, Fp> RelIndexMerge for LocalsIndCommon<F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.absorb(from);
    }
    // default merge_delta_to_total_new_to_delta: move delta->total, swap new<->delta.
}

// ---------------------------------------------------------------------------
// Write targets.
// ---------------------------------------------------------------------------

/// No-op write target for the partial (view) indices: data enters the store only through
/// the full index, and merging is done only by the ind_common.
pub struct NoopWrite<K, V>(PhantomData<(K, V)>);
impl<K, V> Default for NoopWrite<K, V> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<K, V> RelIndexWrite for NoopWrite<K, V> {
    type Key = K;
    type Value = V;
    #[inline(always)]
    fn index_insert(&mut self, _key: K, _value: V) {}
}
impl<K, V> RelIndexMerge for NoopWrite<K, V> {
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

/// Write target of the full existence index: performs the real inserts into the store, but
/// has a NO-OP merge (the real merge is the ind_common's, see module docs).
pub struct FullWrite<'a, F, V, P, M, Fp>(&'a mut LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;

impl<'a, F, V, P, M, Fp> RelFullIndexWrite for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    type Key = (F, V, P, M, Fp);
    type Value = ();
    #[inline]
    fn insert_if_not_present(&mut self, key: &Self::Key, _v: ()) -> bool {
        self.0.insert(key)
    }
}
impl<'a, F, V, P, M, Fp> RelIndexWrite for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    type Key = (F, V, P, M, Fp);
    type Value = ();
    #[inline]
    fn index_insert(&mut self, key: Self::Key, _value: ()) {
        self.0.insert(&key);
    }
}
impl<'a, F, V, P, M, Fp> RelIndexMerge for FullWrite<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
}

// ---------------------------------------------------------------------------
// Read views + their ToRelIndex markers.
//
// Each `To*` marker is a zero-sized field in the generated struct; `to_rel_index` borrows
// the shared store to produce a read view, `to_rel_index_write` produces a write target.
// ---------------------------------------------------------------------------

/// Defines a zero-sized `ToRelIndex` marker over the concrete `LocalsIndCommon` store.
/// `$wrty`/`$wrbody` give the write target (real for the full index, no-op for the views).
macro_rules! marker {
    ($to:ident, $view:ident, $wrty:ty, $rel:ident => $wrbody:expr) => {
        pub struct $to<F, V, P, M, Fp>(PhantomData<(F, V, P, M, Fp)>);
        impl<F, V, P, M, Fp> Default for $to<F, V, P, M, Fp> {
            fn default() -> Self {
                Self(PhantomData)
            }
        }
        impl<F, V, P, M, Fp> ToRelIndex<LocalsIndCommon<F, V, P, M, Fp>> for $to<F, V, P, M, Fp>
        where
            F: Clone + Eq + Hash,
            V: Clone + Eq + Hash,
            P: Clone + Eq + Hash,
            M: Clone + Eq + Hash,
            Fp: Clone + Eq + Hash,
        {
            type RelIndex<'a>
                = $view<'a, F, V, P, M, Fp>
            where
                Self: 'a,
                LocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index<'a>(
                &'a self,
                rel: &'a LocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndex<'a> {
                $view(rel)
            }
            type RelIndexWrite<'a>
                = $wrty
            where
                Self: 'a,
                LocalsIndCommon<F, V, P, M, Fp>: 'a;
            #[inline]
            fn to_rel_index_write<'a>(
                &'a mut self,
                $rel: &'a mut LocalsIndCommon<F, V, P, M, Fp>,
            ) -> Self::RelIndexWrite<'a> {
                $wrbody
            }
        }
    };
}

marker!(ToNone, ViewNone, NoopWrite<(), (F, V, P, M, Fp)>, _rel => NoopWrite::default());
marker!(To01, View01, NoopWrite<(F, V), (P, M, Fp)>, _rel => NoopWrite::default());
marker!(To012, View012, NoopWrite<(F, V, P), (M, Fp)>, _rel => NoopWrite::default());
marker!(To034, View034, NoopWrite<(F, M, Fp), (V, P)>, _rel => NoopWrite::default());
marker!(ToFull, ViewFull, FullWrite<'a, F, V, P, M, Fp>, rel => FullWrite(rel));

pub struct ViewNone<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View01<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View012<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct View034<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;
pub struct ViewFull<'a, F, V, P, M, Fp>(&'a LocalsIndCommon<F, V, P, M, Fp>)
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash;

// ---- none: () -> (F,V,P,M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for ViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
        let c = self.0;
        Some(DynIter::new(move || {
            c.fwd
                .iter()
                .flat_map(|((f, v), group)| group.iter().map(move |(p, m, fp)| (f, v, p, m, fp)))
        }))
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
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for ViewNone<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = ();
    type Value = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = std::iter::Once<((), Self::ValueIteratorType)>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        std::iter::once(((), RelIndexRead::index_get(self, &()).unwrap()))
    }
}

// ---- 0_1: (F,V) -> (P,M,Fp) -----------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V);
    type Value = (&'a P, &'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, V)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.get(key)?;
        Some(DynIter::new(move || {
            group.iter().map(|(p, m, fp)| (p, m, fp))
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View01<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V);
    type Value = (&'a P, &'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.iter().map(|((f, v), group)| {
            let it = DynIter::new(move || group.iter().map(|(p, m, fp)| (p, m, fp)));
            ((f, v), it)
        }))
    }
}

// ---- 0_1_2: (F,V,P) -> (M,Fp) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, V, P);
    type Value = (&'a M, &'a Fp);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, V, P)) -> Option<Self::IteratorType> {
        let group = self.0.fwd.get(&(key.0.clone(), key.1.clone()))?;
        let p = key.2.clone();
        match group {
            Group::Small(vec) => {
                // The vec is sorted by (P,M,Fp), so all leaves sharing this P are contiguous.
                let lo = vec.partition_point(|(pp, _, _)| *pp < p);
                let hi = vec.partition_point(|(pp, _, _)| *pp <= p);
                if lo == hi {
                    return None;
                }
                let slice = &vec[lo..hi];
                Some(DynIter::new(move || slice.iter().map(|(_, m, fp)| (m, fp))))
            }
            Group::Large(set) => {
                // Unordered: filter the set to leaves carrying this P. Returns `Some` of a
                // possibly-empty iterator (join-equivalent to `None`); the producer re-scans
                // on each rebuild, which is fine — `0_1_2` is not a hot driver for the dense
                // groups that become `Large`.
                Some(DynIter::new(move || {
                    let p = p.clone();
                    set.iter()
                        .filter_map(move |(pp, m, fp)| (*pp == p).then_some((m, fp)))
                }))
            }
        }
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.fwd.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View012<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V, &'a P);
    type Value = (&'a M, &'a Fp);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.iter().flat_map(|((f, v), group)| {
            // Emit one key per distinct (F,V,P), each with all its (M,Fp) leaves.
            let inner: Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a> =
                match group {
                    Group::Small(vec) => {
                        // Sorted: split into contiguous runs of equal P.
                        let mut runs: Vec<(usize, usize)> = Vec::new();
                        let mut i = 0;
                        while i < vec.len() {
                            let p0 = &vec[i].0;
                            let mut j = i + 1;
                            while j < vec.len() && &vec[j].0 == p0 {
                                j += 1;
                            }
                            runs.push((i, j));
                            i = j;
                        }
                        Box::new(runs.into_iter().map(move |(s, e)| {
                            let slice = &vec[s..e];
                            let p = &vec[s].0;
                            let it = DynIter::new(move || slice.iter().map(|(_, m, fp)| (m, fp)));
                            ((f, v, p), it)
                        }))
                    }
                    Group::Large(set) => {
                        // Unordered: bucket leaves by P into borrowed lists, one key per P.
                        let mut byp: Map<&'a P, Vec<(&'a M, &'a Fp)>> = Map::default();
                        for (p, m, fp) in set.iter() {
                            byp.entry(p).or_default().push((m, fp));
                        }
                        Box::new(byp.into_iter().map(move |(p, mfs)| {
                            let it = DynIter::new(move || mfs.clone().into_iter());
                            ((f, v, p), it)
                        }))
                    }
                };
            inner
        }))
    }
}

// ---- 0_3_4: (F,M,Fp) -> (V,P) ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for View034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (F, M, Fp);
    type Value = (&'a V, &'a P);
    type IteratorType = DynIter<'a, Self::Value>;
    #[inline]
    fn index_get(&'a self, key: &(F, M, Fp)) -> Option<Self::IteratorType> {
        // Derived (no materialized inverse): for the probed function `f`, visit only its
        // flow-variables via the `fidx` side-index, then yield every `(v, p)` whose group vec
        // contains a leaf with this `(m, fp)`. This touches one function's `(F,V)` groups
        // instead of scanning all of `fwd`. Cold path (rule 2.2, ~tens of probes). Returns
        // `Some` of a possibly-empty iterator; an empty result is join-equivalent to `None`.
        let c = self.0;
        let (f, m, fp) = key.clone();
        Some(DynIter::new(move || {
            let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
            // `fidx[f]` and `fwd` are maintained in lockstep, so every V here has an `fwd` group.
            c.fidx.get(&f).into_iter().flat_map(move |vs| {
                let (f, m, fp) = (f.clone(), m.clone(), fp.clone());
                vs.iter().flat_map(move |v| {
                    let (m, fp) = (m.clone(), fp.clone());
                    c.fwd
                        .get(&(f.clone(), v.clone()))
                        .into_iter()
                        .flat_map(move |group| {
                            let (m, fp) = (m.clone(), fp.clone());
                            group.iter().filter_map(move |(p, mm, ffp)| {
                                if *mm == m && *ffp == fp {
                                    Some((v, p))
                                } else {
                                    None
                                }
                            })
                        })
                })
            })
        }))
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        // No materialized inverse to size. `0_3_4` is only ever probed (never a join driver),
        // so a large estimate is safe — it just discourages the planner from choosing it as a
        // driver, which is exactly what we want. Use the row count as a conservative upper bound.
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for View034<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a M, &'a Fp);
    type Value = (&'a V, &'a P);
    type ValueIteratorType = DynIter<'a, Self::Value>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    fn iter_all(&'a self) -> Self::AllIteratorType {
        // Not on any hot path: `0_3_4` is only ever point-probed (rule 2.2), never iterated as a
        // driver. Provide a correct fallback by transiently inverting `fwd` on demand. If this
        // ever shows up in a profile, it means some rule started iterating `0_3_4` and a
        // materialized inverse should be reconsidered.
        let mut groups: Map<(&'a F, &'a M, &'a Fp), Vec<(&'a V, &'a P)>> = Map::default();
        for ((f, v), group) in self.0.fwd.iter() {
            for (p, m, fp) in group.iter() {
                groups.entry((f, m, fp)).or_default().push((v, p));
            }
        }
        Box::new(groups.into_iter().map(|(key, vps)| {
            let it = DynIter::new(move || vps.clone().into_iter());
            (key, it)
        }))
    }
}

// ---- full 0_1_2_3_4: existence ---------------------------------------------
impl<'a, F, V, P, M, Fp> RelFullIndexRead<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    type Key = (F, V, P, M, Fp);
    #[inline]
    fn contains_key(&'a self, key: &Self::Key) -> bool {
        self.0.contains(&key.0, &key.1, &key.2, &key.3, &key.4)
    }
}
impl<'a, F, V, P, M, Fp> RelIndexRead<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash + Ord,
    M: Clone + Eq + Hash + Ord,
    Fp: Clone + Eq + Hash + Ord,
{
    type Key = (F, V, P, M, Fp);
    type Value = &'a ();
    type IteratorType = std::iter::Once<&'a ()>;
    #[inline]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        if self.0.contains(&key.0, &key.1, &key.2, &key.3, &key.4) {
            Some(std::iter::once(&()))
        } else {
            None
        }
    }
    #[inline]
    fn len_estimate(&self) -> usize {
        self.0.len()
    }
}
impl<'a, F, V, P, M, Fp> RelIndexReadAll<'a> for ViewFull<'a, F, V, P, M, Fp>
where
    F: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
    P: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    Fp: Clone + Eq + Hash,
{
    type Key = (&'a F, &'a V, &'a P, &'a M, &'a Fp);
    type Value = &'a ();
    type ValueIteratorType = std::iter::Once<&'a ()>;
    type AllIteratorType = Box<dyn Iterator<Item = (Self::Key, Self::ValueIteratorType)> + 'a>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        Box::new(self.0.fwd.iter().flat_map(|((f, v), group)| {
            group
                .iter()
                .map(move |(p, m, fp)| ((f, v, p, m, fp), std::iter::once(&())))
        }))
    }
}

// ---------------------------------------------------------------------------
// The BYODS provider macros. Codegen calls these as
//   locals_trie::rel!(Name, (cols), [inds], ser, (args))
//   locals_trie::rel_ind_common!(Name, (cols), [inds], ser, (args))
//   locals_trie::rel_full_ind!(Name, (cols), [inds], ser, (args), Key, Val)
//   locals_trie::rel_ind!(Name, (cols), [inds], ser, (args), [subset], Key, Val)
//   locals_trie::rel_codegen!(Name, (cols), [inds], ser, (args))
// ---------------------------------------------------------------------------

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_codegen {
    ($($tt:tt)*) => {};
}
pub use locals_trie_rel_codegen as rel_codegen;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::locals_trie::CountingVec<($f, $v, $p, $m, $fp)>
    };
}
pub use locals_trie_rel as rel;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_ind_common {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt) => {
        $crate::index_engine::locals_trie::LocalsIndCommon<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_ind_common as rel_ind_common;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_full_ind {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::ToFull<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_full_ind as rel_full_ind;

#[doc(hidden)]
#[macro_export]
macro_rules! locals_trie_rel_ind {
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::ToNone<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 1], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To01<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 1, 2], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To012<$f, $v, $p, $m, $fp>
    };
    ($name:ident, ($f:ty, $v:ty, $p:ty, $m:ty, $fp:ty), $inds:tt, $par:ident, $args:tt, [0, 3, 4], $key:ty, $val:ty) => {
        $crate::index_engine::locals_trie::To034<$f, $v, $p, $m, $fp>
    };
}
pub use locals_trie_rel_ind as rel_ind;

#[cfg(test)]
mod tests {
    use super::{merge_size, merge_sorted};

    // Build a sorted, deduplicated Vec from arbitrary ints (the invariant both fns require).
    fn sorted_dedup(mut v: Vec<i32>) -> Vec<i32> {
        v.sort_unstable();
        v.dedup();
        v
    }

    fn check(a: Vec<i32>, b: Vec<i32>) {
        let a = sorted_dedup(a);
        let b = sorted_dedup(b);
        let predicted = merge_size(&a, &b);
        let mut merged = a.clone();
        let added = merge_sorted(&mut merged, b.clone());
        assert_eq!(
            predicted,
            merged.len(),
            "merge_size must equal merged length"
        );
        assert_eq!(
            added,
            merged.len() - a.len(),
            "added must equal newly-inserted count"
        );
    }

    #[test]
    fn merge_size_matches_merge_sorted() {
        check(vec![], vec![]);
        check(vec![1, 2, 3], vec![]);
        check(vec![], vec![4, 5]);
        check(vec![1, 2, 3], vec![1, 2, 3]); // full overlap
        check(vec![1, 3, 5], vec![2, 4, 6]); // interleaved, disjoint
        check(vec![1, 2, 3], vec![3, 4, 5]); // partial overlap
        // A small deterministic sweep (no rand dep) covering many overlap shapes.
        for i in 0..16u32 {
            let a = (0..8)
                .filter(|k| (i >> (k % 4)) & 1 == 0)
                .map(|k| k as i32)
                .collect();
            let b = (0..8)
                .filter(|k| (i >> (k % 3)) & 1 == 1)
                .map(|k| (k as i32) + 2)
                .collect();
            check(a, b);
        }
    }
}
