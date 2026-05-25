//! Data types for facts

use std::collections::BTreeMap;
use std::ops::Deref;
use std::str::FromStr;
use std::{fmt, fmt::Display};

use derive_builder::Builder;
use internment::ArcIntern;
use packed_struct::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::{Error, ErrorContext};
pub use crate::path::{AccessPathSet, Path, match_prefix, parse_path_string};
use ascent::lattice::Lattice;
use ctadl_ir::{Idx, mir};
use trie::Trie;

pub mod parquet;
pub mod schema;

pub type Str = ArcIntern<str>;
type EltId = Str;

lazy_static::lazy_static! {
    pub static ref EMPTY_STR: Str = ArcIntern::<str>::from("");
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

/// A bidirectional map representing reachability between (Variable, Path) pairs.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct AccessPathMap {
    pub forward:
        BTreeMap<FlowVariable, Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>>,
    pub backward:
        BTreeMap<FlowVariable, Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>>,
}

impl PartialOrd for AccessPathMap {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self == other {
            return Some(std::cmp::Ordering::Equal);
        }

        let mut self_le_other = true;
        for (v1, trie1) in &self.forward {
            let trie2 = match other.forward.get(v1) {
                Some(t) => t,
                None => {
                    self_le_other = false;
                    break;
                }
            };
            if !trie_le(trie1, trie2) {
                self_le_other = false;
                break;
            }
        }

        let mut other_le_self = true;
        for (v1, trie2) in &other.forward {
            let trie1 = match self.forward.get(v1) {
                Some(t) => t,
                None => {
                    other_le_self = false;
                    break;
                }
            };
            if !trie_le(trie2, trie1) {
                other_le_self = false;
                break;
            }
        }

        match (self_le_other, other_le_self) {
            (true, true) => Some(std::cmp::Ordering::Equal),
            (true, false) => Some(std::cmp::Ordering::Less),
            (false, true) => Some(std::cmp::Ordering::Greater),
            (false, false) => None,
        }
    }
}

fn trie_le(
    t1: &Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>,
    t2: &Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>,
) -> bool {
    for (seq, map1) in t1.all_sequences() {
        let map2 = match t2.get(&seq) {
            Some(m) => m,
            None => return false,
        };
        for (v2, set1) in map1 {
            let set2 = match map2.get(&v2) {
                Some(s) => s,
                None => return false,
            };
            if set1.partial_cmp(set2) == Some(std::cmp::Ordering::Greater)
                || set1.partial_cmp(set2).is_none()
            {
                return false;
            }
        }
    }
    true
}

impl AccessPathMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    pub fn singleton(v1: FlowVariable, p1: Path, v2: FlowVariable, p2: Path) -> Self {
        let mut res = Self::new();
        res.insert(v1, p1, v2, p2);
        res
    }

    pub fn insert(&mut self, v1: FlowVariable, p1: Path, v2: FlowVariable, p2: Path) {
        // Forward: v1.p1 -> {v2.p2}
        let trie_f = self.forward.entry(v1.clone()).or_default();
        let mut map_f = trie_f.get(&p1.0).cloned().unwrap_or_default();
        map_f.entry(v2.clone()).or_default().insert(&p2);
        trie_f.insert(p1.0.iter().cloned(), map_f);

        // Backward: v2.p2 -> {v1.p1}
        let trie_b = self.backward.entry(v2).or_default();
        let mut map_b = trie_b.get(&p2.0).cloned().unwrap_or_default();
        map_b.entry(v1).or_default().insert(&p1);
        trie_b.insert(p2.0.iter().cloned(), map_b);
    }

    pub fn union(&mut self, other: &Self) {
        if self == other {
            return;
        }
        for (v1, trie_other) in &other.forward {
            let trie_self = self.forward.entry(v1.clone()).or_default();
            for (seq, map_other) in trie_other.all_sequences() {
                let mut map_self = trie_self.get(&seq).cloned().unwrap_or_default();
                for (v2, set_other) in map_other {
                    map_self.entry(v2.clone()).or_default().union(&set_other);
                }
                trie_self.insert(seq, map_self);
            }
        }
        for (v2, trie_other) in &other.backward {
            let trie_self = self.backward.entry(v2.clone()).or_default();
            for (seq, map_other) in trie_other.all_sequences() {
                let mut map_self = trie_self.get(&seq).cloned().unwrap_or_default();
                for (v1, set_other) in map_other {
                    map_self.entry(v1.clone()).or_default().union(&set_other);
                }
                trie_self.insert(seq, map_self);
            }
        }
    }

    pub fn reached_from(
        &self,
        v: &FlowVariable,
    ) -> Option<&Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>> {
        self.forward.get(v)
    }

    pub fn reached_to(
        &self,
        v: &FlowVariable,
    ) -> Option<&Trie<mir::FieldAccess, BTreeMap<FlowVariable, AccessPathSet>>> {
        self.backward.get(v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (FlowVariable, Path, FlowVariable, Path)> + '_ {
        self.forward.iter().flat_map(|(v1, trie)| {
            trie.all_sequences()
                .into_iter()
                .flat_map(move |(seq1, map)| {
                    let p1 = Path(seq1);
                    map.into_iter().flat_map(move |(v2, set)| {
                        let v1 = v1.clone();
                        let v2 = v2.clone();
                        let p1 = p1.clone();
                        set.iter()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .map(move |p2| (v1.clone(), p1.clone(), v2.clone(), p2))
                    })
                })
        })
    }

    pub fn propagate_assignment(
        &self,
        v_src: &FlowVariable,
        p_src: &Path,
        v_dst: &FlowVariable,
        p_dst: &Path,
        valid_paths: &AccessPathSet,
    ) -> Option<Self> {
        let trie_src = self.forward.get(v_src)?;
        if let Some(new_trie_for_dst) = trie_src.substitute_prefix(&p_src.0, p_dst.0.clone()) {
            let filtered_trie =
                new_trie_for_dst.filter(|seq, _map| valid_paths.contains_accesses(seq));
            if filtered_trie.is_empty() {
                return None;
            }

            let mut res = Self::new();
            for (seq, map) in filtered_trie.all_sequences() {
                let p_new = Path(seq);
                for (v_tgt, set_tgt) in map {
                    for p_tgt in set_tgt.iter() {
                        res.insert(v_dst.clone(), p_new.clone(), v_tgt.clone(), p_tgt);
                    }
                }
            }
            Some(res)
        } else {
            None
        }
    }

    pub fn propagate_assignment_reverse(
        &self,
        v_src: &FlowVariable,
        p_src_long: &Path,
        v_dst: &FlowVariable,
        p_dst: &Path,
        valid_paths: &AccessPathSet,
    ) -> Option<Self> {
        let trie_src = self.forward.get(v_src)?;
        let mut res = Self::new();
        let mut found = false;

        for (p_src_short_seq, map) in trie_src.all_sequences() {
            let p_src_short = Path(p_src_short_seq);
            // If p_src_short is a prefix of p_src_long
            if let Some(suffix) = match_prefix(p_src_long, &p_src_short) {
                for (v_tgt, set_tgt) in map {
                    for p_tgt in set_tgt.iter() {
                        let mut p_tgt_new = p_tgt.clone();
                        p_tgt_new.extend_merging(suffix.iter().cloned());
                        if valid_paths.contains(&p_tgt_new) {
                            found = true;
                            res.insert(v_dst.clone(), p_dst.clone(), v_tgt.clone(), p_tgt_new);
                        }
                    }
                }
            }
        }

        if found { Some(res) } else { None }
    }
}

impl Lattice for AccessPathMap {
    fn meet(self, other: Self) -> Self {
        let mut res = Self::new();
        for (v1, trie1) in self.forward {
            if let Some(trie2) = other.forward.get(&v1) {
                let mut trie_met = Trie::new();
                for (seq, map1) in trie1.all_sequences() {
                    if let Some(map2) = trie2.get(&seq) {
                        let mut map_met = BTreeMap::new();
                        for (v2, set1) in map1 {
                            if let Some(set2) = map2.get(&v2) {
                                let set_met = set1.meet(set2.clone());
                                if !set_met.is_empty() {
                                    map_met.insert(v2, set_met);
                                }
                            }
                        }
                        if !map_met.is_empty() {
                            trie_met.insert(seq, map_met);
                        }
                    }
                }
                if !trie_met.is_empty() {
                    res.forward.insert(v1, trie_met);
                }
            }
        }
        // Sync backward
        for (v1, trie) in &res.forward {
            for (seq, map) in trie.all_sequences() {
                let p1 = Path(seq);
                for (v2, set) in map {
                    for p2 in set.iter() {
                        let trie_b = res.backward.entry(v2.clone()).or_default();
                        let mut map_b = trie_b.get(&p2.0).cloned().unwrap_or_default();
                        map_b.entry(v1.clone()).or_default().insert(&p1);
                        trie_b.insert(p2.0.iter().cloned(), map_b);
                    }
                }
            }
        }
        res
    }

    fn join(mut self, other: Self) -> Self {
        self.union(&other);
        self
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        *self = self.clone().meet(other);
        *self != old
    }

    fn join_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        self.union(&other);
        *self != old
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
        let functions: Vec<Function> = function_id.iter().map(|(_, v)| v.clone()).collect();
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
        let mut p23_components = Vec::new();
        p23_components.push(FieldAccess::Offset(Offset(1)));
        let p23 = Path(p23_components);

        // Create p2 = '' (empty path)
        let p2 = Path::empty();

        // Create p1 = .[1]
        let mut p1_components = Vec::new();
        p1_components.push(FieldAccess::Offset(Offset(1)));
        let p1 = Path(p1_components);

        let result = p23.substitute_prefix(&p2, &p1).unwrap();

        // Create expected = .[2]
        let mut expected_components = Vec::new();
        expected_components.push(FieldAccess::Offset(Offset(2)));
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
        let mut path_components = Vec::new();
        path_components.push(FieldAccess::Symbol(ArcIntern::from("foo")));
        path_components.push(FieldAccess::Offset(Offset(42)));
        path_components.push(FieldAccess::Symbol(ArcIntern::from("bar")));
        let path = Path(path_components);

        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.[42].bar");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }
}
