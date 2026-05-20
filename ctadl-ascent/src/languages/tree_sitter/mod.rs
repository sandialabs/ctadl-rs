//! This module handles the Tree-sitter AST extraction for C files.
//!
//! It is responsible for parsing C source code (POST PREPROCESSOR)
//!
//! # Known Limitations
//!
//!
//! ## Initialialization in `if(int x = 0; x > 7)`
//!
//! This is *legal* C in C23 (according to Gemini), but illegal before C23.
//! Tree-sitter builds an Error node around this, we just drop the error.
//!
//! ## Implicit int return type
//!    
//! Old C allowed a function declaration without an explicit return type.
//! This is quasi-legal C  (and foo is equivalent in type to bar):
//! ```c
//! foo(){
//! return 1;
//! }
//!
//! int bar(){
//! return 1;
//! }
//!
//! ## Non constant subscript indices
//!
//! We do not handle x.y[n].yada   x.y[1] makes a variable named [1] but [n] doesn't make [n]...
//! TODO what does denbuen says about this?
//!
//!
//! ## Pointer references feel the same a values
//!
//! Currently there is no differene between
//!
//! ```c
//!
//! int foo(int *x){
//! return *x
//! }
//!
//! and
//! int bar(int x){
//! return x;
//! }
//! and
//! int *baz(int *x){
//! return x;
//! }
//!

use hashbrown::hash_map::HashMap;

use crate::error::Error;

use ctadl_ir::index::index_vec::IndexVec;
use ctadl_ir::mir::*;

use internment::ArcIntern;
use smallvec::{SmallVec, smallvec};
use streaming_iterator::{IntoStreamingIterator, StreamingIterator};
use tree_sitter::{Parser, Query, QueryCapture, QueryCursor, QueryMatch, Tree};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod experimental_tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarKind {
    Global,
    Local,     // Standard local variable
    Parameter, // Function argument
}

// TODO_JDB: implement var type thing to accomodate parameters have extra *stuff*
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub kind: VarKind,
    pub param_idx: Option<usize>,
    pub param_kind: Option<ParameterType>,
    pub shadows: bool, // this is set at creation time, because at the time of the declaration is when the shadowing occurs,
    // so assigns that have already happened will never ask about the variable again.  you will never add a VarDecl that doesn't shadow, and then later "upgrade it to shadow"
    pub sidx: usize,
}

#[derive(Debug)]
pub struct ScopeBox {
    pub scope_name: String,
    pub parent_idx: Option<usize>,
    pub variables: Vec<VarDecl>,
}

#[derive(Debug, Default)]
pub struct ScopeTree {
    pub scopes: Vec<ScopeBox>,
}

impl ScopeTree {
    pub fn new() -> Self {
        ScopeTree { scopes: Vec::new() }
    }

    pub fn add_scope(&mut self, name: String, parent: Option<usize>) -> usize {
        let new_scope = ScopeBox {
            scope_name: name,
            parent_idx: parent,
            variables: Vec::new(),
        };

        let index = self.scopes.len();
        self.scopes.push(new_scope);
        index
    }

    // this returns just 'symbol' //name, or scope_name.sidx.var
    pub fn to_string(&self, var: &VarDecl) -> String {
        if var.shadows {
            if let Some(scope) = self.scopes.get(var.sidx) {
                return format!("{}.{}.{}", scope.scope_name, var.sidx, var.name);
            } else {
                panic!("Variable had a scope {} that didn't exist", var.sidx);
            }
        }
        var.name.to_string()
    }

    pub fn add_variable(
        &mut self,
        sidx: usize,
        symbol: String,
        kind: VarKind,
        param_idx: Option<usize>,
        param_kind: Option<ParameterType>,
    ) {
        let shadows = self.find_variable(sidx, symbol.as_str()).is_some();
        if kind == VarKind::Parameter {
            //these optionals have gotten out of hand, i'll refactor this once scoping settles down
            assert!(param_idx.is_some());
            assert!(param_kind.is_some())
        }
        if let Some(scope) = self.scopes.get_mut(sidx) {
            scope.variables.push(VarDecl {
                name: symbol,
                kind,
                param_idx,
                param_kind,
                shadows,
                sidx,
            });
        } else {
            panic!("attempt to add to nonexistent scope: {}", sidx)
        }
    }

    pub fn find_variable(&self, start_idx: usize, target_name: &str) -> Option<&VarDecl> {
        let mut current_idx = Some(start_idx);

        while let Some(idx) = current_idx {
            let scope = &self.scopes[idx];

            // Look for the variable in the current scope
            if let Some(var) = scope.variables.iter().find(|v| v.name == target_name) {
                return Some(var); // Found it! Return a reference to the VarDecl
            }

            // Move up the linked list to the parent scope
            current_idx = scope.parent_idx;
        }

        None // Variable not found in this scope or any parents
    }
}

fn link_blocks(
    program: &mut Program,
    from_sv: &ScopeView,
    to_sv: &ScopeView,
    continuation: bool,
) -> Result<(), Error> {
    log::info!(
        "linking (continuation={},{:?} -> {:?}",
        continuation,
        from_sv,
        to_sv
    );
    let fdat = &mut program.functions[from_sv.fidx];
    let target_val = if continuation {
        to_sv.continuation_blidx
    } else {
        to_sv.blidx
    };

    if let Some(block) = fdat.blocks.get_mut(from_sv.blidx) {
        if let Some(termy) = &mut block.terminator {
            match &mut termy.kind {
                TerminatorKind::Goto { targets } => {
                    targets.push(target_val);
                    Ok(())
                }
                TerminatorKind::Return { .. } => Err(Error::TreeSitterParse(format!(
                    "attempt to overwriting return with destination block: {:?} -> {:?}",
                    from_sv, target_val
                ))),
            }
        } else {
            block.terminator = Some(Terminator::new_kind(TerminatorKind::Goto {
                targets: vec![target_val].into(),
            }));
            Ok(())
        }
    } else {
        Err(Error::TreeSitterParse(format!(
            "attempt to link a non existing from block: {:?} -> {:?}",
            from_sv, to_sv
        )))
    }
}

fn add_scoped_block(
    program: &mut Program,
    scope_view: &ScopeView,
    scope_tree: &mut ScopeTree,
    debug_explainer: Option<&str>,
) -> Result<ScopeView, Error> {
    let fdat = &mut program.functions[scope_view.fidx];
    let blidx = fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    let scope_label = format!("{}.cs", scope_view.func_name);
    let sidx = scope_tree.add_scope(scope_label, Some(scope_view.sidx));
    let result = ScopeView {
        func_name: scope_view.func_name.clone(),
        fidx: scope_view.fidx,
        blidx,
        sidx,
        continuation_blidx: scope_view.continuation_blidx,
    };
    link_blocks(program, scope_view, &result, false)?;
    if let Some(explain) = debug_explainer {
        log::info!("ADD_SCOPED_BLOCK: ({}) =---> {:?}", explain, result);
    }
    Ok(result)
}

fn add_block(
    program: &mut Program,
    scope_view: &ScopeView,
    link_the_blocks: bool,
    debug_explainer: Option<&str>,
) -> Result<ScopeView, Error> {
    let fdat = &mut program.functions[scope_view.fidx];
    let blidx = fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    let result = ScopeView {
        func_name: scope_view.func_name.clone(),
        fidx: scope_view.fidx,
        blidx,
        sidx: scope_view.sidx,
        continuation_blidx: scope_view.continuation_blidx,
    };
    if link_the_blocks {
        link_blocks(program, scope_view, &result, false)?;
    }
    if let Some(explain) = debug_explainer {
        log::info!("ADD_BLOCK: ({}) =---> {:?}", explain, result);
    }
    Ok(result)
}

fn add_scope(
    scope_view: &ScopeView,
    scope_tree: &mut ScopeTree,
    debug_explainer: Option<&str>,
) -> ScopeView {
    let scope_label = format!("{}.cs", scope_view.func_name);
    let sidx = scope_tree.add_scope(scope_label, Some(scope_view.sidx));
    let result = ScopeView {
        func_name: scope_view.func_name.clone(),
        fidx: scope_view.fidx,
        blidx: scope_view.blidx,
        sidx,
        continuation_blidx: scope_view.continuation_blidx,
    };
    if let Some(explain) = debug_explainer {
        log::info!("ADD_SCOPE: ({}) =---> {:?}", explain, result);
    }
    result
}

#[derive(Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
struct FunctionName<'a>(&'a str);

#[derive(Debug, Default)]
struct Context<'a> {
    functions: HashMap<FunctionName<'a>, FunctionIdx>,
    param_names: HashMap<FunctionName<'a>, IndexVec<ParameterIdx, &'a str>>,
    scope_tree: ScopeTree,
    allocator: TempAllocator,
}

pub struct MatchExtractor<'q, 'cursor, 'tree> {
    query: &'q Query,
    m: &'cursor QueryMatch<'cursor, 'tree>,
}

impl<'query, 'cursor, 'tree> MatchExtractor<'query, 'cursor, 'tree> {
    pub fn new(query: &'query Query, m: &'cursor QueryMatch<'cursor, 'tree>) -> Self {
        Self { query, m }
    }

    pub fn get(&self, name: &str) -> Result<Node<'tree>, Error> {
        let r = self.get_opt(name);
        if let Some(result) = r {
            Ok(result)
        } else {
            Err(Error::TreeSitterParse(format!(
                "Query failed to find mandatory capture: @{name}"
            )))
        }
    }

    pub fn get_opt(&self, name: &str) -> Option<Node<'tree>> {
        self.m
            .captures
            .iter()
            .find(|c| self.query.capture_names()[c.index as usize] == name)
            .map(|c| c.node)
    }
}

/// Parse the C source in `source` into a CTADL IR program.
/// returns the Program and a flag whether it had tree-sitter-syntax-errors
pub fn parse_c_program(source: &str) -> anyhow::Result<(Program, bool), Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("error loading C grammar");

    let mut ctx = Context::default();
    let mut program = Program::default();
    let tree = parser
        .parse(source, None)
        .expect("tree‐sitter failed to parse");
    ctx.parse(source, &tree, &mut program)?;
    Ok((program, tree.root_node().has_error()))
}

pub fn compile_query(query_src: &str) -> Query {
    Query::new(&tree_sitter_c::LANGUAGE.into(), query_src).unwrap_or_else(|e| {
        let header = "--- Query Syntax Error ---";
        let snippet = query_src
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:3} | {}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        panic!(
            "{}\n{}\nError Message: {}\nAt byte offset: {}",
            header, snippet, e.message, e.offset
        );
    })
}

fn to_str<'b>(n: &Node<'_>, source: &'b str) -> &'b str {
    n.utf8_text(source.as_bytes()).unwrap().trim()
}

// This struct temporarily holds the specific book keeping needs of a function parse
#[derive(Debug, Clone)]
pub struct ScopeView {
    pub func_name: String,
    pub fidx: FunctionIdx,
    pub blidx: BasicBlockIdx,
    pub sidx: usize, // i tried to make my own idx like the blockidx, and fidx, but couldn't figure out how ot import newindex_type. or wahtever
    pub continuation_blidx: BasicBlockIdx,
}

impl<'a> Context<'a> {
    fn parse(
        &mut self,
        source: &'a str,
        tree: &Tree,
        program: &mut Program,
    ) -> anyhow::Result<(), Error> {
        self.toplevel(source, tree, program)
    }

    fn collect_assignment(
        &mut self,
        source: &str,
        program: &mut Program,
        scope_view: &ScopeView,
        target_node: Node<'_>,
        expr_node: Node<'_>,
    ) -> anyhow::Result<Exp, Error> {
        let target_var = self.flatten_expr(program, target_node, source, scope_view)?;

        let rhs_var = self.flatten_expr(program, expr_node, source, scope_view)?;

        self.add_assign_to_program(program, scope_view, target_var.clone(), rhs_var, None);
        Ok(target_var)
    }

    fn walk_compound_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        compound: &Node<'_>,
        scope_view_meowsers: &ScopeView,
    ) -> Result<(), Error> {
        let mut scope_view = scope_view_meowsers.clone();

        let mut cursor = compound.walk();

        for child in compound.children(&mut cursor) {
            if !child.is_named() {
                continue; // we skip , ( stuff like that...
            }
            let kind = child.kind();
            match kind {
                "comment" => {}
                "compound_statement" => {
                    let inner_view = add_scope(
                        &scope_view,
                        &mut self.scope_tree,
                        Some("Compound_statement"),
                    );

                    self.evaluate_block(source, program, &inner_view, &child)?;
                }
                "declaration" => self.walk_declaration(source, program, &scope_view, child)?,
                "assignment_expression" | "expression_statement" => {
                    if let Some(inner_child) = child.child(0) {
                        self.flatten_expr(program, inner_child, source, &scope_view)?;
                    }
                }
                "if_statement" => self.walk_if(source, program, &mut scope_view, child)?,
                "return_statement" => {
                    return self.walk_return(source, program, &mut scope_view, child);
                }
                "ERROR" => {
                    let node_str = to_str(&child, source);
                    log::warn!("Unknown token(2): {kind}: {node_str}");
                }
                _ => {
                    let node_str = to_str(&child, source);
                    log::error!("Unknown token(2): {kind}: {node_str}");
                    debug_print_tree(child, 0, None, Some(5));
                    return Err(Error::TreeSitterParse(
                        format!("Unknown Token({})", kind).to_owned(),
                    ));
                }
            }
        }

        //walked off a compound_statement
        log::info!("EOF linking blocks: ");
        link_blocks(program, &scope_view, scope_view_meowsers, true)?;

        Ok(())
    }

    fn walk_declaration(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        let nest_decl = child
            .child_by_field_name("declarator")
            .expect("Declarations always have declarators");
        let decl_kind = nest_decl.kind();
        let decl_ident = match decl_kind {
            "init_declarator" => nest_decl
                .child_by_field_name("declarator")
                .expect("double declarators on inits"),
            "identifier" => nest_decl,
            _ => {
                return Err(Error::TreeSitterParse(
                    "Declaration didn't had an unexpected kind {decl_kind}".to_owned(),
                ));
            }
        };
        let var_name = to_str(&decl_ident, source);
        self.scope_tree.add_variable(
            scope_view.sidx,
            var_name.to_string(),
            VarKind::Local,
            None,
            None,
        );
        if let Some(vc) = nest_decl.child_by_field_name("value") {
            self.collect_assignment(source, program, scope_view, decl_ident, vc)?;
        };
        Ok(())
    }

    fn walk_return(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        if let Some(ret_val_node) = child.child(1)
            && ret_val_node.kind() != ";"
        {
            let ret_exp = self.flatten_expr(program, ret_val_node, source, &*scope_view)?;
            let term = Terminator::new_kind(TerminatorKind::Return {
                args: vec![ret_exp].into(),
            });
            program.functions[scope_view.fidx].blocks[scope_view.blidx].terminator = Some(term);
        } else {
            program.functions[scope_view.fidx].blocks[scope_view.blidx].terminator =
                Some(Terminator::new_kind(TerminatorKind::Return {
                    args: vec![].into(),
                }));
        }
        Ok(())
    }

    fn walk_if(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        debug_print_tree(child, 0, Some("if"), Some(20));
        let condition = child
            .child_by_field_name("condition")
            .expect("always has condition");
        self.flatten_expr(program, condition, source, &*scope_view)?; // gather field accesses and what not but we don't care about the condition result,etc.
        let consequence = child
            .child_by_field_name("consequence")
            .expect("always has consequence");
        let link_if_to_continuation = true;
        let mut consequence_scope = add_scoped_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            Some("Consequence"),
        )?;
        let continuation = add_block(
            program,
            &*scope_view,
            link_if_to_continuation,
            Some("Continuation"),
        )?;
        consequence_scope.continuation_blidx = continuation.blidx;
        log::info!(
            "FixedUp Consequence Continuation index({:?}):\n\t{:?} ->\n\t{:?}",
            continuation.blidx,
            consequence_scope,
            continuation,
        );
        self.evaluate_block(source, program, &consequence_scope, &consequence)?;
        //the else block
        if let Some(alternative) = child.child_by_field_name("alternative") {
            //braced block:
            debug_print_tree(alternative, 0, Some("alternative"), Some(20));

            // 1. Create a cursor to walk the children
            let mut cursor = alternative.walk();

            // 2. Use an iterator to find the first child that matches the type
            if let Some(cs) = alternative
                .named_children(&mut cursor)
                .find(|c| c.kind() == "compound_statement")
            {
                log::info!("Found the compound statement: {}", cs.kind());
                // It's a non-braced consequent!
                //            if let Some(cs) = alternative.child_by_field_type("compound_statement") {
                let mut alternative_scope = add_scoped_block(
                    program,
                    &*scope_view,
                    &mut self.scope_tree,
                    Some("Alternative"),
                )?;

                alternative_scope.continuation_blidx = continuation.blidx;
                log::info!(
                    "FixedUp Alternative Continuation index({:?}):\n\t{:?} -> \n\t{:?}",
                    continuation.blidx,
                    alternative_scope,
                    continuation
                );

                self.evaluate_block(source, program, &alternative_scope, &cs)?;
            } else {
                return Err(Error::TreeSitterParse(
                    "TODO: Unbraced consequences and alternatives".to_string(),
                ));
            }
        }
        *scope_view = continuation;
        Ok(())
    }

    fn get_param_idx(&self, func_name: &str, var_name: &str) -> Option<ParameterIdx> {
        let param_vec = self.param_names.get(&FunctionName(func_name)).unwrap();
        // Find returns Option<(ParameterIdx, &String)>
        // Map transforms it into Option<ParameterIdx>
        param_vec
            .iter_enumerated()
            .find(|&(_, &p)| p == var_name)
            .map(|(param_idx, _)| param_idx)
    }

    fn build_access_path(
        &self,
        name_pre_scope: &str,
        field_path: FieldAccesses,
        scope_view: &ScopeView,
    ) -> AccessPath {
        let name: String;
        let varkind: VarKind;
        if let Some(vardecl) = self
            .scope_tree
            .find_variable(scope_view.sidx, name_pre_scope)
        {
            name = self.scope_tree.to_string(vardecl);
            varkind = vardecl.kind.clone();
        } else {
            name = name_pre_scope.to_string();
            if name.starts_with("<t")
            // this is a temp
            {
                varkind = VarKind::Local
            } else {
                log::info!("Implicit Global bourn: {}", name);
                varkind = VarKind::Global;
            }
        }

        match varkind {
            VarKind::Global => AccessPath::new_global(name.as_str(), field_path),
            VarKind::Local => ctadl_ir::mir::AccessPath {
                variable_ref: VariableRef::new_local(name),
                path: field_path,
            },
            VarKind::Parameter => {
                if let Some(param_idx) =
                    self.get_param_idx(scope_view.func_name.as_str(), name.as_str())
                {
                    ctadl_ir::mir::AccessPath {
                        variable_ref: VariableRef::new_parameter(param_idx),
                        path: field_path,
                    }
                } else {
                    panic!("no parameter index for parameters");
                }
            }
        } // end match
    }

    // This will gather assigns, find terminator, and recursively descend if you've got inner blocks.
    fn evaluate_block(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        body: &Node<'_>,
    ) -> Result<(), Error> {
        self.walk_compound_statement(source, program, body, scope_view)?;
        Ok(())
    }

    fn toplevel(
        &mut self,
        source: &'a str,
        tree: &Tree,
        program: &mut Program,
    ) -> anyhow::Result<(), Error> {
        let global_sidx = self.scope_tree.add_scope("%GLOBAL".to_string(), None);
        self.collect_functions(source, tree, program, global_sidx)
    }

    fn collect_params(
        &mut self,
        source: &'a str,
        param_list: &Node<'_>,
        fdat: &mut FunctionData,
        function_name: &'a str,
        scope_view: &ScopeView,
    ) -> anyhow::Result<(), Error> {
        let param_names = self
            .param_names
            .entry(FunctionName(function_name))
            .or_default();

        let query_src = r#"
        (parameter_declaration
            declarator: [
                (identifier) @var_name
                (pointer_declarator declarator: (identifier) @var_name) @is_ref
                (array_declarator declarator: (identifier) @var_name) @is_ref
            ]
        )
    "#;
        let query = compile_query(query_src);

        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, *param_list, source.as_bytes());

        let mut ctr = 0;
        while let Some(m) = matches_iter.next() {
            let extract = MatchExtractor::new(&query, m);
            let param_name = extract.get("var_name")?;
            let is_ref = extract.get_opt("is_ref");

            // Check the AST node type of the wrapper!
            let param_type = if is_ref.is_some() {
                ParameterType::ByRef
            } else {
                ParameterType::ByVal
            };

            fdat.params.push(param_type);
            let pn = to_str(&param_name, source);
            param_names.push(pn);

            self.scope_tree.add_variable(
                scope_view.sidx,
                pn.to_string(),
                VarKind::Parameter,
                Some(ctr),
                Some(param_type),
            );
            ctr += 1;
        }
        Ok(())
    }

    /// Flattens an expression into a list of assignments and returns the
    /// variable name (or temp name) that holds the final result of this node.
    fn flatten_expr(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> Result<Exp, Error> {
        //debug_print_tree(node, 0, Some("FLATTEN_EXPR"), Some(50));
        let text = to_str(&node, source); //.to_string();
        match node.kind() {
            "identifier" => Ok(Exp::AccessPath(self.build_access_path(
                text,
                Default::default(),
                scope_view,
            ))),
            "pointer_declarator" => self.flatten_pointer_decl(node, source, scope_view),
            "number_literal" | "string_literal" => Ok(Exp::Str(ArcIntern::<str>::from(text))),

            // COMPOUND NODES: Flatten children first, then generate a temp.
            "binary_expression" => self.flatten_binary(program, node, source, scope_view),

            // PASS-THROUGH NODES: Parentheses don't need their own temp,
            // just pass the inner value up.
            "parenthesized_expression" => {
                // () is not a valid expression.
                let inner_node = node.child(1).expect("missing inner expr");
                self.flatten_expr(program, inner_node, source, scope_view)
            }

            "field_expression" => {
                let mut path_vec = Vec::<&str>::new();
                //let tt = to_str(&node, &source);
                let final_ident = extract_field_expression(node, source, &mut path_vec)?;
                let ret = Exp::AccessPath(self.build_access_path(
                    final_ident,
                    path_vec.into_iter().collect(),
                    scope_view,
                ));
                Ok(ret)
            }

            "assignment_expression" => self.collect_assignment(
                source,
                program,
                scope_view,
                node.child_by_field_name("left").expect("always a left"),
                node.child_by_field_name("right").expect("always a right"),
            ),
            "pointer_expression" => self.flatten_expr(
                program,
                node.child_by_field_name("argument")
                    .expect("always a argument for the * operator"),
                source,
                scope_view,
            ),
            "subscript_expression" => self.flatten_subscript(program, node, source, scope_view),
            "call_expression" => {
                let x = self.allocator.next_temp();
                self.collect_call(program, node, source, scope_view, x)
            }
            _ => {
                debug_print_tree(node, 0, None, None);
                Err(Error::TreeSitterParse(format!(
                    "ERR 78: Unsupported expression type: {}",
                    node.kind()
                )))
            }
        }
    }

    fn flatten_pointer_decl(
        &mut self,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        //how come only this declarator came up in expr? see pointer_decl way?
        if let Some(iden) = node.child_by_field_name("declarator") {
            let symbol = to_str(&iden, source);
            self.scope_tree.add_variable(
                scope_view.sidx,
                symbol.to_string(),
                VarKind::Local,
                None,
                None,
            );
            Ok(Exp::AccessPath(self.build_access_path(
                symbol,
                Default::default(),
                scope_view,
            )))
        } else {
            debug_print_tree(node, 0, None, None);
            Err(Error::TreeSitterParse(
                "Surprised, Pointer Declarators dont always have a declarators".to_string(),
            ))
        }
    }

    fn flatten_binary(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        // 1. Extract the children
        let left_node = node.child_by_field_name("left").expect("missing left");
        let right_node = node.child_by_field_name("right").expect("missing right");
        // 2. Recurse down! (Bottom-up evaluation)
        let left_val = self.flatten_expr(program, left_node, source, scope_view)?;
        let right_val = self.flatten_expr(program, right_node, source, scope_view)?;
        // 3. Generate a new temporary for this specific operation
        let temp_name = self.allocator.next_temp();
        let target = Exp::AccessPath(self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
        ));
        self.add_assign_to_program(program, scope_view, target, left_val, Some(right_val));
        // 5. Return the temporary to whatever parent called us
        Ok(Exp::AccessPath(ctadl_ir::mir::AccessPath {
            variable_ref: VariableRef::new_local(temp_name),
            path: Default::default(),
        }))
    }

    fn flatten_subscript(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        let lhs = self.flatten_expr(
            program,
            node.child_by_field_name("argument").unwrap(),
            source,
            scope_view,
        )?;
        let index = self.flatten_expr(
            program,
            node.child_by_field_name("index").unwrap(),
            source,
            scope_view,
        )?;
        //TODO check if LHS is Exp of type bytes if so you've got 3[f];
        let mut s = format!("[{:?}]", index);
        if let Exp::Str(esp) = index {
            s = format!("[{}]", esp);
        } else {
            log::warn!("Not a str is this an ident? : {}", s);
            s = "[_elem_]".to_string();
        }
        if let Exp::AccessPath(eap) = lhs {
            let mut fields = eap.path.fields.clone();
            fields.push(FieldAccess::Symbol(ArcIntern::<str>::from(s)));

            Ok(Exp::AccessPath(ctadl_ir::mir::AccessPath {
                variable_ref: eap.variable_ref,
                path: fields.into_iter().collect(),
            }))
        } else {
            Err(Error::TreeSitterParse("EAP wasnt accessPath".to_owned()))
        }
    }

    fn collect_arguments(
        &mut self,
        program: &mut Program,
        arg_list: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> Result<SmallVec<[Exp; 4]>, Error> {
        let mut result = SmallVec::new();

        assert_eq!(
            arg_list.kind(),
            "argument_list",
            "extract_arguments called with node kind: {}",
            arg_list.kind()
        );

        //walk does not descend into the grandchildren, neat.
        let mut cursor = arg_list.walk();

        for child in arg_list.children(&mut cursor) {
            if !child.is_named() {
                continue; // we skip , ( stuff like that...
            }
            result.push(self.flatten_expr(program, child, source, scope_view)?);
        }

        Ok(result)
    }

    /*
    Call expression always 'assign' into a temp variable, that way the collect_assignment can be consistent
     */
    fn collect_call(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
        temp_name: String,
    ) -> Result<Exp, Error> {
        let func_name = to_str(
            &(node.child_by_field_name("function").expect("always has")),
            source,
        );

        let call_edges = CallEdges::Explicit(smallvec![func_name.to_string()]);

        let style = CallStyle::DirectCall { call_edges };
        let arg_node = node.child_by_field_name("arguments").expect("always has");
        let args = self.collect_arguments(program, arg_node, source, scope_view)?;

        program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
            StatementKind::CallAssign {
                style,
                rets: vec![VariableRef::new_local(temp_name.clone())].into(),
                args,
            },
        ));
        //we return the temp_name, so that the assignment expression for the actual int x = foo() gets the result of foo()
        Ok(Exp::AccessPath(self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
        )))
    }

    /// parses and creates new functions and parameters
    fn collect_functions(
        &mut self,
        source: &'a str,
        tree: &Tree,
        program: &mut Program,
        global_sidx: usize,
    ) -> anyhow::Result<(), Error> {
        let query_src = r#"
            (function_definition
                type: (primitive_type)? @return_type            	
                declarator: (function_declarator
                    declarator: (identifier) @func.name
                    parameters: (parameter_list) @param_list                                        
                ) @func.dev
                body: (compound_statement) @body)
            "#;

        let query = compile_query(query_src);
        // Each match binds *all* captures.
        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches_iter.next() {
            let extract = MatchExtractor::new(&query, m);
            //boo, so TREE_SITTER doesn't add a node for an implicit int function type
            let return_type = extract.get_opt("return_type");
            let func_name_node = extract.get("func.name")?;
            let param_list = extract.get("param_list")?;
            let body_node = extract.get("body")?;

            let func_name = to_str(&func_name_node, source);
            self.allocator.reset();
            let fidx = *self
                .functions
                .entry(FunctionName(func_name))
                .or_insert_with(|| program.new_function());

            let fdat = &mut program.functions[fidx];
            fdat.name = func_name.to_string();

            //return type, remember C can have an implicit int return type. boo
            let ret_ct = if let Some(rt) = return_type
                && to_str(&rt, source).eq_ignore_ascii_case("void")
            {
                0
            } else {
                1
            };

            fdat.set_return_type(ReturnType { arity: ret_ct });
            let scope_name = format!("{}.params", func_name);
            let blidx = fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
            let param_sidx = self.scope_tree.add_scope(scope_name, Some(global_sidx));
            let para_scope_view = ScopeView {
                func_name: func_name.to_string(),
                fidx,
                blidx,
                sidx: param_sidx,
                continuation_blidx: blidx,
            };
            let body_name = format!("{}.body", func_name);
            self.collect_params(source, &param_list, fdat, func_name, &para_scope_view)?;
            let block_scope = self.scope_tree.add_scope(body_name, Some(param_sidx));
            let block_scope_view = ScopeView {
                func_name: func_name.to_string(),
                fidx,
                blidx,
                sidx: block_scope,
                continuation_blidx: blidx,
            };

            self.evaluate_block(source, program, &block_scope_view, &body_node)?;
        }
        Ok(())
    }

    //this is a helper function to take the SSA list and shove them all into the block
    fn add_assign_to_program(
        &mut self,
        program: &mut Program,
        scope_view: &ScopeView,
        target: Exp,
        left_op: Exp,
        right_op: Option<Exp>,
    ) {
        let val_exp = left_op; //todo get rid of val_exp and just use left_op
        if let Exp::AccessPath(my_path) = target {
            //what's with this if? //todo: why can't i take a Exp::AccessPath?
            let mut fa: Vec<Exp> = [val_exp.clone()].into();
            if let Some(righty) = right_op {
                fa.push(righty);
            }

            let sa = if my_path.path.is_empty() {
                StatementKind::assign(my_path.variable_ref.clone(), fa)
            } else {
                StatementKind::update(my_path, val_exp.clone())
            };
            program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(sa));
        }
    }
}

// A little helper to make grabbing stuff out of the tree-sitter iterator easier
pub fn collect_matches<'a>(
    mut matches: impl StreamingIterator<Item = QueryMatch<'a, 'a>>,
    query: &'a Query,
    source: &'a str,
) -> Vec<(usize, Vec<(&'a str, &'a str)>)> {
    let mut result = Vec::new();
    while let Some(m) = matches.next() {
        result.push((
            m.pattern_index,
            format_captures(m.captures.iter().into_streaming_iter_ref(), query, source),
        ));
    }
    result
}

pub fn collect_captures<'a>(
    captures: impl StreamingIterator<Item = (QueryMatch<'a, 'a>, usize)>,
    query: &'a Query,
    source: &'a str,
) -> Vec<(&'a str, &'a str)> {
    format_captures(captures.map(|(m, i)| m.captures[*i]), query, source)
}

fn format_captures<'a>(
    mut captures: impl StreamingIterator<Item = QueryCapture<'a>>,
    query: &'a Query,
    source: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    while let Some(capture) = captures.next() {
        result.push((
            query.capture_names()[capture.index as usize],
            to_str(&capture.node, source),
        ));
    }
    result
}

use anyhow::Result;
use tree_sitter::Node;

// A simple counter to generate unique temp names (t0, t1, t2...)
#[derive(Debug, Default)]
pub struct TempAllocator {
    counter: usize,
}

impl TempAllocator {
    pub fn new() -> Self {
        Self { counter: 0 }
    }
    pub fn next_temp(&mut self) -> String {
        let name = format!("<t{}>", self.counter);
        self.counter += 1;
        name
    }
    pub fn reset(&mut self) {
        self.counter = 0;
    }
}

/// Recursively prints a Tree-sitter node and all its descendants.
///
/// # Arguments
/// * `node` - The current Tree-sitter node to print.
/// * `depth` - The current recursion depth (start with 0).
/// * `field_name` - The field name of the current node, if any (start with None).
pub fn debug_print_tree(
    node: Node<'_>,
    depth: usize,
    field_name: Option<&str>,
    depth_limit: Option<usize>,
) {
    // 1. Create the visual indentation
    let indent = "  ".repeat(depth);

    // 2. Format the field name nicely if it exists
    let field_prefix = match field_name {
        Some(name) => format!("{name}: "),
        None => String::new(),
    };

    // 3. Print the current node
    log::info!("{}|-- {}{}", indent, field_prefix, node.kind());

    if let Some(dl) = depth_limit
        && depth >= dl
    {
        return;
    }
    // 4. Recurse into all children
    for i in 0..node.child_count() {
        let child = node
            .child(i.try_into().unwrap())
            .expect("Child node should exist");
        let child_field = node.field_name_for_child(i as u32);

        // Increase the depth by 1 for the next level down
        debug_print_tree(child, depth + 1, child_field, depth_limit);
    }
}

// this returns the field expresion chained from the 1st field_expression,
// The final argument of kind "identifier" is returned, as it needs to be stuffed
// in the variable field, while the rest (the out_vec) is the path

fn extract_field_expression<'a>(
    chain: Node<'a>,
    source: &'a str,
    out_vec: &mut Vec<&'a str>,
) -> anyhow::Result<&'a str, Error> {
    if chain.kind() == "identifier" {
        return Ok(to_str(&chain, source));
    }
    //otherwise, we have a field expression, and expect 2 children.
    assert!(
        chain.kind() == "field_expression",
        "Expected only nodes of kind field_expression"
    );
    let argument = chain
        .child_by_field_name("argument")
        .expect("expected all field_expressions have argument,field children");
    let field = chain
        .child_by_field_name("field")
        .expect("expected all field_expressions have argument,field children");

    let final_res = extract_field_expression(argument, source, out_vec);
    out_vec.push(to_str(&field, source));
    final_res
}
