//! cargo xtask: repo maintenance commands.
//!
//! - `codegen`   — regenerate crates/cil-reader/src/cil/opcodes.rs from the
//!   vendored Cecil sources (provably matching Cecil's opcode table).
//! - `fixtures`  — build tests/fixtures from Cecil's .il/.cs test sources.
//! - `regression` — full-read sweep + golden dump diff over the corpus.
//! - `compare`   — run cil-dump vs CecilDump side by side.
//! - `bless`     — regenerate goldens from CecilDump.
//! - `vendor`    — materialise extern/cecil from the nix-pinned CECIL_SRC.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod codegen;
mod compare;
mod fixtures;
mod regression;
mod vendor;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    let rest = &args[1.min(args.len())..];

    // Everything below reads Cecil's sources or test assemblies.
    if let Some("codegen" | "fixtures" | "regression" | "compare" | "bless") = cmd
        && let Err(e) = vendor::ensure(&root())
    {
        eprintln!("vendor: {e}");
        return ExitCode::FAILURE;
    }

    match cmd {
        Some("codegen") => codegen::run(&root()),
        Some("fixtures") => fixtures::run(&root()),
        Some("regression") => regression::run(&root(), rest),
        Some("compare") => compare::run(&root(), rest, false),
        Some("bless") => compare::run(&root(), rest, true),
        Some("vendor") => vendor::run(&root()),
        Some(other) => {
            eprintln!("unknown xtask command: {other}");
            eprintln!("available: codegen, fixtures, regression, compare, bless, vendor");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("usage: cargo xtask <command>");
            eprintln!();
            eprintln!("commands:");
            eprintln!("  codegen     regenerate opcodes.rs from Cecil's OpCodes.cs");
            eprintln!("  fixtures    compile Cecil's .il/.cs test fixtures (needs nix devshell)");
            eprintln!("  regression  full-read sweep + golden dump diff over the corpus");
            eprintln!("  compare     cil-dump vs CecilDump side by side (needs nix devshell)");
            eprintln!(
                "  bless       regenerate tests/expected goldens from CecilDump (needs nix devshell)"
            );
            eprintln!("  vendor      copy Cecil from $CECIL_SRC (nix-pinned) into extern/cecil");
            ExitCode::FAILURE
        }
    }
}
