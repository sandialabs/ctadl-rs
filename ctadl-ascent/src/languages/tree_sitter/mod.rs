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

mod test_utils;

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

enum BlockTypeRequest {
    NewBlockOrScopedBlock, // things that induce lexical scope like compound statements.
    JustScope, // things like the conditional of an if.  or compound statements mid expression run.
    JustBlock, // the For loop's scope is defined at the initializer, so we don't want extra scope for the body
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
    pub blocks: Vec<ScopeView>,
}

impl ScopeTree {
    pub fn new() -> Self {
        ScopeTree {
            scopes: Vec::new(),
            blocks: Vec::new(),
        }
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

    pub fn add_block(&mut self, scope_view: &ScopeView) {
        self.blocks.push(scope_view.clone());
    }

    pub fn get_explainers(blocks: &[ScopeView], target_func: &str, target_blidx: u32) -> String {
        //self.blocks
        blocks
            .iter()
            // 1. Keep only the ones that match your criteria
            .filter(|sv| sv.func_name == target_func && sv.blidx.get() == target_blidx)
            // 2. Extract just the explainer string as a string slice (&str)
            .map(|sv| sv.explainer.as_str())
            // 3. Collect them into a temporary Vec, then join them with a newline
            .collect::<Vec<&str>>()
            .join("&")
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

// In order to unify logic around consequents and unbraced body nodes.
pub struct CompoundProxy<'a> {
    pub nodes: Vec<Node<'a>>,
    pub was_compound: bool,
}

impl<'a> CompoundProxy<'a> {
    pub fn from_node(body_node: Node<'a>) -> Self {
        match body_node.kind() {
            // If it's a block, collect all the children inside the braces
            "compound_statement" => {
                let mut cursor = body_node.walk();
                let all_children = body_node.children(&mut cursor).collect();
                Self {
                    nodes: all_children,
                    was_compound: true,
                }
            }
            // If it's an empty statement, return an empty vector
            "empty_statement" | ";" => Self {
                nodes: vec![],
                was_compound: false,
            },
            "expression_statement" => {
                let child = body_node.child(0);
                if let Some(c) = child
                    && _is_empty(&c)
                {
                    return Self {
                        nodes: vec![],
                        was_compound: false,
                    };
                }
                Self {
                    nodes: vec![body_node],
                    was_compound: false,
                }
            }

            // If it's a naked statement, wrap it in a single-element vector
            _ => Self {
                nodes: vec![body_node],
                was_compound: false,
            },
        }
    }
}
//used by if processing
fn _is_empty(node: &Node<'_>) -> bool {
    node.kind() == ";" || node.kind() == "empty_statement"
}

fn get_line_num(node: &Node<'_>) -> usize {
    node.start_position().row //hmm our tests always start w/ a lf.
}

fn link_blocks(
    program: &mut Program,
    from_sv: &ScopeView,
    to_sv: &ScopeView,
    continuation: bool,
) -> Result<(), Error> {
    /*
        //from_sv.explainer == "2.if_continuation(of)::4" {
        log::debug!(
            "linking (continuation={},\n{:?} -> \n{:?}",
            continuation,
            from_sv,
            to_sv
        );
    */

    let target_val = if continuation {
        match to_sv.continuation_blidx {
            Some(idx) => idx,
            None => {
                // Falls off the end of the function body: emit an implicit empty
                // `return` (SSA `complete()` rewrites it into a goto-to-exit).
                // Mirrors the empty-return shape produced by `walk_return`.
                if let Some(block) =
                    program.functions[from_sv.fidx].blocks.get_mut(from_sv.blidx)
                {
                    if block.terminator.is_none() {
                        block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
                            args: vec![].into(),
                        }));
                    }
                    return Ok(());
                }
                return Err(Error::TreeSitterParse(format!(
                    "attempt to link a non existing from block: {:?}",
                    from_sv
                )));
            }
        }
    } else {
        to_sv.blidx
    };

    let fdat = &mut program.functions[from_sv.fidx];
    if let Some(block) = fdat.blocks.get_mut(from_sv.blidx) {
        if let Some(termy) = &mut block.terminator {
            match &mut termy.kind {
                TerminatorKind::Goto { targets } => {
                    log::debug!("Final append {:?} -> {:?}", from_sv.blidx, target_val);
                    targets.push(target_val);
                    Ok(())
                }
                TerminatorKind::Return { .. } => Err(Error::TreeSitterParse(format!(
                    "attempt to overwriting return with destination block: {:?} -> {:?}",
                    from_sv, target_val
                ))),
            }
        } else {
            log::debug!("Final add {:?} -> {:?}", from_sv.blidx, target_val);
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
    link_the_blocks: bool,
    debug_explainer: &str,
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
        explainer: format!("{}.{}", blidx.get(), debug_explainer),
    };
    if link_the_blocks {
        link_blocks(program, scope_view, &result, false)?;
    }

    scope_tree.add_block(&result);
    Ok(result)
}

fn add_block(
    program: &mut Program,
    scope_view: &ScopeView,
    scope_tree: &mut ScopeTree,
    link_the_blocks: bool,
    debug_explainer: &str,
) -> Result<ScopeView, Error> {
    let fdat = &mut program.functions[scope_view.fidx];
    let blidx = fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    let result = ScopeView {
        func_name: scope_view.func_name.clone(),
        fidx: scope_view.fidx,
        blidx,
        sidx: scope_view.sidx,
        continuation_blidx: scope_view.continuation_blidx,
        explainer: format!("{}.{}", blidx.get(), debug_explainer),
    };
    if link_the_blocks {
        link_blocks(program, scope_view, &result, false)?;
    }
    scope_tree.add_block(&result);
    Ok(result)
}

fn add_scope(
    scope_view: &ScopeView,
    scope_tree: &mut ScopeTree,
    debug_explainer: &str,
) -> ScopeView {
    let scope_label = format!("{}.cs", scope_view.func_name);
    let sidx = scope_tree.add_scope(scope_label, Some(scope_view.sidx));

    ScopeView {
        func_name: scope_view.func_name.clone(),
        fidx: scope_view.fidx,
        blidx: scope_view.blidx,
        sidx,
        continuation_blidx: scope_view.continuation_blidx,
        explainer: format!("{}.{}", scope_view.blidx.get(), debug_explainer),
    }
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
pub fn inject_explainers_into_ir(ir_text: &str, views: &[ScopeView]) -> String {
    let mut result = String::with_capacity(ir_text.len() + 500); // Pre-allocate some space
    let mut current_func = String::new();

    for line in ir_text.lines() {
        let trimmed = line.trim_start();

        // 1. Track the current function name
        if trimmed.starts_with("define ") {
            // Extracts "simple_elif" from "define simple_elif(@p0..."
            if let Some(name_part) = trimmed.split(' ').nth(1)
                && let Some(name) = name_part.split('(').next()
            {
                current_func = name.to_string();
            }
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 2. Find the blocks and inject the explainer
        if trimmed.starts_with("begin block_") {
            let mut explainer_text = String::from("**MISSING**");

            // Extract just the number (e.g., from "block_0 [start]:" -> "0")
            if let Some(after_block) = trimmed.split("block_").nth(1) {
                // Take only the digits before the space or colon
                let num_str: String = after_block
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();

                if let Ok(blidx_val) = num_str.parse::<u32>() {
                    // Fetch the explainer using your ScopeTree logic
                    let explainer = ScopeTree::get_explainers(views, &current_func, blidx_val);
                    if !explainer.is_empty() {
                        // Flatten newlines so it stays a single-line comment in the IR
                        explainer_text = explainer.replace('\n', " | ");
                    }
                }
            }

            // Push exactly ONCE: The original line + comment + newline
            result.push_str(&format!("{} // {}\n", line, explainer_text));
            continue;
        }

        // 3. Keep all other lines exactly as they are
        result.push_str(line);
        result.push('\n');
    }

    result
}

pub fn _inject_explainers_into_ir(ir_text: &str, views: &[ScopeView]) -> String {
    let mut result = String::with_capacity(ir_text.len() + 500); // Pre-allocate some space
    let mut current_func = String::new();

    for line in ir_text.lines() {
        let trimmed = line.trim_start();

        // 1. Track the current function name
        if trimmed.starts_with("define ") {
            // Extracts "simple_elif" from "define simple_elif(@p0..."
            if let Some(name_part) = trimmed.split(' ').nth(1)
                && let Some(name) = name_part.split('(').next()
            {
                current_func = name.to_string();
            }
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 2. Find the blocks and inject the explainer
        if trimmed.starts_with("begin block_") {
            // Keep the original 'begin block_X' line (Remove this push if you want to strictly overwrite it)
            result.push_str(line);
            result.push('\n');

            // Extract just the number (e.g., from "block_0 [start]:" -> "0")
            if let Some(after_block) = trimmed.split("block_").nth(1) {
                // Take only the digits before the space or colon
                let num_str: String = after_block
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();

                if let Ok(blidx_val) = num_str.parse::<u32>() {
                    // Fetch the explainer using our previous iterator logic
                    let explainer = ScopeTree::get_explainers(views, &current_func, blidx_val);

                    if !explainer.is_empty() {
                        result.push_str(format!("{}//{}", line, explainer).as_str());
                    } else {
                        result.push_str(format!("{}// **MISSING**", line).as_str());
                    }
                    /*                    if !explainer.is_empty() {
                        // Inject the explainer (formatted as comments so the IR remains readable)
                        for exp_line in explainer.lines() {
                            result.push_str(&format!("    // Explainer: {}\n", exp_line));
                        }
                    }*/
                }
            }
            continue;
        } else {
            // 3. Keep all other lines exactly as they are
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

fn markup(program: &Program, ctx: &Context<'_>) -> String {
    let dump = program.to_string();
    inject_explainers_into_ir(&dump, &ctx.scope_tree.blocks)
}

/// Parse the C source in `source` into a CTADL IR program.
/// returns the Program and a flag whether it had tree-sitter-syntax-errors
pub fn parse_c_program(source: &str) -> anyhow::Result<(Program, bool, String), Error> {
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
    let marked_up = markup(&program, &ctx);
    Ok((program, tree.root_node().has_error(), marked_up))
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
    // `None` is the fall-off-the-end sentinel: a continuation link to `None` becomes
    // an implicit `return` rather than a `goto` back to the entry block.
    pub continuation_blidx: Option<BasicBlockIdx>,
    pub explainer: String,
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
        operator_node: Option<Node<'_>>,
    ) -> Result<Exp, Error> {
        let target_var = self.flatten_expr(program, target_node, source, scope_view)?;
        let rhs_var = self.flatten_expr(program, expr_node, source, scope_view)?;
        let mut right_op = None;

        if let Some(oper_node) = operator_node
            && oper_node.kind() != "="
        {
            //these are y+= expr type things.
            right_op = Some(&target_var);
        }

        self.add_assign_to_program(program, scope_view, &target_var, &rhs_var, right_op);
        Ok(target_var)
    }

    fn setup_compound<'b>(
        &mut self,
        program: &mut Program,
        // scope_tree: &mut ScopeTree,
        scope_view: &mut ScopeView,
        node: Node<'b>,
        block_type: BlockTypeRequest, // is this a new execution block? or just scope?
        link_the_blocks: bool,
        explainer: &str,
    ) -> Result<(ScopeView, CompoundProxy<'b>), Error> {
        let cp = CompoundProxy::from_node(node);
        //is it this guy's job to add blocks? i should think so.

        match block_type {
            BlockTypeRequest::NewBlockOrScopedBlock => {
                if cp.was_compound {
                    Ok((
                        add_scoped_block(
                            program,
                            scope_view,
                            &mut self.scope_tree,
                            link_the_blocks,
                            explainer,
                        )?,
                        cp,
                    ))
                } else {
                    Ok((
                        add_block(
                            program,
                            scope_view,
                            &mut self.scope_tree,
                            link_the_blocks,
                            explainer,
                        )?,
                        cp,
                    ))
                }
            } //end BTR::NewBLock
            BlockTypeRequest::JustScope => {
                if cp.was_compound {
                    Ok((add_scope(scope_view, &mut self.scope_tree, explainer), cp))
                } else {
                    Err(Error::TreeSitterParse(
                        "Requested JustScope on a non-compound node".to_string(),
                    ))
                }
            } // end BTR::JustScope
            BlockTypeRequest::JustBlock => Ok((
                add_block(
                    program,
                    scope_view,
                    &mut self.scope_tree,
                    link_the_blocks,
                    explainer,
                )?,
                cp,
            )),
        }
    }

    fn walk_compound_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view_meowsers: &ScopeView,
        compound: &CompoundProxy<'_>,
    ) -> Result<(), Error> {
        let mut scope_view = scope_view_meowsers.clone();

        /*  let mut cursor = compound.walk();

        for child in compound.children(&mut cursor) {
        */
        for &child in &compound.nodes {
            if !child.is_named() {
                continue; // we skip , ( stuff like that...
            }
            let kind = child.kind();
            match kind {
                "comment" => {}
                "compound_statement" => {
                    let (inner_view, cp) = self.setup_compound(
                        program,
                        &mut scope_view,
                        child,
                        BlockTypeRequest::JustScope,
                        true,
                        "compound_statement",
                    )?;
                    self.walk_compound_statement(source, program, &inner_view, &cp)?;
                }
                "declaration" => {
                    self.walk_declaration(source, program, &scope_view, child)?;
                }
                "assignment_expression" => {
                    self.flatten_expr(program, child, source, &scope_view)?;
                }
                "expression_statement" | "update_expression" => {
                    if let Some(inner_child) = child.child(0) {
                        self.flatten_expr(program, inner_child, source, &scope_view)?;
                    }
                }
                "parenthesized_expression" => {
                    if let Some(inner_child) = child.child(1) {
                        self.flatten_expr(program, inner_child, source, &scope_view)?;
                    }
                }
                "if_statement" => self.walk_if(source, program, &mut scope_view, child)?,
                "while_statement" => {
                    self.walk_while(source, program, &mut scope_view, child)?;
                }
                "do_statement" => {
                    self.walk_do_while(source, program, &mut scope_view, child)?;
                }
                "for_statement" => {
                    self.walk_for(source, program, &mut scope_view, child)?;
                }
                "return_statement" => {
                    return self.walk_return(source, program, &mut scope_view, child);
                }
                "ERROR" => {
                    let node_str = to_str(&child, source);
                    log::warn!("Unknown token(2): {kind}: {node_str}");
                }
                _ => {
                    self.flatten_expr(program, child, source, &scope_view)?;
                    /*
                    let node_str = to_str(&child, source);
                    log::error!(
                        "Unknown token?maybe? or we fall through now(2): {kind}: {node_str}"
                    );*/

                    /*debug_print_tree(child, 0, None, Some(5));
                    return Err(Error::TreeSitterParse(
                        format!("Unknown Token({})", kind).to_owned(),
                    ));*/
                }
            }
        }

        //walked off a compound_statement
        log::info!("EOCS linking blocks: ");
        link_blocks(program, &scope_view, scope_view_meowsers, true)?;

        Ok(())
    }

    fn walk_declaration(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        node: Node<'_>,
    ) -> Result<(), Error> {
        let mut cursor = node.walk();

        for nest_decl in node.children_by_field_name("declarator", &mut cursor) {
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
                self.collect_assignment(source, program, scope_view, decl_ident, vc, None)?;
            };
        }
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
    fn walk_for(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        //  debug_print_tree(child, 0, Some("do"), Some(20));

        let initializer_node = child
            .child_by_field_name("initializer")
            .expect("always has initializer");
        let condition_node = child
            .child_by_field_name("condition")
            .expect("always has initializer");
        let update_node = child
            .child_by_field_name("update")
            .expect("always has initializer");
        let body_node = child.child_by_field_name("body").expect("always has body");

        let for_sidx = self
            .scope_tree
            .add_scope("for_loop".to_string(), Some(scope_view.sidx));
        let mut for_sv = ScopeView {
            func_name: scope_view.func_name.clone(),
            fidx: scope_view.fidx,
            blidx: scope_view.blidx,
            sidx: for_sidx,
            continuation_blidx: scope_view.continuation_blidx,
            explainer: "for_loop".to_string(),
        };

        let (mut init_scope, init_cp) = self.setup_compound(
            program,
            &mut for_sv,
            initializer_node,
            BlockTypeRequest::JustBlock,
            true,
            "for_initializer_block",
        )?;

        let (mut body_scope, body_cp) = self.setup_compound(
            program,
            &mut init_scope,
            body_node,
            BlockTypeRequest::JustBlock,
            false,
            "for_body",
        )?;

        let (mut update_scope, update_cp) = self.setup_compound(
            program,
            &mut body_scope,
            update_node,
            BlockTypeRequest::JustBlock,
            false,
            "for_update",
        )?;

        let (mut condition_scope, condition_cp) = self.setup_compound(
            program,
            &mut update_scope,
            condition_node,
            BlockTypeRequest::JustBlock,
            false,
            "for_condition",
        )?;
        let continuation = add_block(
            program,
            &condition_scope,
            &mut self.scope_tree,
            false,
            "for_Continuation",
        )?;

        condition_scope.continuation_blidx = Some(body_scope.blidx);
        init_scope.continuation_blidx = Some(condition_scope.blidx);
        body_scope.continuation_blidx = Some(update_scope.blidx);
        update_scope.continuation_blidx = Some(condition_scope.blidx);
        //        self.flatten_expr(program, condition, source, &condition_sv)?; // gather field accesses and what not but we don't care about the condition result,etc.
        self.walk_compound_statement(source, program, &init_scope, &init_cp)?;
        self.walk_compound_statement(source, program, &condition_scope, &condition_cp)?;
        //add 'sad edge'
        link_blocks(program, &condition_scope, &continuation, false)?;
        //what is the difference between walk_compound_statemnet and walk_compound_statement?
        body_scope.continuation_blidx = Some(update_scope.blidx);
        self.walk_compound_statement(source, program, &body_scope, &body_cp)?;
        self.walk_compound_statement(source, program, &update_scope, &update_cp)?;
        *scope_view = continuation;
        Ok(())
    }

    fn walk_do_while(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        //debug_print_tree(child, 0, Some("do"), Some(20));

        let body_node = child.child_by_field_name("body").expect("always has body");

        let (mut body_scope, body_cp) = self.setup_compound(
            program,
            scope_view,
            body_node,
            BlockTypeRequest::NewBlockOrScopedBlock,
            true,
            "do_while_body",
        )?;

        let condition = child
            .child_by_field_name("condition")
            .expect("always has condition");

        let (mut condition_sv, cp) = self.setup_compound(
            program,
            &mut body_scope,
            condition,
            BlockTypeRequest::NewBlockOrScopedBlock,
            false, //we'll get the link from walking the body
            "while_condition",
        )?;

        let continuation = add_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            false,
            "Continuation",
        )?;

        condition_sv.continuation_blidx = Some(continuation.blidx);
        //        self.flatten_expr(program, condition, source, &condition_sv)?; // gather field accesses and what not but we don't care about the condition result,etc.
        self.walk_compound_statement(source, program, &condition_sv, &cp)?;
        // A do-while tests *after* the body, then loops back into it: add the
        // back-edge from the condition to the body. The exit edge to the
        // continuation was already added by the condition's end-of-compound link.
        link_blocks(program, &condition_sv, &body_scope, false)?;
        body_scope.continuation_blidx = Some(condition_sv.blidx);
        self.walk_compound_statement(source, program, &body_scope, &body_cp)?;
        *scope_view = continuation;
        Ok(())
    }

    // there is a lot of commonality between the top of the while and the top of the if.. then the if descends into madness.
    // hopefully we can use "walk_while" more generically for all the looping constructs
    fn walk_while(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        //debug_print_tree(child, 0, Some("while"), Some(20));
        let condition = child
            .child_by_field_name("condition")
            .expect("always has condition");

        let (mut condition_sv, cp) = self.setup_compound(
            program,
            scope_view,
            condition,
            BlockTypeRequest::NewBlockOrScopedBlock,
            true,
            "while_condition",
        )?;

        let continuation = add_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            false,
            "Continuation",
        )?;

        condition_sv.continuation_blidx = Some(continuation.blidx);
        //        self.flatten_expr(program, condition, source, &condition_sv)?; // gather field accesses and what not but we don't care about the condition result,etc.
        self.walk_compound_statement(source, program, &condition_sv, &cp)?;

        let body_node = child.child_by_field_name("body").expect("always has body");

        let (mut body_scope, cp) = self.setup_compound(
            program,
            &mut condition_sv,
            body_node,
            BlockTypeRequest::NewBlockOrScopedBlock,
            true,
            "while_body",
        )?;

        body_scope.continuation_blidx = Some(condition_sv.blidx);
        self.walk_compound_statement(source, program, &body_scope, &cp)?;
        *scope_view = continuation;
        Ok(())
    }

    fn walk_if(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        //debug_print_tree(child, 0, Some("if"), Some(20));
        let condition = child
            .child_by_field_name("condition")
            .expect("always has condition");
        self.flatten_expr(program, condition, source, &*scope_view)?; // gather field accesses and what not but we don't care about the condition result,etc.
        let consequence = child
            .child_by_field_name("consequence")
            .expect("always has consequence");

        let (mut consequence_sv, if_cond_cp) = self.setup_compound(
            program,
            scope_view,
            consequence,
            BlockTypeRequest::NewBlockOrScopedBlock,
            true,
            format!("if_consequence(of)::{}", get_line_num(&child)).as_str(),
        )?;

        // Created without auto-linking: the condition only falls through to the
        // continuation when there is no `else` (added in the `else` arm below).
        let mut continuation = add_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            false,
            format!("if_continuation(of)::{}", get_line_num(&child)).as_str(),
        )?;

        // The if-continuation inherits the enclosing scope's continuation (which may be
        // the `None` fall-off-the-end sentinel); the consequence flows into it.
        continuation.continuation_blidx = scope_view.continuation_blidx;
        consequence_sv.continuation_blidx = Some(continuation.blidx);
        self.walk_compound_statement(source, program, &consequence_sv, &if_cond_cp)?;
        //the else block
        if let Some(alternative) = child.child_by_field_name("alternative") {
            //debug_print_tree(alternative, 0, Some("alternative"), Some(20));

            let mut cursor = alternative.walk();
            // The else clause's body is its first non-comment child: an `if_statement`
            // for `else if`, a `compound_statement` for a braced else, or a bare
            // statement for an unbraced else (e.g. `else x = z;`).
            if let Some(cs) = alternative
                .named_children(&mut cursor)
                .find(|c| c.kind() != "comment")
            {
                match cs.kind() {
                    "if_statement" => {
                        //it's an else if
                        let mut if_block =
                            add_block(program, scope_view, &mut self.scope_tree, true, "if")?;
                        if_block.continuation_blidx = Some(continuation.blidx);
                        self.walk_if(source, program, &mut if_block, cs)?;
                        // `if_block` is now the else-if's own continuation block. Nobody
                        // walks it, so without this it ends up with no terminator (an
                        // orphaned join). Tie it to our continuation.
                        link_blocks(program, &if_block, &if_block, true)?;
                    }
                    _ => {
                        // A braced `{ ... }` or an unbraced single statement; both are
                        // handled by setup_compound / CompoundProxy::from_node.
                        let (mut alternative_sv, alternative_cp) = self.setup_compound(
                            program,
                            scope_view,
                            cs,
                            BlockTypeRequest::NewBlockOrScopedBlock,
                            true,
                            "alternative",
                        )?;
                        alternative_sv.continuation_blidx = Some(continuation.blidx);
                        self.walk_compound_statement(
                            source,
                            program,
                            &alternative_sv,
                            &alternative_cp,
                        )?;
                    }
                }
            }
        } else {
            // No `else`: the condition's false path falls through to the
            // continuation. (With an `else` the false path goes to the alternative,
            // which is linked above, so the condition must not also reach the join.)
            link_blocks(program, scope_view, &continuation, false)?;
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
        //       debug_print_tree(*param_list, 0, None, None); //depth, field_name, depth_limit);
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
            "comma_expression" => {
                let ch1 = node.child_by_field_name("left").expect("always left");
                let ch2 = node.child_by_field_name("right").expect("always right");
                self.flatten_expr(program, ch1, source, scope_view)?;
                self.flatten_expr(program, ch2, source, scope_view)
            }
            "pointer_declarator" | "function_declarator" => {
                self.flatten_nested_decl(program, node, source, scope_view)
            }
            "number_literal" | "string_literal" => Ok(Exp::Str(ArcIntern::<str>::from(text))),
            "unary_expression" => {
                let ch = node
                    .child_by_field_name("argument")
                    .expect("always has an argument");
                self.flatten_expr(program, ch, source, scope_view)
            }
            // COMPOUND NODES: Flatten children first, then generate a temp.
            "binary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .expect("always has an operator");
                self.flatten_binary(program, node, operator, source, scope_view)
            }
            "update_expression" => {
                self.flatten_update_expression(program, node, source, scope_view)
            }

            // PASS-THROUGH NODES: Parentheses don't need their own temp,
            // just pass the inner value up.
            "parenthesized_expression" | "parenthesized_declarator" => {
                // () is not a valid expression.
                //debug_print_tree(node, 0, None, Some(50));
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
                /*Some(
                node.child_by_field_name("operator")
                    .expect("always has operator"),*/
                node.child_by_field_name("operator"),
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

    fn flatten_nested_decl(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        //how come only this declarator came up in expr? see pointer_decl way?
        // ... well function_declarators come up too.  see the logic there //TODO: why does this not worry about parenthesized_declarators?
        if let Some(iden) = node.child_by_field_name("declarator") {
            //oh noes.. look whats under that! a pointer declarator!
            if iden.kind() == "identifier" {
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
                //iden was something nested
                self.flatten_expr(program, iden, source, scope_view)
            }
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
        operator: Node<'_>,
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

        match operator.kind() {
            "==" | "<=" | ">=" => {
                //todo: what are all of these?
                log::info!("Not assigning for comparison operators");
            }
            _ => {
                self.add_assign_to_program(
                    program,
                    scope_view,
                    &target,
                    &left_val,
                    Some(&right_val),
                );
            }
        }

        Ok(Exp::AccessPath(ctadl_ir::mir::AccessPath {
            variable_ref: VariableRef::new_local(temp_name),
            path: Default::default(),
        }))
    }

    fn flatten_update_expression(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        // 1. Extract the children
        let argument = node.child_by_field_name("argument").expect("missing left");
        //let right_node = node.child_by_field_name("right").expect("missing right");
        // 2. Recurse down! (Bottom-up evaluation)
        let left_val = self.flatten_expr(program, argument, source, scope_view)?;
        // let right_val = self.flatten_expr(program, right_node, source, scope_view)?;
        let right_val = Exp::Str(ArcIntern::<str>::from("1"));
        // 3. Generate a new temporary for this specific operation
        let temp_name = self.allocator.next_temp();
        let target = Exp::AccessPath(self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
        ));
        self.add_assign_to_program(program, scope_view, &target, &left_val, Some(&right_val));
        // 5. Return the temporary to whatever parent called us
        self.add_assign_to_program(program, scope_view, &left_val, &target, None);
        let text = to_str(&argument, source); //.to_string();

        Ok(Exp::AccessPath(self.build_access_path(
            text,
            Default::default(),
            scope_view,
        )))
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
    // hmmm DEF TODO: figure out a x->v().. seems like  we need a path_vec containing the start
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

        let arg_node = node.child_by_field_name("arguments").expect("always has");
        let args = self.collect_arguments(program, arg_node, source, scope_view)?;

        let path_vec = Vec::<&str>::new();

        let access_path =
            self.build_access_path(func_name, path_vec.into_iter().collect(), scope_view);

        let var = &*access_path.variable_ref.variable;
        let style = match var {
            Variable::Local(name) => {
                log::info!("This is an Indirect LOCAL call: {}", name);
                CallStyle::FuncPtrCall {
                    callee: access_path,
                    signature: (Some("FOR THOSE ABOUT TO ROCK!".to_string())),
                }
            }
            Variable::Param(idx) => {
                log::info!("This is an Indirect PARAMETER call: {}", idx.get());
                CallStyle::FuncPtrCall {
                    callee: access_path,
                    signature: (Some("FOR THOSE ABOUT TO ROCK!".to_string())),
                }
            }
            Variable::GlobalHeap => CallStyle::DirectCall { call_edges },
        };

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
            //debug_print_tree(body_node, 0, None, Some(50));
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
                continuation_blidx: None,
                explainer: "params".to_string(),
            };

            let body_name = format!("{}.body", func_name);
            self.collect_params(source, &param_list, fdat, func_name, &para_scope_view)?;

            //we have to build this one by hand, becuase we want the initial scope without the extra block
            let block_scope = self.scope_tree.add_scope(body_name, Some(param_sidx));
            let block_scope_view = ScopeView {
                func_name: func_name.to_string(),
                fidx,
                blidx,
                sidx: block_scope,
                continuation_blidx: None,
                explainer: "initial_block".to_string(),
            };
            self.scope_tree.blocks.push(block_scope_view.clone());
            let cp = CompoundProxy::from_node(body_node);

            self.walk_compound_statement(source, program, &block_scope_view, &cp)?;
        }
        Ok(())
    }

    //this is a helper function to take the SSA list and shove them all into the block
    fn add_assign_to_program(
        &mut self,
        program: &mut Program,
        scope_view: &ScopeView,
        target: &Exp,
        left_op: &Exp,
        right_op: Option<&Exp>,
    ) {
        let val_exp = left_op; //todo get rid of val_exp and just use left_op
        if let Exp::AccessPath(my_path) = target {
            //what's with this if? //todo: why can't i take a Exp::AccessPath?
            let mut fa: Vec<Exp> = [val_exp.clone()].into();
            if let Some(righty) = right_op {
                fa.push(righty.clone());
            }

            let sa = if my_path.path.is_empty() {
                StatementKind::assign(my_path.variable_ref.clone(), fa)
            } else {
                StatementKind::update(my_path.clone(), val_exp.clone())
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
