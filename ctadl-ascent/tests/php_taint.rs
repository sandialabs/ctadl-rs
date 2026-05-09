use std::path::PathBuf;
use std::sync::Once;
use tempfile::tempdir;

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::*;

static INIT: Once = Once::new();

pub fn initialize() {
    INIT.call_once(|| {
        let dir = tempdir().unwrap();
        init_store_path(Some(dir)).unwrap();
    });
}

fn run_store_test<F>(test: F)
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    initialize();
    let result = std::panic::catch_unwind(test);
    assert!(result.is_ok())
}

fn php_test_file(filename: &str) -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "php-reader",
        "tests",
        "taint",
        filename,
    ]
    .iter()
    .collect()
}

fn php_model_file() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "php-reader",
        "models",
        "php_taint_models.json",
    ]
    .iter()
    .collect()
}

/// Imports, indexes, and queries `filename`, returning the number of taint
/// tuples the query found.
fn taint_result_count(filename: &str) -> usize {
    let artifact_path = php_test_file(filename);
    let project_name = format!("php_taint_{}", filename.replace(".", "_"));
    let model_path = php_model_file();

    let import =
        ArtifactImport::try_create(&project_name, ArtifactLanguage::Php, &artifact_path).unwrap();
    cli::import(&import).unwrap();

    let project = AnalysisProject::try_create(&project_name, &[&project_name]).unwrap();

    // Use CHA for basic resolution; the remaining knobs take the same defaults
    // the `index` subcommand applies.
    cli::index(
        &project,
        &[],
        &[model_path.clone()],
        CallResolutionStrategy::Cha,
        true,
        true,
        None,
    )
    .unwrap();

    let (result, _ids, _index_facts) = cli::query_taint(&project, &[model_path]).unwrap();
    result.taint.len()
}

// Simple test to ensure the machinery works
#[test]
fn test_php_taint_simple() {
    run_store_test(|| {
        // We expect at least one taint flow (e.g. $_GET -> echo)
        assert!(
            taint_result_count("simple.php") > 0,
            "Expected taint flows for simple.php, found none"
        );
    });
}

#[test]
fn test_php_taint_functions() {
    run_store_test(|| {
        // We expect at least one taint flow (e.g. $_POST -> exec)
        assert!(
            taint_result_count("functions.php") > 0,
            "Expected taint flows for functions.php, found none"
        );
    });
}

#[test]
fn test_php_taint_access_path() {
    run_store_test(|| {
        // We expect at least one taint flow (e.g. $_GET -> passthru)
        assert!(
            taint_result_count("access_path.php") > 0,
            "Expected taint flows for access_path.php, found none"
        );
    });
}
