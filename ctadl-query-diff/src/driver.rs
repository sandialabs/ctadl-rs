//! [`diff_query`]: the comparison of two SARIF files.

use std::cmp::Ordering;
use std::path::Path;

use diff_report::{Samples, Smallest};
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
    /// The most differing rows printed per side, for each of results, notifications and
    /// invocations.
    pub limit: usize,
    /// Compare results of [`ENDPOINTS_RULE`] by the endpoints of their thread flows, leaving
    /// out their interior steps, primary locations and result taint vertices. Each endpoint
    /// is compared whole, its step properties included.
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

/// Compares the SARIF files `a` and `b`, each pooled over its runs: their results as multisets
/// of canonical results, and their execution notifications and invocation statuses as
/// multisets of rows. An error names the file it is about.
pub fn diff_query(a: &Path, b: &Path, opts: &Options) -> Result<Report, Error> {
    diff_with(a, b, opts, Atoms::new())
}

/// Compares the SARIF files `a` and `b` as [`diff_query`] does, interning their strings into
/// `atoms`, which may already hold some. The report does not depend on which atom each string
/// gets.
pub(crate) fn diff_with(
    a: &Path,
    b: &Path,
    opts: &Options,
    mut atoms: Atoms,
) -> Result<Report, Error> {
    let parse = |file: &Path, atoms: &mut Atoms| {
        read_runs(file, atoms).map_err(|e| error(file, ErrorKind::Parse(e)))
    };
    let pa = parse(a, &mut atoms)?;
    let pb = parse(b, &mut atoms)?;

    // An absent atom means neither file has a result of the rule, so there is nothing to cut.
    let endpoints_rule = opts
        .endpoints_only
        .then(|| atoms.get_id(ENDPOINTS_RULE))
        .flatten();
    let canon = |file: &Path, runs: Vec<RawRun<u32>>| {
        canon_runs_u32(&runs, endpoints_rule).map_err(|e| error(file, ErrorKind::Canon(e)))
    };
    let ca = canon(a, pa.runs)?;
    let cb = canon(b, pb.runs)?;

    // Samples are chosen and ordered by their printed form, which does not depend on which
    // atom a string got.
    let render = |r: &CanonResult<u32>| render::result(r, &atoms);
    let by_text = |x: &CanonResult<u32>, y: &CanonResult<u32>| render(x).cmp(&render(y));
    let (more_in_a, more_in_b) = differing(&ca, &cb, cmp_result_u32, by_text, render, opts.limit);
    // A row is its own printed form, so it is compared and sampled by the same order.
    let (notifications_more_in_a, notifications_more_in_b) = differing(
        &pa.notifications,
        &pb.notifications,
        String::cmp,
        String::cmp,
        String::clone,
        opts.limit,
    );
    let (invocations_more_in_a, invocations_more_in_b) = differing(
        &pa.statuses,
        &pb.statuses,
        String::cmp,
        String::cmp,
        String::clone,
        opts.limit,
    );
    Ok(Report {
        a: a.display().to_string(),
        b: b.display().to_string(),
        endpoints_only: opts.endpoints_only,
        limit: opts.limit,
        results_a: ca.len(),
        results_b: cb.len(),
        more_in_a,
        more_in_b,
        notifications_a: pa.notifications.len(),
        notifications_b: pb.notifications.len(),
        notifications_more_in_a,
        notifications_more_in_b,
        invocations_a: pa.statuses.len(),
        invocations_b: pb.statuses.len(),
        invocations_more_in_a,
        invocations_more_in_b,
    })
}

/// The values `a` has more of than `b`, and those `b` has more of, both sorted by `cmp`, as at
/// most `limit` samples each: the first by `by_text`, each printed by `render`.
fn differing<T>(
    a: &[T],
    b: &[T],
    cmp: impl Fn(&T, &T) -> Ordering,
    by_text: impl Fn(&T, &T) -> Ordering,
    render: impl Fn(&T) -> String,
    limit: usize,
) -> (Samples, Samples) {
    let mut more_in_a = Smallest::new(limit);
    let mut more_in_b = Smallest::new(limit);
    for d in multiset_diff(a, b, cmp) {
        if d.count_a > d.count_b {
            more_in_a.push(d, &by_text);
        } else {
            more_in_b.push(d, &by_text);
        }
    }
    (
        more_in_a.finish(&by_text, &render),
        more_in_b.finish(&by_text, &render),
    )
}

fn error(file: &Path, kind: ErrorKind) -> Error {
    Error {
        file: file.to_path_buf(),
        kind,
    }
}
