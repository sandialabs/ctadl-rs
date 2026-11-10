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
use ctadl_index::facts::parquet::Writer;
use ctadl_index::facts::schema;
use ctadl_index::staged::files::{assemble, run_frame, write_facts};
use ctadl_index::staged::plan::Plan;
use ctadl_index::staged::store::{
    KeyConfig, RunKey, STAGE_ROWS_PER_GROUP, Stage, StageDir, write_atomic,
};

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
) -> Result<(), Error> {
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
    let plan = Plan::whole_sccs(&facts, staged.budget, paths.len());
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
        "staged index: {} batches of {} functions, largest estimate {:.1} MB of a {:.1} MB \
         budget, {:.1} MB",
        plan.batches.len(),
        plan.batches
            .iter()
            .map(|b| b.functions.len())
            .sum::<usize>(),
        plan.batches.iter().map(|b| b.estimate).max().unwrap_or(0) as f64 / MB,
        staged.budget as f64 / MB,
        phys_footprint_mb()
    );

    share_path_set(&stage, path_set)?;
    let ran = (|| {
        for batch in &plan.batches {
            if !stage.is_done(Stage::Batch(batch.id)) {
                executor.run(&stage, batch.id)?;
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
    Ok(())
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
        .and_then(|()| IndexResult::try_load(index_dir.path()))
        .unwrap_or_else(|e| panic!("the staged index run failed: {e:?}"));
    log::debug!(
        "[mem cp] staged: tables loaded: {:.1} MB",
        phys_footprint_mb()
    );
    // Every batch did, or the run would have failed.
    result.stats.profile.reached_fixpoint = true;
    result
}
