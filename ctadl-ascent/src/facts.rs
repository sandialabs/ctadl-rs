//! Data types for facts

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display};
use std::ops::Deref;
use std::str::FromStr;

use ascent::lattice::Lattice;
use ctadl_ir::Symbol;
use derive_builder::Builder;
use immortal::StringRef;
use internment::ArcIntern;
use packed_struct::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::error::{Error, ErrorContext};
use ctadl_ir::{Idx, mir, mir::Offset};

pub mod parquet;
pub mod schema;

use elsa::sync::FrozenVec;
use std::collections::HashMap;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref STRING_TABLE: StringTable = StringTable::default();
}

#[derive(Default)]
struct StringTable {
    map: Mutex<HashMap<StringRef, u64>>,
    vec: FrozenVec<Box<StringRef>>,
}

impl StringTable {
    fn intern(&self, s: &str) -> u64 {
        let string_ref = StringRef::new(s);
        let mut map = self.map.lock().unwrap();
        if let Some(&idx) = map.get(&string_ref) {
            return idx;
        }
        let idx = self.vec.len() as u64;
        self.vec.push(Box::new(string_ref));
        map.insert(string_ref, idx);
        idx
    }

    fn get(&self, idx: u64) -> &str {
        self.vec.get(idx as usize).unwrap().as_str()
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Str(u64);

impl Default for Str {
    fn default() -> Self {
        *EMPTY_STR
    }
}

impl Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for Str {
    fn from(s: &str) -> Self {
        Str(STRING_TABLE.intern(s))
    }
}

impl From<String> for Str {
    fn from(s: String) -> Self {
        Str(STRING_TABLE.intern(&s))
    }
}

impl From<ArcIntern<str>> for Str {
    fn from(s: ArcIntern<str>) -> Self {
        Str(STRING_TABLE.intern(&s))
    }
}

impl Str {
    pub fn as_str(&self) -> &'static str {
        STRING_TABLE.get(self.0)
    }
}

impl Deref for Str {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

type EltId = Str;

lazy_static::lazy_static! {
    pub static ref EMPTY_STR: Str = Str::from("");
}

/// A sequence of field/array accesses
///
/// The access path is represented as a stack of accesses, innermost first. So an access path like
/// x.foo.bar.baz is represented as `cons(.foo, cons(.bar, cons(.baz)))`. This representation allows
/// common suffix sharing, something provided by the [`SuffixSeq`] datatype.
///
/// Access paths are composed of field and offset accesses. Contiguous runs of offset accesses are
/// summed, so there is never more than one offset access in a row. In effect, the offsets are
/// "addresses of" the containing field.
#[derive(
    Clone, Copy, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord,
)]
pub struct Path(pub tailshare::Seq<mir::FieldAccess>);

impl Path {
    /// Creates access path from a sequences of accesses
    pub fn from_accesses(iter: impl IntoIterator<Item = mir::FieldAccess>) -> Self {
        let mut items: Vec<mir::FieldAccess> = Vec::new();
        for item in iter {
            match (items.last_mut(), &item) {
                (Some(mir::FieldAccess::Offset(last_off)), mir::FieldAccess::Offset(new_off)) => {
                    last_off.0 += new_off.0;
                    // A net-zero offset is identity: by convention offset-0 is
                    // omitted from paths, so `.[k].[-k]` collapses to its base.
                    if last_off.0 == 0 {
                        items.pop();
                    }
                }
                // A standalone `.[0]` is likewise identity (e.g. left behind when
                // `substitute_prefix` fully consumes a matched offset). Dropping it
                // keeps `.[0].deref` equivalent to `.deref` so taint crossing a
                // stack-slot binding (`formal <-> __stack_top.[off]`) lands on
                // `formal.deref`, matching summary edges and sink checks.
                (_, mir::FieldAccess::Offset(off)) if off.0 == 0 => {}
                _ => items.push(item),
            }
        }
        Path(items.into_iter().collect())
    }

    /// Creates an empty path
    #[inline]
    pub fn empty() -> Self {
        Path(tailshare::Seq::new())
    }

    /// Denotes the empty path
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Len is O(n)
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

    /// Iterates from the innermost access out
    pub fn iter(&self) -> tailshare::Iter<mir::FieldAccess> {
        self.0.iter()
    }

    /// Concatenates self onto other, returning new path
    pub fn concat(&self, other: &Self) -> Self {
        Self::from_accesses(self.iter().chain(other.iter()).cloned())
    }

    /// If the path begins with an [`Offset`](mir::FieldAccess::Offset) component,
    /// returns the path with that leading offset removed; otherwise `None`.
    ///
    /// Used to model offset-insensitive loads: a read `v.[k]..rest` (element `k`
    /// of a buffer/array) should inherit taint from the base's offset-0 view
    /// `v..rest`, so a tainted buffer base taints every element read off it
    /// (e.g. `argv[i]` inherits `argv`'s taint).
    #[inline]
    pub fn strip_leading_offset(&self) -> Option<Path> {
        match self.0.head() {
            Some(mir::FieldAccess::Offset(_)) => Some(Path(self.0.tail().unwrap_or_default())),
            _ => None,
        }
    }

    /// Substitutes given prefix of path with new_prefix and returns the new path.
    #[inline(always)]
    pub fn substitute_prefix(&self, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        match_prefix(self, prefix).map(|suffix| {
            let iter = new_prefix.0.iter().chain(suffix.iter()).cloned();
            Path::from_accesses(iter)
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
                let iter = new_prefix.0.iter().chain(suffix.iter()).cloned();
                Path::from_accesses(iter)
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

immortal::immortal! {
    /// A sequence of call sites representing a calling context.
    #[derive(Serialize, Deserialize)]
    #[serde(from = "Vec<PackedInsnSiteId>")]
    pub struct CallString([PackedInsnSiteId])
}

impl From<Vec<PackedInsnSiteId>> for CallString {
    fn from(v: Vec<PackedInsnSiteId>) -> Self {
        Self::intern(&v)
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
        CallString::intern(&[])
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
            return (*self, None);
        }
        let popped = self.0.last().cloned();
        let new_slice = &self.0[..self.0.len() - 1];
        (CallString::intern(new_slice), popped)
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
        Some(CallString::intern(&new_vec))
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

/// Compares two call strings by *lattice height* for [`SmallestCallString`].
///
/// Returns [`Ordering::Greater`] when `a` is the higher (preferred) element — i.e.
/// when `a` is the "smaller" call string. Smaller means shorter; among equal
/// lengths the lexicographically smaller string is higher. (`CallString`'s derived
/// `Ord` is the lexicographic order over its frames, used here for the tie-break.)
fn call_string_height_cmp(a: &CallString, b: &CallString) -> std::cmp::Ordering {
    // Shorter `a` => a.len() < b.len() => b.len().cmp(a.len()) == Greater => `a` higher.
    // Equal length, lex-smaller `a` => a < b => b.cmp(a) == Greater => `a` higher.
    b.len().cmp(&a.len()).then_with(|| b.cmp(a))
}

/// A lattice over [`CallString`] that joins toward the *smallest* call string.
///
/// Unlike [`crate::lattice::Consistent`] — which sticks to the first value seen for
/// a key — this lattice always moves toward the smaller call string: shorter is
/// higher, and among equal lengths the lexicographically smaller is higher. The
/// empty call string (length 0) is therefore the top element, so a key converges to
/// the empty / fully-resolved context whenever it is derivable. This guarantees the
/// `cs.is_empty()` feedback (e.g. index_engine rule 3.5) is never missed merely
/// because some longer call string happened to be recorded first.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SmallestCallString {
    /// No value yet — the lattice bottom and the identity for `join`.
    #[default]
    Bottom,
    /// A call string. Higher elements hold smaller call strings.
    Value(CallString),
}

impl SmallestCallString {
    /// Returns the call string, if any.
    pub fn value(&self) -> Option<&CallString> {
        match self {
            SmallestCallString::Value(cs) => Some(cs),
            SmallestCallString::Bottom => None,
        }
    }

    /// Empty call string is top
    pub fn top() -> Self {
        SmallestCallString::Value(CallString::new())
    }
}

impl PartialOrd for SmallestCallString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmallestCallString {
    /// `Bottom` is the least element; among values, the smaller call string is the
    /// greater (higher) element. This is a total order, consistent with the
    /// pointer-based `Eq` because interning makes equal slices pointer-equal.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (SmallestCallString::Bottom, SmallestCallString::Bottom) => Ordering::Equal,
            (SmallestCallString::Bottom, SmallestCallString::Value(_)) => Ordering::Less,
            (SmallestCallString::Value(_), SmallestCallString::Bottom) => Ordering::Greater,
            (SmallestCallString::Value(a), SmallestCallString::Value(b)) => {
                call_string_height_cmp(a, b)
            }
        }
    }
}

impl Lattice for SmallestCallString {
    /// Least upper bound: moves UP toward the smaller call string. `Bottom` is the
    /// identity; among values the smaller call string wins.
    fn join_mut(&mut self, other: Self) -> bool {
        if other > *self {
            *self = other;
            true
        } else {
            false
        }
    }

    /// Greatest lower bound: moves DOWN toward the larger call string / `Bottom`.
    fn meet_mut(&mut self, other: Self) -> bool {
        if other < *self {
            *self = other;
            true
        } else {
            false
        }
    }
}

/// Parses a path string into components, handling dot prefixes and escaped dots
fn parse_path_string(s: &str) -> Path {
    let s = s.trim_start_matches('.'); // Remove leading dot if present
    if s.is_empty() {
        return Path::empty();
    }

    let mut accesses = Vec::new();
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
                accesses.push(mir::FieldAccess::Symbol(ArcIntern::from(
                    current_component.clone(),
                )));
                current_component.clear();
            }
            let mut offset_str = String::new();
            #[allow(clippy::while_let_on_iterator)]
            while let Some(ch) = chars.next() {
                if ch == ']' {
                    if let Ok(offset) = offset_str.parse::<i64>() {
                        accesses.push(mir::FieldAccess::Offset(Offset(offset)));
                    }
                    break;
                }
                offset_str.push(ch);
            }
        } else if ch == '.' {
            // This is a separator dot - end of component
            if !current_component.is_empty() {
                accesses.push(mir::FieldAccess::Symbol(ArcIntern::from(
                    current_component.clone(),
                )));
                current_component.clear();
            }
        } else {
            current_component.push(ch);
        }
    }

    // Add the last component if it's not empty
    if !current_component.is_empty() {
        accesses.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
    }

    Path::from_accesses(accesses)
}

impl Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path({})", self.to_dot_string())
    }
}

impl From<&mir::FieldAccesses> for Path {
    #[inline]
    fn from(path: &mir::FieldAccesses) -> Self {
        Self::from_accesses(path.iter().cloned())
    }
}

impl From<&[&str]> for Path {
    #[inline]
    fn from(path: &[&str]) -> Self {
        Self::from_accesses(
            path.iter()
                .map(|&fld| mir::FieldAccess::Symbol(ArcIntern::from(fld))),
        )
    }
}

impl<S: AsRef<str>> FromIterator<S> for Path {
    #[inline]
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self::from_accesses(
            iter.into_iter()
                .map(|fld| mir::FieldAccess::Symbol(ArcIntern::from(fld.as_ref()))),
        )
    }
}

impl From<&str> for Path {
    fn from(s: &str) -> Self {
        parse_path_string(s)
    }
}

impl FromStr for Path {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let p = parse_path_string(s);
        Ok(p)
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

impl From<ArcIntern<str>> for Function {
    #[inline]
    fn from(s: ArcIntern<str>) -> Self {
        Function(Str::from(s))
    }
}

impl FromStr for Function {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Function(s.into()))
    }
}

/// Index into the parameter list. Negative indices are reserved for the engine
#[derive(
    Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Serialize, Deserialize, Default,
)]
#[repr(transparent)]
pub struct FormalIndex(i16);

impl FormalIndex {
    #[inline]
    pub fn new(i: i16) -> Self {
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

impl From<i8> for FormalIndex {
    #[inline]
    fn from(i: i8) -> Self {
        Self(i.into())
    }
}

impl From<i16> for FormalIndex {
    #[inline]
    fn from(i: i16) -> Self {
        Self(i)
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackedCallArg(pub [u8; 8]);

impl PackedCallArg {
    pub fn try_from_parts(
        insn_id: InsnId,
        formal: FormalIndex,
    ) -> Result<Self, packed_struct::PackingError> {
        let call_arg = CallArgId::new(insn_id, formal);
        CallArgId::pack(&call_arg).map(PackedCallArg)
    }
}

impl Deref for PackedCallArg {
    type Target = [u8; 8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<CallArgId> for PackedCallArg {
    type Error = packed_struct::PackingError;
    fn try_from(call_arg: CallArgId) -> Result<PackedCallArg, Self::Error> {
        CallArgId::pack(&call_arg).map(PackedCallArg)
    }
}

impl TryFrom<PackedCallArg> for CallArgId {
    type Error = packed_struct::PackingError;
    fn try_from(packed: PackedCallArg) -> Result<CallArgId, Self::Error> {
        CallArgId::unpack(&packed)
    }
}

impl TryFrom<&PackedCallArg> for CallArgId {
    type Error = packed_struct::PackingError;
    fn try_from(packed: &PackedCallArg) -> Result<CallArgId, Self::Error> {
        CallArgId::unpack(packed)
    }
}

impl Display for PackedCallArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(call_arg) = CallArgId::try_from(self) {
            write!(f, "{}:{}", call_arg.insn_id.id, call_arg.formal)
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

/// A call argument represents an instruction and the formal index of the argument. We use a packed
/// struct so we only need 64 bits for this information. The instruction id is stored in 36 bits;
/// the formal index is stored in 16 bits.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, PackedStruct)]
#[packed_struct(bit_numbering = "msb0", size_bytes = "8")]
pub struct CallArgId {
    #[packed_field(bits = "0..36", endian = "msb")]
    pub insn_id: InsnId,
    #[packed_field(bits = "36..52", endian = "msb")]
    pub formal: i16,
}

impl CallArgId {
    pub fn new(insn_id: InsnId, formal: FormalIndex) -> Self {
        Self {
            insn_id,
            formal: *formal,
        }
    }

    pub fn formal(&self) -> FormalIndex {
        FormalIndex::from(self.formal)
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
///
/// Flow variables are crafted to be 8 bytes. There are four variants, requiring 2 bits to
/// represent. The Local variable is represented with a string ID, a compact index into a string
/// table, which will be under 62 bits. The Formal is an i16. And the Packed Call Arg similarly only
/// requires 62 bits.
#[derive(Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Default)]
pub struct FlowVariable(u64);

const TAG_UNINIT: u64 = 0 << 62;
const TAG_LOCAL: u64 = 1 << 62;
const TAG_FORMAL: u64 = 2 << 62;
const TAG_CALL_ARG: u64 = 3 << 62;
const TAG_MASK: u64 = 3 << 62;
const DATA_MASK: u64 = !TAG_MASK;

#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub enum FlowVariableKind {
    Uninit,
    Local(Str),
    Formal(FormalIndex),
    CallArg(PackedCallArg),
}

impl From<FlowVariableKind> for FlowVariable {
    fn from(kind: FlowVariableKind) -> Self {
        match kind {
            FlowVariableKind::Uninit => FlowVariable(TAG_UNINIT),
            FlowVariableKind::Local(s) => {
                let val = s.0;
                debug_assert!((val & TAG_MASK) == 0, "Local top bits must be 0");
                FlowVariable(val | TAG_LOCAL)
            }
            FlowVariableKind::Formal(f) => {
                let val = (*f as u16) as u64;
                FlowVariable(val | TAG_FORMAL)
            }
            FlowVariableKind::CallArg(c) => {
                let val = u64::from_be_bytes(c.0);
                debug_assert!((val & TAG_MASK) == 0, "CallArg top bits must be 0");
                FlowVariable(val | TAG_CALL_ARG)
            }
        }
    }
}

impl FlowVariable {
    pub fn local(s: Str) -> Self {
        FlowVariableKind::Local(s).into()
    }

    pub fn formal_index(f: FormalIndex) -> Self {
        FlowVariableKind::Formal(f).into()
    }

    pub fn call_arg_packed(c: PackedCallArg) -> Self {
        FlowVariableKind::CallArg(c).into()
    }

    pub fn as_local(&self) -> Option<Str> {
        if self.is_local() {
            let idx = self.0 & DATA_MASK;
            Some(Str(idx))
        } else {
            None
        }
    }

    pub fn as_formal(&self) -> Option<FormalIndex> {
        self.formal()
    }

    pub fn as_call_arg(&self) -> Option<PackedCallArg> {
        if self.is_call_arg() {
            let val = self.0 & DATA_MASK;
            Some(PackedCallArg(val.to_be_bytes()))
        } else {
            None
        }
    }

    pub fn kind(&self) -> FlowVariableKind {
        match self.0 & TAG_MASK {
            TAG_UNINIT => FlowVariableKind::Uninit,
            TAG_LOCAL => {
                let idx = self.0 & DATA_MASK;
                FlowVariableKind::Local(Str(idx))
            }
            TAG_FORMAL => {
                let val = (self.0 & DATA_MASK) as i16;
                FlowVariableKind::Formal(FormalIndex::from(val))
            }
            TAG_CALL_ARG => {
                let val = self.0 & DATA_MASK;
                FlowVariableKind::CallArg(PackedCallArg(val.to_be_bytes()))
            }
            _ => unreachable!(),
        }
    }

    pub fn is_uninit(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_UNINIT
    }

    pub fn is_local(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_LOCAL
    }

    pub fn is_formal(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_FORMAL
    }

    pub fn is_call_arg(&self) -> bool {
        (self.0 & TAG_MASK) == TAG_CALL_ARG
    }
}

impl Debug for FlowVariable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind())
    }
}

impl Serialize for FlowVariable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.kind().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlowVariable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let kind = FlowVariableKind::deserialize(deserializer)?;
        Ok(FlowVariable::from(kind))
    }
}

impl FlowVariable {
    pub fn formal(&self) -> Option<FormalIndex> {
        if self.is_formal() {
            let val = (self.0 & DATA_MASK) as i16;
            Some(FormalIndex::from(val))
        } else {
            None
        }
    }

    pub fn is_globals(&self) -> bool {
        crate::codegen::variable_is_globals(self)
    }
}

impl Display for FlowVariable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            FlowVariableKind::Uninit => write!(f, "uninit"),
            FlowVariableKind::Local(name) => write!(f, "local({name})"),
            FlowVariableKind::Formal(index) => write!(f, "formal({index})"),
            FlowVariableKind::CallArg(packed) => {
                let CallArgId { insn_id, formal } = packed.try_into().unwrap();
                write!(f, "call-arg({}, {formal})", insn_id.id)
            }
        }
    }
}

impl TryFrom<mir::ParameterIdx> for FlowVariable {
    type Error = TryFromVariableError;
    #[inline]
    fn try_from(idx: mir::ParameterIdx) -> Result<FlowVariable, Self::Error> {
        match idx.try_into() {
            Ok(i) => Ok(FlowVariableKind::Formal(i).into()),
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
                Ok(FlowVariableKind::Local(Str::from(name)).into())
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
                Ok(FlowVariableKind::Local(Str::from(name)).into())
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

/// Resolvents represent target information for a virtual function call or indirect call. They con
/// be either a function ID or an object. The function ID can be used directly. Typically the object
/// is used in conjunction with virtual method table information to resolve the call.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Resolvent {
    #[default]
    Unresolved,
    Function(FunctionId),
    Object(Symbol),
}

impl Display for Resolvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resolvent::Unresolved => write!(f, "unresolved"),
            Resolvent::Function(func_id) => write!(f, "function({})", func_id.id),
            Resolvent::Object(cls) => write!(f, "object({cls})"),
        }
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

/// The kind of a taint-graph edge, in execution / data-flow order.
///
/// An `Intra` edge is a flow-insensitive intraprocedural step (assign/alias) and
/// carries no instruction. `Call` and `Return` are interprocedural steps anchored
/// at the call instruction they cross: `Call` runs from an actual argument in the
/// caller into the corresponding formal of the callee, and `Return` runs from a
/// callee formal/return back out to the caller's actual. The anchoring site is
/// what lets a path step be attributed to *this* call rather than guessed from the
/// variable alone, and the call/return tags are what a call/return-matched
/// (realizable-path) graph search consumes.
#[derive(Clone, Copy, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default)]
pub enum FlowEdge {
    #[default]
    Intra,
    Call(PackedInsnSiteId),
    Return(PackedInsnSiteId),
}

impl FlowEdge {
    /// The call instruction anchoring this edge, or `None` for an [`Intra`](FlowEdge::Intra) edge.
    #[inline]
    pub fn site(&self) -> Option<PackedInsnSiteId> {
        match self {
            FlowEdge::Intra => None,
            FlowEdge::Call(s) | FlowEdge::Return(s) => Some(*s),
        }
    }
}

impl Display for FlowEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowEdge::Intra => write!(f, "intra"),
            FlowEdge::Call(s) => write!(f, "call({s})"),
            FlowEdge::Return(s) => write!(f, "return({s})"),
        }
    }
}

/// Taint label
#[derive(Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Label(pub Str);

impl From<Str> for Label {
    #[inline]
    fn from(s: Str) -> Self {
        Label(s)
    }
}

impl From<ArcIntern<str>> for Label {
    #[inline]
    fn from(s: ArcIntern<str>) -> Self {
        Label(Str::from(s))
    }
}

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

/// Returns true if the formal flows output
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
pub fn match_prefix(ap: &Path, prefix: &Path) -> Option<tailshare::Seq<mir::FieldAccess>> {
    use mir::{FieldAccess, Offset};
    let mut ap_seq = ap.0;
    let mut prefix_seq = prefix.0;

    if prefix_seq.is_empty() {
        return Some(ap_seq);
    }

    // Check that all components except the last one match exactly
    // Invariant: prefix_seq.tail() is a non-empty seq
    while prefix_seq.tail().unwrap_or_default().head().is_some() {
        if prefix_seq.head() != ap_seq.head() {
            return None;
        }
        prefix_seq = prefix_seq.tail().unwrap();
        ap_seq = ap_seq.tail().unwrap_or_default();
    }
    // unwrap() safety follows from while condition
    let prefix_last = prefix_seq.head().unwrap();

    // There's one prefix component left. If there are no more ap components, we fail to match.
    let ap_last = ap_seq.head()?;

    match (ap_last, prefix_last) {
        // The last offsets match with an offset adjustment
        (FieldAccess::Offset(Offset(an)), FieldAccess::Offset(Offset(pn))) => {
            let adjust = an - pn;
            Some(ap_seq.map_head(|_| FieldAccess::Offset(Offset(adjust))))
        }
        (a, p) if a == p => {
            // Exact match for the last prefix component
            Some(ap_seq.tail().unwrap())
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
        let p23 = Path::from_accesses([FieldAccess::Offset(Offset(1))]);

        // Create p2 = '' (empty path)
        let p2 = Path::empty();

        // Create p1 = .[1]
        let p1 = Path::from_accesses([FieldAccess::Offset(Offset(1))]);

        let result = p23.substitute_prefix(&p2, &p1).unwrap();

        // Create expected = .[2]
        let expected = Path::from_accesses([FieldAccess::Offset(Offset(2))]);

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
    fn test_zero_offset_normalized_away() {
        use ctadl_ir::mir::{FieldAccess, Offset};

        // A standalone `.[0]` is identity and must not appear in a path.
        let deref: Path = ".deref".into();
        let with_zero = Path::from_accesses([FieldAccess::Offset(Offset(0))]).concat(&deref);
        assert_eq!(with_zero, deref);

        // Offsets that net to zero collapse to their base (here, the empty path).
        let net_zero = Path::from_accesses([
            FieldAccess::Offset(Offset(8)),
            FieldAccess::Offset(Offset(-8)),
        ]);
        assert!(net_zero.is_empty());

        // The leak that blocked stack-buffer flows: fully consuming a matched
        // offset must yield `.deref`, not `.[0].deref`, so a summary edge written
        // on `formal.deref` fires after taint crosses a `formal <-> base.[off]`
        // binding.
        let taint_path: Path = ".[-1304].deref".into();
        let binding_src: Path = ".[-1304]".into();
        let formal_view = taint_path
            .substitute_prefix(&binding_src, &Path::empty())
            .unwrap();
        assert_eq!(formal_view, deref);

        // Non-zero residual offsets are preserved (sub-buffer addressing).
        let p: Path = ".[16].deref".into();
        let prefix: Path = ".[8]".into();
        assert_eq!(
            p.substitute_prefix(&prefix, &Path::empty()).unwrap(),
            ".[8].deref".into()
        );
    }

    #[test]
    fn test_substitute_prefix_comprehensive() {
        // 1. Several offsets
        let p: Path = ".x.[30]".into();
        let q: Path = ".x.[10]".into();
        let r: Path = ".y.[2]".into();
        let expected: Path = ".y.[22]".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p: Path = ".a.[100].b.[50]".into();
        let q: Path = ".a.[60]".into();
        let r: Path = ".c.[10]".into();
        let expected: Path = ".c.[50].b.[50]".into(); // .c.[10] + ([100]-[60]) = .c.[50]
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 2. Empty prefix
        let p: Path = ".x.[10]".into();
        let q: Path = Path::empty();
        let r: Path = ".y.[5]".into();
        let expected: Path = ".y.[5].x.[10]".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p: Path = ".[10]".into();
        let q: Path = Path::empty();
        let r: Path = ".[5]".into();
        let expected: Path = ".[15]".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 3. Empty new_prefix
        let p: Path = ".x.y".into();
        let q: Path = ".x".into();
        let r: Path = Path::empty();
        let expected: Path = ".y".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p: Path = ".x.[10]".into();
        let q: Path = ".x".into();
        let r: Path = Path::empty();
        let expected: Path = ".[10]".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        // 4. Empty path
        let p: Path = Path::empty();
        let q: Path = Path::empty();
        let r: Path = ".x.[10]".into();
        let expected: Path = ".x.[10]".into();
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));

        let p: Path = Path::empty();
        let q: Path = ".x".into();
        let r: Path = ".y".into();
        assert_eq!(p.substitute_prefix(&q, &r), None);

        // Additional cases: Negative offsets
        let p: Path = ".[10]".into();
        let q: Path = ".[20]".into();
        let r: Path = ".[5]".into();
        let expected: Path = ".[-5]".into(); // [5] + ([10] - [20]) = [5] - [10] = [-5]
        assert_eq!(p.substitute_prefix(&q, &r), Some(expected));
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
        let path = Path::from_accesses([
            FieldAccess::Symbol(ArcIntern::from("foo")),
            FieldAccess::Offset(Offset(42)),
            FieldAccess::Symbol(ArcIntern::from("bar")),
        ]);

        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.[42].bar");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_call_string_interning() {
        let cs1 = CallString::new();
        let cs2 = CallString::new();
        assert_eq!(cs1, cs2);
        assert!(std::ptr::eq(cs1.0, cs2.0));

        let site = PackedInsnSiteId([1, 2, 3, 4, 5, 6, 7, 8]);
        // We need a valid site for push to work because of cycle detection and unpacking
        let cs3 = cs1.push(site).expect("push failed");
        let cs4 = cs2.push(site).expect("push failed");
        assert_eq!(cs3, cs4);
        assert!(std::ptr::eq(cs3.0, cs4.0));

        let (cs5, popped) = cs3.pop();
        assert_eq!(cs5, cs1);
        assert!(std::ptr::eq(cs5.0, cs1.0));
        assert_eq!(popped, Some(site));
    }
}
