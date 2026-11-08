//! End-to-end test of the n-ary mmap-LSM Ascent `ds` provider.
//!
//! Runs the *same* program under (a) the LSM provider and (b) Ascent's default
//! provider, and asserts the computed relations are identical. The program uses
//! arity-3 and arity-5 relations and a 16-byte column placed in several
//! positions, exercising the tuple encode/decode path for columns wider than one
//! `u64` and for multiple index patterns. `mix` mixes 1-, 2-, 4-, 8- and 16-byte
//! columns and is read by prefixes of one, two and three columns and by an index
//! of its own on non-prefix columns.

use std::collections::BTreeSet;

use ascent::ascent;
use lsm::lsm_col;

/// A 16-byte column, wider than a word.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
struct Lab(u64, u64);
lsm_col!(Lab, [u64, u64]);

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

    relation mix(u8, u32, u16, Lab, u64);
    relation by_a(u8, u64, u64);
    relation by_ab(u8, u32, u16);
    relation by_abc(Lab, Lab);
    relation by_be(u8, u8);

    mix(a, *b2, c, l, e) <-- mix(a, b, c, l, e), edge(b, b2, _);
    // Probes `mix` by its first one, two and three columns.
    by_a(a, e, e2) <-- mix(a, _, _, _, e), mix(a, _, _, _, e2);
    by_ab(a, b, c2) <-- mix(a, b, _, _, _), mix(a, b, c2, _, _);
    by_abc(l, l2) <-- mix(a, b, c, l, _), mix(a, b, c, l2, _);
    // Probes `mix` by columns 1 and 4, an index of its own.
    by_be(a, a2) <-- mix(a, b, _, _, e), mix(a2, b, _, _, e);
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

    relation mix(u8, u32, u16, Lab, u64);
    relation by_a(u8, u64, u64);
    relation by_ab(u8, u32, u16);
    relation by_abc(Lab, Lab);
    relation by_be(u8, u8);

    mix(a, *b2, c, l, e) <-- mix(a, b, c, l, e), edge(b, b2, _);
    // Probes `mix` by its first one, two and three columns.
    by_a(a, e, e2) <-- mix(a, _, _, _, e), mix(a, _, _, _, e2);
    by_ab(a, b, c2) <-- mix(a, b, _, _, _), mix(a, b, c2, _, _);
    by_abc(l, l2) <-- mix(a, b, c, l, _), mix(a, b, c, l2, _);
    // Probes `mix` by columns 1 and 4, an index of its own.
    by_be(a, a2) <-- mix(a, b, _, _, e), mix(a2, b, _, _, e);
}

type Mix = (u8, u32, u16, Lab, u64);

fn sample_mix() -> Vec<Mix> {
    (0..40u64)
        .map(|i| {
            let lab = Lab(i % 2, i);
            (
                (i % 3) as u8,
                (i % 5) as u32,
                (i % 4) as u16,
                lab,
                (i * 7) % 11,
            )
        })
        .collect()
}

/// Serializes the tests that change the process-wide memtable limit.
static LIMIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    let mut lsm = LsmProg {
        edge: edges.clone(),
        ..Default::default()
    };
    lsm.run();

    let mut def = DefaultProg {
        edge: edges,
        ..Default::default()
    };
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

    let _limit = LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    // With nothing set, the limit is the crate's plain default.
    assert_eq!(memtable_limit(), DEFAULT_MEMTABLE_LIMIT);

    // A tiny per-map limit drives the run-file read/merge paths much harder.
    // Results must be identical regardless of the limit.
    set_memtable_limit(1);
    assert_eq!(memtable_limit(), 1);

    let edges = sample_edges();

    let mut lsm = LsmProg {
        edge: edges.clone(),
        ..Default::default()
    };
    lsm.run();

    let mut def = DefaultProg {
        edge: edges,
        ..Default::default()
    };
    def.run();

    let lsm_reach: BTreeSet<_> = lsm.reach.iter().cloned().collect();
    let def_reach: BTreeSet<_> = def.reach.iter().cloned().collect();
    assert_eq!(
        lsm_reach, def_reach,
        "reach differs under tiny memtable limit"
    );

    let lsm_quint: BTreeSet<_> = lsm.quint.iter().cloned().collect();
    let def_quint: BTreeSet<_> = def.quint.iter().cloned().collect();
    assert_eq!(
        lsm_quint, def_quint,
        "quint differs under tiny memtable limit"
    );

    set_memtable_limit(DEFAULT_MEMTABLE_LIMIT);
}

#[test]
fn lsm_provider_dedups() {
    // Duplicate input edges must not produce duplicate tuples.
    let mut edges = sample_edges();
    edges.extend(sample_edges());

    let mut lsm = LsmProg {
        edge: edges,
        ..Default::default()
    };
    lsm.run();

    let unique: BTreeSet<_> = lsm.reach.iter().cloned().collect();
    assert_eq!(unique.len(), lsm.reach.len(), "reach contains duplicates");
}

/// Checks the `mix` relations against the default provider at the default
/// memtable limit, and at 4, so that reads cross the memtable and many runs.
#[test]
fn mixed_widths_match_default() {
    use lsm::ascent_provider::{DEFAULT_MEMTABLE_LIMIT, set_memtable_limit};

    let _limit = LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    for limit in [DEFAULT_MEMTABLE_LIMIT, 4] {
        set_memtable_limit(limit);
        let mut lsm = LsmProg {
            edge: sample_edges(),
            mix: sample_mix(),
            ..Default::default()
        };
        lsm.run();
        let mut def = DefaultProg {
            edge: sample_edges(),
            mix: sample_mix(),
            ..Default::default()
        };
        def.run();

        fn set<T: Ord + Clone>(rows: &[T]) -> BTreeSet<T> {
            rows.iter().cloned().collect()
        }
        assert_eq!(set(&lsm.mix), set(&def.mix), "mix, limit {limit}");
        assert_eq!(set(&lsm.by_a), set(&def.by_a), "by_a, limit {limit}");
        assert_eq!(set(&lsm.by_ab), set(&def.by_ab), "by_ab, limit {limit}");
        assert_eq!(set(&lsm.by_abc), set(&def.by_abc), "by_abc, limit {limit}");
        assert_eq!(set(&lsm.by_be), set(&def.by_be), "by_be, limit {limit}");
        assert!(def.mix.len() > sample_mix().len());
        assert!(!def.by_abc.is_empty() && !def.by_be.is_empty());
    }
    set_memtable_limit(DEFAULT_MEMTABLE_LIMIT);
}
