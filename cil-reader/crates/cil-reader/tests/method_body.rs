//! Ported from Test/Mono.Cecil.Tests/MethodBodyTests.cs (read subset), plus
//! the body-dependent tests deferred from TypeTests/MethodTests.
//!
//! Skipped: ThisParameterStaticMethod/Primitive/ValueType/Object (read the
//! running framework's corlib), UnattachedMethodBody/Add/Insert/Remove
//! (ILProcessor, write side).

mod testutil;

use cil_reader::Module;
use cil_reader::cil::{Code, RawOperand};
use cil_reader::model::{MethodDef, TypeDef};
use testutil::{normalize, read_fixture, read_resource};

fn get_method<'a>(ty: &TypeDef<'a>, name: &str) -> MethodDef<'a> {
    ty.methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == name)
        .unwrap_or_else(|| panic!("no method {name}"))
}

/// `BaseTestFixture.AssertCode`: expected IL copied verbatim from the Cecil
/// tests, normalized like `BaseTestFixture.Normalize` (trim + CRLF→LF).
macro_rules! assert_code {
    ($expected:expr, $method:expr) => {{
        let body = $method.body().unwrap();
        assert_eq!(normalize($expected), normalize(&body.formatted().unwrap()));
    }};
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:14 (MultiplyMethod)
#[test]
fn multiply_method() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let bar = get_method(&foo, "Bar");
    // IsIL
    assert_eq!(bar.impl_attributes().unwrap() & 0x3, 0);

    assert_code!(
        r#"
	.locals init (System.Int32 V_0)
	IL_0000: ldarg.0
	IL_0001: ldarg.1
	IL_0002: mul
	IL_0003: stloc.0
	IL_0004: ldloc.0
	IL_0005: call System.Void Foo::Baz(System.Int32)
	IL_000a: ret
"#,
        bar
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:38 (PrintStringEmpty)
#[test]
fn print_string_empty() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let print_empty = get_method(&foo, "PrintEmpty");

    assert_code!(
        r#"
	.locals ()
	IL_0000: ldsfld System.String System.String::Empty
	IL_0005: call System.Void System.Console::WriteLine(System.String)
	IL_000a: ret
"#,
        print_empty
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:57 (Branch)
#[test]
fn branch() {
    let data = read_resource("libhello.dll");
    let module = Module::read(&data).unwrap();
    let lib = module.get_type("Library").unwrap().unwrap();
    let method = get_method(&lib, "GetHelloString");

    assert_code!(
        r#"
	.locals init (System.String V_0)
	IL_0000: nop
	IL_0001: ldstr "hello world of tomorrow"
	IL_0006: stloc.0
	IL_0007: br.s IL_0009
	IL_0009: ldloc.0
	IL_000a: ret
"#,
        method
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:79 (Switch)
#[test]
fn switch_test() {
    let data = read_resource("switch.exe");
    let module = Module::read(&data).unwrap();
    let program = module.get_type("Program").unwrap().unwrap();
    let method = get_method(&program, "Main");

    assert_code!(
        r#"
	.locals init (System.Int32 V_0)
	IL_0000: ldarg.0
	IL_0001: ldlen
	IL_0002: conv.i4
	IL_0003: stloc.0
	IL_0004: ldloc.0
	IL_0005: ldc.i4.8
	IL_0006: bgt.s IL_0026
	IL_0008: ldloc.0
	IL_0009: ldc.i4.1
	IL_000a: sub
	IL_000b: switch (IL_0032, IL_0034, IL_0038, IL_0034)
	IL_0020: ldloc.0
	IL_0021: ldc.i4.8
	IL_0022: beq.s IL_0036
	IL_0024: br.s IL_0038
	IL_0026: ldloc.0
	IL_0027: ldc.i4.s 16
	IL_0029: beq.s IL_0036
	IL_002b: ldloc.0
	IL_002c: ldc.i4.s 32
	IL_002e: beq.s IL_0036
	IL_0030: br.s IL_0038
	IL_0032: ldc.i4.0
	IL_0033: ret
	IL_0034: ldc.i4.1
	IL_0035: ret
	IL_0036: ldc.i4.2
	IL_0037: ret
	IL_0038: ldc.i4.s 42
	IL_003a: ret
"#,
        method
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:125 (MethodSpec)
#[test]
fn method_spec() {
    let data = read_fixture("methodspecs.il.dll");
    let module = Module::read(&data).unwrap();
    let tamtam = module.get_type("Tamtam").unwrap().unwrap();
    let bar = get_method(&tamtam, "Bar");

    assert_code!(
        r#"
	.locals ()
	IL_0000: ldc.i4.2
	IL_0001: call System.Void Tamtam::Foo<System.Int32>(TFoo)
	IL_0006: ret
"#,
        bar
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:143 (NestedTryCatchFinally)
#[test]
fn nested_try_catch_finally() {
    let data = read_resource("catch.exe");
    let module = Module::read(&data).unwrap();
    let program = module.get_type("Program").unwrap().unwrap();
    let main = get_method(&program, "Main");

    assert_code!(
        r#"
	.locals ()
	IL_0000: call System.Void Program::Foo()
	IL_0005: leave.s IL_000d
	IL_0007: call System.Void Program::Baz()
	IL_000c: endfinally
	IL_000d: leave.s IL_001f
	IL_000f: pop
	IL_0010: call System.Void Program::Bar()
	IL_0015: leave.s IL_001f
	IL_0017: pop
	IL_0018: call System.Void Program::Bar()
	IL_001d: leave.s IL_001f
	IL_001f: leave.s IL_0027
	IL_0021: call System.Void Program::Baz()
	IL_0026: endfinally
	IL_0027: ret
	.try IL_0000 to IL_0007 finally handler IL_0007 to IL_000d
	.try IL_0000 to IL_000f catch System.ArgumentException handler IL_000f to IL_0017
	.try IL_0000 to IL_000f catch System.Exception handler IL_0017 to IL_001f
	.try IL_0000 to IL_0021 finally handler IL_0021 to IL_0027
"#,
        main
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:176 (FunctionPointersAndCallSites)
#[test]
fn function_pointers_and_call_sites() {
    let data = read_resource("fptr.exe");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    let ty = &types[0];
    let start = get_method(ty, "Start");

    assert_code!(
        r#"
	.locals init ()
	IL_0000: ldc.i4.1
	IL_0001: call method System.Int32 *(System.Int32) MakeDecision::Decide()
	IL_0006: calli System.Int32(System.Int32)
	IL_000b: call System.Void System.Console::WriteLine(System.Int32)
	IL_0010: ldc.i4.1
	IL_0011: call method System.Int32 *(System.Int32) MakeDecision::Decide()
	IL_0016: calli System.Int32(System.Int32)
	IL_001b: call System.Void System.Console::WriteLine(System.Int32)
	IL_0020: ldc.i4.1
	IL_0021: call method System.Int32 *(System.Int32) MakeDecision::Decide()
	IL_0026: calli System.Int32(System.Int32)
	IL_002b: call System.Void System.Console::WriteLine(System.Int32)
	IL_0030: ret
"#,
        start
    );
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:203 (ThisParameter)
#[test]
fn this_parameter() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo").unwrap().unwrap();
    let method = get_method(&ty, "Gazonk");

    assert_code!(
        r#"
	.locals ()
	IL_0000: ldarg 0
	IL_0004: pop
	IL_0005: ret
"#,
        method
    );

    // Cecil: Instructions[0].Operand == body.ThisParameter (arg index 0 on an
    // instance method).
    let body = method.body().unwrap();
    assert_eq!(body.instructions[0].operand, RawOperand::Arg(0));
    assert!(method.signature().unwrap().has_this);
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:272 (FilterMaxStack)
#[test]
fn filter_max_stack() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo").unwrap().unwrap();
    let method = get_method(&ty, "TestFilter");
    assert_eq!(method.body().unwrap().max_stack, 2);
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:284 (BranchOutsideMethod) — not
// Windows-only here: CoreCLR ilasm assembles branch-out.il fine (PLAN.md §7).
#[test]
fn branch_outside_method() {
    let data = read_fixture("branch-out.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo").unwrap().unwrap();
    let method = get_method(&ty, "BranchOutside");

    let body = method.body().unwrap();
    let leave = &body.instructions[0];
    assert_eq!(leave.opcode.code, Code::Leave);
    assert_eq!(body.format_instruction(leave).unwrap(), "IL_0000: leave");
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:303 (Iterator)
#[test]
fn iterator() {
    let data = read_resource("iterator.exe");
    let module = Module::read(&data).unwrap();
    let program = module.get_type("Program").unwrap().unwrap();
    let method = get_method(&program, "GetLittleArgs");
    assert!(!method.body().unwrap().instructions.is_empty());
}

// Test/Mono.Cecil.Tests/MethodBodyTests.cs:312 (LoadString)
#[test]
fn load_string() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("FooAttribute").unwrap().unwrap();
    let get_fiou = get_method(&ty, "get_Fiou");

    let body = get_fiou.body().unwrap();
    let ldstr = body
        .instructions
        .iter()
        .find(|i| i.opcode.code == Code::Ldstr)
        .expect("ldstr");
    let RawOperand::String(token) = ldstr.operand else {
        panic!("expected string operand")
    };
    assert_eq!(
        module
            .metadata()
            .user_strings()
            .get(token.rid())
            .unwrap()
            .to_string_lossy(),
        "fiou"
    );
}

// Test/Mono.Cecil.Tests/TypeTests.cs:163 (TypeReferenceValueType) — deferred
// from tests/types.rs; needs bodies.
#[test]
fn type_reference_value_type() {
    use cil_reader::model::TypeSig;
    let data = read_fixture("Methods.cs.dll");
    let module = Module::read(&data).unwrap();
    let baz = module.get_type("Baz").unwrap().unwrap();
    let method = get_method(&baz, "PrintAnswer");

    let body = method.body().unwrap();
    let boxing = body
        .instructions
        .iter()
        .find(|i| i.opcode.code == Code::Box)
        .expect("box instruction");
    let RawOperand::Token(token) = boxing.operand else {
        panic!("expected token operand")
    };
    // Cecil: the boxed int32 TypeReference knows it is a value type. Here the
    // primitive lives in the TypeRef table; its System.Int32 identity is the
    // check.
    match module.lookup_token(token).unwrap().unwrap() {
        cil_reader::Entity::TypeRef(tr) => {
            assert_eq!(tr.full_name().unwrap(), "System.Int32");
        }
        cil_reader::Entity::TypeSpec(ts) => {
            assert_eq!(ts.signature().unwrap(), TypeSig::I4);
        }
        _ => panic!("unexpected box operand"),
    }
}

// Test/Mono.Cecil.Tests/MethodTests.cs:178 (GenericInstanceMethod) — deferred
// from tests/methods.rs.
#[test]
fn generic_instance_method() {
    use cil_reader::TokenType;
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("It").unwrap().unwrap();
    let method = get_method(&ty, "ReadPwow");

    let body = method.body().unwrap();
    let spec_token = body
        .instructions
        .iter()
        .find_map(|i| match i.operand {
            RawOperand::Token(t) if t.token_type() == Some(TokenType::MethodSpec) => Some(t),
            _ => None,
        })
        .expect("MethodSpec operand");
    assert_ne!(spec_token.rid(), 0);
}

// Test/Mono.Cecil.Tests/MethodTests.cs:200 (MethodRefDeclaredOnGenerics) —
// deferred from tests/methods.rs.
#[test]
fn method_ref_declared_on_generics() {
    use cil_reader::TokenType;
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Tamtam").unwrap().unwrap();

    for (method_name, expected) in [
        ("Beta", "System.Collections.Generic.List`1<TBeta>"),
        ("Charlie", "System.Collections.Generic.List`1<TCharlie>"),
    ] {
        let method = get_method(&ty, method_name);
        let body = method.body().unwrap();
        let ins = body
            .instructions
            .iter()
            .find(|i| {
                matches!(i.operand, RawOperand::Token(t)
                    if t.token_type() == Some(TokenType::MemberRef))
            })
            .expect("MemberRef operand");
        // The rendered call is ".ctor" on List`1<TBeta>; check the declaring
        // type's full name inside the formatted operand.
        let formatted = body.format_instruction(ins).unwrap();
        assert!(
            formatted.contains(expected),
            "{method_name}: {formatted} missing {expected}"
        );
    }
}

// Test/Mono.Cecil.Tests/MethodTests.cs:144 (VarArgs) — body half deferred from
// tests/methods.rs: call-site references to vararg methods render with the
// sentinel.
#[test]
fn varargs_call_sites() {
    let data = read_resource("varargs.exe");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    let module_type = &types[0];
    let baz = get_method(module_type, "Baz");
    let body = baz.body().unwrap();

    let call_at = |offset: u32| {
        body.instructions
            .iter()
            .find(|i| i.offset == offset)
            .expect("instruction at offset")
    };
    // 0x000a: call to Foo(...) with an empty fixed part → sentinel position 0.
    let formatted = body.format_instruction(call_at(0x000a)).unwrap();
    assert!(formatted.contains("...,"), "{formatted}");
    // 0x0023: call to Bar(str, ...) → sentinel position 1.
    let formatted = body.format_instruction(call_at(0x0023)).unwrap();
    assert!(formatted.contains("...,"), "{formatted}");
}
