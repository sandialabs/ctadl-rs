//! [`diff_sarif`]: the comparison of two SARIF files.

use std::path::Path;

use diff_report::Smallest;
use multiset_diff::multiset_diff;
use sarif_canon::types::{CanonResult, RawRun};
use sarif_canon::u32_atoms::{canon_runs_u32, cmp_result_u32};

use crate::report::Report;
use crate::{Atoms, Error, ErrorKind, read_runs, render};

/// The rule whose results `--endpoints-only` compares by their thread flows' endpoints.
pub const ENDPOINTS_RULE: &str = "C0001.tainted-path";

/// How to compare and how much to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The most differing results printed per side.
    pub limit: usize,
    /// Compare results of [`ENDPOINTS_RULE`] by the endpoints of their thread flows, leaving
    /// out their interior steps, primary locations and taint vertices.
    pub endpoints_only: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            limit: 20,
            endpoints_only: false,
        }
    }
}

/// Compares the results of the SARIF files `a` and `b`, pooled over each file's runs, as
/// multisets of canonical results. An error names the file it is about.
pub fn diff_sarif(a: &Path, b: &Path, opts: &Options) -> Result<Report, Error> {
    diff_with(a, b, opts, Atoms::new())
}

/// Compares the results of the SARIF files `a` and `b` as [`diff_sarif`] does, interning
/// their strings into `atoms`, which may already hold some. The report does not depend on
/// which atom each string gets.
pub(crate) fn diff_with(
    a: &Path,
    b: &Path,
    opts: &Options,
    mut atoms: Atoms,
) -> Result<Report, Error> {
    let parse = |file: &Path, atoms: &mut Atoms| {
        read_runs(file, atoms).map_err(|e| error(file, ErrorKind::Parse(e)))
    };
    let runs_a = parse(a, &mut atoms)?;
    let runs_b = parse(b, &mut atoms)?;

    // An absent atom means neither file has a result of the rule, so there is nothing to cut.
    let endpoints_rule = opts
        .endpoints_only
        .then(|| atoms.get_id(ENDPOINTS_RULE))
        .flatten();
    let canon = |file: &Path, runs: Vec<RawRun<u32>>| {
        canon_runs_u32(&runs, endpoints_rule).map_err(|e| error(file, ErrorKind::Canon(e)))
    };
    let ca = canon(a, runs_a)?;
    let cb = canon(b, runs_b)?;

    // Samples are chosen and ordered by their printed form, which does not depend on which
    // atom a string got.
    let render = |r: &CanonResult<u32>| render::result(r, &atoms);
    let by_text = |x: &CanonResult<u32>, y: &CanonResult<u32>| render(x).cmp(&render(y));
    let mut more_in_a = Smallest::new(opts.limit);
    let mut more_in_b = Smallest::new(opts.limit);
    for d in multiset_diff(&ca, &cb, cmp_result_u32) {
        if d.count_a > d.count_b {
            more_in_a.push(d, by_text);
        } else {
            more_in_b.push(d, by_text);
        }
    }
    Ok(Report {
        a: a.display().to_string(),
        b: b.display().to_string(),
        endpoints_only: opts.endpoints_only,
        limit: opts.limit,
        results_a: ca.len(),
        results_b: cb.len(),
        more_in_a: more_in_a.finish(by_text, render),
        more_in_b: more_in_b.finish(by_text, render),
    })
}

fn error(file: &Path, kind: ErrorKind) -> Error {
    Error {
        file: file.to_path_buf(),
        kind,
    }
}
