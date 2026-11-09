//! Property tests of `MmapLsmMultiMap` against a `BTreeMap` model: random
//! sequences of inserts, flushes, clears, absorbs and reopens, under random
//! memtable limits, level fanouts and prefix filters, with every read checked
//! after every step.
//!
//! Keys come from a small space so that keys, prefixes and values repeat, and
//! limits are small so that a few dozen inserts flush and merge many runs.
//!
//! Each test runs 32 cases unless `PROPTEST_CASES` says otherwise; the
//! `lsm-property-tests` flake check runs many more each night.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bytemuck::Pod;
use lsm::MmapLsmMultiMap;
use proptest::prelude::*;

type Key = [u64; 3];
type Model<V> = BTreeMap<Key, Vec<V>>;

/// The key space inserts draw from.
fn key() -> impl Strategy<Value = Key> {
    (0..3u64, 0..3u64, 0..6u64).prop_map(|(a, b, c)| [a, b, c])
}

/// Every key inserts can draw, and keys one past each word's range, which are
/// never present.
fn probes() -> impl Iterator<Item = Key> {
    (0..4u64).flat_map(|a| (0..4u64).flat_map(move |b| (0..7u64).map(move |c| [a, b, c])))
}

fn memtable_limit() -> impl Strategy<Value = usize> {
    1..=6usize
}

fn level_fanout() -> impl Strategy<Value = usize> {
    1..=3usize
}

/// The key prefix lengths, in bytes, a map keeps filters and chains for.
fn prefix_lens() -> impl Strategy<Value = Vec<usize>> {
    prop_oneof![
        Just(vec![]),
        Just(vec![8]),
        Just(vec![16]),
        Just(vec![8, 16]),
        Just(vec![8, 16, 24]),
    ]
}

/// A value type under test.
trait Value: Pod + Ord + Debug {
    fn strategy() -> BoxedStrategy<Self>;
    /// A value no insert uses, if the type has one.
    fn absent() -> Option<Self>;
}

impl Value for u64 {
    fn strategy() -> BoxedStrategy<Self> {
        (0..4u64).boxed()
    }
    fn absent() -> Option<Self> {
        Some(99)
    }
}

impl Value for () {
    fn strategy() -> BoxedStrategy<Self> {
        Just(()).boxed()
    }
    fn absent() -> Option<Self> {
        None
    }
}

/// How a test inserts, which decides what it can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Any key and value, repeats included: every value is kept.
    Multimap,
    /// A set that only gets keys it does not hold, as the Ascent provider
    /// inserts.
    Set,
    /// A set that gets repeated keys. A run holds a key once, but `len` counts
    /// inserts and a key in several runs has a value from each, so only
    /// membership is checked.
    SetWithRepeats,
}

#[derive(Clone, Debug)]
enum Op<V> {
    Insert(Key, V),
    Flush,
    Clear,
    /// Absorbs a temporary map built from `entries`, with its own limits and
    /// prefix filters, so that some entries are in its memtable and some in
    /// runs at several levels.
    Absorb {
        entries: Vec<(Key, V)>,
        limit: usize,
        fanout: usize,
        prefix_lens: Vec<usize>,
    },
    /// Flushes, drops the map, and opens its directory again.
    Reopen {
        limit: usize,
        fanout: usize,
    },
}

fn insert<V: Value>() -> impl Strategy<Value = Op<V>> {
    (key(), V::strategy()).prop_map(|(k, v)| Op::Insert(k, v))
}

/// Operations on a temporary map: no reopen.
fn temp_ops<V: Value>() -> impl Strategy<Value = Vec<Op<V>>> {
    let op = prop_oneof![
        20 => insert::<V>(),
        2 => Just(Op::Flush),
        1 => Just(Op::Clear),
        2 => (
            prop::collection::vec((key(), V::strategy()), 0..30),
            memtable_limit(),
            level_fanout(),
            prefix_lens(),
        )
            .prop_map(|(entries, limit, fanout, prefix_lens)| Op::Absorb {
                entries,
                limit,
                fanout,
                prefix_lens,
            }),
    ];
    prop::collection::vec(op, 0..80)
}

/// Operations on a map in a directory: no absorb, since moved runs stay in the
/// directory of the map they came from.
fn persistent_ops<V: Value>() -> impl Strategy<Value = Vec<Op<V>>> {
    let op = prop_oneof![
        20 => insert::<V>(),
        2 => Just(Op::Flush),
        1 => Just(Op::Clear),
        3 => (memtable_limit(), level_fanout())
            .prop_map(|(limit, fanout)| Op::Reopen { limit, fanout }),
    ];
    prop::collection::vec(op, 0..80)
}

/// Whether `mode` lets `key` be inserted into a map whose model is `model`.
fn may_insert<V>(mode: Mode, model: &Model<V>, key: &Key) -> bool {
    mode != Mode::Set || !model.contains_key(key)
}

fn sorted<V: Ord>(vals: impl IntoIterator<Item = V>) -> Vec<V> {
    let mut vals: Vec<V> = vals.into_iter().collect();
    vals.sort_unstable();
    vals
}

/// Checks every read of `map` against `model`.
fn check<V: Value>(
    map: &MmapLsmMultiMap<Key, V>,
    model: &Model<V>,
    mode: Mode,
) -> Result<(), TestCaseError> {
    let exact = mode != Mode::SetWithRepeats;
    let keys: BTreeSet<Key> = model.keys().copied().collect();

    prop_assert_eq!(map.is_empty(), model.is_empty());
    if exact {
        prop_assert_eq!(map.len(), model.values().map(Vec::len).sum::<usize>());
    }

    for probe in probes() {
        let want = model.get(&probe);
        prop_assert_eq!(
            map.contains_key(&probe),
            want.is_some(),
            "contains_key {:?}",
            probe
        );
        let got = map.get_refs(&probe).map(|vals| sorted(vals.copied()));
        if exact {
            prop_assert_eq!(
                got,
                want.map(|vals| sorted(vals.iter().copied())),
                "get_refs {:?}",
                probe
            );
        } else {
            prop_assert_eq!(got.is_some(), want.is_some(), "get_refs {:?}", probe);
        }
        for value in want.into_iter().flatten() {
            prop_assert!(
                map.contains_value(&probe, value),
                "contains_value {:?} {:?}",
                probe,
                value
            );
        }
        if let Some(absent) = V::absent() {
            prop_assert!(!map.contains_value(&probe, &absent));
        }
    }

    // A scan yields each key once, in ascending order, with all its values.
    let all: Vec<(Key, Vec<V>)> = map
        .iter_all_refs()
        .map(|(k, vals)| (*k, sorted(vals.copied())))
        .collect();
    prop_assert!(all.windows(2).all(|w| w[0].0 < w[1].0), "scan out of order");
    if exact {
        let want: Vec<(Key, Vec<V>)> = model
            .iter()
            .map(|(k, vals)| (*k, sorted(vals.iter().copied())))
            .collect();
        prop_assert_eq!(all, want);
    } else {
        prop_assert!(all.iter().all(|(_, vals)| !vals.is_empty()));
        prop_assert_eq!(
            all.iter().map(|(k, _)| *k).collect::<BTreeSet<_>>(),
            keys.clone()
        );
    }

    // `keys` and `keys_with_prefix` yield a key once per source holding it; a
    // set that only gets absent keys has one source per key.
    let check_keys =
        |got: Vec<Key>, want: BTreeSet<Key>, what: &str| -> Result<(), TestCaseError> {
            let distinct: BTreeSet<Key> = got.iter().copied().collect();
            prop_assert_eq!(&distinct, &want, "{}", what);
            if mode == Mode::Set {
                prop_assert_eq!(got.len(), want.len(), "{} repeats a key", what);
            }
            Ok(())
        };
    check_keys(map.keys().copied().collect(), keys.clone(), "keys")?;
    let mut prefixes: Vec<Vec<u64>> = vec![vec![]];
    for a in 0..4u64 {
        prefixes.push(vec![a]);
        for b in 0..4u64 {
            prefixes.push(vec![a, b]);
        }
    }
    prefixes.extend(probes().map(|k| k.to_vec()));
    for prefix in &prefixes {
        let want = keys
            .iter()
            .filter(|k| k.starts_with(prefix))
            .copied()
            .collect();
        let got = map.keys_with_prefix(prefix).copied().collect();
        check_keys(got, want, &format!("keys_with_prefix {prefix:?}"))?;
    }

    // Every key lands in a group of its prefix, which holds the group's key.
    for len in 1..=3 {
        let mut grouped: Vec<Key> = Vec::new();
        for (first, group) in map.prefix_groups(len) {
            let group: Vec<Key> = group.copied().collect();
            prop_assert!(group.contains(first), "group of {:?} lacks it", first);
            for k in &group {
                prop_assert_eq!(&k[..len], &first[..len], "group by {} words", len);
            }
            grouped.extend(group);
        }
        check_keys(grouped, keys.clone(), &format!("prefix_groups({len})"))?;
    }
    Ok(())
}

fn run_temp<V: Value>(
    mode: Mode,
    limit: usize,
    fanout: usize,
    lens: &[usize],
    ops: Vec<Op<V>>,
) -> Result<(), TestCaseError> {
    let new_map = |limit, fanout, lens: &[usize]| {
        MmapLsmMultiMap::<Key, V>::with_memtable_limit_and_level_fanout(limit, fanout)
            .with_prefix_reads(lens)
    };
    let mut map = new_map(limit, fanout, lens);
    let mut model = Model::<V>::new();
    for op in ops {
        match op {
            Op::Insert(k, v) => {
                if may_insert(mode, &model, &k) {
                    map.insert(k, v);
                    model.entry(k).or_default().push(v);
                }
            }
            Op::Flush => map.flush().unwrap(),
            Op::Clear => {
                map.clear();
                model.clear();
            }
            Op::Absorb {
                entries,
                limit,
                fanout,
                prefix_lens,
            } => {
                let mut from = new_map(limit, fanout, &prefix_lens);
                let mut from_model = Model::<V>::new();
                for (k, v) in entries {
                    if may_insert(mode, &model, &k) && may_insert(mode, &from_model, &k) {
                        from.insert(k, v);
                        from_model.entry(k).or_default().push(v);
                    }
                }
                check(&from, &from_model, mode)?;
                map.absorb(&mut from);
                for (k, vals) in from_model {
                    model.entry(k).or_default().extend(vals);
                }
                check(&from, &Model::new(), mode)?;
            }
            Op::Reopen { .. } => unreachable!("a temporary map has no directory"),
        }
        check(&map, &model, mode)?;
    }
    Ok(())
}

/// A directory of its own for one test case, removed when the case ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lsm-props-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_persistent<V: Value>(
    mode: Mode,
    limit: usize,
    fanout: usize,
    lens: &[usize],
    ops: Vec<Op<V>>,
) -> Result<(), TestCaseError> {
    let dir = TempDir::new();
    let mut map = MmapLsmMultiMap::<Key, V>::open_with_level_fanout(&dir.0, limit, fanout)
        .unwrap()
        .with_prefix_reads(lens);
    let mut model = Model::<V>::new();
    for op in ops {
        match op {
            Op::Insert(k, v) => {
                if may_insert(mode, &model, &k) {
                    map.insert(k, v);
                    model.entry(k).or_default().push(v);
                }
            }
            Op::Flush => map.flush().unwrap(),
            Op::Clear => {
                map.clear();
                model.clear();
            }
            Op::Reopen { limit, fanout } => {
                map.flush().unwrap();
                drop(map);
                map = MmapLsmMultiMap::open_with_level_fanout(&dir.0, limit, fanout).unwrap();
            }
            Op::Absorb { .. } => unreachable!("persistent maps do not absorb"),
        }
        check(&map, &model, mode)?;
    }
    Ok(())
}

/// 32 cases, or as many as `PROPTEST_CASES` asks for.
fn config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
    }
    config
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn multimap_matches_model(
        limit in memtable_limit(),
        fanout in level_fanout(),
        lens in prefix_lens(),
        ops in temp_ops::<u64>(),
    ) {
        run_temp(Mode::Multimap, limit, fanout, &lens, ops)?;
    }

    #[test]
    fn set_matches_model(
        limit in memtable_limit(),
        fanout in level_fanout(),
        lens in prefix_lens(),
        ops in temp_ops::<()>(),
    ) {
        run_temp(Mode::Set, limit, fanout, &lens, ops)?;
    }

    #[test]
    fn set_with_repeats_matches_model(
        limit in memtable_limit(),
        fanout in level_fanout(),
        lens in prefix_lens(),
        ops in temp_ops::<()>(),
    ) {
        run_temp(Mode::SetWithRepeats, limit, fanout, &lens, ops)?;
    }

    #[test]
    fn persistent_multimap_matches_model(
        limit in memtable_limit(),
        fanout in level_fanout(),
        lens in prefix_lens(),
        ops in persistent_ops::<u64>(),
    ) {
        run_persistent(Mode::Multimap, limit, fanout, &lens, ops)?;
    }

    #[test]
    fn persistent_set_matches_model(
        limit in memtable_limit(),
        fanout in level_fanout(),
        lens in prefix_lens(),
        ops in persistent_ops::<()>(),
    ) {
        run_persistent(Mode::Set, limit, fanout, &lens, ops)?;
    }
}
