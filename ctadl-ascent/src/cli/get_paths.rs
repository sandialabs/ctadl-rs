/*! Taint paths through a program point named by binary URI and byte offset.

This backs `ctadl get-paths`, which an editor integration calls to ask "what taint paths run
through this address?". Each `BINARY_URI,BYTE_OFFSET` pair is resolved to the instructions the
index recorded at that offset, those instructions' flow vertices become extra query endpoints
alongside the project's model-defined sources and sinks, and the taint graph is searched for a
path joining a target to a sink (forward) or a source to a target (backward). The result is
JSON on stdout: one entry per path, each a list of edges carrying the variable, method, class,
access path, and byte offset an editor needs to draw the flow.
*/

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use datafusion::arrow::array::{StringViewArray, UInt32Array, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;
use datafusion::prelude::*;
use packed_struct::prelude::*;
use serde::Serialize;

use crate::error::Error;
use crate::facts::{
    FlowVariable, FlowVariableKind, FunctionId, InsnId, InsnSiteId, Label, Path,
    TaintDirection as FactsTaintDirection, TaintState,
};
use crate::index_engine::{IndexFacts, IndexResult};
use crate::query_engine::formatter::{
    FormatFactsBuilder, TaintAnalysisResults, build_taint_flow_graph,
};
use crate::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};
use ctadl_ir::graph::{DirectedGraph, LabeledSuccessors, find_annotated_path_to_set};

/// Which taint directions to search from the queried program points.
#[derive(Debug, Clone, clap::ValueEnum, Copy)]
pub enum TaintDirection {
    All,
    Fwd,
    Bwd,
}

/// One `BINARY_URI,BYTE_OFFSET` pair off the command line.
#[derive(Debug, Clone)]
pub struct OffsetQuery {
    pub binary_uri: String,
    pub byte_offset: u64,
}

fn parse_pair(input: &str) -> Result<OffsetQuery, Error> {
    let Some(idx) = input.rfind(',') else {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid pair '{input}': expected binary_uri,byte_offset"),
        )));
    };
    let (uri, off_str_with_comma) = input.split_at(idx);
    let off_str = &off_str_with_comma[1..];
    if uri.is_empty() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid pair '{input}': empty binary_uri"),
        )));
    }
    // Hex needs the `0x` prefix. Disassemblers print addresses bare, but a bare number is
    // already a decimal offset here, and `1505856` is a valid figure in both bases naming two
    // different addresses -- so the prefix is what makes the base explicit rather than guessed.
    let byte_offset = match off_str
        .strip_prefix("0x")
        .or_else(|| off_str.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => off_str.parse::<u64>(),
    }
    .map_err(|e| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid pair '{input}': failed to parse byte_offset '{off_str}': {e}"),
        ))
    })?;
    Ok(OffsetQuery {
        binary_uri: uri.to_string(),
        byte_offset,
    })
}

#[derive(Serialize)]
pub struct CtadlNodeInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ap: Option<String>,
}

#[derive(Serialize)]
pub struct CtadlDataResult {
    #[serde(skip_serializing_if = "Option::is_none", rename = "byteOffset")]
    pub byte_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "inNode")]
    pub in_node: Option<CtadlNodeInfo>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "outNode")]
    pub out_node: Option<CtadlNodeInfo>,
}

#[derive(Serialize)]
pub struct Output {
    pub fwd: Vec<Vec<CtadlDataResult>>,
    pub bwd: Vec<Vec<CtadlDataResult>>,
}

/// Find the taint paths through each `BINARY_URI,BYTE_OFFSET` pair and write them to stdout
/// as JSON.
///
/// `sources` and `sinks` are optional `call-arg(INSN, FORMAL)` endpoints. Giving any source
/// replaces the model-defined sources, and giving any sink replaces the model-defined sinks.
///
/// # Errors
///
/// If the project or its index cannot be read, a pair is malformed, or the analysis fails.
pub fn get_paths(
    project_name: &str,
    pairs: &[String],
    direction: TaintDirection,
    sources: &[String],
    sinks: &[String],
) -> Result<(), Error> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run(project_name, pairs, direction, sources, sinks))
}

async fn run(
    project_name: &str,
    pairs: &[String],
    direction: TaintDirection,
    sources: &[String],
    sinks: &[String],
) -> Result<(), Error> {
    let queries: Vec<OffsetQuery> = pairs
        .iter()
        .map(|s| parse_pair(s))
        .collect::<Result<_, _>>()?;

    let project = match crate::project::AnalysisProject::try_load_name(project_name) {
        Ok(p) => p,
        Err(e) => {
            let mut err_msg = format!("{}", e);
            let mut current_err: &dyn std::error::Error = &e;
            while let Some(source) = current_err.source() {
                err_msg.push_str(&format!(": {}", source));
                current_err = source;
            }
            eprintln!("Error loading project '{}': {}", project_name, err_msg);
            eprintln!("This may be missing if run on a different machine or as a different user.");
            eprintln!("Changing XDG_STATE_HOME can override the default state path.");
            std::process::exit(1);
        }
    };
    let parquet_index_path = project.index_path()?;

    let mut parquet_source_info_path = PathBuf::new();
    for import_name in &project.imports {
        let import = match crate::project::ArtifactImport::load_by_name(import_name) {
            Ok(i) => i,
            Err(e) => {
                let mut err_msg = format!("{}", e);
                let mut current_err: &dyn std::error::Error = &e;
                while let Some(source) = current_err.source() {
                    err_msg.push_str(&format!(": {}", source));
                    current_err = source;
                }
                eprintln!("Error loading import '{}': {}", import_name, err_msg);
                eprintln!(
                    "This may be missing if run on a different machine or as a different user."
                );
                eprintln!("Changing XDG_STATE_HOME can override the default state path.");
                std::process::exit(1);
            }
        };
        parquet_source_info_path = import.source_info_dir();
    }
    if parquet_source_info_path.as_os_str().is_empty() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No imports found in project to get source info from",
        )));
    }

    let index_facts = IndexFacts::try_load(&parquet_index_path)?;
    let index_result = IndexResult::try_load(&parquet_index_path)?;

    let ctx = SessionContext::new();
    ctx.register_parquet(
        "index_source_map",
        parquet_index_path
            .join("index_source_map.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "file_spans",
        parquet_source_info_path
            .join("file_spans.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "spans",
        parquet_source_info_path
            .join("spans.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "files",
        parquet_source_info_path
            .join("files.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "artifacts",
        parquet_source_info_path
            .join("artifacts.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;

    let all_arts_df = ctx
        .sql("SELECT canonical_path FROM artifacts ORDER BY canonical_path")
        .await?
        .collect()
        .await?;
    let mut artifact_paths = Vec::new();
    for batch in all_arts_df {
        let cols = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            artifact_paths.push(cols.value(i).to_string());
        }
    }

    let mut matched_queries = queries.clone();
    for q in &mut matched_queries {
        let q_uri = &q.binary_uri;
        let mut best_match = None;
        let mut max_match_len = 0;

        for art in &artifact_paths {
            if art == q_uri {
                best_match = Some(art.clone());
                break;
            }
            if art.ends_with(q_uri) {
                best_match = Some(art.clone());
                break;
            }
            if q_uri.ends_with(art) {
                best_match = Some(art.clone());
                break;
            }

            // if neither is a suffix of the other, compare from the end
            let art_parts: Vec<_> = art.split('/').rev().collect();
            let q_parts: Vec<_> = q_uri.split('/').rev().collect();

            let mut match_len = 0;
            for (a, b) in art_parts.iter().zip(q_parts.iter()) {
                if a == b {
                    match_len += 1;
                } else {
                    break;
                }
            }
            if match_len > max_match_len {
                max_match_len = match_len;
                best_match = Some(art.clone());
            } else if match_len == max_match_len {
                // Deterministic tie-break: pick lexicographically smallest artifact path.
                if best_match.as_ref().is_none_or(|b| art < b) {
                    best_match = Some(art.clone());
                }
            }
        }

        if let Some(best) = best_match {
            q.binary_uri = best;
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("uri", DataType::Utf8View, false),
        Field::new("offset", DataType::UInt64, false),
    ]));
    let uri_array = StringViewArray::from(
        matched_queries
            .iter()
            .map(|q| q.binary_uri.as_str())
            .collect::<Vec<_>>(),
    );
    let offset_array = UInt64Array::from(
        matched_queries
            .iter()
            .map(|q| q.byte_offset)
            .collect::<Vec<_>>(),
    );

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(uri_array), Arc::new(offset_array)],
    )?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    ctx.register_table("queries", Arc::new(table))?;

    let mut id_to_name: BTreeMap<u32, String> = BTreeMap::new();

    ctx.register_parquet(
        "function_id",
        parquet_index_path
            .join("function_id.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;

    let sql = "SELECT id, name FROM function_id";
    let mut func_batches = ctx.sql(sql).await?.collect().await?;
    for batch in func_batches.drain(..) {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let names = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            id_to_name.insert(ids.value(i), names.value(i).to_string());
        }
    }

    let sql = "
        SELECT DISTINCT index_source_map.func_id, index_source_map.insn_id
        FROM queries q
        JOIN artifacts a ON a.canonical_path = q.uri OR a.canonical_path LIKE '%' || q.uri
        JOIN files f ON f.artifact_id = a.artifact_id
        JOIN file_spans fs ON fs.file_id = f.file_id
        JOIN spans s ON s.span_id = fs.span_id AND s.start = q.offset
        JOIN index_source_map ON index_source_map.source_span_id = fs.file_span_id
    ";
    let mut batches = ctx.sql(sql).await?.collect().await?;

    let mut target_sites = BTreeSet::new();
    for batch in batches.drain(..) {
        let func_ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let insn_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        for i in 0..batch.num_rows() {
            target_sites.insert((func_ids.value(i), insn_ids.value(i)));
        }
    }

    // Now query for all locations
    let mut site_to_loc: BTreeMap<(u32, u64), (String, u64)> = BTreeMap::new();
    let loc_sql = "
        SELECT index_source_map.func_id, index_source_map.insn_id, a.canonical_path, s.start
        FROM index_source_map
        JOIN file_spans fs ON index_source_map.source_span_id = fs.file_span_id
        JOIN files f ON f.file_id = fs.file_id
        JOIN artifacts a ON a.artifact_id = f.artifact_id
        JOIN spans s ON s.span_id = fs.span_id
    ";
    let mut loc_batches = ctx.sql(loc_sql).await?.collect().await?;
    for batch in loc_batches.drain(..) {
        let func_ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let insn_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        let uris = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        let offsets = batch
            .column(3)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        for i in 0..batch.num_rows() {
            site_to_loc.insert(
                (func_ids.value(i), insn_ids.value(i)),
                (uris.value(i).to_string(), offsets.value(i) as u64),
            );
        }
    }

    let mut target_vertices = Vec::new();
    for (site_packed, _, vertex) in &index_facts.actual_param {
        let site = InsnSiteId::unpack(site_packed).unwrap();
        if target_sites.contains(&(site.func_id.id, site.insn_id.id)) {
            target_vertices.push((site.func_id, Some(*site_packed), vertex.clone()));
        }
    }
    for (func_id, v1, p1, v2, p2) in &index_result.assign_like {
        let is_target = target_sites.iter().any(|(f, i)| {
            if *f != func_id.id {
                return false;
            }
            if let crate::facts::FlowVariableKind::CallArg(packed) = v1.kind()
                && let Ok(call_arg) = crate::facts::CallArgId::try_from(packed)
                && call_arg.insn_id.id == *i
            {
                return true;
            }
            if let crate::facts::FlowVariableKind::CallArg(packed) = v2.kind()
                && let Ok(call_arg) = crate::facts::CallArgId::try_from(packed)
                && call_arg.insn_id.id == *i
            {
                return true;
            }
            false
        });

        if is_target {
            target_vertices.push((*func_id, None, crate::facts::FlowVertex(*v1, *p1)));
            target_vertices.push((*func_id, None, crate::facts::FlowVertex(*v2, *p2)));
        }
    }

    let mut insn_to_func = BTreeMap::new();
    for (site_packed, _, _) in &index_facts.actual_param {
        let site = crate::facts::InsnSiteId::unpack(site_packed).unwrap();
        insn_to_func.insert(site.insn_id.id, site.func_id);
    }
    for (site_packed, _) in &index_facts.call {
        let site = crate::facts::InsnSiteId::unpack(site_packed).unwrap();
        insn_to_func.insert(site.insn_id.id, site.func_id);
    }
    let mut endpoints = Vec::new();

    // In addition to the target vertices defined via command line input, we must load
    // the source and sink definitions associated with the current project, as these are
    // what actually define the true sources and sinks we search for.
    let mut model_endpoints = Vec::new();
    let index_path = project.index_path()?;
    let ids = crate::facts::IdMap::try_load(&index_path)?;
    // First pass: load shipped default models into a match accumulator.
    // We'll run Stage 2 (index-dependent endpoint resolution + expansion) once, after the
    // loop, because it also needs the union-find over the whole `assign_like` relation.
    let mut model_matches = crate::models::ProgramModelMatches::default();
    for import in project.iter_imports() {
        let import = import?;
        let program_info = super::load_program_info_without_source_info(&import)?;
        let match_index = crate::models::ProgramMatchIndex::new(
            &program_info,
            crate::models::ImportScope::new(import.language, &import.name),
        );
        // Load whatever defaults apply for this program's VMT.
        crate::models::try_load_default_models(&match_index, &mut model_matches)?;

        // Flowy endpoints are already resolved against the call graph and don't need Stage 2.
        if import.language == crate::project::ArtifactLanguage::Flowy {
            let eps = crate::codegen::flowy::get_endpoints(&import, &ids, &index_facts.call)?;
            for (ep,) in eps {
                model_endpoints.push(ep);
            }
        }
    }

    // Stage 2: resolve default (name-based) matched endpoints against the index and expand
    // call-site fan-out / wildcard sinks.
    let built = crate::query_engine::build_query_endpoints(
        &model_matches.endpoints,
        &index_facts,
        &ids,
        &index_result.assign_like,
    );
    for (ep,) in built.endpoints {
        model_endpoints.push(ep);
    }
    let built_formals = built.formals;

    if !sources.is_empty() {
        model_endpoints.retain(|ep| ep.direction != FactsTaintDirection::Forward);
        for s in sources {
            if let Some((var, path)) = parse_vertex_str(s) {
                if let crate::facts::FlowVariableKind::CallArg(packed) = var.kind() {
                    let call_arg_id = crate::facts::CallArgId::try_from(packed).unwrap();
                    let insn_id = call_arg_id.insn_id.id;
                    if let Some(&func_id) = insn_to_func.get(&insn_id) {
                        model_endpoints.push(QueryEndpoint {
                            infunc: func_id,
                            vertex: crate::facts::FlowVertex(var, path),
                            label: Label("user_source".into()),
                            direction: FactsTaintDirection::Forward,
                            call_site: None,
                            saturating: false,
                        });
                    } else {
                        eprintln!("Warning: could not find function for source {}", s);
                    }
                } else {
                    eprintln!("Warning: only call-arg sources are supported, got {}", s);
                }
            } else {
                eprintln!("Warning: failed to parse source {}", s);
            }
        }
    }

    if !sinks.is_empty() {
        model_endpoints.retain(|ep| ep.direction != FactsTaintDirection::Backward);
        for s in sinks {
            if let Some((var, path)) = parse_vertex_str(s) {
                if let crate::facts::FlowVariableKind::CallArg(packed) = var.kind() {
                    let call_arg_id = crate::facts::CallArgId::try_from(packed).unwrap();
                    let insn_id = call_arg_id.insn_id.id;
                    if let Some(&func_id) = insn_to_func.get(&insn_id) {
                        model_endpoints.push(QueryEndpoint {
                            infunc: func_id,
                            vertex: crate::facts::FlowVertex(var, path),
                            label: Label("user_sink".into()),
                            direction: FactsTaintDirection::Backward,
                            call_site: None,
                            saturating: false,
                        });
                    } else {
                        eprintln!("Warning: could not find function for sink {}", s);
                    }
                } else {
                    eprintln!("Warning: only call-arg sinks are supported, got {}", s);
                }
            } else {
                eprintln!("Warning: failed to parse sink {}", s);
            }
        }
    }

    let want_forward = matches!(direction, TaintDirection::All | TaintDirection::Fwd);
    let want_backward = matches!(direction, TaintDirection::All | TaintDirection::Bwd);

    for ep in &model_endpoints {
        if (want_forward && ep.direction == FactsTaintDirection::Forward)
            || (want_backward && ep.direction == FactsTaintDirection::Backward)
        {
            endpoints.push((ep.clone(),));
        }
    }

    for (func_id, site_packed, vertex) in target_vertices {
        if matches!(direction, TaintDirection::Fwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_fwd".into()),
                direction: FactsTaintDirection::Forward,
                call_site: site_packed,
                saturating: false,
            },));
        }
        if matches!(direction, TaintDirection::Bwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_bwd".into()),
                direction: FactsTaintDirection::Backward,
                call_site: site_packed,
                saturating: false,
            },));
        }
    }
    eprintln!(
        "DEBUG: found {} endpoints for taint_analysis",
        endpoints.len()
    );

    let mut formal_param = index_facts.formal_param.clone();
    formal_param.extend(built_formals);

    let query_facts = QueryFacts {
        formal_param,
        actual_param: index_facts.actual_param.clone(),
        call: index_facts.call.clone(),
        assign: index_result.assign_like.clone(),
        paths: index_result.paths.clone(),
        endpoints: endpoints.clone(),
        external_function: index_result.external_function.clone(),
    };

    let query_result = taint_analysis(query_facts, None);

    let mut b = FormatFactsBuilder::default();
    b.taint(query_result.taint.clone())
        .taint_edge(query_result.taint_edge.clone())
        .index_actual_param(index_facts.actual_param.clone())
        .call(index_facts.call.clone());
    let format_facts = b.build().unwrap();

    let taint_results = TaintAnalysisResults::from_query_result(&query_result);

    let fg = build_taint_flow_graph(&format_facts, &taint_results);
    let graph = fg.graph;
    let id_to_node = fg.id_to_node;
    let node_to_id = fg.node_to_id;

    let mut rev_edges = Vec::new();
    for i in 0..graph.num_nodes() {
        for (succ, label) in graph.labeled_successors(i as u32) {
            let rev_label = match label {
                crate::facts::FlowEdge::Call(s) => crate::facts::FlowEdge::Return(s),
                crate::facts::FlowEdge::Return(s) => crate::facts::FlowEdge::Call(s),
                crate::facts::FlowEdge::Intra => crate::facts::FlowEdge::Intra,
            };
            rev_edges.push((succ, i as u32, rev_label));
        }
    }
    let rev_graph =
        crate::query_engine::formatter::LabeledTaintGraph::new(graph.num_nodes(), rev_edges);

    let mut all_sources: BTreeSet<&QueryEndpoint> = BTreeSet::new();
    let mut all_sinks: BTreeSet<&QueryEndpoint> = BTreeSet::new();
    for ep in &model_endpoints {
        match ep.direction {
            FactsTaintDirection::Forward => {
                all_sources.insert(ep);
            }
            FactsTaintDirection::Backward => {
                all_sinks.insert(ep);
            }
        }
    }

    // Map each node to an instruction for location info
    let mut node_to_site: BTreeMap<(FunctionId, FlowVariable, Path), (FunctionId, InsnId)> =
        BTreeMap::new();
    for (site, _, v, p) in &format_facts.actual_param {
        let site_unpacked = InsnSiteId::unpack(site).unwrap();
        node_to_site
            .entry((site_unpacked.func_id, *v, *p))
            .or_insert((site_unpacked.func_id, site_unpacked.insn_id));
    }

    let mut output = Output {
        fwd: Vec::new(),
        bwd: Vec::new(),
    };

    let mut seen_fwd: HashSet<Vec<u32>> = HashSet::new();
    let mut seen_bwd: HashSet<Vec<u32>> = HashSet::new();

    for endpoint_tuple in &endpoints {
        let endpoint = &endpoint_tuple.0;
        let start_n = (endpoint.infunc, endpoint.vertex.0, endpoint.vertex.1);
        if let Some(&start_id) = node_to_id.get(&start_n) {
            if endpoint.direction == FactsTaintDirection::Forward {
                for sink in &all_sinks {
                    let end_n = (sink.infunc, sink.vertex.0, sink.vertex.1);
                    if let Some(&target_id) = node_to_id.get(&end_n)
                        && let Some(path) =
                            find_annotated_path_to_set(&graph, start_id, |n, _s: &TaintState| {
                                n == target_id
                            })
                    {
                        let path_ids: Vec<u32> = path.into_iter().map(|(n, _s)| n).collect();
                        if seen_fwd.insert(path_ids.clone()) {
                            let path_res = build_path(
                                &path_ids,
                                &id_to_node,
                                &id_to_name,
                                &node_to_site,
                                &site_to_loc,
                            );
                            output.fwd.push(path_res);
                        }
                    }
                }
            } else if endpoint.direction == FactsTaintDirection::Backward {
                for source in &all_sources {
                    let target_n = (source.infunc, source.vertex.0, source.vertex.1);
                    if let Some(&target_id) = node_to_id.get(&target_n)
                        && let Some(path) = find_annotated_path_to_set(
                            &rev_graph,
                            start_id,
                            |n, _s: &TaintState| n == target_id,
                        )
                    {
                        let mut path_ids: Vec<u32> = path.into_iter().map(|(n, _s)| n).collect();
                        path_ids.reverse();
                        if seen_bwd.insert(path_ids.clone()) {
                            let path_res = build_path(
                                &path_ids,
                                &id_to_node,
                                &id_to_name,
                                &node_to_site,
                                &site_to_loc,
                            );
                            output.bwd.push(path_res);
                        }
                    }
                }
            }
        }
    }

    serde_json::to_writer_pretty(std::io::stdout(), &output)?;

    Ok(())
}

fn build_path(
    path_ids: &[u32],
    id_to_node: &[(FunctionId, FlowVariable, Path)],
    id_to_name: &BTreeMap<u32, String>,
    node_to_site: &BTreeMap<(FunctionId, FlowVariable, Path), (FunctionId, InsnId)>,
    site_to_loc: &BTreeMap<(u32, u64), (String, u64)>,
) -> Vec<CtadlDataResult> {
    let mut path = Vec::new();
    for window in path_ids.windows(2) {
        let in_id = window[0];
        let out_id = window[1];

        let in_node = &id_to_node[in_id as usize];
        let out_node = &id_to_node[out_id as usize];

        let in_var_str = match in_node.1.kind() {
            FlowVariableKind::Local(name) => name.to_string(),
            _ => format!("{}", in_node.1),
        };
        let out_var_str = match out_node.1.kind() {
            FlowVariableKind::Local(name) => name.to_string(),
            _ => format!("{}", out_node.1),
        };

        let in_mth_name = id_to_name
            .get(&in_node.0.id)
            .cloned()
            .unwrap_or_else(|| format!("{}", in_node.0.id));
        let out_mth_name = id_to_name
            .get(&out_node.0.id)
            .cloned()
            .unwrap_or_else(|| format!("{}", out_node.0.id));

        let in_class = in_mth_name.split(";->").next().map(|s| s.to_string());
        let out_class = out_mth_name.split(";->").next().map(|s| s.to_string());

        let in_node_info = CtadlNodeInfo {
            var: Some(in_var_str),
            mth: Some(in_mth_name.clone()),
            class: in_class,
            ap: Some(in_node.2.to_dot_string()),
        };

        let out_node_info = CtadlNodeInfo {
            var: Some(out_var_str),
            mth: Some(out_mth_name.clone()),
            class: out_class,
            ap: Some(out_node.2.to_dot_string()),
        };

        let mut byte_offset = None;
        if let Some(site) = node_to_site.get(out_node)
            && let Some((_, offset)) = site_to_loc.get(&(site.0.id, site.1.id))
        {
            byte_offset = Some(*offset);
        }

        path.push(CtadlDataResult {
            byte_offset,
            in_node: Some(in_node_info),
            out_node: Some(out_node_info),
        });
    }
    path
}

fn parse_vertex_str(s: &str) -> Option<(crate::facts::FlowVariable, crate::facts::Path)> {
    if let Some(rest) = s.strip_prefix("call-arg(")
        && let Some(idx) = rest.find(')')
    {
        let inner = &rest[..idx];
        let mut parts = inner.split(',');
        let insn_str = parts.next()?.trim();
        let formal_str = parts.next()?.trim();
        let insn_id = insn_str.parse::<u64>().ok()?;
        let formal = formal_str.parse::<i16>().ok()?;

        let call_arg_id = crate::facts::CallArgId::new(
            crate::facts::InsnId { id: insn_id },
            crate::facts::FormalIndex::from(formal),
        );
        let packed = crate::facts::PackedCallArg::try_from(call_arg_id).ok()?;
        let var = crate::facts::FlowVariableKind::CallArg(packed).into();

        let path_str = &rest[idx + 1..];
        let path = if path_str.is_empty() {
            crate::facts::Path::empty()
        } else {
            path_str.parse::<crate::facts::Path>().ok()?
        };

        return Some((var, path));
    }
    None
}
