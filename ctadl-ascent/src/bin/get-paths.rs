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
use serde_sarif::sarif::{Message, Result as SarifResult};

use ctadl_ascent::error::Error;
use ctadl_ascent::facts::{
    FlowVariable, FlowVariableKind, FunctionId, InsnId, InsnSiteId, Label, Path,
    TaintDirection as FactsTaintDirection,
};
use ctadl_ascent::index_engine::{IndexFacts, IndexResult};
use ctadl_ascent::query_engine::formatter::{
    FormatFactsBuilder, build_taint_flow_graph, compute_taint_results,
};
use ctadl_ascent::query_engine::{QueryEndpoint, QueryFacts, taint_analysis};
use ctadl_ir::graph::{Predecessors, Successors, find_path};

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
    pub result: SarifResult,
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

    let mut target_vertices = Vec::new();
    for (site_packed, _, vertex) in &index_facts.actual_param {
        let site = InsnSiteId::unpack(site_packed).unwrap();
        if target_sites.contains(&(site.func_id.id, site.insn_id.id)) {
            target_vertices.push((site.func_id, vertex.clone()));
        }
    }

    let mut endpoints = Vec::new();
    for (func_id, vertex) in target_vertices {
        if matches!(direction, TaintDirection::Fwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_fwd".into()),
                direction: FactsTaintDirection::Forward,
            },));
        }
        if matches!(direction, TaintDirection::Bwd | TaintDirection::All) {
            endpoints.push((QueryEndpoint {
                infunc: func_id,
                vertex: vertex.clone(),
                label: Label("target_bwd".into()),
                direction: FactsTaintDirection::Backward,
            },));
        }
    }

    let query_facts = QueryFacts {
        formal_param: index_facts.formal_param.clone(),
        actual_param: index_facts.actual_param.clone(),
        call: index_facts.call.clone(),
        assign: index_result.assign_like.clone(),
        paths: index_result.paths.clone(),
        endpoints: endpoints.clone(),
    };

    let query_result = taint_analysis(query_facts, None);

    let mut b = FormatFactsBuilder::default();
    b.taint(query_result.taint.clone())
        .formal_param(query_result.formal_param.clone())
        .index_actual_param(index_facts.actual_param.clone())
        .call(index_facts.call.clone())
        .assign(index_result.assign_like.clone())
        .paths(index_result.paths.clone())
        .external_function(index_result.external_function.clone());
    let format_facts = b.build().unwrap();

    let taint_results = compute_taint_results(&format_facts);

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
                    for succ in graph.successors(curr) {
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
                    if let Some(path_ids) = find_path(&graph, start_id, leaf) {
                        let path = build_path(&path_ids, &id_to_node, &id_to_name);
                        output.fwd.push(path);
                    }
                }
            } else if endpoint.direction == FactsTaintDirection::Backward {
                let mut visited = vec![false; id_to_node.len()];
                let mut queue = vec![start_id];
                visited[start_id as usize] = true;
                let mut reachable_roots = Vec::new();

                while let Some(curr) = queue.pop() {
                    let mut has_preds = false;
                    for pred in graph.predecessors(curr) {
                        has_preds = true;
                        if !visited[pred as usize] {
                            visited[pred as usize] = true;
                            queue.push(pred);
                        }
                    }
                    if !has_preds {
                        reachable_roots.push(curr);
                    }
                }

                for root in reachable_roots {
                    if let Some(path_ids) = find_path(&graph, root, start_id) {
                        let path = build_path(&path_ids, &id_to_node, &id_to_name);
                        output.bwd.push(path);
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
) -> Vec<CtadlDataResult> {
    let mut path = Vec::new();
    for &id in path_ids {
        let node = &id_to_node[id as usize];

        let var_str = match node.1.kind() {
            FlowVariableKind::Local(name) => name.to_string(),
            _ => format!("{}", node.1),
        };

        let mth_name = id_to_name
            .get(&node.0.id)
            .cloned()
            .unwrap_or_else(|| format!("{}", node.0.id));

        let in_node = CtadlNodeInfo {
            var: Some(var_str),
            mth: Some(mth_name.clone()),
            class: None,
            ap: Some(node.2.to_dot_string()),
        };

        let result = SarifResult::builder()
            .message(
                Message::builder()
                    .text(format!("Node {} {}", mth_name, node.1))
                    .build(),
            )
            .build();

        path.push(CtadlDataResult {
            result,
            in_node: Some(in_node),
            out_node: None,
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
