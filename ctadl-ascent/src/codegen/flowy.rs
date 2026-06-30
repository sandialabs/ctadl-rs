/*! Check flowy programs

This module provides a function, [`check`], to check the assertions in a Flowy program.
*/
use std::path::Path;

use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::{Error, ErrorContext};
use crate::facts as fx;
use crate::index_engine::{
    IndexConfig, IndexFacts, IndexResult, source_info::IndexSourceInfo, taint_index_with_config,
};
use crate::project::ArtifactImport;
use crate::query_engine::formatter;
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
    call: &[(fx::PackedInsnSiteId, fx::FunctionId)],
) -> Result<Vec<(QueryEndpoint,)>, Error> {
    let (_, endpoint_requires) = load_requirements(import)?;
    let endpoints = endpoint_requires
        .requires
        .iter()
        .flat_map(|(_k, v)| v.iter().map(|(ep, _)| ep))
        .flat_map(|e| {
            from_flowy_endpoint(sites, call, e)
                .into_iter()
                .map(|ep| (ep,))
        })
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

/// Checks the human SARIF profile: every declared source/sink pair must agree
/// with the formatter's path output.
///
/// A flow that is required to be present (`FlowSpec::FlowPresent`, i.e.
/// `source`/`sink`) must surface as a source -> sink path: every such sink must
/// be the terminus of at least one human-profile path, and every such source
/// must begin one. A flow that is forbidden (`FlowSpec::FlowAbsent`, i.e.
/// `errsource`/`errsink`) must *not* appear in any path. This runs the very
/// path-finding that `format_sarif` uses to build the human-profile
/// `tainted-path` results (`compute_taint_results` + `find_endpoint_paths`), so
/// the formatter is exercised directly.
fn check_human_profile_paths(
    format_facts: &crate::query_engine::formatter::FormatFacts,
    endpoint_requires: &EndpointRequires,
    sites: &fx::IdMap,
) -> (usize, usize) {
    use flowy::EndpointDirection;

    let taint_results = formatter::compute_taint_results(format_facts);
    let paths = formatter::find_endpoint_paths(format_facts, &taint_results);

    // A declared source/sink pair shares a taint label, so only consider paths
    // whose source and sink carry the same label. The taint graph itself is
    // label-agnostic (its nodes are `(function, variable, path)`), so the
    // path-finder can connect, say, a `Hit` source to a node that is *also* an
    // `Extra` sink. Those cross-label paths are not the flow a declared pair
    // names, and filtering them here is what lets an `errsink` on a variable
    // that legitimately sinks a *different* label still read as "absent".
    let labeled: Vec<&formatter::EndpointPath> = paths
        .iter()
        .filter(|p| p.source.label == p.sink.label)
        .collect();

    let mut pass_count = 0;
    let mut fail_count = 0;
    for flow_specs in endpoint_requires.requires.values() {
        for (endpoint, flow_spec) in flow_specs {
            // Resolve the declared endpoint to the same QueryEndpoint the query
            // ran with. Skip if its function never made it into the index --
            // `query_check_endpoints` already reports that as a failure.
            if sites
                .get_function_id(endpoint.infunc.clone().into())
                .is_none()
            {
                continue;
            }
            // Resolve the declaration to the same call-site-anchored endpoints the
            // query seeded: a declaration on a callee's formal fans out to one
            // endpoint per call site. Count how many distinct human-profile paths
            // touch *any* of them -- that is what a declared `path_count` (the
            // trailing integer on `source`/`sink`) asserts, and the two flows that
            // differ only in their call site now land on distinct call-arg
            // endpoints instead of collapsing onto the shared formal vertex.
            let qes: std::collections::HashSet<QueryEndpoint> =
                from_flowy_endpoint(sites, &format_facts.call, endpoint)
                    .into_iter()
                    .collect();
            let (kind, path_hits) = match endpoint.direction {
                EndpointDirection::Source => (
                    "source",
                    labeled.iter().filter(|p| qes.contains(&p.source)).count(),
                ),
                EndpointDirection::Sink => (
                    "sink",
                    labeled.iter().filter(|p| qes.contains(&p.sink)).count(),
                ),
            };
            let on_path = path_hits > 0;
            match flow_spec {
                FlowSpec::FlowPresent => {
                    if let Some(expected) = endpoint.path_count {
                        // The endpoint declared an exact number of paths to find.
                        if path_hits == expected {
                            pass_count += 1;
                        } else {
                            fail_count += 1;
                            println!(
                                "Human profile: expected {expected} path(s) for {kind} endpoint \
                                 but found {path_hits}: {endpoint}"
                            );
                        }
                    } else if on_path {
                        pass_count += 1;
                    } else {
                        fail_count += 1;
                        println!(
                            "Human profile: no path found for required {kind} endpoint: {endpoint}"
                        );
                    }
                }
                FlowSpec::FlowAbsent => {
                    if on_path {
                        fail_count += 1;
                        println!(
                            "Human profile: forbidden {kind} endpoint appears on a path: {endpoint}"
                        );
                    } else {
                        pass_count += 1;
                    }
                }
            }
        }
    }
    (pass_count, fail_count)
}

/// Check a flowy program, running the ctadl index and query steps, and print errors.
pub fn check<P: AsRef<Path>>(file: P, dump_index_graph: Option<&Path>) -> anyhow::Result<()> {
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
        .flat_map(|e| {
            from_flowy_endpoint(&source_info.sites, &index_facts.call, e)
                .into_iter()
                .map(|ep| (ep,))
        })
        .collect();
    let index_result = taint_index_with_config(
        index_facts.clone(),
        IndexConfig::default(),
        Some(&source_info.sites),
    );

    if let Some(dot_path) = dump_index_graph {
        let mut file = std::fs::File::create(dot_path).err_context(|| "creating dot file")?;
        crate::graphviz::render_index_graph(
            &index_result.assign_like,
            &source_info.sites,
            &mut file,
        )
        .err_context(|| "rendering index graph")?;
        eprintln!("Wrote index graph to '{}'", dot_path.display());
    }

    let (ipass, ifail) = index_check_summaries(
        &index_result,
        program.requirements.summary_requires,
        &source_info.sites,
    )?;
    pass_count += ipass;
    fail_count += ifail;

    // Build the format facts now, cloning the index-derived facts before the
    // query consumes them. These feed the human-profile path check below, which
    // runs the same formatter path-finding `format_sarif` uses.
    let mut format_facts_builder = formatter::FormatFactsBuilder::default();
    format_facts_builder
        .index_actual_param(index_facts.actual_param.clone())
        .call(index_facts.call.clone())
        .assign(index_result.assign_like.clone())
        .paths(index_result.paths.clone())
        .external_function(index_result.external_function.clone())
        .id_to_name(source_info.sites.get_id_to_name_map());

    let query_facts = QueryFacts {
        formal_param: index_facts.formal_param,
        actual_param: index_facts.actual_param,
        call: index_facts.call,
        assign: index_result.assign_like,
        paths: index_facts.paths,
        endpoints,
    };
    let query_result = taint_analysis(query_facts, Some(&source_info.sites));

    // The human-profile path check needs the declared endpoints; clone them
    // before `query_check_endpoints` consumes the requirements.
    let endpoint_requires = program.requirements.endpoint_requires.clone();
    let (ipass, ifail) = query_check_endpoints(
        &query_result,
        program.requirements.endpoint_requires,
        &source_info.sites,
    )?;
    pass_count += ipass;
    fail_count += ifail;

    // Human-profile formatter check: every declared source/sink pair that is
    // required to flow must surface as a source -> sink path in the human SARIF
    // profile.
    let format_facts = format_facts_builder
        .taint(query_result.taint.clone())
        .formal_param(query_result.formal_param.clone())
        .build()
        .expect("building format facts");
    let (hpass, hfail) =
        check_human_profile_paths(&format_facts, &endpoint_requires, &source_info.sites);
    pass_count += hpass;
    fail_count += hfail;

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
            infunc: Function(e.infunc.clone().into()),
            vertex,
            label: Label(e.label.clone().into()),
            direction: match e.direction {
                EndpointDirection::Source => TaintDirection::Forward,
                EndpointDirection::Sink => TaintDirection::Backward,
            },
        }
    }
}

/// Builds the function-anchored query endpoint a flowy `source`/`sink` declares, before any
/// call-site fanning.
fn flowy_endpoint_base(sites: &fx::IdMap, endpoint: &flowy::Endpoint) -> QueryEndpoint {
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
        call_site: None,
    }
}

/// Resolves a flowy `source`/`sink` declaration to the query endpoints it seeds and checks
/// against. A declaration on a parameter fans out to one endpoint per call site of its function
/// (so flows that differ only by call site stay distinct); a declaration on a local (e.g. a
/// `source`'s returned value), a global, or a function with no callers stays function-anchored.
/// The parameter index is taken from `endpoint.formal` (resolved by the flowy front-end), since
/// SSA has versioned the port into a local. The same set is used for both seeding and requirement
/// checking, so a path's endpoint compares equal to a declared one.
fn from_flowy_endpoint(
    sites: &fx::IdMap,
    call: &[(fx::PackedInsnSiteId, fx::FunctionId)],
    endpoint: &flowy::Endpoint,
) -> Vec<QueryEndpoint> {
    let base = flowy_endpoint_base(sites, endpoint);
    match endpoint.formal {
        Some(formal) => base.anchored_at_callsites(fx::FormalIndex::from(formal), call),
        None => vec![base],
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
