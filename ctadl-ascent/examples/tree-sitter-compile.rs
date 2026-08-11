fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let contents = source_info::read_source(std::path::Path::new(&args[1]))?;
    println!("no");
    ctadl_ascent::languages::tree_sitter_c::parse_c_program(&contents)?;
    Ok(())
}
