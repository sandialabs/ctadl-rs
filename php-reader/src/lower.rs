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

/// The symbolic field every array element shares when its key is not a constant string.
///
/// `$a[$i]`, `$a[0]`, `$a[]` (append) and `foreach ($a as &$e)` all name an element this lowering
/// cannot pin down to a distinct string key. They fold to one field, so a write through any of
/// them is visible to a read through any other -- including a read at a literal index, which lands
/// here too. This mirrors the single array-element field the JVM frontend uses (`[]`).
///
/// The new IR ([`ctadl_ir::mir::StatementKind::Store`]) requires a *symbolic* field for every
/// memory write, so array elements are modeled as this symbolic field rather than as a numeric
/// offset (which is pointer arithmetic that carries no memory taint).
const ARRAY_ELEMENT_FIELD: &str = "[]";

/// A PHP place: a base variable plus a not-yet-materialized sequence of symbolic field/element
/// accesses (property names, array keys, global slots).
///
/// Unlike the old model, where an [`AccessPath`] could carry symbolic fields directly, the new IR
/// keeps symbolic access out of access paths: a *read* of `a.f.g` becomes a chain of
/// [`ctadl_ir::mir::StatementKind::Load`]s and a *write* becomes loads for the interior fields plus
/// a final [`ctadl_ir::mir::StatementKind::Store`]. A `Place` defers that lowering so the same
/// expression can be materialized as a read ([`Lowerer::read_place`]) or a write target
/// ([`Lowerer::write_place`]) depending on where it appears.
#[derive(Clone)]
struct Place {
    base: VariableRef,
    segments: Vec<mir::PathSegment>,
}

impl Place {
    fn variable(base: VariableRef) -> Self {
        Place {
            base,
            segments: Vec::new(),
        }
    }

    fn global() -> Self {
        Place::variable(VariableRef::new_global())
    }
}

/// Whether `kind` is a tree-sitter node that denotes an assignable place (an lvalue): a bare
/// variable, a property access, or a subscript. Only these are lowered with [`Lowerer::lower_place`]
/// on the left of an assignment; anything else is evaluated for its value alone.
fn is_place_kind(kind: &str) -> bool {
    matches!(
        kind,
        "variable_name"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
            | "scoped_property_access_expression"
            | "subscript_expression"
    )
}

/// Whether `name` (without its `$`) is a PHP superglobal: a variable that names global state
/// directly in every scope, with no declaration and no capturing.
fn is_superglobal(name: &str) -> bool {
    matches!(
        name,
        "GLOBALS"
            | "_SERVER"
            | "_GET"
            | "_POST"
            | "_FILES"
            | "_COOKIE"
            | "_SESSION"
            | "_REQUEST"
            | "_ENV"
    )
}

/// Node kinds that declare a named type whose body can hold `method_declaration`s.
///
/// All four are one thing to the lowering: a name to qualify their methods with and a body to walk.
/// Only classes can be instantiated, but PHP resolves a call by method name across every type that
/// declares it, so an interface's or trait's methods have to be registered like any other.
fn is_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration"
    )
}

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
    /// Free functions called by name, mapped to the largest argument count seen at any call site.
    /// Any of these still undefined after lowering is stubbed by [`Lowerer::stub_called_functions`].
    called_functions: BTreeMap<String, usize>,
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
            called_functions: BTreeMap::new(),
        }
    }

    pub fn lower(&mut self, tree: &Tree) -> Result<(), PhpReaderError> {
        let root = tree.root_node();

        // Pass 1: Collect symbols
        self.collect_symbols(root, String::new(), None)?;

        // Pass 2: Lower bodies
        self.lower_bodies(root)?;

        // Pass 3: Stub whatever pass 2 called but no pass ever defined
        self.stub_called_functions();

        self.extend_vmt()?;
        Ok(())
    }

    /// Give every called-but-undefined free function an empty definition.
    ///
    /// PHP programs are open: they call builtins (`echo`, `exec`, `sprintf`) and functions from
    /// files we were not handed. The analysis needs each of those to exist as a function with
    /// declared formals regardless, for two reasons. Taint only crosses a call boundary into a
    /// formal the callee declares, so a sink model on `exec`'s `Argument(0)` matches nothing if
    /// `exec` declares no parameters. And a body-less function is what marks a function
    /// *external*, which is what lets a model describe its behavior instead of the (absent) code.
    ///
    /// Arity comes from the widest call site rather than any real signature, which is all the
    /// analysis needs: a formal exists for every argument anyone actually passes. This mirrors how
    /// the dex frontend stubs its unresolved callees.
    fn stub_called_functions(&mut self) {
        let defined: BTreeSet<&str> = self
            .program_info
            .program
            .functions
            .functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        let stubs: Vec<(String, usize)> = self
            .called_functions
            .iter()
            .filter(|(name, _)| !defined.contains(name.as_str()))
            .map(|(name, argc)| (name.clone(), *argc))
            .collect();

        for (name, argc) in stubs {
            log::debug!("stubbing external php function {name}/{argc}");
            let mut params = Params::default();
            for _ in 0..argc {
                params.parameters.push(mir::ParameterType::ByVal);
            }
            self.program_info
                .program
                .functions
                .functions
                .push(FunctionData {
                    name: name.clone(),
                    params,
                    return_type: ReturnType { arity: 1 },
                    // No blocks at all: this is what codegen reads as "external function".
                    blocks: BasicBlocks::new(),
                });
            self.vmt_methods.insert((
                PhpClass(ArcIntern::from("")),
                PhpMethodName(ArcIntern::from(name.to_lowercase().as_str())),
                PhpMethod(ArcIntern::from(name.as_str())),
            ));
        }
    }

    /// Record a call to a free function, widening its recorded arity to `argc`.
    fn record_called_function(&mut self, name: &str, argc: usize) {
        let entry = self.called_functions.entry(name.to_string()).or_default();
        *entry = (*entry).max(argc);
    }

    /// Names of the traits a type declaration pulls in via `use`, fully qualified.
    ///
    /// The trait names sit in `use_declaration` nodes inside the body, which is where this looks;
    /// the `use` *statement* that imports a namespace is a `namespace_use_declaration` and is a
    /// different node, so there is no confusion between the two senses of the keyword.
    fn used_traits(&self, decl: Node<'_>, current_namespace: &str) -> Vec<String> {
        let Some(body) = decl.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut traits = Vec::new();
        for member in body.children(&mut body.walk()) {
            if member.kind() != "use_declaration" {
                continue;
            }
            for name_node in member.children(&mut member.walk()) {
                if !matches!(name_node.kind(), "name" | "qualified_name") {
                    continue;
                }
                let trait_name = self.text(name_node);
                traits.push(
                    if current_namespace.is_empty() || trait_name.starts_with('\\') {
                        trait_name.trim_start_matches('\\').to_string()
                    } else {
                        format!("{}\\{}", current_namespace, trait_name)
                    },
                );
            }
        }
        traits
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
                kind if is_type_declaration(kind) => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| self.text(n).to_string())
                        .unwrap_or_default();
                    let fqn_class = if current_namespace.is_empty() {
                        name.clone()
                    } else {
                        format!("{}\\{}", current_namespace, name)
                    };

                    // Every supertype is a parent, whichever clause names it: `extends` (a class
                    // base, or the interfaces an interface extends), `implements`, and `use` (a
                    // trait, whose methods PHP flattens into the using type as if declared there).
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
                    bases.extend(
                        self.used_traits(child, &current_namespace)
                            .into_iter()
                            .map(|t| PhpClass(ArcIntern::from(t.as_str()))),
                    );
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
        main_func.is_main = true;
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
                kind if is_type_declaration(kind) => {
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
                    let fqn = match (child.kind(), current_class.as_ref()) {
                        // A method outside any type declaration is not something the grammar can
                        // produce; name it like a free function rather than panicking if it does.
                        ("method_declaration", Some(cls)) => format!("{}::{}", cls, name),
                        _ => {
                            if current_namespace.is_empty() {
                                name.clone()
                            } else {
                                format!("{}\\{}", current_namespace, name)
                            }
                        }
                    };

                    let mut inner_func = FunctionLowerer::new(fqn);

                    let by_ref = self.lower_params(child, &mut inner_func);

                    if let Some(body) = child.child_by_field_name("body") {
                        self.lower_block(
                            body,
                            &mut inner_func,
                            current_namespace.clone(),
                            current_class.clone(),
                        )?;
                    }

                    self.write_back_by_ref_params(&by_ref, &mut inner_func, child);

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
                    self.record_called_function("echo", args.len());

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
                "foreach_statement" => {
                    self.lower_foreach(
                        child,
                        func_ctx,
                        current_namespace.clone(),
                        current_class.clone(),
                    )?;
                }
                // `global $x, $y;` rebinds those names to the global slots of the same name for
                // the rest of the function -- reads and writes both.
                "global_declaration" => {
                    let mut g_cursor = child.walk();
                    for v in child.named_children(&mut g_cursor) {
                        if v.kind() != "variable_name" {
                            continue;
                        }
                        let text = self.text(v);
                        let name = text.strip_prefix('$').unwrap_or(text).to_string();
                        func_ctx.global_aliases.insert(name.clone(), name);
                    }
                }
                // A function-local `static` keeps its value across calls, so it is state that
                // outlives the frame -- a global slot in all but name. The slot is qualified by
                // the function so two functions' statics cannot collide.
                "function_static_declaration" => {
                    let mut s_cursor = child.walk();
                    for decl in child.named_children(&mut s_cursor) {
                        if decl.kind() != "static_variable_declaration" {
                            continue;
                        }
                        let Some(name_node) = decl.child_by_field_name("name") else {
                            continue;
                        };
                        let text = self.text(name_node);
                        let name = text.strip_prefix('$').unwrap_or(text).to_string();
                        let slot = format!("{}::{}", func_ctx.func.name, name);
                        func_ctx.global_aliases.insert(name, slot.clone());

                        if let Some(value_node) = decl.child_by_field_name("value") {
                            let value = self.lower_exp(value_node, func_ctx)?;
                            let place = Place {
                                base: VariableRef::new_global(),
                                segments: vec![mir::PathSegment::symbol(&slot)],
                            };
                            self.write_place(place, value, func_ctx, decl);
                        }
                    }
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

    /// Lower a closure (`function () use (..) {..}` or `fn () => ..`) into its own function, and
    /// evaluate to a pointer to it.
    ///
    /// The pointer is what lets the closure be called later: assigning it to a variable records
    /// which function that variable holds, so a call through the variable resolves back to this
    /// body -- see the dynamic-call arm of [`Lowerer::lower_exp`].
    ///
    /// Captured variables cross from the enclosing frame into a frame that does not exist yet, so
    /// they travel through the one piece of state both frames can name: the global heap. Each
    /// capture gets a slot private to this closure, written at the point of creation and read back
    /// inside the body (via the same aliasing that serves `global` declarations). A by-reference
    /// capture (`use (&$x)`) additionally reads the slot back out into the enclosing variable,
    /// since a write inside the closure is meant to be visible outside it.
    fn lower_closure(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
    ) -> Result<Exp, PhpReaderError> {
        // Byte offset keeps the name unique and stable: no two closures start at the same place.
        let closure_name = format!("{{closure}}@{}:{}", self.file_name, node.start_byte());
        let mut closure = FunctionLowerer::new(closure_name.clone());

        let by_ref = self.lower_params(node, &mut closure);
        let param_names: BTreeSet<String> = closure
            .func
            .params
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(i, _)| self.param_local_name(node, i))
            .collect();

        // `use (..)` lists an anonymous function's captures explicitly; an arrow function has no
        // list and captures by value whatever its body reads from the enclosing scope.
        let captures: Vec<(String, bool)> = match node.kind() {
            "arrow_function" => node
                .child_by_field_name("body")
                .map(|body| {
                    self.free_variables(body, &param_names)
                        .into_iter()
                        .map(|name| (name, false))
                        .collect()
                })
                .unwrap_or_default(),
            _ => self.use_clause_captures(node),
        };

        for (name, _) in &captures {
            closure
                .global_aliases
                .insert(name.clone(), format!("{closure_name}::{name}"));
        }

        match node.kind() {
            "arrow_function" => {
                // An arrow function's body is one expression, and its value is what it returns.
                if let Some(body) = node.child_by_field_name("body") {
                    let value = self.lower_exp(body, &mut closure)?;
                    closure.builder().create_ret(vec![value]);
                    self.set_terminator_source_info(&mut closure, body);
                }
            }
            _ => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.lower_block(body, &mut closure, String::new(), None)?;
                }
            }
        }

        self.write_back_by_ref_params(&by_ref, &mut closure, node);
        if closure.func.blocks[closure.current_block]
            .terminator
            .is_none()
        {
            closure.builder().create_ret(std::iter::empty());
            self.set_terminator_source_info(&mut closure, node);
        }
        self.program_info
            .program
            .functions
            .functions
            .push(closure.func);

        // Back in the enclosing function: fill each capture slot from the variable it captures.
        for (name, is_by_ref) in &captures {
            let slot = format!("{closure_name}::{name}");
            let slot_place = || Place {
                base: VariableRef::new_global(),
                segments: vec![mir::PathSegment::symbol(&slot)],
            };

            let source = self.place_for_variable(name, func_ctx);
            let outer = self.read_place(source, func_ctx, node);
            self.write_place(slot_place(), outer, func_ctx, node);

            if *is_by_ref {
                // The closure has not run yet, so this edge is placed early on purpose: it stands
                // for "whenever the closure runs, this write is visible here".
                let slot_value = self.read_place(slot_place(), func_ctx, node);
                let outer_place = self.place_for_variable(name, func_ctx);
                self.write_place(outer_place, slot_value, func_ctx, node);
            }
        }

        Ok(Exp::ObjectRef(mir::CallObject::FunctionPtr(
            ArcIntern::from(closure_name.as_str()),
        )))
    }

    /// The local name bound to parameter `index` of `decl`, if it declares one.
    fn param_local_name(&self, decl: Node<'_>, index: usize) -> Option<String> {
        let params_node = decl.child_by_field_name("parameters")?;
        let p = params_node
            .children(&mut params_node.walk())
            .filter(|p| {
                matches!(
                    p.kind(),
                    "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
                )
            })
            .nth(index)?;
        let name_node = p.child_by_field_name("name")?;
        let text = self.text(name_node);
        Some(text.strip_prefix('$').unwrap_or(text).to_string())
    }

    /// The `use (..)` captures of an anonymous function, as `(name, by_reference)`.
    fn use_clause_captures(&self, node: Node<'_>) -> Vec<(String, bool)> {
        let mut captures = Vec::new();
        for c in node.children(&mut node.walk()) {
            if c.kind() != "anonymous_function_use_clause" {
                continue;
            }
            for v in c.named_children(&mut c.walk()) {
                let (var_node, is_by_ref) = match v.kind() {
                    "by_ref" => (v.named_child(0), true),
                    "variable_name" => (Some(v), false),
                    _ => continue,
                };
                let Some(var_node) = var_node else { continue };
                let text = self.text(var_node);
                captures.push((
                    text.strip_prefix('$').unwrap_or(text).to_string(),
                    is_by_ref,
                ));
            }
        }
        captures
    }

    /// Variable names read anywhere under `node` that `bound` does not account for.
    ///
    /// This is what an arrow function captures: everything its body mentions except its own
    /// parameters, `$this`, and the superglobals (which name the global heap directly and need no
    /// capturing).
    fn free_variables(&self, node: Node<'_>, bound: &BTreeSet<String>) -> BTreeSet<String> {
        let mut free = BTreeSet::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "variable_name" {
                let text = self.text(n);
                let name = text.strip_prefix('$').unwrap_or(text);
                if !bound.contains(name) && !is_superglobal(name) && name != "this" {
                    free.insert(name.to_string());
                }
            }
            stack.extend(n.named_children(&mut n.walk()));
        }
        free
    }

    /// Publish a file-scope assignment to the global heap, in addition to the local it wrote.
    ///
    /// A variable assigned at file scope *is* a global in PHP: another function can reach it with
    /// `global $x` or `$GLOBALS['x']`, both of which lower to the global slot `x`. Mirroring the
    /// assignment there is what makes those two views see what file scope wrote.
    ///
    /// The local write stays the primary one, and file scope keeps reading the local. That is the
    /// deliberate half of this: a local is strongly updated, so a later `$x = 'constant'` kills the
    /// taint an earlier `$x = $_GET[..]` put there, whereas heap slots accumulate. Keeping file
    /// scope on locals keeps that kill; the mirror only adds the cross-function view.
    fn mirror_main_scope_global(
        &mut self,
        left: Node<'_>,
        rhs: &Exp,
        func_ctx: &mut FunctionLowerer,
        node: Node<'_>,
    ) {
        if !func_ctx.is_main || left.kind() != "variable_name" {
            return;
        }
        let text = self.text(left);
        let name = text.strip_prefix('$').unwrap_or(text);
        // An aliased name already wrote the global slot directly.
        if func_ctx.global_aliases.contains_key(name) {
            return;
        }
        let place = Place {
            base: VariableRef::new_global(),
            segments: vec![mir::PathSegment::symbol(name)],
        };
        self.write_place(place, rhs.clone(), func_ctx, node);
    }

    /// Bind each declared parameter to a local of the same name, and declare its passing mode.
    ///
    /// Returns the by-reference parameters as `(index, local name)`, which the caller must feed to
    /// [`Lowerer::write_back_by_ref_params`] once the body is lowered.
    ///
    /// A variadic parameter (`...$args`) collects the arguments from its position onward into an
    /// array. It is declared as one parameter here, which is enough for the array's *elements* to
    /// carry taint: an argument passed at a later position has no formal of its own to arrive at,
    /// but the analysis widens a function's arity to the widest call site, so a model over
    /// `Argument(*)` still ranges over what callers actually pass.
    fn lower_params(
        &mut self,
        decl: Node<'_>,
        func_ctx: &mut FunctionLowerer,
    ) -> Vec<(usize, String)> {
        let Some(params_node) = decl.child_by_field_name("parameters") else {
            return Vec::new();
        };
        let mut by_ref = Vec::new();
        let mut p_cursor = params_node.walk();
        for p in params_node.children(&mut p_cursor) {
            if !matches!(
                p.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            ) {
                continue;
            }
            let param_name = p
                .child_by_field_name("name")
                .map(|n| self.text(n).to_string())
                .unwrap_or_default();
            let param_name = param_name
                .strip_prefix('$')
                .unwrap_or(&param_name)
                .to_string();

            // `&$x` is what makes a parameter an out-parameter: the analysis flows taint on a
            // by-ref formal back out to every caller's argument.
            let is_by_ref = p
                .children(&mut p.walk())
                .any(|c| c.kind() == "reference_modifier");

            let index = func_ctx.func.params.parameters.len();
            let param_idx = mir::ParameterIdx::new(index);
            func_ctx.func.params.parameters.push(if is_by_ref {
                mir::ParameterType::ByRef
            } else {
                mir::ParameterType::ByVal
            });

            let param_var = mir::VariableRef::new_parameter(param_idx);
            let local_var = mir::VariableRef::new_local(param_name.clone());
            let stmt = func_ctx.builder().create_assign(
                local_var,
                vec![mir::Exp::AccessPath(mir::AccessPath::from(param_var))],
            );
            self.set_stmt_source_info(func_ctx, stmt, p);

            if is_by_ref {
                by_ref.push((index, param_name));
            }
        }
        by_ref
    }

    /// Copy each by-reference parameter's local back onto the formal it came from.
    ///
    /// Assigning to `&$out` inside a function is visible to the caller, but the body assigns the
    /// *local* the parameter was bound to, which by itself goes nowhere. Copying the local back
    /// onto the formal is what connects the two: taint that reaches the formal flows out to the
    /// argument at every call site.
    fn write_back_by_ref_params(
        &mut self,
        by_ref: &[(usize, String)],
        func_ctx: &mut FunctionLowerer,
        node: Node<'_>,
    ) {
        for (index, name) in by_ref {
            let param_var = mir::VariableRef::new_parameter(mir::ParameterIdx::new(*index));
            let local_var = mir::VariableRef::new_local(name.clone());
            let stmt = func_ctx.builder().create_assign(
                param_var,
                vec![mir::Exp::AccessPath(mir::AccessPath::from(local_var))],
            );
            self.set_stmt_source_info(func_ctx, stmt, node);
        }
    }

    /// Lower `foreach (COLL as [K =>] V) BODY`.
    ///
    /// The loop variable takes one element per iteration, but which element -- which field of the
    /// collection -- is not known statically, so `V` is modeled as a copy of the whole collection.
    /// That is the sound direction: it keeps every field of `COLL` reachable through `V`, so taint
    /// anywhere in the collection stays visible at the same field path on the loop variable.
    ///
    /// `foreach` by reference (`as &$v`) writes back through the alias, so an assignment to `$v`
    /// inside the body lands in the collection. That is modeled with the mirrored copy after the
    /// body, which is what carries taint assigned to the element out into `COLL`.
    ///
    /// The key of a `K => V` pair is modeled as a copy of the collection for the same reason the
    /// value is: a tainted key is reachable from the collection, and nothing here can tell which
    /// field it came from.
    ///
    /// The loop is lowered with the same shape as `while`: a condition block that branches to
    /// either the body or the exit, and a back edge, so a value assigned late in the body is still
    /// visible to a use earlier in it.
    fn lower_foreach(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
        current_namespace: String,
        current_class: Option<String>,
    ) -> Result<(), PhpReaderError> {
        let body = node.child_by_field_name("body");
        // The collection and the loop target are positional: the grammar gives them no field
        // names, and `body` is the only named child that has one.
        let mut header = Vec::new();
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if Some(c) == body {
                continue;
            }
            header.push(c);
        }
        let (Some(&collection_node), Some(&target_node)) = (header.first(), header.get(1)) else {
            log::warn!("foreach with no collection or no loop variable; skipping");
            return Ok(());
        };

        let cond_block = func_ctx.new_block();
        let body_block = func_ctx.new_block();
        let end_block = func_ctx.new_block();

        func_ctx.finish_block_with_goto(cond_block);
        func_ctx.current_block = cond_block;
        let collection = self.lower_place(collection_node, func_ctx)?;
        func_ctx.builder().create_goto(vec![body_block, end_block]);
        self.set_terminator_source_info(func_ctx, node);

        func_ctx.current_block = body_block;

        // `K => V` splits the target; a bare target is the value alone.
        let (key_node, value_node) = if target_node.kind() == "pair" {
            let mut pair_cursor = target_node.walk();
            let parts: Vec<_> = target_node.named_children(&mut pair_cursor).collect();
            (parts.first().copied(), parts.get(1).copied())
        } else {
            (None, Some(target_node))
        };

        // Which element is bound is unknown, so the whole collection is copied into the loop
        // variable: every field of it stays reachable through the binding.
        let collection_val = self.read_place(collection.clone(), func_ctx, collection_node);
        for bind in [key_node, value_node].into_iter().flatten() {
            // `as &$v` wraps the variable; the alias itself is what `by_ref` adds, and the
            // element copy below is the same either way.
            let by_ref = bind.kind() == "by_ref";
            let bind_target = if by_ref {
                bind.named_child(0).unwrap_or(bind)
            } else {
                bind
            };

            if is_place_kind(bind_target.kind()) {
                let dest = self.lower_place(bind_target, func_ctx)?;
                self.write_place(dest, collection_val.clone(), func_ctx, bind_target);
            }
        }

        if let Some(b) = body {
            self.lower_block(b, func_ctx, current_namespace, current_class)?;
        }

        // Write the element back through a by-reference alias, after the body has had its say.
        // The write lands on an element of the collection, not on the collection itself, so it
        // goes to the same shared element field that a subscript with no statically known index
        // lowers to -- which is what a later `$items[0]` reads back.
        if let Some(value_node) = value_node
            && value_node.kind() == "by_ref"
        {
            let mut element_slot = collection;
            element_slot
                .segments
                .push(mir::PathSegment::symbol(ARRAY_ELEMENT_FIELD));
            let element_node = value_node.named_child(0).unwrap_or(value_node);
            let element = self.lower_exp(element_node, func_ctx)?;
            self.write_place(element_slot, element, func_ctx, value_node);
        }

        func_ctx.finish_block_with_goto(cond_block);
        func_ctx.current_block = end_block;
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

    /// The place a bare variable name denotes, honoring `global`/`static` aliases.
    ///
    /// An aliased name is a global heap slot rather than a local; a plain name is a local of the
    /// same name. Superglobals and `$GLOBALS` are handled by [`Lowerer::lower_place`], which is the
    /// entry point for a full place expression.
    fn place_for_variable(&self, name: &str, func_ctx: &FunctionLowerer) -> Place {
        match func_ctx.global_aliases.get(name) {
            Some(slot) => Place {
                base: VariableRef::new_global(),
                segments: vec![mir::PathSegment::symbol(slot)],
            },
            None => Place::variable(VariableRef::new_local(name.to_string())),
        }
    }

    /// Lower an lvalue expression into a [`Place`] without materializing its symbolic accesses.
    ///
    /// Variables, property accesses and subscripts compose here: `$a->b['c']` builds the base `$a`
    /// then appends the fields `b` and `c`. Anything that is not itself a place (a call result, a
    /// literal) is evaluated to a value and spilled into a fresh temporary, whose bare variable
    /// becomes the base -- so `foo()->bar` works too.
    fn lower_place(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
    ) -> Result<Place, PhpReaderError> {
        match node.kind() {
            "variable_name" => {
                let text = self.text(node);
                let name = text.strip_prefix('$').unwrap_or(text);
                Ok(match name {
                    // `$GLOBALS` *is* the global symbol table: the global heap itself, no field of
                    // its own, so `$GLOBALS['g']` and a `global $g` name the same slot.
                    "GLOBALS" => Place::global(),
                    _ if is_superglobal(name) => Place {
                        base: VariableRef::new_global(),
                        segments: vec![mir::PathSegment::symbol(name)],
                    },
                    _ => self.place_for_variable(name, func_ctx),
                })
            }
            "member_access_expression"
            | "nullsafe_member_access_expression"
            | "scoped_property_access_expression" => {
                let obj_node = node
                    .child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("scope"))
                    .unwrap();
                let mut place = self.lower_place(obj_node, func_ctx)?;
                let prop = node.child_by_field_name("name").unwrap();
                let prop_name = self.text(prop);
                place.segments.push(mir::PathSegment::symbol(prop_name));
                Ok(place)
            }
            "subscript_expression" => {
                let obj_node = node
                    .child_by_field_name("object")
                    .or_else(|| node.child(0))
                    .unwrap();
                let mut place = self.lower_place(obj_node, func_ctx)?;
                let index_node = node
                    .child_by_field_name("index")
                    .or_else(|| node.named_child(1));
                // A key the evaluator folds to a constant string names its own field, which is
                // what keeps `$a['evil']` and `$a['safe']` apart. A numeric or dynamic index cannot
                // be pinned down, so it folds to the shared element field.
                let segment = if let Some(index_node) = index_node
                    && self.text(index_node).trim().parse::<i64>().is_err()
                    && let Some(value) = self.evaluator.eval_node(index_node, self.source)
                {
                    mir::PathSegment::symbol(value.as_str())
                } else {
                    mir::PathSegment::symbol(ARRAY_ELEMENT_FIELD)
                };
                place.segments.push(segment);
                Ok(place)
            }
            // Not a place: evaluate it and spill the value into a temporary to root the place on.
            _ => {
                let value = self.lower_exp(node, func_ctx)?;
                match value {
                    Exp::AccessPath(ap) if ap.path.is_empty() => Ok(Place::variable(ap.variable_ref)),
                    other => {
                        let tmp = func_ctx.fresh_temp();
                        let stmt = func_ctx.builder().create_assign(tmp.clone(), vec![other]);
                        self.set_stmt_source_info(func_ctx, stmt, node);
                        Ok(Place::variable(tmp))
                    }
                }
            }
        }
    }

    /// Materialize a place as a readable value, emitting one [`ctadl_ir::mir::StatementKind::Load`]
    /// per symbolic field.
    ///
    /// `a.f.g` becomes `t1 = load a.f; t2 = load t1.g` and evaluates to `t2`; a place with no
    /// fields is its base variable, read directly. This is the read-side counterpart of
    /// [`Lowerer::write_place`].
    fn read_place(&mut self, place: Place, func_ctx: &mut FunctionLowerer, node: Node<'_>) -> Exp {
        let mut cur = place.base;
        for segment in place.segments {
            // PHP builds only symbolic segments; a bare offset would be pointer arithmetic we never
            // synthesize, so treat anything else as a no-op.
            let mir::PathSegment::Symbol(field) = segment else {
                continue;
            };
            let dest = func_ctx.fresh_temp();
            let stmt = func_ctx.builder().create_load(
                dest.clone(),
                AccessPath::from(cur),
                mir::FieldPath::new(field),
            );
            self.set_stmt_source_info(func_ctx, stmt, node);
            cur = dest;
        }
        Exp::AccessPath(AccessPath::from(cur))
    }

    /// Write `value` into a place: a [`ctadl_ir::mir::StatementKind::Load`] for each interior field
    /// and a final [`ctadl_ir::mir::StatementKind::Store`] for the last (or a plain assign when the
    /// place is a bare variable).
    ///
    /// `a.f.g = v` becomes `t1 = load a.f; store t1.g := v`; `x = v` is a plain assign. This is the
    /// write-side counterpart of [`Lowerer::read_place`].
    fn write_place(
        &mut self,
        place: Place,
        value: Exp,
        func_ctx: &mut FunctionLowerer,
        node: Node<'_>,
    ) {
        let mut segments = place.segments;
        // The last symbolic field is the store's field; everything before it is an address to
        // materialize (loads for interior derefs).
        let field = match segments.pop() {
            Some(mir::PathSegment::Symbol(field)) => Some(mir::FieldPath::new(field)),
            Some(other) => {
                segments.push(other);
                None
            }
            None => None,
        };
        let addr = self.read_place(
            Place {
                base: place.base,
                segments,
            },
            func_ctx,
            node,
        );
        let Exp::AccessPath(addr) = addr else {
            unreachable!("read_place always returns an access path");
        };
        let stmt = func_ctx
            .builder()
            .create_assign_or_store(addr, field, value);
        self.set_stmt_source_info(func_ctx, stmt, node);
    }

    fn lower_exp(
        &mut self,
        node: Node<'_>,
        func_ctx: &mut FunctionLowerer,
    ) -> Result<Exp, PhpReaderError> {
        match node.kind() {
            // A variable, property access or subscript read is a place materialized as a value:
            // its symbolic fields become loads (`$obj->prop` ⟶ `t = load obj.prop`), while a bare
            // variable is read directly.
            "variable_name"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
            | "scoped_property_access_expression"
            | "subscript_expression" => {
                let place = self.lower_place(node, func_ctx)?;
                Ok(self.read_place(place, func_ctx, node))
            }
            "string" => {
                let val = self
                    .evaluator
                    .eval_node(node, self.source)
                    .unwrap_or_else(|| self.text(node).to_string());
                Ok(func_ctx.builder().new_str_exp(&val))
            }
            "integer" => Ok(func_ctx.builder().new_str_exp(self.text(node))),
            // `$a = $b` and `$a = &$b` lower the same way. A reference assignment binds the two
            // names to one value rather than copying, but a copy carries taint from right to left
            // exactly as the binding does; what the copy does not carry is a later write through
            // `$a` showing up in `$b`.
            "assignment_expression" | "reference_assignment_expression" => {
                let left = node.child_by_field_name("left").unwrap();
                let right = node.child_by_field_name("right").unwrap();

                let rhs = self.lower_exp(right, func_ctx)?;

                if left.kind() == "variable_name"
                    && let Some(value) = self.evaluator.eval_node(right, self.source)
                {
                    self.evaluator.assign(self.text(left).to_string(), value);
                }

                // Only a real lvalue is a store target. Anything else on the left (e.g. a `list()`
                // destructuring pattern, which this lowering does not model) carries no store.
                if is_place_kind(left.kind()) {
                    let place = self.lower_place(left, func_ctx)?;
                    self.write_place(place, rhs.clone(), func_ctx, node);
                }
                self.mirror_main_scope_global(left, &rhs, func_ctx, node);
                Ok(rhs)
            }
            "anonymous_function" | "arrow_function" => self.lower_closure(node, func_ctx),
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
                    // A call whose callee is spelled out (`foo(..)`, `new Foo`) names its target;
                    // a call through anything else (`$fn(..)`, `$this->cb()(..)`) only knows the
                    // target at runtime, as whatever function pointer the expression evaluates to.
                    // The latter is a FuncPtrCall, which the analysis resolves by following the
                    // pointers assigned into the callee -- naming a *function* after the callee
                    // variable, as a direct call would, would invent one that does not exist.
                    let names_its_callee =
                        fn_name_node.is_some_and(|n| matches!(n.kind(), "name" | "qualified_name"));
                    if names_its_callee {
                        let fn_name = self.text(fn_name_node.expect("checked by names_its_callee"));
                        self.record_called_function(fn_name, args.len());
                        CallStyle::PhpCall {
                            receiver: None,
                            declared_class: None,
                            method_name: None,
                            callee: AccessPath::from(VariableRef::new_local(fn_name.to_string())),
                            kind: PhpCallKind::DirectFunction,
                        }
                    } else {
                        let callee = match fn_name_node {
                            Some(n) => self.lower_exp(n, func_ctx)?,
                            None => Exp::AccessPath(AccessPath::from(VariableRef::new_local(
                                "unknown_func".to_string(),
                            ))),
                        };
                        CallStyle::FuncPtrCall {
                            callee: match callee {
                                Exp::AccessPath(ap) => ap,
                                _ => AccessPath::from(VariableRef::new_local(
                                    "unknown_func".to_string(),
                                )),
                            },
                            signature: None,
                        }
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
    /// Names that denote a global heap slot rather than a local, mapped to the field naming that
    /// slot. Populated by `global` and `static` declarations. See [`Lowerer::lower_exp`].
    global_aliases: BTreeMap<String, String>,
    /// Whether this is the file-level `__php_main__`, whose variables PHP scopes as globals.
    is_main: bool,
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
            global_aliases: BTreeMap::new(),
            is_main: false,
        }
    }

    fn builder(&mut self) -> BasicBlockBuilder<'_> {
        let block_data = &mut self.func.blocks.blocks_mut()[self.current_block];
        BasicBlockBuilder::new(block_data)
    }

    /// Mint a fresh, uniquely-named temporary local (`_t0`, `_t1`, ...).
    fn fresh_temp(&mut self) -> VariableRef {
        let var = VariableRef::new_local(format!("_t{}", self.next_temp_idx));
        self.next_temp_idx += 1;
        var
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
