//! Malformed metadata that Cecil cannot read: its reader overflows its stack,
//! hangs or throws, where cil-reader returns an error or reads what it can;
//! and some that Cecil reads only partly, next to a variant it throws on. No
//! compiler emits these shapes, so each test patches a corpus image in
//! memory.

use super::{cecil_root, read_fixture, read_resource, Test};
use cil_reader::model::custom_attributes::MAX_ATTRIBUTE_DEPTH;
use cil_reader::model::resolver::{AssemblyCache, DirectoryLoader};
use cil_reader::model::sig::MAX_TYPE_SIG_DEPTH;
use cil_reader::raw::pe::PeImage;
use cil_reader::raw::tables::{
    CustomAttributeRaw, FieldRaw, ModuleRaw, NestedClassRaw, Table, TypeDefRaw, TypeSpecRaw,
};
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

fn type_spec<'m>(module: &'m Module<'_>, rid: u32) -> cil_reader::model::TypeSpec<'m> {
    match module.lookup_token(MetadataToken::new(TokenType::TypeSpec, rid)) {
        Ok(Some(Entity::TypeSpec(t))) => t,
        _ => panic!("TypeSpec {rid}"),
    }
}

fn is_unsupported<T>(result: cil_reader::Result<T>, what: &str) -> bool {
    matches!(result, Err(Error::Unsupported(w)) if w == what)
}

/// The NestedClass row whose nested type is `nested`.
fn nested_class_row(module: &Module<'_>, nested: u32) -> u32 {
    let table = module.metadata().table::<NestedClassRaw>();
    (1..=table.len())
        .find(|&rid| table.row(rid).unwrap().nested_class == nested)
        .unwrap()
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
    let (bar_row, baz_row) = (
        nested_class_row(&module, bar),
        nested_class_row(&module, baz),
    );

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

// A NestedClass row naming rid 0 or a rid past the TypeDef table, as its
// nested or its enclosing type. Cecil's GetTypeDefinition returns null past
// the table, so ReadNestedTypes drops the type and DeclaringType is null;
// at rid 0 it throws. Neither names a type here.
fn nested_class_rids_out_of_range() {
    let original = read_fixture("NestedTypes.cs.dll");
    let module = Module::read(&original).unwrap();
    let foo = module.get_type("Foo").unwrap().unwrap();
    let bar = foo.nested_types().unwrap()[0].token().rid();
    let row = nested_class_row(&module, bar);
    let past = module.metadata().row_count(Table::TypeDef) + 1;

    for enclosing in [0, past] {
        let mut data = original.clone();
        patch_column(&mut data, Table::NestedClass, row, 2, enclosing as u16);
        let module = Module::read(&data).unwrap();
        let bar = type_def(&module, bar);
        assert!(bar.declaring_type().unwrap().is_none());
        assert_eq!(bar.full_name().unwrap(), "Bar");
        assert!(bar.fields().is_ok());
    }

    for nested in [0, past] {
        let mut data = original.clone();
        patch_column(&mut data, Table::NestedClass, row, 0, nested as u16);
        let module = Module::read(&data).unwrap();
        let foo = module.get_type("Foo").unwrap().unwrap();
        assert!(foo.nested_types().unwrap().is_empty());
        assert_eq!(type_def(&module, bar).full_name().unwrap(), "Bar");
    }
}

/// Every member of every type, by name; each list and name must read.
fn member_names(module: &Module<'_>) -> Vec<Vec<String>> {
    module
        .all_types()
        .map(|ty| {
            let fields = ty.fields().unwrap().into_iter().map(|f| f.name());
            let methods = ty.methods().unwrap().into_iter().map(|m| m.name());
            let properties = ty.properties().unwrap().into_iter().map(|p| p.name());
            let events = ty.events().unwrap().into_iter().map(|e| e.name());
            fields
                .chain(methods)
                .chain(properties)
                .chain(events)
                .map(|name| name.unwrap().into_owned())
                .collect()
        })
        .collect()
}

// Member lists that run past the table they index, and Ptr rows naming
// rid 0 or a rid past the member table. Cecil reads those rows at the
// wrong offset, as whatever bytes are there; here they name no member.
fn member_lists_out_of_range() {
    // TypeDef's FieldList column, after Flags, Name, Namespace and Extends.
    const FIELD_LIST: usize = 10;
    let original = read_fixture("Fields.cs.dll");
    let module = Module::read(&original).unwrap();
    let fields = module.metadata().row_count(Table::Field);
    let types = module.metadata().row_count(Table::TypeDef);
    // The last type with fields, whose list the one after it ends.
    let last = (1..types)
        .rev()
        .find(|&rid| type_def(&module, rid).has_fields().unwrap())
        .unwrap();
    let mut data = original.clone();
    patch_column(
        &mut data,
        Table::TypeDef,
        last + 1,
        FIELD_LIST,
        (fields + 10) as u16,
    );
    let module = Module::read(&data).unwrap();
    member_names(&module);
    let tail = type_def(&module, last).fields().unwrap();
    assert_eq!(tail.last().unwrap().token().rid(), fields);
    assert!(type_def(&module, last + 1).fields().unwrap().is_empty());

    let original = read_fixture("ptr-Fields.cs.dll");
    let before = member_names(&Module::read(&original).unwrap());
    let fields = Module::read(&original)
        .unwrap()
        .metadata()
        .row_count(Table::Field);
    let mut data = original.clone();
    patch_column(&mut data, Table::FieldPtr, 1, 0, 0);
    patch_column(&mut data, Table::FieldPtr, 2, 0, (fields + 1) as u16);
    let after = member_names(&Module::read(&data).unwrap());
    let count = |names: &Vec<Vec<String>>| names.iter().map(Vec::len).sum::<usize>();
    assert_eq!(count(&after), count(&before) - 2);

    // PropertyMap's PropertyList column, after the TypeDef index.
    let original = read_fixture("Properties.cs.dll");
    let module = Module::read(&original).unwrap();
    let properties = module.metadata().row_count(Table::Property);
    let maps = module.metadata().row_count(Table::PropertyMap);
    let mut data = original.clone();
    patch_column(
        &mut data,
        Table::PropertyMap,
        maps,
        2,
        (properties + 10) as u16,
    );
    let module = Module::read(&data).unwrap();
    member_names(&module);
    let with_properties: Vec<_> = module
        .all_types()
        .filter(|ty| !ty.properties().unwrap().is_empty())
        .collect();
    let tail = with_properties.last().unwrap().properties().unwrap();
    assert_eq!(tail.last().unwrap().token().rid(), properties);
}

// A type that is its own base, directly or through another, and a member
// reference to it that names no member: Cecil's GetMethod and GetField
// walk the bases until they find the member, so they never return.
fn base_type_cycles() {
    // TypeDef's Extends column, after Flags, Name and Namespace; TypeDef's
    // tag is 0. MemberRef's Name column follows its Class.
    const EXTENDS: usize = 8;
    const NAME: usize = 2;
    let original = read_fixture("Generics.cs.dll");
    let module = Module::read(&original).unwrap();
    let rid_of = |name: &str| module.get_type(name).unwrap().unwrap().token().rid();
    let (parent, tamtam) = (rid_of("Parent`1"), rid_of("Tamtam"));
    // Parent`1<T>::.ctor(), renamed after the type, which has no such method.
    let member = MetadataToken::new(TokenType::MemberRef, 0x13);
    let unknown = module.metadata().row::<TypeDefRaw>(parent).unwrap().name.0;
    let loader = DirectoryLoader::new(Vec::new());
    let cache = AssemblyCache::new(&loader);
    assert!(module.resolve_member(member, &cache).unwrap().is_some());

    let mut renamed = original.clone();
    patch_column(&mut renamed, Table::MemberRef, 0x13, NAME, unknown as u16);
    for bases in [
        vec![(parent, parent)],
        vec![(parent, tamtam), (tamtam, parent)],
    ] {
        let mut data = renamed.clone();
        for (rid, base) in bases {
            patch_column(&mut data, Table::TypeDef, rid, EXTENDS, coded(base, 2, 0));
        }
        let module = Module::read(&data).unwrap();
        assert!(is_unsupported(
            module.resolve_member(member, &cache),
            "base type cycle"
        ));
    }
}

/// Runs `test` on a thread with a 1 MiB stack, half Rust's default, so a
/// test of nesting at the limit also checks that it fits.
fn on_small_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(test)
        .unwrap()
        .join()
        .unwrap();
}

fn is_nested_too_deeply<T>(result: cil_reader::Result<T>) -> bool {
    matches!(
        result,
        Err(Error::BadImage {
            reason: "custom attribute argument nested too deeply",
            ..
        })
    )
}

// An enum whose underlying type is itself, directly or through another
// enum. Cecil's ReadCustomAttributeEnum reads the underlying type's value
// as an enum's in turn, until its stack overflows.
fn attribute_enum_underlying_cycles() {
    // Field's Signature column, after Flags and Name.
    const SIGNATURE: usize = 4;
    let original = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&original).unwrap();
    let fields_of = |name: &str| module.get_type(name).unwrap().unwrap().fields().unwrap();
    // Each enum's value__, and one of its literals, typed as the enum.
    let enums = ["Bingo", "GenericWithEnum`1/OnGenericNumber"].map(|name| {
        let fields = fields_of(name);
        let value = fields[0].token().rid();
        let literal = fields[1].token().rid();
        let typed_as_enum = module
            .metadata()
            .row::<FieldRaw>(literal)
            .unwrap()
            .signature
            .0;
        (value, typed_as_enum as u16)
    });
    // FooAttribute(Bingo).
    let attribute = (1..=module.metadata().row_count(Table::CustomAttribute))
        .find(|&rid| {
            let values = module.custom_attribute(rid).unwrap().values();
            values.is_ok_and(|v| {
                v.constructor_args
                    .first()
                    .is_some_and(|arg| arg.ty.full_name() == "Bingo")
            })
        })
        .unwrap();
    let (bingo, other) = (enums[0], enums[1]);
    let self_typed = {
        let mut data = original.clone();
        patch_column(&mut data, Table::Field, bingo.0, SIGNATURE, bingo.1);
        data
    };
    let each_other = {
        let mut data = original.clone();
        patch_column(&mut data, Table::Field, bingo.0, SIGNATURE, other.1);
        patch_column(&mut data, Table::Field, other.0, SIGNATURE, bingo.1);
        data
    };
    for data in [self_typed, each_other] {
        on_small_stack(move || {
            let module = Module::read(&data).unwrap();
            let values = module.custom_attribute(attribute).unwrap().values();
            assert!(is_nested_too_deeply(values));
        });
    }
}

// Arrays of arrays, and boxed values of boxed values, as deep as an
// argument may nest and one level deeper. Cecil reads each level of both
// recursively, without bound.
fn attribute_argument_nesting() {
    // CustomAttribute's Value column, after Parent and Type.
    const VALUE: usize = 4;
    let original = read_fixture("CustomAttributes.cs.dll");
    let module = Module::read(&original).unwrap();
    let attributes = 1..=module.metadata().row_count(Table::CustomAttribute);
    let row = |rid| module.metadata().row::<CustomAttributeRaw>(rid).unwrap();
    let blob = |rid| module.custom_attribute(rid).unwrap().blob().unwrap();
    // An attribute whose constructor has no parameters, and the largest blob.
    let target = attributes
        .clone()
        .find(|&rid| blob(rid) == [1, 0, 0, 0])
        .unwrap();
    let largest = attributes.max_by_key(|&rid| blob(rid).len()).unwrap();
    let room = blob(largest).len();
    let at = blob(largest).as_ptr() as usize - original.as_ptr() as usize;
    let mut redirected = original.clone();
    patch_column(
        &mut redirected,
        Table::CustomAttribute,
        target,
        VALUE,
        row(largest).value.0 as u16,
    );

    // One named field `A` of `type`, then `value`.
    let named = |ty: &[u8], value: &[u8]| {
        let mut bytes = vec![0x01, 0x00, 0x01, 0x00, 0x53];
        bytes.extend(ty);
        bytes.extend([0x01, b'A']);
        bytes.extend(value);
        assert!(bytes.len() <= room);
        let mut data = redirected.clone();
        data[at..at + bytes.len()].copy_from_slice(&bytes);
        data
    };
    let limit = MAX_ATTRIBUTE_DEPTH as usize;
    // `levels` arrays of int32, each of length 1, around 7.
    let arrays = |levels: usize| {
        let mut ty = vec![0x1d; levels];
        ty.push(0x08);
        let mut value = [1, 0, 0, 0].repeat(levels);
        value.extend([7, 0, 0, 0]);
        named(&ty, &value)
    };
    // `levels` boxes around the int32 7: the field's own type is object.
    let boxes = |levels: usize| {
        let mut value = vec![0x51; levels - 1];
        value.extend([0x08, 7, 0, 0, 0]);
        named(&[0x51], &value)
    };
    for (kind, at_limit, past_limit) in [
        ("arrays", arrays(limit), arrays(limit + 1)),
        ("boxes", boxes(limit), boxes(limit + 1)),
    ] {
        on_small_stack(move || {
            let module = Module::read(&at_limit).unwrap();
            let values = module.custom_attribute(target).unwrap().values();
            assert_eq!(values.unwrap().fields.len(), 1, "{kind}");
            let module = Module::read(&past_limit).unwrap();
            let values = module.custom_attribute(target).unwrap().values();
            assert!(is_nested_too_deeply(values), "{kind}");
        });
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

// An attribute argument whose enum type is a TypeRef in a scope cycle.
// Cecil's MetadataResolver follows the cycle until its stack overflows.
fn attribute_enum_scope_cycle() {
    let mut data = read_fixture("CustomAttributes.cs.dll");
    let (ace_flags, other) = {
        let module = Module::read(&data).unwrap();
        let rids = 1..=module.metadata().row_count(Table::TypeRef);
        let named = |name: &str| {
            rids.clone()
                .find(|&rid| type_ref(&module, rid).full_name().unwrap() == name)
                .unwrap()
        };
        (
            named("System.Security.AccessControl.AceFlags"),
            named("System.Object"),
        )
    };
    patch_column(&mut data, Table::TypeRef, ace_flags, 0, coded(other, 2, 3));
    patch_column(&mut data, Table::TypeRef, other, 0, coded(ace_flags, 2, 3));
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Ace").unwrap().unwrap();
    let attribute = module
        .custom_attributes_of(ty.token())
        .unwrap()
        .into_iter()
        .find(|a| a.attribute_type_name().unwrap().starts_with("Foo"))
        .unwrap();
    let loader = DirectoryLoader::new(Vec::new());
    let cache = AssemblyCache::new(&loader);
    assert!(is_unsupported(
        attribute.values_with(&cache),
        "TypeRef scope cycle"
    ));
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
    let name = second.namespace().unwrap().to_string() + "." + &second.name().unwrap();
    let name = name.trim_start_matches('.');
    assert_eq!(second.full_name().unwrap(), format!("{}/{name}", names[0]));
    assert_eq!(second.scope_name().unwrap(), scope);
}

// An exported type whose File names the assembly's own module: resolving
// it finds the module, misses the type there, and comes back to the same
// exported type, which ExportedType.Resolve throws on.
fn exported_type_file_loop() {
    let mut data = read_resource("mma.exe");
    let (file, own_name) = {
        let module = Module::read(&data).unwrap();
        let foo = module
            .exported_types()
            .find(|et| et.full_name().unwrap() == "Module.A.Foo")
            .unwrap();
        let implementation = foo.implementation().unwrap();
        assert_eq!(implementation.token_type(), Some(TokenType::File));
        let own_name = module.metadata().row::<ModuleRaw>(1).unwrap().name.0;
        (implementation.rid(), own_name)
    };
    // Name is the File table's second column, after the 4-byte Flags.
    patch_column(&mut data, Table::File, file, 4, own_name as u16);
    let module = Module::read(&data).unwrap();
    let loader = DirectoryLoader::new(vec![cecil_root().join("Test/Resources/assemblies")]);
    let cache = AssemblyCache::new(&loader);
    assert!(is_unsupported(
        module.resolve_type_name("Module.A", "Foo", &cache),
        "exported type circularity"
    ));
}

/// SQLite-net.dll's TypeSpecs whose signatures have room for `class
/// TypeSpec<rid>`, with the file offset of each signature.
fn roomy_type_specs(data: &[u8]) -> Vec<(u32, usize)> {
    let module = Module::read(data).unwrap();
    let md = module.metadata();
    (1..=md.row_count(Table::TypeSpec))
        .filter_map(|rid| {
            let blob = md
                .blobs()
                .get(md.row::<TypeSpecRaw>(rid).unwrap().signature)
                .unwrap();
            // A one-byte length prefix, and room for the longest signature.
            (3..0x80)
                .contains(&blob.len())
                .then(|| (rid, blob.as_ptr() as usize - data.as_ptr() as usize))
        })
        .collect()
}

/// Rewrites the signature at file offset `at`, and its length prefix.
fn set_signature(data: &mut [u8], at: usize, sig: &[u8]) {
    data[at - 1] = sig.len() as u8;
    data[at..at + sig.len()].copy_from_slice(sig);
}

/// `class TypeSpec<rid>`: CLASS, then the TypeDefOrRef coded index
/// (TypeSpec's tag is 2), compressed.
fn class_of_type_spec(rid: u32) -> Vec<u8> {
    let index = rid << 2 | 2;
    let mut sig = vec![0x12];
    if index < 0x80 {
        sig.push(index as u8);
    } else {
        sig.extend(u16::try_from(index | 0x8000).unwrap().to_be_bytes());
    }
    sig
}

fn is_too_deep<T>(result: cil_reader::Result<T>) -> bool {
    matches!(
        result,
        Err(Error::BadImage {
            reason: "type signature nested too deeply",
            ..
        })
    )
}

// A TypeSpec naming a TypeSpec: Cecil reads the named one's signature at
// that point of the first's, so the chain nests like one signature, and a
// TypeSpec naming itself recurses until Cecil's stack overflows. Each one
// in a chain counts as a level toward the nesting limit.
fn type_spec_chains() {
    let original = read_resource("SQLite-net.dll");
    let specs = roomy_type_specs(&original);
    let loader = DirectoryLoader::new(Vec::new());
    let cache = AssemblyCache::new(&loader);

    let (rid, at) = specs[0];
    let mut data = original.clone();
    set_signature(&mut data, at, &class_of_type_spec(rid));
    let module = Module::read(&data).unwrap();
    assert!(is_too_deep(type_spec(&module, rid).full_name()));
    let token = MetadataToken::new(TokenType::TypeSpec, rid);
    assert!(is_too_deep(module.resolve_type(token, &cache)));

    // `length` TypeSpecs, each naming the next, the last int32.
    let chain = |length: usize| {
        let mut data = original.clone();
        let links = &specs[..length];
        for pair in links.windows(2) {
            set_signature(&mut data, pair[0].1, &class_of_type_spec(pair[1].0));
        }
        set_signature(&mut data, links[length - 1].1, &[0x08]);
        data
    };
    let limit = MAX_TYPE_SIG_DEPTH as usize;
    let token = MetadataToken::new(TokenType::TypeSpec, specs[0].0);

    let data = chain(limit);
    let module = Module::read(&data).unwrap();
    assert_eq!(
        type_spec(&module, specs[0].0).full_name().unwrap(),
        "System.Int32"
    );
    assert!(module.resolve_type(token, &cache).is_ok());

    let data = chain(limit + 1);
    let module = Module::read(&data).unwrap();
    assert!(is_too_deep(type_spec(&module, specs[0].0).full_name()));
    assert!(is_too_deep(module.resolve_type(token, &cache)));
}

// A byte past the end of the opcode tables, where CodeReader.ReadOpCode
// throws: the body keeps the instructions before it, its locals and its
// exception handler.
fn opcode_past_the_tables() {
    let mut data = read_resource("PdbTarget.exe");
    // Dispose of a compiler-generated iterator: a fat body with one local
    // and a try/finally, whose instruction at IL_000e is ldloc.0.
    let rid = 0x0c;
    let code = {
        let module = Module::read(&data).unwrap();
        let method = module.method_def(rid).unwrap();
        assert_eq!(method.body().unwrap().instructions[7].offset, 0x0e);
        let image = PeImage::parse(&data).unwrap();
        // The 12-byte fat header precedes the code.
        image.resolve_rva(method.rva().unwrap()).unwrap() + 12
    };
    for byte in [0xe1, 0xff] {
        data[code + 0x0e] = byte;
        let module = Module::read(&data).unwrap();
        let body = module.method_def(rid).unwrap().body().unwrap();
        assert_eq!(body.instructions.len(), 7);
        assert!(matches!(
            body.decode_error,
            Some((0x0e, Error::BadImage { .. }))
        ));
        assert_eq!(body.variables.len(), 1);
        assert_eq!(body.exception_handlers.len(), 1);
        assert!(body.formatted().is_err());
    }
}

pub(super) const TESTS: &[Test] = &[
    ("nested_class_cycles", nested_class_cycles),
    (
        "nested_class_rids_out_of_range",
        nested_class_rids_out_of_range,
    ),
    ("member_lists_out_of_range", member_lists_out_of_range),
    ("type_ref_scope_cycles", type_ref_scope_cycles),
    ("base_type_cycles", base_type_cycles),
    (
        "attribute_enum_underlying_cycles",
        attribute_enum_underlying_cycles,
    ),
    ("attribute_argument_nesting", attribute_argument_nesting),
    ("attribute_enum_scope_cycle", attribute_enum_scope_cycle),
    ("exported_type_nesting", exported_type_nesting),
    ("exported_type_file_loop", exported_type_file_loop),
    ("type_spec_chains", type_spec_chains),
    ("opcode_past_the_tables", opcode_past_the_tables),
];
