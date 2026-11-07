//! Differential tests of the IR passes against the reference implementation (S18 of `docs/ir.md`).
//!
//! A pass is correct if the SSA form of its output has the same observable labeling as the SSA
//! form of its input (S18). For every program of the semantics corpus and every `.tnt` program
//! the engines are tested on, and for each pass and the whole pipeline, this evaluates both and
//! compares them. The only differences allowed are the approximations the register lists, and a
//! corpus program that documents one says so on a `// passes: loses <pass>, ...` line, where the
//! output may only lose observations, or a `// passes: adds <pass>, ...` line, where it may only
//! gain them.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{corpus, rendered, tnt_programs};
use ctadl_ir::cir::Program;
use ctadl_ir::semantics::{Config, Ports, evaluate, ssa_form};
use ctadl_ir::ssa::{self, EscapePolicy, Mem2Reg, Pipeline};

/// The flowy builtins, by name: before SSA, flowy's `source` and `sink` are still calls.
fn ports() -> Ports {
    Ports {
        sources: ["source", "errsource"].map(String::from).into(),
        sinks: ["sink", "errsink"].map(String::from).into(),
        ..Ports::default()
    }
}

/// A pass, applied to the program before SSA, or after it when `after_ssa`.
struct Pass {
    name: &'static str,
    after_ssa: bool,
    run: fn(&mut Program),
}

fn passes() -> Vec<Pass> {
    vec![
        Pass {
            name: "dead_temps",
            after_ssa: false,
            run: ssa::eliminate_dead_temps,
        },
        Pass {
            name: "coalesce",
            after_ssa: false,
            run: ssa::coalesce_copies,
        },
        Pass {
            name: "mem2reg window:8",
            after_ssa: false,
            run: |p| {
                ssa::promote_stack_slots(p, EscapePolicy::Window(8));
            },
        },
        Pass {
            name: "mem2reg above",
            after_ssa: false,
            run: |p| {
                ssa::promote_stack_slots(p, EscapePolicy::AllAbove);
            },
        },
        Pass {
            name: "mem2reg full",
            after_ssa: false,
            run: |p| {
                ssa::promote_stack_slots(
                    p,
                    Mem2Reg {
                        escape: EscapePolicy::Window(8),
                        full_above: Some(0),
                    },
                );
            },
        },
        Pass {
            name: "copy_prop",
            after_ssa: true,
            run: ssa::propagate_copies,
        },
        Pass {
            name: "pipeline",
            after_ssa: false,
            run: |p| ssa::run_pipeline(p, Pipeline::index_default()),
        },
    ]
}

/// The passes a program's `// passes: <how> ...` line names, `how` being `loses` or `adds`.
fn declared(text: &str, how: &str) -> BTreeSet<String> {
    let prefix = format!("// passes: {how}");
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(prefix.as_str()))
        .flat_map(|rest| rest.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Every difference between the oracle before and after each pass on `file`, beyond what the
/// file declares; empty when there is none.
fn check(file: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(file).unwrap();
    let input = match ctadl_flowy::compile_ir(&file.to_string_lossy(), &text) {
        Ok(p) => p,
        Err(e) => return vec![format!("does not compile: {e}")],
    };
    let ports = ports();
    let config = Config::default();
    let ssa_input = ssa_form(&input);
    let before = rendered(&evaluate(&ssa_input, &ports, &config));
    let losses = declared(&text, "loses");
    let adds = declared(&text, "adds");
    let mut out = Vec::new();
    for pass in passes() {
        let mut program = if pass.after_ssa {
            ssa_input.clone()
        } else {
            input.clone()
        };
        (pass.run)(&mut program);
        let after = rendered(&evaluate(&ssa_form(&program), &ports, &config));
        if after == before {
            if losses.contains(pass.name) || adds.contains(pass.name) {
                out.push(format!(
                    "{}: declared to change the observable, but does not",
                    pass.name
                ));
            }
            continue;
        }
        let lost: Vec<&String> = before.difference(&after).collect();
        let gained: Vec<&String> = after.difference(&before).collect();
        if losses.contains(pass.name) && gained.is_empty() {
            continue;
        }
        if adds.contains(pass.name) && lost.is_empty() {
            continue;
        }
        out.push(format!("{}: lost {lost:?}, gained {gained:?}", pass.name));
    }
    out
}

#[test]
fn passes_preserve_the_observable() {
    let mut failed = Vec::new();
    let mut checked = 0;
    for file in corpus().into_iter().chain(tnt_programs()) {
        checked += 1;
        let failures = check(&file);
        if !failures.is_empty() {
            failed.push(format!("{}:\n  {}", file.display(), failures.join("\n  ")));
        }
    }
    assert!(checked > 0);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
