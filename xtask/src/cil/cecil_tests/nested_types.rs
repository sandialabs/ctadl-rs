//! Ported from Test/Mono.Cecil.Tests/NestedTypesTests.cs (read subset).
//! Skipped: NestedTypeFullName (constructs definitions).

use super::{read_fixture, read_resource, Test};
use cil_reader::Module;

// Test/Mono.Cecil.Tests/NestedTypesTests.cs:12 (NestedTypes)
fn nested_types() {
    let data = read_fixture("NestedTypes.cs.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();

    assert_eq!(foo.name().unwrap(), "Foo");
    assert_eq!(foo.full_name().unwrap(), "Foo");
    let foo_nested = foo.nested_types().unwrap();
    assert_eq!(foo_nested.len(), 1);

    let bar = foo_nested[0];
    assert_eq!(bar.name().unwrap(), "Bar");
    assert_eq!(bar.full_name().unwrap(), "Foo/Bar");
    let bar_nested = bar.nested_types().unwrap();
    assert_eq!(bar_nested.len(), 1);

    let baz = bar_nested[0];
    assert_eq!(baz.name().unwrap(), "Baz");
    assert_eq!(baz.full_name().unwrap(), "Foo/Bar/Baz");
}

// Test/Mono.Cecil.Tests/NestedTypesTests.cs:38 (DirectNestedType)
fn direct_nested_type() {
    let data = read_fixture("NestedTypes.cs.dll");
    let module = Module::read(&data).unwrap();
    let bingo = module.get_type("Bingo").unwrap().unwrap();
    let get_fuel = bingo
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "GetFuel")
        .unwrap();
    assert_eq!(
        module
            .type_sig_full_name(&get_fuel.return_type().unwrap(), Default::default())
            .unwrap(),
        "Bingo/Fuel"
    );
}

// Test/Mono.Cecil.Tests/NestedTypesTests.cs:49 (NestedTypeWithOwnNamespace)
fn nested_type_with_own_namespace() {
    let data = read_resource("bug-185.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let foo_child = foo.nested_types().unwrap()[0];

    assert_eq!(foo_child.namespace().unwrap(), "<IFoo<System.Byte[]>");
    assert_eq!(foo_child.name().unwrap(), "Do>d__0");
    assert_eq!(
        foo_child.full_name().unwrap(),
        "Foo/<IFoo<System.Byte[]>.Do>d__0"
    );
}

pub(super) const TESTS: &[Test] = &[
    ("nested_types", nested_types),
    ("direct_nested_type", direct_nested_type),
    (
        "nested_type_with_own_namespace",
        nested_type_with_own_namespace,
    ),
];
