//! A whole staged run: plan, stage directory, batches, tables.

use std::path::Path;
use std::time::Instant;

use crate::error::{Error, ErrorContext};
use crate::facts::IdMap;
use crate::index_engine::{
    IndexConfig, IndexFacts, IndexResult, ProgramPaths, StagedConfig, Storage, phys_footprint_mb,
};
use crate::staged::batch::{forget_path_set, run_intent_stage, share_path_set};
use crate::staged::executor::{Executor, InProcess};
use ctadl_index::facts::FunctionId;
use ctadl_index::facts::parquet::Writer;
use ctadl_index::facts::schema;
use ctadl_index::staged::files::{
    assemble, finish_split_scc, read_summary_counts, run_frame, write_facts,
};
use ctadl_index::staged::plan::{BatchId, Plan, rerun_estimate};
use ctadl_index::staged::store::{
    KeyConfig, LatestRounds, RunKey, STAGE_ROWS_PER_GROUP, Stage, StageDir, write_atomic,
};
use hashbrown::HashMap;

/// What a staged run did.
#[derive(Debug, Default)]
pub struct RunStats {
    /// Each split SCC's rounds, in plan order.
    pub split_sccs: Vec<SccRounds>,
}

/// The rounds of one split SCC.
#[derive(Debug)]
pub struct SccRounds {
    pub scc: u32,
    /// The rounds that ran a part. The last ran a part whose summaries did not grow.
    pub rounds: u32,
    /// Each part's batch, with the number of times it ran.
    pub part_runs: Vec<(BatchId, u32)>,
}

/// Computes the index of `facts` in stages and writes its tables to `index_dir`, keeping the
/// stage directory under it.
///
/// Before any batch runs, it plans the batches, writes the facts, the path set, the plan and
/// the run key to the stage directory, and drops the facts. It then runs the batches in plan
/// order on `executor`, the global frame and the intent stage, and streams their outputs into
/// the tables. A stage directory already there is removed first.
pub fn run_staged(
    mut facts: IndexFacts,
    config: &IndexConfig,
    staged: StagedConfig,
    index_dir: &Path,
    executor: &mut dyn Executor,
) -> Result<RunStats, Error> {
    let start = Instant::now();
    if config.storage == Storage::Lsm {
        log::warn!("a staged run keeps its relations in memory; ignoring the LSM storage");
    }
    let stage = StageDir::new(index_dir);
    if stage.root().exists() {
        std::fs::remove_dir_all(stage.root())
            .err_context(|| format!("removing {}", stage.root().display()))?;
    }
    stage.create()?;

    let ProgramPaths { paths, path_set } = ProgramPaths::from_facts(&facts, config.deref_paths);
    let plan = Plan::new(&facts, staged.budget, staged.floor, paths.len());
    log::debug!("[mem cp] staged: planned: {:.1} MB", phys_footprint_mb());
    write_atomic(&stage.paths_file(), |tmp| {
        Writer::new(tmp)
            .rows_per_group(STAGE_ROWS_PER_GROUP)
            .write_iter(&schema::paths::COLUMNS, paths)
    })?;
    plan.save(&stage)?;
    write_facts(&stage, &mut facts, &plan)?;
    log::debug!(
        "[mem cp] staged: facts written: {:.1} MB",
        phys_footprint_mb()
    );
    drop(facts);
    log::debug!(
        "[mem cp] staged: facts dropped: {:.1} MB",
        phys_footprint_mb()
    );
    let key_config = KeyConfig::new(config, staged.budget, staged.floor);
    key_config.save(&stage)?;
    RunKey::new(&stage.facts_dir(), &key_config)?.save(&stage)?;
    stage.mark_done(Stage::Facts)?;
    log::info!(
        "staged index: {} batches of {} functions, {} split SCCs, largest estimate {:.1} MB of a \
         {:.1} MB budget, {:.1} MB",
        plan.batches.len(),
        plan.batches
            .iter()
            .map(|b| b.functions.len())
            .sum::<usize>(),
        plan.split_sccs.len(),
        plan.batches.iter().map(|b| b.estimate).max().unwrap_or(0) as f64 / MB,
        staged.budget as f64 / MB,
        phys_footprint_mb()
    );

    share_path_set(&stage, path_set)?;
    let mut stats = RunStats::default();
    let ran = (|| {
        for batch in &plan.batches {
            if stage.is_done(Stage::Batch(batch.id)) {
                continue;
            }
            match batch.part {
                // The SCC's parts follow one another in the plan, so its first runs them all.
                Some(part) => stats
                    .split_sccs
                    .push(run_split_scc(&stage, &plan, part.scc, executor)?),
                None => executor.run(&stage, batch.id, None)?,
            }
        }
        log::info!(
            "staged index: batches done in {:.1} s, {:.1} MB",
            start.elapsed().as_secs_f64(),
            phys_footprint_mb()
        );
        let edges = run_frame(&stage, &plan)?;
        log::info!(
            "staged index: {edges} edges of the global frame, {:.1} MB",
            phys_footprint_mb()
        );
        run_intent_stage(&stage)?;
        log::debug!(
            "[mem cp] staged: intent stage done: {:.1} MB",
            phys_footprint_mb()
        );
        Ok::<_, Error>(())
    })();
    forget_path_set(&stage);
    ran?;

    assemble(&stage, &plan, index_dir)?;
    log::debug!(
        "[mem cp] staged: tables written: {:.1} MB",
        phys_footprint_mb()
    );
    Ok(stats)
}

/// Runs the parts of split SCC `scc` in rounds, in plan order, until their summaries stop
/// growing, then moves each part's latest run to its batch and marks it done.
///
/// A part runs in the first round, and reruns in a later one only if a callee in another part
/// has more `summary` rows than when the part last ran. Its seeds only grow and every run
/// reaches its fixpoint, so a function's rows never shrink, and a count that differs has grown.
/// A round that reruns nothing ends the SCC, with every part's latest run seeded by the final
/// summaries of its callees.
fn run_split_scc(
    stage: &StageDir,
    plan: &Plan,
    scc: u32,
    executor: &mut dyn Executor,
) -> Result<SccRounds, Error> {
    let start = Instant::now();
    let parts: Vec<_> = plan.split_sccs[scc as usize]
        .parts
        .iter()
        .map(|b| &plan.batches[*b as usize])
        .collect();
    // Each part's callees in other parts: those of the batches it does not depend on.
    let callees: Vec<Vec<FunctionId>> = parts
        .iter()
        .map(|part| {
            part.callees
                .iter()
                .filter(|(_, b)| part.deps.binary_search(b).is_err())
                .map(|(f, _)| *f)
                .collect()
        })
        .collect();
    // The `summary` rows of each function of the SCC, from its part's latest run.
    let mut rows: HashMap<FunctionId, u64> = HashMap::new();
    // The rows of each part's callees in other parts when it last ran.
    let mut seen: Vec<Option<Vec<u64>>> = vec![None; parts.len()];
    let mut runs = vec![0u32; parts.len()];
    let mut latest = LatestRounds::default();
    let mut round = 0;
    loop {
        let round_start = Instant::now();
        let mut ran = 0;
        for (i, part) in parts.iter().enumerate() {
            let now: Vec<u64> = callees[i]
                .iter()
                .map(|f| rows.get(f).copied().unwrap_or(0))
                .collect();
            if seen[i].as_ref() == Some(&now) {
                continue;
            }
            log::debug!(
                "split SCC {scc}: part {} in round {round}, estimate {:.1} MB",
                part.id,
                rerun_estimate(part, now.iter().sum()) as f64 / MB
            );
            executor.run(stage, part.id, Some(round))?;
            for (f, n) in read_summary_counts(stage, scc, part.id, round)? {
                let old = rows.insert(f, n).unwrap_or(0);
                debug_assert!(n >= old, "{f:?} had {old} summary rows and now has {n}");
            }
            let previous = latest.latest.insert(part.id, round);
            latest.save(stage, scc)?;
            if let Some(previous) = previous {
                #[cfg(debug_assertions)]
                assert_rows_grew(stage, scc, part.id, previous, round);
                stage.unmark(Stage::Round {
                    batch: part.id,
                    round: previous,
                })?;
                let dir = stage.round_dir(scc, part.id, previous);
                std::fs::remove_dir_all(&dir)
                    .err_context(|| format!("removing {}", dir.display()))?;
            }
            seen[i] = Some(now);
            runs[i] += 1;
            ran += 1;
        }
        if ran == 0 {
            break;
        }
        log::info!(
            "split SCC {scc}: round {round} ran {ran} of {} parts in {:.2} s",
            parts.len(),
            round_start.elapsed().as_secs_f64()
        );
        round += 1;
    }
    finish_split_scc(stage, scc, &latest)?;
    log::info!(
        "split SCC {scc}: {} functions in {} parts, {round} rounds, {} part runs, {:.2} s",
        parts.iter().map(|p| p.functions.len()).sum::<usize>(),
        parts.len(),
        runs.iter().sum::<u32>(),
        start.elapsed().as_secs_f64()
    );
    Ok(SccRounds {
        scc,
        rounds: round,
        part_runs: parts.iter().map(|p| p.id).zip(runs).collect(),
    })
}

/// Asserts that the `summary` rows of part `batch`'s run in `round` include those of its run in
/// `previous`, as the rounds' termination needs.
#[cfg(debug_assertions)]
fn assert_rows_grew(stage: &StageDir, scc: u32, batch: BatchId, previous: u32, round: u32) {
    use ctadl_index::facts::parquet::Reader;
    use hashbrown::HashSet;
    let read = |round| -> HashSet<schema::summary::Record> {
        Reader::new(
            stage
                .round_dir(scc, batch, round)
                .join(schema::summary::FILENAME),
        )
        .read_vec(&schema::summary::COLUMNS)
        .expect("reading a part's summary")
        .into_iter()
        .collect()
    };
    let new = read(round);
    let lost = read(previous).difference(&new).count();
    assert_eq!(
        lost, 0,
        "part {batch} lost {lost} summary rows from round {previous} to {round}"
    );
}

const MB: f64 = (1 << 20) as f64;

/// [`run_staged`] in a temporary index directory, batches on the calling thread, returning the
/// tables it wrote. For callers that index in memory: tests, and small programs.
///
/// Panics if the run fails, as a monolithic run that cannot go on does.
pub fn run_in_process(
    facts: IndexFacts,
    config: IndexConfig,
    staged: StagedConfig,
    id_map: Option<&IdMap>,
) -> IndexResult {
    let index_dir = tempfile::tempdir().expect("creating a temporary index directory");
    let mut executor = InProcess {
        parallelism: config.parallelism,
        id_map,
    };
    let mut result = run_staged(facts, &config, staged, index_dir.path(), &mut executor)
        .and_then(|_| IndexResult::try_load(index_dir.path()))
        .unwrap_or_else(|e| panic!("the staged index run failed: {e:?}"));
    log::debug!(
        "[mem cp] staged: tables loaded: {:.1} MB",
        phys_footprint_mb()
    );
    // Every batch did, or the run would have failed.
    result.stats.profile.reached_fixpoint = true;
    result
}
