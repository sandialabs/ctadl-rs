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

[`DslMatcher`] is used the way the JSON matcher is: built before the import loop, fed one
program at a time, and finished after. Nearly every rule is emitted during the import that
satisfies it. What is retained between imports is only the *bindings* of bridging rules, plus
the head keys already emitted — never a program's tables, never its IR.

Finishing after the loop is what makes a bridging rule expressible at all. Such a rule names two
programs in one body, and no single import satisfies it; the body's connected components are
accumulated separately and joined at the end. See [`eval`].

The phase is fixed when the matcher is built. `index` keeps the propagation / bridge /
access-path / dispatch / skip-analysis heads and `query` keeps the source / sink ones, and a rule with no head
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
        let mut out = String::new();
        if self.errors.len() > 1 {
            out.push_str(&format!(
                "found {} problem(s) in {}\n",
                self.errors.len(),
                path.display()
            ));
        }
        for e in &self.errors {
            let (line, col) = e.span().line_col(source);
            out.push_str(&format!(
                "> {}:{line}:{col}: {}\n",
                path.display(),
                e.message()
            ));
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
}

impl DslFile {
    /// Parses and checks `text`. `path` is used only in diagnostics.
    pub fn from_text(path: impl Into<PathBuf>, text: impl Into<String>) -> Result<Self, Error> {
        let path = path.into();
        let source = text.into();
        let program = parse::parse_program(&source).map_err(|e| e.into_error(&path, &source))?;
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
        if !errors.is_empty() {
            return Err(errors.into_error(&path, &source));
        }
        Ok(DslFile {
            path,
            source,
            program,
            plans,
        })
    }

    pub fn read(path: impl AsRef<FsPath>) -> Result<Self, Error> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .err_context(|| format!("reading model DSL file: {}", path.display()))?;
        Self::from_text(path.to_path_buf(), text)
    }

    /// `path:rule-index`, the provenance every diagnostic carries. The index counts *rules*,
    /// which is what a DSL file's counterpart of a JSON generator index is.
    pub fn provenance(&self, rule: usize) -> String {
        format!("{}:{rule}", self.path.display())
    }
}

/// Every DSL file a run was given, parsed once before the import loop.
#[derive(Default)]
pub struct DslModelSet {
    pub files: Vec<DslFile>,
}

impl DslModelSet {
    /// Parses the DSL files among `paths`, leaving the rest to the JSON loader.
    pub fn scan(paths: &[PathBuf]) -> Result<Self, Error> {
        let mut files = Vec::new();
        for path in paths {
            if is_dsl_path(path) {
                files.push(DslFile::read(path)?);
            }
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
    /// The DSL's half of the gate [`ModelFileSpecs::finds_dispatch`](super::spec::ModelFileSpecs)
    /// opens: the signature keys `callsig` ranges over cost a pass over every statement to
    /// collect, so an import pays for them only when some rule asks.
    pub fn uses_callsig(&self) -> bool {
        self.files.iter().any(|f| f.program.uses_callsig())
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
                    skipped_rules: 0,
                };
                let mut emitted = DslErrors::default();
                for (ri, state) in rules.into_iter().enumerate() {
                    let Some(mut state) = state else {
                        file_report.skipped_rules += 1;
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
                    return Err(errors.into_error(&file.path, &file.source));
                }
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

/// Loads one DSL file against one program, in one shot.
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
            for (i, stats) in file.rules.iter().enumerate() {
                if stats.live_heads > 0 && stats.total_rows() == 0 {
                    out.push(format!("{}:{i}", file.path.display()));
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
            Phase::Query => (
                "query",
                "propagation/bridge/access_paths/dispatch/skip_analysis",
            ),
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
    /// Rules whose heads all belong to the other phase.
    pub skipped_rules: usize,
}
