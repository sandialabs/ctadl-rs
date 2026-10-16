//! Differential dataflow: jvm-reader against ASM, instruction by instruction.
//!
//! `AsmFlow.java` runs each instruction of a `.class` through ASM's own model
//! of the JVM (`Frame.execute`) and prints where every value goes: which stack
//! slots and locals the instruction writes and what each now holds, what it
//! stores into the heap, and what it passes to a call. [`dump_ours`] prints the
//! same form from jvm-reader's decoded dataflow (`compute_basic_blocks_for_method`,
//! before normalization). The two must agree line for line. The file comment
//! of `AsmFlow.java` describes the form.
//!
//! jvm-reader's dataflow is read the way the frontend reads it:
//!
//! - Each destination depends on every source the instruction lists.
//! - A stack slot the instruction pushes but no dataflow names keeps whatever
//!   was there before, because the frontend emits nothing for it and SSA
//!   leaves the old definition in place. That is how `checkcast` passes its
//!   input through.
//! - A field's object is the stack source `getfield` reads, and the deepest
//!   stack source of `putfield` (`ctadl-jvm` takes it from there too).
//! - A call's return value fills every slot the call pushes.
//!
//! ASM decides the rest independently: the stack height before each
//! instruction, how many slots it pops and pushes, the JVMS permutations of
//! `dup_x*` and `swap`, and the operand positions of every load, store and
//! call. The inputs are the compiled jvm-reader samples, the nightly Java
//! cases, and `AllOpcodes.java`, which builds a class holding every opcode,
//! including those javac never emits.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use jvm_reader::flow::compute_basic_blocks_for_method;
use jvm_reader::{
    instruction_stack_effect, ClassFileParser, ConstantValue, InstructionFlowInfo, Location,
    MethodInfo, ParameterSlotMap,
};

use crate::exec;

const ORACLE: &str = include_str!("AsmFlow.java");
const ALL_OPCODES: &str = include_str!("AllOpcodes.java");

/// The ASM jars' classpath. `ASM_CLASSPATH` names it explicitly; otherwise the
/// `asm-classpath` script the Nix toolchain puts on `PATH` prints it.
pub fn find_classpath() -> Option<String> {
    if let Some(cp) = std::env::var_os("ASM_CLASSPATH") {
        return cp.into_string().ok();
    }
    let script = exec::which("asm-classpath")?;
    let out = Command::new(script).output().ok()?;
    let cp = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !cp.is_empty()).then_some(cp)
}

/// Build `AllOpcodes.class` into `work` and return its path. The check fails
/// if the class stops covering an opcode.
pub fn build_all_opcodes(cp: &str, work: &Path) -> Result<PathBuf> {
    exec::fresh_dir(work)?;
    let generator = work.join("AllOpcodes.java");
    std::fs::write(&generator, ALL_OPCODES)
        .with_context(|| format!("writing {}", generator.display()))?;
    let mut cmd = Command::new("java");
    cmd.arg("-cp").arg(cp).arg(&generator).arg(work);
    exec::run_checked(cmd, "java AllOpcodes")?;
    Ok(work.join("AllOpcodes.class"))
}

/// The opcodes no instruction in `classes` uses, by name. `wide` counts as used
/// when any instruction carries the prefix.
pub fn missing_opcodes(classes: &[PathBuf]) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    for path in classes {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let parser = ClassFileParser::parse(&bytes)
            .map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
        let cf = parser.class_file();
        for method in parser.methods() {
            let Some(code) = &method.code else { continue };
            // A method jvm-reader cannot decode is reported by `compare`.
            let Ok(cfg) = compute_basic_blocks_for_method(cf, method) else {
                continue;
            };
            for info in cfg.instructions() {
                seen.insert(code.code[info.pc as usize]);
                seen.insert(info.opcode);
            }
        }
    }
    let mut missing = Vec::new();
    for op in 0u8..=0xc9 {
        if !seen.contains(&op) {
            missing.push(format!("0x{op:02x}"));
        }
    }
    Ok(missing)
}

/// Opcodes on which jvm-reader is known to disagree with ASM, and why. A
/// disagreement on one of these is reported but does not fail the check, and
/// an entry the all-opcodes class no longer trips does, so that fixing a bug
/// means deleting its entry here. The opcode is the one ASM reports.
pub const KNOWN: &[(u8, &str)] = &[
    (0x0c, "fconst_1 holds the bits 0x1, not 1.0f"),
    (0x0d, "fconst_2 holds the bits 0x2, not 2.0f"),
    (0x0f, "dconst_1 holds the bits 0x1, not 1.0"),
    (
        0x5a,
        "dup_x1 sends every input to every output (jvm-reader improvements doc 3.2)",
    ),
    (0x5b, "dup_x2 sends every input to every output (3.2)"),
    (
        0x5c,
        "dup2 sends every input to every output, and rewrites its inputs (3.2)",
    ),
    (0x5d, "dup2_x1 sends every input to every output (3.2)"),
    (0x5e, "dup2_x2 sends every input to every output (3.2)"),
    (0x5f, "swap sends both inputs to both outputs (3.4)"),
    (
        0xbd,
        "anewarray of an array type allocates [L[I; rather than [[I",
    ),
];

/// A method jvm-reader cannot decode at all, known when ASM's dump of it
/// shows an `ldc` of one of these constants.
pub const KNOWN_UNDECODABLE: (&[&str], &str) = (
    &["=const:MT", "=const:MH", "=const:CD"],
    "ldc of a MethodType, MethodHandle or dynamic constant fails the method (3.4)",
);

/// What [`compare`] found.
#[derive(Debug, Default)]
pub struct Report {
    /// Instructions compared (live ones, in methods both sides dumped).
    pub compared: usize,
    /// Disagreements on a [`KNOWN`] opcode or an undecodable method, by reason.
    pub known: BTreeMap<&'static str, usize>,
}

/// Dump `classes` with both ASM and jvm-reader and require identical dumps,
/// except for [`KNOWN`] disagreements. `work` holds the oracle, both dumps and
/// every disagreement, so a failure can be inspected.
pub fn compare(cp: &str, label: &str, classes: &[PathBuf], work: &Path) -> Result<Report> {
    std::fs::create_dir_all(work).with_context(|| format!("creating {}", work.display()))?;
    let oracle = work.join("AsmFlow.java");
    std::fs::write(&oracle, ORACLE).with_context(|| format!("writing {}", oracle.display()))?;
    let mut cmd = Command::new("java");
    cmd.arg("-cp").arg(cp).arg(&oracle).args(classes);
    let theirs_text = exec::capture_stdout(cmd, "java AsmFlow")?;
    std::fs::write(work.join(format!("{label}.asm.txt")), &theirs_text)?;

    let theirs = parse_dump(&theirs_text)?;
    let ours_text = dump_ours(classes, &theirs)?;
    std::fs::write(work.join(format!("{label}.jvm-reader.txt")), &ours_text)?;
    let ours = parse_dump(&ours_text)?;

    let mut report = Report::default();
    let mut problems = Vec::new();
    let mut all = String::new();
    for (method, want) in &theirs {
        let got = ours.get(method);
        if got.is_some_and(|g| g.first().is_some_and(|l| l.starts_with("error "))) {
            let (markers, reason) = KNOWN_UNDECODABLE;
            let known = want.iter().any(|l| markers.iter().any(|m| l.contains(m)));
            let error = &got.unwrap()[0];
            writeln!(all, "{method}: {error}")?;
            if known {
                *report.known.entry(reason).or_default() += 1;
            } else {
                problems.push(format!("{method}: jvm-reader: {error}"));
            }
            continue;
        }
        let Some(got) = got else {
            problems.push(format!("{method}: missing from jvm-reader"));
            continue;
        };
        for i in 0..want.len().max(got.len()) {
            let (w, g) = (want.get(i), got.get(i));
            if w.is_some_and(|w| w.ends_with(" dead")) {
                continue;
            }
            report.compared += 1;
            if w == g {
                continue;
            }
            let text = format!(
                "{method}:\n  asm:        {}\n  jvm-reader: {}",
                w.map_or("<end>", String::as_str),
                g.map_or("<end>", String::as_str)
            );
            writeln!(all, "{text}")?;
            match w.and_then(|w| known_reason(w)) {
                Some(reason) => *report.known.entry(reason).or_default() += 1,
                None => problems.push(text),
            }
        }
    }
    for method in ours.keys().filter(|m| !theirs.contains_key(*m)) {
        problems.push(format!("{method}: missing from asm"));
    }
    let disagreements = work.join(format!("{label}.disagreements.txt"));
    std::fs::write(&disagreements, &all)?;
    if !problems.is_empty() {
        let shown = problems.iter().take(20).cloned().collect::<Vec<_>>();
        bail!(
            "{} instructions of {label} disagree (all in {}):\n{}",
            problems.len(),
            disagreements.display(),
            shown.join("\n")
        );
    }
    Ok(report)
}

/// The [`KNOWN`] reason for a disagreement on ASM's line `line`, if any.
fn known_reason(line: &str) -> Option<&'static str> {
    let op: u8 = line
        .split(' ')
        .find_map(|w| w.strip_prefix("op="))?
        .parse()
        .ok()?;
    KNOWN.iter().find(|(k, _)| *k == op).map(|(_, why)| *why)
}

/// Each method's lines, keyed by `<class> <name><descriptor>`.
type Dump = BTreeMap<String, Vec<String>>;

fn parse_dump(text: &str) -> Result<Dump> {
    let mut out = Dump::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.starts_with("class ") {
            continue;
        }
        if let Some(method) = line.strip_prefix("method ") {
            if out.insert(method.to_string(), Vec::new()).is_some() {
                bail!("method {method} appears twice");
            }
            current = Some(method.to_string());
        } else {
            let method = current
                .as_ref()
                .with_context(|| format!("instruction before method: {line}"))?;
            out.get_mut(method).unwrap().push(line.to_string());
        }
    }
    Ok(out)
}

/// jvm-reader's dataflow for every method in `classes`, in `AsmFlow.java`'s
/// form. Stack positions need the height before each instruction, which is
/// taken from ASM's dump (`theirs`), so that an instruction whose stack effect
/// is wrong shows up on its own line rather than shifting every line after it.
fn dump_ours(classes: &[PathBuf], theirs: &Dump) -> Result<String> {
    let mut out = String::new();
    for path in classes {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let parser = ClassFileParser::parse(&bytes)
            .map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
        let cf = parser.class_file();
        let class = cf
            .this_class_name()
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        writeln!(out, "class {class}")?;
        for method in parser.methods() {
            if method.code.is_none() {
                continue;
            }
            let name = cf.get_utf8(method.name_index).map_err(|e| anyhow!("{e}"))?;
            let desc = cf
                .get_utf8(method.descriptor_index)
                .map_err(|e| anyhow!("{e}"))?;
            let key = format!("{class} {name}{desc}");
            writeln!(out, "method {key}")?;
            let heights = theirs.get(&key).map(|lines| heights(lines)).transpose()?;
            let cfg = match compute_basic_blocks_for_method(cf, method) {
                Ok(cfg) => cfg,
                Err(e) => {
                    writeln!(out, "error {}", e.to_string().replace('\n', " "))?;
                    continue;
                }
            };
            let locals = LocalNames::new(method, desc);
            for (idx, info) in cfg.instructions().iter().enumerate() {
                let Some(before) = heights.as_ref().and_then(|h| h.get(idx).copied().flatten())
                else {
                    // Dead code, or a method ASM did not dump: nothing to place
                    // the stack against.
                    writeln!(out, "{idx} dead")?;
                    continue;
                };
                writeln!(out, "{}", render(idx, info, before, &locals))?;
            }
        }
    }
    Ok(out)
}

/// The stack height before each instruction of one method in ASM's dump, or
/// `None` where ASM found it unreachable.
fn heights(lines: &[String]) -> Result<Vec<Option<usize>>> {
    lines
        .iter()
        .map(|line| {
            if line.ends_with(" dead") {
                return Ok(None);
            }
            let h = line
                .split(' ')
                .find_map(|w| w.strip_prefix("h="))
                .and_then(|h| h.split("->").next())
                .with_context(|| format!("no height in {line}"))?;
            Ok(Some(h.parse()?))
        })
        .collect()
}

/// Maps jvm-reader's `Parameter(ordinal)` back to the local it lives in.
pub(crate) struct LocalNames {
    first_slot: Vec<u16>,
}

impl LocalNames {
    pub(crate) fn new(method: &MethodInfo, desc: &str) -> Self {
        let is_instance = method.access_flags & 0x0008 == 0;
        let map = ParameterSlotMap::for_method(desc, is_instance);
        let mut first_slot = Vec::new();
        for slot in 0..map.slot_count() as u16 {
            if let Some(ordinal) = map.ordinal_for_slot(slot) {
                if ordinal as usize == first_slot.len() {
                    first_slot.push(slot);
                }
            }
        }
        LocalNames { first_slot }
    }

    fn parameter(&self, ordinal: u16) -> String {
        match self.first_slot.get(ordinal as usize) {
            Some(slot) => format!("local{slot}"),
            None => format!("param?{ordinal}"),
        }
    }
}

fn atoms<const N: usize>(a: [String; N]) -> BTreeSet<String> {
    a.into_iter().collect()
}

/// ASM reports the long forms of instructions that have a short one.
fn asm_opcode(op: u8) -> u8 {
    match op {
        0x1a..=0x2d => 0x15 + (op - 0x1a) / 4, // xload_<n>
        0x3b..=0x4e => 0x36 + (op - 0x3b) / 4, // xstore_<n>
        0x13 | 0x14 => 0x12,                   // ldc_w, ldc2_w
        0xc8 => 0xa7,                          // goto_w
        0xc9 => 0xa8,                          // jsr_w
        _ => op,
    }
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

fn constant(c: &ConstantValue) -> String {
    match c {
        ConstantValue::Integer(v) => format!("const:I{v}"),
        ConstantValue::Long(v) => format!("const:J{v}"),
        ConstantValue::Float(bits) => format!("const:F{bits:x}"),
        ConstantValue::Double(bits) => format!("const:D{bits:x}"),
        ConstantValue::String(s) => format!("const:S{}", escape(s)),
        ConstantValue::ClassRef(name) => format!("const:C{name}"),
        ConstantValue::Null => "const:null".to_string(),
    }
}

/// What jvm-reader says one instruction does, read as the module comment
/// describes: the stack height after it, and every place it writes with the
/// atoms that place then holds. Stack slots it pushes are all present, even
/// one that ends up holding what it held before.
pub(crate) struct Claims {
    pub after: usize,
    pub places: BTreeMap<String, BTreeSet<String>>,
}

/// jvm-reader's [`Claims`] for `info`, with `before` slots on the stack.
pub(crate) fn claims(info: &InstructionFlowInfo<'_>, before: usize, locals: &LocalNames) -> Claims {
    let (consume, produce) = instruction_stack_effect(info);
    let remaining = before.saturating_sub(consume);
    let after = remaining + produce;
    let slot = |depth: u8| match before.checked_sub(depth as usize + 1) {
        Some(p) => format!("in{p}"),
        None => format!("in?{depth}"),
    };

    // The object a field access acts on: the stack source of `getfield`, the
    // deepest stack source of `putfield`.
    let object = info
        .dataflow
        .first()
        .and_then(|df| {
            df.sources
                .iter()
                .filter_map(|s| match s {
                    Location::StackInput(d) => Some(*d),
                    _ => None,
                })
                .max()
        })
        .filter(|_| matches!(info.opcode, 0xb4 | 0xb5));

    let atom = |loc: &Location| -> String {
        match loc {
            Location::Register(n) => format!("local{n}"),
            Location::Parameter(o) => locals.parameter(*o),
            Location::Constant(c) => constant(c),
            Location::StackInput(d) => slot(*d),
            Location::FieldRef(f) => match object {
                Some(d) => format!("field:{}.{}@{}", f.class_name, f.field_name, slot(d)),
                None => format!("field:{}.{}", f.class_name, f.field_name),
            },
            Location::ArrayElement { base, offset } => {
                let part = |l: &Location| match l {
                    Location::StackInput(d) => slot(*d),
                    other => format!("{other:?}"),
                };
                format!("elem:{}[{}]", part(base), part(offset))
            }
            Location::Allocation(ty) => format!("alloc:{ty}"),
            Location::StackSlot(id) => format!("slot?{id}"),
            Location::StackOutput => "out?".to_string(),
        }
    };

    let mut effects: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stack: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut pushed = 0;
    for df in &info.dataflow {
        let sources = df.sources.iter().filter(|s| match (s, object) {
            // putfield's object is where the value goes, not part of it.
            (Location::StackInput(d), Some(o)) => info.opcode != 0xb5 || *d != o,
            _ => true,
        });
        let sources: BTreeSet<String> = sources.map(&atom).collect();
        let place = match &df.destination {
            Location::StackOutput => {
                let p = remaining + pushed;
                pushed += 1;
                stack.entry(p).or_default().extend(sources);
                continue;
            }
            // athrow's one dataflow names the thrown value as its destination.
            Location::StackInput(_) if info.opcode == 0xbf => "thrown".to_string(),
            other => atom(other),
        };
        effects.entry(place).or_default().extend(sources);
    }
    if let Some(call) = &info.call {
        if let Some(Location::StackInput(d)) = &call.receiver {
            effects.entry("recv".into()).or_default().insert(slot(*d));
        }
        for (j, arg) in call.arguments.iter().enumerate() {
            effects
                .entry(format!("arg{j}"))
                .or_default()
                .insert(atom(arg));
        }
        if call.return_value.is_some() {
            for p in remaining..after {
                stack.entry(p).or_default().insert("ret".into());
            }
        }
    }
    for p in remaining..after {
        let s = stack.entry(p).or_default();
        if s.is_empty() && p < before {
            s.insert(format!("in{p}"));
        }
    }
    for (p, s) in stack {
        effects.insert(format!("out{p}"), s);
    }
    Claims {
        after,
        places: effects,
    }
}

/// One instruction's line: its [`Claims`], less the stack slots and locals
/// that still hold what they held before, as `AsmFlow.java` prints them.
fn render(
    idx: usize,
    info: &InstructionFlowInfo<'_>,
    before: usize,
    locals: &LocalNames,
) -> String {
    let Claims { after, mut places } = claims(info, before, locals);
    places.retain(|place, s| {
        let identity = match place.strip_prefix("out") {
            Some(p) => {
                p.parse::<usize>().is_ok_and(|p| p < before) && *s == atoms([format!("in{p}")])
            }
            None => place.starts_with("local") && *s == atoms([place.clone()]),
        };
        !identity
    });

    let mut line = format!("{idx} op={} h={before}->{after}", asm_opcode(info.opcode));
    for (place, s) in places {
        let atoms: Vec<&str> = s.iter().map(String::as_str).collect();
        let _ = write!(line, " {place}={}", atoms.join("+"));
    }
    line
}
