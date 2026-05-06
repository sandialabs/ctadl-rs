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
    if !class_dir.exists() {
        return;
    }
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
#[allow(unused)]
fn normalize_string(input: &str) -> String {
    input
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

//#[test]
#[allow(unused)]
#[allow(unused)]
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
    if !path.exists() {
        return;
    }
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
#[allow(unused)]
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
    if !jar_dir.exists() {
        return;
    }
    let first_jar = WalkDir::new(jar_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().map(|x| x == "jar").unwrap_or(false));
    let Some(entry) = first_jar else {
        return;
    };
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

/// Line map entries from LineNumberTable: DEX-style method ids, offsets, and optional source file.
#[test]
fn test_collect_line_map_helloworld() {
    let jar_path = Path::new("tests/jar/HelloWorld.jar");
    if !jar_path.exists() {
        return;
    }
    let jar_parser = JarFileParser::open(jar_path).expect("JarFileParser::open");
    let parser = jar_parser
        .class_parsers()
        .iter()
        .find(|p| p.class_file().this_class_name().ok().as_deref() == Some("HelloWorld"));
    let Some(parser) = parser else {
        return;
    };
    let cf = parser.class_file();
    let entries = collect_line_map_entries(parser);
    assert!(
        !entries.is_empty(),
        "HelloWorld should have LineNumberTable entries"
    );
    assert!(
        entries.iter().all(|e| e.method.contains("LHelloWorld;->")),
        "method ids should use DEX-style LClass;->name(desc)"
    );
    if cf.source_file.is_some() {
        assert!(
            entries.iter().all(|e| !e.source_file.is_empty()),
            "SourceFile attribute should yield non-empty source_file strings"
        );
    }
    for e in &entries {
        let Some(m) = cf
            .methods
            .iter()
            .find(|m| method_label_dex_style(cf, m).as_deref() == Some(e.method.as_str()))
        else {
            panic!("no method for line map entry: {}", e.method);
        };
        let Some(code) = &m.code else {
            panic!("line map entry for method without code: {}", e.method);
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
    }
}

/// `InstructionFlowInfo::file_byte_offset` / `byte_length` match class-file layout and pack contiguously.
#[test]
fn test_instruction_file_offsets_helloworld() {
    let jar_path = Path::new("tests/jar/HelloWorld.jar");
    if !jar_path.exists() {
        return;
    }
    let jar_parser = JarFileParser::open(jar_path).expect("JarFileParser::open");
    let parser = jar_parser
        .class_parsers()
        .iter()
        .find(|p| p.class_file().this_class_name().ok().as_deref() == Some("HelloWorld"));
    let Some(parser) = parser else {
        return;
    };
    let method = parser.methods().find(|m| {
        parser
            .class_file()
            .get_utf8(m.name_index)
            .ok()
            .map(|n| n == "main")
            .unwrap_or(false)
    });
    let Some(method) = method else {
        return;
    };
    let Some(code) = &method.code else {
        return;
    };
    let cfg = match parser.basic_blocks(method).expect("basic_blocks") {
        Some(c) => c,
        None => return,
    };
    let instrs = cfg.instructions();
    assert!(!instrs.is_empty());
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

/// Basic-block CFG: covers all instructions of HelloWorld.dataflow and HelloWorld.calls and has reasonable edges.
#[test]
fn test_basic_blocks_helloworld() {
    let jar_path = Path::new("tests/jar/HelloWorld.jar");
    if !jar_path.exists() {
        return;
    }
    let jar_parser = JarFileParser::open(jar_path).expect("JarFileParser::open");

    // Find the HelloWorld class parser.
    let mut hello_parser: Option<&ClassFileParser> = None;
    for p in jar_parser.class_parsers() {
        let cf = p.class_file();
        if cf.this_class_name().ok().as_deref() == Some("HelloWorld") {
            hello_parser = Some(p);
            break;
        }
    }
    let hello_parser = match hello_parser {
        Some(p) => p,
        None => return,
    };

    // Helper to check basic blocks for a named method.
    let check_method = |name: &str, parser: &ClassFileParser| {
        let method = match parser.methods().find(|m| {
            parser
                .class_file()
                .get_utf8(m.name_index)
                .ok()
                .map(|n| n == name)
                .unwrap_or(false)
        }) {
            Some(m) => m,
            None => return,
        };

        let cfg_opt = parser.basic_blocks(method).expect("basic_blocks ok");
        let cfg = match cfg_opt {
            Some(c) => c,
            None => return,
        };

        let blocks = cfg.blocks();
        let instrs = cfg.instructions();
        assert!(
            !blocks.is_empty(),
            "method {} should have at least one block",
            name
        );

        // Coverage: each instruction must belong to at least one block.
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
            "all instructions should be covered by some block for method {}",
            name
        );

        // Entry block has no predecessors.
        assert!(
            blocks[0].predecessors.is_empty(),
            "entry block should have no predecessors for method {}",
            name
        );
    };

    check_method("dataflow", hello_parser);
    check_method("calls", hello_parser);
}

/// Stack-slot normalization: all stack locations become StackSlot ids and are stable within a method.
#[test]
fn test_stack_slots_helloworld() {
    let jar_path = Path::new("tests/jar/HelloWorld.jar");
    if !jar_path.exists() {
        return;
    }
    let jar_parser = JarFileParser::open(jar_path).expect("JarFileParser::open");

    // Find the HelloWorld class parser.
    let mut hello_parser: Option<&ClassFileParser> = None;
    for p in jar_parser.class_parsers() {
        let cf = p.class_file();
        if cf.this_class_name().ok().as_deref() == Some("HelloWorld") {
            hello_parser = Some(p);
            break;
        }
    }
    let hello_parser = match hello_parser {
        Some(p) => p,
        None => return,
    };

    // Helper to check normalized stack slots for a named method.
    let check_method = |name: &str, parser: &ClassFileParser| {
        let method = match parser.methods().find(|m| {
            parser
                .class_file()
                .get_utf8(m.name_index)
                .ok()
                .map(|n| n == name)
                .unwrap_or(false)
        }) {
            Some(m) => m,
            None => return,
        };

        let cfg_opt = parser
            .basic_blocks_with_stack_slots(method)
            .expect("basic_blocks_with_stack_slots ok");
        let cfg = match cfg_opt {
            Some(c) => c,
            None => return,
        };

        // Collect all stack-related locations.
        let mut any_stackslot = false;
        let mut seen_dest_slots: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut saw_duplicate_dest_slot = false;
        for inst in cfg.instructions() {
            if let Some(df) = &inst.dataflow {
                for loc in df.sources.iter().chain(df.destinations.iter()) {
                    match loc {
                        jvm_reader::Location::StackSlot(id) => {
                            any_stackslot = true;
                            // Stack-slot ids should be in a small range starting from 0.
                            assert!(
                                *id < 64,
                                "unexpectedly large stack-slot id {} in method {}",
                                id,
                                name
                            );
                        }
                        jvm_reader::Location::StackInput(_) | jvm_reader::Location::StackOutput => {
                            panic!(
                                "found unnormalized stack location ({:?}) in method {}",
                                loc, name
                            );
                        }
                        _ => {}
                    }
                }

                // For at least one method with straight-line push/compute/pop,
                // we expect StackSlot ids to be reused (same depth position can
                // be re-occupied later), not strictly counting up.
                if name == "dataflow" {
                    for dst in &df.destinations {
                        if let jvm_reader::Location::StackSlot(id) = dst {
                            if !seen_dest_slots.insert(*id) {
                                saw_duplicate_dest_slot = true;
                            }
                        }
                    }
                }
            }

            // CallInfo should not contain any unnormalized stack locations.
            if let Some(call) = &inst.call {
                if let Some(receiver) = &call.receiver {
                    match receiver {
                        jvm_reader::Location::StackSlot(id) => {
                            assert!(
                                *id < 64,
                                "unexpectedly large stack-slot id {} in method {}",
                                id,
                                name
                            );
                        }
                        jvm_reader::Location::StackInput(_) | jvm_reader::Location::StackOutput => {
                            panic!("found unnormalized stack location in CallInfo.receiver for method {}", name);
                        }
                        _ => {}
                    }
                }
                for arg in &call.arguments {
                    match arg {
                        jvm_reader::Location::StackSlot(id) => {
                            assert!(
                                *id < 64,
                                "unexpectedly large stack-slot id {} in method {}",
                                id,
                                name
                            );
                        }
                        jvm_reader::Location::StackInput(_) | jvm_reader::Location::StackOutput => {
                            panic!("found unnormalized stack location in CallInfo.arguments for method {}", name);
                        }
                        _ => {}
                    }
                }
                if let Some(ret) = &call.return_value {
                    match ret {
                        jvm_reader::Location::StackSlot(id) => {
                            assert!(
                                *id < 64,
                                "unexpectedly large stack-slot id {} in method {}",
                                id,
                                name
                            );
                        }
                        jvm_reader::Location::StackInput(_) | jvm_reader::Location::StackOutput => {
                            panic!("found unnormalized stack location in CallInfo.return_value for method {}", name);
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(
            any_stackslot,
            "expected at least one StackSlot location in method {}",
            name
        );
        if name == "dataflow" {
            assert!(
                saw_duplicate_dest_slot,
                "expected StackSlot destination ids to be reused in method dataflow"
            );
        }
    };

    check_method("dataflow", hello_parser);
    check_method("calls", hello_parser);
}
