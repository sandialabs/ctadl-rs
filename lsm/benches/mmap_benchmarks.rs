//! Benchmarks of the memory-mapped LSM map and of the n-ary Ascent provider built on it, at a
//! write-buffer size small enough that every structure spills into many runs.

use std::hint::black_box;
use std::time::Duration;

use ascent::ascent;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use lsm::MmapLsmMultiMap;

const ENTRIES: u64 = 200_000;
const KEY_SPACE: u64 = 50_000;
const MEMTABLE_LIMIT: usize = 4_096;

fn key_for(i: u64) -> u64 {
    i.wrapping_mul(0x9E37_79B9_7F4A_7C15) % KEY_SPACE
}

fn populate() -> MmapLsmMultiMap<u64, u64> {
    let mut map = MmapLsmMultiMap::with_memtable_limit(MEMTABLE_LIMIT);
    for i in 0..ENTRIES {
        map.insert(key_for(i), i);
    }
    map
}

fn bench_map(c: &mut Criterion) {
    let mut group = c.benchmark_group("mmap_lsm");
    group.throughput(Throughput::Elements(ENTRIES));
    group.bench_function("insert", |b| {
        b.iter_batched(
            || (),
            |()| black_box(populate().len()),
            BatchSize::SmallInput,
        );
    });

    let map = populate();
    group.bench_function("get_refs_hit", |b| {
        b.iter(|| {
            let mut n = 0;
            for i in 0..ENTRIES {
                n += map.get_refs(&key_for(i)).map_or(0, Iterator::count);
            }
            black_box(n)
        });
    });
    group.bench_function("get_refs_miss", |b| {
        b.iter(|| {
            let mut n = 0;
            for i in 0..ENTRIES {
                n += usize::from(map.get_refs(&(KEY_SPACE + i)).is_some());
            }
            black_box(n)
        });
    });
    group.bench_function("iter_all", |b| {
        b.iter(|| {
            black_box(
                map.iter_all_refs()
                    .map(|(_, vals)| vals.count())
                    .sum::<usize>(),
            )
        });
    });
    group.finish();
}

// Transitive closure over `COMPONENTS` disjoint, strongly connected components, so the closure
// has COMPONENTS * NODES^2 rows and many semi-naive iterations.
const COMPONENTS: u64 = 40;
const NODES: u64 = 60;

fn graph() -> Vec<(u64, u64, u64)> {
    let mut edges = Vec::new();
    for c in 0..COMPONENTS {
        for n in 0..NODES {
            let node = |m: u64| c * NODES + m % NODES;
            edges.push((c, node(n), node(n + 1)));
            edges.push((c, node(n), node(n * 7 + 3)));
        }
    }
    edges
}

ascent! {
    #![ds(lsm::ascent_provider::provider)]
    struct LsmTc;
    relation edge(u64, u64, u64);
    relation reach(u64, u64, u64);
    reach(c, x, y) <-- edge(c, x, y);
    reach(c, x, z) <-- reach(c, x, y), edge(c, y, z);
}

ascent! {
    struct DefaultTc;
    relation edge(u64, u64, u64);
    relation reach(u64, u64, u64);
    reach(c, x, y) <-- edge(c, x, y);
    reach(c, x, z) <-- reach(c, x, y), edge(c, y, z);
}

fn bench_provider(c: &mut Criterion) {
    lsm::ascent_provider::set_memtable_limit(MEMTABLE_LIMIT);
    let edges = graph();
    let mut group = c.benchmark_group("provider_tc");
    group.throughput(Throughput::Elements(COMPONENTS * NODES * NODES));
    group.bench_function("lsm", |b| {
        b.iter(|| {
            let mut prog = LsmTc {
                edge: edges.clone(),
                ..Default::default()
            };
            prog.run();
            black_box(prog.reach.len())
        });
    });
    group.bench_function("default", |b| {
        b.iter(|| {
            let mut prog = DefaultTc {
                edge: edges.clone(),
                ..Default::default()
            };
            prog.run();
            black_box(prog.reach.len())
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3));
    targets = bench_map, bench_provider
}
criterion_main!(benches);
