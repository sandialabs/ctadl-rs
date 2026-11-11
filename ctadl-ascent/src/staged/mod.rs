//! The staged index run, which runs the index fixpoint in batches of functions, callees first.
//! `ctadl_index::staged` holds the parts that need no engine: the stage directory, its files and
//! the plan.
//!
//! [`run::run_staged`] plans a run, writes the stage directory, schedules the batches on an
//! [`executor::Executor`] and assembles the index tables. A batch runs in
//! [`batch::run_batch`], which reads its inputs only from the stage directory, either on the
//! calling thread or in a [`worker`] process, which reports its [`footprint`]. Batches whose
//! callees are done run at once while their estimates fit the budget. The parts of a split
//! recursive SCC run one at a time, in rounds, until their summaries stop growing. [`compare`]
//! checks one run's output tables against another's.

pub mod batch;
pub mod compare;
pub mod executor;
pub mod footprint;
pub mod run;
pub mod worker;

#[cfg(test)]
mod tests;
