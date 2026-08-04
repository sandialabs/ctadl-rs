/*! `ctadl check-models`: what a model file matches, before there is an index.

Model matching is already split in two, and only the second half needs an index. **Stage 1**
evaluates every generator's `where` against one program's name / parent / signature /
qualified-id tables -- it reads an artifact import and nothing else. **Stage 2** resolves the
matched names to [`crate::facts::FunctionId`]s, fans endpoints out over call sites, and expands
wildcard sinks. This command runs Stage 1 alone, so a model file can be made ready while the
index is still building.

Two passes are joined into one report:

- **Inventory**, per model file and needing no program at all: what each generator declares
  (`find`, its `model` keys, its ports) and what its `in` clause says. This is what makes the
  report honest about scope -- [`crate::models::json::ModelGeneratorIngest::visit_model_generator`]
  returns early for a generator whose scope excludes the import, leaving *no* stats entry, which
  is indistinguishable from "matched nothing" if you only look at the matching pass.
- **Matching**, per import: the ordinary Stage-1 pass with the per-generator capture turned on.

# What it deliberately cannot tell you

Each of these is reported inline as a [`Caveat`] rather than left implicit, because a count that
looks like a match but is not is worse than no count. See [`Caveat::describe`].
*/

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::path::{Path as FsPath, PathBuf};

use serde::Serialize;

use crate::error::{Error, ErrorContext};
use crate::facts::TaintDirection;
use crate::models::matches::diagnose;
use crate::models::spec::{self, BridgeSpec, ProgramScope};
use crate::models::{
    self, EndpointStats, ImportScope, MatchedFunctions, ModelCheckError, ProgramMatchIndex,
    ProgramModelMatches, PropagationStats, UnmatchedReason,
};
use crate::project::{AnalysisProject, ArtifactImport};
use ctadl_ir::ProgramInfo;

/// How to run the check.
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions<'a> {
    /// The `--models` files, in the order given. Spelled the same as `index`/`query`.
    pub models: &'a [PathBuf],
    /// Also check the built-in per-language propagation defaults. Off by default: the Java
    /// file alone is dozens of generators and would bury the file being edited.
    pub default_models: bool,
    /// `None` reports counts only; `Some(0)` lists every matched name; `Some(n)` lists at
    /// most `n`.
    pub show_matches: Option<usize>,
}

impl CheckOptions<'_> {
    /// The name cap to hand the matcher: how many matched names to retain per generator.
    fn capture(&self) -> usize {
        match self.show_matches {
            None => 0,
            Some(0) => usize::MAX,
            Some(n) => n,
        }
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// Everything the check found. One data type, two renderers ([`Self::render_human`] and
/// `serde`), so the text and the JSON can never disagree.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckReport {
    /// The imports matched against, in the order they were checked. Empty when no import was
    /// named, which is the file-lint mode.
    pub imports: Vec<ImportSummary>,
    /// One entry per model file, in the order given, plus any built-in default file an import
    /// selected.
    pub files: Vec<FileReport>,
    /// Errors belonging to the run rather than to one file -- a bridge whose constraints only
    /// the evaluator could reject, for instance.
    pub errors: Vec<String>,
}

/// One import the generators were matched against.
#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub name: String,
    /// The artifact language, when the caller knew it.
    pub language: Option<String>,
    /// Functions in the import's IR. This is what a [`MatchedFunctions::All`] generator matches,
    /// and it is known even for a frontend whose method table is `Unknown`.
    pub functions: usize,
    /// Set when the import could not be loaded; the other fields are then placeholders.
    pub error: Option<String>,
}

/// One model file's generators, in file order.
#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub path: PathBuf,
    /// True for a built-in default file, which has no path on disk.
    pub builtin: bool,
    /// Deduplicated by rendered text: a shape error is a property of the file, so matching the
    /// same file against five imports raises it five times.
    pub errors: Vec<String>,
    pub generators: Vec<GeneratorReport>,
}

/// What one generator declares, and what Stage 1 did with it.
#[derive(Debug, Clone, Serialize)]
pub struct GeneratorReport {
    /// The generator's index in the file -- the same one JSON errors and `CTADL0004` name.
    pub index: usize,
    pub find: Option<String>,
    /// The rendered `in` clause, when there is one.
    pub scope: Option<String>,
    /// The named imports this generator's `in` clause admits. Empty with a non-empty import
    /// list means the clause excluded every import named, which is not the same condition as
    /// matching nothing.
    pub applicable_imports: Vec<String>,
    /// A sample of the matched function names, capped by `--show-matches`.
    pub functions: BTreeSet<String>,
    /// How many functions the `where` selected, summed over imports. `None` means the
    /// generator was unnarrowed in some import -- it matches *every* function there, which is
    /// rendered against that import's function count and never counted as dead.
    pub functions_total: Option<usize>,
    /// What the `model` declares, in a fixed order: sources, sinks, propagation, access paths,
    /// bridge.
    pub kinds: Vec<GeneratorKind>,
    pub caveats: Vec<Caveat>,
}

/// One thing a generator's `model` declares, with what Stage 1 could say about it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorKind {
    Endpoint {
        /// `sources` or `sinks`.
        direction: EndpointDirection,
        /// The port spellings the file declares, verbatim.
        ports: Vec<String>,
        stats: EndpointReport,
    },
    Propagation {
        /// `input -> output`, one per entry.
        ports: Vec<String>,
        ports_declared: usize,
        /// Summary rows Stage 1 emitted.
        rows: usize,
    },
    AccessPaths {
        declared: usize,
        registered: usize,
    },
    Bridge {
        from_matched: usize,
        to_matched: usize,
        /// [`diagnose`]'s verdict. There is deliberately no pair count: pairing needs the fact
        /// base.
        diagnosis: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointDirection {
    Sources,
    Sinks,
}

impl EndpointDirection {
    fn of(direction: TaintDirection) -> Self {
        match direction {
            TaintDirection::Forward => EndpointDirection::Sources,
            TaintDirection::Backward => EndpointDirection::Sinks,
        }
    }

    fn label(self) -> &'static str {
        match self {
            EndpointDirection::Sources => "sources",
            EndpointDirection::Sinks => "sinks",
        }
    }
}

/// [`EndpointStats`], with its reasons rendered. A mirror rather than the type itself so the
/// JSON shape belongs to this command and does not pin the matcher's internals.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EndpointReport {
    pub ports_declared: usize,
    pub endpoints_matched: usize,
    pub functions_matched: usize,
    /// Why nothing matched. Meaningful only when `endpoints_matched` is zero.
    pub unmatched: Vec<String>,
}

impl EndpointReport {
    fn of(stats: &EndpointStats) -> Self {
        Self {
            ports_declared: stats.ports_declared,
            endpoints_matched: stats.endpoints_matched,
            functions_matched: stats.functions_matched,
            unmatched: stats.unmatched.iter().map(unmatched_reason).collect(),
        }
    }
}

/// Why a declared port produced no endpoint row, in one clause.
fn unmatched_reason(reason: &UnmatchedReason) -> String {
    match reason {
        UnmatchedReason::NoFunctionMatched => "no function matched `where`".to_string(),
        UnmatchedReason::LocalNotFound(name) => {
            format!("functions matched, but none declares a local named `{name}`")
        }
        UnmatchedReason::NoCallerMatched => "no caller matched `in_function`".to_string(),
        UnmatchedReason::PortRejected => "the port is not usable in this generator".to_string(),
    }
}

/// A count this command can produce that does not mean what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Caveat {
    /// `find: callsites`: Stage 1 matches the callee, and the caller for `in_function`. The
    /// call-site fan-out itself is Stage 2.
    CallsiteFanout,
    /// `Argument(*)`: expanded over an arity computed from actual parameters and call sites
    /// across every import, which only the fact base has.
    AnyArgument,
    /// A bridge's two sides can live in two imports, and its pair count needs the fact base.
    BridgePairs,
}

impl Caveat {
    /// Which row a caveat belongs on. A caveat printed against the wrong count is no better
    /// than no caveat at all.
    fn applies_to(self, kind: &GeneratorKind) -> bool {
        match self {
            Caveat::CallsiteFanout | Caveat::AnyArgument => {
                matches!(
                    kind,
                    GeneratorKind::Endpoint { .. } | GeneratorKind::Propagation { .. }
                )
            }
            Caveat::BridgePairs => matches!(kind, GeneratorKind::Bridge { .. }),
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Caveat::CallsiteFanout => {
                "call sites need the index: a matched callee is not yet a call site"
            }
            Caveat::AnyArgument => "`Argument(*)` expands over an arity only the index knows",
            Caveat::BridgePairs => {
                "bridge pairing needs the index, and a bridge's sides may span imports"
            }
        }
    }
}

impl GeneratorReport {
    /// Whether this generator declared a model and matched nothing -- what `--strict` fails on.
    ///
    /// Three conditions are deliberately *not* dead. A generator declaring no model at all has
    /// nothing to match. A generator whose `in` clause admits none of the named imports was
    /// never attempted. And a generator with no `where` matches every function in some import
    /// ([`MatchedFunctions::All`]), whose count this command refuses to enumerate -- reporting
    /// it as zero is the count-that-lies the whole command exists to prevent.
    pub fn is_dead(&self) -> bool {
        if self.kinds.is_empty()
            || self.applicable_imports.is_empty()
            || self.functions_total.is_none()
        {
            return false;
        }
        self.kinds.iter().all(|kind| match kind {
            GeneratorKind::Endpoint { stats, .. } => stats.endpoints_matched == 0,
            GeneratorKind::Propagation { rows, .. } => *rows == 0,
            // A declared path is registered unconditionally; there is nothing to match.
            GeneratorKind::AccessPaths { registered, .. } => *registered == 0,
            GeneratorKind::Bridge {
                from_matched,
                to_matched,
                ..
            } => *from_matched == 0 || *to_matched == 0,
        })
    }
}

impl CheckReport {
    /// The generators `--strict` fails on, as `(file, generator index)`.
    ///
    /// Empty in file-lint mode: with no import to match against, no generator can be called
    /// dead.
    pub fn dead_generators(&self) -> Vec<(&FsPath, usize)> {
        if self.imports.is_empty() {
            return Vec::new();
        }
        self.files
            .iter()
            .flat_map(|file| {
                file.generators
                    .iter()
                    .filter(|g| g.is_dead())
                    .map(move |g| (file.path.as_path(), g.index))
            })
            .collect()
    }

    /// Whether any file, or the run itself, reported an error.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty() || self.files.iter().any(|f| !f.errors.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Store-facing: resolves import (or project) names and checks every model file against them.
///
/// A name that resolves to a project is expanded to that project's imports, so `check-models
/// app` works both before and after `ctadl index app`. Naming an import expands it with its
/// `sub_imports` -- an APK's native libraries -- matching what
/// [`AnalysisProject::try_create`] does, but without calling it: that writes a project config
/// into the store, and checking a model file must not.
///
/// With no name at all this runs the inventory pass alone: parse every file, check every key
/// and port, and print the generator inventory. That is the file lint, and the report says
/// plainly that nothing was matched against a program.
pub fn check_models(imports: &[String], opts: CheckOptions<'_>) -> Result<CheckReport, Error> {
    let names = resolve_import_names(imports)?;
    // Lazy on purpose: each item loads one import's IR, and the loop drops it before the next
    // one is loaded. A borrowed item would keep every import's IR alive for the whole loop,
    // which on a real APK is gigabytes.
    let programs = names.into_iter().map(|name| {
        let import = ArtifactImport::load_by_name(&name)
            .err_context(|| format!("loading import: '{name}'"))?;
        let scope = ImportScope::new(import.language, &import.name);
        let program_info = super::load_program_info_without_source_info(&import)
            .err_context(|| format!("loading import: '{name}'"))?;
        Ok((scope, program_info))
    });
    check_programs(programs, opts)
}

/// Expands the named imports and projects into the import names to match against, deduplicated
/// and order-preserving.
fn resolve_import_names(names: &[String]) -> Result<Vec<String>, Error> {
    let mut resolved = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in names {
        // An import first: `ctadl index app` creates a project named `app` *and* leaves the
        // import named `app`, and expanding the import gives the same set the project holds.
        let expanded: Vec<String> = match ArtifactImport::load_by_name(name) {
            Ok(import) => std::iter::once(import.name.clone())
                .chain(import.sub_imports.iter().cloned())
                .collect(),
            Err(import_error) => match AnalysisProject::try_load_name(name) {
                Ok(project) => project.imports.clone(),
                // Report the import error, not the project one: naming an import is the
                // common case, and its message says what was actually looked for.
                Err(_) => return Err(import_error).err_context(|| format!("resolving '{name}'")),
            },
        };
        for name in expanded {
            if seen.insert(name.clone()) {
                resolved.push(name);
            }
        }
    }
    Ok(resolved)
}

/// The core. Needs no store.
///
/// The iterator's items are **owned**, and that is load-bearing: an owned item can be produced
/// lazily and dropped at the end of its loop iteration, which is the streaming posture
/// [`super::index`] keeps. An `Err` item is recorded and the loop continues -- one unreadable
/// import must not cost the report for the others.
pub fn check_programs(
    programs: impl IntoIterator<Item = Result<(ImportScope, ProgramInfo), Error>>,
    opts: CheckOptions<'_>,
) -> Result<CheckReport, Error> {
    let capture = opts.capture();
    let mut report = CheckReport::default();
    let mut files: Vec<FileState> = Vec::new();
    let mut bridges: Vec<BridgeSpec> = Vec::new();

    // Inventory: program-independent, so it runs once, before any IR is loaded.
    for path in opts.models {
        let mut state = FileState::new(path.clone(), false);
        let readable = state.take_inventory(&ModelSource::File(path.clone()));
        // Scanned per file so a malformed bridge names the file it is in. One rejected bridge
        // costs that file's bridges, not the run's. Skipped when the inventory could not read
        // the file: the scan would fail on the same thing and say it twice.
        if readable {
            match spec::scan_model_files(std::slice::from_ref(path)) {
                Ok(specs) => bridges.extend(specs.bridges),
                Err(e) => state.record_error(render_chain(&e)),
            }
        }
        files.push(state);
    }

    // Matching, per import.
    let mut matches = ProgramModelMatches::default();
    for item in programs {
        let (scope, program_info) = match item {
            Ok(loaded) => loaded,
            Err(e) => {
                report.imports.push(ImportSummary {
                    name: "<unloadable import>".to_string(),
                    language: None,
                    functions: 0,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };
        let match_index = ProgramMatchIndex::new(&program_info, scope.clone());

        // A built-in default file joins the report the first time an import selects it, so
        // `--default-models` shows what it loads rather than folding it in invisibly.
        let builtin = if opts.default_models {
            models::default_model_file(match_index.vmt())
        } else {
            None
        };
        if let Some((name, contents)) = builtin
            && !files
                .iter()
                .any(|f| f.builtin && f.path == FsPath::new(name))
        {
            let mut state = FileState::new(PathBuf::from(name), true);
            state.take_inventory(&ModelSource::Builtin { contents });
            files.push(state);
        }

        // Which generators the `in` clause admits, accumulated per import. Computed here and
        // not from the matching pass because a scoped-out generator leaves no stats at all.
        for file in &mut files {
            file.note_applicable(&scope);
        }

        if let Some((name, contents)) = builtin {
            let (load, errors) = models::try_check_jsonl_models(
                &match_index,
                BufReader::new(contents),
                capture,
                &mut matches,
            );
            let file = files
                .iter_mut()
                .find(|f| f.builtin && f.path == FsPath::new(name))
                .expect("the builtin file was just added");
            file.absorb(load, errors, capture);
        }
        for file in files.iter_mut().filter(|f| !f.builtin) {
            let (load, errors) =
                models::try_check_models(&match_index, &file.path, capture, &mut matches);
            file.absorb(load, errors, capture);
        }

        // Bridge sides, matched exactly as indexing matches them. An evaluator error here is
        // about the run rather than about one file: the spec it came from is not identified.
        if let Err(e) = models::matches::observe_import(&match_index, &bridges, &mut matches) {
            let text = render_chain(&e);
            if !report.errors.contains(&text) {
                report.errors.push(text);
            }
        }

        report.imports.push(ImportSummary {
            name: scope.import.clone().unwrap_or_else(|| scope.describe()),
            language: scope.language.map(|l| l.name().to_string()),
            functions: program_info.program.functions.len(),
            error: None,
        });
        // `program_info` and `match_index` drop here -- the same streaming posture
        // `cli::index` keeps, which the owned iterator items exist to preserve.
    }

    // Bridge verdicts, once, over every import: "unmatched" means not matched anywhere. With no
    // import at all there is nothing to diagnose, and saying "matched nothing" would be a lie
    // about a pass that never ran.
    if !report.imports.is_empty() {
        for (i, spec) in bridges.iter().enumerate() {
            let side = matches.bridges.get(i);
            let diagnosis = diagnose(spec, side).map(|(_, message)| message);
            for file in files.iter_mut().filter(|f| f.path == spec.source) {
                file.set_bridge(
                    spec.index,
                    side.from.len(),
                    side.to.len(),
                    diagnosis.clone(),
                );
            }
        }
    }

    report.files = files
        .into_iter()
        .map(|f| f.finish(!report.imports.is_empty()))
        .collect();
    Ok(report)
}

// ---------------------------------------------------------------------------
// Per-file accumulation
// ---------------------------------------------------------------------------

/// Where a file's generators are read from. The built-in defaults have no path on disk, and
/// the inventory has to walk them the same way it walks a `--models` file.
enum ModelSource {
    File(PathBuf),
    Builtin { contents: &'static [u8] },
}

/// One model file, accumulating across imports.
struct FileState {
    path: PathBuf,
    builtin: bool,
    errors: Vec<String>,
    generators: Vec<GeneratorState>,
    /// Generator index -> position in [`Self::generators`]. Not the same thing: a stream error
    /// can truncate the inventory, leaving gaps the matching pass still reports on.
    positions: BTreeMap<usize, usize>,
}

/// What the inventory read about one generator, plus what matching added.
struct GeneratorState {
    index: usize,
    find: Option<String>,
    scope_text: Option<String>,
    scope: ProgramScope,
    /// A generator whose `in` is malformed is admitted by nothing, exactly as the matcher
    /// treats it.
    scope_malformed: bool,
    applicable_imports: Vec<String>,
    source_ports: Vec<String>,
    sink_ports: Vec<String>,
    propagation_ports: Vec<String>,
    access_paths_declared: usize,
    has_bridge: bool,
    matched: Option<MatchedFunctions>,
    endpoint_stats: BTreeMap<TaintDirection, EndpointStats>,
    propagation_stats: PropagationStats,
    access_paths_registered: usize,
    bridge: Option<(usize, usize, Option<String>)>,
}

impl FileState {
    fn new(path: PathBuf, builtin: bool) -> Self {
        Self {
            path,
            builtin,
            errors: Vec::new(),
            generators: Vec::new(),
            positions: BTreeMap::new(),
        }
    }

    /// Shape errors are a property of the file, and the file is matched once per import, so the
    /// same error arrives once per import. Dedup by rendered text.
    fn record_error(&mut self, text: String) {
        if !self.errors.contains(&text) {
            self.errors.push(text);
        }
    }

    /// Reads what every generator declares, without any program. Returns whether the file was
    /// read to the end.
    fn take_inventory(&mut self, source: &ModelSource) -> bool {
        let mut visit = |n: usize, value: &serde_json::Value| {
            let mut scope_errors = Vec::new();
            let scope = ProgramScope::parse(value.get("in"), n, &mut scope_errors);
            let scope_malformed = !scope_errors.is_empty();
            let state = GeneratorState {
                index: n,
                find: value
                    .get("find")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                scope_text: value.get("in").map(render_scope),
                scope,
                scope_malformed,
                applicable_imports: Vec::new(),
                source_ports: ports_of(value, "sources"),
                sink_ports: ports_of(value, "sinks"),
                propagation_ports: propagation_ports_of(value),
                access_paths_declared: value
                    .pointer("/model/access_paths")
                    .and_then(|v| v.as_array())
                    .map_or(0, Vec::len),
                has_bridge: value.pointer("/model/bridge").is_some(),
                matched: None,
                endpoint_stats: BTreeMap::new(),
                propagation_stats: PropagationStats::default(),
                access_paths_registered: 0,
                bridge: None,
            };
            self.positions.insert(n, self.generators.len());
            self.generators.push(state);
        };
        let outcome = match source {
            ModelSource::File(path) => spec::visit_model_file(path, |n, value| {
                visit(n, value);
                Ok(())
            }),
            ModelSource::Builtin { contents } => visit_jsonl_bytes(contents, visit),
        };
        match outcome {
            Ok(()) => true,
            Err(e) => {
                self.record_error(render_chain(&e));
                false
            }
        }
    }

    /// Records which of the generators this import's scope admits.
    fn note_applicable(&mut self, scope: &ImportScope) {
        let name = scope.import.clone().unwrap_or_else(|| scope.describe());
        for generator in &mut self.generators {
            if !generator.scope_malformed && generator.scope.admits(scope) {
                generator.applicable_imports.push(name.clone());
            }
        }
    }

    /// Folds in one import's load of this file.
    fn absorb(
        &mut self,
        load: models::ModelLoadReport,
        errors: Vec<ModelCheckError>,
        capture: usize,
    ) {
        for error in errors {
            self.record_error(render_chain(&error));
        }
        for ((index, direction), stats) in &load.endpoint_stats {
            if let Some(generator) = self.generator_mut(*index) {
                generator
                    .endpoint_stats
                    .entry(*direction)
                    .or_default()
                    .merge(stats);
            }
        }
        for (index, matched) in &load.matched {
            if let Some(generator) = self.generator_mut(*index) {
                match &mut generator.matched {
                    Some(existing) => existing.merge(matched, capture),
                    slot @ None => *slot = Some(matched.clone()),
                }
            }
        }
        for (index, stats) in &load.propagation_stats {
            if let Some(generator) = self.generator_mut(*index) {
                generator.propagation_stats.merge(stats);
            }
        }
        for (index, count) in &load.access_path_stats {
            if let Some(generator) = self.generator_mut(*index) {
                // A property of the file, not of the program: the same paths are re-declared
                // once per import.
                generator.access_paths_registered = generator.access_paths_registered.max(*count);
            }
        }
    }

    fn set_bridge(
        &mut self,
        index: usize,
        from_matched: usize,
        to_matched: usize,
        diagnosis: Option<String>,
    ) {
        if let Some(generator) = self.generator_mut(index) {
            generator.bridge = Some((from_matched, to_matched, diagnosis));
        }
    }

    fn generator_mut(&mut self, index: usize) -> Option<&mut GeneratorState> {
        let position = *self.positions.get(&index)?;
        self.generators.get_mut(position)
    }

    fn finish(self, matched_against_a_program: bool) -> FileReport {
        FileReport {
            path: self.path,
            builtin: self.builtin,
            errors: self.errors,
            generators: self
                .generators
                .into_iter()
                .map(|g| g.finish(matched_against_a_program))
                .collect(),
        }
    }
}

impl GeneratorState {
    fn finish(self, matched_against_a_program: bool) -> GeneratorReport {
        let mut kinds = Vec::new();
        for (direction, ports) in [
            (TaintDirection::Forward, &self.source_ports),
            (TaintDirection::Backward, &self.sink_ports),
        ] {
            if ports.is_empty() {
                continue;
            }
            kinds.push(GeneratorKind::Endpoint {
                direction: EndpointDirection::of(direction),
                ports: ports.clone(),
                stats: self
                    .endpoint_stats
                    .get(&direction)
                    .map(EndpointReport::of)
                    .unwrap_or_default(),
            });
        }
        if !self.propagation_ports.is_empty() {
            kinds.push(GeneratorKind::Propagation {
                ports: self.propagation_ports.clone(),
                ports_declared: self.propagation_stats.ports_declared,
                rows: self.propagation_stats.rows,
            });
        }
        if self.access_paths_declared > 0 {
            kinds.push(GeneratorKind::AccessPaths {
                declared: self.access_paths_declared,
                registered: self.access_paths_registered,
            });
        }
        if self.has_bridge {
            let (from_matched, to_matched, diagnosis) = self.bridge.unwrap_or((0, 0, None));
            kinds.push(GeneratorKind::Bridge {
                from_matched,
                to_matched,
                diagnosis,
            });
        }

        let mut caveats = Vec::new();
        if self.find.as_deref() == Some("callsites") {
            caveats.push(Caveat::CallsiteFanout);
        }
        if self
            .source_ports
            .iter()
            .chain(&self.sink_ports)
            .chain(&self.propagation_ports)
            .any(|p| p.contains("Argument(*)"))
        {
            caveats.push(Caveat::AnyArgument);
        }
        if self.has_bridge {
            caveats.push(Caveat::BridgePairs);
        }

        GeneratorReport {
            index: self.index,
            find: self.find,
            scope: self.scope_text,
            applicable_imports: self.applicable_imports,
            functions: self
                .matched
                .as_ref()
                .map(|m| m.names().clone())
                .unwrap_or_default(),
            // Absent entirely when no program was matched -- not zero, which would read as
            // "matched nothing".
            functions_total: if matched_against_a_program {
                self.matched.as_ref().and_then(MatchedFunctions::total)
            } else {
                None
            },
            kinds,
            caveats,
        }
    }
}

/// Walks the generators of a `jsonl` byte slice, the way [`spec::visit_model_file`] walks a
/// file. Skipped lines do not consume a generator index.
fn visit_jsonl_bytes<F>(contents: &[u8], mut f: F) -> Result<(), Error>
where
    F: FnMut(usize, &serde_json::Value),
{
    use std::io::BufRead as _;
    let mut n = 0usize;
    for (lineno, line) in BufReader::new(contents).lines().enumerate() {
        let line = line?;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .err_context(|| format!("reading model line {}", lineno + 1))?;
        f(n, &value);
        n += 1;
    }
    Ok(())
}

/// The `port` spellings of a `model.sources` / `model.sinks` list, verbatim.
fn ports_of(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .pointer(&format!("/model/{key}"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("port")
                        .and_then(|p| p.as_str())
                        .unwrap_or("<no port>")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `input -> output` per `model.propagation` entry.
fn propagation_ports_of(value: &serde_json::Value) -> Vec<String> {
    value
        .pointer("/model/propagation")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let field = |key: &str| {
                        item.get(key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string()
                    };
                    format!("{} -> {}", field("input"), field("output"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The whole error chain on one line.
///
/// [`Error::Context`] displays as its context alone, so rendering only the top of the chain
/// prints "opening model JSON file: x.json" and drops the half that says why -- which is the
/// only half a reader can act on.
fn render_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let next = cause.to_string();
        // A delegating `Display` (as `ModelCheckError` has) would otherwise repeat itself.
        if !text.ends_with(&next) {
            text.push_str(": ");
            text.push_str(&next);
        }
        source = cause.source();
    }
    text
}

/// How to spell an `in` clause on one line.
fn render_scope(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl CheckReport {
    /// The `--format human` rendering.
    pub fn render_human(&self, w: &mut impl Write) -> std::io::Result<()> {
        if self.imports.is_empty() {
            writeln!(
                w,
                "imports: none -- checking the model files alone. Name an import (or a \
                 project) to see what each generator matches."
            )?;
        } else {
            let described: Vec<String> = self
                .imports
                .iter()
                .map(|import| match &import.error {
                    Some(error) => format!("{} (not loaded: {error})", import.name),
                    None => match &import.language {
                        Some(language) => format!(
                            "{} ({language}, {} functions)",
                            import.name, import.functions
                        ),
                        None => format!("{} ({} functions)", import.name, import.functions),
                    },
                })
                .collect();
            writeln!(w, "imports: {}", described.join(", "))?;
        }
        for error in &self.errors {
            writeln!(w, "error: {error}")?;
        }

        let mut generators = 0usize;
        let mut dead = 0usize;
        for file in &self.files {
            writeln!(w)?;
            if file.builtin {
                writeln!(w, "{} (built-in defaults)", file.path.display())?;
            } else {
                writeln!(w, "{}", file.path.display())?;
            }
            for error in &file.errors {
                writeln!(w, "  error: {error}")?;
            }
            let mut rows: Vec<Row> = Vec::new();
            for generator in &file.generators {
                generators += 1;
                if generator.is_dead() {
                    dead += 1;
                }
                rows.extend(generator.rows(&self.imports));
            }
            write_rows(w, &rows)?;
            for generator in &file.generators {
                if generator.functions.is_empty() {
                    continue;
                }
                writeln!(w, "  [{}] matched:", generator.index)?;
                for name in &generator.functions {
                    writeln!(w, "        {name}")?;
                }
                if let Some(total) = generator.functions_total
                    && total > generator.functions.len()
                {
                    writeln!(
                        w,
                        "        … and {} more",
                        total - generator.functions.len()
                    )?;
                }
            }
        }

        writeln!(w)?;
        if self.imports.is_empty() {
            writeln!(
                w,
                "{generators} generator(s); nothing was matched against a program"
            )?;
        } else {
            writeln!(
                w,
                "{generators} generator(s); {} matched, {dead} did not",
                generators - dead
            )?;
        }
        Ok(())
    }
}

/// One line of the human table. `note` is last because it is the only free-text column.
struct Row {
    index: String,
    find: String,
    kind: String,
    ports: String,
    result: String,
    note: String,
}

impl GeneratorReport {
    /// One row per declared model kind, or a single row saying the generator declares nothing.
    fn rows(&self, imports: &[ImportSummary]) -> Vec<Row> {
        let index = format!("[{}]", self.index);
        let find = self.find.clone().unwrap_or_else(|| "?".to_string());
        // In file-lint mode nothing was matched, so no generator can have "matched nothing":
        // every count is absent rather than zero, and so is every reason one would be zero.
        let matched = !imports.is_empty();
        // The one condition no count can express: the `in` clause excluded every import named,
        // so this generator was never attempted. Said instead of a count, never alongside one.
        let scoped_out = matched && self.applicable_imports.is_empty();
        let scope_note = || match &self.scope {
            Some(scope) => format!("`in: {scope}` admits no import you named"),
            None => "no import you named admits it".to_string(),
        };

        if self.kinds.is_empty() {
            return vec![Row {
                index,
                find,
                kind: "(no model)".to_string(),
                ports: String::new(),
                result: "-".to_string(),
                note: if scoped_out {
                    scope_note()
                } else {
                    String::new()
                },
            }];
        }

        self.kinds
            .iter()
            .map(|kind| {
                let (kind_name, ports, result, mut note) = match kind {
                    GeneratorKind::Endpoint {
                        direction,
                        ports,
                        stats,
                    } => (
                        direction.label().to_string(),
                        ports.join(", "),
                        self.function_column(scoped_out),
                        // The count in the result column is of *functions*, so a port that
                        // matched functions and still produced no endpoint needs the reason
                        // spelled out -- that is exactly the case a count hides.
                        if scoped_out {
                            scope_note()
                        } else if matched && stats.endpoints_matched == 0 {
                            stats.unmatched.join("; ")
                        } else {
                            String::new()
                        },
                    ),
                    GeneratorKind::Propagation { ports, rows, .. } => (
                        "propagation".to_string(),
                        ports.join(", "),
                        self.function_column(scoped_out),
                        if scoped_out {
                            scope_note()
                        } else if matched && *rows == 0 {
                            "no function matched `where`".to_string()
                        } else {
                            String::new()
                        },
                    ),
                    GeneratorKind::AccessPaths {
                        declared,
                        registered,
                    } => (
                        "access_paths".to_string(),
                        format!("{declared} declared"),
                        if matched {
                            format!("{registered} registered")
                        } else {
                            "-".to_string()
                        },
                        String::new(),
                    ),
                    GeneratorKind::Bridge {
                        from_matched,
                        to_matched,
                        diagnosis,
                    } => (
                        "bridge".to_string(),
                        // Two side counts and no pair count: pairing needs the fact base.
                        if matched {
                            format!("from {from_matched}, to {to_matched}")
                        } else {
                            String::new()
                        },
                        String::new(),
                        diagnosis.clone().unwrap_or_default(),
                    ),
                };
                for caveat in self.caveats.iter().filter(|c| c.applies_to(kind)) {
                    if !note.is_empty() {
                        note.push_str("; ");
                    }
                    note.push_str("* ");
                    note.push_str(caveat.describe());
                }
                Row {
                    index: index.clone(),
                    find: find.clone(),
                    kind: kind_name,
                    ports,
                    result,
                    note,
                }
            })
            .collect()
    }

    /// The result column: what the `where` selected, in the unit the `find` names.
    ///
    /// Never a number for an unnarrowed generator. Its count exists only relative to an
    /// import's function count, and printing a zero there is exactly the count-that-lies this
    /// command exists to prevent.
    fn function_column(&self, scoped_out: bool) -> String {
        if scoped_out {
            return "-".to_string();
        }
        let unit = if self.find.as_deref() == Some("callsites") {
            "callee(s)"
        } else {
            "function(s)"
        };
        match self.functions_total {
            Some(total) => format!("{total} {unit}"),
            None if self.applicable_imports.is_empty() => "-".to_string(),
            None => "all functions".to_string(),
        }
    }
}

fn write_rows(w: &mut impl Write, rows: &[Row]) -> std::io::Result<()> {
    // The ports cell is the one column a model file can make arbitrarily wide: a propagation
    // list of four entries spells out eight ports. Truncate the cell rather than let one
    // generator set the indentation for every other row in the file.
    let ports: Vec<String> = rows.iter().map(|r| truncate(&r.ports, 44)).collect();
    let width =
        |f: fn(&Row) -> &String| rows.iter().map(|r| f(r).chars().count()).max().unwrap_or(0);
    let (i, fi, k, r) = (
        width(|r| &r.index),
        width(|r| &r.find),
        width(|r| &r.kind),
        width(|r| &r.result),
    );
    let p = ports.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    for (row, ports) in rows.iter().zip(&ports) {
        // A short note reads as a column; a long one (a bridge verdict is a paragraph) would
        // push every row off the right edge, so it goes on its own line instead.
        let inline = if row.note.len() <= NOTE_COLUMN_WIDTH {
            row.note.as_str()
        } else {
            ""
        };
        let line = format!(
            "  {:i$}  {:fi$}  {:k$}  {:p$}  {:r$}  {}",
            row.index, row.find, row.kind, ports, row.result, inline
        );
        writeln!(w, "{}", line.trim_end())?;
        if inline.is_empty() && !row.note.is_empty() {
            writeln!(w, "        {}", row.note)?;
        }
    }
    Ok(())
}

/// How long a note may be before it gets its own line.
const NOTE_COLUMN_WIDTH: usize = 72;

/// `text`, shortened to `width` characters with a trailing ellipsis.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}
