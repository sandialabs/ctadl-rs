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
  additionally be done through the [`StatementKind::Load`] instruction.

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
pub mod pos;
mod terminator;
mod verify;
pub mod visit;

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
pub enum FieldAccess {
    /// A symbolic field name (e.g., "field_name")
    Symbol(Symbol),
    /// A numeric offset (e.g., 42)
    Offset(Offset),
}

impl FieldAccess {
    pub fn is_symbol(&self) -> bool {
        matches!(self, FieldAccess::Symbol(_))
    }

    pub fn is_offset(&self) -> bool {
        matches!(self, FieldAccess::Offset(_))
    }
}

impl Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Print signed hex
        if self.0 < 0i64 {
            let pos = -self.0;
            write!(f, "-0x{:x}", pos)
        } else {
            write!(f, "0x{:x}", self.0)
        }
    }
}

impl Display for FieldAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldAccess::Symbol(symbol) => write!(f, "{symbol}"),
            FieldAccess::Offset(offset) => write!(f, "[{offset}]"),
        }
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

    /// Load a value from a source variable's field path into a destination variable:
    ///
    /// ```text
    /// dest = load source.path;
    /// ```
    ///
    /// The destination is defined; the source variable is only read. The path must be
    /// non-empty (a pathless load is just an assign).
    Load {
        dest: VariableRef,
        source: VariableRef,
        path: FieldAccesses,
    },

    /// Store a value into a destination variable's field path:
    ///
    /// ```text
    /// store dest.path := value;
    /// ```
    ///
    /// Unlike the old functional `Update` instruction, a store defines NO variable: the
    /// destination base is only read (as a location), so SSA conversion does not create a new
    /// version of the aggregate on every write. The path must be non-empty (a pathless store
    /// is just an assign). This instruction is used to handle local variables with fields and
    /// global variables.
    Store {
        dest: VariableRef,
        path: FieldAccesses,
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
    /// A local variable
    Local(String),
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
pub struct AccessPath {
    pub variable_ref: VariableRef,
    pub path: FieldAccesses,
}

/// A sequence of field accesses. The first or innermost field is index 0.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldAccesses {
    pub fields: ThinVec<FieldAccess>,
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
    /// A bare variable reference. Reading a field is NOT expressible as an expression: use a
    /// [`StatementKind::Load`] to load a field path into a variable, then reference that
    /// variable here. This makes loads explicit in the IR (there is exactly one way to read a
    /// field).
    Variable(VariableRef),
    Str(ArcIntern<str>),
    Bytes(Vec<u8>),
    ObjectRef(CallObject),
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
    pub fn new_local(name: String) -> Self {
        Variable::Local(name)
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
    pub fn local(&self) -> Option<&str> {
        match self {
            Variable::Local(name) => Some(name),
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

    /// Creates a local reference with no version
    #[inline]
    pub fn new_local(name: String) -> Self {
        VariableRef {
            variable: Variable::new_local(name).into(),
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
    pub fn new(variable: VariableRef, path: impl IntoIterator<Item = FieldAccess>) -> Self {
        Self {
            variable_ref: variable,
            path: path.into_iter().collect(),
        }
    }

    #[inline]
    pub fn without_fields(variable: VariableRef) -> Self {
        Self {
            variable_ref: variable,
            path: FieldAccesses::new(std::iter::empty::<FieldAccess>()),
        }
    }

    /// This functions takes a name like `count` or `frobnaz` and returns a reference to the global
    /// variable with that name. The name should not have a dot (`.`) in it.
    pub fn new_global(name: &str, fp: FieldAccesses) -> Self {
        let path =
            std::iter::once(FieldAccess::Symbol(ArcIntern::<str>::from(name))).chain(fp.fields);
        Self::new(VariableRef::new_global(), path)
    }
}

impl From<&str> for AccessPath {
    #[inline]
    fn from(s: &str) -> Self {
        AccessPath::without_fields(VariableRef::new_local(s.to_string()))
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
            variable_ref: variable,
            path,
        } = self;
        write!(f, "{variable}{path}")
    }
}

impl FieldAccesses {
    #[inline]
    pub fn new(path: impl IntoIterator<Item = FieldAccess>) -> Self {
        Self {
            fields: path.into_iter().collect(),
        }
    }

    #[inline]
    pub fn empty() -> Self {
        Self {
            fields: ThinVec::new(),
        }
    }

    /// Create a new FieldAccesses with a single offset
    #[inline]
    pub fn with_offset(offset: i64) -> Self {
        Self {
            fields: thin_vec::thin_vec![FieldAccess::Offset(Offset(offset))],
        }
    }

    /// Create a new FieldAccesses with mixed field accesses
    #[inline]
    pub fn mixed<S: AsRef<str>>(path: impl IntoIterator<Item = Result<S, i64>>) -> Self {
        Self {
            fields: path
                .into_iter()
                .map(|item| match item {
                    Ok(s) => FieldAccess::Symbol(ArcIntern::from(s.as_ref())),
                    Err(offset) => FieldAccess::Offset(Offset(offset)),
                })
                .collect(),
        }
    }
}

impl<S: AsRef<str>> FromIterator<S> for FieldAccesses {
    #[inline]
    fn from_iter<I: IntoIterator<Item = S>>(data: I) -> Self {
        Self {
            fields: data
                .into_iter()
                .map(|s| FieldAccess::Symbol(s.as_ref().into()))
                .collect(),
        }
    }
}

impl FromIterator<FieldAccess> for FieldAccesses {
    #[inline]
    fn from_iter<I: IntoIterator<Item = FieldAccess>>(data: I) -> Self {
        Self {
            fields: data.into_iter().collect(),
        }
    }
}

impl Display for FieldAccesses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for field in &self.fields {
            match field {
                FieldAccess::Symbol(symbol) => write!(f, ".{symbol}")?,
                FieldAccess::Offset(offset) => write!(f, ".[{offset}]")?,
            }
        }
        Ok(())
    }
}

impl From<&str> for Variable {
    #[inline]
    fn from(s: &str) -> Self {
        Variable::new_local(s.to_string())
    }
}

impl From<String> for Variable {
    fn from(s: String) -> Self {
        Variable::new_local(s)
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
            Variable::Local(name) => write!(f, "%{name}"),
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

impl Deref for FieldAccesses {
    type Target = [FieldAccess];
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.fields[..]
    }
}

impl Exp {
    #[inline]
    pub fn variable(v: VariableRef) -> Self {
        Self::Variable(v)
    }

    #[inline]
    pub fn new_bytes(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
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

    /// Constructs a load of a field path. The path must be non-empty.
    pub fn load(dest: VariableRef, source: VariableRef, path: FieldAccesses) -> Self {
        assert!(!path.is_empty(), "load source path must be non-empty");
        StatementKind::Load { dest, source, path }
    }

    /// Constructs a store to a structure field path. The destination path must be non-empty.
    pub fn store(dest: AccessPath, src: Exp) -> Self {
        let AccessPath { variable_ref, path } = dest;
        assert!(!path.is_empty(), "store destination path must be non-empty");
        StatementKind::Store {
            dest: variable_ref,
            path,
            value: src,
        }
    }

    /// Generates either an assign or a store depending on whether fields are being set. Use
    /// this when the caller might or might not be writing a field.
    #[inline]
    pub fn assign_or_store(dest: AccessPath, src: Exp) -> Self {
        if !dest.path.is_empty() {
            Self::store(dest, src)
        } else {
            StatementKind::Assign {
                dest: dest.variable_ref,
                sources: smallvec![src],
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
            Assign { dest: _, sources } => Box::new(sources.iter().filter_map(|src| {
                if let Exp::Variable(v) = src {
                    Some(v)
                } else {
                    None
                }
            })),
            CallAssign { args, style, .. } => {
                let a: VarIter<'s> = Box::new(args.iter().filter_map(|src| {
                    if let Exp::Variable(v) = src {
                        Some(v)
                    } else {
                        None
                    }
                }));
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
                path: _,
            } => Box::new(std::iter::once(source)),
            Store { dest, path: _, value } => {
                let a: VarIter<'s> = Box::new(std::iter::once(dest));
                let b: VarIter<'s> = if let Exp::Variable(v) = value {
                    Box::new(std::iter::once(v))
                } else {
                    Box::new(std::iter::empty())
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
            Assign { dest: _, sources } => Box::new(sources.iter_mut().filter_map(|src| {
                if let Exp::Variable(v) = src {
                    Some(v)
                } else {
                    None
                }
            })),
            CallAssign { args, style, .. } => {
                let a: VarIterMut<'s> = Box::new(args.iter_mut().filter_map(|src| {
                    if let Exp::Variable(v) = src {
                        Some(v)
                    } else {
                        None
                    }
                }));
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
                path: _,
            } => Box::new(std::iter::once(source)),
            Store { dest, path: _, value } => {
                let a: VarIterMut<'s> = Box::new(std::iter::once(dest));
                let b: VarIterMut<'s> = if let Exp::Variable(v) = value {
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
            Nop => Box::new(std::iter::empty()),
        }
    }
}

/// Lowers a *read* of an access path into a sequence of [`StatementKind::Load`] instructions,
/// appending them to `out` and returning the variable that holds the loaded value.
///
/// Reading a field is not expressible as an [`Exp`] (there is no `Exp::AccessPath`), so every
/// field read must go through a `Load`. This helper is the one place that turns a chain of
/// field accesses into loads. The chain is split into one load per pointer dereference: fields
/// are accumulated and a load is emitted whenever a `deref` field is seen (and once more for any
/// trailing non-deref fields). Each intermediate value flows through a fresh temporary minted by
/// `fresh`.
///
/// A pathless access path is returned as-is (no load emitted); a purely symbolic field chain with
/// no `deref` (e.g. `a.f.g`) becomes a single load carrying the whole path.
pub fn load_access_path(
    ap: AccessPath,
    out: &mut Vec<Statement>,
    mut fresh: impl FnMut() -> VariableRef,
) -> VariableRef {
    let AccessPath { variable_ref, path } = ap;
    if path.fields.is_empty() {
        return variable_ref;
    }
    // Experiment toggle: emit a single load carrying the whole path instead of splitting into
    // one load per pointer dereference. Per-deref splitting is the points-to-normalized form,
    // but under the current syntactic index engine it broadens forward propagation (short-path
    // temporaries reach more variables). Set CTADL_NO_SPLIT_LOADS to measure the single-load form.
    if std::env::var_os("CTADL_NO_SPLIT_LOADS").is_some() {
        let dest = fresh();
        out.push(Statement::new_kind(StatementKind::load(
            dest.clone(),
            variable_ref,
            path,
        )));
        return dest;
    }
    let n = path.fields.len();
    let mut cur = variable_ref;
    let mut seg: ThinVec<FieldAccess> = ThinVec::new();
    for (i, field) in path.fields.into_iter().enumerate() {
        let is_deref = matches!(&field, FieldAccess::Symbol(s) if &**s == "deref");
        seg.push(field);
        let is_last = i + 1 == n;
        if is_deref || is_last {
            let dest = fresh();
            out.push(Statement::new_kind(StatementKind::load(
                dest.clone(),
                cur,
                FieldAccesses {
                    fields: std::mem::take(&mut seg),
                },
            )));
            cur = dest;
        }
    }
    cur
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
            return_type,
        }
    }

    #[inline]
    pub fn num_parameters(&self) -> usize {
        self.params.parameters.len()
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
            return_type,
        } = self;
        writeln!(f, "define {name}({params}) -> {return_type}:")?;
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
    /// Converts a *pathless* access path into a variable expression. Reading a field cannot be
    /// expressed as an [`Exp`]; lower it into a [`StatementKind::Load`] first (see
    /// [`load_access_path`]). Panics if the access path carries a non-empty field path.
    fn from(ap: AccessPath) -> Self {
        assert!(
            ap.path.is_empty(),
            "cannot convert a field access path into an Exp; use a Load instruction: {ap}"
        );
        Exp::Variable(ap.variable_ref)
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
            Exp::Str(s) => write!(f, "<const: {s:#?}>"),
            Exp::Variable(v) => write!(f, "{}", v),
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
            Load { dest, source, path } => write!(f, "{dest} = load {source}{path}"),
            Store { dest, path, value } => {
                write!(f, "store {dest}{path} := {value}")
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
