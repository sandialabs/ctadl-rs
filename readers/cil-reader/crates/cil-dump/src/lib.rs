//! Deterministic text dump of a CIL image.
//!
//! **The format is the contract**: `tools/CecilDump` implements the same
//! format on top of Mono.Cecil, and the root workspace's `cargo xtask
//! regression --frontend cil` diffs the two.
//! Everything is ordered by metadata token; floats print as bit patterns (IL
//! float operands also print first as the test Formatter renders them);
//! opaque blobs print as lowercase hex.

use cil_reader::cil::RawOperand;
use cil_reader::model::AssemblyName;
use cil_reader::model::custom_attributes::{CaArgument, CaValue, UNRESOLVED_ENUM};
use cil_reader::model::resolver::{
    AssemblyCache, AssemblyLoader, DirectoryLoader, ResolvedMember, ResolvedType,
};
use cil_reader::model::{ConstantValue, Entity, ExceptionHandlerType, Module, Resource, TypeDef};
use cil_reader::raw::{ModuleKind, Table, TargetArchitecture};
use cil_reader::{MetadataToken, Result, TokenType};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Module,
    Assembly,
    AssemblyRefs,
    ModuleRefs,
    Resources,
    ExportedTypes,
    Types,
    Fields,
    Methods,
    Properties,
    Events,
    CustomAttributes,
    Il,
    Resolve,
}

pub const ALL_SECTIONS: &[(&str, Section)] = &[
    ("module", Section::Module),
    ("assembly", Section::Assembly),
    ("assembly-refs", Section::AssemblyRefs),
    ("module-refs", Section::ModuleRefs),
    ("resources", Section::Resources),
    ("exported-types", Section::ExportedTypes),
    ("types", Section::Types),
    ("fields", Section::Fields),
    ("methods", Section::Methods),
    ("properties", Section::Properties),
    ("events", Section::Events),
    ("custom-attributes", Section::CustomAttributes),
    ("il", Section::Il),
    ("resolve", Section::Resolve),
];

fn hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "-".to_string();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn tok(t: MetadataToken) -> String {
    format!("0x{:08x}", t.raw())
}

/// Renders a dump of `module` for the selected sections. `dir` is the
/// directory [`Section::Resolve`] probes for referenced assemblies (the
/// input's own); without one, that section is left out. Custom attributes
/// load the assembly's netmodules from it.
pub fn dump(module: &Module<'_>, sections: &[Section], dir: Option<&Path>) -> Result<String> {
    let mut out = String::new();
    let has = |s: Section| sections.contains(&s);

    if has(Section::Module) {
        dump_module(module, &mut out)?;
    }
    if has(Section::Assembly) {
        dump_assembly(module, &mut out)?;
    }
    if has(Section::AssemblyRefs) {
        out.push_str("[assembly-refs]\n");
        for r in module.assembly_references()? {
            writeln!(
                out,
                "{}={} flags=0x{:08x}",
                tok(r.token),
                r.full_name(),
                r.attributes
            )
            .unwrap();
        }
    }
    if has(Section::ModuleRefs) {
        out.push_str("[module-refs]\n");
        for r in module.module_references()? {
            writeln!(out, "{}={}", tok(r.token), r.name).unwrap();
        }
    }
    if has(Section::Resources) {
        dump_resources(module, &mut out)?;
    }
    if has(Section::ExportedTypes) {
        out.push_str("[exported-types]\n");
        for et in module.exported_types() {
            writeln!(
                out,
                "{}={} scope={} flags=0x{:08x}",
                tok(et.token()),
                et.full_name()?,
                et.scope_name().unwrap_or("!error"),
                et.attributes()?,
            )
            .unwrap();
        }
    }
    if has(Section::Types) {
        dump_types(module, &mut out)?;
    }
    if has(Section::Fields) {
        dump_fields(module, &mut out)?;
    }
    if has(Section::Methods) {
        dump_methods(module, &mut out)?;
    }
    if has(Section::Properties) {
        dump_properties(module, &mut out)?;
    }
    if has(Section::Events) {
        dump_events(module, &mut out)?;
    }
    if has(Section::CustomAttributes) {
        dump_custom_attributes(module, dir, &mut out)?;
    }
    if has(Section::Il) {
        dump_il(module, &mut out)?;
    }
    if let (true, Some(dir)) = (has(Section::Resolve), dir) {
        dump_resolve(module, dir, &mut out);
    }
    Ok(out)
}

/// Every TypeRef, TypeSpec and MemberRef row, and the definition it resolves
/// to through a [`DirectoryLoader`] over `dir`: `-` for none, `!error` for an
/// error.
fn dump_resolve(module: &Module<'_>, dir: &Path, out: &mut String) {
    out.push_str("[resolve]\n");
    let loader = DirectoryLoader::new(vec![dir.to_path_buf()]);
    let cache = AssemblyCache::new(&loader);
    let md = module.metadata();
    let types = [
        (TokenType::TypeRef, Table::TypeRef),
        (TokenType::TypeSpec, Table::TypeSpec),
    ]
    .into_iter()
    .flat_map(|(t, table)| (1..=md.row_count(table)).map(move |rid| MetadataToken::new(t, rid)));
    for token in types {
        let result = module.resolve_type(token, &cache).and_then(|r| {
            r.map(|ResolvedType { module, ty }| definition(module, ty.token(), ty.full_name()))
                .transpose()
        });
        writeln!(out, "{} -> {}", tok(token), resolution(result)).unwrap();
    }
    for rid in 1..=md.row_count(Table::MemberRef) {
        let token = MetadataToken::new(TokenType::MemberRef, rid);
        let result = module.resolve_member(token, &cache).and_then(|r| {
            r.map(|member| match member {
                ResolvedMember::Field(module, f) => definition(module, f.token(), f.full_name()),
                ResolvedMember::Method(module, m) => definition(module, m.token(), m.full_name()),
            })
            .transpose()
        });
        writeln!(out, "{} -> {}", tok(token), resolution(result)).unwrap();
    }
}

fn definition(
    module: &Module<'_>,
    token: MetadataToken,
    full_name: Result<String>,
) -> Result<String> {
    Ok(format!("{}!{} {}", module.name()?, tok(token), full_name?))
}

fn resolution(result: Result<Option<String>>) -> String {
    match result {
        Ok(Some(definition)) => definition,
        Ok(None) => "-".to_string(),
        Err(_) => "!error".to_string(),
    }
}

fn dump_module(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[module]\n");
    writeln!(out, "name={}", module.name()?).unwrap();
    writeln!(out, "mvid={}", module.mvid()?).unwrap();
    let kind = match module.kind() {
        ModuleKind::Dll => "Dll",
        ModuleKind::Console => "Console",
        ModuleKind::Windows => "Windows",
        ModuleKind::NetModule => "NetModule",
    };
    writeln!(out, "kind={kind}").unwrap();
    writeln!(out, "runtime={}", module.runtime_version()).unwrap();
    let arch = match module.architecture() {
        TargetArchitecture::I386 => "I386".to_string(),
        TargetArchitecture::Amd64 => "AMD64".to_string(),
        TargetArchitecture::Ia64 => "IA64".to_string(),
        TargetArchitecture::Arm => "ARM".to_string(),
        TargetArchitecture::Armv7 => "ARMv7".to_string(),
        TargetArchitecture::Arm64 => "ARM64".to_string(),
        TargetArchitecture::Unknown(v) => format!("0x{v:04x}"),
    };
    writeln!(out, "architecture={arch}").unwrap();
    writeln!(out, "module-attributes=0x{:08x}", module.attributes()).unwrap();
    match module.entry_point()? {
        Some(ep) => writeln!(out, "entry-point={}", tok(ep.token())).unwrap(),
        None => out.push_str("entry-point=-\n"),
    }
    Ok(())
}

fn dump_assembly(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[assembly]\n");
    let Some(name) = module.assembly_name()? else {
        return Ok(());
    };
    writeln!(out, "full-name={}", name.full_name()).unwrap();
    writeln!(out, "hash-algorithm=0x{:08x}", name.hash_algorithm).unwrap();
    writeln!(out, "flags=0x{:08x}", name.attributes).unwrap();
    writeln!(out, "public-key={}", hex(name.public_key)).unwrap();
    Ok(())
}

fn dump_resources(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[resources]\n");
    for r in module.resources()? {
        match &r {
            Resource::Embedded {
                name,
                attributes,
                offset,
            } => {
                let size = module
                    .resource_data(*offset)
                    .map(|d| d.len() as i64)
                    .unwrap_or(-1);
                writeln!(
                    out,
                    "embedded name={name} flags=0x{attributes:08x} size={size}"
                )
                .unwrap();
            }
            Resource::Linked {
                name,
                attributes,
                file,
                ..
            } => {
                writeln!(
                    out,
                    "linked name={name} flags=0x{attributes:08x} file={file}"
                )
                .unwrap();
            }
            Resource::AssemblyLinked {
                name,
                attributes,
                assembly,
            } => {
                writeln!(
                    out,
                    "assembly-linked name={name} flags=0x{attributes:08x} assembly={}",
                    assembly.full_name()
                )
                .unwrap();
            }
        }
    }
    Ok(())
}

fn dump_types(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[types]\n");
    for ty in module.all_types() {
        writeln!(
            out,
            "{}={} flags=0x{:08x}{}{}",
            tok(ty.token()),
            ty.full_name()?,
            ty.attributes()?,
            if ty.is_value_type()? {
                " valuetype"
            } else {
                ""
            },
            if ty.is_enum()? { " enum" } else { "" },
        )
        .unwrap();
        if let Some(base) = ty.base_type()? {
            writeln!(out, "\tbase={}", base.full_name_ctx(ty.generic_context())?).unwrap();
        }
        if let Some((packing, size)) = ty.class_layout()? {
            writeln!(out, "\tlayout packing={packing} size={size}").unwrap();
        }
        for iface in ty.interfaces()? {
            writeln!(
                out,
                "\tinterface={}",
                iface.interface.full_name_ctx(ty.generic_context())?
            )
            .unwrap();
        }
        dump_generic_params(module, &ty, out)?;
    }
    Ok(())
}

fn dump_generic_params(_module: &Module<'_>, ty: &TypeDef<'_>, out: &mut String) -> Result<()> {
    for gp in ty.generic_params()? {
        writeln!(
            out,
            "\tgeneric-param {} name={} flags=0x{:04x}",
            gp.number()?,
            gp.name()?,
            gp.attributes()?,
        )
        .unwrap();
        for c in gp.constraints()? {
            writeln!(
                out,
                "\t\tconstraint={}",
                c.constraint.full_name_ctx(ty.generic_context())?
            )
            .unwrap();
        }
    }
    Ok(())
}

fn dump_fields(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[fields]\n");
    for rid in 1..=module.metadata().row_count(Table::Field) {
        let Some(field) = module.field_def(rid) else {
            continue;
        };
        if field.is_deleted().unwrap_or(false) {
            continue; // as CecilDump, which reads members through their types
        }
        let full_name = field.full_name().unwrap_or_else(|_| "!error".into());
        write!(
            out,
            "{}={} flags=0x{:04x}",
            tok(field.token()),
            full_name,
            field.attributes()?,
        )
        .unwrap();
        if let Some(offset) = field.offset()? {
            write!(out, " offset={offset}").unwrap();
        }
        if let Some(rva) = field.rva()? {
            let data = field.initial_value()?.unwrap_or(&[]);
            write!(out, " rva=0x{rva:08x} rva-data={}", hex(data)).unwrap();
        }
        if let Some(constant) = field.constant()? {
            write!(out, " constant={}", format_constant(&constant)).unwrap();
        }
        if let Some(marshal) = field.marshal_info()? {
            write!(out, " marshal={}", format_marshal(&marshal)).unwrap();
        }
        out.push('\n');
    }
    Ok(())
}

fn format_constant(c: &ConstantValue<'_>) -> String {
    match c {
        ConstantValue::Boolean(v) => format!("Boolean:{}", if *v { "True" } else { "False" }),
        ConstantValue::Char(v) => format!("Char:{v}"),
        ConstantValue::I1(v) => format!("I1:{v}"),
        ConstantValue::U1(v) => format!("U1:{v}"),
        ConstantValue::I2(v) => format!("I2:{v}"),
        ConstantValue::U2(v) => format!("U2:{v}"),
        ConstantValue::I4(v) => format!("I4:{v}"),
        ConstantValue::U4(v) => format!("U4:{v}"),
        ConstantValue::I8(v) => format!("I8:{v}"),
        ConstantValue::U8(v) => format!("U8:{v}"),
        ConstantValue::R4(v) => format!("R4:0x{:08x}", v.to_bits()),
        ConstantValue::R8(v) => format!("R8:0x{:016x}", v.to_bits()),
        ConstantValue::String(s) => format!("String:\"{}\"", s.to_string_lossy()),
        ConstantValue::Null => "Null".to_string(),
    }
}

fn format_marshal(info: &cil_reader::model::MarshalInfo<'_>) -> String {
    use cil_reader::model::MarshalInfo as M;
    match info {
        M::Array {
            element_type,
            size,
            size_parameter_index,
            size_parameter_multiplier,
        } => format!(
            "array(elem=0x{element_type:02x},size={size},index={size_parameter_index},mult={size_parameter_multiplier})"
        ),
        M::SafeArray { element_type } => format!("safearray(0x{element_type:02x})"),
        M::FixedArray { size, element_type } => {
            format!("fixedarray(size={size},elem=0x{element_type:02x})")
        }
        M::FixedSysString { size } => format!("fixedsysstring({size})"),
        M::CustomMarshaler {
            guid,
            unmanaged_type,
            managed_type,
            cookie,
        } => {
            // The model's parsed GUID prints in .NET's "d" format, as is;
            // the managed type prints as its parsed FullName, like CecilDump.
            let managed = cil_reader::model::ParsedType::parse(managed_type)
                .map(|p| p.cecil_full_name())
                .unwrap_or_default();
            format!(
                "custom(guid={guid},unmanaged={unmanaged_type},managed={managed},cookie={cookie})"
            )
        }
        M::Simple { native } => format!("native(0x{native:02x})"),
    }
}

fn dump_methods(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[methods]\n");
    for rid in 1..=module.metadata().row_count(Table::Method) {
        let Some(method) = module.method_def(rid) else {
            continue;
        };
        if method.is_deleted().unwrap_or(false) {
            continue; // as CecilDump, which reads members through their types
        }
        let full_name = method.full_name().unwrap_or_else(|_| "!error".into());
        write!(
            out,
            "{}={} flags=0x{:04x} impl=0x{:04x} rva=0x{:08x}",
            tok(method.token()),
            full_name,
            method.attributes()?,
            method.impl_attributes()?,
            method.rva()?,
        )
        .unwrap();
        let semantics = method.semantics()?;
        if semantics != 0 {
            write!(out, " semantics=0x{semantics:04x}").unwrap();
        }
        if let Some(pinvoke) = method.pinvoke_info()? {
            write!(
                out,
                " pinvoke=({},{},0x{:04x})",
                pinvoke.module, pinvoke.entry_point, pinvoke.attributes
            )
            .unwrap();
        }
        out.push('\n');
        for gp in method.generic_params()? {
            writeln!(
                out,
                "\tgeneric-param {} name={} flags=0x{:04x}",
                gp.number()?,
                gp.name()?,
                gp.attributes()?,
            )
            .unwrap();
        }
        let method_ctx = method.generic_context()?;
        for over in method.overrides()? {
            writeln!(
                out,
                "\toverride={}",
                operand_name_ctx(module, over, method_ctx)
            )
            .unwrap();
        }
        let sig = method.signature()?;
        let has_implicit_this = sig.has_this && !sig.explicit_this;
        for p in method.parameters()? {
            if p.token.is_some() {
                // ParameterDefinition.Sequence: index+1 with an implicit this.
                let seq = if has_implicit_this {
                    p.index + 1
                } else {
                    p.index
                };
                write!(out, "\tparam {seq} name={}", p.name).unwrap();
                write!(out, " flags=0x{:04x}", p.attributes).unwrap();
                if let Some(c) = p.constant()? {
                    write!(out, " constant={}", format_constant(&c)).unwrap();
                }
                if let Some(marshal) = p.marshal_info()? {
                    write!(out, " marshal={}", format_marshal(&marshal)).unwrap();
                }
                out.push('\n');
            }
        }
    }
    Ok(())
}

fn operand_name_ctx(
    module: &Module<'_>,
    token: MetadataToken,
    ctx: cil_reader::model::GenericContext,
) -> String {
    let render = || -> Result<String> {
        Ok(match module.lookup_token(token)? {
            Some(Entity::TypeDef(t)) => t.full_name()?,
            Some(Entity::TypeRef(t)) => t.full_name()?,
            Some(Entity::TypeSpec(t)) => t.full_name_ctx(ctx)?,
            Some(Entity::Field(f)) => f.full_name()?,
            Some(Entity::Method(m)) => m.full_name()?,
            Some(Entity::MemberRef(m)) => m.full_name_with_outer(ctx)?,
            Some(Entity::MethodSpec(m)) => m.full_name_with_outer(ctx)?,
            None => tok(token),
        })
    };
    render().unwrap_or_else(|_| "!error".into())
}

fn dump_properties(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[properties]\n");
    for rid in 1..=module.metadata().row_count(Table::Property) {
        let Some(prop) = module.property_def(rid) else {
            continue;
        };
        if prop.is_deleted().unwrap_or(false) {
            continue; // as CecilDump, which reads members through their types
        }
        let full_name = prop.full_name().unwrap_or_else(|_| "!error".into());
        write!(
            out,
            "{}={} flags=0x{:04x}",
            tok(prop.token()),
            full_name,
            prop.attributes()?,
        )
        .unwrap();
        if let Some(g) = prop.get_method()? {
            write!(out, " getter={}", tok(g.token())).unwrap();
        }
        if let Some(s) = prop.set_method()? {
            write!(out, " setter={}", tok(s.token())).unwrap();
        }
        write_other_methods(&prop.other_methods()?, out);
        if let Some(c) = prop.constant()? {
            write!(out, " constant={}", format_constant(&c)).unwrap();
        }
        out.push('\n');
    }
    Ok(())
}

fn dump_events(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[events]\n");
    for rid in 1..=module.metadata().row_count(Table::Event) {
        let Some(event) = module.event_def(rid) else {
            continue;
        };
        if event.is_deleted().unwrap_or(false) {
            continue; // as CecilDump, which reads members through their types
        }
        write!(
            out,
            "{}={} flags=0x{:04x}",
            tok(event.token()),
            event.name()?,
            event.attributes()?,
        )
        .unwrap();
        if let Some(t) = event.event_type()? {
            let ctx = event
                .declaring_type()?
                .map(|d| d.generic_context())
                .unwrap_or_default();
            write!(
                out,
                " type={}",
                t.full_name_ctx(ctx).unwrap_or_else(|_| "!error".into())
            )
            .unwrap();
        }
        if let Some(m) = event.add_method()? {
            write!(out, " add={}", tok(m.token())).unwrap();
        }
        if let Some(m) = event.remove_method()? {
            write!(out, " remove={}", tok(m.token())).unwrap();
        }
        if let Some(m) = event.invoke_method()? {
            write!(out, " fire={}", tok(m.token())).unwrap();
        }
        write_other_methods(&event.other_methods()?, out);
        out.push('\n');
    }
    Ok(())
}

fn write_other_methods(methods: &[cil_reader::model::MethodDef<'_>], out: &mut String) {
    if methods.is_empty() {
        return;
    }
    let toks: Vec<String> = methods.iter().map(|m| tok(m.token())).collect();
    write!(out, " other={}", toks.join(",")).unwrap();
}

/// CecilDump's assembly resolver outside `[resolve]`, which throws for
/// every assembly; Cecil still reads an assembly's netmodules from its
/// directory.
struct NetmodulesOnly(DirectoryLoader);

impl AssemblyLoader for NetmodulesOnly {
    fn load_assembly(&self, _: &AssemblyName<'_>) -> Option<Vec<u8>> {
        None
    }

    fn load_file(&self, file_name: &str) -> Option<Vec<u8>> {
        self.0.load_file(file_name)
    }
}

fn dump_custom_attributes(module: &Module<'_>, dir: Option<&Path>, out: &mut String) -> Result<()> {
    use cil_reader::TokenType as TT;
    out.push_str("[custom-attributes]\n");
    let loader = NetmodulesOnly(DirectoryLoader::new(
        dir.into_iter().map(Path::to_path_buf).collect(),
    ));
    let cache = AssemblyCache::throwing(&loader);
    // Restricted to parents Cecil's object model can surface (a CA row may
    // legally hang off e.g. a TypeRef, which Cecil cannot reach); rids are
    // omitted because Cecil does not expose them.
    const VISIBLE: &[TT] = &[
        TT::Module,
        TT::Assembly,
        TT::TypeDef,
        TT::Field,
        TT::Method,
        TT::Param,
        TT::InterfaceImpl,
        TT::Property,
        TT::Event,
        TT::GenericParam,
        TT::GenericParamConstraint,
    ];
    for rid in 1..=module.metadata().row_count(Table::CustomAttribute) {
        let Some(attr) = module.custom_attribute(rid) else {
            continue;
        };
        let parent = attr.parent()?;
        if !parent.token_type().is_some_and(|tt| VISIBLE.contains(&tt)) {
            continue;
        }
        let ctor = attr
            .constructor_full_name()
            .unwrap_or_else(|_| "!error".into());
        write!(out, "ca parent={} ctor={}", tok(parent), ctor).unwrap();
        match attr.values_with(&cache) {
            Ok(values) => {
                let mut s = String::new();
                s.push('(');
                for (i, arg) in values.constructor_args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    pp_argument(arg, &mut s);
                }
                s.push(')');
                for f in &values.fields {
                    write!(s, " field {}=", f.name).unwrap();
                    pp_argument(&f.argument, &mut s);
                }
                for p in &values.properties {
                    write!(s, " property {}=", p.name).unwrap();
                    pp_argument(&p.argument, &mut s);
                }
                write!(out, " args={s}").unwrap();
            }
            // Cecil swallows ResolutionException (an enum type resolved to
            // null) while reading CA values and reports empty argument
            // collections (CustomAttribute.Resolve) — but an
            // AssemblyResolutionException propagates. Mirror both.
            Err(e) if e == UNRESOLVED_ENUM => {
                write!(out, " args=()").unwrap();
            }
            Err(_) => {
                write!(out, " args=!unresolved blob={}", hex(attr.blob()?)).unwrap();
            }
        }
        out.push('\n');
    }
    Ok(())
}

fn pp_argument(arg: &CaArgument<'_>, s: &mut String) {
    s.push('(');
    s.push_str(&arg.ty.etype_name());
    s.push(':');
    pp_value(&arg.value, s);
    s.push(')');
}

fn pp_value(value: &CaValue<'_>, s: &mut String) {
    match value {
        CaValue::Null => s.push_str("null"),
        CaValue::Array(items) => {
            s.push('{');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                pp_argument(item, s);
            }
            s.push('}');
        }
        CaValue::String(v) => write!(s, "\"{v}\"").unwrap(),
        CaValue::Char(c) => write!(s, "char:0x{c:04x}").unwrap(),
        CaValue::Boxed(inner) => pp_argument(inner, s),
        CaValue::Boolean(b) => s.push_str(if *b { "True" } else { "False" }),
        CaValue::I1(v) => write!(s, "{v}").unwrap(),
        CaValue::U1(v) => write!(s, "{v}").unwrap(),
        CaValue::I2(v) => write!(s, "{v}").unwrap(),
        CaValue::U2(v) => write!(s, "{v}").unwrap(),
        CaValue::I4(v) => write!(s, "{v}").unwrap(),
        CaValue::U4(v) => write!(s, "{v}").unwrap(),
        CaValue::I8(v) => write!(s, "{v}").unwrap(),
        CaValue::U8(v) => write!(s, "{v}").unwrap(),
        CaValue::R4(v) => write!(s, "r4:0x{:08x}", v.to_bits()).unwrap(),
        CaValue::R8(v) => write!(s, "r8:0x{:016x}", v.to_bits()).unwrap(),
        CaValue::TypeName(name) => write!(s, "typeof({name})").unwrap(),
    }
}

fn dump_il(module: &Module<'_>, out: &mut String) -> Result<()> {
    out.push_str("[il]\n");
    for rid in 1..=module.metadata().row_count(Table::Method) {
        let Some(method) = module.method_def(rid) else {
            continue;
        };
        if !method.has_body()? || method.rva()? == 0 || method.is_deleted().unwrap_or(false) {
            continue;
        }
        let body = match method.body() {
            Ok(b) => b,
            Err(_) => {
                writeln!(out, "{} !error", tok(method.token())).unwrap();
                continue;
            }
        };
        writeln!(
            out,
            "{} max-stack={} init-locals={} code-size={}",
            tok(method.token()),
            body.max_stack,
            body.init_locals,
            body.code_size,
        )
        .unwrap();
        for (i, var) in body.variables.iter().enumerate() {
            let name = module
                .type_sig_full_name(var, method.generic_context()?)
                .unwrap_or_else(|_| "!error".into());
            writeln!(out, "\tlocal {i}={name}").unwrap();
        }
        let is_offset = |offset: u32| {
            body.instructions
                .binary_search_by_key(&offset, |i| i.offset)
                .is_ok()
        };
        let label_or_end = |offset: u32| {
            if is_offset(offset) {
                format!("IL_{offset:04x}")
            } else {
                "IL_END".to_string()
            }
        };
        for ins in &body.instructions {
            // Float operands print as the Formatter renders them, then as bit
            // patterns; switch targets outside the method print IL_END
            // (Cecil's resolved Instruction[] holds nulls there); everything
            // else uses the Formatter rendering.
            let formatted = || {
                body.format_instruction(ins).unwrap_or_else(|_| {
                    format!("IL_{:04x}: {} !error", ins.offset, ins.opcode.name())
                })
            };
            match &ins.operand {
                RawOperand::F32(v) => {
                    writeln!(out, "\t{} r4:0x{:08x}", formatted(), v.to_bits()).unwrap();
                }
                RawOperand::F64(v) => {
                    writeln!(out, "\t{} r8:0x{:016x}", formatted(), v.to_bits()).unwrap();
                }
                RawOperand::Switch(targets) => {
                    write!(out, "\tIL_{:04x}: {} (", ins.offset, ins.opcode.name()).unwrap();
                    for (i, t) in targets.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&label_or_end(t));
                    }
                    out.push_str(")\n");
                }
                _ => writeln!(out, "\t{}", formatted()).unwrap(),
            }
        }
        // The catch type is a TypeSpec in generic code, so it needs the
        // method's generic context to print parameter names rather than
        // !0/!!0 (Cecil's Formatter does).
        let method_ctx = method.generic_context()?;
        for h in &body.exception_handlers {
            let kind = match h.handler_type {
                ExceptionHandlerType::Catch => {
                    let catch_type = h
                        .catch_type
                        .map(|t| operand_name_ctx(module, t, method_ctx))
                        .unwrap_or_else(|| "-".into());
                    format!("catch {catch_type}")
                }
                ExceptionHandlerType::Filter => {
                    format!("filter {}", label_or_end(h.filter_start.unwrap_or(0)))
                }
                ExceptionHandlerType::Finally => "finally".to_string(),
                ExceptionHandlerType::Fault => "fault".to_string(),
            };
            writeln!(
                out,
                "\t.try {} to {} {} handler {} to {}",
                label_or_end(h.try_start),
                label_or_end(h.try_end),
                kind,
                label_or_end(h.handler_start),
                label_or_end(h.handler_end)
            )
            .unwrap();
        }
    }
    Ok(())
}

/// The default full-read sweep: touch everything that decodes lazily.
pub fn sweep(module: &Module<'_>) -> Result<()> {
    let _ = dump(
        module,
        &ALL_SECTIONS.iter().map(|&(_, s)| s).collect::<Vec<_>>(),
        None,
    )?;
    Ok(())
}
