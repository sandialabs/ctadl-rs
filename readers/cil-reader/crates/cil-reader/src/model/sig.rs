//! Signature blob decoding.
//!
//! Port of Cecil's `SignatureReader` (`Mono.Cecil/AssemblyReader.cs:3269-3901`),
//! except that type signatures decode into an owned, context-free [`TypeSig`]
//! tree referencing metadata tokens; the model layer resolves tokens and
//! generic-parameter names when rendering.

use crate::error::{Error, Result};
use crate::raw::coded::CodedIndexKind;
use crate::raw::cursor::Cur;
use crate::token::MetadataToken;

/// `Mono.Cecil.Metadata.ElementType` (ECMA-335 II.23.1.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ElementType {
    None = 0x00,
    Void = 0x01,
    Boolean = 0x02,
    Char = 0x03,
    I1 = 0x04,
    U1 = 0x05,
    I2 = 0x06,
    U2 = 0x07,
    I4 = 0x08,
    U4 = 0x09,
    I8 = 0x0a,
    U8 = 0x0b,
    R4 = 0x0c,
    R8 = 0x0d,
    String = 0x0e,
    Ptr = 0x0f,
    ByRef = 0x10,
    ValueType = 0x11,
    Class = 0x12,
    Var = 0x13,
    Array = 0x14,
    GenericInst = 0x15,
    TypedByRef = 0x16,
    I = 0x18,
    U = 0x19,
    FnPtr = 0x1b,
    Object = 0x1c,
    SzArray = 0x1d,
    MVar = 0x1e,
    CModReqD = 0x1f,
    CModOpt = 0x20,
    Internal = 0x21,
    Modifier = 0x40,
    Sentinel = 0x41,
    Pinned = 0x45,
    /// Custom-attribute blobs only (undocumented): System.Type value.
    Type = 0x50,
    /// Custom-attribute blobs only: boxed object.
    Boxed = 0x51,
    /// Custom-attribute blobs only: enum value with serialized type name.
    Enum = 0x55,
}

impl ElementType {
    pub fn from_byte(b: u8) -> Option<ElementType> {
        use ElementType::*;
        Some(match b {
            0x00 => None,
            0x01 => Void,
            0x02 => Boolean,
            0x03 => Char,
            0x04 => I1,
            0x05 => U1,
            0x06 => I2,
            0x07 => U2,
            0x08 => I4,
            0x09 => U4,
            0x0a => I8,
            0x0b => U8,
            0x0c => R4,
            0x0d => R8,
            0x0e => String,
            0x0f => Ptr,
            0x10 => ByRef,
            0x11 => ValueType,
            0x12 => Class,
            0x13 => Var,
            0x14 => Array,
            0x15 => GenericInst,
            0x16 => TypedByRef,
            0x18 => I,
            0x19 => U,
            0x1b => FnPtr,
            0x1c => Object,
            0x1d => SzArray,
            0x1e => MVar,
            0x1f => CModReqD,
            0x20 => CModOpt,
            0x21 => Internal,
            0x40 => Modifier,
            0x41 => Sentinel,
            0x45 => Pinned,
            0x50 => Type,
            0x51 => Boxed,
            0x55 => Enum,
            _ => return Option::None,
        })
    }
}

/// One dimension of a multi-dimensional array: optional lower and upper bound
/// (Cecil `ArrayDimension`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayDimension {
    pub lower: Option<i32>,
    pub upper: Option<i32>,
}

/// An owned, context-free decoded type signature.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeSig {
    Void,
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
    /// System.IntPtr (`ELEMENT_TYPE_I`).
    IntPtr,
    /// System.UIntPtr (`ELEMENT_TYPE_U`).
    UIntPtr,
    /// System.TypedReference.
    TypedByRef,
    /// `ELEMENT_TYPE_CLASS` / `ELEMENT_TYPE_VALUETYPE`: a TypeDef/TypeRef/TypeSpec token.
    Named {
        token: MetadataToken,
        is_value_type: bool,
    },
    Ptr(Box<TypeSig>),
    ByRef(Box<TypeSig>),
    Pinned(Box<TypeSig>),
    SzArray(Box<TypeSig>),
    Array {
        element: Box<TypeSig>,
        dimensions: Vec<ArrayDimension>,
    },
    GenericInst {
        is_value_type: bool,
        ty: MetadataToken,
        args: Vec<TypeSig>,
    },
    /// Type generic parameter `!n`.
    Var(u32),
    /// Method generic parameter `!!n`.
    MVar(u32),
    FnPtr(Box<MethodSig>),
    CModReq {
        modifier: MetadataToken,
        inner: Box<TypeSig>,
    },
    CModOpt {
        modifier: MetadataToken,
        inner: Box<TypeSig>,
    },
    /// Vararg sentinel marker wrapping the following parameter type.
    Sentinel(Box<TypeSig>),
}

impl TypeSig {
    /// Strips modifiers/pinned/sentinel wrappers (Cecil `GetElementType`-ish,
    /// used where Cecil dispatches on `etype`).
    pub fn unwrap_modifiers(&self) -> &TypeSig {
        match self {
            TypeSig::CModReq { inner, .. } | TypeSig::CModOpt { inner, .. } => {
                inner.unwrap_modifiers()
            }
            TypeSig::Pinned(inner) | TypeSig::Sentinel(inner) => inner.unwrap_modifiers(),
            other => other,
        }
    }

    pub fn is_sentinel(&self) -> bool {
        matches!(self, TypeSig::Sentinel(_))
    }

    pub fn is_pinned(&self) -> bool {
        matches!(self, TypeSig::Pinned(_))
    }
}

/// `Mono.Cecil.MethodCallingConvention` values (low bits of the calling
/// convention byte).
pub mod calling_convention {
    pub const DEFAULT: u8 = 0x0;
    pub const C: u8 = 0x1;
    pub const STD_CALL: u8 = 0x2;
    pub const THIS_CALL: u8 = 0x3;
    pub const FAST_CALL: u8 = 0x4;
    pub const VAR_ARG: u8 = 0x5;
    pub const GENERIC: u8 = 0x10;
}

/// A decoded method signature (also used for `calli` call sites and function
/// pointers).
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub has_this: bool,
    pub explicit_this: bool,
    /// The calling-convention byte with `has_this`/`explicit_this` masked off.
    pub calling_convention: u8,
    /// Generic parameter count (0 unless the `generic` bit was set).
    pub generic_arity: u32,
    pub return_type: TypeSig,
    pub params: Vec<TypeSig>,
}

/// A decoded property signature.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertySig {
    pub has_this: bool,
    pub property_type: TypeSig,
}

/// Reader over one signature blob (already length-stripped).
pub struct SigReader<'a> {
    pub(crate) cur: Cur<'a>,
    end: usize,
}

impl<'a> SigReader<'a> {
    /// `blob` is the signature contents (the compressed length prefix already
    /// removed by the blob heap accessor).
    pub fn new(blob: &'a [u8]) -> SigReader<'a> {
        SigReader {
            cur: Cur::new(blob),
            end: blob.len(),
        }
    }

    pub fn can_read_more(&self) -> bool {
        self.cur.pos() < self.end
    }

    fn type_token(&mut self) -> Result<MetadataToken> {
        Ok(CodedIndexKind::TypeDefOrRef.decode(self.cur.compressed_u32()?))
    }

    /// `SignatureReader.ReadTypeSignature`.
    pub fn type_sig(&mut self) -> Result<TypeSig> {
        let b = self.cur.u8()?;
        let etype = ElementType::from_byte(b).ok_or(Error::bad(
            self.cur.pos(),
            "unknown element type in signature",
        ))?;
        self.type_sig_of(etype)
    }

    fn type_sig_of(&mut self, etype: ElementType) -> Result<TypeSig> {
        use ElementType as ET;
        Ok(match etype {
            ET::Void => TypeSig::Void,
            ET::Boolean => TypeSig::Boolean,
            ET::Char => TypeSig::Char,
            ET::I1 => TypeSig::I1,
            ET::U1 => TypeSig::U1,
            ET::I2 => TypeSig::I2,
            ET::U2 => TypeSig::U2,
            ET::I4 => TypeSig::I4,
            ET::U4 => TypeSig::U4,
            ET::I8 => TypeSig::I8,
            ET::U8 => TypeSig::U8,
            ET::R4 => TypeSig::R4,
            ET::R8 => TypeSig::R8,
            ET::String => TypeSig::String,
            ET::Object => TypeSig::Object,
            ET::I => TypeSig::IntPtr,
            ET::U => TypeSig::UIntPtr,
            ET::TypedByRef => TypeSig::TypedByRef,
            ET::ValueType => TypeSig::Named {
                token: self.type_token()?,
                is_value_type: true,
            },
            ET::Class => TypeSig::Named {
                token: self.type_token()?,
                is_value_type: false,
            },
            ET::Ptr => TypeSig::Ptr(Box::new(self.type_sig()?)),
            ET::ByRef => TypeSig::ByRef(Box::new(self.type_sig()?)),
            ET::Pinned => TypeSig::Pinned(Box::new(self.type_sig()?)),
            ET::SzArray => TypeSig::SzArray(Box::new(self.type_sig()?)),
            ET::FnPtr => TypeSig::FnPtr(Box::new(self.method_sig()?)),
            ET::Sentinel => TypeSig::Sentinel(Box::new(self.type_sig()?)),
            ET::Var => TypeSig::Var(self.cur.compressed_u32()?),
            ET::MVar => TypeSig::MVar(self.cur.compressed_u32()?),
            ET::CModReqD => TypeSig::CModReq {
                modifier: self.type_token()?,
                inner: Box::new(self.type_sig()?),
            },
            ET::CModOpt => TypeSig::CModOpt {
                modifier: self.type_token()?,
                inner: Box::new(self.type_sig()?),
            },
            ET::Array => self.array_sig()?,
            ET::GenericInst => {
                let is_value_type = self.cur.u8()? == ElementType::ValueType as u8;
                let ty = self.type_token()?;
                let arity = self.cur.compressed_u32()?;
                let mut args = Vec::with_capacity(arity.min(64) as usize);
                for _ in 0..arity {
                    args.push(self.type_sig()?);
                }
                TypeSig::GenericInst {
                    is_value_type,
                    ty,
                    args,
                }
            }
            _ => {
                return Err(Error::bad(
                    self.cur.pos(),
                    "unexpected element type in type signature",
                ));
            }
        })
    }

    /// `SignatureReader.ReadArrayTypeSignature`.
    fn array_sig(&mut self) -> Result<TypeSig> {
        let element = self.type_sig()?;
        let rank = self.cur.compressed_u32()?;
        let num_sizes = self.cur.compressed_u32()?;
        let mut sizes = Vec::with_capacity(num_sizes.min(64) as usize);
        for _ in 0..num_sizes {
            sizes.push(self.cur.compressed_u32()?);
        }
        let num_low_bounds = self.cur.compressed_u32()?;
        let mut low_bounds = Vec::with_capacity(num_low_bounds.min(64) as usize);
        for _ in 0..num_low_bounds {
            low_bounds.push(self.cur.compressed_i32()?);
        }
        let mut dimensions = Vec::with_capacity(rank.min(64) as usize);
        for i in 0..rank as usize {
            let lower = low_bounds.get(i).copied();
            // Cecil: `upper = lower + (int) sizes [i] - 1` — C# nullable
            // arithmetic leaves upper null when lower is null.
            let upper = match (lower, sizes.get(i)) {
                (Some(l), Some(&s)) => Some(l + s as i32 - 1),
                _ => None,
            };
            dimensions.push(ArrayDimension { lower, upper });
        }
        Ok(TypeSig::Array {
            element: Box::new(element),
            dimensions,
        })
    }

    /// `SignatureReader.ReadMethodSignature`.
    pub fn method_sig(&mut self) -> Result<MethodSig> {
        let mut cc = self.cur.u8()?;
        const HAS_THIS: u8 = 0x20;
        const EXPLICIT_THIS: u8 = 0x40;
        let has_this = cc & HAS_THIS != 0;
        cc &= !HAS_THIS;
        let explicit_this = cc & EXPLICIT_THIS != 0;
        cc &= !EXPLICIT_THIS;

        let generic_arity = if cc & calling_convention::GENERIC != 0 {
            self.cur.compressed_u32()?
        } else {
            0
        };

        let param_count = self.cur.compressed_u32()?;
        let return_type = self.type_sig()?;
        let mut params = Vec::with_capacity(param_count.min(256) as usize);
        for _ in 0..param_count {
            params.push(self.type_sig()?);
        }

        Ok(MethodSig {
            has_this,
            explicit_this,
            calling_convention: cc,
            generic_arity,
            return_type,
            params,
        })
    }

    /// Field signature: 0x06 prefix then a type.
    pub fn field_sig(&mut self) -> Result<TypeSig> {
        const FIELD_SIG: u8 = 0x6;
        if self.cur.u8()? != FIELD_SIG {
            return Err(Error::bad(self.cur.pos(), "not a field signature"));
        }
        self.type_sig()
    }

    /// Property signature: 0x08 (| has_this) prefix, param count, type, params.
    /// Like Cecil (`MetadataReader.ReadProperty`), the parameters are not
    /// read: obfuscators put junk there, and `PropertyDef::parameters` comes
    /// from the accessors.
    pub fn property_sig(&mut self) -> Result<PropertySig> {
        const PROPERTY_SIG: u8 = 0x8;
        let cc = self.cur.u8()?;
        if cc & PROPERTY_SIG == 0 {
            return Err(Error::bad(self.cur.pos(), "not a property signature"));
        }
        let has_this = cc & 0x20 != 0;
        let _count = self.cur.compressed_u32()?;
        let property_type = self.type_sig()?;
        Ok(PropertySig {
            has_this,
            property_type,
        })
    }

    /// Local-variable signature: 0x07 prefix, count, types.
    pub fn local_var_sig(&mut self) -> Result<Vec<TypeSig>> {
        const LOCAL_SIG: u8 = 0x7;
        if self.cur.u8()? != LOCAL_SIG {
            return Err(Error::bad(self.cur.pos(), "not a local variable signature"));
        }
        let count = self.cur.compressed_u32()?;
        let mut vars = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            vars.push(self.type_sig()?);
        }
        Ok(vars)
    }

    /// MethodSpec instantiation signature: 0x0a prefix, arity, types.
    pub fn method_spec_sig(&mut self) -> Result<Vec<TypeSig>> {
        const METHOD_SPEC_SIG: u8 = 0x0a;
        if self.cur.u8()? != METHOD_SPEC_SIG {
            return Err(Error::bad(self.cur.pos(), "not a method spec signature"));
        }
        let arity = self.cur.compressed_u32()?;
        let mut args = Vec::with_capacity(arity.min(64) as usize);
        for _ in 0..arity {
            args.push(self.type_sig()?);
        }
        Ok(args)
    }

    /// A `null`-aware length-prefixed UTF-8 string (`SignatureReader.ReadUTF8String`).
    pub fn utf8_string(&mut self) -> Result<Option<&'a str>> {
        let first = self.cur.data().get(self.cur.pos()).copied();
        if first == Some(0xff) {
            self.cur.skip(1)?;
            return Ok(None);
        }
        let len = self.cur.compressed_u32()? as usize;
        if len == 0 {
            return Ok(Some(""));
        }
        let bytes = self.cur.bytes(len)?;
        Ok(Some(std::str::from_utf8(bytes).map_err(|_| {
            Error::bad(self.cur.pos(), "invalid UTF-8 string in blob")
        })?))
    }
}
