//! End-to-end test of the n-ary mmap-LSM Ascent `ds` provider.
//!
//! Runs the *same* program under (a) the LSM provider and (b) Ascent's default
//! provider, and asserts the computed relations are identical. The program uses
//! arity-3 and arity-5 relations and a 16-byte column (two `u64` slots) placed in
//! several positions, exercising the tuple encode/decode path for `Slots` wider
//! than one `u64` and for multiple index patterns.

use std::collections::BTreeSet;

use ascent::ascent;
use lsm::lsm_col;

/// A 16-byte column: forces a 2-slot `LsmCol` encoding (size > 8 bytes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
struct Lab(u64, u64);
lsm_col!(Lab, 2);

// ---- LSM-backed program -------------------------------------------------------
ascent! {
    #![ds(lsm::ascent_provider::provider)]
    struct LsmProg;

    relation edge(u32, u32, Lab);
    relation reach(u32, u32, Lab);
    // arity-5 relation with two 16-byte columns in non-adjacent positions
    relation quint(u32, Lab, u32, Lab, u32);

    reach(x, y, l) <-- edge(x, y, l);
    reach(x, z, l) <-- reach(x, y, l), edge(y, z, _l2);

    quint(a, *l1, b, *l2, c) <-- edge(a, b, l1), edge(b, c, l2);
}

// ---- Default-backed baseline (identical rules) --------------------------------
ascent! {
    struct DefaultProg;

    relation edge(u32, u32, Lab);
    relation reach(u32, u32, Lab);
    relation quint(u32, Lab, u32, Lab, u32);

    reach(x, y, l) <-- edge(x, y, l);
    reach(x, z, l) <-- reach(x, y, l), edge(y, z, _l2);

    quint(a, *l1, b, *l2, c) <-- edge(a, b, l1), edge(b, c, l2);
}

fn sample_edges() -> Vec<(u32, u32, Lab)> {
    vec![
        (0, 1, Lab(10, 100)),
        (1, 2, Lab(11, 101)),
        (2, 3, Lab(12, 102)),
        (3, 0, Lab(13, 103)), // cycle, to force fixpoint iteration
        (1, 4, Lab(14, 104)),
        (4, 2, Lab(15, 105)),
    ]
}

#[test]
fn lsm_provider_matches_default() {
    let edges = sample_edges();

    let mut lsm = LsmProg::default();
    lsm.edge = edges.clone();
    lsm.run();

    let mut def = DefaultProg::default();
    def.edge = edges;
    def.run();

    let lsm_reach: BTreeSet<_> = lsm.reach.iter().cloned().collect();
    let def_reach: BTreeSet<_> = def.reach.iter().cloned().collect();
    assert_eq!(lsm_reach, def_reach, "reach (arity 3) differs");

    let lsm_quint: BTreeSet<_> = lsm.quint.iter().cloned().collect();
    let def_quint: BTreeSet<_> = def.quint.iter().cloned().collect();
    assert_eq!(lsm_quint, def_quint, "quint (arity 5) differs");

    // Sanity: the program actually computed something non-trivial.
    assert!(!def_reach.is_empty());
    assert!(!def_quint.is_empty());
    // Every node reaches every node (the edges form one strongly-connected
    // component over {0,1,2,3} plus 4 which joins it), so reach has many pairs.
    assert!(lsm_reach.len() >= def.edge.len());
}

#[test]
fn tiny_memtable_limit_still_correct() {
    use lsm::ascent_provider::{DEFAULT_MEMTABLE_LIMIT, memtable_limit, set_memtable_limit};

    // With nothing set, the limit is the crate's plain default.
    assert_eq!(memtable_limit(), DEFAULT_MEMTABLE_LIMIT);

    // A tiny per-map limit drives the run-file read/merge paths much harder.
    // Results must be identical regardless of the limit.
    set_memtable_limit(1);
    assert_eq!(memtable_limit(), 1);

    let edges = sample_edges();

    let mut lsm = LsmProg::default();
    lsm.edge = edges.clone();
    lsm.run();

    let mut def = DefaultProg::default();
    def.edge = edges;
    def.run();

    let lsm_reach: BTreeSet<_> = lsm.reach.iter().cloned().collect();
    let def_reach: BTreeSet<_> = def.reach.iter().cloned().collect();
    assert_eq!(lsm_reach, def_reach, "reach differs under tiny memtable limit");

    let lsm_quint: BTreeSet<_> = lsm.quint.iter().cloned().collect();
    let def_quint: BTreeSet<_> = def.quint.iter().cloned().collect();
    assert_eq!(lsm_quint, def_quint, "quint differs under tiny memtable limit");

    set_memtable_limit(DEFAULT_MEMTABLE_LIMIT);
}

#[test]
fn lsm_provider_dedups() {
    // Duplicate input edges must not produce duplicate tuples.
    let mut edges = sample_edges();
    edges.extend(sample_edges());

    let mut lsm = LsmProg::default();
    lsm.edge = edges;
    lsm.run();

    let unique: BTreeSet<_> = lsm.reach.iter().cloned().collect();
    assert_eq!(unique.len(), lsm.reach.len(), "reach contains duplicates");
}
