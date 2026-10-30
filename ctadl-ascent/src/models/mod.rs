/*! Model support

Loads model files and matches them against one program, appending what matched into a
[`ProgramModelMatches`]. That structure is the only thing a load produces; the loaders return
a [`ModelLoadReport`] alongside it for the Stage-1 counters the diagnostics need.
*/

use std::collections::BTreeMap;

use crate::error::{Error, ErrorContext};
use crate::facts::TaintDirection;
use ctadl_ir::mir::call::{Dispatch, VirtualMethodTable};

pub mod codegen;
pub mod json_schema;
pub mod match_index;
pub mod matches;
pub mod portcullis;
pub mod spec;
pub mod stats;
#[doc(hidden)]
pub mod test_vmt;

pub use match_index::{DispatchKeys, ProgramMatchIndex};
pub use matches::{
    DispatchKey, DispatchModel, Disposition, EndpointMatch, ModelPort, ProgramModelMatches,
    PropagationMatch,
};
pub use portcullis::{
    Phase as PortcullisPhase, PortcullisFile, PortcullisMatcher, PortcullisModelSet,
    PortcullisReport, is_portcullis_path,
};
pub use spec::{
    BridgeSpec, Direction, ImportScope, PortPair, ProgramScope, ResolvedBridge, Severity, SideSpec,
};
pub use stats::{
    EndpointStats, IndexTimeModelCounts, MatchedFunctions, PropagationStats, UnmatchedReason,
};

#[cfg(test)]
mod tests;

/// The built-in default model file for each [`VirtualMethodTable`] variant, as
/// `(name, contents)`. `name` appears in error context and is what [`DEFAULT_MODEL_FILES`]
/// enumerates for the drift test.
///
/// These are the **JSON** originals. They are not what an index loads — see
/// [`DEFAULT_DSL_MODELS`] — but they are the migrator's input: each `.ctadl` default is exactly
/// what `ctadl migrate-models` writes for its `.jsonl`, which `tests/default_models.rs` checks
/// byte for byte.
pub const JAVA_DEFAULT_MODELS: (&str, &[u8]) = (
    "java-index.jsonl",
    include_bytes!("defaults/java-index.jsonl"),
);
pub const NATIVE_DEFAULT_MODELS: (&str, &[u8]) = (
    "native-index.jsonl",
    include_bytes!("defaults/native-index.jsonl"),
);
pub const LUA_DEFAULT_MODELS: (&str, &[u8]) = (
    "lua-index.jsonl",
    include_bytes!("defaults/lua-index.jsonl"),
);
pub const CIL_DEFAULT_MODELS: (&str, &[u8]) = (
    "cil-index.jsonl",
    include_bytes!("defaults/cil-index.jsonl"),
);

/// Every shipped `.jsonl` default, whether or not a given import loads it, parallel to
/// [`DEFAULT_DSL_MODELS`]. Exists so the tests can check each against the `.ctadl` migrated from
/// it, and check that each still migrates and loads.
pub const DEFAULT_MODEL_FILES: &[(&str, &[u8])] = &[
    JAVA_DEFAULT_MODELS,
    NATIVE_DEFAULT_MODELS,
    LUA_DEFAULT_MODELS,
    CIL_DEFAULT_MODELS,
];

/// The languages each of [`DEFAULT_MODEL_FILES`] is loaded for, by
/// [`ArtifactLanguage`](crate::project::ArtifactLanguage) name: the imports whose
/// [`VirtualMethodTable`] selects it. Its `.ctadl` is migrated for these
/// (`ctadl migrate-models --language`), so it leaves out what only another language could match.
pub const DEFAULT_MODEL_LANGUAGES: &[&[&str]] = &[
    &["dex", "jvm", "jar", "apk", "xapk"],
    &["pcode"],
    &["lua"],
    &["cil"],
];

/// The DSL form of each shipped default: what an index actually loads.
///
/// Written by `ctadl migrate-models` from the `.jsonl` beside each one and checked in, so a
/// reader can see what the defaults say without running anything. The pair is pinned by
/// `tests/default_models.rs`, which requires each `.ctadl` to be the migration of its `.jsonl`
/// byte for byte — edit one without the other and that fails.
pub const DEFAULT_DSL_MODELS: &[(&str, &str)] = &[
    (
        "java-index.ctadl",
        include_str!("defaults/java-index.ctadl"),
    ),
    (
        "native-index.ctadl",
        include_str!("defaults/native-index.ctadl"),
    ),
    ("lua-index.ctadl", include_str!("defaults/lua-index.ctadl")),
    ("cil-index.ctadl", include_str!("defaults/cil-index.ctadl")),
];

/// Returns the built-in default models for the program `index` was built from, selected by its
/// [`VirtualMethodTable`] variant.
///
/// The VMT is the key rather than [`crate::project::ArtifactLanguage`] because dex and jvm want
/// the same file and differ only in language, and because it gives `Unknown` (flowy) the right
/// answer -- nothing -- for free. An import never runs a match pass over another language's
/// generators.
///
/// This is deliberately *not* how a user's `in` scope is resolved. The VMT is the right key for
/// "which shipped file"; `in` is the right key for "which import did the user mean", and it can
/// tell `dex` from `apk` from `jar`, which the VMT variant cannot.
// TODO load summary parquet models as well as json. Such a decoder would build
// `PropagationMatch`es directly.
pub fn try_load_default_models(
    index: &ProgramMatchIndex<'_>,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    log::trace!("load_models");
    let Some(set) = default_portcullis_models(index.vmt())? else {
        return Ok(ModelLoadReport::default());
    };
    let facts = portcullis::program_facts(index, &set.needs());
    load_default_models_from_facts(set, &facts, out)
}

/// [`try_load_default_models`] over relations already built, for a caller that shares one
/// [`portcullis::relations::ProgramFacts`] between the defaults and the user's DSL files. `set` is
/// what [`default_portcullis_models`] returned for the program `facts` were built from, and the
/// facts must cover its [`portcullis::PortcullisModelSet::needs`].
pub fn load_default_models_from_facts(
    set: &portcullis::PortcullisModelSet,
    facts: &portcullis::relations::ProgramFacts,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let name = set
        .files
        .first()
        .map(|f| f.path.display().to_string())
        .unwrap_or_default();
    log::debug!("loading default models from {name}");
    // Both phases: the defaults are propagation-only, so `All` and `Index` name the same rules,
    // and a caller that hands them to a query gets what the file says rather than a phase
    // decision made here.
    let mut matcher = portcullis::PortcullisMatcher::new(set, portcullis::Phase::All);
    matcher.observe_facts(facts, out);
    matcher
        .finish(out)
        .map(ModelLoadReport::from_portcullis)
        .err_context(|| format!("loading default index models: {name}"))
}

/// The shipped DSL defaults, parsed and checked once per process rather than once per import.
/// Parallel to [`DEFAULT_DSL_MODELS`]. A file that does not parse keeps its rendered error.
static DEFAULT_DSL_SETS: std::sync::LazyLock<Vec<Result<portcullis::PortcullisModelSet, String>>> =
    std::sync::LazyLock::new(|| {
        DEFAULT_DSL_MODELS
            .iter()
            .map(|&(name, source)| {
                portcullis::PortcullisFile::from_text(name, source)
                    .map(|file| portcullis::PortcullisModelSet { files: vec![file] })
                    .map_err(|e| e.to_string())
            })
            .collect()
    });

/// The parsed DSL default a program with this [`VirtualMethodTable`] selects; see
/// [`default_portcullis_model_file`]. An error only if a shipped file does not parse, which is a
/// build-time bug.
pub fn default_portcullis_models(
    vmt: &VirtualMethodTable,
) -> Result<Option<&'static portcullis::PortcullisModelSet>, Error> {
    let Some(index) = default_portcullis_index(vmt) else {
        return Ok(None);
    };
    match &DEFAULT_DSL_SETS[index] {
        Ok(set) => Ok(Some(set)),
        Err(message) => Err(Error::PortcullisModel(message.clone())).err_context(|| {
            format!(
                "loading default index models: {}",
                DEFAULT_DSL_MODELS[index].0
            )
        }),
    }
}

/// Whether the built-in defaults for a program with this [`VirtualMethodTable`] ask for the
/// call-site signature keys.
///
/// The other half of the gate [`PortcullisModelSet::uses_callsig`] opens for the `--models` files:
/// together they decide whether an import pays for collecting its call sites' signature keys.
///
/// Read off the DSL defaults, because those are what [`try_load_default_models`] runs.
pub fn default_models_find_dispatch(vmt: &VirtualMethodTable) -> bool {
    // A shipped default that does not parse is a build-time bug, and every other caller of this
    // file reports it with a span; here there is no diagnostic channel and nothing to gain from
    // guessing, so an unparsable file simply asks for nothing.
    default_portcullis_models(vmt)
        .ok()
        .flatten()
        .is_some_and(|set| set.uses_callsig())
}

/// The DSL default a program with this [`VirtualMethodTable`] selects: the one
/// [`try_load_default_models`] runs.
pub fn default_portcullis_model_file(
    vmt: &VirtualMethodTable,
) -> Option<(&'static str, &'static str)> {
    DEFAULT_DSL_MODELS
        .get(default_portcullis_index(vmt)?)
        .copied()
}

/// Which entry of [`DEFAULT_DSL_MODELS`] a program with this [`VirtualMethodTable`] selects.
fn default_portcullis_index(vmt: &VirtualMethodTable) -> Option<usize> {
    match &vmt.dispatch {
        Dispatch::Java { .. } => Some(0),
        Dispatch::Native => Some(1),
        Dispatch::Lua { .. } => Some(2),
        Dispatch::Cil { .. } => Some(3),
        Dispatch::Unknown => None,
    }
}

/// Load models from a file, through the DSL engine. A `.ctadl` / `.dl` file is loaded as
/// written; any other is a JSON / JSON5 / JSONL file and is migrated first (see
/// [`portcullis::PortcullisFile::read`]).
///
/// Both phases' heads are kept: a caller with one program in hand and no phase to declare
/// wants everything the file says, and `cli::index` / `cli::query` drive [`PortcullisMatcher`]
/// themselves precisely so they can say which half they are keeping.
///
/// Everything the file matched is *appended* to `out`, which is what lets a caller accumulate
/// across every (import x model file) pair without a merge step.
pub fn try_load_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let report = portcullis::try_load_portcullis_models(index, path, portcullis::Phase::All, out)?;
    Ok(ModelLoadReport::from_portcullis(report))
}

/// What one model-file load recorded *besides* the matches themselves.
///
/// The matches go into the caller's [`ProgramModelMatches`]; these are the Stage-1 counters
/// the diagnostics need, which belong to the load rather than to the accumulated match set.
#[derive(Debug, Default, Clone)]
pub struct ModelLoadReport {
    /// What each (generator index, direction) matched -- see [`EndpointStats`]. Key presence
    /// means the generator declared a port of that direction; a zero `endpoints_matched` means
    /// it matched nothing, which `cli::query` turns into a `CTADL0004` SARIF notification.
    ///
    /// Deliberately *not* keyed by model file: file identity belongs to the caller.
    /// `cli::query` re-keys each file's stats by file before merging them, which is also what
    /// keeps two files that number their generators the same from conflating.
    pub endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats>,
    /// How many generators declared an index-time-only construct. Lets `ctadl query` say once
    /// that it is ignoring them instead of dropping them in silence.
    pub index_time_models: IndexTimeModelCounts,
    /// Per generator, what its `where` selected: the functions its rules' heads anchored at,
    /// with a capped sample of their names.
    pub matched: BTreeMap<usize, MatchedFunctions>,
    /// Per generator, what its `propagation` list declared and emitted.
    pub propagation_stats: BTreeMap<usize, PropagationStats>,
    /// Per generator, how many `model.access_paths` entries were registered.
    pub access_path_stats: BTreeMap<usize, usize>,
    /// The DSL engine's own report, which every load is (a JSON file through its migration).
    /// The counters above are projected from it so every diagnostic surface reads one shape — a
    /// rule's label plays the part of a generator index, and is one for a migrated file — and
    /// this carries what has no JSON counterpart.
    pub portcullis: Option<portcullis::PortcullisReport>,
}

impl ModelLoadReport {
    /// Projects a DSL load onto the counters the diagnostics read.
    ///
    /// A rule index stands in for a generator index: both name "the *n*th thing in this file",
    /// which is what `CTADL0004` and the JSON error messages report.
    pub fn from_portcullis(report: portcullis::PortcullisReport) -> Self {
        let mut endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats> = BTreeMap::new();
        let mut matched: BTreeMap<usize, MatchedFunctions> = BTreeMap::new();
        let mut propagation_stats: BTreeMap<usize, PropagationStats> = BTreeMap::new();
        let mut access_path_stats: BTreeMap<usize, usize> = BTreeMap::new();
        for file in &report.files {
            for (key, stats) in portcullis_endpoint_stats(file) {
                endpoint_stats.entry(key).or_default().merge(&stats);
            }
            // Keyed by label, so a JSON generator migrated to several rules — one per disjunct
            // of its `where` — reports once, as the union of its rules.
            for (&i, stats) in file.labels.iter().zip(&file.rules) {
                if stats.propagation_heads > 0 {
                    propagation_stats
                        .entry(i)
                        .or_default()
                        .merge(&PropagationStats {
                            ports_declared: stats.propagation_heads,
                            rows: stats.propagations,
                        });
                }
                if stats.access_paths > 0 {
                    *access_path_stats.entry(i).or_default() += stats.access_paths;
                }
                // A rule's match set is the functions its heads anchored at: a rule with an
                // unconstrained `fun(F)` enumerates, so the count is exact and only the *names*
                // are capped. Over two rules of one generator the counts sum, so a function both
                // select counts twice.
                let rule_matched = MatchedFunctions {
                    total: stats.functions,
                    names: stats.matched_functions.clone(),
                };
                matched
                    .entry(i)
                    .or_default()
                    .merge(&rule_matched, portcullis::emit::MATCHED_SAMPLE_CAP);
            }
        }
        let totals = report.totals();
        Self {
            endpoint_stats,
            index_time_models: IndexTimeModelCounts {
                propagations: report
                    .files
                    .iter()
                    .flat_map(|f| f.by_label())
                    .filter(|(_, rules)| rules.iter().any(|r| r.propagation_heads > 0))
                    .count(),
                bridges: totals.bridges,
                // No DSL rule can declare `modes`; see the migrator's unsupported-key list.
                modes: 0,
            },
            matched,
            propagation_stats,
            access_path_stats,
            portcullis: Some(report),
        }
    }
}

/// The source/sink counters one DSL file's rules project onto, keyed `(label, direction)`: one
/// entry per direction a rule declares a head in. `ctadl query` and
/// [`ModelLoadReport::from_portcullis`] both read a DSL load's endpoint stats through this. Two
/// rules with one label — the disjuncts of one JSON generator — yield two entries under one key,
/// for the caller to [`EndpointStats::merge`].
pub fn portcullis_endpoint_stats(
    file: &portcullis::PortcullisFileReport,
) -> impl Iterator<Item = ((usize, TaintDirection), EndpointStats)> + '_ {
    file.labels.iter().zip(&file.rules).flat_map(|(&i, stats)| {
        [
            (stats.source_heads, stats.sources, TaintDirection::Forward),
            (stats.sink_heads, stats.sinks, TaintDirection::Backward),
        ]
        .into_iter()
        .filter(|&(declared, _, _)| declared > 0)
        .map(move |(declared, rows, direction)| {
            let mut entry = EndpointStats {
                ports_declared: declared,
                endpoints_matched: rows,
                functions_matched: stats.functions,
                ..Default::default()
            };
            if rows == 0 {
                entry.unmatched.insert(UnmatchedReason::NoFunctionMatched);
            }
            ((i, direction), entry)
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormalIndexTypeTag {
    /// Index of a formal for the builder
    Index,
    /// Return value
    Return,
    /// Global value
    Global,
    /// Any parameter (excluding return and global)
    AnyArgument,
    /// A named local variable, selected by source name (`Variable(name)`). The resolved base
    /// `LocalIdx` is carried out-of-band in the endpoint's `local_index` column, not in `index`.
    Local,
}
