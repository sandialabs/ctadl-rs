/*! What a model file matches, when there is no index to query.

Model matching is split in two, and only the second half needs an index. **Stage 1** evaluates
every generator's `where` against one program's name / parent / signature / qualified-id tables
-- it reads an artifact import and nothing else. **Stage 2** resolves the matched names to
[`crate::facts::FunctionId`]s, fans endpoints out over call sites, and expands wildcard sinks.

`ctadl query` runs Stage 1 either way. When the project has no index it cannot run Stage 2, and
instead of failing outright it reports what Stage 1 alone found (see [`super::query`]) -- which
is most of what a model file being written needs, because functions are never optimized out, so
a source or sink naming a function call is fully decided in Stage 1.

Two passes are joined into one [`ModelCheck`]:

- **Inventory**, per model file and needing no program at all: what each generator declares
  (`find`, its `model` keys) and what its `in` clause says. This is what makes the report honest
  about scope -- [`crate::models::json::ModelGeneratorIngest::visit_model_generator`] returns
  early for a generator whose scope excludes the import, leaving *no* stats entry, which is
  indistinguishable from "matched nothing" if you only look at the matching pass.
- **Matching**, per import: the ordinary Stage-1 pass with the per-generator capture turned on.

# What it deliberately cannot tell you

Each of these is stated once, in the `CTADL0008` notification, rather than left implicit: a
count that looks like a match but is not is worse than no count.

- **`find: callsites`** -- Stage 1 matches the callee, and the caller for `in_function`. The
  call-site fan-out is Stage 2, so "3 callees" does not mean any call site exists.
- **`Argument(*)`** -- expands over an arity computed from actual parameters and call sites
  across every import, which only the fact base has.
- **A matched name can still vanish.** Stage 2 raises `CTADL0005` for a name the index does not
  contain.
- **Bridge pair counts.** [`diagnose`] settles unmatched and ambiguous sides without an index;
  the pair count needs the fact base, so none is reported.
- **Cross-import bridges.** A bridge's sides can live in two imports, so a bridge verdict is
  only meaningful over every import the real project will index.
*/

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, ErrorContext};
use crate::facts::TaintDirection;
use crate::models::matches::diagnose;
use crate::models::spec::{self, BridgeSpec, ProgramScope, Severity};
use crate::models::{
    self, EndpointStats, ImportScope, MatchedFunctions, ModelCheckError, ProgramMatchIndex,
    ProgramModelMatches, PropagationStats,
};
use crate::project::{AnalysisProject, ArtifactImport};
use crate::query_engine::formatter::{
    BridgeDiagnosis, GeneratorMatch, IndexTimeDead, ModelCheck, ModelFileError, QueryDiagnostics,
    ScopeExcluded,
};
use ctadl_ir::ProgramInfo;

/// How many matched function names to keep per generator, for the `CTADL0011` sample.
///
/// A cap and not a flag: the names answer "did this `where` select what I meant", which a
/// handful answers, and the exact count is reported beside them. Nothing here ever enumerates
/// an unnarrowed generator's match set -- see [`MatchedFunctions`].
const SAMPLE_CAP: usize = 5;

/// Everything the check found, in the two pieces `cli::query` hands the SARIF writer.
#[derive(Debug, Default)]
pub struct ModelCheckOutcome {
    /// What only this pass knows, rendered as `CTADL0008`-`CTADL0012`.
    pub check: ModelCheck,
    /// Stage 1's per-declaration counters, keyed the way
    /// [`crate::query_engine::formatter::QueryDiagnostics::generator_stats`] wants them, so an
    /// endpoint that matched nothing raises the same `CTADL0004` it would after an index.
    pub endpoint_stats: BTreeMap<(PathBuf, usize, TaintDirection), EndpointStats>,
}

impl ModelCheckOutcome {
    /// Ports declared in one direction, over every file.
    pub fn ports_declared(&self, direction: TaintDirection) -> usize {
        self.endpoint_stats
            .iter()
            .filter(|((_, _, d), _)| *d == direction)
            .map(|(_, stats)| stats.ports_declared)
            .sum()
    }

    /// Stage-1 rows emitted in one direction, over every file. Pre-fan-out: see the module
    /// docs, and the `CTADL0100` wording that says so in the file itself.
    pub fn rows_matched(&self, direction: TaintDirection) -> usize {
        self.endpoint_stats
            .iter()
            .filter(|((_, _, d), _)| *d == direction)
            .map(|(_, stats)| stats.endpoints_matched)
            .sum()
    }

    /// Whether a model file could not be read, or holds a malformed generator. `ctadl query`
    /// fails the run on this *after* writing the report, which is what explains it.
    pub fn has_file_errors(&self) -> bool {
        !self.check.file_errors.is_empty()
    }

    /// How many declarations matched nothing: one per `CTADL0004` the report will carry.
    ///
    /// In declaration units, not generator ones -- a generator declaring both a source and a
    /// sink can be dead in one and live in the other, and counting it once would have to pick
    /// which.
    pub fn dead_declarations(&self) -> usize {
        self.endpoint_stats
            .values()
            .filter(|stats| stats.endpoints_matched == 0)
            .count()
            + self.check.index_time_dead.len()
    }

    /// What the SARIF writer reports, minus the three fields only the process knows: the
    /// caller fills in `command_line`, `arguments` and `start_time_utc`.
    ///
    /// The declared counts mean exactly what they mean after an index -- model ports -- so
    /// `CTADL0001`--`CTADL0003` read the same either way. The matched counts do not: they are
    /// Stage-1 rows, which `CTADL0100`'s model-check message and `CTADL0008` both say.
    pub fn into_diagnostics(self) -> QueryDiagnostics {
        let (sources_declared, sinks_declared) = (
            self.ports_declared(TaintDirection::Forward),
            self.ports_declared(TaintDirection::Backward),
        );
        let (sources_matched, sinks_matched) = (
            self.rows_matched(TaintDirection::Forward),
            self.rows_matched(TaintDirection::Backward),
        );
        QueryDiagnostics {
            generator_stats: self.endpoint_stats,
            sources_declared,
            sinks_declared,
            sources_matched,
            sinks_matched,
            // Nothing was resolved against an index, so nothing can be reported as missing from
            // one: `CTADL0005` belongs to Stage 2.
            unresolved_functions: Default::default(),
            model_check: Some(self.check),
            ..Default::default()
        }
    }
}

/// Store-facing: checks every model file against every import of `project`.
///
/// The project need not have been indexed, or even created: see
/// [`AnalysisProject::ephemeral`], which is how `ctadl query my-import` works on a name that was
/// only ever imported.
pub fn check_models(
    project: &AnalysisProject,
    models: &[PathBuf],
) -> Result<ModelCheckOutcome, Error> {
    // Lazy on purpose: each item loads one import's IR, and the loop drops it before the next
    // one is loaded. A borrowed item would keep every import's IR alive for the whole loop,
    // which on a real APK is gigabytes.
    let programs = project.imports.iter().map(|name| {
        let import = ArtifactImport::load_by_name(name)
            .err_context(|| format!("loading import: '{name}'"))?;
        let scope = ImportScope::new(import.language, &import.name);
        let program_info = super::load_program_info_without_source_info(&import)
            .err_context(|| format!("loading import: '{name}'"))?;
        Ok((scope, program_info))
    });
    check_programs(programs, models)
}

/// The core. Needs no store.
///
/// The iterator's items are **owned**, and that is load-bearing: an owned item can be produced
/// lazily and dropped at the end of its loop iteration, which is the streaming posture
/// [`super::index`] keeps. An `Err` item is recorded and the loop continues -- one unreadable
/// import must not cost the report for the others.
pub fn check_programs(
    programs: impl IntoIterator<Item = Result<(ImportScope, ProgramInfo), Error>>,
    models: &[PathBuf],
) -> Result<ModelCheckOutcome, Error> {
    let mut outcome = ModelCheckOutcome::default();
    let mut files: Vec<FileState> = Vec::new();
    let mut bridges: Vec<BridgeSpec> = Vec::new();

    // Inventory: program-independent, so it runs once, before any IR is loaded.
    for path in models {
        let mut state = FileState::new(path.clone());
        let readable = state.take_inventory();
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
    let mut imports_checked = 0usize;
    for item in programs {
        let (scope, program_info) = match item {
            Ok(loaded) => loaded,
            // An import that will not load is not the model file's fault, and it is not the
            // other imports' problem either: record it and check the rest.
            Err(e) => {
                outcome.check.file_errors.push(ModelFileError {
                    file: None,
                    message: render_chain(&e),
                });
                continue;
            }
        };
        let match_index = ProgramMatchIndex::new(&program_info, scope.clone());

        // Which generators this import's scope admits, accumulated per import. Computed here
        // and not from the matching pass because a scoped-out generator leaves no stats at all.
        for file in &mut files {
            file.note_applicable(&scope);
        }
        // The built-in default models are deliberately not loaded: `ctadl query` does not load
        // them either, and this is `ctadl query` reporting on the files it was given.
        for file in &mut files {
            let (load, errors) =
                models::try_check_models(&match_index, &file.path, SAMPLE_CAP, &mut matches);
            file.absorb(load, errors);
        }

        // Bridge sides, matched exactly as indexing matches them. An evaluator error here is
        // about the run rather than about one file: the spec it came from is not identified.
        if let Err(e) = models::matches::observe_import(&match_index, &bridges, &mut matches) {
            let message = render_chain(&e);
            if !outcome
                .check
                .file_errors
                .iter()
                .any(|error| error.message == message)
            {
                outcome.check.file_errors.push(ModelFileError {
                    file: None,
                    message,
                });
            }
        }

        outcome.check.imports.push((
            scope.import.clone().unwrap_or_else(|| scope.describe()),
            program_info.program.functions.len(),
        ));
        imports_checked += 1;
        // `program_info` and `match_index` drop here -- the same streaming posture
        // `cli::index` keeps, which the owned iterator items exist to preserve.
    }

    // Bridge verdicts, once, over every import: "unmatched" means not matched anywhere. With no
    // import at all there is nothing to diagnose, and saying "matched nothing" would be a lie
    // about a pass that never ran.
    if imports_checked > 0 {
        for (i, spec) in bridges.iter().enumerate() {
            let Some((severity, message)) = diagnose(spec, matches.bridges.get(i)) else {
                continue;
            };
            // `Severity::Ignore` is silence, exactly as it is at index time; see
            // `codegen::model_matches::classify`.
            if severity == Severity::Ignore {
                continue;
            }
            outcome.check.bridges.push(BridgeDiagnosis {
                file: spec.source.clone(),
                index: spec.index,
                error: severity == Severity::Error,
                message,
            });
        }
    }

    for file in files {
        file.finish(imports_checked > 0, &mut outcome);
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Per-file accumulation
// ---------------------------------------------------------------------------

/// One model file, accumulating across imports.
struct FileState {
    path: PathBuf,
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
    /// The `in` clause as the file spells it.
    scope_text: Option<String>,
    scope: ProgramScope,
    /// A generator whose `in` is malformed is admitted by nothing, exactly as the matcher
    /// treats it.
    scope_malformed: bool,
    /// Whether any checked import's scope admits it.
    applicable: bool,
    declares_endpoints: bool,
    declares_propagation: bool,
    declares_access_paths: bool,
    /// A `modes` directive. Counts as declaring a model on its own: a generator carrying
    /// `modes: ["skip-analysis"]` and nothing else changes the analysis (it removes what a body
    /// contributes), so a check that ignored it would report nothing about the one generator
    /// whose failure to match is invisible everywhere else.
    declares_modes: bool,
    has_bridge: bool,
    matched: Option<MatchedFunctions>,
    endpoint_stats: BTreeMap<TaintDirection, EndpointStats>,
    propagation_stats: PropagationStats,
}

impl FileState {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
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
    fn take_inventory(&mut self) -> bool {
        let path = self.path.clone();
        let outcome = spec::visit_model_file(&path, |n, value| {
            // The errors are dropped: the matching pass parses the same clause and reports
            // them, once, as the model errors they are. What is wanted here is only whether
            // the clause can admit anything at all.
            let mut scope_errors = Vec::new();
            let scope = ProgramScope::parse(value.get("in"), n, &mut scope_errors);
            let state = GeneratorState {
                index: n,
                find: value
                    .get("find")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                scope_text: value.get("in").map(render_scope),
                scope,
                scope_malformed: !scope_errors.is_empty(),
                applicable: false,
                declares_endpoints: has_entries(value, "sources") || has_entries(value, "sinks"),
                declares_propagation: has_entries(value, "propagation"),
                declares_access_paths: has_entries(value, "access_paths"),
                declares_modes: has_entries(value, "modes"),
                has_bridge: value.pointer("/model/bridge").is_some(),
                matched: None,
                endpoint_stats: BTreeMap::new(),
                propagation_stats: PropagationStats::default(),
            };
            self.positions.insert(n, self.generators.len());
            self.generators.push(state);
            Ok(())
        });
        match outcome {
            Ok(()) => true,
            Err(e) => {
                self.record_error(render_chain(&e));
                false
            }
        }
    }

    /// Records whether this import's scope admits each generator.
    fn note_applicable(&mut self, scope: &ImportScope) {
        for generator in &mut self.generators {
            generator.applicable |= !generator.scope_malformed && generator.scope.admits(scope);
        }
    }

    /// Folds in one import's load of this file.
    fn absorb(&mut self, load: models::ModelLoadReport, errors: Vec<ModelCheckError>) {
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
                    Some(existing) => existing.merge(matched, SAMPLE_CAP),
                    slot @ None => *slot = Some(matched.clone()),
                }
            }
        }
        for (index, stats) in &load.propagation_stats {
            if let Some(generator) = self.generator_mut(*index) {
                generator.propagation_stats.merge(stats);
            }
        }
    }

    fn generator_mut(&mut self, index: usize) -> Option<&mut GeneratorState> {
        let position = *self.positions.get(&index)?;
        self.generators.get_mut(position)
    }

    /// Folds this file's findings into the run's, in the categories the SARIF writer reports.
    fn finish(self, matched_against_a_program: bool, outcome: &mut ModelCheckOutcome) {
        for message in self.errors {
            outcome.check.file_errors.push(ModelFileError {
                file: Some(self.path.clone()),
                message,
            });
        }
        for generator in self.generators {
            // Re-keyed by file here, and only here: `ModelLoadReport` is keyed by (generator
            // index, direction) alone, which would conflate two model files that number their
            // generators the same.
            for (direction, stats) in &generator.endpoint_stats {
                outcome
                    .endpoint_stats
                    .entry((self.path.clone(), generator.index, *direction))
                    .or_default()
                    .merge(stats);
            }
            if !matched_against_a_program {
                continue;
            }
            let declares_a_model = generator.declares_endpoints
                || generator.declares_propagation
                || generator.declares_access_paths
                || generator.declares_modes
                || generator.has_bridge;
            // Reported only for a generator that declares a model: a scoped-out generator
            // declaring nothing is nothing to act on.
            if !generator.applicable {
                if declares_a_model {
                    outcome.check.scope_excluded.push(ScopeExcluded {
                        file: self.path.clone(),
                        index: generator.index,
                        scope: generator.scope_text.clone(),
                    });
                }
                continue;
            }
            // A propagation that emitted no row is dead exactly as an endpoint that produced no
            // row is; the phase that would have consumed it is `ctadl index`.
            if generator.declares_propagation && generator.propagation_stats.rows == 0 {
                outcome.check.index_time_dead.push(IndexTimeDead {
                    file: self.path.clone(),
                    index: generator.index,
                    kind: "propagation".to_string(),
                });
            }
            // A `modes` directive that matched no function. Reported off the match set rather
            // than an emitted-row count, because the directive emits no rows at all: what it
            // does is take rows *away*. `MatchedFunctions::All` has no count and is never dead.
            if generator.declares_modes
                && generator.matched.as_ref().and_then(MatchedFunctions::total) == Some(0)
            {
                outcome.check.index_time_dead.push(IndexTimeDead {
                    file: self.path.clone(),
                    index: generator.index,
                    kind: "modes directive".to_string(),
                });
            }
            // What the `where` selected, for the generators that selected something. A zero is
            // not reported here: that is what `CTADL0004` and the bridge verdict are for, and
            // saying it twice in two vocabularies is worse than saying it once.
            let Some(matched) = &generator.matched else {
                continue;
            };
            if !declares_a_model || matched.total() == Some(0) {
                continue;
            }
            outcome.check.matched.push(GeneratorMatch {
                file: self.path.clone(),
                index: generator.index,
                find: generator.find.clone(),
                total: matched.total(),
                sample: matched.names().iter().cloned().collect(),
            });
        }
    }
}

/// Whether a `model` key holds at least one entry.
fn has_entries(value: &serde_json::Value, key: &str) -> bool {
    value
        .pointer(&format!("/model/{key}"))
        .and_then(|v| v.as_array())
        .is_some_and(|items| !items.is_empty())
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
