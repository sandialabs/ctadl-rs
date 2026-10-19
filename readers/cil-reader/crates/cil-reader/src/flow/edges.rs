//! The flat edge view: "A flows to B" pairs derived from the effects.
//!
//! Unlike the effects, this view applies a policy, the one the Dex and JVM
//! frontends use: calls, constants, tests and clears give no intra-instruction
//! edges, and element indices and block sizes do not flow. It is a
//! convenience, not the specification, and is computed on demand.

use super::{Effect, Field, InsFlow, Op, OpFlags, Path, Place, Root, Slot, Transfer};
use std::fmt;

/// A location an edge connects.
#[derive(Debug, Clone, PartialEq)]
pub enum Loc<S> {
    Var(S),
    Path(Path<S>),
    Place(Place<S>),
}

/// `from` flows to `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge<S> {
    pub from: Loc<S>,
    pub to: Loc<S>,
    pub transfer: Transfer,
}

impl<S: Clone> Effect<S> {
    /// This effect's edges, in operand order.
    pub fn edges(&self) -> Vec<Edge<S>> {
        let var = |s: &S| Loc::Var(s.clone());
        let copy = |from, to| Edge {
            from,
            to,
            transfer: Transfer::Copy,
        };
        match self {
            Effect::Assign {
                dst,
                srcs,
                transfer,
            } => srcs
                .iter()
                .map(|s| Edge {
                    from: var(s),
                    to: var(dst),
                    transfer: transfer.clone(),
                })
                .collect(),
            Effect::Load { dst, src } => vec![copy(Loc::Path(src.clone()), var(dst))],
            Effect::Store { dst, src } => vec![copy(var(src), Loc::Path(dst.clone()))],
            // The address aliases the place.
            Effect::AddressOf { dst, place } => vec![copy(Loc::Place(place.clone()), var(dst))],
            Effect::New { dst, sizes, .. } => {
                // A single size is the length itself; several (a
                // multi-dimensional array) multiply into it.
                let transfer = if sizes.len() == 1 {
                    Transfer::Copy
                } else {
                    Transfer::Derived(Op::Mul(OpFlags::default()))
                };
                let length = Loc::Path(Path {
                    root: Root::Slot(dst.clone()),
                    field: Field::Length,
                });
                sizes
                    .iter()
                    .map(|s| Edge {
                        from: var(s),
                        to: length.clone(),
                        transfer: transfer.clone(),
                    })
                    .collect()
            }
            Effect::Opaque { srcs, dsts, .. } => srcs
                .iter()
                .flat_map(|s| {
                    dsts.iter().map(move |d| Edge {
                        from: Loc::Var(s.clone()),
                        to: Loc::Var(d.clone()),
                        transfer: Transfer::Derived(Op::Opaque),
                    })
                })
                .collect(),
            Effect::Const { .. }
            | Effect::FunctionPtr { .. }
            | Effect::Clear { .. }
            | Effect::Call(_)
            | Effect::Return { .. }
            | Effect::Throw { .. }
            | Effect::Jump { .. }
            | Effect::Test { .. } => Vec::new(),
        }
    }
}

impl InsFlow {
    /// The edges of every effect, in order.
    pub fn edges(&self) -> impl Iterator<Item = Edge<Slot>> + '_ {
        self.effects.iter().flat_map(|e| e.edges())
    }
}

impl<S: fmt::Display> fmt::Display for Loc<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Loc::Var(s) => s.fmt(f),
            Loc::Path(p) => p.fmt(f),
            Loc::Place(p) => write!(f, "&{p}"),
        }
    }
}

impl<S: fmt::Display> fmt::Display for Edge<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {} {}", self.from, self.to, self.transfer)
    }
}
