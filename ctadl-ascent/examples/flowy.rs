use clap::Parser;
use ctadl_ascent::codegen::flowy;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The flowy program file to check
    file: PathBuf,

    /// Dump the index graph to a dot file
    #[arg(long)]
    pub dump_index_graph: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_timestamp(None).init();
    let args = Args::parse();
    flowy::check(&args.file, args.dump_index_graph.as_deref())
}
