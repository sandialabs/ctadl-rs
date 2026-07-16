use crate::error::PhpReaderError;
use ctadl_ir::mir::call::VirtualMethodTable;
use ctadl_ir::mir::{Program, ProgramInfo};
use tree_sitter::{Parser, Tree};

pub mod error;
pub mod evaluator;
pub mod lower;

pub fn parse_php(source_code: &str) -> Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_php::LANGUAGE_PHP;
    parser
        .set_language(&language.into())
        .expect("Error loading PHP grammar");
    parser.parse(source_code, None).expect("Error parsing PHP")
}

fn debug_print_tree(tree: &Tree, source_code: &str) {
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        let field_name = cursor.field_name();
        let indent = "  ".repeat(depth);
        let field_prefix = match field_name {
            Some(name) => format!("{name}: "),
            None => String::new(),
        };

        if node.child_count() == 0 {
            let text = &source_code[node.start_byte()..node.end_byte()];
            log::trace!("{}|-- {}{} {:?}", indent, field_prefix, node.kind(), text);
        } else {
            log::trace!("{}|-- {}{}", indent, field_prefix, node.kind());
        }

        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }

        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
            depth -= 1;
        }
    }
}

pub fn lower_php(source_code: &str, file_name: &str) -> Result<ProgramInfo, PhpReaderError> {
    let mut program_info = ProgramInfo {
        program: Program::default(),
        vmt: VirtualMethodTable::new_php(),
        source_info: source_info::SourceInfo::default(),
    };
    lower_php_into(source_code, file_name, file_name, &mut program_info)?;
    Ok(program_info)
}

/// Main lowering entry point. Lowers PHP file into IR.
pub fn lower_php_into(
    source_code: &str,
    file_name: &str,
    source_path: &str,
    program_info: &mut ProgramInfo,
) -> Result<(), PhpReaderError> {
    let tree = parse_php(source_code);
    if log::log_enabled!(log::Level::Trace) {
        debug_print_tree(&tree, source_code);
    }
    if tree.root_node().has_error() {
        log::warn!("Parse error, skipping: {}", file_name);
        return Err(PhpReaderError::ParseError {
            offset: tree.root_node().start_byte(),
        });
    }

    let mut lowerer = lower::Lowerer::new(source_code, file_name, source_path, program_info);
    lowerer.lower(&tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctadl_ir::graph::{DirectedGraph, StartNode, Successors};
    use ctadl_ir::index::idx::Idx;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::Path;

    /// Parse every PHP case in the nightly regression corpus, asserting only that
    /// the grammar accepts it.
    ///
    /// The corpus lives in `nightly/tests/php` because the end-to-end taint checks
    /// over those same files are xtask regression cases. This is the cheap
    /// parser-level half: it runs in `cargo test` on every PR, where the nightly
    /// suite does not, and it picks up new cases as they are added there.
    #[test]
    fn test_parse_taint_cases() {
        let test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../nightly/tests/php");
        let entries = fs::read_dir(&test_dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", test_dir.display()));

        let mut parsed = 0;
        for entry in entries {
            let entry = entry.expect("Failed to read entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("php") {
                let content = fs::read_to_string(&path).expect("Failed to read file");
                let tree = parse_php(&content);
                assert!(!tree.root_node().has_error(), "Parse error in {:?}", path);
                parsed += 1;
            }
        }
        // A corpus that silently emptied out (moved again, say) would leave this
        // test passing while parsing nothing.
        assert!(parsed > 0, "no PHP cases found in {}", test_dir.display());
    }

    #[test]
    fn test_lower_nested_subscript() {
        use ctadl_ir::mir::{Exp, FieldAccess, StatementKind, Variable};
        let source = r#"
            <?php
            $a = $_GET['input']['more'];
        "#;
        let program_info = lower_php(source, "nested.php").expect("Lowering failed");
        let main = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == "__php_main__::nested.php")
            .expect("Expected to find main function");

        let mut found = false;
        for bb in main.blocks.iter() {
            for stmt in &bb.statements {
                if let StatementKind::Assign { dest, sources } = &stmt.kind {
                    if dest.variable.local() == Some("a") {
                        if let Some(Exp::AccessPath(ap)) = sources.first() {
                            if matches!(*ap.variable_ref.variable, Variable::GlobalHeap) {
                                if ap.path.fields.len() == 3 {
                                    let f1 = match &ap.path.fields[0] {
                                        FieldAccess::Symbol(s) => s.to_string(),
                                        _ => String::new(),
                                    };
                                    let f2 = match &ap.path.fields[1] {
                                        FieldAccess::Symbol(s) => s.to_string(),
                                        _ => String::new(),
                                    };
                                    let f3 = match &ap.path.fields[2] {
                                        FieldAccess::Symbol(s) => s.to_string(),
                                        _ => String::new(),
                                    };
                                    if f1 == "_GET" && f2 == "input" && f3 == "more" {
                                        found = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "Expected to find assignment $a = $_GET['input']['more']"
        );
    }

    #[test]
    fn test_lower_get_input() {
        use ctadl_ir::mir::{Exp, FieldAccess, StatementKind, Variable};
        let source = r#"
            <?php
            $a = $_GET['input'];
        "#;
        let program_info = lower_php(source, "get.php").expect("Lowering failed");
        let main = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == "__php_main__::get.php")
            .expect("Expected to find main function");

        let mut found = false;
        for bb in main.blocks.iter() {
            for stmt in &bb.statements {
                if let StatementKind::Assign { dest, sources } = &stmt.kind {
                    if dest.variable.local() == Some("a") {
                        if let Some(Exp::AccessPath(ap)) = sources.first() {
                            if matches!(*ap.variable_ref.variable, Variable::GlobalHeap) {
                                if ap.path.fields.len() == 2 {
                                    let f1 = match &ap.path.fields[0] {
                                        FieldAccess::Symbol(s) => s.to_string(),
                                        _ => String::new(),
                                    };
                                    let f2 = match &ap.path.fields[1] {
                                        FieldAccess::Symbol(s) => s.to_string(),
                                        _ => String::new(),
                                    };
                                    if f1 == "_GET" && f2 == "input" {
                                        found = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(found, "Expected to find assignment $a = $_GET['input']");
    }

    #[test]
    fn test_lower_php_basic() {
        let source = r#"
            <?php
            function test() {
                $a = $_GET['input'];
                echo $a;
            }
        "#;
        let program_info = lower_php(source, "test.php").expect("Lowering failed");
        let program = program_info.program;

        let has_test_fn = program.functions.functions.iter().any(|f| f.name == "test");
        assert!(has_test_fn, "Expected to find function 'test'");

        let has_main = program
            .functions
            .functions
            .iter()
            .any(|f| f.name == "__php_main__::test.php");
        assert!(has_main, "Expected to find main function");
    }

    #[test]
    fn test_lower_php_populates_source_info() {
        let source = r#"
            <?php
            echo $_GET['name'];
        "#;
        let program_info = lower_php(source, "source-info.php").expect("Lowering failed");
        assert!(!program_info.source_info.artifacts.is_empty());
        assert!(!program_info.source_info.files.is_empty());
        assert!(!program_info.source_info.spans.is_empty());
        assert!(!program_info.source_info.file_spans.is_empty());

        let main = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == "__php_main__::source-info.php")
            .expect("Expected to find main function");
        let stmt_has_span = main
            .blocks
            .iter()
            .flat_map(|bb| bb.statements.iter())
            .any(|stmt| stmt.source_info.span_id != source_info::NO_SPAN);
        assert!(
            stmt_has_span,
            "expected at least one statement with source info"
        );
    }

    #[test]
    fn test_lower_php_if_elseif_cfg_reachable() {
        fn reachable_count(blocks: &ctadl_ir::mir::BasicBlocks) -> usize {
            let mut seen = vec![false; blocks.num_nodes()];
            let mut queue = VecDeque::from([blocks.start_node()]);
            while let Some(bb) = queue.pop_front() {
                if seen[bb.index()] {
                    continue;
                }
                seen[bb.index()] = true;
                for succ in blocks.successors(bb) {
                    queue.push_back(succ);
                }
            }
            seen.into_iter().filter(|x| *x).count()
        }

        let source = r#"
            <?php
            function test($x, $y) {
                if ($x) {
                    return 1;
                } elseif ($y) {
                    return 2;
                } else {
                    return 3;
                }
            }
        "#;

        let program_info = lower_php(source, "elseif.php").expect("Lowering failed");
        let func = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == "test")
            .expect("Expected to find function 'test'");

        assert_eq!(reachable_count(&func.blocks), func.blocks.num_nodes());
    }
}
