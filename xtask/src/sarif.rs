//! Validate the SARIF a case emits, using the `checksarif` wrapper around the
//! SARIF Multitool (`nix/sarif-multitool/checksarif.nix`).
//!
//! The wrapper checks a file against both the SARIF 2.1.0 JSON schema and the
//! Multitool's rule set, configured by `nix/sarif-multitool/sarif-validation.xml`.
//! Its exit code cannot be used as the verdict: it reports whether the *analysis
//! ran*, not whether the file was clean, so a schema-invalid log still exits 0.
//! The verdict is read out of the validation log the tool writes with
//! `--output` instead, where each diagnostic is a `result`.
//!
//! Both `error` and `warning` diagnostics fail the case. A result carries no
//! `level` unless it is an error, so the SARIF default of `warning` is what the
//! recommendation rules report at; taking only errors would silently accept
//! every one of them. A rule that does not apply to a binary/bytecode analyzer
//! is turned off in `sarif-validation.xml` (see `SARIF2017` there), which keeps
//! "checksarif says nothing" as the pass criterion rather than maintaining a
//! second, invisible allowlist here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::exec;

/// The validator wrapper, as it appears on `PATH`.
const CHECKSARIF: &str = "checksarif";

/// Where [`preflight`] recorded the validator, if it found one.
static VALIDATOR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolve the validator once, before any worker starts, and report whether
/// SARIF validation is in play.
///
/// A missing `checksarif` is not fatal. It is a .NET tool that only the Nix
/// environments supply (`nix develop`, and the `regression` check),
/// and a developer without it should still be able to run the suite -- so the
/// checks are skipped, loudly, rather than failing every case. CI runs inside
/// those environments, so the gate is enforced where it matters.
pub fn preflight() {
    let found = exec::which(CHECKSARIF);
    match &found {
        Some(path) => println!("Validating emitted SARIF with {}", path.display()),
        None => eprintln!(
            "warning: `{CHECKSARIF}` not found on PATH; emitted SARIF will not be validated.\n\
             warning: enter the dev shell (`nix develop`) to enable it."
        ),
    }
    // First (and only) writer; a later worker reading it back sees this value.
    let _ = VALIDATOR.set(found);
}

fn validator() -> Option<&'static Path> {
    VALIDATOR.get()?.as_deref()
}

/// Validate every file in `files`, writing the validation log into `work`.
///
/// Returns `Ok(None)` when they are all clean -- or when no validator is
/// available -- and `Ok(Some(why))` describing every diagnostic otherwise.
pub fn validate(work: &Path, files: &[&Path]) -> Result<Option<String>> {
    let Some(bin) = validator() else {
        return Ok(None);
    };

    let log = work.join("checksarif-log.sarif");
    let mut cmd = Command::new(bin);
    cmd.current_dir(work)
        .arg("--output")
        .arg(&log)
        // Rewrite the log rather than fail if a case validates twice.
        .args(["--log", "ForceOverwrite"])
        // A file over the size cap is *skipped*, not failed, which would read as
        // a clean run. Today's cases emit tens of KB against a 1 MB cap, but a
        // case that grew past it would go quietly unchecked; raise the cap out
        // of reach instead.
        .args(["--max-file-size-in-kb", "1048576"])
        // One or two files per case; the suite's own workers already own the
        // machine's parallelism.
        .args(["--threads", "1"]);
    for file in files {
        cmd.arg(file);
    }
    let out = exec::run_checked(cmd, CHECKSARIF)?;

    let mut problems = diagnostics(&log)
        .with_context(|| format!("failed to read the {CHECKSARIF} log {}", log.display()))?;

    // A file the tool declined to scan yields no diagnostics, which is
    // indistinguishable from a clean one in the log. The count is only reported
    // on the console, so read it there -- and only complain when it clearly
    // disagrees, so a change in wording cannot invent failures.
    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(scanned) = files_scanned(&stdout) {
        if scanned < files.len() {
            problems.push(format!(
                "{CHECKSARIF} scanned {scanned} of the {} file(s) it was given",
                files.len()
            ));
        }
    }

    if problems.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "invalid SARIF: {} {CHECKSARIF} diagnostic(s) (log: {}):\n{}",
        problems.len(),
        log.display(),
        problems.join("\n"),
    )))
}

/// Every error/warning diagnostic in a validation log, rendered one per line.
fn diagnostics(log: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(log)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;

    let mut out = Vec::new();
    for run in value["runs"].as_array().into_iter().flatten() {
        let templates = templates(&run["tool"]["driver"]);

        for result in run["results"].as_array().into_iter().flatten() {
            // Only an error carries an explicit level; everything else reports
            // at the SARIF default of `warning`. Both fail, so the only levels
            // to drop are the informational ones a rule can opt into.
            let level = result["level"].as_str().unwrap_or("warning");
            if !matches!(level, "error" | "warning") {
                continue;
            }
            let rule = result["ruleId"].as_str().unwrap_or("<no rule>");
            out.push(format!(
                "  {} {level} {rule}: {}",
                location(&result["locations"][0]["physicalLocation"]),
                message(&result["message"], rule, &templates),
            ));
        }

        // Tool-level failures (an unreadable target, a bad configuration) are
        // notifications rather than results, and never reach the results array.
        // Their default level is `warning` too, but here that is the tool
        // talking about itself -- disabling a rule raises one on every run --
        // so only an error is a failure.
        for invocation in run["invocations"].as_array().into_iter().flatten() {
            for key in [
                "toolConfigurationNotifications",
                "toolExecutionNotifications",
            ] {
                for note in invocation[key].as_array().into_iter().flatten() {
                    if note["level"].as_str() != Some("error") {
                        continue;
                    }
                    let descriptor = note["descriptor"]["id"].as_str().unwrap_or("<no id>");
                    out.push(format!(
                        "  {CHECKSARIF} error {descriptor}: {}",
                        message(&note["message"], descriptor, &templates),
                    ));
                }
            }
            if invocation["executionSuccessful"].as_bool() == Some(false) {
                out.push(format!("  {CHECKSARIF} did not complete its analysis"));
            }
        }
    }
    Ok(out)
}

/// Every message template the log's tool declares, keyed by the id of the rule
/// or notification descriptor that owns it and the id of the message itself.
///
/// A message id is only unique within its owner -- several rules declare an
/// `Error_Default` -- so both halves are needed to reach the right template.
fn templates(driver: &serde_json::Value) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    // Rules describe results; notification descriptors describe the tool's own
    // notifications. Both carry an `id` and a `messageStrings` bag.
    for descriptor in ["rules", "notifications"]
        .into_iter()
        .flat_map(|key| driver[key].as_array().into_iter().flatten())
    {
        let Some(owner) = descriptor["id"].as_str() else {
            continue;
        };
        for (id, string) in descriptor["messageStrings"]
            .as_object()
            .into_iter()
            .flatten()
        {
            if let Some(text) = string["text"].as_str() {
                out.insert((owner.to_string(), id.clone()), text.to_string());
            }
        }
    }
    out
}

/// Render a diagnostic's message, given the id of the rule or notification
/// descriptor it came from.
///
/// The Multitool writes messages in their formatted form -- a message id plus
/// the arguments to substitute into the owner's template -- rather than as text,
/// so the template has to be looked up and filled in. Falls back to whatever is
/// present when a message does not follow that shape.
fn message(
    message: &serde_json::Value,
    owner: &str,
    templates: &BTreeMap<(String, String), String>,
) -> String {
    if let Some(text) = message["text"].as_str() {
        return text.to_string();
    }
    let Some(id) = message["id"].as_str() else {
        return "<no message>".to_string();
    };
    let Some(template) = templates.get(&(owner.to_string(), id.to_string())) else {
        return id.to_string();
    };

    let mut text = template.clone();
    for (index, arg) in message["arguments"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let arg = arg.as_str().map_or_else(|| arg.to_string(), str::to_string);
        text = text.replace(&format!("{{{index}}}"), &arg);
    }
    text
}

/// `path(line,col)` for a diagnostic, matching how the Multitool prints one.
fn location(physical: &serde_json::Value) -> String {
    let uri = physical["artifactLocation"]["uri"]
        .as_str()
        .unwrap_or("<no file>");
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    match (
        physical["region"]["startLine"].as_i64(),
        physical["region"]["startColumn"].as_i64(),
    ) {
        (Some(line), Some(column)) => format!("{path}({line},{column}):"),
        (Some(line), None) => format!("{path}({line}):"),
        _ => format!("{path}:"),
    }
}

/// The file count from the Multitool's closing `Done. N files scanned.` line.
fn files_scanned(stdout: &str) -> Option<usize> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("Done."))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::{diagnostics, files_scanned};

    #[test]
    fn scanned_count_is_read_from_the_closing_line() {
        assert_eq!(
            files_scanned("Analyzing...\nDone. 2 files scanned.\n"),
            Some(2)
        );
        // No count to read is not a count of zero: it must not invent a failure.
        assert_eq!(files_scanned("Analysis completed successfully.\n"), None);
    }

    /// A log holding one error, one (level-less, therefore warning) rule
    /// diagnostic, one informational result to ignore, and the notification the
    /// tool raises on every run because a rule is disabled.
    const LOG: &str = r#"{
      "runs": [{
        "tool": { "driver": { "rules": [
          { "id": "SARIF1009", "messageStrings": {
              "Error_Index": { "text": "{0}: index {1} is out of range." } } },
          { "id": "SARIF2017", "messageStrings": {
              "Warning_MissingRegion": { "text": "{0}: no region." } } }
        ] } },
        "invocations": [{
          "toolConfigurationNotifications": [
            { "descriptor": { "id": "WRN999" }, "message": { "text": "rule disabled" } }
          ],
          "executionSuccessful": true
        }],
        "results": [
          { "ruleId": "SARIF1009", "level": "error",
            "message": { "id": "Error_Index", "arguments": ["runs[0].results[0]", "7"] },
            "locations": [{ "physicalLocation": {
              "artifactLocation": { "uri": "file:///tmp/out.sarif" },
              "region": { "startLine": 12, "startColumn": 3 } } }] },
          { "ruleId": "SARIF2017",
            "message": { "id": "Warning_MissingRegion", "arguments": ["runs[0].results[1]"] },
            "locations": [{ "physicalLocation": {
              "artifactLocation": { "uri": "file:///tmp/out.sarif" } } }] },
          { "ruleId": "SARIF2099", "level": "note", "message": { "text": "just a note" } }
        ]
      }]
    }"#;

    /// Parse `log` from disk (what [`diagnostics`] reads). `name` keeps
    /// concurrently running tests off each other's scratch directory.
    fn parse(name: &str, log: &str) -> Vec<String> {
        let dir =
            std::env::temp_dir().join(format!("xtask_sarif_test_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.sarif");
        std::fs::write(&path, log).unwrap();
        let found = diagnostics(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    #[test]
    fn errors_and_warnings_are_reported_with_their_message_filled_in() {
        let found = parse("rendered", LOG);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(
            found[0].trim(),
            "/tmp/out.sarif(12,3): error SARIF1009: runs[0].results[0]: index 7 is out of range."
        );
        // A missing region leaves nothing to point at but the file itself.
        assert_eq!(
            found[1].trim(),
            "/tmp/out.sarif: warning SARIF2017: runs[0].results[1]: no region."
        );
    }

    #[test]
    fn informational_results_and_non_error_notifications_are_ignored() {
        let found = parse("ignored", LOG);
        assert!(
            !found
                .iter()
                .any(|d| d.contains("SARIF2099") || d.contains("WRN999")),
            "{found:#?}"
        );
    }

    #[test]
    fn a_failed_tool_run_is_a_diagnostic_of_its_own() {
        let log = LOG.replace(
            "\"executionSuccessful\": true",
            "\"executionSuccessful\": false",
        );
        let found = parse("failed_run", &log);
        assert!(
            found.iter().any(|d| d.contains("did not complete")),
            "{found:#?}"
        );
    }

    #[test]
    fn error_notifications_are_reported() {
        let log = LOG.replace(
            "{ \"descriptor\": { \"id\": \"WRN999\" }, \"message\": { \"text\": \"rule disabled\" } }",
            "{ \"descriptor\": { \"id\": \"ERR997\" }, \"level\": \"error\",\
             \"message\": { \"text\": \"No valid analysis targets were specified.\" } }",
        );
        let found = parse("notifications", &log);
        assert!(
            found
                .iter()
                .any(|d| d.contains("ERR997")
                    && d.contains("No valid analysis targets were specified.")),
            "{found:#?}"
        );
    }
}
