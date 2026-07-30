//! A set that changes representation with its size: a **linear-probing** open-addressed table
//! while it is small, a [`hashbrown::HashTable`] once it is large.
//!
//! This is the `Set<V>` half of the `Map<K, Set<V>>` a Datalog index on `K` needs (see
//! `locals-trie-hybrid-ds.md`). In the workloads that motivate it the number of values per key
//! varies over four orders of magnitude — most keys hold a couple of values, a few hold
//! thousands — and no single representation is right for both ends:
//!
//! * A `hashbrown` table is wrong for 2 elements. Its bucket count is a power of two held at
//!   ≤ 87.5 % load, so it rounds a 2-element set up to a 4-bucket table and adds one control
//!   byte per bucket plus a `Group::WIDTH` mirror: 108 B to hold 48 B of payload
//!   (`locals-trie-benchmark.md` §1). Multiplied by the ~400 k tiny sets a real index holds,
//!   that slack *is* the store.
//! * A linear scan is wrong for 10 000 elements, and so is anything that re-copies the whole
//!   set when a delta is merged into it — that is what made large groups quadratic to build.
//!
//! So: [`HybridSet`] is a [`Probe`] table up to [`SMALL_THRESHOLD`] elements and a
//! [`hashbrown::HashTable`] above it.
//!
//! ## The small representation
//!
//! Open addressing with linear probing over `slots.len()` slots (a power of two, so the bucket
//! index is a mask), and — the one unusual choice — **the occupancy map is a `u64` bitmask
//! stored inline in the struct** rather than a byte per slot on the heap:
//!
//! ```text
//! occupied: 0b0010_1001          slots: [ x | _ | _ | y | _ | z | _ | _ ]
//! ```
//!
//! That buys three things a byte-per-slot control array does not:
//!
//! 1. **No per-element metadata on the heap.** A small set's allocation is exactly
//!    `slots * size_of::<T>()` — no control bytes, no group mirror — and `slots` starts at
//!    **one** and doubles, so a singleton set costs one element and a 2-element set costs two.
//!    That matters because in the distribution this exists for, most sets are that small.
//! 2. **Probing tests a register, not memory.** The empty/occupied check is a bit test on a
//!    value already in a register; only a hit touches the heap. For a set that fits in a couple
//!    of cache lines this is the whole lookup cost.
//! 3. **Iteration is `trailing_zeros`,** so iterating a sparse table costs O(elements), not
//!    O(slots) — which matters because every read view of the index iterates groups.
//!
//! The bitmask is what caps [`SMALL_THRESHOLD`] at 64 ([`MAX_SMALL_THRESHOLD`]), which is also
//! the value in use: measured end to end, the threshold is a pure memory knob, because the 2×
//! step a set takes when it promotes is paid by every set that crosses, and lowering the
//! threshold only moves more sets across it (`locals-trie-hybrid-eval.md` §5).
//!
//! Because nothing is ever removed from a Datalog index, there are no tombstones: a probe
//! sequence stops at the first empty slot, and the table is allowed to fill *completely*
//! before it grows. A full small table degenerates to a linear scan of its slots, which at these
//! sizes is the same worst case a packed array would have had — but it means the small
//! representation never pays load-factor slack, only `Vec`-style doubling slack. It is the one
//! place the design is measurably worse than either alternative: a *miss* against a completely
//! full table costs ~24 ns at 32 slots against ~5 ns for a sorted `Vec` or a `hashbrown` table
//! (`locals-trie-hybrid-eval.md` §2, finding 4).
//!
//! ## Transitioning
//!
//! The threshold crossing is one pass over at most [`SMALL_THRESHOLD`] elements:
//! [`hashbrown::HashTable::with_capacity`] sized for the known final element count (so the new
//! table never rehashes while being filled), elements **moved** out of the small buffer rather
//! than cloned, and the small buffer freed as soon as it is empty. Nothing else is touched:
//! the transition is local to one set, never walks the enclosing map, and never happens twice
//! for the same set (without removals a set's size only grows).
//!
//! [`HybridSet::merge`] gets the same treatment from the other direction: the union of two sets
//! is commutative, so it inserts the *smaller* side into the larger, making a merge cost
//! O(min(|a|, |b|)) lookups regardless of which side the caller passed as `self`.

use std::fmt;
use std::hash::{BuildHasher, BuildHasherDefault, Hash};
use std::mem::MaybeUninit;

use hashbrown::HashTable;
use hashbrown::hash_table;
use rustc_hash::FxHasher;

use super::hb_bytes;

/// Default hasher: the store keys are trusted, program-derived ids, so hash on the fast,
/// deterministic `FxHasher` rather than the std collections' DoS-resistant SipHash.
pub type DefaultHashBuilder = BuildHasherDefault<FxHasher>;

/// Element count at which a set switches from the linear-probing [`Probe`] table to
/// [`hashbrown::HashTable`]. A set holding this many elements is still `Probe`; the element
/// that would make it `SMALL_THRESHOLD + 1` promotes it.
///
/// Promotion is a hard ~2× on the bytes a set spends per element — a `hashbrown` table holds a
/// 24 B element in a power-of-two bucket array at ≤ 87.5 % load, ~50 B/element, against the
/// probe table's 24 — and, without removals, a set that crosses never comes back. So this
/// constant chooses how much of the size distribution pays that step, and it should sit *above*
/// the bulk of it. Measured at 16 / 32 / 64 on the `locals` workloads, 64 is the best of the
/// three: it keeps 33–64-element sets at 24 B/element, worth 14 % of the whole store where such
/// sets occur, at no measurable time cost (`locals-trie-hybrid-eval.md` §5).
pub const SMALL_THRESHOLD: usize = 64;

/// The largest [`SMALL_THRESHOLD`] the inline `u64` occupancy bitmask can describe.
pub const MAX_SMALL_THRESHOLD: usize = u64::BITS as usize;

const _: () = assert!(
    SMALL_THRESHOLD >= 1 && SMALL_THRESHOLD <= MAX_SMALL_THRESHOLD,
    "SMALL_THRESHOLD must fit the u64 occupancy bitmask"
);

/// Most slots the small table will ever allocate. If [`SMALL_THRESHOLD`] is not a power of two
/// the last growth overshoots it, exactly as a `Vec` would.
const SMALL_SLOTS_MAX: usize = SMALL_THRESHOLD.next_power_of_two();

/// First non-zero slot count.
///
/// **One**, not `Vec`'s minimum-non-zero-capacity of 4. A probe table has no reason to hold
/// spare slots — a 1-slot table is simply a table whose probe sequence has one stop — and in
/// the distribution this set exists for, most sets never hold more than two elements. Starting
/// at 4 would round every one of those up to 96 B of leaves to hold 24; starting at 1 makes a
/// singleton set cost exactly its element. The price is one extra doubling (1, 2, 4, 8, ...) for
/// sets that do grow, which is O(n) work amortized and allocator traffic that only the growing
/// minority pays.
const SMALL_SLOTS_MIN: usize = 1;

// ---------------------------------------------------------------------------
// The small representation: linear-probing table with an inline occupancy bitmask.
// ---------------------------------------------------------------------------

/// Where a probe sequence ended.
enum Probed {
    /// The element is at this slot.
    Found(usize),
    /// The element is absent; this empty slot is where it belongs.
    Vacant(usize),
    /// The element is absent and every slot is occupied.
    Full,
}

/// Open-addressed, linear-probing table of at most [`MAX_SMALL_THRESHOLD`] elements.
///
/// # Invariants
///
/// * `slots.len()` is either 0 or a power of two in `SMALL_SLOTS_MIN..=SMALL_SLOTS_MAX`.
/// * Bit `i` of `occupied` is set **iff** `slots[i]` holds an initialized `T`. Bits at or above
///   `slots.len()` are always clear.
///
/// Every `unsafe` block below is justified by the second invariant alone.
struct Probe<T> {
    slots: Box<[MaybeUninit<T>]>,
    occupied: u64,
}

impl<T> Probe<T> {
    fn new() -> Self {
        Self {
            // Does not allocate.
            slots: Vec::new().into_boxed_slice(),
            occupied: 0,
        }
    }

    /// A table with room for `slots` elements (rounded up to a power of two, clamped to the
    /// small representation's range).
    fn with_slots(slots: usize) -> Self {
        let slots = slot_count_for(slots);
        if slots == 0 {
            return Self::new();
        }
        Self {
            slots: Box::new_uninit_slice(slots),
            occupied: 0,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.occupied.count_ones() as usize
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    /// Walk the probe sequence for `hash` once, reporting where the element is or where it would
    /// go. One pass serves both `contains` and `insert`.
    ///
    /// Linear probing from `hash`, stopping at the first empty slot — sound because elements are
    /// never removed, so no tombstone can hide a later match. The `for` bound also makes a
    /// completely full table terminate, as [`Probed::Full`].
    #[inline]
    fn probe(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Probed {
        let n = self.slots.len();
        if n == 0 {
            return Probed::Full;
        }
        let mask = n - 1;
        let mut i = (hash as usize) & mask;
        for _ in 0..n {
            if self.occupied & (1u64 << i) == 0 {
                return Probed::Vacant(i);
            }
            // SAFETY: bit `i` is set, so `slots[i]` is initialized.
            if eq(unsafe { self.slots[i].assume_init_ref() }) {
                return Probed::Found(i);
            }
            i = (i + 1) & mask;
        }
        Probed::Full
    }

    #[inline]
    fn find(&self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<usize> {
        match self.probe(hash, eq) {
            Probed::Found(i) => Some(i),
            _ => None,
        }
    }

    /// Fill a slot [`Probed::Vacant`] reported.
    #[inline]
    fn fill(&mut self, index: usize, value: T) {
        debug_assert!(self.occupied & (1u64 << index) == 0, "slot is occupied");
        self.slots[index].write(value);
        self.occupied |= 1u64 << index;
    }

    /// Insert an element known to be absent. The table must not be full.
    #[inline]
    fn insert_unique(&mut self, hash: u64, value: T) {
        debug_assert!(!self.is_full(), "insert_unique into a full Probe");
        let mask = self.slots.len() - 1;
        let mut i = (hash as usize) & mask;
        while self.occupied & (1u64 << i) != 0 {
            i = (i + 1) & mask;
        }
        self.fill(i, value);
    }

    /// Move one arbitrary element out, or `None` when empty. Used by growth, promotion and
    /// `into_iter`; leaves the slot buffer allocated.
    #[inline]
    fn pop(&mut self) -> Option<T> {
        if self.occupied == 0 {
            return None;
        }
        let i = self.occupied.trailing_zeros() as usize;
        self.occupied &= !(1u64 << i);
        // SAFETY: bit `i` was set, so `slots[i]` was initialized; clearing the bit first makes
        // this the only read of that element (`Drop` and `find` both skip clear bits).
        Some(unsafe { self.slots[i].assume_init_read() })
    }

    /// Rehash into a table of `new_slots` slots. The old buffer is freed on return.
    fn resize(&mut self, new_slots: usize, hash_of: impl Fn(&T) -> u64) {
        debug_assert!(new_slots > self.len());
        let mut fresh = Self::with_slots(new_slots);
        while let Some(value) = self.pop() {
            let hash = hash_of(&value);
            fresh.insert_unique(hash, value);
        }
        *self = fresh;
    }

    #[inline]
    fn iter(&self) -> ProbeIter<'_, T> {
        ProbeIter {
            slots: &self.slots,
            bits: self.occupied,
        }
    }
}

impl<T> Drop for Probe<T> {
    fn drop(&mut self) {
        if std::mem::needs_drop::<T>() {
            let mut bits = self.occupied;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                // SAFETY: bit `i` is set, so `slots[i]` is initialized, and each index is
                // visited once.
                unsafe { self.slots[i].assume_init_drop() }
            }
        }
    }
}

impl<T: Clone> Clone for Probe<T> {
    fn clone(&self) -> Self {
        // Cloning slot-for-slot keeps every element on its own probe sequence, so the copy is
        // O(elements) with no hashing at all.
        let mut slots = Box::new_uninit_slice(self.slots.len());
        let mut bits = self.occupied;
        while bits != 0 {
            let i = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            // SAFETY: bit `i` is set, so `slots[i]` is initialized.
            slots[i].write(unsafe { self.slots[i].assume_init_ref() }.clone());
        }
        // Note: a panic in `T::clone` leaks the elements written so far (a `Box<[MaybeUninit<T>]>`
        // drops nothing). No unsoundness, and the element types used here do not panic.
        Self {
            slots,
            occupied: self.occupied,
        }
    }
}

/// Iterator over a [`Probe`] table's elements, in slot order. Costs O(elements), not O(slots).
struct ProbeIter<'a, T> {
    slots: &'a [MaybeUninit<T>],
    bits: u64,
}

impl<'a, T> Iterator for ProbeIter<'a, T> {
    type Item = &'a T;
    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        if self.bits == 0 {
            return None;
        }
        let i = self.bits.trailing_zeros() as usize;
        self.bits &= self.bits - 1;
        // SAFETY: bit `i` was set in the source table's `occupied`, so `slots[i]` is
        // initialized and stays so for `'a` (the iterator holds a shared borrow).
        Some(unsafe { self.slots[i].assume_init_ref() })
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.bits.count_ones() as usize;
        (n, Some(n))
    }
}

impl<T> ExactSizeIterator for ProbeIter<'_, T> {}

impl<T> Clone for ProbeIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots,
            bits: self.bits,
        }
    }
}

/// Slots to allocate to hold `n` elements in the small representation: a power of two, at least
/// [`SMALL_SLOTS_MIN`], at most [`SMALL_SLOTS_MAX`]. Load factor is allowed to reach 1.0, so
/// this rounds up only to the power of two, not past it.
#[inline]
fn slot_count_for(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        n.next_power_of_two()
            .clamp(SMALL_SLOTS_MIN, SMALL_SLOTS_MAX)
    }
}

// ---------------------------------------------------------------------------
// The hybrid set.
// ---------------------------------------------------------------------------

/// A set of `T` that is a linear-probing table while it holds at most [`SMALL_THRESHOLD`]
/// elements and a [`hashbrown::HashTable`] above that. See the module docs.
pub struct HybridSet<T, S = DefaultHashBuilder> {
    repr: Repr<T>,
    /// Zero-sized for the default hasher, so it costs nothing per set.
    hasher: S,
}

enum Repr<T> {
    Small(Probe<T>),
    Large(HashTable<T>),
}

impl<T, S: Default> Default for HybridSet<T, S> {
    fn default() -> Self {
        Self {
            repr: Repr::Small(Probe::new()),
            hasher: S::default(),
        }
    }
}

impl<T, S: Default> HybridSet<T, S> {
    /// An empty set. Does not allocate.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T, S> HybridSet<T, S> {
    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.repr {
            Repr::Small(p) => p.len(),
            Repr::Large(t) => t.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Elements this set can hold before it grows or changes representation. For the small
    /// representation this is the allocated slot count (load factor may reach 1.0).
    #[inline]
    pub fn capacity(&self) -> usize {
        match &self.repr {
            Repr::Small(p) => p.capacity(),
            Repr::Large(t) => t.capacity(),
        }
    }

    /// Whether this set has crossed [`SMALL_THRESHOLD`] and is now a `hashbrown` table.
    #[inline]
    pub fn is_large(&self) -> bool {
        matches!(self.repr, Repr::Large(_))
    }

    /// Elements, in unspecified order.
    #[inline]
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            inner: match &self.repr {
                Repr::Small(p) => IterInner::Small(p.iter()),
                Repr::Large(t) => IterInner::Large(t.iter()),
            },
        }
    }

    /// Heap bytes this set holds, including whatever slack the representation carries: exactly
    /// `slots * size_of::<T>()` while small (no per-element metadata at all), and hashbrown's
    /// own layout — buckets, control bytes and group mirror — once large.
    ///
    /// Used by the index's `heap_report`; [`super::hb_bytes`] is exact for these element types,
    /// so this is an allocation-size accounting rather than a payload count.
    pub fn heap_bytes(&self) -> usize {
        match &self.repr {
            Repr::Small(p) => p.capacity() * std::mem::size_of::<T>(),
            Repr::Large(t) => hb_bytes(t.capacity(), std::mem::size_of::<T>()),
        }
    }
}

impl<T, S> HybridSet<T, S>
where
    T: Hash + Eq,
    S: BuildHasher + Default,
{
    /// An empty set sized for `capacity` elements: a small table if that fits under the
    /// threshold, a `hashbrown` table straight away if it does not. Lets a caller that knows
    /// the final size skip the transition entirely.
    pub fn with_capacity(capacity: usize) -> Self {
        let repr = if capacity > SMALL_THRESHOLD {
            Repr::Large(HashTable::with_capacity(capacity))
        } else {
            Repr::Small(Probe::with_slots(capacity))
        };
        Self {
            repr,
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
        match &self.repr {
            Repr::Small(p) => p.find(hash, |x| x == value).is_some(),
            Repr::Large(t) => t.find(hash, |x| x == value).is_some(),
        }
    }

    /// Insert `value`; returns whether it was newly added.
    pub fn insert(&mut self, value: T) -> bool {
        let Self { repr, hasher } = self;
        let hash = hasher.hash_one(&value);
        match repr {
            Repr::Small(p) => {
                // Bind the probe result so the `eq` closure -- which borrows `value` -- is
                // dropped before `value` is moved into the table.
                let probed = p.probe(hash, |x| *x == value);
                match probed {
                    Probed::Found(_) => false,
                    Probed::Vacant(i) => {
                        p.fill(i, value);
                        true
                    }
                    // Absent, and there is nowhere to put it: either grow or change
                    // representation.
                    Probed::Full => {
                        if p.len() >= SMALL_THRESHOLD {
                            // Crossing the threshold: build the `hashbrown` table at its final
                            // size so filling it never rehashes, then add the element that
                            // triggered the move.
                            let final_len = p.len() + 1;
                            let mut table = promote(p, final_len, hasher);
                            let _ = table.insert_unique(hash, value, |x| hasher.hash_one(x));
                            *repr = Repr::Large(table);
                        } else {
                            let new_slots = if p.capacity() == 0 {
                                SMALL_SLOTS_MIN
                            } else {
                                p.capacity() * 2
                            };
                            p.resize(new_slots, |x| hasher.hash_one(x));
                            p.insert_unique(hash, value);
                        }
                        true
                    }
                }
            }
            Repr::Large(t) => {
                // Bind the entry so the `eq` closure -- which borrows `value` -- is dropped
                // before the arm that moves `value` into the table.
                let entry = t.entry(hash, |x| *x == value, |x| hasher.hash_one(x));
                match entry {
                    hash_table::Entry::Occupied(_) => false,
                    hash_table::Entry::Vacant(e) => {
                        e.insert(value);
                        true
                    }
                }
            }
        }
    }

    /// Make room for `additional` more elements, changing representation up front if that many
    /// more elements could not fit in the small one.
    ///
    /// Callers that only have an *upper bound* on the additional count (a union, say, whose
    /// overlap is unknown) should not use this: over-reserving here permanently promotes a set
    /// that might have stayed small. [`Self::merge`] deliberately does not call it.
    pub fn reserve(&mut self, additional: usize) {
        let Self { repr, hasher } = self;
        match repr {
            Repr::Small(p) => {
                let want = p.len() + additional;
                if want > SMALL_THRESHOLD {
                    *repr = Repr::Large(promote(p, want, hasher));
                } else if want > p.capacity() {
                    p.resize(slot_count_for(want), |x| hasher.hash_one(x));
                }
            }
            Repr::Large(t) => t.reserve(additional, |x| hasher.hash_one(x)),
        }
    }

    /// Union `other` into `self`; returns how many elements were **newly added to `self`**.
    ///
    /// Cost is O(min(|self|, |other|)) lookups: a union is commutative, so if `other` is the
    /// bigger side the two are swapped and the smaller one is inserted into it. That is what
    /// keeps a delta→total merge proportional to the delta rather than to the accumulated
    /// total, whichever way round the caller has them.
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
        // `self` now holds the union either way; `before` is the size of the set the caller
        // passed as `self`, which is what "newly added to self" is measured against.
        self.len() - before
    }
}

/// Move a small table's elements into a right-sized `hashbrown` table, leaving it empty (its
/// buffer is freed when the caller overwrites the `Repr`).
fn promote<T: Hash + Eq, S: BuildHasher>(
    p: &mut Probe<T>,
    capacity: usize,
    hasher: &S,
) -> HashTable<T> {
    let mut table = HashTable::with_capacity(capacity);
    while let Some(value) = p.pop() {
        let hash = hasher.hash_one(&value);
        // Elements come from a set, so they are distinct; `with_capacity` above means the
        // rehash closure is never called.
        let _ = table.insert_unique(hash, value, |x| hasher.hash_one(x));
    }
    table
}

impl<T, S> Clone for HybridSet<T, S>
where
    T: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            repr: match &self.repr {
                Repr::Small(p) => Repr::Small(p.clone()),
                Repr::Large(t) => Repr::Large(t.clone()),
            },
            hasher: self.hasher.clone(),
        }
    }
}

impl<T, S> Extend<T> for HybridSet<T, S>
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

impl<T, S> FromIterator<T> for HybridSet<T, S>
where
    T: Hash + Eq,
    S: BuildHasher + Default,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        // The lower bound is a count of elements *before* dedup, so it is only a hint; sizing
        // to it avoids the transition when the caller knows the set is already large.
        let mut set = Self::with_capacity(iter.size_hint().0);
        set.extend(iter);
        set
    }
}

impl<T: fmt::Debug, S> fmt::Debug for HybridSet<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// Shared-borrow iterator over a [`HybridSet`], in unspecified order.
pub struct Iter<'a, T> {
    inner: IterInner<'a, T>,
}

enum IterInner<'a, T> {
    Small(ProbeIter<'a, T>),
    Large(hash_table::Iter<'a, T>),
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;
    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        match &mut self.inner {
            IterInner::Small(i) => i.next(),
            IterInner::Large(i) => i.next(),
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            IterInner::Small(i) => i.size_hint(),
            IterInner::Large(i) => i.size_hint(),
        }
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

impl<T> Clone for Iter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            inner: match &self.inner {
                IterInner::Small(i) => IterInner::Small(i.clone()),
                IterInner::Large(i) => IterInner::Large(i.clone()),
            },
        }
    }
}

impl<'a, T, S> IntoIterator for &'a HybridSet<T, S> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

/// Owning iterator over a [`HybridSet`], in unspecified order.
pub struct IntoIter<T> {
    inner: IntoIterInner<T>,
}

enum IntoIterInner<T> {
    Small(Probe<T>),
    Large(hash_table::IntoIter<T>),
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        match &mut self.inner {
            IntoIterInner::Small(p) => p.pop(),
            IntoIterInner::Large(i) => i.next(),
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = match &self.inner {
            IntoIterInner::Small(p) => p.len(),
            IntoIterInner::Large(i) => return i.size_hint(),
        };
        (n, Some(n))
    }
}

impl<T> ExactSizeIterator for IntoIter<T> {}

impl<T, S> IntoIterator for HybridSet<T, S> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> IntoIter<T> {
        IntoIter {
            inner: match self.repr {
                Repr::Small(p) => IntoIterInner::Small(p),
                Repr::Large(t) => IntoIterInner::Large(t.into_iter()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    use super::{HybridSet, SMALL_THRESHOLD, slot_count_for};

    type Set = HybridSet<u64>;

    /// Cheap deterministic pseudo-random sequence (no `rand` dependency).
    fn lcg(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s >> 11
        }
    }

    /// Every element inserted is findable, every element not inserted is not, and the size and
    /// the iteration agree with a `BTreeSet` built from the same inserts.
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

    #[test]
    fn matches_a_set_model_across_the_threshold() {
        model_check(&[]);
        model_check(&[7]);
        model_check(&[7, 7, 7]);
        // Dense low values: probe sequences wrap and collide constantly.
        model_check(&(0..100u64).collect::<Vec<_>>());
        // Every element hashes to the same slot in a power-of-two table would be ideal; the
        // multiples below at least share low bits after FxHash mixing.
        model_check(&(0..80u64).map(|i| i * 64).collect::<Vec<_>>());
        // Exactly at / around the threshold, which is where representation flips.
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

    #[test]
    fn absent_elements_are_not_found_at_every_size() {
        // A full small table has no empty slot to stop a probe at, so the "not present" answer
        // has to come from the bounded scan. Check it at every size up to and past promotion.
        for n in 0..=(SMALL_THRESHOLD + 4) as u64 {
            let set: Set = (0..n).collect();
            assert_eq!(set.len() as u64, n);
            for absent in n..n + 8 {
                assert!(!set.contains(&absent), "n={n}: {absent} must be absent");
            }
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
    fn small_representation_allocates_only_element_slots() {
        // The point of the inline occupancy bitmask: a small set's heap cost is exactly its
        // slots, with no control bytes. Slot counts follow `Vec`-style doubling from 4.
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
            assert_eq!(
                set.heap_bytes(),
                slots * std::mem::size_of::<u64>(),
                "n={n}: heap bytes must be exactly the slots"
            );
            assert!(!set.is_large(), "n={n} must stay small");
        }
        assert_eq!(slot_count_for(0), 0);
        assert_eq!(slot_count_for(1), 1);
        assert_eq!(slot_count_for(33), SMALL_THRESHOLD.next_power_of_two());
    }

    #[test]
    fn with_capacity_skips_the_transition() {
        let small: Set = Set::with_capacity(SMALL_THRESHOLD);
        assert!(!small.is_large());
        let large: Set = Set::with_capacity(SMALL_THRESHOLD + 1);
        assert!(large.is_large(), "sized past the threshold, start large");
        assert!(large.capacity() > SMALL_THRESHOLD);
        // ... and stays usable and correct.
        let mut large = large;
        for i in 0..100 {
            assert!(large.insert(i));
        }
        assert_eq!(large.len(), 100);
    }

    #[test]
    fn reserve_promotes_only_when_it_must() {
        let mut set: Set = (0..4).collect();
        assert_eq!(set.capacity(), 4);
        set.reserve(SMALL_THRESHOLD - 4);
        assert!(!set.is_large());
        assert_eq!(set.capacity(), SMALL_THRESHOLD.next_power_of_two());
        assert_eq!(set.len(), 4);
        set.reserve(SMALL_THRESHOLD);
        assert!(set.is_large());
        assert_eq!(set.len(), 4, "reserve must not disturb the contents");
        for i in 0..4 {
            assert!(set.contains(&i));
        }
    }

    /// Elements with a destructor must be dropped exactly once, from either representation and
    /// through every path that moves them (growth, promotion, `into_iter`, drop).
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
        impl std::hash::Hash for Counted {
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
                    // Insert each value twice: the duplicate is dropped immediately.
                    set.insert(Counted(i, log.clone()));
                    set.insert(Counted(i, log.clone()));
                }
                assert_eq!(set.len() as u64, n);
                // The duplicates, and nothing else, have been dropped so far.
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

        // Same, but the set is consumed by `into_iter` and the yielded elements are dropped by
        // the caller — the un-yielded remainder must still be dropped exactly once.
        let log = Rc::new(RefCell::new(Vec::new()));
        {
            let mut set: HybridSet<Counted> = HybridSet::new();
            for i in 0..10 {
                set.insert(Counted(i, log.clone()));
            }
            let mut it = set.into_iter();
            drop(it.next());
            drop(it.next());
            // `it` (holding 8 elements) is dropped here.
        }
        let mut all = log.borrow().clone();
        all.sort_unstable();
        assert_eq!(all, (0..10).collect::<Vec<_>>());
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
        // No rehash happened while filling the promoted table: it was sized for exactly the
        // elements it received.
        assert!(set.capacity() >= set.len());
    }
}
