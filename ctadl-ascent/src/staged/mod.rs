//! The staged index run, which runs the index fixpoint in batches of functions, callees first.
//! `ctadl_index::staged` holds the parts that need no engine: the stage directory, its files and
//! the plan.
//!
//! [`run::run_staged`] plans a run, writes the stage directory, runs each batch through an
//! [`executor::Executor`] and assembles the index tables. A batch runs in
//! [`batch::run_batch`], which reads its inputs only from the stage directory. [`compare`]
//! checks one run's output tables against another's.

pub mod batch;
pub mod compare;
pub mod executor;
pub mod run;

#[cfg(test)]
mod tests;
