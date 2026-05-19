use std::ops::{Deref, DerefMut};
use std::{fmt, fmt::Display};

use hashbrown::hash_set::HashSet;
use thiserror::Error;

use crate::index::idx::Idx;
use crate::mir::{
    BasicBlockData, BasicBlockIdx, FunctionData, FunctionIdx, Location, ParameterIdx,
    StatementKind, TerminatorKind, Variable, VariableRef, visit::Visitor,
};

/// These are the errors that CTADL IR verification detects. Any of these errors mean that the IR
/// is malformed and attempting to compile it may result in panics.
#[derive(Error, Debug, Eq, PartialEq)]
pub enum VerifyError {
    /// A function was found that has an empty name
    #[error("function has no name: {}", index.index())]
    UnnamedFunction { index: FunctionIdx },

    #[error("multiply-defined function: {}", index.index())]
    MultiplyDefinedFunction { index: FunctionIdx },

    /// An Update instruction that doesn't update any field
    #[error("update with no field: {location}")]
    EmptyFieldUpdate { location: Location },

    /// Parameter reference found outside the bounds of declared parameters
    #[error("in function: {function}: reference to nonexistent parameter: '{}'", parameter.index())]
    ParameterDoesNotExist {
        function: String,
        parameter: ParameterIdx,
    },

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
}

/// A list of verification errors
#[derive(Debug, Default, Eq, PartialEq)]
pub struct VerifyErrors {
    errors: Vec<VerifyError>,
}

impl std::error::Error for VerifyErrors {}

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
pub struct MirVerify {
    error: VerifyErrors,
    /// Name of the last function
    name: String,
    /// Number of blocks in last function
    num_blocks: Option<usize>,
    /// Length of the last function's parameter list
    params_len: Option<ParameterIdx>,
    /// Length of return values from function
    return_arity: Option<u8>,
    defines: HashSet<String>,
}

impl MirVerify {
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

impl Visitor for MirVerify {
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
        self.return_arity = Some(data.return_type.arity);
        self.super_function_data(idx, data);
        // After visiting a function, clear state.
        self.name = "".to_string();
        self.num_blocks = None;
        self.params_len = None;
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
        if let Variable::Param(idx) = variable.variable.as_ref()
            && *idx >= self.params_len.unwrap()
        {
            //self.error = Some(Box::new(VerifyError::ParameterDoesNotExist));
            self.add_error(VerifyError::ParameterDoesNotExist {
                function: self.name.clone(),
                parameter: *idx,
            });
        }
    }

    #[inline]
    fn visit_statement_kind(&mut self, statement: &StatementKind, location: Location) {
        self.super_statement_kind(statement, location);
    }

    #[inline]
    fn visit_terminator_kind(&mut self, terminator: &TerminatorKind, location: Location) {
        self.super_terminator_kind(terminator, location);
        match terminator {
            TerminatorKind::Return { args } => {
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
