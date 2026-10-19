//! `FullName` rendering with Cecil parity.
//!
//! Ports the rules spread across Cecil's `TypeReference.FullName`, the
//! `TypeSpecification` subclasses, `MethodReference.FullName`,
//! `IMethodSignature.MethodSignatureFullName` and
//! `IGenericInstance.GenericInstanceFullName`.

use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::sig::{MethodSig, TypeSig};
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};

/// Generic context for resolving `!n` / `!!n` to declared parameter names.
///
/// Cecil resolves these while reading signatures via `reader.context`; here the
/// renderer takes the context instead. Real names come only from TypeDef /
/// MethodDef owners in this module; anything else falls back to `!n` / `!!n`,
/// matching `SignatureReader.GetUnboundGenericParameter`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GenericContext {
    /// Owner for `Var` — meaningful when it is a TypeDef token.
    pub type_owner: Option<MetadataToken>,
    /// Owner for `MVar` — a MethodDef rid.
    pub method_owner: Option<u32>,
}

impl GenericContext {
    pub fn for_type(token: MetadataToken) -> GenericContext {
        GenericContext {
            type_owner: Some(token),
            method_owner: None,
        }
    }

    pub fn for_method(type_token: MetadataToken, method_rid: u32) -> GenericContext {
        GenericContext {
            type_owner: Some(type_token),
            method_owner: Some(method_rid),
        }
    }
}

/// `TypeReference.FullName` for a TypeDef/TypeRef/TypeSpec token.
pub(crate) fn token_full_name(
    m: &ModuleData<'_>,
    token: MetadataToken,
    ctx: GenericContext,
    out: &mut String,
) -> Result<()> {
    token_name(m, token, ctx, 0, out)
}

/// [`token_full_name`] for a token of a signature node at level `depth`
/// (see [`SigReader::below`](crate::model::sig::SigReader::below)).
fn token_name(
    m: &ModuleData<'_>,
    token: MetadataToken,
    ctx: GenericContext,
    depth: u32,
    out: &mut String,
) -> Result<()> {
    match token.token_type() {
        Some(TokenType::TypeDef) => typedef_full_name(m, token.rid(), out),
        Some(TokenType::TypeRef) => typeref_full_name(m, token.rid(), out),
        Some(TokenType::TypeSpec) => {
            let sig = m.type_spec_sig(token.rid(), depth)?;
            sig_name(m, &sig, ctx, depth, out)
        }
        _ => {
            out.push_str("<invalid>");
            Ok(())
        }
    }
}

/// Writes `Declaring/Nested` names, outermost first.
fn push_nested_names<S: AsRef<str>>(
    names: impl DoubleEndedIterator<Item = (S, S)>,
    out: &mut String,
) {
    for (i, (ns, name)) in names.rev().enumerate() {
        if i > 0 {
            out.push('/');
        }
        let (ns, name) = (ns.as_ref(), name.as_ref());
        if !ns.is_empty() {
            out.push_str(ns);
            out.push('.');
        }
        out.push_str(name);
    }
}

pub(crate) fn typedef_full_name(m: &ModuleData<'_>, rid: u32, out: &mut String) -> Result<()> {
    // TypeReference.FullName: nested types render as Declaring/Nested.
    // Nested type names ignore the namespace column in Cecil's rendering only
    // insofar as TypeFullName still applies; nested types normally have an
    // empty namespace, but a non-empty one is still printed. Cecil recurses
    // through DeclaringType, and overflows its stack on a NestedClass cycle;
    // here a chain longer than the TypeDef table is an error.
    let mut names = Vec::new();
    let mut current = Some(rid);
    while let Some(rid) = current {
        if names.len() == m.md.row_count(Table::TypeDef) as usize {
            return Err(Error::Unsupported("nested type cycle"));
        }
        let row = m.md.row::<TypeDefRaw>(rid)?;
        names.push((
            m.md.strings().get(row.namespace)?,
            m.md.strings().get(row.name)?,
        ));
        current = m.declaring_type_rid(rid)?;
    }
    push_nested_names(names.into_iter(), out);
    Ok(())
}

pub(crate) fn typeref_full_name(m: &ModuleData<'_>, rid: u32, out: &mut String) -> Result<()> {
    // A TypeRef scope of TypeRef kind means "nested inside", unless it names
    // no other TypeRef row (see `typeref_declaring_rid`). Cecil overflows its
    // stack on a longer cycle, as with NestedClass.
    let mut names = Vec::new();
    let mut current = Some(rid);
    while let Some(rid) = current {
        if names.len() == m.md.row_count(Table::TypeRef) as usize {
            return Err(Error::Unsupported("TypeRef scope cycle"));
        }
        let row = m.md.row::<TypeRefRaw>(rid)?;
        names.push((
            m.md.strings().get(row.namespace)?,
            m.md.strings().get(row.name)?,
        ));
        current = m.typeref_declaring_rid(rid, row.scope);
    }
    push_nested_names(names.into_iter(), out);
    Ok(())
}

/// The `!n` / `!!n` resolution: name of the n-th generic parameter of the
/// context owner, if declared in this module.
fn generic_param_name(
    m: &ModuleData<'_>,
    ctx: GenericContext,
    index: u32,
    method: bool,
    out: &mut String,
) -> Result<()> {
    let owner = if method {
        ctx.method_owner
            .map(|rid| MetadataToken::new(TokenType::Method, rid))
    } else {
        ctx.type_owner
            .filter(|t| t.token_type() == Some(TokenType::TypeDef))
    };
    if let Some(owner) = owner
        && let Some(ranges) = m.generic_param_ranges()?.get(&owner)
    {
        let mut remaining = index;
        for range in ranges {
            if remaining < range.length {
                let row = m.md.row::<GenericParamRaw>(range.start + remaining)?;
                let name = m.md.strings().get(row.name)?;
                if name.is_empty() {
                    break;
                }
                out.push_str(&name);
                return Ok(());
            }
            remaining -= range.length;
        }
    }
    // GenericParameter.Name fallback: "!"/"!!" + position.
    out.push_str(if method { "!!" } else { "!" });
    out.push_str(&index.to_string());
    Ok(())
}

/// `TypeReference.FullName` over a decoded signature tree.
pub(crate) fn sig_full_name(
    m: &ModuleData<'_>,
    sig: &TypeSig,
    ctx: GenericContext,
    out: &mut String,
) -> Result<()> {
    sig_name(m, sig, ctx, 0, out)
}

/// [`sig_full_name`] for a node below `depth` enclosing levels.
fn sig_name(
    m: &ModuleData<'_>,
    sig: &TypeSig,
    ctx: GenericContext,
    depth: u32,
    out: &mut String,
) -> Result<()> {
    // This node's level; its tokens' TypeSpecs and its children are below it.
    let level = depth + 1;
    match sig {
        TypeSig::Void => out.push_str("System.Void"),
        TypeSig::Boolean => out.push_str("System.Boolean"),
        TypeSig::Char => out.push_str("System.Char"),
        TypeSig::I1 => out.push_str("System.SByte"),
        TypeSig::U1 => out.push_str("System.Byte"),
        TypeSig::I2 => out.push_str("System.Int16"),
        TypeSig::U2 => out.push_str("System.UInt16"),
        TypeSig::I4 => out.push_str("System.Int32"),
        TypeSig::U4 => out.push_str("System.UInt32"),
        TypeSig::I8 => out.push_str("System.Int64"),
        TypeSig::U8 => out.push_str("System.UInt64"),
        TypeSig::R4 => out.push_str("System.Single"),
        TypeSig::R8 => out.push_str("System.Double"),
        TypeSig::String => out.push_str("System.String"),
        TypeSig::Object => out.push_str("System.Object"),
        TypeSig::IntPtr => out.push_str("System.IntPtr"),
        TypeSig::UIntPtr => out.push_str("System.UIntPtr"),
        TypeSig::TypedByRef => out.push_str("System.TypedReference"),
        TypeSig::Named { token, .. } => token_name(m, *token, ctx, level, out)?,
        TypeSig::Ptr(inner) => {
            sig_name(m, inner, ctx, level, out)?;
            out.push('*');
        }
        TypeSig::ByRef(inner) => {
            sig_name(m, inner, ctx, level, out)?;
            out.push('&');
        }
        // PinnedType and SentinelType add no suffix in Cecil.
        TypeSig::Pinned(inner) | TypeSig::Sentinel(inner) => sig_name(m, inner, ctx, level, out)?,
        TypeSig::SzArray(inner) => {
            sig_name(m, inner, ctx, level, out)?;
            out.push_str("[]");
        }
        TypeSig::Array {
            element,
            dimensions,
        } => {
            sig_name(m, element, ctx, level, out)?;
            // ArrayType.Suffix: a single unsized dimension is a vector.
            let is_vector = dimensions.len() == 1
                && dimensions[0].lower.is_none()
                && dimensions[0].upper.is_none();
            if is_vector {
                out.push_str("[]");
            } else {
                out.push('[');
                for (i, d) in dimensions.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    // ArrayDimension.ToString: empty if unsized, else
                    // lower + "..." + upper with nulls printing as empty.
                    if d.lower.is_some() || d.upper.is_some() {
                        if let Some(l) = d.lower {
                            out.push_str(&l.to_string());
                        }
                        out.push_str("...");
                        if let Some(u) = d.upper {
                            out.push_str(&u.to_string());
                        }
                    }
                }
                out.push(']');
            }
        }
        TypeSig::GenericInst { ty, args, .. } => {
            token_name(m, *ty, ctx, level, out)?;
            out.push('<');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                sig_name(m, a, ctx, level, out)?;
            }
            out.push('>');
        }
        TypeSig::Var(n) => generic_param_name(m, ctx, *n, false, out)?,
        TypeSig::MVar(n) => generic_param_name(m, ctx, *n, true, out)?,
        TypeSig::CModReq { modifier, inner } => {
            sig_name(m, inner, ctx, level, out)?;
            out.push_str(" modreq(");
            token_name(m, *modifier, ctx, level, out)?;
            out.push(')');
        }
        TypeSig::CModOpt { modifier, inner } => {
            sig_name(m, inner, ctx, level, out)?;
            out.push_str(" modopt(");
            token_name(m, *modifier, ctx, level, out)?;
            out.push(')');
        }
        TypeSig::FnPtr(func) => {
            // FunctionPointerType.FullName: name ("method") + " " + ret + " *"
            // + params.
            out.push_str("method ");
            sig_name(m, &func.return_type, ctx, level, out)?;
            out.push_str(" *");
            params_name(m, func, ctx, level, out)?;
        }
    }
    Ok(())
}

/// `IMethodSignature.MethodSignatureFullName`: "(" p1 "," p2 ... ")", with a
/// "...," marker before a sentinel parameter.
pub(crate) fn method_params_full_name(
    m: &ModuleData<'_>,
    sig: &MethodSig,
    ctx: GenericContext,
    out: &mut String,
) -> Result<()> {
    params_name(m, sig, ctx, 0, out)
}

/// [`method_params_full_name`] for parameters below `depth` enclosing levels.
fn params_name(
    m: &ModuleData<'_>,
    sig: &MethodSig,
    ctx: GenericContext,
    depth: u32,
    out: &mut String,
) -> Result<()> {
    out.push('(');
    for (i, p) in sig.params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if p.is_sentinel() {
            out.push_str("...,");
        }
        sig_name(m, p, ctx, depth, out)?;
    }
    out.push(')');
    Ok(())
}

/// `TypeReference.TypeFullName`: the namespace and name joined by a dot, or
/// the name alone with no namespace.
pub(crate) fn type_full_name(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}.{name}")
    }
}
