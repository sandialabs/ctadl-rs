//! Unit tests of the frontend's pure tables.
//!
//! A `MethodFlow` cannot be built outside cil-reader (its address facts are private), so the
//! lowering tables are checked by the IR goldens and corpus sweep of `cil:frontend`, the
//! `Cil:*` taint cases, and `cil:taint` (all in `cargo xtask regression --frontend cil`).
//! What is tested here needs no assembly.

use cil_reader::MetadataToken;
use cil_reader::model::{MethodSig, TypeSig};
use ctadl_ir::ParameterType;

use crate::method::{holds_address, parameter_type, parameters};
use crate::names::{MethodKey, simple_name};

fn named(is_value_type: bool) -> TypeSig {
    TypeSig::Named {
        token: MetadataToken(0x0200_0002),
        is_value_type,
    }
}

/// Reference types, addresses and generic parameters are `Shared`; primitives and
/// value types `SharedFields`.
#[test]
fn parameter_types() {
    use ParameterType::*;
    let cases = [
        (TypeSig::I4, SharedFields),
        (TypeSig::R8, SharedFields),
        (TypeSig::IntPtr, SharedFields),
        (named(true), SharedFields),
        (named(false), Shared),
        (TypeSig::String, Shared),
        (TypeSig::Object, Shared),
        (TypeSig::SzArray(Box::new(TypeSig::I4)), Shared),
        (TypeSig::ByRef(Box::new(TypeSig::I4)), Shared),
        (TypeSig::Ptr(Box::new(TypeSig::I4)), Shared),
        (TypeSig::Var(0), Shared),
        (TypeSig::MVar(1), Shared),
        (
            TypeSig::CModOpt {
                modifier: MetadataToken(0x0100_0001),
                inner: Box::new(TypeSig::I4),
            },
            SharedFields,
        ),
    ];
    for (sig, want) in cases {
        assert_eq!(parameter_type(&sig), want, "{sig:?}");
    }
}

#[test]
fn addresses_are_byref_and_pointer_types() {
    assert!(holds_address(&TypeSig::ByRef(Box::new(TypeSig::I4))));
    assert!(holds_address(&TypeSig::Ptr(Box::new(TypeSig::I4))));
    assert!(holds_address(&TypeSig::Pinned(Box::new(TypeSig::ByRef(
        Box::new(TypeSig::I4)
    )))));
    assert!(!holds_address(&TypeSig::IntPtr));
    assert!(!holds_address(&named(false)));
}

/// `this` first, then each parameter; a vararg call site's extras after the sentinel are
/// not parameters of the method.
#[test]
fn parameters_of_a_signature() {
    let sig = MethodSig {
        has_this: true,
        explicit_this: false,
        calling_convention: cil_reader::model::sig::calling_convention::VAR_ARG,
        generic_arity: 0,
        return_type: TypeSig::Void,
        params: vec![
            TypeSig::I4,
            TypeSig::String,
            TypeSig::Sentinel(Box::new(TypeSig::Object)),
        ],
    };
    assert_eq!(
        parameters(&sig),
        vec![
            ParameterType::Shared,
            ParameterType::SharedFields,
            ParameterType::Shared
        ]
    );
}

/// A generic method's arity is part of its simple name, and the function id joins
/// class, simple name and descriptor.
#[test]
fn names() {
    assert_eq!(simple_name("Parse", 0), "Parse");
    assert_eq!(simple_name("Select", 2), "Select``2");
    let key = MethodKey {
        cls: "[MyApp]Acme.Gadget".to_string(),
        simple: "Parse".to_string(),
        desc: "(System.String)->System.Int32".to_string(),
    };
    assert_eq!(
        key.fid(),
        "[MyApp]Acme.Gadget::Parse(System.String)->System.Int32"
    );
}

/// The assembly-selection policy.
#[test]
fn framework_assemblies() {
    for name in [
        "System.Runtime",
        "System",
        "mscorlib",
        "netstandard",
        "Microsoft.Extensions.Logging",
    ] {
        assert!(crate::is_framework_assembly(name), "{name}");
    }
    for name in ["MyApp", "Newtonsoft.Json", "SystemX", "Microsoft.Acme"] {
        assert!(!crate::is_framework_assembly(name), "{name}");
    }
}
