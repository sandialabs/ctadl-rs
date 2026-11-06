//! The rule-by-rule corpus of the IR's semantics (`docs/ir.md`), run on the reference
//! implementation, `ctadl_ir::semantics`.
//!
//! Each `tests/semantics/*.flowy` program pins one rule of `docs/ir.md`. Its `sink(x, L)` calls
//! assert that label `L` reaches `x`, its `errsink(x, L)` calls that it does not, and a trailing
//! block of `// expect: <function>: <sink>(<label>)#<n> <- <label>` lines (or `// expect: none`)
//! lists the whole observable, so a flow nobody asked about cannot appear unnoticed. `#<n>`
//! numbers a function's sinks of one kind in source order.

mod common;

use common::{corpus, evaluate_flowy, expected, rendered};

#[test]
fn corpus_matches_the_semantics() {
    let mut failed = Vec::new();
    for file in corpus() {
        let text = std::fs::read_to_string(&file).unwrap();
        let run = evaluate_flowy(&file, &text);
        let mut failures = run.assertion_failures();
        if std::env::var_os("SEMANTICS_PRINT").is_some() {
            eprintln!("== {}", file.display());
            for line in rendered(&run.outcome) {
                eprintln!("// expect: {line}");
            }
        }
        if let Some(want) = expected(&text) {
            let got = rendered(&run.outcome);
            if got != want {
                failures.push(format!(
                    "observable differs from the expect block:\n  expected {want:#?}\n  got {got:#?}"
                ));
            }
        }
        if run.outcome.truncated {
            failures.push("a bound of the evaluation was reached".to_string());
        }
        if !failures.is_empty() {
            failed.push(format!("{}:\n  {}", file.display(), failures.join("\n  ")));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
