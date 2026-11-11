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
    assemble, finish_split_scc, read_summary_counts, run_frame, write_facts, write_intent_output,
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

    executor.lend_path_set(&stage, path_set)?;
    let ran = (|| {
        let stats = Scheduler::new(&stage, &plan).run(executor)?;
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
        if plan.intent_batches.is_empty() {
            // No function sends an intent, so no worker need read the path set to find none.
            write_intent_output(&stage, IntentOutput::default())?;
        } else {
            executor.start(&stage, Job::Intent, true)?;
            executor.wait().1?;
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

/// What the scheduler runs: a batch, or all the parts of a split SCC, one at a time in rounds.
enum Unit {
    Batch(BatchId),
    Scc(SplitRun),
}

/// Runs the batches of a plan on an executor: each once the batches that hold its callees are
/// done, in plan order, as many at once as the executor's threads and the budget allow.
struct Scheduler<'a> {
    stage: &'a StageDir,
    plan: &'a Plan,
    /// In plan order, which runs every unit after those it depends on.
    units: Vec<Unit>,
    /// The units each unit waits for, less those done.
    waiting: Vec<usize>,
    /// The units that wait for each unit.
    dependents: Vec<Vec<usize>>,
    /// Units whose dependencies are done and that are not running or done, by plan order.
    ready: BTreeSet<usize>,
    /// The running jobs, with their units, estimates and threads.
    running: HashMap<Job, (usize, u64, usize)>,
    /// Whether each unit is done.
    finished: Vec<bool>,
    done: usize,
    stats: RunStats,
}

impl<'a> Scheduler<'a> {
    fn new(stage: &'a StageDir, plan: &'a Plan) -> Self {
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
            stats: RunStats::default(),
        }
    }

    /// Runs every unit, and stops at the first job that fails.
    ///
    /// It starts the first ready unit's next job while a thread is free and the estimates of the
    /// running jobs and that one fit the budget together. It never passes over that unit for a
    /// smaller one behind it, which could starve it, and starts it whatever its estimate when
    /// nothing runs. A job is alone, and takes every thread, when nothing runs beside it and
    /// nothing else is ready.
    fn run(mut self, executor: &mut dyn Executor) -> Result<RunStats, Error> {
        // Units whose batches a run before this one finished.
        for u in 0..self.units.len() {
            let batches = self.units[u].batches(self.plan);
            if batches.iter().all(|b| self.stage.is_done(Stage::Batch(*b))) {
                self.ready.remove(&u);
                self.finish(u);
            }
        }
        let threads = executor.threads();
        while self.done < self.units.len() {
            while let Some(&u) = self.ready.first() {
                let Some((job, estimate)) = self.next_job(u) else {
                    self.ready.remove(&u);
                    let Unit::Scc(scc) = &mut self.units[u] else {
                        unreachable!("only a split SCC has no next job");
                    };
                    let rounds = scc.finish(self.stage)?;
                    self.stats.split_sccs.push(rounds);
                    self.finish(u);
                    continue;
                };
                let used: usize = self.running.values().map(|r| r.2).sum();
                let held: u64 = self.running.values().map(|r| r.1).sum();
                if !self.running.is_empty()
                    && (used >= threads || held.saturating_add(estimate) > self.plan.budget)
                {
                    break;
                }
                let alone = self.running.is_empty() && self.ready.len() == 1;
                log::debug!(
                    "{job}: starting, estimate {:.1} MB, {:.1} MB of estimates running",
                    estimate as f64 / MB,
                    held as f64 / MB
                );
                executor.start(self.stage, job, alone)?;
                self.ready.remove(&u);
                self.running
                    .insert(job, (u, estimate, if alone { threads } else { 1 }));
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
            let (job, result) = executor.wait();
            result?;
            let (u, _, _) = self
                .running
                .remove(&job)
                .expect("the executor ended a job it was given");
            match (&mut self.units[u], job) {
                (
                    Unit::Scc(scc),
                    Job::Batch {
                        id,
                        round: Some(round),
                    },
                ) => {
                    scc.ran(self.stage, id, round)?;
                    self.ready.insert(u);
                }
                _ => self.finish(u),
            }
        }
        self.stats.split_sccs.sort_by_key(|s| s.scc);
        Ok(self.stats)
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
        let parts: Vec<_> = plan.split_sccs[scc as usize]
            .parts
            .iter()
            .map(|b| &plan.batches[*b as usize])
            .collect();
        // Each part's callees in other parts: those of the batches it does not depend on.
        let callees = parts
            .iter()
            .map(|part| {
                part.callees
                    .iter()
                    .filter(|(_, b)| part.deps.binary_search(b).is_err())
                    .map(|(f, _)| *f)
                    .collect()
            })
            .collect();
        Self {
            scc,
            parts: parts.iter().map(|p| p.id).collect(),
            callees,
            rows: HashMap::new(),
            seen: vec![None; parts.len()],
            runs: vec![0; parts.len()],
            latest: LatestRounds::default(),
            round: 0,
            next: 0,
            ran: 0,
            start: None,
            round_start: None,
        }
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
    fn ran(&mut self, stage: &StageDir, batch: BatchId, round: u32) -> Result<(), Error> {
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
            #[cfg(debug_assertions)]
            assert_rows_grew(stage, self.scc, batch, previous, round);
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
