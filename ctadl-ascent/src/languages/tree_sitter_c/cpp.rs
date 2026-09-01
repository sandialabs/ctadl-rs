//! C++ frontend: `parse_cpp_program`.
//!
//! This drives the **tree-sitter-cpp** grammar but reuses the C frontend's
//! language-neutral lowering core in the parent module ([`super::Context`] and its
//! statement/expression walkers, the scope tree, CFG/IR builders, and temp allocator) —
//! the same code the C frontend runs after parsing. The shared core never branches on the
//! language: it is handed the C++ grammar via [`Context::new`](super::Context) and uses it
//! only to compile queries against the grammar that parsed the tree. Any **C++-specific**
//! lowering belongs in this module, never as a language branch in the shared core.
//!
//! `parse_c_program` is left byte-for-byte unchanged; this is a *new* entry point, per
//! the constitution's "new entry point + shared lowering core" rule.
//!
//! As of Milestone 2 (`c-frontend-parity`) the C++ frontend ingests the **entire C
//! subset** the C frontend handles — locals, control flow (`if`/`else`/`while`/`do`/`for`/
//! `switch`), structs and fields, pointers, arrays, globals, and direct/indirect calls —
//! at parity with the C frontend. It does so purely by reusing the shared lowering; the
//! only tree-sitter-cpp/-c node-shape divergences in that subset (`if`/`while`/`switch`
//! conditions wrapped in a `condition_clause`, and array indices nested under a
//! `subscript_argument_list`) are bridged by [`CPP_HOOKS`], not by any language branch in
//! the shared core. C++-only constructs (classes, methods, references, namespaces,
//! templates, …) are later milestones and may still error.

use ctadl_ir::mir::{CallEdges, CallStyle, Exp, Program, Statement, StatementKind, VariableRef};
use ctadl_ir::{PathSegment, ThinVec, thin_vec};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, QueryCursor};

use super::{
    ClassInfo, Context, GrammarHooks, MatchExtractor, RawPath, ScopeView, VarKind,
    declarator_leaf_ident, is_class_member_definition, markup, param_arity, to_str,
};
use crate::error::Error;

/// Map a C++ `if`/`while`/`switch` `condition` field to the expression to flatten.
///
/// tree-sitter-cpp wraps the condition in a `condition_clause` — `( [init;] value )`,
/// where `value` is the real test expression (C++17 allows a leading init-statement). C
/// instead exposes the condition directly as a `parenthesized_expression`. Unwrapping to
/// `value` here lets the shared walker flatten it exactly as it flattens C's condition,
/// with no language branch in the core. `do`/`for` conditions are *not* `condition_clause`
/// (do = parenthesized like C, for = a bare expression), so those sites keep the C hook.
fn cpp_condition_expr(node: Node<'_>) -> Node<'_> {
    node.child_by_field_name("value").unwrap_or(node)
}

/// Map a C++ `subscript_expression` to its index expression.
///
/// tree-sitter-cpp nests the index under an `indices` field (a `subscript_argument_list`,
/// because C++ supports multi-arg `a[i, j]` subscripting); the actual index is its first
/// named child. C uses a flat `index` field. Reading the first named child of `indices`
/// recovers the same single index node the C path gets from `index`.
fn cpp_subscript_index(node: Node<'_>) -> Node<'_> {
    node.child_by_field_name("indices")
        .and_then(|indices| indices.named_child(0))
        .or_else(|| node.child_by_field_name("index"))
        .expect("C++ subscript_expression always has an index under `indices`")
}

/// Recover the leaf member name from a `field_declaration`'s declarator. Scalar members
/// (`int v;`) declare it directly as a `field_identifier`; descend through any
/// pointer/array wrappers to reach it. Anything else (a nested function declarator, etc.)
/// is not a plain data member in this slice and yields `None`.
fn member_name<'a>(decl: Node<'_>, source: &'a str) -> Option<&'a str> {
    match decl.kind() {
        "field_identifier" => Some(to_str(&decl, source)),
        "pointer_declarator" | "array_declarator" | "parenthesized_declarator" => decl
            .child_by_field_name("declarator")
            .and_then(|d| member_name(d, source)),
        _ => None,
    }
}

/// Whether a class-member node carries the **`static`** storage-class specifier — a
/// `field_declaration` (`static int total;`) or a member `function_definition`
/// (`static void add(…)`). tree-sitter-cpp exposes it as a `storage_class_specifier` child whose
/// source text is `static` (the same node kind also spells `extern`, `register`, `thread_local`,
/// …, so we check the text). A `static` data member is modeled as a class-scoped global and a
/// `static` member function is lowered without the implicit `this` (spec 015). Inert for C: a C
/// struct field has no such child, and the C path never reaches class-member discovery.
fn has_static_specifier(node: Node<'_>, source: &str) -> bool {
    let mut c = node.walk();
    node.children(&mut c)
        .any(|ch| ch.kind() == "storage_class_specifier" && to_str(&ch, source) == "static")
}

/// Extract the **direct base-class names** of a `struct_specifier`/`class_specifier` from its
/// `base_class_clause`. The clause (`: Base`, `: public Base`) is an unnamed child of the
/// specifier holding, per base, an optional `access_specifier` / `virtual` keyword followed by
/// the base's `type_identifier`; we collect every `type_identifier` (so `: A, B` yields both,
/// though multiple inheritance is out of scope). A namespaced base (`ns::Base`,
/// `qualified_identifier`) or a base-less class yields nothing recorded here. Returns an empty
/// `Vec` when there is no `base_class_clause`. Inert for C (no such node).
fn base_class_names(specifier: Node<'_>, source: &str) -> Vec<String> {
    let mut bases = Vec::new();
    let mut sc = specifier.walk();
    for child in specifier.children(&mut sc) {
        if child.kind() != "base_class_clause" {
            continue;
        }
        let mut cc = child.walk();
        for b in child.children(&mut cc) {
            if b.kind() == "type_identifier" {
                bases.push(to_str(&b, source).to_string());
            }
        }
    }
    bases
}

/// Unwrap a `reference_declarator` (`Class& m(…)`, `int& get(…)`) to the `function_`
/// `declarator` it wraps; pass any other declarator through unchanged. A reference return
/// type nests the function declarator inside a `reference_declarator`, which the
/// method-discovery and top-level function queries key on the bare `function_declarator` and
/// would otherwise skip. Inert for C (no `reference_declarator`).
fn unwrap_reference_declarator(decl: Node<'_>) -> Node<'_> {
    if decl.kind() == "reference_declarator" {
        let mut c = decl.walk();
        decl.children(&mut c)
            .find(|n| n.kind() == "function_declarator")
            .unwrap_or(decl)
    } else {
        decl
    }
}

/// Whether a method body returns the receiver object itself (`return *this;`), so its
/// (reference) result aliases arg 0. Scans the body's `return` statements for a `*this` — a
/// `pointer_expression` dereferencing the `this` node. Combined with a reference return type
/// by the caller to mark a fluent setter as `returns_self`. Inert for C (no `this` node).
fn body_returns_deref_this(body: Node<'_>) -> bool {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_statement"
            && let Some(val) = n.named_child(0)
            && val.kind() == "pointer_expression"
            && val
                .child_by_field_name("argument")
                .is_some_and(|a| a.kind() == "this")
        {
            return true;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// Whether an inline member `function_definition` declares a **virtual** method: either the
/// `virtual` keyword — a `virtual` node that is a direct child of the definition
/// (`virtual int get(){…}`) — or a trailing `override`/`final` — a `virtual_specifier` node under
/// its `function_declarator` (`int get() override {…}`). Both mark the method for CHA dispatch
/// (an `override` of a virtual base method is itself virtual even without the keyword). `func_`
/// `declr` is the already-unwrapped `function_declarator`. Inert for C (neither node exists in a
/// C tree).
fn method_is_virtual_def(fdef: Node<'_>, func_declr: Node<'_>) -> bool {
    let mut fc = fdef.walk();
    if fdef.children(&mut fc).any(|ch| ch.kind() == "virtual") {
        return true;
    }
    let mut dc = func_declr.walk();
    func_declr
        .children(&mut dc)
        .any(|ch| ch.kind() == "virtual_specifier")
}

/// The namespace qualifier prefix (`ns::`, or nested `a::b::`) of every `namespace_`
/// `definition` enclosing `node`, outermost first, or `""` if `node` is at file scope. A
/// namespaced entity is lowered under its fully-qualified IR name (`ns::f`, `ns::Box`), so a
/// qualified reference at the use site (`ns::f(…)`, `ns::Box b;`) resolves to it. Walks the
/// ancestor chain for named `namespace_definition`s and joins their names with `::`. C trees
/// have no `namespace_definition`, so this is always `""` for C.
fn enclosing_namespace_prefix(node: Node<'_>, source: &str) -> String {
    let mut names: Vec<&str> = Vec::new();
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "namespace_definition"
            && let Some(name) = n.child_by_field_name("name")
        {
            names.push(to_str(&name, source));
        }
        cur = n.parent();
    }
    names.reverse();
    let mut prefix = String::new();
    for name in names {
        prefix.push_str(name);
        prefix.push_str("::");
    }
    prefix
}

/// The fully-qualified name of the `class_specifier`/`struct_specifier` enclosing `node`
/// (any namespace prefix + the class's simple name), or `None` if `node` is not inside a
/// class. Matches the `qualified_name` [`cpp_collect_methods`] registers a class and its
/// methods under, so an inline method's discovered overload base name (`Box::f`, `ns::Box::f`)
/// is byte-identical to the string the dispatch edge and phase-2 registration build.
fn enclosing_class_qualified_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if (n.kind() == "class_specifier" || n.kind() == "struct_specifier")
            && let Some(name) = n.child_by_field_name("name")
        {
            return Some(format!(
                "{}{}",
                enclosing_namespace_prefix(name, source),
                to_str(&name, source)
            ));
        }
        cur = n.parent();
    }
    None
}

/// The IR **base name** and explicit **arity** of a `function_definition`, for overload
/// discovery — or `None` if it is not a name this slice overloads (a destructor, or an
/// unrecognized declarator shape). It mirrors *exactly* the naming the registration/call
/// touchpoints use, so the recorded arity set keys the same string the mangler is later
/// asked about:
/// - a free function (`identifier` name) at file scope → the bare `f`; inside a namespace →
///   the qualified `ns::f`;
/// - an inline **constructor** (an `identifier`-named member equal to the enclosing class's
///   simple name, `Box(int){…}`) → the qualified ctor name `Class::Class` (`ns::Box::ns::Box`);
///   any other `identifier`-named member is not lowered, so it yields `None`;
/// - an inline **method** (`field_identifier` name) → `Class::method`, the class qualified by
///   any enclosing namespace;
/// - an **out-of-line** method (`qualified_identifier` name `Class::m`) → `Class::m`, including
///   an out-of-line **constructor** (`Box::Box`, name == scope) → `Box::Box`.
///
/// Constructors are **included** (spec 010): a class's ctors form an overload set keyed on
/// `Class::Class`, so ctors of ≥2 distinct arities each get an arity-mangled name and a
/// construction resolves to the matching one. A reference-returning definition (`Box& setV(…)`)
/// is unwrapped first, exactly as discovery does elsewhere. Every shape matched here is
/// C++-only, so this yields `None` for all of C.
fn overload_base_name(fdef: Node<'_>, source: &str) -> Option<(String, usize)> {
    let declr = fdef.child_by_field_name("declarator")?;
    let func_declr = unwrap_reference_declarator(declr);
    if func_declr.kind() != "function_declarator" {
        return None;
    }
    let params = func_declr.child_by_field_name("parameters")?;
    let arity = param_arity(params);
    let name_node = func_declr.child_by_field_name("declarator")?;
    match name_node.kind() {
        // A free function, or an inline constructor / identifier-named member.
        "identifier" => {
            if is_class_member_definition(name_node) {
                // An identifier-named class member is a **constructor** iff its name equals the
                // enclosing class's simple name (`Box(int){…}` inside `struct Box`); model it
                // under the qualified ctor name `Class::Class` (`ns::Box::ns::Box`) — the same
                // string ctor registration and the construction edge build. Any other
                // identifier-named member is not lowered, so it is not overloadable here.
                let class = enclosing_class_qualified_name(fdef, source)?;
                let simple = class.rsplit("::").next().unwrap_or(class.as_str());
                if to_str(&name_node, source) == simple {
                    return Some((format!("{class}::{class}"), arity));
                }
                return None;
            }
            let base = format!(
                "{}{}",
                enclosing_namespace_prefix(fdef, source),
                to_str(&name_node, source)
            );
            Some((base, arity))
        }
        // An inline method: qualify the enclosing class, then `Class::method`.
        "field_identifier" => {
            let class = enclosing_class_qualified_name(fdef, source)?;
            Some((format!("{class}::{}", to_str(&name_node, source)), arity))
        }
        // An out-of-line method `Class::m`, or an out-of-line constructor `Box::Box` (name ==
        // scope) — both keyed on `scope::name`, exactly as registration builds them.
        "qualified_identifier" => {
            let scope = name_node.child_by_field_name("scope")?;
            let name = name_node.child_by_field_name("name")?;
            let scope_s = to_str(&scope, source);
            let name_s = to_str(&name, source);
            Some((format!("{scope_s}::{name_s}"), arity))
        }
        _ => None,
    }
}

/// Discover **arity-overloaded** names — the C++ `GrammarHooks::collect_overloads`
/// implementation, run once before any function is registered. It scans every
/// `function_definition` (free, namespaced, inline-method, out-of-line-method), computes each
/// one's IR base name and explicit-parameter arity via [`overload_base_name`], and records the
/// arity into the neutral [`Context::overloads`] map. A base name that ends up with **≥2**
/// distinct arities is thereby marked overloaded, so [`Context::overload_name`] mangles it
/// (`id#1`/`id#2`, `Box::f#1`/`Box::f#2`) at every touchpoint; a single-arity name stays bare.
/// Constructors are **included** (spec 010): a class's ctors are keyed on `Class::Class`, so a
/// class with ctors of ≥2 arities gets `Box::Box#1`/`Box::Box#2` and a construction resolves to
/// the matching one; a single-ctor class keeps the bare `Class::Class` and is unchanged. Inert
/// for C: no C tree contains any of the shapes matched here, so nothing is recorded and the map
/// stays empty.
fn cpp_discover_overloads<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    root: Node<'_>,
) -> anyhow::Result<(), Error> {
    let query = ctx.compile_query(r#"(function_definition) @def"#);
    let mut pairs: Vec<(String, usize)> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(&query, root, source.as_bytes());
        while let Some(m) = it.next() {
            let extract = MatchExtractor::new(&query, m);
            if let Some(pair) = overload_base_name(extract.get("def")?, source) {
                pairs.push(pair);
            }
        }
    }
    for (base, arity) in pairs {
        ctx.overloads.entry(base).or_default().insert(arity);
    }
    Ok(())
}

/// Union each class's **transitive** base-class data members into its own `members`, so an
/// inherited member used inside a derived method resolves to `this.<member>` exactly as it does
/// in the base (the base subobject shares the derived object's field-named access paths). For
/// every registered class we walk its full base chain — reading each reachable base's *own*
/// members — and extend the derived class's `members` with them; walking the whole chain (not
/// just direct bases) makes the result transitive regardless of discovery order, and a `seen`
/// set guards against a malformed base cycle. A class with no bases (all of C, every base-less
/// C++ class) gains nothing, so the C path and existing C++ cases are unchanged.
fn flatten_inherited_members(ctx: &mut Context<'_>) {
    let class_names: Vec<String> = ctx.classes.keys().cloned().collect();
    for name in class_names {
        let mut inherited: Vec<String> = Vec::new();
        let mut stack: Vec<String> = ctx
            .classes
            .get(&name)
            .map(|info| info.bases.clone())
            .unwrap_or_default();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(base) = stack.pop() {
            if !seen.insert(base.clone()) {
                continue;
            }
            if let Some(binfo) = ctx.classes.get(&base) {
                inherited.extend(binfo.members.iter().cloned());
                stack.extend(binfo.bases.iter().cloned());
            }
        }
        if let Some(info) = ctx.classes.get_mut(&name) {
            info.members.extend(inherited);
        }
    }
}

/// Build the neutral **subclass** hierarchy — the reverse of each class's `bases` — into
/// [`Context::subclasses`], so a virtual call can walk a static type's subclass subtree and
/// gather every override (CHA). Run once after every class is discovered (so a base declared
/// after its derived class is still linked). For each class we register it as a direct subclass
/// of every class in its `bases`. Empty for C and for every base-less C++ class (no `bases`
/// edges), so it adds nothing there and the C path is unchanged.
fn build_subclasses(ctx: &mut Context<'_>) {
    let edges: Vec<(String, String)> = ctx
        .classes
        .iter()
        .flat_map(|(name, info)| {
            info.bases
                .iter()
                .map(move |base| (base.clone(), name.clone()))
        })
        .collect();
    for (base, sub) in edges {
        ctx.subclasses.entry(base).or_default().push(sub);
    }
}

/// Discover C++ instance methods and constructors — inline *and* out-of-line — and lower
/// them through the shared core.
///
/// The top-level `function_definition` query in `Context::collect_functions` only matches
/// definitions whose name is a plain `identifier`; a member function's name is either a
/// `field_identifier` nested inside a `class_specifier`/`struct_specifier` (inline body) or
/// a `qualified_identifier` `Class::m` at top level (out-of-line body) — both invisible to
/// it. A constructor is the special case whose name is the class name (`Box(int){…}` inline,
/// or `Box::Box(int){…}` out of line): it is modeled as the function `Class::Class` with the
/// same implicit `this`, plus any member-initializer list lowered before the body. This hook (installed only for C++) finds each class, records its data members and
/// method names into `Context::classes` (the neutral map the shared core consults for member
/// resolution and `recv.method(…)` dispatch), and lowers every method body via
/// `Context::lower_function` with an implicit `this` (`ByRef`) parameter. An out-of-line body
/// resolves its enclosing class from the qualifier and is otherwise identical to an inline
/// one (same implicit `this`, same `this.<member>` resolution).
///
/// Phases: gather each class's members/inline methods (phase 1); discover out-of-line
/// definitions for already-declared classes (phase 1.5); register a `FunctionIdx` for every
/// method, inline or out-of-line (phase 2a), so a method or a later top-level body can
/// resolve a call to it; then lower the bodies (phase 2b). Each gathering step finishes (and
/// drops its tree-query cursor) before any `lower_function` call, which needs `&mut Context`.
fn cpp_collect_methods<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    root: Node<'_>,
    program: &mut Program,
    global_sidx: usize,
) -> anyhow::Result<(), Error> {
    let query = ctx.compile_query(
        r#"
        [(struct_specifier name: (type_identifier) @class.name
            body: (field_declaration_list) @class.body) @class.node
         (class_specifier name: (type_identifier) @class.name
            body: (field_declaration_list) @class.body) @class.node]
        "#,
    );

    // A method body to lower in phase 2. Nodes are `Copy` and tied to the tree, not to
    // `ctx`, so they can be stashed across the `&mut ctx` lowering calls below.
    struct MethodDef<'t> {
        class: String,
        name: String,
        params: Node<'t>,
        body: Node<'t>,
        void: bool,
        /// Whether the method's result aliases the receiver object — a reference return type
        /// (`Class& m()`) whose body does `return *this`. Recorded in `ClassInfo.returns_self`
        /// so a chained `recv.m(…).n(…)` dispatches `n` on the same object.
        returns_self: bool,
        /// Whether this is a **`static`** member function — lowered *without* the implicit `this`
        /// (its parameters are exactly the declared ones), like a receiver-less free function
        /// keyed `Class::method` (spec 015). A static method is not an instance method, so it is
        /// not registered in [`ClassInfo::methods`] (an `obj.m()` dispatch must not resolve to it)
        /// and is never `returns_self`/virtual. `false` for every ordinary method and constructor.
        is_static: bool,
        /// A constructor's member-initializer list as `(member, init-expr)` pairs; empty
        /// for an ordinary method and for a constructor written without one.
        member_inits: Vec<(String, Node<'t>)>,
    }
    let mut methods: Vec<MethodDef<'_>> = Vec::new();

    // Phase 1: scan classes, populate `ctx.classes`, collect method bodies. The query
    // cursor borrows the tree (not `ctx`), so populating `ctx.classes` here is fine.
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, root, source.as_bytes());
    while let Some(m) = it.next() {
        let extract = MatchExtractor::new(&query, m);
        // A class in a named namespace is registered — and its methods/ctor keyed — under its
        // **qualified** name (`ns::Box`), so a qualified declaration `ns::Box b;` dispatches;
        // an inline constructor still names the class by its **simple** name (`Box(int){…}`),
        // so `simple_name` is what the ctor identifier is compared against. At file scope the
        // prefix is empty and the two coincide (no change to the non-namespaced path).
        let class_name_node = extract.get("class.name")?;
        let simple_name = to_str(&class_name_node, source);
        let qualified_name = format!(
            "{}{}",
            enclosing_namespace_prefix(class_name_node, source),
            simple_name
        );
        let body = extract.get("class.body")?;

        // Record direct base classes from the `base_class_clause` (`: Base` / `: public Base`),
        // an unnamed child of the class specifier between the name and the body. Its base name
        // is a `type_identifier` (optionally preceded by an `access_specifier`, which taint
        // ignores; a `virtual` keyword or a namespaced `qualified_identifier` base are later
        // milestones, so we simply take each `type_identifier`). `bases` stays empty for a
        // base-less class. C has no `base_class_clause`, so this is inert for C.
        let mut info = ClassInfo {
            bases: base_class_names(extract.get("class.node")?, source),
            ..Default::default()
        };
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            match child.kind() {
                "field_declaration" => {
                    // A `static` data member (`static int total;`) is a class-scoped **global**,
                    // not a per-object field: record it in `static_members` too so an unqualified
                    // reference inside a method resolves to the global `Class::<member>` rather
                    // than `this.<member>` (spec 015). A plain data member is not static.
                    let is_static = has_static_specifier(child, source);
                    let mut dc = child.walk();
                    for declr in child.children_by_field_name("declarator", &mut dc) {
                        if let Some(name) = member_name(declr, source) {
                            info.members.insert(name.to_string());
                            if is_static {
                                info.static_members.insert(name.to_string());
                            }
                        }
                    }
                }
                "function_definition" => {
                    // A member function: declarator is a `function_declarator` whose own
                    // declarator is either the method-name `field_identifier` (an ordinary
                    // method) or a plain `identifier` equal to the class name (an inline
                    // **constructor** — `Box(int x){…}`, modeled as `Class::Class`). A
                    // destructor's `destructor_name` declarator matches neither and is
                    // ingested-but-not-lowered (its definition still parses). Out-of-line
                    // defs are handled in phase 1.5.
                    let Some(declr) = child.child_by_field_name("declarator") else {
                        continue;
                    };
                    // A reference return type (`Class& m(…)`, `int& get(…)`) nests the
                    // `function_declarator` inside a `reference_declarator`; unwrap it so the
                    // method is discovered (otherwise it is silently skipped).
                    let is_reference_return = declr.kind() == "reference_declarator";
                    let func_declr = unwrap_reference_declarator(declr);
                    if func_declr.kind() != "function_declarator" {
                        continue;
                    }
                    let (Some(name_node), Some(params), Some(method_body)) = (
                        func_declr.child_by_field_name("declarator"),
                        func_declr.child_by_field_name("parameters"),
                        child.child_by_field_name("body"),
                    ) else {
                        continue;
                    };
                    let is_ctor = name_node.kind() == "identifier"
                        && to_str(&name_node, source) == simple_name;
                    // A destructor `~Class(){…}` names a `destructor_name` declarator; it is
                    // lowered as the niladic `this`-method `Class::~Class` (spec 016). It is
                    // neither a `field_identifier` method nor the class-named ctor `identifier`.
                    let is_dtor = name_node.kind() == "destructor_name";
                    if name_node.kind() != "field_identifier" && !is_ctor && !is_dtor {
                        continue;
                    }
                    // A constructor's method name is the class's **qualified** name, so its IR
                    // name is `{qualified}::{qualified}` (`ns::Box::ns::Box`) — the same key the
                    // `construct` hook builds via `format!("{class}::{class}")`. A destructor's
                    // method-name portion is `~{simple}`, so its IR name is `{qualified}::~{simple}`
                    // (`Box::~Box`) — the same key the `delete_expr` hook builds. An ordinary
                    // method keeps its simple name (`set`), qualified to `ns::Box::set`.
                    let name = if is_ctor {
                        qualified_name.clone()
                    } else if is_dtor {
                        format!("~{simple_name}")
                    } else {
                        to_str(&name_node, source).to_string()
                    };
                    let void = child
                        .child_by_field_name("type")
                        .is_some_and(|t| to_str(&t, source).eq_ignore_ascii_case("void"));
                    if is_dtor {
                        // A destructor returns nothing and writes/reads the object via `this`;
                        // model it as a void niladic `Class::~Class` method (an implicit `this`
                        // by-ref param 0 and no other params). Record that the class declares its
                        // own destructor, and — for a `virtual ~Class()` — that it is virtual, so
                        // a `delete` through a base pointer dispatches to the subtree's
                        // destructors (CHA, spec 016). A destructor is never overloaded, static,
                        // or `returns_self`.
                        info.has_dtor = true;
                        if method_is_virtual_def(child, func_declr) {
                            info.dtor_virtual = true;
                        }
                        methods.push(MethodDef {
                            class: qualified_name.clone(),
                            name,
                            params,
                            body: method_body,
                            void: true,
                            returns_self: false,
                            is_static: false,
                            member_inits: Vec::new(),
                        });
                    } else if is_ctor {
                        // A constructor returns no value and writes the object via `this`;
                        // model it as a void `Class::Class` method and record the member-
                        // initializer list (`: v(x)`) so construction can lower it.
                        info.has_ctor = true;
                        methods.push(MethodDef {
                            class: qualified_name.clone(),
                            name,
                            params,
                            body: method_body,
                            void: true,
                            returns_self: false,
                            is_static: false,
                            member_inits: ctor_member_inits(child, source),
                        });
                    } else {
                        // A `static` member function has no implicit `this`: it is a receiver-less
                        // function keyed `Class::method`, called `Class::method(args)` (spec 015).
                        // It is therefore NOT an instance method — it is not registered in
                        // `info.methods` (an `obj.m()` dispatch, out of scope, must not resolve to
                        // it) and is never `returns_self`/virtual. An ordinary (non-static) method
                        // registers as before.
                        let is_static = has_static_specifier(child, source);
                        // A reference-returning method that returns `*this` (a fluent setter)
                        // has a result aliasing the receiver object — never a static method.
                        let returns_self = !is_static
                            && is_reference_return
                            && body_returns_deref_this(method_body);
                        if !is_static {
                            info.methods.insert(name.clone());
                            if returns_self {
                                info.returns_self.insert(name.clone());
                            }
                            // A `virtual`/`override` method is recorded for CHA dispatch: a call to
                            // it through a base pointer/reference resolves to every override in the
                            // static type's subclass subtree, not just the static type's method.
                            if method_is_virtual_def(child, func_declr) {
                                info.virtual_methods.insert(name.clone());
                            }
                        }
                        methods.push(MethodDef {
                            class: qualified_name.clone(),
                            name,
                            params,
                            body: method_body,
                            void,
                            returns_self,
                            is_static,
                            member_inits: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        }
        ctx.classes.insert(qualified_name, info);
    }
    drop(it);
    drop(cursor);

    // Phase 1b: flatten inherited data members. A derived class's `members` becomes the union
    // of its own and its bases' members (transitively), so an inherited member used inside a
    // derived method resolves to `this.<member>` exactly as it does in the base — the base
    // subobject shares the derived object's field-named access paths. Walking the full base
    // chain here (not just direct bases) makes it transitive even though a base `ClassInfo` may
    // still hold only its own members. `bases` is empty for every C and base-less class, so this
    // adds nothing there. Done before phase 2b lowers the bodies, which read `members`.
    flatten_inherited_members(ctx);

    // Build the subclass hierarchy (reverse of `bases`) now that every class is known, so a
    // virtual call lowered in phase 2b — or in a later top-level body — can walk a static type's
    // subclass subtree to gather every override (CHA). Empty for C and base-less C++ classes.
    build_subclasses(ctx);

    // Phase 1.5: discover out-of-line method definitions — a top-level `function_definition`
    // whose declarator names a `qualified_identifier` (`ret Class::m(params){…}`). The
    // top-level `function_definition` query in `Context::collect_functions` only matches a
    // plain `identifier` name, so these are invisible to it and must be found here. We gather
    // (class, method, params, body) first — the cursor borrows the tree, not `ctx`, so reading
    // `ctx.classes` to filter to already-declared classes is fine — then register the method
    // names and queue the bodies after the cursor drops.
    // Match both a plain out-of-line declarator and a reference-returning one
    // (`Box& Box::setV(…)`), whose `function_declarator` is wrapped in a `reference_declarator`.
    let ool_query = ctx.compile_query(
        r#"
        (function_definition
            declarator: [
                (function_declarator
                    declarator: (qualified_identifier
                        scope: (namespace_identifier) @class
                        name: (identifier) @method)
                    parameters: (parameter_list) @params)
                (reference_declarator
                    (function_declarator
                        declarator: (qualified_identifier
                            scope: (namespace_identifier) @class
                            name: (identifier) @method)
                        parameters: (parameter_list) @params))
            ]
            body: (compound_statement) @body) @def
        "#,
    );
    let mut out_of_line: Vec<(bool, MethodDef<'_>)> = Vec::new();
    {
        let mut ool_cursor = QueryCursor::new();
        let mut ool_it = ool_cursor.matches(&ool_query, root, source.as_bytes());
        while let Some(m) = ool_it.next() {
            let extract = MatchExtractor::new(&ool_query, m);
            let class = to_str(&extract.get("class")?, source).to_string();
            // Only lower out-of-line bodies for a class known from its declaration (this
            // slice's scope). An unknown qualifier (e.g. a namespaced free function) is left
            // alone — it is not an instance method we can model here.
            if !ctx.classes.contains_key(&class) {
                continue;
            }
            let name = to_str(&extract.get("method")?, source).to_string();
            let def = extract.get("def")?;
            let params = extract.get("params")?;
            let body = extract.get("body")?;
            // `Box::Box(…)` (name == class) is an out-of-line **constructor**: model it as a
            // void `Class::Class` and carry its member-initializer list. An ordinary
            // out-of-line method keeps its declared return arity and has no init list.
            let is_ctor = name == class;
            let void = is_ctor
                || def
                    .child_by_field_name("type")
                    .is_some_and(|t| to_str(&t, source).eq_ignore_ascii_case("void"));
            // A reference-returning out-of-line definition (`Box& Box::setV(…)`) has its
            // `function_declarator` wrapped in a `reference_declarator`; if its body returns
            // `*this` the result aliases the receiver object (a fluent setter).
            let returns_self = !is_ctor
                && def
                    .child_by_field_name("declarator")
                    .is_some_and(|d| d.kind() == "reference_declarator")
                && body_returns_deref_this(body);
            let member_inits = if is_ctor {
                ctor_member_inits(def, source)
            } else {
                Vec::new()
            };
            out_of_line.push((
                is_ctor,
                MethodDef {
                    class,
                    name,
                    params,
                    body,
                    void,
                    returns_self,
                    // Out-of-line static methods are out of this slice's scope (the `static`
                    // keyword appears only on the in-class declaration, not the out-of-line
                    // definition); an out-of-line definition here is always an instance method.
                    is_static: false,
                    member_inits,
                },
            ));
        }
    }
    // Register each out-of-line method's name on its class (so `recv.m(…)` dispatches even
    // though the body lives outside the class body), or mark the class as having a
    // constructor; then queue it for lowering alongside the inline methods.
    for (is_ctor, md) in out_of_line {
        if let Some(info) = ctx.classes.get_mut(&md.class) {
            if is_ctor {
                info.has_ctor = true;
            } else {
                info.methods.insert(md.name.clone());
                if md.returns_self {
                    info.returns_self.insert(md.name.clone());
                }
            }
        }
        methods.push(md);
    }

    // Phase 2a: register a function index for every method, so a method calling another
    // method (or a top-level body calling one) resolves it regardless of definition order.
    // An **overloaded** method or constructor (`Box::f#1`/`Box::f#2`, `Box::Box#1`/`Box::Box#2`)
    // reserves a distinct `FunctionIdx` per arity via the neutral mangler; a non-overloaded
    // method and a single-arity constructor stay bare — the same `Class::method` / `Class::Class`
    // string a `recv.m(…)` dispatch and the `construct` hook build. (Spec 010 lifted the
    // constructor exclusion, so a class with ctors of ≥2 arities mangles them here too.)
    for md in &methods {
        let base = format!("{}::{}", md.class, md.name);
        let qualified = ctx.overload_name(&base, param_arity(md.params));
        ctx.functions
            .entry(qualified)
            .or_insert_with(|| program.new_function());
    }
    // Phase 2b: lower each method body under its class context. An **instance** method or
    // constructor installs an implicit `this` of its class (`has_implicit_this = true`), so member
    // writes flow back through the by-ref receiver; a constructor additionally emits its
    // member-initializer list before the body. A **static** method (`is_static`) has the same
    // class context — so an unqualified `static` data member resolves to its class-scoped global —
    // but **no** `this` (spec 015), so its declared params number from 0 like a free function.
    for md in methods {
        let base = format!("{}::{}", md.class, md.name);
        let qualified = ctx.overload_name(&base, param_arity(md.params));
        ctx.lower_function(
            source,
            program,
            global_sidx,
            &qualified,
            md.void,
            md.params,
            md.body,
            Some(&md.class),
            !md.is_static,
            &md.member_inits,
        )?;
    }

    cpp_collect_namespaced_functions(ctx, source, root, program, global_sidx)?;
    Ok(())
}

/// Discover **namespaced free functions** — a `function_definition` that is a direct child
/// of a named namespace's `declaration_list` — and lower each under its fully-qualified IR
/// name (`ns::f`), so a qualified call `ns::f(args)` resolves to it. The direct-child
/// constraint excludes methods (those live inside a class's `field_declaration_list`, not the
/// namespace's `declaration_list`). The shared `collect_functions` skips these (they are
/// nested in a `namespace_definition`), so they are not also registered under their bare name.
///
/// A namespaced free function is an ordinary free function (no implicit `this`), so it is
/// lowered with `implicit_this = None` and no member-initializer list — only its **name** is
/// qualified. Inert for C (no `namespace_definition`).
fn cpp_collect_namespaced_functions<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    root: Node<'_>,
    program: &mut Program,
    global_sidx: usize,
) -> anyhow::Result<(), Error> {
    let query = ctx.compile_query(
        r#"
        (namespace_definition
            body: (declaration_list
                (function_definition
                    declarator: (function_declarator
                        declarator: (identifier) @func.name
                        parameters: (parameter_list) @params)
                    body: (compound_statement) @body) @def))
        "#,
    );

    struct NsFn<'t> {
        qualified: String,
        params: Node<'t>,
        body: Node<'t>,
        void: bool,
    }
    let mut ns_fns: Vec<NsFn<'_>> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(&query, root, source.as_bytes());
        while let Some(m) = it.next() {
            let extract = MatchExtractor::new(&query, m);
            let def = extract.get("def")?;
            let fname = to_str(&extract.get("func.name")?, source);
            let params = extract.get("params")?;
            // Qualified name (`ns::f`), then arity-mangled if this namespaced free function is
            // itself overloaded (`ns::f#1`/`ns::f#2`) — matching the call-site edge. A
            // non-overloaded namespaced function stays bare-qualified (identity mangler), so
            // the existing namespace cases are unchanged.
            let base = format!("{}{}", enclosing_namespace_prefix(def, source), fname);
            let qualified = ctx.overload_name(&base, param_arity(params));
            let void = def
                .child_by_field_name("type")
                .is_some_and(|t| to_str(&t, source).eq_ignore_ascii_case("void"));
            ns_fns.push(NsFn {
                qualified,
                params,
                body: extract.get("body")?,
                void,
            });
        }
    }

    // Register every qualified name first (so an inter-function reference resolves regardless
    // of order), then lower each body as an ordinary free function keyed on the qualified name.
    for f in &ns_fns {
        ctx.functions
            .entry(f.qualified.clone())
            .or_insert_with(|| program.new_function());
    }
    for f in ns_fns {
        ctx.lower_function(
            source,
            program,
            global_sidx,
            &f.qualified,
            f.void,
            f.params,
            f.body,
            None,
            false,
            &[],
        )?;
    }
    Ok(())
}

/// Extract a constructor's member-initializer list from its `function_definition` node as
/// `(member, init-expression)` pairs. The list (`: v(x), w(y)`) appears as an unnamed
/// `field_initializer_list` child between the declarator and the body; each
/// `field_initializer` names a `field_identifier` member followed by an `argument_list`
/// (`v(x)`) or `initializer_list` (`v{x}`) holding the initializer. We take the first
/// initializer expression for each member (scalar members, this slice). A base-class
/// initializer (`: Base(x)`, inheritance — out of scope) names a type, not a
/// `field_identifier`, and is skipped. Returns empty when there is no init list.
fn ctor_member_inits<'t>(fdef: Node<'t>, source: &str) -> Vec<(String, Node<'t>)> {
    let mut inits = Vec::new();
    let mut fc = fdef.walk();
    for child in fdef.children(&mut fc) {
        if child.kind() != "field_initializer_list" {
            continue;
        }
        let mut ic = child.walk();
        for fi in child.children(&mut ic) {
            if fi.kind() != "field_initializer" {
                continue;
            }
            let Some(member) = fi.named_child(0).filter(|n| n.kind() == "field_identifier") else {
                continue;
            };
            if let Some(expr) = fi.named_child(1).and_then(|args| args.named_child(0)) {
                inits.push((to_str(&member, source).to_string(), expr));
            }
        }
    }
    inits
}

/// Try to lower a class-typed declaration as an **object construction** — the C++
/// `GrammarHooks::construct` implementation. The shared [`Context::walk_declaration`] only
/// calls this when the declaration's type names a known class (the `classes` map is empty
/// for C, so the C path never reaches here). Recognized forms, all requiring the class to
/// declare a constructor (`has_ctor`):
/// - **direct** `Box b(args)` — with **all** arguments type-like (`Box b(source())`),
///   tree-sitter parses this as a function declaration (the "most vexing parse"): a
///   `function_declarator` whose declarator is the object `identifier` and whose `parameters`
///   hold the arguments shredded into `parameter_declaration`s, which [`reconstruct_mvp_arg`]
///   turns back into argument expressions. When an argument is **not** type-like (e.g. a
///   literal, `Box b(0, source())`) the MVP does not apply and the object is instead an
///   `init_declarator` whose `value` is the constructor `argument_list`;
/// - **copy** `Box b = Box(args)` — an `init_declarator` whose value is a `call_expression`
///   on the class name;
/// - **brace** `Box b{args}` — an `init_declarator` whose value is an `initializer_list`.
///
/// Each lowers to: register the local object, then `DirectCall Class::Class` with the
/// object as the arg-0 (`ByRef`) receiver and the constructor arguments following — so the
/// constructor's `this.<member>` writes propagate back into the object (exactly like a
/// setter call). Returns `true` when it recognized and lowered a construction, `false`
/// otherwise (e.g. `Box b;`, or a class without a constructor) so the caller handles the
/// declaration normally.
fn cpp_try_construct<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &mut ScopeView,
    decl_node: Node<'_>,
    class: &str,
) -> anyhow::Result<bool, Error> {
    // Heap allocation `Type* p = new AllocType(args)`. Checked before the `has_ctor` gate: the
    // *declared* pointer type (`Base`) may declare no constructor while the *allocated* type
    // (`Derived`) does, and even a ctor-less `new` must still bind the pointer to a heap object.
    if cpp_try_new(ctx, source, program, scope_view, decl_node, class)? {
        return Ok(true);
    }
    if !ctx.classes.get(class).is_some_and(|info| info.has_ctor) {
        return Ok(false);
    }
    let Some(declr) = decl_node.child_by_field_name("declarator") else {
        return Ok(false);
    };

    // Recover the object's name and the constructor argument nodes, by form.
    let (obj_node, args): (Node<'_>, ThinVec<Exp>) = match declr.kind() {
        // Most-vexing-parse direct init: `Box b(args)`.
        "function_declarator" => {
            let Some(obj_node) = declr
                .child_by_field_name("declarator")
                .filter(|d| d.kind() == "identifier")
            else {
                return Ok(false);
            };
            let Some(params) = declr.child_by_field_name("parameters") else {
                return Ok(false);
            };
            let mut pcursor = params.walk();
            let pds: Vec<Node<'_>> = params
                .children(&mut pcursor)
                .filter(|c| c.kind() == "parameter_declaration")
                .collect();
            if pds.is_empty() {
                // `Box b()` is a function prototype, not a construction (no default-ctor
                // call in this slice); let the normal declarator handling deal with it.
                return Ok(false);
            }
            let mut args: ThinVec<Exp> = ThinVec::new();
            for pd in pds {
                args.push(reconstruct_mvp_arg(ctx, source, program, scope_view, pd)?);
            }
            (obj_node, args)
        }
        // Copy/brace init: `Box b = Box(args)` / `Box b{args}`.
        "init_declarator" => {
            let Some(obj_node) = declr
                .child_by_field_name("declarator")
                .filter(|d| d.kind() == "identifier")
            else {
                return Ok(false);
            };
            let Some(value) = declr.child_by_field_name("value") else {
                return Ok(false);
            };
            match value.kind() {
                "call_expression" => {
                    // Only a call on the class name itself is construction (`Box(args)`).
                    let on_class = value
                        .child_by_field_name("function")
                        .filter(|f| f.kind() == "identifier")
                        .is_some_and(|f| to_str(&f, source) == class);
                    if !on_class {
                        return Ok(false);
                    }
                    let arg_list = value
                        .child_by_field_name("arguments")
                        .expect("call_expression always has arguments");
                    let args = ctx.collect_arguments(program, arg_list, source, scope_view)?;
                    (obj_node, args)
                }
                "initializer_list" => {
                    let mut lcursor = value.walk();
                    let mut args: ThinVec<Exp> = ThinVec::new();
                    for elem in value.children(&mut lcursor) {
                        if elem.is_named() {
                            args.push(ctx.flatten_expr(program, elem, source, scope_view)?);
                        }
                    }
                    (obj_node, args)
                }
                // Direct init `Box b(a, b)` whose arguments are **not** all type-like — e.g.
                // one is a literal (`Box b(0, source())`). Because a literal cannot be a
                // parameter declaration, tree-sitter does **not** most-vexing-parse this as a
                // function declaration: the object is an `init_declarator` whose `value` is the
                // constructor `argument_list`. (The all-type-like direct form `Box b(source())`
                // is MVP-mangled and handled by the `function_declarator` arm above.) Collect
                // the arguments exactly as a call's argument list.
                "argument_list" => {
                    let args = ctx.collect_arguments(program, value, source, scope_view)?;
                    (obj_node, args)
                }
                _ => return Ok(false),
            }
        }
        _ => return Ok(false),
    };

    emit_construction(ctx, source, program, scope_view, class, obj_node, args);
    Ok(true)
}

/// Reconstruct one most-vexing-parse constructor argument from the `parameter_declaration`
/// tree-sitter produced for it. `Box b(source())` parses the argument `source()` as a
/// parameter of type `source` with an `abstract_function_declarator` (`()`); `Box b(y)`
/// parses `y` as a parameter of type `y` with no declarator. We map the former to a call
/// `source(…)` (recursing on any nested arguments) and the latter to a reference to the
/// named value. Anything else falls back to a reference to the type name.
fn reconstruct_mvp_arg<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &ScopeView,
    pd: Node<'_>,
) -> anyhow::Result<Exp, Error> {
    let name = pd
        .child_by_field_name("type")
        .map(|t| to_str(&t, source))
        .ok_or_else(|| {
            Error::TreeSitterParse("constructor argument parameter_declaration has no type".into())
        })?;
    match pd.child_by_field_name("declarator") {
        Some(d) if d.kind() == "abstract_function_declarator" => {
            // The argument is a call `name(nested args)`.
            let mut call_args: ThinVec<Exp> = ThinVec::new();
            if let Some(nested) = d.child_by_field_name("parameters") {
                let mut ncursor = nested.walk();
                let inner: Vec<Node<'_>> = nested
                    .children(&mut ncursor)
                    .filter(|c| c.kind() == "parameter_declaration")
                    .collect();
                for pd in inner {
                    call_args.push(reconstruct_mvp_arg(ctx, source, program, scope_view, pd)?);
                }
            }
            let temp = ctx.allocator.next_temp();
            let ret =
                VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp));
            program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
                StatementKind::CallAssign {
                    style: CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(thin_vec![name.to_string()]),
                    },
                    rets: thin_vec![ret],
                    args: call_args,
                },
            ));
            let ap = ctx.build_access_path(
                &temp,
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            );
            Ok(Exp::access_path(ctx.emit_loads(program, scope_view, ap)))
        }
        // A bare value reference (`y`), or a fallback.
        _ => {
            let ap = ctx.build_access_path(
                name,
                Default::default(),
                scope_view,
                &mut program[scope_view.fidx].locals,
            );
            Ok(Exp::access_path(ctx.emit_loads(program, scope_view, ap)))
        }
    }
}

/// Register the constructed local and emit the `DirectCall Class::Class(&obj, args…)`. The
/// object is registered in `local_types` so a later `obj.method(…)` dispatches, and passed
/// as the plain arg-0 access path (by-ref semantics come from the constructor's param 0
/// being `ByRef`, exactly as for an ordinary method receiver). The void constructor's
/// result goes to a throwaway temp, as method-call lowering already does.
///
/// The ctor edge is arity-resolved (spec 010): the callee name is
/// `overload_name("Class::Class", <explicit-arg-count>)`, so a class with ctors of ≥2 arities
/// resolves the construction to the ctor with exactly that many parameters (`Box::Box#1` vs
/// `Box::Box#2`); a single-ctor class keeps the bare `Class::Class` (identity mangler), so
/// every existing construction is byte-for-byte unchanged.
fn emit_construction<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &ScopeView,
    class: &str,
    obj_node: Node<'_>,
    ctor_args: ThinVec<Exp>,
) {
    let obj_name = to_str(&obj_node, source);
    ctx.scope_tree.add_variable(
        scope_view.sidx,
        obj_name.to_string(),
        VarKind::Local,
        None,
        None,
    );
    ctx.local_types
        .insert(obj_name.to_string(), class.to_string());
    // Record this constructed stack object in the enclosing scope's destructor frame so its
    // destructor runs at the scope's closing `}` (spec 017). Heap `new` objects go through
    // [`emit_ctor_call`] directly (not here), so they are not recorded and get no scope-exit
    // destructor. The frame is empty for C (no class objects), so this never fires on the C path.
    if let Some(frame) = ctx.dtor_frames.last_mut() {
        frame.push((obj_name.to_string(), class.to_string()));
    }
    emit_ctor_call(ctx, program, scope_view, class, obj_name, ctor_args);
}

/// Emit the constructor `DirectCall Class::Class#<arity>(&obj, args…)` for an **already-
/// registered** local `obj_name`. Its access path is the arg-0 (`ByRef`) receiver, so the
/// constructor's `this.<member>` writes land in the object; the void result goes to a
/// throwaway temp. The callee name is arity-resolved via the neutral mangler (spec 010), so a
/// class with ctors of ≥2 arities resolves to the matching `Box::Box#1`/`Box::Box#2` and a
/// single-ctor class keeps the bare `Class::Class`. Shared by stack construction
/// ([`emit_construction`]) and heap `new` ([`lower_new_expression`]).
fn emit_ctor_call(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    class: &str,
    obj_name: &str,
    ctor_args: ThinVec<Exp>,
) {
    // The object under construction is the by-ref arg 0, so resolve it to its location. A
    // plain local object is pathless (no load emitted); a global one must be loaded, which is
    // the only way the offset-only IR can name it.
    let recv_ap = ctx.build_access_path(
        obj_name,
        Default::default(),
        scope_view,
        &mut program[scope_view.fidx].locals,
    );
    let recv = Exp::access_path(ctx.emit_loads(program, scope_view, recv_ap));
    let arity = ctor_args.len();
    let mut args: ThinVec<Exp> = thin_vec![recv];
    args.extend(ctor_args);

    let qualified = ctx.overload_name(&format!("{class}::{class}"), arity);
    let temp = ctx.allocator.next_temp();
    let ret = VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp));
    program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
        StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(thin_vec![qualified]),
            },
            rets: thin_vec![ret],
            args,
        },
    ));
}

/// Lower a C++ `new_expression` (`new Type(args)`) to a **synthetic heap object** and return
/// its access path. Allocates a fresh local `O`, registers `O`'s type in `local_types` when
/// `Type` is a known class, and — if `Type` declares a constructor — runs it via
/// [`emit_ctor_call`] (`Type::Type#<arity>(&O, args…)`), reusing the exact construction edge
/// stack construction uses (specs 006/010). The caller binds the pointer local to `O` (an
/// alias, like a reference — [`cpp_try_new`]), so a later `p->m(…)` operates on `O`'s storage.
///
/// A heap object is, for taint, an anonymous constructed object the pointer aliases — the same
/// field-named member access paths a stack object has — so no new taint analysis is needed.
/// Array-`new` (`new T[n]`, a `declarator` field) and placement-`new` (a `placement` argument
/// list) are out of this slice's scope and yield `None`, leaving the caller to fall through.
/// `new_expression` never occurs under the C grammar, so this is unreachable for C.
fn lower_new_expression<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &mut ScopeView,
    new_expr: Node<'_>,
) -> anyhow::Result<Option<super::RawPath>, Error> {
    // Array-new / placement-new are out of scope; leave them unrecognized.
    if new_expr.child_by_field_name("declarator").is_some()
        || new_expr.child_by_field_name("placement").is_some()
    {
        return Ok(None);
    }
    let Some(type_node) = new_expr.child_by_field_name("type") else {
        return Ok(None);
    };
    let alloc_type = to_str(&type_node, source);

    // The constructor arguments: a parenthesized `argument_list` (`new Box(source())`) or a
    // braced `initializer_list` (`new Box{source()}`); absent for `new Box`/`new Box()`.
    let ctor_args: ThinVec<Exp> = match new_expr.child_by_field_name("arguments") {
        Some(args) if args.kind() == "argument_list" => {
            ctx.collect_arguments(program, args, source, scope_view)?
        }
        Some(args) if args.kind() == "initializer_list" => {
            let mut cursor = args.walk();
            let mut collected: ThinVec<Exp> = ThinVec::new();
            for elem in args.children(&mut cursor) {
                if elem.is_named() {
                    collected.push(ctx.flatten_expr(program, elem, source, scope_view)?);
                }
            }
            collected
        }
        _ => ThinVec::new(),
    };

    // Allocate the synthetic object as a fresh local, recording its (allocated) class so a
    // direct dispatch on it would resolve; the pointer that aliases it carries the *declared*
    // static type for CHA (set by the caller).
    let obj = ctx.allocator.next_temp();
    ctx.scope_tree
        .add_variable(scope_view.sidx, obj.clone(), VarKind::Local, None, None);
    if ctx.classes.contains_key(alloc_type) {
        ctx.local_types.insert(obj.clone(), alloc_type.to_string());
    }
    // Run the constructor iff the allocated type declares one (reuse the ctor edge). A
    // ctor-less allocation (`new Box()` with no user ctor) just yields the fresh object.
    if ctx
        .classes
        .get(alloc_type)
        .is_some_and(|info| info.has_ctor)
    {
        emit_ctor_call(ctx, program, scope_view, alloc_type, &obj, ctor_args);
    }
    // Returned as a `RawPath` (not lowered to loads): the caller binds the pointer local to
    // this object as an *alias*, so it must keep naming the object's storage.
    Ok(Some(ctx.build_access_path(
        &obj,
        Default::default(),
        scope_view,
        &mut program[scope_view.fidx].locals,
    )))
}

/// Recognize and lower a heap-object declaration `Type* p = new AllocType(args)` — the `new`
/// arm of the C++ `construct` hook. Lowers the `new_expression` to a synthetic heap object
/// ([`lower_new_expression`]) and binds the pointer local `p` as an **alias** of that object
/// (via [`Context::reference_aliases`], exactly as a reference binds — so every use of `p`,
/// receiver or read, resolves to the object's storage), recording `p`'s **declared** static
/// type (`Base` in `Base* p = new Derived()`) in `local_types` so a later virtual `p->m(…)`
/// dispatches by CHA over that static type's subtree (spec 012).
///
/// Returns `true` when it handled a `new`-declaration, `false` otherwise (so the caller
/// proceeds to ordinary construction / declarator handling). Only the pointer-local form in
/// this slice's scope is recognized; anything else falls through. `new_expression` never
/// occurs under the C grammar, so this never fires for C.
fn cpp_try_new<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &mut ScopeView,
    decl_node: Node<'_>,
    declared_class: &str,
) -> anyhow::Result<bool, Error> {
    let Some(declr) = decl_node.child_by_field_name("declarator") else {
        return Ok(false);
    };
    // A `new`-initialized pointer is an `init_declarator` whose `value` is a `new_expression`.
    if declr.kind() != "init_declarator" {
        return Ok(false);
    }
    let Some(value) = declr.child_by_field_name("value") else {
        return Ok(false);
    };
    if value.kind() != "new_expression" {
        return Ok(false);
    }
    // The pointer's leaf identifier (`p` in `Base* p` — its declarator is a `pointer_declarator`
    // wrapping the identifier).
    let Some(ptr_name) = declr
        .child_by_field_name("declarator")
        .and_then(|d| declarator_leaf_ident(d, source))
    else {
        return Ok(false);
    };

    let Some(obj_path) = lower_new_expression(ctx, source, program, scope_view, value)? else {
        return Ok(false);
    };
    // Bind the pointer as an alias of the heap object and record its declared static type.
    ctx.reference_aliases.insert(ptr_name.to_string(), obj_path);
    ctx.local_types
        .insert(ptr_name.to_string(), declared_class.to_string());
    Ok(true)
}

/// The simple (unqualified) name of a possibly-namespaced class key — the last `::`-separated
/// component (`ns::Box` → `Box`, `Box` → `Box`). A class's destructor IR name is
/// `{class}::~{simple}`, so this recovers the `~{simple}` suffix for a target edge.
fn simple_class_name(class: &str) -> &str {
    class.rsplit("::").next().unwrap_or(class)
}

/// Walk `static_class` then its base chain (transitively) to the nearest class that declares its
/// **own** destructor ([`ClassInfo::has_dtor`]), returning that class's name — the destructor the
/// static type statically binds (the "defining class" for a `delete`). `None` if no class in the
/// chain declares a destructor (a destructor-less hierarchy: `delete` stays 014's no-op). Mirrors
/// [`Context::resolve_method_class`]'s base-chain walk (own class first, then ancestors), with a
/// `seen` guard against a malformed base cycle. Empty `bases`/`has_dtor` for C and base-less
/// classes, so this never fires on the C path.
fn dtor_defining_class(ctx: &Context<'_>, static_class: &str) -> Option<String> {
    let mut level = vec![static_class.to_string()];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(c) = level.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        if let Some(info) = ctx.classes.get(&c) {
            if info.has_dtor {
                return Some(c);
            }
            level.extend(info.bases.iter().cloned());
        }
    }
    None
}

/// Whether the destructor is **virtual** on static type `static_class` — declared `virtual` on
/// `static_class` itself or on any class in its base chain (a virtual base destructor makes every
/// derived destructor virtual too). A virtual destructor makes `delete p` dispatch by
/// class-hierarchy analysis over the subtree; a non-virtual one stays single-target. Mirrors
/// [`Context::method_is_virtual`]. `false` for C and every non-virtual destructor.
fn dtor_is_virtual(ctx: &Context<'_>, static_class: &str) -> bool {
    let mut level = vec![static_class.to_string()];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(c) = level.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        if let Some(info) = ctx.classes.get(&c) {
            if info.dtor_virtual {
                return true;
            }
            level.extend(info.bases.iter().cloned());
        }
    }
    false
}

/// The destructor-call target set for `delete p` on static type `static_class`, given the
/// `defining` class whose destructor the static type binds (from [`dtor_defining_class`]): the
/// static-type destructor `defining::~{defining}` **plus** `sub::~{sub}` for every transitive
/// subclass `sub` of `static_class` that declares its **own** destructor. A sound superset of the
/// single destructor chain the dynamic type actually runs (Principle I) — whichever `Derived` the
/// heap object is, its `~Derived` is in the set, and the base `~Base` is the `defining` target.
/// The per-class destructor naming (`Sub::~Sub`, each named after its own class) is why this
/// mirrors — rather than reuses — [`Context::cha_targets`] (which shares one method name across the
/// subtree). Duplicate-free and order-stable; the subtree is walked over the neutral
/// [`Context::subclasses`] map (empty for C, so this reduces to the single `defining` target). A
/// `seen` guard bounds a malformed hierarchy.
fn dtor_cha_targets(ctx: &Context<'_>, static_class: &str, defining: &str) -> Vec<String> {
    let mut targets: Vec<String> = vec![format!("{defining}::~{}", simple_class_name(defining))];
    let mut stack: Vec<String> = ctx
        .subclasses
        .get(static_class)
        .cloned()
        .unwrap_or_default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(sub) = stack.pop() {
        if !seen.insert(sub.clone()) {
            continue;
        }
        if ctx.classes.get(&sub).is_some_and(|info| info.has_dtor) {
            let edge = format!("{sub}::~{}", simple_class_name(&sub));
            if !targets.contains(&edge) {
                targets.push(edge);
            }
        }
        if let Some(subs) = ctx.subclasses.get(&sub) {
            stack.extend(subs.iter().cloned());
        }
    }
    targets
}

/// Lower a C++ `delete p;` (the `delete_expr` hook) as a call to the destructor of `p`'s
/// pointed-to object — closing the spec-016 soundness gap where a destructor moves taint (e.g.
/// `~Derived(){ sink(v); }`). The operand `p` resolves (via [`Context::resolve_recv_obj`], reusing
/// the pointer/reference-receiver machinery) to the referent object and its **static** class; when
/// that class (or a base) declares a destructor, we emit a `DirectCall` whose targets are the
/// static type's subtree destructors for a **virtual** destructor ([`dtor_cha_targets`], CHA), or
/// the single static-type destructor otherwise, with the referent `*p` as the arg-0 (`ByRef`)
/// receiver — so the destructor body's `this.<member>` reads/writes land on the heap object the
/// setter already tainted. A destructor-less hierarchy (or a non-class operand) emits nothing,
/// leaving `delete` a taint no-op exactly as spec 014 left it. Neutral-map-driven, so it never
/// fires on the C path (`delete_expression` does not occur under the C grammar).
fn cpp_lower_delete<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    node: Node<'_>,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<(), Error> {
    // The operand of `delete <expr>` is its single named child (`p`).
    let Some(operand) = node.named_child(0) else {
        return Ok(());
    };
    // Resolve the operand to the pointed-to object and its static class. A non-class pointer
    // (or an unrecognized operand) yields `None` — nothing to destroy in the taint model.
    let Some(recv) = ctx.resolve_recv_obj(program, operand, source, scope_view)? else {
        return Ok(());
    };
    // The destructor the static type binds — walking to the nearest base that declares one. No
    // destructor anywhere in the hierarchy ⇒ `delete` is a no-op (unchanged from spec 014).
    let Some(defining) = dtor_defining_class(ctx, &recv.class) else {
        return Ok(());
    };
    // A virtual destructor dispatches by CHA over the static type's subtree destructors; a
    // non-virtual one is the single static-type destructor (`defining::~{defining}`).
    let targets: ThinVec<String> = if dtor_is_virtual(ctx, &recv.class) {
        dtor_cha_targets(ctx, &recv.class, &defining).into()
    } else {
        thin_vec![format!("{defining}::~{}", simple_class_name(&defining))]
    };
    // Emit `DirectCall <targets>(recv)` — the referent `*p` as the arg-0 by-ref receiver.
    emit_dtor_call(ctx, program, scope_view, targets, recv.exp);
    Ok(())
}

/// Emit a destructor `DirectCall <targets>(recv)` — the object as the arg-0 (`ByRef`) receiver (a
/// destructor is niladic beyond its implicit `this`); the void result goes to a throwaway temp.
/// Shared by the two destructor triggers that differ only in receiver and target set: `delete p`
/// ([`cpp_lower_delete`], receiver `*p`, CHA-or-single targets) and scope exit ([`cpp_scope_exit`],
/// the object itself, single exact-type target).
fn emit_dtor_call(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    targets: ThinVec<String>,
    recv: Exp,
) {
    let temp = ctx.allocator.next_temp();
    let ret = VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp));
    program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
        StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(targets),
            },
            rets: thin_vec![ret],
            args: thin_vec![recv],
        },
    ));
}

/// The C++ `scope_exit` hook (spec 017): at a scope's normal fall-through exit, emit the destructor
/// of each class-typed **stack** (automatic) object constructed in that scope, in **reverse**
/// construction order (LIFO, matching C++). Each object was recorded in the scope's destructor frame
/// ([`Context::dtor_frames`]) at its birthplace (`walk_declaration`'s value declarator, or
/// [`emit_construction`]); for each whose class (or a base) declares a destructor
/// ([`dtor_defining_class`]) we emit a **single-target exact-type** destructor `DirectCall` — a
/// stack object's dynamic type equals its declared type, so scope exit is always the non-virtual
/// exact-type branch (no CHA; even a `virtual ~Base` stack `Base b` destroys as exactly
/// `Base::~Base`), reusing [`emit_dtor_call`] with the object itself as the arg-0 (`ByRef`)
/// receiver. A destructor-less object emits nothing (spec 014's no-op). The top frame is empty for
/// C (which constructs no class objects), so this is inert on the C path.
fn cpp_scope_exit<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &ScopeView,
) -> anyhow::Result<(), Error> {
    // Clone the current scope's frame so we can emit while mutating `ctx` (allocator/program); the
    // frame itself is popped by `walk_compound_statement`.
    let Some(frame) = ctx.dtor_frames.last().cloned() else {
        return Ok(());
    };
    for (obj_name, class) in frame.into_iter().rev() {
        // Only a class whose hierarchy declares a destructor produces a call; a destructor-less
        // stack object emits nothing (unchanged from spec 014).
        let Some(defining) = dtor_defining_class(ctx, &class) else {
            continue;
        };
        let targets: ThinVec<String> =
            thin_vec![format!("{defining}::~{}", simple_class_name(&defining))];
        let recv_ap = ctx.build_access_path(
            &obj_name,
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        let recv = Exp::access_path(ctx.emit_loads(program, scope_view, recv_ap));
        emit_dtor_call(ctx, program, scope_view, targets, recv);
    }
    Ok(())
}

/// The C++ `ctor_prologue` hook: lower a constructor's **member-initializer list** into the
/// stores it stands for, before the body runs (C++ initialization order).
///
/// Each `member(expr)` becomes `this.<member> = <expr>`. The target is built directly as
/// `@p0.<member>` rather than resolved by name, so a parameter that shadows the member
/// (`Box(int v) : v(v)`) cannot redirect the *left* side away from `this` — only the right side
/// goes through the shared expression lowering, where `v` correctly resolves to the parameter
/// `@pN`. `this` is parameter 0, installed by `lower_function` for every instance method and
/// constructor, and the symbolic `<member>` field makes this a store in the offset-only IR,
/// hence the `RawPath`.
///
/// The pairs come from `ctor_member_inits`, which reads them off the constructor definition
/// during method discovery; a non-constructor gets an empty slice and emits nothing.
fn cpp_emit_member_inits<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &mut ScopeView,
    source: &'a str,
    member_inits: &[(String, Node<'_>)],
) -> anyhow::Result<(), Error> {
    for (member, init_expr) in member_inits {
        let target = RawPath::new(
            VariableRef::new_parameter(0u32.into()),
            thin_vec![PathSegment::symbol(member.as_str())],
        );
        let val = ctx.flatten_expr(program, *init_expr, source, scope_view)?;
        ctx.add_assign_to_program(program, scope_view, &target, &val, None);
    }
    Ok(())
}

/// The C++ grammar-shape adapters installed on the lowering [`Context`]. The first two
/// bridge the only C-subset node-shape divergences between the two grammars (per the
/// spec-002 triage); [`cpp_collect_methods`] adds C++ instance-method/constructor discovery,
/// which the top-level function query cannot see, and [`cpp_try_construct`] lowers object
/// construction at a declaration. The shared core is otherwise grammar-neutral.
pub(super) const CPP_HOOKS: GrammarHooks = GrammarHooks {
    condition_expr: cpp_condition_expr,
    subscript_index: cpp_subscript_index,
    collect_aux: cpp_collect_methods,
    // Recognize and lower object construction (`Box b(args)` and the copy/brace variants)
    // to a constructor `DirectCall`. Only invoked by the shared walker for a declaration
    // whose type is a known class, so it is inert on the C path.
    construct: cpp_try_construct,
    // Discover arity-overloaded free functions and methods, populating the neutral
    // `overloads` map before any function is registered so the mangler can distinguish
    // overloaded names (`id#1`/`id#2`, `Box::f#1`/`Box::f#2`) at every touchpoint.
    collect_overloads: cpp_discover_overloads,
    // Lower `delete p;` as a call to the destructor of `*p` (a CHA multi-target `DirectCall` for
    // a virtual destructor, the single static-type destructor otherwise); a destructor-less
    // hierarchy stays 014's no-op. Only reached for a `delete_expression`, which never occurs in
    // a C tree, so the C path is untouched.
    delete_expr: cpp_lower_delete,
    // Emit a stack object's destructor at its enclosing scope's fall-through exit (a single-target
    // exact-type `DirectCall`, reverse construction order). Driven by the neutral `dtor_frames`
    // stack (empty for C), so no scope ever emits a destructor on the C path.
    scope_exit: cpp_scope_exit,
    // Emit a constructor's member-initializer list (`Box(int x) : v(x)`) as `this.<member> =
    // <expr>` stores, before the body. Empty for every non-constructor, so the hook is inert
    // outside constructors and never fires on the C path.
    ctor_prologue: cpp_emit_member_inits,
    // The C declarator shapes plus the C++-only `reference_declarator` (`T& r`), captured
    // `@is_ref_cpp`. The shared classifier maps a non-const reference to `ByRef` (write-back)
    // and a `const T&` to `ByVal` (inbound only), reading the grammar-neutral `const`
    // qualifier; the C grammar has no `reference_declarator`, so this query is the reason the
    // classifier query is carried per-grammar (it could not compile against the C grammar).
    param_query: r#"
        (parameter_declaration
            declarator: [
                (identifier) @var_name
                (pointer_declarator declarator: (identifier) @var_name) @is_ref
                (array_declarator declarator: (identifier) @var_name) @is_ref
                (function_declarator
                    declarator: (parenthesized_declarator
                        (pointer_declarator declarator: (identifier) @var_name)))
                (reference_declarator (identifier) @var_name) @is_ref_cpp
            ]
        )
    "#,
};

/// Parse the C++ source in `source` into a CTADL IR program.
///
/// Signature-identical to [`super::parse_c_program`]: returns the lowered [`Program`],
/// whether tree-sitter reported a syntax error, and the marked-up IR dump.
pub fn parse_cpp_program(source: &str) -> anyhow::Result<(Program, bool, String), Error> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .expect("error loading C++ grammar");

    let mut ctx = Context::new(tree_sitter_cpp::LANGUAGE.into());
    ctx.set_hooks(CPP_HOOKS);
    let mut program = Program::default();
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter failed to parse");
    ctx.parse(source, &tree, &mut program)?;
    let marked_up = markup(&program, &ctx);
    Ok((program, tree.root_node().has_error(), marked_up))
}
