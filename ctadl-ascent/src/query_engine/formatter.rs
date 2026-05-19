/*! Format taint results

The taint engine produces taint information for each vertex in the target program. The goal of
this module is to compute location information for each tainted vertex and instruction. Since
most frontends only store instruction location information (as opposed to locations for each
variable access), we focus on instruction locations.

The schema of tables for formatting is:

```text
function_id:
id (int), function_name (string)


source-info:
metadata:
hash_algorithm, hash_len, version

artifacts:
artifact_id, canonical_path, sub_artifact_id, encoding, content_hash

files:
file_id, artifact_id

spans:
span_id, start, len_tag, len_value

file_spans:
file_span_id, file_id, span_id
```

*/
use std::fs::File;
use std::path;
use std::sync::Arc;

use ascent::ascent;
use ctadl_ir::Idx;
use ctadl_ir::graph::{DirectedGraph, Predecessors, Successors, find_path};
use datafusion::arrow::array::{StringViewArray, UInt8Array, UInt32Array, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;
use datafusion::prelude::*;
use derive_builder::Builder;
use memmap::MmapOptions;
use packed_struct::prelude::*;
use serde::{Deserialize, Serialize};
use serde_sarif::sarif::{
    Address, ArtifactLocation, CodeFlow, Location, LogicalLocation, Message,
    MultiformatMessageString, PhysicalLocation, PropertyBag, Region, ReportingDescriptor,
    Result as SarifResult, ResultKind, ResultLevel, Run, Sarif, ThreadFlow, ThreadFlowLocation,
    Tool, ToolComponent,
};
use source_info::FileSpanId;
use source_info::{LineMap, offset_to_line_column};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ErrorContext};
use crate::facts::schema;
use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, InsnId, InsnSiteId, Label,
    PackedInsnSiteId, Path, TaintDirection, TaintState, isout,
};
use crate::project::{AnalysisProject, ArtifactLanguage};
use crate::query_engine::QueryEndpoint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize, Default)]
pub enum SarifProfile {
    #[default]
    Human,
    Agent,
    Machine,
    Debug,
}

pub struct ProjectContext<'a, P: AsRef<path::Path>> {
    pub source_spans: &'a [(FileSpanId, FunctionId, InsnId)],
    pub index_dir: P,
    pub source_info_dir: P,
    pub details_by_span: &'a BTreeMap<u32, Vec<(Label, FunctionId, FlowVariable, Path)>>,
    pub facts: &'a FormatFacts,
    pub taint_results: &'a TaintAnalysisResults,
    pub language: ArtifactLanguage,
}

pub struct FormatConfig {
    pub compact: bool,
    pub profile: SarifProfile,
}

// SARIF rule identifier for any tainted path result
const TAINTED_PATH_RULE_ID: &str = "C0001.tainted-path";
const TAINTED_PATH_RULE_NAME: &str = "Tainted paths";
const TAINTED_PATH_RULE_DESCRIPTION: &str = "A path of tainted data flow";

// SARIF rule identifier for any tainted instruction result
const TAINTED_INSTRUCTION_RULE_ID: &str = "C0002.tainted-instruction";
const TAINTED_INSTRUCTION_RULE_NAME: &str = "Tainted instructions";
const TAINTED_INSTRUCTION_RULE_DESCRIPTION: &str = "An instruction with tainted data";

// SARIF rule identifiers for taint source and sink
const TAINT_SOURCE_RULE_ID: &str = "C0003.taint-source";
const TAINT_SOURCE_RULE_NAME: &str = "Tainted data sources";
const TAINT_SOURCE_RULE_DESCRIPTION: &str = "Tainted data source";

const TAINT_SINK_RULE_ID: &str = "C0004.taint-sink";
const TAINT_SINK_RULE_NAME: &str = "Tainted data sink";
const TAINT_SINK_RULE_DESCRIPTION: &str = "Tainted data sinks";

// SARIF rule identifiers for tainted data and almost-path functions
const TAINTED_DATA_RULE_ID: &str = "C0005";
const TAINTED_DATA_RULE_NAME: &str = "Tainted data";
const TAINTED_DATA_RULE_DESCRIPTION: &str = "Tainted variables and fields";

const ALMOST_PATH_FUNCTION_RULE_ID: &str = "C0006";
const ALMOST_PATH_FUNCTION_RULE_NAME: &str = "Almost-path function";
const ALMOST_PATH_FUNCTION_RULE_DESCRIPTION: &str = "A function which contains source-tainted and sink-tainted data, which means there's 'almost' a path between them.";

#[derive(Default, Builder, Clone)]
pub struct FormatFacts {
    /// Taint results on variables
    #[builder(default)]
    pub taint: Vec<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)>,
    #[builder(default)]
    pub formal_param: Vec<(FunctionId, FlowVariable, FormalType)>,
    #[builder(default)]
    pub actual_param: Vec<(PackedInsnSiteId, FormalIndex, FlowVariable, Path)>,
    #[builder(default)]
    pub call: Vec<(PackedInsnSiteId, FunctionId)>,
    #[builder(default)]
    pub assign: Vec<(FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path)>,
    #[builder(default)]
    pub paths: Vec<(Path,)>,
    #[builder(default)]
    pub id_to_name: BTreeMap<u32, String>,
}

pub struct TaintedInstructions {
    // (site id, label, variable, access path)
    pub tainted_insn: Vec<(PackedInsnSiteId, Label, FlowVariable, Path)>,
}

pub struct TaintAnalysisResults {
    pub edges: Vec<(
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )>,
    pub tainted_insns: TaintedInstructions,
}

impl FormatFactsBuilder {
    /// Converts the actual_param from indexing into our format
    pub fn index_actual_param(
        &mut self,
        facts: Vec<(PackedInsnSiteId, FormalIndex, FlowVertex)>,
    ) -> &mut Self {
        self.actual_param(
            facts
                .into_iter()
                .map(|(id, i, vx)| {
                    let FlowVertex(var, path) = vx;
                    (id, i, var, path)
                })
                .collect(),
        )
    }
}

pub fn compute_taint_results(facts: &FormatFacts) -> TaintAnalysisResults {
    ascent! {
        struct FormatterEngine;
        macro produce_taint($df:expr, $dts:expr, $dv:expr, $dp:expr, $a:expr, $sf:expr, $sv:expr, $sp:expr) {
            taint($df, $dts, $dv, $dp, $a),
            taint_edge($df, $dv, $dp, $sf, $sv, $sp)
        }
        relation taint_edge(FunctionId, FlowVariable, Path, FunctionId, FlowVariable, Path);
        relation tainted_var_at_insn(PackedInsnSiteId, Label, FlowVariable, Path);

        include_source!(crate::query_engine::ascent_code::taint_analysis_rules);

        // taint call sites
        tainted_var_at_insn(id, label, v2, p2) <--
            taint(_, _, v2, p2, src),
            if !v2.is_globals(),
            if let FlowVariable::CallArg { id, formal } = v2,
            if **formal >= 0,
            let label = src.label.clone();

        // taint assigns
        tainted_var_at_insn(id, label.clone(), v2, p2) <--
            taint(func_id, _, v2, p2, src),
            if !v2.is_globals(),
            (assign_like(func_id, insn_id, _, _, v2, p2) | assign_like(func_id, insn_id, v2, p2, _, _)),
            let site_id = InsnSiteId {func_id: *func_id, insn_id: *insn_id},
            let id = InsnSiteId::pack(&site_id).map(PackedInsnSiteId).expect("pack error"),
            let label = &src.label;
    }

    let mut engine = FormatterEngine {
        taint: facts.taint.clone(),
        formal_param: facts.formal_param.clone(),
        call: facts.call.clone(),
        assign_like: facts.assign.clone(),
        paths: facts.paths.clone(),
        ..Default::default()
    };
    engine.run();

    TaintAnalysisResults {
        edges: engine.taint_edge,
        tainted_insns: TaintedInstructions {
            tainted_insn: engine.tainted_var_at_insn.into_iter().collect(),
        },
    }
}

/// A simple graph implementation for taint analysis.
pub struct TaintGraph<N: Idx> {
    num_nodes: usize,
    successors: Vec<Vec<N>>,
    predecessors: Vec<Vec<N>>,
}

impl<N: Idx> TaintGraph<N> {
    pub fn new(num_nodes: usize, edges: Vec<(N, N)>) -> Self {
        let mut successors = vec![Vec::new(); num_nodes];
        let mut predecessors = vec![Vec::new(); num_nodes];
        for (src, dst) in edges {
            successors[src.index()].push(dst);
            predecessors[dst.index()].push(src);
        }
        Self {
            num_nodes,
            successors,
            predecessors,
        }
    }
}

impl<N: Idx> DirectedGraph for TaintGraph<N> {
    type Node = N;

    fn num_nodes(&self) -> usize {
        self.num_nodes
    }
}

impl<N: Idx> Successors for TaintGraph<N> {
    fn successors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        self.successors[node.index()].iter().cloned()
    }
}

impl<N: Idx> Predecessors for TaintGraph<N> {
    fn predecessors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        self.predecessors[node.index()].iter().cloned()
    }
}

pub fn format_sarif(
    project: &AnalysisProject,
    facts: FormatFacts,
    compact: bool,
    output: &path::Path,
    profile: SarifProfile,
) -> Result<(), Error> {
    log::trace!("format_sarif entry");
    let taint_results = compute_taint_results(&facts);
    let rt = tokio::runtime::Runtime::new()?;
    let config = FormatConfig { compact, profile };
    let final_sarif =
        rt.block_on(async { async_format_sarif(project, &taint_results, &facts, &config).await })?;

    let writer: Box<dyn std::io::Write> = if output.to_str() == Some("-") {
        Box::new(std::io::stdout())
    } else {
        Box::new(File::create(output).err_context(|| "creating sarif output file")?)
    };

    if compact {
        serde_json::to_writer(writer, &final_sarif).err_context(|| "writing sarif")
    } else {
        serde_json::to_writer_pretty(writer, &final_sarif).err_context(|| "writing sarif")
    }
}

#[derive(Default)]
pub struct SarifData {
    pub global_logical_locations_map: BTreeMap<String, usize>,
    pub global_logical_locations: Vec<LogicalLocation>,
}

#[derive(Default)]
pub struct SourceLocationData {
    pub all_locations: BTreeMap<(u32, u64), Location>,
    pub batch_data: Vec<(u32, u32, u64, Location)>,
    pub id_to_name: BTreeMap<u32, String>,
}

async fn async_format_sarif(
    project: &AnalysisProject,
    taint_results: &TaintAnalysisResults,
    facts: &FormatFacts,
    config: &FormatConfig,
) -> Result<serde_json::Value, Error> {
    let path = project
        .index_path()?
        .join(schema::index_source_map::FILENAME);
    // Find mapping from (function, insn) -> source span and collect details per instruction.
    let source_spans = find_source_ids(&path, &taint_results.tainted_insns)
        .await
        .err_context(|| "finding source ids")?;
    // Map (function ID, instruction ID) -> list of taint details.
    let mut instr_to_details: BTreeMap<(u32, u64), Vec<(Label, FlowVariable, Path)>> =
        BTreeMap::new();
    for (site_id, label, var, pth) in &taint_results.tainted_insns.tainted_insn {
        let site = InsnSiteId::unpack(site_id).expect("unpack error");
        let key = (site.func_id.id, site.insn_id.id);
        instr_to_details
            .entry(key)
            .or_default()
            .push((label.clone(), var.clone(), pth.clone()));
    }
    // Build a map from each file span to its associated taint details.
    let mut details_by_span: BTreeMap<u32, Vec<(Label, FunctionId, FlowVariable, Path)>> =
        BTreeMap::new();
    for (fs, func_id, insn_id) in source_spans.iter() {
        let key = (func_id.id, insn_id.id);
        if let Some(details) = instr_to_details.get(&key) {
            for (label, var, pth) in details {
                details_by_span.entry(fs.0).or_default().push((
                    label.clone(),
                    *func_id,
                    var.clone(),
                    pth.clone(),
                ));
            }
        }
    }
    let mut results = Vec::new();
    let mut sarif_data = SarifData::default();
    let index_dir = project.index_path()?;
    // projects should have only one set of parquet files, so just take the last one
    let mut parquet_dir = String::from("");
    for import in project.iter_imports() {
        let import = import?;
        let dir = import.source_info_dir();
        parquet_dir = String::from(dir.to_string_lossy());
        let ctx = ProjectContext {
            source_spans: &source_spans,
            index_dir: index_dir.clone(),
            source_info_dir: dir,
            details_by_span: &details_by_span,
            facts,
            taint_results,
            language: import.language,
        };
        let sarif_results = format_source_info_results(&ctx, config, &mut sarif_data)
            .await
            .err_context(|| "formatting results")?;
        results.extend(sarif_results);
    }

    const CTADL_FULL_DESCRIPTION: &str = "CTADL (Compositional Taint Analysis in Datalog).";
    let tool = Tool::builder()
        .driver(
            ToolComponent::builder()
                .name("ctadl")
                .version("2026.1")
                .information_uri("https://github.com/sandialabs/ctadl-rs")
                .full_description(
                    MultiformatMessageString::builder()
                        .text(CTADL_FULL_DESCRIPTION)
                        .build(),
                )
                .rules(vec![
                    ReportingDescriptor::builder()
                        .id(TAINTED_PATH_RULE_ID)
                        .name(TAINTED_PATH_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(TAINTED_PATH_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This is a tainted source-sink path.")
                                .build(),
                        )]))
                        .build(),
                    ReportingDescriptor::builder()
                        .id(TAINTED_INSTRUCTION_RULE_ID)
                        .name(TAINTED_INSTRUCTION_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(TAINTED_INSTRUCTION_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This instruction manipulates tainted data.")
                                .build(),
                        )]))
                        .build(),
                    ReportingDescriptor::builder()
                        .id(TAINT_SOURCE_RULE_ID)
                        .name(TAINT_SOURCE_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(TAINT_SOURCE_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This is a source of tainted data.")
                                .build(),
                        )]))
                        .build(),
                    ReportingDescriptor::builder()
                        .id(TAINT_SINK_RULE_ID)
                        .name(TAINT_SINK_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(TAINT_SINK_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This is an desired sink of tainted data.")
                                .build(),
                        )]))
                        .build(),
                    ReportingDescriptor::builder()
                        .id(TAINTED_DATA_RULE_ID)
                        .name(TAINTED_DATA_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(TAINTED_DATA_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This vertex is tainted.")
                                .build(),
                        )]))
                        .build(),
                    ReportingDescriptor::builder()
                        .id(ALMOST_PATH_FUNCTION_RULE_ID)
                        .name(ALMOST_PATH_FUNCTION_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(ALMOST_PATH_FUNCTION_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This function contains source and sink taint.")
                                .build(),
                        )]))
                        .build(),
                ])
                .build(),
        )
        .build();

    let properties = PropertyBag::builder()
        .additional_properties(BTreeMap::from([(
            "parquet_dir".to_string(),
            serde_json::json!(parquet_dir),
        )]))
        .build();

    let run = if sarif_data.global_logical_locations.is_empty() {
        Run::builder().tool(tool).results(results).build()
    } else {
        Run::builder()
            .tool(tool)
            .results(results)
            .logical_locations(sarif_data.global_logical_locations)
            .build()
    };
    // we need to deconstruct and rebuild the run to ensure a certain order (needs serde_json feature preserve_order)
    let final_run = match serde_json::to_value(&run).unwrap() {
        serde_json::Value::Object(mut old_map) => {
            let mut new_map = serde_json::Map::new();
            new_map.insert("tool".to_string(), old_map.remove("tool").unwrap());
            // the order of the rest doesn't matter
            for (k, v) in old_map {
                new_map.insert(k, v);
            }
            serde_json::Value::Object(new_map)
        }
        _ => panic!("Failed to extract serde_json sarif run"),
    };

    // the runs have to be inserted manually since the order would not be preserved if inserted as a runs object
    let sarif = Sarif::builder()
        .version("2.1.0")
        .properties(properties)
        .build();
    // rebuild sarif to preserve order
    let final_sarif = match serde_json::to_value(&sarif).unwrap() {
        serde_json::Value::Object(mut old_map) => {
            // remove the default (empty array) in the old map
            old_map.remove("runs");
            let mut new_map = serde_json::Map::new();
            new_map.insert("version".to_string(), old_map.remove("version").unwrap());
            new_map.insert(
                "properties".to_string(),
                old_map.remove("properties").unwrap(),
            );
            new_map.insert(
                "$schema".to_string(),
                serde_json::json!("https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/schemata/sarif-schema-2.1.0.json"),
            );
            // the order of the rest doesn't matter
            for (k, v) in old_map {
                new_map.insert(k, v);
            }
            // runs should be last
            new_map.insert(
                "runs".to_string(),
                serde_json::Value::Array(vec![final_run]),
            );
            serde_json::Value::Object(new_map)
        }
        _ => panic!("Failed to extract serde_json sarif map"),
    };
    Ok(final_sarif)
}

async fn populate_source_info<P: AsRef<path::Path>>(
    ctx: &ProjectContext<'_, P>,
    config: &FormatConfig,
    sarif_data: &mut SarifData,
    source_data: &mut SourceLocationData,
    needed_spans: &[(FileSpanId, FunctionId, InsnId)],
) -> Result<(), Error> {
    let dir = ctx.source_info_dir.as_ref();
    let index_dir = ctx.index_dir.as_ref();
    let ctx_session = SessionContext::new();

    ctx_session
        .register_parquet(
            "file_spans",
            dir.join("file_spans.parquet").to_string_lossy(),
            ParquetReadOptions::default(),
        )
        .await
        .err_context(|| "reading file_spans")?;
    ctx_session
        .register_parquet(
            "spans",
            dir.join("spans.parquet").to_string_lossy(),
            ParquetReadOptions::default(),
        )
        .await
        .err_context(|| "reading spans")?;
    ctx_session
        .register_parquet(
            "files",
            dir.join("files.parquet").to_string_lossy(),
            ParquetReadOptions::default(),
        )
        .await
        .err_context(|| "reading files")?;
    ctx_session
        .register_parquet(
            "artifacts",
            dir.join("artifacts.parquet").to_string_lossy(),
            ParquetReadOptions::default(),
        )
        .await
        .err_context(|| "reading artifacts")?;
    ctx_session
        .register_parquet(
            "function_id",
            index_dir.join("function_id.parquet").to_string_lossy(),
            ParquetReadOptions::default(),
        )
        .await
        .err_context(|| "reading function_id")?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("file_span_id", DataType::UInt32, false),
        Field::new("func_id", DataType::UInt32, false),
        Field::new("insn_id", DataType::UInt64, false),
    ]));

    let file_span_id_array: UInt32Array = needed_spans.iter().map(|(s, _, _)| s.0).collect();
    let func_id_array: UInt32Array = needed_spans.iter().map(|(_, f, _)| f.id).collect();
    let insn_id_array: UInt64Array = needed_spans.iter().map(|(_, _, i)| i.id).collect();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(file_span_id_array),
            Arc::new(func_id_array),
            Arc::new(insn_id_array),
        ],
    )?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    ctx_session.register_table("site_id", Arc::new(table))?;

    let sql = "
        SELECT fs_in.file_span_id, fs_in.func_id, fs_in.insn_id, f_id.name as func_name,
               art.canonical_path, art.encoding, s.start, s.len_tag, s.len_value
        FROM site_id fs_in
        JOIN file_spans fs ON fs.file_span_id = fs_in.file_span_id
        JOIN spans s   ON fs.span_id    = s.span_id
        JOIN files f   ON fs.file_id    = f.file_id
        JOIN artifacts art ON f.artifact_id = art.artifact_id
        JOIN function_id f_id ON fs_in.func_id = f_id.id
        ORDER BY fs_in.file_span_id
    ";

    log::trace!("running sql query");
    let batches = ctx_session.sql(sql).await?.collect().await?;
    log::trace!("done running sql query");

    for batch in batches {
        let file_span_ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let func_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let insn_ids = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        let func_names = batch
            .column(3)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        let canonical_paths = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        let encoding_arr = batch
            .column(5)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        let starts = batch
            .column(6)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let len_tags = batch
            .column(7)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        let len_values = batch
            .column(8)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();

        for i in 0..batch.num_rows() {
            let file_span_id = file_span_ids.value(i);
            let func_id = func_ids.value(i);
            let insn_id = insn_ids.value(i);
            let func_name = func_names.value(i);
            source_data
                .id_to_name
                .insert(func_id, func_name.to_string());
            let canonical_path = canonical_paths.value(i);
            let encoding = source_info::ArtifactEncoding::from_u8(encoding_arr.value(i));
            let start = starts.value(i);
            let len_tag = len_tags.value(i);
            let len_value = if len_tags.value(i) == 1 {
                len_values.value(i)
            } else {
                0
            };

            let region = match encoding {
                source_info::ArtifactEncoding::Binary => {
                    let builder = Region::builder().byte_offset(start);
                    if config.compact {
                        builder.build()
                    } else {
                        builder.byte_length(len_value).build()
                    }
                }
                source_info::ArtifactEncoding::Utf8 | source_info::ArtifactEncoding::Utf16 => {
                    let file = File::open(canonical_path)?;
                    // SAFETY: This is inherently unsafe because of mmap(). *shrug*
                    let contents = unsafe { MmapOptions::new().map(&file)? };
                    let line_map = LineMap::from_bytes(&contents);
                    let end_byte = match len_tag {
                        0 => start,
                        1 => start + len_value,
                        2 => contents[start as usize..]
                            .iter()
                            .position(|&b| b == b'\n')
                            .map(|p| start + p as u32 + 1)
                            .unwrap_or(contents.len() as u32),
                        _ => start,
                    };
                    let start_lc = offset_to_line_column(&line_map, start);
                    let end_lc =
                        offset_to_line_column(&line_map, end_byte.saturating_sub(1).max(start));
                    Region::builder()
                        .start_line(start_lc.line as i64)
                        .start_column((start_lc.column + 1) as i64)
                        .end_line(end_lc.line as i64)
                        .end_column((end_lc.column + 1) as i64)
                        .build()
                }
            };

            let uri_str = canonical_path.to_string();
            let uri_stripped = uri_str.strip_prefix('/').unwrap_or(&uri_str);
            let artifact_location = ArtifactLocation::builder()
                .uri(uri_stripped.to_string())
                .build();

            let is_pcode = ctx.language == ArtifactLanguage::Pcode;
            let physical_location = match encoding {
                source_info::ArtifactEncoding::Binary if is_pcode => {
                    let address = Address::builder()
                        .absolute_address(start as i64)
                        .kind("instruction")
                        .build();
                    PhysicalLocation::builder()
                        .artifact_location(artifact_location)
                        .address(address)
                        .build()
                }
                _ => PhysicalLocation::builder()
                    .artifact_location(artifact_location)
                    .region(region)
                    .build(),
            };

            let fully_qualified_name = match encoding {
                source_info::ArtifactEncoding::Binary => {
                    format!("{}@{:08x}:{:08x}", func_name, start, start)
                }
                _ => func_name.to_string(),
            };
            let loc_idx = *sarif_data
                .global_logical_locations_map
                .entry(fully_qualified_name.clone())
                .or_insert_with(|| {
                    let idx = sarif_data.global_logical_locations.len();
                    sarif_data.global_logical_locations.push(
                        LogicalLocation::builder()
                            .kind("member")
                            .name(func_name)
                            .fully_qualified_name(fully_qualified_name)
                            .build(),
                    );
                    idx
                });

            let logical_location = LogicalLocation::builder().index(loc_idx as i64).build();
            let location = Location::builder()
                .physical_location(physical_location)
                .logical_locations(vec![logical_location])
                .build();

            source_data
                .all_locations
                .insert((func_id, insn_id), location.clone());
            source_data
                .batch_data
                .push((file_span_id, func_id, insn_id, location));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn format_source_info_results<P: AsRef<path::Path>>(
    ctx: &ProjectContext<'_, P>,
    config: &FormatConfig,
    sarif_data: &mut SarifData,
) -> Result<Vec<SarifResult>, Error> {
    // Prepare graph for path finding when the selected profile emits path traces.
    let mut node_to_id: BTreeMap<(FunctionId, FlowVariable, Path), u32> = BTreeMap::new();
    let mut id_to_node: Vec<(FunctionId, FlowVariable, Path)> = Vec::new();

    let graph = if matches!(config.profile, SarifProfile::Human | SarifProfile::Debug) {
        let taint_edge = &ctx.taint_results.edges;
        // Collect all nodes into node_to_id first
        for (f, _, v, p, src) in &ctx.facts.taint {
            let n = (*f, v.clone(), p.clone());
            if !node_to_id.contains_key(&n) {
                node_to_id.insert(n.clone(), id_to_node.len() as u32);
                id_to_node.push(n);
            }
            let src_n = (src.infunc, src.vertex.0.clone(), src.vertex.1.clone());
            if !node_to_id.contains_key(&src_n) {
                node_to_id.insert(src_n.clone(), id_to_node.len() as u32);
                id_to_node.push(src_n);
            }
        }
        for (df, dv, dp, sf, sv, sp) in taint_edge {
            let src_n = (*sf, sv.clone(), sp.clone());
            if !node_to_id.contains_key(&src_n) {
                node_to_id.insert(src_n.clone(), id_to_node.len() as u32);
                id_to_node.push(src_n);
            }
            let dst_n = (*df, dv.clone(), dp.clone());
            if !node_to_id.contains_key(&dst_n) {
                node_to_id.insert(dst_n.clone(), id_to_node.len() as u32);
                id_to_node.push(dst_n);
            }
        }

        let edges: Vec<(u32, u32)> = taint_edge
            .iter()
            .map(|(df, dv, dp, sf, sv, sp)| {
                let src_n = (*sf, sv.clone(), sp.clone());
                let dst_n = (*df, dv.clone(), dp.clone());
                let src_id = *node_to_id.get(&src_n).unwrap();
                let dst_id = *node_to_id.get(&dst_n).unwrap();
                (src_id, dst_id)
            })
            .collect();
        Some(TaintGraph::new(id_to_node.len(), edges))
    } else {
        None
    };

    // Map each node to its endpoints
    let mut node_to_endpoint: BTreeMap<(FunctionId, FlowVariable, Path), Vec<QueryEndpoint>> =
        BTreeMap::new();
    for (f, _, v, p, src) in &ctx.facts.taint {
        node_to_endpoint
            .entry((*f, v.clone(), p.clone()))
            .or_default()
            .push(src.clone());
    }
    // All unique endpoints
    let endpoints: BTreeSet<_> = node_to_endpoint.values().flat_map(|v| v.iter()).collect();

    // Map each node to an instruction for location info
    let mut node_to_site: BTreeMap<(FunctionId, FlowVariable, Path), (FunctionId, InsnId)> =
        BTreeMap::new();
    for (f, i, v1, p1, v2, p2) in &ctx.facts.assign {
        node_to_site
            .entry((*f, v1.clone(), p1.clone()))
            .or_insert((*f, *i));
        node_to_site
            .entry((*f, v2.clone(), p2.clone()))
            .or_insert((*f, *i));
    }
    for (site, _, v, p) in &ctx.facts.actual_param {
        let site_unpacked = InsnSiteId::unpack(site).unwrap();
        node_to_site
            .entry((site_unpacked.func_id, v.clone(), p.clone()))
            .or_insert((site_unpacked.func_id, site_unpacked.insn_id));
    }

    // 1. Find all paths
    let has_sinks = node_to_endpoint.iter().any(|(_, ends)| {
        ends.iter()
            .any(|src| src.direction == crate::facts::TaintDirection::Backward)
    });

    let mut results_by_path: BTreeMap<
        Vec<u32>,
        (u32, Vec<(QueryEndpoint, Option<QueryEndpoint>, Label)>),
    > = BTreeMap::new();
    if let Some(ref g) = graph {
        for (fs_id, details) in ctx.details_by_span {
            let mut seen_pairs = BTreeSet::new();
            for (lbl, func_id, var, pth) in details {
                let node = (*func_id, var.clone(), pth.clone());
                if let Some(sources) = node_to_endpoint.get(&node) {
                    let (fwd_sources, bwd_sinks): (Vec<_>, Vec<_>) = sources
                        .iter()
                        .partition(|s| s.direction == crate::facts::TaintDirection::Forward);

                    if has_sinks {
                        for sink in &bwd_sinks {
                            for src in &fwd_sources {
                                let start_node =
                                    (src.infunc, src.vertex.0.clone(), src.vertex.1.clone());
                                let end_node =
                                    (sink.infunc, sink.vertex.0.clone(), sink.vertex.1.clone());
                                if let (Some(&start_id), Some(&end_id)) =
                                    (node_to_id.get(&start_node), node_to_id.get(&end_node))
                                    && seen_pairs.insert((start_id, end_id))
                                    && let Some(p) = find_path(g, start_id, end_id)
                                {
                                    results_by_path
                                        .entry(p)
                                        .or_insert((*fs_id, Vec::new()))
                                        .1
                                        .push(((*src).clone(), Some((*sink).clone()), lbl.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut needed_spans = ctx.source_spans.to_vec();
    let mut seen_sites: BTreeSet<(u32, u64)> = ctx
        .source_spans
        .iter()
        .map(|(_, f, i)| (f.id, i.id))
        .collect();

    let mut path_sites = BTreeSet::new();
    for path in results_by_path.keys() {
        for &node_id in path {
            let node = &id_to_node[node_id as usize];
            if let Some(site) = node_to_site.get(node)
                && seen_sites.insert((site.0.id, site.1.id))
            {
                path_sites.insert((site.0, site.1));
            }
        }
    }

    if !path_sites.is_empty() {
        let tainted = TaintedInstructions {
            tainted_insn: path_sites
                .into_iter()
                .map(|(f, i)| {
                    (
                        InsnSiteId::new(f, i).try_into().unwrap(),
                        Label(crate::facts::EMPTY_STR.clone()),
                        FlowVariable::default(),
                        Path::default(),
                    )
                })
                .collect(),
        };
        let path_spans = find_source_ids(
            &ctx.index_dir
                .as_ref()
                .join(schema::index_source_map::FILENAME),
            &tainted,
        )
        .await?;
        needed_spans.extend(path_spans);
    }

    let mut results: Vec<SarifResult> = Vec::new();
    let mut source_data = SourceLocationData::default();
    // Populate id_to_name with names from facts (as fallback)
    for (&id, name) in &ctx.facts.id_to_name {
        source_data.id_to_name.insert(id, name.clone());
    }
    populate_source_info(ctx, config, sarif_data, &mut source_data, &needed_spans).await?;

    let mut span_to_location: BTreeMap<u32, Location> = BTreeMap::new();
    for (file_span_id, _, _, location) in &source_data.batch_data {
        span_to_location.insert(*file_span_id, location.clone());
    }

    let mut code_flows_by_span: BTreeMap<u32, Vec<CodeFlow>> = BTreeMap::new();
    for (path, (file_span_id, _details)) in &results_by_path {
        let mut thread_flow_locations = Vec::new();
        let mut last_loc_id: Option<(&String, Option<String>)> = None;
        for &node_id in path {
            let node = &id_to_node[node_id as usize];
            if let Some(site) = node_to_site.get(node)
                && let Some(loc) = source_data.all_locations.get(&(site.0.id, site.1.id))
            {
                let current_loc_id = loc.physical_location.as_ref().and_then(|p| {
                    let uri = p.artifact_location.as_ref()?.uri.as_ref()?;
                    let pos = p
                        .address
                        .as_ref()
                        .and_then(|a| a.absolute_address.as_ref().map(|v| v.to_string()))
                        .or_else(|| {
                            p.region.as_ref().and_then(|r| {
                                Some(format!("{}:{}", r.start_line?, r.start_column?))
                            })
                        });
                    Some((uri, pos))
                });

                if current_loc_id.is_some() && current_loc_id == last_loc_id {
                    continue;
                }
                last_loc_id = current_loc_id;

                let mut loc_with_msg = loc.clone();
                loc_with_msg.message = Some(Message::builder().text(format!("{}", node.1)).build());
                thread_flow_locations
                    .push(ThreadFlowLocation::builder().location(loc_with_msg).build());
            }
        }

        if !thread_flow_locations.is_empty() {
            code_flows_by_span.entry(*file_span_id).or_default().push(
                CodeFlow::builder()
                    .thread_flows(vec![
                        ThreadFlow::builder()
                            .locations(thread_flow_locations)
                            .build(),
                    ])
                    .build(),
            );
        }
    }

    // Now build results for tainted instructions (only for Debug or Machine profiles)
    if config.profile == SarifProfile::Debug || config.profile == SarifProfile::Machine {
        let tainted_span_ids: BTreeSet<u32> =
            ctx.source_spans.iter().map(|(fs, _, _)| fs.0).collect();

        let mut results_by_span: BTreeMap<u32, SarifResult> = BTreeMap::new();
        for (file_span_id, func_id, insn_id, location) in &source_data.batch_data {
            if !tainted_span_ids.contains(file_span_id) {
                continue;
            }
            if results_by_span.contains_key(file_span_id) {
                continue;
            }

            let mut all_labels = BTreeSet::new();
            let mut labels_to_vertices: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            if let Some(details) = ctx.details_by_span.get(file_span_id) {
                for (lbl, _func_id, var, pth) in details {
                    all_labels.insert(lbl.clone());
                    let vertex = format!("{}{}", var, pth.to_dot_string());
                    labels_to_vertices
                        .entry(lbl.to_string())
                        .or_default()
                        .insert(vertex);
                }
            }

            let mut sorted_labels: Vec<String> =
                all_labels.into_iter().map(|l| l.to_string()).collect();
            sorted_labels.sort();

            let msg_text = if sorted_labels.is_empty() {
                format!("span {file_span_id}")
            } else {
                format!("Taint flow labelled '{}'", sorted_labels.join("', '"))
            };

            let mut final_msg_text = msg_text;
            if config.compact {
                const COMPACT_MAX_MESSAGE_CHARS: usize = 100;
                if let Some((byte_idx, _)) =
                    final_msg_text.char_indices().nth(COMPACT_MAX_MESSAGE_CHARS)
                {
                    final_msg_text.truncate(byte_idx);
                }
            }

            let mut additional_properties = BTreeMap::from([
                ("taintLabels".to_string(), serde_json::json!(sorted_labels)),
                (
                    "taintVertices".to_string(),
                    serde_json::json!(labels_to_vertices),
                ),
            ]);
            if config.profile == SarifProfile::Debug {
                additional_properties
                    .insert("fileSpanId".to_string(), serde_json::json!(*file_span_id));
                additional_properties.insert("funcId".to_string(), serde_json::json!(*func_id));
                additional_properties.insert("insnId".to_string(), serde_json::json!(*insn_id));
            }
            let properties = PropertyBag::builder()
                .additional_properties(additional_properties)
                .build();

            let result = SarifResult::builder()
                .rule_id(TAINTED_INSTRUCTION_RULE_ID.to_string())
                .kind(ResultKind::Informational)
                .level(ResultLevel::None)
                .message(Message::builder().text(final_msg_text).build())
                .locations(vec![location.clone()])
                .properties(properties)
                .build();

            results_by_span.insert(*file_span_id, result);
        }
        results.extend(results_by_span.into_values());
    }

    // Add source and sink location results (only for Debug profile)
    if config.profile == SarifProfile::Debug {
        results.extend(format_source_sink_results(
            sarif_data,
            &endpoints,
            &source_data.id_to_name,
            &node_to_site,
            &source_data.all_locations,
        ));
    }

    // Now build results for paths (for Human or Debug profiles, one per path)
    if config.profile == SarifProfile::Human || config.profile == SarifProfile::Debug {
        for (_path, (file_span_id, details)) in results_by_path {
            let location = if let Some(loc) = span_to_location.get(&file_span_id) {
                loc.clone()
            } else {
                continue;
            };

            let mut labels_set = BTreeSet::new();

            for (src, _sink, _) in &details {
                labels_set.insert(src.label.to_string());
            }

            let mut sorted_labels: Vec<String> = labels_set.into_iter().collect();
            sorted_labels.sort();

            let mut labels_to_vertices: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            if let Some(details) = ctx.details_by_span.get(&file_span_id) {
                for (lbl, _func_id, var, pth) in details {
                    let vertex = format!("{}{}", var, pth.to_dot_string());
                    labels_to_vertices
                        .entry(lbl.to_string())
                        .or_default()
                        .insert(vertex);
                }
            }

            let msg_text = format!("Taint flow labelled '{}'", sorted_labels.join("', '"));

            let mut final_msg_text = msg_text;
            if config.compact {
                const COMPACT_MAX_MESSAGE_CHARS: usize = 100;
                if let Some((byte_idx, _)) =
                    final_msg_text.char_indices().nth(COMPACT_MAX_MESSAGE_CHARS)
                {
                    final_msg_text.truncate(byte_idx);
                }
            }

            let additional_properties = BTreeMap::from([
                ("taintLabels".to_string(), serde_json::json!(sorted_labels)),
                (
                    "taintVertices".to_string(),
                    serde_json::json!(labels_to_vertices),
                ),
            ]);
            let properties = PropertyBag::builder()
                .additional_properties(additional_properties)
                .build();

            if let Some(code_flows) = code_flows_by_span.get(&file_span_id) {
                let result = SarifResult::builder()
                    .rule_id(TAINTED_PATH_RULE_ID.to_string())
                    .kind(ResultKind::Informational)
                    .level(ResultLevel::None)
                    .message(Message::builder().text(final_msg_text).build())
                    .locations(vec![location])
                    .properties(properties)
                    .code_flows(code_flows.clone())
                    .build();

                results.push(result);
            }
        }
    }

    Ok(results)
}

fn format_source_sink_results(
    sarif_data: &mut SarifData,
    endpoints: &BTreeSet<&QueryEndpoint>,
    id_to_name: &BTreeMap<u32, String>,
    node_to_site: &BTreeMap<(FunctionId, FlowVariable, Path), (FunctionId, InsnId)>,
    all_locations: &BTreeMap<(u32, u64), Location>,
) -> Vec<SarifResult> {
    let mut source_sink_results = Vec::new();

    // Collect all source and sink nodes with their endpoints
    for endpoint in endpoints {
        let is_source = endpoint.direction == crate::facts::TaintDirection::Forward;
        let is_sink = endpoint.direction == crate::facts::TaintDirection::Backward;

        let node = (
            endpoint.infunc,
            endpoint.vertex.0.clone(),
            endpoint.vertex.1.clone(),
        );
        // Use the logical location of the source, and use the physical location additionally if it's available
        if is_source || is_sink {
            let rule_id = if is_source {
                TAINT_SOURCE_RULE_ID
            } else {
                TAINT_SINK_RULE_ID
            };
            let msg_text = if is_source {
                format!(
                    "Source of tainted data: {} in function {}",
                    node.1,
                    id_to_name.get(&node.0.id).unwrap_or(&"unknown".to_string())
                )
            } else {
                format!(
                    "Sink of tainted data: {} in function {}",
                    node.1,
                    id_to_name.get(&node.0.id).unwrap_or(&"unknown".to_string())
                )
            };

            let fully_qualified_name = id_to_name
                .get(&node.0.id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let loc_idx = *sarif_data
                .global_logical_locations_map
                .entry(fully_qualified_name.clone())
                .or_insert_with(|| {
                    let idx = sarif_data.global_logical_locations.len();
                    sarif_data.global_logical_locations.push(
                        LogicalLocation::builder()
                            .kind("member")
                            .name(fully_qualified_name.clone())
                            .fully_qualified_name(fully_qualified_name)
                            .build(),
                    );
                    idx
                });

            let logical_location = LogicalLocation::builder().index(loc_idx as i64).build();
            let mut locations = vec![
                Location::builder()
                    .logical_locations(vec![logical_location.clone()])
                    .build(),
            ];

            if let Some(&site) = node_to_site.get(&node)
                && let Some(physical_loc) = all_locations.get(&(site.0.id, site.1.id))
            {
                let mut loc_with_phys = physical_loc.clone();
                loc_with_phys.logical_locations = Some(vec![logical_location]);
                locations = vec![loc_with_phys];
            }

            let result = SarifResult::builder()
                .rule_id(rule_id.to_string())
                .kind(ResultKind::Informational)
                .level(ResultLevel::None)
                .message(Message::builder().text(msg_text).build())
                .locations(locations)
                .build();

            source_sink_results.push(result);
        }
    }
    source_sink_results
}

/// Look up the sites in the index source map and returns the span ids
pub async fn find_source_ids(
    source_map: &path::Path,
    tainted: &TaintedInstructions,
) -> Result<Vec<(FileSpanId, FunctionId, InsnId)>, Error> {
    let mut ctx = SessionContext::new();
    ctx.register_parquet(
        "index_source_map",
        source_map.to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await
    .err_context(|| "register index_source_map")?;

    build_selector_table(&mut ctx, tainted)
        .await
        .err_context(|| "building selector tables")?;

    let sql = "
        SELECT index_source_map.source_span_id, index_source_map.func_id, index_source_map.insn_id
        FROM index_source_map
        JOIN site_id
        ON index_source_map.func_id = site_id.func_id
        AND index_source_map.insn_id = site_id.insn_id
        WHERE index_source_map.source_span_id != 0
        ORDER BY index_source_map.source_span_id
    ";

    let mut batches = ctx.sql(sql).await?.collect().await?;
    let mut result = Vec::new();

    for batch in batches.drain(..) {
        let span_ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let func_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let insn_ids = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();

        for i in 0..batch.num_rows() {
            let span_id = span_ids.value(i);
            let func_id = func_ids.value(i);
            let insn_id = insn_ids.value(i);
            result.push((
                FileSpanId(span_id),
                FunctionId::new(func_id),
                InsnId::new(insn_id),
            ));
        }
    }
    Ok(result)
}

/// Creates and registers a selector table 'site_id' with two columns: 'function_id' and 'insn_id'.
/// There's one row per tainted instruction.
async fn build_selector_table(
    ctx: &mut SessionContext,
    tainted: &TaintedInstructions,
) -> Result<(), Error> {
    let mut sites = BTreeSet::new();
    for (site_id, _, _, _) in &tainted.tainted_insn {
        let site_id = InsnSiteId::unpack(site_id).expect("unpack error");
        sites.insert((site_id.func_id.id, site_id.insn_id.id));
    }
    let tuples: Vec<_> = sites.into_iter().collect();
    let function_id_array =
        UInt32Array::from(tuples.iter().copied().map(|(id, _)| id).collect::<Vec<_>>());
    let insn_id_array =
        UInt64Array::from(tuples.iter().copied().map(|(_, id)| id).collect::<Vec<_>>());
    let schema = Arc::new(Schema::new(vec![
        Field::new("func_id", DataType::UInt32, false),
        Field::new("insn_id", DataType::UInt64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(function_id_array), Arc::new(insn_id_array)],
    )
    .err_context(|| "building selector table")?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    ctx.register_table("site_id", Arc::new(table))?;
    Ok(())
}
