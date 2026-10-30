use anyhow::Context;

use ctadl_ascent::codegen::flowy;
use ctadl_ascent::index_engine::{ContextJoin, HybridContext, IndexConfig};

/// The configurations every fixture must pass under: the three context joins compute one
/// fixpoint, so a fixture that passes under one must pass under all. `CTADL_TEST_HYBRID_CONTEXT`
/// (e.g. `collapse`, `bounded:2`) swaps in a coarser hybrid context for all of them, to see
/// which fixtures a precision trade-off breaks; it is not set in CI.
fn configs() -> Vec<IndexConfig> {
    let hybrid_context = std::env::var("CTADL_TEST_HYBRID_CONTEXT")
        .ok()
        .map(|s| {
            s.parse::<HybridContext>()
                .expect("CTADL_TEST_HYBRID_CONTEXT")
        })
        .unwrap_or_default();
    [ContextJoin::Sets, ContextJoin::Scan, ContextJoin::Unfold]
        .into_iter()
        .map(|context_join| IndexConfig {
            hybrid_context,
            context_join,
            ..IndexConfig::default()
        })
        .collect()
}

/// Indexes a .tnt file and ensures the summary requirements are met.
///
/// A sibling `<stem>.models.jsonl` or `<stem>.models.ctadl` is loaded as if passed to
/// `ctadl index --models`. That is how a `.tnt` fixture pins what a *model port* means: flowy
/// needs no toolchain, gets no default models of its own, and its `where summaries [...]` clause
/// asserts on the index summary relation directly — so a fixture can say where taint lands, not
/// merely whether one fixed probe fires.
///
/// Both extensions are picked up, so a fixture can be written in either format.
fn tnt_test<P: AsRef<std::path::Path>>(filename: P) -> anyhow::Result<()> {
    let filename = filename.as_ref();
    let models: Vec<std::path::PathBuf> = vec![
        filename.with_extension("models.jsonl"),
        filename.with_extension("models.ctadl"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    tnt_test_with(filename, &models)
}

/// [`tnt_test`] with the model files given.
fn tnt_test_with(filename: &std::path::Path, models: &[std::path::PathBuf]) -> anyhow::Result<()> {
    for config in configs() {
        flowy::check_with_config(filename, None, models, config.clone())
            .map(|_| ())
            .with_context(|| {
                format!(
            "Running test {} under {:?}. The per-check failures are logged at `warn`, and this test binary prints no log, so run the case on its own to see them: 'RUST_LOG=warn cargo run -p ctadl-ascent --example flowy -- {}'",
            filename.display(),
            config,
            filename.display()
        )
            })?;
    }
    Ok(())
}

/// Fixtures the engine cannot satisfy yet.
///
/// `substitute_prefix_demo.tnt` demonstrates offset-arithmetic path substitution through a
/// field-to-field copy (`result.c.[10] = p_val.a.[60]`). Field reads lower to `Load`
/// instructions, so the copy is `t = load p_val.a.[60]; store result.c.[10] := t`, and the
/// intermediate taint path produced by the offset arithmetic (`.[40]...` = `[100]-[60]`) is not
/// a syntactic program path, so the `paths` gate stops it. Widening the gate to admit
/// arithmetic-derived paths would let the path set grow without bound; covering this case needs
/// an analysis that reasons about heap objects instead of syntactic path composition.
///
/// `path_composition_store.tnt` and `alias_cycle.tnt` reproduce two misses `cil:taint` accepts as
/// known: a path built by storing an object whose own field was stored, which no statement
/// spells and so the index does not admit; and a cycle through a field, which the aliasing rule
/// does not summarize. Each file says what it should derive. Restore them when the engine does.
fn is_known_limitation(path: &std::path::Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("substitute_prefix_demo.tnt" | "path_composition_store.tnt" | "alias_cycle.tnt")
    )
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
            for entry in entries.flatten() {
                let path = entry.path();
                // Check if the file has a .tnt extension
                if path.extension().and_then(|s| s.to_str()) == Some("tnt") {
                    if is_known_limitation(&path) {
                        eprintln!("SKIP (known limitation): {}", path.display());
                        continue;
                    }
                    tnt_test(&path)?;
                }
            }
        }
        Err(_) => panic!("Could not read test dir: {:?}", dir_path),
    }
    Ok(())
}

/// Every fixture with a JSON model file passes with what `ctadl migrate-models` makes of it in
/// its place, so the JSON file and its migration reach the same summaries through the real
/// index pipeline. `port_bare_portcullis.tnt` is the hand-written twin of one of these; this
/// covers the rest without a second copy of each.
#[test]
fn every_json_fixture_passes_as_its_migration() -> anyhow::Result<()> {
    let dir: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "tnt"]
        .iter()
        .collect();
    let out = tempfile::tempdir()?;
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        let json = path.with_extension("models.jsonl");
        if path.extension().and_then(|s| s.to_str()) != Some("tnt")
            || is_known_limitation(&path)
            || !json.exists()
        {
            continue;
        }
        let (text, _) = ctadl_ascent::models::portcullis::migrate::migrate_file(&json, None)?;
        let migrated = out
            .path()
            .join(json.with_extension("ctadl").file_name().unwrap());
        std::fs::write(&migrated, text)?;
        tnt_test_with(&path, &[migrated])?;
        checked += 1;
    }
    assert!(checked > 0, "no fixture has a JSON model file");
    Ok(())
}
