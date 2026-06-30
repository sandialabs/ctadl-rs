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
//! This provider keeps the same shape (each index owns its own storage, the
//! shared `rel_ind_common` is `()`) but replaces each `HashMap` with an
//! [`MmapLsmMultiMap`]. The bulk of every index therefore lives in `mmap`'d run
//! files (off the heap, in the page cache); only a small write buffer is in RAM.
//!
//! # Encoding
//!
//! Reads from `MmapLsmMultiMap` hand back `&T` references straight into the
//! mapping, so keys and values must be fixed-width `bytemuck::Pod`. Relation
//! columns generally are *not* `Pod` (e.g. interned `&'static` pointers like
//! `Path`/`CallString`). Each column type implements [`LsmCol`], which encodes a
//! value into a fixed number of `u64` "slots" and reinterprets those slots back
//! as a `&Column`.
//!
//! For pointer-backed interned types the slot holds the raw pointer bits. This is
//! only valid **within a single process run** (the run files are per-process temp
//! files and the interner is never freed) and assumes a **little-endian** target
//! so that the value's bytes occupy the low end of each slot. Both hold for the
//! intended use (one `ctadl index` run on x86-64 / arm64).

use std::iter::{Once, once};
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use bytemuck::{Pod, Zeroable};

use crate::mmap::{IterAllRefs, MmapLsmMultiMap, ValsRefs};

// ---------------------------------------------------------------------------
// Process-wide per-map memtable limit.
//
// Every index map this provider creates (one per index pattern Ascent
// generates, times three versions: new/delta/total) uses the same memtable
// limit: the map's in-memory write buffer flushes to a run file once it holds
// this many entries. Callers pick the value (e.g. derived from available RAM)
// via [`set_memtable_limit`]; this crate stays policy-free.
// ---------------------------------------------------------------------------

/// Per-map memtable limit (entries) used until [`set_memtable_limit`] overrides it.
pub const DEFAULT_MEMTABLE_LIMIT: usize = 100_000;

/// The per-map memtable limit, in entries.
static MEMTABLE_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_MEMTABLE_LIMIT);

/// Sets the per-map memtable limit, in entries (`(key, value)` insertions): each
/// index map flushes its in-memory buffer to disk once it holds this many. Larger
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
/// * any slot blob produced by `to_slots` from a live value remains a valid
///   `Self` for as long as the slots are read back (process lifetime for
///   pointer-backed interned types).
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
}

/// Implements [`LsmCol`] for a `Copy` type that fits in `$n` eight-byte slots.
///
/// `$n` must satisfy `size_of::<$t>() <= 8 * $n` and `align_of::<$t>() <= 8`
/// (both are checked at compile time). The target must be little-endian.
#[macro_export]
macro_rules! lsm_col {
    ($t:ty, $n:literal) => {
        // SAFETY: the asserts below enforce the size/alignment invariants; the
        // caller is responsible for little-endianness and process-local validity.
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
                unsafe { &*(s.as_ptr() as *const $t) }
            }
        }
    };
}

// Built-in column encodings for the integer types used by tests / primitive
// columns. (Downstream crates implement `LsmCol` for their own column types via
// the `lsm_col!` macro.)
lsm_col!(u8, 1);
lsm_col!(u16, 1);
lsm_col!(u32, 1);
lsm_col!(u64, 1);
lsm_col!(i8, 1);
lsm_col!(i16, 1);
lsm_col!(i32, 1);
lsm_col!(i64, 1);

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
}

// arity 0 — only ever a *key* (the "no index" full scan). Uses a one-slot
// sentinel so the storage is never a zero-sized type.
impl LsmTuple for () {
    type Arr = [u64; 1];
    type Ref<'a> = () where Self: 'a;
    #[inline]
    fn encode(&self) -> [u64; 1] {
        [0]
    }
    #[inline]
    unsafe fn decode(_a: &[u64; 1]) -> () {}
}

// Arities 1..=6. The `Arr` is a right-folded `Cat` of each column's slots, e.g.
// arity-3 is `Cat<A, Cat<B, C>>`, so field `b` is `a.1.0`, `c` is `a.1.1`, etc.
// `decode` is written as a real method (not a closure) so the output references
// are tied to the lifetime of the input `&Self::Arr`.

impl<A: LsmCol> LsmTuple for (A,) {
    type Arr = A::Slots;
    type Ref<'a> = (&'a A,) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        self.0.to_slots()
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (unsafe { A::ref_from(a) },)
    }
}

impl<A: LsmCol, B: LsmCol> LsmTuple for (A, B) {
    type Arr = Cat<A::Slots, B::Slots>;
    type Ref<'a> = (&'a A, &'a B) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(self.0.to_slots(), self.1.to_slots())
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (unsafe { A::ref_from(&a.0) }, unsafe { B::ref_from(&a.1) })
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol> LsmTuple for (A, B, C) {
    type Arr = Cat<A::Slots, Cat<B::Slots, C::Slots>>;
    type Ref<'a> = (&'a A, &'a B, &'a C) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(self.0.to_slots(), Cat(self.1.to_slots(), self.2.to_slots()))
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (
            unsafe { A::ref_from(&a.0) },
            unsafe { B::ref_from(&a.1.0) },
            unsafe { C::ref_from(&a.1.1) },
        )
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol> LsmTuple for (A, B, C, D) {
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, D::Slots>>>;
    type Ref<'a> = (&'a A, &'a B, &'a C, &'a D) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(self.0.to_slots(), Cat(self.1.to_slots(), Cat(self.2.to_slots(), self.3.to_slots())))
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (
            unsafe { A::ref_from(&a.0) },
            unsafe { B::ref_from(&a.1.0) },
            unsafe { C::ref_from(&a.1.1.0) },
            unsafe { D::ref_from(&a.1.1.1) },
        )
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol, E: LsmCol> LsmTuple for (A, B, C, D, E) {
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, Cat<D::Slots, E::Slots>>>>;
    type Ref<'a> = (&'a A, &'a B, &'a C, &'a D, &'a E) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(
            self.0.to_slots(),
            Cat(self.1.to_slots(), Cat(self.2.to_slots(), Cat(self.3.to_slots(), self.4.to_slots()))),
        )
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (
            unsafe { A::ref_from(&a.0) },
            unsafe { B::ref_from(&a.1.0) },
            unsafe { C::ref_from(&a.1.1.0) },
            unsafe { D::ref_from(&a.1.1.1.0) },
            unsafe { E::ref_from(&a.1.1.1.1) },
        )
    }
}

impl<A: LsmCol, B: LsmCol, C: LsmCol, D: LsmCol, E: LsmCol, F: LsmCol> LsmTuple
    for (A, B, C, D, E, F)
{
    type Arr = Cat<A::Slots, Cat<B::Slots, Cat<C::Slots, Cat<D::Slots, Cat<E::Slots, F::Slots>>>>>;
    type Ref<'a> = (&'a A, &'a B, &'a C, &'a D, &'a E, &'a F) where Self: 'a;
    #[inline]
    fn encode(&self) -> Self::Arr {
        Cat(
            self.0.to_slots(),
            Cat(
                self.1.to_slots(),
                Cat(self.2.to_slots(), Cat(self.3.to_slots(), Cat(self.4.to_slots(), self.5.to_slots()))),
            ),
        )
    }
    #[inline]
    unsafe fn decode(a: &Self::Arr) -> Self::Ref<'_> {
        (
            unsafe { A::ref_from(&a.0) },
            unsafe { B::ref_from(&a.1.0) },
            unsafe { C::ref_from(&a.1.1.0) },
            unsafe { D::ref_from(&a.1.1.1.0) },
            unsafe { E::ref_from(&a.1.1.1.1.0) },
            unsafe { F::ref_from(&a.1.1.1.1.1) },
        )
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
        Self { inner: self.inner.clone() }
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

/// Iterator over every key of an [`LsmFullIndex`] (values are unit).
pub struct FullAllIter<'a, K: LsmTuple> {
    inner: IterAllRefs<'a, K::Arr, u8>,
}

impl<'a, K: LsmTuple + 'a> Iterator for FullAllIter<'a, K> {
    type Item = (K::Ref<'a>, Once<()>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the stored key blob was produced by `K::encode`.
        self.inner.next().map(|(k, _)| (unsafe { K::decode(k) }, once(())))
    }
}

// ---------------------------------------------------------------------------
// A non-full index: key -> remaining columns, backed by one LSM map.
// ---------------------------------------------------------------------------

/// An Ascent index for key columns `K` mapping to value columns `V`, backed by a
/// single [`MmapLsmMultiMap`]. This is the n-ary analogue of the default
/// provider's `RelIndexType1<K, V>` (a `HashMap<K, Vec<V>>`).
pub struct LsmIndex<K: LsmTuple, V: LsmTuple> {
    map: MmapLsmMultiMap<K::Arr, V::Arr>,
}

impl<K: LsmTuple, V: LsmTuple> Default for LsmIndex<K, V> {
    fn default() -> Self {
        Self { map: MmapLsmMultiMap::with_memtable_limit(memtable_limit()) }
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
        AllIter { inner: self.map.iter_all_refs() }
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
        for (k, vals) in from.map.iter_all_refs() {
            for v in vals {
                to.map.insert(*k, *v);
            }
        }
        from.map.clear();
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
// The full index: a set of full tuples, used for dedup / membership.
// ---------------------------------------------------------------------------

/// An Ascent full index over key columns `K` (the whole tuple), backed by an LSM
/// map used as a set (`K -> ()`). Analogue of the default provider's
/// `RelFullIndexType<K, ()>`.
pub struct LsmFullIndex<K: LsmTuple> {
    map: MmapLsmMultiMap<K::Arr, u8>,
}

impl<K: LsmTuple> Default for LsmFullIndex<K> {
    fn default() -> Self {
        Self { map: MmapLsmMultiMap::with_memtable_limit(memtable_limit()) }
    }
}

impl<K: LsmTuple> LsmFullIndex<K> {
    #[inline]
    fn contains_arr(&self, arr: &K::Arr) -> bool {
        self.map.get_refs(arr).is_some()
    }
}

impl<'a, K: LsmTuple + 'a> RelIndexRead<'a> for LsmFullIndex<K> {
    type Key = K;
    type Value = ();
    type IteratorType = Once<()>;

    #[inline]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        if self.contains_arr(&key.encode()) { Some(once(())) } else { None }
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

impl<'a, K: LsmTuple + 'a> RelIndexReadAll<'a> for LsmFullIndex<K> {
    type Key = K::Ref<'a>;
    type Value = ();
    type ValueIteratorType = Once<()>;
    type AllIteratorType = FullAllIter<'a, K>;

    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        FullAllIter { inner: self.map.iter_all_refs() }
    }
}

impl<'a, K: LsmTuple> RelFullIndexRead<'a> for LsmFullIndex<K> {
    type Key = K;
    #[inline]
    fn contains_key(&'a self, key: &Self::Key) -> bool {
        self.contains_arr(&key.encode())
    }
}

impl<K: LsmTuple + Clone> RelFullIndexWrite for LsmFullIndex<K> {
    type Key = K;
    type Value = ();

    #[inline]
    fn insert_if_not_present(&mut self, key: &Self::Key, _v: Self::Value) -> bool {
        let arr = key.encode();
        if self.contains_arr(&arr) {
            false
        } else {
            self.map.insert(arr, 0u8);
            true
        }
    }
}

impl<K: LsmTuple> RelIndexWrite for LsmFullIndex<K> {
    type Key = K;
    type Value = ();

    #[inline]
    fn index_insert(&mut self, key: Self::Key, _value: Self::Value) {
        let arr = key.encode();
        if !self.contains_arr(&arr) {
            self.map.insert(arr, 0u8);
        }
    }
}

impl<K: LsmTuple> RelIndexMerge for LsmFullIndex<K> {
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        for (k, _) in from.map.iter_all_refs() {
            if to.map.get_refs(k).is_none() {
                to.map.insert(*k, 0u8);
            }
        }
        from.map.clear();
    }
}

impl<K: LsmTuple, Rel> ToRelIndex<Rel> for LsmFullIndex<K> {
    type RelIndex<'a>
        = &'a LsmFullIndex<K>
    where
        Self: 'a,
        Rel: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, _rel: &'a Rel) -> Self::RelIndex<'a> {
        self
    }

    type RelIndexWrite<'a>
        = &'a mut LsmFullIndex<K>
    where
        Self: 'a,
        Rel: 'a;
    #[inline]
    fn to_rel_index_write<'a>(&'a mut self, _rel: &'a mut Rel) -> Self::RelIndexWrite<'a> {
        self
    }
}

// ---------------------------------------------------------------------------
// The provider macros. Reference this module from a relation's `#[ds(...)]`
// attribute (or the program's `#![ds(...)]`).
// ---------------------------------------------------------------------------

// The relation's own field stays an ordinary `Vec` (one in-RAM copy). Ascent
// reads input facts from it (`update_indices`), pushes derived tuples into it,
// and the user reads results from it. The memory win comes from the *indices*:
// Ascent builds several per relation, each in three versions (new/delta/total),
// and this provider keeps those in `mmap`'d LSM files instead of in-RAM hash
// maps — so the data is no longer duplicated across many heap structures.
#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel {
    ($name:ident, $field_types:ty, $indices:expr, ser, ()) => {
        ::std::vec::Vec<$field_types>
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
    ($name:ident, $field_types:ty, $indices:expr, ser, ()) => {
        ()
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_full_ind {
    ($name:ident, $field_types:ty, $indices:expr, ser, (), $key:ty, $val:ty) => {
        $crate::ascent_provider::LsmFullIndex<$key>
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! lsm_nary_rel_ind {
    ($name:ident, $field_types:ty, $indices:expr, ser, (), $ind:expr, $key:ty, $val:ty) => {
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
