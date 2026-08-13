//! [`RawTable`] is one hash table that *is* a compact linear-probing table while it is small, and
//! a Swiss table once it is large. It stays one structure with one set of fields; there is no tag
//! and no second type.
//!
//! # Why there is no enum
//!
//! The obvious way to build "one representation below a threshold, another above it" is an enum
//! with a small arm and a large arm. That costs three things, and this module exists so that we
//! pay none of them:
//!
//! 1. **A discriminant.** Even filled into a niche it constrains the layout, and every accessor
//!    reads it before it can read anything else. That means `len`, `capacity`, `iter`, and
//!    `insert`.
//! 2. **Two of everything.** Two structs, two `Drop`s, two `Clone`s, two iterator types wrapped
//!    in two more enums, and a `match` in every method of the wrapper.
//! 3. **Width.** The struct is as wide as its widest arm, plus whatever the tag cannot hide in.
//!
//! Instead, the two regimes are the *same* four fields read two ways. Which way to read them is a
//! property of a number the table already has to keep:
//!
//! ```text
//!   small (cap <= SMALL):        large (cap > SMALL):
//!   [ T3 T2 T1 T0 ][ u64 ]       [ .. T1 T0 ][ ctrl bytes | mirror ]
//!                   ^ptr                      ^ptr
//!   metadata: 1 bit per slot     metadata: 1 byte per bucket
//! ```
//!
//! Both regimes put their **metadata at `ptr` and their elements below it**. So in either one,
//! bucket `i` sits at `ptr - (i + 1) * size_of::<T>()`. That is hashbrown's backwards-indexed
//! element array, reused verbatim for a table that has no control bytes at all. Both regimes
//! allocate one block, aligned and laid out by one function. All that differs is how we read the
//! bytes at `ptr`: either as a single `u64` occupancy word, where bit `i` means "slot `i` is
//! full", or as `cap + GROUP_WIDTH` control bytes holding `h2`s. [`RawTable::is_large`] is
//! `cap > SMALL`, an integer compare on a field every operation loads anyway. The bucket count
//! that decides it provably cannot be ambiguous; see [`RawTable::MIN_LARGE_BUCKETS`].
//!
//! The result is **16 bytes**, namely `{ptr, cap, len}`, for any `T`. The enum this replaces cost
//! 32, on *every* set the enclosing map holds, whether or not that set ever promotes. Being
//! narrower is not the only gain: `len` and `capacity` no longer branch at all, the iterator is
//! one type rather than three, and growth, promotion, cloning, dropping, and freeing are each
//! written once for both regimes.
//!
//! # The small regime
//!
//! The small regime is open addressing with linear probing over `cap` slots. `cap` is a power of
//! two, so the bucket index is a mask. The one unusual choice is the occupancy map: it is a
//! **`u64` bitmask** with one bit per slot, rather than a byte per slot.
//!
//! ```text
//! occupancy: 0b0010_1001          slots: [ x | _ | _ | y | _ | z | _ | _ ]
//! ```
//!
//! That buys us three things a byte-per-slot control array does not:
//!
//! 1. **Eight bytes of metadata, rather than one per element plus a group mirror.** A small set's
//!    allocation is `cap * size_of::<T>()` rounded up to 8, plus 8. And `cap` starts at **one**
//!    and doubles, so a singleton set costs one element and a 2-element set costs two. That
//!    matters, because in the distribution this exists for, most sets are that small.
//! 2. **The whole probe reads memory once.** One aligned `u64` load covers every slot. From then
//!    on, the empty-or-occupied test is a bit test on a register, and only a hit touches the
//!    element array. A Swiss table reloads control bytes once per group.
//! 3. **Iteration is `trailing_zeros`.** So iterating a sparse table costs O(elements) rather
//!    than O(slots), which matters because every read view of the index iterates groups.
//!
//! The bitmask is also what caps the threshold at 64; see [`MAX_SMALL_THRESHOLD`].
//!
//! Nothing is ever removed from a Datalog index, so there are no tombstones. A probe sequence can
//! therefore stop at the first empty slot, and we let the table fill *completely* before it
//! grows. A full small table degenerates to a linear scan of its slots. At these sizes that is
//! the same worst case a packed array would have had, and it means the small regime never pays
//! load-factor slack, only `Vec`-style doubling slack.
//!
//! This is the one place where the design is measurably worse than either alternative. A *miss*
//! against a completely full table costs about 21 ns at 32 slots and about 45 ns at 64, against
//! about 5 to 6 ns for a sorted `Vec`'s binary search and about 2.5 ns for a `hashbrown` table.
//! [`super::SMALL_THRESHOLD`]'s docs argue why the workload does not pay for that.
//!
//! # Transitioning
//!
//! Promotion is [`RawTable::rebuild`] with a large `cap`. That is the same function growth uses,
//! because the two are the same operation: allocate the new block, move every element across,
//! free the old one. It takes one pass over at most `SMALL` elements. We size the new table for
//! the final element count, which we already know, so it never rehashes while being filled, and
//! we **move** elements rather than clone them. Nothing else is touched. The transition stays
//! local to one table, never walks the enclosing map, and, without removals, never happens twice
//! for the same table.

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

/// The first non-zero slot count in the small regime.
///
/// It is **one**, not `Vec`'s minimum non-zero capacity of 4. A probe table has no reason to hold
/// spare slots; a 1-slot table is simply a table whose probe sequence has one stop. And in the
/// distribution this exists for, most sets never hold more than two elements. Starting at 4 would
/// round every one of those up to 96 B of leaves to hold 24 B, whereas starting at 1 makes a
/// singleton set cost exactly its element. The price is one extra doubling for sets that do grow
/// (1, 2, 4, 8, and so on). That is O(n) work amortized, plus allocator traffic that only the
/// growing minority pays.
const SMALL_SLOTS_MIN: usize = 1;

/// The metadata alignment. It is also the alignment of the whole allocation, unless `T` is more
/// strictly aligned. Both regimes' metadata want the same 8 bytes: the occupancy word is a `u64`,
/// and we read a control array one [`GROUP_WIDTH`]-byte word at a time.
const META_ALIGN: usize = GROUP_WIDTH;

/// How many slots to allocate to hold `n` elements in the small regime. The answer is a power of
/// two, at least [`SMALL_SLOTS_MIN`] and at most `small_max`. The load factor may reach 1.0, so
/// this rounds up to the power of two and no further.
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
/// We return this enum; we never store it. It lives in a register across one `#[inline]` call
/// boundary and never reaches memory.
enum Probed {
    /// The element is at this slot.
    Found(usize),
    /// The element is absent, and this empty slot is where it belongs.
    Vacant(usize),
    /// The element is absent and every slot is occupied.
    Full,
}

/// The metadata a table with no allocation points at. It is one word of zeroes, which reads as an
/// occupancy bitmask with every slot empty. An unallocated table is always in the small regime,
/// because `cap == 0`, so nothing ever reads these bytes as control bytes. Nothing ever writes
/// them either, because the first insert allocates first.
#[repr(C, align(8))]
struct AlignedEmptyMeta([u8; GROUP_WIDTH]);

static EMPTY_META: AlignedEmptyMeta = AlignedEmptyMeta([0; GROUP_WIDTH]);

/// A hash table of `T` in one 16-byte structure. It is a linear-probing table at or below `SMALL`
/// elements, and a Swiss table above that. See the module docs.
///
/// It holds no hasher. Every operation takes the hash of the element it is looking for, plus an
/// `eq` closure, plus a `hash` closure where it may rehash. `hashbrown::HashTable` works the same
/// way. [`super::HybridSet`] owns the `BuildHasher`.
///
/// `SMALL` must be `0`, or a power of two no greater than [`MAX_SMALL_THRESHOLD`]. `0` means
/// "never small", which is how we get a pure Swiss table for A/B measurement.
///
/// # Invariants
///
/// * `cap` is `0`, or a power of two that is at most `SMALL` (**small**), or a power of two at
///   least [`Self::MIN_LARGE_BUCKETS`] (**large**). Those ranges do not overlap, so
///   `cap > SMALL` decides the regime.
/// * When `cap == 0` the table has no allocation and `ptr` points at [`EMPTY_META`]. Otherwise
///   `ptr` is `base + meta_offset` of a block laid out by [`Self::layout`], and is `META_ALIGN`-
///   aligned.
/// * Bucket `i < cap` holds an initialized `T` at `ptr - (i + 1) * size_of::<T>()` **if and only
///   if** its metadata says so. That means bit `i` of the occupancy word when small, and a
///   control byte with the high bit clear when large.
/// * `len` is the number of such buckets. When the table is large, `len` never exceeds
///   `capacity_for(cap)`. So at least one bucket is always [`EMPTY`], and every group probe
///   sequence terminates.
///
/// These four invariants alone justify every `unsafe` block below.
pub struct RawTable<T, const SMALL: usize> {
    /// Start of the metadata. It is also the *end* of the element array, which grows downwards.
    ptr: NonNull<u8>,
    /// Slot count when small, bucket count when large. It is a `u32` so that the whole table fits
    /// in two words; a single set would need 4 billion leaves to overflow it.
    cap: u32,
    len: u32,
    marker: PhantomData<T>,
}

// SAFETY: `RawTable` owns its elements outright. The only pointer into the allocation is `ptr`,
// and no `T` is aliased, so the table may cross threads exactly when `T` may.
unsafe impl<T: Send, const SMALL: usize> Send for RawTable<T, SMALL> {}
// SAFETY: as above, and `&RawTable<T, _>` hands out only `&T`.
unsafe impl<T: Sync, const SMALL: usize> Sync for RawTable<T, SMALL> {}

impl<T, const SMALL: usize> RawTable<T, SMALL> {
    /// Rejects a `SMALL` that the invariants cannot hold for. Every construction site forces it.
    ///
    /// We require a power of two so that "the small regime's largest slot count" and "the
    /// threshold" are the same number. That is what makes `cap > SMALL` an exact test of the
    /// regime, and what makes `len > SMALL` an exact test of it too.
    const CHECK_SMALL: () = assert!(
        SMALL == 0 || (SMALL.is_power_of_two() && SMALL <= MAX_SMALL_THRESHOLD),
        "SMALL must be 0 or a power of two that fits the u64 occupancy bitmask"
    );

    /// The smallest bucket count the large regime allocates.
    ///
    /// We chose it to be **greater than every small `cap`**. That is what makes `cap > SMALL` a
    /// sound regime test rather than a coincidence. It never actually binds. The smallest large
    /// table is built for `SMALL + 1` elements, and `buckets_for(SMALL + 1) >= 2 * SMALL` holds
    /// for every power-of-two `SMALL`, so no table is ever rounded up to reach it. It is a static
    /// guarantee, not a cost, and [`Self::allocate`] asserts as much.
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

    /// A table sized to hold `capacity` elements without growing or changing regime. It is large
    /// from the outset if that many elements could not be small, so a caller that knows the final
    /// size skips the transition entirely.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = Self::cap_for(capacity);
        if cap == 0 {
            Self::new()
        } else {
            Self::allocate(cap)
        }
    }

    /// The `cap` a table holding `n` elements needs. That is a slot count while `n` fits the
    /// small regime, and a bucket count once it does not.
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
    /// This is the whole of the dispatch: one compare against a constant, on a field every
    /// operation loads anyway. `cap == 0` counts as small, which is why an unallocated table
    /// needs no case of its own on any path.
    #[inline]
    pub fn is_large(&self) -> bool {
        self.cap as usize > SMALL
    }

    #[inline]
    fn cap(&self) -> usize {
        self.cap as usize
    }

    /// `cap - 1`, which is the index mask in either regime, because `cap` is a power of two. For
    /// an unallocated table this wraps to `usize::MAX`, which is harmless: that table's probe
    /// loops run zero times.
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

    /// How many elements this table can hold before it grows or changes regime. The small
    /// regime's load factor may reach 1.0, so for it the answer is simply its slot count.
    #[inline]
    pub fn capacity(&self) -> usize {
        if self.is_large() {
            capacity_for(self.cap())
        } else {
            self.cap()
        }
    }

    /// How many elements can still be added without growing.
    #[inline]
    fn growth_left(&self) -> usize {
        self.capacity() - self.len()
    }

    // -----------------------------------------------------------------------
    // Layout: one block, one function, both regimes.
    // -----------------------------------------------------------------------

    /// How many metadata bytes a table of `cap` buckets needs. That is one occupancy word when
    /// small, and one control byte per bucket plus the group mirror when large.
    #[inline]
    const fn meta_len(cap: usize) -> usize {
        if cap > SMALL {
            ctrl_len(cap)
        } else {
            mem::size_of::<u64>()
        }
    }

    /// The allocation layout of a `cap`-bucket table of `T`, and the offset of the metadata
    /// within it. The block holds the element array, padded up to the metadata alignment, then
    /// the metadata.
    ///
    /// This is hashbrown's `calculate_layout_for`, generalized over what the metadata is.
    fn layout(cap: usize) -> (Layout, usize) {
        debug_assert!(cap > 0 && cap.is_power_of_two());
        let align = mem::align_of::<T>().max(META_ALIGN);
        let meta_offset = (mem::size_of::<T>() * cap).next_multiple_of(align);
        let bytes = meta_offset + Self::meta_len(cap);
        let layout = Layout::from_size_align(bytes, align).expect("table layout overflowed");
        (layout, meta_offset)
    }

    /// Heap bytes held, including whatever slack the regime carries. While the table is small
    /// that is the element slots plus the occupancy word. Once it is large it is the buckets,
    /// plus control bytes, plus load-factor slack.
    ///
    /// This is the allocation the table actually made, not an estimate. The enclosing map still
    /// needs [`super::super::locals_trie::hb_bytes`] to model hashbrown's layout, but this
    /// structure computes its own layout, so it can simply report it.
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
    // Element and metadata access. Both regimes share these.
    // -----------------------------------------------------------------------

    /// The address of bucket `index`'s element.
    ///
    /// The element array runs *backwards* from `ptr`, as hashbrown's does, so one pointer locates
    /// both arrays. That works in either regime, because both put their metadata at `ptr`.
    ///
    /// # Safety
    ///
    /// `index < self.cap()`, which implies the table is allocated. The returned pointer is
    /// initialized only when the bucket's metadata says so.
    #[inline]
    unsafe fn bucket(&self, index: usize) -> *mut T {
        debug_assert!(index < self.cap());
        // SAFETY: `meta_offset >= size_of::<T>() * cap`. So for `index < cap`, subtracting
        // `(index + 1) * size` stays at or above the start of the allocation.
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
        // SAFETY: `ptr` is `META_ALIGN`-aligned and addresses at least 8 initialized bytes.
        // `allocate` wrote them, or, for an unallocated table, they are the `EMPTY_META` static.
        unsafe { self.ptr.as_ptr().cast::<u64>().read() }
    }

    /// Overwrite the small regime's occupancy bitmask.
    ///
    /// # Safety
    ///
    /// The table must be small **and allocated**, because `EMPTY_META` is not writable, and
    /// `bits` must describe exactly the initialized slots.
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
    /// The table must be large, and `pos <= self.mask()`, so that the group stays inside the
    /// control array. The array's trailing [`GROUP_WIDTH`] mirror bytes exist for exactly this.
    #[inline]
    unsafe fn group_at(&self, pos: usize) -> Group {
        debug_assert!(self.is_large() && pos <= self.mask());
        // SAFETY: `pos + GROUP_WIDTH <= cap + GROUP_WIDTH`, the control array's length.
        unsafe { Group::load(self.ptr.as_ptr().add(pos)) }
    }

    /// Set bucket `index`'s control byte, keeping the mirror in sync.
    ///
    /// The mirrored index is hashbrown's branchless
    /// `((index - GROUP_WIDTH) & mask) + GROUP_WIDTH`. For `index >= GROUP_WIDTH` that is `index`
    /// itself; below it, it is `index + cap`. Those are exactly the bytes a wrapped group load
    /// can read.
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

    /// Mark bucket `index` full, having just written an element there. The small regime ignores
    /// `h2`, because it records occupancy positionally.
    ///
    /// # Safety
    ///
    /// `index < self.cap()`, the table is allocated, and bucket `index` now holds an initialized
    /// `T` that no other bucket claims. This does **not** bump `len`; the caller does.
    #[inline]
    unsafe fn mark_full(&mut self, index: usize, h2: u8) {
        if self.is_large() {
            // SAFETY: large, and `index < cap` by the caller's guarantee.
            unsafe { self.set_ctrl(index, h2) }
        } else {
            let bits = self.occupancy() | (1u64 << index);
            // SAFETY: small and allocated, by the caller's guarantee. The new bit is the slot the
            // caller just initialized.
            unsafe { self.set_occupancy(bits) }
        }
    }

    /// Mark bucket `index` empty, having just moved its element out, and drop `len` by one.
    ///
    /// In the large regime this writes [`EMPTY`] where a probe sequence may pass, so it is sound
    /// only for a table that will not be searched again. The one caller, [`IntoIter`], owns the
    /// table, so that holds.
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

    /// Bucket `index`'s control byte. Returns 0 in the small regime, which has no control bytes.
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

    /// The element equal to the one we are looking up, or `None`.
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

    /// Walk the small regime's probe sequence once, reporting where the element is, or where it
    /// would go. One pass serves both `contains` and `insert`.
    ///
    /// This probes linearly from `hash` and stops at the first empty slot. That is sound because
    /// elements are never removed, so no tombstone can hide a later match. The `for` bound does
    /// two more jobs: it terminates on a completely full table, reporting [`Probed::Full`], and
    /// it makes an unallocated table (`cap == 0`) report `Full` without a case of its own.
    #[inline]
    fn small_probe(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Probed {
        debug_assert!(!self.is_large());
        // One load covers the whole table. Every step after this is a register bit test.
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
            // A group with an empty bucket ends the probe sequence. With no tombstones, an
            // element that hashed here would have been placed at or before that bucket.
            if group.match_empty().any() {
                return None;
            }
            probe.move_next(mask);
        }
    }

    /// Where an element known to be absent belongs in the large regime. That is the first empty
    /// bucket in its probe sequence. This requires [`Self::growth_left`] > 0, which guarantees
    /// such a bucket exists.
    #[inline]
    fn find_insert_slot(&self, hash: u64) -> usize {
        debug_assert!(self.is_large() && self.growth_left() > 0);
        let mask = self.mask();
        let mut probe = ProbeSeq::new(hash, mask);
        loop {
            // SAFETY: `probe.pos` is always masked to `mask`, on a large table.
            let group = unsafe { self.group_at(probe.pos) };
            if let Some(bit) = group.match_empty().lowest() {
                // The masking is what makes a hit in the mirror bytes name the bucket it mirrors.
                return (probe.pos + bit) & mask;
            }
            probe.move_next(mask);
        }
    }

    // -----------------------------------------------------------------------
    // Insertion.
    // -----------------------------------------------------------------------

    /// Insert `value` if no equal element is present. Returns whether it was added.
    ///
    /// `hash` must be the hash of `value`, and `hash_of` must agree with it for every element
    /// already in the table.
    ///
    /// Unlike [`Self::find`], this compares with `T: Eq` rather than a caller-supplied closure. A
    /// closure would have to borrow the very value being moved in. hashbrown solves that with its
    /// `Entry` API. A set only ever needs equality on whole elements, so we ask for it directly
    /// instead.
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
        // Bind the probe result, so that the `eq` closure, which borrows `value`, is dropped
        // before we move `value` into the table.
        let probed = self.small_probe(hash, |x| *x == value);
        match probed {
            Probed::Found(_) => false,
            Probed::Vacant(i) => {
                // SAFETY: `Vacant` names an empty slot of an allocated table, because a table
                // with `cap == 0` probes as `Full`. So writing there overwrites nothing.
                unsafe {
                    self.bucket(i).write(value);
                    self.mark_full(i, 0);
                }
                self.len += 1;
                true
            }
            // The element is absent and there is nowhere to put it, so we either grow or change
            // regime. Both are one call to `rebuild`, sized so that the element we are about to
            // add fits without a second growth.
            Probed::Full => {
                self.rebuild(Self::cap_for(self.len() + 1), hash_of);
                // SAFETY: `rebuild` sized the table for one more element, and the element is
                // absent, because the probe that reported `Full` searched every slot.
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

        // The element is absent, and `slot` is where it goes. Unless there is no room: then we
        // rebuild the table and recompute the slot.
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
    /// [`Self::growth_left`] > 0. In the small regime that last condition means an empty slot
    /// exists.
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
        // nothing. The metadata marks it full afterwards.
        unsafe {
            self.bucket(index).write(value);
            self.mark_full(index, h2(hash));
        }
        self.len += 1;
    }

    /// Make room for `additional` more elements. If that many more could not be held small, this
    /// changes regime up front.
    ///
    /// Do not use this if you only have an *upper bound* on the additional count, as you would
    /// for a union whose overlap is unknown. Over-reserving here permanently promotes a table
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
    /// This is growth, promotion, and reservation all at once. Each of the old and new tables
    /// gets its regime from its own `cap`, so a small-to-small doubling, a small-to-large
    /// promotion, and a large-to-large doubling are the same three lines. Nothing distinguishes
    /// promotion except the number passed in.
    ///
    /// hashbrown can sometimes rehash *in place*, reusing the allocation by shuffling elements
    /// into the buckets a tombstone freed. Without removals there are no such buckets and no such
    /// case. A rebuild here is always a fresh allocation, one linear pass, and a free.
    fn rebuild(&mut self, new_cap: usize, hash_of: impl Fn(&T) -> u64) {
        debug_assert!(new_cap > self.len());
        let mut fresh = Self::allocate(new_cap);
        // Snapshot the element positions, then disown them *before* the loop. The iterator has
        // already recorded how many to visit. Disowning up front means that a panic in `hash_of`
        // leaks the elements we have not moved yet, rather than dropping elements the loop has
        // already moved into `fresh`.
        let indices = self.raw_iter();
        let moved = self.len;
        self.len = 0;
        for index in indices {
            // SAFETY: `raw_iter` yields full buckets, each exactly once, so this reads each
            // element once. And `self` no longer claims to own any of them.
            let value = unsafe { self.bucket(index).read() };
            let hash = hash_of(&value);
            // SAFETY: the elements come from a set, so they are distinct, and `fresh` was sized
            // for all of them, so it never runs out of room.
            unsafe { fresh.insert_unique_no_grow(hash, value) };
        }
        debug_assert_eq!(fresh.len, moved);
        // Dropping the old table now frees its allocation. It drops no elements, because it owns
        // none.
        *self = fresh;
    }

    // -----------------------------------------------------------------------
    // Iteration.
    // -----------------------------------------------------------------------

    /// The indices of the full buckets, in bucket order.
    ///
    /// This is the one iterator both regimes use. For a small table it loads the whole occupancy
    /// word here and never refills it. For a large table it loads the first control group here
    /// and refills a group at a time. See [`RawIter`] for how that is one code path.
    #[inline]
    fn raw_iter(&self) -> RawIter {
        let (bits, shift) = if self.is_large() {
            // SAFETY: an allocated large table has at least `MIN_BUCKETS == GROUP_WIDTH` control
            // bytes.
            let group = unsafe { Group::load(self.ptr.as_ptr()) };
            (group.match_full().0, BitMask::SHIFT)
        } else {
            // This is `>=`, not `==`, because `rebuild` deliberately zeroes `len` while the bits
            // still stand, so that a panic mid-move leaks rather than double-drops. Everywhere
            // else the two agree.
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

    /// Drop every element and release the allocation, leaving the table unallocated.
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
            // SAFETY: `allocate` placed `ptr` at `base + meta_offset` with this exact layout, and
            // nothing else frees it, because we leave `self` unallocated below.
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
    /// Copies the metadata element by element, and clones each element into the *same* bucket. So
    /// the copy costs no hashing at all, and it preserves every probe sequence. That holds in
    /// either regime, because a slot's position is all the small regime's metadata says about it.
    fn clone(&self) -> Self {
        if self.cap == 0 {
            return Self::new();
        }
        let mut fresh = Self::allocate(self.cap());
        for index in self.raw_iter() {
            // SAFETY: `index` is a full bucket of `self`, and an empty one of `fresh`. We just
            // allocated `fresh` with the same `cap`, so the index is in range.
            unsafe {
                fresh.bucket(index).write((*self.bucket(index)).clone());
                // Keep the metadata the source had. For a large table that is the element's own
                // `h2`, so the clone answers every probe identically.
                let byte = self.meta_byte(index);
                fresh.mark_full(index, byte);
            }
            // We bump this per element, so that a panicking `T::clone` leaves `fresh` consistent
            // and its destructor drops exactly what we wrote.
            fresh.len += 1;
        }
        fresh
    }
}

/// Iterator over the indices of a table's full buckets, in **either** regime.
///
/// In both regimes the metadata reads as "one bit per bucket, lowest bucket in the lowest bit".
/// They differ in only two ways: how far apart the bits are, and whether there is more metadata
/// to load after the first word.
///
/// | | bit spacing | words |
/// |-|-------------|-------|
/// | small | 1 bit per slot (`shift == 0`) | one, covering the whole table |
/// | large | 1 byte per bucket ([`BitMask::SHIFT`]) | one per [`GROUP_WIDTH`] buckets |
///
/// So the whole difference is a shift amount, plus a loop that a small table never enters.
/// `remaining` reaches zero first for a small table, because the one word it loaded described
/// every element. There is no tag, no branch per element, and one `next` for both.
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
    /// Elements not yet yielded. This is also what stops the large regime from walking off the
    /// control array.
    remaining: usize,
    /// Bucket count, used for a debug assertion only.
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
            // `remaining > 0`, so a full bucket exists in a later group, and this cannot walk off
            // the end. A small table never reaches here. Its single word described every element,
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

// Hand-written rather than derived. A shared-borrow iterator is copyable whatever `T` is, but
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
/// We mark each yielded element's bucket empty as the element leaves. So the table's own
/// destructor drops whatever the caller did not consume, exactly once.
pub struct IntoIter<T, const SMALL: usize> {
    table: RawTable<T, SMALL>,
    indices: RawIter,
}

impl<T, const SMALL: usize> Iterator for IntoIter<T, SMALL> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        let index = self.indices.next()?;
        // SAFETY: `index` is a full bucket. Clearing its metadata first means the table's
        // destructor will not touch the element we read out. Clearing cannot break a later
        // lookup, because `IntoIter` owns the table and nothing can look it up again.
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
