//! The raw and canonical forms of SARIF results, over an atom type `A`.
//!
//! A raw run holds what the parser keeps of one SARIF run: references are still numbers into
//! the run's `logicalLocations` table, `executionOrder` is still there, and collections are in
//! file order. Every field the comparison ignores is absent from these types, so it cannot
//! affect the result.
//!
//! A canonical result is what a result is compared by. Its types have no table index, no
//! `executionOrder` and no other per-run number. Unordered collections are `Vec`s that the
//! canonical form keeps sorted; a thread flow's steps keep their order.
//!
//! Types that hold no reference and no order number (`Physical`, `Region`, `Address`,
//! `Keyed`, `Props`) are shared by both forms; the canonical form sorts their bags.
//!
//! Absence follows SARIF. An array SARIF defaults to empty (`locations`, `codeFlows`,
//! `threadFlows`, `kinds`, `logicalLocations`) is a `Vec`, absent read as empty. A field SARIF
//! gives no default, and every ctadl property, is an `Option`, so absent and empty differ.

// The derived `Clone`s are for unverified callers; Verus gives them no spec.
#![cfg_attr(verus_keep_ghost, verifier::allow(autoderive_clone_without_spec))]

use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------------------
// Shared by the raw and canonical forms.

/// `region`. The parser drops its `message` and `snippet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_line: Option<i64>,
    pub end_column: Option<i64>,
    pub byte_offset: Option<i64>,
    pub byte_length: Option<i64>,
}

/// `address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address<A> {
    pub absolute: Option<i64>,
    pub relative: Option<i64>,
    pub kind: Option<A>,
}

/// `physicalLocation`. `base` is the `uriBaseId`'s name, never its value; the
/// `artifactLocation`'s `index` is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Physical<A> {
    pub base: Option<A>,
    pub uri: Option<A>,
    pub region: Region,
    pub address: Option<Address<A>>,
}

/// One entry of a property that maps a key to a set of atoms: a taint label to its vertices
/// (`taintVertices`), or a formal to its labels (`taintedFormals`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyed<A> {
    pub key: A,
    /// A bag.
    pub values: Vec<A>,
}

/// The `properties` of a result that are compared. Each is `None` when the key is absent.
/// `sourceCallee`, `sinkCallee` and the ids of the debug profile are not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Props<A> {
    /// `taintLabels`, a bag.
    pub labels: Option<Vec<A>>,
    /// `taintVertex`, with the instruction id of a `call-arg` vertex already replaced.
    pub vertex: Option<A>,
    /// `taintVertices`, a bag of entries, each a label and a bag of vertices, the vertices
    /// rewritten as `vertex` is.
    pub vertices: Option<Vec<Keyed<A>>>,
    /// `sourceFunctions`, a bag.
    pub source_fns: Option<Vec<A>>,
    /// `sinkFunctions`, a bag.
    pub sink_fns: Option<Vec<A>>,
    /// `taintedFormals`, a bag of entries, each a formal and a bag of labels.
    pub tainted_formals: Option<Vec<Keyed<A>>>,
    /// `import`, the debug profile's import name.
    pub import: Option<A>,
}

// ---------------------------------------------------------------------------
// The raw form.

/// One SARIF run's results and the table their references index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRun<A> {
    /// `run.logicalLocations`.
    pub logical: Vec<RawLogical<A>>,
    /// `run.results`, in file order.
    pub results: Vec<RawResult<A>>,
}

/// An entry of `run.logicalLocations`: its `fullyQualifiedName`, else its `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLogical<A> {
    pub fqn: A,
}

/// An element of `location.logicalLocations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawLogicalRef<A> {
    /// An `index` into the run's `logical` table.
    Index(usize),
    /// An entry with no `index`: its `fullyQualifiedName`, else its `name`.
    Inline(A),
}

/// `location`. The parser drops its `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLocation<A> {
    pub physical: Option<Physical<A>>,
    /// `logicalLocations`, in file order.
    pub logical: Vec<RawLogicalRef<A>>,
}

/// `threadFlowLocation`. The parser drops its `location`'s `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawStep<A> {
    /// `executionOrder`.
    pub order: Option<i64>,
    /// `kinds`, in file order.
    pub kinds: Vec<A>,
    pub location: Option<RawLocation<A>>,
}

/// `threadFlow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawThreadFlow<A> {
    /// `locations`: the steps, in flow order.
    pub steps: Vec<RawStep<A>>,
}

/// `codeFlow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCodeFlow<A> {
    /// `threadFlows`, in file order.
    pub thread_flows: Vec<RawThreadFlow<A>>,
}

/// `result`. The parser drops its `message` and `ruleIndex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResult<A> {
    /// `ruleId`.
    pub rule: Option<A>,
    /// `kind`; the parser fills in `fail` when absent.
    pub kind: A,
    /// `level`; the parser fills in `warning` when absent.
    pub level: A,
    /// `locations`, in file order.
    pub locations: Vec<RawLocation<A>>,
    /// `codeFlows`, in file order.
    pub code_flows: Vec<RawCodeFlow<A>>,
    /// `properties`, with its bags in file order.
    pub props: Props<A>,
}

// ---------------------------------------------------------------------------
// The canonical form.

/// A location, its logical references replaced by the names they resolve to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonLocation<A> {
    pub physical: Option<Physical<A>>,
    /// The `fqn` of each logical location, a bag.
    pub logical: Vec<A>,
}

/// A step, without its `executionOrder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonStep<A> {
    /// A bag.
    pub kinds: Vec<A>,
    pub location: Option<CanonLocation<A>>,
}

/// A thread flow: all its steps in order, or, when only endpoints are compared, its first and
/// last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonThreadFlow<A> {
    Steps(Vec<CanonStep<A>>),
    Endpoints(CanonStep<A>, CanonStep<A>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonCodeFlow<A> {
    /// A bag.
    pub thread_flows: Vec<CanonThreadFlow<A>>,
}

/// What a result is compared by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonResult<A> {
    pub rule: Option<A>,
    pub kind: A,
    pub level: A,
    /// A bag; empty when only endpoints are compared.
    pub locations: Vec<CanonLocation<A>>,
    /// A bag.
    pub code_flows: Vec<CanonCodeFlow<A>>,
    /// Its bags sorted. When only endpoints are compared, only `labels`, `source_fns` and
    /// `sink_fns` are kept; the rest are `None`.
    pub props: Props<A>,
}

/// Why a run has no canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A logical reference past the end of the run's `logicalLocations`.
    OutOfRange,
    /// A thread flow whose `executionOrder` values do not increase along its steps.
    NotIncreasing,
    /// A thread flow with no steps, which SARIF does not allow.
    EmptyFlow,
}

// ---------------------------------------------------------------------------
// Views: the same values with every `Vec` replaced by its `Seq`, for the specs.
//
// Verus's `==` on a type that holds a `Vec` is not equality of contents, and specs cannot
// build a `Vec`, so every statement about these types is made of their views. A type with no
// `Vec` in it (`Region`, `Address`, `Physical`, `RawLogical`, `RawLogicalRef`, `Error`) is its
// own view.

pub struct KeyedV<A> {
    pub key: A,
    pub values: Seq<A>,
}

pub struct PropsV<A> {
    pub labels: Option<Seq<A>>,
    pub vertex: Option<A>,
    pub vertices: Option<Seq<KeyedV<A>>>,
    pub source_fns: Option<Seq<A>>,
    pub sink_fns: Option<Seq<A>>,
    pub tainted_formals: Option<Seq<KeyedV<A>>>,
    pub import: Option<A>,
}

pub struct RawRunV<A> {
    pub logical: Seq<RawLogical<A>>,
    pub results: Seq<RawResultV<A>>,
}

pub struct RawLocationV<A> {
    pub physical: Option<Physical<A>>,
    pub logical: Seq<RawLogicalRef<A>>,
}

pub struct RawStepV<A> {
    pub order: Option<i64>,
    pub kinds: Seq<A>,
    pub location: Option<RawLocationV<A>>,
}

pub struct RawThreadFlowV<A> {
    pub steps: Seq<RawStepV<A>>,
}

pub struct RawCodeFlowV<A> {
    pub thread_flows: Seq<RawThreadFlowV<A>>,
}

pub struct RawResultV<A> {
    pub rule: Option<A>,
    pub kind: A,
    pub level: A,
    pub locations: Seq<RawLocationV<A>>,
    pub code_flows: Seq<RawCodeFlowV<A>>,
    pub props: PropsV<A>,
}

pub struct CanonLocationV<A> {
    pub physical: Option<Physical<A>>,
    pub logical: Seq<A>,
}

pub struct CanonStepV<A> {
    pub kinds: Seq<A>,
    pub location: Option<CanonLocationV<A>>,
}

pub enum CanonThreadFlowV<A> {
    Steps(Seq<CanonStepV<A>>),
    Endpoints(CanonStepV<A>, CanonStepV<A>),
}

pub struct CanonCodeFlowV<A> {
    pub thread_flows: Seq<CanonThreadFlowV<A>>,
}

pub struct CanonResultV<A> {
    pub rule: Option<A>,
    pub kind: A,
    pub level: A,
    pub locations: Seq<CanonLocationV<A>>,
    pub code_flows: Seq<CanonCodeFlowV<A>>,
    pub props: PropsV<A>,
}

pub open spec fn opt_seq<T>(v: Option<Vec<T>>) -> Option<Seq<T>> {
    match v {
        Some(v) => Some(v@),
        None => None,
    }
}

pub open spec fn opt_keyed<A>(v: Option<Vec<Keyed<A>>>) -> Option<Seq<KeyedV<A>>> {
    match v {
        Some(v) => Some(v@.map_values(|k: Keyed<A>| k@)),
        None => None,
    }
}

impl<A> Keyed<A> {
    pub open spec fn view(&self) -> KeyedV<A> {
        KeyedV { key: self.key, values: self.values@ }
    }
}

impl<A> Props<A> {
    pub open spec fn view(&self) -> PropsV<A> {
        PropsV {
            labels: opt_seq(self.labels),
            vertex: self.vertex,
            vertices: opt_keyed(self.vertices),
            source_fns: opt_seq(self.source_fns),
            sink_fns: opt_seq(self.sink_fns),
            tainted_formals: opt_keyed(self.tainted_formals),
            import: self.import,
        }
    }
}

impl<A> RawRun<A> {
    pub open spec fn view(&self) -> RawRunV<A> {
        RawRunV {
            logical: self.logical@,
            results: self.results@.map_values(|r: RawResult<A>| r@),
        }
    }
}

impl<A> RawLocation<A> {
    pub open spec fn view(&self) -> RawLocationV<A> {
        RawLocationV { physical: self.physical, logical: self.logical@ }
    }
}

impl<A> RawStep<A> {
    pub open spec fn view(&self) -> RawStepV<A> {
        RawStepV {
            order: self.order,
            kinds: self.kinds@,
            location: match self.location {
                Some(l) => Some(l@),
                None => None,
            },
        }
    }
}

impl<A> RawThreadFlow<A> {
    pub open spec fn view(&self) -> RawThreadFlowV<A> {
        RawThreadFlowV { steps: self.steps@.map_values(|s: RawStep<A>| s@) }
    }
}

impl<A> RawCodeFlow<A> {
    pub open spec fn view(&self) -> RawCodeFlowV<A> {
        RawCodeFlowV { thread_flows: self.thread_flows@.map_values(|t: RawThreadFlow<A>| t@) }
    }
}

impl<A> RawResult<A> {
    pub open spec fn view(&self) -> RawResultV<A> {
        RawResultV {
            rule: self.rule,
            kind: self.kind,
            level: self.level,
            locations: self.locations@.map_values(|l: RawLocation<A>| l@),
            code_flows: self.code_flows@.map_values(|c: RawCodeFlow<A>| c@),
            props: self.props@,
        }
    }
}

impl<A> CanonLocation<A> {
    pub open spec fn view(&self) -> CanonLocationV<A> {
        CanonLocationV { physical: self.physical, logical: self.logical@ }
    }
}

impl<A> CanonStep<A> {
    pub open spec fn view(&self) -> CanonStepV<A> {
        CanonStepV {
            kinds: self.kinds@,
            location: match self.location {
                Some(l) => Some(l@),
                None => None,
            },
        }
    }
}

impl<A> CanonThreadFlow<A> {
    pub open spec fn view(&self) -> CanonThreadFlowV<A> {
        match self {
            CanonThreadFlow::Steps(s) => CanonThreadFlowV::Steps(
                s@.map_values(|x: CanonStep<A>| x@),
            ),
            CanonThreadFlow::Endpoints(a, b) => CanonThreadFlowV::Endpoints(a@, b@),
        }
    }
}

impl<A> CanonCodeFlow<A> {
    pub open spec fn view(&self) -> CanonCodeFlowV<A> {
        CanonCodeFlowV {
            thread_flows: self.thread_flows@.map_values(|t: CanonThreadFlow<A>| t@),
        }
    }
}

impl<A> CanonResult<A> {
    pub open spec fn view(&self) -> CanonResultV<A> {
        CanonResultV {
            rule: self.rule,
            kind: self.kind,
            level: self.level,
            locations: self.locations@.map_values(|l: CanonLocation<A>| l@),
            code_flows: self.code_flows@.map_values(|c: CanonCodeFlow<A>| c@),
            props: self.props@,
        }
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_build() {
        let none = Props {
            labels: None,
            vertex: None,
            vertices: None,
            source_fns: None,
            sink_fns: None,
            tainted_formals: None,
            import: None,
        };
        let loc = RawLocation {
            physical: Some(Physical {
                base: Some(1u32),
                uri: Some(2),
                region: Region {
                    start_line: Some(3),
                    start_column: None,
                    end_line: None,
                    end_column: None,
                    byte_offset: None,
                    byte_length: None,
                },
                address: None,
            }),
            logical: vec![RawLogicalRef::Index(0), RawLogicalRef::Inline(3)],
        };
        let step = RawStep {
            order: Some(1),
            kinds: vec![4],
            location: Some(loc.clone()),
        };
        let run = RawRun {
            logical: vec![RawLogical { fqn: 5 }],
            results: vec![RawResult {
                rule: Some(6),
                kind: 7,
                level: 8,
                locations: vec![loc],
                code_flows: vec![RawCodeFlow {
                    thread_flows: vec![RawThreadFlow {
                        steps: vec![step.clone(), step],
                    }],
                }],
                props: Props {
                    labels: Some(vec![9]),
                    vertices: Some(vec![Keyed {
                        key: 9,
                        values: vec![10],
                    }]),
                    ..none
                },
            }],
        };
        assert_eq!(run.clone(), run);
        assert_eq!(run.results[0].code_flows[0].thread_flows[0].steps.len(), 2);
    }
}
