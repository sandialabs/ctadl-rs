//! Metadata tokens and token types.
//!
//! Port of `Mono.Cecil.Metadata/{MetadataToken,TokenType}.cs`.

use std::fmt;

/// The table (or heap) a metadata token points into. Values are the token's
/// high byte shifted into place, exactly as in Cecil's `TokenType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TokenType {
    Module = 0x0000_0000,
    TypeRef = 0x0100_0000,
    TypeDef = 0x0200_0000,
    Field = 0x0400_0000,
    Method = 0x0600_0000,
    Param = 0x0800_0000,
    InterfaceImpl = 0x0900_0000,
    MemberRef = 0x0a00_0000,
    CustomAttribute = 0x0c00_0000,
    Permission = 0x0e00_0000,
    Signature = 0x1100_0000,
    Event = 0x1400_0000,
    Property = 0x1700_0000,
    ModuleRef = 0x1a00_0000,
    TypeSpec = 0x1b00_0000,
    Assembly = 0x2000_0000,
    AssemblyRef = 0x2300_0000,
    File = 0x2600_0000,
    ExportedType = 0x2700_0000,
    ManifestResource = 0x2800_0000,
    GenericParam = 0x2a00_0000,
    MethodSpec = 0x2b00_0000,
    GenericParamConstraint = 0x2c00_0000,

    Document = 0x3000_0000,
    MethodDebugInformation = 0x3100_0000,
    LocalScope = 0x3200_0000,
    LocalVariable = 0x3300_0000,
    LocalConstant = 0x3400_0000,
    ImportScope = 0x3500_0000,
    StateMachineMethod = 0x3600_0000,
    CustomDebugInformation = 0x3700_0000,

    String = 0x7000_0000,
}

impl TokenType {
    pub fn from_high_byte(byte: u8) -> Option<TokenType> {
        use TokenType::*;
        Some(match byte {
            0x00 => Module,
            0x01 => TypeRef,
            0x02 => TypeDef,
            0x04 => Field,
            0x06 => Method,
            0x08 => Param,
            0x09 => InterfaceImpl,
            0x0a => MemberRef,
            0x0c => CustomAttribute,
            0x0e => Permission,
            0x11 => Signature,
            0x14 => Event,
            0x17 => Property,
            0x1a => ModuleRef,
            0x1b => TypeSpec,
            0x20 => Assembly,
            0x23 => AssemblyRef,
            0x26 => File,
            0x27 => ExportedType,
            0x28 => ManifestResource,
            0x2a => GenericParam,
            0x2b => MethodSpec,
            0x2c => GenericParamConstraint,
            0x30 => Document,
            0x31 => MethodDebugInformation,
            0x32 => LocalScope,
            0x33 => LocalVariable,
            0x34 => LocalConstant,
            0x35 => ImportScope,
            0x36 => StateMachineMethod,
            0x37 => CustomDebugInformation,
            0x70 => String,
            _ => return None,
        })
    }
}

/// A 32-bit metadata token: token type in the high byte, rid in the low 24 bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MetadataToken(pub u32);

impl MetadataToken {
    pub const ZERO: MetadataToken = MetadataToken(0);

    pub fn new(token_type: TokenType, rid: u32) -> MetadataToken {
        MetadataToken(token_type as u32 | rid)
    }

    pub fn rid(self) -> u32 {
        self.0 & 0x00ff_ffff
    }

    pub fn token_type(self) -> Option<TokenType> {
        TokenType::from_high_byte((self.0 >> 24) as u8)
    }

    pub fn is_null(self) -> bool {
        self.rid() == 0
    }

    pub fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for MetadataToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MetadataToken({:#010x})", self.0)
    }
}

impl fmt::Display for MetadataToken {
    // Cecil MetadataToken.ToString: "[{TokenType}:0x{RID:x4}]"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.token_type() {
            Some(tt) => write!(f, "[{:?}:0x{:04x}]", tt, self.rid()),
            None => write!(f, "[0x{:02x}?:0x{:04x}]", self.0 >> 24, self.rid()),
        }
    }
}
