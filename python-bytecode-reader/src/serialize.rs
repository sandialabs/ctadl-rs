//! Running the embedded Python serializer.
//!
//! This is the single place that spawns a Python interpreter, reused by both the
//! CTADL frontend and the reader's own differential tests. It mirrors how the
//! pcode frontend shells out to Ghidra (`ctadl-ascent/.../pcode/ghidra.rs`), but
//! results come back on **stdout** (captured via [`std::process::Command::output`])
//! rather than in a facts directory.
//!
//! The `bytecode_text` package is embedded into this crate with `include_str!`
//! and staged to a fresh tempdir at run time, with `PYTHONPATH` pointed at it, so
//! CTADL is self-contained: it needs only a `python3` interpreter present — no
//! pip install, no user `PYTHONPATH` setup.

use std::path::Path;
use std::process::Command;

use crate::error::SerializeError;

/// Which output the serializer should produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The stable text this crate's grammar parses.
    Stable,
    /// The JSON oracle (same normalized records); used by the differential tests.
    Json,
}

impl Format {
    fn as_arg(self) -> &'static str {
        match self {
            Format::Stable => "stable",
            Format::Json => "json",
        }
    }
}

/// The embedded `bytecode_text` package: `(relative filename, contents)`.
const PACKAGE_FILES: &[(&str, &str)] = &[
    (
        "__init__.py",
        include_str!("../python/bytecode_text/__init__.py"),
    ),
    (
        "__main__.py",
        include_str!("../python/bytecode_text/__main__.py"),
    ),
    ("model.py", include_str!("../python/bytecode_text/model.py")),
    (
        "normalize.py",
        include_str!("../python/bytecode_text/normalize.py"),
    ),
    (
        "collect.py",
        include_str!("../python/bytecode_text/collect.py"),
    ),
    (
        "serialize.py",
        include_str!("../python/bytecode_text/serialize.py"),
    ),
];

/// Run the serializer against `source` (a `.py` or `.pyc` file), returning its
/// stdout as a `String`.
///
/// # Errors
///
/// - [`SerializeError::NoInterpreter`] if no interpreter can be found.
/// - [`SerializeError::Python`] if the serializer exits non-zero (stderr attached).
/// - [`SerializeError::Io`] / [`SerializeError::NonUtf8`] on I/O or decoding failure.
pub fn run_serializer(source: &Path, fmt: Format) -> Result<String, SerializeError> {
    let python = select_interpreter(source)?;
    let staging = stage_package()?;

    let mut command = Command::new(&python);
    command
        .env("PYTHONPATH", staging.path())
        .arg("-m")
        .arg("bytecode_text")
        .arg(source)
        .arg("--format")
        .arg(fmt.as_arg());

    let output = command.output()?;
    if !output.status.success() {
        return Err(SerializeError::Python {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|_| SerializeError::NonUtf8)
}

/// Choose the interpreter to run against `source`.
///
/// `.py` source (and stdin) compiles under whatever interpreter we pick, so any
/// one will do — [`find_interpreter`]. A `.pyc`, by contrast, carries a fixed
/// bytecode version in its 4-byte magic; `marshal` can crash or silently corrupt
/// on a mismatch, so we read that magic and dispatch to an interpreter whose own
/// bytecode magic matches exactly — [`find_matching_interpreter`].
fn select_interpreter(source: &Path) -> Result<std::path::PathBuf, SerializeError> {
    let is_pyc = source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pyc"));
    if is_pyc {
        find_matching_interpreter(source)
    } else {
        find_interpreter()
    }
}

/// Locate a Python interpreter: `PYTHON` env var first, else `python3` on PATH.
fn find_interpreter() -> Result<std::path::PathBuf, SerializeError> {
    if let Some(python) = std::env::var_os("PYTHON") {
        return Ok(python.into());
    }
    which::which("python3").map_err(|_| SerializeError::NoInterpreter)
}

/// Read the 4-byte bytecode magic from the head of a `.pyc`.
fn read_pyc_magic(source: &Path) -> Result<[u8; 4], SerializeError> {
    use std::io::Read;
    let mut file = std::fs::File::open(source).map_err(|e| SerializeError::Pyc {
        path: source.display().to_string(),
        message: e.to_string(),
    })?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|e| SerializeError::Pyc {
        path: source.display().to_string(),
        message: format!("reading magic: {e}"),
    })?;
    Ok(magic)
}

/// Candidate interpreters to probe for a `.pyc`, in priority order: `$PYTHON`
/// first (an explicit user choice), then `python3`, then versioned `python3.N`
/// across the range we might encounter. Names that don't resolve are skipped.
fn candidate_interpreters() -> Vec<std::path::PathBuf> {
    let mut names: Vec<String> = Vec::new();
    if let Some(python) = std::env::var_os("PYTHON") {
        return vec![python.into()]; // explicit override: probe only this one
    }
    names.push("python3".to_string());
    // Newest first — a .pyc is most likely from a current interpreter.
    for minor in (8..=15).rev() {
        names.push(format!("python3.{minor}"));
    }
    names
        .iter()
        .filter_map(|n| which::which(n).ok())
        .collect()
}

/// Ask an interpreter for its own bytecode magic (`importlib.util.MAGIC_NUMBER`).
/// Returns `None` if the interpreter can't be run or gives short output.
fn interpreter_magic(python: &Path) -> Option<[u8; 4]> {
    let output = Command::new(python)
        .args([
            "-c",
            "import importlib.util,sys;sys.stdout.buffer.write(importlib.util.MAGIC_NUMBER)",
        ])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() < 4 {
        return None;
    }
    Some([output.stdout[0], output.stdout[1], output.stdout[2], output.stdout[3]])
}

/// Find an interpreter whose bytecode magic matches this `.pyc` exactly. An
/// exact 4-byte match (not just major.minor) is the correct safety criterion:
/// it is precisely what guarantees `marshal.loads` will accept the payload.
fn find_matching_interpreter(source: &Path) -> Result<std::path::PathBuf, SerializeError> {
    let want = read_pyc_magic(source)?;
    let candidates = candidate_interpreters();
    if candidates.is_empty() {
        return Err(SerializeError::NoInterpreter);
    }
    let mut tried = Vec::new();
    for python in candidates {
        match interpreter_magic(&python) {
            Some(got) if got == want => return Ok(python),
            Some(got) => tried.push(format!("{} (magic {})", python.display(), hex4(got))),
            None => tried.push(format!("{} (unavailable)", python.display())),
        }
    }
    Err(SerializeError::NoMatchingInterpreter {
        path: source.display().to_string(),
        magic: hex4(want),
        tried: tried.join(", "),
    })
}

/// Format a 4-byte magic as lowercase hex, matching Python's `bytes.hex()`.
fn hex4(magic: [u8; 4]) -> String {
    magic.iter().map(|b| format!("{b:02x}")).collect()
}

/// Stage the embedded package into a fresh tempdir as `bytecode_text/*.py`, so
/// `python3 -m bytecode_text` can import it via `PYTHONPATH`. The returned
/// [`tempfile::TempDir`] must be kept alive until the interpreter has run.
fn stage_package() -> Result<tempfile::TempDir, SerializeError> {
    let dir = tempfile::Builder::new().prefix("ctadl-pybc").tempdir()?;
    let pkg = dir.path().join("bytecode_text");
    std::fs::create_dir_all(&pkg)?;
    for (name, contents) in PACKAGE_FILES {
        std::fs::write(pkg.join(name), contents)?;
    }
    Ok(dir)
}
