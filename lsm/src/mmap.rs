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
use std::io::{self, BufWriter, Write};
use std::iter::{Flatten, Peekable};
use std::marker::PhantomData;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::vec;

use bytemuck::Pod;
use memmap2::Mmap;

const DEFAULT_LEVEL_FANOUT: usize = 4;
const RUN_MAGIC: &[u8; 4] = b"LSMM";
/// Fixed run header: magic(4) + version(4) + entries_count(8) + values_len(8) +
/// keys_off(8) + offsets_off(8) + values_off(8).
const HEADER_LEN: usize = 4 + 4 + 8 * 5;
const DEFAULT_DISK_MEMTABLE_LIMIT: usize = 10_000;
static NEXT_DISK_DIR_ID: AtomicU64 = AtomicU64::new(0);

/// Borrowing value iterator: a flattened list of slices, one per source (the
/// memtable plus each run that holds the key). Cheap to clone, which the Ascent
/// `Ind*ValsIter` associated types require.
pub type ValsRefs<'a, V> = Flatten<vec::IntoIter<&'a [V]>>;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// A single immutable run, memory-mapped. Holds no decoded keys or values — only
/// the mapping plus the section offsets parsed from the header.
#[derive(Debug)]
struct MmapRun<K, V> {
    mmap: Mmap,
    path: PathBuf,
    entries_count: usize,
    values_len: usize,
    keys_off: usize,
    offsets_off: usize,
    values_off: usize,
    _marker: PhantomData<(K, V)>,
}

impl<K: Pod + Ord, V: Pod> MmapRun<K, V> {
    fn keys(&self) -> &[K] {
        let bytes = &self.mmap[self.keys_off..self.keys_off + self.entries_count * size_of::<K>()];
        bytemuck::cast_slice(bytes)
    }

    fn offsets(&self) -> &[u64] {
        let bytes =
            &self.mmap[self.offsets_off..self.offsets_off + (self.entries_count + 1) * size_of::<u64>()];
        bytemuck::cast_slice(bytes)
    }

    fn values(&self) -> &[V] {
        let bytes = &self.mmap[self.values_off..self.values_off + self.values_len * size_of::<V>()];
        bytemuck::cast_slice(bytes)
    }

    /// Values for `key`, as a zero-copy slice into the mapping, or `None`.
    fn find(&self, key: &K) -> Option<&[V]> {
        let keys = self.keys();
        match keys.binary_search(key) {
            Ok(i) => {
                let offsets = self.offsets();
                Some(&self.values()[offsets[i] as usize..offsets[i + 1] as usize])
            }
            Err(_) => None,
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

/// Total buffered run bytes above which the [`RunBuilder`] spills its columnar
/// sections to scratch files instead of holding them in memory. Below it a run
/// is assembled entirely in RAM and written in one pass (no scratch files —
/// fast, the common case during compaction); above it the run streams to disk so
/// peak memory stays bounded by roughly this amount regardless of run size.
const SPILL_THRESHOLD: usize = 256 * 1024;

/// One columnar section of a run. It accumulates in memory until the builder
/// crosses [`SPILL_THRESHOLD`], at which point every section spills to a scratch
/// file and subsequent writes stream straight to disk.
enum Section {
    Mem(Vec<u8>),
    Spilled {
        writer: BufWriter<fs::File>,
        path: PathBuf,
    },
}

impl Section {
    fn mem_len(&self) -> usize {
        match self {
            Section::Mem(buf) => buf.len(),
            Section::Spilled { .. } => 0,
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Section::Mem(buf) => {
                buf.extend_from_slice(bytes);
                Ok(())
            }
            Section::Spilled { writer, .. } => writer.write_all(bytes),
        }
    }

    /// Moves an in-memory section to a scratch file (no-op if already spilled).
    fn spill(&mut self, path: PathBuf) -> io::Result<()> {
        if let Section::Mem(buf) = self {
            let mut writer = BufWriter::new(fs::File::create(&path)?);
            writer.write_all(buf)?;
            *self = Section::Spilled { writer, path };
        }
        Ok(())
    }

    /// Streams this section's bytes into `out`, consuming it.
    fn copy_into(self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Section::Mem(buf) => out.write_all(&buf),
            Section::Spilled { mut writer, path } => {
                writer.flush()?;
                drop(writer);
                let mut file = fs::File::open(&path)?;
                io::copy(&mut file, out)?;
                let _ = fs::remove_file(&path);
                Ok(())
            }
        }
    }
}

/// Assembles a run's three columnar sections, keeping small runs entirely in
/// memory and spilling larger ones to scratch files past [`SPILL_THRESHOLD`], so
/// peak memory is bounded by the threshold while the common (small) case never
/// touches the filesystem.
struct RunBuilder {
    keys: Section,
    offsets: Section,
    values: Section,
    out_path: PathBuf,
    entries_count: usize,
    values_len: u64,
    spilled: bool,
}

impl RunBuilder {
    fn new(out_path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            keys: Section::Mem(Vec::new()),
            offsets: Section::Mem(Vec::new()),
            values: Section::Mem(Vec::new()),
            out_path,
            entries_count: 0,
            values_len: 0,
            spilled: false,
        })
    }

    fn mem_total(&self) -> usize {
        self.keys.mem_len() + self.offsets.mem_len() + self.values.mem_len()
    }

    /// Spills all sections to scratch files if the in-memory buffers plus the
    /// `incoming` bytes about to be written would exceed the threshold. Called
    /// before each write so a single large append can't blow the bound.
    fn reserve(&mut self, incoming: usize) -> io::Result<()> {
        if !self.spilled && self.mem_total() + incoming > SPILL_THRESHOLD {
            self.keys.spill(self.out_path.with_extension("keys.tmp"))?;
            self.offsets.spill(self.out_path.with_extension("offsets.tmp"))?;
            self.values.spill(self.out_path.with_extension("values.tmp"))?;
            self.spilled = true;
        }
        Ok(())
    }

    /// Begins a new entry, recording its key and the offset at which its values
    /// start. Must be called in ascending key order, then followed by zero or
    /// more `append_values` calls for that entry.
    fn start_key<K: Pod>(&mut self, key: &K) -> io::Result<()> {
        self.reserve(size_of::<K>() + size_of::<u64>())?;
        self.keys.write_all(bytemuck::bytes_of(key))?;
        self.offsets.write_all(&self.values_len.to_ne_bytes())?;
        self.entries_count += 1;
        Ok(())
    }

    /// Appends values to the current entry. May be called several times to
    /// concatenate slices coming from different source runs.
    fn append_values<V: Pod>(&mut self, vals: &[V]) -> io::Result<()> {
        let bytes: &[u8] = bytemuck::cast_slice(vals);
        self.reserve(bytes.len())?;
        self.values.write_all(bytes)?;
        self.values_len += vals.len() as u64;
        Ok(())
    }

    /// Assembles the sections into the final, mmap-able run file.
    fn finish<K: Pod + Ord, V: Pod>(mut self) -> io::Result<MmapRun<K, V>> {
        // Closing sentinel offset (== total values).
        self.offsets.write_all(&self.values_len.to_ne_bytes())?;

        let entries_count = self.entries_count;
        let values_len = self.values_len;
        let keys_bytes = entries_count * size_of::<K>();
        let offsets_bytes = (entries_count + 1) * size_of::<u64>();
        let keys_off = align8(HEADER_LEN);
        let offsets_off = align8(keys_off + keys_bytes);
        let values_off = align8(offsets_off + offsets_bytes);

        let mut out = BufWriter::new(fs::File::create(&self.out_path)?);
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(RUN_MAGIC);
        // header[4..8] left as the zero version/pad.
        header[8..16].copy_from_slice(&(entries_count as u64).to_ne_bytes());
        header[16..24].copy_from_slice(&values_len.to_ne_bytes());
        header[24..32].copy_from_slice(&(keys_off as u64).to_ne_bytes());
        header[32..40].copy_from_slice(&(offsets_off as u64).to_ne_bytes());
        header[40..48].copy_from_slice(&(values_off as u64).to_ne_bytes());
        out.write_all(&header)?;
        // HEADER_LEN == keys_off (48, already 8-aligned), so no pad before keys.
        self.keys.copy_into(&mut out)?;
        write_padding(&mut out, offsets_off - (keys_off + keys_bytes))?;
        self.offsets.copy_into(&mut out)?;
        write_padding(&mut out, values_off - (offsets_off + offsets_bytes))?;
        self.values.copy_into(&mut out)?;
        out.flush()?;
        drop(out);
        load_run(self.out_path)
    }
}

/// Writes `n` (< 8) zero padding bytes to align the next section.
fn write_padding(out: &mut impl Write, n: usize) -> io::Result<()> {
    const ZEROS: [u8; 8] = [0u8; 8];
    if n > 0 {
        out.write_all(&ZEROS[..n])?;
    }
    Ok(())
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
    Ok(MmapRun {
        mmap,
        path,
        entries_count,
        values_len,
        keys_off,
        offsets_off,
        values_off,
        _marker: PhantomData,
    })
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
    dir: PathBuf,
    next_run_id: u64,
    is_temp: bool,
    dir_created: bool,
}

impl<K, V> Drop for MmapLsmMultiMap<K, V> {
    fn drop(&mut self) {
        if self.is_temp && self.dir_created {
            // Drop the mappings before removing the directory.
            self.levels.clear();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

impl<K: Pod + Ord, V: Pod> Default for MmapLsmMultiMap<K, V> {
    fn default() -> Self {
        Self::with_memtable_limit_and_level_fanout(DEFAULT_DISK_MEMTABLE_LIMIT, DEFAULT_LEVEL_FANOUT)
    }
}

impl<K: Pod + Ord, V: Pod> MmapLsmMultiMap<K, V> {
    /// Creates a zero-copy LSM in a unique temporary directory. The directory is
    /// created lazily, on the first flush.
    pub fn with_memtable_limit(memtable_limit: usize) -> Self {
        Self::with_memtable_limit_and_level_fanout(memtable_limit, DEFAULT_LEVEL_FANOUT)
    }

    pub fn with_memtable_limit_and_level_fanout(memtable_limit: usize, level_fanout: usize) -> Self {
        let id = NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lsm-mmap-{}-{id}", std::process::id()));
        Self {
            memtable: BTreeMap::new(),
            levels: Vec::new(),
            memtable_len: 0,
            len: 0,
            memtable_limit: memtable_limit.max(1),
            level_fanout: level_fanout.max(1),
            dir,
            next_run_id: 0,
            is_temp: true,
            dir_created: false,
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
            dir,
            next_run_id,
            is_temp: false,
            dir_created: true,
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
        let mut slices: Vec<&[V]> = Vec::new();
        if let Some(vals) = self.memtable.get(key) {
            if !vals.is_empty() {
                slices.push(vals);
            }
        }
        for run in self.levels.iter().flatten() {
            if let Some(vals) = run.find(key) {
                if !vals.is_empty() {
                    slices.push(vals);
                }
            }
        }
        if slices.is_empty() {
            None
        } else {
            Some(slices.into_iter().flatten())
        }
    }

    /// Membership test for a `(key, value)` pair across memtable and runs,
    /// allocation-free.
    pub fn contains_value(&self, key: &K, value: &V) -> bool
    where
        V: PartialEq,
    {
        if self.memtable.get(key).is_some_and(|vals| vals.contains(value)) {
            return true;
        }
        self.levels
            .iter()
            .flatten()
            .any(|run| run.find(key).is_some_and(|vals| vals.contains(value)))
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
            let _ = fs::remove_file(run.path);
        }
        self.memtable_len = 0;
        self.len = 0;
    }

    /// Persists the current memtable as a run.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.memtable_len == 0 {
            return Ok(());
        }
        self.ensure_dir()?;
        let memtable = std::mem::take(&mut self.memtable);
        self.memtable_len = 0;
        // The memtable iterates ascending — stream it straight to a run.
        let mut builder = RunBuilder::new(self.new_run_path(0))?;
        for (key, vals) in &memtable {
            builder.start_key(key)?;
            builder.append_values(vals)?;
        }
        let run = builder.finish()?;
        self.push_disk_run(0, run)
    }

    fn ensure_dir(&mut self) -> io::Result<()> {
        if !self.dir_created {
            fs::create_dir_all(&self.dir)?;
            self.dir_created = true;
        }
        Ok(())
    }

    fn new_run_path(&mut self, level: usize) -> PathBuf {
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.dir.join(format!("run_{level}_{id}.lsm"))
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
    /// runs and stream that key's values (concatenated from every run holding it)
    /// straight to the [`RunBuilder`]. Memory stays bounded by the cursors plus
    /// the I/O buffers — the merged run is never materialized.
    fn merge_runs(&mut self, level: usize, runs: Vec<MmapRun<K, V>>) -> io::Result<MmapRun<K, V>> {
        let mut builder = RunBuilder::new(self.new_run_path(level))?;
        // Per-run cursor: index of the next unconsumed entry.
        let mut cursors: Vec<usize> = vec![0; runs.len()];

        loop {
            // Smallest key currently at any run's cursor head.
            let mut min_key: Option<K> = None;
            for (r, run) in runs.iter().enumerate() {
                if cursors[r] < run.entries_count {
                    let key = run.keys()[cursors[r]];
                    if min_key.is_none_or(|current| key < current) {
                        min_key = Some(key);
                    }
                }
            }
            let Some(min_key) = min_key else { break };

            builder.start_key(&min_key)?;
            // Append the values from every run whose head equals `min_key`,
            // writing the mmap'd slices straight through.
            for (r, run) in runs.iter().enumerate() {
                if cursors[r] < run.entries_count && run.keys()[cursors[r]] == min_key {
                    let (_, vals) = run.entry(cursors[r]);
                    builder.append_values(vals)?;
                    cursors[r] += 1;
                }
            }
        }

        let new_run = builder.finish()?;
        for run in runs {
            let _ = fs::remove_file(&run.path);
        }
        Ok(new_run)
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
        let mut slices: Vec<&'a [V]> = Vec::new();
        for source in &mut self.sources {
            if source.peek().is_some_and(|&(key, _)| *key == *min_key) {
                let (_, vals) = source.next().unwrap();
                if !vals.is_empty() {
                    slices.push(vals);
                }
            }
        }
        Some((min_key, slices.into_iter().flatten()))
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
            NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn reads_across_memtable_and_runs() {
        // Small limit forces several flushes and a compaction.
        let mut lsm = MmapLsmMultiMap::<u64, u64>::with_memtable_limit_and_level_fanout(2, 2);
        for (k, v) in [(3, 30), (1, 10), (3, 31), (2, 20), (1, 11), (4, 40), (1, 12)] {
            lsm.insert(k, v);
        }
        assert_eq!(lsm.len(), 7);

        let got: BTreeSet<u64> = lsm.get_refs(&1).unwrap().copied().collect();
        assert_eq!(got, BTreeSet::from([10, 11, 12]));
        let got: BTreeSet<u64> = lsm.get_refs(&3).unwrap().copied().collect();
        assert_eq!(got, BTreeSet::from([30, 31]));
        assert_eq!(lsm.get_refs(&4).unwrap().copied().collect::<Vec<_>>(), vec![40]);
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
            let mut lsm =
                MmapLsmMultiMap::<u64, u64>::open_with_level_fanout(&dir, 8, 2).unwrap();
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
