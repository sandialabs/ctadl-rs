//! A set that changes representation as it grows. While it is small it is a **linear-probing**
//! open-addressed table; once it is large it is a **Swiss table**. Either way it stays *one*
//! structure, not an enum over two.
//!
//! A Datalog index on `K` needs a `Map<K, Set<V>>`. This is the `Set<V>` half of that. In the
//! workloads that motivate it, the number of values per key spans four orders of magnitude: most
//! keys hold a couple of values, and a few hold thousands. No single representation is right for
//! both ends.
//!
//! * A Swiss table is wrong for 2 elements. Its bucket count is a power of two, held at 87.5%
//!   load or less, and it adds one control byte per bucket plus a group mirror. So 48 B of
//!   payload — two 24 B leaves — costs 108 B in hashbrown, which rounds up to its 4-bucket
//!   floor, and 208 B in the table here, which has no 4-bucket regime and so rounds up to one
//!   8-bucket group. A real index holds about 400k such tiny sets, and at that multiple the
//!   slack *is* the store.
//! * A linear scan is wrong for 10 000 elements. So is anything that re-copies the whole set
//!   when a delta is merged into it. That is what made large groups quadratic to build.
//!
//! So a [`HybridSet`] probes linearly up to [`SMALL_THRESHOLD`] elements, and switches to Swiss
//! probing above that. The spec this implements asks us to write the upper representation here
//! rather than take it from a library. [`swiss`] is therefore a SwissTable built from scratch,
//! with control bytes, word-parallel group scans, and hashbrown's sizing rules. Its own module
//! docs describe it, including the three places where it deliberately parts company with
//! hashbrown.
//!
//! ## Not an enum
//!
//! The switch is **not** a tagged union. [`raw::RawTable`] is a single structure. Its four
//! fields mean one thing below the threshold and another above it, and it reads the regime off
//! the capacity it already has to store:
//!
//! ```text
//!   HybridSet = { ptr, cap, len } + hasher (zero-sized by default) = 16 bytes
//!
//!   small (cap <= 64):           large (cap > 64):
//!   [ T3 T2 T1 T0 ][ u64 ]       [ .. T1 T0 ][ ctrl bytes | mirror ]
//!                   ^ptr                      ^ptr
//!   1 occupancy bit per slot     1 control byte per bucket
//!
//!   is_large()  =  cap > SMALL_THRESHOLD
//! ```
//!
//! Both regimes keep their metadata at `ptr` and their elements below it. So one allocation
//! routine, one element addressing rule, one `Drop`, one `Clone`, one growth path, and one
//! iterator serve both, and `len` and `capacity` do not branch at all. `raw`'s module docs give
//! the full argument. The short version is that the enum this replaces cost a discriminant on
//! every access, two of every impl, and 16 bytes on **every** entry of the enclosing map,
//! whether or not that entry ever grew past two elements.
//!
//! ## Choosing the threshold
//!
//! [`SMALL_THRESHOLD`] is 64: the spec's value, and the largest a `u64` occupancy bitmask
//! admits. It is not hard-wired, though. It is the default of the `SMALL` const parameter, so a
//! measurement can sweep it without editing the structure. Setting `SMALL = 0` gives a pure
//! Swiss table, which is the A/B the `hybrid_set` bench runs against `hashbrown`.
//!
//! What the threshold decides is **how much of the size distribution pays for promotion**, and
//! promotion roughly doubles the bytes a set spends per element: a Swiss table holds a 24 B
//! element in a power-of-two bucket array at 87.5% load or less, which comes to about 50 B per
//! element against the probe table's 24 B. Without removals, a set that crosses the threshold
//! never comes back, so that step is never refunded. Nothing pulls the other way on memory, so
//! a lower threshold strictly loses: it would move every set of 33 to 64 elements from 24 to
//! about 50 B per element.
//!
//! What a lower threshold *would* buy is the one case this design is measurably worse at — a
//! miss against an exactly-full probe table, which degenerates to a scan and halves from about
//! 45 ns at 64 slots to about 21 ns at 32. We do not take that trade, because load factor 1.0 is
//! what buys the 24 B per element in the first place, and because the workload that manufactures
//! exactly that shape — every group holding exactly 64 leaves — is the *fastest* configuration
//! in the store-level sweep, not the slowest.
//!
//! ## Transitioning
//!
//! Crossing the threshold takes one pass over at most [`SMALL_THRESHOLD`] elements. We size the
//! new table for the final element count, which we already know, so it never rehashes while
//! being filled, and we **move** elements out of the small buffer rather than clone them. This
//! is the same `rebuild` that ordinary growth uses; see [`raw`]. Nothing else is touched. The
//! transition stays local to one set, never walks the enclosing map, and never happens twice for
//! the same set, because without removals a set's size only grows.
//!
//! [`HybridSet::merge`] gets the same treatment from the other direction. The union of two sets
//! is commutative, so `merge` inserts the *smaller* side into the larger. A merge therefore
//! costs O(min(|a|, |b|)) lookups, whichever side the caller passed as `self`.

use std::fmt;
use std::hash::{BuildHasher, BuildHasherDefault, Hash};

use rustc_hash::FxHasher;

pub mod raw;
pub mod swiss;

pub use raw::{IntoIter, Iter, MAX_SMALL_THRESHOLD};

use raw::RawTable;

/// The default hasher. The store keys are trusted ids derived from the program, so we hash them
/// with the fast, deterministic `FxHasher` instead of the DoS-resistant SipHash the std
/// collections use.
pub type DefaultHashBuilder = BuildHasherDefault<FxHasher>;

/// The default element count at which a set switches from linear probing to Swiss probing. A set
/// holding exactly this many elements is still a probe table. The element that would take it to
/// `SMALL_THRESHOLD + 1` is what promotes it.
///
/// See the module docs for why we chose 64. The value must be 0, or a power of two no greater
/// than [`MAX_SMALL_THRESHOLD`]. [`raw::RawTable`] asserts that at compile time.
pub const SMALL_THRESHOLD: usize = 64;

/// A set of `T` in one 16-byte structure. It is a linear-probing table while it holds at most
/// `SMALL` elements, and a Swiss table above that. See the module docs.
///
/// `SMALL` exists so that a benchmark can sweep the threshold. `0` means "always a Swiss table".
pub struct HybridSet<T, S = DefaultHashBuilder, const SMALL: usize = SMALL_THRESHOLD> {
    table: RawTable<T, SMALL>,
    /// Zero-sized for the default hasher, so it costs nothing per set.
    hasher: S,
}

impl<T, S: Default, const SMALL: usize> Default for HybridSet<T, S, SMALL> {
    fn default() -> Self {
        Self {
            table: RawTable::new(),
            hasher: S::default(),
        }
    }
}

impl<T, S: Default, const SMALL: usize> HybridSet<T, S, SMALL> {
    /// An empty set. Does not allocate.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T, S, const SMALL: usize> HybridSet<T, S, SMALL> {
    /// Number of elements. Does not branch on the representation.
    #[inline]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many elements this set can hold before it grows or changes representation. In the
    /// small representation that is the allocated slot count, because the load factor may reach
    /// 1.0.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Whether this set has crossed `SMALL` and is now a Swiss table.
    #[inline]
    pub fn is_large(&self) -> bool {
        self.table.is_large()
    }

    /// Elements, in unspecified order.
    #[inline]
    pub fn iter(&self) -> Iter<'_, T, SMALL> {
        self.table.iter()
    }

    /// Heap bytes this set holds, including whatever slack its representation carries. While the
    /// set is small that is the element slots plus one 8-byte occupancy word. Once it is large
    /// it is the Swiss layout: buckets, control bytes, and the group mirror.
    ///
    /// Either way this is the allocation the set actually made, not an estimate. The enclosing
    /// map still needs [`super::locals_trie::hb_bytes`] to model hashbrown's layout, but this
    /// structure computes its own layout, so it can simply report it.
    pub fn heap_bytes(&self) -> usize {
        self.table.heap_bytes()
    }
}

impl<T, S, const SMALL: usize> HybridSet<T, S, SMALL>
where
    T: Hash + Eq,
    S: BuildHasher + Default,
{
    /// An empty set sized for `capacity` elements. It is a probe table if that many elements fit
    /// under the threshold, and a Swiss table straight away if they do not. A caller that knows
    /// the final size can use this to skip the transition entirely.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            table: RawTable::with_capacity(capacity),
            hasher: S::default(),
        }
    }

    #[inline]
    fn hash(&self, value: &T) -> u64 {
        self.hasher.hash_one(value)
    }

    #[inline]
    pub fn contains(&self, value: &T) -> bool {
        let hash = self.hash(value);
        self.table.find(hash, |x| x == value).is_some()
    }

    /// Insert `value`. Returns whether it was newly added.
    #[inline]
    pub fn insert(&mut self, value: T) -> bool {
        let Self { table, hasher } = self;
        let hash = hasher.hash_one(&value);
        table.insert(hash, value, |x| hasher.hash_one(x))
    }

    /// Make room for `additional` more elements. If that many more would not fit in the small
    /// representation, this changes representation up front.
    ///
    /// Do not use this if you only have an *upper bound* on the additional count, as you would
    /// for a union whose overlap is unknown. Over-reserving here permanently promotes a set that
    /// might have stayed small. [`Self::merge`] deliberately does not call it.
    pub fn reserve(&mut self, additional: usize) {
        let Self { table, hasher } = self;
        table.reserve(additional, |x| hasher.hash_one(x));
    }

    /// Union `other` into `self`. Returns how many elements were **newly added to `self`**.
    ///
    /// This costs O(min(|self|, |other|)) lookups. A union is commutative, so if `other` is the
    /// bigger side we swap the two and insert the smaller one into it. That is what keeps a
    /// delta->total merge proportional to the delta rather than to the accumulated total,
    /// whichever way round the caller has them.
    pub fn merge(&mut self, mut other: Self) -> usize {
        if other.is_empty() {
            return 0;
        }
        let before = self.len();
        if before == 0 {
            *self = other;
            return self.len();
        }
        if other.len() > before {
            std::mem::swap(self, &mut other);
        }
        for value in other {
            self.insert(value);
        }
        // Either way, `self` now holds the union. `before` is the size of the set the caller
        // passed as `self`, which is what we measure "newly added to self" against.
        self.len() - before
    }
}

impl<T, S, const SMALL: usize> Clone for HybridSet<T, S, SMALL>
where
    T: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
            hasher: self.hasher.clone(),
        }
    }
}

impl<T, S, const SMALL: usize> Extend<T> for HybridSet<T, S, SMALL>
where
    T: Hash + Eq,
    S: BuildHasher + Default,
{
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for value in iter {
            self.insert(value);
        }
    }
}

impl<T, S, const SMALL: usize> FromIterator<T> for HybridSet<T, S, SMALL>
where
    T: Hash + Eq,
    S: BuildHasher + Default,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        // The lower bound counts elements *before* dedup, so it is only a hint. Sizing to it
        // avoids the transition when the caller knows the set is already large.
        let mut set = Self::with_capacity(iter.size_hint().0);
        set.extend(iter);
        set
    }
}

impl<T: fmt::Debug, S, const SMALL: usize> fmt::Debug for HybridSet<T, S, SMALL> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<'a, T, S, const SMALL: usize> IntoIterator for &'a HybridSet<T, S, SMALL> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T, SMALL>;
    fn into_iter(self) -> Iter<'a, T, SMALL> {
        self.iter()
    }
}

impl<T, S, const SMALL: usize> IntoIterator for HybridSet<T, S, SMALL> {
    type Item = T;
    type IntoIter = IntoIter<T, SMALL>;
    fn into_iter(self) -> IntoIter<T, SMALL> {
        self.table.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::hash::{BuildHasher, Hash};
    use std::rc::Rc;

    use super::super::locals_trie::{HB_GROUP_WIDTH, hb_buckets, hb_bytes};
    use super::raw::slot_count_for;
    use super::{DefaultHashBuilder, HybridSet, SMALL_THRESHOLD};

    type Set = HybridSet<u64>;

    /// The same structure with the small regime switched off, giving a pure Swiss table. That is
    /// what the hashbrown-parity tests below measure.
    type Swiss<T> = HybridSet<T, DefaultHashBuilder, 0>;

    /// A cheap, deterministic pseudo-random sequence, so we need no `rand` dependency.
    fn lcg(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s >> 11
        }
    }

    /// Checks that we can find every element we inserted, that we cannot find any element we did
    /// not, and that the size and the iteration agree with a `BTreeSet` built from the same
    /// inserts.
    fn model_check(values: &[u64]) {
        let mut set = Set::new();
        let mut model = BTreeSet::new();
        for (i, &v) in values.iter().enumerate() {
            let added = set.insert(v);
            assert_eq!(added, model.insert(v), "insert {v} at step {i} disagreed");
            assert_eq!(set.len(), model.len(), "len after {i} inserts");
            assert_eq!(
                set.is_large(),
                model.len() > SMALL_THRESHOLD,
                "representation after {i} inserts (len {})",
                model.len()
            );
        }
        for v in &model {
            assert!(set.contains(v), "{v} was inserted but is not found");
        }
        let iterated: BTreeSet<u64> = set.iter().copied().collect();
        assert_eq!(iterated, model, "iteration must yield exactly the elements");
        let owned: BTreeSet<u64> = set.clone().into_iter().collect();
        assert_eq!(owned, model, "into_iter must yield exactly the elements");
        let cloned: BTreeSet<u64> = set.clone().iter().copied().collect();
        assert_eq!(cloned, model, "clone must copy exactly the elements");
        assert!(set.capacity() >= set.len());
    }

    /// The same checks, against a set that is a Swiss table at every size.
    fn swiss_model_check(values: &[u64]) {
        let mut set = Swiss::new();
        let mut model = BTreeSet::new();
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(set.insert(v), model.insert(v), "insert {v} at step {i}");
            assert_eq!(set.len(), model.len(), "len after {i} inserts");
            assert!(set.is_large() || model.is_empty(), "must never be small");
            assert!(
                set.len() <= set.capacity(),
                "step {i}: {} elements in a table with capacity {}",
                set.len(),
                set.capacity()
            );
        }
        for v in &model {
            assert!(set.contains(v), "{v} was inserted but is not found");
        }
        assert_eq!(
            set.iter().copied().collect::<BTreeSet<_>>(),
            model,
            "iteration must yield exactly the elements"
        );
        let cloned = set.clone();
        for v in &model {
            assert!(cloned.contains(v), "{v} is missing from the clone");
        }
        assert_eq!(
            set.into_iter().collect::<BTreeSet<_>>(),
            model,
            "into_iter must yield exactly the elements"
        );
    }

    #[test]
    fn matches_a_set_model_across_the_threshold() {
        model_check(&[]);
        model_check(&[7]);
        model_check(&[7, 7, 7]);
        // Dense low values, so probe sequences wrap and collide constantly.
        model_check(&(0..100u64).collect::<Vec<_>>());
        // Ideally every element would hash to the same slot in a power-of-two table. The
        // multiples below at least share low bits after FxHash mixing.
        model_check(&(0..80u64).map(|i| i * 64).collect::<Vec<_>>());
        // At the threshold and on either side of it, where the representation flips.
        for n in [
            SMALL_THRESHOLD - 1,
            SMALL_THRESHOLD,
            SMALL_THRESHOLD + 1,
            SMALL_THRESHOLD + 2,
        ] {
            model_check(&(0..n as u64).collect::<Vec<_>>());
        }
        // Random, with duplicates.
        let mut next = lcg(12345);
        for len in [1usize, 5, 31, 33, 200, 1000] {
            let values: Vec<u64> = (0..len).map(|_| next() % (len as u64 * 2)).collect();
            model_check(&values);
        }
    }

    /// Exercises the large regime on its own, starting from empty. An ordinary `HybridSet`
    /// reaches this path only after promotion, so it needs its own coverage at small sizes.
    #[test]
    fn matches_a_set_model_with_no_small_regime() {
        swiss_model_check(&[]);
        swiss_model_check(&[7, 7, 7]);
        swiss_model_check(&(0..300u64).collect::<Vec<_>>());
        // Values whose low bits collide after mixing, which forces heavy probing.
        swiss_model_check(&(0..200u64).map(|i| i * 4096).collect::<Vec<_>>());
        // Every size across the first few growth steps.
        for n in 0..40u64 {
            swiss_model_check(&(0..n).collect::<Vec<_>>());
        }
        let mut next = lcg(0x5eed);
        for len in [7usize, 8, 9, 56, 57, 1000] {
            let values: Vec<u64> = (0..len).map(|_| next() % (len as u64 * 2)).collect();
            swiss_model_check(&values);
        }
    }

    #[test]
    fn absent_elements_are_not_found_at_every_size() {
        // A full small table has no empty slot for a probe to stop at, so the "not present"
        // answer has to come from the bounded scan. Check that at every size up to promotion
        // and past it.
        for n in 0..=(SMALL_THRESHOLD + 4) as u64 {
            let set: Set = (0..n).collect();
            assert_eq!(set.len() as u64, n);
            for absent in n..n + 8 {
                assert!(!set.contains(&absent), "n={n}: {absent} must be absent");
            }
        }
        // Now the same, for a table that is Swiss at every size.
        for n in 0..=200u64 {
            let mut set: Swiss<u64> = Swiss::new();
            for i in 0..n {
                set.insert(i * 3);
            }
            for i in 0..n {
                assert!(set.contains(&(i * 3)), "n={n}: {} must be present", i * 3);
                assert!(
                    !set.contains(&(i * 3 + 1)),
                    "n={n}: {} must be absent",
                    i * 3 + 1
                );
            }
            assert!(!set.contains(&u64::MAX));
        }
    }

    #[test]
    fn merge_is_a_union_and_reports_new_elements() {
        let cases: [(std::ops::Range<u64>, std::ops::Range<u64>); 7] = [
            (0..0, 0..5),
            (0..5, 0..0),
            (0..5, 5..10),   // disjoint, both small
            (0..5, 0..5),    // identical
            (0..5, 3..40),   // small + large, overlapping: promotes
            (0..40, 38..45), // large + small
            (0..40, 20..90), // large + large
        ];
        for (a, b) in cases {
            let mut set: Set = a.clone().collect();
            let other: Set = b.clone().collect();
            let added = set.merge(other);
            let expected: BTreeSet<u64> = a.clone().chain(b.clone()).collect();
            let a_len = a.clone().count();
            assert_eq!(set.len(), expected.len(), "union size for {a:?} + {b:?}");
            assert_eq!(
                added,
                expected.len() - a_len,
                "newly-added count for {a:?} + {b:?}"
            );
            assert_eq!(
                set.iter().copied().collect::<BTreeSet<_>>(),
                expected,
                "union contents for {a:?} + {b:?}"
            );
            assert_eq!(set.is_large(), set.len() > SMALL_THRESHOLD);
        }
    }

    #[test]
    fn small_representation_allocates_only_slots_and_one_word() {
        // This is what the bitmask buys us. A small set's heap cost is its slots plus a single
        // 8-byte occupancy word, with no per-element control bytes. Slot counts double from 1.
        for (n, slots) in [
            (0usize, 0usize),
            (1, 1),
            (2, 2),
            (3, 4),
            (4, 4),
            (5, 8),
            (8, 8),
            (9, 16),
            (32, 32),
        ] {
            let set: Set = (0..n as u64).collect();
            assert_eq!(set.capacity(), slots, "n={n}");
            let expected = if slots == 0 {
                0
            } else {
                slots * std::mem::size_of::<u64>() + std::mem::size_of::<u64>()
            };
            assert_eq!(
                set.heap_bytes(),
                expected,
                "n={n}: heap bytes must be the slots plus the occupancy word"
            );
            assert!(!set.is_large(), "n={n} must stay small");
        }
        assert_eq!(slot_count_for(0, SMALL_THRESHOLD), 0);
        assert_eq!(slot_count_for(1, SMALL_THRESHOLD), 1);
        assert_eq!(slot_count_for(33, SMALL_THRESHOLD), 64);
        assert_eq!(slot_count_for(1000, SMALL_THRESHOLD), SMALL_THRESHOLD);
        assert_eq!(slot_count_for(4, 0), 0, "a zero threshold is never small");
    }

    /// This is what the exercise is for. Once the table is large, its bucket counts must be
    /// hashbrown's, at every size a real hashbrown table steps through. The one exception is
    /// below a single group, where this table starts at 8 buckets rather than 4.
    #[test]
    fn bucket_counts_track_hashbrown() {
        let mut theirs: hashbrown::HashSet<u64, DefaultHashBuilder> = hashbrown::HashSet::default();
        let mut ours: Swiss<u64> = Swiss::new();
        assert_eq!(ours.capacity(), 0, "an unallocated table holds nothing");
        for i in 0..2000u64 {
            theirs.insert(i);
            ours.insert(i);
            let hb = hb_buckets(theirs.capacity());
            if hb < super::swiss::GROUP_WIDTH {
                // This is hashbrown's 4-bucket table, which we deliberately do not have.
                continue;
            }
            // Wherever hashbrown's group is also 8 bytes wide, our allocation matches
            // hashbrown's byte for byte.
            if HB_GROUP_WIDTH == super::swiss::GROUP_WIDTH {
                assert_eq!(
                    ours.heap_bytes(),
                    hb_bytes(theirs.capacity(), std::mem::size_of::<u64>()),
                    "after {} elements: allocation size differs",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn with_capacity_skips_the_transition() {
        let small: Set = Set::with_capacity(SMALL_THRESHOLD);
        assert!(!small.is_large());
        let large: Set = Set::with_capacity(SMALL_THRESHOLD + 1);
        assert!(large.is_large(), "sized past the threshold, start large");
        assert!(large.capacity() > SMALL_THRESHOLD);
        // It must also stay usable and correct.
        let mut large = large;
        for i in 0..100 {
            assert!(large.insert(i));
        }
        assert_eq!(large.len(), 100);

        // Sizing up front must also mean no further growth.
        for capacity in [1usize, 7, 8, 100, 1000] {
            let mut set: Set = Set::with_capacity(capacity);
            let cap = set.capacity();
            assert!(cap >= capacity);
            for i in 0..capacity as u64 {
                set.insert(i);
            }
            assert_eq!(set.capacity(), cap, "capacity {capacity} grew anyway");
        }
    }

    #[test]
    fn reserve_promotes_only_when_it_must() {
        let mut set: Set = (0..4).collect();
        assert_eq!(set.capacity(), 4);
        set.reserve(SMALL_THRESHOLD - 4);
        assert!(!set.is_large());
        assert_eq!(set.capacity(), SMALL_THRESHOLD);
        assert_eq!(set.len(), 4);
        set.reserve(SMALL_THRESHOLD);
        assert!(set.is_large());
        assert_eq!(set.len(), 4, "reserve must not disturb the contents");
        for i in 0..4 {
            assert!(set.contains(&i));
        }
        // A reserve that is already satisfied must not rebuild.
        let cap = set.capacity();
        set.reserve(1);
        assert_eq!(set.capacity(), cap);

        // A large reserve must hold too.
        let mut set: Set = (0..10).collect();
        set.reserve(500);
        let cap = set.capacity();
        assert!(cap >= 510);
        for i in 10..510u64 {
            set.insert(i);
        }
        assert_eq!(set.capacity(), cap, "reserve did not hold");
        assert_eq!(set.len(), 510);
        for i in 0..510u64 {
            assert!(set.contains(&i));
        }
    }

    /// Elements with a destructor must be dropped exactly once. That holds for either
    /// representation, and through every path that moves them: growth, promotion, `into_iter`,
    /// and drop.
    #[test]
    fn drops_each_element_exactly_once() {
        #[derive(Clone)]
        struct Counted(u64, Rc<RefCell<Vec<u64>>>);
        impl PartialEq for Counted {
            fn eq(&self, other: &Self) -> bool {
                self.0 == other.0
            }
        }
        impl Eq for Counted {}
        impl Hash for Counted {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.0.hash(state)
            }
        }
        impl Drop for Counted {
            fn drop(&mut self) {
                self.1.borrow_mut().push(self.0);
            }
        }

        for n in [1u64, 5, 31, 32, 33, 100] {
            let log = Rc::new(RefCell::new(Vec::new()));
            {
                let mut set: HybridSet<Counted> = HybridSet::new();
                for i in 0..n {
                    // Insert each value twice. The set drops the duplicate immediately.
                    set.insert(Counted(i, log.clone()));
                    set.insert(Counted(i, log.clone()));
                }
                assert_eq!(set.len() as u64, n);
                // So far the set has dropped the duplicates and nothing else.
                let mut so_far = log.borrow().clone();
                so_far.sort_unstable();
                assert_eq!(
                    so_far,
                    (0..n).collect::<Vec<_>>(),
                    "n={n}: duplicates dropped"
                );
            }
            let mut all = log.borrow().clone();
            all.sort_unstable();
            let mut expected: Vec<u64> = (0..n).chain(0..n).collect();
            expected.sort_unstable();
            assert_eq!(all, expected, "n={n}: every element dropped exactly once");
        }

        // The same again, but `into_iter` consumes the set and the caller drops the elements it
        // yields. The remainder, which the iterator never yielded, must still be dropped exactly
        // once. We run this on both sides of the threshold.
        for n in [10u64, 50, 200] {
            let log = Rc::new(RefCell::new(Vec::new()));
            {
                let mut set: HybridSet<Counted> = HybridSet::new();
                for i in 0..n {
                    set.insert(Counted(i, log.clone()));
                }
                let mut it = set.into_iter();
                for _ in 0..7 {
                    drop(it.next());
                }
                // `it` still holds the rest, and is dropped here.
            }
            let mut all = log.borrow().clone();
            all.sort_unstable();
            assert_eq!(all, (0..n).collect::<Vec<_>>(), "n={n}");
        }
    }

    /// The set is two words wide, whatever it holds. Those two words are a pointer into one
    /// allocation, at its metadata, with its elements lying below, and a packed slot-or-bucket
    /// count and element count. That is 16 bytes narrower than the 32-byte enum it replaced, on
    /// every entry of the enclosing map.
    #[test]
    fn set_is_two_words() {
        let word = std::mem::size_of::<usize>();
        assert_eq!(std::mem::size_of::<HybridSet<(u64, i16, u64)>>(), 2 * word);
        assert_eq!(std::mem::size_of::<HybridSet<u64>>(), 2 * word);
        assert_eq!(std::mem::size_of::<HybridSet<[u8; 40]>>(), 2 * word);
        // The threshold is a type parameter, not a second layout.
        assert_eq!(std::mem::size_of::<Swiss<u64>>(), 2 * word);
    }

    /// The representation switch must not change what the set holds, at the exact boundary.
    #[test]
    fn transition_preserves_contents() {
        let mut set = Set::new();
        for i in 0..SMALL_THRESHOLD as u64 {
            set.insert(i * 7 + 1);
        }
        assert!(!set.is_large());
        let before: BTreeSet<u64> = set.iter().copied().collect();
        assert!(set.insert(9999));
        assert!(set.is_large(), "one past the threshold must be large");
        let mut expected = before;
        expected.insert(9999);
        assert_eq!(set.iter().copied().collect::<BTreeSet<_>>(), expected);
        // Filling the promoted table caused no rehash, because we sized it for exactly the
        // elements it received.
        assert!(set.capacity() >= set.len());
    }

    /// A non-default threshold must behave the same way at its own boundary. The const parameter
    /// is a knob, not a special case.
    #[test]
    fn other_thresholds_switch_at_their_own_boundary() {
        fn check<const SMALL: usize>() {
            let mut set: HybridSet<u64, DefaultHashBuilder, SMALL> = HybridSet::new();
            let mut model = BTreeSet::new();
            for i in 0..(SMALL as u64 * 4 + 8) {
                let v = i * 13 + 5;
                assert_eq!(set.insert(v), model.insert(v));
                assert_eq!(set.len(), model.len());
                assert_eq!(
                    set.is_large(),
                    model.len() > SMALL,
                    "SMALL={SMALL}, len={}",
                    model.len()
                );
            }
            for v in &model {
                assert!(set.contains(v), "SMALL={SMALL}: {v} missing");
            }
            assert_eq!(set.iter().copied().collect::<BTreeSet<_>>(), model);
        }
        check::<0>();
        check::<1>();
        check::<2>();
        check::<8>();
        check::<16>();
        check::<32>();
        check::<64>();
    }

    /// Hashing is the enclosing set's job. After every rebuild it must still use the *same*
    /// hasher the table was built with. A promotion that rehashed with a different seed would
    /// lose elements silently.
    #[test]
    fn a_stateful_hasher_survives_promotion() {
        #[derive(Clone, Default)]
        struct Seeded;
        impl BuildHasher for Seeded {
            type Hasher = std::collections::hash_map::DefaultHasher;
            fn build_hasher(&self) -> Self::Hasher {
                std::collections::hash_map::DefaultHasher::new()
            }
        }
        let mut set: HybridSet<u64, Seeded> = HybridSet::new();
        for i in 0..500u64 {
            set.insert(i);
        }
        for i in 0..500u64 {
            assert!(set.contains(&i), "{i} lost across a rebuild");
        }
        assert_eq!(set.len(), 500);
    }
}
