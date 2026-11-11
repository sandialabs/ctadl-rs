//! The stages that run the index's rules, each reading its inputs only from the stage directory:
//! a stage-1 batch ([`run_batch`]), the index rules on a batch's functions and its callees'
//! summaries, and the intent stage ([`run_intent_stage`]).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::error::Error;
use crate::facts::{IdMap, Path};
use crate::index_engine::path_set::PathSetRef;
use crate::index_engine::{EngineInput, Parallelism, ProgramPaths, phys_footprint_mb, run_engine};
use ctadl_index::facts::parquet::Reader;
use ctadl_index::facts::schema;
use ctadl_index::index_engine::IntentOutput;
use ctadl_index::staged::files::{
    BatchFacts, BatchOutputs, read_batch_facts, read_intent_input, write_intent_output,
};
use ctadl_index::staged::plan::{Batch, BatchId, Plan};
use ctadl_index::staged::store::{KeyConfig, RunKey, Stage, StageDir};

/// What running a batch did.
#[derive(Debug)]
pub struct BatchRun {
    /// Every stage file the batch read its input from, in the order read.
    pub read: Vec<PathBuf>,
}

/// Runs batch `id` of the run in `stage` on the engine of `parallelism`, writes its outputs and
/// marks it done. Its inputs are read only from `stage`: its descriptor, the run's
/// configuration, the path set, its functions' facts and its callees' summaries from the
/// batches that ran them, which must be done.
///
/// A part of a split SCC runs in a `round`, and every other batch in none. A part reads the
/// other parts' summaries from their latest runs, and its outputs are that round's run.
///
/// Fails if the fixpoint stops short of its end (`CTADL_INDEX_TIMEOUT_SECS`), and then writes
/// nothing.
pub fn run_batch(
    stage: &StageDir,
    id: BatchId,
    round: Option<u32>,
    parallelism: Parallelism,
    id_map: Option<&IdMap>,
) -> Result<BatchRun, Error> {
    let start = Instant::now();
    let batch = Batch::load(stage, id)?;
    if batch.part.is_some() != round.is_some() {
        return Err(Error::Staged(format!(
            "batch {id} runs {}, but was given round {round:?}",
            if batch.part.is_some() {
                "in rounds"
            } else {
                "once"
            }
        )));
    }
    let config = KeyConfig::load(stage)?.index_config(parallelism);
    let path_set = path_set(stage)?;
    let BatchFacts { facts, read } = read_batch_facts(stage, &batch)?;
    let mut input = EngineInput::from_slice(
        facts,
        ProgramPaths {
            paths: Vec::new(),
            path_set,
        },
        config,
        id_map,
    );
    input.seed_paths = false;
    // A dump refuses to overwrite another, and a process may run many batches.
    input.dump = false;
    let result = run_engine(input);
    if !result.stats.profile.reached_fixpoint {
        return Err(Error::Staged(format!(
            "batch {id} stopped before its fixpoint, so its outputs would be partial"
        )));
    }
    let outputs = BatchOutputs::new(&batch, result.summary, result.assign_like);
    log::debug!(
        "batch {id}{}: {} functions, {} summary rows, {} assign rows, {} frame edges, {:.2} s, \
         {:.1} MB",
        round.map(|r| format!(", round {r}")).unwrap_or_default(),
        batch.functions.len(),
        outputs.summary.len(),
        outputs.assign.len(),
        outputs.frame.len(),
        start.elapsed().as_secs_f64(),
        phys_footprint_mb()
    );
    match (batch.part, round) {
        (Some(part), Some(round)) => {
            outputs.write_round(stage, part.scc, id, round)?;
            stage.mark_done(Stage::Round { batch: id, round })?;
        }
        _ => {
            outputs.write(&stage.batch_dir(id))?;
            stage.mark_done(Stage::Batch(id))?;
        }
    }
    Ok(BatchRun { read })
}

/// Runs the intent rules alone on the flow of the functions that send intents and on the global
/// frame, which must be done, and writes the intent pairs and the calls they resolve. When no
/// function sends an intent, it writes none and runs nothing.
pub fn run_intent_stage(stage: &StageDir) -> Result<(), Error> {
    let start = Instant::now();
    let plan = Plan::load(stage)?;
    let output = match read_intent_input(stage, &plan, path_set(stage)?)? {
        Some(input) => {
            let rows = input.assign_like.len();
            let output = ctadl_index_serial::run_intent(input);
            log::debug!(
                "intent stage: {rows} assign rows in, {} intent pairs, {:.2} s, {:.1} MB",
                output.intent_pair.len(),
                start.elapsed().as_secs_f64(),
                phys_footprint_mb()
            );
            output
        }
        None => IntentOutput::default(),
    };
    write_intent_output(stage, output)
}

/// The path set of the last run a batch of this process ran, by its run key. Batches of one run
/// share it, so it is read once per process.
static PATH_SET: Mutex<Option<(RunKey, PathSetRef)>> = Mutex::new(None);

/// The path set of the run in `stage`, read from its paths file unless this process holds it.
fn path_set(stage: &StageDir) -> Result<PathSetRef, Error> {
    let key = RunKey::load(stage)
        .ok_or_else(|| Error::Staged(format!("no run key in {}", stage.root().display())))?;
    let mut cached = PATH_SET.lock().unwrap();
    if let Some((k, path_set)) = &*cached
        && *k == key
    {
        return Ok(path_set.clone());
    }
    let start = Instant::now();
    let before = phys_footprint_mb();
    let paths: Vec<Path> = Reader::new(stage.paths_file())
        .read_vec::<schema::paths::Record>(&schema::paths::COLUMNS)?
        .into_iter()
        .map(|(p,)| p)
        .collect();
    let path_set = ProgramPaths::from_paths(paths).path_set;
    log::info!(
        "read the path set of {} paths in {:.2} s, {:.1} MB",
        path_set.len(),
        start.elapsed().as_secs_f64(),
        phys_footprint_mb() - before
    );
    *cached = Some((key, path_set.clone()));
    Ok(path_set)
}

/// Lends `path_set`, the path set of the run in `stage`, to the batches this process runs, so
/// they need not read it.
pub fn share_path_set(stage: &StageDir, path_set: PathSetRef) -> Result<(), Error> {
    let key = RunKey::load(stage)
        .ok_or_else(|| Error::Staged(format!("no run key in {}", stage.root().display())))?;
    *PATH_SET.lock().unwrap() = Some((key, path_set));
    Ok(())
}

/// Drops the path set this process holds for the run in `stage`, if it holds that one.
pub fn forget_path_set(stage: &StageDir) {
    let key = RunKey::load(stage);
    let mut cached = PATH_SET.lock().unwrap();
    if cached.as_ref().map(|(k, _)| *k) == key {
        *cached = None;
    }
}
