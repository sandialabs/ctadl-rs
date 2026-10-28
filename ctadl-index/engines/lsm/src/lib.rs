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
use ctadl_index::index_engine::{
    AssignLikeRow, EngineInput, EngineStores, IndexResult, LocalsProbe, StoreSizes,
};

// `Storage::Lsm`: the same rules, with the store-backed relations redeclared onto the LSM
// provider (Ascent keeps a relation's last declaration). Each is `counting`, or `seeded` if it
// takes input rows, so none keeps its rows in its field; every one is read back, if at all,
// from its store.
ascent! {
    #![measure_rule_times]
    #![generate_run_timeout]
    struct LsmIndexProg;
    include_source!(ctadl_index::index_engine::index_rules);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation locals(FunctionId, FlowVariable, Path, FormalIndex, Path);
    #[ds(lsm::ascent_provider::provider: seeded)]
    relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation locals_key(FunctionId, FlowVariable, Path, Path, Path);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation edge_split(FunctionId, FlowVariable, Path, Path, (FlowVariable, Path));
    #[ds(lsm::ascent_provider::provider: counting)]
    relation ext_dst(FunctionId, FlowVariable, Path, FlowVariable, Path);
    // The next largest: the derived relations that reach hundreds of thousands of rows,
    // and the inputs that do. The rest stay in memory, where a relation of a few thousand
    // rows costs less than the LSM's per-index maps.
    #[ds(lsm::ascent_provider::provider: counting)]
    relation reach_vp(FunctionId, FlowVariable, Path);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation edge_split_wild(FunctionId, FlowVariable, Path, Path, FlowVariable, Path);
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
    fn seed_assign_like(&mut self, mut rows: Vec<AssignLikeRow>) {
        // Ascent returns a seeded field as given, duplicates included, where the trie's
        // `FromRows` keeps one of each.
        rows.sort_unstable();
        rows.dedup();
        self.assign_like = rows.into();
    }
    fn take_assign_like(&mut self) -> Vec<AssignLikeRow> {
        let rows = self
            .__assign_like_ind_common
            .iter()
            .map(|(f, vd, pd, vs, ps)| (*f, *vd, *pd, *vs, *ps))
            .collect();
        self.__assign_like_ind_common = Default::default();
        rows
    }
    fn store_sizes(&self) -> StoreSizes {
        StoreSizes {
            assign_like: self.assign_like.len(),
            locals: self.locals.len(),
            edge_split: self.edge_split.len(),
            locals_key: self.locals_key.len(),
            ext_dst: self.ext_dst.len(),
        }
    }
    fn heap_reports(&self) -> Vec<String> {
        Vec::new()
    }
    fn num_reached_variables(&self) -> usize {
        let mut reached = hashbrown::HashSet::new();
        for (f, v, ..) in self.__locals_ind_common.iter() {
            reached.insert((*f, *v));
        }
        reached.len()
    }
    fn locals_probe(&self) -> &dyn LocalsProbe {
        &self.__locals_ind_common
    }
    fn dump_stores(&self, dump: &mut RelationDump<'_>) {
        macro_rules! stores {
            ($($rel:ident: $store:ident),* $(,)?) => {$(
                dump.relation(stringify!($rel), |w| self.$store.iter().for_each(|r| w.row(&r)));
            )*};
        }
        stores!(
            locals: __locals_ind_common,
            assign_like: __assign_like_ind_common,
            locals_key: __locals_key_ind_common,
            edge_split: __edge_split_ind_common,
            ext_dst: __ext_dst_ind_common,
            reach_vp: __reach_vp_ind_common,
            edge_split_wild: __edge_split_wild_ind_common,
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
