use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DEFAULT_LEVEL_FANOUT: usize = 4;
const RUN_MAGIC: &[u8; 4] = b"LSM1";
/// Bytes preceding the entries section: `RUN_MAGIC` (4) + `entries_count` (8).
const RUN_HEADER_LEN: u64 = RUN_MAGIC.len() as u64 + 8;
/// Fixed footer at the end of a run: `index_start` (8) + `values_len` (8).
const RUN_FOOTER_LEN: i64 = 16;

const DEFAULT_DISK_MEMTABLE_LIMIT: usize = 10_000;
static NEXT_DISK_DIR_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct Run<K, V> {
    /// Invariant: no duplicate keys, sorted from max key to min key.
    entries: Vec<(K, Vec<V>)>,
}

pub trait LsmDiskCodec: Sized {
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()>;
    fn decode<R: Read>(reader: &mut R) -> io::Result<Self>;
}

macro_rules! impl_lsm_disk_codec_for_int {
    ($($ty: ty),* $(,)?) => {
        $(
            impl LsmDiskCodec for $ty {
                fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
                    writer.write_all(&self.to_le_bytes())
                }

                fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
                    let mut bytes = [0_u8; std::mem::size_of::<$ty>()];
                    reader.read_exact(&mut bytes)?;
                    Ok(<$ty>::from_le_bytes(bytes))
                }
            }
        )*
    };
}

impl_lsm_disk_codec_for_int!(u8, u16, u32, u64, u128, usize);
impl_lsm_disk_codec_for_int!(i8, i16, i32, i64, i128, isize);

impl LsmDiskCodec for () {
    fn encode<W: Write>(&self, _writer: &mut W) -> io::Result<()> {
        Ok(())
    }

    fn decode<R: Read>(_reader: &mut R) -> io::Result<Self> {
        Ok(())
    }
}

impl<T0: LsmDiskCodec> LsmDiskCodec for (T0,) {
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok((T0::decode(reader)?,))
    }
}

impl<T0: LsmDiskCodec, T1: LsmDiskCodec> LsmDiskCodec for (T0, T1) {
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.encode(writer)?;
        self.1.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok((T0::decode(reader)?, T1::decode(reader)?))
    }
}

impl<T0: LsmDiskCodec, T1: LsmDiskCodec, T2: LsmDiskCodec> LsmDiskCodec for (T0, T1, T2) {
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.encode(writer)?;
        self.1.encode(writer)?;
        self.2.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok((
            T0::decode(reader)?,
            T1::decode(reader)?,
            T2::decode(reader)?,
        ))
    }
}

impl<T0: LsmDiskCodec, T1: LsmDiskCodec, T2: LsmDiskCodec, T3: LsmDiskCodec> LsmDiskCodec
    for (T0, T1, T2, T3)
{
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.encode(writer)?;
        self.1.encode(writer)?;
        self.2.encode(writer)?;
        self.3.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok((
            T0::decode(reader)?,
            T1::decode(reader)?,
            T2::decode(reader)?,
            T3::decode(reader)?,
        ))
    }
}

impl<T0: LsmDiskCodec, T1: LsmDiskCodec, T2: LsmDiskCodec, T3: LsmDiskCodec, T4: LsmDiskCodec>
    LsmDiskCodec for (T0, T1, T2, T3, T4)
{
    fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.0.encode(writer)?;
        self.1.encode(writer)?;
        self.2.encode(writer)?;
        self.3.encode(writer)?;
        self.4.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok((
            T0::decode(reader)?,
            T1::decode(reader)?,
            T2::decode(reader)?,
            T3::decode(reader)?,
            T4::decode(reader)?,
        ))
    }
}

/// Per-run metadata. Crucially this holds **no keys**: keys live only in the run
/// file, so memory stays O(1) per run regardless of key size or count. Lookups
/// binary-search the on-disk offset index (see [`DiskLsmMultiMap::find_entry`]).
#[derive(Clone, Debug)]
struct DiskRun<K> {
    path: PathBuf,
    /// Number of distinct keys (entries) in the run.
    entries_count: u64,
    /// Absolute file offset where the dense `u64` offset index begins.
    index_start: u64,
    /// Total number of values across all keys.
    len: usize,
    _marker: PhantomData<K>,
}

/// Streams a single run to disk with roughly constant memory. Entry offsets are
/// spilled to a scratch `.idx.tmp` file as we go, then appended to the run on
/// [`finish`](RunWriter::finish), so no in-memory offset buffer is needed.
struct RunWriter {
    data: CountingWriter<BufWriter<fs::File>>,
    idx: BufWriter<fs::File>,
    data_path: PathBuf,
    idx_path: PathBuf,
    entries_count: u64,
    values_len: u64,
}

/// A `Write` adapter that tracks the number of bytes written, giving us the
/// current absolute file offset without flushing the underlying `BufWriter`.
struct CountingWriter<W> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl RunWriter {
    fn new(data_path: PathBuf) -> io::Result<Self> {
        let idx_path = data_path.with_extension("idx.tmp");
        let mut data = BufWriter::new(fs::File::create(&data_path)?);
        data.write_all(RUN_MAGIC)?;
        write_u64(&mut data, 0)?; // entries_count placeholder, patched in finish
        let data = CountingWriter {
            inner: data,
            count: RUN_HEADER_LEN,
        };
        let idx = BufWriter::new(fs::File::create(&idx_path)?);
        Ok(Self {
            data,
            idx,
            data_path,
            idx_path,
            entries_count: 0,
            values_len: 0,
        })
    }

    /// Appends one sorted entry. Callers must push keys in ascending order.
    fn push<K: LsmDiskCodec, V: LsmDiskCodec>(&mut self, key: &K, values: &[V]) -> io::Result<()> {
        let entry_offset = self.data.count;
        write_u64(&mut self.idx, entry_offset)?;
        key.encode(&mut self.data)?;
        write_u64(&mut self.data, values.len() as u64)?;
        for value in values {
            value.encode(&mut self.data)?;
        }
        self.entries_count += 1;
        self.values_len += values.len() as u64;
        Ok(())
    }

    fn finish<K>(self) -> io::Result<DiskRun<K>> {
        let RunWriter {
            data,
            mut idx,
            data_path,
            idx_path,
            entries_count,
            values_len,
        } = self;
        let index_start = data.count;

        // Flush both streams and recover the raw data file handle.
        idx.flush()?;
        drop(idx);
        let mut data_buf = data.inner;
        data_buf.flush()?;
        let mut data_file = data_buf.into_inner().map_err(|e| e.into_error())?;

        // Append the spilled offset index, then the fixed footer.
        let mut idx_file = fs::File::open(&idx_path)?;
        io::copy(&mut idx_file, &mut data_file)?;
        write_u64(&mut data_file, index_start)?;
        write_u64(&mut data_file, values_len)?;

        // Patch the entries_count placeholder in the header.
        data_file.seek(io::SeekFrom::Start(RUN_MAGIC.len() as u64))?;
        write_u64(&mut data_file, entries_count)?;
        data_file.sync_data()?;
        let _ = fs::remove_file(&idx_path);

        Ok(DiskRun {
            path: data_path,
            entries_count,
            index_start,
            len: values_len as usize,
            _marker: PhantomData,
        })
    }
}

/// Sequential, forward-only reader over a run's entries in ascending key order.
/// Holds at most one decoded entry at a time, so it supports constant-memory
/// streaming scans and k-way merges.
struct RunScanner<K, V> {
    reader: BufReader<fs::File>,
    remaining: u64,
    _marker: PhantomData<(K, V)>,
}

impl<K: LsmDiskCodec, V: LsmDiskCodec> RunScanner<K, V> {
    fn open<RK>(run: &DiskRun<RK>) -> io::Result<Self> {
        let mut file = fs::File::open(&run.path)?;
        file.seek(io::SeekFrom::Start(RUN_HEADER_LEN))?;
        Ok(Self {
            reader: BufReader::new(file),
            remaining: run.entries_count,
            _marker: PhantomData,
        })
    }

    fn next_entry(&mut self) -> io::Result<Option<(K, Vec<V>)>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let key = K::decode(&mut self.reader)?;
        let values_count = read_u64(&mut self.reader)?;
        let mut values = Vec::with_capacity(values_count as usize);
        for _ in 0..values_count {
            values.push(V::decode(&mut self.reader)?);
        }
        self.remaining -= 1;
        Ok(Some((key, values)))
    }
}

/// One input stream during a merge: its scanner plus the currently buffered head
/// entry (the next entry to be consumed, or `None` once exhausted).
struct MergeCursor<K, V> {
    scanner: RunScanner<K, V>,
    head: Option<(K, Vec<V>)>,
}

/// A disk-backed LSM multi-map. Immutable runs are stored as files; memory only
/// keeps the active memtable plus each run's key/offset index.
#[derive(Debug)]
pub struct DiskLsmMultiMap<K, V> {
    memtable: BTreeMap<K, Vec<V>>,
    levels: Vec<Vec<DiskRun<K>>>,
    memtable_len: usize,
    len: usize,
    /// After a write, if this limit is exceeded, flush the memtable to disk.
    memtable_limit: usize,
    /// Max number of on-disk tables per level.
    level_fanout: usize,
    dir: PathBuf,
    next_run_id: u64,
    /// Whether `dir` is a throwaway temp directory we own and should delete on
    /// drop. `false` for directories opened via [`Self::open`], which may hold
    /// durable user data.
    is_temp: bool,
    /// Whether `dir` has actually been created on disk yet. The temp-directory
    /// constructors defer the `create_dir_all` syscall until the first flush, so
    /// a map that never spills to disk touches the filesystem zero times.
    dir_created: bool,
}

impl<K, V> Drop for DiskLsmMultiMap<K, V> {
    fn drop(&mut self) {
        // Only remove directories we created and own; never touch the filesystem
        // for a temp map that never flushed.
        if self.is_temp && self.dir_created {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

impl<K, V> Default for DiskLsmMultiMap<K, V>
where
    K: Ord + Clone + LsmDiskCodec,
    V: Clone + LsmDiskCodec,
{
    fn default() -> Self {
        Self::with_memtable_limit_and_level_fanout(
            DEFAULT_DISK_MEMTABLE_LIMIT,
            DEFAULT_LEVEL_FANOUT,
        )
    }
}

impl<K, V> DiskLsmMultiMap<K, V>
where
    K: Ord + Clone + LsmDiskCodec,
    V: Clone + LsmDiskCodec,
{
    /// Creates a disk-backed LSM in a unique temporary directory. This is the
    /// constructor used by the Ascent disk provider.
    pub fn with_memtable_limit(memtable_limit: usize) -> Self {
        Self::with_memtable_limit_and_level_fanout(memtable_limit, DEFAULT_LEVEL_FANOUT)
    }

    /// Creates a disk-backed LSM in a unique temporary directory with explicit
    /// write-buffer and per-level run fanout limits.
    pub fn with_memtable_limit_and_level_fanout(
        memtable_limit: usize,
        level_fanout: usize,
    ) -> Self {
        let id = NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lsm-disk-{}-{id}", std::process::id()));
        // Lazily created: no directory is touched on disk until the first flush.
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

    /// Opens or creates a disk-backed LSM in `dir`. Existing `.lsm` run files
    /// are scanned to rebuild the in-memory key/offset indexes.
    pub fn open(dir: impl Into<PathBuf>, memtable_limit: usize) -> io::Result<Self> {
        Self::open_with_level_fanout(dir, memtable_limit, DEFAULT_LEVEL_FANOUT)
    }

    /// Opens or creates a disk-backed LSM in `dir` with an explicit per-level
    /// run fanout. Existing `.lsm` run files are scanned to rebuild the
    /// in-memory key/offset indexes.
    pub fn open_with_level_fanout(
        dir: impl Into<PathBuf>,
        memtable_limit: usize,
        level_fanout: usize,
    ) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;

        let mut levels: Vec<Vec<DiskRun<K>>> = Vec::new();
        let mut len = 0;
        let mut next_run_id = 0;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("lsm") {
                continue;
            }
            let Some((level, id)) = parse_run_file_name(&path) else {
                continue;
            };
            let run = Self::load_run(path)?;
            len += run.len;
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

    /// Appends a value to the in-memory write buffer and flushes it as a disk
    /// run when the buffer is full.
    pub fn insert(&mut self, key: K, value: V) {
        self.memtable.entry(key).or_default().push(value);
        self.memtable_len += 1;
        self.len += 1;
        if self.memtable_len >= self.memtable_limit {
            self.flush().expect("failed to flush disk LSM memtable");
        }
    }

    /// Persists the current memtable as a disk run. Call this before reopening a
    /// disk LSM if the write buffer has not reached its automatic flush limit.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_memtable()
    }

    /// Reads all values for `key`. Matching values in disk runs are decoded from
    /// file payloads on demand rather than cached in memory.
    pub fn get(&self, key: &K) -> io::Result<Vec<V>> {
        let mut values = Vec::new();
        if let Some(mem_values) = self.memtable.get(key) {
            values.extend(mem_values.iter().cloned());
        }
        for run in self.levels.iter().flatten() {
            values.extend(Self::read_run_values(run, key)?);
        }
        Ok(values)
    }

    /// Checks membership by probing the memtable, then binary-searching each
    /// run's on-disk offset index. No keys are held in memory.
    pub fn contains_key(&self, key: &K) -> bool {
        self.memtable.contains_key(key)
            || self.levels.iter().flatten().any(|run| {
                Self::find_entry(run, key)
                    .map(|entry| entry.is_some())
                    .unwrap_or(false)
            })
    }

    /// Returns all grouped entries. This intentionally decodes payloads from
    /// disk each time; it is used by Ascent's all-index iteration path.
    pub fn iter_all_owned(&self) -> io::Result<Vec<(K, Vec<V>)>> {
        let mut all = Vec::new();
        all.extend(self.memtable.iter().map(|(k, v)| (k.clone(), v.clone())));
        for run in self.levels.iter().flatten() {
            all.extend(Self::read_run_all(run)?);
        }
        Ok(all)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrowing view of the values for `key` **in the memtable only** — flushed
    /// runs are not consulted. This is the zero-copy read path: it hands back
    /// references straight into the memtable rather than decoding owned values
    /// like [`get`](Self::get). It is therefore only a *complete* view while
    /// nothing has spilled to disk; callers that need run data too must use
    /// `get`/`iter_all_owned` (or the future mmap-backed borrowing path).
    pub fn memtable_get(&self, key: &K) -> Option<std::slice::Iter<'_, V>> {
        debug_assert!(
            self.levels.iter().all(|level| level.is_empty()),
            "memtable_get ignores flushed runs; data has spilled to disk"
        );
        self.memtable.get(key).map(|values| values.iter())
    }

    /// Borrowing iterator over every `(key, values)` group in the memtable only
    /// (flushed runs are not consulted; see [`memtable_get`](Self::memtable_get)).
    pub fn memtable_iter(&self) -> std::collections::btree_map::Iter<'_, K, Vec<V>> {
        debug_assert!(
            self.levels.iter().all(|level| level.is_empty()),
            "memtable_iter ignores flushed runs; data has spilled to disk"
        );
        self.memtable.iter()
    }

    /// Removes and returns the entire memtable, leaving any flushed run levels
    /// intact. The value count is decremented by the drained memtable size.
    pub fn take_memtable(&mut self) -> BTreeMap<K, Vec<V>> {
        let memtable = std::mem::take(&mut self.memtable);
        self.len -= self.memtable_len;
        self.memtable_len = 0;
        memtable
    }

    /// Creates the backing directory on first use. Temp-directory maps defer this
    /// syscall until they actually need to spill a run to disk.
    fn ensure_dir(&mut self) -> io::Result<()> {
        if !self.dir_created {
            fs::create_dir_all(&self.dir)?;
            self.dir_created = true;
        }
        Ok(())
    }

    /// Drops all entries, deleting any on-disk runs, while keeping the map usable
    /// (and its backing directory, if already created). Used to empty an index in
    /// place without reallocating a fresh disk-backed map.
    pub fn clear(&mut self) {
        self.memtable.clear();
        for run in std::mem::take(&mut self.levels).into_iter().flatten() {
            let _ = fs::remove_file(run.path);
        }
        self.memtable_len = 0;
        self.len = 0;
    }

    fn flush_memtable(&mut self) -> io::Result<()> {
        if self.memtable_len == 0 {
            return Ok(());
        }
        self.ensure_dir()?;
        let memtable = std::mem::take(&mut self.memtable);
        let run = Run {
            entries: memtable.into_iter().rev().collect(),
        };
        self.memtable_len = 0;
        self.push_run(0, run)
    }

    fn push_run(&mut self, level: usize, run: Run<K, V>) -> io::Result<()> {
        let disk_run = self.write_run(level, run)?;
        self.push_disk_run(level, disk_run)
    }

    /// Places an already-written run at `level`, compacting (and cascading to the
    /// next level) once the level exceeds its fanout.
    fn push_disk_run(&mut self, level: usize, disk_run: DiskRun<K>) -> io::Result<()> {
        if self.levels.len() <= level {
            self.levels.resize_with(level + 1, Vec::new);
        }
        self.levels[level].push(disk_run);
        if self.levels[level].len() > self.level_fanout {
            let runs = std::mem::take(&mut self.levels[level]);
            let merged = self.merge_disk_runs(level + 1, runs)?;
            self.push_disk_run(level + 1, merged)?;
        }
        Ok(())
    }

    fn write_run(&mut self, level: usize, run: Run<K, V>) -> io::Result<DiskRun<K>> {
        let path = self.new_run_path(level);
        let mut writer = RunWriter::new(path)?;
        // `run.entries` is stored max-to-min; `.rev()` yields ascending order.
        for (key, values) in run.entries.into_iter().rev() {
            writer.push(&key, &values)?;
        }
        writer.finish()
    }

    fn new_run_path(&mut self, level: usize) -> PathBuf {
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.dir.join(format!("run_{level}_{id}.lsm"))
    }

    fn load_run(path: PathBuf) -> io::Result<DiskRun<K>> {
        let mut file = fs::File::open(&path)?;
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic)?;
        if &magic != RUN_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid LSM run magic",
            ));
        }
        let entries_count = read_u64(&mut file)?;
        // Read the fixed footer; no full scan, so keys never enter memory.
        file.seek(io::SeekFrom::End(-RUN_FOOTER_LEN))?;
        let index_start = read_u64(&mut file)?;
        let values_len = read_u64(&mut file)?;
        Ok(DiskRun {
            path,
            entries_count,
            index_start,
            len: values_len as usize,
            _marker: PhantomData,
        })
    }

    /// Binary-searches the on-disk offset index for `key`. On a hit, returns the
    /// open file positioned immediately after the key (i.e. at `values_count`).
    fn find_entry(run: &DiskRun<K>, key: &K) -> io::Result<Option<fs::File>> {
        if run.entries_count == 0 {
            return Ok(None);
        }
        let mut file = fs::File::open(&run.path)?;
        let (mut lo, mut hi) = (0_u64, run.entries_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            file.seek(io::SeekFrom::Start(run.index_start + mid * 8))?;
            let entry_offset = read_u64(&mut file)?;
            file.seek(io::SeekFrom::Start(entry_offset))?;
            let mid_key = K::decode(&mut file)?;
            match mid_key.cmp(key) {
                CmpOrdering::Less => lo = mid + 1,
                CmpOrdering::Greater => hi = mid,
                CmpOrdering::Equal => return Ok(Some(file)),
            }
        }
        Ok(None)
    }

    fn read_run_values(run: &DiskRun<K>, key: &K) -> io::Result<Vec<V>> {
        let Some(mut file) = Self::find_entry(run, key)? else {
            return Ok(Vec::new());
        };
        let values_count = read_u64(&mut file)?;
        let mut values = Vec::with_capacity(values_count as usize);
        for _ in 0..values_count {
            values.push(V::decode(&mut file)?);
        }
        Ok(values)
    }

    fn read_run_all(run: &DiskRun<K>) -> io::Result<Vec<(K, Vec<V>)>> {
        let mut scanner = RunScanner::<K, V>::open(run)?;
        let mut all = Vec::with_capacity(run.entries_count as usize);
        while let Some(entry) = scanner.next_entry()? {
            all.push(entry);
        }
        Ok(all)
    }

    /// Streaming k-way merge of sorted runs into a single run at `level`, using
    /// memory proportional to the number of input runs (not the data size).
    /// Values for equal keys are concatenated in input-run order.
    fn merge_disk_runs(&mut self, level: usize, runs: Vec<DiskRun<K>>) -> io::Result<DiskRun<K>> {
        let path = self.new_run_path(level);
        let mut writer = RunWriter::new(path)?;

        let mut cursors: Vec<MergeCursor<K, V>> = Vec::with_capacity(runs.len());
        for run in &runs {
            let mut scanner = RunScanner::<K, V>::open(run)?;
            let head = scanner.next_entry()?;
            cursors.push(MergeCursor { scanner, head });
        }

        loop {
            // Smallest buffered head key across all cursors.
            let Some(min_key) = cursors
                .iter()
                .filter_map(|cursor| cursor.head.as_ref().map(|(key, _)| key))
                .min()
                .cloned()
            else {
                break;
            };

            // Drain every cursor whose head equals `min_key`, in run order, so
            // value concatenation is deterministic.
            let mut merged_values = Vec::new();
            for cursor in cursors.iter_mut() {
                if cursor.head.as_ref().is_some_and(|(key, _)| *key == min_key) {
                    let (_, mut values) = cursor.head.take().unwrap();
                    merged_values.append(&mut values);
                    cursor.head = cursor.scanner.next_entry()?;
                }
            }
            writer.push(&min_key, &merged_values)?;
        }

        let merged = writer.finish()?;
        for run in runs {
            let _ = fs::remove_file(run.path);
        }
        Ok(merged)
    }

    pub fn drain_all_into(&mut self, target: &mut Self) -> io::Result<()> {
        let memtable = std::mem::take(&mut self.memtable);
        for (key, values) in memtable {
            for value in values {
                target.insert(key.clone(), value);
            }
        }

        let levels = std::mem::take(&mut self.levels);
        for run in levels.into_iter().flatten() {
            let mut scanner = RunScanner::<K, V>::open(&run)?;
            while let Some((key, values)) = scanner.next_entry()? {
                for value in values {
                    target.insert(key.clone(), value);
                }
            }
            let _ = fs::remove_file(run.path);
        }

        self.memtable_len = 0;
        self.len = 0;
        Ok(())
    }
}

impl<K> DiskLsmMultiMap<K, ()>
where
    K: Ord + Clone + LsmDiskCodec,
{
    pub fn drain_keys_into(&mut self, target: &mut Self) -> io::Result<()> {
        let memtable = std::mem::take(&mut self.memtable);
        for (key, values) in memtable {
            if !values.is_empty() {
                target.insert(key, ());
            }
        }

        let levels = std::mem::take(&mut self.levels);
        for run in levels.into_iter().flatten() {
            let mut scanner = RunScanner::<K, ()>::open(&run)?;
            while let Some((key, _)) = scanner.next_entry()? {
                target.insert(key, ());
            }
            let _ = fs::remove_file(run.path);
        }

        self.memtable_len = 0;
        self.len = 0;
        Ok(())
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

fn write_u64<W: Write>(writer: &mut W, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn disk_lsm_writes_reads_and_reopens_runs() {
        let dir = std::env::temp_dir().join(format!(
            "lsm-disk-test-{}-{}",
            std::process::id(),
            NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));

        {
            let mut lsm = DiskLsmMultiMap::<(i32,), (i32,)>::open(&dir, 2).unwrap();
            lsm.insert((1,), (2,));
            lsm.insert((9,), (10,));
            lsm.insert((1,), (3,));
            assert_eq!(
                lsm.get(&(1,)).unwrap().into_iter().collect::<BTreeSet<_>>(),
                BTreeSet::from([(2,), (3,)])
            );
            lsm.flush().unwrap();
        }

        let reopened = DiskLsmMultiMap::<(i32,), (i32,)>::open(&dir, 2).unwrap();
        assert_eq!(
            reopened
                .get(&(1,))
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([(2,), (3,)])
        );
        assert_eq!(reopened.get(&(404,)).unwrap(), Vec::<(i32,)>::new());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_lsm_level_fanout_is_configurable() {
        let dir = std::env::temp_dir().join(format!(
            "lsm-disk-fanout-test-{}-{}",
            std::process::id(),
            NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));

        let mut lsm =
            DiskLsmMultiMap::<(i32,), (i32,)>::open_with_level_fanout(&dir, 1, 1).unwrap();
        lsm.insert((1,), (10,));
        assert_eq!(lsm.levels[0].len(), 1);

        lsm.insert((2,), (20,));
        assert_eq!(lsm.levels[0].len(), 0);
        assert_eq!(lsm.levels[1].len(), 1);
        assert_eq!(lsm.get(&(1,)).unwrap(), vec![(10,)]);
        assert_eq!(lsm.get(&(2,)).unwrap(), vec![(20,)]);

        let _ = fs::remove_dir_all(dir);
    }

    fn test_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lsm-disk-{tag}-{}-{}",
            std::process::id(),
            NEXT_DISK_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// After reopening, a run keeps no keys in memory; lookups must work purely
    /// from the on-disk offset index and footer.
    #[test]
    fn disk_lsm_reopen_reads_only_footer() {
        let dir = test_dir("footer");
        {
            // A high limit keeps all six writes in one flushed run.
            let mut lsm = DiskLsmMultiMap::<(i32,), (i32,)>::open(&dir, 16).unwrap();
            for key in 0..6 {
                lsm.insert((key,), (key * 10,));
            }
            lsm.flush().unwrap();
        }

        let reopened = DiskLsmMultiMap::<(i32,), (i32,)>::open(&dir, 16).unwrap();
        // No keys are cached: the run only stores entries_count / index_start / len.
        let run = &reopened.levels[0][0];
        assert_eq!(run.entries_count, 6);
        assert_eq!(run.len, 6);
        assert!(run.index_start > RUN_HEADER_LEN);

        assert_eq!(reopened.len(), 6);
        assert_eq!(reopened.get(&(0,)).unwrap(), vec![(0,)]);
        assert_eq!(reopened.get(&(5,)).unwrap(), vec![(50,)]);
        assert_eq!(reopened.get(&(3,)).unwrap(), vec![(30,)]);
        assert!(reopened.contains_key(&(4,)));
        assert!(!reopened.contains_key(&(99,)));
        assert_eq!(reopened.get(&(99,)).unwrap(), Vec::<(i32,)>::new());

        let _ = fs::remove_dir_all(dir);
    }

    /// Streaming merge across several overlapping runs must concatenate values
    /// per key and preserve the total value count.
    #[test]
    fn disk_lsm_streaming_merge_combines_overlapping_runs() {
        let dir = test_dir("merge");
        // memtable_limit=1 flushes every insert into its own run; fanout=3
        // triggers a multi-way merge once four runs accumulate.
        let mut lsm =
            DiskLsmMultiMap::<(i32,), (i32,)>::open_with_level_fanout(&dir, 1, 3).unwrap();

        let writes = [
            ((3,), (30,)),
            ((1,), (10,)),
            ((3,), (31,)),
            ((2,), (20,)),
            ((1,), (11,)),
            ((4,), (40,)),
            ((2,), (21,)),
            ((1,), (12,)),
        ];
        for (key, value) in writes {
            lsm.insert(key, value);
        }
        lsm.flush().unwrap();

        assert_eq!(lsm.len(), writes.len());
        assert_eq!(
            lsm.get(&(1,)).unwrap().into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([(10,), (11,), (12,)])
        );
        assert_eq!(
            lsm.get(&(2,)).unwrap().into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([(20,), (21,)])
        );
        assert_eq!(
            lsm.get(&(3,)).unwrap().into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([(30,), (31,)])
        );
        assert_eq!(lsm.get(&(4,)).unwrap(), vec![(40,)]);

        // Everything round-trips through iter_all_owned as well.
        let total: usize = lsm
            .iter_all_owned()
            .unwrap()
            .iter()
            .map(|(_, values)| values.len())
            .sum();
        assert_eq!(total, writes.len());

        let _ = fs::remove_dir_all(dir);
    }

    /// Drives enough distinct keys through small fanout to force compaction into
    /// higher levels, then checks every key/value survived and reopens cleanly.
    #[test]
    fn disk_lsm_multi_level_compaction_round_trips() {
        let dir = test_dir("multilevel");
        const KEYS: i64 = 200;
        {
            let mut lsm =
                DiskLsmMultiMap::<(i64,), (i64,)>::open_with_level_fanout(&dir, 4, 2).unwrap();
            for key in 0..KEYS {
                lsm.insert((key,), (key + 1000,));
                lsm.insert((key,), (key + 2000,));
            }
            lsm.flush().unwrap();
            // Compaction should have promoted runs beyond level 0.
            assert!(lsm.levels.len() > 1);
        }

        let reopened =
            DiskLsmMultiMap::<(i64,), (i64,)>::open_with_level_fanout(&dir, 4, 2).unwrap();
        assert_eq!(reopened.len() as i64, KEYS * 2);
        for key in 0..KEYS {
            assert_eq!(
                reopened
                    .get(&(key,))
                    .unwrap()
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([(key + 1000,), (key + 2000,)])
            );
        }

        let _ = fs::remove_dir_all(dir);
    }
}
