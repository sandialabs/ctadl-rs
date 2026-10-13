//! C++ frontend: `parse_cpp_program`.
//!
//! This drives the **tree-sitter-cpp** grammar but reuses the C frontend's
//! language-neutral lowering core in the parent module ([`super::Context`] and its
//! statement/expression walkers, the scope tree, CFG/IR builders, and temp allocator) —
//! the same code the C frontend runs after parsing. The shared core never branches on the
//! language: [`super::lower_units`] hands it the C++ grammar (and [`CPP_HOOKS`]) and it uses
//! the grammar only to compile queries against the one that parsed the tree. Any **C++-specific**
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
use hashbrown::hash_map::HashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, QueryCursor};

use super::{
    ArgShape, ClassInfo, Context, GrammarHooks, MatchExtractor, OverloadSig, ParamType, RawPath,
    ScopeView, TranslationUnit, VarKind, compile_query_for, declarator_leaf_ident, declared_class,
    function_declarator_of, is_class_member_definition, is_namespaced_definition, lower_units,
    mangle_overload, param_types, returns_void, signature_text, to_str, unparenthesize,
};
use ctadl_import::error::Error;

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

/// The IR method name of an `operator_name` declarator, normalized.
///
/// tree-sitter hands back the operator's source spelling, and C++ allows whitespace inside it:
/// `operator=`, `operator []` and `operator ()` are all legal and all name the same method. The
/// punctuation forms are squeezed to a canonical `operator=` / `operator[]` / `operator()` so a
/// definition and the expression that dispatches to it agree on one string. The **word** forms
/// (`operator new`, `operator delete`, a conversion operator `operator int`) need their
/// separator, so they keep a single space; none of them is dispatched to in this slice, but they
/// still get a well-formed name. The IR never parses a function name and the taint models match
/// `source`/`sink` by regex, so the punctuation is inert downstream.
fn operator_method_name(text: &str) -> String {
    let rest = text.strip_prefix("operator").unwrap_or(text).trim();
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        format!(
            "operator {}",
            rest.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    } else {
        format!("operator{}", rest.split_whitespace().collect::<String>())
    }
}

/// The **data member** a reference-returning method returns a reference to — `int& operator[]
/// (int i) { return v; }` yields `Some("v")` — or `None` for any other body.
///
/// A method returning `T&` denotes a *location*, so a write through its result (`a[0] = x`) must
/// land on that location. This recovers the only shape this slice models: a `return` of a bare
/// member name, written unqualified (`return v;`) or through `this` (`return this->v;` /
/// `return (*this).v;`). Whether the name really is a member of the class is checked by the
/// caller against [`ClassInfo::members`] (which is not yet complete while a class body is being
/// walked). Anything else — an element, a computed reference, a call — is out of scope and left
/// to the ordinary lowering. Only ever asked about a C++ definition; C has no reference returns.
fn body_returns_member<'a>(body: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "return_statement"
            && let Some(val) = n.named_child(0)
        {
            match val.kind() {
                "identifier" => return Some(to_str(&val, source)),
                // `return this->v;` / `return (*this).v;` — the same member, spelled explicitly.
                "field_expression" => {
                    let arg = val.child_by_field_name("argument")?;
                    let is_this = arg.kind() == "this"
                        || (arg.kind() == "pointer_expression"
                            && arg
                                .child_by_field_name("argument")
                                .is_some_and(|a| a.kind() == "this"))
                        || unparenthesize(arg).is_some_and(|a| a.kind() == "this");
                    if is_this {
                        return Some(to_str(&val.child_by_field_name("field")?, source));
                    }
                    return None;
                }
                _ => return None,
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
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

/// The IR **base name** and written **parameter types** of a `function_definition`, for overload
/// discovery — or `None` if it is not a name this slice overloads (a destructor, or an
/// unrecognized declarator shape). The types' length is the explicit arity, and their
/// [`signature_text`] is what keys a definition apart from a same-name, same-arity sibling
/// (spec 024). It mirrors *exactly* the naming the registration/call touchpoints use, so the
/// recorded signatures key the same string the mangler is later asked about:
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
/// construction resolves to the matching one. A pointer- or reference-returning definition
/// (`Box *make(…)`, `Box& setV(…)`) is unwrapped first, exactly as discovery does elsewhere.
/// Every shape matched here is C++-only, so this yields `None` for all of C.
/// The suffix that keys a class's **copy constructor** apart from its same-arity siblings.
///
/// Overload resolution in this frontend is by **arity** (spec 009/010), and a copy constructor
/// has the same arity — one — as any other one-argument constructor: `Box(int)` and
/// `Box(const Box&)` would key the single name `Box::Box`, be counted as *one* arity, and have
/// both bodies lowered into one `FunctionIdx` (the second appending its parameters to the
/// first's). The copy constructor is therefore registered, lowered and called under
/// `Class::Class#copy` instead, which is neither counted in `Class::Class`'s arity set nor
/// mangled by it. `#` cannot occur in a C++ identifier, so the key collides with nothing —
/// the same reason the arity mangler spells itself `#1`/`#2`.
const COPY_CTOR_SUFFIX: &str = "#copy";

/// The IR name of `class`'s copy constructor — the key [`cpp_collect_methods`] registers it
/// under and [`emit_ctor_call`] calls. `class` is the class's **qualified** name, exactly as
/// the ordinary ctor key `Class::Class` uses it (`ns::Box` → `ns::Box::ns::Box#copy`).
fn copy_ctor_name(class: &str) -> String {
    format!("{class}::{class}{COPY_CTOR_SUFFIX}")
}

/// Whether a constructor's `parameters` are the **copy constructor's** signature: exactly one
/// parameter whose declared type is the class itself, taken by reference (`const Box&`, `Box&`)
/// or by value (`Box`).
///
/// A pointer parameter (`Box(Box* p)`) is *not* a copy constructor — it is an ordinary
/// one-argument constructor that happens to take its own class — so its `pointer_declarator`
/// is rejected. The type is compared on its **simple** name so an in-class `Box(const Box&)`
/// and an out-of-line `ns::Box::Box(const ns::Box&)` both match. Move constructors
/// (`Box(Box&&)`, an `rvalue_reference_declarator`) are out of this slice's scope and do not
/// match. Inert for C: no C definition is ever a constructor, so this is never asked.
fn is_copy_ctor_params(params: Node<'_>, simple_class: &str, source: &str) -> bool {
    let mut cursor = params.walk();
    let mut pds = params
        .children(&mut cursor)
        .filter(|c| c.kind() == "parameter_declaration");
    let (Some(pd), None) = (pds.next(), pds.next()) else {
        return false;
    };
    let Some(ty) = pd
        .child_by_field_name("type")
        .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
    else {
        return false;
    };
    let ty = to_str(&ty, source);
    if ty.rsplit("::").next().unwrap_or(ty) != simple_class {
        return false;
    }
    match pd.child_by_field_name("declarator") {
        // `Box(const Box& o)` / `Box(const Box&)` — the ordinary spelling.
        Some(d) => matches!(
            d.kind(),
            "reference_declarator" | "abstract_reference_declarator" | "identifier"
        ),
        // `Box(Box)` — a nameless by-value parameter.
        None => true,
    }
}

fn overload_base_name(fdef: Node<'_>, source: &str) -> Option<(String, Vec<ParamType>)> {
    let declr = fdef.child_by_field_name("declarator")?;
    let (func_declr, _) = function_declarator_of(declr)?;
    let params = func_declr.child_by_field_name("parameters")?;
    let ptypes = param_types(params, source);
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
                    // The **copy** constructor is keyed apart from its same-arity siblings, so
                    // its arity is recorded against `Class::Class#copy` and never joins
                    // `Class::Class`'s set — `Box(int)` + `Box(const Box&)` are then two
                    // single-arity names, each lowered under its own `FunctionIdx`, instead of
                    // one name with one arity holding two bodies (spec 022 FR-1).
                    if is_copy_ctor_params(params, simple, source) {
                        return Some((copy_ctor_name(&class), ptypes));
                    }
                    return Some((format!("{class}::{class}"), ptypes));
                }
                return None;
            }
            let base = format!(
                "{}{}",
                enclosing_namespace_prefix(fdef, source),
                to_str(&name_node, source)
            );
            Some((base, ptypes))
        }
        // An inline method: qualify the enclosing class, then `Class::method`.
        "field_identifier" => {
            let class = enclosing_class_qualified_name(fdef, source)?;
            Some((format!("{class}::{}", to_str(&name_node, source)), ptypes))
        }
        // An inline **operator** method (`Acc& operator=(int){…}`): a method whose name is the
        // normalized operator spelling, keyed `Class::operator=` exactly as discovery keys it
        // (spec 023). Its arity joins the class's overload set like any other method's, so two
        // `operator()`s of different arity mangle to `Class::operator()#1`/`#2`.
        // A free operator (`S operator+(const S&, const S&)`, spec 032) is keyed like a free
        // function: its normalized spelling, namespace-qualified.
        "operator_name" => {
            let name = operator_method_name(to_str(&name_node, source));
            match enclosing_class_qualified_name(fdef, source) {
                Some(class) => Some((format!("{class}::{name}"), ptypes)),
                None => Some((
                    format!("{}{name}", enclosing_namespace_prefix(fdef, source)),
                    ptypes,
                )),
            }
        }
        // An out-of-line method `Class::m`, or an out-of-line constructor `Box::Box` (name ==
        // scope) — both keyed on `scope::name`, exactly as registration builds them.
        "qualified_identifier" => {
            let scope = name_node.child_by_field_name("scope")?;
            let name = name_node.child_by_field_name("name")?;
            let scope_s = to_str(&scope, source);
            // An out-of-line **operator** (`Acc& Acc::operator+=(int)`) names an `operator_name`
            // here; normalize it to the same string the inline form and the dispatch site build.
            let name_s = &if name.kind() == "operator_name" {
                operator_method_name(to_str(&name, source))
            } else {
                to_str(&name, source).to_string()
            };
            let name_s = name_s.as_str();
            // An out-of-line **copy** constructor `Box::Box(const Box& o)` (name == scope) takes
            // the same distinct key its inline form does, so either spelling lowers to the one
            // `Class::Class#copy` a construction calls.
            if scope_s == name_s && is_copy_ctor_params(params, scope_s, source) {
                return Some((copy_ctor_name(scope_s), ptypes));
            }
            Some((format!("{scope_s}::{name_s}"), ptypes))
        }
        _ => None,
    }
}

/// Discover **arity-overloaded** names — the C++ `GrammarHooks::collect_overloads`
/// implementation, run by `lower_units` over every unit before any function is registered. It
/// scans every `function_definition` (free, namespaced, inline-method, out-of-line-method),
/// computes each one's IR base name and explicit-parameter arity via [`overload_base_name`],
/// and records one [`OverloadSig`] per definition into `overloads`, which every unit's
/// [`Context::overloads`] then holds. A base name that ends up with **≥2**
/// distinct arities is thereby marked overloaded, so [`Context::overload_def_name`] mangles it
/// (`id#1`/`id#2`, `Box::f#1`/`Box::f#2`) at every touchpoint; a single-arity name stays bare.
/// A base name with **several definitions of one arity** (`Box(int)` and `Box(Box*)`) is marked
/// further: each gets the signature-mangled key `base#arity(<sig>)`, so two definitions are two
/// functions and a call site can choose between them by argument shape (spec 024).
/// Constructors are **included** (spec 010): a class's ctors are keyed on `Class::Class`, so a
/// class with ctors of ≥2 arities gets `Box::Box#1`/`Box::Box#2` and a construction resolves to
/// the matching one; a single-ctor class keeps the bare `Class::Class` and is unchanged. Inert
/// for C: no C tree contains any of the shapes matched here, so nothing is recorded and the map
/// stays empty.
///
/// A signature is recorded **once** per base name however many units declare it, so a header's
/// `static inline` included twice is still one definition of one signature — exactly as the text
/// dedup in `plan_definitions` treats it — and its arity group does not look shared.
fn cpp_discover_overloads(
    source: &str,
    root: Node<'_>,
    overloads: &mut HashMap<String, Vec<OverloadSig>>,
) -> anyhow::Result<(), Error> {
    let query = compile_query_for(
        &tree_sitter_cpp::LANGUAGE.into(),
        r#"(function_definition) @def"#,
    );
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, root, source.as_bytes());
    while let Some(m) = it.next() {
        let extract = MatchExtractor::new(&query, m);
        if let Some((base, params)) = overload_base_name(extract.get("def")?, source) {
            let arity = params.len();
            let sig = signature_text(&params);
            let sigs = overloads.entry(base).or_default();
            if !sigs.iter().any(|s| s.arity == arity && s.sig == sig) {
                sigs.push(OverloadSig { arity, sig, params });
            }
        }
    }
    Ok(())
}

/// The C++ `GrammarHooks::aux_owns_definition`: whether a `function_definition` the shared
/// definition query found is one [`cpp_collect_methods`] / [`cpp_collect_namespaced_functions`]
/// own, rather than a free function of the unit. A class member — an inline method or
/// constructor ([`is_class_member_definition`]) — and a function in a named namespace
/// ([`is_namespaced_definition`]) are lowered there under their qualified names; so is an
/// out-of-line member (`void Box::m() {…}`, a `qualified_identifier`), a destructor (`~Box`),
/// an operator, and any other definition whose function name is not a plain `identifier` —
/// none of which the shared `function_head` can name, and none of which is a free function.
/// Only a definition naming a plain `identifier` at file scope is left to the shared pass,
/// which then sees (and reports) exactly the declarator shapes it would for C.
fn cpp_aux_owns_definition(def: Node<'_>) -> bool {
    if is_class_member_definition(def) || is_namespaced_definition(def) {
        return true;
    }
    // Unwrap what a definition's declarator may wrap around its `function_declarator` (a
    // pointer or reference return, parentheses) and look at the name it declares.
    match def
        .child_by_field_name("declarator")
        .and_then(function_declarator_of)
    {
        Some((func_declr, _)) => func_declr
            .child_by_field_name("declarator")
            .and_then(unparenthesize)
            .is_none_or(|name| name.kind() != "identifier"),
        // Not function-shaped at all: left to the shared pass, which reports it as for C.
        None => false,
    }
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
/// The top-level `function_definition` query in `Context::lower_definitions` only matches
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
        /// The data member the method returns a reference to (`int& operator[](int){ return v; }`
        /// → `Some("v")`), recorded in `ClassInfo.returns_member` so a write *through* the result
        /// (`a[0] = x`) becomes a store to `<receiver>.<member>`. `None` for every method that
        /// returns by value or returns anything but a bare member.
        returns_member: Option<String>,
        /// Whether this is a **`static`** member function — lowered *without* the implicit `this`
        /// (its parameters are exactly the declared ones), like a receiver-less free function
        /// keyed `Class::method` (spec 015). A static method is not an instance method, so it is
        /// not registered in [`ClassInfo::methods`] (an `obj.m()` dispatch must not resolve to it)
        /// and is never `returns_self`/virtual. `false` for every ordinary method and constructor.
        is_static: bool,
        /// A constructor's member-initializer list as `(member, init-expr)` pairs; empty
        /// for an ordinary method and for a constructor written without one.
        member_inits: Vec<(String, Node<'t>)>,
        /// Whether this is a constructor (copy constructors and the synthesized implicit
        /// default constructor included), which applies the class's default member
        /// initializers to every member its own list leaves out (spec 028).
        is_ctor: bool,
    }
    let mut methods: Vec<MethodDef<'_>> = Vec::new();
    // Each class's default member initializers (`int v = e;` / `int v{e};`) and the opening
    // brace of its body, which stands in for the empty parameter list and body of a synthesized
    // implicit default constructor (spec 028).
    let mut nsdmis: HashMap<String, Vec<(String, Node<'_>)>> = HashMap::new();
    let mut class_anchor: HashMap<String, Node<'_>> = HashMap::new();

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
                    // A default member initializer: `= e` is the expression, `{e}` its one
                    // element (spec 028). A static member's initializer is a global's, not a
                    // constructor's.
                    let default_value = child
                        .child_by_field_name("default_value")
                        .filter(|_| !is_static);
                    // A by-value member whose type names a class is a member object (spec 029);
                    // whether that class is known is decided when a constructor runs.
                    let object_class = child
                        .child_by_field_name("type")
                        .filter(|t| matches!(t.kind(), "type_identifier" | "qualified_identifier"))
                        .filter(|_| !is_static)
                        .map(|t| to_str(&t, source).to_string());
                    let mut dc = child.walk();
                    for declr in child.children_by_field_name("declarator", &mut dc) {
                        if let Some(name) = member_name(declr, source) {
                            info.members.insert(name.to_string());
                            if is_static {
                                info.static_members.insert(name.to_string());
                            }
                            if let Some(class) = &object_class
                                && declr.kind() == "field_identifier"
                            {
                                info.member_objects.push((name.to_string(), class.clone()));
                            }
                            if let Some(init) = default_value {
                                nsdmis
                                    .entry(qualified_name.clone())
                                    .or_default()
                                    .push((name.to_string(), init));
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
                    // A pointer or reference return type (`int *get(…)`, `Class& m(…)`) nests
                    // the `function_declarator` inside `pointer_declarator`s / a
                    // `reference_declarator`; unwrap them so the method is discovered (this
                    // used to demand the bare `function_declarator` and silently dropped every
                    // pointer-returning method body -- spec 130, on the C++ side) and so a
                    // `void *` return counts as the value it is.
                    let is_reference_return = declr.kind() == "reference_declarator";
                    let Some((func_declr, returns_pointer)) = function_declarator_of(declr) else {
                        continue;
                    };
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
                    // An **operator** method (`Acc& operator=(int x){…}`) names an `operator_name`
                    // declarator, which is neither a `field_identifier` nor a class-named
                    // `identifier`, so discovery used to drop it and every `a = x` on the object
                    // lowered as the built-in store (spec 023). It is an ordinary instance method
                    // under a punctuation name: `Acc::operator=`, with the implicit `this`.
                    let is_operator = name_node.kind() == "operator_name";
                    if name_node.kind() != "field_identifier"
                        && !is_ctor
                        && !is_dtor
                        && !is_operator
                    {
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
                    } else if is_operator {
                        operator_method_name(to_str(&name_node, source))
                    } else {
                        to_str(&name_node, source).to_string()
                    };
                    let void =
                        returns_void(child.child_by_field_name("type"), returns_pointer, source);
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
                            returns_member: None,
                            is_static: false,
                            member_inits: Vec::new(),
                            is_ctor: false,
                        });
                    } else if is_ctor {
                        // A constructor returns no value and writes the object via `this`;
                        // model it as a void `Class::Class` method and record the member-
                        // initializer list (`: v(x)`) so construction can lower it.
                        info.has_ctor = true;
                        // A constructor whose single parameter is a same-class object is the
                        // class's **copy** constructor: it is lowered under the distinct key
                        // `Class::Class#copy` (which `overload_base_name` also recorded, so the
                        // mangler leaves both it and its same-arity siblings bare), and the class
                        // is marked so a construction from an object — and a by-value class
                        // argument — runs it instead of taking the implicit value copy.
                        let name = if is_copy_ctor_params(params, simple_name, source) {
                            info.has_copy_ctor = true;
                            format!("{name}{COPY_CTOR_SUFFIX}")
                        } else {
                            name
                        };
                        methods.push(MethodDef {
                            class: qualified_name.clone(),
                            name,
                            params,
                            body: method_body,
                            void: true,
                            returns_self: false,
                            returns_member: None,
                            is_static: false,
                            member_inits: ctor_member_inits(child, source),
                            is_ctor: true,
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
                        // A reference-returning method whose body returns a bare **member**
                        // (`int& operator[](int){ return v; }`) yields a reference to that
                        // member's storage, so a write through the result must land there
                        // (spec 023 FR-4). Never a static method, and never one that already
                        // returns `*this`.
                        let returns_member = (!is_static && is_reference_return && !returns_self)
                            .then(|| body_returns_member(method_body, source))
                            .flatten()
                            .map(str::to_string);
                        if !is_static {
                            info.methods.insert(name.clone());
                            if returns_self {
                                info.returns_self.insert(name.clone());
                            }
                            if let Some(member) = &returns_member {
                                info.returns_member.insert(name.clone(), member.to_string());
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
                            returns_member,
                            is_static,
                            member_inits: Vec::new(),
                            is_ctor: false,
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(brace) = body.child(0) {
            class_anchor.insert(qualified_name.clone(), brace);
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
    // top-level `function_definition` query in `Context::lower_definitions` only matches a
    // plain `identifier` name, so these are invisible to it and must be found here. We gather
    // (class, method, params, body) first — the cursor borrows the tree, not `ctx`, so reading
    // `ctx.classes` to filter to already-declared classes is fine — then register the method
    // names and queue the bodies after the cursor drops.
    // The declarator is captured whole and unwrapped by `function_declarator_of`: a pointer
    // return (`int *Box::get(…)`) nests the `function_declarator` in a `pointer_declarator`
    // per `*`, a reference return (`Box& Box::setV(…)`) in a `reference_declarator`, and a
    // query pattern cannot follow either to arbitrary depth.
    let ool_query = ctx.compile_query(
        r#"
        (function_definition
            declarator: (_) @decl
            body: (compound_statement) @body) @def
        "#,
    );
    let mut out_of_line: Vec<(bool, bool, MethodDef<'_>)> = Vec::new();
    {
        let mut ool_cursor = QueryCursor::new();
        let mut ool_it = ool_cursor.matches(&ool_query, root, source.as_bytes());
        while let Some(m) = ool_it.next() {
            let extract = MatchExtractor::new(&ool_query, m);
            let decl = extract.get("decl")?;
            // An out-of-line member is named by a `qualified_identifier` whose scope is the
            // class (`Box::m`); anything else the shared pass or the inline walk above owns.
            let Some((func_declr, returns_pointer)) = function_declarator_of(decl) else {
                continue;
            };
            let Some(name_node) = func_declr.child_by_field_name("declarator") else {
                continue;
            };
            if name_node.kind() != "qualified_identifier" {
                continue;
            }
            let (Some(scope), Some(method_node), Some(params)) = (
                name_node.child_by_field_name("scope"),
                name_node.child_by_field_name("name"),
                func_declr.child_by_field_name("parameters"),
            ) else {
                continue;
            };
            // An out-of-line **operator** (`Acc& Acc::operator+=(int x){…}`) names its method
            // with an `operator_name` rather than an `identifier`; accept it and normalize the
            // spelling to the same string the inline form and the dispatch site build (spec 023).
            if scope.kind() != "namespace_identifier"
                || !matches!(method_node.kind(), "identifier" | "operator_name")
            {
                continue;
            }
            let class = to_str(&scope, source).to_string();
            // Only lower out-of-line bodies for a class known from its declaration (this
            // slice's scope). An unknown qualifier (e.g. a namespaced free function) is left
            // alone — it is not an instance method we can model here.
            if !ctx.classes.contains_key(&class) {
                continue;
            }
            let name = if method_node.kind() == "operator_name" {
                operator_method_name(to_str(&method_node, source))
            } else {
                to_str(&method_node, source).to_string()
            };
            let def = extract.get("def")?;
            let body = extract.get("body")?;
            // `Box::Box(…)` (name == class) is an out-of-line **constructor**: model it as a
            // void `Class::Class` and carry its member-initializer list. An ordinary
            // out-of-line method keeps its declared return arity and has no init list.
            let is_ctor = name == class;
            // An out-of-line copy constructor `Box::Box(const Box& o) : v(o.v) {}` takes the same
            // distinct key the inline form does, so either spelling is the one function a copy
            // construction calls.
            let is_copy_ctor = is_ctor && is_copy_ctor_params(params, &class, source);
            let void =
                is_ctor || returns_void(def.child_by_field_name("type"), returns_pointer, source);
            // A reference-returning out-of-line definition (`Box& Box::setV(…)`) has its
            // `function_declarator` wrapped in a `reference_declarator`; if its body returns
            // `*this` the result aliases the receiver object (a fluent setter).
            let returns_self =
                !is_ctor && decl.kind() == "reference_declarator" && body_returns_deref_this(body);
            // Same reference-to-member record the inline path builds (spec 023 FR-4).
            let returns_member =
                (!is_ctor && decl.kind() == "reference_declarator" && !returns_self)
                    .then(|| body_returns_member(body, source))
                    .flatten()
                    .map(str::to_string);
            let member_inits = if is_ctor {
                ctor_member_inits(def, source)
            } else {
                Vec::new()
            };
            out_of_line.push((
                is_ctor,
                is_copy_ctor,
                MethodDef {
                    class,
                    name: if is_copy_ctor {
                        format!("{name}{COPY_CTOR_SUFFIX}")
                    } else {
                        name
                    },
                    params,
                    body,
                    void,
                    returns_self,
                    returns_member,
                    // Out-of-line static methods are out of this slice's scope (the `static`
                    // keyword appears only on the in-class declaration, not the out-of-line
                    // definition); an out-of-line definition here is always an instance method.
                    is_static: false,
                    member_inits,
                    is_ctor,
                },
            ));
        }
    }
    // Register each out-of-line method's name on its class (so `recv.m(…)` dispatches even
    // though the body lives outside the class body), or mark the class as having a
    // constructor; then queue it for lowering alongside the inline methods.
    for (is_ctor, is_copy_ctor, md) in out_of_line {
        if let Some(info) = ctx.classes.get_mut(&md.class) {
            if is_ctor {
                info.has_ctor = true;
                info.has_copy_ctor |= is_copy_ctor;
            } else {
                info.methods.insert(md.name.clone());
                if md.returns_self {
                    info.returns_self.insert(md.name.clone());
                }
                if let Some(member) = &md.returns_member {
                    info.returns_member.insert(md.name.clone(), member.clone());
                }
            }
        }
        methods.push(md);
    }

    // Phase 1.6 (spec 028): default member initializers. A class that declares no constructor
    // but has some gets the implicit default constructor C++ defines for it, so `B b;` runs
    // them; its parameter list and body are the class body's opening brace, which lowers to
    // nothing. Then every constructor applies the defaults for the members its own initializer
    // list leaves out.
    // Spec 029 widens "has some": a base or member object whose class has a constructor
    // (perhaps itself implicit) needs constructing too, so this runs to a fixpoint.
    loop {
        let needs_ctor: Vec<String> = ctx
            .classes
            .iter()
            .filter(|(class, info)| {
                let constructs = |c: &String| ctx.classes.get(c).is_some_and(|i| i.has_ctor);
                !info.has_ctor
                    && class_anchor.contains_key(*class)
                    && (nsdmis.get(*class).is_some_and(|d| !d.is_empty())
                        || info.bases.iter().any(constructs)
                        || info.member_objects.iter().any(|(_, c)| constructs(c)))
            })
            .map(|(class, _)| class.clone())
            .collect();
        if needs_ctor.is_empty() {
            break;
        }
        for class in needs_ctor {
            let anchor = class_anchor[&class];
            if let Some(info) = ctx.classes.get_mut(&class) {
                info.has_ctor = true;
            }
            methods.push(MethodDef {
                class: class.clone(),
                name: class,
                params: anchor,
                body: anchor,
                void: true,
                returns_self: false,
                returns_member: None,
                is_static: false,
                member_inits: Vec::new(),
                is_ctor: true,
            });
        }
    }
    for md in methods.iter_mut().filter(|md| md.is_ctor) {
        for (member, init) in nsdmis.get(&md.class).into_iter().flatten() {
            if !md.member_inits.iter().any(|(m, _)| m == member) {
                md.member_inits.push((member.clone(), *init));
            }
        }
    }

    // Phase 1.75: record which parameters of every function this unit defines are class objects
    // taken **by value** (`void take(Box b)`). Runs after every class is known (phase 1) and
    // before any body is lowered (phase 2b, and `lower_definitions` after it), so a call site
    // anywhere in the unit finds the record regardless of definition order.
    cpp_discover_byval_class_params(ctx, source, root)?;

    // Phase 2a: register a function index for every method, so a method calling another
    // method (or a top-level body calling one) resolves it regardless of definition order.
    // An **overloaded** method or constructor (`Box::f#1`/`Box::f#2`, `Box::Box#1`/`Box::Box#2`)
    // reserves a distinct `FunctionIdx` per arity via the neutral mangler — and per *signature*
    // where one arity has several definitions (`Box::Box#1(int)` / `Box::Box#1(Box*)`), so no two
    // definitions ever share a function (spec 024 FR-1); a non-overloaded
    // method and a single-arity constructor stay bare — the same `Class::method` / `Class::Class`
    // string a `recv.m(…)` dispatch and the `construct` hook build. (Spec 010 lifted the
    // constructor exclusion, so a class with ctors of ≥2 arities mangles them here too.)
    for md in &methods {
        let base = format!("{}::{}", md.class, md.name);
        let qualified = ctx.overload_def_name(&base, md.params, source);
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
        let qualified = ctx.overload_def_name(&base, md.params, source);
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
    cpp_collect_free_operators(ctx, source, root, program, global_sidx)?;
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
                    declarator: (_) @decl
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
            // The declarator is captured whole so a pointer return (`char *ns::dup(…)`) is
            // found through its `pointer_declarator`s, like everywhere else.
            let Some((func_declr, returns_pointer)) = function_declarator_of(extract.get("decl")?)
            else {
                continue;
            };
            let (Some(name_node), Some(params)) = (
                func_declr.child_by_field_name("declarator"),
                func_declr.child_by_field_name("parameters"),
            ) else {
                continue;
            };
            if name_node.kind() != "identifier" {
                continue;
            }
            let fname = to_str(&name_node, source);
            // Qualified name (`ns::f`), then arity-mangled if this namespaced free function is
            // itself overloaded (`ns::f#1`/`ns::f#2`) — matching the call-site edge. A
            // non-overloaded namespaced function stays bare-qualified (identity mangler), so
            // the existing namespace cases are unchanged.
            let base = format!("{}{}", enclosing_namespace_prefix(def, source), fname);
            let qualified = ctx.overload_def_name(&base, params, source);
            let void = returns_void(def.child_by_field_name("type"), returns_pointer, source);
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

/// Discover and lower **free operators** (spec 032): a `function_definition` outside any class
/// whose declarator names an `operator_name` (`S operator+(const S& a, const S& b)`). Each is keyed
/// by [`overload_base_name`] -- its normalized spelling, namespace-qualified and overload-mangled
/// like any free function -- which is the name [`free_operator_call`] resolves. The shared
/// definition pass leaves these alone ([`cpp_aux_owns_definition`]: the name is no `identifier`),
/// so they used to be dropped. An out-of-line member operator (`S::operator+`) names a
/// `qualified_identifier` and is method discovery's.
fn cpp_collect_free_operators<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    root: Node<'_>,
    program: &mut Program,
    global_sidx: usize,
) -> anyhow::Result<(), Error> {
    let query = ctx.compile_query(
        r#"
        (function_definition
            declarator: (_) @decl
            body: (compound_statement) @body) @def
        "#,
    );
    let mut defs: Vec<(String, Node<'_>, Node<'_>, bool)> = Vec::new();
    {
        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(&query, root, source.as_bytes());
        while let Some(m) = it.next() {
            let extract = MatchExtractor::new(&query, m);
            let def = extract.get("def")?;
            if is_class_member_definition(def) {
                continue;
            }
            let Some((func_declr, returns_pointer)) = function_declarator_of(extract.get("decl")?)
            else {
                continue;
            };
            let (Some(name_node), Some(params)) = (
                func_declr.child_by_field_name("declarator"),
                func_declr.child_by_field_name("parameters"),
            ) else {
                continue;
            };
            if name_node.kind() != "operator_name" {
                continue;
            }
            let Some((base, _)) = overload_base_name(def, source) else {
                continue;
            };
            let qualified = ctx.overload_def_name(&base, params, source);
            let void = returns_void(def.child_by_field_name("type"), returns_pointer, source);
            defs.push((qualified, params, extract.get("body")?, void));
        }
    }
    for (qualified, _, _, _) in &defs {
        ctx.functions
            .entry(qualified.clone())
            .or_insert_with(|| program.new_function());
    }
    for (qualified, params, body, void) in defs {
        ctx.lower_function(
            source,
            program,
            global_sidx,
            &qualified,
            void,
            params,
            body,
            None,
            false,
            &[],
        )?;
    }
    Ok(())
}

/// Record, per function this unit defines, which of its **explicit** parameters are class
/// objects taken **by value** — the neutral [`Context::byval_class_params`] map
/// [`cpp_copy_args`] consults at every call site (spec 022 FR-3).
///
/// C++ copy-constructs a by-value parameter from the argument, so `void take(Box b)` called as
/// `take(a)` runs `Box`'s copy constructor on the way in; the callee's `b` is the *copy* that
/// constructor built, not `a`. Nothing in the IR expresses that, so the call site has to emit it
/// — and to do that it must know the callee's parameter modes, which live at the callee's
/// definition, possibly further down the file. Hence a discovery pass: it scans every
/// `function_definition` (free, namespaced, inline-method, out-of-line-method) exactly as
/// [`cpp_discover_overloads`] does, keys each by the same IR name a call site resolves to
/// ([`overload_base_name`] + the arity mangler), and records one slot per explicit parameter.
///
/// A parameter counts as by value iff its declarator is the bare parameter **name**: a pointer
/// (`Box* p`) or a reference (`Box& b`, `const Box& b`) declarator names the caller's object, and
/// no copy runs for it. The type must name a class this unit declared ([`declared_class`], the
/// same resolver that binds a class-typed local or parameter), so an `int` parameter records
/// nothing. Only functions with at least one such parameter are recorded, so the map stays empty
/// for C — where `classes` is empty and no parameter can ever resolve to a class.
fn cpp_discover_byval_class_params(
    ctx: &mut Context<'_>,
    source: &str,
    root: Node<'_>,
) -> anyhow::Result<(), Error> {
    let query = ctx.compile_query(r#"(function_definition) @def"#);
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, root, source.as_bytes());
    let mut found: Vec<(String, Vec<Option<String>>)> = Vec::new();
    while let Some(m) = it.next() {
        let extract = MatchExtractor::new(&query, m);
        let def = extract.get("def")?;
        let Some((base, ptypes)) = overload_base_name(def, source) else {
            continue;
        };
        let Some(params) = def
            .child_by_field_name("declarator")
            .and_then(function_declarator_of)
            .and_then(|(func_declr, _)| func_declr.child_by_field_name("parameters"))
        else {
            continue;
        };
        let mut pcursor = params.walk();
        let byval: Vec<Option<String>> = params
            .children(&mut pcursor)
            .filter(|pd| pd.kind() == "parameter_declaration")
            .map(|pd| {
                pd.child_by_field_name("declarator")
                    .filter(|d| d.kind() == "identifier")
                    .and_then(|_| declared_class(&ctx.classes, pd, source))
            })
            .collect();
        if byval.iter().any(Option::is_some) {
            found.push((
                mangle_overload(
                    &ctx.overloads,
                    &base,
                    ptypes.len(),
                    &signature_text(&ptypes),
                ),
                byval,
            ));
        }
    }
    drop(it);
    drop(cursor);
    ctx.byval_class_params.extend(found);
    Ok(())
}

/// Copy-construct a call's **by-value class** arguments — the C++ `GrammarHooks::copy_args`
/// implementation (spec 022 FR-3).
///
/// For each explicit argument whose callee parameter [`cpp_discover_byval_class_params`] recorded
/// as a by-value object of a class that declares a **user** copy constructor, and whose argument
/// expression names a known object of exactly that class, this emits
/// `Class::Class#copy(&<temp>, arg)` into a fresh object and substitutes that object for the
/// argument — so the callee's parameter holds what the copy constructor built, exactly as it does
/// at `Box b(a)`. The copy is emitted before the call statement, which is where C++ runs it.
///
/// Everything else is left alone: a class **without** a user copy constructor keeps the existing
/// value copy (its implicit member-wise copy *is* that copy), a by-reference or pointer parameter
/// records nothing so never copies, and an argument that is not a named object of the parameter's
/// class (a temporary, a literal, a derived object) is out of this slice's scope. `callees` are
/// the call's already-resolved candidate targets, so a call whose targets this unit did not define
/// finds no record. A position is copied when **any** candidate takes it by value (spec 024 FR-3):
/// one copy per argument, so a multi-target call emits exactly what a single-target one does.
/// Inert for C: `byval_class_params` is empty there, so every lookup misses.
fn cpp_copy_args<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &mut ScopeView,
    source: &'a str,
    callees: &[String],
    arg_list: Node<'_>,
    args: &mut ThinVec<Exp>,
) -> anyhow::Result<(), Error> {
    let records: Vec<Vec<Option<String>>> = callees
        .iter()
        .filter_map(|callee| ctx.byval_class_params.get(callee).cloned())
        .collect();
    if records.is_empty() {
        return Ok(());
    }
    let mut cursor = arg_list.walk();
    let arg_nodes: Vec<Node<'_>> = arg_list
        .children(&mut cursor)
        .filter(|c| c.is_named())
        .collect();
    for (i, arg) in args.iter_mut().enumerate() {
        // The class this position is taken by value as, on the first candidate that takes it
        // that way — one copy per argument however many targets the call has.
        let class = records
            .iter()
            .find_map(|byval| byval.get(i).cloned().flatten());
        let (Some(class), Some(node)) = (class, arg_nodes.get(i)) else {
            continue;
        };
        let class = &class;
        if !ctx
            .classes
            .get(class)
            .is_some_and(|info| info.has_copy_ctor)
            || !names_class_object(ctx, source, *node, class)
        {
            continue;
        }
        // The parameter's own object. A `<t…>` temp is a local by name alone (no scope entry
        // needed), and it is deliberately *not* recorded in a destructor frame: it is the
        // callee's parameter, destroyed at the end of the call, not an object of this scope.
        let obj = ctx.allocator.next_temp();
        emit_ctor_call(
            ctx,
            program,
            scope_view,
            class,
            &obj,
            thin_vec![arg.clone()],
            &[ArgShape::Object(class.clone())],
            true,
        );
        let ap = ctx.build_access_path(
            &obj,
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        *arg = Exp::access_path(ctx.emit_loads(program, scope_view, ap));
    }
    Ok(())
}

/// Extract a constructor's initializer list from its `function_definition` node as
/// `(name, field_initializer)` pairs. The list (`: Base(x), v(y), in{z}`) appears as an unnamed
/// `field_initializer_list` child between the declarator and the body. Each `field_initializer`
/// names a member (`field_identifier`) or a base class (`type_identifier` /
/// `qualified_identifier`), followed by its `argument_list` or `initializer_list`;
/// [`cpp_emit_member_inits`] reads the arguments off it ([`init_args`]). Returns empty when
/// there is no list.
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
            let Some(name) = fi.named_child(0).filter(|n| {
                matches!(
                    n.kind(),
                    "field_identifier" | "type_identifier" | "qualified_identifier"
                )
            }) else {
                continue;
            };
            inits.push((to_str(&name, source).to_string(), fi));
        }
    }
    inits
}

/// The argument expressions of one constructor initializer: the arguments of a
/// `field_initializer` (`v(x, y)` / `v{x}`), the elements of a braced default (`int v{x};`), or
/// a default `= e`'s one expression.
fn init_args(node: Node<'_>) -> Vec<Node<'_>> {
    let list = match node.kind() {
        "field_initializer" => node.named_child(1),
        "initializer_list" | "argument_list" => Some(node),
        _ => return vec![node],
    };
    let Some(list) = list else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect()
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

    // Whether this class runs a **user** copy constructor on a copy. A class without one keeps
    // the value-copy lowering below byte-for-byte: its implicit member-wise copy *is* that copy.
    let has_copy_ctor = ctx
        .classes
        .get(class)
        .is_some_and(|info| info.has_copy_ctor);

    // Recover the object's name and the constructor argument nodes, by form. `copy` says the
    // construction is a **copy construction** from a same-class object, which calls the class's
    // copy constructor `Class::Class#copy` rather than the arity-resolved `Class::Class`.
    // `shapes` is what the arguments can *mean*, read off the syntax before any of them is
    // lowered — and before the object itself is registered, so `Box b(&a)` classifies `a`
    // (spec 024 FR-2). An empty `shapes` goes with a copy construction, which resolves by key.
    let (obj_node, args, shapes, copy): (Node<'_>, ThinVec<Exp>, Vec<ArgShape>, bool) = match declr
        .kind()
    {
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
            // `Box b(a)` — one bare type-like argument naming a known object of this very
            // class — is a **copy construction**. `reconstruct_mvp_arg` already turns that
            // `parameter_declaration` back into a read of `a`, so only the callee changes.
            let copy = has_copy_ctor
                && pds.len() == 1
                && mvp_arg_names_class_object(ctx, source, pds[0], class);
            let shapes: Vec<ArgShape> = pds
                .iter()
                .map(|pd| mvp_arg_shape(ctx, source, *pd))
                .collect();
            let mut args: ThinVec<Exp> = ThinVec::new();
            for pd in pds {
                args.push(reconstruct_mvp_arg(ctx, source, program, scope_view, pd)?);
            }
            (obj_node, args, shapes, copy)
        }
        // Default construction `Box b;` runs the default constructor (spec 028).
        "identifier" => (declr, ThinVec::new(), Vec::new(), false),
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
                    let shapes = ctx.arg_shapes(arg_list, source);
                    let args = ctx.collect_arguments(program, arg_list, source, scope_view)?;
                    (obj_node, args, shapes, false)
                }
                // Copy initialization from an object: `Box b = a`. The value is the bare
                // identifier `a`; when it names an object of this class and the class declares a
                // copy constructor, C++ *runs* that constructor, so lower the construction edge
                // instead of falling through to the shared value copy. Without a user copy
                // constructor this arm does not fire and `Box b = a` keeps the value copy, which
                // is exactly the implicit member-wise copy (spec 022 FR-2).
                "identifier" if has_copy_ctor && names_class_object(ctx, source, value, class) => {
                    let arg = ctx.flatten_expr(program, value, source, scope_view)?;
                    (obj_node, thin_vec![arg], Vec::new(), true)
                }
                "initializer_list" => {
                    let mut lcursor = value.walk();
                    let elems: Vec<Node<'_>> = value
                        .children(&mut lcursor)
                        .filter(|e| e.is_named())
                        .collect();
                    // `Box b{a}` is the brace spelling of the same copy construction.
                    let copy = has_copy_ctor
                        && elems.len() == 1
                        && names_class_object(ctx, source, elems[0], class);
                    let shapes: Vec<ArgShape> = elems
                        .iter()
                        .map(|elem| ctx.arg_shape(*elem, source))
                        .collect();
                    let mut args: ThinVec<Exp> = ThinVec::new();
                    for elem in elems {
                        args.push(ctx.flatten_expr(program, elem, source, scope_view)?);
                    }
                    (obj_node, args, shapes, copy)
                }
                // Direct init `Box b(a, b)` whose arguments are **not** all type-like — e.g.
                // one is a literal (`Box b(0, source())`). Because a literal cannot be a
                // parameter declaration, tree-sitter does **not** most-vexing-parse this as a
                // function declaration: the object is an `init_declarator` whose `value` is the
                // constructor `argument_list`. (The all-type-like direct form `Box b(source())`
                // is MVP-mangled and handled by the `function_declarator` arm above.) Collect
                // the arguments exactly as a call's argument list.
                "argument_list" => {
                    let shapes = ctx.arg_shapes(value, source);
                    let args = ctx.collect_arguments(program, value, source, scope_view)?;
                    (obj_node, args, shapes, false)
                }
                _ => return Ok(false),
            }
        }
        _ => return Ok(false),
    };

    emit_construction(
        ctx, source, program, scope_view, class, obj_node, args, &shapes, copy,
    );
    Ok(true)
}

/// The [`ArgShape`] of one most-vexing-parse constructor argument — [`Context::arg_shape`] for
/// the spelling `Box b(p)` gives its argument, where tree-sitter has shredded `p` into a
/// `parameter_declaration` of type `p` with no declarator (see [`reconstruct_mvp_arg`]).
///
/// Only that one shape can be an object or a pointer to one: the argument is a bare name, so its
/// shape is what the neutral maps say that name holds. Any other `parameter_declaration` spells
/// something the maps cannot classify — `Box b(source())` is a call (an
/// `abstract_function_declarator`), `Box b(int x)` is a function declaration — and is
/// [`ArgShape::Other`], which is what makes an undecided call a multi-target edge.
fn mvp_arg_shape(ctx: &Context<'_>, source: &str, pd: Node<'_>) -> ArgShape {
    if pd.child_by_field_name("declarator").is_some() {
        return ArgShape::Other;
    }
    pd.child_by_field_name("type")
        .filter(|t| matches!(t.kind(), "type_identifier" | "identifier"))
        .map_or(ArgShape::Other, |t| ctx.named_shape(to_str(&t, source)))
}

/// Whether `node` is an expression naming a **known object of exactly `class`** — a local, a
/// class-typed parameter (spec 020) or a reference bound to one, all of which the neutral
/// `local_types` map holds for the function being lowered. This is the whole of the
/// "same-class object" test a copy construction needs: derived-to-base copies are out of scope,
/// so the classes must be equal, and anything that is not a plain identifier (a literal, a call,
/// a temporary) is not an object this slice can name. `local_types` is empty for C.
fn names_class_object(ctx: &Context<'_>, source: &str, node: Node<'_>, class: &str) -> bool {
    node.kind() == "identifier"
        && ctx
            .local_types
            .get(to_str(&node, source))
            .is_some_and(|c| c == class)
}

/// [`names_class_object`] for the most-vexing-parse spelling of the same argument. `Box b(a)`
/// parses `a` as a `parameter_declaration` of type `a` with **no** declarator (see
/// [`reconstruct_mvp_arg`]), so the object's name is the parameter's *type* text. A
/// `parameter_declaration` that does carry a declarator spells something else — `Box b(int x)`
/// is a function declaration, not a construction — and is not an object.
fn mvp_arg_names_class_object(ctx: &Context<'_>, source: &str, pd: Node<'_>, class: &str) -> bool {
    pd.child_by_field_name("declarator").is_none()
        && pd
            .child_by_field_name("type")
            .filter(|t| t.kind() == "type_identifier")
            .is_some_and(|t| {
                ctx.local_types
                    .get(to_str(&t, source))
                    .is_some_and(|c| c == class)
            })
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
/// The ctor edge is arity-resolved (spec 010) and then shape-resolved among the ctors of that
/// arity (spec 024): the callee names are
/// `overload_targets("Class::Class", <explicit-arg-count>, <arg shapes>)`, so a class with ctors
/// of ≥2 arities resolves the construction to the ctor with exactly that many parameters
/// (`Box::Box#1` vs `Box::Box#2`) and a class with two ctors of one arity to the one its
/// arguments can mean (`Box::Box#1(Box*)` for `Box b(&a)`); a single-ctor class keeps the bare
/// `Class::Class` (identity mangler), so every existing construction is byte-for-byte unchanged.
fn emit_construction<'a>(
    ctx: &mut Context<'a>,
    source: &'a str,
    program: &mut Program,
    scope_view: &ScopeView,
    class: &str,
    obj_node: Node<'_>,
    ctor_args: ThinVec<Exp>,
    arg_shapes: &[ArgShape],
    copy: bool,
) {
    let obj_name = to_str(&obj_node, source);
    ctx.scope_tree
        .add_variable(scope_view.sidx, obj_name.to_string(), VarKind::Local);
    ctx.local_types
        .insert(obj_name.to_string(), class.to_string());
    // Record this constructed stack object in the enclosing scope's destructor frame so its
    // destructor runs at the scope's closing `}` (spec 017). Heap `new` objects go through
    // [`emit_ctor_call`] directly (not here), so they are not recorded and get no scope-exit
    // destructor. The frame is empty for C (no class objects), so this never fires on the C path.
    if let Some(frame) = ctx.dtor_frames.last_mut() {
        frame.push((obj_name.to_string(), class.to_string()));
    }
    emit_ctor_call(
        ctx, program, scope_view, class, obj_name, ctor_args, arg_shapes, copy,
    );
}

/// Emit the constructor `DirectCall Class::Class#<arity>(&obj, args…)` for an **already-
/// registered** local `obj_name`. Its access path is the arg-0 (`ByRef`) receiver, so the
/// constructor's `this.<member>` writes land in the object; the void result goes to a
/// throwaway temp. The callee name is arity-resolved via the neutral mangler (spec 010), so a
/// class with ctors of ≥2 arities resolves to the matching `Box::Box#1`/`Box::Box#2` and a
/// single-ctor class keeps the bare `Class::Class`. Where one arity has **several** constructors
/// (`Box(int)` and `Box(Box*)`), `arg_shapes` picks the one the arguments can mean —
/// `Box::Box#1(Box*)` for `Box b(&a)` — and, when they can mean more than one, every candidate
/// becomes a target of the one construction edge (spec 024). Shared by stack construction
/// ([`emit_construction`]) and heap `new` ([`lower_new_expression`]).
fn emit_ctor_call(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    class: &str,
    obj_name: &str,
    ctor_args: ThinVec<Exp>,
    arg_shapes: &[ArgShape],
    copy: bool,
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
    emit_ctor_edge(
        ctx, program, scope_view, class, recv, ctor_args, arg_shapes, copy,
    );
}

/// The constructor edge itself: `DirectCall Class::Class…(recv, args…)` with `recv` -- an
/// already-resolved receiver expression -- as arg 0. See [`emit_ctor_call`].
#[allow(clippy::too_many_arguments)]
fn emit_ctor_edge(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    class: &str,
    recv: Exp,
    ctor_args: ThinVec<Exp>,
    arg_shapes: &[ArgShape],
    copy: bool,
) {
    let arity = ctor_args.len();
    let mut args: ThinVec<Exp> = thin_vec![recv];
    args.extend(ctor_args);

    // A **copy** construction calls the copy constructor's own key, which no arity or signature
    // can name — so it keeps winning over a same-arity sibling (spec 022, 024 FR-2); every other
    // construction resolves by arity, then by argument shape among the constructors of that arity
    // (spec 010, 024). `copy` is only ever `true` for a class that declares a copy constructor,
    // so a class without one is unchanged.
    let targets: ThinVec<String> = if copy {
        thin_vec![copy_ctor_name(class)]
    } else {
        ctx.overload_targets(&format!("{class}::{class}"), arity, arg_shapes)
            .into_iter()
            .collect()
    };
    let temp = ctx.allocator.next_temp();
    let ret = VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp));
    program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
        StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(targets),
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
    // The shapes are read off the syntax first (they do not depend on the lowering), so the
    // constructor edge below can pick among same-arity constructors (spec 024).
    let arg_shapes: Vec<ArgShape> = new_expr
        .child_by_field_name("arguments")
        .filter(|args| matches!(args.kind(), "argument_list" | "initializer_list"))
        .map(|args| ctx.arg_shapes(args, source))
        .unwrap_or_default();
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
        .add_variable(scope_view.sidx, obj.clone(), VarKind::Local);
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
        // Heap `new Type(args)` resolves by arity as it always has, and then by argument shape
        // among the constructors of that arity: copy-constructing from an object (`new Box(a)`)
        // is a form this slice does not cover, so `copy` is `false`.
        emit_ctor_call(
            ctx,
            program,
            scope_view,
            alloc_type,
            &obj,
            ctor_args,
            &arg_shapes,
            false,
        );
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
    let location = match &recv.exp {
        Exp::Variable(v) => RawPath::new(v.clone(), ThinVec::new()),
        Exp::AccessPath(ap) => super::raw_path_of(ap),
        _ => return Ok(()),
    };
    // A non-virtual destructor destroys exactly the static type: its own destructor, members
    // and bases, as at scope exit (spec 030).
    if !dtor_is_virtual(ctx, &recv.class) {
        destroy_object(ctx, program, scope_view, location, &recv.class, true, 0);
        return Ok(());
    }
    // A virtual destructor dispatches by CHA over the static type's subtree destructors
    // (`DirectCall <targets>(recv)`, the referent `*p` as the arg-0 by-ref receiver). The
    // defining class -- the nearest one in the base chain that declares a destructor -- has its
    // members and bases destroyed after it, whichever override ran.
    let Some(defining) = dtor_defining_class(ctx, &recv.class) else {
        return Ok(());
    };
    let targets: ThinVec<String> = dtor_cha_targets(ctx, &recv.class, &defining).into();
    emit_dtor_call(ctx, program, scope_view, targets, recv.exp);
    destroy_object(ctx, program, scope_view, location, &defining, false, 0);
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
    let Some(top) = ctx.dtor_frames.len().checked_sub(1) else {
        return Ok(());
    };
    drain_dtor_frame(ctx, program, scope_view, top);
    Ok(())
}

/// The C++ `scope_unwind` hook (spec 021): a `return`/`break`/`continue` leaves the top `frames`
/// scopes at once, so it runs the destructors of all of them — **innermost frame first**, reverse
/// construction order within each, exactly as C++ unwinds. Each object's call is spec 017's
/// (the single-target exact-type destructor with the object as the arg-0 `ByRef` receiver), so this
/// is [`cpp_scope_exit`] repeated over a range of frames instead of just the top one.
///
/// The frames are **read, not popped**: the compound walk that pushed them still pops them on its
/// way out, and its fall-through `scope_exit` emission lands in a different block — the diverging
/// statement's block is terminated here — so no object is destroyed twice on any single path. The
/// frames are empty for C, so nothing is emitted on the C path.
fn cpp_scope_unwind<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &ScopeView,
    frames: usize,
) -> anyhow::Result<(), Error> {
    let depth = ctx.dtor_frames.len();
    for idx in (depth.saturating_sub(frames)..depth).rev() {
        drain_dtor_frame(ctx, program, scope_view, idx);
    }
    Ok(())
}

/// Emit the destructor of every object recorded in destructor frame `idx`, in **reverse**
/// construction order (LIFO, matching C++). Shared by the two triggers that differ only in which
/// frames they cover: a scope's fall-through exit ([`cpp_scope_exit`], the top frame) and an early
/// exit ([`cpp_scope_unwind`], every frame the control transfer leaves). The frame is left in place.
fn drain_dtor_frame(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    idx: usize,
) {
    // Clone the frame so we can emit while mutating `ctx` (allocator/program); the frames
    // themselves are pushed and popped by `walk_compound_body`.
    let frame = ctx.dtor_frames[idx].clone();
    for (obj_name, class) in frame.into_iter().rev() {
        let location = ctx.build_access_path(
            &obj_name,
            Default::default(),
            scope_view,
            &mut program[scope_view.fidx].locals,
        );
        destroy_object(ctx, program, scope_view, location, &class, true, 0);
    }
}

/// Destroy the object at `location`, of exact class `class`, as C++ does (spec 030): its own
/// destructor body (when `own` and the class declares one), then its member objects in reverse
/// declaration order, then its base subobjects in reverse order -- each recursively. A tree
/// with no destructor anywhere emits nothing (unchanged from spec 014). A member object is
/// passed as the value loaded from `this.<member>`; nothing is written back, since the object
/// is gone. `depth` bounds a malformed (cyclic) hierarchy.
fn destroy_object(
    ctx: &mut Context<'_>,
    program: &mut Program,
    scope_view: &ScopeView,
    location: RawPath,
    class: &str,
    own: bool,
    depth: usize,
) {
    let Some(info) = ctx.classes.get(class) else {
        return;
    };
    if depth > 32 {
        return;
    }
    let (has_dtor, members, bases) = (
        info.has_dtor,
        info.member_objects.clone(),
        info.bases.clone(),
    );
    if own && has_dtor {
        let recv = Exp::access_path(ctx.emit_loads(program, scope_view, location.clone()));
        let target = format!("{class}::~{}", simple_class_name(class));
        emit_dtor_call(ctx, program, scope_view, thin_vec![target], recv);
    }
    for (member, member_class) in members.iter().rev() {
        let mut member_location = location.clone();
        member_location
            .fields
            .push(PathSegment::symbol(member.as_str()));
        destroy_object(
            ctx,
            program,
            scope_view,
            member_location,
            member_class,
            true,
            depth + 1,
        );
    }
    for base in bases.iter().rev() {
        destroy_object(
            ctx,
            program,
            scope_view,
            location.clone(),
            base,
            true,
            depth + 1,
        );
    }
}

/// The C++ `ctor_prologue` hook: lower a constructor's initializer list -- and the
/// initialization C++ performs implicitly -- before the body runs, in C++ order: base
/// subobjects, then members.
///
/// - Each base class with a constructor is constructed on the object itself (`@p0`; a base
///   subobject shares the derived object's member paths), with the arguments its `: Base(x)`
///   entry gives, or by default (spec 029).
/// - Each listed member that is an object of a class with a constructor is constructed through
///   it on `this.<member>` (spec 029). Any other listed member stores its value:
///   `member(expr)` is `this.<member> = <expr>`. That target is built directly as
///   `@p0.<member>` rather than resolved by name, so a parameter that shadows the member
///   (`Box(int v) : v(v)`) cannot redirect the left side away from `this`. Only the right side
///   goes through the shared expression lowering, where `v` resolves to the parameter `@pN`.
/// - Each member object the list leaves out is default-constructed (spec 029).
///
/// The pairs come from `ctor_member_inits`, plus the class's default member initializers for
/// the members it does not name (spec 028); a non-constructor gets an empty slice and emits
/// nothing.
fn cpp_emit_member_inits<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &mut ScopeView,
    source: &'a str,
    member_inits: &[(String, Node<'_>)],
) -> anyhow::Result<(), Error> {
    let this_member = |member: &str| {
        RawPath::new(
            VariableRef::new_parameter(0u32.into()),
            thin_vec![PathSegment::symbol(member)],
        )
    };
    let (bases, member_objects) = ctx
        .current_method_class
        .as_ref()
        .and_then(|class| ctx.classes.get(class))
        .map(|info| (info.bases.clone(), info.member_objects.clone()))
        .unwrap_or_default();
    let has_ctor =
        |ctx: &Context<'_>, class: &str| ctx.classes.get(class).is_some_and(|info| info.has_ctor);
    for base in &bases {
        let init = member_inits
            .iter()
            .find(|(name, _)| name == base)
            .map(|(_, node)| init_args(*node))
            .unwrap_or_default();
        if has_ctor(ctx, base) {
            let this = RawPath::new(VariableRef::new_parameter(0u32.into()), ThinVec::new());
            construct_subobject(ctx, program, scope_view, source, base, this, init)?;
        } else {
            for arg in init {
                ctx.flatten_expr(program, arg, source, scope_view)?;
            }
        }
    }
    for (member, init) in member_inits {
        if bases.contains(member) {
            continue;
        }
        let args = init_args(*init);
        let object_class = member_objects
            .iter()
            .find(|(m, _)| m == member)
            .map(|(_, class)| class.clone())
            .filter(|class| has_ctor(ctx, class));
        if let Some(class) = object_class {
            construct_subobject(
                ctx,
                program,
                scope_view,
                source,
                &class,
                this_member(member),
                args,
            )?;
            continue;
        }
        // A scalar or class-less member takes its value; `v()` value-initializes it.
        let mut value: Option<Exp> = None;
        for arg in args {
            let arg = ctx.flatten_expr(program, arg, source, scope_view)?;
            value = Some(match value {
                None => arg,
                Some(prev) => {
                    let temp = ctx.allocator.next_temp();
                    let target = ctx.build_access_path(
                        &temp,
                        Default::default(),
                        scope_view,
                        &mut program[scope_view.fidx].locals,
                    );
                    ctx.add_assign_to_program(program, scope_view, &target, &prev, Some(&arg));
                    Exp::Variable(target.base)
                }
            });
        }
        if let Some(value) = value {
            ctx.add_assign_to_program(program, scope_view, &this_member(member), &value, None);
        }
    }
    for (member, class) in &member_objects {
        if member_inits.iter().any(|(m, _)| m == member) || !has_ctor(ctx, class) {
            continue;
        }
        construct_subobject(
            ctx,
            program,
            scope_view,
            source,
            class,
            this_member(member),
            Vec::new(),
        )?;
    }
    Ok(())
}

/// Construct the subobject at `recv` -- a base subobject (the object itself) or a member
/// object (`this.<member>`) -- through `class`'s constructor with the argument expressions
/// `arg_nodes`. A single argument that is an object of `class` (or a subclass of it) runs the
/// copy constructor when the class declares one (spec 029).
fn construct_subobject<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    scope_view: &mut ScopeView,
    source: &'a str,
    class: &str,
    recv: RawPath,
    arg_nodes: Vec<Node<'_>>,
) -> anyhow::Result<(), Error> {
    let shapes: Vec<ArgShape> = arg_nodes
        .iter()
        .map(|arg| ctx.arg_shape(*arg, source))
        .collect();
    let copy = ctx
        .classes
        .get(class)
        .is_some_and(|info| info.has_copy_ctor)
        && matches!(shapes.as_slice(), [ArgShape::Object(c)] if ctx.class_is_a(c, class));
    let mut args: ThinVec<Exp> = ThinVec::new();
    for arg in arg_nodes {
        args.push(ctx.flatten_expr(program, arg, source, scope_view)?);
    }
    let location = recv.clone();
    let recv = Exp::access_path(ctx.emit_loads(program, scope_view, recv));
    emit_ctor_edge(
        ctx,
        program,
        scope_view,
        class,
        recv.clone(),
        args,
        &shapes,
        copy,
    );
    // A member object is named by a symbolic field, which the offset-only IR can only pass as
    // the value loaded from it. Store that value back, so what the constructor wrote into the
    // receiver lands on `this.<member>` (copy-in/copy-out).
    if !location.is_pathless() {
        ctx.add_assign_to_program(program, scope_view, &location, &recv, None);
    }
    Ok(())
}

/// The C++ grammar-shape adapters installed on the lowering [`Context`]. The first two
/// bridge the only C-subset node-shape divergences between the two grammars (per the
/// spec-002 triage); [`cpp_collect_methods`] adds C++ instance-method/constructor discovery,
/// which the top-level function query cannot see, and [`cpp_try_construct`] lowers object
/// construction at a declaration. The shared core is otherwise grammar-neutral.
/// The class of the object an expression names, when it is one — the neutral question every
/// operator dispatch below starts from.
///
/// An operator is only a method call if its left operand (or, for `f(args)`, its callee) is an
/// **object**: a local or parameter whose declared type is a class this unit knows. That is
/// exactly what [`Context::local_types`] records, so this is one map lookup — and the map is
/// empty for C, which is why no C assignment, subscript or call is ever rewritten.
fn operand_class(
    ctx: &Context<'_>,
    node: Node<'_>,
    source: &str,
    scope_view: &ScopeView,
) -> Option<String> {
    match node.kind() {
        // A pointer is not an object: `p + 1` is pointer arithmetic whatever `p` points to.
        "identifier" if ctx.local_pointers.contains(to_str(&node, source)) => None,
        "this" => None,
        "parenthesized_expression" => {
            operand_class(ctx, super::first_named_child(node)?, source, scope_view)
        }
        // An operator applied to an object, whose member operator returns `*this`, is that
        // object again: `s << a << b` chains on `s` (spec 031).
        "binary_expression" | "unary_expression" | "update_expression" => {
            let (operand, op) = match node.kind() {
                "binary_expression" => (
                    node.child_by_field_name("left")?,
                    node.child_by_field_name("operator")?,
                ),
                _ => (
                    node.child_by_field_name("argument")?,
                    node.child_by_field_name("operator")?,
                ),
            };
            let class = operand_class(ctx, operand, source, scope_view)?;
            let method = format!("operator{}", to_str(&op, source));
            let (_, returns_self) = ctx.resolve_method_class(&class, &method)?;
            returns_self.then_some(class)
        }
        // A local or parameter object, `*p`, `*this`, or a member object (spec 031).
        _ => ctx.object_class_of(node, source, scope_view),
    }
}

/// Lower an expression whose operator is a user-defined **method** — the C++
/// `GrammarHooks::operator_call` implementation (spec 023).
///
/// An overloaded operator is a method with a punctuation name, and the expression that applies
/// it is a call on the left operand: `a = x` is `a.operator=(x)`, `a += x` is
/// `a.operator+=(x)`, `a[i]` is `a.operator[](i)`, `f(args)` is `f.operator()(args)`. Each is
/// lowered through [`Context::emit_method_call`] — the very builder `recv.m(args)` uses — with
/// the object as the arg-0 (`ByRef`) receiver, so the member writes the operator body performs
/// flow back into the object and a member it returns flows out.
///
/// It answers `None`, leaving the shared arm's built-in lowering untouched, whenever the operand
/// is not a known class object or its class (and base chain) declares no such operator — which
/// covers every C assignment/subscript/call and every C++ one on a scalar, array, pointer or
/// operator-less class. It never partially lowers: the decision is made from the neutral
/// `local_types`/`classes` maps *before* any operand is evaluated, so an operand is flattened
/// exactly once on either path.
fn cpp_operator_call<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    node: Node<'_>,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<Option<Exp>, Error> {
    match node.kind() {
        "assignment_expression" => cpp_operator_assign(ctx, program, node, source, scope_view),
        "subscript_expression" => {
            // `a[i]` as a value. The index is read through the grammar hook, so this is the same
            // node the built-in lowering would have used as the subscript.
            let obj = node
                .child_by_field_name("argument")
                .expect("subscript_expression always has an argument");
            let Some(class) = operand_class(ctx, obj, source, scope_view) else {
                return Ok(None);
            };
            if ctx.resolve_method_class(&class, SUBSCRIPT_OP).is_none() {
                return Ok(None);
            }
            let Some(recv) = ctx.resolve_recv_obj(program, obj, source, scope_view)? else {
                return Ok(None);
            };
            let index_node = (ctx.hooks.subscript_index)(node);
            let index = ctx.flatten_expr(program, index_node, source, scope_view)?;
            let temp = ctx.allocator.next_temp();
            let out = ctx.emit_method_call(
                program,
                scope_view,
                source,
                &recv,
                SUBSCRIPT_OP,
                thin_vec![index],
                node.child_by_field_name("indices"),
                temp,
            )?;
            Ok(out.map(|o| o.value))
        }
        "compound_literal_expression" => {
            construct_temporary(ctx, program, node, source, scope_view)
        }
        // `a <op> b` with `a` an object: `a.operator<op>(b)` (spec 031).
        "binary_expression" => {
            let (Some(left), Some(right), Some(op)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
                node.child_by_field_name("operator"),
            ) else {
                return Ok(None);
            };
            let method = format!("operator{}", to_str(&op, source));
            if let Some(value) = member_operator_call(
                ctx,
                program,
                left,
                &method,
                &[right],
                false,
                source,
                scope_view,
            )? {
                return Ok(Some(value));
            }
            free_operator_call(
                ctx,
                program,
                &method,
                &[left, right],
                false,
                source,
                scope_view,
            )
        }
        // `<op> a` with `a` an object: `a.operator<op>()` -- `-a`, `!a`, `~a`, `*a` (spec 031).
        "unary_expression" | "pointer_expression" => {
            let (Some(arg), Some(op)) = (
                node.child_by_field_name("argument"),
                node.child_by_field_name("operator"),
            ) else {
                return Ok(None);
            };
            let op = to_str(&op, source);
            if node.kind() == "pointer_expression" && op != "*" {
                return Ok(None);
            }
            let method = format!("operator{op}");
            if let Some(value) =
                member_operator_call(ctx, program, arg, &method, &[], false, source, scope_view)?
            {
                return Ok(Some(value));
            }
            free_operator_call(ctx, program, &method, &[arg], false, source, scope_view)
        }
        // `++a` is `a.operator++()`; `a++` is `a.operator++(0)`, the dummy `int` that tells the
        // postfix overload apart (spec 031).
        "update_expression" => {
            let (Some(arg), Some(op)) = (
                node.child_by_field_name("argument"),
                node.child_by_field_name("operator"),
            ) else {
                return Ok(None);
            };
            let postfix = op.start_byte() > arg.start_byte();
            let method = format!("operator{}", to_str(&op, source));
            if let Some(value) =
                member_operator_call(ctx, program, arg, &method, &[], postfix, source, scope_view)?
            {
                return Ok(Some(value));
            }
            free_operator_call(ctx, program, &method, &[arg], postfix, source, scope_view)
        }
        "call_expression" => {
            // `f(args)` where `f` names an object, not a function: the call operator. A callee
            // that is anything else — every function call, in C and C++ alike — misses in
            // `local_types` and is left to the ordinary call lowering.
            let callee = node
                .child_by_field_name("function")
                .expect("call_expression always has a function");
            // A named or functional cast is a built-in operator too, spelled as a call (spec 025).
            if let Some(operand) = cast_call_operand(ctx, node, source, scope_view) {
                return ctx
                    .flatten_expr(program, operand, source, scope_view)
                    .map(Some);
            }
            if let Some(object) = construct_temporary(ctx, program, node, source, scope_view)? {
                return Ok(Some(object));
            }
            let Some(class) = operand_class(ctx, callee, source, scope_view) else {
                return Ok(None);
            };
            if ctx.resolve_method_class(&class, CALL_OP).is_none() {
                return Ok(None);
            }
            let Some(recv) = ctx.resolve_recv_obj(program, callee, source, scope_view)? else {
                return Ok(None);
            };
            let arg_node = node
                .child_by_field_name("arguments")
                .expect("call_expression always has arguments");
            let args = ctx.collect_arguments(program, arg_node, source, scope_view)?;
            let temp = ctx.allocator.next_temp();
            let out = ctx.emit_method_call(
                program,
                scope_view,
                source,
                &recv,
                CALL_OP,
                args,
                Some(arg_node),
                temp,
            )?;
            Ok(out.map(|o| o.value))
        }
        _ => Ok(None),
    }
}

/// The class and argument list of a temporary: `C(args)` for a known class `C` that no value
/// in scope shadows, or `C{args}` for one that declares a constructor (a constructor-less
/// `C{..}` is an aggregate, which the shared compound-literal lowering initializes). `None`
/// for every other expression, and for all of C, where `classes` is empty.
fn temporary_of<'t>(
    ctx: &Context<'_>,
    node: Node<'t>,
    source: &str,
    scope_view: &ScopeView,
) -> Option<(String, Node<'t>)> {
    let (type_node, args, braced) = match node.kind() {
        "call_expression" => (
            node.child_by_field_name("function")?,
            node.child_by_field_name("arguments")?,
            false,
        ),
        "compound_literal_expression" => (
            node.child_by_field_name("type")?,
            node.child_by_field_name("value")?,
            true,
        ),
        _ => return None,
    };
    if !matches!(
        type_node.kind(),
        "identifier" | "type_identifier" | "qualified_identifier"
    ) {
        return None;
    }
    let name = to_str(&type_node, source);
    let info = ctx.classes.get(name)?;
    if (braced && !info.has_ctor) || ctx.names_a_value(name, scope_view) {
        return None;
    }
    Some((name.to_string(), args))
}

/// Lower the temporary `C(args)` / `C{args}` (spec 027) and return the object, or `None` when
/// `node` is not one (see [`temporary_of`]).
///
/// A temporary is a fresh local object, constructed exactly as a declared one is: the
/// arity- and shape-resolved constructor edge ([`emit_ctor_call`]) with the object as the
/// arg-0 receiver, or the copy constructor for `C(c)` from an object of `C`. Its value is that
/// object, so `take(C(x))`, `return C(x)` and `C(x).m()` all see what the constructor stored.
/// A class with no constructor yields the fresh object after evaluating the arguments for
/// their effects.
///
/// The temporary joins the enclosing scope's destructor frame. C++ destroys it at the end of
/// its full expression, which is earlier, but nothing can reach the object in between, so the
/// members its destructor reads are the same either way.
fn construct_temporary<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    node: Node<'_>,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<Option<Exp>, Error> {
    let Some((class, args_node)) = temporary_of(ctx, node, source, scope_view) else {
        return Ok(None);
    };
    let (has_ctor, has_copy_ctor) = ctx
        .classes
        .get(&class)
        .map_or((false, false), |info| (info.has_ctor, info.has_copy_ctor));
    let mut cursor = args_node.walk();
    let arg_nodes: Vec<Node<'_>> = args_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect();
    let copy = has_copy_ctor
        && arg_nodes.len() == 1
        && names_class_object(ctx, source, arg_nodes[0], &class);
    let shapes: Vec<ArgShape> = arg_nodes
        .iter()
        .map(|arg| ctx.arg_shape(*arg, source))
        .collect();
    let mut args: ThinVec<Exp> = ThinVec::new();
    for arg in arg_nodes {
        args.push(ctx.flatten_expr(program, arg, source, scope_view)?);
    }

    let obj = ctx.allocator.next_temp();
    ctx.scope_tree
        .add_variable(scope_view.sidx, obj.clone(), VarKind::Local);
    ctx.local_types.insert(obj.clone(), class.clone());
    ctx.local_pointers.remove(&obj);
    if let Some(frame) = ctx.dtor_frames.last_mut() {
        frame.push((obj.clone(), class.clone()));
    }
    if has_ctor {
        emit_ctor_call(ctx, program, scope_view, &class, &obj, args, &shapes, copy);
    }
    let path = ctx.build_access_path(
        &obj,
        Default::default(),
        scope_view,
        &mut program[scope_view.fidx].locals,
    );
    Ok(Some(Exp::access_path(
        ctx.emit_loads(program, scope_view, path),
    )))
}

/// The operand of a cast the C++ grammar spells as a call, or `None` for any other call.
///
/// tree-sitter-cpp has no node for either shape, so the ordinary lowering would call a function
/// nothing defines (`static_cast<long>`, `long`) and drop the operand's taint. Like a C
/// `cast_expression`, both are value-preserving for taint, so the cast's value is its one
/// operand's (spec 025):
/// - a named cast, `static_cast<T>(e)` / `reinterpret_cast` / `const_cast` / `dynamic_cast`,
///   whose callee is a `template_function` named by the keyword. A `dynamic_cast` that fails
///   yields null, which carries nothing; reading it as the operand over-approximates.
/// - a functional cast `T(e)` to a non-class type: `int(e)` (a `primitive_type` callee),
///   `long(e)` / `unsigned(e)` (the grammar reads a lone size keyword as an `identifier`), or a
///   typedef name the unit declares that no value shadows. `C(e)` for a class `C` is a
///   construction, not a conversion, and is not answered here.
fn cast_call_operand<'t>(
    ctx: &Context<'_>,
    call: Node<'t>,
    source: &str,
    scope_view: &ScopeView,
) -> Option<Node<'t>> {
    let callee = call.child_by_field_name("function")?;
    let is_cast = match callee.kind() {
        "template_function" => callee.child_by_field_name("name").is_some_and(|name| {
            matches!(
                to_str(&name, source),
                "static_cast" | "reinterpret_cast" | "const_cast" | "dynamic_cast"
            )
        }),
        "primitive_type" | "sized_type_specifier" => true,
        "identifier" | "type_identifier" => {
            let name = to_str(&callee, source);
            matches!(name, "long" | "short" | "unsigned" | "signed")
                || (ctx.type_names.contains(name)
                    && !ctx.classes.contains_key(name)
                    && !ctx.names_a_value(name, scope_view))
        }
        _ => false,
    };
    if !is_cast {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut operands = args
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment");
    let operand = operands.next()?;
    operands.next().is_none().then_some(operand)
}

/// Lower an operator applied to an object as a call of its class's member `method` (spec
/// 031): `obj` is the receiver, `operands` the remaining operands, and `dummy_int` appends the
/// `0` a postfix `++`/`--` passes. Declines -- before evaluating anything -- unless `obj` is an
/// object whose class (or a base) declares `method`, so a built-in operator, and every C one,
/// lowers as before. An overload set of `method` resolves by arity; the operand shapes are not
/// read, so same-arity overloads all become targets.
#[allow(clippy::too_many_arguments)]
fn member_operator_call<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    obj: Node<'_>,
    method: &str,
    operands: &[Node<'_>],
    dummy_int: bool,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<Option<Exp>, Error> {
    let Some(class) = operand_class(ctx, obj, source, scope_view) else {
        return Ok(None);
    };
    if ctx.resolve_method_class(&class, method).is_none() {
        return Ok(None);
    }
    let recv = match obj.kind() {
        // A chained operator (`s << a` in `s << a << b`): its value is the object it returned,
        // which `emit_method_call` aliased to the receiver.
        "binary_expression" | "unary_expression" | "update_expression" => super::RecvObj {
            exp: ctx.flatten_expr(program, obj, source, scope_view)?,
            class,
            write_back: None,
        },
        _ => match ctx.resolve_recv_obj(program, obj, source, scope_view)? {
            Some(recv) => recv,
            None => return Ok(None),
        },
    };
    let mut args: ThinVec<Exp> = ThinVec::new();
    for operand in operands {
        args.push(ctx.flatten_expr(program, *operand, source, scope_view)?);
    }
    if dummy_int {
        args.push(Exp::new_str("0"));
    }
    let temp = ctx.allocator.next_temp();
    let out = ctx.emit_method_call(program, scope_view, source, &recv, method, args, None, temp)?;
    Ok(out.map(|o| o.value))
}

/// Lower an operator on a class object as a call of a **free** `operator<op>` (spec 032), when
/// the unit defines one of that arity: `a + b` is `operator+(a, b)`, `-a` is `operator-(a)`, and a
/// postfix `a++` passes the dummy `0`. Declines -- before evaluating anything -- unless some
/// operand is a class object (a built-in operator on scalars or pointers is never rewritten) and
/// a free operator of that name and arity exists. Same-arity candidates resolve by the operands'
/// shapes ([`Context::overload_targets`]). Only file-scope operators are candidates: a namespaced
/// one would be found by argument-dependent lookup, which is not modeled.
fn free_operator_call<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    method: &str,
    operands: &[Node<'_>],
    dummy_int: bool,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<Option<Exp>, Error> {
    if !operands
        .iter()
        .any(|operand| operand_class(ctx, *operand, source, scope_view).is_some())
    {
        return Ok(None);
    }
    let arity = operands.len() + usize::from(dummy_int);
    if !ctx
        .overloads
        .get(method)
        .is_some_and(|sigs| sigs.iter().any(|sig| sig.arity == arity))
    {
        return Ok(None);
    }
    let mut shapes: Vec<ArgShape> = operands
        .iter()
        .map(|operand| ctx.arg_shape(*operand, source))
        .collect();
    let mut args: ThinVec<Exp> = ThinVec::new();
    for operand in operands {
        args.push(ctx.flatten_expr(program, *operand, source, scope_view)?);
    }
    if dummy_int {
        shapes.push(ArgShape::Other);
        args.push(Exp::new_str("0"));
    }
    let targets: ThinVec<String> = ctx
        .overload_targets(method, arity, &shapes)
        .into_iter()
        .collect();
    let temp = ctx.allocator.next_temp();
    let ret = VariableRef::new_local_idx(program[scope_view.fidx].locals.get_or_intern(&temp));
    program[scope_view.fidx].blocks[scope_view.blidx].push_back(Statement::new_kind(
        StatementKind::CallAssign {
            style: CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(targets),
            },
            rets: thin_vec![ret.clone()],
            args,
        },
    ));
    Ok(Some(Exp::Variable(ret)))
}

/// The method name of the subscript operator, `a[i]`.
const SUBSCRIPT_OP: &str = "operator[]";
/// The method name of the call operator, `f(args)`.
const CALL_OP: &str = "operator()";

/// The assignment half of [`cpp_operator_call`]: `a = x` / `a += x` on an object whose class
/// declares that operator, and the write-through `a[i] = x` of a reference-returning
/// `operator[]`.
///
/// A **plain or compound assignment to an object** is a call taking only the right-hand side:
/// `Acc& operator+=(int x){ v = v + x; … }` reads the old value itself, through `this`, so the
/// built-in compound lowering's second operand has no place here.
///
/// A **write through a subscript** (`a[0] = x`, FR-4) is two steps: `operator[]` runs, and the
/// store lands on the location the reference it returned denotes. Only the shape this slice
/// models is recognized — an `operator[]` whose body returns a bare data member
/// ([`ClassInfo::returns_member`]) — and the store then goes to `<object>.<member>`, which is
/// where a later `a.get()` reads. Any other reference-returning body declines *before* anything
/// is emitted, so the expression keeps exactly the built-in lowering it has today.
fn cpp_operator_assign<'a>(
    ctx: &mut Context<'a>,
    program: &mut Program,
    node: Node<'_>,
    source: &'a str,
    scope_view: &mut ScopeView,
) -> anyhow::Result<Option<Exp>, Error> {
    let left = node
        .child_by_field_name("left")
        .expect("assignment_expression always has a left");
    let right = node
        .child_by_field_name("right")
        .expect("assignment_expression always has a right");
    let op = node
        .child_by_field_name("operator")
        .map_or("=", |o| to_str(&o, source));

    if let Some(class) = operand_class(ctx, left, source, scope_view) {
        // `a = x` / `a += x`: the operator named by the assignment's own spelling.
        let method = format!("operator{op}");
        if ctx.resolve_method_class(&class, &method).is_none() {
            return Ok(None);
        }
        let Some(recv) = ctx.resolve_recv_obj(program, left, source, scope_view)? else {
            return Ok(None);
        };
        let rhs = ctx.flatten_expr(program, right, source, scope_view)?;
        let temp = ctx.allocator.next_temp();
        let out = ctx.emit_method_call(
            program,
            scope_view,
            source,
            &recv,
            &method,
            thin_vec![rhs],
            None,
            temp,
        )?;
        return Ok(out.map(|o| o.value));
    }

    // `a[i] = x` — a plain assignment through a reference-returning `operator[]`.
    if op != "=" || left.kind() != "subscript_expression" {
        return Ok(None);
    }
    let obj = left
        .child_by_field_name("argument")
        .expect("subscript_expression always has an argument");
    let Some(class) = operand_class(ctx, obj, source, scope_view) else {
        return Ok(None);
    };
    let Some((defining, _)) = ctx.resolve_method_class(&class, SUBSCRIPT_OP) else {
        return Ok(None);
    };
    // The member the returned reference denotes. Without it the location written is unknown, so
    // decline and let the built-in store lowering stand rather than invent a target.
    let Some(member) = ctx
        .classes
        .get(&defining)
        .and_then(|info| info.returns_member.get(SUBSCRIPT_OP))
        .cloned()
    else {
        return Ok(None);
    };
    let Some(recv) = ctx.resolve_recv_obj(program, obj, source, scope_view)? else {
        return Ok(None);
    };
    let index_node = (ctx.hooks.subscript_index)(left);
    let index = ctx.flatten_expr(program, index_node, source, scope_view)?;
    let temp = ctx.allocator.next_temp();
    ctx.emit_method_call(
        program,
        scope_view,
        source,
        &recv,
        SUBSCRIPT_OP,
        thin_vec![index],
        left.child_by_field_name("indices"),
        temp,
    )?;
    let rhs = ctx.flatten_expr(program, right, source, scope_view)?;
    // The store goes to the member of *this* object — the storage the returned reference names.
    let mut target = ctx.build_access_path(
        to_str(&obj, source),
        Default::default(),
        scope_view,
        &mut program[scope_view.fidx].locals,
    );
    target.fields.push(PathSegment::symbol(&member));
    ctx.add_assign_to_program(program, scope_view, &target, &rhs, None);
    Ok(Some(rhs))
}

pub(super) const CPP_HOOKS: GrammarHooks = GrammarHooks {
    condition_expr: cpp_condition_expr,
    subscript_index: cpp_subscript_index,
    collect_aux: cpp_collect_methods,
    // Recognize and lower object construction (`Box b(args)` and the copy/brace variants)
    // to a constructor `DirectCall`. Only invoked by the shared walker for a declaration
    // whose type is a known class, so it is inert on the C path.
    construct: cpp_try_construct,
    // Copy-construct a by-value class argument at the call site: passing an object to a
    // `void take(Box b)` parameter runs `Box`'s copy constructor in C++, so the callee receives
    // the copy that constructor built. Driven by the neutral per-function by-value record (empty
    // for C), so no C call is rewritten.
    copy_args: cpp_copy_args,
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
    // Emit the same destructors on the paths that leave a scope *early*: a `return` unwinds every
    // frame of the function, a `break`/`continue` every frame back to the loop body, innermost
    // first, immediately before the control transfer. Same neutral `dtor_frames` stack, so the C
    // path emits nothing.
    scope_unwind: cpp_scope_unwind,
    // Emit a constructor's member-initializer list (`Box(int x) : v(x)`) as `this.<member> =
    // <expr>` stores, before the body. Empty for every non-constructor, so the hook is inert
    // outside constructors and never fires on the C path.
    ctor_prologue: cpp_emit_member_inits,
    // The definitions the shared definition pass must leave to `cpp_collect_methods` /
    // `cpp_collect_namespaced_functions`: class members, namespaced functions, and every
    // definition whose function name is not a plain `identifier` (an out-of-line member).
    aux_owns_definition: cpp_aux_owns_definition,
    // Lower an overloaded operator as the method it is: `a = x` / `a += x` / `a[i]` / `f(args)`
    // on an object whose class declares that operator becomes a `DirectCall Class::operator…`
    // with the object as the arg-0 receiver. Declines for every operand that is not a known class
    // object, so the built-in lowering stands for all of C and for every scalar/array/pointer.
    operator_call: cpp_operator_call,
    arrow_operator: |ctx, program, object, source, scope_view| {
        member_operator_call(
            ctx,
            program,
            object,
            "operator->",
            &[],
            false,
            source,
            scope_view,
        )
    },
};

/// Parse the C++ source in `source` into a CTADL IR program.
///
/// Signature-identical to [`super::parse_c_program`]: returns the lowered [`Program`],
/// whether tree-sitter reported a syntax error, and the marked-up IR dump.
pub fn parse_cpp_program(source: &str) -> anyhow::Result<(Program, bool, String), Error> {
    let units = [TranslationUnit::new(None, source.to_string())];
    let lowered = lower_units(
        &units,
        tree_sitter_cpp::LANGUAGE.into(),
        CPP_HOOKS,
        false,
        false,
        true,
    )?;
    Ok((lowered.program, lowered.has_error, lowered.marked_up))
}
