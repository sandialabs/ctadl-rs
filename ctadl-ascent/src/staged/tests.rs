//! A staged run must derive the monolithic serial run's tables, compared as sets by content,
//! however the program is batched and whatever order its facts come in.

use std::collections::BTreeSet;

use super::batch::run_batch;
use super::compare::compare_results;
use super::executor::{Executor, InProcess, Job};
use super::run::{RunStats, SccRounds, run_staged};
use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, GlobalId, IdMap, InsnId,
    PackedInsnSiteId, Path, Str,
};
use crate::index_engine::{
    IndexConfig, IndexFacts, IndexResult, Parallelism, StagedConfig, taint_index_with_config,
};
use crate::index_engine_tests::{SRC, index_program, intent_facts, program_from_string};
use ctadl_index::facts::schema;
use ctadl_index::staged::files::batch_file;
use ctadl_index::staged::plan::{BatchId, Plan};
use ctadl_index::staged::shuffle::shuffle_facts;
use ctadl_index::staged::store::StageDir;

/// A budget no two SCCs fit together, so each is a batch of its own.
const ONE_SCC_PER_BATCH: StagedConfig = StagedConfig {
    budget: 0,
    floor: usize::MAX,
};

/// Runs `facts` staged in a temporary index directory, and returns the tables, the plan and
/// what the run did.
fn run(
    facts: IndexFacts,
    staged: StagedConfig,
    id_map: Option<&IdMap>,
) -> (IndexResult, Plan, RunStats) {
    let index = tempfile::tempdir().unwrap();
    let mut executor = InProcess::new(Parallelism::Serial, id_map);
    let stats = run_staged(
        facts,
        &IndexConfig::default(),
        staged,
        index.path(),
        &mut executor,
    )
    .unwrap();
    let plan = Plan::load(&StageDir::new(index.path())).unwrap();
    (IndexResult::try_load(index.path()).unwrap(), plan, stats)
}

/// Asserts that `facts` staged, shuffled by each of `seeds` (and unshuffled when there are
/// none), has the monolithic serial run's tables. Returns each run's plan and what it did.
fn assert_staged_matches(
    facts: &IndexFacts,
    id_map: Option<&IdMap>,
    staged: StagedConfig,
    seeds: &[u64],
    what: &str,
) -> Vec<(Plan, RunStats)> {
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), id_map);
    let seeds: Vec<Option<u64>> = if seeds.is_empty() {
        vec![None]
    } else {
        seeds.iter().copied().map(Some).collect()
    };
    seeds
        .into_iter()
        .map(|seed| {
            let mut facts = facts.clone();
            if let Some(seed) = seed {
                shuffle_facts(&mut facts, seed);
            }
            let (result, plan, stats) = run(facts, staged, id_map);
            let diff = compare_results(&monolithic, &result, id_map);
            assert!(
                diff.is_empty(),
                "{what}, seed {seed:?}, {} batches: monolithic < > staged\n{diff}",
                plan.batches.len()
            );
            (plan, stats)
        })
        .collect()
}

/// The number of batches of each plan of `runs`.
fn batch_counts(runs: &[(Plan, RunStats)]) -> Vec<usize> {
    runs.iter().map(|(plan, _)| plan.batches.len()).collect()
}

fn assert_c_matches(src: &str, batches: usize) {
    let (facts, source_info) = index_program(program_from_string(src).0);
    let runs = assert_staged_matches(
        &facts,
        Some(&source_info.sites),
        ONE_SCC_PER_BATCH,
        &[1, 2, 3],
        src,
    );
    let counts = batch_counts(&runs);
    assert!(counts.iter().all(|&n| n == batches), "{counts:?}");
}

#[test]
fn staged_matches_monolithic_chain() {
    assert_c_matches(
        r"
        int c(int p) { return p; }
        int b(int p) { return c(p); }
        int a(int p) { return b(p); }",
        3,
    );
}

const DIAMOND: &str = r"
    struct s { int f; int g; };
    int d(struct s *x) { return x->f; }
    int b(struct s *x) { x->g = d(x); return x->g; }
    int c(struct s *x) { return d(x) + 1; }
    int a(struct s *x) { return b(x) + c(x); }";

#[test]
fn staged_matches_monolithic_diamond() {
    assert_c_matches(DIAMOND, 4);
}

/// Flow through globals across batches: a summary rooted at a global instantiated in a caller,
/// and flows between globals made in two batches.
#[test]
fn staged_matches_monolithic_globals() {
    assert_c_matches(
        r"
        int g; int h; int k;
        void set(int x) { g = x; }
        void copy1(void) { h = g; }
        void copy2(void) { k = h; }
        int get(int y) { set(y); copy1(); copy2(); return k + g; }",
        4,
    );
}

/// Direct and indirect calls, summaries and field flow, in one batch and in many.
#[test]
fn staged_matches_monolithic_calls() {
    let (facts, source_info) = index_program(program_from_string(SRC).0);
    let id_map = Some(&source_info.sites);
    let one = assert_staged_matches(
        &facts,
        id_map,
        StagedConfig::with_budget(u64::MAX),
        &[],
        "one batch",
    );
    assert_eq!(batch_counts(&one), vec![1]);
    let many = assert_staged_matches(&facts, id_map, ONE_SCC_PER_BATCH, &[1, 2, 3], "many");
    let many = batch_counts(&many);
    assert!(many.iter().all(|&n| n > 1), "{many:?}");
}

/// A function with assignments and no formals is planned and run, and its rows kept.
#[test]
fn staged_matches_monolithic_function_without_formals() {
    let (no_formals, id, caller) = (FunctionId::new(1), FunctionId::new(2), FunctionId::new(3));
    let site = |f, i| PackedInsnSiteId::try_from_parts(f, InsnId::new(i)).unwrap();
    let local = |s: &str| FlowVertex::new(FlowVariable::local(Str::from(s)), Path::empty());
    let formal = |i| FlowVariable::formal_index(FormalIndex::new(i));
    let ret = FlowVertex::new(formal(-1), Path::empty());
    let facts = IndexFacts {
        formal_param: vec![
            (id, formal(0), FormalType::default()),
            (id, formal(-1), FormalType::default()),
            (caller, formal(0), FormalType::default()),
            (caller, formal(-1), FormalType::default()),
        ],
        assign: vec![
            (site(no_formals, 1), local("a"), local("b")),
            (site(no_formals, 2), local("c"), local("a")),
            (site(id, 1), ret, FlowVertex::new(formal(0), Path::empty())),
            (site(caller, 1), ret, local("r")),
        ],
        actual_param: vec![
            (
                site(caller, 5),
                FormalIndex::new(0),
                FlowVertex::new(formal(0), Path::empty()),
            ),
            (site(caller, 5), FormalIndex::new(-1), local("r")),
        ],
        call: vec![(site(caller, 5), id), (site(caller, 6), no_formals)],
        ..Default::default()
    };
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), None);
    assert!(monolithic.assign_like.iter().any(|r| r.0 == no_formals));
    assert!(monolithic.summary.iter().any(|r| r.0 == caller));
    let counts = batch_counts(&assert_staged_matches(
        &facts,
        None,
        ONE_SCC_PER_BATCH,
        &[1, 2, 3],
        "no formals",
    ));
    assert!(counts.iter().all(|&n| n == 3), "{counts:?}");
}

/// Intent pairs, one resolved through the label exchange across the global frame, with the
/// sender, the setter of the global and the receivers in batches of their own.
#[test]
fn staged_matches_monolithic_intent() {
    let facts = intent_facts();
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), None);
    assert_eq!(monolithic.intent_pair.len(), 2);
    let counts = batch_counts(&assert_staged_matches(
        &facts,
        None,
        ONE_SCC_PER_BATCH,
        &[1, 2, 3],
        "intent",
    ));
    assert!(counts.iter().all(|&n| n == 4), "{counts:?}");
}

/// The program-level flows a `--summary` project loads are edges of the global frame, beside
/// those the batches find, each once.
#[test]
fn staged_matches_monolithic_summary_project() {
    let (reader, writer, copier) = (FunctionId::new(1), FunctionId::new(2), FunctionId::new(3));
    let site = |f, i| PackedInsnSiteId::try_from_parts(f, InsnId::new(i)).unwrap();
    let global = |name: &str| GlobalId::new(name);
    let gv = |name: &str| FlowVertex::new(FlowVariable::global(global(name)), Path::empty());
    let formal = |i| FlowVariable::formal_index(FormalIndex::new(i));
    let ret = FlowVertex::new(formal(-1), Path::empty());
    let facts = IndexFacts {
        formal_param: vec![
            (reader, formal(-1), FormalType::default()),
            (writer, formal(0), FormalType::default()),
        ],
        assign: vec![
            // reader: return $g2
            (site(reader, 1), ret, gv("g2")),
            // writer: $g1 = p0
            (
                site(writer, 1),
                gv("g1"),
                FlowVertex::new(formal(0), Path::empty()),
            ),
            // copier: $g3 = $g1, an edge of the frame
            (site(copier, 1), gv("g3"), gv("g1")),
        ],
        global_flow: vec![
            (global("g2"), Path::empty(), global("g1"), Path::empty()),
            // Also the copier's edge.
            (global("g3"), Path::empty(), global("g1"), Path::empty()),
        ],
        ..Default::default()
    };
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), None);
    assert_eq!(monolithic.global_flow.len(), 2);
    assert_staged_matches(
        &facts,
        None,
        ONE_SCC_PER_BATCH,
        &[1, 2, 3],
        "summary project",
    );
}

/// Every flowy fixture, on facts in three orders, one SCC per batch, which splits every
/// recursive SCC into single functions, and in batches as large as can be, with recursive SCCs
/// split at floors 4, 2 and 1.
#[test]
fn staged_matches_monolithic_flowy_floor_sweep() {
    let dir: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "tnt"]
        .iter()
        .collect();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "tnt"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut split = false;
    for file in files {
        let models: Vec<_> = ["models.jsonl", "models.ctadl"]
            .iter()
            .map(|ext| file.with_extension(ext))
            .filter(|p| p.exists())
            .collect();
        let (facts, source_info, _) = crate::codegen::flowy::program_facts(&file, &models)
            .unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
        for staged in [
            ONE_SCC_PER_BATCH,
            StagedConfig {
                budget: u64::MAX,
                floor: 4,
            },
            StagedConfig {
                budget: u64::MAX,
                floor: 2,
            },
            StagedConfig {
                budget: u64::MAX,
                floor: 1,
            },
        ] {
            let runs = assert_staged_matches(
                &facts,
                Some(&source_info.sites),
                staged,
                &[1, 2, 3],
                &format!("{}, floor {}", file.display(), staged.floor),
            );
            split |= runs.iter().any(|(plan, _)| !plan.split_sccs.is_empty());
        }
    }
    assert!(split, "some fixture has a recursive SCC to split");
}

/// A batch reads its own facts and the `summary` files of the batches that hold its callees,
/// and nothing else.
#[test]
fn batch_reads_only_callees() {
    let (facts, source_info) = index_program(program_from_string(DIAMOND).0);
    let id_map = Some(&source_info.sites);
    let index = tempfile::tempdir().unwrap();
    let mut executor = InProcess::new(Parallelism::Serial, id_map);
    run_staged(
        facts,
        &IndexConfig::default(),
        ONE_SCC_PER_BATCH,
        index.path(),
        &mut executor,
    )
    .unwrap();
    let stage = StageDir::new(index.path());
    let plan = Plan::load(&stage).unwrap();
    let facts_dir = stage.facts_dir();
    let mut with_callees = 0;
    for batch in &plan.batches {
        let read = run_batch(&stage, batch.id, None, Parallelism::Serial, id_map)
            .unwrap()
            .read;
        let callee_batches: BTreeSet<BatchId> = batch.callees.iter().map(|(_, b)| *b).collect();
        let expected: BTreeSet<_> = callee_batches
            .iter()
            .map(|b| batch_file(&stage, *b, schema::summary::FILENAME))
            .collect();
        let outside: BTreeSet<_> = read
            .iter()
            .filter(|f| !f.starts_with(&facts_dir))
            .cloned()
            .collect();
        assert_eq!(outside, expected, "batch {}", batch.id);
        with_callees += usize::from(!callee_batches.is_empty());
    }
    assert_eq!(with_callees, 3, "b, c and a call another batch's function");
}

/// The function `name` of the program `id_map` names.
fn function(id_map: &IdMap, name: &str) -> FunctionId {
    id_map
        .functions()
        .find(|(_, f)| f.as_str() == name)
        .unwrap_or_else(|| panic!("no function {name}"))
        .0
}

/// The runs of a split SCC: its plan and its rounds.
struct SplitRuns {
    plan: Plan,
    scc: SccRounds,
}

impl SplitRuns {
    /// How many times the part holding `f` ran.
    fn runs_of(&self, f: FunctionId) -> u32 {
        let batch = self
            .plan
            .batches
            .iter()
            .find(|b| b.functions.contains(&f))
            .unwrap()
            .id;
        self.scc
            .part_runs
            .iter()
            .find(|(b, _)| *b == batch)
            .unwrap()
            .1
    }

    fn part_runs(&self) -> u32 {
        self.scc.part_runs.iter().map(|(_, runs)| runs).sum()
    }
}

/// Indexes `src` at `staged`, which must split exactly one SCC, compares it with the
/// monolithic run on facts in three orders, and returns each run's split SCC with the
/// program's names.
fn split_scc_runs(src: &str, staged: StagedConfig) -> (Vec<SplitRuns>, IdMap) {
    let (facts, source_info) = index_program(program_from_string(src).0);
    let runs = assert_staged_matches(&facts, Some(&source_info.sites), staged, &[1, 2, 3], src);
    let runs = runs
        .into_iter()
        .map(|(plan, stats)| {
            assert_eq!(plan.split_sccs.len(), 1, "{plan:?}");
            let [scc] = <[SccRounds; 1]>::try_from(stats.split_sccs).unwrap();
            log::info!("{} parts: {scc:?}", plan.split_sccs[0].parts.len());
            SplitRuns { plan, scc }
        })
        .collect();
    (runs, source_info.sites)
}

const FLOOR_ONE: StagedConfig = StagedConfig {
    budget: u64::MAX,
    floor: 1,
};

/// A ring of `n` functions of three formals, each calling the next with its formals rotated,
/// `(p, q, r)` to `(q, r, p)`, and the first returning `p` too. Every function returns each of
/// its formals, but a flow takes a lap of the ring per rotation to show it, and with the ring
/// split into single functions, a lap is a round.
fn ring(n: usize) -> String {
    let mut src = String::new();
    for i in 0..n {
        src += &format!("int f{i}(int p, int q, int r);\n");
    }
    for i in 0..n {
        let next = (i + 1) % n;
        let base = if i == 0 { "if (r) " } else { "" };
        src += &format!("int f{i}(int p, int q, int r) {{ {base}return f{next}(q, r, p);");
        src += if i == 0 { " return p; }\n" } else { " }\n" };
    }
    src
}

/// Mutual recursion whose summaries feed each other: each of the two parts must run again
/// after the other's summary grows, more than once, before they close.
#[test_log::test]
fn split_scc_matches_monolithic() {
    let (runs, id_map) = split_scc_runs(&ring(2), FLOOR_ONE);
    for run in runs {
        assert!(run.scc.rounds >= 3, "{:?}", run.scc);
        for f in ["f0", "f1"] {
            assert!(run.runs_of(function(&id_map, f)) >= 3, "{:?}", run.scc);
        }
    }
}

/// The worst case for rounds: rings split into single functions, where a flow crosses the
/// ring once per round. Records the rounds and part runs.
#[test_log::test]
fn ring_rounds_are_recorded() {
    for n in [16, 64] {
        let (runs, _) = split_scc_runs(&ring(n), FLOOR_ONE);
        for run in runs {
            log::info!(
                "ring of {n}: {} rounds, {} part runs",
                run.scc.rounds,
                run.part_runs()
            );
            assert_eq!(run.scc.part_runs.len(), n);
            assert!(run.scc.rounds >= 3, "{:?}", run.scc);
        }
    }
}

/// In a cycle `a → b → c → a`, split into single functions, `c`'s summary does not depend on
/// `a`'s. The parts run `c`, `b`, `a`, callees first. `c` runs again once `a` has a summary,
/// and finds the same summary, so `b` and `a`, whose callees did not grow, run once.
#[test_log::test]
fn part_not_rerun_when_callees_stable() {
    let (runs, id_map) = split_scc_runs(
        r"
        int b(int x);
        int c(int x);
        int a(int x) { return b(x); }
        int b(int x) { return c(x); }
        int c(int x) { a(0); return x; }",
        FLOOR_ONE,
    );
    let runs_of = |run: &SplitRuns, f| run.runs_of(function(&id_map, f));
    for run in runs {
        assert_eq!(run.scc.rounds, 2, "{:?}", run.scc);
        assert_eq!(runs_of(&run, "c"), 2);
        assert_eq!(runs_of(&run, "b"), 1);
        assert_eq!(runs_of(&run, "a"), 1);
    }
}

/// Starts each job when the scheduler starts it, but runs it on the calling thread only when the
/// scheduler waits, latest started first, so jobs end out of order. Records the jobs running
/// at each start.
struct Deferred<'a> {
    inner: InProcess<'a>,
    threads: usize,
    stage: Option<StageDir>,
    running: Vec<(Job, bool)>,
    at_once: Vec<Vec<Job>>,
}

impl Executor for Deferred<'_> {
    fn threads(&self) -> usize {
        self.threads
    }

    fn lend_path_set(
        &mut self,
        stage: &StageDir,
        path_set: crate::index_engine::path_set::PathSetRef,
    ) -> Result<(), crate::error::Error> {
        self.inner.lend_path_set(stage, path_set)
    }

    fn start(
        &mut self,
        stage: &StageDir,
        job: Job,
        alone: bool,
    ) -> Result<(), crate::error::Error> {
        self.stage = Some(stage.clone());
        self.running.push((job, alone));
        self.at_once
            .push(self.running.iter().map(|(job, _)| *job).collect());
        Ok(())
    }

    fn wait(&mut self) -> Option<(Job, Result<(), crate::error::Error>)> {
        let (job, alone) = self.running.pop().expect("a job was started");
        let stage = self.stage.as_ref().unwrap();
        self.inner.start(stage, job, alone).unwrap();
        self.inner.wait()
    }

    fn kill(&mut self, job: Job) {
        self.running.retain(|(j, _)| *j != job);
    }

    fn stop_all(&mut self) {
        self.running.clear();
    }
}

/// The scheduler runs at once as many jobs as there are threads and their estimates fit the
/// budget, never more, and a job that ends out of order changes no table.
#[test]
fn scheduler_runs_jobs_at_once_within_threads_and_budget() {
    // The diamond packs into one batch, and each ring is a split SCC whose parts run one at a
    // time, so three jobs at most can run at once.
    let other_ring = ring(4)
        .replace("int f", "int g")
        .replace("return f", "return g");
    let src = format!("{DIAMOND}\n{}\n{other_ring}", ring(4));
    let (facts, source_info) = index_program(program_from_string(&src).0);
    let id_map = Some(&source_info.sites);
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), id_map);
    let largest = {
        let index = tempfile::tempdir().unwrap();
        let mut executor = InProcess::new(Parallelism::Serial, id_map);
        run_staged(
            facts.clone(),
            &IndexConfig::default(),
            FLOOR_ONE,
            index.path(),
            &mut executor,
        )
        .unwrap();
        let plan = Plan::load(&StageDir::new(index.path())).unwrap();
        plan.batches.iter().map(|b| b.estimate).max().unwrap()
    };
    // The fewest and most jobs at once that each budget allows, with three threads. Any two
    // first runs fit twice the largest estimate.
    for (budget, fewest, most) in [(u64::MAX, 3, 3), (2 * largest, 2, 3), (0, 1, 1)] {
        let staged = StagedConfig {
            budget,
            ..FLOOR_ONE
        };
        let index = tempfile::tempdir().unwrap();
        let mut executor = Deferred {
            inner: InProcess::new(Parallelism::Serial, id_map),
            threads: 3,
            stage: None,
            running: Vec::new(),
            at_once: Vec::new(),
        };
        run_staged(
            facts.clone(),
            &IndexConfig::default(),
            staged,
            index.path(),
            &mut executor,
        )
        .unwrap();
        let plan = Plan::load(&StageDir::new(index.path())).unwrap();
        let estimate = |job: &Job| match job {
            Job::Batch { id, .. } => plan.batches[*id as usize].estimate,
            Job::Intent => 0,
        };
        let seen = executor.at_once.iter().map(Vec::len).max().unwrap();
        assert!(
            (fewest..=most).contains(&seen),
            "budget {budget}: {seen} at once: {:?}",
            executor.at_once
        );
        for jobs in executor.at_once.iter().filter(|jobs| jobs.len() > 1) {
            let sum: u64 = jobs.iter().map(estimate).sum();
            assert!(sum <= budget, "budget {budget}: {jobs:?} estimate {sum}");
        }
        let result = IndexResult::try_load(index.path()).unwrap();
        let diff = compare_results(&monolithic, &result, id_map);
        assert!(
            diff.is_empty(),
            "budget {budget}: monolithic < > staged\n{diff}"
        );
    }
}
