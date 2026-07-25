/*! Format taint results

The taint engine produces taint information for each vertex in the target program. The goal of
this module is to compute location information for each tainted vertex and instruction. Since
most frontends only store instruction location information (as opposed to locations for each
variable access), we focus on instruction locations.

The main entry point is [`format_sarif`]. It reads the last query and formats its results in SARIF.

Format has four profiles:
- Human (the default)
- Machine
- Agent
- Debug

The human profile is designed for loading results into a visualizer for a human to look at.
It emphasizes clarity and the important steps that explain the finding.
The agent profile is almost an extension of the human, including sources & sinks found &
details of exactly what is tainted in each place. It's intended to communicate high level
findings as well as how exactly to reason about the chain. It also includes functions that
absorb taint, which allows agents to produce their own models to further the analysis.
The machine profile contains explicit detail about each individual finding -- tainted
instructions. The debug profile contains as much information as has been useful for debugging.

*/
use std::fs::File;
use std::path;
use std::sync::Arc;

use ctadl_ir::graph::{Annotation, DirectedGraph, LabeledSuccessors, find_annotated_path_to_set};
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
    Address, ArtifactLocation, CodeFlow, ConfigurationOverride, Invocation, Location,
    LogicalLocation, Message, MultiformatMessageString, Notification, PhysicalLocation,
    PropertyBag, Region, ReportingConfiguration, ReportingDescriptor, ReportingDescriptorReference,
    Result as SarifResult, ResultKind, ResultLevel, Run, Sarif, ThreadFlow, ThreadFlowLocation,
    Tool, ToolComponent,
};
use source_info::FileSpanId;
use source_info::{LineMap, offset_to_line_column};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ErrorContext};
use crate::facts::schema;
use crate::facts::{
    CallArgId, FlowEdge, FlowVariable, FlowVertex, FormalIndex, FunctionId, InsnId, InsnSiteId,
    Label, PackedInsnSiteId, Path, TaintDirection, TaintState,
};
use crate::models::{EndpointStats, UnmatchedReason};
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
    /// Base address the disassembler loaded the artifact at, if known. Used to
    /// emit `relativeAddress` (the section-relative offset) alongside the
    /// absolute instruction address in binary SARIF locations.
    pub image_base: Option<i64>,
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

const ABSORBING_FUNCTION_RULE_ID: &str = "C0007.absorbing-function";
const ABSORBING_FUNCTION_RULE_NAME: &str = "Absorbing functions";
const ABSORBING_FUNCTION_RULE_DESCRIPTION: &str = "An external function that receives tainted data";

// Notification descriptor ids (SARIF §3.19.24, referenced from
// `invocation.toolConfigurationNotifications` / `toolExecutionNotifications`). The
// `CTADL00xx` block describes how the query was *configured*; the `CTADL01xx` block
// describes what running it *did*. See `notification_descriptors` for the message
// strings, and `build_invocation` for the conditions.
const NOTIF_NO_ENDPOINTS: &str = "CTADL0001.no-endpoints-configured";
const NOTIF_NO_SOURCES_CONFIGURED: &str = "CTADL0002.no-sources-configured";
const NOTIF_NO_SINKS_CONFIGURED: &str = "CTADL0003.no-sinks-configured";
const NOTIF_GENERATOR_DEAD: &str = "CTADL0004.generator-matched-nothing";
const NOTIF_FUNCTION_NOT_INDEXED: &str = "CTADL0005.endpoint-function-not-indexed";
const NOTIF_NO_SOURCES_MATCHED: &str = "CTADL0006.no-sources-matched";
const NOTIF_NO_SINKS_MATCHED: &str = "CTADL0007.no-sinks-matched";
const NOTIF_MATCH_SUMMARY: &str = "CTADL0100.endpoint-match-summary";
const NOTIF_PATHS_DISABLED: &str = "CTADL0101.path-generation-disabled";
const NOTIF_NO_PATHS: &str = "CTADL0102.no-paths-found";
const NOTIF_PATH_DROPPED: &str = "CTADL0103.path-dropped-no-location";

/// Everything the SARIF writer needs to explain what the query *did*, independent of what
/// it *found*. Assembled in `cli::query`, which is the only place that sees both the model
/// files and the resolved endpoints.
///
/// The `_declared` counts are model *ports* — one per `sources`/`sinks` entry, plus one per
/// flowy endpoint; the `_matched` counts are the post-fan-out `QueryEndpoint`s those ports
/// resolved to. One port can match many functions, so the two are not equal, but they are
/// the two ends of one fan-out and every message that prints them names which is which.
#[derive(Debug, Default, Clone)]
pub struct QueryDiagnostics {
    /// What Stage 1 did, keyed by (model file, generator index, direction). A zero
    /// `endpoints_matched` is a generator that declared a port and matched nothing.
    pub generator_stats: BTreeMap<(path::PathBuf, usize, TaintDirection), EndpointStats>,
    /// Names Stage 1 matched that the index does not contain (see [`BuiltEndpoints`]).
    ///
    /// [`BuiltEndpoints`]: crate::query_engine::BuiltEndpoints
    pub unresolved_functions: BTreeSet<String>,
    pub sources_declared: usize,
    pub sinks_declared: usize,
    pub sources_matched: usize,
    pub sinks_matched: usize,
    pub command_line: String,
    pub arguments: Vec<String>,
    /// SARIF-format UTC timestamp; see [`utc_timestamp`].
    pub start_time_utc: String,
}

/// What happened to the `C0001.tainted-path` rule this run. Exactly one of these holds, and
/// it is what the single non-`fail` `C0001` result (if any) reports — see
/// [`path_status_result`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathOutcome {
    /// The profile does not perform path search at all, so the rule did not run.
    Disabled,
    /// One end of the query was empty, so the rule could not be evaluated. Carries the
    /// human-readable reason.
    NotApplicable(String),
    /// Both ends matched and the search completed without finding a flow.
    NoneFound,
    /// Flows were found and reported as `kind: "fail"` results.
    Found(usize),
}

/// Counts collected while assembling path results, for the `CTADL01xx` notifications.
#[derive(Debug, Default, Clone, Copy)]
struct PathStats {
    /// `C0001` results actually emitted.
    reported: usize,
    /// Paths that were found but discarded for want of a resolvable reporting location.
    dropped_no_location: usize,
}

impl PathStats {
    fn merge(&mut self, other: PathStats) {
        self.reported += other.reported;
        self.dropped_no_location += other.dropped_no_location;
    }
}

/// Assemble `run.invocations[0]`: the configuration and execution notifications, the rules
/// the profile turned off, and the whole-run status.
///
/// Per SARIF §3.20.21/§3.20.22 the presence of any `error`-level notification means the run
/// failed, so `execution_successful` is exactly "no error notification was emitted", and the
/// caller sets a non-zero process exit code to match (§3.58.6).
fn build_invocation(
    diagnostics: &QueryDiagnostics,
    config: &FormatConfig,
    outcome: &PathOutcome,
    path_stats: PathStats,
) -> Invocation {
    let mut config_notifications: Vec<Notification> = Vec::new();
    let mut exec_notifications: Vec<Notification> = Vec::new();

    let model_files: BTreeSet<&path::Path> = diagnostics
        .generator_stats
        .keys()
        .map(|(f, _, _)| f.as_path())
        .collect();

    // --- Configuration: what the models declared (§3.20.22). ---
    if diagnostics.sources_declared == 0 && diagnostics.sinks_declared == 0 {
        config_notifications.push(notification(
            NOTIF_NO_ENDPOINTS,
            "error",
            vec![model_files.len().to_string()],
            format!(
                "No taint sources or sinks were configured ({} model file(s) loaded, no \
                 built-in endpoints), so the query has nothing to search for.",
                model_files.len()
            ),
            BTreeMap::from([(
                "modelFiles".to_string(),
                serde_json::json!(model_files.len()),
            )]),
        ));
    } else if diagnostics.sources_declared == 0 {
        config_notifications.push(notification(
            NOTIF_NO_SOURCES_CONFIGURED,
            "error",
            vec![diagnostics.sinks_declared.to_string()],
            format!(
                "No taint sources were configured, but {} sink port(s) were declared. A taint \
                 query with no source is vacuous.",
                diagnostics.sinks_declared
            ),
            BTreeMap::from([(
                "sinksDeclared".to_string(),
                serde_json::json!(diagnostics.sinks_declared),
            )]),
        ));
    } else if diagnostics.sinks_declared == 0 {
        config_notifications.push(notification(
            NOTIF_NO_SINKS_CONFIGURED,
            "error",
            vec![diagnostics.sources_declared.to_string()],
            format!(
                "No taint sinks were configured, but {} source port(s) were declared. A taint \
                 query with no sink is vacuous.",
                diagnostics.sources_declared
            ),
            BTreeMap::from([(
                "sourcesDeclared".to_string(),
                serde_json::json!(diagnostics.sources_declared),
            )]),
        ));
    }

    // CTADL0004: a generator that declared a port and produced no endpoint. The
    // notification points at the model *file* — `EndpointRow` carries no provenance and
    // serde_json gives no spans, so there is no line/column to point at. Which of the
    // several ways it can produce nothing happened is what `unmatched_message` reports:
    // "matched no function" is only one of them.
    let mut unmatched_declarations = 0usize;
    let mut ports_unmatched = 0usize;
    for ((file, index, direction), stats) in &diagnostics.generator_stats {
        if stats.endpoints_matched > 0 {
            continue;
        }
        unmatched_declarations += 1;
        ports_unmatched += stats.ports_declared;
        let direction = match direction {
            TaintDirection::Forward => "source",
            TaintDirection::Backward => "sink",
        };
        let file_display = file.display().to_string();
        let detail = unmatched_message(*index, &file_display, direction, stats);
        let mut properties = BTreeMap::from([
            ("generatorIndex".to_string(), serde_json::json!(index)),
            ("direction".to_string(), serde_json::json!(direction)),
            (
                "portsDeclared".to_string(),
                serde_json::json!(stats.ports_declared),
            ),
            (
                "functionsMatched".to_string(),
                serde_json::json!(stats.functions_matched),
            ),
            ("reason".to_string(), serde_json::json!(detail.message_id)),
        ]);
        if let Some(name) = &detail.variable {
            properties.insert("variableName".to_string(), serde_json::json!(name));
        }
        let mut notif = notification_with_message_id(
            NOTIF_GENERATOR_DEAD,
            detail.message_id,
            "warning",
            detail.arguments,
            detail.text,
            properties,
        );
        notif.locations = Some(vec![
            Location::builder()
                .physical_location(
                    PhysicalLocation::builder()
                        .artifact_location(
                            ArtifactLocation::builder().uri(artifact_uri(file)).build(),
                        )
                        .build(),
                )
                .build(),
        ]);
        config_notifications.push(notif);
    }

    // CTADL0005: Stage 1 matched a name that Stage 2 could not resolve against the index.
    for name in &diagnostics.unresolved_functions {
        config_notifications.push(notification(
            NOTIF_FUNCTION_NOT_INDEXED,
            "warning",
            vec![name.clone()],
            format!(
                "Function '{name}' was matched by a model but is not present in the index, so \
                 its source/sink endpoints were dropped."
            ),
            BTreeMap::from([("function".to_string(), serde_json::json!(name))]),
        ));
    }

    // CTADL0006/0007: declared but nothing survived to a `QueryEndpoint`. These carry
    // `associatedRule` (§3.58.3) because they explain why `C0001` could not be evaluated.
    let associate_c0001 = |mut notif: Notification| -> Notification {
        notif.associated_rule = Some(
            ReportingDescriptorReference::builder()
                .id(TAINTED_PATH_RULE_ID.to_string())
                .build(),
        );
        notif
    };
    if diagnostics.sources_declared > 0 && diagnostics.sources_matched == 0 {
        config_notifications.push(associate_c0001(notification(
            NOTIF_NO_SOURCES_MATCHED,
            "error",
            vec![diagnostics.sources_declared.to_string()],
            format!(
                "{} source port(s) were declared but none of them matched anything in the \
                 program, so no taint could be seeded.",
                diagnostics.sources_declared
            ),
            BTreeMap::from([(
                "sourcesDeclared".to_string(),
                serde_json::json!(diagnostics.sources_declared),
            )]),
        )));
    }
    if diagnostics.sinks_declared > 0 && diagnostics.sinks_matched == 0 {
        config_notifications.push(associate_c0001(notification(
            NOTIF_NO_SINKS_MATCHED,
            "error",
            vec![diagnostics.sinks_declared.to_string()],
            format!(
                "{} sink port(s) were declared but none of them matched anything in the \
                 program, so no flow could be detected.",
                diagnostics.sinks_declared
            ),
            BTreeMap::from([(
                "sinksDeclared".to_string(),
                serde_json::json!(diagnostics.sinks_declared),
            )]),
        )));
    }

    // --- Execution: what running the query did (§3.20.21). ---
    // Declared and matched are the two ends of one fan-out — ports in, endpoints out — so
    // the message states both units rather than printing the numbers side by side as though
    // they were the same thing. `ports_unmatched` is in port units too: it sums the ports of
    // every declaration that produced no endpoint.
    exec_notifications.push(notification(
        NOTIF_MATCH_SUMMARY,
        "note",
        vec![
            diagnostics.sources_declared.to_string(),
            diagnostics.sinks_declared.to_string(),
            diagnostics.sources_matched.to_string(),
            diagnostics.sinks_matched.to_string(),
            ports_unmatched.to_string(),
        ],
        format!(
            "Declared {} source and {} sink port(s), which matched {} source and {} sink \
             endpoint(s) in the program; {} declared port(s) matched nothing.",
            diagnostics.sources_declared,
            diagnostics.sinks_declared,
            diagnostics.sources_matched,
            diagnostics.sinks_matched,
            ports_unmatched
        ),
        BTreeMap::from([
            (
                "sourcesDeclared".to_string(),
                serde_json::json!(diagnostics.sources_declared),
            ),
            (
                "sinksDeclared".to_string(),
                serde_json::json!(diagnostics.sinks_declared),
            ),
            (
                "sourcesMatched".to_string(),
                serde_json::json!(diagnostics.sources_matched),
            ),
            (
                "sinksMatched".to_string(),
                serde_json::json!(diagnostics.sinks_matched),
            ),
            (
                "portsDeclared".to_string(),
                serde_json::json!(diagnostics.sources_declared + diagnostics.sinks_declared),
            ),
            (
                "portsUnmatched".to_string(),
                serde_json::json!(ports_unmatched),
            ),
            // One per `CTADL0004` above: the (generator, direction) declarations that
            // produced no endpoint. In generator-direction units, not ports.
            (
                "unmatchedDeclarations".to_string(),
                serde_json::json!(unmatched_declarations),
            ),
        ]),
    ));

    match outcome {
        PathOutcome::Disabled => {
            let profile = format!("{:?}", config.profile).to_lowercase();
            exec_notifications.push(notification(
                NOTIF_PATHS_DISABLED,
                "note",
                vec![profile.clone(), TAINTED_PATH_RULE_ID.to_string()],
                format!(
                    "The '{profile}' SARIF profile does not perform source-to-sink path search, \
                     so this run cannot produce '{TAINTED_PATH_RULE_ID}' results."
                ),
                BTreeMap::from([("profile".to_string(), serde_json::json!(profile))]),
            ));
        }
        PathOutcome::NoneFound => {
            exec_notifications.push(notification(
                NOTIF_NO_PATHS,
                "note",
                vec![
                    diagnostics.sources_matched.to_string(),
                    diagnostics.sinks_matched.to_string(),
                ],
                format!(
                    "Path search completed over {} source and {} sink endpoints and found no \
                     source-to-sink flow.",
                    diagnostics.sources_matched, diagnostics.sinks_matched
                ),
                BTreeMap::new(),
            ));
        }
        PathOutcome::NotApplicable(_) | PathOutcome::Found(_) => {}
    }

    if path_stats.dropped_no_location > 0 {
        exec_notifications.push(notification(
            NOTIF_PATH_DROPPED,
            "warning",
            vec![path_stats.dropped_no_location.to_string()],
            format!(
                "{} source-to-sink path(s) were found but discarded because no reporting \
                 location could be resolved for them.",
                path_stats.dropped_no_location
            ),
            BTreeMap::from([(
                "droppedPaths".to_string(),
                serde_json::json!(path_stats.dropped_no_location),
            )]),
        ));
    }

    // §3.20.22 / §3.20.21: an error-level notification in either array means the run failed.
    let is_error = |n: &Notification| n.level.as_ref().and_then(|l| l.as_str()) == Some("error");
    let execution_successful =
        !config_notifications.iter().any(is_error) && !exec_notifications.iter().any(is_error);

    let rule_configuration_overrides: Vec<ConfigurationOverride> = disabled_rules(config.profile)
        .into_iter()
        .map(|rule_id| {
            ConfigurationOverride::builder()
                .descriptor(
                    ReportingDescriptorReference::builder()
                        .id(rule_id.to_string())
                        .build(),
                )
                .configuration(ReportingConfiguration::builder().enabled(false).build())
                .build()
        })
        .collect();

    Invocation::builder()
        .execution_successful(execution_successful)
        .exit_code(if execution_successful { 0 } else { 1 })
        .exit_code_description(if execution_successful {
            "query completed".to_string()
        } else {
            "query configuration produced no analyzable endpoints".to_string()
        })
        .command_line(diagnostics.command_line.clone())
        .arguments(diagnostics.arguments.clone())
        .start_time_utc(diagnostics.start_time_utc.clone())
        .end_time_utc(utc_timestamp())
        .tool_configuration_notifications(config_notifications)
        .tool_execution_notifications(exec_notifications)
        .rule_configuration_overrides(rule_configuration_overrides)
        // No `environmentVariables`: §3.20.20 NOTE 2 — it leaks credentials.
        .build()
}

/// Why the taint query has an empty end, if it does. `Some` means `C0001` cannot be
/// evaluated at all: there is nothing to search from, or nothing to search for.
pub fn empty_end_reason(diagnostics: &QueryDiagnostics) -> Option<String> {
    match (
        diagnostics.sources_declared,
        diagnostics.sinks_declared,
        diagnostics.sources_matched,
        diagnostics.sinks_matched,
    ) {
        (0, 0, _, _) => Some("no taint sources or sinks were configured".to_string()),
        (0, _, _, _) => Some("no taint sources were configured".to_string()),
        (_, 0, _, _) => Some("no taint sinks were configured".to_string()),
        (_, _, 0, 0) => Some("no configured source or sink matched the program".to_string()),
        (_, _, 0, _) => Some("no configured source matched the program".to_string()),
        (_, _, _, 0) => Some("no configured sink matched the program".to_string()),
        _ => None,
    }
}

/// The single `C0001` result reporting the rule's *evaluation state* when no path result
/// was emitted (§3.27.9).
///
/// `notApplicable` means the rule was not evaluated because one end of the query was empty.
/// `open` means it *was* evaluated and the tool has insufficient information to decide —
/// CTADL does not prove the absence of a flow, so `pass` would overclaim. When the profile
/// disabled path search there is no result at all; the `ruleConfigurationOverride` and
/// `CTADL0101` say why.
fn path_status_result(outcome: &PathOutcome) -> Option<SarifResult> {
    let (kind, text) = match outcome {
        PathOutcome::Disabled | PathOutcome::Found(_) => return None,
        PathOutcome::NotApplicable(reason) => (
            ResultKind::NotApplicable,
            format!("Taint path analysis was not applicable: {reason}."),
        ),
        PathOutcome::NoneFound => (
            ResultKind::Open,
            "Taint path analysis ran to completion and found no source-to-sink flow. CTADL \
             does not prove the absence of a flow, so this is reported as 'open' rather than \
             'pass'."
                .to_string(),
        ),
    };
    Some(
        SarifResult::builder()
            .rule_id(TAINTED_PATH_RULE_ID.to_string())
            .kind(kind)
            // §3.27.10: when `kind` is not "fail", `level` SHALL be "none".
            .level(ResultLevel::None)
            .message(Message::builder().text(text).build())
            .build(),
    )
}

/// A `file:` URI for a path a notification points at, falling back to the lossy display
/// form when the path cannot be made absolute (SARIF wants a URI, but a readable relative
/// path beats no location at all).
fn artifact_uri(p: &path::Path) -> String {
    path::absolute(p)
        .ok()
        .and_then(|abs| url::Url::from_file_path(abs).ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| p.to_string_lossy().replace('\\', "/"))
}

/// A UTC timestamp in the `date-time` form SARIF requires (§3.9).
pub fn utc_timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Whether `profile` runs the source → sink path search that produces `C0001` results.
/// This is the same condition the graph construction and the path-result loop use.
fn profile_finds_paths(profile: SarifProfile) -> bool {
    matches!(
        profile,
        SarifProfile::Human | SarifProfile::Debug | SarifProfile::Agent
    )
}

/// The rules `profile` does not evaluate, reported as `invocation.ruleConfigurationOverrides`
/// so the SARIF says why a rule produced nothing instead of leaving it to be inferred from
/// an absence. Mirrors the profile gates in `format_source_info_results`; keep in sync.
fn disabled_rules(profile: SarifProfile) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !profile_finds_paths(profile) {
        out.push(TAINTED_PATH_RULE_ID);
    }
    if !matches!(profile, SarifProfile::Machine | SarifProfile::Debug) {
        out.push(TAINTED_INSTRUCTION_RULE_ID);
    }
    // C0003/C0004 are emitted in every profile and so are never overridden.
    // C0005 and C0006 are declared in the rules array but no code path emits them; marking
    // them permanently disabled stops the log advertising results the tool cannot produce.
    out.push(TAINTED_DATA_RULE_ID);
    out.push(ALMOST_PATH_FUNCTION_RULE_ID);
    if profile != SarifProfile::Agent {
        out.push(ABSORBING_FUNCTION_RULE_ID);
    }
    out
}

/// Build a notification carrying `message.id` + `arguments` (so consumers can read it
/// structurally rather than by parsing prose) plus a `properties` bag of raw counts.
fn notification(
    descriptor_id: &str,
    level: &str,
    arguments: Vec<String>,
    text: String,
    properties: BTreeMap<String, serde_json::Value>,
) -> Notification {
    notification_with_message_id(descriptor_id, "default", level, arguments, text, properties)
}

/// [`notification`] for a descriptor that declares more than one message string. `message_id`
/// selects which (§3.11.7) and must be a key of the descriptor's `messageStrings`, otherwise
/// a consumer re-rendering the message from `arguments` has nothing to render.
fn notification_with_message_id(
    descriptor_id: &str,
    message_id: &str,
    level: &str,
    arguments: Vec<String>,
    text: String,
    properties: BTreeMap<String, serde_json::Value>,
) -> Notification {
    let mut notif = Notification::builder()
        .descriptor(
            ReportingDescriptorReference::builder()
                .id(descriptor_id.to_string())
                .build(),
        )
        // `level` is `Option<serde_json::Value>` in serde-sarif (unlike `ResultLevel`), so
        // the enum value goes in as a JSON string.
        .level(serde_json::json!(level))
        .message(
            // Both forms: `text` for a human reading the file, `id` + `arguments` so a
            // consumer can read the condition without parsing prose.
            Message::builder()
                .id(message_id.to_string())
                .arguments(arguments)
                .text(text)
                .build(),
        )
        .build();
    // Set after building: the generated builder is type-state, so an optional field cannot
    // be assigned conditionally.
    if !properties.is_empty() {
        notif.properties = Some(
            PropertyBag::builder()
                .additional_properties(properties)
                .build(),
        );
    }
    notif
}

/// Declare a notification descriptor with a single `default` message string. The `{n}`
/// placeholders are filled from each notification's `message.arguments`.
fn notification_descriptor(
    id: &str,
    name: &str,
    description: &str,
    message: &str,
) -> ReportingDescriptor {
    notification_descriptor_multi(id, name, description, &[("default", message)])
}

/// [`notification_descriptor`] for a notification whose prose depends on which of several
/// conditions fired, one `messageStrings` entry per condition (§3.49.11). The entry a given
/// notification selects is its `message.id`; `default` is the one used when the condition is
/// not known more precisely.
fn notification_descriptor_multi(
    id: &str,
    name: &str,
    description: &str,
    messages: &[(&str, &str)],
) -> ReportingDescriptor {
    ReportingDescriptor::builder()
        .id(id.to_string())
        .name(name.to_string())
        .short_description(
            MultiformatMessageString::builder()
                .text(description.to_string())
                .build(),
        )
        .message_strings(
            messages
                .iter()
                .map(|(key, text)| {
                    (
                        (*key).to_string(),
                        MultiformatMessageString::builder()
                            .text((*text).to_string())
                            .build(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
        .build()
}

/// The `message.id`s [`unmatched_message`] can select on `CTADL0004`, one per way a declared
/// port can produce no endpoint. Also the notification's `properties.reason`, so a consumer
/// can branch on the cause without parsing prose.
const DEAD_UNKNOWN: &str = "default";
const DEAD_NO_FUNCTION: &str = "noFunctionMatched";
const DEAD_LOCAL_NOT_FOUND: &str = "localNotFound";
const DEAD_NO_CALLER: &str = "noCallerMatched";
const DEAD_PORT_REJECTED: &str = "portRejected";
const DEAD_MIXED: &str = "mixedReasons";

/// The prose, `message.id` and `message.arguments` for one `CTADL0004`.
struct UnmatchedMessage {
    message_id: &'static str,
    arguments: Vec<String>,
    text: String,
    /// The `Variable(name)` the port named, when that is what failed to resolve.
    variable: Option<String>,
}

/// Explain *why* a generator's declaration produced no endpoint.
///
/// The count reaching zero is not by itself evidence that nothing matched the generator's
/// `where` constraints: functions can match and the port still resolve in none of them.
/// Saying "matched no function" in that case sends the reader to rewrite a constraint that
/// was working. Every arm shares `{0}` = generator index, `{1}` = model file, `{2}` =
/// direction; later placeholders are arm-specific.
fn unmatched_message(
    index: usize,
    file: &str,
    direction: &str,
    stats: &EndpointStats,
) -> UnmatchedMessage {
    let head = vec![index.to_string(), file.to_string(), direction.to_string()];
    let functions = stats.functions_matched;
    // A generator declaring several ports of one direction can fail for several reasons at
    // once; naming one of them would misreport the others.
    let mut reasons = stats.unmatched.iter();
    let (first, rest) = (reasons.next(), reasons.next());
    match (first, rest) {
        (Some(UnmatchedReason::NoFunctionMatched), None) => UnmatchedMessage {
            message_id: DEAD_NO_FUNCTION,
            arguments: head,
            text: format!(
                "Model generator {index} in '{file}' declares a {direction}, but no function in \
                 the program matched its 'where' constraints, so it contributes nothing to the \
                 query."
            ),
            variable: None,
        },
        (Some(UnmatchedReason::LocalNotFound(name)), None) => UnmatchedMessage {
            message_id: DEAD_LOCAL_NOT_FOUND,
            arguments: {
                let mut a = head;
                a.push(name.clone());
                a.push(functions.to_string());
                a
            },
            text: format!(
                "Model generator {index} in '{file}' declares a {direction} on port \
                 'Variable({name})': its 'where' constraints matched {functions} function(s), but \
                 none of them has a local variable named '{name}', so it contributes nothing to \
                 the query."
            ),
            variable: Some(name.clone()),
        },
        (Some(UnmatchedReason::NoCallerMatched), None) => UnmatchedMessage {
            message_id: DEAD_NO_CALLER,
            arguments: {
                let mut a = head;
                a.push(functions.to_string());
                a
            },
            text: format!(
                "Model generator {index} in '{file}' declares a {direction} at call sites: its \
                 'where' constraints matched {functions} function(s), but no caller of them \
                 satisfied its 'in_function' constraint, so it contributes nothing to the query."
            ),
            variable: None,
        },
        (Some(UnmatchedReason::PortRejected), None) => UnmatchedMessage {
            message_id: DEAD_PORT_REJECTED,
            arguments: head,
            text: format!(
                "Model generator {index} in '{file}' declares a {direction} on a port that is not \
                 valid for its 'find' method, so it contributes nothing to the query."
            ),
            variable: None,
        },
        (Some(_), Some(_)) => {
            let joined = stats
                .unmatched
                .iter()
                .map(unmatched_reason_phrase)
                .collect::<Vec<_>>()
                .join("; ");
            UnmatchedMessage {
                message_id: DEAD_MIXED,
                arguments: {
                    let mut a = head;
                    a.push(stats.ports_declared.to_string());
                    a.push(joined.clone());
                    a
                },
                text: format!(
                    "Model generator {index} in '{file}' declares {} {direction} port(s), none of \
                     which matched anything in the program ({joined}), so it contributes nothing \
                     to the query.",
                    stats.ports_declared
                ),
                variable: None,
            }
        }
        // Unreachable in practice: Stage 1 records a reason on every zero-row path. Fall back
        // to prose that claims no cause rather than to one that might be wrong.
        (None, _) => UnmatchedMessage {
            message_id: DEAD_UNKNOWN,
            arguments: head,
            text: format!(
                "Model generator {index} in '{file}' declares a {direction} that matched nothing \
                 in the program, so it contributes nothing to the query."
            ),
            variable: None,
        },
    }
}

/// One clause of the `mixedReasons` list.
fn unmatched_reason_phrase(reason: &UnmatchedReason) -> String {
    match reason {
        UnmatchedReason::NoFunctionMatched => {
            "no function matched its 'where' constraints".to_string()
        }
        UnmatchedReason::LocalNotFound(name) => {
            format!("no matched function has a local named '{name}'")
        }
        UnmatchedReason::NoCallerMatched => {
            "no caller satisfied its 'in_function' constraint".to_string()
        }
        UnmatchedReason::PortRejected => "a port is not valid for its 'find' method".to_string(),
    }
}

/// The `CTADL00xx`/`CTADL01xx` descriptors, declared once in `tool.driver.notifications`
/// (§3.19.24) so every notification's `message.id` resolves.
fn notification_descriptors() -> Vec<ReportingDescriptor> {
    vec![
        notification_descriptor(
            NOTIF_NO_ENDPOINTS,
            "No endpoints configured",
            "Neither a source nor a sink was declared anywhere",
            "No taint sources or sinks were configured ({0} model file(s) loaded, no built-in \
             endpoints), so the query has nothing to search for.",
        ),
        notification_descriptor(
            NOTIF_NO_SOURCES_CONFIGURED,
            "No sources configured",
            "Sinks were declared but no source was",
            "No taint sources were configured, but {0} sink endpoint(s) were. A taint query \
             with no source is vacuous.",
        ),
        notification_descriptor(
            NOTIF_NO_SINKS_CONFIGURED,
            "No sinks configured",
            "Sources were declared but no sink was",
            "No taint sinks were configured, but {0} source endpoint(s) were. A taint query \
             with no sink is vacuous.",
        ),
        notification_descriptor_multi(
            NOTIF_GENERATOR_DEAD,
            "Model generator matched nothing",
            "A model generator declared a port that produced no endpoint",
            // One message per cause; see `unmatched_message`. `{0}` = generator index,
            // `{1}` = model file, `{2}` = direction throughout.
            &[
                (
                    DEAD_UNKNOWN,
                    "Model generator {0} in '{1}' declares a {2} that matched nothing in the \
                     program, so it contributes nothing to the query.",
                ),
                (
                    DEAD_NO_FUNCTION,
                    "Model generator {0} in '{1}' declares a {2}, but no function in the program \
                     matched its 'where' constraints, so it contributes nothing to the query.",
                ),
                (
                    DEAD_LOCAL_NOT_FOUND,
                    "Model generator {0} in '{1}' declares a {2} on port 'Variable({3})': its \
                     'where' constraints matched {4} function(s), but none of them has a local \
                     variable named '{3}', so it contributes nothing to the query.",
                ),
                (
                    DEAD_NO_CALLER,
                    "Model generator {0} in '{1}' declares a {2} at call sites: its 'where' \
                     constraints matched {3} function(s), but no caller of them satisfied its \
                     'in_function' constraint, so it contributes nothing to the query.",
                ),
                (
                    DEAD_PORT_REJECTED,
                    "Model generator {0} in '{1}' declares a {2} on a port that is not valid for \
                     its 'find' method, so it contributes nothing to the query.",
                ),
                (
                    DEAD_MIXED,
                    "Model generator {0} in '{1}' declares {3} {2} port(s), none of which matched \
                     anything in the program ({4}), so it contributes nothing to the query.",
                ),
            ],
        ),
        notification_descriptor(
            NOTIF_FUNCTION_NOT_INDEXED,
            "Endpoint function not indexed",
            "A matched function name is absent from the index",
            "Function '{0}' was matched by a model but is not present in the index, so its \
             source/sink endpoints were dropped.",
        ),
        notification_descriptor(
            NOTIF_NO_SOURCES_MATCHED,
            "No sources matched",
            "Sources were declared but none matched the program",
            "{0} source port(s) were declared but none of them matched anything in the program, \
             so no taint could be seeded.",
        ),
        notification_descriptor(
            NOTIF_NO_SINKS_MATCHED,
            "No sinks matched",
            "Sinks were declared but none matched the program",
            "{0} sink port(s) were declared but none of them matched anything in the program, so \
             no flow could be detected.",
        ),
        notification_descriptor(
            NOTIF_MATCH_SUMMARY,
            "Endpoint match summary",
            "How many ports the models declared and how many endpoints they matched",
            "Declared {0} source and {1} sink port(s), which matched {2} source and {3} sink \
             endpoint(s) in the program; {4} declared port(s) matched nothing.",
        ),
        notification_descriptor(
            NOTIF_PATHS_DISABLED,
            "Path generation disabled",
            "The selected SARIF profile does not perform path search",
            "The '{0}' SARIF profile does not perform source-to-sink path search, so this run \
             cannot produce '{1}' results.",
        ),
        notification_descriptor(
            NOTIF_NO_PATHS,
            "No paths found",
            "Path search completed without finding a flow",
            "Path search completed over {0} source and {1} sink endpoints and found no \
             source-to-sink flow.",
        ),
        notification_descriptor(
            NOTIF_PATH_DROPPED,
            "Path dropped for want of a location",
            "A path was found but could not be reported",
            "{0} source-to-sink path(s) were found but discarded because no reporting location \
             could be resolved for them.",
        ),
    ]
}

#[derive(Default, Builder, Clone)]
pub struct FormatFacts {
    /// Taint results on variables
    #[builder(default)]
    pub taint: Vec<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)>,
    /// Taint-graph edges in execution / data-flow order, exactly as the query
    /// engine produced and persisted them (`schema::taint_edge`): the source
    /// vertex `(src_func, src_var, src_path)` flows to the destination vertex
    /// `(dst_func, dst_var, dst_path)`, and `edge` is the [`FlowEdge`]
    /// classifying the step (`Intra`/`Call`/`Return`). The formatter loads these
    /// rather than recomputing them, and drives its realizable-path search off
    /// them (see [`build_taint_flow_graph`]).
    #[builder(default)]
    pub taint_edge: Vec<(
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )>,
    #[builder(default)]
    pub actual_param: Vec<(PackedInsnSiteId, FormalIndex, FlowVariable, Path)>,
    #[builder(default)]
    pub call: Vec<(PackedInsnSiteId, FunctionId)>,
    #[builder(default)]
    pub id_to_name: BTreeMap<u32, String>,
}

pub struct TaintedInstructions {
    // (site id, label, variable, access path)
    pub tainted_insn: Vec<(PackedInsnSiteId, Label, FlowVariable, Path)>,
}

pub struct TaintAnalysisResults {
    /// Taint-graph edges in execution / data-flow order, as loaded from the query
    /// engine's persisted `taint_edge` (see [`FormatFacts::taint_edge`]). Each
    /// tuple is `(edge, src_func, src_var, src_path, dst_func, dst_var,
    /// dst_path)`: the source vertex flows to the destination vertex, and `edge`
    /// is the [`FlowEdge`] classifying the step as `Intra`, `Call`, or `Return`.
    /// Call/return matching is *not* baked into the vertices (they carry no taint
    /// state); it is recovered on the fly by the realizable-path search, which
    /// carries a [`TaintState`] annotation that evolves along these edge labels.
    pub edges: Vec<(
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )>,
    pub tainted_insns: TaintedInstructions,
    pub absorbing_functions: Vec<(FunctionId, QueryEndpoint, FormalIndex)>,
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

impl TaintAnalysisResults {
    /// Repackages the taint pass's output for the formatter. The taint closure,
    /// taint graph, and instruction-level facts are all computed in a single
    /// [`taint_analysis`](crate::query_engine::taint_analysis) pass; this just
    /// borrows the pieces the formatter reads. No taint is (re)computed here.
    pub fn from_query_result(result: &crate::query_engine::QueryResult) -> Self {
        TaintAnalysisResults {
            edges: result.taint_edge.clone(),
            tainted_insns: TaintedInstructions {
                tainted_insn: result.tainted_insn.clone(),
            },
            absorbing_functions: result.absorbing_functions.clone(),
        }
    }
}

/// A taint dataflow graph whose edges are labeled with a [`FlowEdge`]. The
/// labels are what a realizable-path search inspects: it carries a [`TaintState`]
/// annotation that evolves across `Intra`/`Call`/`Return` edges (see the
/// [`Annotation`] impl for [`TaintState`]) to keep call/return matching, so the
/// graph nodes themselves need not be taint-state-qualified.
pub struct LabeledTaintGraph {
    num_nodes: usize,
    successors: Vec<Vec<(u32, FlowEdge)>>,
}

impl LabeledTaintGraph {
    pub fn new(num_nodes: usize, edges: Vec<(u32, u32, FlowEdge)>) -> Self {
        let mut successors = vec![Vec::new(); num_nodes];
        for (src, dst, label) in edges {
            successors[src as usize].push((dst, label));
        }
        Self {
            num_nodes,
            successors,
        }
    }
}

impl DirectedGraph for LabeledTaintGraph {
    type Node = u32;

    fn num_nodes(&self) -> usize {
        self.num_nodes
    }
}

impl LabeledSuccessors for LabeledTaintGraph {
    type Label = FlowEdge;

    fn labeled_successors(&self, node: Self::Node) -> impl Iterator<Item = (Self::Node, FlowEdge)> {
        self.successors[node as usize].iter().copied()
    }
}

/// A [`TaintState`] carried along a search path to enforce call/return matching.
///
/// The search starts `Free` at a source. Along an edge it evolves per the query
/// engine's realizability rules: an `Intra` step preserves the state; a `Call`
/// step (descending into a callee) enters `Restricted`; and a `Return` step
/// (leaving a callee) is only traversable while `Free` — a `Restricted` return
/// would leave the call it descended through unmatched, so that edge is pruned —
/// and keeps the state `Free`. This one-bit discipline admits exactly the paths
/// that ascend to callers before descending into callees, i.e. the realizable
/// (call/return-balanced) ones.
impl Annotation<LabeledTaintGraph> for TaintState {
    fn start() -> Self {
        TaintState::Free
    }

    fn expand(
        &self,
        _graph: &LabeledTaintGraph,
        _from: u32,
        label: &FlowEdge,
        _to: u32,
    ) -> Option<Self> {
        match label {
            FlowEdge::Intra => Some(*self),
            FlowEdge::Call(_) => Some(TaintState::Restricted),
            FlowEdge::Return(_) => match self {
                TaintState::Free => Some(TaintState::Free),
                TaintState::Restricted => None,
            },
        }
    }
}

/// A node in the taint dataflow graph: a function-local vertex (a variable
/// together with its access path). Unlike before, the node carries no taint
/// state — call/return matching is enforced by the [`TaintState`] annotation the
/// realizable-path search threads along the [`FlowEdge`] labels, not by splitting
/// a vertex into per-state copies.
pub type FlowNode = (FunctionId, FlowVariable, Path);

/// The taint dataflow graph the formatter walks to emit path (code-flow)
/// results, together with the maps needed to translate between interned node
/// ids and the `(function, variable, path)` vertices they stand for.
pub struct TaintFlowGraph {
    /// Labeled reachability graph over interned nodes, oriented source -> derived,
    /// each edge tagged with its [`FlowEdge`].
    pub graph: LabeledTaintGraph,
    /// Vertex -> node id. Because nodes are bare vertices, this is exactly how a
    /// source/sink endpoint vertex resolves to its (single) graph node.
    pub node_to_id: BTreeMap<FlowNode, u32>,
    /// Node id -> vertex (the id indexes this vector).
    pub id_to_node: Vec<FlowNode>,
    /// Call instruction anchoring each `(src_id, dst_id)` edge, when the edge is
    /// a call/return propagation. Assign/alias edges contribute nothing.
    pub site_by_edge: BTreeMap<(u32, u32), InsnSiteId>,
    /// [`FlowEdge`] label for every `(src_id, dst_id)` edge, so a code-flow step
    /// can report whether taint crossed a call, returned, or stayed intra-procedural.
    pub edge_by_edge: BTreeMap<(u32, u32), FlowEdge>,
}

/// Builds the taint dataflow graph from the format facts and the computed taint
/// results. This is the profile-agnostic core that both the SARIF path formatter
/// and the flowy human-profile path check build on; it performs no source-info
/// I/O.
pub fn build_taint_flow_graph(
    facts: &FormatFacts,
    taint_results: &TaintAnalysisResults,
) -> TaintFlowGraph {
    use std::collections::btree_map::Entry;

    let mut node_to_id: BTreeMap<FlowNode, u32> = BTreeMap::new();
    let mut id_to_node: Vec<FlowNode> = Vec::new();
    let mut site_by_edge: BTreeMap<(u32, u32), InsnSiteId> = BTreeMap::new();
    let mut edge_by_edge: BTreeMap<(u32, u32), FlowEdge> = BTreeMap::new();

    let intern =
        |n: FlowNode, node_to_id: &mut BTreeMap<FlowNode, u32>, id_to_node: &mut Vec<FlowNode>| {
            if let Entry::Vacant(e) = node_to_id.entry(n) {
                e.insert(id_to_node.len() as u32);
                id_to_node.push(n);
            }
        };

    let taint_edge = &taint_results.edges;
    // Collect all nodes into node_to_id first: every tainted vertex and the
    // endpoint that tainted it (so an endpoint always resolves to a node even if
    // it has no incident edge), then both ends of every propagation edge. Nodes
    // are bare vertices; the taint state is not part of node identity.
    for (f, _ts, v, p, src) in &facts.taint {
        intern((*f, *v, *p), &mut node_to_id, &mut id_to_node);
        intern(
            (src.infunc, src.vertex.0, src.vertex.1),
            &mut node_to_id,
            &mut id_to_node,
        );
    }
    for (_edge, sf, sv, sp, df, dv, dp) in taint_edge {
        intern((*sf, *sv, *sp), &mut node_to_id, &mut id_to_node);
        intern((*df, *dv, *dp), &mut node_to_id, &mut id_to_node);
    }

    // The persisted edges are already in execution / data-flow order
    // (source -> derived), so every edge is walked as-is. Realizability is
    // enforced during the search by the taint-state annotation evolving along the
    // edge labels, not by pre-filtering edges here.
    let mut edges: Vec<(u32, u32, FlowEdge)> = Vec::with_capacity(taint_edge.len());
    for (edge, sf, sv, sp, df, dv, dp) in taint_edge {
        let src_id = *node_to_id.get(&(*sf, *sv, *sp)).unwrap();
        let dst_id = *node_to_id.get(&(*df, *dv, *dp)).unwrap();
        edges.push((src_id, dst_id, *edge));
        edge_by_edge.insert((src_id, dst_id), *edge);
        // Anchor this edge to its call instruction so the code-flow step walking
        // src_id -> dst_id resolves to *this* call site rather than whatever site
        // happened to be recorded first for the variable. Only Call/Return edges
        // carry a site; Intra edges contribute nothing.
        if let Some(packed) = edge.site()
            && let Ok(s) = InsnSiteId::try_from(packed)
        {
            site_by_edge.insert((src_id, dst_id), s);
        }
    }

    TaintFlowGraph {
        graph: LabeledTaintGraph::new(id_to_node.len(), edges),
        node_to_id,
        id_to_node,
        site_by_edge,
        edge_by_edge,
    }
}

/// A source -> sink taint path discovered in the dataflow graph: the source and
/// sink endpoints it connects, and the interned graph node ids from source to
/// sink.
#[derive(Debug, Clone)]
pub struct EndpointPath {
    pub source: QueryEndpoint,
    pub sink: QueryEndpoint,
    /// Graph node ids walked from source to sink (see [`TaintFlowGraph`]).
    pub nodes: Vec<u32>,
}

/// Finds the source -> sink taint paths the human profile reports.
///
/// It pairs every forward-source endpoint with every backward-sink endpoint that
/// taint actually reached, and keeps the pairs connected in the dataflow graph.
/// This is the path-existence core behind the human-profile SARIF `tainted-path`
/// results, exposed so the flowy checker can assert a path exists for each
/// declared source/sink pair.
pub fn find_endpoint_paths(
    facts: &FormatFacts,
    taint_results: &TaintAnalysisResults,
) -> Vec<EndpointPath> {
    let fg = build_taint_flow_graph(facts, taint_results);

    // The source/sink endpoints actually present on tainted nodes -- the same
    // endpoint set the SARIF formatter draws on. Forward endpoints are sources,
    // backward endpoints are sinks.
    let mut sources: BTreeSet<&QueryEndpoint> = BTreeSet::new();
    let mut sinks: BTreeSet<&QueryEndpoint> = BTreeSet::new();
    for (_, _, _, _, ep) in &facts.taint {
        match ep.direction {
            TaintDirection::Forward => {
                sources.insert(ep);
            }
            TaintDirection::Backward => {
                sinks.insert(ep);
            }
        }
    }

    let mut paths = Vec::new();
    for sink in &sinks {
        let end_vertex = (sink.infunc, sink.vertex.0, sink.vertex.1);
        // Nodes are bare vertices, so a sink endpoint resolves to a single node.
        let Some(&target_id) = fg.node_to_id.get(&end_vertex) else {
            continue;
        };
        for src in &sources {
            let start_vertex = (src.infunc, src.vertex.0, src.vertex.1);
            let Some(&start_id) = fg.node_to_id.get(&start_vertex) else {
                continue;
            };
            // Endpoints are anchored at their call sites: a sink on a callee's
            // formal becomes one endpoint per caller call site, each on the
            // distinct call-arg vertex that call passes. Two flows that differ
            // only in their call site are therefore distinct (source, sink) pairs
            // already, so a single search per pair suffices. The search carries a
            // `TaintState` annotation that evolves along the edge labels, so only
            // realizable (call/return-balanced) walks reach the target.
            if let Some(path) =
                find_annotated_path_to_set(&fg.graph, start_id, |n, _s: &TaintState| n == target_id)
            {
                paths.push(EndpointPath {
                    source: (*src).clone(),
                    sink: (*sink).clone(),
                    nodes: path.into_iter().map(|(n, _s)| n).collect(),
                });
            }
        }
    }
    paths
}

/// Path string for DataFusion / object_store local parquet reads.
///
/// Project paths on Windows are often canonicalized to verbatim `\\?\` form. Naive
/// backslash-to-slash rewriting breaks absolute-path detection, so DataFusion treats
/// the path as relative to the process cwd (e.g. a scratch dir containing
/// `ArrayFlow.class`) and object_store fails URL conversion. Returns an absolute path.
fn object_store_path(path: &path::Path) -> String {
    let absolutized = path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = absolutized.as_path();
    let parsed = url::Url::from_file_path(path);
    #[cfg(windows)]
    let parsed = parsed.or_else(|_| {
        let s = path.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            url::Url::from_file_path(stripped)
        } else {
            Err(())
        }
    });
    parsed.map(|url| url.to_string()).unwrap_or_else(|_| {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let normalized = normalized.strip_prefix("//?/").unwrap_or(&normalized);
        format!("file:///{normalized}")
    })
}

/// Register a local parquet file and fail loudly if it is unreadable.
///
/// DataFusion's `register_parquet` happily accepts a non-existent path and infers
/// an empty (zero-column) schema instead of erroring. A downstream query against
/// that table then fails with a misleading "No field named ..." schema error far
/// from the real cause. Catch it here by checking the inferred schema and surface
/// a clear message naming the offending path.
async fn register_parquet_checked(
    ctx: &SessionContext,
    table_name: &str,
    path: String,
) -> Result<(), Error> {
    ctx.register_parquet(table_name, &path, ParquetReadOptions::default())
        .await
        .err_context(|| format!("reading {table_name} from {path}"))?;
    let table = ctx
        .table(table_name)
        .await
        .err_context(|| format!("reading {table_name} from {path}"))?;
    if table.schema().fields().is_empty() {
        return Err(Error::Path {
            message: format!(
                "cannot read parquet for table `{table_name}`: no columns found at {path} \
                 (the file is missing or unreadable)"
            ),
        });
    }
    Ok(())
}

/// Formats the query results as SARIF and writes them to `output`.
///
/// Returns `invocation.executionSuccessful`: false when an `error`-level notification was
/// emitted, which per §3.58.6 the caller turns into a non-zero exit code — *after* the file
/// has been written, since the file is what explains the failure.
pub fn format_sarif(
    project: &AnalysisProject,
    facts: &FormatFacts,
    taint_results: &TaintAnalysisResults,
    compact: bool,
    output: &path::Path,
    profile: SarifProfile,
    diagnostics: &QueryDiagnostics,
) -> Result<bool, Error> {
    log::trace!("format_sarif entry");
    let rt = tokio::runtime::Runtime::new()?;
    let config = FormatConfig { compact, profile };
    let (final_sarif, execution_successful) = rt.block_on(async {
        async_format_sarif(project, taint_results, facts, &config, diagnostics).await
    })?;

    let writer: Box<dyn std::io::Write> = if output.to_str() == Some("-") {
        Box::new(std::io::stdout())
    } else {
        Box::new(File::create(output).err_context(|| "creating sarif output file")?)
    };

    if compact {
        serde_json::to_writer(writer, &final_sarif).err_context(|| "writing sarif")?;
    } else {
        serde_json::to_writer_pretty(writer, &final_sarif).err_context(|| "writing sarif")?;
    }
    Ok(execution_successful)
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
    diagnostics: &QueryDiagnostics,
) -> Result<(serde_json::Value, bool), Error> {
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
            .push((label.clone(), *var, *pth));
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
                    *var,
                    *pth,
                ));
            }
        }
    }
    let mut results = Vec::new();
    let mut path_stats = PathStats::default();
    let mut sarif_data = SarifData::default();
    let index_dir = project.index_path()?;
    // projects should have only one set of parquet files, so just take the last one
    let mut parquet_dir = String::from("");
    for import in project.iter_imports() {
        let import = import?;
        let dir = import.source_info_dir();
        parquet_dir = object_store_path(&dir);
        let ctx = ProjectContext {
            source_spans: &source_spans,
            index_dir: index_dir.clone(),
            source_info_dir: dir,
            details_by_span: &details_by_span,
            facts,
            taint_results,
            language: import.language,
            image_base: import.image_base,
        };
        let (sarif_results, import_path_stats) =
            format_source_info_results(&ctx, config, &mut sarif_data)
                .await
                .err_context(|| "formatting results")?;
        results.extend(sarif_results);
        path_stats.merge(import_path_stats);
    }

    // What happened to `C0001` this run. Decided here rather than per import: the profile
    // gate and the endpoint counts are run-wide, and exactly one status result may be
    // emitted no matter how many imports the project has.
    let path_outcome = if !profile_finds_paths(config.profile) {
        PathOutcome::Disabled
    } else if let Some(reason) = empty_end_reason(diagnostics) {
        PathOutcome::NotApplicable(reason)
    } else if path_stats.reported == 0 {
        PathOutcome::NoneFound
    } else {
        PathOutcome::Found(path_stats.reported)
    };
    results.extend(path_status_result(&path_outcome));

    let invocation = build_invocation(diagnostics, config, &path_outcome, path_stats);
    let execution_successful = invocation.execution_successful;

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
                    ReportingDescriptor::builder()
                        .id(ABSORBING_FUNCTION_RULE_ID)
                        .name(ABSORBING_FUNCTION_RULE_NAME)
                        .short_description(
                            MultiformatMessageString::builder()
                                .text(ABSORBING_FUNCTION_RULE_DESCRIPTION)
                                .build(),
                        )
                        .message_strings(BTreeMap::from([(
                            "default".to_string(),
                            MultiformatMessageString::builder()
                                .text("This external function receives tainted data.")
                                .build(),
                        )]))
                        .build(),
                ])
                .notifications(notification_descriptors())
                .build(),
        )
        .build();

    let properties = PropertyBag::builder()
        .additional_properties(BTreeMap::from([(
            "parquet_dir".to_string(),
            serde_json::json!(parquet_dir),
        )]))
        .build();

    // `results` is always set, never omitted: per the JSON schema it "must be present (but
    // may be empty) if a log file represents an actual scan".
    let run = if sarif_data.global_logical_locations.is_empty() {
        Run::builder()
            .tool(tool)
            .invocations(vec![invocation])
            .results(results)
            .build()
    } else {
        Run::builder()
            .tool(tool)
            .invocations(vec![invocation])
            .results(results)
            .logical_locations(sarif_data.global_logical_locations)
            .build()
    };
    // we need to deconstruct and rebuild the run to ensure a certain order (needs serde_json feature preserve_order)
    let final_run = match serde_json::to_value(&run).unwrap() {
        serde_json::Value::Object(mut old_map) => {
            let mut new_map = serde_json::Map::new();
            new_map.insert("tool".to_string(), old_map.remove("tool").unwrap());
            // `invocations` right after `tool`: it is the run's status, and pinning it here
            // keeps diffs between runs stable.
            new_map.insert(
                "invocations".to_string(),
                old_map.remove("invocations").unwrap(),
            );
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
    Ok((final_sarif, execution_successful))
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

    register_parquet_checked(
        &ctx_session,
        "file_spans",
        object_store_path(&dir.join("file_spans.parquet")),
    )
    .await?;
    register_parquet_checked(
        &ctx_session,
        "spans",
        object_store_path(&dir.join("spans.parquet")),
    )
    .await?;
    register_parquet_checked(
        &ctx_session,
        "files",
        object_store_path(&dir.join("files.parquet")),
    )
    .await?;
    register_parquet_checked(
        &ctx_session,
        "artifacts",
        object_store_path(&dir.join("artifacts.parquet")),
    )
    .await?;
    register_parquet_checked(
        &ctx_session,
        "function_id",
        object_store_path(&index_dir.join("function_id.parquet")),
    )
    .await?;

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
                    // `start` is the absolute instruction address (it includes
                    // the disassembler's image base). Emit the section-relative
                    // offset too so consumers (e.g. addr2line in the regression
                    // suite) need not assume a particular base. When the base is
                    // unknown, the relative offset degenerates to the absolute.
                    let address = Address::builder()
                        .absolute_address(start as i64)
                        .relative_address(start as i64 - ctx.image_base.unwrap_or(0))
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
) -> Result<(Vec<SarifResult>, PathStats), Error> {
    // Prepare graph for path finding when the selected profile emits path traces.
    let mut id_to_node: Vec<FlowNode> = Vec::new();
    // The call instruction anchoring each graph edge, keyed by `(src_id, dst_id)`
    // node-id pair (same orientation as the graph edges, i.e. origin -> derived).
    // Only call/return edges have a site; assign/alias edges contribute nothing.
    let mut site_by_edge: BTreeMap<(u32, u32), InsnSiteId> = BTreeMap::new();
    // FlowEdge label per edge, so a code-flow step can name it as a call/return.
    let mut edge_by_edge: BTreeMap<(u32, u32), FlowEdge> = BTreeMap::new();
    // Endpoint vertex -> its (single) graph node id.
    let mut node_to_id: BTreeMap<FlowNode, u32> = BTreeMap::new();

    let graph = if matches!(
        config.profile,
        SarifProfile::Human | SarifProfile::Debug | SarifProfile::Agent
    ) {
        // Same graph the human-profile path check uses; see `build_taint_flow_graph`.
        let fg = build_taint_flow_graph(ctx.facts, ctx.taint_results);
        id_to_node = fg.id_to_node;
        site_by_edge = fg.site_by_edge;
        edge_by_edge = fg.edge_by_edge;
        node_to_id = fg.node_to_id;
        Some(fg.graph)
    } else {
        None
    };

    // Call site -> callee, used to name a call-site-anchored source/sink by the
    // framework method it models rather than the caller `infunc` now holds.
    let call_callee: BTreeMap<PackedInsnSiteId, FunctionId> =
        ctx.facts.call.iter().copied().collect();

    // Map each node to its endpoints
    let mut node_to_endpoint: BTreeMap<(FunctionId, FlowVariable, Path), Vec<QueryEndpoint>> =
        BTreeMap::new();
    for (f, _, v, p, src) in &ctx.facts.taint {
        node_to_endpoint
            .entry((*f, *v, *p))
            .or_default()
            .push(src.clone());
    }
    // All unique endpoints
    let endpoints: BTreeSet<_> = node_to_endpoint.values().flat_map(|v| v.iter()).collect();

    // Map each node to an instruction for location info
    let mut site_by_var: BTreeMap<(FunctionId, FlowVariable), (FunctionId, InsnId)> =
        BTreeMap::new();
    for (site, _, v, _) in &ctx.facts.actual_param {
        let site_unpacked = InsnSiteId::unpack(site).unwrap();
        site_by_var
            .entry((site_unpacked.func_id, *v))
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
        // Each distinct (source vertex, sink vertex) pair is searched once.
        let mut tested_pairs: BTreeSet<(
            (FunctionId, FlowVariable, Path),
            (FunctionId, FlowVariable, Path),
        )> = BTreeSet::new();
        for (fs_id, details) in ctx.details_by_span {
            if !has_sinks {
                break;
            }
            for (lbl, func_id, var, pth) in details {
                let node = (*func_id, *var, *pth);
                let Some(sources) = node_to_endpoint.get(&node) else {
                    continue;
                };
                let (fwd_sources, bwd_sinks): (Vec<_>, Vec<_>) = sources
                    .iter()
                    .partition(|s| s.direction == crate::facts::TaintDirection::Forward);

                for sink in &bwd_sinks {
                    let end_vertex = (sink.infunc, sink.vertex.0, sink.vertex.1);
                    // Nodes are bare vertices, so a sink resolves to one node.
                    let Some(&target_id) = node_to_id.get(&end_vertex) else {
                        continue;
                    };
                    for src in &fwd_sources {
                        let start_vertex = (src.infunc, src.vertex.0, src.vertex.1);
                        if !tested_pairs.insert((start_vertex, end_vertex)) {
                            continue;
                        }
                        let Some(&start_id) = node_to_id.get(&start_vertex) else {
                            continue;
                        };
                        // A real source -> sink flow is a realizable walk from the
                        // source seed to the node the sink names (its call-arg).
                        // The `TaintState` annotation threaded along the edge
                        // labels prunes unrealizable (call/return-mismatched)
                        // walks, and pinning the target to this sink's call-arg
                        // keeps a call into the sink's function on an unrelated
                        // argument from being mistaken for this flow.
                        if let Some(path) =
                            find_annotated_path_to_set(g, start_id, |n, _s: &TaintState| {
                                n == target_id
                            })
                        {
                            let nodes: Vec<u32> = path.into_iter().map(|(n, _s)| n).collect();
                            results_by_path
                                .entry(nodes)
                                .or_insert((*fs_id, Vec::new()))
                                .1
                                .push(((*src).clone(), Some((*sink).clone()), lbl.clone()));
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

    // The call instruction a *call-arg* vertex belongs to. A call-arg vertex is an
    // actual argument at a call and encodes that call's instruction directly (its
    // `insn_id`), which lives in the vertex's own function (the caller). This is how
    // a summarized callee -- linked by an intra summary edge between its actual-arg
    // vertices rather than a Call/Return edge -- is anchored back to its call line.
    let call_arg_site = |node: &FlowNode| -> Option<InsnSiteId> {
        let packed = node.1.as_call_arg()?;
        let CallArgId { insn_id, .. } = CallArgId::try_from(packed).ok()?;
        Some(InsnSiteId::new(node.0, insn_id))
    };

    let mut path_sites = BTreeSet::new();
    for (path, (_fs, details)) in &results_by_path {
        for window in path.windows(2) {
            if let Some(site) = site_by_edge.get(&(window[0], window[1]))
                && seen_sites.insert((site.func_id.id, site.insn_id.id))
            {
                path_sites.insert((site.func_id, site.insn_id));
            }
        }
        // An interprocedural transfer whose callee was summarized (not descended
        // into) shows up as an *intra* summary edge between the call's actual-arg
        // vertices -- e.g. `transfer(&x.b, y)` links `call-arg(3, 1)` (the tainted
        // input) to `call-arg(3, 0).d` (the tainted output) with no Call/Return edge
        // to anchor. The call instruction is therefore on no path *edge*, but each
        // such call-arg *node* encodes it (see `call_arg_site`). Load those sites so
        // the summarized call line is reported rather than silently elided.
        for &n in path {
            let node = &id_to_node[n as usize];
            if let Some(site) = call_arg_site(node)
                && seen_sites.insert((site.func_id.id, site.insn_id.id))
            {
                path_sites.insert((site.func_id, site.insn_id));
            }
        }
        // The endpoints are anchored at their call sites, and a source -> sink path
        // walks the call-arg vertices via assign edges that carry no instruction. The
        // source/sink call instructions are therefore not on any path edge; pull them
        // from the endpoints so they are still loaded and reported as located sites.
        for (src, sink, _lbl) in details {
            for ep in std::iter::once(src).chain(sink.as_ref()) {
                if let Some(site) = ep.call_site.and_then(|p| InsnSiteId::try_from(&p).ok())
                    && seen_sites.insert((site.func_id.id, site.insn_id.id))
                {
                    path_sites.insert((site.func_id, site.insn_id));
                }
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
                        Label(*crate::facts::EMPTY_STR),
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
    for (path, (file_span_id, details)) in &results_by_path {
        let mut thread_flow_locations = Vec::new();
        let mut last_loc_id: Option<(String, Option<String>)> = None;
        // Monotonic step counter for the whole flow, surfaced as SARIF `executionOrder`
        // so a viewer/`jq` can order steps unambiguously across the (possibly several)
        // code flows a result carries.
        let mut exec_order: i64 = 0;
        // Resolve a function id to its (possibly obfuscated) fully-qualified name so a
        // step reads as `... in LX/09h;->A02(...)` rather than a bare vertex token.
        let fname = |fid: FunctionId| -> String {
            source_data
                .id_to_name
                .get(&fid.id)
                .cloned()
                .unwrap_or_else(|| format!("func#{}", fid.id))
        };
        // Emit a located code-flow step for a call instruction, deduping against the
        // previous step's location. `message` describes the step (the full vertex —
        // variable *and* access path — its function, and the edge kind); `kinds` are the
        // SARIF well-known step categories (`call`/`return`/`taint`); `exec_order` is
        // bumped so every emitted step carries its temporal order.
        let push_site_step = |thread_flow_locations: &mut Vec<ThreadFlowLocation>,
                              last_loc_id: &mut Option<(String, Option<String>)>,
                              exec_order: &mut i64,
                              site: &InsnSiteId,
                              kinds: Vec<String>,
                              message: String| {
            let Some(loc) = source_data
                .all_locations
                .get(&(site.func_id.id, site.insn_id.id))
            else {
                return;
            };
            // Identity of a physical location for step deduping. Different artifact kinds
            // locate an instruction differently: native code by `address.absoluteAddress`,
            // source by `region.startLine:startColumn`, bytecode (e.g. a `.dex`) by
            // `region.byteOffset`. Build the key from *every* dimension that is present
            // rather than picking one — a first-wins fallback would collapse two steps that
            // agree on one dimension but differ on another (and an all-`None` key collapses
            // every step in a file to `(uri, None)`, which once flattened whole `.dex` flows
            // down to a lone source step).
            let current_loc_id = loc.physical_location.as_ref().and_then(|p| {
                let uri = p.artifact_location.as_ref()?.uri.as_ref()?.clone();
                let mut parts: Vec<String> = Vec::new();
                if let Some(a) = p.address.as_ref().and_then(|a| a.absolute_address.as_ref()) {
                    parts.push(format!("addr:{a}"));
                }
                if let Some(r) = p.region.as_ref() {
                    if let (Some(l), Some(c)) = (r.start_line, r.start_column) {
                        parts.push(format!("line:{l}:{c}"));
                    }
                    if let Some(b) = r.byte_offset {
                        parts.push(format!("byte:{b}"));
                    }
                }
                let pos = if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("|"))
                };
                Some((uri, pos))
            });
            if current_loc_id.is_some() && current_loc_id == *last_loc_id {
                return;
            }
            *last_loc_id = current_loc_id;
            *exec_order += 1;
            let mut loc_with_msg = loc.clone();
            loc_with_msg.message = Some(Message::builder().text(message).build());
            thread_flow_locations.push(
                ThreadFlowLocation::builder()
                    .location(loc_with_msg)
                    .execution_order(*exec_order)
                    .kinds(kinds)
                    .build(),
            );
        };

        // Lead with the source endpoints' call sites: because the endpoints are
        // anchored at call sites, the source/sink call instructions are not on any
        // path edge (the path walks call-arg vertices via assign edges), so they
        // would otherwise be absent from the code flow.
        for (src, _sink, _lbl) in details {
            if let Some(site) = src.call_site.and_then(|p| InsnSiteId::try_from(&p).ok()) {
                let callee = endpoint_callee(src, &call_callee);
                push_site_step(
                    &mut thread_flow_locations,
                    &mut last_loc_id,
                    &mut exec_order,
                    &site,
                    vec!["taint".to_string()],
                    format!(
                        "source {}{} in {}",
                        src.vertex.0,
                        src.vertex.1.to_dot_string(),
                        fname(callee)
                    ),
                );
            }
        }
        // The source and sink endpoints are reported by the leading/trailing steps
        // above and below; interior call-arg steps must not collide with them.
        // `push_site_step` dedups by *location*, so an interior step must be skipped
        // whenever it lands on a source/sink endpoint's node OR its call *instruction*
        // -- otherwise, emitted first, its `call ...` message pre-empts the endpoint's
        // `source ...`/`sink ...` step (the code-flow integrity check keys on those).
        // The instruction guard matters because a call passes several actual-arg
        // vertices at one site: the flow can visit a *non-endpoint* formal of the very
        // call the sink is anchored on (e.g. `call-arg(43, -2)` when the sink is
        // `call-arg(43, 0)`), which shares the sink's location and would otherwise
        // swallow its step.
        let endpoint_node_ids: BTreeSet<u32> = details
            .iter()
            .flat_map(|(src, sink, _)| std::iter::once(src).chain(sink.as_ref()))
            .filter_map(|ep| {
                node_to_id
                    .get(&(ep.infunc, ep.vertex.0, ep.vertex.1))
                    .copied()
            })
            .collect();
        let endpoint_sites: BTreeSet<(FunctionId, InsnId)> = details
            .iter()
            .flat_map(|(src, sink, _)| std::iter::once(src).chain(sink.as_ref()))
            .filter_map(|ep| ep.call_site.and_then(|p| InsnSiteId::try_from(&p).ok()))
            .map(|s| (s.func_id, s.insn_id))
            .collect();

        // Walk the path edge-by-edge: each consecutive `(src_id, dst_id)` pair is
        // a graph edge, and `site_by_edge` gives the call instruction that edge
        // flowed through. Attributing per edge keeps each call site distinct even
        // when the same variable participates in several calls.
        for window in path.windows(2) {
            let (src_id, dst_id) = (window[0], window[1]);
            if let Some(site) = site_by_edge.get(&(src_id, dst_id)) {
                let dst_node = &id_to_node[dst_id as usize];
                let kind = match edge_by_edge.get(&(src_id, dst_id)) {
                    Some(FlowEdge::Call(_)) => "call",
                    Some(FlowEdge::Return(_)) => "return",
                    _ => "taint",
                };
                push_site_step(
                    &mut thread_flow_locations,
                    &mut last_loc_id,
                    &mut exec_order,
                    site,
                    vec![kind.to_string()],
                    format!(
                        "{} {}{} in {}",
                        kind,
                        dst_node.1,
                        dst_node.2.to_dot_string(),
                        fname(dst_node.0)
                    ),
                );
            }
            // A summarized callee contributes no Call/Return edge, so its call line
            // would be skipped by the `site_by_edge` walk above. Surface it from the
            // destination *node* instead: an interior *call-arg* vertex is an actual
            // argument at a call and encodes that call's instruction, so emitting a
            // step there reports the summarized call (e.g. `transfer(&x.b, y)`) that
            // the taint flowed through. Deduping by location collapses the call's
            // several actual-arg vertices to one step.
            //
            // Restricting to call-arg vertices is what keeps this precise: the locals
            // and formals a summary threads through include ones that merely feed the
            // final sink, and surfacing those would both pre-empt that sink's
            // `sink ...` step and, in a case whose point is a *clean* sibling sink,
            // wrongly report its line.
            let dst_node = &id_to_node[dst_id as usize];
            if !endpoint_node_ids.contains(&dst_id)
                && let Some(site) = call_arg_site(dst_node)
                && !endpoint_sites.contains(&(site.func_id, site.insn_id))
            {
                let callee = InsnSiteId::new(site.func_id, site.insn_id)
                    .try_into()
                    .ok()
                    .and_then(|packed| call_callee.get(&packed).copied())
                    .unwrap_or(dst_node.0);
                push_site_step(
                    &mut thread_flow_locations,
                    &mut last_loc_id,
                    &mut exec_order,
                    &site,
                    vec!["call".to_string()],
                    format!(
                        "call {}{} in {}",
                        dst_node.1,
                        dst_node.2.to_dot_string(),
                        fname(callee)
                    ),
                );
            }
        }
        // Close with the sink endpoints' call sites, for the same reason.
        for (_src, sink, _lbl) in details {
            if let Some(s) = sink.as_ref()
                && let Some(site) = s.call_site.and_then(|p| InsnSiteId::try_from(&p).ok())
            {
                let callee = endpoint_callee(s, &call_callee);
                push_site_step(
                    &mut thread_flow_locations,
                    &mut last_loc_id,
                    &mut exec_order,
                    &site,
                    vec!["taint".to_string()],
                    format!(
                        "sink {}{} in {}",
                        s.vertex.0,
                        s.vertex.1.to_dot_string(),
                        fname(callee)
                    ),
                );
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

    // Which sources and sinks actually matched is part of what the run *did*, so it is
    // reported in every profile rather than only where the extra detail was wanted. These
    // stay `kind: "informational"` / `level: "none"`: they are context, not findings.
    results.extend(format_source_sink_results(
        sarif_data,
        &endpoints,
        &source_data.id_to_name,
        &call_callee,
        &site_by_var,
        &source_data.all_locations,
    ));

    if config.profile == SarifProfile::Agent {
        results.extend(format_absorbing_function_results(
            sarif_data,
            &ctx.taint_results.absorbing_functions,
            &source_data.id_to_name,
        ));
    }

    // Now build results for paths (for Human, Debug, or Agent profiles, one per path)
    let mut path_stats = PathStats::default();
    if profile_finds_paths(config.profile) {
        for (_path, (file_span_id, details)) in results_by_path {
            let location = if let Some(loc) = span_to_location.get(&file_span_id) {
                loc.clone()
            } else {
                // A path was found but has nowhere to be reported. Counted so the run can
                // say so via `CTADL0103` instead of dropping it in silence.
                path_stats.dropped_no_location += 1;
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

            // Resolve the source/sink callee name(s) so consumers can match on
            // the function directly instead of reconstructing it from the taint
            // statement vertex. The model attaches an endpoint to the callee
            // method (e.g. `nvram_get` / `system`); after call-site anchoring the
            // endpoint's `infunc` is the *caller*, so the modeled method is the
            // callee at its `call_site` -- see `endpoint_callee`. `taintLabels`
            // carries the source *kind*; this adds the source/sink *function names*.
            let mut source_functions: BTreeSet<String> = BTreeSet::new();
            let mut sink_functions: BTreeSet<String> = BTreeSet::new();
            for (src, sink_opt, _lbl) in &details {
                if let Some(name) = source_data
                    .id_to_name
                    .get(&endpoint_callee(src, &call_callee).id)
                {
                    source_functions.insert(name.clone());
                }
                if let Some(sink) = sink_opt
                    && let Some(name) = source_data
                        .id_to_name
                        .get(&endpoint_callee(sink, &call_callee).id)
                {
                    sink_functions.insert(name.clone());
                }
            }
            let source_functions: Vec<String> = source_functions.into_iter().collect();
            let sink_functions: Vec<String> = sink_functions.into_iter().collect();

            let mut additional_properties = BTreeMap::from([
                ("taintLabels".to_string(), serde_json::json!(sorted_labels)),
                (
                    "taintVertices".to_string(),
                    serde_json::json!(labels_to_vertices),
                ),
            ]);
            if let Some(first) = source_functions.first() {
                additional_properties.insert("sourceCallee".to_string(), serde_json::json!(first));
                additional_properties.insert(
                    "sourceFunctions".to_string(),
                    serde_json::json!(source_functions),
                );
            }
            if let Some(first) = sink_functions.first() {
                additional_properties.insert("sinkCallee".to_string(), serde_json::json!(first));
                additional_properties.insert(
                    "sinkFunctions".to_string(),
                    serde_json::json!(sink_functions),
                );
            }
            let properties = PropertyBag::builder()
                .additional_properties(additional_properties)
                .build();

            if let Some(code_flows) = code_flows_by_span.get(&file_span_id) {
                let result = SarifResult::builder()
                    .rule_id(TAINTED_PATH_RULE_ID.to_string())
                    // A reported taint path is a finding, not a note: `kind: "fail"` puts
                    // it on the same axis as the `open`/`notApplicable` states the rule
                    // reports when it finds nothing (§3.27.9). `informational` there and
                    // `open` here would be incoherent.
                    .kind(ResultKind::Fail)
                    .level(ResultLevel::Warning)
                    .message(Message::builder().text(final_msg_text).build())
                    .locations(vec![location])
                    .properties(properties)
                    .code_flows(code_flows.clone())
                    .build();

                results.push(result);
                path_stats.reported += 1;
            } else {
                // Same silent drop as above, one step later: located, but no code flow
                // survived to describe it.
                path_stats.dropped_no_location += 1;
            }
        }
    }

    Ok((results, path_stats))
}

/// The callee function an endpoint denotes for source/sink *naming*.
///
/// A source/sink is modeled on the framework method it calls (e.g.
/// `ContentResolver.query`), so before call-site anchoring an endpoint's `infunc`
/// was that callee and naming read straight off it. After anchoring (8fbc7ca),
/// `infunc` is the *caller* (the app method containing the call) and `vertex` is
/// the call-arg vertex; the modeled method is now the callee at `call_site`.
/// Recover it via the static call graph so reported source/sink callees stay the
/// framework method, not the caller. Function-anchored endpoints (no call site:
/// a local/global port or a callee with no callers) keep `infunc`.
fn endpoint_callee(
    ep: &QueryEndpoint,
    call_callee: &BTreeMap<PackedInsnSiteId, FunctionId>,
) -> FunctionId {
    ep.call_site
        .and_then(|site| call_callee.get(&site).copied())
        .unwrap_or(ep.infunc)
}

fn format_source_sink_results(
    sarif_data: &mut SarifData,
    endpoints: &BTreeSet<&QueryEndpoint>,
    id_to_name: &BTreeMap<u32, String>,
    call_callee: &BTreeMap<PackedInsnSiteId, FunctionId>,
    site_by_var: &BTreeMap<(FunctionId, FlowVariable), (FunctionId, InsnId)>,
    all_locations: &BTreeMap<(u32, u64), Location>,
) -> Vec<SarifResult> {
    let mut source_sink_results = Vec::new();

    // Collect all source and sink nodes with their endpoints
    for endpoint in endpoints {
        let is_source = endpoint.direction == crate::facts::TaintDirection::Forward;
        let is_sink = endpoint.direction == crate::facts::TaintDirection::Backward;

        let node = (endpoint.infunc, endpoint.vertex.0, endpoint.vertex.1);
        // Use the logical location of the source, and use the physical location additionally if it's available
        if is_source || is_sink {
            let rule_id = if is_source {
                TAINT_SOURCE_RULE_ID
            } else {
                TAINT_SINK_RULE_ID
            };
            // Render the full vertex (variable *and* access path) so distinct
            // endpoints on the same variable -- e.g. a model that taints both
            // `Argument(1).deref` and `Argument(1).deref.deref` -- read as the
            // separate sources they are, instead of collapsing to identical
            // "formal(1) in function main" lines. The label (taint kind) is the
            // other distinguishing field; carry it in `properties` below.
            let vertex = format!("{}{}", node.1, node.2.to_dot_string());
            // Name the source/sink by the framework method it models (the callee
            // at its anchored call site), not the caller `infunc` holds post-8fbc7ca.
            let callee_id = endpoint_callee(endpoint, call_callee);
            let func_name = id_to_name
                .get(&callee_id.id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let label = endpoint.label.0.to_string();
            let msg_text = if is_source {
                format!("Source of tainted data: {vertex} in function {func_name} (kind '{label}')")
            } else {
                format!("Sink of tainted data: {vertex} in function {func_name} (kind '{label}')")
            };

            let fully_qualified_name = id_to_name
                .get(&callee_id.id)
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

            if let Some(&site) = site_by_var.get(&(node.0, node.1))
                && let Some(physical_loc) = all_locations.get(&(site.0.id, site.1.id))
            {
                let mut loc_with_phys = physical_loc.clone();
                loc_with_phys.logical_locations = Some(vec![logical_location]);
                locations = vec![loc_with_phys];
            }

            let properties = PropertyBag::builder()
                .additional_properties(BTreeMap::from([
                    ("taintLabels".to_string(), serde_json::json!([label])),
                    ("taintVertex".to_string(), serde_json::json!(vertex)),
                ]))
                .build();

            let result = SarifResult::builder()
                .rule_id(rule_id.to_string())
                .kind(ResultKind::Informational)
                .level(ResultLevel::None)
                .message(Message::builder().text(msg_text).build())
                .locations(locations)
                .properties(properties)
                .build();

            source_sink_results.push(result);
        }
    }
    source_sink_results
}

fn format_absorbing_function_results(
    sarif_data: &mut SarifData,
    absorbing_functions: &[(FunctionId, QueryEndpoint, FormalIndex)],
    id_to_name: &BTreeMap<u32, String>,
) -> Vec<SarifResult> {
    let mut results = Vec::new();

    // Group by FunctionId, then map FormalIndex -> Set of Labels
    let mut grouped: BTreeMap<FunctionId, BTreeMap<FormalIndex, BTreeSet<String>>> =
        BTreeMap::new();

    for (fid, qe, formal) in absorbing_functions {
        grouped
            .entry(*fid)
            .or_default()
            .entry(*formal)
            .or_default()
            .insert(qe.label.0.to_string());
    }

    for (fid, formals_map) in grouped {
        let func_name = id_to_name
            .get(&fid.id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let msg_text = format!("Absorbing function: {} receives tainted data", func_name);

        // Convert the BTreeMap<FormalIndex, BTreeSet<String>> into a format for JSON serialization
        let mut tainted_formals_json: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (formal_idx, labels) in formals_map {
            let sorted_labels: Vec<String> = labels.into_iter().collect();
            tainted_formals_json.insert(formal_idx.to_string(), sorted_labels);
        }

        let properties = PropertyBag::builder()
            .additional_properties(BTreeMap::from([(
                "taintedFormals".to_string(),
                serde_json::json!(tainted_formals_json),
            )]))
            .build();

        // Logical location for the absorbing function
        let loc_idx = *sarif_data
            .global_logical_locations_map
            .entry(func_name.clone())
            .or_insert_with(|| {
                let idx = sarif_data.global_logical_locations.len();
                sarif_data.global_logical_locations.push(
                    LogicalLocation::builder()
                        .kind("function")
                        .name(func_name.clone())
                        .fully_qualified_name(func_name)
                        .build(),
                );
                idx
            });

        let logical_location = LogicalLocation::builder().index(loc_idx as i64).build();
        let location = Location::builder()
            .logical_locations(vec![logical_location])
            .build();

        let result = SarifResult::builder()
            .rule_id(ABSORBING_FUNCTION_RULE_ID.to_string())
            .kind(ResultKind::Informational)
            .level(ResultLevel::None)
            .message(Message::builder().text(msg_text).build())
            .locations(vec![location])
            .properties(properties)
            .build();
        results.push(result);
    }
    results
}

/// Look up the sites in the index source map and returns the span ids
pub async fn find_source_ids(
    source_map: &path::Path,
    tainted: &TaintedInstructions,
) -> Result<Vec<(FileSpanId, FunctionId, InsnId)>, Error> {
    let mut ctx = SessionContext::new();
    register_parquet_checked(&ctx, "index_source_map", object_store_path(source_map)).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FormalIndex;

    #[test]
    fn object_store_path_absolutizes_relative_input() {
        // A relative path must be resolved to an absolute `file://` URL, not left
        // as a bogus `file:///<relative>` rooted at the filesystem root. Compute
        // the expectation from the cwd the same way the function does.
        let rel = path::Path::new("mystore/imports/bt/file_spans.parquet");
        let expected = url::Url::from_file_path(path::absolute(rel).unwrap())
            .unwrap()
            .to_string();
        assert_eq!(object_store_path(rel), expected);
        assert!(object_store_path(rel).starts_with("file:///"));
        // The relative components survive absolutization.
        assert!(object_store_path(rel).ends_with("mystore/imports/bt/file_spans.parquet"));
    }

    #[test]
    #[cfg(not(windows))]
    fn object_store_path_preserves_absolute_input() {
        let abs = path::Path::new("/var/data/store/file_spans.parquet");
        assert_eq!(
            object_store_path(abs),
            "file:///var/data/store/file_spans.parquet"
        );
    }

    #[test]
    #[cfg(windows)]
    fn object_store_path_handles_windows_verbatim() {
        // Verbatim `\\?\` paths are absolute; the `\\?\` prefix is stripped so
        // object_store gets a clean drive-letter URL.
        let verbatim = path::Path::new(r"\\?\C:\proj\store\file_spans.parquet");
        assert_eq!(
            object_store_path(verbatim),
            "file:///C:/proj/store/file_spans.parquet"
        );
        // A plain absolute Windows path round-trips too.
        let plain = path::Path::new(r"C:\proj\store\file_spans.parquet");
        assert_eq!(
            object_store_path(plain),
            "file:///C:/proj/store/file_spans.parquet"
        );
    }

    fn formal(i: i16) -> FlowVariable {
        FlowVariable::formal_index(FormalIndex::new(i))
    }

    /// An endpoint on `formal(0)` of `func`.
    fn endpoint(func: u32, label: &str, dir: TaintDirection) -> QueryEndpoint {
        endpoint_on(func, formal(0), label, dir)
    }

    /// An endpoint on an arbitrary vertex of `func`.
    fn endpoint_on(
        func: u32,
        var: FlowVariable,
        label: &str,
        dir: TaintDirection,
    ) -> QueryEndpoint {
        QueryEndpoint {
            infunc: FunctionId::new(func),
            vertex: FlowVertex(var, Path::empty()),
            label: Label(label.into()),
            direction: dir,
            call_site: None,
            saturating: false,
        }
    }

    /// A taint fact placing endpoint `ep` on the vertex `(func, var)`. The taint
    /// state is incidental now (the graph nodes are bare vertices), so it is
    /// always `Free`.
    fn taint_fact(
        func: u32,
        var: FlowVariable,
        ep: QueryEndpoint,
    ) -> (FunctionId, TaintState, FlowVariable, Path, QueryEndpoint) {
        (
            FunctionId::new(func),
            TaintState::Free,
            var,
            Path::empty(),
            ep,
        )
    }

    /// A graph vertex `(func, var)` (with the empty access path).
    fn node(func: u32, var: FlowVariable) -> FlowNode {
        (FunctionId::new(func), var, Path::empty())
    }

    /// An arbitrary call instruction to anchor `Call`/`Return` edges in tests.
    fn a_site() -> crate::facts::PackedInsnSiteId {
        crate::facts::PackedInsnSiteId::try_from_parts(FunctionId::new(9), InsnId::new(1)).unwrap()
    }

    fn taint_results(
        edges: Vec<(
            FlowEdge,
            FunctionId,
            FlowVariable,
            Path,
            FunctionId,
            FlowVariable,
            Path,
        )>,
    ) -> TaintAnalysisResults {
        TaintAnalysisResults {
            edges,
            tainted_insns: TaintedInstructions {
                tainted_insn: vec![],
            },
            absorbing_functions: vec![],
        }
    }

    /// A `label`-classified edge along which data flows `src -> dst`, in the same
    /// execution / data-flow order the query engine persists.
    #[allow(clippy::type_complexity)]
    fn edge(
        label: FlowEdge,
        src: FlowNode,
        dst: FlowNode,
    ) -> (
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    ) {
        (label, src.0, src.1, src.2, dst.0, dst.1, dst.2)
    }

    /// A source in function 1 connected by one propagation edge to a sink in
    /// function 2 yields exactly one source -> sink path, anchored at the right
    /// nodes.
    #[test]
    fn finds_path_for_connected_source_and_sink() {
        let source = endpoint(1, "X", TaintDirection::Forward);
        let sink = endpoint(2, "X", TaintDirection::Backward);
        let src_node = node(1, formal(0));
        let sink_node = node(2, formal(0));

        let facts = FormatFacts {
            taint: vec![
                // sink node is forward-tainted by the source endpoint
                taint_fact(2, formal(0), source.clone()),
                // source node is backward-tainted by the sink endpoint
                taint_fact(1, formal(0), sink.clone()),
            ],
            ..Default::default()
        };
        // One edge, oriented as data flows: source node -> sink node.
        let results = taint_results(vec![edge(FlowEdge::Intra, src_node, sink_node)]);

        let paths = find_endpoint_paths(&facts, &results);
        assert_eq!(paths.len(), 1, "expected exactly one source->sink path");
        assert_eq!(paths[0].source, source);
        assert_eq!(paths[0].sink, sink);

        let fg = build_taint_flow_graph(&facts, &results);
        assert_eq!(paths[0].nodes.first(), Some(&fg.node_to_id[&src_node]));
        assert_eq!(paths[0].nodes.last(), Some(&fg.node_to_id[&sink_node]));
    }

    /// With no propagation edge, the source cannot reach the sink, so the human
    /// profile reports no path.
    #[test]
    fn finds_no_path_when_disconnected() {
        let source = endpoint(1, "X", TaintDirection::Forward);
        let sink = endpoint(2, "X", TaintDirection::Backward);

        let facts = FormatFacts {
            taint: vec![
                taint_fact(2, formal(0), source),
                taint_fact(1, formal(0), sink),
            ],
            ..Default::default()
        };
        let results = taint_results(vec![]);

        let paths = find_endpoint_paths(&facts, &results);
        assert!(
            paths.is_empty(),
            "disconnected endpoints must not yield a path"
        );
    }

    /// Taint that enters a callee through a `Call` edge (moving the search's
    /// [`TaintState`] annotation to `Restricted`) must not then be spliced onto a
    /// `Return` edge: a `Restricted` return is exactly the unrealizable
    /// call/return mismatch the annotation prunes.
    ///
    /// Layout (forward analysis): source `S` in func 1 flows through a `Call`
    /// into callee formal `F` in func 2 (so the annotation becomes `Restricted`).
    /// From `F` a `Return` edge leads to `T` in func 1, where the sink lives.
    /// Because the annotation is `Restricted` at `F`, `TaintState::expand` prunes
    /// the `Return`, so no source -> sink path exists.
    #[test]
    fn taint_state_blocks_unrealizable_call_return() {
        let source = endpoint_on(1, formal(0), "X", TaintDirection::Forward);
        let sink = endpoint_on(1, formal(1), "X", TaintDirection::Backward);

        // s: source in caller; f: callee formal; t: returned-to vertex in caller.
        let s = node(1, formal(0));
        let f = node(2, formal(0));
        let t = node(1, formal(1));

        let facts = FormatFacts {
            taint: vec![
                taint_fact(1, formal(0), source.clone()),
                taint_fact(1, formal(1), sink.clone()),
            ],
            ..Default::default()
        };
        let results = taint_results(vec![
            // call: S flows into the callee formal (annotation -> Restricted).
            edge(FlowEdge::Call(a_site()), s, f),
            // return: F flows back out to T, but a Restricted return is pruned.
            edge(FlowEdge::Return(a_site()), f, t),
        ]);

        let paths = find_endpoint_paths(&facts, &results);
        assert!(
            paths.is_empty(),
            "call/return mismatch must not surface as a realizable path"
        );
    }

    /// The realizable counterpart: a source in the caller flows through a `Call`
    /// into a callee, where the sink lives. The `Call` moves the annotation to
    /// `Restricted` but the sink is right there, so the path is found.
    #[test]
    fn finds_realizable_path_through_call() {
        let source = endpoint_on(1, formal(0), "X", TaintDirection::Forward);
        let sink = endpoint_on(2, formal(0), "X", TaintDirection::Backward);

        let s = node(1, formal(0));
        let f = node(2, formal(0));

        let facts = FormatFacts {
            taint: vec![
                taint_fact(1, formal(0), source.clone()),
                // The sink endpoint sits on the callee formal.
                taint_fact(2, formal(0), sink.clone()),
            ],
            ..Default::default()
        };
        // call: S flows into the callee formal.
        let results = taint_results(vec![edge(FlowEdge::Call(a_site()), s, f)]);

        let paths = find_endpoint_paths(&facts, &results);
        assert_eq!(paths.len(), 1, "expected the realizable call path");
        assert_eq!(paths[0].source, source);
        assert_eq!(paths[0].sink, sink);

        // The path ends on the callee formal F.
        let fg = build_taint_flow_graph(&facts, &results);
        assert_eq!(paths[0].nodes.last(), Some(&fg.node_to_id[&f]));
    }

    /// A `Return` taken while still `Free` (the search has not descended through
    /// a `Call`) is realizable: entering a callee's returned value and flowing
    /// back to an unknown caller is allowed, and keeps the annotation `Free`.
    #[test]
    fn finds_realizable_return_path() {
        let source = endpoint_on(2, formal(0), "X", TaintDirection::Forward);
        let sink = endpoint_on(1, formal(1), "X", TaintDirection::Backward);

        // f: source on a callee vertex; t: returned-to vertex in the caller.
        let f = node(2, formal(0));
        let t = node(1, formal(1));

        let facts = FormatFacts {
            taint: vec![
                taint_fact(2, formal(0), source.clone()),
                taint_fact(1, formal(1), sink.clone()),
            ],
            ..Default::default()
        };
        // return: F flows out to T while the annotation is still Free.
        let results = taint_results(vec![edge(FlowEdge::Return(a_site()), f, t)]);

        let paths = find_endpoint_paths(&facts, &results);
        assert_eq!(paths.len(), 1, "expected the realizable return path");
        assert_eq!(paths[0].source, source);
        assert_eq!(paths[0].sink, sink);

        let fg = build_taint_flow_graph(&facts, &results);
        assert_eq!(paths[0].nodes.first(), Some(&fg.node_to_id[&f]));
        assert_eq!(paths[0].nodes.last(), Some(&fg.node_to_id[&t]));
    }
}
