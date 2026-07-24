fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let contents = std::fs::read_to_string(&args[1])?;
    println!("no");
    ctadl_ascent::languages::tree_sitter::parse_c_program(&contents)?;
    Ok(())
}
