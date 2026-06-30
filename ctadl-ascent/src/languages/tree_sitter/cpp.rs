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

use ctadl_ir::mir::Program;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, QueryCursor};

use super::{ClassInfo, Context, GrammarHooks, MatchExtractor, markup, to_str};
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

/// Discover C++ instance methods — inline *and* out-of-line — and lower them through the
/// shared core.
///
/// The top-level `function_definition` query in `Context::collect_functions` only matches
/// definitions whose name is a plain `identifier`; a member function's name is either a
/// `field_identifier` nested inside a `class_specifier`/`struct_specifier` (inline body) or
/// a `qualified_identifier` `Class::m` at top level (out-of-line body) — both invisible to
/// it. This hook (installed only for C++) finds each class, records its data members and
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
            body: (field_declaration_list) @class.body)
         (class_specifier name: (type_identifier) @class.name
            body: (field_declaration_list) @class.body)]
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
    }
    let mut methods: Vec<MethodDef<'_>> = Vec::new();

    // Phase 1: scan classes, populate `ctx.classes`, collect method bodies. The query
    // cursor borrows the tree (not `ctx`), so populating `ctx.classes` here is fine.
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, root, source.as_bytes());
    while let Some(m) = it.next() {
        let extract = MatchExtractor::new(&query, m);
        let class_name = to_str(&extract.get("class.name")?, source).to_string();
        let body = extract.get("class.body")?;

        let mut info = ClassInfo::default();
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            match child.kind() {
                "field_declaration" => {
                    let mut dc = child.walk();
                    for declr in child.children_by_field_name("declarator", &mut dc) {
                        if let Some(name) = member_name(declr, source) {
                            info.members.insert(name.to_string());
                        }
                    }
                }
                "function_definition" => {
                    // A member function: declarator is a `function_declarator` whose own
                    // declarator is the method-name `field_identifier`. Out-of-line defs,
                    // constructors, etc. don't match this shape and are skipped (later specs).
                    let Some(declr) = child.child_by_field_name("declarator") else {
                        continue;
                    };
                    if declr.kind() != "function_declarator" {
                        continue;
                    }
                    let (Some(name_node), Some(params), Some(method_body)) = (
                        declr.child_by_field_name("declarator"),
                        declr.child_by_field_name("parameters"),
                        child.child_by_field_name("body"),
                    ) else {
                        continue;
                    };
                    if name_node.kind() != "field_identifier" {
                        continue;
                    }
                    let name = to_str(&name_node, source).to_string();
                    let void = child
                        .child_by_field_name("type")
                        .is_some_and(|t| to_str(&t, source).eq_ignore_ascii_case("void"));
                    info.methods.insert(name.clone());
                    methods.push(MethodDef {
                        class: class_name.clone(),
                        name,
                        params,
                        body: method_body,
                        void,
                    });
                }
                _ => {}
            }
        }
        ctx.classes.insert(class_name, info);
    }
    drop(it);
    drop(cursor);

    // Phase 1.5: discover out-of-line method definitions — a top-level `function_definition`
    // whose declarator names a `qualified_identifier` (`ret Class::m(params){…}`). The
    // top-level `function_definition` query in `Context::collect_functions` only matches a
    // plain `identifier` name, so these are invisible to it and must be found here. We gather
    // (class, method, params, body) first — the cursor borrows the tree, not `ctx`, so reading
    // `ctx.classes` to filter to already-declared classes is fine — then register the method
    // names and queue the bodies after the cursor drops.
    let ool_query = ctx.compile_query(
        r#"
        (function_definition
            declarator: (function_declarator
                declarator: (qualified_identifier
                    scope: (namespace_identifier) @class
                    name: (identifier) @method)
                parameters: (parameter_list) @params)
            body: (compound_statement) @body) @def
        "#,
    );
    let mut out_of_line: Vec<MethodDef<'_>> = Vec::new();
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
            let params = extract.get("params")?;
            let body = extract.get("body")?;
            let void = extract
                .get("def")?
                .child_by_field_name("type")
                .is_some_and(|t| to_str(&t, source).eq_ignore_ascii_case("void"));
            out_of_line.push(MethodDef {
                class,
                name,
                params,
                body,
                void,
            });
        }
    }
    // Register each out-of-line method's name on its class (so `recv.m(…)` dispatches even
    // though the body lives outside the class body), then queue it for lowering alongside the
    // inline methods.
    for md in out_of_line {
        if let Some(info) = ctx.classes.get_mut(&md.class) {
            info.methods.insert(md.name.clone());
        }
        methods.push(md);
    }

    // Phase 2a: register a function index for every method, so a method calling another
    // method (or a top-level body calling one) resolves it regardless of definition order.
    for md in &methods {
        let qualified = format!("{}::{}", md.class, md.name);
        ctx.functions
            .entry(qualified)
            .or_insert_with(|| program.new_function());
    }
    // Phase 2b: lower each method body with an implicit `this` of its class.
    for md in methods {
        let qualified = format!("{}::{}", md.class, md.name);
        ctx.lower_function(
            source,
            program,
            global_sidx,
            &qualified,
            md.void,
            md.params,
            md.body,
            Some(&md.class),
        )?;
    }
    Ok(())
}

/// The C++ grammar-shape adapters installed on the lowering [`Context`]. The first two
/// bridge the only C-subset node-shape divergences between the two grammars (per the
/// spec-002 triage); [`cpp_collect_methods`] adds C++ instance-method discovery, which the
/// top-level function query cannot see. The shared core is otherwise grammar-neutral.
pub(super) const CPP_HOOKS: GrammarHooks = GrammarHooks {
    condition_expr: cpp_condition_expr,
    subscript_index: cpp_subscript_index,
    collect_aux: cpp_collect_methods,
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
