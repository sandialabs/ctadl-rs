//! What the semantics corpus and the pass differential tests share: the corpus, and running a
//! flowy program's endpoints on the reference implementation.

#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ctadl_flowy::{EndpointDirection, FlowSpec, FlowyProgram};
use ctadl_ir::mir::Program;
use ctadl_ir::semantics::{Config, Observer, Outcome, Ports, Seed, evaluate};

/// The corpus files, sorted.
pub fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/semantics");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "flowy"))
        .collect();
    files.sort();
    files
}

/// The `.tnt` programs the engines are tested on, sorted.
pub fn tnt_programs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ctadl-ascent/tests/tnt");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "tnt"))
        .collect();
    files.sort();
    files
}

/// A flowy program's sources and sinks as the reference's ports, and the sinks' requirements:
/// `(observer name, label, whether the flow must be present)`.
pub fn flowy_ports(prog: &FlowyProgram) -> (Ports, Vec<(String, String, String, bool)>) {
    let mut ports = Ports::default();
    let mut requirements = Vec::new();
    let mut functions: Vec<_> = prog
        .requirements
        .endpoint_requires
        .requires
        .iter()
        .collect();
    functions.sort_by(|a, b| a.0.cmp(b.0));
    for (function, endpoints) in functions {
        let mut endpoints: Vec<_> = endpoints.iter().collect();
        endpoints.sort_by_key(|(e, _)| e.source_info);
        let mut n: HashMap<&str, usize> = HashMap::new();
        for (endpoint, spec) in endpoints {
            match endpoint.direction {
                EndpointDirection::Source => ports.seeds.push(Seed {
                    function: function.to_string(),
                    path: endpoint.port.clone(),
                    label: endpoint.label.clone(),
                }),
                EndpointDirection::Sink => {
                    let present = matches!(spec, FlowSpec::FlowPresent);
                    let kind = if present { "sink" } else { "errsink" };
                    let k = n.entry(kind).or_default();
                    let name = format!("{kind}({})#{k}", endpoint.label);
                    *k += 1;
                    ports.observers.push(Observer {
                        function: function.to_string(),
                        path: endpoint.port.clone(),
                        name: name.clone(),
                    });
                    requirements.push((
                        function.to_string(),
                        name,
                        endpoint.label.clone(),
                        present,
                    ));
                }
            }
        }
    }
    (ports, requirements)
}

/// A flowy program evaluated on the reference, with its sinks' requirements.
pub struct Run {
    pub outcome: Outcome,
    pub requirements: Vec<(String, String, String, bool)>,
}

impl Run {
    /// Each `sink` the semantics does not reach and each `errsink` it does.
    pub fn assertion_failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (function, name, label, present) in &self.requirements {
            let reached = self
                .outcome
                .observed
                .iter()
                .any(|o| &o.function == function && &o.site == name && &o.label == label);
            if reached != *present {
                out.push(format!(
                    "{function}: {name} {}",
                    if *present {
                        "is not reached"
                    } else {
                        "is reached"
                    }
                ));
            }
        }
        out
    }
}

/// Compiles `text` and evaluates it on the reference with its own endpoints.
pub fn evaluate_flowy(file: &Path, text: &str) -> Run {
    let prog = ctadl_flowy::compile_program_contents(&file.to_string_lossy(), text)
        .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let (ports, requirements) = flowy_ports(&prog);
    let outcome = evaluate(&prog.program_info.program, &ports, &Config::default());
    Run {
        outcome,
        requirements,
    }
}

/// Evaluates `program`, put in SSA form first, with the endpoints of the flowy program it was
/// compiled from. The endpoints name SSA variables, so `program` must lower to the same SSA
/// names: true of the IR passes, which keep the variables they do not delete.
pub fn evaluate_with(program: &Program, ports: &Ports) -> Outcome {
    let ssa = ctadl_ir::semantics::ssa_form(program);
    evaluate(&ssa, ports, &Config::default())
}

/// One line per observation.
pub fn rendered(outcome: &Outcome) -> BTreeSet<String> {
    outcome.observed.iter().map(|o| o.to_string()).collect()
}

/// The `// expect:` lines of a corpus file.
pub fn expected(text: &str) -> Option<BTreeSet<String>> {
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("// expect:"))
        .map(str::trim)
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(
        lines
            .into_iter()
            .filter(|l| *l != "none")
            .map(String::from)
            .collect(),
    )
}
