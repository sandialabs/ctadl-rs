//! Staged index runs whose batches run in `ctadl index-worker` processes.

use std::path::{Path, PathBuf};

use ctadl_ascent::codegen::flowy::program_facts;
use ctadl_ascent::error::Error;
use ctadl_ascent::facts::IdMap;
use ctadl_ascent::index_engine::{IndexConfig, IndexFacts, Parallelism, StagedConfig};
use ctadl_ascent::staged::compare::compare_index_dirs;
use ctadl_ascent::staged::executor::{Executor, InProcess, Process};
use ctadl_ascent::staged::run::{RunStats, run_staged, run_staged_until};
use ctadl_ascent::staged::worker::{TEST_HOOK_FAIL_STATUS, TEST_HOOK_VAR};
use ctadl_index::staged::plan::Plan;
use ctadl_index::staged::store::{Stage, StageDir};

/// A budget no two SCCs fit together, so each is a batch of its own and runs alone.
const ONE_SCC_PER_BATCH: StagedConfig = StagedConfig {
    budget: 0,
    floor: usize::MAX,
};

/// A budget everything fits, with every recursive SCC split into single functions, so that
/// batches and the parts of split SCCs run beside one another.
const FLOOR_ONE: StagedConfig = StagedConfig {
    budget: u64::MAX,
    floor: 1,
};

/// A budget each fixture fits in one batch, which the `footprint` test hook can exceed.
const ONE_GIB: StagedConfig = StagedConfig {
    budget: 1 << 30,
    floor: usize::MAX,
};

fn tnt(name: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "tnt", name]
        .iter()
        .collect()
}

/// The facts of the flowy fixture `name` and the map that names their functions.
fn facts(name: &str) -> (IndexFacts, IdMap) {
    let (facts, source_info, _) = program_facts(&tnt(name), &[]).unwrap();
    (facts, source_info.sites)
}

/// The facts of a ring of four functions, each calling the next with its two arguments
/// swapped, and the map that names their functions. Its summaries grow over several rounds.
fn ring_facts(dir: &Path) -> (IndexFacts, IdMap) {
    let mut src = String::new();
    for i in 0..4 {
        let next = (i + 1) % 4;
        src.push_str(&format!(
            "def F{i}(a, b): 1\n{{\ns:\n  r = F{next}(b, a);\n  goto x, y;\nx:\n  return r;\ny:\n  return a;\n}}\n\n"
        ));
    }
    let file = dir.join("ring.tnt");
    std::fs::write(&file, src).unwrap();
    let (facts, source_info, _) = program_facts(&file, &[]).unwrap();
    (facts, source_info.sites)
}

/// Runs `facts` staged on the calling thread into a fresh index directory.
fn in_process(facts: IndexFacts, staged: StagedConfig, id_map: &IdMap) -> tempfile::TempDir {
    let index = tempfile::tempdir().unwrap();
    let mut executor = InProcess::new(Parallelism::Serial, Some(id_map));
    run(facts, staged, index.path(), &mut executor).unwrap();
    index
}

fn worker_exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_ctadl"))
}

/// Runs `facts` staged on `executor` into `index`.
fn run(
    facts: IndexFacts,
    staged: StagedConfig,
    index: &Path,
    executor: &mut dyn Executor,
) -> Result<(), ctadl_ascent::error::Error> {
    run_staged(facts, &IndexConfig::default(), staged, index, executor).map(|_| ())
}

/// Every fixture, through worker processes, has the tables of the run on the calling thread:
/// one SCC at a time, and many batches and parts at once on several threads.
#[test]
fn process_executor_matches_in_process() {
    let mut split = false;
    for name in [
        "funcptr_through_indirect.tnt",
        "value_flow.tnt",
        "poly.tnt",
        "recursion.tnt",
    ] {
        let (facts, id_map) = facts(name);
        for (staged, parallelism) in [
            (ONE_SCC_PER_BATCH, Parallelism::Serial),
            (FLOOR_ONE, Parallelism::from_jobs(4)),
        ] {
            let in_process = tempfile::tempdir().unwrap();
            let workers = tempfile::tempdir().unwrap();
            run(
                facts.clone(),
                staged,
                in_process.path(),
                &mut InProcess::new(Parallelism::Serial, Some(&id_map)),
            )
            .unwrap();
            run(
                facts.clone(),
                staged,
                workers.path(),
                &mut Process::new(worker_exe(), parallelism),
            )
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let diff =
                compare_index_dirs(in_process.path(), workers.path(), Some(&id_map)).unwrap();
            assert!(diff.is_empty(), "{name}: in process < > workers\n{diff}");
            let plan = Plan::load(&StageDir::new(workers.path())).unwrap();
            split |= !plan.split_sccs.is_empty();
        }
    }
    assert!(split, "some fixture has a recursive SCC to split");
}

/// A worker that logs a lot finishes: its stderr goes to a file, not a pipe that fills.
#[test]
fn verbose_worker_does_not_block() {
    let (facts, _) = facts("value_flow.tnt");
    let index = tempfile::tempdir().unwrap();
    let mut executor = Process::new(worker_exe(), Parallelism::Serial)
        .env("RUST_LOG", "trace")
        .env(TEST_HOOK_VAR, "noise=batch-0");
    run(facts, ONE_SCC_PER_BATCH, index.path(), &mut executor).unwrap();
    let logged: u64 = std::fs::read_dir(StageDir::new(index.path()).logs_dir())
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .max()
        .unwrap();
    assert!(
        logged > 1 << 20,
        "the largest worker log has {logged} bytes"
    );
}

/// A worker that fails fails the run with its log, and the batches done before it stay done.
#[test]
fn worker_failure_fails_run() {
    let (facts, _) = facts("value_flow.tnt");
    let index = tempfile::tempdir().unwrap();
    let mut executor =
        Process::new(worker_exe(), Parallelism::Serial).env(TEST_HOOK_VAR, "fail=batch-1");
    let error = run(facts, ONE_SCC_PER_BATCH, index.path(), &mut executor)
        .expect_err("batch 1's worker fails");
    let message = format!("{error:?}");
    assert!(
        message.contains("batch 1") && message.contains(TEST_HOOK_VAR),
        "{message}"
    );
    let stage = StageDir::new(index.path());
    assert!(stage.is_done(Stage::Batch(0)));
    assert!(!stage.is_done(Stage::Batch(1)));
}

/// Each worker maps the run's frozen path set instead of reading and freezing its paths, and its
/// tables are those of the run on the calling thread.
#[test]
fn worker_maps_path_set() {
    let (facts, id_map) = facts("poly.tnt");
    let in_process = tempfile::tempdir().unwrap();
    let workers = tempfile::tempdir().unwrap();
    run(
        facts.clone(),
        ONE_SCC_PER_BATCH,
        in_process.path(),
        &mut InProcess::new(Parallelism::Serial, Some(&id_map)),
    )
    .unwrap();
    let mut executor = Process::new(worker_exe(), Parallelism::Serial).env("RUST_LOG", "info");
    run(facts, ONE_SCC_PER_BATCH, workers.path(), &mut executor).unwrap();
    let diff = compare_index_dirs(in_process.path(), workers.path(), Some(&id_map)).unwrap();
    assert!(diff.is_empty(), "in process < > workers\n{diff}");
    let logs: Vec<String> = std::fs::read_dir(StageDir::new(workers.path()).logs_dir())
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(!logs.is_empty());
    for log in logs {
        assert!(log.contains("mapped the path set of"), "{log}");
    }
}

fn serial_workers() -> Process {
    Process::new(worker_exe(), Parallelism::Serial)
}

/// Runs `facts` staged on serial workers into `index` under `config`, stopping after batch
/// `stop_after`, and expects it to stop there.
fn stop_after(
    facts: IndexFacts,
    config: &IndexConfig,
    staged: StagedConfig,
    index: &Path,
    batch: u32,
) {
    let stopped = run_staged_until(
        facts,
        config,
        staged,
        index,
        &mut serial_workers(),
        Some(batch),
    );
    assert!(
        matches!(stopped, Err(Error::StagedStopped(b)) if b == batch),
        "{stopped:?}"
    );
}

/// Runs `facts` on serial workers into a fresh index directory, from scratch.
fn reference(facts: IndexFacts, config: &IndexConfig, staged: StagedConfig) -> tempfile::TempDir {
    let index = tempfile::tempdir().unwrap();
    let stats = run_staged(facts, config, staged, index.path(), &mut serial_workers()).unwrap();
    assert!(!stats.resumed);
    index
}

fn assert_same_tables(expected: &Path, got: &Path, id_map: &IdMap) {
    let diff = compare_index_dirs(expected, got, Some(id_map)).unwrap();
    assert!(diff.is_empty(), "uninterrupted < > resumed\n{diff}");
}

/// A run stopped after batch 1 writes no table. Rerun, it skips batches 0 and 1 and writes the
/// tables of a run that was never stopped.
#[test]
fn killed_run_resumes_to_same_tables() {
    let (facts, id_map) = facts("value_flow.tnt");
    let config = IndexConfig::default();
    let expected = reference(facts.clone(), &config, ONE_SCC_PER_BATCH);
    let index = tempfile::tempdir().unwrap();
    stop_after(facts.clone(), &config, ONE_SCC_PER_BATCH, index.path(), 1);
    let stage = StageDir::new(index.path());
    assert!(stage.is_done(Stage::Batch(0)) && stage.is_done(Stage::Batch(1)));
    assert!(!stage.is_done(Stage::Batch(2)));
    assert!(!index.path().join("summary.parquet").exists());

    let stats = run_staged(
        facts,
        &config,
        ONE_SCC_PER_BATCH,
        index.path(),
        &mut serial_workers(),
    )
    .unwrap();
    assert!(stats.resumed);
    assert_eq!(stats.skipped, vec![0, 1]);
    assert_same_tables(expected.path(), index.path(), &id_map);
}

/// A run stopped midway and rerun with another configuration skips nothing, and writes the
/// tables of that configuration.
#[test]
fn changed_config_starts_over() {
    let (facts, id_map) = facts("value_flow.tnt");
    let changed = IndexConfig {
        alias_rule: false,
        ..IndexConfig::default()
    };
    let expected = reference(facts.clone(), &changed, ONE_SCC_PER_BATCH);
    let index = tempfile::tempdir().unwrap();
    stop_after(
        facts.clone(),
        &IndexConfig::default(),
        ONE_SCC_PER_BATCH,
        index.path(),
        1,
    );
    let stats = run_staged(
        facts,
        &changed,
        ONE_SCC_PER_BATCH,
        index.path(),
        &mut serial_workers(),
    )
    .unwrap();
    assert!(!stats.resumed);
    assert!(stats.skipped.is_empty(), "{:?}", stats.skipped);
    assert_same_tables(expected.path(), index.path(), &id_map);
}

/// Files no marker vouches for are removed when a run resumes, before any batch runs: here, a
/// batch's outputs, the frame's and a temporary file. Batch 1's worker then fails, so nothing
/// rewrites them.
#[test]
fn unmarked_files_are_discarded() {
    let (facts, id_map) = facts("value_flow.tnt");
    let config = IndexConfig::default();
    let expected = reference(facts.clone(), &config, ONE_SCC_PER_BATCH);
    let index = tempfile::tempdir().unwrap();
    stop_after(facts.clone(), &config, ONE_SCC_PER_BATCH, index.path(), 0);
    let stage = StageDir::new(index.path());
    let planted = [
        stage.batch_dir(1).join("summary.parquet"),
        stage.frame_dir().join("frame_edge.parquet"),
        stage.batch_dir(0).join(".summary.parquet.1.0.tmp"),
    ];
    for file in &planted {
        std::fs::write(file, b"not parquet").unwrap();
    }

    let mut failing = serial_workers().env(TEST_HOOK_VAR, "fail=batch-1");
    let error = run(facts.clone(), ONE_SCC_PER_BATCH, index.path(), &mut failing)
        .expect_err("batch 1's worker fails");
    assert!(
        format!("{error:?}").contains(&format!("exit status: {TEST_HOOK_FAIL_STATUS}")),
        "{error:?}"
    );
    for file in &planted {
        assert!(!file.exists(), "{} is still there", file.display());
    }
    assert!(stage.is_done(Stage::Batch(0)));
    assert!(stage.batch_dir(0).join("summary.parquet").exists());

    run(
        facts,
        ONE_SCC_PER_BATCH,
        index.path(),
        &mut serial_workers(),
    )
    .unwrap();
    assert_same_tables(expected.path(), index.path(), &id_map);
}

/// A run that fails in the first round of a split SCC, after one of its parts ran, resumes the
/// SCC's rounds from that part's run, and writes the tables of a run that never failed.
#[test]
fn failed_split_scc_resumes() {
    let (facts, id_map) = facts("recursion.tnt");
    let config = IndexConfig::default();
    let expected = reference(facts.clone(), &config, FLOOR_ONE);
    let plan = Plan::load(&StageDir::new(expected.path())).unwrap();
    let parts = &plan
        .split_sccs
        .iter()
        .find(|s| s.parts.len() >= 2)
        .expect("recursion.tnt has a recursive SCC of two functions or more")
        .parts;

    let index = tempfile::tempdir().unwrap();
    let mut failing =
        serial_workers().env(TEST_HOOK_VAR, format!("fail=batch-{}-round-0", parts[1]));
    run(facts.clone(), FLOOR_ONE, index.path(), &mut failing).expect_err("a part's worker fails");
    let stage = StageDir::new(index.path());
    assert!(stage.is_done(Stage::Round {
        batch: parts[0],
        round: 0
    }));

    let stats: RunStats = run_staged(
        facts,
        &config,
        FLOOR_ONE,
        index.path(),
        &mut serial_workers(),
    )
    .unwrap();
    assert!(stats.resumed);
    assert_same_tables(expected.path(), index.path(), &id_map);
}

/// A batch whose worker reports more than the budget is killed, split and rerun until its
/// halves fit, and the run has the tables of the run on the calling thread: a batch of whole
/// SCCs is cut between them, a batch of one SCC becomes a split SCC, and a part of a split SCC
/// is cut into two parts, the second starting from a copy of the part's earlier run.
#[test]
fn over_budget_batch_is_split_and_rerun() {
    let dir = tempfile::tempdir().unwrap();
    // Batch 0 holds every function, and its worker reports 600 MB for each, until it holds
    // one. The ring's halves are parts of a split SCC, whose jobs the hook leaves alone.
    let cases = [
        (facts("value_flow.tnt"), ONE_GIB, "footprint=batch-0:600"),
        (facts("recursion.tnt"), ONE_GIB, "footprint=batch-0:600"),
        // At floor 2 the ring is two parts of two functions. Part 0 reruns in round 1, and is
        // killed then.
        (
            ring_facts(dir.path()),
            StagedConfig {
                floor: 2,
                ..ONE_GIB
            },
            "footprint=batch-0-round-1:600",
        ),
    ];
    for ((facts, id_map), staged, hook) in cases {
        let expected = in_process(facts.clone(), staged, &id_map);
        let index = tempfile::tempdir().unwrap();
        // Every other worker reports nothing, so that only the hook's report is over.
        let mut executor = serial_workers().env(TEST_HOOK_VAR, format!("{hook},footprint=*:0"));
        let stats = run_staged(
            facts,
            &IndexConfig::default(),
            staged,
            index.path(),
            &mut executor,
        )
        .unwrap_or_else(|e| panic!("{hook}: {e:?}"));
        let diff = compare_index_dirs(expected.path(), index.path(), Some(&id_map)).unwrap();
        assert!(diff.is_empty(), "{hook}: in process < > split\n{diff}");
        assert!(stats.kills >= 1, "{hook}: {stats:?}");
        assert_eq!(stats.splits.len() as u32, stats.kills, "{hook}: {stats:?}");
        assert_eq!(stats.splits[0].0, 0, "{hook}: {stats:?}");
        let plan = Plan::load(&StageDir::new(index.path())).unwrap();
        assert!(
            plan.batches[0].functions.len() * 600 <= 1024,
            "{hook}: batch 0 still holds {} functions",
            plan.batches[0].functions.len()
        );
        if hook.contains("round") {
            assert_eq!(plan.split_sccs[0].parts.len(), 3, "{hook}: {plan:?}");
        } else if !plan.split_sccs.is_empty() {
            assert!(plan.split_sccs[0].parts.contains(&0), "{hook}: {plan:?}");
        }
    }
}

/// A worker of one function over the budget is never killed: it runs alone to its end, and the
/// run says so.
#[test]
fn single_function_is_never_killed() {
    let (facts, id_map) = facts("value_flow.tnt");
    let expected = in_process(facts.clone(), ONE_SCC_PER_BATCH, &id_map);
    let index = tempfile::tempdir().unwrap();
    // Every batch is one function. Batch 3 reports 2000 MB against a budget of 0, and every
    // other batch nothing.
    let mut executor = serial_workers().env(TEST_HOOK_VAR, "footprint=batch-3:2000,footprint=*:0");
    let stats = run_staged(
        facts,
        &IndexConfig::default(),
        ONE_SCC_PER_BATCH,
        index.path(),
        &mut executor,
    )
    .unwrap();
    assert_eq!(stats.kills, 0, "{stats:?}");
    assert_eq!(stats.over_budget_alone, vec![3]);
    assert!(stats.splits.is_empty());
    let stage = StageDir::new(index.path());
    assert!(stage.is_done(Stage::Batch(3)));
    let plan = Plan::load(&stage).unwrap();
    assert_eq!(plan.batches[3].functions.len(), 1);
    let diff = compare_index_dirs(expected.path(), index.path(), Some(&id_map)).unwrap();
    assert!(diff.is_empty(), "in process < > workers\n{diff}");
}

/// When every estimate is half the footprint, the first workers that run at once are over the
/// budget, and a kill raises every estimate, so that few kills follow, and no batch that fits
/// alone is split.
#[test]
fn uniform_underestimate_kills_few() {
    let (facts, id_map) = facts("value_flow.tnt");
    // Batches of about a third of the functions, so that two halved estimates fit together
    // and several batches are ready at once.
    let plan = Plan::load(&StageDir::new(
        in_process(facts.clone(), ONE_SCC_PER_BATCH, &id_map).path(),
    ))
    .unwrap();
    let rows: u64 = plan
        .batches
        .iter()
        .map(|b| b.estimate - plan.worker_bytes)
        .sum();
    let staged = StagedConfig::with_budget(plan.worker_bytes + rows / 3);
    let expected = in_process(facts.clone(), staged, &id_map);
    let index = tempfile::tempdir().unwrap();
    // Each worker reports its batch's estimate, which the scheduler halves.
    let mut executor = Process::new(worker_exe(), Parallelism::from_jobs(4))
        .env(TEST_HOOK_VAR, "footprint=*:estimate,scale=*:0.5");
    let stats = run_staged(
        facts,
        &IndexConfig::default(),
        staged,
        index.path(),
        &mut executor,
    )
    .unwrap();
    let plan = Plan::load(&StageDir::new(index.path())).unwrap();
    let batches = plan.batches.len();
    assert!(batches >= 3, "{plan:?}");
    assert!(stats.kills >= 1, "{stats:?}");
    assert!(
        stats.kills <= (batches as f64).log2().ceil() as u32,
        "{} kills of {batches} batches: {stats:?}",
        stats.kills
    );
    assert!(stats.splits.is_empty(), "{stats:?}");
    assert!(stats.alpha > 1.9, "{stats:?}");
    let diff = compare_index_dirs(expected.path(), index.path(), Some(&id_map)).unwrap();
    assert!(diff.is_empty(), "in process < > workers\n{diff}");
}

/// A run that split a batch and then stopped resumes from the plan it saved, splits and all,
/// rather than from the plan of the facts, and writes the tables of a run that never stopped.
#[test]
fn split_run_resumes() {
    let (facts, id_map) = facts("value_flow.tnt");
    let config = IndexConfig::default();
    let expected = in_process(facts.clone(), ONE_GIB, &id_map);
    let index = tempfile::tempdir().unwrap();
    let mut executor = serial_workers().env(TEST_HOOK_VAR, "footprint=batch-0:600,footprint=*:0");
    let stopped = run_staged_until(
        facts.clone(),
        &config,
        ONE_GIB,
        index.path(),
        &mut executor,
        Some(0),
    );
    assert!(
        matches!(stopped, Err(Error::StagedStopped(0))),
        "{stopped:?}"
    );
    let split = Plan::load(&StageDir::new(index.path())).unwrap();
    assert!(split.batches.len() > 1);

    let stats = run_staged(facts, &config, ONE_GIB, index.path(), &mut serial_workers()).unwrap();
    assert!(stats.resumed);
    assert_eq!(stats.skipped, vec![0]);
    assert!(stats.splits.is_empty());
    assert_eq!(Plan::load(&StageDir::new(index.path())).unwrap(), split);
    assert_same_tables(expected.path(), index.path(), &id_map);
}
