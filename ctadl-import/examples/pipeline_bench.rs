//! Times the default index pipeline on one import at several thread counts, and checks that every
//! thread count produces the same IR.
//!
//!   cargo run --release -p ctadl-import --example pipeline_bench -- <import-dir> 1 2 4 8
//!
//! Each run starts from a fresh copy of the loaded program. A `none` row fingerprints the IR before
//! any pass runs, so the rows after it show what the passes did.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use ctadl_import::SourceInfoMode;
use ctadl_import::load_import;
use ctadl_ir::mir::{Program, StatementKind};
use ctadl_ir::ssa;

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    blocks: usize,
    statements: usize,
    phis: usize,
    loads: usize,
    /// Hash of every function's text.
    hash: u64,
}

fn fingerprint(p: &Program) -> Fingerprint {
    let (mut blocks, mut statements, mut phis, mut loads) = (0, 0, 0, 0);
    let mut h = DefaultHasher::new();
    for f in p.functions.iter() {
        for (_, data) in f.blocks.iter_enumerated() {
            blocks += 1;
            for s in data.iter() {
                statements += 1;
                match s.kind {
                    StatementKind::Phi { .. } => phis += 1,
                    StatementKind::Load { .. } => loads += 1,
                    _ => {}
                }
            }
        }
        f.to_string().hash(&mut h);
    }
    Fingerprint {
        blocks,
        statements,
        phis,
        loads,
        hash: h.finish(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let import = ctadl_import::store::resolve_import(&args[1]).expect("import");
    let t = Instant::now();
    let info = load_import(&import, SourceInfoMode::Skip).expect("load");
    let pipeline = ssa::Pipeline::index_default();
    println!(
        "{}: {} functions, loaded in {:.2?}, pipeline {}",
        import.name,
        info.program.functions.len(),
        t.elapsed(),
        pipeline.tag()
    );
    println!("none: {:?}", fingerprint(&info.program));
    // No thread count can finish faster than the largest function takes on its own.
    if let Some(big) = info.program.functions.iter().max_by_key(|f| {
        f.blocks
            .iter_enumerated()
            .map(|(_, b)| b.iter().count())
            .sum::<usize>()
    }) {
        let mut one = Program::default();
        one.functions.push(big.clone());
        let t = Instant::now();
        ssa::run_pipeline(&mut one, pipeline);
        println!("largest function {}: {:.2?} alone", big.name, t.elapsed());
    }
    let mut reference: Option<(usize, f64, Fingerprint)> = None;
    for threads in args[2..]
        .iter()
        .map(|s| s.parse::<usize>().expect("thread count"))
    {
        let mut program = info.program.clone();
        let t = Instant::now();
        ssa::run_pipeline_with_threads(&mut program, pipeline, threads);
        let secs = t.elapsed().as_secs_f64();
        let fp = fingerprint(&program);
        drop(program);
        match &reference {
            None => {
                println!("j={threads}: {secs:.2} s  {fp:?}");
                reference = Some((threads, secs, fp));
            }
            Some((rt, rs, rfp)) => {
                println!(
                    "j={threads}: {secs:.2} s  speedup vs j={rt}: {:.2}x  same IR: {}",
                    rs / secs,
                    &fp == rfp
                );
                assert_eq!(&fp, rfp, "j={threads} produced different IR from j={rt}");
            }
        }
    }
}
