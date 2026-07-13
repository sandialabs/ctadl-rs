use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use datafusion::arrow::array::{StringViewArray, UInt32Array, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;
use datafusion::prelude::*;
use packed_struct::prelude::*;
use serde::Serialize;

use ctadl_ascent::error::Error;
use ctadl_ascent::facts::{
    FlowVariable, FlowVariableKind, FunctionId, InsnId, InsnSiteId, Label, Path,
    TaintDirection as FactsTaintDirection, TaintState,
};
use ctadl_ascent::index_engine::{IndexFacts, IndexResult};
use ctadl_ascent::query_engine::formatter::{
    FormatFactsBuilder, build_taint_flow_graph, TaintAnalysisResults,
};
use ctadl_ascent::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};
use ctadl_ir::graph::{LabeledSuccessors, find_annotated_path_to_set};

#[derive(Debug, Clone, ValueEnum, Copy)]
pub enum TaintDirection {
    All,
    Fwd,
    Bwd,
}

#[derive(Debug, Parser)]
#[command(name = "get-paths")]
#[command(about = "Find taint graph paths from binary URI + byte offset pairs")]
struct Args {
    #[arg(value_name = "PARQUET_SOURCE_INFO_PATH")]
    parquet_source_info_path: PathBuf,

    #[arg(value_name = "PARQUET_INDEX_PATH")]
    parquet_index_path: PathBuf,

    #[arg(value_name = "BINARY_URI,BYTE_OFFSET", required = true, num_args = 1..)]
    pairs: Vec<String>,

    #[arg(long, short, value_enum, default_value_t = TaintDirection::All)]
    pub taint_direction: TaintDirection,
}

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
    let byte_offset = off_str.parse::<u64>().map_err(|e| {
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

async fn run() -> Result<(), Error> {
    let args = Args::parse();
    let direction = args.taint_direction;
    let queries: Vec<OffsetQuery> = args
        .pairs
        .iter()
        .map(|s| parse_pair(s))
        .collect::<Result<_, _>>()?;

    let index_facts = IndexFacts::try_load(&args.parquet_index_path)?;
    let index_result = IndexResult::try_load(&args.parquet_index_path)?;

    let ctx = SessionContext::new();
    ctx.register_parquet(
        "index_source_map",
        args.parquet_index_path
            .join("index_source_map.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "file_spans",
        args.parquet_source_info_path
            .join("file_spans.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "spans",
        args.parquet_source_info_path
            .join("spans.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "files",
        args.parquet_source_info_path
            .join("files.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet(
        "artifacts",
        args.parquet_source_info_path
            .join("artifacts.parquet")
            .to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("uri", DataType::Utf8View, false),
        Field::new("offset", DataType::UInt64, false),
    ]));
    let uri_array = StringViewArray::from(
        queries
            .iter()
            .map(|q| q.binary_uri.as_str())
            .collect::<Vec<_>>(),
    );
    let offset_array = UInt64Array::from(queries.iter().map(|q| q.byte_offset).collect::<Vec<_>>());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(uri_array), Arc::new(offset_array)],
    )?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    ctx.register_table("queries", Arc::new(table))?;

    let mut id_to_name: BTreeMap<u32, String> = BTreeMap::new();

    ctx.register_parquet(
        "function_id",
        args.parquet_index_path
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
        JOIN artifacts a ON a.canonical_path = q.uri
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
            target_vertices.push((site.func_id, *site_packed, vertex.clone()));
        }
    }

    let mut endpoints = Vec::new();
    for (func_id, site_packed, vertex) in target_vertices {
        if matches!(direction, TaintDirection::Fwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_fwd".into()),
                direction: FactsTaintDirection::Forward,
                call_site: Some(site_packed),
            },));
        }
        if matches!(direction, TaintDirection::Bwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_bwd".into()),
                direction: FactsTaintDirection::Backward,
                call_site: Some(site_packed),
            },));
        }
    }

    let query_facts = QueryFacts {
        formal_param: index_facts.formal_param.clone(),
        actual_param: index_facts.actual_param.clone(),
        call: index_facts.call.clone(),
        assign: index_result.assign_like.clone(),
        paths: index_result.paths.clone(),
        external_function: index_result.external_function.clone(),
        endpoints: endpoints.clone(),
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

    for endpoint_tuple in &endpoints {
        let endpoint = &endpoint_tuple.0;
        let start_n = (endpoint.infunc, endpoint.vertex.0, endpoint.vertex.1);
        if let Some(&start_id) = node_to_id.get(&start_n) {
            if endpoint.direction == FactsTaintDirection::Forward {
                let mut visited = vec![false; id_to_node.len()];
                let mut queue = vec![start_id];
                visited[start_id as usize] = true;
                let mut reachable_leaves = Vec::new();

                while let Some(curr) = queue.pop() {
                    let mut has_succs = false;
                    for (succ, _) in graph.labeled_successors(curr) {
                        has_succs = true;
                        if !visited[succ as usize] {
                            visited[succ as usize] = true;
                            queue.push(succ);
                        }
                    }
                    if !has_succs {
                        reachable_leaves.push(curr);
                    }
                }

                for leaf in reachable_leaves {
                    if let Some(path) = find_annotated_path_to_set(&graph, start_id, |n, _s: &TaintState| n == leaf) {
                        let path_ids: Vec<u32> = path.into_iter().map(|(n, _s)| n).collect();
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
            } else if endpoint.direction == FactsTaintDirection::Backward {
                let mut visited = vec![false; id_to_node.len()];
                let mut queue = vec![start_id];
                visited[start_id as usize] = true;
                let mut reachable_leaves = Vec::new();

                while let Some(curr) = queue.pop() {
                    let mut has_succs = false;
                    for (succ, _) in graph.labeled_successors(curr) {
                        has_succs = true;
                        if !visited[succ as usize] {
                            visited[succ as usize] = true;
                            queue.push(succ);
                        }
                    }
                    if !has_succs {
                        reachable_leaves.push(curr);
                    }
                }

                for leaf in reachable_leaves {
                    if let Some(path) = find_annotated_path_to_set(&graph, start_id, |n, _s: &TaintState| n == leaf) {
                        let mut path_ids: Vec<u32> = path.into_iter().map(|(n, _s)| n).collect();
                        path_ids.reverse();
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
        if let Some(site) = node_to_site.get(out_node) {
            if let Some((_, offset)) = site_to_loc.get(&(site.0.id, site.1.id)) {
                byte_offset = Some(*offset);
            }
        }

        path.push(CtadlDataResult {
            byte_offset,
            in_node: Some(in_node_info),
            out_node: Some(out_node_info),
        });
    }
    path
}

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run()) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
