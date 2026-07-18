//! Property test: generate an arbitrary [`BytecodeFile`], serialize it to stable
//! text with a test-only Rust serializer, parse it back with the reader, and
//! assert the round-trip is the identity. Explores optional fields, unicode /
//! escaped strings, all const variants, nesting, empty lists, and large numbers.

use proptest::prelude::*;
use python_bytecode_reader::model::*;
use python_bytecode_reader::parse;

// --- Test-only stable-text serializer -------------------------------------

/// Minimal JSON-ish string escape: the reader's grammar accepts any raw byte in
/// a string except `"` and `\`, so escaping just those two round-trips exactly.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn ser_value(v: &ConstEntry) -> String {
    match v {
        ConstEntry::None => "none".into(),
        ConstEntry::Bool(b) => format!("bool {}", if *b { "true" } else { "false" }),
        ConstEntry::Int(i) => format!("int {i}"),
        ConstEntry::Float(s) => format!("float {}", esc(s)),
        ConstEntry::Str(s) => format!("str {}", esc(s)),
        ConstEntry::Bytes(s) => format!("bytes {}", esc(s)),
        ConstEntry::Code(n) => format!("code {n}"),
        ConstEntry::Other(s) => format!("other {}", esc(s)),
    }
}

fn ser_opt_int(v: &Option<i64>) -> String {
    v.map(|i| i.to_string()).unwrap_or_else(|| "none".into())
}

fn ser_file(f: &BytecodeFile) -> String {
    let mut out = format!("bytecode_format {}\n", f.format_version);
    for co in &f.code_objects {
        ser_code(&mut out, co);
    }
    out
}

fn ser_code(out: &mut String, co: &CodeObject) {
    out.push_str("code_object {\n");
    out.push_str(&format!("name {}\n", esc(&co.name)));
    out.push_str(&format!("qualname {}\n", esc(&co.qualname)));
    out.push_str(&format!("filename {}\n", esc(&co.filename)));
    out.push_str(&format!("first_line {}\n", ser_opt_int(&co.first_line)));
    out.push_str(&format!("flags {}\n", co.flags));
    out.push_str(&format!("arg_count {}\n", co.arg_count));
    out.push_str(&format!("kwonly_count {}\n", co.kwonly_count));
    out.push_str(&format!(
        "names [{}]\n",
        co.names
            .iter()
            .map(|s| esc(s))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "varnames [{}]\n",
        co.varnames
            .iter()
            .map(|s| esc(s))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "consts [{}]\n",
        co.consts
            .iter()
            .map(ser_value)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    for insn in &co.instructions {
        ser_insn(out, insn);
    }
    for nested in &co.nested_code_objects {
        ser_code(out, nested);
    }
    out.push_str("}\n");
}

fn ser_insn(out: &mut String, insn: &Instruction) {
    out.push_str("instruction {\n");
    out.push_str(&format!("offset {}\n", insn.offset));
    out.push_str(&format!("opname {}\n", insn.opname));
    out.push_str(&format!("opcode {}\n", insn.opcode));
    out.push_str(&format!("arg {}\n", ser_opt_int(&insn.arg)));
    out.push_str(&format!("argval {}\n", ser_value(&insn.argval)));
    out.push_str(&format!(
        "argrepr {}\n",
        insn.argrepr
            .as_ref()
            .map(|s| esc(s))
            .unwrap_or_else(|| "none".into())
    ));
    out.push_str(&format!("starts_line {}\n", ser_opt_int(&insn.starts_line)));
    out.push_str(&format!(
        "is_jump_target {}\n",
        if insn.is_jump_target { "true" } else { "false" }
    ));
    out.push_str(&format!(
        "jump_targets [{}]\n",
        insn.jump_targets
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    match &insn.position {
        None => out.push_str("position none\n"),
        Some(p) => out.push_str(&format!(
            "position {}:{}-{}:{}\n",
            p.start_line, p.start_column, p.end_line, p.end_column
        )),
    }
    out.push_str("}\n");
}

// --- Strategies -----------------------------------------------------------

/// Strings with tricky characters: quotes, backslashes, control chars, unicode.
fn tricky_string() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z\"\\\\\n\t\u{00e9}\u{1F600}]{0,6}").unwrap()
}

/// An identifier-shaped opname (grammar: `(ALNUM | _)+`, non-empty).
fn opname() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Z_][A-Z0-9_]{0,10}").unwrap()
}

fn const_entry() -> impl Strategy<Value = ConstEntry> {
    prop_oneof![
        Just(ConstEntry::None),
        any::<bool>().prop_map(ConstEntry::Bool),
        any::<i64>().prop_map(ConstEntry::Int),
        tricky_string().prop_map(ConstEntry::Float),
        tricky_string().prop_map(ConstEntry::Str),
        tricky_string().prop_map(ConstEntry::Bytes),
        any::<u32>().prop_map(ConstEntry::Code),
        tricky_string().prop_map(ConstEntry::Other),
    ]
}

fn position() -> impl Strategy<Value = Option<Position>> {
    prop_oneof![
        Just(None),
        (any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>()).prop_map(|(a, b, c, d)| Some(
            Position {
                start_line: a,
                start_column: b,
                end_line: c,
                end_column: d,
            }
        )),
    ]
}

fn instruction() -> impl Strategy<Value = Instruction> {
    (
        any::<i64>(),
        opname(),
        any::<i64>(),
        proptest::option::of(any::<i64>()),
        const_entry(),
        proptest::option::of(tricky_string()),
        proptest::option::of(any::<i64>()),
        any::<bool>(),
        prop::collection::vec(any::<i64>(), 0..4),
        position(),
    )
        .prop_map(
            |(
                offset,
                opname,
                opcode,
                arg,
                argval,
                argrepr,
                starts_line,
                is_jump_target,
                jump_targets,
                position,
            )| {
                Instruction {
                    offset,
                    opname,
                    opcode,
                    arg,
                    argval,
                    argrepr,
                    starts_line,
                    is_jump_target,
                    jump_targets,
                    position,
                }
            },
        )
}

/// A code object without nesting (leaf), used to bound recursion depth.
fn leaf_code() -> impl Strategy<Value = CodeObject> {
    (
        tricky_string(),
        tricky_string(),
        tricky_string(),
        proptest::option::of(any::<i64>()),
        any::<i64>(),
        any::<i64>(),
        any::<i64>(),
        prop::collection::vec(tricky_string(), 0..3),
        prop::collection::vec(tricky_string(), 0..3),
        prop::collection::vec(const_entry(), 0..4),
        prop::collection::vec(instruction(), 0..3),
    )
        .prop_map(
            |(
                name,
                qualname,
                filename,
                first_line,
                flags,
                arg_count,
                kwonly_count,
                names,
                varnames,
                consts,
                instructions,
            )| {
                CodeObject {
                    name,
                    qualname,
                    filename,
                    first_line,
                    flags,
                    arg_count,
                    kwonly_count,
                    names,
                    varnames,
                    consts,
                    instructions,
                    nested_code_objects: Vec::new(),
                }
            },
        )
}

/// A code object with up to two levels of nesting.
fn code_object() -> impl Strategy<Value = CodeObject> {
    leaf_code().prop_recursive(2, 8, 2, |inner| {
        (leaf_code(), prop::collection::vec(inner, 0..2)).prop_map(|(mut co, nested)| {
            co.nested_code_objects = nested;
            co
        })
    })
}

fn bytecode_file() -> impl Strategy<Value = BytecodeFile> {
    prop::collection::vec(code_object(), 1..3).prop_map(|code_objects| BytecodeFile {
        format_version: 1,
        code_objects,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn round_trip(file in bytecode_file()) {
        let text = ser_file(&file);
        let parsed = parse(&text).unwrap_or_else(|e| panic!("parse failed: {e}\n--- text ---\n{text}"));
        prop_assert_eq!(parsed, file);
    }
}
