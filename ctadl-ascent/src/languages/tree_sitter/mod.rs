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

use ctadl_ir::index::index_vec::IndexVec;
use ctadl_ir::mir::*;

use internment::ArcIntern;
use smallvec::{SmallVec, smallvec};
use streaming_iterator::{IntoStreamingIterator, StreamingIterator};
use tree_sitter::{Parser, Query, QueryCapture, QueryCursor, QueryMatch, Tree};

mod test_utils;
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
    /// The tree-sitter query [`Context::collect_params`] runs over a `parameter_list` to
    /// classify each parameter (name + by-ref/by-value mode). C and C++ share the same
    /// declarator shapes for plain/pointer/array/function-pointer params; C++ additionally
    /// has the `reference_declarator` (`T& r`) node, which the C grammar lacks — so a query
    /// mentioning it cannot be compiled against the C grammar. Carrying the query string per
    /// grammar keeps that C++-only node out of the C path (the C string is byte-for-byte the
    /// historical one) without an `is_cpp` branch in the shared classifier. A `reference_`
    /// `declarator` is captured as `@is_ref_cpp`; the classifier then reads the `const`
    /// qualifier (grammar-neutral) to pick `ByVal` (`const T&`, inbound) vs `ByRef` (`T&`).
    param_query: &'static str,
    /// Discover and lower any *auxiliary* function definitions that the top-level
    /// `function_definition` query in [`Context::collect_functions`] does not reach. C has
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
        &'s ScopeView,
        Node<'t>,
        &'s str,
    ) -> anyhow::Result<bool, Error>,
    /// Populate [`Context::overloads`] — the neutral arity-overload map — *before* any
    /// function is registered or lowered. C has no overloading, so its hook is a no-op (the
    /// map stays empty and [`Context::overload_name`] is the identity). C++ uses it to scan
    /// every `function_definition` (free, namespaced, and member), grouping by IR base name and
    /// recording each definition's explicit-parameter arity, so the mangler can tell — at all
    /// four touchpoints, definition-side and call-side alike — which names are overloaded. It
    /// runs first (ahead of the top-level pre-pass and `collect_aux`) so the map is complete
    /// before the first `functions.entry(...)`.
    collect_overloads:
        for<'a, 't> fn(&mut Context<'a>, &'a str, Node<'t>) -> anyhow::Result<(), Error>,
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
        collect_overloads: |_ctx, _source, _root| Ok(()),
        // The historical C parameter query, verbatim: plain, pointer (`@is_ref`), array
        // (`@is_ref`), and function-pointer declarators. C has no `reference_declarator`.
        param_query: r#"
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
    "#,
    };
}

#[derive(Debug)]
struct Context<'a> {
    functions: HashMap<String, FunctionIdx>,
    param_names: HashMap<String, IndexVec<ParameterIdx, &'a str>>,
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
    /// submodule overrides them via [`Context::set_hooks`]. The shared walker reads
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
    reference_aliases: HashMap<String, AccessPath>,
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
}

impl Context<'_> {
    /// Create a lowering context driving `grammar`. Both frontend entry points build the
    /// context this way — `parse_c_program` with the C grammar, `parse_cpp_program` with
    /// the C++ grammar — so the shared lowering stays language-neutral.
    fn new(grammar: tree_sitter::Language) -> Self {
        Self {
            functions: HashMap::default(),
            param_names: HashMap::default(),
            scope_tree: ScopeTree::default(),
            allocator: TempAllocator::default(),
            label_blocks: HashMap::default(),
            grammar,
            hooks: GrammarHooks::C,
            classes: HashMap::default(),
            local_types: HashMap::default(),
            current_method_class: None,
            reference_aliases: HashMap::default(),
            overloads: HashMap::default(),
            subclasses: HashMap::default(),
        }
    }

    /// The neutral overload mangler consulted at all four overloading touchpoints (free- and
    /// method-function registration, free-call and method-dispatch edges). If `base` names an
    /// **overloaded** entity — present in [`Self::overloads`] with **≥2** distinct arities — it
    /// returns the arity-mangled IR name `base#arity`; otherwise it returns `base` unchanged.
    /// Because [`Self::overloads`] is empty for C (and holds a single arity for every
    /// non-overloaded C++ name), this is the **identity** for all of C and for every ordinary
    /// non-overloaded call — so wiring it into a touchpoint changes only genuinely overloaded
    /// names, and never introduces a language branch.
    fn overload_name(&self, base: &str, arity: usize) -> String {
        match self.overloads.get(base) {
            Some(arities) if arities.len() >= 2 => format!("{base}#{arity}"),
            _ => base.to_string(),
        }
    }

    /// Install grammar-shape adapters for the driving grammar. `parse_c_program` keeps the
    /// default [`GrammarHooks::C`]; `parse_cpp_program` calls this with `cpp::CPP_HOOKS`.
    fn set_hooks(&mut self, hooks: GrammarHooks) {
        self.hooks = hooks;
    }

    /// Compile a tree-sitter query against this context's grammar. Use this (not the
    /// free [`compile_query`], which is hard-wired to C) for any query run over a tree
    /// produced by this context, so the tree is queried with the grammar that parsed it.
    fn compile_query(&self, query_src: &str) -> Query {
        compile_query_for(&self.grammar, query_src)
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

    let mut ctx = Context::new(tree_sitter_c::LANGUAGE.into());
    let mut program = Program::default();
    let tree = parser
        .parse(source, None)
        .expect("tree‐sitter failed to parse");
    ctx.parse(source, &tree, &mut program)?;
    let marked_up = markup(&program, &ctx);
    Ok((program, tree.root_node().has_error(), marked_up))
}

pub fn compile_query(query_src: &str) -> Query {
    compile_query_for(&tree_sitter_c::LANGUAGE.into(), query_src)
}

/// Compile a tree-sitter query against an explicit grammar. The C frontend goes through
/// [`compile_query`] (C); the C++ frontend drives this with the C++ grammar via
/// [`Context::compile_query`]. A query must be compiled against the same grammar that
/// parsed the tree it runs over — tree-sitter matches by numeric symbol id, and the C and
/// C++ grammars assign different ids to the same node kind.
pub(crate) fn compile_query_for(language: &tree_sitter::Language, query_src: &str) -> Query {
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

        for &child in &compound.nodes {
            if !child.is_named() {
                continue; // we skip , ( stuff like that...
            }
            // A statement that diverges (return/break/continue, or a label whose body
            // diverges) ends the compound; the trailing fall-through link is skipped.
            if self.walk_statement(source, program, &mut scope_view, child)? {
                return Ok(());
            }
        }

        //walked off a compound_statement
        log::info!("EOCS linking blocks: ");
        link_blocks(program, &scope_view, scope_view_meowsers, true)?;

        Ok(())
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
                self.walk_compound_statement(source, program, &inner_view, &cp)?;
            }
            "declaration" => {
                self.walk_declaration(source, program, scope_view, child)?;
            }
            "assignment_expression" => {
                self.flatten_expr(program, child, source, scope_view)?;
            }
            "expression_statement" | "update_expression" => {
                if let Some(inner_child) = child.child(0) {
                    self.flatten_expr(program, inner_child, source, scope_view)?;
                }
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
            // fall-through, so they end the compound (skipping its end link).
            "return_statement" => {
                self.walk_return(source, program, scope_view, child)?;
                return Ok(true);
            }
            "break_statement" => {
                self.walk_break(program, scope_view)?;
                return Ok(true);
            }
            "continue_statement" => {
                self.walk_continue(program, scope_view)?;
                return Ok(true);
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
            if let Some(class) = &class_type {
                // Key the class-typed local by its leaf identifier so a pointer receiver
                // (`Box* p = &b`) is recorded as `p`, not `*p` — otherwise a later
                // `p->m(…)` (whose receiver is the identifier `p`) would not dispatch.
                let key = declarator_leaf_ident(decl_ident, source).unwrap_or(var_name);
                self.local_types.insert(key.to_string(), class.clone());
            }
            if let Some(vc) = nest_decl.child_by_field_name("value") {
                self.collect_assignment(source, program, scope_view, decl_ident, vc, None)?;
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
        scope_view: &ScopeView,
        ref_decl: Node<'_>,
        init: Node<'_>,
    ) -> Result<(), Error> {
        let name_node = ref_decl.named_child(0).filter(|n| n.kind() == "identifier");
        let value = init.child_by_field_name("value");

        if let Some(name_node) = name_node {
            let ref_name = to_str(&name_node, source);
            if let Some(val) = value
                && matches!(
                    val.kind(),
                    "identifier" | "field_expression" | "call_expression"
                )
                && let Exp::AccessPath(path) =
                    self.flatten_expr(program, val, source, scope_view)?
            {
                // Alias the reference to its referent's access path. A `call_expression`
                // referent is a reference-returning method (`Box& r = b.setV(x)`): its result
                // aliases the receiver object (registered in `reference_aliases` by
                // `dispatch_call`), so flattening it already yields that object's path — `r`
                // binds straight to the real object, and a later `r.m(…)` dispatches on it.
                self.reference_aliases.insert(ref_name.to_string(), path);
                return Ok(());
            }
            // Fallback: register a plain local and copy whatever initializer there is.
            self.scope_tree.add_variable(
                scope_view.sidx,
                ref_name.to_string(),
                VarKind::Local,
                None,
                None,
            );
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
        let condition_raw = child
            .child_by_field_name("condition")
            .expect("switch always has a condition");
        let condition = (self.hooks.condition_expr)(condition_raw);
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
    /// `scope_view.break_target` — no stack to consult.
    fn walk_break(&self, program: &mut Program, scope_view: &ScopeView) -> Result<(), Error> {
        match scope_view.break_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)
            }
            None => Err(Error::TreeSitterParse(
                "`break` outside of a switch or loop".to_string(),
            )),
        }
    }

    /// `continue`: terminate the current block with a goto to the innermost enclosing
    /// loop's re-test/update block (`scope_view.continue_target`).
    fn walk_continue(&self, program: &mut Program, scope_view: &ScopeView) -> Result<(), Error> {
        match scope_view.continue_target {
            Some(target) => {
                let mut to = scope_view.clone();
                to.blidx = target;
                link_blocks(program, scope_view, &to, false)
            }
            None => Err(Error::TreeSitterParse(
                "`continue` outside of a loop".to_string(),
            )),
        }
    }

    /// `goto L`: terminate the current block with a jump to label `L`'s block (created
    /// up front by the per-function pre-scan, so forward jumps resolve too). Unlike
    /// `break`/`continue`, this does NOT end the compound — statements after a `goto`
    /// are unreachable but may contain labels, so we keep lowering them into a fresh
    /// (unlinked) block.
    fn walk_goto(
        &mut self,
        source: &str,
        program: &mut Program,
        scope_view: &mut ScopeView,
        child: Node<'_>,
    ) -> Result<(), Error> {
        let label_node = child
            .child_by_field_name("label")
            .expect("goto_statement always has a label");
        let label = to_str(&label_node, source);
        let target = *self.label_blocks.get(label).ok_or_else(|| {
            Error::TreeSitterParse(format!("`goto` to undefined label `{label}`"))
        })?;
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
        let condition_raw = child
            .child_by_field_name("condition")
            .expect("always has condition");
        let condition = (self.hooks.condition_expr)(condition_raw);
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
        field_path: FieldAccesses,
        scope_view: &ScopeView,
    ) -> AccessPath {
        // A C++ reference local (`T& r = x`) aliases its referent: resolve the name to the
        // referent's access path and append any trailing field accesses. The map is empty
        // for C (no `reference_declarator`), so this never fires on the C path.
        if let Some(aliased) = self.reference_aliases.get(name_pre_scope) {
            let mut ap = aliased.clone();
            ap.path.fields.extend(field_path.fields);
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
            // Inside a method body, an unqualified name that is a data member of the
            // enclosing class (and was not shadowed by a local/parameter — those are found
            // above) resolves to `this.<member>`: the implicit `this` is parameter 0
            // (installed by `lower_function`), so the member becomes the leading field on it.
            // Any trailing `field_path` (rare for the in-scope method slice) follows. This is
            // data-driven on the neutral `classes` map, which is empty for C — no language branch.
            return ctadl_ir::mir::AccessPath {
                variable_ref: VariableRef::new_parameter(0u32.into()),
                path: std::iter::once(FieldAccess::Symbol(name_pre_scope.into()))
                    .chain(field_path.fields)
                    .collect(),
            };
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
        function_name: &str,
        scope_view: &ScopeView,
        start_idx: usize,
    ) -> anyhow::Result<(), Error> {
        // The classifier query comes from the grammar hooks: C and C++ share the plain/
        // pointer/array/function-pointer declarator shapes; the C++ string additionally
        // matches the `reference_declarator` (`T& r`) node (captured `@is_ref_cpp`), which
        // the C grammar lacks. Keeping it per-grammar keeps that C++-only node out of the
        // query compiled against the C grammar — no `is_cpp` branch in this classifier.
        let query_src = self.hooks.param_query;
        //       debug_print_tree(*param_list, 0, None, None); //depth, field_name, depth_limit);
        // Compile against this context's grammar *before* taking the mutable borrow of
        // `param_names` below (both touch `self`).
        let query = self.compile_query(query_src);

        let param_names = self
            .param_names
            .entry(function_name.to_string())
            .or_default();

        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, *param_list, source.as_bytes());

        // `start_idx` lets an implicit leading parameter (a method's `this`, installed by
        // `lower_function`) occupy index 0, so the declared params number from 1.
        let mut ctr = start_idx;
        while let Some(m) = matches_iter.next() {
            let extract = MatchExtractor::new(&query, m);
            let param_name = extract.get("var_name")?;
            let is_ref = extract.get_opt("is_ref");

            // Classify the parameter's passing mode. Pointer/array out-params are `ByRef`
            // (the value can be written back) — the historical C behavior. A C++ lvalue
            // reference (`@is_ref_cpp`, only ever captured by the C++ query) is also storage
            // shared with the caller, but `const T&` is read-only: model `const T&` as
            // `ByVal` (the referent's value flows in, nothing flows back) and a non-const
            // `T&` as `ByRef` (write-back), exactly like a pointer out-param. The `const`
            // probe is grammar-neutral (`type_qualifier` exists in both grammars) and only
            // reached for a reference param, so the C path is unaffected.
            let param_type = if let Some(ref_decl) = extract.get_opt("is_ref_cpp") {
                let param_decl = ref_decl
                    .parent()
                    .expect("a reference_declarator is a child of its parameter_declaration");
                if node_has_const_qualifier(&param_decl, source) {
                    ParameterType::ByVal
                } else {
                    ParameterType::ByRef
                }
            } else if is_ref.is_some() {
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
            "identifier" => {
                // A bare identifier that names a known function (and is not shadowed by
                // a variable in scope) is a function *reference* used as a value -- the
                // RHS of `fp = id`, an initializer `int (*fp)(int) = id`, a call argument
                // `apply(id, x)`, or a field store `o.op = id`. Lower it as a
                // function-pointer object (as the pcode backend does) so codegen emits
                // the `func_ptr_assign` fact that indirect-call taint resolution needs;
                // otherwise `id` is treated as a plain global and taint is dropped (F1).
                // Direct calls are unaffected: `collect_call` resolves an identifier
                // callee via `build_access_path`, not through here.
                if self
                    .scope_tree
                    .find_variable(scope_view.sidx, text)
                    .is_none()
                    && self.functions.keys().any(|f| f == text)
                {
                    Ok(Exp::ObjectRef(CallObject::FunctionPtr(text.into())))
                } else {
                    Ok(Exp::AccessPath(self.build_access_path(
                        text,
                        Default::default(),
                        scope_view,
                    )))
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
            // The C++ `this` receiver — inside a method body, `this` is the implicit param 0
            // installed by `lower_function`, so it resolves to `@p0`. `*this` reaches here via
            // the `pointer_expression` arm above (its argument is this node). A `this` node
            // never occurs under the C grammar, so this arm is inert for C.
            "this" => Ok(Exp::AccessPath(self.build_access_path(
                "this",
                Default::default(),
                scope_view,
            ))),
            // A C++ qualified name used as a value or callee (`ns::f`, `ns::Box::m`). Its
            // source text is the fully-qualified string (`ns::f`), which resolves to a global
            // by that name — so a qualified call `ns::f(args)` (whose `function` child is this
            // node) lowers via `collect_call` to a `DirectCall Explicit(["ns::f"])` that
            // matches the namespaced definition lowered under the same qualified IR name. A
            // `qualified_identifier` node never occurs under the C grammar, so this is inert
            // for C.
            "qualified_identifier" => Ok(Exp::AccessPath(self.build_access_path(
                text,
                Default::default(),
                scope_view,
            ))),
            "subscript_expression" => self.flatten_subscript(program, node, source, scope_view),
            "call_expression" => {
                let x = self.allocator.next_temp();
                self.collect_call(program, node, source, scope_view, x)
            }
            // C++ `delete p;` — destroying a heap object is a taint **no-op** (the object is
            // gone; its taint is irrelevant afterward), so this lowers to nothing that moves
            // taint and yields a fresh throwaway temp. A `delete_expression` node never occurs
            // under the C grammar, so this arm is inert for C (like the `this` arm above).
            "delete_expression" => {
                let temp = self.allocator.next_temp();
                Ok(Exp::AccessPath(self.build_access_path(
                    temp.as_str(),
                    Default::default(),
                    scope_view,
                )))
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
            (self.hooks.subscript_index)(node),
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
        source: &str,
        scope_view: &ScopeView,
    ) -> Result<Option<RecvObj>, Error> {
        match recv_node.kind() {
            "identifier" => {
                let name = to_str(&recv_node, source);
                match self.local_types.get(name).cloned() {
                    Some(class) => {
                        let exp = Exp::AccessPath(self.build_access_path(
                            name,
                            Default::default(),
                            scope_view,
                        ));
                        Ok(Some(RecvObj { exp, class }))
                    }
                    None => Ok(None),
                }
            }
            // The implicit receiver inside a method body: `this.m()` (rare) or `(*this).m()`.
            "this" => match self.current_method_class.clone() {
                Some(class) => {
                    let exp = Exp::AccessPath(self.build_access_path(
                        "this",
                        Default::default(),
                        scope_view,
                    ));
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
        source: &str,
        scope_view: &ScopeView,
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
        let targets: SmallVec<[String; 4]> = bases
            .iter()
            .map(|b| self.overload_name(b, explicit_args.len()))
            .collect();
        let mut method_args: SmallVec<[Exp; 4]> = smallvec![recv.exp.clone()];
        method_args.extend(explicit_args);
        program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
            StatementKind::CallAssign {
                style: CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(targets),
                },
                rets: vec![VariableRef::new_local(temp_name.clone())].into(),
                args: method_args,
            },
        ));

        // A receiver-returning method makes the call's result an alias to the receiver
        // object. Register the result temp as an alias of that object so `Class& r = call`
        // and any read of the temp resolve to the real object, and surface it so a chained
        // `.n(…)` dispatches on the same object (the by-ref receiver carries chained writes
        // back to it). Only class-object receivers (an `AccessPath`) can alias.
        let aliased = if returns_self && matches!(recv.exp, Exp::AccessPath(_)) {
            if let Exp::AccessPath(recv_path) = &recv.exp {
                self.reference_aliases
                    .insert(temp_name.clone(), recv_path.clone());
                self.local_types
                    .insert(temp_name.clone(), recv.class.clone());
            }
            Some(recv.clone())
        } else {
            None
        };
        let value = Exp::AccessPath(self.build_access_path(
            temp_name.as_str(),
            Default::default(),
            scope_view,
        ));
        Ok(Some(DispatchOut { value, aliased }))
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
        // C++ instance-method / chained call: `recv.method(args)` (including a chained
        // receiver `recv.a(args).b(args2)`). Delegate to `dispatch_call`, which is driven by
        // the neutral `local_types`/`classes` maps (empty for C, so it returns `None` there —
        // no language branch). If it lowers the call, its result value is the answer.
        if let Some(out) =
            self.dispatch_call(program, node, source, scope_view, temp_name.clone())?
        {
            return Ok(out.value);
        }

        let func_node = node.child_by_field_name("function").expect("always has");
        let arg_node = node.child_by_field_name("arguments").expect("always has");
        let func_name = to_str(&func_node, source);

        let args = self.collect_arguments(program, arg_node, source, scope_view)?;

        // Resolve an overloaded free callee to its arity-matching overload by the number of
        // explicit arguments (`id(a, b)` -> the `id#2` edge); a non-overloaded callee (all of
        // C, and every C++ name with a single arity) stays bare via the identity mangler. Only
        // the `DirectCall` (`GlobalHeap`) arm below consults this; a funcptr call ignores it.
        let call_edges = CallEdges::Explicit(smallvec![self.overload_name(func_name, args.len())]);

        // Resolve the callee. A plain `foo(...)` is an identifier; the legacy
        // dereference form `(*op_func)(...)` wraps the pointer in a
        // parenthesized/pointer expression, so route it through flatten_expr to
        // recover the underlying variable (`op_func`).
        let access_path = if func_node.kind() == "identifier" {
            self.build_access_path(func_name, Default::default(), scope_view)
        } else {
            match self.flatten_expr(program, func_node, source, scope_view)? {
                Exp::AccessPath(ap) => ap,
                _ => self.build_access_path(func_name, Default::default(), scope_view),
            }
        };

        let var = &*access_path.variable_ref.variable;
        let style = match var {
            Variable::Local(name) => {
                log::info!("This is an Indirect LOCAL call: {}", name);
                CallStyle::FuncPtrCall {
                    callee: access_path,
                    signature: (Some("indirect-call".to_string())),
                }
            }
            Variable::Param(idx) => {
                log::info!("This is an Indirect PARAMETER call: {}", idx.get());
                CallStyle::FuncPtrCall {
                    callee: access_path,
                    signature: (Some("indirect-call".to_string())),
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

        let query = self.compile_query(query_src);

        // Overload discovery FIRST — before any `functions.entry(...)` below or in the
        // `collect_aux` hook — so the neutral `overloads` map is complete and the mangler can
        // tell which names are overloaded at registration time. No-op for C (map stays empty).
        let discover = self.hooks.collect_overloads;
        discover(self, source, tree.root_node())?;

        // Pre-pass: register every function name up front so a function-pointer
        // reference to a function defined LATER in the file (`fp = later;`) is already
        // known when its using function is lowered. Without this, `flatten_expr` would
        // not recognise `later` as a function and would drop the indirect-call taint.
        let mut name_cursor = QueryCursor::new();
        let mut name_matches = name_cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = name_matches.next() {
            let extract = MatchExtractor::new(&query, m);
            if let Ok(name_node) = extract.get("func.name") {
                // The `function_definition` query matches at any depth. A C++ inline
                // constructor (`Box(int){…}`) is a `function_definition` whose name is a
                // plain `identifier` *inside a class body*, and a namespaced free function
                // (`namespace ns { int f(){…} }`) is one whose name is a plain `identifier`
                // *inside a namespace* — both match too, but each is owned by the `collect_aux`
                // hook (lowered as `Class::Class` / `ns::f`). Skip them here so they are not
                // also registered/lowered bare. Inert for C (no class/namespace bodies).
                if is_class_member_definition(name_node) || is_namespaced_definition(name_node) {
                    continue;
                }
                let func_name = to_str(&name_node, source);
                // Register under the (possibly arity-mangled) overload name so an overloaded
                // free function reserves a *distinct* `FunctionIdx` per arity (`id#1`, `id#2`)
                // instead of colliding on the bare `id`; a non-overloaded name stays bare.
                let arity = extract
                    .get("param_list")
                    .map(param_arity)
                    .unwrap_or_default();
                let registered = self.overload_name(func_name, arity);
                self.functions
                    .entry(registered)
                    .or_insert_with(|| program.new_function());
            }
        }

        // Discover and lower any functions the top-level query above can't see (C++ inline
        // methods, named by a `field_identifier` inside a class). The C hook is a no-op.
        // This runs first so a top-level body's `recv.method(…)` call resolves its callee,
        // and so the method's class is registered for member resolution. No language branch
        // here — the shared core just calls through the installed hook.
        let aux = self.hooks.collect_aux;
        aux(self, source, tree.root_node(), program, global_sidx)?;

        // Each match binds *all* captures. Lower every top-level function via the shared
        // `lower_function`, which methods also funnel through (with an implicit `this`).
        let mut cursor = QueryCursor::new();
        let mut matches_iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches_iter.next() {
            let extract = MatchExtractor::new(&query, m);
            //boo, so TREE_SITTER doesn't add a node for an implicit int function type
            let return_type = extract.get_opt("return_type");
            let func_name_node = extract.get("func.name")?;
            // A C++ inline constructor or a namespaced free function matches this query but is
            // owned by the `collect_aux` hook (lowered as `Class::Class` / `ns::f`); skip it so
            // it is not double-lowered (inert for C).
            if is_class_member_definition(func_name_node)
                || is_namespaced_definition(func_name_node)
            {
                continue;
            }
            let param_list = extract.get("param_list")?;
            let body_node = extract.get("body")?;
            //debug_print_tree(body_node, 0, None, Some(50));
            let func_name = to_str(&func_name_node, source);
            // C allows an implicit `int` return type (no `type` node); only an explicit
            // `void` is arity 0.
            let return_is_void =
                return_type.is_some_and(|rt| to_str(&rt, source).eq_ignore_ascii_case("void"));
            // Lower under the (possibly arity-mangled) overload name, matching the pre-pass
            // registration and the call-site edge so `id#1`/`id#2` are two distinct functions
            // (neither clobbers the other); a non-overloaded name stays bare.
            let lowered_name = self.overload_name(func_name, param_arity(param_list));
            self.lower_function(
                source,
                program,
                global_sidx,
                &lowered_name,
                return_is_void,
                param_list,
                body_node,
                None,
                &[],
            )?;
        }
        Ok(())
    }

    /// Lower a single function body with the shared lowering core: allocate (or reuse) its
    /// `FunctionIdx`, set its name and return arity, build its parameter and body scopes,
    /// collect parameters, pre-create `goto`-label blocks, and walk the body.
    ///
    /// Both frontend entry points funnel through here. A free function passes
    /// `implicit_this = None`. A C++ instance method passes `Some(class_name)`: an implicit
    /// `this` parameter (`ByRef`) is installed at index 0 (so writes the body makes to
    /// `this.<member>` propagate back to the caller's receiver, exactly like an out-param),
    /// the declared parameters number from 1, and `current_method_class` is set so the body's
    /// unqualified member names resolve to `this.<member>`. `func_name` is the IR name and the
    /// resolution key — a free function's bare name, or a method's qualified `Class::method`.
    ///
    /// `member_inits` carries a C++ constructor's member-initializer list (`Box(int x) : v(x)`)
    /// as neutral `(member-name, init-expression-node)` pairs gathered by the C++ discovery
    /// hook; each is emitted as `this.<member> = <init-expr>` *before* the body (matching C++
    /// initialization order), reusing the same `this`-by-ref write that a body assignment
    /// `v = x` produces. It is always empty for a free function and for C, so nothing is
    /// emitted there.
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
        implicit_this: Option<&str>,
        member_inits: &[(String, Node<'_>)],
    ) -> anyhow::Result<(), Error> {
        self.allocator.reset();
        // Per-function state: a fresh local→class-type map (filled as we walk declarations),
        // a fresh reference-alias map (`T& r = x`), and the enclosing class for member
        // resolution (set only for a method body).
        self.local_types.clear();
        self.reference_aliases.clear();
        self.current_method_class = implicit_this.map(str::to_string);

        let fidx = *self
            .functions
            .entry(func_name.to_string())
            .or_insert_with(|| program.new_function());

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

        // A method gets an implicit `this` at parameter 0, passed `ByRef` so the existing
        // out-param machinery carries member writes back to the caller. The declared params
        // follow, numbered from 1.
        let start_idx = if implicit_this.is_some() {
            fdat.params.push(ParameterType::ByRef);
            self.param_names
                .entry(func_name.to_string())
                .or_default()
                .push("this");
            self.scope_tree.add_variable(
                param_sidx,
                "this".to_string(),
                VarKind::Parameter,
                Some(0),
                Some(ParameterType::ByRef),
            );
            1
        } else {
            0
        };

        let body_name = format!("{}.body", func_name);
        self.collect_params(
            source,
            &param_list,
            fdat,
            func_name,
            &para_scope_view,
            start_idx,
        )?;

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

        // A C++ constructor's member-initializer list runs before the body. Each
        // `member(expr)` becomes `this.<member> = <expr>`: the target is `@p0.<member>`
        // (built directly so a parameter that shadows the member — `Box(int v) : v(v)` —
        // does not redirect the *left* side away from `this`), and the init expression is
        // flattened with the shared expression lowering (so a param reference resolves to
        // its `@pN`). Empty for every free function and for C, so nothing is emitted there.
        for (member, init_expr) in member_inits {
            let target = Exp::AccessPath(AccessPath {
                variable_ref: VariableRef::new_parameter(0u32.into()),
                path: std::iter::once(FieldAccess::Symbol(member.as_str().into())).collect(),
            });
            let val = self.flatten_expr(program, *init_expr, source, &block_scope_view)?;
            self.add_assign_to_program(program, &block_scope_view, &target, &val, None);
        }

        self.walk_compound_statement(source, program, &block_scope_view, &cp)?;
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

/// The leaf identifier a declarator ultimately names, descending through pointer
/// (`Box* p`), array, parenthesized, and C++ reference (`Box& r`) wrappers. Used to key a
/// class-typed local in [`Context::local_types`] by its plain name (`p`/`r`), so a later
/// `p->m(…)` / `r.m(…)` call can look up its class — `to_str` of the wrapping declarator
/// would otherwise yield `*p`, which never matches the receiver identifier. Returns `None`
/// for shapes that name no single identifier (e.g. a function declarator).
/// Whether `node` sits inside a class/struct body (a `field_declaration_list`). The
/// top-level `function_definition` query in [`Context::collect_functions`] matches at any
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
/// [`Context::collect_functions`] from a global free function — but it must be lowered under
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

fn extract_field_expression<'a>(
    chain: Node<'a>,
    source: &'a str,
    out_vec: &mut Vec<&'a str>,
) -> anyhow::Result<&'a str, Error> {
    // A `this->member` access (C++) bases the chain on a `this` node rather than an
    // identifier. Its source text is literally "this", which `build_access_path` resolves
    // to parameter 0 (the implicit `this` installed by `lower_function`) — so `this->v`
    // yields the same `@p0.v` access path as the unqualified member `v`. The `this` node
    // never occurs under the C grammar, so this is inert for C.
    if chain.kind() == "identifier" || chain.kind() == "this" {
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
