//! Ported from Test/Mono.Cecil.Tests/TypeTests.cs (read subset).
//!
//! Deferred to M4 (need method bodies): TypeReferenceValueType,
//! GenericMultidimensionalArray — see tests/method_body.rs once it exists.
//! Skipped: CorlibPrimitive/DeferredCorlibTypeDef/CorlibTypesMetadataType
//! (read the running framework's corlib; corlib-sensitive tests are pinned
//! to the mdb mscorlib fixture instead where they matter).

use super::{read_fixture, read_resource, Test};
use cil_reader::model::{TypeDefOrRef, TypeSig};
use cil_reader::Module;

// Test/Mono.Cecil.Tests/TypeTests.cs:15 (TypeLayout)
fn type_layout() {
    let data = read_fixture("Layouts.cs.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    assert!(foo.is_value_type().unwrap());
    let (_, class_size) = foo.class_layout().unwrap().unwrap();
    assert_eq!(class_size, 16);

    let babar = module.get_type("Babar").unwrap().unwrap();
    assert!(!babar.is_value_type().unwrap());
    assert!(babar.class_layout().unwrap().is_none());
}

// Test/Mono.Cecil.Tests/TypeTests.cs:33 (SimpleInterfaces)
fn simple_interfaces() {
    let data = read_fixture("types.il.dll");
    let module = Module::read(&data).unwrap();
    let ibaz = module.get_type("IBaz").unwrap().unwrap();
    assert!(ibaz.has_interfaces().unwrap());
    let interfaces = ibaz.interfaces().unwrap();
    assert_eq!(interfaces.len(), 2);
    let names: Vec<String> = interfaces
        .iter()
        .map(|i| i.interface.full_name().unwrap())
        .collect();
    assert!(names.contains(&"IBar".to_string()));
    assert!(names.contains(&"IFoo".to_string()));
}

// Test/Mono.Cecil.Tests/TypeTests.cs:52 (GenericTypeDefinition)
fn generic_type_definition() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let foo = module.get_type("Foo`2").unwrap().unwrap();
    assert!(foo.has_generic_params().unwrap());
    let params = foo.generic_params().unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name().unwrap(), "TBar");
    assert_eq!(params[0].owner().unwrap(), foo.token());
    assert_eq!(params[1].name().unwrap(), "TBaz");
    assert_eq!(params[1].owner().unwrap(), foo.token());
}

// Test/Mono.Cecil.Tests/TypeTests.cs:74 (ConstrainedGenericType)
fn constrained_generic_type() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let bongo_t = module.get_type("Bongo`1").unwrap().unwrap();
    let params = bongo_t.generic_params().unwrap();
    let t = &params[0];
    assert_eq!(t.name().unwrap(), "T");
    assert!(t.has_constraints().unwrap());
    let constraints = t.constraints().unwrap();
    assert_eq!(constraints.len(), 2);
    assert_eq!(constraints[0].constraint.full_name().unwrap(), "Zap");
    assert_eq!(constraints[1].constraint.full_name().unwrap(), "IZoom");
}

// Test/Mono.Cecil.Tests/TypeTests.cs:93 (GenericBaseType)
fn generic_base_type() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let child = module.get_type("Child`1").unwrap().unwrap();
    let base = child.base_type().unwrap().unwrap();
    let TypeDefOrRef::Spec(spec) = base else {
        panic!("expected a generic instance base type")
    };
    assert_ne!(spec.token().rid(), 0);
    let TypeSig::GenericInst { args, .. } = spec.signature().unwrap() else {
        panic!("expected a generic instance signature")
    };
    // Cecil: instance.GenericArguments[0] == child's generic parameter T.
    assert_eq!(args[0], TypeSig::Var(0));
    assert_eq!(
        base.full_name_ctx(child.generic_context()).unwrap(),
        "Parent`1<T>"
    );
}

// Test/Mono.Cecil.Tests/TypeTests.cs:110 (GenericConstraintOnGenericParameter)
fn generic_constraint_on_generic_parameter() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let duel = module.get_type("Duel`3").unwrap().unwrap();
    let params = duel.generic_params().unwrap();
    assert_eq!(params.len(), 3);
    let ctx = duel.generic_context();

    let t2_constraints = params[1].constraints().unwrap();
    let t3_constraints = params[2].constraints().unwrap();
    // T2's constraint is T1, T3's is T2 (each rendered via the owner context).
    assert_eq!(
        t2_constraints[0].constraint.full_name_ctx(ctx).unwrap(),
        params[0].name().unwrap()
    );
    assert_eq!(
        t3_constraints[0].constraint.full_name_ctx(ctx).unwrap(),
        params[1].name().unwrap()
    );
}

// Test/Mono.Cecil.Tests/TypeTests.cs:127 (GenericForwardBaseType)
fn generic_forward_base_type() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let tamchild = module.get_type("TamChild").unwrap().unwrap();
    let base = tamchild.base_type().unwrap().unwrap();
    let TypeDefOrRef::Spec(spec) = base else {
        panic!("expected a generic instance base type")
    };
    let TypeSig::GenericInst { args, .. } = spec.signature().unwrap() else {
        panic!("expected a generic instance signature")
    };
    assert_eq!(args.len(), 1);
    let tamtam = module.get_type("Tamtam").unwrap().unwrap();
    assert_eq!(
        args[0],
        TypeSig::Named {
            token: tamtam.token(),
            is_value_type: false
        }
    );
}

// Test/Mono.Cecil.Tests/TypeTests.cs:145 (TypeExtentingGenericOfSelf)
fn type_extending_generic_of_self() {
    let data = read_fixture("Generics.cs.dll");
    let module = Module::read(&data).unwrap();
    let rec_child = module.get_type("RecChild").unwrap().unwrap();
    let base = rec_child.base_type().unwrap().unwrap();
    let TypeDefOrRef::Spec(spec) = base else {
        panic!("expected a generic instance base type")
    };
    let TypeSig::GenericInst { args, .. } = spec.signature().unwrap() else {
        panic!("expected a generic instance signature")
    };
    assert_eq!(args.len(), 1);
    assert_eq!(
        args[0],
        TypeSig::Named {
            token: rec_child.token(),
            is_value_type: false
        }
    );
}

// Test/Mono.Cecil.Tests/TypeTests.cs:177 (GenericInterfaceReference)
fn generic_interface_reference() {
    let data = read_resource("gifaceref.exe");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Program").unwrap().unwrap();
    let iface = &ty.interfaces().unwrap()[0];
    let TypeDefOrRef::Spec(spec) = iface.interface else {
        panic!("expected a generic instance interface")
    };
    let TypeSig::GenericInst {
        ty: owner, args, ..
    } = spec.signature().unwrap()
    else {
        panic!("expected a generic instance signature")
    };
    assert_eq!(args.len(), 1);
    // owner is a TypeRef to a one-generic-parameter interface; assert the
    // arity encoded in its name.
    let owner = TypeDefOrRef::from_token_test(&module, owner);
    assert!(owner.ends_with("`1"), "owner {owner}");
}

// Helper for the test above: resolve a token's full name through the module.
trait FromTokenTest {
    fn from_token_test(module: &Module<'_>, token: cil_reader::MetadataToken) -> String;
}

impl FromTokenTest for TypeDefOrRef<'_> {
    fn from_token_test(module: &Module<'_>, token: cil_reader::MetadataToken) -> String {
        match module.lookup_token(token).unwrap().unwrap() {
            cil_reader::Entity::TypeDef(t) => t.full_name().unwrap(),
            cil_reader::Entity::TypeRef(t) => t.full_name().unwrap(),
            cil_reader::Entity::TypeSpec(t) => t.full_name().unwrap(),
            _ => panic!("not a type token"),
        }
    }
}

// Test/Mono.Cecil.Tests/TypeTests.cs:192 (UnboundGenericParameter)
fn unbound_generic_parameter() {
    let data = read_resource("cscgpbug.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("ListViewModel").unwrap().unwrap();
    let method = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "<>n__FabricatedMethod1")
        .unwrap();
    // Cecil: the return type is a generic parameter at position 0 with no
    // owner — the signature says Var(0) but the declaring type declares no
    // generic parameters, so it stays unbound (rendered "!0").
    assert_eq!(method.return_type().unwrap(), TypeSig::Var(0));
    assert!(ty.generic_params().unwrap().is_empty());
    assert!(method.full_name().unwrap().starts_with("!0 "));
}

// Test/Mono.Cecil.Tests/TypeTests.cs:238 (ExplicitThis)
fn explicit_this() {
    let data = read_fixture("explicitthis.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("MakeDecision").unwrap().unwrap();
    let method = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "Decide")
        .unwrap();
    let TypeSig::FnPtr(fptr) = method.return_type().unwrap() else {
        panic!("expected a function pointer return type")
    };
    assert!(fptr.has_this);
    assert!(fptr.explicit_this);
    assert_eq!(fptr.params.len(), 2);
}

// Test/Mono.Cecil.Tests/TypeTests.cs:278 (SelfReferencingTypeRef): must load
// without blowing the stack on the obfuscated self-referential TypeRef row.
fn self_referencing_type_ref() {
    let data = read_resource("self-ref-typeref.dll");
    let module = Module::read(&data).unwrap();
    for rid in 1..=module.metadata().row_count(cil_reader::raw::Table::TypeRef) {
        let tr = module.type_ref(rid).unwrap();
        let _ = tr.full_name().unwrap();
        let _ = tr.scope_name().unwrap();
    }
}

pub(super) const TESTS: &[Test] = &[
    ("type_layout", type_layout),
    ("simple_interfaces", simple_interfaces),
    ("generic_type_definition", generic_type_definition),
    ("constrained_generic_type", constrained_generic_type),
    ("generic_base_type", generic_base_type),
    (
        "generic_constraint_on_generic_parameter",
        generic_constraint_on_generic_parameter,
    ),
    ("generic_forward_base_type", generic_forward_base_type),
    (
        "type_extending_generic_of_self",
        type_extending_generic_of_self,
    ),
    ("generic_interface_reference", generic_interface_reference),
    ("unbound_generic_parameter", unbound_generic_parameter),
    ("explicit_this", explicit_this),
    ("self_referencing_type_ref", self_referencing_type_ref),
];
