use ascent::ascent;
use byods_example::btreerel;
use byods_example::lmdbrel;
use byods_example::util::{print_resources, read_csv};
use clap::Parser;
use nix::sys::resource::{UsageWho, getrusage};
use std::path::PathBuf;
use std::time::Instant;

ascent! {
    struct PathsDefault;

    relation edge(String, String);
    relation path(String, String);

    path(x, y) <-- edge(x, y);
    path(x, z) <-- path(x, y), edge(y, z);
}

ascent! {
    struct PathsBTree;

    #[ds(btreerel)]
    relation edge(String, String);
    #[ds(btreerel)]
    relation path(String, String);

    path(x, y) <-- edge(x, y);
    path(x, z) <-- path(x, y), edge(y, z);
}

ascent! {
    struct PathsLmdb;

    #[ds(lmdbrel)]
    relation edge(String, String);
    #[ds(lmdbrel)]
    relation path(String, String);

    path(x, y) <-- edge(x, y);
    path(x, z) <-- path(x, y), edge(y, z);
}

#[derive(clap::ValueEnum, Clone)]
enum Mode {
    Default, // default
    BTree,   // b-tree
    Lmdb,    // lmdb
}

/// Parser for command line arguments
#[derive(Parser)]
struct Args {
    /// The path to the file to read
    file_path: PathBuf,
    /// The variant of the program to run
    #[arg(value_enum)]
    mode: Mode,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.mode {
        Mode::Default => {
            println!("\nRunning PathsDefault....");
            let mut prog = PathsDefault {
                edge: read_csv(&args.file_path).unwrap().collect(),
                ..Default::default()
            };
            let start = Instant::now();
            prog.run();
            let end = Instant::now();
            println!("num paths: {}\n", prog.path.len());
            // println!("paths: {:?}", prog.path);
            let usage = getrusage(UsageWho::RUSAGE_SELF).expect("failed to get resource usage");
            print_resources(end - start, &usage);
        }
        Mode::BTree => {
            println!("\nRunning PathsBTree....");
            let mut prog = PathsBTree {
                edge: read_csv(&args.file_path).unwrap().collect(),
                ..Default::default()
            };
            let start = Instant::now();
            prog.run();
            let end = Instant::now();
            println!("num paths: {}\n", prog.path.len());
            // println!("paths: {:?}", prog.path);
            let usage = getrusage(UsageWho::RUSAGE_SELF).expect("failed to get resource usage");
            print_resources(end - start, &usage);
        }
        Mode::Lmdb => {
            println!("\nRunning PathsLmdb....");
            let mut prog = PathsLmdb {
                edge: read_csv(&args.file_path).unwrap().collect(),
                ..Default::default()
            };
            let start = Instant::now();
            prog.run();
            let end = Instant::now();
            println!("num paths: {}\n", prog.path.len());
            // println!("paths: {:?}", prog.path);
            let usage = getrusage(UsageWho::RUSAGE_SELF).expect("failed to get resource usage");
            print_resources(end - start, &usage);
        }
    }

    Ok(())
}
