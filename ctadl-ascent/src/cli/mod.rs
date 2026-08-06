/*! CLI support

This module implements the CLI interface for CTADL. After parsing command line arguments, the main
rust file should be a thin wrapper on this API. The CLI is defined in terms of two key concepts,
the [`ArtifactImport`] and the [`AnalysisProject`]. An import refers to the original artifact
(outside the CTADL store) and where its code gets imported into an IR program. A project is a
collection of such programs that are analyzed together. This module understands the layout of where
all the intermediate files should be stored; the API below this should be written with those paths
as parameters.
*/

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

mod model_check;
pub use model_check::{ModelCheckOutcome, check_models, check_programs};

use itertools::Itertools;

use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::{Error, ErrorContext};
use crate::facts;
use crate::facts::FlowVariable;
use crate::index_engine::{
    IndexFacts, IndexResult, source_info::IndexSourceInfo, taint_index_with_config,
};
use crate::languages::{apk_native, dex, jni, jvm, lua, pcode, tree_sitter, xapk};
use crate::project::{AnalysisProject, ArtifactImport, ArtifactLanguage};
use crate::query_engine;
use crate::query_engine::{QueryFactsBuilder, taint_analysis};
use ctadl_ir::graph::is_connected;
use ctadl_ir::ssa;
use ctadl_ir::{ProgramInfo, encode};

/// How to perform one import, beyond the artifact and its language.
///
/// Every field only matters to an APK, which is the one artifact that imports *other*
/// artifacts out of itself (its native libraries; see [`apk_native`]).
/// [`Default`] is the plain behavior: import everything, reuse nothing.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions<'a> {
    /// Reuse an existing sub-import whose stored artifact hash still matches instead of
    /// redoing it. The parent artifact's own skip check lives in `main`; this is what
    /// carries the flag down to the sub-imports, where the saving (a disassembly run
    /// each) is much larger.
    pub skip_existing: bool,
    /// Import the native libraries packaged inside an APK. On by default.
    pub native_libs: bool,
    /// Import this ABI's libraries rather than the preferred one. See
    /// [`dex_reader::apk::ABI_PREFERENCE`].
    pub native_abi: Option<&'a str>,
}

impl Default for ImportOptions<'_> {
    fn default() -> Self {
        Self {
            skip_existing: false,
            native_libs: true,
            native_abi: None,
        }
    }
}

// Imports a program for an artifact into the store
pub fn import(import: &ArtifactImport, opts: ImportOptions<'_>) -> Result<(), Error> {
    use ArtifactLanguage::*;
    log::info!(
        "importing {} artifact '{}' from {}",
        import.language,
        import.name,
        import.artifact_path.display()
    );
    let program_info = match &import.language {
        Dex => dex::import_dex(&import.artifact_path)?,
        Apk => {
            // Dex first: it is cheap and it is what fails fast on an APK that is not one,
            // before any native library is extracted or handed to Ghidra.
            let dex::ApkImport {
                program_info,
                dex_count,
            } = dex::import_apk(&import.artifact_path)?;
            if dex_count > 0 {
                log::info!(
                    "{}: {} classes*.dex entr{}",
                    import.artifact_path.display(),
                    dex_count,
                    if dex_count == 1 { "y" } else { "ies" },
                );
            }
            // A split APK out of an app bundle has no Dex of its own; its libraries are
            // the whole import. Decided before extracting anything so an APK that has
            // neither half fails immediately, and so the reason is the APK's contents
            // rather than whatever `import_native_libs` happened to be able to do with
            // them (it returns no sub-imports when Ghidra is missing, too).
            if dex_count == 0 {
                apk_native::require_native_libs(&import.artifact_path)?;
                if opts.native_libs {
                    log::info!(
                        "{}: no classes*.dex entries; importing as a native-only split APK",
                        import.artifact_path.display(),
                    );
                } else {
                    // Not an error -- the user asked for this -- but the result is an
                    // import with nothing in it, which is worth saying out loud.
                    log::warn!(
                        "{}: no classes*.dex entries and --no-native-libs was passed, so this \
                         import will be empty",
                        import.artifact_path.display(),
                    );
                }
            }
            let sub_imports = apk_native::import_native_libs(import, opts)?;
            if !sub_imports.is_empty() {
                log::info!(
                    "'{}': {} sub-import(s) indexed alongside it: {}",
                    import.name,
                    sub_imports.len(),
                    sub_imports.join(", ")
                );
                // Reload rather than saving `import` back: a sub-import may have rewritten
                // the parent's config in the meantime, and the caller reloads after this
                // to pick these names up.
                let mut updated = ArtifactImport::load_by_name(&import.name)?;
                updated.sub_imports = sub_imports;
                updated.save()?;
            }
            program_info
        }
        Xapk => {
            let sub_imports = xapk::import_bundle(import, opts)?;
            if !sub_imports.is_empty() {
                log::info!(
                    "'{}': {} sub-import(s) indexed alongside it: {}",
                    import.name,
                    sub_imports.len(),
                    sub_imports.join(", ")
                );
                // Reload rather than saving `import` back: each split rewrote its own config in
                // the meantime, and the caller reloads after this to pick these names up.
                let mut updated = ArtifactImport::load_by_name(&import.name)?;
                updated.sub_imports = sub_imports;
                updated.save()?;
            }
            ProgramInfo::default()
        }
        Jar => jvm::import_jar(&import.artifact_path)?,
        Jvm => jvm::import_class(&import.artifact_path)?,
        Pcode => pcode::import_pcode(import)?,
        Lua => lua::import_lua(&import.artifact_path)?,
        Flowy => crate::codegen::flowy::import(import)?,
        C => tree_sitter::import_c(&import.artifact_path)?,
    };
    log::info!(
        "'{}': imported {} function(s)",
        import.name,
        program_info.program.functions.len()
    );
    log::debug!("encoding");
    save_program_info(program_info, import)?;
    Ok(())
}

/// How to perform one index, beyond the project and the model files.
///
/// Follows [`ImportOptions`]. [`Default`] is what `ctadl index` does
/// with no flags.
#[derive(Debug, Clone, Copy)]
pub struct IndexOptions<'a> {
    /// Suppress the automatic JNI link between Java `native` stubs and their native implementations
    /// (see [`crate::languages::jni`]). Suppresses the registry with it: the registry is one of the
    /// bridge's resolution tiers, not a separate feature.
    pub no_jni_bridge: bool,
    /// Ignore the `RegisterNatives` tables recovered at import time, leaving the bridge with the
    /// `Java_…` symbol convention alone. The clean A/B for what the registry contributes, with no
    /// re-import needed -- scanning happens at import time either way.
    pub no_jni_registry: bool,
    pub strategy: CallResolutionStrategy,
    pub prune_unreachable_cfg_nodes: bool,
    pub alias_rule: bool,
    pub dump_index_graph: Option<&'a Path>,
}

impl Default for IndexOptions<'_> {
    fn default() -> Self {
        Self {
            no_jni_bridge: false,
            no_jni_registry: false,
            strategy: CallResolutionStrategy::Mixed,
            prune_unreachable_cfg_nodes: true,
            alias_rule: true,
            dump_index_graph: None,
        }
    }
}

/// Indexes a project
/// If summary_projects is provided, loads summaries from those projects and maps them into the current project.
/// `no_default_models` suppresses the built-in per-language defaults, leaving `models` as the
/// complete set. See [`IndexOptions`] for the rest.
pub fn index(
    project: &AnalysisProject,
    summary_projects: &[String],
    models: &[std::path::PathBuf],
    no_default_models: bool,
    opts: IndexOptions<'_>,
) -> Result<(), Error> {
    let IndexOptions {
        no_jni_bridge,
        no_jni_registry,
        strategy,
        prune_unreachable_cfg_nodes,
        alias_rule,
        dump_index_graph,
    } = opts;
    use crate::index_engine::phys_footprint_mb;
    log::info!(
        "indexing project '{}' from {} import(s): {}",
        project.name,
        project.imports.len(),
        project.imports.join(", ")
    );
    log::debug!("[mem cp] index() start: {:.1} MB", phys_footprint_mb());
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();

    let file_specs = crate::models::scan_model_files(models)?;
    // Every matched model, instantiated against the IR being indexed. It persists across the import
    // loop and is codegen'd after it. Matches are a function of (artifact x models files) while the
    // import cache is a pure function of the artifact, and persisting them would let `ctadl index
    // --models a.json` poison the next `ctadl index --models b.json`.
    let mut model_matches = crate::models::ProgramModelMatches::default();
    model_matches.bridges.prepare(&file_specs.bridges);
    // Source/sink models are inert at index time; warn once rather than discarding in silence.
    // Keyed by file, not counted per (file, import) pair -- declaring an endpoint is a property
    // of the file, and every file is re-matched once per import.
    let mut files_declaring_endpoints: BTreeSet<&std::path::PathBuf> = BTreeSet::new();

    // Collects both halves of every JNI boundary as the imports go by; the link itself can only
    // happen after the loop, when one `IdMap` holds every program's functions.
    let mut jni_observer = jni::JniObserver::new();
    for import in project.iter_imports() {
        let import = import?;
        // Everything codegen records from here to the next import belongs to this one. Source
        // spans are per-import indices, so this is what keeps them resolvable afterwards.
        source_info.begin_import(&import.name);
        log::info!("'{}': loading IR", import.name);
        let mut program_info = load_program_info_without_source_info(&import)?;
        log::debug!(
            "[mem cp] loaded IR program (before SSA/codegen): {:.1} MB",
            phys_footprint_mb()
        );
        if !no_jni_bridge {
            jni_observer.observe(&program_info, jni::SlotModel::for_language(import.language));
            if !no_jni_registry {
                // The `RegisterNatives` tables this import's library was scanned for. Read from
                // the import directory rather than from the IR: they are a sidecar, so no
                // import format version moved and every older import simply has none.
                jni_observer.observe_registry(&import)?;
            }
        }

        // Match this import while its IR is in hand, and retain only the *matches*. The index
        // and the IR are both dropped before the next import is loaded, which is what keeps
        // the memory posture streaming rather than "every import's match index resident".
        {
            let scope = crate::models::ImportScope::new(import.language, &import.name);
            let match_index = crate::models::ProgramMatchIndex::new(&program_info, scope);
            if !no_default_models {
                crate::models::try_load_default_models(&match_index, &mut model_matches)?;
            }
            for model_path in models {
                let report =
                    crate::models::try_load_models(&match_index, model_path, &mut model_matches)?;
                if !report.endpoint_stats.is_empty() {
                    files_declaring_endpoints.insert(model_path);
                }
            }
            crate::models::matches::observe_import(
                &match_index,
                &file_specs.bridges,
                &mut model_matches,
            )?;
        }

        // Delete assigned-but-never-read temporaries, then fuse single-use
        // copy temporaries, both before SSA. Together they cut the statement /
        // variable count that SSA and the datalog fact base pay for. Dead-temp
        // elimination runs first: it removes defs that coalescing can't (a dead
        // temp has no use to fuse into) and shrinks the input coalescing scans.
        // Both are no-ops on programs already in SSA form (e.g. flowy imports).
        log::info!(
            "'{}': preprocessing {} function(s) (SSA) and generating facts",
            import.name,
            program_info.program.functions.len()
        );
        ssa::eliminate_dead_temps(&mut program_info.program);
        ssa::coalesce_copies(&mut program_info.program);
        ssa::transform_program(&mut program_info.program, prune_unreachable_cfg_nodes);
        ssa::propagate_copies(&mut program_info.program);
        log::debug!(
            "[mem cp] after SSA transform: {:.1} MB",
            phys_footprint_mb()
        );
        codegen_program(program_info, &mut facts, &mut source_info, strategy);
        log::debug!(
            "[mem cp] after codegen_program (IR dropped, facts built): {:.1} MB",
            phys_footprint_mb()
        );
    }
    // Measured, not assumed. Streaming matching retains only the *matches* -- one interned
    // name plus two (tag, index, path) triples per propagation -- and drops each import's
    // match index and IR with the import. On a 6.4 MB APK (`com.noto_54.apk`, 1224 propagation
    // matches from the shipped Java defaults plus a bridge spec) this checkpoint reads the same
    // 406.7 MB as the `after codegen_program` one immediately before it: the retained matches
    // are below the resolution of the gauge. Peak physical footprint for that whole run was
    // 2.27 GB, in `ascent_run`, not here.
    //
    // Still unquantified: the APK + `.so` pair. It is no longer an exotic shape -- importing
    // an APK now imports its native libraries as pcode sub-imports (see
    // [`crate::languages::apk_native`]), so a project naming one APK routinely walks several
    // programs here -- but the loop drops each import's IR before loading the next, so the
    // posture is per-import peak rather than the sum.
    log::debug!(
        "[mem cp] after import loop ({} propagation match(es), {} declared path(s), {} bridge \
         spec(s) retained): {:.1} MB",
        model_matches.propagations.len(),
        model_matches.access_paths.len(),
        file_specs.bridges.len(),
        phys_footprint_mb()
    );

    // Every import's functions are interned by now, which is what the bridge needs to resolve a
    // Java `native` stub and its `Java_…` implementation to two ids in the same map.
    if !no_jni_bridge {
        jni::link(&jni_observer, &mut facts, &mut source_info);
    }

    let model_report = crate::codegen::model_matches::codegen_model_matches(
        &model_matches,
        &file_specs.bridges,
        &mut facts,
        &mut source_info,
    )?;
    log::info!(
        "models: {} summary row(s), {} declared access path(s)",
        model_report.summaries,
        model_report.declared_paths
    );
    // Unconditionally, at info, even when nothing went wrong: a bridge-only generator appears on
    // no other surface, and this line is what catches the mis-paired case (wrong slot, wrong
    // path, wrong function matched) that warn-on-empty cannot.
    for stats in &model_report.bridges {
        log::info!("bridge {stats}");
    }
    if !files_declaring_endpoints.is_empty() {
        // The mirror of the warning `ctadl query` emits about propagation/bridging models: each
        // phase silently discarded what the other consumes, and this closes the second half.
        log::warn!(
            "{} of the given model file(s) declare source/sink models, which `ctadl index` \
             ignores -- pass them to `ctadl query` instead: {}",
            files_declaring_endpoints.len(),
            files_declaring_endpoints
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    drop(model_matches);

    // Load and map summaries from multiple projects if specified
    for summary_project_name in summary_projects {
        load_and_map_summaries(summary_project_name, project, &mut facts, &mut source_info)?;
    }

    let path = project.index_path()?;
    facts.try_save(&path)?;
    inspect_index_facts(&facts, Some(&source_info.sites)).unwrap();
    // Only the (small) site IdMap is needed after saving
    let sites = source_info.sites.clone();
    source_info.try_save(&path)?;
    log::debug!(
        "[mem cp] after facts.try_save: {:.1} MB",
        phys_footprint_mb()
    );
    let config = crate::index_engine::IndexConfig { alias_rule };
    log::info!("indexing (computing the flow relation)");
    let result = taint_index_with_config(facts, config, Some(&sites));

    // Slightly ugly special case for flowy artifacts. Since they have specific assertions at index
    // time, check them here.
    for import in project.iter_imports() {
        let import = import?;
        if import.language == ArtifactLanguage::Flowy {
            crate::codegen::flowy::index_check(&import, &result, &sites)?;
        }
    }

    if let Some(dot_path) = dump_index_graph {
        dump_index_graph_dot(&result.assign_like, &sites, dot_path)?;
    }

    let path = project.index_path()?;
    result
        .try_save(&path)
        .err_context(|| format!("saving index: {}", path.display()))?;
    // Last, so a run that dies partway through leaves no stamp claiming the index is readable.
    project.write_index_config()?;
    log::info!("wrote index to {}", path.display());
    Ok(())
}

/// What [`query`] concluded about the run as a whole, mirroring
/// `invocation.executionSuccessful` in the SARIF it just wrote. `false` means an
/// `error`-level notification was emitted — the query was vacuous — and per SARIF §3.58.6
/// the process should exit non-zero, *after* the SARIF file has been written.
#[derive(Debug, Clone, Copy)]
pub struct QueryStatus {
    pub execution_successful: bool,
    /// True when there was no index and the run reported on the model files alone. The SARIF
    /// says so in `CTADL0008`; this is so the caller can say it on the terminal too.
    pub model_check_only: bool,
}

/// Runs a taint query and formats the results as SARIF.
///
/// Taint results are computed in memory and handed straight to the formatter; they are
/// not persisted. The `profile` selects what the SARIF reports: path profiles enumerate
/// source -> sink paths, closure profiles list every tainted instruction (see
/// [`query_engine::formatter`]).
///
/// # Without an index
///
/// Model matching is decided per import and only half of it needs an index (see
/// [`model_check`]). So when the project has not been indexed -- or is *being* indexed, since
/// the version stamp is written last -- and model files were given, this reports what those
/// files match in the imported programs rather than failing outright. The SARIF is the ordinary
/// one, carrying the same notifications and saying in `CTADL0008` what it could not determine;
/// the run is still a failure, because the query asked for could not be answered.
pub fn query(
    project: &AnalysisProject,
    models: &[std::path::PathBuf],
    output: &Path,
    profile: query_engine::formatter::SarifProfile,
    dump_taint_graph: Option<&Path>,
) -> Result<QueryStatus, Error> {
    let start_time_utc = query_engine::formatter::utc_timestamp();
    log::info!("querying project '{}'", project.name);
    if !project.has_index() {
        if models.is_empty() {
            return Err(Error::MissingIndex {
                project: project.name.clone(),
            });
        }
        return query_model_check(project, models, output, profile, start_time_utc);
    }
    // Before touching a table: the parquet decoders panic on an encoding they cannot read, and
    // this is what turns that into an actionable "re-run `ctadl index`".
    project.check_index_config()?;
    let index_path = project.index_path()?;
    let ids = facts::IdMap::try_load(&index_path)
        .err_context(|| format!("loading IdMap from index: {}", index_path.display()))?;
    // Load the index tables once; they seed the query and are reused to format the results.
    let index_facts = IndexFacts::try_load(&index_path)
        .err_context(|| format!("loading index facts from: {}", index_path.display()))?;
    let index_result = IndexResult::try_load(&index_path)
        .err_context(|| format!("loading index result from: {}", index_path.display()))?;

    // Assembled alongside the query itself and handed to the SARIF writer, which turns it
    // into `run.invocations[0]`. See `formatter::QueryDiagnostics`.
    let mut diagnostics = query_engine::formatter::QueryDiagnostics {
        command_line: std::env::args_os()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" "),
        arguments: std::env::args_os()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        start_time_utc,
        ..Default::default()
    };

    let facts = {
        // One accumulator across every (import x model file) pair. Endpoints carry their own
        // `facts::Path`, so accumulation is an append and there is nothing to renumber.
        let mut model_matches = crate::models::ProgramModelMatches::default();
        // Counted across every file and import, and reported once at the end: bridges and
        // propagations are index-time constructs, and a query that silently drops them looks
        // exactly like one whose models did nothing.
        let mut ignored = crate::models::IndexTimeModelCounts::default();
        // Import outer, model file inner: one `ProgramInfo` decode and one match index per
        // import, reused across every model file, rather than one of each per (file, import)
        // pair. The match tables are a function of the program alone.
        if !models.is_empty() {
            for import in project.iter_imports() {
                let import = import?;
                let program_info = load_program_info_without_source_info(&import)?;
                let match_index = crate::models::ProgramMatchIndex::new(
                    &program_info,
                    crate::models::ImportScope::new(import.language, &import.name),
                );
                for model_path in models {
                    let report = crate::models::try_load_models(
                        &match_index,
                        model_path,
                        &mut model_matches,
                    )?;
                    ignored.merge(&report.index_time_models);
                    // Re-key this file's Stage-1 counts by file: `ModelLoadReport` is keyed by
                    // (generator index, direction) alone, which would conflate two model files
                    // that happen to number their generators the same. Merging over imports is
                    // what makes a generator that is dead against one import but live against
                    // another come out live.
                    for ((index, direction), stats) in &report.endpoint_stats {
                        diagnostics
                            .generator_stats
                            .entry((model_path.clone(), *index, *direction))
                            .or_default()
                            .merge(stats);
                    }
                }
            }
        }
        log::debug!(
            "[mem cp] after query model accumulation ({} endpoint match(es), {} propagation \
             match(es) ignored): {:.1} MB",
            model_matches.endpoints.len(),
            model_matches.propagations.len(),
            crate::index_engine::phys_footprint_mb()
        );
        if !ignored.is_empty() {
            log::warn!(
                "ignoring {} in the given model file(s); they take effect at \
                 `ctadl index` time, so re-run `ctadl index` with them to use them",
                ignored.describe()
            );
        }
        let mut builder = QueryFactsBuilder::default();
        let mut endpoints = Vec::new();
        // Slightly ugly special case for flowy artifacts. Since the query is built in, take it
        // into account here
        for import in project.iter_imports() {
            let import = import?;
            if import.language == ArtifactLanguage::Flowy {
                let eps = crate::codegen::flowy::get_endpoints(&import, &ids, &index_facts.call)?;
                endpoints.extend(eps);
            }
        }
        // Flowy endpoints are already resolved, so they count as both declared and matched:
        // a flowy-only run configures endpoints without any `-m` and must not be reported
        // as having configured none.
        let flowy_sources = endpoints
            .iter()
            .filter(|(ep,)| ep.direction == crate::facts::TaintDirection::Forward)
            .count();
        let flowy_sinks = endpoints.len() - flowy_sources;

        let mut formal_params = index_facts.formal_param.clone();
        // Gated on having something to resolve, not merely on `-m` having been passed: Stage 2
        // union-finds the whole `assign_like` relation before it touches an endpoint, and a
        // model-less or flowy-only query must not pay for that.
        if !model_matches.endpoints.is_empty() {
            let built = query_engine::build_query_endpoints(
                &model_matches.endpoints,
                &index_facts,
                &ids,
                &index_result.assign_like,
            );
            diagnostics.unresolved_functions = built.unresolved_functions;
            endpoints.extend(built.endpoints);
            formal_params.extend(built.formals);
        }

        // Declared: model *ports* of that direction, plus flowy's already-resolved
        // endpoints (one port each). Matched: post-fan-out `QueryEndpoint`s. Counting
        // (generator, direction) keys here instead made `CTADL0100` compare a count of
        // generators against a count of endpoints.
        let count_declared = |direction| {
            diagnostics
                .generator_stats
                .iter()
                .filter(|((_, _, d), _)| *d == direction)
                .map(|(_, stats)| stats.ports_declared)
                .sum::<usize>()
        };
        diagnostics.sources_declared =
            count_declared(crate::facts::TaintDirection::Forward) + flowy_sources;
        diagnostics.sinks_declared =
            count_declared(crate::facts::TaintDirection::Backward) + flowy_sinks;

        let sources = endpoints
            .iter()
            .filter(|(ep,)| ep.direction == crate::facts::TaintDirection::Forward)
            .count();
        let sinks = endpoints
            .iter()
            .filter(|(ep,)| ep.direction == crate::facts::TaintDirection::Backward)
            .count();
        diagnostics.sources_matched = sources;
        diagnostics.sinks_matched = sinks;
        log::info!("matched {} sources and {} sinks", sources, sinks);
        // The line above reads the same whether an end is empty by accident or by design,
        // so say which it is rather than leaving the terminal silent about a vacuous query.
        if let Some(reason) = query_engine::formatter::empty_end_reason(&diagnostics) {
            log::warn!("{reason}; see the SARIF invocation for details");
        }

        // `actual_param` and `call` are also needed by `FormatFacts` below, so the
        // query facts take clones and the originals feed the formatter. `assign`,
        // `paths`, and `external_function` are consumed only by the query engine, so
        // they are moved.
        builder
            .endpoints(endpoints)
            .formal_param(formal_params)
            .actual_param(index_facts.actual_param.clone())
            .call(index_facts.call.clone())
            .assign(index_result.assign_like)
            .paths(index_result.paths)
            .external_function(index_result.external_function);
        builder.build().unwrap()
    };

    // A single taint pass computes the closure, the taint graph, and the
    // instruction-level facts the formatter needs.
    let result = taint_analysis(facts, Some(&ids));
    for import in project.iter_imports() {
        let import = import?;
        if import.language == ArtifactLanguage::Flowy {
            crate::codegen::flowy::query_check(&import, &result, &ids, &index_facts.call)?;
        }
    }

    let taint_results = query_engine::formatter::TaintAnalysisResults::from_query_result(&result);
    let mut b = query_engine::formatter::FormatFactsBuilder::default();
    b.taint(result.taint)
        .taint_edge(result.taint_edge)
        .index_actual_param(index_facts.actual_param)
        .call(index_facts.call)
        .id_to_name(ids.get_id_to_name_map());
    let facts = b.build().unwrap();

    let execution_successful = query_engine::formatter::format_sarif(
        project,
        &facts,
        &taint_results,
        output,
        profile,
        &diagnostics,
    )
    .err_context(|| "formatting sarif")?;

    if let Some(dot_path) = dump_taint_graph {
        dump_taint_graph_dot(&facts, &index_path, dot_path)?;
    }

    if output.to_str() != Some("-") {
        log::info!("wrote {}", output.display());
    }
    Ok(QueryStatus {
        execution_successful,
        model_check_only: false,
    })
}

/// [`query`] for a project with no index: report what the model files match against the
/// imported programs, and say plainly that that is all this is.
///
/// The output is the ordinary SARIF, written by the ordinary writer, because the questions it
/// answers are the ones `ctadl query` always answers -- which generators matched nothing, which
/// declared ports produced no row -- asked of Stage 1 alone. What is different is stated in the
/// file: see `CTADL0008`.
fn query_model_check(
    project: &AnalysisProject,
    models: &[std::path::PathBuf],
    output: &Path,
    profile: query_engine::formatter::SarifProfile,
    start_time_utc: String,
) -> Result<QueryStatus, Error> {
    log::warn!(
        "project '{}' has no index, so no taint analysis can run. Checking what the given model \
         file(s) select in the imported program(s) instead -- partial feedback: call sites, \
         `Argument(*)` and wildcard sinks need the index. Run `ctadl index {}` for a real query",
        project.name,
        project.name
    );
    let outcome = model_check::check_models(project, models)?;
    // The counts, on the terminal; the SARIF is where each of them is named and explained.
    log::info!(
        "checked {} model file(s) against {} import(s): {} generator(s) matched something, {} \
         declaration(s) matched nothing, {} generator(s) admitted by no import",
        models.len(),
        outcome.check.imports.len(),
        outcome.check.matched.len(),
        outcome.dead_declarations(),
        outcome.check.scope_excluded.len(),
    );

    let has_file_errors = outcome.has_file_errors();
    let mut diagnostics = outcome.into_diagnostics();
    diagnostics.command_line = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    diagnostics.arguments = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    diagnostics.start_time_utc = start_time_utc;

    let execution_successful =
        query_engine::formatter::format_model_check_sarif(project, output, profile, &diagnostics)
            .err_context(|| "formatting sarif")?;
    if output.to_str() != Some("-") {
        log::info!("wrote {}", output.display());
    }
    // A file error is reported in the SARIF (`CTADL0012`) rather than raised, so that one typo
    // does not cost the rest of the check. It still fails the run: `execution_successful` is
    // already false, since that notification is error-level.
    if has_file_errors {
        log::error!(
            "one or more model files have errors; see {}",
            output.display()
        );
    }
    Ok(QueryStatus {
        execution_successful,
        model_check_only: true,
    })
}

/// Renders the index (assign-like) graph to a Graphviz DOT file.
fn dump_index_graph_dot(
    assign_like: &[(
        facts::FunctionId,
        FlowVariable,
        facts::Path,
        FlowVariable,
        facts::Path,
    )],
    id_map: &facts::IdMap,
    dot_path: &Path,
) -> Result<(), Error> {
    let mut file = std::fs::File::create(dot_path)
        .err_context(|| format!("creating dot file: {}", dot_path.display()))?;
    // Embed the legend as a leading DOT comment so the file documents itself
    // (kept in sync with the log message below).
    {
        use std::io::Write as _;
        writeln!(
            file,
            "// Index (assign-like) graph. An edge `A -> B` is the assignment `B = A`;\n\
             // both directions appear for by-ref/aliased vertices.\n\
             //\n\
             // Node label grammar (two lines):\n\
             //   function(<name>)\n\
             //   <variable><access-path>\n\
             //\n\
             // The access path is a suffix of the second line, so a node is identified by\n\
             // the TRIPLE (function, variable, access path) -- `local(t)` and `local(t).x`\n\
             // are distinct nodes. The EMPTY access path renders as the empty string, so a\n\
             // label with no `.field` suffix means \"this vertex, empty path\" -- it does\n\
             // NOT mean paths are omitted from labels.\n"
        )
        .err_context(|| format!("writing index graph legend: {}", dot_path.display()))?;
    }
    crate::graphviz::render_index_graph(assign_like, id_map, &mut file)
        .err_context(|| format!("rendering index graph: {}", dot_path.display()))?;
    // Multi-line, with its own hanging indentation: the log formatter prepends nothing to
    // an info record, so the block below reaches the terminal as written.
    log::info!(
        "Wrote index graph to '{}'\n  \
         edge A -> B is the assignment B = A\n  \
         node label: `function(<name>)` / `<variable><access-path>`; nodes are keyed by\n  \
         (function, variable, access path), and an empty access path renders as nothing\n  \
         (a label with no `.field` suffix is the empty path, not an omitted one)",
        dot_path.display()
    );
    Ok(())
}

/// Renders the meet-in-the-middle taint graph to a Graphviz DOT file.
///
/// Classifies every tainted node by cone membership, orients each propagation
/// edge in data-flow direction (source → sink, matching `find_path`), then
/// renders the result with a self-documenting legend.
fn dump_taint_graph_dot(
    facts: &query_engine::formatter::FormatFacts,
    index_path: &Path,
    dot_path: &Path,
) -> Result<(), Error> {
    use crate::facts::TaintDirection;
    use crate::graphviz::Cone;

    // Classify every tainted node by cone membership: a node carrying a
    // Forward endpoint is in the forward cone, a Backward endpoint puts
    // it in the backward cone, and both makes it a "meet" (Cone::Both)
    // — exactly the nodes on a complete source→sink path.
    let mut node_cone: std::collections::BTreeMap<_, Cone> = std::collections::BTreeMap::new();
    for (func_id, _, var, path, ep) in &facts.taint {
        let cone = match ep.direction {
            TaintDirection::Forward => Cone::Forward,
            TaintDirection::Backward => Cone::Backward,
        };
        node_cone
            .entry((*func_id, *var, *path))
            .and_modify(|c| *c = c.join(cone))
            .or_insert(cone);
    }

    // The persisted taint edges are already in data-flow (execution) order
    // (source → derived, matching `find_path`) and no longer carry the
    // exploration direction, so draw each edge as-is and dedup. Approximate an
    // edge's cone from the cones its endpoints already carry (from the
    // authoritative `facts.taint` classification above); a `Call`/`Return`/`Intra`
    // label does not itself imply forward vs backward.
    let mut oriented: std::collections::BTreeMap<_, Cone> = std::collections::BTreeMap::new();
    for (_edge, sf, sv, sp, df, dv, dp) in &facts.taint_edge {
        let src = (*sf, *sv, *sp);
        let dst = (*df, *dv, *dp);
        let cone = match (node_cone.get(&src), node_cone.get(&dst)) {
            (Some(a), Some(b)) => a.join(*b),
            (Some(c), None) | (None, Some(c)) => *c,
            (None, None) => Cone::Both,
        };
        oriented
            .entry((src, dst))
            .and_modify(|c| *c = c.join(cone))
            .or_insert(cone);
    }
    let edges: Vec<_> = oriented
        .into_iter()
        .map(|((src, dst), cone)| (src.0, src.1, src.2, dst.0, dst.1, dst.2, cone))
        .collect();

    // The saved query taint is pruned to the relevant (content-level)
    // vertices, but the re-derived edges also touch pointer-level
    // intermediates. Declare every edge endpoint so none renders as an
    // uncolored auto-node. Nodes already classified from `facts.taint`
    // endpoints keep that authoritative (find_path-consistent) cone —
    // structural pointer edges must not promote a content node to a
    // false meet — while edge-only intermediates are colored by their
    // incident edges' cone(s).
    let from_taint: BTreeSet<_> = node_cone.keys().cloned().collect();
    for (sf, sv, sp, df, dv, dp, cone) in &edges {
        for n in [(*sf, *sv, *sp), (*df, *dv, *dp)] {
            if from_taint.contains(&n) {
                continue;
            }
            node_cone
                .entry(n)
                .and_modify(|c| *c = c.join(*cone))
                .or_insert(*cone);
        }
    }

    let sources: BTreeSet<_> = facts
        .taint
        .iter()
        .filter_map(|(_, _, _, _, ep)| {
            if ep.direction == TaintDirection::Forward {
                Some((ep.infunc, ep.vertex.0, ep.vertex.1))
            } else {
                None
            }
        })
        .collect();
    let sinks: BTreeSet<_> = facts
        .taint
        .iter()
        .filter_map(|(_, _, _, _, ep)| {
            if ep.direction == TaintDirection::Backward {
                Some((ep.infunc, ep.vertex.0, ep.vertex.1))
            } else {
                None
            }
        })
        .collect();
    let mut file = std::fs::File::create(dot_path)
        .err_context(|| format!("creating dot file: {}", dot_path.display()))?;
    // Embed the legend as a leading DOT comment so the file documents
    // itself (kept in sync with the log message below).
    {
        use std::io::Write as _;
        writeln!(
            file,
            "// Taint graph (meet-in-the-middle): a forward cone grows from each source,\n\
             // a backward cone from each sink. A \"meet\" lies in both cones -- i.e. on a\n\
             // complete source->sink path. Reading reachability is then visual: follow the\n\
             // forward cone out of a source diamond and see whether it reaches the meet.\n\
             //\n\
             // Nodes (shape = role, fill = cone):\n\
             //   diamond  = source vertex      (fill gold)\n\
             //   ellipse  = sink vertex        (fill orange)\n\
             //   box      = propagated vertex\n\
             //   fill lightblue = forward cone (reachable from a source)\n\
             //   fill mistyrose = backward cone (reaches a sink)\n\
             //   fill palegreen = meet (on a source->sink path)\n\
             //\n\
             // Edges (oriented in data-flow direction, source -> sink, matching find_path):\n\
             //   blue        = forward propagation\n\
             //   red dashed  = backward propagation\n\
             //   bold green  = meet edge (on a source->sink path)\n"
        )
        .err_context(|| format!("writing taint graph legend: {}", dot_path.display()))?;
    }
    let ids = facts::IdMap::try_load(index_path).err_context(|| {
        format!(
            "loading IdMap for taint graph from index: {}",
            index_path.display()
        )
    })?;
    crate::graphviz::render_taint_graph(node_cone, &edges, &sources, &sinks, &ids, &mut file)
        .err_context(|| format!("rendering taint graph: {}", dot_path.display()))?;
    // Multi-line; see the note in `dump_index_graph_dot`.
    log::info!(
        "Wrote taint graph to '{}'\n  \
         nodes: diamond=source, ellipse=sink, box=propagated; \
         fill lightblue=forward cone, mistyrose=backward cone, palegreen=meet (on a source→sink path)\n  \
         edges (data-flow oriented, source→sink): blue=forward, red dashed=backward, bold green=meet",
        dot_path.display()
    );
    Ok(())
}

pub fn save_program_info(
    mut program_info: ProgramInfo,
    import: &ArtifactImport,
) -> Result<(), Error> {
    let path = &import.program_path();
    let obj = std::mem::take(&mut program_info.program);
    for f in obj.functions.iter() {
        if f.blocks.is_empty() {
            continue;
        }
        // Real disassembled binaries routinely contain functions with blocks
        // that are unreachable from entry (Ghidra CFG recovery artifacts). This
        // is not an import error: indexing prunes unreachable blocks before the
        // SSA/dominator pass (see `--prune-unreachable-cfg-nodes`, on by
        // default), so record but don't reject them here.
        if !is_connected(&f.blocks) {
            log::debug!("function has blocks unreachable from entry: {}", f.name);
        }
    }
    let data = encode::encode_program(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing program: {}", path.display()))?;
    log::debug!("wrote {}", path.display());

    let path = &import.vmt_path();
    let obj = std::mem::take(&mut program_info.vmt);
    let data = bitcode::serialize(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing vmt: {}", path.display()))?;
    log::debug!("wrote {}", path.display());

    let path = import.source_info_dir();
    let obj = std::mem::take(&mut program_info.source_info);
    std::fs::create_dir_all(&path)
        .err_context(|| format!("creating source info dir: {}", path.display()))?;
    source_info::write_parquet_source_info(&path, &obj)
        .err_context(|| format!("writing source info: {}", path.display()))?;
    Ok(())
}

/// Load a serialized [`ProgramInfo`] from the import directory. The source info is elided.
fn load_program_info_without_source_info(import: &ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.program_path();
    log::debug!("reading {}", path.display());
    let data =
        std::fs::read(path).err_context(|| format!("reading program: {}", path.display()))?;
    let program = ctadl_ir::encode::decode_program(&data)
        .err_context(|| format!("decoding program: {}", path.display()))?;

    let path = &import.vmt_path();
    log::debug!("reading {}", path.display());
    let data = std::fs::read(path).err_context(|| format!("reading vmt: {}", path.display()))?;
    let vmt =
        bitcode::deserialize(&data).err_context(|| format!("decoding vmt: {}", path.display()))?;

    Ok(ProgramInfo {
        program,
        vmt,
        source_info: Default::default(),
    })
}

/// Load summaries from a previously indexed project and map them into the current project.
/// This function handles the FunctionId mapping between the source and target projects.
fn load_and_map_summaries(
    summary_project_name: &str,
    _current_project: &AnalysisProject,
    current_facts: &mut IndexFacts,
    current_source_info: &mut IndexSourceInfo,
) -> Result<(), Error> {
    log::info!("Loading summaries from project: {}", summary_project_name);

    // Load the summary project
    let summary_project = AnalysisProject::try_load_name(summary_project_name)
        .err_context(|| format!("loading summary project: {}", summary_project_name))?;

    // Load summaries directly using schema::summary::try_load
    summary_project.check_index_config()?;
    let summary_index_path = summary_project.index_path()?;
    let source_summaries = crate::facts::schema::summary::try_load(&summary_index_path)
        .err_context(|| format!("loading source project summaries: {}", summary_project_name))?;

    // Load the source project's id map for function name resolution
    let source_id_map = facts::IdMap::try_load(&summary_index_path)
        .err_context(|| format!("loading source project id map: {}", summary_project_name))?;

    // Get the current project's id map
    let current_id_map = &current_source_info.sites;

    log::info!(
        "Found {} summaries in source project",
        source_summaries.len()
    );

    // Map summaries from source project to current project
    let mut mapped_summaries = 0;
    let mut discarded_summaries = 0;

    for (source_func_id, dst_index, dst_path, src_index, src_path) in source_summaries {
        // Get the function name from the source project
        let source_func_name = match source_id_map.get_function(source_func_id) {
            Some(func) => func,
            None => {
                log::warn!(
                    "Source function ID {} not found in source id map",
                    source_func_id.id
                );
                discarded_summaries += 1;
                continue;
            }
        };

        // Check if this function exists in the current project
        let target_func_id = match current_id_map.get_function_id(source_func_name.clone()) {
            Some(func_id) => func_id,
            None => {
                log::trace!(
                    "Function {} not found in current project, discarding summary",
                    source_func_name
                );
                discarded_summaries += 1;
                continue;
            }
        };

        // Add the mapped summary to current facts
        current_facts
            .summary
            .push((target_func_id, dst_index, dst_path, src_index, src_path));
        mapped_summaries += 1;
    }

    log::info!(
        "Summary mapping complete: {} mapped, {} discarded",
        mapped_summaries,
        discarded_summaries
    );

    Ok(())
}

/// Pretty-print the imported IR. With `filter`, only functions whose name contains that
/// substring are printed; otherwise every function is printed. Uses the `Display` impls in
/// `ctadl_ir::mir`, so the output matches the in-memory AST the analysis consumes -- except that
/// locals are resolved to their source names through each function's `Locals` table
/// (`WithLocalNames`), since `%L7` on its own tells a reader nothing.
pub fn dump_ir(import: &ArtifactImport, filter: Option<&str>) -> Result<(), Error> {
    let program_info = load_program_info_without_source_info(import)?;
    let mut matched = 0usize;
    for func in program_info.program.functions.iter() {
        if filter.is_none_or(|pat| func.name.contains(pat)) {
            matched += 1;
            println!("{}", ctadl_ir::mir::WithLocalNames(func));
        }
    }
    if let Some(pat) = filter
        && matched == 0
    {
        log::warn!("no function name contains '{pat}'");
    }
    Ok(())
}

pub fn inspect(import: &ArtifactImport) -> Result<(), Error> {
    let program_info = load_program_info_without_source_info(import)?;
    let program = &program_info.program;

    let mut total_assignments = 0;
    let mut func_assignments = Vec::new();
    let mut call_style_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();

    for func in program.functions.iter() {
        let mut current_func_assignments = 0;
        for block in func.blocks.iter() {
            for stmt in block.statements.iter() {
                current_func_assignments += stmt.iter_dst_var().count();

                if let ctadl_ir::StatementKind::CallAssign { style, .. } = &stmt.kind {
                    let style_name = match style {
                        ctadl_ir::call::CallStyle::Unknown => "Unknown",
                        ctadl_ir::call::CallStyle::DirectCall { .. } => "DirectCall",
                        ctadl_ir::call::CallStyle::FuncPtrCall { .. } => "FuncPtrCall",
                        ctadl_ir::call::CallStyle::JavaCall { .. } => "JavaCall",
                        ctadl_ir::call::CallStyle::LuaCall { .. } => "LuaCall",
                    };
                    *call_style_counts.entry(style_name).or_insert(0) += 1;
                }
            }
        }
        total_assignments += current_func_assignments;
        func_assignments.push(current_func_assignments);
    }

    func_assignments.sort_unstable();
    let median_assignments = if func_assignments.is_empty() {
        0.0
    } else {
        let mid = func_assignments.len() / 2;
        if func_assignments.len() % 2 == 0 {
            (func_assignments[mid - 1] + func_assignments[mid]) as f64 / 2.0
        } else {
            func_assignments[mid] as f64
        }
    };

    println!(
        "Artifact: {} ({})",
        import.name,
        import.artifact_path.display()
    );
    println!("  Number of functions: {}", program.functions.len());
    println!("  Total number of assignments: {}", total_assignments);
    println!(
        "  Median assignments per function: {:.1}",
        median_assignments
    );
    println!("  CallStyle Distribution:");
    if call_style_counts.is_empty() {
        println!("    None");
    } else {
        let mut sorted_counts: Vec<_> = call_style_counts.into_iter().collect();
        sorted_counts.sort_by_key(|&(style, _)| style);
        for (style, count) in sorted_counts {
            println!("    {}: {}", style, count);
        }
    }

    Ok(())
}

pub fn list_store_contents() -> Result<(), Error> {
    use crate::project::StorePaths;
    use std::fs;

    println!("Store root: {}", StorePaths::root().display());

    // List imports
    let import_path = StorePaths::import_path();
    println!("Imported artifacts:");
    let mut imports = Vec::new();
    if import_path.exists() {
        let entries = fs::read_dir(&import_path)
            .err_context(|| format!("listing imports: {}", import_path.display()))?;
        for entry in entries {
            let entry =
                entry.err_context(|| format!("listing imports: {}", import_path.display()))?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                imports.push(name.to_string());
            }
        }
    }
    if imports.is_empty() {
        println!("Found no imported artifacts. Use the `import` command to import one");
    } else {
        imports.sort();
        for name in imports {
            if let Ok(import) = ArtifactImport::load_by_name(&name) {
                println!("  {} ({})", name, import.artifact_path.display());
            } else {
                println!("  {}", name);
            }
        }
    }

    println!();

    // List projects
    let projects_path = StorePaths::projects_path();
    println!("Analysis projects:");
    let mut projects = Vec::new();
    if projects_path.exists() {
        let entries = fs::read_dir(&projects_path)
            .err_context(|| format!("listing projects: {}", projects_path.display()))?;
        for entry in entries {
            let entry =
                entry.err_context(|| format!("listing projects: {}", projects_path.display()))?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                projects.push(name.to_string());
            }
        }
    }
    if projects.is_empty() {
        println!("Found no analysis projects. Use the `index` command to create one");
    } else {
        projects.sort();
        for name in projects {
            if let Ok(project) = AnalysisProject::try_load_name(&name) {
                println!("  {} ({})", name, project.imports.join(", "));
            } else {
                println!("  {}", name);
            }
        }
    }

    Ok(())
}

pub fn inspect_parquet<P: AsRef<std::path::Path>>(path: P) -> Result<(), Error> {
    use crate::facts::schema::*;
    let path = path.as_ref();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Path {
            message: "invalid filename".to_string(),
        })?;

    let parent = path.parent().unwrap_or(std::path::Path::new("."));

    macro_rules! match_schema {
        ($($mod:ident),*) => {
            match filename {
                $($mod::FILENAME => {
                    let records = $mod::try_load(parent)?;
                    for record in records {
                        println!("{:?}", record);
                    }
                })*
                _ => return Err(Error::Path { message: format!("unrecognized parquet file: {}", filename) }),
            }
        }
    }

    match_schema!(
        formal_param,
        actual_param,
        call,
        assign,
        call_target_assign,
        callee_info,
        callee_resolvents,
        summary,
        paths,
        taint,
        index_source_map,
        import_id,
        function_id,
        external_function
    );

    Ok(())
}

/// Prints the `RegisterNatives` tables recovered from an import: one row per entry, with the
/// address it was found at, the function it resolved to, and its Java signature.
///
/// # Errors
///
/// If the file cannot be read or parsed.
pub fn inspect_jni_registry<P: AsRef<std::path::Path>>(path: P) -> Result<(), Error> {
    jni::registry::JniRegistry::print(path.as_ref())
}

pub fn inspect_bitcode<P: AsRef<std::path::Path>>(path: P) -> Result<(), Error> {
    let path = path.as_ref();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Path {
            message: "invalid filename".to_string(),
        })?;

    // Unlike every other reader of these files, this one is handed a raw path rather than an
    // `ArtifactImport` — deliberately, so a store can still be inspected when its import is
    // too old for `ArtifactImport::load` to accept. The cost is that nothing has checked
    // `IMPORT_FORMAT_VERSION` by the time we decode, and `bitcode::Error` is a zero-sized type
    // outside debug builds that renders as a bare "bitcode error". Read the version out of the
    // sibling config so the failure names the real cause instead of guessing at it.
    let decode_failed = |what: &str| {
        let expected = crate::project::IMPORT_FORMAT_VERSION;
        match crate::project::import_format_version_beside(path) {
            Some(found) if found != expected => format!(
                "decoding {what} '{}': it was written with import format {found}, but this \
                 build expects {expected}; re-import the artifact",
                path.display(),
            ),
            Some(_) => format!(
                "decoding {what} '{}': its import format ({expected}) matches this build, so \
                 the file is likely truncated or corrupt",
                path.display(),
            ),
            None => format!(
                "decoding {what} '{}': no readable '{}' beside it, so its import format is \
                 unknown; this build expects {expected}",
                path.display(),
                crate::project::IMPORT_CONFIG_FILE,
            ),
        }
    };
    let data = std::fs::read(path).err_context(|| format!("reading {}", path.display()))?;
    if filename == crate::project::PROGRAM_BITCODE_FILE {
        let program =
            ctadl_ir::encode::decode_program(&data).err_context(|| decode_failed("program"))?;
        // Resolve locals through each function's table: a dump full of `%L7` is unreadable now
        // that the name lives in `FunctionData::locals` rather than in the variable itself.
        println!("{}", ctadl_ir::mir::WithLocalNames(&program));
    } else if filename == crate::project::VMT_BITCODE_FILE {
        let vmt: ctadl_ir::call::VirtualMethodTable =
            bitcode::deserialize(&data).err_context(|| decode_failed("vmt"))?;
        println!("{}", vmt);
    } else {
        return Err(Error::Path {
            message: format!("unrecognized bitcode file: {}", filename),
        });
    }

    Ok(())
}

// fn build_query_facts(project: &AnalysisProject) -> Result<IndexResult, Error> {
//     // Get the original programs to
//     for import in project.iter_imports() {
//         let import = import?;
//         let program_info = load_program_info_without_source_info(&import)?;
//     }
//     let path = &project.index_path()?;
//     let index = IndexResult::try_load(path)?;
//     Ok(index)
// }

pub fn inspect_index_facts(
    facts: &IndexFacts,
    id_map: Option<&facts::IdMap>,
) -> anyhow::Result<()> {
    log::debug!("IndexFacts Statistics:");
    log::debug!("  formal_param:   {}", facts.formal_param.len());
    log::debug!("  actual_param:   {}", facts.actual_param.len());
    log::debug!("  call:           {}", facts.call.len());
    log::debug!("  assign:         {}", facts.assign.len());
    log::debug!("  summary:        {}", facts.summary.len());
    log::debug!("  paths:          {}", facts.paths.len());
    log::debug!("  callee_info:    {}", facts.callee_info.len());
    log::debug!("  callee_resolvents: {}", facts.callee_resolvents.len());
    log::debug!("  call_target_assign:{}", facts.call_target_assign.len());
    log::debug!("  external_function: {}", facts.external_function.len());

    use crate::facts::InsnSiteId;

    let mut site_resolvents: Vec<_> = facts
        .call
        .iter()
        .sorted_by_key(|(s, _)| *s)
        .chunk_by(|(s, _)| *s)
        .into_iter()
        .map(|(site, group)| (site, group.map(|(_, r)| *r).unique().collect::<Vec<_>>()))
        .collect();

    site_resolvents.sort_by_key(|k| k.1.len());

    let top_n = 50;
    log::debug!("\nTop {top_n} busiest call sites (by number of unique targets):");
    for (site, resolvents) in site_resolvents.iter().rev().take(top_n) {
        let num_resolvents = resolvents.len();
        let InsnSiteId { func_id, insn_id } = InsnSiteId::try_from(*site).unwrap();

        let func_name = id_map
            .and_then(|m| m.get_function(func_id))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");

        log::debug!(
            "  Site in {func_name} ({}):{} has {num_resolvents} targets",
            func_id.id,
            insn_id.id
        );
        for target_id in resolvents.iter().take(3) {
            let target_name = id_map
                .and_then(|m| m.get_function(*target_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");
            log::debug!("    -> {target_name} ({target_id:?})");
        }
        if num_resolvents > 3 {
            log::debug!("    ... and {} more", num_resolvents - 3);
        }
    }

    let mut target_count_dist = HashMap::new();
    for (_, resolvents) in &site_resolvents {
        *target_count_dist.entry(resolvents.len()).or_insert(0) += 1;
    }
    let mut sorted_dist: Vec<_> = target_count_dist.into_iter().collect();
    sorted_dist.sort_by_key(|(count, _)| *count);
    log::debug!("\nCall site target count distribution:");
    for (count, num_sites) in sorted_dist {
        log::debug!("  {count} targets: {num_sites} sites");
    }

    // Assign analysis - which functions have most assigns?
    let mut func_assigns = HashMap::new();
    for (site, _, _) in &facts.assign {
        let InsnSiteId { func_id, .. } = InsnSiteId::try_from(*site).unwrap();
        *func_assigns.entry(func_id).or_insert(0) += 1;
    }
    let mut sorted_assigns: Vec<_> = func_assigns.into_iter().collect();
    sorted_assigns.sort_by_key(|(_, count)| *count);
    log::debug!("\nTop 20 functions by number of assigns:");
    for (func_id, count) in sorted_assigns.iter().rev().take(20) {
        let func_name = id_map
            .and_then(|m| m.get_function(*func_id))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");
        log::debug!("  {func_name} ({func_id:?}): {count} assigns");
    }

    // Path analysis
    let mut path_len_dist = HashMap::new();
    let mut path_examples: HashMap<usize, Vec<String>> = HashMap::new();
    for (path,) in &facts.paths {
        let s = path.to_dot_string();
        let len = path.len();
        *path_len_dist.entry(len).or_insert(0) += 1;
        let examples = path_examples.entry(len).or_default();
        if examples.len() < 2 {
            examples.push(s.to_string());
        }
    }
    let mut sorted_path_lens: Vec<_> = path_len_dist.into_iter().collect();
    sorted_path_lens.sort_by_key(|(len, _)| *len);
    log::debug!("\nPath length distribution:");
    for (len, count) in sorted_path_lens {
        let examples = &path_examples[&len];
        log::debug!(
            "  length {len}: {count} paths (e.g., {})",
            examples.join(", ")
        );
    }

    // Actual param analysis
    let mut site_actuals = HashMap::new();
    for (site, _, _) in &facts.actual_param {
        *site_actuals.entry(*site).or_insert(0) += 1;
    }
    let mut actual_count_dist = HashMap::new();
    for count in site_actuals.values() {
        *actual_count_dist.entry(*count).or_insert(0) += 1;
    }
    let mut sorted_actual_dist: Vec<_> = actual_count_dist.into_iter().collect();
    sorted_actual_dist.sort_by_key(|(count, _)| *count);
    log::debug!("\nActual params per call site distribution:");
    for (count, num_sites) in sorted_actual_dist {
        log::debug!("  {count} actuals: {num_sites} sites");
    }

    Ok(())
}
