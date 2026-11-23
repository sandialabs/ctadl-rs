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

/// Creates a data flow graph for taint analysis, with the engine that `config` selects.
pub fn taint_index_with_config(
    facts: IndexFacts,
    config: IndexConfig,
    id_map: Option<&IdMap>,
) -> IndexResult {
    let (storage, parallelism) = (config.storage, config.parallelism);
    let input = EngineInput::new(facts, config, id_map);
    let result = match (storage, parallelism) {
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
    };
    log::trace!("index result: {}", result.display(id_map));
    log::debug!(
        "flow variable size: {}",
        std::mem::size_of::<FlowVariable>()
    );
    result
}
