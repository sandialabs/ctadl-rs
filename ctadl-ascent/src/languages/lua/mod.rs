//! Lua language frontend (tree-sitter).
//!
//! This module parses Lua source with the tree-sitter Lua grammar and is the
//! entry point that [`crate::cli::import`] dispatches to for `ctadl import -l lua`.
//!
//! # Status
//!
//! The parsing and plumbing are wired up: `-l lua` (and the `.lua` extension via
//! autodetection) route here, the source is parsed with
//! [`tree_sitter_lua::LANGUAGE`], and syntax errors are surfaced. The translation
//! from the Lua AST into CTADL IR ([`ProgramInfo`]) is **not yet implemented** --
//! [`import_lua`] returns an empty program, so taint queries find no flows.
//!
//! The Lua regression cases under `nightly/tests/lua/` are written against the
//! frontend this module is intended to become (direct flows, table/field
//! sensitivity, closures, varargs, and metatable-based OOP), so they are expected
//! to fail until the AST-to-IR lowering below is filled in.
//!
//! The intended lowering mirrors the C tree-sitter frontend
//! ([`crate::languages::tree_sitter`]): walk the parse tree, build a
//! [`Program`](ctadl_ir::mir::Program) of functions and basic blocks, and lower
//! Lua assignments, calls, table constructors, field/index accesses, and
//! metatable dispatch (`setmetatable`/`__index`) into IR statements with access
//! paths.

use std::path::Path;

use ctadl_ir::mir::{Program, ProgramInfo};
use tree_sitter::{Node, Parser};

use crate::error::Error;

/// Parse a Lua source file and translate it into CTADL IR.
///
/// Today this validates that the file parses under the tree-sitter Lua grammar
/// and reports a summary of what it found, then returns an empty [`ProgramInfo`].
/// The AST-to-IR lowering is still to be written (see the module docs); until it
/// is, the taint analysis has no statements to work with and the Lua regression
/// cases fail.
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

    let function_count = count_functions(root);
    log::info!(
        "lua frontend: parsed {} ({} function definition(s)); \
         AST-to-IR lowering is not yet implemented, so the imported program is empty",
        path.display(),
        function_count,
    );

    // TODO: lower the Lua AST into `program`. See the module documentation for the
    // constructs the regression suite exercises.
    let program = Program::default();

    Ok(ProgramInfo {
        program,
        ..Default::default()
    })
}

/// Count Lua function definitions in the parse tree.
///
/// Covers both named declarations (`function f() end`, `local function f() end`,
/// `function T:m() end`) and anonymous function expressions (`function() end`),
/// which tree-sitter Lua exposes as `function_declaration` and
/// `function_definition` nodes respectively. This is only used for the import log
/// today; it also confirms the grammar loaded and the tree walks as expected.
fn count_functions(root: Node<'_>) -> usize {
    let mut count = 0;
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "function_declaration" || kind == "function_definition" {
            count += 1;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    count
}
