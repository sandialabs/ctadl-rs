//! [`RawTable`]: one hash table that *is* a compact linear-probing table while it is small and a
//! Swiss table once it is large — one structure, one set of fields, no tag and no second type.
//!
//! # Why there is no enum
//!
//! The obvious way to build "one representation below a threshold, another above it" is an enum
//! with a small arm and a large arm. That costs three things, and this module exists to pay none
//! of them:
//!
//! 1. **A discriminant.** Even niche-filled it constrains the layout, and every accessor —
//!    `len`, `capacity`, `iter`, `insert` — reads it before it can read anything else.
//! 2. **Two of everything.** Two structs, two `Drop`s, two `Clone`s, two iterator types wrapped
//!    in two more enums, and a `match` in every method of the wrapper.
//! 3. **Width.** The struct is as wide as its widest arm plus whatever the tag cannot hide in.
//!
//! Instead the two regimes are the *same* four fields, read two ways, and which way is a property
//! of a number the table already has to keep:
//!
//! ```text
//!   small (cap <= SMALL):        large (cap > SMALL):
//!   [ T3 T2 T1 T0 ][ u64 ]       [ .. T1 T0 ][ ctrl bytes | mirror ]
//!                   ^ptr                      ^ptr
//!   metadata: 1 bit per slot     metadata: 1 byte per bucket
//! ```
//!
//! Both regimes put their **metadata at `ptr` and their elements below it**, so bucket `i` is at
//! `ptr - (i + 1) * size_of::<T>()` in either one — hashbrown's backwards-indexed element array,
//! reused verbatim for a table that has no control bytes at all. Both allocate one block, aligned
//! and laid out by one function. What differs is only how the bytes at `ptr` are read: a single
//! `u64` occupancy word where bit `i` means "slot `i` is full", or `cap + GROUP_WIDTH` control
//! bytes holding `h2`s. [`RawTable::is_large`] is `cap > SMALL` — an integer compare on a field
//! every operation loads anyway, decided by a bucket count that provably cannot be ambiguous (see
//! [`RawTable::MIN_LARGE_BUCKETS`]).
//!
//! The result is **16 bytes** — `{ptr, cap, len}` — for any `T`, against 24 for the enum this
//! replaces, on *every* set the enclosing map holds whether or not it ever promotes. And it is
//! not just narrower: `len` and `capacity` no longer branch at all, the iterator is one type
//! rather than three, and growth, promotion, cloning, dropping and freeing are each written once
//! for both regimes.
//!
//! # The small regime
//!
//! Open addressing with linear probing over `cap` slots (a power of two, so the bucket index is a
//! mask), and — the one unusual choice — the occupancy map is a **`u64` bitmask**, one bit per
//! slot, rather than a byte per slot:
//!
//! ```text
//! occupancy: 0b0010_1001          slots: [ x | _ | _ | y | _ | z | _ | _ ]
//! ```
//!
//! That buys three things a byte-per-slot control array does not:
//!
//! 1. **Eight bytes of metadata, not one per element plus a group mirror.** A small set's
//!    allocation is `cap * size_of::<T>()` rounded up to 8, plus 8 — and `cap` starts at **one**
//!    and doubles, so a singleton set costs one element and a 2-element set costs two. That
//!    matters because in the distribution this exists for, most sets are that small.
//! 2. **The whole probe reads memory once.** One aligned `u64` load covers every slot; from then
//!    on the empty/occupied test is a bit test on a register, and only a hit touches the element
//!    array. A Swiss table reloads control bytes once per group.
//! 3. **Iteration is `trailing_zeros`,** so iterating a sparse table costs O(elements), not
//!    O(slots) — which matters because every read view of the index iterates groups.
//!
//! The bitmask is what caps the threshold at 64 ([`MAX_SMALL_THRESHOLD`]).
//!
//! Because nothing is ever removed from a Datalog index, there are no tombstones: a probe
//! sequence stops at the first empty slot, and the table is allowed to fill *completely* before
//! it grows. A full small table degenerates to a linear scan of its slots, which at these sizes
//! is the same worst case a packed array would have had — but it means the small regime never
//! pays load-factor slack, only `Vec`-style doubling slack. It is the one place the design is
//! measurably worse than either alternative: a *miss* against a completely full table costs
//! ~19 ns at 32 slots and ~40 at 64, against ~2.5 ns for a sorted `Vec` or a `hashbrown` table
//! (`locals-trie-hybrid-eval.md` §3 finding 6, §10.1 finding 16).
//!
//! # Transitioning
//!
//! Promotion is [`RawTable::rebuild`] with a large `cap` — the same function growth uses, because
//! the two are the same operation: allocate the new block, move every element across, free the
//! old one. It is one pass over at most `SMALL` elements, sized for the known final element count
//! so the new table never rehashes while being filled, with elements **moved** rather than
//! cloned. Nothing else is touched: the transition is local to one table, never walks the
//! enclosing map, and without removals never happens twice for the same table.

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::marker::PhantomData;
use std::mem;
use std::ptr::NonNull;

use super::swiss::{
    BitMask, EMPTY, GROUP_WIDTH, Group, MIN_BUCKETS, ProbeSeq, buckets_for, capacity_for, ctrl_len,
    h2,
};

/// The largest threshold the small regime's `u64` occupancy bitmask can describe.
pub const MAX_SMALL_THRESHOLD: usize = u64::BITS as usize;

/// First non-zero slot count in the small regime.
///
/// **One**, not `Vec`'s minimum-non-zero-capacity of 4. A probe table has no reason to hold spare
/// slots — a 1-slot table is simply a table whose probe sequence has one stop — and in the
/// distribution this exists for, most sets never hold more than two elements. Starting at 4 would
/// round every one of those up to 96 B of leaves to hold 24; starting at 1 makes a singleton set
/// cost exactly its element. The price is one extra doubling (1, 2, 4, 8, …) for sets that do
/// grow, which is O(n) work amortized and allocator traffic that only the growing minority pays.
const SMALL_SLOTS_MIN: usize = 1;

/// Metadata alignment, and therefore the alignment of the whole allocation when `T` is not more
/// strictly aligned. Both regimes' metadata want the same 8: the occupancy word is a `u64` and a
/// control array is read a [`GROUP_WIDTH`]-byte word at a time.
const META_ALIGN: usize = GROUP_WIDTH;

/// Slots to allocate to hold `n` elements in the small regime: a power of two, at least
/// [`SMALL_SLOTS_MIN`], at most `small_max`. Load factor is allowed to reach 1.0, so this rounds
/// up only to the power of two, not past it.
#[inline]
pub(super) fn slot_count_for(n: usize, small_max: usize) -> usize {
    if n == 0 || small_max == 0 {
        0
    } else {
        n.next_power_of_two().clamp(SMALL_SLOTS_MIN, small_max)
    }
}

/// Where a linear probe sequence ended.
///
/// A returned enum, not a stored one: it is three words in a register across one `#[inline]`
/// call boundary and never reaches memory.
enum Probed {
    /// The element is at this slot.
    Found(usize),
    /// The element is absent; this empty slot is where it belongs.
    Vacant(usize),
    /// The element is absent and every slot is occupied.
    Full,
}

/// The metadata a table with no allocation points at: one word of zeroes, which reads as an
/// occupancy bitmask with every slot empty. An unallocated table is always in the small regime
/// (`cap == 0`), so these bytes are never read as control bytes, and never written — the first
/// insert allocates first.
#[repr(C, align(8))]
struct AlignedEmptyMeta([u8; GROUP_WIDTH]);

static EMPTY_META: AlignedEmptyMeta = AlignedEmptyMeta([0; GROUP_WIDTH]);

/// A hash table of `T` that is a linear-probing table at or below `SMALL` elements and a Swiss
/// table above it, in one 16-byte structure. See the module docs.
///
/// Holds no hasher: every operation takes the hash of the element it is looking for, plus an `eq`
/// closure and (where it may rehash) a `hash` closure, exactly as `hashbrown::HashTable` does.
/// [`super::HybridSet`] owns the `BuildHasher`.
///
/// `SMALL` must be `0` or a power of two no greater than [`MAX_SMALL_THRESHOLD`]; `0` means "never
/// small", which is how a pure Swiss table is obtained for A/B measurement.
///
/// # Invariants
///
/// * `cap` is `0`, or a power of two that is at most `SMALL` (**small**), or a power of two at
///   least [`Self::MIN_LARGE_BUCKETS`] (**large**). Those ranges do not overlap, so
///   `cap > SMALL` decides the regime.
/// * When `cap == 0` the table has no allocation and `ptr` points at [`EMPTY_META`]. Otherwise
///   `ptr` is `base + meta_offset` of a block laid out by [`Self::layout`], and is `META_ALIGN`-
///   aligned.
/// * Bucket `i < cap` holds an initialized `T` at `ptr - (i + 1) * size_of::<T>()` **iff** its
///   metadata says so: bit `i` of the occupancy word when small, a control byte with the high bit
///   clear when large.
/// * `len` is the number of such buckets. When large it never exceeds `capacity_for(cap)`, so at
///   least one bucket is always [`EMPTY`] and every group probe sequence terminates.
///
/// Every `unsafe` block below is justified by these four alone.
pub struct RawTable<T, const SMALL: usize> {
    /// Start of the metadata; also the *end* of the element array, which grows downwards.
    ptr: NonNull<u8>,
    /// Slot count when small, bucket count when large. `u32` to keep the whole table in two
    /// words: a single set would need 4 billion leaves to overflow it.
    cap: u32,
    len: u32,
    marker: PhantomData<T>,
}

// SAFETY: `RawTable` owns its elements outright — the only pointer into the allocation is `ptr`,
// and no `T` is aliased — so it may cross threads exactly when `T` may.
unsafe impl<T: Send, const SMALL: usize> Send for RawTable<T, SMALL> {}
// SAFETY: as above; `&RawTable<T, _>` hands out only `&T`.
unsafe impl<T: Sync, const SMALL: usize> Sync for RawTable<T, SMALL> {}

impl<T, const SMALL: usize> RawTable<T, SMALL> {
    /// Rejects a `SMALL` the invariants cannot hold for. Forced at every construction site.
    ///
    /// A power of two is required so that "the small regime's largest slot count" and "the
    /// threshold" are the same number, which is what makes `cap > SMALL` an exact test of the
    /// regime *and* makes `len > SMALL` an exact test of it too.
    const CHECK_SMALL: () = assert!(
        SMALL == 0 || (SMALL.is_power_of_two() && SMALL <= MAX_SMALL_THRESHOLD),
        "SMALL must be 0 or a power of two that fits the u64 occupancy bitmask"
    );

    /// Smallest bucket count the large regime allocates.
    ///
    /// Chosen to be **greater than every small `cap`**, which is what makes `cap > SMALL` a sound
    /// regime test rather than a coincidence. It never actually binds: the smallest large table
    /// is built for `SMALL + 1` elements, and `buckets_for(SMALL + 1) >= 2 * SMALL` for every
    /// power-of-two `SMALL`, so no table is ever rounded up to reach it. It is a static guarantee,
    /// not a cost — [`Self::allocate`] asserts as much.
    const MIN_LARGE_BUCKETS: usize = if 2 * SMALL > MIN_BUCKETS {
        2 * SMALL
    } else {
        MIN_BUCKETS
    };

    /// An empty table. Does not allocate.
    #[inline]
    pub fn new() -> Self {
        let () = Self::CHECK_SMALL;
        Self {
            ptr: NonNull::from(&EMPTY_META).cast(),
            cap: 0,
            len: 0,
            marker: PhantomData,
        }
    }

    /// A table sized to hold `capacity` elements without growing or changing regime — large from
    /// the outset if that many elements could not be small, so a caller that knows the final size
    /// skips the transition entirely.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = Self::cap_for(capacity);
        if cap == 0 {
            Self::new()
        } else {
            Self::allocate(cap)
        }
    }

    /// The `cap` a table holding `n` elements needs: slots while `n` fits the small regime,
    /// buckets once it does not.
    #[inline]
    fn cap_for(n: usize) -> usize {
        if n > SMALL {
            buckets_for(n).max(Self::MIN_LARGE_BUCKETS)
        } else {
            slot_count_for(n, SMALL)
        }
    }

    /// Whether this table has crossed the threshold and is a Swiss table.
    ///
    /// The whole of the dispatch: one compare against a constant, on a field every operation
    /// loads anyway. `cap == 0` is small, which is why an unallocated table needs no case of its
    /// own on any path.
    #[inline]
    pub fn is_large(&self) -> bool {
        self.cap as usize > SMALL
    }

    #[inline]
    fn cap(&self) -> usize {
        self.cap as usize
    }

    /// `cap - 1`: the index mask in either regime, since `cap` is a power of two. Wraps to
    /// `usize::MAX` for an unallocated table, whose probe loops run zero times.
    #[inline]
    fn mask(&self) -> usize {
        self.cap().wrapping_sub(1)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Elements this table can hold before it grows or changes regime. The small regime's load
    /// factor may reach 1.0, so that is simply its slot count.
    #[inline]
    pub fn capacity(&self) -> usize {
        if self.is_large() {
            capacity_for(self.cap())
        } else {
            self.cap()
        }
    }

    /// Elements that can still be added without growing.
    #[inline]
    fn growth_left(&self) -> usize {
        self.capacity() - self.len()
    }

    // -----------------------------------------------------------------------
    // Layout. One block, one function, both regimes.
    // -----------------------------------------------------------------------

    /// Metadata bytes a table of `cap` buckets needs: one occupancy word when small, one control
    /// byte per bucket plus the group mirror when large.
    #[inline]
    const fn meta_len(cap: usize) -> usize {
        if cap > SMALL {
            ctrl_len(cap)
        } else {
            mem::size_of::<u64>()
        }
    }

    /// Allocation layout of a `cap`-bucket table of `T`, and the offset of the metadata within
    /// it: the element array, padded up to the metadata alignment, then the metadata.
    ///
    /// hashbrown's `calculate_layout_for`, generalized over what the metadata is.
    fn layout(cap: usize) -> (Layout, usize) {
        debug_assert!(cap > 0 && cap.is_power_of_two());
        let align = mem::align_of::<T>().max(META_ALIGN);
        let meta_offset = (mem::size_of::<T>() * cap).next_multiple_of(align);
        let bytes = meta_offset + Self::meta_len(cap);
        let layout = Layout::from_size_align(bytes, align).expect("table layout overflowed");
        (layout, meta_offset)
    }

    /// Heap bytes held, including whatever slack the regime carries: the element slots plus the
    /// occupancy word while small, the buckets plus control bytes plus load-factor slack once
    /// large.
    ///
    /// This is the allocation the table actually made, not an estimate: unlike the
    /// [`super::super::hb_bytes`] model of hashbrown's layout that the enclosing map still needs,
    /// this structure computes its own layout, so it can simply report it.
    pub fn heap_bytes(&self) -> usize {
        if self.cap == 0 {
            0
        } else {
            Self::layout(self.cap()).0.size()
        }
    }

    /// Allocate a `cap`-bucket table with every bucket empty.
    fn allocate(cap: usize) -> Self {
        let () = Self::CHECK_SMALL;
        debug_assert!(cap > 0 && cap.is_power_of_two());
        debug_assert!(
            cap <= SMALL || cap >= Self::MIN_LARGE_BUCKETS,
            "a large cap must be unambiguously above every small one"
        );
        assert!(
            u32::try_from(cap).is_ok(),
            "a table is limited to 2^32 buckets"
        );
        let (layout, meta_offset) = Self::layout(cap);
        // SAFETY: `layout` has non-zero size — it includes at least 8 bytes of metadata.
        let base = unsafe { alloc(layout) };
        let Some(base) = NonNull::new(base) else {
            handle_alloc_error(layout)
        };
        // SAFETY: `meta_offset` is within `layout`, whose whole extent was just allocated.
        let ptr = unsafe { base.add(meta_offset) };
        if cap > SMALL {
            // SAFETY: the control array is `ctrl_len(cap)` bytes starting at `ptr`, all inside
            // the allocation, and `u8` needs no initialization to be written.
            unsafe { ptr.write_bytes(EMPTY, ctrl_len(cap)) };
        } else {
            // SAFETY: the occupancy word is 8 bytes starting at `ptr`, inside the allocation and
            // `META_ALIGN`-aligned by construction.
            unsafe { ptr.cast::<u64>().write(0) };
        }
        Self {
            ptr,
            cap: cap as u32,
            len: 0,
            marker: PhantomData,
        }
    }

    // -----------------------------------------------------------------------
    // Element and metadata access, shared by both regimes.
    // -----------------------------------------------------------------------

    /// Address of bucket `index`'s element.
    ///
    /// The element array runs *backwards* from `ptr`, as hashbrown's does, so that one pointer
    /// locates both arrays — in either regime, since both put their metadata at `ptr`.
    ///
    /// # Safety
    ///
    /// `index < self.cap()`, which implies the table is allocated. The returned pointer is only
    /// initialized when the bucket's metadata says so.
    #[inline]
    unsafe fn bucket(&self, index: usize) -> *mut T {
        debug_assert!(index < self.cap());
        // SAFETY: `meta_offset >= size_of::<T>() * cap`, so subtracting `(index + 1) * size` for
        // `index < cap` stays at or above the start of the allocation.
        unsafe {
            self.ptr
                .as_ptr()
                .sub((index + 1) * mem::size_of::<T>())
                .cast()
        }
    }

    /// The small regime's occupancy bitmask. Reads as all-empty for an unallocated table.
    #[inline]
    fn occupancy(&self) -> u64 {
        debug_assert!(!self.is_large());
        // SAFETY: `ptr` is `META_ALIGN`-aligned and addresses at least 8 initialized bytes —
        // written by `allocate`, or the `EMPTY_META` static when unallocated.
        unsafe { self.ptr.as_ptr().cast::<u64>().read() }
    }

    /// Overwrite the small regime's occupancy bitmask.
    ///
    /// # Safety
    ///
    /// The table must be small **and allocated** (`EMPTY_META` is not writable), and `bits` must
    /// describe exactly the initialized slots.
    #[inline]
    unsafe fn set_occupancy(&mut self, bits: u64) {
        debug_assert!(!self.is_large() && self.cap > 0);
        // SAFETY: the caller guarantees an allocated small table, whose metadata is a writable,
        // aligned `u64` at `ptr`.
        unsafe { self.ptr.as_ptr().cast::<u64>().write(bits) }
    }

    /// Load the group of control bytes starting at bucket `pos`.
    ///
    /// # Safety
    ///
    /// The table must be large and `pos <= self.mask()`, so the group stays inside the control
    /// array (whose trailing [`GROUP_WIDTH`] mirror bytes exist for exactly this).
    #[inline]
    unsafe fn group_at(&self, pos: usize) -> Group {
        debug_assert!(self.is_large() && pos <= self.mask());
        // SAFETY: `pos + GROUP_WIDTH <= cap + GROUP_WIDTH`, the control array's length.
        unsafe { Group::load(self.ptr.as_ptr().add(pos)) }
    }

    /// Set bucket `index`'s control byte, keeping the mirror in sync.
    ///
    /// The mirrored index is hashbrown's branchless `((index - GROUP_WIDTH) & mask) + GROUP_WIDTH`
    /// — which is `index` itself for `index >= GROUP_WIDTH` and `index + cap` below it, i.e.
    /// exactly the bytes a wrapped group load can read.
    ///
    /// # Safety
    ///
    /// The table must be large and `index < self.cap()`.
    #[inline]
    unsafe fn set_ctrl(&mut self, index: usize, byte: u8) {
        debug_assert!(self.is_large() && index < self.cap());
        let mask = self.mask();
        let mirror = (index.wrapping_sub(GROUP_WIDTH) & mask) + GROUP_WIDTH;
        // SAFETY: both `index` and `mirror` are below `ctrl_len(cap)`, the length of the control
        // array.
        unsafe {
            self.ptr.as_ptr().add(index).write(byte);
            self.ptr.as_ptr().add(mirror).write(byte);
        }
    }

    /// Mark bucket `index` full, having just written an element there. `h2` is ignored by the
    /// small regime, which records occupancy positionally.
    ///
    /// # Safety
    ///
    /// `index < self.cap()`, the table is allocated, and bucket `index` now holds an initialized
    /// `T` that no other bucket claims. Does **not** bump `len`; the caller does.
    #[inline]
    unsafe fn mark_full(&mut self, index: usize, h2: u8) {
        if self.is_large() {
            // SAFETY: large, and `index < cap` by the caller's guarantee.
            unsafe { self.set_ctrl(index, h2) }
        } else {
            let bits = self.occupancy() | (1u64 << index);
            // SAFETY: small and allocated by the caller's guarantee; the new bit is the slot the
            // caller just initialized.
            unsafe { self.set_occupancy(bits) }
        }
    }

    /// Mark bucket `index` empty, having just moved its element out, and drop `len` by one.
    ///
    /// In the large regime this writes [`EMPTY`] where a probe sequence may pass, so it is only
    /// sound for a table that will not be searched again — which is the only caller
    /// ([`IntoIter`], which owns the table).
    ///
    /// # Safety
    ///
    /// `index < self.cap()` and bucket `index` was full and has been moved out of.
    #[inline]
    unsafe fn clear_slot(&mut self, index: usize) {
        if self.is_large() {
            // SAFETY: large, and `index < cap` by the caller's guarantee.
            unsafe { self.set_ctrl(index, EMPTY) }
        } else {
            let bits = self.occupancy() & !(1u64 << index);
            // SAFETY: small, and allocated because it held an element.
            unsafe { self.set_occupancy(bits) }
        }
        self.len -= 1;
    }

    /// Bucket `index`'s control byte, or 0 in the small regime, which has none.
    ///
    /// # Safety
    ///
    /// `index < self.cap()`.
    #[inline]
    unsafe fn meta_byte(&self, index: usize) -> u8 {
        if self.is_large() {
            // SAFETY: `index < cap < ctrl_len(cap)`.
            unsafe { self.ptr.as_ptr().add(index).read() }
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------
    // Lookup.
    // -----------------------------------------------------------------------

    /// The element equal to the one being looked up, or `None`.
    #[inline]
    pub fn find(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<&T> {
        let index = self.find_index(hash, &mut eq)?;
        // SAFETY: `find_index` only returns the index of a full bucket.
        Some(unsafe { &*self.bucket(index) })
    }

    #[inline]
    fn find_index(&self, hash: u64, eq: &mut impl FnMut(&T) -> bool) -> Option<usize> {
        if self.is_large() {
            self.large_find_index(hash, eq)
        } else {
            match self.small_probe(hash, eq) {
                Probed::Found(i) => Some(i),
                _ => None,
            }
        }
    }

    /// Walk the small regime's probe sequence once, reporting where the element is or where it
    /// would go. One pass serves both `contains` and `insert`.
    ///
    /// Linear probing from `hash`, stopping at the first empty slot — sound because elements are
    /// never removed, so no tombstone can hide a later match. The `for` bound also makes a
    /// completely full table terminate, as [`Probed::Full`], and makes an unallocated table
    /// (`cap == 0`) report `Full` without a case of its own.
    #[inline]
    fn small_probe(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Probed {
        debug_assert!(!self.is_large());
        // One load covers the whole table; every step after this is a register bit test.
        let bits = self.occupancy();
        let cap = self.cap();
        let mask = self.mask();
        let mut i = (hash as usize) & mask;
        for _ in 0..cap {
            if bits & (1u64 << i) == 0 {
                return Probed::Vacant(i);
            }
            // SAFETY: bit `i` is set, so bucket `i` holds an initialized element, and `i <= mask`.
            if eq(unsafe { &*self.bucket(i) }) {
                return Probed::Found(i);
            }
            i = (i + 1) & mask;
        }
        Probed::Full
    }

    #[inline]
    fn large_find_index(&self, hash: u64, eq: &mut impl FnMut(&T) -> bool) -> Option<usize> {
        debug_assert!(self.is_large());
        let h2 = h2(hash);
        let mask = self.mask();
        let mut probe = ProbeSeq::new(hash, mask);
        loop {
            // SAFETY: `probe.pos` is always masked to `mask`, on a large table.
            let group = unsafe { self.group_at(probe.pos) };
            for bit in group.match_byte(h2) {
                let index = (probe.pos + bit) & mask;
                // SAFETY: the control byte at `index` matched an `h2`, so its high bit is clear
                // and the bucket holds an initialized element.
                if eq(unsafe { &*self.bucket(index) }) {
                    return Some(index);
                }
            }
            // A group with an empty bucket ends the probe sequence: with no tombstones, an
            // element that hashed here would have been placed at or before it.
            if group.match_empty().any() {
                return None;
            }
            probe.move_next(mask);
        }
    }

    /// Where an element known to be absent belongs in the large regime: the first empty bucket in
    /// its probe sequence. Requires [`Self::growth_left`] > 0, which guarantees one exists.
    #[inline]
    fn find_insert_slot(&self, hash: u64) -> usize {
        debug_assert!(self.is_large() && self.growth_left() > 0);
        let mask = self.mask();
        let mut probe = ProbeSeq::new(hash, mask);
        loop {
            // SAFETY: `probe.pos` is always masked to `mask`, on a large table.
            let group = unsafe { self.group_at(probe.pos) };
            if let Some(bit) = group.match_empty().lowest() {
                // Masking is what makes a hit in the mirror bytes name the bucket it mirrors.
                return (probe.pos + bit) & mask;
            }
            probe.move_next(mask);
        }
    }

    // -----------------------------------------------------------------------
    // Insertion.
    // -----------------------------------------------------------------------

    /// Insert `value` if no equal element is present; returns whether it was added.
    ///
    /// `hash` must be the hash of `value`, and `hash_of` must agree with it for every element
    /// already in the table.
    ///
    /// Unlike [`Self::find`] this compares with `T: Eq` rather than a caller-supplied closure: a
    /// closure would have to borrow the very value being moved in. hashbrown solves that with its
    /// `Entry` API; a set only ever needs equality on whole elements, so this asks for it
    /// directly.
    pub fn insert(&mut self, hash: u64, value: T, hash_of: impl Fn(&T) -> u64) -> bool
    where
        T: Eq,
    {
        if self.is_large() {
            self.large_insert(hash, value, hash_of)
        } else {
            self.small_insert(hash, value, hash_of)
        }
    }

    fn small_insert(&mut self, hash: u64, value: T, hash_of: impl Fn(&T) -> u64) -> bool
    where
        T: Eq,
    {
        // Bind the probe result so the `eq` closure — which borrows `value` — is dropped before
        // `value` is moved into the table.
        let probed = self.small_probe(hash, |x| *x == value);
        match probed {
            Probed::Found(_) => false,
            Probed::Vacant(i) => {
                // SAFETY: `Vacant` names an empty slot of an allocated table (`cap == 0` probes
                // as `Full`), so writing there overwrites nothing.
                unsafe {
                    self.bucket(i).write(value);
                    self.mark_full(i, 0);
                }
                self.len += 1;
                true
            }
            // Absent, and there is nowhere to put it: either grow or change regime. Both are one
            // call to `rebuild`, sized so that the element about to be added fits without a
            // second growth.
            Probed::Full => {
                self.rebuild(Self::cap_for(self.len() + 1), hash_of);
                // SAFETY: `rebuild` sized the table for one more element, and the element is
                // absent — the probe that reported `Full` searched every slot.
                unsafe { self.insert_unique_no_grow(hash, value) };
                true
            }
        }
    }

    fn large_insert(&mut self, hash: u64, value: T, hash_of: impl Fn(&T) -> u64) -> bool
    where
        T: Eq,
    {
        let h2 = h2(hash);
        let mask = self.mask();
        let mut probe = ProbeSeq::new(hash, mask);
        let slot = loop {
            // SAFETY: `probe.pos` is always masked to `mask`, on a large table.
            let group = unsafe { self.group_at(probe.pos) };
            for bit in group.match_byte(h2) {
                let index = (probe.pos + bit) & mask;
                // SAFETY: the control byte at `index` matched an `h2`, so the bucket is full.
                if unsafe { &*self.bucket(index) } == &value {
                    return false;
                }
            }
            if let Some(bit) = group.match_empty().lowest() {
                break (probe.pos + bit) & mask;
            }
            probe.move_next(mask);
        };

        // Absent. `slot` is where it goes — unless there is no room, in which case the table is
        // rebuilt and the slot recomputed.
        let slot = if self.growth_left() == 0 {
            self.rebuild(self.cap() * 2, hash_of);
            self.find_insert_slot(hash)
        } else {
            slot
        };
        // SAFETY: `slot` is an empty bucket of an allocated large table.
        unsafe {
            self.bucket(slot).write(value);
            self.set_ctrl(slot, h2);
        }
        self.len += 1;
        true
    }

    /// Insert an element known to be absent into a table known to have room.
    ///
    /// # Safety
    ///
    /// No element equal to `value` is present, `hash` is its hash, and
    /// [`Self::growth_left`] > 0 — for the small regime, that means an empty slot exists.
    #[inline]
    unsafe fn insert_unique_no_grow(&mut self, hash: u64, value: T) {
        debug_assert!(self.growth_left() > 0);
        let index = if self.is_large() {
            self.find_insert_slot(hash)
        } else {
            let mask = self.mask();
            let bits = self.occupancy();
            let mut i = (hash as usize) & mask;
            while bits & (1u64 << i) != 0 {
                i = (i + 1) & mask;
            }
            i
        };
        // SAFETY: `index` is an empty bucket of an allocated table, so writing there overwrites
        // nothing, and the metadata marks it full afterwards.
        unsafe {
            self.bucket(index).write(value);
            self.mark_full(index, h2(hash));
        }
        self.len += 1;
    }

    /// Make room for `additional` more elements, changing regime up front if that many more could
    /// not be held small.
    ///
    /// Callers that only have an *upper bound* on the additional count (a union, say, whose
    /// overlap is unknown) should not use this: over-reserving here permanently promotes a table
    /// that might have stayed small.
    pub fn reserve(&mut self, additional: usize, hash_of: impl Fn(&T) -> u64) {
        let want = self.len() + additional;
        if want <= self.capacity() {
            return;
        }
        self.rebuild(Self::cap_for(want), hash_of);
    }

    /// Rebuild into a fresh `new_cap`-bucket table, moving every element across and rehashing it.
    ///
    /// This is growth, promotion and reservation all at once: the regimes of the old and new
    /// tables are read off their own `cap`s, so a small→small doubling, a small→large promotion
    /// and a large→large doubling are the same three lines. Nothing distinguishes promotion but
    /// the number passed in.
    ///
    /// hashbrown can sometimes rehash *in place*, reusing the allocation by shuffling elements
    /// between the buckets a tombstone freed. Without removals there are no such buckets and no
    /// such case: a rebuild here is always a fresh allocation, one linear pass, and a free.
    fn rebuild(&mut self, new_cap: usize, hash_of: impl Fn(&T) -> u64) {
        debug_assert!(new_cap > self.len());
        let mut fresh = Self::allocate(new_cap);
        // Snapshot the element positions, then disown them *before* the loop: the iterator has
        // already recorded how many to visit, and disowning up front means a panic in `hash_of`
        // leaks the un-moved remainder rather than dropping elements the loop has already moved
        // into `fresh`.
        let indices = self.raw_iter();
        let moved = self.len;
        self.len = 0;
        for index in indices {
            // SAFETY: `raw_iter` yields full buckets, each exactly once, so this reads each
            // element once — and `self` no longer claims to own any of them.
            let value = unsafe { self.bucket(index).read() };
            let hash = hash_of(&value);
            // SAFETY: the elements come from a set, so they are distinct, and `fresh` was sized
            // for all of them, so it never runs out of room.
            unsafe { fresh.insert_unique_no_grow(hash, value) };
        }
        debug_assert_eq!(fresh.len, moved);
        // Dropping the old table now frees its allocation and drops nothing, since it owns
        // nothing.
        *self = fresh;
    }

    // -----------------------------------------------------------------------
    // Iteration.
    // -----------------------------------------------------------------------

    /// Indices of the full buckets, in bucket order.
    ///
    /// The one iterator both regimes use. The small regime's whole occupancy word is loaded here
    /// and never refilled; the large regime's first control group is loaded here and refilled a
    /// group at a time. See [`RawIter`] for how that is one code path.
    #[inline]
    fn raw_iter(&self) -> RawIter {
        let (bits, shift) = if self.is_large() {
            // SAFETY: an allocated large table has at least `MIN_BUCKETS == GROUP_WIDTH` control
            // bytes.
            let group = unsafe { Group::load(self.ptr.as_ptr()) };
            (group.match_full().0, BitMask::SHIFT)
        } else {
            // `>=`, not `==`: `rebuild` deliberately zeroes `len` while the bits still stand, so
            // that a panic mid-move leaks rather than double-drops. Everywhere else they agree.
            debug_assert!(self.occupancy().count_ones() as usize >= self.len());
            (self.occupancy(), 0)
        };
        RawIter {
            meta: self.ptr.as_ptr(),
            bits,
            base: 0,
            shift,
            remaining: self.len(),
            cap: self.cap(),
        }
    }

    /// Elements, in unspecified order.
    #[inline]
    pub fn iter(&self) -> Iter<'_, T, SMALL> {
        Iter {
            indices: self.raw_iter(),
            table: self,
        }
    }

    /// Drop every element and release the allocation, leaving an unallocated table.
    fn free(&mut self) {
        if mem::needs_drop::<T>() {
            for index in self.raw_iter() {
                // SAFETY: `raw_iter` yields each full bucket once, so each element is dropped
                // exactly once.
                unsafe { self.bucket(index).drop_in_place() };
            }
        }
        if self.cap != 0 {
            let (layout, meta_offset) = Self::layout(self.cap());
            // SAFETY: `ptr` was allocated at `base + meta_offset` with this exact layout in
            // `allocate`, and nothing else frees it (`self` is left unallocated below).
            unsafe { dealloc(self.ptr.as_ptr().sub(meta_offset), layout) };
        }
        self.ptr = NonNull::from(&EMPTY_META).cast();
        self.cap = 0;
        self.len = 0;
    }
}

impl<T, const SMALL: usize> Default for RawTable<T, SMALL> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const SMALL: usize> Drop for RawTable<T, SMALL> {
    fn drop(&mut self) {
        self.free();
    }
}

impl<T: Clone, const SMALL: usize> Clone for RawTable<T, SMALL> {
    /// Copies the metadata element by element and clones each element into the *same* bucket, so
    /// the copy costs no hashing at all and every probe sequence is preserved — in either regime,
    /// since a slot's position is all the small regime's metadata says about it.
    fn clone(&self) -> Self {
        if self.cap == 0 {
            return Self::new();
        }
        let mut fresh = Self::allocate(self.cap());
        for index in self.raw_iter() {
            // SAFETY: `index` is a full bucket of `self`, and an empty one of `fresh` (which was
            // just allocated with the same `cap`, so the index is in range).
            unsafe {
                fresh.bucket(index).write((*self.bucket(index)).clone());
                // Keep the metadata the source had: for a large table that is the element's own
                // `h2`, so the clone answers every probe identically.
                let byte = self.meta_byte(index);
                fresh.mark_full(index, byte);
            }
            // Bumped per element so that a panicking `T::clone` leaves `fresh` consistent and its
            // destructor drops exactly what was written.
            fresh.len += 1;
        }
        fresh
    }
}

/// Iterator over the indices of a table's full buckets, in **either** regime.
///
/// Both regimes' metadata is "one bit per bucket, lowest bucket in the lowest bit"; they differ
/// only in how far apart the bits are and in whether there is more metadata to load after the
/// first word:
///
/// | | bit spacing | words |
/// |-|-------------|-------|
/// | small | 1 bit per slot (`shift == 0`) | one, covering the whole table |
/// | large | 1 byte per bucket ([`BitMask::SHIFT`]) | one per [`GROUP_WIDTH`] buckets |
///
/// So the difference is a shift amount and a loop that a small table never enters — `remaining`
/// reaches zero first, because the one word it loaded described every element. No tag, no branch
/// per element, and one `next` for both.
#[derive(Clone)]
struct RawIter {
    /// The table's metadata pointer.
    meta: *const u8,
    /// Bits still to yield, at `1 << (index << shift)`.
    bits: u64,
    /// Bucket index of bit 0 of `bits`.
    base: usize,
    /// `0` for a bitmask, [`BitMask::SHIFT`] for control bytes.
    shift: u32,
    /// Elements not yet yielded. Also what stops the large regime walking off the control array.
    remaining: usize,
    /// Bucket count, for a debug assertion only.
    cap: usize,
}

impl Iterator for RawIter {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.remaining == 0 {
            return None;
        }
        loop {
            if self.bits != 0 {
                let index = self.base + (self.bits.trailing_zeros() as usize >> self.shift);
                // Clear the lowest set bit.
                self.bits &= self.bits - 1;
                self.remaining -= 1;
                return Some(index);
            }
            // `remaining > 0`, so a full bucket exists in a later group and this cannot walk off
            // the end. A small table never reaches here: its single word described every element,
            // so `remaining` hit zero above.
            self.base += GROUP_WIDTH;
            debug_assert!(self.base < self.cap, "ran past the metadata");
            // SAFETY: `base` is a multiple of `GROUP_WIDTH` below `cap`, which is itself a
            // multiple of `GROUP_WIDTH` (a power of two >= MIN_BUCKETS == GROUP_WIDTH), so the
            // group lies inside the control array.
            self.bits = unsafe { Group::load(self.meta.add(self.base)) }
                .match_full()
                .0;
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RawIter {}

/// Shared-borrow iterator over a [`RawTable`], in bucket order.
pub struct Iter<'a, T, const SMALL: usize> {
    indices: RawIter,
    table: &'a RawTable<T, SMALL>,
}

impl<'a, T, const SMALL: usize> Iterator for Iter<'a, T, SMALL> {
    type Item = &'a T;
    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        let index = self.indices.next()?;
        // SAFETY: `index` is a full bucket of `self.table`, which is borrowed for `'a`, so the
        // element stays initialized and unaliased for that long.
        Some(unsafe { &*self.table.bucket(index) })
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}

impl<T, const SMALL: usize> ExactSizeIterator for Iter<'_, T, SMALL> {}

// Hand-written rather than derived: a shared-borrow iterator is copyable whatever `T` is, and
// `derive(Clone)` would demand `T: Clone`.
impl<T, const SMALL: usize> Clone for Iter<'_, T, SMALL> {
    fn clone(&self) -> Self {
        Self {
            indices: self.indices.clone(),
            table: self.table,
        }
    }
}

impl<'a, T, const SMALL: usize> IntoIterator for &'a RawTable<T, SMALL> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T, SMALL>;
    fn into_iter(self) -> Iter<'a, T, SMALL> {
        self.iter()
    }
}

/// Owning iterator over a [`RawTable`], in bucket order.
///
/// Each yielded element's bucket is marked empty as it leaves, so whatever the caller does not
/// consume is dropped exactly once by the table's own destructor.
pub struct IntoIter<T, const SMALL: usize> {
    table: RawTable<T, SMALL>,
    indices: RawIter,
}

impl<T, const SMALL: usize> Iterator for IntoIter<T, SMALL> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        let index = self.indices.next()?;
        // SAFETY: `index` is a full bucket; clearing its metadata first means the table's
        // destructor will not touch the element this reads out. Clearing cannot break a later
        // lookup because `IntoIter` owns the table and nothing can look it up again.
        unsafe {
            let value = self.table.bucket(index).read();
            self.table.clear_slot(index);
            Some(value)
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}

impl<T, const SMALL: usize> ExactSizeIterator for IntoIter<T, SMALL> {}

impl<T, const SMALL: usize> IntoIterator for RawTable<T, SMALL> {
    type Item = T;
    type IntoIter = IntoIter<T, SMALL>;
    fn into_iter(self) -> IntoIter<T, SMALL> {
        let indices = self.raw_iter();
        IntoIter {
            table: self,
            indices,
        }
    }
}
