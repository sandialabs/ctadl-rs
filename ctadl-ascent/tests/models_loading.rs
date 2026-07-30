use ctadl_ascent::facts::TaintDirection;
use ctadl_ascent::models::json::ModelGeneratorIngest;
use ctadl_ascent::models::{ImportScope, ProgramMatchIndex};
use ctadl_ascent::models::{ProgramModelMatches, UnmatchedReason, try_load_models};
use ctadl_ir::mir::ProgramInfo;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_load_models_json() {
    let program_info = ProgramInfo::default();
    let mut file = NamedTempFile::new().unwrap();
    let json_content = r#"{
        "model_generators": [
            {
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "test"}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
            }
        ]
    }"#;
    writeln!(file, "{}", json_content).unwrap();

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    let result = try_load_models(&match_index, file.path(), &mut matches);
    assert!(
        result.is_ok(),
        "Failed to load JSON models: {:?}",
        result.err()
    );
}

#[test]
fn test_load_models_jsonl() {
    let program_info = ProgramInfo::default();
    // Use .jsonl extension
    let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
    let jsonl_content = r#"{"find": "methods", "where": [{"constraint": "signature_match", "name": "test"}], "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}}"#;
    writeln!(file, "{}", jsonl_content).unwrap();

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    let result = try_load_models(&match_index, file.path(), &mut matches);
    assert!(
        result.is_ok(),
        "Failed to load JSONL models: {:?}",
        result.err()
    );
}

#[test]
fn test_load_models_json5() {
    let program_info = ProgramInfo::default();
    // Use .json5 extension
    let mut file = NamedTempFile::with_suffix(".json5").unwrap();
    let json5_content = r#"{
        // This is a comment, allowed in JSON5
        model_generators: [ // Unquoted keys allowed in JSON5
            {
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": "test"}],
                "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
            },
        ] // Trailing commas allowed in JSON5
    }"#;
    writeln!(file, "{}", json5_content).unwrap();

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    let result = try_load_models(&match_index, file.path(), &mut matches);
    assert!(
        result.is_ok(),
        "Failed to load JSON5 models: {:?}",
        result.err()
    );
}

/// Loading is batched in chunks of 1024 (`try_load_models_from_values`). Every generator must
/// survive that batching, and every generator's index must keep counting across batch
/// boundaries: the index names the generator in JSON error messages and in the `CTADL0004`
/// SARIF notification, and it keys `endpoint_stats`, so a per-batch index both misnames
/// generators and collides their match counts.
#[test]
fn test_load_models_across_batch_boundary() {
    const COUNT: usize = 1030;
    let program_info = ProgramInfo::default();
    let mut file = NamedTempFile::new().unwrap();
    let generators: Vec<serde_json::Value> = (0..COUNT)
        .map(|i| {
            serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "name", "pattern": format!("^f{i}$")}],
                "model": {"sources": [{"port": "Argument(0)", "kind": "K"}]}
            })
        })
        .collect();
    write!(
        file,
        "{}",
        serde_json::json!({ "model_generators": generators })
    )
    .unwrap();

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    let report =
        try_load_models(&match_index, file.path(), &mut matches).expect("loading models");
    let indices: Vec<usize> = report.endpoint_stats.keys().map(|(i, _)| *i).collect();
    assert_eq!(
        indices.len(),
        COUNT,
        "every generator should be accounted for, including #1025"
    );
    assert_eq!(indices.first().copied(), Some(0));
    assert_eq!(indices.last().copied(), Some(COUNT - 1));
}

/// `CTADL0100` reports two different units: model *ports* declared, and *endpoints* matched
/// after fanning those ports out over the program. `ports_declared` therefore counts one per
/// `sources`/`sinks` entry, not one per (generator, direction) — counting the latter made the
/// summary compare a count of generators against a count of endpoints.
///
/// `CTADL0004` reports which of the several ways a port can produce no endpoint happened.
/// "Matched no function" is only one of them: functions can match and the port still resolve
/// in none of them, and reporting that as a failed `where` sends the reader to rewrite a
/// constraint that was working.
#[test]
fn test_endpoint_stats_ports_and_unmatched_reasons() {
    const PROGRAM: &str = r#"
def reader(buf):1 {
s:
  x = buf;
  return x;
}

def writer(dst):1 {
s:
  y = dst;
  return y;
}

def main() {
s:
  a = reader(1);
  b = writer(a);
  return;
}
"#;
    let program_info = ctadl_flowy::compile_program_contents("test.tnt", PROGRAM)
        .expect("compiling flowy program")
        .program_info;
    let mut matches = ProgramModelMatches::default();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
    let generators = vec![
        // 0: two source ports on one generator, both matching `reader`.
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^reader$"}],
            "model": {"sources": [
                {"port": "Argument(0)", "kind": "S"},
                {"port": "Return", "kind": "S"},
            ]},
        }),
        // 1: the `where` constraint selects no function.
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^nosuchfunction$"}],
            "model": {"sinks": [{"port": "Argument(0)", "kind": "S"}]},
        }),
        // 2: `writer` matches, but has no local named `nope`.
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^writer$"}],
            "model": {"sinks": [{"port": "Variable(nope)", "kind": "S"}]},
        }),
        // 3: `writer` matches as a callee, but no caller satisfies `in_function`.
        serde_json::json!({
            "find": "callsites",
            "where": [
                {"constraint": "name", "pattern": "^writer$"},
                {"constraint": "in_function",
                 "inner": {"constraint": "name", "pattern": "^nosuchcaller$"}},
            ],
            "model": {"sinks": [{"port": "Argument(0)", "kind": "S"}]},
        }),
        // 4: two sink ports of one direction failing for two different reasons.
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^writer$"}],
            "model": {"sinks": [
                {"port": "Variable(nope)", "kind": "S"},
                {"port": "Variable(alsonope)", "kind": "S"},
            ]},
        }),
    ];
    ingest.encode_models(generators).expect("encoding models");
    let stats = &ingest.endpoint_stats;
    let get = |index: usize, direction| {
        stats
            .get(&(index, direction))
            .unwrap_or_else(|| panic!("no stats for generator {index}"))
    };

    let live = get(0, TaintDirection::Forward);
    assert_eq!(live.ports_declared, 2, "one per `sources` entry");
    assert_eq!(live.endpoints_matched, 2, "one function per port");
    assert!(live.unmatched.is_empty());

    let no_function = get(1, TaintDirection::Backward);
    assert_eq!(no_function.ports_declared, 1);
    assert_eq!(no_function.endpoints_matched, 0);
    assert_eq!(no_function.functions_matched, 0);
    assert_eq!(
        no_function.unmatched.iter().collect::<Vec<_>>(),
        vec![&UnmatchedReason::NoFunctionMatched]
    );

    let no_local = get(2, TaintDirection::Backward);
    assert_eq!(no_local.endpoints_matched, 0);
    assert_eq!(
        no_local.functions_matched, 1,
        "the `where` constraint did match; only the port did not resolve"
    );
    assert_eq!(
        no_local.unmatched.iter().collect::<Vec<_>>(),
        vec![&UnmatchedReason::LocalNotFound("nope".to_string())]
    );

    let no_caller = get(3, TaintDirection::Backward);
    assert_eq!(no_caller.endpoints_matched, 0);
    assert_eq!(no_caller.functions_matched, 1, "the callee matched");
    assert_eq!(
        no_caller.unmatched.iter().collect::<Vec<_>>(),
        vec![&UnmatchedReason::NoCallerMatched]
    );

    let mixed = get(4, TaintDirection::Backward);
    assert_eq!(mixed.ports_declared, 2);
    assert_eq!(mixed.endpoints_matched, 0);
    assert_eq!(
        mixed.unmatched.iter().collect::<Vec<_>>(),
        vec![
            &UnmatchedReason::LocalNotFound("alsonope".to_string()),
            &UnmatchedReason::LocalNotFound("nope".to_string()),
        ],
        "each port's own reason survives, so `CTADL0004` can list both"
    );
}

/// The same model file is matched once per import, so the per-import stats are folded
/// together. Only the endpoint counts sum: `ports_declared` is a property of the file and
/// would otherwise be multiplied by the number of imports, re-inflating the count
/// `CTADL0100` prints.
#[test]
fn test_endpoint_stats_merge_across_imports() {
    use ctadl_ascent::models::EndpointStats;

    let import_a = EndpointStats {
        ports_declared: 2,
        endpoints_matched: 0,
        functions_matched: 0,
        unmatched: [UnmatchedReason::NoFunctionMatched].into_iter().collect(),
    };
    let import_b = EndpointStats {
        ports_declared: 2,
        endpoints_matched: 3,
        functions_matched: 3,
        unmatched: Default::default(),
    };
    let mut merged = EndpointStats::default();
    merged.merge(&import_a);
    merged.merge(&import_b);

    assert_eq!(merged.ports_declared, 2, "not 4");
    assert_eq!(
        merged.endpoints_matched, 3,
        "dead against one import, live against another, so live"
    );
    assert_eq!(merged.functions_matched, 3);
}

// ---------------------------------------------------------------------------
// A model port can name a real offset.
//
// Port access paths used to be a `Vec<&str>` stored verbatim and revived through a
// blanket `FromIterator<AsRef<str>>` that made *every* segment a `Symbol`. So
// `Argument(1).[8].deref` was `Symbol("[8]"), Symbol("deref")` and could never match
// the `Offset(8), Symbol("deref")` that pcode's `push_offset` emits — even though the
// docs and the JSON schema have advertised exactly that spelling all along.
// ---------------------------------------------------------------------------

/// A native (binary-frontend) program with one 2-parameter function per name.
fn native_program(names: &[&str]) -> ProgramInfo {
    use ctadl_ir::mir::call::{
        NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };
    use ctadl_ir::mir::{
        BasicBlockData, FunctionData, Functions, ParameterType, Program, Statement, StatementKind,
    };

    let functions: Vec<FunctionData> = names
        .iter()
        .map(|name| {
            let mut f = FunctionData::default();
            f.set_name((*name).to_string());
            f.params.parameters.push(ParameterType::ByVal);
            f.params.parameters.push(ParameterType::ByVal);
            let blocks = f.blocks.blocks_mut();
            let body = blocks.push(BasicBlockData::new(None));
            blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
            f
        })
        .collect();

    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: names
                .iter()
                .map(|name| {
                    (
                        NativeSimpleName((*name).into()),
                        NativeSignature((*name).into()),
                        NativeFunction((*name).into()),
                        NativeQualifiedName((*name).into()),
                    )
                })
                .collect(),
        },
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

/// A native (binary-frontend) program with one 2-parameter function `f`.
fn native_program_with_f() -> ProgramInfo {
    native_program(&["f"])
}

/// Loads one propagation generator and returns every access path in the resulting
/// summary batch, as `facts::Path`.
fn summary_paths_for(input: &str, output: &str) -> Vec<ctadl_ascent::facts::Path> {
    use serde_json::json;
    let program_info = native_program_with_f();
    let mut matches = ProgramModelMatches::default();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
    let model = json!({
        "find": "methods",
        "where": [{"constraint": "signature_match", "name": "f"}],
        "model": {"propagation": [{"input": input, "output": output}]}
    });
    ingest
        .encode_models(vec![model])
        .unwrap_or_else(|e| panic!("loading {input:?} -> {output:?}: {e}"));
    drop(ingest);
    assert!(
        !matches.propagations.is_empty(),
        "generator matched nothing for {input:?} -> {output:?}"
    );
    let mut paths: Vec<_> = matches
        .propagations
        .iter()
        .flat_map(|p| [p.dst.path, p.src.path])
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[test]
fn offset_port_produces_a_real_offset() {
    use ctadl_ir::mir::{Offset, PathSegment};

    let paths = summary_paths_for("Argument(1).[8].deref", "Return");
    let expected = ctadl_ascent::facts::Path::from_accesses([
        PathSegment::Offset(Offset(8)),
        PathSegment::symbol("deref"),
    ]);
    assert!(
        paths.contains(&expected),
        "expected a path with Offset(8), got: {:?}",
        paths.iter().map(|p| p.to_dot_string()).collect::<Vec<_>>()
    );

    // ... and it is NOT the symbol whose name happens to be "[8]", which is what every
    // segment used to become.
    let as_symbol = ctadl_ascent::facts::Path::from_accesses([
        PathSegment::symbol("[8]"),
        PathSegment::symbol("deref"),
    ]);
    assert_ne!(expected, as_symbol);
    assert!(!paths.contains(&as_symbol));
}

/// The other side of the same coin: an *escaped* bracketed name stays a symbol, so a
/// port can still name the synthetic array-element field the dex/jvm/lua frontends emit.
#[test]
fn escaped_bracketed_port_stays_a_symbol() {
    use ctadl_ir::mir::PathSegment;

    let paths = summary_paths_for(r"Argument(1).\[_elem_]", "Return");
    let expected = ctadl_ascent::facts::Path::from_accesses([PathSegment::symbol("[_elem_]")]);
    assert!(
        paths.contains(&expected),
        "expected Symbol(\"[_elem_]\"), got: {:?}",
        paths.iter().map(|p| p.to_dot_string()).collect::<Vec<_>>()
    );
    assert_eq!(expected.to_dot_string(), r".\[_elem_]");
}

// ---------------------------------------------------------------------------
// An endpoint keeps the access path its own model file declared.
//
// `cli::query` accumulates one batch per (import x model file) pair. Accumulating must not
// let one pair's trailing port path reach another pair's endpoint -- a source or sink that is
// silently widened, narrowed, or moved is indistinguishable from one the user wrote that way.
// See `removing-modelbuilders-plan.md` §2 for the two triggers pinned below.
// ---------------------------------------------------------------------------

/// One model file declaring one source on `func`, whose port carries `path`.
fn endpoint_model_file(kind: &str, func: &str, port: &str) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "find": "methods",
            "where": [{"constraint": "signature_match", "name": func}],
            "model": {"sources": [{"kind": kind, "port": port}]},
        })
    )
    .unwrap();
    file.flush().unwrap();
    file
}

/// Loads `(program, file)` pairs in the given order, accumulating them the way `cli::query`
/// does, and returns each matched endpoint's label paired with the access path it resolves to.
///
/// Only this accumulation harness is specific to how matches are represented; the fixtures
/// above and the assertions below are not.
fn accumulated_endpoint_paths(
    loads: &[(&ProgramInfo, &std::path::Path)],
) -> std::collections::BTreeMap<String, ctadl_ascent::facts::Path> {
    let mut acc = ProgramModelMatches::default();
    for (program_info, model_path) in loads {
        let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
        try_load_models(&match_index, model_path, &mut acc).expect("load");
    }
    acc.endpoints
        .iter()
        .map(|ep| (ep.label.to_string(), ep.path))
        .collect()
}

fn path_of(segments: &[&str]) -> ctadl_ascent::facts::Path {
    use ctadl_ir::mir::PathSegment;
    ctadl_ascent::facts::Path::from_accesses(segments.iter().map(|s| PathSegment::symbol(*s)))
}

/// Trigger 1: two model files, one program. Each file's ports are numbered from zero, so the
/// second file's path table shadows the first's and file A's source silently acquires file B's
/// path.
#[test]
fn two_model_files_keep_their_own_endpoint_paths() {
    let program_info = native_program(&["a", "b"]);
    let file_a = endpoint_model_file("A", "a", "Argument(0).headers");
    let file_b = endpoint_model_file("B", "b", "Argument(0).body.raw");

    let got = accumulated_endpoint_paths(&[
        (&program_info, file_a.path()),
        (&program_info, file_b.path()),
    ]);

    assert_eq!(got.get("A"), Some(&path_of(&["headers"])));
    assert_eq!(got.get("B"), Some(&path_of(&["body", "raw"])));
}

/// Trigger 2: one model file, two imports. Each import matches a different function, so the
/// same file produces two different append sequences and the same ids bind to different path
/// tables -- a single file colliding with itself, which is the bridging configuration.
#[test]
fn one_model_file_across_two_imports_keeps_its_endpoint_paths() {
    let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
    for (kind, func, port) in [
        ("A", "a", "Argument(0).headers"),
        ("B", "b", "Argument(0).body.raw"),
    ] {
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "find": "methods",
                "where": [{"constraint": "signature_match", "name": func}],
                "model": {"sources": [{"kind": kind, "port": port}]},
            })
        )
        .unwrap();
    }
    file.flush().unwrap();

    // Two imports, each containing only one of the two modeled functions.
    let import_a = native_program(&["a"]);
    let import_b = native_program(&["b"]);
    let got =
        accumulated_endpoint_paths(&[(&import_a, file.path()), (&import_b, file.path())]);

    assert_eq!(got.get("A"), Some(&path_of(&["headers"])));
    assert_eq!(got.get("B"), Some(&path_of(&["body", "raw"])));
}
