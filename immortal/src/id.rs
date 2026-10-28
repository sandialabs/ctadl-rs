//! An interner that hands out 4-byte ids instead of pointers.
//!
//! [`IdInterner`] maps each distinct value to an [`Id`], a `NonZeroU32`, and back. Values
//! are never freed, so [`IdInterner::get`] returns `&'static T` and takes no lock.
//!
//! **Layout.** `S` shards, each a padded `RwLock` over a `HashTable<u64>` whose entries
//! pack `id | hash32 << 32`. The table rehashes from the stored half of the hash, so
//! resizes and tag misses never read a value. A shard's values live in its own
//! append-only arena of buckets of `64 << b` slots, allocated under the shard's write
//! lock. The bucket pointers sit in an array apart from the locks, so a `get` never
//! shares a cache line with a lock word that interning writes.
//!
//! **Ids.** `((local + 1) << log2 S) | shard`, where `local` is the value's index in its
//! shard. Each shard has room for just under `2^(32 - log2 S)` values; running out
//! panics with the handle type's name.
//!
//! **Unsized values.** `str` and `[T]` slots hold a 4-byte offset and a length into a
//! per-shard bump arena, so a value costs no malloc block of its own.
//!
//! **Soundness.** A slot is a plain write made under the shard's write lock, and `get`
//! reads it without one. That is sound because an `Id` cannot be forged: every `Id<B>`
//! was returned by `intern`, `try_get` or `from_raw` on the one interner branded `B`, and
//! each of those happens after the slot's write (through the shard lock, or through the
//! shard length's Release/Acquire pair). [`IdInterner::new`] is `unsafe` because it is
//! where that one-interner-per-brand promise is made; [`id_handle!`] makes it for you.
//!
//! [`id_handle!`]: crate::id_handle

use hashbrown::HashTable;
use parking_lot::{RwLock, const_rwlock};
use rustc_hash::FxBuildHasher;
use std::alloc::{Layout, alloc, handle_alloc_error};
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

/// Slots in a shard's first bucket; bucket `b` holds `FIRST_BUCKET << b`.
const FIRST_BUCKET: u64 = 64;
/// Enough buckets for `2^32` slots, the most a shard can hold (with `S = 1`).
const BUCKETS: usize = 27;
/// Units in a shard's first bump-arena chunk; chunk `c` holds `FIRST_CHUNK << c`.
const FIRST_CHUNK: u64 = 1024;
/// Enough chunks to cover every `u32` offset.
const CHUNKS: usize = 23;

/// A handle to a value interned in the one [`IdInterner`] branded `B`.
///
/// `Option<Id<B>>` is 4 bytes. `Eq`, `Hash` and `Ord` act on the id alone.
#[repr(transparent)]
pub struct Id<B>(NonZeroU32, PhantomData<fn() -> B>);

impl<B> Id<B> {
    #[inline]
    fn new(raw: u32) -> Self {
        Id(NonZeroU32::new(raw).expect("ids are never 0"), PhantomData)
    }

    /// The id's bits, for storage that [`IdInterner::from_raw`] reads back.
    #[inline]
    pub fn raw(self) -> u32 {
        self.0.get()
    }

    /// The id whose bits are `raw`, unchecked: for bits this process took from an `Id<B>`
    /// itself and never let leave it (R13), where [`IdInterner::from_raw`]'s check would
    /// cost an atomic load on every read.
    ///
    /// # Safety
    ///
    /// `raw` is [`raw`](Self::raw) of an `Id<B>` the caller holds or held, so the caller
    /// happens after that id was issued.
    #[inline]
    pub unsafe fn from_raw_unchecked(raw: u32) -> Self {
        // SAFETY: an issued id is never 0 (the caller's contract).
        Id(unsafe { NonZeroU32::new_unchecked(raw) }, PhantomData)
    }
}

impl<B> Clone for Id<B> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<B> Copy for Id<B> {}

impl<B> PartialEq for Id<B> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<B> Eq for Id<B> {}

impl<B> PartialOrd for Id<B> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<B> Ord for Id<B> {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl<B> Hash for Id<B> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<B> fmt::Debug for Id<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

/// An interner's size, for the profile (C2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Stats {
    pub entries: usize,
    /// Heap bytes: hash tables, slot buckets and bump-arena chunks.
    pub bytes: usize,
}

mod sealed {
    pub trait Sealed {}
}

/// A type an [`IdInterner`] can hold: any `Clone` sized type, `str`, or `[T]`.
pub trait Internable: sealed::Sealed + Hash + Eq + Send + Sync + 'static {
    /// What a bucket slot holds: the value itself, or a [`Span`] into the bump arena.
    #[doc(hidden)]
    type Slot: Send + Sync + 'static;
    /// The bump arena's element type; unused for sized values.
    #[doc(hidden)]
    type Unit: Send + Sync + 'static;

    /// Makes the slot for `self`, copying any unsized contents into `arena`. `None`
    /// means the arena is out of offsets.
    #[doc(hidden)]
    fn store(&self, arena: &mut Bump<'_, Self::Unit>) -> Option<Self::Slot>;

    /// # Safety
    ///
    /// `slot` was made by `store` with this `chunks` array, and the caller happens after
    /// that call and its chunk-pointer stores.
    #[doc(hidden)]
    unsafe fn load(
        slot: &'static Self::Slot,
        chunks: &[AtomicPtr<Self::Unit>; CHUNKS],
    ) -> &'static Self;
}

impl<T: Hash + Eq + Clone + Send + Sync + 'static> sealed::Sealed for T {}

impl<T: Hash + Eq + Clone + Send + Sync + 'static> Internable for T {
    type Slot = T;
    type Unit = ();

    #[inline]
    fn store(&self, _: &mut Bump<'_, ()>) -> Option<T> {
        Some(self.clone())
    }

    #[inline]
    unsafe fn load(slot: &'static T, _: &[AtomicPtr<()>; CHUNKS]) -> &'static T {
        slot
    }
}

/// An unsized value's place in its shard's bump arena.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Span {
    off: u32,
    len: u32,
}

impl sealed::Sealed for str {}

impl Internable for str {
    type Slot = Span;
    type Unit = u8;

    #[inline]
    fn store(&self, arena: &mut Bump<'_, u8>) -> Option<Span> {
        arena.store_slice(self.as_bytes())
    }

    #[inline]
    unsafe fn load(slot: &'static Span, chunks: &[AtomicPtr<u8>; CHUNKS]) -> &'static str {
        // SAFETY: the caller's contract, and `store` copied a `str`'s bytes, which are UTF-8.
        unsafe { std::str::from_utf8_unchecked(load_slice(slot, chunks)) }
    }
}

impl<T: Hash + Eq + Clone + Send + Sync + 'static> sealed::Sealed for [T] {}

impl<T: Hash + Eq + Clone + Send + Sync + 'static> Internable for [T] {
    type Slot = Span;
    type Unit = T;

    #[inline]
    fn store(&self, arena: &mut Bump<'_, T>) -> Option<Span> {
        arena.store_slice(self)
    }

    #[inline]
    unsafe fn load(slot: &'static Span, chunks: &[AtomicPtr<T>; CHUNKS]) -> &'static [T] {
        // SAFETY: the caller's contract.
        unsafe { load_slice(slot, chunks) }
    }
}

/// Bucket `b` and index `i` within it of the `n`th slot (or arena unit), where bucket `b`
/// holds `first << b` and `first` is a power of two.
#[inline]
fn split(n: u64, first: u64) -> (usize, usize) {
    let x = n + first;
    let b = x.ilog2() - first.ilog2();
    (b as usize, (x - (first << b)) as usize)
}

/// Allocates room for `n` values of `U`, uninitialized and never freed.
fn alloc_uninit<U>(n: usize) -> *mut U {
    let layout = Layout::array::<U>(n).expect("interner bucket overflows isize");
    if layout.size() == 0 {
        return NonNull::dangling().as_ptr();
    }
    // SAFETY: `layout` has nonzero size.
    let p = unsafe { alloc(layout) };
    if p.is_null() {
        handle_alloc_error(layout);
    }
    p.cast()
}

/// A shard's bump arena while its write lock is held: the chunk pointers, which readers
/// load, and the next free offset.
#[doc(hidden)]
pub struct Bump<'a, U> {
    chunks: &'a [AtomicPtr<U>; CHUNKS],
    next: &'a mut u64,
    bytes: &'a mut usize,
}

impl<U: Clone> Bump<'_, U> {
    /// Copies `v` into the arena. A value never straddles chunks: if it doesn't fit in
    /// the rest of the current chunk, it starts the next one that is large enough.
    /// Skipped chunks are never allocated, so only the current chunk's tail is lost.
    fn store_slice(&mut self, v: &[U]) -> Option<Span> {
        let len = u32::try_from(v.len()).ok()?;
        if len == 0 {
            return Some(Span { off: 0, len: 0 });
        }
        let n = len as u64;
        loop {
            let (c, i) = split(*self.next, FIRST_CHUNK);
            if c >= CHUNKS {
                return None;
            }
            let size = FIRST_CHUNK << c;
            if i as u64 + n > size {
                *self.next += size - i as u64;
                continue;
            }
            let off = u32::try_from(*self.next).ok()?;
            let mut chunk = self.chunks[c].load(Ordering::Relaxed);
            if chunk.is_null() {
                chunk = alloc_uninit::<U>(size as usize);
                *self.bytes += size as usize * size_of::<U>();
                // Relaxed: a reader gets an id for this value only after the shard length's
                // Release store or the write lock's release, both sequenced after this.
                self.chunks[c].store(chunk, Ordering::Relaxed);
            }
            for (k, x) in v.iter().enumerate() {
                // SAFETY: `i + n <= size`, so `i + k` is inside the chunk's `size` units.
                // Offsets only grow, so these units were never written before, and the
                // write lock excludes every other writer.
                unsafe { chunk.add(i + k).write(x.clone()) };
            }
            *self.next += n;
            return Some(Span { off, len });
        }
    }
}

/// # Safety
///
/// `slot` was made by `store_slice` with `chunks`, and the caller happens after that call.
#[inline]
unsafe fn load_slice<U>(slot: &Span, chunks: &[AtomicPtr<U>; CHUNKS]) -> &'static [U] {
    if slot.len == 0 {
        return &[];
    }
    let (c, i) = split(slot.off as u64, FIRST_CHUNK);
    // Relaxed suffices; see `store_slice`.
    let chunk = chunks[c].load(Ordering::Relaxed);
    // SAFETY: `store_slice` wrote `len` units at `i` in chunk `c` before the caller could
    // see this slot, and chunks are never freed or written again.
    unsafe { std::slice::from_raw_parts(chunk.add(i), slot.len as usize) }
}

/// A shard's state that only its write lock may change.
struct Locked {
    table: HashTable<u64>,
    len: u32,
    /// The bump arena's next free offset.
    next: u64,
    slot_bytes: usize,
    arena_bytes: usize,
}

/// The locked half of a shard, padded to Apple's 128-byte line so that shards' lock words
/// don't share lines.
#[repr(align(128))]
struct Shard {
    lock: RwLock<Locked>,
    /// `Locked::len`, published with Release after the newest slot is written.
    len: AtomicU32,
}

/// The read half of a shard: bucket and chunk pointers, written once each.
struct Ptrs<Slot, U> {
    slots: [AtomicPtr<Slot>; BUCKETS],
    chunks: [AtomicPtr<U>; CHUNKS],
}

/// A sharded, append-only interner from `T` to [`Id<B>`](Id).
///
/// `B` is the brand: there is exactly one `IdInterner` per `B` (see [`new`](Self::new)),
/// so an `Id<B>` always indexes this interner. `S`, the number of shards, is a power of
/// two. It is meant to live in a `static`, which [`id_handle!`](crate::id_handle) declares.
pub struct IdInterner<T: ?Sized + Internable, B: 'static, const S: usize = 256> {
    shards: [Shard; S],
    ptrs: [Ptrs<T::Slot, T::Unit>; S],
    /// The most values a shard may hold; below `CAP` only in tests.
    limit: u32,
    _marker: PhantomData<(fn() -> B, &'static T)>,
}

/// Picks a value's shard and its 32 stored hash bits from one 64-bit hash.
#[inline]
fn hash64<T: ?Sized + Hash>(value: &T) -> u64 {
    // FxHash's low output bits come from the low bits of a product, which mix poorly, and
    // the table's tag and the shard are both taken from them. Fold a full 64x64->128
    // multiply over it, so every bit depends on every input bit.
    let h = FxBuildHasher.hash_one(value);
    let m = (h as u128).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (m as u64) ^ ((m >> 64) as u64)
}

/// The 64-bit hash the table indexes and tags with, rebuilt from the stored 32 bits:
/// hashbrown takes its bucket index from the low bits and its tag from the top 7.
#[inline]
fn table_hash(h32: u32) -> u64 {
    h32 as u64 | (h32 as u64) << 32
}

impl<T: ?Sized + Internable, B: 'static, const S: usize> IdInterner<T, B, S> {
    const LOG2_S: u32 = S.trailing_zeros();
    /// The most values a shard can hold: `(local + 1) << log2 S` must fit in 32 bits.
    const CAP: u32 = ((1u64 << (32 - Self::LOG2_S)) - 1) as u32;

    /// An empty interner, for a `static`.
    ///
    /// # Safety
    ///
    /// No other `IdInterner` branded `B` may ever be made in this process, for any `T`
    /// or `S`, even after this one is dropped. `get` trusts that every `Id<B>` came from
    /// this interner. Dropping it leaks its values, so references `get` returned stay valid.
    pub const unsafe fn new() -> Self {
        // SAFETY: the caller's contract.
        unsafe { Self::with_limit(Self::CAP) }
    }

    /// An empty interner whose ids all fit in the low `bits` bits, so a handle can keep the
    /// bits above them for a tag of its own. Each shard holds just under `2^(bits - log2 S)`
    /// values.
    ///
    /// # Safety
    ///
    /// As [`new`](Self::new).
    pub const unsafe fn with_id_bits(bits: u32) -> Self {
        assert!(bits > Self::LOG2_S && bits <= 32, "id bits");
        // SAFETY: the caller's contract.
        unsafe { Self::with_limit(((1u64 << (bits - Self::LOG2_S)) - 1) as u32) }
    }

    /// # Safety
    ///
    /// As [`new`](Self::new).
    const unsafe fn with_limit(limit: u32) -> Self {
        const { assert!(S.is_power_of_two() && S <= 1 << 16, "shard count") };
        IdInterner {
            shards: [const {
                Shard {
                    lock: const_rwlock(Locked {
                        table: HashTable::new(),
                        len: 0,
                        next: 0,
                        slot_bytes: 0,
                        arena_bytes: 0,
                    }),
                    len: AtomicU32::new(0),
                }
            }; S],
            ptrs: [const {
                Ptrs {
                    slots: [const { AtomicPtr::new(ptr::null_mut()) }; BUCKETS],
                    chunks: [const { AtomicPtr::new(ptr::null_mut()) }; CHUNKS],
                }
            }; S],
            limit,
            _marker: PhantomData,
        }
    }

    /// The shard and local index an id names.
    #[inline]
    fn decode(raw: u32) -> (usize, u32) {
        (
            raw as usize & (S - 1),
            (raw >> Self::LOG2_S).wrapping_sub(1),
        )
    }

    /// # Safety
    ///
    /// `raw` was issued by this interner, and the caller happens after its slot was
    /// written: it holds the shard lock that the write was made under, or it read the
    /// shard's length with Acquire and found the id below it.
    #[inline]
    unsafe fn value(&self, raw: u32) -> &'static T {
        let (shard, local) = Self::decode(raw);
        let (b, i) = split(local as u64, FIRST_BUCKET);
        let ptrs = &self.ptrs[shard];
        // Relaxed: the caller's happens-before covers this pointer's store too.
        let bucket = ptrs.slots[b].load(Ordering::Relaxed);
        // SAFETY: the caller's contract. `intern` wrote slot `i` of bucket `b`, which is
        // never freed or written again, and `T::load` gets the chunks it stored into.
        unsafe { T::load(&*bucket.add(i), &ptrs.chunks) }
    }

    /// The value `id` names, without a lock.
    #[inline]
    pub fn get(&self, id: Id<B>) -> &'static T {
        // SAFETY: an `Id<B>` comes only from `intern`, `try_get` or `from_raw` on the one
        // interner branded `B`, which is `self` (`new`'s contract). Each happens after the
        // slot's write, and so does anything that received the id from them: safe code can
        // pass an id to another thread only through something that synchronizes.
        unsafe { self.value(id.raw()) }
    }

    #[inline]
    fn find(&self, table: &HashTable<u64>, h32: u32, value: &T) -> Option<Id<B>> {
        table
            .find(table_hash(h32), |&e| {
                // SAFETY: an entry is inserted after its slot is written, under the write
                // lock; the caller holds that lock, read or write.
                (e >> 32) as u32 == h32 && unsafe { self.value(e as u32) } == value
            })
            .map(|&e| Id::new(e as u32))
    }

    /// The id of `value`, interning it if it is new.
    pub fn intern(&self, value: &T) -> Id<B> {
        let hash = hash64(value);
        let s = (hash >> 32) as usize & (S - 1);
        let h32 = hash as u32;
        let shard = &self.shards[s];
        if let Some(id) = self.find(&shard.lock.read().table, h32, value) {
            return id;
        }
        let mut guard = shard.lock.write();
        let locked = &mut *guard;
        if let Some(id) = self.find(&locked.table, h32, value) {
            return id;
        }
        let local = locked.len;
        if local >= self.limit {
            panic!(
                "IdInterner<{}>: shard {s} is full at {local} values",
                std::any::type_name::<B>()
            );
        }
        let (b, i) = split(local as u64, FIRST_BUCKET);
        let ptrs = &self.ptrs[s];
        let mut bucket = ptrs.slots[b].load(Ordering::Relaxed);
        if bucket.is_null() {
            let n = (FIRST_BUCKET << b) as usize;
            bucket = alloc_uninit::<T::Slot>(n);
            locked.slot_bytes += n * size_of::<T::Slot>();
            // Relaxed: see `Bump::store_slice`.
            ptrs.slots[b].store(bucket, Ordering::Relaxed);
        }
        let slot = value
            .store(&mut Bump {
                chunks: &ptrs.chunks,
                next: &mut locked.next,
                bytes: &mut locked.arena_bytes,
            })
            .unwrap_or_else(|| {
                panic!(
                    "IdInterner<{}>: shard {s}'s arena is out of offsets",
                    std::any::type_name::<B>()
                )
            });
        // SAFETY: `i < FIRST_BUCKET << b`, the bucket's length. Slot `local` was never
        // written, and the write lock excludes other writers; readers reach it only by an
        // id, which nobody has until below.
        unsafe { bucket.add(i).write(slot) };
        let raw = ((local + 1) << Self::LOG2_S) | s as u32;
        locked
            .table
            .insert_unique(table_hash(h32), raw as u64 | (h32 as u64) << 32, |&e| {
                table_hash((e >> 32) as u32)
            });
        locked.len = local + 1;
        shard.len.store(local + 1, Ordering::Release);
        Id::new(raw)
    }

    /// The id of `value` if it was interned. Never inserts.
    pub fn try_get(&self, value: &T) -> Option<Id<B>> {
        let hash = hash64(value);
        let shard = &self.shards[(hash >> 32) as usize & (S - 1)];
        self.find(&shard.lock.read().table, hash as u32, value)
    }

    /// The id whose bits are `raw`, if this interner issued it. Takes no lock.
    pub fn from_raw(&self, raw: u32) -> Option<Id<B>> {
        let (shard, local) = Self::decode(raw);
        // `local` wraps to `u32::MAX` when `raw` has no local bits (including `raw == 0`),
        // and no shard ever reaches that length. Acquire pairs with `intern`'s Release, so
        // the caller happens after the slot's write.
        (local < self.shards[shard].len.load(Ordering::Acquire)).then(|| Id::new(raw))
    }

    /// Whether this interner issued `id`. Always true for an `Id` that came from it; for
    /// checking ids read back from storage that bypassed `from_raw` (C6).
    pub fn is_valid(&self, id: Id<B>) -> bool {
        self.from_raw(id.raw()).is_some()
    }

    /// Sizes each shard's table for `n` values in all, plus 10% for uneven shards.
    /// Slot buckets keep their sizes, so their slack stays at most 2x.
    pub fn reserve(&self, n: usize) {
        let per_shard = n.div_ceil(S) + n.div_ceil(S) / 10;
        for shard in &self.shards {
            let mut locked = shard.lock.write();
            let additional = per_shard.saturating_sub(locked.table.len());
            locked
                .table
                .reserve(additional, |&e| table_hash((e >> 32) as u32));
        }
    }

    /// How many values this interner holds and the heap bytes it uses.
    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for shard in &self.shards {
            let locked = shard.lock.read();
            stats.entries += locked.len as usize;
            stats.bytes += locked.table.allocation_size() + locked.slot_bytes + locked.arena_bytes;
        }
        stats
    }
}

/// Declares a 4-byte handle type backed by its own static [`IdInterner`].
///
/// ```
/// immortal::id_handle! {
///     /// An interned name.
///     pub struct Name(str);
/// }
/// let a = Name::intern("x");
/// assert_eq!(a, Name::intern("x"));
/// assert_eq!(a.get(), "x");
/// assert_eq!(Name::try_get("y"), None);
/// ```
///
/// The shard count defaults to 256; `struct Name(str) in 16 shards;` sets it.
/// `struct Name(str) in 256 shards, 31 bits;` also keeps every id below `2^31`, so a
/// wrapper can use the top bit as a tag.
///
/// Handles of different types never mix, even over the same value type:
///
/// ```compile_fail
/// immortal::id_handle! { struct A(u32); }
/// immortal::id_handle! { struct B(u32); }
/// let b: B = A::intern(&1);
/// ```
#[macro_export]
macro_rules! id_handle {
    ($(#[$meta:meta])* $vis:vis struct $name:ident($inner:ty);) => {
        $crate::id_handle! { $(#[$meta])* $vis struct $name($inner) in 256 shards; }
    };
    ($(#[$meta:meta])* $vis:vis struct $name:ident($inner:ty) in $shards:literal shards;) => {
        $crate::id_handle! { $(#[$meta])* $vis struct $name($inner) in $shards shards, 32 bits; }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident($inner:ty) in $shards:literal shards, $bits:literal bits;
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        #[repr(transparent)]
        $vis struct $name($crate::Id<$name>);

        #[allow(dead_code)]
        impl $name {
            #[inline(always)]
            fn interner() -> &'static $crate::IdInterner<$inner, $name, $shards> {
                const BITS: u32 = $bits;
                static INTERNER: $crate::IdInterner<$inner, $name, $shards> =
                    // SAFETY: the brand is `$name`, which this invocation declares, and
                    // this static, in a non-generic fn, is the only interner branded `$name`.
                    unsafe { $crate::IdInterner::with_id_bits(BITS) };
                &INTERNER
            }

            /// The handle of `value`, interning it if it is new.
            #[inline]
            $vis fn intern(value: &$inner) -> Self {
                $name(Self::interner().intern(value))
            }

            /// The handle of `value` if it was interned. Never inserts.
            #[inline]
            $vis fn try_get(value: &$inner) -> ::std::option::Option<Self> {
                Self::interner().try_get(value).map($name)
            }

            /// The value, without a lock.
            #[inline]
            $vis fn get(self) -> &'static $inner {
                Self::interner().get(self.0)
            }

            /// The handle's bits, for storage that `from_raw` reads back.
            #[inline]
            $vis fn raw(self) -> u32 {
                self.0.raw()
            }

            /// The handle whose bits are `raw`, if it was handed out.
            #[inline]
            $vis fn from_raw(raw: u32) -> ::std::option::Option<Self> {
                Self::interner().from_raw(raw).map($name)
            }

            /// The handle whose bits are `raw`, unchecked, for bits taken from a handle
            /// this process holds.
            ///
            /// # Safety
            ///
            /// `raw` is `raw()` of a handle of this type that the caller holds or held.
            #[inline]
            $vis unsafe fn from_raw_unchecked(raw: u32) -> Self {
                // SAFETY: the caller's contract is `Id::from_raw_unchecked`'s.
                $name(unsafe { $crate::Id::from_raw_unchecked(raw) })
            }

            /// Whether this handle was handed out, for handles read back from storage
            /// that bypassed `from_raw`.
            #[inline]
            $vis fn is_valid(self) -> bool {
                Self::interner().is_valid(self.0)
            }

            /// Pre-sizes the interner for `n` values in all.
            $vis fn reserve(n: usize) {
                Self::interner().reserve(n)
            }

            /// How many values are interned and the heap bytes they take.
            $vis fn stats() -> $crate::Stats {
                Self::interner().stats()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Fewer values under Miri, which runs about a thousand times slower.
    const fn scale(native: usize, miri: usize) -> usize {
        if cfg!(miri) { miri } else { native }
    }

    #[test]
    fn handles_are_4_bytes() {
        id_handle! { struct H(u64); }
        assert_eq!(size_of::<H>(), 4);
        assert_eq!(size_of::<Option<H>>(), 4);
        assert_eq!(size_of::<Option<Id<H>>>(), 4);
    }

    #[test]
    fn interns_sized_values() {
        id_handle! { struct H(u64) in 4 shards; }
        let n = scale(10_000, 300) as u64;
        let ids: Vec<H> = (0..n).map(|k| H::intern(&k)).collect();
        for (k, &id) in ids.iter().enumerate() {
            assert_eq!(*id.get(), k as u64);
            assert_eq!(H::intern(&(k as u64)), id);
        }
        let mut raws: Vec<u32> = ids.iter().map(|id| id.raw()).collect();
        raws.sort_unstable();
        raws.dedup();
        assert_eq!(raws.len(), n as usize);
        assert_eq!(H::stats().entries, n as usize);
        assert!(H::stats().bytes > 0);
    }

    #[test]
    fn interns_str_and_slices() {
        id_handle! { struct S(str) in 2 shards; }
        id_handle! { struct L([u32]) in 2 shards; }
        assert_eq!(S::intern("").get(), "");
        assert_eq!(S::intern(""), S::intern(""));
        // Longer than the first chunk, so it skips ahead to a chunk it fits in.
        let long = "x".repeat(3000);
        let words: Vec<String> = (0..scale(5000, 200)).map(|k| format!("w{k}")).collect();
        let mut ids: Vec<S> = words.iter().map(|w| S::intern(w)).collect();
        ids.push(S::intern(&long));
        for (w, id) in words.iter().zip(&ids) {
            assert_eq!(id.get(), w);
            assert_eq!(S::try_get(w), Some(*id));
        }
        assert_eq!(ids.last().unwrap().get(), long);

        let a = L::intern(&[1, 2, 3]);
        assert_eq!(a, L::intern(&[1, 2, 3]));
        assert_ne!(a, L::intern(&[1, 2]));
        assert_eq!(a.get(), &[1, 2, 3]);
        assert_eq!(L::intern(&[]).get(), &[] as &[u32]);
    }

    #[test]
    fn try_get_never_inserts() {
        id_handle! { struct H(str); }
        assert_eq!(H::try_get("a"), None);
        assert_eq!(H::try_get("a"), None);
        assert_eq!(H::stats().entries, 0);
        let a = H::intern("a");
        assert_eq!(H::try_get("a"), Some(a));
        assert_eq!(H::try_get("b"), None);
        assert_eq!(H::stats().entries, 1);
    }

    #[test]
    fn from_raw_rejects_ids_never_issued() {
        id_handle! { struct H(u32) in 4 shards; }
        assert_eq!(H::from_raw(0), None);
        // Shard bits with no local bits.
        for shard in 0..4 {
            assert_eq!(H::from_raw(shard), None);
        }
        let ids: Vec<H> = (0..100u32).map(|k| H::intern(&k)).collect();
        for &id in &ids {
            assert_eq!(H::from_raw(id.raw()), Some(id));
            assert!(id.is_valid());
        }
        let issued: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        for raw in (0..2000).chain([u32::MAX, u32::MAX - 3, 1 << 31]) {
            assert_eq!(
                H::from_raw(raw).is_some(),
                issued.contains(&raw),
                "raw {raw}"
            );
        }
    }

    #[test]
    fn exhaustion_panics_with_the_type_name() {
        struct Tiny;
        static I: IdInterner<u64, Tiny, 2> =
            // SAFETY: the only interner branded `Tiny`.
            unsafe { IdInterner::with_limit(3) };
        let err = std::panic::catch_unwind(|| {
            for k in 0..10u64 {
                I.intern(&k);
            }
        })
        .expect_err("6 values fill both shards of 3");
        let msg = err.downcast_ref::<String>().unwrap();
        assert!(msg.contains("Tiny") && msg.contains("full"), "{msg}");
        // The interner is still usable, and the values it took are intact.
        let entries = I.stats().entries;
        assert!((3..=6).contains(&entries), "{entries}");
        for k in 0..10u64 {
            if let Some(id) = I.try_get(&k) {
                assert_eq!(*I.get(id), k);
            }
        }
    }

    #[test]
    fn id_bits_cap_every_id() {
        id_handle! { struct H(u64) in 4 shards, 4 bits; }
        // Each of the 4 shards holds 3 values, all below 2^4.
        let mut ids = Vec::new();
        let err = std::panic::catch_unwind(|| {
            for k in 0..40u64 {
                H::intern(&k);
            }
        })
        .expect_err("40 values overfill 4 shards of 3");
        let msg = err.downcast_ref::<String>().unwrap();
        assert!(msg.contains("H") && msg.contains("full"), "{msg}");
        for k in 0..40u64 {
            ids.extend(H::try_get(&k));
        }
        assert!((3..=12).contains(&ids.len()), "{}", ids.len());
        assert!(ids.iter().all(|id| id.raw() < 16));
    }

    #[test]
    fn real_cap_fits_32_bits() {
        type I1 = IdInterner<u8, (), 1>;
        type I256 = IdInterner<u8, (), 256>;
        assert_eq!(I1::CAP, u32::MAX);
        assert_eq!(I256::CAP, (1 << 24) - 1);
        let top = ((I256::CAP - 1 + 1) << I256::LOG2_S) | 255;
        assert_eq!(I256::decode(top), (255, I256::CAP - 1));
        assert!(split(I1::CAP as u64 - 1, FIRST_BUCKET).0 < BUCKETS);
        assert!(split(u32::MAX as u64, FIRST_CHUNK).0 < CHUNKS);
    }

    #[test]
    fn concurrent_stress() {
        id_handle! { struct H(u64) in 16 shards; }
        id_handle! { struct S(str) in 16 shards; }
        let threads = scale(16, 4);
        let n = scale(20_000, 100) as u64;
        // Every thread interns the same values in a different order, and checks each
        // id against the value as it goes, while the others are inserting.
        let results: Vec<(Vec<(u64, H)>, Vec<S>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    scope.spawn(move || {
                        let mut out = Vec::new();
                        let mut strs = Vec::new();
                        for j in 0..n {
                            let k = (j * 7919 + t as u64 * 104_729) % n;
                            let id = H::intern(&k);
                            assert_eq!(*id.get(), k);
                            assert_eq!(H::from_raw(id.raw()), Some(id));
                            out.push((k, id));
                            if j % 4 == 0 {
                                let s = S::intern(&k.to_string());
                                assert_eq!(s.get(), k.to_string());
                                strs.push(s);
                            }
                        }
                        (out, strs)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut seen: HashMap<u64, H> = HashMap::new();
        for (out, _) in &results {
            for &(k, id) in out {
                assert_eq!(*seen.entry(k).or_insert(id), id);
            }
        }
        assert_eq!(seen.len(), n as usize);
        assert_eq!(H::stats().entries, n as usize);
        for (_, strs) in &results {
            for s in strs {
                assert_eq!(S::try_get(s.get()), Some(*s));
            }
        }
    }
}
