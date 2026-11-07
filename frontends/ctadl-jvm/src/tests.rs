use super::import_class;
use ctadl_ir::cir::TerminatorKind;
use ctadl_ir::cir::call::Dispatch;

/// A class file for `Demo` with a `public static` method for each
/// `(name, descriptor, bytecode)`.
fn class_with_methods(methods: &[(&str, &str, &[u8])]) -> Vec<u8> {
    class_with_pool(&[], methods)
}

/// [`class_with_methods`], with `pool` appended to the constant pool. Its
/// entries are numbered from `6 + 2 * methods.len()`.
fn class_with_pool(pool: &[&[u8]], methods: &[(&str, &str, &[u8])]) -> Vec<u8> {
    fn utf8(out: &mut Vec<u8>, s: &str) {
        out.push(1);
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
    }
    let mut out = Vec::new();
    out.extend(0xCAFE_BABEu32.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend(49u16.to_be_bytes());
    // Entries 1..=5, then a name and a descriptor per method.
    out.extend((6 + 2 * methods.len() as u16 + pool.len() as u16).to_be_bytes());
    utf8(&mut out, "Demo"); // 1
    out.extend([7, 0, 1]); // 2: Class Demo
    utf8(&mut out, "java/lang/Object"); // 3
    out.extend([7, 0, 3]); // 4: Class java/lang/Object
    utf8(&mut out, "Code"); // 5
    for (name, descriptor, _) in methods {
        utf8(&mut out, name);
        utf8(&mut out, descriptor);
    }
    for entry in pool {
        out.extend(*entry);
    }
    out.extend(0x0021u16.to_be_bytes()); // public super
    out.extend(2u16.to_be_bytes());
    out.extend(4u16.to_be_bytes());
    out.extend(0u16.to_be_bytes()); // interfaces
    out.extend(0u16.to_be_bytes()); // fields
    out.extend((methods.len() as u16).to_be_bytes());
    for (i, (_, _, code)) in methods.iter().enumerate() {
        let name_index = 6 + 2 * i as u16;
        out.extend(0x0009u16.to_be_bytes()); // public static
        out.extend(name_index.to_be_bytes());
        out.extend((name_index + 1).to_be_bytes());
        out.extend(1u16.to_be_bytes()); // one attribute: Code
        out.extend(5u16.to_be_bytes());
        out.extend((12 + code.len() as u32).to_be_bytes());
        out.extend(4u16.to_be_bytes()); // max_stack
        out.extend(4u16.to_be_bytes()); // max_locals
        out.extend((code.len() as u32).to_be_bytes());
        out.extend(*code);
        out.extend(0u16.to_be_bytes()); // exception table
        out.extend(0u16.to_be_bytes()); // attributes
    }
    out.extend(0u16.to_be_bytes()); // attributes
    out
}

/// A method jvm-reader cannot decode is imported without a body, and the rest
/// of the class still imports.
#[test]
fn undecodable_method_is_imported_without_a_body() {
    // 0: iconst_0
    // 1: ifeq 5
    // 4: iconst_1
    // 5: return     reached with stack height 0 and 1
    let bad = [0x03, 0x99, 0x00, 0x04, 0x04, 0xb1];
    let good = [0xb1];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(
        &path,
        class_with_methods(&[("good", "()V", &good), ("bad", "()V", &bad)]),
    )
    .expect("write class");

    let info = import_class(&path).expect("the class imports");
    let body_len = |name: &str| {
        let f = info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name.contains(name))
            .unwrap_or_else(|| panic!("no function {name}"));
        f.blocks.len()
    };
    assert!(body_len("->good()V") > 0, "good keeps its body");
    assert_eq!(body_len("->bad()V"), 0, "bad has no body");
}

/// A value return returns the top of the stack, not its bottom: verified
/// bytecode may return with other values beneath the result.
#[test]
fn value_return_returns_the_top_of_the_stack() {
    // iload_0; iload_1; ireturn     returns the second argument, stack0 is the first
    let int = [0x1a, 0x1b, 0xac];
    // iload_0; lload_1; lreturn     the long is in slots 1 and 2
    let long = [0x1a, 0x1f, 0xad];
    // iload_0; ireturn              alone on the stack
    let alone = [0x1a, 0xac];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(
        &path,
        class_with_methods(&[
            ("int", "(II)I", &int),
            ("long", "(IJ)J", &long),
            ("alone", "(I)I", &alone),
        ]),
    )
    .expect("write class");

    let info = import_class(&path).expect("the class imports");
    let returned = |name: &str| {
        let f = info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name.contains(name))
            .unwrap_or_else(|| panic!("no function {name}"));
        let returns: Vec<String> = f
            .blocks
            .iter()
            .filter_map(|bb| match &bb.terminator.as_ref()?.kind {
                TerminatorKind::Return { args } => {
                    let var = args[0].variable_ref().expect("a variable is returned");
                    Some(f.locals.name(var.variable.local()?).to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(returns.len(), 1, "{name} has one return");
        returns.into_iter().next().unwrap()
    };
    assert_eq!(returned("->int(II)I"), "stack1");
    assert_eq!(returned("->long(IJ)J"), "stack1");
    assert_eq!(returned("->alone(I)I"), "stack0");
}

/// A receiver-less `invokedynamic` targets an extern whose class is named in
/// the same `L…;` form as its signature's class.
#[test]
fn receiverless_invokedynamic_extern_class_matches_its_signature() {
    fn utf8(s: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
        out
    }
    let name = utf8("makeConcatWithConstants"); // 8
    let descriptor = utf8("()Ljava/lang/String;"); // 9
    let name_and_type = [12, 0, 8, 0, 9]; // 10
    let invokedynamic = [18, 0, 0, 0, 10]; // 11: bootstrap method 0
    // invokedynamic #11; pop; return
    let code = [0xba, 0x00, 0x0b, 0x00, 0x00, 0x57, 0xb1];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(
        &path,
        class_with_pool(
            &[&name, &descriptor, &name_and_type, &invokedynamic],
            &[("cat", "()V", &code)],
        ),
    )
    .expect("write class");

    let info = import_class(&path).expect("the class imports");
    let Dispatch::Java { methods, .. } = &info.vmt.dispatch else {
        panic!("a Java VMT");
    };
    let (class, _, _, method) = methods
        .iter()
        .find(|(_, simple_name, _, _)| simple_name.0 == "makeConcatWithConstants")
        .expect("the invokedynamic target is in the VMT");
    assert_eq!(class.0, "L?unknown;");
    assert_eq!(
        method.0,
        "L?unknown;->makeConcatWithConstants()Ljava/lang/String;"
    );
}

/// The copies in the function whose name contains `name`, as `(dest, sources)` with a
/// variable printed by name and `value(v)` for a value-only source.
fn copies(info: &ctadl_ir::ProgramInfo, name: &str) -> Vec<(String, Vec<String>)> {
    use ctadl_ir::{Exp, VariableRef, cir::StatementKind};
    let f = info
        .program
        .functions
        .functions
        .iter()
        .find(|f| f.name.contains(name))
        .unwrap_or_else(|| panic!("no function {name}"));
    let var = |v: &VariableRef| match v.variable.local() {
        Some(l) => f.locals.name(l).to_string(),
        None => format!("{v}"),
    };
    let exp = |e: &Exp| match e {
        Exp::Path(ap) if ap.is_pathless() => var(&ap.base),
        Exp::Value(ap) if ap.is_pathless() => format!("value({})", var(&ap.base)),
        e => format!("{e}"),
    };
    f.blocks
        .iter()
        .flat_map(|bb| bb.statements.iter())
        .filter_map(|s| match &s.kind {
            StatementKind::Assign { dest, sources } if dest.is_pathless() => {
                Some((var(&dest.base), sources.iter().map(exp).collect()))
            }
            _ => None,
        })
        .collect()
}

/// Arithmetic, `iinc` and comparisons supply values only (S19); a load from a local stays a
/// copy.
#[test]
fn arithmetic_and_comparisons_supply_values_only() {
    // iload_0; iload_1; iadd; ireturn
    let add = [0x1a, 0x1b, 0x60, 0xac];
    // iinc 0, 1; iload_0; ireturn
    let inc = [0x84, 0x00, 0x01, 0x1a, 0xac];
    // lload_0; lload_2; lcmp; ireturn
    let cmp = [0x1e, 0x20, 0x94, 0xac];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(
        &path,
        class_with_methods(&[
            ("add", "(II)I", &add),
            ("inc", "(I)I", &inc),
            ("cmp", "(JJ)I", &cmp),
        ]),
    )
    .expect("write class");

    let info = import_class(&path).expect("the class imports");
    let is_value = |s: &String| s.starts_with("value(");

    let add = copies(&info, "->add(II)I");
    assert_eq!(add.len(), 3, "{add:?}");
    assert!(
        add[..2].iter().all(|(_, s)| !s.iter().any(is_value)),
        "the loads are copies: {add:?}"
    );
    assert_eq!(
        add[2],
        (
            "stack0".to_string(),
            vec!["value(stack1)".to_string(), "value(stack0)".to_string()]
        )
    );

    let inc = copies(&info, "->inc(I)I");
    assert!(
        matches!(&inc[0], (_, s) if s.len() == 1 && is_value(&s[0])),
        "iinc reads its local's value: {inc:?}"
    );

    let cmp = copies(&info, "->cmp(JJ)I");
    let (_, sources) = cmp.last().expect("lcmp assigns");
    assert_eq!(sources.len(), 4, "{cmp:?}");
    assert!(sources.iter().all(is_value), "{cmp:?}");
}

/// `ldc` of a `CONSTANT_MethodHandle` that invokes a method is a function reference to it,
/// named as its `FunctionData` is (S11). A `REF_newInvokeSpecial` handle stays an object.
#[test]
fn method_handle_constant_is_a_function_reference() {
    fn utf8(s: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
        out
    }
    let name = utf8("target"); // 8
    let descriptor = utf8("(I)V"); // 9
    let name_and_type = [12, 0, 8, 0, 9]; // 10
    let methodref = [10, 0, 2, 0, 10]; // 11: Demo.target(I)V
    let target = "ptr<LDemo;->target(I)V>";
    // (reference kind, expected source)
    let cases = [
        (5, target),                                  // REF_invokeVirtual
        (6, target),                                  // REF_invokeStatic
        (7, target),                                  // REF_invokeSpecial
        (9, target),                                  // REF_invokeInterface
        (8, "java<Ljava/lang/invoke/MethodHandle;>"), // REF_newInvokeSpecial
    ];
    for (kind, expected) in cases {
        let handle = [15, kind, 0, 11]; // 12
        // ldc #12; astore_0; return
        let code = [0x12, 0x0c, 0x4b, 0xb1];
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Demo.class");
        std::fs::write(
            &path,
            class_with_pool(
                &[&name, &descriptor, &name_and_type, &methodref, &handle],
                &[("mh", "()V", &code)],
            ),
        )
        .expect("write class");

        let info = import_class(&path).expect("the class imports");
        let flows = copies(&info, "->mh()V");
        assert_eq!(flows[0].1, [expected], "kind {kind}: {flows:?}");
        let declared = info
            .program
            .functions
            .functions
            .iter()
            .any(|f| f.name == "LDemo;->target(I)V");
        assert_eq!(
            declared,
            expected == target,
            "kind {kind}: the target is a function"
        );
    }
}

/// `mh.invokeExact(x)` calls whatever the handle `mh` refers to: an indirect call through the
/// handle, passing `x` (S11), so a handle constant resolves it.
#[test]
fn method_handle_invoke_is_an_indirect_call_through_the_handle() {
    use ctadl_ir::cir::StatementKind;
    use ctadl_ir::cir::call::CallStyle;
    fn utf8(s: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
        out
    }
    let pool: [&[u8]; 9] = [
        &[12, 0, 6, 0, 7],                      // 8: NameAndType mh(I)V
        &[10, 0, 2, 0, 8],                      // 9: Methodref Demo.mh(I)V
        &[15, 6, 0, 9],                         // 10: MethodHandle REF_invokeStatic #9
        &utf8("java/lang/invoke/MethodHandle"), // 11
        &[7, 0, 11],                            // 12: Class MethodHandle
        &utf8("invokeExact"),                   // 13
        &utf8("(I)V"),                          // 14
        &[12, 0, 13, 0, 14],                    // 15: NameAndType invokeExact(I)V
        &[10, 0, 12, 0, 15],                    // 16: Methodref MethodHandle.invokeExact
    ];
    // ldc #10; iload_0; invokevirtual #16; return
    let code = [0x12, 0x0a, 0x1a, 0xb6, 0x00, 0x10, 0xb1];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(&path, class_with_pool(&pool, &[("mh", "(I)V", &code)])).expect("write class");

    let info = import_class(&path).expect("the class imports");
    let f = info
        .program
        .functions
        .functions
        .iter()
        .find(|f| f.name == "LDemo;->mh(I)V")
        .expect("mh is defined");
    let calls: Vec<_> = f
        .blocks
        .iter()
        .flat_map(|bb| bb.statements.iter())
        .filter_map(|s| match &s.kind {
            StatementKind::CallAssign { style, args, .. } => Some((style, args)),
            _ => None,
        })
        .collect();
    let [(CallStyle::FuncPtrCall { callee, .. }, args)] = calls[..] else {
        panic!("expected one indirect call: {calls:?}");
    };
    assert!(callee.segments.is_empty(), "{callee}");
    assert_eq!(args.len(), 1, "the handle is not an argument: {args:?}");
    let handle = f
        .locals
        .name(callee.base.variable.local().expect("a local"));
    let flows = copies(&info, "->mh(I)V");
    assert!(
        flows
            .iter()
            .any(|(dest, sources)| dest == handle && sources == &["ptr<LDemo;->mh(I)V>"]),
        "the callee holds the handle's function reference: {flows:?}"
    );
}

/// A static field is a field of its class's global: `putstatic Demo.f` is the store
/// `$LDemo;.f = v` and `getstatic Demo.f` the load `x = $LDemo;.f`, the class declared once.
#[test]
fn a_static_field_is_a_field_of_its_class_global() {
    fn utf8(s: &str) -> Vec<u8> {
        let mut out = vec![1];
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
        out
    }
    let name = utf8("f"); // 8
    let descriptor = utf8("I"); // 9
    let name_and_type = [12, 0, 8, 0, 9]; // 10
    let fieldref = [9, 0, 2, 0, 10]; // 11: Demo.f:I
    // iload_0; putstatic #11; getstatic #11; ireturn
    let code = [0x1a, 0xb3, 0x00, 0x0b, 0xb2, 0x00, 0x0b, 0xac];
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Demo.class");
    std::fs::write(
        &path,
        class_with_pool(
            &[&name, &descriptor, &name_and_type, &fieldref],
            &[("roundtrip", "(I)I", &code)],
        ),
    )
    .expect("write class");

    let info = import_class(&path).expect("the class imports");
    let globals: Vec<String> = info
        .program
        .globals
        .iter_enumerated()
        .map(|(_, g)| g.name.to_string())
        .collect();
    assert_eq!(globals, ["LDemo;"]);
    let dump = info.program.to_string();
    assert!(dump.contains("store $LDemo;.f := "), "{dump}");
    assert!(dump.contains(" = $LDemo;.f ["), "{dump}");

    // The global is declared at the class header: `access_flags`, `this_class` and
    // `super_class` as `class_with_pool` writes them.
    let demo = info.program.globals.lookup("LDemo;").unwrap();
    let span = info
        .program
        .globals
        .get(demo)
        .unwrap()
        .span
        .expect("a span");
    let (artifact, span) = info.source_info.resolve(span).expect("the span resolves");
    assert_eq!(artifact.canonical_path, path.to_string_lossy());
    assert_eq!(span.len, source_info::SpanLen::ByteLen(6));
    let bytes = std::fs::read(&path).unwrap();
    let start = span.start as usize;
    assert_eq!(
        bytes[start..start + 6],
        [0x00, 0x21, 0x00, 0x02, 0x00, 0x04]
    );
}
