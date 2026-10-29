//! The built-in default model files (`ctadl-ascent/src/models/defaults/`) and the VMT dispatch
//! that picks one per import.
//!
//! Two things are pinned here. First, that the dispatch is *language-aware*: a Lua import must
//! not pay for a match pass over the Java generators, and — the part that actually changes
//! results — a function whose bare name collides with another language's library function must
//! not pick up that language's summary. Second, that every shipped file still parses: the loader
//! hard-errors on unknown keys and on malformed access paths, so a stale default file breaks
//! *every* index of the language that selects it.

use ctadl_ascent::models::{
    DEFAULT_MODEL_FILES, ImportScope, ProgramMatchIndex, ProgramModelMatches,
    try_load_default_models, try_load_jsonl_models,
};
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::Symbol;
use ctadl_ir::mir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, JavaClass, JavaMethod, JavaSignature,
    JavaSimpleName, NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName,
    VirtualMethodTable,
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
        f.params.parameters.push(ParameterType::SharedFields);
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
            interfaces: Default::default(),
            abstract_methods: Default::default(),
            natives: Default::default(),
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

/// The .NET probes: `String.Concat`, and two ports only a pointer reaches, a struct
/// enumerator's `Current` and a `ValueTuple` constructor's field.
const CIL_CONCAT: &str =
    "[System.Private.CoreLib]System.String::Concat(System.String,System.String)->System.String";
const CIL_CURRENT: &str =
    "[System.Private.CoreLib]System.Collections.Generic.List`1/Enumerator::get_Current()->!0";
const CIL_TUPLE: &str = "[System.Private.CoreLib]System.ValueTuple`2::.ctor(!0,!1)->System.Void";

fn cil_program() -> ProgramInfo {
    let row = |cls: &str, simple: &str, sig: &str, fid: &str| {
        (
            CilClass(cls.into()),
            CilSimpleName(simple.into()),
            CilSignature(sig.into()),
            CilMethod(fid.into()),
        )
    };
    let methods = vec![
        row(
            "[System.Private.CoreLib]System.String",
            "Concat",
            "(System.String,System.String)->System.String",
            CIL_CONCAT,
        ),
        row(
            "[System.Private.CoreLib]System.Collections.Generic.List`1/Enumerator",
            "get_Current",
            "()->!0",
            CIL_CURRENT,
        ),
        row(
            "[System.Private.CoreLib]System.ValueTuple`2",
            ".ctor",
            "(!0,!1)->System.Void",
            CIL_TUPLE,
        ),
        // Bare names that collide with the native and Lua defaults' functions.
        row(
            "[App]C",
            NATIVE_PROBE,
            "()->System.Void",
            "[App]C::strcpy()->System.Void",
        ),
        row(
            "[App]C",
            LUA_PROBE,
            "()->System.Void",
            "[App]C::format()->System.Void",
        ),
    ];
    let names: Vec<String> = methods.iter().map(|m| m.3.to_string()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let vmt = VirtualMethodTable::Cil {
        methods: methods.clone(),
        hierarchy: Default::default(),
        interfaces: Default::default(),
        abstract_methods: Default::default(),
        pinvokes: Default::default(),
    };
    program(vmt, &names)
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
    let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    try_load_default_models(&match_index, &mut matches).expect("loading default models");
    matches
        .propagations
        .iter()
        .map(|p| p.function.to_string())
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

#[test]
fn cil_import_loads_only_the_cil_defaults() {
    let summarized = summarized(&cil_program());
    for probe in [CIL_CONCAT, CIL_CURRENT, CIL_TUPLE] {
        assert!(
            summarized.contains(probe),
            "expected a .NET summary for {probe}, got {summarized:?}"
        );
    }
    for other in [
        "[App]C::strcpy()->System.Void",
        "[App]C::format()->System.Void",
    ] {
        assert!(
            !summarized.contains(other),
            "{other} picked up another language's defaults: {summarized:?}"
        );
    }
}

/// A struct receiver arrives as a pointer, so a struct enumerator's `Current` reads
/// `Argument(0).deref.\[]`; and a `ValueTuple` field is a symbol full of dots, which the port
/// has to escape. Both are silent if wrong: the port parses and names a path no frontend writes.
#[test]
fn cil_ports_name_the_pointee_and_the_tuple_field() {
    use ctadl_ir::mir::PathSegment;

    let program_info = cil_program();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    try_load_default_models(&match_index, &mut matches).expect("loading default models");
    let src_of = |fid: &str| -> Vec<Vec<PathSegment>> {
        matches
            .propagations
            .iter()
            .filter(|p| p.function.to_string() == fid)
            .map(|p| p.src.path.iter().cloned().collect())
            .collect()
    };
    let dst_of = |fid: &str| -> Vec<Vec<PathSegment>> {
        matches
            .propagations
            .iter()
            .filter(|p| p.function.to_string() == fid)
            .map(|p| p.dst.path.iter().cloned().collect())
            .collect()
    };
    let deref = PathSegment::symbol(ctadl_ir::mir::DEREF_FIELD);
    assert!(
        src_of(CIL_CURRENT).contains(&vec![deref.clone(), PathSegment::symbol("[]")]),
        "get_Current does not read deref.[]: {:?}",
        src_of(CIL_CURRENT)
    );
    let item1 = PathSegment::symbol("<[System.Private.CoreLib]System.ValueTuple`2::Item1>");
    assert!(
        dst_of(CIL_TUPLE).contains(&vec![deref, item1]),
        "the ValueTuple constructor does not write deref.Item1: {:?}",
        dst_of(CIL_TUPLE)
    );
}

/// Flowy has no VMT, so there is no default file to pick and nothing is loaded. The program
/// deliberately contains functions every other default file would model.
#[test]
fn unknown_vmt_loads_no_defaults() {
    let program_info = unknown_program();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    try_load_default_models(&match_index, &mut matches).expect("loading default models");
    assert!(matches.propagations.is_empty());
    assert!(matches.endpoints.is_empty());
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
        ("cil", cil_program()),
        ("unknown", unknown_program()),
    ];
    for (name, contents) in DEFAULT_MODEL_FILES {
        for (vmt_name, program_info) in &programs {
            let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
            let mut matches = ProgramModelMatches::default();
            let result =
                try_load_jsonl_models(&match_index, BufReader::new(*contents), &mut matches);
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

    let program_info = java_program();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut matches = ProgramModelMatches::default();
    try_load_default_models(&match_index, &mut matches).expect("loading default models");
    let paths: Vec<_> = matches
        .propagations
        .iter()
        .flat_map(|p| [p.dst.path, p.src.path])
        .collect();
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

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let mut bare = ProgramModelMatches::default();
    try_load_jsonl_models(&match_index, BufReader::new(real.as_bytes()), &mut bare).unwrap();
    let mut commented = ProgramModelMatches::default();
    try_load_jsonl_models(
        &match_index,
        BufReader::new(with_comments.as_bytes()),
        &mut commented,
    )
    .unwrap();
    assert_eq!(bare.propagations.len(), commented.propagations.len());
    assert!(!bare.propagations.is_empty());
}
