//! The JSON `parent` / `extends` constraints, and their migration to the DSL.
//!
//! Each case runs one generator through the migrator and the DSL engine against each program,
//! and requires it to select exactly the functions (or, for `find: dispatch`, the signature
//! keys) the JSON matcher selected, which each case writes out. The programs give each
//! frontend the shape that tells a faithful translation from a near miss: a Java and a CIL class
//! with two direct supertypes, which is what separates "one supertype satisfies the whole
//! predicate" from "each part is satisfied by some supertype"; and a Lua class, which the DSL's
//! `parent` sees and the JSON's did not. On Lua the migration takes the DSL's meaning, so the two
//! are compared everywhere else and the Lua results are pinned on their own.
//!
//! The expectations are the answers of the JSON matcher that used to run these generators,
//! recorded from it while it existed.

use ctadl_ascent::models::portcullis::migrate::migrate_generators;
use ctadl_ascent::models::test_vmt;
use ctadl_ascent::models::{
    DispatchKeys, ImportScope, PortcullisFile, PortcullisMatcher, PortcullisModelSet,
    PortcullisPhase, ProgramMatchIndex, ProgramModelMatches,
};
use ctadl_ascent::project::ArtifactLanguage;
use ctadl_ir::mir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, Dispatch, JavaClass, JavaMethod,
    JavaSignature, JavaSimpleName, VirtualMethodTable,
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
        test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Java {
            methods,
            hierarchy,
            interfaces: vec![JavaClass("Lp/I;".into())],
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        })),
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
        test_vmt::with_method_names(VirtualMethodTable::new(Dispatch::Cil {
            methods,
            hierarchy,
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            pinvokes: Vec::new(),
        })),
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
        test_vmt::lua(
            vec![(account.into(), "deposit".into(), deposit.into())],
            &[
                (Symbol::from("deposit"), Symbol::from(deposit)),
                (Symbol::from("helper"), Symbol::from(helper)),
            ],
            &[],
            [(
                Symbol::from(account),
                smallvec::smallvec![Symbol::from(base)],
            )]
            .into_iter()
            .collect(),
        ),
        &[deposit.to_string(), helper.to_string()],
        Vec::new(),
    )
}

fn native_program() -> ProgramInfo {
    let names = ["system", "Lp/C;"];
    let signatures = names.map(|n| format!("{n}()"));
    program(
        test_vmt::native(
            names
                .iter()
                .zip(&signatures)
                .map(|(n, sig)| (*n, sig.as_str(), *n, *n)),
        ),
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

/// Loads `generator` through the migrated DSL, and returns what it selected and the migrated
/// text.
fn run_portcullis(
    generator: &Value,
    info: &ProgramInfo,
    scope: &ImportScope,
) -> (Selected, String) {
    let keys = DispatchKeys::from_program(&info.program);
    let index = ProgramMatchIndex::new_with_dispatch(info, scope.clone(), Some(&keys));
    let (text, report) = migrate_generators(std::iter::once(generator), None, None)
        .unwrap_or_else(|e| panic!("migrating {generator}: {e:?}"));
    assert!(
        report.warnings.is_empty(),
        "migrating {generator} warned: {:?}\n{text}",
        report.warnings
    );
    let file = PortcullisFile::from_text("migrated.ctadl", text.clone())
        .unwrap_or_else(|e| panic!("migrated {generator} does not load: {e}\n{text}"));
    let set = PortcullisModelSet { files: vec![file] };
    let mut matcher = PortcullisMatcher::new(&set, PortcullisPhase::All);
    let mut from_portcullis = ProgramModelMatches::default();
    matcher.observe_import(&index, &mut from_portcullis);
    matcher
        .finish(&mut from_portcullis)
        .unwrap_or_else(|e| panic!("DSL run of {generator}: {e}"));
    (selected(&from_portcullis), text)
}

/// The Java program `generator` is checked against: [`java_program_with_sites`] for a generator
/// that ranges over call sites or signature keys, which only the caller there has.
fn java_for(generator: &Value) -> fn() -> ProgramInfo {
    match generator["find"].as_str() {
        Some("methods") => java_program,
        _ => java_program_with_sites,
    }
}

/// What one generator selected, by the JSON matcher, on each program but Lua's (where the
/// JSON's class predicates matched nothing; see [`parent_and_extends_see_lua_classes`]).
///
/// Java and CIL are written as the Java class whose `m` was selected (`Lp/A;` for
/// `Lp/A;->m(I)V`, or for `[App]P.A::m(…)`); for a dispatch generator, the key on that class.
/// Native names functions. The CIL classes are spelled `[App]P.A`, so a predicate naming
/// `Lp/A;` does not hold of one.
struct Selects {
    java: &'static [&'static str],
    cil: &'static [&'static str],
    native: &'static [&'static str],
}

/// Nothing selected anywhere.
const NONE: Selects = Selects {
    java: &[],
    cil: &[],
    native: &[],
};

/// Every function of [`native_program`], which has no class at all.
const ALL_NATIVE: &[&str] = &["Lp/C;", "system"];

/// Every class with a method.
const ALL_CLASSES: &[&str] = &["Lp/A;", "Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;"];

/// Requires the migrated DSL to select what the JSON matcher selected for `generator`, on each
/// program but Lua's and under both scopes: the JSON keyed its class constraints off the method
/// table, not the declared language, so the scope must not matter.
fn selects(generator: Value, expected: Selects) {
    for (name, info, scope) in programs(java_for(&generator)) {
        let want: Selected = match name.split(' ').next() {
            Some("java") => expected.java.iter().map(|c| java_method(c)).collect(),
            Some("cil") => expected
                .cil
                .iter()
                .map(|c| cil_method(&cil_class(c)))
                .collect(),
            Some("native") => expected.native.iter().map(|f| f.to_string()).collect(),
            _ => continue,
        };
        let (portcullis, text) = run_portcullis(&generator, &info, &scope);
        assert_eq!(
            portcullis, want,
            "the migrated DSL on {name} for {generator} does not select what the JSON \
             selected\nmigrated:\n{text}"
        );
    }
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
    selects(
        methods(vec![parent(sig("Lp/A;"))]),
        Selects {
            java: &["Lp/A;"],
            ..NONE
        },
    );
    // `names` and `name` together are one membership test.
    selects(
        methods(vec![parent(json!({"constraint": "signature_match",
                                    "name": "Lp/A;", "names": ["Lp/B;"]}))]),
        Selects {
            java: &["Lp/A;", "Lp/B;"],
            ..NONE
        },
    );
    // Unanchored, as the JSON's `is_match` is.
    selects(
        methods(vec![parent(name("C"))]),
        Selects {
            java: &["Lp/C;"],
            cil: &["Lp/C;"],
            ..NONE
        },
    );
    // No name at all is a predicate no class satisfies.
}

/// The JSON's `parent` and `extends` matched nothing on Lua; a migrated rule takes the DSL's
/// meaning instead, where a metatable-recovered class is a class like any other.
#[test]
fn parent_and_extends_see_lua_classes() {
    selects(
        methods(vec![parent(name(".*"))]),
        Selects {
            java: ALL_CLASSES,
            cil: ALL_CLASSES,
            ..NONE
        },
    );
    let lua = lua_program();
    let cases = [
        (
            methods(vec![parent(sig("lua$class$Account"))]),
            vec!["m.Account.deposit"],
        ),
        (
            methods(vec![not(parent(sig("lua$class$Account")))]),
            vec!["m.helper"],
        ),
        (
            methods(vec![extends(sig("lua$class$Base"))]),
            vec!["m.Account.deposit"],
        ),
        (
            methods(vec![not(extends(sig("lua$class$Base")))]),
            vec!["m.helper"],
        ),
    ];
    for (generator, expected) in cases {
        for scope in [
            ImportScope::new(ArtifactLanguage::Lua, "app"),
            ImportScope::unknown(),
        ] {
            let (portcullis, text) = run_portcullis(&generator, &lua, &scope);
            let expected: Selected = expected.iter().map(|f| f.to_string()).collect();
            assert_eq!(
                portcullis, expected,
                "migrated DSL on Lua for {generator}\nmigrated:\n{text}"
            );
            // What the JSON did instead, so the difference stays a deliberate one.
            let negated = generator.to_string().contains("\"not\"");
            let json_expected: Selected = if negated {
                ["m.Account.deposit", "m.helper"]
                    .map(String::from)
                    .into_iter()
                    .collect()
            } else {
                Selected::new()
            };
            assert_ne!(portcullis, json_expected, "{generator}");
        }
    }
}

#[test]
fn parent_with_a_negated_inner() {
    selects(
        methods(vec![parent(not(sig("Lp/C;")))]),
        Selects {
            java: &["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"],
            cil: ALL_CLASSES,
            ..NONE
        },
    );
    selects(
        methods(vec![parent(not(name("^Lp/[AB];$")))]),
        Selects {
            java: &["Lp/C;", "Lp/D;", "Lp/E;"],
            cil: ALL_CLASSES,
            ..NONE
        },
    );
    selects(
        methods(vec![parent(not(not(sig("Lp/C;"))))]),
        Selects {
            java: &["Lp/C;"],
            ..NONE
        },
    );
}

#[test]
fn parent_with_nested_combinators() {
    // A class under `Lp/` that is neither `A` nor anything matching `D`.
    selects(
        methods(vec![parent(all_of(vec![
            name("^Lp/"),
            not(any_of(vec![sig("Lp/A;"), name("D")])),
        ]))]),
        Selects {
            java: &["Lp/B;", "Lp/C;", "Lp/E;"],
            ..NONE
        },
    );
    selects(
        methods(vec![parent(any_of(vec![
            all_of(vec![name("^Lp/"), not(name("[A-C]"))]),
            sig("Lp/A;"),
        ]))]),
        Selects {
            java: &["Lp/A;", "Lp/D;", "Lp/E;"],
            ..NONE
        },
    );
    // The empty combinators: `all_of` holds of every class, `any_of` of none.
    selects(
        methods(vec![parent(all_of(vec![]))]),
        Selects {
            java: ALL_CLASSES,
            cil: ALL_CLASSES,
            ..NONE
        },
    );
    selects(methods(vec![parent(any_of(vec![]))]), NONE);
    // Under a top-level `not`, a function with no class at all is selected too.
    selects(
        methods(vec![not(parent(any_of(vec![])))]),
        Selects {
            java: ALL_CLASSES,
            cil: ALL_CLASSES,
            native: ALL_NATIVE,
        },
    );
    selects(
        methods(vec![not(parent(all_of(vec![])))]),
        Selects {
            native: ALL_NATIVE,
            ..NONE
        },
    );
}

#[test]
fn parent_under_an_outer_not() {
    selects(
        methods(vec![not(parent(sig("Lp/C;")))]),
        Selects {
            java: &["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"],
            cil: ALL_CLASSES,
            native: ALL_NATIVE,
        },
    );
    selects(
        methods(vec![not(parent(any_of(vec![sig("Lp/A;"), name("D")])))]),
        Selects {
            java: &["Lp/B;", "Lp/C;", "Lp/E;"],
            cil: &["Lp/A;", "Lp/B;", "Lp/C;", "Lp/E;"],
            native: ALL_NATIVE,
        },
    );
    selects(
        methods(vec![not(parent(all_of(vec![
            name("^Lp/"),
            not(sig("Lp/A;")),
        ])))]),
        Selects {
            java: &["Lp/A;"],
            cil: ALL_CLASSES,
            native: ALL_NATIVE,
        },
    );
    // Alongside other constraints, and inside a `where`-level combinator.
    selects(
        methods(vec![
            json!({"constraint": "signature_match", "name": "m"}),
            not(any_of(vec![parent(sig("Lp/A;")), extends(sig("Lp/A;"))])),
        ]),
        Selects {
            java: &["Lp/B;", "Lp/E;"],
            cil: ALL_CLASSES,
            ..NONE
        },
    );
}

/// The `extends` divergence: one direct supertype must satisfy the *whole* predicate. `C`'s
/// two supertypes are `A` and `I`, so no single one is both.
#[test]
fn extends_tests_one_supertype_against_the_whole_predicate() {
    selects(
        methods(vec![extends(all_of(vec![sig("Lp/A;"), sig("Lp/I;")]))]),
        NONE,
    );
    selects(
        methods(vec![extends(any_of(vec![sig("Lp/A;"), sig("Lp/I;")]))]),
        Selects {
            java: &["Lp/C;", "Lp/D;"],
            ..NONE
        },
    );
    selects(
        methods(vec![extends(all_of(vec![name("^Lp/"), not(sig("Lp/A;"))]))]),
        Selects {
            java: &["Lp/C;"],
            ..NONE
        },
    );
    // Direct supertypes only: `C` reaches `Object` through `A`, but does not extend it.
    selects(
        methods(vec![extends(sig("Ljava/lang/Object;"))]),
        Selects {
            java: &["Lp/A;"],
            ..NONE
        },
    );
}

#[test]
fn extends_with_a_negated_inner_and_under_an_outer_not() {
    // Some direct supertype is not `A`.
    selects(
        methods(vec![extends(not(sig("Lp/A;")))]),
        Selects {
            java: &["Lp/A;", "Lp/C;"],
            cil: &["Lp/A;", "Lp/C;", "Lp/D;"],
            ..NONE
        },
    );
    // Every direct supertype is `A`, vacuously so for a class with none.
    selects(
        methods(vec![not(extends(not(sig("Lp/A;"))))]),
        Selects {
            java: &["Lp/B;", "Lp/D;", "Lp/E;"],
            cil: &["Lp/B;", "Lp/E;"],
            native: ALL_NATIVE,
        },
    );
    // No supertype at all.
    selects(
        methods(vec![not(extends(all_of(vec![])))]),
        Selects {
            java: &["Lp/B;", "Lp/E;"],
            cil: &["Lp/B;", "Lp/E;"],
            native: ALL_NATIVE,
        },
    );
    selects(
        methods(vec![not(extends(any_of(vec![
            sig("Lp/I;"),
            name("Object"),
        ])))]),
        Selects {
            java: &["Lp/B;", "Lp/D;", "Lp/E;"],
            cil: &["Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;"],
            native: ALL_NATIVE,
        },
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
        let expected: Selected = expected.iter().map(|c| java_method(c)).collect();
        for scope in [
            ImportScope::new(ArtifactLanguage::Dex, "app"),
            ImportScope::unknown(),
        ] {
            let program = java_program_with_sites();
            let (portcullis, text) = run_portcullis(&generator, &program, &scope);
            assert_eq!(portcullis, expected, "{generator}\nmigrated:\n{text}");
        }
    }
}

/// A dispatch generator's `parent` is the call site's class, on any frontend; its `extends`
/// reads the Java or CIL hierarchy. Only the Java program has call sites, so only it has keys.
#[test]
fn class_predicates_on_dispatch_keys() {
    let java = |java| Selects { java, ..NONE };
    for (where_, expected) in [
        (vec![parent(name("^Lp/[CI];$"))], java(&["Lp/C;", "Lp/I;"])),
        (
            vec![not(parent(name("^Lp/[CI];$")))],
            java(&["Lp/A;", "Lp/B;", "Lp/D;", "Lp/E;"]),
        ),
        (
            vec![parent(not(sig("Lp/A;")))],
            java(&["Lp/B;", "Lp/C;", "Lp/D;", "Lp/E;", "Lp/I;"]),
        ),
        (
            vec![extends(all_of(vec![sig("Lp/A;"), sig("Lp/I;")]))],
            NONE,
        ),
        (vec![extends(not(sig("Lp/A;")))], java(&["Lp/A;", "Lp/C;"])),
        (
            vec![not(extends(any_of(vec![sig("Lp/A;"), sig("Lp/I;")])))],
            java(&["Lp/A;", "Lp/B;", "Lp/E;", "Lp/I;"]),
        ),
    ] {
        selects(with_model("dispatch", Value::Array(where_)), expected);
    }
}
