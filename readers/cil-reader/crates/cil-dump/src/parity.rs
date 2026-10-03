//! Cecil parity harness: the corpus, the committed goldens, and the
//! Cecil-side dumper (`tools/CecilDump`).
//!
//! Used by the root workspace's xtask: the `cil:*` checks of `cargo xtask
//! regression --frontend cil`, and `cargo xtask cil bless`, which writes the
//! goldens those checks read.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cecil's test-resource directories, relative to a Cecil checkout, each with
/// the tag that prefixes its golden names. Several inputs share a file name
/// across these directories, hence the tag.
pub const CECIL_CORPUS_DIRS: &[(&str, &str)] = &[
    ("main", "Test/Resources/assemblies"),
    ("rocks", "rocks/Test/Resources/assemblies"),
    ("mdb", "symbols/mdb/Test/Resources/assemblies"),
    ("pdb", "symbols/pdb/Test/Resources/assemblies"),
];

/// Tag of cil-reader's own fixtures: Cecil's .il/.cs test sources, compiled
/// by the root flake's `cilFixtures` derivation rather than committed.
pub const FIXTURES_TAG: &str = "fixtures";

/// Environment variable naming the compiled fixtures directory; the dev shell
/// and the flake checks set it to `cilFixtures`.
pub const FIXTURES_ENV: &str = "CIL_READER_FIXTURES";

/// Committed goldens (CecilDump output), relative to the cil-reader directory.
pub const GOLDENS_DIR: &str = "tests/expected";

/// Inputs that are not ECMA-335 CLI images (or intentionally malformed):
/// reading them must FAIL. Relative to Cecil's main resource directory.
pub const MUST_FAIL: &[&str] = &["text_file.txt"];

/// Compared side-by-side but too big to commit goldens for.
pub const NO_GOLDEN: &[&str] = &[
    "Mono.Android.dll",
    "System.Private.Xml.dll",
    "mscorlib.dll",
    "FSharp.Core.dll",
    "cecil.dll",
];

/// Excluded from the CecilDump comparison: Cecil applies WinRT projections
/// on Windows metadata (renamed types, virtual assembly refs); we read the
/// raw metadata (PLAN.md non-goal).
pub fn compare_skip(name: &str) -> bool {
    name.ends_with(".winmd")
}

/// One corpus assembly.
pub struct Input {
    /// Which corpus directory it came from; see [`CECIL_CORPUS_DIRS`].
    pub tag: &'static str,
    pub path: PathBuf,
}

impl Input {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// Unique, filesystem-safe name: the tag plus the file name.
    pub fn id(&self) -> String {
        format!("{}-{}", self.tag, self.name())
    }

    /// File name of this input's golden under [`GOLDENS_DIR`], if it gets
    /// one. Goldens are kept only for Cecil's committed assemblies: the
    /// fixtures are build outputs, so a golden would pin one toolchain's
    /// bytes (MVIDs and all). The live CecilDump comparison covers them, as
    /// it does the [`NO_GOLDEN`] inputs.
    pub fn golden_name(&self) -> Option<String> {
        let skip = self.tag == FIXTURES_TAG || NO_GOLDEN.contains(&self.name().as_str());
        (!skip).then(|| format!("{}.dump", self.id()))
    }
}

/// Every .dll/.exe/.netmodule/.winmd under Cecil's test resources (`cecil` is
/// a Cecil checkout) plus, if given, the compiled fixtures. Missing
/// directories are skipped.
pub fn corpus(cecil: &Path, fixtures: Option<&Path>) -> Vec<Input> {
    let dirs = CECIL_CORPUS_DIRS
        .iter()
        .map(|&(tag, rel)| (tag, cecil.join(rel)))
        .chain(fixtures.map(|f| (FIXTURES_TAG, f.to_path_buf())));
    let mut files = Vec::new();
    for (tag, dir) in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "dll" | "exe" | "netmodule" | "winmd") {
                files.push(Input { tag, path });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Paths of the [`MUST_FAIL`] inputs in a Cecil checkout.
pub fn must_fail_inputs(cecil: &Path) -> Vec<PathBuf> {
    MUST_FAIL
        .iter()
        .map(|name| cecil.join(CECIL_CORPUS_DIRS[0].1).join(name))
        .collect()
}

/// Full dump of an image with every section, as the goldens hold it. A panic
/// inside the reader is reported as an error rather than unwinding.
pub fn rust_dump(data: &[u8]) -> Result<String, String> {
    let sections: Vec<crate::Section> = crate::ALL_SECTIONS.iter().map(|&(_, s)| s).collect();
    match std::panic::catch_unwind(|| -> cil_reader::Result<String> {
        let module = cil_reader::Module::read(data)?;
        crate::dump(&module, &sections)
    }) {
        Ok(Ok(dump)) => Ok(dump),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("panicked".to_string()),
    }
}

/// First line at which `expected` and `actual` differ (1-based), with the two
/// lines when both exist.
pub fn first_divergence(expected: &str, actual: &str) -> (usize, Option<(String, String)>) {
    for (i, (le, la)) in expected.lines().zip(actual.lines()).enumerate() {
        if le != la {
            return (i + 1, Some((le.to_string(), la.to_string())));
        }
    }
    (
        expected.lines().count().min(actual.lines().count()) + 1,
        None,
    )
}

/// Environment variable naming the CecilDump launcher; overrides PATH lookup.
pub const CECIL_DUMP_ENV: &str = "CECIL_DUMP";

/// The Cecil side of the comparison: a command that takes an assembly path
/// and prints its dump.
pub struct CecilDump {
    program: OsString,
    args: Vec<OsString>,
}

impl CecilDump {
    /// The prebuilt launcher: `$CECIL_DUMP`, else `cecil-dump` on PATH (the
    /// nix devshell provides it; see `cecilDump` in the root flake).
    pub fn locate() -> Option<Self> {
        let program = std::env::var_os(CECIL_DUMP_ENV)
            .map(PathBuf::from)
            .or_else(|| which("cecil-dump"))?;
        Some(CecilDump {
            program: program.into(),
            args: Vec::new(),
        })
    }

    /// A CecilDump.dll built by hand, run through `dotnet` on PATH.
    pub fn from_dll(dll: &Path) -> Self {
        CecilDump {
            program: "dotnet".into(),
            args: vec![dll.into()],
        }
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![self.program.to_string_lossy().into_owned()];
        parts.extend(self.args.iter().map(|a| a.to_string_lossy().into_owned()));
        parts.join(" ")
    }

    pub fn dump(&self, input: &Path) -> Result<String, String> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .arg(input)
            // Keep the first-run banner and telemetry notice off stdout,
            // which is the dump.
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
            .output()
            .map_err(|e| format!("{}: {e}", self.describe()))?;
        if !output.status.success() {
            return Err(format!(
                "CecilDump failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|p| p.is_file())
}
