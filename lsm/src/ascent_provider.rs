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
//! meaningful to their interner). Each column type implements [`LsmCol`], which
//! writes a value's bytes into a blob and reinterprets them in place as a
//! `&Column`.
//!
//! A tuple is stored as one blob of `u64` words, with each column's bytes at a
//! fixed offset given by the tuple's [`Layout`], which [`store_layout`] computes
//! from the columns' sizes and alignments and the relation's prefix indices.
//! Each column starts on a word of its own, in declared order, and the bytes
//! after it up to the next word are zero. Every column sits at an offset aligned
//! for its type, so a read hands out a `&Column` into the blob, and the bytes
//! are written and read in native byte order.
//!
//! For interned types the blob holds the handle's raw bits. This is only valid
//! **within a single process run**: the run files are per-process temp files, and
//! the interned values must outlive them.

use std::iter::{Once, once};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::internal::{
    RelFullIndexRead, RelFullIndexWrite, RelIndexMerge, RelIndexRead, RelIndexReadAll,
    RelIndexWrite, ToRelIndex,
};
use smallvec::SmallVec;

use crate::mmap::{IterAllRefs, KeysIter, MmapLsmMultiMap, PrefixGroups, ValsRefs};

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
// Column encoding.
// ---------------------------------------------------------------------------

/// A relation column, stored as its own bytes at an aligned offset of a tuple's
/// blob and read back in place.
///
/// # Safety
///
/// Implementors must guarantee:
/// * `align_of::<Self>() <= 8`, so that an offset aligned for `Self` in an
///   8-aligned blob is aligned in memory;
/// * `write_to` writes exactly the first `size_of::<Self>()` bytes of `dst`,
///   every one initialized, and equal values write equal bytes: the stores sort
///   and deduplicate by bytes;
/// * bytes `write_to` wrote from a live value, at an offset aligned for `Self`,
///   are a valid `Self` for as long as they are read back through `ref_at`
///   (process lifetime for interned types).
///
/// Use the [`lsm_col!`](crate::lsm_col) macro to implement this for a `Copy`
/// type without padding.
pub unsafe trait LsmCol: Copy + 'static {
    /// Writes the value's `size_of::<Self>()` bytes to the start of `dst`.
    /// Panics if `dst` is shorter.
    fn write_to(self, dst: &mut [u8]);

    /// Reinterprets the bytes of `blob` at `offset` as a reference to this
    /// column.
    ///
    /// # Safety
    /// `blob[offset..offset + size_of::<Self>()]` must hold bytes that
    /// [`LsmCol::write_to`] wrote from a still-live value, and
    /// `blob.as_ptr().add(offset)` must be aligned for `Self`.
    unsafe fn ref_at(blob: &[u8], offset: usize) -> &Self;

    /// Whether every interned handle in this value was handed out by its interner.
    /// `ref_at` asserts it in debug builds, since the stored bytes bypassed the
    /// interners' checked conversions.
    #[inline]
    fn is_valid(&self) -> bool {
        true
    }
}

/// Implements [`LsmCol`] for a `Copy` type without padding and with alignment
/// at most 8.
///
/// The bracketed list names the types of `$t`'s fields, or `$t` itself for a
/// primitive. Their sizes must sum to `size_of::<$t>()`, which is checked at
/// compile time: a value's bytes are copied into the blob and compared there,
/// so a padding byte, which is uninitialized, must not exist. The alignment
/// bound is checked at compile time too.
///
/// `valid = f` names a `fn(&$t) -> bool` that becomes [`LsmCol::is_valid`], for
/// a type that holds interned handles.
///
/// ```
/// # #[derive(Clone, Copy)] struct Pair(u32, u32);
/// lsm::lsm_col!(Pair, [u32, u32]);
/// ```
///
/// A type with padding:
///
/// ```compile_fail
/// # #[derive(Clone, Copy)] struct Padded(u64, u32);
/// lsm::lsm_col!(Padded, [u64, u32]);
/// ```
///
/// An incomplete field list:
///
/// ```compile_fail
/// # #[derive(Clone, Copy)] struct Pair(u32, u32);
/// lsm::lsm_col!(Pair, [u32]);
/// ```
///
/// An alignment above 8:
///
/// ```compile_fail
/// # #[derive(Clone, Copy)] #[repr(align(16))] struct Wide(u64, u64);
/// lsm::lsm_col!(Wide, [u64, u64]);
/// ```
#[macro_export]
macro_rules! lsm_col {
    ($t:ty, [$($field:ty),+ $(,)?] $(, valid = $valid:expr)? $(,)?) => {
        // SAFETY: the asserts in `write_to` enforce that `$t` is at most
        // 8-aligned and has no padding, so `write_to` copies `size_of::<$t>()`
        // initialized bytes, the value's own. The caller is responsible for
        // process-local validity.
        unsafe impl $crate::ascent_provider::LsmCol for $t {
            #[inline]
            fn write_to(self, dst: &mut [u8]) {
                const _: () = assert!(
                    ::core::mem::align_of::<$t>() <= 8,
                    "lsm_col!: type alignment exceeds 8",
                );
                const _: () = assert!(
                    ::core::mem::size_of::<$t>() == 0 $(+ ::core::mem::size_of::<$field>())+,
                    "lsm_col!: type has padding, or the field list is incomplete",
                );
                // SAFETY: `$t` has no padding (asserted above), so all of its
                // `size_of` bytes are initialized.
                let src = unsafe {
                    ::core::slice::from_raw_parts(
                        &self as *const $t as *const u8,
                        ::core::mem::size_of::<$t>(),
                    )
                };
                dst[..::core::mem::size_of::<$t>()].copy_from_slice(src);
            }

            #[inline]
            unsafe fn ref_at(blob: &[u8], offset: usize) -> &$t {
                debug_assert!(offset + ::core::mem::size_of::<$t>() <= blob.len());
                // SAFETY: by the caller's contract, the bytes at `offset` are in
                // bounds, aligned for `$t`, and hold a valid `$t` that `write_to`
                // wrote.
                let value = unsafe { &*(blob.as_ptr().add(offset) as *const $t) };
                debug_assert!((value as *const $t).is_aligned());
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
lsm_col!(u8, [u8]);
lsm_col!(u16, [u16]);
lsm_col!(u32, [u32]);
lsm_col!(u64, [u64]);
lsm_col!(i8, [i8]);
lsm_col!(i16, [i16]);
lsm_col!(i32, [i32]);
lsm_col!(i64, [i64]);

// A pair of columns, for a relation that carries two values in one column. It
// is stored as the pair itself, so a reference into the blob is a `&(A, B)`.
// The pair may have padding: `write_to` zeroes the pair's bytes and then writes
// each field through its own `write_to`, so the padding bytes are zeros, never
// uninitialized bytes copied from the value.
//
// SAFETY: `write_to` asserts the pair is at most 8-aligned. It writes exactly
// `size_of::<(A, B)>()` bytes, every one initialized: zeroed, then overwritten
// with the fields' initialized bytes at their places in the pair, so equal pairs
// give equal bytes. Validity of the reinterpreted fields is inherited from
// `A`/`B`, and each field is aligned within an aligned pair.
unsafe impl<A: LsmCol, B: LsmCol> LsmCol for (A, B) {
    #[inline]
    fn write_to(self, dst: &mut [u8]) {
        const { assert!(align_of::<(A, B)>() <= 8) }
        let dst = &mut dst[..size_of::<(A, B)>()];
        dst.fill(0);
        self.0.write_to(&mut dst[std::mem::offset_of!((A, B), 0)..]);
        self.1.write_to(&mut dst[std::mem::offset_of!((A, B), 1)..]);
    }

    #[inline]
    unsafe fn ref_at(blob: &[u8], offset: usize) -> &Self {
        debug_assert!(offset + size_of::<(A, B)>() <= blob.len());
        // SAFETY: by the caller's contract, the bytes at `offset` are in bounds,
        // aligned for `(A, B)`, and hold the fields `write_to` wrote from a live
        // pair.
        let pair = unsafe { &*(blob.as_ptr().add(offset) as *const (A, B)) };
        debug_assert!((pair as *const (A, B)).is_aligned());
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
// Tuple encoding: an N-column tuple <-> its columns at a layout's offsets.
// ---------------------------------------------------------------------------

/// A tuple of [`LsmCol`] columns, written into a blob at given offsets and read
/// back in place as a tuple of borrowed columns.
///
/// The offsets come from a [`Layout`]: a store's layout for its whole tuples,
/// and a slice of it for the key or value columns of a prefix index.
pub trait LsmTuple {
    /// Each column's `size_of`, in order.
    const SIZES: &'static [usize];
    /// Each column's `align_of`, in order.
    const ALIGNS: &'static [usize];

    /// A tuple of references into a stored blob. Outlives `'a` because it is a
    /// tuple of `&'a Column` (and columns are `'static`).
    type Ref<'a>: 'a
    where
        Self: 'a;

    /// Writes column `i` at `blob[offsets[i]..]`, leaving the other bytes alone.
    fn encode_at(&self, blob: &mut [u8], offsets: &[usize]);

    /// Reinterprets the columns at `offsets` in `blob` as a tuple of column
    /// references.
    ///
    /// # Safety
    /// For each column `i`, the bytes at `blob[offsets[i]..]` must satisfy
    /// [`LsmCol::ref_at`]'s contract for that column's type: written by its
    /// `write_to` from a live value, at an address aligned for it.
    unsafe fn decode_at<'a>(blob: &'a [u8], offsets: &[usize]) -> Self::Ref<'a>;
}

// Arity 0: only ever a key (the "no index" full scan), which has no columns.
impl LsmTuple for () {
    const SIZES: &'static [usize] = &[];
    const ALIGNS: &'static [usize] = &[];
    type Ref<'a>
        = ()
    where
        Self: 'a;
    #[inline]
    fn encode_at(&self, _blob: &mut [u8], _offsets: &[usize]) {}
    #[inline]
    unsafe fn decode_at(_blob: &[u8], _offsets: &[usize]) {}
}

/// Implements [`LsmTuple`] for the tuple of the given columns, each named by its
/// tuple index and type parameter.
macro_rules! lsm_tuple {
    ($($i:tt $c:ident),+) => {
        impl<$($c: LsmCol),+> LsmTuple for ($($c,)+) {
            const SIZES: &'static [usize] = &[$(size_of::<$c>()),+];
            const ALIGNS: &'static [usize] = &[$(align_of::<$c>()),+];
            type Ref<'a>
                = ($(&'a $c,)+)
            where
                Self: 'a;
            #[inline]
            fn encode_at(&self, blob: &mut [u8], offsets: &[usize]) {
                $(self.$i.write_to(&mut blob[offsets[$i]..]);)+
            }
            #[inline]
            unsafe fn decode_at<'a>(blob: &'a [u8], offsets: &[usize]) -> Self::Ref<'a> {
                // SAFETY: forwarded from the caller, column by column.
                unsafe { ($($c::ref_at(blob, offsets[$i]),)+) }
            }
        }
    };
}

lsm_tuple!(0 A);
lsm_tuple!(0 A, 1 B);
lsm_tuple!(0 A, 1 B, 2 C);
lsm_tuple!(0 A, 1 B, 2 C, 3 D);
lsm_tuple!(0 A, 1 B, 2 C, 3 D, 4 E);
lsm_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);

// ---------------------------------------------------------------------------
// An index whose key columns are not a prefix of the relation's: key ->
// remaining columns, backed by an LSM map of its own.
// ---------------------------------------------------------------------------

/// An Ascent index for key columns `K` mapping to value columns `V`, backed by a
/// single [`MmapLsmMultiMap`]. This is the n-ary analogue of the default
/// provider's `RelIndexType1<K, V>` (a `HashMap<K, Vec<V>>`).
///
/// Keys and values are blobs of `KW` and `VW` words, laid out as tuples with no
/// prefix boundaries, since the map reads them only whole. The widths must be
/// the ones [`store_layout`] gives, which is checked at compile time.
pub struct LsmIndex<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize> {
    map: MmapLsmMultiMap<[u64; KW], [u64; VW]>,
    _marker: PhantomData<(K, V)>,
}

impl<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize> LsmIndex<K, V, KW, VW> {
    const K_LAYOUT: Layout = store_layout(K::SIZES, K::ALIGNS, 0);
    const V_LAYOUT: Layout = store_layout(V::SIZES, V::ALIGNS, 0);

    #[inline]
    fn encode_key(key: &K) -> [u64; KW] {
        let mut blob = [0; KW];
        key.encode_at(bytemuck::bytes_of_mut(&mut blob), Self::K_LAYOUT.offsets());
        blob
    }

    #[inline]
    fn encode_value(value: &V) -> [u64; VW] {
        let mut blob = [0; VW];
        value.encode_at(bytemuck::bytes_of_mut(&mut blob), Self::V_LAYOUT.offsets());
        blob
    }
}

impl<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize> Default
    for LsmIndex<K, V, KW, VW>
{
    fn default() -> Self {
        const {
            assert!(
                KW == Self::K_LAYOUT.words && VW == Self::V_LAYOUT.words,
                "LsmIndex: KW and VW must be the key's and value's widths",
            );
        }
        Self {
            map: MmapLsmMultiMap::with_memtable_limit(memtable_limit()),
            _marker: PhantomData,
        }
    }
}

/// Iterator over a key's values, decoding each stored blob into a tuple of
/// column references on the fly. Cheap to clone (as Ascent's `IteratorType`
/// requires) because the underlying [`ValsRefs`] is.
pub struct ValIter<'a, K, V, const KW: usize, const VW: usize> {
    inner: ValsRefs<'a, [u64; VW]>,
    _marker: PhantomData<(K, V)>,
}

impl<K, V, const KW: usize, const VW: usize> Clone for ValIter<'_, K, V, KW, VW> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const KW: usize, const VW: usize> Iterator
    for ValIter<'a, K, V, KW, VW>
{
    type Item = V::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let offsets = LsmIndex::<K, V, KW, VW>::V_LAYOUT.offsets();
        // SAFETY: every stored value blob was written by `encode_value`, with
        // these offsets, from a live value.
        self.inner
            .next()
            .map(|v| unsafe { V::decode_at(bytemuck::bytes_of(v), offsets) })
    }
}

/// Iterator over every `(key, values)` group of an [`LsmIndex`].
pub struct AllIter<'a, K, V, const KW: usize, const VW: usize> {
    inner: IterAllRefs<'a, [u64; KW], [u64; VW]>,
    _marker: PhantomData<(K, V)>,
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const KW: usize, const VW: usize> Iterator
    for AllIter<'a, K, V, KW, VW>
{
    type Item = (K::Ref<'a>, ValIter<'a, K, V, KW, VW>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let offsets = LsmIndex::<K, V, KW, VW>::K_LAYOUT.offsets();
        self.inner.next().map(|(k, vals)| {
            // SAFETY: every stored key blob was written by `encode_key`, with
            // these offsets, from a live key.
            let key = unsafe { K::decode_at(bytemuck::bytes_of(k), offsets) };
            let vals = ValIter {
                inner: vals,
                _marker: PhantomData,
            };
            (key, vals)
        })
    }
}

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const KW: usize, const VW: usize> RelIndexRead<'a>
    for LsmIndex<K, V, KW, VW>
{
    type Key = K;
    type Value = V::Ref<'a>;
    type IteratorType = ValIter<'a, K, V, KW, VW>;

    #[inline]
    fn index_get(&'a self, key: &Self::Key) -> Option<Self::IteratorType> {
        let vals = self.map.get_refs(&Self::encode_key(key))?;
        Some(ValIter {
            inner: vals,
            _marker: PhantomData,
        })
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

impl<'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const KW: usize, const VW: usize> RelIndexReadAll<'a>
    for LsmIndex<K, V, KW, VW>
{
    type Key = K::Ref<'a>;
    type Value = V::Ref<'a>;
    type ValueIteratorType = ValIter<'a, K, V, KW, VW>;
    type AllIteratorType = AllIter<'a, K, V, KW, VW>;

    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        AllIter {
            inner: self.map.iter_all_refs(),
            _marker: PhantomData,
        }
    }
}

impl<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize> RelIndexWrite
    for LsmIndex<K, V, KW, VW>
{
    type Key = K;
    type Value = V;

    #[inline]
    fn index_insert(&mut self, key: Self::Key, value: Self::Value) {
        self.map
            .insert(Self::encode_key(&key), Self::encode_value(&value));
    }
}

impl<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize> RelIndexMerge
    for LsmIndex<K, V, KW, VW>
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        to.map.absorb(&mut from.map);
    }
}

impl<K: LsmTuple, V: LsmTuple, const KW: usize, const VW: usize, Rel> ToRelIndex<Rel>
    for LsmIndex<K, V, KW, VW>
{
    type RelIndex<'a>
        = &'a LsmIndex<K, V, KW, VW>
    where
        Self: 'a,
        Rel: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, _rel: &'a Rel) -> Self::RelIndex<'a> {
        self
    }

    type RelIndexWrite<'a>
        = &'a mut LsmIndex<K, V, KW, VW>
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
// A tuple's blob is its columns' bytes at its layout's offsets, and the blobs'
// order is the lexicographic order of their `u64` words. Every prefix index's
// key columns end on a word boundary of the layout, so the tuples whose first
// `k` columns are given are one contiguous range of the set, and their
// remaining columns are in the words after the prefix. Every index whose key
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
/// prefixes, so a read skips the runs that hold none of its prefix. Each tuple
/// is a blob of `W` words, which must be the width [`store_layout`] gives for
/// `T` and `PREFIXES`; that is checked at compile time.
pub struct LsmStore<T: LsmTuple, const PREFIXES: u32, const W: usize> {
    set: MmapLsmMultiMap<[u64; W], ()>,
    _marker: PhantomData<T>,
}

impl<T: LsmTuple, const PREFIXES: u32, const W: usize> Default for LsmStore<T, PREFIXES, W> {
    fn default() -> Self {
        const {
            assert!(
                W == Self::LAYOUT.words,
                "LsmStore: W must be the tuple's width"
            );
        }
        let mut lens: SmallVec<[usize; 2]> = SmallVec::new();
        for k in 1..T::SIZES.len() {
            if PREFIXES & (1 << k) != 0 {
                lens.push(Self::LAYOUT.prefix_bytes(k));
            }
        }
        Self {
            set: MmapLsmMultiMap::with_memtable_limit(memtable_limit()).with_prefix_reads(&lens),
            _marker: PhantomData,
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

/// The most columns a tuple has.
const MAX_COLS: usize = 6;

/// Where each column of a tuple lives in its blob, for one set of prefix
/// boundaries; computed by [`store_layout`].
///
/// Offsets and segment ends are in bytes from the start of the blob. The blob
/// is `words` `u64`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Byte offset of each column, in declared column order.
    offsets: [usize; MAX_COLS],
    /// End of each segment, rounded up to a multiple of eight.
    seg_ends: [usize; MAX_COLS],
    /// The column index one past each segment's last column.
    seg_cols: [usize; MAX_COLS],
    n_cols: usize,
    n_segs: usize,
    /// The blob's width in words.
    pub words: usize,
}

impl Layout {
    /// The byte offset of each column, in declared column order.
    pub const fn offsets(&self) -> &[usize] {
        self.offsets.split_at(self.n_cols).0
    }

    /// The end of each segment in bytes, a multiple of eight, in order.
    pub const fn seg_ends(&self) -> &[usize] {
        self.seg_ends.split_at(self.n_segs).0
    }

    /// The number of columns.
    pub const fn n_cols(&self) -> usize {
        self.n_cols
    }

    /// The width in bytes of a prefix of the first `k` columns: the end of the
    /// segment whose last column is `k - 1`, or 0 for `k == 0`.
    ///
    /// Panics unless `k` is 0, a prefix boundary or the number of columns.
    pub const fn prefix_bytes(&self, k: usize) -> usize {
        if k == 0 {
            return 0;
        }
        let mut s = 0;
        while s < self.n_segs {
            if self.seg_cols[s] == k {
                return self.seg_ends[s];
            }
            s += 1;
        }
        panic!("prefix_bytes: k is not a segment boundary");
    }
}

/// The layout of a tuple whose columns have the given `sizes` and `aligns` (in
/// declared order), under the prefix boundaries in `prefixes` (bit `k` for a
/// prefix of `k` columns, as [`prefix_mask`] computes).
///
/// The rules:
///
/// 1. The bits `k` with `0 < k < n` (for `n` columns) split the columns into
///    segments. Bit 0 (a scan) and bit `n` (the full tuple) split nothing.
/// 2. Within a segment, columns are placed in order of decreasing alignment,
///    ties in declared order. Every column then lands at a multiple of its
///    alignment with no padding between columns, since each earlier column's
///    size is a multiple of an alignment at least as large.
/// 3. Each segment starts at a multiple of eight; the bytes from its last
///    column to the next multiple of eight are padding.
/// 4. The width is the last segment's end in words, at least one.
///
/// Segments start on words because the stores order blobs word by word, as
/// native `u64`s: a word holding the end of one prefix and the start of the
/// rest would interleave the bytes, and tuples sharing that prefix would not
/// be contiguous. Whole-word prefixes also let prefix reads compare `u64`s.
/// Within a segment, column order changes where a column lives, never which
/// tuples share a prefix. Columns keep their alignment because reads hand out
/// `&Column` into the blob, which is itself 8-aligned.
///
/// The result depends only on the arguments, so a relation's layout is the
/// same on every run and every build. Panics (at compile time, in a const
/// context) on more than six columns, mismatched lengths, an alignment that is
/// not 1, 2, 4 or 8, or a size that is not a multiple of its alignment.
pub const fn layout(sizes: &[usize], aligns: &[usize], prefixes: u32) -> Layout {
    let n = sizes.len();
    assert!(n <= MAX_COLS, "layout: more than six columns");
    assert!(
        aligns.len() == n,
        "layout: sizes and aligns differ in length"
    );
    let mut c = 0;
    while c < n {
        let a = aligns[c];
        assert!(
            a == 1 || a == 2 || a == 4 || a == 8,
            "layout: alignment must divide 8"
        );
        assert!(
            sizes[c].is_multiple_of(a),
            "layout: size not a multiple of alignment"
        );
        c += 1;
    }

    let mut out = Layout {
        offsets: [0; MAX_COLS],
        seg_ends: [0; MAX_COLS],
        seg_cols: [0; MAX_COLS],
        n_cols: n,
        n_segs: 0,
        words: 0,
    };
    let mut start_col = 0;
    let mut seg_start = 0;
    loop {
        // This segment is columns `start_col..end_col`.
        let mut end_col = start_col + 1;
        while end_col < n && prefixes & (1 << end_col) == 0 {
            end_col += 1;
        }
        if end_col > n {
            end_col = n;
        }
        // Stable selection by decreasing alignment.
        let mut at = seg_start;
        let mut align = 8;
        while align >= 1 {
            let mut c = start_col;
            while c < end_col {
                if aligns[c] == align {
                    out.offsets[c] = at;
                    at += sizes[c];
                }
                c += 1;
            }
            align /= 2;
        }
        let seg_end = at.next_multiple_of(8);
        out.seg_ends[out.n_segs] = seg_end;
        out.seg_cols[out.n_segs] = end_col;
        out.n_segs += 1;
        seg_start = seg_end;
        start_col = end_col;
        if start_col >= n {
            break;
        }
    }
    let words = seg_start / 8;
    out.words = if words == 0 { 1 } else { words };
    out
}

/// The layout that puts each column on words of its own: the columns in
/// declared order, each at the next multiple of eight, with zero padding to the
/// next word. Every column boundary is a segment boundary, so any prefix of
/// the columns is a whole number of words.
const fn slot_layout(sizes: &[usize], aligns: &[usize]) -> Layout {
    let n = sizes.len();
    assert!(n <= MAX_COLS, "layout: more than six columns");
    assert!(
        aligns.len() == n,
        "layout: sizes and aligns differ in length"
    );
    let mut out = Layout {
        offsets: [0; MAX_COLS],
        seg_ends: [0; MAX_COLS],
        seg_cols: [0; MAX_COLS],
        n_cols: n,
        n_segs: n,
        words: 0,
    };
    let mut at = 0;
    let mut c = 0;
    while c < n {
        let a = aligns[c];
        assert!(
            a == 1 || a == 2 || a == 4 || a == 8,
            "layout: alignment must divide 8"
        );
        out.offsets[c] = at;
        at = (at + sizes[c]).next_multiple_of(8);
        out.seg_ends[c] = at;
        out.seg_cols[c] = c + 1;
        c += 1;
    }
    out.words = if at == 0 { 1 } else { at / 8 };
    out
}

/// The layout the provider stores a tuple in: of a relation's tuples under its
/// prefix mask, or of an own index's keys or values with mask 0. Every blob
/// width and every set of column offsets the provider uses comes from here.
pub const fn store_layout(sizes: &[usize], aligns: &[usize], _prefixes: u32) -> Layout {
    slot_layout(sizes, aligns)
}

/// Whether `a` followed by `b` is `whole`.
const fn concat_eq(a: &[usize], b: &[usize], whole: &[usize]) -> bool {
    if a.len() + b.len() != whole.len() {
        return false;
    }
    let mut i = 0;
    while i < whole.len() {
        let x = if i < a.len() { a[i] } else { b[i - a.len()] };
        if x != whole[i] {
            return false;
        }
        i += 1;
    }
    true
}

impl<T: LsmTuple, const PREFIXES: u32, const W: usize> LsmStore<T, PREFIXES, W> {
    const LAYOUT: Layout = store_layout(T::SIZES, T::ALIGNS, PREFIXES);

    /// The number of tuples.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, tuple: &T) -> bool {
        self.set.contains_key(&Self::encode(tuple))
    }

    /// Every tuple, as column references, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = T::Ref<'_>> {
        // SAFETY: the blobs come from the set.
        self.set.keys().map(|blob| unsafe { Self::decode(blob) })
    }

    /// The tuple's blob: its columns at the layout's offsets, the rest zero.
    #[inline]
    fn encode(tuple: &T) -> [u64; W] {
        let mut blob = [0; W];
        tuple.encode_at(bytemuck::bytes_of_mut(&mut blob), Self::LAYOUT.offsets());
        blob
    }

    /// The columns of a blob in the set.
    ///
    /// # Safety
    /// `blob` must come from the set, whose blobs `encode` wrote from live
    /// tuples.
    #[inline]
    unsafe fn decode(blob: &[u64; W]) -> T::Ref<'_> {
        // SAFETY: `encode` wrote each column at its layout offset, which is
        // aligned for the column within the 8-aligned blob, from a live tuple.
        unsafe { T::decode_at(bytemuck::bytes_of(blob), Self::LAYOUT.offsets()) }
    }

    fn insert_if_not_present(&mut self, tuple: &T) -> bool {
        let blob = Self::encode(tuple);
        if self.set.contains_key(&blob) {
            false
        } else {
            self.set.insert(blob, ());
            true
        }
    }
}

impl<T: LsmTuple, const PREFIXES: u32, const W: usize> RelIndexMerge for LsmStore<T, PREFIXES, W> {
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
pub struct FullView<'a, T: LsmTuple, const P: u32, const W: usize>(&'a LsmStore<T, P, W>);
/// A prefix index's read view.
pub struct PrefixView<'a, T: LsmTuple, K, V, const P: u32, const W: usize>(
    &'a LsmStore<T, P, W>,
    PhantomData<(K, V)>,
);
/// The "none" index's read view.
pub struct ScanView<'a, T: LsmTuple, const P: u32, const W: usize>(&'a LsmStore<T, P, W>);

/// The full index's write target: the one way tuples enter the store.
pub struct FullWrite<'a, T: LsmTuple, const P: u32, const W: usize>(&'a mut LsmStore<T, P, W>);
/// The write target of a view index, which has nothing of its own to write.
pub struct NoopWrite<K, V>(PhantomData<(K, V)>);

impl<T: LsmTuple, const P: u32, const W: usize> ToRelIndex<LsmStore<T, P, W>> for ToFull<T> {
    type RelIndex<'a>
        = FullView<'a, T, P, W>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P, W>) -> Self::RelIndex<'a> {
        FullView(store)
    }

    type RelIndexWrite<'a>
        = FullWrite<'a, T, P, W>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        store: &'a mut LsmStore<T, P, W>,
    ) -> Self::RelIndexWrite<'a> {
        FullWrite(store)
    }
}

impl<T: LsmTuple, K: LsmTuple, V: LsmTuple, const P: u32, const W: usize>
    ToRelIndex<LsmStore<T, P, W>> for ToPrefix<T, K, V>
{
    type RelIndex<'a>
        = PrefixView<'a, T, K, V, P, W>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P, W>) -> Self::RelIndex<'a> {
        const {
            assert!(
                concat_eq(K::SIZES, V::SIZES, T::SIZES)
                    && concat_eq(K::ALIGNS, V::ALIGNS, T::ALIGNS),
                "a prefix index's key and value columns must be the tuple's",
            );
        }
        PrefixView(store, PhantomData)
    }

    type RelIndexWrite<'a>
        = NoopWrite<K, V>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        _store: &'a mut LsmStore<T, P, W>,
    ) -> Self::RelIndexWrite<'a> {
        NoopWrite(PhantomData)
    }
}

impl<T: LsmTuple, const P: u32, const W: usize> ToRelIndex<LsmStore<T, P, W>> for ToScan<T> {
    type RelIndex<'a>
        = ScanView<'a, T, P, W>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index<'a>(&'a self, store: &'a LsmStore<T, P, W>) -> Self::RelIndex<'a> {
        ScanView(store)
    }

    type RelIndexWrite<'a>
        = NoopWrite<(), T>
    where
        Self: 'a,
        LsmStore<T, P, W>: 'a;
    #[inline]
    fn to_rel_index_write<'a>(
        &'a mut self,
        _store: &'a mut LsmStore<T, P, W>,
    ) -> Self::RelIndexWrite<'a> {
        NoopWrite(PhantomData)
    }
}

// ---- write targets ----

impl<T: LsmTuple + Clone, const P: u32, const W: usize> RelFullIndexWrite
    for FullWrite<'_, T, P, W>
{
    type Key = T;
    type Value = ();
    #[inline]
    fn insert_if_not_present(&mut self, key: &T, _v: ()) -> bool {
        self.0.insert_if_not_present(key)
    }
}

impl<T: LsmTuple, const P: u32, const W: usize> RelIndexWrite for FullWrite<'_, T, P, W> {
    type Key = T;
    type Value = ();
    #[inline]
    fn index_insert(&mut self, key: T, _value: ()) {
        self.0.insert_if_not_present(&key);
    }
}

impl<T: LsmTuple, const P: u32, const W: usize> RelIndexMerge for FullWrite<'_, T, P, W> {
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

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> RelIndexRead<'a>
    for FullView<'a, T, P, W>
{
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

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> RelFullIndexRead<'a>
    for FullView<'a, T, P, W>
{
    type Key = T;
    #[inline]
    fn contains_key(&'a self, key: &T) -> bool {
        self.0.contains(key)
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> RelIndexReadAll<'a>
    for FullView<'a, T, P, W>
{
    type Key = T::Ref<'a>;
    type Value = ();
    type ValueIteratorType = Once<()>;
    type AllIteratorType = FullAllIter<'a, T, P, W>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        FullAllIter {
            keys: self.0.set.keys(),
            _marker: PhantomData,
        }
    }
}

/// Iterator over every tuple of an [`LsmStore`], as the full index's keys.
pub struct FullAllIter<'a, T, const P: u32, const W: usize> {
    keys: KeysIter<'a, [u64; W], ()>,
    _marker: PhantomData<T>,
}

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> Iterator for FullAllIter<'a, T, P, W> {
    type Item = (T::Ref<'a>, Once<()>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the blobs come from the store's set.
        self.keys
            .next()
            .map(|blob| (unsafe { LsmStore::<T, P, W>::decode(blob) }, once(())))
    }
}

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32, const W: usize>
    RelIndexRead<'a> for PrefixView<'a, T, K, V, P, W>
{
    type Key = K;
    type Value = V::Ref<'a>;
    type IteratorType = SuffixIter<'a, T, V, P, W>;
    #[inline]
    fn index_get(&'a self, key: &K) -> Option<Self::IteratorType> {
        let k = const { K::SIZES.len() };
        let prefix_words =
            const { LsmStore::<T, P, W>::LAYOUT.prefix_bytes(K::SIZES.len()) / size_of::<u64>() };
        let mut blob = [0u64; W];
        let offsets = &LsmStore::<T, P, W>::LAYOUT.offsets()[..k];
        key.encode_at(bytemuck::bytes_of_mut(&mut blob), offsets);
        let prefix = &blob[..prefix_words];
        let mut keys = self.0.set.keys_with_prefix(prefix).peekable();
        keys.peek()?;
        Some(SuffixIter {
            keys,
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

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32, const W: usize>
    RelIndexReadAll<'a> for PrefixView<'a, T, K, V, P, W>
{
    type Key = K::Ref<'a>;
    type Value = V::Ref<'a>;
    type ValueIteratorType = SuffixIter<'a, T, V, P, W>;
    type AllIteratorType = PrefixGroupsIter<'a, T, K, V, P, W>;
    /// The tuples, grouped by prefix. A prefix the memtable and several runs
    /// hold comes as several groups, which suits Ascent: it reads an index whole
    /// to join each group against another relation, and joining a prefix's
    /// tuples in parts gives the same result.
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        let prefix_words =
            const { LsmStore::<T, P, W>::LAYOUT.prefix_bytes(K::SIZES.len()) / size_of::<u64>() };
        PrefixGroupsIter {
            groups: self.0.set.prefix_groups(prefix_words),
            _marker: PhantomData,
        }
    }
}

/// The groups of a [`PrefixView`]'s `iter_all`.
pub struct PrefixGroupsIter<'a, T, K, V, const P: u32, const W: usize> {
    groups: PrefixGroups<'a, [u64; W], ()>,
    _marker: PhantomData<(T, K, V)>,
}

impl<'a, T: LsmTuple + 'a, K: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32, const W: usize>
    Iterator for PrefixGroupsIter<'a, T, K, V, P, W>
{
    type Item = (K::Ref<'a>, SuffixIter<'a, T, V, P, W>);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let (first, keys) = self.groups.next()?;
        let offsets = &LsmStore::<T, P, W>::LAYOUT.offsets()[..const { K::SIZES.len() }];
        // SAFETY: `first` is a blob of the set, which `LsmStore::encode` wrote
        // from a live tuple whose first columns are `K`'s, at these offsets.
        let key = unsafe { K::decode_at(bytemuck::bytes_of(first), offsets) };
        let values = SuffixIter {
            keys: keys.peekable(),
            _marker: PhantomData,
        };
        Some((key, values))
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> RelIndexRead<'a>
    for ScanView<'a, T, P, W>
{
    type Key = ();
    type Value = T::Ref<'a>;
    type IteratorType = ScanIter<'a, T, P, W>;
    #[inline]
    fn index_get(&'a self, _key: &()) -> Option<Self::IteratorType> {
        if self.0.is_empty() {
            return None;
        }
        Some(ScanIter {
            keys: self.0.set.keys(),
            _marker: PhantomData,
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

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> RelIndexReadAll<'a>
    for ScanView<'a, T, P, W>
{
    type Key = ();
    type Value = T::Ref<'a>;
    type ValueIteratorType = ScanIter<'a, T, P, W>;
    type AllIteratorType = std::option::IntoIter<((), ScanIter<'a, T, P, W>)>;
    #[inline]
    fn iter_all(&'a self) -> Self::AllIteratorType {
        self.index_get(&()).map(|vals| ((), vals)).into_iter()
    }
}

/// The value columns of the tuples a prefix read found: each tuple's columns
/// after the prefix, decoded as a `V` in place.
pub struct SuffixIter<'a, T, V, const P: u32, const W: usize> {
    keys: std::iter::Peekable<KeysIter<'a, [u64; W], ()>>,
    _marker: PhantomData<(T, V)>,
}

impl<T, V, const P: u32, const W: usize> Clone for SuffixIter<'_, T, V, P, W> {
    fn clone(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            _marker: PhantomData,
        }
    }
}

impl<'a, T: LsmTuple + 'a, V: LsmTuple + 'a, const P: u32, const W: usize> Iterator
    for SuffixIter<'a, T, V, P, W>
{
    type Item = V::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let tuple = self.keys.next()?;
        let k = const { T::SIZES.len() - V::SIZES.len() };
        let offsets = &LsmStore::<T, P, W>::LAYOUT.offsets()[k..];
        // SAFETY: `tuple` is a blob of the set, which `LsmStore::encode` wrote
        // from a live tuple whose last columns are `V`'s, at these offsets.
        Some(unsafe { V::decode_at(bytemuck::bytes_of(tuple), offsets) })
    }
}

/// Every tuple of a store, decoded.
pub struct ScanIter<'a, T, const P: u32, const W: usize> {
    keys: KeysIter<'a, [u64; W], ()>,
    _marker: PhantomData<T>,
}

impl<T, const P: u32, const W: usize> Clone for ScanIter<'_, T, P, W> {
    fn clone(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            _marker: PhantomData,
        }
    }
}

impl<'a, T: LsmTuple + 'a, const P: u32, const W: usize> Iterator for ScanIter<'a, T, P, W> {
    type Item = T::Ref<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: the blobs come from the store's set.
        self.keys
            .next()
            .map(|blob| unsafe { LsmStore::<T, P, W>::decode(blob) })
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
            {
                $crate::lsm_tuple_words!(
                    $field_types,
                    $crate::ascent_provider::prefix_mask(&[$(&[$($col),*]),*])
                )
            },
        >
    };
}

/// The width in words of a blob holding tuples of type `$t` under the prefix
/// mask `$mask`, as a const expression.
#[doc(hidden)]
#[macro_export]
macro_rules! lsm_tuple_words {
    ($t:ty, $mask:expr) => {
        $crate::ascent_provider::store_layout(
            <$t as $crate::ascent_provider::LsmTuple>::SIZES,
            <$t as $crate::ascent_provider::LsmTuple>::ALIGNS,
            $mask,
        )
        .words
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
        $crate::ascent_provider::LsmIndex<
            $key,
            $val,
            { $crate::lsm_tuple_words!($key, 0) },
            { $crate::lsm_tuple_words!($val, 0) },
        >
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

#[cfg(test)]
mod layout_tests {
    use super::{Layout, layout};

    /// Proves `layout` evaluates at compile time.
    const _: Layout = layout(&[4, 8, 4], &[4, 8, 4], 0);

    /// One case: `(sizes, aligns, mask) -> (offsets, seg_ends, words)`.
    type Case = (
        &'static [usize],
        &'static [usize],
        u32,
        &'static [usize],
        &'static [usize],
        usize,
    );

    const CASES: &[Case] = &[
        // locals_key: FunctionId, FlowVariable, Path, Path, Path, probed by 3 columns.
        (
            &[4, 8, 4, 4, 4],
            &[4, 8, 4, 4, 4],
            1 << 3,
            &[8, 0, 12, 16, 20],
            &[16, 24],
            3,
        ),
        // reach_vp: FunctionId, FlowVariable, Path, no inner boundary.
        (&[4, 8, 4], &[4, 8, 4], 0, &[8, 0, 12], &[16], 2),
        // Bits 0 and n (scan, full) split nothing.
        (&[4, 8, 4], &[4, 8, 4], 1 | 1 << 3, &[8, 0, 12], &[16], 2),
        // actual_param: PackedInsnSiteId, FormalIndex, FlowVertex (8-aligned).
        (&[8, 2, 16], &[8, 2, 8], 0, &[0, 24, 8], &[32], 4),
        // A segment that pads to a word before the next.
        (&[4, 8], &[4, 8], 1 << 1, &[0, 8], &[8, 16], 2),
        // 1, 2, 4, 8 and an 8-aligned 16 in one segment: 31 bytes, 4 words.
        (
            &[1, 2, 4, 8, 16],
            &[1, 2, 4, 8, 8],
            0,
            &[30, 28, 24, 0, 8],
            &[32],
            4,
        ),
        // Several boundaries over five u32s: segments {0}, {1}, {2, 3}, {4}.
        (
            &[4, 4, 4, 4, 4],
            &[4, 4, 4, 4, 4],
            1 << 1 | 1 << 2 | 1 << 4,
            &[0, 8, 16, 20, 24],
            &[8, 16, 24, 32],
            4,
        ),
        // Ties keep declared order.
        (&[4, 4, 4], &[4, 4, 4], 0, &[0, 4, 8], &[16], 2),
        // Arity 0 is still one word.
        (&[], &[], 0, &[], &[0], 1),
        // Six byte columns, every boundary.
        (
            &[1, 1, 1, 1, 1, 1],
            &[1, 1, 1, 1, 1, 1],
            0b11_1110,
            &[0, 8, 16, 24, 32, 40],
            &[8, 16, 24, 32, 40, 48],
            6,
        ),
    ];

    #[test]
    fn layout_table() {
        for &(sizes, aligns, mask, offsets, seg_ends, words) in CASES {
            let l = layout(sizes, aligns, mask);
            let case = format!("sizes {sizes:?} aligns {aligns:?} mask {mask:#b}");
            assert_eq!(l.offsets(), offsets, "offsets, {case}");
            assert_eq!(l.seg_ends(), seg_ends, "seg_ends, {case}");
            assert_eq!(l.words, words, "words, {case}");
            assert_eq!(l.n_cols(), sizes.len(), "n_cols, {case}");
            // Every column is aligned, and no two columns overlap.
            for c in 0..sizes.len() {
                assert_eq!(offsets[c] % aligns[c], 0, "column {c} misaligned, {case}");
                for d in 0..c {
                    let disjoint =
                        offsets[c] + sizes[c] <= offsets[d] || offsets[d] + sizes[d] <= offsets[c];
                    assert!(disjoint, "columns {d} and {c} overlap, {case}");
                }
            }
        }
    }

    #[test]
    fn layout_prefix_bytes() {
        let l = layout(&[4, 8, 4, 4, 4], &[4, 8, 4, 4, 4], 1 << 3);
        assert_eq!(l.prefix_bytes(0), 0);
        assert_eq!(l.prefix_bytes(3), 16);
        assert_eq!(l.prefix_bytes(5), 24);

        let l = layout(&[4; 5], &[4; 5], 1 << 1 | 1 << 2 | 1 << 4);
        assert_eq!([1, 2, 4, 5].map(|k| l.prefix_bytes(k)), [8, 16, 24, 32]);

        const K3: usize = layout(&[4, 8, 4, 4, 4], &[4, 8, 4, 4, 4], 1 << 3).prefix_bytes(3);
        assert_eq!(K3, 16);
    }

    #[test]
    #[should_panic(expected = "not a segment boundary")]
    fn layout_prefix_bytes_inside_a_segment() {
        layout(&[4, 8, 4], &[4, 8, 4], 0).prefix_bytes(1);
    }

    #[test]
    #[should_panic(expected = "more than six columns")]
    fn layout_rejects_seven_columns() {
        layout(&[1; 7], &[1; 7], 0);
    }

    #[test]
    #[should_panic(expected = "alignment must divide 8")]
    fn layout_rejects_align_16() {
        layout(&[16], &[16], 0);
    }
}

#[cfg(test)]
mod store_tests {
    use std::fmt::Debug;

    use super::*;

    /// A 16-byte column.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
    struct Wide(u64, u64);
    crate::lsm_col!(Wide, [u64, u64]);

    /// A column shaped like CTADL's `FlowVertex`: a word and two 4-byte fields.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
    struct Vtx(u64, u32, u32);
    crate::lsm_col!(Vtx, [u64, u32, u32]);

    /// A column type with distinct sample values.
    trait Sample: LsmCol + PartialEq + Debug {
        fn sample(i: u64) -> Self;
    }
    impl Sample for u8 {
        fn sample(i: u64) -> Self {
            (i * 37 + 1) as u8
        }
    }
    impl Sample for u16 {
        fn sample(i: u64) -> Self {
            (i * 4099 + 2) as u16
        }
    }
    impl Sample for u32 {
        fn sample(i: u64) -> Self {
            (i * 65_537 + 3) as u32
        }
    }
    impl Sample for u64 {
        fn sample(i: u64) -> Self {
            i.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1
        }
    }
    impl Sample for Wide {
        fn sample(i: u64) -> Self {
            Wide(i + 5, !i)
        }
    }
    impl Sample for Vtx {
        fn sample(i: u64) -> Self {
            Vtx(i + 7, (i * 3) as u32, (i * 11 + 1) as u32)
        }
    }
    impl<A: Sample, B: Sample> Sample for (A, B) {
        fn sample(i: u64) -> Self {
            (A::sample(i), B::sample(i + 1))
        }
    }

    /// A tuple the tests can build, copy out of a blob, and split.
    trait TestTuple: LsmTuple + Copy + PartialEq + Debug + 'static {
        /// A tuple whose column `c` is `sample(i + c)`.
        fn sample(i: u64) -> Self;
        /// This tuple with column `c` replaced by `sample(i)`.
        fn vary(self, c: usize, i: u64) -> Self;
        /// The tuple the references point to.
        fn owned(r: Self::Ref<'_>) -> Self;
        /// Checks every split of `blob`, this tuple's blob under `layout`, into
        /// its first `k` and remaining columns.
        fn check_splits(self, blob: &[u8], layout: &Layout, mask: u32);
    }

    impl TestTuple for () {
        fn sample(_i: u64) -> Self {}
        fn vary(self, _c: usize, _i: u64) -> Self {}
        fn owned(_r: ()) -> Self {}
        fn check_splits(self, _blob: &[u8], _layout: &Layout, _mask: u32) {}
    }

    /// Checks that `blob[..]` splits into `key` and `val` at `K`'s arity: they
    /// decode in place, and at a prefix boundary the key's columns lie before
    /// the prefix's end and the value's after it.
    fn check_split<K: TestTuple, V: TestTuple>(
        key: K,
        val: V,
        blob: &[u8],
        layout: &Layout,
        mask: u32,
    ) {
        let k = K::SIZES.len();
        let (koffs, voffs) = layout.offsets().split_at(k);
        // SAFETY: `blob` was encoded from a live tuple with `layout`.
        let (kr, vr) = unsafe { (K::decode_at(blob, koffs), V::decode_at(blob, voffs)) };
        assert_eq!(K::owned(kr), key, "key at split {k}, mask {mask:#b}");
        assert_eq!(V::owned(vr), val, "value at split {k}, mask {mask:#b}");
        if k == layout.n_cols() || mask & (1 << k) != 0 {
            let end = layout.prefix_bytes(k);
            assert_eq!(end % 8, 0);
            for (c, &off) in koffs.iter().enumerate() {
                assert!(off + K::SIZES[c] <= end, "key column {c} past {end}");
            }
            for &off in voffs {
                assert!(off >= end, "value column at {off} before {end}");
            }
        }
    }

    /// Calls [`check_split`] for each split of the columns after `[$h ...]`.
    macro_rules! splits {
        ($t:ident, $blob:ident, $l:ident, $mask:ident, [$($h:tt $H:ident)*], []) => {};
        (
            $t:ident, $blob:ident, $l:ident, $mask:ident,
            [$($h:tt $H:ident)*], [$n:tt $N:ident $($r:tt $R:ident)*]
        ) => {
            check_split::<($($H,)* $N,), ($($R,)*)>(
                ($($t.$h,)* $t.$n,),
                ($($t.$r,)*),
                $blob,
                $l,
                $mask,
            );
            splits!($t, $blob, $l, $mask, [$($h $H)* $n $N], [$($r $R)*]);
        };
    }

    macro_rules! test_tuple {
        ($($i:tt $c:ident),+) => {
            impl<$($c: Sample),+> TestTuple for ($($c,)+) {
                fn sample(i: u64) -> Self {
                    ($($c::sample(i + $i),)+)
                }
                fn vary(mut self, c: usize, i: u64) -> Self {
                    match c {
                        $($i => self.$i = $c::sample(i),)+
                        _ => unreachable!(),
                    }
                    self
                }
                fn owned(r: Self::Ref<'_>) -> Self {
                    ($(*r.$i,)+)
                }
                fn check_splits(self, blob: &[u8], layout: &Layout, mask: u32) {
                    let t = self;
                    splits!(t, blob, layout, mask, [], [$($i $c)+]);
                }
            }
        };
    }

    test_tuple!(0 A);
    test_tuple!(0 A, 1 B);
    test_tuple!(0 A, 1 B, 2 C);
    test_tuple!(0 A, 1 B, 2 C, 3 D);
    test_tuple!(0 A, 1 B, 2 C, 3 D, 4 E);
    test_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);

    /// `t`'s blob under `layout`, written over words of `fill`.
    fn encode_over<T: LsmTuple>(t: &T, layout: &Layout, fill: u64) -> Vec<u64> {
        let mut blob = vec![fill; layout.words];
        t.encode_at(bytemuck::cast_slice_mut(&mut blob), layout.offsets());
        blob
    }

    /// For every mask over `T`'s inner boundaries: decoding gives back the
    /// tuple, whole and split at each boundary; pad bytes are zero; a column's
    /// bytes do not depend on what the blob held; and blobs are equal exactly
    /// when tuples are.
    fn round_trip<T: TestTuple>() {
        let n = T::SIZES.len();
        for inner in 0..1u32 << (n - 1) {
            let mask = inner << 1;
            let layout = store_layout(T::SIZES, T::ALIGNS, mask);
            let mut covered = vec![false; layout.words * 8];
            for (c, &off) in layout.offsets().iter().enumerate() {
                covered[off..off + T::SIZES[c]].fill(true);
            }
            for i in 0..16 {
                let t = T::sample(i);
                let zeros = encode_over(&t, &layout, 0);
                let ones = encode_over(&t, &layout, u64::MAX);
                let bytes: &[u8] = bytemuck::cast_slice(&zeros);
                // SAFETY: `bytes` was encoded from a live tuple with `layout`.
                let back = T::owned(unsafe { T::decode_at(bytes, layout.offsets()) });
                assert_eq!(back, t, "mask {mask:#b}");
                let ones: &[u8] = bytemuck::cast_slice(&ones);
                for (b, &is_col) in covered.iter().enumerate() {
                    if is_col {
                        assert_eq!(bytes[b], ones[b], "byte {b} of {t:?}, mask {mask:#b}");
                    } else {
                        assert_eq!(bytes[b], 0, "pad byte {b} of {t:?}, mask {mask:#b}");
                    }
                }
                t.check_splits(bytes, &layout, mask);
                assert_eq!(encode_over(&T::sample(i), &layout, 0), zeros);
                for c in 0..n {
                    let u = t.vary(c, i + 1000);
                    assert_ne!(u, t);
                    assert_ne!(encode_over(&u, &layout, 0), zeros, "{u:?} vs {t:?}");
                }
            }
        }
    }

    type P2 = (u32, u16);

    #[test]
    fn round_trips() {
        round_trip::<(u8,)>();
        round_trip::<(P2,)>();
        round_trip::<(u16, Wide)>();
        round_trip::<(u32, P2, u8)>();
        round_trip::<(u64, u8, Wide, u16)>();
        round_trip::<(Wide, u32, u16, P2, u64)>();
        round_trip::<(u8, u16, u32, u64, P2, Wide)>();
        round_trip::<(u8, Vtx, u8, u16, u8, u32)>();
    }

    #[test]
    fn arity_zero_is_one_zero_word() {
        let layout = store_layout(<()>::SIZES, <()>::ALIGNS, 0);
        assert_eq!(layout.words, 1);
        assert_eq!(encode_over(&(), &layout, 0), [0]);
    }

    /// A tuple's blob with each column on words of its own: each column's
    /// bytes, zero-padded to a multiple of eight, in column order.
    fn slot_bytes<T: TestTuple>(t: &T) -> Vec<u8> {
        let n = T::SIZES.len();
        let mut out = Vec::new();
        for (c, &size) in T::SIZES.iter().enumerate() {
            // Column `c` at 0, alone; the others out of the way at 64.
            let mut scratch = vec![0u64; 16];
            let offsets: Vec<usize> = (0..n).map(|d| if d == c { 0 } else { 64 }).collect();
            t.encode_at(bytemuck::cast_slice_mut(&mut scratch), &offsets);
            let bytes: &[u8] = bytemuck::cast_slice(&scratch);
            out.extend_from_slice(&bytes[..size.next_multiple_of(8)]);
        }
        out
    }

    #[test]
    fn same_bytes_as_slots() {
        type T6 = (u8, u16, u32, u64, Wide, Vtx);
        const P: u32 = 1 << 2 | 1 << 4;
        const W: usize = crate::lsm_tuple_words!(T6, P);
        assert_eq!(W, 8);
        type T3 = (u16, Vtx, u32);
        const W3: usize = crate::lsm_tuple_words!(T3, 0);
        assert_eq!(W3, 4);
        for i in 0..8 {
            let t = T6::sample(i);
            let blob = LsmStore::<T6, P, W>::encode(&t);
            assert_eq!(bytemuck::bytes_of(&blob), slot_bytes(&t));
            let t = T3::sample(i);
            let blob = LsmIndex::<T3, (), W3, 1>::encode_key(&t);
            assert_eq!(bytemuck::bytes_of(&blob), slot_bytes(&t));
        }
    }

    type C4 = (u32, u64, u16, u32);

    /// Reads `store` by its first `K::SIZES.len()` columns, by key and grouped,
    /// and checks that each read gives exactly the tuples with that prefix.
    fn check_prefix<K, V, const P: u32, const W: usize>(
        store: &LsmStore<C4, P, W>,
        tuples: &[C4],
        split: fn(C4) -> (K, V),
        join: fn(K, V) -> C4,
    ) where
        K: TestTuple + Ord,
        V: TestTuple,
    {
        let marker = ToPrefix::<C4, K, V>::default();
        let view = marker.to_rel_index(store);
        let mut keys: Vec<K> = tuples.iter().map(|&t| split(t).0).collect();
        keys.sort();
        keys.dedup();
        for &key in &keys {
            let mut got: Vec<C4> = view
                .index_get(&key)
                .expect("a present prefix")
                .map(|v| join(key, V::owned(v)))
                .collect();
            got.sort();
            let mut want: Vec<C4> = tuples
                .iter()
                .copied()
                .filter(|&t| split(t).0 == key)
                .collect();
            want.sort();
            assert_eq!(got, want, "prefix {key:?}");
        }
        let absent = split((u32::MAX, u64::MAX, u16::MAX, u32::MAX)).0;
        assert!(view.index_get(&absent).is_none());

        let mut all: Vec<C4> = Vec::new();
        for (kr, vals) in view.iter_all() {
            let key = K::owned(kr);
            all.extend(vals.map(|v| join(key, V::owned(v))));
        }
        all.sort();
        let mut want = tuples.to_vec();
        want.sort();
        assert_eq!(all, want, "iter_all by {} columns", K::SIZES.len());
    }

    /// A store of shuffled tuples with a tiny memtable, so that they spread
    /// over the memtable and many runs, after checking that the full index
    /// reads them back.
    fn filled_store<const P: u32, const W: usize>() -> (LsmStore<C4, P, W>, Vec<C4>) {
        let mut tuples: Vec<C4> = (0..500u32)
            .map(|i| (i % 5, (i % 7) as u64 * 1_000_000_007, (i % 3) as u16, i))
            .collect();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for i in (1..tuples.len()).rev() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            tuples.swap(i, (state >> 33) as usize % (i + 1));
        }
        set_memtable_limit(4);
        let mut store = LsmStore::<C4, P, W>::default();
        let mut full = ToFull::<C4>::default();
        {
            let mut write = full.to_rel_index_write(&mut store);
            for t in &tuples {
                assert!(write.insert_if_not_present(t, ()));
            }
            for t in &tuples[..50] {
                assert!(!write.insert_if_not_present(t, ()));
            }
        }
        assert_eq!(store.len(), tuples.len());
        let view = full.to_rel_index(&store);
        let mut all: Vec<C4> = view.iter_all().map(|(r, _)| C4::owned(r)).collect();
        all.sort();
        let mut want = tuples.clone();
        want.sort();
        assert_eq!(all, want);
        (store, tuples)
    }

    fn check_prefix_1<const P: u32, const W: usize>(store: &LsmStore<C4, P, W>, tuples: &[C4]) {
        check_prefix::<(u32,), (u64, u16, u32), P, W>(
            store,
            tuples,
            |(a, b, c, d)| ((a,), (b, c, d)),
            |(a,), (b, c, d)| (a, b, c, d),
        );
    }

    fn check_prefix_2<const P: u32, const W: usize>(store: &LsmStore<C4, P, W>, tuples: &[C4]) {
        check_prefix::<(u32, u64), (u16, u32), P, W>(
            store,
            tuples,
            |(a, b, c, d)| ((a, b), (c, d)),
            |(a, b), (c, d)| (a, b, c, d),
        );
    }

    fn check_prefix_3<const P: u32, const W: usize>(store: &LsmStore<C4, P, W>, tuples: &[C4]) {
        check_prefix::<(u32, u64, u16), (u32,), P, W>(
            store,
            tuples,
            |(a, b, c, d)| ((a, b, c), (d,)),
            |(a, b, c), (d,)| (a, b, c, d),
        );
    }

    #[test]
    fn one_prefix_is_contiguous() {
        const P: u32 = 1 << 2;
        let (store, tuples) = filled_store::<P, { crate::lsm_tuple_words!(C4, P) }>();
        check_prefix_2(&store, &tuples);
    }

    #[test]
    fn several_prefixes_are_contiguous() {
        const P: u32 = 1 << 1 | 1 << 2 | 1 << 3;
        let (store, tuples) = filled_store::<P, { crate::lsm_tuple_words!(C4, P) }>();
        check_prefix_1(&store, &tuples);
        check_prefix_2(&store, &tuples);
        check_prefix_3(&store, &tuples);
    }
}
