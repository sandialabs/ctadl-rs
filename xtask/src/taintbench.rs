//! TaintBench regression suite.
//!
//! [TaintBench](https://github.com/TaintBench) is a set of real Android malware
//! apps, each shipped with a hand-curated `*_findings.json` of ground-truth taint
//! flows (a labelled source and sink per finding). This task runs ctadl on an
//! app's APK and checks how many of those ground-truth flows we reproduce.
//!
//! **ctadl must find a connected source->sink path, but we do not check that its
//! intermediate steps mirror TaintBench's.** We match a path on the *callee
//! method* of its two endpoints: TaintBench identifies a source/sink by the
//! framework method it calls (its `targetName` and the `<Class: ret method(args)>`
//! in the Jimple IR), and ctadl's `--sarif-profile agent` tainted-path results
//! carry the same `sourceCallee`/`sinkCallee`. A finding is **detected** when
//! ctadl reports a tainted path whose source callee and sink callee match the
//! finding's source and sink. (The per-endpoint `taint-source`/`taint-sink`
//! results are still parsed, but only for the diagnostic `source:`/`sink:` hit
//! columns -- the match itself requires a connected path.)
//!
//! The pass criterion is a **baseline snapshot**: each app dir carries an
//! `expected.json` listing the finding IDs ctadl currently detects, plus the
//! ones flagged `isNegative` (non-flows) it currently reports anyway. The check
//! fails if any baseline finding stops being detected (a regression) or if a
//! negative outside the baseline gets reported (a *new* false positive). Newly
//! detected findings and false positives that go away do not fail; they are
//! reported as improvements to fold into the baseline.
//!
//! An app whose `app.json` carries an `excluded` reason is kept but left out of
//! the default run: `nix/taintbench.nix` does not fetch its APK, and this task
//! reports it skipped with that reason. Naming its APK with `--apk` still runs
//! it.
//!
//! An app's `model.json` is passed to both `ctadl index` and `ctadl query`,
//! since the two phases consume different halves of it: index turns its
//! `propagation` models into function summaries -- several of these apps
//! exfiltrate through framework classes with no body in the APK, and the taint
//! stops there without one -- and query reads its sources and sinks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::exec;

/// SARIF rule IDs emitted by `ctadl query --sarif-profile agent`.
const TAINT_SOURCE_RULE_ID: &str = "C0003.taint-source";
const TAINT_SINK_RULE_ID: &str = "C0004.taint-sink";
const TAINTED_PATH_RULE_ID: &str = "C0001.tainted-path";

#[derive(Default)]
pub struct Options {
    /// Directory of per-app subdirectories (default: auto-detect
    /// `taintbench/apps` or `../taintbench/apps`).
    pub apps_dir: Option<PathBuf>,
    /// APKs to analyze, as `(app_name, path)`. An app with no APK is skipped
    /// (the Nix check passes the fetched APK path here).
    pub apks: Vec<(String, PathBuf)>,
    /// Only run apps whose name contains this substring.
    pub filter: Option<String>,
}

enum Outcome {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Run the suite. Returns `Ok(true)` if every selected app passed or was skipped.
pub fn run(opts: &Options) -> Result<bool> {
    let apps_dir = resolve_apps_dir(opts.apps_dir.as_deref())?;
    let mut apps = discover_apps(&apps_dir)?;
    if let Some(filter) = &opts.filter {
        apps.retain(|a| a.name.contains(filter));
    }
    if apps.is_empty() {
        bail!("no TaintBench apps found under {}", apps_dir.display());
    }

    if exec::which("ctadl").is_none() {
        bail!("`ctadl` not found on PATH");
    }

    let apk_by_name: BTreeMap<&str, &Path> = opts
        .apks
        .iter()
        .map(|(n, p)| (n.as_str(), p.as_path()))
        .collect();

    let mut results: Vec<(String, Outcome)> = Vec::new();
    for app in &apps {
        // An excluded app still runs when its APK is named explicitly -- the
        // exclusion keeps it out of the default run, it does not retire it.
        let outcome = match (apk_by_name.get(app.name.as_str()), &app.excluded) {
            (Some(apk), _) => {
                run_app(app, apk).unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")))
            }
            (None, Some(why)) => Outcome::Skip(format!("excluded: {why}")),
            (None, None) => Outcome::Skip("no APK provided (pass --apk <name>=<path>)".to_string()),
        };
        results.push((app.name.clone(), outcome));
    }

    println!(
        "\nRan {} TaintBench app(s) (apps from {})",
        results.len(),
        apps_dir.display()
    );
    let (mut passed, mut skipped, mut failures) = (0, 0, 0);
    for (name, outcome) in &results {
        match outcome {
            Outcome::Pass(why) => {
                passed += 1;
                println!("  PASS  {name}  ({why})");
            }
            Outcome::Skip(why) => {
                skipped += 1;
                println!("  SKIP  {name}  ({why})");
            }
            Outcome::Fail(why) => {
                failures += 1;
                println!(
                    "  FAIL  {name}\n        {}",
                    why.replace('\n', "\n        ")
                );
            }
        }
    }
    println!(
        "\n{passed} passed, {skipped} skipped, {failures} failed of {} app(s)",
        results.len()
    );
    Ok(failures == 0)
}

// --- discovery -------------------------------------------------------------

struct App {
    name: String,
    dir: PathBuf,
    /// Why this app is kept but left out of the default run, from `app.json`'s
    /// `excluded` key. `nix/taintbench.nix` never fetches its APK, so the check
    /// reports it skipped with this reason. Passing `--apk <name>=<path>`
    /// explicitly still runs it.
    excluded: Option<String>,
}

/// The half of `app.json` this task reads. The rest (APK coordinates,
/// provenance) is for `nix/taintbench.nix`.
#[derive(Deserialize, Default)]
struct AppMeta {
    #[serde(default)]
    excluded: Option<String>,
}

fn resolve_apps_dir(override_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = override_dir {
        if dir.is_dir() {
            return Ok(dir.to_path_buf());
        }
        bail!("--apps-dir {} is not a directory", dir.display());
    }
    for candidate in ["taintbench/apps", "../taintbench/apps"] {
        let path = Path::new(candidate);
        if path.is_dir() {
            return Ok(path.to_path_buf());
        }
    }
    bail!("could not find a TaintBench apps directory (looked for `taintbench/apps`); pass --apps-dir")
}

/// An app dir is any subdirectory that has both a `findings.json` (ground truth)
/// and a `model.json` (ctadl query model). The app name is the directory name.
fn discover_apps(apps_dir: &Path) -> Result<Vec<App>> {
    let mut apps = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(apps_dir)
        .with_context(|| format!("failed to read {}", apps_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for dir in entries {
        if !dir.is_dir() {
            continue;
        }
        if dir.join("findings.json").is_file() && dir.join("model.json").is_file() {
            let name = dir
                .file_name()
                .and_then(|s| s.to_str())
                .context("app dir has no name")?
                .to_string();
            let meta: AppMeta = match std::fs::read_to_string(dir.join("app.json")) {
                Ok(text) => serde_json::from_str(&text)
                    .with_context(|| format!("failed to parse {}/app.json", dir.display()))?,
                Err(_) => AppMeta::default(),
            };
            apps.push(App {
                name,
                dir: std::fs::canonicalize(&dir)?,
                excluded: meta.excluded,
            });
        }
    }
    Ok(apps)
}

// --- ground truth / baseline ----------------------------------------------

#[derive(Deserialize)]
struct Findings {
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Finding {
    #[serde(rename = "ID")]
    id: i64,
    source: Endpoint,
    sink: Endpoint,
    #[serde(rename = "isNegative", default)]
    is_negative: bool,
}

#[derive(Deserialize)]
struct Endpoint {
    #[serde(rename = "className")]
    class_name: String,
    #[serde(rename = "lineNo")]
    line_no: i64,
    #[serde(rename = "targetName")]
    target_name: String,
    #[serde(rename = "IRs", default)]
    irs: Vec<Ir>,
}

#[derive(Deserialize)]
struct Ir {
    #[serde(rename = "IRstatement")]
    ir_statement: String,
}

impl Endpoint {
    /// The callee `(class, method)` this source/sink calls, parsed from the
    /// Jimple IR (`<Class: ret method(args)>`). Used to match against ctadl's
    /// reported source/sink callees.
    fn callee(&self) -> Option<Callee> {
        self.irs
            .iter()
            .find_map(|ir| parse_jimple_callee(&ir.ir_statement))
    }
}

#[derive(Deserialize, Default)]
struct Expected {
    #[serde(rename = "matched_finding_ids", default)]
    matched_finding_ids: Vec<i64>,
    /// `isNegative` findings ctadl reports today. Each is a call site TaintBench
    /// says is not a flow; ctadl claiming it is imprecision we have measured and
    /// accepted, not a reason to fail. A negative *outside* this list still
    /// fails the check, so no new false positive slips in unnoticed.
    #[serde(rename = "false_positive_finding_ids", default)]
    false_positive_finding_ids: Vec<i64>,
}

/// A callee identified by dotted class plus method name, ignoring the type
/// signature so e.g. `FileInputStream.<init>(String)` and `(File)` both match.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Callee {
    class: String,
    method: String,
}

impl std::fmt::Display for Callee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.class, self.method)
    }
}

// --- per-app run -----------------------------------------------------------

fn run_app(app: &App, apk: &Path) -> Result<Outcome> {
    let findings: Findings = read_json(&app.dir.join("findings.json"))?;
    let model = app.dir.join("model.json");
    let expected: Expected = match std::fs::read_to_string(app.dir.join("expected.json")) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}/expected.json", app.dir.display()))?,
        Err(_) => Expected::default(),
    };

    let work = scratch_dir(&app.name)?;
    let state = work.join("state");
    std::fs::create_dir_all(&state)?;

    let project = format!("{}_tb", app.name);
    let sarif = work.join("results.sarif");
    let apk_str = apk.to_string_lossy().into_owned();

    run_ctadl(
        &work,
        &state,
        &["import", "-l", "apk", "-n", &project, &apk_str],
    )?;
    // The model goes to both phases: `index` consumes its propagations (they
    // become function summaries) and `query` its sources and sinks. Each phase
    // warns about the part it ignores.
    run_ctadl(
        &work,
        &state,
        &["index", &project, "-m", &model.to_string_lossy()],
    )?;
    run_ctadl(
        &work,
        &state,
        &[
            "query",
            &project,
            "-m",
            &model.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
            "--sarif-profile",
            "agent",
        ],
    )?;

    let sarif_json: Value = read_json(&sarif)?;
    let detected_sources = collect_endpoint_callees(&sarif_json, TAINT_SOURCE_RULE_ID);
    let detected_sinks = collect_endpoint_callees(&sarif_json, TAINT_SINK_RULE_ID);
    let path_pairs = collect_path_pairs(&sarif_json);

    // Diagnostic dump so the baseline is easy to read off the check log.
    println!("\n== {} ==", app.name);
    dump_callees("reported taint sources", &detected_sources);
    dump_callees("reported taint sinks (reached by taint)", &detected_sinks);
    if path_pairs.is_empty() {
        println!("  connected source->sink paths: (none)");
    } else {
        println!("  connected source->sink paths:");
        for (s, k) in &path_pairs {
            println!("    {s}  ==>  {k}");
        }
    }

    // The source/sink callee pairs of the ground-truth *positive* flows. A
    // negative (isNegative) finding that shares its pair with some positive is
    // indistinguishable from it at callee granularity -- ctadl reporting that
    // pair means it found the positive, not the negative -- so such a negative
    // cannot fairly count as a false positive (see below).
    let positive_pairs: BTreeSet<(Callee, Callee)> = findings
        .findings
        .iter()
        .filter(|f| !f.is_negative)
        .filter_map(|f| Some((f.source.callee()?, f.sink.callee()?)))
        .collect();

    // Match each finding on the callee of its source AND its sink.
    let mut matched_positive: BTreeSet<i64> = BTreeSet::new();
    let mut matched_negative: BTreeSet<i64> = BTreeSet::new();
    println!("  findings:");
    for f in &findings.findings {
        let src = f.source.callee();
        let sink = f.sink.callee();
        let src_hit = src.as_ref().is_some_and(|c| detected_sources.contains(c));
        let sink_hit = sink.as_ref().is_some_and(|c| detected_sinks.contains(c));
        let pair = match (&src, &sink) {
            (Some(s), Some(k)) => Some((s.clone(), k.clone())),
            _ => None,
        };
        // A finding is detected when ctadl reports a connected source->sink path
        // whose endpoints' callees match this finding's source and sink. We do
        // NOT check that the path's intermediate steps mirror TaintBench's, only
        // that a connected flow exists between the same source and sink methods.
        let matched = pair.as_ref().is_some_and(|p| path_pairs.contains(p));
        // A negative whose callee pair is also a positive finding's pair is
        // "shadowed": we cannot tell ctadl's hit on it apart from the real
        // positive flow, so it is not counted as a false positive.
        let shadowed = f.is_negative && pair.as_ref().is_some_and(|p| positive_pairs.contains(p));
        let verdict = if !matched {
            "-"
        } else if f.is_negative {
            if shadowed {
                "MATCH(shadowed-by-positive)"
            } else {
                "FALSE-POSITIVE"
            }
        } else {
            "MATCH"
        };
        println!(
            "    #{:<2} {} [{}:{} {} -> {}:{} {}]  source:{}  sink:{}  path:{}  => {}",
            f.id,
            if f.is_negative { "NEG" } else { "pos" },
            f.source.class_name,
            f.source.line_no,
            callee_or_target(&src, &f.source.target_name),
            f.sink.class_name,
            f.sink.line_no,
            callee_or_target(&sink, &f.sink.target_name),
            if src_hit { "HIT" } else { "miss" },
            if sink_hit { "HIT" } else { "miss" },
            if matched { "yes" } else { "no" },
            verdict,
        );
        if matched {
            if f.is_negative {
                if !shadowed {
                    matched_negative.insert(f.id);
                }
            } else {
                matched_positive.insert(f.id);
            }
        }
    }

    // Compare against the baseline.
    let expected_ids: BTreeSet<i64> = expected.matched_finding_ids.iter().copied().collect();
    let regressions: Vec<i64> = expected_ids
        .difference(&matched_positive)
        .copied()
        .collect();
    let improvements: Vec<i64> = matched_positive
        .difference(&expected_ids)
        .copied()
        .collect();

    // Same treatment for the negatives: only a false positive we have not
    // already measured and written down is a failure.
    let expected_fps: BTreeSet<i64> = expected
        .false_positive_finding_ids
        .iter()
        .copied()
        .collect();
    let new_fps: Vec<i64> = matched_negative
        .difference(&expected_fps)
        .copied()
        .collect();
    let fixed_fps: Vec<i64> = expected_fps
        .difference(&matched_negative)
        .copied()
        .collect();

    let total_positive = findings.findings.iter().filter(|f| !f.is_negative).count();
    let summary = format!(
        "{}/{} ground-truth flows detected{}",
        matched_positive.len(),
        total_positive,
        if matched_negative.is_empty() {
            String::new()
        } else {
            format!(", {} known false positive(s)", matched_negative.len())
        }
    );

    if !new_fps.is_empty() {
        return Ok(Outcome::Fail(format!(
            "false positives: detected non-flow finding(s) {new_fps:?} (flagged isNegative) \
             that are not in the expected.json baseline"
        )));
    }
    if !regressions.is_empty() {
        return Ok(Outcome::Fail(format!(
            "regression: baseline finding(s) {regressions:?} no longer detected ({summary})"
        )));
    }
    let mut notes = Vec::new();
    if !improvements.is_empty() {
        notes.push(format!(
            "NEW finding(s) {improvements:?} detected -- set matched_finding_ids to {:?}",
            matched_positive.iter().collect::<Vec<_>>()
        ));
    }
    if !fixed_fps.is_empty() {
        notes.push(format!(
            "false positive(s) {fixed_fps:?} gone -- set false_positive_finding_ids to {:?}",
            matched_negative.iter().collect::<Vec<_>>()
        ));
    }
    if notes.is_empty() {
        return Ok(Outcome::Pass(summary));
    }
    Ok(Outcome::Pass(format!("{summary}; {}", notes.join("; "))))
}

fn callee_or_target(callee: &Option<Callee>, target: &str) -> String {
    callee
        .as_ref()
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("{target}(?)"))
}

fn dump_callees(label: &str, callees: &BTreeSet<Callee>) {
    if callees.is_empty() {
        println!("  {label}: (none)");
        return;
    }
    println!("  {label}:");
    for c in callees {
        println!("    {c}");
    }
}

// --- SARIF parsing ---------------------------------------------------------

fn results_with_rule<'a>(sarif: &'a Value, rule_id: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for run in sarif
        .get("runs")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        for res in run
            .get("results")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if res.get("ruleId").and_then(|v| v.as_str()) == Some(rule_id) {
                out.push(res);
            }
        }
    }
    out
}

/// The set of source/sink callees ctadl reported for `rule_id`. Each result's
/// logical location names the callee, either inline or by index into the run's
/// `logicalLocations` table.
fn collect_endpoint_callees(sarif: &Value, rule_id: &str) -> BTreeSet<Callee> {
    let mut out = BTreeSet::new();
    let run = sarif
        .get("runs")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    let table = run
        .and_then(|r| r.get("logicalLocations"))
        .and_then(|v| v.as_array());
    for res in results_with_rule(sarif, rule_id) {
        for loc in res
            .get("locations")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(fqn) = logical_location_fqn(loc, table) {
                if let Some(c) = parse_sarif_callee(&fqn) {
                    out.insert(c);
                }
            }
        }
    }
    out
}

/// The set of `(source callee, sink callee)` pairs of all reported tainted
/// paths -- a finding whose pair is here has a genuine connected flow.
fn collect_path_pairs(sarif: &Value) -> BTreeSet<(Callee, Callee)> {
    let mut out = BTreeSet::new();
    for res in results_with_rule(sarif, TAINTED_PATH_RULE_ID) {
        let props = res.get("properties");
        let src = props
            .and_then(|p| p.get("sourceCallee"))
            .and_then(|v| v.as_str())
            .and_then(parse_sarif_callee);
        let sink = props
            .and_then(|p| p.get("sinkCallee"))
            .and_then(|v| v.as_str())
            .and_then(parse_sarif_callee);
        if let (Some(s), Some(k)) = (src, sink) {
            out.insert((s, k));
        }
    }
    out
}

/// Resolve a location's logical-location fully-qualified name, following an
/// `index` into the run-level `logicalLocations` table when present.
fn logical_location_fqn(loc: &Value, table: Option<&Vec<Value>>) -> Option<String> {
    let ll = loc
        .get("logicalLocations")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())?;
    if let Some(name) = ll
        .get("fullyQualifiedName")
        .or_else(|| ll.get("name"))
        .and_then(|v| v.as_str())
    {
        return Some(name.to_string());
    }
    let idx = ll.get("index").and_then(|v| v.as_u64())? as usize;
    let entry = table?.get(idx)?;
    entry
        .get("fullyQualifiedName")
        .or_else(|| entry.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// --- callee parsing --------------------------------------------------------

/// Parse a ctadl/dex method FQN like
/// `Ljava/io/DataOutputStream;->write([BII)V` -> `java.io.DataOutputStream.write`.
fn parse_sarif_callee(fqn: &str) -> Option<Callee> {
    let (class_desc, rest) = fqn.split_once("->")?;
    let method = rest.split(['(', '<']).next().filter(|m| !m.is_empty());
    // `<init>` / `<clinit>` keep their angle brackets.
    let method = if rest.starts_with('<') {
        rest.split('(').next()
    } else {
        method
    }?;
    Some(Callee {
        class: normalize_class(class_desc),
        method: method.to_string(),
    })
}

/// Parse the callee of a Jimple invoke like
/// `... <android.content.ContentResolver: android.database.Cursor query(...)>(...)`
/// -> `android.content.ContentResolver.query`.
fn parse_jimple_callee(ir: &str) -> Option<Callee> {
    let start = ir.find('<')?;
    // The method ref is `<Class: ret method(args)>`; its body itself may contain
    // `<init>` / `<clinit>`, so the closing bracket is the LAST `>`, not the first.
    let end = ir.rfind('>').filter(|&e| e > start)?;
    let inner = &ir[start + 1..end]; // `Class: ret method(args)`
    let (class, rest) = inner.split_once(':')?;
    // `rest` is ` ret method(args)`; the method name is the token before `(`.
    let before_paren = rest.split('(').next()?.trim();
    let method = before_paren.rsplit(char::is_whitespace).next()?;
    if method.is_empty() {
        return None;
    }
    Some(Callee {
        class: normalize_class(class.trim()),
        method: method.to_string(),
    })
}

/// Canonicalize a class name for matching: accept a JVM descriptor
/// (`Lcom/foo/Bar;`) or an already-dotted name, and collapse nested-class `$`
/// to `.` so TaintBench's `Foo.Inner` matches the dex's `Foo$Inner`.
fn normalize_class(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('L').unwrap_or(s);
    let s = s.strip_suffix(';').unwrap_or(s);
    s.replace(['/', '$'], ".")
}

// --- shared ----------------------------------------------------------------

fn run_ctadl(work: &Path, state: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("ctadl");
    cmd.current_dir(work)
        .env("XDG_STATE_HOME", state)
        .args(args);
    exec::run_checked(
        cmd,
        &format!("ctadl {}", args.first().copied().unwrap_or("")),
    )?;
    Ok(())
}

fn scratch_dir(name: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("ctadl_taintbench_{name}"));
    exec::fresh_dir(&dir)?;
    Ok(dir)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse JSON {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callee(class: &str, method: &str) -> Callee {
        Callee {
            class: class.to_string(),
            method: method.to_string(),
        }
    }

    #[test]
    fn normalize_class_forms() {
        assert_eq!(
            normalize_class("Lcom/beita/contact/MyContacts;"),
            "com.beita.contact.MyContacts"
        );
        assert_eq!(
            normalize_class("android.content.ContentResolver"),
            "android.content.ContentResolver"
        );
        assert_eq!(normalize_class("Lcom/a/B$Inner;"), "com.a.B.Inner");
    }

    #[test]
    fn parse_sarif_callee_forms() {
        assert_eq!(
            parse_sarif_callee("Landroid/content/ContentResolver;->query(Landroid/net/Uri;)Landroid/database/Cursor;"),
            Some(callee("android.content.ContentResolver", "query"))
        );
        assert_eq!(
            parse_sarif_callee("Ljava/io/FileInputStream;-><init>(Ljava/lang/String;)V"),
            Some(callee("java.io.FileInputStream", "<init>"))
        );
        assert_eq!(
            parse_sarif_callee("Ljava/io/DataOutputStream;->write([BII)V"),
            Some(callee("java.io.DataOutputStream", "write"))
        );
        assert_eq!(parse_sarif_callee("not a method"), None);
    }

    #[test]
    fn parse_jimple_callee_forms() {
        assert_eq!(
            parse_jimple_callee(
                "$r5 = virtualinvoke $r3.<android.content.ContentResolver: android.database.Cursor query(android.net.Uri,java.lang.String[])>($r4, null)"
            ),
            Some(callee("android.content.ContentResolver", "query"))
        );
        assert_eq!(
            parse_jimple_callee(
                "specialinvoke $r8.<java.io.FileInputStream: void <init>(java.lang.String)>($r4)"
            ),
            Some(callee("java.io.FileInputStream", "<init>"))
        );
        assert_eq!(
            parse_jimple_callee(
                "virtualinvoke $r10.<javax.mail.Transport: void sendMessage(javax.mail.Message,javax.mail.Address[])>($r5, $r11)"
            ),
            Some(callee("javax.mail.Transport", "sendMessage"))
        );
    }

    #[test]
    fn jimple_and_sarif_agree() {
        let from_ir = parse_jimple_callee(
            "virtualinvoke $r2.<java.io.BufferedWriter: void write(java.lang.String)>($r0)",
        );
        let from_sarif = parse_sarif_callee("Ljava/io/BufferedWriter;->write(Ljava/lang/String;)V");
        assert_eq!(from_ir, from_sarif);
        assert_eq!(from_ir, Some(callee("java.io.BufferedWriter", "write")));
    }
}
