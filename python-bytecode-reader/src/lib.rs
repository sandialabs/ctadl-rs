/*!
Reader for the **stable Python-bytecode text format**.

This crate is the version-independent boundary between Python and CTADL. A small
Python tool (`bytecode_text`, embedded here and staged at run time) compiles
source (or reads `.pyc`), normalizes each `dis.Instruction` into a
version-independent record, and emits a stable text format that a `pest` grammar
here parses into typed records ([`BytecodeFile`] / [`CodeObject`] /
[`Instruction`]).

The crate carries **no dependency on CTADL's IR**: lowering to IR lives in the
CTADL frontend (`ctadl-ascent/src/languages/python`). Two layers, kept separate
exactly like `pcode-reader` (pure parsing) vs. `languages/pcode` (lowering):

- [`parse`] — stable text → typed records (pure, no Python spawned).
- [`serialize::run_serializer`] — the single place that spawns the Python tool.

See `FORMAT.md` for the text format.
*/

pub mod error;
pub mod extract;
pub mod model;
mod parse;
pub mod serialize;

pub use error::{ParseError, SerializeError};
pub use extract::{SUPPORTED_FORMAT_VERSION, parse};
pub use model::{BytecodeFile, CodeObject, ConstEntry, Instruction, Position};
pub use serialize::{Format, run_serializer};
