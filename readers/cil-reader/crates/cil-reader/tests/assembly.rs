//! Ported from Test/Mono.Cecil.Tests/AssemblyTests.cs (read subset).
//!
//! Skipped: ParseLowerCaseNameParts/ZeroVersion/NoBuildOrMajor (construct
//! AssemblyNameReference values, no reading), Retargetable's custom-attribute
//! half (ported with M5; the FullName half is covered below),
//! MismatchedLibraryAndSymbols (symbols).

mod testutil;

use cil_reader::Module;
use testutil::read_resource;

// Test/Mono.Cecil.Tests/AssemblyTests.cs:13 (Name)
#[test]
fn name() {
    let data = read_resource("hello.exe");
    let module = Module::read(&data).unwrap();
    let name = module.assembly_name().unwrap().unwrap();
    assert_eq!(name.name, "hello");
    assert_eq!(name.culture, "");
    assert_eq!(name.version, (0, 0, 0, 0));
    // AssemblyHashAlgorithm.SHA1
    assert_eq!(name.hash_algorithm, 0x8004);
}

// Test/Mono.Cecil.Tests/AssemblyTests.cs:60 (Retargetable) — the FullName
// rendering half; the attribute-argument navigation is ported with M5.
#[test]
fn retargetable_full_name() {
    let data = read_resource("RetargetableExample.dll");
    let module = Module::read(&data).unwrap();
    let refs = module.assembly_references().unwrap();
    let reference = refs
        .iter()
        .find(|r| r.name == "System.Data")
        .expect("System.Data reference");
    assert_eq!(
        reference.full_name(),
        "System.Data, Version=3.5.0.0, Culture=neutral, PublicKeyToken=969db8053d3322ac, Retargetable=Yes"
    );
}

// Test/Mono.Cecil.Tests/AssemblyTests.cs:80 (SystemRuntime) — the parts that
// do not require assembly resolution.
#[test]
fn system_runtime() {
    let data = read_resource("System.Runtime.dll");
    let module = Module::read(&data).unwrap();
    assert_eq!(
        module.assembly_name().unwrap().unwrap().name,
        "System.Runtime"
    );
    assert_eq!(module.assembly_references().unwrap().len(), 1);
}

// Sanity: a delay-signed assembly exposes its full public key
// (ImageReadTests.cs:147, model half).
#[test]
fn delay_signed_public_key() {
    let data = read_resource("delay-signed.dll");
    let module = Module::read(&data).unwrap();
    let name = module.assembly_name().unwrap().unwrap();
    assert!(!name.public_key.is_empty());
}

// wp7.dll (ImageReadTests.cs:159, model half): no public key.
#[test]
fn wp7_no_public_key() {
    let data = read_resource("wp7.dll");
    let module = Module::read(&data).unwrap();
    let name = module.assembly_name().unwrap().unwrap();
    assert!(name.public_key.is_empty());
}
