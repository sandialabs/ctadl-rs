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
use hashbrown::hash_set::HashSet;

use crate::error::Error;

use ctadl_ir::ThinVec;
use ctadl_ir::index::index_vec::IndexVec;
use ctadl_ir::mir::*;

use source_info::{ArtifactEncoding, ArtifactKey, ArtifactMetadata, SourceInfoBuilder, SpanLen};

use internment::ArcIntern;
use streaming_iterator::{IntoStreamingIterator, StreamingIterator};
use tree_sitter::{Parser, Query, QueryCapture, QueryCursor, QueryMatch, Tree};

mod test_utils;
mod testing_block_flow_ascii;

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

/// A base variable plus a mixed (offset + symbolic-field) path, before lowering to loads/stores.
/// Access paths in the IR are offset-only and load/store fields are single symbols, so C member
/// and subscript accesses are threaded here and lowered via [`mir::load_access_path`] /
/// [`mir::store_access_path`].
#[derive(Debug, Clone)]
struct RawPath {
    base: VariableRef,
    fields: ThinVec<PathSegment>,
}

impl RawPath {
    fn new(base: VariableRef, fields: ThinVec<PathSegment>) -> Self {
        Self { base, fields }
    }

    fn is_pathless(&self) -> bool {
        self.fields.is_empty()
    }
}

enum BlockTypeRequest {
    NewBlockOrScopedBlock, // things that induce lexical scope like compound statements.
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
                if let Some(block) = program.functions[from_sv.fidx]
                    .blocks
                    .get_mut(from_sv.blidx)
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
        break_target: scope_view.break_target,
        continue_target: scope_view.continue_target,
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
        break_target: scope_view.break_target,
        continue_target: scope_view.continue_target,
        explainer: format!("{}.{}", blidx.get(), debug_explainer),
    };
    if link_the_blocks {
        link_blocks(program, scope_view, &result, false)?;
    }
    scope_tree.add_block(&result);
    Ok(result)
}

#[derive(Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
struct FunctionName<'a>(&'a str);

/// Synthetic field name that all members of a `union` variable collapse to, so they share a
/// single access path (union members alias -- they occupy the same storage). The `$` keeps it
/// out of the C identifier space, so it can never collide with a real source-level field.
const UNION_FIELD: &str = "$union";

#[derive(Debug, Default)]
struct Context<'a> {
    functions: HashMap<FunctionName<'a>, FunctionIdx>,
    param_names: HashMap<FunctionName<'a>, IndexVec<ParameterIdx, &'a str>>,
    scope_tree: ScopeTree,
    allocator: TempAllocator,
    /// Block that each `goto` label maps to, for the function currently being walked.
    /// Labels are function-scoped and can be jumped to before they are defined, so
    /// (unlike `break`/`continue` targets, which ride on `ScopeView`) the blocks are
    /// created in a pre-scan over the whole body and looked up here. Reset per function.
    label_blocks: HashMap<String, BasicBlockIdx>,
    /// Intraprocedural must-points-to for address-taken locals: maps a pointer variable
    /// `p` to the access path it was taken to (`x` after `p = &x`) together with the basic
    /// block in which that binding was established. Used to resolve a dereference `*p` --
    /// as a store LHS (`*p = v`) or a load RHS (`y = *p`) -- to the pointee `x`, so a write
    /// through the alias taints `x` (the F3 soundness gap: CTADL models pointers as value
    /// copies, which is sound for reads but drops the write-back). The block key confines
    /// each binding to the straight-line region it was recorded in: a lookup only trusts an
    /// entry whose block matches the current `blidx`, so once control flow moves to another
    /// block the alias is dropped and we fall back to the value-copy model. That keeps the
    /// must-points-to exact (no cross-branch may-alias reasoning) and never less sound than
    /// before. Reset per function.
    addr_alias: HashMap<VariableRef, (AccessPath, BasicBlockIdx)>,
    /// Variables declared with a `union` type. A union's members share storage, so every
    /// member access aliases the others (`u.a = v` is observable at a read of `u.b`). CTADL
    /// is otherwise field-sensitive -- correct for structs, whose members are disjoint --
    /// so union members are collapsed to a single synthetic field (see `UNION_FIELD`) when a
    /// `field_expression` is lowered off one of these variables, making all members the same
    /// access path (the F4 soundness gap). Populated from `union_specifier`-typed local
    /// declarations; reset per function.
    union_vars: HashSet<VariableRef>,
    /// Builder that interns source spans, or `None` when spans are not being recorded
    /// (the `parse_c_program` path used by tests and the marked-up dump). `import_c`
    /// installs a builder so imported IR carries locations back to the C source.
    source_info: Option<SourceInfoBuilder>,
    /// Maps offsets in the parsed buffer back to the original file(s). Empty (and thus
    /// span-less) unless `import_c` populated it. See [`read_c_source`].
    file_map: FileMap,
    /// Span attached to every IR statement emitted while lowering the C statement currently
    /// being walked. Set once per source statement in [`Context::walk_statement`] so that all
    /// the IR it expands into (calls, loads, stores) points back at that statement.
    cur_span: SourceInfo,
}

/// One source file's placement inside the combined parse buffer produced by
/// [`read_c_source`].
#[derive(Debug)]
struct FileSlice {
    /// Offset in the combined buffer where this file's content begins.
    combined_start: usize,
    /// Length in bytes of this file's content within the combined buffer.
    len: usize,
    /// Path of the original file (as displayed).
    path: String,
    /// SHA-256 of the file's content, matching the store's artifact-hash scheme.
    hash: Vec<u8>,
}

/// Maps offsets in the combined parse buffer back to the original source file and the
/// offset within it, so IR spans reference real files even when a directory is parsed
/// as one concatenated translation unit. Slices are non-overlapping; the marker lines
/// inserted between files are gaps that map to no file.
#[derive(Debug, Default)]
struct FileMap {
    slices: Vec<FileSlice>,
}

impl FileMap {
    /// Locate the file containing combined-buffer offset `off`, returning that file's
    /// artifact key, the offset within the file, and the number of bytes remaining in
    /// the file from that offset (so a span can be clamped to the file boundary).
    fn locate(&self, off: usize) -> Option<(ArtifactKey, u32, usize)> {
        let slice = self
            .slices
            .iter()
            .find(|s| off >= s.combined_start && off < s.combined_start + s.len)?;
        let local = off - slice.combined_start;
        let key = ArtifactKey {
            path: slice.path.clone(),
            sub_artifact_id: 0,
            hash: slice.hash.clone(),
            encoding: ArtifactEncoding::Utf8,
        };
        Some((key, local as u32, slice.len - local))
    }
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

/// Import C source at `path` into a [`ProgramInfo`], ready for [`crate::cli::import`].
///
/// `path` may be a single C source file (`.c`) or header (`.h`), or a directory
/// tree containing such files. A directory is imported as one translation unit:
/// every `.h` and `.c` file it contains (recursively) is concatenated -- headers
/// first, then `.c` files, each group in sorted path order -- and parsed together,
/// so declarations in the headers are in scope for the definitions that use them
/// and references resolve across files.
///
/// The frontend expects post-preprocessor C source: `#include` directives are not
/// expanded here, so a directory should already contain preprocessed translation
/// units (or self-contained sources).
pub fn import_c(path: &std::path::Path) -> Result<ProgramInfo, Error> {
    let (source, file_map) = read_c_source(path)?;
    let (program, source_info, has_error, _marked_up) =
        parse_c_with_source_info(&source, file_map)?;
    if has_error {
        log::warn!(
            "tree-sitter reported syntax errors while parsing C source at '{}'; \
             the imported IR may be incomplete (is the input already preprocessed?)",
            path.display()
        );
    }
    Ok(ProgramInfo {
        program,
        source_info,
        ..Default::default()
    })
}

/// Parse `source` into a [`Program`] together with the [`source_info::SourceInfo`] that maps
/// its IR statements back to the original files described by `file_map`. This is the
/// span-recording variant of [`parse_c_program`], used by [`import_c`].
fn parse_c_with_source_info(
    source: &str,
    file_map: FileMap,
) -> Result<(Program, source_info::SourceInfo, bool, String), Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("error loading C grammar");

    let mut ctx = Context {
        source_info: Some(SourceInfoBuilder::new(ArtifactMetadata::new())),
        file_map,
        ..Default::default()
    };
    let mut program = Program::default();
    let tree = parser
        .parse(source, None)
        .expect("tree‐sitter failed to parse");
    ctx.parse(source, &tree, &mut program)?;
    // Give called-but-undefined functions (external declarations like `int source();`) a
    // function id so taint models can match them by name. Only the import path needs this;
    // the in-memory `parse_c_program` used by unit tests deliberately omits these stubs.
    define_extern_functions(&mut program);
    let marked_up = markup(&program, &ctx);
    let has_error = tree.root_node().has_error();
    let source_info = ctx
        .source_info
        .take()
        .expect("builder is Some in this path")
        .finish();
    Ok((program, source_info, has_error, marked_up))
}

/// Read the C source for [`import_c`], returning the buffer to parse and a [`FileMap`]
/// mapping offsets in that buffer back to the original files. A single file maps to
/// itself; a directory is concatenated into one translation unit -- every header and
/// `.c` file underneath it (headers first, then `.c` files, each group in sorted path
/// order) -- and the map records where each file landed so IR spans still name it.
fn read_c_source(path: &std::path::Path) -> Result<(String, FileMap), Error> {
    if !path.is_dir() {
        let contents = std::fs::read_to_string(path)?;
        let mut map = FileMap::default();
        map.slices.push(FileSlice {
            combined_start: 0,
            len: contents.len(),
            path: path.display().to_string(),
            hash: source_info::sha256(contents.as_bytes()),
        });
        return Ok((contents, map));
    }

    let mut headers = Vec::new();
    let mut sources = Vec::new();
    collect_c_files(path, &mut headers, &mut sources)?;
    if headers.is_empty() && sources.is_empty() {
        return Err(Error::Path {
            message: format!("no .c or .h files found under '{}'", path.display()),
        });
    }
    headers.sort();
    sources.sort();

    let mut combined = String::new();
    let mut map = FileMap::default();
    for file in headers.iter().chain(sources.iter()) {
        let contents = std::fs::read_to_string(file)?;
        // Mark where each file's contents begin (aids debugging the merged unit)
        // and separate files with newlines so tokens across a boundary never merge.
        combined.push_str(&format!("// ==== {} ====\n", file.display()));
        // The file's content begins *after* the marker line; record that so spans
        // land at the right offset within the original file.
        let combined_start = combined.len();
        let hash = source_info::sha256(contents.as_bytes());
        combined.push_str(&contents);
        map.slices.push(FileSlice {
            combined_start,
            len: contents.len(),
            path: file.display().to_string(),
            hash,
        });
        combined.push('\n');
    }
    Ok((combined, map))
}

/// Recursively collect `.h` files into `headers` and `.c` files into `sources`
/// under `dir`. Any other file is ignored.
fn collect_c_files(
    dir: &std::path::Path,
    headers: &mut Vec<std::path::PathBuf>,
    sources: &mut Vec<std::path::PathBuf>,
) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_c_files(&path, headers, sources)?;
        } else if metadata.is_file() {
            match path.extension().and_then(|e| e.to_str()) {
                Some("h") => headers.push(path),
                Some("c") => sources.push(path),
                _ => {}
            }
        }
    }
    Ok(())
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

/// Collect the names of every `labeled_statement` label reachable under `node`
/// (recursing through nested blocks/ifs/loops). Used to pre-create a block per label
/// before the body is walked, so a `goto` to a not-yet-seen label still resolves.
fn collect_labels(node: Node<'_>, source: &str, out: &mut Vec<String>) {
    if node.kind() == "labeled_statement"
        && let Some(label) = node.child_by_field_name("label")
    {
        out.push(to_str(&label, source).to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_labels(child, source, out);
    }
}

/// Whether any `labeled_statement` — i.e. a `goto` target — appears anywhere under `node`.
fn has_label(node: Node<'_>) -> bool {
    if node.kind() == "labeled_statement" {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).any(has_label)
}

/// Register every directly-called function that has no definition in this translation unit
/// as an empty-body (external) function.
///
/// Taint models identify sources, sinks, and propagators by name (`source`, `sink`,
/// `malloc`, ...), and both the model engine and query-time endpoint resolution look the
/// name up among the program's functions. A function that is only *declared* in C (e.g.
/// `int source();`) never reaches `collect_functions` -- which matches `function_definition`
/// nodes -- so without this pass its calls have edges pointing at a name that no IR function
/// carries, and every model targeting it silently matches nothing. Creating an empty-body
/// function gives the name a function id the model/query can resolve; the empty body also
/// marks it external during indexing (see codegen's `external_function`). Mirrors the extern
/// pass in the dex/jvm frontends. Runs on the import path only (see `parse_c_with_source_info`).
fn define_extern_functions(program: &mut Program) {
    use std::collections::BTreeMap;

    // Direct-call target name -> the largest argument count seen at any call site, so an
    // `AnyArgument`/by-index model has enough formal parameters to anchor to. A BTreeMap
    // keeps creation order deterministic.
    let mut called: BTreeMap<String, usize> = BTreeMap::new();
    // Names that already have a body; these must not be recreated.
    let mut defined: HashSet<String> = HashSet::new();
    for func in program.functions.iter() {
        if !func.blocks.is_empty() {
            defined.insert(func.name.clone());
        }
        for block in func.blocks.iter() {
            for stmt in &block.statements {
                if let StatementKind::CallAssign {
                    style:
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit(names),
                        },
                    args,
                    ..
                } = &stmt.kind
                {
                    for name in names {
                        let slot = called.entry(name.clone()).or_insert(0);
                        *slot = (*slot).max(args.len());
                    }
                }
            }
        }
    }

    for (name, arity) in called {
        if defined.contains(&name) {
            continue;
        }
        let fidx = program.new_function();
        let fdat = &mut program.functions[fidx];
        fdat.name = name;
        for _ in 0..arity {
            fdat.params.push(ParameterType::ByVal);
        }
    }
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
    // Where a `break` jumps: the innermost enclosing `switch`/loop continuation.
    // `None` means there is no enclosing breakable construct (a `break` here is an
    // error). Like `continuation_blidx`, this rides along by value as we descend, so
    // a child scope that overrides it has the parent's value automatically restored
    // on return — no explicit stack/push/pop is needed.
    pub break_target: Option<BasicBlockIdx>,
    // Where a `continue` jumps: the innermost enclosing loop's re-test/update block.
    // A `switch` deliberately leaves this untouched, so a `continue` inside a switch
    // arm still targets the enclosing loop (matching C semantics).
    pub continue_target: Option<BasicBlockIdx>,
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
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        target_node: Node<'_>,
        expr_node: Node<'_>,
        operator_node: Option<Node<'_>>,
    ) -> Result<Exp, Error> {
        let target_ap = self.flatten_lvalue(program, target_node, source, scope_view)?;
        let rhs_var = self.flatten_expr(program, expr_node, source, scope_view)?;

        // A compound assignment (`y += expr`) needs the current value of the target as a second
        // operand; read it (a field target lowers to loads) before the write.
        let compound = operator_node.is_some_and(|o| o.kind() != "=");
        let right_op = if compound {
            Some(Exp::access_path(self.emit_loads(
                program,
                scope_view,
                target_ap.clone(),
            )))
        } else {
            None
        };

        self.add_assign_to_program(program, scope_view, &target_ap, &rhs_var, right_op.as_ref());

        // Maintain the address-of must-points-to map (see `Context::addr_alias`). Only a
        // plain, whole assignment to a variable (`p = &x`, or a declarator initializer)
        // updates it; a store through a dereference (`*p = ...`, whose `target_node` is
        // itself a `pointer_expression`) writes *through* the alias and must not disturb it.
        if target_node.kind() != "pointer_expression" && target_ap.is_pathless() {
            let is_plain_assign = operator_node.is_none_or(|op| op.kind() == "=");
            let addr_of_pointee = if is_plain_assign
                && expr_node.kind() == "pointer_expression"
                && expr_node
                    .child_by_field_name("operator")
                    .is_some_and(|op| to_str(&op, source) == "&")
            {
                // `p = &x`: `rhs_var` is the pointee (`&x` flattened to `x`). A plain local
                // pointee is an `Exp::Variable`; a field/global pointee is an `Exp::AccessPath`.
                match &rhs_var {
                    Exp::Variable(v) => Some(AccessPath::without_fields(v.clone())),
                    Exp::AccessPath(pointee) => Some(pointee.clone()),
                    _ => None,
                }
            } else {
                None
            };
            match addr_of_pointee {
                Some(pointee) => {
                    self.addr_alias
                        .insert(target_ap.base.clone(), (pointee, scope_view.blidx));
                }
                // Any other assignment to `p` (a different pointer, a computed value, a
                // compound `+=`) makes its pointee unknown -- drop the stale binding so a
                // later `*p` falls back to the value-copy model instead of resolving to the
                // wrong local.
                None => {
                    self.addr_alias.remove(&target_ap.base);
                }
            }
        }

        // The value of an assignment expression is the assigned location, so a chained
        // assignment (`b = a = 5`) flows the target `a` into `b`. A field target (a store) has
        // no bare-variable value, so fall back to the assigned value there.
        if target_ap.is_pathless() {
            Ok(Exp::Variable(target_ap.base))
        } else {
            Ok(rhs_var)
        }
    }

    /// Lower an aggregate brace initializer (`int a[2] = { s, 0 }`,
    /// `struct P p = { s, 0 }`) into per-element stores. `decl_ident` is the declarator
    /// being initialized (an `array_declarator` for arrays, an `identifier` for structs
    /// / scalars); flattening it yields -- and registers -- the base access path.
    fn collect_initializer_list(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        decl_ident: Node<'_>,
        init_list: Node<'_>,
    ) -> Result<(), Error> {
        let base_ap = self.flatten_lvalue(program, decl_ident, source, scope_view)?;
        self.lower_initializer_list(source, program, scope_view, &base_ap, init_list)
    }

    /// Walk the elements of an `initializer_list`, storing each into a successive
    /// synthetic index field `[i]` of `base_ap` -- the same `[N]` field shape a
    /// constant-index subscript read (`a[0]`) resolves to (see `flatten_subscript`), so
    /// taint deposited here is later observed at the read. Positional struct fields reuse
    /// the same `[i]` synthesis (no type info to recover member names). Nested aggregates
    /// (`{{..},{..}}`) recurse, extending the base path by the outer index.
    fn lower_initializer_list(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        base_ap: &RawPath,
        init_list: Node<'_>,
    ) -> Result<(), Error> {
        let mut cursor = init_list.walk();
        let mut idx = 0usize;
        for elem in init_list.children(&mut cursor) {
            if !elem.is_named() {
                continue; // skip the `{`, `,`, `}` tokens
            }
            // Pick this element's target sub-field + its value node.
            let (field, value_node) = if elem.kind() == "initializer_pair" {
                // Designated: `.member = e` or `[n] = e`.
                let designator = elem
                    .child_by_field_name("designator")
                    .expect("initializer_pair always has a designator");
                let value = elem
                    .child_by_field_name("value")
                    .expect("initializer_pair always has a value");
                let dtext = to_str(&designator, source);
                let field = if let Some(member) = dtext.strip_prefix('.') {
                    // `.a` -> Symbol("a"), matching how a `.a` field read is lowered.
                    PathSegment::symbol(member.trim())
                } else {
                    // `[n]` array designator -> the same `[n]` symbol a subscript read uses.
                    PathSegment::symbol(dtext.trim())
                };
                (field, value)
            } else {
                // Positional element -> successive `[i]`.
                let field = PathSegment::symbol(format!("[{idx}]"));
                idx += 1;
                (field, elem)
            };
            let mut elem_ap = base_ap.clone();
            elem_ap.fields.push(field);
            if value_node.kind() == "initializer_list" {
                self.lower_initializer_list(source, program, scope_view, &elem_ap, value_node)?;
            } else {
                let rhs = self.flatten_expr(program, value_node, source, scope_view)?;
                self.add_assign_to_program(program, scope_view, &elem_ap, &rhs, None);
            }
        }
        Ok(())
    }

    /// [`Self::setup_compound`] (as [`BlockTypeRequest::JustBlock`]) for a clause C
    /// allows to be omitted — the `for` header's initializer, condition and update.
    /// A missing clause still gets its own, empty, block so the loop's block structure
    /// is the same whether or not the clause is written.
    fn setup_optional_compound<'b>(
        &mut self,
        program: &mut Program,
        scope_view: &mut ScopeView,
        node: Option<Node<'b>>,
        link_the_blocks: bool,
        explainer: &str,
    ) -> Result<(ScopeView, CompoundProxy<'b>), Error> {
        match node {
            Some(node) => self.setup_compound(
                program,
                scope_view,
                node,
                BlockTypeRequest::JustBlock,
                link_the_blocks,
                explainer,
            ),
            None => Ok((
                add_block(
                    program,
                    scope_view,
                    &mut self.scope_tree,
                    link_the_blocks,
                    explainer,
                )?,
                CompoundProxy {
                    nodes: vec![],
                    was_compound: false,
                },
            )),
        }
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

    /// Lower the statements of a compound. Returns `true` if the compound *diverged* —
    /// a statement in it terminated the current block with no fall-through — in which
    /// case the end-of-compound link was skipped and no caller may link out of that
    /// block either.
    fn walk_compound_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view_meowsers: &ScopeView,
        compound: &CompoundProxy<'_>,
    ) -> Result<bool, Error> {
        let mut scope_view = scope_view_meowsers.clone();

        // we skip , ( stuff like that...
        let stmts: Vec<Node<'_>> = compound
            .nodes
            .iter()
            .copied()
            .filter(Node::is_named)
            .collect();
        for (i, &child) in stmts.iter().enumerate() {
            // A statement that diverges (return/break/continue, or a label whose body
            // diverges) ends the compound; the trailing fall-through link is skipped.
            if self.walk_statement(source, program, &mut scope_view, child)? {
                let rest = &stmts[i + 1..];
                // ... unless what follows holds a `goto` label. The tail is unreachable by
                // fall-through, but a label in it is still a jump target: the standard C
                // error-handling shape puts `return 0;` immediately before the `err_out:`
                // labels that the body's own `goto`s target. Abandoning the compound here
                // left those pre-created label blocks empty *and* unterminated while the
                // gotos pointed straight at them. So keep lowering, from a fresh unlinked
                // block -- the same treatment `walk_goto` gives the code after a `goto`.
                if !rest.iter().any(|n| has_label(*n)) {
                    return Ok(true);
                }
                scope_view = add_block(
                    program,
                    &scope_view,
                    &mut self.scope_tree,
                    false,
                    &format!("after_diverge::{}", get_line_num(&child)),
                )?;
            }
        }

        //walked off a compound_statement
        log::debug!("EOCS linking blocks: ");
        link_blocks(program, &scope_view, scope_view_meowsers, true)?;

        Ok(false)
    }

    /// Lower a single statement, threading `scope_view` (so control-flow statements can
    /// move the "current block" for following statements). Returns `true` if the
    /// statement *diverged* — i.e. it terminated the current block with no fall-through
    /// (`return`/`break`/`continue`, or a `labeled_statement` whose body diverges) — so
    /// the enclosing compound should stop and skip its end-of-compound link.
    /// Intern a span for `node`'s byte range (mapped back to the original file via
    /// [`FileMap`]) and return the [`SourceInfo`] pointing at it. Returns the default
    /// (no-span) `SourceInfo` when span recording is off or the offset falls outside any
    /// known file (e.g. the marker lines between concatenated files).
    fn span_for_node(&mut self, node: Node<'_>) -> SourceInfo {
        let start = node.start_byte();
        let end = node.end_byte();
        let Some((key, local_start, max_len)) = self.file_map.locate(start) else {
            return SourceInfo::default();
        };
        let len = end.saturating_sub(start).min(max_len) as u32;
        match self.source_info.as_mut() {
            Some(builder) => {
                SourceInfo::new(builder.span_for(key, local_start, SpanLen::ByteLen(len)))
            }
            None => SourceInfo::default(),
        }
    }

    fn walk_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<bool, Error> {
        let kind = child.kind();
        // Every IR statement lowered from this C statement inherits its source span.
        self.cur_span = self.span_for_node(child);
        match kind {
            "comment" => {}
            // A nested block `{ .. }`. It introduces a lexical scope but not a basic
            // block: its statements lower into the *current* block, and `scope_view` is
            // threaded through them so blocks they do create (an `if` inside the braces)
            // carry on into the rest of the enclosing compound.
            //
            // This deliberately does not go through `walk_compound_statement`. That would
            // end the nested block with a fall-through link resolved against the enclosing
            // *continuation*, which skips the remaining siblings -- and at the top level of
            // a function body, where there is no continuation, stamps an implicit `return`
            // onto the shared block, so the next statement then fails to link out of it.
            "compound_statement" => {
                let outer_sidx = scope_view.sidx;
                scope_view.sidx = self
                    .scope_tree
                    .add_scope(format!("{}.cs", scope_view.func_name), Some(outer_sidx));
                let mut cursor = child.walk();
                let stmts: Vec<Node<'_>> =
                    child.children(&mut cursor).filter(Node::is_named).collect();
                let mut diverged = false;
                for stmt in stmts {
                    if self.walk_statement(source, program, scope_view, stmt)? {
                        diverged = true;
                        break;
                    }
                }
                // Leave the current *block* wherever the statements moved it, but close
                // the scope: declarations inside the braces must not be visible after them.
                scope_view.sidx = outer_sidx;
                return Ok(diverged);
            }
            "declaration" => {
                self.walk_declaration(source, program, scope_view, child)?;
            }
            "assignment_expression" => {
                self.flatten_expr(program, child, source, scope_view)?;
            }
            "expression_statement" => {
                // An empty statement (`;`) -- e.g. the body of a label, `done: ;` --
                // parses as an `expression_statement` whose only child is the `;` token.
                // There is no expression to lower, so skip it; otherwise the bare `;`
                // falls through to `flatten_expr`'s catch-all and fails ingestion (ERR 78).
                if let Some(inner_child) = child.child(0)
                    && !_is_empty(&inner_child)
                {
                    self.flatten_expr(program, inner_child, source, scope_view)?;
                }
            }
            // `i++` / `++i` at statement position -- notably a `for` update clause, which
            // reaches here unwrapped by an `expression_statement`. Lower the whole node,
            // not `child(0)`: for a prefix update that child is the `++`/`--` token
            // itself, which has no `flatten_expr` arm (ERR 78), and for a postfix one it
            // is the bare operand, which silently drops the increment.
            "update_expression" => {
                self.flatten_expr(program, child, source, scope_view)?;
            }
            "parenthesized_expression" => {
                if let Some(inner_child) = child.child(1) {
                    self.flatten_expr(program, inner_child, source, scope_view)?;
                }
            }
            "if_statement" => self.walk_if(source, program, scope_view, child)?,
            "while_statement" => {
                self.walk_while(source, program, scope_view, child)?;
            }
            "do_statement" => {
                self.walk_do_while(source, program, scope_view, child)?;
            }
            "for_statement" => {
                self.walk_for(source, program, scope_view, child)?;
            }
            "switch_statement" => {
                self.walk_switch(source, program, scope_view, child)?;
            }
            // A `case X:` outside the `switch` that owns it, which happens when a `#if`
            // region slices a switch body up (`case A: \n #ifdef X \n case B: .. \n #endif`)
            // and the arms land inside the region instead. `walk_switch` never saw them, so
            // lower the arm's statements here, inline in the current block: they fall
            // through from whatever precedes them rather than getting their own edge from
            // the switch entry -- imprecise, but it keeps their code, and `break_target` is
            // inherited so a `break` inside still leaves the switch.
            "case_statement" => {
                let value_id = child.child_by_field_name("value").map(|v| v.id());
                let mut cursor = child.walk();
                let cp = CompoundProxy {
                    nodes: child
                        .children(&mut cursor)
                        .filter(|n| n.is_named() && Some(n.id()) != value_id)
                        .collect(),
                    was_compound: false,
                };
                return self.walk_compound_statement(source, program, scope_view, &cp);
            }
            // Preprocessor directives inside a function body mean the input was not
            // preprocessed. A conditional region becomes a non-deterministic branch over
            // its arms; the rest (`#define`, `#undef`, `#include`, `#pragma`, `#error`,
            // and bare macro invocations at statement position) has no runtime effect
            // here, so it is skipped rather than hitting `flatten_expr`'s catch-all.
            "preproc_if" | "preproc_ifdef" => {
                self.walk_preproc_conditional(source, program, scope_view, child)?;
            }
            "preproc_def"
            | "preproc_function_def"
            | "preproc_call"
            | "preproc_include"
            | "preproc_directive" => {}
            // A `#if` region that cuts an expression or declaration in half (common in
            // the middle of a `return a && \n #ifdef X \n b && \n #endif \n c;`) leaves
            // the parser recovering into top-level forms *inside* the region, so a
            // function or type definition can turn up at statement position. Function
            // definitions are collected by their own query over the whole tree, so they
            // are already lowered on their own and re-lowering here would duplicate them;
            // the rest are pure type/attribute syntax and carry no dataflow at all.
            "function_definition"
            | "type_definition"
            | "linkage_specification"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "static_assert_declaration"
            | "attribute_declaration"
            | "ms_declspec_modifier" => {}
            // `return`/`break`/`continue` terminate the current block and have no
            // fall-through, so they end the compound (skipping its end link). A
            // `break`/`continue` with no enclosing target is the exception: it lowers to
            // a no-op and reports no divergence, so lowering continues past it.
            "return_statement" => {
                self.walk_return(source, program, scope_view, child)?;
                return Ok(true);
            }
            "break_statement" => {
                return self.walk_break(source, program, scope_view, &child);
            }
            "continue_statement" => {
                return self.walk_continue(source, program, scope_view, &child);
            }
            // Unlike break/continue, a `goto` does NOT end the compound: code after it
            // is unreachable but may hold labels that must still lower, so it updates
            // `scope_view` (to a fresh block) and we fall through to the next sibling.
            "goto_statement" => self.walk_goto(source, program, scope_view, child)?,
            // A label's body diverges iff its inner statement does; propagate that so a
            // `L: return x;` at the tail of a compound doesn't leave a dangling block.
            "labeled_statement" => {
                return self.walk_labeled_statement(source, program, scope_view, child);
            }
            "ERROR" => {
                let node_str = to_str(&child, source);
                log::warn!("Unknown token(2): {kind}: {node_str}");
            }
            _ => {
                self.flatten_expr(program, child, source, scope_view)?;
            }
        }
        Ok(false)
    }

    fn walk_declaration(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        node: Node<'_>,
    ) -> Result<(), Error> {
        let mut cursor = node.walk();

        // A `union`-typed declaration (`union U u;`, inline `union U { .. } u;`, or anonymous
        // `union { .. } u;`) has a `union_specifier` as its type. Its members share storage, so
        // record the declared variables to collapse their member accesses (see `union_vars`).
        let is_union = node
            .child_by_field_name("type")
            .is_some_and(|t| t.kind() == "union_specifier");

        for nest_decl in node.children_by_field_name("declarator", &mut cursor) {
            let decl_kind = nest_decl.kind();
            let decl_ident = match decl_kind {
                "init_declarator" => nest_decl
                    .child_by_field_name("declarator")
                    .expect("double declarators on inits"),
                "identifier" => nest_decl,
                // Function-pointer / pointer / array declarators without an
                // initializer, e.g. `int (*op_func)(int, int);`. Recurse to
                // register the inner identifier as a local; there is no value
                // to assign.
                "function_declarator"
                | "pointer_declarator"
                | "parenthesized_declarator"
                | "array_declarator" => {
                    self.flatten_expr(program, nest_decl, source, scope_view)?;
                    continue;
                }
                _ => {
                    return Err(Error::TreeSitterParse(format!(
                        "Declaration declarator had an unexpected kind {decl_kind}"
                    )));
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
            // Mark a plainly-declared union variable so its member accesses collapse. Only a
            // bare identifier declarator is handled (pointer/array union declarators take the
            // `continue` path above and are left to the value-copy model).
            if is_union && decl_ident.kind() == "identifier" {
                let vref = self
                    .build_access_path(
                        var_name,
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    )
                    .base;
                self.union_vars.insert(vref);
            }
            if let Some(vc) = nest_decl.child_by_field_name("value") {
                if vc.kind() == "initializer_list" {
                    // Aggregate brace initializer, e.g. `int a[2] = { s, 0 }`. Lower it
                    // to per-element stores `a[i] = elem_i` so taint flows into the
                    // indexed access paths a later `a[0]` read resolves to. (Without this
                    // the `initializer_list` reaches `flatten_expr`'s catch-all -> ERR 78.)
                    self.collect_initializer_list(source, program, scope_view, decl_ident, vc)?;
                } else {
                    self.collect_assignment(source, program, scope_view, decl_ident, vc, None)?;
                }
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

        // Every clause of a `for` header is optional in C (`for (;;)`), so each is looked
        // up as an `Option` and an absent one lowers to an empty block (see
        // `setup_optional_compound`) -- keeping the loop's block structure, and the
        // `break`/`continue` targets pointing into it, identical either way.
        let initializer_node = child.child_by_field_name("initializer");
        let condition_node = child.child_by_field_name("condition");
        let update_node = child.child_by_field_name("update");
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
            break_target: scope_view.break_target,
            continue_target: scope_view.continue_target,
            explainer: "for_loop".to_string(),
        };

        let (mut init_scope, init_cp) = self.setup_optional_compound(
            program,
            &mut for_sv,
            initializer_node,
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

        let (mut update_scope, update_cp) = self.setup_optional_compound(
            program,
            &mut body_scope,
            update_node,
            false,
            "for_update",
        )?;

        let (mut condition_scope, condition_cp) = self.setup_optional_compound(
            program,
            &mut update_scope,
            condition_node,
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
        // `break` leaves the loop; `continue` jumps to the update expression (which
        // then re-tests the condition). Set on the body view so they ride into every
        // nested non-loop block and are restored after the loop.
        body_scope.break_target = Some(continuation.blidx);
        body_scope.continue_target = Some(update_scope.blidx);
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
        // `break` leaves the loop; `continue` jumps to the post-body condition test.
        body_scope.break_target = Some(continuation.blidx);
        body_scope.continue_target = Some(condition_sv.blidx);
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
        // `break` leaves the loop; `continue` jumps back to the condition re-test.
        body_scope.break_target = Some(continuation.blidx);
        body_scope.continue_target = Some(condition_sv.blidx);
        self.walk_compound_statement(source, program, &body_scope, &cp)?;
        *scope_view = continuation;
        Ok(())
    }

    /// Lower a `#if`/`#ifdef` region that survived into a function body, i.e. the input
    /// was not preprocessed. Which arm the compiler would keep depends on macro
    /// definitions we do not have, so treat the region exactly like a `switch`: the entry
    /// block branches non-deterministically to a block per arm (`#if` body, then each
    /// `#elif`/`#else` body in the `alternative` chain), and every arm falls through to a
    /// shared continuation. When the chain has no `#else`, entry also links straight to
    /// the continuation — the "no arm is compiled" case. Lowering every arm keeps all of
    /// the code visible to taint (over-approximate, the safe direction); dropping the
    /// region instead would silently lose whatever flows it contains.
    ///
    /// Arms inherit the enclosing scope view, so a `break`/`continue` inside `#ifdef`
    /// still targets the enclosing loop or switch.
    fn walk_preproc_conditional(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        // Walk the `alternative` chain, collecting each directive's statement children:
        // everything but its condition/name and the nested alternative itself.
        let mut arms: Vec<Vec<Node<'_>>> = Vec::new();
        let mut has_else = false;
        let mut directive = Some(child);
        while let Some(node) = directive {
            has_else = node.kind() == "preproc_else";
            let skip: Vec<usize> = ["condition", "name", "alternative"]
                .iter()
                .filter_map(|f| node.child_by_field_name(f))
                .map(|n| n.id())
                .collect();
            let mut cursor = node.walk();
            arms.push(
                node.children(&mut cursor)
                    .filter(|n| n.is_named() && !skip.contains(&n.id()))
                    .collect(),
            );
            directive = node.child_by_field_name("alternative");
        }

        // Where control resumes after `#endif`, and where every arm falls through to.
        let continuation = add_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            false,
            format!("preproc_continuation(of)::{}", get_line_num(&child)).as_str(),
        )?;

        for (i, stmts) in arms.into_iter().enumerate() {
            let mut arm_sv = add_block(
                program,
                &*scope_view,
                &mut self.scope_tree,
                false,
                format!("preproc_arm{i}(of)::{}", get_line_num(&child)).as_str(),
            )?;
            link_blocks(program, &*scope_view, &arm_sv, false)?;
            arm_sv.continuation_blidx = Some(continuation.blidx);
            let cp = CompoundProxy {
                nodes: stmts,
                was_compound: false,
            };
            self.walk_compound_statement(source, program, &arm_sv, &cp)?;
        }
        if !has_else {
            link_blocks(program, &*scope_view, &continuation, false)?;
        }

        *scope_view = continuation;
        Ok(())
    }

    /// Lower a `switch` the way `if` is lowered: path-insensitively. The scrutinee
    /// is flattened for its side effects but does not select a branch — the entry
    /// block jumps non-deterministically to every arm. Arms fall through to the
    /// next arm (C semantics) unless a `break` redirects to the continuation.
    fn walk_switch(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        // `switch ( <condition> ) <body>`. Flatten the scrutinee for side effects.
        let condition = child
            .child_by_field_name("condition")
            .expect("switch always has a condition");
        self.flatten_expr(program, condition, source, &*scope_view)?;

        let body = child
            .child_by_field_name("body")
            .expect("switch always has a body");

        // Where control resumes after the switch, and the target of every `break`.
        // `add_block` inherits the enclosing continuation, so a fall-off the end of
        // the post-switch code still links correctly.
        let continuation = add_block(
            program,
            &*scope_view,
            &mut self.scope_tree,
            false,
            format!("switch_continuation(of)::{}", get_line_num(&child)).as_str(),
        )?;

        // The arms. `default:` is a valueless `case_statement`; there is no separate
        // `default` node kind.
        let mut cursor = body.walk();
        let arms: Vec<Node<'_>> = body
            .children(&mut cursor)
            .filter(|n| n.kind() == "case_statement")
            .collect();
        let has_default = arms
            .iter()
            .any(|a| a.child_by_field_name("value").is_none());

        // One block per arm, created up front so each arm can fall through to the
        // next one. They inherit the switch's scope view, so each arm's
        // `continue_target` is the enclosing loop's (a `switch` is transparent to
        // `continue`); only `break_target` is overridden below.
        let mut arm_svs: Vec<ScopeView> = Vec::with_capacity(arms.len());
        for i in 0..arms.len() {
            arm_svs.push(add_block(
                program,
                &*scope_view,
                &mut self.scope_tree,
                false,
                format!("switch_case{i}(of)::{}", get_line_num(&child)).as_str(),
            )?);
        }

        // Entry branches (non-deterministically) to every arm, plus straight to the
        // continuation when no `default` guarantees an arm runs (covers an empty
        // switch and the "value matched no case" path).
        for sv in &arm_svs {
            link_blocks(program, &*scope_view, sv, false)?;
        }
        if !has_default {
            link_blocks(program, &*scope_view, &continuation, false)?;
        }

        for (i, arm) in arms.iter().enumerate() {
            // Fall through to the next arm, or out of the switch on the last arm.
            let fallthrough = arm_svs
                .get(i + 1)
                .map(|sv| sv.blidx)
                .unwrap_or(continuation.blidx);
            let mut arm_sv = arm_svs[i].clone();
            arm_sv.continuation_blidx = Some(fallthrough);
            // `break` in any arm jumps to the continuation. `continue_target` is left
            // inherited so a `continue` here still targets the enclosing loop.
            arm_sv.break_target = Some(continuation.blidx);

            // Arm body = the case_statement's statement children (everything except
            // the `case` value expression).
            let value_id = arm.child_by_field_name("value").map(|v| v.id());
            let mut body_cursor = arm.walk();
            let stmts: Vec<Node<'_>> = arm
                .children(&mut body_cursor)
                .filter(|n| n.is_named() && Some(n.id()) != value_id)
                .collect();
            let cp = CompoundProxy {
                nodes: stmts,
                was_compound: false,
            };
            self.walk_compound_statement(source, program, &arm_sv, &cp)?;
        }

        *scope_view = continuation;
        Ok(())
    }

    /// `break`: terminate the current block with a goto to the innermost enclosing
    /// `switch`/loop continuation. The target rides on the scope view, so it is just
    /// `scope_view.break_target` — no stack to consult. Returns whether the statement
    /// actually diverged (see [`Self::orphan_jump`] for the no-target case).
    fn walk_break(
        &self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        node: &Node<'_>,
    ) -> Result<bool, Error> {
        match scope_view.break_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)?;
                Ok(true)
            }
            None => Ok(self.orphan_jump(source, node, "break", "a switch or loop")),
        }
    }

    /// `continue`: terminate the current block with a goto to the innermost enclosing
    /// loop's re-test/update block (`scope_view.continue_target`). Returns whether the
    /// statement actually diverged (see [`Self::orphan_jump`] for the no-target case).
    fn walk_continue(
        &self,
        source: &'a str,
        program: &mut Program,
        scope_view: &ScopeView,
        node: &Node<'_>,
    ) -> Result<bool, Error> {
        match scope_view.continue_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)?;
                Ok(true)
            }
            None => Ok(self.orphan_jump(source, node, "continue", "a loop")),
        }
    }

    /// A `break`/`continue` with no enclosing target. In valid C this cannot happen, so
    /// it means the loop was lost in the parse -- overwhelmingly because the input is not
    /// preprocessed and the loop is spelled as a macro (`list_for_each_entry(p, h, n) { ..
    /// continue; .. }` parses as a call expression followed by an unattached compound
    /// statement). Failing the whole translation unit over that would make any real-world
    /// unpreprocessed source unimportable, so warn and lower the jump as a no-op: control
    /// falls through to the next statement. That over-approximates control flow (the code
    /// after a real `continue` is not reachable in that iteration), which is the safe
    /// direction for taint -- dropping the successor block instead would silently lose
    /// flows. Returns `false` so the enclosing compound keeps lowering.
    fn orphan_jump(&self, source: &str, node: &Node<'_>, kw: &str, ctx: &str) -> bool {
        log::warn!(
            "{}: `{kw}` outside of {ctx}; treating it as a no-op \
             (is the input already preprocessed?)",
            self.node_location(node, source),
        );
        false
    }

    /// Human-readable `path:line` for `node`, mapping its offset in the combined
    /// translation unit back to the original file (and that file's own line numbering)
    /// through the [`FileMap`]. Falls back to the combined-buffer line when there is no
    /// map covering the offset, as in the in-memory `parse_c_program` used by tests.
    fn node_location(&self, node: &Node<'_>, source: &str) -> String {
        let off = node.start_byte();
        match self.file_map.locate(off) {
            Some((key, local, _)) => {
                let file_start = off - local as usize;
                let line = source[file_start..off]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1;
                format!("{}:{line}", key.path)
            }
            None => format!("<input>:{}", node.start_position().row + 1),
        }
    }

    /// `goto L`: terminate the current block with a jump to label `L`'s block (created
    /// up front by the per-function pre-scan, so forward jumps resolve too). Unlike
    /// `break`/`continue`, this does NOT end the compound — statements after a `goto`
    /// are unreachable but may contain labels, so we keep lowering them into a fresh
    /// (unlinked) block.
    fn walk_goto(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        let label_node = child
            .child_by_field_name("label")
            .expect("goto_statement always has a label");
        let label = to_str(&label_node, source);
        // A label the pre-scan never saw. In valid C the label is always in the same
        // function, so this means the function was torn apart in the parse -- typically a
        // `#if` region that opens before a `} else {` and so carries an unbalanced brace,
        // leaving the label outside the recovered body that holds the `goto`. There is
        // nothing to jump to, so lower the `goto` as a no-op: control falls through to the
        // next statement, over-approximating in the same direction as an orphaned
        // `break`/`continue` rather than dropping the rest of the body on the floor.
        let Some(&target) = self.label_blocks.get(label) else {
            log::warn!(
                "{}: `goto` to undefined label `{label}`; treating it as a no-op \
                 (is the input already preprocessed?)",
                self.node_location(&child, source),
            );
            return Ok(());
        };
        let mut to = scope_view.clone();
        to.blidx = target;
        link_blocks(program, scope_view, &to, false)?;
        // Anything after the goto is unreachable until the next label; lower it into a
        // fresh block so following labels/statements still parse.
        let dead = add_block(
            program,
            scope_view,
            &mut self.scope_tree,
            false,
            &format!("after_goto::{}", get_line_num(&child)),
        )?;
        *scope_view = dead;
        Ok(())
    }

    /// `L: <stmt>`: control falls through into label `L`'s (pre-created) block, the
    /// inner statement is lowered there, and subsequent statements continue in an
    /// after-block. The label block is also the target of any `goto L`.
    fn walk_labeled_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<bool, Error> {
        let label_node = child
            .child_by_field_name("label")
            .expect("labeled_statement always has a label");
        let label = to_str(&label_node, source);
        let label_blidx = *self
            .label_blocks
            .get(label)
            .expect("label block pre-created in collect_functions");

        // Fall through from the current block into the (pre-created) label block, then
        // make it the current block — the inner statement and any following siblings
        // continue from here, exactly as if the label weren't there.
        let mut label_sv = scope_view.clone();
        label_sv.blidx = label_blidx;
        link_blocks(program, scope_view, &label_sv, false)?;
        *scope_view = label_sv;

        // Lower the labeled statement's inner statement(s) (everything but the label),
        // threading `scope_view` so control flow continues naturally. The label
        // diverges iff its body does (e.g. `L: return x;`), which we propagate up so a
        // trailing labeled-return doesn't leave a dangling fall-through block.
        let label_id = label_node.id();
        let mut cursor = child.walk();
        let inner: Vec<Node<'_>> = child
            .children(&mut cursor)
            .filter(|n| n.is_named() && n.id() != label_id)
            .collect();
        for stmt in inner {
            if self.walk_statement(source, program, scope_view, stmt)? {
                return Ok(true);
            }
        }
        Ok(false)
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
        mut field_path: ThinVec<PathSegment>,
        scope_view: &ScopeView,
        locals: &mut Locals,
    ) -> RawPath {
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
                log::debug!("Implicit Global bourn: {}", name);
                varkind = VarKind::Global;
            }
        }

        match varkind {
            // A global `name` is a symbolic field of the globals object: `$globals.name.<fields>`.
            VarKind::Global => {
                let mut fields = ThinVec::with_capacity(field_path.len() + 1);
                fields.push(PathSegment::symbol(name.as_str()));
                fields.append(&mut field_path);
                RawPath::new(VariableRef::new_global(), fields)
            }
            VarKind::Local => RawPath::new(
                VariableRef::new_local_idx(locals.get_or_intern(&name)),
                field_path,
            ),
            VarKind::Parameter => {
                if let Some(param_idx) =
                    self.get_param_idx(scope_view.func_name.as_str(), name.as_str())
                {
                    RawPath::new(VariableRef::new_parameter(param_idx), field_path)
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
                (function_declarator
                    declarator: (parenthesized_declarator
                        (pointer_declarator declarator: (identifier) @var_name)))
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
        source: &'a str,
        scope_view: &ScopeView,
    ) -> Result<Exp, Error> {
        //debug_print_tree(node, 0, Some("FLATTEN_EXPR"), Some(50));
        let text = to_str(&node, source); //.to_string();
        match node.kind() {
            "identifier" => {
                // A bare identifier that names a known function (and is not shadowed by
                // a variable in scope) is a function *reference* used as a value -- the
                // RHS of `fp = id`, an initializer `int (*fp)(int) = id`, a call argument
                // `apply(id, x)`, or a field/array store `o.op = id` / `fps[0] = id`. Lower
                // it as a function-pointer object (as the pcode backend does) so codegen
                // emits the `call_target_assign` fact that indirect-call taint resolution
                // needs; otherwise `id` is treated as a plain global and the taint is
                // dropped (F1/F2). Direct calls are unaffected: `collect_call` resolves an
                // identifier callee via `build_access_path`, not through here.
                if self
                    .scope_tree
                    .find_variable(scope_view.sidx, text)
                    .is_none()
                    && self.functions.keys().any(|f| f.0 == text)
                {
                    Ok(Exp::ObjectRef(CallObject::FunctionPtr(text.into())))
                } else {
                    // A read of a variable. A global identifier `a` is really `$globals.a` (a
                    // field of the globals object), so this may lower to a load; a local is a
                    // bare variable (no load emitted).
                    let ap = self.build_access_path(
                        text,
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    );
                    Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
                }
            }
            "comma_expression" => {
                let ch1 = node.child_by_field_name("left").expect("always left");
                let ch2 = node.child_by_field_name("right").expect("always right");
                self.flatten_expr(program, ch1, source, scope_view)?;
                self.flatten_expr(program, ch2, source, scope_view)
            }
            "pointer_declarator" | "function_declarator" | "array_declarator" => {
                self.flatten_nested_decl(program, node, source, scope_view)
            }
            // A character literal (`'a'`, `'\n'`) is a compile-time constant, exactly like a
            // numeric literal, so lower it to an `Exp::Str` constant (carries no taint). Without
            // this arm any program containing a char literal hit `flatten_expr`'s catch-all and
            // failed ingestion (ERR 78) -- a broad gap, since char literals are everyday C.
            //
            // `true`/`false` (`stdbool.h`, C23, and the kernel's own `TRUE`/`FALSE`) and
            // `null` (`NULL`/`nullptr`) get their own node kinds in tree-sitter-c rather than
            // parsing as `number_literal`, so they need naming here too -- they are constants
            // by the same argument. `concatenated_string` ("a" "b") is likewise a literal.
            "number_literal"
            | "string_literal"
            | "char_literal"
            | "concatenated_string"
            | "true"
            | "false"
            | "null" => Ok(Exp::Str(ArcIntern::<str>::from(text))),
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
                // A field read on the RHS. Resolve the location as an lvalue -- which composes
                // this field onto any base (variable, array element, deref) and applies the
                // union-member collapse -- then lower it to loads and yield the loaded value,
                // exactly as `flatten_subscript` does for `a[i]`. Keeping the read and lvalue
                // paths on the same resolver is what lets `a[i].f` (a field of an array element)
                // work on both sides of an assignment.
                let ap = self.flatten_lvalue(program, node, source, scope_view)?;
                Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
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
            // Both dereference (`*p`) and address-of (`&x`) parse as `pointer_expression`;
            // the operator child distinguishes them. Historically CTADL passed the operand
            // straight through for both (`*p` -> `p`, `&x` -> `x`), a value-copy model that
            // is sound for reads but drops writes through a pointer (F3). For a dereference
            // whose operand is a plain variable with a known same-block address-of alias
            // (`p = &x`), resolve `*p` to the pointee `x` so a store `*p = v` becomes a real
            // write to `x` (and a load `y = *p` reads the current `x`). Everything else --
            // `&x`, a dereference of a non-aliased/compound operand -- keeps the pass-through.
            "pointer_expression" => {
                let arg = node
                    .child_by_field_name("argument")
                    .expect("always a argument for the * operator");
                let is_deref = node
                    .child_by_field_name("operator")
                    .is_some_and(|op| to_str(&op, source) == "*");
                let arg_exp = self.flatten_expr(program, arg, source, scope_view)?;
                // A plain local pointer is an `Exp::Variable`; a pathless access path also names
                // a bare pointer. Either can carry a same-block address-of alias.
                let ptr_ref = match &arg_exp {
                    Exp::Variable(v) => Some(v.clone()),
                    Exp::AccessPath(ptr_ap) if ptr_ap.path.is_empty() => {
                        Some(ptr_ap.variable_ref.clone())
                    }
                    _ => None,
                };
                if is_deref
                    && let Some(ptr_ref) = ptr_ref
                    && let Some((pointee, blk)) = self.addr_alias.get(&ptr_ref)
                    && *blk == scope_view.blidx
                {
                    return Ok(Exp::access_path(pointee.clone()));
                }
                Ok(arg_exp)
            }
            "subscript_expression" => self.flatten_subscript(program, node, source, scope_view),
            "call_expression" => {
                let x = self.allocator.next_temp();
                self.collect_call(program, node, source, scope_view, x)
            }
            // A cast is value-preserving for taint: the target type is irrelevant to
            // dataflow, so lower the cast operand and pass it straight through
            // (`(long)x` carries `x`). Mirrors the `unary_expression` pass-through.
            "cast_expression" => {
                let value = node
                    .child_by_field_name("value")
                    .expect("cast_expression always has a value");
                self.flatten_expr(program, value, source, scope_view)
            }
            // `sizeof` does NOT evaluate its operand -- it yields a compile-time size --
            // so it must not carry taint from the operand. Lower it as a constant (the
            // source text), exactly like a numeric literal; the operand is never visited.
            // `offsetof(T, m)` and `alignof(T)` are the same story: a compile-time number
            // whose "operands" are a type and a member name, neither of which is a value.
            "sizeof_expression" | "offsetof_expression" | "alignof_expression" => {
                Ok(Exp::Str(ArcIntern::<str>::from(text)))
            }
            // A ternary `c ? a : b` is path-insensitive here: either arm may be the
            // value, so blend both into a temp (like `flatten_binary`). The condition is
            // a control dependence, not a data source -- evaluate it for side effects but
            // don't blend it into the result. The exception is GNU's `c ?: b`, where the
            // consequence is omitted and the condition *is* the value of that arm; there
            // the condition's value is blended in.
            "conditional_expression" => {
                let cond = node
                    .child_by_field_name("condition")
                    .expect("conditional_expression always has a condition");
                let alt = node
                    .child_by_field_name("alternative")
                    .expect("conditional_expression always has an alternative");
                let cond_val = self.flatten_expr(program, cond, source, scope_view)?;
                let cons_val = match node.child_by_field_name("consequence") {
                    Some(cons) => self.flatten_expr(program, cons, source, scope_view)?,
                    None => cond_val,
                };
                let alt_val = self.flatten_expr(program, alt, source, scope_view)?;
                let temp_name = self.allocator.next_temp();
                let target = self.build_access_path(
                    temp_name.as_str(),
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                self.add_assign_to_program(program, scope_view, &target, &cons_val, Some(&alt_val));
                Ok(Exp::Variable(VariableRef::new_local_idx(
                    program[scope_view.fidx].locals.get_or_intern(&temp_name),
                )))
            }
            // A compound literal `(struct T){ .a = x }` is an anonymous aggregate value.
            // Materialize it as a temp and lower the brace initializer into that temp with
            // the same per-element stores a declared aggregate gets, so `f((struct T){ .a =
            // tainted })` carries `tainted` into the argument.
            "compound_literal_expression" => {
                let value = node
                    .child_by_field_name("value")
                    .expect("compound_literal_expression always has a value");
                let temp_name = self.allocator.next_temp();
                let target = self.build_access_path(
                    temp_name.as_str(),
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                self.lower_initializer_list(source, program, scope_view, &target, value)?;
                Ok(Exp::Variable(target.base.clone()))
            }
            // A `compound_statement` in expression position. In valid C that is a GNU
            // statement expression (`({ int t = f(); t; })`), whose value is its last
            // statement; on unpreprocessed source it is also what a brace block inside a
            // macro invocation's argument list looks like (`TP_fast_assign({ .. })`).
            // Either way the statements inside carry real assignments and calls, so they
            // are lowered rather than dropped -- straight-line, see
            // `flatten_statements_inline`.
            "compound_statement" => {
                self.flatten_statements_inline(program, node, source, scope_view)
            }
            // Trivia and pure type syntax that only reach an expression position on
            // unpreprocessed input -- a comment or stray `#pragma`/`#error` inside a
            // recovered region, or a macro-spelled type name (`__be32 x`, or a bare
            // `LIST_ITEMS` macro that expands to statements). None denotes a value.
            "comment"
            | "preproc_directive"
            | "macro_type_specifier"
            | "type_identifier"
            | "primitive_type"
            | "sized_type_specifier"
            | "type_descriptor" => Ok(Exp::Str(ArcIntern::<str>::from("$unparsed"))),
            // Inline assembly. Its operands do carry data flow, but the constraint syntax
            // that says which are read and which are written is not modeled here, so the
            // block is treated as opaque: it contributes no taint in either direction.
            "gnu_asm_expression" => Ok(Exp::Str(ArcIntern::<str>::from("$asm"))),
            // A tree-sitter `ERROR` node: this text did not parse at all. That is not a
            // gap in the frontend (which is what the catch-all below reports) but a gap
            // in the input -- routine on unpreprocessed source, where macros expanding to
            // declarations, attribute macros and inline assembly all defeat the C grammar.
            // Failing the translation unit over one such region would make any real
            // unpreprocessed tree unimportable, so lower whatever children *do* parse --
            // calls and assignments inside the region still contribute flows -- and yield
            // an opaque constant for the region's own value.
            "ERROR" => {
                log::warn!(
                    "{}: unparsable text `{}`; lowering it opaquely (is the input already \
                     preprocessed?)",
                    self.node_location(&node, source),
                    text.lines().next().unwrap_or_default().trim(),
                );
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> =
                    node.children(&mut cursor).filter(Node::is_named).collect();
                for c in children {
                    // A child of an unparsable region may itself be unlowerable; that is
                    // expected here, so keep going rather than failing the whole unit.
                    let _ = self.flatten_expr(program, c, source, scope_view);
                }
                Ok(Exp::Str(ArcIntern::<str>::from("$unparsed")))
            }
            _ => {
                debug_print_tree(node, 0, None, None);
                Err(Error::TreeSitterParse(format!(
                    "{}: ERR 78: Unsupported expression type: {}",
                    self.node_location(&node, source),
                    node.kind()
                )))
            }
        }
    }

    fn flatten_nested_decl(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
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
                let ap = self.build_access_path(
                    symbol,
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
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

    /// Lower the statements under `node` straight-line into the *current* block: every
    /// expression statement and declaration in source order, with control-flow structure
    /// (`if`, loops, `switch`, `#if` arms) flattened away rather than turned into blocks.
    /// Returns the value of the trailing expression statement -- the value of a GNU
    /// statement expression -- or an opaque constant if there is none.
    ///
    /// Blocks are deliberately not created. [`Self::flatten_expr`] cannot move its
    /// caller's "current block" (it only has `&ScopeView`), so a region that branched
    /// would leave its last block dangling with no terminator and no way to rejoin the
    /// enclosing expression. Going straight-line keeps every assignment inside the region
    /// visible to taint, which is the point, at the cost of path sensitivity the rest of
    /// this frontend does not have either.
    fn flatten_statements_inline(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> Result<Exp, Error> {
        /// Statement and clause nodes that only group other statements: recurse through
        /// them. Anything not listed is lowered as an expression.
        const CONTAINERS: &[&str] = &[
            "compound_statement",
            "if_statement",
            "else_clause",
            "while_statement",
            "do_statement",
            "for_statement",
            "switch_statement",
            "case_statement",
            "labeled_statement",
            "return_statement",
            "preproc_if",
            "preproc_ifdef",
            "preproc_else",
            "preproc_elif",
            "preproc_elifdef",
        ];

        let mut cursor = node.walk();
        let stmts: Vec<Node<'_>> = node.children(&mut cursor).filter(Node::is_named).collect();
        let last = stmts.len().saturating_sub(1);
        let mut value = Exp::Str(ArcIntern::<str>::from("$unparsed"));
        for (i, stmt) in stmts.into_iter().enumerate() {
            match stmt.kind() {
                "comment" => {}
                // A jump has no value and nowhere to jump to from here; `goto`'s label is
                // an identifier that must not be lowered as a variable read.
                "break_statement" | "continue_statement" | "goto_statement" => {}
                "declaration" => self.walk_declaration(source, program, scope_view, stmt)?,
                "expression_statement" => {
                    if let Some(inner) = stmt.child(0)
                        && !_is_empty(&inner)
                    {
                        let v = self.flatten_expr(program, inner, source, scope_view)?;
                        if i == last {
                            value = v;
                        }
                    }
                }
                kind if CONTAINERS.contains(&kind) => {
                    self.flatten_statements_inline(program, stmt, source, scope_view)?;
                }
                _ => {
                    let v = self.flatten_expr(program, stmt, source, scope_view)?;
                    if i == last {
                        value = v;
                    }
                }
            }
        }
        Ok(value)
    }

    fn flatten_binary(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        operator: Node<'_>,
        source: &'a str,
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
        let target = self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );

        match operator.kind() {
            "==" | "<=" | ">=" => {
                //todo: what are all of these?
                log::debug!("Not assigning for comparison operators");
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

        Ok(Exp::Variable(VariableRef::new_local_idx(
            program[scope_view.fidx].locals.get_or_intern(&temp_name),
        )))
    }

    fn flatten_update_expression(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        // The location being updated (`x`, `p->f`, `a[i]`, ...).
        let argument = node.child_by_field_name("argument").expect("missing left");
        let loc = self.flatten_lvalue(program, argument, source, scope_view)?;
        // Its current value (a field location lowers to loads).
        let cur = Exp::access_path(self.emit_loads(program, scope_view, loc.clone()));
        let one = Exp::Str(ArcIntern::<str>::from("1"));
        // temp = cur + 1
        let temp_name = self.allocator.next_temp();
        let target = self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        self.add_assign_to_program(program, scope_view, &target, &cur, Some(&one));
        // loc = temp
        let new_val = Exp::Variable(target.base.clone());
        self.add_assign_to_program(program, scope_view, &loc, &new_val, None);
        // The value of the update expression is the updated location's value.
        Ok(new_val)
    }

    fn flatten_subscript(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> std::result::Result<Exp, Error> {
        // A subscript read on the RHS: resolve the location (base path + index field) and lower
        // it to loads. (As an lvalue it is handled by `flatten_lvalue` instead.)
        let ap = self.flatten_lvalue(program, node, source, scope_view)?;
        Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
    }

    fn collect_arguments(
        &mut self,
        program: &mut Program,
        arg_list: Node<'_>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> Result<ctadl_ir::ThinVec<Exp>, Error> {
        let mut result = ctadl_ir::ThinVec::new();

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
        source: &'a str,
        scope_view: &ScopeView,
        temp_name: String,
    ) -> Result<Exp, Error> {
        let func_node = node.child_by_field_name("function").expect("always has");
        let func_name = to_str(&func_node, source);

        let call_edges = CallEdges::Explicit(ctadl_ir::thin_vec![func_name.to_string()]);

        let arg_node = node.child_by_field_name("arguments").expect("always has");
        let args = self.collect_arguments(program, arg_node, source, scope_view)?;

        // Resolve the callee. A plain `foo(...)` is an identifier; the legacy
        // dereference form `(*op_func)(...)` wraps the pointer in a
        // parenthesized/pointer expression, so route it through flatten_expr to
        // recover the underlying variable (`op_func`).
        let access_path = if func_node.kind() == "identifier" {
            self.build_access_path(
                func_name,
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            )
        } else {
            // The callee is a call-target location (e.g. `(*op_func)(...)`); resolve it as an
            // lvalue so its access path is preserved rather than lowered into a load.
            self.flatten_lvalue(program, func_node, source, scope_view)
                .unwrap_or_else(|_| {
                    self.build_access_path(
                        func_name,
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    )
                })
        };

        let var = access_path.base.variable.clone();
        let style = match &*var {
            Variable::Local(name) => {
                log::debug!(
                    "This is an Indirect LOCAL call: {}",
                    program[scope_view.fidx].locals.name(*name)
                );
                // The callee is a call-target address. Any symbolic field (e.g. `%o.f`, a
                // function pointer stored in a field) is lowered to a load, leaving an
                // offset-only callee address.
                let callee = self.emit_loads(program, scope_view, access_path);
                CallStyle::FuncPtrCall {
                    callee,
                    signature: (Some("indirect-call".to_string())),
                }
            }
            Variable::Param(idx) => {
                log::debug!("This is an Indirect PARAMETER call: {}", idx.get());
                let callee = self.emit_loads(program, scope_view, access_path);
                CallStyle::FuncPtrCall {
                    callee,
                    signature: (Some("indirect-call".to_string())),
                }
            }
            Variable::GlobalHeap => CallStyle::DirectCall { call_edges },
        };

        let ret =
            VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp_name));
        program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new(
            StatementKind::CallAssign {
                style,
                rets: vec![ret].into(),
                args,
            },
            self.cur_span,
        ));
        //we return the temp_name, so that the assignment expression for the actual int x = foo() gets the result of foo()
        Ok(Exp::Variable(
            self.build_access_path(
                temp_name.as_str(),
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            )
            .base,
        ))
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

        // Pre-pass: register every function name up front so a function-pointer
        // reference to a function defined LATER in the file (`fp = later;`) is already
        // known when its using function is lowered. Without this, `flatten_expr` would
        // not recognise `later` as a function and would drop the indirect-call taint.
        let mut name_cursor = QueryCursor::new();
        let mut name_matches = name_cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = name_matches.next() {
            let extract = MatchExtractor::new(&query, m);
            if let Ok(name_node) = extract.get("func.name") {
                let func_name = to_str(&name_node, source);
                self.functions
                    .entry(FunctionName(func_name))
                    .or_insert_with(|| program.new_function());
            }
        }

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
                break_target: None,
                continue_target: None,
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
                break_target: None,
                continue_target: None,
                explainer: "initial_block".to_string(),
            };
            self.scope_tree.blocks.push(block_scope_view.clone());
            let cp = CompoundProxy::from_node(body_node);

            // Pre-create a block for every `goto` label in this function so forward
            // jumps (a `goto L` appearing before `L:`) resolve. Reset per function.
            self.label_blocks.clear();
            // Address-of aliases are function-local and confined to a straight-line block.
            self.addr_alias.clear();
            // Union-typed locals are function-scoped.
            self.union_vars.clear();
            let mut labels = Vec::new();
            collect_labels(body_node, source, &mut labels);
            for label in labels {
                let label_block = add_block(
                    program,
                    &block_scope_view,
                    &mut self.scope_tree,
                    false,
                    &format!("label:{label}"),
                )?;
                self.label_blocks.insert(label, label_block.blidx);
            }

            self.walk_compound_statement(source, program, &block_scope_view, &cp)?;

            // Every block must end with a terminator -- `BasicBlockData::terminator` and
            // the CFG traversals built on it panic without one. A pre-created label block
            // that the walk never reached is the one way to still be missing it here: the
            // label sits in a region the parse tore away from this function (unbalanced
            // braces inside a `#if`), so nothing lowered into it and nothing jumps to it.
            // Close those with an empty return; they are unreachable, so it changes nothing.
            for block in program.functions[fidx].blocks.blocks_mut().iter_mut() {
                if block.terminator.is_none() {
                    block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
                        args: vec![].into(),
                    }));
                }
            }
        }
        Ok(())
    }

    //this is a helper function to take the SSA list and shove them all into the block
    fn add_assign_to_program(
        &mut self,
        program: &mut Program,
        scope_view: &ScopeView,
        target: &RawPath,
        left_op: &Exp,
        right_op: Option<&Exp>,
    ) {
        let val_exp = left_op; //todo get rid of val_exp and just use left_op
        if target.is_pathless() {
            let mut fa: Vec<Exp> = [val_exp.clone()].into();
            if let Some(righty) = right_op {
                fa.push(righty.clone());
            }
            program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new(
                StatementKind::assign(target.base.clone(), fa),
                self.cur_span,
            ));
        } else {
            // A store writes a single value into the field path; the field read is not
            // expressible as an operand, so a compound op's second operand is dropped here
            // (matching the prior behavior for field stores). Any intermediate dereferences are
            // materialized as loads by `store_access_path`.
            let mut stmts = Vec::new();
            let allocator = &mut self.allocator;
            let locals = &mut program[scope_view.fidx].locals;
            ctadl_ir::mir::store_access_path(
                target.base.clone(),
                target.fields.iter().cloned(),
                val_exp.clone(),
                &mut stmts,
                || VariableRef::new_local_idx(locals.get_or_intern(&allocator.next_temp())),
            );
            for mut s in stmts {
                s.source_info = self.cur_span;
                program[scope_view.fidx].blocks[scope_view.blidx].push_back(s);
            }
        }
    }

    /// Resolves an assignable location to its access path WITHOUT emitting loads. Used for the
    /// left-hand side of assignments and for the base of subscripts, where the field path must be
    /// preserved so a store (or a composed subscript) can target it.
    fn flatten_lvalue(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> Result<RawPath, Error> {
        match node.kind() {
            "identifier" => Ok(self.build_access_path(
                to_str(&node, source),
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            )),
            "field_expression" => {
                // Resolve the object being accessed as an lvalue *first*, then append this
                // field. Recursing through `flatten_lvalue` (rather than walking an
                // identifier-rooted chain of `field_expression`s) lets a field be composed on
                // top of ANY location -- a plain variable (`s.f`), an array element
                // (`a[i].f`), a pointer deref (`p->f` / `(*p).f`) -- so the base's own path
                // (e.g. the `[i]` index segment a subscript contributes) is preserved and the
                // field is layered onto it.
                let argument = node
                    .child_by_field_name("argument")
                    .expect("field_expression always has an argument");
                let field = node
                    .child_by_field_name("field")
                    .expect("field_expression always has a field");
                let mut base = self.flatten_lvalue(program, argument, source, scope_view)?;
                // Collapse a union member access to the shared `$union` field so a write to one
                // member is observed at a read of another (union members alias; F4). Only the
                // access *on the union variable itself* collapses -- detected by the resolved
                // base being the bare union variable -- so a struct field nested inside a union
                // member (`u.a.b`) keeps its own name. This matches the prior behavior of
                // rewriting only the first path segment.
                let seg = if base.is_pathless() && self.union_vars.contains(&base.base) {
                    PathSegment::symbol(UNION_FIELD)
                } else {
                    PathSegment::symbol(to_str(&field, source))
                };
                base.fields.push(seg);
                Ok(base)
            }
            "subscript_expression" => {
                let base = self.flatten_lvalue(
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
                let s = if let Exp::Str(esp) = &index {
                    format!("[{}]", esp)
                } else {
                    "[_elem_]".to_string()
                };
                let mut ap = base;
                ap.fields.push(PathSegment::symbol(s));
                Ok(ap)
            }
            "parenthesized_expression" | "parenthesized_declarator" => {
                let inner = node.child(1).expect("missing inner expr");
                self.flatten_lvalue(program, inner, source, scope_view)
            }
            "pointer_expression" => {
                let arg = node
                    .child_by_field_name("argument")
                    .expect("always a argument");
                let is_deref = node
                    .child_by_field_name("operator")
                    .is_some_and(|op| to_str(&op, source) == "*");
                let ptr = self.flatten_lvalue(program, arg, source, scope_view)?;
                // A store through `*p` where `p` has a known same-block address-of alias
                // (`p = &x`) targets the pointee `x` directly (F3), so the write is observed at
                // reads of `x`. Mirrors the read path in `flatten_expr`.
                if is_deref
                    && ptr.is_pathless()
                    && let Some((pointee, blk)) = self.addr_alias.get(&ptr.base)
                    && *blk == scope_view.blidx
                {
                    let fields = pointee
                        .path
                        .fields
                        .iter()
                        .cloned()
                        .map(PathSegment::from)
                        .collect();
                    return Ok(RawPath::new(pointee.variable_ref.clone(), fields));
                }
                Ok(ptr)
            }
            _ => match self.flatten_expr(program, node, source, scope_view)? {
                Exp::Variable(v) => Ok(RawPath::new(v, ThinVec::new())),
                // Not a location this frontend can name. In valid C that means the store
                // target is an expression we do not model; on unpreprocessed source it is
                // usually recovery debris, where the "assignment" is not one at all.
                // Point the store at a fresh temp so the right-hand side is still
                // evaluated (and its calls still lowered) instead of failing the whole
                // translation unit -- the write itself is dropped, so flows *through* this
                // target are lost, which is why it is reported.
                _ => {
                    log::warn!(
                        "{}: not an lvalue: {} (`{}`); storing to a temp instead, so any \
                         flow through this target is dropped",
                        self.node_location(&node, source),
                        node.kind(),
                        to_str(&node, source).lines().next().unwrap_or_default(),
                    );
                    let temp_name = self.allocator.next_temp();
                    Ok(self.build_access_path(
                        temp_name.as_str(),
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    ))
                }
            },
        }
    }

    /// Lowers the symbolic-field reads of `ap` into a sequence of loads (see
    /// [`mir::load_access_path`]) appended to the current block, returning the residual *address*
    /// (base variable plus any trailing offsets) as an offset-only access path. A pathless or
    /// offset-only `ap` is returned unchanged, emitting nothing.
    fn emit_loads(
        &mut self,
        program: &mut Program,
        scope_view: &ScopeView,
        ap: RawPath,
    ) -> AccessPath {
        let mut stmts = Vec::new();
        let allocator = &mut self.allocator;
        let locals = &mut program[scope_view.fidx].locals;
        let v = ctadl_ir::mir::load_access_path(ap.base, ap.fields, &mut stmts, || {
            VariableRef::new_local_idx(locals.get_or_intern(&allocator.next_temp()))
        });
        for mut s in stmts {
            s.source_info = self.cur_span;
            program[scope_view.fidx].blocks[scope_view.blidx].push_back(s);
        }
        v
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
    log::debug!("{}|-- {}{}", indent, field_prefix, node.kind());

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
