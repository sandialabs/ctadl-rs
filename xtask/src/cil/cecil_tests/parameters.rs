//! Ported from Test/Mono.Cecil.Tests/ParameterTests.cs (read subset).
//! Skipped: AddParameterIndex/RemoveAtParameterIndex/RemoveParameterIndex/
//! InsertParameterIndex (mutation), plus write-side variants.

use super::{Test, read_fixture, read_resource};
use cil_reader::Module;
use cil_reader::model::marshal::{MarshalInfo, native_type, variant_type};
use cil_reader::model::{ConstantValue, MethodDef, TypeDef};
use cil_reader::raw::Guid;

fn get_method<'a>(ty: &TypeDef<'a>, name: &str) -> MethodDef<'a> {
    ty.methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == name)
        .unwrap_or_else(|| panic!("no method {name}"))
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:11 (MarshalAsI4)
fn marshal_as_i4() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar").unwrap().unwrap();
    let pan = get_method(&bar, "Pan");
    let params = pan.parameters().unwrap();
    assert_eq!(params.len(), 1);
    let parameter = &params[0];
    assert!(parameter.has_marshal_info().unwrap());
    match parameter.marshal_info().unwrap().unwrap() {
        MarshalInfo::Simple { native } => assert_eq!(native, native_type::I4),
        other => panic!("expected plain marshal info, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:30 (CustomMarshaler)
fn custom_marshaler() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar").unwrap().unwrap();
    let method = get_method(&bar, "PanPan");
    let parameter = &method.parameters().unwrap()[0];
    assert!(parameter.has_marshal_info().unwrap());
    match parameter.marshal_info().unwrap().unwrap() {
        MarshalInfo::CustomMarshaler {
            guid,
            unmanaged_type,
            managed_type,
            cookie,
        } => {
            assert_eq!(guid, Guid::default());
            assert_eq!(unmanaged_type, "");
            assert_eq!(managed_type, "Boc");
            assert_eq!(cookie, "nomnom");
        }
        other => panic!("expected custom marshaler, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:53 (SafeArrayMarshaler)
fn safe_array_marshaler() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar").unwrap().unwrap();
    let pan = get_method(&bar, "PanPan");
    match pan.return_param_marshal_info().unwrap().unwrap() {
        MarshalInfo::SafeArray { element_type } => {
            assert_eq!(element_type, variant_type::DISPATCH)
        }
        other => panic!("expected safe array, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:68 (ArrayMarshaler)
fn array_marshaler() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar").unwrap().unwrap();
    let method = get_method(&bar, "PanPan");
    let params = method.parameters().unwrap();

    let parameter = &params[1];
    assert!(parameter.has_marshal_info().unwrap());
    match parameter.marshal_info().unwrap().unwrap() {
        MarshalInfo::Array {
            element_type,
            size,
            size_parameter_index,
            ..
        } => {
            assert_eq!(element_type, native_type::I8);
            assert_eq!(size, 66);
            assert_eq!(size_parameter_index, 2);
        }
        other => panic!("expected array marshal info, got {other:?}"),
    }

    let parameter = &params[3];
    assert!(parameter.has_marshal_info().unwrap());
    match parameter.marshal_info().unwrap().unwrap() {
        MarshalInfo::Array {
            element_type,
            size,
            size_parameter_index,
            ..
        } => {
            assert_eq!(element_type, native_type::I2);
            assert_eq!(size, -1);
            assert_eq!(size_parameter_index, -1);
        }
        other => panic!("expected array marshal info, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:97 (ArrayMarshalerSized)
fn array_marshaler_sized() {
    let data = read_resource("marshal.dll");
    let module = Module::read(&data).unwrap();
    let delegate_type = module.get_type("SomeMethod").unwrap().unwrap();
    let method = get_method(&delegate_type, "Invoke");
    let parameter = &method.parameters().unwrap()[1];
    assert!(parameter.has_marshal_info().unwrap());
    match parameter.marshal_info().unwrap().unwrap() {
        MarshalInfo::Array {
            size_parameter_multiplier,
            ..
        } => assert_eq!(size_parameter_multiplier, 0),
        other => panic!("expected array marshal info, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:113 (NullableConstant)
fn nullable_constant() {
    let data = read_resource("nullable-constant.exe");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Program").unwrap().unwrap();
    let method = get_method(&ty, "Foo");
    let params = method.parameters().unwrap();
    assert!(params[0].has_constant().unwrap());
    assert!(params[1].has_constant().unwrap());
    assert!(params[2].has_constant().unwrap());

    assert_eq!(params[0].constant().unwrap(), Some(ConstantValue::I4(1234)));
    assert_eq!(params[1].constant().unwrap(), Some(ConstantValue::Null));
    assert_eq!(params[2].constant().unwrap(), Some(ConstantValue::I4(12)));
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:130 (BoxedDefaultArgumentValue)
fn boxed_default_argument_value() {
    let data = read_resource("boxedoptarg.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let method = get_method(&foo, "Bar");
    let baz = &method.parameters().unwrap()[0];
    assert!(baz.has_constant().unwrap());
    assert_eq!(baz.constant().unwrap(), Some(ConstantValue::I4(-1)));
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:236 (GenericParameterConstant)
fn generic_parameter_constant() {
    let data = read_fixture("hello.il.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let method = get_method(&foo, "GetState");
    let parameter = &method.parameters().unwrap()[1];
    assert!(parameter.has_constant().unwrap());
    assert_eq!(parameter.constant().unwrap(), Some(ConstantValue::Null));
}

// Test/Mono.Cecil.Tests/ParameterTests.cs:252 (NullablePrimitiveParameterConstant)
fn nullable_primitive_parameter_constant() {
    let data = read_resource("nullable-parameter.dll");
    let module = Module::read(&data).unwrap();
    let test = module.get_type("Test").unwrap().unwrap();
    let method = get_method(&test, "Foo");
    let params = method.parameters().unwrap();
    assert!(params[0].has_constant().unwrap());
    assert_eq!(params[0].constant().unwrap(), Some(ConstantValue::I4(1234)));
    assert!(params[1].has_constant().unwrap());
    assert_eq!(params[1].constant().unwrap(), Some(ConstantValue::Null));
    assert!(params[2].has_constant().unwrap());
    assert_eq!(params[2].constant().unwrap(), Some(ConstantValue::I4(12)));
}

pub(super) const TESTS: &[Test] = &[
    ("marshal_as_i4", marshal_as_i4),
    ("custom_marshaler", custom_marshaler),
    ("safe_array_marshaler", safe_array_marshaler),
    ("array_marshaler", array_marshaler),
    ("array_marshaler_sized", array_marshaler_sized),
    ("nullable_constant", nullable_constant),
    ("boxed_default_argument_value", boxed_default_argument_value),
    ("generic_parameter_constant", generic_parameter_constant),
    (
        "nullable_primitive_parameter_constant",
        nullable_primitive_parameter_constant,
    ),
];
