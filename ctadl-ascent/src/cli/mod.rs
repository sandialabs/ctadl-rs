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

use itertools::Itertools;

use crate::codegen::models::codegen_summary;
use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::error::{Error, ErrorContext};
use crate::facts;
use crate::facts::{FlowVariable, FlowVertex, Label};
use crate::index_engine::{
    IndexFacts, IndexResult, source_info::IndexSourceInfo, taint_index_with_config,
};
use crate::languages::{dex, jvm, pcode};
use crate::project::{AnalysisProject, ArtifactImport, ArtifactLanguage};
use crate::query_engine;
use crate::query_engine::{QueryFactsBuilder, QueryResult, taint_analysis};
use ctadl_ir::ssa;
use ctadl_ir::{ProgramInfo, encode};

/// Helper: turn model endpoint table into QueryEndpoint vec
fn build_query_endpoints(
    batch: &crate::models::EndpointBatch,
    facts: &IndexFacts,
    idmap: &facts::IdMap,
) -> (
    Vec<(crate::query_engine::QueryEndpoint,)>,
    Vec<(facts::FunctionId, facts::FlowVariable, facts::FormalType)>,
) {
    use crate::models::FormalIndexTypeTag;
    let ap_map = batch.aps.build_ap_map();
    let func_num_params = facts.compute_num_params();

    let mut out_eps = Vec::new();
    let mut out_formals = Vec::new();
    for (func_name, selector_ty, idx_opt, path_id, label_str, direction) in batch.iter_endpoints() {
        // Resolve function name → FunctionId; skip if not present.
        let infunc = match idmap.get_function_id(crate::facts::Function(func_name.into())) {
            Some(id) => id,
            None => continue,
        };

        // Map selector tag to variables.
        let vars = match selector_ty {
            FormalIndexTypeTag::Index => {
                let i16_val = idx_opt.expect("index missing");
                vec![FlowVariable::Formal(i16_val.into())]
            }
            FormalIndexTypeTag::Return => {
                vec![FlowVariable::Formal(RETURN_INDEX.into())]
            }
            FormalIndexTypeTag::Global => {
                vec![FlowVariable::Formal(GLOBALS_INDEX.into())]
            }
            FormalIndexTypeTag::AnyArgument => func_num_params
                .get(&infunc)
                .map(|n| (0..*n).map(|i| FlowVariable::Formal(i.into())).collect())
                .unwrap_or_default(),
        };

        let ap: facts::Path = ap_map[&path_id].iter().cloned().collect();

        // Build label and direction.
        let lbl = Label(label_str.into());

        for var in vars {
            out_eps.push((crate::query_engine::QueryEndpoint {
                infunc,
                vertex: FlowVertex(var.clone(), ap.clone()),
                label: lbl.clone(),
                direction,
            },));
            if let FlowVariable::Formal(_) = var {
                out_formals.push((infunc, var, facts::FormalType::ByRef));
            }
        }
    }
    (out_eps, out_formals)
}

// Imports a program for an artifact into the store
pub fn import(import: &ArtifactImport) -> Result<(), Error> {
    use ArtifactLanguage::*;
    let program_info = match &import.language {
        Dex => dex::import_dex(&import.artifact_path)?,
        Apk => dex::import_apk(&import.artifact_path)?,
        Jar => jvm::import_jar(&import.artifact_path)?,
        Jvm => jvm::import_class(&import.artifact_path)?,
        Pcode => pcode::import_pcode(import)?,
        Flowy => crate::codegen::flowy::import(import)?,
        _ => unimplemented!(),
    };
    log::info!("encoding");
    save_program_info(program_info, import)?;
    Ok(())
}

/// Indexes a project
/// If summary_projects is provided, loads summaries from those projects and maps them into the current project.
pub fn index(
    project: &AnalysisProject,
    summary_projects: &[String],
    models: &[std::path::PathBuf],
    strategy: CallResolutionStrategy,
    prune_unreachable_cfg_nodes: bool,
    dump_object_graph: Option<&Path>,
) -> Result<(), Error> {
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    for import in project.iter_imports() {
        let import = import?;
        let mut program_info = load_program_info_without_source_info(&import)?;
        let mut models_batch = crate::models::try_load_default_models(&program_info)?;
        for model_path in models {
            let model = crate::models::try_load_models(&program_info, model_path)?;
            models_batch.union_with(&model)?;
        }

        log::trace!("summary length: {}", models_batch.summary.num_rows());
        ssa::transform_program(&mut program_info.program, prune_unreachable_cfg_nodes);
        codegen_program(program_info, &mut facts, &mut source_info, strategy);
        log::trace!("summary length: {}", facts.summary.len());
        codegen_summary(models_batch.summary, &mut facts, &mut source_info);
        log::trace!("summary length: {}", facts.summary.len());
    }

    // Load and map summaries from multiple projects if specified
    for summary_project_name in summary_projects {
        load_and_map_summaries(summary_project_name, project, &mut facts, &mut source_info)?;
    }

    let path = project.index_path()?;
    facts.clone().try_save(&path)?;
    inspect_index_facts(&facts, Some(&source_info.sites)).unwrap();
    source_info.clone().try_save(&path)?;
    // Index and save to the project dir
    let mut config = crate::index_engine::IndexConfig::default();
    if project.iter_imports().any(|i| {
        i.as_ref()
            .map(|imp| imp.language == crate::project::ArtifactLanguage::Pcode)
            .unwrap_or(false)
    }) {
        config.alias_rule = false;
    }
    let result = taint_index_with_config(facts, config);

    if let Some(dot_path) = dump_object_graph {
        let mut file =
            std::fs::File::create(dot_path).err_context(|| "creating object graph dot file")?;
        crate::index_engine::graphviz::render_object_graph(
            &result.vtx_points_to,
            &result.fld_points_to,
            &source_info.sites,
            &mut file,
        )
        .err_context(|| "rendering object graph")?;
        eprintln!("Wrote object graph to '{}'", dot_path.display());
    }

    // Slightly ugly special case for flowy artifacts. Since they have specific assertions at index
    // time, check them here.
    for import in project.iter_imports() {
        let import = import?;
        if import.language == ArtifactLanguage::Flowy {
            crate::codegen::flowy::index_check(&import, &result, &source_info.sites)?;
        }
    }
    let path = project.index_path()?;
    result
        .try_save(&path)
        .err_context(|| format!("saving index: {}", path.display()))?;
    Ok(())
}

/// Runs a taint query
pub fn query(project: &AnalysisProject, models: &[std::path::PathBuf]) -> Result<(), Error> {
    let index_path = project.index_path()?;
    let facts = {
        let mut models_batch: Option<crate::models::ModelsBatch> = None;
        let ids = facts::IdMap::try_load(&index_path).err_context(|| "loading IdMap")?;
        for model_path in models {
            for import in project.iter_imports() {
                let import = import?;
                let program_info = load_program_info_without_source_info(&import)?;
                let s = crate::models::try_load_models(&program_info, model_path)?;
                if let Some(ref mut s0) = models_batch {
                    s0.union_with(&s)?;
                } else {
                    models_batch = Some(s);
                }
            }
        }
        let mut builder = QueryFactsBuilder::default();
        let index_facts = IndexFacts::try_load(&index_path)?;
        let mut endpoints = Vec::new();
        // Slightly ugly special case for flowy artifacts. Since the query is built in, take it
        // into account here
        for import in project.iter_imports() {
            let import = import?;
            if import.language == ArtifactLanguage::Flowy {
                let eps = crate::codegen::flowy::get_endpoints(&import, &ids)?;
                endpoints.extend(eps);
            }
        }
        let mut formal_params = index_facts.formal_param.clone();
        if let Some(ref batch) = models_batch {
            let (eps, model_formals) = build_query_endpoints(&batch.endpoint, &index_facts, &ids);
            endpoints.extend(eps);
            formal_params.extend(model_formals);
        }

        let sources = endpoints
            .iter()
            .filter(|(ep,)| ep.direction == crate::facts::TaintDirection::Forward)
            .count();
        let sinks = endpoints
            .iter()
            .filter(|(ep,)| ep.direction == crate::facts::TaintDirection::Backward)
            .count();
        eprintln!("Matched {} sources and {} sinks", sources, sinks);

        builder
            .endpoints(endpoints)
            .formal_param(formal_params)
            .actual_param(index_facts.actual_param)
            .call(index_facts.call);
        let index_result = IndexResult::try_load(&index_path)?;
        builder
            .assign(index_result.assign_like)
            .paths(index_result.paths);
        // Insert model-derived endpoints if present
        builder.build().unwrap()
    };

    let result = taint_analysis(facts);
    for import in project.iter_imports() {
        let import = import?;
        if import.language == ArtifactLanguage::Flowy {
            let ids = facts::IdMap::try_load(&index_path).err_context(|| "loading IdMap")?;
            crate::codegen::flowy::query_check(&import, &result, &ids)?;
        }
    }
    let path = project.query_path()?;
    result
        .try_save(&path)
        .err_context(|| format!("saving query: {}", path.display()))?;
    Ok(())
}

pub fn format(
    project: &AnalysisProject,
    compact: bool,
    output: &Path,
    profile: query_engine::formatter::SarifProfile,
    dump_taint_graph: Option<&Path>,
) -> Result<(), Error> {
    let index_path = project.index_path()?;
    let query_path = project.query_path()?;
    {
        let ids = facts::IdMap::try_load(&index_path).err_context(|| "loading id map")?;
        let index_facts =
            IndexFacts::try_load(&index_path).err_context(|| "loading index facts")?;
        let index_result =
            IndexResult::try_load(&index_path).err_context(|| "loading index result")?;
        let taint_result =
            QueryResult::try_load(&query_path).err_context(|| "loading query result")?;

        let formal_params = taint_result.formal_param.clone();

        let mut b = query_engine::formatter::FormatFactsBuilder::default();
        b.taint(taint_result.taint)
            .formal_param(formal_params)
            .index_actual_param(index_facts.actual_param)
            .call(index_facts.call)
            .assign(index_result.assign_like)
            .paths(index_result.paths)
            .id_to_name(ids.get_id_to_name_map());
        let facts = b.build().unwrap();

        query_engine::formatter::format_sarif(project, facts.clone(), compact, output, profile)
            .err_context(|| "formatting sarif")?;

        if let Some(dot_path) = dump_taint_graph {
            let taint_results = query_engine::formatter::compute_taint_results(&facts);
            let edges = taint_results.edges;
            let nodes: BTreeSet<_> = facts
                .taint
                .iter()
                .map(|(func_id, _, var, path, _)| (*func_id, var.clone(), path.clone()))
                .collect();
            let nodes: Vec<_> = nodes.into_iter().collect();
            let sources: BTreeSet<_> = facts
                .taint
                .iter()
                .filter_map(|(_, _, _, _, ep)| {
                    if ep.direction == crate::facts::TaintDirection::Forward {
                        Some((ep.infunc, ep.vertex.0.clone(), ep.vertex.1.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let sinks: BTreeSet<_> = facts
                .taint
                .iter()
                .filter_map(|(_, _, _, _, ep)| {
                    if ep.direction == crate::facts::TaintDirection::Backward {
                        Some((ep.infunc, ep.vertex.0.clone(), ep.vertex.1.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let mut file = std::fs::File::create(dot_path).err_context(|| "creating dot file")?;
            let ids = facts::IdMap::try_load(&index_path)
                .err_context(|| "loading IdMap for taint graph")?;
            query_engine::graphviz::render_taint_graph(
                &nodes, &edges, &sources, &sinks, &ids, &mut file,
            )
            .err_context(|| "rendering taint graph")?;
            eprintln!("Wrote taint graph to '{}'", dot_path.display());
        }
    };
    if output.to_str() != Some("-") {
        eprintln!("Wrote '{}'", output.display());
    }
    Ok(())
}

pub fn save_program_info(
    mut program_info: ProgramInfo,
    import: &ArtifactImport,
) -> Result<(), Error> {
    let path = &import.program_path();
    let obj = std::mem::take(&mut program_info.program);
    let data = encode::encode_program(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing program: {}", path.display()))?;
    log::info!("wrote {}", path.display());

    let path = &import.vmt_path();
    let obj = std::mem::take(&mut program_info.vmt);
    let data = bitcode::serialize(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing vmt: {}", path.display()))?;
    log::info!("wrote {}", path.display());

    let path = import.source_info_dir();
    let obj = std::mem::take(&mut program_info.source_info);
    std::fs::create_dir_all(&path)?;
    source_info::write_parquet_source_info(&path, &obj)?;
    Ok(())
}

/// Load a serialized [`ProgramInfo`] from the import directory. The source info is elided.
fn load_program_info_without_source_info(import: &ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.program_path();
    log::info!("reading {}", path.display());
    let data = std::fs::read(path)?;
    let program = ctadl_ir::encode::decode_program(&data)?;

    let path = &import.vmt_path();
    log::info!("reading {}", path.display());
    let data = std::fs::read(path)?;
    let vmt = bitcode::deserialize(&data)?;

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
        for entry in fs::read_dir(import_path)? {
            let entry = entry?;
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
        for entry in fs::read_dir(projects_path)? {
            let entry = entry?;
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
        java_obj_assign,
        java_call,
        java_resolvents,
        summary,
        paths,
        taint,
        index_source_map,
        function_id
    );

    Ok(())
}

pub fn inspect_bitcode<P: AsRef<std::path::Path>>(path: P) -> Result<(), Error> {
    let path = path.as_ref();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Path {
            message: "invalid filename".to_string(),
        })?;

    let data = std::fs::read(path)?;
    if filename == "ir-program.bitcode" {
        let program = ctadl_ir::encode::decode_program(&data)?;
        println!("{}", program);
    } else if filename == "ir-vmt.bitcode" {
        let vmt: ctadl_ir::call::VirtualMethodTable = bitcode::deserialize(&data)?;
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
    log::info!("IndexFacts Statistics:");
    log::info!("  formal_param:   {}", facts.formal_param.len());
    log::info!("  actual_param:   {}", facts.actual_param.len());
    log::info!("  call:           {}", facts.call.len());
    log::info!("  assign:         {}", facts.assign.len());
    log::info!("  summary:        {}", facts.summary.len());
    log::info!("  paths:          {}", facts.paths.len());
    log::info!("  indirect_call:  {}", facts.indirect_call.len());
    log::info!("  java_call:      {}", facts.java_call.len());
    log::info!("  java_obj_assign:{}", facts.java_obj_assign.len());

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
    log::info!("\nTop {top_n} busiest call sites (by number of unique targets):");
    for (site, resolvents) in site_resolvents.iter().rev().take(top_n) {
        let num_resolvents = resolvents.len();
        let InsnSiteId { func_id, insn_id } = InsnSiteId::try_from(*site).unwrap();

        let func_name = id_map
            .and_then(|m| m.get_function(func_id))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");

        log::info!(
            "  Site in {func_name} ({}):{} has {num_resolvents} targets",
            func_id.id,
            insn_id.id
        );
        for target_id in resolvents.iter().take(3) {
            let target_name = id_map
                .and_then(|m| m.get_function(*target_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");
            log::info!("    -> {target_name} ({target_id:?})");
        }
        if num_resolvents > 3 {
            log::info!("    ... and {} more", num_resolvents - 3);
        }
    }

    let mut target_count_dist = HashMap::new();
    for (_, resolvents) in &site_resolvents {
        *target_count_dist.entry(resolvents.len()).or_insert(0) += 1;
    }
    let mut sorted_dist: Vec<_> = target_count_dist.into_iter().collect();
    sorted_dist.sort_by_key(|(count, _)| *count);
    log::info!("\nCall site target count distribution:");
    for (count, num_sites) in sorted_dist {
        log::info!("  {count} targets: {num_sites} sites");
    }

    // Assign analysis - which functions have most assigns?
    let mut func_assigns = HashMap::new();
    for (site, _, _) in &facts.assign {
        let InsnSiteId { func_id, .. } = InsnSiteId::try_from(*site).unwrap();
        *func_assigns.entry(func_id).or_insert(0) += 1;
    }
    let mut sorted_assigns: Vec<_> = func_assigns.into_iter().collect();
    sorted_assigns.sort_by_key(|(_, count)| *count);
    log::info!("\nTop 20 functions by number of assigns:");
    for (func_id, count) in sorted_assigns.iter().rev().take(20) {
        let func_name = id_map
            .and_then(|m| m.get_function(*func_id))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");
        log::info!("  {func_name} ({func_id:?}): {count} assigns");
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
    log::info!("\nPath length distribution:");
    for (len, count) in sorted_path_lens {
        let examples = &path_examples[&len];
        log::info!(
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
    log::info!("\nActual params per call site distribution:");
    for (count, num_sites) in sorted_actual_dist {
        log::info!("  {count} actuals: {num_sites} sites");
    }

    Ok(())
}
