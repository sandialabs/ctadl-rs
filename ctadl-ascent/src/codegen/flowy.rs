/*! Check flowy programs

This module provides a function, [`check`], to check the assertions in a Flowy program.
*/
use std::path::Path;

use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::{Error, ErrorContext};
use crate::facts as fx;
use crate::index_engine::{IndexFacts, IndexResult, source_info::IndexSourceInfo, taint_index};
use crate::project::ArtifactImport;
use crate::query_engine::{QueryEndpoint, QueryFacts, QueryResult, taint_analysis};
use ctadl_flowy as flowy;
use ctadl_flowy::{EndpointRequires, FlowSpec, Port, PortBase, SummaryRequires, SummarySpec};
use ctadl_ir::ProgramInfo;
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::Variable;

/// Imports a flowy artifact into the store. This also saves the requirements so that they can be
/// checked at query time.
pub fn import(import: &ArtifactImport) -> Result<ProgramInfo, Error> {
    let program = flowy::compile_program(&import.artifact_path)?;

    // Save requirements
    let data = bitcode::serialize(&program.requirements).map_err(Error::Bitcode)?;
    std::fs::write(import.requirements_path(), data)
        .map_err(Error::Io)
        .err_context(|| {
            format!(
                "writing requirements: {}",
                import.requirements_path().display()
            )
        })?;

    Ok(program.program_info)
}

/// Loads flowy requirements for an import.
fn load_requirements(
    import: &ArtifactImport,
) -> Result<(SummaryRequires, EndpointRequires), Error> {
    let data = std::fs::read(import.requirements_path())?;
    let reqs: (SummaryRequires, EndpointRequires) = bitcode::deserialize(&data)?;
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
                        println!(
                            "Function {func_name} required summary flow is absent: {flow_spec}"
                        );
                    } else {
                        pass_count += 1
                    }
                }
                FlowSpec::FlowAbsent => {
                    if index_result.summary.contains(&record) {
                        fail_count += 1;
                        println!(
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

/// Returns query endpoints for a flowy import.
pub fn get_endpoints(
    import: &ArtifactImport,
    sites: &fx::IdMap,
) -> Result<Vec<(QueryEndpoint,)>, Error> {
    let (_, endpoint_requires) = load_requirements(import)?;
    let endpoints = endpoint_requires
        .requires
        .iter()
        .flat_map(|(_k, v)| v.iter().map(|(ep, _)| ep))
        .map(|e| (from_flowy_endpoint(sites, e),))
        .collect();
    Ok(endpoints)
}

fn query_check_endpoints(
    query_result: &QueryResult,
    endpoint_requires: EndpointRequires,
    sites: &fx::IdMap,
) -> Result<(usize, usize), Error> {
    let mut pass_count = 0;
    let mut fail_count = 0;
    for (func_name, flow_specs) in endpoint_requires.requires {
        for (endpoint, flow_spec) in flow_specs.iter() {
            let fx_endpoint: fx::TaintEndpoint = endpoint.into();
            let func_id = sites.get_function_id(func_name.clone().into());
            let Some(func_id) = func_id else {
                log::warn!("Function {func_name} not found in query results");
                fail_count += 1;
                continue;
            };

            let present = query_result.taint.iter().any(|r| {
                r.0 == func_id
                    && r.4.label == fx_endpoint.label
                    && r.4.direction == fx_endpoint.direction.reversed()
                    && r.2 == fx_endpoint.vertex.0
                    && r.3 == fx_endpoint.vertex.1
            });

            match flow_spec {
                FlowSpec::FlowPresent => {
                    if !present {
                        fail_count += 1;
                        println!("Required endpoint not found: {}", fx_endpoint.reversed());
                    } else {
                        pass_count += 1;
                    }
                }
                FlowSpec::FlowAbsent => {
                    if present {
                        fail_count += 1;
                        println!("Forbidden endpoint is present: {}", fx_endpoint.reversed());
                    } else {
                        pass_count += 1;
                    }
                }
            }
        }
    }
    Ok((pass_count, fail_count))
}

/// Checks endpoint requirements for a flowy import.
pub fn query_check(
    import: &ArtifactImport,
    query_result: &QueryResult,
    sites: &fx::IdMap,
) -> Result<(usize, usize), Error> {
    let (_, endpoint_requires) = load_requirements(import)?;
    query_check_endpoints(query_result, endpoint_requires, sites)
}

/// Check a flowy program, running the ctadl index and query steps, and print errors.
pub fn check<P: AsRef<Path>>(file: P, dump_object_graph: Option<&Path>) -> anyhow::Result<()> {
    let file = file.as_ref();
    let program = flowy::compile_program(file)?;
    let mut pass_count = 0;
    let mut fail_count = 0;

    let mut index_facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program.program_info,
        &mut index_facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
    );
    log::debug!("Function ID to Name mapping:");
    for (id, name) in source_info.sites.functions() {
        log::debug!("{}: {}", id.id, name.0);
    }
    log::trace!("requirements: {}", program.requirements);
    crate::cli::inspect_index_facts(&index_facts, Some(&source_info.sites))?;
    let endpoints = program
        .requirements
        .endpoint_requires
        .requires
        .iter()
        .flat_map(|(_k, v)| v.iter().map(|(ep, _)| ep))
        .map(|e| (from_flowy_endpoint(&source_info.sites, e),))
        .collect();
    let index_result = taint_index(index_facts.clone());

    if let Some(dot_path) = dump_object_graph {
        use crate::error::ErrorContext;
        let mut f =
            std::fs::File::create(dot_path).err_context(|| "creating object graph dot file")?;
        crate::index_engine::graphviz::render_object_graph(
            &index_result.vtx_points_to,
            &index_result.fld_points_to,
            &source_info.sites,
            &mut f,
        )
        .err_context(|| "rendering object graph")?;
        eprintln!("Wrote object graph to '{}'", dot_path.display());
    }

    let (ipass, ifail) = index_check_summaries(
        &index_result,
        program.requirements.summary_requires,
        &source_info.sites,
    )?;
    pass_count += ipass;
    fail_count += ifail;
    let query_facts = QueryFacts {
        formal_param: index_facts.formal_param,
        actual_param: index_facts.actual_param,
        call: index_facts.call,
        assign: index_result.assign_like,
        paths: index_facts.paths,
        endpoints,
    };
    let query_result = taint_analysis(query_facts);
    let (ipass, ifail) = query_check_endpoints(
        &query_result,
        program.requirements.endpoint_requires,
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

//fn from_endpoint(sites: &fx::IdMap, endpoint: &flowy::Endpoint) -> fx::TaintEndpoint2 {
//    //let infunc = sites.get_function(endpoint.infunc.clone().into()).unwrap();
//    let vertex = {
//        let (var, fields) = &endpoint.port;
//        fx::FlowVertex2(var.try_into().unwrap(), fields.into())
//    };
//    fx::TaintEndpoint2 {
//        infunc: endpoint.infunc.clone().into(),
//        vertex,
//        label: fx::Label(endpoint.label.clone().into()),
//        direction: match endpoint.direction {
//            flowy::EndpointDirection::Source => fx::TaintDirection::Forward,
//            flowy::EndpointDirection::Sink => fx::TaintDirection::Backward,
//        },
//    }
//}

impl From<&flowy::Endpoint> for fx::TaintEndpoint {
    #[inline]
    fn from(e: &flowy::Endpoint) -> Self {
        use flowy::*;
        use fx::*;
        let vertex = {
            let (var, fields) = &e.port;
            FlowVertex(var.try_into().unwrap(), fields.into())
        };
        Self {
            infunc: Function(e.infunc.clone()),
            vertex,
            label: Label(e.label.clone().into()),
            direction: match e.direction {
                EndpointDirection::Source => TaintDirection::Forward,
                EndpointDirection::Sink => TaintDirection::Backward,
            },
        }
    }
}

fn from_flowy_endpoint(sites: &fx::IdMap, endpoint: &flowy::Endpoint) -> QueryEndpoint {
    use flowy::*;
    use fx::*;
    let infunc = sites
        .get_function_id(endpoint.infunc.clone().into())
        .unwrap();
    let vertex = {
        let (var, fields) = &endpoint.port;
        FlowVertex(var.try_into().unwrap(), fields.into())
    };
    QueryEndpoint {
        infunc,
        vertex,
        label: Label(endpoint.label.clone().into()),
        direction: match endpoint.direction {
            EndpointDirection::Source => TaintDirection::Forward,
            EndpointDirection::Sink => TaintDirection::Backward,
        },
    }
}

fn port_to_index(port: &Port) -> anyhow::Result<(fx::FormalIndex, fx::Path)> {
    let Port { base, fields } = port;
    match base {
        PortBase::Return => Ok(((-1i16).into(), fields.into())),
        PortBase::Var(v) => match v.variable.as_ref() {
            Variable::Param(idx) => Ok((idx.index().try_into().unwrap(), fields.into())),
            Variable::Local(_) => {
                panic!("summary requires refers to local")
            }
            Variable::GlobalHeap => anyhow::bail!("global found in summary, not yet checked"),
        },
    }
}
