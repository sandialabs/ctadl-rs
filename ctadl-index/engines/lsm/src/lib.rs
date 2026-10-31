//! The index engine on one thread, with its largest relations in LSM stores
//! (`Storage::Lsm`): `ctadl_index::index_engine::index_rules` under `ascent!`. Each engine is its
//! own crate so that the engines compile in parallel.

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
use ctadl_index::index_engine::dump::RelationDump;
use ctadl_index::index_engine::rules_prelude::*;
use ctadl_index::index_engine::{EngineInput, EngineStores, IndexResult, StoreSizes};

// `Storage::Lsm`: the same rules, with the store-backed relations redeclared onto the LSM
// provider (Ascent keeps a relation's last declaration). Each is `counting`, or `seeded` if it
// takes input rows, so none keeps its rows in its field; every one is read back, if at all,
// from its store. `locals`, `assign_like` and the relations keyed on them are lattices, which
// Ascent keeps in its own storage.
ascent! {
    #![measure_rule_times]
    #![generate_run_timeout]
    struct LsmIndexProg;
    include_source!(ctadl_index::index_engine::index_rules);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation locals_key(FunctionId, FlowVariable, Path, Path, Path);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation reach_vp(FunctionId, FlowVariable, Path);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation locals_key_wild(FunctionId, FlowVariable, Path, Path, Path);
    #[ds(lsm::ascent_provider::provider: seeded)]
    relation actual_param(PackedInsnSiteId, FormalIndex, FlowVertex);
    #[ds(lsm::ascent_provider::provider: seeded)]
    relation alias_of_formal(FunctionId, FlowVariable, FormalIndex);
    #[ds(lsm::ascent_provider::provider: seeded)]
    relation prog_store(FunctionId, FlowVariable, Path);
}

impl EngineStores for LsmIndexProg {
    fn store_sizes(&self) -> StoreSizes {
        StoreSizes {
            locals_key: self.locals_key.len(),
        }
    }
    fn dump_stores(&self, dump: &mut RelationDump<'_>) {
        macro_rules! stores {
            ($($rel:ident: $store:ident),* $(,)?) => {$(
                dump.relation(stringify!($rel), |w| self.$store.iter().for_each(|r| w.row(&r)));
            )*};
        }
        stores!(
            locals_key: __locals_key_ind_common,
            reach_vp: __reach_vp_ind_common,
            locals_key_wild: __locals_key_wild_ind_common,
            actual_param: __actual_param_ind_common,
            alias_of_formal: __alias_of_formal_ind_common,
            prog_store: __prog_store_ind_common,
        );
    }
}

/// Runs the index rules to fixpoint on the calling thread.
pub fn run(input: EngineInput<'_>) -> IndexResult {
    ctadl_index::run_engine!(LsmIndexProg, input)
}
