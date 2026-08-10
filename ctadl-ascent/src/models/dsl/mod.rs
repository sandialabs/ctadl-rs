/*! The model-matching DSL: a small Datalog with an execution engine.

A model file is a list of rules. Each derives one or more *output* atoms — `source`, `sink`,
`propagation`, `bridge`, `access_paths` — from a conjunction of atoms over the *input* relations
the analyzer already has: `fun`, `param`, `callsite`, `subclass`, `uses_field`.

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
| [`parse`] | pest → AST, and the argument-position checks the grammar cannot make |
| [`check`] | relation shapes, **modes**, and the execution plan |
| [`relations`] | the input relations, materialized over one program |
| [`eval`] | the engine |
| [`emit`] | groundings → [`ProgramModelMatches`] |
| [`migrate`] | the JSON model-generator format, re-expressed in this one |

# Two phases, and the streaming posture

[`DslMatcher`] is used the way the JSON matcher is: built before the import loop, fed one
program at a time, and finished after. What it retains between imports is only the *bindings*
its rules produced — never a program's tables, never its IR.

Finishing after the loop rather than per import is what makes a bridging rule expressible at
all. Such a rule names two programs in one body, and no single import satisfies it; the body's
connected components are accumulated separately and joined at the end. See [`eval`].

`index` keeps the propagation / bridge / access-path heads and `query` keeps the source / sink
ones. Each says how many rules contributed nothing to it, rather than dropping them in silence;
a rule contributing at least one head to the running phase is never counted.
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
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Accumulates rule solutions across the import loop.
///
/// One per run, not one per import: see the module docs for why the join cannot happen while a
/// single program is in hand.
pub struct DslMatcher<'a> {
    files: &'a [DslFile],
    /// `[file][rule]`.
    solutions: Vec<Vec<eval::RuleSolutions>>,
    /// How many imports were observed, so `finish` can say "nothing was matched against" rather
    /// than "nothing matched".
    imports: usize,
}

impl<'a> DslMatcher<'a> {
    pub fn new(set: &'a DslModelSet) -> Self {
        let solutions = set
            .files
            .iter()
            .map(|f| f.plans.iter().map(eval::RuleSolutions::for_plan).collect())
            .collect();
        Self {
            files: &set.files,
            solutions,
            imports: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Evaluates every rule against one program, folding the bindings in.
    pub fn observe_import(&mut self, index: &ProgramMatchIndex<'_>) {
        if self.files.is_empty() {
            return;
        }
        self.imports += 1;
        let facts = relations::ProgramFacts::build(index);
        log::trace!(
            "DSL relations for {}: {} function(s), {} call site(s), {} field use(s)",
            index.scope.describe(),
            facts.funs.len(),
            facts.callsites.len(),
            facts.uses_field.len()
        );
        for (fi, file) in self.files.iter().enumerate() {
            for (ri, rule) in file.program.rules.iter().enumerate() {
                eval::evaluate_rule(rule, &file.plans[ri], &facts, &mut self.solutions[fi][ri]);
            }
        }
    }

    /// Instantiates every rule's heads for `phase` into `out`.
    pub fn finish(self, phase: Phase, out: &mut ProgramModelMatches) -> Result<DslReport, Error> {
        let mut report = DslReport::default();
        for (fi, file) in self.files.iter().enumerate() {
            let mut errors = DslErrors::default();
            let mut file_report = DslFileReport {
                path: file.path.clone(),
                phase,
                rules: Vec::with_capacity(file.program.rules.len()),
                skipped_rules: 0,
            };
            for (ri, rule) in file.program.rules.iter().enumerate() {
                let (index_time, query_time) = rule.phases();
                let contributes = match phase {
                    Phase::All => true,
                    Phase::Index => index_time,
                    Phase::Query => query_time,
                };
                if !contributes {
                    file_report.skipped_rules += 1;
                    file_report.rules.push(RuleStats::default());
                    continue;
                }
                let stats = emit::emit_rule(
                    rule,
                    &file.plans[ri],
                    &self.solutions[fi][ri],
                    &file.provenance(ri),
                    phase,
                    out,
                    &mut errors,
                );
                file_report.rules.push(stats);
            }
            if !errors.is_empty() {
                return Err(errors.into_error(&file.path, &file.source));
            }
            report.files.push(file_report);
        }
        Ok(report)
    }
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
    let mut matcher = DslMatcher::new(&set);
    matcher.observe_import(index);
    matcher.finish(phase, out)
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
            Phase::Query => ("query", "propagation/bridge/access_paths"),
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
