use crate::index::idx::Idx;
use crate::mir::call::CallStyle;
use crate::mir::terminator::{Terminator, TerminatorKind};
use crate::mir::{
    AccessPath, BasicBlockData, Exp, FieldAccesses, ParameterIdx, Statement, StatementIdx,
    StatementKind, VariableRef,
};

/// A builder for creating basic blocks with convenient methods for inserting statements.
///
/// The BasicBlockBuilder provides an API for constructing basic blocks by allowing insertion of
/// statements at specific positions, similar to LLVM's IRBuilder.
#[derive(Debug)]
pub struct BasicBlockBuilder<'a> {
    /// Mutable reference to the basic block being constructed
    block_data: &'a mut BasicBlockData,
    /// Current insertion point within the basic block
    insertion_point: usize,
}

impl<'a> BasicBlockBuilder<'a> {
    /// Create a new BasicBlockBuilder with given basic block
    pub fn new(block_data: &'a mut BasicBlockData) -> Self {
        Self {
            block_data,
            insertion_point: 0,
        }
    }

    /// Set the insertion point to a specific position
    ///
    /// # Arguments
    /// * `position` - The index at which to insert the next statement
    pub fn set_insertion_point(&mut self, position: usize) {
        self.insertion_point = position;
    }

    /// Get the current insertion point
    pub fn get_insertion_point(&self) -> usize {
        self.insertion_point
    }

    /// Insert a statement at the current insertion point and increment insertion point. If the
    /// insertion point is beyond current length, this is equivalent to a push.
    ///
    /// # Arguments
    /// * `statement` - The statement to insert
    pub fn insert_statement(&mut self, statement: Statement) {
        // Insert at the current insertion point
        if self.insertion_point <= self.block_data.statements.len() {
            // Use the IndexVecDeque::insert_at method directly
            self.block_data
                .statements
                .insert_at(StatementIdx::new(self.insertion_point), statement);
        } else {
            // If insertion point is beyond current length, push back
            self.block_data.statements.push_back(statement);
        }

        // Update insertion point to be after the inserted statement
        self.insertion_point += 1;
    }

    /// Create and insert an assignment statement
    ///
    /// # Arguments
    /// * `dest` - Destination variable
    /// * `sources` - Source expressions
    pub fn create_assign(
        &mut self,
        dest: VariableRef,
        sources: impl IntoIterator<Item = Exp>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::assign(dest, sources));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert an update statement
    ///
    /// # Arguments
    /// * `dest` - Destination access path
    /// * `source` - Source expression
    pub fn create_update(&mut self, dest: AccessPath, source: Exp) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::update(dest, source));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert an assign_or_update statement
    ///
    /// # Arguments
    /// * `dest` - Destination access path
    /// * `source` - Source expression
    pub fn create_assign_or_update(&mut self, dest: AccessPath, source: Exp) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::assign_or_update(dest, source));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a call statement
    ///
    /// # Arguments
    /// * `style` - Call style (direct, indirect, etc.)
    /// * `rets` - Return variables
    /// * `args` - Argument expressions
    pub fn create_call(
        &mut self,
        style: CallStyle,
        rets: impl IntoIterator<Item = VariableRef>,
        args: impl IntoIterator<Item = Exp>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: rets.into_iter().collect(),
            args: args.into_iter().collect(),
        });
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a return terminator
    ///
    /// # Arguments
    /// * `values` - Return values
    pub fn create_ret(&mut self, values: impl IntoIterator<Item = Exp>) {
        let terminator = Terminator::new_kind(TerminatorKind::Return {
            args: values.into_iter().collect(),
        });
        self.block_data.terminator = Some(terminator);
    }

    /// Create and insert a goto terminator
    ///
    /// # Arguments
    /// * `targets` - Target basic blocks
    pub fn create_goto(&mut self, targets: impl IntoIterator<Item = crate::mir::BasicBlockIdx>) {
        let terminator = Terminator::new_kind(TerminatorKind::Goto {
            targets: targets.into_iter().collect(),
        });
        self.block_data.terminator = Some(terminator);
    }

    /// Create and insert a phi statement
    ///
    /// # Arguments
    /// * `dest` - Destination variable
    /// * `operands` - Pairs of (basic block index, variable)
    pub fn create_phi(
        &mut self,
        dest: VariableRef,
        operands: impl IntoIterator<Item = (crate::mir::BasicBlockIdx, VariableRef)>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::Phi {
            dest,
            operands: operands.into_iter().collect(),
        });
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a param-flow statement
    ///
    /// # Arguments
    /// * `arity` - Number of parameters
    pub fn create_param_flow(&mut self, arity: usize) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::param_flow(arity));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a nop statement
    pub fn create_nop(&mut self) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::Nop);
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create a new local variable reference
    ///
    /// # Arguments
    /// * `name` - Variable name
    pub fn new_local_var(&self, name: &str) -> VariableRef {
        VariableRef::new_local(name.to_string())
    }

    /// Create a new parameter variable reference
    ///
    /// # Arguments
    /// * `param_idx` - Parameter index
    pub fn new_param_var(&self, param_idx: ParameterIdx) -> VariableRef {
        VariableRef::new_parameter(param_idx)
    }

    /// Create a new global heap variable reference
    pub fn new_global_var(&self) -> VariableRef {
        VariableRef::new_global()
    }

    /// Create a new access path
    ///
    /// # Arguments
    /// * `variable` - Variable reference
    /// * `fields` - Field access path
    pub fn new_access_path<S: AsRef<str>>(
        &self,
        variable_ref: VariableRef,
        fields: impl IntoIterator<Item = S>,
    ) -> AccessPath {
        AccessPath {
            variable_ref,
            path: fields.into_iter().collect(),
        }
    }

    /// Create a new field access path
    ///
    /// # Arguments
    /// * `fields` - Field names
    pub fn new_field_path<S: AsRef<str>>(
        &self,
        fields: impl IntoIterator<Item = S>,
    ) -> FieldAccesses {
        fields.into_iter().collect()
    }

    /// Create a new field access path with a single offset
    ///
    /// # Arguments
    /// * `offset` - Numeric offset
    pub fn new_offset_path(&self, offset: i64) -> FieldAccesses {
        FieldAccesses::with_offset(offset)
    }

    /// Create a new field access path with mixed field accesses
    ///
    /// # Arguments
    /// * `fields` - Sequence of either field names (Ok) or offsets (Err)
    pub fn new_mixed_field_path<S: AsRef<str>>(
        &self,
        fields: impl IntoIterator<Item = Result<S, u64>>,
    ) -> FieldAccesses {
        FieldAccesses::mixed(fields)
    }

    /// Create a string expression
    ///
    /// # Arguments
    /// * `s` - String value
    pub fn new_str_exp(&self, s: &str) -> Exp {
        Exp::new_str(s)
    }

    /// Create a bytes expression
    ///
    /// # Arguments
    /// * `bytes` - Byte values
    pub fn new_bytes_exp(&self, bytes: Vec<u8>) -> Exp {
        Exp::new_bytes(bytes)
    }
}
