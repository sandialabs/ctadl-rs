//! Shared helpers for ported Cecil tests.
//!
//! Analogue of `BaseTestFixture.GetAssemblyResourcePath`
//! (`Test/Mono.Cecil.Tests/BaseTestFixture.cs:45-63`).
//!
//! Most tests read Cecil's own test assemblies, which are not committed: they
//! come from the vendored `extern/cecil` (`cargo xtask vendor`) or, failing
//! that, the nix-pinned checkout named by `$CECIL_SRC`. With neither, those
//! tests skip (`read_resource!` / `require_cecil!` return early) so that a
//! plain `cargo test` from a fresh clone stays green. Setting
//! `CIL_READER_REQUIRE_CECIL=1` makes a missing checkout fail instead.

// Each test binary compiles this module separately and uses only some helpers.
#![allow(dead_code, unused_imports, unused_macros)]

use std::path::PathBuf;

/// Root of the Cecil checkout the tests read, if there is one.
pub fn cecil_root() -> Option<PathBuf> {
    let vendored = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extern/cecil");
    let root = if vendored.is_dir() {
        Some(vendored)
    } else {
        std::env::var_os("CECIL_SRC")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
    };
    if root.is_none() {
        if std::env::var_os("CIL_READER_REQUIRE_CECIL").is_some() {
            panic!(
                "CIL_READER_REQUIRE_CECIL is set but neither extern/cecil nor $CECIL_SRC exists"
            );
        }
        eprintln!(
            "skipping: Cecil test resources not found \
             (run `cargo xtask vendor` in readers/cil-reader, or set CECIL_SRC)"
        );
    }
    root
}

/// Path of a fixture we compiled ourselves (see `cargo xtask fixtures`).
pub fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.join("../../tests/fixtures").join(name)
}

/// Contents of a Cecil test assembly, or `None` when there is no Cecil
/// checkout. Panics if the checkout exists but lacks the file.
pub fn try_read_resource(name: &str) -> Option<Vec<u8>> {
    let path = cecil_root()?.join("Test/Resources/assemblies").join(name);
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

/// Returns from the enclosing test (a skip) when there is no Cecil checkout.
macro_rules! require_cecil {
    () => {
        if $crate::testutil::cecil_root().is_none() {
            return;
        }
    };
}
pub(crate) use require_cecil;

pub fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// `BaseTestFixture.Normalize`: trim + CRLF→LF only; interior tabs are
/// significant.
pub fn normalize(s: &str) -> String {
    s.trim().replace("\r\n", "\n")
}
