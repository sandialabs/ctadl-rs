//! A whole staged run: plan, stage directory, batches, tables.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use crate::error::{Error, ErrorContext};
use crate::facts::IdMap;
use crate::index_engine::{
    IndexConfig, IndexFacts, IndexResult, ProgramPaths, StagedConfig, Storage, phys_footprint_mb,
};
use crate::staged::batch::forget_path_set;
use crate::staged::executor::{Executor, InProcess, Job};
use ctadl_index::facts::FunctionId;
use ctadl_index::facts::parquet::Writer;
use ctadl_index::facts::schema;
use ctadl_index::index_engine::IntentOutput;
use ctadl_index::staged::files::{
    assemble, copy_part_run, finish_split_scc, read_calls, read_summary_counts, run_frame,
    write_facts, write_intent_output,
};
use ctadl_index::staged::plan::{BatchId, Plan, SplitKind, rerun_estimate};
use ctadl_index::staged::store::{
    KeyConfig, LatestRounds, RunKey, STAGE_ROWS_PER_GROUP, Stage, StageDir, write_atomic,
};
use hashbrown::{HashMap, HashSet};

/// What a staged run did.
#[derive(Debug, Default)]
pub struct RunStats {
    /// Whether it resumed from a stage directory an earlier run of the same facts and
    /// configuration left.
    pub resumed: bool,
    /// The batches an earlier run did, which this one skipped, sorted.
    pub skipped: Vec<BatchId>,
    /// Each split SCC's rounds, in plan order.
    pub split_sccs: Vec<SccRounds>,
    /// The workers killed because the running ones were over the budget.
    pub kills: u32,
    /// The batches split after a kill, each with the new batch that took its second half.
    pub splits: Vec<(BatchId, BatchId)>,
    /// What every estimate was multiplied by in the end: at least the largest footprint over
    /// estimate of a killed worker, and 1 if none was killed.
    pub alpha: f64,
    /// The batches of one function that ran alone over the budget.
    pub over_budget_alone: Vec<BatchId>,
}

/// A test hook: set to a batch id, it makes a staged run that the CLI starts stop once that
/// batch is done, with [`Error::StagedStopped`], leaving its stage directory to resume from.
pub const STOP_AFTER_VAR: &str = "CTADL_STAGED_STOP_AFTER";

/// The batch [`STOP_AFTER_VAR`] names, if it names one.
pub fn stop_after_from_env() -> Option<BatchId> {
    std::env::var(STOP_AFTER_VAR).ok()?.trim().parse().ok()
}

/// Where a run writes a fresh stage directory in the index directory, until it knows whether
/// the one there can be resumed.
const FRESH_STAGE: &str = ".staging.new";

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
/// stage directory under it. See [`run_staged_until`].
pub fn run_staged(
    facts: IndexFacts,
    config: &IndexConfig,
    staged: StagedConfig,
    index_dir: &Path,
    executor: &mut dyn Executor,
) -> Result<RunStats, Error> {
    run_staged_until(facts, config, staged, index_dir, executor, None)
}

/// Computes the index of `facts` in stages and writes its tables to `index_dir`, keeping the
/// stage directory under it.
///
/// Before any batch runs, it plans the batches and writes the path set (as rows, and frozen for
/// workers to map), the plan and the facts to a fresh stage directory, drops the facts, and
/// computes the run key of the facts written and the configuration. If the stage directory
/// already in `index_dir` has the same key, the run resumes from it: the fresh one is removed,
/// files of stages not marked done are discarded, and the stages that are done are skipped.
/// Otherwise the fresh directory, with its key, replaces it.
///
/// It then runs the batches in plan order on `executor`, the global frame and the intent stage,
/// and streams their outputs into the tables. With `stop_after`, it stops instead, with
/// [`Error::StagedStopped`], once that batch is done.
pub fn run_staged_until(
    mut facts: IndexFacts,
    config: &IndexConfig,
    staged: StagedConfig,
    index_dir: &Path,
    executor: &mut dyn Executor,
    stop_after: Option<BatchId>,
) -> Result<RunStats, Error> {
    let start = Instant::now();
    if config.storage == Storage::Lsm {
        log::warn!("a staged run keeps its relations in memory; ignoring the LSM storage");
    }
    let stage = StageDir::new(index_dir);
    let fresh = StageDir::from_root(index_dir.join(FRESH_STAGE));
    remove_dir_if_exists(fresh.root())?;
    fresh.create()?;

    let ProgramPaths { paths, path_set } = ProgramPaths::from_facts(&facts, config.deref_paths);
    let mut plan = Plan::new(&facts, staged.budget, staged.floor, paths.len());
    log::debug!("[mem cp] staged: planned: {:.1} MB", phys_footprint_mb());
    write_atomic(&fresh.paths_file(), |tmp| {
        Writer::new(tmp)
            .rows_per_group(STAGE_ROWS_PER_GROUP)
            .write_iter(&schema::paths::COLUMNS, paths)
    })?;
    write_atomic(&fresh.path_set_file(), |tmp| path_set.write_file(tmp))?;
    log::debug!(
        "[mem cp] staged: path set written: {:.1} MB",
        phys_footprint_mb()
    );
    plan.save(&fresh)?;
    write_facts(&fresh, &mut facts, &plan)?;
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
    let key = RunKey::new(&fresh.facts_dir(), &key_config)?;
    let resumed = stage.is_done(Stage::Facts) && RunKey::load(&stage) == Some(key);
    if resumed {
        remove_dir_if_exists(fresh.root())?;
        // The earlier run may have split batches. Its descriptors are rewritten, in case it
        // stopped while it saved a split.
        plan = Plan::load(&stage)?;
        plan.save(&stage)?;
        stage.discard_unmarked(&plan)?;
        log::info!(
            "staged index: resuming the run in {}, whose facts and configuration are the same",
            stage.root().display()
        );
    } else {
        key_config.save(&fresh)?;
        key.save(&fresh)?;
        fresh.mark_done(Stage::Facts)?;
        if stage.root().exists() {
            log::info!(
                "staged index: the facts or configuration of the run in {} differ; starting over",
                stage.root().display()
            );
        }
        remove_dir_if_exists(stage.root())?;
        std::fs::rename(fresh.root(), stage.root()).err_context(|| {
            format!(
                "renaming {} to {}",
                fresh.root().display(),
                stage.root().display()
            )
        })?;
    }
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

    executor.lend_path_set(&stage, path_set)?;
    let ran = (|| {
        let mut stats = Scheduler::new(&stage, &mut plan, stop_after).run(executor)?;
        stats.resumed = resumed;
        log::info!(
            "staged index: batches done in {:.1} s, {} of them by an earlier run, {} workers \
             killed, {} batches split, estimates scaled by {:.2}, {:.1} MB",
            start.elapsed().as_secs_f64(),
            stats.skipped.len(),
            stats.kills,
            stats.splits.len(),
            stats.alpha,
            phys_footprint_mb()
        );
        if stage.is_done(Stage::Frame) {
            log::info!("staged index: the global frame was done by an earlier run");
        } else {
            let edges = run_frame(&stage, &plan)?;
            log::info!(
                "staged index: {edges} edges of the global frame, {:.1} MB",
                phys_footprint_mb()
            );
        }
        if stage.is_done(Stage::Intent) {
            log::info!("staged index: the intent stage was done by an earlier run");
        } else if plan.intent_batches.is_empty() {
            // No function sends an intent, so no worker need read the path set to find none.
            write_intent_output(&stage, IntentOutput::default())?;
        } else {
            executor.start(&stage, Job::Intent, true)?;
            loop {
                if let Some((_, result)) = executor.wait() {
                    break result?;
                }
            }
        }
        log::debug!(
            "[mem cp] staged: intent stage done: {:.1} MB",
            phys_footprint_mb()
        );
        Ok::<_, Error>(stats)
    })();
    forget_path_set(&stage);
    if ran.is_err() {
        executor.stop_all();
    }
    let stats = ran?;

    assemble(&stage, &plan, index_dir)?;
    log::debug!(
        "[mem cp] staged: tables written: {:.1} MB",
        phys_footprint_mb()
    );
    Ok(stats)
}

fn remove_dir_if_exists(dir: &Path) -> Result<(), Error> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(Error::from(e)).err_context(|| format!("removing {}", dir.display()))
        }
        _ => Ok(()),
    }
}

/// What the scheduler runs: a batch, or all the parts of a split SCC, one at a time in rounds.
enum Unit {
    Batch(BatchId),
    Scc(SplitRun),
}

/// Runs the batches of a plan on an executor: each once the batches that hold its callees are
/// done, in plan order, as many at once as the executor's threads and the budget allow, and
/// kills workers while those running are over the budget.
///
/// While workers run it holds the plan, and of each unit only its batches, dependencies and
/// estimates: the facts and the call graph are dropped before it starts.
struct Scheduler<'a> {
    stage: &'a StageDir,
    plan: &'a mut Plan,
    /// In plan order, which runs every unit after those it depends on, then those that splits
    /// made.
    units: Vec<Unit>,
    /// The units each unit waits for, less those done.
    waiting: Vec<usize>,
    /// The units that wait for each unit.
    dependents: Vec<Vec<usize>>,
    /// Units whose dependencies are done and that are not running or done, by plan order.
    ready: BTreeSet<usize>,
    running: HashMap<Job, Running>,
    /// Whether each unit is done.
    finished: Vec<bool>,
    done: usize,
    /// The batch after which to stop.
    stop_after: Option<BatchId>,
    /// What the executor multiplies every estimate by.
    scale: f64,
    /// What every estimate is multiplied by besides `scale`: the largest footprint over
    /// estimate of a killed worker, at least 1.
    alpha: f64,
    /// The jobs of one function logged as running alone over the budget.
    over_budget_alone: HashSet<Job>,
    stats: RunStats,
}

/// A running job.
struct Running {
    unit: usize,
    /// As the plan estimates it, before `scale` and `alpha`.
    estimate: u64,
    threads: usize,
}

impl<'a> Scheduler<'a> {
    fn new(stage: &'a StageDir, plan: &'a mut Plan, stop_after: Option<BatchId>) -> Self {
        let mut units = Vec::new();
        let mut unit_of = vec![0; plan.batches.len()];
        let mut scc_unit = HashMap::new();
        for batch in &plan.batches {
            unit_of[batch.id as usize] = match batch.part {
                None => {
                    units.push(Unit::Batch(batch.id));
                    units.len() - 1
                }
                Some(part) => *scc_unit.entry(part.scc).or_insert_with(|| {
                    units.push(Unit::Scc(SplitRun::new(plan, part.scc)));
                    units.len() - 1
                }),
            };
        }
        let mut waiting = vec![0; units.len()];
        let mut dependents = vec![Vec::new(); units.len()];
        for (u, unit) in units.iter().enumerate() {
            let mut deps: Vec<usize> = unit
                .batches(plan)
                .iter()
                .flat_map(|b| &plan.batches[*b as usize].deps)
                .map(|d| unit_of[*d as usize])
                .filter(|d| *d != u)
                .collect();
            deps.sort_unstable();
            deps.dedup();
            waiting[u] = deps.len();
            for d in deps {
                dependents[d].push(u);
            }
        }
        let ready = (0..units.len()).filter(|u| waiting[*u] == 0).collect();
        Self {
            finished: vec![false; units.len()],
            stage,
            plan,
            units,
            waiting,
            dependents,
            ready,
            running: HashMap::new(),
            done: 0,
            stop_after,
            scale: 1.0,
            alpha: 1.0,
            over_budget_alone: HashSet::new(),
            stats: RunStats::default(),
        }
    }

    /// Runs every unit, and stops at the first job that fails.
    ///
    /// It starts the first ready unit's next job while a thread is free and the running jobs
    /// and that one fit the budget together: each running job counts as its estimate or its
    /// latest footprint, whichever is larger. It never passes over that unit for a smaller one
    /// behind it, which could starve it, and starts it whatever its estimate when nothing runs.
    /// A job is alone, and takes every thread, when nothing runs beside it and nothing else is
    /// ready. Between starts, it keeps the running jobs' footprints within the budget
    /// ([`Scheduler::enforce`]).
    ///
    /// Units a run before this one did are skipped, and a split SCC it left midway resumes.
    fn run(mut self, executor: &mut dyn Executor) -> Result<RunStats, Error> {
        for u in 0..self.units.len() {
            let batches = self.units[u].batches(self.plan);
            let done = batches
                .iter()
                .filter(|b| self.stage.is_done(Stage::Batch(**b)))
                .count();
            if done == batches.len() {
                log::debug!("batches {batches:?}: done by an earlier run");
                self.stats.skipped.extend(batches);
                self.ready.remove(&u);
                self.finish(u);
                continue;
            }
            let Unit::Scc(scc) = &mut self.units[u] else {
                continue;
            };
            scc.resume(self.stage)?;
            if done > 0 {
                // The earlier run stopped while it moved the parts' runs to their batches.
                let rounds = scc.finish(self.stage)?;
                self.stats.split_sccs.push(rounds);
                self.ready.remove(&u);
                self.finish(u);
            }
        }
        self.stats.skipped.sort_unstable();
        let threads = executor.threads();
        self.scale = executor.estimate_scale();
        while self.done < self.units.len() {
            let footprints: HashMap<Job, u64> = executor.footprints().into_iter().collect();
            while let Some(&u) = self.ready.first() {
                let Some((job, estimate)) = self.next_job(u) else {
                    self.ready.remove(&u);
                    let Unit::Scc(scc) = &mut self.units[u] else {
                        unreachable!("only a split SCC has no next job");
                    };
                    let rounds = scc.finish(self.stage)?;
                    self.stats.split_sccs.push(rounds);
                    self.finish(u);
                    self.stop_if_asked(u)?;
                    continue;
                };
                let used: usize = self.running.values().map(|r| r.threads).sum();
                let held = self.running.iter().fold(0u64, |sum, (job, r)| {
                    let footprint = footprints.get(job).copied().unwrap_or(0);
                    sum.saturating_add(self.scaled(r.estimate).max(footprint))
                });
                if !self.running.is_empty()
                    && (used >= threads
                        || held.saturating_add(self.scaled(estimate)) > self.plan.budget)
                {
                    break;
                }
                let alone = self.running.is_empty() && self.ready.len() == 1;
                log::debug!(
                    "{job}: starting, estimate {:.1} MB, {:.1} MB running",
                    self.scaled(estimate) as f64 / MB,
                    held as f64 / MB
                );
                executor.start(self.stage, job, alone)?;
                self.ready.remove(&u);
                let threads = if alone { threads } else { 1 };
                self.running.insert(
                    job,
                    Running {
                        unit: u,
                        estimate,
                        threads,
                    },
                );
            }
            if self.done == self.units.len() {
                break;
            }
            if self.running.is_empty() {
                return Err(Error::Staged(format!(
                    "{} of {} units of the plan can never run",
                    self.units.len() - self.done,
                    self.units.len()
                )));
            }
            if let Some((job, result)) = executor.wait() {
                result?;
                let running = self
                    .running
                    .remove(&job)
                    .expect("the executor ended a job it was given");
                self.ended(job, running.unit)?;
            }
            self.enforce(executor)?;
        }
        self.stats.split_sccs.sort_by_key(|s| s.scc);
        self.stats.alpha = self.alpha;
        Ok(self.stats)
    }

    /// `estimate` multiplied by the executor's scale and by alpha.
    fn scaled(&self, estimate: u64) -> u64 {
        // A cast from `f64` saturates.
        (estimate as f64 * self.scale * self.alpha) as u64
    }

    /// Records that `job` of unit `u` ended well.
    fn ended(&mut self, job: Job, u: usize) -> Result<(), Error> {
        match (&mut self.units[u], job) {
            (
                Unit::Scc(scc),
                Job::Batch {
                    id,
                    round: Some(round),
                },
            ) => {
                scc.ran(self.stage, self.plan, id, round)?;
                self.ready.insert(u);
            }
            _ => {
                self.finish(u);
                self.stop_if_asked(u)?;
            }
        }
        Ok(())
    }

    /// Kills workers until the latest footprints of those running fit the budget. It kills the
    /// largest worker that holds more than one function. When every one holds one, it kills all
    /// but the largest, which then runs alone to its end, however large.
    fn enforce(&mut self, executor: &mut dyn Executor) -> Result<(), Error> {
        loop {
            let footprints = executor.footprints();
            let total = footprints
                .iter()
                .fold(0u64, |sum, (_, f)| sum.saturating_add(*f));
            if total <= self.plan.budget {
                return Ok(());
            }
            let functions = |job: &Job| match job {
                Job::Batch { id, .. } => self.plan.batches[*id as usize].functions.len(),
                Job::Intent => 1,
            };
            let largest = footprints
                .iter()
                .filter(|(job, _)| functions(job) > 1)
                .max_by_key(|(_, f)| *f);
            if let Some(&(job, footprint)) = largest {
                self.kill(executor, job, footprint, total)?;
                continue;
            }
            let &(largest, footprint) = footprints
                .iter()
                .max_by_key(|(_, f)| *f)
                .expect("over the budget, so a worker runs");
            if self.over_budget_alone.insert(largest) {
                log::warn!(
                    "staged index: {largest} holds one function and uses {:.1} MB, with {:.1} MB \
                     running against a {:.1} MB budget; it runs alone to its end",
                    footprint as f64 / MB,
                    total as f64 / MB,
                    self.plan.budget as f64 / MB
                );
                if let Job::Batch { id, .. } = largest {
                    self.stats.over_budget_alone.push(id);
                }
            }
            for &(job, f) in &footprints {
                if job != largest {
                    self.kill(executor, job, f, total)?;
                }
            }
            return Ok(());
        }
    }

    /// Kills `job`, whose latest footprint is `footprint` of the `total` running, discards its
    /// files and requeues it: split in two if it is over the budget alone, and otherwise whole,
    /// to run when fewer run beside it. Raises alpha to its footprint over its estimate.
    fn kill(
        &mut self,
        executor: &mut dyn Executor,
        job: Job,
        footprint: u64,
        total: u64,
    ) -> Result<(), Error> {
        executor.kill(job);
        let running = self
            .running
            .remove(&job)
            .expect("the scheduler kills only jobs it started");
        let Job::Batch { id, round } = job else {
            unreachable!("the intent stage runs after the scheduler");
        };
        let marker = match round {
            Some(round) => Stage::Round { batch: id, round },
            None => Stage::Batch(id),
        };
        if self.stage.is_done(marker) {
            log::debug!("{job}: ended before it was killed");
            return self.ended(job, running.unit);
        }
        self.stage
            .discard_run(&self.plan.batches[id as usize], round)?;
        self.stats.kills += 1;
        let estimated = running.estimate as f64 * self.scale;
        if estimated > 0.0 {
            self.alpha = self.alpha.max(footprint as f64 / estimated);
        }
        let batch = &self.plan.batches[id as usize];
        // Splitting cannot lower what every worker holds, but the plan's estimates count that
        // in the budget, and a batch over the budget alone holds more than that.
        let split = footprint > self.plan.budget && batch.functions.len() > 1;
        log::info!(
            "staged index: killed {job} of {} functions at {:.1} MB (estimate {:.1} MB), with \
             {:.1} MB running against a {:.1} MB budget; {}; estimates now scaled by {:.2}",
            batch.functions.len(),
            footprint as f64 / MB,
            running.estimate as f64 * self.scale / MB,
            total as f64 / MB,
            self.plan.budget as f64 / MB,
            if split {
                "splitting it in two"
            } else {
                "requeued whole"
            },
            self.alpha
        );
        if split {
            self.split(running.unit, id)?;
        }
        self.ready.insert(running.unit);
        Ok(())
    }

    /// Splits batch `id` of unit `u` in two with [`Plan::split_batch`], saves the batches that
    /// changed, and updates the units: the second half of a batch of whole SCCs is a new unit
    /// that the unit's dependents wait for, and a batch of one SCC becomes a split SCC.
    fn split(&mut self, u: usize, id: BatchId) -> Result<(), Error> {
        let calls = read_calls(self.stage, &self.plan.batches[id as usize].functions)?;
        let Some(split) = self.plan.split_batch(id, &calls) else {
            return Ok(());
        };
        let second = split.second;
        if let (SplitKind::Part(_), Unit::Scc(scc)) = (split.kind, &mut self.units[u]) {
            scc.split_part(self.stage, self.plan, id, second)?;
        }
        self.plan.save_batches(self.stage, &split.changed)?;
        self.stats.splits.push((id, second));
        let size = |b: BatchId| {
            let batch = &self.plan.batches[b as usize];
            (batch.functions.len(), batch.estimate as f64 / MB)
        };
        let ((n1, e1), (n2, e2)) = (size(id), size(second));
        log::info!(
            "staged index: split batch {id} {}: {n1} functions, estimate {e1:.1} MB, stay; \
             {n2} functions, estimate {e2:.1} MB, are batch {second}",
            match split.kind {
                SplitKind::Sccs => "between SCCs".to_string(),
                SplitKind::NewScc(scc) => format!("within its SCC, now split SCC {scc}"),
                SplitKind::Part(scc) => format!("within split SCC {scc}"),
            }
        );
        match split.kind {
            SplitKind::Sccs => {
                let v = self.units.len();
                self.units.push(Unit::Batch(second));
                self.finished.push(false);
                let dependents = self.dependents[u].clone();
                for d in &dependents {
                    self.waiting[*d] += 1;
                }
                self.dependents.push(dependents);
                let waits = self.plan.batches[second as usize].deps.contains(&id);
                self.waiting.push(usize::from(waits));
                if waits {
                    self.dependents[u].push(v);
                } else {
                    self.ready.insert(v);
                }
            }
            SplitKind::NewScc(scc) => self.units[u] = Unit::Scc(SplitRun::new(self.plan, scc)),
            SplitKind::Part(_) => {}
        }
        Ok(())
    }

    /// The next job of unit `u` and its estimate, or `None` if `u` is a split SCC whose
    /// summaries stopped growing.
    fn next_job(&mut self, u: usize) -> Option<(Job, u64)> {
        match &mut self.units[u] {
            Unit::Batch(id) => Some((
                Job::Batch {
                    id: *id,
                    round: None,
                },
                self.plan.batches[*id as usize].estimate,
            )),
            Unit::Scc(scc) => {
                let (part, round, callee_rows) = scc.next()?;
                let estimate = rerun_estimate(&self.plan.batches[part as usize], callee_rows);
                Some((
                    Job::Batch {
                        id: part,
                        round: Some(round),
                    },
                    estimate,
                ))
            }
        }
    }

    /// Fails with [`Error::StagedStopped`] if unit `u`, done, holds the batch to stop after.
    fn stop_if_asked(&self, u: usize) -> Result<(), Error> {
        match self.stop_after {
            Some(b) if self.units[u].batches(self.plan).contains(&b) => {
                log::info!("staged index: stopping after batch {b}, as {STOP_AFTER_VAR} says");
                Err(Error::StagedStopped(b))
            }
            _ => Ok(()),
        }
    }

    /// Records that unit `u` is done, and readies the units that waited only for it.
    fn finish(&mut self, u: usize) {
        self.finished[u] = true;
        self.done += 1;
        for d in std::mem::take(&mut self.dependents[u]) {
            self.waiting[d] -= 1;
            if self.waiting[d] == 0 && !self.finished[d] {
                self.ready.insert(d);
            }
        }
    }
}

impl Unit {
    fn batches(&self, plan: &Plan) -> Vec<BatchId> {
        match self {
            Unit::Batch(id) => vec![*id],
            Unit::Scc(scc) => plan.split_sccs[scc.scc as usize].parts.clone(),
        }
    }
}

/// The rounds of split SCC `scc`, which run its parts in plan order until their summaries stop
/// growing, then move each part's latest run to its batch and mark it done.
///
/// A part runs in the first round, and reruns in a later one only if a callee in another part
/// has more `summary` rows than when the part last ran. Its seeds only grow and every run
/// reaches its fixpoint, so a function's rows never shrink, and a count that differs has grown.
/// A round that reruns nothing ends the SCC, with every part's latest run seeded by the final
/// summaries of its callees.
struct SplitRun {
    scc: u32,
    parts: Vec<BatchId>,
    /// Each part's callees in other parts.
    callees: Vec<Vec<FunctionId>>,
    /// The `summary` rows of each function of the SCC, from its part's latest run.
    rows: HashMap<FunctionId, u64>,
    /// The rows of each part's callees in other parts when it last ran.
    seen: Vec<Option<Vec<u64>>>,
    runs: Vec<u32>,
    latest: LatestRounds,
    round: u32,
    /// The part of `round` to look at next.
    next: usize,
    /// The parts that ran in `round`.
    ran: u32,
    /// When its first part started.
    start: Option<Instant>,
    /// When `round` started.
    round_start: Option<Instant>,
}

impl SplitRun {
    fn new(plan: &Plan, scc: u32) -> Self {
        let parts = plan.split_sccs[scc as usize].parts.clone();
        Self {
            scc,
            callees: part_callees(plan, &parts),
            rows: HashMap::new(),
            seen: vec![None; parts.len()],
            runs: vec![0; parts.len()],
            parts,
            latest: LatestRounds::default(),
            round: 0,
            next: 0,
            ran: 0,
            start: None,
            round_start: None,
        }
    }

    /// Picks up where an earlier run of this SCC that stopped midway left it: each part's latest
    /// run, which [`StageDir::discard_unmarked`] kept. The rounds go on from the round after the
    /// latest, in which every part runs again, since what its callees held when it last ran is
    /// not kept.
    fn resume(&mut self, stage: &StageDir) -> Result<(), Error> {
        self.latest = LatestRounds::load(stage, self.scc)?;
        for (&batch, &round) in &self.latest.latest {
            self.round = self.round.max(round + 1);
            if !stage.is_done(Stage::Batch(batch)) {
                self.rows
                    .extend(read_summary_counts(stage, self.scc, batch, round)?);
            }
        }
        if !self.latest.latest.is_empty() {
            log::info!(
                "split SCC {}: resuming in round {}, from the latest runs of {} of {} parts",
                self.scc,
                self.round,
                self.latest.latest.len(),
                self.parts.len()
            );
        }
        Ok(())
    }

    /// The rows of part `i`'s callees in other parts.
    fn callee_rows(&self, i: usize) -> Vec<u64> {
        self.callees[i]
            .iter()
            .map(|f| self.rows.get(f).copied().unwrap_or(0))
            .collect()
    }

    /// The part to run next, its round and the summary rows of its callees in other parts, or
    /// `None` once a round ran no part. Until [`SplitRun::ran`] records its run, it returns the
    /// same part.
    fn next(&mut self) -> Option<(BatchId, u32, u64)> {
        let now = Instant::now();
        self.start.get_or_insert(now);
        self.round_start.get_or_insert(now);
        loop {
            if self.next == self.parts.len() {
                if self.ran == 0 {
                    return None;
                }
                log::info!(
                    "split SCC {}: round {} ran {} of {} parts in {:.2} s",
                    self.scc,
                    self.round,
                    self.ran,
                    self.parts.len(),
                    self.round_start.map_or(0.0, |t| t.elapsed().as_secs_f64())
                );
                self.round += 1;
                self.next = 0;
                self.ran = 0;
                self.round_start = Some(Instant::now());
            }
            let now = self.callee_rows(self.next);
            if self.seen[self.next].as_ref() != Some(&now) {
                return Some((self.parts[self.next], self.round, now.iter().sum()));
            }
            self.next += 1;
        }
    }

    /// Records the run of `batch`, the part [`SplitRun::next`] returned, in `round`, and
    /// deletes its previous run.
    fn ran(
        &mut self,
        stage: &StageDir,
        plan: &Plan,
        batch: BatchId,
        round: u32,
    ) -> Result<(), Error> {
        let i = self.next;
        assert_eq!(
            (self.parts[i], self.round),
            (batch, round),
            "split SCC {} ran a part out of turn",
            self.scc
        );
        let now = self.callee_rows(i);
        for (f, n) in read_summary_counts(stage, self.scc, batch, round)? {
            let old = self.rows.insert(f, n).unwrap_or(0);
            debug_assert!(n >= old, "{f:?} had {old} summary rows and now has {n}");
        }
        let previous = self.latest.latest.insert(batch, round);
        self.latest.save(stage, self.scc)?;
        if let Some(previous) = previous {
            if cfg!(debug_assertions) {
                assert_rows_grew(stage, plan, self.scc, batch, previous, round);
            }
            stage.unmark(Stage::Round {
                batch,
                round: previous,
            })?;
            let dir = stage.round_dir(self.scc, batch, previous);
            std::fs::remove_dir_all(&dir).err_context(|| format!("removing {}", dir.display()))?;
        }
        self.seen[i] = Some(now);
        self.runs[i] += 1;
        self.ran += 1;
        self.next += 1;
        Ok(())
    }

    /// Records that part `first`, the part [`SplitRun::next`] returned, was split, and that
    /// `second` holds the second half of its functions as the part after it. Both run next.
    ///
    /// If `first` ran before, `second` gets a copy of that run, so the parts that read
    /// `second`'s summaries, `first` among them, still read what they read from `first`: their
    /// summaries then never shrink, as the rounds need.
    fn split_part(
        &mut self,
        stage: &StageDir,
        plan: &Plan,
        first: BatchId,
        second: BatchId,
    ) -> Result<(), Error> {
        let i = self.next;
        assert_eq!(
            self.parts[i], first,
            "split SCC {} split a part out of turn",
            self.scc
        );
        self.parts.insert(i + 1, second);
        self.callees = part_callees(plan, &self.parts);
        self.seen[i] = None;
        self.seen.insert(i + 1, None);
        self.runs.insert(i + 1, 0);
        if let Some(&round) = self.latest.latest.get(&first) {
            let functions = &plan.batches[second as usize].functions;
            copy_part_run(stage, self.scc, first, second, round, functions)?;
            stage.mark_done(Stage::Round {
                batch: second,
                round,
            })?;
            self.latest.latest.insert(second, round);
            self.latest.save(stage, self.scc)?;
        }
        Ok(())
    }

    /// Moves each part's latest run to its batch and marks it done, once [`SplitRun::next`]
    /// returned `None`.
    fn finish(&mut self, stage: &StageDir) -> Result<SccRounds, Error> {
        finish_split_scc(stage, self.scc, &self.latest)?;
        log::info!(
            "split SCC {}: {} parts, {} rounds, {} part runs, {:.2} s",
            self.scc,
            self.parts.len(),
            self.round,
            self.runs.iter().sum::<u32>(),
            self.start.map_or(0.0, |t| t.elapsed().as_secs_f64())
        );
        Ok(SccRounds {
            scc: self.scc,
            rounds: self.round,
            part_runs: self.parts.iter().copied().zip(self.runs.clone()).collect(),
        })
    }
}

/// Each of `parts`' callees in other parts: those of the batches it does not depend on.
fn part_callees(plan: &Plan, parts: &[BatchId]) -> Vec<Vec<FunctionId>> {
    parts
        .iter()
        .map(|b| {
            let part = &plan.batches[*b as usize];
            part.callees
                .iter()
                .filter(|(_, b)| part.deps.binary_search(b).is_err())
                .map(|(f, _)| *f)
                .collect()
        })
        .collect()
}

/// Asserts that the `summary` rows of part `batch`'s run in `round` include those of its
/// functions in its run in `previous`, as the rounds' termination needs. The previous run may be
/// a copy of the run of the part it was split from.
fn assert_rows_grew(
    stage: &StageDir,
    plan: &Plan,
    scc: u32,
    batch: BatchId,
    previous: u32,
    round: u32,
) {
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
    let functions = &plan.batches[batch as usize].functions;
    let lost = read(previous)
        .difference(&new)
        .filter(|r| functions.binary_search(&r.0).is_ok())
        .count();
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
    let mut executor = InProcess::new(config.parallelism, id_map);
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
