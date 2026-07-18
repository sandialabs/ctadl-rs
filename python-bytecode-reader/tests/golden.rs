//! Golden-fixture test: checked-in stable text + JSON oracle, so the reader is
//! exercised end-to-end without spawning Python. Keeps a parser regression
//! distinguishable from a Python-environment misconfiguration.

use python_bytecode_reader::{BytecodeFile, parse};

/// Parse the checked-in stable text and assert it exactly matches the checked-in
/// JSON oracle (deserialized into the reader's own model via serde).
#[test]
fn taint_fixture_matches_oracle() {
    let stable = include_str!("fixtures/stable/taint.pybc");
    let oracle_json = include_str!("fixtures/expected/taint.json");

    let parsed: BytecodeFile = parse(stable).expect("parse stable fixture");
    let oracle: BytecodeFile = serde_json::from_str(oracle_json).expect("deserialize json oracle");

    assert_eq!(parsed, oracle);

    // Spot-check structure so a bad-but-equal pair can't pass silently.
    let module = &parsed.code_objects[0];
    assert_eq!(module.name, "<module>");
    // Two top-level functions (`transfer`, `main`) are nested code objects.
    assert_eq!(module.nested_code_objects.len(), 2);
    let names: Vec<_> = module
        .nested_code_objects
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, vec!["transfer", "main"]);
}
