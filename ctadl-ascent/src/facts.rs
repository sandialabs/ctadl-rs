//! Data types for facts

use std::collections::{BTreeMap, VecDeque};
use std::ops::Deref;
use std::str::FromStr;
use std::{fmt, fmt::Display};

use derive_builder::Builder;
use internment::ArcIntern;
use packed_struct::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::{Error, ErrorContext};
use ctadl_ir::{Idx, mir, mir::Offset};

pub mod parquet;
pub mod schema;

pub type Str = ArcIntern<str>;
type EltId = Str;

lazy_static::lazy_static! {
    pub static ref EMPTY_STR: Str = ArcIntern::<str>::from("");
}

/// A sequence of field/array accesses
///
/// The path dereferences go left to right
/// ["foo", "bar", "baz"] represents .foo.bar.baz
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Path(pub VecDeque<mir::FieldAccess>);

impl Path {
    /// Creates an empty path
    #[inline]
    pub fn empty() -> Self {
        Path(VecDeque::new())
    }

    /// Denotes the empty path
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns the string representation with dot prefixes for display
    /// e.g., ["foo", "bar"] becomes ".foo.bar"
    pub fn to_dot_string(&self) -> String {
        if self.0.is_empty() {
            String::new()
        } else {
            let mut result = String::with_capacity(self.0.len() * 2); // Rough estimate

            // Add leading dot for the whole path
            result.push('.');

            for (i, component) in self.0.iter().enumerate() {
                if i > 0 {
                    // Add separator dot (unescaped)
                    result.push('.');
                }

                // Handle both Symbol and Offset variants
                match component {
                    mir::FieldAccess::Symbol(symbol) => {
                        // Escape dots WITHIN components
                        let symbol_str: &str = symbol.as_ref();
                        let escaped = symbol_str.replace(".", "\\.");
                        result.push_str(&escaped);
                    }
                    mir::FieldAccess::Offset(offset) => {
                        result.push_str(&format!("[{}]", offset.0));
                    }
                }
            }

            result
        }
    }

    /// Concatenates two paths by combining their components, merging adjacent offsets.
    #[inline]
    pub fn concat(&self, other: &Path) -> Self {
        let mut result = self.clone();
        result.extend_merging(other.0.iter().cloned());
        result
    }

    /// Pushes a new component to the path, merging offsets if possible.
    pub fn push(&mut self, component: mir::FieldAccess) {
        if let (mir::FieldAccess::Offset(new_off), Some(mir::FieldAccess::Offset(last_off))) =
            (&component, self.0.back_mut())
        {
            last_off.0 += new_off.0;
            return;
        }
        self.0.push_back(component);
    }

    pub fn is_prefix_of(&self, other: &Path) -> bool {
        match_prefix(other, self).map_or(false, |suffix| {
            !matches!((self.0.back(), suffix.front()), 
                (Some(mir::FieldAccess::Offset(_)), Some(mir::FieldAccess::Offset(Offset(d)))) if *d < 0)
        })
    }

    pub fn pop(mut self) -> Option<Self> {
        match self.0.pop_back() {
            Some(_) => Some(self),
            None => None,
        }
    }

    /// Appends components from an iterator, merging offsets.
    pub fn extend_merging(&mut self, iter: impl IntoIterator<Item = mir::FieldAccess>) {
        for component in iter {
            self.push(component);
        }
    }

    /// Substitutes given prefix of path with new_prefix.
    /// self is ["p2", "p3"]
    /// prefix is ["p2"]
    /// new_prefix is ["p1"]
    /// result is ["p1", "p3"] (if p2 matches prefix of self)
    #[inline(always)]
    pub fn substitute_prefix(&self, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        match_prefix(self, prefix).map(|suffix| {
            let mut result = new_prefix.clone();
            result.extend_merging(suffix);
            result
        })
    }

    /// Same as substitute_prefix but only returns a new path if the suffix after prefix matching
    /// is non-empty. Also merges offsets like substitute_prefix.
    #[inline]
    pub fn substitute_prefix_with_nonempty_suffix(
        &self,
        prefix: &Path,
        new_prefix: &Path,
    ) -> Option<Path> {
        match_prefix(self, prefix)
            .filter(|s| !s.is_empty())
            .map(|suffix| {
                let mut result = new_prefix.clone();
                result.extend_merging(suffix);
                result
            })
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Heap {
    pub formal_index: FormalIndex,
    pub path: Path,
}

impl Heap {
    pub fn new(formal_index: FormalIndex) -> Self {
        Self {
            formal_index,
            path: Path::empty(),
        }
    }

    pub fn with_path(formal_index: FormalIndex, path: Path) -> Self {
        Self { formal_index, path }
    }

    pub fn index(&self) -> FormalIndex {
        self.formal_index
    }
}

/// A sequence of call sites representing a calling context.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(from = "Vec<PackedInsnSiteId>")]
pub struct CallString(ArcIntern<[PackedInsnSiteId]>);

impl From<Vec<PackedInsnSiteId>> for CallString {
    fn from(v: Vec<PackedInsnSiteId>) -> Self {
        Self(ArcIntern::from(v))
    }
}

impl Default for CallString {
    fn default() -> Self {
        Self::new()
    }
}

impl CallString {
    /// Creates an empty call string
    pub fn new() -> Self {
        Self(ArcIntern::from(Vec::new()))
    }

    /// Returns true if the call string is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of frames in the call string
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns the top frame (most recent call site)
    pub fn top(&self) -> Option<PackedInsnSiteId> {
        self.0.last().cloned()
    }

    /// Pops the top frame, returning the new call string and the popped frame
    pub fn pop(&self) -> (Self, Option<PackedInsnSiteId>) {
        if self.0.is_empty() {
            return (self.clone(), None);
        }
        let popped = self.0.last().cloned();
        let new_slice = &self.0[..self.0.len() - 1];
        (Self(ArcIntern::from(new_slice)), popped)
    }

    /// Pushes a new call site onto the call string.
    /// Returns None if a cycle is detected (i.e., the function is already in the call string).
    pub fn push(&self, site: PackedInsnSiteId) -> Option<Self> {
        let site_id = InsnSiteId::unpack_from_slice(&*site).ok()?;
        // Cycle detection: if the function ID of the call site is already present in the call string, do not push it.
        for existing_site in self.0.iter() {
            if let Ok(existing_site_id) = InsnSiteId::unpack_from_slice(&**existing_site)
                && existing_site_id.func_id == site_id.func_id
            {
                return None;
            }
        }
        let mut new_vec = self.0.to_vec();
        new_vec.push(site);
        Some(Self(ArcIntern::from(new_vec)))
    }

    /// Returns true if the call string contains the given call site
    pub fn contains(&self, site: &PackedInsnSiteId) -> bool {
        self.0.contains(site)
    }
}

impl Display for CallString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        for (i, site) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", site)?;
        }
        write!(f, "]")
    }
}

/// Parses a path string into components, handling dot prefixes and escaped dots
fn parse_path_string(s: &str) -> Vec<mir::FieldAccess> {
    let s = s.trim_start_matches('.'); // Remove leading dot if present
    if s.is_empty() {
        return Vec::new();
    }

    let mut path = Path::empty();
    let mut current_component = String::new();
    let mut chars = s.chars().peekable();

    // The iteration logic needs to advance the iterator inside the loop, so, skip the clippy
    // warning.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // Handle escaped character
            if let Some(next_ch) = chars.next() {
                // This is an escaped character - add it to the current component
                current_component.push(next_ch);
            }
        } else if ch == '[' {
            // Handle offset notation like [42]
            if !current_component.is_empty() {
                path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
                current_component = String::new();
            }
            let mut offset_str = String::new();
            #[allow(clippy::while_let_on_iterator)]
            while let Some(ch) = chars.next() {
                if ch == ']' {
                    if let Ok(offset) = offset_str.parse::<i64>() {
                        path.push(mir::FieldAccess::Offset(Offset(offset)));
                    }
                    break;
                }
                offset_str.push(ch);
            }
        } else if ch == '.' {
            // This is a separator dot - end of component
            if !current_component.is_empty() {
                path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
                current_component = String::new();
            }
        } else {
            current_component.push(ch);
        }
    }

    // Add the last component if it's not empty
    if !current_component.is_empty() {
        path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
    }

    path.0.into()
}

impl Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path({})", self.to_dot_string())
    }
}

impl From<&mir::FieldAccesses> for Path {
    #[inline]
    fn from(path: &mir::FieldAccesses) -> Self {
        let mut p = Path::empty();
        p.extend_merging(path.iter().cloned());
        p
    }
}

impl From<&[&str]> for Path {
    #[inline]
    fn from(path: &[&str]) -> Self {
        let mut p = Path::empty();
        p.extend_merging(
            path.iter()
                .map(|&fld| mir::FieldAccess::Symbol(ArcIntern::from(fld))),
        );
        p
    }
}

impl<S: AsRef<str>> FromIterator<S> for Path {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut p = Path::empty();
        p.extend_merging(
            iter.into_iter()
                .map(|fld| mir::FieldAccess::Symbol(ArcIntern::from(fld.as_ref()))),
        );
        p
    }
}

impl From<&str> for Path {
    fn from(s: &str) -> Self {
        let components = parse_path_string(s);
        Path(components.into())
    }
}

impl FromStr for Path {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let components = parse_path_string(s);
        Ok(Path(components.into()))
    }
}

/// Fully qualified ID of a function
#[repr(transparent)]
#[derive(Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Function(pub EltId);

impl Deref for Function {
    type Target = Str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Function(name) = self;
        write!(f, "function({name})")
    }
}

impl From<Str> for Function {
    #[inline]
    fn from(s: Str) -> Self {
        Function(s)
    }
}

impl FromStr for Function {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Function(s.into()))
    }
}

/// An index, like for formals.
#[derive(
    Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize,
)]
#[repr(transparent)]
pub struct Index(i16);

impl Index {
    #[inline]
    pub fn new(i: i16) -> Self {
        Self(i)
    }
}

impl Deref for Index {
    type Target = i16;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Display for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Index(i) = self;
        write!(f, "{i}")
    }
}

impl From<i8> for Index {
    #[inline]
    fn from(i: i8) -> Self {
        Self(i.into())
    }
}

impl From<i16> for Index {
    #[inline]
    fn from(i: i16) -> Self {
        Self(i)
    }
}

impl TryFrom<usize> for Index {
    type Error = Error;
    fn try_from(i: usize) -> Result<Self, Self::Error> {
        match i.try_into() {
            Ok(i) => Ok(Self(i)),
            Err(_) => Err(Error::FactsConvert("usize too lang for Index".to_string())),
        }
    }
}

/// Index into the parameter list. Negative indices are reserved for the engine
#[derive(
    Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Serialize, Deserialize, Default,
)]
#[repr(transparent)]
pub struct FormalIndex(Index);

impl FormalIndex {
    #[inline]
    pub fn new(i: Index) -> Self {
        Self(i)
    }
}

impl Deref for FormalIndex {
    type Target = i16;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Index> for FormalIndex {
    #[inline]
    fn from(i: Index) -> Self {
        Self(i)
    }
}

impl From<i8> for FormalIndex {
    #[inline]
    fn from(i: i8) -> Self {
        Self(i.into())
    }
}

impl From<i16> for FormalIndex {
    #[inline]
    fn from(i: i16) -> Self {
        Self(i.into())
    }
}

impl TryFrom<usize> for FormalIndex {
    type Error = Error;
    #[inline]
    fn try_from(i: usize) -> Result<Self, Self::Error> {
        match i.try_into() {
            Ok(i) => Ok(Self(i)),
            Err(_) => Err(Error::FactsConvert(
                "usize too big for FormalIndex".to_string(),
            )),
        }
    }
}

impl Display for FormalIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<mir::ParameterIdx> for FormalIndex {
    type Error = Error;
    fn try_from(p: mir::ParameterIdx) -> Result<FormalIndex, Self::Error> {
        match p.index().try_into() {
            Ok(i) => Ok(FormalIndex(i)),
            Err(_e) => Err(Error::FactsConvert(
                "ParameterIdx too big for FormalIndex".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackedInsnSiteId(pub [u8; 8]);

impl PackedInsnSiteId {
    pub fn try_from_parts(
        func_id: FunctionId,
        insn_id: InsnId,
    ) -> Result<Self, packed_struct::PackingError> {
        let site_id = InsnSiteId::new(func_id, insn_id);
        InsnSiteId::pack(&site_id).map(PackedInsnSiteId)
    }
}

impl Deref for PackedInsnSiteId {
    type Target = [u8; 8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<InsnSiteId> for PackedInsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: InsnSiteId) -> Result<PackedInsnSiteId, Self::Error> {
        InsnSiteId::pack(&site_id).map(PackedInsnSiteId)
    }
}

impl TryFrom<PackedInsnSiteId> for InsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: PackedInsnSiteId) -> Result<InsnSiteId, Self::Error> {
        InsnSiteId::unpack(&site_id)
    }
}

impl TryFrom<&PackedInsnSiteId> for InsnSiteId {
    type Error = packed_struct::PackingError;
    fn try_from(site_id: &PackedInsnSiteId) -> Result<InsnSiteId, Self::Error> {
        InsnSiteId::unpack(site_id)
    }
}

impl Display for PackedInsnSiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(site_id) = InsnSiteId::try_from(self) {
            write!(f, "{}:{}", site_id.func_id.id, site_id.insn_id.id)
        } else {
            write!(f, "packed({:?})", self.0)
        }
    }
}

/// An instruction site represents an instruction and the function in which it is contained. We use
/// a packed struct so we only need 64 bits for this information. The function id is stored in 28
/// bits; the instruction id is stored in the remaining 36 bits.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, PackedStruct)]
#[packed_struct(bit_numbering = "msb0", size_bytes = "8")]
pub struct InsnSiteId {
    #[packed_field(bits = "0..=27", endian = "msb")]
    pub func_id: FunctionId,
    #[packed_field(bits = "28..64", endian = "msb")]
    pub insn_id: InsnId,
}

impl InsnSiteId {
    pub fn new(function_id: FunctionId, insn_id: InsnId) -> Self {
        Self {
            func_id: function_id,
            insn_id,
        }
    }
}

/// A function ID. The packed bit-size of this has to be kept in sync with [`InsnSiteId`].
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    PackedStruct,
    Serialize,
    Deserialize,
    Default,
)]
#[packed_struct(bit_numbering = "msb0", size_bits = "28")]
#[repr(transparent)]
pub struct FunctionId {
    #[packed_field(bits = "0..28", endian = "msb")]
    pub id: u32,
}

impl FunctionId {
    pub fn new(id: u32) -> Self {
        FunctionId { id }
    }

    pub fn incr_assign(&mut self) {
        self.id += 1;
    }
}

/// An instruction ID. The packed bit-size of this has to be kept in sync with [`InsnSiteId`].
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    PackedStruct,
    Serialize,
    Deserialize,
    Default,
)]
#[packed_struct(bit_numbering = "msb0", size_bits = "36")]
#[repr(transparent)]
pub struct InsnId {
    #[packed_field(bits = "0..36", endian = "msb")]
    pub id: u64,
}

impl InsnId {
    pub fn new(id: u64) -> Self {
        InsnId { id }
    }

    pub fn incr_assign(&mut self) {
        self.id += 1;
    }
}

/// A variable with metadata that relates it to functions and call sites
#[derive(Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum FlowVariable {
    #[default]
    Uninit,
    Local(Str),
    Formal(FormalIndex),
    CallArg {
        id: PackedInsnSiteId,
        formal: FormalIndex,
    },
}

impl FlowVariable {
    pub fn formal(&self) -> Option<FormalIndex> {
        match self {
            FlowVariable::Formal(i) => Some(*i),
            _ => None,
        }
    }

    pub fn is_globals(&self) -> bool {
        crate::codegen::variable_is_globals(self)
    }
}

impl Display for FlowVariable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use FlowVariable::*;
        match self {
            Uninit => write!(f, "uninit"),
            Local(name) => write!(f, "local({name})"),
            Formal(index) => write!(f, "formal({index})"),
            CallArg { id, formal } => {
                let InsnSiteId { func_id, insn_id } = id.try_into().unwrap();
                write!(f, "call-arg({}:{}, {formal})", func_id.id, insn_id.id)
            }
        }
    }
}

impl TryFrom<mir::ParameterIdx> for FlowVariable {
    type Error = TryFromVariableError;
    #[inline]
    fn try_from(idx: mir::ParameterIdx) -> Result<FlowVariable, Self::Error> {
        match idx.try_into() {
            Ok(i) => Ok(FlowVariable::Formal(i)),
            Err(_) => Err(TryFromVariableError::Param),
        }
    }
}

#[derive(Error, Debug)]
pub enum TryFromVariableError {
    #[error("global variables unsupported in datalog")]
    Global,
    #[error("parameter doesn't fit")]
    Param,
}

impl TryFrom<&mir::Variable> for FlowVariable {
    type Error = TryFromVariableError;
    #[inline]
    fn try_from(v: &mir::Variable) -> Result<FlowVariable, Self::Error> {
        match v {
            mir::Variable::Local(_) => {
                let name = format!("{v}");
                Ok(FlowVariable::Local(ArcIntern::<str>::from(name)))
            }
            mir::Variable::Param(idx) => (*idx).try_into(),
            mir::Variable::GlobalHeap => Err(TryFromVariableError::Global),
        }
    }
}

impl TryFrom<&mir::VariableRef> for FlowVariable {
    type Error = TryFromVariableError;
    /// If the variable has no version, tries to convert from a variable. Otherwise, *formats* the
    /// variable and returns a local. Every version var, in other words, becomes a local.
    #[inline]
    fn try_from(v: &mir::VariableRef) -> Result<FlowVariable, Self::Error> {
        let mir::VariableRef { variable, version } = v;
        match version {
            None => variable.as_ref().try_into(),
            Some(version) => {
                let name = format!("{variable}_{version}");
                Ok(FlowVariable::Local(ArcIntern::<str>::from(name)))
            }
        }
    }
}

/// Variable and access path
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Serialize, Deserialize)]
pub struct FlowVertex(pub FlowVariable, pub Path);

impl Display for FlowVertex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let FlowVertex(var, path) = self;
        write!(f, "{var}{path}")
    }
}

/// This data type is used for enforcing call/return matching during taint analysis.
#[derive(
    Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize,
)]
pub enum TaintState {
    #[default]
    Free,
    Restricted,
}

/// Taint label
#[derive(Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Label(pub Str);

impl Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Label(name) = self;
        write!(f, "{name}")
    }
}

/// An endpoint for a query. In source-sink terms, sources are represented with a `TaintEndpoint`
/// with forward direction; sinks are represented with backward direction.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct TaintEndpoint {
    pub infunc: Function,
    pub vertex: FlowVertex,
    pub label: Label,
    pub direction: TaintDirection,
}

impl TaintEndpoint {
    pub fn reversed(&self) -> Self {
        TaintEndpoint {
            direction: self.direction.reversed(),
            ..self.clone()
        }
    }
}

impl Display for TaintEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let TaintEndpoint {
            infunc,
            vertex,
            label,
            direction,
        } = self;
        write!(
            f,
            "In '{infunc}' label '{label}'. Vertex '{vertex}' direction '{direction}'"
        )
    }
}

impl TaintDirection {
    pub fn reversed(&self) -> Self {
        match self {
            TaintDirection::Forward => TaintDirection::Backward,
            TaintDirection::Backward => TaintDirection::Forward,
        }
    }
}

impl Display for TaintDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaintDirection::Forward => write!(f, "forward"),
            TaintDirection::Backward => write!(f, "backward"),
        }
    }
}

/// Denotes the direction of program execution. Forward is the normal execution direction; backward
/// is the reverse.
#[derive(
    Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize, Copy,
)]
pub enum TaintDirection {
    #[default]
    Forward,
    Backward,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum FormalType {
    #[default]
    ByVal,
    ByRef,
}

impl From<mir::ParameterType> for FormalType {
    #[inline]
    fn from(ty: mir::ParameterType) -> Self {
        use mir::ParameterType::*;
        match ty {
            ByRef => FormalType::ByRef,
            ByVal => FormalType::ByVal,
        }
    }
}

/// Returns true if the formal flows input
#[inline(always)]
pub fn isin(formal: i64) -> bool {
    formal == -3 || formal >= 0
}

// /// Returns true if the formal flows output
// #[inline(always)]
// pub fn isout(formal: i64, ap: &Path) -> bool {
//     formal < 0 || *ap != *EMPTY_PATH
// }

#[inline(always)]
pub fn isout(formal_index: &FormalIndex, formal_type: FormalType, ap: &Path) -> bool {
    let i: i16 = **formal_index;
    if i >= 0 {
        match formal_type {
            FormalType::ByRef => true,
            FormalType::ByVal => !ap.is_empty(),
        }
    } else {
        true
    }
}

/// Returns the suffix solving the equation ap = prefix + suffix, if there is one. The suffix may
/// be empty. Otherwise returns none.
///
/// This supports offset arithmetic. For example, if ap = .x.[2] and prefix = .x.[1],
/// the suffix is .[1].
#[inline]
pub fn match_prefix(ap: &Path, prefix: &Path) -> Option<VecDeque<mir::FieldAccess>> {
    use mir::FieldAccess;
    use mir::Offset;
    let (ap_comps, prefix_comps) = (&ap.0, &prefix.0);

    if prefix_comps.is_empty() {
        return Some(ap_comps.clone());
    }

    if ap_comps.len() < prefix_comps.len() {
        return None;
    }

    // Check that all components except the last one match exactly
    for i in 0..prefix_comps.len() - 1 {
        if ap_comps[i] != prefix_comps[i] {
            return None;
        }
    }

    let last_idx = prefix_comps.len() - 1;
    match (&ap_comps[last_idx], &prefix_comps[last_idx]) {
        (FieldAccess::Offset(Offset(an)), FieldAccess::Offset(Offset(pn))) => {
            let mut suffix = VecDeque::new();
            let diff = an - pn;
            // Include an Offset in the suffix
            suffix.push_back(FieldAccess::Offset(Offset(diff)));
            // Append the remaining components of ap
            for comp in ap_comps.iter().skip(prefix_comps.len()) {
                suffix.push_back(comp.clone());
            }
            Some(suffix)
        }
        (a, p) if a == p => {
            // Exact match for the last prefix component
            Some(ap_comps.range(prefix_comps.len()..).cloned().collect())
        }
        _ => None,
    }
}

/// Keeps track of the mapping of intern'd function names to index ID's, which are generated at
/// index time. This is a helper when doing codegen.
#[derive(Default, Debug, Clone, Builder)]
pub struct IdMap {
    function_id: BTreeMap<Function, FunctionId>,
    functions: Vec<Function>,
}

impl IdMap {
    pub fn new() -> Self {
        Self {
            function_id: Default::default(),
            functions: Default::default(),
        }
    }

    /// Clears the function ID map and resets counters
    pub fn clear(&mut self) {
        self.function_id.clear();
        self.functions.clear();
    }

    pub fn get_id_to_name_map(&self) -> BTreeMap<u32, String> {
        self.functions
            .iter()
            .enumerate()
            .map(|(i, f)| (i as u32, f.0.to_string()))
            .collect()
    }

    pub fn try_save<P: AsRef<std::path::Path>>(self, path: P) -> Result<(), Error> {
        schema::function_id::try_save(
            path,
            self.functions
                .into_iter()
                .enumerate()
                .map(|(i, v)| (FunctionId::new(i as u32), v)),
        )
        .err_context(|| "saving IdMap")?;
        Ok(())
    }

    pub fn try_load<P: AsRef<std::path::Path>>(path: P) -> Result<Self, Error> {
        let function_id = schema::function_id::try_load(path).err_context(|| "loading IdMap")?;
        let functions: Vec<_> = function_id.iter().map(|(_, v)| v.clone()).collect();
        Ok(Self {
            function_id: function_id.into_iter().map(|(v, k)| (k, v)).collect(),
            functions,
        })
    }

    pub fn get_function_id(&self, f: Function) -> Option<FunctionId> {
        self.function_id.get(&f).copied()
    }

    pub fn get_function(&self, func_id: FunctionId) -> Option<&Function> {
        self.functions.get(func_id.id as usize)
    }

    pub fn functions(&self) -> impl Iterator<Item = (FunctionId, &Function)> {
        self.functions
            .iter()
            .enumerate()
            .map(|(i, f)| (FunctionId::new(i as u32), f))
    }

    /// Adds a function or returns the id previously assigned for the function.
    pub fn get_or_add_function(&mut self, f: Function) -> FunctionId {
        if let Some(id) = self.function_id.get(&f) {
            return *id;
        }
        let i = FunctionId::new(self.functions.len().try_into().unwrap());
        self.function_id.insert(f.clone(), i);
        self.functions.push(f);
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_prefix() {
        let p: Path = Path::empty();
        assert_eq!(p, p.substitute_prefix(&p, &p).unwrap());

        let p: Path = ["a", "c"].iter().collect();
        let q: Path = ["a"].iter().collect();
        let r: Path = ["b"].iter().collect();
        let e: Path = ["b", "c"].iter().collect();

        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p: Path = ["a", "b"].iter().collect();
        let q: Path = ["c", "d"].iter().collect();
        assert!(p.substitute_prefix(&q, &Path::empty()).is_none());

        // Test case: p23.substitute_prefix(p2, p1) where p23=.[1], p2='', p1=.[1] -> .[2]
        // This tests offset merging when matching empty prefix
        use ctadl_ir::mir::{FieldAccess, Offset};

        // Create p23 = .[1]
        let mut p23_components = VecDeque::new();
        p23_components.push_back(FieldAccess::Offset(Offset(1)));
        let p23 = Path(p23_components);

        // Create p2 = '' (empty path)
        let p2 = Path::empty();

        // Create p1 = .[1]
        let mut p1_components = VecDeque::new();
        p1_components.push_back(FieldAccess::Offset(Offset(1)));
        let p1 = Path(p1_components);

        let result = p23.substitute_prefix(&p2, &p1).unwrap();

        // Create expected = .[2]
        let mut expected_components = VecDeque::new();
        expected_components.push_back(FieldAccess::Offset(Offset(2)));
        let expected = Path(expected_components);

        assert_eq!(result, expected);

        // More offset arithmetic tests
        let p: Path = ".x.[2]".into();
        let q: Path = ".x.[1]".into();
        let r: Path = ".y".into();
        let e: Path = ".y.[1]".into();
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p: Path = ".x.[1].f".into();
        let q: Path = ".x".into();
        let r: Path = ".y".into();
        let e: Path = ".y.[1].f".into();
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());
    }

    #[test]
    fn test_is_prefix_of() {
        let p1: Path = ".x.[1]".into();
        let p2: Path = ".x.[2]".into();
        let p3: Path = ".x.[2].y".into();
        let p4: Path = ".y".into();

        assert!(p1.is_prefix_of(&p2));
        assert!(p1.is_prefix_of(&p3));
        assert!(p2.is_prefix_of(&p3));
        assert!(!p2.is_prefix_of(&p1));
        assert!(!p1.is_prefix_of(&p4));

        let empty = Path::empty();
        assert!(empty.is_prefix_of(&p1));
        assert!(p1.is_prefix_of(&p1));
        assert!(p3.is_prefix_of(&p3));
    }

    #[test]
    fn test_path_serialization() {
        let path: Path = ["foo", "bar.baz"].iter().collect();
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.bar\\.baz");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_dots() {
        let path: Path = ["foo.bar", "baz.qux"].iter().collect();
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo\\.bar.baz\\.qux");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_offsets() {
        // Test path with numeric offsets
        // Create a path manually with mixed FieldAccess types
        use ctadl_ir::mir::{FieldAccess, Offset};
        let mut path_components = VecDeque::new();
        path_components.push_back(FieldAccess::Symbol(ArcIntern::from("foo")));
        path_components.push_back(FieldAccess::Offset(Offset(42)));
        path_components.push_back(FieldAccess::Symbol(ArcIntern::from("bar")));
        let path = Path(path_components);

        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.[42].bar");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }
}
