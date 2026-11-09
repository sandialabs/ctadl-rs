//! Orchestration of the regression suite: drive the analyzer per case and check
//! its output against the case's known answer.
//!
//! Each case runs in its own scratch directory so that ctadl project state
//! (kept under `XDG_STATE_HOME`) and build artifacts never collide between
//! cases. A single failing case does not abort the run: we execute every
//! selected case and report them all.
//!
//! Cases are independent, so they run on a pool of `--jobs` workers rather than
//! one after another. The two things that are *not* per-case -- the scratch
//! directory and Ghidra's user directories -- are made per-worker or per-case
//! explicitly; see [`Worker`] and [`scratch_dir`].

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::apk;
use crate::cil;
use crate::dex;
use crate::discovery::{self, Frontend, Kind, TestCase};
use crate::exec;
use crate::jvm;

pub struct Options {
    pub filter: Option<String>,
    /// Frontends to exercise. Defaults to all of them.
    pub frontends: BTreeSet<Frontend>,
    pub tests_dir: Option<PathBuf>,
    pub jvm_samples: Option<PathBuf>,
    pub dex_apk: Option<PathBuf>,
    /// The committed Cecil goldens. `None` auto-detects; see
    /// [`cil::resolve_goldens`].
    pub cil_goldens: Option<PathBuf>,
    /// The Cecil checkout whose test assemblies the `cil:*` checks read.
    /// `None` auto-detects; see [`cil::resolve_cecil_src`].
    pub cecil_src: Option<PathBuf>,
    /// The compiled cil-reader fixtures. `None` auto-detects; see
    /// [`cil::resolve_fixtures`].
    pub cil_fixtures: Option<PathBuf>,
    /// How many cases to run concurrently. `None` picks [`default_jobs`].
    pub jobs: Option<usize>,
    /// Build (and run) the release `ctadl` binary instead of the debug one.
    /// Either way the binary is rebuilt from current source before any case
    /// runs; see [`build_ctadl`].
    pub release: bool,
    /// The `--mem2reg` policy every `ctadl index` runs with; see [`ctadl_command`]. `None`
    /// runs the default.
    pub mem2reg: Option<String>,
    /// The scratch root of an earlier run (`$TMPDIR/ctadl_xtask_<pid>`). When set, every
    /// index this run writes has its `paths.parquet` compared with the same case's in that run,
    /// and a difference fails the run; see [`compare_paths`].
    pub compare_paths: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            filter: None,
            frontends: Frontend::ALL.iter().copied().collect(),
            tests_dir: None,
            jvm_samples: None,
            dex_apk: None,
            cil_goldens: None,
            cecil_src: None,
            cil_fixtures: None,
            jobs: None,
            release: false,
            mem2reg: None,
            compare_paths: None,
        }
    }
}

/// Upper bound on the default worker count.
///
/// A case is mostly spent inside external processes that are already parallel on
/// their own (Ghidra's JVM, ctadl's rayon pool), so workers stop buying wall clock
/// well before they stop buying cores: wall clock falls steeply up to a handful of
/// jobs and flattens out past about eight. Past this the extra Ghidra JVMs -- each
/// asking for a large heap -- cost more memory than the seconds are worth. Raise it
/// with `--jobs` on a machine with headroom to spare.
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
}

/// Run the suite. Returns `Ok(true)` if every selected case passed or was
/// skipped, `Ok(false)` if any failed.
pub fn run(opts: &Options) -> Result<bool> {
    MEM2REG
        .set(opts.mem2reg.clone())
        .map_err(|_| anyhow::anyhow!("internal error: regression suite run more than once"))?;
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

    // The `apk:*` checks drive `ctadl`, so preflight has to build it for them as
    // well -- `--filter apk` selects no cases at all, and without this the
    // binary would never be built and every check would report the internal
    // error from [`ctadl_bin`].
    let apk_selected = opts.frontends.contains(&Frontend::Dex)
        && opts
            .filter
            .as_ref()
            .is_none_or(|f| apk::CHECKS.iter().any(|name| name.contains(f.as_str())));

    // ctadl/dex-reader/jvm-reader are only needed for the matching case kinds.
    if !cases.is_empty() || ghidra_selected || apk_selected {
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
    // The APK is read by two independent families: `dex:apk`, which parses it
    // with `dex-reader` alongside the compiled samples, and the `apk:*`
    // end-to-end checks, which only need `ctadl`. So it is resolved for `dex`
    // alone, whether or not the Java samples resolved: the `apk:*` checks must
    // not vanish on a machine with no sample sources.
    let dex_apk = if wants_dex {
        resolve_dex_apk(opts.dex_apk.as_deref())?
    } else {
        None
    };
    let wants_cil = opts.frontends.contains(&Frontend::Cil);
    let (cil_goldens, cecil_src, cil_fixtures) = if wants_cil {
        let goldens = cil::resolve_goldens(opts.cil_goldens.as_deref())?;
        // Only for a Cecil checkout cloned by hand into its `extern/cecil`.
        let cil_dir = cil::resolve_cil_dir(None)?;
        let cecil = cil::resolve_cecil_src(opts.cecil_src.as_deref(), cil_dir.as_deref())?;
        let fixtures = cil::resolve_fixtures(opts.cil_fixtures.as_deref())?;
        (goldens, cecil, fixtures)
    } else {
        (None, None, None)
    };

    // Assemble every independent unit of work, in report order. The pool below
    // preserves that order regardless of which worker finishes first.
    let mut tasks: Vec<Task<'_>> = cases.iter().map(Task::Case).collect();
    let java_cases = tests_dir.join("java");
    if let Some(samples) = samples_dir.as_deref().filter(|_| wants_jvm) {
        tasks.push(Task::JvmChecks {
            samples,
            cases: Some(java_cases.as_path()).filter(|dir| dir.is_dir()),
        });
    }
    if let Some(samples) = samples_dir.as_deref().filter(|_| wants_dex) {
        tasks.push(Task::DexChecks {
            samples,
            apk: dex_apk.as_deref(),
        });
    }
    if let Some(apk) = dex_apk.as_deref().filter(|_| wants_dex) {
        tasks.push(Task::ApkChecks { apk });
        tasks.push(Task::ApkIndexCheck { apk });
    }
    // Ghidra existing-project smoke check: drive the pcode importer against an
    // *existing* Ghidra project (not a fresh binary). Only meaningful when Ghidra
    // and a target binary are available (guaranteed under the Nix regression env),
    // and self-skips otherwise, so it is cheap to always attempt.
    if ghidra_selected {
        tasks.push(Task::GhidraProject);
    }
    // Always queued when `cil` is selected, so a missing corpus or CecilDump
    // reports as a Skip rather than vanishing from the report.
    if wants_cil {
        tasks.push(Task::CilChecks {
            goldens: cil_goldens.as_deref(),
            cecil: cecil_src.as_deref(),
            fixtures: cil_fixtures.as_deref(),
        });
        tasks.push(Task::CilFrontend {
            goldens: cil_goldens.as_deref(),
            cecil: cecil_src.as_deref(),
            fixtures: cil_fixtures.as_deref(),
        });
    }

    // More workers than tasks would just create idle scratch homes; no tasks at
    // all means no workers, and the empty report below reports the selection.
    let jobs = opts.jobs.unwrap_or_else(default_jobs).min(tasks.len());
    let (results, mut timings) = run_tasks(&tasks, opts, jobs)?;

    if results.is_empty() {
        bail!("no test cases selected ({})", describe_selection(opts));
    }

    println!(
        "Ran {} regression case(s) (tests from {}, {}, {jobs} job(s))",
        results.len(),
        tests_dir.display(),
        describe_selection(opts)
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
    timings.sort_by_key(|(_, elapsed)| std::cmp::Reverse(*elapsed));
    let slowest: Vec<String> = timings
        .iter()
        .take(SLOWEST_SHOWN)
        .map(|(label, elapsed)| format!("{label} {:.1}s", elapsed.as_secs_f64()))
        .collect();
    println!("Slowest: {}", slowest.join(", "));
    println!("Scratch root: {}", run_root().display());
    let paths_equal = match &opts.compare_paths {
        Some(baseline) => compare_paths(baseline, &run_root())?,
        None => true,
    };
    Ok(failures == 0 && paths_equal)
}

/// Compares the admissible path set of every index under `run` with the same case's under
/// `baseline`, both scratch roots of a regression run, and prints each difference. Returns
/// whether they all agree.
///
/// Both files are read through the current `facts::Path` parser, so an older run's paths are
/// compared in today's normal form. This is the check that makes "no changed answers" mean
/// something across a change to the IR's statements: the labeling does not depend on how a
/// path is spelled, but the admissible set is built from program paths, so it can.
pub fn compare_paths(baseline: &Path, run: &Path) -> Result<bool> {
    use ctadl_ascent::facts::{Path as FactPath, schema};
    fn indexes(root: &Path) -> Result<BTreeMap<PathBuf, PathBuf>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name() == Some(schema::paths::FILENAME.as_ref()) {
                    let index = path.parent().expect("a file has a parent").to_path_buf();
                    let rel = index.strip_prefix(root).unwrap_or(&index).to_path_buf();
                    out.insert(rel, index);
                }
            }
        }
        Ok(out)
    }
    let load = |dir: &Path| -> Result<BTreeSet<String>> {
        Ok(schema::paths::try_load(dir)?
            .into_iter()
            .map(|(p,): (FactPath,)| p.to_dot_string())
            .collect())
    };
    let base = indexes(baseline)?;
    let ours = indexes(run)?;
    let mut equal = true;
    let mut compared = 0;
    for (rel, dir) in &ours {
        let Some(base_dir) = base.get(rel) else {
            println!("  PATHS new index, no baseline: {}", rel.display());
            continue;
        };
        compared += 1;
        let (a, b) = (load(base_dir)?, load(dir)?);
        if a != b {
            equal = false;
            let lost: Vec<&String> = a.difference(&b).take(8).collect();
            let gained: Vec<&String> = b.difference(&a).take(8).collect();
            println!(
                "  PATHS differ: {} ({} vs {} paths)\n        lost: {lost:?}\n        gained: {gained:?}",
                rel.display(),
                a.len(),
                b.len()
            );
        }
    }
    println!(
        "Compared the path sets of {compared} index(es) against {}: {}",
        baseline.display(),
        if equal { "all equal" } else { "DIFFERENT" }
    );
    Ok(equal)
}

/// How many of the slowest tasks the report names, so a slow case is noticed before it is the
/// one that sets the suite's wall clock.
const SLOWEST_SHOWN: usize = 8;

/// One independent unit of work. Each yields zero or more report entries.
///
/// The reader checks are single tasks rather than one per check: they share the
/// compiled samples, so splitting them would recompile the same sources N times.
enum Task<'a> {
    Case(&'a TestCase),
    JvmChecks {
        samples: &'a Path,
        cases: Option<&'a Path>,
    },
    DexChecks {
        samples: &'a Path,
        apk: Option<&'a Path>,
    },
    /// End-to-end checks driving `ctadl` over a real-world Android app; see
    /// [`crate::apk`]. Separate from [`Task::DexChecks`] even though both read
    /// the same APK: those parse it with `dex-reader`, these import it with the
    /// analyzer, and only the former needs the Java toolchain.
    ApkChecks {
        apk: &'a Path,
    },
    /// `apk:manifest-and-intents`, which indexes the app; see [`apk::run_index_check`].
    ApkIndexCheck {
        apk: &'a Path,
    },
    /// cil-reader's Cecil parity checks; see [`crate::cil`].
    CilChecks {
        goldens: Option<&'a Path>,
        cecil: Option<&'a Path>,
        fixtures: Option<&'a Path>,
    },
    /// `cil:frontend`; see [`cil::frontend_check`].
    CilFrontend {
        goldens: Option<&'a Path>,
        cecil: Option<&'a Path>,
        fixtures: Option<&'a Path>,
    },
    GhidraProject,
}

impl Task<'_> {
    /// Whether workers take this task before the single cases. These each run many checks or one
    /// long one, so started last they would run on after everything else had finished.
    fn is_long(&self) -> bool {
        !matches!(self, Task::Case(_))
    }

    /// The task's name in the timing summary.
    fn label(&self) -> String {
        match self {
            Task::Case(case) => case.name.clone(),
            Task::JvmChecks { .. } => "jvm checks".to_string(),
            Task::DexChecks { .. } => "dex checks".to_string(),
            Task::ApkChecks { .. } => "apk checks".to_string(),
            Task::ApkIndexCheck { .. } => "apk index check".to_string(),
            Task::CilChecks { .. } => "cil checks".to_string(),
            Task::CilFrontend { .. } => "cil:frontend".to_string(),
            Task::GhidraProject => GHIDRA_PROJECT_CASE.to_string(),
        }
    }

    /// Run this task to completion. Infrastructure failures (analyzer crashed,
    /// tool missing, etc.) are reported as failed entries rather than propagated,
    /// so one broken case never aborts the run.
    fn run(&self, worker: &Worker, opts: &Options) -> Vec<(String, Outcome)> {
        match self {
            Task::Case(case) => {
                let outcome =
                    run_case(case, worker).unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")));
                vec![(case.name.clone(), outcome)]
            }
            Task::JvmChecks { samples, cases } => {
                let results = scratch_dir("jvm")
                    .and_then(|work| jvm::run_checks(samples, *cases, &work))
                    .unwrap_or_else(|err| {
                        vec![("jvm".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::DexChecks { samples, apk } => {
                let results = scratch_dir("dex")
                    .and_then(|work| dex::run_checks(samples, *apk, &work))
                    .unwrap_or_else(|err| {
                        vec![("dex".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::ApkChecks { apk } => {
                let results = scratch_dir("apk")
                    .and_then(|work| apk::run_checks(apk, &work))
                    .unwrap_or_else(|err| {
                        vec![("apk:import".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::ApkIndexCheck { apk } => {
                let results = scratch_dir("apk-index")
                    .and_then(|work| apk::run_index_check(apk, &work))
                    .unwrap_or_else(|err| {
                        vec![(
                            "apk:manifest-and-intents".to_string(),
                            Outcome::Fail(format!("{err:#}")),
                        )]
                    });
                retain_filtered(results, opts)
            }
            Task::CilChecks {
                goldens,
                cecil,
                fixtures,
            } => {
                let results = scratch_dir("cil")
                    .and_then(|work| cil::run_checks(*goldens, *cecil, *fixtures, &work))
                    .unwrap_or_else(|err| {
                        vec![("cil".to_string(), Outcome::Fail(format!("{err:#}")))]
                    });
                retain_filtered(results, opts)
            }
            Task::CilFrontend {
                goldens,
                cecil,
                fixtures,
            } => {
                let results = scratch_dir("cil-frontend")
                    .and_then(|work| cil::frontend_check(*goldens, *cecil, *fixtures, &work))
                    .unwrap_or_else(|err| {
                        vec![(
                            "cil:frontend".to_string(),
                            Outcome::Fail(format!("{err:#}")),
                        )]
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

/// Apply `--filter` to reader-check results. Unlike the cases, these names
/// are only known once the checks have run, so they are filtered on the way out.
fn retain_filtered(mut results: Vec<(String, Outcome)>, opts: &Options) -> Vec<(String, Outcome)> {
    if let Some(filter) = &opts.filter {
        results.retain(|(name, _)| name.contains(filter));
    }
    results
}

/// Run `tasks` across `jobs` workers, returning every result in task order, and how long each
/// task took.
///
/// Each worker pulls the next unclaimed task until the list is exhausted, so a
/// slow case (a Ghidra import) never blocks the queue behind it. The long tasks
/// ([`Task::is_long`]) are pulled first, so the suite does not end waiting on one
/// that started late. Results are
/// tagged with their task index and sorted at the end rather than appended as
/// they land, so the report reads identically no matter who finished first.
fn run_tasks(
    tasks: &[Task<'_>],
    opts: &Options,
    jobs: usize,
) -> Result<(Vec<(String, Outcome)>, Vec<(String, Duration)>)> {
    let workers = (0..jobs).map(Worker::new).collect::<Result<Vec<_>>>()?;

    let mut order: Vec<usize> = (0..tasks.len()).collect();
    order.sort_by_key(|&index| !tasks[index].is_long());
    let order = &order;
    let next = &AtomicUsize::new(0);
    let results = &Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for worker in &workers {
            scope.spawn(move || {
                loop {
                    let Some(&index) = order.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let task = &tasks[index];
                    let start = Instant::now();
                    let outcomes = task.run(worker, opts);
                    let elapsed = start.elapsed();
                    // Held only to push a finished result; no test work happens
                    // under the lock.
                    results.lock().unwrap().push((index, outcomes, elapsed));
                }
            });
        }
    });

    let mut collected = std::mem::take(&mut *results.lock().unwrap());
    collected.sort_by_key(|(index, _, _)| *index);
    let timings = collected
        .iter()
        .map(|(index, _, elapsed)| (tasks[*index].label(), *elapsed))
        .collect();
    let outcomes = collected
        .into_iter()
        .flat_map(|(_, outcomes, _)| outcomes)
        .collect();
    Ok((outcomes, timings))
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
    [
        "readers/jvm-reader/tests/sample",
        "../readers/jvm-reader/tests/sample",
    ]
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
/// current source and records its path, so it's not stale. The reader programs
/// (`dex-reader`/`jvm-reader`) are cargo *examples* rather than bins, so that they stay out of the
/// distro; they come from `PATH` (the Nix `testEnv` builds them) and are still just
/// presence-checked here. To supply them by hand: `cargo build --example dex-reader` and put
/// `target/debug/examples` on `PATH`.
fn preflight(cases: &[TestCase], ghidra_selected: bool, release: bool) -> Result<()> {
    let bin = prebuilt_ctadl()?.map_or_else(|| build_ctadl(release), Ok)?;
    // First (and only) writer; a later worker reading it back via `ctadl_bin`
    // sees this value.
    CTADL_BIN
        .set(bin)
        .map_err(|_| anyhow::anyhow!("internal error: ctadl binary built more than once"))?;
    let needs_pcode = ghidra_selected
        || cases
            .iter()
            .any(|c| matches!(c.kind, Kind::PcodeFacts { .. }));
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
         Enter the dev shell (`nix develop`), which supplies one.",
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
         Enter the dev shell (`nix develop`), which supplies a JDK.",
        java.display(),
        out.status
            .code()
            .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
        stderr.trim().replace('\n', "\n  "),
    );
}

fn run_case(case: &TestCase, worker: &Worker) -> Result<Outcome> {
    match &case.kind {
        Kind::PcodeFacts { source } => run_pcode_facts(&case.name, source, worker),
    }
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
/// test is actually broken. The only path that can be returned is the one
/// [`build_ctadl`] just built from the current tree, or the one `$CTADL_BIN`
/// names (see [`prebuilt_ctadl`]).
pub(crate) fn ctadl_bin() -> Result<PathBuf> {
    CTADL_BIN.get().cloned().context(
        "internal error: ctadl binary requested before it was built; preflight must run first",
    )
}

/// The `--mem2reg` policy from [`Options::mem2reg`]. Set once by [`run`] before any worker starts.
static MEM2REG: OnceLock<Option<String>> = OnceLock::new();

/// `ctadl <args>` with [`ctadl_bin`]; see [`ctadl_command_with`].
pub(crate) fn ctadl_command<S: AsRef<OsStr>>(args: &[S]) -> Result<Command> {
    Ok(ctadl_command_with(&ctadl_bin()?, args))
}

/// `<ctadl> <args>`. A `ctadl index` is also passed the suite's `--mem2reg` policy, if one was
/// given. Every case spawns `ctadl` through here, so none misses the policy.
pub(crate) fn ctadl_command_with<S: AsRef<OsStr>>(ctadl: &Path, args: &[S]) -> Command {
    let mut cmd = Command::new(ctadl);
    cmd.args(args);
    if let Some(Some(policy)) = MEM2REG.get()
        && args.first().is_some_and(|a| a.as_ref() == "index")
    {
        cmd.arg("--mem2reg").arg(policy);
    }
    cmd
}

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
pub(crate) fn prebuilt_ctadl() -> Result<Option<PathBuf>> {
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

/// Build the `ctadl` binary from the current source tree and return the path to
/// the executable cargo produced.
///
/// The binary is rebuilt in this same invocation, so it is guaranteed to
/// correspond to the tree under test (see [`ctadl_bin`]). We parse cargo's
/// `--message-format=json` stream and take the `executable` path from the `ctadl`
/// compiler-artifact message, so there is no guessing about the profile directory
/// or a `.exe` suffix: cargo tells us exactly where it wrote the binary.
pub(crate) fn build_ctadl(release: bool) -> Result<PathBuf> {
    // `cargo` sets `$CARGO` for the subcommands it spawns (`cargo xtask` is one),
    // so prefer it; fall back to the bare name for a direct `xtask` invocation.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let profile = if release { "release" } else { "debug" };
    println!("Building ctadl ({profile}) before running the suite...");

    let mut cmd = Command::new(cargo);
    // Run from the workspace root regardless of the caller's cwd. The xtask crate
    // lives at `<root>/xtask`, so its manifest dir's parent is the root.
    //
    // `--workspace` rather than `-p ctadl-ascent`: cargo resolves features across the packages it
    // selects, so a `-p` build can enable different features on a shared dependency, which
    // changes its hash and that of every crate above it, and compile its own copies instead of
    // reusing the workspace build's.
    cmd.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().context(
        "internal error: CARGO_MANIFEST_DIR has no parent; expected `<workspace>/xtask`",
    )?)
    .args([
        "build",
        "--message-format=json",
        "--workspace",
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
        if msg["reason"] == "compiler-artifact"
            && msg["target"]["name"] == "ctadl"
            && let Some(path) = msg["executable"].as_str()
        {
            exe = Some(PathBuf::from(path));
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

// --- Pcode facts ------------------------------------------------------------

/// Compiles `source` and exports it with Ghidra, then checks the facts with
/// [`check_pcode_facts`].
fn run_pcode_facts(name: &str, source: &Path, worker: &Worker) -> Result<Outcome> {
    let work = scratch_dir(name)?;
    let state = work.join("state");
    std::fs::create_dir_all(&state)?;

    let (cc, _) = pick_toolchain();
    let obj = work.join("case.o");
    let mut compile = Command::new(&cc);
    compile
        .current_dir(&work)
        .args(["-g", "-O0", "-c"])
        .arg(source)
        .arg("-o")
        .arg(&obj);
    exec::run_checked(compile, &cc)?;

    run_ctadl_env(
        &work,
        &state,
        &worker.ghidra_env,
        &[
            "import",
            "-l",
            "pcode",
            &obj.to_string_lossy(),
            "-n",
            "case",
        ],
    )?;
    check_pcode_facts(&state.join("ctadl/imports/case/facts"))
}

/// Pass criterion for a [`Kind::PcodeFacts`] case: the export gives each of a function's
/// high variables its own id, and the case has some that would share one if ids were names.
///
/// The reader keeps one type per high variable, so variables that share an id share a type,
/// and which one they get depends on the order the export wrote the rows in.
fn check_pcode_facts(facts: &Path) -> Result<Outcome> {
    let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in fact_rows(facts, "HVAR_TYPE")? {
        if let [hvar, ty] = &row[..] {
            types.entry(hvar.clone()).or_default().insert(ty.clone());
        }
    }
    let several: Vec<_> = types.iter().filter(|(_, tys)| tys.len() > 1).collect();
    if !several.is_empty() {
        return Ok(Outcome::Fail(format!(
            "high variables with several types: {several:?}"
        )));
    }

    // A varnode's name is its high variable's name, a colon and its number.
    let names: BTreeMap<String, String> = fact_rows(facts, "VNODE_NAME")?
        .into_iter()
        .filter_map(|row| match &row[..] {
            [vnode, name] => Some((vnode.clone(), name.rsplit_once(':')?.0.to_string())),
            _ => None,
        })
        .collect();
    // (function, name) -> the types of the high variables of that name.
    let mut by_name: BTreeMap<(String, String), BTreeMap<String, Option<String>>> = BTreeMap::new();
    for row in fact_rows(facts, "VNODE_HVAR")? {
        let [vnode, hvar] = &row[..] else { continue };
        let (Some(name), Some((func, _))) = (names.get(vnode), hvar.split_once(':')) else {
            continue;
        };
        let ty = types.get(hvar).and_then(|tys| tys.first().cloned());
        by_name
            .entry((func.to_string(), name.clone()))
            .or_default()
            .insert(hvar.clone(), ty);
    }
    let shared = by_name
        .values()
        .any(|hvars| hvars.len() > 1 && hvars.values().collect::<BTreeSet<_>>().len() > 1);
    if !shared {
        return Ok(Outcome::Fail(
            "no two high variables of a function share a name and differ in type, so the case \
             does not test what it is for"
                .to_string(),
        ));
    }
    Ok(Outcome::Pass)
}

/// The rows of the table `table` in the facts directory `facts`, gzipped or not.
fn fact_rows(facts: &Path, table: &str) -> Result<Vec<Vec<String>>> {
    use std::io::Read;
    let plain = facts.join(format!("{table}.facts"));
    let text = if plain.is_file() {
        std::fs::read_to_string(&plain).with_context(|| format!("reading {}", plain.display()))?
    } else {
        let gz = facts.join(format!("{table}.facts.gz"));
        let file = std::fs::File::open(&gz).with_context(|| format!("opening {}", gz.display()))?;
        let mut text = String::new();
        flate2::read::MultiGzDecoder::new(file)
            .read_to_string(&mut text)
            .with_context(|| format!("reading {}", gz.display()))?;
        text
    };
    Ok(text
        .lines()
        .map(|line| line.split('\t').map(str::to_string).collect())
        .collect())
}

/// Pick the compiler and `addr2line`: prefer an x86_64 Linux (cross-)compiler,
/// fall back to the native tools.
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
    let mut cmd = ctadl_command(args)?;
    cmd.current_dir(work).env("XDG_STATE_HOME", state);
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

        // Ghidra also needs a writable HOME, and the ambient one may be unset,
        // read-only, or `/var/empty` under a Nix sandbox. This purpose-built one is
        // writable by construction.
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
        if std::env::var_os("JAVA_HOME").is_none()
            && let Some(java_home) = guess_java_home()
        {
            ghidra_env.push(("JAVA_HOME".to_string(), java_home.display().to_string()));
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
/// means one deletes the other's working tree, possibly while the other is still
/// working in it. The names
/// are distinct today; this keeps that a checked invariant rather than a
/// coincidence of what the test directories happen to contain, since the failure
/// it guards is silent and would look like a flaky case rather than a collision.
static SCRATCH_NAMES: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn scratch_dir(name: &str) -> Result<PathBuf> {
    let dir = scratch_path(name);
    if !SCRATCH_NAMES
        .lock()
        .unwrap()
        .insert(dir.display().to_string())
    {
        bail!(
            "two regression cases claim the same scratch directory `{}`",
            dir.display()
        );
    }
    exec::fresh_dir(&dir)?;
    Ok(dir)
}

/// Where [`scratch_dir`] puts the work of the case `name`.
fn scratch_path(name: &str) -> PathBuf {
    // Colons are invalid in Windows directory names (e.g. `Jvm:Foo`).
    run_root().join(name.replace(':', "_"))
}
