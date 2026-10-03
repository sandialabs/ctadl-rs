//! Marshal descriptor (FieldMarshal blob) decoding.
//!
//! Port of `SignatureReader.ReadMarshalInfo` (`AssemblyReader.cs:3736-3789`).

use crate::error::{Error, Result};
use crate::model::sig::SigReader;
use crate::raw::Guid;

/// Raw `NativeType` byte values (ECMA-335 II.23.4 + Cecil `NativeType`).
pub mod native_type {
    pub const BOOLEAN: u8 = 0x02;
    pub const I1: u8 = 0x03;
    pub const U1: u8 = 0x04;
    pub const I2: u8 = 0x05;
    pub const U2: u8 = 0x06;
    pub const I4: u8 = 0x07;
    pub const U4: u8 = 0x08;
    pub const I8: u8 = 0x09;
    pub const U8: u8 = 0x0a;
    pub const R4: u8 = 0x0b;
    pub const R8: u8 = 0x0c;
    pub const CURRENCY: u8 = 0x0f;
    pub const BSTR: u8 = 0x13;
    pub const LP_STR: u8 = 0x14;
    pub const LP_WSTR: u8 = 0x15;
    pub const LP_TSTR: u8 = 0x16;
    pub const FIXED_SYS_STRING: u8 = 0x17;
    pub const IUNKNOWN: u8 = 0x19;
    pub const IDISPATCH: u8 = 0x1a;
    pub const STRUCT: u8 = 0x1b;
    pub const INT_F: u8 = 0x1c;
    pub const SAFE_ARRAY: u8 = 0x1d;
    pub const FIXED_ARRAY: u8 = 0x1e;
    pub const INT: u8 = 0x1f;
    pub const UINT: u8 = 0x20;
    pub const BY_VAL_STR: u8 = 0x22;
    pub const ANSI_BSTR: u8 = 0x23;
    pub const TBSTR: u8 = 0x24;
    pub const VARIANT_BOOL: u8 = 0x25;
    pub const FUNC: u8 = 0x26;
    pub const AS_ANY: u8 = 0x28;
    pub const ARRAY: u8 = 0x2a;
    pub const LP_STRUCT: u8 = 0x2b;
    pub const CUSTOM_MARSHALER: u8 = 0x2c;
    pub const ERROR: u8 = 0x2d;
    pub const MAX: u8 = 0x50;
    /// Cecil `NativeType.None` (no explicit value read).
    pub const NONE: u8 = 0x66;
}

/// `VariantType` values (SafeArray element types).
pub mod variant_type {
    pub const NONE: u8 = 0;
    pub const I2: u8 = 2;
    pub const I4: u8 = 3;
    pub const R4: u8 = 4;
    pub const R8: u8 = 5;
    pub const CY: u8 = 6;
    pub const DATE: u8 = 7;
    pub const BSTR: u8 = 8;
    pub const DISPATCH: u8 = 9;
    pub const ERROR: u8 = 10;
    pub const BOOL: u8 = 11;
    pub const VARIANT: u8 = 12;
    pub const UNKNOWN: u8 = 13;
    pub const DECIMAL: u8 = 14;
    pub const I1: u8 = 16;
    pub const UI1: u8 = 17;
    pub const UI2: u8 = 18;
    pub const UI4: u8 = 19;
    pub const I8: u8 = 20;
    pub const UI8: u8 = 21;
    pub const INT: u8 = 22;
    pub const UINT: u8 = 23;
}

/// A decoded marshal descriptor (Cecil `MarshalInfo` + subclasses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarshalInfo<'a> {
    /// Any native type without extra data.
    Simple { native: u8 },
    /// `NativeType.Array` (Cecil `ArrayMarshalInfo`; unread fields default to
    /// `None`/-1).
    Array {
        element_type: u8,
        size_parameter_index: i32,
        size: i32,
        size_parameter_multiplier: i32,
    },
    /// `NativeType.SafeArray` (Cecil `SafeArrayMarshalInfo`).
    SafeArray { element_type: u8 },
    /// `NativeType.FixedArray` (Cecil `FixedArrayMarshalInfo`).
    FixedArray { size: i32, element_type: u8 },
    /// `NativeType.FixedSysString`.
    FixedSysString { size: i32 },
    /// `NativeType.CustomMarshaler`; `managed_type` is the serialized type
    /// name (Cecil parses it with `TypeParser`). `guid` is parsed as
    /// `new Guid(string)` parses it, and is all zeros when the blob's is empty.
    CustomMarshaler {
        guid: Guid,
        unmanaged_type: &'a str,
        managed_type: &'a str,
        cookie: &'a str,
    },
}

impl<'a> MarshalInfo<'a> {
    pub fn native_type(&self) -> u8 {
        match self {
            MarshalInfo::Simple { native } => *native,
            MarshalInfo::Array { .. } => native_type::ARRAY,
            MarshalInfo::SafeArray { .. } => native_type::SAFE_ARRAY,
            MarshalInfo::FixedArray { .. } => native_type::FIXED_ARRAY,
            MarshalInfo::FixedSysString { .. } => native_type::FIXED_SYS_STRING,
            MarshalInfo::CustomMarshaler { .. } => native_type::CUSTOM_MARSHALER,
        }
    }

    pub fn parse(blob: &'a [u8]) -> Result<MarshalInfo<'a>> {
        let mut r = SigReader::new(blob);
        let native = r.cur.u8()?;
        Ok(match native {
            native_type::ARRAY => {
                let mut element_type = native_type::NONE;
                let mut size_parameter_index = -1;
                let mut size = -1;
                let mut size_parameter_multiplier = -1;
                if r.can_read_more() {
                    element_type = r.cur.u8()?;
                }
                if r.can_read_more() {
                    size_parameter_index = r.cur.compressed_u32()? as i32;
                }
                if r.can_read_more() {
                    size = r.cur.compressed_u32()? as i32;
                }
                if r.can_read_more() {
                    size_parameter_multiplier = r.cur.compressed_u32()? as i32;
                }
                MarshalInfo::Array {
                    element_type,
                    size_parameter_index,
                    size,
                    size_parameter_multiplier,
                }
            }
            native_type::SAFE_ARRAY => {
                let mut element_type = variant_type::NONE;
                if r.can_read_more() {
                    element_type = r.cur.u8()?;
                }
                MarshalInfo::SafeArray { element_type }
            }
            native_type::FIXED_ARRAY => {
                let mut size = 0;
                let mut element_type = native_type::NONE;
                if r.can_read_more() {
                    size = r.cur.compressed_u32()? as i32;
                }
                if r.can_read_more() {
                    element_type = r.cur.u8()?;
                }
                MarshalInfo::FixedArray { size, element_type }
            }
            native_type::FIXED_SYS_STRING => {
                let mut size = -1;
                if r.can_read_more() {
                    size = r.cur.compressed_u32()? as i32;
                }
                MarshalInfo::FixedSysString { size }
            }
            native_type::CUSTOM_MARSHALER => {
                let at = r.cur.pos();
                let guid = match r.utf8_string()? {
                    None | Some("") => Guid::default(),
                    Some(text) => Guid::parse(text).ok_or(Error::bad(
                        at,
                        "custom marshaler GUID not in a .NET Guid format",
                    ))?,
                };
                let unmanaged_type = r.utf8_string()?.unwrap_or("");
                let managed_type = r.utf8_string()?.unwrap_or("");
                let cookie = r.utf8_string()?.unwrap_or("");
                MarshalInfo::CustomMarshaler {
                    guid,
                    unmanaged_type,
                    managed_type,
                    cookie,
                }
            }
            native => MarshalInfo::Simple { native },
        })
    }
}
