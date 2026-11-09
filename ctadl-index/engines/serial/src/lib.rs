//! The index engine on one thread, with its large relations in the serial tries:
//! `ctadl_index::index_engine::index_rules` under `ascent!`. Each engine is its own crate so that
//! the engines compile in parallel. It also runs the intent rules alone ([`run_intent`]).

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
use ctadl_index::index_engine::{EngineInput, IndexResult, IntentInput, IntentOutput};

ascent! {
    #![measure_rule_times]
    #![generate_run_timeout]
    struct IndexProg;
    include_source!(ctadl_index::index_engine::intent_rules);
    include_source!(ctadl_index::index_engine::index_rules);
}
ctadl_index::trie_stores!(IndexProg);
ctadl_index::trie_relations!(IndexProg);

/// Runs the index rules to fixpoint on the calling thread.
pub fn run(input: EngineInput<'_>) -> IndexResult {
    ctadl_index::run_engine!(IndexProg, input)
}

ascent! {
    struct IntentProg;
    include_source!(ctadl_index::index_engine::intent_rules);
}

/// Runs the intent rules alone to fixpoint, on flow that the index rules have already closed.
pub fn run_intent(input: IntentInput) -> IntentOutput {
    let IntentInput {
        assign_like,
        const_str_assign,
        intent_frame,
        intent_send,
        intent_filter,
        intent_component,
        path_set,
    } = input;
    let mut prog = IntentProg::default();
    // Stored source first, as `index_rules` stores it.
    prog.assign_like = assign_like
        .into_iter()
        .map(|(f, v1, p1, v2, p2)| (f, v2, p2, v1, p1))
        .collect();
    prog.const_str_assign = const_str_assign.into_iter().collect();
    prog.intent_frame = intent_frame.into_iter().collect();
    prog.intent_send = intent_send.into_iter().collect();
    prog.intent_filter = intent_filter.into_iter().collect();
    prog.intent_component = intent_component.into_iter().collect();
    prog.path_set = vec![(path_set,)].into_iter().collect();
    prog.run();
    IntentOutput {
        intent_pair: prog.intent_pair.into_iter().collect(),
        resolved_call: prog.resolved_call.into_iter().collect(),
    }
}
