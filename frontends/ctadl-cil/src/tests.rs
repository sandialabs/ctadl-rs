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

/// Arithmetic, bitwise operations, shifts and comparisons supply values only (S19); a
/// conversion (a cast), the operations that carry an address, and the unknown one stay copies.
#[test]
fn value_only_arithmetic() {
    use cil_reader::flow::{CmpOp, ConvTarget, ConvType, Op, OpFlags, TypeOp};
    let f = OpFlags::default();
    let ty = TypeOp {
        token: MetadataToken(0x0200_0002),
    };
    for op in [
        Op::Add(f),
        Op::Sub(f),
        Op::Mul(f),
        Op::Div(f),
        Op::Rem(f),
        Op::And,
        Op::Or,
        Op::Xor,
        Op::Shl,
        Op::Shr(f),
        Op::Neg,
        Op::Not,
        Op::Cmp(CmpOp::Eq, f),
        Op::Cmp(CmpOp::Lt, f),
        Op::RefAnyType,
    ] {
        assert!(crate::effects::value_only(&op), "{op:?}");
    }
    let conv = Op::Conv(ConvTarget {
        ty: ConvType::I4,
        flags: f,
    });
    for op in [conv, Op::MkRefAny(ty), Op::RefAnyVal(ty), Op::Opaque] {
        assert!(!crate::effects::value_only(&op), "{op:?}");
    }
}

/// A static field's class is declared as a global once per program, however many statements
/// name it: the emitter interns it by name.
#[test]
fn an_emitter_declares_a_class_global_once() {
    use crate::method::Emitter;
    use ctadl_ir::cir::{FunctionData, Globals};
    let mut fdat = FunctionData::default();
    let mut globals = Globals::default();
    let mut em = Emitter::with_globals(&mut fdat, &mut globals);
    let a = em.global("NS.C");
    let b = em.global("NS.D");
    assert_eq!(em.global("NS.C"), a);
    assert_ne!(a, b);
    assert_eq!(globals.len(), 2);
    assert_eq!(globals.lookup("NS.C"), a.variable.global());
}

/// `is_cli_image` gives the same verdict as parsing the whole file read into memory.
#[test]
fn is_cli_image_matches_whole_file_read() {
    use std::path::Path;

    use cil_reader::Module;

    use crate::is_cli_image;

    fn whole_file(path: &Path) -> bool {
        std::fs::read(path).is_ok_and(|d| Module::read(&d).is_ok())
    }
    fn check(path: &Path, expected: Option<bool>) {
        let got = is_cli_image(path);
        assert_eq!(got, whole_file(path), "{}", path.display());
        if let Some(expected) = expected {
            assert_eq!(got, expected, "{}", path.display());
        }
    }

    let dir = std::env::temp_dir().join(format!("ctadl-cil-is-cli-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let text = dir.join("text.dll");
    std::fs::write(&text, "not a PE image\n").unwrap();
    check(&text, Some(false));
    check(&dir.join("missing.dll"), Some(false));
    let subdir = dir.join("subdir.dll");
    std::fs::create_dir(&subdir).unwrap();
    check(&subdir, Some(false));

    match std::env::var_os("CIL_READER_FIXTURES") {
        None => eprintln!("CIL_READER_FIXTURES unset: skipping the fixture cases"),
        Some(fixtures) => {
            let mut dlls: Vec<_> = std::fs::read_dir(&fixtures)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "dll"))
                .collect();
            dlls.sort();
            assert!(!dlls.is_empty(), "no *.dll in {}", Path::new(&fixtures).display());
            for dll in &dlls {
                check(dll, Some(true));
            }

            let bytes = std::fs::read(&dlls[0]).unwrap();
            let len = bytes.len();
            for (cut, expected) in [
                (0, Some(false)),
                (64, Some(false)),
                (127, Some(false)),
                (128, None),
                (300, None),
                (len / 2, None),
                (len - 1, None),
            ] {
                let p = dir.join(format!("truncated-{cut}.dll"));
                std::fs::write(&p, &bytes[..cut]).unwrap();
                check(&p, expected);
            }

            // Zero the CLI header data directory: a PE with no CLI header.
            let mut no_cli = bytes.clone();
            let lfanew = u32::from_le_bytes(no_cli[0x3C..0x40].try_into().unwrap()) as usize;
            let magic = u16::from_le_bytes(no_cli[lfanew + 24..lfanew + 26].try_into().unwrap());
            let cli_dir = match magic {
                0x10b => lfanew + 232,
                0x20b => lfanew + 248,
                m => panic!("optional-header magic {m:#x}"),
            };
            no_cli[cli_dir..cli_dir + 8].fill(0);
            let p = dir.join("no-cli-header.dll");
            std::fs::write(&p, &no_cli).unwrap();
            check(&p, Some(false));
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
