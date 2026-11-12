//! Times loading a `paths.parquet` the way a staged worker does: decode, then freeze.
use ctadl_index::facts::Path;
use ctadl_index::facts::parquet::Reader;
use ctadl_index::facts::schema;
use ctadl_index::index_engine::path_set::PathSet;
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
    let segs: usize = paths.iter().map(|p| p.len()).sum();
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
        "{} paths, {} segments, max depth {}; read+decode+intern {:.2} s; PathSet::from_paths {:.2} s",
        paths.len(),
        segs,
        paths.iter().map(|p| p.len()).max().unwrap_or(0),
        read.as_secs_f64(),
        freeze.as_secs_f64()
    );
    drop(set);
}
