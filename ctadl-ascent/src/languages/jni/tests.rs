use super::*;

use ctadl_ir::mir::call::{
    JavaClass, JavaMethod, JavaSignature, JavaSimpleName, NativeFunction, NativeQualifiedName,
    NativeSignature, NativeSimpleName,
};

use crate::facts::{Function, InsnSiteId};

// ---------------------------------------------------------------------------
// Mangling
// ---------------------------------------------------------------------------

#[test]
fn mangles_the_four_escapes() {
    assert_eq!(mangle_component("com/example/Crypto"), "com_example_Crypto");
    assert_eq!(mangle_component("my_method"), "my_1method");
    assert_eq!(
        mangle_component("Ljava/lang/String;"),
        "Ljava_lang_String_2"
    );
    assert_eq!(mangle_component("[I"), "_3I");
    assert_eq!(
        mangle_component("[[Ljava/lang/String;"),
        "_3_3Ljava_lang_String_2"
    );
}

#[test]
fn mangles_non_ascii_as_utf16_units() {
    // BMP: one `_0XXXX` escape, lowercase hex.
    assert_eq!(mangle_component("caf\u{e9}"), "caf_000e9");
    // Outside the BMP: one escape per surrogate.
    assert_eq!(mangle_component("\u{1f600}"), "_0d83d_0de00");
    // The dollar sign an inner class name carries is not alphanumeric.
    assert_eq!(mangle_component("Outer$Inner"), "Outer_00024Inner");
}

#[test]
fn builds_the_spec_example_names() {
    let cls = internal_class_name("Lcom/example/Crypto;");
    assert_eq!(cls, "com/example/Crypto");
    let descriptor = "(Ljava/lang/String;)Ljava/lang/String;";
    assert_eq!(
        short_name(cls, "encrypt"),
        "Java_com_example_Crypto_encrypt"
    );
    assert_eq!(
        long_name(cls, "encrypt", &param_descriptor(descriptor).unwrap()),
        "Java_com_example_Crypto_encrypt__Ljava_lang_String_2"
    );
}

#[test]
fn long_name_distinguishes_an_overload() {
    let cls = "Foo";
    let a = param_descriptor("(Ljava/lang/String;)V").unwrap();
    let b = param_descriptor("(I)V").unwrap();
    assert_eq!(short_name(cls, "f"), "Java_Foo_f");
    assert_ne!(long_name(cls, "f", &a), long_name(cls, "f", &b));
    assert_eq!(long_name(cls, "f", &b), "Java_Foo_f__I");
}

#[test]
fn internal_class_name_passes_through_a_bare_name() {
    assert_eq!(internal_class_name("JniFlow"), "JniFlow");
    assert_eq!(internal_class_name("LJniFlow;"), "JniFlow");
}

// ---------------------------------------------------------------------------
// Descriptor parsing
// ---------------------------------------------------------------------------

#[test]
fn splits_descriptor_parameters() {
    assert_eq!(descriptor_params("()V").unwrap(), Vec::<&str>::new());
    assert_eq!(
        descriptor_params("(Ljava/lang/String;[IJ)Ljava/lang/Object;").unwrap(),
        vec!["Ljava/lang/String;", "[I", "J"]
    );
    assert_eq!(
        descriptor_params("([[Ljava/lang/String;ZD)V").unwrap(),
        vec!["[[Ljava/lang/String;", "Z", "D"]
    );
    assert_eq!(
        param_descriptor("(Ljava/lang/String;[IJ)V").unwrap(),
        "Ljava/lang/String;[IJ"
    );
}

#[test]
fn rejects_a_malformed_descriptor() {
    assert!(descriptor_params("Ljava/lang/String;").is_none()); // no leading paren
    assert!(descriptor_params("(Ljava/lang/String").is_none()); // unterminated class name
    assert!(descriptor_params("(Q)V").is_none()); // not a type code
    assert!(descriptor_params("(I").is_none()); // unterminated parameter list
}

// ---------------------------------------------------------------------------
// Port map
// ---------------------------------------------------------------------------

/// `(java, native)` pairs as plain `i16`s, for readable assertions.
fn ports(descriptor: &str, is_static: bool, slots: SlotModel) -> Vec<(i16, i16)> {
    port_map(descriptor, is_static, slots)
        .unwrap()
        .into_iter()
        .map(|(j, n)| (*j, *n))
        .collect()
}

#[test]
fn static_method_shifts_arguments_by_two() {
    assert_eq!(
        ports("(Ljava/lang/String;)V", true, SlotModel::Argument),
        vec![
            (0, 2),
            (RETURN_INDEX, RETURN_INDEX),
            (GLOBALS_INDEX, GLOBALS_INDEX)
        ]
    );
}

#[test]
fn instance_method_maps_this_onto_the_jobject() {
    assert_eq!(
        ports(
            "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            false,
            SlotModel::Argument
        ),
        vec![
            (0, 1),
            (1, 2),
            (2, 3),
            (RETURN_INDEX, RETURN_INDEX),
            (GLOBALS_INDEX, GLOBALS_INDEX)
        ]
    );
}

#[test]
fn register_slots_widen_for_long_and_double() {
    // Dex numbers by register, so `J` at slot 0 pushes the next parameter to slot 2 -- while the
    // native side still advances by exactly one.
    assert_eq!(
        ports("(JLjava/lang/String;D)V", true, SlotModel::Register),
        vec![
            (0, 2),
            (2, 3),
            (3, 4),
            (RETURN_INDEX, RETURN_INDEX),
            (GLOBALS_INDEX, GLOBALS_INDEX)
        ]
    );
    // JVM numbers by argument, so the same descriptor is dense.
    assert_eq!(
        ports("(JLjava/lang/String;D)V", true, SlotModel::Argument),
        vec![
            (0, 2),
            (1, 3),
            (2, 4),
            (RETURN_INDEX, RETURN_INDEX),
            (GLOBALS_INDEX, GLOBALS_INDEX)
        ]
    );
}

#[test]
fn an_array_of_wides_is_a_reference_and_takes_one_register() {
    assert_eq!(
        ports("([JI)V", true, SlotModel::Register),
        vec![
            (0, 2),
            (1, 3),
            (RETURN_INDEX, RETURN_INDEX),
            (GLOBALS_INDEX, GLOBALS_INDEX)
        ]
    );
}

#[test]
fn return_and_globals_appear_exactly_once() {
    for is_static in [true, false] {
        for slots in [SlotModel::Register, SlotModel::Argument] {
            let p = ports("(JJ)J", is_static, slots);
            assert_eq!(
                p.iter()
                    .filter(|(j, n)| *j == RETURN_INDEX && *n == RETURN_INDEX)
                    .count(),
                1
            );
            assert_eq!(
                p.iter()
                    .filter(|(j, n)| *j == GLOBALS_INDEX && *n == GLOBALS_INDEX)
                    .count(),
                1
            );
            // Only `-1` is mapped: a Java function's `-2` (exception return) has no native
            // counterpart.
            assert!(!p.iter().any(|(j, _)| *j == -2));
        }
    }
}

#[test]
fn port_map_rejects_a_malformed_descriptor() {
    assert!(port_map("not-a-descriptor", true, SlotModel::Argument).is_none());
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `(class descriptor, simple name, method descriptor, is_static)` -> a Java program whose VMT
/// declares exactly those `native` methods. The IR function name is spelled the way both Java
/// frontends spell it, `<class>-><name><descriptor>`.
fn java_program(natives: &[(&str, &str, &str, bool)]) -> ProgramInfo {
    ProgramInfo {
        vmt: VirtualMethodTable::Java {
            methods: Vec::new(),
            hierarchy: Default::default(),
            natives: natives
                .iter()
                .map(|(cls, name, descriptor, is_static)| {
                    (
                        JavaClass((*cls).into()),
                        JavaSimpleName((*name).into()),
                        JavaSignature((*descriptor).into()),
                        JavaMethod(java_method_name(cls, name, descriptor).as_str().into()),
                        *is_static,
                    )
                })
                .collect(),
        },
        ..Default::default()
    }
}

fn java_method_name(cls: &str, name: &str, descriptor: &str) -> String {
    format!("{cls}->{name}{descriptor}")
}

/// `(simple name, fully-qualified IR name)` -> a native program whose VMT carries those symbols.
fn native_program(symbols: &[(&str, &str)]) -> ProgramInfo {
    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: symbols
                .iter()
                .map(|(simple, func)| {
                    (
                        NativeSimpleName((*simple).into()),
                        NativeSignature("undefined()".into()),
                        NativeFunction((*func).into()),
                        NativeQualifiedName((*simple).into()),
                    )
                })
                .collect(),
        },
        ..Default::default()
    }
}

/// Interns `functions` in the order given and returns a fact base in which each has `arity`
/// declared formals, mimicking what codegen leaves behind.
fn fact_base(functions: &[(&str, i16)]) -> (IndexFacts, IndexSourceInfo) {
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    for (name, arity) in functions {
        let id = source_info
            .sites
            .get_or_add_function(Function((*name).into()));
        for i in 0..*arity {
            facts.formal_param.push((
                id,
                FlowVariable::formal_index(FormalIndex::new(i)),
                FormalType::ByRef,
            ));
        }
    }
    (facts, source_info)
}

fn function_id(source_info: &IndexSourceInfo, name: &str) -> FunctionId {
    source_info
        .sites
        .get_function_id(Function(name.into()))
        .unwrap_or_else(|| panic!("{name} was not interned"))
}

/// The `(formal index, java-side variable)` pairs the bridge emitted at `site`.
fn actuals(facts: &IndexFacts, site: PackedInsnSiteId) -> Vec<(i16, i16)> {
    let mut rows: Vec<(i16, i16)> = facts
        .actual_param
        .iter()
        .filter(|(s, _, _)| *s == site)
        .map(|(_, index, FlowVertex(var, path))| {
            assert!(path.is_empty(), "bridge vertices carry no path");
            (**index, *var.as_formal().expect("a formal-index vertex"))
        })
        .collect();
    rows.sort();
    rows
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

#[test]
fn links_an_instance_native_to_its_implementation() {
    let java = "Lcom/example/Crypto;";
    let descriptor = "(Ljava/lang/String;)Ljava/lang/String;";
    let stub = java_method_name(java, "encrypt", descriptor);

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "encrypt", descriptor, false)]),
        SlotModel::Register,
    );
    obs.observe(
        &native_program(&[(
            "Java_com_example_Crypto_encrypt",
            "Java_com_example_Crypto_encrypt",
        )]),
        SlotModel::Argument,
    );

    let (mut facts, mut source_info) =
        fact_base(&[(&stub, 0), ("Java_com_example_Crypto_encrypt", 3)]);
    // A site that already exists in the stub, so "the bridge mints a fresh one" is testable.
    let existing = source_info.add_insn_site(function_id(&source_info, &stub));

    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 1,
            registered: 0,
            unresolved: 0,
            ambiguous: 0,
            unattributed: 0
        }
    );

    let java_id = function_id(&source_info, &stub);
    let native_id = function_id(&source_info, "Java_com_example_Crypto_encrypt");
    assert_eq!(facts.call.len(), 1);
    let (site, target) = facts.call[0];
    assert_eq!(target, native_id);

    let unpacked = InsnSiteId::try_from(site).unwrap();
    assert_eq!(unpacked.func_id, java_id, "the site lives in the Java stub");
    assert_ne!(unpacked.insn_id, existing.insn_id, "the site is fresh");

    // `this` -> jobject, argument 0 -> native 2, return -> return, globals -> globals.
    assert_eq!(
        actuals(&facts, site),
        vec![
            (GLOBALS_INDEX, GLOBALS_INDEX),
            (RETURN_INDEX, RETURN_INDEX),
            (1, 0),
            (2, 1),
        ]
    );

    // The bodyless stub gets the formals the summary rule joins on.
    let mut declared: Vec<i16> = facts
        .formal_param
        .iter()
        .filter(|(f, _, _)| *f == java_id)
        .map(|(_, var, _)| *var.as_formal().unwrap())
        .collect();
    declared.sort();
    declared.dedup();
    assert_eq!(declared, vec![GLOBALS_INDEX, RETURN_INDEX, 0, 1]);
}

#[test]
fn links_a_static_native_without_a_this_port() {
    let java = "LJniFlow;";
    let descriptor = "(Ljava/lang/String;)V";
    let stub = java_method_name(java, "nativeStash", descriptor);

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "nativeStash", descriptor, true)]),
        SlotModel::Register,
    );
    obs.observe(
        &native_program(&[("Java_JniFlow_nativeStash", "Java_JniFlow_nativeStash")]),
        SlotModel::Argument,
    );

    let (mut facts, mut source_info) = fact_base(&[(&stub, 0), ("Java_JniFlow_nativeStash", 3)]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(stats.linked, 1);

    let site = facts.call[0].0;
    assert_eq!(
        actuals(&facts, site),
        vec![
            (GLOBALS_INDEX, GLOBALS_INDEX),
            (RETURN_INDEX, RETURN_INDEX),
            // Java argument 0 lands on native index 2: past `JNIEnv *` AND past `jclass`.
            (2, 0),
        ]
    );
    assert!(
        !actuals(&facts, site).iter().any(|(n, _)| *n == 1),
        "a static native has no receiver, so nothing maps onto the jclass"
    );
}

#[test]
fn two_natives_get_two_distinct_sites() {
    let java = "LJniFlow;";
    let stash = "(Ljava/lang/String;)V";
    let fetch = "()Ljava/lang/String;";

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[
            (java, "nativeStash", stash, true),
            (java, "nativeFetch", fetch, true),
        ]),
        SlotModel::Register,
    );
    obs.observe(
        &native_program(&[
            ("Java_JniFlow_nativeStash", "Java_JniFlow_nativeStash"),
            ("Java_JniFlow_nativeFetch", "Java_JniFlow_nativeFetch"),
        ]),
        SlotModel::Argument,
    );

    let (mut facts, mut source_info) = fact_base(&[
        (&java_method_name(java, "nativeStash", stash), 0),
        (&java_method_name(java, "nativeFetch", fetch), 0),
        ("Java_JniFlow_nativeStash", 3),
        ("Java_JniFlow_nativeFetch", 2),
    ]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(stats.linked, 2);

    let sites: Vec<_> = facts.call.iter().map(|(s, _)| *s).collect();
    assert_eq!(sites.len(), 2);
    assert_ne!(sites[0], sites[1], "each bridge mints its own site");
}

/// The same method observed from two imports (an app and the library jar declaring it) is one
/// `FunctionId`, so it gets one bridge, not two.
#[test]
fn a_method_observed_twice_is_bridged_once() {
    let java = "LJniFlow;";
    let descriptor = "()V";
    let stub = java_method_name(java, "go", descriptor);
    let program = java_program(&[(java, "go", descriptor, true)]);

    let mut obs = JniObserver::new();
    obs.observe(&program, SlotModel::Register);
    obs.observe(&program, SlotModel::Argument);
    obs.observe(
        &native_program(&[("Java_JniFlow_go", "Java_JniFlow_go")]),
        SlotModel::Argument,
    );

    let (mut facts, mut source_info) = fact_base(&[(&stub, 0), ("Java_JniFlow_go", 2)]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 1,
            registered: 0,
            unresolved: 0,
            ambiguous: 0,
            unattributed: 0
        }
    );
    assert_eq!(facts.call.len(), 1);
}

#[test]
fn emits_nothing_when_there_is_no_native_import() {
    let java = "LJniFlow;";
    let descriptor = "()V";
    let stub = java_method_name(java, "go", descriptor);

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "go", descriptor, true)]),
        SlotModel::Register,
    );
    assert!(obs.is_empty());

    let (mut facts, mut source_info) = fact_base(&[(&stub, 0)]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 0,
            registered: 0,
            unresolved: 1,
            ambiguous: 0,
            unattributed: 0
        }
    );
    assert!(facts.call.is_empty());
    assert!(facts.actual_param.is_empty());
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Runs `link` over one Java native and the given native symbol table, returning the stats plus
/// the IR name of whatever it linked to.
fn resolve_against(
    natives: &[(&str, &str, &str, bool)],
    symbols: &[(&str, &str)],
) -> (LinkStats, Option<String>) {
    let mut obs = JniObserver::new();
    obs.observe(&java_program(natives), SlotModel::Register);
    obs.observe(&native_program(symbols), SlotModel::Argument);

    let mut functions: Vec<(String, i16)> = natives
        .iter()
        .map(|(cls, name, descriptor, _)| (java_method_name(cls, name, descriptor), 0))
        .collect();
    functions.extend(symbols.iter().map(|(_, func)| ((*func).to_string(), 8)));
    let borrowed: Vec<(&str, i16)> = functions.iter().map(|(n, a)| (n.as_str(), *a)).collect();

    let (mut facts, mut source_info) = fact_base(&borrowed);
    let stats = link(&obs, &mut facts, &mut source_info);
    let linked = facts.call.first().map(|(_, target)| {
        source_info
            .sites
            .get_function(*target)
            .expect("target is interned")
            .to_string()
    });
    (stats, linked)
}

#[test]
fn prefers_the_long_name_when_both_symbols_exist() {
    let (stats, linked) = resolve_against(
        &[("LFoo;", "f", "(I)V", true)],
        &[
            ("Java_Foo_f", "Java_Foo_f"),
            ("Java_Foo_f__I", "Java_Foo_f__I"),
        ],
    );
    assert_eq!(stats.linked, 1);
    assert_eq!(linked.as_deref(), Some("function(Java_Foo_f__I)"));
}

#[test]
fn falls_back_to_the_short_name_when_unambiguous() {
    let (stats, linked) = resolve_against(
        &[("LFoo;", "f", "(I)V", true)],
        &[("Java_Foo_f", "Java_Foo_f")],
    );
    assert_eq!(stats.linked, 1);
    assert_eq!(linked.as_deref(), Some("function(Java_Foo_f)"));
}

/// An overloaded native whose only symbol is the short form cannot be attributed to either
/// overload. Warn and skip rather than guess.
#[test]
fn skips_an_overloaded_native_with_only_a_short_symbol() {
    let (stats, linked) = resolve_against(
        &[("LFoo;", "f", "(I)V", true), ("LFoo;", "f", "(J)V", true)],
        &[("Java_Foo_f", "Java_Foo_f")],
    );
    assert_eq!(
        stats,
        LinkStats {
            natives: 2,
            linked: 0,
            registered: 0,
            unresolved: 0,
            ambiguous: 2,
            unattributed: 0
        }
    );
    assert_eq!(linked, None);
}

/// ... but each overload still links when the implementations use their long names.
#[test]
fn links_each_overload_through_its_long_name() {
    let (stats, _) = resolve_against(
        &[("LFoo;", "f", "(I)V", true), ("LFoo;", "f", "(J)V", true)],
        &[
            ("Java_Foo_f__I", "Java_Foo_f__I"),
            ("Java_Foo_f__J", "Java_Foo_f__J"),
        ],
    );
    assert_eq!(stats.linked, 2);
    assert_eq!(stats.ambiguous, 0);
}

/// Mach-O prefixes every C symbol with `_`, which the pcode frontend strips when it computes the
/// VMT's simple name. Matching against that simple name is what makes the bridge work there, and
/// the *decorated* IR name is what the call edge must target.
#[test]
fn matches_a_macho_underscore_prefixed_symbol() {
    let (stats, linked) = resolve_against(
        &[("LFoo;", "f", "(I)V", true)],
        &[("Java_Foo_f", "_Java_Foo_f")],
    );
    assert_eq!(stats.linked, 1);
    assert_eq!(linked.as_deref(), Some("function(_Java_Foo_f)"));
}

/// Ghidra uniques colliding names, so one simple name can front several IR functions. There is no
/// basis for picking one, so skip.
#[test]
fn skips_a_symbol_carried_by_several_native_functions() {
    let (stats, _) = resolve_against(
        &[("LFoo;", "f", "(I)V", true)],
        &[("Java_Foo_f", "Java_Foo_f"), ("Java_Foo_f", "Java_Foo_f_1")],
    );
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 0,
            registered: 0,
            unresolved: 0,
            ambiguous: 1,
            unattributed: 0
        }
    );
}

#[test]
fn reports_a_native_with_no_matching_symbol_as_unresolved() {
    let (stats, _) = resolve_against(
        &[("LFoo;", "f", "(I)V", true)],
        &[("Java_Bar_g", "Java_Bar_g")],
    );
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 0,
            registered: 0,
            unresolved: 1,
            ambiguous: 0,
            unattributed: 0
        }
    );
}

#[test]
fn slot_model_follows_the_frontend() {
    assert_eq!(
        SlotModel::for_language(ArtifactLanguage::Dex),
        SlotModel::Register
    );
    assert_eq!(
        SlotModel::for_language(ArtifactLanguage::Apk),
        SlotModel::Register
    );
    assert_eq!(
        SlotModel::for_language(ArtifactLanguage::Jar),
        SlotModel::Argument
    );
    assert_eq!(
        SlotModel::for_language(ArtifactLanguage::Jvm),
        SlotModel::Argument
    );
}

// ---------------------------------------------------------------------------
// Natives bound by `RegisterNatives`
// ---------------------------------------------------------------------------

/// A library's recovered tables, as `observe_registry` would have loaded them: one contiguous
/// ELF64 table of `(name, descriptor, function)` triples.
fn observe_table(obs: &mut JniObserver, import: &str, rows: &[(&str, &str, &str)]) {
    obs.registries.push((
        import.to_string(),
        registry::JniRegistry {
            entry_size: 24,
            entries: rows
                .iter()
                .enumerate()
                .map(
                    |(i, (name, descriptor, function))| registry::RegistryEntry {
                        table_addr: 0x2a000 + 24 * i as u64,
                        fn_addr: 0x1000 + 16 * i as u64,
                        name: (*name).to_string(),
                        descriptor: (*descriptor).to_string(),
                        function: Some((*function).to_string()),
                        veneer_target: None,
                    },
                )
                .collect(),
        },
    ));
}

/// The case the whole registry exists for: the implementation exports no `Java_…` symbol at all,
/// so the symbol convention finds nothing and the binding is only visible in the table.
#[test]
fn links_a_native_bound_by_register_natives() {
    let java = "Lcom/example/Superpack;";
    let descriptor = "(JII[BI)V";
    let stub = java_method_name(java, "readBytesNative", descriptor);

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "readBytesNative", descriptor, false)]),
        SlotModel::Register,
    );
    // The library exports one unrelated symbol; nothing here is a mangled JNI name.
    obs.observe(
        &native_program(&[("stash_impl", "stash_impl")]),
        SlotModel::Argument,
    );
    observe_table(
        &mut obs,
        "app__arm64-v8a__libsuperpack-jni",
        &[("readBytesNative", descriptor, "stash_impl")],
    );

    let (mut facts, mut source_info) = fact_base(&[(&stub, 0), ("stash_impl", 7)]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(
        stats,
        LinkStats {
            natives: 1,
            linked: 1,
            registered: 1,
            unresolved: 0,
            ambiguous: 0,
            unattributed: 0
        }
    );
    assert_eq!(facts.call.len(), 1);
    assert_eq!(facts.call[0].1, function_id(&source_info, "stash_impl"));
}

/// Where a method resolves both ways, the registration wins -- that is what the runtime does --
/// and exactly one bridge is emitted, so `emit_bridge` cannot mint two sites for one method.
#[test]
fn a_registration_wins_over_a_matching_symbol() {
    let java = "LJniFlow;";
    let descriptor = "()V";
    let stub = java_method_name(java, "nativeStash", descriptor);

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "nativeStash", descriptor, true)]),
        SlotModel::Register,
    );
    obs.observe(
        &native_program(&[("Java_JniFlow_nativeStash", "Java_JniFlow_nativeStash")]),
        SlotModel::Argument,
    );
    observe_table(
        &mut obs,
        "lib",
        &[("nativeStash", descriptor, "stash_impl")],
    );

    let (mut facts, mut source_info) = fact_base(&[
        (&stub, 0),
        ("Java_JniFlow_nativeStash", 2),
        ("stash_impl", 2),
    ]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(stats.linked, 1);
    assert_eq!(stats.registered, 1);
    assert_eq!(facts.call.len(), 1, "one bridge, not two");
    assert_eq!(facts.call[0].1, function_id(&source_info, "stash_impl"));
}

/// Tier 0 has to run *before* the symbol tiers, not as a fallback: the ambiguous arm `continue`s
/// without ever reaching one, so a fallback-only registry would never rescue an overloaded
/// native -- the case `RegisterNatives` matters most for.
#[test]
fn a_registration_rescues_an_overload_a_short_symbol_cannot_resolve() {
    let java = "LFoo;";
    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "f", "(I)V", true), (java, "f", "(J)V", true)]),
        SlotModel::Register,
    );
    // Only the short symbol exists, so both overloads are ambiguous by name alone.
    obs.observe(
        &native_program(&[("Java_Foo_f", "Java_Foo_f")]),
        SlotModel::Argument,
    );
    observe_table(
        &mut obs,
        "lib",
        &[("f", "(I)V", "f_int"), ("f", "(J)V", "f_long")],
    );

    let (mut facts, mut source_info) = fact_base(&[
        (&java_method_name(java, "f", "(I)V"), 0),
        (&java_method_name(java, "f", "(J)V"), 0),
        ("Java_Foo_f", 3),
        ("f_int", 3),
        ("f_long", 3),
    ]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(
        stats,
        LinkStats {
            natives: 2,
            linked: 2,
            registered: 2,
            unresolved: 0,
            ambiguous: 0,
            unattributed: 0
        }
    );
    let targets: HashSet<FunctionId> = facts.call.iter().map(|(_, target)| *target).collect();
    assert_eq!(targets.len(), 2, "each overload got its own implementation");
}

/// An entry tier 1 cannot attribute is counted, never guessed at, and emits no link.
#[test]
fn an_unattributed_entry_is_counted_and_not_linked() {
    let java = "Lcom/example/A;";
    let stub = java_method_name(java, "a", "()V");

    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[(java, "a", "()V", true)]),
        SlotModel::Register,
    );
    obs.observe(
        &native_program(&[("unrelated", "unrelated")]),
        SlotModel::Argument,
    );
    // The Java half of this library ships outside `classes.dex`, so nothing matches.
    observe_table(
        &mut obs,
        "lib",
        &[("getBdjoN", "(J)Lorg/videolan/bdjo/Bdjo;", "bdjo_impl")],
    );

    let (mut facts, mut source_info) = fact_base(&[(&stub, 0), ("bdjo_impl", 3)]);
    let stats = link(&obs, &mut facts, &mut source_info);
    assert_eq!(stats.linked, 0);
    assert_eq!(stats.registered, 0);
    assert_eq!(stats.unattributed, 1);
    assert_eq!(stats.unresolved, 1);
    assert!(facts.call.is_empty());
}

/// The registry half counts as a native half on its own: a library that exports not one `Java_…`
/// symbol and binds everything through `RegisterNatives` is the ordinary case.
#[test]
fn a_registry_alone_is_not_an_empty_observer() {
    let mut obs = JniObserver::new();
    obs.observe(
        &java_program(&[("LA;", "a", "()V", true)]),
        SlotModel::Register,
    );
    assert!(obs.is_empty(), "no native half at all");
    observe_table(&mut obs, "lib", &[("a", "()V", "a_impl")]);
    assert!(!obs.is_empty());
}
