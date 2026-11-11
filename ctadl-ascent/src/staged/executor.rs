//! Where the batches of a staged run run.

use crate::error::Error;
use crate::facts::IdMap;
use crate::index_engine::Parallelism;
use crate::staged::batch::run_batch;
use ctadl_index::staged::plan::BatchId;
use ctadl_index::staged::store::StageDir;

/// Runs the batches of a staged run. A batch's inputs and outputs are all in the stage
/// directory, so an executor needs only the batch's id.
pub trait Executor {
    /// Runs `batch`, whose dependencies are done, in `round` if it is a part of a split SCC, and
    /// marks the run done.
    fn run(&mut self, stage: &StageDir, batch: BatchId, round: Option<u32>) -> Result<(), Error>;
}

/// Runs each batch on the calling thread, one at a time, on the engine of `parallelism`.
pub struct InProcess<'a> {
    pub parallelism: Parallelism,
    /// Names functions in log lines.
    pub id_map: Option<&'a IdMap>,
}

impl Executor for InProcess<'_> {
    fn run(&mut self, stage: &StageDir, batch: BatchId, round: Option<u32>) -> Result<(), Error> {
        run_batch(stage, batch, round, self.parallelism, self.id_map).map(|_| ())
    }
}
