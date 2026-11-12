//! A worker process of a staged run: `ctadl index-worker`, which runs one job.

use std::path::Path;
use std::time::Duration;

use crate::error::Error;
use crate::index_engine::Parallelism;
use crate::staged::batch::path_set;
use crate::staged::executor::Job;
use crate::staged::footprint;
use ctadl_index::staged::plan::{Batch, BatchId};
use ctadl_index::staged::store::StageDir;

/// Test hooks, as `name=job` or `name=job:value` entries separated by commas, where `job` is a
/// [`Job::name`] or `*` for every job. The first entry of a name that matches a job applies.
/// A worker reads:
/// - `fail=batch-0` makes batch 0's worker exit with status 3 before it runs;
/// - `noise=batch-0` makes it log [`NOISE_BYTES`] at `trace` before it runs;
/// - `footprint=batch-0:500` makes it report 500 MB per function of its batch in place of its
///   footprint, and `footprint=batch-0:estimate` its batch's estimate. It reports it for
///   [`HOOK_HOLD`] before it runs, so the scheduler sees it.
///
/// [`super::executor::Process`] reads `scale=*:0.5`, which makes the scheduler halve every
/// estimate.
pub const TEST_HOOK_VAR: &str = "CTADL_STAGED_TEST_HOOK";

/// What the `noise` hook logs.
pub const NOISE_BYTES: usize = 2 << 20;

/// The exit status of a worker failed by [`TEST_HOOK_VAR`].
pub const TEST_HOOK_FAIL_STATUS: i32 = 3;

/// How long a worker under the `footprint` hook reports its footprint before it runs.
pub const HOOK_HOLD: Duration = Duration::from_millis(300);

/// Runs `job` of the run in `stage_dir`, on the engine of `parallelism`, while reporting this
/// process's footprint on stdout. It maps the run's path set first, before anything interns a
/// path, so that the job's paths are numbered as the file numbers them.
pub fn run_worker(stage_dir: &Path, job: Job, parallelism: Parallelism) -> Result<(), Error> {
    let hooks = std::env::var(TEST_HOOK_VAR).unwrap_or_default();
    let name = job.name();
    let stage = StageDir::from_root(stage_dir);
    let reported = match (test_hook(&hooks, "footprint", Some(&name)), job) {
        (Some(value), Job::Batch { id, .. }) => Some(hooked_footprint(&stage, id, value)?),
        _ => None,
    };
    footprint::report_to_stdout(reported)?;
    if test_hook(&hooks, "fail", Some(&name)).is_some() {
        log::error!("{job}: failing, as {TEST_HOOK_VAR} says");
        std::process::exit(TEST_HOOK_FAIL_STATUS);
    }
    if test_hook(&hooks, "noise", Some(&name)).is_some() {
        let line = "noise ".repeat(170);
        for _ in 0..NOISE_BYTES / line.len() {
            log::trace!("{line}");
        }
    }
    if let Some(mb) = reported {
        log::info!("{job}: reporting a footprint of {mb:.1} MB, as {TEST_HOOK_VAR} says");
        std::thread::sleep(HOOK_HOLD);
    }
    path_set(&stage)?;
    job.run(&stage, parallelism, None)
}

/// The footprint, in MB, that the `footprint` hook's `value` gives batch `id`.
fn hooked_footprint(stage: &StageDir, id: BatchId, value: &str) -> Result<f64, Error> {
    let batch = Batch::load(stage, id)?;
    if value == "estimate" {
        // Rounded down to the tenth of a MB a report holds, so that a batch whose estimate
        // fits the budget is reported to fit it.
        let tenths = batch.estimate * 10 / (1 << 20);
        return Ok(tenths as f64 / 10.0);
    }
    let per_function: f64 = value.parse().map_err(|_| {
        Error::Staged(format!(
            "{TEST_HOOK_VAR}: footprint takes MB per function or `estimate`, not {value:?}"
        ))
    })?;
    Ok(per_function * batch.functions.len() as f64)
}

/// The value of the first entry of `hooks` (see [`TEST_HOOK_VAR`]) named `name` that matches
/// the job named `job`, or every job when `job` is `None`: the empty string if it has none.
pub fn test_hook<'a>(hooks: &'a str, name: &str, job: Option<&str>) -> Option<&'a str> {
    hooks.split(',').find_map(|hook| {
        let (n, rest) = hook.split_once('=')?;
        let (j, value) = rest.split_once(':').unwrap_or((rest, ""));
        (n == name && (j == "*" || Some(j) == job)).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hooks_parse() {
        let hooks = "fail=batch-1,footprint=batch-0:500,footprint=*:estimate,scale=*:0.5";
        assert_eq!(test_hook(hooks, "fail", Some("batch-1")), Some(""));
        assert_eq!(test_hook(hooks, "fail", Some("batch-0")), None);
        assert_eq!(test_hook(hooks, "footprint", Some("batch-0")), Some("500"));
        assert_eq!(
            test_hook(hooks, "footprint", Some("batch-2")),
            Some("estimate")
        );
        assert_eq!(test_hook(hooks, "scale", None), Some("0.5"));
        assert_eq!(test_hook(hooks, "noise", Some("batch-0")), None);
        assert_eq!(test_hook("", "fail", Some("batch-0")), None);
    }
}
