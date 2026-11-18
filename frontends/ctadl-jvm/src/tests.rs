use super::import_class;
use ctadl_ir::mir::TerminatorKind;
use ctadl_ir::mir::call::VirtualMethodTable;

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
    let VirtualMethodTable::Java { methods, .. } = &info.vmt else {
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
