//! CLI: deterministic text dump of a CIL image.
//!
//! Usage: cil-dump [--sections a,b,c] [--no-il] <file>

use cil_dump::{ALL_SECTIONS, Section, dump};
use cil_reader::Module;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut sections: Vec<Section> = ALL_SECTIONS.iter().map(|&(_, s)| s).collect();
    let mut path = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sections" => {
                let Some(list) = args.next() else {
                    eprintln!("--sections needs a value");
                    return ExitCode::FAILURE;
                };
                sections = Vec::new();
                for name in list.split(',') {
                    match ALL_SECTIONS.iter().find(|(n, _)| *n == name) {
                        Some(&(_, s)) => sections.push(s),
                        None => {
                            eprintln!("unknown section: {name}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            "--no-il" => sections.retain(|&s| s != Section::Il),
            other if path.is_none() => path = Some(other.to_string()),
            other => {
                eprintln!("unexpected argument: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(path) = path else {
        eprintln!("usage: cil-dump [--sections a,b,c] [--no-il] <file>");
        return ExitCode::FAILURE;
    };

    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let module = match Module::read(&data) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot load {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match dump(&module, &sections) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("dump failed: {e}");
            ExitCode::FAILURE
        }
    }
}
