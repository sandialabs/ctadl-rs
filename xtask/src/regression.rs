//! Orchestration of the regression suite: drive the analyzer per case and check
//! its output against the case's known answer.
//!
//! Each case runs in its own scratch directory so that ctadl project state
//! (kept under `XDG_STATE_HOME`) and build artifacts never collide between
//! cases. Unlike the old `set -e` scripts, a single failing case does not abort
//! the run: we execute every selected case and report them all.
//!
//! Cases are independent, so they run on a pool of `--jobs` workers rather than
//! one after another. The two things that are *not* per-case -- the scratch
//! directory and Ghidra's user directories -- are made per-worker or per-case
//! explicitly; see [`Worker`] and [`scratch_dir`].

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::assertions;
use crate::dex;
use crate::discovery::{self, Frontend, Kind, TestCase};
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

pub struct Options {
    pub filter: Option<String>,
    /// Frontends to exercise. Defaults to all of them, so an unqualified
    /// `xtask regression` behaves exactly as before.
    pub frontends: BTreeSet<Frontend>,
    pub tests_dir: Option<PathBuf>,
    pub jvm_samples: Option<PathBuf>,
    pub dex_apk: Option<PathBuf>,
    /// How many cases to run concurrently. `None` picks [`default_jobs`].
    pub jobs: Option<usize>,
    /// Build (and run) the release `ctadl` binary instead of the debug one.
    /// Either way the binary is rebuilt from current source before any case
    /// runs; see [`build_ctadl`].
    pub release: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            filter: None,
            frontends: Frontend::ALL.iter().copied().collect(),
            tests_dir: None,
            jvm_samples: None,
            dex_apk: None,
            jobs: None,
            release: false,
        }
    }
}

/// Upper bound on the default worker count.
///
/// A case is mostly spent inside external processes that are already parallel on
/// their own (Ghidra's JVM, ctadl's rayon pool), so workers stop buying wall clock
/// well before they stop buying cores: on a 20-core machine the full suite ran in
/// 3m24s at 1 job, 52s at 4, 34s at 8, and 30s at 16. Past this the extra Ghidra
/// JVMs -- each asking for a large heap -- cost more memory than the seconds are
/// worth. Raise it with `--jobs` on a machine with headroom to spare.
const MAX_DEFAULT_JOBS: usize = 8;

/// Worker count when `--jobs` is not given: one per available core, capped.
///
/// `available_parallelism` honours cgroup CPU limits, so this stays polite inside
/// the Nix sandbox and CI containers rather than reading the whole host.
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, MAX_DEFAULT_JOBS)
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
    cases.retain(|c| opts.frontends.contains(&c.kind.frontend()));
    if let Some(filter) = &opts.filter {
        cases.retain(|c| c.name.contains(filter));
    }

    // The Ghidra existing-project check is selected before it runs: a headless
    // Ghidra analysis is far too expensive to run only to drop from the report.
    // Decided up front because preflight needs to know whether Ghidra is in play.
    let ghidra_selected = opts.frontends.contains(&Frontend::Pcode)
        && opts
            .filter
            .as_ref()
            .is_none_or(|f| GHIDRA_PROJECT_CASE.contains(f));

    // ctadl/dex-reader/jvm-reader are only needed for the matching case kinds.
    if !cases.is_empty() || ghidra_selected {
        preflight(&cases, ghidra_selected, opts.release)?;
    }

    // The jvm-reader and dex-reader checks share the same Java sample sources:
    // jvm-reader exercises the `.class`/`.jar`, dex-reader the `.dex` they
    // compile down to. Resolving them is only worth it when a Java frontend is
    // selected.
    let wants_jvm = opts.frontends.contains(&Frontend::Jvm);
    let wants_dex = opts.frontends.contains(&Frontend::Dex);
    let samples_dir = if wants_jvm || wants_dex {
        resolve_jvm_samples(opts.jvm_samples.as_deref())?
    } else {
        None
    };
    let dex_apk = if samples_dir.is_some() && wants_dex {
        resolve_dex_apk(opts.dex_apk.as_deref())?
    } else {
        None
    };

    // Assemble every independent unit of work, in report order. The pool below
    // preserves that order regardless of which worker finishes first.
    let mut tasks: Vec<Task<'_>> = cases.iter().map(Task::Case).collect();
    if let Some(samples) = samples_dir.as_deref().filter(|_| wants_jvm) {
        tasks.push(Task::JvmChecks { samples });
    }
    if let Some(samples) = samples_dir.as_deref().filter(|_| wants_dex) {
        tasks.push(Task::DexChecks {
            samples,
            apk: dex_apk.as_deref(),
        });
    }
    // Ghidra existing-project smoke check: drive the pcode importer against an
    // *existing* Ghidra project (not a fresh binary). Only meaningful when Ghidra
    // and a target binary are available (guaranteed under the Nix regression env),
    // and self-skips otherwise, so it is cheap to always attempt.
    if ghidra_selected {
        tasks.push(Task::GhidraProject);
    }

    // More workers than tasks would just create idle scratch homes; no tasks at
    // all means no workers, and the empty report below reports the selection.
    let jobs = opts.jobs.unwrap_or_else(default_jobs).min(tasks.len());
    let results = run_tasks(&tasks, opts, jobs)?;

    if results.is_empty() {
        bail!("no test cases selected ({})", describe_selection(opts));
    }

    println!(
        "Ran {} regression case(s) (tests from {}, {}, {jobs} job(s))",
        results.len(),
        tests_dir.display(),
        describe_selection(opts)
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

/// One independent unit of work. Each yields zero or more report entries.
///
/// The reader checks are single tasks rather than one per check: they share the
/// compiled samples, so splitting them would recompile the same sources N times.
enum Task<'a> {
    Case(&'a TestCase),
    JvmChecks {
        samples: &'a Path,
    },
    DexChecks {
        samples: &'a Path,
        apk: Option<&'a Path>,
    },
    GhidraProject,
}

impl Task<'_> {
    /// Run this task to completion. Infrastructure failures (analyzer crashed,
    /// tool missing, etc.) are reported as failed entries rather than propagated,
    /// so one broken case never aborts the run.
    fn run(&self, worker: &Worker, opts: &Options) -> Vec<(String, Outcome)> {
        match self {
            Task::Case(case) => {
                let outcome =
                    run_case(case, worker).unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")));
                vec![(case.name.clone(), apply_xfail_policy(&case.name, outcome))]
            }
            Task::JvmChecks { samples } => {
                // jvm-reader checks: compile the sample .java and exercise
                // jvm-reader on the resulting .class files and a jar built from
                // them.
                let results = scratch_dir("jvm")
                    .and_then(|work| jvm::run_checks(samples, &work))
                    .unwrap_or_else(|err| {
                        vec![("jvm".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::DexChecks { samples, apk } => {
                // dex-reader checks: compile the same samples down to .dex and
                // parse them, plus a real-world APK that xtask owns.
                let results = scratch_dir("dex")
                    .and_then(|work| dex::run_checks(samples, *apk, &work))
                    .unwrap_or_else(|err| {
                        vec![("dex".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::GhidraProject => {
                let (name, outcome) = run_ghidra_project_check(worker).unwrap_or_else(|err| {
                    (
                        GHIDRA_PROJECT_CASE.to_string(),
                        Outcome::Fail(format!("{err:#}")),
                    )
                });
                vec![(name, outcome)]
            }
        }
    }
}

/// Apply `--filter` to reader-check results. Unlike the taint cases, these names
/// are only known once the checks have run, so they are filtered on the way out.
fn retain_filtered(mut results: Vec<(String, Outcome)>, opts: &Options) -> Vec<(String, Outcome)> {
    if let Some(filter) = &opts.filter {
        results.retain(|(name, _)| name.contains(filter));
    }
    results
}

/// Run `tasks` across `jobs` workers, returning every result in task order.
///
/// Each worker pulls the next unclaimed task until the list is exhausted, so a
/// slow case (a Ghidra import) never blocks the queue behind it. Results are
/// tagged with their task index and sorted at the end rather than appended as
/// they land, so the report reads identically no matter who finished first.
fn run_tasks(tasks: &[Task<'_>], opts: &Options, jobs: usize) -> Result<Vec<(String, Outcome)>> {
    let workers = (0..jobs).map(Worker::new).collect::<Result<Vec<_>>>()?;

    let next = &AtomicUsize::new(0);
    let results = &Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for worker in &workers {
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(task) = tasks.get(index) else { break };
                let outcomes = task.run(worker, opts);
                // Held only to push a finished result; no test work happens
                // under the lock.
                results.lock().unwrap().push((index, outcomes));
            });
        }
    });

    let mut collected = std::mem::take(&mut *results.lock().unwrap());
    collected.sort_by_key(|(index, _)| *index);
    Ok(collected
        .into_iter()
        .flat_map(|(_, outcomes)| outcomes)
        .collect())
}

/// Human-readable summary of what `--frontend`/`--filter` selected, so a short
/// run is never mistaken for a full one.
fn describe_selection(opts: &Options) -> String {
    let frontends: Vec<&str> = opts.frontends.iter().map(|f| f.as_str()).collect();
    let mut desc = if frontends.len() == Frontend::ALL.len() {
        "all frontends".to_string()
    } else {
        format!("frontends: {}", frontends.join(", "))
    };
    if let Some(filter) = &opts.filter {
        desc.push_str(&format!("; filter: {filter}"));
    }
    desc
}

/// Locate the jvm-reader sample sources. With no override, look where the crate
/// lives relative to the repo root (`cargo xtask`) or the nightly cwd.
fn resolve_jvm_samples(override_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = override_dir {
        return Ok(Some(std::fs::canonicalize(dir).with_context(|| {
            format!("failed to canonicalize {}", dir.display())
        })?));
    }
    ["jvm-reader/tests/sample", "../jvm-reader/tests/sample"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .map(|p| std::fs::canonicalize(&p))
        .transpose()
        .with_context(|| "failed to canonicalize jvm sample directory")
}

/// Locate the real-world APK xtask owns for the dex-reader smoke test. With no
/// override, look where it lives relative to the repo root or the nightly cwd.
fn resolve_dex_apk(override_path: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = override_path {
        return Ok(Some(std::fs::canonicalize(path).with_context(|| {
            format!("failed to canonicalize {}", path.display())
        })?));
    }
    [
        "xtask/tests/dex/com.noto_54.apk",
        "../xtask/tests/dex/com.noto_54.apk",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .map(|p| std::fs::canonicalize(&p))
    .transpose()
    .with_context(|| "failed to canonicalize dex apk path")
}

/// Ensure the executables needed for the selected cases are available.
///
/// Runs once, single-threaded, before any worker starts. It (re)builds the `ctadl` binary from
/// current source and records its path, so it's not stale. The reader binaries
/// (`dex-reader`/`jvm-reader`) are workspace-excluded and come from `PATH`, so those are still just
/// presence-checked here.
fn preflight(cases: &[TestCase], ghidra_selected: bool, release: bool) -> Result<()> {
    let bin = prebuilt_ctadl()?.map_or_else(|| build_ctadl(release), Ok)?;
    // First (and only) writer; a later worker reading it back via `ctadl_bin`
    // sees this value.
    CTADL_BIN
        .set(bin)
        .map_err(|_| anyhow::anyhow!("internal error: ctadl binary built more than once"))?;
    let needs_dex = cases.iter().any(|c| matches!(c.kind, Kind::Dex { .. }));
    let needs_jvm = cases.iter().any(|c| matches!(c.kind, Kind::Jvm { .. }));
    let needs_pcode = ghidra_selected || cases.iter().any(|c| matches!(c.kind, Kind::Pcode { .. }));
    if needs_dex && exec::which("dex-reader").is_none() {
        bail!("`dex-reader` not found on PATH");
    }
    if needs_jvm && exec::which("jvm-reader").is_none() {
        bail!("`jvm-reader` not found on PATH");
    }
    if needs_pcode {
        preflight_java()?;
    }
    Ok(())
}

/// Fail fast when the JDK that Ghidra needs is missing or unusable.
///
/// The pcode cases drive Ghidra, whose launcher probes for a JDK before doing
/// any work. On a Mac with no JDK installed, `/usr/bin/java` is Apple's stub:
/// `java -version` exits non-zero with "Unable to locate a Java Runtime", but
/// the probe Ghidra actually runs (`java -XshowSettings:properties -version`)
/// *spins forever* instead of failing. `analyzeHeadless` then hangs with no
/// output and no timeout, which reads as a wedged suite rather than a missing
/// dependency. One cheap, bounded probe here turns that into a real error.
fn preflight_java() -> Result<()> {
    let java = exec::which("java").context(
        "`java` not found on PATH, but the pcode frontend needs a JDK to run Ghidra.\n\
         Enter the regression dev shell (`nix develop .#regression`), which supplies one.",
    )?;

    let mut cmd = Command::new(&java);
    cmd.arg("-version");
    let out = exec::run_with_timeout(cmd, "java -version", Duration::from_secs(30))?;
    if out.status.success() {
        return Ok(());
    }

    // The stub is the overwhelmingly likely reason to get here on macOS, and its
    // message is the one that explains the situation, so surface it verbatim.
    let stderr = String::from_utf8_lossy(&out.stderr);
    bail!(
        "`{}` is not a usable JDK, but the pcode frontend needs one to run Ghidra.\n\
         It exited {} with:\n  {}\n\
         On macOS this is usually Apple's `/usr/bin/java` stub with no JDK behind it;\n\
         Ghidra's own JDK probe hangs forever on it rather than failing.\n\
         Enter the regression dev shell (`nix develop .#regression`), which supplies a JDK.",
        java.display(),
        out.status
            .code()
            .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
        stderr.trim().replace('\n', "\n  "),
    );
}

/// Failures on a frontend that is still maturing are reported as XFAIL so the
/// suite can stay green while its gaps are visible in the report.
///
/// One frontend qualifies: non-enforced JVM E2E cases, per [`JVM_E2E_ENFORCED`].
///
/// `Php:` cases used to qualify too, back when the frontend found sources, sinks
/// and tainted instructions but did not link them into an end-to-end path. It
/// links them now and every case passes, so they are enforced like any other.
fn apply_xfail_policy(name: &str, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Fail(why) if name.starts_with("Jvm:") && !JVM_E2E_ENFORCED.contains(&name) => {
            Outcome::Xfail(why)
        }
        other => other,
    }
}

fn run_case(case: &TestCase, worker: &Worker) -> Result<Outcome> {
    match &case.kind {
        Kind::Dex { java, config } => run_dex(&case.name, java, config),
        Kind::Jvm { java, config } => run_jvm(&case.name, java, config),
        Kind::Pcode { source, query } => run_pcode(&case.name, source, query, worker),
        Kind::Php { source, query } => run_php(&case.name, source, query),
    }
}

// --- PHP -------------------------------------------------------------------

/// Analyze a PHP source file and check the reported lines against the known
/// answer.
///
/// The shortest runner here: PHP needs no toolchain and no compile step, and the
/// lowering records source spans, so the SARIF regions carry source lines
/// directly. There is nothing to map back.
///
/// Pass criterion matches DEX/JVM (at least one `expected_lines` entry among the
/// reported lines) rather than pcode's stricter all-of, since a path result names
/// the lines along one flow and need not cover every line a case lists.
fn run_php(name: &str, source: &Path, query: &Path) -> Result<Outcome> {
    let work = scratch_dir(name)?;
    let state = work.join("state");
    std::fs::create_dir_all(&state)?;

    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("bad php file name {}", source.display()))?;

    let project = format!("{stem}_php_test");
    let sarif = work.join(format!("{stem}_output.sarif"));
    let source_str = source.to_string_lossy().into_owned();

    // `-l php` rather than extension sniffing, so the case still exercises the
    // PHP frontend if detection ever changes.
    run_ctadl(
        &work,
        &state,
        &["import", "-l", "php", &source_str, "-n", &project],
    )?;
    run_ctadl(
        &work,
        &state,
        &["index", &project, "-m", &query.to_string_lossy()],
    )?;
    run_ctadl(
        &work,
        &state,
        &[
            "query",
            &project,
            "-m",
            &query.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
    )?;

    let expected = assertions::read_expected_lines(query)?;
    let found = assertions::collect_start_lines(&sarif)?;

    if expected.is_empty() {
        // Negative case: no flow may be reported at all.
        return Ok(if found.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail(format!("expected no flows, but found lines {found:?}"))
        });
    }

    if found.is_empty() {
        return Ok(Outcome::Fail(
            "no source lines in SARIF output (no tainted path reported)".to_string(),
        ));
    }

    if !expected.iter().any(|line| found.contains(line)) {
        return Ok(Outcome::Fail(format!(
            "none of the expected lines {expected:?} appear in reported lines {found:?}"
        )));
    }

    let unexpected = assertions::read_unexpected_lines(query)?;
    if let Some(why) = assertions::check_unexpected_lines(&unexpected, &found) {
        return Ok(Outcome::Fail(why));
    }

    Ok(Outcome::Pass)
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

    // Analyze: import / index / query. The human profile carries the code flows
    // (the traced source -> ... -> sink path); the machine profile carries the tainted
    // instructions. The check reads both -- see `check_flow_case`.
    let project = format!("{name}_test");
    let sarif = work.join(format!("{name}_output.sarif"));
    let machine_sarif = work.join(format!("{name}_machine.sarif"));
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
    run_ctadl(
        &work,
        &state,
        &[
            "query",
            &project,
            "-m",
            &config.to_string_lossy(),
            "--sarif-profile",
            "machine",
            "-o",
            &machine_sarif.to_string_lossy(),
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

    check_flow_case(config, &sarif, &machine_sarif, &linemap)
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

    // Analyze: import / index / query. Human profile -> code flows; machine profile ->
    // tainted instructions. See `check_flow_case`.
    let project = format!("{stem}_jvm_test");
    let sarif = work.join(format!("{stem}_output.sarif"));
    let machine_sarif = work.join(format!("{stem}_machine.sarif"));
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
    run_ctadl(
        &work,
        &state,
        &[
            "query",
            &project,
            "-m",
            &config.to_string_lossy(),
            "--sarif-profile",
            "machine",
            "-o",
            &machine_sarif.to_string_lossy(),
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

    check_flow_case(config, &sarif, &machine_sarif, &linemap)
}

/// DEX/JVM pass criterion, over the human profile (code flows) and machine profile
/// (tainted instructions) of one case:
///
///  1. *Code-flow integrity*: a human-profile code flow connects a source to a sink.
///     This is what regressed when the step dedup collapsed every flow to its lone
///     source step; a lines-only check can't see it, because the source line alone
///     satisfies line coverage. Negative cases (`expected_lines` empty) invert this:
///     no such flow may exist.
///  2. *Reached lines*: every `expected_lines` entry maps from some offset in the union
///     of the code-flow offsets and the tainted-instruction offsets. The union is used
///     because aliasing legitimately elides a pure copy hop (`local = tainted_field`)
///     from the traced flow while it still shows up as a tainted instruction -- so a
///     line the flow visits only "through" an alias is covered by the machine profile.
///  3. *Unexpected lines*: no `unexpected_lines` entry is among the reached lines.
fn check_flow_case(
    config: &Path,
    human_sarif: &Path,
    machine_sarif: &Path,
    linemap: &Path,
) -> Result<Outcome> {
    let expected = assertions::read_expected_lines(config)?;
    let unexpected = assertions::read_unexpected_lines(config)?;
    let connects = assertions::codeflow_connects_source_and_sink(human_sarif)?;

    if expected.is_empty() {
        // Negative case: there must be no traced source -> sink flow. (No reached-line
        // claim to make, so this subsumes any `unexpected_lines`.)
        return Ok(if connects {
            Outcome::Fail(
                "expected no flow, but a code flow connects a source to a sink".to_string(),
            )
        } else {
            Outcome::Pass
        });
    }

    if !connects {
        return Ok(Outcome::Fail(
            "no code flow connects a source to a sink".to_string(),
        ));
    }

    let entries = assertions::load_linemap(linemap)?;
    let mut offsets = assertions::collect_codeflow_byte_offsets(human_sarif)?;
    offsets.extend(assertions::collect_byte_offsets(machine_sarif)?);
    let reached: BTreeSet<i64> = offsets
        .iter()
        .filter_map(|&off| assertions::map_offset_to_line(&entries, off))
        .collect();

    let missing: Vec<i64> = expected
        .iter()
        .copied()
        .filter(|line| !reached.contains(line))
        .collect();
    if !missing.is_empty() {
        return Ok(Outcome::Fail(format!(
            "expected lines {missing:?} not reached (reached {reached:?}; all of {expected:?} required)"
        )));
    }

    if let Some(why) = assertions::check_unexpected_lines(&unexpected, &reached) {
        return Ok(Outcome::Fail(why));
    }

    Ok(Outcome::Pass)
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

/// Path to the `ctadl` binary that [`build_ctadl`] produced. Set exactly once by
/// [`preflight`] before any worker starts, then read (never written) by the
/// per-case helpers below.
static CTADL_BIN: OnceLock<PathBuf> = OnceLock::new();

/// The freshly built `ctadl` binary the cases run against.
///
/// This deliberately does *not* discover a binary on disk. Scanning
/// `target/{release,debug}` and running whatever exists is a correctness hazard:
/// a stale `target/release/ctadl` from an older commit is silently exercised
/// against current source, so the suite can report all-green while the code under
/// test is actually broken. That really happened -- a stale release binary masked
/// a total taint-flow regression in the PHP frontend. The only path that can be
/// returned is the one [`build_ctadl`] just built from the current tree.
fn ctadl_bin() -> Result<PathBuf> {
    CTADL_BIN.get().cloned().context(
        "internal error: ctadl binary requested before it was built; preflight must run first",
    )
}

/// Build the `ctadl` binary from the current source tree and return the path to
/// the executable cargo produced.
///
/// The binary is rebuilt in this same invocation, so it is guaranteed to
/// correspond to the tree under test -- the whole point of replacing the old
/// on-disk discovery (see [`ctadl_bin`]). We parse cargo's `--message-format=json`
/// stream and take the `executable` path from the `ctadl` compiler-artifact
/// message, so there is no guessing about the profile directory or a `.exe`
/// suffix: cargo tells us exactly where it wrote the binary.
/// A prebuilt `ctadl` binary to run against, taken from `$CTADL_BIN`.
///
/// The default flow rebuilds `ctadl` from the tree under test so a stale binary
/// can never mask a regression (see [`build_ctadl`]). But some environments have
/// no source tree and no `cargo` to build with -- notably the Nix `regression`
/// check, which runs the prebuilt `xtask` against the `ctadl` that ships in
/// `packages.default`. There, `$CTADL_BIN` points at that binary and we trust it
/// rather than trying (and failing) to compile.
///
/// Returns `Ok(None)` when the variable is unset (the normal build-from-source
/// path) and an error only when it is set but does not name a real file.
fn prebuilt_ctadl() -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os("CTADL_BIN") else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_file() {
        bail!(
            "$CTADL_BIN is set to `{}`, but that is not a file",
            path.display()
        );
    }
    println!("Using prebuilt ctadl from $CTADL_BIN: {}", path.display());
    Ok(Some(path))
}

fn build_ctadl(release: bool) -> Result<PathBuf> {
    // `cargo` sets `$CARGO` for the subcommands it spawns (`cargo xtask` is one),
    // so prefer it; fall back to the bare name for a direct `xtask` invocation.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let profile = if release { "release" } else { "debug" };
    println!("Building ctadl ({profile}) before running the suite...");

    let mut cmd = Command::new(cargo);
    // Run from the workspace root regardless of the caller's cwd. The xtask crate
    // lives at `<root>/xtask`, so its manifest dir's parent is the root.
    cmd.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().context(
        "internal error: CARGO_MANIFEST_DIR has no parent; expected `<workspace>/xtask`",
    )?)
    .args([
        "build",
        "--message-format=json",
        "-p",
        "ctadl-ascent",
        "--bin",
        "ctadl",
    ]);
    if release {
        cmd.arg("--release");
    }

    // Stream the machine-readable artifact records on stdout for us to parse,
    // while cargo's human-readable progress and any errors flow straight through
    // stderr to the user.
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn `cargo build` for the ctadl binary")?;

    let stdout = child
        .stdout
        .take()
        .context("cargo build produced no stdout to read artifacts from")?;

    let mut exe: Option<PathBuf> = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.context("failed to read `cargo build` output")?;
        // cargo emits one JSON object per line; ignore anything unparseable
        // rather than assume the whole stream is JSON.
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // The `ctadl` binary's compiler-artifact carries the path we want in its
        // (non-null) `executable` field.
        if msg["reason"] == "compiler-artifact" && msg["target"]["name"] == "ctadl" {
            if let Some(path) = msg["executable"].as_str() {
                exe = Some(PathBuf::from(path));
            }
        }
    }

    let status = child
        .wait()
        .context("failed to wait on `cargo build` for the ctadl binary")?;
    if !status.success() {
        bail!(
            "`cargo build --bin ctadl` failed ({})",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string()),
        );
    }

    exe.context(
        "`cargo build --bin ctadl` reported no executable for the `ctadl` binary\n\
         (expected a compiler-artifact message with a non-null `executable` path)",
    )
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

fn run_pcode(name: &str, source: &Path, query: &Path, worker: &Worker) -> Result<Outcome> {
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

    let project = format!("{name}_pcode");
    let sarif = outdir.join(format!("{name}_results.sarif"));
    let obj_str = obj.to_string_lossy().into_owned();
    let pcode_env = &worker.ghidra_env;

    run_ctadl_env(
        &work,
        &state,
        pcode_env,
        &["import", "-l", "pcode", &obj_str, "-n", &project],
    )?;
    run_ctadl_env(&work, &state, pcode_env, &["index", &project])?;
    run_ctadl_env(
        &work,
        &state,
        pcode_env,
        &[
            "query",
            &project,
            "-m",
            &query.to_string_lossy(),
            "-o",
            &sarif.to_string_lossy(),
        ],
    )?;

    // Read the known answer up front so a negative case (`expected_lines: []`)
    // can be judged purely on code-flow connectivity, without needing any tainted
    // instruction output at all.
    let expected = assertions::read_expected_lines(query)?;

    // Code-flow integrity, at parity with the DEX/JVM check (see `check_flow_case`):
    // a human-profile code flow must connect a source to a sink. This is exactly
    // what would regress if step dedup ever collapsed a pcode flow to its lone
    // source step -- and the whole-document address check below cannot see such a
    // collapse, because the tainted-instruction result locations carry the
    // addresses regardless of whether any flow was actually traced. Pcode steps key
    // on `address.absoluteAddress`, so the specific dedup bug we fixed never
    // manifested here; this check guards against a future regression, not a current
    // one. A negative case inverts it: no such flow may exist.
    let connects = assertions::codeflow_connects_source_and_sink(&sarif)?;
    if expected.is_empty() {
        return Ok(if connects {
            Outcome::Fail(
                "expected no flow, but a code flow connects a source to a sink".to_string(),
            )
        } else {
            Outcome::Pass
        });
    }

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

    // A positive case must also carry a connected source -> sink flow. Checked
    // after the tainted-instruction guard above so the macOS self-skip (where there
    // is no taint output at all) still wins rather than reporting a spurious failure.
    if !connects {
        return Ok(Outcome::Fail(
            "no code flow connects a source to a sink".to_string(),
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

    let missing: Vec<i64> = expected
        .iter()
        .copied()
        .filter(|l| !found.contains(l))
        .collect();

    if !missing.is_empty() {
        return Ok(Outcome::Fail(format!(
            "expected lines {missing:?} not found among {found:?}"
        )));
    }

    let unexpected = assertions::read_unexpected_lines(query)?;
    if let Some(why) = assertions::check_unexpected_lines(&unexpected, &found) {
        return Ok(Outcome::Fail(why));
    }

    // PASS only if every expected line was found (the pcode criterion) and no
    // unexpected line was.
    Ok(Outcome::Pass)
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

fn guess_java_home() -> Option<PathBuf> {
    let java = exec::which("java")?;
    let real = std::fs::canonicalize(java).ok()?;
    // <java_home>/bin/java -> <java_home>
    Some(real.parent()?.parent()?.to_path_buf())
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
fn run_ghidra_project_check(worker: &Worker) -> Result<(String, Outcome)> {
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

    // This check drives Ghidra twice -- once directly, once through ctadl -- so
    // both get the worker's private Ghidra directories, same as the pcode cases.
    let env = &worker.ghidra_env;

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
    for (k, v) in env {
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
        env,
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

/// A single lane of execution, and the private state the cases it runs need.
///
/// State lives here, rather than per case, when it is safe to *reuse serially*
/// but not to *share concurrently*: a worker runs one case at a time, so each of
/// these directories has exactly one live user, while Ghidra's first-run setup is
/// still amortized across every case that worker picks up.
struct Worker {
    /// Environment for the Ghidra-driving cases (pcode, and the existing-project
    /// check). See [`Worker::new`] for why each entry is here.
    ghidra_env: Vec<(String, String)>,
}

impl Worker {
    fn new(index: usize) -> Result<Self> {
        // Ghidra keeps three user directories -- settings, cache, and temp -- and
        // writes to all of them on every headless run. Left to itself it derives
        // them from `user.home`, `XDG_CACHE_HOME`, and `java.io.tmpdir`, none of
        // which vary per process: two concurrent runs would land on the same
        // files. It checks these `application.*dir` properties first, though, so
        // pointing each worker at its own copies keeps concurrent Ghidras from
        // ever meeting. (Their *projects* are already private -- the importer
        // builds each in a fresh temp dir -- so only the user dirs need this.)
        let root = run_root().join(format!("worker-{index}"));
        let home = root.join("home");
        let settings = root.join("ghidra-settings");
        let cache = root.join("ghidra-cache");
        let temp = root.join("ghidra-temp");
        for dir in [&home, &settings, &cache, &temp] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }

        // A purpose-built HOME also settles the question the old write-probe
        // asked: Ghidra needs a writable HOME, and the ambient one may be unset,
        // read-only, or `/var/empty` under a Nix sandbox. This one is writable by
        // construction.
        let mut ghidra_env = vec![
            ("HOME".to_string(), home.display().to_string()),
            (
                "JAVA_TOOL_OPTIONS".to_string(),
                format!(
                    "-Duser.home={} -Dapplication.settingsdir={} -Dapplication.cachedir={} -Dapplication.tempdir={}",
                    home.display(),
                    settings.display(),
                    cache.display(),
                    temp.display(),
                ),
            ),
        ];
        // Guess JAVA_HOME from `java` if the environment did not set it.
        if std::env::var_os("JAVA_HOME").is_none() {
            if let Some(java_home) = guess_java_home() {
                ghidra_env.push(("JAVA_HOME".to_string(), java_home.display().to_string()));
            }
        }
        Ok(Self { ghidra_env })
    }
}

/// Root scratch directory for *this* xtask process.
///
/// The pid matters: `scratch_dir` clears the directory it hands out, so without
/// it two concurrent runs (a local shell alongside CI, two terminals, an editor
/// task) resolve to the same per-case path and one run's `fresh_dir` deletes the
/// other's working tree out from under a live subprocess. Ghidra is the worst
/// victim -- it does not fail, it blocks forever on the project it was using.
fn run_root() -> PathBuf {
    std::env::temp_dir().join(format!("ctadl_xtask_{}", std::process::id()))
}

/// Scratch names already handed out in this process.
///
/// `scratch_dir` *clears* the directory it returns, so two claimants of one name
/// means one deletes the other's working tree -- and now that cases run
/// concurrently, it would do so while the other is still working in it. The names
/// are distinct today; this keeps that a checked invariant rather than a
/// coincidence of what the test directories happen to contain, since the failure
/// it guards is silent and would look like a flaky case rather than a collision.
static SCRATCH_NAMES: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn scratch_dir(name: &str) -> Result<PathBuf> {
    // Colons are invalid in Windows directory names (e.g. `Jvm:Foo`).
    let safe_name = name.replace(':', "_");
    if !SCRATCH_NAMES.lock().unwrap().insert(safe_name.clone()) {
        bail!("two regression cases claim the same scratch directory `{safe_name}`");
    }
    let dir = run_root().join(&safe_name);
    exec::fresh_dir(&dir)?;
    Ok(dir)
}
