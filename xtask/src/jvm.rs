//! jvm-reader regression checks, driven entirely from Java source.
//!
//! For each `tests/sample/*.java` we `javac` it, exercise the jvm-reader library
//! on the resulting `.class` files (disassembly compared against `javap`, plus
//! line-map / basic-block / stack-slot analyses), then bundle them with `jar`
//! and re-check the `.jar` (parsed classes compared against `jar tf`). No
//! compiled `.class`/`.jar` artifacts are committed; everything is built here.
//!
//! These checks are a faithful port of the former `jvm-reader/tests/test_disas.rs`
//! integration tests, restructured to report pass/fail rather than panic and to
//! run against freshly compiled inputs instead of committed binaries.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use jvm_reader::flow::compute_basic_blocks_for_method;
use jvm_reader::types::CpEntry;
use jvm_reader::{
    collect_line_map_entries, disassemble_class_file, disassemble_jar_file, ClassFile,
    ClassFileError, ClassFileParser, InstructionKind, JarFileParser, Location, MethodInfo,
};

use crate::exec;
use crate::regression::Outcome;

/// A compiled sample program.
struct Sample {
    name: String,
    /// `.class` files produced from this sample.
    classes: Vec<PathBuf>,
    /// A jar bundling the compiled classes.
    jar: PathBuf,
}

/// Compile every sample and run all jvm-reader checks. Returns named
/// (case, outcome) pairs to fold into the regression report. If the JDK tools
/// are missing, returns a single Skip (mirrors the pcode Darwin skip).
pub fn run_checks(samples_dir: &Path, work: &Path) -> Result<Vec<(String, Outcome)>> {
    for tool in ["javac", "jar", "javap"] {
        if exec::which(tool).is_none() {
            return Ok(vec![(
                "jvm".to_string(),
                Outcome::Skip(format!("`{tool}` not on PATH")),
            )]);
        }
    }

    let samples = build_samples(samples_dir, work)?;
    if samples.is_empty() {
        bail!("no .java samples found in {}", samples_dir.display());
    }

    let checks: [(&str, fn(&[Sample]) -> Result<()>); 11] = [
        ("jvm:disassemble-class", check_class_disassembly),
        ("jvm:javap", check_javap),
        ("jvm:jar-classes", check_jar_classes),
        ("jvm:jar-disassemble", check_jar_disassembly),
        ("jvm:instruction-flow", check_instruction_flow),
        ("jvm:line-map", check_line_map),
        ("jvm:file-offsets", check_file_offsets),
        ("jvm:basic-blocks", check_basic_blocks),
        ("jvm:stack-slots", check_stack_slots),
        ("jvm:switch-shapes", check_switch_shapes),
        ("jvm:utf8-constants", check_utf8_constants),
    ];

    Ok(checks
        .into_iter()
        .map(|(name, run)| (name.to_string(), to_outcome(run(&samples))))
        .collect())
}

fn to_outcome(result: Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Pass,
        Err(err) => Outcome::Fail(format!("{err:#}")),
    }
}

// --- compilation ----------------------------------------------------------

fn build_samples(samples_dir: &Path, work: &Path) -> Result<Vec<Sample>> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(samples_dir)
        .with_context(|| format!("reading {}", samples_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("java"))
        .collect();
    sources.sort();

    let mut samples = Vec::new();
    for java in sources {
        let name = java
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("bad sample name {}", java.display()))?
            .to_string();

        let class_dir = work.join(&name);
        exec::fresh_dir(&class_dir)?;

        let mut javac = Command::new("javac");
        javac
            .args(["-encoding", "UTF-8", "-d"])
            .arg(&class_dir)
            .arg(&java);
        exec::run_checked(javac, "javac")?;

        let mut classes = Vec::new();
        collect_class_files(&class_dir, &mut classes)?;
        classes.sort();
        if classes.is_empty() {
            bail!("javac produced no .class files for {name}");
        }

        let jar = work.join(format!("{name}.jar"));
        let mut jar_cmd = Command::new("jar");
        jar_cmd
            .arg("cf")
            .arg(&jar)
            .arg("-C")
            .arg(&class_dir)
            .arg(".");
        exec::run_checked(jar_cmd, "jar")?;

        samples.push(Sample { name, classes, jar });
    }
    Ok(samples)
}

fn collect_class_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_class_files(&path, out)?;
        } else if path.extension().and_then(|x| x.to_str()) == Some("class") {
            out.push(path);
        }
    }
    Ok(())
}

fn open_jar(sample: &Sample) -> Result<JarFileParser> {
    JarFileParser::open(&sample.jar).map_err(|e| anyhow!("opening jar for {}: {e}", sample.name))
}

// --- class-level checks ---------------------------------------------------

/// jvm-reader disassembles every compiled class into non-empty output.
fn check_class_disassembly(samples: &[Sample]) -> Result<()> {
    for sample in samples {
        for class in &sample.classes {
            if disassemble_class_file(class).is_empty() {
                bail!("empty disassembly for {}", class.display());
            }
        }
    }
    Ok(())
}

/// jvm-reader's `-c` disassembly matches the JDK's `javap -c` (whitespace
/// normalized) for every compiled class.
fn check_javap(samples: &[Sample]) -> Result<()> {
    for sample in samples {
        for class in &sample.classes {
            let mut cmd = Command::new("javap");
            cmd.arg("-c").arg(class);
            let javap = exec::capture_stdout(cmd, "javap")?;
            let expected = normalize(&javap);
            let actual = normalize(&disassemble_class_file(class));
            if let Some((line, want, got)) = first_difference(&expected, &actual) {
                bail!(
                    "javap mismatch for {} at line {}:\n  javap : {want}\n  reader: {got}",
                    class.display(),
                    line + 1,
                );
            }
        }
    }
    Ok(())
}

/// Collapse runs of whitespace and trim each line, matching the old test's
/// comparison rules.
fn normalize(input: &str) -> String {
    input
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// First line index (and both lines) at which two normalized texts differ.
fn first_difference(expected: &str, actual: &str) -> Option<(usize, String, String)> {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    for i in 0..expected.len().max(actual.len()) {
        let want = expected.get(i).copied().unwrap_or("<missing>");
        let got = actual.get(i).copied().unwrap_or("<missing>");
        if want != got {
            return Some((i, want.to_string(), got.to_string()));
        }
    }
    None
}

// --- jar-level checks -----------------------------------------------------

/// The classes jvm-reader parses from each jar match the entries `jar tf` lists.
fn check_jar_classes(samples: &[Sample]) -> Result<()> {
    for sample in samples {
        let mut cmd = Command::new("jar");
        cmd.arg("tf").arg(&sample.jar);
        let listing = exec::capture_stdout(cmd, "jar tf")?;
        let expected: BTreeSet<String> = listing
            .lines()
            .map(str::trim)
            .filter(|line| line.ends_with(".class"))
            .map(|line| line.trim_end_matches(".class").to_string())
            .collect();

        let parser = open_jar(sample)?;
        let parsed: BTreeSet<String> = parser
            .classes()
            .filter_map(|cf| cf.this_class_name().ok())
            .map(str::to_string)
            .collect();

        if expected != parsed {
            bail!(
                "jar {}: `jar tf` classes {expected:?} != parsed {parsed:?}",
                sample.name,
            );
        }
    }
    Ok(())
}

/// jvm-reader disassembles every jar into non-empty output.
fn check_jar_disassembly(samples: &[Sample]) -> Result<()> {
    for sample in samples {
        if disassemble_jar_file(&sample.jar).is_empty() {
            bail!("empty jar disassembly for {}", sample.name);
        }
    }
    Ok(())
}

/// The instruction-flow iterator yields at least one Dataflow instruction and at
/// least one Call/Other instruction across the samples.
fn check_instruction_flow(samples: &[Sample]) -> Result<()> {
    let (mut dataflow, mut call, mut other) = (false, false, false);
    for sample in samples {
        let parser = open_jar(sample)?;
        for info in parser.instruction_flow_iter().flatten() {
            match info.kind {
                InstructionKind::Dataflow => dataflow = true,
                InstructionKind::Call => call = true,
                InstructionKind::Other => other = true,
            }
        }
    }
    if !dataflow {
        bail!("no Dataflow instructions across samples");
    }
    if !(call || other) {
        bail!("no Call or Other instructions across samples");
    }
    Ok(())
}

// --- analysis checks ------------------------------------------------------

fn method_label(cf: &ClassFile, m: &MethodInfo) -> Option<String> {
    let class = cf.this_class_name().ok()?;
    let name = cf.get_utf8(m.name_index).ok()?;
    let descriptor = cf.get_utf8(m.descriptor_index).ok()?;
    Some(format!("L{class};->{name}{descriptor}"))
}

/// Line-map entries use DEX-style method ids and offsets that land inside the
/// method's code.
fn check_line_map(samples: &[Sample]) -> Result<()> {
    let mut saw_any = false;
    for sample in samples {
        let parser = open_jar(sample)?;
        for class_parser in parser.class_parsers() {
            let cf = class_parser.class_file();
            let entries = collect_line_map_entries(class_parser);
            if entries.is_empty() {
                continue;
            }
            saw_any = true;
            for e in &entries {
                if !(e.method.starts_with('L') && e.method.contains(";->")) {
                    bail!(
                        "line-map method id not DEX-style in {}: {}",
                        sample.name,
                        e.method
                    );
                }
                let Some(m) = cf
                    .methods
                    .iter()
                    .find(|m| method_label(cf, m).as_deref() == Some(e.method.as_str()))
                else {
                    bail!(
                        "no method for line-map entry {} in {}",
                        e.method,
                        sample.name
                    );
                };
                let Some(code) = &m.code else {
                    bail!(
                        "line-map entry for code-less method {} in {}",
                        e.method,
                        sample.name
                    );
                };
                let base = u64::from(code.code_byte_offset_in_classfile);
                if e.dex_offset < base {
                    bail!(
                        "dex_offset {} below code base {} in {}",
                        e.dex_offset,
                        base,
                        sample.name
                    );
                }
                if (e.dex_offset - base) as usize >= code.code.len() {
                    bail!("dex_offset implies pc outside code in {}", sample.name);
                }
                if cf.source_file.is_some() && e.source_file.is_empty() {
                    bail!(
                        "empty source_file despite SourceFile attribute in {}",
                        sample.name
                    );
                }
            }
        }
    }
    if !saw_any {
        bail!("no line-map entries across samples");
    }
    Ok(())
}

/// Each instruction's file offset/length matches the class-file layout and
/// consecutive instructions are contiguous.
fn check_file_offsets(samples: &[Sample]) -> Result<()> {
    let mut checked = 0usize;
    for sample in samples {
        let parser = open_jar(sample)?;
        for class_parser in parser.class_parsers() {
            for method in class_parser.methods() {
                let Some(code) = &method.code else { continue };
                let Some(cfg) = class_parser
                    .basic_blocks(method)
                    .map_err(|e| anyhow!("basic_blocks in {}: {e}", sample.name))?
                else {
                    continue;
                };
                let instrs = cfg.instructions();
                if instrs.is_empty() {
                    continue;
                }
                checked += 1;
                for (i, inst) in instrs.iter().enumerate() {
                    if inst.file_byte_offset != code.code_byte_offset_in_classfile + inst.pc {
                        bail!("instruction {i} file offset mismatch in {}", sample.name);
                    }
                    if inst.byte_length < 1 {
                        bail!("instruction {i} has zero byte length in {}", sample.name);
                    }
                }
                for w in instrs.windows(2) {
                    if w[0].file_byte_offset + w[0].byte_length != w[1].file_byte_offset {
                        bail!("non-contiguous instruction spans in {}", sample.name);
                    }
                }
            }
        }
    }
    if checked == 0 {
        bail!("no methods with instructions across samples");
    }
    Ok(())
}

/// Every instruction is covered by some basic block and the entry block has no
/// predecessors.
fn check_basic_blocks(samples: &[Sample]) -> Result<()> {
    let mut checked = 0usize;
    for sample in samples {
        let parser = open_jar(sample)?;
        for class_parser in parser.class_parsers() {
            for method in class_parser.methods() {
                if method.code.is_none() {
                    continue;
                }
                let Some(cfg) = class_parser
                    .basic_blocks(method)
                    .map_err(|e| anyhow!("basic_blocks in {}: {e}", sample.name))?
                else {
                    continue;
                };
                checked += 1;
                let blocks = cfg.blocks();
                let instrs = cfg.instructions();
                if blocks.is_empty() {
                    bail!("method with no basic blocks in {}", sample.name);
                }
                let mut covered = vec![false; instrs.len()];
                for block in blocks {
                    for inst in block.instructions(&cfg) {
                        if let Some(idx) = instrs.iter().position(|i| i.pc == inst.pc) {
                            covered[idx] = true;
                        }
                    }
                }
                if !covered.iter().all(|&v| v) {
                    bail!("not all instructions covered by blocks in {}", sample.name);
                }
                // The entry block may legitimately have predecessors: when a
                // method opens with a loop header (`while` at the top of the
                // body), javac emits a back edge to pc 0. What must not happen
                // is a *forward* edge into the entry, which would mean a second
                // way into the method.
                for &pred in &blocks[0].predecessors {
                    if blocks[pred].start_pc <= blocks[0].start_pc {
                        bail!(
                            "entry block has a non-back-edge predecessor (block {pred}, pc {}) in {}",
                            blocks[pred].start_pc,
                            sample.name
                        );
                    }
                }
            }
        }
    }
    if checked == 0 {
        bail!("no methods with basic blocks across samples");
    }
    Ok(())
}

/// After stack-slot normalization no relative stack locations remain, slot ids
/// stay small, and across the samples we observe both a StackSlot and a reused
/// destination slot id.
fn check_stack_slots(samples: &[Sample]) -> Result<()> {
    let mut saw_stack_slot = false;
    let mut saw_duplicate_dest = false;
    let mut skipped_join_shape = 0usize;

    for sample in samples {
        let parser = open_jar(sample)?;
        for class_parser in parser.class_parsers() {
            let class_name = class_parser
                .class_file()
                .this_class_name()
                .unwrap_or("<class-name-error>")
                .to_string();
            for method in class_parser.methods() {
                if method.code.is_none() {
                    continue;
                }
                let method_name = class_parser
                    .class_file()
                    .get_utf8(method.name_index)
                    .unwrap_or("<method-utf8-error>")
                    .to_string();
                let cfg = match class_parser.basic_blocks_with_stack_slots(method) {
                    Ok(cfg) => cfg,
                    Err(ClassFileError::StackHeightMismatch { .. }) => {
                        skipped_join_shape += 1;
                        continue;
                    }
                    Err(e) => bail!(
                        "basic_blocks_with_stack_slots failed for {class_name}.{method_name}: {e}"
                    ),
                };
                let Some(cfg) = cfg else { continue };

                let mut seen_dest_slots: HashSet<u32> = HashSet::new();
                for inst in cfg.instructions() {
                    for df in &inst.dataflow {
                        for loc in df.sources.iter().chain(std::iter::once(&df.destination)) {
                            assert_normalized(loc, &mut saw_stack_slot)?;
                        }
                        if let Location::StackSlot(id) = &df.destination {
                            if !seen_dest_slots.insert(*id) {
                                saw_duplicate_dest = true;
                            }
                        }
                    }
                    if let Some(call) = &inst.call {
                        for loc in call
                            .receiver
                            .iter()
                            .chain(call.arguments.iter())
                            .chain(call.return_value.iter())
                        {
                            assert_normalized(loc, &mut saw_stack_slot)?;
                        }
                    }
                }
            }
        }
    }

    if !saw_stack_slot {
        bail!("no StackSlot locations across samples");
    }
    if !saw_duplicate_dest {
        bail!("no method reused a StackSlot destination id");
    }
    if skipped_join_shape >= 8 {
        bail!("too many methods skipped for unsupported join shape: {skipped_join_shape}");
    }
    Ok(())
}

/// Recursively assert a location carries no un-normalized stack reference, and
/// note whether we have seen any concrete StackSlot.
fn assert_normalized(loc: &Location, saw_stack_slot: &mut bool) -> Result<()> {
    match loc {
        Location::StackSlot(id) => {
            *saw_stack_slot = true;
            if *id >= 64 {
                bail!("unexpectedly large stack-slot id {id}");
            }
        }
        Location::StackInput(_) | Location::StackOutput => {
            bail!("found un-normalized stack location {loc:?}");
        }
        Location::ArrayElement { base, offset } => {
            *saw_stack_slot = true;
            assert_normalized(base, saw_stack_slot)?;
            assert_normalized(offset, saw_stack_slot)?;
        }
        _ => {}
    }
    Ok(())
}

// --- fixture-shape checks -------------------------------------------------
//
// These two came out of `jvm-reader`'s `flow.rs` unit tests, which used to load
// compiled classes from a directory someone had to populate first. Everything
// they asserted about *decoding* those fixtures is now covered end to end by
// the `SwitchFlow` / `StringSwitchFlow` / `WideParamFlow` / `ShiftFlow` taint
// cases, in both frontends. What is left is what a taint case cannot say.

/// Read one sample's compiled `.class` by class name.
fn class_bytes(samples: &[Sample], name: &str) -> Result<Vec<u8>> {
    let sample = samples
        .iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no sample named {name}"))?;
    let file = format!("{name}.class");
    let path = sample
        .classes
        .iter()
        .find(|p| p.file_name().and_then(|f| f.to_str()) == Some(file.as_str()))
        .with_context(|| format!("{name} produced no {file}"))?;
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// The mnemonics of one method, in order.
fn mnemonics(bytes: &[u8], method_name: &str) -> Result<Vec<&'static str>> {
    let parser = ClassFileParser::parse(bytes).map_err(|e| anyhow!("parse: {e}"))?;
    let cf = parser.class_file();
    let method = cf
        .methods
        .iter()
        .find(|m| cf.get_utf8(m.name_index).ok() == Some(method_name))
        .with_context(|| format!("no method named {method_name}"))?;
    let cfg = compute_basic_blocks_for_method(cf, method).map_err(|e| anyhow!("cfg: {e}"))?;
    Ok(cfg.instructions().iter().map(|i| i.mnemonic).collect())
}

/// The switch fixtures still contain the switch instruction they are named for.
///
/// A vacuity guard, and the one thing the taint cases cannot check: if a later
/// `javac` lowered these shapes some other way -- a chain of `if_icmpeq`, say --
/// `SwitchFlow` and `StringSwitchFlow` would keep passing while no longer
/// exercising a switch at all.
fn check_switch_shapes(samples: &[Sample]) -> Result<()> {
    for (class, method, mnemonic) in [
        ("SparseSwitch", "parseTable", "lookupswitch"),
        ("DenseSwitch", "parseTable", "tableswitch"),
    ] {
        let bytes = class_bytes(samples, class)?;
        let found = mnemonics(&bytes, method)?;
        if !found.contains(&mnemonic) {
            bail!("{class}.{method} should contain {mnemonic}");
        }
    }

    // A Java 8 string switch lowers to both, in this order.
    let bytes = class_bytes(samples, "StringSwitch")?;
    let switches: Vec<&str> = mnemonics(&bytes, "decode")?
        .into_iter()
        .filter(|m| *m == "lookupswitch" || *m == "tableswitch")
        .collect();
    if switches != ["lookupswitch", "tableswitch"] {
        bail!(
            "StringSwitch.decode should lower to lookupswitch then tableswitch, got {switches:?}"
        );
    }
    Ok(())
}

/// Modified-UTF-8 string constants survive the constant-pool parse.
///
/// No taint case can stand in for this: an unpaired surrogate is inert data in
/// a constant, so a decoder that mangles it changes no flow. These two fixtures
/// carry nothing else. `dex:utf8-constants` is the same check on the other
/// frontend, over the same two sources compiled to DEX.
fn check_utf8_constants(samples: &[Sample]) -> Result<()> {
    // A supplementary character arrives as a CESU-8 pair and must recombine.
    let bytes = class_bytes(samples, "PairedOnly")?;
    let parser = ClassFileParser::parse(&bytes).map_err(|e| anyhow!("parse PairedOnly: {e}"))?;
    let cf = parser.class_file();
    let emoji = cf
        .constant_pool
        .iter()
        .flatten()
        .filter_map(|e| match e {
            CpEntry::Utf8(s) => s.as_str(),
            _ => None,
        })
        .find(|s| s.chars().any(|c| c as u32 > 0xFFFF))
        .context("PairedOnly should hold a supplementary character constant")?;
    if emoji != "\u{1F600}" {
        bail!("recombined pair is {emoji:?}, want U+1F600");
    }

    // Unpaired surrogates are legal, must not stop the parse, and cannot be
    // handed out as `&str`.
    let bytes = class_bytes(samples, "SurrogateConstants")?;
    let parser =
        ClassFileParser::parse(&bytes).map_err(|e| anyhow!("parse SurrogateConstants: {e}"))?;
    let cf = parser.class_file();
    if cf.this_class_name().map_err(|e| anyhow!("{e}"))? != "SurrogateConstants" {
        bail!("names and descriptors should be unaffected by a surrogate constant");
    }
    let mut unpaired: Vec<Vec<u16>> = Vec::new();
    for entry in cf.constant_pool.iter().flatten() {
        if let CpEntry::Utf8(s) = entry {
            if s.as_str().is_none() {
                if !s.to_string_lossy().contains('\u{FFFD}') {
                    bail!("an unpaired surrogate should read back lossily as U+FFFD");
                }
                unpaired.push(s.code_units().collect());
            }
        }
    }
    if unpaired.is_empty() {
        bail!("SurrogateConstants should hold unpaired surrogates");
    }
    for (unit, what) in [(0xD800u16, "high"), (0xDC00u16, "low")] {
        if !unpaired.iter().any(|u| u.contains(&unit)) {
            bail!("the lone {what} surrogate should survive as a code unit");
        }
    }
    Ok(())
}
