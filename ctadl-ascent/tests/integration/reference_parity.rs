//! The engines measured against the reference implementation of the IR's semantics (`docs/ir.md`).
//!
//! For every program of the semantics corpus (`frontends/ctadl-flowy/tests/semantics`) and
//! every `.tnt` program, this computes the observable twice, once with the index and the query
//! and once with `ctadl_ir::semantics`, each sink restricted to its own label, and diffs them.
//! Every difference must be a row of the approximations register (`docs/limitations.md`), and
//! the program that shows it says so on a line
//!
//! ```text
//! // parity: <row> <adds|loses>: <function>: <sink> <- <label>
//! ```
//!
//! naming the row and the observation. A difference with no such line is a bug in the engines
//! or the reference, and so is a line no difference matches. A `.tnt` program with a sibling
//! model file is skipped: models are applied by the engines and not by the reference.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ctadl_ascent::codegen::flowy::engine_observable;
use ctadl_ascent::index_engine::IndexConfig;
use ctadl_ir::semantics::{Config, Observer, Ports, Seed, evaluate_programs};

/// Whether a register row's deviation adds flows or loses them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Direction {
    Adds,
    Loses,
}

/// The register's rows that the engines can show on a flowy program, by the name a `// parity:`
/// line uses, with their direction. The register in `docs/limitations.md` is the authority; this
/// mirrors it.
const ROWS: &[(&str, Direction)] = &[
    ("admissible paths", Direction::Loses),
    ("callee cells merge", Direction::Adds),
    ("resolution ladder", Direction::Adds),
    ("hybrid context", Direction::Adds),
    ("backward alias rule", Direction::Loses),
    ("shared formals", Direction::Adds),
    ("copied formals", Direction::Loses),
    ("object hop", Direction::Adds),
    ("copy_prop", Direction::Adds),
    ("mem2reg window", Direction::Loses),
    ("mem2reg full", Direction::Loses),
];

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for (dir, ext) in [
        (
            root.join("../frontends/ctadl-flowy/tests/semantics"),
            "flowy",
        ),
        (root.join("tests/tnt"), "tnt"),
    ] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|e| e == ext) {
                files.push(p);
            }
        }
    }
    files.sort();
    files
}

/// The reference's observable for `file`, each sink restricted to its own label. A file of
/// several programs is evaluated as one project.
fn reference_observable(file: &Path) -> BTreeSet<String> {
    let progs = ctadl_flowy::compile_programs(file).unwrap();
    let mut ports = Ports::default();
    let mut own_label = std::collections::HashMap::new();
    for prog in &progs {
        for (function, endpoint) in prog.sources() {
            ports.seeds.push(Seed {
                function,
                path: endpoint.port.clone(),
                label: endpoint.label.clone(),
            });
        }
        for sink in prog.named_sinks() {
            own_label.insert(
                (sink.function.clone(), sink.name.clone()),
                sink.endpoint.label.clone(),
            );
            ports.observers.push(Observer {
                function: sink.function,
                path: sink.endpoint.port.clone(),
                name: sink.name,
            });
        }
    }
    let programs: Vec<&ctadl_ir::mir::Program> =
        progs.iter().map(|p| &p.program_info.program).collect();
    evaluate_programs(&programs, &ports, &Config::default())
        .observed
        .into_iter()
        .filter(|o| own_label.get(&(o.function.clone(), o.site.clone())) == Some(&o.label))
        .map(|o| o.to_string())
        .collect()
}

/// The `// parity:` lines of a program: `(row, direction, observation)`.
fn declared(text: &str) -> Vec<(String, Direction, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("// parity:") else {
            continue;
        };
        let (head, observation) = rest
            .split_once(": ")
            .unwrap_or_else(|| panic!("malformed parity line: {line}"));
        let head = head.trim();
        let (row, direction) = if let Some(row) = head.strip_suffix(" adds") {
            (row, Direction::Adds)
        } else if let Some(row) = head.strip_suffix(" loses") {
            (row, Direction::Loses)
        } else {
            panic!("a parity line says `adds` or `loses`: {line}")
        };
        out.push((row.to_string(), direction, observation.trim().to_string()));
    }
    out
}

/// The unexplained differences of one program, empty when there are none.
fn check(file: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(file).unwrap();
    // A program that must not compile has no observable.
    if text.contains("// expect-error:") {
        return Vec::new();
    }
    let engine = match engine_observable(file, IndexConfig::default()) {
        Ok(o) => o,
        Err(e) => return vec![format!("the engines failed: {e:#}")],
    };
    let reference = reference_observable(file);
    let mut diffs: Vec<(Direction, String)> = engine
        .difference(&reference)
        .map(|o| (Direction::Adds, o.clone()))
        .collect();
    diffs.extend(
        reference
            .difference(&engine)
            .map(|o| (Direction::Loses, o.clone())),
    );
    let declared = declared(&text);
    let mut out = Vec::new();
    for (row, direction, _) in &declared {
        if !ROWS.contains(&(row.as_str(), *direction)) {
            out.push(format!(
                "`{row}` {direction:?} is not a row of the approximations register"
            ));
        }
    }
    for (direction, observation) in &diffs {
        if !declared
            .iter()
            .any(|(_, d, o)| d == direction && o == observation)
        {
            out.push(format!(
                "unexplained difference: the engines {} `{observation}`",
                match direction {
                    Direction::Adds => "add",
                    Direction::Loses => "lose",
                }
            ));
        }
    }
    for (row, direction, observation) in &declared {
        if !diffs
            .iter()
            .any(|(d, o)| d == direction && o == observation)
        {
            out.push(format!(
                "`{row}` is declared to explain `{observation}`, which is no difference"
            ));
        }
    }
    out
}

#[test]
fn engines_match_the_reference_up_to_the_register() {
    let mut failed = Vec::new();
    for file in corpus() {
        let has_models = ["models.jsonl", "models.ctadl"]
            .iter()
            .any(|ext| file.with_extension(ext).exists());
        if has_models {
            continue;
        }
        let failures = check(&file);
        if !failures.is_empty() {
            failed.push(format!("{}:\n  {}", file.display(), failures.join("\n  ")));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
