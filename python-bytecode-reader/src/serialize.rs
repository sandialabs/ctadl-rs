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
    let python = find_interpreter()?;
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

/// Locate a Python interpreter: `PYTHON` env var first, else `python3` on PATH.
fn find_interpreter() -> Result<std::path::PathBuf, SerializeError> {
    if let Some(python) = std::env::var_os("PYTHON") {
        return Ok(python.into());
    }
    which::which("python3").map_err(|_| SerializeError::NoInterpreter)
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
