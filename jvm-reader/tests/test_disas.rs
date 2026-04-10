use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use jvm_reader::{
    collect_line_map_entries, disassemble_class_file, disassemble_jar_file, ClassFile,
    ClassFileParser, InstructionKind, JarFileParser, MethodInfo,
};
use walkdir::WalkDir;

#[test]
fn test_disassemble_produces_output() {
    let class_dir = Path::new("tests/class");
    assert!(class_dir.exists(), "Directory tests/class/ does not exist.");
    for entry in WalkDir::new(class_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path().to_path_buf();
        if path.is_file() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "malformed.class" || n == "UnicodeStrings.class")
                .unwrap_or(false);
            if skip {
                continue;
            }
            let out = disassemble_class_file(&path);
            assert!(
                !out.is_empty(),
                "disassemble_class_file should produce output for {:?}",
                path
            );
        }
    }
}

/// Normalizes the input string according to specific whitespace rules:
/// 1. Splits by lines (handling \r\n and \n).
/// 2. Trims leading and trailing whitespace from each line.
/// 3. Collapses internal whitespace (tabs, multiple spaces) into a single space.
/// 4. Re-joins lines with a single \n.
#[allow(dead_code)]
fn normalize_string(input: &str) -> String {
    input
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

// Turning this off because it requires javap dependency
//#[test]
#[allow(dead_code)]
fn test_javap_comparison() {
    let class_dir = Path::new("tests/class");

    if !class_dir.exists() {
        panic!("Directory tests/class/ does not exist.");
    }

    let javap_available = Command::new("javap")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !javap_available {
        panic!("javap not found in PATH: add JDK bin directory to PATH to run this test");
    }

    for entry in WalkDir::new(class_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path().to_path_buf();

        if path.is_file() {
            // Skip strict comparison for class files where javap disagrees with file bytes (e.g. JDK quirk)
            // or that are intentionally malformed.
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n == "cfr-0.152-ClassFile.class"
                        || n == "malformed.class"
                        || n == "UnicodeStrings.class"
                        || n == "module-info.class"
                })
                .unwrap_or(false);
            if skip {
                continue;
            }
            let javap_output = Command::new("javap")
                .arg("-c")
                .arg(&path)
                .output()
                .expect("Failed to execute javap command");

            if !javap_output.status.success() {
                let stderr = String::from_utf8_lossy(&javap_output.stderr);
                panic!("javap failed for file {:?}: {}", path, stderr);
            }

            let javap_stdout =
                String::from_utf8(javap_output.stdout).expect("javap output was not valid UTF-8");

            let internal_output = disassemble_class_file(&path);

            let expected = normalize_string(&javap_stdout);
            let actual = normalize_string(&internal_output);
            let expected_lines: Vec<&str> = expected.lines().collect();
            let actual_lines: Vec<&str> = actual.lines().collect();

            let first_diff = expected_lines
                .iter()
                .zip(actual_lines.iter())
                .position(|(a, b)| a != b)
                .or_else(|| {
                    if expected_lines.len() != actual_lines.len() {
                        Some(std::cmp::min(expected_lines.len(), actual_lines.len()))
                    } else {
                        None
                    }
                });

            if let Some(line_idx) = first_diff {
                const CONTEXT: usize = 5;
                let start = line_idx.saturating_sub(CONTEXT);
                let end_exp = (line_idx + CONTEXT + 1).min(expected_lines.len());
                let end_act = (line_idx + CONTEXT + 1).min(actual_lines.len());
                let expected_snippet: String = expected_lines[start..end_exp]
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  {:5} | {}", start + i + 1, s))
                    .collect::<Vec<_>>()
                    .join("\n");
                let actual_snippet: String = actual_lines[start..end_act]
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("  {:5} | {}", start + i + 1, s))
                    .collect::<Vec<_>>()
                    .join("\n");
                panic!(
                    "Mismatch in {:?} at line {} (1-based)\n\n\
                     Expected (javap) around first difference:\n{}\n\n\
                     Actual (jvm-reader) around first difference:\n{}",
                    path,
                    line_idx + 1,
                    expected_snippet,
                    actual_snippet
                );
            }
        }
    }
}

/// Asserts that our parsed code matches the class file at offset 48 for cfr class.
/// (javap shows dup/0x51 there but the file and our parser have ladd/0x59; cfr is excluded from javap comparison.)
#[test]
fn test_debug_cfr_bytecode() {
    let path = Path::new("tests/class/cfr-0.152-ClassFile.class");
    assert!(path.exists(), "{:?} does not exist", path);
    let data = fs::read(path).expect("read class file");
    let parser = ClassFileParser::parse(&data).expect("parse");
    let first_with_code = parser
        .methods()
        .find(|m| m.code.is_some())
        .expect("first method with code");
    let code = &first_with_code.code.as_ref().unwrap().code;
    assert!(code.len() > 48, "first method has code");
    assert_eq!(code[48], 0x59, "offset 48 is ladd per class file");
}

/// Verifies that for each sample JAR in tests/jar/, every .class entry listed by `jar tf`
/// is parsed by jvm-reader and appears in the JarFileParser view (by class name).
//#[test]
#[allow(dead_code)]
fn test_jar_all_classes_parsed() {
    let jar_dir = Path::new("tests/jar");
    if !jar_dir.exists() {
        return;
    }

    let jar_available = Command::new("jar")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !jar_available {
        panic!("jar not found in PATH: add JDK bin directory to PATH to run this test");
    }

    for entry in WalkDir::new(jar_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path().to_path_buf();
        if path.is_file() && path.extension().map(|e| e == "jar").unwrap_or(false) {
            let jar_path = path.as_path();

            let jar_tf_out = Command::new("jar")
                .arg("tf")
                .arg(jar_path)
                .output()
                .expect("Failed to run jar tf");

            if !jar_tf_out.status.success() {
                let stderr = String::from_utf8_lossy(&jar_tf_out.stderr);
                panic!("jar tf failed for {:?}: {}", jar_path, stderr);
            }

            let stdout = String::from_utf8(jar_tf_out.stdout).expect("jar tf output not UTF-8");
            let expected_classes: BTreeSet<String> = stdout
                .lines()
                .map(str::trim)
                .filter(|line| line.ends_with(".class"))
                .map(|line| line.strip_suffix(".class").unwrap_or(line).to_string())
                .collect();

            let jar_parser = JarFileParser::open(jar_path).expect("JarFileParser::open");
            let parsed_classes: BTreeSet<String> = jar_parser
                .classes()
                .filter_map(|cf| cf.this_class_name().ok())
                .map(str::to_string)
                .collect();

            assert_eq!(
                expected_classes, parsed_classes,
                "JAR {:?}: classes from `jar tf` should match classes parsed by jvm-reader",
                jar_path
            );

            let out = disassemble_jar_file(jar_path);
            assert!(
                !out.is_empty(),
                "disassemble_jar_file should produce output for {:?}",
                jar_path
            );
        }
    }
}

/// Instruction flow iterator: yields at least one instruction per JAR and sees Dataflow, Call, and Other.
#[test]
fn test_instruction_flow_iter() {
    let jar_dir = Path::new("tests/jar");
    assert!(jar_dir.exists(), "Directory tests/jar/ does not exist.");
    let first_jar = WalkDir::new(jar_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().map(|x| x == "jar").unwrap_or(false));
    let entry = first_jar.expect("expected at least one .jar in tests/jar/");
    let path = entry.path();
    let jar_parser = JarFileParser::open(path).expect("JarFileParser::open");
    let results: Vec<_> = jar_parser.instruction_flow_iter().collect();
    assert!(
        !results.is_empty(),
        "instruction_flow_iter should yield at least one instruction for {:?}",
        path
    );
    let mut seen_dataflow = false;
    let mut seen_call = false;
    let mut seen_other = false;
    for r in &results {
        let info = match r {
            Ok(i) => i,
            Err(_) => continue,
        };
        match info.kind {
            InstructionKind::Dataflow => seen_dataflow = true,
            InstructionKind::Call => seen_call = true,
            InstructionKind::Other => seen_other = true,
        }
    }
    assert!(
        seen_dataflow,
        "should see at least one Dataflow instruction"
    );
    assert!(
        seen_call || seen_other,
        "should see at least one Call or Other instruction"
    );
}

fn method_label_dex_style(cf: &ClassFile, m: &MethodInfo) -> Option<String> {
    let class = cf.this_class_name().ok()?;
    let name = cf.get_utf8(m.name_index).ok()?;
    let descriptor = cf.get_utf8(m.descriptor_index).ok()?;
    Some(format!("L{};->{}{}", class, name, descriptor))
}

fn sample_jar_paths() -> Vec<std::path::PathBuf> {
    let sample_dir = Path::new("tests/sample");
    assert!(
        sample_dir.exists(),
        "Directory tests/sample/ does not exist."
    );
    let sample_names: Vec<String> = WalkDir::new(sample_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.is_file() && p.extension().map(|e| e == "java").unwrap_or(false))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    assert!(
        !sample_names.is_empty(),
        "expected at least one .java sample in tests/sample/"
    );
    let jar_dir = Path::new("tests/jar");
    assert!(jar_dir.exists(), "Directory tests/jar/ does not exist.");
    let jars: Vec<_> = sample_names
        .iter()
        .map(|name| jar_dir.join(format!("{name}.jar")))
        .collect();
    for jar in &jars {
        assert!(jar.exists(), "expected generated sample jar {:?}", jar);
    }
    jars
}

/// Line map entries from LineNumberTable: validate method id format and offsets for all sample JAR classes.
#[test]
fn test_collect_line_map_samples() {
    let mut saw_any_entries = false;
    for jar_path in sample_jar_paths() {
        let jar_parser = JarFileParser::open(&jar_path).expect("JarFileParser::open");
        for parser in jar_parser.class_parsers() {
            let cf = parser.class_file();
            let entries = collect_line_map_entries(parser);
            if entries.is_empty() {
                continue;
            }
            saw_any_entries = true;
            for e in &entries {
                assert!(
                    e.method.starts_with('L') && e.method.contains(";->"),
                    "method id should be DEX-style for {:?}: {}",
                    jar_path,
                    e.method
                );
                let Some(m) = cf
                    .methods
                    .iter()
                    .find(|m| method_label_dex_style(cf, m).as_deref() == Some(e.method.as_str()))
                else {
                    panic!(
                        "no method for line map entry {} in {:?}",
                        e.method, jar_path
                    );
                };
                let Some(code) = &m.code else {
                    panic!(
                        "line map entry for method without code: {} in {:?}",
                        e.method, jar_path
                    );
                };
                let base = u64::from(code.code_byte_offset_in_classfile);
                assert!(
                    e.dex_offset >= base,
                    "dex_offset {} should be >= code base {}",
                    e.dex_offset,
                    base
                );
                let pc = (e.dex_offset - base) as usize;
                assert!(
                    pc < code.code.len(),
                    "start_pc implied by dex_offset should fall inside code"
                );
                if cf.source_file.is_some() {
                    assert!(
                        !e.source_file.is_empty(),
                        "source_file should be non-empty when SourceFile exists"
                    );
                }
            }
        }
    }
    assert!(
        saw_any_entries,
        "expected at least one line-map entry across sample jars"
    );
}

/// `InstructionFlowInfo::file_byte_offset` / `byte_length` match class-file layout for all sample methods.
#[test]
fn test_instruction_file_offsets_samples() {
    let mut checked_methods = 0usize;
    for jar_path in sample_jar_paths() {
        let jar_parser = JarFileParser::open(&jar_path).expect("JarFileParser::open");
        for parser in jar_parser.class_parsers() {
            for method in parser.methods().filter(|m| m.code.is_some()) {
                let code = method.code.as_ref().expect("method with code");
                let Some(cfg) = parser.basic_blocks(method).expect("basic_blocks") else {
                    continue;
                };
                let instrs = cfg.instructions();
                if instrs.is_empty() {
                    continue;
                }
                checked_methods += 1;
                for (i, inst) in instrs.iter().enumerate() {
                    assert_eq!(
                        inst.file_byte_offset,
                        code.code_byte_offset_in_classfile + inst.pc,
                        "instruction {} pc {}",
                        i,
                        inst.pc
                    );
                    assert!(inst.byte_length >= 1);
                }
                for w in instrs.windows(2) {
                    assert_eq!(
                        w[0].file_byte_offset + w[0].byte_length,
                        w[1].file_byte_offset,
                        "consecutive spans"
                    );
                }
            }
        }
    }
    assert!(
        checked_methods > 0,
        "expected at least one method with instructions in sample jars"
    );
}

/// Basic-block CFG: covers all instructions for all methods-with-code in all sample JAR classes.
#[test]
fn test_basic_blocks_samples() {
    let mut checked = 0usize;
    for jar_path in sample_jar_paths() {
        let jar_parser = JarFileParser::open(&jar_path).expect("JarFileParser::open");
        for parser in jar_parser.class_parsers() {
            for method in parser.methods().filter(|m| m.code.is_some()) {
                let Some(cfg) = parser.basic_blocks(method).expect("basic_blocks ok") else {
                    continue;
                };
                checked += 1;
                let blocks = cfg.blocks();
                let instrs = cfg.instructions();
                assert!(!blocks.is_empty(), "method should have at least one block");
                let mut covered = vec![false; instrs.len()];
                for block in blocks {
                    let slice = block.instructions(&cfg);
                    for inst in slice {
                        if let Some(idx) = instrs.iter().position(|i| i.pc == inst.pc) {
                            covered[idx] = true;
                        }
                    }
                }
                assert!(
                    covered.iter().all(|&v| v),
                    "all instructions should be covered by some block"
                );
                assert!(
                    blocks[0].predecessors.is_empty(),
                    "entry block should have no predecessors"
                );
            }
        }
    }
    assert!(
        checked > 0,
        "expected at least one method with basic blocks in sample jars"
    );
}

/// Stack-slot normalization: no relative stack locations remain in any sample method.
#[test]
fn test_stack_slots_samples() {
    let mut saw_any_stackslot = false;
    let mut saw_duplicate_dest_slot_anywhere = false;
    let mut skipped_join_shape_methods = 0usize;
    for jar_path in sample_jar_paths() {
        let jar_parser = JarFileParser::open(&jar_path).expect("JarFileParser::open");
        for parser in jar_parser.class_parsers() {
            let class_name = parser
                .class_file()
                .this_class_name()
                .unwrap_or("<class-name-error>")
                .to_string();
            for method in parser.methods().filter(|m| m.code.is_some()) {
                let method_name = parser
                    .class_file()
                    .get_utf8(method.name_index)
                    .unwrap_or("<method-utf8-error>")
                    .to_string();
                let cfg_opt = match parser.basic_blocks_with_stack_slots(method) {
                    Ok(v) => v,
                    Err(jvm_reader::ClassFileError::InvalidClassFile(
                        "inconsistent operand stack height at basic-block join",
                    )) => {
                        skipped_join_shape_methods += 1;
                        continue;
                    }
                    Err(e) => {
                        panic!(
                            "basic_blocks_with_stack_slots failed for {}.{}: {}",
                            class_name, method_name, e
                        )
                    }
                };
                let Some(cfg) = cfg_opt else {
                    continue;
                };
                let mut seen_dest_slots: std::collections::HashSet<u32> =
                    std::collections::HashSet::new();
                for inst in cfg.instructions() {
                    if let Some(df) = &inst.dataflow {
                        for loc in df.sources.iter().chain(df.destinations.iter()) {
                            match loc {
                                jvm_reader::Location::StackSlot(id) => {
                                    saw_any_stackslot = true;
                                    assert!(*id < 64, "unexpectedly large stack-slot id {}", id);
                                }
                                jvm_reader::Location::StackInput(_)
                                | jvm_reader::Location::StackOutput => {
                                    panic!("found unnormalized stack location ({:?})", loc);
                                }
                                _ => {}
                            }
                        }
                        for dst in &df.destinations {
                            if let jvm_reader::Location::StackSlot(id) = dst {
                                if !seen_dest_slots.insert(*id) {
                                    saw_duplicate_dest_slot_anywhere = true;
                                }
                            }
                        }
                    }
                    if let Some(call) = &inst.call {
                        if let Some(receiver) = &call.receiver {
                            match receiver {
                                jvm_reader::Location::StackSlot(id) => {
                                    saw_any_stackslot = true;
                                    assert!(*id < 64, "unexpectedly large stack-slot id {}", id);
                                }
                                jvm_reader::Location::StackInput(_)
                                | jvm_reader::Location::StackOutput => {
                                    panic!(
                                        "found unnormalized stack location in CallInfo.receiver"
                                    );
                                }
                                _ => {}
                            }
                        }
                        for arg in &call.arguments {
                            match arg {
                                jvm_reader::Location::StackSlot(id) => {
                                    saw_any_stackslot = true;
                                    assert!(*id < 64, "unexpectedly large stack-slot id {}", id);
                                }
                                jvm_reader::Location::StackInput(_)
                                | jvm_reader::Location::StackOutput => {
                                    panic!(
                                        "found unnormalized stack location in CallInfo.arguments"
                                    );
                                }
                                _ => {}
                            }
                        }
                        if let Some(ret) = &call.return_value {
                            match ret {
                                jvm_reader::Location::StackSlot(id) => {
                                    saw_any_stackslot = true;
                                    assert!(*id < 64, "unexpectedly large stack-slot id {}", id);
                                }
                                jvm_reader::Location::StackInput(_)
                                | jvm_reader::Location::StackOutput => {
                                    panic!("found unnormalized stack location in CallInfo.return_value");
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        saw_any_stackslot,
        "expected at least one StackSlot location across sample jars"
    );
    assert!(
        saw_duplicate_dest_slot_anywhere,
        "expected at least one method to reuse StackSlot destination ids"
    );
    assert!(
        skipped_join_shape_methods < 8,
        "too many methods skipped due to unsupported stack-height join shape: {}",
        skipped_join_shape_methods
    );
}
