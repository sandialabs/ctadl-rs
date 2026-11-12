/*!
The stage directory, `<index>/staging/`, and the writes that make its files safe to resume
from.

Its layout:

| Path | Holds |
| --- | --- |
| `facts/` | the program's facts, one file per relation |
| `paths.parquet` | the program-wide path set |
| `path_set.bin` | the same set frozen, with its splits, for workers to map |
| `plan.json` | the plan of batches |
| `config.json` | the run's [`KeyConfig`] |
| `key` | the [`RunKey`] of the facts and configuration the files were made from |
| `batches/<id>/` | a batch's descriptor and outputs |
| `rounds/<scc>/` | a split SCC's [`LatestRounds`] and, under `<batch>/<round>/`, each part's latest run |
| `frame/` | the global frame's edges |
| `intent/` | the intent stage's outputs |
| `done/` | a marker per completed [`Stage`] |

Every file is written by [`write_atomic`], so it is complete or absent. A stage's marker is
written the same way after all of its files, so a marker vouches for every file of its stage.
*/

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorContext};
use crate::index_engine::{IndexConfig, Parallelism, Storage, index_timeout_secs};
use crate::staged::plan::{Batch, Plan};

/// A unit of work whose completion is marked in `done/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// The facts, path set and plan are written.
    Facts,
    /// A batch of the plan. A part of a split SCC is done when its SCC is.
    Batch(u32),
    /// A run of a part of a split SCC in a round.
    Round { batch: u32, round: u32 },
    /// The global frame.
    Frame,
    /// The intent rules.
    Intent,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stage::Facts => write!(f, "facts"),
            Stage::Batch(id) => write!(f, "batch-{id}"),
            Stage::Round { batch, round } => write!(f, "round-{batch}-{round}"),
            Stage::Frame => write!(f, "frame"),
            Stage::Intent => write!(f, "intent"),
        }
    }
}

/// The stage directory of one index directory.
#[derive(Clone, Debug)]
pub struct StageDir {
    root: PathBuf,
}

impl StageDir {
    /// The stage directory's name in the index directory.
    pub const NAME: &str = "staging";

    /// The stage directory of `index_dir`. Nothing is created.
    pub fn new(index_dir: impl AsRef<Path>) -> Self {
        Self {
            root: index_dir.as_ref().join(Self::NAME),
        }
    }

    /// The stage directory whose root is `root`, as [`StageDir::root`] names it.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Creates the directory and its subdirectories, if they are missing.
    pub fn create(&self) -> Result<(), Error> {
        for dir in [
            self.facts_dir(),
            self.root.join("batches"),
            self.rounds_dir(),
            self.frame_dir(),
            self.intent_dir(),
            self.done_dir(),
            self.logs_dir(),
        ] {
            fs::create_dir_all(&dir)
                .map_err(Error::from)
                .err_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn facts_dir(&self) -> PathBuf {
        self.root.join("facts")
    }

    pub fn paths_file(&self) -> PathBuf {
        self.root.join("paths.parquet")
    }

    /// The frozen path set that [`PathSet::write_file`] writes and a worker maps. The run key does
    /// not cover it: it is derived from the facts, which the key covers.
    ///
    /// [`PathSet::write_file`]: crate::index_engine::path_set::PathSet::write_file
    pub fn path_set_file(&self) -> PathBuf {
        self.root.join("path_set.bin")
    }

    pub fn plan_file(&self) -> PathBuf {
        self.root.join("plan.json")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn key_file(&self) -> PathBuf {
        self.root.join("key")
    }

    pub fn batch_dir(&self, batch: u32) -> PathBuf {
        self.root.join("batches").join(batch.to_string())
    }

    pub fn rounds_dir(&self) -> PathBuf {
        self.root.join("rounds")
    }

    /// Where split SCC `scc` keeps its rounds.
    pub fn scc_dir(&self, scc: u32) -> PathBuf {
        self.rounds_dir().join(scc.to_string())
    }

    /// The outputs of the run of part `batch` of split SCC `scc` in `round`.
    pub fn round_dir(&self, scc: u32, batch: u32, round: u32) -> PathBuf {
        self.scc_dir(scc)
            .join(batch.to_string())
            .join(round.to_string())
    }

    pub fn frame_dir(&self) -> PathBuf {
        self.root.join("frame")
    }

    pub fn intent_dir(&self) -> PathBuf {
        self.root.join("intent")
    }

    pub fn done_dir(&self) -> PathBuf {
        self.root.join("done")
    }

    /// Where worker processes write their logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The marker file of `stage`.
    pub fn marker(&self, stage: Stage) -> PathBuf {
        self.done_dir().join(stage.to_string())
    }

    /// Marks `stage` done. Call it only once every file of the stage is in place.
    pub fn mark_done(&self, stage: Stage) -> Result<(), Error> {
        write_atomic(&self.marker(stage), |tmp| {
            fs::write(tmp, b"").map_err(Error::from)
        })
    }

    /// Whether `stage` was marked done.
    pub fn is_done(&self, stage: Stage) -> bool {
        self.marker(stage).is_file()
    }

    /// Removes `stage`'s marker, if it has one. Call it before removing any file of the stage.
    pub fn unmark(&self, stage: Stage) -> Result<(), Error> {
        let marker = self.marker(stage);
        match fs::remove_file(&marker) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(Error::from(e)).err_context(|| format!("removing {}", marker.display()))
            }
            _ => Ok(()),
        }
    }

    /// Removes what a run of `plan` killed midway left that no marker vouches for, so that a
    /// run resuming from the directory reads only files of stages that are done:
    /// - temporary paths;
    /// - the outputs of every batch that is not done (its descriptor, written with the plan,
    ///   stays);
    /// - every run of a part of a split SCC but the latest that [`LatestRounds`] names, and the
    ///   rounds of a split SCC whose parts are all done;
    /// - the global frame's and the intent stage's files, if those stages are not done.
    pub fn discard_unmarked(&self, plan: &Plan) -> Result<(), Error> {
        remove_temporaries(&self.root)?;
        for batch in &plan.batches {
            if !self.is_done(Stage::Batch(batch.id)) {
                let descriptor = Batch::descriptor_file(self, batch.id);
                clear_dir(&self.batch_dir(batch.id), |path| path != descriptor)?;
            }
        }
        for (scc, split) in plan.split_sccs.iter().enumerate() {
            self.discard_stale_rounds(scc as u32, &split.parts)?;
        }
        if !self.is_done(Stage::Frame) {
            clear_dir(&self.frame_dir(), |_| true)?;
        }
        if !self.is_done(Stage::Intent) {
            clear_dir(&self.intent_dir(), |_| true)?;
        }
        Ok(())
    }

    /// Removes what a killed run of `batch` left, in `round` if it is a part of a split SCC:
    /// its outputs, whole or partly written. Call it only if the run left no marker.
    pub fn discard_run(&self, batch: &Batch, round: Option<u32>) -> Result<(), Error> {
        match (batch.part, round) {
            (Some(part), Some(round)) => {
                let dir = self.round_dir(part.scc, batch.id, round);
                if dir.exists() {
                    remove_path(&dir)?;
                }
                // A partly written run is a temporary directory beside it.
                let parent = dir.parent().expect("a round has a part directory");
                clear_dir(parent, is_temporary)
            }
            _ => {
                let descriptor = Batch::descriptor_file(self, batch.id);
                clear_dir(&self.batch_dir(batch.id), |path| path != descriptor)
            }
        }
    }

    /// Keeps, of split SCC `scc` with `parts`, only the latest done run of each part that is not
    /// itself done, and their markers.
    fn discard_stale_rounds(&self, scc: u32, parts: &[u32]) -> Result<(), Error> {
        let dir = self.scc_dir(scc);
        let mut latest = LatestRounds::load(self, scc)?;
        // A part's latest run is marked before `latest.json` names it. A part that is done had
        // its run moved to its batch.
        let named = latest.latest.len();
        latest.latest.retain(|&batch, &mut round| {
            self.is_done(Stage::Batch(batch)) || self.is_done(Stage::Round { batch, round })
        });
        if latest.latest.len() != named {
            latest.save(self, scc)?;
        }
        let all_done = parts.iter().all(|b| self.is_done(Stage::Batch(*b)));
        let keep = |batch: u32, round: u32| {
            !all_done
                && !self.is_done(Stage::Batch(batch))
                && latest.latest.get(&batch) == Some(&round)
        };
        for &batch in parts {
            for round in self.marked_rounds(batch)? {
                if !keep(batch, round) {
                    self.unmark(Stage::Round { batch, round })?;
                }
            }
            let part_dir = dir.join(batch.to_string());
            clear_dir(&part_dir, |path| {
                let round = path.file_name().and_then(|n| n.to_str()?.parse().ok());
                round.is_none_or(|round| !keep(batch, round))
            })?;
        }
        if all_done && dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(Error::from)
                .err_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }

    /// The rounds of part `batch` that have a marker.
    fn marked_rounds(&self, batch: u32) -> Result<Vec<u32>, Error> {
        let prefix = format!("round-{batch}-");
        let mut rounds = Vec::new();
        for entry in read_dir_if_exists(&self.done_dir())? {
            let name = entry.file_name();
            if let Some(round) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) {
                rounds.extend(round.parse::<u32>().ok());
            }
        }
        Ok(rounds)
    }
}

/// The entries of `dir`, none if it is missing.
fn read_dir_if_exists(dir: &Path) -> Result<Vec<fs::DirEntry>, Error> {
    let context = || format!("reading {}", dir.display());
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .collect::<Result<_, _>>()
            .map_err(Error::from)
            .err_context(context),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::from(e)).err_context(context),
    }
}

/// Removes each entry of `dir`, file or directory, whose path `remove` accepts.
fn clear_dir(dir: &Path, remove: impl Fn(&Path) -> bool) -> Result<(), Error> {
    for entry in read_dir_if_exists(dir)? {
        let path = entry.path();
        if remove(&path) {
            remove_path(&path)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<(), Error> {
    let removed = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    removed
        .map_err(Error::from)
        .err_context(|| format!("removing {}", path.display()))
}

/// Whether `path` is a temporary path of [`write_atomic`].
fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
}

/// Removes every temporary path [`write_atomic`] left under `dir`.
fn remove_temporaries(dir: &Path) -> Result<(), Error> {
    for entry in read_dir_if_exists(dir)? {
        let path = entry.path();
        if is_temporary(&path) {
            remove_path(&path)?;
        } else if entry.file_type().map_err(Error::from)?.is_dir() {
            remove_temporaries(&path)?;
        }
    }
    Ok(())
}

/// The latest done run of each part of a split SCC that has run, by batch. Its parts read each
/// other's summaries from the runs it names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestRounds {
    pub latest: std::collections::BTreeMap<u32, u32>,
}

impl LatestRounds {
    fn file(stage: &StageDir, scc: u32) -> PathBuf {
        stage.scc_dir(scc).join("latest.json")
    }

    /// The latest rounds of split SCC `scc`: none, if none of its parts has run.
    pub fn load(stage: &StageDir, scc: u32) -> Result<Self, Error> {
        let path = Self::file(stage, scc);
        match fs::read(&path) {
            Ok(json) => serde_json::from_slice(&json).map_err(|e| Error::Context {
                context: format!("reading {}", path.display()),
                source: Box::new(e),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::from(e)).err_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, stage: &StageDir, scc: u32) -> Result<(), Error> {
        let path = Self::file(stage, scc);
        write_atomic(&path, |tmp| {
            fs::write(
                tmp,
                serde_json::to_vec(self).expect("latest rounds serialize"),
            )
            .map_err(Error::from)
            .err_context(|| format!("writing {}", path.display()))
        })
    }
}

/// Rows per row group of a stage file. A batch that reads one callee's rows from another's file
/// decodes at most a group of them beyond its own, rather than the default group's 64K.
pub const STAGE_ROWS_PER_GROUP: usize = 4096;

/// Numbers temporary names, so that two writers in one process never share one.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `path` all at once: `write` writes a temporary path beside it, a file or a
/// directory, which is then renamed to `path`. If `write` fails, the temporary path is removed
/// and `path` is left as it was. A process killed midway leaves at most a temporary path,
/// whose name starts with `.`.
///
/// `path`'s parent directory is created if it is missing. A directory can replace only a
/// missing or empty one.
pub fn write_atomic<T>(
    path: &Path,
    write: impl FnOnce(&Path) -> Result<T, Error>,
) -> Result<T, Error> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Error::FactsConvert(format!(
            "no file name in {}",
            path.display()
        )));
    };
    fs::create_dir_all(parent)
        .map_err(Error::from)
        .err_context(|| format!("creating {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write(&tmp).and_then(|value| {
        fs::rename(&tmp, path)
            .map_err(Error::from)
            .err_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
        Ok(value)
    });
    if result.is_err() {
        if tmp.is_dir() {
            let _ = fs::remove_dir_all(&tmp);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    result
}

/// The configuration a [`RunKey`] covers: everything that can change a staged run's plan or
/// tables. The engine is left out, since it changes neither. It is also what a stage reads to
/// configure its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyConfig {
    pub alias_rule: bool,
    pub deref_paths: bool,
    /// The memory budget, in bytes, which changes the plan.
    pub budget: u64,
    /// The batch floor, which changes the plan.
    pub floor: usize,
    /// `CTADL_INDEX_TIMEOUT_SECS` of the run, which every stage run's fixpoint stops at: a run
    /// with no timeout must not resume from batches that one stopped short.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl KeyConfig {
    /// The configuration of a run of `config`, `budget` and `floor`, with the timeout this
    /// process's environment sets.
    pub fn new(config: &IndexConfig, budget: u64, floor: usize) -> Self {
        let IndexConfig {
            alias_rule,
            parallelism: _,
            storage: _,
            deref_paths,
            staged: _,
        } = config;
        Self {
            alias_rule: *alias_rule,
            deref_paths: *deref_paths,
            budget,
            floor,
            timeout_secs: index_timeout_secs(),
        }
    }

    /// Writes the configuration to `stage`'s config file.
    pub fn save(&self, stage: &StageDir) -> Result<(), Error> {
        let path = stage.config_file();
        write_atomic(&path, |tmp| {
            fs::write(
                tmp,
                serde_json::to_vec(self).expect("a key config serializes"),
            )
            .map_err(Error::from)
            .err_context(|| format!("writing {}", path.display()))
        })
    }

    /// Reads the configuration [`KeyConfig::save`] wrote.
    pub fn load(stage: &StageDir) -> Result<Self, Error> {
        let path = stage.config_file();
        let json = fs::read(&path)
            .map_err(Error::from)
            .err_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&json).map_err(|e| Error::Context {
            context: format!("reading {}", path.display()),
            source: Box::new(e),
        })
    }

    /// The index configuration a stage runs under, on the engine of `parallelism`.
    pub fn index_config(&self, parallelism: Parallelism) -> IndexConfig {
        IndexConfig {
            alias_rule: self.alias_rule,
            parallelism,
            storage: Storage::Memory,
            deref_paths: self.deref_paths,
            staged: None,
        }
    }
}

/// The version of the stage directory's files, which the run key covers, so that a build does
/// not resume from a stage directory another build wrote in another format. Change it whenever
/// a stage file, the plan or a batch descriptor changes how it is written.
pub const STAGE_FORMAT: u32 = 1;

/// A SHA-256 over a stage directory's fact files and a [`KeyConfig`]. A run resumes from a stage
/// directory only if the key it stored equals the one of its own facts and configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunKey([u8; 32]);

impl RunKey {
    /// The key of the files in `facts_dir` and of `config`. The files are hashed in the order of
    /// their names, each with its name and length, and temporary files (names starting with
    /// `.`) are left out.
    pub fn new(facts_dir: &Path, config: &KeyConfig) -> Result<Self, Error> {
        let context = || format!("hashing the facts in {}", facts_dir.display());
        let mut files = Vec::new();
        for entry in fs::read_dir(facts_dir)
            .map_err(Error::from)
            .err_context(context)?
        {
            let entry = entry.map_err(Error::from).err_context(context)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !entry.file_type().map_err(Error::from)?.is_file() {
                return Err(Error::FactsConvert(format!(
                    "{} is not a fact file",
                    entry.path().display()
                )));
            }
            files.push(name);
        }
        files.sort_unstable();
        let mut hasher = Sha256::new();
        for name in files {
            let path = facts_dir.join(&name);
            let mut file = fs::File::open(&path)
                .map_err(Error::from)
                .err_context(context)?;
            let len = file.metadata().map_err(Error::from)?.len();
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update(len.to_le_bytes());
            io::copy(&mut file, &mut hasher)
                .map_err(Error::from)
                .err_context(|| format!("reading {}", path.display()))?;
        }
        let config = serde_json::to_vec(config).expect("a key config serializes");
        hasher.update((config.len() as u64).to_le_bytes());
        hasher.update(&config);
        hasher.update(STAGE_FORMAT.to_le_bytes());
        Ok(RunKey(hasher.finalize().into()))
    }

    /// Stores the key in `stage`'s key file.
    pub fn save(&self, stage: &StageDir) -> Result<(), Error> {
        write_atomic(&stage.key_file(), |tmp| {
            fs::write(tmp, self.to_string()).map_err(Error::from)
        })
    }

    /// The key stored in `stage`, if there is one that reads.
    pub fn load(stage: &StageDir) -> Option<Self> {
        fs::read_to_string(stage.key_file())
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl fmt::Display for RunKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl std::str::FromStr for RunKey {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        if s.len() != 64 || !s.is_ascii() {
            return Err(());
        }
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| ())?;
        }
        Ok(RunKey(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::parquet::{FunctionFilter, Reader, Writer};
    use crate::facts::schema::summary;
    use crate::facts::{FormalIndex, FunctionId, Path as FactPath, Root};

    fn summary_row(f: u32) -> summary::Record {
        let root = Root::formal(FormalIndex::new(0));
        let path = FactPath::from_symbol_names(["f"]);
        (FunctionId::new(f), root, path, root, FactPath::empty())
    }

    /// Rows of more keys than a file may have row groups share groups, as few as keep the file
    /// under the limit, and every row is kept.
    #[test]
    fn grouped_writer_stays_under_the_row_group_limit() {
        use crate::facts::parquet::MAX_KEYED_ROW_GROUPS;
        use crate::facts::schema::external_function;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(external_function::FILENAME);
        let n = MAX_KEYED_ROW_GROUPS * 2 + 1;
        let rows: Vec<_> = (0..n as u32).map(|f| (FunctionId::new(f),)).collect();
        Writer::new(&file)
            .rows_per_group(4096)
            .write_iter_grouped_by(&external_function::COLUMNS, rows.iter().copied(), |r| r.0)
            .unwrap();
        let reader = Reader::new(&file);
        assert!(reader.num_row_groups().unwrap() <= MAX_KEYED_ROW_GROUPS);
        let got: Vec<external_function::Record> =
            reader.read_vec(&external_function::COLUMNS).unwrap();
        assert_eq!(got, rows);
    }

    /// Three row groups, of functions 0-9, 20-29 and 40-49. A filter on functions of the second
    /// decodes only it, and gets only their rows. One between two of its functions decodes
    /// that group and gets nothing.
    #[test]
    fn filtered_reader_skips_row_groups() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(summary::FILENAME);
        let rows: Vec<_> = (0..3)
            .flat_map(|group| (0..10).map(move |i| summary_row(20 * group + i)))
            .collect();
        Writer::new(&file)
            .rows_per_group(4096)
            .write_iter_grouped_by(&summary::COLUMNS, rows, |row| row.0.id / 20)
            .unwrap();
        let reader = Reader::new(&file);
        assert_eq!(reader.num_row_groups().unwrap(), 3);

        let ids = [FunctionId::new(21), FunctionId::new(25)];
        let filter = FunctionFilter::new("func_id", &ids);
        assert_eq!(reader.row_groups_for(filter).unwrap(), vec![1]);
        let got: Vec<summary::Record> = reader
            .rows_of(&summary::COLUMNS, filter)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(got, vec![summary_row(21), summary_row(25)]);

        let mut batches = 0;
        reader
            .for_each_batch_of(&summary::COLUMNS, filter, |rows: Vec<summary::Record>| {
                batches += 1;
                assert_eq!(rows, vec![summary_row(21), summary_row(25)]);
                Ok(())
            })
            .unwrap();
        assert_eq!(batches, 1);

        let missing = [
            FunctionId::new(15),
            FunctionId::new(35),
            FunctionId::new(99),
        ];
        let filter = FunctionFilter::new("func_id", &missing);
        assert!(reader.row_groups_for(filter).unwrap().is_empty());
        let ids = [FunctionId::new(45)];
        let filter = FunctionFilter::new("func_id", &ids);
        assert_eq!(reader.row_groups_for(filter).unwrap(), vec![2]);
    }

    /// `Reader::rows` feeds `Writer::write_rows`, which copies a file group by group.
    #[test]
    fn rows_stream_into_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from.parquet");
        let to = dir.path().join("to.parquet");
        let rows: Vec<_> = (0..10).map(summary_row).collect();
        Writer::new(&from)
            .rows_per_group(3)
            .write_iter(&summary::COLUMNS, rows.clone())
            .unwrap();
        Writer::new(&to)
            .rows_per_group(3)
            .write_rows(
                &summary::COLUMNS,
                Reader::new(&from)
                    .rows::<summary::Record>(&summary::COLUMNS)
                    .unwrap(),
            )
            .unwrap();
        let reader = Reader::new(&to);
        assert_eq!(reader.num_row_groups().unwrap(), 4);
        assert_eq!(
            reader
                .read_vec::<summary::Record>(&summary::COLUMNS)
                .unwrap(),
            rows
        );
    }

    /// A write that fails leaves neither its file, nor a temporary one, nor the marker that
    /// would have followed it. One that succeeds leaves only the file and its marker.
    #[test]
    fn atomic_write_leaves_no_partial_file() {
        let index = tempfile::tempdir().unwrap();
        let stage = StageDir::new(index.path());
        stage.create().unwrap();
        let file = stage.batch_dir(0).join(summary::FILENAME);
        let write_batch = |fail: bool| -> Result<(), Error> {
            write_atomic(&file, |tmp| {
                Writer::new(tmp).write_iter(&summary::COLUMNS, (0..10).map(summary_row))?;
                if fail {
                    return Err(Error::FactsConvert("killed".into()));
                }
                Ok(())
            })?;
            stage.mark_done(Stage::Batch(0))
        };

        assert!(write_batch(true).is_err());
        assert!(!file.exists());
        assert!(!stage.is_done(Stage::Batch(0)));
        assert_eq!(fs::read_dir(stage.batch_dir(0)).unwrap().count(), 0);

        write_batch(false).unwrap();
        assert!(stage.is_done(Stage::Batch(0)));
        assert!(!stage.is_done(Stage::Batch(1)));
        let names: Vec<_> = fs::read_dir(stage.batch_dir(0))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![summary::FILENAME]);
        assert_eq!(fs::read_dir(stage.done_dir()).unwrap().count(), 1);
    }

    /// The key changes with one byte of one fact file, with a file's name, and with each
    /// configuration field, and not with temporary files, the order files were written in or the
    /// engine.
    #[test]
    fn run_key_changes_with_facts_and_config() {
        let index = tempfile::tempdir().unwrap();
        let stage = StageDir::new(index.path());
        stage.create().unwrap();
        let facts = stage.facts_dir();
        fs::write(facts.join("b.parquet"), b"bbbb").unwrap();
        fs::write(facts.join("a.parquet"), b"aaaa").unwrap();
        let config = KeyConfig::new(&IndexConfig::default(), 1 << 30, 1);
        let key = RunKey::new(&facts, &config).unwrap();

        // Stable, and stored.
        assert_eq!(RunKey::new(&facts, &config).unwrap(), key);
        assert_eq!(RunKey::load(&stage), None);
        key.save(&stage).unwrap();
        assert_eq!(RunKey::load(&stage), Some(key));

        // Temporary files do not count; write order does not either.
        fs::write(facts.join(".a.parquet.1.0.tmp"), b"partial").unwrap();
        assert_eq!(RunKey::new(&facts, &config).unwrap(), key);
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("a.parquet"), b"aaaa").unwrap();
        fs::write(other.path().join("b.parquet"), b"bbbb").unwrap();
        assert_eq!(RunKey::new(other.path(), &config).unwrap(), key);

        // One byte.
        fs::write(facts.join("b.parquet"), b"bbbc").unwrap();
        assert_ne!(RunKey::new(&facts, &config).unwrap(), key);
        fs::write(facts.join("b.parquet"), b"bbbb").unwrap();
        // A name, with the same bytes.
        fs::rename(facts.join("b.parquet"), facts.join("c.parquet")).unwrap();
        assert_ne!(RunKey::new(&facts, &config).unwrap(), key);
        fs::rename(facts.join("c.parquet"), facts.join("b.parquet")).unwrap();
        assert_eq!(RunKey::new(&facts, &config).unwrap(), key);

        // Each configuration field.
        let changed = [
            KeyConfig {
                alias_rule: !config.alias_rule,
                ..config.clone()
            },
            KeyConfig {
                deref_paths: !config.deref_paths,
                ..config.clone()
            },
            KeyConfig {
                budget: config.budget + 1,
                ..config.clone()
            },
            KeyConfig {
                floor: config.floor + 1,
                ..config.clone()
            },
            KeyConfig {
                timeout_secs: Some(config.timeout_secs.map_or(0, |t| t + 1)),
                ..config.clone()
            },
        ];
        for changed in changed {
            assert_ne!(RunKey::new(&facts, &changed).unwrap(), key, "{changed:?}");
        }
        // Not the engine.
        let engine = KeyConfig::new(
            &IndexConfig {
                parallelism: Parallelism::from_jobs(2),
                storage: Storage::Lsm,
                ..IndexConfig::default()
            },
            config.budget,
            config.floor,
        );
        assert_eq!(RunKey::new(&facts, &engine).unwrap(), key);
    }
}
