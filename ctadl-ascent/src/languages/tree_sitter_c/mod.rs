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
//! A subscript is lowered as the pointer arithmetic it is: `a[3]` is the address `a.[3]` plus
//! the dereference performed there, `deref` (see [`DEREF_FIELD`]). A non-constant index has no
//! offset to name, so `a[n]` becomes a bare dereference of the base address -- literally the
//! path `a[0]` produces. A write through `a[n]` is therefore observed at a read of `a[0]`, but
//! *not* at a nonzero constant index: `a[n] = v` does not reach `a[2]`. That is the remaining
//! half of the F5 gap. Closing it means over-approximating the index here -- for instance,
//! lowering *every* subscript on a base the function also indexes non-constantly to the bare
//! dereference -- and not asking the path matcher to alias two spellings, which it does not do.
//!
//! ## Addresses of struct members
//!
//! [`Context::flatten_address_of`] can form the address of an array element (`&a[1]`), because
//! an address in the IR is a base variable plus numeric offsets. A member address (`&s.f`) has
//! no such spelling -- it would need `f`'s byte offset, which this frontend, having no type
//! information, cannot compute -- so `&s.f` falls back to the value-copy model and a callee's
//! write through that pointer is dropped.
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
//! ## `asm goto`
//!
//! GNU inline assembly is lowered as an operand transfer ([`Context::flatten_gnu_asm`]): every
//! input operand may reach every output operand, and a `"+"` operand keeps its identity flow.
//! `asm goto` additionally *jumps* to one of its labels, and those are real CFG edges out of an
//! expression -- `flatten_expr` yields a value and has no way to add successors -- so the jumps
//! are not modeled and the construct keeps reporting a frontend gap. Its operands still lower.
//! Pinned by the `#[ignore]`d `asm_goto_is_a_known_limitation`. No corpus (dropbear, OpenSSH,
//! nginx) uses it.
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
                // The block already ends in a `return`, so it has no fall-through
                // and this continuation edge is redundant: a block cannot both
                // return and goto. This arises from over-eager continuation
                // wiring around an if/else chain whose arms all diverge (e.g.
                // dropbear's `svr_dropbear_exit`). Keep the `return` and drop the
                // spurious edge rather than aborting the whole import.
                TerminatorKind::Return { .. } => recoverable_report(
                    "frontend gap",
                    format!(
                        "continuation edge into a block that already returns, dropped: \
                             {:?} -> {:?}",
                        from_sv.blidx, target_val
                    ),
                ),
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

/// The basic-block contract is a sequence of statements ending in a terminator,
/// and every function graph must satisfy it by the time it leaves the frontend.
/// The walk can leave a pre-created block unterminated when a diverging
/// statement cut the compound short before the block was reached: a `goto`
/// label after a `return` (`walk_compound_statement` stops at the `return`, so
/// `walk_labeled_statement` never runs for the label), a recovered/skipped
/// subtree that contained a label, or a duplicate label name (the first
/// pre-created block is orphaned). Give every such block the same implicit
/// empty `return` that falling off the end of the body gets (see
/// `link_blocks`), then report once per function: an unterminated block means
/// its statements were dropped, a frontend gap worth surfacing under
/// CTADL_ERROR_ON_AST. Same pattern as the Lua frontend's
/// `finalize_terminators`.
fn finalize_terminators(
    program: &mut Program,
    fidx: FunctionIdx,
    func_name: &str,
) -> Result<(), Error> {
    let mut patched: Vec<BasicBlockIdx> = Vec::new();
    for (bb, data) in program.functions[fidx]
        .blocks
        .blocks_mut()
        .iter_enumerated_mut()
    {
        if data.terminator.is_none() {
            data.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
                args: vec![].into(),
            }));
            patched.push(bb);
        }
    }
    if !patched.is_empty() {
        unexpected_ast(format!(
            "function `{func_name}`: {} block(s) left without a terminator by the walk \
             ({patched:?}); their statements (e.g. code under a label after a `return`) \
             were dropped. Gave them an implicit empty `return`.",
            patched.len(),
        ))?;
    }
    Ok(())
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
        break_target: scope_view.break_target,
        continue_target: scope_view.continue_target,
        explainer: format!("{}.{}", scope_view.blidx.get(), debug_explainer),
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
struct FunctionName<'a>(&'a str);

/// Synthetic field name that all members of a `union` variable collapse to, so they share a
/// single access path (union members alias -- they occupy the same storage). The `$` keeps it
/// out of the C identifier space, so it can never collide with a real source-level field.
const UNION_FIELD: &str = "$union";

/// Field name for the *memory* an address names -- the dereference itself. An `Offset(N)` in a
/// path is pointer arithmetic and nothing else: it moves an address, it never reads. Every
/// access that actually touches memory ends in this symbol, so `a[3]`, which is `*(a + 3)`,
/// lowers to two segments: `Offset(3)` on the address, then `deref` for the read or write
/// performed there -- the path `a.[3].deref`.
///
/// Splitting the two is what makes element addresses composable. Offsets are summed when paths
/// meet (`facts::Path::from_accesses`), so an address formed by `&a[1]` (`a.[1]`) that a callee
/// writes at `.[1].deref` lands on `a.[2].deref` -- the same path a caller's `a[2]` reads. A
/// single symbolic `[N]` field, which is what this frontend used to emit, cannot compose that
/// way: no arithmetic relates `Symbol("[1]")` to `Symbol("[2]")`. The name and the
/// `base.[off].deref` shape are the pcode frontend's, so the two C frontends spell a memory
/// access the same way. (The dex and jvm frontends spell their element field `[]`; those are
/// typed array loads, not pointer arithmetic, so they have no offset to keep separate.)
///
/// This is the *only* dereference field the frontend emits: a non-constant index lowers to it
/// with no offset, so `a[n]` and `a[0]` are one path. See [`push_element`].
const DEREF_FIELD: &str = "deref";

/// True for the synthetic dereference field ([`DEREF_FIELD`]) -- the memory at an address, as
/// opposed to a struct member. Taking the address of such an access (`&a[i]`) drops this field,
/// leaving the address itself.
fn is_deref_field(seg: &PathSegment) -> bool {
    matches!(seg, PathSegment::Symbol(s) if &**s == DEREF_FIELD)
}

/// The compile-time value of a lowered subscript index, if it has one. Only an integer literal
/// counts: [`Context::flatten_expr`] lowers every constant to its source text (`Exp::Str`), so
/// this is where `a[0x10]`, `a[3u]` and `a[3]` become the same offset and `a['c']`, `a["s"]`,
/// `a[n]` become none. C integer suffixes (`u`, `l`, and their combinations) are dropped; a
/// negative index is a real (if unusual) offset and is kept.
fn constant_index(exp: &Exp) -> Option<i64> {
    let Exp::Str(text) = exp else {
        return None;
    };
    let text = text.trim();
    let digits = text.trim_end_matches(['u', 'U', 'l', 'L']);
    let (radix, digits) = match digits.strip_prefix(['-', '+']) {
        // Sign is re-attached below by parsing the whole (suffix-stripped) text in base 10.
        Some(_) => (10, digits),
        None => match digits.get(..2) {
            Some("0x") | Some("0X") => (16, &digits[2..]),
            Some("0b") | Some("0B") => (2, &digits[2..]),
            _ => (10, digits),
        },
    };
    i64::from_str_radix(digits, radix).ok()
}

/// The location a dereference of `pointee` names, for a pointer bound by address-of
/// ([`Context::addr_alias`]). An interior element address (`p = &x[1]` binds the address
/// `x.[1]`) is dereferenced by reading the memory there, so `*p` is `x.[1].deref`. A pointee
/// that is a bare variable (`p = &x`) has no address path: CTADL models it as the variable
/// itself (the value-copy model), so there is nothing to add and this returns `None`.
fn deref_of_pointee(pointee: &AccessPath) -> Option<RawPath> {
    if pointee.path.is_empty() {
        return None;
    }
    let mut fields: ThinVec<PathSegment> = pointee
        .path
        .fields
        .iter()
        .cloned()
        .map(PathSegment::from)
        .collect();
    fields.push(PathSegment::symbol(DEREF_FIELD));
    Some(RawPath::new(pointee.variable_ref.clone(), fields))
}

/// Appends the path segments a subscript contributes: a pointer-arithmetic offset for a constant
/// index (elided when it is zero -- `a[0]` *is* `*a`, and the analysis drops `Offset(0)` anyway)
/// followed by the dereference the subscript performs. See [`DEREF_FIELD`].
///
/// An index that is not a compile-time constant has no offset to name, so it lowers to the bare
/// dereference -- the same path `a[0]` produces. That is deliberate: the two *are* the same path,
/// so a write through `a[n]` is observed at a read of `a[0]` and vice versa, with no help from the
/// path matcher, which treats every symbol as an opaque name. Approximating the unknown index by
/// the base address is the frontend's own choice of over-approximation; see the module-level
/// "Non constant subscript indices" note for what it still misses.
fn push_element(fields: &mut ThinVec<PathSegment>, index: Option<i64>) {
    match index {
        Some(n) if n != 0 => {
            fields.push(PathSegment::offset(n));
            fields.push(PathSegment::symbol(DEREF_FIELD));
        }
        Some(_) | None => fields.push(PathSegment::symbol(DEREF_FIELD)),
    }
}

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
    /// **Record layout registry**: a record tag mapped to its data members in declaration
    /// order. Filled once per translation unit by [`Context::collect_struct_layouts`], before
    /// any function is lowered, so a member's own type is available regardless of declaration
    /// order. Consulted only by [`Context::collect_initializer_list`], to map a *positional*
    /// brace initializer onto the members it writes. A tag that is absent (anonymous, declared
    /// in another translation unit) simply takes the positional-element fallback, so an
    /// incomplete registry is always safe.
    struct_layouts: HashMap<String, Vec<MemberSlot>>,
}

/// One data member in a [`Context::struct_layouts`] entry.
#[derive(Debug, Clone)]
struct MemberSlot {
    /// The member's name -- the `Symbol` a `.name` access lowers to.
    name: String,
    /// The record tag of this member's *own* type, when it has one: `struct Q q;` records
    /// `Q`, so a brace nested at this member's position can recurse with `Q`'s layout.
    /// `None` for scalars and, deliberately, for **pointer** and **array** members: those are
    /// not inline records, and treating a brace at their position as one would write a wrong
    /// path rather than fix one. Resolved against the registry lazily at use, so a member
    /// whose type is defined later in the file still works.
    type_tag: Option<String>,
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

/// The record tag a declaration's *type* names, if it names one: `struct P p = ...` yields
/// `P`, a `typedef`'d record (`P p = ...`) yields the typedef name, and a plain `int` yields
/// `None`. The tag is looked up in [`Context::struct_layouts`], which records both spellings,
/// so both resolve to the same layout.
fn declaration_type_tag<'s>(decl_node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let ty = decl_node.child_by_field_name("type")?;
    match ty.kind() {
        // `struct P x;` / `union U u;` / (C++) `class C c;`
        "struct_specifier" | "union_specifier" | "class_specifier" => {
            ty.child_by_field_name("name").map(|n| to_str(&n, source))
        }
        // `P x;` where `P` is a typedef of a record.
        "type_identifier" => Some(to_str(&ty, source)),
        _ => None,
    }
}

/// How many array dimensions a declarator declares: `a[2]` is 1, `m[2][2]` is 2, a plain
/// identifier 0. The declared type describes the innermost element, so this is how many brace
/// levels an initializer must descend before that type's layout applies. Descends through
/// parenthesized and pointer declarators, which do not add a dimension.
///
/// The `abstract_*` spellings are the same declarators written without a name, which is how a
/// `type_descriptor` spells them: `(int[]){ .. }`'s type carries an `abstract_array_declarator`
/// and must count as rank 1 exactly like `int a[]` does.
fn array_declarator_rank(decl: Node<'_>) -> usize {
    match decl.kind() {
        "array_declarator" | "abstract_array_declarator" => {
            1 + decl
                .child_by_field_name("declarator")
                .map(array_declarator_rank)
                .unwrap_or(0)
        }
        "parenthesized_declarator" | "abstract_parenthesized_declarator" => {
            decl.named_child(0).map(array_declarator_rank).unwrap_or(0)
        }
        "pointer_declarator" | "abstract_pointer_declarator" | "init_declarator" => decl
            .child_by_field_name("declarator")
            .map(array_declarator_rank)
            .unwrap_or(0),
        _ => 0,
    }
}

/// The data members of a record definition, in declaration order, each with the tag of its own
/// record type when it has one. `None` for a node that is not a record definition, or for a
/// body this cannot read *completely* -- a partial layout would silently mis-map every element
/// after the gap, which is worse than no layout, so it is dropped and the caller falls back to
/// element numbering.
///
/// Not counted as members, and not treated as gaps: a C++ **method** declaration (also a
/// `field_declaration`, but its `function_declarator` names the method directly) and a
/// **`static`** data member (class-scoped storage, not part of the object). A function-pointer
/// *member* (`int (*a)(int);`, likewise a `function_declarator`, but wrapping a parenthesized
/// pointer) **is** a member.
fn record_member_slots(node: Node<'_>, source: &str) -> Option<Vec<MemberSlot>> {
    if !matches!(
        node.kind(),
        "struct_specifier" | "union_specifier" | "class_specifier"
    ) {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    let mut members = Vec::new();
    let mut cursor = body.walk();
    for field_decl in body.children(&mut cursor) {
        if field_decl.kind() != "field_declaration" {
            continue;
        }
        // `static int total;` -- one class-scoped global, not a per-object slot.
        let mut scursor = field_decl.walk();
        if field_decl
            .children(&mut scursor)
            .any(|ch| ch.kind() == "storage_class_specifier" && to_str(&ch, source) == "static")
        {
            continue;
        }
        // The member's own record type, if its type names one. Only a **bare identifier**
        // declarator keeps it: a pointer (`struct Q *q;`) or array (`struct Q qs[2];`) member
        // is not an inline record, and recursing into a brace at its position with `Q`'s
        // layout would write a wrong path. A self-referential record is excluded for free,
        // since the recursive member is always a pointer.
        let ty_tag = field_decl
            .child_by_field_name("type")
            .and_then(|ty| match ty.kind() {
                "struct_specifier" | "union_specifier" | "class_specifier" => {
                    ty.child_by_field_name("name").map(|n| to_str(&n, source))
                }
                "type_identifier" => Some(to_str(&ty, source)),
                _ => None,
            });
        let mut dcursor = field_decl.walk();
        for declarator in field_decl.children_by_field_name("declarator", &mut dcursor) {
            // `void set(int);` -- a method, not storage.
            if declarator.kind() == "function_declarator"
                && declarator
                    .child_by_field_name("declarator")
                    .is_some_and(|d| d.kind() == "field_identifier")
            {
                continue;
            }
            match declarator_member_name(declarator, source) {
                Some((name, is_plain)) => members.push(MemberSlot {
                    name: name.to_string(),
                    type_tag: if is_plain {
                        ty_tag.map(str::to_string)
                    } else {
                        None
                    },
                }),
                // A shape whose slot cannot be named: the layout is incomplete, so drop it.
                None => return None,
            }
        }
    }
    Some(members)
}

/// The name a member declarator introduces, plus whether it is a **plain** one (a bare
/// identifier, so the member is stored inline and its declared type is its real type) as
/// opposed to a pointer/array/function-pointer wrapper. `None` for a shape that names no
/// single member.
fn declarator_member_name<'s>(decl: Node<'_>, source: &'s str) -> Option<(&'s str, bool)> {
    match decl.kind() {
        "field_identifier" => Some((to_str(&decl, source), true)),
        "pointer_declarator" | "array_declarator" => decl
            .child_by_field_name("declarator")
            .and_then(|d| declarator_member_name(d, source))
            .map(|(n, _)| (n, false)),
        "parenthesized_declarator" => decl
            .named_child(0)
            .and_then(|d| declarator_member_name(d, source))
            .map(|(n, _)| (n, false)),
        // `int (*a)(int);` -- a function-pointer member.
        "function_declarator" => decl
            .child_by_field_name("declarator")
            .and_then(|d| declarator_member_name(d, source))
            .map(|(n, _)| (n, false)),
        _ => None,
    }
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
        scope_view: &mut ScopeView,
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

    /// Fill the [`Context::struct_layouts`] registry: for every record definition in the
    /// translation unit, its data members in declaration order, keyed by the tag a declaration
    /// can name it with. Two spellings are recorded:
    /// - a **tagged** definition (`struct P { ... };`) under its tag;
    /// - a **typedef** of a definition (`typedef struct { ... } P;`) under the typedef name,
    ///   which is how an otherwise-anonymous record becomes nameable.
    ///
    /// A recursive node walk rather than a tree-sitter query, because the record kinds differ
    /// per grammar (`class_specifier` exists only in C++, and a query naming it would not
    /// compile against the C grammar). Matching on `kind()` is neutral by construction: a kind
    /// a grammar does not have simply never occurs. A layout that could not be read completely
    /// is **not** recorded (see [`record_member_slots`]), so positional mapping is only ever
    /// attempted where every slot is known.
    fn collect_struct_layouts(&mut self, source: &'a str, node: Node<'_>) {
        if let Some(slots) = record_member_slots(node, source) {
            // `struct P { ... }` -- nameable by its own tag.
            if let Some(name) = node.child_by_field_name("name") {
                self.struct_layouts
                    .insert(to_str(&name, source).to_string(), slots.clone());
            }
            // `typedef struct { ... } P;` -- nameable by the typedef's name. The record is the
            // `type` of the enclosing `type_definition`, whose declarator is that name.
            if let Some(parent) = node.parent()
                && parent.kind() == "type_definition"
            {
                let mut pcursor = parent.walk();
                for declarator in parent.children_by_field_name("declarator", &mut pcursor) {
                    if declarator.kind() == "type_identifier" {
                        self.struct_layouts
                            .insert(to_str(&declarator, source).to_string(), slots.clone());
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.collect_struct_layouts(source, child);
        }
    }

    /// Lower an aggregate brace initializer (`int a[2] = { s, 0 }`,
    /// `struct P p = { s, 0 }`) into per-element stores. `decl_node` is the whole declaration
    /// (it carries the type, and so the record tag); `decl_ident` is the declarator being
    /// initialized (an `array_declarator` for arrays, an `identifier` for structs / scalars),
    /// whose flattening yields -- and registers -- the base access path.
    ///
    /// A *record*'s positional elements map onto the members those positions name, from the
    /// [`Context::struct_layouts`] registry: `struct P p = { s, 0 }` writes `p.x` and `p.y`,
    /// which is what a later `p.x` read resolves to. Numbering them as array elements instead
    /// would write `p.deref` and silently drop the taint, since a write at one path is not
    /// observed at a read of another. An *array*'s elements keep the element numbering, and
    /// carry the element type's layout down so an array **of** records maps its members too.
    fn collect_initializer_list(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        decl_node: Node<'_>,
        decl_ident: Node<'_>,
        init_list: Node<'_>,
    ) -> Result<(), Error> {
        // The declaration's own record layout, if its type names a record we know.
        let own = declaration_type_tag(decl_node, source)
            .and_then(|tag| self.struct_layouts.get(tag).cloned());
        // An array declarator's *rank*: `struct Q qs[2]` is 1, `int m[2][2]` is 2, a
        // non-array 0. The declared type describes the innermost element, so its layout
        // applies only once that many brace levels have been entered.
        let rank = array_declarator_rank(decl_ident);

        let base_ap = self.flatten_lvalue(program, decl_ident, source, scope_view)?;

        self.lower_braced_value(source, program, scope_view, &base_ap, init_list, own, rank)
    }

    /// Store a brace-enclosed value into `base_ap`, given the record layout its type names
    /// (`own`) and that type's array `rank`. Shared by the two places a brace can appear: a
    /// declaration's initializer ([`Context::collect_initializer_list`]) and a C99 compound
    /// literal in expression position (`(T){ ... }`, lowered in [`Context::flatten_expr`]).
    /// The two differ only in where the base path comes from -- a declarator versus a fresh
    /// temp -- so everything downstream of that is here.
    fn lower_braced_value(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        base_ap: &RawPath,
        init_list: Node<'_>,
        own: Option<Vec<MemberSlot>>,
        rank: usize,
    ) -> Result<(), Error> {
        // A braced **scalar** (`int x = { v };`) is not an aggregate: it has no elements to
        // place and its single value initializes the variable itself, exactly as `int x = v;`
        // does. Numbering it as element 0 would write the synthetic `deref` field of a scalar.
        if rank == 0 && own.is_none() {
            let mut cursor = init_list.walk();
            let elems: Vec<Node<'_>> = init_list
                .children(&mut cursor)
                .filter(|c| c.is_named())
                .collect();
            if let [elem] = elems[..]
                && !matches!(elem.kind(), "initializer_list" | "initializer_pair")
            {
                let rhs = self.flatten_expr(program, elem, source, scope_view)?;
                self.add_assign_to_program(program, scope_view, base_ap, &rhs, None);
                return Ok(());
            }
        }

        // At rank 0 the declared type *is* this level's layout; at rank N this level is an
        // array and the layout belongs N levels down.
        let (members, elem_layout, depth) = if rank == 0 {
            (own, None, 0)
        } else {
            (None, own, rank)
        };
        self.lower_initializer_list(
            source,
            program,
            scope_view,
            base_ap,
            init_list,
            members.as_deref(),
            elem_layout.as_deref(),
            depth,
        )
    }

    /// Walk the elements of an `initializer_list`, storing each into the sub-path of `base_ap`
    /// that its position names.
    ///
    /// With a `members` layout this level is a **record**: element *i* writes the member named
    /// at position *i*, the same `Symbol` a `.name` read resolves to. Without one it is an
    /// **array**: element *i* gets the offset + `deref` shape a constant-index subscript read
    /// (`a[i]`) resolves to (see `push_element` and `flatten_subscript`), so taint deposited
    /// here is observed at the read.
    ///
    /// Nested braces recurse with the layout of whatever the inner level *is*: a record
    /// member's own record type (from its [`MemberSlot::type_tag`]), or -- once `depth` array
    /// levels have been entered -- the array's element layout in `elem_layout`. Anything not
    /// resolvable falls back to element numbering, which is the pre-existing behavior.
    #[allow(clippy::too_many_arguments)]
    fn lower_initializer_list(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        base_ap: &RawPath,
        init_list: Node<'_>,
        members: Option<&[MemberSlot]>,
        elem_layout: Option<&[MemberSlot]>,
        depth: usize,
    ) -> Result<(), Error> {
        let mut cursor = init_list.walk();
        let mut idx = 0usize;
        for elem in init_list.children(&mut cursor) {
            if !elem.is_named() {
                continue; // skip the `{`, `,`, `}` tokens
            }
            // Pick this element's target sub-path, its value node, and the layout to lower a
            // nested brace with: a member's own record type, or the array element layout once
            // `depth` array levels have been entered.
            let (fields, value_node, nested) = if elem.kind() == "initializer_pair" {
                // Designated: `.member = e` or `[n] = e`.
                let designator = elem
                    .child_by_field_name("designator")
                    .expect("initializer_pair always has a designator");
                let value = elem
                    .child_by_field_name("value")
                    .expect("initializer_pair always has a value");
                let dtext = to_str(&designator, source);
                let mut fields = ThinVec::new();
                if let Some(member) = dtext.strip_prefix('.') {
                    // `.a` -> Symbol("a"), matching how a `.a` field read is lowered.
                    let member = member.trim();
                    fields.push(PathSegment::symbol(member));
                    let nested = self.member_layout(members, |m| m.name == member);
                    (fields, value, nested)
                } else {
                    // `[n]` array designator -> the same offset + dereference a subscript read
                    // of that index resolves to.
                    let index = dtext.trim().trim_start_matches('[').trim_end_matches(']');
                    push_element(
                        &mut fields,
                        constant_index(&Exp::Str(ArcIntern::<str>::from(index))),
                    );
                    let nested = self.elem_layout_at(elem_layout, depth);
                    (fields, value, nested)
                }
            } else {
                let mut fields = ThinVec::new();
                let nested = match members.and_then(|m| m.get(idx)) {
                    // Positional element of a record whose layout is known -> the member that
                    // position names.
                    Some(slot) => {
                        fields.push(PathSegment::symbol(slot.name.as_str()));
                        slot.type_tag
                            .as_deref()
                            .and_then(|tag| self.struct_layouts.get(tag).cloned())
                    }
                    // Array element, or a record with more elements than its layout describes
                    // -> successive indices.
                    None => {
                        push_element(&mut fields, Some(idx as i64));
                        self.elem_layout_at(elem_layout, depth)
                    }
                };
                idx += 1;
                (fields, elem, nested)
            };
            let mut elem_ap = base_ap.clone();
            elem_ap.fields.extend(fields);
            if value_node.kind() == "initializer_list" {
                // One array level consumed; the element layout applies when `depth` reaches 0.
                self.lower_initializer_list(
                    source,
                    program,
                    scope_view,
                    &elem_ap,
                    value_node,
                    nested.as_deref(),
                    elem_layout,
                    depth.saturating_sub(1),
                )?;
            } else {
                let rhs = self.flatten_expr(program, value_node, source, scope_view)?;
                self.add_assign_to_program(program, scope_view, &elem_ap, &rhs, None);
            }
        }
        Ok(())
    }

    /// The layout of the record type of the member `members` holds matching `pred`, if that
    /// member names a record we know. Used to recurse into a brace at a designated member.
    fn member_layout(
        &self,
        members: Option<&[MemberSlot]>,
        pred: impl Fn(&MemberSlot) -> bool,
    ) -> Option<Vec<MemberSlot>> {
        members?
            .iter()
            .find(|m| pred(m))
            .and_then(|m| m.type_tag.as_deref())
            .and_then(|tag| self.struct_layouts.get(tag).cloned())
    }

    /// An array's element layout, but only at the level where the elements actually are:
    /// `struct Q qs[2]` reaches its records after one brace level, `struct Q qs[2][2]` after
    /// two. Above that the level is still an array and must keep element numbering.
    fn elem_layout_at(
        &self,
        elem_layout: Option<&[MemberSlot]>,
        depth: usize,
    ) -> Option<Vec<MemberSlot>> {
        (depth == 1)
            .then(|| elem_layout.map(<[MemberSlot]>::to_vec))
            .flatten()
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

    /// Walk the statements of `compound`, starting in `scope_view_meowsers`, and return the
    /// scope view the walk ended in together with whether the last statement *diverged*.
    /// Performs no end-of-compound link: that is the caller's call, because it depends on
    /// whether the compound was given a basic block of its own.
    fn walk_compound_body(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view_meowsers: &ScopeView,
        compound: &CompoundProxy<'_>,
    ) -> Result<(ScopeView, bool), Error> {
        let mut scope_view = scope_view_meowsers.clone();
        // Set when the statement just walked diverged (return/break/continue, or a label
        // whose body diverges): the current block is terminated and has no fall-through.
        let mut diverged = false;

        for &child in &compound.nodes {
            if !child.is_named() || child.kind() == "comment" {
                continue; // we skip , ( comments, stuff like that...
            }
            if diverged {
                // The previous statement diverged yet siblings remain. They are
                // unreachable by fall-through, but a `goto` label among them -- the
                // ubiquitous `out:` cleanup idiom -- is still reachable through its jump
                // edge, and its body has to lower or the cleanup code is dropped from the
                // IR entirely (`finalize_terminators` would then patch the orphaned label
                // block with an implicit empty `return` and report a frontend gap).
                // Keep walking in a fresh unlinked block, exactly as `walk_goto` does for
                // the statements that follow a `goto`.
                scope_view = add_block(
                    program,
                    &scope_view,
                    &mut self.scope_tree,
                    false,
                    &format!("after_diverge::{}", get_line_num(&child)),
                )?;
            }
            diverged = self.walk_statement(source, program, &mut scope_view, child)?;
        }

        Ok((scope_view, diverged))
    }

    /// Walk a compound that owns a basic block (an `if`/`while`/`for`/`switch` arm, a
    /// function body, ...) and link its fall-through to the enclosing continuation.
    fn walk_compound_statement(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view_meowsers: &ScopeView,
        compound: &CompoundProxy<'_>,
    ) -> Result<(), Error> {
        let (scope_view, diverged) =
            self.walk_compound_body(source, program, scope_view_meowsers, compound)?;

        // A compound whose last statement diverged has no fall-through, so the
        // end-of-compound link is skipped (it would push a continuation edge into a
        // block that already returns).
        if diverged {
            return Ok(());
        }

        //walked off a compound_statement
        log::debug!("EOCS linking blocks: ");
        link_blocks(program, &scope_view, scope_view_meowsers, true)?;

        Ok(())
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
            "compound_statement" => {
                let (inner_view, cp) = self.setup_compound(
                    program,
                    scope_view,
                    child,
                    BlockTypeRequest::JustScope,
                    true,
                    "compound_statement",
                )?;
                // A bare `{ ... }` in statement position gets a `JustScope` view: a fresh
                // lexical scope that deliberately *shares* the enclosing basic block. So it
                // has no end-of-compound edge to link, and asking `walk_compound_statement`
                // for one is what broke the CFG: with the function body's fall-off-the-end
                // sentinel (`continuation_blidx == None`) inherited, `link_blocks` stamped
                // an implicit `return` onto the block we are still filling, and every
                // following statement was then either orphaned (its incoming edge dropped
                // with a `continuation edge into a block that already returns` gap) or
                // appended behind that terminator.
                //
                // Walk the body directly instead, then thread the block it ended in back to
                // the caller -- keeping the caller's scope, since names declared inside the
                // braces must not outlive them -- and propagate its divergence, so
                // `{ return; }` still ends the enclosing compound's fall-through.
                let (end_view, diverged) =
                    self.walk_compound_body(source, program, &inner_view, &cp)?;
                *scope_view = ScopeView {
                    sidx: scope_view.sidx,
                    ..end_view
                };
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
            "update_expression" => {
                // A bare `++i` / `i++` in statement position -- in practice a `for`'s update
                // clause, which arrives here directly rather than wrapped in an
                // `expression_statement`. Lower the whole node: `flatten_expr` dispatches to
                // `flatten_update_expression`, which reads the `argument` field and so handles
                // prefix and postfix alike. Descending to `child(0)` instead (as the shared
                // `expression_statement` arm does) is wrong for both spellings: for prefix that
                // child is the `++` operator token, which reaches `flatten_expr`'s catch-all
                // ("ERR 78: Unsupported expression type: ++"), and for postfix it is the bare
                // identifier, which lowers to a read and silently drops the increment.
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
            // `return`/`break`/`continue` terminate the current block and have no
            // fall-through, so they end the compound (skipping its end link). A stray
            // `break`/`continue` outside any loop/switch recovers as a no-op and
            // reports `false`, so the compound continues normally.
            "return_statement" => {
                self.walk_return(source, program, scope_view, child)?;
                return Ok(true);
            }
            "break_statement" => {
                return self.walk_break(program, scope_view);
            }
            "continue_statement" => {
                return self.walk_continue(program, scope_view);
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
                unexpected_ast(format!("Unknown token(2): {kind}: {node_str}"))?;
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
        scope_view: &mut ScopeView,
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
                    unexpected_ast(format!(
                        "Declaration declarator had an unexpected kind {decl_kind}"
                    ))?;
                    continue;
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
                    self.collect_initializer_list(
                        source, program, scope_view, node, decl_ident, vc,
                    )?;
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
            let ret_exp = self.flatten_expr(program, ret_val_node, source, scope_view)?;
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

        // `for (;;)` legally omits any of the three clauses, and tree-sitter then
        // has no field for the missing one(s). Fall back to one of the for's own
        // `;`/`(` tokens: its CompoundProxy is empty, so the clause lowers to an
        // empty block and the loop wiring below is unchanged (a missing condition
        // still gets the exit edge -- conservative, and the condition's value is
        // ignored here anyway).
        let empty_clause = (0..child.child_count())
            .filter_map(|i| child.child(i as u32))
            .find(|n| n.kind() == ";" || n.kind() == "(");
        let (Some(initializer_node), Some(condition_node), Some(update_node)) = (
            child.child_by_field_name("initializer").or(empty_clause),
            child.child_by_field_name("condition").or(empty_clause),
            child.child_by_field_name("update").or(empty_clause),
        ) else {
            return malformed_source(format!(
                "for statement at line {} has no parsable clauses",
                get_line_num(&child) + 1
            ));
        };
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
        self.flatten_expr(program, condition, source, scope_view)?;

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
    /// `scope_view.break_target` — no stack to consult. Returns whether the block was
    /// terminated: a stray `break` outside any switch/loop (a source problem) recovers
    /// as a no-op, so following statements keep lowering into the same block.
    fn walk_break(&self, program: &mut Program, scope_view: &ScopeView) -> Result<bool, Error> {
        match scope_view.break_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)?;
                Ok(true)
            }
            None => {
                malformed_source("`break` outside of a switch or loop".to_string())?;
                Ok(false)
            }
        }
    }

    /// `continue`: terminate the current block with a goto to the innermost enclosing
    /// loop's re-test/update block (`scope_view.continue_target`). Termination and
    /// stray-`continue` recovery mirror [`Self::walk_break`].
    fn walk_continue(&self, program: &mut Program, scope_view: &ScopeView) -> Result<bool, Error> {
        match scope_view.continue_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)?;
                Ok(true)
            }
            None => {
                malformed_source("`continue` outside of a loop".to_string())?;
                Ok(false)
            }
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
        let Some(&target) = self.label_blocks.get(label) else {
            malformed_source(format!("`goto` to undefined label `{label}`"))?;
            // Recover as a no-op: with no target block to jump to, lowering simply
            // falls through to the next statement in the current block.
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
        self.flatten_expr(program, condition, source, scope_view)?; // gather field accesses and what not but we don't care about the condition result,etc.
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
        // Record layouts first: a positional brace initializer in any function body needs the
        // layout of a record that may be defined anywhere in the translation unit.
        self.collect_struct_layouts(source, tree.root_node());
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
        scope_view: &mut ScopeView,
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
            // `concatenated_string` is adjacent literals ("a" "b") -- one string
            // constant. `true`/`false`/`null` (NULL/nullptr) are keyword tokens the
            // grammar special-cases; they only survive to the AST when the source
            // was preprocessed without stdbool.h/stddef.h expanding them.
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
            // write to `x` (and a load `y = *p` reads the current `x`). `&a[i]` forms the
            // element's address (see `flatten_address_of`). Everything else -- `&x`, `&s.f`,
            // a dereference of a non-aliased/compound operand -- keeps the pass-through.
            "pointer_expression" => {
                let arg = node
                    .child_by_field_name("argument")
                    .expect("always a argument for the * operator");
                let is_deref = node
                    .child_by_field_name("operator")
                    .is_some_and(|op| to_str(&op, source) == "*");
                if !is_deref
                    && let Some(addr) = self.flatten_address_of(program, arg, source, scope_view)?
                {
                    return Ok(Exp::access_path(addr));
                }
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
                    let pointee = pointee.clone();
                    // A pointee that is a bare variable *is* the value (the pass-through model);
                    // one that is an interior address (`p = &x[1]` binds `x.[1]`) names memory,
                    // so reading `*p` loads the `deref` field at that address.
                    return match deref_of_pointee(&pointee) {
                        Some(ap) => Ok(Exp::access_path(self.emit_loads(program, scope_view, ap))),
                        None => Ok(Exp::access_path(pointee)),
                    };
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
            "sizeof_expression" => Ok(Exp::Str(ArcIntern::<str>::from(text))),
            // A C99 compound literal `(T){ .a = x }` is an unnamed object of type `T`
            // initialized by the brace, and the expression's value is that object. Model it
            // exactly that way: materialize a fresh temp to stand for the object, run the
            // *same* brace lowering a declaration's initializer gets (so designators and
            // positions land on `T`'s members, unknown tags fall back to element numbering),
            // and yield the temp. Without this the literal hit the catch-all below and every
            // value inside the braces was dropped -- the largest gap class in the corpus.
            //
            // The `type_descriptor` node has the same `type` field a declaration does, so
            // `declaration_type_tag` reads its record tag unchanged; its optional `declarator`
            // is the abstract spelling of an array declarator, and carries the rank.
            "compound_literal_expression" => {
                let ty = node
                    .child_by_field_name("type")
                    .expect("compound_literal_expression always has a type");
                let value = node
                    .child_by_field_name("value")
                    .expect("compound_literal_expression always has a value");
                let own = declaration_type_tag(ty, source)
                    .and_then(|tag| self.struct_layouts.get(tag).cloned());
                let rank = ty
                    .child_by_field_name("declarator")
                    .map(array_declarator_rank)
                    .unwrap_or(0);
                let temp_name = self.allocator.next_temp();
                let base_ap = self.build_access_path(
                    temp_name.as_str(),
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                self.lower_braced_value(source, program, scope_view, &base_ap, value, own, rank)?;
                Ok(Exp::Variable(VariableRef::new_local_idx(
                    program[scope_view.fidx].locals.get_or_intern(&temp_name),
                )))
            }
            // A GNU statement expression `({ s1; s2; value; })`. tree-sitter-c 0.24.1 has no
            // node for the construct: it parses as a `parenthesized_expression` wrapping a
            // `compound_statement`, so it lands here with `node.kind() == "compound_statement"`.
            // See `lower_statement_expression_effects`.
            "compound_statement" => {
                let outer_sidx = scope_view.sidx;
                let outer_span = self.cur_span;
                let value = match self
                    .lower_statement_expression_effects(program, node, source, scope_view)?
                {
                    Some(inner) => self.flatten_expr(program, inner, source, scope_view)?,
                    // A `void`-valued statement expression has no value to yield; hand back a
                    // temp nothing reads, exactly as the recovery path would.
                    None => {
                        let temp_name = self.allocator.next_temp();
                        Exp::Variable(VariableRef::new_local_idx(
                            program[scope_view.fidx].locals.get_or_intern(&temp_name),
                        ))
                    }
                };
                scope_view.sidx = outer_sidx;
                self.cur_span = outer_span;
                Ok(value)
            }
            // A ternary `c ? a : b` is path-insensitive here: either arm may be the
            // value, so blend both into a temp (like `flatten_binary`). The condition is
            // a control dependence, not a data source -- evaluate it for side effects but
            // don't blend it into the result.
            //
            // GNU's `c ?: b` omits the consequence, and there the condition IS the value
            // when it is truthy (evaluated once). tree-sitter leaves the `consequence`
            // field absent for that shape, so reuse the condition's already-computed
            // value as the consequent arm rather than assuming the field is present.
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
                Ok(self.blend_into_temp(program, scope_view, &[cons_val, alt_val]))
            }
            // A C11 generic selection `_Generic(ctrl, T1: e1, ..., default: eN)`. Only the arm
            // whose type matches the controlling expression is evaluated, and picking it needs
            // the static type of `ctrl` -- which this frontend does not have and must not start
            // computing. So the selection is treated exactly like the ternary above, just with
            // N arms instead of two: any of them may be the value, so all of them lower and all
            // of them blend into one temp.
            //
            // The controlling expression is NOT evaluated in C (`_Generic` selects on its
            // *type*), so it must not join the blend -- but it cannot simply be skipped either,
            // because the kernel's `_Generic(*(&sl->seqcount), ...)` mentions the object nowhere
            // else. It gets the ternary condition's treatment: lowered for its effects, its
            // value dropped.
            //
            // Without this arm every `_Generic` became an opaque temp, which in the kernel meant
            // the whole `__seqprop_*`/`container_of`/`min`/`max` dispatch family -- the calls in
            // the arms included -- was invisible.
            "generic_expression" => {
                let (ctrl, arms) = generic_selection_parts(node);
                if let Some(ctrl) = ctrl {
                    self.flatten_expr(program, ctrl, source, scope_view)?;
                }
                let mut values = Vec::with_capacity(arms.len());
                for arm in arms {
                    values.push(self.flatten_expr(program, arm, source, scope_view)?);
                }
                Ok(self.blend_into_temp(program, scope_view, &values))
            }
            // GNU inline assembly (`__asm__ ("..." : outs : ins : clobbers)`). The assembly text
            // is opaque to this frontend, so it is modeled as the operand transfer it is rather
            // than dropped -- see `flatten_gnu_asm`. Without this arm every `__asm__` hit the
            // catch-all below, so a value laundered through inline asm lost its taint and an
            // `"+r"` in/out operand lost its identity flow (the openssh `crypto_int*` shapes).
            "gnu_asm_expression" => self.flatten_gnu_asm(program, node, source, scope_view),
            _ => {
                debug_print_tree(node, 0, None, None);
                unexpected_ast(format!(
                    "ERR 78: Unsupported expression type: {}",
                    node.kind()
                ))?;
                // Recover with a fresh temp nothing else reads or writes: this
                // expression's value becomes opaque (no flows in or out), but the
                // surrounding statement still lowers.
                let temp_name = self.allocator.next_temp();
                Ok(Exp::Variable(VariableRef::new_local_idx(
                    program[scope_view.fidx].locals.get_or_intern(&temp_name),
                )))
            }
        }
    }

    /// Lowers the *effects* of a GNU statement expression `({ s1; s2; value; })` -- everything
    /// between the braces except the value -- and hands back the expression node that produces
    /// the value, if there is one.
    ///
    /// The construct is by a wide margin the kernel's most pervasive idiom (`container_of`,
    /// `READ_ONCE`/`WRITE_ONCE`, `min`/`max`, `smp_load_acquire`, every locked bit-op and every
    /// instrumented atomic expands to one) and, at 27k occurrences, was the largest gap class the
    /// kernel census found. Before this it reached `flatten_expr`'s catch-all, which substituted
    /// a temp nothing reads or writes: both the statements inside the braces and the value they
    /// produce were dropped, so taint entering a statement expression never left it.
    ///
    /// Its value is the value of the **last** statement, which C requires to be an expression
    /// statement. So the leading statements go through the ordinary statement walker -- making
    /// the declarations, calls and assignments inside the braces real IR -- and the trailing
    /// expression is left to the caller: `flatten_expr` wants its `Exp`, while `flatten_lvalue`
    /// wants its access path, because `container_of(p, T, m)->f = v` puts a statement expression
    /// in store position.
    ///
    /// Like a bare block (`walk_statement`'s `compound_statement` arm) the braces get a fresh
    /// lexical scope but deliberately *share* the enclosing basic block -- a statement expression
    /// continues the enclosing block, it does not close it -- so there is no end-of-compound link
    /// to make. On return `scope_view` names that inner scope and the block the body ended in, so
    /// the caller can lower the value node there; the caller then restores its own `sidx` (names
    /// declared between the braces must not outlive them) and `cur_span` (walking the body
    /// retargets the span at every statement inside).
    ///
    /// The block has to be threaded back out because statements inside may open blocks of their
    /// own -- `do { } while (0)` sits in every `READ_ONCE` -- and everything lowered afterwards
    /// has to land in the block control actually reaches. Statement expressions nest, too (the
    /// kernel's RCU accessors put one inside another), and this is re-entrant: the nested one is
    /// just another `flatten_expr` call on the value node.
    fn lower_statement_expression_effects<'t>(
        &mut self,
        program: &mut Program,
        node: Node<'t>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Option<Node<'t>>, Error> {
        let (inner_view, cp) = self.setup_compound(
            program,
            scope_view,
            node,
            BlockTypeRequest::JustScope,
            true,
            "statement_expression",
        )?;

        // Split off the trailing statement: it produces the value, everything before it runs
        // only for its effects. `cp.nodes` still holds the braces and any comments.
        let mut stmts = cp.nodes;
        stmts.retain(|child| child.is_named() && child.kind() != "comment");
        let value_node = stmts.pop();
        let prefix = CompoundProxy {
            nodes: stmts,
            was_compound: true,
        };

        let (mut end_view, mut diverged) =
            self.walk_compound_body(source, program, &inner_view, &prefix)?;
        if diverged {
            end_view = self.block_after_stmt_expr_diverge(program, &end_view, node)?;
        }

        let value = match value_node {
            // `({ ...; e; })` -- the ordinary shape. `e` is the value of the construct.
            Some(last) if last.kind() == "expression_statement" => {
                self.cur_span = self.span_for_node(last);
                // `({ ...; ; })` -- a trailing empty statement carries no expression.
                last.child(0).filter(|inner| !_is_empty(inner))
            }
            // A `void`-valued statement expression, e.g. the kernel's ubiquitous
            // `({ do { } while (0); })`. Well-defined C, not a gap: lower the statement here
            // and leave the caller to invent a value nothing reads.
            Some(last) => {
                diverged = self.walk_statement(source, program, &mut end_view, last)?;
                if diverged {
                    end_view = self.block_after_stmt_expr_diverge(program, &end_view, node)?;
                }
                None
            }
            // `({ })`, or braces holding nothing but comments.
            None => None,
        };

        *scope_view = end_view;
        Ok(value)
    }

    /// Opens a fresh *unlinked* block to keep lowering a statement expression into after its
    /// body diverged (a `return`/`break`/`continue` between the braces). The value expression
    /// and everything after the construct are unreachable, but they still have to lower
    /// somewhere: appending them to the block the divergence terminated would strand IR behind
    /// a terminator. Same recovery `walk_compound_body` uses for statements after a divergence.
    fn block_after_stmt_expr_diverge(
        &mut self,
        program: &mut Program,
        view: &ScopeView,
        node: Node<'_>,
    ) -> Result<ScopeView, Error> {
        add_block(
            program,
            view,
            &mut self.scope_tree,
            false,
            &format!("after_stmt_expr_diverge::{}", get_line_num(&node)),
        )
    }

    /// Lowers a GNU inline-assembly expression as an opaque **operand transfer**.
    ///
    /// The assembly body is never analyzed, so the only sound model is that the black box
    /// relates all of its operands: every input operand may reach every output operand. That is
    /// emitted as one blend temp holding all the inputs, assigned into each output. Funnelling
    /// through a single temp -- rather than handing each write two operands -- is what lets an
    /// output that is a *field* path carry the value at all: a store lowers exactly one value
    /// (see [`Context::add_assign_to_program`]), so a second operand would be silently dropped.
    ///
    /// An output constrained with `+` is read-modify-write, so its old value is also a source:
    /// that is what keeps the `x -> x` identity flow in openssh's
    /// `__asm__ ("sarw $15,%0" : "+r"(x) : : "cc")`. Every read is emitted before every write,
    /// matching the C semantics. Clobbers name registers, not C locations, so they carry no
    /// dataflow and are ignored.
    ///
    /// `asm goto` still reports a frontend gap: its jumps to the label list are real CFG edges
    /// out of an expression, which cannot be built from here. The operands are modeled anyway,
    /// so only the control edges are missing (see `asm_goto_is_a_known_limitation`). No corpus
    /// uses it.
    fn flatten_gnu_asm(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Exp, Error> {
        if node
            .child_by_field_name("goto_labels")
            .is_some_and(|labels| labels.child_by_field_name("label").is_some())
        {
            unexpected_ast(
                "asm goto: jumps to the label list are not modeled as CFG edges (operands still \
                 lower)"
                    .to_string(),
            )?;
        }

        let outputs = gnu_asm_operands(node, "output_operands");
        let inputs = gnu_asm_operands(node, "input_operands");

        // Reads first, writes after: the asm consumes every input before producing any output,
        // so a `"+r"` operand that is both must be read while it still holds the old value.
        let mut sources = Vec::with_capacity(inputs.len() + outputs.len());
        for operand in &inputs {
            let value = gnu_asm_operand_value(*operand);
            sources.push(self.flatten_expr(program, value, source, scope_view)?);
        }
        let mut targets = Vec::with_capacity(outputs.len());
        for operand in &outputs {
            let value = gnu_asm_operand_value(*operand);
            let target = self.flatten_lvalue(program, value, source, scope_view)?;
            if gnu_asm_operand_is_readwrite(*operand, source) {
                let old = self.emit_loads(program, scope_view, target.clone());
                sources.push(Exp::access_path(old));
            }
            targets.push(target);
        }

        // Blend the sources into one temp, folding in one operand at a time -- the same
        // `t = src op t` shape a compound assignment (`y += x`) lowers to, so the running value
        // is read before this statement redefines it. A temp with no sources at all is never
        // written, which is exactly the opaque value an operand-less `__asm__ ("pause")` yields.
        let blend_name = self.allocator.next_temp();
        let blend = self.build_access_path(
            blend_name.as_str(),
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        for (i, src) in sources.iter().enumerate() {
            let running = if i == 0 {
                None
            } else {
                let so_far = self.emit_loads(program, scope_view, blend.clone());
                Some(Exp::access_path(so_far))
            };
            self.add_assign_to_program(program, scope_view, &blend, src, running.as_ref());
        }

        let blended = Exp::access_path(self.emit_loads(program, scope_view, blend));
        for target in &targets {
            self.add_assign_to_program(program, scope_view, target, &blended, None);
        }

        // The value of an asm expression is the first output operand's location; with no
        // outputs it is the opaque blend temp. In practice nothing reads it -- every real site
        // is a statement -- but `flatten_expr` must yield something.
        match targets.first() {
            Some(target) => {
                let read_back = self.emit_loads(program, scope_view, target.clone());
                Ok(Exp::access_path(read_back))
            }
            None => Ok(blended),
        }
    }

    fn flatten_nested_decl(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
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
            unexpected_ast(
                "Surprised, Pointer Declarators dont always have a declarators".to_string(),
            )?;
            // Recover as an opaque temp, like `flatten_expr`'s catch-all.
            let temp_name = self.allocator.next_temp();
            Ok(Exp::Variable(VariableRef::new_local_idx(
                program[scope_view.fidx].locals.get_or_intern(&temp_name),
            )))
        }
    }

    fn flatten_binary(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        operator: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
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
        scope_view: &mut ScopeView,
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
        scope_view: &mut ScopeView,
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
        scope_view: &mut ScopeView,
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
        scope_view: &mut ScopeView,
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
            self.lower_function(
                source,
                program,
                global_sidx,
                func_name,
                return_type,
                param_list,
                body_node,
            )?;
        }
        Ok(())
    }

    /// Lower one `function_definition` into its IR function: register the name, set the return
    /// arity, build the parameter and body scopes, pre-create a block per `goto` label, walk the
    /// body, and finalize terminators.
    ///
    /// Split out of [`Context::collect_functions`] so the per-function lowering is one named unit
    /// rather than an 80-line loop body. That keeps the query/dispatch loop readable, and it is
    /// what lets a change in here be reviewed (and merged) as a diff against a function instead of
    /// against an anonymous block.
    fn lower_function(
        &mut self,
        source: &'a str,
        program: &mut Program,
        global_sidx: usize,
        func_name: &'a str,
        return_type: Option<Node<'_>>,
        param_list: Node<'_>,
        body_node: Node<'_>,
    ) -> anyhow::Result<(), Error> {
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
        finalize_terminators(program, fidx, func_name)?;
        Ok(())
    }

    /// Blends every expression in `values` into one fresh temp and yields it -- the value of a
    /// construct where the frontend cannot tell which of several expressions is the result, so
    /// all of them may be (a ternary's two arms, a generic selection's N).
    ///
    /// Two values fit in a single `assign`, which is the statement a ternary has always lowered
    /// to; any further value folds in with the running temp as the second operand -- the
    /// `t = src, t` shape [`Context::flatten_gnu_asm`] uses -- because an `assign` carries at
    /// most two. A blend of nothing is a temp that is never written, i.e. an opaque value.
    fn blend_into_temp(
        &mut self,
        program: &mut Program,
        scope_view: &ScopeView,
        values: &[Exp],
    ) -> Exp {
        let temp_name = self.allocator.next_temp();
        let target = self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        let mut rest = values.iter();
        if let Some(first) = rest.next() {
            let second = rest.next();
            self.add_assign_to_program(program, scope_view, &target, first, second);
            for extra in rest {
                let so_far = self.emit_loads(program, scope_view, target.clone());
                let running = Exp::access_path(so_far);
                self.add_assign_to_program(program, scope_view, &target, extra, Some(&running));
            }
        }
        Exp::Variable(VariableRef::new_local_idx(
            program[scope_view.fidx].locals.get_or_intern(&temp_name),
        ))
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
            //
            // A store must end in a symbolic field: a target that is a pure *address*
            // (offsets only, e.g. the pointee of `p = &x[1]` reached through `*p = v`) names
            // memory, so terminate it with the dereference field, exactly as the pcode frontend
            // terminates an offset-only address with `.deref`. Without this the write would
            // trip `assign_or_store`'s "storing to an offset address with no field" assertion.
            let mut fields = target.fields.clone();
            if fields.iter().all(PathSegment::is_offset) {
                fields.push(PathSegment::symbol(DEREF_FIELD));
            }
            let mut stmts = Vec::new();
            let allocator = &mut self.allocator;
            let locals = &mut program[scope_view.fidx].locals;
            ctadl_ir::mir::store_access_path(
                target.base.clone(),
                fields,
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

    /// Lowers `&e` to the *address* of the location `e` names, or `None` when this frontend has
    /// no way to name that address (the caller then falls back to the historical value-copy
    /// model, which lowers `&e` to a read of `e`).
    ///
    /// An address in the IR is a base variable plus pointer-arithmetic offsets, so the addresses
    /// that *are* nameable are exactly the element accesses: `&a[1]` is `a.[1]`, the same address
    /// a subscript computes before dereferencing it. Forming it -- rather than loading the
    /// element -- is what preserves pointer identity across a call: a callee that stores at
    /// `.[1].deref` through the parameter writes `a.[2].deref`, which is where the caller's `a[2]`
    /// reads (offsets are summed when the paths meet). Loading the element instead hands the
    /// callee a *copy*, and the write is lost.
    ///
    /// Not nameable, and so left to the value model: `&x` (a whole variable, which is already its
    /// own address in this IR -- the pass-through in `flatten_expr` handles it), and `&s.f`, whose
    /// address would need the byte offset of a struct member that this frontend, having no type
    /// information, cannot compute; it names members symbolically instead.
    fn flatten_address_of(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Option<AccessPath>, Error> {
        match node.kind() {
            "parenthesized_expression" => {
                let inner = node.child(1).expect("missing inner expr");
                self.flatten_address_of(program, inner, source, scope_view)
            }
            "subscript_expression" => {
                let mut ap = self.flatten_lvalue(program, node, source, scope_view)?;
                // Drop the dereference field: the address is everything up to the dereference the
                // subscript performs. Whatever symbolic fields remain in the prefix (`&s.a[1]`,
                // `&a[1][2]`) are still real dereferences, so `emit_loads` materializes them and
                // returns the residual base + offsets -- the address. (`flatten_lvalue` of a
                // subscript always ends in a dereference, so the pop only fails if that ever
                // stops holding.)
                match ap.fields.pop() {
                    Some(seg) if is_deref_field(&seg) => {}
                    _ => return Ok(None),
                }
                Ok(Some(self.emit_loads(program, scope_view, ap)))
            }
            _ => Ok(None),
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
        scope_view: &mut ScopeView,
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
                // `a[N]` is `*(a + N)`: the index is pointer arithmetic on the address and the
                // element itself is the memory read/written there (see `DEREF_FIELD`).
                let mut ap = base;
                push_element(&mut ap.fields, constant_index(&index));
                Ok(ap)
            }
            // A GNU statement expression in lvalue position -- `container_of(p, T, m)->f = v`
            // and the rest of the kernel's list/RCU accessors. The location it names is the
            // location its value expression names, so run the braces for their effects and
            // resolve that value node as the lvalue. Falling through to the catch-all below
            // instead only worked when the value happened to lower to a bare variable; anything
            // else (a field path, a cast) reported `not an lvalue: compound_statement` and the
            // store was dropped onto a dead temp.
            "compound_statement" => {
                let outer_sidx = scope_view.sidx;
                let outer_span = self.cur_span;
                let path = match self
                    .lower_statement_expression_effects(program, node, source, scope_view)?
                {
                    Some(inner) => self.flatten_lvalue(program, inner, source, scope_view)?,
                    None => {
                        let temp_name = self.allocator.next_temp();
                        RawPath::new(
                            VariableRef::new_local_idx(
                                program[scope_view.fidx].locals.get_or_intern(&temp_name),
                            ),
                            ThinVec::new(),
                        )
                    }
                };
                scope_view.sidx = outer_sidx;
                self.cur_span = outer_span;
                Ok(path)
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
                // reads of `x`. Mirrors the read path in `flatten_expr`, including the `deref`
                // field an interior pointee (`p = &x[1]`) needs to name its memory.
                if is_deref
                    && ptr.is_pathless()
                    && let Some((pointee, blk)) = self.addr_alias.get(&ptr.base)
                    && *blk == scope_view.blidx
                {
                    return Ok(deref_of_pointee(pointee).unwrap_or_else(|| {
                        RawPath::new(pointee.variable_ref.clone(), ThinVec::new())
                    }));
                }
                Ok(ptr)
            }
            _ => match self.flatten_expr(program, node, source, scope_view)? {
                Exp::Variable(v) => Ok(RawPath::new(v, ThinVec::new())),
                _ => {
                    unexpected_ast(format!("not an lvalue: {}", node.kind()))?;
                    // Recover by targeting a dead temp: this one store is dropped,
                    // the rest of the function still lowers.
                    let temp_name = self.allocator.next_temp();
                    Ok(RawPath::new(
                        VariableRef::new_local_idx(
                            program[scope_view.fidx].locals.get_or_intern(&temp_name),
                        ),
                        ThinVec::new(),
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

/// The switch behind [`unexpected_ast`] and [`malformed_source`]: by default log a
/// warning (prefixed with who is at fault) and return `Ok(())` so the call site can
/// recover and the user still gets useful results from the rest of the program. Set
/// `CTADL_ERROR_ON_AST` (to any value) to promote every such report to a hard
/// ingestion error, which is what you want when hunting frontend gaps.
fn recoverable_report(attribution: &str, msg: String) -> Result<(), Error> {
    if error_on_ast() {
        Err(Error::TreeSitterParse(msg))
    } else {
        log::warn!("{attribution}: {msg} (recovering; set CTADL_ERROR_ON_AST to fail instead)");
        Ok(())
    }
}

/// Whether AST/source problems are hard errors: `CTADL_ERROR_ON_AST` in the
/// environment, or the per-thread test override below.
fn error_on_ast() -> bool {
    #[cfg(test)]
    if FORCE_ERROR_ON_AST.with(std::cell::Cell::get) {
        return true;
    }
    std::env::var_os("CTADL_ERROR_ON_AST").is_some()
}

#[cfg(test)]
thread_local! {
    static FORCE_ERROR_ON_AST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only: strict `CTADL_ERROR_ON_AST` behavior on this thread for the returned
/// guard's lifetime. A per-thread flag rather than the env var itself, which is
/// process-global and would race the rest of the parallel test harness. Ingestion
/// runs on the caller's thread, so the flag covers `parse_c_program`.
#[cfg(test)]
fn force_error_on_ast() -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_ERROR_ON_AST.with(|f| f.set(false));
        }
    }
    FORCE_ERROR_ON_AST.with(|f| f.set(true));
    Reset
}

/// An AST shape the frontend does not lower (unknown statement kinds, unsupported
/// expressions, declarators we don't recognize) — a gap in the frontend, not a
/// problem in the analyzed source. Call sites recover by skipping the construct or
/// substituting a fresh opaque temp.
fn unexpected_ast(msg: String) -> Result<(), Error> {
    recoverable_report("frontend gap", msg)
}

/// A construct the analyzed source itself misuses (`break` outside a loop, `goto` to
/// an undefined label) — a problem in that code, not a frontend gap. Same switch as
/// [`unexpected_ast`]; the warning attributes the fault to the source.
fn malformed_source(msg: String) -> Result<(), Error> {
    recoverable_report("source problem", msg)
}

/// Splits a `generic_expression` into its controlling expression and the *values* of its
/// associations (`_Generic(ctrl, T1: e1, ..., default: eN)` -> `ctrl`, `[e1, ..., eN]`).
///
/// tree-sitter-c 0.24.1 gives the construct no field names and no per-association node: the
/// named children are the controlling expression followed by a flat `type_descriptor`, value,
/// `type_descriptor`, value, ... sequence, and `default:` is spelled as a `type_descriptor`
/// whose type is the identifier `default`. So the values are exactly the named children after
/// the first that are not `type_descriptor`s -- reading them positionally instead would break
/// on `comment`, which is an `extra` and may appear anywhere.
///
/// An `ERROR` child counts as a value, so it is lowered (and reported) like any other. The
/// kernel produces one per `container_of`: `_Generic(sk, const typeof(*(sk)) *: ...)` has an
/// association type tree-sitter-c cannot parse, and the stray `*` it recovers with lands here.
/// Charging that to the ERROR-debris class is deliberate -- it is the same parse debris every
/// other dispatch point reports, and reclassifying all of it at once is its own spec.
fn generic_selection_parts<'t>(node: Node<'t>) -> (Option<Node<'t>>, Vec<Node<'t>>) {
    let mut cursor = node.walk();
    let mut named = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment");
    let controlling = named.next();
    let values = named
        .filter(|child| child.kind() != "type_descriptor")
        .collect();
    (controlling, values)
}

/// The `operand` children of a `gnu_asm_expression`'s `output_operands` / `input_operands` list.
/// Both fields are optional AND a present list may still be empty -- `__asm__ ("pause")` has no
/// lists at all, while `__asm__ ("mfence" ::: "memory")` parses as two empty ones -- so this
/// yields nothing in either case.
fn gnu_asm_operands<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let Some(list) = node.child_by_field_name(field) else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.children_by_field_name("operand", &mut cursor)
        .collect()
}

/// The C expression an asm operand names: the lvalue written for an output, the value read for
/// an input. Required by the grammar for both operand kinds.
fn gnu_asm_operand_value<'t>(operand: Node<'t>) -> Node<'t> {
    operand
        .child_by_field_name("value")
        .expect("a gnu asm operand always has a value")
}

/// Whether an asm output operand is read-modify-write. GNU spells that with a `+` in the
/// constraint (`"+r"`, `"+&r"`, the multi-alternative `"+r,m"`), as against write-only `"="`.
fn gnu_asm_operand_is_readwrite(operand: Node<'_>, source: &str) -> bool {
    operand
        .child_by_field_name("constraint")
        .is_some_and(|c| to_str(&c, source).contains('+'))
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
