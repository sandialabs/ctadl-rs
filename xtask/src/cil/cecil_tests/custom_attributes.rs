//! Ported from Test/Mono.Cecil.Tests/CustomAttributesTests.cs (read subset).
//!
//! Skipped: DefineCustomAttributeFromBlob (writes a module).
//! Adapted: TypeReferenceEnum/BoxedEnumReference resolve
//! System.Security.AccessControl.AceFlags through a loader that serves the
//! mdb corpus's mscorlib.dll, which defines it, where Cecil's resolver finds
//! the framework's.

use super::{Test, cecil_root, read_fixture, read_resource};
use cil_reader::model::AssemblyName;
use cil_reader::model::custom_attributes::{CaArgument, CaValue, CustomAttribute};
use cil_reader::model::resolver::{AssemblyCache, AssemblyLoader};
use cil_reader::{MetadataToken, Module};
use std::fmt::Write as _;

/// `CustomAttributesTests.GetAttribute`: first attribute whose type name
/// starts with `name`.
fn get_attribute<'a>(
    module: &'a Module<'a>,
    owner: MetadataToken,
    name: &str,
) -> CustomAttribute<'a> {
    module
        .custom_attributes_of(owner)
        .unwrap()
        .into_iter()
        .find(|a| a.attribute_type_name().unwrap().starts_with(name))
        .unwrap_or_else(|| panic!("no {name} attribute"))
}

fn type_attr<'a>(module: &'a Module<'a>, type_name: &str, attr: &str) -> CustomAttribute<'a> {
    let ty = module.get_type(type_name).unwrap().unwrap();
    get_attribute(module, ty.token(), attr)
}

fn assert_argument(expected_type: &str, expected: &CaValue<'_>, argument: &CaArgument<'_>) {
    assert_eq!(argument.ty.full_name(), expected_type);
    assert_eq!(&argument.value, expected);
}

// --- the tests' PrettyPrint, ported verbatim in behavior ---

fn pretty_print(attr: &CustomAttribute<'_>) -> String {
    let values = attr.values().unwrap();
    let mut s = String::from(".ctor (");
    for (i, arg) in values.constructor_args.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        pp_argument(arg, &mut s);
    }
    s.push(')');
    s
}

fn pp_argument(arg: &CaArgument<'_>, s: &mut String) {
    s.push('(');
    s.push_str(&arg.ty.etype_name());
    s.push(':');
    pp_value(&arg.value, s);
    s.push(')');
}

fn pp_value(value: &CaValue<'_>, s: &mut String) {
    match value {
        CaValue::Null => s.push_str("null"),
        CaValue::Array(items) => {
            s.push('{');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                pp_argument(item, s);
            }
            s.push('}');
        }
        CaValue::String(v) => write!(s, "\"{v}\"").unwrap(),
        CaValue::Char(c) => write!(s, "'{}'", char::from_u32(*c as u32).unwrap()).unwrap(),
        CaValue::Boxed(inner) => pp_argument(inner, s),
        CaValue::Boolean(b) => write!(s, "{}", if *b { "True" } else { "False" }).unwrap(),
        CaValue::I1(v) => write!(s, "{v}").unwrap(),
        CaValue::U1(v) => write!(s, "{v}").unwrap(),
        CaValue::I2(v) => write!(s, "{v}").unwrap(),
        CaValue::U2(v) => write!(s, "{v}").unwrap(),
        CaValue::I4(v) => write!(s, "{v}").unwrap(),
        CaValue::U4(v) => write!(s, "{v}").unwrap(),
        CaValue::I8(v) => write!(s, "{v}").unwrap(),
        CaValue::U8(v) => write!(s, "{v}").unwrap(),
        CaValue::R4(v) => write!(s, "{v}").unwrap(),
        CaValue::R8(v) => write!(s, "{v}").unwrap(),
        CaValue::TypeName(name) => s.push_str(name),
    }
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:18 (StringArgumentOnType)
fn string_argument_on_type() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let hamster = module.get_type("Hamster").unwrap().unwrap();
    let attrs = module.custom_attributes_of(hamster.token()).unwrap();
    assert!(module.has_custom_attributes(hamster.token()).unwrap());
    assert_eq!(attrs.len(), 1);

    let attribute = &attrs[0];
    assert_eq!(
        attribute.constructor_full_name().unwrap(),
        "System.Void FooAttribute::.ctor(System.String)"
    );
    let values = attribute.values().unwrap();
    assert_eq!(values.constructor_args.len(), 1);
    assert_argument(
        "System.String",
        &CaValue::String("bar".into()),
        &values.constructor_args[0],
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:38 (NullString)
fn null_string() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Dentist", "Foo");
    let values = attribute.values().unwrap();
    assert_argument("System.String", &CaValue::Null, &values.constructor_args[0]);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:51 (Primitives1)
fn primitives1() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Steven", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_argument("System.SByte", &CaValue::I1(-12), &args[0]);
    assert_argument("System.Byte", &CaValue::U1(242), &args[1]);
    assert_argument("System.Boolean", &CaValue::Boolean(true), &args[2]);
    assert_argument("System.Boolean", &CaValue::Boolean(false), &args[3]);
    assert_argument("System.UInt16", &CaValue::U2(4242), &args[4]);
    assert_argument("System.Int16", &CaValue::I2(-1983), &args[5]);
    assert_argument("System.Char", &CaValue::Char('c' as u16), &args[6]);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:69 (Primitives2)
fn primitives2() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Seagull", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_argument("System.Int32", &CaValue::I4(-100000), &args[0]);
    assert_argument("System.UInt32", &CaValue::U4(200000), &args[1]);
    assert_argument("System.Single", &CaValue::R4(12.12), &args[2]);
    assert_argument("System.Int64", &CaValue::I8(i64::MAX), &args[3]);
    assert_argument("System.UInt64", &CaValue::U8(u64::MAX), &args[4]);
    assert_argument("System.Double", &CaValue::R8(64.646464), &args[5]);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:87 (StringArgumentOnAssembly)
fn string_argument_on_assembly() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = get_attribute(&module, module.assembly_token(), "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_argument("System.String", &CaValue::String("bingo".into()), &args[0]);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:100 (CharArray)
fn char_array() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Rifle", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    let argument = &args[0];
    assert_eq!(argument.ty.full_name(), "System.Char[]");

    let CaValue::Array(array) = &argument.value else {
        panic!("expected array")
    };
    let expected = "cecil";
    assert_eq!(array.len(), expected.len());
    for (item, c) in array.iter().zip(expected.chars()) {
        assert_argument("System.Char", &CaValue::Char(c as u16), item);
    }
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:125 (BoxedArguments)
fn boxed_arguments() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Worm", "Foo");
    assert_eq!(
        pretty_print(&attribute),
        ".ctor ((Object:(String:\"2\")), (Object:(I4:2)))"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:139 (BoxedArraysArguments)
fn boxed_arrays_arguments() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Sheep", "Foo");
    assert_eq!(
        pretty_print(&attribute),
        ".ctor ((Object:(Object[]:{(Object:(String:\"2\")), (Object:(I4:2)), (Object:(Char:'c'))})), (Object:(Object[]:{(Object:(Object[]:{(Object:(I4:1)), (Object:(I4:2)), (Object:(I4:3))})), (Object:(String:null))})))"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:153 (FieldsAndProperties)
fn fields_and_properties() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Angola", "Foo");
    let values = attribute.values().unwrap();

    assert_eq!(values.fields.len(), 2);
    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "Pan")
        .unwrap()
        .argument;
    let mut s = String::new();
    pp_argument(argument, &mut s);
    assert_eq!(
        s,
        "(Object:(Object[]:{(Object:(I4:1)), (Object:(String:\"2\")), (Object:(Char:'3'))}))"
    );

    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "PanPan")
        .unwrap()
        .argument;
    let mut s = String::new();
    pp_argument(argument, &mut s);
    assert_eq!(s, "(String[]:{(String:\"yo\"), (String:\"yo\")})");

    assert_eq!(values.properties.len(), 2);
    let argument = &values
        .properties
        .iter()
        .find(|p| p.name == "Bang")
        .unwrap()
        .argument;
    assert_argument("System.Int32", &CaValue::I4(42), argument);

    let argument = &values
        .properties
        .iter()
        .find(|p| p.name == "Fiou")
        .unwrap()
        .argument;
    assert_argument("System.String", &CaValue::Null, argument);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:182 (BoxedStringField)
fn boxed_string_field() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "BoxedStringField", "Foo");
    let values = attribute.values().unwrap();
    assert_eq!(values.fields.len(), 1);
    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "Pan")
        .unwrap()
        .argument;
    let mut s = String::new();
    pp_argument(argument, &mut s);
    assert_eq!(s, "(Object:(String:\"fiouuu\"))");
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:196 (TypeDefinitionEnum)
fn type_definition_enum() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Zero", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].value, CaValue::I2(2));
    assert_eq!(args[0].ty.full_name(), "Bingo");
}

/// Serves the mdb corpus's mscorlib.dll, which defines
/// System.Security.AccessControl.AceFlags, for every assembly.
struct Mscorlib;

impl AssemblyLoader for Mscorlib {
    fn load_assembly(&self, _: &AssemblyName<'_>) -> Option<Vec<u8>> {
        std::fs::read(cecil_root().join("symbols/mdb/Test/Resources/assemblies/mscorlib.dll")).ok()
    }

    fn load_file(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:212 (TypeReferenceEnum) —
// adapted: the AceFlags enum lives in System.Security.AccessControl, which
// the Mscorlib loader stands in for.
fn type_reference_enum() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Ace", "Foo");

    // Without resolution the enum's underlying size is unknowable.
    assert!(attribute.values().is_err());

    let cache = AssemblyCache::new(&Mscorlib);
    let args = attribute.values_with(&cache).unwrap().constructor_args;
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].value, CaValue::U1(0x04));
    assert_eq!(
        args[0].ty.full_name(),
        "System.Security.AccessControl.AceFlags"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:229 (BoxedEnumReference)
fn boxed_enum_reference() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Bzzz", "Foo");

    let cache = AssemblyCache::new(&Mscorlib);
    let values = attribute.values_with(&cache).unwrap();
    assert_eq!(values.constructor_args.len(), 2);

    let mut s = String::new();
    pp_argument(&values.constructor_args[0], &mut s);
    assert_eq!(
        s,
        "(Object:(Object[]:{(Object:(Bingo:2)), (Object:(Bingo:4))}))"
    );

    let mut s = String::new();
    pp_argument(&values.constructor_args[1], &mut s);
    assert_eq!(s, "(Object:(String:null))");

    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "Pan")
        .unwrap()
        .argument;
    let mut s = String::new();
    pp_argument(argument, &mut s);
    assert_eq!(s, "(Object:(System.Security.AccessControl.AceFlags:4))");
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:256 (TypeOfTypeDefinition)
fn typeof_type_definition() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Typed", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].ty.full_name(), "System.Type");
    assert_eq!(args[0].value, CaValue::TypeName("Bingo".into()));
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:278 (TypeOfNestedTypeDefinition)
fn typeof_nested_type_definition() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "NestedTyped", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(
        args[0].value,
        CaValue::TypeName("FooAttribute/Token".into())
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:300 (FieldTypeOf)
fn field_typeof() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Truc", "Foo");
    let values = attribute.values().unwrap();
    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "Chose")
        .unwrap()
        .argument;
    assert_eq!(argument.ty.full_name(), "System.Type");
    assert_eq!(argument.value, CaValue::TypeName("Typed".into()));
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:320 (EscapedTypeName)
fn escaped_type_name() {
    let data = read_resource("bug-185.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let foo_do = foo
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| !m.is_constructor().unwrap())
        .unwrap();

    let attribute = module
        .custom_attributes_of(foo_do.token())
        .unwrap()
        .into_iter()
        .find(|a| a.attribute_type_name().unwrap() == "AsyncStateMachineAttribute")
        .unwrap();
    let nested = foo.nested_types().unwrap()[0].full_name().unwrap();
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args[0].value, CaValue::TypeName(nested));

    let function = module.get_type("Function`1").unwrap().unwrap();
    let apply = function
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| !m.is_constructor().unwrap())
        .unwrap();
    let attribute = module
        .custom_attributes_of(apply.token())
        .unwrap()
        .into_iter()
        .find(|a| a.attribute_type_name().unwrap() == "AsyncStateMachineAttribute")
        .unwrap();
    let nested = function.nested_types().unwrap()[0].full_name().unwrap();
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args[0].value, CaValue::TypeName(nested));
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:341 (FieldNullTypeOf)
fn field_null_typeof() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Machin", "Foo");
    let values = attribute.values().unwrap();
    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "Chose")
        .unwrap()
        .argument;
    assert_eq!(argument.ty.full_name(), "System.Type");
    assert_eq!(argument.value, CaValue::Null);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:358 (OpenGenericTypeOf)
fn open_generic_typeof() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "OpenGeneric`2", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args[0].ty.full_name(), "System.Type");
    assert_eq!(
        args[0].value,
        CaValue::TypeName("System.Collections.Generic.Dictionary`2".into())
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:381 (ClosedGenericTypeOf)
fn closed_generic_typeof() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "ClosedGeneric", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(
        args[0].value,
        CaValue::TypeName(
            "System.Collections.Generic.Dictionary`2<System.String,OpenGeneric`2<Machin,System.Int32>[,]>"
                .into()
        )
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:404 (TypeOfArrayOfNestedClass)
fn typeof_array_of_nested_class() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let attribute = type_attr(&module, "Parent", "Foo");
    let args = attribute.values().unwrap().constructor_args;
    assert_eq!(args[0].value, CaValue::TypeName("Parent/Child[]".into()));
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:427 (EmptyBlob)
fn empty_blob() {
    let data = read_fixture("ca-empty-blob.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("CustomAttribute").unwrap().unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].values().unwrap().constructor_args.len(), 0);
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:437 (InterfaceImplementation)
// — not Windows-only here: CoreCLR ilasm supports .interfaceimpl.
fn interface_implementation() {
    let data = read_fixture("ca-iface-impl.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("FooType").unwrap().unwrap();
    let iface = ty
        .interfaces()
        .unwrap()
        .into_iter()
        .find(|i| i.interface.unwrap().full_name().unwrap() == "IFoo")
        .unwrap();
    let attrs = module.custom_attributes_of(iface.token).unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].attribute_type_full_name().unwrap(), "FooAttribute");
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:452 (GenericParameterConstraint)
fn generic_parameter_constraint_attribute() {
    let data = read_resource("GenericParameterConstraintAttributes.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo.Library`1").unwrap().unwrap();
    let gp = &ty.generic_params().unwrap()[0];
    let constraint = &gp.constraints().unwrap()[0];

    let attrs = module.custom_attributes_of(constraint.token).unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(
        attrs[0].attribute_type_full_name().unwrap(),
        "System.Runtime.CompilerServices.NullableAttribute"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:467 (GenericAttributeString)
fn generic_attribute_string() {
    let data = read_resource("GenericAttributes.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("WithGenericAttribute_OfString")
        .unwrap()
        .unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    assert_eq!(attrs.len(), 1);
    let attribute = &attrs[0];
    assert_eq!(
        attribute.attribute_type_full_name().unwrap(),
        "GenericAttribute`1<System.String>"
    );
    let values = attribute.values().unwrap();
    assert_argument(
        "System.String",
        &CaValue::String("t".into()),
        &values.constructor_args[0],
    );
    assert_eq!(values.fields.len(), 1);
    assert_eq!(values.fields[0].name, "F");
    assert_argument(
        "System.String",
        &CaValue::String("f".into()),
        &values.fields[0].argument,
    );
    assert_eq!(values.properties.len(), 1);
    assert_eq!(values.properties[0].name, "P");
    assert_argument(
        "System.String",
        &CaValue::String("p".into()),
        &values.properties[0].argument,
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:498 (GenericAttributeInt)
fn generic_attribute_int() {
    let data = read_resource("GenericAttributes.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("WithGenericAttribute_OfInt")
        .unwrap()
        .unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    let attribute = &attrs[0];
    assert_eq!(
        attribute.attribute_type_full_name().unwrap(),
        "GenericAttribute`1<System.Int32>"
    );
    let values = attribute.values().unwrap();
    assert_argument("System.Int32", &CaValue::I4(1), &values.constructor_args[0]);
    assert_argument("System.Int32", &CaValue::I4(2), &values.fields[0].argument);
    assert_argument(
        "System.Int32",
        &CaValue::I4(3),
        &values.properties[0].argument,
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:528 (ConstrainedGenericAttribute)
fn constrained_generic_attribute() {
    let data = read_resource("GenericAttributes.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("WithConstrainedGenericAttribute")
        .unwrap()
        .unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(
        attrs[0].attribute_type_full_name().unwrap(),
        "ConstrainedGenericAttribute`1<DerivedFromConstraintType>"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:541 (NullCharInString)
fn null_char_in_string() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("NullCharInString").unwrap().unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    assert_eq!(attrs.len(), 1);
    let args = attrs[0].values().unwrap().constructor_args;
    let CaValue::String(value) = &args[0].value else {
        panic!("expected string")
    };
    assert_eq!(value.chars().count(), 8);
    assert_eq!(value.chars().nth(7), Some('\0'));
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:556 (OrderedAttributes)
fn ordered_attributes() {
    let data = read_resource("ordered-attrs.exe");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Program").unwrap().unwrap();
    let method = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "Main")
        .unwrap();
    let attrs = module.custom_attributes_of(method.token()).unwrap();
    assert_eq!(attrs.len(), 6);

    let expectations = [
        ("AAttribute", "Main.A1"),
        ("AAttribute", "Main.A2"),
        ("BAttribute", "Main.B1"),
        ("AAttribute", "Main.A3"),
        ("BAttribute", "Main.B2"),
        ("BAttribute", "Main.B3"),
    ];
    for (attr, (name, field_value)) in attrs.iter().zip(expectations) {
        assert_eq!(attr.attribute_type_name().unwrap(), name);
        let values = attr.values().unwrap();
        assert_eq!(
            values.fields[0].argument.value,
            CaValue::String(field_value.into())
        );
    }
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:624 (BoxedEnumOnGenericArgumentOnType)
fn boxed_enum_on_generic_argument_on_type() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("BoxedValueEnumOnGenericType")
        .unwrap()
        .unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    assert_eq!(attrs.len(), 1);
    let attribute = &attrs[0];
    assert_eq!(
        attribute.constructor_full_name().unwrap(),
        "System.Void FooAttribute::.ctor(System.Object,System.Object)"
    );
    let values = attribute.values().unwrap();
    assert_eq!(values.constructor_args.len(), 2);
    let mut s = String::new();
    pp_argument(&values.constructor_args[0], &mut s);
    assert_eq!(
        s,
        "(Object:(GenericWithEnum`1/OnGenericNumber<System.Int32>:0))"
    );
    let mut s = String::new();
    pp_argument(&values.constructor_args[1], &mut s);
    assert_eq!(
        s,
        "(Object:(GenericWithEnum`1/OnGenericNumber<System.String>:1))"
    );
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:645 (EnumOnGenericArgumentOnType)
fn enum_on_generic_argument_on_type() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("ValueEnumOnGenericType").unwrap().unwrap();
    let attrs = module.custom_attributes_of(ty.token()).unwrap();
    let attribute = &attrs[0];
    assert_eq!(
        attribute.constructor_full_name().unwrap(),
        "System.Void FooAttribute::.ctor(GenericWithEnum`1/OnGenericNumber<Bingo>)"
    );
    let values = attribute.values().unwrap();
    let mut s = String::new();
    pp_argument(&values.constructor_args[0], &mut s);
    assert_eq!(s, "(GenericWithEnum`1/OnGenericNumber<Bingo>:1)");
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:665 (EnumOnGenericFieldOnType)
fn enum_on_generic_field_on_type() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("FieldEnumOnGenericType").unwrap().unwrap();
    let attribute = &module.custom_attributes_of(ty.token()).unwrap()[0];
    let values = attribute.values().unwrap();
    let argument = &values
        .fields
        .iter()
        .find(|f| f.name == "NumberEnumField")
        .unwrap()
        .argument;
    let mut s = String::new();
    pp_argument(argument, &mut s);
    assert_eq!(s, "(GenericWithEnum`1/OnGenericNumber<System.Byte>:0)");
}

// Test/Mono.Cecil.Tests/CustomAttributesTests.cs:697 (EnumDeclaredInGenericTypeArray)
fn enum_declared_in_generic_type_array() {
    let data = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("WithAttributeUsingNestedEnumArray")
        .unwrap()
        .unwrap();
    let attribute = &module.custom_attributes_of(ty.token()).unwrap()[0];
    let values = attribute.values().unwrap();
    assert_eq!(values.fields.len(), 1);
    let arg = &values.fields[0].argument;
    assert_eq!(arg.ty.full_name(), "System.Object");

    let CaValue::Boxed(inner) = &arg.value else {
        panic!("expected boxed value")
    };
    assert_eq!(
        inner.ty.full_name(),
        "GenericWithEnum`1/OnGenericNumber<System.String>[]"
    );
    let CaValue::Array(items) = &inner.value else {
        panic!("expected array value")
    };
    assert_eq!(items[0].value, CaValue::I4(0));
    assert_eq!(items[1].value, CaValue::I4(1));
}

pub(super) const TESTS: &[Test] = &[
    ("string_argument_on_type", string_argument_on_type),
    ("null_string", null_string),
    ("primitives1", primitives1),
    ("primitives2", primitives2),
    ("string_argument_on_assembly", string_argument_on_assembly),
    ("char_array", char_array),
    ("boxed_arguments", boxed_arguments),
    ("boxed_arrays_arguments", boxed_arrays_arguments),
    ("fields_and_properties", fields_and_properties),
    ("boxed_string_field", boxed_string_field),
    ("type_definition_enum", type_definition_enum),
    ("type_reference_enum", type_reference_enum),
    ("boxed_enum_reference", boxed_enum_reference),
    ("typeof_type_definition", typeof_type_definition),
    (
        "typeof_nested_type_definition",
        typeof_nested_type_definition,
    ),
    ("field_typeof", field_typeof),
    ("escaped_type_name", escaped_type_name),
    ("field_null_typeof", field_null_typeof),
    ("open_generic_typeof", open_generic_typeof),
    ("closed_generic_typeof", closed_generic_typeof),
    ("typeof_array_of_nested_class", typeof_array_of_nested_class),
    ("empty_blob", empty_blob),
    ("interface_implementation", interface_implementation),
    (
        "generic_parameter_constraint_attribute",
        generic_parameter_constraint_attribute,
    ),
    ("generic_attribute_string", generic_attribute_string),
    ("generic_attribute_int", generic_attribute_int),
    (
        "constrained_generic_attribute",
        constrained_generic_attribute,
    ),
    ("null_char_in_string", null_char_in_string),
    ("ordered_attributes", ordered_attributes),
    (
        "boxed_enum_on_generic_argument_on_type",
        boxed_enum_on_generic_argument_on_type,
    ),
    (
        "enum_on_generic_argument_on_type",
        enum_on_generic_argument_on_type,
    ),
    (
        "enum_on_generic_field_on_type",
        enum_on_generic_field_on_type,
    ),
    (
        "enum_declared_in_generic_type_array",
        enum_declared_in_generic_type_array,
    ),
];
