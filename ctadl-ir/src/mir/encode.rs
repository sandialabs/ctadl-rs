#[cfg(feature = "serde")]
use super::*;
#[cfg(feature = "serde")]
use crate::mir::call::VirtualMethodTable;
#[cfg(feature = "serde")]
use bitcode::Error;

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

/// Encodes a [`VirtualMethodTable`] for the `ir-vmt.bitcode` artifact beside the program.
///
/// The VMT used to be written with a bare `bitcode::serialize` at the one call site that wrote
/// it, which left the store's second artifact with no helper to mirror [`encode_program`] --
/// so anything reading an import had to know that detail and restate it. Both halves live here
/// now, and the encoding is the same one that call site used, so existing artifacts read back
/// unchanged.
#[cfg(feature = "serde")]
#[inline]
pub fn encode_vmt(vmt: &VirtualMethodTable) -> Result<Vec<u8>, Error> {
    bitcode::serialize(vmt)
}

/// Decodes a [`VirtualMethodTable`]. See [`encode_vmt`].
#[cfg(feature = "serde")]
#[inline]
pub fn decode_vmt(bytes: &[u8]) -> Result<VirtualMethodTable, Error> {
    bitcode::deserialize(bytes)
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use crate::mir::call::VirtualMethodTable;

    /// The two store artifacts round-trip through the helpers that name them. The VMT half is
    /// the new one: it exists so a reader does not have to know that the writer reached for
    /// `bitcode::serialize` directly.
    #[test]
    fn vmt_round_trips() {
        for vmt in [
            VirtualMethodTable::Unknown,
            VirtualMethodTable::new_java(),
            VirtualMethodTable::new_native(),
            VirtualMethodTable::new_lua(),
        ] {
            let bytes = encode_vmt(&vmt).expect("encode");
            let back = decode_vmt(&bytes).expect("decode");
            assert_eq!(vmt.to_string(), back.to_string());
        }
    }

    #[test]
    fn program_round_trips() {
        let program = Program::default();
        let bytes = encode_program(&program).expect("encode");
        let back = decode_program(&bytes).expect("decode");
        assert_eq!(program.to_string(), back.to_string());
    }
}
