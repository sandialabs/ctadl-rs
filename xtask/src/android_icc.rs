//! Nightly Android ICC external-suite runner.
//!
//! Phase 5 keeps DroidBench / ICC-Bench style fixtures out of the fast default
//! regression path, but gives them a first-class runner so pinned APKs can be
//! dropped under `nightly/tests/android-icc/` without changing Rust code.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::assertions;
use crate::regression::Outcome;

#[derive(Debug, Deserialize)]
pub struct CaseSpec {
    /// APK path relative to this spec file, or absolute.
    pub apk: PathBuf,
    /// Query model path relative to this spec file, or absolute.
    pub model: PathBuf,
    /// Whether a source-to-sink path is expected.
    #[serde(default = "default_true")]
    pub expect_flow: bool,
    /// Intent-pair rows expected in `intent_pair.parquet`, if this case wants to assert them.
    pub expected_intent_pairs: Option<usize>,
    /// Current expected status for this external case.
    #[serde(default = "default_status")]
    pub status: CaseStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CaseStatus {
    Pass,
    Xfail,
    Unsupported,
    Phase6Plus,
}

fn default_status() -> CaseStatus {
    CaseStatus::Pass
}

fn default_true() -> bool {
    true
}

pub fn load_spec(path: &Path) -> Result<CaseSpec> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    json5::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn resolve_paths(spec_path: &Path, spec: &CaseSpec) -> (PathBuf, PathBuf) {
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let apk = resolve_one(base, &spec.apk);
    let model = resolve_one(base, &spec.model);
    (apk, model)
}

fn resolve_one(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

pub fn check_sarif(spec: &CaseSpec, sarif: &Path) -> Result<Outcome> {
    let has_flow = assertions::codeflow_connects_source_and_sink(sarif)?;
    match (spec.expect_flow, has_flow) {
        (true, true) | (false, false) => Ok(Outcome::Pass),
        (true, false) => Ok(Outcome::Fail(
            "expected source-to-sink ICC flow, found none".into(),
        )),
        (false, true) => Ok(Outcome::Fail(
            "expected no ICC flow, but SARIF contains one".into(),
        )),
    }
}

pub fn check_intent_pairs(index_dir: &Path, expected: Option<usize>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let pairs = ctadl_ascent::facts::schema::intent_pair::try_load(index_dir)?;
    if pairs.len() != expected {
        bail!(
            "expected {expected} intent pair row(s), found {} in {}",
            pairs.len(),
            index_dir.display()
        );
    }
    Ok(())
}
