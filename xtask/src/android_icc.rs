//! Nightly Android ICC external-suite runner.
//!
//! Phase 5 keeps DroidBench / ICC-Bench style fixtures out of the fast default
//! regression path, but gives them a first-class runner. The APKs are not in
//! the repo: the flake fetches them at a pinned commit and names the directory
//! in `CTADL_ANDROID_ICC_APKS`, which the dev shell and the regression check set.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::assertions;
use crate::regression::Outcome;

#[derive(Debug, Deserialize)]
pub struct CaseSpec {
    /// APK file name inside the `CTADL_ANDROID_ICC_APKS` directory.
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

/// Where the fetched benchmark APKs live; see the module docs.
pub const APKS_ENV: &str = "CTADL_ANDROID_ICC_APKS";

/// The case's APK (`None` when `CTADL_ANDROID_ICC_APKS` is unset) and model file.
pub fn resolve_paths(spec_path: &Path, spec: &CaseSpec) -> (Option<PathBuf>, PathBuf) {
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let apk = std::env::var_os(APKS_ENV).map(|dir| PathBuf::from(dir).join(&spec.apk));
    (apk, base.join(&spec.model))
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
