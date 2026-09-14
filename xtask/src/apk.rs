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
    "apk:report",
    "apk:report-invariants",
    "apk:skip-existing",
];

// The store layout `ctadl` writes. Duplicated from `ctadl_import::project` rather than imported:
// xtask deliberately does not depend on the analyzer crate (see `xtask/Cargo.toml`), and these
// checks are *about* the on-disk contract anyway -- a path that moves should fail them loudly
// here rather than follow the analyzer silently.
const IMPORTS_DIR: &str = "imports";
const PROJECTS_DIR: &str = "projects";
const IMPORT_CONFIG_FILE: &str = "import_config.json";
const PROGRAM_BITCODE_FILE: &str = "ir-program.bitcode";
/// The `version` an import config carries today (`IMPORT_FORMAT_VERSION`). Pinned so a bump
/// that forgets the store's readers has to come through here.
const IMPORT_FORMAT_VERSION: &str = "8";

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
        to_outcome(check_report(work, &state, &store)),
        to_outcome(check_report_invariants(work, &state)),
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

/// `ctadl report` runs on the imported app, says which tier it ran at, and produces the JSON
/// a nightly job would diff.
///
/// The pinned counts are the substance. Everything else here would still pass if call
/// resolution silently changed what it resolves to, and these would not. They are a property
/// of this APK and of the Dex frontend, so a *deliberate* frontend change moves them: re-pin
/// by running `ctadl import -l apk --name app xtask/tests/dex/com.noto_54.apk` followed by
/// `ctadl report app --format json` and reading the four numbers back out, and say in the
/// commit message which frontend change moved them.
const REPORT_TOTAL_SITES: u64 = 178_310;
const REPORT_VIRTUAL_SITES: u64 = 99_551;
const REPORT_CHA_EDGES: u64 = 2_080_404;
const REPORT_RTA_EDGES: u64 = 2_026_676;

/// The sections this app's report must carry. `com.noto` is a Java APK, so every section
/// applies; a program with no class hierarchy would legitimately have only the first few.
const REPORT_SECTIONS: &[&str] = &[
    "census",
    "virtual_targets",
    "worst_signatures",
    "rta",
    "hard_cases",
    "kotlin_lambdas",
    "functional_interfaces",
    "fan_in",
    "recursion",
];

/// The dispatch kinds this APK's report must tell apart, with the site count pinned for each.
///
/// Pinned for the same reason as the four totals above and re-pinned the same way: these are
/// the numbers that would silently go to zero if a frontend stopped recording the dispatch
/// instruction, and every other assertion here would still pass. The three sum to
/// `REPORT_VIRTUAL_SITES`, which the invariants check independently.
const REPORT_DISPATCH_SITES: &[(&str, u64)] = &[
    ("virtual", 76_791),
    ("interface", 21_153),
    ("super", 1_607),
    ("unknown", 0),
];

fn check_report(work: &Path, state: &Path, store: &Path) -> Result<()> {
    // The text form first, because its opening line is the contract: a reader has to be able
    // to tell at a glance which tier produced the numbers.
    let text = capture(work, state, &["report", IMPORT])?;
    let first = text.lines().next().unwrap_or_default();
    ensure!(
        first.contains("static tier"),
        "the report's first line must name the tier it ran at, got: {first:?}"
    );

    let doc = report_json(work, state)?;
    ensure!(
        doc["tier"] == "static",
        "the JSON must name the tier too, got {}",
        doc["tier"]
    );
    let programs = doc["programs"]
        .as_array()
        .context("the report lists no programs")?;
    ensure!(
        programs.len() == 1,
        "this APK is one program with no native libraries, got {} program(s)",
        programs.len()
    );
    let p = &programs[0];
    ensure!(
        p["import"] == IMPORT,
        "the program section names {} rather than the import it measured",
        p["import"]
    );
    for section in REPORT_SECTIONS {
        ensure!(
            !p[section].is_null(),
            "a Java program's report must carry a `{section}` section; it has {:?}",
            p.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }

    // Nothing was indexed, so nothing may have been written. `index_path()` creates the
    // project directory as a side effect, which is exactly the mistake this catches.
    let project = store.join(PROJECTS_DIR).join(IMPORT);
    ensure!(
        !project.exists(),
        "the report wrote a project config: {}",
        project.display()
    );

    let census = &p["census"];
    let virt = &p["virtual_targets"];
    let pinned = [
        ("total call sites", &census["total"], REPORT_TOTAL_SITES),
        (
            "virtual call sites",
            &census["virtual"],
            REPORT_VIRTUAL_SITES,
        ),
        ("CHA edges", &virt["total_edges"], REPORT_CHA_EDGES),
        ("RTA edges", &p["rta"]["rta_edges"], REPORT_RTA_EDGES),
    ];
    for (what, got, want) in pinned {
        ensure!(
            got.as_u64() == Some(want),
            "{what}: the report says {got}, this APK has {want}. If a frontend change really \
             moved it, re-pin the constants in xtask/src/apk.rs (see their doc comment)"
        );
    }

    // The dispatch split. A frontend that stopped reading the invoke opcode would report
    // every virtual call under one kind, and nothing above would notice.
    let by_dispatch = &census["by_dispatch"];
    ensure!(
        !by_dispatch.is_null(),
        "a Java program's census must split its virtual calls by dispatch kind; it has {:?}",
        census.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    for (kind, want) in REPORT_DISPATCH_SITES {
        let got = &by_dispatch[kind];
        ensure!(
            got.as_u64() == Some(*want),
            "{kind} call sites: the report says {got}, this APK has {want}. Re-pin \
             REPORT_DISPATCH_SITES in xtask/src/apk.rs if a frontend change really moved it"
        );
    }
    Ok(())
}

/// The report's numbers have to add up, and two runs over one import have to agree.
///
/// These are the assertions that survive a re-pin: they hold for any program, so they catch
/// an arithmetic mistake in a section without anyone having to know what the right answer
/// for this APK is. Counts and sets only -- per `docs/debugging.md`, never a byte-diff of the
/// rendered text.
fn check_report_invariants(work: &Path, state: &Path) -> Result<()> {
    let doc = report_json(work, state)?;
    let p = &doc["programs"][0];
    let n = |v: &Value| -> Result<u64> {
        v.as_u64()
            .with_context(|| format!("expected a number, got {v}"))
    };

    let census = &p["census"];
    let total = n(&census["total"])?;
    let parts = ["direct", "virtual", "func_ptr", "lua", "unknown"]
        .iter()
        .map(|k| n(&census[k]))
        .sum::<Result<u64>>()?;
    ensure!(
        parts == total,
        "the call census does not add up: the kinds sum to {parts}, the total says {total}"
    );

    let virt = &p["virtual_targets"];
    let sites = n(&virt["sites"])?;
    ensure!(
        sites == n(&census["virtual"])?,
        "the virtual-target section counts {sites} sites, the census counts {}",
        census["virtual"]
    );
    let split = n(&virt["sites_with_zero_targets"])?
        + n(&virt["sites_with_one_target"])?
        + n(&virt["sites_deferred_to_hybrid_inlining"])?;
    ensure!(
        split == sites,
        "zero + one + many targets is {split} sites, but there are {sites}"
    );

    // Every distribution is monotone by construction; a broken weighted percentile is the
    // way that stops being true.
    for (name, d) in [
        ("targets_per_site", &virt["targets_per_site"]),
        ("gap_per_site", &p["rta"]["gap_per_site"]),
        ("calls_per_method", &p["fan_in"]["calls_per_method"]),
    ] {
        let (p50, p90, p99, max) = (n(&d["p50"])?, n(&d["p90"])?, n(&d["p99"])?, n(&d["max"])?);
        ensure!(
            p50 <= p90 && p90 <= p99 && p99 <= max,
            "{name} is not monotone: p50 {p50}, p90 {p90}, p99 {p99}, max {max}"
        );
        let mean = d["mean"].as_f64().context("a distribution has no mean")?;
        ensure!(
            mean <= max as f64,
            "{name} has mean {mean} above its max {max}"
        );
    }

    // RTA restricts CHA, so it can only ever keep fewer edges, and the two edge totals have
    // to be the same number counted in two places.
    let cha = n(&virt["total_edges"])?;
    ensure!(
        cha == n(&p["rta"]["cha_edges"])?,
        "the CHA edge total disagrees with itself: {cha} vs {}",
        p["rta"]["cha_edges"]
    );
    let rta = n(&p["rta"]["rta_edges"])?;
    ensure!(rta <= cha, "RTA kept {rta} edges where CHA found {cha}");
    ensure!(
        n(&p["rta"]["edges_dropped"])? == cha - rta,
        "the dropped-edge count is not the difference of the two totals"
    );

    // Each printed signature row is `sites * cha_targets` edges, and RTA never exceeds CHA
    // on any one of them. Both rankings, since they are built separately.
    for list in ["top_by_targets", "top_by_excess"] {
        let rows = p["worst_signatures"][list]
            .as_array()
            .with_context(|| format!("the `{list}` signature list is missing"))?;
        ensure!(!rows.is_empty(), "`{list}` is empty");
        for row in rows {
            ensure!(
                n(&row["edges"])? == n(&row["sites"])? * n(&row["cha_targets"])?,
                "a signature row's edge count is not sites x targets: {row}"
            );
            ensure!(
                n(&row["excess"])? == n(&row["sites"])? * n(&row["cha_targets"])?.saturating_sub(1),
                "a signature row's excess is not sites x (targets - 1): {row}"
            );
            ensure!(
                n(&row["rta_targets"])? <= n(&row["cha_targets"])?,
                "a signature row keeps more RTA targets than CHA ones: {row}"
            );
        }
        // Each list is sorted by the thing it ranks on.
        let key = if list == "top_by_excess" {
            "excess"
        } else {
            "cha_targets"
        };
        let mut prev = u64::MAX;
        for row in rows {
            let value = n(&row[key])?;
            ensure!(value <= prev, "`{list}` is not sorted by `{key}`: {row}");
            prev = value;
        }
    }

    let share = |k: &str| -> Result<f64> {
        p["worst_signatures"][k]
            .as_f64()
            .with_context(|| format!("{k} is not a number"))
    };
    for (ten, hundred) in [
        ("top_10_site_share", "top_100_site_share"),
        (
            "top_10_signature_excess_share",
            "top_100_signature_excess_share",
        ),
    ] {
        let (a, b) = (share(ten)?, share(hundred)?);
        ensure!(
            (0.0..=1.0).contains(&a) && a <= b && b <= 1.0,
            "the shares are not ordered: {ten} {a}, {hundred} {b}"
        );
    }
    // Excess is what is left of the edge total once each resolved site is granted the one
    // edge it must have.
    let resolved_sites = sites - n(&virt["sites_with_zero_targets"])?;
    ensure!(
        n(&p["worst_signatures"]["excess_edges"])? == cha - resolved_sites,
        "excess edges is {} but total edges minus resolved sites is {}",
        p["worst_signatures"]["excess_edges"],
        cha - resolved_sites
    );

    // Every Java call site lands in exactly one bucket of the call-resolution policy, pooled
    // and again per dispatch kind. This is the invariant `ctadl index` asserts on the fact
    // side; here it is checked on the numbers a user actually reads.
    let policy = &p["policy"];
    let buckets = |b: &Value| -> Result<(u64, u64)> {
        let sum = ["modelled", "skipped", "cha", "inlined"]
            .iter()
            .map(|k| n(&b[k]))
            .sum::<Result<u64>>()?;
        Ok((sum, n(&b["java_sites"])?))
    };
    let (sum, java_sites) = buckets(&policy["buckets"])?;
    ensure!(
        sum == java_sites,
        "the policy buckets sum to {sum} but there are {java_sites} java sites"
    );
    ensure!(
        java_sites == n(&census["virtual"])?,
        "the policy counts {java_sites} java sites, the census counts {}",
        census["virtual"]
    );
    let mut per_kind = 0u64;
    for row in policy["by_dispatch"]
        .as_array()
        .context("the policy section has no per-dispatch split")?
    {
        let (sum, sites) = buckets(row)?;
        ensure!(
            sum == sites,
            "the {} buckets sum to {sum} but that kind has {sites} sites",
            row["dispatch"]
        );
        per_kind += sites;
    }
    ensure!(
        per_kind == java_sites,
        "the dispatch kinds hold {per_kind} sites, the pooled count says {java_sites}"
    );
    // A sub-count lives inside its bucket rather than beside it.
    ensure!(
        n(&policy["buckets"]["cha_zero_targets"])? + n(&policy["buckets"]["cha_super_exact"])?
            <= n(&policy["buckets"]["cha"])?,
        "the CHA sub-counts exceed the bucket they are inside"
    );
    ensure!(
        n(&policy["buckets"]["inlined_by_model"])? <= n(&policy["buckets"]["inlined"])?,
        "more sites were inlined by a model than were inlined"
    );
    // The policy can only ever emit fewer edges than plain CHA: every rung either keeps the
    // CHA set, replaces it with one edge, or drops it.
    ensure!(
        n(&policy["cha_edges"])? == cha,
        "the policy section's CHA edge total disagrees with the virtual-target section: {} vs {cha}",
        policy["cha_edges"]
    );
    ensure!(
        n(&policy["policy_edges"])? <= cha,
        "the policy emits {} edges where plain CHA emits {cha}",
        policy["policy_edges"]
    );

    // The dispatch kinds partition the virtual sites: every `JavaCall` has exactly one, so
    // they sum to the census's virtual count and to the target section's site count. This is
    // what catches a site counted twice or dropped when the per-kind tables are built.
    let by_dispatch = &p["census"]["by_dispatch"];
    let kinds = ["virtual", "interface", "super", "unknown"];
    let dispatch_sum = kinds
        .iter()
        .map(|k| n(&by_dispatch[k]))
        .sum::<Result<u64>>()?;
    ensure!(
        dispatch_sum == sites,
        "the dispatch kinds sum to {dispatch_sum} sites, but there are {sites} virtual sites"
    );

    // Each per-kind row is the whole report recomputed over that kind's sites, so every
    // invariant that holds for the pooled numbers holds inside it, and the rows sum back.
    let rows = virt["by_dispatch"]
        .as_array()
        .context("the virtual-target section carries no per-dispatch breakdown")?;
    ensure!(
        !rows.is_empty(),
        "a Java program's virtual-target section must break down by dispatch kind"
    );
    let mut row_sites = 0;
    let mut row_edges = 0;
    for row in rows {
        let kind = row["dispatch"]
            .as_str()
            .with_context(|| format!("a per-dispatch row has no kind: {row}"))?;
        ensure!(
            kinds.contains(&kind),
            "a per-dispatch row names an unknown kind {kind:?}"
        );
        let s = n(&row["sites"])?;
        ensure!(
            s == n(&by_dispatch[kind])?,
            "the {kind} row counts {s} sites, the census counts {}",
            by_dispatch[kind]
        );
        ensure!(
            n(&row["sites_with_zero_targets"])?
                + n(&row["sites_with_one_target"])?
                + n(&row["sites_deferred_to_hybrid_inlining"])?
                == s,
            "zero + one + many does not account for the {kind} row's sites: {row}"
        );
        let d = &row["targets_per_site"];
        let (p50, p90, p99, max) = (n(&d["p50"])?, n(&d["p90"])?, n(&d["p99"])?, n(&d["max"])?);
        ensure!(
            p50 <= p90 && p90 <= p99 && p99 <= max,
            "the {kind} row's distribution is not monotone: p50 {p50}, p90 {p90}, p99 {p99}, max {max}"
        );
        row_sites += s;
        row_edges += n(&row["total_edges"])?;
    }
    ensure!(
        row_sites == sites && row_edges == cha,
        "the per-dispatch rows account for {row_sites} sites and {row_edges} edges, \
         the pooled numbers say {sites} and {cha}"
    );

    // The same partition again on the two other sections that split: the excess and the RTA
    // edge totals are each counted once per kind and once pooled.
    let excess_sum = p["worst_signatures"]["by_dispatch"]
        .as_array()
        .context("the worst-signature section carries no per-dispatch breakdown")?
        .iter()
        .map(|row| n(&row["excess_edges"]))
        .sum::<Result<u64>>()?;
    ensure!(
        excess_sum == n(&p["worst_signatures"]["excess_edges"])?,
        "the per-kind excess sums to {excess_sum}, the pooled total says {}",
        p["worst_signatures"]["excess_edges"]
    );
    let rta_rows = p["rta"]["by_dispatch"]
        .as_array()
        .context("the RTA section carries no per-dispatch breakdown")?;
    let (mut rta_cha, mut rta_rta) = (0, 0);
    for row in rta_rows {
        ensure!(
            n(&row["rta_edges"])? <= n(&row["cha_edges"])?,
            "a per-kind RTA row keeps more edges than CHA found: {row}"
        );
        rta_cha += n(&row["cha_edges"])?;
        rta_rta += n(&row["rta_edges"])?;
    }
    ensure!(
        rta_cha == cha && rta_rta == rta,
        "the per-kind RTA rows sum to {rta_cha}/{rta_rta} edges, the pooled totals say {cha}/{rta}"
    );

    // Removing edges can only shrink the graph, never grow it. That is the whole content of
    // the interface-free counterfactual, and it is checkable without knowing this APK.
    if let Some(cv) = p["recursion"]["without_interface_edges"].as_object() {
        let full = &p["recursion"];
        // Not `nontrivial_sccs`: that one is genuinely free to *rise*. Cutting edges can
        // break one huge component into several smaller ones, which is more components and
        // fewer functions inside them -- the shape the fixture actually shows.
        for key in [
            "edges",
            "self_recursive",
            "functions_in_nontrivial_sccs",
            "largest_scc",
        ] {
            let (part, whole) = (n(&cv[key])?, n(&full[key])?);
            ensure!(
                part <= whole,
                "the graph without interface edges has more `{key}` ({part}) than the whole \
                 graph ({whole})"
            );
        }
    }

    // Reproducible: the tables the report is built from are documented as byte-stable, so
    // two runs over one import must agree exactly. This is the assertion `docs/debugging.md`
    // says to make, and it is safe to make here because nothing post-fixpoint is involved.
    let again = report_json(work, state)?;
    ensure!(
        again == doc,
        "two reports over the same import disagree; the report is not reproducible"
    );
    Ok(())
}

fn report_json(work: &Path, state: &Path) -> Result<Value> {
    let text = capture(work, state, &["report", IMPORT, "--format", "json"])?;
    serde_json::from_str(&text)
        .with_context(|| format!("`ctadl report --format json` emitted invalid JSON:\n{text}"))
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
        assert_eq!(CHECKS.len(), 6, "CHECKS and run_checks must stay in step");
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
