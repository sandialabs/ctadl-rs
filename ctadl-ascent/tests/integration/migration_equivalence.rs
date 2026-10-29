//! Differential tests for the JSON → DSL model migrator.
//!
//! JSON model files are run by translating them with [`dsl::migrate::migrate_generators`] and
//! loading the result on the DSL engine (see `DESIGN.md` §4). That is only sound if the
//! translation is *faithful*: for every generator, the migrated rules must match exactly what the
//! JSON matcher (`models/json.rs`) matched. Each test here takes one generator, runs it through
//! both, and compares the whole [`ProgramModelMatches`] -- propagations, function- and
//! call-site-anchored endpoints, access paths, `skip-analysis` marks, dispatch dispositions,
//! closure-shaped markers and bridges.
//!
//! When the JSON matcher refuses a generator, the faithful outcome is that the migrated form is
//! refused too: the DSL fails to load or to emit, or migration produced no row at all. A
//! migration that *warns* and still emits rows is not faithful -- a file the old loader rejected
//! would start applying models.
//!
//! The fixtures are deliberately small but discriminating: a multi-level Java hierarchy with a
//! class that has two direct supertypes, methods with and without bodies and of several arities,
//! a lowered function the method table does not name, field loads, stores and updates, call
//! sites of several callees from several callers, a Lua metatable hierarchy and externals, native
//! symbols published under several spellings, and a CIL table with an override alias row.
//!
//! Tests that fail because the migration is unfaithful are `#[ignore]`d with the reason, so the
//! suite stays green while documenting each divergence; run them with `--ignored`.
//!
//! Where the JSON matcher has a bug or a limitation, the migration takes the DSL's meaning
//! rather than reproducing it. Those cases are not ignored: they pin the exact difference
//! ([`Known`]), so neither side can drift without a test noticing.

use std::collections::BTreeSet;
use std::io::Write as _;

use serde_json::{Value, json};

use ctadl_ascent::models::matches::observe_import;
use ctadl_ascent::models::{
    DispatchKeys, ImportScope, ProgramMatchIndex, ProgramModelMatches, dsl, scan_model_files,
    try_load_models_from_values,
};
use ctadl_ascent::project::ArtifactLanguage;
use ctadl_ir::Idx;
use ctadl_ir::builder::FunctionBuilder;
use ctadl_ir::mir::call::{
    CallEdges, CallStyle, CilClass, CilMethod, CilSignature, CilSimpleName, JavaClass,
    JavaDispatch, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction, NativeQualifiedName,
    NativeSignature, NativeSimpleName, VirtualMethodTable,
};
use ctadl_ir::mir::{
    Exp, FunctionData, Functions, ParameterIdx, ParameterType, Program, ProgramInfo, StatementKind,
    Symbol,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// What a fixture function's body does, besides existing.
#[derive(Default)]
struct Body {
    loads: &'static [&'static str],
    stores: &'static [&'static str],
    /// Written through `Update`, which the JSON `uses_field` counts.
    updates: &'static [&'static str],
    calls: Vec<CallStyle>,
}

/// A function of `arity` parameters with one block holding `body`'s statements, in the order
/// loads, stores, updates, calls.
fn function(name: &str, arity: usize, body: Body) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    let mut fb = FunctionBuilder::new(&mut f);
    for _ in 0..arity {
        fb.add_param(ParameterType::Shared);
    }
    let block = fb.add_block();
    let mut b = fb.at_block(block);
    let base = if arity > 0 {
        b.new_param_var(ParameterIdx::new(0))
    } else {
        b.new_local_var("base")
    };
    for field in body.loads {
        let dest = b.new_local_var("loaded");
        b.create_load(dest, base.clone(), *field);
    }
    for field in body.stores {
        let value = b.new_local_var("stored");
        b.create_store(base.clone(), *field, Exp::Variable(value));
    }
    for field in body.updates {
        let agg = b.new_local_var("agg");
        let value = b.new_local_var("value");
        b.create_update(agg.clone(), agg, *field, Exp::Variable(value));
    }
    for style in body.calls {
        let ret = b.new_local_var("ret");
        b.create_call(style, vec![ret], vec![Exp::Variable(base.clone())]);
    }
    b.create_nop();
    f
}

/// A function with parameters and IR data but no body: an `ext` stub as a frontend lowers it.
fn stub(name: &str, arity: usize) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    for _ in 0..arity {
        f.params.parameters.push(ParameterType::Shared);
    }
    f
}

fn java_call(cls: &str, name: &str, desc: &str) -> CallStyle {
    let mut scratch = FunctionData::default();
    let mut fb = FunctionBuilder::new(&mut scratch);
    let block = fb.add_block();
    let receiver = fb.at_block(block).new_local_var("recv");
    CallStyle::JavaCall {
        receiver,
        cls: cls.into(),
        simple_name: name.into(),
        descriptor: desc.into(),
        dispatch: JavaDispatch::Virtual,
        super_start: None,
    }
}

fn cil_call(cls: &str, name: &str, desc: &str) -> CallStyle {
    let mut scratch = FunctionData::default();
    let mut fb = FunctionBuilder::new(&mut scratch);
    let block = fb.add_block();
    let receiver = fb.at_block(block).new_local_var("recv");
    CallStyle::CilCall {
        receiver,
        cls: cls.into(),
        simple_name: name.into(),
        descriptor: desc.into(),
    }
}

fn lua_method_call(method: &str) -> CallStyle {
    let mut scratch = FunctionData::default();
    let mut fb = FunctionBuilder::new(&mut scratch);
    let block = fb.add_block();
    let receiver = fb.at_block(block).new_local_var("recv");
    CallStyle::LuaCall {
        receiver,
        method: method.into(),
    }
}

fn direct_call(callees: &[&str]) -> CallStyle {
    CallStyle::DirectCall {
        call_edges: CallEdges::Explicit(callees.iter().map(|c| c.to_string()).collect()),
    }
}

fn program_info(vmt: VirtualMethodTable, functions: Vec<FunctionData>) -> ProgramInfo {
    ProgramInfo {
        vmt,
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

const J_BASE_RUN: &str = "Lapp/Base;->run()V";
const J_BASE_GET: &str = "Lapp/Base;->get(I)I";
const J_MID_RUN: &str = "Lapp/Mid;->run()V";
const J_LEAF_PUT: &str = "Lapp/Leaf;->put(II)V";
const J_LEAF_UPD: &str = "Lapp/Leaf;->upd()V";
const J_LEAF_EXT: &str = "Lapp/Leaf;->ext()V";
const J_OTHER_GET: &str = "Lapp/Other;->get(I)I";
const J_MAIN: &str = "Lapp/Main;->main([Ljava/lang/String;)V";
const J_EXEC: &str = "Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process;";
/// Lowered, but named by no row of the method table.
const J_SYNTH: &str = "Lapp/Synth;->lambda$0()V";

/// Java (dex): `Leaf <: {Mid, Other}`, `Mid <: {Base, Iface}`, `Base <: Object`.
fn java() -> ProgramInfo {
    java_with(Vec::new())
}

/// [`java`], plus a lowered lambda no row of the method table names -- which is what a JSON
/// generator's universe is built from.
fn java_unnamed() -> ProgramInfo {
    let exec = java_call(
        "Ljava/lang/Runtime;",
        "exec",
        "(Ljava/lang/String;)Ljava/lang/Process;",
    );
    java_with(vec![function(
        J_SYNTH,
        0,
        Body {
            loads: &["secret"],
            calls: vec![exec],
            ..Default::default()
        },
    )])
}

fn java_with(extra: Vec<FunctionData>) -> ProgramInfo {
    let row = |cls: &str, name: &str, sig: &str, fid: &str| {
        (
            JavaClass(cls.into()),
            JavaSimpleName(name.into()),
            JavaSignature(sig.into()),
            JavaMethod(fid.into()),
        )
    };
    let methods = vec![
        row("Lapp/Base;", "run", "()V", J_BASE_RUN),
        row("Lapp/Base;", "get", "(I)I", J_BASE_GET),
        row("Lapp/Mid;", "run", "()V", J_MID_RUN),
        row("Lapp/Leaf;", "put", "(II)V", J_LEAF_PUT),
        row("Lapp/Leaf;", "upd", "()V", J_LEAF_UPD),
        row("Lapp/Leaf;", "ext", "()V", J_LEAF_EXT),
        row("Lapp/Other;", "get", "(I)I", J_OTHER_GET),
        row("Lapp/Main;", "main", "([Ljava/lang/String;)V", J_MAIN),
        row(
            "Ljava/lang/Runtime;",
            "exec",
            "(Ljava/lang/String;)Ljava/lang/Process;",
            J_EXEC,
        ),
    ];
    let mut hierarchy: hashbrown::HashMap<JavaClass, smallvec::SmallVec<[JavaClass; 2]>> =
        Default::default();
    for (sub, sups) in [
        ("Lapp/Base;", &["Ljava/lang/Object;"][..]),
        ("Lapp/Iface;", &["Ljava/lang/Object;"][..]),
        ("Lapp/Mid;", &["Lapp/Base;", "Lapp/Iface;"][..]),
        ("Lapp/Leaf;", &["Lapp/Mid;", "Lapp/Other;"][..]),
        ("Lapp/Main;", &["Ljava/lang/Object;"][..]),
    ] {
        hierarchy.insert(
            JavaClass(sub.into()),
            sups.iter().map(|s| JavaClass((*s).into())).collect(),
        );
    }
    let exec = || {
        java_call(
            "Ljava/lang/Runtime;",
            "exec",
            "(Ljava/lang/String;)Ljava/lang/Process;",
        )
    };
    let functions = vec![
        function(J_BASE_RUN, 1, Body::default()),
        function(
            J_BASE_GET,
            2,
            Body {
                loads: &["secret"],
                ..Default::default()
            },
        ),
        function(
            J_MID_RUN,
            1,
            Body {
                stores: &["count"],
                calls: vec![java_call("Lapp/Base;", "get", "(I)I"), exec()],
                ..Default::default()
            },
        ),
        function(
            J_LEAF_PUT,
            3,
            Body {
                loads: &["count"],
                // Statically `Base.run`; CHA also resolves it to the override `Mid.run`.
                calls: vec![java_call("Lapp/Base;", "run", "()V")],
                ..Default::default()
            },
        ),
        function(
            J_LEAF_UPD,
            1,
            Body {
                updates: &["flag"],
                ..Default::default()
            },
        ),
        stub(J_OTHER_GET, 2),
        function(
            J_MAIN,
            1,
            Body {
                calls: vec![
                    java_call("Lapp/Leaf;", "put", "(II)V"),
                    exec(),
                    exec(),
                    java_call("Ljava/util/Iterator;", "next", "()Ljava/lang/Object;"),
                ],
                ..Default::default()
            },
        ),
    ];
    program_info(
        VirtualMethodTable::Java {
            methods,
            hierarchy,
            interfaces: vec![JavaClass("Lapp/Iface;".into())],
            abstract_methods: Default::default(),
            natives: Default::default(),
        },
        functions.into_iter().chain(extra).collect(),
    )
}

const N_SYSTEM: &str = "<EXTERNAL>::system@00101008";
const N_STRCPY: &str = "strcpy";
const N_FOO_BAR: &str = "Foo::bar@1000";
const N_BAZ_BAR: &str = "Baz::bar@2000";
const N_UNNAMED: &str = "FUN_3000";

/// Native (pcode): a decorated external, and two same-named methods in different namespaces.
fn native() -> ProgramInfo {
    native_with(Vec::new())
}

/// [`native`], plus a lowered function the table does not name.
fn native_unnamed() -> ProgramInfo {
    native_with(vec![function(
        N_UNNAMED,
        0,
        Body {
            calls: vec![direct_call(&[N_STRCPY])],
            ..Default::default()
        },
    )])
}

fn native_with(extra: Vec<FunctionData>) -> ProgramInfo {
    let row = |simple: &str, sig: &str, fq: &str, qualified: &str| {
        (
            NativeSimpleName(simple.into()),
            NativeSignature(sig.into()),
            NativeFunction(fq.into()),
            NativeQualifiedName(qualified.into()),
        )
    };
    let methods = vec![
        row(
            "system",
            "int system(char *)",
            N_SYSTEM,
            "<EXTERNAL>::system",
        ),
        row(
            "strcpy",
            "char * strcpy(char *, char *)",
            N_STRCPY,
            "strcpy",
        ),
        row("bar", "void bar(int)", N_FOO_BAR, "Foo::bar"),
        row("bar", "void bar(int, int)", N_BAZ_BAR, "Baz::bar"),
    ];
    let functions = vec![
        function(N_STRCPY, 2, Body::default()),
        function(
            N_FOO_BAR,
            1,
            Body {
                loads: &["x"],
                calls: vec![direct_call(&[N_SYSTEM]), direct_call(&[N_STRCPY])],
                ..Default::default()
            },
        ),
        function(
            N_BAZ_BAR,
            2,
            Body {
                stores: &["x"],
                calls: vec![direct_call(&[N_SYSTEM])],
                ..Default::default()
            },
        ),
    ];
    program_info(
        VirtualMethodTable::Native { methods },
        functions.into_iter().chain(extra).collect(),
    )
}

const L_SOURCE: &str = "app.source";
const L_DEPOSIT: &str = "app.Account.deposit";
const L_WITHDRAW: &str = "app.Account.withdraw";
const L_SUB_NEW: &str = "app.Sub.new";
const L_F1: &str = "app.f%1";
const L_EXECUTE: &str = "os.execute";
const L_FORMAT: &str = "string.format";

/// Lua: a metatable class `Sub <: Account`, a name collision (`app.f%1` is simply `f`), and
/// two externals, `format` registered under both of its call spellings.
fn lua() -> ProgramInfo {
    let sym = |s: &str| Symbol::from(s);
    let functions = vec![
        (sym("source"), sym(L_SOURCE)),
        (sym("deposit"), sym(L_DEPOSIT)),
        (sym("withdraw"), sym(L_WITHDRAW)),
        (sym("new"), sym(L_SUB_NEW)),
        (sym("f"), sym(L_F1)),
    ];
    let externals = vec![
        (sym("execute"), sym(L_EXECUTE)),
        (sym("format"), sym(L_FORMAT)),
        (sym("format"), sym("format")),
    ];
    let methods = vec![
        (sym("lua$class$Account"), sym("deposit"), sym(L_DEPOSIT)),
        (sym("lua$class$Account"), sym("withdraw"), sym(L_WITHDRAW)),
        (sym("lua$class$Sub"), sym("new"), sym(L_SUB_NEW)),
    ];
    let mut hierarchy: hashbrown::HashMap<Symbol, smallvec::SmallVec<[Symbol; 2]>> =
        Default::default();
    hierarchy.insert(
        sym("lua$class$Sub"),
        std::iter::once(sym("lua$class$Account")).collect(),
    );
    let bodies = vec![
        function(
            L_SOURCE,
            0,
            Body {
                calls: vec![direct_call(&[L_EXECUTE])],
                ..Default::default()
            },
        ),
        function(
            L_DEPOSIT,
            2,
            Body {
                stores: &["balance"],
                calls: vec![lua_method_call("format")],
                ..Default::default()
            },
        ),
        function(
            L_WITHDRAW,
            2,
            Body {
                loads: &["balance"],
                calls: vec![direct_call(&[L_FORMAT]), direct_call(&[L_EXECUTE])],
                ..Default::default()
            },
        ),
        function(L_SUB_NEW, 1, Body::default()),
        function(L_F1, 3, Body::default()),
    ];
    program_info(
        VirtualMethodTable::Lua {
            methods,
            functions,
            externals,
            hierarchy,
        },
        bodies,
    )
}

const C_A_M: &str = "[App]A::M()->System.Void";
const C_B_M: &str = "[App]B::M()->System.Void";
const C_B_N: &str = "[App]B::N(System.Int32)->System.Int32";
const C_MAIN: &str = "[App]Program::Main()->System.Void";

/// CIL: `B <: {A, I}`, with an override alias row making `B::M` matchable under `[App]I` too.
fn cil() -> ProgramInfo {
    let row = |cls: &str, simple: &str, sig: &str, fid: &str| {
        (
            CilClass(cls.into()),
            CilSimpleName(simple.into()),
            CilSignature(sig.into()),
            CilMethod(fid.into()),
        )
    };
    let methods = vec![
        row("[App]A", "M", "()->System.Void", C_A_M),
        row("[App]B", "M", "()->System.Void", C_B_M),
        row("[App]I", "M", "()->System.Void", C_B_M),
        row("[App]B", "N", "(System.Int32)->System.Int32", C_B_N),
        row("[App]Program", "Main", "()->System.Void", C_MAIN),
    ];
    let mut hierarchy: hashbrown::HashMap<CilClass, smallvec::SmallVec<[CilClass; 2]>> =
        Default::default();
    hierarchy.insert(
        CilClass("[App]B".into()),
        [CilClass("[App]A".into()), CilClass("[App]I".into())]
            .into_iter()
            .collect(),
    );
    let functions = vec![
        function(C_A_M, 1, Body::default()),
        function(
            C_B_M,
            1,
            Body {
                loads: &["f"],
                ..Default::default()
            },
        ),
        function(C_B_N, 2, Body::default()),
        function(
            C_MAIN,
            0,
            Body {
                calls: vec![
                    cil_call("[App]A", "M", "()->System.Void"),
                    cil_call("[App]B", "N", "(System.Int32)->System.Int32"),
                ],
                ..Default::default()
            },
        ),
    ];
    program_info(
        VirtualMethodTable::Cil {
            methods,
            hierarchy,
            interfaces: Default::default(),
            abstract_methods: Default::default(),
            pinvokes: Default::default(),
        },
        functions,
    )
}

/// Flowy / C: no method table at all, so the IR's function names are all there is.
fn unknown() -> ProgramInfo {
    program_info(
        VirtualMethodTable::Unknown,
        vec![
            function("f1", 1, Body::default()),
            function(
                "f2",
                2,
                Body {
                    loads: &["fld"],
                    calls: vec![direct_call(&["f1"])],
                    ..Default::default()
                },
            ),
        ],
    )
}

/// The fixture a test names, and the import identity it is matched under.
fn frontend(name: &str) -> (ImportScope, ProgramInfo) {
    match name {
        "java" => (ImportScope::new(ArtifactLanguage::Dex, "app"), java()),
        "java_unnamed" => (
            ImportScope::new(ArtifactLanguage::Dex, "app"),
            java_unnamed(),
        ),
        "native" => (ImportScope::new(ArtifactLanguage::Pcode, "lib"), native()),
        "native_unnamed" => (
            ImportScope::new(ArtifactLanguage::Pcode, "lib"),
            native_unnamed(),
        ),
        "lua" => (ImportScope::new(ArtifactLanguage::Lua, "script"), lua()),
        "cil" => (ImportScope::new(ArtifactLanguage::Cil, "asm"), cil()),
        "unknown" => (ImportScope::new(ArtifactLanguage::Flowy, "flow"), unknown()),
        other => panic!("no fixture named {other}"),
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Everything a load matched, rendered as sorted strings so a mismatch prints readably.
///
/// Call-site endpoints are compared by what they *denote*: the `(callee, caller)` pairs of the
/// program's call sites they apply to. The JSON matcher writes one row per matched callee with
/// "any caller", or one per matched caller; the DSL writes one per site. Both are expanded over
/// the program's statically named callees -- the same `callee_string` the DSL's `callsite`
/// relation reads.
#[derive(Default, Debug, PartialEq, Eq)]
struct Matched {
    propagations: BTreeSet<String>,
    endpoints: BTreeSet<String>,
    callsite_endpoints: BTreeSet<String>,
    access_paths: BTreeSet<String>,
    skip_analysis: BTreeSet<String>,
    dispatch: BTreeSet<String>,
    closure_shaped: BTreeSet<String>,
    bridges: BTreeSet<String>,
}

impl Matched {
    fn is_empty(&self) -> bool {
        *self == Matched::default()
    }

    fn fields(&self) -> [(&'static str, &BTreeSet<String>); 8] {
        [
            ("propagations", &self.propagations),
            ("endpoints", &self.endpoints),
            ("callsite endpoints", &self.callsite_endpoints),
            ("access_paths", &self.access_paths),
            ("skip_analysis", &self.skip_analysis),
            ("dispatch", &self.dispatch),
            ("closure_shaped", &self.closure_shaped),
            ("bridges", &self.bridges),
        ]
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (name, set) in self.fields() {
            for row in set {
                out.push_str(&format!("    {name}: {row}\n"));
            }
        }
        if out.is_empty() {
            out.push_str("    (nothing)\n");
        }
        out
    }

    fn diff(&self, dsl: &Matched) -> String {
        let mut out = String::new();
        for ((name, j), (_, d)) in self.fields().into_iter().zip(dsl.fields()) {
            for row in j.difference(d) {
                out.push_str(&format!("    JSON only  {name}: {row}\n"));
            }
            for row in d.difference(j) {
                out.push_str(&format!("    DSL only   {name}: {row}\n"));
            }
        }
        out
    }
}

/// Every `(caller, callee)` pair a call site of `program` names, spelled as the DSL's
/// `callsite` relation spells the callee.
fn static_sites(program: &ProgramInfo) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for func in program.program.functions.iter() {
        for block in func.blocks.iter() {
            for stmt in block.statements.iter() {
                let StatementKind::CallAssign { style, .. } = &stmt.kind else {
                    continue;
                };
                let callees: Vec<String> = match style {
                    CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(edges),
                    } => edges.iter().map(|e| e.to_string()).collect(),
                    CallStyle::JavaCall {
                        cls,
                        simple_name,
                        descriptor,
                        ..
                    } => vec![format!("{cls}->{simple_name}{descriptor}")],
                    CallStyle::CilCall {
                        cls,
                        simple_name,
                        descriptor,
                        ..
                    } => vec![format!("{cls}::{simple_name}{descriptor}")],
                    CallStyle::LuaCall { method, .. } => vec![method.to_string()],
                    _ => Vec::new(),
                };
                for callee in callees {
                    out.insert((func.name.clone(), callee));
                }
            }
        }
    }
    out
}

fn port_text(
    tag: impl std::fmt::Debug,
    index: Option<i16>,
    path: &ctadl_ascent::facts::Path,
) -> String {
    let index = index.map(|i| format!("({i})")).unwrap_or_default();
    format!("{tag:?}{index}{}", path.to_dot_string())
}

fn normalize(m: &ProgramModelMatches, sites: &BTreeSet<(String, String)>) -> Matched {
    let mut out = Matched::default();
    for p in &m.propagations {
        out.propagations.insert(format!(
            "{}: {} <- {}",
            p.function,
            port_text(p.dst.tag, p.dst.index, &p.dst.path),
            port_text(p.src.tag, p.src.index, &p.src.path),
        ));
    }
    for e in &m.endpoints {
        let what = format!(
            "{:?} {} kind={} wildcard={} saturating={} local={:?}",
            e.direction,
            port_text(e.selector_ty, e.index, &e.path),
            e.label,
            e.wildcard,
            e.saturating,
            e.local_index,
        );
        if !e.callsite_scoped {
            out.endpoints.insert(format!("{} {what}", e.function));
            continue;
        }
        let callee = e.function.to_string();
        let in_function = e.in_function.map(|s| s.to_string());
        for (caller, site_callee) in sites {
            if *site_callee == callee && in_function.as_ref().is_none_or(|c| c == caller) {
                out.callsite_endpoints
                    .insert(format!("{callee} in {caller} {what}"));
            }
        }
    }
    for p in &m.access_paths {
        out.access_paths.insert(p.to_dot_string());
    }
    for f in &m.skip_analysis {
        out.skip_analysis.insert(f.to_string());
    }
    for ((cls, name, desc), model) in &m.dispatch {
        // Provenance names the file and the generator or rule, so the loaders must differ there.
        out.dispatch.insert(format!(
            "{cls}->{name}{desc}: {}",
            disposition_text(&model.disposition)
        ));
    }
    for (cls, name, desc) in &m.closure_shaped {
        out.closure_shaped.insert(format!("{cls}->{name}{desc}"));
    }
    for b in &m.resolved_bridges {
        for pair in &b.ports {
            out.bridges
                .insert(format!("{} -> {}: {}", b.from, b.to, pair_text(pair)));
        }
    }
    out
}

/// A disposition with its port list sorted: two loaders may list one key's ports in different
/// orders, and the list is a set to codegen.
fn disposition_text(d: &ctadl_ascent::models::matches::Disposition) -> String {
    use ctadl_ascent::models::matches::Disposition;
    let ports = |ps: &[(
        ctadl_ascent::models::matches::ModelPort,
        ctadl_ascent::models::matches::ModelPort,
    )]| {
        let mut v: Vec<String> = ps
            .iter()
            .map(|(dst, src)| {
                format!(
                    "{} <- {}",
                    port_text(dst.tag, dst.index, &dst.path),
                    port_text(src.tag, src.index, &src.path)
                )
            })
            .collect();
        v.sort();
        v.join(", ")
    };
    match d {
        Disposition::Inline(ps) => format!("inline[{}]", ports(ps)),
        Disposition::Model(ps) => format!("model[{}]", ports(ps)),
        Disposition::Skip => "skip".to_string(),
    }
}

fn pair_text(pair: &ctadl_ascent::models::spec::PortPair) -> String {
    format!(
        "{:?}{} {:?} {:?}{}",
        pair.from.index,
        pair.from.path.to_dot_string(),
        pair.direction,
        pair.to.index,
        pair.to.path.to_dot_string()
    )
}

/// One import: its identity and its program.
type Import = (ImportScope, ProgramInfo);

/// What the JSON matcher matched for `generator` across `imports`, or why it refused.
fn run_json(generator: &Value, imports: &[Import]) -> Result<Matched, String> {
    let mut matched = Matched::default();
    for (scope, program) in imports {
        let keys = DispatchKeys::from_program(&program.program);
        let index = ProgramMatchIndex::new_with_dispatch(program, scope.clone(), Some(&keys));
        let mut one = ProgramModelMatches::default();
        try_load_models_from_values(&index, std::iter::once(Ok(generator.clone())), &mut one)
            .map_err(|e| format!("{e:#}"))?;
        let sites = static_sites(program);
        let n = normalize(&one, &sites);
        merge(&mut matched, n);
    }
    if generator.pointer("/model/bridge").is_some() {
        merge(&mut matched, run_json_bridges(generator, imports)?);
    }
    Ok(matched)
}

/// A JSON bridge is not matched by the per-import loader: it is scanned from a file, both sides
/// are observed per import, and phase 2 pairs every `from` with every `to`.
fn run_json_bridges(generator: &Value, imports: &[Import]) -> Result<Matched, String> {
    let dir = std::env::temp_dir();
    let mut file = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile_in(dir)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{}", serde_json::to_string(generator).unwrap()).unwrap();
    file.flush().unwrap();
    let specs = scan_model_files(&[file.path().to_path_buf()]).map_err(|e| format!("{e:#}"))?;
    let mut out = ProgramModelMatches::default();
    out.bridges.prepare(&specs.bridges);
    for (scope, program) in imports {
        let index = ProgramMatchIndex::new(program, scope.clone());
        observe_import(&index, &specs.bridges, &mut out).map_err(|e| format!("{e:#}"))?;
    }
    let mut matched = Matched::default();
    for (i, spec) in specs.bridges.iter().enumerate() {
        for (from, to) in out.bridges.get(i).pairs() {
            if !spec.ports_given {
                matched
                    .bridges
                    .insert(format!("{from} -> {to}: <identity over the shared arity>"));
            }
            for pair in &spec.ports {
                matched
                    .bridges
                    .insert(format!("{from} -> {to}: {}", pair_text(pair)));
            }
        }
    }
    Ok(matched)
}

fn merge(into: &mut Matched, from: Matched) {
    into.propagations.extend(from.propagations);
    into.endpoints.extend(from.endpoints);
    into.callsite_endpoints.extend(from.callsite_endpoints);
    into.access_paths.extend(from.access_paths);
    into.skip_analysis.extend(from.skip_analysis);
    into.dispatch.extend(from.dispatch);
    into.closure_shaped.extend(from.closure_shaped);
    into.bridges.extend(from.bridges);
}

/// What the migrated DSL matched across `imports`, or why it refused.
fn run_dsl(text: &str, imports: &[Import]) -> Result<Matched, String> {
    let file = dsl::DslFile::from_text("migrated.ctadl", text).map_err(|e| format!("{e:#}"))?;
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set, dsl::Phase::All);
    let mut out = ProgramModelMatches::default();
    let mut sites = BTreeSet::new();
    let keys: Vec<DispatchKeys> = imports
        .iter()
        .map(|(_, p)| DispatchKeys::from_program(&p.program))
        .collect();
    for ((scope, program), keys) in imports.iter().zip(&keys) {
        let index = ProgramMatchIndex::new_with_dispatch(program, scope.clone(), Some(keys));
        matcher.observe_import(&index, &mut out);
        sites.extend(static_sites(program));
    }
    matcher.finish(&mut out).map_err(|e| format!("{e:#}"))?;
    Ok(normalize(&out, &sites))
}

/// Requires the migrated form of `generator` to match exactly what the JSON matcher matched
/// across `imports`, or to be refused where the JSON matcher refused it.
fn assert_faithful(generator: &Value, imports: &[Import]) {
    let (text, report) = dsl::migrate::migrate_generators(std::iter::once(generator), None);
    let json = run_json(generator, imports);
    let dsl = run_dsl(&text, imports);
    let context = || {
        format!(
            "generator:\n{}\nmigrated DSL:\n{text}\nmigration warnings:\n{}\n",
            serde_json::to_string_pretty(generator).unwrap(),
            if report.warnings.is_empty() {
                "    (none)".to_string()
            } else {
                report
                    .warnings
                    .iter()
                    .map(|w| format!("    {w}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        )
    };
    match (json, dsl) {
        (Ok(j), Ok(d)) => {
            if j != d {
                panic!(
                    "migration is unfaithful\n{}JSON matched:\n{}DSL matched:\n{}difference:\n{}",
                    context(),
                    j.render(),
                    d.render(),
                    j.diff(&d)
                );
            }
        }
        (Ok(j), Err(e)) => panic!(
            "the JSON matcher accepted this generator but the migrated DSL fails: {e}\n{}\
             JSON matched:\n{}",
            context(),
            j.render()
        ),
        // Refused on both sides.
        (Err(_), Err(_)) => {}
        (Err(e), Ok(d)) => {
            if !d.is_empty() {
                panic!(
                    "the JSON matcher refuses this generator ({e}), but the migrated DSL loads \
                     and matches\n{}DSL matched:\n{}",
                    context(),
                    d.render()
                );
            }
        }
    }
}

/// [`assert_faithful`] against one named fixture.
fn assert_faithful_on(frontend_name: &str, generator: &Value) {
    assert_faithful(generator, &[frontend(frontend_name)]);
}

/// A deliberate difference between the JSON matcher and the migrated DSL: the JSON matcher's
/// behavior is a bug or a limitation, and the migration keeps the DSL's meaning.
struct Known {
    /// Which JSON behavior the migration does not reproduce.
    reason: &'static str,
    /// The difference, one `JSON only …` / `DSL only …` line per row, as [`Matched::diff`]
    /// prints it.
    diff: &'static [&'static str],
}

const JSON_UNIVERSE: &str = "the JSON matcher's universe is the method table; the DSL's is \
    every function, including one the table does not name";
const FULL_NAME: &str = "the DSL also accepts a function's full name as a name or signature";
const LUA_CLASSES: &str = "the JSON matcher sees no class on Lua; the DSL sees metatable classes";
const IN_FUNCTION_BUG: &str =
    "JSON matcher bug: `in_function` under `not`/`any_of` narrows the callee set";

/// Requires the JSON matcher and the migrated DSL to differ on `frontend_name` by exactly
/// `known.diff`.
fn assert_known_on(frontend_name: &str, generator: &Value, known: &Known) {
    let imports = [frontend(frontend_name)];
    let (text, _) = dsl::migrate::migrate_generators(std::iter::once(generator), None);
    let json = run_json(generator, &imports).expect("the JSON matcher accepts the generator");
    let dsl = run_dsl(&text, &imports)
        .unwrap_or_else(|e| panic!("the migrated DSL fails: {e}\nmigrated DSL:\n{text}"));
    let actual: Vec<String> = json
        .diff(&dsl)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let expected: Vec<String> = known.diff.iter().map(|l| l.to_string()).collect();
    assert!(
        actual == expected,
        "the known difference ({}) changed\nmigrated DSL:\n{text}\nexpected:\n{expected:#?}\n\
         actual:\n{actual:#?}",
        known.reason,
    );
}

/// A test module named `$name` holding one test per listed frontend, each requiring the
/// migration of `$generator` to be faithful on that frontend's fixture. A frontend may carry
/// attributes, which is how a known divergence is `#[ignore]`d on just the frontend it shows on,
/// or `=> Known { … }`, which pins a deliberate difference there instead.
macro_rules! faithful {
    ($(#[doc = $doc:literal])* $name:ident = $generator:expr; $( $(#[$attr:meta])* $fe:ident $(=> $known:expr)? ),+ $(,)?) => {
        $(#[doc = $doc])*
        mod $name {
            use super::*;
            fn generator() -> Value {
                $generator
            }
            $(
                $(#[$attr])*
                #[test]
                fn $fe() {
                    let known: Option<Known> = None $(.or(Some($known)))?;
                    match known {
                        Some(known) => assert_known_on(stringify!($fe), &generator(), &known),
                        None => assert_faithful_on(stringify!($fe), &generator()),
                    }
                }
            )+
        }
    };
}

// ---------------------------------------------------------------------------
// Generator builders
// ---------------------------------------------------------------------------

/// A model with one of each function-level kind, so every test compares a source, a sink and a
/// propagation at once.
fn method_model() -> Value {
    json!({
        "sources": [{"kind": "Src", "port": "Return"}],
        "sinks": [{"kind": "Snk", "port": "Argument(0)"}],
        "propagation": [{"input": "Argument(0)", "output": "Return"}],
    })
}

/// A `find: methods` generator over `where_` carrying [`method_model`].
fn methods(where_: Value) -> Value {
    json!({"find": "methods", "where": where_, "model": method_model()})
}

/// A `find: callsites` generator over `where_`: call-site endpoints only, since a propagation is
/// refused there.
fn callsites(where_: Value) -> Value {
    json!({
        "find": "callsites",
        "where": where_,
        "model": {
            "sources": [{"kind": "Src", "port": "Return"}],
            "sinks": [{"kind": "Snk", "port": "Argument(0)", "wildcard": false}],
        },
    })
}

/// A `find: dispatch` generator over `where_` with one modelled flow.
fn dispatch(where_: Value) -> Value {
    json!({
        "find": "dispatch",
        "where": where_,
        "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
    })
}

fn not(inner: Value) -> Value {
    json!({"constraint": "not", "inner": inner})
}

fn any_of(inners: Value) -> Value {
    json!({"constraint": "any_of", "inners": inners})
}

fn all_of(inners: Value) -> Value {
    json!({"constraint": "all_of", "inners": inners})
}

fn sig_name(name: &str) -> Value {
    json!({"constraint": "signature_match", "name": name})
}

fn arity(op: &str, value: i64) -> Value {
    json!({"constraint": "number_parameters", "inner": {"constraint": op, "value": value}})
}

// ---------------------------------------------------------------------------
// No constraints
// ---------------------------------------------------------------------------

faithful! {
    /// A generator with no `where` matches every function the import names.
    no_where = json!({"find": "methods", "model": method_model()});
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    java, native, lua, cil,
}

// ---------------------------------------------------------------------------
// signature_match
// ---------------------------------------------------------------------------

faithful! {
    /// `signature_match` on a simple name: Java `get` is two methods, native `bar` two
    /// namespaces, Lua `f` a collided `app.f%1`, `format` an external with two spellings.
    signature_match_name = methods(json!([{"constraint": "signature_match", "name": "get"}]));
    java, native, lua, cil, unknown,
}

faithful! {
    signature_match_name_native_bar = methods(json!([sig_name("bar")]));
    java, native, lua, cil, unknown,
}

faithful! {
    signature_match_name_lua = methods(json!([
        {"constraint": "signature_match", "names": ["f", "format", "execute", "f1"]}
    ]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// Under `not`: everything the import names but the matched set.
    signature_match_name_not = methods(json!([not(sig_name("get"))]));
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    java, native, lua, cil,
}

faithful! {
    /// `name` and `names` together are a union.
    signature_match_name_and_names = methods(json!([
        {"constraint": "signature_match", "name": "run", "names": ["get", "bar", "source"]}
    ]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// A full Java method id is not a *name*: the JSON index keys `name` on the simple name
    /// only, so this matches nothing there. The DSL accepts every lowered function's full name
    /// as one of its names.
    signature_match_name_is_not_the_full_id = methods(json!([sig_name(J_BASE_GET)]));
    java => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: Lapp/Base;->get(I)I: Return <- Index(0)",
            "DSL only endpoints: Lapp/Base;->get(I)I Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Base;->get(I)I Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    native, lua, cil,
}

faithful! {
    /// The CIL counterpart of [`signature_match_name_is_not_the_full_id`].
    signature_match_name_is_not_the_full_cil_id = methods(json!([sig_name(C_B_M)]));
    cil => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: [App]B::M()->System.Void: Return <- Index(0)",
            "DSL only endpoints: [App]B::M()->System.Void Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: [App]B::M()->System.Void Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// A native function the method table does not name: the JSON index has no row for it.
    signature_match_name_unnamed_native = methods(json!([sig_name(N_UNNAMED)]));
    native_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: FUN_3000: Return <- Index(0)",
            "DSL only endpoints: FUN_3000 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: FUN_3000 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// `parent` on the frontends that record a declaring class, including CIL's alias row.
    signature_match_parent = methods(json!([
        {"constraint": "signature_match", "parents": ["Lapp/Leaf;", "[App]I"], "parent": "Lapp/Base;"}
    ]));
    java, native, cil, unknown,
}

faithful! {
    /// Lua's metatable classes are not `parent`s to the JSON index, so this matches nothing.
    signature_match_parent_lua = methods(json!([
        {"constraint": "signature_match", "parent": "lua$class$Account"}
    ]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "DSL only propagations: app.Account.deposit: Return <- Index(0)",
            "DSL only propagations: app.Account.withdraw: Return <- Index(0)",
            "DSL only endpoints: app.Account.deposit Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: app.Account.deposit Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: app.Account.withdraw Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: app.Account.withdraw Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    signature_match_parent_lua_not = methods(json!([
        not(json!({"constraint": "signature_match", "parent": "lua$class$Account"}))
    ]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "JSON only propagations: app.Account.deposit: Return <- Index(0)",
            "JSON only propagations: app.Account.withdraw: Return <- Index(0)",
            "JSON only endpoints: app.Account.deposit Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "JSON only endpoints: app.Account.deposit Forward Return kind=Src wildcard=false saturating=false local=None",
            "JSON only endpoints: app.Account.withdraw Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "JSON only endpoints: app.Account.withdraw Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// `name` and `parent` in one constraint intersect.
    signature_match_name_and_parent = methods(json!([
        {"constraint": "signature_match", "name": "run", "parent": "Lapp/Mid;"}
    ]));
    java, cil,
}

faithful! {
    signature_match_name_and_parent_not = methods(json!([
        not(json!({"constraint": "signature_match", "name": "run", "parent": "Lapp/Mid;"}))
    ]));
    java, cil,
}

faithful! {
    /// `qualified-id` on each frontend's spelling of it.
    signature_match_qualified_id = methods(json!([{
        "constraint": "signature_match",
        "qualified-ids": [J_LEAF_EXT, "Foo::bar", N_SYSTEM, L_F1, L_FORMAT, C_B_N, "f2"],
        "qualified-id": "<EXTERNAL>::system",
    }]));
    java, native, lua, cil, unknown,
}

faithful! {
    signature_match_qualified_id_not = methods(json!([
        not(json!({"constraint": "signature_match", "qualified-ids": [J_LEAF_EXT, "Foo::bar", L_F1, C_B_N]}))
    ]));
    java, native, lua, cil,
}

faithful! {
    /// A Lua function's bare name is not its qualified id.
    signature_match_qualified_id_bare_lua = methods(json!([
        {"constraint": "signature_match", "qualified-id": "f"}
    ]));
    lua,
}

faithful! {
    /// A field the constraint does not honor is a hard error in the JSON loader: dropping it
    /// would widen the match.
    signature_match_unknown_field = methods(json!([
        {"constraint": "signature_match", "name": "get", "extends": "Lapp/Base;"}
    ]));
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    /// No honored field at all.
    signature_match_no_field = methods(json!([{"constraint": "signature_match"}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

// ---------------------------------------------------------------------------
// name / signature / signature_pattern
// ---------------------------------------------------------------------------

faithful! {
    /// An unanchored regex over every simple-name spelling.
    name_regex = methods(json!([{"constraint": "name", "pattern": "^(get|bar|f|format)$"}]));
    java, native, lua, cil, unknown,
}

faithful! {
    name_regex_not = methods(json!([not(json!({"constraint": "name", "pattern": "^(get|bar|f|format)$"}))]));
    java, native, lua, cil,
}

faithful! {
    /// The search is unanchored, and native/Lua publish the full name as a name too.
    name_regex_unanchored = methods(json!([{"constraint": "name", "pattern": "ystem|Account|execute"}]));
    native, lua,
}

faithful! {
    /// On Java and CIL a pattern over the class part of the id matches no *name*.
    name_regex_over_the_full_id = methods(json!([{"constraint": "name", "pattern": "Leaf|\\[App\\]B"}]));
    java => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: Lapp/Leaf;->put(II)V: Return <- Index(0)",
            "DSL only propagations: Lapp/Leaf;->upd()V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Leaf;->put(II)V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Leaf;->put(II)V Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: Lapp/Leaf;->upd()V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Leaf;->upd()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    cil => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: [App]B::M()->System.Void: Return <- Index(0)",
            "DSL only propagations: [App]B::N(System.Int32)->System.Int32: Return <- Index(0)",
            "DSL only endpoints: [App]B::M()->System.Void Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: [App]B::M()->System.Void Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: [App]B::N(System.Int32)->System.Int32 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: [App]B::N(System.Int32)->System.Int32 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// `signature` regex over Java descriptors, native type signatures and Lua names.
    signature_regex = methods(json!([{"constraint": "signature", "pattern": "\\(I\\)|int\\)|^app\\.Account"}]));
    java, native, lua, cil,
}

faithful! {
    signature_pattern_regex = methods(json!([{"constraint": "signature_pattern", "pattern": "->System.Void$|char \\*"}]));
    java, native, lua, cil,
}

faithful! {
    signature_regex_not = methods(json!([not(json!({"constraint": "signature", "pattern": "\\(I\\)|int\\)|^app\\.Account|Void"}))]));
    java, native, lua, cil,
}

faithful! {
    /// A Java signature is the descriptor: the method name is not in it.
    signature_regex_over_the_full_id = methods(json!([{"constraint": "signature", "pattern": "put|Main"}]));
    java => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: Lapp/Leaf;->put(II)V: Return <- Index(0)",
            "DSL only propagations: Lapp/Main;->main([Ljava/lang/String;)V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Leaf;->put(II)V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Leaf;->put(II)V Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: Lapp/Main;->main([Ljava/lang/String;)V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Main;->main([Ljava/lang/String;)V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    cil => Known {
        reason: FULL_NAME,
        diff: &[
            "DSL only propagations: [App]Program::Main()->System.Void: Return <- Index(0)",
            "DSL only endpoints: [App]Program::Main()->System.Void Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: [App]Program::Main()->System.Void Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// An invalid regex is refused by both.
    signature_regex_invalid = methods(json!([{"constraint": "signature", "pattern": "("}]));
    java,
}

faithful! {
    name_regex_invalid = methods(json!([{"constraint": "name", "pattern": "("}]));
    java,
}

// ---------------------------------------------------------------------------
// has_code
// ---------------------------------------------------------------------------

faithful! {
    /// Bodyless methods (no IR data) and stubs (IR data, no blocks) are both "no code" -- but
    /// only the stub can match `has_code: false`.
    has_code_true = methods(json!([{"constraint": "has_code", "value": true}]));
    java, native, lua, cil, unknown,
}

faithful! {
    has_code_false = methods(json!([{"constraint": "has_code", "value": false}]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// Complemented against the import's method universe, which holds bodyless methods.
    has_code_not = methods(json!([not(json!({"constraint": "has_code", "value": true}))]));
    java, native, lua, cil,
}

faithful! {
    has_code_not_false = methods(json!([not(json!({"constraint": "has_code", "value": false}))]));
    java, native, lua, cil,
}

// ---------------------------------------------------------------------------
// number_parameters
// ---------------------------------------------------------------------------

faithful! {
    number_parameters_lt = methods(json!([arity("<", 2)]));
    java, native, lua, cil, unknown,
}

faithful! {
    number_parameters_le = methods(json!([arity("<=", 1)]));
    java, native, lua, cil, unknown,
}

faithful! {
    number_parameters_gt = methods(json!([arity(">", 1)]));
    java, native, lua, cil, unknown,
}

faithful! {
    number_parameters_ge = methods(json!([arity(">=", 2)]));
    java, native, lua, cil, unknown,
}

faithful! {
    number_parameters_eq = methods(json!([arity("==", 2)]));
    java, native, lua, cil, unknown,
}

faithful! {
    number_parameters_ne = methods(json!([arity("!=", 2)]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// Nested combinators inside the integer predicate.
    number_parameters_nested = methods(json!([{
        "constraint": "number_parameters",
        "inner": {"constraint": "any_of", "inners": [
            {"constraint": "==", "value": 0},
            {"constraint": "all_of", "inners": [
                {"constraint": ">", "value": 1},
                {"constraint": "not", "inner": {"constraint": "==", "value": 3}},
            ]},
        ]},
    }]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// A bodyless method has no arity, so it is in the complement.
    number_parameters_not = methods(json!([not(arity("<", 2))]));
    java, native, lua, cil,
}

faithful! {
    /// A class predicate inside an integer one is an authoring error.
    number_parameters_wrong_inner = methods(json!([{
        "constraint": "number_parameters",
        "inner": {"constraint": "name", "pattern": "x"},
    }]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

// ---------------------------------------------------------------------------
// uses_field
// ---------------------------------------------------------------------------

faithful! {
    uses_field_name = methods(json!([{"constraint": "uses_field", "name": "count"}]));
    java, native, lua, cil, unknown,
}

faithful! {
    uses_field_names = methods(json!([{"constraint": "uses_field", "names": ["secret", "x", "balance", "f", "fld"]}]));
    java, native, lua, cil, unknown,
}

faithful! {
    uses_field_not = methods(json!([not(json!({"constraint": "uses_field", "names": ["secret", "x", "balance", "f"]}))]));
    java, native, lua, cil,
}

faithful! {
    /// A field written through an `Update` statement is a use to the JSON matcher.
    uses_field_update = methods(json!([{"constraint": "uses_field", "name": "flag"}]));
    java,
}

// ---------------------------------------------------------------------------
// parent
// ---------------------------------------------------------------------------

faithful! {
    parent_signature_match = methods(json!([{
        "constraint": "parent",
        "inner": {"constraint": "signature_match", "names": ["Lapp/Base;", "[App]I"], "name": "Lapp/Leaf;"},
    }]));
    java, native, cil, unknown,
}

faithful! {
    parent_name = methods(json!([{"constraint": "parent", "inner": {"constraint": "name", "pattern": "Mid|Leaf|\\]B$"}}]));
    java, native, cil, unknown,
}

faithful! {
    parent_not = methods(json!([not(json!({"constraint": "parent", "inner": {"constraint": "name", "pattern": "Mid|Leaf|\\]B$"}}))]));
    java, native, cil,
}

faithful! {
    parent_any_of = methods(json!([{"constraint": "parent", "inner": any_of(json!([
        sig_name("Lapp/Mid;"),
        {"constraint": "name", "pattern": "Other|\\]A$"},
    ]))}]));
    java, cil,
}

faithful! {
    parent_any_of_not = methods(json!([not(json!({"constraint": "parent", "inner": any_of(json!([
        sig_name("Lapp/Mid;"),
        {"constraint": "name", "pattern": "Other|\\]A$"},
    ]))}))]));
    java, cil,
}

faithful! {
    /// One class must satisfy every inner. `B::M` is declared by `[App]B` and aliased under
    /// `[App]I`; no single one of those matches both patterns.
    parent_all_of = methods(json!([{"constraint": "parent", "inner": all_of(json!([
        {"constraint": "name", "pattern": "\\]B$"},
        {"constraint": "name", "pattern": "\\]I$"},
    ]))}]));
    cil, java,
}

faithful! {
    /// `not` inside a class predicate.
    parent_inner_not = methods(json!([{"constraint": "parent", "inner": not(sig_name("Lapp/Base;"))}]));
    java, cil,
}

faithful! {
    /// The JSON `parent` needs a Java or CIL class hierarchy and matches nothing elsewhere.
    parent_lua = methods(json!([{"constraint": "parent", "inner": {"constraint": "name", "pattern": "Account"}}]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "DSL only propagations: app.Account.deposit: Return <- Index(0)",
            "DSL only propagations: app.Account.withdraw: Return <- Index(0)",
            "DSL only endpoints: app.Account.deposit Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: app.Account.deposit Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: app.Account.withdraw Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: app.Account.withdraw Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    parent_lua_not = methods(json!([not(json!({"constraint": "parent", "inner": {"constraint": "name", "pattern": "Account"}}))]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "JSON only propagations: app.Account.deposit: Return <- Index(0)",
            "JSON only propagations: app.Account.withdraw: Return <- Index(0)",
            "JSON only endpoints: app.Account.deposit Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "JSON only endpoints: app.Account.deposit Forward Return kind=Src wildcard=false saturating=false local=None",
            "JSON only endpoints: app.Account.withdraw Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "JSON only endpoints: app.Account.withdraw Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// `parent` is not a field of a class-level `signature_match`.
    parent_inner_unknown_field = methods(json!([{"constraint": "parent", "inner": {"constraint": "signature_match", "name": "Lapp/Base;", "parent": "x"}}]));
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    parent_inner_unknown_constraint = methods(json!([{"constraint": "parent", "inner": {"constraint": "has_code", "value": true}}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

// ---------------------------------------------------------------------------
// extends
// ---------------------------------------------------------------------------

faithful! {
    /// A *direct* supertype: `Leaf`'s methods extend `Mid` and `Other`, not `Base`.
    extends_signature_match = methods(json!([{"constraint": "extends", "inner": sig_name("Lapp/Mid;")}]));
    java, native, cil, unknown,
}

faithful! {
    extends_signature_match_names = methods(json!([{"constraint": "extends", "inner": {"constraint": "signature_match", "names": ["Lapp/Base;", "[App]I"]}}]));
    java, cil,
}

faithful! {
    extends_name = methods(json!([{"constraint": "extends", "inner": {"constraint": "name", "pattern": "Iface|Object|\\]A$"}}]));
    java, native, cil, unknown,
}

faithful! {
    extends_not = methods(json!([not(json!({"constraint": "extends", "inner": {"constraint": "name", "pattern": "Iface|Other|\\]A$"}}))]));
    java, native, cil,
}

faithful! {
    extends_any_of = methods(json!([{"constraint": "extends", "inner": any_of(json!([
        sig_name("Lapp/Other;"),
        {"constraint": "name", "pattern": "Iface|\\]I$"},
    ]))}]));
    java, cil,
}

faithful! {
    /// One direct supertype must satisfy every inner. `Leaf`'s supertypes are `Mid` and
    /// `Other`; neither matches both patterns, but each matches one.
    extends_all_of = methods(json!([{"constraint": "extends", "inner": all_of(json!([
        {"constraint": "name", "pattern": "Mid|\\]A$"},
        {"constraint": "name", "pattern": "Other|\\]I$"},
    ]))}]));
    java,
    cil,
}

faithful! {
    extends_all_of_not = methods(json!([not(json!({"constraint": "extends", "inner": all_of(json!([
        {"constraint": "name", "pattern": "Mid|\\]A$"},
        {"constraint": "name", "pattern": "Other|\\]I$"},
    ]))}))]));
    java,
    cil,
}

faithful! {
    extends_inner_not = methods(json!([{"constraint": "extends", "inner": not(sig_name("Lapp/Mid;"))}]));
    java,
}

faithful! {
    /// `not` nested in an `any_of` inside the class predicate.
    extends_inner_any_of_not = methods(json!([{"constraint": "extends", "inner": any_of(json!([
        not(sig_name("Ljava/lang/Object;")),
        sig_name("Lapp/Base;"),
    ]))}]));
    java,
}

faithful! {
    extends_lua = methods(json!([{"constraint": "extends", "inner": {"constraint": "name", "pattern": "Account"}}]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "DSL only propagations: app.Sub.new: Return <- Index(0)",
            "DSL only endpoints: app.Sub.new Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: app.Sub.new Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    extends_lua_not = methods(json!([not(json!({"constraint": "extends", "inner": {"constraint": "name", "pattern": "Account"}}))]));
    lua => Known {
        reason: LUA_CLASSES,
        diff: &[
            "JSON only propagations: app.Sub.new: Return <- Index(0)",
            "JSON only endpoints: app.Sub.new Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "JSON only endpoints: app.Sub.new Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

// ---------------------------------------------------------------------------
// any_of / all_of / not
// ---------------------------------------------------------------------------

faithful! {
    any_of_leaves = methods(json!([any_of(json!([
        sig_name("run"),
        {"constraint": "uses_field", "name": "x"},
        arity("==", 3),
        sig_name("source"),
    ]))]));
    java, native, lua, cil, unknown,
}

faithful! {
    any_of_not = methods(json!([not(any_of(json!([sig_name("run"), arity("==", 3), sig_name("source")])))]));
    java, lua, cil,
}

faithful! {
    /// A `not` inside an `any_of` complements against the `any_of`'s own working set, so this
    /// is the whole method universe. See `universe_any_of_containing_not` for where that
    /// universe differs.
    any_of_containing_not = methods(json!([any_of(json!([not(sig_name("get")), sig_name("get")]))]));
    java, native, lua, cil,
}

faithful! {
    any_of_containing_not_narrowed = methods(json!([
        {"constraint": "has_code", "value": true},
        any_of(json!([not(sig_name("get")), arity("==", 3)])),
    ]));
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    java, native, lua, cil,
}

faithful! {
    all_of_leaves = methods(json!([all_of(json!([sig_name("get"), {"constraint": "has_code", "value": true}]))]));
    java, native, lua, cil, unknown,
}

faithful! {
    all_of_not = methods(json!([
        {"constraint": "has_code", "value": true},
        not(all_of(json!([sig_name("get"), {"constraint": "uses_field", "name": "secret"}]))),
    ]));
    java, native, lua, cil, unknown,
}

faithful! {
    not_not = methods(json!([not(not(sig_name("run")))]));
    java, native, lua, cil, unknown,
}

faithful! {
    /// An empty `any_of` matches nothing.
    any_of_empty = methods(json!([any_of(json!([]))]));
    java,
}

faithful! {
    /// An empty `all_of` constrains nothing.
    all_of_empty = methods(json!([all_of(json!([]))]));
    java,
}

faithful! {
    not_without_inner = methods(json!([{"constraint": "not"}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

faithful! {
    /// An unknown constraint is a hard error to the JSON loader.
    unknown_constraint = methods(json!([{"constraint": "parameter", "index": 0}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

faithful! {
    unknown_constraint_not = methods(json!([not(json!({"constraint": "frobnicate"}))]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

faithful! {
    /// `in_function` under `find: methods` is refused.
    in_function_on_methods = methods(json!([{"constraint": "in_function", "inner": sig_name("main")}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

// ---------------------------------------------------------------------------
// in
// ---------------------------------------------------------------------------

fn scoped(scope: Value, where_: Value) -> Value {
    let mut g = methods(where_);
    g["in"] = scope;
    g
}

faithful! {
    in_language = scoped(json!({"language": "lua"}), json!([]));
    java, native, lua, cil, unknown,
}

faithful! {
    in_languages = scoped(json!({"languages": ["dex", "pcode"]}), json!([not(sig_name("get"))]));
    java, native, lua, cil,
}

faithful! {
    in_import = scoped(json!({"import": "lib"}), json!([any_of(json!([sig_name("bar"), sig_name("get")]))]));
    java, native, lua, cil,
}

faithful! {
    in_language_and_import = scoped(json!({"language": "pcode", "import": "lib"}), json!([{"constraint": "uses_field", "name": "x"}]));
    java, native,
}

faithful! {
    in_unknown_language = scoped(json!({"language": "cobol"}), json!([sig_name("get")]));
    java,
}

/// `in` against two imports at once: only the admitted one contributes.
#[test]
fn in_scope_across_two_imports() {
    let generator = scoped(json!({"language": "lua"}), json!([sig_name("deposit")]));
    assert_faithful(&generator, &[frontend("java"), frontend("lua")]);
}

/// An import with no identity is admitted only by an unscoped generator.
#[test]
fn in_scope_on_an_anonymous_import() {
    let generator = scoped(json!({"language": "dex"}), json!([sig_name("get")]));
    assert_faithful(&generator, &[(ImportScope::unknown(), java())]);
}

// ---------------------------------------------------------------------------
// find: callsites
// ---------------------------------------------------------------------------

faithful! {
    callsites_by_callee = callsites(json!([any_of(json!([
        sig_name("exec"), sig_name("system"), sig_name("execute"), sig_name("format"), sig_name("N"), sig_name("f1"),
    ]))]));
    java, native, lua, cil, unknown,
}

faithful! {
    callsites_in_function = callsites(json!([
        sig_name("exec"),
        {"constraint": "in_function", "inner": sig_name("main")},
    ]));
    java,
}

faithful! {
    callsites_in_function_native = callsites(json!([
        sig_name("system"),
        {"constraint": "in_function", "inner": {"constraint": "signature_match", "qualified-id": "Baz::bar"}},
    ]));
    native,
}

faithful! {
    callsites_in_function_any_of = callsites(json!([
        sig_name("exec"),
        {"constraint": "in_function", "inner": any_of(json!([sig_name("main"), sig_name("run")]))},
    ]));
    java,
}

faithful! {
    /// `not` inside `in_function` complements the caller set.
    callsites_in_function_inner_not = callsites(json!([
        sig_name("exec"),
        {"constraint": "in_function", "inner": not(sig_name("main"))},
    ]));
    java,
}

faithful! {
    /// `not` *around* `in_function`. The JSON matcher evaluates the inner against the `not`'s
    /// scratch set, which narrows the *callee* set by the caller's constraint: callees not named
    /// `main`, called from anywhere. The DSL reads it as "not called from `main`".
    callsites_not_in_function = callsites(json!([
        sig_name("exec"),
        not(json!({"constraint": "in_function", "inner": sig_name("main")})),
    ]));
    java => Known {
        reason: IN_FUNCTION_BUG,
        diff: &[
            "JSON only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Main;->main([Ljava/lang/String;)V Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "JSON only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Main;->main([Ljava/lang/String;)V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    callsites_any_of_in_function = callsites(json!([
        sig_name("exec"),
        any_of(json!([
            {"constraint": "in_function", "inner": sig_name("main")},
            {"constraint": "in_function", "inner": sig_name("run")},
        ])),
    ]));
    java => Known {
        reason: IN_FUNCTION_BUG,
        diff: &[
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Main;->main([Ljava/lang/String;)V Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Main;->main([Ljava/lang/String;)V Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Mid;->run()V Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Mid;->run()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    callsites_not_callee = callsites(json!([not(sig_name("exec"))]));
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only callsite endpoints: f1 in f2 Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: f1 in f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    java, native, lua, cil,
}

/// The callees a `find: callsites` load anchored endpoints at, before any expansion over call
/// sites.
fn callsite_callees(m: &ProgramModelMatches) -> BTreeSet<String> {
    m.endpoints
        .iter()
        .filter(|e| e.callsite_scoped)
        .map(|e| e.function.to_string())
        .collect()
}

/// What the call-site comparison above normalizes away. The JSON matcher gives *every*
/// matching callee a call-site endpoint, and Stage 2 anchors it at the sites the resolved call
/// graph sends there -- including a virtual call that names the base method (`Base.run`) and
/// that CHA resolves to an override (`Mid.run`). The DSL joins on the callee the site names, so
/// it anchors only `Base.run`, and only where a site spells it. The migration keeps the DSL's
/// static meaning; a resolved-callee relation is future work (`DESIGN.md`, `resolves_to`).
#[test]
fn callsites_anchor_at_every_matched_callee() {
    let generator = callsites(json!([sig_name("run")]));
    let (scope, program) = frontend("java");
    let index = ProgramMatchIndex::new(&program, scope);
    let mut from_json = ProgramModelMatches::default();
    try_load_models_from_values(
        &index,
        std::iter::once(Ok(generator.clone())),
        &mut from_json,
    )
    .expect("json");
    let (text, _) = dsl::migrate::migrate_generators(std::iter::once(&generator), None);
    let file = dsl::DslFile::from_text("migrated.ctadl", text).expect("dsl");
    let set = dsl::DslModelSet { files: vec![file] };
    let mut matcher = dsl::DslMatcher::new(&set, dsl::Phase::All);
    let mut from_dsl = ProgramModelMatches::default();
    matcher.observe_import(&index, &mut from_dsl);
    matcher.finish(&mut from_dsl).expect("finish");
    let callees = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(
        callsite_callees(&from_json),
        callees(&[J_BASE_RUN, J_MID_RUN])
    );
    assert_eq!(callsite_callees(&from_dsl), callees(&[J_BASE_RUN]));
}

faithful! {
    callsites_uses_field =callsites(json!([{"constraint": "uses_field", "name": "secret"}]));
    java,
}

faithful! {
    /// A propagation is a function-level fact, refused under `find: callsites`.
    callsites_propagation = json!({
        "find": "callsites",
        "where": [sig_name("exec")],
        "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
    });
    java,
}

// ---------------------------------------------------------------------------
// find: dispatch
// ---------------------------------------------------------------------------

faithful! {
    dispatch_name = dispatch(json!([sig_name("next")]));
    java, cil, native,
}

faithful! {
    dispatch_parent = dispatch(json!([{"constraint": "signature_match", "parent": "Ljava/lang/Runtime;"}]));
    java,
}

faithful! {
    dispatch_qualified_id = dispatch(json!([{"constraint": "signature_match", "qualified-id": "[App]B::N(System.Int32)->System.Int32"}]));
    cil, java,
}

faithful! {
    dispatch_signature = dispatch(json!([{"constraint": "signature", "pattern": "^\\(I"}]));
    java, cil,
}

faithful! {
    dispatch_not = dispatch(json!([not(sig_name("next"))]));
    java, cil,
}

faithful! {
    dispatch_parent_constraint = dispatch(json!([{"constraint": "parent", "inner": {"constraint": "name", "pattern": "Leaf|Iterator"}}]));
    java,
}

faithful! {
    dispatch_extends = dispatch(json!([{"constraint": "extends", "inner": sig_name("Lapp/Mid;")}]));
    java,
}

faithful! {
    dispatch_any_of = dispatch(json!([any_of(json!([sig_name("next"), sig_name("put")]))]));
    java,
}

faithful! {
    dispatch_no_where = json!({"find": "dispatch", "model": {"propagation": []}});
    java, cil,
}

faithful! {
    dispatch_resolve_inline = json!({"find": "dispatch", "where": [sig_name("exec")], "model": {"resolve": "inline"}});
    java,
}

faithful! {
    /// `resolve: inline` with a `propagation` is one disposition: inline, plus the summary.
    dispatch_resolve_inline_with_propagation = json!({
        "find": "dispatch",
        "where": [sig_name("exec")],
        "model": {"resolve": "inline", "propagation": [{"input": "Argument(1)", "output": "Return"}]},
    });
    #[ignore = "migrator divergence: `resolve` drops the `propagation` beside it"]
    java,
}

faithful! {
    dispatch_closure_shaped = json!({"find": "dispatch", "where": [sig_name("next")], "model": {"closure_shaped": true}});
    java,
}

faithful! {
    dispatch_closure_shaped_false = json!({"find": "dispatch", "where": [sig_name("next")], "model": {"closure_shaped": false}});
    java,
}

faithful! {
    dispatch_two_propagations = json!({
        "find": "dispatch",
        "where": [sig_name("put")],
        "model": {"propagation": [
            {"input": "Argument(1)", "output": "Argument(0).count"},
            {"input": "Argument(0)", "output": "Return"},
        ]},
    });
    java,
}

faithful! {
    /// Only one of `resolve` / `propagation` and `closure_shaped`.
    dispatch_two_dispositions = json!({
        "find": "dispatch",
        "where": [sig_name("next")],
        "model": {"closure_shaped": true, "resolve": "inline"},
    });
    #[ignore = "migrator divergence: conflicting dispatch dispositions are not refused"]
    java,
}

faithful! {
    dispatch_with_source = json!({
        "find": "dispatch",
        "where": [sig_name("next")],
        "model": {"propagation": [], "sources": [{"kind": "Src", "port": "Return"}]},
    });
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    /// `in` applies to a dispatch generator like any other: the import is not `pcode`.
    dispatch_in_scope = json!({
        "find": "dispatch",
        "in": {"language": "pcode"},
        "where": [sig_name("next")],
        "model": {"propagation": []},
    });
    #[ignore = "migrator divergence: `in` is dropped on a dispatch generator"]
    java,
}

faithful! {
    /// Function-level constraints are refused under `find: dispatch`.
    dispatch_has_code = dispatch(json!([{"constraint": "has_code", "value": true}]));
    java,
}

faithful! {
    dispatch_number_parameters = dispatch(json!([arity("==", 1)]));
    java,
}

faithful! {
    dispatch_uses_field = dispatch(json!([{"constraint": "uses_field", "name": "count"}]));
    java,
}

faithful! {
    dispatch_in_function = dispatch(json!([sig_name("next"), {"constraint": "in_function", "inner": sig_name("main")}]));
    #[ignore = "migrator divergence: a skipped constraint becomes true instead of refusing the generator"]
    java,
}

// ---------------------------------------------------------------------------
// Model keys
// ---------------------------------------------------------------------------

fn with_model(where_: Value, model: Value) -> Value {
    json!({"find": "methods", "where": where_, "model": model})
}

faithful! {
    model_ports_and_paths = with_model(json!([sig_name("get")]), json!({
        "sources": [
            {"kind": "A", "port": "Argument(1).f.[8]", "saturating": true},
            {"kind": "B", "port": "Argument(*)"},
            {"kind": "C", "port": "Return.\\[]", "saturating": false},
        ],
        "sinks": [
            {"kind": "D", "port": "Argument(0).x", "wildcard": false},
            {"kind": "E", "port": "Return", "wildcard": true},
            {"kind": "F", "port": "Argument(2).\"q.r\""},
        ],
        "propagation": [
            {"input": "Argument(*)", "output": "Return.out"},
            {"input": "Argument(1).[0]", "output": "Argument(0).*"},
        ],
    }));
    java, native, lua, cil,
}

faithful! {
    model_access_paths = with_model(json!([sig_name("get")]), json!({"access_paths": ["a.b", "c.[4].d"]}));
    java,
}

faithful! {
    /// `access_paths` needs no match: the JSON loader registers them whatever `where` selected.
    model_access_paths_unmatched = with_model(json!([sig_name("no_such_method")]), json!({"access_paths": ["a.b"]}));
    java,
}

faithful! {
    model_access_paths_invalid = with_model(json!([]), json!({"access_paths": ["a..b"]}));
    java,
}

faithful! {
    /// `modes: ["skip-analysis"]` marks the matched functions.
    model_modes_skip_analysis = with_model(json!([sig_name("get")]), json!({
        "modes": ["skip-analysis"],
        "propagation": [{"input": "Argument(1)", "output": "Return"}],
    }));
    #[ignore = "migrator divergence: `modes: skip-analysis` is dropped as if it had no effect"]
    java,
}

faithful! {
    /// `taint` and `forward_self` are schema-only: both loaders ignore them.
    model_schema_only_keys = with_model(json!([sig_name("get")]), json!({
        "taint": [], "forward_self": true,
        "sources": [{"kind": "S", "port": "Return"}],
    }));
    java,
}

faithful! {
    /// A source with no `kind` is refused, not defaulted.
    model_source_without_kind = with_model(json!([sig_name("get")]), json!({"sources": [{"port": "Return"}]}));
    #[ignore = "migrator divergence: a missing `kind` is defaulted instead of refusing"]
    java,
}

faithful! {
    model_sink_with_saturating = with_model(json!([sig_name("get")]), json!({"sinks": [{"kind": "K", "port": "Return", "saturating": true}]}));
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    model_source_with_wildcard = with_model(json!([sig_name("get")]), json!({"sources": [{"kind": "K", "port": "Return", "wildcard": true}]}));
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    model_propagation_with_wildcard = with_model(json!([sig_name("get")]), json!({"propagation": [{"input": "Argument(0)", "output": "Return", "wildcard": true}]}));
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    /// A `Variable(name)` source port selects a named local of the matched function.
    model_variable_port = with_model(json!([sig_name("get")]), json!({"sources": [{"kind": "K", "port": "Variable(loaded)"}]}));
    #[ignore = "migrator divergence: `Variable(...)` ports are not translated"]
    java,
}

faithful! {
    model_bad_port = with_model(json!([sig_name("get")]), json!({"sources": [{"kind": "K", "port": "Argument(x)"}]}));
    java,
}

faithful! {
    model_unknown_key = with_model(json!([sig_name("get")]), json!({"propagations": [{"input": "Argument(0)", "output": "Return"}]}));
    java,
}

faithful! {
    /// A misspelled generator key is refused rather than dropped.
    generator_unknown_key = json!({"find": "methods", "wehre": [sig_name("get")], "model": method_model()});
    #[ignore = "migrator divergence: an unknown key is ignored instead of refusing the generator"]
    java,
}

faithful! {
    generator_without_find = json!({"where": [sig_name("get")], "model": method_model()});
    #[ignore = "migrator divergence: a missing `find` defaults to methods instead of refusing"]
    java,
}

// ---------------------------------------------------------------------------
// Constraint order
// ---------------------------------------------------------------------------

faithful! {
    /// A `not` written before the constraint that narrows the subject. `where` is a
    /// conjunction, so the order means nothing to the JSON matcher.
    not_before_a_positive_constraint = methods(json!([
        not(sig_name("get")),
        {"constraint": "has_code", "value": true},
    ]));
    #[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
    java,
    #[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
    native,
    #[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
    lua,
    #[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
    cil,
    #[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
    unknown,
}

faithful! {
    not_before_a_positive_constraint_callsites = callsites(json!([
        not(sig_name("get")),
        sig_name("exec"),
    ]));
    java,
}

// ---------------------------------------------------------------------------
// The universe `All` and `not` range over
// ---------------------------------------------------------------------------
//
// A JSON generator's universe is the method table: `java_unnamed` and `native_unnamed` each
// lower one function the table does not name, and an `Unknown` table names none at all.

faithful! {
    universe_no_where = json!({"find": "methods", "model": method_model()});
    java_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: Lapp/Synth;->lambda$0()V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    native_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: FUN_3000: Return <- Index(0)",
            "DSL only endpoints: FUN_3000 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: FUN_3000 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    universe_not = methods(json!([not(sig_name("get"))]));
    java_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: Lapp/Synth;->lambda$0()V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    native_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: FUN_3000: Return <- Index(0)",
            "DSL only endpoints: FUN_3000 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: FUN_3000 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    universe_not_has_code_false = methods(json!([not(json!({"constraint": "has_code", "value": false}))]));
    java_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: Lapp/Synth;->lambda$0()V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    native_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: FUN_3000: Return <- Index(0)",
            "DSL only endpoints: FUN_3000 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: FUN_3000 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    /// A body-dependent constraint reaches every lowered function, named by the table or not.
    universe_has_code = methods(json!([{"constraint": "has_code", "value": true}]));
    java_unnamed, native_unnamed, unknown,
}

faithful! {
    /// ... so a lowered function the table does not name can be matched positively but never
    /// through `not`.
    universe_any_of_containing_not = methods(json!([any_of(json!([not(sig_name("get")), sig_name("get")]))]));
    java_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: Lapp/Synth;->lambda$0()V: Return <- Index(0)",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: Lapp/Synth;->lambda$0()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    native_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: FUN_3000: Return <- Index(0)",
            "DSL only endpoints: FUN_3000 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: FUN_3000 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only propagations: f1: Return <- Index(0)",
            "DSL only propagations: f2: Return <- Index(0)",
            "DSL only endpoints: f1 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f1 Forward Return kind=Src wildcard=false saturating=false local=None",
            "DSL only endpoints: f2 Backward Index(0) kind=Snk wildcard=true saturating=false local=None",
            "DSL only endpoints: f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    universe_callsites_in_function_not = callsites(json!([
        sig_name("exec"),
        {"constraint": "in_function", "inner": not(sig_name("main"))},
    ]));
    java_unnamed => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Synth;->lambda$0()V Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: Ljava/lang/Runtime;->exec(Ljava/lang/String;)Ljava/lang/Process; in Lapp/Synth;->lambda$0()V Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
}

faithful! {
    universe_callsites_no_where = callsites(json!([]));
    unknown => Known {
        reason: JSON_UNIVERSE,
        diff: &[
            "DSL only callsite endpoints: f1 in f2 Backward Index(0) kind=Snk wildcard=false saturating=false local=None",
            "DSL only callsite endpoints: f1 in f2 Forward Return kind=Src wildcard=false saturating=false local=None",
        ],
    },
    java_unnamed, native_unnamed,
}

// ---------------------------------------------------------------------------
// Bridges
// ---------------------------------------------------------------------------

/// A Lua callee bridged to its native implementation, across two imports.
fn bridge(from_where: Value, to_where: Value, arguments: Option<Value>) -> Value {
    let mut bridge = json!({
        "to": {"in": {"language": "pcode"}, "where": to_where},
    });
    if let Some(arguments) = arguments {
        bridge["arguments"] = arguments;
    }
    json!({
        "find": "methods",
        "in": {"language": "lua"},
        "where": from_where,
        "model": {"bridge": bridge},
    })
}

fn bridge_imports() -> Vec<Import> {
    vec![frontend("lua"), frontend("native")]
}

fn arguments() -> Value {
    json!([
        {"from": "Argument(0)", "to": "Argument(0).stack.[1]", "direction": "in"},
        {"from": "Return", "to": "Return", "direction": "out"},
        {"from": "Argument(1).f", "to": "Argument(1)", "direction": "both"},
    ])
}

/// A port pair with no `direction` is `both` to the JSON loader (`spec::Direction`'s default).
#[ignore = "migrator divergence: a bridge pair with no `direction` becomes `in`, not `both`"]
#[test]
fn bridge_default_direction() {
    assert_faithful(
        &bridge(
            json!([sig_name("source")]),
            json!([sig_name("strcpy")]),
            Some(json!([{"from": "Argument(2)", "to": "Argument(0)"}])),
        ),
        &bridge_imports(),
    );
}

/// Every direction, against a one-to-one pairing.
#[test]
fn bridge_with_arguments() {
    assert_faithful(
        &bridge(
            json!([sig_name("source")]),
            json!([sig_name("strcpy")]),
            Some(arguments()),
        ),
        &bridge_imports(),
    );
}

/// Many-to-many: every `from` pairs with every `to`.
#[test]
fn bridge_cross_product() {
    assert_faithful(
        &bridge(
            json!([any_of(json!([sig_name("deposit"), sig_name("withdraw")]))]),
            json!([sig_name("bar")]),
            Some(arguments()),
        ),
        &bridge_imports(),
    );
}

/// `not` on side A.
#[ignore = "migrator divergence: a leading `not` is emitted before the atom that binds F"]
#[test]
fn bridge_not_on_the_from_side() {
    assert_faithful(
        &bridge(
            json!([not(sig_name("source")), {"constraint": "has_code", "value": true}]),
            json!([sig_name("strcpy")]),
            Some(arguments()),
        ),
        &bridge_imports(),
    );
}

/// A disjunction on side B is a union.
#[ignore = "migrator divergence: only the first alternative of a bridge's `to` disjunction is kept"]
#[test]
fn bridge_disjunction_on_the_to_side() {
    assert_faithful(
        &bridge(
            json!([sig_name("source")]),
            json!([any_of(json!([sig_name("strcpy"), sig_name("system")]))]),
            Some(arguments()),
        ),
        &bridge_imports(),
    );
}

/// No `arguments`: the JSON loader falls back to an identity map over the shared arity, which
/// the migration cannot write as a rule.
#[ignore = "migrator divergence: a bridge with no `arguments` carries only the return value"]
#[test]
fn bridge_without_arguments() {
    assert_faithful(
        &bridge(
            json!([sig_name("source")]),
            json!([sig_name("strcpy")]),
            None,
        ),
        &bridge_imports(),
    );
}

/// The `to` side's scope admits neither import.
#[test]
fn bridge_to_side_out_of_scope() {
    let mut g = bridge(
        json!([sig_name("source")]),
        json!([sig_name("strcpy")]),
        Some(arguments()),
    );
    g["model"]["bridge"]["to"]["in"] = json!({"language": "cil"});
    assert_faithful(&g, &bridge_imports());
}
