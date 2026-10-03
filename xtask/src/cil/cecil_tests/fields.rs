//! Ported from Test/Mono.Cecil.Tests/FieldTests.cs (read subset).
//!
//! Skipped: FieldRVAAlignment (round-trips through Write), UnattachedField
//! (constructs definitions).

use super::{read_fixture, read_resource, Test};
use cil_reader::model::marshal::{native_type, MarshalInfo};
use cil_reader::model::{ConstantValue, FieldDef, TypeDef, TypeSig};
use cil_reader::Module;
use cil_reader::TokenType;

fn get_field<'a>(ty: &TypeDef<'a>, name: &str) -> FieldDef<'a> {
    ty.fields()
        .unwrap()
        .into_iter()
        .find(|f| f.name().unwrap() == name)
        .unwrap_or_else(|| panic!("no field {name}"))
}

// Test/Mono.Cecil.Tests/FieldTests.cs:13 (TypeDefField)
fn type_def_field() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    let ty = &types[1];
    assert_eq!(ty.name().unwrap(), "Foo");
    let fields = ty.fields().unwrap();
    assert_eq!(fields.len(), 1);

    let field = &fields[0];
    assert_eq!(field.name().unwrap(), "bar");
    assert_eq!(field.token().rid(), 1);
    assert_eq!(field.token().token_type(), Some(TokenType::Field));
    let out = module
        .type_sig_full_name(&field.field_type().unwrap(), Default::default())
        .unwrap();
    assert_eq!(out, "Bar");
    assert!(!field.has_constant().unwrap());
}

// Test/Mono.Cecil.Tests/FieldTests.cs:32 (PrimitiveTypes)
fn primitive_types() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Baz").unwrap().unwrap();
    for (name, expected) in [
        ("char", "System.Char"),
        ("bool", "System.Boolean"),
        ("sbyte", "System.SByte"),
        ("byte", "System.Byte"),
        ("int16", "System.Int16"),
        ("uint16", "System.UInt16"),
        ("int32", "System.Int32"),
        ("uint32", "System.UInt32"),
        ("int64", "System.Int64"),
        ("uint64", "System.UInt64"),
        ("single", "System.Single"),
        ("double", "System.Double"),
        ("string", "System.String"),
        ("object", "System.Object"),
    ] {
        let field = get_field(&ty, name);
        let out = module
            .type_sig_full_name(&field.field_type().unwrap(), Default::default())
            .unwrap();
        assert_eq!(out, expected, "{name}");
    }
}

// Test/Mono.Cecil.Tests/FieldTests.cs:55 (VolatileField)
fn volatile_field() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Bar").unwrap().unwrap();
    let fields = ty.fields().unwrap();
    assert_eq!(fields.len(), 1);
    let field = &fields[0];
    assert_eq!(field.name().unwrap(), "oiseau");
    let out = module
        .type_sig_full_name(&field.field_type().unwrap(), Default::default())
        .unwrap();
    assert_eq!(
        out,
        "System.Int32 modreq(System.Runtime.CompilerServices.IsVolatile)"
    );
    assert!(!field.has_constant().unwrap());
}

// Test/Mono.Cecil.Tests/FieldTests.cs:73 (FieldLayout)
fn field_layout() {
    let data = read_fixture("Layouts.cs.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let fields = foo.fields().unwrap();

    assert_eq!(fields[0].name().unwrap(), "Bar");
    assert_eq!(fields[0].offset().unwrap(), Some(0));
    assert_eq!(fields[1].name().unwrap(), "Baz");
    assert_eq!(fields[1].offset().unwrap(), Some(2));
    assert_eq!(fields[2].name().unwrap(), "Gazonk");
    assert_eq!(fields[2].offset().unwrap(), Some(4));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:104 (FieldRVA)
fn field_rva() {
    let data = read_fixture("Layouts.cs.dll");
    let module = Module::read(&data).unwrap();
    let priv_impl = module
        .types()
        .unwrap()
        .into_iter()
        .find(|t| {
            t.full_name()
                .unwrap()
                .contains("<PrivateImplementationDetails>")
        })
        .expect("<PrivateImplementationDetails>");

    let fields = priv_impl.fields().unwrap();
    assert_eq!(fields.len(), 1);
    let field = &fields[0];
    assert_ne!(field.rva().unwrap(), Some(0));
    let value = field.initial_value().unwrap().expect("initial value");
    assert_eq!(value.len(), 16);
    for (i, chunk) in value.chunks_exact(4).enumerate() {
        assert_eq!(u32::from_le_bytes(chunk.try_into().unwrap()), i as u32 + 1);
    }
}

// Test/Mono.Cecil.Tests/FieldTests.cs:181 (GenericFieldDefinition)
fn generic_field_definition() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar`1").unwrap().unwrap();
    assert!(bar.has_generic_params().unwrap());
    let t = &bar.generic_params().unwrap()[0];
    assert_eq!(t.name().unwrap(), "T");
    assert_eq!(t.owner().unwrap(), bar.token());

    let bang = get_field(&bar, "bang");
    assert_eq!(bang.field_type().unwrap(), TypeSig::Var(0));
    // Rendered against the declaring type's context, that Var is "T".
    assert!(bang.full_name().unwrap().starts_with("T "));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:202 (ArrayFields)
fn array_fields() {
    let data = read_fixture("types.il.dll");
    let module = Module::read(&data).unwrap();
    let types = module.get_type("Types").unwrap().unwrap();

    let TypeSig::Array { dimensions, .. } = get_field(&types, "rank_two").field_type().unwrap()
    else {
        panic!("expected array")
    };
    assert_eq!(dimensions.len(), 2);
    assert!(dimensions[0].lower.is_none() && dimensions[0].upper.is_none());
    assert!(dimensions[1].lower.is_none() && dimensions[1].upper.is_none());

    let TypeSig::Array { dimensions, .. } = get_field(&types, "rank_two_low_bound_zero")
        .field_type()
        .unwrap()
    else {
        panic!("expected array")
    };
    assert_eq!(dimensions.len(), 2);
    assert_eq!(dimensions[0].lower, Some(0));
    assert_eq!(dimensions[0].upper, None);
    assert_eq!(dimensions[1].lower, Some(0));
    assert_eq!(dimensions[1].upper, None);

    let TypeSig::Array { dimensions, .. } = get_field(&types, "rank_one_low_bound_m1")
        .field_type()
        .unwrap()
    else {
        panic!("expected array")
    };
    assert_eq!(dimensions.len(), 1);
    assert_eq!(dimensions[0].lower, Some(-1));
    assert_eq!(dimensions[0].upper, Some(4));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:242 (EnumFieldsConstant)
fn enum_fields_constant() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let pim = module.get_type("Pim").unwrap().unwrap();
    let field = get_field(&pim, "Pam");
    assert!(field.has_constant().unwrap());
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I4(1)));
    let field = get_field(&pim, "Poum");
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I4(2)));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:258 (StringAndClassConstant)
fn string_and_class_constant() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let panpan = module.get_type("PanPan").unwrap().unwrap();

    let field = get_field(&panpan, "Peter");
    assert!(field.has_constant().unwrap());
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::Null));

    let field = get_field(&panpan, "QQ");
    match field.constant().unwrap() {
        Some(ConstantValue::String(s)) => assert_eq!(s.to_string_lossy(), "qq"),
        other => panic!("expected string constant, got {other:?}"),
    }

    let field = get_field(&panpan, "nil");
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::Null));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:277 (ObjectConstant)
fn object_constant() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let panpan = module.get_type("PanPan").unwrap().unwrap();
    let field = get_field(&panpan, "obj");
    assert!(field.has_constant().unwrap());
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::Null));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:290 (NullPrimitiveConstant)
fn null_primitive_constant() {
    let data = read_fixture("types.il.dll");
    let module = Module::read(&data).unwrap();
    let fields = module.get_type("Fields").unwrap().unwrap();
    let field = get_field(&fields, "int32_nullref");
    assert!(field.has_constant().unwrap());
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::Null));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:302 (ArrayConstant)
fn array_constant() {
    let data = read_fixture("Fields.cs.dll");
    let module = Module::read(&data).unwrap();
    let panpan = module.get_type("PanPan").unwrap().unwrap();
    let field = get_field(&panpan, "ints");
    assert!(field.has_constant().unwrap());
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::Null));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:315 (ConstantCoalescing)
fn constant_coalescing() {
    let data = read_fixture("types.il.dll");
    let module = Module::read(&data).unwrap();
    let fields = module.get_type("Fields").unwrap().unwrap();

    let field = get_field(&fields, "int32_int16");
    assert_eq!(field.field_type().unwrap(), TypeSig::I4);
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I2(1)));

    let field = get_field(&fields, "int16_int32");
    assert_eq!(field.field_type().unwrap(), TypeSig::I2);
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I4(1)));

    let field = get_field(&fields, "char_int16");
    assert_eq!(field.field_type().unwrap(), TypeSig::Char);
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I2(1)));

    let field = get_field(&fields, "int16_char");
    assert_eq!(field.field_type().unwrap(), TypeSig::I2);
    assert_eq!(
        field.constant().unwrap(),
        Some(ConstantValue::Char('s' as u16))
    );
}

// Test/Mono.Cecil.Tests/FieldTests.cs:347 (NestedEnumOfGenericTypeDefinition)
fn nested_enum_of_generic_type_definition() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let dang = module.get_type("Bongo`1/Dang").unwrap().unwrap();
    let field = get_field(&dang, "Ding");
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I4(2)));
    let field = get_field(&dang, "Dong");
    assert_eq!(field.constant().unwrap(), Some(ConstantValue::I4(12)));
}

// Test/Mono.Cecil.Tests/FieldTests.cs:364 (MarshalAsFixedStr)
fn marshal_as_fixed_str() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let boc = module.get_type("Boc").unwrap().unwrap();
    let field = get_field(&boc, "a");
    assert!(field.has_marshal_info().unwrap());
    match field.marshal_info().unwrap().unwrap() {
        MarshalInfo::FixedSysString { size } => assert_eq!(size, 42),
        other => panic!("expected FixedSysString, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/FieldTests.cs:381 (MarshalAsFixedArray)
fn marshal_as_fixed_array() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let boc = module.get_type("Boc").unwrap().unwrap();
    let field = get_field(&boc, "b");
    assert!(field.has_marshal_info().unwrap());
    match field.marshal_info().unwrap().unwrap() {
        MarshalInfo::FixedArray { size, element_type } => {
            assert_eq!(size, 12);
            assert_eq!(element_type, native_type::BOOLEAN);
        }
        other => panic!("expected FixedArray, got {other:?}"),
    }
}

// Not a Cecil port: parity bug found by bringing Mono.Android.dll into
// the Cecil comparison (PLAN.md §5.3). An ELEMENT_TYPE_STRING constant whose
// Value blob is empty is "", not null — blob index 0 is the heap's
// zero-length entry, not an absent blob (AssemblyReader.ReadConstantString
// returns string.Empty for count == 0; a null string is ELEMENT_TYPE_CLASS).
fn empty_string_constant() {
    let data = read_resource("Mono.Android.dll");
    let module = Module::read(&data).unwrap();
    let parser = module
        .get_type("Org.XmlPull.V1.XmlPullParser")
        .unwrap()
        .unwrap();
    let field = get_field(&parser, "NoNamespace");
    assert!(field.has_constant().unwrap());
    match field.constant().unwrap() {
        Some(ConstantValue::String(s)) => assert_eq!(s.to_string_lossy(), ""),
        other => panic!("expected an empty string constant, got {other:?}"),
    }
}

pub(super) const TESTS: &[Test] = &[
    ("type_def_field", type_def_field),
    ("primitive_types", primitive_types),
    ("volatile_field", volatile_field),
    ("field_layout", field_layout),
    ("field_rva", field_rva),
    ("generic_field_definition", generic_field_definition),
    ("array_fields", array_fields),
    ("enum_fields_constant", enum_fields_constant),
    ("string_and_class_constant", string_and_class_constant),
    ("object_constant", object_constant),
    ("null_primitive_constant", null_primitive_constant),
    ("array_constant", array_constant),
    ("constant_coalescing", constant_coalescing),
    (
        "nested_enum_of_generic_type_definition",
        nested_enum_of_generic_type_definition,
    ),
    ("marshal_as_fixed_str", marshal_as_fixed_str),
    ("marshal_as_fixed_array", marshal_as_fixed_array),
    ("empty_string_constant", empty_string_constant),
];
