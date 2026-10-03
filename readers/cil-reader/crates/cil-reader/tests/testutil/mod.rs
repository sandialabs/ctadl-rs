//! Shared helpers for ported Cecil tests.
//!
//! Analogue of `BaseTestFixture.GetAssemblyResourcePath`
//! (`Test/Mono.Cecil.Tests/BaseTestFixture.cs:45-63`).
//!
//! The tests read two kinds of input, neither committed:
//!
//! - Cecil's own test assemblies, from the nix-pinned checkout named by
//!   `$CECIL_SRC`, or from a checkout cloned by hand into `extern/cecil`,
//!   which wins.
//! - Our fixtures, Cecil's .il/.cs test sources compiled by the root flake's
//!   `cilFixtures` derivation, from `$CIL_READER_FIXTURES`.
//!
//! The dev shell sets both variables. Without an input, the tests that need it
//! skip (`read_resource!` / `read_fixture!` / `require_cecil!` return early),
//! so that a plain `cargo test` from a fresh clone stays green; the nightly
//! `cil-reader-tests` flake check runs them all, and sets
//! `CIL_READER_REQUIRE_INPUTS=1` so a missing input fails there instead.

// Each test binary compiles this module separately and uses only some helpers.
#![allow(dead_code, unused_imports, unused_macros)]

use std::path::PathBuf;

/// `None` (after a note on stderr) when an input is absent, unless
/// `CIL_READER_REQUIRE_INPUTS` is set, in which case that is a failure.
fn absent(what: &str, how: &str) -> Option<PathBuf> {
    if std::env::var_os("CIL_READER_REQUIRE_INPUTS").is_some() {
        panic!("CIL_READER_REQUIRE_INPUTS is set but {what} were not found ({how})");
    }
    eprintln!("skipping: {what} not found ({how})");
    None
}

/// Root of the Cecil checkout the tests read, if there is one.
pub fn cecil_root() -> Option<PathBuf> {
    let vendored = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extern/cecil");
    if vendored.is_dir() {
        return Some(vendored);
    }
    std::env::var_os("CECIL_SRC")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            absent(
                "Cecil test resources",
                "set CECIL_SRC, as the dev shell does",
            )
        })
}

/// Directory of compiled fixtures, if there is one.
pub fn fixtures_root() -> Option<PathBuf> {
    std::env::var_os("CIL_READER_FIXTURES")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            absent(
                "compiled fixtures",
                "set CIL_READER_FIXTURES, as the dev shell does; `nix build .#cil-fixtures`",
            )
        })
}

/// Contents of a Cecil test assembly, or `None` when there is no Cecil
/// checkout. Panics if the checkout exists but lacks the file.
pub fn try_read_resource(name: &str) -> Option<Vec<u8>> {
    let path = cecil_root()?.join("Test/Resources/assemblies").join(name);
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display())))
}

/// Contents of a compiled fixture, or `None` when there are none. Panics if
/// the directory exists but lacks the file.
pub fn try_read_fixture(name: &str) -> Option<Vec<u8>> {
    let path = fixtures_root()?.join(name);
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display())))
}

/// `read_resource!(name)`: the bytes of a Cecil test assembly; returns from
/// the enclosing test (a skip) when there is no Cecil checkout.
macro_rules! read_resource {
    ($name:expr) => {
        match $crate::testutil::try_read_resource($name) {
            Some(data) => data,
            None => return,
        }
    };
}
pub(crate) use read_resource;

/// `read_fixture!(name)`: the bytes of a compiled fixture; returns from the
/// enclosing test (a skip) when there are no fixtures.
macro_rules! read_fixture {
    ($name:expr) => {
        match $crate::testutil::try_read_fixture($name) {
            Some(data) => data,
            None => return,
        }
    };
}
pub(crate) use read_fixture;

/// Returns from the enclosing test (a skip) when there is no Cecil checkout.
macro_rules! require_cecil {
    () => {
        if $crate::testutil::cecil_root().is_none() {
            return;
        }
    };
}
pub(crate) use require_cecil;

/// `BaseTestFixture.Normalize`: trim + CRLF→LF only; interior tabs are
/// significant.
pub fn normalize(s: &str) -> String {
    s.trim().replace("\r\n", "\n")
}
