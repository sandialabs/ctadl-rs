use hashbrown::HashSet;

use ctadl_ir::Idx;
use ctadl_ir::index::index_vec_deque::IndexVecDeque;
use ctadl_ir::mir::visit::MutVisitor;
use ctadl_ir::mir::*;

#[derive(Debug, Default)]
pub struct LowerAccessPaths {
    used_locals: HashSet<String>,
    next_temp: usize,
}

impl LowerAccessPaths {
    pub fn lower_function(function: &mut FunctionData) {
        let mut visitor = Self::default();
        visitor.visit_function_data(FunctionIdx::new(0), function);
    }

    fn fresh_temp(&mut self) -> VariableRef {
        loop {
            let name = format!("__ap{}", self.next_temp);
            self.next_temp += 1;
            if self.used_locals.insert(name.clone()) {
                return VariableRef::new_local(name);
            }
        }
    }

    fn collect_used_locals(&mut self, function: &FunctionData) {
        self.used_locals.clear();
        for block in function.blocks.iter() {
            for statement in block.statements.iter() {
                self.collect_statement_locals(statement);
            }
            if let Some(terminator) = &block.terminator {
                self.collect_terminator_locals(terminator);
            }
        }
    }

    fn collect_statement_locals(&mut self, statement: &Statement) {
        for var in statement.iter_src_var().chain(statement.iter_dst_var()) {
            self.collect_local(var);
        }
    }

    fn collect_terminator_locals(&mut self, terminator: &Terminator) {
        if let TerminatorKind::Return { args } = &terminator.kind {
            for arg in args {
                if let Exp::AccessPath(ap) = arg {
                    self.collect_local(&ap.variable_ref);
                }
            }
        }
    }

    fn collect_local(&mut self, variable: &VariableRef) {
        if let Variable::Local(name) = variable.variable.as_ref() {
            self.used_locals.insert(name.clone());
        }
    }

    fn lower_exp(&mut self, exp: &mut Exp, source_info: SourceInfo, out: &mut Vec<Statement>) {
        let Some(ap) = exp.access_path().cloned() else {
            return;
        };
        if ap.path.len() <= 1 {
            return;
        }

        let mut base = ap.variable_ref;
        let path = ap.path.fields;
        for field in path.iter().take(path.len() - 1).cloned() {
            let tmp = self.fresh_temp();
            out.push(Statement::new(
                StatementKind::assign(
                    tmp.clone(),
                    [Exp::AccessPath(AccessPath::new(base, [field]))],
                ),
                source_info,
            ));
            base = tmp;
        }

        let last = path[path.len() - 1].clone();
        *exp = Exp::AccessPath(AccessPath::new(base, [last]));
    }
}

impl MutVisitor for LowerAccessPaths {
    fn visit_function_data(&mut self, idx: FunctionIdx, function_data: &mut FunctionData) {
        self.collect_used_locals(function_data);
        self.next_temp = 0;
        self.super_function_data(idx, function_data);
    }

    fn visit_basic_block_data(
        &mut self,
        _function: FunctionIdx,
        _block: BasicBlockIdx,
        basic_block_data: &mut BasicBlockData,
    ) {
        let statements = std::mem::take(&mut basic_block_data.statements);
        let mut lowered = IndexVecDeque::with_capacity(statements.len());

        for (_, mut statement) in statements.into_iter_enumerated() {
            let mut prefix = Vec::new();
            match &mut statement.kind {
                StatementKind::Assign { sources, .. } => {
                    for source in sources {
                        self.lower_exp(source, statement.source_info, &mut prefix);
                    }
                }
                StatementKind::Update { value, .. } => {
                    self.lower_exp(value, statement.source_info, &mut prefix);
                }
                StatementKind::CallAssign { args, .. } => {
                    for arg in args {
                        self.lower_exp(arg, statement.source_info, &mut prefix);
                    }
                }
                StatementKind::Phi { .. }
                | StatementKind::ParamFlow { .. }
                | StatementKind::Nop => {}
            }

            for statement in prefix {
                lowered.push_back(statement);
            }
            lowered.push_back(statement);
        }

        basic_block_data.statements = lowered;
    }
}
