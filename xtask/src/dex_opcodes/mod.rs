//! `cargo xtask dex-opcodes`: run each Dalvik opcode on ART and check dex-reader's
//! `data_flow()` and `can_throw()` against what it actually does.
//!
//! For every case in [`cases`], this generates a smali class whose `run()` loads registers
//! v0..v13 from static fields of `H` (H.java), executes the one instruction under test, and
//! stores the registers back. `Driver.java` runs every case under `dalvikvm64 -Xint` on a device
//! or emulator: once with baseline inputs, once per alternative value of each 32-bit register
//! slot, and once per fault assignment. It reports which slots the instruction wrote, which
//! slots changed its observable result, and what it threw. This module then decodes the same
//! `.dex` with dex-reader and compares:
//!
//!  - a slot written that `data_flow().dest` omits: FAIL;
//!  - a slot that changed the result that `data_flow().source` omits: FAIL;
//!  - an exception from an instruction whose `can_throw()` is false: FAIL;
//!  - a listed source never seen to matter, a listed dest never seen to change, or a
//!    `can_throw()` instruction never seen to throw: REVIEW. A run shows dependence only for the
//!    values it tries, so these need a person; the expected ones are in [`EXPECTED_REVIEWS`].
//!
//! It needs `adb` with one device attached (or `--avd <name>` to boot an emulator), plus `java`,
//! `javac`, `d8` and `smali`. All inputs and outputs land in `--out`.

mod cases;
mod saved;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use dex_reader::instructions::{Instruction, DALVIK_OPCODES};
use dex_reader::parser::{decode_code_item, DecodedCodeItem};
use dex_reader::DexParser;

use crate::exec;
use cases::{Case, Fault, In, Ty, CASE_REGS};

const H_JAVA: &str = include_str!("H.java");
const DRIVER_JAVA: &str = include_str!("Driver.java");
const DEVICE_DIR: &str = "/data/local/tmp/ctadl-dex-opcodes";

/// REVIEW findings that are expected, by case name and finding, with the reason.
const EXPECTED_REVIEWS: &[(&str, &str, &str)] = &[
    ("const-string", "can_throw() but never threw", RESOLUTION),
    ("const-string/jumbo", "can_throw() but never threw", RESOLUTION),
    ("const-class", "can_throw() but never threw", RESOLUTION),
    ("const-method-handle", "can_throw() but never threw", RESOLUTION),
    ("const-method-type", "can_throw() but never threw", RESOLUTION),
    ("instance-of", "can_throw() but never threw", RESOLUTION),
    ("new-instance", "can_throw() but never threw", RESOLUTION),
    ("filled-new-array", "can_throw() but never threw", RESOLUTION),
    ("filled-new-array/range", "can_throw() but never threw", RESOLUTION),
    ("sget", "can_throw() but never threw", RESOLUTION),
    ("sput", "can_throw() but never threw", RESOLUTION),
    ("sget-wide", "can_throw() but never threw", RESOLUTION),
    ("sput-wide", "can_throw() but never threw", RESOLUTION),
    ("sget-object", "can_throw() but never threw", RESOLUTION),
    ("sput-object", "can_throw() but never threw", RESOLUTION),
    ("sget-boolean", "can_throw() but never threw", RESOLUTION),
    ("sput-boolean", "can_throw() but never threw", RESOLUTION),
    ("sget-byte", "can_throw() but never threw", RESOLUTION),
    ("sput-byte", "can_throw() but never threw", RESOLUTION),
    ("sget-char", "can_throw() but never threw", RESOLUTION),
    ("sput-char", "can_throw() but never threw", RESOLUTION),
    ("sget-short", "can_throw() but never threw", RESOLUTION),
    ("sput-short", "can_throw() but never threw", RESOLUTION),
    ("sget F", "can_throw() but never threw", RESOLUTION),
    ("sget-wide D", "can_throw() but never threw", RESOLUTION),
    ("sput F", "can_throw() but never threw", RESOLUTION),
    ("sput-wide D", "can_throw() but never threw", RESOLUTION),
    ("monitor-exit", "can_throw() but never threw", "IllegalMonitorStateException needs an unheld lock; the `monitor-exit unlocked` case throws it."),
    ("div-int/lit16", "can_throw() but never threw", "The divisor is a literal; the `div-int/lit16 by zero` case throws."),
    ("rem-int/lit16", "can_throw() but never threw", "The divisor is a literal; the `rem-int/lit16 by zero` case throws."),
    ("div-int/lit8", "can_throw() but never threw", "The divisor is a literal; the `div-int/lit8 by zero` case throws."),
    ("rem-int/lit8", "can_throw() but never threw", "The divisor is a literal; the `rem-int/lit8 by zero` case throws."),
    ("monitor-enter", "source v1 never changed the result", MONITOR),
    ("monitor-exit", "source v1 never changed the result", MONITOR),
    ("check-cast", "dest v1 never changed", "check-cast rewrites vA with the same reference, now known to have the cast type."),
    ("long-to-int", "source v5 never changed the result", "Truncation drops the high word, but the instruction reads the pair: the verifier requires vB, vB+1 to hold a long."),
];

/// Why resolution-only throws are not provoked by inputs.
const RESOLUTION: &str = "Throws only when a reference fails to resolve or a class fails to \
     initialize; the `... missing` cases provoke that.";
/// Why monitors show no value dependence.
const MONITOR: &str =
    "Locking changes no value the harness can see; the null fault shows the read.";

#[derive(Debug)]
pub struct Options {
    pub out: PathBuf,
    pub avd: Option<String>,
    pub filter: Option<String>,
    /// Save the run's observations to [`saved::path`] for `dex:opcodes-replay`.
    pub save: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            out: PathBuf::from("target/dex-opcodes"),
            avd: None,
            filter: None,
            save: false,
        }
    }
}

/// Returns `Ok(true)` when no case failed and every REVIEW finding is expected.
pub fn run(opts: &Options) -> Result<bool> {
    for tool in ["adb", "java", "javac", "d8", "smali"] {
        if exec::which(tool).is_none() {
            bail!("`{tool}` not on PATH (it is in the Nix dev shell)");
        }
    }
    let mut all = cases::all();
    if let Some(filter) = &opts.filter {
        all.retain(|c| c.name.contains(filter.as_str()));
    }
    if all.is_empty() {
        bail!("no cases selected");
    }
    if opts.save && opts.filter.is_some() {
        bail!("--save needs every case; drop --filter");
    }
    let out = std::path::absolute(&opts.out)?;
    exec::fresh_dir(&out)?;
    println!(
        "dex-opcodes: {} cases, output in {}",
        all.len(),
        out.display()
    );

    let built = build(&all, &out)?;
    let _emulator = ensure_device(opts.avd.as_deref(), &out)?;
    let results_text = run_on_device(&built, &out)?;
    let results = parse_results(&results_text)?;

    let decoded = decode_cases(&built)?;

    let evaluation = evaluate(&all, &results, &decoded)?;
    std::fs::write(out.join("report.txt"), &evaluation.report)?;
    print!("{}", evaluation.report);
    println!("report: {}", out.join("report.txt").display());
    if opts.save {
        if !evaluation.ok {
            bail!("not saving the observations of a run that did not pass");
        }
        let path = saved::path();
        std::fs::write(&path, saved::render(&all, &device_name()?, &results_text))?;
        println!("saved observations: {}", path.display());
    }
    Ok(evaluation.ok)
}

/// The `dex:opcodes-replay` check: judge dex-reader against the observations saved by
/// `--save`, with no device. Needs only `smali`, to rebuild the cases. Writes the report to
/// `work/report.txt`.
pub fn replay(work: &Path) -> Result<()> {
    let all = cases::all();
    let saved = saved::parse(saved::SAVED)?;
    saved::check_current(&saved, &all)?;
    let results = parse_results(saved.results)?;
    exec::fresh_dir(work)?;
    let built = build_cases(&all, work)?;
    let decoded = decode_cases(&built)?;
    let evaluation = evaluate(&all, &results, &decoded)?;
    let report = work.join("report.txt");
    std::fs::write(&report, &evaluation.report)?;
    if !evaluation.ok {
        bail!(
            "dex-reader disagrees with ART's observations ({}; see {}):\n{}",
            saved.device,
            report.display(),
            evaluation
                .report
                .lines()
                .filter(|l| !l.starts_with("PASS") && !l.contains("expected: "))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

/// The verdict on every case, as report text, and whether they all passed.
struct Evaluation {
    report: String,
    ok: bool,
}

/// Judge every case in `all` against its observation in `results` and its decoding in
/// `decoded`, both keyed by class name.
fn evaluate(
    all: &[Case],
    results: &BTreeMap<String, Observed>,
    decoded: &BTreeMap<String, Instruction>,
) -> Result<Evaluation> {
    let mut report = String::new();
    let mut failed = 0usize;
    let mut unexpected_reviews = 0usize;
    let mut seen_expected = BTreeSet::new();
    for (i, case) in all.iter().enumerate() {
        let class = class_name(i);
        let verdict = judge(case, results.get(&class), decoded.get(&class));
        let reviews: Vec<(String, bool)> = verdict
            .reviews
            .iter()
            .map(|r| {
                let expected = EXPECTED_REVIEWS
                    .iter()
                    .any(|(n, f, _)| *n == case.name && f == r);
                if expected {
                    seen_expected.insert((case.name.clone(), r.clone()));
                }
                (r.clone(), expected)
            })
            .collect();
        let status = if !verdict.fails.is_empty() {
            failed += 1;
            "FAIL"
        } else if reviews.iter().any(|(_, expected)| !expected) {
            unexpected_reviews += 1;
            "REVIEW"
        } else {
            "PASS"
        };
        writeln!(report, "{status:6} {:32} {}", case.name, case.insn)?;
        for f in &verdict.fails {
            writeln!(report, "         fail: {f}")?;
        }
        for (r, expected) in &reviews {
            let tag = if *expected { "expected" } else { "review" };
            writeln!(report, "         {tag}: {r}")?;
        }
    }
    let mut stale = 0usize;
    for (name, finding, _) in EXPECTED_REVIEWS {
        let in_run = all.iter().any(|c| c.name == *name);
        if in_run && !seen_expected.contains(&(name.to_string(), finding.to_string())) {
            stale += 1;
            writeln!(
                report,
                "STALE  {name}: expected review no longer occurs: {finding}"
            )?;
        }
    }
    writeln!(
        report,
        "\n{} cases: {failed} failed, {unexpected_reviews} need review, {stale} stale expectations",
        all.len()
    )?;
    Ok(Evaluation {
        report,
        ok: failed == 0 && unexpected_reviews == 0 && stale == 0,
    })
}

fn class_name(i: usize) -> String {
    format!("Case_{i}")
}

// --- generation ---------------------------------------------------------------

/// Where each case's instruction under test sits, and the built `.dex` files.
struct Built {
    driver_dex: PathBuf,
    cases_dex: PathBuf,
    manifest: PathBuf,
    /// Class name -> index of the instruction under test among `run()`'s instructions.
    insn_index: BTreeMap<String, usize>,
}

/// The type of each register a case loads: its own inputs, plus an int in every other register.
fn input_layout(case: &Case) -> BTreeMap<u32, In> {
    let mut layout = case.ins.clone();
    let covered = |layout: &BTreeMap<u32, In>, r: u32| {
        layout
            .iter()
            .any(|(&s, v)| s == r || (v.ty().is_wide() && s + 1 == r))
    };
    for r in 0..CASE_REGS {
        if !covered(&layout, r) {
            layout.insert(r, cases::ints(r));
        }
    }
    layout
}

/// The type of each register after the instruction, with the harness's registers last.
fn output_layout(case: &Case, input: &BTreeMap<u32, In>) -> Vec<(u32, Ty)> {
    let width = |t: Ty| if t.is_wide() { 2 } else { 1 };
    let mut layout: Vec<(u32, Ty)> = input.iter().map(|(&r, v)| (r, v.ty())).collect();
    for &(r, ty) in &case.writes {
        // A write ends any value it overlaps; a pair it splits can no longer be read.
        layout.retain(|&(s, t)| s + width(t) <= r || r + width(ty) <= s);
        layout.push((r, ty));
    }
    layout.retain(|(r, _)| !case.hidden.contains(r));
    layout.sort_by_key(|&(r, _)| r);
    layout.extend(case.harness.iter().copied());
    layout
}

fn is_instruction_line(line: &str) -> bool {
    let t = line.trim();
    !(t.is_empty() || t.starts_with('.') || t.starts_with(':') || t.starts_with('#'))
}

fn smali_for(case: &Case, class: &str) -> (String, usize) {
    let input = input_layout(case);
    let output = output_layout(case, &input);
    let subst = |s: &str| s.replace("@SELF@", class);

    let mut body: Vec<String> = Vec::new();
    for (&r, v) in &input {
        match v {
            In::I(_) => body.push(format!("sget v{r}, LH;->iI{r}:I")),
            In::F(_) => body.push(format!("sget v{r}, LH;->iF{r}:F")),
            In::J(_) => body.push(format!("sget-wide v{r}, LH;->iJ{r}:J")),
            In::D(_) => body.push(format!("sget-wide v{r}, LH;->iD{r}:D")),
            In::L { desc, .. } => {
                body.push(format!("sget-object v{r}, LH;->iL{r}:Ljava/lang/Object;"));
                if *desc != "Ljava/lang/Object;" {
                    body.push(format!("check-cast v{r}, {}", subst(desc)));
                }
            }
        }
    }
    body.extend(case.prelude.iter().cloned());
    let index = body.iter().filter(|l| is_instruction_line(l)).count();
    body.push(subst(&case.insn));
    body.extend(case.postlude.iter().cloned());
    let ret_desc = match case.ret {
        Some((_, desc)) => desc,
        None => {
            for &(r, ty) in &output {
                body.push(match ty {
                    Ty::I => format!("sput v{r}, LH;->oI{r}:I"),
                    Ty::F => format!("sput v{r}, LH;->oF{r}:F"),
                    Ty::J => format!("sput-wide v{r}, LH;->oJ{r}:J"),
                    Ty::D => format!("sput-wide v{r}, LH;->oD{r}:D"),
                    Ty::L => format!("sput-object v{r}, LH;->oL{r}:Ljava/lang/Object;"),
                });
            }
            body.push("return-void".to_string());
            "V"
        }
    };
    body.extend(case.payload.iter().cloned());

    let mut s = String::new();
    let _ = writeln!(
        s,
        ".class public L{class};\n.super LH$Obj;\n# {}: {}\n",
        case.name, case.insn
    );
    s.push_str(
        ".method public constructor <init>()V\n    .registers 1\n    invoke-direct {p0}, LH$Obj;-><init>()V\n    return-void\n.end method\n\n",
    );
    s.push_str(
        ".method private p(IJLjava/lang/Object;)V\n    .registers 5\n    invoke-static/range {p0 .. p4}, LH;->recDirect(LH$Obj;IJLjava/lang/Object;)V\n    return-void\n.end method\n\n",
    );
    let _ = writeln!(
        s,
        ".method public static run(){ret_desc}\n    .registers 16"
    );
    for line in body {
        let indent = if line.starts_with(':') { "" } else { "    " };
        let _ = writeln!(s, "{indent}{line}");
    }
    s.push_str(".end method\n");
    (s, index)
}

/// The candidate list for one wide register: low words, then high words. Besides the other
/// candidates' words, each half gets a perturbation of its own, so a change to just that half
/// is tried even when the other candidates share it.
fn wide_candidates(bits: &[u64], double: bool) -> (Vec<u32>, Vec<u32>) {
    let lo = |b: u64| b as u32;
    let hi = |b: u64| (b >> 32) as u32;
    let (lo_flip, hi_flip) = if double {
        (0xffc0_0000, 0x0008_0000)
    } else {
        (0x5a5a_5a5a, 0x1234)
    };
    let mut los = vec![lo(bits[0]), lo(bits[0]) ^ lo_flip, lo(bits[0]) ^ 1];
    let mut his = vec![hi(bits[0]), hi(bits[0]) ^ hi_flip, hi(bits[0]) ^ 1];
    los.extend(bits[1..].iter().map(|&b| lo(b)));
    his.extend(bits[1..].iter().map(|&b| hi(b)));
    let dedup = |v: Vec<u32>| {
        let mut seen = BTreeSet::new();
        v.into_iter()
            .filter(|x| seen.insert(*x))
            .collect::<Vec<_>>()
    };
    (dedup(los), dedup(his))
}

fn csv<T: ToString>(v: impl IntoIterator<Item = T>) -> String {
    v.into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn manifest_for(case: &Case, class: &str) -> String {
    let input = input_layout(case);
    let mut m = String::new();
    let ret = case.ret.map_or('V', |(letter, _)| letter);
    let _ = writeln!(m, "case {class} {ret}");
    for (&r, v) in &input {
        let line = match v {
            In::I(c) => format!("in {r} I {}", csv(c)),
            In::F(c) => format!("in {r} F {}", csv(c.iter().map(|f| f.to_bits() as i32))),
            In::J(c) => {
                let (lo, hi) =
                    wide_candidates(&c.iter().map(|&x| x as u64).collect::<Vec<_>>(), false);
                format!("in {r} J {} {}", csv(lo), csv(hi))
            }
            In::D(c) => {
                let (lo, hi) =
                    wide_candidates(&c.iter().map(|x| x.to_bits()).collect::<Vec<_>>(), true);
                format!("in {r} D {} {}", csv(lo), csv(hi))
            }
            In::L { domain, count, .. } => format!("in {r} L {domain} {count}"),
        };
        let _ = writeln!(m, "{line}");
    }
    if case.ret.is_none() {
        for (r, ty) in output_layout(case, &input) {
            let _ = writeln!(m, "out {r} {}", ty.letter());
        }
    }
    for fault in &case.faults {
        let mut parts = Vec::new();
        for &(r, f) in fault {
            match f {
                Fault::Int(v) => parts.push(format!("{r}={v}")),
                Fault::Long(v) => {
                    parts.push(format!("{r}={}", v as u64 as u32));
                    parts.push(format!("{}={}", r + 1, (v as u64 >> 32) as u32));
                }
                Fault::Null => parts.push(format!("{r}=null")),
                Fault::Obj(k) => parts.push(format!("{r}={k}")),
            }
        }
        let _ = writeln!(m, "fault {}", parts.join(" "));
    }
    let _ = writeln!(m, "end");
    m
}

fn h_java() -> String {
    let mut fields = String::new();
    for r in 0..16 {
        for (dir, _) in [("i", ()), ("o", ())] {
            let _ = writeln!(
                fields,
                "    public static int {dir}I{r};\n    public static float {dir}F{r};\n    public static long {dir}J{r};\n    public static double {dir}D{r};\n    public static Object {dir}L{r};"
            );
        }
    }
    H_JAVA.replace("    // @FIELDS@\n", &fields)
}

fn build(all: &[Case], out: &Path) -> Result<Built> {
    // The Java side: H and Driver, compiled to Java 8 bytecode and dexed.
    let java = out.join("java");
    exec::fresh_dir(&java)?;
    std::fs::write(java.join("H.java"), h_java())?;
    std::fs::write(java.join("Driver.java"), DRIVER_JAVA)?;
    let classes = java.join("classes");
    exec::fresh_dir(&classes)?;
    let mut javac = Command::new("javac");
    javac
        .args(["--release", "8", "-nowarn", "-d"])
        .arg(&classes)
        .arg(java.join("H.java"))
        .arg(java.join("Driver.java"));
    exec::run_checked(javac, "javac")?;
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "class"))
        .collect();
    class_files.sort();
    let mut d8 = Command::new("d8");
    d8.args(["--min-api", "28", "--output"])
        .arg(&java)
        .args(&class_files);
    exec::run_checked(d8, "d8")?;
    let driver_dex = out.join("driver.dex");
    std::fs::rename(java.join("classes.dex"), &driver_dex)?;
    Ok(Built {
        driver_dex,
        ..build_cases(all, out)?
    })
}

/// The cases alone: one smali class each, so a verifier failure costs only its own case.
/// `driver_dex` is left empty.
fn build_cases(all: &[Case], out: &Path) -> Result<Built> {
    let smali_dir = out.join("smali");
    exec::fresh_dir(&smali_dir)?;
    let mut manifest = String::new();
    let mut insn_index = BTreeMap::new();
    for (i, case) in all.iter().enumerate() {
        let class = class_name(i);
        let (smali, index) = smali_for(case, &class);
        std::fs::write(smali_dir.join(format!("{class}.smali")), smali)?;
        manifest.push_str(&manifest_for(case, &class));
        insn_index.insert(class, index);
    }
    let cases_dex = out.join("cases.dex");
    let mut smali = Command::new("smali");
    smali
        .args(["assemble", "--api", "28", "-o"])
        .arg(&cases_dex)
        .arg(&smali_dir);
    exec::run_checked(smali, "smali assemble")?;
    let manifest_path = out.join("manifest.txt");
    std::fs::write(&manifest_path, manifest)?;
    Ok(Built {
        driver_dex: PathBuf::new(),
        cases_dex,
        manifest: manifest_path,
        insn_index,
    })
}

// --- the device -----------------------------------------------------------------

/// An emulator this run started, killed when dropped.
struct Emulator(Child);

impl Drop for Emulator {
    fn drop(&mut self) {
        let _ = Command::new("adb").args(["emu", "kill"]).output();
        let _ = self.0.wait();
    }
}

fn devices() -> Result<Vec<String>> {
    let out = exec::capture_stdout(
        {
            let mut c = Command::new("adb");
            c.arg("devices");
            c
        },
        "adb devices",
    )?;
    Ok(out
        .lines()
        .skip(1)
        .filter_map(|l| l.strip_suffix("\tdevice").map(str::to_string))
        .collect())
}

/// Use the attached device, or boot `avd` headless when none is attached.
fn ensure_device(avd: Option<&str>, out: &Path) -> Result<Option<Emulator>> {
    if !devices()?.is_empty() || std::env::var_os("ANDROID_SERIAL").is_some() {
        return Ok(None);
    }
    let Some(avd) = avd else {
        bail!(
            "no device attached. Boot an emulator first, or pass --avd <name>. To make one:\n  \
             avdmanager create avd -n dexop -k \"system-images;android-34;google_apis;arm64-v8a\""
        );
    };
    let log = std::fs::File::create(out.join("emulator.log"))?;
    let child = Command::new("emulator")
        .args([
            "-avd",
            avd,
            "-no-window",
            "-no-audio",
            "-no-boot-anim",
            "-no-snapshot",
        ])
        .stdout(log.try_clone()?)
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .context("starting the emulator")?;
    let emulator = Emulator(child);
    let start = Instant::now();
    loop {
        let booted = Command::new("adb")
            .args(["shell", "getprop", "sys.boot_completed"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1");
        if booted {
            return Ok(Some(emulator));
        }
        if start.elapsed() > Duration::from_secs(600) {
            bail!("the emulator did not boot within 10 minutes (see emulator.log)");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The attached device's API level, ABI and build, for the saved observations.
fn device_name() -> Result<String> {
    let prop = |name: &str| -> Result<String> {
        Ok(adb(&["shell", "getprop", name], "adb shell getprop")?
            .trim()
            .to_string())
    };
    Ok(format!(
        "api {} {} {}",
        prop("ro.build.version.sdk")?,
        prop("ro.product.cpu.abi")?,
        prop("ro.build.fingerprint")?
    ))
}

fn adb(args: &[&str], what: &str) -> Result<String> {
    let mut c = Command::new("adb");
    c.args(args);
    exec::capture_stdout(c, what)
}

fn run_on_device(built: &Built, out: &Path) -> Result<String> {
    adb(&["shell", "rm", "-rf", DEVICE_DIR], "adb shell rm")?;
    adb(
        &[
            "shell",
            "mkdir",
            "-p",
            &format!("{DEVICE_DIR}/dalvik-cache"),
        ],
        "adb shell mkdir",
    )?;
    for f in [&built.driver_dex, &built.cases_dex, &built.manifest] {
        adb(&["push", &f.to_string_lossy(), DEVICE_DIR], "adb push")?;
    }
    // -Xint, so each instruction runs as written rather than as compiled code.
    let cmd = format!(
        "cd {DEVICE_DIR} && ANDROID_DATA={DEVICE_DIR} dalvikvm64 -Xint \
         -cp {DEVICE_DIR}/driver.dex:{DEVICE_DIR}/cases.dex Driver manifest.txt results.txt"
    );
    let output = Command::new("adb")
        .args(["shell", &cmd])
        .output()
        .context("running dalvikvm64")?;
    let mut log = output.stdout.clone();
    log.extend_from_slice(&output.stderr);
    std::fs::write(out.join("device.log"), &log)?;
    if !output.status.success() {
        bail!(
            "dalvikvm64 failed (see {}):\n{}",
            out.join("device.log").display(),
            String::from_utf8_lossy(&log)
        );
    }
    let results = out.join("results.txt");
    adb(
        &[
            "pull",
            &format!("{DEVICE_DIR}/results.txt"),
            &results.to_string_lossy(),
        ],
        "adb pull",
    )?;
    std::fs::read_to_string(&results).context("reading results.txt")
}

// --- comparison -----------------------------------------------------------------

#[derive(Debug, Default)]
struct Observed {
    written: BTreeSet<u32>,
    influence: BTreeSet<u32>,
    threw: Vec<String>,
    faults: Vec<String>,
    error: Option<String>,
}

fn regs(s: &str) -> Result<BTreeSet<u32>> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(|x| x.parse().with_context(|| format!("bad register {x}")))
        .collect()
}

fn parse_results(text: &str) -> Result<BTreeMap<String, Observed>> {
    let mut out = BTreeMap::new();
    let mut cur: Option<(String, Observed)> = None;
    for line in text.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "case" => cur = Some((rest.to_string(), Observed::default())),
            "end" => {
                let (name, obs) = cur.take().context("`end` without `case`")?;
                out.insert(name, obs);
            }
            _ => {
                let (_, obs) = cur
                    .as_mut()
                    .with_context(|| format!("line outside a case: {line}"))?;
                match key {
                    "baseline" => {}
                    "written" => obs.written = regs(rest)?,
                    "influence" => obs.influence = regs(rest)?,
                    "threw" => {
                        obs.threw = rest
                            .split(',')
                            .filter(|x| !x.is_empty())
                            .map(String::from)
                            .collect()
                    }
                    "fault" => obs
                        .faults
                        .push(rest.split_once(' ').map_or("", |x| x.1).to_string()),
                    "error" => obs.error = Some(rest.to_string()),
                    _ => bail!("unknown result line: {line}"),
                }
            }
        }
    }
    Ok(out)
}

/// dex-reader's decoding of each case's instruction under test, from `built.cases_dex`.
fn decode_cases(built: &Built) -> Result<BTreeMap<String, Instruction>> {
    let bytes = std::fs::read(&built.cases_dex)?;
    let parser = DexParser::new(&bytes).map_err(|e| anyhow!("parsing cases.dex: {e}"))?;
    instructions_under_test(&parser, built)
}

/// dex-reader's decoding of each case's instruction under test.
fn instructions_under_test(
    parser: &DexParser<'_>,
    built: &Built,
) -> Result<BTreeMap<String, Instruction>> {
    let mut out = BTreeMap::new();
    for class_def in parser.classes() {
        let class = parser
            .class_name(class_def)
            .map_err(|e| anyhow!("class name: {e}"))?;
        let class = class
            .trim_start_matches('L')
            .trim_end_matches(';')
            .to_string();
        let Some(&index) = built.insn_index.get(&class) else {
            continue;
        };
        let data = parser
            .class_data(class_def)
            .map_err(|e| anyhow!("class data: {e}"))?;
        for enc in &data.direct_methods {
            let mi = &parser.method_ids[enc.method_idx as usize];
            if parser.strings.get(mi.name_idx as usize).ok().as_deref() != Some("run") {
                continue;
            }
            let code = enc
                .code(parser.data)
                .map_err(|e| anyhow!("code: {e}"))?
                .context("run() has no code")?;
            let inst = decode_code_item(&code)
                .into_iter()
                .filter_map(|item| match item {
                    DecodedCodeItem::Instruction { inst, .. } => Some(inst),
                    DecodedCodeItem::Payload { .. } => None,
                })
                .nth(index)
                .with_context(|| format!("{class}: no instruction #{index}"))?;
            out.insert(class.clone(), inst);
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Verdict {
    fails: Vec<String>,
    reviews: Vec<String>,
}

fn reg_list(s: &BTreeSet<u32>) -> String {
    s.iter()
        .map(|r| format!("v{r}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn judge(case: &Case, observed: Option<&Observed>, inst: Option<&Instruction>) -> Verdict {
    let mut v = Verdict::default();
    let (Some(obs), Some(inst)) = (observed, inst) else {
        v.fails
            .push("no result or no decoded instruction for this case".into());
        return v;
    };
    if let Some(err) = &obs.error {
        v.fails.push(format!("did not run: {err}"));
        return v;
    }
    let decoded = DALVIK_OPCODES[usize::from(inst.discriminant())];
    if decoded != case.mnemonic() {
        v.fails.push(format!(
            "harness: instruction under test decodes as {decoded}, expected {}",
            case.mnemonic()
        ));
        return v;
    }

    let threw = !obs.threw.is_empty() || obs.faults.iter().any(|f| f != "none");
    if threw && !inst.can_throw() {
        let what: Vec<&String> = obs
            .threw
            .iter()
            .chain(obs.faults.iter().filter(|f| *f != "none"))
            .collect();
        v.fails
            .push(format!("threw {what:?}, but can_throw() is false"));
    }
    if case.fault_only {
        if !threw {
            v.reviews.push("never threw".into());
        }
        return v;
    }
    if !obs.threw.is_empty() && obs.written.is_empty() && obs.influence.is_empty() {
        v.fails
            .push(format!("harness: every run threw {:?}", obs.threw));
        return v;
    }
    for (i, f) in obs.faults.iter().enumerate() {
        if f == "none" {
            v.reviews.push(format!("fault {i} did not throw"));
        }
    }
    if inst.can_throw() && !threw {
        v.reviews.push("can_throw() but never threw".into());
    }

    let df = inst.data_flow();
    let checked = |r: &u32| *r < CASE_REGS;
    let source: BTreeSet<u32> = df.source.iter().map(|r| r.0).collect();
    let dest: BTreeSet<u32> = df.dest.iter().map(|r| r.0).collect();
    let written: BTreeSet<u32> = obs.written.iter().copied().filter(checked).collect();
    let influence: BTreeSet<u32> = obs.influence.iter().copied().filter(checked).collect();

    let unlisted_writes: BTreeSet<u32> = written.difference(&dest).copied().collect();
    if !unlisted_writes.is_empty() {
        v.fails.push(format!(
            "writes {}, missing from data_flow().dest = [{}]",
            reg_list(&unlisted_writes),
            reg_list(&dest)
        ));
    }
    let unlisted_reads: BTreeSet<u32> = influence.difference(&source).copied().collect();
    if !unlisted_reads.is_empty() {
        v.fails.push(format!(
            "result depends on {}, missing from data_flow().source = [{}]",
            reg_list(&unlisted_reads),
            reg_list(&source)
        ));
    }
    for r in source.difference(&influence) {
        v.reviews
            .push(format!("source v{r} never changed the result"));
    }
    for r in dest.difference(&written) {
        v.reviews.push(format!("dest v{r} never changed"));
    }
    v
}
