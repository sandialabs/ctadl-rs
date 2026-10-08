/*! `ctadl report` -- measuring program-analysis-relevant properties of an imported program.

The emphasis is the call graph and the class hierarchy: how many calls a program makes, how
precisely CHA resolves them, where the imprecision concentrates, and what shape the resulting
graph has. Every number is meant to be traceable to a decision -- special-case the worst
handful of call sites, push hybrid inlining one frame deeper, or leave it alone.

# Tiers

`spec.md` describes two tiers: **static**, which needs only an import, and **resolved**,
which reads an index. This version implements the static tier only, and says so in its first
line. That is not a shortcut. Every rung of the [`CallResolutionStrategy::Mixed`] ladder is a
function of the import -- a signature's CHA target count, the class hierarchy an
`invoke-super` walks, and which signatures a model file names -- so the `policy` section
simulates the whole classification without reading an index, which is what lets a user write a
dispatch model and see its effect in seconds. Fan-in and recursion over the CHA graph, which is
what hybrid inlining actually contends with, need no index either. What an index would
genuinely add is how many call frames away a receiver's allocation is, and that needs a new
output relation first.

[`CallResolutionStrategy::Mixed`]: crate::codegen::CallResolutionStrategy

# Interface calls are never averaged in with class-virtual ones

Every measurement that counts call sites is reported twice: pooled, and again per dispatch
kind. `CallStyle::JavaCall` carries which of `invoke-virtual`, `invoke-interface` and
`invoke-super` it came from, and the three behave nothing alike under CHA -- an interface
admits every unrelated class that implements it, and a pooled percentile over the two
populations belongs to neither. Nothing *resolves* differently for it: an `invoke-super` is
still resolved as though the receiver's type were unknown, and this measures what that costs
rather than changing it.

The virtual method table carries two matching facts about types rather than calls: which ones
the import declares `interface`, and which method declarations are abstract. Together they
identify a functional interface -- one interface, one abstract method -- without matching a
name or a package, which is what makes that section survive an obfuscated app where the
Kotlin one cannot.

# One report per program

A name resolves the way `ctadl query` resolves it -- a project, or an import of the same
name -- and then expands to that project's imports, sub-imports included. Every non-empty one
is measured separately, because the class hierarchy is per program: two imports have two
virtual method tables and resolving a call in one against the other's hierarchy would be
making the answer up.

Measuring them separately is also the only thing that works. An `.xapk` splits into one
program per split APK and **the parent import carries no functions at all** -- we have
observed a parent with zero functions whose single split carried over 1.8 million. A report
that took "the first import" would print zeros for a two-million-function app.
*/

use std::path::Path;

use serde::Serialize;

use crate::error::{Error, ErrorContext};
use crate::project::AnalysisProject;
use ctadl_import::{SourceInfoMode, load_import};

pub mod callgraph;
pub mod render;

pub use callgraph::CallGraphReport;

/// Output format for [`report`]'s rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ReportFormat {
    /// For reading.
    #[default]
    Text,
    /// One object per section, for tracking numbers across runs.
    Json,
}

/// How to produce one report, beyond the project. Follows [`crate::cli::IndexOptions`].
#[derive(Debug, Clone, Copy)]
pub struct ReportOptions {
    pub format: ReportFormat,
    /// How many rows the worst-signature and fan-in lists carry.
    pub top: usize,
    /// Measure recursion and strongly connected components.
    ///
    /// On by default, and the one section worth turning off. It is the only part of the
    /// report that has to materialize the CHA call graph, whose size is not the size of the
    /// program: we have observed 5.2 million virtual call sites expand to 1.21 billion
    /// deduplicated edges, and turning this section off took that run from 89 s to 55 s.
    ///
    /// It buys wall time, not headroom. The peak is reached earlier, inside `run_cha`, and
    /// the graph fits underneath it -- the same run peaked at 24.9 GiB with this section and
    /// 24.1 GiB without.
    ///
    /// It now runs Tarjan twice: once over the whole graph and once with the
    /// interface-dispatched edges removed, which is what says how much of a program's largest
    /// cycle is interface resolution rather than the program. On the run above that went from
    /// 95 s to 114 s and did not move the peak, because both passes read one successor array
    /// -- each caller's class-virtual targets are stored first and the second pass stops
    /// there.
    pub recursion: bool,
    /// Suppress the built-in default models when simulating the call-resolution policy, so
    /// `--models` is the complete set. Mirrors `ctadl index`.
    pub no_default_models: bool,
    /// The policy the `policy` section simulates. Give `ctadl report` the same flags as
    /// `ctadl index` and the two agree.
    pub call_policy: crate::codegen::CallPolicy,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            format: ReportFormat::Text,
            // `intent.md` asks for the top 10.
            top: 10,
            recursion: true,
            no_default_models: false,
            call_policy: crate::codegen::CallPolicy::default(),
        }
    }
}

/// A report over every program in a project.
#[derive(Debug, Serialize)]
pub struct Report {
    /// `"static"`. Named in the output so a number can never be mistaken for one an index
    /// produced.
    pub tier: &'static str,
    /// The project or import name the command was given.
    pub name: String,
    /// One entry per import that has functions, in project order (parent first).
    pub programs: Vec<CallGraphReport>,
    /// Imports that carry no functions, named rather than dropped: an `.xapk` parent is
    /// always one of these, and so is a split that ships only resources.
    pub empty_imports: Vec<String>,
}

/// Measures every program in `project` and returns the result. Reads no index.
pub fn report(
    project: &AnalysisProject,
    models: &[std::path::PathBuf],
    opts: ReportOptions,
) -> Result<Report, Error> {
    log::info!(
        "reporting on '{}' from {} import(s): {}",
        project.name,
        project.imports.len(),
        project.imports.join(", ")
    );
    let mut programs = Vec::new();
    let mut empty_imports = Vec::new();
    for import in project.iter_imports() {
        let import = import?;
        // Source info is what maps instructions back to the artifact; nothing here reports a
        // location, so it is skipped -- on a large APK it is a large read for nothing.
        let program_info = load_import(&import, SourceInfoMode::Skip)
            .err_context(|| format!("loading import '{}'", import.name))?;
        if program_info.program.functions.is_empty() {
            log::debug!("report: '{}' has no functions; skipping", import.name);
            empty_imports.push(import.name.clone());
            continue;
        }
        // One line per import, which is bounded by the project's import list rather than by
        // the size of any program in it -- `docs/debugging.md`'s rule for what may be logged
        // above `debug`. It is worth having: an `.xapk` is thirty sub-imports and one of them
        // may take a minute on its own.
        log::info!(
            "report: measuring '{}' ({} functions)",
            import.name,
            program_info.program.functions.len()
        );
        // The policy section simulates the ladder, and rung 1 is a matched dispatch model. This
        // is the same load `ctadl index` does, against the same program, so the two agree on
        // which signatures a model covers.
        let matches = match_models(&program_info, &import, models, opts)?;
        programs.push(callgraph::measure(
            &import.name,
            &program_info,
            opts,
            &matches,
        ));
        // `program_info` goes out of scope here in any case; naming the drop is a note that
        // it must, since the next import is decoded into the space this one held and the
        // peak should be one program rather than all of them.
        drop(program_info);
    }
    Ok(Report {
        tier: "static",
        name: project.name.clone(),
        programs,
        empty_imports,
    })
}

/// Loads the model files against one import, the way `ctadl index` does.
///
/// Only the dispatch generators matter to the report, but the whole file is loaded: an endpoint
/// is what refuses a dispatch model, and a report that did not see the endpoints would claim
/// coverage the index will not give.
fn match_models(
    program_info: &ctadl_ir::ProgramInfo,
    import: &crate::project::ArtifactImport,
    models: &[std::path::PathBuf],
    opts: ReportOptions,
) -> Result<crate::models::ProgramModelMatches, Error> {
    let mut matches = crate::models::ProgramModelMatches::default();
    let scope = crate::models::ImportScope::new(import.language, &import.name);
    let keys = crate::models::DispatchKeys::from_program(&program_info.program);
    let match_index =
        crate::models::ProgramMatchIndex::new_with_dispatch(program_info, scope, Some(&keys));
    if !opts.no_default_models {
        crate::models::try_load_default_models(&match_index, &mut matches)?;
    }
    for path in models {
        crate::models::try_load_models(&match_index, path, &mut matches)?;
    }
    Ok(matches)
}

/// Renders `report` to `output`, or to stdout for `-`.
///
/// Both formats go to stdout by default, per `docs/debugging.md`: stdout is the answer,
/// stderr is progress.
pub fn write(report: &Report, opts: ReportOptions, output: &Path) -> Result<(), Error> {
    let mut writer: Box<dyn std::io::Write> = if output.to_str() == Some("-") {
        Box::new(std::io::stdout())
    } else {
        Box::new(
            std::fs::File::create(output)
                .err_context(|| format!("creating report output file: {}", output.display()))?,
        )
    };
    match opts.format {
        ReportFormat::Json => serde_json::to_writer_pretty(&mut writer, report)
            .err_context(|| format!("writing report: {}", output.display()))?,
        ReportFormat::Text => render::text(&mut writer, report)
            .err_context(|| format!("writing report: {}", output.display()))?,
    }
    writeln!(writer).err_context(|| format!("writing report: {}", output.display()))?;
    Ok(())
}
