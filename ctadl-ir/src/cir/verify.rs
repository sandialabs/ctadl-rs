use std::ops::{Deref, DerefMut};
use std::{fmt, fmt::Display};

use hashbrown::hash_set::HashSet;
use thiserror::Error;

use crate::cir::path::{self, PathClass};
use crate::cir::{
    AccessPath, BasicBlockData, BasicBlockIdx, CallStyle, Exp, FunctionData, FunctionIdx, LocalIdx,
    Location, ParameterIdx, PathSegment, StatementKind, TerminatorKind, Variable, VariableRef,
    path_syntax, visit::Visitor,
};
use crate::index::idx::Idx;

/// These are the errors that CTADL IR verification detects. Any of these errors mean that the IR
/// is malformed and attempting to compile it may result in panics.
#[derive(Error, Debug, Eq, PartialEq)]
pub enum VerifyError {
    /// A function was found that has an empty name
    #[error("function has no name: {}", index.index())]
    UnnamedFunction { index: FunctionIdx },

    #[error("multiply-defined function: {}", index.index())]
    MultiplyDefinedFunction { index: FunctionIdx },

    /// Parameter reference found outside the bounds of declared parameters
    #[error("in function: {function}: reference to nonexistent parameter: '{}'", parameter.index())]
    ParameterDoesNotExist {
        function: String,
        parameter: ParameterIdx,
    },

    /// Local reference found outside the bounds of the function's locals table
    #[error("in function: {function}: reference to nonexistent local: '{}'", local.index())]
    LocalDoesNotExist { function: String, local: LocalIdx },

    #[error("in function: {function}: goto to non-existent block: {}", target.index())]
    BlockDoesNotExist {
        function: String,
        block: BasicBlockIdx,
        target: BasicBlockIdx,
    },

    /// A 'return' is found that doesn't return the same number of values as the function's return
    /// arity.
    #[error(
        "inconsistent returns in function '{function}': expected {expected_arity} got {actual_arity}"
    )]
    InconsistentReturns {
        function: String,
        expected_arity: u8,
        actual_arity: usize,
    },

    #[error("found variable ref without enclosing function")]
    VariableRefOutsideFunction,

    #[error("in function: {}: basic block has no terminator: block_{}", function.index(), block.index())]
    NoTerminator {
        function: FunctionIdx,
        block: BasicBlockIdx,
    },

    /// A goto instruction with no target blocks
    #[error("in function: {function}: empty goto at {location}")]
    EmptyGoto {
        function: String,
        location: Location,
    },

    /// An access path that is not in normal form (`docs/ir.md` S2). Paths are compared only in
    /// normal form, so one that is not would silently miss its equal.
    #[error("in function: {function}: access path {path} at {location} is not in normal form")]
    NotNormal {
        function: String,
        location: Location,
        path: String,
    },

    /// A void access path (a field or `deref` after `#value`, S3) as an operand. It denotes
    /// nothing, so the statement that names it says nothing.
    #[error("in function: {function}: void access path {path} at {location}")]
    VoidPath {
        function: String,
        location: Location,
        path: String,
    },

    /// An address as an assignment's destination. [`crate::cir::StatementKind::assign`]
    /// rewrites `p.[k] = e` into `p = e.[-k]` (S5), so one that survives bypassed it.
    #[error("in function: {function}: address destination {path} at {location}")]
    AddressDestination {
        function: String,
        location: Location,
        path: String,
    },

    /// `value(p)` anywhere but as an assignment's source.
    #[error(
        "in function: {function}: value(...) used outside an assignment's sources at {location}"
    )]
    MisplacedValue {
        function: String,
        location: Location,
    },

    /// A write to the whole of `$globals`: a copy into it, or a call result bound to it. A global
    /// `g` is the location `$globals.g` (S13); the variable itself is shared, never rebound.
    #[error("in function: {function}: write to $globals with an empty path at {location}")]
    EmptyGlobalsWrite {
        function: String,
        location: Location,
    },

    /// The `#value` segment spelled as a field. `#value` names no field of any source language,
    /// so a field of that name is a front end that meant [`PathSegment::Value`].
    #[error("in function: {function}: field named #value in {path} at {location}")]
    ValueSpelledAsField {
        function: String,
        location: Location,
        path: String,
    },

    /// In SSA form, a store whose base is a version nothing defines: the store was treated as
    /// defining its base, which a store never does (S5, S16).
    #[error("in function: {function}: store at {location} defines its base {base}")]
    StoreDefinesBase {
        function: String,
        location: Location,
        base: String,
    },
}

/// A list of verification errors
#[derive(Debug, Default, Eq, PartialEq)]
pub struct VerifyErrors {
    errors: Vec<VerifyError>,
}

impl std::error::Error for VerifyErrors {}

impl VerifyErrors {
    /// The errors, by value.
    pub fn errors_vec(self) -> Vec<VerifyError> {
        self.errors
    }
}

impl Deref for VerifyErrors {
    type Target = Vec<VerifyError>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.errors
    }
}

impl DerefMut for VerifyErrors {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.errors
    }
}

impl Display for VerifyErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.errors.len() > 1 {
            writeln!(f, "found {} errors", self.errors.len())?;
        }
        for err in &self.errors {
            writeln!(f, "> {err}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct CirVerify {
    error: VerifyErrors,
    /// Name of the last function
    name: String,
    /// Number of blocks in last function
    num_blocks: Option<usize>,
    /// Length of the last function's parameter list
    params_len: Option<ParameterIdx>,
    /// Number of locals in the last function's locals table
    locals_len: Option<usize>,
    /// Length of return values from function
    return_arity: Option<u8>,
    defines: HashSet<String>,
}

impl CirVerify {
    /// After calling the visitor on an AST element, call this to retrieve any error. Also clears
    /// the error state.
    #[inline]
    pub fn take_error(&mut self) -> Result<(), VerifyErrors> {
        let error = std::mem::take(&mut self.error);
        // clear our state
        *self = Default::default();
        if error.is_empty() { Ok(()) } else { Err(error) }
    }

    #[inline]
    fn add_error(&mut self, e: VerifyError) {
        self.error.push(e)
    }
}

impl CirVerify {
    /// The checks every access path operand gets: normal form, not void, no `#value` field.
    fn check_access_path(&mut self, ap: &AccessPath, location: Location) {
        let printed = || ap.to_string();
        if !path::is_normal(&ap.segments) {
            self.add_error(VerifyError::NotNormal {
                function: self.name.clone(),
                location,
                path: printed(),
            });
        }
        if path::classify(&ap.segments) == PathClass::Void {
            self.add_error(VerifyError::VoidPath {
                function: self.name.clone(),
                location,
                path: printed(),
            });
        }
        if ap
            .segments
            .iter()
            .any(|s| matches!(s, PathSegment::Field(f) if &**f == path_syntax::VALUE))
        {
            self.add_error(VerifyError::ValueSpelledAsField {
                function: self.name.clone(),
                location,
                path: printed(),
            });
        }
    }

    /// Checks an operand: its access path, and that it is not a `value(...)` unless `value_ok`.
    fn check_exp(&mut self, e: &Exp, value_ok: bool, location: Location) {
        if let Some(ap) = e.access_path() {
            self.check_access_path(ap, location);
        }
        if matches!(e, Exp::Value(_)) && !value_ok {
            self.add_error(VerifyError::MisplacedValue {
                function: self.name.clone(),
                location,
            });
        }
    }

    /// In SSA form a store reads its base, so the base's version must be defined somewhere (or
    /// be version 0, the incoming value). See [`VerifyError::StoreDefinesBase`].
    fn check_store_bases(&mut self, idx: FunctionIdx, data: &FunctionData) {
        let mut defined: HashSet<VariableRef> = HashSet::new();
        for (_, block) in data.blocks.iter_enumerated() {
            for s in block.statements.iter() {
                defined.extend(s.iter_dst_var().cloned());
            }
        }
        for (bb, block) in data.blocks.iter_enumerated() {
            for (i, s) in block.statements.iter().enumerate() {
                if let StatementKind::Assign { dest, .. } = &s.kind
                    && !dest.is_pathless()
                    && dest.base.version.is_some_and(|v| v != 0)
                    && !defined.contains(&dest.base)
                {
                    self.add_error(VerifyError::StoreDefinesBase {
                        function: data.name.clone(),
                        location: Location {
                            function: idx,
                            block: bb,
                            statement_index: i,
                        },
                        base: dest.base.to_string(),
                    });
                }
            }
        }
    }
}

impl Visitor for CirVerify {
    fn visit_statement_kind(&mut self, kind: &StatementKind, location: Location) {
        self.super_statement_kind(kind, location);
        match kind {
            StatementKind::Assign { dest, sources } => {
                self.check_access_path(dest, location);
                if path::classify(&dest.segments) == PathClass::Address {
                    self.add_error(VerifyError::AddressDestination {
                        function: self.name.clone(),
                        location,
                        path: dest.to_string(),
                    });
                }
                // SSA's entry binding `$globals_0 = $globals` names the incoming heap; it is the
                // one copy into `$globals` there is.
                if dest.is_pathless()
                    && dest.base.variable == Variable::GlobalHeap
                    && dest.base.version != Some(0)
                {
                    self.add_error(VerifyError::EmptyGlobalsWrite {
                        function: self.name.clone(),
                        location,
                    });
                }
                for e in sources {
                    self.check_exp(e, true, location);
                }
            }
            StatementKind::CallAssign { style, rets, args } => {
                for e in args {
                    self.check_exp(e, false, location);
                }
                if let CallStyle::FuncPtrCall { callee, .. } = style {
                    self.check_access_path(callee, location);
                }
                if rets.iter().any(|r| r.variable == Variable::GlobalHeap) {
                    self.add_error(VerifyError::EmptyGlobalsWrite {
                        function: self.name.clone(),
                        location,
                    });
                }
            }
            _ => {}
        }
    }

    #[inline]
    fn visit_function_data(&mut self, idx: FunctionIdx, data: &FunctionData) {
        if data.name.is_empty() {
            self.add_error(VerifyError::UnnamedFunction { index: idx });
        }
        if !self.defines.insert(data.name.clone()) {
            self.add_error(VerifyError::MultiplyDefinedFunction { index: idx })
        }
        self.name = data.name.clone();
        self.num_blocks = Some(data.blocks.len());
        self.params_len = Some(ParameterIdx::new(data.num_parameters()));
        self.locals_len = Some(data.locals.len());
        self.return_arity = Some(data.return_type.arity);
        self.super_function_data(idx, data);
        self.check_store_bases(idx, data);
        // After visiting a function, clear state.
        self.name = "".to_string();
        self.num_blocks = None;
        self.params_len = None;
        self.locals_len = None;
        self.return_arity = None;
    }

    fn visit_basic_block_data(
        &mut self,
        function: FunctionIdx,
        block: BasicBlockIdx,
        data: &BasicBlockData,
    ) {
        self.super_basic_block_data(function, block, data);
        if data.terminator.is_none() {
            self.add_error(VerifyError::NoTerminator { function, block })
        }
    }

    #[inline]
    fn visit_variable_ref(&mut self, variable: &VariableRef) {
        self.super_variable_ref(variable);
        if self.params_len.is_none() {
            self.add_error(VerifyError::VariableRefOutsideFunction);
            return;
        }
        // Check that referenced variable exists.
        if let Variable::Param(idx) = &variable.variable
            && *idx >= self.params_len.unwrap()
        {
            self.add_error(VerifyError::ParameterDoesNotExist {
                function: self.name.clone(),
                parameter: *idx,
            });
        }
        // Check that referenced local exists.
        if let Variable::Local(idx) = &variable.variable
            && idx.index() >= self.locals_len.unwrap()
        {
            self.add_error(VerifyError::LocalDoesNotExist {
                function: self.name.clone(),
                local: *idx,
            });
        }
    }

    #[inline]
    fn visit_terminator_kind(&mut self, terminator: &TerminatorKind, location: Location) {
        self.super_terminator_kind(terminator, location);
        match terminator {
            TerminatorKind::Return { args } => {
                for e in args {
                    self.check_exp(e, false, location);
                }
                let Some(return_arity) = self.return_arity else {
                    panic!("bug in verify: no return arity")
                };
                if args.len() != return_arity as usize {
                    self.add_error(VerifyError::InconsistentReturns {
                        function: self.name.clone(),
                        expected_arity: return_arity,
                        actual_arity: args.len(),
                    });
                }
            }
            TerminatorKind::Goto { targets } => {
                if targets.is_empty() {
                    self.add_error(VerifyError::EmptyGoto {
                        function: self.name.clone(),
                        location,
                    });
                } else {
                    for target in targets {
                        if target.index() >= self.num_blocks.unwrap() {
                            self.add_error(VerifyError::BlockDoesNotExist {
                                function: self.name.clone(),
                                block: location.block,
                                target: *target,
                            })
                        }
                    }
                }
            }
        }
    }
}
