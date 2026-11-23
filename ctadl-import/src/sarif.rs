//! What every SARIF file ctadl writes shares: the tool it names, the invocation's command line
//! and times, how notifications and their descriptors are built, and the document's key order.
//! `ctadl query`'s results and each step's `notifications.sarif` (see [`crate::notifications`])
//! are both built from these.

use std::collections::BTreeMap;

use serde_sarif::sarif::{
    Message, MultiformatMessageString, Notification, PropertyBag, ReportingDescriptor,
    ReportingDescriptorReference, ToolComponent,
};

use crate::notifications::DEFAULT_MESSAGE;

/// The tool every SARIF file names as its `tool.driver`.
pub const TOOL_NAME: &str = "ctadl";
/// The driver's `version` and `semanticVersion`: the workspace version, so `Cargo.toml` is the
/// one place to change it.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TOOL_INFORMATION_URI: &str = "https://github.com/sandialabs/ctadl-rs";
pub const TOOL_FULL_DESCRIPTION: &str = "CTADL (Compositional Taint Analysis in Datalog).";
pub const SCHEMA_URI: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/schemata/sarif-schema-2.1.0.json";

/// The `tool.driver` every SARIF file names, without rules or notification descriptors.
pub fn driver() -> ToolComponent {
    ToolComponent::builder()
        .name(TOOL_NAME)
        .version(TOOL_VERSION)
        .semantic_version(TOOL_VERSION)
        .information_uri(TOOL_INFORMATION_URI)
        .full_description(
            MultiformatMessageString::builder()
                .text(TOOL_FULL_DESCRIPTION)
                .build(),
        )
        .build()
}

/// A UTC timestamp in the `date-time` form SARIF requires (§3.9).
pub fn utc_timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// This process's command line, for a SARIF `invocation.commandLine`.
pub fn command_line() -> String {
    std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// This process's arguments after the program name, for a SARIF `invocation.arguments`.
pub fn arguments() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Build a notification carrying `message.id` + `arguments` (so consumers can read it
/// structurally rather than by parsing prose) plus a `properties` bag of raw counts.
pub fn notification(
    descriptor_id: &str,
    level: &str,
    arguments: Vec<String>,
    text: String,
    properties: BTreeMap<String, serde_json::Value>,
) -> Notification {
    notification_with_message_id(
        descriptor_id,
        DEFAULT_MESSAGE,
        level,
        arguments,
        text,
        properties,
    )
}

/// [`notification`] for a descriptor that declares more than one message string. `message_id`
/// selects which (§3.11.7) and must be a key of the descriptor's `messageStrings`, otherwise
/// a consumer re-rendering the message from `arguments` has nothing to render.
pub fn notification_with_message_id(
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
pub fn notification_descriptor(
    id: &str,
    name: &str,
    description: &str,
    message: &str,
) -> ReportingDescriptor {
    notification_descriptor_multi(id, name, description, &[(DEFAULT_MESSAGE, message)])
}

/// [`notification_descriptor`] for a notification whose prose depends on which of several
/// conditions fired, one `messageStrings` entry per condition (§3.49.11). The entry a given
/// notification selects is its `message.id`; `default` is the one used when the condition is
/// not known more precisely.
pub fn notification_descriptor_multi(
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

/// The SARIF 2.1.0 document holding `run`, with `properties` when given, as JSON in a fixed key
/// order so that diffs between files stay stable: `version`, `properties`, `$schema`, then
/// `runs`; within the run, `tool`, then `invocations` (its status), then the rest.
pub fn document(
    properties: Option<PropertyBag>,
    run: &serde_sarif::sarif::Run,
) -> serde_json::Value {
    let mut run = match serde_json::to_value(run).expect("a SARIF run serializes") {
        serde_json::Value::Object(map) => map,
        _ => unreachable!("a SARIF run serializes to an object"),
    };
    let mut ordered_run = serde_json::Map::new();
    for key in ["tool", "invocations"] {
        if let Some(value) = run.remove(key) {
            ordered_run.insert(key.to_string(), value);
        }
    }
    ordered_run.extend(run);

    let mut doc = serde_json::Map::new();
    doc.insert("version".to_string(), serde_json::json!("2.1.0"));
    if let Some(properties) = properties {
        doc.insert(
            "properties".to_string(),
            serde_json::to_value(properties).expect("a SARIF property bag serializes"),
        );
    }
    doc.insert("$schema".to_string(), serde_json::json!(SCHEMA_URI));
    doc.insert(
        "runs".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::Object(ordered_run)]),
    );
    serde_json::Value::Object(doc)
}
