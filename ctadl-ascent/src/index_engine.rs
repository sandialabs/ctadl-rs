//! The index engine: everything in [`ctadl_index::index_engine`], plus [`taint_index`] and
//! [`taint_index_with_config`], which pick an engine and run it. The engines are crates of their
//! own that depend on `ctadl-index`, so these two live here, downstream of them.
//!
//! ```
//! use ctadl_ascent::index_engine::{IndexFacts, taint_index};
//! use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
//! use ctadl_ascent::facts::Function;
//! let mut facts = IndexFacts::default();
//! let mut source_info = IndexSourceInfo::default();
//! // ... generate facts
//! let name_id = source_info.sites.get_or_add_function(Function("name".into()));
//! // ... save the source_info
//! let result = taint_index(facts);
//! ```

pub use ctadl_index::index_engine::*;

use crate::facts::{FlowVariable, IdMap};

/// Creates a data flow graph for taint analysis.
pub fn taint_index(facts: IndexFacts) -> IndexResult {
    taint_index_with_config(facts, IndexConfig::default(), None)
}

/// Creates a data flow graph for taint analysis, with the engine that `config` selects, staged
/// when it says so.
pub fn taint_index_with_config(
    facts: IndexFacts,
    config: IndexConfig,
    id_map: Option<&IdMap>,
) -> IndexResult {
    let checked = check_staged_enabled().then(|| (facts.clone(), config.clone()));
    let result = run_configured(facts, config, id_map);
    if let Some((facts, config)) = checked {
        check_staged_with(facts, &config, id_map, &result, |_| {});
    }
    result
}

fn run_configured(facts: IndexFacts, config: IndexConfig, id_map: Option<&IdMap>) -> IndexResult {
    if let Some(staged) = config.staged {
        return crate::staged::run::run_in_process(facts, config, staged, id_map);
    }
    let result = run_engine(EngineInput::new(facts, config, id_map));
    log::trace!("index result: {}", result.display(id_map));
    log::debug!(
        "flow variable size: {}",
        std::mem::size_of::<FlowVariable>()
    );
    result
}

/// Set to `1`, it makes [`taint_index_with_config`] check every index it computes against the
/// staged runs of the same facts (see [`check_staged_with`]). It is read once per process.
pub const CHECK_STAGED_VAR: &str = "CTADL_INDEX_CHECK_STAGED";

fn check_staged_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(CHECK_STAGED_VAR).is_ok_and(|v| v == "1"))
}

/// The staged configurations the check runs: one SCC per batch, whole, and every recursive SCC
/// split down to single functions.
const CHECKED_STAGINGS: [StagedConfig; 2] = [
    StagedConfig {
        budget: 0,
        floor: usize::MAX,
    },
    StagedConfig {
        budget: 0,
        floor: 1,
    },
];

/// The seed of the shuffle of the facts each checked staged run indexes.
const CHECK_SEED: u64 = 1;

/// Checks `result`, the index of `facts` under `config`, against the monolithic serial run:
/// `result` itself unless it is that run, and the staged runs of [`CHECKED_STAGINGS`] on the
/// facts shuffled. `edit` changes each staged run's tables before they are compared, so that a
/// test can see a difference caught.
///
/// Panics on the first run whose tables differ, with the rows that differ in each table.
pub(crate) fn check_staged_with(
    facts: IndexFacts,
    config: &IndexConfig,
    id_map: Option<&IdMap>,
    result: &IndexResult,
    edit: impl Fn(&mut IndexResult),
) {
    let serial = IndexConfig {
        parallelism: Parallelism::Serial,
        storage: Storage::Memory,
        staged: None,
        ..config.clone()
    };
    let monolithic;
    let reference = if *config == serial {
        result
    } else {
        monolithic = run_configured(facts.clone(), serial.clone(), id_map);
        assert_same_tables(&monolithic, result, id_map, &format!("{config:?}"));
        &monolithic
    };
    let mut shuffled = facts;
    ctadl_index::staged::shuffle::shuffle_facts(&mut shuffled, CHECK_SEED);
    for staged in CHECKED_STAGINGS {
        let config = IndexConfig {
            staged: Some(staged),
            ..serial.clone()
        };
        let mut run = run_configured(shuffled.clone(), config, id_map);
        edit(&mut run);
        assert_same_tables(
            reference,
            &run,
            id_map,
            &format!("staged {staged:?}, facts shuffled with seed {CHECK_SEED}"),
        );
    }
}

fn assert_same_tables(
    reference: &IndexResult,
    run: &IndexResult,
    id_map: Option<&IdMap>,
    what: &str,
) {
    let diff = crate::staged::compare::compare_results(reference, run, id_map);
    assert!(
        diff.is_empty(),
        "{CHECK_STAGED_VAR}: the index of {what} differs from the monolithic serial run's \
         (< monolithic, > {what}):\n{diff}"
    );
}

/// Runs `input` to fixpoint on the engine its config selects.
pub fn run_engine(input: EngineInput<'_>) -> IndexResult {
    let (storage, parallelism) = (input.config.storage, input.config.parallelism);
    match (storage, parallelism) {
        (Storage::Lsm, parallelism) => {
            if parallelism != Parallelism::Serial {
                log::warn!("the LSM storage runs the serial engine only; ignoring {parallelism}");
            }
            let result = ctadl_index_lsm::run(input);
            let mib = |bytes: usize| bytes as f64 / f64::from(1 << 20);
            let budget = lsm::memory_budget().map_or_else(
                || "no budget".to_string(),
                |b| format!("budget {:.1} MiB", mib(b)),
            );
            log::info!(
                "LSM write buffers peaked at {:.1} MiB ({budget})",
                mib(lsm::memory_peak())
            );
            result
        }
        (Storage::Memory, Parallelism::Serial) => ctadl_index_serial::run(input),
        (Storage::Memory, Parallelism::Threads(threads)) => {
            // A dedicated pool rather than rayon's global one, so `-j N` means exactly N index
            // threads no matter what else in the process has touched rayon. The parallel program
            // runs its rules with `rayon::scope` / `par_iter`, which pick up the pool they are
            // `install`ed in.
            let pool = ascent::rayon::ThreadPoolBuilder::new()
                .num_threads(threads.get())
                .thread_name(|i| format!("ctadl-index-{i}"))
                .build()
                .expect("spawning the index thread pool");
            pool.install(move || ctadl_index_par::run(input))
        }
    }
}
