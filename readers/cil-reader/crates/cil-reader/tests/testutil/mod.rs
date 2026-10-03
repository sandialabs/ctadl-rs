//! Shared helpers for ported Cecil tests.
//!
//! Analogue of `BaseTestFixture.GetAssemblyResourcePath`
//! (`Test/Mono.Cecil.Tests/BaseTestFixture.cs:45-63`).

// Each test binary compiles this module separately and uses only some helpers.
#![allow(dead_code)]

use std::path::PathBuf;

/// Path of a test assembly in the vendored Cecil resources.
pub fn cecil_resource(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.join("../../extern/cecil/Test/Resources/assemblies")
        .join(name)
}

/// Path of a fixture we compiled ourselves (see `cargo xtask fixtures`).
#[allow(dead_code)]
pub fn fixture(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.join("../../tests/fixtures").join(name)
}

pub fn read_resource(name: &str) -> Vec<u8> {
    let path = cecil_resource(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[allow(dead_code)]
pub fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// `BaseTestFixture.Normalize`: trim + CRLF→LF only; interior tabs are
/// significant.
#[allow(dead_code)]
pub fn normalize(s: &str) -> String {
    s.trim().replace("\r\n", "\n")
}
