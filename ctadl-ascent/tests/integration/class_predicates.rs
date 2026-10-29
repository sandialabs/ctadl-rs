//! The JSON `parent` / `extends` constraints, and their migration to the DSL.
//!
//! Each case loads one generator twice against the same program: through the JSON matcher
//! directly, and through the migrator and the DSL engine. The two must match the same
//! functions (or, for `find: dispatch`, the same signature keys). The programs give each
//! frontend the shape that tells a faithful translation from a near miss: a Java and a CIL class
//! with two direct supertypes, which is what separates "one supertype satisfies the whole
//! predicate" from "each part is satisfied by some supertype"; and a Lua class, which the DSL's
//! `parent` sees and the JSON's does not.

use ctadl_ascent::models::dsl::migrate::migrate_generators;
use ctadl_ascent::models::{
    DispatchKeys, DslFile, DslMatcher, DslModelSet, DslPhase, ImportScope, ProgramMatchIndex,
    ProgramModelMatches, try_load_models_from_values,
};
use ctadl_ascent::project::ArtifactLanguage;
use ctadl_ir::mir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, JavaClass, JavaMethod, JavaSignature,
    JavaSimpleName, NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName,
    VirtualMethodTable,
};
use ctadl_ir::mir::{
    BasicBlockData, FunctionData, Functions, ParameterType, Program, ProgramInfo, Statement,
    StatementKind, Symbol,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// A one-parameter function with a body, so a propagation from `Argument(0)` has a formal.
fn function(name: &str) -> FunctionData {
    let mut f = FunctionData::default();
    f.set_name(name.to_string());
    f.params.parameters.push(ParameterType::SharedFields);
    let blocks = f.blocks.blocks_mut();
    let body = blocks.push(BasicBlockData::new(None));
    blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
    f
}

fn program(vmt: VirtualMethodTable, names: &[String], extra: Vec<FunctionData>) -> ProgramInfo {
    let funcs: Vec<_> = names.iter().map(|n| function(n)).chain(extra).collect();
    ProgramInfo {
        vmt,
        program: Program::new(Functions::new(funcs)),
        ..Default::default()
    }
}

/// The Java classes, as `(class, direct supertypes)`. `C` has two supertypes, `E` none at all,
/// and `I` is a supertype that declares no method.
const JAVA_CLASSES: &[(&str, &[&str])] = &[
    ("Lp/A;", &["Ljava/lang/Object;"]),
    ("Lp/B;", &[]),
    ("Lp/C;", &["Lp/A;", "Lp/I;"]),
    ("Lp/D;", &["Lp/A;"]),
    ("Lp/E;", &[]),
];

fn java_method(cls: &str) -> String {
    format!("{cls}->m(I)V")
}

/// One `JavaCall` per class name, so the dispatch universe has a key on each of them.
fn dispatch_caller(classes: &[&str]) -> FunctionData {
    use ctadl_ir::Idx;
    use ctadl_ir::builder::FunctionBuilder;
    use ctadl_ir::mir::Exp;
    use ctadl_ir::mir::call::{CallStyle, JavaDispatch};

    let mut f = FunctionData {
        name: "Lq/Caller;->run()V".to_string(),
        ..Default::default()
    };
    f.params.parameters.push(ParameterType::Shared);
    let mut fb = FunctionBuilder::new(&mut f);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    for cls in classes {
        let recv = b.new_local_var("recv");
        let result = b.new_local_var("result");
        b.create_call(
            CallStyle::JavaCall {
                receiver: recv,
                cls: (*cls).into(),
                simple_name: "m".into(),
                descriptor: "(I)V".into(),
                dispatch: JavaDispatch::Virtual,
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

fn java_program() -> ProgramInfo {
    java_program_and(Vec::new())
}

fn java_program_and(extra: Vec<FunctionData>) -> ProgramInfo {
    let methods = JAVA_CLASSES
        .iter()
        .map(|(cls, _)| {
            (
                JavaClass((*cls).into()),
                JavaSimpleName("m".into()),
                JavaSignature("(I)V".into()),
                JavaMethod(java_method(cls).as_str().into()),
            )
        })
        .collect();
    let hierarchy = JAVA_CLASSES
        .iter()
        .filter(|(_, supers)| !supers.is_empty())
        .map(|(cls, supers)| {
            (
                JavaClass((*cls).into()),
                supers.iter().map(|s| JavaClass((*s).into())).collect(),
            )
        })
        .collect();
    let names: Vec<String> = JAVA_CLASSES.iter().map(|(c, _)| java_method(c)).collect();
    program(
        VirtualMethodTable::Java {
            methods,
            hierarchy,
            interfaces: vec![JavaClass("Lp/I;".into())],
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        },
        &names,
        extra,
    )
}

/// [`java_program`], plus a caller with a call to `m` on every class, `I` included, for the
/// call-site and dispatch generators.
///
/// Kept apart because the caller is a function the method table does not name, and a method
/// generator's top-level `not` complements against the method table in the JSON but against
/// every function in the DSL. That is not a class question, and it would drown this file's.
fn java_program_with_sites() -> ProgramInfo {
    let mut keyed: Vec<&str> = JAVA_CLASSES.iter().map(|(c, _)| *c).collect();
    keyed.push("Lp/I;");
    java_program_and(vec![dispatch_caller(&keyed)])
}

/// The Java shape over .NET spellings.
fn cil_class(java: &str) -> String {
    let simple = java.trim_start_matches("Lp/").trim_end_matches(';');
    match java {
        "Ljava/lang/Object;" => "[System.Private.CoreLib]System.Object".to_string(),
        _ => format!("[App]P.{simple}"),
    }
}

fn cil_method(cls: &str) -> String {
    format!("{cls}::m(System.Int32)->System.Void")
}

fn cil_program() -> ProgramInfo {
    let classes: Vec<(String, Vec<String>)> = JAVA_CLASSES
        .iter()
        .map(|(c, s)| (cil_class(c), s.iter().map(|s| cil_class(s)).collect()))
        .collect();
    let methods = classes
        .iter()
        .map(|(cls, _)| {
            (
                CilClass(cls.as_str().into()),
                CilSimpleName("m".into()),
                CilSignature("(System.Int32)->System.Void".into()),
                CilMethod(cil_method(cls).as_str().into()),
            )
        })
        .collect();
    let hierarchy = classes
        .iter()
        .filter(|(_, supers)| !supers.is_empty())
        .map(|(cls, supers)| {
            (
                CilClass(cls.as_str().into()),
                supers.iter().map(|s| CilClass(s.as_str().into())).collect(),
            )
        })
        .collect();
    let names: Vec<String> = classes.iter().map(|(c, _)| cil_method(c)).collect();
    program(
        VirtualMethodTable::Cil {
            methods,
            hierarchy,
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            pinvokes: Vec::new(),
        },
        &names,
        Vec::new(),
    )
}

/// A metatable class with a method and a recovered supertype, and a plain function. The DSL's
/// `fun(F, parent = P)` and `subclass` see the class; the JSON's `parent` and `extends` do not.
fn lua_program() -> ProgramInfo {
    let account = "lua$class$Account";
    let base = "lua$class$Base";
    let deposit = "m.Account.deposit";
    let helper = "m.helper";
    program(
        VirtualMethodTable::Lua {
            methods: vec![(account.into(), "deposit".into(), deposit.into())],
            functions: vec![
                (Symbol::from("deposit"), Symbol::from(deposit)),
                (Symbol::from("helper"), Symbol::from(helper)),
            ],
            externals: Vec::new(),
            hierarchy: [(
                Symbol::from(account),
                smallvec::smallvec![Symbol::from(base)],
            )]
            .into_iter()
            .collect(),
        },
        &[deposit.to_string(), helper.to_string()],
        Vec::new(),
    )
}

fn native_program() -> ProgramInfo {
    let names = ["system", "Lp/C;"];
    let methods = names
        .iter()
        .map(|n| {
            (
                NativeSimpleName((*n).into()),
                NativeSignature(format!("{n}()").into()),
                NativeFunction((*n).into()),
                NativeQualifiedName((*n).into()),
            )
        })
        .collect();
    program(
        VirtualMethodTable::Native { methods },
        &names.map(String::from),
        Vec::new(),
    )
}

/// Every program, under both the scope a real import has and none at all. The JSON keys its
/// class constraints off the method table, not the declared language, so the two must agree
/// whichever the scope says.
fn programs(java: fn() -> ProgramInfo) -> Vec<(String, ProgramInfo, ImportScope)> {
    let mut out = Vec::new();
    for (name, build, language) in [
        ("java", java, ArtifactLanguage::Dex),
        ("cil", cil_program, ArtifactLanguage::Cil),
        ("lua", lua_program, ArtifactLanguage::Lua),
        ("native", native_program, ArtifactLanguage::Pcode),
    ] {
        out.push((
            format!("{name} (scoped)"),
            build(),
            ImportScope::new(language, "app"),
        ));
        out.push((
            format!("{name} (unscoped)"),
            build(),
            ImportScope::unknown(),
        ));
    }
    out
}

/// What one generator selected: function ids for a method or call-site generator, signature
/// keys (as `cls->name desc`) for a dispatch one.
type Selected = BTreeSet<String>;

fn selected(matches: &ProgramModelMatches) -> Selected {
    let functions = matches.propagations.iter().map(|p| p.function.to_string());
    let sites = matches.endpoints.iter().map(|e| e.function.to_string());
    let keys = matches
        .closure_shaped
        .iter()
        .map(|(cls, name, desc)| format!("{cls}->{name}{desc}"));
    functions.chain(sites).chain(keys).collect()
}

fn with_model(find: &str, where_: Value) -> Value {
    let model = if find == "dispatch" {
        json!({"closure_shaped": true})
    } else {
        json!({"propagation": [{"input": "Argument(0)", "output": "Return"}]})
    };
    json!({"find": find, "where": where_, "model": model})
}

/// Loads `generator` through the JSON matcher and through the migrated DSL, and returns both.
fn both(
    generator: &Value,
    info: &ProgramInfo,
    scope: &ImportScope,
) -> (Selected, Selected, String) {
    let keys = DispatchKeys::from_program(&info.program);
    let index = ProgramMatchIndex::new_with_dispatch(info, scope.clone(), Some(&keys));

    let mut from_json = ProgramModelMatches::default();
    try_load_models_from_values(
        &index,
        std::iter::once(Ok(generator.clone())),
        &mut from_json,
    )
    .unwrap_or_else(|e| panic!("JSON load of {generator}: {e}"));

    let (text, report) = migrate_generators(std::iter::once(generator), None);
    assert!(
        report.warnings.is_empty(),
        "migrating {generator} warned: {:?}\n{text}",
        report.warnings
    );
    let file = DslFile::from_text("migrated.ctadl", text.clone())
        .unwrap_or_else(|e| panic!("migrated {generator} does not load: {e}\n{text}"));
    let set = DslModelSet { files: vec![file] };
    let mut matcher = DslMatcher::new(&set, DslPhase::All);
    let mut from_dsl = ProgramModelMatches::default();
    matcher.observe_import(&index, &mut from_dsl);
    matcher
        .finish(&mut from_dsl)
        .unwrap_or_else(|e| panic!("DSL run of {generator}: {e}"));
    (selected(&from_json), selected(&from_dsl), text)
}

/// The Java program `generator` is checked against: [`java_program_with_sites`] for a generator
/// that ranges over call sites or signature keys, which only the caller there has.
fn java_for(generator: &Value) -> fn() -> ProgramInfo {
    match generator["find"].as_str() {
        Some("methods") => java_program,
        _ => java_program_with_sites,
    }
}

/// Requires the JSON and the migrated DSL to agree on every program.
fn agree(generator: Value) {
    for (name, info, scope) in programs(java_for(&generator)) {
        let (json, dsl, text) = both(&generator, &info, &scope);
        assert_eq!(
            json, dsl,
            "JSON and migrated DSL disagree on {name} for {generator}\nmigrated:\n{text}"
        );
    }
}

/// [`agree`], plus what the JSON selects on the Java program, so a case cannot pass by both
/// sides matching nothing.
fn agree_on_java(generator: Value, expected: &[&str]) {
    agree(generator.clone());
    let (json, _, _) = both(&generator, &java_for(&generator)(), &ImportScope::unknown());
    let expected: Selected = expected.iter().map(|c| java_method(c)).collect();
    assert_eq!(json, expected, "the JSON selection for {generator}");
}

fn sig(name: &str) -> Value {
    json!({"constraint": "signature_match", "name": name})
}

fn name(pattern: &str) -> Value {
    json!({"constraint": "name", "pattern": pattern})
}

fn not(inner: Value) -> Value {
    json!({"constraint": "not", "inner": inner})
}

fn all_of(inners: Vec<Value>) -> Value {
    json!({"constraint": "all_of", "inners": inners})
}

fn any_of(inners: Vec<Value>) -> Value {
    json!({"constraint": "any_of", "inners": inners})
}

fn parent(inner: Value) -> Value {
    json!({"constraint": "parent", "inner": inner})
}

fn extends(inner: Value) -> Value {
    json!({"constraint": "extends", "inner": inner})
}

fn methods(constraints: Vec<Value>) -> Value {
    with_model("methods", Value::Array(constraints))
}

#[test]
fn parent_leaves() {
    agree_on_java(methods(vec![parent(sig("Lp/A;"))]), &["Lp/A;"]);
    // `names` and `name` together are one membership test.
    agree_on_java(
        methods(vec![parent(json!({"constraint": "signature_match",
                                    "name": "Lp/A;", "names": ["Lp/B;"]}))]),
        &["Lp/A;", "Lp/B;"],
    );
    // Unanchored, as the JSON's `is_match` is.
    agree_on_java(methods(vec![parent(name("C"))]), &["Lp/C;"]);
    // No name at all is a predicate no class satisfies.
}

/// The Lua divergence: the DSL's `parent` covers metatable classes, the JSON's does not, so a
/// positive predicate must fail on Lua and a negated one must hold of everything there.
#[test]
fn parent_matches_nothing_outside_java_and_cil() {
    agree_on_java(
        methods(vec![parent(name(".*"))]),
        &["Lp/A;", "Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;"],
    );
    agree(methods(vec![not(parent(sig("lua$class$Account")))]));
    agree(methods(vec![not(parent(name(".*")))]));
    agree(methods(vec![extends(name(".*"))]));
    agree(methods(vec![not(extends(sig("lua$class$Base")))]));

    let lua = lua_program();
    let (json, _, _) = both(
        &methods(vec![not(parent(sig("lua$class$Account")))]),
        &lua,
        &ImportScope::unknown(),
    );
    assert_eq!(
        json,
        ["m.Account.deposit", "m.helper"]
            .map(String::from)
            .into_iter()
            .collect(),
        "the JSON's negated `parent` on Lua complements an empty match"
    );
}

#[test]
fn parent_with_a_negated_inner() {
    agree_on_java(
        methods(vec![parent(not(sig("Lp/C;")))]),
        &["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"],
    );
    agree_on_java(
        methods(vec![parent(not(name("^Lp/[AB];$")))]),
        &["Lp/C;", "Lp/D;", "Lp/E;"],
    );
    agree_on_java(methods(vec![parent(not(not(sig("Lp/C;"))))]), &["Lp/C;"]);
}

#[test]
fn parent_with_nested_combinators() {
    // A class under `Lp/` that is neither `A` nor anything matching `D`.
    agree_on_java(
        methods(vec![parent(all_of(vec![
            name("^Lp/"),
            not(any_of(vec![sig("Lp/A;"), name("D")])),
        ]))]),
        &["Lp/B;", "Lp/C;", "Lp/E;"],
    );
    agree_on_java(
        methods(vec![parent(any_of(vec![
            all_of(vec![name("^Lp/"), not(name("[A-C]"))]),
            sig("Lp/A;"),
        ]))]),
        &["Lp/A;", "Lp/D;", "Lp/E;"],
    );
    // The empty combinators: `all_of` holds of every class, `any_of` of none.
    agree_on_java(
        methods(vec![parent(all_of(vec![]))]),
        &["Lp/A;", "Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;"],
    );
    agree_on_java(methods(vec![parent(any_of(vec![]))]), &[]);
    agree(methods(vec![not(parent(any_of(vec![])))]));
    agree(methods(vec![not(parent(all_of(vec![])))]));
}

#[test]
fn parent_under_an_outer_not() {
    agree_on_java(
        methods(vec![not(parent(sig("Lp/C;")))]),
        &["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"],
    );
    agree_on_java(
        methods(vec![not(parent(any_of(vec![sig("Lp/A;"), name("D")])))]),
        &["Lp/B;", "Lp/C;", "Lp/E;"],
    );
    agree_on_java(
        methods(vec![not(parent(all_of(vec![
            name("^Lp/"),
            not(sig("Lp/A;")),
        ])))]),
        &["Lp/A;"],
    );
    // Alongside other constraints, and inside a `where`-level combinator.
    agree_on_java(
        methods(vec![
            json!({"constraint": "signature_match", "name": "m"}),
            not(any_of(vec![parent(sig("Lp/A;")), extends(sig("Lp/A;"))])),
        ]),
        &["Lp/B;", "Lp/E;"],
    );
}

/// The `extends` divergence: one direct supertype must satisfy the *whole* predicate. `C`'s
/// two supertypes are `A` and `I`, so no single one is both.
#[test]
fn extends_tests_one_supertype_against_the_whole_predicate() {
    agree_on_java(
        methods(vec![extends(all_of(vec![sig("Lp/A;"), sig("Lp/I;")]))]),
        &[],
    );
    agree_on_java(
        methods(vec![extends(any_of(vec![sig("Lp/A;"), sig("Lp/I;")]))]),
        &["Lp/C;", "Lp/D;"],
    );
    agree_on_java(
        methods(vec![extends(all_of(vec![name("^Lp/"), not(sig("Lp/A;"))]))]),
        &["Lp/C;"],
    );
    // Direct supertypes only: `C` reaches `Object` through `A`, but does not extend it.
    agree_on_java(
        methods(vec![extends(sig("Ljava/lang/Object;"))]),
        &["Lp/A;"],
    );
}

#[test]
fn extends_with_a_negated_inner_and_under_an_outer_not() {
    // Some direct supertype is not `A`.
    agree_on_java(
        methods(vec![extends(not(sig("Lp/A;")))]),
        &["Lp/A;", "Lp/C;"],
    );
    // Every direct supertype is `A`, vacuously so for a class with none.
    agree_on_java(
        methods(vec![not(extends(not(sig("Lp/A;"))))]),
        &["Lp/B;", "Lp/D;", "Lp/E;"],
    );
    // No supertype at all.
    agree_on_java(
        methods(vec![not(extends(all_of(vec![])))]),
        &["Lp/B;", "Lp/E;"],
    );
    agree_on_java(
        methods(vec![not(extends(any_of(vec![
            sig("Lp/I;"),
            name("Object"),
        ])))]),
        &["Lp/B;", "Lp/D;", "Lp/E;"],
    );
}

/// `parent` and `extends` on a `find: callsites` callee go through the same translation.
///
/// Java only: the JSON attaches a call-site endpoint to every matching callee whether or not
/// anything calls it, and the DSL only where a call site exists, so on a program with no calls
/// the two differ for a reason that is not about classes.
#[test]
fn class_predicates_on_call_sites() {
    for (where_, expected) in [
        (
            vec![parent(not(name("Lp/[AB];")))],
            vec!["Lp/C;", "Lp/D;", "Lp/E;"],
        ),
        (
            vec![not(extends(sig("Lp/A;")))],
            vec!["Lp/A;", "Lp/B;", "Lp/E;"],
        ),
        (
            vec![extends(all_of(vec![sig("Lp/A;"), sig("Lp/I;")]))],
            vec![],
        ),
    ] {
        let generator = json!({
            "find": "callsites",
            "where": where_,
            "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}
        });
        for scope in [
            ImportScope::new(ArtifactLanguage::Dex, "app"),
            ImportScope::unknown(),
        ] {
            let (json, dsl, text) = both(&generator, &java_program_with_sites(), &scope);
            assert_eq!(json, dsl, "{generator}\nmigrated:\n{text}");
        }
        let (json, _, _) = both(
            &generator,
            &java_program_with_sites(),
            &ImportScope::unknown(),
        );
        let expected: Selected = expected.iter().map(|c| java_method(c)).collect();
        assert_eq!(json, expected, "the JSON selection for {generator}");
    }
}

/// A dispatch generator's `parent` is the call site's class, on any frontend; its `extends`
/// reads the Java or CIL hierarchy.
#[test]
fn class_predicates_on_dispatch_keys() {
    for (where_, expected) in [
        (vec![parent(name("^Lp/[CI];$"))], vec!["Lp/C;", "Lp/I;"]),
        (
            vec![not(parent(name("^Lp/[CI];$")))],
            vec!["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"],
        ),
        (
            vec![parent(not(sig("Lp/A;")))],
            vec!["Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;", "Lp/I;"],
        ),
        (
            vec![extends(all_of(vec![sig("Lp/A;"), sig("Lp/I;")]))],
            vec![],
        ),
        (vec![extends(not(sig("Lp/A;")))], vec!["Lp/A;", "Lp/C;"]),
        (
            vec![not(extends(any_of(vec![sig("Lp/A;"), sig("Lp/I;")])))],
            vec!["Lp/A;", "Lp/B;", "Lp/E;", "Lp/I;"],
        ),
    ] {
        let generator = with_model("dispatch", Value::Array(where_));
        agree_on_java(generator, &expected);
    }
}
