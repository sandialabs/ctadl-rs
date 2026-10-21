//! A disk-backed LSM multi-map whose **reads return references straight into
//! memory-mapped run files** — no decoding into owned `Vec`s.
//!
//! This is the zero-copy counterpart to [`crate::DiskLsmMultiMap`]. It is
//! constrained to fixed-width plain-old-data keys and values (`bytemuck::Pod`)
//! so that the bytes on disk can be reinterpreted in place as `&[K]` / `&[V]`.
//!
//! Runs are stored in a columnar layout — a sorted `keys[]` array, a prefix-sum
//! `offsets[]` array, and a flat `values[]` array — each section aligned so that
//! `bytemuck::cast_slice` can view it without copying. A run is `mmap`ed once and
//! the mapping is held inside the run, so references handed out by `get_refs` /
//! `iter_all_refs` stay valid for as long as the map is borrowed.
//!
//! On-disk bytes are written in **native** endianness (the `mmap` is reinterpreted
//! directly), so a run file is only portable to machines of the same endianness.

use std::fs;
use std::io;
use std::iter::Peekable;
use std::marker::PhantomData;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use bytemuck::Pod;
use hashbrown::HashTable;
use memmap2::{Mmap, MmapMut};
use smallvec::SmallVec;

use crate::bloom::{self, Block, hash_bytes, hash_key};

const DEFAULT_LEVEL_FANOUT: usize = 4;
const RUN_MAGIC: &[u8; 4] = b"LSMM";
const RUN_VERSION: u32 = 2;
/// The most key prefix lengths a map can keep filters for.
const MAX_PREFIX_FILTERS: usize = 4;
/// Fixed run header: magic(4) + version(4) + entries_count(8) + values_len(8) +
/// keys_off(8) + offsets_off(8) + values_off(8) + filter_off(8) +
/// filter_blocks(8) + prefix filter count(8) + per prefix filter, its length and
/// offset (8 + 8).
const HEADER_LEN: usize = 4 + 4 + 8 * 8 + 16 * MAX_PREFIX_FILTERS;
const DEFAULT_DISK_MEMTABLE_LIMIT: usize = 10_000;
static NEXT_TEMP_RUN_ID: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// The memory budget, shared by every map in the process.
// ---------------------------------------------------------------------------

/// The heap all maps' write buffers may hold together, in bytes.
static MEMORY_BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);
/// The heap all maps' write buffers hold now, and the most they have held.
static MEMORY_IN_USE: AtomicUsize = AtomicUsize::new(0);
static MEMORY_PEAK: AtomicUsize = AtomicUsize::new(0);
/// Under the budget, a map writes its buffer out only once it holds this much,
/// so a buffer that gets an insert while others hold the budget does not write
/// a run per insert.
const MIN_BUDGET_FLUSH_BYTES: usize = 64 * 1024;

/// Caps the heap that the write buffers of all maps in the process hold
/// together, in bytes; `None` lifts the cap. Over the cap, a map that takes an
/// insert (or absorbs another map) writes its buffer out as a run. The rest of
/// a map is on disk or in page cache: its runs, their Bloom filters, and its
/// per-run bookkeeping of a few dozen bytes. Each map's own entry limit still
/// applies under the cap.
pub fn set_memory_budget(bytes: Option<usize>) {
    MEMORY_BUDGET.store(bytes.unwrap_or(usize::MAX), Ordering::Relaxed);
}

/// The cap [`set_memory_budget`] set, if any.
pub fn memory_budget() -> Option<usize> {
    let budget = MEMORY_BUDGET.load(Ordering::Relaxed);
    (budget != usize::MAX).then_some(budget)
}

/// The heap all maps' write buffers hold now, in bytes (an estimate, from their
/// entries, hash tables and prefix chains).
pub fn memory_in_use() -> usize {
    MEMORY_IN_USE.load(Ordering::Relaxed)
}

/// The most [`memory_in_use`] has been.
pub fn memory_peak() -> usize {
    MEMORY_PEAK.load(Ordering::Relaxed)
}

/// Borrowing value iterator over a list of slices, one per source (the memtable
/// plus each run that holds the key). The list is held inline up to a few
/// sources, so a lookup does not allocate. Cheap to clone.
#[derive(Clone)]
pub struct ValsRefs<'a, V> {
    slices: SmallVec<[&'a [V]; 4]>,
    /// The next slice to start once `current` runs out.
    next: usize,
    current: std::slice::Iter<'a, V>,
}

impl<'a, V> ValsRefs<'a, V> {
    fn new(slices: SmallVec<[&'a [V]; 4]>) -> Self {
        Self {
            slices,
            next: 0,
            current: [].iter(),
        }
    }
}

impl<'a, V> Iterator for ValsRefs<'a, V> {
    type Item = &'a V;

    #[inline]
    fn next(&mut self) -> Option<&'a V> {
        loop {
            if let Some(v) = self.current.next() {
                return Some(v);
            }
            let slice = self.slices.get(self.next)?;
            self.next += 1;
            self.current = slice.iter();
        }
    }
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// A single immutable run, memory-mapped. Holds no decoded keys or values — only
/// the mapping plus the section offsets parsed from the header.
#[derive(Debug)]
struct MmapRun<K, V> {
    bytes: Mmap,
    /// The run's file, for a persistent map. A temporary run's file is unlinked
    /// as soon as it is mapped, so it needs no cleanup.
    path: Option<PathBuf>,
    entries_count: usize,
    values_len: usize,
    keys_off: usize,
    offsets_off: usize,
    values_off: usize,
    /// Where the Bloom filter of the run's keys starts, for skipping the run on
    /// a lookup of a key it lacks. The filters are in the run's own bytes, so
    /// they cost page cache rather than heap.
    filter_off: usize,
    /// The number of blocks in each of the run's filters.
    filter_blocks: usize,
    /// For each key prefix length (in bytes) the map was asked to filter, where
    /// the filter of the run's key prefixes of that length starts.
    prefix_filters: SmallVec<[(usize, usize); 2]>,
    _marker: PhantomData<(K, V)>,
}

impl<K: Pod + Ord, V: Pod> MmapRun<K, V> {
    fn keys(&self) -> &[K] {
        let bytes = &self.bytes[self.keys_off..self.keys_off + self.entries_count * size_of::<K>()];
        bytemuck::cast_slice(bytes)
    }

    fn offsets(&self) -> &[u64] {
        let bytes = &self.bytes
            [self.offsets_off..self.offsets_off + (self.entries_count + 1) * size_of::<u64>()];
        bytemuck::cast_slice(bytes)
    }

    fn values(&self) -> &[V] {
        if size_of::<V>() == 0 {
            // A set's values are zero-sized, which `cast_slice` refuses to make.
            // SAFETY: any non-null aligned pointer is a valid slice of zero-sized
            // values, of any length.
            return unsafe {
                std::slice::from_raw_parts(std::ptr::NonNull::dangling().as_ptr(), self.values_len)
            };
        }
        let bytes =
            &self.bytes[self.values_off..self.values_off + self.values_len * size_of::<V>()];
        bytemuck::cast_slice(bytes)
    }

    fn filter(&self, off: usize) -> &[Block] {
        bytemuck::cast_slice(&self.bytes[off..off + self.filter_blocks * size_of::<Block>()])
    }

    fn contains_key(&self, key: &K, hash: u64) -> bool {
        bloom::may_contain(self.filter(self.filter_off), hash)
            && self.keys().binary_search(key).is_ok()
    }

    /// False only if no key starts with a prefix of `len` bytes hashing to `hash`.
    fn may_have_prefix(&self, len: usize, hash: u64) -> bool {
        self.prefix_filters
            .iter()
            .find(|(l, _)| *l == len)
            .is_none_or(|&(_, off)| bloom::may_contain(self.filter(off), hash))
    }

    /// Values for `key`, as a zero-copy slice into the mapping, or `None`.
    /// `hash` is the key's [`hash_key`].
    fn find(&self, key: &K, hash: u64) -> Option<&[V]> {
        if !bloom::may_contain(self.filter(self.filter_off), hash) {
            return None;
        }
        let keys = self.keys();
        match keys.binary_search(key) {
            Ok(i) => {
                let offsets = self.offsets();
                Some(&self.values()[offsets[i] as usize..offsets[i + 1] as usize])
            }
            Err(_) => None,
        }
    }

    /// Deletes the run's file, if it still has one.
    fn delete(self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }

    /// The `i`th entry as `(&key, &values)`, both borrowing the mapping.
    fn entry(&self, i: usize) -> (&K, &[V]) {
        let offsets = self.offsets();
        (
            &self.keys()[i],
            &self.values()[offsets[i] as usize..offsets[i + 1] as usize],
        )
    }
}

/// Writes a run straight into its final file through a writable mapping. The
/// caller knows the run's shape up front (its value count, and at most how many
/// entries), so the layout is fixed before the first byte and every section is
/// written in place: no intermediate buffers or scratch files, and no write
/// syscalls. Entries short of the bound leave the tail of the key and offset
/// sections unwritten, which costs address space but no disk.
struct RunWriter {
    bytes: MmapMut,
    path: PathBuf,
    keep_file: bool,
    max_entries: usize,
    values_len: usize,
    keys_off: usize,
    offsets_off: usize,
    values_off: usize,
    /// Entries and values written so far.
    keys_written: usize,
    values_written: usize,
    filter_off: usize,
    filter_blocks: usize,
    prefix_filters: SmallVec<[(usize, usize); 2]>,
}

impl RunWriter {
    /// Creates the run file at `path`, sized for at most `max_entries` keys and
    /// exactly `values_len` values of the given widths, and maps it for writing.
    /// A temporary run (`keep_file == false`) is unlinked once it is mapped for
    /// reading, so it disappears with its last mapping.
    fn create<K: Pod, V: Pod>(
        path: PathBuf,
        keep_file: bool,
        max_entries: usize,
        values_len: usize,
        prefix_lens: &[usize],
    ) -> io::Result<Self> {
        let keys_off = align8(HEADER_LEN);
        let offsets_off = align8(keys_off + max_entries * size_of::<K>());
        let values_off = align8(offsets_off + (max_entries + 1) * size_of::<u64>());
        let filter_off = align8(values_off + values_len * size_of::<V>());
        let filter_blocks = bloom::blocks_for(max_entries);
        let filter_bytes = filter_blocks * size_of::<Block>();
        let prefix_filters: SmallVec<[(usize, usize); 2]> = prefix_lens
            .iter()
            .enumerate()
            .map(|(i, &len)| (len, filter_off + (i + 1) * filter_bytes))
            .collect();
        let file_len = filter_off + (1 + prefix_filters.len()) * filter_bytes;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(file_len as u64)?;
        // SAFETY: the file was just created by us and nothing else maps or writes
        // it while this mapping lives.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            bytes: mmap,
            path,
            keep_file,
            max_entries,
            values_len,
            keys_off,
            offsets_off,
            values_off,
            keys_written: 0,
            values_written: 0,
            filter_off,
            filter_blocks,
            prefix_filters,
        })
    }

    fn write_offset(&mut self, i: usize, offset: usize) {
        let at = self.offsets_off + i * size_of::<u64>();
        self.bytes[at..at + size_of::<u64>()].copy_from_slice(&(offset as u64).to_ne_bytes());
    }

    /// Begins a new entry. Must be called in ascending key order, then followed
    /// by zero or more `append_values` calls for that entry.
    fn start_key<K: Pod>(&mut self, key: &K) {
        assert!(self.keys_written < self.max_entries, "run overflow: keys");
        let at = self.keys_off + self.keys_written * size_of::<K>();
        self.bytes[at..at + size_of::<K>()].copy_from_slice(bytemuck::bytes_of(key));
        let filter_bytes = self.filter_blocks * size_of::<Block>();
        let off = self.filter_off;
        bloom::insert(
            bytemuck::cast_slice_mut(&mut self.bytes[off..off + filter_bytes]),
            hash_key(key),
        );
        for &(len, off) in &self.prefix_filters {
            bloom::insert(
                bytemuck::cast_slice_mut(&mut self.bytes[off..off + filter_bytes]),
                hash_bytes(&bytemuck::bytes_of(key)[..len]),
            );
        }
        self.write_offset(self.keys_written, self.values_written);
        self.keys_written += 1;
    }

    /// Appends values to the current entry. May be called several times to
    /// concatenate slices coming from different source runs.
    fn append_values<V: Pod>(&mut self, vals: &[V]) {
        assert!(
            self.values_written + vals.len() <= self.values_len,
            "run overflow: values"
        );
        let bytes: &[u8] = bytemuck::cast_slice(vals);
        let at = self.values_off + self.values_written * size_of::<V>();
        self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        self.values_written += vals.len();
    }

    fn finish<K: Pod + Ord, V: Pod>(mut self) -> io::Result<MmapRun<K, V>> {
        assert_eq!(
            self.values_written, self.values_len,
            "run underflow: values"
        );
        let entries_count = self.keys_written;
        // Closing sentinel offset (== total values).
        self.write_offset(entries_count, self.values_len);
        let header = &mut self.bytes[..HEADER_LEN];
        header[0..4].copy_from_slice(RUN_MAGIC);
        header[4..8].copy_from_slice(&RUN_VERSION.to_ne_bytes());
        header[8..16].copy_from_slice(&(entries_count as u64).to_ne_bytes());
        header[16..24].copy_from_slice(&(self.values_len as u64).to_ne_bytes());
        header[24..32].copy_from_slice(&(self.keys_off as u64).to_ne_bytes());
        header[32..40].copy_from_slice(&(self.offsets_off as u64).to_ne_bytes());
        header[40..48].copy_from_slice(&(self.values_off as u64).to_ne_bytes());
        header[48..56].copy_from_slice(&(self.filter_off as u64).to_ne_bytes());
        header[56..64].copy_from_slice(&(self.filter_blocks as u64).to_ne_bytes());
        header[64..72].copy_from_slice(&(self.prefix_filters.len() as u64).to_ne_bytes());
        for (i, &(len, off)) in self.prefix_filters.iter().enumerate() {
            let at = 72 + 16 * i;
            header[at..at + 8].copy_from_slice(&(len as u64).to_ne_bytes());
            header[at + 8..at + 16].copy_from_slice(&(off as u64).to_ne_bytes());
        }
        let mmap = self.bytes.make_read_only()?;
        let path = if self.keep_file {
            Some(self.path)
        } else {
            fs::remove_file(&self.path)?;
            None
        };
        Ok(MmapRun {
            bytes: mmap,
            path,
            entries_count,
            values_len: self.values_len,
            keys_off: self.keys_off,
            offsets_off: self.offsets_off,
            values_off: self.values_off,
            filter_off: self.filter_off,
            filter_blocks: self.filter_blocks,
            prefix_filters: self.prefix_filters,
            _marker: PhantomData,
        })
    }
}

/// Maps an existing run file and parses its header.
fn load_run<K: Pod + Ord, V: Pod>(path: PathBuf) -> io::Result<MmapRun<K, V>> {
    let file = fs::File::open(&path)?;
    // SAFETY: run files are immutable once written and are never modified while
    // mapped, so the mapping's bytes do not change under us.
    let mmap = unsafe { Mmap::map(&file)? };
    if mmap.len() < HEADER_LEN
        || &mmap[0..4] != RUN_MAGIC
        || mmap[4..8] != RUN_VERSION.to_ne_bytes()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid mmap LSM run magic",
        ));
    }
    let read = |range: std::ops::Range<usize>| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&mmap[range]);
        u64::from_ne_bytes(b) as usize
    };
    let entries_count = read(8..16);
    let values_len = read(16..24);
    let keys_off = read(24..32);
    let offsets_off = read(32..40);
    let values_off = read(40..48);
    let filter_off = read(48..56);
    let filter_blocks = read(56..64);
    let prefix_filters = (0..read(64..72).min(MAX_PREFIX_FILTERS))
        .map(|i| {
            (
                read(72 + 16 * i..80 + 16 * i),
                read(80 + 16 * i..88 + 16 * i),
            )
        })
        .collect();
    Ok(MmapRun {
        bytes: mmap,
        path: Some(path),
        entries_count,
        values_len,
        keys_off,
        offsets_off,
        values_off,
        filter_off,
        filter_blocks,
        prefix_filters,
        _marker: PhantomData,
    })
}

/// Bytes per chunk of a [`Memtable`]'s entries.
const MEMTABLE_CHUNK_BYTES: usize = 16 * 1024;

type MemtableEntry<K, V> = (K, SmallVec<[V; 1]>);

/// The in-memory write buffer: each key's values, hashed by [`hash_key`] (the
/// same hash the runs' Bloom filters take, so a lookup hashes its key once).
///
/// The entries sit in insertion order in fixed-size chunks, and the hash table
/// holds only their positions. Small chunks rather than one growing `Vec`
/// because of how the allocator returns memory: freed small blocks go back as
/// reclaimable pages, where a large block, freed or merely emptied, stays
/// resident, so a memtable that has been flushed would still hold its peak.
/// A table's bucket array is touched all over as soon as it fills a little;
/// keeping its buckets at four bytes keeps that cost small next to the entries.
///
/// For each prefix length a [`MmapLsmMultiMap::keys_with_prefix`] read may ask
/// for, a [`PrefixChains`] links the entries that share a prefix, so the read
/// finds them without the memtable ever being sorted.
struct Memtable<K, V> {
    index: HashTable<u32>,
    chunks: Vec<Vec<MemtableEntry<K, V>>>,
    len: usize,
    prefixes: SmallVec<[PrefixChains; 2]>,
}

/// The entries of a [`Memtable`] that share a key prefix of `len` bytes, as a
/// chain through `next` from the head the table holds for that prefix.
struct PrefixChains {
    len: usize,
    heads: HashTable<u32>,
    /// Per entry, the next older entry with the same prefix, or [`NO_ENTRY`].
    next: Vec<u32>,
}

const NO_ENTRY: u32 = u32::MAX;

impl<K: std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for Memtable<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.chunks.iter().flatten().map(|(k, v)| (k, v)))
            .finish()
    }
}

impl<K: Pod + Ord, V: Pod> Memtable<K, V> {
    const CHUNK: usize = {
        let n = MEMTABLE_CHUNK_BYTES / size_of::<MemtableEntry<K, V>>();
        if n == 0 { 1 } else { n }
    };

    /// An empty memtable that chains its entries by prefixes of each of
    /// `prefix_lens` bytes.
    fn new(prefix_lens: &[usize]) -> Self {
        Self {
            index: HashTable::new(),
            chunks: Vec::new(),
            len: 0,
            prefixes: prefix_lens
                .iter()
                .map(|&len| PrefixChains {
                    len,
                    heads: HashTable::new(),
                    next: Vec::new(),
                })
                .collect(),
        }
    }

    /// Takes the entries out, leaving an empty memtable that chains the same
    /// prefixes.
    fn take(&mut self) -> Self {
        let lens: SmallVec<[usize; 2]> = self.prefixes.iter().map(|p| p.len).collect();
        std::mem::replace(self, Self::new(&lens))
    }

    fn entry(&self, i: u32) -> &MemtableEntry<K, V> {
        let i = i as usize;
        &self.chunks[i / Self::CHUNK][i % Self::CHUNK]
    }

    fn find(&self, hash: u64, key: &K) -> Option<u32> {
        self.index.find(hash, |&i| self.entry(i).0 == *key).copied()
    }

    fn insert(&mut self, hash: u64, key: K, value: V) {
        if let Some(i) = self.find(hash, &key) {
            let i = i as usize;
            self.chunks[i / Self::CHUNK][i % Self::CHUNK].1.push(value);
            return;
        }
        let i = u32::try_from(self.len).expect("memtable over u32::MAX keys");
        assert!(i != NO_ENTRY, "memtable over u32::MAX keys");
        if self.len.is_multiple_of(Self::CHUNK) {
            self.chunks.push(Vec::with_capacity(Self::CHUNK));
        }
        self.chunks
            .last_mut()
            .unwrap()
            .push((key, smallvec::smallvec![value]));
        self.len += 1;
        let chunks = &self.chunks;
        let key_bytes = |j: u32| {
            let j = j as usize;
            bytemuck::bytes_of(&chunks[j / Self::CHUNK][j % Self::CHUNK].0)
        };
        self.index
            .insert_unique(hash, i, |&j| hash_bytes(key_bytes(j)));
        let bytes = bytemuck::bytes_of(&key);
        for chains in &mut self.prefixes {
            let len = chains.len;
            let prefix = &bytes[..len];
            match chains.heads.entry(
                hash_bytes(prefix),
                |&j| &key_bytes(j)[..len] == prefix,
                |&j| hash_bytes(&key_bytes(j)[..len]),
            ) {
                hashbrown::hash_table::Entry::Occupied(mut head) => {
                    chains.next.push(*head.get());
                    *head.get_mut() = i;
                }
                hashbrown::hash_table::Entry::Vacant(head) => {
                    chains.next.push(NO_ENTRY);
                    head.insert(i);
                }
            }
        }
    }

    fn get(&self, hash: u64, key: &K) -> Option<&[V]> {
        self.find(hash, key).map(|i| self.entry(i).1.as_slice())
    }

    fn keys_len(&self) -> usize {
        self.len
    }

    /// The heap the memtable holds for its keys (its values past each key's
    /// first are the caller's to count): chunks, hash table and prefix chains.
    fn key_bytes(&self) -> usize {
        // A table's buckets are a four-byte position plus a control byte each.
        let table = |t: &HashTable<u32>| t.capacity() * (size_of::<u32>() + 1);
        self.chunks.len() * Self::CHUNK * size_of::<MemtableEntry<K, V>>()
            + table(&self.index)
            + self
                .prefixes
                .iter()
                .map(|c| table(&c.heads) + c.next.capacity() * size_of::<u32>())
                .sum::<usize>()
    }

    /// Every entry, by ascending key.
    fn sorted(&self) -> Vec<(&K, &[V])> {
        let mut entries: Vec<_> = self
            .chunks
            .iter()
            .flatten()
            .map(|(k, vals)| (k, vals.as_slice()))
            .collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        entries
    }

    /// The entries whose keys start with `prefix` (hashing to `hash`), if this
    /// memtable chains prefixes of its length.
    fn prefix_chain(&self, prefix: &[u8], hash: u64) -> Option<(&[u32], u32)> {
        let chains = self.prefixes.iter().find(|c| c.len == prefix.len())?;
        let head = chains
            .heads
            .find(hash, |&j| {
                &bytemuck::bytes_of(&self.entry(j).0)[..prefix.len()] == prefix
            })
            .copied()
            .unwrap_or(NO_ENTRY);
        Some((&chains.next, head))
    }
}

/// A disk-backed LSM multi-map with zero-copy, reference-returning reads.
#[derive(Debug)]
pub struct MmapLsmMultiMap<K, V> {
    memtable: Memtable<K, V>,
    levels: Vec<Vec<MmapRun<K, V>>>,
    memtable_len: usize,
    len: usize,
    memtable_limit: usize,
    level_fanout: usize,
    /// Where a persistent map (see [`Self::open`]) keeps its run files. A
    /// temporary map has none: its runs live in unlinked files under the
    /// temporary directory.
    dir: Option<PathBuf>,
    next_run_id: u64,
    /// The key prefix lengths, in bytes, whose prefixes each run filters for
    /// [`Self::keys_with_prefix`].
    prefix_lens: SmallVec<[usize; 2]>,
    /// This map's share of [`memory_in_use`].
    memory: usize,
}

impl<K, V> Drop for MmapLsmMultiMap<K, V> {
    fn drop(&mut self) {
        MEMORY_IN_USE.fetch_sub(self.memory, Ordering::Relaxed);
    }
}

impl<K: Pod + Ord, V: Pod> Default for MmapLsmMultiMap<K, V> {
    fn default() -> Self {
        Self::with_memtable_limit_and_level_fanout(
            DEFAULT_DISK_MEMTABLE_LIMIT,
            DEFAULT_LEVEL_FANOUT,
        )
    }
}

impl<K: Pod + Ord, V: Pod> MmapLsmMultiMap<K, V> {
    /// Creates a temporary zero-copy LSM, whose runs are unlinked files under
    /// [`std::env::temp_dir`].
    pub fn with_memtable_limit(memtable_limit: usize) -> Self {
        Self::with_memtable_limit_and_level_fanout(memtable_limit, DEFAULT_LEVEL_FANOUT)
    }

    pub fn with_memtable_limit_and_level_fanout(
        memtable_limit: usize,
        level_fanout: usize,
    ) -> Self {
        Self {
            memtable: Memtable::new(&[]),
            levels: Vec::new(),
            memtable_len: 0,
            len: 0,
            memtable_limit: memtable_limit.max(1),
            level_fanout: level_fanout.max(1),
            dir: None,
            next_run_id: 0,
            prefix_lens: SmallVec::new(),
            memory: 0,
        }
    }

    /// Prepares the map for [`Self::keys_with_prefix`] reads by prefixes of each
    /// of `lens` bytes: the memtable links its entries by those prefixes, and
    /// every run keeps a Bloom filter of them, so a read finds the memtable's
    /// matches without a scan and skips the runs that hold none. Reads by other
    /// lengths still work, by scanning the memtable.
    pub fn with_prefix_reads(mut self, lens: &[usize]) -> Self {
        assert!(self.is_empty(), "with_prefix_reads on a non-empty map");
        assert!(lens.len() <= MAX_PREFIX_FILTERS, "too many prefix lengths");
        self.prefix_lens = lens.iter().copied().collect();
        self.memtable = Memtable::new(lens);
        self
    }

    /// Opens or creates a zero-copy LSM in `dir`, mapping any existing run files.
    pub fn open(dir: impl Into<PathBuf>, memtable_limit: usize) -> io::Result<Self> {
        Self::open_with_level_fanout(dir, memtable_limit, DEFAULT_LEVEL_FANOUT)
    }

    pub fn open_with_level_fanout(
        dir: impl Into<PathBuf>,
        memtable_limit: usize,
        level_fanout: usize,
    ) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;

        let mut levels: Vec<Vec<MmapRun<K, V>>> = Vec::new();
        let mut len = 0;
        let mut next_run_id = 0;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("lsm") {
                continue;
            }
            let Some((level, id)) = parse_run_file_name(&path) else {
                continue;
            };
            let run = load_run::<K, V>(path)?;
            len += run.values_len;
            next_run_id = next_run_id.max(id + 1);
            if levels.len() <= level {
                levels.resize_with(level + 1, Vec::new);
            }
            levels[level].push(run);
        }

        Ok(Self {
            memtable: Memtable::new(&[]),
            levels,
            memtable_len: 0,
            len,
            memtable_limit: memtable_limit.max(1),
            level_fanout: level_fanout.max(1),
            dir: Some(dir),
            next_run_id,
            prefix_lens: SmallVec::new(),
            memory: 0,
        })
    }

    pub fn insert(&mut self, key: K, value: V) {
        self.memtable.insert(hash_key(&key), key, value);
        self.memtable_len += 1;
        self.len += 1;
        self.count_memory();
        if self.memtable_len >= self.memtable_limit {
            self.flush().expect("failed to flush mmap LSM memtable");
        } else {
            self.keep_to_budget();
        }
    }

    /// Brings this map's share of [`memory_in_use`] up to date with its memtable.
    fn count_memory(&mut self) {
        let now = self.memtable.key_bytes()
            + (self.memtable_len - self.memtable.keys_len()) * size_of::<V>();
        if now >= self.memory {
            let total =
                MEMORY_IN_USE.fetch_add(now - self.memory, Ordering::Relaxed) + now - self.memory;
            MEMORY_PEAK.fetch_max(total, Ordering::Relaxed);
        } else {
            MEMORY_IN_USE.fetch_sub(self.memory - now, Ordering::Relaxed);
        }
        self.memory = now;
    }

    /// Writes the memtable out if all maps' memtables are over the budget and
    /// this one holds enough to be worth a run.
    fn keep_to_budget(&mut self) {
        if self.memory >= MIN_BUDGET_FLUSH_BYTES
            && MEMORY_IN_USE.load(Ordering::Relaxed) > MEMORY_BUDGET.load(Ordering::Relaxed)
        {
            self.flush().expect("failed to flush mmap LSM memtable");
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Values for `key` as a borrowing iterator that yields `&V` straight out of
    /// the memtable and the memory-mapped runs (no copying / decoding).
    pub fn get_refs(&self, key: &K) -> Option<ValsRefs<'_, V>> {
        let mut slices: SmallVec<[&[V]; 4]> = SmallVec::new();
        let hash = hash_key(key);
        if let Some(vals) = self.memtable.get(hash, key) {
            slices.push(vals);
        }
        for run in self.levels.iter().flatten() {
            if let Some(vals) = run.find(key, hash)
                && !vals.is_empty()
            {
                slices.push(vals);
            }
        }
        if slices.is_empty() {
            None
        } else {
            Some(ValsRefs::new(slices))
        }
    }

    /// Whether any value is stored under `key`.
    pub fn contains_key(&self, key: &K) -> bool {
        let hash = hash_key(key);
        if self.memtable.get(hash, key).is_some() {
            return true;
        }
        self.levels
            .iter()
            .flatten()
            .any(|run| run.contains_key(key, hash))
    }

    /// Membership test for a `(key, value)` pair across memtable and runs,
    /// allocation-free.
    pub fn contains_value(&self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        let hash = hash_key(key);
        if self
            .memtable
            .get(hash, key)
            .is_some_and(|vals| vals.contains(value))
        {
            return true;
        }
        self.levels
            .iter()
            .flatten()
            .any(|run| run.find(key, hash).is_some_and(|vals| vals.contains(value)))
    }

    /// Every key, from the memtable and every run, in no particular order. A key
    /// several runs hold comes once from each.
    pub fn keys(&self) -> KeysIter<'_, K, V> {
        KeysIter {
            memtable: &self.memtable,
            mem: MemKeys::All(0..self.memtable.len as u32),
            runs: self.levels.iter().flatten().map(|run| run.keys()).collect(),
            next_run: 0,
            current: [].iter(),
        }
    }

    /// The keys that begin with the words of `prefix`, from the memtable and
    /// every run, in no particular order. `K` must be made of `u64` words, whose
    /// lexicographic order is `K`'s own.
    pub fn keys_with_prefix(&self, prefix: &[u64]) -> KeysIter<'_, K, V> {
        let before = |k: &K| cmp_prefix(words(k), prefix).is_lt();
        let matches = |k: &K| cmp_prefix(words(k), prefix).is_eq();
        let in_range = |keys: &[K]| {
            let lo = keys.partition_point(before);
            let len = keys[lo..].partition_point(matches);
            lo..lo + len
        };
        let prefix_bytes: &[u8] = bytemuck::cast_slice(prefix);
        let hash = hash_bytes(prefix_bytes);
        let mem = match self.memtable.prefix_chain(prefix_bytes, hash) {
            Some((next, head)) => MemKeys::Chain { next, at: head },
            None => MemKeys::List(
                (0..self.memtable.len as u32)
                    .filter(|&i| matches(&self.memtable.entry(i).0))
                    .collect::<Vec<_>>()
                    .into(),
                0,
            ),
        };
        let mut runs: SmallVec<[&[K]; 4]> = SmallVec::new();
        for run in self.levels.iter().flatten() {
            if !run.may_have_prefix(prefix_bytes.len(), hash) {
                continue;
            }
            let keys = run.keys();
            let range = in_range(keys);
            if !range.is_empty() {
                runs.push(&keys[range]);
            }
        }
        KeysIter {
            memtable: &self.memtable,
            mem,
            runs,
            next_run: 0,
            current: [].iter(),
        }
    }

    /// Every key, grouped by its first `prefix_len` words: each group is one
    /// prefix's keys out of the memtable or out of one run. A prefix several of
    /// them hold comes as several groups, in no particular order; each comes with
    /// one of its keys, whose leading words are the prefix.
    pub fn prefix_groups(&self, prefix_len: usize) -> PrefixGroups<'_, K, V> {
        let bytes = prefix_len * size_of::<u64>();
        let chains = self.memtable.prefixes.iter().find(|c| c.len == bytes);
        let mem_heads: Vec<u32> = match chains {
            Some(chains) => chains.heads.iter().copied().collect(),
            // Without chains for this length, one group per memtable key.
            None => (0..self.memtable.len as u32).collect(),
        };
        PrefixGroups {
            map: self,
            prefix_len,
            mem_heads: mem_heads.into_iter(),
            mem_chains: chains.map(|chains| chains.next.as_slice()),
            runs: self.levels.iter().flatten().map(|run| run.keys()).collect(),
            next_run: 0,
            current: &[],
        }
    }

    /// Borrowing scan over every distinct key with all of its values, merged
    /// across the memtable and runs. Keys and values are references into the
    /// memtable / memory-mapped runs.
    pub fn iter_all_refs(&self) -> IterAllRefs<'_, K, V> {
        let mut sources: Vec<Peekable<Box<dyn Iterator<Item = (&K, &[V])> + '_>>> = Vec::new();
        let memtable: Box<dyn Iterator<Item = (&K, &[V])>> =
            Box::new(self.memtable.sorted().into_iter());
        sources.push(memtable.peekable());
        for run in self.levels.iter().flatten() {
            let run_iter: Box<dyn Iterator<Item = (&K, &[V])>> =
                Box::new((0..run.entries_count).map(move |i| run.entry(i)));
            sources.push(run_iter.peekable());
        }
        IterAllRefs { sources }
    }

    /// Empties the map (deleting any run files), keeping it usable.
    pub fn clear(&mut self) {
        self.memtable.take();
        for run in std::mem::take(&mut self.levels).into_iter().flatten() {
            run.delete();
        }
        self.memtable_len = 0;
        self.len = 0;
        self.count_memory();
    }

    /// Moves every entry of `other` into `self`, leaving `other` empty. A key
    /// both maps hold ends up with both maps' values.
    ///
    /// `other`'s runs move over as they are, with no copying. Its memtable is
    /// inserted into `self`'s when it is small next to `self`'s write buffer, and
    /// otherwise written out as a run of its own and moved like the others.
    pub fn absorb(&mut self, other: &mut Self) {
        if other.memtable_len >= self.memtable_limit / 4 {
            other.flush().expect("failed to flush mmap LSM memtable");
        }
        let memtable = other.memtable.take();
        let other_values = other.memtable_len;
        other.memtable_len = 0;
        other.count_memory();
        other.memtable_len = other_values;
        for (key, vals) in memtable.chunks.into_iter().flatten() {
            for value in vals {
                self.insert(key, value);
            }
        }
        // `insert` counted the memtable's entries already.
        self.len += other.len - other.memtable_len;
        for (level, runs) in std::mem::take(&mut other.levels).into_iter().enumerate() {
            for run in runs {
                self.push_disk_run(level, run)
                    .expect("failed to compact mmap LSM runs");
            }
        }
        other.memtable_len = 0;
        other.len = 0;
        self.keep_to_budget();
    }

    /// Persists the current memtable as a run.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.memtable_len == 0 {
            return Ok(());
        }
        let memtable = self.memtable.take();
        let values_len = self.memtable_len;
        self.memtable_len = 0;
        self.count_memory();
        let mut writer = self.new_run_writer(0, memtable.keys_len(), values_len)?;
        for (key, vals) in memtable.sorted() {
            writer.start_key(key);
            writer.append_values(vals);
        }
        let run = writer.finish()?;
        self.push_disk_run(0, run)
    }

    fn new_run_writer(
        &mut self,
        level: usize,
        entries_count: usize,
        values_len: usize,
    ) -> io::Result<RunWriter> {
        let (path, keep_file) = match &self.dir {
            Some(dir) => {
                let id = self.next_run_id;
                self.next_run_id += 1;
                (dir.join(format!("run_{level}_{id}.lsm")), true)
            }
            None => {
                let id = NEXT_TEMP_RUN_ID.fetch_add(1, Ordering::Relaxed);
                let name = format!("lsm-mmap-{}-{id}.run", std::process::id());
                (std::env::temp_dir().join(name), false)
            }
        };
        RunWriter::create::<K, V>(
            path,
            keep_file,
            entries_count,
            values_len,
            &self.prefix_lens,
        )
    }

    fn push_disk_run(&mut self, level: usize, run: MmapRun<K, V>) -> io::Result<()> {
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, Vec::new);
        }
        self.levels[level].push(run);
        if self.levels[level].len() > self.level_fanout {
            let runs = std::mem::take(&mut self.levels[level]);
            let merged = self.merge_runs(level + 1, runs)?;
            self.push_disk_run(level + 1, merged)?;
        }
        Ok(())
    }

    /// Streaming k-way merge of several runs into one. Each run is already sorted
    /// ascending by key, so we repeatedly pick the smallest head key across the
    /// runs and copy that key's values (concatenated from every run holding it)
    /// straight into the new run's mapping, sized for the case where no two runs
    /// share a key.
    fn merge_runs(&mut self, level: usize, runs: Vec<MmapRun<K, V>>) -> io::Result<MmapRun<K, V>> {
        let max_entries = runs.iter().map(|run| run.entries_count).sum();
        let values_len = runs.iter().map(|run| run.values_len).sum();
        let mut writer = self.new_run_writer(level, max_entries, values_len)?;
        let sections: Vec<_> = runs
            .iter()
            .map(|run| (run.keys(), run.offsets(), run.values()))
            .collect();
        let keys: Vec<&[K]> = sections.iter().map(|&(keys, _, _)| keys).collect();
        merge_keys(&keys, |key, holders| {
            writer.start_key(key);
            for &(r, i) in holders {
                let (_, offsets, values) = sections[r];
                writer.append_values(&values[offsets[i] as usize..offsets[i + 1] as usize]);
            }
        });
        let new_run = writer.finish()?;
        for run in runs {
            run.delete();
        }
        Ok(new_run)
    }
}

/// Visits the distinct keys of several sorted key lists in ascending order, each
/// with the `(list, index)` positions that hold it, in list order.
fn merge_keys<K: Ord>(lists: &[&[K]], mut visit: impl FnMut(&K, &[(usize, usize)])) {
    // Per-list cursor: index of the next unconsumed key.
    let mut cursors: Vec<usize> = vec![0; lists.len()];
    let mut holders: Vec<(usize, usize)> = Vec::with_capacity(lists.len());
    loop {
        // Smallest key currently at any list's cursor head.
        let mut min_key: Option<&K> = None;
        for (keys, &cursor) in lists.iter().zip(&cursors) {
            if let Some(key) = keys.get(cursor)
                && min_key.is_none_or(|current| key < current)
            {
                min_key = Some(key);
            }
        }
        let Some(min_key) = min_key else { break };
        holders.clear();
        for (r, (keys, cursor)) in lists.iter().zip(&mut cursors).enumerate() {
            if keys.get(*cursor) == Some(min_key) {
                holders.push((r, *cursor));
                *cursor += 1;
            }
        }
        visit(min_key, &holders);
    }
}

/// A key's `u64` words.
fn words<K: Pod>(key: &K) -> &[u64] {
    bytemuck::cast_slice(bytemuck::bytes_of(key))
}

/// How the leading words of `key` compare with `prefix`. A loop rather than a
/// slice comparison, which calls out to `memcmp` for these few words.
#[inline]
fn cmp_prefix(key: &[u64], prefix: &[u64]) -> std::cmp::Ordering {
    for (k, p) in key.iter().zip(prefix) {
        match k.cmp(p) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Keys out of the memtable and out of a list of run slices. Cheap to clone.
pub struct KeysIter<'a, K, V> {
    memtable: &'a Memtable<K, V>,
    mem: MemKeys<'a>,
    runs: SmallVec<[&'a [K]; 4]>,
    next_run: usize,
    current: std::slice::Iter<'a, K>,
}

/// Which of the memtable's entries a [`KeysIter`] yields.
#[derive(Clone)]
enum MemKeys<'a> {
    /// All of them.
    All(std::ops::Range<u32>),
    /// One prefix's, along its chain from `at`.
    Chain { next: &'a [u32], at: u32 },
    /// Those listed, from the `usize`th on.
    List(std::rc::Rc<[u32]>, usize),
}

impl MemKeys<'_> {
    fn next(&mut self) -> Option<u32> {
        match self {
            MemKeys::All(range) => range.next(),
            MemKeys::Chain { next, at } => {
                let i = *at;
                if i == NO_ENTRY {
                    return None;
                }
                *at = next[i as usize];
                Some(i)
            }
            MemKeys::List(list, at) => {
                let i = *list.get(*at)?;
                *at += 1;
                Some(i)
            }
        }
    }
}

impl<K, V> Clone for KeysIter<'_, K, V> {
    fn clone(&self) -> Self {
        Self {
            memtable: self.memtable,
            mem: self.mem.clone(),
            runs: self.runs.clone(),
            next_run: self.next_run,
            current: self.current.clone(),
        }
    }
}

impl<'a, K: Pod + Ord, V: Pod> Iterator for KeysIter<'a, K, V> {
    type Item = &'a K;

    #[inline]
    fn next(&mut self) -> Option<&'a K> {
        if let Some(i) = self.mem.next() {
            return Some(&self.memtable.entry(i).0);
        }
        loop {
            if let Some(key) = self.current.next() {
                return Some(key);
            }
            let keys = self.runs.get(self.next_run)?;
            self.next_run += 1;
            self.current = keys.iter();
        }
    }
}

/// The groups of [`MmapLsmMultiMap::prefix_groups`].
pub struct PrefixGroups<'a, K, V> {
    map: &'a MmapLsmMultiMap<K, V>,
    prefix_len: usize,
    mem_heads: std::vec::IntoIter<u32>,
    /// The `next` links of the chains the memtable heads start, or `None` when
    /// each head stands alone.
    mem_chains: Option<&'a [u32]>,
    runs: SmallVec<[&'a [K]; 4]>,
    next_run: usize,
    /// The rest of the run being grouped.
    current: &'a [K],
}

impl<'a, K: Pod + Ord, V: Pod> Iterator for PrefixGroups<'a, K, V> {
    type Item = (&'a K, KeysIter<'a, K, V>);

    fn next(&mut self) -> Option<Self::Item> {
        let memtable = &self.map.memtable;
        if let Some(head) = self.mem_heads.next() {
            let mem = match self.mem_chains {
                Some(next) => MemKeys::Chain { next, at: head },
                None => MemKeys::All(head..head + 1),
            };
            let keys = KeysIter {
                memtable,
                mem,
                runs: SmallVec::new(),
                next_run: 0,
                current: [].iter(),
            };
            return Some((&memtable.entry(head).0, keys));
        }
        while self.current.is_empty() {
            self.current = self.runs.get(self.next_run)?;
            self.next_run += 1;
        }
        let first = &self.current[0];
        let prefix = &words(first)[..self.prefix_len];
        let len = self
            .current
            .partition_point(|k| cmp_prefix(words(k), prefix).is_eq());
        let (group, rest) = self.current.split_at(len);
        self.current = rest;
        let keys = KeysIter {
            memtable,
            mem: MemKeys::All(0..0),
            runs: smallvec::smallvec![group],
            next_run: 0,
            current: [].iter(),
        };
        Some((first, keys))
    }
}

/// Streaming k-way merge of the memtable and all runs, yielding each distinct
/// key once with a borrowing iterator over its merged values.
pub struct IterAllRefs<'a, K, V> {
    sources: Vec<Peekable<Box<dyn Iterator<Item = (&'a K, &'a [V])> + 'a>>>,
}

impl<'a, K: Ord, V> Iterator for IterAllRefs<'a, K, V> {
    type Item = (&'a K, ValsRefs<'a, V>);

    fn next(&mut self) -> Option<Self::Item> {
        // Smallest key currently at the head of any source.
        let mut min_key: Option<&'a K> = None;
        for source in &mut self.sources {
            if let Some(&(key, _)) = source.peek() {
                min_key = Some(match min_key {
                    Some(current) if *current <= *key => current,
                    _ => key,
                });
            }
        }
        let min_key = min_key?;

        // Collect (and advance) every source whose head equals `min_key`.
        let mut slices: SmallVec<[&'a [V]; 4]> = SmallVec::new();
        for source in &mut self.sources {
            if source.peek().is_some_and(|&(key, _)| *key == *min_key) {
                let (_, vals) = source.next().unwrap();
                if !vals.is_empty() {
                    slices.push(vals);
                }
            }
        }
        Some((min_key, ValsRefs::new(slices)))
    }
}

fn parse_run_file_name(path: &Path) -> Option<(usize, u64)> {
    let stem = path.file_stem()?.to_str()?;
    let mut parts = stem.split('_');
    if parts.next()? != "run" {
        return None;
    }
    let level = parts.next()?.parse().ok()?;
    let id = parts.next()?.parse().ok()?;
    Some((level, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lsm-mmap-test-{tag}-{}-{}",
            std::process::id(),
            NEXT_TEMP_RUN_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn reads_across_memtable_and_runs() {
        // Small limit forces several flushes and a compaction.
        let mut lsm = MmapLsmMultiMap::<u64, u64>::with_memtable_limit_and_level_fanout(2, 2);
        for (k, v) in [
            (3, 30),
            (1, 10),
            (3, 31),
            (2, 20),
            (1, 11),
            (4, 40),
            (1, 12),
        ] {
            lsm.insert(k, v);
        }
        assert_eq!(lsm.len(), 7);

        let got: BTreeSet<u64> = lsm.get_refs(&1).unwrap().copied().collect();
        assert_eq!(got, BTreeSet::from([10, 11, 12]));
        let got: BTreeSet<u64> = lsm.get_refs(&3).unwrap().copied().collect();
        assert_eq!(got, BTreeSet::from([30, 31]));
        assert_eq!(
            lsm.get_refs(&4).unwrap().copied().collect::<Vec<_>>(),
            vec![40]
        );
        assert!(lsm.get_refs(&99).is_none());

        assert!(lsm.contains_value(&2, &20));
        assert!(!lsm.contains_value(&2, &99));

        // iter_all_refs yields each key once with all merged values.
        let mut all: Vec<(u64, Vec<u64>)> = lsm
            .iter_all_refs()
            .map(|(k, vals)| (*k, vals.copied().collect()))
            .collect();
        for (_, vals) in &mut all {
            vals.sort_unstable();
        }
        assert_eq!(
            all,
            vec![
                (1, vec![10, 11, 12]),
                (2, vec![20]),
                (3, vec![30, 31]),
                (4, vec![40]),
            ]
        );
        let total: usize = lsm.iter_all_refs().map(|(_, vals)| vals.count()).sum();
        assert_eq!(total, 7);
    }

    /// A set: zero-sized values, spread over the memtable and several runs.
    #[test]
    fn unit_values_across_runs() {
        let mut set = MmapLsmMultiMap::<u64, ()>::with_memtable_limit_and_level_fanout(3, 2);
        for k in (0..40u64).step_by(2) {
            set.insert(k, ());
        }
        assert!(!set.levels.is_empty());
        for k in 0..40u64 {
            assert_eq!(set.contains_key(&k), k % 2 == 0, "key {k}");
            assert_eq!(
                set.get_refs(&k).map(Iterator::count),
                (k % 2 == 0).then_some(1)
            );
        }
        assert_eq!(set.iter_all_refs().count(), 20);
    }

    /// `absorb` keeps every value of both maps, whether it sat in a memtable or a
    /// run, and leaves the source empty.
    #[test]
    fn absorb_moves_runs_and_memtable() {
        for other_entries in [3u64, 50] {
            let mut to = MmapLsmMultiMap::<u64, u64>::with_memtable_limit_and_level_fanout(8, 2);
            let mut from = MmapLsmMultiMap::<u64, u64>::with_memtable_limit_and_level_fanout(8, 2);
            for i in 0..30u64 {
                to.insert(i % 7, i);
            }
            for i in 0..other_entries {
                from.insert(i % 5, 1000 + i);
            }
            to.absorb(&mut from);
            assert!(from.is_empty());
            assert_eq!(from.iter_all_refs().count(), 0);
            assert_eq!(to.len() as u64, 30 + other_entries);
            for k in 0..7u64 {
                let got: BTreeSet<u64> = to.get_refs(&k).into_iter().flatten().copied().collect();
                let want: BTreeSet<u64> = (0..30)
                    .filter(|i| i % 7 == k)
                    .chain((0..other_entries).filter(|i| i % 5 == k).map(|i| 1000 + i))
                    .collect();
                assert_eq!(got, want, "key {k}");
            }
        }
    }

    /// Prefix reads see every key with the prefix, whether in a run or the
    /// memtable, including keys added after an earlier read.
    #[test]
    fn keys_with_prefix_across_runs_and_memtable() {
        for prefix_lens in [&[][..], &[8, 16]] {
            keys_with_prefix_reads_back(prefix_lens);
        }
    }

    fn keys_with_prefix_reads_back(prefix_lens: &[usize]) {
        let mut set = MmapLsmMultiMap::<[u64; 3], ()>::with_memtable_limit_and_level_fanout(16, 2)
            .with_prefix_reads(prefix_lens);
        let mut all = BTreeSet::new();
        let check = |set: &MmapLsmMultiMap<[u64; 3], ()>, all: &BTreeSet<[u64; 3]>| {
            for a in 0..5u64 {
                let got: BTreeSet<_> = set.keys_with_prefix(&[a]).copied().collect();
                let want: BTreeSet<_> = all.iter().filter(|k| k[0] == a).copied().collect();
                assert_eq!(got, want, "prefix [{a}]");
                for b in 0..4u64 {
                    let got: BTreeSet<_> = set.keys_with_prefix(&[a, b]).copied().collect();
                    let want: BTreeSet<_> = all
                        .iter()
                        .filter(|k| k[0] == a && k[1] == b)
                        .copied()
                        .collect();
                    assert_eq!(got, want, "prefix [{a}, {b}]");
                }
            }
            let every: BTreeSet<_> = set.keys().copied().collect();
            assert_eq!(&every, all);
            for len in 1..=2 {
                let mut grouped = BTreeSet::new();
                for (first, keys) in set.prefix_groups(len) {
                    for key in keys {
                        assert_eq!(key[..len], first[..len], "group by {len} words");
                        assert!(grouped.insert(*key), "{key:?} in two groups");
                    }
                }
                assert_eq!(&grouped, all);
            }
        };
        for i in 0..200u64 {
            let key = [i * 7 % 5, i * 3 % 4, i];
            set.insert(key, ());
            all.insert(key);
            if i % 23 == 0 {
                check(&set, &all);
            }
        }
        assert!(!set.levels.is_empty());
        check(&set, &all);
    }

    #[test]
    fn reopen_maps_existing_runs() {
        let dir = temp_dir("reopen");
        {
            let mut lsm = MmapLsmMultiMap::<u64, u64>::open(&dir, 4).unwrap();
            for k in 0..6u64 {
                lsm.insert(k, k * 10);
                lsm.insert(k, k * 10 + 1);
            }
            lsm.flush().unwrap();
        }
        let reopened = MmapLsmMultiMap::<u64, u64>::open(&dir, 4).unwrap();
        assert_eq!(reopened.len(), 12);
        for k in 0..6u64 {
            let got: BTreeSet<u64> = reopened.get_refs(&k).unwrap().copied().collect();
            assert_eq!(got, BTreeSet::from([k * 10, k * 10 + 1]));
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// Drives many keys (each with several values, overlapping across runs)
    /// through small limit/fanout to force repeated multi-level streaming merges,
    /// then checks every value survived — and that it round-trips after reopen.
    #[test]
    fn streaming_merge_multi_level_round_trips() {
        let dir = temp_dir("multilevel");
        const KEYS: u64 = 300;
        const VALUES_PER_KEY: u64 = 4;
        {
            let mut lsm = MmapLsmMultiMap::<u64, u64>::open_with_level_fanout(&dir, 8, 2).unwrap();
            // Interleave keys so values for one key land in many different runs.
            for v in 0..VALUES_PER_KEY {
                for k in 0..KEYS {
                    lsm.insert(k, k * 1000 + v);
                }
            }
            lsm.flush().unwrap();
            assert_eq!(lsm.len() as u64, KEYS * VALUES_PER_KEY);
            // Compaction must have promoted runs beyond level 0.
            assert!(lsm.levels.len() > 1);
            for k in 0..KEYS {
                let got: BTreeSet<u64> = lsm.get_refs(&k).unwrap().copied().collect();
                let want: BTreeSet<u64> = (0..VALUES_PER_KEY).map(|v| k * 1000 + v).collect();
                assert_eq!(got, want, "key {k}");
            }
        }

        let reopened = MmapLsmMultiMap::<u64, u64>::open(&dir, 8).unwrap();
        assert_eq!(reopened.len() as u64, KEYS * VALUES_PER_KEY);
        let scanned: usize = reopened.iter_all_refs().map(|(_, vals)| vals.count()).sum();
        assert_eq!(scanned as u64, KEYS * VALUES_PER_KEY);
        let _ = fs::remove_dir_all(dir);
    }
}
