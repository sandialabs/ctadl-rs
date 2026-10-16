//! The `jvm:opcodes` check: run each JVM opcode and check jvm-reader's dataflow
//! against what it actually does.
//!
//! `JvmOpcodes.java` builds one class per case with ASM, runs it on the JVM
//! that runs the check, and reports, for every place the instruction under
//! test can write (stack slots, locals, fields, array elements, call
//! arguments, the thrown value), whether it wrote it and which input slots
//! changed it. It perturbs one 32-bit input slot at a time, as the Dex harness
//! (`dex_opcodes`) does on ART. This module decodes the same classes with
//! jvm-reader, reads its dataflow as [`asm_flow::claims`] does, and compares:
//!
//!  - an input that changed a place jvm-reader does not draw from it: FAIL;
//!  - a place written that jvm-reader does not list: FAIL;
//!  - a source jvm-reader lists that never changed its place, or a place it
//!    lists that was never written: REVIEW. A run shows dependence only for
//!    the values it tries, so these need a person; the expected ones, with the
//!    reason for each, are in [`EXPECTED_REVIEWS`].
//!
//! Places are compared at the level of input slots. A load from an array
//! element or a field depends on the slots holding the array and index or the
//! object, so those count as its sources. A store into an object or array is
//! keyed by the slot holding it, and choosing a different object does not
//! count as changing what was stored.
//!
//! Branches, switches, returns, `goto`, `jsr` and `ret` are not run: they move
//! no data that jvm-reader models, and `jvm:asm-flow` checks their stack
//! effect. Every other opcode, including the wide forms and `ldc_w`, must be
//! covered by some case.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use jvm_reader::flow::compute_basic_blocks_for_method;
use jvm_reader::ClassFileParser;

use crate::asm_flow::{self, LocalNames};
use crate::exec;

const HARNESS: &str = include_str!("JvmOpcodes.java");

/// REVIEW findings that are expected, by case name and finding, with the
/// reason. A finding of `*` covers every REVIEW finding of the case; those are
/// the known jvm-reader bugs `jvm:asm-flow` also lists, and are stale once the
/// case has none.
pub const EXPECTED_REVIEWS: &[(&str, &str, &str)] = &[
    ("dup_x1", "*", DUP),
    ("dup_x2_ints", "*", DUP),
    ("dup_x2_long", "*", DUP),
    ("dup2_ints", "*", DUP),
    ("dup2_x1_ints", "*", DUP),
    ("dup2_x1_long", "*", DUP),
    ("dup2_x2_ints", "*", DUP),
    ("dup2_x2_long_over_ints", "*", DUP),
    ("dup2_x2_ints_over_long", "*", DUP),
    ("dup2_x2_long_over_long", "*", DUP),
    ("swap", "*", SWAP),
    ("swap_refs", "*", SWAP),
    (
        "l2i",
        "out1 lists in2, which never changed it",
        "Truncation drops the high word, but l2i pops the whole long.",
    ),
    (
        "d2i",
        "out1 lists in1, which never changed it",
        "The low word holds only bits below the integer part of the candidates.",
    ),
    (
        "d2l",
        "out1 lists in1, which never changed it",
        "The low word holds only bits below the integer part of the candidates.",
    ),
    (
        "d2l",
        "out2 lists in1, which never changed it",
        "The low word holds only bits below the integer part of the candidates.",
    ),
];

const DUP: &str = "jvm-reader sends every input of dup_x* and dup2* to every output \
     (jvm-reader improvements doc 3.2).";
const SWAP: &str = "jvm-reader sends both inputs of swap to both outputs (3.4).";

/// Opcodes the harness does not run; see the module comment.
const NOT_RUN: &[u8] = &[
    0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, // if<cond>
    0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, // if_icmp<cond>, if_acmp<cond>
    0xa7, 0xa8, 0xa9, // goto, jsr, ret
    0xaa, 0xab, // tableswitch, lookupswitch
    0xac, 0xad, 0xae, 0xaf, 0xb0, 0xb1, // returns
    0xc6, 0xc7, 0xc8, 0xc9, // ifnull, ifnonnull, goto_w, jsr_w
];

/// One case as the harness ran it.
#[derive(Debug, Default)]
struct Observed {
    name: String,
    class: String,
    opcode: u8,
    index: usize,
    height: usize,
    /// Place -> (written by the baseline run, input slots that changed it).
    places: BTreeMap<String, (bool, BTreeSet<String>)>,
    errors: Vec<String>,
}

/// Run every case and judge jvm-reader against it. Everything the run
/// produces, including `report.txt`, lands in `work`.
pub fn check(cp: &str, work: &Path) -> Result<()> {
    exec::fresh_dir(work)?;
    let harness = work.join("JvmOpcodes.java");
    std::fs::write(&harness, HARNESS).with_context(|| format!("writing {}", harness.display()))?;
    let mut cmd = Command::new("java");
    cmd.arg("-cp").arg(cp).arg(&harness).arg(work);
    exec::run_checked(cmd, "java JvmOpcodes")?;
    let results = std::fs::read_to_string(work.join("results.txt"))
        .with_context(|| format!("reading {}/results.txt", work.display()))?;
    let observed = parse_results(&results)?;

    let classes = work.join("classes");
    let mut report = String::new();
    let (mut failed, mut unexpected) = (0, 0);
    let mut seen_expected = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for case in &observed {
        let class = classes.join(format!("{}.class", case.class));
        let (fails, reviews) = match judge(case, &class, &mut covered) {
            Ok(v) => v,
            Err(err) => (vec![format!("{err:#}")], Vec::new()),
        };
        let reviews: Vec<(String, bool)> = reviews
            .into_iter()
            .map(|r| {
                let hit = EXPECTED_REVIEWS
                    .iter()
                    .find(|(n, f, _)| *n == case.name && (*f == r || *f == "*"));
                let expected = hit.is_some();
                if let Some((_, f, _)) = hit {
                    seen_expected.insert((case.name.clone(), f.to_string()));
                }
                (r, expected)
            })
            .collect();
        let status = if !fails.is_empty() {
            failed += 1;
            "FAIL"
        } else if reviews.iter().any(|(_, expected)| !expected) {
            unexpected += 1;
            "REVIEW"
        } else {
            "PASS"
        };
        writeln!(report, "{status:6} {}", case.name)?;
        for f in &fails {
            writeln!(report, "         fail: {f}")?;
        }
        for (r, expected) in &reviews {
            let tag = if *expected { "expected" } else { "review" };
            writeln!(report, "         {tag}: {r}")?;
        }
    }
    let mut stale = 0;
    for (name, finding, _) in EXPECTED_REVIEWS {
        if !seen_expected.contains(&(name.to_string(), finding.to_string())) {
            stale += 1;
            writeln!(
                report,
                "STALE  {name}: expected review no longer occurs: {finding}"
            )?;
        }
    }
    let missing: Vec<String> = (0u8..=0xc9)
        .filter(|op| !covered.contains(op) && !NOT_RUN.contains(op))
        .map(|op| format!("0x{op:02x}"))
        .collect();
    if !missing.is_empty() {
        writeln!(report, "UNCOVERED opcodes: {}", missing.join(", "))?;
    }
    writeln!(
        report,
        "\n{} cases: {failed} failed, {unexpected} need review, {stale} stale expectations",
        observed.len()
    )?;
    let path = work.join("report.txt");
    std::fs::write(&path, &report)?;
    if failed > 0 || unexpected > 0 || stale > 0 || !missing.is_empty() {
        bail!(
            "jvm-reader disagrees with the JVM (see {}):\n{}",
            path.display(),
            report
                .lines()
                .filter(|l| !l.starts_with("PASS") && !l.contains("expected: "))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

fn parse_results(text: &str) -> Result<Vec<Observed>> {
    let mut out = Vec::new();
    let mut cur: Option<Observed> = None;
    for line in text.lines() {
        let mut words = line.split(' ');
        match words.next() {
            Some("case") => {
                let mut c = Observed {
                    name: words.next().context("case name")?.to_string(),
                    class: words.next().context("case class")?.to_string(),
                    ..Observed::default()
                };
                for w in words {
                    let (k, v) = w.split_once('=').with_context(|| format!("bad {line}"))?;
                    match k {
                        "op" => c.opcode = v.parse()?,
                        "index" => c.index = v.parse()?,
                        "h" => c.height = v.parse()?,
                        _ => bail!("bad {line}"),
                    }
                }
                cur = Some(c);
            }
            Some("place") => {
                let c = cur
                    .as_mut()
                    .with_context(|| format!("{line} outside a case"))?;
                let place = words.next().context("place name")?.to_string();
                let changed = words.next() == Some("changed=1");
                let influence = words
                    .next()
                    .and_then(|w| w.strip_prefix("influence="))
                    .context("influence")?
                    .split(',')
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
                    .collect();
                c.places.insert(place, (changed, influence));
            }
            Some("error") => {
                let c = cur
                    .as_mut()
                    .with_context(|| format!("{line} outside a case"))?;
                c.errors.push(line["error ".len()..].to_string());
            }
            Some("end") => out.push(cur.take().context("end outside a case")?),
            _ => bail!("bad results line: {line}"),
        }
    }
    Ok(out)
}

fn is_slot(atom: &str) -> bool {
    ["in", "local"].iter().any(|p| {
        atom.strip_prefix(p)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The input slots an atom of [`asm_flow::claims`] reads: itself, or the
/// slots that locate a field or element it loads. Constants, allocations,
/// static fields and call results read none.
fn slots_of(atom: &str, out: &mut BTreeSet<String>) {
    if is_slot(atom) {
        out.insert(atom.to_string());
    } else if let Some(field) = atom.strip_prefix("field:") {
        if let Some((_, object)) = field.split_once('@') {
            out.insert(object.to_string());
        }
    } else if let Some(elem) = atom.strip_prefix("elem:") {
        if let Some((array, rest)) = elem.split_once('[') {
            out.insert(array.to_string());
            out.insert(rest.trim_end_matches(']').to_string());
        }
    }
}

/// The input slot that names the object a heap place belongs to.
fn owner(place: &str) -> Option<&str> {
    if let Some(field) = place.strip_prefix("field:") {
        return field.split_once('@').map(|(_, o)| o);
    }
    place.strip_prefix("elem:")
}

/// The slot a stack slot or local holds when the instruction leaves it alone.
fn identity(place: &str) -> Option<String> {
    if let Some(p) = place.strip_prefix("out") {
        return Some(format!("in{p}"));
    }
    place.starts_with("local").then(|| place.to_string())
}

/// FAIL and REVIEW findings for one case. Records the opcodes it ran in
/// `covered`, as raw bytes, so the short and wide forms count separately.
fn judge(
    case: &Observed,
    class: &Path,
    covered: &mut BTreeSet<u8>,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut fails: Vec<String> = case
        .errors
        .iter()
        .map(|e| format!("harness: {e}"))
        .collect();
    let mut reviews = Vec::new();

    let bytes = std::fs::read(class).with_context(|| format!("reading {}", class.display()))?;
    let parser =
        ClassFileParser::parse(&bytes).map_err(|e| anyhow!("parsing {}: {e}", class.display()))?;
    let cf = parser.class_file();
    let method = parser
        .methods()
        .find(|m| cf.get_utf8(m.name_index).is_ok_and(|n| n == "run"))
        .context("no run()")?;
    let code = &method.code.as_ref().context("run() has no code")?.code;
    let cfg =
        compute_basic_blocks_for_method(cf, method).map_err(|e| anyhow!("jvm-reader: {e}"))?;
    let info = cfg
        .instructions()
        .get(case.index)
        .with_context(|| format!("no instruction {}", case.index))?;
    covered.insert(code[info.pc as usize]);
    covered.insert(info.opcode);
    if asm_flow::asm_opcode(info.opcode) != case.opcode {
        bail!(
            "instruction {} is {} (0x{:02x}), not opcode {}",
            case.index,
            info.mnemonic,
            info.opcode,
            case.opcode
        );
    }

    // jvm-reader's places, each with the input slots it draws from. An array
    // store's place is the array; the index says where in it, so it counts as
    // a source.
    let claims = asm_flow::claims(info, case.height, &LocalNames::new(method, "()V"));
    let mut claimed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (place, atoms) in &claims.places {
        let mut slots = BTreeSet::new();
        for atom in atoms {
            slots_of(atom, &mut slots);
        }
        let place = match place.strip_prefix("elem:").and_then(|e| e.split_once('[')) {
            Some((array, index)) => {
                slots.insert(index.trim_end_matches(']').to_string());
                format!("elem:{array}")
            }
            None => place.clone(),
        };
        claimed.entry(place).or_default().extend(slots);
    }

    let empty = (false, BTreeSet::new());
    let places: BTreeSet<&String> = claimed.keys().chain(case.places.keys()).collect();
    for place in places {
        let (changed, influence) = case.places.get(place).unwrap_or(&empty);
        let mut influence = influence.clone();
        if let Some(o) = owner(place) {
            influence.remove(o);
        }
        let same = identity(place);
        match claimed.get(place) {
            None => {
                let untouched = !changed
                    && influence
                        .iter()
                        .all(|a| Some(a.as_str()) == same.as_deref());
                if !untouched {
                    let from = influence.iter().cloned().collect::<Vec<_>>().join(",");
                    fails.push(format!(
                        "writes {place} (from {from}), which jvm-reader does not list"
                    ));
                }
            }
            Some(sources) => {
                for atom in influence.difference(sources) {
                    fails.push(format!(
                        "{atom} changes {place}, but jvm-reader does not draw it from {atom}"
                    ));
                }
                let is_identity = sources.len() == 1 && sources.first() == same.as_ref();
                if !changed && influence.is_empty() && !is_identity {
                    reviews.push(format!("{place} is listed but was never written"));
                }
                for atom in sources.difference(&influence) {
                    if !is_identity {
                        reviews.push(format!("{place} lists {atom}, which never changed it"));
                    }
                }
            }
        }
    }
    Ok((fails, reviews))
}
