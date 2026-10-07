//! cil-reader's ports of Mono.Cecil's test suites (`Test/Mono.Cecil.Tests`,
//! read subset), run by the `cil:cecil-tests` check. Every one reads Cecil's
//! test assemblies or the compiled fixtures, neither of which a checkout has,
//! so they live here rather than under `cargo test`.
//!
//! Each module ports one Cecil test class, except `malformed`, which covers
//! input Cecil cannot read. A test is a plain `fn()` that panics on failure,
//! listed in its module's `TESTS`; the check runs them all and reports each
//! panic. Analogue of `BaseTestFixture`
//! (`Test/Mono.Cecil.Tests/BaseTestFixture.cs`): [`read_resource`] and
//! [`read_fixture`] read an input by name and panic if it is missing.

// The tests keep the names of the Cecil code they port, `foo` and `baz` included.
#![allow(clippy::disallowed_names)]

mod assembly;
mod clr_layouts;
mod custom_attributes;
mod fields;
mod image_read;
mod malformed;
mod method_body;
mod methods;
mod module;
mod nested_types;
mod parameters;
mod properties_events;
mod resolve;
mod security_declarations;
mod types;

use std::cell::{Cell, RefCell};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Once, RwLock};

use super::{parallel_map, to_outcome};
use crate::regression::Outcome;

type Test = (&'static str, fn());

const SUITES: &[(&str, &[Test])] = &[
    ("assembly", assembly::TESTS),
    ("clr_layouts", clr_layouts::TESTS),
    ("custom_attributes", custom_attributes::TESTS),
    ("fields", fields::TESTS),
    ("image_read", image_read::TESTS),
    ("malformed", malformed::TESTS),
    ("method_body", method_body::TESTS),
    ("methods", methods::TESTS),
    ("module", module::TESTS),
    ("nested_types", nested_types::TESTS),
    ("parameters", parameters::TESTS),
    ("properties_events", properties_events::TESTS),
    ("resolve", resolve::TESTS),
    ("security_declarations", security_declarations::TESTS),
    ("types", types::TESTS),
];

/// The Cecil checkout and the compiled fixtures, for the tests' helpers.
static INPUTS: RwLock<Option<(PathBuf, PathBuf)>> = RwLock::new(None);

/// Run every test; each panic is a failure, reported as `suite::test`.
pub fn run(cecil: &Path, fixtures: &Path) -> Outcome {
    *INPUTS.write().unwrap() = Some((cecil.to_path_buf(), fixtures.to_path_buf()));
    install_panic_hook();
    let tests: Vec<(String, fn())> = SUITES
        .iter()
        .flat_map(|(suite, tests)| {
            tests
                .iter()
                .map(move |(name, test)| (format!("{suite}::{name}"), *test))
        })
        .collect();
    let results = parallel_map(&tests, |(_, test)| run_one(*test));
    let failures = tests
        .iter()
        .zip(results)
        .filter_map(|((name, _), result)| result.err().map(|e| format!("{name}: {e}")))
        .collect();
    to_outcome(failures, "test(s)")
}

thread_local! {
    /// Set while this thread runs a test, whose panic message the hook keeps
    /// in `PANIC_MESSAGE` instead of printing.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    static PANIC_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Chain a hook in front of the existing one that captures test panics. Other
/// threads' panics, and this thread's outside a test, still go to the old one.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if CAPTURING.get() {
                PANIC_MESSAGE.set(Some(info.to_string()));
            } else {
                previous(info);
            }
        }));
    });
}

fn run_one(test: fn()) -> Result<(), String> {
    CAPTURING.set(true);
    let result = panic::catch_unwind(test);
    CAPTURING.set(false);
    result.map_err(|_| {
        PANIC_MESSAGE
            .take()
            .unwrap_or_else(|| "panicked".to_string())
    })
}

/// Root of the Cecil checkout.
pub fn cecil_root() -> PathBuf {
    let inputs = INPUTS.read().unwrap();
    inputs.as_ref().expect("inputs set by `run`").0.clone()
}

fn fixtures_root() -> PathBuf {
    let inputs = INPUTS.read().unwrap();
    inputs.as_ref().expect("inputs set by `run`").1.clone()
}

fn read(path: PathBuf) -> Vec<u8> {
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Contents of a Cecil test assembly (`Test/Resources/assemblies/<name>`).
pub fn read_resource(name: &str) -> Vec<u8> {
    read(cecil_root().join("Test/Resources/assemblies").join(name))
}

/// Contents of a compiled fixture.
pub fn read_fixture(name: &str) -> Vec<u8> {
    read(fixtures_root().join(name))
}

/// `BaseTestFixture.Normalize`: trim + CRLF→LF only; interior tabs are
/// significant.
pub fn normalize(s: &str) -> String {
    s.trim().replace("\r\n", "\n")
}
