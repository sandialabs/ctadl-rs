//! `FullName` rendering with Cecil parity.
//!
//! Ports the rules spread across Cecil's `TypeReference.FullName`, the
//! `TypeSpecification` subclasses, `MethodReference.FullName`,
//! `IMethodSignature.MethodSignatureFullName` and
//! `IGenericInstance.GenericInstanceFullName`.

use crate::error::Result;
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
    match token.token_type() {
        Some(TokenType::TypeDef) => typedef_full_name(m, token.rid(), out),
        Some(TokenType::TypeRef) => typeref_full_name(m, token.rid(), out),
        Some(TokenType::TypeSpec) => {
            let row = m.md.row::<TypeSpecRaw>(token.rid())?;
            let blob = m.md.blobs().get(row.signature)?;
            let sig = crate::model::sig::SigReader::new(blob).type_sig()?;
            sig_full_name(m, &sig, ctx, out)
        }
        _ => {
            out.push_str("<invalid>");
            Ok(())
        }
    }
}

pub(crate) fn typedef_full_name(m: &ModuleData<'_>, rid: u32, out: &mut String) -> Result<()> {
    // TypeReference.FullName: nested types render as Declaring/Nested.
    if let Some(&declaring) = m.nested_maps()?.reverse.get(&rid) {
        typedef_full_name(m, declaring, out)?;
        out.push('/');
        // Nested type names ignore the namespace column in Cecil's rendering
        // only insofar as TypeFullName still applies; nested types normally
        // have an empty namespace, but a non-empty one is still printed.
    }
    let row = m.md.row::<TypeDefRaw>(rid)?;
    let ns = m.md.strings().get(row.namespace)?;
    let name = m.md.strings().get(row.name)?;
    if !ns.is_empty() {
        out.push_str(ns);
        out.push('.');
    }
    out.push_str(name);
    Ok(())
}

pub(crate) fn typeref_full_name(m: &ModuleData<'_>, rid: u32, out: &mut String) -> Result<()> {
    let row = m.md.row::<TypeRefRaw>(rid)?;
    // A TypeRef scope of TypeRef kind means "nested inside" (unless it points
    // at itself — obfuscated modules do that, `AssemblyReader.cs:1134-1142`).
    if row.scope.token_type() == Some(TokenType::TypeRef) && row.scope.rid() != rid {
        typeref_full_name(m, row.scope.rid(), out)?;
        out.push('/');
    }
    let ns = m.md.strings().get(row.namespace)?;
    let name = m.md.strings().get(row.name)?;
    if !ns.is_empty() {
        out.push_str(ns);
        out.push('.');
    }
    out.push_str(name);
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
                out.push_str(m.md.strings().get(row.name)?);
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
        TypeSig::Named { token, .. } => token_full_name(m, *token, ctx, out)?,
        TypeSig::Ptr(inner) => {
            sig_full_name(m, inner, ctx, out)?;
            out.push('*');
        }
        TypeSig::ByRef(inner) => {
            sig_full_name(m, inner, ctx, out)?;
            out.push('&');
        }
        // PinnedType and SentinelType add no suffix in Cecil.
        TypeSig::Pinned(inner) | TypeSig::Sentinel(inner) => sig_full_name(m, inner, ctx, out)?,
        TypeSig::SzArray(inner) => {
            sig_full_name(m, inner, ctx, out)?;
            out.push_str("[]");
        }
        TypeSig::Array {
            element,
            dimensions,
        } => {
            sig_full_name(m, element, ctx, out)?;
            // ArrayType.Suffix: a single unsized dimension is a vector.
            let is_vector =
                dimensions.len() == 1 && dimensions[0].lower.is_none() && dimensions[0].upper.is_none();
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
            token_full_name(m, *ty, ctx, out)?;
            out.push('<');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                sig_full_name(m, a, ctx, out)?;
            }
            out.push('>');
        }
        TypeSig::Var(n) => generic_param_name(m, ctx, *n, false, out)?,
        TypeSig::MVar(n) => generic_param_name(m, ctx, *n, true, out)?,
        TypeSig::CModReq { modifier, inner } => {
            sig_full_name(m, inner, ctx, out)?;
            out.push_str(" modreq(");
            token_full_name(m, *modifier, ctx, out)?;
            out.push(')');
        }
        TypeSig::CModOpt { modifier, inner } => {
            sig_full_name(m, inner, ctx, out)?;
            out.push_str(" modopt(");
            token_full_name(m, *modifier, ctx, out)?;
            out.push(')');
        }
        TypeSig::FnPtr(func) => {
            // FunctionPointerType.FullName: name ("method") + " " + ret + " *"
            // + params.
            out.push_str("method ");
            sig_full_name(m, &func.return_type, ctx, out)?;
            out.push_str(" *");
            method_params_full_name(m, func, ctx, out)?;
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
    out.push('(');
    for (i, p) in sig.params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if p.is_sentinel() {
            out.push_str("...,");
        }
        sig_full_name(m, p, ctx, out)?;
    }
    out.push(')');
    Ok(())
}
