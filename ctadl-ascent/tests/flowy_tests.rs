use anyhow::Context;

use ctadl_ascent::codegen::flowy;

/// Indexes a .tnt file and ensures the summary requirements are met.
fn tnt_test<P: AsRef<std::path::Path>>(filename: P) -> anyhow::Result<()> {
    let filename = filename.as_ref();
    flowy::check(filename, None)
        .map(|_| ())
        .with_context(|| {
            format!(
            "Running test {}. Run 'cargo test -- --nocapture' to see full output. Run 'cargo run -p ctadl-ascent --bin flowy' on the file to run individual test case",
            filename.display()
        )
        })
}

// Parse index files and discharge the assertions.
#[test]
fn all_flowy_tests() -> anyhow::Result<()> {
    use std::{fs, path};
    let dir_path: path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "tnt"]
        .iter()
        .collect();
    match fs::read_dir(&dir_path) {
        Ok(entries) => {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    // Check if the file has a .tnt extension
                    if path.extension().and_then(|s| s.to_str()) == Some("tnt") {
                        tnt_test(&path)?;
                    }
                }
            }
        }
        Err(_) => panic!("Could not read test dir: {:?}", dir_path),
    }
    Ok(())
}
