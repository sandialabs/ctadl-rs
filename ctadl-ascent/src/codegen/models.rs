/*! Codegen for models

Take encoded models and codegen them into the index facts.

The expansion itself lives in [`crate::codegen::model_matches`], which is phase 2 of codegen.
This module is the adapter for callers that hold a columnar [`SummaryBatch`] rather than the
native match structure -- notably the flowy driver, which indexes a single program in one shot
and has no import loop to run a second phase after.
*/

use crate::index_engine::IndexFacts;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::models::{ProgramModelMatches, SummaryBatch};

/// Take a batch of summaries and codegen them into the facts.
///
/// `Argument(*)` expands over `compute_arg_arity` at the moment this runs, so a caller that
/// indexes several imports should collect matches into a [`ProgramModelMatches`] and run phase
/// 2 once at the end instead: a per-import expansion under-counts a function whose call sites
/// span imports.
pub fn codegen_summary(
    batch: SummaryBatch,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    let mut matches = ProgramModelMatches::default();
    matches.extend_from_summaries(&batch);
    crate::codegen::model_matches::codegen_model_matches(&matches, &[], facts, source_info)
        // With no bridge specs there is nothing that can raise the model errors phase 2
        // reports: every one of them is about pairing two bridge sides.
        .expect("summary-only codegen raises no model errors");
}
