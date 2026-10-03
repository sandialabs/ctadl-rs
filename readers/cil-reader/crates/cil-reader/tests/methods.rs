//! Ported from Test/Mono.Cecil.Tests/MethodTests.cs (read subset).
//!
//! Deferred to M4 (need method bodies): the body-reading halves of VarArgs,
//! GenericInstanceMethod, MethodRefDeclaredOnGenerics — see tests/method_body.rs.
//! Skipped: ReturnParameterMethod (reflection round-trip),
//! InstanceAndStaticMethodComparison / FunctionPointerArgumentOverload /
//! PrivateScope* (need MetadataResolver, M7).

mod testutil;

use cil_reader::Module;
use cil_reader::model::{MethodDef, TypeDef, method_attributes, sig::calling_convention};
use testutil::{read_fixture, read_resource};

fn get_method<'a>(ty: &TypeDef<'a>, name: &str) -> MethodDef<'a> {
    ty.methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == name)
        .unwrap_or_else(|| panic!("no method {name}"))
}

// Test/Mono.Cecil.Tests/MethodTests.cs:15 (AbstractMethod)
#[test]
fn abstract_method() {
    let data = read_fixture!("Methods.cs.dll");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    let ty = &types[1];
    assert_eq!(ty.name().unwrap(), "Foo");
    assert_eq!(ty.methods().unwrap().len(), 2);

    let method = get_method(ty, "Bar");
    assert_ne!(
        method.attributes().unwrap() & method_attributes::ABSTRACT,
        0
    );

    let params = method.parameters().unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "a");
    assert_eq!(
        module
            .type_sig_full_name(&params[0].param_type, Default::default())
            .unwrap(),
        "System.Int32"
    );
}

// Test/Mono.Cecil.Tests/MethodTests.cs:37 (SimplePInvoke)
#[test]
fn simple_pinvoke() {
    let data = read_fixture!("Methods.cs.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar").unwrap().unwrap();
    let pan = get_method(&bar, "Pan");

    assert_ne!(
        pan.attributes().unwrap() & method_attributes::PINVOKE_IMPL,
        0
    );
    let pinvoke = pan.pinvoke_info().unwrap().expect("pinvoke info");
    assert_eq!(pinvoke.entry_point, "Pan");
    assert_eq!(pinvoke.module, "foo.dll");
}

// Test/Mono.Cecil.Tests/MethodTests.cs:53 (GenericMethodDefinition)
#[test]
fn generic_method_definition() {
    let data = read_fixture!("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let baz = module.get_type("Baz").unwrap().unwrap();
    let gazonk = get_method(&baz, "Gazonk");
    assert!(gazonk.has_generic_params().unwrap());
    let params = gazonk.generic_params().unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name().unwrap(), "TBang");
}

// Test/Mono.Cecil.Tests/MethodTests.cs:69 (ReturnGenericInstance)
#[test]
fn return_generic_instance() {
    use cil_reader::model::TypeSig;
    let data = read_fixture!("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let bar = module.get_type("Bar`1").unwrap().unwrap();

    let self_method = get_method(&bar, "Self");
    let TypeSig::GenericInst { args, .. } = self_method.return_type().unwrap() else {
        panic!("expected generic instance return")
    };
    assert_eq!(args[0], TypeSig::Var(0));
    assert_eq!(self_method.full_name().unwrap(), "Bar`1<T> Bar`1::Self()");

    let self_str = get_method(&bar, "SelfString");
    let TypeSig::GenericInst { args, .. } = self_str.return_type().unwrap() else {
        panic!("expected generic instance return")
    };
    assert_eq!(args[0], TypeSig::String);
    assert_eq!(
        self_str.full_name().unwrap(),
        "Bar`1<System.String> Bar`1::SelfString()"
    );
}

// Test/Mono.Cecil.Tests/MethodTests.cs:98 (ReturnGenericInstanceWithMethodParameter)
#[test]
fn return_generic_instance_with_method_parameter() {
    use cil_reader::model::TypeSig;
    let data = read_fixture!("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let baz = module.get_type("Baz").unwrap().unwrap();
    let gazoo = get_method(&baz, "Gazoo");
    let TypeSig::GenericInst { args, .. } = gazoo.return_type().unwrap() else {
        panic!("expected generic instance return")
    };
    // Cecil: the instance argument is gazoo's own generic parameter.
    assert_eq!(args[0], TypeSig::MVar(0));
    let name = gazoo.generic_params().unwrap()[0]
        .name()
        .unwrap()
        .to_string();
    assert!(gazoo.full_name().unwrap().contains(&format!("<{name}>")));
}

// Test/Mono.Cecil.Tests/MethodTests.cs:117 (SimpleOverrides)
#[test]
fn simple_overrides() {
    let data = read_fixture!("Interfaces.cs.dll");
    let module = Module::read(&data).unwrap();
    let ibingo = module.get_type("IBingo").unwrap().unwrap();
    let ibingo_foo = get_method(&ibingo, "Foo");
    let ibingo_bar = get_method(&ibingo, "Bar");

    let bingo = module.get_type("Bingo").unwrap().unwrap();

    let foo = get_method(&bingo, "IBingo.Foo");
    assert!(foo.has_overrides().unwrap());
    assert_eq!(foo.overrides().unwrap()[0], ibingo_foo.token());

    let bar = get_method(&bingo, "IBingo.Bar");
    assert!(bar.has_overrides().unwrap());
    assert_eq!(bar.overrides().unwrap()[0], ibingo_bar.token());
}

// Test/Mono.Cecil.Tests/MethodTests.cs:144 (VarArgs) — signature half; the
// call-site half needs bodies (M4).
#[test]
fn varargs() {
    let data = read_resource!("varargs.exe");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    let module_type = &types[0];
    assert_eq!(module_type.methods().unwrap().len(), 3);

    let is_vararg = |m: &MethodDef<'_>| {
        m.signature().unwrap().calling_convention == calling_convention::VAR_ARG
    };
    let bar = get_method(module_type, "Bar");
    let baz = get_method(module_type, "Baz");
    let foo = get_method(module_type, "Foo");
    assert!(is_vararg(&bar));
    assert!(!is_vararg(&baz));
    assert!(is_vararg(&foo));
}
