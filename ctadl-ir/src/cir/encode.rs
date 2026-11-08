#[cfg(feature = "serde")]
use super::*;
#[cfg(feature = "serde")]
use crate::cir::call::VirtualMethodTable;
#[cfg(feature = "serde")]
use bitcode::Error;

/// The first bytes of an encoded [`Program`].
///
/// A program is written as a sequence of frames rather than as one bitcode value: bitcode lays a
/// value out column by column, so encoding a whole program means holding all its columns at
/// once (on a large app, a gigabyte on top of the program itself) and doing it on one thread.
/// Frames are independent, so they are encoded and decoded in parallel, and written as they are
/// done.
///
/// The layout, all integers little-endian:
///
/// ```text
/// magic           8 bytes, PROGRAM_MAGIC
/// frame count     u64, not counting the globals frame
/// globals frame   u64 length, then the bitcode of `Program::globals`
/// function frames u64 length, then the bitcode of up to FUNCTIONS_PER_FRAME functions, in order
/// ```
#[cfg(feature = "serde")]
const PROGRAM_MAGIC: &[u8; 8] = b"ctadlPF1";

/// Functions per frame of an encoded program: small enough that a frame's columns are a small
/// amount of memory and that frames spread over every thread, large enough that bitcode's
/// columns stay long.
#[cfg(feature = "serde")]
const FUNCTIONS_PER_FRAME: usize = 2048;

/// Encodes a [`Program`] into the bytes of an `ir-program.bitcode` file. See [`PROGRAM_MAGIC`]
/// for the layout; [`encode_program_to`] writes it out without building it in memory first.
#[cfg(feature = "serde")]
pub fn encode_program(p: &Program) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    match encode_frames(p, |bytes| {
        out.extend_from_slice(bytes);
        Ok::<(), std::convert::Infallible>(())
    })? {
        Ok(()) => Ok(out),
    }
}

/// Writes a [`Program`] to `w` as [`encode_program`] encodes it, frame by frame, so that only a
/// few frames are in memory at a time.
#[cfg(feature = "serde")]
pub fn encode_program_to<W: std::io::Write>(p: &Program, mut w: W) -> std::io::Result<()> {
    encode_frames(p, |bytes| w.write_all(bytes)).map_err(std::io::Error::other)??;
    w.flush()
}

/// Encodes `p` frame by frame on the rayon pool, handing the bytes to `sink` in file order. A
/// failure to encode is the outer error, a failure of `sink` the inner one.
#[cfg(feature = "serde")]
fn encode_frames<E>(
    p: &Program,
    mut sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<Result<(), E>, Error> {
    use rayon::prelude::*;
    let chunks: Vec<&[FunctionData]> = p
        .functions
        .functions
        .raw
        .chunks(FUNCTIONS_PER_FRAME)
        .collect();
    let globals = bitcode::serialize(&p.globals)?;
    if let Err(e) = sink(PROGRAM_MAGIC).and_then(|()| sink(&(chunks.len() as u64).to_le_bytes())) {
        return Ok(Err(e));
    }
    let mut frame = |bytes: &[u8]| -> Result<(), E> {
        sink(&(bytes.len() as u64).to_le_bytes())?;
        sink(bytes)
    };
    if let Err(e) = frame(&globals) {
        return Ok(Err(e));
    }
    // A batch of frames is encoded at once, then written in order and freed.
    let batch = (2 * rayon::current_num_threads()).max(1);
    for group in chunks.chunks(batch) {
        let encoded: Vec<Vec<u8>> = group
            .par_iter()
            .map(bitcode::serialize)
            .collect::<Result<_, _>>()?;
        for bytes in encoded {
            if let Err(e) = frame(&bytes) {
                return Ok(Err(e));
            }
        }
    }
    Ok(Ok(()))
}

/// Decodes the bytes of an `ir-program.bitcode` file, its frames in parallel.
///
/// Bytes that do not start with [`PROGRAM_MAGIC`] are decoded as one bitcode value, the layout
/// before frames. `ctadl_import`'s format version check normally refuses such a file before it
/// gets here.
#[cfg(feature = "serde")]
pub fn decode_program(bytes: &[u8]) -> Result<Program, Error> {
    use rayon::prelude::*;
    let Some(mut rest) = bytes.strip_prefix(PROGRAM_MAGIC.as_slice()) else {
        return bitcode::deserialize(bytes);
    };
    let take_u64 = |rest: &mut &[u8]| -> Result<u64, Error> {
        let (n, tail) = rest.split_first_chunk::<8>().ok_or_else(malformed)?;
        *rest = tail;
        Ok(u64::from_le_bytes(*n))
    };
    let count = take_u64(&mut rest)?;
    let mut frames: Vec<&[u8]> = Vec::new();
    for _ in 0..=count {
        let len = usize::try_from(take_u64(&mut rest)?).map_err(|_| malformed())?;
        if rest.len() < len {
            return Err(malformed());
        }
        let (frame, tail) = rest.split_at(len);
        frames.push(frame);
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(malformed());
    }
    let globals: Globals = bitcode::deserialize(frames[0])?;
    let decoded: Vec<Vec<FunctionData>> = frames[1..]
        .par_iter()
        .map(|frame| bitcode::deserialize(frame))
        .collect::<Result<_, _>>()?;
    let mut functions = Vec::with_capacity(decoded.iter().map(Vec::len).sum());
    for fs in decoded {
        functions.extend(fs);
    }
    Ok(Program {
        functions: Functions {
            functions: IndexVec::from_raw(functions),
        },
        globals,
    })
}

/// The error for bytes that are not a well-formed encoded program. bitcode's error has no public
/// constructor, so this is the one it gives for input that ends too soon.
#[cfg(feature = "serde")]
fn malformed() -> Error {
    bitcode::deserialize::<u64>(&[]).expect_err("an empty input is not a u64")
}

/// Encodes a [`VirtualMethodTable`] into the `ir-vmt.bitcode` file that sits next to the
/// program.
///
/// Like [`encode_program`] for the store's other file, so that a reader of an import does not
/// have to know how the table was written.
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
    use crate::cir::call::VirtualMethodTable;

    /// A program of several frames, and one of none, survive [`encode_program`] and
    /// [`decode_program`], and [`encode_program_to`] writes the same bytes.
    #[test]
    fn program_round_trips_across_frames() {
        for n in [0, 1, FUNCTIONS_PER_FRAME, 2 * FUNCTIONS_PER_FRAME + 3] {
            let mut program = Program::default();
            program.declare_global("g", None);
            for i in 0..n {
                let f = program.new_function();
                program[f].name = format!("f{i}");
                program[f].locals.get_or_intern("x");
            }
            let bytes = encode_program(&program).expect("encode");
            let mut written = Vec::new();
            encode_program_to(&program, &mut written).expect("write");
            assert_eq!(bytes, written);
            let back = decode_program(&bytes).expect("decode");
            assert_eq!(back.functions.functions.raw.len(), n);
            assert_eq!(program.to_string(), back.to_string());
            assert_eq!(back.globals.len(), 1);
        }
    }

    /// A program cut short anywhere is an error, not a shorter program.
    #[test]
    fn a_truncated_program_does_not_decode() {
        let mut program = Program::default();
        for i in 0..3 {
            let f = program.new_function();
            program[f].name = format!("f{i}");
        }
        let bytes = encode_program(&program).expect("encode");
        for len in [9, 17, bytes.len() - 1] {
            assert!(
                decode_program(&bytes[..len]).is_err(),
                "decoded {len} bytes"
            );
        }
    }

    /// Checks that each kind of table survives a trip through [`encode_vmt`] and
    /// [`decode_vmt`].
    #[test]
    fn vmt_round_trips() {
        for vmt in [
            VirtualMethodTable::default(),
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
