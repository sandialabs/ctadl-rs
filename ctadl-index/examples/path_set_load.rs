//! Times loading a `paths.parquet` the way a staged worker did: decode, then freeze. With
//! `--mapped`, the file is a `path_set.bin` instead, and it times mapping it the way a worker
//! does now, and what touching every page of it adds to the footprint.
use ctadl_index::facts::Path;
use ctadl_index::facts::parquet::Reader;
use ctadl_index::facts::schema;
use ctadl_index::index_engine::path_set::PathSet;
use ctadl_index::index_engine::phys_footprint_mb;
use std::time::Instant;

fn main() {
    let file = std::env::args().nth(1).expect(
        "usage: path_set_load <paths.parquet> [--decode|--steps] | <path_set.bin> --mapped",
    );
    if std::env::args().nth(2).as_deref() == Some("--mapped") {
        return mapped(&file);
    }
    let t = Instant::now();
    let paths: Vec<Path> = Reader::new(&file)
        .read_vec::<schema::paths::Record>(&schema::paths::COLUMNS)
        .unwrap()
        .into_iter()
        .map(|(p,)| p)
        .collect();
    let read = t.elapsed();
    let decoded_mb = phys_footprint_mb();
    let segs: usize = paths.iter().map(|p| p.len()).sum();
    if std::env::args().nth(2).as_deref() == Some("--decode") {
        // What a decode costs beyond consing: the strings alone, and consing alone.
        let strings: Vec<String> = paths.iter().map(Path::to_dot_string).collect();
        let t = Instant::now();
        let n: usize = strings
            .iter()
            .map(|s| ctadl_ir::cir::parse_segments(s).unwrap().len())
            .sum();
        println!(
            "parse_segments {:.2} s ({n} segments)",
            t.elapsed().as_secs_f64()
        );
        let segs: Vec<Vec<_>> = paths.iter().map(|p| p.iter().cloned().collect()).collect();
        let t = Instant::now();
        let n = segs
            .iter()
            .map(|s| Path::from_accesses(s.iter().cloned()))
            .filter(|p| !p.is_empty())
            .count();
        println!(
            "cons every segment, all interned {:.2} s ({n} paths)",
            t.elapsed().as_secs_f64()
        );
        // Distinct nodes: a path and each of its tails.
        let mut nodes = std::collections::HashSet::new();
        for p in &paths {
            let mut q = *p;
            while !q.is_empty() && nodes.insert(q) {
                q = q.tail();
            }
        }
        println!("{} distinct nodes", nodes.len());
    }
    if std::env::args().nth(2).as_deref() == Some("--steps") {
        // `PathSet::from_paths`'s per-member work, one step at a time, in its order.
        let mut sorted = paths.clone();
        let t = Instant::now();
        sorted.sort_unstable();
        sorted.dedup();
        println!("sort+dedup {:.2} s", t.elapsed().as_secs_f64());
        let t = Instant::now();
        let n: usize = sorted.iter().map(|p| p.prefix_keys().len()).sum();
        println!(
            "prefix_keys {:.2} s ({n} splits)",
            t.elapsed().as_secs_f64()
        );
        let t = Instant::now();
        let n: usize = sorted.iter().map(|p| p.prefix_keys_wild().len()).sum();
        println!(
            "prefix_keys_wild {:.2} s ({n} splits)",
            t.elapsed().as_secs_f64()
        );
        let t = Instant::now();
        let n = sorted
            .iter()
            .filter(|p| p.split_trailing_offset().is_some())
            .count();
        println!(
            "split_trailing_offset {:.2} s ({n} some)",
            t.elapsed().as_secs_f64()
        );
    }
    let t = Instant::now();
    let set = PathSet::from_paths(paths.iter().copied());
    let freeze = t.elapsed();
    println!(
        "footprint: {decoded_mb:.0} MB after decode, {:.0} MB after from_paths",
        phys_footprint_mb()
    );
    println!(
        "{} paths, {} segments, max depth {}; read+decode+intern {:.2} s; PathSet::from_paths {:.2} s",
        paths.len(),
        segs,
        paths.iter().map(|p| p.len()).max().unwrap_or(0),
        read.as_secs_f64(),
        freeze.as_secs_f64()
    );
    drop(set);
}

/// Maps `file`, a `path_set.bin`, as the first thing this process does with paths.
fn mapped(file: &str) {
    let before = phys_footprint_mb();
    let t = Instant::now();
    let set = PathSet::map_file(std::path::Path::new(file)).unwrap();
    let map = t.elapsed();
    let mapped_mb = phys_footprint_mb();
    // Touch every page: each member's splits, and each node of each path they name.
    let t = Instant::now();
    let mut segments = 0usize;
    let mut splits = 0usize;
    let mut raw = 1u32;
    let mut seen = 0;
    while seen < set.len() {
        if let Some(p) = Path::from_raw(raw).filter(|p| set.contains(p)) {
            seen += 1;
            let s = set.splits(&p);
            splits += s.exact.len() + s.wild.len();
            for &(key, rest) in s.exact.iter().chain(s.wild) {
                segments += key.len() + rest.len();
            }
            segments += p.len();
            let _ = set.split_trailing_offset(&p);
        }
        raw += 1;
    }
    let touch = t.elapsed();
    println!(
        "{} paths, {splits} splits, {segments} segments walked; map {:.3} s, touch {:.2} s",
        set.len(),
        map.as_secs_f64(),
        touch.as_secs_f64()
    );
    println!(
        "footprint: {before:.0} MB before, {mapped_mb:.0} MB after map, {:.0} MB after touching \
         every page",
        phys_footprint_mb()
    );
}
