use ctadl_ascent::codegen::flowy;

fn main() -> anyhow::Result<()> {
    env_logger::builder().format_timestamp(None).init();
    let args: Vec<String> = std::env::args().collect();
    flowy::check(&args[1])
}
