//! Tree-sitter AST extraction and IR lowering for the **C family**.
//!
//! This module is the lowering core. It parses source (POST PREPROCESSOR) and lowers it to the
//! IR, and it is *grammar-parameterized* rather than C-only: both frontends run this same code.
//! There are two entry points.
//!
//! - [`parse_c_program`] (here) drives it with the C grammar. This is the production frontend.
//! - `cpp::parse_cpp_program` drives it with the C++ grammar. This is **experimental** and may
//!   not survive; nothing in the C path should be complicated to accommodate it.
//!
//! # How the two share this code
//!
//! The core contains **no language branches**. Where the two grammars genuinely diverge, the
//! difference is reached through `GrammarHooks` — a table of plain `fn` pointers installed on
//! the `Context`. `GrammarHooks::C` is the default and defines the C behavior; `cpp.rs`
//! overrides it with `cpp::CPP_HOOKS`. Two kinds of hook live there:
//!
//! - **Node-shape adapters** for the same construct spelled differently (`condition_expr` for
//!   C++'s `condition_clause` wrapper, `subscript_index` for its `subscript_argument_list`,
//!   `param_query` for the C++-only `reference_declarator`).
//! - **Emission hooks** for constructs C does not have at all (`construct`, `delete_expr`,
//!   `scope_exit`, `ctor_prologue`, `collect_aux`, `collect_overloads`). Every one of these is a
//!   **no-op** in `GrammarHooks::C`, and the C++ implementations live in `cpp.rs` — so reading
//!   the C path means reading this file plus a set of no-ops.
//!
//! The core does carry some state that only C++ populates (`Context::classes`,
//! `subclasses`, `dtor_frames`, `overloads`, `local_types`, `reference_aliases`). Those maps are
//! empty on the C path, so every lookup simply misses and the C lowering is unchanged. That is
//! the one place the experiment is visible here; it is data, never a branch.
//!
//! Note the directory name is historical: `tree_sitter_c/` hosts both grammars, not just C.
//!
//! # Known Limitations
//!
//! These are limitations of the C lowering. For the C++ frontend's own gaps (templates,
//! multiple inheritance, exception-path destructors, and more), see `cpp.rs`.
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
//! ```
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
//! ```
//!
//! ## `asm goto`
//!
//! GNU inline assembly is lowered as an operand transfer ([`Context::flatten_gnu_asm`]): every
//! input operand may reach every output operand, and a `"+"` operand keeps its identity flow.
//! `asm goto` additionally *jumps* to one of its labels. Those are real CFG edges, and they are
//! built from inside the expression walk ([`Context::link_asm_goto_labels`]): the block the asm
//! sits in gets an edge to each label's pre-created block *plus* the fall-through edge into a
//! fresh block that the rest of the statement continues in. Unlike `goto`, an `asm goto` does
//! not diverge -- it may fall through -- so it is not reported as a divergence. Pinned by
//! `asm_goto_label_is_reachable`, `asm_goto_also_falls_through` and
//! `asm_goto_multiple_labels_all_link`. Every kernel static key (`arch_static_branch`) is one.
//!

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;

use crate::error::Error;

use ctadl_ir::ThinVec;
use ctadl_ir::index::index_vec::IndexVec;
use ctadl_ir::mir::*;

use source_info::{ArtifactEncoding, ArtifactKey, ArtifactMetadata, SourceInfoBuilder, SpanLen};

use internment::ArcIntern;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, QueryMatch, Tree};

#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod testing_block_flow_ascii;

/// C++ frontend entry point (`parse_cpp_program`). A submodule so it can reach the
/// language-agnostic lowering core in this module (`Context`, `markup`, …) and reuse it
/// without duplication; only the grammar it drives differs from the C path.
mod cpp;
pub use cpp::parse_cpp_program;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cpp_tests;

#[cfg(test)]
mod experimental_tests;

#[derive(Debug, Clone, PartialEq, Eq)]
enum VarKind {
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

/// Re-open an already-lowered [`AccessPath`] (base + offsets, an address) as a [`RawPath`] so
/// further symbolic fields can be appended before it is lowered again. Used where a resolved
/// object has to be recorded for later path extension — a C++ reference alias, whose uses append
/// the fields written through the reference.
fn raw_path_of(ap: &AccessPath) -> RawPath {
    RawPath::new(
        ap.variable_ref.clone(),
        ap.path.iter().cloned().map(PathSegment::from).collect(),
    )
}

enum BlockTypeRequest {
    NewBlockOrScopedBlock, // things that induce lexical scope like compound statements.
    JustScope, // things like the conditional of an if.  or compound statements mid expression run.
    JustBlock, // the For loop's scope is defined at the initializer, so we don't want extra scope for the body
}

// TODO_JDB: implement var type thing to accomodate parameters have extra *stuff*
#[derive(Debug, Clone)]
struct VarDecl {
    pub name: String,
    pub kind: VarKind,
    pub shadows: bool, // this is set at creation time, because at the time of the declaration is when the shadowing occurs,
    // so assigns that have already happened will never ask about the variable again.  you will never add a VarDecl that doesn't shadow, and then later "upgrade it to shadow"
    pub sidx: usize,
}

#[derive(Debug)]
struct ScopeBox {
    pub scope_name: String,
    pub parent_idx: Option<usize>,
    pub variables: Vec<VarDecl>,
}

#[derive(Debug, Default)]
struct ScopeTree {
    pub scopes: Vec<ScopeBox>,
    pub blocks: Vec<ScopeView>,
}

impl ScopeTree {
    fn add_scope(&mut self, name: String, parent: Option<usize>) -> usize {
        let new_scope = ScopeBox {
            scope_name: name,
            parent_idx: parent,
            variables: Vec::new(),
        };

        let index = self.scopes.len();
        self.scopes.push(new_scope);
        index
    }

    fn add_block(&mut self, scope_view: &ScopeView) {
        self.blocks.push(scope_view.clone());
    }

    fn get_explainers(blocks: &[ScopeView], target_func: &str, target_blidx: u32) -> String {
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
    fn to_string(&self, var: &VarDecl) -> String {
        if var.shadows {
            if let Some(scope) = self.scopes.get(var.sidx) {
                return format!("{}.{}.{}", scope.scope_name, var.sidx, var.name);
            } else {
                panic!("Variable had a scope {} that didn't exist", var.sidx);
            }
        }
        var.name.to_string()
    }

    fn add_variable(&mut self, sidx: usize, symbol: String, kind: VarKind) {
        let shadows = self.find_variable(sidx, symbol.as_str()).is_some();
        if let Some(scope) = self.scopes.get_mut(sidx) {
            scope.variables.push(VarDecl {
                name: symbol,
                kind,
                shadows,
                sidx,
            });
        } else {
            panic!("attempt to add to nonexistent scope: {}", sidx)
        }
    }

    fn find_variable(&self, start_idx: usize, target_name: &str) -> Option<&VarDecl> {
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
struct CompoundProxy<'a> {
    pub nodes: Vec<Node<'a>>,
    pub was_compound: bool,
}

impl<'a> CompoundProxy<'a> {
    fn from_node(body_node: Node<'a>) -> Self {
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

/// The name of the local every synthesized return reads. Angle brackets are not legal in a C
/// identifier, so this can never collide with a name from the source (the same trick
/// [`TempAllocator::next_temp`] uses for `<tN>`). One per function is enough: it is never
/// written, so every read of it is the same absence of a value.
const IMPLICIT_RETURN_LOCAL: &str = "<implicit-return>";

/// The terminator to close a block with when the frontend, not the source, decides it returns.
///
/// `lower_function` gives every function whose declared return type is not `void` a
/// [`ReturnType`] of arity 1, and `verify()` rejects a `Return` carrying a different number of
/// arguments ([`ctadl_ir::mir::VerifyError::InconsistentReturns`]), so the empty `return` is
/// only well-formed in a `void` function. Three places have no expression to return and used to
/// emit the empty one regardless -- falling off the end of a body (see [`link_blocks`]),
/// patching a block the walk left unterminated ([`finalize_terminators`]), and a bare `return;`
/// written inside a non-`void` function, which is legal C and common in `int` error paths. All
/// three produced IR that did not verify, silently: no warning is logged, and nothing on the
/// `ctadl import` path checks -- unlike the dex, jvm and pcode front ends, this one never calls
/// `program.verify()`, and `ssa::transform_program`'s post-SSA check passes because
/// `complete()` has already rewritten every `Return` into a goto to a single exit block. Only
/// `get_summary` in this module's tests ever asked, which is why no census saw it.
///
/// Satisfy the arity contract with a local that is never assigned. C says the value of such a
/// return is indeterminate, and an unwritten local is exactly that in this IR: it carries no
/// taint into the return, so no flow is invented -- the same "value nothing reads" shape
/// [`Context::flatten_gnu_asm`]'s operand-less blend already yields.
fn implicit_return(program: &mut Program, fidx: FunctionIdx) -> Terminator {
    let fdat = &mut program.functions[fidx];
    let arity = fdat.return_type.arity as usize;
    let args: Vec<Exp> = if arity == 0 {
        vec![]
    } else {
        let local = VariableRef::new_local_idx(fdat.locals.get_or_intern(IMPLICIT_RETURN_LOCAL));
        vec![Exp::Variable(local); arity]
    };
    Terminator::new_kind(TerminatorKind::Return { args: args.into() })
}

fn link_blocks(
    program: &mut Program,
    from_sv: &ScopeView,
    to_sv: &ScopeView,
    continuation: bool,
) -> Result<(), Error> {
    let target_val = if continuation {
        match to_sv.continuation_blidx {
            Some(idx) => idx,
            None => {
                // Falls off the end of the function body: emit an implicit
                // `return` (SSA `complete()` rewrites it into a goto-to-exit).
                // Mirrors the shape `walk_return` produces for a bare `return;`.
                match program.functions[from_sv.fidx].blocks.get(from_sv.blidx) {
                    Some(block) if block.terminator.is_some() => return Ok(()),
                    Some(_) => {}
                    None => {
                        return Err(Error::TreeSitterParse(format!(
                            "attempt to link a non existing from block: {:?}",
                            from_sv
                        )));
                    }
                }
                let term = implicit_return(program, from_sv.fidx);
                program.functions[from_sv.fidx].blocks[from_sv.blidx].terminator = Some(term);
                return Ok(());
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
///
/// `stranded_labels` names the blocks pre-created for labels `walk_labeled_statement` never
/// entered. In a body the parser did not fully parse (`body_holds_recovery`) those are
/// patched like any other but **not** reported: the label sits in parse-recovery output,
/// which this frontend deliberately does not analyze, so the loss is a fact about the
/// analyzed source and is stated as one -- once per function, by
/// `Context::report_unanalyzed_recovery` -- rather than a second time as a frontend gap. That
/// is spec 064's rule applied to the last class that still broke it: all 29 of the kernel
/// census's remaining `left without a terminator` warnings were label blocks of this kind.
/// In a body that parsed cleanly a stranded label block is still a gap and still reported --
/// a duplicate label orphans one (`error_on_ast_promotes_unterminated_block`), and that block
/// is not in `stranded_labels` at all, since `label_blocks` kept only the later of the two.
fn finalize_terminators(
    program: &mut Program,
    fidx: FunctionIdx,
    func_name: &str,
    stranded_labels: &HashSet<BasicBlockIdx>,
    body_holds_recovery: bool,
) -> Result<(), Error> {
    // Collect first, patch second: `implicit_return` needs the whole function (it reads the
    // return arity and interns a local), which it cannot have while a block is borrowed.
    let unterminated: Vec<BasicBlockIdx> = program.functions[fidx]
        .blocks
        .iter_enumerated()
        .filter(|(_, data)| data.terminator.is_none())
        .map(|(bb, _)| bb)
        .collect();
    let mut patched: Vec<BasicBlockIdx> = Vec::new();
    for bb in unterminated {
        let term = implicit_return(program, fidx);
        program.functions[fidx].blocks[bb].terminator = Some(term);
        if body_holds_recovery && stranded_labels.contains(&bb) {
            continue;
        }
        patched.push(bb);
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

/// The three things [`function_head`] recovers from a definition's declarator.
struct FunctionHead<'tree> {
    /// The `identifier` naming the function.
    name: Node<'tree>,
    /// Its `parameter_list`.
    params: Node<'tree>,
    /// Whether the declarator wraps the `function_declarator` in at least one
    /// `pointer_declarator`, i.e. the function returns a pointer. `void *f()` returns a
    /// value even though its `type:` is `void` -- the `void` is the pointee.
    returns_pointer: bool,
}

/// One `function_definition` the frontend found, as [`plan_definitions`] sees it: enough to
/// decide whether it is a function of its own, a character-for-character copy of another
/// translation unit's, or a second function that merely shares a name.
#[derive(Debug)]
struct Definition<'a> {
    /// Index of the translation unit it came from, among the units lowered together.
    unit: usize,
    /// Identity of the `function_definition` node within that unit's tree, so the walk can
    /// look its plan up again.
    id: usize,
    /// The IR name it is registered under: the name as written, arity-mangled when that name
    /// is overloaded (C++ only, see [`GrammarHooks::collect_overloads`]; the identity for C).
    name: String,
    /// The definition's source text. This frontend lowers text, so two definitions that
    /// quote the same characters lower to the same IR -- which is what makes deduplicating
    /// them exact rather than a guess.
    text: &'a str,
}

/// What [`plan_definitions`] decided about ONE translation unit's definitions. The first two
/// are keyed by `function_definition` node id; a definition in neither is the ordinary case --
/// the only definition of its name -- and keeps the name it was written with.
#[derive(Debug, Default, Clone)]
struct UnitPlan {
    /// Definitions that repeat, character for character, one another unit already
    /// contributes. The same function, included by two translation units; lowered once, there.
    duplicates: HashSet<usize>,
    /// IR name of a definition that cannot keep its bare name because another definition of
    /// that name got there first.
    effective: HashMap<usize, String>,
    /// For a name several definitions claim, the IR name a reference from THIS unit means.
    /// C resolves a call to the definition the caller's own translation unit holds, so that is
    /// what a reference from here resolves to; a unit that defines none of them is absent
    /// here and falls back to the bare name. Empty unless some name really is claimed twice.
    local_names: HashMap<String, String>,
}

impl UnitPlan {
    /// The IR name of the definition `id`, which is the name it was written with unless this
    /// plan gave it one of its own.
    fn name_of<'n>(&'n self, id: usize, written: &'n str) -> &'n str {
        self.effective.get(&id).map_or(written, String::as_str)
    }
}

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

/// The field of the globals object that names the object at a compile-time constant address --
/// the location `(T *)K` designates, for the constant `K` exactly as it is spelled in the
/// source. `((struct inet_sock *)0)->sk` becomes `$globals.<address 0>.sk`.
///
/// A global rather than a fresh temp per site, because two casts of the same constant designate
/// the same object in C: a store through `*(volatile u32 *)0xfee00300` must be observed at a
/// read of it. Keyed on the constant's *text*, so `0` and `0x0` are two names for what is really
/// one address -- accepted deliberately, since normalizing would mean re-implementing C integer
/// literal semantics (bases, suffixes, character constants) for a distinction the corpus does not
/// draw: all 2,177 sites in the kernel corpus spell it `0`.
///
/// `<...>` is not C identifier syntax, so this can never collide with a global the program
/// declares -- the collision-proofing `next_temp` uses for `<tN>` and spec 090 for
/// `<implicit-return>`.
fn literal_address_path(constant: &str) -> RawPath {
    let mut fields = ThinVec::with_capacity(1);
    fields.push(PathSegment::symbol(format!("<address {constant}>")));
    RawPath::new(VariableRef::new_global(), fields)
}

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

/// What `parse_cpp_program`'s method-discovery hook learns about one class/struct: the
/// data members and inline member-function names it declares. The shared lowering core
/// consults these neutral maps (member resolution, method-call dispatch) but never builds
/// them — only the C++ `collect_aux` hook populates [`Context::classes`], so for C the map
/// is empty and every lookup misses, leaving the C path byte-for-byte unchanged.
#[derive(Debug, Default)]
struct ClassInfo {
    /// Names of the class's data members (resolved to `this.<member>` inside a method).
    members: HashSet<String>,
    /// Names of inline member functions (resolve a `recv.<method>(…)` call to `Class::method`).
    methods: HashSet<String>,
    /// Method names whose call result aliases the receiver object, because the method has a
    /// reference return type and its body returns the receiver (`Class& m(){ … return *this; }`).
    /// A chained `recv.m(…).n(…)` then dispatches `n` on the same object, and `Class& r =
    /// recv.m(…)` binds `r` to it. A subset of [`Self::methods`]; empty for C.
    returns_self: HashSet<String>,
    /// Whether the class declares a (user-defined) constructor, modeled as the function
    /// `Class::Class` with an implicit `this` (`ByRef`) param 0. Set by the C++ method
    /// discovery hook; consulted by the `construct` hook so a class-typed declaration with
    /// arguments (`Box b(args)` etc.) lowers to a `DirectCall Class::Class(&b, args…)`.
    /// Always `false` for C (the `classes` map is empty), so construction never fires there.
    has_ctor: bool,
    /// The class's **direct** base classes (`struct Derived : Base` → `["Base"]`), recorded
    /// from the `base_class_clause` by the C++ discovery hook. A derived class's [`Self::members`]
    /// already include its bases' (flattened transitively at discovery); this list additionally
    /// lets method **dispatch** walk the base chain to the class that defines an inherited method
    /// ([`Context::resolve_method_class`]). Empty for every C class and every base-less C++ class,
    /// so the base-chain walk is a no-op there and the C path is unchanged. Never branched on.
    bases: Vec<String>,
    /// Method names the class declares **`virtual`** (or that `override` a virtual base method) —
    /// the set that makes a call through a base pointer/reference dispatch by class-hierarchy
    /// analysis (CHA) rather than by static type. Recorded per class by the C++ discovery hook
    /// (both the `virtual` keyword and a trailing `override` mark a method here). A call is
    /// virtual on a static type iff some class in its base chain records the method here
    /// ([`Context::method_is_virtual`]). Empty for C and for every non-polymorphic C++ class, so
    /// dispatch stays single-target static there and the C path is unchanged. Never branched on.
    virtual_methods: HashSet<String>,
    /// Names of the class's **`static`** data members — modeled as class-scoped **globals**
    /// (`Class::<member>`) rather than per-object `this.<member>` (spec 015). A subset of
    /// [`Self::members`], recorded per class (its **own** static members, *not* flattened into
    /// subclasses) by the C++ discovery hook. When an unqualified member name inside a method
    /// (static or instance) resolves here, [`Context::build_access_path`] binds it to the global
    /// `<declaring-class>::<member>` — the same key a qualified `Class::<member>` read resolves
    /// to — so taint written through one method is read by another. Empty for C and for every
    /// class with no static members, so the resolution is unchanged there. Never branched on.
    static_members: HashSet<String>,
    /// Whether the class declares its **own** destructor `~Class(){…}` — lowered as the niladic
    /// `this`-method `Class::~Class` (spec 016). Set by the C++ discovery hook; consulted by the
    /// `delete_expr` hook so `delete p` invokes the destructor of `p`'s pointed-to object (a
    /// `DirectCall` with the referent as arg-0 receiver). This records only the class's *own*
    /// destructor (not an inherited one), so the destructor-call gatherer can find each class
    /// that contributes a `Sub::~Sub` edge to the chain. `false` for C (no destructors) and for
    /// any C++ class without a user destructor, so `delete` stays 014's no-op there.
    has_dtor: bool,
    /// Whether the class's own destructor is declared **`virtual`** (`virtual ~Base(){…}`, or a
    /// `~Derived() override`). A `delete p` through a static type whose destructor is virtual (on
    /// that type or an ancestor) dispatches by class-hierarchy analysis — the subtree's `Sub::~Sub`
    /// destructors — rather than the single static-type destructor (spec 016). `false` for C and
    /// for every non-virtual destructor, so a non-polymorphic `delete` stays single-target.
    dtor_virtual: bool,
}

/// The object a C++ instance-method receiver resolves to: the access-path expression to
/// pass as the arg-0 (`ByRef`) receiver, and the object's class (to look up the method).
/// Produced by [`Context::resolve_recv_obj`] for a plain identifier local, the implicit
/// `this`, or a chained call whose callee returns its receiver.
#[derive(Clone)]
struct RecvObj {
    exp: Exp,
    class: String,
}

/// The outcome of lowering a C++ instance-method (or chained) call in [`Context::dispatch_call`].
struct DispatchOut {
    /// The call's result value (a temp), for use as an ordinary rvalue.
    value: Exp,
    /// When the callee returns a reference to its receiver (`return *this`), the object the
    /// result aliases — so a chained `.n(…)` dispatches on it and `Class& r = call` binds to it.
    aliased: Option<RecvObj>,
}

/// Grammar-shape adapters for the *few* places tree-sitter-cpp and tree-sitter-c expose
/// the same construct under a different node shape. The shared lowering core stays
/// language-neutral (constitution Principle III: no `is_cpp`, no `if <language> {…}` in
/// the shared walker); instead it reads through these hooks. The C frontend installs
/// [`GrammarHooks::C`] (the historical behavior, byte-for-byte) and the `cpp` submodule
/// installs its own (`cpp::CPP_HOOKS`). Each hook is a plain `fn` pointer so `Context`
/// stays `Copy`-free but cheap to clone-free.
#[derive(Debug, Clone, Copy)]
struct GrammarHooks {
    /// Map an `if`/`while`/`switch` `condition` field node to the expression to flatten.
    /// C exposes it directly as a `parenthesized_expression` (identity); C++ wraps it in a
    /// `condition_clause` whose `value` field is the real condition.
    condition_expr: for<'t> fn(Node<'t>) -> Node<'t>,
    /// Map a `subscript_expression` node to its index expression. C uses a direct `index`
    /// field; C++ nests it under an `indices` `subscript_argument_list`.
    subscript_index: for<'t> fn(Node<'t>) -> Node<'t>,
    /// Whether a `function_definition` the shared definition query found belongs to
    /// [`GrammarHooks::collect_aux`] rather than to the shared pass ([`collect_definitions`]
    /// and [`Context::lower_definitions`] both skip it). C has no such definitions, so its hook
    /// is always `false`. C++ answers `true` for a class member (an inline method or
    /// constructor), a function in a named namespace, and an out-of-line member — all of which
    /// `collect_aux` registers and lowers under their qualified names, and none of which the
    /// shared pass could name.
    aux_owns_definition: for<'t> fn(Node<'t>) -> bool,
    /// Discover and lower any *auxiliary* function definitions that the top-level
    /// `function_definition` query in [`Context::lower_definitions`] does not reach. C has
    /// none, so its hook is a no-op. C++ uses it to discover inline instance methods (which
    /// live inside a `class_specifier`/`struct_specifier`, named by a `field_identifier`),
    /// register their members/methods into [`Context::classes`], and lower each method body
    /// via [`Context::lower_function`] with an implicit `this` parameter. Runs *before* the
    /// top-level functions are lowered, so a `recv.method(…)` call can resolve its callee.
    collect_aux: for<'a, 't> fn(
        &mut Context<'a>,
        &'a str,
        Node<'t>,
        &mut Program,
        usize,
    ) -> anyhow::Result<(), Error>,
    /// Try to lower a *declaration* as an **object construction** (C++ only). The shared
    /// [`Context::walk_declaration`] calls this for a declaration whose type names a known
    /// class (so it is never invoked for C, where [`Context::classes`] is empty). C++ uses
    /// it to recognize `Box b(args)` / `Box b = Box(args)` / `Box b{args}` — a construction
    /// of a class that declares a constructor — and lower it to a `DirectCall Class::Class`
    /// with the new object as the arg-0 (`ByRef`) receiver, so the constructor's member
    /// writes flow back into the object. Returns `true` if it recognized and fully lowered
    /// the declaration (the caller then skips its normal declarator handling), `false`
    /// otherwise (a plain `Box b;`, a non-class type, or a class with no constructor —
    /// handled normally). The C hook always returns `false`.
    construct: for<'a, 't, 's> fn(
        &mut Context<'a>,
        &'a str,
        &mut Program,
        &'s mut ScopeView,
        Node<'t>,
        &'s str,
    ) -> anyhow::Result<bool, Error>,
    /// Populate the neutral arity-overload map that becomes every unit's
    /// [`Context::overloads`] — *before* any function is registered or lowered. C has no
    /// overloading, so its hook is a no-op (the map stays empty and [`Context::overload_name`]
    /// is the identity). C++ uses it to scan every `function_definition` (free, namespaced, and
    /// member), grouping by IR base name and recording each definition's explicit-parameter
    /// arity, so the mangler can tell — at all four touchpoints, definition-side and call-side
    /// alike — which names are overloaded. [`lower_units`] runs it over every unit first, ahead
    /// of [`collect_definitions`] (which registers a definition under its mangled name) and of
    /// `collect_aux`, so the map is complete before the first name is registered. A plain
    /// function rather than a `Context` method because no context exists yet at that point.
    collect_overloads: for<'t> fn(
        &str,
        Node<'t>,
        &mut HashMap<String, HashSet<usize>>,
    ) -> anyhow::Result<(), Error>,
    /// Lower a C++ `delete p;` — destroying a heap object runs its destructor(s) (spec 016).
    /// The shared `delete_expression` arm calls this and then yields a throwaway temp for the
    /// expression's (void) value. C has no `delete`, so its hook is a **no-op** — `delete` never
    /// occurs in a C tree and the arm is inert for C, exactly as in spec 014. C++ uses it to emit
    /// the destructor call: when the pointer's static class (or a base) declares a destructor, a
    /// `DirectCall` over the static type's subtree destructors for a **virtual** destructor (CHA),
    /// or the single static-type destructor otherwise, with the referent `*p` as the arg-0
    /// (`ByRef`) receiver — so taint a destructor body moves at `delete` time is captured. A
    /// hierarchy with no destructor emits nothing (014's no-op). Driven by the neutral
    /// `classes`/`subclasses`/`local_types` maps (empty for C), so no language branch.
    delete_expr: for<'a, 't> fn(
        &mut Context<'a>,
        &mut Program,
        Node<'t>,
        &'a str,
        &mut ScopeView,
    ) -> anyhow::Result<(), Error>,
    /// Emit the destructors of the class-typed **stack** (automatic) objects constructed in a
    /// scope, at that scope's **normal fall-through exit** — a stack object's destructor runs at
    /// the closing `}` (spec 017). The shared [`Context::walk_compound_statement`] calls this after
    /// the statement loop (before the end-of-compound link) when the compound did **not** diverge
    /// (a `return`/`break`/`continue` in the middle is this slice's out-of-scope early-exit case).
    /// C has no destructors, so its hook is a **no-op**. C++ drains the current scope's destructor
    /// frame ([`Context::dtor_frames`]) in **reverse** construction order (LIFO) and, for each
    /// object whose class (or a base) declares a destructor, emits a single-target exact-type
    /// destructor `DirectCall` (a stack object's dynamic type equals its declared type, so no CHA)
    /// with the object itself as the arg-0 (`ByRef`) receiver. The frame is empty for C, so this is
    /// inert on the C path. No language branch: driven by the neutral `classes`/`dtor_frames` state.
    scope_exit: for<'a> fn(&mut Context<'a>, &mut Program, &ScopeView) -> anyhow::Result<(), Error>,
    /// Emit whatever a function's *prologue* contributes before its body is walked. C has
    /// nothing there, so its hook is a **no-op**. C++ uses it for a constructor's
    /// member-initializer list (`Box(int x) : v(x)`), lowering each `member(expr)` to a
    /// `this.<member> = <expr>` store in initialization order. The pairs are gathered by the
    /// C++ discovery hook and handed straight back here, so the shared core neither builds nor
    /// interprets them — it only decides *when* the prologue runs. Always empty for a free
    /// function and for C, so nothing is emitted on the C path.
    ctor_prologue: for<'a, 't> fn(
        &mut Context<'a>,
        &mut Program,
        &mut ScopeView,
        &'a str,
        &[(String, Node<'t>)],
    ) -> anyhow::Result<(), Error>,
}

impl GrammarHooks {
    /// The C frontend's behavior — the long-standing default. `parse_c_program` runs with
    /// these, so its lowering is unchanged.
    const C: GrammarHooks = GrammarHooks {
        condition_expr: |n| n,
        subscript_index: |n| {
            n.child_by_field_name("index")
                .expect("C subscript_expression always has an `index` field")
        },
        // C has no functions hidden from the top-level query: no methods, no classes.
        collect_aux: |_ctx, _source, _root, _program, _global_sidx| Ok(()),
        // C has no classes/constructors, so no declaration is ever a construction.
        construct: |_ctx, _source, _program, _scope_view, _node, _class| Ok(false),
        // C has no overloading: the `overloads` map stays empty and `overload_name` is the
        // identity, so every C (and every non-overloaded) name is registered/resolved bare.
        collect_overloads: |_source, _root, _overloads| Ok(()),
        // C has no definitions the shared pass cannot own: no methods, no namespaces.
        aux_owns_definition: |_def| false,
        // C has no `delete_expression` (nor destructors), so this is never invoked on the C path;
        // the no-op keeps `delete` a taint no-op exactly as spec 014 left it.
        delete_expr: |_ctx, _program, _node, _source, _scope_view| Ok(()),
        // C constructs no class objects, so every destructor frame is empty and no scope exits ever
        // emit a destructor — a no-op, exactly as before spec 017.
        scope_exit: |_ctx, _program, _scope_view| Ok(()),
        // C has no constructors, so no function has a prologue to emit — a no-op, and the
        // slice handed to it is always empty on the C path anyway.
        ctor_prologue: |_ctx, _program, _scope_view, _source, _inits| Ok(()),
    };
}

#[derive(Debug)]
struct Context<'a> {
    /// Every function this import knows, by IR name: the definitions of EVERY translation
    /// unit, registered by [`lower_units`] before any unit is lowered, so a reference to a
    /// function another unit defines (`fp = later;`) is recognised as one. The name is the one
    /// written in the source except where [`plan_definitions`] had to mint one, so an extern
    /// declaration, a call from a unit that defines nothing of that name, and every taint model
    /// still resolve by the plain name.
    functions: HashMap<String, FunctionIdx>,
    param_names: HashMap<String, IndexVec<ParameterIdx, &'a str>>,
    /// What [`plan_definitions`] decided about this unit's definitions, and what a colliding
    /// name means from here. See [`UnitPlan`].
    unit_plan: UnitPlan,
    scope_tree: ScopeTree,
    allocator: TempAllocator,
    /// Block that each `goto` label maps to, for the function currently being walked.
    /// Labels are function-scoped and can be jumped to before they are defined, so
    /// (unlike `break`/`continue` targets, which ride on `ScopeView`) the blocks are
    /// created in a pre-scan over the whole body and looked up here. Reset per function.
    label_blocks: HashMap<String, BasicBlockIdx>,
    /// The tree-sitter grammar this lowering is driving, supplied by the frontend entry
    /// point (`parse_c_program` passes C; the `cpp` submodule's `parse_cpp_program` passes
    /// C++). The shared lowering is language-neutral and never branches on the language;
    /// this handle is used only to compile queries against the same grammar that parsed
    /// the tree (queries match by numeric symbol id, which differs between grammars). Any
    /// C++-specific behavior belongs in the `cpp` submodule, not in branches here.
    grammar: tree_sitter::Language,
    /// Grammar-shape adapters for the handful of node-shape divergences between
    /// tree-sitter-c and tree-sitter-cpp. Defaults to [`GrammarHooks::C`]; the `cpp`
    /// submodule hands [`lower_units`] its own (`cpp::CPP_HOOKS`). The shared walker reads
    /// conditions and subscript indices through these instead of branching on language.
    hooks: GrammarHooks,
    /// Classes/structs and their members + inline methods, keyed by class name. Populated
    /// **only** by the C++ `collect_aux` hook (method discovery); empty for C. The shared
    /// lowering reads it for member resolution (`this.<member>`) and method-call dispatch —
    /// a data-driven lookup that misses on C, so the C path is unchanged. Never branched on.
    classes: HashMap<String, ClassInfo>,
    /// Maps a local variable to the name of its class type, for the function currently being
    /// walked. Filled in [`Context::walk_declaration`] when a declaration's type names a known
    /// class (so it only ever has entries under C++); used by `collect_call` to dispatch
    /// `recv.method(…)`. Reset per function in [`Context::lower_function`].
    local_types: HashMap<String, String>,
    /// The class whose method body is currently being lowered, or `None` for a free function.
    /// Set in [`Context::lower_function`] from the implicit-`this` argument. Member resolution
    /// in `build_access_path` consults this (plus [`Context::classes`]) to rewrite an
    /// unqualified member name to `this.<member>`. `None` for every C function.
    current_method_class: Option<String>,
    /// C++ reference locals (`T& r = x`) map their name to the referent's access path, for
    /// the function currently being lowered. A reference *is* its referent's storage, so a
    /// use of `r` resolves to `x`'s path (with any trailing fields appended) rather than a
    /// copy — this is the alias [`Context::build_access_path`] applies before scope lookup.
    /// Filled in [`Context::walk_declaration`]; only ever populated under C++ (C has no
    /// `reference_declarator`), so it is empty for C and the C path is unchanged. Reset per
    /// function in [`Context::lower_function`].
    reference_aliases: HashMap<String, RawPath>,
    /// Overloaded-by-arity names: an IR **base name** (a free function's bare/qualified name
    /// `f` / `ns::f`, or a method's qualified `Class::method`) mapped to the set of explicit
    /// **arities** (parameter counts, `this` excluded) defined under it. A name is *overloaded*
    /// iff its arity set has **≥2** members; those are lowered — and their calls resolved —
    /// under an arity-mangled name (`f#1`, `f#2`, `Box::area#0`) so a call reaches exactly the
    /// arity-matching overload. Populated **only** by the C++ `collect_overloads` hook
    /// (`cpp::cpp_discover_overloads`); **empty for C**, so [`Self::overload_name`] is the
    /// identity there and the C path — and every non-overloaded C++ name — is unchanged. Never
    /// branched on: the four touchpoints just consult it through the neutral mangler.
    overloads: HashMap<String, HashSet<usize>>,
    /// The class-hierarchy in the **subclass** direction — a class mapped to its **direct**
    /// subclasses (the reverse of each [`ClassInfo::bases`] edge). Built once by the C++
    /// discovery hook after every class is known, it lets a virtual call over-approximate by
    /// CHA: [`Context::cha_targets`] walks a static type's subclass subtree to gather every
    /// override. Populated **only** under C++ (empty for C, where `classes` is empty), and read
    /// only when a call is virtual — so it is a no-op on the C path and for non-polymorphic C++.
    /// Never branched on.
    subclasses: HashMap<String, Vec<String>>,
    /// A stack of **destructor frames**, one per lexical scope currently being walked
    /// ([`Context::walk_compound_statement`] pushes on entry, pops on exit). Each frame holds the
    /// `(object-name, class-name)` of every class-typed **stack** (automatic) object constructed in
    /// that scope, in construction order; at the scope's normal fall-through exit the C++
    /// `scope_exit` hook drains the top frame in **reverse** order and emits each object's
    /// destructor (spec 017). Recorded by the two birthplaces of a stack class object — the value
    /// declarator branch of [`Context::walk_declaration`] (default/copy init, `Widget w;`) and
    /// `cpp::emit_construction` (constructed, `Widget w(args)`) — both gated on the neutral
    /// `classes` map (empty for C). A heap `new` object is *not* recorded (it flows through
    /// `emit_ctor_call` directly), so it gets no scope-exit destructor. Empty for C (which
    /// constructs no class objects), so the C path never emits a scope-exit destructor. Never
    /// branched on.
    dtor_frames: Vec<Vec<(String, String)>>,
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
    /// Builder that interns source spans, or `None` when spans are not being recorded (the
    /// unit-test path). [`lower_units`] threads one builder through every unit's context, so
    /// the imported IR carries locations back to each unit's file.
    source_info: Option<SourceInfoBuilder>,
    /// The artifact spans in this unit are recorded under, with the unit's length in bytes so
    /// a span can be clamped to it; `None` when spans are not recorded.
    unit_key: Option<(ArtifactKey, usize)>,
    /// Span attached to every IR statement emitted while lowering the C statement currently
    /// being walked. Set once per source statement in [`Context::walk_statement`] so that all
    /// the IR it expands into (calls, loads, stores) points back at that statement.
    cur_span: SourceInfo,
    /// **Record layout registry**: a record tag mapped to its data members in declaration
    /// order. Filled once per translation unit by [`Context::collect_type_registry`], before
    /// any function is lowered, so a member's own type is available regardless of declaration
    /// order. Consulted only by [`Context::collect_initializer_list`], to map a *positional*
    /// brace initializer onto the members it writes. A tag that is absent (anonymous, declared
    /// in another translation unit) simply takes the positional-element fallback, so an
    /// incomplete registry is always safe.
    struct_layouts: HashMap<String, Vec<MemberSlot>>,
    /// **Type-name registry**: every name this translation unit uses as a type. Filled by the
    /// same pre-pass as `struct_layouts` (see [`Context::collect_type_registry`]), from the
    /// `type_identifier` nodes tree-sitter itself produced -- a `typedef`'s declared name, and
    /// equally a name used as a type in any declaration, which is the only evidence available
    /// for a typedef that lives in a system header the corpus did not preprocess (nginx and
    /// openssh use `u_char` and `uid_t` without ever declaring them).
    ///
    /// It exists to tell a cast from a call: tree-sitter cannot know `__be16` is a type, so
    /// `(__be16)(x)` parses as a `call_expression` through a parenthesized callee -- the exact
    /// shape of a genuine `(fp)(x)`. See [`Context::cast_shaped_call`], which is the only
    /// consumer. A record TAG is deliberately not recorded: `struct stat` is a type but `stat`
    /// alone is not one, and it is the name of a function.
    type_names: HashSet<String>,
    /// Names the translation unit declares as functions without defining them -- prototypes,
    /// `int zzz(int);` -- collected in the same pre-pass as `type_names`. The positive
    /// evidence that `(zzz)(x)` is a call, symmetric to the type-use evidence `type_names`
    /// holds for a cast. Read by [`Context::cast_shaped_call`] only: it keeps the report made
    /// on a name with no evidence either way quiet for the one idiom that legitimately lands
    /// there, `(free)(p)` -- parentheses to suppress a function-like macro -- with libc's
    /// prototype in scope.
    declared_functions: HashSet<String>,
    /// `ERROR` nodes already reported as unparsable constructs, by `Node::id`, so one
    /// syntax error draws one warning. See [`Context::report_unparsable_construct`].
    reported_parse_errors: HashSet<usize>,
    /// Functions whose body was already reported as holding parse-recovery output, so the
    /// notice is one per function rather than one per node the recovery left behind (41,751
    /// of them in the kernel census). See [`Context::report_unanalyzed_recovery`].
    functions_with_recovery: HashSet<String>,
    /// The pre-created label blocks [`Context::walk_labeled_statement`] actually entered.
    /// Reset per function alongside `label_blocks`; its complement within `label_blocks` is
    /// the set of labels the walk never reached, which [`finalize_terminators`] needs in
    /// order to tell a label stranded in parse-recovery output from a genuinely dropped
    /// block.
    walked_label_blocks: HashSet<BasicBlockIdx>,
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
    /// Layout of an **anonymous** inline record member -- `struct { int a; int b; } q;` -- which
    /// has no tag to resolve against the registry. Carrying its members here lets a brace nested
    /// at this position map onto them, exactly as a tagged member's does; without it the nested
    /// brace would fall back to element numbering and write a path (`r.q.deref`) that a later
    /// `r.q.a` read never resolves to, silently dropping the taint. Set only when the member's
    /// type is a record *definition* with no name.
    inline_layout: Option<Vec<MemberSlot>>,
}

impl MemberSlot {
    /// The layout to lower a brace nested at this member's position with: its own inline
    /// members if it is an anonymous record, otherwise its record tag looked up in `registry`.
    fn nested_layout(
        &self,
        registry: &HashMap<String, Vec<MemberSlot>>,
    ) -> Option<Vec<MemberSlot>> {
        self.inline_layout.clone().or_else(|| {
            self.type_tag
                .as_deref()
                .and_then(|tag| registry.get(tag).cloned())
        })
    }
}

/// One translation unit to lower: what the compiler would see for one `.c` file.
#[derive(Debug)]
struct TranslationUnit {
    /// Display path, or `None` for an in-memory unit (the [`parse_c_program`] path used by
    /// tests), which has no file to name.
    path: Option<String>,
    /// SHA-256 of `source`, matching the store's artifact-hash scheme.
    hash: Vec<u8>,
    source: String,
}

impl TranslationUnit {
    fn new(path: Option<String>, source: String) -> Self {
        let hash = source_info::sha256(source.as_bytes());
        Self { path, hash, source }
    }

    /// The artifact spans in this unit are recorded under; `None` for an in-memory unit.
    fn artifact_key(&self) -> Option<ArtifactKey> {
        self.path.as_ref().map(|path| ArtifactKey {
            path: path.clone(),
            sub_artifact_id: 0,
            hash: self.hash.clone(),
            encoding: ArtifactEncoding::Utf8,
        })
    }
}

/// The neutral overload mangler behind [`Context::overload_name`], usable before any context
/// exists: [`collect_definitions`] registers a definition under the name this yields. If
/// `base` names an **overloaded** entity — present in `overloads` with **≥2** distinct
/// arities — the arity-mangled IR name `base#arity`; otherwise `base` unchanged.
fn mangle_overload(
    overloads: &HashMap<String, HashSet<usize>>,
    base: &str,
    arity: usize,
) -> String {
    match overloads.get(base) {
        Some(arities) if arities.len() >= 2 => format!("{base}#{arity}"),
        _ => base.to_string(),
    }
}

impl Context<'_> {
    /// Create a lowering context driving `grammar` through `hooks`. [`lower_units`] builds one
    /// per translation unit this way — the C grammar with [`GrammarHooks::C`] for
    /// `parse_c_program`/`import_c`, the C++ grammar with `cpp::CPP_HOOKS` for
    /// `parse_cpp_program` — so the shared lowering stays language-neutral.
    fn new(grammar: tree_sitter::Language, hooks: GrammarHooks) -> Self {
        Self {
            functions: HashMap::default(),
            param_names: HashMap::default(),
            unit_plan: UnitPlan::default(),
            scope_tree: ScopeTree::default(),
            allocator: TempAllocator::default(),
            label_blocks: HashMap::default(),
            grammar,
            hooks,
            classes: HashMap::default(),
            local_types: HashMap::default(),
            current_method_class: None,
            reference_aliases: HashMap::default(),
            overloads: HashMap::default(),
            subclasses: HashMap::default(),
            dtor_frames: Vec::new(),
            addr_alias: HashMap::default(),
            union_vars: HashSet::default(),
            source_info: None,
            unit_key: None,
            cur_span: SourceInfo::default(),
            struct_layouts: HashMap::default(),
            type_names: HashSet::default(),
            declared_functions: HashSet::default(),
            reported_parse_errors: HashSet::default(),
            functions_with_recovery: HashSet::default(),
            walked_label_blocks: HashSet::default(),
        }
    }

    /// The neutral overload mangler consulted at all four overloading touchpoints (free- and
    /// method-function registration, free-call and method-dispatch edges). If `base` names an
    /// **overloaded** entity — present in [`Self::overloads`] with **≥2** distinct arities — it
    /// returns the arity-mangled IR name `base#arity`; otherwise it returns `base` unchanged.
    /// Because [`Self::overloads`] is empty for C (and holds a single arity for every
    /// non-overloaded C++ name), this is the **identity** for all of C and for every ordinary
    /// non-overloaded call — so wiring it into a touchpoint changes only genuinely overloaded
    /// names, and never introduces a language branch. See [`mangle_overload`].
    fn overload_name(&self, base: &str, arity: usize) -> String {
        mangle_overload(&self.overloads, base, arity)
    }

    /// Compile a tree-sitter query against this context's grammar, so a tree produced by
    /// this context is queried with the grammar that parsed it (see [`compile_query_for`]).
    fn compile_query(&self, query_src: &str) -> Query {
        compile_query_for(&self.grammar, query_src)
    }
}

struct MatchExtractor<'q, 'cursor, 'tree> {
    query: &'q Query,
    m: &'cursor QueryMatch<'cursor, 'tree>,
}

impl<'query, 'cursor, 'tree> MatchExtractor<'query, 'cursor, 'tree> {
    fn new(query: &'query Query, m: &'cursor QueryMatch<'cursor, 'tree>) -> Self {
        Self { query, m }
    }

    fn get(&self, name: &str) -> Result<Node<'tree>, Error> {
        let r = self.get_opt(name);
        if let Some(result) = r {
            Ok(result)
        } else {
            Err(Error::TreeSitterParse(format!(
                "Query failed to find mandatory capture: @{name}"
            )))
        }
    }

    fn get_opt(&self, name: &str) -> Option<Node<'tree>> {
        self.m
            .captures
            .iter()
            .find(|c| self.query.capture_names()[c.index as usize] == name)
            .map(|c| c.node)
    }
}
fn inject_explainers_into_ir(ir_text: &str, views: &[ScopeView]) -> String {
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

/// Parse the C source in `source` -- one in-memory translation unit -- into a CTADL IR
/// program. Returns the program, whether tree-sitter reported syntax errors, and the
/// marked-up dump. No spans are recorded and no extern stubs are created: this is the
/// unit-test path, and a test that needs the stubs goes through [`parse_c_files`].
pub fn parse_c_program(source: &str) -> anyhow::Result<(Program, bool, String), Error> {
    let units = [TranslationUnit::new(None, source.to_string())];
    let lowered = lower_units(
        &units,
        tree_sitter_c::LANGUAGE.into(),
        GrammarHooks::C,
        false,
        false,
        true,
    )?;
    Ok((lowered.program, lowered.has_error, lowered.marked_up))
}

/// Parse several named C files, each a translation unit of its own, into one program -- the
/// shape [`import_c`] produces for a directory -- and return it with its marked-up dump.
///
/// The import path, not [`parse_c_program`]: it records spans and it creates the extern
/// stubs. That is what a test needs in order to pin anything about several translation units
/// meeting in one program, which is where a `static` function's file scope stops being
/// implicit (spec 120).
pub fn parse_c_files(files: &[(String, String)]) -> Result<(Program, String), Error> {
    let units: Vec<TranslationUnit> = files
        .iter()
        .map(|(path, contents)| TranslationUnit::new(Some(path.clone()), contents.clone()))
        .collect();
    let lowered = lower_units(
        &units,
        tree_sitter_c::LANGUAGE.into(),
        GrammarHooks::C,
        true,
        true,
        true,
    )?;
    Ok((lowered.program, lowered.marked_up))
}

/// Import C source at `path` into a [`ProgramInfo`], ready for [`crate::cli::import`].
///
/// `path` may be a single C source file (`.c`) or header (`.h`), or a directory tree
/// containing such files. Every file is a translation unit of its own: each is parsed
/// separately, and all of them lower into one program, where a reference from one unit to a
/// function another defines resolves to that definition (see [`lower_units`]).
///
/// The frontend expects post-preprocessor C source: `#include` directives are not expanded
/// here, so each file should already be a complete translation unit. A header found in the
/// directory is lowered for the definitions it holds (a `static inline`), not prepended to
/// the `.c` files -- their preprocessed forms already contain it.
pub fn import_c(path: &std::path::Path) -> Result<ProgramInfo, Error> {
    let units = read_c_units(path)?;
    let lowered = lower_units(
        &units,
        tree_sitter_c::LANGUAGE.into(),
        GrammarHooks::C,
        true,
        true,
        false,
    )?;
    if lowered.has_error {
        log::warn!(
            "tree-sitter reported syntax errors while parsing C source at '{}'; \
             the imported IR may be incomplete (is the input already preprocessed?)",
            path.display()
        );
    }
    Ok(ProgramInfo {
        program: lowered.program,
        source_info: lowered
            .source_info
            .expect("spans are recorded on the import path"),
        ..Default::default()
    })
}

/// What [`lower_units`] produced.
struct Lowered {
    program: Program,
    /// The recorded spans, when they were asked for.
    source_info: Option<source_info::SourceInfo>,
    /// Whether tree-sitter reported a syntax error in any unit.
    has_error: bool,
    /// The IR dump with each block's scope explainer, when `dump` asked for it -- tests read
    /// it; the import path does not, and a full render of a corpus is not free.
    marked_up: String,
}

/// Lower `units` into one program: the compiler-and-linker split, in miniature.
///
/// Each unit is parsed on its own and lowered by a [`Context`] of its own -- its type names,
/// prototypes, record layouts and scopes are its own, as C says they are, and a syntax error
/// in one unit cannot re-parent what follows it in another. What the units share is the
/// program, whose function table is one namespace. So before any body is lowered every
/// unit's definitions are collected and [`plan_definitions`] decides what each one is -- a
/// function of its own, a copy of one another unit already contributes, or a second function
/// that merely shares a name and needs an IR name of its own -- and every surviving name is
/// registered up front, so a reference from one unit to a function another defines (a call,
/// or `fp = later;`) resolves the way a linker would resolve it.
///
/// With `record_spans`, every IR statement carries a span into its unit's file; with
/// `extern_stubs`, called-but-undefined functions get an empty definition at the end so taint
/// models can match them by name. The import path wants both; the unit-test path neither.
/// With `dump`, the marked-up IR text is rendered too (tests read it; imports skip it).
///
/// `language` is the grammar every unit is parsed with and `hooks` the adapters for its node
/// shapes ([`GrammarHooks`]): C for [`parse_c_program`]/[`import_c`], C++ for
/// `cpp::parse_cpp_program`. The lowering itself never branches on which.
fn lower_units(
    units: &[TranslationUnit],
    language: tree_sitter::Language,
    hooks: GrammarHooks,
    record_spans: bool,
    extern_stubs: bool,
    dump: bool,
) -> Result<Lowered, Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("error loading grammar");
    let trees: Vec<Tree> = units
        .iter()
        .map(|unit| {
            parser
                .parse(&unit.source, None)
                .expect("tree-sitter failed to parse")
        })
        .collect();
    let has_error = trees.iter().any(|tree| tree.root_node().has_error());

    // Overloads first (C++ only; C's hook records nothing): an overloaded name's definitions
    // are registered under their arity-mangled names, so the mangler must know every arity
    // before `collect_definitions` names anything.
    let mut overloads: HashMap<String, HashSet<usize>> = HashMap::default();
    for (unit, tree) in units.iter().zip(&trees) {
        (hooks.collect_overloads)(&unit.source, tree.root_node(), &mut overloads)?;
    }

    // The link step: what is defined where, and what each definition is.
    let query = function_definition_query(&language);
    let mut defs: Vec<Definition<'_>> = Vec::new();
    for (index, (unit, tree)) in units.iter().zip(&trees).enumerate() {
        collect_definitions(
            index,
            &unit.source,
            tree,
            &query,
            &overloads,
            hooks,
            &mut defs,
        )?;
    }
    let mut plans = plan_definitions(units, &defs)?;
    // Names first: a positional brace initializer in any body needs the layout of a record
    // that may be declared anywhere in its unit, and telling a cast from a call needs the
    // type names and prototypes of every unit (see [`UnitRegistry`]).
    let mut registries: Vec<UnitRegistry> = Vec::with_capacity(units.len());
    let mut type_names: HashSet<String> = HashSet::new();
    let mut declared_functions: HashSet<String> = HashSet::new();
    for (unit, tree) in units.iter().zip(&trees) {
        let mut registry = UnitRegistry::default();
        collect_registry(&unit.source, tree.root_node(), &mut registry);
        type_names.extend(registry.type_names.iter().cloned());
        declared_functions.extend(registry.declared_functions.iter().cloned());
        registries.push(registry);
    }
    let mut program = Program::default();
    let mut functions: HashMap<String, FunctionIdx> = HashMap::new();
    for def in &defs {
        let plan = &plans[def.unit];
        if plan.duplicates.contains(&def.id) {
            continue;
        }
        let name = plan.name_of(def.id, &def.name);
        if !functions.contains_key(name) {
            let fidx = program.new_function();
            functions.insert(name.to_string(), fidx);
        }
    }

    // Lowering: one context per unit, all into the one program.
    let mut builder = record_spans.then(|| SourceInfoBuilder::new(ArtifactMetadata::new()));
    let mut views: Vec<ScopeView> = Vec::new();
    for (index, (unit, tree)) in units.iter().zip(&trees).enumerate() {
        let mut ctx = Context {
            functions: functions.clone(),
            unit_plan: std::mem::take(&mut plans[index]),
            struct_layouts: std::mem::take(&mut registries[index].struct_layouts),
            type_names: type_names.clone(),
            declared_functions: declared_functions.clone(),
            unit_key: if record_spans {
                unit.artifact_key().map(|key| (key, unit.source.len()))
            } else {
                None
            },
            source_info: builder.take(),
            overloads: overloads.clone(),
            ..Context::new(language.clone(), hooks)
        };
        ctx.toplevel(&unit.source, tree, &mut program, &query)?;
        builder = ctx.source_info.take();
        if dump {
            views.append(&mut ctx.scope_tree.blocks);
        }
    }
    if extern_stubs {
        define_extern_functions(&mut program);
    }
    let marked_up = if dump {
        inject_explainers_into_ir(&program.to_string(), &views)
    } else {
        String::new()
    };
    Ok(Lowered {
        program,
        source_info: builder.map(SourceInfoBuilder::finish),
        has_error,
        marked_up,
    })
}

/// The query that finds every `function_definition`. The declarator is captured whole, not
/// pattern-matched: `_declarator` is a CHOICE in the grammar and a pointer return nests a
/// `pointer_declarator` per `*` around the `function_declarator`, which no single query
/// pattern can follow. [`function_head`] does the unwrapping and rejects the shapes that name
/// no single function.
fn function_definition_query(language: &tree_sitter::Language) -> Query {
    compile_query_for(
        language,
        r#"
            (function_definition
                type: (primitive_type)? @return_type
                declarator: (_) @func.decl
                body: (compound_statement) @body) @func.def
            "#,
    )
}

/// The definitions in unit `unit`, appended to `defs` with their identity: the first half of
/// [`lower_units`]'s link step. A definition whose declarator names no single function is
/// reported here -- dropping it is still dropping a body, so it is said out loud rather than
/// left to be noticed as a bodyless `define`, which is exactly how the whole
/// pointer-returning class hid for as long as it did -- and not collected.
fn collect_definitions<'a>(
    unit: usize,
    source: &'a str,
    tree: &Tree,
    query: &Query,
    overloads: &HashMap<String, HashSet<usize>>,
    hooks: GrammarHooks,
    defs: &mut Vec<Definition<'a>>,
) -> Result<(), Error> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        let extract = MatchExtractor::new(query, m);
        let (Ok(decl_node), Ok(def_node)) = (extract.get("func.decl"), extract.get("func.def"))
        else {
            continue;
        };
        // A definition the grammar's auxiliary pass owns -- a C++ class member or a
        // namespaced function, which `collect_aux` registers and lowers under its qualified
        // name -- is not one of this unit's free definitions. Never true for C.
        if (hooks.aux_owns_definition)(def_node) {
            continue;
        }
        let Some(head) = function_head(decl_node) else {
            let (quote, elided) = quote_construct(to_str(&decl_node, source));
            let elision = if elided > 0 {
                format!(" (+{elided} chars elided)")
            } else {
                String::new()
            };
            unexpected_ast(format!(
                "unsupported declarator in a function definition{elision}; the function it \
                 defines has no body in the IR -- declarator: {quote}"
            ))?;
            continue;
        };
        defs.push(Definition {
            unit,
            id: def_node.id(),
            name: mangle_overload(
                overloads,
                to_str(&head.name, source),
                param_arity(head.params),
            ),
            text: to_str(&def_node, source),
        });
    }
    Ok(())
}

/// Decide what each definition *is*, before any of them is lowered: the second half of
/// [`lower_units`]'s link step, and the reason it exists.
///
/// A name is not an identity here. The program's function table is one namespace, so the
/// definitions of every translation unit arrive in it together, and two things that C keeps
/// apart collide in it:
///
/// * a header's `static inline` is one definition per translation unit that included it
///   -- 137,596 of the Linux corpus's 148,001 definitions are repeats of this kind, and
///   1,294 of nginx's 2,308;
/// * a `static` helper is scoped to its own file, so two files may each define their own
///   `parse_dest_constraint`, and a corpus of several programs has one `main` per program
///   (openssh: 70 names with more than one definition, 21 of them `main`).
///
/// Keying the function table on the name alone lowered every one of those into ONE
/// `FunctionData`: parameter lists concatenated, the return arity of whichever body was
/// walked last imposed on all of them, and every call site resolving to the chimera --
/// silently, with no warning of any attribution (spec 120).
///
/// So: definitions that quote the same characters are the same function. This frontend
/// lowers *text*, so N copies of one header's inline produce N identical bodies; keeping
/// the first and dropping the rest is exact, not an approximation. What is left after
/// that really is several functions sharing a name, and each gets a name of its own,
/// `g$ssh-agent.c`, saying which file it came from. The first keeps the bare `g`, so an
/// extern declaration, a call from a unit that defines none of them, and every taint
/// model still resolve; a unit's [`UnitPlan::local_names`] sends a reference from a unit
/// that *does* have its own definition to that one instead.
fn plan_definitions(
    units: &[TranslationUnit],
    defs: &[Definition<'_>],
) -> Result<Vec<UnitPlan>, Error> {
    let mut plans: Vec<UnitPlan> = vec![UnitPlan::default(); units.len()];
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, def) in defs.iter().enumerate() {
        by_name.entry(def.name.as_str()).or_default().push(i);
    }
    // Every name as written is spoken for: a minted name must never shadow a real one.
    let mut used: HashSet<String> = defs.iter().map(|def| def.name.to_string()).collect();
    let mut planned: HashSet<&str> = HashSet::new();

    for def in defs {
        if !planned.insert(def.name.as_str()) {
            continue;
        }
        let group = &by_name[def.name.as_str()];
        if group.len() == 1 {
            continue; // the ordinary case, and the overwhelming majority of names
        }

        // Copies first: `owner[k]` is the definition whose body group member `k` shares.
        let mut owner_of_text: HashMap<&str, usize> = HashMap::new();
        let mut owner: Vec<usize> = Vec::with_capacity(group.len());
        let mut distinct: Vec<usize> = Vec::new();
        for &i in group {
            if let Some(&first) = owner_of_text.get(defs[i].text) {
                plans[defs[i].unit].duplicates.insert(defs[i].id);
                owner.push(first);
            } else {
                owner_of_text.insert(defs[i].text, i);
                distinct.push(i);
                owner.push(i);
            }
        }
        if distinct.len() == 1 {
            log::debug!(
                "`{}` is defined identically in {} translation units; lowering it once",
                def.name,
                group.len()
            );
            continue;
        }

        // What is left is genuinely several functions with one name.
        for (k, &i) in distinct.iter().enumerate().skip(1) {
            let base = match &units[defs[i].unit].path {
                // The base name is the readable half of the path and the half that
                // identifies the translation unit; two files that share one still get
                // distinct names, from the `#n` below.
                Some(file) => format!(
                    "{}${}",
                    def.name,
                    std::path::Path::new(file)
                        .file_name()
                        .map_or(file.as_str(), |f| f.to_str().unwrap_or(file.as_str()))
                ),
                None => format!("{}${}", def.name, k),
            };
            let mut minted = base.clone();
            let mut n = 2;
            while !used.insert(minted.clone()) {
                minted = format!("{base}#{n}");
                n += 1;
            }
            log::debug!(
                "`{}` also names a definition analyzed as `{minted}`",
                def.name
            );
            plans[defs[i].unit].effective.insert(defs[i].id, minted);
        }

        // Which of them a reference from a given unit means.
        for (pos, &i) in group.iter().enumerate() {
            let owner_def = &defs[owner[pos]];
            let effective = plans[owner_def.unit]
                .name_of(owner_def.id, &def.name)
                .to_string();
            let held = plans[defs[i].unit]
                .local_names
                .entry(def.name.to_string())
                .or_insert_with(|| effective.clone());
            if *held != effective {
                // Two DIFFERENT definitions of one name in ONE translation unit is not C
                // -- and not a concatenation artifact either, since they were written
                // that way. Lower both (the second under its minted name, so neither
                // body is thrown away or glued onto the other) and say what a call to
                // the name means here, which is the first one.
                let minted = effective;
                let first = held.clone();
                let place = match &units[defs[i].unit].path {
                    Some(file) => format!("`{file}`"),
                    None => "this translation unit".to_string(),
                };
                malformed_source(format!(
                    "`{}` is defined more than once in {place}; C gives a name one \
                     definition per translation unit, so this one is analyzed as \
                     `{minted}` and a call to `{}` here resolves to `{first}`",
                    def.name, def.name
                ))?;
            }
        }
    }
    Ok(plans)
}

/// The translation units under `path` for [`import_c`]: the file itself, or every `.c` and
/// `.h` file under a directory, in sorted path order.
fn read_c_units(path: &std::path::Path) -> Result<Vec<TranslationUnit>, Error> {
    let mut files = Vec::new();
    if path.is_dir() {
        collect_c_files(path, &mut files)?;
        if files.is_empty() {
            return Err(Error::Path {
                message: format!("no .c or .h files found under '{}'", path.display()),
            });
        }
        files.sort();
    } else {
        files.push(path.to_path_buf());
    }
    files
        .into_iter()
        .map(|file| {
            let contents = std::fs::read_to_string(&file)?;
            Ok(TranslationUnit::new(
                Some(file.display().to_string()),
                contents,
            ))
        })
        .collect()
}

/// Recursively collect the `.c` and `.h` files under `dir`. Any other file is ignored.
fn collect_c_files(
    dir: &std::path::Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_c_files(&path, files)?;
        } else if metadata.is_file()
            && matches!(path.extension().and_then(|e| e.to_str()), Some("c" | "h"))
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Compile a tree-sitter query against an explicit grammar: the C frontend's for a C tree,
/// the C++ frontend's for a C++ tree ([`Context::compile_query`] picks the context's own). A
/// query must be compiled against the same grammar that parsed the tree it runs over —
/// tree-sitter matches by numeric symbol id, and the C and C++ grammars assign different ids
/// to the same node kind.
fn compile_query_for(language: &tree_sitter::Language, query_src: &str) -> Query {
    Query::new(language, query_src).unwrap_or_else(|e| {
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
        let member_ty = field_decl.child_by_field_name("type");
        let ty_tag = declaration_type_tag(field_decl, source);
        // An *anonymous* inline record member (`struct { int a; } q;`) has no tag to record, so
        // its layout is carried on the slot itself. Only when the type is nameless: a tagged
        // definition is already reachable through `ty_tag`.
        let inline_layout = member_ty
            .filter(|ty| ty.child_by_field_name("name").is_none())
            .and_then(|ty| record_member_slots(ty, source));
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
                    inline_layout: if is_plain {
                        inline_layout.clone()
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

/// What a translation unit's declarations say about names -- the evidence [`Context`] lowers
/// with, collected by [`collect_registry`] before any function is.
///
/// The record layouts are the unit's own: a tag names whatever struct THIS unit declares
/// under it. The two name sets are unioned across every unit of an import by [`lower_units`]:
/// the units of one program share the headers the corpus did not preprocess, so a name one
/// unit uses as a type (`u_int`, from `<sys/types.h>`) is a type in a unit that only casts to
/// it, and a prototype in one unit says what the name is in all of them -- the same reasoning
/// that makes [`Context::functions`] import-wide.
#[derive(Debug, Default)]
struct UnitRegistry {
    struct_layouts: HashMap<String, Vec<MemberSlot>>,
    type_names: HashSet<String>,
    declared_functions: HashSet<String>,
}

/// Fill `registry` from the subtree under `node`: the pre-pass of [`lower_units`] that gives
/// [`Context`] its evidence about names before any function is lowered.
///
/// Fill the two registries a translation unit's *types* provide, in one walk.
///
/// [`Context::struct_layouts`]: for every record definition in the translation unit, its
/// data members in declaration order, keyed by the tag a declaration can name it with. Two
/// spellings are recorded:
/// - a **tagged** definition (`struct P { ... };`) under its tag;
/// - a **typedef** of a definition (`typedef struct { ... } P;`) under the typedef name,
///   which is how an otherwise-anonymous record becomes nameable.
///
/// [`Context::type_names`]: every name used as a type anywhere in the unit, read off the
/// `type_identifier` nodes tree-sitter produced (see [`is_type_name`] for the one kind of
/// `type_identifier` that is *not* a type name).
///
/// A recursive node walk rather than a tree-sitter query, because the record kinds differ
/// per grammar (`class_specifier` exists only in C++, and a query naming it would not
/// compile against the C grammar). Matching on `kind()` is neutral by construction: a kind
/// a grammar does not have simply never occurs. A layout that could not be read completely
/// is **not** recorded (see [`record_member_slots`]), so positional mapping is only ever
/// attempted where every slot is known.
fn collect_registry(source: &str, node: Node<'_>, registry: &mut UnitRegistry) {
    if is_type_name(node) {
        registry
            .type_names
            .insert(to_str(&node, source).to_string());
    }
    if node.kind() == "declaration" {
        // A prototype, `int zzz(int);`. Only function-shaped declarators count:
        // `void (*fp)(int);` declares a variable, and `function_head` says no to it.
        let mut dcursor = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut dcursor) {
            if let Some(head) = function_head(declarator) {
                registry
                    .declared_functions
                    .insert(to_str(&head.name, source).to_string());
            }
        }
    }
    if let Some(slots) = record_member_slots(node, source) {
        // `struct P { ... }` -- nameable by its own tag.
        if let Some(name) = node.child_by_field_name("name") {
            registry
                .struct_layouts
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
                    registry
                        .struct_layouts
                        .insert(to_str(&declarator, source).to_string(), slots.clone());
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_registry(source, child, registry);
    }
}

/// What a `function_definition`'s declarator says about the function it defines: the node
/// that names it, its parameter list, and whether the value it returns is a pointer.
///
/// tree-sitter-c's `function_definition` accepts any `_declarator`, and a pointer return wraps
/// the `function_declarator` in one `pointer_declarator` per `*`, so `char **argv_of(void) {}`
/// parses as `function_definition > pointer_declarator > pointer_declarator >
/// function_declarator`. A query pattern cannot recurse, so [`function_definition_query`]
/// captures the declarator whole and this unwraps it -- for years the pattern demanded a
/// `function_declarator` directly, and every pointer-returning definition in every corpus was
/// dropped without a word (spec 130).
///
/// `None` for a declarator that names no single function: a definition made through a
/// parenthesized declarator (`char *(*f(int))(void)`, a function returning a function
/// pointer), an array or attributed declarator, or parse-recovery debris.
fn function_head(declarator: Node<'_>) -> Option<FunctionHead<'_>> {
    let mut node = unparenthesize(declarator)?;
    let mut returns_pointer = false;
    while node.kind() == "pointer_declarator" {
        returns_pointer = true;
        node = unparenthesize(node.child_by_field_name("declarator")?)?;
    }
    if node.kind() != "function_declarator" {
        return None;
    }
    // `(f)(int)` and `(f(int))` both declare `f`; a macro-shy definition writes one of them.
    // The parens are NOT redundant when what they hold is a pointer -- `char *(*f(int))(void)`
    // returns a function pointer -- and that shape is rejected here, by the identifier test.
    let name = unparenthesize(node.child_by_field_name("declarator")?)?;
    if name.kind() != "identifier" {
        return None;
    }
    Some(FunctionHead {
        name,
        params: node.child_by_field_name("parameters")?,
        returns_pointer,
    })
}

/// The declarator inside any number of layers of parentheses: `(d)` declares exactly what `d`
/// declares, so a parenthesized declarator is transparent to [`function_head`]. `None` if the
/// parentheses hold nothing (parse-recovery debris).
///
/// openssh spells one this way without meaning to: `openbsd-compat/getrrsetbyname.c` defines
/// `_getshort` under a `#define _getshort(x) (_ssh_compat_getshort(x))`, so the *definition*
/// preprocesses to `static u_int16_t (_ssh_compat_getshort(const u_char *msgp)) { ... }`.
fn unparenthesize(node: Node<'_>) -> Option<Node<'_>> {
    let mut node = node;
    while node.kind() == "parenthesized_declarator" {
        node = first_named_child(node)?;
    }
    Some(node)
}

/// The first named child that is not a comment -- the single expression a
/// `parenthesized_expression` holds, or the first operand of an `argument_list`. `None` when
/// there is none: empty parentheses (`f()`, or parse-recovery debris) have no named child.
fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() != "comment")
}

/// Is this `type_identifier` a name that is a *type*?
///
/// Every `type_identifier` is, except one: the tag of a record or enum. `struct stat { ... }`
/// puts `stat` in a namespace of its own, where it is only ever reachable through the keyword;
/// the name `stat` on its own means the FUNCTION. Recording tags would make `(stat)(path, &st)`
/// -- a call written with redundant parentheses -- read as a cast to `stat`, silently deleting
/// the call. See [`Context::type_names`].
fn is_type_name(node: Node<'_>) -> bool {
    if node.kind() != "type_identifier" {
        return false;
    }
    let is_tag = node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "struct_specifier" | "union_specifier" | "enum_specifier"
        ) && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id())
    });
    !is_tag
}

/// The callee of a call with its redundant grouping parentheses peeled: `(f)(x)` calls `f` and
/// `(*fp)(x)` calls through `fp`, exactly as the unparenthesized spellings do.
///
/// The parentheses are not cosmetic to a frontend that names a callee by its source text: the
/// BSD red-black-tree macros openssh compiles in expand to `comp = (blob_cmp)(elm, parent);`,
/// and lowering that as a direct call to a function literally named `(blob_cmp)` both invented
/// a function and lost the call edge to the real one.
///
/// A GNU statement expression `({ ... })(x)` is NOT peeled. Those parentheses are part of the
/// construct rather than grouping around an expression, so peeling them would leave a
/// `compound_statement` that is not the callee's spelling either. There is nothing to peel *to*:
/// what stands in callee position is the whole construct, and [`is_statement_expression`] is what
/// [`Context::collect_call`] asks about it.
fn unparenthesized_callee(node: Node<'_>) -> Node<'_> {
    let mut node = node;
    while node.kind() == "parenthesized_expression" {
        match first_named_child(node) {
            Some(inner) if inner.kind() != "compound_statement" => node = inner,
            _ => break,
        }
    }
    node
}

/// Is `node` a GNU statement expression -- `({ s1; s2; value; })`?
///
/// tree-sitter-c 0.24.1 has no node for the construct: it parses as a `parenthesized_expression`
/// wrapping a `compound_statement`, which is why [`Context::flatten_expr`] and
/// [`Context::flatten_lvalue`] both key on the inner `compound_statement` and reach it through
/// their `parenthesized_expression` arms. A caller holding the OUTER node --
/// [`unparenthesized_callee`] hands one back unpeeled -- asks here instead.
fn is_statement_expression(node: Node<'_>) -> bool {
    match node.kind() {
        "compound_statement" => true,
        "parenthesized_expression" => {
            first_named_child(node).is_some_and(|inner| inner.kind() == "compound_statement")
        }
        _ => false,
    }
}

/// The name a parameter's declarator declares, and whether the parameter is a reference.
///
/// A declarator is not a fixed list of spellings, it NESTS: `char **argv` is a
/// `pointer_declarator` inside a `pointer_declarator`, `char *v[]` an `array_declarator`
/// inside one, `int (*cb)(int, int)` a `function_declarator` around a parenthesized one, and
/// any of them can wear parentheses. Walking down to the identifier binds every depth; the
/// query this replaced enumerated the one-level shapes and dropped the rest in silence
/// (spec 140), which cost `main` its `argv`.
///
/// The name is `None` when the declarator declares none -- an abstract declarator
/// (`void f(char **)`), or parse debris. That is not the same as "there is no parameter":
/// see [`Context::collect_params`], which still owns the slot.
///
/// A pointer or an array parameter is [`ParameterType::ByRef`] -- it is a handle on storage
/// the caller can see written -- at every depth. A function pointer is not: what it points at
/// is code, so `int (*cb)(int, int)` stays `ByVal`, as the query had it.
///
/// A C++ lvalue reference (`T& r`, a `reference_declarator`, which the C grammar does not
/// have) is also storage shared with the caller, but `const T&` is read-only: it is `ByVal`
/// (the referent's value flows in, nothing flows back) and a non-const `T&` is `ByRef`
/// (write-back), exactly like a pointer out-param. The `const` probe reads `decl`, the
/// `parameter_declaration`, and is grammar-neutral (`type_qualifier` exists in both grammars);
/// it is only reached for a reference, so the C path never asks.
fn param_head<'t>(
    decl: Node<'t>,
    declarator: Node<'t>,
    source: &str,
) -> (Option<Node<'t>>, ParameterType) {
    let mut node = declarator;
    let (mut is_ref, mut is_reference, mut is_function, mut name) = (false, false, false, None);
    loop {
        let Some(current) = unparenthesize(node) else {
            break;
        };
        match current.kind() {
            "identifier" => {
                name = Some(current);
                break;
            }
            "pointer_declarator"
            | "abstract_pointer_declarator"
            | "array_declarator"
            | "abstract_array_declarator" => is_ref = true,
            "reference_declarator" | "abstract_reference_declarator" => is_reference = true,
            "function_declarator" | "abstract_function_declarator" => is_function = true,
            // A declarator shape with no name in it to find (an attribute, debris).
            _ => break,
        }
        // `char **` bottoms out at an `abstract_pointer_declarator` with no inner declarator.
        // A `reference_declarator` has no `declarator` field at all: what it declares is its
        // one named child.
        let inner = current.child_by_field_name("declarator").or_else(|| {
            (current.kind() == "reference_declarator")
                .then(|| current.named_child(0))
                .flatten()
        });
        match inner {
            Some(inner) => node = inner,
            None => break,
        }
    }
    let param_type = if is_function {
        ParameterType::ByVal
    } else if is_reference {
        if node_has_const_qualifier(&decl, source) {
            ParameterType::ByVal
        } else {
            ParameterType::ByRef
        }
    } else if is_ref {
        ParameterType::ByRef
    } else {
        ParameterType::ByVal
    };
    (name, param_type)
}

/// The name recorded for a parameter that declares none, so that `param_names` stays indexed
/// by parameter position. A NUL cannot occur in a C identifier, so no lookup can reach it.
const UNNAMED_PARAM: &str = "\0<unnamed>";

fn to_str<'b>(n: &Node<'_>, source: &'b str) -> &'b str {
    n.utf8_text(source.as_bytes()).unwrap().trim()
}

/// Whether `node` carries a leading `const` type qualifier (a `type_qualifier` child whose
/// text is `const`). Used to tell `const T&` (read-only → `ByVal`) from `T&` (write-back →
/// `ByRef`). `type_qualifier`/`const` are grammar-neutral (both tree-sitter-c and -cpp emit
/// them), so this carries no language assumption; the C++ classifier is simply the only
/// caller, since only a reference parameter consults it.
fn node_has_const_qualifier(node: &Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "type_qualifier" && to_str(&child, source) == "const")
}

/// The `goto` labels of the function whose body is `node`, in tree order, so
/// `lower_function` can pre-create a block for each and a forward `goto L` resolves.
///
/// A nested `function_definition` is not descended into. A label's scope in C is the function
/// that contains it, so a `goto` in this body can never target one, and the same query that
/// found this body finds the nested one and lowers it as a function of its own -- with its
/// own label blocks, walked there. Pre-creating them here instead leaves blocks nothing ever
/// enters and nothing ever terminates, which `finalize_terminators` then patched and charged
/// to a function whose own code is fine. In the kernel corpus these are rarely GNU nested
/// functions: they are parse recovery, which resumes by re-parenting the following
/// definitions into the previous function's `compound_statement` (spec 064) -- which is how
/// `resource_intersection`, three straight-line statements with no label at all, came to own
/// a 1 MB body holding 2,208 of them, and two `out:` labels with it.
///
/// A `sizeof`/`_Alignof` operand is not descended into either, for the same reason from the
/// other direction: the operand is *unevaluated*, so `flatten_expr` lowers the whole
/// construct to the compile-time constant it is (`Exp::Str` of its own source text, spec 063)
/// and never walks inside it. A label in there names no reachable code -- `goto` into an
/// unevaluated operand is not C -- so its block, too, would only ever be an empty orphan.
///
/// A label the *recovery* holds is a different matter and is deliberately still collected:
/// plenty of well-formed code lowers out of a damaged body (that is spec 064's whole point),
/// and dropping its labels would break its `goto`s. Its block simply goes unentered, which
/// [`finalize_terminators`] knows not to charge to the frontend.
fn collect_labels(node: Node<'_>, source: &str, out: &mut Vec<String>) {
    if node.kind() == "labeled_statement"
        && let Some(label) = node.child_by_field_name("label")
    {
        out.push(to_str(&label, source).to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "function_definition" | "sizeof_expression" | "alignof_expression"
        ) {
            continue;
        }
        collect_labels(child, source, out);
    }
}

/// Register every directly-called function that has no definition in this translation unit
/// as an empty-body (external) function.
///
/// Taint models identify sources, sinks, and propagators by name (`source`, `sink`,
/// `malloc`, ...), and both the model engine and query-time endpoint resolution look the
/// name up among the program's functions. A function that is only *declared* in C (e.g.
/// `int source();`) never reaches `collect_definitions` -- which matches `function_definition`
/// nodes -- so without this pass its calls have edges pointing at a name that no IR function
/// carries, and every model targeting it silently matches nothing. Creating an empty-body
/// function gives the name a function id the model/query can resolve; the empty body also
/// marks it external during indexing (see codegen's `external_function`). Mirrors the extern
/// pass in the dex/jvm frontends. Runs on the import path only (see [`lower_units`]).
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
struct ScopeView {
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
        // A **union**'s members share storage, so a member access on the union variable is
        // collapsed to the synthetic `UNION_FIELD` when it is read (see `union_vars`, the F4
        // model). The initializer has to deposit taint at that same path: writing the first
        // member's own name would leave *every* read -- including `u.a`, the member actually
        // initialized -- resolving to an untainted path and silently dropping the taint. The
        // variable is registered in `union_vars` by `walk_declaration` before it gets here.
        let collapsed = base_ap.is_pathless() && self.union_vars.contains(&base_ap.base);
        self.lower_initializer_list(
            source,
            program,
            scope_view,
            base_ap,
            init_list,
            members.as_deref(),
            elem_layout.as_deref(),
            depth,
            collapsed,
        )
    }

    /// The field name element *i* of a record level writes: the member's own name, or the
    /// shared [`UNION_FIELD`] when this level's members alias each other (a union variable).
    fn write_name<'s>(&self, slot: &'s MemberSlot, collapsed: bool) -> &'s str {
        if collapsed {
            UNION_FIELD
        } else {
            slot.name.as_str()
        }
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
    /// member's own record type (from its [`MemberSlot::type_tag`] or, for an anonymous inline
    /// record, its [`MemberSlot::inline_layout`]), or -- once `depth` array levels have been
    /// entered -- the array's element layout in `elem_layout`. Anything not resolvable falls
    /// back to element numbering, which is the pre-existing behavior.
    ///
    /// `collapsed` marks a level whose members alias -- a union variable, whose member reads all
    /// resolve to [`UNION_FIELD`] -- so every element of *this* level writes that shared field
    /// instead of the member's own name. It does not carry into nested levels: only the access
    /// on the union variable itself collapses (`u.q.a` reads `$union` then `a`), which is what
    /// recursing with the member's real layout reproduces.
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
        collapsed: bool,
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
                    // `.a` -> Symbol("a"), matching how a `.a` field read is lowered -- or the
                    // shared field, when this level's members all alias (a union).
                    let member = member.trim();
                    let slot = members.and_then(|m| m.iter().find(|m| m.name == member));
                    fields.push(PathSegment::symbol(if collapsed {
                        UNION_FIELD
                    } else {
                        member
                    }));
                    let nested = slot.and_then(|s| s.nested_layout(&self.struct_layouts));
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
                        fields.push(PathSegment::symbol(self.write_name(slot, collapsed)));
                        slot.nested_layout(&self.struct_layouts)
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
                    false,
                )?;
            } else {
                let rhs = self.flatten_expr(program, value_node, source, scope_view)?;
                self.add_assign_to_program(program, scope_view, &elem_ap, &rhs, None);
            }
        }
        Ok(())
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

        // Open a destructor frame for this lexical scope: each class-typed stack object
        // constructed here is recorded into it (by `walk_declaration`'s value declarator /
        // `cpp::emit_construction`), and at the scope's normal fall-through exit its destructor is
        // emitted by the `scope_exit` hook (spec 017). The frame lives here rather than in
        // `walk_compound_statement` because a bare `{ ... }` walks its body through *this*
        // function directly, and it has stack objects to destroy just the same. C constructs no
        // class objects, so every frame is empty and this is inert on the C path.
        self.dtor_frames.push(Vec::new());
        let mut result: Result<(), Error> = Ok(());

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
                scope_view = match add_block(
                    program,
                    &scope_view,
                    &mut self.scope_tree,
                    false,
                    &format!("after_diverge::{}", get_line_num(&child)),
                ) {
                    Ok(sv) => sv,
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                };
            }
            match self.walk_statement(source, program, &mut scope_view, child) {
                Ok(d) => diverged = d,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }

        // Emit this scope's stack objects' destructors (reverse construction order) at the closing
        // `}`, before the frame is popped -- the hook drains the top frame -- and only on the
        // fall-through path: a compound that diverged took an early exit, whose scope-exit
        // destructors are out of spec 017's scope. Nothing propagates with `?` until after the
        // pop, or a failure would leave the frame stack unbalanced.
        if result.is_ok() && !diverged {
            let scope_exit = self.hooks.scope_exit;
            result = scope_exit(self, program, &scope_view);
        }
        self.dtor_frames.pop();
        result?;

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

    /// Intern a span for `node`'s byte range in this unit's file and return the [`SourceInfo`]
    /// pointing at it -- the default (no-span) `SourceInfo` when spans are not being recorded.
    fn span_for_node(&mut self, node: Node<'_>) -> SourceInfo {
        let (Some((key, unit_len)), Some(builder)) = (&self.unit_key, self.source_info.as_mut())
        else {
            return SourceInfo::default();
        };
        let start = node.start_byte();
        let len = node
            .end_byte()
            .saturating_sub(start)
            .min(unit_len.saturating_sub(start)) as u32;
        SourceInfo::new(builder.span_for(key.clone(), start as u32, SpanLen::ByteLen(len)))
    }

    /// Lower a single statement, threading `scope_view` (so control-flow statements can
    /// move the "current block" for following statements). Returns `true` if the
    /// statement *diverged* — i.e. it terminated the current block with no fall-through
    /// (`return`/`break`/`continue`, or a `labeled_statement` whose body diverges) — so
    /// the enclosing compound should stop and skip its end-of-compound link.
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
            // A syntax error in statement position: a problem in the analyzed source,
            // not a gap in this frontend. This is the position in which tree-sitter's
            // ERROR node still names the construct that defeated it, so it is quoted --
            // see `Context::report_unparsable_construct`.
            "ERROR" => {
                self.report_unparsable_construct(child, source)?;
            }
            _ => {
                self.flatten_expr(program, child, source, scope_view)?;
            }
        }
        Ok(false)
    }

    /// Report an unparsable construct: an `ERROR` node met where a *statement* was
    /// expected, which is the position in which tree-sitter's ERROR node still names the
    /// construct that defeated it. Reported once per node, against the analyzed source.
    ///
    /// This is the same population the old `frontend gap: Unknown token(2): ERROR: ...`
    /// warning covered (180 nodes in the kernel census), re-attributed and with the quote
    /// bounded. `run-linux.sh`'s parse-error triage classifies these by the
    /// `ERROR: <construct>` tail, so that tail is load-bearing -- do not drop it.
    fn report_unparsable_construct(
        &mut self,
        error_node: Node<'_>,
        source: &'a str,
    ) -> Result<(), Error> {
        if !self.reported_parse_errors.insert(error_node.id()) {
            return Ok(());
        }
        let (quote, elided) = quote_construct(to_str(&error_node, source));
        let elision = if elided > 0 {
            format!(" (+{elided} chars elided)")
        } else {
            String::new()
        };
        malformed_source(format!(
            "parse error{elision}; construct not parsed -- ERROR: {quote}"
        ))
    }

    /// Report that a function's body contains parse-recovery output that is not analyzed,
    /// once per function.
    ///
    /// Everything the recovery produced or re-parented is then skipped silently. Reporting
    /// each such node as a construct the frontend failed to support is what blamed ctadl
    /// 41,751 times in the kernel census -- for 180 actual parse errors -- and, worse,
    /// described the wreckage as if it were the analyzed program: a `int foo(void) {...}`
    /// tree-sitter re-parented into the previous function's body was logged as
    /// "Unsupported expression type: function_definition".
    ///
    /// Per *function*, not per region, and deliberately without an `ERROR: <text>` tail.
    /// The nodes on this path are in expression position, where an `ERROR` is a shard of
    /// the recovery (`long`, `*`, `struct`, `{ return f(x`) rather than the construct that
    /// failed -- quoting them would flood the triage with 1,600 "constructs" that are not
    /// constructs. What the body did choke on is reported by
    /// [`Context::report_unparsable_construct`]; what a reader needs here is which function
    /// is not trustworthy.
    fn report_unanalyzed_recovery(&mut self, func_name: &str) -> Result<(), Error> {
        if self.functions_with_recovery.contains(func_name) {
            return Ok(());
        }
        self.functions_with_recovery.insert(func_name.to_string());
        malformed_source(format!(
            "function `{func_name}`: tree-sitter parse-recovery output in this body is not \
             analyzed (the code it displaced is not in the parse tree)"
        ))
    }

    /// Report a name node that quotes to the empty string, and say whose fault it is.
    ///
    /// `identifier` and `field_identifier` are *required* by the grammar in the positions this
    /// frontend reads a name from, so when the parse fails there tree-sitter repairs it by
    /// INSERTING a zero-width token -- a node with a kind, a position and no text at all. The
    /// empty string is not a name: handed to [`Context::build_access_path`] it resolves to
    /// nothing in scope and mints the global `$globals.` with an empty first path segment,
    /// which serializes to the access path `"."` -- and `facts::Path`'s own parser rejects
    /// that, so ONE such token anywhere in a corpus makes the whole index unqueryable
    /// (`ctadl query` panics reading the parquet it just wrote, before printing any result).
    /// The kernel's x86 percpu headers put 134 of them into two translation units.
    ///
    /// A token nobody wrote names nothing, so this is spec 064's rule again: an inserted token
    /// only exists because the body around it did not parse, so say once that the body holds
    /// recovery output and let the caller substitute a fresh temp -- the same recovery the
    /// `flatten_expr`/`flatten_lvalue` catch-alls use. The `else` arm cannot fire on a
    /// tree-sitter tree (an inserted token sets `has_error` on every ancestor, so
    /// `recovery_region` always finds one) and is kept so a nameless node arriving any other
    /// way is still reported rather than silently dropped.
    fn report_missing_name(&mut self, node: Node<'_>, scope_view: &ScopeView) -> Result<(), Error> {
        if recovery_region(node).is_some() {
            let func_name = scope_view.func_name.clone();
            self.report_unanalyzed_recovery(&func_name)
        } else {
            unexpected_ast(format!(
                "{} with no name (a zero-width node names no object)",
                node.kind()
            ))
        }
    }

    /// A fresh local nothing else names, as an access path: the standard recovery for a
    /// location this frontend could not resolve. A store to it is dropped; a read of it is
    /// opaque. Either way the surrounding function still lowers.
    fn dead_temp_path(&mut self, program: &mut Program, scope_view: &ScopeView) -> RawPath {
        let temp_name = self.allocator.next_temp();
        RawPath::new(
            VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp_name)),
            ThinVec::new(),
        )
    }

    fn walk_declaration(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        node: Node<'_>,
    ) -> Result<(), Error> {
        // If this declaration's type names a known class (only ever true under C++, where
        // the `collect_aux` hook has populated `self.classes`), remember the class of each
        // local it declares so a later `recv.method(…)` call can dispatch on it. For C the
        // `classes` map is empty, so this is always `None` and records nothing. A namespaced
        // class type is a `qualified_identifier` (`ns::Box`) rather than a plain
        // `type_identifier`; its full text (`ns::Box`) is the qualified key the C++ hook
        // registered the class under. `qualified_identifier` never occurs under the C grammar,
        // so accepting it is a neutral node-shape check, not a language branch.
        let class_type: Option<String> = node
            .child_by_field_name("type")
            .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
            .map(|t| to_str(&t, source))
            .filter(|name| self.classes.contains_key(*name))
            .map(str::to_string);

        // A class-typed declaration may be an object *construction* (`Box b(args)` and the
        // copy/brace variants). Delegate that recognition + lowering to the C++ `construct`
        // hook: if it handles the declaration, we are done. Only reached when the type names
        // a known class — empty for C, so the C path never calls the hook (its hook is a
        // no-op returning `false` regardless). No language branch here; this is data-driven
        // on the neutral `classes` map.
        if let Some(class) = &class_type {
            let construct = self.hooks.construct;
            if construct(self, source, program, scope_view, node, class)? {
                return Ok(());
            }
        }

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
                "init_declarator" => {
                    let inner = nest_decl
                        .child_by_field_name("declarator")
                        .expect("double declarators on inits");
                    // A C++ reference local (`T& r = x`) aliases its referent rather than
                    // copying it; `reference_declarator` only occurs under the C++ grammar,
                    // so this is inert for C.
                    if inner.kind() == "reference_declarator" {
                        self.bind_reference_local(source, program, scope_view, inner, nest_decl)?;
                        // A reference to a known class object (`Box& r = b`) carries that
                        // class so a later `r.m(…)` dispatches; the alias above already maps
                        // `r` to the referent, which becomes the arg-0 receiver. Only ever
                        // fires under C++ for a class-typed reference (`class_type` is `None`
                        // for C and for `int&`), so the C path is unaffected.
                        if let Some(class) = &class_type
                            && let Some(leaf) = declarator_leaf_ident(inner, source)
                        {
                            self.local_types.insert(leaf.to_string(), class.clone());
                        }
                        continue;
                    }
                    inner
                }
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
                // The asm annotation on a declarator, not a declarator: a GCC
                // explicit-register variable (`register unsigned long sp asm("rsp");`, from
                // x86's `current_stack_pointer`) or an asm label on an object
                // (`extern int v asm("othersym");`). The grammar files it under the *same*
                // `declarator` field as the name it annotates --
                // `field('declarator', seq(_declaration_declarator, optional(gnu_asm_expression)))`
                // -- and tree-sitter distributes a field over every element of the sequence,
                // so `children_by_field_name("declarator")` yields two children for one
                // declared name: the declarator, handled by the arms above on the previous
                // iteration, and this. What it names is where the variable is stored (a
                // machine register) or the symbol it is emitted under; neither is a value and
                // neither carries dataflow, so there is nothing to lower. Deliberately NOT
                // routed to `flatten_gnu_asm`: that models an operand transfer, and this asm
                // has no operands at all -- only a register-name string.
                "gnu_asm_expression" => continue,
                _ => {
                    unexpected_ast(format!(
                        "Declaration declarator had an unexpected kind {decl_kind}"
                    ))?;
                    continue;
                }
            };
            let var_name = to_str(&decl_ident, source);
            self.scope_tree
                .add_variable(scope_view.sidx, var_name.to_string(), VarKind::Local);
            if let Some(class) = &class_type {
                // Key the class-typed local by its leaf identifier so a pointer receiver
                // (`Box* p = &b`) is recorded as `p`, not `*p` — otherwise a later
                // `p->m(…)` (whose receiver is the identifier `p`) would not dispatch.
                let key = declarator_leaf_ident(decl_ident, source).unwrap_or(var_name);
                self.local_types.insert(key.to_string(), class.clone());
                // A class-typed **value** local (`Widget w;` / `Widget w = other;`) is a stack
                // (automatic) object: record it in the enclosing scope's destructor frame so its
                // destructor runs at scope exit (spec 017). Only a bare `identifier` declarator is a
                // value object — a pointer (`Widget* p = …`, a `pointer_declarator`) or a reference
                // (handled earlier as a `reference_declarator`) is *not*, and must not get a
                // scope-exit destructor. Constructed objects (`Widget w(args)`) are recorded by
                // `cpp::emit_construction` (the `construct` hook returned early, above). Neutral:
                // `class_type` is `None` for C, so nothing is recorded and the C path is unchanged.
                if decl_ident.kind() == "identifier"
                    && let Some(frame) = self.dtor_frames.last_mut()
                {
                    frame.push((key.to_string(), class.clone()));
                }
            }
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
                // An **aggregate (brace) initializer** — `int a[2] = { s, 0 }`,
                // `struct P p = { s, 0 }` — is not an expression the flattener can evaluate;
                // it is shorthand for the element assignments the programmer could have
                // written, so it is desugared element-wise (otherwise `{ … }` reaches
                // `flatten_expr`'s catch-all: `ERR 78: … initializer_list`). Both grammars
                // name this node `initializer_list`, so recognizing it is a neutral
                // node-shape check. `node` carries the declaration's type, which is what
                // lets a `struct` tag be mapped onto its member names.
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

    /// Bind a C++ reference local `T& r = x;`. A reference *is* its referent's storage, so
    /// instead of emitting a copy we record `r -> <referent access path>` in
    /// [`Context::reference_aliases`]; [`Context::build_access_path`] then resolves every use
    /// (read or write) of `r` to the referent. The referent must be a resolvable lvalue — an
    /// identifier or a field access, the in-scope forms, which [`Context::flatten_expr`]
    /// returns as a side-effect-free `AccessPath`. For anything else (or a malformed
    /// reference) we fall back to a plain local plus a copy so lowering never fails. Only
    /// reached under C++ (C has no `reference_declarator`), so the C path is unaffected.
    fn bind_reference_local(
        &mut self,
        source: &'a str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        ref_decl: Node<'_>,
        init: Node<'_>,
    ) -> Result<(), Error> {
        let name_node = ref_decl.named_child(0).filter(|n| n.kind() == "identifier");
        let value = init.child_by_field_name("value");

        if let Some(name_node) = name_node {
            let ref_name = to_str(&name_node, source);
            // Alias the reference to its referent's *location*, not to a loaded copy of it —
            // a reference **is** the referent's storage, so a write through `r` must land on
            // `x`. An identifier/field referent is an lvalue, so resolve it with
            // `flatten_lvalue` (which keeps the symbolic path intact instead of loading it).
            if let Some(val) = value
                && matches!(val.kind(), "identifier" | "field_expression")
                && let Ok(path) = self.flatten_lvalue(program, val, source, scope_view)
            {
                self.reference_aliases.insert(ref_name.to_string(), path);
                return Ok(());
            }
            // A `call_expression` referent is a reference-returning method
            // (`Box& r = b.setV(x)`): its result temp was already registered as an alias of
            // the receiver object by `dispatch_call`, so flattening it yields that object and
            // `r` binds straight through to it.
            if let Some(val) = value
                && val.kind() == "call_expression"
            {
                // The call's value is already the aliased object when the callee returns the
                // receiver (`dispatch_call` registered its result temp before resolving it), so
                // this binds `r` straight through to that object. A pathless object normalizes
                // to `Exp::Variable`, an offset-only address to `Exp::AccessPath`; both name a
                // location a reference can bind to.
                let loc = match self.flatten_expr(program, val, source, scope_view)? {
                    Exp::Variable(v) => Some(RawPath::new(v, ThinVec::new())),
                    Exp::AccessPath(ap) => Some(raw_path_of(&ap)),
                    _ => None,
                };
                if let Some(loc) = loc {
                    self.reference_aliases.insert(ref_name.to_string(), loc);
                    return Ok(());
                }
            }
            // Fallback: register a plain local and copy whatever initializer there is.
            self.scope_tree
                .add_variable(scope_view.sidx, ref_name.to_string(), VarKind::Local);
            if let Some(val) = value {
                self.collect_assignment(source, program, scope_view, name_node, val, None)?;
            }
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
            // A bare `return;`. Legal in a non-`void` function (and common in `int` error
            // paths), where the arity contract still demands an argument: `implicit_return`
            // supplies the indeterminate value C says such a return has.
            let term = implicit_return(program, scope_view.fidx);
            program.functions[scope_view.fidx].blocks[scope_view.blidx].terminator = Some(term);
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
        self.walk_compound_statement(source, program, &init_scope, &init_cp)?;
        self.walk_compound_statement(source, program, &condition_scope, &condition_cp)?;
        //add 'sad edge'
        link_blocks(program, &condition_scope, &continuation, false)?;
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
        let condition_raw = child
            .child_by_field_name("condition")
            .expect("always has condition");
        let condition = (self.hooks.condition_expr)(condition_raw);

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
        let condition_raw = child
            .child_by_field_name("condition")
            .expect("switch always has a condition");
        let condition = (self.hooks.condition_expr)(condition_raw);
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
            .expect("label block pre-created in lower_function");
        self.walked_label_blocks.insert(label_blidx);

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
        let condition_raw = child
            .child_by_field_name("condition")
            .expect("always has condition");
        let condition = (self.hooks.condition_expr)(condition_raw);
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
        let param_vec = self.param_names.get(func_name).unwrap();
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
        // A C++ reference local (`T& r = x`) aliases its referent: resolve the name to the
        // referent's access path and append any trailing field accesses. The map is empty
        // for C (no `reference_declarator`), so this never fires on the C path.
        if let Some(aliased) = self.reference_aliases.get(name_pre_scope) {
            let mut ap = aliased.clone();
            ap.fields.append(&mut field_path);
            return ap;
        }
        let name: String;
        let varkind: VarKind;
        if let Some(vardecl) = self
            .scope_tree
            .find_variable(scope_view.sidx, name_pre_scope)
        {
            name = self.scope_tree.to_string(vardecl);
            varkind = vardecl.kind.clone();
        } else if let Some(class) = self.current_method_class.as_deref()
            && self
                .classes
                .get(class)
                .is_some_and(|info| info.members.contains(name_pre_scope))
        {
            // Inside a method body, an unqualified name that is a data member of the enclosing
            // class (and was not shadowed by a local/parameter — those are found above) resolves
            // to a member of the enclosing object. A **`static`** data member, though, is not
            // per-object: it is a single class-scoped **global** `<declaring-class>::<member>`, so
            // resolve it to that global (the same key a qualified `Class::<member>` read resolves
            // to) — taint written through one method is then read by another. This is what a
            // static method (which has *no* `this`) and any method sharing the static member need
            // (spec 015). An ordinary per-object member instead becomes `this.<member>`: the
            // implicit `this` is parameter 0 (installed by `lower_function`), so the member is the
            // leading field on it, with any trailing `field_path` following. Both cases are
            // data-driven on the neutral `classes`/`static_members` maps, which are empty for C —
            // no language branch.
            if let Some(decl_class) = self.static_member_class(class, name_pre_scope) {
                // Same shape as the `VarKind::Global` arm below: `$globals.<Class::member>`.
                let mut fields = ThinVec::with_capacity(field_path.len() + 1);
                fields.push(PathSegment::symbol(
                    format!("{decl_class}::{name_pre_scope}").as_str(),
                ));
                fields.append(&mut field_path);
                return RawPath::new(VariableRef::new_global(), fields);
            }
            let mut fields = ThinVec::with_capacity(field_path.len() + 1);
            fields.push(PathSegment::symbol(name_pre_scope));
            fields.append(&mut field_path);
            return RawPath::new(VariableRef::new_parameter(0u32.into()), fields);
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
        query: &Query,
    ) -> anyhow::Result<(), Error> {
        let global_sidx = self.scope_tree.add_scope("%GLOBAL".to_string(), None);
        // The definitions the shared query cannot own (C++ inline and out-of-line methods,
        // namespaced functions) are discovered, registered and lowered by the grammar's hook
        // first, so a free function's `recv.method(...)` resolves its callee and the method's
        // class is registered for member resolution. The C hook is a no-op.
        let aux = self.hooks.collect_aux;
        aux(self, source, tree.root_node(), program, global_sidx)?;
        self.lower_definitions(source, tree, program, global_sidx, query)
    }

    fn collect_params(
        &mut self,
        source: &'a str,
        param_list: &Node<'_>,
        fdat: &mut FunctionData,
        function_name: &str,
        scope_view: &ScopeView,
    ) -> anyhow::Result<(), Error> {
        let param_names = self
            .param_names
            .entry(function_name.to_string())
            .or_default();

        // The parameters are the parameter list's own children, walked in order, and a
        // parameter's index is its position among them. The query this replaced matched
        // `parameter_declaration` ANYWHERE under the list and numbered by match order, which
        // made position and index two different things: it bound a function pointer's own
        // formals as this function's, and a declarator shape it did not enumerate (`char **v`,
        // via `param_head`) took every later parameter down a slot with it.
        let mut cursor = param_list.walk();
        for decl in param_list.named_children(&mut cursor) {
            // `...` is a `variadic_parameter`, not a formal (clang does not count it either);
            // an old-style `f(a, b)` list holds bare `identifier`s, whose types live in the
            // declarations between the list and the body, and which this does not bind.
            if decl.kind() != "parameter_declaration" {
                continue;
            }
            // A `parameter_declaration` with no declarator at all declares no parameter here.
            // In well-formed C that shape is `f(void)` -- where the `void` IS the empty list
            // -- or C23's nameless `f(int)`, which no definition in the corpora writes. It is
            // also what a parameter list tree-sitter could not parse leaves behind: the
            // kernel's SYSCALL_DEFINE expands to `__typeof(__builtin_choose_expr(...)) fd`,
            // whose `__typeof` has no grammar rule, and the recovery reads the type as one
            // parameter and the NAME as a second bare type. Reserving a slot for both would
            // invent a parameter per formal in 140 kernel definitions. An abstract declarator
            // (`char **`, below) is a different matter: that one is unambiguous.
            let Some(declarator) = decl.child_by_field_name("declarator") else {
                continue;
            };
            let (param_name, param_type) = param_head(decl, declarator, source);

            fdat.params.push(param_type);
            match param_name {
                Some(param_name) => {
                    let pn = to_str(&param_name, source);
                    param_names.push(pn);
                    self.scope_tree.add_variable(
                        scope_view.sidx,
                        pn.to_string(),
                        VarKind::Parameter,
                    );
                }
                // An abstract declarator names nothing, so nothing in the body can read this
                // parameter -- but it still occupies a position, and a taint model naming
                // `Argument(1)` means the second one. The slot is held; only the name is not.
                None => {
                    param_names.push(UNNAMED_PARAM);
                }
            }
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
        let text = to_str(&node, source); //.to_string();
        match node.kind() {
            // A name tree-sitter inserted to repair the parse, quoting to the empty string.
            // It cannot become a path segment (see `report_missing_name`), so the read is
            // opaque: a fresh temp nothing else writes.
            "identifier" if text.is_empty() => {
                self.report_missing_name(node, scope_view)?;
                let temp_name = self.allocator.next_temp();
                Ok(Exp::Variable(VariableRef::new_local_idx(
                    program[scope_view.fidx].locals.get_or_intern(&temp_name),
                )))
            }
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
                    && (self.functions.contains_key(text) || self.declared_functions.contains(text))
                {
                    // Which definition of `text` this file means, when more than one file
                    // defines one (see `Context::resolve_reference`).
                    let target = self.resolve_reference(text);
                    Ok(Exp::ObjectRef(CallObject::FunctionPtr(target.into())))
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
                node.child_by_field_name("operator"),
            ),
            // The C++ `this` receiver — inside a method body, `this` is the implicit param 0
            // installed by `lower_function`, so it resolves to `@p0`. `*this` reaches here via
            // the `pointer_expression` arm below (its argument is this node). A `this` node
            // never occurs under the C grammar, so this arm is inert for C.
            "this" => {
                let ap = self.build_access_path(
                    "this",
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                // `this` is parameter 0, a bare variable, so this emits no loads.
                Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
            }
            // A C++ qualified name used as a value or callee (`ns::f`, `ns::Box::m`). Its
            // source text is the fully-qualified string (`ns::f`), which resolves to a global
            // by that name — so a qualified call `ns::f(args)` (whose `function` child is this
            // node) lowers via `collect_call` to a `DirectCall Explicit(["ns::f"])` that
            // matches the namespaced definition lowered under the same qualified IR name. A
            // `qualified_identifier` node never occurs under the C grammar, so this is inert
            // for C.
            "qualified_identifier" => {
                let ap = self.build_access_path(
                    text,
                    Default::default(),
                    scope_view,
                    &mut program[scope_view.fidx].locals,
                );
                Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)))
            }
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
                // `*(T *)K` -- a read through a constant address. `flatten_lvalue`'s
                // `cast_expression` arm gives that address a location, so the read must name the
                // same one: the pass-through would yield the constant `K` and a store through
                // `*(volatile u32 *)0xfee00300` would be unobservable at a read of it, which is
                // the whole reason the store side names a global rather than a fresh temp. Only
                // a cast, and only when the cast's value is the constant -- `*p` on a variable
                // keeps the pass-through, which is already symmetric because both sides name
                // `p`. Re-deriving the path here rather than calling `flatten_lvalue` avoids
                // lowering the operand twice; it is a literal, so `arg_exp` cost nothing.
                if is_deref
                    && arg.kind() == "cast_expression"
                    && let Exp::Str(constant) = &arg_exp
                    && !constant.is_empty()
                {
                    let ap = literal_address_path(constant);
                    return Ok(Exp::access_path(self.emit_loads(program, scope_view, ap)));
                }
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
            // `(__be16)(x)` is a cast that tree-sitter could only read as a call; when it is
            // one, it lowers exactly like the `cast_expression` arm below. See
            // `cast_shaped_call` for how the two are told apart.
            "call_expression" => match self.cast_shaped_call(node, source, scope_view)? {
                Some(operands) => self.flatten_cast_operands(program, operands, source, scope_view),
                None => {
                    let x = self.allocator.next_temp();
                    self.collect_call(program, node, source, scope_view, x)
                }
            },
            // C++ `delete p;` — destroying a heap object runs its destructor(s). The
            // `delete_expr` hook emits the destructor call (a CHA multi-target `DirectCall` for a
            // virtual destructor, the single static-type destructor otherwise, with the referent
            // `*p` as the arg-0 receiver) so taint the destructor body moves at `delete` time is
            // captured (spec 016); a hierarchy with no destructor emits nothing (014's no-op).
            // `delete` yields no value, so this still returns a fresh throwaway temp. The hook is
            // a no-op for C — a `delete_expression` node never occurs under the C grammar, so this
            // arm is inert for C (like the `this` arm above), no language branch.
            "delete_expression" => {
                let delete_expr = self.hooks.delete_expr;
                delete_expr(self, program, node, source, scope_view)?;
                let temp = self.allocator.next_temp();
                Ok(Exp::Variable(VariableRef::new_local_idx(
                    program[scope_view.fidx].locals.get_or_intern(&temp),
                )))
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
            //
            // `_Alignof` / `__alignof__` / `__alignof` / `_alignof` / `alignof` (all one
            // `alignof_expression` node) obey the same rule -- unevaluated operand,
            // compile-time constant result -- so they share this arm. In tree-sitter-c
            // 0.24.1 an `alignof_expression`'s operand is always a `type_descriptor`, so
            // even the GNU expression spelling `__alignof__(x)` parses `x` as a type name
            // and never reaches a value; lowering the whole node as its source text keeps
            // it that way. (An operand the type grammar cannot swallow, e.g.
            // `__alignof__(p->f)`, is a parse error before we ever get here -- a
            // tree-sitter-c grammar limit, not a frontend gap.)
            "sizeof_expression" | "alignof_expression" => {
                Ok(Exp::Str(ArcIntern::<str>::from(text)))
            }
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
                // Before blaming the frontend, ask whether the parser even reached here
                // from well-formed source. A node the recovery produced or re-parented is
                // not a construct this frontend failed to support: say once that this
                // body holds recovery output, and skip the rest of the wreckage silently.
                // In the kernel census that is the difference between 41,751 "frontend
                // gap" warnings and 180 honest parse errors -- the `int foo(void) {...}`
                // logged as "Unsupported expression type: function_definition" is a
                // perfectly good function tree-sitter re-parented into the *previous*
                // function's body, and is imported and lowered normally by
                // `lower_definitions`, which queries the whole tree.
                if recovery_region(node).is_some() {
                    let func_name = scope_view.func_name.clone();
                    self.report_unanalyzed_recovery(&func_name)?;
                } else {
                    debug_print_tree(node, 0, None, None);
                    unexpected_ast(format!(
                        "ERR 78: Unsupported expression type: {}",
                        node.kind()
                    ))?;
                }
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
    /// `asm goto` carries a label list on top of that, and those jumps are real CFG edges. They
    /// are wired by [`Context::link_asm_goto_labels`] *after* all of the operand statements are
    /// emitted, because linking sets the block's terminator and nothing may be appended to a
    /// block past its terminator.
    fn flatten_gnu_asm(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Exp, Error> {
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
        // is a statement -- but `flatten_expr` must yield something. Read it back *before* the
        // `asm goto` edges are wired: `emit_loads` appends statements, and after
        // `link_asm_goto_labels` the current block is a different, already-linked one.
        let value = match targets.first() {
            Some(target) => {
                let read_back = self.emit_loads(program, scope_view, target.clone());
                Exp::access_path(read_back)
            }
            None => blended,
        };

        self.link_asm_goto_labels(program, node, source, scope_view)?;
        Ok(value)
    }

    /// Turns the label list of a GNU `asm goto` into real CFG edges.
    ///
    /// The labels are ordinary `goto` targets, so this drives the very machinery `walk_goto`
    /// does: the per-function pre-scan in `lower_function` already created a block for every
    /// `labeled_statement` in the body and recorded it in `label_blocks`, which is what makes a
    /// forward jump (the usual shape -- `l_yes:` sits *after* the asm) resolve. `link_blocks`
    /// then appends each target to the current block's `Goto` terminator.
    ///
    /// Where `goto` *diverges*, an `asm goto` may or may not jump: control either lands on one
    /// of the labels or falls out the bottom. So the fall-through is an edge like any other --
    /// a fresh block, linked from the same terminator, that the rest of the enclosing statement
    /// and its siblings continue in. Nothing is reported as diverging, so the enclosing compound
    /// keeps walking normally.
    ///
    /// Building the edges from expression context (rather than a `walk_statement` pre-pass) is
    /// what `scope_view: &mut ScopeView` is for: it names the current function *and* block, and
    /// threading a new block back out through it is exactly how `lower_statement_expression_effects`
    /// already moves the walk forward from inside `flatten_expr`. A pre-pass would have to find
    /// the asm before its operands lower and could not place the split after them.
    fn link_asm_goto_labels(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<(), Error> {
        let Some(list) = node.child_by_field_name("goto_labels") else {
            return Ok(());
        };
        let mut cursor = list.walk();
        let labels: Vec<Node<'_>> = list.children_by_field_name("label", &mut cursor).collect();
        if labels.is_empty() {
            return Ok(());
        }

        // A label may legally appear twice in one list; the edge is the same edge, and pushing
        // it twice would put a duplicate successor in the terminator.
        let mut linked: Vec<BasicBlockIdx> = Vec::with_capacity(labels.len());
        for label_node in labels {
            let label = to_str(&label_node, source);
            let Some(&target) = self.label_blocks.get(label) else {
                malformed_source(format!("`asm goto` to undefined label `{label}`"))?;
                continue;
            };
            if linked.contains(&target) {
                continue;
            }
            linked.push(target);
            let mut to = scope_view.clone();
            to.blidx = target;
            link_blocks(program, scope_view, &to, false)?;
        }

        let after = add_block(
            program,
            scope_view,
            &mut self.scope_tree,
            true,
            &format!("after_asm_goto::{}", get_line_num(&node)),
        )?;
        *scope_view = after;
        Ok(())
    }

    fn flatten_nested_decl(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> std::result::Result<Exp, Error> {
        //how come only this declarator came up in expr? see pointer_decl way?
        // ... well function_declarators come up too.  see the logic there
        if let Some(iden) = node.child_by_field_name("declarator") {
            //oh noes.. look whats under that! a pointer declarator!
            if iden.kind() == "identifier" {
                let symbol = to_str(&iden, source);
                self.scope_tree
                    .add_variable(scope_view.sidx, symbol.to_string(), VarKind::Local);
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

    /// Walk `class` then its base chain (transitively) to the class that **defines** `method`,
    /// returning that class's name and whether the method returns its receiver (`returns_self`).
    /// The receiver's own class is checked **first**, so a derived class that redefines an
    /// inherited method (a non-virtual override) wins for a derived static-type receiver;
    /// otherwise the walk descends to the base that owns the method. Returns `None` if no class
    /// in the chain defines it. Driven by the neutral [`ClassInfo::bases`] (empty for C and for
    /// base-less classes, so this reduces to a single own-class lookup and never fires on C).
    /// A cycle guard (`seen`) keeps a malformed base loop from spinning; single inheritance in
    /// this slice is a linear chain, but the level-by-level walk also handles a wider chain.
    fn resolve_method_class(&self, class: &str, method: &str) -> Option<(String, bool)> {
        let mut level = vec![class.to_string()];
        let mut seen: HashSet<String> = HashSet::default();
        while !level.is_empty() {
            let mut next = Vec::new();
            for c in level {
                if !seen.insert(c.clone()) {
                    continue;
                }
                if let Some(info) = self.classes.get(&c) {
                    if info.methods.contains(method) {
                        return Some((c, info.returns_self.contains(method)));
                    }
                    next.extend(info.bases.iter().cloned());
                }
            }
            level = next;
        }
        None
    }

    /// If `member` names a **`static`** data member visible from `class` — declared `static` on
    /// `class` itself or on some class in its base chain — return the name of the class that
    /// **declares** it (the key its class-scoped global uses, `<declaring-class>::<member>`);
    /// otherwise `None` (an ordinary per-object member, which [`Self::build_access_path`] resolves
    /// to `this.<member>`). Walks the neutral [`ClassInfo::bases`] chain like
    /// [`Self::resolve_method_class`], reading each class's **own** [`ClassInfo::static_members`]
    /// (which are not flattened into subclasses, so the declaring class is recoverable). A `seen`
    /// guard bounds a malformed base cycle. Empty `static_members`/`bases` for C and non-static
    /// classes, so this is always `None` there and member resolution is unchanged.
    fn static_member_class(&self, class: &str, member: &str) -> Option<String> {
        let mut level = vec![class.to_string()];
        let mut seen: HashSet<String> = HashSet::default();
        while let Some(c) = level.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(info) = self.classes.get(&c) {
                if info.static_members.contains(member) {
                    return Some(c);
                }
                level.extend(info.bases.iter().cloned());
            }
        }
        None
    }

    /// Whether `method` is **virtual** on static type `class` — declared `virtual` (or
    /// `override`) on `class` itself or on any class in its base chain, transitively. A virtual
    /// method dispatches by class-hierarchy analysis (all subtree overrides); a non-virtual one
    /// stays single-target static dispatch. Driven by the neutral [`ClassInfo::virtual_methods`]
    /// sets, which are empty for C and for non-polymorphic C++ classes — so this is always
    /// `false` there and dispatch is unchanged. The base-chain walk mirrors
    /// [`Self::resolve_method_class`] (a `seen` guard against a malformed cycle).
    fn method_is_virtual(&self, class: &str, method: &str) -> bool {
        let mut level = vec![class.to_string()];
        let mut seen: HashSet<String> = HashSet::default();
        while let Some(c) = level.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(info) = self.classes.get(&c) {
                if info.virtual_methods.contains(method) {
                    return true;
                }
                level.extend(info.bases.iter().cloned());
            }
        }
        false
    }

    /// The class-hierarchy-analysis (CHA) target set for a virtual call of `method` on static
    /// type `static_class`, given the class `defining_class` that owns `method` for that static
    /// type (from [`Self::resolve_method_class`]): the static-type target `defining_class::method`
    /// **plus** `sub::method` for every transitive subclass `sub` of `static_class` that declares
    /// its **own** `method` (an override). This is a sound superset of the single override the
    /// dynamic type actually selects (Principle I). Names are bare `Class::method` (the caller
    /// arity-mangles each). Duplicate-free and order-stable; the subclass subtree is walked via
    /// the neutral [`Self::subclasses`] map (empty for C, so this reduces to the single
    /// static-type target there). A `seen` guard bounds a malformed hierarchy.
    fn cha_targets(&self, static_class: &str, defining_class: &str, method: &str) -> Vec<String> {
        let mut targets: Vec<String> = vec![format!("{defining_class}::{method}")];
        // Walk the subclass subtree rooted at `static_class`, collecting each subclass that
        // defines its own override of `method`.
        let mut stack: Vec<String> = self
            .subclasses
            .get(static_class)
            .cloned()
            .unwrap_or_default();
        let mut seen: HashSet<String> = HashSet::default();
        while let Some(sub) = stack.pop() {
            if !seen.insert(sub.clone()) {
                continue;
            }
            if let Some(info) = self.classes.get(&sub)
                && info.methods.contains(method)
            {
                let edge = format!("{sub}::{method}");
                if !targets.contains(&edge) {
                    targets.push(edge);
                }
            }
            if let Some(subs) = self.subclasses.get(&sub) {
                stack.extend(subs.iter().cloned());
            }
        }
        targets
    }

    /// Resolve a C++ instance-method call receiver node into the object to dispatch on — its
    /// arg-0 (`ByRef`) access-path expression and its class — or `None` if it is not a known
    /// class object. Handles a plain identifier local (`b.m()`), the implicit receiver
    /// (`this`/`*this`), a parenthesized receiver, and a **chained** call whose callee returns
    /// its receiver (`recv.a(…).b(…)` — `a` does `return *this`): the chained case lowers the
    /// inner call and yields the same object `a` dispatched on. Neutral-map-driven
    /// (`local_types`/`classes`, both empty for C), so it never fires on the C path.
    fn resolve_recv_obj(
        &mut self,
        program: &mut Program,
        recv_node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Option<RecvObj>, Error> {
        match recv_node.kind() {
            "identifier" => {
                let name = to_str(&recv_node, source);
                match self.local_types.get(name).cloned() {
                    Some(class) => {
                        // The receiver is passed by reference as arg 0, so resolve it to its
                        // location. A plain local object is pathless, so no load is emitted;
                        // a global object (`$globals.g`) has a symbolic field and must be
                        // loaded, which is the only way the offset-only IR can name it.
                        let ap = self.build_access_path(
                            name,
                            Default::default(),
                            scope_view,
                            &mut program[scope_view.fidx].locals,
                        );
                        let exp = Exp::access_path(self.emit_loads(program, scope_view, ap));
                        Ok(Some(RecvObj { exp, class }))
                    }
                    None => Ok(None),
                }
            }
            // The implicit receiver inside a method body: `this.m()` (rare) or `(*this).m()`.
            "this" => match self.current_method_class.clone() {
                Some(class) => {
                    let ap = self.build_access_path(
                        "this",
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    );
                    let exp = Exp::access_path(self.emit_loads(program, scope_view, ap));
                    Ok(Some(RecvObj { exp, class }))
                }
                None => Ok(None),
            },
            "parenthesized_expression" => {
                let inner = recv_node
                    .child(1)
                    .expect("parenthesized_expression has an inner node");
                self.resolve_recv_obj(program, inner, source, scope_view)
            }
            "pointer_expression" => {
                let arg = recv_node
                    .child_by_field_name("argument")
                    .expect("pointer_expression always has an argument");
                self.resolve_recv_obj(program, arg, source, scope_view)
            }
            // A chained receiver: lower the inner call; only a receiver-returning method
            // (`return *this`) yields an object to chain on.
            "call_expression" => {
                let temp = self.allocator.next_temp();
                Ok(self
                    .dispatch_call(program, recv_node, source, scope_view, temp)?
                    .and_then(|out| out.aliased))
            }
            _ => Ok(None),
        }
    }

    /// Try to lower `call_node` as a C++ instance-method call — `recv.method(args)`, including
    /// a chained receiver (`recv.a(args).b(args2)`). Returns `None` if it is not an instance-
    /// method call (a plain function call, or a function-pointer *member* call like `s.fp(x)`);
    /// the caller then lowers it as an ordinary call. On success it emits
    /// `DirectCall Class::method(recv, args…)` — `recv` prepended as the arg-0 (`ByRef`)
    /// receiver, so the callee's `this.<member>` writes flow back and a returned member flows
    /// out — and returns the result value plus, for a **receiver-returning** method
    /// (`return *this`), the object the result aliases (for chaining and `Class& r = call`
    /// binding). Neutral-map-driven, so it never fires on the C path.
    fn dispatch_call(
        &mut self,
        program: &mut Program,
        call_node: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
        temp_name: String,
    ) -> Result<Option<DispatchOut>, Error> {
        let func_node = call_node
            .child_by_field_name("function")
            .expect("call_expression always has a function");
        if func_node.kind() != "field_expression" {
            return Ok(None);
        }
        let recv_node = func_node
            .child_by_field_name("argument")
            .expect("field_expression always has an argument");
        let field_node = func_node
            .child_by_field_name("field")
            .expect("field_expression always has a field");
        let method = to_str(&field_node, source);

        let Some(recv) = self.resolve_recv_obj(program, recv_node, source, scope_view)? else {
            return Ok(None);
        };
        // Resolve `method` against the receiver's static class, then (if not defined there)
        // walk its base chain to the class that defines it — an inherited method dispatches to
        // the base that owns it, with the derived object as the by-ref receiver. Bail to
        // ordinary lowering if no class in the chain defines it (e.g. a function-pointer member
        // `s.fp(x)` on a plain struct). Checking the receiver's own class first makes a
        // non-virtual override (a derived redefinition) win by static type. The base chain is
        // empty for C and base-less classes, so this is a plain own-class lookup there.
        let (defining_class, returns_self) = match self.resolve_method_class(&recv.class, method) {
            Some(found) => found,
            None => return Ok(None),
        };
        let arg_node = call_node
            .child_by_field_name("arguments")
            .expect("call_expression always has arguments");
        let explicit_args = self.collect_arguments(program, arg_node, source, scope_view)?;
        // Build the call's target edge(s). A **non-virtual** method is a single static-type
        // target: the class that *defines* the method (`DefiningClass::method`), so an inherited
        // method reaches the base's lowered body (spec 011). A method that is **virtual** on the
        // receiver's static class dispatches by class-hierarchy analysis — the target set is the
        // static-type resolution *plus* every override in the static type's subclass subtree
        // ([`Self::cha_targets`]), a sound superset of the single dynamically-selected override.
        // `virtual_methods`/`subclasses` are empty for C and non-polymorphic C++, so this stays a
        // single-target static edge there (no language branch). Each target is arity-mangled by
        // the explicit-argument count (`b.f(x, y)` -> `Box::f#2`); a non-overloaded name stays
        // bare (the `methods`/`returns_self`/`virtual_methods` sets are keyed by the bare method
        // name, an overload-set property, so only the callee *string* changes).
        let bases: Vec<String> = if self.method_is_virtual(&recv.class, method) {
            self.cha_targets(&recv.class, &defining_class, method)
        } else {
            vec![format!("{defining_class}::{method}")]
        };
        let targets: ThinVec<String> = bases
            .iter()
            .map(|b| self.overload_name(b, explicit_args.len()))
            .collect();
        let mut method_args: ThinVec<Exp> = ctadl_ir::thin_vec![recv.exp.clone()];
        method_args.extend(explicit_args);
        let ret =
            VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp_name));
        program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
            StatementKind::CallAssign {
                style: CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(targets),
                },
                rets: ctadl_ir::thin_vec![ret],
                args: method_args,
            },
        ));

        // A receiver-returning method makes the call's result an alias to the receiver
        // object. Register the result temp as an alias of that object so `Class& r = call`
        // and any read of the temp resolve to the real object, and surface it so a chained
        // `.n(…)` dispatches on the same object (the by-ref receiver carries chained writes
        // back to it). Only class-object receivers (an `AccessPath`) can alias.
        // A receiver names a location either as a bare variable (`Exp::Variable`, which is what
        // `Exp::access_path` normalizes a pathless object to) or as an offset-only address
        // (`Exp::AccessPath`). Both can alias; only a constant cannot.
        let recv_loc = match &recv.exp {
            Exp::Variable(v) => Some(RawPath::new(v.clone(), ThinVec::new())),
            Exp::AccessPath(ap) => Some(raw_path_of(ap)),
            _ => None,
        };
        let aliased = match (returns_self, recv_loc) {
            (true, Some(loc)) => {
                self.reference_aliases.insert(temp_name.clone(), loc);
                self.local_types
                    .insert(temp_name.clone(), recv.class.clone());
                Some(recv.clone())
            }
            _ => None,
        };
        let ap = self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        let value = Exp::access_path(self.emit_loads(program, scope_view, ap));
        Ok(Some(DispatchOut { value, aliased }))
    }

    /// The cast tree-sitter cannot see -- its operand list, to lower as one -- or `None` for
    /// a call.
    ///
    /// tree-sitter-c knows a fixed list of primitive type names and has no symbol table, so a
    /// cast to a typedef written with the operand parenthesized, `(__be16)(x)`, parses as a
    /// `call_expression` whose callee is a `parenthesized_expression` -- character for
    /// character the shape of a genuine call through a parenthesized function pointer,
    /// `(fp)(x)`.
    ///
    /// Lowering every one of them as a call was silent and unsound at once: `define_extern_
    /// functions` invented an empty-bodied function named `( __be16)` (2,931 call sites in the
    /// kernel corpus, 605 in openssh, 229 in nginx), and an empty body returns nothing, so the
    /// taint that went into the cast did not come out.
    ///
    /// The unit's own evidence decides, and only *positive* evidence -- a name known to be one
    /// thing here, not merely absent from the other list:
    ///
    /// * for a call: a variable in scope (C lets a block-scope declaration shadow a typedef,
    ///   and `(fp)(x)` with a local `fp` is a call however `fp` is spelled elsewhere); a
    ///   function any unit of this import defines (`Context::functions` spans the import, so
    ///   a name one unit typedefs and another defines as a function meets itself here, and
    ///   the definition wins because a call to it has somewhere to go); or a prototype ([`Context::declared_functions`] -- `(free)(p)` with libc's
    ///   prototype in scope is the macro-suppression idiom, a call);
    /// * for a cast: a use of the name as a type anywhere in the unit
    ///   ([`Context::type_names`]), and at least one operand.
    ///
    /// Two shapes carry neither, and each draws a report instead of a silent guess. The
    /// lowering is what it always was -- a call, to a function `define_extern_functions` will
    /// invent -- but the census now sees it. `(T)()` with `T` a type casts nothing, which is
    /// not C: a source problem. A name the unit neither defines, declares, nor uses as a type
    /// is a frontend gap: the frontend cannot know, and if the name IS a type, the invented
    /// callee's empty body is where the operand's taint stops. Neither shape occurs in the
    /// dropbear, openssh, nginx, or linux corpora; the report is for the corpus that changes
    /// that.
    fn cast_shaped_call<'t>(
        &self,
        node: Node<'t>,
        source: &'a str,
        scope_view: &ScopeView,
    ) -> Result<Option<Node<'t>>, Error> {
        let Some(callee) = node.child_by_field_name("function") else {
            return Ok(None);
        };
        if callee.kind() != "parenthesized_expression" {
            return Ok(None);
        }
        let Some(inner) = first_named_child(callee) else {
            return Ok(None);
        };
        if !matches!(inner.kind(), "identifier" | "type_identifier") {
            return Ok(None);
        }
        let name = to_str(&inner, source);
        if self
            .scope_tree
            .find_variable(scope_view.sidx, name)
            .is_some()
            || self.functions.contains_key(name)
            || self.declared_functions.contains(name)
        {
            return Ok(None);
        }
        let operands = node.child_by_field_name("arguments");
        let has_operand = operands.is_some_and(|list| first_named_child(list).is_some());
        let is_type = self.type_names.contains(name);
        if !has_operand {
            // Zero operands can only be a call: a cast needs something to cast.
            if is_type {
                let (quote, _) = quote_construct(to_str(&node, source));
                malformed_source(format!(
                    "`{quote}` casts nothing: `{name}` is a type here and a cast needs an \
                     operand; lowered as a call to `{name}`"
                ))?;
            }
            return Ok(None);
        }
        if !is_type {
            let (quote, _) = quote_construct(to_str(&node, source));
            unexpected_ast(format!(
                "cast-shaped call `{quote}`: this unit neither defines, declares, nor uses \
                 `{name}` as a type, so it cannot be told from a cast; lowered as a call -- \
                 if `{name}` is a type, the operand's taint stops here"
            ))?;
            return Ok(None);
        }
        Ok(operands)
    }

    /// Lower the operands of a cast [`Context::cast_shaped_call`] recognised, and yield its
    /// value.
    ///
    /// A cast is value-preserving for taint -- the target type is irrelevant to dataflow --
    /// so this is the `cast_expression` arm's pass-through, reached through an `argument_list`
    /// instead of a `value` field. More than one operand means the cast is over a comma
    /// expression (`(T)(a, b)`, which tree-sitter cannot tell from a two-argument call
    /// either): every operand is evaluated for its effects, and the value is the last one's.
    fn flatten_cast_operands(
        &mut self,
        program: &mut Program,
        operands: Node<'_>,
        source: &'a str,
        scope_view: &mut ScopeView,
    ) -> Result<Exp, Error> {
        let mut cursor = operands.walk();
        let nodes: Vec<Node<'_>> = operands
            .named_children(&mut cursor)
            .filter(|child| child.kind() != "comment")
            .collect();
        let mut value = None;
        for child in nodes {
            value = Some(self.flatten_expr(program, child, source, scope_view)?);
        }
        Ok(value.expect("cast_shaped_call rejects an empty operand list"))
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
        // C++ instance-method / chained call: `recv.method(args)` (including a chained
        // receiver `recv.a(args).b(args2)`). Delegate to `dispatch_call`, which is driven by
        // the neutral `local_types`/`classes` maps (empty for C, so it returns `None` there —
        // no language branch). If it lowers the call, its result value is the answer.
        if let Some(out) =
            self.dispatch_call(program, node, source, scope_view, temp_name.clone())?
        {
            return Ok(out.value);
        }

        // Grouping parentheses around the callee are peeled first: they change nothing about
        // what is called, but a callee is named by its source text here, so `(f)(x)` without
        // the peel names a function `(f)` that does not exist. See [`unparenthesized_callee`].
        let func_node =
            unparenthesized_callee(node.child_by_field_name("function").expect("always has"));
        let func_name = to_str(&func_node, source);

        let arg_node = node.child_by_field_name("arguments").expect("always has");
        let args = self.collect_arguments(program, arg_node, source, scope_view)?;

        // Resolve an overloaded free callee to its arity-matching overload by the number of
        // explicit arguments (`id(a, b)` -> the `id#2` edge); a non-overloaded callee (all of
        // C, and every C++ name with a single arity) stays bare via the identity mangler. Then
        // a call names the definition the *caller's* file holds when several files define
        // that name; `resolve_reference` is the identity function for every other name. Only
        // the `DirectCall` (`GlobalHeap`) arm below consults this; a funcptr call ignores it.
        let call_edges = CallEdges::Explicit(ctadl_ir::thin_vec![
            self.resolve_reference(&self.overload_name(func_name, args.len()))
        ]);

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

        // A GNU statement expression in callee position produces a VALUE, and calling a value is
        // calling through it -- the kernel's `static_call(cond_resched)(...)`, which expands to
        // `({ ...; (&__SCT__cond_resched); })(...)`. The rule below cannot see that from the
        // access path alone: the value node resolves to the bare global path
        // `$globals.__SCT__cond_resched`, which is exactly the shape of a global callee that IS a
        // name (`hook(1)`, see `a_bare_global_callee_is_still_a_name`). What tells them apart is
        // the construct in callee position -- `({ ... })`, not `__SCT__cond_resched` -- so ask it
        // here. Left as a name, the callee was named by the braces' own source text, and since
        // the expansion embeds a `__UNIQUE_ID___...` counter every one of the 220 kernel call
        // sites invented an empty-bodied function of its own.
        let callee_is_a_value = is_statement_expression(func_node);

        // Direct call or indirect call, decided by what the callee *resolved to* rather than by
        // how it was spelled: a callee that is a name is a direct call to that name, and a
        // callee that is a location holding a function pointer is a call through what that
        // location holds. Every arm below reads the resolved access path, so the three storage
        // classes only differ in how a location is spelled, not in what counts as one.
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
            // A global's access path is `$globals.<name>.<fields>`, so the base variable being
            // the globals object says nothing about whether the callee is a name -- what says
            // it is the path. `$globals.hook` IS the object `hook`, and a bare global name is
            // still resolved by name (see `a_bare_global_callee_is_still_a_name` for why it has
            // to be). Anything past that leading segment is a location *inside* the object,
            // reached by a load: `ses.remoteclosed`, `ngx_os_io.send`, `ses.packettypes[i]
            // .handler` are function pointers a file-scope object holds, not functions, and
            // naming a call edge after the expression's source text both invented an empty
            // function to receive it and lost the indirect call the program actually makes.
            //
            // The second disjunct is that same conclusion reached from the construct instead of
            // from the path, for the one shape whose path cannot say it: see `callee_is_a_value`.
            Variable::GlobalHeap if access_path.fields.len() > 1 || callee_is_a_value => {
                log::debug!("This is an Indirect GLOBAL call: {func_name}");
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

    /// The IR name a reference to the function `name` from this unit resolves to.
    ///
    /// Almost always `name` itself. It differs only for a name several definitions claim,
    /// where C resolves the reference within the referring translation unit first: a call to
    /// `g` in `ssh-agent.c` means *that* file's `static g`, not the one `ssh-add.c` happens
    /// to define under the same name. A unit that defines no `g` falls back to the bare name,
    /// which is both what C does (the external definition) and what keeps an undefined `g`
    /// resolvable -- [`define_extern_functions`], and every taint model, matches by name.
    fn resolve_reference(&self, name: &str) -> String {
        self.unit_plan
            .local_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Lower this unit's definitions, each through [`Context::lower_function`]: skipping the
    /// copies [`plan_definitions`] said another unit already contributes, and using the IR
    /// name it gave the ones that could not keep their own.
    fn lower_definitions(
        &mut self,
        source: &'a str,
        tree: &Tree,
        program: &mut Program,
        global_sidx: usize,
        query: &Query,
    ) -> anyhow::Result<(), Error> {
        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches_iter.next() {
            let extract = MatchExtractor::new(query, m);
            //boo, so TREE_SITTER doesn't add a node for an implicit int function type
            let return_type = extract.get_opt("return_type");
            let body_node = extract.get("body")?;
            let def_node = extract.get("func.def")?;
            // A definition the grammar's auxiliary pass owns (a C++ class member or namespaced
            // function, lowered under its qualified name by `collect_aux`) is not one of this
            // loop's; `collect_definitions` skipped it the same way. Never true for C.
            if (self.hooks.aux_owns_definition)(def_node) {
                continue;
            }
            // Already reported by `collect_definitions`, which saw the same matches.
            let Some(head) = function_head(extract.get("func.decl")?) else {
                continue;
            };
            let written = to_str(&head.name, source);
            // A definition another translation unit already contributed, character for
            // character: one function, lowered once, at the first copy.
            if self.unit_plan.duplicates.contains(&def_node.id()) {
                log::debug!("skipping a repeated definition of `{written}`");
                continue;
            }
            // The name it was registered under: arity-mangled when the name is overloaded
            // (C++ only; the identity for C), then whatever `plan_definitions` decided for a
            // name several definitions claim.
            let mangled = self.overload_name(written, param_arity(head.params));
            let func_name = self.unit_plan.name_of(def_node.id(), &mangled).to_string();
            // C allows an implicit `int` return type (no `type` node); only an explicit `void`
            // is arity 0. A pointer return is a return: `void *xmalloc(size_t)` has `type: void`
            // and an arity of 1, because the `void` describes the pointee, not the function.
            let return_is_void = !head.returns_pointer
                && return_type.is_some_and(|rt| to_str(&rt, source).eq_ignore_ascii_case("void"));
            self.lower_function(
                source,
                program,
                global_sidx,
                &func_name,
                return_is_void,
                head.params,
                body_node,
                None,
                false,
                &[],
            )?;
        }
        Ok(())
    }

    /// Lower one `function_definition` into its IR function: register the name, set the return
    /// arity, build the parameter and body scopes, pre-create a block per `goto` label, walk the
    /// body, flag the label blocks the walk never reached, and finalize terminators.
    ///
    /// Split out of [`Context::lower_definitions`] so the per-function lowering is one named unit
    /// rather than an 80-line loop body. That keeps the query/dispatch loop readable, and it is
    /// what lets a change in here be reviewed (and merged) as a diff against a function instead of
    /// against an anonymous block.
    ///
    /// Both frontend entry points funnel through here. A free function passes
    /// `class_context = None, has_implicit_this = false`. A C++ **instance** method / constructor
    /// passes `Some(class_name), true`: an implicit `this` parameter (`ByRef`) is installed at
    /// index 0 (so writes the body makes to `this.<member>` propagate back to the caller's
    /// receiver, exactly like an out-param), the declared parameters number from 1, and
    /// `current_method_class` is set so the body's unqualified member names resolve. A C++
    /// **static** method (spec 015) passes `Some(class_name), false`: it has a class context (so an
    /// unqualified `static` data member resolves to its class-scoped global `Class::<member>`) but
    /// *no* `this`, so its declared parameters number from 0, exactly like a free function.
    /// `func_name` is the IR name and the resolution key — a free function's bare name, a method's
    /// qualified `Class::method`, or a static method's likewise-qualified `Class::method`.
    ///
    /// `member_inits` is opaque prologue data: `(name, expression-node)` pairs that the caller
    /// gathered and that this function only forwards to the `ctor_prologue` hook, which decides
    /// what they mean and what to emit. Empty for every free function and for all of C, whose
    /// hook is a no-op. (C++ passes a constructor's member-initializer list; see
    /// `cpp::cpp_emit_member_inits`.)
    #[allow(clippy::too_many_arguments)]
    fn lower_function(
        &mut self,
        source: &'a str,
        program: &mut Program,
        global_sidx: usize,
        func_name: &str,
        return_is_void: bool,
        param_list: Node<'_>,
        body_node: Node<'_>,
        class_context: Option<&str>,
        has_implicit_this: bool,
        member_inits: &[(String, Node<'_>)],
    ) -> anyhow::Result<(), Error> {
        self.allocator.reset();
        // Per-function state: a fresh local→class-type map (filled as we walk declarations),
        // a fresh reference-alias map (`T& r = x`), and the enclosing class for member
        // resolution (set for an instance *or* static method body — a static method has a class
        // context but no `this`, so its unqualified static-member names still resolve).
        self.local_types.clear();
        self.reference_aliases.clear();
        self.current_method_class = class_context.map(str::to_string);
        // Address-of aliases are function-local and confined to a straight-line block.
        self.addr_alias.clear();
        // Union-typed locals are function-scoped.
        self.union_vars.clear();

        let fidx = *self
            .functions
            .get(func_name)
            .expect("every definition is registered by lower_units before any is lowered");

        let fdat = &mut program.functions[fidx];
        fdat.name = func_name.to_string();
        fdat.set_return_type(ReturnType {
            arity: if return_is_void { 0 } else { 1 },
        });

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

        // An instance method / constructor gets an implicit `this` at parameter 0, passed `ByRef`
        // so the existing out-param machinery carries member writes back to the caller; the
        // declared params follow, numbered from 1. A **static** method has a class context (for
        // static-member resolution) but *no* `this`, so its declared params number from 0 — like
        // a free function (spec 015).
        if has_implicit_this {
            fdat.params.push(ParameterType::ByRef);
            self.param_names
                .entry(func_name.to_string())
                .or_default()
                .push("this");
            self.scope_tree
                .add_variable(param_sidx, "this".to_string(), VarKind::Parameter);
        }

        let body_name = format!("{}.body", func_name);
        self.collect_params(source, &param_list, fdat, func_name, &para_scope_view)?;

        //we have to build this one by hand, becuase we want the initial scope without the extra block
        let block_scope = self.scope_tree.add_scope(body_name, Some(param_sidx));
        let mut block_scope_view = ScopeView {
            func_name: func_name.to_string(),
            fidx,
            blidx,
            sidx: block_scope,
            continuation_blidx: None,
            break_target: None,
            continue_target: None,
            explainer: "initial_block".to_string(),
        };
        self.scope_tree.add_block(&block_scope_view);
        let cp = CompoundProxy::from_node(body_node);

        // Pre-create a block for every `goto` label in this function so forward
        // jumps (a `goto L` appearing before `L:`) resolve. Reset per function.
        self.label_blocks.clear();
        self.walked_label_blocks.clear();
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

        // Whatever the language puts in a function's prologue, emitted before the body. C has
        // nothing; C++ has a constructor's member-initializer list. What that means is the
        // hook's business — the core only fixes the point at which it runs.
        let ctor_prologue = self.hooks.ctor_prologue;
        ctor_prologue(self, program, &mut block_scope_view, source, member_inits)?;

        self.walk_compound_statement(source, program, &block_scope_view, &cp)?;

        // Label blocks the walk never entered. In a damaged body they are the parse
        // recovery's labels, not this function's: say so once, as a source problem, and
        // let `finalize_terminators` patch them without blaming the frontend.
        let stranded: HashSet<BasicBlockIdx> = self
            .label_blocks
            .values()
            .copied()
            .filter(|blidx| !self.walked_label_blocks.contains(blidx))
            .collect();
        let body_holds_recovery = body_node.has_error();
        if body_holds_recovery && !stranded.is_empty() {
            self.report_unanalyzed_recovery(func_name)?;
        }
        finalize_terminators(program, fidx, func_name, &stranded, body_holds_recovery)?;
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
            // The store side of the same repair: a name tree-sitter inserted quotes to the
            // empty string and names no location, so the store lands on a dead temp.
            "identifier" if to_str(&node, source).is_empty() => {
                self.report_missing_name(node, scope_view)?;
                Ok(self.dead_temp_path(program, scope_view))
            }
            "identifier" => Ok(self.build_access_path(
                to_str(&node, source),
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            )),
            // C++ `this` and a qualified name (`ns::f`, `Class::member`) are locations named
            // by a single (possibly qualified) symbol, exactly like an identifier — `this`
            // resolves to the implicit parameter 0, a qualified name to the global of that
            // name. Neither node kind occurs under the C grammar, so both arms are inert for C.
            "this" | "qualified_identifier" => Ok(self.build_access_path(
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
                // `p->` with the member name missing: tree-sitter inserted the
                // `field_identifier`, so it quotes to the empty string and cannot be a path
                // segment. Appending nothing would silently alias the whole access to `p`, so
                // the object's effects stay lowered and the access itself names a dead temp.
                if to_str(&field, source).is_empty() {
                    self.report_missing_name(field, scope_view)?;
                    return Ok(self.dead_temp_path(program, scope_view));
                }
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
                // Grammar-shape hook: C exposes the index directly on an `index` field, C++
                // nests it under `indices` (it allows multi-arg `a[i, j]`). Reading it through
                // the hook keeps this arm language-neutral.
                let index = self.flatten_expr(
                    program,
                    (self.hooks.subscript_index)(node),
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
                    None => self.dead_temp_path(program, scope_view),
                };
                scope_view.sidx = outer_sidx;
                self.cur_span = outer_span;
                Ok(path)
            }
            // A string literal as a location: `"\004\002\006\006"[(flags) & 3]`, the
            // kernel's `ACC_MODE()`. C makes a string literal an object -- an unnamed array
            // of char with static storage -- so it is a legitimate subscript base and a
            // legitimate operand of `&`, and `flatten_lvalue` is reached for the *base* of a
            // subscript even when the whole expression is a pure read. It is also a
            // compile-time constant: there is nothing to store into it and nothing in it to
            // taint, the same reason `flatten_expr` lowers `sizeof`/`_Alignof` operands to a
            // constant rather than walking them. So give it a location nothing else names,
            // and the read lowers to a load that yields nothing. Falling through to the
            // catch-all reported `not an lvalue: string_literal` and burned an anonymous
            // temp -- the identical recovery, minus the false accusation.
            "string_literal" | "concatenated_string" => {
                Ok(self.dead_temp_path(program, scope_view))
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
            // A cast in lvalue position. The cast itself is value-preserving (`flatten_expr`'s
            // `cast_expression` arm passes the operand straight through), so the location a
            // cast names is the location its operand names -- which is why `((struct S *)p)->f
            // = v` and `*(int *)(t->q) = v` already resolved through the catch-all below, where
            // the operand happens to lower to a variable.
            //
            // What did not resolve is the one thing C has a cast FOR in this position: naming
            // an address that is not any declared object. `(T *)<constant>` is an address
            // constant, and the operand lowers to an `Exp::Str`, not a variable, so the
            // catch-all charged the frontend with `not an lvalue: cast_expression` and dropped
            // the access onto a dead temp. That is the whole of spec 100's class -- 2,177
            // occurrences across the kernel corpus, every single one of them `(T *)0`, from
            // `container_of()`'s type check (`__same_type(*(ptr), ((type *)0)->member)`, which
            // names a member's TYPE and evaluates nothing) -- and a driver's
            // `*(volatile u32 *)0xfee00300` is the same construct with a store behind it.
            //
            // A constant address is an lvalue in C: `*(T *)K` designates the object at `K`, and
            // two such designations with the same `K` designate the SAME object. So give it a
            // location that says exactly that -- a field of the globals object named after the
            // constant -- rather than a fresh temp per occurrence, which would silently make
            // every reference to one hardware register a distinct location. The name cannot
            // collide with anything the program declares because `<...>` is not C identifier
            // syntax (the trick `next_temp` uses for `<tN>` and spec 090 for
            // `<implicit-return>`), and it is never empty, which is the invariant spec 070's
            // `"."` access path broke.
            //
            // Only a *cast* gets this reading. A bare constant in location position (`3[a] = b`
            // -- legal C, the commuted subscript) is not an address and still says so through
            // the catch-all: the rule keys on the construct that turns a constant into an
            // address, not on "the value came out constant".
            "cast_expression" => match self.flatten_expr(program, node, source, scope_view)? {
                Exp::Variable(v) => Ok(RawPath::new(v, ThinVec::new())),
                Exp::Str(constant) if !constant.is_empty() => Ok(literal_address_path(&constant)),
                _ => self.not_a_location(program, node, scope_view),
            },
            _ => match self.flatten_expr(program, node, source, scope_view)? {
                Exp::Variable(v) => Ok(RawPath::new(v, ThinVec::new())),
                _ => self.not_a_location(program, node, scope_view),
            },
        }
    }

    /// The recovery for a node in location position that names no location: say whose fault
    /// that is, then target a dead temp so the one access is dropped and the rest of the
    /// function still lowers.
    ///
    /// Spec 064's rule, applied to the store side: before blaming the frontend, ask whether the
    /// parser reached here from well-formed source. A node the recovery produced or re-parented
    /// names no location because it is not the program -- the kernel's `min()` over a `typeof`
    /// of a cast (a tree-sitter-c 0.24.1 grammar limit) leaves whole statements re-parented into
    /// a chain of `assignment_expression`s that never appeared in the source, and charging the
    /// frontend with "not an lvalue: assignment_expression" asserts ctadl failed to support a
    /// construct nobody wrote. Say once that this body holds recovery output, and drop the store
    /// silently like every other node in the region.
    fn not_a_location(
        &mut self,
        program: &mut Program,
        node: Node<'_>,
        scope_view: &ScopeView,
    ) -> Result<RawPath, Error> {
        if recovery_region(node).is_some() {
            let func_name = scope_view.func_name.clone();
            self.report_unanalyzed_recovery(&func_name)?;
        } else {
            unexpected_ast(format!("not an lvalue: {}", node.kind()))?;
        }
        Ok(self.dead_temp_path(program, scope_view))
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
use anyhow::Result;
use tree_sitter::Node;

// A simple counter to generate unique temp names (t0, t1, t2...)
#[derive(Debug, Default)]
struct TempAllocator {
    counter: usize,
}

impl TempAllocator {
    fn next_temp(&mut self) -> String {
        let name = format!("<t{}>", self.counter);
        self.counter += 1;
        name
    }
    fn reset(&mut self) {
        self.counter = 0;
    }
}

/// The switch behind [`unexpected_ast`] and [`malformed_source`]: by default log a
/// warning (prefixed with who is at fault) and return `Ok(())` so the call site can
/// recover and the user still gets useful results from the rest of the program. Set
/// `CTADL_ERROR_ON_AST` (to any value) to promote every such report to a hard
/// ingestion error, which is what you want when hunting frontend gaps.
fn recoverable_report(attribution: &'static str, msg: String) -> Result<(), Error> {
    #[cfg(test)]
    REPORTS.with(|r| r.borrow_mut().push((attribution, msg.clone())));
    if error_on_ast() {
        Err(Error::TreeSitterParse(msg))
    } else {
        log::warn!("{attribution}: {msg} (recovering; set CTADL_ERROR_ON_AST to fail instead)");
        Ok(())
    }
}

// Test-only: every `recoverable_report` made on this thread, newest last. Lets a test assert
// not just that ingestion survived but *what* was reported and who was blamed -- the
// distinction spec 064 turns on, since a suppressed warning and a re-attributed one are both
// invisible to a test that only checks the program came out. (A plain comment, not a doc
// comment: rustdoc does not document items produced by a macro invocation.)
#[cfg(test)]
thread_local! {
    static REPORTS: std::cell::RefCell<Vec<(&'static str, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Test-only: drain and return this thread's reports. Each `#[test]` runs on its own
/// thread, so the log starts empty per test and needs no explicit reset.
#[cfg(test)]
fn take_reports() -> Vec<(&'static str, String)> {
    REPORTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
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

/// Attribution prefix of an [`unexpected_ast`] warning: this frontend is at fault.
const FRONTEND_GAP: &str = "frontend gap";

/// Attribution prefix of a [`malformed_source`] warning: the analyzed code is at fault.
const SOURCE_PROBLEM: &str = "source problem";

/// How much of an unparsable construct a parse-error warning quotes, in characters.
/// Preprocessed kernel source puts a whole macro expansion on one line, so an `ERROR`
/// node there is routinely kilobytes long -- the longest in the kernel census is 23,268
/// characters -- and the old message embedded all of it.
///
/// 200 rather than something tighter because the quote has to stay *diagnostic*:
/// `run-linux.sh`'s triage identifies the grammar limit behind each parse error by finding
/// the `typeof(`/`...` marker in this text, and in the corpus's worst case (a
/// `WRITE_ONCE(x, min_t(...))` expansion) that marker sits at character 142. A 120-char
/// quote was tried first and left 9 of the census's 180 constructs unclassifiable.
const PARSE_ERROR_QUOTE_CHARS: usize = 200;

/// The parse-recovery region `node` belongs to, or `None` if the parser produced it
/// from source it parsed cleanly.
///
/// tree-sitter signals a syntax error in two ways and only the first is a node kind:
/// an `ERROR` node covering text it could not parse, and -- once it resumes -- an
/// ordinary, perfectly well-formed subtree re-parented somewhere it does not belong.
/// The kernel corpus is dominated by the second. An unparsable
/// `__typeof(__builtin_choose_expr(...))` in a top-level declarator (`SYSCALL_DEFINE`)
/// leaves the ~87 function definitions that follow it re-parented into the *previous*
/// function's `compound_statement`, where each looks exactly like a GNU nested
/// function. Not one of them is inside an `ERROR` node, so an ancestry-only test finds
/// nothing; what marks them is that the body holding them did not parse.
///
/// So: the innermost enclosing `ERROR` node if there is one, else the innermost enclosing
/// `compound_statement` whose own subtree failed to parse.
///
/// The `compound_statement` fallback deliberately stops at the *first* enclosing body
/// rather than walking to the root. It has to: in `fs__read_write.c` the parse fails so
/// badly that the **root node itself is an `ERROR`**, and in `net__ipv4__route.c` one
/// top-level `ERROR` spans 2.1 MB of the 3.2 MB file. A rule that looked for damage
/// anywhere above would excuse every gap in those translation units. Stopping at the first
/// enclosing body keeps a construct inside a cleanly parsed body reported as the frontend
/// gap it is, even when the function next to it is wreckage.
fn recovery_region(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_error() {
        return Some(node);
    }
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.is_error() {
            return Some(n);
        }
        if n.kind() == "compound_statement" {
            return n.has_error().then_some(n);
        }
        cur = n.parent();
    }
    None
}

/// The text of an unparsable construct, normalized for a one-line warning: runs of
/// whitespace collapse to a single space (so one warning is one log line, and
/// `grep -c` counts warnings rather than source lines) and the result is cut at
/// [`PARSE_ERROR_QUOTE_CHARS`]. Returns the quote and how many characters were elided.
fn quote_construct(text: &str) -> (String, usize) {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = one_line.chars().count();
    if len <= PARSE_ERROR_QUOTE_CHARS {
        (one_line, 0)
    } else {
        (
            one_line.chars().take(PARSE_ERROR_QUOTE_CHARS).collect(),
            len - PARSE_ERROR_QUOTE_CHARS,
        )
    }
}

/// An AST shape the frontend does not lower (unknown statement kinds, unsupported
/// expressions, declarators we don't recognize) — a gap in the frontend, not a
/// problem in the analyzed source. Call sites recover by skipping the construct or
/// substituting a fresh opaque temp.
fn unexpected_ast(msg: String) -> Result<(), Error> {
    recoverable_report(FRONTEND_GAP, msg)
}

/// A construct the analyzed source itself misuses (`break` outside a loop, `goto` to
/// an undefined label) — a problem in that code, not a frontend gap. Same switch as
/// [`unexpected_ast`]; the warning attributes the fault to the source.
fn malformed_source(msg: String) -> Result<(), Error> {
    recoverable_report(SOURCE_PROBLEM, msg)
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
fn debug_print_tree(
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

/// The leaf identifier a declarator ultimately names, descending through pointer
/// (`Box* p`), array, parenthesized, and C++ reference (`Box& r`) wrappers. Used to key a
/// class-typed local in [`Context::local_types`] by its plain name (`p`/`r`), so a later
/// `p->m(…)` / `r.m(…)` call can look up its class — `to_str` of the wrapping declarator
/// would otherwise yield `*p`, which never matches the receiver identifier. Returns `None`
/// for shapes that name no single identifier (e.g. a function declarator).
/// Whether `node` sits inside a class/struct body (a `field_declaration_list`). The
/// top-level `function_definition` query in [`Context::lower_definitions`] matches at any
/// depth, so a C++ inline constructor — a `function_definition` whose name is a plain
/// `identifier` *inside a class* — matches it just like a free function. Such members are
/// discovered and lowered by the C++ `collect_aux` hook, so the shared loop uses this to
/// skip them and avoid double-lowering. C trees contain no `field_declaration_list`, so
/// this returns `false` for every C definition and the C path is unaffected.
/// The **arity** of a `parameter_list` — the number of explicit `parameter_declaration`
/// children (a method's implicit `this` is never in the list, so this is the count the call
/// site's explicit-argument count matches). Used to key the neutral overload map and to
/// mangle an overloaded definition's IR name. Ignores punctuation and any variadic/`void`
/// marker (out of scope for arity-overloading), so an ordinary `f(int, int)` counts 2.
fn param_arity(param_list: Node<'_>) -> usize {
    let mut cursor = param_list.walk();
    param_list
        .children(&mut cursor)
        .filter(|c| c.kind() == "parameter_declaration")
        .count()
}

fn is_class_member_definition(node: Node<'_>) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "field_declaration_list" => return true,
            "translation_unit" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

/// Whether `node` sits inside a `namespace_definition`. A C++ free function defined in a
/// named namespace (`namespace ns { int f(){…} }`) is a `function_definition` with a plain
/// `identifier` name — indistinguishable to the top-level `function_definition` query in
/// [`Context::lower_definitions`] from a global free function — but it must be lowered under
/// its *qualified* name (`ns::f`) by the C++ `collect_aux` hook, not registered bare here.
/// The shared loop uses this neutral structural ancestor check (mirroring
/// [`is_class_member_definition`]) to skip it. C trees contain no `namespace_definition`, so
/// this returns `false` for every C definition and the C path is unaffected.
fn is_namespaced_definition(node: Node<'_>) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "namespace_definition" => return true,
            "translation_unit" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

fn declarator_leaf_ident<'s>(decl: Node<'_>, source: &'s str) -> Option<&'s str> {
    match decl.kind() {
        "identifier" => Some(to_str(&decl, source)),
        "pointer_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "reference_declarator" => decl
            .child_by_field_name("declarator")
            .or_else(|| decl.named_child(0))
            .and_then(|d| declarator_leaf_ident(d, source)),
        _ => None,
    }
}

// this returns the field expresion chained from the 1st field_expression,
// The final argument of kind "identifier" is returned, as it needs to be stuffed
// in the variable field, while the rest (the out_vec) is the path
