//! What `ctadl index` has to tell a reader about the index it built, beyond the log: each place
//! it analyzed in a way that may miss flows. Written next to the index as `notifications.log`, one
//! fact per line, and carried by `ctadl query` into the SARIF's `toolExecutionNotifications`, so
//! a missing flow can be traced to it.
//!
//! The file is kept to what a reader acts on, like `log_once::warn_once!`: each fact is stated
//! once, never once per site, and a line holds no counts, so the same fact always reads the same
//! way. Counts and sites go in the debug log.

use std::collections::BTreeSet;
use std::path::Path;

pub const NOTIFICATIONS_FILE: &str = "notifications.log";

/// Writes `lines` to the index at `dir`, replacing what an earlier index left there.
pub fn save(dir: impl AsRef<Path>, lines: &BTreeSet<String>) -> std::io::Result<()> {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(dir.as_ref().join(NOTIFICATIONS_FILE), text)
}

/// The lines of the index at `dir`. An index built before the file existed has none.
pub fn load(dir: impl AsRef<Path>) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(dir.as_ref().join(NOTIFICATIONS_FILE)) {
        Ok(text) => Ok(text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
