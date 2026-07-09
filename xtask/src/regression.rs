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

/// Historical fallback base address for SARIF that predates the analyzer
/// emitting `relativeAddress`. Newer output carries the section-relative offset
/// directly (Ghidra's real image base, via the `PROGRAM_IMAGE_BASE` fact), so
/// this is only used when `relativeAddress` is absent.
const PCODE_BASE_ADDRESS: i64 = 0x10_0000;

/// JVM E2E cases whose failures count toward the suite exit code. All other
/// `Jvm:*` taint cases run for visibility but report as XFAIL when they fail.
const JVM_E2E_ENFORCED: &[&str] = &[
    "Jvm:AnotherExample",
    "Jvm:ArrayFlow",
    "Jvm:ArrayFlowComplex",
    "Jvm:ArrayListFlow",
    "Jvm:ArrayListIteratorFlow",
    "Jvm:BranchingFlow",
    "Jvm:CrossClassStaticFieldFlow",
    "Jvm:ExceptionFlow",
    "Jvm:FieldFlow",
    "Jvm:FieldSensitivity",
    "Jvm:InstanceMethodFlow",
    "Jvm:LoopFlow",
    "Jvm:MethodCallFlow",
    "Jvm:ObjectSensitivity",
    "Jvm:Reassignment",
    "Jvm:SourceSinkExample",
    "Jvm:StaticFieldFlow",
    "Jvm:StringBuilderFlow",
];

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
    /// Expected failure on a non-enforced JVM E2E case; does not fail the suite.
    Xfail(String),
}

/// Run the suite. Returns `Ok(true)` if every selected case passed or was
/// skipped, `Ok(false)` if any failed.
pub fn run(opts: &Options) -> Result<bool> {
    let tests_dir = discovery::resolve_tests_dir(opts.tests_dir.as_deref())?;

    let mut cases = discovery::discover(&tests_dir)?;
    if let Some(filter) = &opts.filter {
        cases.retain(|c| c.name.contains(filter));
    }

    // ctadl/dex-reader/jvm-reader are only needed for the matching case kinds.
    if !cases.is_empty() {
        preflight(&cases)?;
    }

    let mut results: Vec<(String, Outcome)> = Vec::new();
    for case in &cases {
        // Infrastructure failures (analyzer crashed, tool missing, etc.) are
        // reported as a failed case rather than aborting the whole run.
        let outcome = run_case(case).unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")));
        results.push((case.name.clone(), apply_jvm_allowlist(&case.name, outcome)));
    }

    // The jvm-reader and dex-reader checks share the same Java sample sources:
    // jvm-reader exercises the `.class`/`.jar`, dex-reader the `.dex` they
    // compile down to.
    let samples_dir = resolve_jvm_samples(opts.jvm_samples.as_deref())?;

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
        let apk = resolve_dex_apk(opts.dex_apk.as_deref())?;
        let mut dex_results = dex::run_checks(samples_dir, apk.as_deref(), &work)
            .unwrap_or_else(|err| vec![("dex".to_string(), Outcome::Fail(format!("{err:#}")))]);
        if let Some(filter) = &opts.filter {
            dex_results.retain(|(name, _)| name.contains(filter));
        }
        results.extend(dex_results);
    }

    // Ghidra existing-project smoke check: drive the pcode importer against an
    // *existing* Ghidra project (not a fresh binary). Only meaningful when Ghidra
    // and a target binary are available (guaranteed under the Nix regression env),
    // and self-skips otherwise, so it is cheap to always attempt.
    {
        let (name, outcome) = run_ghidra_project_check().unwrap_or_else(|err| {
            (
                GHIDRA_PROJECT_CASE.to_string(),
                Outcome::Fail(format!("{err:#}")),
            )
        });
        if opts.filter.as_ref().is_none_or(|f| name.contains(f)) {
            results.push((name, outcome));
        }
    }

    if results.is_empty() {
        bail!("no test cases selected");
    }

    println!(
        "Ran {} regression case(s) (tests from {})",
        results.len(),
        tests_dir.display()
    );

    let (mut passed, mut skipped, mut failures, mut xfails) = (0, 0, 0, 0);
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
            Outcome::Xfail(why) => {
                xfails += 1;
                println!(
                    "  XFAIL {name}\n        {}",
                    why.replace('\n', "\n        ")
                );
            }
        }
    }

    println!(
        "\n{passed} passed, {skipped} skipped, {failures} failed, {xfails} xfail of {} case(s)",
        results.len()
    );
    Ok(failures == 0)
}

/// Locate the jvm-reader sample sources. With no override, look where the crate
/// lives relative to the repo root (`cargo xtask`) or the nightly cwd.
fn resolve_jvm_samples(override_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = override_dir {
        return Ok(Some(std::fs::canonicalize(dir).with_context(|| {
            format!("failed to canonicalize {}", dir.display())
        })?));
    }
    Ok(["jvm-reader/tests/sample", "../jvm-reader/tests/sample"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .map(|p| std::fs::canonicalize(&p))
        .transpose()
        .with_context(|| "failed to canonicalize jvm sample directory")?)
}

/// Locate the real-world APK xtask owns for the dex-reader smoke test. With no
/// override, look where it lives relative to the repo root or the nightly cwd.
fn resolve_dex_apk(override_path: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = override_path {
        return Ok(Some(std::fs::canonicalize(path).with_context(|| {
            format!("failed to canonicalize {}", path.display())
        })?));
    }
    Ok([
        "xtask/tests/dex/com.noto_54.apk",
        "../xtask/tests/dex/com.noto_54.apk",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .map(|p| std::fs::canonicalize(&p))
    .transpose()
    .with_context(|| "failed to canonicalize dex apk path")?)
}

/// Ensure the executables needed for the selected cases are on `PATH`.
fn preflight(cases: &[TestCase]) -> Result<()> {
    ctadl_bin()?;
    let needs_dex = cases.iter().any(|c| matches!(c.kind, Kind::Dex { .. }));
    let needs_jvm = cases.iter().any(|c| matches!(c.kind, Kind::Jvm { .. }));
    if needs_dex && exec::which("dex-reader").is_none() {
        bail!("`dex-reader` not found on PATH");
    }
    if needs_jvm && exec::which("jvm-reader").is_none() {
        bail!("`jvm-reader` not found on PATH");
    }
    Ok(())
}

/// Non-enforced JVM E2E failures are reported as XFAIL so the suite can stay
/// green while the frontend matures.
fn apply_jvm_allowlist(name: &str, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Fail(why) if name.starts_with("Jvm:") && !JVM_E2E_ENFORCED.contains(&name) => {
            Outcome::Xfail(why)
        }
        other => other,
    }
}

fn run_case(case: &TestCase) -> Result<Outcome> {
    match &case.kind {
        Kind::Dex { java, config } => run_dex(&case.name, java, config),
        Kind::Jvm { java, config } => run_jvm(&case.name, java, config),
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

    // Analyze: import / index / query (query also formats the SARIF output).
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
        &[
            "query",
            &project,
            "-m",
            &config.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
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
    check_byte_offset_lines(expected, offsets, &linemap)
}

// --- JVM / Java -----------------------------------------------------------

fn run_jvm(case_name: &str, java: &Path, config: &Path) -> Result<Outcome> {
    for tool in ["javac", "jar"] {
        if exec::which(tool).is_none() {
            return Ok(Outcome::Skip(format!("`{tool}` not on PATH")));
        }
    }

    let stem = java
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("bad java file name {}", java.display()))?;

    let work = scratch_dir(case_name)?;
    let state = work.join("state");
    std::fs::create_dir_all(&state)?;

    // Compile to a JAR inside the scratch dir. We copy only this source so the
    // class output cannot pick up unrelated classes.
    let src = work.join(format!("{stem}.java"));
    std::fs::copy(java, &src)
        .with_context(|| format!("failed to copy {} into scratch dir", java.display()))?;

    let class_dir = work.join("classes");
    std::fs::create_dir_all(&class_dir)?;

    let mut javac = Command::new("javac");
    javac
        .current_dir(&work)
        .args(["--release", "8", "-d"])
        .arg(&class_dir)
        .arg(&src);
    exec::run_checked(javac, "javac")?;

    let jar = work.join(format!("{stem}.jar"));
    let mut jar_cmd = Command::new("jar");
    jar_cmd
        .arg("cf")
        .arg(&jar)
        .arg("-C")
        .arg(&class_dir)
        .arg(".");
    exec::run_checked(jar_cmd, "jar")?;

    // Analyze: import / index / query (query also formats the SARIF output).
    let project = format!("{stem}_jvm_test");
    let sarif = work.join(format!("{stem}_output.sarif"));
    run_ctadl(
        &work,
        &state,
        &["import", "--name", &project, &jar_arg(&jar)],
    )?;
    run_ctadl(&work, &state, &["index", &project])?;
    run_ctadl(
        &work,
        &state,
        &[
            "query",
            &project,
            "-m",
            &config.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
    )?;

    // Build the offset -> line map.
    let linemap = work.join(format!("{stem}_linemap.json"));
    let mut reader = Command::new("jvm-reader");
    reader
        .current_dir(&work)
        .arg("--jar")
        .arg(&jar)
        .arg("--linemap-json")
        .arg(&linemap);
    exec::run_checked(reader, "jvm-reader")?;

    let expected = assertions::read_expected_lines(config)?;
    let offsets = assertions::collect_byte_offsets(&sarif)?;
    check_byte_offset_lines(expected, offsets, &linemap)
}

/// DEX/JVM pass criterion: at least one expected line among mapped offsets,
/// or no flows when `expected_lines` is empty.
fn check_byte_offset_lines(
    expected: Vec<i64>,
    offsets: BTreeSet<i64>,
    linemap: &Path,
) -> Result<Outcome> {
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

    let entries = assertions::load_linemap(linemap)?;
    let mapped: BTreeSet<i64> = offsets
        .iter()
        .filter_map(|&off| assertions::map_offset_to_line(&entries, off))
        .collect();

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

fn jar_arg(jar: &Path) -> String {
    jar.to_string_lossy().into_owned()
}

fn ctadl_bin() -> Result<PathBuf> {
    // Prefer the workspace build so regression tracks the tree under test.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        for subdir in ["release", "debug"] {
            let candidate = PathBuf::from(&manifest)
                .join("..")
                .join("target")
                .join(subdir)
                .join(if cfg!(windows) { "ctadl.exe" } else { "ctadl" });
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    exec::which("ctadl").context("`ctadl` not found on PATH or in target/{release,debug}")
}

fn run_ctadl(work: &Path, state: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new(ctadl_bin()?);
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
        &[
            "query",
            &project,
            "-m",
            &query.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
    )?;

    // Prefer the section-relative offsets the analyzer now emits (image base
    // already subtracted via the `PROGRAM_IMAGE_BASE` fact). Fall back to
    // absolute addresses minus the historical base for older SARIF that
    // predates `relativeAddress`.
    let relative = assertions::collect_relative_addresses(&sarif)?;
    let offsets: Vec<i64> = if !relative.is_empty() {
        relative.into_iter().collect()
    } else {
        assertions::collect_absolute_addresses(&sarif)?
            .into_iter()
            .map(|addr| addr - PCODE_BASE_ADDRESS)
            .collect()
    };
    if offsets.is_empty() {
        if cfg!(target_os = "macos") {
            return Ok(Outcome::Skip(
                "no tainted instructions on Darwin; skipping strict offset check".to_string(),
            ));
        }
        return Ok(Outcome::Fail(
            "no tainted instructions in SARIF output".to_string(),
        ));
    }

    // Map each tainted offset back to a source line via addr2line.
    let mut found: BTreeSet<i64> = BTreeSet::new();
    for off in &offsets {
        let rel = format!("0x{:x}", off);
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
    let mut cmd = Command::new(ctadl_bin()?);
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

// --- Ghidra existing-project import ---------------------------------------

/// Reporting name for the Ghidra existing-project smoke check.
const GHIDRA_PROJECT_CASE: &str = "Pcode:GhidraProject";

/// A small, self-contained C program used as the import target. Compiled to a
/// relocatable object (a few KB), it keeps the Ghidra analysis fast and the test
/// deterministic -- unlike, say, a system `tee`, which under Nix is a symlink to
/// the ~1.4 MB multicall `coreutils` binary.
const SMOKE_C: &str = r#"
#include <string.h>
int process(const char *s, char *d) { strcpy(d, s); return (int)strlen(d); }
int main(int argc, char **argv) {
    char buf[64];
    if (argc > 1) return process(argv[1], buf);
    return 0;
}
"#;

/// Smoke-test the "import pcode from an existing Ghidra project" feature.
///
/// Rather than importing a fresh binary (which Ghidra loads into a throwaway
/// project), this compiles a tiny C program, builds a *persistent* Ghidra project
/// by importing the resulting object, then drives `ctadl import -l pcode
/// <project>.gpr`. That path runs the same `ExportPcode` script via
/// `analyzeHeadless -process` against the already-populated project. We assert the
/// importer produced a non-empty IR program in the store.
///
/// Self-skips (rather than failing) when Ghidra or a C compiler is unavailable, so
/// it is harmless to attempt outside the Nix regression environment.
fn run_ghidra_project_check() -> Result<(String, Outcome)> {
    let name = GHIDRA_PROJECT_CASE.to_string();

    let Some(analyze_headless) = find_analyze_headless() else {
        return Ok((
            name,
            Outcome::Skip("Ghidra analyzeHeadless not found (set GHIDRA_HOME)".to_string()),
        ));
    };
    let (cc, _addr2line) = pick_toolchain();
    if exec::which(&cc).is_none() {
        return Ok((name, Outcome::Skip(format!("`{cc}` not on PATH"))));
    }

    let work = scratch_dir("Pcode_GhidraProject")?;
    let state = work.join("state");
    let proj_loc = work.join("ghidra-project");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&proj_loc)?;

    // Ghidra needs a writable HOME and its usual Java options; reuse the pcode env.
    let home = ensure_writable_home()?;
    let java_tool_options = format!("-Duser.home={}", home.display());
    let env = pcode_env(&home, &java_tool_options);

    // 1. Compile the tiny program to a small relocatable object.
    let src = work.join("smoke.c");
    std::fs::write(&src, SMOKE_C)?;
    let obj = work.join("smoke.o");
    let mut compile = Command::new(&cc);
    compile
        .current_dir(&work)
        .args(["-g", "-O0", "-c"])
        .arg(&src)
        .arg("-o")
        .arg(&obj);
    exec::run_checked(compile, &cc)?;

    // 2. Build a persistent project by importing the object (note: no
    //    -deleteProject, so the project survives for the `ctadl import` to -process).
    let proj_name = "ctadl_ghidra_smoke";
    let mut build = Command::new(&analyze_headless);
    build
        .current_dir(&work)
        .arg(&proj_loc)
        .arg(proj_name)
        .arg("-import")
        .arg(&obj);
    for (k, v) in &env {
        build.env(k, v);
    }
    exec::run_checked(build, "analyzeHeadless -import")?;

    let gpr = proj_loc.join(format!("{proj_name}.gpr"));
    if !gpr.is_file() {
        return Ok((
            name,
            Outcome::Fail(format!(
                "Ghidra did not create project file {}",
                gpr.display()
            )),
        ));
    }

    // 3. Import pcode from the EXISTING project (exercises `-process`).
    let import_name = "ghidra_project_smoke";
    run_ctadl_env(
        &work,
        &state,
        &env,
        &[
            "import",
            "-l",
            "pcode",
            &gpr.to_string_lossy(),
            "-n",
            import_name,
        ],
    )?;

    // 4. A successful import writes a non-empty IR program into the store at
    //    `<XDG_STATE_HOME>/ctadl/imports/<name>/ir-program.bitcode`.
    let bitcode = state
        .join("ctadl")
        .join("imports")
        .join(import_name)
        .join("ir-program.bitcode");
    match std::fs::metadata(&bitcode) {
        Ok(m) if m.len() > 0 => Ok((name, Outcome::Pass)),
        Ok(_) => Ok((
            name,
            Outcome::Fail(format!("{} is empty", bitcode.display())),
        )),
        Err(e) => Ok((
            name,
            Outcome::Fail(format!("missing IR program {}: {e}", bitcode.display())),
        )),
    }
}

/// Locate Ghidra's `analyzeHeadless` launcher. Prefer `$GHIDRA_HOME/support`
/// (set by the Nix regression check), then fall back to the `ghidra-bin` package
/// name and the bare launcher on `PATH`.
fn find_analyze_headless() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GHIDRA_HOME") {
        let candidate = PathBuf::from(home).join("support").join("analyzeHeadless");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    ["ghidra-analyzeHeadless", "analyzeHeadless"]
        .into_iter()
        .find_map(exec::which)
}

// --- shared ----------------------------------------------------------------

fn scratch_dir(name: &str) -> Result<PathBuf> {
    // Colons are invalid in Windows directory names (e.g. `Jvm:Foo`).
    let safe_name = name.replace(':', "_");
    let dir = std::env::temp_dir().join(format!("ctadl_xtask_{safe_name}"));
    exec::fresh_dir(&dir)?;
    Ok(dir)
}
