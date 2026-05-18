use std::collections::HashSet;

use crate::index::idx::Idx;
use crate::index::index_vec_deque::IndexVecDeque;
use crate::mir::visit::{MutVisitor, Visitor};
use crate::mir::{
    AccessPath, BasicBlockData, BasicBlockIdx, Exp, FieldAccess, FieldAccesses, FunctionData,
    FunctionIdx, Program, Statement, StatementIdx, StatementKind, Terminator, TerminatorKind,
    Variable, VariableRef,
};

pub fn transform_program(program: &mut Program) {
    for (_, function) in program.functions.iter_enumerated_mut() {
        transform(function);
    }
}

pub fn transform(function: &mut FunctionData) {
    if function.blocks.is_empty() {
        return;
    }
    let mut lower = MemLower::for_function(function);
    lower.visit_function_data(FunctionIdx::new(0), function);
}

#[derive(Debug)]
struct MemLower {
    used_names: HashSet<String>,
    next_temp: usize,
}

impl MemLower {
    fn for_function(function: &FunctionData) -> Self {
        let mut collector = LocalNameCollector::default();
        collector.visit_function_data(FunctionIdx::new(0), function);
        Self {
            used_names: collector.locals,
            next_temp: 0,
        }
    }

    fn fresh_temp(&mut self) -> VariableRef {
        loop {
            let name = format!("_$mem{}", self.next_temp);
            self.next_temp += 1;
            if self.used_names.insert(name.clone()) {
                return VariableRef::new_local(name);
            }
        }
    }

    fn lower_access_path_to_var(
        &mut self,
        access_path: AccessPath,
        out: &mut Vec<Statement>,
        source_info: crate::mir::SourceInfo,
    ) -> VariableRef {
        let AccessPath { variable_ref, path } = access_path;
        let mut current = variable_ref;
        for field in path.fields {
            let temp = self.fresh_temp();
            out.push(Statement::new(
                StatementKind::Load {
                    dest: temp.clone(),
                    source: current,
                    field,
                },
                source_info,
            ));
            current = temp;
        }
        current
    }

    fn lower_exp_for_assign_like(
        &mut self,
        exp: Exp,
        out: &mut Vec<Statement>,
        source_info: crate::mir::SourceInfo,
    ) -> Exp {
        match exp {
            Exp::AccessPath(access_path) if !access_path.path.is_empty() => {
                let value = self.lower_access_path_to_var(access_path, out, source_info);
                Exp::AccessPath(AccessPath::without_fields(value))
            }
            other => other,
        }
    }

    fn lower_exp_to_var(
        &mut self,
        exp: Exp,
        out: &mut Vec<Statement>,
        source_info: crate::mir::SourceInfo,
    ) -> VariableRef {
        match exp {
            Exp::AccessPath(access_path) => {
                if access_path.path.is_empty() {
                    access_path.variable_ref
                } else {
                    self.lower_access_path_to_var(access_path, out, source_info)
                }
            }
            other => {
                let temp = self.fresh_temp();
                out.push(Statement::new(
                    StatementKind::assign(temp.clone(), [other]),
                    source_info,
                ));
                temp
            }
        }
    }

    fn lower_call_style(
        &mut self,
        style: crate::mir::CallStyle,
        out: &mut Vec<Statement>,
        source_info: crate::mir::SourceInfo,
    ) -> crate::mir::CallStyle {
        match style {
            crate::mir::CallStyle::FuncPtrCall { callee, signature } if !callee.path.is_empty() => {
                let callee = self.lower_access_path_to_var(callee, out, source_info);
                crate::mir::CallStyle::FuncPtrCall {
                    callee: AccessPath::without_fields(callee),
                    signature,
                }
            }
            other => other,
        }
    }

    fn lower_nested_store(
        &mut self,
        root: VariableRef,
        fields: FieldAccesses,
        value: VariableRef,
        out: &mut Vec<Statement>,
        source_info: crate::mir::SourceInfo,
    ) {
        debug_assert!(!fields.is_empty());

        let mut current = root;
        let mut parents: Vec<(VariableRef, FieldAccess, VariableRef)> = Vec::new();
        let last_index = fields.len() - 1;

        for field in fields.fields[..last_index].iter().cloned() {
            let temp = self.fresh_temp();
            out.push(Statement::new(
                StatementKind::Load {
                    dest: temp.clone(),
                    source: current.clone(),
                    field: field.clone(),
                },
                source_info,
            ));
            parents.push((current, field, temp.clone()));
            current = temp;
        }

        let field = fields.fields[last_index].clone();
        out.push(Statement::new(
            StatementKind::Store {
                dest: current,
                field,
                value,
            },
            source_info,
        ));

        for (base, field, temp) in parents.into_iter().rev() {
            out.push(Statement::new(
                StatementKind::Store {
                    dest: base,
                    field,
                    value: temp,
                },
                source_info,
            ));
        }
    }

    fn lower_statement(&mut self, statement: Statement) -> Vec<Statement> {
        let mut out = Vec::new();
        let source_info = statement.source_info;
        match statement.kind {
            StatementKind::Assign { dest, sources } => {
                let sources = sources
                    .into_iter()
                    .map(|src| self.lower_exp_for_assign_like(src, &mut out, source_info))
                    .collect();
                out.push(Statement::new(
                    StatementKind::Assign { dest, sources },
                    source_info,
                ));
            }
            StatementKind::CallAssign { style, rets, args } => {
                let style = self.lower_call_style(style, &mut out, source_info);
                let args = args
                    .into_iter()
                    .map(|arg| self.lower_exp_for_assign_like(arg, &mut out, source_info))
                    .collect();
                out.push(Statement::new(
                    StatementKind::CallAssign { style, rets, args },
                    source_info,
                ));
            }
            StatementKind::Update {
                dest: (dest, fields),
                source,
                value,
            } => {
                let value = self.lower_exp_to_var(value, &mut out, source_info);
                out.push(Statement::new(
                    StatementKind::assign(
                        dest.clone(),
                        [Exp::AccessPath(AccessPath::without_fields(source))],
                    ),
                    source_info,
                ));
                self.lower_nested_store(dest, fields, value, &mut out, source_info);
            }
            other => out.push(Statement::new(other, source_info)),
        }
        out
    }

    fn lower_terminator(&mut self, terminator: Terminator) -> (Vec<Statement>, Terminator) {
        let mut out = Vec::new();
        let source_info = terminator.source_info;
        let kind = match terminator.kind {
            TerminatorKind::Return { args } => {
                let args = args
                    .into_iter()
                    .map(|arg| self.lower_exp_for_assign_like(arg, &mut out, source_info))
                    .collect();
                TerminatorKind::Return { args }
            }
            TerminatorKind::Goto { targets } => TerminatorKind::Goto { targets },
        };
        (
            out,
            Terminator::new_kind(kind).with_source_info(source_info),
        )
    }
}

impl MutVisitor for MemLower {
    fn visit_basic_block_data(
        &mut self,
        _function: FunctionIdx,
        _block: BasicBlockIdx,
        data: &mut BasicBlockData,
    ) {
        let statements: Vec<_> = data.statements.drain(..).collect();
        let mut lowered = IndexVecDeque::<StatementIdx, Statement>::new();
        for statement in statements {
            for statement in self.lower_statement(statement) {
                lowered.push_back(statement);
            }
        }
        data.statements = lowered;

        if let Some(terminator) = data.terminator.take() {
            let (statements, terminator) = self.lower_terminator(terminator);
            for statement in statements {
                data.statements.push_back(statement);
            }
            data.terminator = Some(terminator);
        }
    }
}

#[derive(Debug, Default)]
struct LocalNameCollector {
    locals: HashSet<String>,
}

impl Visitor for LocalNameCollector {
    fn visit_variable_ref(&mut self, variable_ref: &VariableRef) {
        if let Variable::Local(name) = variable_ref.variable.as_ref() {
            self.locals.insert(name.clone());
        }
    }
}

trait TerminatorExt {
    fn with_source_info(self, source_info: crate::mir::SourceInfo) -> Self;
}

impl TerminatorExt for Terminator {
    fn with_source_info(mut self, source_info: crate::mir::SourceInfo) -> Self {
        self.source_info = source_info;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::FunctionBuilder;
    use crate::mir::visit::Visitor;
    use crate::mir::{BasicBlocks, CallEdges, CallStyle, Params, ReturnType};
    use smallvec::smallvec;

    fn new_test_function(name: &str, return_arity: u8) -> FunctionData {
        FunctionData::new(
            name,
            Params::default(),
            BasicBlocks::default(),
            ReturnType {
                arity: return_arity,
            },
        )
    }

    fn field(name: &str) -> FieldAccess {
        FieldAccess::Symbol(name.into())
    }

    #[derive(Default)]
    struct UnversionedVarCollector {
        refs: Vec<VariableRef>,
    }

    impl Visitor for UnversionedVarCollector {
        fn visit_variable_ref(&mut self, variable_ref: &VariableRef) {
            if variable_ref.version.is_none() {
                self.refs.push(variable_ref.clone());
            }
        }
    }

    #[test]
    fn lowers_access_paths_to_load_store() {
        let mut function = new_test_function("mem_lower_test", 1);
        let entry = function.blocks.new_block();
        {
            let mut function_builder = FunctionBuilder::new(&mut function);
            let mut builder = function_builder.at_block(entry);

            let out = builder.new_local_var("out");
            let src = builder.new_local_var("src");
            let ret = builder.new_local_var("ret");
            let mid = builder.new_local_var("mid");
            let root = builder.new_local_var("root");
            let root_next = builder.new_local_var("root_next");
            let rhs = builder.new_local_var("rhs");

            builder.create_assign(
                out,
                [Exp::AccessPath(builder.new_access_path(src, ["a", "b"]))],
            );
            builder.create_call(
                CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(vec!["callee".to_string()].into()),
                },
                [ret],
                [
                    Exp::AccessPath(builder.new_access_path(mid, ["c"])),
                    builder.new_str_exp("literal"),
                ],
            );
            builder.insert_statement(Statement::new_kind(StatementKind::Update {
                dest: (root_next.clone(), FieldAccesses::from_iter(["x", "y"])),
                source: root,
                value: Exp::AccessPath(builder.new_access_path(rhs, ["z"])),
            }));
            builder.create_ret([Exp::AccessPath(
                builder.new_access_path(root_next, ["x", "y"]),
            )]);
        }

        eprintln!("before mem_lower: {}", &function);
        transform(&mut function);
        function.verify().unwrap();
        eprintln!("after mem_lower: {}", &function);

        let statements: Vec<_> = function.blocks[BasicBlockIdx::START_BLOCK]
            .statements
            .iter()
            .map(|statement| statement.kind.clone())
            .collect();

        assert!(
            statements
                .iter()
                .all(|statement| !matches!(statement, StatementKind::Update { .. }))
        );

        let mut load_count = 0;
        let mut store_count = 0;

        for statement in &statements {
            match statement {
                StatementKind::Assign { sources, .. } => {
                    assert!(sources.iter().all(|src| match src {
                        Exp::AccessPath(path) => path.path.is_empty(),
                        _ => true,
                    }));
                }
                StatementKind::CallAssign { args, .. } => {
                    assert!(args.iter().all(|arg| match arg {
                        Exp::AccessPath(path) => path.path.is_empty(),
                        _ => true,
                    }));
                }
                StatementKind::Load { .. } => load_count += 1,
                StatementKind::Store { .. } => store_count += 1,
                _ => {}
            }
        }

        let TerminatorKind::Return { args } = &function.blocks[BasicBlockIdx::START_BLOCK]
            .terminator()
            .kind
        else {
            panic!("expected return terminator");
        };
        assert!(args.iter().all(|arg| match arg {
            Exp::AccessPath(path) => path.path.is_empty(),
            _ => true,
        }));

        assert_eq!(load_count, 7);
        assert_eq!(store_count, 2);

        let rendered = function.to_string();
        assert!(rendered.contains("load"));
        assert!(rendered.contains("store"));
        assert!(!rendered.contains("update"));
    }

    #[test]
    fn lowers_each_nested_assign_source_in_order() {
        let mut function = new_test_function("mem_lower_assign_multi", 0);
        let entry = function.blocks.new_block();
        {
            let mut function_builder = FunctionBuilder::new(&mut function);
            let mut builder = function_builder.at_block(entry);

            let out = builder.new_local_var("out");
            let left = builder.new_local_var("left");
            let right = builder.new_local_var("right");
            let plain = builder.new_local_var("plain");

            builder.create_assign(
                out,
                [
                    Exp::AccessPath(builder.new_access_path(left, ["head", "tail"])),
                    Exp::AccessPath(builder.new_access_path(right, ["slot"])),
                    Exp::AccessPath(AccessPath::without_fields(plain)),
                ],
            );
            builder.create_ret([]);
        }

        transform(&mut function);
        function.verify().unwrap();

        let statements: Vec<_> = function.blocks[BasicBlockIdx::START_BLOCK]
            .statements
            .iter()
            .map(|statement| statement.kind.clone())
            .collect();

        assert_eq!(statements.len(), 4);
        assert_eq!(
            statements[0],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem0".to_string()),
                source: VariableRef::new_local("left".to_string()),
                field: field("head"),
            }
        );
        assert_eq!(
            statements[1],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem1".to_string()),
                source: VariableRef::new_local("_$mem0".to_string()),
                field: field("tail"),
            }
        );
        assert_eq!(
            statements[2],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem2".to_string()),
                source: VariableRef::new_local("right".to_string()),
                field: field("slot"),
            }
        );
        assert_eq!(
            statements[3],
            StatementKind::Assign {
                dest: VariableRef::new_local("out".to_string()),
                sources: smallvec![
                    Exp::AccessPath(AccessPath::without_fields(VariableRef::new_local(
                        "_$mem1".to_string()
                    ))),
                    Exp::AccessPath(AccessPath::without_fields(VariableRef::new_local(
                        "_$mem2".to_string()
                    ))),
                    Exp::AccessPath(AccessPath::without_fields(VariableRef::new_local(
                        "plain".to_string()
                    ))),
                ],
            }
        );
    }

    #[test]
    fn lowers_funcptr_callee_args_and_return_access_paths() {
        let mut function = new_test_function("mem_lower_funcptr", 1);
        let entry = function.blocks.new_block();
        {
            let mut function_builder = FunctionBuilder::new(&mut function);
            let mut builder = function_builder.at_block(entry);

            let ret = builder.new_local_var("ret");
            let fp = builder.new_local_var("fp");
            let arg = builder.new_local_var("arg");
            let plain = builder.new_local_var("plain");
            let out = builder.new_local_var("out");

            builder.create_call(
                CallStyle::FuncPtrCall {
                    callee: builder.new_access_path(fp, ["dispatch", "target"]),
                    signature: Some("void (*)(void *)".to_string()),
                },
                [ret],
                [
                    Exp::AccessPath(builder.new_access_path(arg, ["payload"])),
                    Exp::AccessPath(AccessPath::without_fields(plain)),
                ],
            );
            builder.create_ret([Exp::AccessPath(builder.new_access_path(out, ["result"]))]);
        }

        transform(&mut function);
        function.verify().unwrap();

        let statements: Vec<_> = function.blocks[BasicBlockIdx::START_BLOCK]
            .statements
            .iter()
            .map(|statement| statement.kind.clone())
            .collect();

        assert_eq!(statements.len(), 5);
        assert_eq!(
            statements[0],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem0".to_string()),
                source: VariableRef::new_local("fp".to_string()),
                field: field("dispatch"),
            }
        );
        assert_eq!(
            statements[1],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem1".to_string()),
                source: VariableRef::new_local("_$mem0".to_string()),
                field: field("target"),
            }
        );
        assert_eq!(
            statements[2],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem2".to_string()),
                source: VariableRef::new_local("arg".to_string()),
                field: field("payload"),
            }
        );
        assert_eq!(
            statements[3],
            StatementKind::CallAssign {
                style: CallStyle::FuncPtrCall {
                    callee: AccessPath::without_fields(VariableRef::new_local(
                        "_$mem1".to_string()
                    )),
                    signature: Some("void (*)(void *)".to_string()),
                },
                rets: smallvec![VariableRef::new_local("ret".to_string())],
                args: smallvec![
                    Exp::AccessPath(AccessPath::without_fields(VariableRef::new_local(
                        "_$mem2".to_string()
                    ))),
                    Exp::AccessPath(AccessPath::without_fields(VariableRef::new_local(
                        "plain".to_string()
                    ))),
                ],
            }
        );
        assert_eq!(
            statements[4],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem3".to_string()),
                source: VariableRef::new_local("out".to_string()),
                field: field("result"),
            }
        );

        let TerminatorKind::Return { args } = &function.blocks[BasicBlockIdx::START_BLOCK]
            .terminator()
            .kind
        else {
            panic!("expected return terminator");
        };
        assert_eq!(
            args.as_slice(),
            &[Exp::AccessPath(AccessPath::without_fields(
                VariableRef::new_local("_$mem3".to_string())
            ))]
        );
    }

    #[test]
    fn lowers_update_with_non_access_value_and_deep_field_chain() {
        let mut function = new_test_function("mem_lower_update", 0);
        let entry = function.blocks.new_block();
        {
            let mut function_builder = FunctionBuilder::new(&mut function);
            let mut builder = function_builder.at_block(entry);

            let root = builder.new_local_var("root");
            let root_next = builder.new_local_var("root_next");

            builder.insert_statement(Statement::new_kind(StatementKind::Update {
                dest: (
                    root_next,
                    FieldAccesses::from_iter(["left", "right", "leaf"]),
                ),
                source: root,
                value: Exp::new_str("literal"),
            }));
            builder.create_ret([]);
        }

        eprintln!("before mem_lower: {}", &function);
        transform(&mut function);
        function.verify().unwrap();
        eprintln!("after mem_lower: {}", &function);

        let statements: Vec<_> = function.blocks[BasicBlockIdx::START_BLOCK]
            .statements
            .iter()
            .map(|statement| statement.kind.clone())
            .collect();

        assert_eq!(statements.len(), 7);
        assert_eq!(
            statements[0],
            StatementKind::Assign {
                dest: VariableRef::new_local("_$mem0".to_string()),
                sources: smallvec![Exp::new_str("literal")],
            }
        );
        assert_eq!(
            statements[1],
            StatementKind::Assign {
                dest: VariableRef::new_local("root_next".to_string()),
                sources: smallvec![Exp::AccessPath(AccessPath::without_fields(
                    VariableRef::new_local("root".to_string())
                ))],
            }
        );
        assert_eq!(
            statements[2],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem1".to_string()),
                source: VariableRef::new_local("root_next".to_string()),
                field: field("left"),
            }
        );
        assert_eq!(
            statements[3],
            StatementKind::Load {
                dest: VariableRef::new_local("_$mem2".to_string()),
                source: VariableRef::new_local("_$mem1".to_string()),
                field: field("right"),
            }
        );
        assert_eq!(
            statements[4],
            StatementKind::Store {
                dest: VariableRef::new_local("_$mem2".to_string()),
                field: field("leaf"),
                value: VariableRef::new_local("_$mem0".to_string()),
            }
        );
        assert_eq!(
            statements[5],
            StatementKind::Store {
                dest: VariableRef::new_local("_$mem1".to_string()),
                field: field("right"),
                value: VariableRef::new_local("_$mem2".to_string()),
            }
        );
        assert_eq!(
            function.blocks[BasicBlockIdx::START_BLOCK].statements[StatementIdx::new(6)].kind,
            StatementKind::Store {
                dest: VariableRef::new_local("root_next".to_string()),
                field: field("left"),
                value: VariableRef::new_local("_$mem1".to_string()),
            }
        );
    }

    #[test]
    fn lowered_mir_survives_ssa_transform() {
        let mut function = new_test_function("mem_lower_then_ssa", 1);
        function.params.push(crate::mir::ParameterType::ByVal);
        let entry = function.blocks.new_block();
        {
            let mut function_builder = FunctionBuilder::new(&mut function);
            let mut builder = function_builder.at_block(entry);

            let param0 = builder.new_param_var(crate::mir::ParameterIdx::new(0));
            let out = builder.new_local_var("out");
            let ret = builder.new_local_var("ret");
            let state = builder.new_local_var("state");
            let state_next = builder.new_local_var("state_next");

            builder.create_assign(
                out.clone(),
                [
                    Exp::AccessPath(builder.new_access_path(param0.clone(), ["a", "b"])),
                    Exp::AccessPath(builder.new_access_path(state.clone(), ["head"])),
                ],
            );
            builder.create_call(
                CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(vec!["callee".to_string()].into()),
                },
                [ret.clone()],
                [Exp::AccessPath(builder.new_access_path(out, ["tail"]))],
            );
            builder.insert_statement(Statement::new_kind(StatementKind::Update {
                dest: (
                    state_next.clone(),
                    FieldAccesses::from_iter(["slot", "leaf"]),
                ),
                source: state,
                value: Exp::AccessPath(builder.new_access_path(ret.clone(), ["next"])),
            }));
            builder.create_ret([Exp::AccessPath(
                builder.new_access_path(state_next, ["slot", "leaf"]),
            )]);
        }

        eprintln!("Before: {}", function);
        transform(&mut function);
        function.verify().unwrap();

        crate::ssa::transform(&mut function, false);
        function.verify().unwrap();
        eprintln!("After: {}", function);

        let rendered = function.to_string();
        assert!(rendered.contains("param-flow"));

        let mut collector = UnversionedVarCollector::default();
        collector.visit_function_data(FunctionIdx::new(0), &function);
        assert!(
            collector.refs.iter().all(|var| {
                matches!(
                    var.variable.as_ref(),
                    Variable::GlobalHeap | Variable::Param(crate::mir::ParameterIdx(0))
                )
            }),
            "found unexpected unversioned vars after SSA: {:?}",
            collector.refs
        );
    }
}
