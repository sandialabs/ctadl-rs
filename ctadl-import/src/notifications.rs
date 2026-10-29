//! What a ctadl step has to tell a reader about what it built, beyond the log: each place it
//! analyzed in a way that may miss flows, and each check of its own output that failed. An import
//! or an index writes them next to itself as `notifications.sarif`, a SARIF 2.1.0 log whose one
//! run holds them as `invocations[0].toolExecutionNotifications` and declares their descriptors
//! as `tool.driver.notifications`. `ctadl index` carries its imports' notifications into its own
//! file, and `ctadl query` copies the index's into the SARIF it writes, descriptors included, so
//! it needs to know none of them.
//!
//! A notification states one fact, never once per site, so the same fact always reads the same
//! way. Counts a tool may want to check go in its `properties`; sites go in the debug log.

use std::collections::BTreeMap;
use std::path::Path;

use serde_sarif::sarif::{
    Invocation, Message, MultiformatMessageString, Notification, PropertyBag, ReportingDescriptor,
    ReportingDescriptorReference, Run, Sarif, Tool, ToolComponent,
};

pub const NOTIFICATIONS_FILE: &str = "notifications.sarif";

/// The SARIF `tool.driver` every SARIF file ctadl writes names: its `name`, `version` (also its
/// `semanticVersion`; the workspace version, so `Cargo.toml` is the one place to change it) and
/// `informationUri`.
pub const TOOL_NAME: &str = "ctadl";
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TOOL_INFORMATION_URI: &str = "https://github.com/sandialabs/ctadl-rs";

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

/// A kind of notification: its SARIF `reportingDescriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    /// `CTADLnnnn.kebab-name`, unique across ctadl, the query's own included.
    pub id: &'static str,
    /// A short name, in sentence case.
    pub name: &'static str,
    /// What one of these means for the results, in one sentence.
    pub description: &'static str,
}

/// A function `--mem2reg full` promoted fully where an escaped address reaches a promoted slot in
/// a way the reloads do not cover. One per function.
pub const STACK_SLOTS_PROMOTED_FULLY: Descriptor = Descriptor {
    id: "CTADL0104.stack-slots-promoted-fully",
    name: "Stack slots promoted fully",
    description: "A function's stack slots were promoted although an escaped address reaches \
                  them, so flows through those slots may be missing.",
};

/// What the dex frontend's split of R8-merged classes did, for the record: its counts are in
/// `properties`. One per import that has merged classes.
pub const MERGED_CLASSES_SPLIT: Descriptor = Descriptor {
    id: "CTADL0105.merged-classes-split",
    name: "Merged classes split",
    description: "The dex frontend split classes R8 merged into one per class id.",
};

/// The split left merged methods live, so calls on a merged object take on every lambda merged
/// into its class. One per failed check: too many classes over the id cap, too many classes not
/// closed, or clones holding too much of the program.
pub const MERGED_CLASSES_NOT_SPLIT: Descriptor = Descriptor {
    id: "CTADL0106.merged-classes-not-split",
    name: "Merged classes not split",
    description: "Some R8-merged classes stay merged, so the results may hold false flows \
                  across the lambdas merged into them.",
};

/// How many call edges reach large merged methods the split left live, for the record: its
/// counts are in `properties`. One per index of a program with such methods.
pub const MERGED_METHOD_CALLS: Descriptor = Descriptor {
    id: "CTADL0107.merged-method-calls",
    name: "Calls to live merged methods",
    description: "Calls that reach merged methods the class split left live.",
};

/// Many calls reach large merged methods the split left live. One per index.
pub const MERGED_METHOD_CALLS_HIGH: Descriptor = Descriptor {
    id: "CTADL0108.many-merged-method-calls",
    name: "Many calls to live merged methods",
    description: "Many calls reach merged methods the class split left live, so the results may \
                  hold false flows across the lambdas merged into them.",
};

/// A notification's SARIF `level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// For the record: nothing is wrong.
    Note,
    /// The results may be missing flows, or hold false ones.
    Warning,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Note => "note",
            Level::Warning => "warning",
        }
    }
}

/// A set of notifications and the descriptors they use.
#[derive(Debug, Default, Clone)]
pub struct Notifications {
    descriptors: BTreeMap<String, ReportingDescriptor>,
    notifications: Vec<Notification>,
}

impl Notifications {
    /// Adds a notification of kind `descriptor`, unless an equal one is already here.
    pub fn push(
        &mut self,
        descriptor: &Descriptor,
        level: Level,
        text: impl Into<String>,
        properties: BTreeMap<String, serde_json::Value>,
    ) {
        self.descriptors
            .entry(descriptor.id.to_string())
            .or_insert_with(|| {
                ReportingDescriptor::builder()
                    .id(descriptor.id.to_string())
                    .name(descriptor.name.to_string())
                    .short_description(
                        MultiformatMessageString::builder()
                            .text(descriptor.description.to_string())
                            .build(),
                    )
                    .build()
            });
        let mut notification = Notification::builder()
            .descriptor(
                ReportingDescriptorReference::builder()
                    .id(descriptor.id.to_string())
                    .build(),
            )
            // `level` is `Option<serde_json::Value>` in serde-sarif, so the enum value goes in as
            // a JSON string.
            .level(serde_json::json!(level.as_str()))
            .message(Message::builder().text(text.into()).build())
            .build();
        // Set after building: the generated builder is type-state, so an optional field cannot
        // be assigned conditionally.
        if !properties.is_empty() {
            notification.properties = Some(
                PropertyBag::builder()
                    .additional_properties(properties)
                    .build(),
            );
        }
        self.insert(notification);
    }

    /// Adds every notification of `other`, and the descriptors they use. With `prefix`, each
    /// message starts with it: an index says this way which import or summary project a
    /// notification came from.
    pub fn extend(&mut self, other: Notifications, prefix: Option<&str>) {
        for (id, descriptor) in other.descriptors {
            self.descriptors.entry(id).or_insert(descriptor);
        }
        for mut notification in other.notifications {
            if let (Some(prefix), Some(text)) = (prefix, notification.message.text.as_mut()) {
                *text = format!("{prefix}{text}");
            }
            self.insert(notification);
        }
    }

    fn insert(&mut self, notification: Notification) {
        if !self.notifications.contains(&notification) {
            self.notifications.push(notification);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }

    pub fn len(&self) -> usize {
        self.notifications.len()
    }

    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &ReportingDescriptor> {
        self.descriptors.values()
    }

    /// The notifications of kind `descriptor`.
    pub fn of<'a>(&'a self, descriptor: &'a Descriptor) -> impl Iterator<Item = &'a Notification> {
        self.notifications
            .iter()
            .filter(move |n| descriptor_id(n) == Some(descriptor.id))
    }

    /// Writes these to `dir`'s `notifications.sarif`, replacing what was there. The invocation
    /// records this process's command line, which says which step wrote them.
    pub fn save(&self, dir: impl AsRef<Path>) -> std::io::Result<()> {
        let driver = ToolComponent::builder()
            .name(TOOL_NAME)
            .version(TOOL_VERSION)
            .semantic_version(TOOL_VERSION)
            .information_uri(TOOL_INFORMATION_URI)
            .notifications(self.descriptors.values().cloned().collect::<Vec<_>>())
            .build();
        let invocation = Invocation::builder()
            .execution_successful(true)
            .command_line(command_line())
            .arguments(arguments())
            .tool_execution_notifications(self.notifications.clone())
            .build();
        let run = Run::builder()
            .tool(Tool::builder().driver(driver).build())
            .invocations(vec![invocation])
            .build();
        let sarif = Sarif::builder().version("2.1.0").runs(vec![run]).build();
        let text = serde_json::to_string_pretty(&sarif).map_err(std::io::Error::other)?;
        std::fs::write(dir.as_ref().join(NOTIFICATIONS_FILE), text + "\n")
    }

    /// The notifications in `dir`'s `notifications.sarif`. A step that wrote no file has none.
    pub fn load(dir: impl AsRef<Path>) -> std::io::Result<Notifications> {
        let path = dir.as_ref().join(NOTIFICATIONS_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Notifications::default());
            }
            Err(e) => return Err(e),
        };
        let sarif: Sarif = serde_json::from_str(&text).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
        let mut out = Notifications::default();
        for run in sarif.runs {
            for descriptor in run.tool.driver.notifications.into_iter().flatten() {
                out.descriptors
                    .entry(descriptor.id.clone())
                    .or_insert(descriptor);
            }
            for invocation in run.invocations.into_iter().flatten() {
                for notification in invocation
                    .tool_execution_notifications
                    .into_iter()
                    .flatten()
                {
                    out.insert(notification);
                }
            }
        }
        Ok(out)
    }
}

/// The id of the descriptor `notification` refers to.
pub fn descriptor_id(notification: &Notification) -> Option<&str> {
    notification.descriptor.as_ref()?.id.as_deref()
}

/// `notification`'s message text.
pub fn text(notification: &Notification) -> &str {
    notification.message.text.as_deref().unwrap_or_default()
}

/// The integer property `key` of `notification`, if it has one.
pub fn property_u64(notification: &Notification, key: &str) -> Option<u64> {
    notification
        .properties
        .as_ref()?
        .additional_properties
        .get(key)?
        .as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: Descriptor = Descriptor {
        id: "CTADL9999.test-kind",
        name: "Test kind",
        description: "A notification for the tests.",
    };

    #[test]
    fn save_then_load_keeps_notifications_and_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = Notifications::default();
        n.push(
            &KIND,
            Level::Warning,
            "f was analyzed loosely",
            [("count".to_string(), serde_json::json!(3))].into(),
        );
        n.push(
            &KIND,
            Level::Warning,
            "f was analyzed loosely",
            [("count".to_string(), serde_json::json!(3))].into(),
        );
        n.save(dir.path()).unwrap();

        let back = Notifications::load(dir.path()).unwrap();
        assert_eq!(back.len(), 1, "an equal notification is kept once");
        let only = &back.notifications()[0];
        assert_eq!(descriptor_id(only), Some(KIND.id));
        assert_eq!(text(only), "f was analyzed loosely");
        assert_eq!(property_u64(only, "count"), Some(3));
        assert_eq!(only.level, Some(serde_json::json!("warning")));
        assert_eq!(back.descriptors().count(), 1);
        assert_eq!(back.of(&KIND).count(), 1);
        let text = std::fs::read_to_string(dir.path().join(NOTIFICATIONS_FILE)).unwrap();
        let sarif: Sarif = serde_json::from_str(&text).unwrap();
        let run = &sarif.runs[0];
        assert_eq!(run.tool.driver.name, TOOL_NAME);
        let invocation = &run.invocations.as_ref().unwrap()[0];
        assert_eq!(
            invocation.command_line.as_deref(),
            Some(command_line().as_str())
        );
    }

    #[test]
    fn a_missing_file_has_no_notifications() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Notifications::load(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn extend_prefixes_messages() {
        let mut inner = Notifications::default();
        inner.push(&KIND, Level::Note, "split 3 classes", BTreeMap::new());
        let mut outer = Notifications::default();
        outer.extend(inner, Some("import 'app': "));
        assert_eq!(
            text(&outer.notifications()[0]),
            "import 'app': split 3 classes"
        );
        assert_eq!(outer.descriptors().count(), 1);
    }
}
