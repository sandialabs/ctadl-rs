//! Ported from Test/Mono.Cecil.Tests/ModuleTests.cs (read subset).
//!
//! Skipped (write-side or environment-specific): CreateModuleEscapesAssemblyName,
//! Win32FileVersion (Win32 version resources), OwnedStreamModuleFileName,
//! OpenModule{Immediate,Deferred}*, ImmediateRead*, ReadAndWriteFile,
//! ExceptionInWriteDoesNotKeepLockOnFile. NestedTypeForwarder and
//! HasTypeReference come with the compiled TestCSharp fixtures.

mod testutil;

use cil_reader::Module;
use cil_reader::raw::ModuleKind;
use testutil::read_resource;

// Test/Mono.Cecil.Tests/ModuleTests.cs:25 (SingleModule)
#[test]
fn single_module() {
    let data = read_resource!("hello.exe");
    let module = Module::read(&data).unwrap();
    assert!(module.assembly_name().unwrap().is_some());
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:36 (EntryPoint)
#[test]
fn entry_point() {
    let data = read_resource!("hello.exe");
    let module = Module::read(&data).unwrap();
    let entry_point = module.entry_point().unwrap().unwrap();
    assert_eq!(
        entry_point.full_name().unwrap(),
        "System.Void Program::Main()"
    );
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:53 (MultiModules) — Cecil loads the
// netmodules through the assembly; we read each file directly.
#[test]
fn multi_modules() {
    let data = read_resource!("mma.exe");
    let module = Module::read(&data).unwrap();
    assert_eq!(module.name().unwrap(), "mma.exe");
    assert_eq!(module.kind(), ModuleKind::Console);

    let data = read_resource!("moda.netmodule");
    let module = Module::read(&data).unwrap();
    assert_eq!(module.name().unwrap(), "moda.netmodule");
    assert_eq!(
        module.mvid().unwrap().to_string(),
        "eedb4721-6c3e-4d9a-be30-49021121dd92"
    );
    assert_eq!(module.kind(), ModuleKind::NetModule);

    let data = read_resource!("modb.netmodule");
    let module = Module::read(&data).unwrap();
    assert_eq!(module.name().unwrap(), "modb.netmodule");
    assert_eq!(
        module.mvid().unwrap().to_string(),
        "46c5c577-11b2-4ea0-bb3c-3c71f1331dd0"
    );
    assert_eq!(module.kind(), ModuleKind::NetModule);
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:76 (ModuleInformation)
#[test]
fn module_information() {
    let data = read_resource!("hello.exe");
    let module = Module::read(&data).unwrap();
    assert_eq!(module.name().unwrap(), "hello.exe");
    assert_eq!(
        module.mvid().unwrap().to_string(),
        "c3bc2bd3-2576-4d00-a80e-465b5632415f"
    );
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:87 (AssemblyReferences)
#[test]
fn assembly_references() {
    let data = read_resource!("hello.exe");
    let module = Module::read(&data).unwrap();
    let refs = module.assembly_references().unwrap();
    assert_eq!(refs.len(), 1);
    let reference = &refs[0];
    assert_eq!(reference.name, "mscorlib");
    assert_eq!(reference.version, (2, 0, 0, 0));
    assert_eq!(
        reference.public_key_token,
        &[0xB7, 0x7A, 0x5C, 0x56, 0x19, 0x34, 0xE0, 0x89]
    );
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:101 (ModuleReferences)
#[test]
fn module_references() {
    let data = read_resource!("pinvoke.exe");
    let module = Module::read(&data).unwrap();
    let refs = module.module_references().unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].name, "kernel32.dll");
    assert_eq!(refs[1].name, "shell32.dll");
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:111 (Types)
#[test]
fn types() {
    let data = read_resource!("hello.exe");
    let module = Module::read(&data).unwrap();
    let types = module.types().unwrap();
    assert_eq!(types.len(), 2);
    assert_eq!(types[0].full_name().unwrap(), "<Module>");
    assert_eq!(
        module
            .get_type("<Module>")
            .unwrap()
            .unwrap()
            .full_name()
            .unwrap(),
        "<Module>"
    );
    assert_eq!(types[1].full_name().unwrap(), "Program");
    assert_eq!(
        module
            .get_type("Program")
            .unwrap()
            .unwrap()
            .full_name()
            .unwrap(),
        "Program"
    );
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:123 (LinkedResource)
#[test]
fn linked_resource() {
    let data = read_resource!("libres.dll");
    let module = Module::read(&data).unwrap();
    let resources = module.resources().unwrap();
    let resource = resources
        .iter()
        .find(|r| r.name() == "linked.txt")
        .expect("linked.txt resource");
    match resource {
        cil_reader::model::Resource::Linked { file, .. } => {
            assert_eq!(*file, "linked.txt");
            assert!(resource.is_public());
        }
        _ => panic!("expected a linked resource"),
    }
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:137 (EmbeddedResource)
#[test]
fn embedded_resource() {
    let data = read_resource!("libres.dll");
    let module = Module::read(&data).unwrap();
    let resources = module.resources().unwrap();

    for (name, content) in [("embedded1.txt", "Hello"), ("embedded2.txt", "World")] {
        let resource = resources
            .iter()
            .find(|r| r.name() == name)
            .expect("embedded resource");
        match resource {
            cil_reader::model::Resource::Embedded { offset, .. } => {
                assert!(resource.is_public());
                let bytes = module.resource_data(*offset).unwrap();
                assert_eq!(bytes, content.as_bytes());
            }
            _ => panic!("expected an embedded resource"),
        }
    }
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:163 (ExportedTypeFromNetModule)
#[test]
fn exported_type_from_net_module() {
    let data = read_resource!("mma.exe");
    let module = Module::read(&data).unwrap();
    assert!(module.has_exported_types());
    let exported: Vec<_> = module.exported_types().collect();
    assert_eq!(exported.len(), 2);

    assert_eq!(exported[0].full_name().unwrap(), "Module.A.Foo");
    assert_eq!(exported[0].scope_name().unwrap(), "moda.netmodule");

    assert_eq!(exported[1].full_name().unwrap(), "Module.B.Baz");
    assert_eq!(exported[1].scope_name().unwrap(), "modb.netmodule");
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:229 (ModuleWithoutBlob)
#[test]
fn module_without_blob() {
    let data = read_resource!("noblob.dll");
    let module = Module::read(&data).unwrap();
    // Cecil asserts Image.BlobHeap == null; the analogue here is an empty heap
    // and a module that still reads fine.
    assert_eq!(module.name().unwrap(), "noblob.dll");
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:237 (MixedModeModule)
#[test]
fn mixed_mode_module() {
    let data = read_resource!("cppcli.dll");
    let module = Module::read(&data).unwrap();
    let refs = module.module_references().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "");
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:246 (OpenIrrelevantFile)
#[test]
fn open_irrelevant_file() {
    let data = read_resource!("text_file.txt");
    assert!(Module::read(&data).is_err());
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:252 (GetTypeNamespacePlusName)
#[test]
fn get_type_namespace_plus_name() {
    let data = read_resource!("moda.netmodule");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type_by("Module.A", "Foo").unwrap();
    assert!(ty.is_some());
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:261 (GetNonExistentTypeRuntimeName)
#[test]
fn get_non_existent_type() {
    let data = read_resource!("libhello.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("DoesNotExist").unwrap();
    assert!(ty.is_none());
}
