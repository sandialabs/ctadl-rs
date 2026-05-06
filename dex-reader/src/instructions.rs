use std::fmt::Formatter;

use streaming_iterator::{DoubleEndedStreamingIterator, StreamingIterator};

use crate::types::{DexConstantPool, MethodId};

macro_rules! index_type {
    ($($nm: ident),*) => {
        $(
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(transparent)]
        pub struct $nm(pub u32);
        impl<T: Into<u32>> From<T> for $nm {
            fn from(value: T) -> Self {
                Self(value.into())
            }
        }
        impl $nm {
            #[inline]
            pub fn next_reg(&self) -> Self {
                Self(self.0.checked_add(1).unwrap())
            }
        }
        )*
    };
}

index_type!(
    Reg, TypeIdx, ProtoIdx, MethodIdx, CallIdx, FieldIdx, StringIdx
);

pub trait DisplayInstr {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error>;
    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error>;
}

impl<T: DisplayInstr> DisplayInstr for &T {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        (**self).display_raw(f)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        (**self).display(f, cp)
    }
}

impl DisplayInstr for Reg {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "v{}", self.0)
    }
}

impl DisplayInstr for TypeIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        let ty = cp
            .type_ids
            .get(self.0 as usize)
            .and_then(|t| t.descriptor(&cp.strings).ok())
            .unwrap_or("<invalid_type>".into());
        write!(f, "{ty}")
    }
}

impl DisplayInstr for StringIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        let s = cp
            .strings
            .get(self.0 as usize)
            .and_then(|s: String| Ok(format!("\"{}\"", s.escape_debug())))
            .unwrap_or("<invalid_type>".into());
        write!(f, "{s}")
    }
}

impl DisplayInstr for ProtoIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "proto<{}>", self.0)
    }
}

impl DisplayInstr for MethodIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        let sig = cp
            .method_ids
            .get(self.0 as usize)
            .and_then(|m: &MethodId| {
                m.signature(&cp.strings, &cp.type_ids, &cp.proto_ids, cp.data)
                    .ok()
            })
            .unwrap_or("<invalid_method>".to_string());

        write!(f, "{}", sig)
    }
}

impl DisplayInstr for CallIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "call<{}>", self.0)
    }
}

impl DisplayInstr for FieldIdx {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        let fld = cp
            .field_ids
            .get(self.0 as usize)
            .and_then(|f| f.pretty_name(cp).ok())
            .unwrap_or("<invalid_field>".into());
        write!(f, "{}", fld)
    }
}

/// Payload instructions (separate from regular instructions)
#[derive(Debug, Clone)]
pub enum PayloadInstruction {
    PackedSwitch(crate::types::PackedSwitchPayload),
    SparseSwitch(crate::types::SparseSwitchPayload),
    FillArrayData(crate::types::FillArrayDataPayload),
}

impl PayloadInstruction {
    /// Branch targets (relative to the switch instruction) for packed/sparse switch payloads; empty otherwise.
    pub fn targets(&self) -> &[i32] {
        match self {
            PayloadInstruction::PackedSwitch(p) => &p.targets,
            PayloadInstruction::SparseSwitch(p) => &p.targets,
            PayloadInstruction::FillArrayData(_) => &[],
        }
    }
}

pub struct RawArg<T>(pub T);

impl<T: DisplayInstr> std::fmt::Display for RawArg<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.display_raw(f)
    }
}

pub struct DispArg<'a, 'b, T>(pub T, pub &'a DexConstantPool<'b>);

impl<'a, 'b, T: DisplayInstr> std::fmt::Display for DispArg<'a, 'b, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.display(f, self.1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format10x;
impl Format10x {
    pub fn new(_insns: &[u16]) -> (usize, Self) {
        (1, Self)
    }
}

impl DisplayInstr for Format10x {
    fn display_raw(&self, _f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        Ok(())
    }

    fn display(
        &self,
        _f: &mut Formatter<'_>,
        _cp: &DexConstantPool,
    ) -> Result<(), std::fmt::Error> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format11x {
    pub a: Reg,
}
impl Format11x {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        (1, Self { a })
    }
}

impl DisplayInstr for Format11x {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.a.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}", DispArg(self.a, cp))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format12x {
    pub a: Reg,
    pub b: Reg,
}
impl Format12x {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f).into();
        let b = ((insns[0] >> 12) & 0x0f).into();
        (1, Self { a, b })
    }
}

impl DisplayInstr for Format12x {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.b.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), DispArg(self.b, cp))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format11n {
    pub a: Reg,
    pub lit: i8,
}
impl Format11n {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f).into();
        let lit = (((insns[0] >> 8) & 0xf0) as i8) >> 4;
        (1, Self { a, lit })
    }
}

impl DisplayInstr for Format11n {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.lit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format21s {
    pub a: Reg,
    pub lit: i16,
}
impl Format21s {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let lit = insns[1] as i16;
        (2, Self { a, lit })
    }
}

impl DisplayInstr for Format21s {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.lit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format23x {
    pub a: Reg,
    pub b: Reg,
    pub c: Reg,
}
impl Format23x {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let b = (insns[1] & 0xFF).into();
        let c = ((insns[1] >> 8) & 0xFF).into();

        (2, Self { a, b, c })
    }
}

impl DisplayInstr for Format23x {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}, {}", self.a.0, self.b.0, self.c.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}",
            DispArg(self.a, cp),
            DispArg(self.b, cp),
            DispArg(self.c, cp)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format31i {
    pub a: Reg,
    pub lit: i32,
}
impl Format31i {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let lit = (((insns[2] as u32) << 16) | (insns[1] as u32)) as i32;
        (3, Self { a, lit })
    }
}

impl DisplayInstr for Format31i {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.lit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format31t {
    pub a: Reg,
    pub tgt: i32,
}
impl Format31t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let w0 = insns[1] as u32;
        let w1 = insns[2] as u32;
        let tgt = ((w1 << 16) | w0) as i32;
        (3, Self { a, tgt })
    }
}

impl DisplayInstr for Format31t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.tgt)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.tgt)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format51l {
    pub a: Reg,
    pub lit: i64,
}
impl Format51l {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();

        let w0 = insns[1] as u64;
        let w1 = insns[2] as u64;
        let w2 = insns[3] as u64;
        let w3 = insns[4] as u64;

        let lit = ((w3 << 48) | (w2 << 32) | (w1 << 16) | w0) as i64;

        (5, Self { a, lit })
    }
}

impl DisplayInstr for Format51l {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}L", self.a.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}L", DispArg(self.a, cp), self.lit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format21h {
    pub a: Reg,
    pub lit: i16,
}
impl Format21h {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let lit = insns[1] as i16;
        (2, Self { a, lit })
    }
}

impl DisplayInstr for Format21h {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.lit)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format22b {
    pub a: Reg,
    pub b: Reg,
    pub c: i8,
}
impl Format22b {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let b = (insns[1] & 0xff).into();
        let c = ((insns[1] >> 8) & 0xff) as u8 as i8;
        (2, Self { a, b, c })
    }
}

impl DisplayInstr for Format22b {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}, {}", self.a.0, self.b.0, self.c)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}",
            DispArg(self.a, cp),
            DispArg(self.b, cp),
            self.c
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format21c<T> {
    pub a: Reg,
    pub idx: T,
}
impl<T: From<u16>> Format21c<T> {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let idx = insns[1].into();
        (2, Self { a, idx })
    }
}

impl<T: DisplayInstr + Copy> DisplayInstr for Format21c<T> {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, RawArg(self.idx))
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), DispArg(self.idx, cp))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format21t {
    pub a: Reg,
    pub tgt: i32,
}
impl Format21t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let tgt = insns[1] as i16 as i32;
        (2, Self { a, tgt })
    }
}

impl DisplayInstr for Format21t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.tgt)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), self.tgt)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format22c<T> {
    pub a: Reg,
    pub b: Reg,
    pub idx: T,
}
impl<T: From<u16>> Format22c<T> {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f).into();
        let b = ((insns[0] >> 12) & 0x0f).into();
        let idx = insns[1].into();
        (2, Self { a, b, idx })
    }
}

impl<T: DisplayInstr + Copy> DisplayInstr for Format22c<T> {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}, {}", self.a.0, self.b.0, RawArg(self.idx))
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}",
            DispArg(self.a, cp),
            DispArg(self.b, cp),
            DispArg(self.idx, cp)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format22x {
    pub a: Reg,
    pub b: Reg,
}
impl Format22x {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let b = insns[1].into();
        (2, Self { a, b })
    }
}

impl DisplayInstr for Format22x {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.b.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), DispArg(self.b, cp),)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format22s {
    pub a: Reg,
    pub b: Reg,
    pub lit: i16,
}
impl Format22s {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f).into();
        let b = ((insns[0] >> 12) & 0x0f).into();
        let lit = insns[1] as i16;
        (2, Self { a, b, lit })
    }
}

impl DisplayInstr for Format22s {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}, {}", self.a.0, self.b.0, self.lit)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}",
            DispArg(self.a, cp),
            DispArg(self.b, cp),
            self.lit
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format22t {
    pub a: Reg,
    pub b: Reg,
    pub tgt: i16,
}
impl Format22t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f).into();
        let b = ((insns[0] >> 12) & 0x0f).into();
        let tgt = insns[1] as i16;
        (2, Self { a, b, tgt })
    }
}
impl DisplayInstr for Format22t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}, {}", self.a.0, self.b.0, self.tgt)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}",
            DispArg(self.a, cp),
            DispArg(self.b, cp),
            self.tgt
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format31c {
    pub a: Reg,
    pub idx: StringIdx,
}
impl Format31c {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0xff).into();
        let w0 = insns[1] as u32;
        let w1 = insns[2] as u32;
        let idx = ((w1 << 16) | w0).into();
        (3, Self { a, idx })
    }
}
impl DisplayInstr for Format31c {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, RawArg(self.idx))
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), DispArg(self.idx, cp),)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format32x {
    pub a: Reg,
    pub b: Reg,
}
impl Format32x {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = insns[1].into();
        let b = insns[2].into();
        (3, Self { a, b })
    }
}
impl DisplayInstr for Format32x {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", self.a.0, self.b.0)
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}, {}", DispArg(self.a, cp), DispArg(self.b, cp),)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format10t {
    pub off: i8,
}
impl Format10t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let off = (insns[0] >> 8) as i8;
        (1, Self { off })
    }
}
impl DisplayInstr for Format10t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format20t {
    pub off: i16,
}
impl Format20t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let off = insns[1] as i16;
        (2, Self { off })
    }
}
impl DisplayInstr for Format20t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format30t {
    pub off: i32,
}
impl Format30t {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let lo = insns[1] as u32;
        let hi = insns[2] as u32;
        let off = ((hi << 16) | lo) as i32;
        (3, Self { off })
    }
}
impl DisplayInstr for Format30t {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.off)
    }
}

#[derive(Debug, Clone)]
pub struct Format35c<T> {
    pub idx: T,
    pub args: Vec<Reg>,
}
impl<T: From<u16>> Format35c<T> {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let count: usize = ((insns[0] >> 12) & 0x0f).into();
        let g = ((insns[0] >> 8) & 0x0f).into();
        let idx = insns[1].into();
        let c = (insns[2] & 0xF).into();
        let d = ((insns[2] >> 4) & 0xF).into();
        let e = ((insns[2] >> 8) & 0xF).into();
        let f = ((insns[2] >> 12) & 0xF).into();
        let mut args: Vec<Reg> = Vec::with_capacity(count);
        if count > 0 {
            args.push(c);
        }
        if count > 1 {
            args.push(d);
        }
        if count > 2 {
            args.push(e);
        }
        if count > 3 {
            args.push(f);
        }
        if count > 4 {
            args.push(g);
        }
        (3, Self { idx, args })
    }
}
impl<T: DisplayInstr + Copy> DisplayInstr for Format35c<T> {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        // Smali-style order: {regs}, idx
        write!(f, "{{")?;
        for (i, arg) in self.args.iter().enumerate() {
            if i != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", RawArg(arg))?;
        }
        write!(f, "}}, {}", RawArg(self.idx))
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // Smali/baksmali style for invokes / filled-new-array:
        //   {v0, v1, ...}, Lcls;->method(II)V
        write!(f, "{{")?;
        for (i, arg) in self.args.iter().enumerate() {
            if i != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", DispArg(*arg, cp))?;
        }
        write!(f, "}}, {}", DispArg(self.idx, cp))
    }
}

#[derive(Debug, Clone)]
pub struct Format3rc<T> {
    pub idx: T,
    pub args: Vec<Reg>,
}
impl<T: From<u16>> Format3rc<T> {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let size: u32 = ((insns[0] >> 8) & 0xff).into();
        let idx: T = insns[1].into();
        let first: u32 = insns[2].into();
        let args = (first..(first + size)).map(|r| Reg::from(r)).collect();
        (3, Self { idx, args })
    }
}

impl<T: DisplayInstr + Copy> DisplayInstr for Format3rc<T> {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        // Smali-style order: {vX .. vY}, idx
        match (self.args.first(), self.args.last()) {
            (Some(first), Some(last)) => write!(
                f,
                "{{{} .. {}}}, {}",
                RawArg(*first),
                RawArg(*last),
                RawArg(self.idx)
            ),
            _ => write!(f, "{{}}, {}", RawArg(self.idx)),
        }
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // Smali/baksmali style for invoke-*/range:
        //   {vX .. vY}, Lcls;->method(...)V
        match (self.args.first(), self.args.last()) {
            (Some(first), Some(last)) => write!(
                f,
                "{{{} .. {}}}, {}",
                DispArg(*first, cp),
                DispArg(*last, cp),
                DispArg(self.idx, cp)
            ),
            _ => write!(f, "{{}}, {}", DispArg(self.idx, cp)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Format45cc {
    pub a: u32,
    pub method_id: MethodIdx,
    pub c: Reg,
    pub d: Reg,
    pub e: Reg,
    pub f: Reg,
    pub g: Reg,
    pub proto_id: ProtoIdx,
}

impl Format45cc {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let a = ((insns[0] >> 8) & 0x0f) as u32;
        let method_id = MethodIdx(insns[1].into());
        let c = Reg((insns[2] & 0x0f).into());
        let d = Reg(((insns[2] >> 4) & 0x0f).into());
        let e = Reg(((insns[2] >> 8) & 0x0f).into());
        let f = Reg(((insns[2] >> 12) & 0x0f).into());
        let g = Reg(((insns[0] >> 12) & 0x0f).into());
        let proto_id = ProtoIdx(insns[3].into());
        (
            4,
            Self {
                a,
                method_id,
                c,
                d,
                e,
                f,
                g,
                proto_id,
            },
        )
    }
}
impl DisplayInstr for Format45cc {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "{}, {}, {}, {}, {}, {}, {}, {}",
            self.a,
            RawArg(self.method_id),
            RawArg(self.c),
            RawArg(self.d),
            RawArg(self.e),
            RawArg(self.f),
            RawArg(self.g),
            RawArg(self.proto_id),
        )
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // Smali-style order:
        //   {regs}, method, proto
        let mut regs: Vec<Reg> = Vec::with_capacity(self.a as usize);
        if self.a > 0 {
            regs.push(self.c);
        }
        if self.a > 1 {
            regs.push(self.d);
        }
        if self.a > 2 {
            regs.push(self.e);
        }
        if self.a > 3 {
            regs.push(self.f);
        }
        if self.a > 4 {
            regs.push(self.g);
        }

        write!(f, "{{")?;
        for (i, r) in regs.iter().enumerate() {
            if i != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", DispArg(*r, cp))?;
        }
        write!(
            f,
            "}}, {}, {}",
            DispArg(self.method_id, cp),
            DispArg(self.proto_id, cp)
        )
    }
}

#[derive(Debug, Clone)]
pub struct Format4rcc {
    pub method_id: MethodIdx,
    pub args: Vec<Reg>,
    pub proto_id: ProtoIdx,
}

impl Format4rcc {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        let size = ((insns[0] >> 8) & 0xff) as u32;
        let method_id = MethodIdx(insns[1].into());
        let first: u32 = insns[2].into();
        let proto_id = ProtoIdx(insns[3].into());
        let args: Vec<Reg> = (first..(first + size)).map(|i| Reg::from(i)).collect();

        (
            4,
            Self {
                method_id,
                args,
                proto_id,
            },
        )
    }
}
impl DisplayInstr for Format4rcc {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}", RawArg(self.method_id))?;
        for arg in &self.args {
            write!(f, ", {}", RawArg(arg))?;
        }
        write!(f, ", {}", RawArg(self.proto_id))
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // Smali-style order:
        //   {vX .. vY}, method, proto
        match (self.args.first(), self.args.last()) {
            (Some(first), Some(last)) => write!(
                f,
                "{{{} .. {}}}, {}, {}",
                DispArg(*first, cp),
                DispArg(*last, cp),
                DispArg(self.method_id, cp),
                DispArg(self.proto_id, cp)
            ),
            _ => write!(
                f,
                "{{}}, {}, {}",
                DispArg(self.method_id, cp),
                DispArg(self.proto_id, cp)
            ),
        }
    }
}

pub struct FormatUnused;

impl FormatUnused {
    pub fn new(_insns: &[u16]) -> (usize, Self) {
        (1, Self)
    }
}

impl DisplayInstr for FormatUnused {
    fn display_raw(&self, _f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        Ok(())
    }

    fn display(
        &self,
        _f: &mut Formatter<'_>,
        _cp: &DexConstantPool,
    ) -> Result<(), std::fmt::Error> {
        Ok(())
    }
}

#[repr(u8)]
pub enum Instruction {
    Nop(Format10x) = 0x00,
    Move(Format12x) = 0x01,
    MoveFrom16(Format22x) = 0x02,
    Move16(Format32x) = 0x03,
    MoveWide(Format12x) = 0x04,
    MoveWideFrom16(Format22x) = 0x05,
    MoveWide16(Format32x) = 0x06,
    MoveObject(Format12x) = 0x07,
    MoveObjectFrom16(Format22x) = 0x08,
    MoveObject16(Format32x) = 0x09,
    MoveResult(Format11x) = 0x0a,
    MoveResultWide(Format11x) = 0x0b,
    MoveResultObject(Format11x) = 0x0c,
    MoveException(Format11x) = 0x0d,
    ReturnVoid(Format10x) = 0x0e,
    Return(Format11x) = 0x0f,
    ReturnWide(Format11x) = 0x10,
    ReturnObject(Format11x) = 0x11,
    Const4(Format11n) = 0x12,
    Const16(Format21s) = 0x13,
    Const(Format31i) = 0x14,
    ConstHigh16(Format21h) = 0x15,
    ConstWide16(Format21s) = 0x16,
    ConstWide32(Format31i) = 0x17,
    ConstWide(Format51l) = 0x18,
    ConstWideHigh16(Format21h) = 0x19,
    ConstString(Format21c<StringIdx>) = 0x1a,
    ConstStringJumbo(Format31c) = 0x1b,
    ConstClass(Format21c<TypeIdx>) = 0x1c,
    MonitorEnter(Format11x) = 0x1d,
    MonitorExit(Format11x) = 0x1e,
    CheckCast(Format21c<TypeIdx>) = 0x1f,
    InstanceOf(Format22c<TypeIdx>) = 0x20,
    ArrayLength(Format12x) = 0x21,
    NewInstance(Format21c<TypeIdx>) = 0x22,
    NewArray(Format22c<TypeIdx>) = 0x23,
    FilledNewArray(Format35c<TypeIdx>) = 0x24,
    FilledNewArrayRange(Format3rc<TypeIdx>) = 0x25,
    FillArrayData(Format31t) = 0x26,
    Throw(Format11x) = 0x27,
    Goto(Format10t) = 0x28,
    Goto16(Format20t) = 0x29,
    Goto32(Format30t) = 0x2a,
    PackedSwitch(Format31t) = 0x2b,
    SparseSwitch(Format31t) = 0x2c,
    CmplFloat(Format23x) = 0x2d,
    CmpgFloat(Format23x) = 0x2e,
    CmplDouble(Format23x) = 0x2f,
    CmpgDouble(Format23x) = 0x30,
    CmpLong(Format23x) = 0x31,
    IfEq(Format22t) = 0x32,
    IfNe(Format22t) = 0x33,
    IfLt(Format22t) = 0x34,
    IfGe(Format22t) = 0x35,
    IfGt(Format22t) = 0x36,
    IfLe(Format22t) = 0x37,
    IfEqz(Format21t) = 0x38,
    IfNez(Format21t) = 0x39,
    IfLtz(Format21t) = 0x3a,
    IfGez(Format21t) = 0x3b,
    IfGtz(Format21t) = 0x3c,
    IfLez(Format21t) = 0x3d,
    Unused3E(FormatUnused) = 0x3e,
    Unused3F(FormatUnused) = 0x3f,
    Unused40(FormatUnused) = 0x40,
    Unused41(FormatUnused) = 0x41,
    Unused42(FormatUnused) = 0x42,
    Unused43(FormatUnused) = 0x43,
    AGet(Format23x) = 0x44,
    AGetWide(Format23x) = 0x45,
    AGetObject(Format23x) = 0x46,
    AGetBoolean(Format23x) = 0x47,
    AGetByte(Format23x) = 0x48,
    AGetChar(Format23x) = 0x49,
    AGetShort(Format23x) = 0x4a,
    APut(Format23x) = 0x4b,
    APutWide(Format23x) = 0x4c,
    APutObject(Format23x) = 0x4d,
    APutBoolean(Format23x) = 0x4e,
    APutByte(Format23x) = 0x4f,
    APutChar(Format23x) = 0x50,
    APutShort(Format23x) = 0x51,
    IGet(Format22c<FieldIdx>) = 0x52,
    IGetWide(Format22c<FieldIdx>) = 0x53,
    IGetObject(Format22c<FieldIdx>) = 0x54,
    IGetBoolean(Format22c<FieldIdx>) = 0x55,
    IGetByte(Format22c<FieldIdx>) = 0x56,
    IGetChar(Format22c<FieldIdx>) = 0x57,
    IGetShort(Format22c<FieldIdx>) = 0x58,
    IPut(Format22c<FieldIdx>) = 0x59,
    IPutWide(Format22c<FieldIdx>) = 0x5a,
    IPutObject(Format22c<FieldIdx>) = 0x5b,
    IPutBoolean(Format22c<FieldIdx>) = 0x5c,
    IPutByte(Format22c<FieldIdx>) = 0x5d,
    IPutChar(Format22c<FieldIdx>) = 0x5e,
    IPutShort(Format22c<FieldIdx>) = 0x5f,
    SGet(Format21c<FieldIdx>) = 0x60,
    SGetWide(Format21c<FieldIdx>) = 0x61,
    SGetObject(Format21c<FieldIdx>) = 0x62,
    SGetBoolean(Format21c<FieldIdx>) = 0x63,
    SGetByte(Format21c<FieldIdx>) = 0x64,
    SGetChar(Format21c<FieldIdx>) = 0x65,
    SGetShort(Format21c<FieldIdx>) = 0x66,
    SPut(Format21c<FieldIdx>) = 0x67,
    SPutWide(Format21c<FieldIdx>) = 0x68,
    SPutObject(Format21c<FieldIdx>) = 0x69,
    SPutBoolean(Format21c<FieldIdx>) = 0x6a,
    SPutByte(Format21c<FieldIdx>) = 0x6b,
    SPutChar(Format21c<FieldIdx>) = 0x6c,
    SPutShort(Format21c<FieldIdx>) = 0x6d,
    InvokeVirtual(Format35c<MethodIdx>) = 0x6e,
    InvokeSuper(Format35c<MethodIdx>) = 0x6f,
    InvokeDirect(Format35c<MethodIdx>) = 0x70,
    InvokeStatic(Format35c<MethodIdx>) = 0x71,
    InvokeInterface(Format35c<MethodIdx>) = 0x72,
    Unused73(FormatUnused) = 0x73,
    InvokeVirtualRange(Format3rc<MethodIdx>) = 0x74,
    InvokeSuperRange(Format3rc<MethodIdx>) = 0x75,
    InvokeDirectRange(Format3rc<MethodIdx>) = 0x76,
    InvokeStaticRange(Format3rc<MethodIdx>) = 0x77,
    InvokeInterfaceRange(Format3rc<MethodIdx>) = 0x78,
    Unused79(FormatUnused) = 0x79,
    Unused7A(FormatUnused) = 0x7a,
    NegInt(Format12x) = 0x7b,
    NotInt(Format12x) = 0x7c,
    NegLong(Format12x) = 0x7d,
    NotLong(Format12x) = 0x7e,
    NegFloat(Format12x) = 0x7f,
    NegDouble(Format12x) = 0x80,
    IntToLong(Format12x) = 0x81,
    IntToFloat(Format12x) = 0x82,
    IntToDouble(Format12x) = 0x83,
    LongToInt(Format12x) = 0x84,
    LongToFloat(Format12x) = 0x85,
    LongToDouble(Format12x) = 0x86,
    FloatToInt(Format12x) = 0x87,
    FloatToLong(Format12x) = 0x88,
    FloatToDouble(Format12x) = 0x89,
    DoubleToInt(Format12x) = 0x8a,
    DoubleToLong(Format12x) = 0x8b,
    DoubleToFloat(Format12x) = 0x8c,
    IntToByte(Format12x) = 0x8d,
    IntToChar(Format12x) = 0x8e,
    IntToShort(Format12x) = 0x8f,
    AddInt(Format23x) = 0x90,
    SubInt(Format23x) = 0x91,
    MulInt(Format23x) = 0x92,
    DivInt(Format23x) = 0x93,
    RemInt(Format23x) = 0x94,
    AndInt(Format23x) = 0x95,
    OrInt(Format23x) = 0x96,
    XorInt(Format23x) = 0x97,
    ShlInt(Format23x) = 0x98,
    ShrInt(Format23x) = 0x99,
    UshrInt(Format23x) = 0x9a,
    AddLong(Format23x) = 0x9b,
    SubLong(Format23x) = 0x9c,
    MulLong(Format23x) = 0x9d,
    DivLong(Format23x) = 0x9e,
    RemLong(Format23x) = 0x9f,
    AndLong(Format23x) = 0xa0,
    OrLong(Format23x) = 0xa1,
    XorLong(Format23x) = 0xa2,
    ShlLong(Format23x) = 0xa3,
    ShrLong(Format23x) = 0xa4,
    UshrLong(Format23x) = 0xa5,
    AddFloat(Format23x) = 0xa6,
    SubFloat(Format23x) = 0xa7,
    MulFloat(Format23x) = 0xa8,
    DivFloat(Format23x) = 0xa9,
    RemFloat(Format23x) = 0xaa,
    AddDouble(Format23x) = 0xab,
    SubDouble(Format23x) = 0xac,
    MulDouble(Format23x) = 0xad,
    DivDouble(Format23x) = 0xae,
    RemDouble(Format23x) = 0xaf,
    AddInt2Addr(Format12x) = 0xb0,
    SubInt2Addr(Format12x) = 0xb1,
    MulInt2Addr(Format12x) = 0xb2,
    DivInt2Addr(Format12x) = 0xb3,
    RemInt2Addr(Format12x) = 0xb4,
    AndInt2Addr(Format12x) = 0xb5,
    OrInt2Addr(Format12x) = 0xb6,
    XorInt2Addr(Format12x) = 0xb7,
    ShlInt2Addr(Format12x) = 0xb8,
    ShrInt2Addr(Format12x) = 0xb9,
    UshrInt2Addr(Format12x) = 0xba,
    AddLong2Addr(Format12x) = 0xbb,
    SubLong2Addr(Format12x) = 0xbc,
    MulLong2Addr(Format12x) = 0xbd,
    DivLong2Addr(Format12x) = 0xbe,
    RemLong2Addr(Format12x) = 0xbf,
    AndLong2Addr(Format12x) = 0xc0,
    OrLong2Addr(Format12x) = 0xc1,
    XorLong2Addr(Format12x) = 0xc2,
    ShlLong2Addr(Format12x) = 0xc3,
    ShrLong2Addr(Format12x) = 0xc4,
    UshrLong2Addr(Format12x) = 0xc5,
    AddFloat2Addr(Format12x) = 0xc6,
    SubFloat2Addr(Format12x) = 0xc7,
    MulFloat2Addr(Format12x) = 0xc8,
    DivFloat2Addr(Format12x) = 0xc9,
    RemFloat2Addr(Format12x) = 0xca,
    AddDouble2Addr(Format12x) = 0xcb,
    SubDouble2Addr(Format12x) = 0xcc,
    MulDouble2Addr(Format12x) = 0xcd,
    DivDouble2Addr(Format12x) = 0xce,
    RemDouble2Addr(Format12x) = 0xcf,
    AddIntLit16(Format22s) = 0xd0,
    RsubInt(Format22s) = 0xd1,
    MulIntLit16(Format22s) = 0xd2,
    DivIntLit16(Format22s) = 0xd3,
    RemIntLit16(Format22s) = 0xd4,
    AndIntLit16(Format22s) = 0xd5,
    OrIntLit16(Format22s) = 0xd6,
    XorIntLit16(Format22s) = 0xd7,
    AddIntLit8(Format22b) = 0xd8,
    RsubIntLit8(Format22b) = 0xd9,
    MulIntLit8(Format22b) = 0xda,
    DivIntLit8(Format22b) = 0xdb,
    RemIntLit8(Format22b) = 0xdc,
    AndIntLit8(Format22b) = 0xdd,
    OrIntLit8(Format22b) = 0xde,
    XorIntLit8(Format22b) = 0xdf,
    ShlIntLit8(Format22b) = 0xe0,
    ShrIntLit8(Format22b) = 0xe1,
    UshrIntLit8(Format22b) = 0xe2,
    UnusedE3(FormatUnused) = 0xe3,
    UnusedE4(FormatUnused) = 0xe4,
    UnusedE5(FormatUnused) = 0xe5,
    UnusedE6(FormatUnused) = 0xe6,
    UnusedE7(FormatUnused) = 0xe7,
    UnusedE8(FormatUnused) = 0xe8,
    UnusedE9(FormatUnused) = 0xe9,
    UnusedEA(FormatUnused) = 0xea,
    UnusedEB(FormatUnused) = 0xeb,
    UnusedEC(FormatUnused) = 0xec,
    UnusedED(FormatUnused) = 0xed,
    UnusedEE(FormatUnused) = 0xee,
    UnusedEF(FormatUnused) = 0xef,
    UnusedF0(FormatUnused) = 0xf0,
    UnusedF1(FormatUnused) = 0xf1,
    UnusedF2(FormatUnused) = 0xf2,
    UnusedF3(FormatUnused) = 0xf3,
    UnusedF4(FormatUnused) = 0xf4,
    UnusedF5(FormatUnused) = 0xf5,
    UnusedF6(FormatUnused) = 0xf6,
    UnusedF7(FormatUnused) = 0xf7,
    UnusedF8(FormatUnused) = 0xf8,
    UnusedF9(FormatUnused) = 0xf9,
    UnusedFA(Format45cc) = 0xfa,
    UnusedFB(Format4rcc) = 0xfb,
    UnusedFC(Format35c<CallIdx>) = 0xfc,
    UnusedFD(Format3rc<CallIdx>) = 0xfd,
    UnusedFE(Format21c<MethodIdx>) = 0xfe,
    UnusedFF(Format21c<ProtoIdx>) = 0xff,
}

/// Represents data flow registers accessed by an instruction
pub struct DataFlow {
    /// Source registers for the instruction
    pub source: DynIter,
    /// Dest registers for the instruction
    pub dest: DynIter,
    /// Ret registers for the instruction
    pub ret: DynIter,
}

type DynIter = Box<dyn DoubleEndedStreamingIterator<Item = Reg>>;

/// Streaming iterator over a slice of registers (used for 35c/3rc args).
struct RegSliceStream {
    vec: Vec<Reg>,
    pos: usize,
}

impl RegSliceStream {
    fn new(slice: &[Reg]) -> Self {
        Self {
            vec: slice.to_vec(),
            pos: 0,
        }
    }
}

impl StreamingIterator for RegSliceStream {
    type Item = Reg;
    fn advance(&mut self) {
        self.pos = (self.pos + 1).min(self.vec.len());
    }
    fn get(&self) -> Option<&Reg> {
        self.vec.get(self.pos)
    }
}

impl DoubleEndedStreamingIterator for RegSliceStream {
    fn advance_back(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }
}

fn dataflow_from_args(args: &[Reg]) -> DataFlow {
    DataFlow {
        source: Box::new(RegSliceStream::new(args)),
        dest: Box::new(streaming_iterator::empty()),
        ret: Box::new(streaming_iterator::empty()),
    }
}

macro_rules! iter_chain {
    ([]) => {
        streaming_iterator::empty()
    };
    ([$first:expr]) => {
        streaming_iterator::once($first)
    };
    ([$first:expr, $($rest:expr),+ $(,)?]) => {
        streaming_iterator::once($first).chain(iter_chain!([$($rest),+]))
    };
}

/// Given arrays of references to source, dest, and return Reg information, build iterators over
/// them and pack them into a DataFlow
macro_rules! dataflow_source_sink_ret {
    ($source:tt, $dest:tt, $ret:tt) => {
        DataFlow {
            source: Box::new(iter_chain!($source)),
            dest: Box::new(iter_chain!($dest)),
            ret: Box::new(iter_chain!($ret)),
        }
    };
}

impl Instruction {
    pub fn new(insns: &[u16]) -> (usize, Self) {
        match (insns[0] & 0xFF) as u8 {
            0x0 => {
                let (size, fmt) = Format10x::new(insns);
                (size, Instruction::Nop(fmt))
            }
            0x1 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::Move(fmt))
            }
            0x2 => {
                let (size, fmt) = Format22x::new(insns);
                (size, Instruction::MoveFrom16(fmt))
            }
            0x3 => {
                let (size, fmt) = Format32x::new(insns);
                (size, Instruction::Move16(fmt))
            }
            0x4 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MoveWide(fmt))
            }
            0x5 => {
                let (size, fmt) = Format22x::new(insns);
                (size, Instruction::MoveWideFrom16(fmt))
            }
            0x6 => {
                let (size, fmt) = Format32x::new(insns);
                (size, Instruction::MoveWide16(fmt))
            }
            0x7 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MoveObject(fmt))
            }
            0x8 => {
                let (size, fmt) = Format22x::new(insns);
                (size, Instruction::MoveObjectFrom16(fmt))
            }
            0x9 => {
                let (size, fmt) = Format32x::new(insns);
                (size, Instruction::MoveObject16(fmt))
            }
            0xa => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MoveResult(fmt))
            }
            0xb => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MoveResultWide(fmt))
            }
            0xc => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MoveResultObject(fmt))
            }
            0xd => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MoveException(fmt))
            }
            0xe => {
                let (size, fmt) = Format10x::new(insns);
                (size, Instruction::ReturnVoid(fmt))
            }
            0xf => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::Return(fmt))
            }
            0x10 => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::ReturnWide(fmt))
            }
            0x11 => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::ReturnObject(fmt))
            }
            0x12 => {
                let (size, fmt) = Format11n::new(insns);
                (size, Instruction::Const4(fmt))
            }
            0x13 => {
                let (size, fmt) = Format21s::new(insns);
                (size, Instruction::Const16(fmt))
            }
            0x14 => {
                let (size, fmt) = Format31i::new(insns);
                (size, Instruction::Const(fmt))
            }
            0x15 => {
                let (size, fmt) = Format21h::new(insns);
                (size, Instruction::ConstHigh16(fmt))
            }
            0x16 => {
                let (size, fmt) = Format21s::new(insns);
                (size, Instruction::ConstWide16(fmt))
            }
            0x17 => {
                let (size, fmt) = Format31i::new(insns);
                (size, Instruction::ConstWide32(fmt))
            }
            0x18 => {
                let (size, fmt) = Format51l::new(insns);
                (size, Instruction::ConstWide(fmt))
            }
            0x19 => {
                let (size, fmt) = Format21h::new(insns);
                (size, Instruction::ConstWideHigh16(fmt))
            }
            0x1a => {
                let (size, fmt) = Format21c::<StringIdx>::new(insns);
                (size, Instruction::ConstString(fmt))
            }
            0x1b => {
                let (size, fmt) = Format31c::new(insns);
                (size, Instruction::ConstStringJumbo(fmt))
            }
            0x1c => {
                let (size, fmt) = Format21c::<TypeIdx>::new(insns);
                (size, Instruction::ConstClass(fmt))
            }
            0x1d => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MonitorEnter(fmt))
            }
            0x1e => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::MonitorExit(fmt))
            }
            0x1f => {
                let (size, fmt) = Format21c::<TypeIdx>::new(insns);
                (size, Instruction::CheckCast(fmt))
            }
            0x20 => {
                let (size, fmt) = Format22c::<TypeIdx>::new(insns);
                (size, Instruction::InstanceOf(fmt))
            }
            0x21 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::ArrayLength(fmt))
            }
            0x22 => {
                let (size, fmt) = Format21c::<TypeIdx>::new(insns);
                (size, Instruction::NewInstance(fmt))
            }
            0x23 => {
                let (size, fmt) = Format22c::<TypeIdx>::new(insns);
                (size, Instruction::NewArray(fmt))
            }
            0x24 => {
                let (size, fmt) = Format35c::<TypeIdx>::new(insns);
                (size, Instruction::FilledNewArray(fmt))
            }
            0x25 => {
                let (size, fmt) = Format3rc::<TypeIdx>::new(insns);
                (size, Instruction::FilledNewArrayRange(fmt))
            }
            0x26 => {
                let (size, fmt) = Format31t::new(insns);
                (size, Instruction::FillArrayData(fmt))
            }
            0x27 => {
                let (size, fmt) = Format11x::new(insns);
                (size, Instruction::Throw(fmt))
            }
            0x28 => {
                let (size, fmt) = Format10t::new(insns);
                (size, Instruction::Goto(fmt))
            }
            0x29 => {
                let (size, fmt) = Format20t::new(insns);
                (size, Instruction::Goto16(fmt))
            }
            0x2a => {
                let (size, fmt) = Format30t::new(insns);
                (size, Instruction::Goto32(fmt))
            }
            0x2b => {
                let (size, fmt) = Format31t::new(insns);
                (size, Instruction::PackedSwitch(fmt))
            }
            0x2c => {
                let (size, fmt) = Format31t::new(insns);
                (size, Instruction::SparseSwitch(fmt))
            }
            0x2d => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::CmplFloat(fmt))
            }
            0x2e => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::CmpgFloat(fmt))
            }
            0x2f => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::CmplDouble(fmt))
            }
            0x30 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::CmpgDouble(fmt))
            }
            0x31 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::CmpLong(fmt))
            }
            0x32 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfEq(fmt))
            }
            0x33 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfNe(fmt))
            }
            0x34 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfLt(fmt))
            }
            0x35 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfGe(fmt))
            }
            0x36 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfGt(fmt))
            }
            0x37 => {
                let (size, fmt) = Format22t::new(insns);
                (size, Instruction::IfLe(fmt))
            }
            0x38 => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfEqz(fmt))
            }
            0x39 => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfNez(fmt))
            }
            0x3a => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfLtz(fmt))
            }
            0x3b => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfGez(fmt))
            }
            0x3c => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfGtz(fmt))
            }
            0x3d => {
                let (size, fmt) = Format21t::new(insns);
                (size, Instruction::IfLez(fmt))
            }
            0x3e => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused3E(fmt))
            }
            0x3f => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused3F(fmt))
            }
            0x40 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused40(fmt))
            }
            0x41 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused41(fmt))
            }
            0x42 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused42(fmt))
            }
            0x43 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused43(fmt))
            }
            0x44 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGet(fmt))
            }
            0x45 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetWide(fmt))
            }
            0x46 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetObject(fmt))
            }
            0x47 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetBoolean(fmt))
            }
            0x48 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetByte(fmt))
            }
            0x49 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetChar(fmt))
            }
            0x4a => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AGetShort(fmt))
            }
            0x4b => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APut(fmt))
            }
            0x4c => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutWide(fmt))
            }
            0x4d => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutObject(fmt))
            }
            0x4e => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutBoolean(fmt))
            }
            0x4f => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutByte(fmt))
            }
            0x50 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutChar(fmt))
            }
            0x51 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::APutShort(fmt))
            }
            0x52 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGet(fmt))
            }
            0x53 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetWide(fmt))
            }
            0x54 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetObject(fmt))
            }
            0x55 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetBoolean(fmt))
            }
            0x56 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetByte(fmt))
            }
            0x57 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetChar(fmt))
            }
            0x58 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IGetShort(fmt))
            }
            0x59 => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPut(fmt))
            }
            0x5a => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutWide(fmt))
            }
            0x5b => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutObject(fmt))
            }
            0x5c => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutBoolean(fmt))
            }
            0x5d => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutByte(fmt))
            }
            0x5e => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutChar(fmt))
            }
            0x5f => {
                let (size, fmt) = Format22c::<FieldIdx>::new(insns);
                (size, Instruction::IPutShort(fmt))
            }
            0x60 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGet(fmt))
            }
            0x61 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetWide(fmt))
            }
            0x62 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetObject(fmt))
            }
            0x63 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetBoolean(fmt))
            }
            0x64 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetByte(fmt))
            }
            0x65 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetChar(fmt))
            }
            0x66 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SGetShort(fmt))
            }
            0x67 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPut(fmt))
            }
            0x68 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutWide(fmt))
            }
            0x69 => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutObject(fmt))
            }
            0x6a => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutBoolean(fmt))
            }
            0x6b => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutByte(fmt))
            }
            0x6c => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutChar(fmt))
            }
            0x6d => {
                let (size, fmt) = Format21c::<FieldIdx>::new(insns);
                (size, Instruction::SPutShort(fmt))
            }
            0x6e => {
                let (size, fmt) = Format35c::<MethodIdx>::new(insns);
                (size, Instruction::InvokeVirtual(fmt))
            }
            0x6f => {
                let (size, fmt) = Format35c::<MethodIdx>::new(insns);
                (size, Instruction::InvokeSuper(fmt))
            }
            0x70 => {
                let (size, fmt) = Format35c::<MethodIdx>::new(insns);
                (size, Instruction::InvokeDirect(fmt))
            }
            0x71 => {
                let (size, fmt) = Format35c::<MethodIdx>::new(insns);
                (size, Instruction::InvokeStatic(fmt))
            }
            0x72 => {
                let (size, fmt) = Format35c::<MethodIdx>::new(insns);
                (size, Instruction::InvokeInterface(fmt))
            }
            0x73 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused73(fmt))
            }
            0x74 => {
                let (size, fmt) = Format3rc::<MethodIdx>::new(insns);
                (size, Instruction::InvokeVirtualRange(fmt))
            }
            0x75 => {
                let (size, fmt) = Format3rc::<MethodIdx>::new(insns);
                (size, Instruction::InvokeSuperRange(fmt))
            }
            0x76 => {
                let (size, fmt) = Format3rc::<MethodIdx>::new(insns);
                (size, Instruction::InvokeDirectRange(fmt))
            }
            0x77 => {
                let (size, fmt) = Format3rc::<MethodIdx>::new(insns);
                (size, Instruction::InvokeStaticRange(fmt))
            }
            0x78 => {
                let (size, fmt) = Format3rc::<MethodIdx>::new(insns);
                (size, Instruction::InvokeInterfaceRange(fmt))
            }
            0x79 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused79(fmt))
            }
            0x7a => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::Unused7A(fmt))
            }
            0x7b => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NegInt(fmt))
            }
            0x7c => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NotInt(fmt))
            }
            0x7d => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NegLong(fmt))
            }
            0x7e => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NotLong(fmt))
            }
            0x7f => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NegFloat(fmt))
            }
            0x80 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::NegDouble(fmt))
            }
            0x81 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToLong(fmt))
            }
            0x82 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToFloat(fmt))
            }
            0x83 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToDouble(fmt))
            }
            0x84 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::LongToInt(fmt))
            }
            0x85 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::LongToFloat(fmt))
            }
            0x86 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::LongToDouble(fmt))
            }
            0x87 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::FloatToInt(fmt))
            }
            0x88 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::FloatToLong(fmt))
            }
            0x89 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::FloatToDouble(fmt))
            }
            0x8a => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DoubleToInt(fmt))
            }
            0x8b => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DoubleToLong(fmt))
            }
            0x8c => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DoubleToFloat(fmt))
            }
            0x8d => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToByte(fmt))
            }
            0x8e => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToChar(fmt))
            }
            0x8f => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::IntToShort(fmt))
            }
            0x90 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AddInt(fmt))
            }
            0x91 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::SubInt(fmt))
            }
            0x92 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::MulInt(fmt))
            }
            0x93 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::DivInt(fmt))
            }
            0x94 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::RemInt(fmt))
            }
            0x95 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AndInt(fmt))
            }
            0x96 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::OrInt(fmt))
            }
            0x97 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::XorInt(fmt))
            }
            0x98 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::ShlInt(fmt))
            }
            0x99 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::ShrInt(fmt))
            }
            0x9a => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::UshrInt(fmt))
            }
            0x9b => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AddLong(fmt))
            }
            0x9c => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::SubLong(fmt))
            }
            0x9d => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::MulLong(fmt))
            }
            0x9e => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::DivLong(fmt))
            }
            0x9f => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::RemLong(fmt))
            }
            0xa0 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AndLong(fmt))
            }
            0xa1 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::OrLong(fmt))
            }
            0xa2 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::XorLong(fmt))
            }
            0xa3 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::ShlLong(fmt))
            }
            0xa4 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::ShrLong(fmt))
            }
            0xa5 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::UshrLong(fmt))
            }
            0xa6 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AddFloat(fmt))
            }
            0xa7 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::SubFloat(fmt))
            }
            0xa8 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::MulFloat(fmt))
            }
            0xa9 => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::DivFloat(fmt))
            }
            0xaa => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::RemFloat(fmt))
            }
            0xab => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::AddDouble(fmt))
            }
            0xac => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::SubDouble(fmt))
            }
            0xad => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::MulDouble(fmt))
            }
            0xae => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::DivDouble(fmt))
            }
            0xaf => {
                let (size, fmt) = Format23x::new(insns);
                (size, Instruction::RemDouble(fmt))
            }
            0xb0 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AddInt2Addr(fmt))
            }
            0xb1 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::SubInt2Addr(fmt))
            }
            0xb2 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MulInt2Addr(fmt))
            }
            0xb3 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DivInt2Addr(fmt))
            }
            0xb4 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::RemInt2Addr(fmt))
            }
            0xb5 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AndInt2Addr(fmt))
            }
            0xb6 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::OrInt2Addr(fmt))
            }
            0xb7 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::XorInt2Addr(fmt))
            }
            0xb8 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::ShlInt2Addr(fmt))
            }
            0xb9 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::ShrInt2Addr(fmt))
            }
            0xba => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::UshrInt2Addr(fmt))
            }
            0xbb => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AddLong2Addr(fmt))
            }
            0xbc => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::SubLong2Addr(fmt))
            }
            0xbd => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MulLong2Addr(fmt))
            }
            0xbe => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DivLong2Addr(fmt))
            }
            0xbf => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::RemLong2Addr(fmt))
            }
            0xc0 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AndLong2Addr(fmt))
            }
            0xc1 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::OrLong2Addr(fmt))
            }
            0xc2 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::XorLong2Addr(fmt))
            }
            0xc3 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::ShlLong2Addr(fmt))
            }
            0xc4 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::ShrLong2Addr(fmt))
            }
            0xc5 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::UshrLong2Addr(fmt))
            }
            0xc6 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AddFloat2Addr(fmt))
            }
            0xc7 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::SubFloat2Addr(fmt))
            }
            0xc8 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MulFloat2Addr(fmt))
            }
            0xc9 => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DivFloat2Addr(fmt))
            }
            0xca => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::RemFloat2Addr(fmt))
            }
            0xcb => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::AddDouble2Addr(fmt))
            }
            0xcc => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::SubDouble2Addr(fmt))
            }
            0xcd => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::MulDouble2Addr(fmt))
            }
            0xce => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::DivDouble2Addr(fmt))
            }
            0xcf => {
                let (size, fmt) = Format12x::new(insns);
                (size, Instruction::RemDouble2Addr(fmt))
            }
            0xd0 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::AddIntLit16(fmt))
            }
            0xd1 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::RsubInt(fmt))
            }
            0xd2 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::MulIntLit16(fmt))
            }
            0xd3 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::DivIntLit16(fmt))
            }
            0xd4 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::RemIntLit16(fmt))
            }
            0xd5 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::AndIntLit16(fmt))
            }
            0xd6 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::OrIntLit16(fmt))
            }
            0xd7 => {
                let (size, fmt) = Format22s::new(insns);
                (size, Instruction::XorIntLit16(fmt))
            }
            0xd8 => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::AddIntLit8(fmt))
            }
            0xd9 => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::RsubIntLit8(fmt))
            }
            0xda => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::MulIntLit8(fmt))
            }
            0xdb => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::DivIntLit8(fmt))
            }
            0xdc => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::RemIntLit8(fmt))
            }
            0xdd => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::AndIntLit8(fmt))
            }
            0xde => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::OrIntLit8(fmt))
            }
            0xdf => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::XorIntLit8(fmt))
            }
            0xe0 => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::ShlIntLit8(fmt))
            }
            0xe1 => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::ShrIntLit8(fmt))
            }
            0xe2 => {
                let (size, fmt) = Format22b::new(insns);
                (size, Instruction::UshrIntLit8(fmt))
            }
            0xe3 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE3(fmt))
            }
            0xe4 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE4(fmt))
            }
            0xe5 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE5(fmt))
            }
            0xe6 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE6(fmt))
            }
            0xe7 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE7(fmt))
            }
            0xe8 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE8(fmt))
            }
            0xe9 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedE9(fmt))
            }
            0xea => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedEA(fmt))
            }
            0xeb => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedEB(fmt))
            }
            0xec => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedEC(fmt))
            }
            0xed => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedED(fmt))
            }
            0xee => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedEE(fmt))
            }
            0xef => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedEF(fmt))
            }
            0xf0 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF0(fmt))
            }
            0xf1 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF1(fmt))
            }
            0xf2 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF2(fmt))
            }
            0xf3 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF3(fmt))
            }
            0xf4 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF4(fmt))
            }
            0xf5 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF5(fmt))
            }
            0xf6 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF6(fmt))
            }
            0xf7 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF7(fmt))
            }
            0xf8 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF8(fmt))
            }
            0xf9 => {
                let (size, fmt) = FormatUnused::new(insns);
                (size, Instruction::UnusedF9(fmt))
            }
            0xfa => {
                let (size, fmt) = Format45cc::new(insns);
                (size, Instruction::UnusedFA(fmt))
            }
            0xfb => {
                let (size, fmt) = Format4rcc::new(insns);
                (size, Instruction::UnusedFB(fmt))
            }
            0xfc => {
                let (size, fmt) = Format35c::<CallIdx>::new(insns);
                (size, Instruction::UnusedFC(fmt))
            }
            0xfd => {
                let (size, fmt) = Format3rc::<CallIdx>::new(insns);
                (size, Instruction::UnusedFD(fmt))
            }
            0xfe => {
                let (size, fmt) = Format21c::<MethodIdx>::new(insns);
                (size, Instruction::UnusedFE(fmt))
            }
            0xff => {
                let (size, fmt) = Format21c::<ProtoIdx>::new(insns);
                (size, Instruction::UnusedFF(fmt))
            }
        }
    }

    pub fn format(&self) -> Format<'_> {
        match self {
            Self::Nop(fmt) => Format::Format10x(fmt),
            Self::Move(fmt) => Format::Format12x(fmt),
            Self::MoveFrom16(fmt) => Format::Format22x(fmt),
            Self::Move16(fmt) => Format::Format32x(fmt),
            Self::MoveWide(fmt) => Format::Format12x(fmt),
            Self::MoveWideFrom16(fmt) => Format::Format22x(fmt),
            Self::MoveWide16(fmt) => Format::Format32x(fmt),
            Self::MoveObject(fmt) => Format::Format12x(fmt),
            Self::MoveObjectFrom16(fmt) => Format::Format22x(fmt),
            Self::MoveObject16(fmt) => Format::Format32x(fmt),
            Self::MoveResult(fmt) => Format::Format11x(fmt),
            Self::MoveResultWide(fmt) => Format::Format11x(fmt),
            Self::MoveResultObject(fmt) => Format::Format11x(fmt),
            Self::MoveException(fmt) => Format::Format11x(fmt),
            Self::ReturnVoid(fmt) => Format::Format10x(fmt),
            Self::Return(fmt) => Format::Format11x(fmt),
            Self::ReturnWide(fmt) => Format::Format11x(fmt),
            Self::ReturnObject(fmt) => Format::Format11x(fmt),
            Self::Const4(fmt) => Format::Format11n(fmt),
            Self::Const16(fmt) => Format::Format21s(fmt),
            Self::Const(fmt) => Format::Format31i(fmt),
            Self::ConstHigh16(fmt) => Format::Format21h(fmt),
            Self::ConstWide16(fmt) => Format::Format21s(fmt),
            Self::ConstWide32(fmt) => Format::Format31i(fmt),
            Self::ConstWide(fmt) => Format::Format51l(fmt),
            Self::ConstWideHigh16(fmt) => Format::Format21h(fmt),
            Self::ConstString(fmt) => Format::Format21cS(fmt),
            Self::ConstStringJumbo(fmt) => Format::Format31c(fmt),
            Self::ConstClass(fmt) => Format::Format21cT(fmt),
            Self::MonitorEnter(fmt) => Format::Format11x(fmt),
            Self::MonitorExit(fmt) => Format::Format11x(fmt),
            Self::CheckCast(fmt) => Format::Format21cT(fmt),
            Self::InstanceOf(fmt) => Format::Format22cT(fmt),
            Self::ArrayLength(fmt) => Format::Format12x(fmt),
            Self::NewInstance(fmt) => Format::Format21cT(fmt),
            Self::NewArray(fmt) => Format::Format22cT(fmt),
            Self::FilledNewArray(fmt) => Format::Format35cT(fmt),
            Self::FilledNewArrayRange(fmt) => Format::Format3rcT(fmt),
            Self::FillArrayData(fmt) => Format::Format31t(fmt),
            Self::Throw(fmt) => Format::Format11x(fmt),
            Self::Goto(fmt) => Format::Format10t(fmt),
            Self::Goto16(fmt) => Format::Format20t(fmt),
            Self::Goto32(fmt) => Format::Format30t(fmt),
            Self::PackedSwitch(fmt) => Format::Format31t(fmt),
            Self::SparseSwitch(fmt) => Format::Format31t(fmt),
            Self::CmplFloat(fmt) => Format::Format23x(fmt),
            Self::CmpgFloat(fmt) => Format::Format23x(fmt),
            Self::CmplDouble(fmt) => Format::Format23x(fmt),
            Self::CmpgDouble(fmt) => Format::Format23x(fmt),
            Self::CmpLong(fmt) => Format::Format23x(fmt),
            Self::IfEq(fmt) => Format::Format22t(fmt),
            Self::IfNe(fmt) => Format::Format22t(fmt),
            Self::IfLt(fmt) => Format::Format22t(fmt),
            Self::IfGe(fmt) => Format::Format22t(fmt),
            Self::IfGt(fmt) => Format::Format22t(fmt),
            Self::IfLe(fmt) => Format::Format22t(fmt),
            Self::IfEqz(fmt) => Format::Format21t(fmt),
            Self::IfNez(fmt) => Format::Format21t(fmt),
            Self::IfLtz(fmt) => Format::Format21t(fmt),
            Self::IfGez(fmt) => Format::Format21t(fmt),
            Self::IfGtz(fmt) => Format::Format21t(fmt),
            Self::IfLez(fmt) => Format::Format21t(fmt),
            Self::Unused3E(fmt) => Format::FormatUnused(fmt),
            Self::Unused3F(fmt) => Format::FormatUnused(fmt),
            Self::Unused40(fmt) => Format::FormatUnused(fmt),
            Self::Unused41(fmt) => Format::FormatUnused(fmt),
            Self::Unused42(fmt) => Format::FormatUnused(fmt),
            Self::Unused43(fmt) => Format::FormatUnused(fmt),
            Self::AGet(fmt) => Format::Format23x(fmt),
            Self::AGetWide(fmt) => Format::Format23x(fmt),
            Self::AGetObject(fmt) => Format::Format23x(fmt),
            Self::AGetBoolean(fmt) => Format::Format23x(fmt),
            Self::AGetByte(fmt) => Format::Format23x(fmt),
            Self::AGetChar(fmt) => Format::Format23x(fmt),
            Self::AGetShort(fmt) => Format::Format23x(fmt),
            Self::APut(fmt) => Format::Format23x(fmt),
            Self::APutWide(fmt) => Format::Format23x(fmt),
            Self::APutObject(fmt) => Format::Format23x(fmt),
            Self::APutBoolean(fmt) => Format::Format23x(fmt),
            Self::APutByte(fmt) => Format::Format23x(fmt),
            Self::APutChar(fmt) => Format::Format23x(fmt),
            Self::APutShort(fmt) => Format::Format23x(fmt),
            Self::IGet(fmt) => Format::Format22cF(fmt),
            Self::IGetWide(fmt) => Format::Format22cF(fmt),
            Self::IGetObject(fmt) => Format::Format22cF(fmt),
            Self::IGetBoolean(fmt) => Format::Format22cF(fmt),
            Self::IGetByte(fmt) => Format::Format22cF(fmt),
            Self::IGetChar(fmt) => Format::Format22cF(fmt),
            Self::IGetShort(fmt) => Format::Format22cF(fmt),
            Self::IPut(fmt) => Format::Format22cF(fmt),
            Self::IPutWide(fmt) => Format::Format22cF(fmt),
            Self::IPutObject(fmt) => Format::Format22cF(fmt),
            Self::IPutBoolean(fmt) => Format::Format22cF(fmt),
            Self::IPutByte(fmt) => Format::Format22cF(fmt),
            Self::IPutChar(fmt) => Format::Format22cF(fmt),
            Self::IPutShort(fmt) => Format::Format22cF(fmt),
            Self::SGet(fmt) => Format::Format21cF(fmt),
            Self::SGetWide(fmt) => Format::Format21cF(fmt),
            Self::SGetObject(fmt) => Format::Format21cF(fmt),
            Self::SGetBoolean(fmt) => Format::Format21cF(fmt),
            Self::SGetByte(fmt) => Format::Format21cF(fmt),
            Self::SGetChar(fmt) => Format::Format21cF(fmt),
            Self::SGetShort(fmt) => Format::Format21cF(fmt),
            Self::SPut(fmt) => Format::Format21cF(fmt),
            Self::SPutWide(fmt) => Format::Format21cF(fmt),
            Self::SPutObject(fmt) => Format::Format21cF(fmt),
            Self::SPutBoolean(fmt) => Format::Format21cF(fmt),
            Self::SPutByte(fmt) => Format::Format21cF(fmt),
            Self::SPutChar(fmt) => Format::Format21cF(fmt),
            Self::SPutShort(fmt) => Format::Format21cF(fmt),
            Self::InvokeVirtual(fmt) => Format::Format35cM(fmt),
            Self::InvokeSuper(fmt) => Format::Format35cM(fmt),
            Self::InvokeDirect(fmt) => Format::Format35cM(fmt),
            Self::InvokeStatic(fmt) => Format::Format35cM(fmt),
            Self::InvokeInterface(fmt) => Format::Format35cM(fmt),
            Self::Unused73(fmt) => Format::FormatUnused(fmt),
            Self::InvokeVirtualRange(fmt) => Format::Format3rcM(fmt),
            Self::InvokeSuperRange(fmt) => Format::Format3rcM(fmt),
            Self::InvokeDirectRange(fmt) => Format::Format3rcM(fmt),
            Self::InvokeStaticRange(fmt) => Format::Format3rcM(fmt),
            Self::InvokeInterfaceRange(fmt) => Format::Format3rcM(fmt),
            Self::Unused79(fmt) => Format::FormatUnused(fmt),
            Self::Unused7A(fmt) => Format::FormatUnused(fmt),
            Self::NegInt(fmt) => Format::Format12x(fmt),
            Self::NotInt(fmt) => Format::Format12x(fmt),
            Self::NegLong(fmt) => Format::Format12x(fmt),
            Self::NotLong(fmt) => Format::Format12x(fmt),
            Self::NegFloat(fmt) => Format::Format12x(fmt),
            Self::NegDouble(fmt) => Format::Format12x(fmt),
            Self::IntToLong(fmt) => Format::Format12x(fmt),
            Self::IntToFloat(fmt) => Format::Format12x(fmt),
            Self::IntToDouble(fmt) => Format::Format12x(fmt),
            Self::LongToInt(fmt) => Format::Format12x(fmt),
            Self::LongToFloat(fmt) => Format::Format12x(fmt),
            Self::LongToDouble(fmt) => Format::Format12x(fmt),
            Self::FloatToInt(fmt) => Format::Format12x(fmt),
            Self::FloatToLong(fmt) => Format::Format12x(fmt),
            Self::FloatToDouble(fmt) => Format::Format12x(fmt),
            Self::DoubleToInt(fmt) => Format::Format12x(fmt),
            Self::DoubleToLong(fmt) => Format::Format12x(fmt),
            Self::DoubleToFloat(fmt) => Format::Format12x(fmt),
            Self::IntToByte(fmt) => Format::Format12x(fmt),
            Self::IntToChar(fmt) => Format::Format12x(fmt),
            Self::IntToShort(fmt) => Format::Format12x(fmt),
            Self::AddInt(fmt) => Format::Format23x(fmt),
            Self::SubInt(fmt) => Format::Format23x(fmt),
            Self::MulInt(fmt) => Format::Format23x(fmt),
            Self::DivInt(fmt) => Format::Format23x(fmt),
            Self::RemInt(fmt) => Format::Format23x(fmt),
            Self::AndInt(fmt) => Format::Format23x(fmt),
            Self::OrInt(fmt) => Format::Format23x(fmt),
            Self::XorInt(fmt) => Format::Format23x(fmt),
            Self::ShlInt(fmt) => Format::Format23x(fmt),
            Self::ShrInt(fmt) => Format::Format23x(fmt),
            Self::UshrInt(fmt) => Format::Format23x(fmt),
            Self::AddLong(fmt) => Format::Format23x(fmt),
            Self::SubLong(fmt) => Format::Format23x(fmt),
            Self::MulLong(fmt) => Format::Format23x(fmt),
            Self::DivLong(fmt) => Format::Format23x(fmt),
            Self::RemLong(fmt) => Format::Format23x(fmt),
            Self::AndLong(fmt) => Format::Format23x(fmt),
            Self::OrLong(fmt) => Format::Format23x(fmt),
            Self::XorLong(fmt) => Format::Format23x(fmt),
            Self::ShlLong(fmt) => Format::Format23x(fmt),
            Self::ShrLong(fmt) => Format::Format23x(fmt),
            Self::UshrLong(fmt) => Format::Format23x(fmt),
            Self::AddFloat(fmt) => Format::Format23x(fmt),
            Self::SubFloat(fmt) => Format::Format23x(fmt),
            Self::MulFloat(fmt) => Format::Format23x(fmt),
            Self::DivFloat(fmt) => Format::Format23x(fmt),
            Self::RemFloat(fmt) => Format::Format23x(fmt),
            Self::AddDouble(fmt) => Format::Format23x(fmt),
            Self::SubDouble(fmt) => Format::Format23x(fmt),
            Self::MulDouble(fmt) => Format::Format23x(fmt),
            Self::DivDouble(fmt) => Format::Format23x(fmt),
            Self::RemDouble(fmt) => Format::Format23x(fmt),
            Self::AddInt2Addr(fmt) => Format::Format12x(fmt),
            Self::SubInt2Addr(fmt) => Format::Format12x(fmt),
            Self::MulInt2Addr(fmt) => Format::Format12x(fmt),
            Self::DivInt2Addr(fmt) => Format::Format12x(fmt),
            Self::RemInt2Addr(fmt) => Format::Format12x(fmt),
            Self::AndInt2Addr(fmt) => Format::Format12x(fmt),
            Self::OrInt2Addr(fmt) => Format::Format12x(fmt),
            Self::XorInt2Addr(fmt) => Format::Format12x(fmt),
            Self::ShlInt2Addr(fmt) => Format::Format12x(fmt),
            Self::ShrInt2Addr(fmt) => Format::Format12x(fmt),
            Self::UshrInt2Addr(fmt) => Format::Format12x(fmt),
            Self::AddLong2Addr(fmt) => Format::Format12x(fmt),
            Self::SubLong2Addr(fmt) => Format::Format12x(fmt),
            Self::MulLong2Addr(fmt) => Format::Format12x(fmt),
            Self::DivLong2Addr(fmt) => Format::Format12x(fmt),
            Self::RemLong2Addr(fmt) => Format::Format12x(fmt),
            Self::AndLong2Addr(fmt) => Format::Format12x(fmt),
            Self::OrLong2Addr(fmt) => Format::Format12x(fmt),
            Self::XorLong2Addr(fmt) => Format::Format12x(fmt),
            Self::ShlLong2Addr(fmt) => Format::Format12x(fmt),
            Self::ShrLong2Addr(fmt) => Format::Format12x(fmt),
            Self::UshrLong2Addr(fmt) => Format::Format12x(fmt),
            Self::AddFloat2Addr(fmt) => Format::Format12x(fmt),
            Self::SubFloat2Addr(fmt) => Format::Format12x(fmt),
            Self::MulFloat2Addr(fmt) => Format::Format12x(fmt),
            Self::DivFloat2Addr(fmt) => Format::Format12x(fmt),
            Self::RemFloat2Addr(fmt) => Format::Format12x(fmt),
            Self::AddDouble2Addr(fmt) => Format::Format12x(fmt),
            Self::SubDouble2Addr(fmt) => Format::Format12x(fmt),
            Self::MulDouble2Addr(fmt) => Format::Format12x(fmt),
            Self::DivDouble2Addr(fmt) => Format::Format12x(fmt),
            Self::RemDouble2Addr(fmt) => Format::Format12x(fmt),
            Self::AddIntLit16(fmt) => Format::Format22s(fmt),
            Self::RsubInt(fmt) => Format::Format22s(fmt),
            Self::MulIntLit16(fmt) => Format::Format22s(fmt),
            Self::DivIntLit16(fmt) => Format::Format22s(fmt),
            Self::RemIntLit16(fmt) => Format::Format22s(fmt),
            Self::AndIntLit16(fmt) => Format::Format22s(fmt),
            Self::OrIntLit16(fmt) => Format::Format22s(fmt),
            Self::XorIntLit16(fmt) => Format::Format22s(fmt),
            Self::AddIntLit8(fmt) => Format::Format22b(fmt),
            Self::RsubIntLit8(fmt) => Format::Format22b(fmt),
            Self::MulIntLit8(fmt) => Format::Format22b(fmt),
            Self::DivIntLit8(fmt) => Format::Format22b(fmt),
            Self::RemIntLit8(fmt) => Format::Format22b(fmt),
            Self::AndIntLit8(fmt) => Format::Format22b(fmt),
            Self::OrIntLit8(fmt) => Format::Format22b(fmt),
            Self::XorIntLit8(fmt) => Format::Format22b(fmt),
            Self::ShlIntLit8(fmt) => Format::Format22b(fmt),
            Self::ShrIntLit8(fmt) => Format::Format22b(fmt),
            Self::UshrIntLit8(fmt) => Format::Format22b(fmt),
            Self::UnusedE3(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE4(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE5(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE6(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE7(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE8(fmt) => Format::FormatUnused(fmt),
            Self::UnusedE9(fmt) => Format::FormatUnused(fmt),
            Self::UnusedEA(fmt) => Format::FormatUnused(fmt),
            Self::UnusedEB(fmt) => Format::FormatUnused(fmt),
            Self::UnusedEC(fmt) => Format::FormatUnused(fmt),
            Self::UnusedED(fmt) => Format::FormatUnused(fmt),
            Self::UnusedEE(fmt) => Format::FormatUnused(fmt),
            Self::UnusedEF(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF0(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF1(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF2(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF3(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF4(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF5(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF6(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF7(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF8(fmt) => Format::FormatUnused(fmt),
            Self::UnusedF9(fmt) => Format::FormatUnused(fmt),
            Self::UnusedFA(fmt) => Format::Format45cc(fmt),
            Self::UnusedFB(fmt) => Format::Format4rcc(fmt),
            Self::UnusedFC(fmt) => Format::Format35cC(fmt),
            Self::UnusedFD(fmt) => Format::Format3rcC(fmt),
            Self::UnusedFE(fmt) => Format::Format21cM(fmt),
            Self::UnusedFF(fmt) => Format::Format21cP(fmt),
        }
    }

    pub fn discriminant(&self) -> u8 {
        // SAFETY: Because `Self` is marked `repr(u8)`, its layout is a `repr(C)` `union`
        // between `repr(C)` structs, each of which has the `u8` discriminant as its first
        // field, so we can read the discriminant without offsetting the pointer.
        unsafe { *<*const _>::from(self).cast::<u8>() }
    }

    /// Returns data flow information about the registers accessed by this instruction
    pub fn data_flow(&self) -> DataFlow {
        use Instruction::*;
        // The order of dataflow_source_sink_ret is source-dest-ret
        match self {
            // Move: dest=a, source=b
            Move(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            MoveFrom16(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            Move16(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            MoveWide(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a, f.a.next_reg()], [])
            }
            MoveWideFrom16(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a, f.a.next_reg()], [])
            }
            MoveWide16(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a, f.a.next_reg()], [])
            }
            MoveObject(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            MoveObjectFrom16(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            MoveObject16(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            // Move result / exception: dest=a only
            MoveResult(f) => dataflow_source_sink_ret!([], [f.a], []),
            MoveResultWide(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            MoveResultObject(f) => dataflow_source_sink_ret!([], [f.a], []),
            MoveException(f) => dataflow_source_sink_ret!([], [f.a], []),
            // No registers
            ReturnVoid(_) | Nop(_) | Goto(_) | Goto16(_) | Goto32(_) => {
                dataflow_source_sink_ret!([], [], [])
            }
            // 11x read only
            Return(f) => dataflow_source_sink_ret!([f.a], [], []),
            ReturnWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [], []),
            ReturnObject(f) => dataflow_source_sink_ret!([f.a], [], []),
            MonitorEnter(f) | MonitorExit(f) | Throw(f) => dataflow_source_sink_ret!([f.a], [], []),
            // Const: dest only
            Const4(f) => dataflow_source_sink_ret!([], [f.a], []),
            Const16(f) => dataflow_source_sink_ret!([], [f.a], []),
            Const(f) => dataflow_source_sink_ret!([], [f.a], []),
            ConstHigh16(f) => dataflow_source_sink_ret!([], [f.a], []),
            ConstWide16(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            ConstWide32(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            ConstWide(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            ConstWideHigh16(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            // 21c/31c: dest=a
            ConstString(f) => dataflow_source_sink_ret!([], [f.a], []),
            ConstStringJumbo(f) => dataflow_source_sink_ret!([], [f.a], []),
            ConstClass(f) | NewInstance(f) => dataflow_source_sink_ret!([], [f.a], []),
            CheckCast(f) => dataflow_source_sink_ret!([f.a], [f.a], []),
            // SGet*: dest=a (wide = two regs)
            SGet(f) | SGetBoolean(f) | SGetByte(f) | SGetChar(f) | SGetShort(f) | SGetObject(f) => {
                dataflow_source_sink_ret!([], [f.a], [])
            }
            SGetWide(f) => dataflow_source_sink_ret!([], [f.a, f.a.next_reg()], []),
            // SPut*: source=a
            SPut(f) | SPutBoolean(f) | SPutByte(f) | SPutChar(f) | SPutShort(f) | SPutObject(f) => {
                dataflow_source_sink_ret!([f.a], [], [])
            }
            SPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [], []),
            // 22c
            InstanceOf(f) | NewArray(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            IGet(f) | IGetBoolean(f) | IGetByte(f) | IGetChar(f) | IGetShort(f) | IGetObject(f) => {
                dataflow_source_sink_ret!([f.b], [f.a], [])
            }
            IGetWide(f) => dataflow_source_sink_ret!([f.b], [f.a, f.a.next_reg()], []),
            IPut(f) | IPutBoolean(f) | IPutByte(f) | IPutChar(f) | IPutShort(f) | IPutObject(f) => {
                dataflow_source_sink_ret!([f.a, f.b], [], [])
            }
            IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], []),
            // 35c/3rc: source = args
            FilledNewArray(f) => dataflow_from_args(&f.args),
            FilledNewArrayRange(f) => dataflow_from_args(&f.args),
            InvokeVirtual(f) | InvokeSuper(f) | InvokeDirect(f) | InvokeStatic(f)
            | InvokeInterface(f) => dataflow_from_args(&f.args),
            InvokeVirtualRange(f)
            | InvokeSuperRange(f)
            | InvokeDirectRange(f)
            | InvokeStaticRange(f)
            | InvokeInterfaceRange(f) => dataflow_from_args(&f.args),
            UnusedFC(f) => dataflow_from_args(&f.args),
            UnusedFD(f) => dataflow_from_args(&f.args),
            // 31t
            FillArrayData(f) | PackedSwitch(f) | SparseSwitch(f) => {
                dataflow_source_sink_ret!([f.a], [], [])
            }
            // 21t/22t branches
            IfEq(f) | IfNe(f) | IfLt(f) | IfGe(f) | IfGt(f) | IfLe(f) => {
                dataflow_source_sink_ret!([f.a, f.b], [], [])
            }
            IfEqz(f) | IfNez(f) | IfLtz(f) | IfGez(f) | IfGtz(f) | IfLez(f) => {
                dataflow_source_sink_ret!([f.a], [], [])
            }
            // 12x unary
            ArrayLength(f) => dataflow_source_sink_ret!([f.b], [f.a], []),
            NegInt(f) | NotInt(f) | IntToByte(f) | IntToChar(f) | IntToShort(f) => {
                dataflow_source_sink_ret!([f.b], [f.a], [])
            }
            NegFloat(f) | IntToFloat(f) | FloatToInt(f) => {
                dataflow_source_sink_ret!([f.b], [f.a], [])
            }
            IntToLong(f) | IntToDouble(f) | FloatToLong(f) | FloatToDouble(f) => {
                dataflow_source_sink_ret!([f.b], [f.a, f.a.next_reg()], [])
            }
            LongToInt(f) | LongToFloat(f) | DoubleToInt(f) | DoubleToFloat(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a], [])
            }
            NegLong(f) | NotLong(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a, f.a.next_reg()], [])
            }
            NegDouble(f) | LongToDouble(f) | DoubleToLong(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg()], [f.a, f.a.next_reg()], [])
            }
            // 12x binary 2addr: source=a,b dest=a (or wide)
            AddInt2Addr(f) | SubInt2Addr(f) | MulInt2Addr(f) | DivInt2Addr(f) | RemInt2Addr(f)
            | AndInt2Addr(f) | OrInt2Addr(f) | XorInt2Addr(f) | ShlInt2Addr(f) | ShrInt2Addr(f)
            | UshrInt2Addr(f) => {
                dataflow_source_sink_ret!([f.a, f.b], [f.a], [])
            }
            AddLong2Addr(f) | SubLong2Addr(f) | MulLong2Addr(f) | DivLong2Addr(f)
            | RemLong2Addr(f) | AndLong2Addr(f) | OrLong2Addr(f) | XorLong2Addr(f)
            | ShlLong2Addr(f) | ShrLong2Addr(f) | UshrLong2Addr(f) => {
                dataflow_source_sink_ret!(
                    [f.a, f.a.next_reg(), f.b, f.b.next_reg()],
                    [f.a, f.a.next_reg()],
                    []
                )
            }
            AddFloat2Addr(f) | SubFloat2Addr(f) | MulFloat2Addr(f) | DivFloat2Addr(f)
            | RemFloat2Addr(f) => {
                dataflow_source_sink_ret!([f.a, f.b], [f.a], [])
            }
            AddDouble2Addr(f) | SubDouble2Addr(f) | MulDouble2Addr(f) | DivDouble2Addr(f)
            | RemDouble2Addr(f) => {
                dataflow_source_sink_ret!(
                    [f.a, f.a.next_reg(), f.b, f.b.next_reg()],
                    [f.a, f.a.next_reg()],
                    []
                )
            }
            // 22s / 22b
            AddIntLit16(f) | RsubInt(f) | MulIntLit16(f) | DivIntLit16(f) | RemIntLit16(f)
            | AndIntLit16(f) | OrIntLit16(f) | XorIntLit16(f) => {
                dataflow_source_sink_ret!([f.b], [f.a], [])
            }
            AddIntLit8(f) | RsubIntLit8(f) | MulIntLit8(f) | DivIntLit8(f) | RemIntLit8(f)
            | AndIntLit8(f) | OrIntLit8(f) | XorIntLit8(f) | ShlIntLit8(f) | ShrIntLit8(f)
            | UshrIntLit8(f) => {
                dataflow_source_sink_ret!([f.b], [f.a], [])
            }
            // 23x aget*: dest=a, source=b,c (array, index)
            AGet(f) | AGetBoolean(f) | AGetByte(f) | AGetChar(f) | AGetShort(f) | AGetObject(f) => {
                dataflow_source_sink_ret!([f.b, f.c], [f.a], [])
            }
            AGetWide(f) => dataflow_source_sink_ret!([f.b, f.c], [f.a, f.a.next_reg()], []),
            // 23x aput*
            APut(f) | APutBoolean(f) | APutByte(f) | APutChar(f) | APutShort(f) | APutObject(f) => {
                dataflow_source_sink_ret!([f.a, f.b, f.c], [], [])
            }
            APutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg(), f.b, f.c], [], []),
            // 23x binop / cmp
            CmplFloat(f) | CmpgFloat(f) => dataflow_source_sink_ret!([f.b, f.c], [f.a], []),
            CmpLong(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg(), f.c, f.c.next_reg()], [f.a], [])
            }
            CmplDouble(f) | CmpgDouble(f) => {
                dataflow_source_sink_ret!([f.b, f.b.next_reg(), f.c, f.c.next_reg()], [f.a], [])
            }
            AddInt(f) | SubInt(f) | MulInt(f) | DivInt(f) | RemInt(f) | AndInt(f) | OrInt(f)
            | XorInt(f) | ShlInt(f) | ShrInt(f) | UshrInt(f) => {
                dataflow_source_sink_ret!([f.b, f.c], [f.a], [])
            }
            AddLong(f) | SubLong(f) | MulLong(f) | DivLong(f) | RemLong(f) | AndLong(f)
            | OrLong(f) | XorLong(f) | ShlLong(f) | ShrLong(f) | UshrLong(f) => {
                dataflow_source_sink_ret!(
                    [f.b, f.b.next_reg(), f.c, f.c.next_reg()],
                    [f.a, f.a.next_reg()],
                    []
                )
            }
            AddFloat(f) | SubFloat(f) | MulFloat(f) | DivFloat(f) | RemFloat(f) => {
                dataflow_source_sink_ret!([f.b, f.c], [f.a], [])
            }
            AddDouble(f) | SubDouble(f) | MulDouble(f) | DivDouble(f) | RemDouble(f) => {
                dataflow_source_sink_ret!(
                    [f.b, f.b.next_reg(), f.c, f.c.next_reg()],
                    [f.a, f.a.next_reg()],
                    []
                )
            }
            // Unused
            Unused3E(_) | Unused3F(_) | Unused40(_) | Unused41(_) | Unused42(_) | Unused43(_)
            | Unused73(_) | Unused79(_) | Unused7A(_) | UnusedE3(_) | UnusedE4(_) | UnusedE5(_)
            | UnusedE6(_) | UnusedE7(_) | UnusedE8(_) | UnusedE9(_) | UnusedEA(_) | UnusedEB(_)
            | UnusedEC(_) | UnusedED(_) | UnusedEE(_) | UnusedEF(_) | UnusedF0(_) | UnusedF1(_)
            | UnusedF2(_) | UnusedF3(_) | UnusedF4(_) | UnusedF5(_) | UnusedF6(_) | UnusedF7(_)
            | UnusedF8(_) | UnusedF9(_) | UnusedFA(_) | UnusedFB(_) | UnusedFE(_) | UnusedFF(_) => {
                dataflow_source_sink_ret!([], [], [])
            }
        }
    }

    /// Returns the argument registers of a call/invoke instruction in order, or `None` if this
    /// instruction is not a call (invoke or UnusedFC/UnusedFD). Array instructions are excluded.
    pub fn call_args(&self) -> Option<&[Reg]> {
        use Instruction::*;
        match self {
            InvokeVirtual(f) | InvokeSuper(f) | InvokeDirect(f) | InvokeStatic(f)
            | InvokeInterface(f) => Some(&f.args),
            InvokeVirtualRange(f)
            | InvokeSuperRange(f)
            | InvokeDirectRange(f)
            | InvokeStaticRange(f)
            | InvokeInterfaceRange(f) => Some(&f.args),
            UnusedFC(f) => Some(&f.args),
            UnusedFD(f) => Some(&f.args),
            _ => None,
        }
    }

    /// Returns `true` if this instruction is an invoke (invoke-virtual, invoke-static, etc.).
    pub fn is_invoke(&self) -> bool {
        use Instruction::*;
        matches!(
            self,
            InvokeVirtual(_)
                | InvokeSuper(_)
                | InvokeDirect(_)
                | InvokeStatic(_)
                | InvokeInterface(_)
                | InvokeVirtualRange(_)
                | InvokeSuperRange(_)
                | InvokeDirectRange(_)
                | InvokeStaticRange(_)
                | InvokeInterfaceRange(_)
        )
    }

    /// Returns `true` if this instruction is a move-result (move-result, move-result-wide, move-result-object).
    pub fn is_move_result(&self) -> bool {
        use Instruction::*;
        matches!(
            self,
            MoveResult(_) | MoveResultWide(_) | MoveResultObject(_)
        )
    }

    /// Returns `true` if this instruction transfers control (branch, switch, throw, return).
    pub fn is_control_flow(&self) -> bool {
        use Instruction::*;
        matches!(
            self,
            Goto(_)
                | Goto16(_)
                | Goto32(_)
                | PackedSwitch(_)
                | SparseSwitch(_)
                | IfEq(_)
                | IfNe(_)
                | IfLt(_)
                | IfGe(_)
                | IfGt(_)
                | IfLe(_)
                | IfEqz(_)
                | IfNez(_)
                | IfLtz(_)
                | IfGez(_)
                | IfGtz(_)
                | IfLez(_)
                | Throw(_)
                | Return(_)
                | ReturnVoid(_)
                | ReturnWide(_)
                | ReturnObject(_)
        )
    }

    /// Returns `true` if executing this instruction may raise an exception.
    ///
    /// This is intentionally conservative for CFG construction around DEX
    /// `try_item` regions: instructions not listed as known-safe default to
    /// `true` so exceptional successors are not missed.
    pub fn can_throw(&self) -> bool {
        use Instruction::*;
        !matches!(
            self,
            Nop(_)
                | Move(_)
                | MoveFrom16(_)
                | Move16(_)
                | MoveWide(_)
                | MoveWideFrom16(_)
                | MoveWide16(_)
                | MoveObject(_)
                | MoveObjectFrom16(_)
                | MoveObject16(_)
                | MoveResult(_)
                | MoveResultWide(_)
                | MoveResultObject(_)
                | MoveException(_)
                | ReturnVoid(_)
                | Return(_)
                | ReturnWide(_)
                | ReturnObject(_)
                | Const4(_)
                | Const16(_)
                | Const(_)
                | ConstHigh16(_)
                | ConstWide16(_)
                | ConstWide32(_)
                | ConstWide(_)
                | ConstWideHigh16(_)
                | Goto(_)
                | Goto16(_)
                | Goto32(_)
                | PackedSwitch(_)
                | SparseSwitch(_)
                | CmplFloat(_)
                | CmpgFloat(_)
                | CmplDouble(_)
                | CmpgDouble(_)
                | CmpLong(_)
                | IfEq(_)
                | IfNe(_)
                | IfLt(_)
                | IfGe(_)
                | IfGt(_)
                | IfLe(_)
                | IfEqz(_)
                | IfNez(_)
                | IfLtz(_)
                | IfGez(_)
                | IfGtz(_)
                | IfLez(_)
                | NegInt(_)
                | NotInt(_)
                | NegLong(_)
                | NotLong(_)
                | NegFloat(_)
                | NegDouble(_)
                | IntToLong(_)
                | IntToFloat(_)
                | IntToDouble(_)
                | LongToInt(_)
                | LongToFloat(_)
                | LongToDouble(_)
                | FloatToInt(_)
                | FloatToLong(_)
                | FloatToDouble(_)
                | DoubleToInt(_)
                | DoubleToLong(_)
                | DoubleToFloat(_)
                | IntToByte(_)
                | IntToChar(_)
                | IntToShort(_)
                | AddInt(_)
                | SubInt(_)
                | MulInt(_)
                | AndInt(_)
                | OrInt(_)
                | XorInt(_)
                | ShlInt(_)
                | ShrInt(_)
                | UshrInt(_)
                | AddLong(_)
                | SubLong(_)
                | MulLong(_)
                | AndLong(_)
                | OrLong(_)
                | XorLong(_)
                | ShlLong(_)
                | ShrLong(_)
                | UshrLong(_)
                | AddFloat(_)
                | SubFloat(_)
                | MulFloat(_)
                | DivFloat(_)
                | RemFloat(_)
                | AddDouble(_)
                | SubDouble(_)
                | MulDouble(_)
                | DivDouble(_)
                | RemDouble(_)
                | AddInt2Addr(_)
                | SubInt2Addr(_)
                | MulInt2Addr(_)
                | AndInt2Addr(_)
                | OrInt2Addr(_)
                | XorInt2Addr(_)
                | ShlInt2Addr(_)
                | ShrInt2Addr(_)
                | UshrInt2Addr(_)
                | AddLong2Addr(_)
                | SubLong2Addr(_)
                | MulLong2Addr(_)
                | AndLong2Addr(_)
                | OrLong2Addr(_)
                | XorLong2Addr(_)
                | ShlLong2Addr(_)
                | ShrLong2Addr(_)
                | UshrLong2Addr(_)
                | AddFloat2Addr(_)
                | SubFloat2Addr(_)
                | MulFloat2Addr(_)
                | DivFloat2Addr(_)
                | RemFloat2Addr(_)
                | AddDouble2Addr(_)
                | SubDouble2Addr(_)
                | MulDouble2Addr(_)
                | DivDouble2Addr(_)
                | RemDouble2Addr(_)
                | AddIntLit16(_)
                | RsubInt(_)
                | MulIntLit16(_)
                | AndIntLit16(_)
                | OrIntLit16(_)
                | XorIntLit16(_)
                | AddIntLit8(_)
                | RsubIntLit8(_)
                | MulIntLit8(_)
                | AndIntLit8(_)
                | OrIntLit8(_)
                | XorIntLit8(_)
                | ShlIntLit8(_)
                | ShrIntLit8(_)
                | UshrIntLit8(_)
                | Unused3E(_)
                | Unused3F(_)
                | Unused40(_)
                | Unused41(_)
                | Unused42(_)
                | Unused43(_)
                | Unused73(_)
                | Unused79(_)
                | Unused7A(_)
                | UnusedE3(_)
                | UnusedE4(_)
                | UnusedE5(_)
                | UnusedE6(_)
                | UnusedE7(_)
                | UnusedE8(_)
                | UnusedE9(_)
                | UnusedEA(_)
                | UnusedEB(_)
                | UnusedEC(_)
                | UnusedED(_)
                | UnusedEE(_)
                | UnusedEF(_)
                | UnusedF0(_)
                | UnusedF1(_)
                | UnusedF2(_)
                | UnusedF3(_)
                | UnusedF4(_)
                | UnusedF5(_)
                | UnusedF6(_)
                | UnusedF7(_)
                | UnusedF8(_)
                | UnusedF9(_)
                | UnusedFA(_)
                | UnusedFB(_)
                | UnusedFC(_)
                | UnusedFD(_)
                | UnusedFE(_)
                | UnusedFF(_)
        )
    }
}

impl DisplayInstr for PayloadInstruction {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            PayloadInstruction::PackedSwitch(payload) => {
                write!(f, ".packed-switch {}", payload.first_key)?;
                for i in 0..payload.targets.len() {
                    write!(f, "\n    :pswitch_{}", i)?;
                }
                write!(f, "\n.end packed-switch")
            }
            PayloadInstruction::SparseSwitch(payload) => {
                write!(f, ".sparse-switch")?;
                for (i, key) in payload.keys.iter().enumerate() {
                    write!(f, "\n    {} -> :sswitch_{}", key, i)?;
                }
                write!(f, "\n.end sparse-switch")
            }
            PayloadInstruction::FillArrayData(payload) => {
                write!(f, ".array-data {}", payload.element_width)?;
                let w = payload.element_width as usize;
                let mut off = 0usize;
                while off + w <= payload.data.len() {
                    let val: i64 = match w {
                        1 => payload.data[off] as i64,
                        2 => {
                            let v = u16::from_le_bytes([payload.data[off], payload.data[off + 1]]);
                            v as i64
                        }
                        4 => {
                            let v = u32::from_le_bytes([
                                payload.data[off],
                                payload.data[off + 1],
                                payload.data[off + 2],
                                payload.data[off + 3],
                            ]);
                            v as i64
                        }
                        8 => {
                            let v = u64::from_le_bytes([
                                payload.data[off],
                                payload.data[off + 1],
                                payload.data[off + 2],
                                payload.data[off + 3],
                                payload.data[off + 4],
                                payload.data[off + 5],
                                payload.data[off + 6],
                                payload.data[off + 7],
                            ]);
                            v as i64
                        }
                        _ => break,
                    };
                    if w == 8 {
                        let v = val as i64;
                        if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
                            write!(f, "\n    {}", v)?;
                        } else {
                            write!(f, "\n    {}L", v)?;
                        }
                    } else {
                        write!(f, "\n    {}", val)?;
                    }
                    off += w;
                }
                write!(f, "\n.end array-data")
            }
        }
    }

    fn display(&self, f: &mut Formatter<'_>, _cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // For payloads, display is the same as display_raw (no constant pool resolution needed)
        self.display_raw(f)
    }
}

impl DisplayInstr for Instruction {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let d: u8 = self.discriminant();
        write!(
            f,
            "{} {}",
            DALVIK_OPCODES[d as usize],
            RawArg(self.format())
        )
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        // const/high16: place 16-bit immediate in high 16 bits of 32-bit reg (low 16 zero).
        // Baksmali prints the full 32-bit value, not the raw immediate.
        if let Self::ConstHigh16(fmt) = self {
            let val = (fmt.lit as i32) << 16;
            return write!(f, "const/high16 {}, {}", DispArg(fmt.a, cp), val);
        }
        // const-wide/high16: place 16-bit immediate in high 16 bits of 64-bit reg (low 48 zero).
        // Baksmali prints the full 64-bit value (e.g. -0x8000000000000000L), not the raw immediate.
        if let Self::ConstWideHigh16(fmt) = self {
            let wide_val = (fmt.lit as i64) << 48;
            let (sign, abs_hex) = if wide_val < 0 {
                ("-", format!("0x{:x}", (wide_val as u64)))
            } else {
                ("", format!("0x{:x}", wide_val as u64))
            };
            return write!(
                f,
                "const-wide/high16 {}, {}{}L",
                DispArg(fmt.a, cp),
                sign,
                abs_hex
            );
        }

        let d: u8 = self.discriminant();
        write!(
            f,
            "{} {}",
            DALVIK_OPCODES[d as usize],
            DispArg(self.format(), cp)
        )
    }
}

pub static DALVIK_OPCODES: [&str; 256] = [
    /* 0x00 */
    "nop",
    "move",
    "move/from16",
    "move/16",
    "move-wide",
    "move-wide/from16",
    "move-wide/16",
    "move-object",
    "move-object/from16",
    "move-object/16",
    "move-result",
    "move-result-wide",
    "move-result-object",
    "move-exception",
    "return-void",
    "return",
    /* 0x10 */
    "return-wide",
    "return-object",
    "const/4",
    "const/16",
    "const",
    "const/high16",
    "const-wide/16",
    "const-wide/32",
    "const-wide",
    "const-wide/high16",
    "const-string",
    "const-string/jumbo",
    "const-class",
    "monitor-enter",
    "monitor-exit",
    "check-cast",
    /* 0x20 */
    "instance-of",
    "array-length",
    "new-instance",
    "new-array",
    "filled-new-array",
    "filled-new-array/range",
    "fill-array-data",
    "throw",
    "goto",
    "goto/16",
    "goto/32",
    "packed-switch",
    "sparse-switch",
    "cmpl-float",
    "cmpg-float",
    "cmpl-double",
    /* 0x30 */
    "cmpg-double",
    "cmp-long",
    "if-eq",
    "if-ne",
    "if-lt",
    "if-ge",
    "if-gt",
    "if-le",
    "if-eqz",
    "if-nez",
    "if-ltz",
    "if-gez",
    "if-gtz",
    "if-lez",
    "unused-3e",
    "unused-3f",
    /* 0x40 */
    "unused-40",
    "unused-41",
    "unused-42",
    "unused-43",
    "aget",
    "aget-wide",
    "aget-object",
    "aget-boolean",
    "aget-byte",
    "aget-char",
    "aget-short",
    "aput",
    "aput-wide",
    "aput-object",
    "aput-boolean",
    "aput-byte",
    /* 0x50 */
    "aput-char",
    "aput-short",
    "iget",
    "iget-wide",
    "iget-object",
    "iget-boolean",
    "iget-byte",
    "iget-char",
    "iget-short",
    "iput",
    "iput-wide",
    "iput-object",
    "iput-boolean",
    "iput-byte",
    "iput-char",
    "iput-short",
    /* 0x60 */
    "sget",
    "sget-wide",
    "sget-object",
    "sget-boolean",
    "sget-byte",
    "sget-char",
    "sget-short",
    "sput",
    "sput-wide",
    "sput-object",
    "sput-boolean",
    "sput-byte",
    "sput-char",
    "sput-short",
    "invoke-virtual",
    "invoke-super",
    /* 0x70 */
    "invoke-direct",
    "invoke-static",
    "invoke-interface",
    "unused-73",
    "invoke-virtual/range",
    "invoke-super/range",
    "invoke-direct/range",
    "invoke-static/range",
    "invoke-interface/range",
    "unused-79",
    "unused-7a",
    "neg-int",
    "not-int",
    "neg-long",
    "not-long",
    "neg-float",
    /* 0x80 */
    "neg-double",
    "int-to-long",
    "int-to-float",
    "int-to-double",
    "long-to-int",
    "long-to-float",
    "long-to-double",
    "float-to-int",
    "float-to-long",
    "float-to-double",
    "double-to-int",
    "double-to-long",
    "double-to-float",
    "int-to-byte",
    "int-to-char",
    "int-to-short",
    /* 0x90 */
    "add-int",
    "sub-int",
    "mul-int",
    "div-int",
    "rem-int",
    "and-int",
    "or-int",
    "xor-int",
    "shl-int",
    "shr-int",
    "ushr-int",
    "add-long",
    "sub-long",
    "mul-long",
    "div-long",
    "rem-long",
    /* 0xA0 */
    "and-long",
    "or-long",
    "xor-long",
    "shl-long",
    "shr-long",
    "ushr-long",
    "add-float",
    "sub-float",
    "mul-float",
    "div-float",
    "rem-float",
    "add-double",
    "sub-double",
    "mul-double",
    "div-double",
    "rem-double",
    /* 0xB0 */
    "add-int/2addr",
    "sub-int/2addr",
    "mul-int/2addr",
    "div-int/2addr",
    "rem-int/2addr",
    "and-int/2addr",
    "or-int/2addr",
    "xor-int/2addr",
    "shl-int/2addr",
    "shr-int/2addr",
    "ushr-int/2addr",
    "add-long/2addr",
    "sub-long/2addr",
    "mul-long/2addr",
    "div-long/2addr",
    "rem-long/2addr",
    /* 0xC0 */
    "and-long/2addr",
    "or-long/2addr",
    "xor-long/2addr",
    "shl-long/2addr",
    "shr-long/2addr",
    "ushr-long/2addr",
    "add-float/2addr",
    "sub-float/2addr",
    "mul-float/2addr",
    "div-float/2addr",
    "rem-float/2addr",
    "add-double/2addr",
    "sub-double/2addr",
    "mul-double/2addr",
    "div-double/2addr",
    "rem-double/2addr",
    /* 0xD0 */
    "add-int/lit16",
    "rsub-int",
    "mul-int/lit16",
    "div-int/lit16",
    "rem-int/lit16",
    "and-int/lit16",
    "or-int/lit16",
    "xor-int/lit16",
    "add-int/lit8",
    "rsub-int/lit8",
    "mul-int/lit8",
    "div-int/lit8",
    "rem-int/lit8",
    "and-int/lit8",
    "or-int/lit8",
    "xor-int/lit8",
    /* 0xE0 */
    "shl-int/lit8",
    "shr-int/lit8",
    "ushr-int/lit8",
    "unused-e3",
    "unused-e4",
    "unused-e5",
    "unused-e6",
    "unused-e7",
    "unused-e8",
    "unused-e9",
    "unused-ea",
    "unused-eb",
    "unused-ec",
    "unused-ed",
    "unused-ee",
    "unused-ef",
    /* 0xF0 */
    "unused-f0",
    "unused-f1",
    "unused-f2",
    "unused-f3",
    "unused-f4",
    "unused-f5",
    "unused-f6",
    "unused-f7",
    "unused-f8",
    "unused-f9",
    "unused-fa",
    "unused-fb",
    "unused-fc",
    "unused-fd",
    "unused-fe",
    "unused-ff",
];

pub enum Format<'a> {
    Format10t(&'a Format10t),
    Format10x(&'a Format10x),
    Format11n(&'a Format11n),
    Format11x(&'a Format11x),
    Format12x(&'a Format12x),
    Format20t(&'a Format20t),
    Format21cF(&'a Format21c<FieldIdx>),
    Format21cM(&'a Format21c<MethodIdx>),
    Format21cP(&'a Format21c<ProtoIdx>),
    Format21cS(&'a Format21c<StringIdx>),
    Format21cT(&'a Format21c<TypeIdx>),
    Format21h(&'a Format21h),
    Format21s(&'a Format21s),
    Format21t(&'a Format21t),
    Format22b(&'a Format22b),
    Format22cF(&'a Format22c<FieldIdx>),
    Format22cT(&'a Format22c<TypeIdx>),
    Format22s(&'a Format22s),
    Format22t(&'a Format22t),
    Format22x(&'a Format22x),
    Format23x(&'a Format23x),
    Format30t(&'a Format30t),
    Format31c(&'a Format31c),
    Format31i(&'a Format31i),
    Format31t(&'a Format31t),
    Format32x(&'a Format32x),
    Format35cC(&'a Format35c<CallIdx>),
    Format35cM(&'a Format35c<MethodIdx>),
    Format35cT(&'a Format35c<TypeIdx>),
    Format3rcC(&'a Format3rc<CallIdx>),
    Format3rcM(&'a Format3rc<MethodIdx>),
    Format3rcT(&'a Format3rc<TypeIdx>),
    Format45cc(&'a Format45cc),
    Format4rcc(&'a Format4rcc),
    Format51l(&'a Format51l),
    FormatUnused(&'a FormatUnused),
}

impl<'a> DisplayInstr for Format<'a> {
    fn display_raw(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Format::Format10t(fmt) => fmt.display_raw(f),
            Format::Format10x(fmt) => fmt.display_raw(f),
            Format::Format11n(fmt) => fmt.display_raw(f),
            Format::Format11x(fmt) => fmt.display_raw(f),
            Format::Format12x(fmt) => fmt.display_raw(f),
            Format::Format20t(fmt) => fmt.display_raw(f),
            Format::Format21cF(fmt) => fmt.display_raw(f),
            Format::Format21cM(fmt) => fmt.display_raw(f),
            Format::Format21cP(fmt) => fmt.display_raw(f),
            Format::Format21cS(fmt) => fmt.display_raw(f),
            Format::Format21cT(fmt) => fmt.display_raw(f),
            Format::Format21h(fmt) => fmt.display_raw(f),
            Format::Format21s(fmt) => fmt.display_raw(f),
            Format::Format21t(fmt) => fmt.display_raw(f),
            Format::Format22b(fmt) => fmt.display_raw(f),
            Format::Format22cF(fmt) => fmt.display_raw(f),
            Format::Format22cT(fmt) => fmt.display_raw(f),
            Format::Format22s(fmt) => fmt.display_raw(f),
            Format::Format22t(fmt) => fmt.display_raw(f),
            Format::Format22x(fmt) => fmt.display_raw(f),
            Format::Format23x(fmt) => fmt.display_raw(f),
            Format::Format30t(fmt) => fmt.display_raw(f),
            Format::Format31c(fmt) => fmt.display_raw(f),
            Format::Format31i(fmt) => fmt.display_raw(f),
            Format::Format31t(fmt) => fmt.display_raw(f),
            Format::Format32x(fmt) => fmt.display_raw(f),
            Format::Format35cC(fmt) => fmt.display_raw(f),
            Format::Format35cM(fmt) => fmt.display_raw(f),
            Format::Format35cT(fmt) => fmt.display_raw(f),
            Format::Format3rcC(fmt) => fmt.display_raw(f),
            Format::Format3rcM(fmt) => fmt.display_raw(f),
            Format::Format3rcT(fmt) => fmt.display_raw(f),
            Format::Format45cc(fmt) => fmt.display_raw(f),
            Format::Format4rcc(fmt) => fmt.display_raw(f),
            Format::Format51l(fmt) => fmt.display_raw(f),
            Format::FormatUnused(fmt) => fmt.display_raw(f),
        }
    }

    fn display(&self, f: &mut Formatter<'_>, cp: &DexConstantPool) -> Result<(), std::fmt::Error> {
        match self {
            Format::Format10t(fmt) => fmt.display(f, cp),
            Format::Format10x(fmt) => fmt.display(f, cp),
            Format::Format11n(fmt) => fmt.display(f, cp),
            Format::Format11x(fmt) => fmt.display(f, cp),
            Format::Format12x(fmt) => fmt.display(f, cp),
            Format::Format20t(fmt) => fmt.display(f, cp),
            Format::Format21cF(fmt) => fmt.display(f, cp),
            Format::Format21cM(fmt) => fmt.display(f, cp),
            Format::Format21cP(fmt) => fmt.display(f, cp),
            Format::Format21cS(fmt) => fmt.display(f, cp),
            Format::Format21cT(fmt) => fmt.display(f, cp),
            Format::Format21h(fmt) => fmt.display(f, cp),
            Format::Format21s(fmt) => fmt.display(f, cp),
            Format::Format21t(fmt) => fmt.display(f, cp),
            Format::Format22b(fmt) => fmt.display(f, cp),
            Format::Format22cF(fmt) => fmt.display(f, cp),
            Format::Format22cT(fmt) => fmt.display(f, cp),
            Format::Format22s(fmt) => fmt.display(f, cp),
            Format::Format22t(fmt) => fmt.display(f, cp),
            Format::Format22x(fmt) => fmt.display(f, cp),
            Format::Format23x(fmt) => fmt.display(f, cp),
            Format::Format30t(fmt) => fmt.display(f, cp),
            Format::Format31c(fmt) => fmt.display(f, cp),
            Format::Format31i(fmt) => fmt.display(f, cp),
            Format::Format31t(fmt) => fmt.display(f, cp),
            Format::Format32x(fmt) => fmt.display(f, cp),
            Format::Format35cC(fmt) => fmt.display(f, cp),
            Format::Format35cM(fmt) => fmt.display(f, cp),
            Format::Format35cT(fmt) => fmt.display(f, cp),
            Format::Format3rcC(fmt) => fmt.display(f, cp),
            Format::Format3rcM(fmt) => fmt.display(f, cp),
            Format::Format3rcT(fmt) => fmt.display(f, cp),
            Format::Format45cc(fmt) => fmt.display(f, cp),
            Format::Format4rcc(fmt) => fmt.display(f, cp),
            Format::Format51l(fmt) => fmt.display(f, cp),
            Format::FormatUnused(fmt) => fmt.display(f, cp),
        }
    }
}
