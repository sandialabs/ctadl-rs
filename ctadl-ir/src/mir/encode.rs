use bitcode::Error;

use super::*;

#[cfg(feature = "serde")]
#[inline]
pub fn encode_program(p: &Program) -> Result<Vec<u8>, Error> {
    bitcode::serialize(p)
}

#[cfg(feature = "serde")]
#[inline]
pub fn decode_program(bytes: &[u8]) -> Result<Program, Error> {
    bitcode::deserialize(bytes)
}
