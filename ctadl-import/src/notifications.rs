//! What a ctadl step has to tell a reader about what it built, beyond the log: each place it
//! analyzed in a way that may miss flows, and each check of its own output that failed. An import
//! or an index writes them next to itself as `notifications.sarif`, a SARIF 2.1.0 log whose one
//! run holds them as `invocations[0].toolExecutionNotifications` and declares their descriptors
//! as `tool.driver.notifications`. `ctadl index` carries its imports' notifications into its own
//! file, and `ctadl query` copies the index's into the SARIF it writes, descriptors included, so
//! it needs to know none of them. Both are built as `ctadl query` builds its own notifications
//! (see [`crate::sarif`]).
//!
//! A notification states one fact, never once per site. Its message has both forms: `text`, and
//! a `message.id` selecting one of its descriptor's `messageStrings` with the `arguments` that
//! fill it, so a consumer can read it without parsing prose. Counts a tool may want to check go
//! in its `properties` too; sites go in the debug log.

use std::collections::BTreeMap;
use std::path::Path;

use serde_sarif::sarif::{
    Invocation, Notification, PropertyBag, ReportingDescriptor, Run, Sarif, Tool,
};

use crate::sarif;

pub const NOTIFICATIONS_FILE: &str = "notifications.sarif";

/// The `message.id` of a descriptor with a single message string.
pub const DEFAULT_MESSAGE: &str = "default";

/// A kind of notification: its SARIF `reportingDescriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    /// `CTADLnnnn.kebab-name`, unique across ctadl, the query's own included.
    pub id: &'static str,
    /// A short name, in sentence case.
    pub name: &'static str,
    /// What one of these means for the results, in one sentence.
    pub description: &'static str,
    /// Its `messageStrings`: each `message.id` and the template its `{n}` arguments fill.
    pub messages: &'static [(&'static str, &'static str)],
}

impl Descriptor {
    /// The text of message `message_id` with `arguments` filled in.
    ///
    /// # Panics
    /// If this descriptor has no message `message_id`.
    pub fn render(&self, message_id: &str, arguments: &[String]) -> String {
        let (_, template) = self
            .messages
            .iter()
            .find(|(id, _)| *id == message_id)
            .unwrap_or_else(|| panic!("{} has no message `{message_id}`", self.id));
        arguments
            .iter()
            .enumerate()
            .fold(template.to_string(), |text, (i, arg)| {
                text.replace(&format!("{{{i}}}"), arg)
            })
    }

    fn reporting_descriptor(&self) -> ReportingDescriptor {
        sarif::notification_descriptor_multi(self.id, self.name, self.description, self.messages)
    }
}

/// A function `--mem2reg full` promoted fully where an escaped address reaches a promoted slot in
/// a way the reloads do not cover. One per function; the arguments are the function and the
/// kinds of escape.
pub const STACK_SLOTS_PROMOTED_FULLY: Descriptor = Descriptor {
    id: "CTADL0104.stack-slots-promoted-fully",
    name: "Stack slots promoted fully",
    description: "A function's stack slots were promoted although an escaped address reaches \
                  them, so flows through those slots may be missing.",
    messages: &[(
        DEFAULT_MESSAGE,
        "mem2reg full: {0}: frame address escapes ({1}); flows through those slots may be missed",
    )],
};

/// What the dex frontend's split of R8-merged classes did, for the record: its counts are in
/// `properties`. One per import that has merged classes.
pub const MERGED_CLASSES_SPLIT: Descriptor = Descriptor {
    id: "CTADL0105.merged-classes-split",
    name: "Merged classes split",
    description: "The dex frontend split classes R8 merged into one per class id.",
    messages: &[(DEFAULT_MESSAGE, "split {0} merged classes")],
};

/// The split left merged methods live, so calls on a merged object take on every lambda merged
/// into its class. One per failed check, which its `message.id` names.
pub const MERGED_CLASSES_NOT_SPLIT: Descriptor = Descriptor {
    id: "CTADL0106.merged-classes-not-split",
    name: "Merged classes not split",
    description: "Some R8-merged classes stay merged, so the results may hold false flows \
                  across the lambdas merged into them.",
    messages: &[
        (
            "overCap",
            "{0} merged classes have more than {1} ids and stay merged; their methods are the \
             union of every lambda behind them",
        ),
        (
            "notClosed",
            "{0} of {1} split classes still have live merged methods (an object of the class \
             itself may exist); calls resolved by class hierarchy reach the union of every arm",
        ),
        (
            "unresolvedAllocations",
            "{0} of {1} allocations of merged classes have no known id and build the merged class \
             instead of its split class",
        ),
        (
            "cloneStatements",
            "the clones of merged methods hold {0} of the program's {1} statements; a clone \
             should keep only its own id's arm",
        ),
    ],
};

/// How many call edges reach large merged methods the split left live, for the record. One per
/// index of a program with such methods.
pub const MERGED_METHOD_CALLS: Descriptor = Descriptor {
    id: "CTADL0107.merged-method-calls",
    name: "Calls to live merged methods",
    description: "Calls that reach merged methods the class split left live.",
    messages: &[(
        DEFAULT_MESSAGE,
        "{0} of {1} call edges reach one of {2} large merged methods the class split left live",
    )],
};

/// Many calls reach large merged methods the split left live. One per index.
pub const MERGED_METHOD_CALLS_HIGH: Descriptor = Descriptor {
    id: "CTADL0108.many-merged-method-calls",
    name: "Many calls to live merged methods",
    description: "Many calls reach merged methods the class split left live, so the results may \
                  hold false flows across the lambdas merged into them.",
    messages: &[(
        DEFAULT_MESSAGE,
        "{0} call edges reach {1} large merged methods the class split left live; each such call \
         takes on every lambda merged into the method's class",
    )],
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

/// Where notifications carried into an index came from, recorded on each as a property.
#[derive(Debug, Clone, Copy)]
pub enum Origin<'a> {
    /// An import the index read: `properties.import` names it.
    Import(&'a str),
    /// A summary project the index read: `properties.summaryProject` names it.
    SummaryProject(&'a str),
}

/// A set of notifications and the descriptors they use, and when the step that made them began.
#[derive(Debug, Default, Clone)]
pub struct Notifications {
    start_time_utc: Option<String>,
    descriptors: BTreeMap<String, ReportingDescriptor>,
    notifications: Vec<Notification>,
}

impl Notifications {
    /// None yet, for a step that begins now: [`Notifications::save`] records the time as the
    /// invocation's `startTimeUtc`.
    pub fn new() -> Self {
        Notifications {
            start_time_utc: Some(sarif::utc_timestamp()),
            ..Notifications::default()
        }
    }

    /// Adds a notification of kind `descriptor` with message `message_id` filled from
    /// `arguments`, unless an equal one is already here.
    ///
    /// # Panics
    /// If `descriptor` has no message `message_id`.
    pub fn push(
        &mut self,
        descriptor: &Descriptor,
        message_id: &str,
        level: Level,
        arguments: Vec<String>,
        properties: BTreeMap<String, serde_json::Value>,
    ) {
        let text = descriptor.render(message_id, &arguments);
        self.descriptors
            .entry(descriptor.id.to_string())
            .or_insert_with(|| descriptor.reporting_descriptor());
        self.insert(sarif::notification_with_message_id(
            descriptor.id,
            message_id,
            level.as_str(),
            arguments,
            text,
            properties,
        ));
    }

    /// Adds every notification of `other`, and the descriptors they use. With `origin`, each
    /// records it in its properties unless it already names one of that kind.
    pub fn extend(&mut self, other: Notifications, origin: Option<Origin<'_>>) {
        let origin = origin.map(|origin| match origin {
            Origin::Import(name) => ("import", name),
            Origin::SummaryProject(name) => ("summaryProject", name),
        });
        for (id, descriptor) in other.descriptors {
            self.descriptors.entry(id).or_insert(descriptor);
        }
        for mut notification in other.notifications {
            if let Some((key, name)) = origin {
                notification
                    .properties
                    .get_or_insert_with(|| PropertyBag::builder().build())
                    .additional_properties
                    .entry(key.to_string())
                    .or_insert_with(|| serde_json::json!(name));
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
    /// records this process's command line, which says which step wrote them, and ends now.
    pub fn save(&self, dir: impl AsRef<Path>) -> std::io::Result<()> {
        let mut driver = sarif::driver();
        driver.notifications = Some(self.descriptors.values().cloned().collect());
        let mut invocation = Invocation::builder()
            .execution_successful(true)
            .command_line(sarif::command_line())
            .arguments(sarif::arguments())
            .end_time_utc(sarif::utc_timestamp())
            .tool_execution_notifications(self.notifications.clone())
            .build();
        invocation.start_time_utc = self.start_time_utc.clone();
        let run = Run::builder()
            .tool(Tool::builder().driver(driver).build())
            .invocations(vec![invocation])
            .build();
        let text = serde_json::to_string_pretty(&sarif::document(None, &run))
            .map_err(std::io::Error::other)?;
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

/// The property `key` of `notification`, if it has one.
pub fn property<'a>(notification: &'a Notification, key: &str) -> Option<&'a serde_json::Value> {
    notification
        .properties
        .as_ref()?
        .additional_properties
        .get(key)
}

/// The integer property `key` of `notification`, if it has one.
pub fn property_u64(notification: &Notification, key: &str) -> Option<u64> {
    property(notification, key)?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: Descriptor = Descriptor {
        id: "CTADL9999.test-kind",
        name: "Test kind",
        description: "A notification for the tests.",
        messages: &[
            (DEFAULT_MESSAGE, "{0} was analyzed loosely"),
            ("other", "{0} and {1}"),
        ],
    };

    fn count(n: u64) -> BTreeMap<String, serde_json::Value> {
        [("count".to_string(), serde_json::json!(n))].into()
    }

    #[test]
    fn save_then_load_keeps_notifications_and_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = Notifications::new();
        for _ in 0..2 {
            n.push(
                &KIND,
                DEFAULT_MESSAGE,
                Level::Warning,
                vec!["f".into()],
                count(3),
            );
        }
        n.save(dir.path()).unwrap();

        let back = Notifications::load(dir.path()).unwrap();
        assert_eq!(back.len(), 1, "an equal notification is kept once");
        let only = &back.notifications()[0];
        assert_eq!(descriptor_id(only), Some(KIND.id));
        assert_eq!(text(only), "f was analyzed loosely");
        assert_eq!(only.message.id.as_deref(), Some(DEFAULT_MESSAGE));
        assert_eq!(only.message.arguments, Some(vec!["f".to_string()]));
        assert_eq!(property_u64(only, "count"), Some(3));
        assert_eq!(only.level, Some(serde_json::json!("warning")));
        assert_eq!(back.of(&KIND).count(), 1);
        let declared: Vec<_> = back.descriptors().collect();
        assert_eq!(declared.len(), 1);
        assert_eq!(
            declared[0].message_strings.as_ref().map(|m| m.len()),
            Some(2)
        );

        let file = std::fs::read_to_string(dir.path().join(NOTIFICATIONS_FILE)).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&file).unwrap();
        let keys: Vec<_> = doc.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["version", "$schema", "runs"]);
        let run = &doc["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], sarif::TOOL_NAME);
        assert_eq!(run["tool"]["driver"]["version"], sarif::TOOL_VERSION);
        let invocation = &run["invocations"][0];
        assert_eq!(invocation["commandLine"], sarif::command_line());
        assert!(invocation["startTimeUtc"].is_string());
        assert!(invocation["endTimeUtc"].is_string());
    }

    #[test]
    fn a_missing_file_has_no_notifications() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Notifications::load(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn render_fills_the_selected_message() {
        assert_eq!(KIND.render("other", &["a".into(), "b".into()]), "a and b");
    }

    #[test]
    #[should_panic(expected = "has no message `missing`")]
    fn an_unknown_message_id_panics() {
        KIND.render("missing", &[]);
    }

    #[test]
    fn extend_records_the_origin_once() {
        let mut import = Notifications::default();
        import.push(
            &KIND,
            DEFAULT_MESSAGE,
            Level::Note,
            vec!["f".into()],
            BTreeMap::new(),
        );
        let mut summary_index = Notifications::default();
        summary_index.extend(import, Some(Origin::Import("lib")));
        let mut app = Notifications::default();
        app.extend(summary_index, Some(Origin::SummaryProject("libproj")));
        let mut again = Notifications::default();
        again.extend(app.clone(), Some(Origin::Import("other")));

        assert_eq!(app.len(), 1);
        let only = &app.notifications()[0];
        assert_eq!(text(only), "f was analyzed loosely");
        assert_eq!(property(only, "import"), Some(&serde_json::json!("lib")));
        assert_eq!(
            property(&again.notifications()[0], "import"),
            Some(&serde_json::json!("lib")),
            "an import already named is not renamed"
        );
        assert_eq!(
            property(only, "summaryProject"),
            Some(&serde_json::json!("libproj"))
        );
        assert_eq!(app.descriptors().count(), 1);
    }
}
