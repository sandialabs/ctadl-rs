//! CTADL IR Terminator instruction

use std::fmt;

use smallvec::SmallVec;

use crate::index::idx::Idx;
use crate::mir::{BasicBlockIdx, Exp, SourceInfo, VariableRef};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Terminator {
    pub source_info: SourceInfo,
    pub kind: TerminatorKind,
}

/// Terminator instructions for basic blocks.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TerminatorKind {
    /// Return instruction. All returns must return the same number of values.
    Return { args: SmallVec<[Exp; 4]> },

    /// Non-deterministic jumps to successor blocks.
    Goto {
        targets: SmallVec<[BasicBlockIdx; 4]>,
    },
}

impl Terminator {
    /// Creates a new terminator
    #[inline]
    pub fn new(kind: TerminatorKind, source_info: SourceInfo) -> Self {
        Self { kind, source_info }
    }

    /// Creates a new terminator with default source info
    #[inline]
    pub fn new_kind(kind: TerminatorKind) -> Self {
        Self {
            kind,
            source_info: Default::default(),
        }
    }

    #[inline]
    pub fn successors(&self) -> impl DoubleEndedIterator<Item = BasicBlockIdx> + '_ {
        self.kind.successors()
    }

    #[inline]
    pub fn iter_src_var<'s>(&'s self) -> Box<dyn DoubleEndedIterator<Item = &'s VariableRef> + 's> {
        self.kind.iter_src_var()
    }

    #[inline]
    pub fn iter_src_var_mut<'s>(
        &'s mut self,
    ) -> Box<dyn DoubleEndedIterator<Item = &'s mut VariableRef> + 's> {
        self.kind.iter_src_var_mut()
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Terminator {
            kind,
            source_info: _,
        } = self;
        write!(f, "{kind}")
    }
}

impl TerminatorKind {
    #[inline]
    pub fn successors(&self) -> impl DoubleEndedIterator<Item = BasicBlockIdx> + '_ {
        use self::TerminatorKind::*;
        match *self {
            Return { .. } => [].iter().copied(),
            Goto { ref targets } => targets.iter().copied(),
        }
    }

    /// Returns an iterator over variables referenced by this statement.
    pub fn iter_src_var<'s>(&'s self) -> Box<dyn DoubleEndedIterator<Item = &'s VariableRef> + 's> {
        use TerminatorKind::*;
        match self {
            Return { args } => Box::new(args.iter().filter_map(|arg| {
                if matches!(arg, Exp::AccessPath(_)) {
                    let Exp::AccessPath(ap) = arg else {
                        unreachable!()
                    };
                    Some(&ap.variable_ref)
                } else {
                    None
                }
            })),
            Goto { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Returns an iterator over variables referenced by this statement.
    pub fn iter_src_var_mut<'s>(
        &'s mut self,
    ) -> Box<dyn DoubleEndedIterator<Item = &'s mut VariableRef> + 's> {
        use TerminatorKind::*;
        match self {
            Return { args } => Box::new(args.iter_mut().filter_map(|arg| {
                if matches!(arg, Exp::AccessPath(_)) {
                    let Exp::AccessPath(ap) = arg else {
                        unreachable!()
                    };
                    Some(&mut ap.variable_ref)
                } else {
                    None
                }
            })),
            Goto { .. } => Box::new(std::iter::empty()),
        }
    }
}

impl fmt::Display for TerminatorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminatorKind::Return { args } => {
                write!(f, "return ")?;
                for (i, t) in args.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
            }
            TerminatorKind::Goto { targets } => {
                write!(f, "goto ")?;
                for (i, t) in targets.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t.index())?;
                }
            }
        }
        Ok(())
    }
}
