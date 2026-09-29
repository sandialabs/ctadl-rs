use std::env;
use std::fs::File;
use std::path::Path;

use jvm_reader::{
    collect_line_map_entries, disassemble_class_file, disassemble_jar_file, write_line_map_json,
    ClassFileParser, JarFileParser,
};

fn main() {
    let mut jar_mode = false;
    let mut linemap_json_path: Option<String> = None;
    let mut paths = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--jar" => jar_mode = true,
            "--linemap-json" => {
                let Some(p) = args.next() else {
                    eprintln!("--linemap-json requires a path");
                    std::process::exit(1);
                };
                linemap_json_path = Some(p);
            }
            _ => paths.push(arg),
        }
    }

    if paths.len() != 1 {
        println!("Usage: jvm-reader [--jar] [--linemap-json <path>] <path-to-class-or-jar-file>");
        return;
    }
    let path = Path::new(&paths[0]);

    if let Some(ref out_path) = linemap_json_path {
        if jar_mode {
            let jar = match JarFileParser::open(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };
            let mut all = Vec::new();
            for p in jar.class_parsers() {
                all.extend(collect_line_map_entries(p));
            }
            let mut out = match File::create(out_path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("create {}: {}", out_path, e);
                    std::process::exit(1);
                }
            };
            if let Err(e) = write_line_map_json(&mut out, &all) {
                eprintln!("write line map: {}", e);
                std::process::exit(1);
            }
        } else {
            let data = match std::fs::read(path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("read {}: {}", path.display(), e);
                    std::process::exit(1);
                }
            };
            let parser = match ClassFileParser::parse(&data) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };
            let entries = collect_line_map_entries(&parser);
            let mut out = match File::create(out_path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("create {}: {}", out_path, e);
                    std::process::exit(1);
                }
            };
            if let Err(e) = write_line_map_json(&mut out, &entries) {
                eprintln!("write line map: {}", e);
                std::process::exit(1);
            }
        }
    }

    let out = if jar_mode {
        disassemble_jar_file(path)
    } else {
        disassemble_class_file(path)
    };
    print!("{}", out);
}
