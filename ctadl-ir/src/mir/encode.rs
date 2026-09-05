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

/// Encodes a [`VirtualMethodTable`] into the `ir-vmt.bitcode` file that sits next to the
/// program.
///
/// The one place that wrote the table used to call `bitcode::serialize` directly. That left the
/// store's second file without a helper to match [`encode_program`], so anything that read an
/// import had to know how the table was written and repeat it. Both the encoder and the decoder
/// now live here. The encoding is the same one that call site used, so files written earlier
/// still read back correctly.
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

    /// Checks that both store files survive a trip through their own encode and decode
    /// helpers. The table helpers are the new ones. They exist so that a reader does not have
    /// to know the writer called `bitcode::serialize` directly.
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

    /// Checks that every `Exp` variant survives being encoded and decoded.
    #[test]
    fn exp_variants_round_trip() {
        let exps = [
            Exp::new_int(0),
            Exp::new_int(1),
            Exp::new_int(-1),
            Exp::new_int(i64::MIN),
            Exp::new_int(i64::MAX),
            Exp::new_str("s"),
            Exp::new_bytes(vec![0, 0, 0, 0, 0, 0, 0, 1]),
        ];
        for exp in &exps {
            let bytes = bitcode::serialize(exp).expect("encode");
            let back: Exp = bitcode::deserialize(&bytes).expect("decode");
            assert_eq!(exp, &back);
        }
        // Written big-endian, `1i64` is the same eight bytes as the blob above, and the two
        // are still not equal.
        assert_ne!(exps[1], exps[6]);
    }

    /// `Int` prints its value in decimal, so a given constant looks the same in an IR dump no
    /// matter which opcode produced it.
    #[test]
    fn int_displays_in_decimal() {
        assert_eq!(
            Exp::new_int(-2147483648).to_string(),
            "<const: -2147483648>"
        );
        assert_eq!(Exp::new_int(1).to_string(), "<const: 1>");
        assert_eq!(Exp::new_int(1).as_int(), Some(1));
        // A byte blob has no number to recover, so `as_int` returns `None` rather than
        // guessing one.
        assert_eq!(Exp::new_bytes(vec![1]).as_int(), None);
    }

    #[test]
    fn program_round_trips() {
        let program = Program::default();
        let bytes = encode_program(&program).expect("encode");
        let back = decode_program(&bytes).expect("decode");
        assert_eq!(program.to_string(), back.to_string());
    }
}
