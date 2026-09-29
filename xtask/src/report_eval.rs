//! `cargo xtask report-eval` -- run `ctadl report` across a corpus of real apps.
//!
//! This is the harness `spec.md` §7.3 asks for, and its job is questions 2 to 4 of §7.2:
//! are the numbers stable, do the distributions have the long tail the intent predicts, and
//! how concentrated is the imprecision. (Question 1, whether the static tier survives a
//! 200 MB app, was answered before any of this existed, with `ctadl import` and
//! `ctadl inspect` alone.)
//!
//! It takes a **directory** of artifacts rather than a hard-coded path, deliberately. The
//! large apps the intent names live on one machine, with no manifest, no hashes and no
//! licence to redistribute, so nothing here may depend on them: point it at that directory,
//! or at a TaintBench app tree, or at anything else. Any number quoted from a private corpus
//! has to name the exact file it came from, which is why the summary table is keyed by
//! filename.
//!
//! **Keeping the per-app JSON is the point.** The table is a convenience; the JSON is what
//! lets a later change be compared against today's numbers instead of re-argued from memory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::exec;
use crate::regression::{build_ctadl, prebuilt_ctadl};

pub struct Options {
    /// Directory of artifacts to sweep. Every regular file in it is offered to `ctadl
    /// import`, which detects the language itself.
    pub apks: PathBuf,
    /// Where the per-app JSON, logs and scratch stores go.
    pub out: PathBuf,
    /// Only sweep artifacts whose filename contains this.
    pub filter: Option<String>,
    /// Rows in each report's worst-signature and fan-in lists.
    pub top: usize,
    /// Exercise the release binary rather than the debug one. Worth it: this sweep is
    /// minutes of analysis, not seconds of process startup.
    pub release: bool,
    /// Also import an APK's native libraries. Off by default -- they go through Ghidra, which
    /// is a different (and far larger) measurement than the Dex call graph.
    pub native_libs: bool,
    /// Keep each app's scratch store instead of deleting it once its report is written. A
    /// 200 MB app imports to a couple of gigabytes, so a whole corpus adds up.
    pub keep_stores: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            apks: PathBuf::new(),
            out: PathBuf::from("target/report-eval"),
            filter: None,
            top: 10,
            release: true,
            native_libs: false,
            keep_stores: false,
        }
    }
}

/// The dispatch split, for the second summary table.
///
/// Site counts are summed over an artifact's programs the way every other count here is. The
/// percentile and share columns come from the largest program alone, for the reason
/// [`summarize`] gives: merging two programs' percentiles would need their full tables.
#[derive(Default)]
struct Dispatch {
    interface_sites: u64,
    virtual_sites: u64,
    super_sites: u64,
    /// Monomorphic share, per kind. The headline comparison.
    interface_one_target: f64,
    virtual_one_target: f64,
    interface_p99: u64,
    virtual_p99: u64,
    /// Share of the program's excess edges owed to interface-dispatched sites.
    interface_excess_share: f64,
    /// Functions inside a cycle, with every interface edge in place and with none.
    in_cycles: u64,
    in_cycles_without_interfaces: Option<u64>,
}

/// One app's measurements, as the summary table prints them.
struct Row {
    artifact: String,
    /// `None` when the app failed; the message says where.
    outcome: Result<Measured>,
}

struct Measured {
    functions: u64,
    sites: u64,
    virtual_sites: u64,
    one_target: u64,
    zero_target: u64,
    cha_edges: u64,
    rta_edges: u64,
    p50: u64,
    p99: u64,
    max_targets: u64,
    /// Share of all call edges owned by the ten worst call *sites*.
    top10: f64,
    top100: f64,
    /// Share of the *excess* edges -- those beyond the one each resolved site must have --
    /// owned by the ten signatures contributing the most of it. The actionable one: a
    /// signature is what you would special-case.
    sig10: f64,
    sig100: f64,
    /// The interface-versus-class-virtual split, which gets its own table: it is the
    /// comparison the whole of phase 2 exists to make, and hanging six more columns off the
    /// table above would bury it.
    dispatch: Dispatch,
    import_secs: f64,
    report_secs: f64,
    import_peak_mb: Option<f64>,
    report_peak_mb: Option<f64>,
    /// Whether a second report over the same import produced identical bytes.
    stable: bool,
}

/// Returns `Ok(true)` when every app measured cleanly.
pub fn run(opts: &Options) -> Result<bool> {
    if !opts.apks.is_dir() {
        bail!(
            "--apks must name a directory of artifacts; `{}` is not one",
            opts.apks.display()
        );
    }
    let bin = prebuilt_ctadl()?.map_or_else(|| build_ctadl(opts.release), Ok)?;

    let json_dir = opts.out.join("json");
    let log_dir = opts.out.join("logs");
    for dir in [&json_dir, &log_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let artifacts = discover(&opts.apks, opts.filter.as_deref())?;
    if artifacts.is_empty() {
        bail!("no artifacts found under {}", opts.apks.display());
    }
    println!(
        "report-eval: {} artifact(s) from {}\n  json  -> {}\n  logs  -> {}",
        artifacts.len(),
        opts.apks.display(),
        json_dir.display(),
        log_dir.display()
    );

    let mut rows = Vec::new();
    for artifact in &artifacts {
        let name = file_name(artifact);
        println!("\n=== {name}");
        let outcome = measure_one(&bin, artifact, opts, &json_dir, &log_dir);
        if let Err(err) = &outcome {
            eprintln!("  failed: {err:#}");
        }
        rows.push(Row {
            artifact: name,
            outcome,
        });
    }

    print_table(&rows);
    let table = opts.out.join("summary.md");
    std::fs::write(
        &table,
        format!("{}\n{}", render_table(&rows), render_dispatch_table(&rows)),
    )
    .with_context(|| format!("writing {}", table.display()))?;
    println!("\nwrote {}", table.display());
    Ok(rows.iter().all(|r| r.outcome.is_ok()))
}

fn measure_one(
    bin: &Path,
    artifact: &Path,
    opts: &Options,
    json_dir: &Path,
    log_dir: &Path,
) -> Result<Measured> {
    let slug = slug(artifact);
    let store = opts.out.join("stores").join(&slug);
    exec::fresh_dir(&store).with_context(|| format!("preparing {}", store.display()))?;

    let ctadl = |args: &[&str]| -> Command {
        let mut cmd = Command::new(bin);
        cmd.arg("--store").arg(&store).args(args);
        cmd
    };

    let mut import_args = vec!["import", "--name", "app"];
    if !opts.native_libs {
        import_args.push("--no-native-libs");
    }
    let artifact_str = artifact.to_string_lossy().into_owned();
    import_args.push(&artifact_str);

    let import_log = log_dir.join(format!("{slug}.import.log"));
    let (import_secs, import_peak_mb) = timed(ctadl(&import_args), "ctadl import", &import_log)?;
    println!("  import  {import_secs:6.1}s  {}", mb(import_peak_mb));

    let json = json_dir.join(format!("{slug}.json"));
    let top = opts.top.to_string();
    let json_str = json.to_string_lossy().into_owned();
    let report_args = [
        "report", "app", "--format", "json", "--top", &top, "--output", &json_str,
    ];
    let report_log = log_dir.join(format!("{slug}.report.log"));
    let (report_secs, report_peak_mb) = timed(ctadl(&report_args), "ctadl report", &report_log)?;
    println!("  report  {report_secs:6.1}s  {}", mb(report_peak_mb));

    // Also keep the human-readable form: the JSON is what a later run diffs against, but the
    // text is what a person reads when the diff moves. Together with the stability check
    // below this is three reports per app rather than one -- on a 1.9-million-function app
    // that is four minutes instead of ninety seconds, which is a fair price for having both
    // artifacts and a reproducibility result.
    let text = exec::capture_stdout(ctadl(&["report", "app", "--top", &top]), "ctadl report")?;
    std::fs::write(json_dir.join(format!("{slug}.txt")), &text)
        .with_context(|| format!("writing the text report for {slug}"))?;

    // §7.2 question 2: two reports over one import must agree. The tables this is built on
    // are documented as byte-stable, so this is an assertion rather than a hope.
    let again = json_dir.join(format!("{slug}.again.json"));
    let again_str = again.to_string_lossy().into_owned();
    let mut second = report_args;
    second[7] = &again_str;
    exec::run_checked(ctadl(&second), "ctadl report")?;
    let stable = std::fs::read(&json)? == std::fs::read(&again)?;
    std::fs::remove_file(&again).ok();

    let doc: Value = serde_json::from_slice(&std::fs::read(&json)?)
        .with_context(|| format!("parsing {}", json.display()))?;

    if !opts.keep_stores {
        std::fs::remove_dir_all(&store).ok();
    }
    summarize(
        &doc,
        import_secs,
        report_secs,
        import_peak_mb,
        report_peak_mb,
        stable,
    )
}

/// Folds a report over a whole project into one row.
///
/// An `.xapk` is several programs -- one per split -- so the counts are summed across them
/// and the distribution columns are taken from the *largest* program by function count. A
/// weighted merge of two programs' percentiles would need their full tables, which the JSON
/// deliberately does not carry; naming the biggest program's shape is honest and is what a
/// reader wants anyway.
fn share(program: &Value, key: &str) -> f64 {
    program["worst_signatures"][key].as_f64().unwrap_or(0.0)
}

fn summarize(
    doc: &Value,
    import_secs: f64,
    report_secs: f64,
    import_peak_mb: Option<f64>,
    report_peak_mb: Option<f64>,
    stable: bool,
) -> Result<Measured> {
    let programs = doc["programs"]
        .as_array()
        .context("the report lists no programs")?;
    if programs.is_empty() {
        bail!("no program in this artifact has any functions");
    }
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let biggest = programs
        .iter()
        .max_by_key(|p| n(&p["functions"]))
        .expect("non-empty");
    let dist = &biggest["virtual_targets"]["targets_per_site"];
    let sum = |path: &[&str]| -> u64 {
        programs
            .iter()
            .map(|p| {
                let mut v = p;
                for key in path {
                    v = &v[*key];
                }
                n(v)
            })
            .sum()
    };
    Ok(Measured {
        functions: sum(&["functions"]),
        sites: sum(&["census", "total"]),
        virtual_sites: sum(&["census", "virtual"]),
        one_target: sum(&["virtual_targets", "sites_with_one_target"]),
        zero_target: sum(&["virtual_targets", "sites_with_zero_targets"]),
        cha_edges: sum(&["virtual_targets", "total_edges"]),
        rta_edges: sum(&["rta", "rta_edges"]),
        p50: n(&dist["p50"]),
        p99: n(&dist["p99"]),
        max_targets: n(&dist["max"]),
        top10: share(biggest, "top_10_site_share"),
        top100: share(biggest, "top_100_site_share"),
        sig10: share(biggest, "top_10_signature_excess_share"),
        sig100: share(biggest, "top_100_signature_excess_share"),
        dispatch: dispatch_split(programs, biggest),
        import_secs,
        report_secs,
        import_peak_mb,
        report_peak_mb,
        stable,
    })
}

/// Pulls the interface-versus-class-virtual comparison out of one report.
///
/// Written to survive a report that does not carry the split at all -- a Lua or pcode
/// artifact, or an older JSON kept for comparison -- by leaving the row at zero rather than
/// failing the app. The table says which is which by printing a dash for a zero site count.
fn dispatch_split(programs: &[Value], biggest: &Value) -> Dispatch {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let sum = |kind: &str| -> u64 {
        programs
            .iter()
            .map(|p| n(&p["census"]["by_dispatch"][kind]))
            .sum()
    };
    let row = |kind: &str| -> Option<&Value> {
        biggest["virtual_targets"]["by_dispatch"]
            .as_array()?
            .iter()
            .find(|r| r["dispatch"].as_str() == Some(kind))
    };
    let one_target = |kind: &str| -> f64 {
        row(kind).map_or(0.0, |r| {
            let sites = n(&r["sites"]);
            if sites == 0 {
                0.0
            } else {
                n(&r["sites_with_one_target"]) as f64 / sites as f64
            }
        })
    };
    let p99 = |kind: &str| -> u64 { row(kind).map_or(0, |r| n(&r["targets_per_site"]["p99"])) };
    let excess_total = n(&biggest["worst_signatures"]["excess_edges"]);
    let interface_excess = biggest["worst_signatures"]["by_dispatch"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|r| r["dispatch"].as_str() == Some("interface"))
        })
        .map_or(0, |r| n(&r["excess_edges"]));
    Dispatch {
        interface_sites: sum("interface"),
        virtual_sites: sum("virtual"),
        super_sites: sum("super"),
        interface_one_target: one_target("interface"),
        virtual_one_target: one_target("virtual"),
        interface_p99: p99("interface"),
        virtual_p99: p99("virtual"),
        interface_excess_share: if excess_total == 0 {
            0.0
        } else {
            interface_excess as f64 / excess_total as f64
        },
        in_cycles: n(&biggest["recursion"]["functions_in_nontrivial_sccs"]),
        in_cycles_without_interfaces: biggest["recursion"]["without_interface_edges"]
            ["functions_in_nontrivial_sccs"]
            .as_u64(),
    }
}

/// Runs `cmd`, appending its output to `log`, and returns `(wall seconds, peak MB)`.
///
/// Wall time is measured here. Peak memory comes from `/usr/bin/time -l`, whose `peak memory
/// footprint` is the kernel's high-water mark for the process -- nothing slips between polls.
/// Where that is unavailable (any non-macOS host, or no `/usr/bin/time`), the column is
/// simply absent rather than filled with a number measured a different way.
fn timed(cmd: Command, what: &str, log: &Path) -> Result<(f64, Option<f64>)> {
    let (mut cmd, wrapped) = wrap_with_time(cmd);
    let start = Instant::now();
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn `{what}`"))?;
    let secs = start.elapsed().as_secs_f64();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    std::fs::write(
        log,
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{stderr}\n--- exit {} ---\n",
            String::from_utf8_lossy(&output.stdout),
            output.status
        ),
    )
    .with_context(|| format!("writing {}", log.display()))?;
    if !output.status.success() {
        bail!("`{what}` exited {}; see {}", output.status, log.display());
    }
    let peak = wrapped.then(|| peak_mb(&stderr)).flatten();
    Ok((secs, peak))
}

/// Wraps `cmd` in `/usr/bin/time -l` when that exists, so the peak is a kernel high-water
/// mark rather than something this process sampled. Returns whether it did.
fn wrap_with_time(cmd: Command) -> (Command, bool) {
    let time = Path::new("/usr/bin/time");
    if !time.is_file() {
        return (cmd, false);
    }
    let mut wrapped = Command::new(time);
    wrapped.arg("-l").arg(cmd.get_program());
    wrapped.args(cmd.get_args());
    (wrapped, true)
}

/// `peak memory footprint` (macOS) or `Maximum resident set size` (GNU time), in MB.
fn peak_mb(stderr: &str) -> Option<f64> {
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(bytes) = line.strip_suffix("peak memory footprint") {
            return bytes.trim().parse::<f64>().ok().map(|b| b / 1e6);
        }
        // GNU time reports kilobytes.
        if let Some(kb) = line.strip_prefix("Maximum resident set size (kbytes): ") {
            return kb.trim().parse::<f64>().ok().map(|k| k / 1e3);
        }
    }
    None
}

fn discover(dir: &Path, filter: Option<&str>) -> Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        // A dotfile in a corpus directory is metadata, not an artifact.
        .filter(|path| !file_name(path).starts_with('.'))
        .filter(|path| filter.is_none_or(|f| file_name(path).contains(f)))
        .collect();
    found.sort();
    Ok(found)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A filename-safe stem. The corpus filenames carry `+` and percent-encoded characters, so
/// nothing may assume they are tidy -- paths are passed as arguments and never through a
/// shell, and derived filenames go through here.
fn slug(path: &Path) -> String {
    file_name(path)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn mb(v: Option<f64>) -> String {
    v.map_or_else(|| "     -".to_string(), |m| format!("{m:6.0} MB"))
}

fn print_table(rows: &[Row]) {
    println!("\n{}", render_table(rows));
    println!("{}", render_dispatch_table(rows));
}

/// The second table: interface dispatch against class-virtual dispatch, app by app.
///
/// Separate from the main one because it answers a different question. The first table says
/// how imprecise a program's call graph is; this one says which half of it the imprecision
/// lives in, which is what decides whether the thing to improve is interface resolution
/// specifically or resolution in general.
fn render_dispatch_table(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str("### Interface versus class-virtual dispatch\n\n");
    out.push_str(
        "Site counts are summed over an artifact's programs; the percentile, share and cycle \
         columns come from its largest program.\n\n",
    );
    out.push_str(
        "| app | iface sites | virtual sites | super sites | iface 1-target | \
         virtual 1-target | iface p99 | virtual p99 | iface share of excess | in cycles | \
         in cycles, no iface edges |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for row in rows {
        let Ok(m) = &row.outcome else { continue };
        let d = &m.dispatch;
        if d.interface_sites == 0 && d.virtual_sites == 0 {
            // No Java dispatch at all: a Lua or pcode artifact has no split to report, and a
            // row of zeros here would read as a finding about a program that cannot have one.
            out.push_str(&format!(
                "| {} | - | - | - | - | - | - | - | - | - | - |\n",
                row.artifact
            ));
            continue;
        }
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.1}% | {:.1}% | {} | {} | {:.1}% | {} | {} |\n",
            row.artifact,
            d.interface_sites,
            d.virtual_sites,
            d.super_sites,
            100.0 * d.interface_one_target,
            100.0 * d.virtual_one_target,
            d.interface_p99,
            d.virtual_p99,
            100.0 * d.interface_excess_share,
            d.in_cycles,
            d.in_cycles_without_interfaces
                .map_or("-".to_string(), |v| v.to_string()),
        ));
    }
    out
}

fn render_table(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(
        "| app | funcs | sites | virtual | 1-target | 0-target | CHA edges | RTA edges | \
         p50 | p99 | max | top10 sites | top100 sites | top10 sigs (excess) | \
         top100 sigs (excess) | import s | \
         report s | import MB | report MB | stable |\n",
    );
    out.push_str(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | \
         ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for row in rows {
        match &row.outcome {
            Err(err) => {
                out.push_str(&format!("| {} | failed: {err} |\n", row.artifact));
            }
            Ok(m) => {
                let pct = |part: u64, whole: u64| {
                    if whole == 0 {
                        "-".to_string()
                    } else {
                        format!("{:.1}%", 100.0 * part as f64 / whole as f64)
                    }
                };
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.2}% | {:.2}% | \
                     {:.1}% | {:.1}% | {:.1} | {:.1} | {} | {} | {} |\n",
                    row.artifact,
                    m.functions,
                    m.sites,
                    m.virtual_sites,
                    pct(m.one_target, m.virtual_sites),
                    pct(m.zero_target, m.virtual_sites),
                    m.cha_edges,
                    m.rta_edges,
                    m.p50,
                    m.p99,
                    m.max_targets,
                    100.0 * m.top10,
                    100.0 * m.top100,
                    100.0 * m.sig10,
                    100.0 * m.sig100,
                    m.import_secs,
                    m.report_secs,
                    m.import_peak_mb.map_or("-".into(), |v| format!("{v:.0}")),
                    m.report_peak_mb.map_or("-".into(), |v| format!("{v:.0}")),
                    if m.stable { "yes" } else { "NO" },
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus filenames carry `+` and percent-encoding, and every derived path goes
    /// through [`slug`].
    #[test]
    fn slug_survives_a_real_corpus_filename() {
        assert_eq!(
            slug(Path::new(
                "/a/TikTok+-+Videos%2C+Shop+%26+LIVE_46.1.3_APKPure.xapk"
            )),
            "TikTok_-_Videos_2C_Shop_-26_LIVE_46.1.3_APKPure.xapk".replace("-26", "_26")
        );
        assert_eq!(slug(Path::new("/a/plain.apk")), "plain.apk");
    }

    #[test]
    fn peak_is_parsed_from_both_time_dialects() {
        // macOS `/usr/bin/time -l`: the number leads, the label follows.
        assert_eq!(
            peak_mb("        18377276800  peak memory footprint\n"),
            Some(18377.2768)
        );
        // GNU time -v.
        assert_eq!(
            peak_mb("\tMaximum resident set size (kbytes): 2048\n"),
            Some(2.048)
        );
        assert_eq!(peak_mb("nothing here"), None);
    }
}
