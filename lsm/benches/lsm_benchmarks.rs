use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use lsm::DiskLsmMultiMap;

const ENTRIES: usize = 10_000;
const KEY_SPACE: u64 = 1_024;
const MEMTABLE_LIMIT: usize = 256;
const LOOKUPS: usize = 20_000;

static NEXT_BENCH_DIR_ID: AtomicU64 = AtomicU64::new(0);

type Key = (u64,);
type Value = (u64, u64);

fn key_for(i: usize) -> Key {
    (((i as u64).wrapping_mul(37)) % KEY_SPACE,)
}

fn value_for(i: usize) -> Value {
    (i as u64, (i as u64).wrapping_mul(17))
}

fn bench_dir(label: &str) -> PathBuf {
    let id = NEXT_BENCH_DIR_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("lsm-bench-{label}-{}-{id}", std::process::id()))
}

fn populate_disk(entries: usize, dir: PathBuf) -> DiskLsmMultiMap<Key, Value> {
    let mut map = DiskLsmMultiMap::open(dir, MEMTABLE_LIMIT).unwrap();
    for i in 0..entries {
        map.insert(key_for(i), value_for(i));
    }
    map.flush().unwrap();
    map
}

fn lookup_keys(count: usize) -> Vec<Key> {
    (0..count).map(|i| key_for(i * 13)).collect()
}

fn bench_disk(c: &mut Criterion) {
    let mut group = c.benchmark_group("disk_lsm");
    group.throughput(Throughput::Elements(ENTRIES as u64));

    group.bench_function("insert", |b| {
        b.iter_batched(
            || bench_dir("insert"),
            |dir| {
                let mut map = DiskLsmMultiMap::open(&dir, MEMTABLE_LIMIT).unwrap();
                for i in 0..ENTRIES {
                    map.insert(black_box(key_for(i)), black_box(value_for(i)));
                }
                map.flush().unwrap();
                let len = map.len();
                drop(map);
                fs::remove_dir_all(&dir).unwrap();
                black_box(len)
            },
            BatchSize::SmallInput,
        );
    });

    let lookup_dir = bench_dir("lookup");
    let map = populate_disk(ENTRIES, lookup_dir.clone());
    let keys = lookup_keys(LOOKUPS);
    group.throughput(Throughput::Elements(LOOKUPS as u64));

    group.bench_function("point_lookup", |b| {
        b.iter(|| {
            let mut values_seen = 0;
            for key in keys.iter() {
                values_seen += black_box(map.get(black_box(key)).unwrap()).len();
            }
            black_box(values_seen)
        });
    });

    group.bench_function("contains_key", |b| {
        b.iter(|| {
            let mut found = 0;
            for key in keys.iter() {
                found += usize::from(black_box(map.contains_key(black_box(key))));
            }
            black_box(found)
        });
    });

    group.bench_function("iter_all_owned", |b| {
        b.iter(|| {
            let entries = black_box(map.iter_all_owned().unwrap());
            black_box(
                entries
                    .into_iter()
                    .map(|(_, values)| values.len())
                    .sum::<usize>(),
            )
        });
    });

    drop(map);
    fs::remove_dir_all(&lookup_dir).unwrap();
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    targets = bench_disk
}
criterion_main!(benches);
