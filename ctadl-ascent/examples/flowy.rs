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

    /// Load models from one or more JSON, JSON5, or JSONL files, as `ctadl index --models`
    /// would. Their propagations become summaries before the requirements are checked.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_timestamp(None).init();
    let args = Args::parse();
    flowy::check(&args.file, args.dump_index_graph.as_deref(), &args.models)
}
