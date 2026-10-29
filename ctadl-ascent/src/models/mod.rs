/*! Model support

Loads model files and matches them against one program, appending what matched into a
[`ProgramModelMatches`]. That structure is the only thing a load produces; the loaders return
a [`ModelLoadReport`] alongside it for the Stage-1 counters the diagnostics need.
*/

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, Read};

use crate::error::{Error, ErrorContext, JsonModelError};
use crate::facts::TaintDirection;
use ctadl_ir::mir::call::VirtualMethodTable;

pub mod codegen;
pub mod dsl;
pub mod json;
pub mod json_schema;
pub mod match_index;
pub mod matches;
pub mod spec;
pub mod universe_set;

pub use dsl::{DslFile, DslMatcher, DslModelSet, DslReport, Phase as DslPhase, is_dsl_path};
pub use json::{
    EndpointStats, IndexTimeModelCounts, MatchedFunctions, PropagationStats, UnmatchedReason,
};
pub use match_index::{DispatchKeys, ProgramMatchIndex};
pub use matches::{
    BridgeMatches, DispatchKey, DispatchModel, Disposition, EndpointMatch, ModelPort,
    ProgramModelMatches, PropagationMatch,
};
pub use spec::{
    BridgeSpec, Direction, ImportScope, ModelFileSpecs, PortPair, ProgramScope, ResolvedBridge,
    Severity, SideSpec, scan_model_files,
};

#[cfg(test)]
mod tests;

/// The built-in default model file for each [`VirtualMethodTable`] variant, as
/// `(name, contents)`. `name` appears in error context and is what [`DEFAULT_MODEL_FILES`]
/// enumerates for the drift test.
///
/// These are the **JSON** originals. They are no longer what an index loads — see
/// [`DEFAULT_DSL_MODELS`] — but they remain the migrator's input and the equivalence oracle the
/// `.ctadl` files are checked against, which is what keeps the pair from drifting.
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

/// Every shipped default file, whether or not a given import loads it. Exists so a test can
/// parse all of them against one program: the loader hard-errors on unknown keys and on
/// malformed access paths, and a stale default file would otherwise break *every* index of the
/// language that selects it.
pub const DEFAULT_MODEL_FILES: &[(&str, &[u8])] = &[
    JAVA_DEFAULT_MODELS,
    NATIVE_DEFAULT_MODELS,
    LUA_DEFAULT_MODELS,
    CIL_DEFAULT_MODELS,
];

/// The DSL form of each shipped default: what an index actually loads.
///
/// Written by `ctadl migrate-models` from the `.jsonl` beside each one and checked in, so a
/// reader can see what the defaults say without running anything. The pair is pinned by
/// `tests/default_models.rs`, which loads both against the same program and requires identical
/// matches — edit one without the other and that fails.
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
    let Some(set) = default_dsl_models(index.vmt())? else {
        return Ok(ModelLoadReport::default());
    };
    let facts = dsl::program_facts(index, &set.needs());
    load_default_models_from_facts(set, &facts, out)
}

/// [`try_load_default_models`] over relations already built, for a caller that shares one
/// [`dsl::relations::ProgramFacts`] between the defaults and the user's DSL files. `set` is
/// what [`default_dsl_models`] returned for the program `facts` were built from, and the facts
/// must cover its [`dsl::DslModelSet::needs`].
pub fn load_default_models_from_facts(
    set: &dsl::DslModelSet,
    facts: &dsl::relations::ProgramFacts,
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
    let mut matcher = dsl::DslMatcher::new(set, dsl::Phase::All);
    matcher.observe_facts(facts, out);
    matcher
        .finish(out)
        .map(ModelLoadReport::from_dsl)
        .err_context(|| format!("loading default index models: {name}"))
}

/// The shipped DSL defaults, parsed and checked once per process rather than once per import.
/// Parallel to [`DEFAULT_DSL_MODELS`]. A file that does not parse keeps its rendered error.
static DEFAULT_DSL_SETS: std::sync::LazyLock<Vec<Result<dsl::DslModelSet, String>>> =
    std::sync::LazyLock::new(|| {
        DEFAULT_DSL_MODELS
            .iter()
            .map(|&(name, source)| {
                dsl::DslFile::from_text(name, source)
                    .map(|file| dsl::DslModelSet { files: vec![file] })
                    .map_err(|e| e.to_string())
            })
            .collect()
    });

/// The parsed DSL default a program with this [`VirtualMethodTable`] selects; see
/// [`default_dsl_model_file`]. An error only if a shipped file does not parse, which is a
/// build-time bug.
pub fn default_dsl_models(
    vmt: &VirtualMethodTable,
) -> Result<Option<&'static dsl::DslModelSet>, Error> {
    let Some(index) = default_dsl_index(vmt) else {
        return Ok(None);
    };
    match &DEFAULT_DSL_SETS[index] {
        Ok(set) => Ok(Some(set)),
        Err(message) => Err(Error::DslModel(message.clone())).err_context(|| {
            format!(
                "loading default index models: {}",
                DEFAULT_DSL_MODELS[index].0
            )
        }),
    }
}

/// The built-in default model file a program with this [`VirtualMethodTable`] selects, as
/// `(name, contents)`. The JSON original; see [`default_dsl_model_file`] for the one an index
/// loads.
pub fn default_model_file(vmt: &VirtualMethodTable) -> Option<(&'static str, &'static [u8])> {
    match vmt {
        VirtualMethodTable::Java { .. } => Some(JAVA_DEFAULT_MODELS),
        VirtualMethodTable::Native { .. } => Some(NATIVE_DEFAULT_MODELS),
        VirtualMethodTable::Lua { .. } => Some(LUA_DEFAULT_MODELS),
        VirtualMethodTable::Cil { .. } => Some(CIL_DEFAULT_MODELS),
        // flowy, and anything else with no method table: a default model file has nothing to
        // match against, and shipping one would be a language guess.
        VirtualMethodTable::Unknown => None,
    }
}

/// Whether the built-in defaults for a program with this [`VirtualMethodTable`] ask for the
/// call-site signature keys.
///
/// The other half of the gate [`ModelFileSpecs::finds_dispatch`](spec::ModelFileSpecs) opens:
/// together they decide whether an import pays for collecting its call sites' signature keys.
///
/// Read off the **DSL** defaults, because those are what [`try_load_default_models`] runs. The
/// `.jsonl` original says the same thing -- the pair is pinned by `tests/default_models.rs` --
/// but answering from the file that is not loaded would be one more way for the two to drift.
pub fn default_models_find_dispatch(vmt: &VirtualMethodTable) -> bool {
    // A shipped default that does not parse is a build-time bug, and every other caller of this
    // file reports it with a span; here there is no diagnostic channel and nothing to gain from
    // guessing, so an unparsable file simply asks for nothing.
    default_dsl_models(vmt)
        .ok()
        .flatten()
        .is_some_and(|set| set.uses_callsig())
}

/// The DSL default a program with this [`VirtualMethodTable`] selects. Parallel to
/// [`default_model_file`], and the one [`try_load_default_models`] runs.
pub fn default_dsl_model_file(vmt: &VirtualMethodTable) -> Option<(&'static str, &'static str)> {
    DEFAULT_DSL_MODELS.get(default_dsl_index(vmt)?).copied()
}

/// Which entry of [`DEFAULT_DSL_MODELS`] a program with this [`VirtualMethodTable`] selects.
fn default_dsl_index(vmt: &VirtualMethodTable) -> Option<usize> {
    match vmt {
        VirtualMethodTable::Java { .. } => Some(0),
        VirtualMethodTable::Native { .. } => Some(1),
        VirtualMethodTable::Lua { .. } => Some(2),
        VirtualMethodTable::Cil { .. } => Some(3),
        VirtualMethodTable::Unknown => None,
    }
}

/// Load models from a `jsonl` source. `jsonl` allows streaming models one at a time efficiently.
/// The stream follows the same schema as elements of a `model_generators` array.
///
/// Blank lines and lines whose first non-space characters are `//` are skipped, so a model file
/// can carry the commentary that explains why an entry is (or is not) there. JSON itself has no
/// comment syntax and `jsonl` has no envelope object to hang one off, so the alternative is a
/// separate document that drifts. Skipped lines do not consume a generator index: the index
/// names the *generator*, and it is what `CTADL0004` and the JSON error messages report.
pub fn try_load_jsonl_models<B: BufRead>(
    index: &ProgramMatchIndex<'_>,
    rdr: B,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    try_load_jsonl_models_named(index, rdr, out, "<models>")
}

/// [`try_load_jsonl_models`] for a caller that knows what to call the stream.
pub fn try_load_jsonl_models_named<B: BufRead>(
    index: &ProgramMatchIndex<'_>,
    rdr: B,
    out: &mut ProgramModelMatches,
    source: &str,
) -> Result<ModelLoadReport, Error> {
    try_load_models_from_values_named(index, jsonl_items(rdr), out, source)
}

/// Load models from a JSON file containing `{ "model_generators": [...] }`.
pub fn try_load_json_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let path = path.as_ref();
    let root = read_json_root(path)?;
    let items = model_generators(&root)?.iter().cloned().map(Ok);
    try_load_models_from_values_named(index, items, out, &path.display().to_string())
}

/// Load models from a JSON5 file containing `{ "model_generators": [...] }`.
pub fn try_load_json5_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let path = path.as_ref();
    let root = read_json5_root(path)?;
    let items = model_generators(&root)?.iter().cloned().map(Ok);
    try_load_models_from_values_named(index, items, out, &path.display().to_string())
}

/// Load models from a file, through the DSL engine. A `.ctadl` / `.dl` file is loaded as
/// written; any other is a JSON / JSON5 / JSONL file and is migrated first (see
/// [`dsl::DslFile::read`]).
///
/// Both phases' heads are kept: a caller with one program in hand and no phase to declare
/// wants everything the file says, and `cli::index` / `cli::query` drive [`DslMatcher`]
/// themselves precisely so they can say which half they are keeping.
///
/// Everything the file matched is *appended* to `out`, which is what lets a caller accumulate
/// across every (import x model file) pair without a merge step.
pub fn try_load_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let report = dsl::try_load_dsl_models(index, path, dsl::Phase::All, out)?;
    Ok(ModelLoadReport::from_dsl(report))
}

/// Load models from a stream of json Values. This processing is batched for efficiency, so the
/// iterator can be large and lazy.
///
/// # Errors leave `out` partially written
///
/// On `Err`, `out` holds every row appended before the error: JSON model errors are *collected*
/// across a batch and returned only at the end (see
/// [`json::ModelGeneratorIngest::encode_models_from`]), long after rows have been emitted.
/// Every production caller propagates with `?` and aborts, so nothing observable depends on it;
/// `json_error_handling.rs` pins that rows emitted before a collected error stay readable.
pub fn try_load_models_from_values(
    index: &ProgramMatchIndex<'_>,
    items: impl Iterator<Item = Result<serde_json::Value, Error>>,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    try_load_models_from_values_named(index, items, out, "<models>")
}

/// [`try_load_models_from_values`] for a caller that knows what to call the file. The name is
/// half of the `file:generator-index` provenance a matched dispatch model records.
pub fn try_load_models_from_values_named(
    index: &ProgramMatchIndex<'_>,
    items: impl Iterator<Item = Result<serde_json::Value, Error>>,
    out: &mut ProgramModelMatches,
    source: &str,
) -> Result<ModelLoadReport, Error> {
    let outcome = run_batches(index, items, out, None, true, source);
    // A stream error ends the input; it is returned as it came, without the "encoding models"
    // context, which names the wrong stage for a file that could not be read.
    if let Some(error) = outcome.stream_error {
        return Err(error);
    }
    if !outcome.errors.is_empty() {
        let mut json_errors = crate::error::JsonModelErrors::default();
        json_errors.extend(outcome.errors);
        return Err(Error::JsonModel(json_errors)).err_context(|| "encoding models".to_string());
    }
    log::trace!("matched {} summary models", out.propagations.len());
    log::trace!("matched {} source/sink models", out.endpoints.len());
    Ok(outcome.report)
}

/// What one lenient load could not do, without that ending the load.
///
/// Two categories, because they mean different things to a reader: a [`Self::Generator`] error
/// names one generator that will not work and leaves the rest of the file's report intact,
/// while a [`Self::Stream`] error means the file (or the rest of it) was never read, so the
/// report *is* incomplete and the missing generators are missing rather than dead.
#[derive(Debug)]
pub enum ModelCheckError {
    /// A shape error in one generator: an unknown key, a malformed port, a malformed scope.
    Generator(JsonModelError),
    /// The file, or the rest of it, could not be read or parsed.
    Stream(Error),
}

impl std::fmt::Display for ModelCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelCheckError::Generator(e) => write!(f, "{e}"),
            ModelCheckError::Stream(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ModelCheckError {
    /// Delegates rather than terminating the chain: an [`Error::Context`] displays as its
    /// context alone, so a caller rendering only the top of the chain would print "opening
    /// model JSON file: x.json" and drop the half that says *why* it could not be opened.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ModelCheckError::Generator(e) => Some(e),
            ModelCheckError::Stream(e) => Some(e),
        }
    }
}

/// [`try_load_models`] for a linter: matches the whole file whatever it finds, and hands back
/// the errors instead of failing on them.
///
/// `capture` is how many of each generator's matched function names to retain; the counts are
/// exact either way.
pub fn try_check_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    capture: usize,
    out: &mut ProgramModelMatches,
) -> (ModelLoadReport, Vec<ModelCheckError>) {
    let path = path.as_ref();
    // A DSL file is checked by loading it: parse and mode errors are load-time, so there is no
    // second, laxer path for a linter to take. One malformed rule still costs the file, which
    // is the one place the DSL is stricter than the JSON loader — a syntax error has no
    // resynchronization point. A JSON file keeps the JSON loader's leniency: a generator that
    // does not migrate is reported and the rest are checked.
    let loaded = if dsl::is_dsl_path(path) {
        dsl::DslFile::read(path).map(|file| (file, None))
    } else {
        dsl::DslFile::read_json_lenient(path)
    };
    let (file, rejected) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return (ModelLoadReport::default(), vec![ModelCheckError::Stream(e)]),
    };
    let mut errors: Vec<ModelCheckError> =
        rejected.into_iter().map(ModelCheckError::Stream).collect();
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set, dsl::Phase::All);
    matcher.observe_import(index, out);
    match matcher.finish(out) {
        Ok(report) => {
            let mut load = ModelLoadReport::from_dsl(report);
            for matched in load.matched.values_mut() {
                if let MatchedFunctions::Some { names, .. } = matched {
                    while names.len() > capture {
                        names.pop_last();
                    }
                }
            }
            (load, errors)
        }
        Err(e) => {
            errors.push(ModelCheckError::Stream(e));
            (ModelLoadReport::default(), errors)
        }
    }
}

/// [`try_check_models`] for a `jsonl` stream. An unparsable line ends the file -- there is
/// nothing left to read -- but the generators before it keep their stats.
pub fn try_check_jsonl_models<B: BufRead>(
    index: &ProgramMatchIndex<'_>,
    rdr: B,
    capture: usize,
    out: &mut ProgramModelMatches,
) -> (ModelLoadReport, Vec<ModelCheckError>) {
    let outcome = run_batches(
        index,
        jsonl_items(rdr),
        out,
        Some(capture),
        false,
        "<models>",
    );
    outcome.into_check()
}

/// The `model_generators` array of a JSON or JSON5 model file's root object.
fn model_generators(root: &serde_json::Value) -> Result<&Vec<serde_json::Value>, Error> {
    root.get("model_generators")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid 'model_generators' array",
            ))
        })
}

fn read_json_root(path: &std::path::Path) -> Result<serde_json::Value, Error> {
    let file =
        File::open(path).err_context(|| format!("opening model JSON file: {}", path.display()))?;
    serde_json::from_reader(file)
        .err_context(|| format!("reading model JSON file: {}", path.display()))
}

fn read_json5_root(path: &std::path::Path) -> Result<serde_json::Value, Error> {
    let mut file =
        File::open(path).err_context(|| format!("opening model JSON5 file: {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .err_context(|| format!("reading model JSON5 file: {}", path.display()))?;
    json5::from_str(&content)
        .err_context(|| format!("parsing model JSON5 file: {}", path.display()))
}

/// The generator values of a `jsonl` stream, skipping blanks and `//` comments. See
/// [`try_load_jsonl_models`] for why a skipped line does not consume a generator index.
fn jsonl_items<B: BufRead>(rdr: B) -> impl Iterator<Item = Result<serde_json::Value, Error>> {
    rdr.lines()
        .map(|line| -> Result<Option<serde_json::Value>, Error> {
            let line = line?;
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                return Ok(None);
            }
            let value = serde_json::from_str(trimmed).err_context(|| "reading model line")?;
            Ok(Some(value))
        })
        .filter_map(Result::transpose)
}

/// What [`run_batches`] found, before either entry point decides what an error means.
struct BatchOutcome {
    report: ModelLoadReport,
    /// Shape errors collected while visiting, in visit order.
    errors: Vec<JsonModelError>,
    /// The error that ended the input early, if any.
    stream_error: Option<Error>,
}

impl BatchOutcome {
    /// The lenient entry points' return shape: the report, plus both error categories in one
    /// list, shape errors first -- they name generators the stream error's truncation has
    /// nothing to do with.
    fn into_check(self) -> (ModelLoadReport, Vec<ModelCheckError>) {
        let errors = self
            .errors
            .into_iter()
            .map(ModelCheckError::Generator)
            .chain(self.stream_error.map(ModelCheckError::Stream))
            .collect();
        (self.report, errors)
    }
}

/// The one batch loop under both entry points.
///
/// `capture` turns on the per-generator recording (see
/// [`json::ModelGeneratorIngest::capture_matches`]); `abort_on_error` stops after the first
/// batch that collected one, which is what preserves `try_load_models`'s contract that a
/// collected error ends the load.
fn run_batches(
    index: &ProgramMatchIndex<'_>,
    mut items: impl Iterator<Item = Result<serde_json::Value, Error>>,
    out: &mut ProgramModelMatches,
    capture: Option<usize>,
    abort_on_error: bool,
    source: &str,
) -> BatchOutcome {
    let mut model_gen = json::ModelGeneratorIngest::new(index, out);
    model_gen.set_source(source);
    if let Some(cap) = capture {
        model_gen.capture_matches(cap);
    }
    let batch_size = 1024;
    let mut batch: Vec<serde_json::Value> = Vec::with_capacity(batch_size);
    let mut errors: Vec<JsonModelError> = Vec::new();
    let mut stream_error: Option<Error> = None;

    // Index of the first generator of the current batch within the model file. Generator
    // indices must count across batches: they name the generator in JSON error messages and
    // in the `CTADL0004` SARIF notification, and they key `endpoint_stats`.
    let mut base = 0usize;

    'outer: loop {
        // Fill the batch. `take` is what bounds it: pulling from `items` directly and
        // breaking on a full batch would consume an item and then drop it on the floor,
        // silently losing every 1025th generator in the file.
        for item in items.by_ref().take(batch_size) {
            match item {
                Ok(value) => batch.push(value),
                Err(e) => {
                    stream_error = Some(e);
                    // Aborting drops the partial batch. A lenient load visits it first: those
                    // generators were read successfully, and the linter's job is to report on
                    // every one it could read.
                    if abort_on_error {
                        break 'outer;
                    }
                    break;
                }
            }
        }
        if batch.is_empty() {
            break;
        }
        let count = batch.len();
        model_gen.visit_models_from(base, batch.drain(..));
        errors.extend(model_gen.drain_errors());
        batch.clear();
        base += count;
        if stream_error.is_some() || (abort_on_error && !errors.is_empty()) {
            break;
        }
    }
    let report = ModelLoadReport {
        endpoint_stats: std::mem::take(&mut model_gen.endpoint_stats),
        index_time_models: model_gen.index_time_models,
        matched: std::mem::take(&mut model_gen.matched),
        in_function_matched: std::mem::take(&mut model_gen.in_function_matched),
        propagation_stats: std::mem::take(&mut model_gen.propagation_stats),
        access_path_stats: std::mem::take(&mut model_gen.access_path_stats),
        dsl: None,
    };
    // `model_gen` holds the `&mut out` borrow, so dropping it is what lets the counters be read
    // back alongside the rows it appended.
    drop(model_gen);
    BatchOutcome {
        report,
        errors,
        stream_error,
    }
}

/// What one model-file load recorded *besides* the matches themselves.
///
/// The matches go into the caller's [`ProgramModelMatches`]; these are the Stage-1 counters
/// the diagnostics need, which belong to the load rather than to the accumulated match set.
#[derive(Debug, Default, Clone)]
pub struct ModelLoadReport {
    /// What Stage 1 did per (generator index, direction) -- see
    /// [`json::ModelGeneratorIngest::endpoint_stats`]. Key presence means the generator
    /// declared a port of that direction; a zero `endpoints_matched` means it matched
    /// nothing, which `cli::query` turns into a `CTADL0004` SARIF notification.
    ///
    /// Deliberately *not* keyed by model file: some loader entry points take a reader or a
    /// value stream, which have no path, so file identity belongs to the caller.
    /// `cli::query` re-keys each file's stats by file before merging them, which is also what
    /// keeps two files that number their generators the same from conflating.
    pub endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats>,
    /// How many generators declared an index-time-only construct. Lets `ctadl query` say once
    /// that it is ignoring them instead of dropping them in silence.
    pub index_time_models: IndexTimeModelCounts,
    /// Per generator, what its `where` selected. Empty unless the load asked for the capture
    /// (see [`try_check_models`]); `index` and `query` do not, and pay nothing for it.
    pub matched: BTreeMap<usize, MatchedFunctions>,
    /// Per `find: callsites` generator, what its `in_function` selected. Same gating as
    /// [`Self::matched`].
    pub in_function_matched: BTreeMap<usize, MatchedFunctions>,
    /// Per generator, what its `propagation` list declared and emitted. Same gating as
    /// [`Self::matched`].
    pub propagation_stats: BTreeMap<usize, PropagationStats>,
    /// Per generator, how many `model.access_paths` entries were registered. Same gating as
    /// [`Self::matched`].
    pub access_path_stats: BTreeMap<usize, usize>,
    /// Set when the file was loaded by the DSL engine, which every path-based load is, a JSON
    /// file through its migration. The counters above are filled in from it so every
    /// diagnostic surface keeps working — a rule's label plays the part of a generator index,
    /// and is one for a migrated file — and this carries what has no JSON counterpart.
    pub dsl: Option<dsl::DslReport>,
}

impl ModelLoadReport {
    /// Projects a DSL load onto the counters the diagnostics read.
    ///
    /// A rule index stands in for a generator index: both name "the *n*th thing in this file",
    /// which is what `CTADL0004` and the JSON error messages report. The `matched` /
    /// `propagation_stats` captures stay empty — those exist for the no-index model check,
    /// which reads [`Self::dsl`] instead.
    pub fn from_dsl(report: dsl::DslReport) -> Self {
        let mut endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats> = BTreeMap::new();
        let mut matched: BTreeMap<usize, MatchedFunctions> = BTreeMap::new();
        let mut propagation_stats: BTreeMap<usize, PropagationStats> = BTreeMap::new();
        let mut access_path_stats: BTreeMap<usize, usize> = BTreeMap::new();
        for file in &report.files {
            for (key, stats) in dsl_endpoint_stats(file) {
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
                // A rule's match set is the functions its heads anchored at. There is no `All`
                // case: a rule with an unconstrained `fun(F)` enumerates rather than staying
                // symbolic, so the count is exact and only the *names* are capped. Over two
                // rules of one generator the counts sum, so a function both select counts
                // twice.
                let rule_matched = MatchedFunctions::Some {
                    total: stats.functions,
                    names: stats.matched_functions.clone(),
                };
                match matched.get_mut(&i) {
                    Some(existing) => existing.merge(&rule_matched, dsl::emit::MATCHED_SAMPLE_CAP),
                    None => {
                        matched.insert(i, rule_matched);
                    }
                }
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
            dsl: Some(report),
            ..Default::default()
        }
    }
}

/// The source/sink counters one DSL file's rules project onto, keyed `(label, direction)`: one
/// entry per direction a rule declares a head in. `ctadl query` and [`ModelLoadReport::from_dsl`]
/// both read a DSL load's endpoint stats through this. Two rules with one label — the
/// disjuncts of one JSON generator — yield two entries under one key, for the caller to
/// [`EndpointStats::merge`].
pub fn dsl_endpoint_stats(
    file: &dsl::DslFileReport,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
