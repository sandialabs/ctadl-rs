use std::ops::Deref;

use u64_dyn::{pack_u64_dyn as pack, unpack_u64_dyn as unpack};

// Packade u64 offset
pub struct BytePos(Vec<u8>);
#[derive(Copy, Clone)]
pub struct ByteLen(u8);

impl From<u64> for BytePos {
    #[inline]
    fn from(val: u64) -> Self {
        let bytes = pack(val);
        Self(bytes)
    }
}

impl From<&BytePos> for u64 {
    #[inline]
    fn from(pos: &BytePos) -> u64 {
        let (value, _len) = unpack(&pos.0).unwrap();
        value
    }
}

impl Deref for BytePos {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<u64> for ByteLen {
    type Error = &'static str;
    #[inline]
    fn try_from(val: u64) -> Result<Self, Self::Error> {
        match val.try_into() {
            Ok(val) => Ok(Self(val)),
            Err(_) => Err("u64 too big"),
        }
    }
}

impl TryFrom<u32> for ByteLen {
    type Error = &'static str;
    #[inline]
    fn try_from(val: u32) -> Result<Self, Self::Error> {
        match val.try_into() {
            Ok(val) => Ok(Self(val)),
            Err(_) => Err("u32 too big"),
        }
    }
}

impl From<u8> for ByteLen {
    fn from(v: u8) -> Self {
        Self(v)
    }
}

impl From<ByteLen> for u8 {
    #[inline]
    fn from(l: ByteLen) -> u8 {
        l.0
    }
}
