/*! Model support

Loads model files and matches them against one program, appending what matched into a
[`ProgramModelMatches`]. That structure is the only thing a load produces; the loaders return
a [`ModelLoadReport`] alongside it for the Stage-1 counters the diagnostics need.
*/

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};

use crate::error::{Error, ErrorContext};
use crate::facts::TaintDirection;
use ctadl_ir::mir::call::VirtualMethodTable;

pub mod codegen;
pub mod json;
pub mod match_index;
pub mod matches;
pub mod spec;
pub mod universe_set;

pub use json::{EndpointStats, IndexTimeModelCounts, UnmatchedReason};
pub use match_index::ProgramMatchIndex;
pub use matches::{
    BridgeMatches, EndpointMatch, ModelPort, ProgramModelMatches, PropagationMatch,
};
pub use spec::{
    BridgeSpec, Direction, ImportScope, ModelFileSpecs, PortPair, ProgramScope, Severity, SideSpec,
    scan_model_files,
};

#[cfg(test)]
mod tests;

/// The built-in default model file for each [`VirtualMethodTable`] variant, as
/// `(name, contents)`. `name` appears in error context and is what [`DEFAULT_MODEL_FILES`]
/// enumerates for the drift test.
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

/// Every shipped default file, whether or not a given import loads it. Exists so a test can
/// parse all of them against one program: the loader hard-errors on unknown keys and on
/// malformed access paths, and a stale default file would otherwise break *every* index of the
/// language that selects it.
pub const DEFAULT_MODEL_FILES: &[(&str, &[u8])] = &[
    JAVA_DEFAULT_MODELS,
    NATIVE_DEFAULT_MODELS,
    LUA_DEFAULT_MODELS,
];

/// Returns the built-in default models for the program `index` was built from, selected by its
/// [`VirtualMethodTable`] variant.
///
/// The VMT is the key rather than [`crate::project::ArtifactLanguage`] because dex and jvm want
/// the same file and differ only in language, and because it gives `Unknown` (flowy) the right
/// answer -- nothing -- for free. Loading every file for every import, as this used to, meant a
/// Lua import ran a full match pass over 55 Java generators and 14 C ones, contributing nothing.
///
/// This is deliberately *not* how a user's `in` scope is resolved. The VMT is the right key for
/// "which shipped file"; `in` is the right key for "which import did the user mean", and it can
/// tell `dex` from `apk` from `jar`, which the VMT variant cannot.
// TODO load summary parquet models as well as json. Such a decoder now builds
// `PropagationMatch`es directly rather than re-deriving them from an Arrow encoding.
pub fn try_load_default_models(
    index: &ProgramMatchIndex<'_>,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    log::trace!("load_models");
    let default = match index.vmt() {
        VirtualMethodTable::Java { .. } => Some(JAVA_DEFAULT_MODELS),
        VirtualMethodTable::Native { .. } => Some(NATIVE_DEFAULT_MODELS),
        VirtualMethodTable::Lua { .. } => Some(LUA_DEFAULT_MODELS),
        // flowy, and anything else with no method table: a default model file has nothing to
        // match against, and shipping one would be a language guess.
        VirtualMethodTable::Unknown => None,
    };
    let Some((name, contents)) = default else {
        return Ok(ModelLoadReport::default());
    };
    log::debug!("loading default models from {name}");
    try_load_jsonl_models(index, BufReader::new(contents), out)
        .err_context(|| format!("loading default index models: {name}"))
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
    let items = rdr
        .lines()
        .map(|line| -> Result<Option<serde_json::Value>, Error> {
            let line = line?;
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                return Ok(None);
            }
            let value = serde_json::from_str(trimmed).err_context(|| "reading model line")?;
            Ok(Some(value))
        })
        .filter_map(Result::transpose);
    try_load_models_from_values(index, items, out)
}

// Load models from a JSON file containing `{ "model_generators": [...] }`.
///
/// The entries in the `model_generators` array are streamed into
/// `load_models_from_values`, preserving the existing batch‑processing logic.
pub fn try_load_json_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    // Open and parse the JSON file
    let file = File::open(&path)
        .err_context(|| format!("opening model JSON file: {}", path.as_ref().display()))?;
    let root: serde_json::Value = serde_json::from_reader(file)
        .err_context(|| format!("reading model JSON file: {}", path.as_ref().display()))?;

    // Extract the `model_generators` array; error if missing or not an array
    let generators = match root.get("model_generators").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid 'model_generators' array",
            )));
        }
    };

    // Stream each entry into the existing loader
    let items = generators.iter().cloned().map(Ok);
    try_load_models_from_values(index, items, out)
}

/// Load models from a JSON5 file containing `{ "model_generators": [...] }`.
pub fn try_load_json5_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let mut file = File::open(&path)
        .err_context(|| format!("opening model JSON5 file: {}", path.as_ref().display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .err_context(|| format!("reading model JSON5 file: {}", path.as_ref().display()))?;
    let root: serde_json::Value = json5::from_str(&content)
        .err_context(|| format!("parsing model JSON5 file: {}", path.as_ref().display()))?;

    // Extract the `model_generators` array; error if missing or not an array
    let generators = match root.get("model_generators").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid 'model_generators' array",
            )));
        }
    };

    // Stream each entry into the existing loader
    let items = generators.iter().cloned().map(Ok);
    try_load_models_from_values(index, items, out)
}

/// Load models from a file. The file extension is used to decide whether to load as `json`,
/// `jsonl`, or `json5`.
///
/// Everything the file matched is *appended* to `out`, which is what lets a caller accumulate
/// across every (import x model file) pair without a merge step. See
/// [`try_load_models_from_values`] for the error contract that appending implies.
pub fn try_load_models<P: AsRef<std::path::Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let path = path.as_ref();
    let extension = path.extension().and_then(|s| s.to_str());
    match extension {
        Some("jsonl") => {
            let file = File::open(path)
                .err_context(|| format!("opening model JSONL file: {}", path.display()))?;
            let rdr = BufReader::new(file);
            try_load_jsonl_models(index, rdr, out)
        }
        Some("json5") => try_load_json5_models(index, path, out),
        _ => try_load_json_models(index, path, out),
    }
}

/// Load models from a stream of json Values. This processing is batched for efficiency, so the
/// iterator can be large and lazy.
///
/// # Errors leave `out` partially written
///
/// On `Err`, `out` holds every row appended before the error. This is not new: JSON model
/// errors are *collected* across a batch and returned only at the end (see
/// [`json::ModelGeneratorIngest::encode_models_from`]), long after rows have been emitted --
/// the out-param merely makes the existing partial-append semantic visible in the caller's
/// accumulator instead of hiding it in a batch that was discarded. Every production caller
/// propagates with `?` and aborts, so nothing observable depends on it; `json_error_handling.rs`
/// pins that rows emitted before a collected error stay readable.
pub fn try_load_models_from_values(
    index: &ProgramMatchIndex<'_>,
    mut items: impl Iterator<Item = Result<serde_json::Value, Error>>,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error> {
    let mut model_gen = json::ModelGeneratorIngest::new(index, out);
    let batch_size = 1024;
    let mut batch: Vec<serde_json::Value> = Vec::with_capacity(batch_size);

    // Index of the first generator of the current batch within the model file. Generator
    // indices must count across batches: they name the generator in JSON error messages and
    // in the `CTADL0004` SARIF notification, and they key `endpoint_stats`.
    let mut base = 0usize;

    loop {
        // Fill the batch. `take` is what bounds it: pulling from `items` directly and
        // breaking on a full batch would consume an item and then drop it on the floor,
        // silently losing every 1025th generator in the file.
        for item in items.by_ref().take(batch_size) {
            batch.push(item?);
        }
        if batch.is_empty() {
            break;
        }
        // Process the batch
        let count = batch.len();
        model_gen
            .encode_models_from(base, batch.drain(..))
            .err_context(|| "encoding models".to_string())?;
        batch.clear();
        base += count;
    }
    let report = ModelLoadReport {
        endpoint_stats: std::mem::take(&mut model_gen.endpoint_stats),
        index_time_models: model_gen.index_time_models,
    };
    // `model_gen` holds the `&mut out` borrow, so dropping it is what lets the counters be read
    // back alongside the rows it appended.
    drop(model_gen);
    log::trace!("matched {} summary models", out.propagations.len());
    log::trace!("matched {} source/sink models", out.endpoints.len());
    Ok(report)
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
    /// Deliberately *not* keyed by model file: two of the loader entry points take a reader
    /// and a value stream, which have no path, so file identity belongs to the caller.
    /// `cli::query` re-keys each file's stats by file before merging them, which is also what
    /// keeps two files that number their generators the same from conflating.
    pub endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats>,
    /// How many generators declared an index-time-only construct. Lets `ctadl query` say once
    /// that it is ignoring them instead of dropping them in silence.
    pub index_time_models: IndexTimeModelCounts,
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
