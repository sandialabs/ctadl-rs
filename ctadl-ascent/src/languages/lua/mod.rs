//! Lua language frontend (tree-sitter).
//!
//! This module parses Lua source with the tree-sitter Lua grammar and lowers it
//! into CTADL IR ([`ProgramInfo`]). It is the entry point that
//! [`crate::cli::import`] dispatches to for `ctadl import -l lua` (and for a bare
//! `.lua` file via extension autodetection).
//!
//! # What it does
//!
//! The lowering mirrors the C tree-sitter frontend ([`crate::languages::tree_sitter_c`]):
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
//! ## Modules and names
//!
//! An import covers a whole *directory* of Lua sources (a single file is just the one-unit case),
//! and that directory is the `require` root. Each file's module name is its path relative to the
//! root with separators replaced by `.` and `.lua` dropped, folding a trailing `init` away -- i.e.
//! exactly what `package.path`'s `?.lua;?/init.lua` resolves. Importing the tree at once is what
//! makes `require` resolvable, and that in turn is what lets a call site be given the *same*
//! fully-qualified name its definition got.
//!
//! Definitions are therefore named by what their root denotes ([`Lowerer::qualified_def_name`]):
//! the module table's fields are the module's exports (`function M.f` in `a/b.lua` with `return M`
//! is `a.b.f`), another file-local table is namespaced under the module (`a.b.T.m`), a `local
//! function` is `a.b.f`, and a *global* root names itself (`function kong.request.get_header` is
//! `kong.request.get_header`) so a shim in another file can define the very same string.
//!
//! Call sites are written with plain Lua names, so the frontend resolves them back to that
//! qualified name ([`Lowerer::qname_of`]) through an alias environment seeded by `require`, by
//! field reads of known module tables, and by `local` aliasing -- which is how OpenResty-style
//! hoisting (`local get_headers = ngx.req.get_headers`) resolves to the API it aliases rather
//! than colliding with every other `get_headers` in the program.
//!
//! ## Data flow
//!
//! Assignments lower to [`StatementKind::Assign`]; field and index writes/reads lower to
//! [`StatementKind::Store`]/[`StatementKind::Load`] via
//! [`load_access_path`]/[`store_access_path`], so Lua tables are field-sensitive. Function
//! calls lower to [`StatementKind::CallAssign`]. Most calls are staged as a
//! [`CallStyle::DirectCall`] whose [`CallEdges::Explicit`] list holds the resolved qualified
//! callee name; the analysis joins the call to a definition or model by that name. A call whose
//! callee is a bare local/parameter that resolves to no definition (a first-class function value,
//! e.g. a closure) is instead a [`CallStyle::FuncPtrCall`], resolved by data flow, and a method
//! call `o:m(...)` is a [`CallStyle::LuaCall`] dispatched on the bare method name through the
//! recovered metatable hierarchy (a Lua receiver has no static type). A handful of standard-library
//! calls (`table.insert`, `ipairs`/`pairs`, `select`) are recognized syntactically and lowered
//! directly to data flow rather than a call, since they carry taint but have no definition or model.
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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ctadl_ir::ThinVec;
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::call::VirtualMethodTable;
use ctadl_ir::mir::*;
use smallvec::SmallVec;
use source_info::{ArtifactKey, ArtifactMetadata, SourceInfoBuilder, SpanLen};
use tree_sitter::{Node, Parser, Tree};

use crate::error::{Error, ErrorContext};

/// One Lua source file in an import: where it came from, the `require` name it answers to, and
/// its text.
struct SourceUnit {
    path: PathBuf,
    /// The file's `require` module name, e.g. `kong.pdk.request` (see [`module_name`]).
    module: String,
    source: String,
}

/// Parse a Lua artifact and translate it into CTADL IR.
///
/// `path` is either a directory -- imported whole, and taken as the `require` root -- or a single
/// `.lua` file, which is just the one-unit case (its parent directory is the root). Importing a
/// tree at once is what lets `require` be resolved to a module in the same import, which is what
/// makes call sites resolvable to fully-qualified definition names.
pub fn import_lua(path: &Path) -> Result<ProgramInfo, Error> {
    lower_lua_units(collect_units(path)?)
}

/// Gathers the source units under `path`, in a stable order.
fn collect_units(path: &Path) -> Result<Vec<SourceUnit>, Error> {
    let (root, files) = if path.is_dir() {
        let mut files = Vec::new();
        collect_lua_files(path, &mut files)?;
        files.sort();
        (path.to_path_buf(), files)
    } else {
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (root, vec![path.to_path_buf()])
    };

    if files.is_empty() {
        return Err(Error::TreeSitterParse(format!(
            "no .lua files found under {}",
            path.display()
        )));
    }

    files
        .into_iter()
        .map(|file| {
            let source = source_info::read_source(&file)
                .err_context(|| format!("reading Lua source: {}", file.display()))?;
            Ok(SourceUnit {
                module: module_name(&root, &file),
                path: file,
                source,
            })
        })
        .collect()
}

/// Recursively collects `.lua` files under `dir`.
fn collect_lua_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries =
        std::fs::read_dir(dir).err_context(|| format!("listing directory: {}", dir.display()))?;
    for entry in entries {
        let path = entry
            .err_context(|| format!("listing directory: {}", dir.display()))?
            .path();
        let meta = std::fs::symlink_metadata(&path)
            .err_context(|| format!("reading file metadata: {}", path.display()))?;
        if meta.is_dir() {
            collect_lua_files(&path, out)?;
        } else if meta.is_file() && path.extension().and_then(|e| e.to_str()) == Some("lua") {
            out.push(path);
        }
    }
    Ok(())
}

/// The `require` name for `file` relative to the import root: the relative path with separators
/// replaced by `.` and `.lua` dropped, folding a trailing `init` away. This is what
/// `package.path`'s default `?.lua;?/init.lua` resolves, so `require "kong.pdk.request"` names
/// `<root>/kong/pdk/request.lua` and `require "kong"` names `<root>/kong/init.lua`.
fn module_name(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut()
        && let Some(stem) = last.strip_suffix(".lua")
    {
        *last = stem.to_string();
    }
    if parts.len() > 1 && parts.last().map(String::as_str) == Some("init") {
        parts.pop();
    }
    parts.join(".")
}

/// Parses and lowers every unit of an import into one [`ProgramInfo`].
fn lower_lua_units(units: Vec<SourceUnit>) -> Result<ProgramInfo, Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_lua::LANGUAGE.into())
        .map_err(Error::TreeSitterLanguage)?;

    // A syntax error in a single-file import is worth surfacing rather than silently importing a
    // partial tree. In a directory import it must not sink the other 600 files, so the bad unit is
    // dropped and counted instead.
    let single = units.len() == 1;
    let mut parsed: Vec<SourceUnit> = Vec::with_capacity(units.len());
    let mut trees: Vec<Tree> = Vec::with_capacity(units.len());
    let mut skipped = 0usize;
    for unit in units {
        match parser.parse(&unit.source, None) {
            Some(tree) if !tree.root_node().has_error() => {
                parsed.push(unit);
                trees.push(tree);
            }
            _ if single => {
                return Err(Error::TreeSitterParse(format!(
                    "syntax error while parsing {}",
                    unit.path.display()
                )));
            }
            _ => {
                log::warn!("lua: skipping {}: syntax error", unit.path.display());
                skipped += 1;
            }
        }
    }
    if parsed.is_empty() {
        return Err(Error::TreeSitterParse(
            "every Lua file in the import failed to parse".to_string(),
        ));
    }
    if skipped > 0 {
        log::warn!("lua: {skipped} file(s) skipped due to syntax errors");
    }

    let mut lowerer = Lowerer::new(&parsed, &trees);
    lowerer.run()?;

    let vmt = std::mem::take(&mut lowerer.vmt);
    let program = std::mem::take(&mut lowerer.program);
    let source_info = lowerer.sib.finish();
    Ok(ProgramInfo {
        program,
        source_info,
        vmt,
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
    /// The name the definition site writes, without any qualification (`deposit` for
    /// `function Account:deposit()`), or the synthetic `%chunk` / `%anonN` for a function with no
    /// name of its own. Recorded here at collection time -- while the name node is still in hand --
    /// and published in the VMT by [`Lowerer::build_vmt`], so nothing downstream has to recover it
    /// from the qualified name.
    simple: String,
    /// Index of the source unit this function was found in.
    unit: usize,
    /// Body node: a `block` for a function definition, or the `chunk` root for the top-level.
    body: Node<'a>,
    /// Parameter list node (`parameters`), if any.
    params: Option<Node<'a>>,
    /// Whether this is a `:` method (an implicit `self` parameter is prepended).
    is_method: bool,
}

/// Chunk-level name facts for one source unit, recovered before that unit is lowered. These drive
/// both fully-qualified definition names and call-site name resolution.
#[derive(Default)]
struct UnitScope {
    /// The chunk-level local this file `return`s, if any. Its fields are the module's exports, so
    /// `function <it>.f()` is named `<module>.f` rather than `<module>.<it>.f`.
    module_table: Option<String>,
    /// Chunk-level local names, whether or not they denote anything nameable. A local with no
    /// alias holds an ordinary value, which is *not* a name -- distinguishing it from a global is
    /// what keeps `local buf = {}` from being mistaken for a global table named `buf`.
    locals: HashSet<String>,
    /// Name -> the fully-qualified name it denotes, for names visible to every function in the
    /// unit: file-local tables used as namespaces, `require` results, aliased API paths, and each
    /// `local function`. Seeded into every function's alias environment.
    aliases: HashMap<String, String>,
    /// File-local tables that something is defined *into* (`function T.m`, `T.f = ...`), mapped to
    /// the qualified name they denote (`<module>.T`). A table used this way is a namespace rather
    /// than a value; a global one already names itself and is not listed here.
    ///
    /// Nesting is deliberately ignored: Kong's PDK declares its tables inside `function new()`,
    /// and `function _REQUEST.get_headers()` there means the same `_REQUEST` as the call
    /// `_REQUEST.get_headers()` beside it. Qualifying by file rather than by scope is what keeps
    /// the two agreeing -- and keeps two files' `_REQUEST` apart.
    namespaces: HashMap<String, String>,
}

struct Lowerer<'a> {
    /// The units of this import, and their parse trees (parallel; indexed by `unit`).
    units: &'a [SourceUnit],
    trees: &'a [Tree],
    /// Per-unit source-info key (parallel to `units`).
    keys: Vec<ArtifactKey>,
    /// Per-unit chunk-level name facts, filled by [`Lowerer::scan_unit`].
    unit_scopes: Vec<UnitScope>,
    /// The unit currently being scanned/collected/lowered; selects `src`, `module` and `key`.
    unit: usize,

    program: Program,
    sib: SourceInfoBuilder,

    /// All functions to lower, in discovery order.
    funcs: Vec<FuncEntry<'a>>,
    /// Set of names already used for a function definition (for uniqueness).
    used_names: HashMap<String, FunctionIdx>,
    anon_counter: usize,
    /// Maps a function-definition node (by unit and node id) to the function it was collected as,
    /// so a closure value expression can recover the [`FunctionIdx`] of its anonymous function.
    func_by_node: HashMap<(usize, usize), FunctionIdx>,
    /// For each closure (anonymous function that captures upvalues), the names it captures. The
    /// enclosing function stores each captured value into a field of the closure object, and the
    /// closure body reads it back from its synthetic self-parameter (`build_var`).
    closure_upvalues: HashMap<FunctionIdx, Vec<String>>,

    // ---- class / metatable recognition (Phase 1) ----
    /// Class table name -> its methods (`(method name, lowered function)`), gathered from
    /// `function T.m` / `function T:m` definitions. Keyed by the class table's lexical name.
    class_methods: HashMap<String, Vec<(String, FunctionIdx)>>,
    /// Subclass name -> `__index` parent name (the metatable chain edge).
    class_parent: HashMap<String, String>,
    /// Every table recognized as a class (has an `__index`, or methods, or a class metatable).
    class_names: HashSet<String>,
    /// Every method name known to belong to some class (gates `LuaCall` emission: a `o:m()` whose
    /// `m` is not a known class method stays a name-based `DirectCall`, keeping it sound).
    method_names: HashSet<String>,
    /// Classes whose `__index` is a function value: dispatch is opaque, so their instances take the
    /// name-based fallback rather than `LuaCall` (Phase 1c/1d).
    opaque_classes: HashSet<String>,
    /// Count of construction sites whose metatable could not be resolved to a class (surfaced at
    /// import so precision regressions are visible; Phase 1d).
    opaque_alloc_count: usize,
    /// Count of call sites whose callee could not be resolved to a qualified name, and so fall
    /// back to the path as written (surfaced at import alongside `opaque_alloc_count`).
    unresolved_call_count: usize,
    /// Every name emitted as a [`CallEdges::Explicit`] target. [`Lowerer::build_vmt`] subtracts
    /// the names it actually defined to get the import's *externals* — the stdlib and anything
    /// from a module outside the import — which the VMT needs so models can name them.
    /// Collected here rather than derived from the finished program because the call lowering is
    /// the only place that knows the callee text as written.
    called_names: HashSet<String>,
    /// Virtual method table recovered for this module (set in [`Lowerer::build_vmt`]).
    vmt: VirtualMethodTable,

    // ---- per-function state ----
    fidx: FunctionIdx,
    /// Upvalue names captured by the function currently being lowered; each resolves to a field of
    /// the closure's self-parameter rather than to a local or global.
    cur_upvalues: HashMap<String, ()>,
    /// Lexical scope stack; innermost last.
    scopes: Vec<HashMap<String, Binding>>,
    /// Name -> the qualified name it denotes, per lexical scope (pushed and popped with `scopes`).
    /// A `None` entry is a name that denotes nothing nameable, which is how a `local` shadows an
    /// outer alias -- `local Account = {}` inside a function must not resolve to the file's
    /// `Account`.
    alias_scopes: Vec<HashMap<String, Option<String>>>,
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
    fn new(units: &'a [SourceUnit], trees: &'a [Tree]) -> Self {
        let keys = units
            .iter()
            .map(|u| ArtifactKey {
                path: u.path.to_string_lossy().into_owned(),
                sub_artifact_id: 0,
                hash: Vec::new(),
                encoding: source_info::ArtifactEncoding::Utf8,
            })
            .collect();
        Self {
            units,
            trees,
            keys,
            unit_scopes: units.iter().map(|_| UnitScope::default()).collect(),
            unit: 0,
            program: Program::default(),
            sib: SourceInfoBuilder::new(ArtifactMetadata::new()),
            funcs: Vec::new(),
            used_names: HashMap::new(),
            anon_counter: 0,
            func_by_node: HashMap::new(),
            closure_upvalues: HashMap::new(),
            class_methods: HashMap::new(),
            class_parent: HashMap::new(),
            class_names: HashSet::new(),
            method_names: HashSet::new(),
            opaque_classes: HashSet::new(),
            opaque_alloc_count: 0,
            unresolved_call_count: 0,
            called_names: HashSet::new(),
            vmt: VirtualMethodTable::new_lua(),
            fidx: FunctionIdx::new(0),
            cur_upvalues: HashMap::new(),
            scopes: Vec::new(),
            alias_scopes: Vec::new(),
            labels: HashMap::new(),
            loop_breaks: Vec::new(),
            normal_arity: 0,
            temp_counter: 0,
            cur_span: SourceInfo::default(),
        }
    }

    fn run(&mut self) -> Result<(), Error> {
        // Each phase runs over every unit before the next begins, because each depends on the
        // previous having covered the *whole* import: names are resolved against modules defined
        // in other files, so nothing can be named until every unit has been scanned.
        for unit in 0..self.units.len() {
            self.scan_unit(unit);
        }
        for unit in 0..self.units.len() {
            self.unit = unit;
            self.collect_functions(self.root_node(unit));
        }
        // Recover class/metatable structure before lowering, so construction sites can be tagged
        // and method calls routed to `LuaCall` (Phase 1a/1c).
        for unit in 0..self.units.len() {
            self.unit = unit;
            self.recognize_classes(self.root_node(unit));
        }
        for entry in self.funcs.clone() {
            self.unit = entry.unit;
            self.lower_function(&entry)?;
        }
        // After lowering, not before: the `externals` column is the set of called names minus the
        // defined ones, and only the call lowering knows what was called. Nothing in lowering
        // reads `self.vmt`, and every other column comes from the collection/recognition passes
        // above, which are complete either way.
        self.build_vmt();
        // The parts a Lua import is made of. The function count is reported once, for every
        // language, by `crate::cli::import`.
        log::info!("lua: parsed {} .lua file(s)", self.units.len());
        if self.opaque_alloc_count > 0 {
            log::warn!(
                "lua: {} construction site(s) had an unresolved metatable; instances fall back to name-based dispatch",
                self.opaque_alloc_count
            );
        }
        if self.unresolved_call_count > 0 {
            log::warn!(
                "lua: {} call site(s) had a callee that could not be resolved to a qualified name",
                self.unresolved_call_count
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Per-unit accessors
    // ------------------------------------------------------------------

    /// Source text of the unit currently being processed.
    fn src(&self) -> &'a str {
        self.units[self.unit].source.as_str()
    }

    /// `require` module name of the unit currently being processed.
    fn module(&self) -> &'a str {
        self.units[self.unit].module.as_str()
    }

    fn root_node(&self, unit: usize) -> Node<'a> {
        self.trees[unit].root_node()
    }

    /// Qualifies `name` under the current unit's module (`<module>.name`).
    fn in_module(&self, name: &str) -> String {
        qualify(self.module(), name)
    }

    /// The unit whose module name is `module`, if this import contains it. `package.path`'s
    /// `?.lua` and `?/init.lua` both reach `a/b/init.lua`, so `require "a.b.init"` names the same
    /// unit as `require "a.b"`.
    fn unit_of_module(&self, module: &str) -> Option<usize> {
        self.units
            .iter()
            .position(|u| u.module == module)
            .or_else(|| {
                let stripped = module.strip_suffix(".init")?;
                self.units.iter().position(|u| u.module == stripped)
            })
    }

    /// The module a `require` names, canonicalized to the module name of the unit it resolves to,
    /// so that every spelling of a file resolves to the one name its definitions carry. A module
    /// outside the import keeps the name as written -- it is still a name, just an external one.
    fn required_module_canonical(&self, node: Node<'a>) -> Option<String> {
        let module = self.required_module(node)?;
        Some(match self.unit_of_module(&module) {
            Some(unit) => self.units[unit].module.clone(),
            None => module,
        })
    }

    // ------------------------------------------------------------------
    // Chunk-level name scanning (per unit, before any naming happens)
    // ------------------------------------------------------------------

    /// Recovers `unit`'s chunk-level name facts: which locals it declares, which of them are used
    /// as namespaces, what each `require`/alias binding denotes, and which local it returns as its
    /// module table. Runs before function collection, since definition names depend on all of it.
    fn scan_unit(&mut self, unit: usize) {
        self.unit = unit;
        let root = self.root_node(unit);
        let mut scope = UnitScope::default();

        // Namespaces first: a table only *is* a namespace by virtue of something being defined
        // into it, which can happen textually before or after its `local` declaration.
        let mut roots = Vec::new();
        let mut declared = HashSet::new();
        self.scan_namespace_roots(root, &mut roots, &mut declared);
        for name in roots {
            if declared.contains(&name) {
                let qualified = self.in_module(&name);
                scope.namespaces.insert(name, qualified);
            }
        }

        // Everything derived lands in one alias map, so name resolution is a single lookup.
        // Namespaces go in before the chunk is walked, so a binding to one (`local B = A`)
        // resolves.
        scope.aliases.extend(scope.namespaces.clone());

        self.scan_chunk_bindings(root, &mut scope);
        scope.module_table = self.scan_module_table(root, &scope);

        // The module table wins over the namespace rule: its fields are the module's exports.
        if let Some(table) = scope.module_table.clone() {
            scope.aliases.insert(table, self.module().to_string());
        }
        self.unit_scopes[unit] = scope;
    }

    /// Collects, over the whole unit at any nesting depth, every name that has something defined
    /// *into* it -- the table root of a `function T.m` / `function T:m` definition or of a
    /// `T.field = ...` assignment -- into `roots`, and every name the unit declares `local` into
    /// `declared`. A local table things are defined into is a namespace; a plain `local buf = {}`
    /// is a value and stays one, and a global root already names itself.
    fn scan_namespace_roots(
        &self,
        node: Node<'a>,
        roots: &mut Vec<String>,
        declared: &mut HashSet<String>,
    ) {
        let root_of = |name: Node<'a>| -> Option<String> {
            matches!(
                name.kind(),
                "dot_index_expression" | "method_index_expression"
            )
            .then(|| self.flatten_name(name).0)
        };
        match node.kind() {
            "function_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(root) = root_of(name) {
                        roots.push(root);
                    } else if is_local_declaration(node) {
                        declared.insert(self.node_text(name).to_string());
                    }
                }
            }
            "assignment_statement" => {
                if let Some(vlist) = child_of_kind(node, "variable_list") {
                    for t in named_of(vlist) {
                        if let Some(root) = root_of(t) {
                            roots.push(root);
                        }
                    }
                }
            }
            "variable_declaration" if is_local_declaration(node) => {
                // `local x` and `local x = ...` both bind names in the enclosed variable list.
                let list = node.named_child(0).and_then(|inner| match inner.kind() {
                    "assignment_statement" => child_of_kind(inner, "variable_list"),
                    "variable_list" => Some(inner),
                    _ => None,
                });
                if let Some(list) = list {
                    for v in named_of(list)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                    {
                        declared.insert(self.node_text(v).to_string());
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.scan_namespace_roots(child, roots, declared);
        }
    }

    /// Walks the chunk's statements in order, recording each chunk-level local and the qualified
    /// name it denotes, if any: `local req = require "a.b"` denotes module `a.b`, and
    /// `local get = ngx.req.get_headers` denotes that API path. Resolution consults the aliases
    /// recorded so far, so chains (`local sub = req.sub`) resolve left to right.
    fn scan_chunk_bindings(&self, chunk: Node<'a>, scope: &mut UnitScope) {
        for stmt in named_of(chunk) {
            let decl = match stmt.kind() {
                "variable_declaration" => stmt.named_child(0),
                _ => None,
            };
            let Some(decl) = decl else { continue };
            match decl.kind() {
                "assignment_statement" => {
                    let targets = child_of_kind(decl, "variable_list")
                        .map(|v| named_of(v))
                        .unwrap_or_default();
                    let values = child_of_kind(decl, "expression_list")
                        .map(|e| named_of(e))
                        .unwrap_or_default();
                    // Resolve every value against the *pre-declaration* scope, so `local x = x`
                    // denotes what the outer `x` did. Dropping the targets' own aliases first is
                    // what makes OpenResty's pervasive `local kong = kong` keep meaning the
                    // global `kong` rather than becoming this file's `kong`.
                    for t in targets.iter().filter(|n| n.kind() == "identifier") {
                        scope.aliases.remove(self.node_text(*t));
                    }
                    let qnames: Vec<Option<String>> =
                        values.iter().map(|&v| self.scan_qname(v, scope)).collect();
                    for (i, t) in targets
                        .iter()
                        .filter(|n| n.kind() == "identifier")
                        .enumerate()
                    {
                        let name = self.node_text(*t).to_string();
                        match qnames.get(i) {
                            Some(Some(q)) => {
                                scope.aliases.insert(name.clone(), q.clone());
                            }
                            // Binds a value, not a name -- unless the file defines into it, in
                            // which case it is a namespace and keeps denoting one.
                            _ => match scope.namespaces.get(&name).cloned() {
                                Some(q) => {
                                    scope.aliases.insert(name.clone(), q);
                                }
                                None => {
                                    scope.aliases.remove(&name);
                                }
                            },
                        }
                        scope.locals.insert(name);
                    }
                }
                "variable_list" => {
                    // A bare `local x` binds nothing yet, but a later `x.f = ...` can still make
                    // it a namespace, so keep that name if it has one.
                    for v in named_of(decl)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                    {
                        let name = self.node_text(v).to_string();
                        if !scope.namespaces.contains_key(&name) {
                            scope.aliases.remove(&name);
                        }
                        scope.locals.insert(name);
                    }
                }
                _ => {}
            }
        }
    }

    /// The chunk-level local a file `return`s -- its module table.
    fn scan_module_table(&self, chunk: Node<'a>, scope: &UnitScope) -> Option<String> {
        let ret = named_of(chunk)
            .into_iter()
            .find(|n| n.kind() == "return_statement")?;
        let expr = named_of(child_of_kind(ret, "expression_list")?)
            .into_iter()
            .next()?;
        if expr.kind() != "identifier" {
            return None;
        }
        let name = self.node_text(expr).to_string();
        scope.locals.contains(&name).then_some(name)
    }

    /// Name resolution against a [`UnitScope`] under construction (see [`Lowerer::qname_of`] for
    /// the lowering-time counterpart, which additionally honors function-local scopes).
    fn scan_qname(&self, node: Node<'a>, scope: &UnitScope) -> Option<String> {
        match node.kind() {
            "identifier" | "global" => {
                let name = self.node_text(node);
                if let Some(q) = scope.aliases.get(name) {
                    return Some(q.clone());
                }
                // An unaliased local holds a value, not a name; anything else is a global, which
                // names itself.
                (!scope.locals.contains(name)).then(|| name.to_string())
            }
            "dot_index_expression" => {
                let table = node.child_by_field_name("table")?;
                let field = node.child_by_field_name("field")?;
                Some(format!(
                    "{}.{}",
                    self.scan_qname(table, scope)?,
                    self.node_text(field)
                ))
            }
            "parenthesized_expression" => self.scan_qname(node.named_child(0)?, scope),
            "function_call" => self.required_module_canonical(node),
            _ => None,
        }
    }

    /// The module named by a `require "a.b"` / `require("a.b")` call, when the argument is a
    /// string literal. The module need not be part of this import (an external `require` still
    /// names a module); [`Lowerer::unit_of_module`] decides whether it resolves to a chunk here.
    fn required_module(&self, node: Node<'a>) -> Option<String> {
        let name = node.child_by_field_name("name")?;
        if !matches!(name.kind(), "identifier" | "global") || self.node_text(name) != "require" {
            return None;
        }
        let arg = named_of(node.child_by_field_name("arguments")?)
            .into_iter()
            .next()?;
        (arg.kind() == "string").then(|| self.string_content(arg))
    }

    // ------------------------------------------------------------------
    // Name qualification
    // ------------------------------------------------------------------

    /// Splits a (possibly dotted) name into its root identifier and the field components after it:
    /// `A.b.c` -> `("A", ["b", "c"])`, `A:m` -> `("A", ["m"])`.
    fn flatten_name(&self, node: Node<'a>) -> (String, Vec<String>) {
        let field = match node.kind() {
            "dot_index_expression" => "field",
            "method_index_expression" => "method",
            _ => return (self.node_text(node).to_string(), Vec::new()),
        };
        let Some(table) = node.child_by_field_name("table") else {
            return (self.node_text(node).to_string(), Vec::new());
        };
        let (root, mut rest) = self.flatten_name(table);
        if let Some(f) = node.child_by_field_name(field) {
            rest.push(self.node_text(f).to_string());
        }
        (root, rest)
    }

    /// The fully-qualified IR name for a `function` definition, from its name node.
    ///
    /// The root decides the qualification: a `local function` and any file-local table are
    /// namespaced under the module, the module table's fields *are* the module's exports, a
    /// `require`d module keeps that module's name, and a global root names itself -- so
    /// `function kong.request.get_header()` is `kong.request.get_header` in every file that
    /// defines it, which is what lets an externals shim stand in for the real thing.
    fn qualified_def_name(&self, name: Node<'a>, is_local: bool) -> String {
        let (root, rest) = self.flatten_name(name);
        let scope = &self.unit_scopes[self.unit];
        let qualified_root = if is_local {
            self.in_module(&root)
        } else if let Some(q) = scope.aliases.get(&root) {
            q.clone()
        } else {
            // A root that is neither aliased nor a plain local is a global: it names itself.
            root
        };
        std::iter::once(qualified_root)
            .chain(rest)
            .collect::<Vec<_>>()
            .join(".")
    }

    // ------------------------------------------------------------------
    // Class / metatable recognition (Phase 1a/1b/1c)
    // ------------------------------------------------------------------

    /// The stable class symbol for a class table's qualified name (`Account` in `oop.lua` ->
    /// `lua$class$oop.Account`). Kept in the same symbol space as the allocation tags emitted at
    /// construction sites, so the CHA resolvents join; qualifying it keeps two files' same-named
    /// class tables apart.
    fn class_symbol(&self, name: &str) -> Symbol {
        Symbol::from(format!("lua$class${name}").as_str())
    }

    /// The qualified name a chunk-level identifier denotes in the current unit, falling back to
    /// the identifier itself (a global names itself). Used by the class recognition pass, which
    /// runs outside the lowering scopes.
    fn unit_qname(&self, name: &str) -> String {
        self.unit_scopes[self.unit]
            .aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Walks the whole tree recovering class tables, their methods, and the `__index` hierarchy.
    /// Runs after [`Lowerer::collect_functions`] so method function definitions already have a
    /// [`FunctionIdx`] recorded in `func_by_node`.
    fn recognize_classes(&mut self, node: Node<'a>) {
        match node.kind() {
            "function_declaration" => {
                if let Some(name) = node.child_by_field_name("name")
                    && matches!(
                        name.kind(),
                        "dot_index_expression" | "method_index_expression"
                    )
                    && let Some(tbl) = name.child_by_field_name("table")
                    && tbl.kind() == "identifier"
                    && let Some(&fidx) = self.func_by_node.get(&(self.unit, node.id()))
                {
                    let cls = self.unit_qname(self.node_text(tbl));
                    let m = self.def_name(name);
                    self.class_names.insert(cls.clone());
                    self.method_names.insert(m.clone());
                    self.class_methods.entry(cls).or_default().push((m, fidx));
                }
            }
            "assignment_statement" => self.recognize_assign(node),
            "variable_declaration" => {
                if let Some(inner) = node.named_child(0)
                    && inner.kind() == "assignment_statement"
                {
                    self.recognize_assign(inner);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.recognize_classes(child);
        }
    }

    /// Recognizes the two class-shaping assignment idioms:
    /// - `T.__index = X` — marks `T` a class; `X != T` records `T`'s `__index` parent, and an
    ///   `X` that is a function value marks `T` opaque for dispatch.
    /// - `T = setmetatable({}, { __index = P })` — records `T --__index--> P`.
    fn recognize_assign(&mut self, node: Node<'a>) {
        let vlist = child_of_kind(node, "variable_list");
        let elist = child_of_kind(node, "expression_list");
        let targets: Vec<Node<'a>> = vlist
            .map(|v| {
                named_of(v)
                    .into_iter()
                    .filter(|n| n.kind() != "attribute")
                    .collect()
            })
            .unwrap_or_default();
        let rhs: Vec<Node<'a>> = elist.map(named_of).unwrap_or_default();
        for (i, t) in targets.iter().enumerate() {
            let val = rhs.get(i).copied();
            match t.kind() {
                "dot_index_expression" => {
                    let field = t.child_by_field_name("field").map(|f| self.node_text(f));
                    if field != Some("__index") {
                        continue;
                    }
                    let Some(tbl) = t
                        .child_by_field_name("table")
                        .filter(|n| n.kind() == "identifier")
                    else {
                        continue;
                    };
                    let cls = self.unit_qname(self.node_text(tbl));
                    self.class_names.insert(cls.clone());
                    match val.map(|v| v.kind()) {
                        Some("identifier") => {
                            let x = self.unit_qname(self.node_text(val.unwrap()));
                            // `T.__index = T` is a self-lookup root; a different name is a parent.
                            if x != cls {
                                self.class_parent.insert(cls, x);
                            }
                        }
                        // `__index` bound to a function value: dispatch is opaque (Phase 1c).
                        Some("function_definition") => {
                            self.opaque_classes.insert(cls);
                        }
                        // Computed / non-literal `__index`: leave as a plain class root.
                        _ => {}
                    }
                }
                "identifier" => {
                    // `T = setmetatable({}, MT)`: a table-constructor MT with an `__index = P`
                    // identifier is an inheritance edge; an identifier MT is an *instance* of a
                    // class (handled at lowering, not here).
                    if let Some(v) = val
                        && let Some((_tbl, mt)) = self.as_setmetatable_call(v)
                        && mt.kind() == "table_constructor"
                        && let Some(parent) = self.index_parent_of_constructor(mt)
                    {
                        let cls = self.unit_qname(self.node_text(*t));
                        self.class_names.insert(cls.clone());
                        self.class_parent.insert(cls, parent);
                    }
                }
                _ => {}
            }
        }
    }

    /// If `node` is a `setmetatable(a, b)` call, returns its two argument nodes.
    fn as_setmetatable_call(&self, node: Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
        if node.kind() != "function_call" {
            return None;
        }
        let name = node.child_by_field_name("name")?;
        if !matches!(name.kind(), "identifier" | "global") || self.node_text(name) != "setmetatable"
        {
            return None;
        }
        let args = node
            .child_by_field_name("arguments")
            .map(named_of)
            .unwrap_or_default();
        match (args.first(), args.get(1)) {
            (Some(a), Some(b)) => Some((*a, *b)),
            _ => None,
        }
    }

    /// The `__index` parent named in a table-constructor metatable (`{ __index = Base }` -> `Base`),
    /// when it is a bare identifier.
    fn index_parent_of_constructor(&self, tc: Node<'a>) -> Option<String> {
        for field in named_of(tc) {
            if field.kind() != "field" {
                continue;
            }
            let name = field.child_by_field_name("name").map(|n| self.node_text(n));
            if name == Some("__index")
                && let Some(value) = field.child_by_field_name("value")
                && value.kind() == "identifier"
            {
                return Some(self.unit_qname(self.node_text(value)));
            }
        }
        None
    }

    /// Lowers the recovered class maps -- and the name of every collected function -- into
    /// [`VirtualMethodTable::Lua`] on `self.vmt`.
    fn build_vmt(&mut self) {
        let mut methods: Vec<(Symbol, Symbol, Symbol)> = Vec::new();
        for (cls, ms) in &self.class_methods {
            let cls_sym = self.class_symbol(cls);
            for (m, fidx) in ms {
                let fname = self.program[*fidx].name.clone();
                methods.push((
                    cls_sym.clone(),
                    Symbol::from(m.as_str()),
                    Symbol::from(fname.as_str()),
                ));
            }
        }
        // Deterministic ordering (the recognition walk is deterministic, but the method map is a
        // HashMap; sort so the emitted VMT — and thus resolvent order — is stable).
        methods.sort_unstable();

        // The VMT's `hierarchy` is a `hashbrown::HashMap` (matching `call.rs`), distinct from the
        // std map used elsewhere in this module.
        let mut hierarchy: hashbrown::HashMap<Symbol, SmallVec<[Symbol; 2]>> =
            hashbrown::HashMap::new();
        for (sub, sup) in &self.class_parent {
            hierarchy
                .entry(self.class_symbol(sub))
                .or_default()
                .push(self.class_symbol(sup));
        }
        // Every collected function, with the simple name its definition site wrote. `self.funcs`
        // holds one entry per lowered function -- the chunk, every declaration, every anonymous
        // definition -- and collection has already run over every unit by the time this does, so
        // the column is complete. Order follows `self.funcs`, which the collection walk builds
        // deterministically; no sort needed.
        let functions: Vec<(Symbol, Symbol)> = self
            .funcs
            .iter()
            .map(|entry| {
                (
                    Symbol::from(entry.simple.as_str()),
                    Symbol::from(self.program[entry.fidx].name.as_str()),
                )
            })
            .collect();

        // Externals: every explicitly-called name that no lowered function defines. Registered
        // under both spellings a Lua library function can be called by -- the fq callee text
        // (`os.execute`) and its last dotted component (`execute`) -- because method-call syntax
        // drops the prefix (`s:format(x)` lowers to a call of `format`) while dotted syntax keeps
        // it. One model generator then covers both. Unlike `functions` above, the simple name is
        // split off the fq name: an external has no definition site to read it from.
        //
        // Sorted because it comes from a `HashSet` and the VMT feeds resolvent order.
        let defined: HashSet<String> = functions.iter().map(|(_, fq)| fq.to_string()).collect();
        let mut externals: Vec<(Symbol, Symbol)> = self
            .called_names
            .iter()
            .filter(|name| !defined.contains(name.as_str()))
            .map(|name| {
                let simple = name
                    .rsplit_once('.')
                    .map_or(name.as_str(), |(_, last)| last);
                (Symbol::from(simple), Symbol::from(name.as_str()))
            })
            .collect();
        externals.sort_unstable();

        self.vmt = VirtualMethodTable::Lua {
            methods,
            functions,
            externals,
            hierarchy,
        };
    }

    // ------------------------------------------------------------------
    // Function discovery
    // ------------------------------------------------------------------

    fn collect_functions(&mut self, root: Node<'a>) {
        // The top-level chunk is a synthetic function holding all top-level statements. It is the
        // module's body, so `require "a.b"` resolves to `a.b.%chunk` (see `eval_call`).
        let fidx = self.new_named_function(self.in_module("%chunk"));
        self.funcs.push(FuncEntry {
            fidx,
            simple: "%chunk".to_string(),
            unit: self.unit,
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
                    let is_method = name_node
                        .map(|n| n.kind() == "method_index_expression")
                        .unwrap_or(false);
                    let is_local = is_local_declaration(child);
                    // Name the function by its fully-qualified name (see `qualified_def_name`),
                    // keeping the definition's own simple name alongside it: this is the one
                    // place both are in hand, and `new_named_function` may disambiguate the
                    // qualified name (`<module>.f%1`) without the function's simple name
                    // changing.
                    let (base, simple) = match name_node {
                        Some(n) => (self.qualified_def_name(n, is_local), self.def_name(n)),
                        None => self.fresh_anon_name(),
                    };
                    let fidx = self.new_named_function(base);
                    self.func_by_node.insert((self.unit, child.id()), fidx);
                    // A `local function f` is a name every call site in the file spells simply as
                    // `f`, so record what it denotes. Nesting is flattened: the alias is recorded
                    // for the whole unit, which is what makes a recursive call inside `f`'s own
                    // body -- lowered as its own function, with its own scopes -- resolve.
                    if is_local && let Some(name) = name_node.filter(|n| n.kind() == "identifier") {
                        let simple = self.node_text(name).to_string();
                        let qualified = self.program[fidx].name.clone();
                        self.unit_scopes[self.unit]
                            .aliases
                            .entry(simple)
                            .or_insert(qualified);
                    }
                    self.funcs.push(FuncEntry {
                        fidx,
                        simple,
                        unit: self.unit,
                        body: child.child_by_field_name("body").unwrap_or(child),
                        params: child.child_by_field_name("parameters"),
                        is_method,
                    });
                }
                "function_definition" => {
                    let (name, simple) = self.fresh_anon_name();
                    let fidx = self.new_named_function(name);
                    self.func_by_node.insert((self.unit, child.id()), fidx);
                    self.funcs.push(FuncEntry {
                        fidx,
                        simple,
                        unit: self.unit,
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

    /// A name for a function that has none of its own, as `(fully-qualified, simple)`. The
    /// counter runs across the whole import, not per file (see the `%anonN` warning in
    /// `docs/model-generators.md`).
    fn fresh_anon_name(&mut self) -> (String, String) {
        let n = self.anon_counter;
        self.anon_counter += 1;
        let simple = format!("%anon{n}");
        (self.in_module(&simple), simple)
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
        // Every function starts out seeing the names its file established at chunk level (module
        // tables, `require` results, hoisted API aliases, `local function`s), which in Lua it
        // reaches as upvalues. Chunk-level locals that denote no name are seeded too, as `None`:
        // they are values, and a function that captures one must not mistake it for a global of
        // the same name.
        let scope = &self.unit_scopes[entry.unit];
        self.alias_scopes = vec![
            scope
                .locals
                .iter()
                .map(|name| (name.clone(), None))
                .chain(
                    scope
                        .aliases
                        .iter()
                        .map(|(k, v)| (k.clone(), Some(v.clone()))),
                )
                .collect(),
        ];
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
        self.program[self.fidx]
            .params
            .parameters
            .push(ParameterType::ByVal);
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
        walk(self.src(), body, &mut max);
        max
    }

    /// Pre-allocate an (empty) block for every label in the function body, so both forward and
    /// backward `goto`s can target them. Does not descend into nested functions.
    fn prealloc_labels(&mut self, body: Node<'a>) {
        let mut names = Vec::new();
        collect_label_names(self.src(), body, &mut names);
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
                    Some(Terminator::new_kind(TerminatorKind::Return {
                        args: args.into(),
                    }));
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
        self.alias_scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.alias_scopes.pop();
    }

    fn declare_local(&mut self, name: &str) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), Binding::Local);
        // Shadow whatever the name denoted outside: until something binds it to a name, it holds a
        // value. A file-local namespace is the exception -- it denotes the same table wherever the
        // file declares it, which is what makes a definition into it and a call through it agree.
        let namespace = self.unit_scopes[self.unit].namespaces.get(name).cloned();
        self.alias_scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), namespace);
    }

    /// Records that `name` denotes the qualified name `qname` in the innermost scope.
    fn declare_alias(&mut self, name: &str, qname: Option<String>) {
        self.alias_scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), qname);
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(*b);
            }
        }
        None
    }

    /// The innermost binding of `name` in the alias environment: `Some(Some(q))` when it denotes
    /// the qualified name `q`, `Some(None)` when it is bound but denotes only a value, and `None`
    /// when it is not bound at all (a global).
    fn lookup_alias(&self, name: &str) -> Option<Option<&str>> {
        for scope in self.alias_scopes.iter().rev() {
            if let Some(q) = scope.get(name) {
                return Some(q.as_deref());
            }
        }
        None
    }

    /// The fully-qualified name an expression denotes, or `None` when it names nothing the
    /// frontend can resolve statically (a call result, a computed index, or a local holding a
    /// plain value). This is the call-site counterpart of [`Lowerer::qualified_def_name`]: it is
    /// what lets `get_headers()` -- hoisted from `ngx.req.get_headers` -- reach the definition
    /// that call actually names.
    fn qname_of(&self, node: Node<'a>) -> Option<String> {
        match node.kind() {
            "identifier" | "global" => {
                let name = self.node_text(node);
                match self.lookup_alias(name) {
                    Some(Some(q)) => Some(q.to_string()),
                    Some(None) => None,
                    // Not bound anywhere: a free name is a global, and a global names itself.
                    None if self.lookup(name).is_none() => Some(name.to_string()),
                    None => None,
                }
            }
            "dot_index_expression" => {
                let table = node.child_by_field_name("table")?;
                let field = node.child_by_field_name("field")?;
                Some(format!(
                    "{}.{}",
                    self.qname_of(table)?,
                    self.node_text(field)
                ))
            }
            "parenthesized_expression" => self.qname_of(node.named_child(0)?),
            "function_call" => self.required_module_canonical(node),
            _ => None,
        }
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
        self.local_ref(&format!("%t{n}"))
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
                    // Register a `local function f` name so later value-uses resolve to a local,
                    // and record the qualified name it denotes so calls to it resolve. The body was
                    // collected separately and is lowered on its own.
                    if let Some(name) = s.child_by_field_name("name")
                        && name.kind() == "identifier"
                        && is_local_declaration(s)
                    {
                        let nm = self.node_text(name).to_string();
                        let qualified = self
                            .func_by_node
                            .get(&(self.unit, s.id()))
                            .map(|&f| self.program[f].name.clone());
                        self.declare_local(&nm);
                        self.declare_alias(&nm, qualified);
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
                    .map(|v| {
                        named_of(v)
                            .into_iter()
                            .filter(|n| n.kind() != "attribute")
                            .collect()
                    })
                    .unwrap_or_default();
                let rhs: Vec<Node<'a>> = elist.map(named_of).unwrap_or_default();
                // Evaluate RHS before declaring the new locals so `local x = x` reads the outer x.
                // Resolve what each value *names* first, for the same reason.
                let qnames: Vec<Option<String>> = rhs.iter().map(|&e| self.qname_of(e)).collect();
                let values: Vec<Exp> = rhs.iter().map(|&e| self.eval_expr(e, blk)).collect();
                for (i, t) in targets.iter().enumerate() {
                    if t.kind() == "identifier" {
                        let nm = self.node_text(*t).to_string();
                        self.declare_local(&nm);
                        // `local req = require "a.b"` / `local get = ngx.req.get_headers`: the new
                        // local is another name for something the frontend can already name.
                        if let Some(Some(q)) = qnames.get(i) {
                            self.declare_alias(&nm, Some(q.clone()));
                        }
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
            .map(|v| {
                named_of(v)
                    .into_iter()
                    .filter(|n| n.kind() != "attribute")
                    .collect()
            })
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
        self.push_stmt(
            blk,
            StatementKind::assign(table.clone(), [Exp::new_str("{}")]),
        );
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
        let Some(&fidx) = self.func_by_node.get(&(self.unit, node.id())) else {
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
        collect_identifiers(
            self.src(),
            node.child_by_field_name("body").unwrap_or(node),
            &mut names,
        );
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
    fn indirect_call_target(
        &mut self,
        name_node: Node<'a>,
        blk: BasicBlockIdx,
    ) -> Option<AccessPath> {
        if !matches!(name_node.kind(), "identifier" | "global") {
            return None;
        }
        let name = self.node_text(name_node);
        // A name that denotes a definition -- directly, or through an alias -- is a direct call.
        if let Some(q) = self.qname_of(name_node)
            && self.used_names.contains_key(&q)
        {
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

        // `setmetatable(t, mt)` returns `t` with its metatable set. Model it as a copy of the table
        // that also carries an allocation-site object tag when `mt` names a known class, so the
        // index engine propagates the class through returns/summaries (Phase 1b).
        if let Some(result) = self.eval_setmetatable(name_node, node, blk) {
            return result;
        }

        let mut args: ThinVec<Exp> = ThinVec::new();

        // When the call is a hierarchy-resolvable method call (`o:m()` whose `m` is a known,
        // non-opaque class method), capture the receiver variable and method name so it lowers to a
        // `LuaCall` resolved via metatable/CHA rather than a name-based `DirectCall` (Phase 4 Tier 2).
        let mut lua_method: Option<(VariableRef, String)> = None;

        let callee = if let Some(name_node) = name_node {
            if name_node.kind() == "method_index_expression" {
                // `o:m(...)` desugars to `m(o, ...)`.
                let method = name_node
                    .child_by_field_name("method")
                    .map(|m| self.node_text(m).to_string())
                    .unwrap_or_default();
                if let Some(table) = name_node.child_by_field_name("table") {
                    let recv = self.eval_expr(table, blk);
                    // Materialize the receiver into a variable so it can be both actual arg 0 and
                    // the `LuaCall` receiver.
                    let recv_var = match recv {
                        Exp::Variable(v) => v,
                        other => {
                            let t = self.fresh_temp();
                            self.push_stmt(blk, StatementKind::assign(t.clone(), [other]));
                            t
                        }
                    };
                    args.push(Exp::Variable(recv_var.clone()));
                    if self.method_names.contains(&method) && !self.method_is_opaque(&method) {
                        lua_method = Some((recv_var, method.clone()));
                    }
                }
                (method, true)
            } else {
                self.resolve_call_name(node, name_node)
            }
        } else {
            (String::new(), false)
        };
        let (callee, callee_resolved) = callee;

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
        } else if let Some((receiver, method)) = lua_method {
            // Tier 2: resolved later via the receiver's object facts + the Lua CHA. The receiver is
            // already actual arg 0 (pushed above), so codegen does not re-insert it.
            CallStyle::LuaCall {
                receiver,
                method: Symbol::from(method.as_str()),
            }
        } else {
            // Tier 3: a callee the frontend could not resolve to a name gets the path as written.
            // It is a sound edge -- nothing else claims that name -- but it will only connect if a
            // shim or model defines the same string, so count it.
            if !callee_resolved {
                self.unresolved_call_count += 1;
                log::trace!("lua: unresolved callee `{callee}`");
            }
            // Whether or not it resolved, this is a name the program calls. `build_vmt` keeps
            // the ones no definition claims and publishes them as externals.
            self.called_names.insert(callee.clone());
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
            let base = name_node
                .child_by_field_name("table")
                .map(|t| self.node_text(t));
            let field = name_node
                .child_by_field_name("field")
                .map(|f| self.node_text(f));
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
                let srcs: Vec<Exp> = arg_nodes[1..]
                    .iter()
                    .map(|&a| self.eval_expr(a, blk))
                    .collect();
                let t = self.fresh_temp();
                self.push_stmt(blk, StatementKind::assign(t.clone(), srcs));
                Some(Exp::Variable(t))
            }
            _ => None,
        }
    }

    /// Lowers `setmetatable(t, mt)` (Phase 1b). Returns `Some(value)` where `value` is a single
    /// variable that (i) carries the table `t`'s data (setmetatable returns its first argument) and
    /// (ii) is tagged with an allocation-site object ref when `mt` resolves to a known class table.
    /// The tag rides the existing `call_target_assign` closure interprocedurally — including the
    /// engine's return-direction rule, which carries a tag on a callee's out-formal up to the
    /// caller's `call_arg` — so a `local acct = Account.new()` receives `acct`'s class for free.
    /// Returns `None` to fall through to the generic call path when `node` is not a
    /// `setmetatable` call.
    fn eval_setmetatable(
        &mut self,
        name_node: Option<Node<'a>>,
        node: Node<'a>,
        blk: BasicBlockIdx,
    ) -> Option<Exp> {
        let name_node = name_node?;
        if !matches!(name_node.kind(), "identifier" | "global")
            || self.node_text(name_node) != "setmetatable"
        {
            return None;
        }
        let args = node
            .child_by_field_name("arguments")
            .map(named_of)
            .unwrap_or_default();
        let table_exp = args
            .first()
            .map(|&a| self.eval_expr(a, blk))
            .unwrap_or_else(nil_exp);
        let cls = args.get(1).and_then(|&mt| self.metatable_class(mt));

        let obj = self.fresh_temp();
        let mut sources: Vec<Exp> = vec![table_exp];
        if let Some(cls) = cls {
            // A single assign carrying both the data (Variable) and the object tag (ObjectRef), so
            // one SSA version of `obj` gets both the field contents and the class tag.
            sources.push(Exp::ObjectRef(CallObject::LuaClass(
                self.class_symbol(&cls),
            )));
        } else {
            // Computed / non-class metatable: still model the return of `t`, but record the
            // imprecision (Phase 1d).
            self.opaque_alloc_count += 1;
        }
        self.push_stmt(blk, StatementKind::assign(obj.clone(), sources));
        Some(Exp::Variable(obj))
    }

    /// Whether a method name must take the name-based fallback because every class defining it has
    /// an opaque (function) `__index` (Phase 1c). Cheap and conservative: a method defined by any
    /// non-opaque class is treated as resolvable.
    fn method_is_opaque(&self, method: &str) -> bool {
        if self.opaque_classes.is_empty() {
            return false;
        }
        !self.class_methods.iter().any(|(cls, ms)| {
            !self.opaque_classes.contains(cls) && ms.iter().any(|(m, _)| m == method)
        })
    }

    /// The class named by a metatable argument, when it is a bare identifier naming a known class
    /// table (`setmetatable({}, Account)` -> `Account`). A table-constructor or computed metatable
    /// yields `None`.
    fn metatable_class(&self, mt: Node<'a>) -> Option<String> {
        if !matches!(mt.kind(), "identifier" | "global") {
            return None;
        }
        let name = self.qname_of(mt)?;
        self.class_names.contains(&name).then_some(name)
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

    /// The name a call site gives its target: the fully-qualified name the callee expression
    /// denotes, so that a call and the definition it reaches spell the same string even across
    /// files. A callee the frontend cannot name statically falls back to the path as written,
    /// which is what an externals shim would have to spell too.
    ///
    /// A `require "a.b"` whose module is part of this import names that module's chunk, so the
    /// module body -- and the table it returns -- is a real call edge rather than an opaque
    /// builtin.
    ///
    /// Returns the name and whether it was actually resolved; an unresolved callee is reported at
    /// import so the imprecision is visible.
    fn resolve_call_name(&mut self, call: Node<'a>, name: Node<'a>) -> (String, bool) {
        if let Some(module) = self.required_module(call) {
            return match self.unit_of_module(&module) {
                Some(unit) => (qualify(&self.units[unit].module, "%chunk"), true),
                // A `require` of a module outside this import stays an opaque builtin call.
                None => ("require".to_string(), false),
            };
        }
        match self.qname_of(name) {
            Some(q) => (q, true),
            None => (self.node_text(name).to_string(), false),
        }
    }

    // ------------------------------------------------------------------
    // Access-path helpers
    // ------------------------------------------------------------------

    /// Builds a [`RawPath`] for a bare name: a parameter or local becomes a bare variable; a free
    /// name is a field of the global heap (`$globals.name`), modeling `_ENV`.
    /// Interns `name` into the current function's locals table and returns a reference to it.
    fn local_ref(&mut self, name: &str) -> VariableRef {
        let fidx = self.fidx;
        VariableRef::new_local_idx(self.program[fidx].locals.get_or_intern(name))
    }

    fn build_var(&mut self, name: &str) -> RawPath {
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
                base: self.local_ref(name),
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
        let fidx = self.fidx;
        let locals = &mut self.program[fidx].locals;
        let ap = load_access_path(rp.base, rp.fields, &mut stmts, || {
            let v = VariableRef::new_local_idx(locals.get_or_intern(&format!("%t{counter}")));
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
            let fidx = self.fidx;
            let locals = &mut self.program[fidx].locals;
            store_access_path(target.base, target.fields, value, &mut stmts, || {
                let v = VariableRef::new_local_idx(locals.get_or_intern(&format!("%t{counter}")));
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
            let sources: Vec<Exp> = if srcs.is_empty() {
                vec![nil_exp()]
            } else {
                srcs
            };
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
        node.utf8_text(self.src().as_bytes()).unwrap_or("").trim()
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
        let key = self.keys[self.unit].clone();
        SourceInfo::new(self.sib.span_for(key, start, SpanLen::ByteLen(len)))
    }
}

fn empty_exp() -> Exp {
    Exp::new_bytes(Vec::new())
}

fn nil_exp() -> Exp {
    Exp::new_str("nil")
}

/// Qualifies `name` under `module`. A module with an empty name (a single file imported as the
/// root itself) contributes no prefix.
fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}.{name}")
    }
}

/// Whether a declaration is a `local` one (`local function f`, `local x = ...`). The grammar
/// aliases the local and global forms to the same node kind, distinguishing them only by the
/// leading keyword.
fn is_local_declaration(node: Node<'_>) -> bool {
    node.child(0).map(|c| c.kind() == "local").unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ctadl_ir::mir::visit::Visitor;

    /// Imports one in-memory file as module `m`.
    fn import_str(src: &str) -> ProgramInfo {
        import_modules(&[("m", src)])
    }

    /// Imports several in-memory files as one directory import, each with the given module name,
    /// so `require` resolution between them can be exercised without touching the filesystem.
    fn import_modules(modules: &[(&str, &str)]) -> ProgramInfo {
        let units = modules
            .iter()
            .map(|(module, src)| SourceUnit {
                path: PathBuf::from(format!("{}.lua", module.replace('.', "/"))),
                module: (*module).to_string(),
                source: (*src).to_string(),
            })
            .collect();
        lower_lua_units(units).expect("lua lowering failed")
    }

    /// The names of every function in the imported program, sorted.
    fn function_names(info: &ProgramInfo) -> Vec<String> {
        let mut names: Vec<String> = info
            .program
            .functions
            .functions
            .iter()
            .map(|f| f.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The recovered `(class, method)` pairs, sorted, ignoring the function-id column.
    fn method_pairs(vmt: &VirtualMethodTable) -> Vec<(String, String)> {
        match vmt {
            VirtualMethodTable::Lua { methods, .. } => {
                let mut v: Vec<(String, String)> = methods
                    .iter()
                    .map(|(cls, m, _fid)| (cls.to_string(), m.to_string()))
                    .collect();
                v.sort();
                v
            }
            other => panic!("expected VirtualMethodTable::Lua, got {other:?}"),
        }
    }

    /// The VMT's `(simple name, fully-qualified name)` pair for every function, sorted by
    /// qualified name.
    fn vmt_functions(vmt: &VirtualMethodTable) -> Vec<(String, String)> {
        match vmt {
            VirtualMethodTable::Lua { functions, .. } => {
                let mut v: Vec<(String, String)> = functions
                    .iter()
                    .map(|(simple, fq)| (simple.to_string(), fq.to_string()))
                    .collect();
                v.sort_by(|a, b| a.1.cmp(&b.1));
                v
            }
            other => panic!("expected VirtualMethodTable::Lua, got {other:?}"),
        }
    }

    /// The VMT's `(simple name, fully-qualified name)` pair for every external.
    fn vmt_externals(vmt: &VirtualMethodTable) -> Vec<(String, String)> {
        match vmt {
            VirtualMethodTable::Lua { externals, .. } => externals
                .iter()
                .map(|(simple, fq)| (simple.to_string(), fq.to_string()))
                .collect(),
            other => panic!("expected VirtualMethodTable::Lua, got {other:?}"),
        }
    }

    /// The recovered `subclass -> parent` edges, sorted.
    fn hierarchy_edges(vmt: &VirtualMethodTable) -> Vec<(String, String)> {
        match vmt {
            VirtualMethodTable::Lua { hierarchy, .. } => {
                let mut v: Vec<(String, String)> = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| {
                        sups.iter()
                            .map(move |sup| (sub.to_string(), sup.to_string()))
                    })
                    .collect();
                v.sort();
                v
            }
            other => panic!("expected VirtualMethodTable::Lua, got {other:?}"),
        }
    }

    /// Collects every allocation-site object class symbol tagged in the program IR.
    #[derive(Default)]
    struct ObjectTagFinder(Vec<String>);
    impl Visitor for ObjectTagFinder {
        fn visit_exp(&mut self, exp: &Exp) {
            if let Exp::ObjectRef(CallObject::LuaClass(cls)) = exp {
                self.0.push(cls.to_string());
            }
            self.super_exp(exp);
        }
    }
    fn object_tags(program_info: &ProgramInfo) -> Vec<String> {
        let mut f = ObjectTagFinder::default();
        for func in &program_info.program.functions.functions {
            f.visit_function_data(FunctionIdx::new(0), func);
        }
        f.0.sort();
        f.0.dedup();
        f.0
    }

    #[test]
    fn recovers_flat_class_methods() {
        // `Account` is a class table (`Account.__index = Account`) with three methods; there is no
        // inheritance edge (the `__index` is a self-root).
        let src = r#"
            local Account = {}
            Account.__index = Account
            function Account.new()
              return setmetatable({}, Account)
            end
            function Account:deposit(amount) self.value = amount end
            function Account:balance() return self.value end
        "#;
        let info = import_str(src);
        assert_eq!(
            method_pairs(&info.vmt),
            vec![
                ("lua$class$m.Account".to_string(), "balance".to_string()),
                ("lua$class$m.Account".to_string(), "deposit".to_string()),
                ("lua$class$m.Account".to_string(), "new".to_string()),
            ]
        );
        assert!(hierarchy_edges(&info.vmt).is_empty());
        // The `setmetatable({}, Account)` construction site is tagged with the class.
        assert_eq!(object_tags(&info), vec!["lua$class$m.Account".to_string()]);
        // Methods are named by the class table they are defined into, under the module.
        assert_eq!(
            function_names(&info),
            vec![
                "m.%chunk",
                "m.Account.balance",
                "m.Account.deposit",
                "m.Account.new"
            ]
        );
    }

    #[test]
    fn vmt_carries_a_simple_name_for_every_function() {
        // Every kind of definition the frontend names: the synthetic chunk, a `local function`, a
        // module-table field, a `:` method, and an anonymous function. The VMT's simple name is the
        // identifier each definition writes, never the module qualification around it.
        let src = r#"
            local M = {}
            local function helper() return 1 end
            function M.read() return helper() end
            local Account = {}
            Account.__index = Account
            function Account:deposit(x) self.v = x end
            M.cb = function(x) return x end
            return M
        "#;
        let info = import_str(src);
        assert_eq!(
            vmt_functions(&info.vmt),
            vec![
                ("%anon0".to_string(), "m.%anon0".to_string()),
                ("%chunk".to_string(), "m.%chunk".to_string()),
                ("deposit".to_string(), "m.Account.deposit".to_string()),
                ("helper".to_string(), "m.helper".to_string()),
                ("read".to_string(), "m.read".to_string()),
            ]
        );
        // The column covers the whole program, not just the class methods.
        assert_eq!(
            vmt_functions(&info.vmt)
                .iter()
                .map(|(_, fq)| fq.clone())
                .collect::<Vec<_>>(),
            function_names(&info)
        );
    }

    #[test]
    fn vmt_carries_called_but_undefined_functions_as_externals() {
        // `string.format` and `os.execute` are called and never defined; `helper` is both. The
        // method-call `s:sub(1, 3)` lowers to a call of the bare `sub`, which is why one model
        // naming `sub` has to cover both spellings.
        let src = r#"
            local function helper(x) return x end
            local function handler(s)
              local t = s:sub(1, 3)
              local cmd = string.format("echo %s", helper(t))
              os.execute(cmd)
            end
            return handler
        "#;
        let info = import_str(src);
        assert_eq!(
            vmt_externals(&info.vmt),
            vec![
                ("execute".to_string(), "os.execute".to_string()),
                ("format".to_string(), "string.format".to_string()),
                ("sub".to_string(), "sub".to_string()),
            ],
            "sorted, and `helper` is defined so it is not an external"
        );
        // The simple name of a dotted external is its last component -- there is no definition
        // site to read one off, unlike `functions`.
        assert!(
            !vmt_functions(&info.vmt)
                .iter()
                .any(|(_, fq)| fq == "os.execute"),
            "an external must not also appear as a defined function"
        );
    }

    /// The externals column is what makes a Lua propagation model file do anything: before it,
    /// every match index was built from the lowered definitions only, so a model naming a stdlib
    /// function matched nothing.
    #[test]
    fn a_model_can_name_a_lua_external() {
        use crate::models::{
            ImportScope, ProgramMatchIndex, ProgramModelMatches, json::ModelGeneratorIngest,
        };

        let info = import_str(r#"local function h(x) return os.getenv(x) end return h"#);
        for port in ["getenv", "os.getenv"] {
            let mut matches = ProgramModelMatches::default();
            let match_index = ProgramMatchIndex::new(&info, ImportScope::unknown());
            let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
            ingest
                .encode_models(vec![serde_json::json!({
                    "find": "methods",
                    "where": [{"constraint": "signature_match", "name": port}],
                    "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}
                })])
                .unwrap_or_else(|e| panic!("loading a model naming {port}: {e}"));
            drop(ingest);
            assert_eq!(
                matches
                    .propagations
                    .iter()
                    .map(|p| p.function.to_string())
                    .collect::<Vec<_>>(),
                vec!["os.getenv".to_string()],
                "a model naming `{port}` must summarize the external"
            );
        }
    }

    #[test]
    fn simple_name_survives_a_qualified_name_collision() {
        // Two `local function f`s in one module qualify to the same name, so the second's IR name
        // is disambiguated to `m.f%1`. Its simple name is still `f` -- which is why the model layer
        // reads this column instead of splitting the qualified name on `.`.
        let src = r#"
            local function f() return 1 end
            local function f() return 2 end
        "#;
        let info = import_str(src);
        assert_eq!(
            vmt_functions(&info.vmt),
            vec![
                ("%chunk".to_string(), "m.%chunk".to_string()),
                ("f".to_string(), "m.f".to_string()),
                ("f".to_string(), "m.f%1".to_string()),
            ]
        );
    }

    #[test]
    fn recovers_index_inheritance() {
        // `Derived` sets `Base` as its `__index` parent via `setmetatable({}, { __index = Base })`.
        let src = r#"
            local Base = {}
            Base.__index = Base
            function Base:set_data(d) self.data = d end
            function Base:get_data() return self.data end

            local Derived = setmetatable({}, { __index = Base })
            Derived.__index = Derived
            function Derived.new() return setmetatable({}, Derived) end
        "#;
        let info = import_str(src);
        assert_eq!(
            method_pairs(&info.vmt),
            vec![
                ("lua$class$m.Base".to_string(), "get_data".to_string()),
                ("lua$class$m.Base".to_string(), "set_data".to_string()),
                ("lua$class$m.Derived".to_string(), "new".to_string()),
            ]
        );
        assert_eq!(
            hierarchy_edges(&info.vmt),
            vec![(
                "lua$class$m.Derived".to_string(),
                "lua$class$m.Base".to_string()
            )]
        );
        // Only the `setmetatable({}, Derived)` instance site is tagged; the class-table definition
        // `setmetatable({}, { __index = Base })` (a table-constructor metatable) is not.
        assert_eq!(object_tags(&info), vec!["lua$class$m.Derived".to_string()]);
    }

    #[test]
    fn definitions_are_qualified_by_what_their_root_denotes() {
        // A local function and a file-local table are namespaced under the module; the module
        // table's fields are the module's exports; a global root names itself.
        let src = r#"
            local _M = {}
            local Helper = {}
            local function private() end
            function _M.get_headers() end
            function Helper.trim(s) return s end
            function Helper:run() end
            function kong.request.get_header() end
            function global_fn() end
            return _M
        "#;
        let info = import_modules(&[("kong.pdk.request", src)]);
        assert_eq!(
            function_names(&info),
            vec![
                "global_fn",
                "kong.pdk.request.%chunk",
                "kong.pdk.request.Helper.run",
                "kong.pdk.request.Helper.trim",
                "kong.pdk.request.get_headers",
                "kong.pdk.request.private",
                "kong.request.get_header",
            ]
        );
    }

    /// The callee names of every `DirectCall` in `function`, in order.
    #[derive(Default)]
    struct DirectCallFinder(Vec<String>);
    impl Visitor for DirectCallFinder {
        fn visit_call_edges(&mut self, edges: &CallEdges) {
            let CallEdges::Explicit(targets) = edges;
            self.0.extend(targets.iter().cloned());
        }
    }
    fn direct_calls(info: &ProgramInfo, function: &str) -> Vec<String> {
        let func = info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == function)
            .unwrap_or_else(|| panic!("no function named {function}"));
        let mut f = DirectCallFinder::default();
        f.visit_function_data(FunctionIdx::new(0), func);
        f.0
    }

    #[test]
    fn calls_resolve_through_require_and_aliases() {
        // A `require`d module, a field of it, and an alias of an external API each resolve to the
        // qualified name the call actually denotes -- not to the bare trailing name.
        let request = r#"
            local _M = {}
            function _M.get_headers() end
            return _M
        "#;
        let handler = r#"
            local request = require "kong.pdk.request"
            local get_headers = request.get_headers
            local ngx_headers = ngx.req.get_headers
            local function run()
              request.get_headers()
              get_headers()
              ngx_headers()
              kong.request.get_header()
            end
        "#;
        let info = import_modules(&[("kong.pdk.request", request), ("handler", handler)]);
        assert_eq!(
            direct_calls(&info, "handler.run"),
            vec![
                // Both spellings reach the one definition, including the hoisted local alias.
                "kong.pdk.request.get_headers",
                "kong.pdk.request.get_headers",
                // An alias of an API outside the import keeps that API's name rather than
                // colliding with the `get_headers` above.
                "ngx.req.get_headers",
                "kong.request.get_header",
            ]
        );
        // The `require` itself is a call to the required module's chunk, so the module table it
        // returns flows to the caller.
        assert_eq!(
            direct_calls(&info, "handler.%chunk"),
            vec!["kong.pdk.request.%chunk"]
        );
    }

    #[test]
    fn a_namespace_declared_inside_a_function_is_still_module_qualified() {
        // Kong's PDK shape: the exported table is declared inside `new()`, not at chunk level.
        // The definition and the call beside it must agree on the name, and two files' `_REQUEST`
        // must stay apart.
        let src = r#"
            local _M = {}
            function _M.new()
              local _REQUEST = {}
              function _REQUEST.get_headers() end
              function _REQUEST.get_header(name)
                local h = _REQUEST.get_headers()
                return h[name]
              end
              return _REQUEST
            end
            return _M
        "#;
        let info = import_modules(&[("kong.pdk.request", src), ("kong.pdk.response", src)]);
        assert!(
            function_names(&info).contains(&"kong.pdk.request._REQUEST.get_headers".to_string())
        );
        assert!(
            function_names(&info).contains(&"kong.pdk.response._REQUEST.get_headers".to_string())
        );
        assert_eq!(
            direct_calls(&info, "kong.pdk.request._REQUEST.get_header"),
            vec!["kong.pdk.request._REQUEST.get_headers"]
        );
    }

    #[test]
    fn localizing_a_global_keeps_the_global_meaning() {
        // `local kong = kong` is pervasive in OpenResty. The local must keep denoting the global,
        // even in a file that also defines into it -- otherwise every call through it would be
        // renamed to that one file's private `kong`.
        let src = r#"
            local kong = kong
            local ngx = ngx
            kong.plugin_flag = true
            local function run()
              kong.request.get_header("x")
              ngx.req.get_headers()
            end
        "#;
        let info = import_modules(&[("kong.plugins.cors.handler", src)]);
        assert_eq!(
            direct_calls(&info, "kong.plugins.cors.handler.run"),
            vec!["kong.request.get_header", "ngx.req.get_headers"]
        );
    }

    #[test]
    fn every_spelling_of_a_module_resolves_to_one_name() {
        // `package.path` reaches `a/b/init.lua` through both `?.lua` and `?/init.lua`, so Kong
        // requires the same file as `kong.db.dao` in one place and `kong.db.dao.init` in another.
        // Both must name the definitions that file actually carries.
        let dao = "local _M = {}\nfunction _M.new() end\nreturn _M";
        let caller = r#"
            local short = require "kong.db.dao"
            local long = require "kong.db.dao.init"
            local function run()
              short.new()
              long.new()
            end
        "#;
        let info = import_modules(&[("kong.db.dao", dao), ("caller", caller)]);
        assert_eq!(
            direct_calls(&info, "caller.run"),
            vec!["kong.db.dao.new", "kong.db.dao.new"]
        );
    }

    #[test]
    fn same_named_functions_in_different_modules_stay_distinct() {
        // The defect this naming scheme exists to fix: two unrelated `get_headers` definitions
        // must not collapse into one symbol.
        let a = "local _M = {}\nfunction _M.get_headers() end\nreturn _M";
        let b = "local _M = {}\nfunction _M.get_headers() end\nreturn _M";
        let info = import_modules(&[("a.req", a), ("b.req", b)]);
        assert!(function_names(&info).contains(&"a.req.get_headers".to_string()));
        assert!(function_names(&info).contains(&"b.req.get_headers".to_string()));
    }

    #[test]
    fn a_local_holding_a_closure_is_still_an_indirect_call() {
        // Aliasing must not swallow first-class function values: `h` denotes no name, so the call
        // stays resolved by data flow rather than becoming a direct edge to something named `h`.
        let src = r#"
            local function run(g)
              local h = function(x) return x end
              return h(1), g(2)
            end
        "#;
        let info = import_modules(&[("m", src)]);
        assert_eq!(direct_calls(&info, "m.run"), Vec::<String>::new());
    }

    #[test]
    fn computed_metatable_is_not_tagged() {
        // A non-literal metatable cannot be resolved to a class, so the construction site takes the
        // fallback and produces no allocation tag.
        let src = r#"
            local function make(mt)
              return setmetatable({}, mt)
            end
        "#;
        let info = import_str(src);
        assert!(object_tags(&info).is_empty());
    }

    #[test]
    fn a_file_that_is_not_utf8_still_parses() {
        // Latin-1 bytes in a comment, plus a byte pair that no encoding of ours accepts. The code
        // around them is ordinary Lua, so the import should recover the function rather than fail.
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("m.lua");
        let mut bytes = b"local M = {}\n-- caf".to_vec();
        bytes.extend_from_slice(&[0xe9, b' ', 0xff, 0xfe]);
        bytes.extend_from_slice(b"\nfunction M.hello() return 1 end\nreturn M\n");
        std::fs::write(&file, &bytes).expect("writing source");

        let info = import_lua(dir.path()).expect("import of a non-UTF-8 file failed");
        assert!(function_names(&info).contains(&"m.hello".to_string()));
    }
}
