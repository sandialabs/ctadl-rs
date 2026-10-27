//! The index engine on a thread pool, with its large relations in the concurrent tries:
//! `ctadl_index::index_engine::index_rules` under `ascent_par!`. Each engine is its own crate so
//! that the engines compile in parallel.

// The `ascent!` block expands to code that trips several style lints (`.clone()` on Copy types,
// auto-borrows, unit-valued lets, and Default field reassignment). These are artifacts of the
// macro's generated code, not the hand-written rules.
#![allow(
    clippy::clone_on_copy,
    clippy::needless_borrow,
    clippy::let_unit_value,
    clippy::field_reassign_with_default
)]

use ascent::ascent_par;
use ctadl_index::index_engine::rules_prelude::*;
use ctadl_index::index_engine::{EngineInput, IndexResult};

ascent_par! {
    #![measure_rule_times]
    #![generate_run_timeout]
    struct ParIndexProg;
    include_source!(ctadl_index::index_engine::index_rules);
}
ctadl_index::trie_stores!(ParIndexProg);

/// Runs the index rules to fixpoint with rayon, in whatever pool the caller `install`s it in.
pub fn run(input: EngineInput<'_>) -> IndexResult {
    ctadl_index::run_engine!(ParIndexProg, input)
}
