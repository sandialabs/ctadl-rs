//! The built-in default model files (`ctadl-ascent/src/models/defaults/`) and the VMT dispatch
//! that picks one per import.
//!
//! Two things are pinned here. First, that the dispatch is *language-aware*: a Lua import must
//! not pay for a match pass over the Java generators, and — the part that actually changes
//! results — a function whose bare name collides with another language's library function must
//! not pick up that language's summary. Second, that every shipped file still parses: the loader
//! hard-errors on unknown keys and on malformed access paths, so a stale default file breaks
//! *every* index of the language that selects it.
//!
//! What each default matches on the fixture programs is also pinned, in `default_models.golden`;
//! a deliberate change to a default regenerates it with `CTADL_BLESS=1`.

use ctadl_ascent::models::test_vmt;
use ctadl_ascent::models::{
    DEFAULT_DSL_MODELS, DispatchKeys, ImportScope, ProgramMatchIndex, ProgramModelMatches,
    try_load_default_models, try_load_models,
};
use ctadl_ir::mir::ProgramInfo;
use ctadl_ir::mir::Symbol;
use ctadl_ir::mir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, Dispatch, JavaClass, JavaMethod,
    JavaSignature, JavaSimpleName, VirtualMethodTable,
};
use ctadl_ir::mir::{
    BasicBlockData, FunctionData, Functions, ParameterType, Program, Statement, StatementKind,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

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

/// Java call sites whose static signatures exercise every shape a dispatch default takes: a
/// modelled one (`toString`), an element-level container read and write, a discard (`size`), an
/// `inline` (`close`), and a closure-shaped marker (`Runnable.run`).
///
/// The equivalence test below needs these because a `find: "dispatch"` generator matches call
/// *signatures*, not methods: with no call sites the dispatch universe is empty and every
/// dispatch default matches nothing, which would pass while proving nothing.
const DISPATCH_SITES: &[(&str, &str, &str)] = &[
    ("Ljava/lang/Object;", "toString", "()Ljava/lang/String;"),
    ("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;"),
    (
        "Ljava/util/Map;",
        "put",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
    ),
    ("Ljava/util/Collection;", "size", "()I"),
    ("Ljava/io/Closeable;", "close", "()V"),
    ("Ljava/lang/Runnable;", "run", "()V"),
];

/// A function whose body is one `JavaCall` per [`DISPATCH_SITES`] entry.
fn dispatch_caller() -> FunctionData {
    use ctadl_ir::Idx;
    use ctadl_ir::builder::FunctionBuilder;
    use ctadl_ir::mir::Exp;
    use ctadl_ir::mir::call::{CallStyle, JavaDispatch};

    let mut f = FunctionData {
        name: "Lcom/example/Caller;->run()V".to_string(),
        ..Default::default()
    };
    f.params.parameters.push(ParameterType::Shared);
    let mut fb = FunctionBuilder::new(&mut f);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    for (cls, name, desc) in DISPATCH_SITES {
        let recv = b.new_local_var("recv");
        let result = b.new_local_var("result");
        b.create_call(
            CallStyle::JavaCall {
                receiver: recv,
                cls: (*cls).into(),
                simple_name: (*name).into(),
                descriptor: (*desc).into(),
                dispatch: JavaDispatch::Interface,
                super_start: None,
            },
            vec![result],
            vec![Exp::Variable(
                b.new_param_var(ctadl_ir::mir::ParameterIdx::new(0)),
            )],
        );
    }
    f
}

/// The two probe names. `strcpy` is modeled by `native-index.ctadl` and by nothing else;
/// `format` by `lua-index.ctadl` and by nothing else. `toString` on `Ljava/lang/String;` is the
/// Java probe, but it needs a class, so it is added per-VMT below.
const NATIVE_PROBE: &str = "strcpy";
const LUA_PROBE: &str = "format";
/// An ARM unwinder the native defaults mark `skip-analysis`.
const NATIVE_UNWINDER: &str = "Unwind_Resume";
const JAVA_PROBE: &str = "Ljava/lang/String;->toString()Ljava/lang/String;";
/// Matches the `List.add` collection generator, whose port names the array element.
const JAVA_LIST_ADD: &str = "Ljava/util/ArrayList;->add(Ljava/lang/Object;)Z";

fn program(vmt: VirtualMethodTable, names: &[&str]) -> ProgramInfo {
    program_with(vmt, names, Vec::new())
}

/// [`program`], plus functions built elsewhere -- the call-site carrier the dispatch defaults
/// need.
fn program_with(vmt: VirtualMethodTable, names: &[&str], extra: Vec<FunctionData>) -> ProgramInfo {
    let funcs = names
        .iter()
        .map(|n| function(n))
        .chain(extra)
        .collect::<Vec<_>>();
    ProgramInfo {
        vmt,
        program: Program::new(Functions::new(funcs)),
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
    program_with(
        test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
            methods,
            hierarchy: Default::default(),
            interfaces: Default::default(),
            abstract_methods: Default::default(),
            natives: Default::default(),
        })),
        &[
            JAVA_PROBE,
            "Lcom/example/C;->strcpy(I)V",
            "Lcom/example/C;->format(I)V",
            JAVA_LIST_ADD,
        ],
        vec![dispatch_caller()],
    )
}

fn native_program() -> ProgramInfo {
    let names = [NATIVE_PROBE, LUA_PROBE, JAVA_PROBE, NATIVE_UNWINDER];
    let signatures = names.map(|n| format!("{n}()"));
    program(
        test_vmt::native(
            names
                .iter()
                .zip(&signatures)
                .map(|(n, sig)| (*n, sig.as_str(), *n, *n)),
        ),
        &[NATIVE_PROBE, LUA_PROBE, JAVA_PROBE, NATIVE_UNWINDER],
    )
}

fn lua_program() -> ProgramInfo {
    let functions: Vec<(Symbol, Symbol)> = [NATIVE_PROBE, LUA_PROBE, JAVA_PROBE]
        .into_iter()
        .map(|n| (Symbol::from(n), Symbol::from(n)))
        .collect();
    program(
        test_vmt::lua(Vec::new(), &functions, &[], Default::default()),
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
    let vmt = test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Cil {
        methods: methods.clone(),
        hierarchy: Default::default(),
        interfaces: Default::default(),
        abstract_methods: Default::default(),
        pinvokes: Default::default(),
    }));
    program(vmt, &names)
}

/// Flowy: no method table at all.
fn unknown_program() -> ProgramInfo {
    let mut info = program(
        VirtualMethodTable::default(),
        &[NATIVE_PROBE, LUA_PROBE, JAVA_PROBE],
    );
    info.vmt = VirtualMethodTable::named_by_ir(&info.program);
    info
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

/// The cheap guard against drift. Every shipped `.ctadl` is loaded the way any model file is,
/// by path, against a program of each VMT shape — a file's load errors do not depend on which
/// functions match, so any one program would do, but running all of them also proves no file
/// panics on a table shape it was not written for.
#[test]
fn every_shipped_default_file_parses() {
    let programs = [
        ("java", java_program()),
        ("native", native_program()),
        ("lua", lua_program()),
        ("cil", cil_program()),
        ("unknown", unknown_program()),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/models/defaults");
    for (name, _) in DEFAULT_DSL_MODELS {
        for (vmt_name, program_info) in &programs {
            let match_index = ProgramMatchIndex::new(program_info, ImportScope::unknown());
            let mut matches = ProgramModelMatches::default();
            let result = try_load_models(&match_index, dir.join(name), &mut matches);
            assert!(
                result.is_ok(),
                "{name} failed to load against a {vmt_name} program: {:?}",
                result.err()
            );
        }
    }
}

/// The Java collection rules must name the array element the dex/jvm frontends actually emit --
/// `PathSegment::Symbol("[]")`, spelled `."[]"` in a port.
///
/// Getting this wrong fails silently: a synthetic field such as `.rep` parses fine and simply
/// matches nothing any frontend writes, so `List.add` / `List.get` chains would compose only
/// with each other. Only an assertion on the decoded segment catches that.
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
    // And nothing names the synthetic `.rep` field.
    let rep = PathSegment::symbol("rep");
    assert!(
        !paths.iter().any(|p| p.iter().any(|s| *s == rep)),
        "a `.rep` port is back; no frontend emits that field"
    );
}

/// The defaults are propagation only: CTADL ships no default sources or sinks, and shipping any
/// would report the cross product of every default source and sink in every program (source and
/// sink `kind`s do not have to pair).
#[test]
fn no_shipped_default_declares_an_endpoint() {
    use ctadl_ascent::models::portcullis::{self, ast::HeadKind};

    for (name, source) in DEFAULT_DSL_MODELS {
        let file = portcullis::PortcullisFile::from_text(*name, *source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for rule in &file.program.rules {
            for head in &rule.heads {
                assert!(
                    !matches!(head.kind, HeadKind::Source { .. } | HeadKind::Sink { .. }),
                    "{name} declares an endpoint, which `cli::index` discards: {head:?}"
                );
            }
        }
    }
}

/// Comments and blank lines are what let a JSONL model file explain itself. They must not shift
/// the generator index, which names the generator in error messages and keys `endpoint_stats`.
#[test]
fn jsonl_comments_are_skipped_without_consuming_an_index() {
    let program_info = native_program();
    let real = r#"{"find":"methods","where":[{"constraint":"signature_match","names":["strcpy"]}],"model":{"propagation":[{"input":"Argument(1)","output":"Argument(0)"}]}}"#;
    let with_comments = format!("// leading commentary\n\n  // indented\n{real}\n\n");

    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let load = |text: &str| {
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .expect("temp file");
        file.write_all(text.as_bytes()).expect("write");
        file.flush().expect("flush");
        let mut out = ProgramModelMatches::default();
        let report = try_load_models(&match_index, file.path(), &mut out).unwrap();
        (out, report)
    };
    let (bare, _) = load(real);
    let (commented, report) = load(&with_comments);
    assert_eq!(bare.propagations.len(), commented.propagations.len());
    assert!(!bare.propagations.is_empty());
    assert_eq!(
        report.propagation_stats.keys().copied().collect::<Vec<_>>(),
        vec![0],
        "the generator after the comments is still generator 0"
    );
}

/// What each shipped default matches on each fixture program. `CTADL_BLESS=1` rewrites it from
/// what the defaults match now.
const DEFAULTS_GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/integration/default_models.golden"
);

/// The fixture programs the defaults are matched against.
fn default_fixtures() -> [(&'static str, ProgramInfo); 5] {
    [
        ("java", java_program()),
        ("native", native_program()),
        ("lua", lua_program()),
        ("cil", cil_program()),
        ("unknown", unknown_program()),
    ]
}

/// What a load matched, as the rows [`DEFAULTS_GOLDEN_PATH`] records: propagations, access
/// paths, dispatch dispositions, closure-shaped keys and skipped bodies.
///
/// Sets, because the golden is not about the order a load visits in, and a
/// duplicate row is inert (codegen pushes the same summary twice). A disposition's port list
/// keeps its order. Dispatch models are compared by disposition only: provenance records which
/// file and which generator or rule a model came from, so two loaders *must* differ there.
fn default_rows(m: &ProgramModelMatches) -> BTreeSet<String> {
    use ctadl_ascent::models::matches::{Disposition, ModelPort};
    let port = |p: &ModelPort| {
        let index = p.index.map(|i| format!("({i})")).unwrap_or_default();
        format!("{:?}{index}{}", p.tag, p.path.to_dot_string())
    };
    let ports = |ps: &[(ModelPort, ModelPort)]| {
        ps.iter()
            .map(|(dst, src)| format!("{} <- {}", port(dst), port(src)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = BTreeSet::new();
    for p in &m.propagations {
        let dst = ModelPort {
            tag: p.dst.tag,
            index: p.dst.index,
            path: p.dst.path,
        };
        let src = ModelPort {
            tag: p.src.tag,
            index: p.src.index,
            path: p.src.path,
        };
        out.insert(format!(
            "propagation: {}: {} <- {}",
            p.function,
            port(&dst),
            port(&src)
        ));
    }
    for p in &m.access_paths {
        out.insert(format!("access_path: {}", p.to_dot_string()));
    }
    for ((cls, name, desc), model) in &m.dispatch {
        let disposition = match &model.disposition {
            Disposition::Inline(ps) => format!("inline[{}]", ports(ps)),
            Disposition::Model(ps) => format!("model[{}]", ports(ps)),
            Disposition::Skip => "skip".to_string(),
        };
        out.insert(format!("dispatch: {cls}->{name}{desc}: {disposition}"));
    }
    for (cls, name, desc) in &m.closure_shaped {
        out.insert(format!("closure_shaped: {cls}->{name}{desc}"));
    }
    for f in &m.skip_analysis {
        out.insert(format!("skip_analysis: {f}"));
    }
    out
}

/// The index `program` is matched under, with the dispatch universe, so a `callsig` rule has
/// something to match. Built unconditionally: a program with no Java call sites simply yields
/// no keys.
fn default_index<'a>(program: &'a ProgramInfo, keys: &'a DispatchKeys) -> ProgramMatchIndex<'a> {
    ProgramMatchIndex::new_with_dispatch(program, ImportScope::unknown(), Some(keys))
}

/// The golden's key for `default` on fixture `program`.
fn default_key(default: &str, program: &str) -> String {
    format!("{default} on {program}")
}

/// Parses [`DEFAULTS_GOLDEN_PATH`]: `== <default> on <program>`, then that load's rows, each
/// indented four spaces. Lines starting with `#` are comments.
fn parse_defaults_golden(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(key) = line.strip_prefix("== ") {
            assert!(
                out.insert(key.to_string(), BTreeSet::new()).is_none(),
                "{DEFAULTS_GOLDEN_PATH}:{}: `{key}` appears twice",
                n + 1
            );
            current = Some(key.to_string());
        } else if let (Some(key), Some(row)) = (&current, line.strip_prefix("    ")) {
            if row != "(nothing)" {
                out.get_mut(key).unwrap().insert(row.to_string());
            }
        } else {
            panic!("{DEFAULTS_GOLDEN_PATH}:{}: unexpected line", n + 1);
        }
    }
    out
}

/// Each default an index loads matches, on every fixture program, exactly what
/// [`DEFAULTS_GOLDEN_PATH`] records. Run with `CTADL_BLESS=1` after a deliberate change to a
/// default to rewrite the golden, and review its diff.
///
/// The fixtures include Java call sites for every dispatch shape, so a `callsig` rule the
/// `.ctadl` dropped is noticed: an index would otherwise silently lose the hybrid call graph for
/// the language whose defaults dropped it.
#[test]
fn the_defaults_match_their_golden() {
    use ctadl_ascent::models::portcullis;

    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, source) in DEFAULT_DSL_MODELS {
        let file = portcullis::PortcullisFile::from_text(*name, *source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let set = portcullis::PortcullisModelSet { files: vec![file] };
        for (program_name, program) in &default_fixtures() {
            let keys = DispatchKeys::from_program(&program.program);
            let index = default_index(program, &keys);
            let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::All);
            let mut matches = ProgramModelMatches::default();
            matcher.observe_import(&index, &mut matches);
            matcher
                .finish(&mut matches)
                .unwrap_or_else(|e| panic!("{name} on {program_name}: {e}"));
            actual.insert(default_key(name, program_name), default_rows(&matches));
        }
    }

    if std::env::var_os("CTADL_BLESS").is_some() {
        std::fs::write(DEFAULTS_GOLDEN_PATH, render_defaults_golden(&actual))
            .unwrap_or_else(|e| panic!("writing {DEFAULTS_GOLDEN_PATH}: {e}"));
        return;
    }
    let text = std::fs::read_to_string(DEFAULTS_GOLDEN_PATH)
        .unwrap_or_else(|e| panic!("reading {DEFAULTS_GOLDEN_PATH}: {e}"));
    let golden = parse_defaults_golden(&text);
    let mut diff = Vec::new();
    for key in golden.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let empty = BTreeSet::new();
        let expected = golden.get(key).unwrap_or(&empty);
        let now = actual.get(key).unwrap_or(&empty);
        diff.extend(
            expected
                .difference(now)
                .map(|r| format!("{key}: golden only  {r}")),
        );
        diff.extend(
            now.difference(expected)
                .map(|r| format!("{key}: now only     {r}")),
        );
        if !golden.contains_key(key) || !actual.contains_key(key) {
            diff.push(format!("{key}: in only one of the golden and this run"));
        }
    }
    assert!(
        diff.is_empty(),
        "the defaults do not match {DEFAULTS_GOLDEN_PATH}; if the change is deliberate, re-run \
         with CTADL_BLESS=1 and review the diff:\n{}",
        diff.join("\n")
    );
}

/// [`DEFAULTS_GOLDEN_PATH`]'s text for `rows`, in the format [`parse_defaults_golden`] reads.
fn render_defaults_golden(rows: &BTreeMap<String, BTreeSet<String>>) -> String {
    let mut out = String::from(
        "# What each shipped default matches on each fixture program in default_models.rs.\n\
         # Regenerate with CTADL_BLESS=1 cargo test -p ctadl-ascent --test integration \\\n\
         #   the_defaults_match_their_golden, and review the diff.\n",
    );
    for (key, set) in rows {
        out.push_str(&format!("\n== {key}\n"));
        if set.is_empty() {
            out.push_str("    (nothing)\n");
        }
        for row in set {
            out.push_str(&format!("    {row}\n"));
        }
    }
    out
}

/// The native defaults keep the ARM unwinders' bodies out of the index, and it is the `.ctadl`
/// an index loads that has to say so.
#[test]
fn the_native_portcullis_defaults_skip_the_unwinders() {
    use ctadl_ascent::models::portcullis;

    let program_info = native_program();
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let (name, source) = DEFAULT_DSL_MODELS
        .iter()
        .find(|(name, _)| *name == "native-index.ctadl")
        .expect("a native DSL default");
    let file = portcullis::PortcullisFile::from_text(name, *source)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Index);
    let mut out = ProgramModelMatches::default();
    matcher.observe_import(&match_index, &mut out);
    matcher.finish(&mut out).expect("finish");
    assert_eq!(
        out.skip_analysis
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        vec![NATIVE_UNWINDER.to_string()]
    );
}

/// `memcpy` and `memset` are skipped, under their MIPS `.pic.` stub names too, and each keeps a
/// model. The rest of the copy family is analyzed.
#[test]
fn the_native_portcullis_defaults_skip_memcpy_and_memset() {
    use ctadl_ascent::models::portcullis;

    let names = [
        "memcpy",
        ".pic.memcpy",
        "memset",
        ".pic.memset",
        "memmove",
        "bcopy",
    ];
    let signatures = names.map(|n| format!("{n}()"));
    let program_info = program(
        test_vmt::native(
            names
                .iter()
                .zip(&signatures)
                .map(|(n, sig)| (*n, sig.as_str(), *n, *n)),
        ),
        &names,
    );
    let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
    let (name, source) = DEFAULT_DSL_MODELS
        .iter()
        .find(|(name, _)| *name == "native-index.ctadl")
        .expect("a native DSL default");
    let file = portcullis::PortcullisFile::from_text(name, *source)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let set = portcullis::PortcullisModelSet { files: vec![file] };
    let mut matcher = portcullis::PortcullisMatcher::new(&set, portcullis::Phase::Index);
    let mut out = ProgramModelMatches::default();
    matcher.observe_import(&match_index, &mut out);
    matcher.finish(&mut out).expect("finish");
    let skipped = out
        .skip_analysis
        .iter()
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>();
    let expected = [".pic.memcpy", ".pic.memset", "memcpy", "memset"]
        .map(String::from)
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(skipped, expected);
    for f in &expected {
        assert!(
            out.propagations.iter().any(|p| p.function.to_string() == *f),
            "{f} is skipped with no model"
        );
    }

    // `bcopy(src, dst, n)`: the copy runs from `arg(0)` to `arg(1)`, and nothing comes back.
    let bcopy = out
        .propagations
        .iter()
        .filter(|p| p.function.to_string() == "bcopy")
        .map(|p| (p.src.index, p.dst.index))
        .collect::<BTreeSet<_>>();
    assert_eq!(bcopy, BTreeSet::from([(Some(0), Some(1))]));
}

/// The gate that decides whether an import pays to collect its call sites' signature keys.
///
/// Worth pinning on its own: if it ever answers `false` for Java, every dispatch rule in the
/// defaults matches nothing and the hybrid call graph quietly reverts to CHA — with no error,
/// because a rule over an empty universe is not a failure. The equivalence test above builds the
/// universe unconditionally, so it would not notice.
#[test]
fn the_java_defaults_ask_for_the_dispatch_universe() {
    use ctadl_ascent::models::default_portcullis_models;

    let uses_callsig = |info: ProgramInfo| {
        default_portcullis_models(&info.vmt)
            .expect("the shipped defaults parse")
            .is_some_and(|set| set.uses_callsig())
    };
    assert!(
        uses_callsig(java_program()),
        "the Java defaults declare dispatch models, so they must open the gate"
    );
    // The other shipped defaults declare none, and must not make every import pay for a pass
    // over every statement.
    assert!(!uses_callsig(native_program()));
    assert!(!uses_callsig(lua_program()));
    assert!(!uses_callsig(unknown_program()));
}
