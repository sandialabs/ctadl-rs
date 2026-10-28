//! A general, **n-ary** Ascent relation data-structure (`ds`) provider backed by
//! the zero-copy [`MmapLsmMultiMap`](crate::mmap::MmapLsmMultiMap).
//!
//! # Relationship to Ascent's default provider
//!
//! Ascent's default provider stores each relation's tuples in a `Vec` and, for
//! every index Ascent needs, an in-RAM `HashMap<Key, Vec<Value>>`. The full
//! tuple data is therefore duplicated across every index *in memory* — which is
//! exactly what blows the heap on large inputs.
//!
//! This provider stores each relation's tuples once, in an [`LsmStore`] (the
//! relation's `rel_ind_common`), and serves from it every index whose key
//! columns are a prefix of the relation's, including the full index; only the
//! other indices get an [`MmapLsmMultiMap`] of their own ([`LsmIndex`]). The
//! bulk of every store and index lives in `mmap`'d run files (off the heap, in
//! the page cache); only a small write buffer is in RAM.
//!
//! # Encoding
//!
//! Reads from `MmapLsmMultiMap` hand back `&T` references straight into the
//! mapping, so keys and values must be fixed-width `bytemuck::Pod`. Relation
//! columns generally are *not* `Pod` (e.g. interned handles whose ids are only
//! meaningful to their interner). Each column type implements [`LsmCol`], which encodes a
//! value into a fixed number of `u64` "slots" and reinterprets those slots back
//! as a `&Column`.
//!
//! For interned types the slot holds the handle's raw bits. This is only valid
//! **within a single process run** (the run files are per-process temp files, and
//! the interned values must outlive them) and assumes a **little-endian** target
//! so that the value's bytes occupy the low end of each slot.

use std::iter::{Once, once};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use bytemuck::{Pod, Zeroable};
use smallvec::SmallVec;

use crate::mmap::{IterAllRefs, KeysIter, MmapLsmMultiMap, PrefixGroups, ValsRefs, words};

// ---------------------------------------------------------------------------
// Process-wide per-map memtable limit.
//
// Every map this provider creates (a relation's store, and an index of its own
// for each non-prefix index pattern, each in three versions: new/delta/total)
// uses the same memtable limit. Callers pick the value (e.g. derived from
// available RAM) via [`set_memtable_limit`]; this crate stays policy-free.
// ---------------------------------------------------------------------------

/// Per-map memtable limit (entries) used until [`set_memtable_limit`] overrides it.
pub const DEFAULT_MEMTABLE_LIMIT: usize = 100_000;

/// The per-map memtable limit, in entries.
static MEMTABLE_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_MEMTABLE_LIMIT);

/// Sets the per-map memtable limit, in entries (`(key, value)` insertions): each
/// map flushes its in-memory buffer to disk once it holds this many. Larger
/// values mean fewer, larger run files — less dedup-scan overhead during the
/// fixpoint — at the cost of more resident RAM per map. Set it before
/// building/running the Ascent program. Minimum 1.
pub fn set_memtable_limit(limit: usize) {
    MEMTABLE_LIMIT.store(limit.max(1), Ordering::Relaxed);
}

/// The current per-map memtable limit (entries).
pub fn memtable_limit() -> usize {
    MEMTABLE_LIMIT.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Slot storage: a no-padding concatenation of two `Pod`, 8-aligned blobs.
// ---------------------------------------------------------------------------

/// Concatenation of two slot blobs `A` and `B`, used to build the fixed-width
/// key/value storage for a tuple out of its columns' per-column slot arrays.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Cat<A, B>(pub A, pub B);

// SAFETY: `Cat` is `#[repr(C)]`. In every use of this provider `A` and `B` are
// `[u64; N]` arrays (or nested `Cat`s of such), which all have alignment 8 and a
// size that is a multiple of 8. Therefore `B` lands at an 8-aligned offset with
// no padding before it, and the whole struct has no trailing padding — every
// byte is initialized, satisfying `Pod`'s contract.
unsafe impl<A: Pod, B: Pod> Pod for Cat<A, B> {}
// SAFETY: an all-zero bit pattern is valid for `A` and `B` (both `Zeroable`).
unsafe impl<A: Zeroable, B: Zeroable> Zeroable for Cat<A, B> {}

// ---------------------------------------------------------------------------
// Column encoding.
// ---------------------------------------------------------------------------

/// Encodes a single relation column into fixed-width `u64` slots and back.
///
/// # Safety
///
/// Implementors must guarantee:
/// * `size_of::<Self>() <= 8 * N` and `align_of::<Self>() <= 8`, where
///   `Slots = [u64; N]`;
/// * the target is little-endian, so the value's bytes written by `to_slots`
///   occupy the low bytes that `ref_from` reinterprets;
/// * every byte of the slots `to_slots` returns is initialized, and equal
///   values give equal slots: the stores sort and deduplicate by slot bits;
/// * any slot blob produced by `to_slots` from a live value remains a valid
///   `Self` for as long as the slots are read back (process lifetime for
///   interned types).
///
/// Use the [`lsm_col!`](crate::lsm_col) macro to implement this safely for a
/// `Copy` type small enough to fit.
pub unsafe trait LsmCol: Copy + 'static {
    /// Fixed-width slot storage for one value of this column. Always `[u64; N]`.
    type Slots: Pod + Ord + Copy;

    /// Copies the value's bytes into freshly zeroed slots.
    fn to_slots(self) -> Self::Slots;

    /// Reinterprets stored slots as a reference to this column.
    ///
    /// # Safety
    /// `s` must have been produced by [`LsmCol::to_slots`] of a still-live value.
    unsafe fn ref_from(s: &Self::Slots) -> &Self;

    /// Whether every interned handle in this value was handed out by its interner.
    /// `ref_from` asserts it in debug builds, since the slots bypassed the
    /// interners' checked conversions.
    #[inline]
    fn is_valid(&self) -> bool {
        true
    }
}

/// Implements [`LsmCol`] for a `Copy` type that fits in `$n` eight-byte slots.
///
/// `$n` must satisfy `size_of::<$t>() <= 8 * $n` and `align_of::<$t>() <= 8`
/// (both are checked at compile time). The target must be little-endian.
///
/// The bracketed list names the types of `$t`'s fields, or `$t` itself for a
/// primitive. Their sizes must sum to `size_of::<$t>()`, which is checked at
/// compile time: a value's bytes are copied into the slots and compared there,
/// so a padding byte, which is uninitialized, must not exist.
///
/// `valid = f` names a `fn(&$t) -> bool` that becomes [`LsmCol::is_valid`], for
/// a type that holds interned handles: [`LsmCol::ref_from`] asserts it, in debug
/// builds, on every value it reads back.
///
/// ```
/// # #[derive(Clone, Copy)] struct Pair(u32, u32);
/// lsm::lsm_col!(Pair, 1, [u32, u32]);
/// ```
///
/// ```compile_fail
/// # #[derive(Clone, Copy)] struct Padded(u64, u32);
/// lsm::lsm_col!(Padded, 2, [u64, u32]);
/// ```
#[macro_export]
macro_rules! lsm_col {
    ($t:ty, $n:literal, [$($field:ty),+ $(,)?] $(, valid = $valid:expr)? $(,)?) => {
        // SAFETY: the asserts below enforce the size/alignment invariants and that
        // every byte is initialized; the caller is responsible for little-endianness
        // and process-local validity.
        unsafe impl $crate::ascent_provider::LsmCol for $t {
            type Slots = [u64; $n];

            #[inline]
            fn to_slots(self) -> [u64; $n] {
                const _: () = assert!(
                    ::core::mem::size_of::<$t>() <= 8 * $n,
                    "lsm_col!: type does not fit in the requested number of slots",
                );
                const _: () = assert!(
                    ::core::mem::align_of::<$t>() <= 8,
                    "lsm_col!: type alignment exceeds 8",
                );
                const _: () = assert!(
                    ::core::mem::size_of::<$t>() == 0 $(+ ::core::mem::size_of::<$field>())+,
                    "lsm_col!: type has padding, or the field list is incomplete",
                );
                let mut slots = [0u64; $n];
                // SAFETY: `slots` has `8*$n >= size_of::<$t>()` bytes and is
                // 8-aligned (>= align of `$t`); we copy the value's bytes into it.
                unsafe {
                    ::core::ptr::copy_nonoverlapping(
                        &self as *const $t as *const u8,
                        slots.as_mut_ptr() as *mut u8,
                        ::core::mem::size_of::<$t>(),
                    );
                }
                slots
            }

            #[inline]
            unsafe fn ref_from(s: &[u64; $n]) -> &$t {
                // SAFETY: `s` is 8-aligned (>= align of `$t`) and its leading
                // `size_of::<$t>()` bytes hold a valid `$t` written by `to_slots`.
                let value = unsafe { &*(s.as_ptr() as *const $t) };
                debug_assert!(
                    $crate::ascent_provider::LsmCol::is_valid(value),
                    "lsm_col!: read back a {} holding a handle that was never handed out",
                    ::core::stringify!($t),
                );
                value
            }
            $(
                #[inline]
                fn is_valid(&self) -> bool {
                    ($valid)(self)
                }
            )?
        }
    };
}

// Column encodings for the primitive integer types.
lsm_col!(u8, 1, [u8]);
lsm_col!(u16, 1, [u16]);
lsm_col!(u32, 1, [u32]);
lsm_col!(u64, 1, [u64]);
lsm_col!(i8, 1, [i8]);
lsm_col!(i16, 1, [i16]);
lsm_col!(i32, 1, [i32]);
lsm_col!(i64, 1, [i64]);

// A pair of columns, for a relation that carries two values in one column. Its
// slots are laid out as the pair itself, so a reference into them is a
// `&(A, B)`. The pair may have padding: each field is written from its own
// `to_slots` encoding into zeroed slots, so the padding bytes are zeros, never
// uninitialized bytes copied from the value.
//
// SAFETY: `to_slots` asserts the pair fits the slots and is at most 8-aligned.
// Every byte of the slots is initialized: zeroed, then overwritten with the
// fields' initialized encodings, so equal pairs give equal slots. Validity of
// the reinterpreted fields is inherited from `A`/`B`.
unsafe impl<A: LsmCol, B: LsmCol> LsmCol for (A, B) {
    type Slots = Cat<A::Slots, B::Slots>;

    #[inline]
    fn to_slots(self) -> Self::Slots {
        const {
            assert!(size_of::<(A, B)>() <= size_of::<Cat<A::Slots, B::Slots>>());
            assert!(align_of::<(A, B)>() <= 8);
        }
        let mut slots = Cat(A::Slots::zeroed(), B::Slots::zeroed());
        let pair = &mut slots as *mut Self::Slots as *mut (A, B);
        let (a, b) = (self.0.to_slots(), self.1.to_slots());
        // SAFETY: the asserts above: `slots` is large enough and aligned for a
        // `(A, B)`, so each field's place is in bounds. A field's encoding begins
        // with its `size_of` value bytes (little-endian, `LsmCol`'s contract).
        unsafe {
            std::ptr::copy_nonoverlapping(
                &a as *const A::Slots as *const u8,
                std::ptr::addr_of_mut!((*pair).0) as *mut u8,
                size_of::<A>(),
            );
            std::ptr::copy_nonoverlapping(
                &b as *const B::Slots as *const u8,
                std::ptr::addr_of_mut!((*pair).1) as *mut u8,
                size_of::<B>(),
            );
        }
        slots
    }

    #[inline]
    unsafe fn ref_from(s: &Self::Slots) -> &Self {
        // SAFETY: `s` is 8-aligned and holds the fields `to_slots` wrote from a
        // live `(A, B)`.
        let pair = unsafe { &*(s as *const Self::Slots as *const (A, B)) };
        debug_assert!(
            pair.is_valid(),
            "LsmCol for (A, B): read back a handle that was never handed out"
        );
        pair
    }

    #[inline]
    fn is_valid(&self) -> bool {
        self.0.is_valid() && self.1.is_valid()
    }
}

// ---------------------------------------------------------------------------
// Tuple encoding: an N-column tuple <-> a single fixed-width slot blob.
// ---------------------------------------------------------------------------

/// Encodes a whole key/value tuple into one fixed-width `Pod` slot blob and
/// decodes it back into a tuple of borrowed columns.
pub trait LsmTuple {
    /// The fixed-width slot storage for this tuple (a nested [`Cat`] of the
    /// columns' [`LsmCol::Slots`]).
    type Arr: Pod + Ord + Copy;
    /// A tuple of references into a stored [`Self::Arr`]. Outlives `'a` because
    /// it is a tuple of `&'a Column` (and columns are `'static`).
    type Ref<'a>: 'a
    where
        Self: 'a;

    /// Encodes the tuple's columns into a slot blob.
    fn encode(&self) -> Self::Arr;

    /// Reinterprets a stored slot blob as a tuple of column references.
    ///
    /// # Safety
    /// `a` must have been produced by [`LsmTuple::encode`] from live values.
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_>;

    /// The width of each column's slots, in bytes.
    const COLUMN_BYTES: &'static [usize];
}

// arity 0 — only ever a *key* (the "no index" full scan). Uses a one-slot
// sentinel so the storage is never a zero-sized type.
impl LsmTuple for () {
    type Arr = [u64; 1];
    type Ref<'a>
        = ()
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> [u64; 1] {
        [0]
    }
    #[inline]
    unsafe fn decode(_a: &[u64; 1]) {}
    const COLUMN_BYTES: &'static [usize] = &[];
}

// Arities 1..=6. The `Arr` is a right-folded `Cat` of each column's slots, e.g.
// arity-3 is `Cat<A, Cat<B, C>>`, so field `b` is `a.1.0`, `c` is `a.1.1`, etc.
// `decode` is written as a real method (not a closure) so the output references
// are tied to the lifetime of the input `&Self::Arr`.

impl<A: LsmCol> LsmTuple for (A,) {
    type Arr = A::Slots;
    const COLUMN_BYTES: &'static [usize] = &[size_of::<A::Slots>()];
    type Ref<'a>
        = (&'a A,)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        self.0.to_slots()
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe { (A::ref_from(a),) }
    }
}

impl<A: LsmCol, B: LsmCol> LsmTuple for (A, B) {
    type Arr = Cat<A::Slots, B::Slots>;
    const COLUMN_BYTES: &'static [usize] = &[size_of::<A::Slots>(), size_of::<B::Slots>()];
    type Ref<'a>
        = (&'a A, &'a B)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(self.0.to_slots(), self.1.to_slots())
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe { (A::ref_from(&a.0), B::ref_from(&a.1)) }
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol> LsmTuple for (A, B, C) {
    type Arr = Cat<A::Slots, Cat<B::Slots, C::Slots>>;
    const COLUMN_BYTES: &'static [usize] = &[
        size_of::<A::Slots>(),
        size_of::<B::Slots>(),
        size_of::<C::Slots>(),
    ];
    type Ref<'a>
        = (&'a A, &'a B, &'a C)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(self.0.to_slots(), Cat(self.1.to_slots(), self.2.to_slots()))
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe { (A::ref_from(&a.0), B::ref_from(&a.1.0), C::ref_from(&a.1.1)) }
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol> LsmTuple for (A, B, C, D) {
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, D::Slots>>>;
    const COLUMN_BYTES: &'static [usize] = &[
        size_of::<A::Slots>(),
        size_of::<B::Slots>(),
        size_of::<C::Slots>(),
        size_of::<D::Slots>(),
    ];
    type Ref<'a>
        = (&'a A, &'a B, &'a C, &'a D)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(
            self.0.to_slots(),
            Cat(self.1.to_slots(), Cat(self.2.to_slots(), self.3.to_slots())),
        )
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe {
            (
                A::ref_from(&a.0),
                B::ref_from(&a.1.0),
                C::ref_from(&a.1.1.0),
                D::ref_from(&a.1.1.1),
            )
        }
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol, E: LsmCol> LsmTuple for (A, B, C, D, E) {
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, Cat<D::Slots, E::Slots>>>>;
    const COLUMN_BYTES: &'static [usize] = &[
        size_of::<A::Slots>(),
        size_of::<B::Slots>(),
        size_of::<C::Slots>(),
        size_of::<D::Slots>(),
        size_of::<E::Slots>(),
    ];
    type Ref<'a>
        = (&'a A, &'a B, &'a C, &'a D, &'a E)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(
            self.0.to_slots(),
            Cat(
                self.1.to_slots(),
                Cat(self.2.to_slots(), Cat(self.3.to_slots(), self.4.to_slots())),
            ),
        )
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe {
            (
                A::ref_from(&a.0),
                B::ref_from(&a.1.0),
                C::ref_from(&a.1.1.0),
                D::ref_from(&a.1.1.1.0),
                E::ref_from(&a.1.1.1.1),
            )
        }
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol, E: LsmCol, F: LsmCol> LsmTuple
    for (A, B, C, D, E, F)
{
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, Cat<D::Slots, Cat<E::Slots, F::Slots>>>>>;
    const COLUMN_BYTES: &'static [usize] = &[
        size_of::<A::Slots>(),
        size_of::<B::Slots>(),
        size_of::<C::Slots>(),
        size_of::<D::Slots>(),
        size_of::<E::Slots>(),
        size_of::<F::Slots>(),
    ];
    type Ref<'a>
        = (&'a A, &'a B, &'a C, &'a D, &'a E, &'a F)
    where
        Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(
            self.0.to_slots(),
            Cat(
                self.1.to_slots(),
                Cat(
                    self.2.to_slots(),
                    Cat(self.3.to_slots(), Cat(self.4.to_slots(), self.5.to_slots())),
                ),
            ),
        )
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        // SAFETY: forwarded from the caller: `a` came from `encode` of live
        // values, so each field holds slots `to_slots` wrote for that column.
        unsafe {
            (
                A::ref_from(&a.0),
                B::ref_from(&a.1.0),
                C::ref_from(&a.1.1.0),
                D::ref_from(&a.1.1.1.0),
                E::ref_from(&a.1.1.1.1.0),
                F::ref_from(&a.1.1.1.1.1),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Borrowing iterators that decode slot blobs lazily.
// ---------------------------------------------------------------------------

/// Iterator over a key's values, decoding each stored slot blob into a tuple of
/// column references on the fly. Cheap to clone (as Ascent's `IteratorType`
/// requires) because the underlying [`ValsRefs`] is.
pub struct ValIter<'a, V: LsmTuple> {
    inner: ValsRefs<'a, V::Arr>,
}

impl<V: LsmTuple> Clone for ValIter<'_, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<'a, V: LsmTuple + 'a> Iterator for ValIter<'a, V> {
    type Item = V::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: every stored blob was produced by `V::encode`.
        self.inner.next().map(|a| unsafe { V::decode(a) })
    }
}

/// Iterator over every `(key, values)` group of an [`LsmIndex`].
pub struct AllIter<'a, K: LsmTuple, V: LsmTuple> {
    inner: IterAllRefs<'a, K::Arr, V::Arr>,
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a> Iterator for AllIter<'a, K, V> {
    type Item = (K::Ref<'a>, ValIter<'a, V>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(k, vals)| {
            // SAFETY: the stored key blob was produced by `K::encode`.
            (unsafe { K::decode(k) }, ValIter { inner: vals })
        })
    }
}

/// Iterator over every tuple of an [`LsmStore`], as the full index's keys.
pub struct FullAllIter<'a, T: LsmTuple> {
    inner: KeysIter<'a, T::Arr, ()>,
}

impl<'a, T: LsmTuple + 'a> Iterator for FullAllIter<'a, T> {
    type Item = (T::Ref<'a>, Once<()>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the stored blob was produced by `T::encode`.
        self.inner
            .next()
            .map(|k| (unsafe { T::decode(k) }, once(())))
    }
}

// ---------------------------------------------------------------------------
// An index whose key columns are not a prefix of the relation's: key ->
// remaining columns, backed by an LSM map of its own.
// ---------------------------------------------------------------------------

/// An Ascent index for key columns `K` mapping to value columns `V`, backed by a
/// single [`MmapLsmMultiMap`]. This is the n-ary analogue of the default
/// provider's `RelIndexType1<K, V>` (a `HashMap<K, Vec<V>>`).
pub struct LsmIndex<K: LsmTuple, V: LsmTuple> {
    map: MmapLsmMultiMap<K::Arr, V::Arr>,
}

impl<K: LsmTuple, V: LsmTuple> Default for LsmIndex<K, V> {
    fn default() -> Self {
        Self {
            map: MmapLsmMultiMap::with_memtable_limit(memtable_limit()),
        }
    }
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a> RelIndexRead<'a> for LsmIndex<K, V> {
    type Key = K;
    type Value = V::Ref<'a>;
    type IteratorType = ValIter<'a, V>;

    #[inline]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        let vals = self.map.get_refs(&key.encode())?;
        Some(ValIter { inner: vals })
    }

    #[inline]
    fn len_estimate(&'a self) -> usize {
        self.map.len()
    }

    #[inline]
    fn is_empty(&'a self) -> bool {
        self.map.is_empty()
    }
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a> RelIndexReadAll<'a> for LsmIndex<K, V> {
    type Key = K::Ref<'a>;
    type Value = V::Ref<'a>;
    type ValueIteratorType = ValIter<'a, V>;
    type AllIteratorType = AllIter<'a, K, V>;

    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        AllIter {
            inner: self.map.iter_all_refs(),
        }
    }
}

impl<K: LsmTuple, V: LsmTuple> RelIndexWrite for LsmIndex<K, V> {
    type Key = K;
    type Value = V;

    #[inline]
    fn index_insert(&mut self, key: Self::Key, value: Self::Value) {
        self.map.insert(key.encode(), value.encode());
    }
}

impl<K: LsmTuple, V: LsmTuple> RelIndexMerge for LsmIndex<K, V> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.map.absorb(&mut from.map);
    }
}

impl<K: LsmTuple, V: LsmTuple, Rel> ToRelIndex<Rel> for LsmIndex<K, V> {
    type RelIndex<'a>
        = &'a LsmIndex<K, V>
    where
        Self: 'a,
        Rel: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, _rel: &'a Rel) -> Self::RelIndex<'a> {
        self
    }

    type RelIndexWrite<'a>
        = &'a mut LsmIndex<K, V>
    where
        Self: 'a,
        Rel: 'a;
    #[inline]
    fn to_rel_index_write<'a>(&'a mut self, _rel: &'a mut Rel) -> Self::RelIndexWrite<'a> {
        self
    }
}

// ---------------------------------------------------------------------------
// The shared store: one set of full tuples per relation, and the indices that
// are views of it.
//
// A tuple's slot blob is its columns' `u64` words in column order, and the
// blobs' order is the lexicographic order of those words. So the tuples whose
// first `k` columns are given are one contiguous range of the set, and their
// remaining columns are the tail words of each blob. Every index whose key
// columns are a prefix of the relation's (including the empty "none" index and
// the full index itself) is therefore served by the one set, as a range read,
// and only the other indices need storage of their own ([`LsmIndex`]).
//
// The protocol is Ascent's BYODS one: the set is the relation's `ind_common`,
// and the view indices are zero-sized markers whose read views borrow it.
// Tuples enter the set only through the full index's write target, and only
// the `ind_common` merges; the views' write targets and merges do nothing,
// since a second merge of the same data would corrupt semi-naive evaluation.
// ---------------------------------------------------------------------------

/// A relation's tuples, once: the `ind_common` of an LSM-provided relation.
///
/// Bit `k` of `PREFIXES` says some index reads the tuples by their first `k`
/// columns (see [`prefix_mask`]); the runs keep a Bloom filter of those
/// prefixes, so a read skips the runs that hold none of its prefix.
pub struct LsmStore<T: LsmTuple, const PREFIXES: u32> {
    set: MmapLsmMultiMap<T::Arr, ()>,
}

impl<T: LsmTuple, const PREFIXES: u32> Default for LsmStore<T, PREFIXES> {
    fn default() -> Self {
        let mut lens: SmallVec<[usize; 2]> = SmallVec::new();
        for k in 1..T::COLUMN_BYTES.len() {
            if PREFIXES & (1 << k) != 0 {
                lens.push(T::COLUMN_BYTES[..k].iter().sum());
            }
        }
        Self {
            set: MmapLsmMultiMap::with_memtable_limit(memtable_limit()).with_prefix_reads(&lens),
        }
    }
}

/// The prefix indices among a relation's `indices` (the key columns of each), as
/// a bit set over the prefix lengths: bit `k` for an index on columns `0..k`.
pub const fn prefix_mask(indices: &[&[usize]]) -> u32 {
    let mut mask = 0;
    let mut i = 0;
    while i < indices.len() {
        let cols = indices[i];
        let mut is_prefix = true;
        let mut c = 0;
        while c < cols.len() {
            if cols[c] != c {
                is_prefix = false;
            }
            c += 1;
        }
        if is_prefix && cols.len() < 32 {
            mask |= 1 << cols.len();
        }
        i += 1;
    }
    mask
}

impl<T: LsmTuple, const PREFIXES: u32> LsmStore<T, PREFIXES> {
    /// The number of tuples.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, tuple: &T) -> bool {
        self.set.contains_key(&tuple.encode())
    }

    /// Every tuple, as column references, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = T::Ref<'_>> {
        // SAFETY: every stored blob was produced by `T::encode`.
        self.set.keys().map(|a| unsafe { T::decode(a) })
    }

    fn insert_if_not_present(&mut self, tuple: &T) -> bool {
        let arr = tuple.encode();
        if self.set.contains_key(&arr) {
            false
        } else {
            self.set.insert(arr, ());
            true
        }
    }
}

impl<T: LsmTuple, const PREFIXES: u32> RelIndexMerge for LsmStore<T, PREFIXES> {
    // Ascent only moves `delta` into `total`, and a tuple enters `new` (the next
    // `delta`) only when neither `total` nor `delta` has it, so the two never
    // share a tuple: the runs move over without a membership check.
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.set.absorb(&mut from.set);
    }
}

/// The marker of the full index.
pub struct ToFull<T>(PhantomData<T>);
/// The marker of an index on the first columns, `K`, with the rest, `V`, as
/// values.
pub struct ToPrefix<T, K, V>(PhantomData<(T, K, V)>);
/// The marker of the index on no columns: every tuple, under the key `()`.
pub struct ToScan<T>(PhantomData<T>);

impl<T> Default for ToFull<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<T, K, V> Default for ToPrefix<T, K, V> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<T> Default for ToScan<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

/// The full index's read view.
pub struct FullView<'a, T: LsmTuple, const P: u32>(&'a LsmStore<T, P>);
/// A prefix index's read view.
pub struct PrefixView<'a, T: LsmTuple, K, V, const P: u32>(&'a LsmStore<T, P>, PhantomData<(K, V)>);
/// The "none" index's read view.
pub struct ScanView<'a, T: LsmTuple, const P: u32>(&'a LsmStore<T, P>);

/// The full index's write target: the one way tuples enter the store.
pub struct FullWrite<'a, T: LsmTuple, const P: u32>(&'a mut LsmStore<T, P>);
/// The write target of a view index, which has nothing of its own to write.
pub struct NoopWrite<K, V>(PhantomData<(K, V)>);

impl<T: LsmTuple, const P: u32> ToRelIndex<LsmStore<T, P>> for ToFull<T> {
    type RelIndex<'a>
        = FullView<'a, T, P>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P>) -> Self::RelIndex<'a> {
        FullView(store)
    }

    type RelIndexWrite<'a>
        = FullWrite<'a, T, P>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        store: &'a mut LsmStore<T, P>,
    ) -> Self::RelIndexWrite<'a> {
        FullWrite(store)
    }
}

impl<T: LsmTuple, K: LsmTuple, V: LsmTuple, const P: u32> ToRelIndex<LsmStore<T, P>>
    for ToPrefix<T, K, V>
{
    type RelIndex<'a>
        = PrefixView<'a, T, K, V, P>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P>) -> Self::RelIndex<'a> {
        const {
            assert!(
                size_of::<K::Arr>() + size_of::<V::Arr>() == size_of::<T::Arr>(),
                "a prefix index's key and value must split the tuple's slots",
            );
        }
        PrefixView(store, PhantomData)
    }

    type RelIndexWrite<'a>
        = NoopWrite<K, V>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        _store: &'a mut LsmStore<T, P>,
    ) -> Self::RelIndexWrite<'a> {
        NoopWrite(PhantomData)
    }
}

impl<T: LsmTuple, const P: u32> ToRelIndex<LsmStore<T, P>> for ToScan<T> {
    type RelIndex<'a>
        = ScanView<'a, T, P>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P>) -> Self::RelIndex<'a> {
        ScanView(store)
    }

    type RelIndexWrite<'a>
        = NoopWrite<(), T>
    where
        Self: 'a,
        LsmStore<T, P>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        _store: &'a mut LsmStore<T, P>,
    ) -> Self::RelIndexWrite<'a> {
        NoopWrite(PhantomData)
    }
}

// ---- write targets ----

impl<T: LsmTuple + Clone, const P: u32> RelFullIndexWrite for FullWrite<'_, T, P> {
    type Key = T;
    type Value = ();
    #[inline]
    fn insert_if_not_present(&mut self, key: &T, _v: ()) -> bool {
        self.0.insert_if_not_present(key)
    }
}

impl<T: LsmTuple, const P: u32> RelIndexWrite for FullWrite<'_, T, P> {
    type Key = T;
    type Value = ();
    #[inline]
    fn index_insert(&mut self, key: T, _value: ()) {
        self.0.insert_if_not_present(&key);
    }
}

impl<T: LsmTuple, const P: u32> RelIndexMerge for FullWrite<'_, T, P> {
    #[inline(always)]
    fn move_index_contents(_from: &mut Self, _to: &mut Self) {}
    #[inline(always)]
    fn merge_delta_to_total_new_to_delta(_new: &mut Self, _delta: &mut Self, _total: &mut Self) {}
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

// ---- read views ----

impl<'a, T: LsmTuple + 'a, const P: u32> RelIndexRead<'a> for FullView<'a, T, P> {
    type Key = T;
    type Value = ();
    type IteratorType = Once<()>;
    #[inline]
    fn index_get(&'a self, key: &T) -> Option<Once<()>> {
        self.0.contains(key).then(|| once(()))
    }
    #[inline]
    fn len_estimate(&'a self) -> usize {
        self.0.len()
    }
    #[inline]
    fn is_empty(&'a self) -> bool {
        self.0.is_empty()
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32> RelFullIndexRead<'a> for FullView<'a, T, P> {
    type Key = T;
    #[inline]
    fn contains_key(&'a self, key: &T) -> bool {
        self.0.contains(key)
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32> RelIndexReadAll<'a> for FullView<'a, T, P> {
    type Key = T::Ref<'a>;
    type Value = ();
    type ValueIteratorType = Once<()>;
    type AllIteratorType = FullAllIter<'a, T>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        FullAllIter {
            inner: self.0.set.keys(),
        }
    }
}

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32> RelIndexRead<'a>
    for PrefixView<'a, T, K, V, P>
{
    type Key = K;
    type Value = V::Ref<'a>;
    type IteratorType = SuffixIter<'a, T, V>;
    #[inline]
    fn index_get(&'a self, key: &K) -> Option<Self::IteratorType> {
        let prefix = key.encode();
        let mut keys = self.0.set.keys_with_prefix(words(&prefix)).peekable();
        keys.peek()?;
        Some(SuffixIter {
            keys,
            skip: size_of::<K::Arr>(),
            _marker: PhantomData,
        })
    }
    #[inline]
    fn len_estimate(&'a self) -> usize {
        self.0.len()
    }
    #[inline]
    fn is_empty(&'a self) -> bool {
        self.0.is_empty()
    }
}

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32> RelIndexReadAll<'a>
    for PrefixView<'a, T, K, V, P>
{
    type Key = K::Ref<'a>;
    type Value = V::Ref<'a>;
    type ValueIteratorType = SuffixIter<'a, T, V>;
    type AllIteratorType = PrefixGroupsIter<'a, T, K, V>;
    /// The tuples, grouped by prefix. A prefix the memtable and several runs
    /// hold comes as several groups, which suits Ascent: it reads an index whole
    /// to join each group against another relation, and joining a prefix's
    /// tuples in parts gives the same result.
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        let skip = size_of::<K::Arr>();
        PrefixGroupsIter {
            groups: self.0.set.prefix_groups(skip / size_of::<u64>()),
            skip,
            _marker: PhantomData,
        }
    }
}

/// The groups of a [`PrefixView`]'s `iter_all`.
pub struct PrefixGroupsIter<'a, T: LsmTuple, K, V> {
    groups: PrefixGroups<'a, T::Arr, ()>,
    skip: usize,
    _marker: PhantomData<(K, V)>,
}

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a> Iterator
    for PrefixGroupsIter<'a, T, K, V>
{
    type Item = (K::Ref<'a>, SuffixIter<'a, T, V>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let (first, keys) = self.groups.next()?;
        let head = &bytemuck::bytes_of(first)[..self.skip];
        // SAFETY: `head` is the leading `K` slots of a blob `T::encode` wrote.
        let key = unsafe { K::decode(bytemuck::from_bytes(head)) };
        let values = SuffixIter {
            keys: keys.peekable(),
            skip: self.skip,
            _marker: PhantomData,
        };
        Some((key, values))
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32> RelIndexRead<'a> for ScanView<'a, T, P> {
    type Key = ();
    type Value = T::Ref<'a>;
    type IteratorType = ScanIter<'a, T>;
    #[inline]
    fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
        if self.0.is_empty() {
            return None;
        }
        Some(ScanIter {
            keys: self.0.set.keys(),
        })
    }
    #[inline]
    fn len_estimate(&'a self) -> usize {
        1
    }
    #[inline]
    fn is_empty(&'a self) -> bool {
        self.0.is_empty()
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32> RelIndexReadAll<'a> for ScanView<'a, T, P> {
    type Key = ();
    type Value = T::Ref<'a>;
    type ValueIteratorType = ScanIter<'a, T>;
    type AllIteratorType = std::option::IntoIter<((), ScanIter<'a, T>)>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        self.index_get(&()).map(|vals| ((), vals)).into_iter()
    }
}

/// The value columns of the tuples a prefix read found: each tuple's slots past
/// its first `skip` bytes, decoded as a `V`.
pub struct SuffixIter<'a, T: LsmTuple, V> {
    keys: std::iter::Peekable<KeysIter<'a, T::Arr, ()>>,
    skip: usize,
    _marker: PhantomData<V>,
}

impl<T: LsmTuple, V> Clone for SuffixIter<'_, T, V> {
    fn clone(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            skip: self.skip,
            _marker: PhantomData,
        }
    }
}

impl<'a, T: LsmTuple + 'a, V: LsmTuple + 'a> Iterator for SuffixIter<'a, T, V> {
    type Item = V::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let tuple = self.keys.next()?;
        let tail = &bytemuck::bytes_of(tuple)[self.skip..];
        // SAFETY: `tail` is the trailing `V` slots of a blob `T::encode` wrote.
        Some(unsafe { V::decode(bytemuck::from_bytes(tail)) })
    }
}

/// Every tuple of a store, decoded.
pub struct ScanIter<'a, T: LsmTuple> {
    keys: KeysIter<'a, T::Arr, ()>,
}

impl<T: LsmTuple> Clone for ScanIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            keys: self.keys.clone(),
        }
    }
}

impl<'a, T: LsmTuple + 'a> Iterator for ScanIter<'a, T> {
    type Item = T::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: every stored blob was produced by `T::encode`.
        self.keys.next().map(|a| unsafe { T::decode(a) })
    }
}

// ---------------------------------------------------------------------------
// A physical relation that stores no tuples.
// ---------------------------------------------------------------------------

/// The physical relation of a `counting` LSM relation: it keeps the row count
/// and nothing else, since every row is already in the relation's [`LsmStore`].
/// Ascent pushes each newly derived row exactly once, so `len` is the relation's
/// size. Such a relation cannot be seeded through its field; its rows are
/// derived.
pub struct CountingVec<T> {
    len: usize,
    _marker: PhantomData<T>,
}

impl<T> Default for CountingVec<T> {
    fn default() -> Self {
        Self {
            len: 0,
            _marker: PhantomData,
        }
    }
}

impl<T> CountingVec<T> {
    #[inline]
    pub fn push(&mut self, _v: T) {
        self.len += 1;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn iter(&self) -> std::iter::Empty<&T> {
        std::iter::empty()
    }
}

impl<T> std::ops::Index<usize> for CountingVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("lsm::ascent_provider::CountingVec stores no tuples")
    }
}

/// The physical relation of a `seeded` LSM relation: a [`CountingVec`] that can
/// also be seeded through the relation's field. Ascent reads a relation's field
/// exactly once, when it builds the indices at the start of a run, and `iter`
/// drains the seed then, so the seed rows end up in the relation's store and
/// not also in its field for the rest of the run.
pub struct SeedVec<T> {
    seed: std::cell::RefCell<Vec<T>>,
    len: usize,
}

impl<T> Default for SeedVec<T> {
    fn default() -> Self {
        Self {
            seed: std::cell::RefCell::new(Vec::new()),
            len: 0,
        }
    }
}

impl<T> FromIterator<T> for SeedVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

impl<T> From<Vec<T>> for SeedVec<T> {
    fn from(seed: Vec<T>) -> Self {
        let len = seed.len();
        Self {
            seed: std::cell::RefCell::new(seed),
            len,
        }
    }
}

impl<T> SeedVec<T> {
    #[inline]
    pub fn push(&mut self, _v: T) {
        self.len += 1;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Drains the seed: the next call sees none.
    #[inline]
    pub fn iter(&self) -> std::vec::IntoIter<T> {
        std::mem::take(&mut *self.seed.borrow_mut()).into_iter()
    }
}

impl<T> std::ops::Index<usize> for SeedVec<T> {
    type Output = T;
    fn index(&self, _index: usize) -> &T {
        panic!("lsm::ascent_provider::SeedVec is not indexable")
    }
}

// ---------------------------------------------------------------------------
// The provider macros. Reference this module from a relation's `#[ds(...)]`
// attribute (or the program's `#![ds(...)]`).
// ---------------------------------------------------------------------------

// By default the relation's own field stays an ordinary `Vec` (one in-RAM copy).
// Ascent reads input facts from it (`update_indices`), pushes derived tuples into
// it, and the user reads results from it.
//
// `#[ds(lsm::ascent_provider::provider: counting)]` drops that last copy too: the
// field becomes a [`CountingVec`], which keeps only the row count, for a derived
// relation whose rows are read after the run from its store (or not at all).
// `: seeded` is the same for a relation that also takes input rows through its
// field: a [`SeedVec`], which hands them to the indices once and keeps none.
#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel {
    ($name:ident, $field_types:ty, $indices:expr, ser, ()) => {
        ::std::vec::Vec<$field_types>
    };
    ($name:ident, $field_types:ty, $indices:expr, ser, (counting)) => {
        $crate::ascent_provider::CountingVec<$field_types>
    };
    ($name:ident, $field_types:ty, $indices:expr, ser, (seeded)) => {
        $crate::ascent_provider::SeedVec<$field_types>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_codegen {
    ( $($tt:tt)* ) => {};
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_ind_common {
    ($name:ident, $field_types:ty, [$([$($col:literal),*]),*], ser, $args:tt) => {
        $crate::ascent_provider::LsmStore<
            $field_types,
            { $crate::ascent_provider::prefix_mask(&[$(&[$($col),*]),*]) },
        >
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_full_ind {
    ($name:ident, $field_types:ty, $indices:expr, ser, $args:tt, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToFull<$field_types>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_ind {
    ($name:ident, $field_types:ty, $indices:expr, ser, $args:tt, $ind:tt, $key:ty, $val:ty) => {
        $crate::lsm_nary_index_for!($ind, $field_types, $key, $val)
    };
}

/// The index type for the key columns `$ind`: a view of the relation's store
/// when they are a prefix of its columns, its own [`LsmIndex`] otherwise.
#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_index_for {
    ([], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToScan<$t>
    };
    ([0], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToPrefix<$t, $key, $val>
    };
    ([0, 1], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToPrefix<$t, $key, $val>
    };
    ([0, 1, 2], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToPrefix<$t, $key, $val>
    };
    ([0, 1, 2, 3], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToPrefix<$t, $key, $val>
    };
    ([0, 1, 2, 3, 4], $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::ToPrefix<$t, $key, $val>
    };
    ($ind:tt, $t:ty, $key:ty, $val:ty) => {
        $crate::ascent_provider::LsmIndex<$key, $val>
    };
}

/// The macro set Ascent's `#[ds(...)]` machinery looks for. Point a relation's
/// `#[ds(lsm::ascent_provider::provider)]` (or the program's `#![ds(...)]`) here.
pub mod provider {
    pub use crate::{
        lsm_nary_rel as rel, lsm_nary_rel_codegen as rel_codegen,
        lsm_nary_rel_full_ind as rel_full_ind, lsm_nary_rel_ind as rel_ind,
        lsm_nary_rel_ind_common as rel_ind_common,
    };
}
