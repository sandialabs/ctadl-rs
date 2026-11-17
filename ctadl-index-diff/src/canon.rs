//! The column types of canonical rows. None of them can hold a function, instruction, span or
//! import id. They are plain `Copy` values: the instruction inside a call argument is a
//! [`CanonInsn`], which is not interned, so nothing here grows a process-wide interner.
//!
//! Each derives `Ord`, which compares intern ids and is used to sort a whole table, and has a
//! [`ContentOrd`], which compares what the value denotes and orders the printed samples.

use std::cmp::Ordering;
use std::fmt::Write as _;

use ctadl_index::facts::{
    CallDispatchKey, CallTargetObject, CallTargetObjectKind, FormalIndex, FormalType, FunctionId,
    GlobalId, IntentPairKind, Path, Root, Str, TaintDirection, TaintState,
};
use ctadl_index::index_engine::dump::{self, DumpCol};

use crate::insn::{CanonInsn, SigVar};
use crate::render::{Col, Cx};

/// A function, by name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FnName(pub Str);

impl FnName {
    pub fn as_str(&self) -> &'static str {
        self.0.as_str()
    }
}

/// A variable of a canonical row. A call argument names its instruction by content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CanonVar {
    Uninit,
    Local(Str),
    Formal(FormalIndex),
    CallArg(CanonInsn, FormalIndex),
    Global(GlobalId),
}

/// A call target: a function by name, or a class. Built only by [`CanonTarget::from_object`],
/// so it never holds a function id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CanonTarget(Target);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum Target {
    Function(FnName),
    /// Any variant but a function id.
    Class(CallTargetObject),
}

impl CanonTarget {
    /// `obj`, with a function id replaced by the name `function` gives it.
    pub fn from_object<E>(
        obj: CallTargetObject,
        function: impl FnOnce(FunctionId) -> Result<FnName, E>,
    ) -> Result<Self, E> {
        Ok(CanonTarget(match obj.kind() {
            CallTargetObjectKind::FunctionId(f) => Target::Function(function(*f)?),
            _ => Target::Class(obj),
        }))
    }
}

/// A taint-graph edge, its call site named by content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CanonEdge {
    Intra,
    Call(CanonInsn),
    Return(CanonInsn),
    Global,
}

/// A query endpoint, its function by name and its call site by content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CanonEndpoint {
    pub infunc: FnName,
    pub var: CanonVar,
    pub path: Path,
    pub label: Str,
    pub direction: TaintDirection,
    pub call_site: Option<CanonInsn>,
    pub saturating: bool,
}

/// A [`FormalType`], which has no order of its own, ordered by declaration.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FType(pub FormalType);

impl FType {
    fn rank(self) -> u8 {
        match self.0 {
            FormalType::Copied => 0,
            FormalType::SharedFields => 1,
            FormalType::Shared => 2,
        }
    }
}

impl Ord for FType {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for FType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An order by what a value denotes, not by intern ids, so that it is the same in every process.
/// Equal under it exactly when equal under `Eq`.
pub trait ContentOrd {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering;
}

macro_rules! content_by_ord {
    ($($t:ty),*) => {$(
        impl ContentOrd for $t {
            fn content_order(&self, other: &Self, _: &Cx<'_>) -> Ordering {
                self.cmp(other)
            }
        }
    )*};
}
content_by_ord!(
    u32,
    bool,
    String,
    FormalIndex,
    FType,
    TaintState,
    TaintDirection,
    IntentPairKind
);

macro_rules! content_by_content_cmp {
    ($($t:ty),*) => {$(
        impl ContentOrd for $t {
            fn content_order(&self, other: &Self, _: &Cx<'_>) -> Ordering {
                self.content_cmp(other)
            }
        }
    )*};
}
// `CallDispatchKey`'s content order compares symbols, which are ordered by their text.
content_by_content_cmp!(Str, Path, GlobalId, Root, CallDispatchKey);

impl ContentOrd for FnName {
    fn content_order(&self, other: &Self, _: &Cx<'_>) -> Ordering {
        self.0.content_cmp(&other.0)
    }
}

impl<T: ContentOrd> ContentOrd for Option<T> {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
        match (self, other) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => a.content_order(b, cx),
        }
    }
}

impl ContentOrd for CanonInsn {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
        self.function
            .content_order(&other.function, cx)
            .then_with(|| self.signature.cmp(&other.signature))
    }
}

impl CanonVar {
    fn rank(&self) -> u8 {
        match self {
            CanonVar::Uninit => 0,
            CanonVar::Local(_) => 1,
            CanonVar::Formal(_) => 2,
            CanonVar::CallArg(..) => 3,
            CanonVar::Global(_) => 4,
        }
    }
}

impl ContentOrd for CanonVar {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
        match (self, other) {
            (CanonVar::Local(a), CanonVar::Local(b)) => a.content_cmp(b),
            (CanonVar::Formal(a), CanonVar::Formal(b)) => a.cmp(b),
            (CanonVar::CallArg(i, n), CanonVar::CallArg(j, m)) => {
                i.content_order(j, cx).then_with(|| n.cmp(m))
            }
            (CanonVar::Global(a), CanonVar::Global(b)) => a.content_cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl CanonTarget {
    /// Orders by content: a function by its name, before any class.
    pub fn content_cmp(&self, other: &Self) -> Ordering {
        match (self.0, other.0) {
            (Target::Function(a), Target::Function(b)) => a.0.content_cmp(&b.0),
            (Target::Function(_), Target::Class(_)) => Ordering::Less,
            (Target::Class(_), Target::Function(_)) => Ordering::Greater,
            // Neither is a function id, so the derived order compares variants and then
            // symbols by their text.
            (Target::Class(a), Target::Class(b)) => a.kind().cmp(b.kind()),
        }
    }
}

impl ContentOrd for CanonTarget {
    fn content_order(&self, other: &Self, _: &Cx<'_>) -> Ordering {
        self.content_cmp(other)
    }
}

impl CanonEdge {
    fn rank(&self) -> u8 {
        match self {
            CanonEdge::Intra => 0,
            CanonEdge::Call(_) => 1,
            CanonEdge::Return(_) => 2,
            CanonEdge::Global => 3,
        }
    }
}

impl ContentOrd for CanonEdge {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
        match (self, other) {
            (CanonEdge::Call(a), CanonEdge::Call(b))
            | (CanonEdge::Return(a), CanonEdge::Return(b)) => a.content_order(b, cx),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl ContentOrd for CanonEndpoint {
    fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
        self.infunc
            .content_order(&other.infunc, cx)
            .then_with(|| self.var.content_order(&other.var, cx))
            .then_with(|| self.path.content_cmp(&other.path))
            .then_with(|| self.label.content_cmp(&other.label))
            .then_with(|| self.direction.cmp(&other.direction))
            .then_with(|| self.call_site.content_order(&other.call_site, cx))
            .then_with(|| self.saturating.cmp(&other.saturating))
    }
}

macro_rules! content_tuple {
    ($($t:ident $i:tt),+) => {
        impl<$($t: ContentOrd),+> ContentOrd for ($($t,)+) {
            fn content_order(&self, other: &Self, cx: &Cx<'_>) -> Ordering {
                Ordering::Equal
                    $(.then_with(|| self.$i.content_order(&other.$i, cx)))+
            }
        }
    };
}
content_tuple!(A 0);
content_tuple!(A 0, B 1);
content_tuple!(A 0, B 1, C 2);
content_tuple!(A 0, B 1, C 2, D 3);
content_tuple!(A 0, B 1, C 2, D 3, E 4);
content_tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
content_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);

impl DumpCol for FnName {
    fn dump_col(&self, _: &dump::Cx<'_>, out: &mut String) {
        out.push_str(self.as_str())
    }
}

impl DumpCol for FType {
    fn dump_col(&self, cx: &dump::Cx<'_>, out: &mut String) {
        self.0.dump_col(cx, out)
    }
}

impl Col for CanonVar {
    fn col(&self, cx: &Cx<'_>, out: &mut String) {
        match self {
            CanonVar::Uninit => out.push_str("uninit"),
            CanonVar::Local(s) => write!(out, "local({s})").unwrap(),
            CanonVar::Formal(i) => write!(out, "formal({i})").unwrap(),
            CanonVar::Global(g) => write!(out, "{g}").unwrap(),
            CanonVar::CallArg(insn, i) => {
                out.push_str("call-arg(");
                insn.col(cx, out);
                write!(out, ", {i})").unwrap()
            }
        }
    }
}

impl DumpCol for SigVar {
    fn dump_col(&self, _: &dump::Cx<'_>, out: &mut String) {
        match self {
            SigVar::Uninit => out.push_str("uninit"),
            SigVar::Local(s) => write!(out, "local({s})").unwrap(),
            SigVar::Formal(i) => write!(out, "formal({i})").unwrap(),
            SigVar::Global(g) => write!(out, "{g}").unwrap(),
            SigVar::This(i) => write!(out, "call-arg(this, {i})").unwrap(),
        }
    }
}

impl DumpCol for CanonTarget {
    fn dump_col(&self, _: &dump::Cx<'_>, out: &mut String) {
        match self.0 {
            Target::Function(f) => write!(out, "ptr<{}>", f.as_str()).unwrap(),
            Target::Class(c) => write!(out, "{c}").unwrap(),
        }
    }
}

impl Col for CanonEdge {
    fn col(&self, cx: &Cx<'_>, out: &mut String) {
        match self {
            CanonEdge::Intra => out.push_str("intra"),
            CanonEdge::Global => out.push_str("global"),
            CanonEdge::Call(i) | CanonEdge::Return(i) => {
                out.push_str(if matches!(self, CanonEdge::Call(_)) {
                    "call("
                } else {
                    "return("
                });
                i.col(cx, out);
                out.push(')')
            }
        }
    }
}

impl Col for CanonEndpoint {
    fn col(&self, cx: &Cx<'_>, out: &mut String) {
        write!(out, "{} ", self.infunc.as_str()).unwrap();
        self.var.col(cx, out);
        write!(out, "{} label {} {}", self.path, self.label, self.direction).unwrap();
        if let Some(site) = &self.call_site {
            out.push_str(" at ");
            site.col(cx, out);
        }
        if self.saturating {
            out.push_str(" saturating");
        }
    }
}
