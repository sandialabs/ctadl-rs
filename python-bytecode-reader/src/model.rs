//! Typed records produced by parsing the stable bytecode text.
//!
//! These are the reader's *own* types; they carry no dependency on CTADL's IR.
//! Field names and enum shapes are chosen so that, with the optional `serde`
//! feature, a [`BytecodeFile`] deserializes directly from the Python JSON oracle
//! (see the differential test), making the oracle comparison a single
//! `assert_eq!`.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A parsed bytecode document: a format version and its top-level code objects.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct BytecodeFile {
    pub format_version: u32,
    pub code_objects: Vec<CodeObject>,
}

/// A Python code object: its metadata, symbol tables, instructions, and any
/// lexically-nested code objects (comprehensions, closures, class/def bodies).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CodeObject {
    pub name: String,
    pub qualname: String,
    pub filename: String,
    /// First source line of the code object. `None` when the interpreter did not
    /// record one (e.g. some synthesized code objects).
    pub first_line: Option<i64>,
    pub flags: i64,
    /// Number of positional parameters (`co_argcount`, including positional-only).
    pub arg_count: i64,
    /// Number of keyword-only parameters (`co_kwonlyargcount`).
    pub kwonly_count: i64,
    /// `co_names`: global/attribute names referenced by the code object.
    pub names: Vec<String>,
    /// `co_varnames`: local variable names (parameters come first).
    pub varnames: Vec<String>,
    /// `co_consts`, normalized. A const that is itself a code object is a
    /// [`ConstEntry::Code`] carrying the document-order id of the matching entry
    /// in [`Self::nested_code_objects`] (numbered pre-order over the whole file).
    pub consts: Vec<ConstEntry>,
    pub instructions: Vec<Instruction>,
    pub nested_code_objects: Vec<CodeObject>,
}

/// A normalized `co_consts` entry (also used for an instruction's resolved
/// `argval`). Absent/`None`-valued operands are [`ConstEntry::None`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ConstEntry {
    /// The literal `none` token: an absent operand or the Python `None` singleton.
    None,
    Bool(bool),
    Int(i64),
    /// A float, kept as its `repr` string so `inf`/`nan`/precision round-trip and
    /// no float parsing/formatting mismatch can occur.
    Float(String),
    Str(String),
    /// A bytes constant, decoded latin-1 (each byte is one code point) so it is a
    /// lossless, JSON-escapable string.
    Bytes(String),
    /// A reference to a nested code object by its document-order id.
    Code(u32),
    /// Any other constant (tuple, frozenset, ...), kept as its `repr` string.
    Other(String),
}

/// A single normalized instruction (`dis.Instruction`), version-independent.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Instruction {
    pub offset: i64,
    pub opname: String,
    pub opcode: i64,
    /// The raw `oparg`, or `None` when the opcode takes no argument.
    pub arg: Option<i64>,
    /// The *resolved* operand: a name, const value, or (for a branch) the target
    /// offset. [`ConstEntry::None`] when there is none.
    pub argval: ConstEntry,
    pub argrepr: Option<String>,
    pub starts_line: Option<i64>,
    pub is_jump_target: bool,
    /// Resolved successor offsets for a branch/jump op; empty otherwise.
    pub jump_targets: Vec<i64>,
    pub position: Option<Position>,
}

/// A source position span (`co_positions` / `dis.Positions`). `None` on the
/// instruction when any component was unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Position {
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
}
