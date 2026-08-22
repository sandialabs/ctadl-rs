//! `cargo xtask vendor`: materialise Mono.Cecil at `extern/cecil`.
//!
//! Cecil is never committed here — it is the upstream we port from, and its
//! test resources alone are ~59 MB. Nix owns the pin: `flake.lock` records
//! the commit and the devshell exports `CECIL_SRC` pointing at the read-only
//! store copy. This command copies that into `extern/cecil` (gitignored).
//!
//! Why a copy and not a symlink to the store: `dotnet build` writes `obj/`
//! next to the project it builds, and CecilDump project-references
//! `extern/cecil/Mono.Cecil.csproj` (§6) — MSBuild fails with "Access to the
//! path .../obj is denied" against a read-only store path.
//!
//! A hand-managed `extern/cecil` (a checkout you cloned yourself — no stamp
//! file) is left alone, so a local clone still overrides the pin.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Written inside the copy; records the `CECIL_SRC` it came from. Its absence
/// means the tree is hand-managed and must not be touched.
const STAMP: &str = ".cecil-src";

pub fn run(root: &Path) -> ExitCode {
    match ensure(root) {
        Ok(()) => {
            println!("vendor: extern/cecil ready");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vendor: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Make sure `extern/cecil` exists and matches the pinned source. Silent
/// no-op when it is already current or hand-managed; the other xtask
/// commands call this before they touch the corpus.
pub fn ensure(root: &Path) -> Result<(), String> {
    let dest = root.join("extern/cecil");
    let stamp = dest.join(STAMP);

    let Some(src) = std::env::var_os("CECIL_SRC").map(PathBuf::from) else {
        return if dest.is_dir() {
            Ok(())
        } else {
            Err(format!(
                "{} is missing and CECIL_SRC is unset.\n\
                 Enter the devshell (`nix develop`), which pins Cecil and sets\n\
                 CECIL_SRC, or clone it yourself:\n  \
                 git clone https://github.com/jbevain/cecil extern/cecil",
                dest.display()
            ))
        };
    };
    if !src.join("Mono.Cecil.csproj").is_file() {
        return Err(format!(
            "CECIL_SRC={} does not look like a Cecil checkout",
            src.display()
        ));
    }

    match std::fs::read_to_string(&stamp) {
        // Already vendored from this exact source.
        Ok(s) if s.trim() == src.to_string_lossy() => return Ok(()),
        // Vendored from a different source: the pin moved, replace it.
        Ok(_) => {
            std::fs::remove_dir_all(&dest).map_err(|e| format!("rm {}: {e}", dest.display()))?;
        }
        // No stamp but a tree is there: hand-managed, leave it alone.
        Err(_) if dest.is_dir() => return Ok(()),
        Err(_) => {}
    }

    eprintln!("vendor: copying Cecil from {} …", src.display());
    std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    // `cp -R src/. dest` copies contents; the store is r-xr-xr-x, so make the
    // copy writable for dotnet's obj/bin.
    sh(Command::new("cp").arg("-R").arg(src.join(".")).arg(&dest))?;
    sh(Command::new("chmod").arg("-R").arg("u+w").arg(&dest))?;
    std::fs::write(&stamp, format!("{}\n", src.display()))
        .map_err(|e| format!("write {}: {e}", stamp.display()))?;
    Ok(())
}

fn sh(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} failed with {status}"))
    }
}
