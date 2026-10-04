//! Custom-attribute and security-declaration blob parsing.
//!
//! Port of `MetadataReader.ReadCustomAttributeSignature`,
//! `SignatureReader.ReadCustomAttribute*` (`AssemblyReader.cs:3508-3721`,
//! including the undocumented `Boxed=0x51`, `Enum=0x55`, `Type=0x50` element
//! types) and `ReadSecurityDeclarationSignature` (both encodings).

use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::handles::{MemberRefSig, MethodDef, TypeDef, TypeRef};
use crate::model::names::{GenericContext, sig_full_name, token_full_name};
use crate::model::sig::{ElementType, SigReader, TypeSig};
use crate::model::typeparser::ParsedType;
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};

/// Resolves an enum's underlying primitive element type from its Cecil-style
/// full name, for enums that live outside this module (Cecil uses its
/// assembly resolver; pass one of these until M7's resolver lands).
pub type EnumResolver<'r> = &'r dyn Fn(&str) -> Option<ElementType>;

/// The type of a custom-attribute argument (Cecil models these as
/// `TypeReference`s; this is the closed set CA blobs can express).
#[derive(Debug, Clone, PartialEq)]
pub enum CaType {
    Boolean,
    Char,
    I1,
    U1,
    I2,
    U2,
    I4,
    U4,
    I8,
    U8,
    R4,
    R8,
    String,
    Object,
    /// System.Type.
    Type,
    Enum {
        full_name: String,
        underlying: Box<CaType>,
    },
    Array(Box<CaType>),
}

impl CaType {
    /// Cecil `TypeReference.FullName` of the argument type.
    pub fn full_name(&self) -> String {
        match self {
            CaType::Boolean => "System.Boolean".into(),
            CaType::Char => "System.Char".into(),
            CaType::I1 => "System.SByte".into(),
            CaType::U1 => "System.Byte".into(),
            CaType::I2 => "System.Int16".into(),
            CaType::U2 => "System.UInt16".into(),
            CaType::I4 => "System.Int32".into(),
            CaType::U4 => "System.UInt32".into(),
            CaType::I8 => "System.Int64".into(),
            CaType::U8 => "System.UInt64".into(),
            CaType::R4 => "System.Single".into(),
            CaType::R8 => "System.Double".into(),
            CaType::String => "System.String".into(),
            CaType::Object => "System.Object".into(),
            CaType::Type => "System.Type".into(),
            CaType::Enum { full_name, .. } => full_name.clone(),
            CaType::Array(elem) => format!("{}[]", elem.full_name()),
        }
    }

    /// The Cecil `ElementType` enum-name (used by the tests' PrettyPrint).
    pub fn etype_name(&self) -> String {
        match self {
            CaType::Boolean => "Boolean".into(),
            CaType::Char => "Char".into(),
            CaType::I1 => "I1".into(),
            CaType::U1 => "U1".into(),
            CaType::I2 => "I2".into(),
            CaType::U2 => "U2".into(),
            CaType::I4 => "I4".into(),
            CaType::U4 => "U4".into(),
            CaType::I8 => "I8".into(),
            CaType::U8 => "U8".into(),
            CaType::R4 => "R4".into(),
            CaType::R8 => "R8".into(),
            CaType::String => "String".into(),
            CaType::Object => "Object".into(),
            CaType::Type => "Type".into(),
            CaType::Enum { full_name, .. } => full_name.clone(),
            CaType::Array(elem) => format!("{}[]", elem.etype_name()),
        }
    }

    fn from_element_type(et: ElementType) -> Result<CaType> {
        Ok(match et {
            ElementType::Boolean => CaType::Boolean,
            ElementType::Char => CaType::Char,
            ElementType::I1 => CaType::I1,
            ElementType::U1 => CaType::U1,
            ElementType::I2 => CaType::I2,
            ElementType::U2 => CaType::U2,
            ElementType::I4 => CaType::I4,
            ElementType::U4 => CaType::U4,
            ElementType::I8 => CaType::I8,
            ElementType::U8 => CaType::U8,
            ElementType::R4 => CaType::R4,
            ElementType::R8 => CaType::R8,
            ElementType::String => CaType::String,
            _ => return Err(Error::Unsupported("not a primitive CA element type")),
        })
    }
}

/// A custom-attribute argument value.
#[derive(Debug, Clone, PartialEq)]
pub enum CaValue<'a> {
    Null,
    Boolean(bool),
    Char(u16),
    I1(i8),
    U1(u8),
    I2(i16),
    U2(u16),
    I4(i32),
    U4(u32),
    I8(i64),
    U8(u64),
    R4(f32),
    R8(f64),
    String(&'a str),
    /// A `typeof(...)` argument: the Cecil-style full name of the type.
    TypeName(String),
    /// A boxed (System.Object-typed) argument.
    Boxed(Box<CaArgument<'a>>),
    Array(Vec<CaArgument<'a>>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaArgument<'a> {
    pub ty: CaType,
    pub value: CaValue<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaNamedArgument<'a> {
    pub name: &'a str,
    pub argument: CaArgument<'a>,
}

/// Parsed custom-attribute values (Cecil `CustomAttribute` after
/// `ReadCustomAttributeSignature`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CaValues<'a> {
    pub constructor_args: Vec<CaArgument<'a>>,
    pub fields: Vec<CaNamedArgument<'a>>,
    pub properties: Vec<CaNamedArgument<'a>>,
}

/// A CustomAttribute table row handle.
#[derive(Clone, Copy)]
pub struct CustomAttribute<'a> {
    pub(crate) m: &'a ModuleData<'a>,
    pub(crate) rid: u32,
}

impl<'a> CustomAttribute<'a> {
    pub fn token(&self) -> MetadataToken {
        MetadataToken::new(TokenType::CustomAttribute, self.rid)
    }

    fn raw(&self) -> Result<CustomAttributeRaw> {
        self.m.md.row::<CustomAttributeRaw>(self.rid)
    }

    pub fn parent(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.parent)
    }

    /// The CustomAttributeType token (MethodDef or MemberRef of the ctor).
    pub fn constructor_token(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.constructor)
    }

    pub fn blob(&self) -> Result<&'a [u8]> {
        self.m.md.blobs().get(self.raw()?.value)
    }

    /// `CustomAttribute.Constructor.FullName`.
    pub fn constructor_full_name(&self) -> Result<String> {
        let ctor = self.constructor_token()?;
        match ctor.token_type() {
            Some(TokenType::Method) => MethodDef {
                m: self.m,
                rid: ctor.rid(),
            }
            .full_name(),
            Some(TokenType::MemberRef) => crate::model::handles::MemberRef {
                m: self.m,
                rid: ctor.rid(),
            }
            .full_name(),
            _ => Err(Error::Unsupported("bad custom attribute constructor")),
        }
    }

    /// `CustomAttribute.AttributeType.FullName` (the ctor's declaring type).
    pub fn attribute_type_full_name(&self) -> Result<String> {
        let ctor = self.constructor_token()?;
        let mut out = String::new();
        match ctor.token_type() {
            Some(TokenType::Method) => {
                let method = MethodDef {
                    m: self.m,
                    rid: ctor.rid(),
                };
                if let Some(t) = method.declaring_type()? {
                    crate::model::names::typedef_full_name(self.m, t.rid(), &mut out)?;
                }
            }
            Some(TokenType::MemberRef) => {
                let mr = crate::model::handles::MemberRef {
                    m: self.m,
                    rid: ctor.rid(),
                };
                token_full_name(
                    self.m,
                    mr.class_token()?,
                    GenericContext::default(),
                    &mut out,
                )?;
            }
            _ => return Err(Error::Unsupported("bad custom attribute constructor")),
        }
        Ok(out)
    }

    /// The attribute type's simple name (last segment).
    pub fn attribute_type_name(&self) -> Result<String> {
        let full = self.attribute_type_full_name()?;
        let base = full.split('<').next().unwrap_or(&full);
        Ok(base.rsplit(['.', '/']).next().unwrap_or(base).to_string())
    }

    pub fn values(&self) -> Result<CaValues<'a>> {
        self.values_with(None)
    }

    /// `ReadCustomAttributeSignature`, with an optional resolver for enums
    /// defined outside this module.
    pub fn values_with(&self, resolver: Option<EnumResolver<'_>>) -> Result<CaValues<'a>> {
        let blob = self.blob()?;
        let mut values = CaValues::default();
        let mut r = SigReader::new(blob);
        if !r.can_read_more() {
            return Ok(values);
        }
        if r.cur.u16()? != 0x0001 {
            return Err(Error::bad(0, "bad custom attribute prolog"));
        }

        let params = self.constructor_param_types()?;
        for param in &params {
            let ty = self.ca_type_of_sig(param, resolver)?;
            values
                .constructor_args
                .push(read_fixed_arg(self.m, &mut r, &ty, resolver)?);
        }

        if !r.can_read_more() {
            return Ok(values);
        }
        let named = r.cur.u16()?;
        read_named_args(self.m, &mut r, named, resolver, &mut values)?;
        Ok(values)
    }

    /// The ctor's parameter types, with generic-attribute type arguments
    /// substituted (`GenericParameterResolver.ResolveParameterTypeIfNeeded`).
    fn constructor_param_types(&self) -> Result<Vec<TypeSig>> {
        let ctor = self.constructor_token()?;
        let (params, class_args): (Vec<TypeSig>, Option<Vec<TypeSig>>) = match ctor.token_type() {
            Some(TokenType::Method) => {
                let method = MethodDef {
                    m: self.m,
                    rid: ctor.rid(),
                };
                (method.signature()?.params, None)
            }
            Some(TokenType::MemberRef) => {
                let mr = crate::model::handles::MemberRef {
                    m: self.m,
                    rid: ctor.rid(),
                };
                let MemberRefSig::Method(sig) = mr.signature()? else {
                    return Err(Error::Unsupported("field MemberRef as CA constructor"));
                };
                let class = mr.class_token()?;
                let args = if class.token_type() == Some(TokenType::TypeSpec) {
                    let row = self.m.md.row::<TypeSpecRaw>(class.rid())?;
                    let spec_blob = self.m.md.blobs().get(row.signature)?;
                    match SigReader::new(spec_blob).type_sig()? {
                        TypeSig::GenericInst { args, .. } => Some(args),
                        _ => None,
                    }
                } else {
                    None
                };
                (sig.params, args)
            }
            _ => return Err(Error::Unsupported("bad custom attribute constructor")),
        };

        Ok(match class_args {
            Some(args) => params
                .into_iter()
                .map(|p| substitute_vars(p, &args))
                .collect(),
            None => params,
        })
    }

    /// Maps a signature type to the CA-blob interpretation
    /// (`ReadCustomAttributeElementValue`'s dispatch).
    fn ca_type_of_sig(&self, sig: &TypeSig, resolver: Option<EnumResolver<'_>>) -> Result<CaType> {
        ca_type_of_sig(self.m, sig, resolver)
    }
}

fn ca_type_of_sig(
    m: &ModuleData<'_>,
    sig: &TypeSig,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaType> {
    Ok(match sig.unwrap_modifiers() {
        TypeSig::Boolean => CaType::Boolean,
        TypeSig::Char => CaType::Char,
        TypeSig::I1 => CaType::I1,
        TypeSig::U1 => CaType::U1,
        TypeSig::I2 => CaType::I2,
        TypeSig::U2 => CaType::U2,
        TypeSig::I4 => CaType::I4,
        TypeSig::U4 => CaType::U4,
        TypeSig::I8 => CaType::I8,
        TypeSig::U8 => CaType::U8,
        TypeSig::R4 => CaType::R4,
        TypeSig::R8 => CaType::R8,
        TypeSig::String => CaType::String,
        TypeSig::Object => CaType::Object,
        TypeSig::SzArray(inner) => CaType::Array(Box::new(ca_type_of_sig(m, inner, resolver)?)),
        TypeSig::Named { token, .. } => {
            if let Some(primitive) = typeref_primitive(m, *token)? {
                return ca_type_of_sig(m, &primitive, resolver);
            }
            let mut full_name = String::new();
            token_full_name(m, *token, GenericContext::default(), &mut full_name)?;
            if full_name == "System.Type" {
                CaType::Type
            } else {
                let underlying = enum_underlying_of_token(m, *token, &full_name, resolver)?;
                CaType::Enum {
                    full_name,
                    underlying: Box::new(underlying),
                }
            }
        }
        TypeSig::GenericInst { ty, .. } => {
            // An enum nested in a generic type: the arguments don't affect the
            // underlying type (`ReadCustomAttributeElementValue`).
            let mut full_name = String::new();
            sig_full_name(m, sig, GenericContext::default(), &mut full_name)?;
            let mut element_name = String::new();
            token_full_name(m, *ty, GenericContext::default(), &mut element_name)?;
            let underlying = enum_underlying_of_token(m, *ty, &element_name, resolver)?;
            CaType::Enum {
                full_name,
                underlying: Box::new(underlying),
            }
        }
        other => {
            let _ = other;
            return Err(Error::Unsupported(
                "unsupported custom attribute argument type",
            ));
        }
    })
}

/// The primitive a TypeRef token names (`TypeRef::primitive`), which Cecil
/// reads as that primitive rather than as an enum.
fn typeref_primitive(m: &ModuleData<'_>, token: MetadataToken) -> Result<Option<TypeSig>> {
    if token.token_type() != Some(TokenType::TypeRef) {
        return Ok(None);
    }
    TypeRef {
        m,
        rid: token.rid(),
    }
    .primitive()
}

/// Underlying primitive of an enum given its TypeDef/TypeRef token
/// (`TypeDefinition.GetEnumUnderlyingType`: the first instance field's type).
fn enum_underlying_of_token(
    m: &ModuleData<'_>,
    token: MetadataToken,
    full_name: &str,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaType> {
    if token.token_type() == Some(TokenType::TypeDef) {
        let ty = TypeDef {
            m,
            rid: token.rid(),
        };
        return enum_underlying_of_def(m, &ty);
    }
    if let Some(et) = resolver.and_then(|r| r(full_name)) {
        return CaType::from_element_type(et);
    }
    // Mirror Cecil's MetadataResolver dichotomy: a TypeRef scoped to another
    // assembly needs the resolver (AssemblyResolutionException, which
    // PROPAGATES out of CA reading); a module-scoped TypeRef (self-referencing
    // scope, netmodule refs) resolves in-module — and a miss there is a
    // ResolutionException, which Cecil SWALLOWS into empty CA values.
    let mut root = token;
    let mut chain: Vec<u32> = Vec::new();
    loop {
        let row = m.md.row::<TypeRefRaw>(root.rid())?;
        chain.push(root.rid());
        if row.scope.token_type() == Some(TokenType::TypeRef) && row.scope.rid() != root.rid() {
            root = row.scope;
        } else {
            if row.scope.token_type() == Some(TokenType::AssemblyRef) {
                return Err(Error::Unsupported(
                    "enum argument requires assembly resolution",
                ));
            }
            break;
        }
    }
    // Module-scoped: find the TypeDef with the same name chain.
    chain.reverse();
    let mut current: Option<TypeDef<'_>> = None;
    for (i, &tr_rid) in chain.iter().enumerate() {
        let row = m.md.row::<TypeRefRaw>(tr_rid)?;
        let ns = m.md.strings().get(row.namespace)?;
        let name = m.md.strings().get(row.name)?;
        current = match current {
            None => {
                let mut found = None;
                for rid in 1..=m.md.row_count(Table::TypeDef) {
                    let t = TypeDef { m, rid };
                    if t.name()? == name && t.namespace()? == ns && !t.is_nested()? {
                        found = Some(t);
                        break;
                    }
                }
                found
            }
            Some(ty) => {
                let mut found = None;
                for nested in ty.nested_types()? {
                    if nested.name()? == name {
                        found = Some(nested);
                        break;
                    }
                }
                found
            }
        };
        if current.is_none() {
            let _ = i;
            return Err(Error::Unsupported("enum type not resolvable in module"));
        }
    }
    match current {
        Some(ty) => enum_underlying_of_def(m, &ty),
        None => Err(Error::Unsupported("enum type not resolvable in module")),
    }
}

fn enum_underlying_of_def(m: &ModuleData<'_>, ty: &TypeDef<'_>) -> Result<CaType> {
    use crate::model::handles::field_attributes;
    for field in ty.fields()? {
        if field.attributes()? & field_attributes::STATIC == 0 {
            return ca_type_of_sig(m, &field.field_type()?, None);
        }
    }
    Err(Error::Unsupported("enum type has no instance field"))
}

/// `ReadCustomAttributeFieldOrPropType`.
fn read_field_or_prop_type<'a>(
    m: &ModuleData<'a>,
    r: &mut SigReader<'a>,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaType> {
    let b = r.cur.u8()?;
    match ElementType::from_byte(b) {
        Some(ElementType::Boxed) => Ok(CaType::Object),
        Some(ElementType::SzArray) => Ok(CaType::Array(Box::new(read_field_or_prop_type(
            m, r, resolver,
        )?))),
        Some(ElementType::Enum) => {
            let name = r
                .utf8_string()?
                .ok_or(Error::Unsupported("null enum type name"))?;
            let parsed =
                ParsedType::parse(name).ok_or(Error::bad(0, "unparseable enum type name"))?;
            let full_name = parsed.cecil_full_name();
            // In-module first, then the resolver.
            let underlying = match resolve_parsed_in_module(m, &parsed)? {
                Some(ty) => enum_underlying_of_def(m, &ty)?,
                None => match resolver.and_then(|res| res(&full_name)) {
                    Some(et) => CaType::from_element_type(et)?,
                    None => {
                        return Err(Error::Unsupported(
                            "enum argument requires assembly resolution",
                        ));
                    }
                },
            };
            Ok(CaType::Enum {
                full_name,
                underlying: Box::new(underlying),
            })
        }
        Some(ElementType::Type) => Ok(CaType::Type),
        Some(et) => CaType::from_element_type(et),
        None => Err(Error::bad(0, "bad field-or-prop element type")),
    }
}

fn resolve_parsed_in_module<'a>(
    m: &'a ModuleData<'a>,
    parsed: &ParsedType,
) -> Result<Option<TypeDef<'a>>> {
    // Inline version of ParsedType::resolve_in_module without needing &Module.
    // As TypeParser.TryCurrentModule: a name qualified with an assembly is
    // in this module only if that is this assembly's full name, exactly.
    if let Some(assembly) = &parsed.assembly
        && !assembly.is_empty()
        && m.assembly_name()?.map(|name| name.full_name()).as_ref() != Some(assembly)
    {
        return Ok(None);
    }
    // Find the top-level type.
    let (ns, name) = crate::model::typeparser::split_full_name(&parsed.fullname);
    let mut found: Option<TypeDef<'a>> = None;
    for rid in 1..=m.md.row_count(Table::TypeDef) {
        let t = TypeDef { m, rid };
        if t.name()? == name && t.namespace()? == ns && !t.is_nested()? {
            found = Some(t);
            break;
        }
    }
    let Some(mut ty) = found else {
        return Ok(None);
    };
    'nested: for nested_name in &parsed.nested_names {
        for nested in ty.nested_types()? {
            if nested.name()? == nested_name {
                ty = nested;
                continue 'nested;
            }
        }
        return Ok(None);
    }
    Ok(Some(ty))
}

/// `ReadCustomAttributeFixedArgument`.
fn read_fixed_arg<'a>(
    m: &ModuleData<'a>,
    r: &mut SigReader<'a>,
    ty: &CaType,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaArgument<'a>> {
    if let CaType::Array(elem) = ty {
        return read_fixed_array_arg(m, r, ty, elem, resolver);
    }
    read_element(m, r, ty, resolver)
}

fn read_fixed_array_arg<'a>(
    m: &ModuleData<'a>,
    r: &mut SigReader<'a>,
    array_ty: &CaType,
    elem: &CaType,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaArgument<'a>> {
    let length = r.cur.u32()?;
    if length == 0xffff_ffff {
        return Ok(CaArgument {
            ty: array_ty.clone(),
            value: CaValue::Null,
        });
    }
    let mut items = Vec::with_capacity((length as usize).min(4096));
    for _ in 0..length {
        items.push(read_element(m, r, elem, resolver)?);
    }
    Ok(CaArgument {
        ty: array_ty.clone(),
        value: CaValue::Array(items),
    })
}

/// `ReadCustomAttributeElement`.
fn read_element<'a>(
    m: &ModuleData<'a>,
    r: &mut SigReader<'a>,
    ty: &CaType,
    resolver: Option<EnumResolver<'_>>,
) -> Result<CaArgument<'a>> {
    if let CaType::Array(elem) = ty {
        return read_fixed_array_arg(m, r, ty, elem, resolver);
    }
    if matches!(ty, CaType::Object) {
        // Boxed: the actual type precedes the value.
        let inner_ty = read_field_or_prop_type(m, r, resolver)?;
        let inner = read_element(m, r, &inner_ty, resolver)?;
        return Ok(CaArgument {
            ty: CaType::Object,
            value: CaValue::Boxed(Box::new(inner)),
        });
    }
    let value = read_element_value(r, ty)?;
    Ok(CaArgument {
        ty: ty.clone(),
        value,
    })
}

/// `ReadCustomAttributeElementValue` + `ReadPrimitiveValue`.
fn read_element_value<'a>(r: &mut SigReader<'a>, ty: &CaType) -> Result<CaValue<'a>> {
    Ok(match ty {
        CaType::String => match r.utf8_string()? {
            Some(s) => CaValue::String(s),
            None => CaValue::Null,
        },
        CaType::Type => match r.utf8_string()? {
            Some(s) => {
                let parsed = ParsedType::parse(s).ok_or(Error::bad(0, "unparseable type name"))?;
                // Cecil resolves in-module definitions; the display name is
                // the same either way.
                CaValue::TypeName(parsed.cecil_full_name())
            }
            None => CaValue::Null,
        },
        CaType::Enum { underlying, .. } => read_element_value(r, underlying)?,
        CaType::Boolean => CaValue::Boolean(r.cur.u8()? == 1),
        CaType::Char => CaValue::Char(r.cur.u16()?),
        CaType::I1 => CaValue::I1(r.cur.i8()?),
        CaType::U1 => CaValue::U1(r.cur.u8()?),
        CaType::I2 => CaValue::I2(r.cur.i16()?),
        CaType::U2 => CaValue::U2(r.cur.u16()?),
        CaType::I4 => CaValue::I4(r.cur.i32()?),
        CaType::U4 => CaValue::U4(r.cur.u32()?),
        CaType::I8 => CaValue::I8(r.cur.i64()?),
        CaType::U8 => CaValue::U8(r.cur.u64()?),
        CaType::R4 => CaValue::R4(r.cur.f32()?),
        CaType::R8 => CaValue::R8(r.cur.f64()?),
        CaType::Object | CaType::Array(_) => unreachable!("handled by read_element"),
    })
}

/// `ReadCustomAttributeNamedArguments`.
fn read_named_args<'a>(
    m: &ModuleData<'a>,
    r: &mut SigReader<'a>,
    count: u16,
    resolver: Option<EnumResolver<'_>>,
    values: &mut CaValues<'a>,
) -> Result<()> {
    for _ in 0..count {
        if !r.can_read_more() {
            return Ok(());
        }
        let kind = r.cur.u8()?;
        let ty = read_field_or_prop_type(m, r, resolver)?;
        let name = r
            .utf8_string()?
            .ok_or(Error::Unsupported("null named argument name"))?;
        let argument = read_fixed_arg(m, r, &ty, resolver)?;
        let named = CaNamedArgument { name, argument };
        match kind {
            0x53 => values.fields.push(named),
            0x54 => values.properties.push(named),
            _ => return Err(Error::bad(0, "bad named argument kind")),
        }
    }
    Ok(())
}

/// Substitutes `Var(n)` with the declaring generic instance's arguments
/// (generic attributes).
fn substitute_vars(sig: TypeSig, args: &[TypeSig]) -> TypeSig {
    match sig {
        TypeSig::Var(n) => args.get(n as usize).cloned().unwrap_or(TypeSig::Var(n)),
        TypeSig::Ptr(inner) => TypeSig::Ptr(Box::new(substitute_vars(*inner, args))),
        TypeSig::ByRef(inner) => TypeSig::ByRef(Box::new(substitute_vars(*inner, args))),
        TypeSig::Pinned(inner) => TypeSig::Pinned(Box::new(substitute_vars(*inner, args))),
        TypeSig::SzArray(inner) => TypeSig::SzArray(Box::new(substitute_vars(*inner, args))),
        TypeSig::Array {
            element,
            dimensions,
        } => TypeSig::Array {
            element: Box::new(substitute_vars(*element, args)),
            dimensions,
        },
        TypeSig::GenericInst {
            is_value_type,
            ty,
            args: inst_args,
        } => TypeSig::GenericInst {
            is_value_type,
            ty,
            args: inst_args
                .into_iter()
                .map(|a| substitute_vars(a, args))
                .collect(),
        },
        TypeSig::CModReq { modifier, inner } => TypeSig::CModReq {
            modifier,
            inner: Box::new(substitute_vars(*inner, args)),
        },
        TypeSig::CModOpt { modifier, inner } => TypeSig::CModOpt {
            modifier,
            inner: Box::new(substitute_vars(*inner, args)),
        },
        TypeSig::Sentinel(inner) => TypeSig::Sentinel(Box::new(substitute_vars(*inner, args))),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Security declarations
// ---------------------------------------------------------------------------

/// One security attribute inside a declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityAttribute<'a> {
    /// The attribute type's Cecil-style full name.
    pub attribute_type: String,
    pub fields: Vec<CaNamedArgument<'a>>,
    pub properties: Vec<CaNamedArgument<'a>>,
}

/// The decoded contents of a DeclSecurity row.
#[derive(Debug, Clone, PartialEq)]
pub enum SecurityDeclarationValue<'a> {
    /// The 2.0 attribute encoding ('.'-prefixed).
    Attributes(Vec<SecurityAttribute<'a>>),
    /// The legacy .NET 1.1 XML form; Cecil surfaces it as a synthesized
    /// `PermissionSetAttribute` with an `XML` property.
    Xml(String),
}

/// A DeclSecurity table row handle.
#[derive(Clone, Copy)]
pub struct SecurityDeclaration<'a> {
    pub(crate) m: &'a ModuleData<'a>,
    pub(crate) rid: u32,
}

impl<'a> SecurityDeclaration<'a> {
    pub fn token(&self) -> MetadataToken {
        MetadataToken::new(TokenType::Permission, self.rid)
    }

    fn raw(&self) -> Result<DeclSecurityRaw> {
        self.m.md.row::<DeclSecurityRaw>(self.rid)
    }

    /// The `SecurityAction` value.
    pub fn action(&self) -> Result<u16> {
        Ok(self.raw()?.action)
    }

    pub fn parent(&self) -> Result<MetadataToken> {
        Ok(self.raw()?.parent)
    }

    pub fn blob(&self) -> Result<&'a [u8]> {
        self.m.md.blobs().get(self.raw()?.permission_set)
    }

    /// `ReadSecurityDeclarationSignature` — both encodings.
    pub fn value(&self) -> Result<SecurityDeclarationValue<'a>> {
        self.value_with(None)
    }

    pub fn value_with(
        &self,
        resolver: Option<EnumResolver<'_>>,
    ) -> Result<SecurityDeclarationValue<'a>> {
        let blob = self.blob()?;
        if blob.first() != Some(&b'.') {
            // Legacy XML: the blob is a UTF-16LE string.
            let units: Vec<u16> = blob
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Ok(SecurityDeclarationValue::Xml(String::from_utf16_lossy(
                &units,
            )));
        }
        let mut r = SigReader::new(blob);
        r.cur.skip(1)?; // '.'
        let count = r.cur.compressed_u32()?;
        let mut attributes = Vec::with_capacity((count as usize).min(64));
        for _ in 0..count {
            // ReadSecurityAttribute: type name, attribute blob size (ignored),
            // named-argument count, named arguments.
            let name = r
                .utf8_string()?
                .ok_or(Error::Unsupported("null security attribute type"))?;
            let parsed =
                ParsedType::parse(name).ok_or(Error::bad(0, "unparseable security attribute"))?;
            r.cur.compressed_u32()?; // blob size
            let named = r.cur.compressed_u32()? as u16;
            let mut values = CaValues::default();
            read_named_args(self.m, &mut r, named, resolver, &mut values)?;
            attributes.push(SecurityAttribute {
                attribute_type: parsed.cecil_full_name(),
                fields: values.fields,
                properties: values.properties,
            });
        }
        Ok(SecurityDeclarationValue::Attributes(attributes))
    }
}
