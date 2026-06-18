//! Orchestration of the regression suite: drive the analyzer per case and check
//! its output against the case's known answer.
//!
//! Each case runs in its own scratch directory so that ctadl project state
//! (kept under `XDG_STATE_HOME`) and build artifacts never collide between
//! cases. Unlike the old `set -e` scripts, a single failing case does not abort
//! the run: we execute every selected case and report them all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::assertions;
use crate::dex;
use crate::discovery::{self, Kind, TestCase};
use crate::exec;
use crate::jvm;

/// Ghidra's pcode facts place the binary at this base address; subtract it to
/// recover an offset usable with `addr2line` on the object file.
const PCODE_BASE_ADDRESS: i64 = 0x10_0000;

#[derive(Default)]
pub struct Options {
    pub filter: Option<String>,
    pub tests_dir: Option<PathBuf>,
    pub jvm_samples: Option<PathBuf>,
    pub dex_apk: Option<PathBuf>,
}

pub(crate) enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

/// Run the suite. Returns `Ok(true)` if every selected case passed or was
/// skipped, `Ok(false)` if any failed.
pub fn run(opts: &Options) -> Result<bool> {
    let tests_dir = discovery::resolve_tests_dir(opts.tests_dir.as_deref())?;

    let mut cases = discovery::discover(&tests_dir)?;
    if let Some(filter) = &opts.filter {
        cases.retain(|c| c.name.contains(filter));
    }

    // ctadl/dex-reader are only needed for the DEX/pcode cases; don't require
    // them when (say) `--filter jvm` selects only the jvm-reader checks.
    if !cases.is_empty() {
        preflight()?;
    }

    let mut results: Vec<(String, Outcome)> = Vec::new();
    for case in &cases {
        // Infrastructure failures (analyzer crashed, tool missing, etc.) are
        // reported as a failed case rather than aborting the whole run.
        let outcome = run_case(case).unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")));
        results.push((case.name.clone(), outcome));
    }

    // The jvm-reader and dex-reader checks share the same Java sample sources:
    // jvm-reader exercises the `.class`/`.jar`, dex-reader the `.dex` they
    // compile down to.
    let samples_dir = resolve_jvm_samples(opts.jvm_samples.as_deref());

    // jvm-reader checks: compile the sample .java and exercise jvm-reader on the
    // resulting .class files and a jar built from them.
    if let Some(samples_dir) = &samples_dir {
        let work = scratch_dir("jvm")?;
        let mut jvm_results = jvm::run_checks(samples_dir, &work)
            .unwrap_or_else(|err| vec![("jvm".to_string(), Outcome::Fail(format!("{err:#}")))]);
        if let Some(filter) = &opts.filter {
            jvm_results.retain(|(name, _)| name.contains(filter));
        }
        results.extend(jvm_results);
    }

    // dex-reader checks: compile the same samples down to .dex and parse them,
    // plus a real-world APK that xtask owns.
    if let Some(samples_dir) = &samples_dir {
        let work = scratch_dir("dex")?;
        let apk = resolve_dex_apk(opts.dex_apk.as_deref());
        let mut dex_results = dex::run_checks(samples_dir, apk.as_deref(), &work)
            .unwrap_or_else(|err| vec![("dex".to_string(), Outcome::Fail(format!("{err:#}")))]);
        if let Some(filter) = &opts.filter {
            dex_results.retain(|(name, _)| name.contains(filter));
        }
        results.extend(dex_results);
    }

    if results.is_empty() {
        bail!("no test cases selected");
    }

    println!(
        "Ran {} regression case(s) (tests from {})",
        results.len(),
        tests_dir.display()
    );

    let (mut passed, mut skipped, mut failures) = (0, 0, 0);
    for (name, outcome) in &results {
        match outcome {
            Outcome::Pass => {
                passed += 1;
                println!("  PASS  {name}");
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
        "\n{passed} passed, {skipped} skipped, {failures} failed of {} case(s)",
        results.len()
    );
    Ok(failures == 0)
}

/// Locate the jvm-reader sample sources. With no override, look where the crate
/// lives relative to the repo root (`cargo xtask`) or the nightly cwd.
fn resolve_jvm_samples(override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return Some(dir.to_path_buf());
    }
    ["jvm-reader/tests/sample", "../jvm-reader/tests/sample"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
}

/// Locate the real-world APK xtask owns for the dex-reader smoke test. With no
/// override, look where it lives relative to the repo root or the nightly cwd.
fn resolve_dex_apk(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return Some(path.to_path_buf());
    }
    [
        "xtask/tests/dex/com.noto_54.apk",
        "../xtask/tests/dex/com.noto_54.apk",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Ensure the executables the suite always needs are on `PATH`, failing early
/// with a clear message (the old `tests.sh` did the same for ctadl/dex-reader).
fn preflight() -> Result<()> {
    for tool in ["ctadl", "dex-reader"] {
        if exec::which(tool).is_none() {
            bail!("`{tool}` not found on PATH");
        }
    }
    Ok(())
}

fn run_case(case: &TestCase) -> Result<Outcome> {
    match &case.kind {
        Kind::Dex { java, config } => run_dex(&case.name, java, config),
        Kind::Pcode { source, query } => run_pcode(&case.name, source, query),
    }
}

// --- DEX / Java -----------------------------------------------------------

fn run_dex(name: &str, java: &Path, config: &Path) -> Result<Outcome> {
    let work = scratch_dir(name)?;
    let state = work.join("state");
    std::fs::create_dir_all(&state)?;

    // Compile to a DEX inside the scratch dir. We copy only this source so the
    // `*.class` glob handed to `dx` cannot pick up unrelated classes.
    let src = work.join(format!("{name}.java"));
    std::fs::copy(java, &src)
        .with_context(|| format!("failed to copy {} into scratch dir", java.display()))?;

    let mut javac = Command::new("javac");
    javac.current_dir(&work).args(["--release", "8"]).arg(&src);
    exec::run_checked(javac, "javac")?;

    let classes = class_files(&work)?;
    if classes.is_empty() {
        bail!("javac produced no .class files");
    }
    let dex = work.join(format!("{name}.dex"));
    let mut dx = Command::new("dx");
    dx.current_dir(&work)
        .arg("--dex")
        .arg(format!("--output={}", dex.display()));
    // `dx` derives the package path from the class file path, so pass bare file
    // names (relative to the scratch cwd) rather than absolute paths.
    for class in &classes {
        dx.arg(class.file_name().context("class file has no name")?);
    }
    exec::run_checked(dx, "dx")?;

    // Analyze: import / index / query / format.
    let project = format!("{name}_test");
    let sarif = work.join(format!("{name}_output.sarif"));
    run_ctadl(
        &work,
        &state,
        &["import", "--name", &project, &dex_arg(&dex)],
    )?;
    run_ctadl(&work, &state, &["index", &project])?;
    run_ctadl(
        &work,
        &state,
        &["query", &project, "-m", &config.to_string_lossy()],
    )?;
    run_ctadl(
        &work,
        &state,
        &["format", &project, "-o", &sarif.to_string_lossy()],
    )?;

    // Build the offset -> line map.
    let linemap = work.join(format!("{name}_linemap.json"));
    let mut reader = Command::new("dex-reader");
    reader
        .current_dir(&work)
        .arg(&dex)
        .arg("--linemap-json")
        .arg(&linemap);
    exec::run_checked(reader, "dex-reader")?;

    let expected = assertions::read_expected_lines(config)?;
    let offsets = assertions::collect_byte_offsets(&sarif)?;

    // Negative test: no flow expected.
    if expected.is_empty() {
        return Ok(if offsets.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail(format!(
                "expected no flows, but found byte offsets {offsets:?}"
            ))
        });
    }

    if offsets.is_empty() {
        return Ok(Outcome::Fail("no byte offsets in SARIF output".to_string()));
    }

    let entries = assertions::load_linemap(&linemap)?;
    let mapped: BTreeSet<i64> = offsets
        .iter()
        .filter_map(|&off| assertions::map_offset_to_line(&entries, off))
        .collect();

    // PASS if at least one expected line is present (the DEX criterion).
    if expected.iter().any(|line| mapped.contains(line)) {
        Ok(Outcome::Pass)
    } else {
        Ok(Outcome::Fail(format!(
            "none of the expected lines {expected:?} appear in mapped lines {mapped:?}"
        )))
    }
}

fn class_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut classes: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("class"))
        .collect();
    classes.sort();
    Ok(classes)
}

fn dex_arg(dex: &Path) -> String {
    dex.to_string_lossy().into_owned()
}

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

// --- Pcode / C ------------------------------------------------------------

fn run_pcode(name: &str, source: &Path, query: &Path) -> Result<Outcome> {
    let work = scratch_dir(name)?;
    let state = work.join("state");
    let outdir = work.join("test-output");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&outdir)?;

    let (cc, addr2line) = pick_toolchain();

    // Compile to a relocatable object with debug info.
    let obj = outdir.join(format!("{name}.o"));
    let mut compile = Command::new(&cc);
    compile
        .current_dir(&work)
        .args(["-g", "-O0", "-c"])
        .arg(source)
        .arg("-o")
        .arg(&obj);
    exec::run_checked(compile, &cc)?;

    // Ghidra (used by the pcode importer) needs a writable HOME.
    let home = ensure_writable_home()?;
    let java_tool_options = format!("-Duser.home={}", home.display());

    let project = format!("{name}_pcode");
    let sarif = outdir.join(format!("{name}_results.sarif"));
    let obj_str = obj.to_string_lossy().into_owned();
    let pcode_env = pcode_env(&home, &java_tool_options);

    run_ctadl_env(
        &work,
        &state,
        &pcode_env,
        &["import", "-l", "pcode", &obj_str, "-n", &project],
    )?;
    run_ctadl_env(&work, &state, &pcode_env, &["index", &project])?;
    run_ctadl_env(
        &work,
        &state,
        &pcode_env,
        &["query", &project, "-m", &query.to_string_lossy()],
    )?;
    run_ctadl_env(
        &work,
        &state,
        &pcode_env,
        &["format", &project, "-o", &sarif.to_string_lossy()],
    )?;

    let addresses = assertions::collect_absolute_addresses(&sarif)?;
    if addresses.is_empty() {
        if cfg!(target_os = "macos") {
            return Ok(Outcome::Skip(
                "no tainted instructions on Darwin; skipping strict offset check".to_string(),
            ));
        }
        return Ok(Outcome::Fail(
            "no tainted instructions in SARIF output".to_string(),
        ));
    }

    // Map each tainted address back to a source line via addr2line.
    let mut found: BTreeSet<i64> = BTreeSet::new();
    for addr in &addresses {
        let rel = format!("0x{:x}", addr - PCODE_BASE_ADDRESS);
        let mut cmd = Command::new(&addr2line);
        cmd.current_dir(&work).arg("-e").arg(&obj).arg(&rel);
        let out = exec::capture_stdout(cmd, &addr2line)?;
        if let Some(line) = parse_addr2line_line(&out) {
            found.insert(line);
        }
    }

    let expected = assertions::read_expected_lines(query)?;
    let missing: Vec<i64> = expected
        .iter()
        .copied()
        .filter(|l| !found.contains(l))
        .collect();

    // PASS only if every expected line was found (the pcode criterion).
    if missing.is_empty() {
        Ok(Outcome::Pass)
    } else {
        Ok(Outcome::Fail(format!(
            "expected lines {missing:?} not found among {found:?}"
        )))
    }
}

/// Parse the line number from an `addr2line` `file:line` result, ignoring any
/// trailing ` (discriminator N)` and unknown `??:?` output.
fn parse_addr2line_line(output: &str) -> Option<i64> {
    let first = output.lines().next()?;
    let after_colon = first.rsplit_once(':')?.1;
    let digits: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Mirror the old script's compiler/addr2line selection: prefer an x86_64 Linux
/// (cross-)compiler, fall back to the native tools.
fn pick_toolchain() -> (String, String) {
    for prefix in ["x86_64-unknown-linux-gnu-", "x86_64-linux-gnu-"] {
        if exec::which(&format!("{prefix}gcc")).is_some() {
            return (format!("{prefix}gcc"), format!("{prefix}addr2line"));
        }
    }
    if !cfg!(target_os = "linux") {
        eprintln!(
            "warning: no x86_64 Linux cross-compiler found and not on Linux; falling back to native gcc"
        );
    }
    ("gcc".to_string(), "addr2line".to_string())
}

fn pcode_env(home: &Path, java_tool_options: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("HOME".to_string(), home.display().to_string()),
        (
            "JAVA_TOOL_OPTIONS".to_string(),
            java_tool_options.to_string(),
        ),
    ];
    // Guess JAVA_HOME from `java` if the environment did not set it.
    if std::env::var_os("JAVA_HOME").is_none() {
        if let Some(java_home) = guess_java_home() {
            env.push(("JAVA_HOME".to_string(), java_home.display().to_string()));
        }
    }
    env
}

fn guess_java_home() -> Option<PathBuf> {
    let java = exec::which("java")?;
    let real = std::fs::canonicalize(java).ok()?;
    // <java_home>/bin/java -> <java_home>
    Some(real.parent()?.parent()?.to_path_buf())
}

/// Return a writable HOME, creating a temp dir if the current one is unset,
/// `/var/empty`, or not writable (Nix sandboxes often set HOME=/var/empty).
fn ensure_writable_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(&home);
        if path != Path::new("/var/empty") && is_writable(&path) {
            return Ok(path);
        }
    }
    let dir = std::env::temp_dir().join(format!("ctadl_xtask_home_{}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".ctadl_xtask_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn run_ctadl_env(work: &Path, state: &Path, env: &[(String, String)], args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("ctadl");
    cmd.current_dir(work)
        .env("XDG_STATE_HOME", state)
        .args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    exec::run_checked(
        cmd,
        &format!("ctadl {}", args.first().copied().unwrap_or("")),
    )?;
    Ok(())
}

// --- shared ----------------------------------------------------------------

fn scratch_dir(name: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("ctadl_xtask_{name}"));
    exec::fresh_dir(&dir)?;
    Ok(dir)
}
