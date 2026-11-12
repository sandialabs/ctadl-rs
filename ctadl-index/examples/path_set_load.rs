//! Times loading a `paths.parquet` the way a staged worker does: decode, then freeze.
use ctadl_index::facts::Path;
use ctadl_index::facts::parquet::Reader;
use ctadl_index::facts::schema;
use ctadl_index::index_engine::path_set::PathSet;
use ctadl_index::index_engine::phys_footprint_mb;
use std::time::Instant;

fn main() {
    let file = std::env::args()
        .nth(1)
        .expect("usage: path_set_load <paths.parquet>");
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
