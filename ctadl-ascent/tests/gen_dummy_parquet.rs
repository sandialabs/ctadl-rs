use ctadl_ascent::facts::FunctionId;
use ctadl_ascent::facts::schema::external_function;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("/tmp/ctadl_test/index");
    std::fs::create_dir_all(&dir).unwrap();
    let records = vec![(FunctionId::new(42),), (FunctionId::new(100),)];
    external_function::try_save(&dir, records).unwrap();
    println!(
        "Saved dummy parquet file to {:?}",
        dir.join("external_function.parquet")
    );
}
