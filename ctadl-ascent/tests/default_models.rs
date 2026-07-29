//! The built-in default model files (`ctadl-ascent/src/models/defaults/`) and the VMT dispatch
//! that picks one per import.
//!
//! Two things are pinned here. First, that the dispatch is *language-aware*: a Lua import must
//! not pay for a match pass over the Java generators, and — the part that actually changes
//! results — a function whose bare name collides with another language's library function must
//! not pick up that language's summary. Second, that every shipped file still parses: the loader
//! hard-errors on unknown keys and on malformed access paths, so a stale default file breaks
//! *every* index of the language that selects it.

use ctadl_ascent::models::{DEFAULT_MODEL_FILES, try_load_default_models, try_load_jsonl_models};
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::Symbol;
use ctadl_ir::mir::call::{
    JavaClass, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction, NativeQualifiedName,
    NativeSignature, NativeSimpleName, VirtualMethodTable,
};
use ctadl_ir::mir::{
    BasicBlockData, FunctionData, Functions, ParameterType, Program, Statement, StatementKind,
};
use std::collections::BTreeSet;
use std::io::BufReader;

/// A 3-parameter function named `name`, so `Argument(0..2)` ports all have a formal to land on.
fn function(name: &str) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    for _ in 0..3 {
        f.params.parameters.push(ParameterType::ByVal);
    }
    let blocks = f.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(None));
    blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
    f
}

/// The two probe names. `strcpy` is modeled by `native-index.jsonl` and by nothing else;
/// `format` by `lua-index.jsonl` and by nothing else. `toString` on `Ljava/lang/String;` is the
/// Java probe, but it needs a class, so it is added per-VMT below.
const NATIVE_PROBE: &str = "strcpy";
const LUA_PROBE: &str = "format";
const JAVA_PROBE: &str = "Ljava/lang/String;->toString()Ljava/lang/String;";
/// Matches the `List.add` collection generator, whose port names the array element.
const JAVA_LIST_ADD: &str = "Ljava/util/ArrayList;->add(Ljava/lang/Object;)Z";

fn program(vmt: VirtualMethodTable, names: &[&str]) -> ProgramInfo {
    ProgramInfo {
        vmt,
        program: Program::new(Functions::new(names.iter().map(|n| function(n)))),
        ..Default::default()
    }
}

fn java_program() -> ProgramInfo {
    let methods = vec![
        (
            JavaClass("Ljava/lang/String;".into()),
            JavaSimpleName("toString".into()),
            JavaSignature("()Ljava/lang/String;".into()),
            JavaMethod(JAVA_PROBE.into()),
        ),
        // Bare name collides with a libc function the native defaults model.
        (
            JavaClass("Lcom/example/C;".into()),
            JavaSimpleName(NATIVE_PROBE.into()),
            JavaSignature("(I)V".into()),
            JavaMethod("Lcom/example/C;->strcpy(I)V".into()),
        ),
        // ... and with a Lua stdlib function the Lua defaults model.
        (
            JavaClass("Lcom/example/C;".into()),
            JavaSimpleName(LUA_PROBE.into()),
            JavaSignature("(I)V".into()),
            JavaMethod("Lcom/example/C;->format(I)V".into()),
        ),
        // A collection write, so the array-element port has something to match.
        (
            JavaClass("Ljava/util/ArrayList;".into()),
            JavaSimpleName("add".into()),
            JavaSignature("(Ljava/lang/Object;)Z".into()),
            JavaMethod(JAVA_LIST_ADD.into()),
        ),
    ];
    program(
        VirtualMethodTable::Java {
            methods,
            hierarchy: Default::default(),
        },
        &[
            JAVA_PROBE,
            "Lcom/example/C;->strcpy(I)V",
            "Lcom/example/C;->format(I)V",
            JAVA_LIST_ADD,
        ],
    )
}

fn native_program() -> ProgramInfo {
    let methods = [NATIVE_PROBE, LUA_PROBE, JAVA_PROBE]
        .into_iter()
        .map(|n| {
            (
                NativeSimpleName(n.into()),
                NativeSignature(format!("{n}()").into()),
                NativeFunction(n.into()),
                NativeQualifiedName(n.into()),
            )
        })
        .collect();
    program(
        VirtualMethodTable::Native { methods },
        &[NATIVE_PROBE, LUA_PROBE, JAVA_PROBE],
    )
}

fn lua_program() -> ProgramInfo {
    let functions: Vec<(Symbol, Symbol)> = [NATIVE_PROBE, LUA_PROBE, JAVA_PROBE]
        .into_iter()
        .map(|n| (Symbol::from(n), Symbol::from(n)))
        .collect();
    program(
        VirtualMethodTable::Lua {
            methods: Vec::new(),
            functions,
            externals: Vec::new(),
            hierarchy: Default::default(),
        },
        &[NATIVE_PROBE, LUA_PROBE, JAVA_PROBE],
    )
}

/// Flowy: no method table at all.
fn unknown_program() -> ProgramInfo {
    program(
        VirtualMethodTable::Unknown,
        &[NATIVE_PROBE, LUA_PROBE, JAVA_PROBE],
    )
}

/// The set of function ids that got at least one summary row out of the defaults.
fn summarized(program_info: &ProgramInfo) -> BTreeSet<String> {
    let batch = try_load_default_models(program_info).expect("loading default models");
    batch
        .summary
        .iter_summaries()
        .map(|(func, ..)| func.to_string())
        .collect()
}

#[test]
fn java_import_loads_only_the_java_defaults() {
    let summarized = summarized(&java_program());
    assert!(
        summarized.contains(JAVA_PROBE),
        "expected a Java summary, got {summarized:?}"
    );
    assert!(
        !summarized.contains("Lcom/example/C;->strcpy(I)V"),
        "a Java method simply named `strcpy` picked up the native defaults: {summarized:?}"
    );
    assert!(
        !summarized.contains("Lcom/example/C;->format(I)V"),
        "a Java method simply named `format` picked up the Lua defaults: {summarized:?}"
    );
}

#[test]
fn native_import_loads_only_the_native_defaults() {
    let summarized = summarized(&native_program());
    assert!(
        summarized.contains(NATIVE_PROBE),
        "expected a native summary, got {summarized:?}"
    );
    assert!(
        !summarized.contains(LUA_PROBE),
        "a native function named `format` picked up the Lua defaults: {summarized:?}"
    );
}

#[test]
fn lua_import_loads_only_the_lua_defaults() {
    let summarized = summarized(&lua_program());
    assert!(
        summarized.contains(LUA_PROBE),
        "expected a Lua summary, got {summarized:?}"
    );
    assert!(
        !summarized.contains(NATIVE_PROBE),
        "a Lua function named `strcpy` picked up the native defaults: {summarized:?}"
    );
}

/// Flowy has no VMT, so there is no default file to pick and nothing is loaded. The program
/// deliberately contains functions every other default file would model.
#[test]
fn unknown_vmt_loads_no_defaults() {
    let batch = try_load_default_models(&unknown_program()).expect("loading default models");
    assert_eq!(batch.summary.num_rows(), 0);
    assert_eq!(batch.endpoint.endpoints.num_rows(), 0);
}

/// The cheap guard against drift. Every shipped file is parsed against a program of each VMT
/// shape — the loader's hard errors (unknown constraint keys, malformed access paths) do not
/// depend on which functions match, so any one program would do, but running all four also
/// proves no file panics on a table shape it was not written for.
#[test]
fn every_shipped_default_file_parses() {
    let programs = [
        ("java", java_program()),
        ("native", native_program()),
        ("lua", lua_program()),
        ("unknown", unknown_program()),
    ];
    for (name, contents) in DEFAULT_MODEL_FILES {
        for (vmt_name, program_info) in &programs {
            let result = try_load_jsonl_models(program_info, BufReader::new(*contents));
            assert!(
                result.is_ok(),
                "{name} failed to load against a {vmt_name} program: {:?}",
                result.err()
            );
        }
    }
}

/// The Java collection generators must name the array element the dex/jvm frontends actually
/// emit -- `PathSegment::Symbol("[]")`, spelled `.\[]` in a port and `"\\[]"` in JSON.
///
/// Getting this wrong fails loudly in one direction and silently in the other. Unescaped,
/// `Argument(0).[]` is `InvalidOffset("")`, a hard load error caught by
/// `every_shipped_default_file_parses`. But the synthetic `.rep` these generators used to carry
/// parses fine and simply matches nothing any frontend writes, which is how `List.add` /
/// `List.get` chains ended up composing only with each other. Only an assertion on the decoded
/// segment catches a silent regression back to that.
#[test]
fn java_collection_generators_name_the_real_array_element() {
    use ctadl_ir::mir::PathSegment;

    let batch = try_load_default_models(&java_program()).expect("loading default models");
    let paths: Vec<_> = batch.summary.aps.build_ap_map().into_values().collect();
    let element = PathSegment::symbol("[]");
    assert!(
        paths.iter().any(|p| p.iter().any(|s| *s == element)),
        "no port in the Java defaults decodes to Symbol(\"[]\"); got {:?}",
        paths
            .iter()
            .map(|p| p.to_dot_string())
            .collect::<BTreeSet<_>>()
    );
    // And nothing still names the synthetic field it replaced.
    let rep = PathSegment::symbol("rep");
    assert!(
        !paths.iter().any(|p| p.iter().any(|s| *s == rep)),
        "a `.rep` port is back; no frontend emits that field"
    );
}

/// An access-path escape in a `.jsonl` file needs *two* levels of quoting -- one for JSON, one
/// for the path grammar -- so `\[]` is written `"\\[]"`. Writing `"\\\\[]"` gets past both the
/// JSON parser and the path parser and produces a `Symbol` named `\[]`, which matches nothing.
/// That is a silent failure, and file-generating scripts make it easily.
///
/// Checked textually, on the JSON-decoded port, because the semantic check above can only see
/// generators that matched the toy program; this sees every port in every shipped file.
#[test]
fn no_shipped_default_port_is_over_escaped() {
    for (name, contents) in DEFAULT_MODEL_FILES {
        for line in String::from_utf8_lossy(contents).lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            let props = value["model"]["propagation"]
                .as_array()
                .into_iter()
                .flatten();
            for prop in props {
                for key in ["input", "output"] {
                    let port = prop[key].as_str().expect("port is a string");
                    assert!(
                        !port.contains(r"\\"),
                        "{name}: port {port:?} is over-escaped -- \
                         a literal backslash reaches the path grammar and escapes the next \
                         character, so this names a field nothing emits"
                    );
                }
            }
        }
    }
}

/// The defaults are propagation only: CTADL ships no default sources or sinks, and shipping any
/// would report the cross product of every default source and sink in every program (source and
/// sink `kind`s do not have to pair).
#[test]
fn no_shipped_default_declares_an_endpoint() {
    for (name, contents) in DEFAULT_MODEL_FILES {
        for line in String::from_utf8_lossy(contents).lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            let model = &value["model"];
            assert!(
                model.get("sources").is_none() && model.get("sinks").is_none(),
                "{name} declares an endpoint, which `cli::index` discards: {line}"
            );
        }
    }
}

/// Comments and blank lines are what let a default file explain itself. They must not shift the
/// generator index, which names the generator in error messages and keys `endpoint_stats`.
#[test]
fn jsonl_comments_are_skipped_without_consuming_an_index() {
    let program_info = native_program();
    let real = r#"{"find":"methods","where":[{"constraint":"signature_match","names":["strcpy"]}],"model":{"propagation":[{"input":"Argument(1)","output":"Argument(0)"}]}}"#;
    let with_comments = format!("// leading commentary\n\n  // indented\n{real}\n\n");

    let bare = try_load_jsonl_models(&program_info, BufReader::new(real.as_bytes())).unwrap();
    let commented =
        try_load_jsonl_models(&program_info, BufReader::new(with_comments.as_bytes())).unwrap();
    assert_eq!(bare.summary.num_rows(), commented.summary.num_rows());
    assert!(bare.summary.num_rows() > 0);
}
