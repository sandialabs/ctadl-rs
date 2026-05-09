use internment::ArcIntern;
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node, Tree};

use crate::error::PhpReaderError;
use crate::evaluator::Evaluator;
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::{
    self, AccessPath, BasicBlocks, Exp, FunctionData, Params, ProgramInfo, ReturnType, VariableRef,
    builder::BasicBlockBuilder,
    call::{CallStyle, PhpCallKind, PhpClass, PhpMethod, PhpMethodName, VirtualMethodTable},
};
use source_info::{
    ArtifactEncoding, ArtifactId, ArtifactMetadata, ArtifactRecord, FileEntry, FileId, FileSpan,
    FileSpanId, Span, SpanId, SpanLen,
};

pub struct Lowerer<'a, 'p> {
    source: &'a str,
    file_name: &'a str,
    source_path: &'a str,
    program_info: &'p mut ProgramInfo,
    vmt_methods: BTreeSet<(PhpClass, PhpMethodName, PhpMethod)>,
    vmt_hierarchy: BTreeMap<PhpClass, SmallVec<[PhpClass; 2]>>,
    vmt_aliases: BTreeMap<mir::Symbol, mir::Symbol>,
    current_file_id: Option<FileId>,
    span_cache: BTreeMap<(u32, u32), FileSpanId>,
    evaluator: Evaluator,
}

impl<'a, 'p> Lowerer<'a, 'p> {
    pub fn new(
        source: &'a str,
        file_name: &'a str,
        source_path: &'a str,
        program_info: &'p mut ProgramInfo,
    ) -> Self {
        Self {
            source,
            file_name,
            source_path,
            program_info,
            vmt_methods: BTreeSet::new(),
            vmt_hierarchy: BTreeMap::new(),
            vmt_aliases: BTreeMap::new(),
            current_file_id: None,
            span_cache: BTreeMap::new(),
            evaluator: Evaluator::new(),
        }
    }

    pub fn lower(&mut self, tree: &Tree) -> Result<(), PhpReaderError> {
        let root = tree.root_node();

        // Pass 1: Collect symbols
        self.collect_symbols(root, String::new(), None)?;

        // Pass 2: Lower bodies
        self.lower_bodies(root)?;

        self.extend_vmt()?;
        Ok(())
    }

    fn extend_vmt(&mut self) -> Result<(), PhpReaderError> {
        match &mut self.program_info.vmt {
            VirtualMethodTable::Unknown => {
                self.program_info.vmt = VirtualMethodTable::new_php();
                self.extend_vmt()
            }
            VirtualMethodTable::Php {
                methods,
                hierarchy,
                aliases,
            } => {
                for method in std::mem::take(&mut self.vmt_methods) {
                    if !methods.contains(&method) {
                        methods.push(method);
                    }
                }

                for (class, parents) in std::mem::take(&mut self.vmt_hierarchy) {
                    let existing = hierarchy.entry(class).or_default();
                    for parent in parents {
                        if !existing.contains(&parent) {
                            existing.push(parent);
                        }
                    }
                }

                for (alias, original) in std::mem::take(&mut self.vmt_aliases) {
                    aliases.entry(alias).or_insert(original);
                }

                Ok(())
            }
            other => Err(PhpReaderError::LoweringFailure {
                message: format!("cannot lower PHP into non-PHP VMT '{}'", other),
            }),
        }
    }

    fn collect_symbols(
        &mut self,
        node: Node<'_>,
        mut current_namespace: String,
        current_class: Option<String>,
    ) -> Result<(), PhpReaderError> {
        if current_namespace.is_empty() && current_class.is_none() {
            self.collect_builtin_functions();
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "namespace_definition" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        current_namespace = self.text(name_node).to_string();
                    } else {
                        current_namespace = String::new();
                    }
                    self.collect_symbols(child, current_namespace.clone(), current_class.clone())?;
                }
                "class_declaration" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    let fqn_class = if current_namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{}\\{}", current_namespace, name)
                    };

                    let mut bases = smallvec::smallvec![];
                    for base_or_iface in child.children(&mut child.walk()) {
                        if base_or_iface.kind() == "base_clause"
                            || base_or_iface.kind() == "class_interface_clause"
                        {
                            for base_child in base_or_iface.children(&mut base_or_iface.walk()) {
                                if base_child.kind() == "name" {
                                    let base_name = self.text(base_child).to_string();
                                    let base_fqn = if current_namespace.is_empty()
                                        || base_name.starts_with('\\')
                                    {
                                        base_name.trim_start_matches('\\').to_string()
                                    } else {
                                        format!("{}\\{}", current_namespace, base_name)
                                    };
                                    bases.push(PhpClass(ArcIntern::from(base_fqn.as_str())));
                                }
                            }
                        }
                    }
                    self.vmt_hierarchy
                        .insert(PhpClass(ArcIntern::from(fqn_class.as_str())), bases);
                    self.collect_symbols(child, current_namespace.clone(), Some(fqn_class))?;
                }
                "method_declaration" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    if let Some(ref cls) = current_class {
                        let fqn_method = format!("{}::{}", cls, name);
                        let php_class = PhpClass(ArcIntern::from(cls.as_str()));
                        let php_method_name =
                            PhpMethodName(ArcIntern::from(name.to_lowercase().as_str()));
                        let php_method = PhpMethod(ArcIntern::from(fqn_method.as_str()));
                        self.vmt_methods
                            .insert((php_class, php_method_name, php_method));
                    }
                    self.collect_symbols(child, current_namespace.clone(), current_class.clone())?;
                }
                "function_definition" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    let fqn_func = if current_namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{}\\{}", current_namespace, name)
                    };

                    // We map free functions to a synthetic global class ""
                    let php_class = PhpClass(ArcIntern::from(""));
                    let php_method_name =
                        PhpMethodName(ArcIntern::from(name.to_lowercase().as_str()));
                    let php_method = PhpMethod(ArcIntern::from(fqn_func.as_str()));
                    self.vmt_methods
                        .insert((php_class, php_method_name, php_method));

                    self.collect_symbols(child, current_namespace.clone(), current_class.clone())?;
                }
                _ => {
                    if child.child_count() > 0 {
                        self.collect_symbols(
                            child,
                            current_namespace.clone(),
                            current_class.clone(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_builtin_functions(&mut self) {
        for name in [
            "echo",
            "exec",
            "mysqli_query",
            "passthru",
            "pg_query",
            "shell_exec",
            "system",
        ] {
            let php_class = PhpClass(ArcIntern::from(""));
            let php_method_name = PhpMethodName(ArcIntern::from(name));
            let php_method = PhpMethod(ArcIntern::from(name));
            self.vmt_methods
                .insert((php_class, php_method_name, php_method));
        }

        let sqlite_query = (
            PhpClass(ArcIntern::from("sqlite3")),
            PhpMethodName(ArcIntern::from("query")),
            PhpMethod(ArcIntern::from("sqlite3::query")),
        );
        self.vmt_methods.insert(sqlite_query);
    }

    fn lower_bodies(&mut self, root: Node<'_>) -> Result<(), PhpReaderError> {
        let mut main_func = FunctionLowerer::new(format!("__php_main__::{}", self.file_name));
        self.lower_block(root, &mut main_func, String::new(), None)?;

        if main_func.func.blocks[main_func.current_block]
            .terminator
            .is_none()
        {
            main_func.builder().create_ret(std::iter::empty());
            self.set_terminator_source_info(&mut main_func, root);
        }
        self.program_info
            .program
            .functions
            .functions
            .push(main_func.func);
        Ok(())
    }

    fn lower_block(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
        mut current_namespace: String,
        current_class: Option<String>,
    ) -> Result<(), PhpReaderError> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if func_ctx.func.blocks[func_ctx.current_block]
                .terminator
                .is_some()
            {
                break;
            }
            match child.kind() {
                "namespace_definition" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        current_namespace = self.text(name_node).to_string();
                    } else {
                        current_namespace = String::new();
                    }
                    self.lower_block(
                        child,
                        func_ctx,
                        current_namespace.clone(),
                        current_class.clone(),
                    )?;
                }
                "class_declaration" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    let fqn_class = if current_namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{}\\{}", current_namespace, name)
                    };
                    self.lower_block(child, func_ctx, current_namespace.clone(), Some(fqn_class))?;
                }
                "function_definition" | "method_declaration" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    let fqn = match child.kind() {
                        "method_declaration" => {
                            format!("{}::{}", current_class.as_ref().unwrap(), name)
                        }
                        _ => {
                            if current_namespace.is_empty() {
                                name.clone()
                            } else {
                                format!("{}\\{}", current_namespace, name)
                            }
                        }
                    };

                    let mut inner_func = FunctionLowerer::new(fqn);

                    if let Some(params_node) = child.child_by_field_name("parameters") {
                        let mut p_cursor = params_node.walk();
                        for p in params_node.children(&mut p_cursor) {
                            if p.kind() == "simple_parameter" {
                                let param_name = p
                                    .child_by_field_name("name")
                                    .map(|n| self.text(n).to_string())
                                    .unwrap_or_default();
                                let param_name = param_name
                                    .strip_prefix('$')
                                    .unwrap_or(&param_name)
                                    .to_string();

                                let param_idx =
                                    mir::ParameterIdx::new(inner_func.func.params.parameters.len());
                                inner_func
                                    .func
                                    .params
                                    .parameters
                                    .push(mir::ParameterType::ByVal);

                                let param_var = mir::VariableRef::new_parameter(param_idx);
                                let local_var = mir::VariableRef::new_local(param_name);
                                let stmt = inner_func.builder().create_assign(
                                    local_var,
                                    vec![mir::Exp::AccessPath(mir::AccessPath::from(param_var))],
                                );
                                self.set_stmt_source_info(&mut inner_func, stmt, p);
                            }
                        }
                    }

                    if let Some(body) = child.child_by_field_name("body") {
                        self.lower_block(
                            body,
                            &mut inner_func,
                            current_namespace.clone(),
                            current_class.clone(),
                        )?;
                    }

                    if inner_func.func.blocks[inner_func.current_block]
                        .terminator
                        .is_none()
                    {
                        inner_func.builder().create_ret(std::iter::empty());
                        self.set_terminator_source_info(&mut inner_func, child);
                    }
                    self.program_info
                        .program
                        .functions
                        .functions
                        .push(inner_func.func);
                }
                "expression_statement" => {
                    if let Some(expr) = child.named_child(0) {
                        self.lower_exp(expr, func_ctx)?;
                    }
                }
                "echo_statement" => {
                    let mut args = vec![];
                    let mut e_cursor = child.walk();
                    for e in child.children(&mut e_cursor) {
                        if e.is_named() {
                            args.push(self.lower_exp(e, func_ctx)?);
                        }
                    }

                    let call_style = CallStyle::PhpCall {
                        receiver: None,
                        declared_class: None,
                        method_name: None,
                        callee: AccessPath::from(VariableRef::new_local("echo".to_string())),
                        kind: PhpCallKind::DirectFunction,
                    };

                    let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                    func_ctx.next_temp_idx += 1;
                    let stmt = func_ctx
                        .builder()
                        .create_call(call_style, vec![ret_var], args);
                    self.set_stmt_source_info(func_ctx, stmt, child);
                }
                "return_statement" => {
                    let mut rets = vec![];
                    if let Some(expr) = child.named_child(0) {
                        rets.push(self.lower_exp(expr, func_ctx)?);
                    }
                    if func_ctx.func.blocks[func_ctx.current_block]
                        .terminator
                        .is_none()
                    {
                        func_ctx.builder().create_ret(rets);
                        self.set_terminator_source_info(func_ctx, child);
                    }
                }
                "if_statement" => {
                    self.lower_if_like(
                        child,
                        func_ctx,
                        current_namespace.clone(),
                        current_class.clone(),
                        None,
                    )?;
                }
                "while_statement" => {
                    let cond_block = func_ctx.new_block();
                    let body_block = func_ctx.new_block();
                    let end_block = func_ctx.new_block();

                    func_ctx.finish_block_with_goto(cond_block);

                    func_ctx.current_block = cond_block;
                    if let Some(cond) = child.child_by_field_name("condition") {
                        self.lower_exp(cond, func_ctx)?;
                    }
                    func_ctx.builder().create_goto(vec![body_block, end_block]);
                    self.set_terminator_source_info(func_ctx, child);

                    func_ctx.current_block = body_block;
                    if let Some(b) = child.child_by_field_name("body") {
                        self.lower_block(
                            b,
                            func_ctx,
                            current_namespace.clone(),
                            current_class.clone(),
                        )?;
                    }
                    func_ctx.finish_block_with_goto(cond_block);

                    func_ctx.current_block = end_block;
                }
                _ => {
                    if child.child_count() > 0
                        && !matches!(
                            child.kind(),
                            "assignment_expression"
                                | "binary_expression"
                                | "unary_op_expression"
                                | "subscript_expression"
                                | "member_access"
                                | "property_access"
                                | "function_call_expression"
                                | "method_call_expression"
                                | "object_creation_expression"
                        )
                    {
                        self.lower_block(
                            child,
                            func_ctx,
                            current_namespace.clone(),
                            current_class.clone(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn lower_if_like(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
        current_namespace: String,
        current_class: Option<String>,
        end_block_override: Option<mir::BasicBlockIdx>,
    ) -> Result<bool, PhpReaderError> {
        let mut cond_expr = None;
        let mut body = None;
        let mut else_clause = None;

        let mut c_cursor = node.walk();
        for c in node.children(&mut c_cursor) {
            if c.kind() == "parenthesized_expression" {
                cond_expr = Some(c);
            } else if c.kind() == "compound_statement" && body.is_none() {
                body = Some(c);
            } else if c.kind() == "else_clause" || c.kind() == "else_if_clause" {
                else_clause = Some(c);
            }
        }

        if let Some(cond) = cond_expr {
            self.lower_exp(cond, func_ctx)?;
        }

        let then_block = func_ctx.new_block();
        if else_clause.is_none() {
            let end_block = end_block_override.unwrap_or_else(|| func_ctx.new_block());
            func_ctx.builder().create_goto(vec![then_block, end_block]);
            self.set_terminator_source_info(func_ctx, node);

            func_ctx.current_block = then_block;
            if let Some(b) = body {
                self.lower_block(
                    b,
                    func_ctx,
                    current_namespace.clone(),
                    current_class.clone(),
                )?;
            }
            if func_ctx.func.blocks[func_ctx.current_block]
                .terminator
                .is_none()
            {
                func_ctx.finish_block_with_goto(end_block);
            }

            func_ctx.current_block = end_block;
            return Ok(true);
        }

        let else_block = func_ctx.new_block();
        func_ctx.builder().create_goto(vec![then_block, else_block]);
        self.set_terminator_source_info(func_ctx, node);

        func_ctx.current_block = then_block;
        if let Some(b) = body {
            self.lower_block(
                b,
                func_ctx,
                current_namespace.clone(),
                current_class.clone(),
            )?;
        }
        let then_exit_block = func_ctx.current_block;
        let then_reaches_end = func_ctx.func.blocks[then_exit_block].terminator.is_none();

        let ec = else_clause.expect("checked above");
        func_ctx.current_block = else_block;
        let else_reaches_end = if ec.kind() == "else_if_clause" {
            self.lower_if_like(
                ec,
                func_ctx,
                current_namespace.clone(),
                current_class.clone(),
                end_block_override,
            )?
        } else {
            self.lower_block(
                ec,
                func_ctx,
                current_namespace.clone(),
                current_class.clone(),
            )?;
            func_ctx.func.blocks[func_ctx.current_block]
                .terminator
                .is_none()
        };
        let else_exit_block = func_ctx.current_block;

        let end_reachable = then_reaches_end || else_reaches_end;
        if !end_reachable {
            return Ok(false);
        }

        let end_block = end_block_override.unwrap_or_else(|| func_ctx.new_block());
        if then_reaches_end {
            func_ctx.current_block = then_exit_block;
            func_ctx.finish_block_with_goto(end_block);
        }
        if else_reaches_end && func_ctx.func.blocks[else_exit_block].terminator.is_none() {
            func_ctx.current_block = else_exit_block;
            func_ctx.finish_block_with_goto(end_block);
        }

        func_ctx.current_block = end_block;
        Ok(true)
    }

    fn lower_exp(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
    ) -> Result<Exp, PhpReaderError> {
        match node.kind() {
            "variable_name" => {
                let text = self.text(node);
                let name = text.strip_prefix('$').unwrap_or(text);
                match name {
                    "GLOBALS" | "_SERVER" | "_GET" | "_POST" | "_FILES" | "_COOKIE"
                    | "_SESSION" | "_REQUEST" | "_ENV" => {
                        let mut ap = AccessPath::from(VariableRef::new_global());
                        ap.path
                            .fields
                            .push(mir::FieldAccess::Symbol(ArcIntern::from(name)));
                        Ok(Exp::AccessPath(ap))
                    }
                    _ => Ok(Exp::AccessPath(AccessPath::from(VariableRef::new_local(
                        name.to_string(),
                    )))),
                }
            }
            "string" => {
                let val = self
                    .evaluator
                    .eval_node(node, self.source)
                    .unwrap_or_else(|| self.text(node).to_string());
                Ok(func_ctx.builder().new_str_exp(&val))
            }
            "integer" => Ok(func_ctx.builder().new_str_exp(self.text(node))),
            "assignment_expression" => {
                let left = node.child_by_field_name("left").unwrap();
                let right = node.child_by_field_name("right").unwrap();

                let rhs = self.lower_exp(right, func_ctx)?;
                let lhs = self.lower_exp(left, func_ctx)?;

                if left.kind() == "variable_name"
                    && let Some(value) = self.evaluator.eval_node(right, self.source)
                {
                    self.evaluator.assign(self.text(left).to_string(), value);
                }

                if let Exp::AccessPath(ap) = lhs {
                    let stmt = func_ctx.builder().create_assign_or_update(ap, rhs.clone());
                    self.set_stmt_source_info(func_ctx, stmt, node);
                }
                Ok(rhs)
            }
            "member_access_expression"
            | "nullsafe_member_access_expression"
            | "scoped_property_access_expression" => {
                let obj_node = node
                    .child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("scope"))
                    .unwrap();
                let obj = self.lower_exp(obj_node, func_ctx)?;
                let prop = node.child_by_field_name("name").unwrap();
                let prop_name = self.text(prop);

                let mut ap = if let Exp::AccessPath(ap) = obj {
                    ap
                } else {
                    let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                    func_ctx.next_temp_idx += 1;
                    let stmt = func_ctx.builder().create_assign(ret_var.clone(), vec![obj]);
                    self.set_stmt_source_info(func_ctx, stmt, obj_node);
                    AccessPath::from(ret_var)
                };

                ap.path
                    .fields
                    .push(mir::FieldAccess::Symbol(ArcIntern::from(prop_name)));
                Ok(Exp::AccessPath(ap))
            }
            "subscript_expression" => {
                let obj_node = node
                    .child_by_field_name("object")
                    .or_else(|| node.child(0))
                    .unwrap();
                let array = self.lower_exp(obj_node, func_ctx)?;
                let mut ap = if let Exp::AccessPath(ap) = array {
                    ap
                } else {
                    let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                    func_ctx.next_temp_idx += 1;
                    let stmt = func_ctx
                        .builder()
                        .create_assign(ret_var.clone(), vec![array]);
                    self.set_stmt_source_info(func_ctx, stmt, obj_node);
                    AccessPath::from(ret_var)
                };

                let index_node = node
                    .child_by_field_name("index")
                    .or_else(|| node.named_child(1));
                if let Some(index_node) = index_node {
                    if let Some(value) = self.evaluator.eval_node(index_node, self.source) {
                        ap.path
                            .fields
                            .push(mir::FieldAccess::Symbol(ArcIntern::from(value.as_str())));
                    } else if let Ok(offset) = self.text(index_node).parse::<i64>() {
                        ap.path
                            .fields
                            .push(mir::FieldAccess::Offset(mir::Offset(offset)));
                    } else {
                        ap.path
                            .fields
                            .push(mir::FieldAccess::Offset(mir::Offset(0)));
                    }
                } else {
                    ap.path
                        .fields
                        .push(mir::FieldAccess::Offset(mir::Offset(0)));
                }
                Ok(Exp::AccessPath(ap))
            }
            "function_call_expression"
            | "member_call_expression"
            | "method_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression"
            | "object_creation_expression" => {
                let function_node = node.child_by_field_name("function");
                let name_node = node.child_by_field_name("name");
                let args_node = node.child_by_field_name("arguments");

                let mut args = vec![];
                if let Some(args_list) = args_node {
                    let mut a_cursor = args_list.walk();
                    for a in args_list.children(&mut a_cursor) {
                        if a.kind() == "argument" {
                            args.push(self.lower_exp(a.named_child(0).unwrap_or(a), func_ctx)?);
                        }
                    }
                }

                let call_style = if node.kind() == "function_call_expression"
                    || node.kind() == "object_creation_expression"
                {
                    let fn_name_node = if node.kind() == "object_creation_expression" {
                        node.child_by_field_name("class")
                            .or_else(|| node.named_child(0))
                    } else {
                        function_node
                    };
                    let fn_name = if let Some(n) = fn_name_node {
                        self.text(n)
                    } else {
                        "unknown_func"
                    };
                    CallStyle::PhpCall {
                        receiver: None,
                        declared_class: None,
                        method_name: None,
                        callee: AccessPath::from(VariableRef::new_local(fn_name.to_string())),
                        kind: PhpCallKind::DirectFunction,
                    }
                } else {
                    let obj_node = node
                        .child_by_field_name("object")
                        .or_else(|| node.child_by_field_name("scope"))
                        .unwrap();
                    let obj = self.lower_exp(obj_node, func_ctx)?;
                    let meth_name = if let Some(n) = name_node {
                        self.text(n)
                    } else {
                        "unknown_method"
                    };
                    CallStyle::PhpCall {
                        receiver: match &obj {
                            Exp::AccessPath(ap) => Some(ap.variable_ref.clone()),
                            _ => Some(VariableRef::new_local("unknown_obj".to_string())),
                        },
                        declared_class: None,
                        method_name: Some(mir::call::PhpMethodName(ArcIntern::from(meth_name))),
                        callee: match obj {
                            Exp::AccessPath(ap) => ap,
                            _ => {
                                AccessPath::from(VariableRef::new_local("unknown_obj".to_string()))
                            }
                        },
                        kind: PhpCallKind::InstanceMethod,
                    }
                };

                let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                func_ctx.next_temp_idx += 1;

                let stmt = func_ctx
                    .builder()
                    .create_call(call_style, vec![ret_var.clone()], args);
                self.set_stmt_source_info(func_ctx, stmt, node);
                Ok(Exp::AccessPath(AccessPath::from(ret_var)))
            }
            "binary_expression" => {
                let left = self.lower_exp(node.child_by_field_name("left").unwrap(), func_ctx)?;
                let right = self.lower_exp(node.child_by_field_name("right").unwrap(), func_ctx)?;
                let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                func_ctx.next_temp_idx += 1;
                let stmt = func_ctx
                    .builder()
                    .create_assign(ret_var.clone(), vec![left, right]);
                self.set_stmt_source_info(func_ctx, stmt, node);
                Ok(Exp::AccessPath(AccessPath::from(ret_var)))
            }
            "unary_op_expression" => {
                let argument = node
                    .child_by_field_name("argument")
                    .or_else(|| node.named_child(0))
                    .unwrap();
                let arg_exp = self.lower_exp(argument, func_ctx)?;
                let ret_var = VariableRef::new_local(format!("_t{}", func_ctx.next_temp_idx));
                func_ctx.next_temp_idx += 1;
                let stmt = func_ctx
                    .builder()
                    .create_assign(ret_var.clone(), vec![arg_exp]);
                self.set_stmt_source_info(func_ctx, stmt, node);
                Ok(Exp::AccessPath(AccessPath::from(ret_var)))
            }
            "parenthesized_expression" => {
                if let Some(child) = node.named_child(0) {
                    self.lower_exp(child, func_ctx)
                } else {
                    Ok(func_ctx.builder().new_str_exp(self.text(node)))
                }
            }
            _ => {
                if node.child_count() > 0 {
                    for i in 0..node.child_count() {
                        if let Some(c) = node.child(i as u32)
                            && c.is_named()
                        {
                            return self.lower_exp(c, func_ctx);
                        }
                    }
                    Ok(func_ctx.builder().new_str_exp(self.text(node)))
                } else {
                    Ok(func_ctx.builder().new_str_exp(self.text(node)))
                }
            }
        }
    }

    // Helper to get node text
    fn text(&self, node: Node<'_>) -> &'a str {
        &self.source[node.start_byte()..node.end_byte()]
    }

    fn set_stmt_source_info(
        &mut self,
        func_ctx: &mut FunctionLowerer,
        stmt_idx: mir::StatementIdx,
        node: Node<'_>,
    ) {
        let span_id = self.file_span_for(node);
        func_ctx.func.blocks[func_ctx.current_block].statements[stmt_idx].source_info =
            mir::SourceInfo::new(span_id);
    }

    fn set_terminator_source_info(&mut self, func_ctx: &mut FunctionLowerer, node: Node<'_>) {
        let span_id = self.file_span_for(node);
        if let Some(terminator) = func_ctx.func.blocks[func_ctx.current_block]
            .terminator
            .as_mut()
        {
            terminator.source_info = mir::SourceInfo::new(span_id);
        }
    }

    fn file_span_for(&mut self, node: Node<'_>) -> FileSpanId {
        let start = node.start_byte() as u32;
        let len = (node.end_byte().saturating_sub(node.start_byte())) as u32;
        if let Some(file_span_id) = self.span_cache.get(&(start, len)) {
            return *file_span_id;
        }

        let file_id = self.ensure_file_id();
        let span_id = SpanId(self.program_info.source_info.spans.len() as u32 + 1);
        self.program_info.source_info.spans.push(Span {
            start,
            len: SpanLen::ByteLen(len),
        });

        let file_span_id = FileSpanId(self.program_info.source_info.file_spans.len() as u32 + 1);
        self.program_info.source_info.file_spans.push(FileSpan {
            file: file_id,
            span: span_id,
        });
        self.span_cache.insert((start, len), file_span_id);
        file_span_id
    }

    fn ensure_file_id(&mut self) -> FileId {
        if let Some(file_id) = self.current_file_id {
            return file_id;
        }

        if self.program_info.source_info.metadata.version == 0 {
            self.program_info.source_info.metadata = ArtifactMetadata::new();
        }

        let artifact_id = self
            .program_info
            .source_info
            .artifacts
            .iter()
            .position(|artifact| artifact.canonical_path == self.source_path)
            .map(|idx| ArtifactId((idx + 1) as u32))
            .unwrap_or_else(|| {
                let hash_len = usize::from(self.program_info.source_info.metadata.hash_len.max(32));
                self.program_info
                    .source_info
                    .artifacts
                    .push(ArtifactRecord {
                        canonical_path: self.source_path.to_string(),
                        sub_artifact_id: 0,
                        encoding: ArtifactEncoding::Utf8,
                        content_hash: vec![0; hash_len],
                    });
                ArtifactId(self.program_info.source_info.artifacts.len() as u32)
            });

        let file_id = self
            .program_info
            .source_info
            .files
            .iter()
            .position(|file| file.artifact == artifact_id)
            .map(|idx| FileId((idx + 1) as u32))
            .unwrap_or_else(|| {
                self.program_info.source_info.files.push(FileEntry {
                    artifact: artifact_id,
                });
                FileId(self.program_info.source_info.files.len() as u32)
            });
        self.current_file_id = Some(file_id);
        file_id
    }
}

struct FunctionLowerer {
    func: FunctionData,
    current_block: mir::BasicBlockIdx,
    next_temp_idx: u32,
}

impl FunctionLowerer {
    fn new(name: String) -> Self {
        let mut func = FunctionData {
            name,
            params: Params::default(),
            return_type: ReturnType { arity: 1 },
            blocks: BasicBlocks::new(),
        };
        let current_block = func.blocks.new_block();
        Self {
            func,
            current_block,
            next_temp_idx: 0,
        }
    }

    fn builder(&mut self) -> BasicBlockBuilder<'_> {
        let block_data = &mut self.func.blocks.blocks_mut()[self.current_block];
        BasicBlockBuilder::new(block_data)
    }
    fn new_block(&mut self) -> mir::BasicBlockIdx {
        self.func.blocks.new_block()
    }

    fn finish_block_with_goto(&mut self, target: mir::BasicBlockIdx) {
        if self.func.blocks[self.current_block].terminator.is_none() {
            self.builder().create_goto(vec![target]);
        }
    }
}
