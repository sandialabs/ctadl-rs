use crate::index::idx::Idx;
use crate::mir::call::CallStyle;
use crate::mir::terminator::{Terminator, TerminatorKind};
use crate::mir::{
    AccessPath, BasicBlockData, BasicBlockIdx, Exp, FieldRef, FunctionData, LocalIdx, Locals,
    OffsetAccesses, ParameterIdx, ParameterType, Statement, StatementIdx, StatementKind,
    VariableRef,
};

/// A builder for creating functions.
#[derive(Debug)]
pub struct FunctionBuilder<'a> {
    function: &'a mut FunctionData,
}

impl<'a> FunctionBuilder<'a> {
    /// Create a new FunctionBuilder wrapping an existing FunctionData
    pub fn new(function: &'a mut FunctionData) -> Self {
        Self { function }
    }

    /// Add a parameter to the function
    pub fn add_param(&mut self, param_type: ParameterType) -> ParameterIdx {
        self.function.params.push(param_type)
    }

    /// Add a new basic block to the function
    pub fn add_block(&mut self) -> BasicBlockIdx {
        self.function.blocks.new_block()
    }

    /// Get a builder for a specific basic block
    pub fn at_block(&mut self, block_idx: BasicBlockIdx) -> BasicBlockBuilder<'_> {
        // Disjoint borrow of the two separate fields of `FunctionData` so the block builder can
        // intern locals into the same function's table while building a block.
        BasicBlockBuilder::new(
            &mut self.function.blocks[block_idx],
            &mut self.function.locals,
        )
    }

    /// Intern a local by name into the function's locals table, returning its index.
    pub fn intern_local(&mut self, name: &str) -> LocalIdx {
        self.function.intern_local(name)
    }

    /// Set the name of the function
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.function.name = name.into();
    }

    /// Set the return arity of the function
    pub fn set_return_arity(&mut self, arity: u8) {
        self.function.return_type.arity = arity;
    }
}

/// A builder for creating basic blocks with convenient methods for inserting statements.
///
/// The BasicBlockBuilder provides an API for constructing basic blocks by allowing insertion of
/// statements at specific positions, similar to LLVM's IRBuilder.
#[derive(Debug)]
pub struct BasicBlockBuilder<'a> {
    /// Mutable reference to the basic block being constructed
    block_data: &'a mut BasicBlockData,
    /// Mutable reference to the enclosing function's locals table, for interning local names
    locals: &'a mut Locals,
    /// Current insertion point within the basic block
    insertion_point: usize,
}

impl<'a> BasicBlockBuilder<'a> {
    /// Create a new BasicBlockBuilder with given basic block and locals table
    pub fn new(block_data: &'a mut BasicBlockData, locals: &'a mut Locals) -> Self {
        Self {
            block_data,
            locals,
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

    /// Create and insert a load statement `dest = source.field`.
    ///
    /// # Arguments
    /// * `dest` - Destination variable
    /// * `source` - Source address (offset-only access path)
    /// * `field` - Symbolic field to load
    pub fn create_load(
        &mut self,
        dest: VariableRef,
        source: impl Into<AccessPath>,
        field: impl Into<FieldRef>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::load(dest, source, field));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a store statement `store dest.field := source`.
    ///
    /// # Arguments
    /// * `dest` - Destination address (offset-only access path)
    /// * `field` - Symbolic field written
    /// * `source` - Source expression
    pub fn create_store(
        &mut self,
        dest: impl Into<AccessPath>,
        field: impl Into<FieldRef>,
        source: impl Into<Exp>,
    ) -> StatementIdx {
        let statement =
            Statement::new_kind(StatementKind::store(dest.into(), field, source.into()));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert an assign (`field` is `None`) or a store of `field` into `dest` (see
    /// [`StatementKind::assign_or_store`]). Storing to an offset address with no field is an error.
    ///
    /// # Arguments
    /// * `dest` - Destination access path (offset-only)
    /// * `field` - Symbolic field written, or `None` for a plain assign to a bare variable
    /// * `source` - Source expression
    pub fn create_assign_or_store(
        &mut self,
        dest: impl Into<AccessPath>,
        field: Option<FieldRef>,
        source: impl Into<Exp>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::assign_or_store(
            dest.into(),
            field,
            source.into(),
        ));
        let current_pos = self.insertion_point;
        self.insert_statement(statement);
        StatementIdx::from(current_pos as u32)
    }

    /// Create and insert a functional update `dest = update (source, dest.field := value)` (see
    /// [`StatementKind::Update`]). Unlike [`Self::create_store`], the `dest` variable is defined (a
    /// new version of the aggregate), so the `source` aggregate is named separately.
    ///
    /// # Arguments
    /// * `dest` - Destination address (offset-only access path); its variable is (re)defined
    /// * `source` - Source aggregate copied into `dest` before the field write
    /// * `field` - Symbolic field written
    /// * `value` - Source expression stored into the field
    pub fn create_update(
        &mut self,
        dest: impl Into<AccessPath>,
        source: VariableRef,
        field: impl Into<FieldRef>,
        value: impl Into<Exp>,
    ) -> StatementIdx {
        let statement = Statement::new_kind(StatementKind::update(
            dest.into(),
            source,
            field,
            value.into(),
        ));
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

    /// Create a new local variable reference, interning `name` into the function's locals table
    ///
    /// # Arguments
    /// * `name` - Variable name
    pub fn new_local_var(&mut self, name: &str) -> VariableRef {
        VariableRef::new_local_idx(self.locals.get_or_intern(name))
    }

    /// Intern a local by name into the function's locals table, returning its index.
    pub fn intern_local(&mut self, name: &str) -> LocalIdx {
        self.locals.get_or_intern(name)
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

    /// Create a new offset-only access path
    ///
    /// # Arguments
    /// * `variable` - Variable reference
    /// * `offsets` - Offset (pointer-arithmetic) field accesses
    pub fn new_access_path(
        &self,
        variable_ref: VariableRef,
        offsets: impl IntoIterator<Item = i64>,
    ) -> AccessPath {
        AccessPath {
            base: variable_ref,
            accesses: OffsetAccesses::with_offsets(offsets),
        }
    }

    /// Create a new access path with a single offset
    ///
    /// # Arguments
    /// * `offset` - Numeric offset
    pub fn new_offset_path(&self, offset: i64) -> OffsetAccesses {
        OffsetAccesses::with_offset(offset)
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

    /// Create an integer expression
    ///
    /// # Arguments
    /// * `value` - The constant's value, sign-extended to `i64`
    pub fn new_int_exp(&self, value: i64) -> Exp {
        Exp::new_int(value)
    }
}
