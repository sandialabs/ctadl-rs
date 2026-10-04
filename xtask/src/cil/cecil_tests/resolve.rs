//! Ported from Test/Mono.Cecil.Tests/ResolveTests.cs (read subset), adapted:
//! corlib-sensitive tests resolve against the pinned
//! `symbols/mdb/.../mscorlib.dll` fixture instead of the running framework's
//! corlib. References come from corpus assemblies rather than `typeof`
//! imports (we have no import machinery — reading only).
//!
//! Skipped: ResolveNullVersionAssembly / ResolvePortableClassLibraryReference
//! (probe the machine's framework installs), ResolveGenericParameter /
//! CorlibPrimitive-style tests (construct references).

use super::{cecil_root, read_fixture, read_resource, Test};
use cil_reader::cil::RawOperand;
use cil_reader::model::resolver::{AssemblyCache, DirectoryLoader, ResolvedMember};
use cil_reader::{MetadataToken, Module, TokenType};

fn loader() -> DirectoryLoader {
    let cecil = cecil_root();
    DirectoryLoader::new(vec![
        // The pinned corlib fixture.
        cecil.join("symbols/mdb/Test/Resources/assemblies"),
        // The main corpus (netmodules, forwarder fixtures).
        cecil.join("Test/Resources/assemblies"),
    ])
}

/// First MemberRef operand in the method's body matching a predicate on the
/// rendered instruction.
fn member_ref_operand(
    module: &Module<'_>,
    type_name: &str,
    method_name: &str,
    contains: &str,
) -> MetadataToken {
    let ty = module.get_type(type_name).unwrap().unwrap();
    let method = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == method_name)
        .unwrap();
    let body = method.body().unwrap();
    body.instructions
        .iter()
        .find_map(|ins| match ins.operand {
            RawOperand::Token(t) if t.token_type() == Some(TokenType::MemberRef) => {
                let rendered = body.format_instruction(ins).unwrap();
                rendered.contains(contains).then_some(t)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no MemberRef matching {contains}"))
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:16 (StringEmpty) — the reference
// comes from hello.il.dll's `ldsfld System.String System.String::Empty`.
fn string_empty() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    let token = member_ref_operand(&module, "Foo", "PrintEmpty", "System.String::Empty");
    match module.resolve_member(token, &cache).unwrap() {
        Some(ResolvedMember::Field(def_module, field)) => {
            assert_eq!(
                field.full_name().unwrap(),
                "System.String System.String::Empty"
            );
            assert_eq!(
                def_module.assembly_name().unwrap().unwrap().name,
                "mscorlib"
            );
        }
        _ => panic!("expected a field definition"),
    }
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:36 (StringSubstring analogue) —
// overload selection: resolve Console::WriteLine(System.String) among the
// many WriteLine overloads.
fn console_write_line_overload() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    let token = member_ref_operand(
        &module,
        "Foo",
        "PrintEmpty",
        "System.Console::WriteLine(System.String)",
    );
    match module.resolve_member(token, &cache).unwrap() {
        Some(ResolvedMember::Method(def_module, method)) => {
            assert_eq!(
                method.full_name().unwrap(),
                "System.Void System.Console::WriteLine(System.String)"
            );
            assert_eq!(
                def_module.assembly_name().unwrap().unwrap().name,
                "mscorlib"
            );
        }
        _ => panic!("expected a method definition"),
    }
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:66 (ListOfStringAdd analogue) — a
// member on a generic instance resolves to the open definition. Adapted to an
// in-module generic type because the net8-built fixture's List`1 lives in
// System.Collections, outside the pinned-fixture universe.
fn generic_instance_member_resolves_to_open_definition() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    // Null::Compare's `ldfld T Nilible`1<T>::t` — a field on a generic
    // instance of an in-module type.
    let token = member_ref_operand(&module, "Null", "Compare", "Nilible`1<T>::t");
    match module.resolve_member(token, &cache).unwrap() {
        Some(ResolvedMember::Field(_, field)) => {
            assert_eq!(field.full_name().unwrap(), "T Nilible`1::t");
        }
        _ => panic!("expected a field definition"),
    }
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:113 (ExportedTypeFromModule) — the
// exported type Module.A.Foo forwards into moda.netmodule via a File row.
fn exported_type_from_module() {
    let data = read_resource("mma.exe");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    // Find the TypeRef for Module.A.Foo (referenced from the main module).
    let tr = (1..=module.metadata().row_count(cil_reader::raw::Table::TypeRef))
        .filter_map(|rid| module.type_ref(rid))
        .find(|tr| tr.full_name().unwrap() == "Module.A.Foo")
        .expect("TypeRef Module.A.Foo");
    let resolved = module
        .resolve_type(tr.token(), &cache)
        .unwrap()
        .expect("resolved");
    assert_eq!(resolved.ty.full_name().unwrap(), "Module.A.Foo");
    assert_eq!(resolved.module.name().unwrap(), "moda.netmodule");
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:152 (TypeForwarderCircularity) —
// TestClass forwards Loop1 → Loop2 → Loop1; Cecil throws, we error.
fn type_forwarder_circularity() {
    let data = read_resource("TypeForwarderLoop1.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    // Loop1 exports MyNamespace.MyType to Loop2, which forwards it back;
    // resolution must error rather than loop (Cecil throws).
    assert!(module
        .resolve_type_name("MyNamespace", "MyType", &cache)
        .is_err());
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:191 (RectangularArrayResolveGetMethod)
// — a Get method on a rectangular generic-parameter array resolves to null.
fn rectangular_array_resolve_get_method() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    let token = member_ref_operand(&module, "LaMatrix", "At", "T[0...,0...]::Get");
    assert!(module.resolve_member(token, &cache).unwrap().is_none());
}

// Test/Mono.Cecil.Tests/ResolveTests.cs:214 (ResolveFunctionPointer) — a
// function-pointer type resolves to null. Cecil reads it from a cppcli field
// type; so do we.
fn resolve_function_pointer() {
    let data = read_resource("cppcli.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    use cil_reader::model::TypeSig;
    let fnptr = (1..=module.metadata().row_count(cil_reader::raw::Table::Field))
        .filter_map(|rid| module.field_def(rid))
        .find_map(|f| match f.field_type() {
            Ok(sig @ TypeSig::FnPtr(_)) => Some(sig),
            Ok(TypeSig::Ptr(inner)) if matches!(*inner, TypeSig::FnPtr(_)) => Some(*inner),
            _ => None,
        })
        .expect("a function-pointer field type");
    assert!(module.resolve_type_sig(&fnptr, &cache).unwrap().is_none());
}

// In-module resolution needs no loader hits: Program's base type Object is a
// TypeRef into the corlib fixture; a TypeRef to an in-module type resolves
// without touching the cache.
fn resolve_in_module_type() {
    let data = read_resource("mma.exe");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    // moda.netmodule's Foo referenced through the exported type; also check a
    // TypeDef token resolves to itself.
    let program = module.types().unwrap()[1];
    let resolved = module
        .resolve_type(program.token(), &cache)
        .unwrap()
        .expect("self-resolution");
    assert_eq!(resolved.ty, program);
}

// gaps.md A19: a primitive resolves as Cecil's TypeSystem type, a reference
// into the core library (TypeSystem.Int32.Resolve()). The corpus covers a
// core library resolving its own primitives; this covers another module's.
fn primitive_resolves_to_corlib() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let loader = loader();
    let cache = AssemblyCache::new(&loader);

    use cil_reader::model::TypeSig;
    for (sig, expected) in [
        (TypeSig::String, "System.String"),
        (TypeSig::SzArray(Box::new(TypeSig::I4)), "System.Int32"),
        (TypeSig::Ptr(Box::new(TypeSig::Void)), "System.Void"),
    ] {
        let resolved = module
            .resolve_type_sig(&sig, &cache)
            .unwrap()
            .unwrap_or_else(|| panic!("{expected} did not resolve"));
        assert_eq!(resolved.ty.full_name().unwrap(), expected);
        assert_eq!(
            resolved.module.assembly_name().unwrap().unwrap().name,
            "mscorlib"
        );
    }
    assert!(module
        .resolve_type_sig(&TypeSig::MVar(0), &cache)
        .unwrap()
        .is_none());
}

pub(super) const TESTS: &[Test] = &[
    ("string_empty", string_empty),
    ("console_write_line_overload", console_write_line_overload),
    (
        "generic_instance_member_resolves_to_open_definition",
        generic_instance_member_resolves_to_open_definition,
    ),
    ("exported_type_from_module", exported_type_from_module),
    ("type_forwarder_circularity", type_forwarder_circularity),
    (
        "rectangular_array_resolve_get_method",
        rectangular_array_resolve_get_method,
    ),
    ("resolve_function_pointer", resolve_function_pointer),
    ("resolve_in_module_type", resolve_in_module_type),
    ("primitive_resolves_to_corlib", primitive_resolves_to_corlib),
];
