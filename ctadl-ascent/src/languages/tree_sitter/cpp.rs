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
//! Coverage is intentionally tiny for Milestone 1 — a value-returning function, a local
//! declaration with a call initializer, a call statement, and `return`. Anything else may
//! error; that error is the Milestone 2 backlog (every unsupported construct becomes a
//! `frontend-error` ticket), not a defect.

use ctadl_ir::mir::Program;
use tree_sitter::Parser;

use super::{Context, markup};
use crate::error::Error;

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
    let mut program = Program::default();
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter failed to parse");
    ctx.parse(source, &tree, &mut program)?;
    let marked_up = markup(&program, &ctx);
    Ok((program, tree.root_node().has_error(), marked_up))
}
