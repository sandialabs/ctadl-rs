//! A staged run must derive the monolithic serial run's tables, compared as sets by content,
//! however the program is batched and whatever order its facts come in.

use std::collections::BTreeSet;

use super::batch::run_batch;
use super::compare::compare_results;
use super::executor::InProcess;
use super::run::run_staged;
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

/// Runs `facts` staged in a temporary index directory, and returns the tables and the plan.
fn run(facts: IndexFacts, staged: StagedConfig, id_map: Option<&IdMap>) -> (IndexResult, Plan) {
    let index = tempfile::tempdir().unwrap();
    let mut executor = InProcess {
        parallelism: Parallelism::Serial,
        id_map,
    };
    run_staged(
        facts,
        &IndexConfig::default(),
        staged,
        index.path(),
        &mut executor,
    )
    .unwrap();
    let plan = Plan::load(&StageDir::new(index.path())).unwrap();
    (IndexResult::try_load(index.path()).unwrap(), plan)
}

/// Asserts that `facts` staged, shuffled by each of `seeds` (and unshuffled when there are
/// none), has the monolithic serial run's tables. Returns the number of batches of each run.
fn assert_staged_matches(
    facts: &IndexFacts,
    id_map: Option<&IdMap>,
    staged: StagedConfig,
    seeds: &[u64],
    what: &str,
) -> Vec<usize> {
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
            let (result, plan) = run(facts, staged, id_map);
            let diff = compare_results(&monolithic, &result, id_map);
            assert!(
                diff.is_empty(),
                "{what}, seed {seed:?}, {} batches: monolithic < > staged\n{diff}",
                plan.batches.len()
            );
            plan.batches.len()
        })
        .collect()
}

fn assert_c_matches(src: &str, batches: usize) {
    let (facts, source_info) = index_program(program_from_string(src).0);
    let counts = assert_staged_matches(
        &facts,
        Some(&source_info.sites),
        ONE_SCC_PER_BATCH,
        &[1, 2, 3],
        src,
    );
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
    assert_eq!(one, vec![1]);
    let many = assert_staged_matches(&facts, id_map, ONE_SCC_PER_BATCH, &[1, 2, 3], "many");
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
    let counts = assert_staged_matches(&facts, None, ONE_SCC_PER_BATCH, &[1, 2, 3], "no formals");
    assert!(counts.iter().all(|&n| n == 3), "{counts:?}");
}

/// Intent pairs, one resolved through the label exchange across the global frame, with the
/// sender, the setter of the global and the receivers in batches of their own.
#[test]
fn staged_matches_monolithic_intent() {
    let facts = intent_facts();
    let monolithic = taint_index_with_config(facts.clone(), IndexConfig::default(), None);
    assert_eq!(monolithic.intent_pair.len(), 2);
    let counts = assert_staged_matches(&facts, None, ONE_SCC_PER_BATCH, &[1, 2, 3], "intent");
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

/// Every flowy fixture, one SCC per batch, on facts in three orders.
#[test]
fn staged_matches_monolithic_flowy() {
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
    for file in files {
        let models: Vec<_> = ["models.jsonl", "models.ctadl"]
            .iter()
            .map(|ext| file.with_extension(ext))
            .filter(|p| p.exists())
            .collect();
        let (facts, source_info, _) = crate::codegen::flowy::program_facts(&file, &models)
            .unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
        assert_staged_matches(
            &facts,
            Some(&source_info.sites),
            ONE_SCC_PER_BATCH,
            &[1, 2, 3],
            &file.display().to_string(),
        );
    }
}

/// A batch reads its own facts and the `summary` files of the batches that hold its callees,
/// and nothing else.
#[test]
fn batch_reads_only_callees() {
    let (facts, source_info) = index_program(program_from_string(DIAMOND).0);
    let id_map = Some(&source_info.sites);
    let index = tempfile::tempdir().unwrap();
    let mut executor = InProcess {
        parallelism: Parallelism::Serial,
        id_map,
    };
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
        let read = run_batch(&stage, batch.id, Parallelism::Serial, id_map)
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
