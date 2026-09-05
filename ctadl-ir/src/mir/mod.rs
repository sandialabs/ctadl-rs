/*!
# CTADL IR

The CIR is our intermediate representation for data flow analysis. It is not a general purpose IR:
it's optimized for analyzing data flow only, not for compilation, or type analysis. This
representation sits between a frontend AST and the Datalog "facts" format. The representation is a
typical basic block and statement representation. Successors are expressed as "gotos" to other
blocks. These gotos are unconditional; the IR abstracts conditional jumps and just the possible
targets of control flow. Terminator instructions are returns and gotos with multiple successors.

- Expressions are all values, which are constants, variables, and access paths.

- [`StatementKind::Assign`] represent assignments. Assignments set a variable and allow expressions
  on the right-hand side. Multiple assignments can be done in parallel in one statement. Assignments
  such as `a, b = b, a` are expressed in vec form as `[(a,b),(b,a)]` and implement a swap.

- Setting of fields is done through the [`StatementKind::Store`] instruction; reading fields can
  additionally be done through the [`StatementKind::Load`] instruction. A frontend that models a
  field write as a *functional* update — producing a fresh version of the whole aggregate — may
  instead use the [`StatementKind::Update`] instruction, which is a `Store` that additionally names
  the `source` aggregate separately from the (defined) destination.

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

- Parameters are unnamed and referred to by number. They can be passed by value or by reference. It
  is an error to refer to a parameter that is not declared by the enclosing function.

- Functions have a return type which is just an arity. Return statements can return multiple values
  and the function declares the arity each return statement must have. This is not necessary to
  emulate returning by reference; like having multiple parallel assignments, it's just a convenience.

- There is a global heap. Together with access paths, global variables can be modeled as fields of
  the global heap.

CIR variables are untyped, but represent an object. Variables have fields that you can load and
store simply by accessing them with the appropriate statements.

There is an CIR visitor in [`self::visit`] which can be used for immutable and mutable traversal.

We have designed the CIR on purpose to exist in inconsistent states; this can be helpful when
generating code. Once you're done generating code, it should be verified. Verification checks for
the various kinds of errors that should not happen in well-defined programs. It can be checked with
[`MirVerify`] or by simply calling [`Program::verify`].

To make a program suitable for Datalog compilation, one can use SSA transformation
([`crate::ssa::transform`]).

## How to Generate CIR

Frontend language assignment statements can be modeled with [`StatementKind::Assign`] instructions.
Assignments like `x = a + b` can be modeled with `(x, x) = (a, b)`. Expressions must be linearized
before conversion. For instance, a frontend language expression like `x = a + (b + c)` can be
linearized as `(t1, t1) = (b, c); (t2, t2) = (a, t1); x = t1`.

Stores into objects and structures often look like `obj.x.y = w` in frontend languages. These are
modeled as [`StatementKind::Store`] instructions whose destination is `obj`.
Statements like `obj.x = F(y.z)` have to be split into two CIR instructions: first, call the
function and return into a temporary like `t1 = F(y.z)`; next, store the temporary to the
destination object.

Globals variables in frontend languages can be modeled using [`Variable::GlobalHeap`] and fields.
Say you have a global variable `speed`. Loading a global is done with an access path whose variable
is the global heap and a field called `speed`. Storing to speed is done with an
[`StatementKind::Store`] instruction to the `speed` field, using the global heap as the
destination.

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
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::{fmt, fmt::Display};

use internment::ArcIntern;
use smallvec::{SmallVec, smallvec};
use thin_vec::ThinVec;

use crate::index::{idx::Idx, index_vec::IndexVec, index_vec_deque::IndexVecDeque};
use crate::mir::call::VirtualMethodTable;
pub use crate::mir::terminator::{Terminator, TerminatorKind};
use crate::mir::visit::Visitor;
pub use crate::mir::{
    basic_blocks::BasicBlocks,
    call::{CallEdges, CallObject, CallStyle},
    verify::{MirVerify, VerifyError, VerifyErrors},
};
use crate::newtype_index;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod builder_tests;

mod basic_blocks;
pub mod builder;
pub mod call;
pub mod encode;
pub mod path_syntax;
pub mod pos;
mod terminator;
mod verify;
pub mod visit;

pub use crate::mir::path_syntax::{
    PathSyntaxError, PathSyntaxErrorKind, parse_segment, parse_segments, path_to_string,
    segment_to_string, write_path, write_segment,
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

/// A single field access, which can be either a symbolic field name or a numeric offset
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OffsetAccess {
    /// A numeric offset (e.g., 42)
    Offset(Offset),
}

impl OffsetAccess {
    #[inline]
    pub fn is_offset(&self) -> bool {
        matches!(self, OffsetAccess::Offset(_))
    }

    /// The numeric offset (access-path field accesses are offset-only).
    #[inline]
    pub fn offset(&self) -> Offset {
        let OffsetAccess::Offset(offset) = self;
        offset.clone()
    }
}

/// A single segment used only as *input* to path lowering ([`load_access_path`] /
/// [`store_access_path`]) and as the element of the analysis-level path (`facts::Path`). Unlike
/// [`OffsetAccess`] (offset-only) and [`FieldRef`] (a single symbol), a segment sequence may
/// freely mix pointer-arithmetic offsets and symbolic field accesses in any order (e.g.
/// `__stack_top.[8].deref.f`). Lowering turns each symbolic access into a
/// [`StatementKind::Load`]/[`StatementKind::Store`], yielding type-correct offset-only access
/// paths and single-symbol field paths.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PathSegment {
    /// A symbolic field name (e.g., `deref`, or a C field `f`)
    Symbol(Symbol),
    /// A numeric offset (pointer arithmetic)
    Offset(Offset),
}

impl PathSegment {
    #[inline]
    pub fn is_symbol(&self) -> bool {
        matches!(self, PathSegment::Symbol(_))
    }

    #[inline]
    pub fn is_offset(&self) -> bool {
        matches!(self, PathSegment::Offset(_))
    }

    #[inline]
    pub fn symbol<S: AsRef<str>>(name: S) -> Self {
        PathSegment::Symbol(ArcIntern::from(name.as_ref()))
    }

    #[inline]
    pub fn offset(offset: i64) -> Self {
        PathSegment::Offset(Offset(offset))
    }
}

impl From<OffsetAccess> for PathSegment {
    #[inline]
    fn from(fa: OffsetAccess) -> Self {
        let OffsetAccess::Offset(offset) = fa;
        PathSegment::Offset(offset)
    }
}

/// Renders one segment in the canonical access-path grammar, WITHOUT its leading `.`: symbols
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

impl Display for OffsetAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&path_syntax::segment_to_string(&PathSegment::from(
            self.clone(),
        )))
    }
}

/// IR Statements. They capture assignments and function calls.
///
/// Frontends typically generate assign, resolve, and call instructions. The phi and param-flow
/// instructions are generated during SSA conversion.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StatementKind {
    /// Assignment of constants and variables. The first element of the tuple is the destination;
    /// the second element of the tuple is a list of sources. Assignments only set variables, to
    /// set fields, use the `Store` instruction.
    ///
    /// The destinations should not overlap. If they do, the right-most destination overwrites the
    /// previous updates, which is probably not what you want.
    Assign {
        dest: VariableRef,
        sources: SmallVec<[Exp; 2]>,
    },

    /// Load a value from a source address's field into a destination variable:
    ///
    /// ```text
    /// dest = load source.field;
    /// ```
    ///
    /// The destination is defined; the source address is only read. `source` is an *address*
    /// [`AccessPath`] whose path is offset-only (pointer arithmetic, e.g. `x.[50]`); `field` is
    /// the symbolic [`FieldRef`] read at that address (e.g. `.deref`), so a full load reads
    /// `source.base` at `source.accesses ++ field`. The loaded `field` must be non-empty (a
    /// pathless load is just an assign). See [`load_access_path`], which is the one place that
    /// lowers a chain of field accesses into loads: offsets accumulate into `source`, and each
    /// symbolic field emits one `Load`.
    Load {
        dest: VariableRef,
        source: AccessPath,
        field: FieldRef,
    },

    /// Store a value into a destination address's field:
    ///
    /// ```text
    /// store dest.field := value;
    /// ```
    Store {
        dest: AccessPath,
        field: FieldRef,
        /// Value to store
        value: Exp,
    },

    /// Functionally update one field of a structure, producing a new version of the destination
    /// aggregate:
    ///
    /// ```text
    /// dest = update (source, dest.field := value);
    /// ```
    ///
    /// An `Update` is exactly a [`StatementKind::Store`] — it writes a single symbolic `field` at
    /// the offset-only `dest` address — except that it names the `source` aggregate *separately*
    /// from the destination. The result `dest` is `source` with `dest.accesses ++ field` set to
    /// `value`.
    ///
    /// Unlike `Store`, which defines no variable and reads its `dest` only as a location, an
    /// `Update` DEFINES `dest.base`: the new version of the aggregate. Specifying the
    /// `source` and destination separately is what lets SSA conversion rename `dest` to a fresh
    /// version while still reading the previous `source` (e.g. `s = update (s, .foo := new_value)`
    /// becomes `s_2 = update (s_1, .foo := new_value)`). This is the functional-update counterpart
    /// of `Store`; a frontend may emit whichever fits its memory model.
    Update {
        dest: AccessPath,
        /// The aggregate copied into `dest` before the field is written.
        source: VariableRef,
        /// Symbolic field written at `dest` (a single symbol, like `Store`).
        field: FieldRef,
        /// Value to store
        value: Exp,
    },

    /// Call instructions. Call instructions pass parameters in `args` and return values in `rets`.
    /// Multiple values may be returned. Effective all handling is complex, depending on a number
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

    /// Function parameter SSA variables & global heap. This in an anchor for uses of a variable.
    /// It helps when generating code from SSA. Return instructions are instrumented with this
    /// instruction.
    ParamFlow {
        params: IndexVec<ParameterIdx, VariableRef>,
        global: VariableRef,
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
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Variable {
    /// A global variable represents a heap for storing globals. This variable may only be written
    /// in a [`StatementKind::Store`] instruction.
    GlobalHeap,
    /// A local variable, identified by its index into the enclosing function's [`Locals`] table.
    Local(LocalIdx),
    /// A parameter
    Param(ParameterIdx),
}

/// A reference to a variable, possibly with version. Versions are computed by SSA conversion.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VariableRef {
    pub variable: ArcIntern<Variable>,
    pub version: Option<u32>,
}

/// An access path is a variable and a sequence of field accesses
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// An access path is a variable and an **offset-only** sequence of field accesses (pointer
/// arithmetic, e.g. `x.[50].[4]`). Symbolic fields never appear in an access path: a field is
/// reachable only through a [`StatementKind::Load`] (read) or [`StatementKind::Store`] (write),
/// whose field operand is a [`FieldRef`]. The offset-only invariant is checked by verification.
pub struct AccessPath {
    pub base: VariableRef,
    pub accesses: OffsetAccesses,
}

/// A sequence of field accesses. The first or innermost field is index 0.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OffsetAccesses {
    pub offsets: ThinVec<OffsetAccess>,
}

/// A **field path**: the *symbolic*-only sequence of field accesses read by a
/// [`StatementKind::Load`] or written by a [`StatementKind::Store`] (e.g. the `.deref` in
/// `Load(dest, x.[50], .deref)`). This is the counterpart to [`AccessPath`]: offsets live in an
/// access path, symbolic fields live in a field path, and the two meet only at a load/store. The
/// symbolic-only invariant is checked by verification.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldRef {
    pub field: Symbol,
}
/*
impl From<Vec<&str>> for FieldAccesses {
    fn from(vec: Vec<&str>) -> Self {
        Self {
            fields: vec.into_iter().map(String::from).collect(),
        }
    }
}*/
/// Expressions. The IR is flat, so expressions are either constants or access paths
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Exp {
    /// A bare variable reference.
    Variable(VariableRef),
    /// An [`AccessPath`] with a non-empty (offset-only) path, such as `x.[50]` — pointer
    /// arithmetic. Because an [`AccessPath`] holds only offsets (never symbolic fields), this is
    /// address computation, not a memory read, so it is expressible directly as an [`Exp`] with no
    /// [`StatementKind::Load`].
    AccessPath(AccessPath),
    Str(ArcIntern<str>),
    /// A block of bytes. It has no value as a number.
    Bytes(Vec<u8>),
    ObjectRef(CallObject),
    /// An integer constant, sign-extended to `i64`.
    Int(i64),
}

/// A sequence of statements ending with a terminator.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BasicBlockData {
    pub statements: IndexVecDeque<StatementIdx, Statement>,
    pub terminator: Option<Terminator>,
}

/// A function consists of a name, sequence of parameters, and CFG of basic blocks.
///
/// Functions take parameter by reference and can return a tuple of values.
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
    #[cfg_attr(feature = "serde", serde(skip))]
    by_name: HashMap<String, LocalIdx>,
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
        if let Some(&idx) = self.by_name.get(name) {
            return idx;
        }
        let idx = self.decls.push(LocalDecl {
            name: name.to_string(),
        });
        self.by_name.insert(name.to_string(), idx);
        idx
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
        self.by_name.clear();
        for (idx, decl) in self.decls.iter_enumerated() {
            self.by_name.entry(decl.name.clone()).or_insert(idx);
        }
    }
}

/// Renders MIR with [`Variable::Local`]s resolved to their source names.
///
/// [`Display for Variable`](Variable#impl-Display-for-Variable) has no access to the enclosing
/// function, so by default a local prints as its opaque index (`%L7`) — the same form the fact
/// base and graphviz labels use. Wrapping a value in `WithLocalNames` turns on name resolution for
/// the duration of that render: every [`FunctionData`] publishes its [`Locals`] table, locals
/// inside it print as `%name`, and the function header gains a `locals:` line giving the
/// index↔name mapping (so a dump can still be correlated with `%L7_2`-style graph vertices).
///
/// ```text
/// define f(@p0[byval]) -> 1:
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
    /// resolution is off, which is every render but `ctadl inspect`'s.
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

/// Parameters can be passed by value or by reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParameterType {
    #[default]
    ByVal,
    ByRef,
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
/// Well-formed programs must satisfy certain invariants. After generating a progrom, you must call
/// [`Program::verify`] to ensure the invariants are satisfied.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Program {
    /// Set of functions
    pub functions: Functions,
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
    pub fn new_global() -> Self {
        Variable::GlobalHeap
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
            variable: variable.into(),
            version: None,
        }
    }

    /// Creates a reference to the variable with no version
    pub fn new_var_ref(variable: ArcIntern<Variable>) -> Self {
        VariableRef {
            variable,
            version: None,
        }
    }

    /// Creates a local reference with no version from a local index
    #[inline]
    pub fn new_local_idx(idx: LocalIdx) -> Self {
        VariableRef {
            variable: Variable::new_local(idx).into(),
            version: None,
        }
    }

    /// Creates a global heap reference with no version
    #[inline]
    pub fn new_global() -> Self {
        VariableRef {
            variable: Variable::GlobalHeap.into(),
            version: None,
        }
    }

    /// Creates a parameter reference with no version
    #[inline]
    pub fn new_parameter(param: ParameterIdx) -> Self {
        VariableRef {
            variable: Variable::Param(param).into(),
            version: None,
        }
    }

    /// Clones the variable and uses the given version
    #[inline]
    pub fn with_version(&self, version: u32) -> Self {
        let variable = self.variable.clone();
        let version = Some(version);
        VariableRef { variable, version }
    }
}

impl AccessPath {
    /// Creates a new access path from a variable and field access iterator
    #[inline]
    pub fn new(variable: VariableRef, path: impl IntoIterator<Item = OffsetAccess>) -> Self {
        Self {
            base: variable,
            accesses: path.into_iter().collect(),
        }
    }

    #[inline]
    pub fn without_fields(variable: VariableRef) -> Self {
        Self {
            base: variable,
            accesses: OffsetAccesses::new(std::iter::empty::<OffsetAccess>()),
        }
    }
}

impl From<Variable> for AccessPath {
    #[inline]
    fn from(v: Variable) -> Self {
        AccessPath::without_fields(VariableRef::new(v))
    }
}

impl From<VariableRef> for AccessPath {
    #[inline]
    fn from(v: VariableRef) -> Self {
        AccessPath::without_fields(v)
    }
}

impl Display for AccessPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let AccessPath {
            base: variable,
            accesses: path,
        } = self;
        write!(f, "{variable}{path}")
    }
}

impl OffsetAccesses {
    #[inline]
    pub fn new(path: impl IntoIterator<Item = OffsetAccess>) -> Self {
        Self {
            offsets: path.into_iter().collect(),
        }
    }

    #[inline]
    pub fn empty() -> Self {
        Self {
            offsets: ThinVec::new(),
        }
    }

    /// Creates an `OffsetAccesses` holding one offset.
    #[inline]
    pub fn with_offset(offset: i64) -> Self {
        Self {
            offsets: thin_vec::thin_vec![OffsetAccess::Offset(Offset(offset))],
        }
    }

    /// Creates an `OffsetAccesses` from a sequence of offsets.
    #[inline]
    pub fn with_offsets(offsets: impl IntoIterator<Item = i64>) -> Self {
        Self {
            offsets: offsets
                .into_iter()
                .map(|o| OffsetAccess::Offset(Offset(o)))
                .collect(),
        }
    }
}

impl FromIterator<OffsetAccess> for OffsetAccesses {
    #[inline]
    fn from_iter<I: IntoIterator<Item = OffsetAccess>>(data: I) -> Self {
        Self {
            offsets: data.into_iter().collect(),
        }
    }
}

impl Display for OffsetAccesses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        for field in &self.offsets {
            out.push('.');
            path_syntax::write_segment(&mut out, &PathSegment::from(field.clone()));
        }
        f.write_str(&out)
    }
}

impl FieldRef {
    /// A field path holding a single symbolic field.
    #[inline]
    pub fn symbol<S: AsRef<str>>(name: S) -> Self {
        Self {
            field: ArcIntern::from(name.as_ref()),
        }
    }

    /// A field path from an already-interned symbol.
    #[inline]
    pub fn new(field: Symbol) -> Self {
        Self { field }
    }

    /// The symbolic field name.
    #[inline]
    pub fn as_str(&self) -> &str {
        self.field.as_ref()
    }

    /// The interned symbol.
    #[inline]
    pub fn symbol_ref(&self) -> &Symbol {
        &self.field
    }
}

impl From<Symbol> for FieldRef {
    #[inline]
    fn from(field: Symbol) -> Self {
        Self { field }
    }
}

impl From<&str> for FieldRef {
    #[inline]
    fn from(name: &str) -> Self {
        Self::symbol(name)
    }
}

impl Display for FieldRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::from(".");
        path_syntax::write_segment(&mut out, &PathSegment::Symbol(self.field.clone()));
        f.write_str(&out)
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
            Variable::GlobalHeap => write!(f, "$globals"),
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

impl Deref for OffsetAccesses {
    type Target = [OffsetAccess];
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.offsets[..]
    }
}

impl Exp {
    #[inline]
    pub fn variable(v: VariableRef) -> Self {
        Self::Variable(v)
    }

    /// Builds an expression from an access path. A pathless access path is a bare
    /// [`Exp::Variable`]; an offset-only path is an [`Exp::AccessPath`] (pointer arithmetic).
    /// Panics on a path that carries a symbolic (non-offset) field — read those through a
    /// [`StatementKind::Load`].
    #[inline]
    pub fn access_path(ap: AccessPath) -> Self {
        if ap.accesses.is_empty() {
            Self::Variable(ap.base)
        } else {
            assert!(
                ap.accesses.iter().all(OffsetAccess::is_offset),
                "an access-path expression must be offset-only; use a Load for field reads: {ap}"
            );
            Self::AccessPath(ap)
        }
    }

    /// Returns the base variable read by this expression, if it reads a variable or an address
    /// derived from one (`Exp::Variable` or `Exp::AccessPath`). Constants return `None`.
    #[inline]
    pub fn base_variable(&self) -> Option<&VariableRef> {
        match self {
            Exp::Variable(v) => Some(v),
            Exp::AccessPath(ap) => Some(&ap.base),
            _ => None,
        }
    }

    /// Mutable counterpart of [`Exp::base_variable`].
    #[inline]
    pub fn base_variable_mut(&mut self) -> Option<&mut VariableRef> {
        match self {
            Exp::Variable(v) => Some(v),
            Exp::AccessPath(ap) => Some(&mut ap.base),
            _ => None,
        }
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

    /// Returns the variable this expression reads, if it is a variable reference.
    #[inline]
    pub fn variable_ref(&self) -> Option<&VariableRef> {
        match self {
            Exp::Variable(v) => Some(v),
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

    /// # Panics
    ///
    /// If there is no terminator.
    #[inline]
    pub fn terminator(&self) -> &Terminator {
        self.terminator.as_ref().expect("no terminator")
    }

    #[inline]
    pub fn terminator_mut(&mut self) -> &mut Terminator {
        self.terminator.as_mut().expect("no terminator")
    }

    /// Returns terminator as an option
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
    /// Generates a [`StatementKind::ParamFlow`] instruction for the given arity.
    pub fn param_flow(arity: usize) -> Self {
        let params = (0..arity)
            .map(|i| VariableRef::new(Variable::new_parameter(ParameterIdx::new(i))))
            .collect();
        Self::ParamFlow {
            params,
            global: VariableRef::new_global(),
        }
    }

    /// Generates an assign from a destination and sources. Use this to flow multiple sources into
    /// a single destination, i.e., when modeling a statement like "x = y + z" where you want "y"
    /// and "z" to flow into "x."
    #[inline]
    pub fn assign<I>(dest: VariableRef, srcs: I) -> Self
    where
        I: IntoIterator<Item = Exp>,
    {
        StatementKind::Assign {
            dest,
            sources: srcs.into_iter().collect(),
        }
    }

    /// Constructs a load `dest = source.field`. The loaded `field` is a single symbol; the
    /// `source` address path (if any) is offset-only (pointer arithmetic — see
    /// [`StatementKind::Load`]).
    pub fn load(
        dest: VariableRef,
        source: impl Into<AccessPath>,
        field: impl Into<FieldRef>,
    ) -> Self {
        StatementKind::Load {
            dest,
            source: source.into(),
            field: field.into(),
        }
    }

    /// Constructs a store `store dest.field := value` to an offset-only `dest` address and a
    /// single symbolic `field`. For a location that includes intermediate symbolic
    /// dereferences, use [`store_access_path`], which emits the needed loads.
    pub fn store(dest: AccessPath, field: impl Into<FieldRef>, value: Exp) -> Self {
        let field = field.into();
        StatementKind::Store { dest, field, value }
    }

    /// Constructs a functional update `dest = update (source, dest.field := value)`: `dest` becomes
    /// `source` with the single symbolic `field` at the offset-only `dest` address set to `value`.
    /// Unlike [`Self::store`], the resulting `dest` variable is defined (a new version of the
    /// aggregate), so the `source` and destination are given separately (see
    /// [`StatementKind::Update`]).
    pub fn update(
        dest: AccessPath,
        source: VariableRef,
        field: impl Into<FieldRef>,
        value: Exp,
    ) -> Self {
        let field = field.into();
        StatementKind::Update {
            dest,
            source,
            field,
            value,
        }
    }

    /// Emits an [`StatementKind::Assign`] when `field` is `None` (a write with no symbolic field),
    /// or a [`Self::store`] of `field` into `dest` otherwise. When `field` is `None` the `dest` must
    /// be a bare variable: storing to an offset address with no field is an error, since a store
    /// always writes a symbolic field.
    #[inline]
    pub fn assign_or_store(dest: AccessPath, field: Option<FieldRef>, src: Exp) -> Self {
        match field {
            Some(field) => Self::store(dest, field, src),
            None => {
                assert!(
                    dest.accesses.is_empty(),
                    "storing to an offset address with no field is an error"
                );
                StatementKind::Assign {
                    dest: dest.base,
                    sources: smallvec![src],
                }
            }
        }
    }

    // #[inline]
    // pub fn assigns<B>(assigns: &[(AccessPath, Exp)]) -> B
    // where
    //     B: FromIterator<Self> + Sized,
    // {
    //     assigns
    //         .iter()
    //         .cloned()
    //         .map(|(lhs, rhs)| Self::assign(lhs, rhs))
    //         .collect()
    // }

    /// Returns an iterator over variables read by this statement.
    pub fn iter_src_var<'s>(&'s self) -> VarIter<'s> {
        use StatementKind::*;
        match self {
            Assign { dest: _, sources } => Box::new(sources.iter().filter_map(Exp::base_variable)),
            CallAssign { args, style, .. } => {
                let a: VarIter<'s> = Box::new(args.iter().filter_map(Exp::base_variable));
                let b: VarIter<'s> = match style.receiver() {
                    Some(r) => Box::new(std::iter::once(r)),
                    None => Box::new(std::iter::empty()),
                };
                Box::new(a.chain(b))
            }
            Phi { operands, .. } => Box::new(operands.iter().map(|(_, v)| v)),
            ParamFlow { params, global } => Box::new(params.iter().chain(std::iter::once(global))),
            Load {
                dest: _,
                source,
                field: _,
            } => Box::new(std::iter::once(&source.base)),
            Store {
                dest,
                field: _,
                value,
            } => {
                let a: VarIter<'s> = Box::new(std::iter::once(&dest.base));
                let b: VarIter<'s> = match Exp::base_variable(value) {
                    Some(v) => Box::new(std::iter::once(v)),
                    None => Box::new(std::iter::empty()),
                };
                Box::new(a.chain(b))
            }
            Update {
                dest: _,
                source,
                field: _,
                value,
            } => {
                let a: VarIter<'s> = Box::new(std::iter::once(source));
                let b: VarIter<'s> = match Exp::base_variable(value) {
                    Some(v) => Box::new(std::iter::once(v)),
                    None => Box::new(std::iter::empty()),
                };
                Box::new(a.chain(b))
            }
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over mutable variables referenced by this statement.
    pub fn iter_src_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        use StatementKind::*;
        match self {
            Assign { dest: _, sources } => {
                Box::new(sources.iter_mut().filter_map(Exp::base_variable_mut))
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
            ParamFlow { params, global } => {
                Box::new(params.iter_mut().chain(std::iter::once(global)))
            }
            Load {
                dest: _,
                source,
                field: _,
            } => Box::new(std::iter::once(&mut source.base)),
            Store {
                dest,
                field: _,
                value,
            } => {
                let a: VarIterMut<'s> = Box::new(std::iter::once(&mut dest.base));
                let b: VarIterMut<'s> = if let Some(v) = Exp::base_variable_mut(value) {
                    Box::new(std::iter::once(v))
                } else {
                    Box::new(std::iter::empty())
                };
                Box::new(a.chain(b))
            }
            Update {
                dest: _,
                source,
                field: _,
                value,
            } => {
                let a: VarIterMut<'s> = Box::new(std::iter::once(source));
                let b: VarIterMut<'s> = if let Some(v) = Exp::base_variable_mut(value) {
                    Box::new(std::iter::once(v))
                } else {
                    Box::new(std::iter::empty())
                };
                Box::new(a.chain(b))
            }
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over variables set by this statement.
    pub fn iter_dst_var<'s>(&'s self) -> VarIter<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, .. } => Box::new(std::iter::once(dest)),
            Load { dest, .. } => Box::new(std::iter::once(dest)),
            CallAssign { rets, .. } => Box::new(rets.iter()),
            Phi { dest, .. } => Box::new(std::iter::once(dest)),
            ParamFlow { .. } => Box::new(std::iter::empty()),
            Store { .. } => Box::new(std::iter::empty()),
            Update { dest, .. } => Box::new(std::iter::once(&dest.base)),
            Nop => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over variables set by this statement.
    pub fn iter_dst_var_mut<'s>(&'s mut self) -> VarIterMut<'s> {
        use StatementKind::*;
        match self {
            Assign { dest, sources: _ } => Box::new(std::iter::once(dest)),
            Load { dest, .. } => Box::new(std::iter::once(dest)),
            CallAssign { rets, .. } => Box::new(rets.iter_mut()),
            Phi { dest: out, .. } => Box::new(std::iter::once(out)),
            ParamFlow { .. } => Box::new(std::iter::empty()),
            Store { .. } => Box::new(std::iter::empty()),
            Update { dest, .. } => Box::new(std::iter::once(&mut dest.base)),
            Nop => Box::new(std::iter::empty()),
        }
    }
}

/// Lowers a *read* of a mixed [`PathSegment`] sequence rooted at `base` into a sequence of
/// [`StatementKind::Load`] instructions, appending them to `out` and returning the residual
/// *address* — the base variable plus any trailing accumulated offset — as an offset-only
/// [`AccessPath`].
///
/// Offsets and symbolic fields are treated differently, matching the two things they mean:
///
/// - A [`PathSegment::Offset`] is pointer arithmetic (address computation), *not* a memory access.
///   It emits no load; it accumulates into the current address, and consecutive offsets merge into
///   a single offset (`x.[10].[40]` ⟶ `x.[50]`).
/// - A [`PathSegment::Symbol`] (e.g. `deref`, or a C field `f`) is a memory read. It emits one
///   [`StatementKind::Load`] whose `source` is the current address (base variable + accumulated
///   offset) and whose loaded `field` is that single symbol. The load's destination — a fresh
///   temporary minted by `fresh` — becomes the new base, and offset accumulation restarts from it.
///
/// So `a.f.g` lowers to `t1 = load a.f; t2 = load t1.g` and returns `t2`;
/// `x.[10].deref.[20].deref` lowers to `t1 = load x.[10].deref; t2 = load t1.[20].deref` and
/// returns `t2`; a pure address `x.[50]` emits nothing and returns `x.[50]`; an empty segment
/// sequence returns `base` unchanged.
pub fn load_access_path(
    base: VariableRef,
    segments: impl IntoIterator<Item = PathSegment>,
    out: &mut Vec<Statement>,
    mut fresh: impl FnMut() -> VariableRef,
) -> AccessPath {
    // `cur` is the current address: the base variable plus a merged trailing offset.
    let mut cur = AccessPath::without_fields(base);
    for segment in segments {
        match segment {
            PathSegment::Offset(offset) => match cur.accesses.offsets.last_mut() {
                // Merge consecutive offsets (address arithmetic composes).
                Some(OffsetAccess::Offset(prev)) => prev.0 = prev.0.wrapping_add(offset.0),
                _ => cur.accesses.offsets.push(OffsetAccess::Offset(offset)),
            },
            PathSegment::Symbol(symbol) => {
                // A symbolic field is a memory read: load it from the current address and continue
                // from the loaded value.
                let dest = fresh();
                let source = std::mem::replace(&mut cur, AccessPath::without_fields(dest.clone()));
                out.push(Statement::new_kind(StatementKind::load(
                    dest,
                    source,
                    FieldRef::new(symbol),
                )));
            }
        }
    }
    cur
}

/// Lowers a *write* of `value` into a mixed [`PathSegment`] sequence rooted at `base` into loads
/// for the intermediate dereferences plus a single [`StatementKind::Store`] (or
/// [`StatementKind::Assign`] for an empty sequence), appending them to `out`.
///
/// This is the write-side counterpart of [`load_access_path`]: offsets are pointer arithmetic and
/// stay on the address; every symbolic field *except the last* is a load (you materialize the
/// intermediate pointer); the *final* symbolic field is the store's field. So `store *(x.[8].deref).f
/// := v` (segments `x.[8].deref.f`) lowers to `t = load x.[8].deref; store t.f := v`. A sequence with
/// no fields or offsets at all is an assign to `base`. An offset-terminated sequence (offsets but no
/// trailing symbolic field) is an error: a store always writes a symbolic field, so the caller must
/// terminate a memory write with one (e.g. a frontend synthesizing a `.deref`).
pub fn store_access_path(
    base: VariableRef,
    segments: impl IntoIterator<Item = PathSegment>,
    value: Exp,
    out: &mut Vec<Statement>,
    mut fresh: impl FnMut() -> VariableRef,
) {
    let mut segments: ThinVec<PathSegment> = segments.into_iter().collect();
    // Split off a trailing symbol (the store field), if any. Everything before it is an address
    // computation (offsets + loads for interior derefs). Any trailing offsets after the last
    // symbol stay on the store address (a field-less offset write).
    let field = match segments.iter().rposition(PathSegment::is_symbol) {
        Some(i) if segments[i + 1..].iter().all(PathSegment::is_offset) => {
            let PathSegment::Symbol(symbol) = segments.remove(i) else {
                unreachable!()
            };
            Some(FieldRef::new(symbol))
        }
        _ => None,
    };
    let addr = load_access_path(base, segments, out, &mut fresh);
    // A trailing symbol becomes the store's field; no symbol over a bare variable is an assign. An
    // offset-terminated sequence (offsets but no field) is an error: a store always writes a
    // symbolic field, so the caller must terminate a memory write with one (see `assign_or_store`).
    out.push(Statement::new_kind(StatementKind::assign_or_store(
        addr, field, value,
    )));
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
        let mut visitor = MirVerify::default();
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
        Self { functions }
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
        let mut visitor = MirVerify::default();
        visitor.visit_program(self);
        visitor.take_error()
    }

    /// Verify a function. See the description of each error in the [`VerifyError`] enum.
    pub fn verify_function(&self, idx: FunctionIdx) -> Result<(), VerifyErrors> {
        let mut visitor = MirVerify::default();
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
            ParameterType::ByVal => write!(f, "byval"),
            ParameterType::ByRef => write!(f, "byref"),
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
    /// Converts an access path into an expression. A pathless path becomes an [`Exp::Variable`]
    /// and an offset-only path becomes an [`Exp::AccessPath`] (address arithmetic). A symbolic
    /// field read cannot be expressed as an [`Exp`]; lower it into a [`StatementKind::Load`] first
    /// (see [`load_access_path`]). Panics if the access path carries a symbolic field.
    fn from(ap: AccessPath) -> Self {
        Exp::access_path(ap)
    }
}

impl From<VariableRef> for Exp {
    fn from(v: VariableRef) -> Self {
        Exp::Variable(v)
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
            Exp::Variable(v) => write!(f, "{}", v),
            Exp::AccessPath(ap) => write!(f, "{}", ap),
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
                write!(f, "assign ")?;
                write!(f, "{dest}")?;
                write!(f, " = ")?;
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
            ParamFlow { params, global } => {
                write!(f, "param-flow ")?;
                for (i, op) in params.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{op}")?;
                }
                write!(f, "; {global}")?;
                Ok(())
            }
            Load {
                dest,
                source,
                field,
            } => write!(f, "{dest} = load {source}{field}"),
            Store { dest, field, value } => {
                write!(f, "store {dest}{field} := {value}")
            }
            Update {
                dest,
                source,
                field,
                value,
            } => {
                // `dest_var = update (source<offsets><field> := value)`, e.g. `x = update (y.f :=
                // v)`. The offsets live on the destination address; the result is the destination
                // variable.
                write!(
                    f,
                    "{} = update ({}{}{} := {})",
                    dest.base, source, dest.accesses, field, value
                )
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
        let Program { functions } = self;
        writeln!(f, "begin ctadl-ir ast program")?;
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
