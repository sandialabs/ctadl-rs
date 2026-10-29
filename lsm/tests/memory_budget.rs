//! The process-wide memory budget: with it set, the write buffers of many maps
//! together stay near it, and every map still reads back what it was given.
//! In a test binary of its own, since the budget is shared by the process.

use std::collections::BTreeSet;

use ascent::ascent;

const MAPS: u64 = 20;
const BUDGET: usize = 1 << 20;
/// What `MmapLsmMultiMap` lets a map hold before the budget makes it flush.
const MIN_FLUSH: usize = 64 * 1024;

ascent! {
    #![ds(lsm::ascent_provider::provider)]
    struct LsmTc;
    relation edge(u64, u64);
    relation reach(u64, u64);
    reach(x, y) <-- edge(x, y);
    reach(x, z) <-- reach(x, y), edge(y, z);
}

ascent! {
    struct DefaultTc;
    relation edge(u64, u64);
    relation reach(u64, u64);
    reach(x, y) <-- edge(x, y);
    reach(x, z) <-- reach(x, y), edge(y, z);
}

#[test]
fn budget_bounds_the_write_buffers() {
    lsm::set_memory_budget(Some(BUDGET));

    // Many maps, none near its own entry limit, filled in turn.
    let mut maps: Vec<_> = (0..MAPS)
        .map(|_| lsm::MmapLsmMultiMap::<[u64; 2], u64>::with_memtable_limit(usize::MAX))
        .collect();
    for i in 0..400_000u64 {
        let m = i % MAPS;
        maps[m as usize].insert([i / MAPS % 5_000, m], i);
    }
    let bound = BUDGET + MAPS as usize * MIN_FLUSH;
    assert!(
        lsm::memory_peak() <= bound,
        "write buffers peaked at {} bytes, over {bound}",
        lsm::memory_peak()
    );
    for (m, map) in maps.iter().enumerate() {
        let got: usize = map.iter_all_refs().map(|(_, vals)| vals.count()).sum();
        assert_eq!(got as u64, 400_000 / MAPS, "map {m}");
    }
    drop(maps);
    assert_eq!(lsm::memory_in_use(), 0);

    // An Ascent program under a budget small enough to make it spill.
    lsm::set_memory_budget(Some(MIN_FLUSH));
    let edges: Vec<(u64, u64)> = (0..300)
        .flat_map(|i| [(i, (i * 7 + 1) % 300), (i, (i + 1) % 300)])
        .collect();
    let mut on_lsm = LsmTc {
        edge: edges.clone(),
        ..Default::default()
    };
    on_lsm.run();
    let mut default = DefaultTc {
        edge: edges,
        ..Default::default()
    };
    default.run();
    let a: BTreeSet<_> = on_lsm.reach.iter().copied().collect();
    let b: BTreeSet<_> = default.reach.iter().copied().collect();
    assert_eq!(a, b);
    lsm::set_memory_budget(None);
}
