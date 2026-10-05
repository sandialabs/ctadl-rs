//! Malformed metadata that Cecil cannot read: its reader overflows its stack
//! or throws, where cil-reader returns an error. No compiler emits these
//! shapes, so each test patches rows of a corpus image in memory.

use super::{read_fixture, read_resource, Test};
use cil_reader::model::resolver::{AssemblyCache, DirectoryLoader};
use cil_reader::raw::pe::PeImage;
use cil_reader::raw::tables::{NestedClassRaw, Table};
use cil_reader::{Entity, Error, MetadataToken, Module, TokenType};

/// File offset of the `#~` (or `#-`) stream, from the metadata root's
/// stream headers (ECMA-335 II.24.2.1-2).
fn tables_stream_offset(data: &[u8]) -> usize {
    let image = PeImage::parse(data).unwrap();
    let root = image
        .resolve_rva(image.metadata_directory.virtual_address)
        .unwrap();
    let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    // Signature, versions and reserved (12 bytes), the version string's
    // padded length and the string, then Flags.
    let mut at = root + 16 + u32_at(root + 12) + 2;
    let streams = u16::from_le_bytes([data[at], data[at + 1]]);
    at += 2;
    for _ in 0..streams {
        let offset = u32_at(at);
        at += 8;
        let name_len = data[at..].iter().position(|&b| b == 0).unwrap();
        let name = &data[at..at + name_len];
        at += (name_len + 4) & !3;
        if name == b"#~" || name == b"#-" {
            return root + offset;
        }
    }
    panic!("no tables stream");
}

/// Overwrites the 2-byte column `column` bytes into row `rid` of `table`.
/// The images patched here have small heaps and tables, so every index
/// column is 2 bytes.
fn patch_column(data: &mut [u8], table: Table, rid: u32, column: usize, value: u16) {
    let at = {
        let module = Module::read(data).unwrap();
        let md = module.metadata();
        assert_eq!(md.heap_sizes, 0, "2-byte heap indexes");
        let info = md.table_info(table);
        assert!((1..=info.rows).contains(&rid));
        tables_stream_offset(data) + (info.offset + info.row_size * (rid - 1)) as usize + column
    };
    data[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// A coded index: `rid` shifted past `tag_bits` bits, tagged with `tag`.
fn coded(rid: u32, tag_bits: u32, tag: u32) -> u16 {
    u16::try_from(rid << tag_bits | tag).unwrap()
}

fn type_def<'m>(module: &'m Module<'_>, rid: u32) -> cil_reader::model::TypeDef<'m> {
    match module.lookup_token(MetadataToken::new(TokenType::TypeDef, rid)) {
        Ok(Some(Entity::TypeDef(t))) => t,
        _ => panic!("TypeDef {rid}"),
    }
}

fn type_ref<'m>(module: &'m Module<'_>, rid: u32) -> cil_reader::model::TypeRef<'m> {
    match module.lookup_token(MetadataToken::new(TokenType::TypeRef, rid)) {
        Ok(Some(Entity::TypeRef(t))) => t,
        _ => panic!("TypeRef {rid}"),
    }
}

fn is_unsupported<T>(result: cil_reader::Result<T>, what: &str) -> bool {
    matches!(result, Err(Error::Unsupported(w)) if w == what)
}

// A type nested in itself, or two nested in each other. Cecil's FullName
// follows DeclaringType until its stack overflows.
fn nested_class_cycles() {
    let original = read_fixture("NestedTypes.cs.dll");
    let module = Module::read(&original).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let bar = foo.nested_types().unwrap()[0];
    let baz = bar.nested_types().unwrap()[0];
    let (bar, baz) = (bar.token().rid(), baz.token().rid());
    // The NestedClass row of each, whose second column is the enclosing type.
    let row_of = |nested: u32| {
        let table = module.metadata().table::<NestedClassRaw>();
        (1..=table.len())
            .find(|&rid| table.row(rid).unwrap().nested_class == nested)
            .unwrap()
    };
    let (bar_row, baz_row) = (row_of(bar), row_of(baz));

    let mut data = original.clone();
    patch_column(&mut data, Table::NestedClass, baz_row, 2, baz as u16);
    let module = Module::read(&data).unwrap();
    assert!(is_unsupported(
        type_def(&module, baz).full_name(),
        "nested type cycle"
    ));
    assert_eq!(type_def(&module, bar).full_name().unwrap(), "Foo/Bar");

    let mut data = original.clone();
    patch_column(&mut data, Table::NestedClass, bar_row, 2, baz as u16);
    let module = Module::read(&data).unwrap();
    for rid in [bar, baz] {
        assert!(is_unsupported(
            type_def(&module, rid).full_name(),
            "nested type cycle"
        ));
    }
}

// Two TypeRefs scoped to each other. Cecil's FullName and Scope follow
// DeclaringType until its stack overflows. A TypeRef scoped to itself is
// one obfuscators write, and Cecil scopes it to the module.
fn type_ref_scope_cycles() {
    let mut data = read_resource("hello.exe");
    let own_name = type_ref(&Module::read(&data).unwrap(), 3)
        .full_name()
        .unwrap();
    // ResolutionScope is the first column; TypeRef's tag is 3.
    patch_column(&mut data, Table::TypeRef, 1, 0, coded(2, 2, 3));
    patch_column(&mut data, Table::TypeRef, 2, 0, coded(1, 2, 3));
    patch_column(&mut data, Table::TypeRef, 3, 0, coded(3, 2, 3));
    let module = Module::read(&data).unwrap();
    let loader = DirectoryLoader::new(Vec::new());
    let cache = AssemblyCache::new(&loader);
    for rid in [1, 2] {
        let tr = type_ref(&module, rid);
        assert!(is_unsupported(tr.full_name(), "TypeRef scope cycle"));
        assert!(is_unsupported(tr.scope_name(), "TypeRef scope cycle"));
        assert!(is_unsupported(
            module.resolve_type(tr.token(), &cache),
            "TypeRef scope cycle"
        ));
    }
    let own = type_ref(&module, 3);
    assert_eq!(own.full_name().unwrap(), own_name);
    assert_eq!(own.scope_name().unwrap(), module.name().unwrap());
}

// Cecil reads ExportedType rows in order, taking a nested one's declaring
// type from the rows before it, and throws on one nested in itself or in a
// later row.
fn exported_type_nesting() {
    let original = read_resource("mma.exe");
    // Implementation is the last column, after Flags, TypeDefId, TypeName
    // and TypeNamespace; ExportedType's tag is 2.
    const IMPLEMENTATION: usize = 12;
    let names: Vec<String> = Module::read(&original)
        .unwrap()
        .exported_types()
        .map(|et| et.full_name().unwrap())
        .collect();
    let scope = {
        let module = Module::read(&original).unwrap();
        let first = module.exported_types().next().unwrap();
        first.scope_name().unwrap().to_string()
    };

    for declaring in [1, 2] {
        let mut data = original.clone();
        patch_column(
            &mut data,
            Table::ExportedType,
            1,
            IMPLEMENTATION,
            coded(declaring, 2, 2),
        );
        let module = Module::read(&data).unwrap();
        let first = module.exported_types().next().unwrap();
        let what = "exported type nested in itself or a later row";
        assert!(is_unsupported(first.declaring_type(), what));
        assert!(is_unsupported(first.full_name(), what));
        assert!(is_unsupported(first.scope_name(), what));
    }

    let mut data = original.clone();
    patch_column(
        &mut data,
        Table::ExportedType,
        2,
        IMPLEMENTATION,
        coded(1, 2, 2),
    );
    let module = Module::read(&data).unwrap();
    let second = module.exported_types().nth(1).unwrap();
    let name = second.namespace().unwrap().to_string() + "." + second.name().unwrap();
    let name = name.trim_start_matches('.');
    assert_eq!(second.full_name().unwrap(), format!("{}/{name}", names[0]));
    assert_eq!(second.scope_name().unwrap(), scope);
}

pub(super) const TESTS: &[Test] = &[
    ("nested_class_cycles", nested_class_cycles),
    ("type_ref_scope_cycles", type_ref_scope_cycles),
    ("exported_type_nesting", exported_type_nesting),
];
