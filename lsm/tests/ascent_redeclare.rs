//! Moving relations of a shared `ascent_source!` onto the LSM provider by
//! redeclaring them after `include_source!` (Ascent keeps the last declaration of
//! a relation). Covers the `counting` physical relation, whose rows are read back
//! from the relation's store, and a pair-valued column.

use std::collections::BTreeSet;

use ascent::{ascent, ascent_source};

ascent_source! {
    reach_rules:
    relation edge(u32, u32);
    relation reach(u32, u32);
    relation reach_via(u32, (u64, u64));

    reach(x, y) <-- edge(x, y);
    reach(x, z) <-- reach(x, y), edge(y, z);
    reach_via(x, (u64::from(*y), u64::from(*z) << 40)) <-- reach(x, y), edge(y, z);
}

ascent! {
    struct DefaultProg;
    include_source!(reach_rules);
}

ascent! {
    struct LsmProg;
    include_source!(reach_rules);
    #[ds(lsm::ascent_provider::provider: counting)]
    relation reach(u32, u32);
    #[ds(lsm::ascent_provider::provider)]
    relation reach_via(u32, (u64, u64));
}

fn edges() -> Vec<(u32, u32)> {
    let mut edges: Vec<_> = (0..200).map(|i| (i, (i * 7 + 3) % 200)).collect();
    edges.extend((0..200).map(|i| (i, (i * 13 + 1) % 200)));
    edges
}

#[test]
fn redeclared_relations_match_default() {
    lsm::ascent_provider::set_memtable_limit(64);

    let mut def = DefaultProg {
        edge: edges(),
        ..Default::default()
    };
    def.run();
    let mut lsm = LsmProg {
        edge: edges(),
        ..Default::default()
    };
    lsm.run();

    let def_reach: BTreeSet<_> = def.reach.iter().copied().collect();
    let lsm_reach: BTreeSet<_> = lsm
        .__reach_ind_common
        .iter()
        .map(|(x, y)| (*x, *y))
        .collect();
    assert_eq!(lsm.reach.len(), def.reach.len());
    assert_eq!(lsm_reach, def_reach);

    let def_via: BTreeSet<_> = def.reach_via.iter().copied().collect();
    let lsm_via: BTreeSet<_> = lsm.reach_via.iter().copied().collect();
    assert_eq!(lsm_via, def_via);
}
