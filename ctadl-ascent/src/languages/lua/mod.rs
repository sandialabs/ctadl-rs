//! Lua language frontend (tree-sitter).
//!
//! This module parses Lua source with the tree-sitter Lua grammar and lowers it
//! into CTADL IR ([`ProgramInfo`]). It is the entry point that
//! [`crate::cli::import`] dispatches to for `ctadl import -l lua` (and for a bare
//! `.lua` file via extension autodetection).
//!
//! # What it does
//!
//! The lowering mirrors the C tree-sitter frontend ([`crate::languages::tree_sitter`]):
//! walk the parse tree, build a [`Program`](ctadl_ir::mir::Program) of functions and
//! basic blocks, and lower Lua assignments, calls, table constructors, field/index
//! accesses, and control flow into IR statements with access paths.
//!
//! ## Functions and control flow
//!
//! Every Lua function definition (named, method, local, anonymous) and the top-level
//! chunk becomes a [`FunctionData`]. Structured control flow (`if`/`elseif`/`else`,
//! `while`, `repeat`, numeric/generic `for`, `do`) is lowered into basic blocks by
//! threading a "current block" cursor through the statement stream: straight-line code
//! accumulates into one block, and control constructs create fresh blocks and wire up
//! `Goto`/`Return` terminators. `goto`/labels are resolved against a per-function map of
//! pre-allocated label blocks, so a run of straight-line code between two labels (or a
//! label and a `goto`) becomes its own block with the appropriate edges.
//!
//! ## Data flow
//!
//! Assignments lower to [`StatementKind::Assign`]; field and index writes/reads lower to
//! [`StatementKind::Store`]/[`StatementKind::Load`] via
//! [`load_access_path`]/[`store_access_path`], so Lua tables are field-sensitive. Function
//! calls lower to [`StatementKind::CallAssign`]. Following the C frontend, most calls are staged
//! as a [`CallStyle::DirectCall`] whose [`CallEdges::Explicit`] list holds the *syntactic* callee
//! name (the bare final component of a dotted/method name, so `o:m(...)` and `T.m(...)` both
//! dispatch on `m`); call resolution is not done here -- the analysis joins the call to a
//! definition or model by name. A call whose callee is a bare local/parameter that is *not* a
//! defined function (a first-class function value, e.g. a closure) is instead a
//! [`CallStyle::FuncPtrCall`], resolved by data flow. A handful of standard-library calls
//! (`table.insert`, `ipairs`/`pairs`, `select`) are recognized syntactically and lowered directly
//! to data flow rather than a call, since they carry taint but have no definition or model.
//!
//! ## Globals
//!
//! `_ENV` is assumed never to be swapped out, so a free identifier (a name not bound by a
//! `local` or a parameter) is modeled uniformly as a field of the global heap:
//! `$globals.name` ([`Variable::GlobalHeap`] + a symbolic field). Reads become loads and
//! writes become stores on the global heap.
//!
//! ## Errors / exceptions
//!
//! Following the dex frontend, a raised error is modeled as an extra return value. Every
//! [`StatementKind::CallAssign`] carries a trailing exception return slot, and every function's
//! return arity is `max_normal_returns + 1` with the exception slot last; `Return` terminators
//! pad the normal values and append an (empty) exception value.
//!
//! ## Coroutines
//!
//! Coroutines are intentionally not modeled; `coroutine.*` calls lower as ordinary calls and
//! are otherwise ignored (no error is raised).

use std::collections::HashMap;
use std::path::Path;

use ctadl_ir::ThinVec;
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::*;
use source_info::{ArtifactKey, ArtifactMetadata, SourceInfoBuilder, SpanLen};
use tree_sitter::{Node, Parser};

use crate::error::Error;

/// Parse a Lua source file and translate it into CTADL IR.
pub fn import_lua(path: &Path) -> Result<ProgramInfo, Error> {
    let source = std::fs::read_to_string(path)?;

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .map_err(Error::TreeSitterLanguage)?;

    let tree = parser.parse(&source, None).ok_or_else(|| {
        Error::TreeSitterParse(format!("tree-sitter failed to parse {}", path.display()))
    })?;

    let root = tree.root_node();
    if root.has_error() {
        // A syntax error in a source frontend is worth surfacing rather than
        // silently importing a partial tree.
        return Err(Error::TreeSitterParse(format!(
            "syntax error while parsing {}",
            path.display()
        )));
    }

    let mut lowerer = Lowerer::new(&source, path);
    lowerer.run(root)?;

    let program = lowerer.program;
    let source_info = lowerer.sib.finish();
    Ok(ProgramInfo {
        program,
        source_info,
        ..Default::default()
    })
}

/// A base variable plus a mixed (offset + symbolic-field) path, before lowering to loads/stores.
/// Access paths in the IR are offset-only and load/store fields are single symbols, so Lua
/// field/index accesses are threaded here and lowered via [`load_access_path`] /
/// [`store_access_path`].
#[derive(Debug, Clone)]
struct RawPath {
    base: VariableRef,
    fields: ThinVec<PathSegment>,
}

impl RawPath {
    fn is_pathless(&self) -> bool {
        self.fields.is_empty()
    }
}

/// A name bound in a lexical scope: a parameter (by index) or an ordinary local.
#[derive(Debug, Clone, Copy)]
enum Binding {
    Param(ParameterIdx),
    Local,
}

/// A function discovered in the parse tree, to be lowered.
#[derive(Clone)]
struct FuncEntry<'a> {
    fidx: FunctionIdx,
    /// Body node: a `block` for a function definition, or the `chunk` root for the top-level.
    body: Node<'a>,
    /// Parameter list node (`parameters`), if any.
    params: Option<Node<'a>>,
    /// Whether this is a `:` method (an implicit `self` parameter is prepended).
    is_method: bool,
}

struct Lowerer<'a> {
    src: &'a str,
    program: Program,
    sib: SourceInfoBuilder,
    key: ArtifactKey,

    /// All functions to lower, in discovery order.
    funcs: Vec<FuncEntry<'a>>,
    /// Set of names already used for a function definition (for uniqueness).
    used_names: HashMap<String, FunctionIdx>,
    anon_counter: usize,
    /// Maps a function-definition node (by id) to the function it was collected as, so a closure
    /// value expression can recover the [`FunctionIdx`] of its anonymous function.
    func_by_node: HashMap<usize, FunctionIdx>,
    /// For each closure (anonymous function that captures upvalues), the names it captures. The
    /// enclosing function stores each captured value into a field of the closure object, and the
    /// closure body reads it back from its synthetic self-parameter (`build_var`).
    closure_upvalues: HashMap<FunctionIdx, Vec<String>>,

    // ---- per-function state ----
    fidx: FunctionIdx,
    /// Upvalue names captured by the function currently being lowered; each resolves to a field of
    /// the closure's self-parameter rather than to a local or global.
    cur_upvalues: HashMap<String, ()>,
    /// Lexical scope stack; innermost last.
    scopes: Vec<HashMap<String, Binding>>,
    /// Label name -> pre-allocated block for that label.
    labels: HashMap<String, BasicBlockIdx>,
    /// Stack of `break` targets (the continuation block of each enclosing loop).
    loop_breaks: Vec<BasicBlockIdx>,
    /// Number of normal (non-exception) return values this function declares.
    normal_arity: usize,
    temp_counter: usize,
    /// Source info stamped on statements emitted for the current source statement.
    cur_span: SourceInfo,
}

impl<'a> Lowerer<'a> {
    fn new(src: &'a str, path: &Path) -> Self {
        let key = ArtifactKey {
            path: path.to_string_lossy().into_owned(),
            sub_artifact_id: 0,
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Utf8,
        };
        Self {
            src,
            program: Program::default(),
            sib: SourceInfoBuilder::new(ArtifactMetadata::new()),
            key,
            funcs: Vec::new(),
            used_names: HashMap::new(),
            anon_counter: 0,
            func_by_node: HashMap::new(),
            closure_upvalues: HashMap::new(),
            fidx: FunctionIdx::new(0),
            cur_upvalues: HashMap::new(),
            scopes: Vec::new(),
            labels: HashMap::new(),
            loop_breaks: Vec::new(),
            normal_arity: 0,
            temp_counter: 0,
            cur_span: SourceInfo::default(),
        }
    }

    fn run(&mut self, root: Node<'a>) -> Result<(), Error> {
        self.collect_functions(root);
        for entry in self.funcs.clone() {
            self.lower_function(&entry)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Function discovery
    // ------------------------------------------------------------------

    fn collect_functions(&mut self, root: Node<'a>) {
        // The top-level chunk is a synthetic function holding all top-level statements.
        let fidx = self.new_named_function("%chunk".to_string());
        self.funcs.push(FuncEntry {
            fidx,
            body: root,
            params: None,
            is_method: false,
        });
        self.collect_nested(root);
    }

    /// Walk the whole tree and register every function definition. Each becomes its own
    /// [`FunctionData`]; nested functions are lowered independently. An anonymous function used as
    /// a value becomes a closure object (see [`Lowerer::eval_closure`]): its captured upvalues are
    /// stored in object fields and it is tagged with a function pointer so an indirect call can
    /// resolve it. (Resolving a closure returned *out* of a function is an engine-level limitation;
    /// see the `closure-flow` regression XFAIL.)
    fn collect_nested(&mut self, node: Node<'a>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "function_declaration" => {
                    let name_node = child.child_by_field_name("name");
                    let is_method =
                        name_node.map(|n| n.kind() == "method_index_expression").unwrap_or(false);
                    // Name the function by the bare final component of its definition name
                    // (`function A.b.c` -> `c`, `function A:m` -> `m`), because calls resolve by
                    // that syntactic tail (`o:m(...)` / `A.b.c(...)` both dispatch on `c`/`m`).
                    let base = name_node
                        .map(|n| self.def_name(n))
                        .unwrap_or_else(|| self.fresh_anon_name());
                    let fidx = self.new_named_function(base);
                    self.func_by_node.insert(child.id(), fidx);
                    self.funcs.push(FuncEntry {
                        fidx,
                        body: child.child_by_field_name("body").unwrap_or(child),
                        params: child.child_by_field_name("parameters"),
                        is_method,
                    });
                }
                "function_definition" => {
                    let name = self.fresh_anon_name();
                    let fidx = self.new_named_function(name);
                    self.func_by_node.insert(child.id(), fidx);
                    self.funcs.push(FuncEntry {
                        fidx,
                        body: child.child_by_field_name("body").unwrap_or(child),
                        params: child.child_by_field_name("parameters"),
                        is_method: false,
                    });
                }
                _ => {}
            }
            self.collect_nested(child);
        }
    }

    fn fresh_anon_name(&mut self) -> String {
        let n = self.anon_counter;
        self.anon_counter += 1;
        format!("%anon{n}")
    }

    /// Allocates a new function and gives it a unique, non-empty name.
    fn new_named_function(&mut self, base: String) -> FunctionIdx {
        let name = if !self.used_names.contains_key(&base) {
            base
        } else {
            let mut i = 1;
            loop {
                let candidate = format!("{base}%{i}");
                if !self.used_names.contains_key(&candidate) {
                    break candidate;
                }
                i += 1;
            }
        };
        let fidx = self.program.new_function();
        self.program[fidx].name = name.clone();
        self.used_names.insert(name, fidx);
        fidx
    }

    // ------------------------------------------------------------------
    // Per-function lowering
    // ------------------------------------------------------------------

    fn lower_function(&mut self, entry: &FuncEntry<'a>) -> Result<(), Error> {
        self.fidx = entry.fidx;
        self.scopes = vec![HashMap::new()];
        self.labels = HashMap::new();
        self.loop_breaks = Vec::new();
        self.temp_counter = 0;
        self.cur_span = SourceInfo::default();
        self.cur_upvalues = HashMap::new();

        // Parameters.
        let mut param_idx = 0usize;
        if entry.is_method {
            self.add_param("self", param_idx);
            param_idx += 1;
        }
        // A closure receives its own closure object as an implicit leading parameter; its captured
        // upvalues are read from fields of that self-parameter (see `build_var`). The caller passes
        // the closure value as argument 0 at the indirect call site (see `eval_call`).
        if let Some(upvalues) = self.closure_upvalues.get(&entry.fidx).cloned() {
            self.add_param("%self", param_idx);
            param_idx += 1;
            for u in upvalues {
                self.cur_upvalues.insert(u, ());
            }
        }
        if let Some(params) = entry.params {
            let mut cursor = params.walk();
            for p in params.named_children(&mut cursor) {
                match p.kind() {
                    "identifier" => {
                        let name = self.node_text(p).to_string();
                        self.add_param(&name, param_idx);
                        param_idx += 1;
                    }
                    "vararg_expression" => {
                        // Model the whole `...` pack as a single parameter, bound under the
                        // reserved name "..." so a `vararg_expression` value-use resolves to it.
                        self.add_param("...", param_idx);
                        param_idx += 1;
                    }
                    _ => {}
                }
            }
        }

        // Return arity: max normal-return count across the body, plus one exception slot.
        self.normal_arity = self.max_return_arity(entry.body);
        let arity = (self.normal_arity + 1).min(u8::MAX as usize) as u8;
        self.program[entry.fidx].set_return_type(ReturnType { arity });

        // Entry block (index 0) then a pre-allocated block for every label in the body.
        let entry_block = self.new_block();
        self.prealloc_labels(entry.body);

        let mut cur = Some(entry_block);
        self.lower_stmts(entry.body, &mut cur);

        // Any block still lacking a terminator falls off the end: give it an implicit return.
        self.finalize_terminators(entry.fidx);
        Ok(())
    }

    fn add_param(&mut self, name: &str, idx: usize) {
        let pidx = ParameterIdx::new(idx);
        self.program[self.fidx].params.parameters.push(ParameterType::ByVal);
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), Binding::Param(pidx));
    }

    /// Largest number of expressions returned by any `return` in `body`, not descending into
    /// nested function definitions.
    fn max_return_arity(&self, body: Node<'a>) -> usize {
        fn walk(src: &str, node: Node<'_>, max: &mut usize) {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    // Nested functions have their own returns.
                    "function_declaration" | "function_definition" => continue,
                    "return_statement" => {
                        let count = child
                            .child(0)
                            .into_iter()
                            .flat_map(|_| {
                                let mut c = child.walk();
                                child
                                    .named_children(&mut c)
                                    .filter(|n| n.kind() == "expression_list")
                                    .collect::<Vec<_>>()
                            })
                            .map(|el| {
                                let mut c = el.walk();
                                el.named_children(&mut c).count()
                            })
                            .max()
                            .unwrap_or(0);
                        let _ = src;
                        if count > *max {
                            *max = count;
                        }
                        walk(src, child, max);
                    }
                    _ => walk(src, child, max),
                }
            }
        }
        let mut max = 0;
        walk(self.src, body, &mut max);
        max
    }

    /// Pre-allocate an (empty) block for every label in the function body, so both forward and
    /// backward `goto`s can target them. Does not descend into nested functions.
    fn prealloc_labels(&mut self, body: Node<'a>) {
        let mut names = Vec::new();
        collect_label_names(self.src, body, &mut names);
        for name in names {
            let blk = self.new_block();
            self.labels.entry(name).or_insert(blk);
        }
    }

    fn finalize_terminators(&mut self, fidx: FunctionIdx) {
        let n = self.program[fidx].blocks.len();
        for i in 0..n {
            let blk = BasicBlockIdx::new(i);
            if self.program[fidx][blk].terminator.is_none() {
                let args = self.empty_return_args();
                self.program[fidx][blk].terminator =
                    Some(Terminator::new_kind(TerminatorKind::Return { args: args.into() }));
            }
        }
    }

    fn empty_return_args(&self) -> Vec<Exp> {
        // normal_arity empty normal values, plus one empty exception slot.
        (0..self.normal_arity + 1).map(|_| empty_exp()).collect()
    }

    // ------------------------------------------------------------------
    // Scopes
    // ------------------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare_local(&mut self, name: &str) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), Binding::Local);
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(*b);
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // Block / terminator helpers
    // ------------------------------------------------------------------

    fn new_block(&mut self) -> BasicBlockIdx {
        self.program[self.fidx]
            .blocks
            .blocks_mut()
            .push(BasicBlockData::new(None))
    }

    fn set_goto(&mut self, from: BasicBlockIdx, targets: &[BasicBlockIdx]) {
        let term = Terminator::new(
            TerminatorKind::Goto {
                targets: targets.to_vec().into(),
            },
            self.cur_span,
        );
        self.program[self.fidx][from].terminator = Some(term);
    }

    fn set_return(&mut self, blk: BasicBlockIdx, args: Vec<Exp>) {
        let term = Terminator::new(TerminatorKind::Return { args: args.into() }, self.cur_span);
        self.program[self.fidx][blk].terminator = Some(term);
    }

    fn push_stmt(&mut self, blk: BasicBlockIdx, kind: StatementKind) {
        self.program[self.fidx][blk].push_back(Statement::new(kind, self.cur_span));
    }

    fn push_stmts_stamped(&mut self, blk: BasicBlockIdx, stmts: Vec<Statement>) {
        for mut s in stmts {
            s.source_info = self.cur_span;
            self.program[self.fidx][blk].push_back(s);
        }
    }

    fn fresh_temp(&mut self) -> VariableRef {
        let n = self.temp_counter;
        self.temp_counter += 1;
        VariableRef::new_local(format!("%t{n}"))
    }

    // ------------------------------------------------------------------
    // Statement lowering
    // ------------------------------------------------------------------

    /// Lowers the statements of a `block`/`chunk` node into the CFG, updating `cur` (the current
    /// fall-through block; `None` once control is terminated until a label revives it).
    fn lower_stmts(&mut self, body: Node<'a>, cur: &mut Option<BasicBlockIdx>) {
        let mut cursor = body.walk();
        let children: Vec<Node<'a>> = body.named_children(&mut cursor).collect();
        for s in children {
            self.cur_span = self.span(s);
            match s.kind() {
                "comment" | "hash_bang_line" | "empty_statement" => {}
                "function_declaration" => {
                    // Register a `local function f` name so later value-uses resolve to a local
                    // (calls resolve by syntactic name regardless). The body was collected
                    // separately and is lowered on its own.
                    if let Some(name) = s.child_by_field_name("name")
                        && name.kind() == "identifier"
                    {
                        let nm = self.node_text(name).to_string();
                        self.declare_local(&nm);
                    }
                }
                "variable_declaration" => {
                    if let Some(b) = *cur {
                        self.lower_local_decl(s, b);
                    }
                }
                "assignment_statement" => {
                    if let Some(b) = *cur {
                        self.lower_assign(s, b);
                    }
                }
                "function_call" => {
                    if let Some(b) = *cur {
                        let _ = self.eval_call(s, b);
                    }
                }
                "if_statement" => self.lower_if(s, cur),
                "while_statement" => self.lower_while(s, cur),
                "repeat_statement" => self.lower_repeat(s, cur),
                "for_statement" => self.lower_for(s, cur),
                "do_statement" => {
                    self.push_scope();
                    if let Some(body) = s.child_by_field_name("body") {
                        self.lower_stmts(body, cur);
                    }
                    self.pop_scope();
                }
                "return_statement" => {
                    if let Some(b) = *cur {
                        self.lower_return(s, b);
                    }
                    *cur = None;
                }
                "break_statement" => {
                    if let Some(b) = *cur
                        && let Some(&target) = self.loop_breaks.last()
                    {
                        self.set_goto(b, &[target]);
                    }
                    *cur = None;
                }
                "goto_statement" => {
                    if let Some(b) = *cur
                        && let Some(ident) = s.named_child(0)
                        && let Some(&target) = self.labels.get(self.node_text(ident))
                    {
                        self.set_goto(b, &[target]);
                    }
                    *cur = None;
                }
                "label_statement" => {
                    if let Some(ident) = s.named_child(0) {
                        let name = self.node_text(ident).to_string();
                        if let Some(&lblk) = self.labels.get(&name) {
                            if let Some(b) = *cur {
                                self.set_goto(b, &[lblk]);
                            }
                            *cur = Some(lblk);
                        }
                    }
                }
                _ => {
                    // Unknown statement: try to evaluate it for side effects (e.g. a stray
                    // expression), otherwise ignore. Leniency avoids failing whole files on
                    // constructs we don't model.
                    if let Some(b) = *cur {
                        let _ = self.eval_expr(s, b);
                    }
                }
            }
        }
    }

    fn lower_local_decl(&mut self, node: Node<'a>, blk: BasicBlockIdx) {
        let Some(child) = node.named_child(0) else {
            return;
        };
        match child.kind() {
            "assignment_statement" => {
                let vlist = child_of_kind(child, "variable_list");
                let elist = child_of_kind(child, "expression_list");
                let targets: Vec<Node<'a>> = vlist
                    .map(|v| named_of(v).into_iter().filter(|n| n.kind() != "attribute").collect())
                    .unwrap_or_default();
                let rhs: Vec<Node<'a>> = elist.map(named_of).unwrap_or_default();
                // Evaluate RHS before declaring the new locals so `local x = x` reads the outer x.
                let values: Vec<Exp> = rhs.iter().map(|&e| self.eval_expr(e, blk)).collect();
                for t in &targets {
                    if t.kind() == "identifier" {
                        let nm = self.node_text(*t).to_string();
                        self.declare_local(&nm);
                    }
                }
                for (i, t) in targets.iter().enumerate() {
                    let target = self.eval_lvalue(*t, blk);
                    let val = values.get(i).cloned().unwrap_or_else(nil_exp);
                    self.assign_to(blk, target, val);
                }
            }
            "variable_list" => {
                for v in named_of(child) {
                    if v.kind() == "identifier" {
                        let nm = self.node_text(v).to_string();
                        self.declare_local(&nm);
                    }
                }
            }
            _ => {}
        }
    }

    fn lower_assign(&mut self, node: Node<'a>, blk: BasicBlockIdx) {
        let vlist = child_of_kind(node, "variable_list");
        let elist = child_of_kind(node, "expression_list");
        let targets: Vec<Node<'a>> = vlist
            .map(|v| named_of(v).into_iter().filter(|n| n.kind() != "attribute").collect())
            .unwrap_or_default();
        let rhs: Vec<Node<'a>> = elist.map(named_of).unwrap_or_default();
        let multi = targets.len() > 1;
        let mut values: Vec<Exp> = Vec::with_capacity(rhs.len());
        for &e in &rhs {
            let v = self.eval_expr(e, blk);
            // Snapshot a bare-variable RHS into a temp for parallel (multi-target) assignment,
            // so `a, b = b, a` behaves like a swap rather than reading the just-written value.
            if multi && matches!(v, Exp::Variable(_)) {
                let t = self.fresh_temp();
                self.push_stmt(blk, StatementKind::assign(t.clone(), [v]));
                values.push(Exp::Variable(t));
            } else {
                values.push(v);
            }
        }
        for (i, t) in targets.iter().enumerate() {
            let target = self.eval_lvalue(*t, blk);
            let val = values.get(i).cloned().unwrap_or_else(nil_exp);
            self.assign_to(blk, target, val);
        }
    }

    fn lower_return(&mut self, node: Node<'a>, blk: BasicBlockIdx) {
        let mut vals: Vec<Exp> = Vec::new();
        if let Some(elist) = child_of_kind(node, "expression_list") {
            // Note on multres: `return f()` propagates all of f's returns and `return (f())`
            // truncates to one. Without call resolution we model a call as producing a single
            // normal value, so both forms yield one value here; the parenthesization is still
            // honored structurally (a `parenthesized_expression` evaluates to one value).
            for e in named_of(elist) {
                vals.push(self.eval_expr(e, blk));
            }
        }
        // Pad the normal values to the declared arity, then append the (empty) exception slot.
        let mut args: Vec<Exp> = (0..self.normal_arity)
            .map(|i| vals.get(i).cloned().unwrap_or_else(empty_exp))
            .collect();
        args.push(empty_exp());
        self.set_return(blk, args);
    }

    // ------------------------------------------------------------------
    // Control flow
    // ------------------------------------------------------------------

    fn lower_if(&mut self, node: Node<'a>, cur: &mut Option<BasicBlockIdx>) {
        let Some(entry) = *cur else { return };
        if let Some(cond) = node.child_by_field_name("condition") {
            let _ = self.eval_expr(cond, entry);
        }
        let join = self.new_block();
        let then_blk = self.new_block();
        let mut then_cur = Some(then_blk);
        if let Some(cons) = node.child_by_field_name("consequence") {
            self.lower_stmts(cons, &mut then_cur);
        }
        if let Some(tc) = then_cur {
            self.set_goto(tc, &[join]);
        }
        let alts = children_by_field(node, "alternative");
        let false_target = self.build_else_chain(&alts, join);
        self.set_goto(entry, &[then_blk, false_target]);
        *cur = Some(join);
    }

    fn build_else_chain(&mut self, alts: &[Node<'a>], join: BasicBlockIdx) -> BasicBlockIdx {
        let Some(alt) = alts.first() else {
            return join;
        };
        match alt.kind() {
            "elseif_statement" => {
                let cond_blk = self.new_block();
                if let Some(cond) = alt.child_by_field_name("condition") {
                    let _ = self.eval_expr(cond, cond_blk);
                }
                let then_blk = self.new_block();
                let mut then_cur = Some(then_blk);
                if let Some(cons) = alt.child_by_field_name("consequence") {
                    self.lower_stmts(cons, &mut then_cur);
                }
                if let Some(tc) = then_cur {
                    self.set_goto(tc, &[join]);
                }
                let rest = self.build_else_chain(&alts[1..], join);
                self.set_goto(cond_blk, &[then_blk, rest]);
                cond_blk
            }
            "else_statement" => {
                let else_blk = self.new_block();
                let mut else_cur = Some(else_blk);
                if let Some(body) = alt.child_by_field_name("body") {
                    self.lower_stmts(body, &mut else_cur);
                }
                if let Some(ec) = else_cur {
                    self.set_goto(ec, &[join]);
                }
                else_blk
            }
            _ => join,
        }
    }

    fn lower_while(&mut self, node: Node<'a>, cur: &mut Option<BasicBlockIdx>) {
        let Some(entry) = *cur else { return };
        let cond_blk = self.new_block();
        self.set_goto(entry, &[cond_blk]);
        let body_blk = self.new_block();
        let join = self.new_block();
        if let Some(cond) = node.child_by_field_name("condition") {
            let _ = self.eval_expr(cond, cond_blk);
        }
        self.set_goto(cond_blk, &[body_blk, join]);
        self.loop_breaks.push(join);
        self.push_scope();
        let mut body_cur = Some(body_blk);
        if let Some(body) = node.child_by_field_name("body") {
            self.lower_stmts(body, &mut body_cur);
        }
        if let Some(bc) = body_cur {
            self.set_goto(bc, &[cond_blk]);
        }
        self.pop_scope();
        self.loop_breaks.pop();
        *cur = Some(join);
    }

    fn lower_repeat(&mut self, node: Node<'a>, cur: &mut Option<BasicBlockIdx>) {
        let Some(entry) = *cur else { return };
        let body_blk = self.new_block();
        self.set_goto(entry, &[body_blk]);
        let cond_blk = self.new_block();
        let join = self.new_block();
        self.loop_breaks.push(join);
        self.push_scope();
        let mut body_cur = Some(body_blk);
        if let Some(body) = node.child_by_field_name("body") {
            self.lower_stmts(body, &mut body_cur);
        }
        if let Some(bc) = body_cur {
            self.set_goto(bc, &[cond_blk]);
        }
        // `repeat ... until cond`: the condition can see the body's locals, so evaluate it in the
        // condition block; either loop back to the body or fall through to the join.
        if let Some(cond) = node.child_by_field_name("condition") {
            let _ = self.eval_expr(cond, cond_blk);
        }
        self.set_goto(cond_blk, &[body_blk, join]);
        self.pop_scope();
        self.loop_breaks.pop();
        *cur = Some(join);
    }

    fn lower_for(&mut self, node: Node<'a>, cur: &mut Option<BasicBlockIdx>) {
        let Some(entry) = *cur else { return };
        let clause = node.child_by_field_name("clause");
        self.push_scope();
        if let Some(clause) = clause {
            match clause.kind() {
                "for_numeric_clause" => {
                    if let Some(name_node) = clause.child_by_field_name("name") {
                        let name = self.node_text(name_node).to_string();
                        self.declare_local(&name);
                        let mut srcs = Vec::new();
                        for f in ["start", "end", "step"] {
                            if let Some(x) = clause.child_by_field_name(f) {
                                srcs.push(self.eval_expr(x, entry));
                            }
                        }
                        let target = self.build_var(&name);
                        self.assign_multi(entry, target, srcs);
                    }
                }
                "for_generic_clause" => {
                    let vlist = child_of_kind(clause, "variable_list");
                    let elist = child_of_kind(clause, "expression_list");
                    let mut vals = Vec::new();
                    if let Some(el) = elist {
                        for e in named_of(el) {
                            vals.push(self.eval_expr(e, entry));
                        }
                    }
                    if let Some(vl) = vlist {
                        for v in named_of(vl) {
                            if v.kind() == "identifier" {
                                let nm = self.node_text(v).to_string();
                                self.declare_local(&nm);
                                let target = self.build_var(&nm);
                                // Approximate the iterator: flow the iterated expression(s) into
                                // each loop variable.
                                let val = vals.first().cloned().unwrap_or_else(nil_exp);
                                self.assign_to(entry, target, val);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let cond_blk = self.new_block();
        self.set_goto(entry, &[cond_blk]);
        let body_blk = self.new_block();
        let join = self.new_block();
        self.set_goto(cond_blk, &[body_blk, join]);
        self.loop_breaks.push(join);
        let mut body_cur = Some(body_blk);
        if let Some(body) = node.child_by_field_name("body") {
            self.lower_stmts(body, &mut body_cur);
        }
        if let Some(bc) = body_cur {
            self.set_goto(bc, &[cond_blk]);
        }
        self.loop_breaks.pop();
        self.pop_scope();
        *cur = Some(join);
    }

    // ------------------------------------------------------------------
    // Expression lowering
    // ------------------------------------------------------------------

    /// Evaluates `node` to a value, emitting any needed statements (loads, sub-calls, temporaries)
    /// into `blk`.
    fn eval_expr(&mut self, node: Node<'a>, blk: BasicBlockIdx) -> Exp {
        match node.kind() {
            "identifier" | "dot_index_expression" | "bracket_index_expression" | "global" => {
                let rp = self.eval_lvalue(node, blk);
                let ap = self.emit_loads(blk, rp);
                Exp::from(ap)
            }
            "number" | "string" | "true" | "false" | "nil" => Exp::new_str(self.node_text(node)),
            "vararg_expression" => {
                // `...` resolves to the vararg parameter when the enclosing function declares one
                // (otherwise it degrades to a global-heap read, which is harmless).
                let rp = self.build_var("...");
                let ap = self.emit_loads(blk, rp);
                Exp::from(ap)
            }
            "parenthesized_expression" => {
                // Parenthesizing truncates a multi-value expression to one value; the inner
                // expression already evaluates to a single value in this model.
                match node.named_child(0) {
                    Some(inner) => self.eval_expr(inner, blk),
                    None => nil_exp(),
                }
            }
            "binary_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map(|n| self.eval_expr(n, blk))
                    .unwrap_or_else(nil_exp);
                let right = node
                    .child_by_field_name("right")
                    .map(|n| self.eval_expr(n, blk))
                    .unwrap_or_else(nil_exp);
                // Both operands flow into the result (covers `..` concatenation and arithmetic).
                let t = self.fresh_temp();
                self.push_stmt(blk, StatementKind::assign(t.clone(), [left, right]));
                Exp::Variable(t)
            }
            "unary_expression" => node
                .child_by_field_name("operand")
                .map(|n| self.eval_expr(n, blk))
                .unwrap_or_else(nil_exp),
            "function_call" => self.eval_call(node, blk),
            "table_constructor" => self.eval_table(node, blk),
            "function_definition" => self.eval_closure(node, blk),
            _ => Exp::new_str(self.node_text(node)),
        }
    }

    /// Resolves an assignable location to a [`RawPath`] WITHOUT emitting loads, so the field path
    /// is preserved for a store (or for load lowering by the caller).
    fn eval_lvalue(&mut self, node: Node<'a>, blk: BasicBlockIdx) -> RawPath {
        match node.kind() {
            "identifier" => self.build_var(self.node_text(node)),
            "global" => self.build_var(self.node_text(node)),
            "dot_index_expression" => {
                let table = node.child_by_field_name("table");
                let field = node.child_by_field_name("field");
                let mut rp = table
                    .map(|t| self.eval_lvalue(t, blk))
                    .unwrap_or_else(|| self.build_var("_"));
                if let Some(field) = field {
                    rp.fields.push(PathSegment::symbol(self.node_text(field)));
                }
                rp
            }
            "bracket_index_expression" => {
                let table = node.child_by_field_name("table");
                let key = node.child_by_field_name("field");
                let mut rp = table
                    .map(|t| self.eval_lvalue(t, blk))
                    .unwrap_or_else(|| self.build_var("_"));
                if let Some(key) = key {
                    rp.fields.push(self.key_segment(key, blk));
                }
                rp
            }
            "parenthesized_expression" => match node.named_child(0) {
                Some(inner) => self.eval_lvalue(inner, blk),
                None => self.build_var("_"),
            },
            "method_index_expression" => node
                .child_by_field_name("table")
                .map(|t| self.eval_lvalue(t, blk))
                .unwrap_or_else(|| self.build_var("_")),
            _ => {
                // Not a syntactic lvalue (e.g. a call result being indexed): evaluate it and use
                // the resulting variable as the base.
                match self.eval_expr(node, blk) {
                    Exp::Variable(v) => RawPath {
                        base: v,
                        fields: ThinVec::new(),
                    },
                    other => {
                        let t = self.fresh_temp();
                        self.push_stmt(blk, StatementKind::assign(t.clone(), [other]));
                        RawPath {
                            base: t,
                            fields: ThinVec::new(),
                        }
                    }
                }
            }
        }
    }

    /// Turns an index key into a symbolic field segment. String and number literals become a
    /// stable field name (so writes and reads of the same key match); a dynamic key evaluates for
    /// side effects and collapses to a generic element field.
    fn key_segment(&mut self, key: Node<'a>, blk: BasicBlockIdx) -> PathSegment {
        match key.kind() {
            "string" => PathSegment::symbol(self.string_content(key)),
            "number" => PathSegment::symbol(format!("[{}]", self.node_text(key))),
            _ => {
                let _ = self.eval_expr(key, blk);
                PathSegment::symbol("[_elem_]")
            }
        }
    }

    fn eval_table(&mut self, node: Node<'a>, blk: BasicBlockIdx) -> Exp {
        let table = self.fresh_temp();
        // Define the table variable so it exists even when empty.
        self.push_stmt(blk, StatementKind::assign(table.clone(), [Exp::new_str("{}")]));
        for field in named_of(node) {
            if field.kind() != "field" {
                continue;
            }
            let value = field
                .child_by_field_name("value")
                .map(|v| self.eval_expr(v, blk))
                .unwrap_or_else(nil_exp);
            let seg = if let Some(name) = field.child_by_field_name("name") {
                match name.kind() {
                    "identifier" | "global" => PathSegment::symbol(self.node_text(name)),
                    "string" => PathSegment::symbol(self.string_content(name)),
                    "number" => PathSegment::symbol(format!("[{}]", self.node_text(name))),
                    _ => {
                        let _ = self.eval_expr(name, blk);
                        PathSegment::symbol("[_elem_]")
                    }
                }
            } else {
                // Positional array entry.
                PathSegment::symbol("[_elem_]")
            };
            let target = RawPath {
                base: table.clone(),
                fields: ThinVec::from(vec![seg]),
            };
            self.assign_to(blk, target, value);
        }
        Exp::Variable(table)
    }

    /// Lowers an anonymous `function ... end` value into a first-class closure object. The object
    /// is tagged with the closure's function (an [`Exp::ObjectRef`] function pointer, so an
    /// indirect call can resolve it) and carries each captured upvalue in a like-named field. The
    /// closure body reads those fields back through its synthetic self-parameter.
    fn eval_closure(&mut self, node: Node<'a>, blk: BasicBlockIdx) -> Exp {
        let Some(&fidx) = self.func_by_node.get(&node.id()) else {
            return Exp::new_str("<function>");
        };
        let fn_name = self.program[fidx].name.clone();

        // Determine captured upvalues: names referenced free in the body that resolve to a local or
        // parameter of the *enclosing* function (globals and the closure's own parameters are not
        // captured).
        let params: HashMap<String, ()> = node
            .child_by_field_name("parameters")
            .map(|p| {
                named_of(p)
                    .into_iter()
                    .filter(|n| n.kind() == "identifier")
                    .map(|n| (self.node_text(n).to_string(), ()))
                    .collect()
            })
            .unwrap_or_default();
        let mut names = Vec::new();
        collect_identifiers(self.src, node.child_by_field_name("body").unwrap_or(node), &mut names);
        let mut upvalues: Vec<String> = Vec::new();
        let mut seen: HashMap<String, ()> = HashMap::new();
        for name in names {
            if params.contains_key(&name) || seen.contains_key(&name) {
                continue;
            }
            if matches!(self.lookup(&name), Some(Binding::Local | Binding::Param(_))) {
                seen.insert(name.clone(), ());
                upvalues.push(name);
            }
        }

        // Build the closure object: bind its function pointer, then store each captured value.
        let closure = self.fresh_temp();
        self.push_stmt(
            blk,
            StatementKind::assign(
                closure.clone(),
                [Exp::ObjectRef(CallObject::FunctionPtr(fn_name.into()))],
            ),
        );
        for u in &upvalues {
            let value = {
                let rp = self.build_var(u);
                Exp::from(self.emit_loads(blk, rp))
            };
            let target = RawPath {
                base: closure.clone(),
                fields: ThinVec::from(vec![PathSegment::symbol(u)]),
            };
            self.assign_to(blk, target, value);
        }
        self.closure_upvalues.insert(fidx, upvalues);
        Exp::Variable(closure)
    }

    /// If `name_node` names a first-class function value (a local/parameter that is not a defined
    /// function), returns the callee's access path for an indirect call; otherwise `None` (the call
    /// resolves by name). A `local function f`/global function is a defined name, so it stays a
    /// direct call.
    fn indirect_call_target(&mut self, name_node: Node<'a>, blk: BasicBlockIdx) -> Option<AccessPath> {
        if !matches!(name_node.kind(), "identifier" | "global") {
            return None;
        }
        let name = self.node_text(name_node);
        if self.used_names.contains_key(name) {
            return None;
        }
        match self.lookup(name) {
            Some(Binding::Local | Binding::Param(_)) => {
                let rp = self.eval_lvalue(name_node, blk);
                Some(self.emit_loads(blk, rp))
            }
            _ => None,
        }
    }

    fn eval_call(&mut self, node: Node<'a>, blk: BasicBlockIdx) -> Exp {
        let name_node = node.child_by_field_name("name");

        // Builtin library calls that carry taint but have no user-visible definition or model.
        // We recognize them syntactically and lower them to plain data flow rather than a call.
        if let Some(result) = self.eval_builtin_call(name_node, node, blk) {
            return result;
        }

        let mut args: ThinVec<Exp> = ThinVec::new();

        let callee = if let Some(name_node) = name_node {
            if name_node.kind() == "method_index_expression" {
                // `o:m(...)` desugars to `m(o, ...)`.
                if let Some(table) = name_node.child_by_field_name("table") {
                    let recv = self.eval_expr(table, blk);
                    args.push(recv);
                }
                name_node
                    .child_by_field_name("method")
                    .map(|m| self.node_text(m).to_string())
                    .unwrap_or_default()
            } else {
                self.call_name(name_node)
            }
        } else {
            String::new()
        };

        // An indirect (value) call: the callee is a bare name bound to a first-class function value
        // (a closure or a function passed as an argument) rather than a defined function. Resolve
        // it by data flow through a `FuncPtrCall`, passing the closure value as the leading `%self`
        // argument so the callee can read its captured upvalues.
        let indirect_callee = name_node.and_then(|n| self.indirect_call_target(n, blk));

        if let Some(arg_node) = node.child_by_field_name("arguments") {
            for a in named_of(arg_node) {
                args.push(self.eval_expr(a, blk));
            }
        }

        // Every call gets a normal result slot and a trailing exception slot (see module docs).
        let result = self.fresh_temp();
        let err = self.fresh_temp();
        let rets: ThinVec<VariableRef> = ThinVec::from(vec![result.clone(), err]);
        let style = if let Some(callee_ap) = indirect_callee {
            args.insert(0, Exp::from(callee_ap.clone()));
            CallStyle::FuncPtrCall {
                callee: callee_ap,
                signature: Some("indirect-call".to_string()),
            }
        } else {
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(ThinVec::from(vec![callee])),
            }
        };
        // Stamp this call with its own syntactic span rather than the enclosing statement's, so
        // that nested calls on one source line (`sink(f(x))`) get distinct source regions. The
        // SARIF formatter deduplicates code-flow steps by region; sharing the statement span would
        // collapse the outer call's step into the inner one and drop the sink from the trace.
        let saved = self.cur_span;
        self.cur_span = self.span(node);
        self.push_stmt(blk, StatementKind::CallAssign { style, rets, args });
        self.cur_span = saved;
        Exp::Variable(result)
    }

    /// Recognizes standard-library calls that only move taint around (no callee definition and no
    /// query model exists for them) and lowers them directly to data flow. Returns `Some(value)`
    /// when `node` was handled as a builtin, `None` to fall through to the generic call path.
    fn eval_builtin_call(
        &mut self,
        name_node: Option<Node<'a>>,
        node: Node<'a>,
        blk: BasicBlockIdx,
    ) -> Option<Exp> {
        let name_node = name_node?;
        let arg_nodes = node
            .child_by_field_name("arguments")
            .map(named_of)
            .unwrap_or_default();

        // `table.insert(t, v)` / `table.insert(t, pos, v)`: v flows into an element of `t`.
        if name_node.kind() == "dot_index_expression" {
            let base = name_node.child_by_field_name("table").map(|t| self.node_text(t));
            let field = name_node.child_by_field_name("field").map(|f| self.node_text(f));
            if base == Some("table") && field == Some("insert") && arg_nodes.len() >= 2 {
                let mut target = self.eval_lvalue(arg_nodes[0], blk);
                let value = self.eval_expr(*arg_nodes.last().unwrap(), blk);
                target.fields.push(PathSegment::symbol("[_elem_]"));
                self.assign_to(blk, target, value);
                return Some(nil_exp());
            }
            return None;
        }

        let callee = match name_node.kind() {
            "identifier" | "global" => self.node_text(name_node),
            _ => return None,
        };
        match callee {
            // `ipairs(t)` / `pairs(t)`: the iterator surfaces the elements of `t`. Model it as a
            // read of `t`'s generic element, so a generic `for` flows table elements into the loop
            // variables (see `lower_for`).
            "ipairs" | "pairs" if !arg_nodes.is_empty() => {
                let mut rp = self.eval_lvalue(arg_nodes[0], blk);
                rp.fields.push(PathSegment::symbol("[_elem_]"));
                Some(Exp::from(self.emit_loads(blk, rp)))
            }
            // `select(k, ...)`: returns the selected varargs; over-approximate by flowing every
            // vararg operand into the result.
            "select" if arg_nodes.len() >= 2 => {
                let srcs: Vec<Exp> =
                    arg_nodes[1..].iter().map(|&a| self.eval_expr(a, blk)).collect();
                let t = self.fresh_temp();
                self.push_stmt(blk, StatementKind::assign(t.clone(), srcs));
                Some(Exp::Variable(t))
            }
            _ => None,
        }
    }

    /// The name given to a function *definition*: the bare final component of its dotted/method
    /// name, matching how [`call_name`] and method-call desugaring name the call target.
    fn def_name(&self, node: Node<'a>) -> String {
        match node.kind() {
            "dot_index_expression" => node
                .child_by_field_name("field")
                .map(|f| self.node_text(f).to_string())
                .unwrap_or_default(),
            "method_index_expression" => node
                .child_by_field_name("method")
                .map(|m| self.node_text(m).to_string())
                .unwrap_or_default(),
            _ => self.node_text(node).to_string(),
        }
    }

    /// The syntactic name of a call target (used as the direct-call edge string).
    fn call_name(&self, node: Node<'a>) -> String {
        match node.kind() {
            "identifier" | "global" => self.node_text(node).to_string(),
            "dot_index_expression" => node
                .child_by_field_name("field")
                .map(|f| self.node_text(f).to_string())
                .unwrap_or_default(),
            "parenthesized_expression" => node
                .named_child(0)
                .map(|n| self.call_name(n))
                .unwrap_or_default(),
            _ => self.node_text(node).to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Access-path helpers
    // ------------------------------------------------------------------

    /// Builds a [`RawPath`] for a bare name: a parameter or local becomes a bare variable; a free
    /// name is a field of the global heap (`$globals.name`), modeling `_ENV`.
    fn build_var(&self, name: &str) -> RawPath {
        // A captured upvalue is stored in a field of the closure's self-parameter.
        if self.cur_upvalues.contains_key(name)
            && let Some(Binding::Param(idx)) = self.lookup("%self")
        {
            return RawPath {
                base: VariableRef::new_parameter(idx),
                fields: ThinVec::from(vec![PathSegment::symbol(name)]),
            };
        }
        match self.lookup(name) {
            Some(Binding::Param(idx)) => RawPath {
                base: VariableRef::new_parameter(idx),
                fields: ThinVec::new(),
            },
            Some(Binding::Local) => RawPath {
                base: VariableRef::new_local(name.to_string()),
                fields: ThinVec::new(),
            },
            None => RawPath {
                base: VariableRef::new_global(),
                fields: ThinVec::from(vec![PathSegment::symbol(name)]),
            },
        }
    }

    /// Lowers the symbolic-field reads of `rp` into a sequence of loads appended to `blk`, and
    /// returns the residual (offset-only / pathless) address as an access path.
    fn emit_loads(&mut self, blk: BasicBlockIdx, rp: RawPath) -> AccessPath {
        let mut stmts = Vec::new();
        let mut counter = self.temp_counter;
        let ap = load_access_path(rp.base, rp.fields, &mut stmts, || {
            let v = VariableRef::new_local(format!("%t{counter}"));
            counter += 1;
            v
        });
        self.temp_counter = counter;
        self.push_stmts_stamped(blk, stmts);
        ap
    }

    /// Assigns `value` into `target`: a bare variable becomes an [`StatementKind::Assign`]; a field
    /// path becomes a store (with loads for intermediate dereferences).
    fn assign_to(&mut self, blk: BasicBlockIdx, target: RawPath, value: Exp) {
        if target.is_pathless() {
            self.push_stmt(blk, StatementKind::assign(target.base, [value]));
        } else {
            let mut stmts = Vec::new();
            let mut counter = self.temp_counter;
            store_access_path(target.base, target.fields, value, &mut stmts, || {
                let v = VariableRef::new_local(format!("%t{counter}"));
                counter += 1;
                v
            });
            self.temp_counter = counter;
            self.push_stmts_stamped(blk, stmts);
        }
    }

    /// Assigns multiple source values into `target` (a bare variable), flowing all of them in.
    fn assign_multi(&mut self, blk: BasicBlockIdx, target: RawPath, srcs: Vec<Exp>) {
        if target.is_pathless() {
            let sources: Vec<Exp> = if srcs.is_empty() { vec![nil_exp()] } else { srcs };
            self.push_stmt(blk, StatementKind::assign(target.base, sources));
        } else {
            let val = srcs.into_iter().next().unwrap_or_else(nil_exp);
            self.assign_to(blk, target, val);
        }
    }

    // ------------------------------------------------------------------
    // Text / span helpers
    // ------------------------------------------------------------------

    fn node_text(&self, node: Node<'_>) -> &'a str {
        node.utf8_text(self.src.as_bytes()).unwrap_or("").trim()
    }

    /// The content of a string literal with its surrounding quotes/long-brackets removed, so a key
    /// like `t["k"]` and `t.k` name the same field.
    fn string_content(&self, node: Node<'a>) -> String {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "string_content" {
                return self.node_text(child).to_string();
            }
        }
        // No content child (e.g. empty string): strip one leading/trailing quote char.
        let text = self.node_text(node);
        text.trim_matches(|c| c == '"' || c == '\'').to_string()
    }

    fn span(&mut self, node: Node<'_>) -> SourceInfo {
        let start = node.start_byte() as u32;
        let len = (node.end_byte() - node.start_byte()) as u32;
        SourceInfo::new(self.sib.span_for(self.key.clone(), start, SpanLen::ByteLen(len)))
    }
}

fn empty_exp() -> Exp {
    Exp::new_bytes(Vec::new())
}

fn nil_exp() -> Exp {
    Exp::new_str("nil")
}

/// The named children of `node`, collected.
fn named_of<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The first named child of `node` with the given kind.
fn child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

/// All children of `node` bound to the given field name.
fn children_by_field<'a>(node: Node<'a>, field: &str) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor).collect()
}

/// Collects the value-position identifier names referenced in `node`, not descending into nested
/// function bodies. The `field`/`method` component of an index expression is a member name, not a
/// variable reference, so only the `table` side is followed. Used to find a closure's free names.
fn collect_identifiers(src: &str, node: Node<'_>, out: &mut Vec<String>) {
    match node.kind() {
        "function_definition" | "function_declaration" => return,
        "identifier" => {
            if let Ok(t) = node.utf8_text(src.as_bytes()) {
                out.push(t.trim().to_string());
            }
            return;
        }
        "dot_index_expression" | "method_index_expression" => {
            if let Some(table) = node.child_by_field_name("table") {
                collect_identifiers(src, table, out);
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(src, child, out);
    }
}

/// Collects all label names in a function body, not descending into nested functions.
fn collect_label_names(src: &str, node: Node<'_>, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "function_definition" => continue,
            "label_statement" => {
                if let Some(ident) = child.named_child(0)
                    && let Ok(name) = ident.utf8_text(src.as_bytes())
                {
                    out.push(name.trim().to_string());
                }
                collect_label_names(src, child, out);
            }
            _ => collect_label_names(src, child, out),
        }
    }
}
