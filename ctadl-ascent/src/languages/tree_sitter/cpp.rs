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
use tree_sitter::{Node, Parser};

use super::{Context, GrammarHooks, markup};
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

/// The C++ grammar-shape adapters installed on the lowering [`Context`]. Everything the
/// C++ frontend needs that differs from C lives behind these two hooks (per the spec-002
/// triage, the *only* C-subset divergences between the two grammars); the shared core is
/// otherwise grammar-neutral.
pub(super) const CPP_HOOKS: GrammarHooks = GrammarHooks {
    condition_expr: cpp_condition_expr,
    subscript_index: cpp_subscript_index,
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
