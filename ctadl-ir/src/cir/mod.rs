/*!
# CTADL IR

The CIR is our intermediate representation for data flow analysis. It is not a general purpose IR:
it's optimized for analyzing data flow only, not for compilation, or type analysis. This
representation sits between a frontend AST and the Datalog "facts" format. The representation is a
typical basic block and statement representation. Successors are expressed as "gotos" to other
blocks. These gotos are unconditional; the IR abstracts conditional jumps and just the possible
targets of control flow. Terminator instructions are returns and gotos with multiple successors.

The semantics of the IR is in `docs/ir.md`; [`path`] owns the path equations.

- Expressions are access paths ([`Exp::Path`]), the value of an access path ([`Exp::Value`]),
  target labels (function and class references), string labels and inert constants.

- [`StatementKind::Assign`] represents assignments. Its destination and sources are full access
  paths. With an empty destination path it is a *copy* and defines the destination variable (a
  load `x = y.f` is a copy); with a non-empty one it is a *store* and defines nothing.

- Calls come in two flavors: direct and indirect. Direct calls are tagged with call edges. Indirect
  calls are tagged with an indirect call style. Calls can be internal or external to a program. Call
  resolution is based on the target function's name, independent of whether we have a definition for
  the function.

- Functions have a name, sequence of parameters, and basic blocks.

- Basic blocks are stored in an array inside the function; the 0'th block is the function's start
  block. Basic blocks are terminated with either [`TerminatorKind::Return`] instructions or
  [`TerminatorKind::Goto`]. Gotos are non-deterministic. Variable occurrences in basic blocks
  refer to parameters, local variables, or globals.  There are no local variable declarations;
  assignment to a variable is sufficient for it to exist.

- Parameters are unnamed and referred to by number. Each has a [`ParameterType`] saying which of
  its access paths the caller shares. It is an error to refer to a parameter that is not declared
  by the enclosing function.

- Functions have a return type which is just an arity. Return statements can return multiple values
  and the function declares the arity each return statement must have. This is not necessary to
  emulate returning by reference; like having multiple parallel assignments, it's just a convenience.

- A program declares its global variables, which every function shares. See "How to Generate
  CIR" below.

CIR variables are untyped, but represent an object. Variables have fields that you can load and
store simply by accessing them with the appropriate statements.

There is a CIR visitor in [`self::visit`] which can be used for immutable and mutable traversal.

We have designed the CIR on purpose to exist in inconsistent states; this can be helpful when
generating code. Once you're done generating code, it should be verified. Verification checks for
the various kinds of errors that should not happen in well-defined programs. It can be checked with
[`CirVerify`] or by simply calling [`Program::verify`].

To make a program suitable for Datalog compilation, one can use SSA transformation
([`crate::ssa::transform`]).

## How to Generate CIR

Frontend language assignment statements can be modeled with [`StatementKind::Assign`] instructions.
Assignments like `x = a + b` can be modeled with `(x, x) = (a, b)`. Expressions must be linearized
before conversion. For instance, a frontend language expression like `x = a + (b + c)` can be
linearized as `(t1, t1) = (b, c); (t2, t2) = (a, t1); x = t2`.

Stores into objects and structures often look like `obj.x.y = w` in frontend languages. These are
modeled as [`StatementKind::Assign`] instructions whose destination is the access path `obj.x.y`.
Statements like `obj.x = F(y.z)` have to be split into two CIR instructions: first, call the
function and return into a temporary like `t1 = F(y.z)`; next, store the temporary to the
destination object.

A program declares its global variables ([`Program::declare_global`]), each with a name and
optionally a span, and refers to one with [`Variable::Global`]. A global is a variable like any
other: an access path may have it as its base, and it prints as `$name` (G1). It differs from a
local in three ways. Every instance of every function shares it (G2), so nothing passes it at a
call. Its identity is its name: two programs of a project that declare `g` declare one variable
(G3), and a front end that wants a global of its own qualifies the name (`libA.so::ram`). And it
is never rebound (G4): never the destination of a copy or of a call result, never versioned by
SSA. Its storage is the locations below it, so a front end lowers a global `speed` to a location
such as `$speed.deref` (C) or a class `C`'s static field `f` to `$C.f` (Java): loading it is an
assignment whose source is that access path, and storing to it is one whose destination is.

Extern functions (functions that are called, for example, but not defined) are modeled with a
[`FunctionData`] and empty basic blocks.

# Source info

We need to report the source locations of instructions when we report taint results. I considered a
naming scheme so that source info could be held externally, off to the side. The problem is that we
don't have a good name for instructions that survives reordering. I didn't want to give a unique
name to each instructions. I decided, instead, to follow the pattern of rust's MIR and store source
info into the instructions themselves.

# Naming of IR Items

**This section is in progress and isn't correct yet.**

There is a naming convention that provides a unique name for every IR element. Function name is
outermost. Next is a namespace either "param" or "local" or "block".
- For parameters, next is the parameter name.
- For local variables, next is the variable name.
- For basic blocks, next is an index into the basic block. Next is an index into the instruction.

This enables us to correlate location information and models with unique IDs for variables,
parameters, and instructions.

# Future

- Varargs parameter passing

*/
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::{fmt, fmt::Display};

use internment::ArcIntern;
use smallvec::SmallVec;
use thin_vec::ThinVec;

use crate::cir::call::VirtualMethodTable;
pub use crate::cir::terminator::{Terminator, TerminatorKind};
use crate::cir::visit::Visitor;
pub use crate::cir::{
    basic_blocks::BasicBlocks,
    call::{CallEdges, CallObject, CallStyle},
    verify::{CirVerify, VerifyError, VerifyErrors},
};
use crate::index::{idx::Idx, index_vec::IndexVec, index_vec_deque::IndexVecDeque};
use crate::newtype_index;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod builder_tests;

mod basic_blocks;
pub mod builder;
pub mod call;
pub mod encode;
pub mod path;
pub mod path_syntax;
pub mod pos;
mod terminator;
mod verify;
pub mod visit;

pub use crate::cir::path_syntax::{
    PathSyntaxError, PathSyntaxErrorKind, parse_path, parse_segment, parse_segments,
    path_to_string, segment_to_string, write_path, write_segment,
};

// Index into basic blocks in `BasicBlocks`
newtype_index!(BasicBlockIdx, u32);
// Index into functions in `Functions`
newtype_index!(FunctionIdx, u32);
// Index into statements in `BasicBlockData`
newtype_index!(StatementIdx, u32);

impl BasicBlockIdx {
    pub const START_BLOCK: BasicBlockIdx = BasicBlockIdx::ZERO;
}

pub type Symbol = ArcIntern<str>;

/// A newtype wrapper for u64 representing a numeric offset in field access
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Offset(pub i64);

/// One segment of an access path (S1 of `docs/ir.md`).
///
/// The four kinds are distinct variants: [`PathSegment::Deref`] and [`PathSegment::Value`] are
/// not field names, so a source-language field named `deref` or `#value` is an ordinary
/// [`PathSegment::Field`] and cannot be mistaken for them. Paths are compared only in normal
/// form; [`path`] owns the equations.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PathSegment {
    /// A symbolic field name, with no layout (e.g. a C field `f`, a Java field).
    Field(Symbol),
    /// A constant offset: address arithmetic, not a memory access.
    Offset(Offset),
    /// The step from an address to the memory there. Front ends with pointers spell every
    /// memory access as an address followed by this segment, so C's `a[3]` is `a.[3].deref` and
    /// `*p` is `p.deref`.
    ///
    /// Keeping the offset apart from the dereference is what makes element addresses
    /// composable: offsets are summed when paths meet, so a callee's write at `.[1].deref`
    /// through `&a[1]` (the address `a.[1]`) lands on `a.[2].deref`, where a caller's `a[2]`
    /// reads.
    Deref,
    /// What a variable's value was computed from, as opposed to what it holds. [`Exp::Value`]
    /// lowers `t = value(v)` to `t.#value <- v`. Terminal: a field or `deref` after it is void,
    /// and it absorbs itself and adjacent offsets.
    Value,
}

impl PathSegment {
    #[inline]
    pub fn is_field(&self) -> bool {
        matches!(self, PathSegment::Field(_))
    }

    #[inline]
    pub fn is_offset(&self) -> bool {
        matches!(self, PathSegment::Offset(_))
    }

    /// A field segment named `name`. A name such as `deref` is a field like any other.
    #[inline]
    pub fn field<S: AsRef<str>>(name: S) -> Self {
        PathSegment::Field(ArcIntern::from(name.as_ref()))
    }

    #[inline]
    pub fn offset(offset: i64) -> Self {
        PathSegment::Offset(Offset(offset))
    }

    #[inline]
    pub fn is_deref(&self) -> bool {
        matches!(self, PathSegment::Deref)
    }

    #[inline]
    pub fn is_value(&self) -> bool {
        matches!(self, PathSegment::Value)
    }
}

impl From<&str> for PathSegment {
    /// A [`PathSegment::Field`] named `name`.
    #[inline]
    fn from(name: &str) -> Self {
        PathSegment::field(name)
    }
}

impl From<Symbol> for PathSegment {
    /// A [`PathSegment::Field`] named `name`.
    #[inline]
    fn from(name: Symbol) -> Self {
        PathSegment::Field(name)
    }
}

/// Renders one segment in the canonical path grammar, WITHOUT its leading `.`: symbols
/// escape `\`, `.`, and a leading `[`; offsets are decimal in brackets.
impl Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&path_syntax::segment_to_string(self))
    }
}

impl Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Decimal, so the IR dump, the fact store, model ports and the flowy grammar all agree on
        // one spelling. If hex is wanted for readability in an IR dump it belongs in a side
        // comment on the statement, never inside a path.
        write!(f, "{}", self.0)
    }
}

/// IR Statements. They capture assignments and function calls.
///
/// Frontends typically generate assign, load, store, and call instructions. The phi instruction is
/// generated during SSA conversion.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StatementKind {
    /// An assignment `dest = s_1, ..., s_n` (S5 of `docs/ir.md`): each source flows into `dest`,
    /// so several sources are sugar for several assignments.
    ///
    /// The destination is a full access path. With an empty path the assignment is a *copy*: it
    /// defines `dest.base`, so SSA versions it (a load `x = y.f` is a copy, and so is pointer
    /// arithmetic `x = y.[8]`). With a non-empty path it is a *store*: it defines no variable,
    /// writes the memory at `dest`, and reads `dest.base`. [`StatementKind::is_store`] says which;
    /// no pass decides it any other way.
    ///
    /// The destination is never an address: [`StatementKind::assign`] rewrites `p.[k] = e` into
    /// `p = e.[-k]` (S5), and [`CirVerify`] rejects an address destination.
    Assign {
        dest: AccessPath,
        sources: SmallVec<[Exp; 2]>,
    },

    /// Call instructions. Call instructions pass parameters in `args` and return values in `rets`.
    /// Multiple values may be returned. Call handling is complex, depending on a number
    /// of factors such as source language, whether the program analysis is partial, and others.
    /// The `style` expresses how this call should be resolved.
    CallAssign {
        style: CallStyle,
        rets: ThinVec<VariableRef>,
        args: ThinVec<Exp>,
    },

    /// Phi node, typically inserted by SSA conversion. It expresses an assignment conditioned on
    /// predecessor blocks.
    Phi {
        dest: VariableRef,
        operands: SmallVec<[(BasicBlockIdx, VariableRef); 2]>,
    },

    /// No operation
    Nop,
}

/// A statement has a kind and a source location. The source location is used for results
/// reporting.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Statement {
    pub source_info: SourceInfo,
    pub kind: StatementKind,
}

/// Source info attached to specific elements of the IR
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(transparent)]
pub struct SourceInfo {
    pub span_id: source_info::FileSpanId,
}

/// A variable name or parameter reference
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Variable {
    /// A global variable, identified by its index into the program's [`Globals`] table. It may
    /// only be written by a store, an assignment with a non-empty destination path (G4).
    Global(GlobalIdx),
    /// A local variable, identified by its index into the enclosing function's [`Locals`] table.
    Local(LocalIdx),
    /// A parameter
    Param(ParameterIdx),
}

/// A reference to a variable, possibly with version. Versions are computed by SSA conversion.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VariableRef {
    /// The referenced variable.
    pub variable: Variable,
    pub version: Option<u32>,
}

/// An **access path**: a (possibly versioned) variable followed by a sequence of offsets and
/// symbolic fields, in any order (e.g. `x.[8].deref.f`). An empty sequence names the variable
/// itself.
///
/// This type defines the term: everywhere else in ctadl, "access path" means a variable plus a
/// [`PathSegment`] sequence, and a "path" is the sequence alone. Access paths are the IR's
/// operands: an assignment's destination and sources, call arguments, an indirect call's callee.
/// [`AccessPath::new`] puts the path in normal form ([`path::normalize`]); [`CirVerify`] checks
/// it, since a deserialized program bypasses the constructor.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AccessPath {
    pub base: VariableRef,
    pub segments: ThinVec<PathSegment>,
}

/// Expressions: an operand of an assignment, a call or a return.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Exp {
    /// An access path, read as a whole. An empty path names the variable, and prints as it.
    Path(AccessPath),
    /// The value of an access path, not its identity: `t = value(p)` lowers to `t.#value <- p`.
    /// The destination's value depends on `p`'s, but nothing reachable through the destination
    /// is reachable through `p`. Integer arithmetic uses it for operands that only supply a
    /// number, so `t = a + i` relates `t` to the memory behind `a`, the base, and not to the
    /// memory behind `i`. See [`PathSegment::Value`].
    ///
    /// Valid only as a source of [`StatementKind::Assign`].
    Value(AccessPath),
    /// A function or class reference: a target label (S11).
    ObjectRef(CallObject),
    /// A constant string: a string label (S12).
    Str(ArcIntern<str>),
    /// An integer constant, sign-extended to `i64`. Carries nothing.
    Int(i64),
    /// A block of bytes. It has no value as a number, and carries nothing.
    Bytes(Vec<u8>),
}

/// A sequence of statements ending with a terminator.
///
/// The terminator says where control goes when the block finishes, so a block that does not
/// have one is not a block anybody can run. It is optional here only to make blocks easier to
/// build: a front end usually knows the statements of a block before it knows the successors,
/// and this lets it fill the terminator in later.
///
/// A missing terminator is therefore a block that is still under construction, not a block in
/// its finished form. Anything that walks the control-flow graph expects the finished form.
/// [`FunctionData::verify`] reports a block without a terminator as
/// [`VerifyError::NoTerminator`], and [`crate::ssa::transform`] is the point where the
/// unfinished form stops being allowed at all.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BasicBlockData {
    pub statements: IndexVecDeque<StatementIdx, Statement>,
    /// Where control goes when the block finishes. `None` means the block is still being
    /// built. See the note on [`BasicBlockData`].
    pub terminator: Option<Terminator>,
}

/// A function consists of a name, sequence of parameters, and CFG of basic blocks.
///
/// Functions take parameters, each with a [`ParameterType`], and can return a tuple of values.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FunctionData {
    /// The name of the function.
    pub name: String,
    /// Parameter info.
    pub params: Params,
    pub return_type: ReturnType,
    /// List of basic blocks of the function. It is allowed to be empty, meaning the function has
    /// no code.
    pub blocks: BasicBlocks,
    /// Declaration table for the function's local variables. Every [`Variable::Local`] reachable in
    /// this function indexes into this table.
    pub locals: Locals,
}

/// Parameter declarations for a function. Parameter passing matches the declaration order
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Params {
    pub parameters: IndexVec<ParameterIdx, ParameterType>,
}

// Index into parameter in `Params`
newtype_index!(ParameterIdx, u32);

impl From<u16> for ParameterIdx {
    fn from(v: u16) -> Self {
        ParameterIdx::new(v.into())
    }
}

// Index into a local declaration in a function's `Locals` table.
newtype_index!(LocalIdx, u32);

// Index into a global declaration in a program's `Globals` table.
newtype_index!(GlobalIdx, u32);

/// The declaration of a global variable: its name, which is its identity across the programs of a
/// project (G3), and where the source declares it, if the front end knows.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GlobalData {
    pub name: Symbol,
    pub span: Option<source_info::FileSpanId>,
}

/// A program's table of global declarations, interned by name: declaring a name twice returns the
/// first declaration's index, and the first span given wins.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Globals {
    decls: IndexVec<GlobalIdx, GlobalData>,
    /// Transient name→index map; rebuilt on demand after deserialize.
    #[cfg_attr(feature = "serde", serde(skip))]
    by_name: HashMap<String, GlobalIdx>,
}

/// The declaration of a single local variable. Currently only holds the local's source name, but
/// has room to grow (type, source info, is_temp, …).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LocalDecl {
    pub name: String,
}

/// A per-function table of local variable declarations. Locals are interned by name via
/// [`Locals::get_or_intern`]: a repeated name within a function returns the same [`LocalIdx`], so
/// every occurrence of a source register maps to one local (required for SSA).
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Locals {
    decls: IndexVec<LocalIdx, LocalDecl>,
    /// Transient name→index map used only during interning; rebuilt on demand after deserialize.
    /// It holds indices into `decls` and hashes the names stored there, rather than a second
    /// copy of every name: a program holds one table per function, and the copies were a
    /// sizable share of an import's memory.
    #[cfg_attr(feature = "serde", serde(skip))]
    by_name: hashbrown::HashTable<LocalIdx>,
    #[cfg_attr(feature = "serde", serde(skip))]
    hasher: hashbrown::DefaultHashBuilder,
}

impl Locals {
    /// Returns the index for `name`, interning a new local if this name has not been seen.
    pub fn get_or_intern(&mut self, name: &str) -> LocalIdx {
        // `by_name` is `#[serde(skip)]`, so a deserialized table arrives with declarations but no
        // index. Rebuild it before interning: otherwise a name already in `decls` would intern to
        // a *second* index, and the one-local-per-name invariant that SSA (and every name-based
        // lookup into this table) relies on would silently break.
        if self.by_name.is_empty() && !self.decls.is_empty() {
            self.rebuild_index();
        }
        let Locals {
            decls,
            by_name,
            hasher,
        } = self;
        let hash = hasher.hash_one(name);
        if let Some(&idx) = by_name.find(hash, |&i| decls[i].name == name) {
            return idx;
        }
        let idx = decls.push(LocalDecl {
            name: name.to_string(),
        });
        by_name.insert_unique(hash, idx, |&i| hasher.hash_one(decls[i].name.as_str()));
        idx
    }

    /// [`Self::get_or_intern`] for the name `prefix` followed by `n` in decimal (`v3`, `stack12`),
    /// built on the stack. Front ends look such a name up for every operand of every
    /// instruction, and formatting each into a `String` of its own only to find it already
    /// interned was an allocation per operand.
    pub fn get_or_intern_numbered(&mut self, prefix: &str, n: u32) -> LocalIdx {
        use std::fmt::Write;
        let mut name = StackName {
            buf: [0; 32],
            len: 0,
        };
        match write!(name, "{prefix}{n}") {
            Ok(()) => self.get_or_intern(name.as_str()),
            Err(_) => self.get_or_intern(&format!("{prefix}{n}")),
        }
    }

    /// The source name of the local at `i`.
    #[inline]
    pub fn name(&self, i: LocalIdx) -> &str {
        &self.decls[i].name
    }

    /// The declaration of the local at `i`, if it exists.
    #[inline]
    pub fn get(&self, i: LocalIdx) -> Option<&LocalDecl> {
        self.decls.get(i)
    }

    /// The number of declared locals.
    #[inline]
    pub fn len(&self) -> usize {
        self.decls.len()
    }

    /// Whether the table has no declared locals.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    /// Iterate over `(LocalIdx, &LocalDecl)` pairs.
    #[inline]
    pub fn iter_enumerated(&self) -> impl DoubleEndedIterator<Item = (LocalIdx, &LocalDecl)> {
        self.decls.iter_enumerated()
    }

    /// Rebuild the transient `by_name` index from the declarations. [`Self::get_or_intern`] calls
    /// this itself when it finds an unindexed table (the map is `#[serde(skip)]`, so a
    /// deserialized table has none), so callers only need it to pre-warm the map.
    pub fn rebuild_index(&mut self) {
        let Locals {
            decls,
            by_name,
            hasher,
        } = self;
        by_name.clear();
        for (idx, decl) in decls.iter_enumerated() {
            let hash = hasher.hash_one(decl.name.as_str());
            // The first declaration of a name wins, as interning would have given it.
            if by_name
                .find(hash, |&i| decls[i].name == decl.name)
                .is_none()
            {
                by_name.insert_unique(hash, idx, |&i| hasher.hash_one(decls[i].name.as_str()));
            }
        }
    }

    /// Frees the name index. A finished function is only read, and [`Self::get_or_intern`]
    /// rebuilds the index if the table is ever extended again.
    pub fn drop_index(&mut self) {
        self.by_name = Default::default();
    }
}

/// A short name written into a fixed buffer; see [`Locals::get_or_intern_numbered`].
struct StackName {
    buf: [u8; 32],
    len: usize,
}

impl StackName {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).expect("written from strs")
    }
}

impl std::fmt::Write for StackName {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        self.buf
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Globals {
    /// Returns the index for `name`, declaring a new global if this name has not been seen. A
    /// repeated declaration keeps the first span given.
    pub fn declare(
        &mut self,
        name: impl AsRef<str>,
        span: Option<source_info::FileSpanId>,
    ) -> GlobalIdx {
        if self.by_name.len() != self.decls.len() {
            self.rebuild_index();
        }
        let name = name.as_ref();
        if let Some(&idx) = self.by_name.get(name) {
            if self.decls[idx].span.is_none() {
                self.decls[idx].span = span;
            }
            return idx;
        }
        let name: Symbol = ArcIntern::from(name);
        let idx = self.decls.push(GlobalData {
            name: name.clone(),
            span,
        });
        self.by_name.insert(name.to_string(), idx);
        idx
    }

    /// The global declared under `name`, if any.
    pub fn lookup(&self, name: &str) -> Option<GlobalIdx> {
        if self.by_name.len() == self.decls.len() {
            self.by_name.get(name).copied()
        } else {
            self.decls
                .iter_enumerated()
                .find(|(_, d)| &*d.name == name)
                .map(|(i, _)| i)
        }
    }

    /// The declaration of global `i`, if it exists.
    #[inline]
    pub fn get(&self, i: GlobalIdx) -> Option<&GlobalData> {
        self.decls.get(i)
    }

    /// The number of declared globals.
    #[inline]
    pub fn len(&self) -> usize {
        self.decls.len()
    }

    /// Whether the program declares no globals.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    /// Iterate over `(GlobalIdx, &GlobalData)` pairs.
    #[inline]
    pub fn iter_enumerated(&self) -> impl DoubleEndedIterator<Item = (GlobalIdx, &GlobalData)> {
        self.decls.iter_enumerated()
    }

    /// Rebuild the transient `by_name` index from the declarations.
    pub fn rebuild_index(&mut self) {
        self.by_name.clear();
        for (idx, decl) in self.decls.iter_enumerated() {
            self.by_name.entry(decl.name.to_string()).or_insert(idx);
        }
    }
}

impl Index<GlobalIdx> for Globals {
    type Output = GlobalData;
    #[inline]
    fn index(&self, index: GlobalIdx) -> &Self::Output {
        &self.decls[index]
    }
}

/// Renders CIR with [`Variable::Global`]s resolved to their names in `program`'s table, for
/// rendering a part of a program (a function, a statement, an access path) on its own.
/// [`Display for Program`](Program#impl-Display-for-Program) does this itself.
pub struct WithGlobalNames<'a, T>(pub &'a Program, pub T);

impl<T: Display> Display for WithGlobalNames<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _guard = global_names::push(&self.0.globals);
        write!(f, "{}", self.1)
    }
}

impl Program {
    /// Publishes this program's global names on the current thread until the guard drops, so
    /// that every `Display` of a [`Variable::Global`] in between prints `$name` rather than its
    /// index. For callers that render many pieces of one program.
    pub fn global_names_guard(&self) -> GlobalNamesGuard {
        global_names::push(&self.globals)
    }
}

/// Keeps a program's global names published for printing; see [`Program::global_names_guard`].
pub struct GlobalNamesGuard(());

impl Drop for GlobalNamesGuard {
    fn drop(&mut self) {
        global_names::pop();
    }
}

/// Scoped, thread-local plumbing for printing a global as its name:
/// [`Program::global_names_guard`], [`WithGlobalNames`] and `Display for Program` publish a
/// program's table, `Display for Variable` reads it back.
mod global_names {
    use std::cell::RefCell;

    use super::{GlobalIdx, GlobalNamesGuard, Globals, Symbol};
    use crate::index::idx::Idx;

    thread_local! {
        static FRAMES: RefCell<Vec<Vec<Symbol>>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn push(globals: &Globals) -> GlobalNamesGuard {
        let names = globals
            .iter_enumerated()
            .map(|(_, d)| d.name.clone())
            .collect();
        FRAMES.with_borrow_mut(|frames| frames.push(names));
        GlobalNamesGuard(())
    }

    pub(super) fn pop() {
        FRAMES.with_borrow_mut(|frames| {
            frames.pop();
        });
    }

    /// The name of `idx` in the innermost published table, if there is one.
    pub(super) fn name_of(idx: GlobalIdx) -> Option<Symbol> {
        FRAMES.with_borrow(|frames| frames.last()?.get(idx.index()).cloned())
    }
}

/// Renders CIR with [`Variable::Local`]s resolved to their source names.
///
/// [`Display for Variable`](Variable#impl-Display-for-Variable) has no access to the enclosing
/// function, so by default a local prints as its opaque index (`%L7`) — the same form the fact
/// base and graphviz labels use. Wrapping a value in `WithLocalNames` turns on name resolution for
/// the duration of that render: every [`FunctionData`] publishes its [`Locals`] table, locals
/// inside it print as `%name`, and the function header gains a `locals:` line giving the
/// index↔name mapping (so a dump can still be correlated with `%L7_2`-style graph vertices).
///
/// ```text
/// define f(@p0[shared_fields]) -> 1:
///   locals: %L0=buf %L1=t0?
///   bb0:
///     assign %buf = @p0
/// ```
pub struct WithLocalNames<T>(pub T);

impl<T: Display> Display for WithLocalNames<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _guard = local_names::enable();
        write!(f, "{}", self.0)
    }
}

/// Scoped, thread-local plumbing for [`WithLocalNames`]: `Display for FunctionData` publishes the
/// table it is rendering and `Display for Variable` reads it back. Inert unless a `WithLocalNames`
/// render is in progress on this thread, so ordinary `{}` output — including the `format!("{v}")`
/// that gives a local its identity in the fact base — is unchanged.
mod local_names {
    use std::cell::{Cell, RefCell};

    use super::{LocalIdx, Locals};
    use crate::index::idx::Idx;

    thread_local! {
        static ENABLED: Cell<bool> = const { Cell::new(false) };
        /// One entry per enclosing `FunctionData` render; the innermost is the one in effect.
        static FRAMES: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
    }

    /// Restores the previous setting, so nesting a plain render inside a named one (or vice
    /// versa) behaves.
    pub(super) struct EnableGuard(bool);

    impl Drop for EnableGuard {
        fn drop(&mut self) {
            ENABLED.set(self.0);
        }
    }

    pub(super) fn enable() -> EnableGuard {
        EnableGuard(ENABLED.replace(true))
    }

    pub(super) fn is_enabled() -> bool {
        ENABLED.get()
    }

    pub(super) struct FrameGuard;

    impl Drop for FrameGuard {
        fn drop(&mut self) {
            FRAMES.with_borrow_mut(|frames| {
                frames.pop();
            });
        }
    }

    /// Publishes `locals` for the nested render. Returns `None` — allocating nothing — when name
    /// resolution is off.
    pub(super) fn push(locals: &Locals) -> Option<FrameGuard> {
        if !is_enabled() {
            return None;
        }
        let names = locals
            .iter_enumerated()
            .map(|(_, decl)| decl.name.clone())
            .collect();
        FRAMES.with_borrow_mut(|frames| frames.push(names));
        Some(FrameGuard)
    }

    /// The source name of `idx` in the innermost published table, if there is one.
    pub(super) fn name_of(idx: LocalIdx) -> Option<String> {
        if !is_enabled() {
            return None;
        }
        FRAMES.with_borrow(|frames| frames.last()?.get(idx.index()).cloned())
    }
}

/// Which of a parameter's access paths the caller shares with the callee: the outputs, where
/// data the callee flows into the parameter reaches the caller.
///
/// This is about what the parameter's paths denote, not about a calling convention. Java passes
/// every argument by value, yet an object parameter is [`Shared`](Self::Shared): its root, the
/// empty path, stands for the object's contents, which the caller sees.
///
/// Rebinding a parameter -- any instruction that defines the parameter variable, such as
/// `p = q` -- never reaches the caller, whatever the parameter's type: SSA conversion writes
/// every parameter back at its entry version, version 0 (see [`crate::ssa::transform`]). What
/// reaches the caller is what flows into that entry version, which is a write to one of its
/// fields (`store p.f := v`) or, at a call, a callee's write to the argument it was passed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParameterType {
    /// Nothing is shared: the callee's parameter is its own copy, fields and all, so no path is
    /// an output. Right for a C struct passed by value.
    Copied,
    /// The root is the callee's own copy, and what it reaches is shared: every path but the
    /// empty one is an output. A flow into the whole value does not reach the caller.
    #[default]
    SharedFields,
    /// Every path is shared, the root included, so a flow into the whole value reaches the
    /// caller. Right for a handle whose root stands for contents the caller sees, such as a Java
    /// object reference or a C pointer: a callee that writes the object it was passed
    /// (`sb.append(x)`) writes the caller's.
    Shared,
}

/// Function's return type. This is simply the arity of returns
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ReturnType {
    /// Number of returned values
    pub arity: u8,
}

/// Set of functions in a program.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Functions {
    pub functions: IndexVec<FunctionIdx, FunctionData>,
}

/// A location denotes the start of a statement; or, if `statement_index` equals the number of
/// statements, then the start of the terminator.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Location {
    pub function: FunctionIdx,
    pub block: BasicBlockIdx,
    pub statement_index: usize,
}

/// An IR program capable of representing multiple functions and internal and external calls.
///
/// Well-formed programs must satisfy certain invariants. After generating a program, you must call
/// [`Program::verify`] to ensure the invariants are satisfied.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Program {
    /// Set of functions
    pub functions: Functions,
    /// The program's global variables. Every [`Variable::Global`] in the program indexes into
    /// this table.
    pub globals: Globals,
}

/// Program together with all metadata that enables CTADL analysis. This is the target datatype for
/// frontend languages.
#[derive(Debug, Default)]
pub struct ProgramInfo {
    /// Program in CTADL IR
    pub program: Program,
    /// Virtual method information
    pub vmt: VirtualMethodTable,
    /// Database of all source information allowing correlation between instructions and original
    /// artifact locations
    pub source_info: source_info::SourceInfo,
}

/// A [`ProgramInfo`] under construction, for a frontend to fill in.
#[derive(Debug)]
pub struct ProgramInfoBuilder {
    pub program: Program,
    pub vmt: VirtualMethodTable,
    pub source_info_builder: source_info::SourceInfoBuilder,
}

impl ProgramInfoBuilder {
    /// An empty program with no source information, whose methods `vmt` will describe.
    pub fn new(vmt: VirtualMethodTable) -> Self {
        Self {
            program: Program::default(),
            vmt,
            source_info_builder: source_info::SourceInfoBuilder::new(
                source_info::ArtifactMetadata::new(),
            ),
        }
    }
}

impl SourceInfo {
    pub fn new(span_id: source_info::FileSpanId) -> Self {
        Self { span_id }
    }
}

impl Default for SourceInfo {
    fn default() -> Self {
        SourceInfo {
            span_id: source_info::NO_SPAN,
        }
    }
}

impl Variable {
    #[inline]
    pub fn new_local(idx: LocalIdx) -> Self {
        Variable::Local(idx)
    }

    #[inline]
    pub fn new_parameter(param: ParameterIdx) -> Self {
        Variable::Param(param)
    }

    #[inline]
    pub fn global(&self) -> Option<GlobalIdx> {
        match self {
            Variable::Global(idx) => Some(*idx),
            _ => None,
        }
    }

    #[inline]
    pub fn is_global(&self) -> bool {
        matches!(self, Variable::Global(_))
    }

    #[inline]
    pub fn local(&self) -> Option<LocalIdx> {
        match self {
            Variable::Local(idx) => Some(*idx),
            _ => None,
        }
    }
}

impl VariableRef {
    /// Creates a reference to the variable with no version
    pub fn new(variable: Variable) -> Self {
        VariableRef {
            variable,
            version: None,
        }
    }

    /// Creates a local reference with no version from a local index
    #[inline]
    pub fn new_local_idx(idx: LocalIdx) -> Self {
        VariableRef {
            variable: Variable::new_local(idx),
            version: None,
        }
    }

    /// Creates a reference to a global. A global is never versioned (G4).
    #[inline]
    pub fn global(idx: GlobalIdx) -> Self {
        VariableRef {
            variable: Variable::Global(idx),
            version: None,
        }
    }

    /// Creates a parameter reference with no version
    #[inline]
    pub fn new_parameter(param: ParameterIdx) -> Self {
        VariableRef {
            variable: Variable::Param(param),
            version: None,
        }
    }

    /// The same variable, with the given version
    #[inline]
    pub fn with_version(&self, version: u32) -> Self {
        let variable = self.variable;
        let version = Some(version);
        VariableRef { variable, version }
    }
}

impl AccessPath {
    /// Creates an access path from a base variable and its path, in normal form
    /// ([`path::normalize`]).
    #[inline]
    pub fn new(base: VariableRef, segments: impl IntoIterator<Item = PathSegment>) -> Self {
        Self {
            base,
            segments: path::normalize(segments).into_iter().collect(),
        }
    }

    /// The classification of this access path's path ([`path::classify`]).
    #[inline]
    pub fn class(&self) -> path::PathClass {
        path::classify(&self.segments)
    }

    /// True when the access path names its base variable itself (its path is empty).
    #[inline]
    pub fn is_pathless(&self) -> bool {
        self.segments.is_empty()
    }

    /// Appends `seg`, keeping the path in normal form. A front end that builds a path a segment
    /// at a time uses this rather than pushing onto `segments`, which would not normalize.
    pub fn push(&mut self, seg: PathSegment) {
        self.extend([seg]);
    }

    /// Appends `segs`, keeping the path in normal form.
    pub fn extend(&mut self, segs: impl IntoIterator<Item = PathSegment>) {
        let mut out: Vec<PathSegment> = std::mem::take(&mut self.segments).into_iter().collect();
        for seg in segs {
            path::push_normalized(&mut out, seg);
        }
        self.segments = out.into_iter().collect();
    }
}

impl From<VariableRef> for AccessPath {
    #[inline]
    fn from(base: VariableRef) -> Self {
        AccessPath::new(base, [])
    }
}

/// Renders the base variable followed by its path in the canonical grammar ([`path_syntax`]).
impl Display for AccessPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.base, path_to_string(&self.segments))
    }
}

impl From<ParameterIdx> for Variable {
    fn from(idx: ParameterIdx) -> Self {
        Variable::Param(idx)
    }
}

impl Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Only a `WithLocalNames` render can resolve the index; everywhere else a local is
            // its index, which is what the fact base keys on.
            Variable::Local(idx) => match local_names::name_of(*idx) {
                Some(name) => write!(f, "%{name}"),
                None => write!(f, "%L{}", idx.index()),
            },
            Variable::Param(i) => write!(f, "@p{}", i.index()),
            // `$name` while a program's global names are published (see `global_names`);
            // otherwise its index.
            Variable::Global(idx) => match global_names::name_of(*idx) {
                Some(name) => {
                    write!(f, "$")?;
                    path_syntax::write_symbol(f, &name)
                }
                None => write!(f, "$#{}", idx.index()),
            },
        }
    }
}

impl Display for VariableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let VariableRef { variable, version } = self;
        write!(f, "{variable}")?;
        if let Some(version) = version {
            write!(f, "_{version}")?;
        }
        Ok(())
    }
}

impl Exp {
    /// The whole variable `v`, an access path with the empty path.
    #[inline]
    pub fn variable(v: VariableRef) -> Self {
        Self::Path(AccessPath::from(v))
    }

    /// The access path `ap`.
    #[inline]
    pub fn path(ap: impl Into<AccessPath>) -> Self {
        Self::Path(ap.into())
    }

    /// [`Exp::Value`] of `ap`.
    #[inline]
    pub fn value(ap: impl Into<AccessPath>) -> Self {
        Self::Value(ap.into())
    }

    /// The access path this expression reads, for [`Exp::Path`] and [`Exp::Value`].
    #[inline]
    pub fn access_path(&self) -> Option<&AccessPath> {
        match self {
            Exp::Path(ap) | Exp::Value(ap) => Some(ap),
            _ => None,
        }
    }

    /// Mutable counterpart of [`Exp::access_path`].
    #[inline]
    pub fn access_path_mut(&mut self) -> Option<&mut AccessPath> {
        match self {
            Exp::Path(ap) | Exp::Value(ap) => Some(ap),
            _ => None,
        }
    }

    /// Returns the base variable read by this expression, if it reads one ([`Exp::Path`] or
    /// [`Exp::Value`]). Constants return `None`.
    #[inline]
    pub fn base_variable(&self) -> Option<&VariableRef> {
        self.access_path().map(|ap| &ap.base)
    }

    /// Mutable counterpart of [`Exp::base_variable`].
    #[inline]
    pub fn base_variable_mut(&mut self) -> Option<&mut VariableRef> {
        self.access_path_mut().map(|ap| &mut ap.base)
    }

    #[inline]
    pub fn new_bytes(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }

    /// Makes an integer constant. Pass the value the front end worked out, sign-extended to
    /// `i64`. Do not pass the bytes the opcode encoded it in. See [`Exp::Int`].
    #[inline]
    pub fn new_int(value: i64) -> Self {
        Self::Int(value)
    }

    /// Returns the value of an integer constant, or `None` for anything else. It does not
    /// decode an [`Exp::Bytes`], on purpose. A block of bytes does not say how wide the number
    /// is or whether it is signed, which is why [`Exp::Int`] exists.
    #[inline]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Exp::Int(value) => Some(*value),
            _ => None,
        }
    }

    #[inline]
    pub fn new_str(s: &str) -> Self {
        Self::Str(ArcIntern::from(s))
    }

    /// Returns the variable this expression reads, if it is a whole variable: an
    /// [`Exp::Path`] with the empty path.
    #[inline]
    pub fn variable_ref(&self) -> Option<&VariableRef> {
        match self {
            Exp::Path(ap) if ap.is_pathless() => Some(&ap.base),
            _ => None,
        }
    }

    #[inline]
    pub fn str(&self) -> Option<&ArcIntern<str>> {
        match self {
            Exp::Str(s) => Some(s),
            _ => None,
        }
    }

    #[inline]
    pub fn new_object_ref(obj: CallObject) -> Self {
        Self::ObjectRef(obj)
    }

    #[inline]
    pub fn object_ref(&self) -> Option<&CallObject> {
        match self {
            Exp::ObjectRef(obj) => Some(obj),
            _ => None,
        }
    }
}

impl BasicBlockData {
    pub fn new(terminator: Option<Terminator>) -> Self {
        Self {
            statements: Default::default(),
            terminator,
        }
    }

    pub fn new_stmts(
        statements: IndexVecDeque<StatementIdx, Statement>,
        terminator: Option<Terminator>,
    ) -> Self {
        Self {
            statements,
            terminator,
        }
    }

    /// Returns the terminator of a finished block.
    ///
    /// Use this when the block is supposed to be finished already, which is the case anywhere
    /// the control-flow graph is being read. Use [`BasicBlockData::terminator_opt`] when the
    /// block may still be under construction.
    ///
    /// # Panics
    ///
    /// If there is no terminator.
    #[inline]
    pub fn terminator(&self) -> &Terminator {
        self.terminator.as_ref().expect("no terminator")
    }

    /// Returns the terminator of a finished block, for modification.
    ///
    /// # Panics
    ///
    /// If there is no terminator.
    #[inline]
    pub fn terminator_mut(&mut self) -> &mut Terminator {
        self.terminator.as_mut().expect("no terminator")
    }

    /// Returns the terminator, or `None` if the block does not have one yet. This is the
    /// accessor to use while a block is still being built.
    #[inline]
    pub fn terminator_opt(&self) -> Option<&Terminator> {
        self.terminator.as_ref()
    }

    #[inline]
    pub fn successors(&self) -> impl DoubleEndedIterator<Item = BasicBlockIdx> + '_ {
        self.terminator().successors()
    }
}

impl Deref for BasicBlockData {
    type Target = IndexVecDeque<StatementIdx, Statement>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.statements
    }
}

impl DerefMut for BasicBlockData {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.statements
    }
}

impl Index<StatementIdx> for BasicBlockData {
    type Output = Statement;
    #[inline]
    fn index(&self, index: StatementIdx) -> &Self::Output {
        &self.statements[index]
    }
}

impl IndexMut<StatementIdx> for BasicBlockData {
    #[inline]
    fn index_mut(&mut self, index: StatementIdx) -> &mut Self::Output {
        &mut self.statements[index]
    }
}

impl Display for BasicBlockData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.statements {
            writeln!(f, "{s}")?;
        }
        if let Some(terminator) = &self.terminator {
            writeln!(f, "{terminator}")?;
        } else {
            writeln!(f, "<no terminator>")?;
        }
        Ok(())
    }
}

pub type VarIter<'s> = Box<dyn DoubleEndedIterator<Item = &'s VariableRef> + 's>;
pub type VarIterMut<'s> = Box<dyn DoubleEndedIterator<Item = &'s mut VariableRef> + 's>;

impl Statement {
    pub fn new(kind: StatementKind, source_info: SourceInfo) -> Self {
        Self { source_info, kind }
    }

    /// Creates a new statement with default source info (none)
    pub fn new_kind(kind: StatementKind) -> Self {
        Self {
            source_info: Default::default(),
            kind,
        }
    }

    #[inline]
    pub fn iter_dst_var<'s>(&'s self) -> VarIter<'s> {
        self.kind.iter_dst_var()
    }

    #[inline]
    pub fn iter_dst_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        self.kind.iter_dst_var_mut()
    }

    #[inline]
    pub fn iter_src_var<'s>(&'s self) -> VarIter<'s> {
        self.kind.iter_src_var()
    }

    #[inline]
    pub fn iter_src_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        self.kind.iter_src_var_mut()
    }
}

impl StatementKind {
    /// Generates an assign from a destination and sources. Use this to flow multiple sources into
    /// a single destination, i.e., when modeling a statement like "x = y + z" where you want "y"
    /// and "z" to flow into "x."
    ///
    /// An address destination `p.[k] = e` is rewritten to `p = e.[-k]` (S5): the result's
    /// destination is never an address.
    #[inline]
    pub fn assign<I>(dest: impl Into<AccessPath>, srcs: I) -> Self
    where
        I: IntoIterator<Item = Exp>,
    {
        let dest = dest.into();
        let sources: SmallVec<[Exp; 2]> = srcs.into_iter().collect();
        let (dest, sources) = rewrite_address_dest(dest, sources);
        StatementKind::Assign { dest, sources }
    }

    /// The copy `dest = source.segment`, a load.
    pub fn load(
        dest: VariableRef,
        source: impl Into<AccessPath>,
        segment: impl Into<PathSegment>,
    ) -> Self {
        let mut source = source.into();
        let segments = path::concat(&source.segments, &[segment.into()]);
        source.segments = segments.into_iter().collect();
        Self::assign(dest, [Exp::Path(source)])
    }

    /// The store `dest.segment = value`.
    pub fn store(dest: impl Into<AccessPath>, segment: impl Into<PathSegment>, value: Exp) -> Self {
        let mut dest = dest.into();
        let segments = path::concat(&dest.segments, &[segment.into()]);
        dest.segments = segments.into_iter().collect();
        Self::assign(dest, [value])
    }

    /// True for an assignment whose destination path is non-empty: it writes memory and defines
    /// no variable (S5). False for every other statement.
    #[inline]
    pub fn is_store(&self) -> bool {
        matches!(self, StatementKind::Assign { dest, .. } if !dest.is_pathless())
    }

    /// Returns an iterator over variables read by this statement. A store reads its
    /// destination's base.
    pub fn iter_src_var<'s>(&'s self) -> VarIter<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, sources } => {
                let a: VarIter<'s> = if dest.is_pathless() {
                    Box::new(std::iter::empty())
                } else {
                    Box::new(std::iter::once(&dest.base))
                };
                Box::new(a.chain(sources.iter().filter_map(Exp::base_variable)))
            }
            CallAssign { args, style, .. } => {
                let a: VarIter<'s> = Box::new(args.iter().filter_map(Exp::base_variable));
                let b: VarIter<'s> = match style.receiver() {
                    Some(r) => Box::new(std::iter::once(r)),
                    None => Box::new(std::iter::empty()),
                };
                Box::new(a.chain(b))
            }
            Phi { operands, .. } => Box::new(operands.iter().map(|(_, v)| v)),
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over mutable variables referenced by this statement.
    pub fn iter_src_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, sources } => {
                let a: VarIterMut<'s> = if dest.is_pathless() {
                    Box::new(std::iter::empty())
                } else {
                    Box::new(std::iter::once(&mut dest.base))
                };
                Box::new(a.chain(sources.iter_mut().filter_map(Exp::base_variable_mut)))
            }
            CallAssign { args, style, .. } => {
                let a: VarIterMut<'s> =
                    Box::new(args.iter_mut().filter_map(Exp::base_variable_mut));
                let b: VarIterMut<'s> = match style.receiver_mut() {
                    Some(r) => Box::new(std::iter::once(r)),
                    None => Box::new(std::iter::empty()),
                };
                Box::new(a.chain(b))
            }
            Phi { operands, .. } => Box::new(operands.iter_mut().map(|(_, v)| v)),
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over variables set by this statement. A copy defines its
    /// destination's base; a store defines nothing.
    pub fn iter_dst_var<'s>(&'s self) -> VarIter<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, .. } if dest.is_pathless() => Box::new(std::iter::once(&dest.base)),
            Assign { .. } => Box::new(std::iter::empty()),
            CallAssign { rets, .. } => Box::new(rets.iter()),
            Phi { dest, .. } => Box::new(std::iter::once(dest)),
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over variables set by this statement.
    pub fn iter_dst_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, .. } if dest.is_pathless() => Box::new(std::iter::once(&mut dest.base)),
            Assign { .. } => Box::new(std::iter::empty()),
            CallAssign { rets, .. } => Box::new(rets.iter_mut()),
            Phi { dest: out, .. } => Box::new(std::iter::once(out)),
            Nop => Box::new(std::iter::empty()),
        }
    }
}

/// Reads the memory part of `ap` into a fresh temporary and returns the temporary followed by
/// `ap`'s trailing offsets: `a.f.g.[8]` appends `t = a.f.g` to `out` and returns `t.[8]`. An
/// address with no memory part (`a.[8]`, or `a` itself) is returned unchanged and appends
/// nothing.
///
/// This is for a front end that wants an operand that is a variable plus offsets, such as a
/// pointer it goes on to offset. Nothing in the IR requires it: an operand may be any access
/// path.
pub fn read_into_temp(
    ap: AccessPath,
    out: &mut Vec<Statement>,
    fresh: impl FnOnce() -> VariableRef,
) -> AccessPath {
    let Some(i) = ap.segments.iter().rposition(|s| !s.is_offset()) else {
        return ap;
    };
    let t = fresh();
    let AccessPath { base, segments } = ap;
    out.push(Statement::new_kind(StatementKind::assign(
        t.clone(),
        [Exp::Path(AccessPath::new(
            base,
            segments[..=i].iter().cloned(),
        ))],
    )));
    AccessPath::new(t, segments[i + 1..].iter().cloned())
}

/// The S5 rewrite of an address destination: `p.[k] = e` is `p = e.[-k]`. A source access path
/// gains the offset `[-k]`; a value source and a constant are unchanged, since
/// `p.[k].#value = p.#value` and an address's labels are its base's (S3).
fn rewrite_address_dest(
    dest: AccessPath,
    mut sources: SmallVec<[Exp; 2]>,
) -> (AccessPath, SmallVec<[Exp; 2]>) {
    let AccessPath { base, segments } = dest;
    let segments = path::normalize(segments);
    let Some(PathSegment::Offset(_)) = segments.last() else {
        return (
            AccessPath {
                base,
                segments: segments.into_iter().collect(),
            },
            sources,
        );
    };
    let (dest_segments, _) = path::rewrite_address_dest(&segments, &[]);
    for src in &mut sources {
        if let Exp::Path(ap) = src {
            let (_, src_segments) = path::rewrite_address_dest(&segments, &ap.segments);
            ap.segments = src_segments.into_iter().collect();
        }
    }
    (
        AccessPath {
            base,
            segments: dest_segments.into_iter().collect(),
        },
        sources,
    )
}

impl Params {
    pub fn new<P: Into<IndexVec<ParameterIdx, ParameterType>>>(parameters: P) -> Self {
        Self {
            parameters: parameters.into(),
        }
    }

    #[inline]
    pub fn last_index(&self) -> Option<ParameterIdx> {
        self.parameters.last_index()
    }

    #[inline]
    pub fn iter_params(&self) -> impl Iterator<Item = &ParameterType> {
        self.parameters.iter()
    }
}

impl FunctionData {
    pub fn new(name: &str, params: Params, blocks: BasicBlocks, return_type: ReturnType) -> Self {
        Self {
            name: name.to_string(),
            params,
            blocks,
            locals: Locals::default(),
            return_type,
        }
    }

    #[inline]
    pub fn num_parameters(&self) -> usize {
        self.params.parameters.len()
    }

    /// Interns a local by name into this function's [`Locals`] table, returning its index. Repeated
    /// names return the same index.
    #[inline]
    pub fn intern_local(&mut self, name: &str) -> LocalIdx {
        self.locals.get_or_intern(name)
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub fn set_return_type(&mut self, return_type: ReturnType) {
        self.return_type = return_type;
    }

    /// Verify that the function satisfies expected invariants. See the description of each error
    /// in the [`VerifyError`] enum.
    pub fn verify(&self) -> Result<(), VerifyErrors> {
        let mut visitor = CirVerify::default();
        visitor.visit_function_data(FunctionIdx::new(0), self);
        visitor.take_error()
    }
}

impl Index<BasicBlockIdx> for FunctionData {
    type Output = BasicBlockData;
    #[inline]
    fn index(&self, index: BasicBlockIdx) -> &Self::Output {
        &self.blocks[index]
    }
}

impl IndexMut<BasicBlockIdx> for FunctionData {
    #[inline]
    fn index_mut(&mut self, index: BasicBlockIdx) -> &mut Self::Output {
        &mut self.blocks[index]
    }
}

impl Functions {
    pub fn new(items: impl IntoIterator<Item = FunctionData>) -> Self {
        Self {
            functions: items.into_iter().collect(),
        }
    }
}

impl Program {
    pub fn new(functions: Functions) -> Self {
        Self {
            functions,
            globals: Globals::default(),
        }
    }

    /// Declares the global `name`, or returns the global already declared under it (the first
    /// span given is kept).
    pub fn declare_global(
        &mut self,
        name: impl AsRef<str>,
        span: Option<source_info::FileSpanId>,
    ) -> GlobalIdx {
        self.globals.declare(name, span)
    }

    /// The global declared under `name`, if any.
    pub fn global_named(&self, name: &str) -> Option<GlobalIdx> {
        self.globals.lookup(name)
    }

    /// The name of global `idx`.
    pub fn global_name(&self, idx: GlobalIdx) -> &Symbol {
        &self.globals[idx].name
    }

    /// Allocates and returns a new function with defaulted contents.
    #[inline]
    pub fn new_function(&mut self) -> FunctionIdx {
        self.functions.functions.push(Default::default());
        self.functions.functions.last_index().unwrap()
    }

    /// Verify that the CTADL IR program satisfies expected invariants. See the description of each
    /// error in the [`VerifyError`] enum.
    pub fn verify(&self) -> Result<(), VerifyErrors> {
        if self.verifies_in_parallel() {
            return Ok(());
        }
        // Something is wrong. Verify again on one thread, which reports exactly what is wrong, in
        // order, with globals printed by name.
        let mut visitor = CirVerify::default();
        visitor.visit_program(self);
        visitor.take_error()
    }

    /// Whether [`Program::verify`] would find nothing wrong, checked on the rayon pool: each
    /// function on its own, with the global count the program check uses, and the function
    /// names for duplicates separately. A program the size of a large app takes seconds to
    /// verify on one thread.
    fn verifies_in_parallel(&self) -> bool {
        use rayon::prelude::*;
        let globals_len = self.globals.len();
        let functions = &self.functions.functions.raw;
        let bodies_ok = functions.par_chunks(256).enumerate().all(|(chunk, fs)| {
            let mut visitor = CirVerify::for_globals(globals_len);
            for (i, f) in fs.iter().enumerate() {
                visitor.visit_function_data(FunctionIdx::new(chunk * 256 + i), f);
            }
            visitor.take_error().is_ok()
        });
        if !bodies_ok {
            return false;
        }
        let mut names = hashbrown::HashSet::with_capacity(functions.len());
        functions.iter().all(|f| names.insert(f.name.as_str()))
    }

    /// Verify a function. See the description of each error in the [`VerifyError`] enum.
    pub fn verify_function(&self, idx: FunctionIdx) -> Result<(), VerifyErrors> {
        let mut visitor = CirVerify::default();
        visitor.visit_function_data(idx, &self[idx]);
        visitor.take_error()
    }

    #[inline]
    pub fn num_functions(&self) -> usize {
        self.functions.len()
    }
}

impl Index<FunctionIdx> for Program {
    type Output = FunctionData;
    #[inline]
    fn index(&self, index: FunctionIdx) -> &Self::Output {
        &self.functions[index]
    }
}

impl IndexMut<FunctionIdx> for Program {
    #[inline]
    fn index_mut(&mut self, index: FunctionIdx) -> &mut Self::Output {
        &mut self.functions[index]
    }
}

impl Index<FunctionIdx> for Functions {
    type Output = FunctionData;
    #[inline]
    fn index(&self, index: FunctionIdx) -> &Self::Output {
        &self.functions[index]
    }
}

impl IndexMut<FunctionIdx> for Functions {
    #[inline]
    fn index_mut(&mut self, index: FunctionIdx) -> &mut Self::Output {
        &mut self.functions[index]
    }
}

impl Deref for Functions {
    type Target = IndexVec<FunctionIdx, FunctionData>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.functions
    }
}

impl DerefMut for Functions {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.functions
    }
}

impl Display for Functions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, func) in self.functions.iter_enumerated() {
            writeln!(f, "begin function_{}", i.index())?;
            write!(f, "{func}")?;
            writeln!(f, "end function_{}", i.index())?;
        }
        Ok(())
    }
}

impl Display for FunctionData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let FunctionData {
            name,
            params,
            blocks,
            locals,
            return_type,
        } = self;
        // Held until this function's body has been written, so locals nested anywhere inside it
        // resolve against this table. `None` (and no output below) unless a `WithLocalNames`
        // render asked for names.
        let _names = local_names::push(locals);
        writeln!(f, "define {name}({params}) -> {return_type}:")?;
        if local_names::is_enabled() && !locals.is_empty() {
            write!(f, "  locals:")?;
            for (idx, decl) in locals.iter_enumerated() {
                write!(f, " %L{}={}", idx.index(), decl.name)?;
            }
            writeln!(f)?;
        }
        write!(f, "{blocks}")
    }
}

impl Index<ParameterIdx> for Params {
    type Output = ParameterType;
    #[inline]
    fn index(&self, index: ParameterIdx) -> &Self::Output {
        &self.parameters[index]
    }
}

impl IndexMut<ParameterIdx> for Params {
    #[inline]
    fn index_mut(&mut self, index: ParameterIdx) -> &mut Self::Output {
        &mut self.parameters[index]
    }
}

impl Deref for Params {
    type Target = IndexVec<ParameterIdx, ParameterType>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.parameters
    }
}

impl DerefMut for Params {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.parameters
    }
}

impl Display for Params {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, ty) in self.parameters.iter_enumerated() {
            if i != 0u32.into() {
                write!(f, ", ")?;
            }
            write!(f, "@p{}[{ty}]", i.index())?;
        }
        Ok(())
    }
}

impl Display for ParameterType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParameterType::Copied => write!(f, "copied"),
            ParameterType::SharedFields => write!(f, "shared_fields"),
            ParameterType::Shared => write!(f, "shared"),
        }
    }
}

impl Display for ReturnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ReturnType { arity } = self;
        write!(f, "{arity}")
    }
}

impl From<AccessPath> for Exp {
    fn from(ap: AccessPath) -> Self {
        Exp::Path(ap)
    }
}

impl From<VariableRef> for Exp {
    fn from(v: VariableRef) -> Self {
        Exp::variable(v)
    }
}

impl From<CallObject> for Exp {
    fn from(obj: CallObject) -> Self {
        Exp::new_object_ref(obj)
    }
}

impl Display for Exp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exp::Bytes(bytes) => write!(f, "<const: {:?}>", bytes),
            Exp::Int(value) => write!(f, "<const: {value}>"),
            Exp::Str(s) => write!(f, "<const: {s:#?}>"),
            Exp::Path(ap) => write!(f, "{ap}"),
            Exp::Value(ap) => write!(f, "value({ap})"),
            Exp::ObjectRef(obj) => write!(f, "{obj}"),
        }
    }
}

impl Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Statement { source_info, kind } = self;
        if source_info.span_id != source_info::NO_SPAN {
            write!(f, "{kind} [{source_info}]")
        } else {
            write!(f, "{kind}")
        }
    }
}

impl Display for SourceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let SourceInfo {
            span_id: source_info::FileSpanId(i),
        } = self;
        write!(f, "{i}")?;
        Ok(())
    }
}

impl Display for StatementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use StatementKind::*;
        match self {
            Assign { dest, sources } => {
                // A store prints as one, so a dump shows which assignments define a variable.
                if dest.is_pathless() {
                    write!(f, "assign {dest} = ")?;
                } else {
                    write!(f, "store {dest} := ")?;
                }
                for (i, src) in sources.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{src}")?;
                }
                Ok(())
            }
            CallAssign { rets, args, style } => {
                for (i, ret) in rets.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", ret)?;
                }
                if !rets.is_empty() {
                    write!(f, " = ")?;
                }
                write!(f, "{}", style)?;
                write!(f, "(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")?;

                if let CallStyle::DirectCall { call_edges } = style
                    && call_edges.len() > 1
                {
                    write!(f, " [{} edges]", call_edges.len())?;
                }
                Ok(())
            }
            Phi {
                dest: out,
                operands,
            } => {
                write!(f, "phi {out} = (")?;
                for (i, (block, op)) in operands.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "[block_{}, {op}]", block.index())?;
                }
                write!(f, ")")
            }
            Nop => write!(f, "nop"),
        }
    }
}

impl Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "block {} statement {}",
            self.block.index(),
            self.statement_index.index()
        )
    }
}

impl Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Program { functions, globals } = self;
        let _names = global_names::push(globals);
        writeln!(f, "begin ctadl-ir ast program")?;
        if !globals.is_empty() {
            write!(f, "globals:")?;
            for (idx, _) in globals.iter_enumerated() {
                write!(f, " {}", Variable::Global(idx))?;
            }
            writeln!(f)?;
        }
        write!(f, "{}", functions)?;
        writeln!(f, "end ctadl-ir ast program")
    }
}

impl Display for ProgramInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ProgramInfo {
            program,
            vmt,
            source_info,
        } = self;
        writeln!(f, "{program}")?;
        writeln!(f, "{vmt}")?;
        writeln!(f, "{source_info}")?;
        Ok(())
    }
}
