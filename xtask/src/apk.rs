//! End-to-end checks driving `ctadl` over a real-world Android app.
//!
//! These used to be `#[test]`s in `ctadl-ascent/tests/cli.rs`, where they ran on every `cargo
//! test`. Four of them imported `xtask/tests/dex/com.noto_54.apk` -- a 6.4 MB app carrying two
//! `classes*.dex` and some 50,000 functions -- and each paid the full ~13 s import to do it.
//! Between them that was ~60 s of the ~77 s every test in the workspace spent executing, and
//! taking them out halved `cargo test --workspace` end to end (33 s to 16 s on the machine this
//! was measured on). That is a lot to charge every contributor for four cases that are not unit
//! tests at all: they import a real artifact through the real pipeline and read the result back
//! out of the store, which is the definition of end to end, and end-to-end work belongs in the
//! nightly suite.
//!
//! Moving them here changes what is actually asserted, for the better. The old tests called
//! `ctadl_ascent::cli::import` and friends as a library, so nothing between `main.rs` and the
//! library was covered -- `--skip-existing`, for one, is decided entirely in `main.rs` and had no
//! test at all. These drive the shipped `ctadl` binary and assert against the store it writes and
//! the SARIF it emits, so the argument wiring, the exit status, and the on-disk layout are all in
//! scope.
//!
//! They also cost less than they did. The import is the expensive part and every check needs the
//! same one, so it is done once and the checks read the same store, rather than each case
//! importing the app for itself.
//!
//! Nothing here needs a toolchain: the APK is prebuilt and checked in, so unlike the `dex:*` and
//! `jvm:*` checks there is no javac, no `dx`, and no Ghidra in the loop. All it needs is `ctadl`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use crate::exec;
use crate::regression::{ctadl_bin, Outcome};

/// The import name every check shares. One import, read by all of them.
const IMPORT: &str = "app";

/// Every check this module reports, in report order.
///
/// Named up front rather than derived from what [`run_checks`] returns, because the selection
/// has to be known *before* anything runs: `--filter` decides whether `ctadl` is built at all,
/// and building it is the expensive preflight step these checks depend on.
pub const CHECKS: &[&str] = &[
    "apk:import",
    "apk:no-native-libs",
    "apk:model-check",
    "apk:skip-existing",
];

// The store layout `ctadl` writes. Duplicated from `ctadl_ascent::project` rather than imported:
// xtask deliberately does not depend on the analyzer crate (see `xtask/Cargo.toml`), and these
// checks are *about* the on-disk contract anyway -- a path that moves should fail them loudly
// here rather than follow the analyzer silently.
const IMPORTS_DIR: &str = "imports";
const PROJECTS_DIR: &str = "projects";
const IMPORT_CONFIG_FILE: &str = "import_config.json";
const PROGRAM_BITCODE_FILE: &str = "ir-program.bitcode";
/// The `version` an import config carries today (`IMPORT_FORMAT_VERSION`). Pinned so a bump
/// that forgets the store's readers has to come through here.
const IMPORT_FORMAT_VERSION: &str = "6";

/// A model file that selects something in any Java app: every `toString` override. The point is
/// the *checking*, not the model, so the cheapest generator that cannot match nothing is the
/// right one.
const MODELS: &str = r#"{"model_generators": [
  {"find": "methods",
   "where": [{"constraint": "signature_match", "name": "toString"}],
   "model": {"sources": [{"kind": "k", "port": "Return"}]}}
]}"#;

/// Import the app once, then run every check against that store. Returns named (check, outcome)
/// pairs to fold into the regression report.
///
/// The import is shared, so a failure in it is not one failed check -- there is nothing for the
/// others to look at. It is reported as `apk:import` failing and the rest are Skipped naming it,
/// which reads as what happened rather than as four independent breakages.
pub fn run_checks(apk: &Path, work: &Path) -> Result<Vec<(String, Outcome)>> {
    let state = work.join("state");
    std::fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
    let store = state.join("ctadl");

    let apk = std::fs::canonicalize(apk)
        .with_context(|| format!("failed to canonicalize {}", apk.display()))?;

    if let Err(err) = import(work, &state, &apk, &[]) {
        let mut results = vec![("apk:import".to_string(), Outcome::Fail(format!("{err:#}")))];
        results.extend(CHECKS.iter().skip(1).map(|name| {
            (
                (*name).to_string(),
                Outcome::Skip("the shared import failed; see apk:import".to_string()),
            )
        }));
        return Ok(results);
    }

    // Positional, and in the order [`CHECKS`] names them -- the checks share a store, so the
    // order is part of the arrangement rather than a presentation choice. `apk:model-check` runs
    // before `apk:skip-existing` because it wants the store exactly as the first import left it,
    // and `apk:skip-existing` re-imports.
    let outcomes = [
        to_outcome(check_import(work, &state, &store, &apk)),
        to_outcome(check_no_native_libs(&store)),
        to_outcome(check_model_check(work, &state, &store)),
        to_outcome(check_skip_existing(work, &state, &store, &apk)),
    ];
    Ok(CHECKS
        .iter()
        .map(|name| (*name).to_string())
        .zip(outcomes)
        .collect())
}

fn to_outcome(result: Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Pass,
        Err(err) => Outcome::Fail(format!("{err:#}")),
    }
}

// --- the checks -----------------------------------------------------------

/// The app imports, and what lands in the store is a program `ctadl` can read back.
///
/// Reading it back is the substance. That the import command exited zero says only that it did
/// not crash; `ctadl inspect` decodes the stored bitcode and reports what is in it, so a
/// truncated or wrongly-encoded program fails here rather than at the next command that needs it.
fn check_import(work: &Path, state: &Path, store: &Path, apk: &Path) -> Result<()> {
    let config = read_config(store)?;
    ensure!(
        config["language"] == "Apk",
        "import config records language {}, expected \"Apk\"",
        config["language"]
    );
    ensure!(
        config["version"] == IMPORT_FORMAT_VERSION,
        "import config records format version {}, expected {IMPORT_FORMAT_VERSION:?}; \
         if the format really changed, update IMPORT_FORMAT_VERSION here",
        config["version"]
    );
    ensure!(
        config["artifact_path"].as_str() == apk.to_str(),
        "import config records artifact {} rather than the APK it was given, {}",
        config["artifact_path"],
        apk.display()
    );
    // Recorded by `main.rs` after a successful import, and what `--skip-existing` reads.
    // `apk:skip-existing` pins what it is *for*; this pins that it is written at all.
    ensure!(
        config["hash"].as_str().is_some_and(|h| !h.is_empty()),
        "import config records no artifact hash: {}",
        config["hash"]
    );

    let program = program_path(store);
    let size = std::fs::metadata(&program)
        .with_context(|| format!("stat {}", program.display()))?
        .len();
    ensure!(size > 0, "{} is empty", program.display());

    // Decode it: `inspect` loads the stored program and reports its statistics.
    let report = capture(work, state, &["inspect", IMPORT])?;
    let functions = report
        .lines()
        .find_map(|line| line.trim().strip_prefix("Number of functions:"))
        .map(str::trim)
        .with_context(|| format!("`ctadl inspect {IMPORT}` reported no function count:\n{report}"))?
        .parse::<u64>()
        .with_context(|| format!("unparseable function count in:\n{report}"))?;
    ensure!(
        functions > 0,
        "the imported program has no functions:\n{report}"
    );
    Ok(())
}

/// This APK ships no `lib/<abi>` entries, so the native-library pass is a no-op: it records no
/// sub-imports and stages nothing.
///
/// This is the path every APK without native code takes, and the one that must not need Ghidra.
/// The `Jni:*+apk` cases cover the other path, where there *are* libraries to find.
fn check_no_native_libs(store: &Path) -> Result<()> {
    let config = read_config(store)?;
    let subs = config["sub_imports"]
        .as_array()
        .context("import config has no `sub_imports` array")?;
    ensure!(
        subs.is_empty(),
        "an APK with no native libraries records no sub-imports, got {subs:?}"
    );
    // Nothing was extracted, so the staging directory was never created.
    let staged = import_dir(store).join("native");
    ensure!(
        !staged.exists(),
        "nothing was extracted, so {} should not exist",
        staged.display()
    );
    Ok(())
}

/// `ctadl query` against an import that was never indexed reports what the model files select,
/// and writes nothing into the store.
///
/// Two halves, and both matter. The report has to name the imports it checked and say the
/// generator matched something -- a check that silently matches nothing is worse than no check,
/// because it reads as a clean bill of health. And the command must leave the store alone: it
/// could not run a query, so a project written here would be an empty index that the next real
/// `ctadl query` would happily use.
///
/// The synthetic-program half of this -- what `check_programs` decides, given programs and no
/// store -- stays in `ctadl-ascent/tests/model_check.rs`, which needs no artifact and runs in
/// milliseconds. What is here is the half that needs a real import.
fn check_model_check(work: &Path, state: &Path, store: &Path) -> Result<()> {
    let models = work.join("models.json");
    std::fs::write(&models, MODELS).with_context(|| format!("writing {}", models.display()))?;
    let sarif = work.join("model-check.sarif");

    // Deliberately not `run_checked`: a query with no index exits non-zero, and that is the
    // contract -- it could not answer the question it was asked. The report it wrote on the way
    // out is what is under test.
    let output = command(
        work,
        state,
        &[
            "query",
            IMPORT,
            "-m",
            &models.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
    )?
    .output()
    .context("failed to spawn `ctadl query`")?;
    ensure!(
        !output.status.success(),
        "`ctadl query` with no index exited 0; it cannot have run a query, and the non-zero \
         exit is what tells a caller so"
    );

    let text = std::fs::read_to_string(&sarif).with_context(|| {
        format!(
            "`ctadl query` wrote no report at {}\n--- stderr ---\n{}",
            sarif.display(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )
    })?;
    let doc: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", sarif.display()))?;
    let notifications = &doc["runs"][0]["invocations"][0]["toolConfigurationNotifications"];
    let notifications = notifications
        .as_array()
        .with_context(|| format!("no toolConfigurationNotifications in {}", sarif.display()))?;
    let by_id = |id: &str| -> Option<&Value> {
        notifications
            .iter()
            .find(|n| n["descriptor"]["id"].as_str() == Some(id))
    };

    // Which imports were checked. Naming the import names everything imported out of it, so for
    // an APK this is the app plus its native libraries -- of which this one has none.
    let checked = by_id("CTADL0008.no-index-model-check-only")
        .with_context(|| format!("no `no-index-model-check-only` notification in:\n{text}"))?;
    let imports: Vec<&str> = checked["properties"]["imports"]
        .as_array()
        .context("the notification lists no imports")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    ensure!(
        imports == [IMPORT],
        "the check ran over {imports:?}, expected [{IMPORT:?}]"
    );
    ensure!(
        checked["properties"]["functions"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "the check reports no functions in the import: {}",
        checked["properties"]
    );

    // And that the generator selected something. This one matches `toString`, which any real
    // Java app has hundreds of.
    let matched = by_id("CTADL0011.generator-matched")
        .with_context(|| format!("the generator matched nothing:\n{text}"))?;
    ensure!(
        matched["level"] == "note",
        "a matched generator is a note, not {}",
        matched["level"]
    );

    // Nothing ran, so nothing may be recorded.
    let project = store.join(PROJECTS_DIR).join(IMPORT);
    ensure!(
        !project.exists(),
        "the model check wrote a project config: {}",
        project.display()
    );
    Ok(())
}

/// `--skip-existing` skips a re-import of an unchanged artifact, and only of an unchanged one.
///
/// Both halves are asserted, because either alone is satisfied by a bug. A flag that always
/// skips passes the first; a flag that never skips passes the second. What distinguishes them is
/// the recorded content hash, so the negative half is produced by falsifying exactly that: the
/// artifact and its path are untouched and only the stored hash is wrong, which is the state a
/// changed artifact leaves behind.
///
/// The observable is the program bitcode's modification time. A skipped import does no work, so
/// it cannot rewrite it; a performed import always does.
fn check_skip_existing(work: &Path, state: &Path, store: &Path, apk: &Path) -> Result<()> {
    let program = program_path(store);
    let before = modified(&program)?;

    import(work, state, apk, &["--skip-existing"])?;
    ensure!(
        modified(&program)? == before,
        "a --skip-existing re-import of an unchanged artifact rewrote {}",
        program.display()
    );

    // Falsify the recorded hash, as a changed artifact would.
    let path = config_path(store);
    let mut config = read_config(store)?;
    let real_hash = config["hash"].clone();
    config["hash"] = Value::String("0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&config)?)
        .with_context(|| format!("writing {}", path.display()))?;

    import(work, state, apk, &["--skip-existing"])?;
    ensure!(
        modified(&program)? != before,
        "a --skip-existing re-import skipped an artifact whose recorded hash does not match, \
         leaving {} untouched",
        program.display()
    );
    // And the import it performed recorded the true hash again, so the next one can skip.
    ensure!(
        read_config(store)?["hash"] == real_hash,
        "the re-import did not record the artifact's hash"
    );
    Ok(())
}

// --- helpers --------------------------------------------------------------

/// Import the app under [`IMPORT`], with `extra` appended to the command.
fn import(work: &Path, state: &Path, apk: &Path, extra: &[&str]) -> Result<()> {
    let mut args = vec!["import", "-l", "apk", "--name", IMPORT];
    args.extend_from_slice(extra);
    let apk = apk.to_string_lossy();
    args.push(&apk);
    exec::run_checked(command(work, state, &args)?, "ctadl import")?;
    Ok(())
}

/// A `ctadl` invocation against the scratch store. `XDG_STATE_HOME` rather than `--store`, so
/// the default store resolution is exercised too -- it is what a user gets.
fn command(work: &Path, state: &Path, args: &[&str]) -> Result<Command> {
    let mut cmd = Command::new(ctadl_bin()?);
    cmd.current_dir(work)
        .env("XDG_STATE_HOME", state)
        .args(args);
    Ok(cmd)
}

fn capture(work: &Path, state: &Path, args: &[&str]) -> Result<String> {
    exec::capture_stdout(
        command(work, state, args)?,
        &format!("ctadl {}", args.first().copied().unwrap_or_default()),
    )
}

fn import_dir(store: &Path) -> PathBuf {
    store.join(IMPORTS_DIR).join(IMPORT)
}

fn config_path(store: &Path) -> PathBuf {
    import_dir(store).join(IMPORT_CONFIG_FILE)
}

fn program_path(store: &Path) -> PathBuf {
    import_dir(store).join(PROGRAM_BITCODE_FILE)
}

fn read_config(store: &Path) -> Result<Value> {
    let path = config_path(store);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn modified(path: &Path) -> Result<SystemTime> {
    std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .modified()
        .with_context(|| format!("no modification time for {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`run_checks`] fills [`CHECKS`] positionally, so a check added to one and not the other
    /// would silently drop a result or mislabel one. The count is what a compiler cannot catch.
    #[test]
    fn every_check_is_named() {
        assert_eq!(CHECKS.len(), 4, "CHECKS and run_checks must stay in step");
        assert_eq!(CHECKS[0], "apk:import", "the shared import reports first");
        assert!(
            CHECKS.iter().all(|n| n.starts_with("apk:")),
            "the family prefix is what --filter selects on: {CHECKS:?}"
        );
    }

    /// The model file the model check is run with has to be a model file. It is written from a
    /// string constant, so nothing else would catch a typo in it until the nightly ran.
    #[test]
    fn the_model_file_is_valid_json() {
        let value: Value = serde_json::from_str(MODELS).expect("MODELS parses");
        assert_eq!(
            value["model_generators"].as_array().map(Vec::len),
            Some(1),
            "one generator, which the check expects to match"
        );
    }
}
