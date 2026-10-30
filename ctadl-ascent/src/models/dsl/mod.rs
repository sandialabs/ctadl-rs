/*! The model-matching DSL: a small Datalog with an execution engine.

A model file is a list of rules. Each derives one or more *output* atoms — `source`, `sink`,
`propagation`, `bridge`, `access_paths`, `dispatch` — from a conjunction of atoms over the
*input* relations the analyzer already has: `fun`, `callsig`, `param`, `callsite`, `subclass`,
`uses_field`.

```text
// java.net.URL.openConnection: the receiver flows to the return value, which is a source
source(F::return), propagation(F::return <- F::arg(0)) :-
  fun(F, name = "openConnection", parent = "Ljava/net/URL;");
```

There is no recursion, so no stratification is needed and the fixpoint is one pass.

# The pieces

| module | job |
| --- | --- |
| [`ast`] | the abstract syntax; program-independent |
| [`parse`] | PEG → AST, and the argument-position checks the grammar cannot make |
| [`check`] | relation shapes, **modes**, and the execution plan |
| [`relations`] | the input relations, materialized over one program |
| [`eval`] | the engine |
| [`emit`] | groundings → [`ProgramModelMatches`] |
| [`migrate`] | the JSON model-generator format, re-expressed in this one |

# Two phases, and the streaming posture

[`DslMatcher`] is built before the import loop, fed one
program at a time, and finished after. Nearly every rule is emitted during the import that
satisfies it. What is retained between imports is only the *bindings* of bridging rules, plus
the head keys already emitted — never a program's tables, never its IR.

Finishing after the loop is what makes a bridging rule expressible at all. Such a rule names two
programs in one body, and no single import satisfies it; the body's connected components are
accumulated separately and joined at the end. See [`eval`].

The phase is fixed when the matcher is built. `index` keeps the propagation / bridge /
access-path / dispatch / mode heads and `query` keeps the source / sink ones, and a rule with no head
for the phase is not evaluated. Each says how many rules contributed nothing to it, rather than
dropping them in silence; a rule contributing at least one head to the running phase is never
counted.
*/

use std::path::{Path as FsPath, PathBuf};

use crate::error::{Error, ErrorContext};
use crate::models::ProgramModelMatches;
use crate::models::match_index::ProgramMatchIndex;

pub mod ast;
pub mod check;
pub mod emit;
pub mod eval;
pub mod migrate;
pub mod parse;
pub mod relations;

#[cfg(test)]
mod tests;

pub use ast::{Program, Rule, Span};
pub use emit::{Phase, RuleStats};

/// The file extensions a `--models` path uses to select this loader.
///
/// `.ctadl` is the name to write; `.dl` is accepted because it is what a Datalog file is
/// conventionally called and a model author reaching for it should not have to find out the
/// hard way.
pub const DSL_EXTENSIONS: &[&str] = &["ctadl", "dl"];

/// Whether `path` names a DSL model file.
pub fn is_dsl_path(path: &FsPath) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DSL_EXTENSIONS.contains(&e))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// One thing wrong with a model file.
#[derive(Clone, Debug)]
pub enum DslError {
    /// The text does not parse. Reported alone: there is no resynchronization point, so
    /// everything after it is unknown rather than wrong.
    Syntax { message: String, span: Span },
    /// A rule parsed but does not mean anything: an unknown relation, an unbound variable in a
    /// head, an operator whose variables are not bound yet.
    Rule { message: String, span: Span },
}

impl DslError {
    pub fn span(&self) -> Span {
        match self {
            DslError::Syntax { span, .. } | DslError::Rule { span, .. } => *span,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            DslError::Syntax { message, .. } | DslError::Rule { message, .. } => message,
        }
    }
}

/// Everything wrong with one model file, in file order.
#[derive(Clone, Debug, Default)]
pub struct DslErrors {
    errors: Vec<DslError>,
}

impl DslErrors {
    pub fn push(&mut self, e: DslError) {
        self.errors.push(e);
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &DslError> {
        self.errors.iter()
    }

    pub fn extend(&mut self, other: DslErrors) {
        self.errors.extend(other.errors);
    }

    /// [`Self::extend`], dropping an error already held: same message at the same span.
    pub fn extend_unique(&mut self, other: DslErrors) {
        for e in other.errors {
            if !self
                .errors
                .iter()
                .any(|held| held.span() == e.span() && held.message() == e.message())
            {
                self.errors.push(e);
            }
        }
    }

    /// Renders every error against the file it came from, one per line, with a `path:line:col`
    /// prefix an editor can jump to.
    pub fn render(&self, path: &FsPath, source: &str) -> String {
        self.render_with(path, |span| {
            let (line, col) = span.line_col(source);
            format!("{}:{line}:{col}", path.display())
        })
    }

    /// [`Self::render`], with each error's location spelled by `locate`.
    fn render_with(&self, path: &FsPath, locate: impl Fn(Span) -> String) -> String {
        let mut out = String::new();
        if self.errors.len() > 1 {
            out.push_str(&format!(
                "found {} problem(s) in {}\n",
                self.errors.len(),
                path.display()
            ));
        }
        for e in &self.errors {
            out.push_str(&format!("> {}: {}\n", locate(e.span()), e.message()));
        }
        out
    }

    /// The crate error a load returns.
    pub fn into_error(self, path: &FsPath, source: &str) -> Error {
        Error::DslModel(self.render(path, source))
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// One parsed, checked model file.
pub struct DslFile {
    pub path: PathBuf,
    /// Retained so a diagnostic can name a line and column after loading.
    pub source: String,
    pub program: Program,
    /// Parallel to `program.rules`.
    pub plans: Vec<check::Plan>,
    /// Set when `path` is a JSON model file and `source` is its migration.
    pub origin: Option<JsonOrigin>,
}

/// What a migrated JSON model file keeps of the JSON, so that its diagnostics name what its
/// author wrote. The rules themselves are synthetic: a line and column in them points at
/// nothing the author can open.
#[derive(Clone, Debug)]
pub struct JsonOrigin {
    /// Parallel to `program.rules`: the generator each rule was migrated from. A generator with
    /// a disjunction in its `where` is several rules, and one that derives nothing is none.
    pub rule_generators: Vec<usize>,
}

impl DslFile {
    /// Parses and checks `text`. `path` is used only in diagnostics.
    pub fn from_text(path: impl Into<PathBuf>, text: impl Into<String>) -> Result<Self, Error> {
        Self::from_parts(path.into(), text.into(), None)
    }

    fn from_parts(
        path: PathBuf,
        source: String,
        origin: Option<JsonOrigin>,
    ) -> Result<Self, Error> {
        let program = match parse::parse_program(&source) {
            Ok(program) => program,
            // A syntax error in a migration has no rule to be attributed to. It is the
            // migrator's bug, so it is reported against the text the migrator wrote.
            Err(e) if origin.is_some() => {
                return Err(Error::DslModel(format!(
                    "{}{}",
                    e.render(&path, &source),
                    migrated_note(&path)
                )));
            }
            Err(e) => return Err(e.into_error(&path, &source)),
        };
        let mut errors = DslErrors::default();
        let mut plans = Vec::with_capacity(program.rules.len());
        for rule in &program.rules {
            match check::plan_rule(rule, &mut errors) {
                Some(plan) => plans.push(plan),
                None => plans.push(check::Plan {
                    components: Vec::new(),
                    var_types: Default::default(),
                }),
            }
        }
        let file = DslFile {
            path,
            source,
            program,
            plans,
            origin,
        };
        if !errors.is_empty() {
            return Err(file.error(errors));
        }
        Ok(file)
    }

    /// Reads a model file: a DSL one as written, a JSON / JSON5 / JSONL one through
    /// [`Self::read_json`]. A path [`is_dsl_path`] does not claim is JSON, as it always was.
    pub fn read(path: impl AsRef<FsPath>) -> Result<Self, Error> {
        let path = path.as_ref();
        if !is_dsl_path(path) {
            return Self::read_json(path);
        }
        let text = std::fs::read_to_string(path)
            .err_context(|| format!("reading model DSL file: {}", path.display()))?;
        Self::from_text(path.to_path_buf(), text)
    }

    /// Migrates a JSON model file and loads the migration, so that it runs on the one engine.
    ///
    /// Warns that the format is deprecated, once per file per process, and logs what the
    /// migration left out. A generator that does not translate exactly fails the load with
    /// the migrator's errors, each naming its generator.
    pub fn read_json(path: &FsPath) -> Result<Self, Error> {
        warn_json_deprecated(path);
        let (text, report) = migrate::migrate_file(path)
            .err_context(|| format!("migrating model file: {}", path.display()))?;
        Self::from_migration(path, text, report)
    }

    /// Migrates `generators`, a JSON model file's generators in file order, and loads the
    /// migration: [`Self::read_json`] for a caller that already has them. `path` names the file
    /// in diagnostics.
    pub fn from_json_generators(
        path: impl Into<PathBuf>,
        generators: &[serde_json::Value],
    ) -> Result<Self, Error> {
        let path = path.into();
        let (text, report) = migrate::migrate_generators(generators, None)
            .err_context(|| format!("migrating model file: {}", path.display()))?;
        Self::from_migration(&path, text, report)
    }

    /// [`Self::read_json`] for a linter: a generator that does not migrate is left out rather
    /// than costing the rest of the file. The second half is the error [`Self::read_json`]
    /// would have failed with, which names every generator left out.
    pub fn read_json_lenient(path: &FsPath) -> Result<(Self, Option<Error>), Error> {
        warn_json_deprecated(path);
        let context = || format!("migrating model file: {}", path.display());
        let mut generators = Vec::new();
        crate::models::spec::visit_model_file(path, |_, value| {
            generators.push(value.clone());
            Ok(())
        })
        .err_context(context)?;
        let mut rejected = None;
        let (text, report) = match migrate::migrate_generators(generators.iter(), None) {
            Err(Error::JsonModel(errors)) => {
                // A generator with no `model` derives nothing, so standing one in for each that
                // failed keeps every other generator at its own index.
                for e in errors.iter() {
                    generators[e.index()] = serde_json::json!({ "find": "methods" });
                }
                rejected = Some(
                    Err::<(), _>(Error::JsonModel(errors))
                        .err_context(context)
                        .unwrap_err(),
                );
                migrate::migrate_generators(generators.iter(), None).err_context(context)?
            }
            migrated => migrated.err_context(context)?,
        };
        Ok((Self::from_migration(path, text, report)?, rejected))
    }

    fn from_migration(
        path: &FsPath,
        text: String,
        report: migrate::MigrationReport,
    ) -> Result<Self, Error> {
        for warning in &report.warnings {
            log::warn!("{}: {warning}", path.display());
        }
        let origin = JsonOrigin {
            rule_generators: report.rule_generators,
        };
        Self::from_parts(path.to_path_buf(), text, Some(origin))
    }

    /// The index diagnostics name rule `rule` by: its position in a DSL file, its generator's
    /// in a migrated JSON one. The two play the same part, so every surface that reports one —
    /// provenance, `CTADL0004`, the dead-rule warning — reads the same whichever was given.
    pub fn label(&self, rule: usize) -> usize {
        label(self.origin.as_ref(), rule)
    }

    /// `path:index`, the provenance every diagnostic carries, with the index of
    /// [`Self::label`]. For a JSON file it is `file:generator-index`.
    pub fn provenance(&self, rule: usize) -> String {
        format!("{}:{}", self.path.display(), self.label(rule))
    }

    /// `errors`, rendered against this file. A migrated file's errors name the generator their
    /// rule came from, rather than a line of text its author has never seen.
    pub fn error(&self, errors: DslErrors) -> Error {
        let Some(origin) = &self.origin else {
            return errors.into_error(&self.path, &self.source);
        };
        let rendered = errors.render_with(&self.path, |span| {
            let rule = self
                .program
                .rules
                .iter()
                .position(|r| r.span.start <= span.start && span.start <= r.span.end);
            match rule {
                Some(rule) => format!(
                    "{}: generator {}",
                    self.path.display(),
                    label(Some(origin), rule)
                ),
                None => {
                    let (line, col) = span.line_col(&self.source);
                    format!("{} (migrated, {line}:{col})", self.path.display())
                }
            }
        });
        Error::DslModel(format!("{rendered}{}", migrated_note(&self.path)))
    }
}

fn label(origin: Option<&JsonOrigin>, rule: usize) -> usize {
    origin
        .and_then(|o| o.rule_generators.get(rule).copied())
        .unwrap_or(rule)
}

/// What follows a migrated file's errors: where to read the rules they are about.
fn migrated_note(path: &FsPath) -> String {
    format!(
        "  (in the DSL translation of {0}; `ctadl migrate-models {0} -o -` prints it)\n",
        path.display()
    )
}

/// The warning a JSON model file draws.
pub fn json_deprecation_warning(path: &FsPath) -> String {
    format!(
        "{0}: JSON model files are deprecated and will be removed in a future release; convert \
         with 'ctadl migrate-models {0}'",
        path.display()
    )
}

/// Logs [`json_deprecation_warning`] for `path` unless this process already has, and returns
/// whether it did.
///
/// Once per process rather than once per load: `ctadl report` loads each file once per import,
/// and saying the same thing about the same file again tells the reader nothing.
pub fn warn_json_deprecated(path: &FsPath) -> bool {
    static WARNED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if warned.iter().any(|p| p == path) {
        return false;
    }
    warned.push(path.to_path_buf());
    log::warn!("{}", json_deprecation_warning(path));
    true
}

/// Every DSL file a run was given, parsed once before the import loop.
#[derive(Default)]
pub struct DslModelSet {
    pub files: Vec<DslFile>,
}

impl DslModelSet {
    /// Parses and checks every file in `paths`, migrating the JSON ones; see [`DslFile::read`].
    pub fn scan(paths: &[PathBuf]) -> Result<Self, Error> {
        let mut files = Vec::new();
        for path in paths {
            files.push(DslFile::read(path)?);
        }
        if !files.is_empty() {
            log::debug!(
                "loaded {} model DSL file(s), {} rule(s)",
                files.len(),
                files.iter().map(|f| f.program.rules.len()).sum::<usize>()
            );
        }
        Ok(Self { files })
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether any rule in the set reads `callsig`.
    ///
    /// The `--models` half of the gate [`super::default_models_find_dispatch`] opens for the
    /// defaults: the signature keys `callsig` ranges over cost a pass over every statement to
    /// collect, so an import pays for them only when some rule asks.
    pub fn uses_callsig(&self) -> bool {
        self.files.iter().any(|f| f.program.uses_callsig())
    }

    /// The files with a rule that has a `source` or `sink` head, whether or not it matches.
    /// What the endpoint digest an index records is taken over, so a property of the files
    /// alone: `ctadl index` and `ctadl query` must agree on it without matching anything.
    pub fn endpoint_files(&self) -> impl Iterator<Item = &PathBuf> {
        self.files
            .iter()
            .filter(|f| {
                f.program.rules.iter().any(|r| {
                    r.heads.iter().any(|h| {
                        matches!(
                            h.kind,
                            ast::HeadKind::Source { .. } | ast::HeadKind::Sink { .. }
                        )
                    })
                })
            })
            .map(|f| &f.path)
    }

    /// The input relations some rule in the set reads, so an import builds only those.
    pub fn needs(&self) -> relations::Needs {
        Self::needs_of(&self.files)
    }

    fn needs_of(files: &[DslFile]) -> relations::Needs {
        let mut needs = relations::Needs::default();
        for file in files {
            needs.union(relations::Needs::of_program(&file.program));
        }
        needs
    }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Matches a model set across the import loop, for one phase.
///
/// One per run, not one per import: see the module docs for why a bridging rule's join cannot
/// happen while a single program is in hand. The lifecycle is [`Self::new`] before the loop,
/// [`Self::observe_import`] (or [`Self::observe_facts`]) once per import, [`Self::finish`]
/// after.
///
/// What it holds between imports is no more than that join needs:
///
/// - a rule the phase keeps no head of is never evaluated;
/// - a rule whose body is one component is emitted into `out` *during* the import that
///   satisfied it and its solutions dropped, since its solution set is the union over imports
///   anyway. Only the head keys it emitted are kept, to drop a row a later import would repeat;
/// - only a bridging rule — two or more components — keeps its solutions until `finish`.
pub struct DslMatcher<'a> {
    files: &'a [DslFile],
    phase: Phase,
    /// `[file][rule]`. `None` for a rule the phase skips.
    rules: Vec<Vec<Option<RuleState>>>,
    /// Per file, what emission reported so far. Held to `finish`, which is where a load has
    /// always reported them, and deduplicated, since a per-import rule says the same thing about
    /// every import.
    errors: Vec<DslErrors>,
}

/// One rule the phase keeps.
struct RuleState {
    /// For a per-import rule, this import's solutions, cleared after each; for a bridging rule,
    /// the union so far.
    solutions: eval::RuleSolutions,
    emitter: emit::RuleEmitter,
    per_import: bool,
}

impl<'a> DslMatcher<'a> {
    pub fn new(set: &'a DslModelSet, phase: Phase) -> Self {
        let rules = set
            .files
            .iter()
            .map(|file| {
                file.program
                    .rules
                    .iter()
                    .zip(&file.plans)
                    .map(|(rule, plan)| {
                        let per_import = plan.components.len() <= 1;
                        let emitter = emit::RuleEmitter::new(rule, plan, phase, per_import);
                        emitter.is_live().then(|| RuleState {
                            solutions: eval::RuleSolutions::for_plan(plan),
                            emitter,
                            per_import,
                        })
                    })
                    .collect()
            })
            .collect();
        Self {
            files: &set.files,
            phase,
            rules,
            errors: vec![DslErrors::default(); set.files.len()],
        }
    }

    /// Whether no rule is live for the phase, so an import need not build relations for it.
    pub fn is_empty(&self) -> bool {
        self.rules.iter().flatten().all(Option::is_none)
    }

    /// The relations the set's rules read; see [`DslModelSet::needs`].
    pub fn needs(&self) -> relations::Needs {
        DslModelSet::needs_of(self.files)
    }

    /// Builds this import's relations and observes them. A caller running several matchers
    /// over one import builds the relations once instead, with the union of their
    /// [`Self::needs`], and calls [`Self::observe_facts`] on each.
    pub fn observe_import(&mut self, index: &ProgramMatchIndex<'_>, out: &mut ProgramModelMatches) {
        if self.is_empty() {
            return;
        }
        let facts = program_facts(index, &self.needs());
        self.observe_facts(&facts, out);
    }

    /// Evaluates every live rule against one program. A per-import rule's heads go into `out`
    /// now; a bridging rule's solutions are folded in for [`Self::finish`].
    ///
    /// Emitting into the caller's accumulator rather than a buffer of its own is what keeps
    /// the posture streaming: a buffer would hold every row until the end, which is what this
    /// exists not to do. `out` must be the same accumulator on every call and at `finish`.
    pub fn observe_facts(
        &mut self,
        facts: &relations::ProgramFacts,
        out: &mut ProgramModelMatches,
    ) {
        for (fi, file) in self.files.iter().enumerate() {
            let mut errors = DslErrors::default();
            for (ri, rule) in file.program.rules.iter().enumerate() {
                let Some(state) = &mut self.rules[fi][ri] else {
                    continue;
                };
                eval::evaluate_rule(rule, &file.plans[ri], facts, &mut state.solutions);
                if state.per_import {
                    state.emitter.emit(
                        rule,
                        &state.solutions,
                        &file.provenance(ri),
                        out,
                        &mut errors,
                    );
                    state.solutions.clear();
                }
            }
            self.errors[fi].extend_unique(errors);
        }
    }

    /// Instantiates the bridging rules' heads into `out`, and reports on every rule. Fails on
    /// the first file emission found a problem in.
    pub fn finish(self, out: &mut ProgramModelMatches) -> Result<DslReport, Error> {
        let mut report = DslReport::default();
        for file in self.finish_each(out) {
            report.files.push(file?);
        }
        Ok(report)
    }

    /// [`Self::finish`], reporting each file separately, for a caller that attributes a
    /// problem to the file it is in rather than failing the run.
    pub fn finish_each(self, out: &mut ProgramModelMatches) -> Vec<Result<DslFileReport, Error>> {
        let Self {
            files,
            phase,
            rules,
            errors,
        } = self;
        files
            .iter()
            .zip(rules)
            .zip(errors)
            .map(|((file, rules), mut errors)| {
                let mut file_report = DslFileReport {
                    path: file.path.clone(),
                    phase,
                    rules: Vec::with_capacity(rules.len()),
                    labels: (0..rules.len()).map(|ri| file.label(ri)).collect(),
                    skipped_rules: 0,
                };
                let mut emitted = DslErrors::default();
                for (ri, state) in rules.into_iter().enumerate() {
                    let Some(mut state) = state else {
                        file_report.rules.push(RuleStats::default());
                        continue;
                    };
                    if !state.per_import {
                        state.emitter.emit(
                            &file.program.rules[ri],
                            &state.solutions,
                            &file.provenance(ri),
                            out,
                            &mut emitted,
                        );
                    }
                    file_report.rules.push(state.emitter.finish());
                }
                errors.extend_unique(emitted);
                if !errors.is_empty() {
                    return Err(file.error(errors));
                }
                file_report.skipped_rules = file_report
                    .by_label()
                    .filter(|(_, rules)| rules.iter().all(|r| r.live_heads == 0))
                    .count();
                Ok(file_report)
            })
            .collect()
    }
}

/// Materializes the relations `needs` names for the program `index` was built from.
pub fn program_facts(
    index: &ProgramMatchIndex<'_>,
    needs: &relations::Needs,
) -> relations::ProgramFacts {
    let facts = relations::ProgramFacts::build(index, needs);
    log::trace!(
        "DSL relations for {}: {} function(s), {} call site(s), {} field use(s)",
        index.scope.describe(),
        facts.funs.len(),
        facts.callsites.len(),
        facts.uses_field.len()
    );
    facts
}

/// Loads one model file against one program, in one shot; a JSON one is migrated first, see
/// [`DslFile::read`].
///
/// The shape a caller with a single program in hand wants — a test, `ctadl query` after the
/// import loop, the flowy codegen path. A run with several imports uses [`DslMatcher`] directly
/// so a rule spanning two of them can still fire.
pub fn try_load_dsl_models(
    index: &ProgramMatchIndex<'_>,
    path: impl AsRef<FsPath>,
    phase: Phase,
    out: &mut ProgramModelMatches,
) -> Result<DslReport, Error> {
    let file = DslFile::read(path)?;
    let set = DslModelSet { files: vec![file] };
    let mut matcher = DslMatcher::new(&set, phase);
    matcher.observe_import(index, out);
    matcher.finish(out)
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// What a whole load did, per file.
#[derive(Clone, Debug, Default)]
pub struct DslReport {
    pub files: Vec<DslFileReport>,
}

impl DslReport {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Rules that contributed no head to the running phase, across every file.
    pub fn skipped_rules(&self) -> usize {
        self.files.iter().map(|f| f.skipped_rules).sum()
    }

    /// Rules that contributed a head to the phase but matched nothing. These are what a model
    /// author is usually looking for: a rule that is live for this phase and silently inert.
    ///
    /// A rule the phase skipped entirely is *not* here. It is reported by
    /// [`Self::phase_warning`], and saying it "matched nothing" as well would name the wrong
    /// problem: it was never run.
    pub fn dead_rules(&self) -> Vec<String> {
        let mut out = Vec::new();
        for file in &self.files {
            for (label, rules) in file.by_label() {
                let mut live = rules.iter().filter(|r| r.live_heads > 0).peekable();
                if live.peek().is_some() && live.all(|r| r.total_rows() == 0) {
                    out.push(format!("{}:{label}", file.path.display()));
                }
            }
        }
        out
    }

    /// The one-line summary each phase logs. `None` when there is nothing to say.
    pub fn phase_warning(&self) -> Option<String> {
        let skipped = self.skipped_rules();
        if skipped == 0 {
            return None;
        }
        let phase = self.files.first().map(|f| f.phase)?;
        let (running, other) = match phase {
            Phase::Index => ("index", "source/sink"),
            Phase::Query => ("query", "propagation/bridge/access_paths/dispatch/mode"),
            Phase::All => return None,
        };
        Some(format!(
            "ctadl {running} is ignoring {skipped} model rule(s) that declare only {other} \
             heads; they take effect in the other phase"
        ))
    }

    /// Total rows emitted, by kind.
    pub fn totals(&self) -> RuleStats {
        let mut total = RuleStats::default();
        for file in &self.files {
            for stats in &file.rules {
                total.merge(stats);
            }
        }
        total
    }
}

/// One file's contribution.
#[derive(Clone, Debug)]
pub struct DslFileReport {
    pub path: PathBuf,
    pub phase: Phase,
    /// Parallel to the file's rules.
    pub rules: Vec<RuleStats>,
    /// Parallel to `rules`: what diagnostics call each, [`DslFile::label`]. The rule's own index
    /// in a DSL file, its generator's in a migrated JSON one.
    pub labels: Vec<usize>,
    /// Rules whose heads all belong to the other phase. Counted by label, so a JSON generator
    /// migrated to several rules counts once.
    pub skipped_rules: usize,
}

impl DslFileReport {
    /// The rules' stats grouped by label, in label order.
    pub fn by_label(&self) -> impl Iterator<Item = (usize, Vec<&RuleStats>)> + '_ {
        let mut groups: std::collections::BTreeMap<usize, Vec<&RuleStats>> = Default::default();
        for (label, stats) in self.labels.iter().zip(&self.rules) {
            groups.entry(*label).or_default().push(stats);
        }
        groups.into_iter()
    }
}
