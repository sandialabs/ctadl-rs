//! A worker process of a staged run: `ctadl index-worker`, which runs one job.

use std::path::Path;

use crate::error::Error;
use crate::index_engine::Parallelism;
use crate::staged::batch::path_set;
use crate::staged::executor::Job;
use crate::staged::footprint;
use ctadl_index::staged::store::StageDir;

/// Test hooks a worker reads, as `name=job` pairs separated by commas, where `job` is a
/// [`Job::name`]:
/// - `fail=batch-0` makes batch 0's worker exit with status 3 before it runs;
/// - `noise=batch-0` makes it log [`NOISE_BYTES`] at `trace` before it runs.
pub const TEST_HOOK_VAR: &str = "CTADL_STAGED_TEST_HOOK";

/// What the `noise` hook logs.
pub const NOISE_BYTES: usize = 2 << 20;

/// The exit status of a worker failed by [`TEST_HOOK_VAR`].
pub const TEST_HOOK_FAIL_STATUS: i32 = 3;

/// Runs `job` of the run in `stage_dir`, on the engine of `parallelism`, while reporting this
/// process's footprint on stdout. It maps the run's path set first, before anything interns a
/// path, so that the job's paths are numbered as the file numbers them.
pub fn run_worker(stage_dir: &Path, job: Job, parallelism: Parallelism) -> Result<(), Error> {
    footprint::report_to_stdout()?;
    if test_hook("fail", job) {
        log::error!("{job}: failing, as {TEST_HOOK_VAR} says");
        std::process::exit(TEST_HOOK_FAIL_STATUS);
    }
    if test_hook("noise", job) {
        let line = "noise ".repeat(170);
        for _ in 0..NOISE_BYTES / line.len() {
            log::trace!("{line}");
        }
    }
    let stage = StageDir::from_root(stage_dir);
    path_set(&stage)?;
    job.run(&stage, parallelism, None)
}

/// Whether [`TEST_HOOK_VAR`] sets hook `name` for `job`.
fn test_hook(name: &str, job: Job) -> bool {
    let Ok(hooks) = std::env::var(TEST_HOOK_VAR) else {
        return false;
    };
    let job = job.name();
    hooks
        .split(',')
        .filter_map(|hook| hook.split_once('='))
        .any(|(n, j)| n == name && j == job)
}
