//! In-process driver for the DFSan dynamic/static taint comparison harness.
//!
//! [`analyze_c_flows`] runs CTADL's full source→sink taint query on a single C
//! program string plus a model file (the same `.json` model format used by the
//! CLI, e.g. `tests/c/xfer.json`), and returns the set of source→sink flows the
//! static analysis reports. The dynamic side (LLVM DFSan) produces a comparable
//! set at runtime; the comparator diffs the two.
//!
//! This mirrors the index+query pipeline in [`crate::cli::query`] and
//! [`crate::codegen::flowy::check`], but runs entirely in memory (no project
//! store on disk) so a harness can evaluate many programs quickly.

use ctadl_ir::{ProgramInfo, ssa};

use crate::cli::build_query_endpoints;
use crate::codegen::models::codegen_summary;
use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::Error;
use crate::facts::TaintDirection;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{IndexConfig, IndexFacts, taint_index_with_config};
use crate::languages::tree_sitter::parse_c_program;
use crate::models::{ModelsBatch, try_load_models};
use crate::query_engine::{QueryFacts, taint_analysis};

/// A single source→sink flow that CTADL reports statically: taint of `label`
/// reaches the sink vertex (`sink_function` + `sink_path`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StaticFlow {
    pub sink_function: String,
    /// Dotted access path at the sink vertex, e.g. "" or ".inner".
    pub sink_path: String,
    pub label: String,
}

/// Parse `src`, index it, and run the taint query using the source/sink model at
/// `model_path`. Returns the deduplicated, sorted set of reported flows.
pub fn analyze_c_flows(
    src: &str,
    model_path: impl AsRef<std::path::Path>,
) -> Result<Vec<StaticFlow>, Error> {
    // 1. Parse C to IR.
    let (program, has_error, _dump) = parse_c_program(src)?;
    if has_error {
        return Err(Error::TreeSitterParse(
            "tree-sitter reported a parse error in the input program".to_owned(),
        ));
    }
    let mut program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    program_info.program.verify()?;

    // 2. Load the source/sink model against the program (needs program_info
    //    before codegen consumes it). Split summary (consumed by indexing) from
    //    endpoints (used to build the query).
    let ModelsBatch {
        summary, endpoint, ..
    } = try_load_models(&program_info, model_path.as_ref())?;

    // 3. Index: SSA → codegen facts → fold in model summaries → datalog index.
    ssa::transform_program(&mut program_info.program, true);
    let mut index_facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut index_facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
    );
    codegen_summary(summary, &mut index_facts, &mut source_info);
    let index_result = taint_index_with_config(
        index_facts.clone(),
        IndexConfig::default(),
        Some(&source_info.sites),
    );

    // 4. Build query endpoints (sources + sinks) from the model.
    let (endpoints, model_formals) =
        build_query_endpoints(&endpoint, &index_facts, &source_info.sites);
    let mut formal_params = index_facts.formal_param.clone();
    formal_params.extend(model_formals);

    {
        let n_src = endpoints
            .iter()
            .filter(|(e,)| e.direction == TaintDirection::Forward)
            .count();
        let n_sink = endpoints
            .iter()
            .filter(|(e,)| e.direction == TaintDirection::Backward)
            .count();
        log::debug!(
            "taint_compare: built {} endpoints ({} sources, {} sinks); functions: {:?}",
            endpoints.len(),
            n_src,
            n_sink,
            source_info
                .sites
                .functions()
                .map(|(_, f)| f.0.to_string())
                .collect::<Vec<_>>(),
        );
    }

    // 5. Run the source/sink taint query.
    let query_facts = QueryFacts {
        formal_param: formal_params,
        actual_param: index_facts.actual_param,
        call: index_facts.call,
        assign: index_result.assign_like,
        paths: index_result.paths,
        endpoints: endpoints.clone(),
    };
    let query_result = taint_analysis(query_facts, Some(&source_info.sites));

    // 6. For each sink endpoint, a flow is present when forward taint carrying
    //    the matching label reached the sink's vertex (same predicate as
    //    flowy::query_check_endpoints).
    let mut flows = Vec::new();
    for (ep,) in &endpoints {
        if ep.direction != TaintDirection::Backward {
            continue;
        }
        let present = query_result.taint.iter().any(|r| {
            r.0 == ep.infunc
                && r.4.label == ep.label
                && r.4.direction == TaintDirection::Forward
                && r.2 == ep.vertex.0
                && r.3 == ep.vertex.1
        });
        if present {
            let sink_function = source_info
                .sites
                .get_function(ep.infunc)
                .map(|f| f.0.to_string())
                .unwrap_or_else(|| format!("<func#{}>", ep.infunc.id));
            flows.push(StaticFlow {
                sink_function,
                sink_path: ep.vertex.1.to_dot_string(),
                label: ep.label.0.to_string(),
            });
        }
    }
    flows.sort();
    flows.dedup();
    Ok(flows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_c_path(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("c");
        p.push(name);
        p
    }

    /// M1 acceptance: a direct source() → sink() flow is reported when driven
    /// through the in-process pipeline.
    #[test_log::test]
    fn direct_flow_is_reported() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                sink(s);
                return 0;
            }
        "#;
        let flows = analyze_c_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("direct flows: {flows:?}");
        // The model sink `sink(Argument(0))` is anchored at the *call site* in `main`
        // (not the `sink` callee): model endpoints on formals fan out to their callers'
        // call-arg vertices so flows that differ by call site stay distinct (see
        // QueryEndpoint::anchored_at_callsites). Every flow analyze_c_flows returns is a
        // sink flow by construction, so we assert the source label reached one — the same
        // predicate the harness itself keys on (ctadl-dynamic compare_program) — rather
        // than the anchoring function's name.
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow (label Test), got: {flows:?}"
        );
    }

    /// Negative control: a program with a source and a sink but no data path
    /// between them must report no flow.
    #[test_log::test]
    fn no_flow_when_disconnected() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                int x = 0;
                sink(x);
                return 0;
            }
        "#;
        let flows = analyze_c_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("disconnected flows: {flows:?}");
        assert!(flows.is_empty(), "expected no flow, got: {flows:?}");
    }
}
