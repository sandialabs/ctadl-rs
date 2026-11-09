/*! Check flowy programs

This module provides a function, [`check`], to check the assertions in a Flowy program. The
*import* half lives in [`ctadl_frontends::flowy`], which needs no engine; see there for why.
*/
use std::path::Path;

use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::{Error, ErrorContext};
use crate::facts as fx;
use crate::index_engine::{
    IndexConfig, IndexFacts, IndexResult, source_info::IndexSourceInfo, taint_index_with_config,
};
use crate::project::ArtifactImport;
use ctadl_flowy as flowy;
use ctadl_flowy::{EndpointRequires, FlowSpec, Port, PortBase, SummaryRequires, SummarySpec};
use ctadl_ir::cir::Variable;
use ctadl_ir::index::idx::Idx;

/// Loads flowy requirements for an import.
fn load_requirements(
    import: &ArtifactImport,
) -> Result<(SummaryRequires, EndpointRequires), Error> {
    let path = import.requirements_path();
    let data =
        std::fs::read(&path).err_context(|| format!("reading requirements: {}", path.display()))?;
    let reqs: (SummaryRequires, EndpointRequires) = bitcode::deserialize(&data)
        .err_context(|| format!("decoding requirements: {}", path.display()))?;
    Ok(reqs)
}

fn index_check_summaries(
    index_result: &IndexResult,
    summary_requires: SummaryRequires,
    sites: &fx::IdMap,
) -> Result<(usize, usize), Error> {
    let mut pass_count = 0;
    let mut fail_count = 0;

    for (func_name, flow_specs) in summary_requires.requires {
        for flow_spec in flow_specs.iter() {
            let SummarySpec {
                dest: dst_port,
                flow,
                source: src_port,
            } = flow_spec;
            let dst_binding = port_to_index(dst_port);
            let Ok((dst, dst_path)) = dst_binding else {
                log::warn!("{}", dst_binding.unwrap_err());
                continue;
            };
            let src_binding = port_to_index(src_port);
            let Ok((src, src_path)) = src_binding else {
                log::warn!("{}", src_binding.unwrap_err());
                continue;
            };
            let func_id = sites.get_function_id(func_name.clone().into());
            let Some(func_id) = func_id else {
                log::warn!("Function {func_name} not found in index");
                fail_count += 1;
                continue;
            };
            let record = (func_id, dst, dst_path, src, src_path);
            match flow {
                FlowSpec::FlowPresent => {
                    if !index_result.summary.contains(&record) {
                        fail_count += 1;
                        log::warn!(
                            "Function {func_name} required summary flow is absent: {flow_spec}"
                        );
                    } else {
                        pass_count += 1
                    }
                }
                FlowSpec::FlowAbsent => {
                    if index_result.summary.contains(&record) {
                        fail_count += 1;
                        log::warn!(
                            "Function {func_name} forbidden summary flow is present: {flow_spec}"
                        );
                    } else {
                        pass_count += 1;
                    }
                }
            }
        }
    }
    Ok((pass_count, fail_count))
}

/// Checks summary requirements for a flowy import.
pub fn index_check(
    import: &ArtifactImport,
    index_result: &IndexResult,
    sites: &fx::IdMap,
) -> Result<(usize, usize), Error> {
    let (summary_requires, _) = load_requirements(import)?;
    index_check_summaries(index_result, summary_requires, sites)
}

/// Check a flowy program's summary requirements against the index, and print errors.
pub fn check<P: AsRef<Path>>(
    file: P,
    dump_index_graph: Option<&Path>,
    models: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    check_with_config(file, dump_index_graph, models, IndexConfig::default())
}

/// [`check`] under an explicit index configuration, so a fixture's assertions can be run under
/// every hybrid-context mode and context join.
pub fn check_with_config<P: AsRef<Path>>(
    file: P,
    dump_index_graph: Option<&Path>,
    models: &[std::path::PathBuf],
    config: IndexConfig,
) -> anyhow::Result<()> {
    let file = file.as_ref();
    let program = flowy::compile_program(file)?;
    let mut pass_count = 0;
    let mut fail_count = 0;

    // Models are loaded before `codegen_program`, which consumes the `ProgramInfo`. A flowy
    // import has `Dispatch::Unknown` and names each function by its IR name, so a generator
    // matches by the IR function name directly and gets no default models of its own. That
    // makes flowy the cheapest place to pin what a *model port* means, which is what
    // `port_semantics/` uses it for.
    let match_index = crate::models::ProgramMatchIndex::new(
        &program.program_info,
        crate::models::ImportScope {
            language: Some(crate::project::ArtifactLanguage::Flowy),
            import: None,
        },
    );
    // One accumulator across every model file, and one phase-2 run over it below. A flowy
    // check has a single program and no import loop, so there is no ordering hazard here --
    // but `Argument(*)` still expands over `compute_arg_arity` when phase 2 runs, and running
    // it once is what keeps that the same expansion the index path performs.
    let mut model_matches = crate::models::ProgramModelMatches::default();
    for model_path in models {
        crate::models::try_load_models(&match_index, model_path, &mut model_matches)?;
    }
    drop(match_index);

    let mut index_facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program.program_info,
        &mut index_facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        crate::codegen::CallPolicy::default(),
        &model_matches,
    );
    crate::codegen::model_matches::codegen_model_matches(
        &model_matches,
        &mut index_facts,
        &mut source_info,
    );
    log::debug!("Function ID to Name mapping:");
    for (id, name) in source_info.sites.functions() {
        log::debug!("{}: {}", id.id, name.0);
    }
    log::trace!("requirements: {}", program.requirements);
    crate::cli::inspect_index_facts(&index_facts, Some(&source_info.sites))?;
    let index_result = taint_index_with_config(index_facts, config, Some(&source_info.sites));
    if let Some(dot_path) = dump_index_graph {
        let mut file = std::fs::File::create(dot_path)
            .err_context(|| format!("creating dot file: {}", dot_path.display()))?;
        crate::graphviz::render_index_graph(
            &index_result.assign_like,
            &source_info.sites,
            &mut file,
        )
        .err_context(|| format!("rendering index graph: {}", dot_path.display()))?;
        log::info!("Wrote index graph to '{}'", dot_path.display());
    }

    // `#value` is terminal (`ctadl_ir::cir::PathSegment::Value`).
    for (p,) in &index_result.paths {
        let segments: Vec<_> = p.iter().collect();
        if let Some(i) = segments.iter().position(|s| s.is_value())
            && i + 1 != segments.len()
        {
            log::warn!("admissible path with a segment after #value: {p}");
            fail_count += 1;
        }
    }

    let (ipass, ifail) = index_check_summaries(
        &index_result,
        program.requirements.summary_requires,
        &source_info.sites,
    )?;
    pass_count += ipass;
    fail_count += ifail;

    if fail_count > 0 {
        anyhow::bail!(
            "Flowy program verification failed: {} checks passed, {} failed",
            pass_count,
            fail_count
        );
    }
    println!("{} checks passed, {} failed", pass_count, fail_count);
    Ok(())
}

fn port_to_index(port: &Port) -> anyhow::Result<(fx::Root, fx::Path)> {
    let Port { base, fields } = port;
    let path = fx::Path::from_accesses(fields.iter().cloned());
    match base {
        PortBase::Return => Ok(((-1i16).into(), path)),
        PortBase::Var(v) => match &v.variable {
            Variable::Param(idx) => Ok((fx::FormalIndex::try_from(idx.index())?.into(), path)),
            Variable::Local(_) => {
                panic!("summary requires refers to local")
            }
            Variable::Global(_) => {
                unreachable!("flowy names a global port by name, `PortBase::Global`")
            }
        },
        PortBase::Global(name) => Ok((fx::GlobalId(fx::Str::from(name.clone())).into(), path)),
    }
}
