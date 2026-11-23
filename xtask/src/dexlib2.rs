//! Differential decoding: dex-reader against dexlib2, instruction by instruction.
//!
//! dexlib2 is the library inside baksmali. `DumpDexlib2Instructions.java` prints every
//! instruction of a `.dex` as dexlib2 decodes it; [`dump_ours`] prints the same canonical form
//! from dex-reader's decoder. The two must agree line for line: mnemonic, offset (and so size),
//! registers, literal, branch offset, constant-pool indices, and payload contents.
//!
//! The textual `dex:baksmali` check compares disassembly, which goes through display code and a
//! battery of normalizations. This check compares decoded operands directly, and runs over
//! `xtask/tests/dex/all-opcodes.smali`, which holds every opcode with edge-case operands. The
//! `javac` + `dx` samples hold only the opcodes `dx` emits.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use dex_reader::DexParser;
use dex_reader::instructions::{DALVIK_OPCODES, Format, Instruction, PayloadInstruction};
use dex_reader::parser::{DecodedCodeItem, decode_code_item};

use crate::exec;

const DUMPER: &str = include_str!("DumpDexlib2Instructions.java");

pub const ALL_OPCODES_SMALI: &str = include_str!("../tests/dex/all-opcodes.smali");

/// Each method's instruction lines, keyed by method index.
type Dump = BTreeMap<u32, Vec<String>>;

/// The baksmali fat jar, which bundles dexlib2. `DEXLIB2_JAR` names it explicitly; otherwise it
/// is the jar the `baksmali` wrapper on `PATH` runs (the Nix wrapper is `java -jar <jar>`).
pub fn find_jar() -> Option<PathBuf> {
    if let Some(jar) = std::env::var_os("DEXLIB2_JAR") {
        return Some(PathBuf::from(jar));
    }
    let wrapper = std::fs::read_to_string(exec::which("baksmali")?).ok()?;
    wrapper
        .split_whitespace()
        .find(|tok| tok.ends_with(".jar"))
        .map(|tok| PathBuf::from(tok.trim_matches(|c| c == '"' || c == '\'')))
        .filter(|jar| jar.is_file())
}

/// Assemble the all-opcodes fixture into `work`, returning the `.dex` path.
pub fn assemble_all_opcodes(work: &Path) -> Result<PathBuf> {
    exec::fresh_dir(work)?;
    let smali = work.join("all-opcodes.smali");
    std::fs::write(&smali, ALL_OPCODES_SMALI)
        .with_context(|| format!("writing {}", smali.display()))?;
    let dex = work.join("all-opcodes.dex");
    let mut cmd = Command::new("smali");
    cmd.args(["assemble", "--api", "28", "-o"])
        .arg(&dex)
        .arg(&smali);
    exec::run_checked(cmd, "smali assemble")?;
    Ok(dex)
}

/// Decode `dex` with both readers and require identical dumps. `work` holds the dumper and both
/// dumps, so a failure can be inspected. Returns the number of instructions compared.
pub fn compare(jar: &Path, dex: &Path, bytes: &[u8], work: &Path) -> Result<usize> {
    std::fs::create_dir_all(work).with_context(|| format!("creating {}", work.display()))?;
    let dumper = work.join("DumpDexlib2Instructions.java");
    std::fs::write(&dumper, DUMPER).with_context(|| format!("writing {}", dumper.display()))?;
    let mut cmd = Command::new("java");
    cmd.arg("-cp").arg(jar).arg(&dumper).arg(dex);
    let theirs_text = exec::capture_stdout(cmd, "java DumpDexlib2Instructions")?;
    let parser =
        DexParser::new(bytes).map_err(|e| anyhow!("DexParser for {}: {e}", dex.display()))?;
    let ours_text = dump_ours(&parser)?;

    let stem = dex.file_stem().and_then(|s| s.to_str()).unwrap_or("dex");
    std::fs::write(work.join(format!("{stem}.dexlib2.txt")), &theirs_text)?;
    std::fs::write(work.join(format!("{stem}.dex-reader.txt")), &ours_text)?;

    let theirs = parse_dump(&theirs_text)?;
    let ours = parse_dump(&ours_text)?;
    let mut problems = Vec::new();
    for (method, want) in &theirs {
        let Some(got) = ours.get(method) else {
            problems.push(format!("method {method}: missing from dex-reader"));
            continue;
        };
        let n = want.len().max(got.len());
        for i in 0..n {
            let (w, g) = (want.get(i), got.get(i));
            if w != g {
                problems.push(format!(
                    "method {method}:\n  dexlib2:    {}\n  dex-reader: {}",
                    w.map_or("<end>", String::as_str),
                    g.map_or("<end>", String::as_str)
                ));
                // Later lines are misaligned once an offset differs.
                break;
            }
        }
    }
    for method in ours.keys().filter(|m| !theirs.contains_key(m)) {
        problems.push(format!("method {method}: missing from dexlib2"));
    }
    if !problems.is_empty() {
        let shown = problems.iter().take(20).cloned().collect::<Vec<_>>();
        bail!(
            "{} methods of {} decode differently (dumps in {}):\n{}",
            problems.len(),
            dex.display(),
            work.display(),
            shown.join("\n")
        );
    }
    Ok(theirs.values().map(Vec::len).sum())
}

fn parse_dump(text: &str) -> Result<Dump> {
    let mut out = Dump::new();
    let mut current: Option<u32> = None;
    for line in text.lines() {
        if let Some(idx) = line.strip_prefix("method ") {
            let idx: u32 = idx
                .parse()
                .with_context(|| format!("bad method line {line}"))?;
            out.entry(idx).or_default();
            current = Some(idx);
        } else {
            let method = current.with_context(|| format!("instruction before method: {line}"))?;
            out.get_mut(&method).unwrap().push(line.to_string());
        }
    }
    Ok(out)
}

/// dex-reader's decoding of every method in `parser`, in `DumpDexlib2Instructions.java`'s form.
pub fn dump_ours(parser: &DexParser<'_>) -> Result<String> {
    let mut out = String::new();
    for class_def in parser.classes() {
        let class_data = parser
            .class_data(class_def)
            .map_err(|e| anyhow!("class data: {e}"))?;
        for enc in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            let Some(code) = enc
                .code(parser.data)
                .map_err(|e| anyhow!("code item: {e}"))?
            else {
                continue;
            };
            writeln!(out, "method {}", enc.method_idx)?;
            for item in decode_code_item(&code) {
                writeln!(out, "{} {}", item.offset(), describe(&item))?;
            }
        }
    }
    Ok(out)
}

fn join<T: ToString>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn describe(item: &DecodedCodeItem) -> String {
    match item {
        DecodedCodeItem::Payload { payload, .. } => describe_payload(payload),
        DecodedCodeItem::Instruction { inst, .. } => describe_instruction(inst),
    }
}

fn describe_payload(payload: &PayloadInstruction) -> String {
    match payload {
        PayloadInstruction::PackedSwitch(p) => {
            let keys = (0..p.targets.len()).map(|i| p.first_key.wrapping_add(i as i32));
            format!(
                "packed-switch-payload keys={} targets={}",
                join(keys),
                join(&p.targets)
            )
        }
        PayloadInstruction::SparseSwitch(p) => format!(
            "sparse-switch-payload keys={} targets={}",
            join(&p.keys),
            join(&p.targets)
        ),
        PayloadInstruction::FillArrayData(p) => {
            let width = usize::from(p.element_width);
            let values = p.data.chunks(width.max(1)).map(|c| {
                let mut v: u64 = 0;
                for (i, b) in c.iter().enumerate() {
                    v |= u64::from(*b) << (8 * i);
                }
                // Sign-extend from the element width, as dexlib2 does.
                let shift = 64 - 8 * c.len() as u32;
                ((v << shift) as i64) >> shift
            });
            format!("array-payload width={width} values={}", join(values))
        }
    }
}

fn describe_instruction(inst: &Instruction) -> String {
    use Format::*;
    let mut regs: Vec<u32> = Vec::new();
    let mut lit: Option<i64> = None;
    let mut off: Option<i32> = None;
    let mut idx: Vec<u32> = Vec::new();
    match inst.format() {
        Format10x(_) | FormatUnused(_) => {}
        Format10t(f) => off = Some(f.off.into()),
        Format20t(f) => off = Some(f.off.into()),
        Format30t(f) => off = Some(f.off),
        Format11x(f) => regs = vec![f.a.0],
        Format12x(f) => regs = vec![f.a.0, f.b.0],
        Format11n(f) => (regs, lit) = (vec![f.a.0], Some(f.lit.into())),
        Format21s(f) => (regs, lit) = (vec![f.a.0], Some(f.lit.into())),
        Format31i(f) => (regs, lit) = (vec![f.a.0], Some(f.lit.into())),
        Format51l(f) => (regs, lit) = (vec![f.a.0], Some(f.lit)),
        Format21h(f) => {
            // The literal is the high 16 bits of the value loaded.
            let value = match inst {
                Instruction::ConstWideHigh16(_) => i64::from(f.lit) << 48,
                _ => i64::from(i32::from(f.lit) << 16),
            };
            (regs, lit) = (vec![f.a.0], Some(value));
        }
        Format21t(f) => (regs, off) = (vec![f.a.0], Some(f.tgt)),
        Format22t(f) => (regs, off) = (vec![f.a.0, f.b.0], Some(f.tgt.into())),
        Format31t(f) => (regs, off) = (vec![f.a.0], Some(f.tgt)),
        Format22x(f) => regs = vec![f.a.0, f.b.0],
        Format32x(f) => regs = vec![f.a.0, f.b.0],
        Format23x(f) => regs = vec![f.a.0, f.b.0, f.c.0],
        Format22b(f) => (regs, lit) = (vec![f.a.0, f.b.0], Some(f.c.into())),
        Format22s(f) => (regs, lit) = (vec![f.a.0, f.b.0], Some(f.lit.into())),
        Format21cF(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format21cM(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format21cH(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format21cP(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format21cS(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format21cT(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format31c(f) => (regs, idx) = (vec![f.a.0], vec![f.idx.0]),
        Format22cF(f) => (regs, idx) = (vec![f.a.0, f.b.0], vec![f.idx.0]),
        Format22cT(f) => (regs, idx) = (vec![f.a.0, f.b.0], vec![f.idx.0]),
        Format35cC(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format35cM(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format35cT(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format3rcC(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format3rcM(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format3rcT(f) => (regs, idx) = (f.args.iter().map(|r| r.0).collect(), vec![f.idx.0]),
        Format45cc(f) => {
            regs = f.args.iter().map(|r| r.0).collect();
            idx = vec![f.method_id.0, f.proto_id.0];
        }
        Format4rcc(f) => {
            regs = f.args.iter().map(|r| r.0).collect();
            idx = vec![f.method_id.0, f.proto_id.0];
        }
    }

    let mut parts = vec![DALVIK_OPCODES[usize::from(inst.discriminant())].to_string()];
    if !regs.is_empty() {
        parts.push(format!("r={}", join(regs)));
    }
    if let Some(lit) = lit {
        parts.push(format!("lit={lit}"));
    }
    if let Some(off) = off {
        parts.push(format!("off={off}"));
    }
    if let Some(i) = idx.first() {
        parts.push(format!("idx={i}"));
    }
    if let Some(i) = idx.get(1) {
        parts.push(format!("idx2={i}"));
    }
    parts.join(" ")
}

/// Opcode bytes (other than unused ones) that no method of `parser` contains.
pub fn missing_opcodes(parser: &DexParser<'_>) -> Result<Vec<&'static str>> {
    let mut seen = [false; 256];
    for class_def in parser.classes() {
        let class_data = parser
            .class_data(class_def)
            .map_err(|e| anyhow!("class data: {e}"))?;
        for enc in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            if let Some(code) = enc
                .code(parser.data)
                .map_err(|e| anyhow!("code item: {e}"))?
            {
                for item in decode_code_item(&code) {
                    if let DecodedCodeItem::Instruction { inst, .. } = item {
                        seen[usize::from(inst.discriminant())] = true;
                    }
                }
            }
        }
    }
    Ok(DALVIK_OPCODES
        .iter()
        .zip(seen)
        .filter(|(name, seen)| !seen && !name.starts_with("unused"))
        .map(|(name, _)| *name)
        .collect())
}
