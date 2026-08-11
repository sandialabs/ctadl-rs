use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use trie::Trie;

fn bench_insert_100k(c: &mut Criterion) {
    let keys: Vec<String> = (0..100_000).map(|i| format!("key{:05}", i)).collect();

    c.bench_function("insert_100k", |b| {
        b.iter(|| {
            let mut t = Trie::new();
            for key in &keys {
                t.insert(key.chars(), 1);
            }
            black_box(t);
        })
    });
}

fn bench_get_100k(c: &mut Criterion) {
    let keys: Vec<String> = (0..100_000).map(|i| format!("key{:05}", i)).collect();
    let mut t = Trie::new();
    for key in &keys {
        t.insert(key.chars(), 1);
    }

    c.bench_function("get_100k", |b| {
        b.iter(|| {
            for key in &keys {
                black_box(t.get(key.chars()));
            }
        })
    });
}

fn bench_union_10k(c: &mut Criterion) {
    let keys1: Vec<String> = (0..10_000).map(|i| format!("key_a_{:04}", i)).collect();
    let keys2: Vec<String> = (0..10_000).map(|i| format!("key_b_{:04}", i)).collect();

    let mut t1 = Trie::new();
    for key in &keys1 {
        t1.insert(key.chars(), 1);
    }

    let mut t2 = Trie::new();
    for key in &keys2 {
        t2.insert(key.chars(), 2);
    }

    c.bench_function("union_10k", |b| {
        b.iter(|| {
            let mut t = t1;
            t.union(&t2);
            black_box(t);
        })
    });
}

fn bench_is_subset_10k(c: &mut Criterion) {
    let keys1: Vec<String> = (0..10_000).map(|i| format!("key_{:05}", i)).collect();
    let keys2: Vec<String> = (0..15_000).map(|i| format!("key_{:05}", i)).collect();

    let mut t1 = Trie::new();
    for key in &keys1 {
        t1.insert(key.chars(), 1);
    }

    let mut t2 = Trie::new();
    for key in &keys2 {
        t2.insert(key.chars(), 1);
    }

    c.bench_function("is_subset_10k_true", |b| {
        b.iter(|| {
            black_box(t1.is_subset(&t2));
        })
    });

    let mut t3 = Trie::new();
    for i in 0..10_000 {
        t3.insert(format!("other_{:05}", i).chars(), 1);
    }

    c.bench_function("is_subset_10k_false", |b| {
        b.iter(|| {
            black_box(t1.is_subset(&t3));
        })
    });
}

fn bench_intersection_10k(c: &mut Criterion) {
    let keys1: Vec<String> = (0..10_000).map(|i| format!("key_a_{:04}", i)).collect();
    let keys2: Vec<String> = (5_000..15_000).map(|i| format!("key_a_{:04}", i)).collect();

    let mut t1 = Trie::new();
    for key in &keys1 {
        t1.insert(key.chars(), 1);
    }

    let mut t2 = Trie::new();
    for key in &keys2 {
        t2.insert(key.chars(), 1);
    }

    c.bench_function("intersection_10k", |b| {
        b.iter(|| {
            let mut t = t1;
            t.intersection(&t2);
            black_box(t);
        })
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(10).measurement_time(std::time::Duration::from_secs_f64(10.0));
    targets = bench_insert_100k, bench_get_100k, bench_union_10k, bench_is_subset_10k, bench_intersection_10k
);
criterion_main!(benches);
