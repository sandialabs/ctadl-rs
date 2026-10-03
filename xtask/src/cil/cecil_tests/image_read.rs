//! Ported from Test/Mono.Cecil.Tests/ImageReadTests.cs (read-relevant subset;
//! raw-layer assertions). Skipped here: WindowsRuntimeComponentAssembly
//! (Windows-only in Cecil), LocallyScopedConstantArray (portable PDB, M8).

use super::{read_resource, Test};
use cil_reader::raw::heaps::GuidIndex;
use cil_reader::raw::pe::module_attributes as attrs;
use cil_reader::raw::pe::module_characteristics as mchar;
use cil_reader::raw::tables::*;
use cil_reader::raw::{parse_runtime, ModuleKind, PeImage, TargetArchitecture, TargetRuntime};

// Test/Mono.Cecil.Tests/ImageReadTests.cs:17 (ImageSections)
fn image_sections() {
    let data = read_resource("hello.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.sections.len(), 3);
    assert_eq!(image.sections[0].name, ".text");
    assert_eq!(image.sections[1].name, ".rsrc");
    assert_eq!(image.sections[2].name, ".reloc");
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:28 (ImageMetadataVersion)
fn image_metadata_version() {
    let data = read_resource("hello.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(parse_runtime(image.runtime_version), TargetRuntime::Net2_0);

    let data = read_resource("hello1.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(parse_runtime(image.runtime_version), TargetRuntime::Net1_1);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:38 (ImageModuleKind)
fn image_module_kind() {
    for (name, kind) in [
        ("hello.exe", ModuleKind::Console),
        ("libhello.dll", ModuleKind::Dll),
        ("hellow.exe", ModuleKind::Windows),
    ] {
        let data = read_resource(name);
        let image = PeImage::parse(&data).unwrap();
        assert_eq!(image.kind, kind, "{name}");
    }
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:51 (MetadataHeaps)
fn metadata_heaps() {
    let data = read_resource("hello.exe");
    let image = PeImage::parse(&data).unwrap();
    let md = image.metadata().unwrap();

    assert_eq!(
        md.strings().get(cil_reader::raw::StringIndex(0)).unwrap(),
        ""
    );
    assert_eq!(
        md.strings().get(cil_reader::raw::StringIndex(1)).unwrap(),
        "<Module>"
    );

    assert_eq!(md.user_strings().get(0).unwrap().to_string_lossy(), "");
    assert_eq!(
        md.user_strings().get(1).unwrap().to_string_lossy(),
        "Hello Cecil World !"
    );

    assert_eq!(
        md.guids().get(GuidIndex(0)).unwrap(),
        cil_reader::raw::Guid([0; 16])
    );
    assert_eq!(
        md.guids().get(GuidIndex(1)).unwrap().to_string(),
        "c3bc2bd3-2576-4d00-a80e-465b5632415f"
    );

    assert_eq!(
        md.blobs().get(cil_reader::raw::BlobIndex(0)).unwrap(),
        &[] as &[u8]
    );
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:74 (TablesHeap)
fn tables_heap() {
    let data = read_resource("hello.exe");
    let image = PeImage::parse(&data).unwrap();
    let md = image.metadata().unwrap();

    assert_eq!(md.row_count(Table::Module), 1);
    assert_eq!(md.row_count(Table::TypeRef), 4);
    assert_eq!(md.row_count(Table::TypeDef), 2);
    assert_eq!(md.row_count(Table::Field), 0);
    assert_eq!(md.row_count(Table::Method), 2);
    assert_eq!(md.row_count(Table::MemberRef), 4);
    assert_eq!(md.row_count(Table::CustomAttribute), 2);
    assert_eq!(md.row_count(Table::Assembly), 1);
    assert_eq!(md.row_count(Table::AssemblyRef), 1);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:94 (X64Module)
fn x64_module() {
    let data = read_resource("hello.x64.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.architecture, TargetArchitecture::Amd64);
    assert_eq!(image.attributes, attrs::IL_ONLY);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:103 (AnyCPU32BitPreferred)
fn anycpu_32bit_preferred() {
    let data = read_resource("anycpu32bitpreferred.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_ne!(image.characteristics & 0x0020, 0);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:111 (X64ModuleTextOnlySection)
fn x64_module_text_only_section() {
    let data = read_resource("hello.textonly.x64.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.architecture, TargetArchitecture::Amd64);
    assert_eq!(image.attributes, attrs::IL_ONLY);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:120 (IA64Module)
fn ia64_module() {
    let data = read_resource("hello.ia64.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.architecture, TargetArchitecture::Ia64);
    assert_eq!(image.attributes, attrs::IL_ONLY);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:129 (X86Module)
fn x86_module() {
    let data = read_resource("hello.x86.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.architecture, TargetArchitecture::I386);
    assert_eq!(image.attributes, attrs::IL_ONLY | attrs::REQUIRED_32_BIT);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:138 (AnyCpuModule)
fn anycpu_module() {
    let data = read_resource("hello.anycpu.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.architecture, TargetArchitecture::I386);
    assert_eq!(image.attributes, attrs::IL_ONLY);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:147 (DelaySignedAssembly) — raw half;
// the Assembly.Name.PublicKey assertions live in the module tests.
fn delay_signed_assembly() {
    let data = read_resource("delay-signed.dll");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.attributes & attrs::STRONG_NAME_SIGNED, 0);
    assert_ne!(image.strong_name_directory.virtual_address, 0);
    assert_ne!(image.strong_name_directory.size, 0);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:159 (WindowsPhoneNonSignedAssembly) — raw half.
fn windows_phone_non_signed_assembly() {
    let data = read_resource("wp7.dll");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.attributes & attrs::STRONG_NAME_SIGNED, 0);
    assert_eq!(image.strong_name_directory.virtual_address, 0);
    assert_eq!(image.strong_name_directory.size, 0);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:170 (MetroAssembly)
fn metro_assembly() {
    let data = read_resource("metro.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(
        image.dll_characteristics & mchar::APP_CONTAINER,
        mchar::APP_CONTAINER
    );
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:196 (DeterministicAssembly)
fn deterministic_assembly() {
    let data = read_resource("Deterministic.dll");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.debug_header.len(), 1);
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == cil_reader::raw::pe::image_debug_type::DETERMINISTIC));
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:209 (Net471TargetingAssembly)
fn net471_targeting_assembly() {
    let data = read_resource("net471.exe");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.subsystem_major, 6);
    assert_eq!(image.subsystem_minor, 0);
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:232 (ExternalPdbDeterministicAssembly)
fn external_pdb_deterministic_assembly() {
    use cil_reader::raw::pe::image_debug_type as idt;
    let data = read_resource("ExternalPdbDeterministic.dll");
    let image = PeImage::parse(&data).unwrap();
    assert!(image.debug_header.len() >= 2);
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == idt::CODE_VIEW));
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == idt::DETERMINISTIC));
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:253 (EmbeddedPdbDeterministicAssembly)
fn embedded_pdb_deterministic_assembly() {
    use cil_reader::raw::pe::image_debug_type as idt;
    let data = read_resource("EmbeddedPdbDeterministic.dll");
    let image = PeImage::parse(&data).unwrap();
    assert!(image.debug_header.len() >= 3);
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == idt::CODE_VIEW));
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == idt::DETERMINISTIC));
    assert!(image
        .debug_header
        .iter()
        .any(|e| e.debug_type == idt::EMBEDDED_PORTABLE_PDB));
}

// Test/Mono.Cecil.Tests/ImageReadTests.cs:189 (winrtcomp raw half): subsystem
// versions are plain header fields, readable without WinRT projections.
fn winrt_component_subsystem() {
    let data = read_resource("winrtcomp.winmd");
    let image = PeImage::parse(&data).unwrap();
    assert_eq!(image.subsystem_major, 6);
    assert_eq!(image.subsystem_minor, 2);
}

// Test/Mono.Cecil.Tests/ModuleTests.cs:249 (BadImageFormat analogue): a text
// file must fail cleanly, never panic.
fn text_file_is_bad_image() {
    let data = read_resource("text_file.txt");
    assert!(PeImage::parse(&data).is_err());
}

// Smoke: raw row reads across every table of a real image.
fn raw_rows_smoke() {
    let data = read_resource("hello.exe");
    let image = PeImage::parse(&data).unwrap();
    let md = image.metadata().unwrap();

    let module = md.row::<ModuleRaw>(1).unwrap();
    assert_eq!(md.strings().get(module.name).unwrap(), "hello.exe");

    let names: Vec<_> = md
        .table::<TypeDefRaw>()
        .rows()
        .map(|(_, r)| md.strings().get(r.unwrap().name).unwrap())
        .collect();
    assert_eq!(names, ["<Module>", "Program"]);

    let asm = md.row::<AssemblyRaw>(1).unwrap();
    assert_eq!(md.strings().get(asm.name).unwrap(), "hello");
}

pub(super) const TESTS: &[Test] = &[
    ("image_sections", image_sections),
    ("image_metadata_version", image_metadata_version),
    ("image_module_kind", image_module_kind),
    ("metadata_heaps", metadata_heaps),
    ("tables_heap", tables_heap),
    ("x64_module", x64_module),
    ("anycpu_32bit_preferred", anycpu_32bit_preferred),
    ("x64_module_text_only_section", x64_module_text_only_section),
    ("ia64_module", ia64_module),
    ("x86_module", x86_module),
    ("anycpu_module", anycpu_module),
    ("delay_signed_assembly", delay_signed_assembly),
    (
        "windows_phone_non_signed_assembly",
        windows_phone_non_signed_assembly,
    ),
    ("metro_assembly", metro_assembly),
    ("deterministic_assembly", deterministic_assembly),
    ("net471_targeting_assembly", net471_targeting_assembly),
    (
        "external_pdb_deterministic_assembly",
        external_pdb_deterministic_assembly,
    ),
    (
        "embedded_pdb_deterministic_assembly",
        embedded_pdb_deterministic_assembly,
    ),
    ("winrt_component_subsystem", winrt_component_subsystem),
    ("text_file_is_bad_image", text_file_is_bad_image),
    ("raw_rows_smoke", raw_rows_smoke),
];
