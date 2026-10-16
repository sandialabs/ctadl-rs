use super::import_class;

/// A class file for `Demo` with two `public static ()V` methods, `good` and
/// `bad`, each given its bytecode.
fn class_with_methods(good: &[u8], bad: &[u8]) -> Vec<u8> {
    fn utf8(out: &mut Vec<u8>, s: &str) {
        out.push(1);
        out.extend((s.len() as u16).to_be_bytes());
        out.extend(s.as_bytes());
    }
    fn method(out: &mut Vec<u8>, name_index: u16, code: &[u8]) {
        out.extend(0x0009u16.to_be_bytes()); // public static
        out.extend(name_index.to_be_bytes());
        out.extend(6u16.to_be_bytes()); // ()V
        out.extend(1u16.to_be_bytes()); // one attribute: Code
        out.extend(7u16.to_be_bytes());
        out.extend((12 + code.len() as u32).to_be_bytes());
        out.extend(2u16.to_be_bytes()); // max_stack
        out.extend(0u16.to_be_bytes()); // max_locals
        out.extend((code.len() as u32).to_be_bytes());
        out.extend(code);
        out.extend(0u16.to_be_bytes()); // exception table
        out.extend(0u16.to_be_bytes()); // attributes
    }
    let mut out = Vec::new();
    out.extend(0xCAFE_BABEu32.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend(49u16.to_be_bytes());
    out.extend(9u16.to_be_bytes()); // constant pool count: entries 1..=8
    utf8(&mut out, "Demo"); // 1
    out.extend([7, 0, 1]); // 2: Class Demo
    utf8(&mut out, "java/lang/Object"); // 3
    out.extend([7, 0, 3]); // 4: Class java/lang/Object
    utf8(&mut out, "good"); // 5
    utf8(&mut out, "()V"); // 6
    utf8(&mut out, "Code"); // 7
    utf8(&mut out, "bad"); // 8
    out.extend(0x0021u16.to_be_bytes()); // public super
    out.extend(2u16.to_be_bytes());
    out.extend(4u16.to_be_bytes());
    out.extend(0u16.to_be_bytes()); // interfaces
    out.extend(0u16.to_be_bytes()); // fields
    out.extend(2u16.to_be_bytes()); // methods
    method(&mut out, 5, good);
    method(&mut out, 8, bad);
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
    std::fs::write(&path, class_with_methods(&good, &bad)).expect("write class");

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
