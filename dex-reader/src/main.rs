#![allow(clippy::all)]
use std::io::Read;

use dex_reader::{
    APKParser, DexParser,
    debug_info::{collect_line_map_entries, write_line_map_json},
    error::DexError,
    instructions::RawArg,
    parser::{DecodedCodeItem, decode_code_item, disassemble_code_item_with_constants},
    smali,
};

fn help() {
    println!(
        "Options:\n  --apk\t\t\tinput file is an APK (ZIP with DEX files)\n  --print-strings\n  --print-protos\n  --print-methods\n  --print-method-info\n  --print-raw-insts\n  --print-disassembly\n  --print-smali\n  --linemap-json <path>\twrite instruction→source line map as JSON to <path>"
    );
    std::process::exit(1);
}

fn main() {
    let mut is_apk = false;
    let mut print_strings = false;
    let mut print_protos = false;
    let mut print_methods = false;
    let mut print_method_info = false;
    let mut print_raw_insts = false;
    let mut print_disassembly = false;
    let mut print_smali = false;
    let mut linemap_json_path: Option<String> = None;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apk" => is_apk = true,
            "--print-strings" => print_strings = true,
            "--print-protos" => print_protos = true,
            "--print-methods" => print_methods = true,
            "--print-method-info" => print_method_info = true,
            "--print-raw-insts" => print_raw_insts = true,
            "--print-disassembly" => print_disassembly = true,
            "--print-smali" => print_smali = true,
            "--linemap-json" => {
                linemap_json_path = Some(args.next().unwrap_or_else(|| {
                    eprintln!("--linemap-json requires a path");
                    help();
                    unreachable!()
                }));
            }
            "--help" => help(),
            "--" => break,
            s if s.starts_with("--") => {
                println!("Invalid argument: {s}");
                println!();
                help()
            }
            _ => {
                paths.push(arg);
            }
        }
    }
    paths.extend(args);

    if paths.len() != 1 {
        panic!("Incorrect number of args");
    }

    let path = paths.into_iter().next().unwrap();

    let mut fin = std::fs::File::options().read(true).open(&path).unwrap();
    let mut buffer = Vec::new();
    fin.read_to_end(&mut buffer).unwrap();

    if is_apk {
        let apk = APKParser::new(&buffer).unwrap();
        let parsers = apk.dex_parsers_with_filenames();
        for (dex_index, (filename, parser)) in parsers.iter().enumerate() {
            let data = parser.data;
            if parsers.len() > 1 {
                println!("--- DEX {} ({}) ---", dex_index + 1, filename);
            }
            run_print_options(
                parser,
                data,
                print_strings,
                print_protos,
                print_methods,
                print_method_info,
                print_raw_insts,
                print_disassembly,
                print_smali,
            );
        }
        if let Some(ref out_path) = linemap_json_path {
            let mut all_entries = Vec::new();
            for (_, parser) in &parsers {
                all_entries.extend(collect_line_map_entries(parser));
            }
            let mut out = std::fs::File::create(out_path).unwrap();
            write_line_map_json(&mut out, &all_entries).unwrap();
        }
    } else {
        let parser = DexParser::new(&buffer).unwrap();
        run_print_options(
            &parser,
            &buffer,
            print_strings,
            print_protos,
            print_methods,
            print_method_info,
            print_raw_insts,
            print_disassembly,
            print_smali,
        );
        if let Some(ref out_path) = linemap_json_path {
            let entries = collect_line_map_entries(&parser);
            let mut out = std::fs::File::create(out_path).unwrap();
            write_line_map_json(&mut out, &entries).unwrap();
        }
    }
}

fn run_print_options(
    parser: &DexParser,
    buffer: &[u8],
    print_strings: bool,
    print_protos: bool,
    print_methods: bool,
    print_method_info: bool,
    print_raw_insts: bool,
    print_disassembly: bool,
    print_smali: bool,
) {
    if print_strings {
        for i in 0..parser.strings.len() {
            let s = parser.strings.get(i).unwrap();
            println!("String {}: {:?}", i, s);
        }
    }

    if print_protos {
        for proto in parser.proto_ids.iter() {
            let sig = proto
                .pretty_signature(buffer, &parser.strings, &parser.type_ids)
                .unwrap();
            let shorty = proto.shorty(&parser.strings).unwrap();
            println!("{} -> {}", shorty, sig);
        }
    }

    if print_methods {
        for class_def in parser.classes() {
            let class_name = parser.class_name(class_def).unwrap();
            let methods = parser.class_methods(class_def).unwrap();
            println!("Class: {}", class_name);
            for method in methods {
                let sig = parser.method_signature(method).unwrap();
                println!("  {}", sig);
            }
        }
    }

    if print_method_info {
        for class_def in parser.classes() {
            let class_name = parser.class_name(class_def).unwrap();
            println!("Class: {}", class_name);

            let class_data = parser.class_data(class_def).unwrap();
            for method in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let method_info = parser
                    .method_ids
                    .get(method.method_idx as usize)
                    .ok_or(DexError::InvalidDex("method_idx out of bounds"))
                    .unwrap();
                let sig = parser.method_signature(method_info).unwrap();
                println!(
                    "  {} (access: 0x{:X}) code_off=0x{:X}",
                    sig, method.access_flags, method.code_off
                );
            }
        }
    }

    if print_raw_insts {
        for class_def in parser.classes() {
            let class_data = parser.class_data(class_def).unwrap();

            for method in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let method_info = parser.method_ids.get(method.method_idx as usize).unwrap();
                let sig = parser.method_signature(method_info).unwrap();
                println!("{} (access: 0x{:X})", sig, method.access_flags);

                if let Some(code_item) = method.code(buffer).unwrap() {
                    let instructions = decode_code_item(&code_item);
                    println!(
                        "  {} registers, {} instructions",
                        code_item.registers_size,
                        instructions.len()
                    );

                    for item in instructions.iter() {
                        let offset = match item {
                            DecodedCodeItem::Instruction { offset, .. } => *offset,
                            DecodedCodeItem::Payload { offset, .. } => *offset,
                        };

                        let display_str = match item {
                            DecodedCodeItem::Instruction { inst, .. } => {
                                format!("{}", RawArg(inst))
                            }
                            DecodedCodeItem::Payload { payload, .. } => {
                                format!("{}", RawArg(payload))
                            }
                        };

                        println!("    {:04X}: {}", offset * 2, display_str);
                    }
                }
            }
        }
    }

    let cp = parser.constant_pool();

    for (i, m) in cp.method_ids.iter().enumerate() {
        if m.proto_idx as usize >= cp.proto_ids.len() {
            println!("Method {} has invalid proto_idx {}", i, m.proto_idx);
        }
        if m.class_idx as usize >= cp.type_ids.len() {
            println!("Method {} has invalid class_idx {}", i, m.class_idx);
        }
        if m.name_idx as usize >= cp.strings.len() {
            println!("Method {} has invalid name_idx {}", i, m.name_idx);
        }
    }

    if print_disassembly {
        for class_def in parser.classes() {
            let class_data = parser.class_data(class_def).unwrap();

            for method in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let method_info = cp.method_ids.get(method.method_idx as usize).unwrap();
                let sig = method_info
                    .signature(&cp.strings, &cp.type_ids, &cp.proto_ids, buffer)
                    .unwrap();
                println!("Method: {} (access: 0x{:X})", sig, method.access_flags);

                if let Some(code_item) = method.code(buffer).unwrap() {
                    let disasm = disassemble_code_item_with_constants(&code_item, &cp);
                    for line in disasm.iter() {
                        println!("  {}", line);
                    }
                }
            }
        }
    }

    if print_smali {
        for class_def in parser.classes() {
            let class_data = parser.class_data(class_def).unwrap();

            for method in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let method_info = cp.method_ids.get(method.method_idx as usize).unwrap();
                let sig = method_info
                    .signature(&cp.strings, &cp.type_ids, &cp.proto_ids, buffer)
                    .unwrap();
                println!("Method: {} (access: 0x{:X})", sig, method.access_flags);

                if let Some(code_item) = method.code(buffer).unwrap() {
                    let lines = smali::format_code_item_smali(&code_item, &cp);
                    for line in lines.iter() {
                        println!("  {}", line);
                    }
                }
            }
        }
    }
}
