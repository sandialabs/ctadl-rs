//! The index engine on one thread, with its large relations in the serial tries:
//! `ctadl_index::index_engine::index_rules` under `ascent!`. Each engine is its own crate so that
//! the engines compile in parallel.

// The `ascent!` block expands to code that trips several style lints (`.clone()` on Copy types,
// auto-borrows, unit-valued lets, and Default field reassignment). These are artifacts of the
// macro's generated code, not the hand-written rules.
#![allow(
    clippy::clone_on_copy,
    clippy::needless_borrow,
    clippy::let_unit_value,
    clippy::field_reassign_with_default
)]

use ascent::ascent;
use ctadl_index::index_engine::rules_prelude::*;
use ctadl_index::index_engine::{EngineInput, IndexResult};

ascent! {
    #![measure_rule_times]
    #![generate_run_timeout]
    struct IndexProg;
    include_source!(ctadl_index::index_engine::index_rules);
}
ctadl_index::trie_stores!(IndexProg);
ctadl_index::cond_stores!(IndexProg, cond_trie);

/// Runs the index rules to fixpoint on the calling thread.
pub fn run(input: EngineInput<'_>) -> IndexResult {
    ctadl_index::run_engine!(IndexProg, input)
}
