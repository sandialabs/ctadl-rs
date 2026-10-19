//! Operand resolution for Layer 1: the little that `instruction_flow` needs
//! to know about a token to give an instruction its shape.
//!
//! [`Operands`] is a trait so that Layer 1 can be driven without a module
//! (the unit tests use a stub); [`Module`] and [`MethodBody`] implement it
//! over the module's metadata.

use super::{FieldOp, MethodOp, TypeOp};
use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::handles::{MemberRef, MemberRefSig, MethodDef, MethodSpec, StandAloneSig};
use crate::model::module::{Entity, lookup_token};
use crate::model::names::{GenericContext, token_full_name};
use crate::model::{MethodBody, MethodSig, Module, TypeSig};
use crate::raw::tables::Table;
use crate::token::{MetadataToken, TokenType};

/// A runtime-provided method of a multi-dimensional array type
/// (ECMA-335 §II.14.2), which Layer 1 reports as an element access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayMethod {
    Get,
    Set,
    Address,
    Ctor,
}

/// A method operand and what Layer 1 needs to know about its declaring type.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMethod {
    pub op: MethodOp,
    /// The declaring type, when the token names one.
    pub declaring: Option<TypeOp>,
    /// Whether the declaring type is a value type, when that is known
    /// without loading another assembly. Decides `NewKind` for newobj.
    pub declaring_is_value_type: Option<bool>,
    /// Set when the method is `Get`/`Set`/`Address`/`.ctor` of a
    /// multi-dimensional array type.
    pub array: Option<ArrayMethod>,
}

/// Resolution of instruction operands.
pub trait Operands {
    /// A field token (`Field` or field `MemberRef`).
    fn field(&self, token: MetadataToken, is_static: bool) -> Result<FieldOp>;
    /// A method token (`MethodDef`, method `MemberRef`, or `MethodSpec`).
    fn method(&self, token: MetadataToken) -> Result<ResolvedMethod>;
    /// A `calli` call-site signature (`StandAloneSig`).
    fn call_site(&self, token: MetadataToken) -> Result<MethodSig>;
    /// Whether a type is a value type, when that is known without loading
    /// another assembly. `None` for generic parameters and for references
    /// into other assemblies that are not primitives.
    fn is_value_type(&self, ty: &TypeOp) -> Option<bool>;
}

/// What Layer 1 needs beyond the instruction: operand resolution and the
/// enclosing method's signature (for `ret`).
#[derive(Clone, Copy)]
pub struct FlowCx<'a> {
    pub operands: &'a dyn Operands,
    pub method_sig: &'a MethodSig,
}

impl<'a> FlowCx<'a> {
    pub fn new(operands: &'a dyn Operands, method_sig: &'a MethodSig) -> FlowCx<'a> {
        FlowCx {
            operands,
            method_sig,
        }
    }
}

fn unresolved() -> Error {
    Error::Unsupported("token does not resolve")
}

/// Whether a signature type is a value type, when that is known locally.
fn sig_is_value_type(m: &ModuleData<'_>, sig: &TypeSig) -> Option<bool> {
    Some(match sig.unwrap_modifiers() {
        TypeSig::Void
        | TypeSig::Boolean
        | TypeSig::Char
        | TypeSig::I1
        | TypeSig::U1
        | TypeSig::I2
        | TypeSig::U2
        | TypeSig::I4
        | TypeSig::U4
        | TypeSig::I8
        | TypeSig::U8
        | TypeSig::R4
        | TypeSig::R8
        | TypeSig::IntPtr
        | TypeSig::UIntPtr
        | TypeSig::TypedByRef
        | TypeSig::Ptr(_)
        | TypeSig::FnPtr(_) => true,
        TypeSig::String
        | TypeSig::Object
        | TypeSig::SzArray(_)
        | TypeSig::Array { .. }
        | TypeSig::ByRef(_) => false,
        TypeSig::GenericInst { is_value_type, .. } => *is_value_type,
        // A `valuetype`/`class` prefix says it, but a TypeDef knows better.
        TypeSig::Named {
            token,
            is_value_type,
        } => return token_is_value_type(m, *token, 1).or(Some(*is_value_type)),
        TypeSig::Var(_) | TypeSig::MVar(_) => return None,
        TypeSig::CModReq { .. }
        | TypeSig::CModOpt { .. }
        | TypeSig::Pinned(_)
        | TypeSig::Sentinel(_) => return None,
    })
}

fn token_is_value_type(m: &ModuleData<'_>, token: MetadataToken, depth: u32) -> Option<bool> {
    match token.token_type()? {
        TokenType::TypeDef => match lookup_token(m, token).ok()?? {
            Entity::TypeDef(t) => t.is_value_type().ok(),
            _ => None,
        },
        TokenType::TypeRef => match lookup_token(m, token).ok()?? {
            Entity::TypeRef(t) => {
                let prim = t.primitive().ok()??;
                sig_is_value_type(m, &prim)
            }
            _ => None,
        },
        TokenType::TypeSpec if depth < crate::model::sig::MAX_TYPE_SIG_DEPTH => {
            let sig = m.type_spec_sig(token.rid(), depth).ok()?;
            sig_is_value_type(m, &sig)
        }
        _ => None,
    }
}

pub(crate) fn field(m: &ModuleData<'_>, token: MetadataToken, is_static: bool) -> Result<FieldOp> {
    let ty = match lookup_token(m, token)? {
        Some(Entity::Field(f)) => f.field_type()?,
        Some(Entity::MemberRef(r)) => match r.signature()? {
            MemberRefSig::Field(ty) => ty,
            MemberRefSig::Method(_) => return Err(unresolved()),
        },
        _ => return Err(unresolved()),
    };
    Ok(FieldOp {
        token,
        is_static,
        ty,
    })
}

pub(crate) fn method(m: &ModuleData<'_>, token: MetadataToken) -> Result<ResolvedMethod> {
    match lookup_token(m, token)? {
        Some(Entity::Method(d)) => method_def(m, token, d),
        Some(Entity::MemberRef(r)) => member_ref(m, token, r),
        Some(Entity::MethodSpec(s)) => method_spec(m, token, s),
        _ => Err(unresolved()),
    }
}

fn method_def(
    _m: &ModuleData<'_>,
    token: MetadataToken,
    d: MethodDef<'_>,
) -> Result<ResolvedMethod> {
    let declaring = d.declaring_type()?;
    Ok(ResolvedMethod {
        op: MethodOp {
            token,
            sig: d.signature()?,
            generic_args: Vec::new(),
        },
        declaring: declaring.map(|t| TypeOp { token: t.token() }),
        declaring_is_value_type: match declaring {
            Some(t) => Some(t.is_value_type()?),
            None => None,
        },
        array: None,
    })
}

fn member_ref(
    m: &ModuleData<'_>,
    token: MetadataToken,
    r: MemberRef<'_>,
) -> Result<ResolvedMethod> {
    let MemberRefSig::Method(sig) = r.signature()? else {
        return Err(unresolved());
    };
    let op = MethodOp {
        token,
        sig,
        generic_args: Vec::new(),
    };
    let class = r.class_token()?;
    let mut resolved = ResolvedMethod {
        op,
        declaring: None,
        declaring_is_value_type: None,
        array: None,
    };
    match class.token_type() {
        Some(TokenType::TypeDef | TokenType::TypeRef) => {
            resolved.declaring = Some(TypeOp { token: class });
            resolved.declaring_is_value_type = token_is_value_type(m, class, 0);
        }
        Some(TokenType::TypeSpec) => {
            resolved.declaring = Some(TypeOp { token: class });
            let sig = m.type_spec_sig(class.rid(), 0)?;
            if let TypeSig::Array { .. } = sig.unwrap_modifiers() {
                resolved.array = match &*r.name()? {
                    "Get" => Some(ArrayMethod::Get),
                    "Set" => Some(ArrayMethod::Set),
                    "Address" => Some(ArrayMethod::Address),
                    ".ctor" => Some(ArrayMethod::Ctor),
                    _ => None,
                };
            }
            resolved.declaring_is_value_type = sig_is_value_type(m, &sig);
        }
        // A vararg call to a MethodDef: the call site's MemberRef points
        // at the definition.
        Some(TokenType::Method) => {
            if let Some(Entity::Method(d)) = lookup_token(m, class)? {
                let def = method_def(m, class, d)?;
                resolved.declaring = def.declaring;
                resolved.declaring_is_value_type = def.declaring_is_value_type;
            }
        }
        // A global function of another module: no declaring type.
        _ => {}
    }
    Ok(resolved)
}

fn method_spec(
    m: &ModuleData<'_>,
    token: MetadataToken,
    s: MethodSpec<'_>,
) -> Result<ResolvedMethod> {
    let inner = s.method_token()?;
    if inner.token_type() == Some(TokenType::MethodSpec) {
        return Err(unresolved());
    }
    let mut resolved = method(m, inner)?;
    resolved.op.token = token;
    resolved.op.generic_args = s.generic_args()?;
    Ok(resolved)
}

pub(crate) fn call_site(m: &ModuleData<'_>, token: MetadataToken) -> Result<MethodSig> {
    let rid = token.rid();
    if token.token_type() != Some(TokenType::Signature)
        || rid == 0
        || rid > m.md.row_count(Table::StandAloneSig)
    {
        return Err(unresolved());
    }
    StandAloneSig { m, rid }.as_method_sig()
}

impl Operands for Module<'_> {
    fn field(&self, token: MetadataToken, is_static: bool) -> Result<FieldOp> {
        field(self.m(), token, is_static)
    }

    fn method(&self, token: MetadataToken) -> Result<ResolvedMethod> {
        method(self.m(), token)
    }

    fn call_site(&self, token: MetadataToken) -> Result<MethodSig> {
        call_site(self.m(), token)
    }

    fn is_value_type(&self, ty: &TypeOp) -> Option<bool> {
        token_is_value_type(self.m(), ty.token, 0)
    }
}

impl Operands for MethodBody<'_> {
    fn field(&self, token: MetadataToken, is_static: bool) -> Result<FieldOp> {
        field(self.m, token, is_static)
    }

    fn method(&self, token: MetadataToken) -> Result<ResolvedMethod> {
        method(self.m, token)
    }

    fn call_site(&self, token: MetadataToken) -> Result<MethodSig> {
        call_site(self.m, token)
    }

    fn is_value_type(&self, ty: &TypeOp) -> Option<bool> {
        token_is_value_type(self.m, ty.token, 0)
    }
}

// ---- names -----------------------------------------------------------------

impl<'a> MethodBody<'a> {
    fn names_context(&self) -> Result<GenericContext> {
        self.method().generic_context()
    }

    /// The field's Cecil `FullName`, for display.
    pub fn field_name(&self, f: &FieldOp) -> Result<String> {
        match lookup_token(self.m, f.token)? {
            Some(Entity::Field(d)) => d.full_name(),
            Some(Entity::MemberRef(r)) => r.full_name_with_outer(self.names_context()?),
            _ => Err(unresolved()),
        }
    }

    /// The type's Cecil `FullName`, for display.
    pub fn type_name(&self, t: &TypeOp) -> Result<String> {
        let mut out = String::new();
        token_full_name(self.m, t.token, self.names_context()?, &mut out)?;
        Ok(out)
    }

    /// The method's Cecil `FullName`, for display.
    pub fn method_name(&self, op: &MethodOp) -> Result<String> {
        match lookup_token(self.m, op.token)? {
            Some(Entity::Method(d)) => d.full_name(),
            Some(Entity::MemberRef(r)) => r.full_name_with_outer(self.names_context()?),
            Some(Entity::MethodSpec(s)) => s.full_name_with_outer(self.names_context()?),
            _ => Err(unresolved()),
        }
    }

    /// The type that declares the method, as the token names it.
    pub fn declaring_type(&self, op: &MethodOp) -> Result<TypeOp> {
        method(self.m, op.token)?.declaring.ok_or_else(unresolved)
    }
}
