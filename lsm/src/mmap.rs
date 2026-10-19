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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::iter::Peekable;
use std::marker::PhantomData;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytemuck::Pod;
use memmap2::{Mmap, MmapMut};
use smallvec::SmallVec;

use crate::bloom::{Bloom, hash_key};

const DEFAULT_LEVEL_FANOUT: usize = 4;
const RUN_MAGIC: &[u8; 4] = b"LSMM";
/// Fixed run header: magic(4) + version(4) + entries_count(8) + values_len(8) +
/// keys_off(8) + offsets_off(8) + values_off(8).
const HEADER_LEN: usize = 4 + 4 + 8 * 5;
const DEFAULT_DISK_MEMTABLE_LIMIT: usize = 10_000;
static NEXT_TEMP_RUN_ID: AtomicU64 = AtomicU64::new(0);

/// Borrowing value iterator over a list of slices, one per source (the memtable
/// plus each run that holds the key). The list is held inline up to a few
/// sources, so a lookup does not allocate. Cheap to clone, which the Ascent
/// `Ind*ValsIter` associated types require.
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
    mmap: Mmap,
    /// The run's file, for a persistent map. A temporary run's file is unlinked
    /// as soon as it is mapped, so it needs no cleanup.
    path: Option<PathBuf>,
    entries_count: usize,
    values_len: usize,
    keys_off: usize,
    offsets_off: usize,
    values_off: usize,
    /// The run's keys, for skipping the run on a lookup of a key it lacks.
    bloom: Bloom,
    _marker: PhantomData<(K, V)>,
}

impl<K: Pod + Ord, V: Pod> MmapRun<K, V> {
    fn keys(&self) -> &[K] {
        let bytes = &self.mmap[self.keys_off..self.keys_off + self.entries_count * size_of::<K>()];
        bytemuck::cast_slice(bytes)
    }

    fn offsets(&self) -> &[u64] {
        let bytes = &self.mmap
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
        let bytes = &self.mmap[self.values_off..self.values_off + self.values_len * size_of::<V>()];
        bytemuck::cast_slice(bytes)
    }

    fn contains_key(&self, key: &K, hash: u64) -> bool {
        self.bloom.may_contain(hash) && self.keys().binary_search(key).is_ok()
    }

    /// Values for `key`, as a zero-copy slice into the mapping, or `None`.
    /// `hash` is the key's [`hash_key`].
    fn find(&self, key: &K, hash: u64) -> Option<&[V]> {
        if !self.bloom.may_contain(hash) {
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
/// caller knows the run's shape up front (entry and value counts), so the layout
/// is fixed before the first byte and every section is written in place: no
/// intermediate buffers or scratch files, and no write syscalls.
struct RunWriter {
    mmap: MmapMut,
    path: PathBuf,
    keep_file: bool,
    entries_count: usize,
    values_len: usize,
    keys_off: usize,
    offsets_off: usize,
    values_off: usize,
    /// Entries and values written so far.
    keys_written: usize,
    values_written: usize,
    bloom: Bloom,
}

impl RunWriter {
    /// Creates the run file at `path`, sized for `entries_count` keys and
    /// `values_len` values of the given widths, and maps it for writing. A
    /// temporary run (`keep_file == false`) is unlinked once it is mapped for
    /// reading, so it disappears with its last mapping.
    fn create<K: Pod, V: Pod>(
        path: PathBuf,
        keep_file: bool,
        entries_count: usize,
        values_len: usize,
    ) -> io::Result<Self> {
        let keys_off = align8(HEADER_LEN);
        let offsets_off = align8(keys_off + entries_count * size_of::<K>());
        let values_off = align8(offsets_off + (entries_count + 1) * size_of::<u64>());
        let file_len = values_off + values_len * size_of::<V>();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(file_len as u64)?;
        // SAFETY: the file was just created by us and nothing else maps or writes
        // it while this mapping lives.
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };
        let header = &mut mmap[..HEADER_LEN];
        header[0..4].copy_from_slice(RUN_MAGIC);
        // header[4..8] stays the zero version/pad.
        header[8..16].copy_from_slice(&(entries_count as u64).to_ne_bytes());
        header[16..24].copy_from_slice(&(values_len as u64).to_ne_bytes());
        header[24..32].copy_from_slice(&(keys_off as u64).to_ne_bytes());
        header[32..40].copy_from_slice(&(offsets_off as u64).to_ne_bytes());
        header[40..48].copy_from_slice(&(values_off as u64).to_ne_bytes());
        Ok(Self {
            mmap,
            path,
            keep_file,
            entries_count,
            values_len,
            keys_off,
            offsets_off,
            values_off,
            keys_written: 0,
            values_written: 0,
            bloom: Bloom::with_capacity(entries_count),
        })
    }

    fn write_offset(&mut self, i: usize, offset: usize) {
        let at = self.offsets_off + i * size_of::<u64>();
        self.mmap[at..at + size_of::<u64>()].copy_from_slice(&(offset as u64).to_ne_bytes());
    }

    /// Begins a new entry. Must be called in ascending key order, then followed
    /// by zero or more `append_values` calls for that entry.
    fn start_key<K: Pod>(&mut self, key: &K) {
        assert!(self.keys_written < self.entries_count, "run overflow: keys");
        let at = self.keys_off + self.keys_written * size_of::<K>();
        self.mmap[at..at + size_of::<K>()].copy_from_slice(bytemuck::bytes_of(key));
        self.bloom.insert(hash_key(key));
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
        self.mmap[at..at + bytes.len()].copy_from_slice(bytes);
        self.values_written += vals.len();
    }

    fn finish<K: Pod + Ord, V: Pod>(mut self) -> io::Result<MmapRun<K, V>> {
        assert_eq!(self.keys_written, self.entries_count, "run underflow: keys");
        assert_eq!(
            self.values_written, self.values_len,
            "run underflow: values"
        );
        // Closing sentinel offset (== total values).
        self.write_offset(self.entries_count, self.values_len);
        let mmap = self.mmap.make_read_only()?;
        let path = if self.keep_file {
            Some(self.path)
        } else {
            fs::remove_file(&self.path)?;
            None
        };
        Ok(MmapRun {
            mmap,
            path,
            entries_count: self.entries_count,
            values_len: self.values_len,
            keys_off: self.keys_off,
            offsets_off: self.offsets_off,
            values_off: self.values_off,
            bloom: self.bloom,
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
    if mmap.len() < HEADER_LEN || &mmap[0..4] != RUN_MAGIC {
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
    let mut run = MmapRun {
        mmap,
        path: Some(path),
        entries_count,
        values_len,
        keys_off,
        offsets_off,
        values_off,
        bloom: Bloom::with_capacity(0),
        _marker: PhantomData,
    };
    let mut bloom = Bloom::with_capacity(entries_count);
    for key in run.keys() {
        bloom.insert(hash_key(key));
    }
    run.bloom = bloom;
    Ok(run)
}

/// A disk-backed LSM multi-map with zero-copy, reference-returning reads.
#[derive(Debug)]
pub struct MmapLsmMultiMap<K, V> {
    memtable: BTreeMap<K, Vec<V>>,
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
            memtable: BTreeMap::new(),
            levels: Vec::new(),
            memtable_len: 0,
            len: 0,
            memtable_limit: memtable_limit.max(1),
            level_fanout: level_fanout.max(1),
            dir: None,
            next_run_id: 0,
        }
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
            memtable: BTreeMap::new(),
            levels,
            memtable_len: 0,
            len,
            memtable_limit: memtable_limit.max(1),
            level_fanout: level_fanout.max(1),
            dir: Some(dir),
            next_run_id,
        })
    }

    pub fn insert(&mut self, key: K, value: V) {
        self.memtable.entry(key).or_default().push(value);
        self.memtable_len += 1;
        self.len += 1;
        if self.memtable_len >= self.memtable_limit {
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
        if let Some(vals) = self.memtable.get(key)
            && !vals.is_empty()
        {
            slices.push(vals);
        }
        let hash = hash_key(key);
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
        if self.memtable.contains_key(key) {
            return true;
        }
        let hash = hash_key(key);
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
        if self
            .memtable
            .get(key)
            .is_some_and(|vals| vals.contains(value))
        {
            return true;
        }
        let hash = hash_key(key);
        self.levels
            .iter()
            .flatten()
            .any(|run| run.find(key, hash).is_some_and(|vals| vals.contains(value)))
    }

    /// Borrowing scan over every distinct key with all of its values, merged
    /// across the memtable and runs. Keys and values are references into the
    /// memtable / memory-mapped runs.
    pub fn iter_all_refs(&self) -> IterAllRefs<'_, K, V> {
        let mut sources: Vec<Peekable<Box<dyn Iterator<Item = (&K, &[V])> + '_>>> = Vec::new();
        let memtable: Box<dyn Iterator<Item = (&K, &[V])>> =
            Box::new(self.memtable.iter().map(|(k, v)| (k, v.as_slice())));
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
        self.memtable.clear();
        for run in std::mem::take(&mut self.levels).into_iter().flatten() {
            run.delete();
        }
        self.memtable_len = 0;
        self.len = 0;
    }

    /// Persists the current memtable as a run.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.memtable_len == 0 {
            return Ok(());
        }
        let memtable = std::mem::take(&mut self.memtable);
        let values_len = self.memtable_len;
        self.memtable_len = 0;
        // The memtable iterates ascending — stream it straight to a run.
        let mut writer = self.new_run_writer(0, memtable.len(), values_len)?;
        for (key, vals) in &memtable {
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
        RunWriter::create::<K, V>(path, keep_file, entries_count, values_len)
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
    /// straight into the new run's mapping. A first pass over the keys alone
    /// counts the distinct keys, which fixes the new run's layout.
    fn merge_runs(&mut self, level: usize, runs: Vec<MmapRun<K, V>>) -> io::Result<MmapRun<K, V>> {
        let values_len = runs.iter().map(|run| run.values_len).sum();
        let mut entries_count = 0;
        merge_keys(&runs, |_, _| entries_count += 1);
        let mut writer = self.new_run_writer(level, entries_count, values_len)?;
        merge_keys(&runs, |key, holders| {
            writer.start_key(key);
            for &(r, i) in holders {
                writer.append_values(runs[r].entry(i).1);
            }
        });
        let new_run = writer.finish()?;
        for run in runs {
            run.delete();
        }
        Ok(new_run)
    }
}

/// Visits the distinct keys of `runs` in ascending order, each with the
/// `(run, entry)` positions that hold it, in run order.
fn merge_keys<K: Pod + Ord, V: Pod>(
    runs: &[MmapRun<K, V>],
    mut visit: impl FnMut(&K, &[(usize, usize)]),
) {
    // Per-run cursor: index of the next unconsumed entry.
    let mut cursors: Vec<usize> = vec![0; runs.len()];
    let mut holders: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
    loop {
        // Smallest key currently at any run's cursor head.
        let mut min_key: Option<&K> = None;
        for (r, run) in runs.iter().enumerate() {
            if cursors[r] < run.entries_count {
                let key = &run.keys()[cursors[r]];
                if min_key.is_none_or(|current| key < current) {
                    min_key = Some(key);
                }
            }
        }
        let Some(min_key) = min_key else { break };
        holders.clear();
        for (r, run) in runs.iter().enumerate() {
            if cursors[r] < run.entries_count && run.keys()[cursors[r]] == *min_key {
                holders.push((r, cursors[r]));
                cursors[r] += 1;
            }
        }
        visit(min_key, &holders);
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
