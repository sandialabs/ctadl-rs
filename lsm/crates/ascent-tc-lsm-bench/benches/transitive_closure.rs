use std::hint::black_box;
use std::time::Duration;

use ascent_tc_lsm_bench::{
    chain_edges, run_transitive_closure, run_transitive_closure_default, set_memtable_limit,
};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const NODES: u64 = 100;

/// LSM write-buffer size for this benchmark. Owned here (not by the provider's
/// implementation default) so the benchmark's behaviour is stable regardless of
/// what the implementation picks as its default.
///
/// Deliberately small so the benchmark actually spills to disk and exercises the
/// memory-mapped read path (flushes + compaction + `mmap` reads), rather than
/// keeping everything in the in-RAM memtable.
///
/// This trades speed for bounded memory and is *slower* here on purpose: the
/// working set fits in RAM, so spilling is pure overhead. Smaller limits spill
/// more (more flushes and compactions) and run slower; a limit above the total
/// fact count keeps everything in the memtable and runs fastest. The point is
/// correctness, zero-copy reads, and *bounded* peak memory under compaction
/// (see the `memory_comparison` example), not beating RAM.
const MEMTABLE_LIMIT: usize = 64;

/// Best-effort sweep of provider run files left in the temp dir. A run file is
/// unlinked as soon as it is mapped, but one whose writer was interrupted (a
/// killed benchmark) is left behind. Sweeping at startup keeps them from
/// accumulating across benchmark invocations. (Assumes benchmarks aren't run
/// concurrently, which holds for this repo.)
fn sweep_leftover_run_files() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("lsm-mmap-") && name.ends_with(".run") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn bench_transitive_closure(c: &mut Criterion) {
    sweep_leftover_run_files();
    set_memtable_limit(MEMTABLE_LIMIT);

    let edges = chain_edges(NODES);
    let mut group = c.benchmark_group("ascent_disk_lsm_transitive_closure");
    group.throughput(Throughput::Elements(edges.len() as u64));

    // Disk-backed LSM relation provider.
    group.bench_function("chain_graph", |b| {
        b.iter(|| run_transitive_closure(black_box(edges.clone())));
    });

    group.bench_function("chain_graph_default", |b| {
        b.iter(|| run_transitive_closure_default(black_box(edges.clone())));
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(17))
        .measurement_time(Duration::from_secs(6));
    targets = bench_transitive_closure
}
criterion_main!(benches);
