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
        use ctadl_ir::mir::Variable;
        let source = r#"
            <?php
            $a = $_GET['input']['more'];
        "#;
        let program_info = lower_php(source, "nested.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::nested.php").expect("expected main");

        // Each symbolic step of `$_GET['input']['more']` is its own `Load` in the new IR: the
        // superglobal off the global heap, then `input`, then `more`.
        let load_fields: Vec<String> =
            loads(main).iter().map(|(_, _, f)| f.field.to_string()).collect();
        for expected in ["_GET", "input", "more"] {
            assert!(
                load_fields.iter().any(|f| f == expected),
                "expected a load of `{expected}`, got {load_fields:?}"
            );
        }
        let reads_get_from_heap = loads(main).iter().any(|(_, source, field)| {
            &*field.field == "_GET"
                && matches!(*source.variable_ref.variable, Variable::GlobalHeap)
                && source.path.is_empty()
        });
        assert!(
            reads_get_from_heap,
            "expected `$_GET` to be loaded from the global heap"
        );
        let assigns_a = assignments(main)
            .iter()
            .any(|(dest, _)| dest.variable.local() == Some("a"));
        assert!(assigns_a, "expected `$a` to be assigned the loaded value");
    }

    #[test]
    fn test_lower_get_input() {
        use ctadl_ir::mir::Variable;
        let source = r#"
            <?php
            $a = $_GET['input'];
        "#;
        let program_info = lower_php(source, "get.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::get.php").expect("expected main");

        // `$_GET['input']` is a load of `_GET` off the global heap, then a load of `input`.
        let load_fields: Vec<String> =
            loads(main).iter().map(|(_, _, f)| f.field.to_string()).collect();
        for expected in ["_GET", "input"] {
            assert!(
                load_fields.iter().any(|f| f == expected),
                "expected a load of `{expected}`, got {load_fields:?}"
            );
        }
        let reads_get_from_heap = loads(main).iter().any(|(_, source, field)| {
            &*field.field == "_GET"
                && matches!(*source.variable_ref.variable, Variable::GlobalHeap)
                && source.path.is_empty()
        });
        assert!(
            reads_get_from_heap,
            "expected `$_GET` to be loaded from the global heap"
        );
        let assigns_a = assignments(main)
            .iter()
            .any(|(dest, _)| dest.variable.local() == Some("a"));
        assert!(assigns_a, "expected `$a` to be assigned the loaded value");
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

    /// Find a lowered function by name.
    fn function<'a>(
        program_info: &'a ProgramInfo,
        name: &str,
    ) -> Option<&'a ctadl_ir::mir::FunctionData> {
        program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name == name)
    }

    /// Every `Assign` in a function, as `(dest, sources)`.
    fn assignments(
        func: &ctadl_ir::mir::FunctionData,
    ) -> Vec<(
        &ctadl_ir::mir::VariableRef,
        &smallvec::SmallVec<[ctadl_ir::mir::Exp; 2]>,
    )> {
        use ctadl_ir::mir::StatementKind;
        func.blocks
            .iter()
            .flat_map(|bb| bb.statements.iter())
            .filter_map(|stmt| match &stmt.kind {
                StatementKind::Assign { dest, sources } => Some((dest, sources)),
                _ => None,
            })
            .collect()
    }

    /// Every `Load` in a function, as `(dest, source, field)`. Reading a symbolic field (a
    /// property, an array key, a global slot) is a `Load` in the new IR, not an access-path field.
    fn loads(
        func: &ctadl_ir::mir::FunctionData,
    ) -> Vec<(
        &ctadl_ir::mir::VariableRef,
        &ctadl_ir::mir::AccessPath,
        &ctadl_ir::mir::FieldPath,
    )> {
        use ctadl_ir::mir::StatementKind;
        func.blocks
            .iter()
            .flat_map(|bb| bb.statements.iter())
            .filter_map(|stmt| match &stmt.kind {
                StatementKind::Load {
                    dest,
                    source,
                    field,
                } => Some((dest, source, field)),
                _ => None,
            })
            .collect()
    }

    /// Every `Store` in a function, as `(dest, field, value)`. Writing a symbolic field is a
    /// `Store` in the new IR, which replaced the old functional `Update`.
    fn stores(
        func: &ctadl_ir::mir::FunctionData,
    ) -> Vec<(
        &ctadl_ir::mir::AccessPath,
        &ctadl_ir::mir::FieldPath,
        &ctadl_ir::mir::Exp,
    )> {
        use ctadl_ir::mir::StatementKind;
        func.blocks
            .iter()
            .flat_map(|bb| bb.statements.iter())
            .filter_map(|stmt| match &stmt.kind {
                StatementKind::Store { dest, field, value } => Some((dest, field, value)),
                _ => None,
            })
            .collect()
    }

    /// A called function with no definition still has to exist as a function with formals: a sink
    /// model on `exec`'s `Argument(0)` has nothing to attach to otherwise, and taint only crosses
    /// a call boundary into a formal the callee declares.
    #[test]
    fn test_lower_stubs_called_but_undefined_functions() {
        use ctadl_ir::mir::ParameterType;
        let source = r#"
            <?php
            exec($_GET['cmd'], $second);
        "#;
        let program_info = lower_php(source, "stub.php").expect("Lowering failed");
        let exec = function(&program_info, "exec").expect("expected `exec` to be stubbed");

        assert!(
            exec.blocks.is_empty(),
            "a stub must have no blocks: that is what marks it external"
        );
        // Arity comes from the widest call site, not a real signature.
        assert_eq!(exec.params.parameters.len(), 2);
        assert!(
            exec.params
                .parameters
                .iter()
                .all(|p| matches!(p, ParameterType::ByVal))
        );
    }

    /// A method declared in an interface or a trait is registered like any other: PHP resolves a
    /// call by method name across every type that declares it. This also pins the crash that
    /// qualifying a method name used to cause for any type declaration that was not a `class`.
    #[test]
    fn test_lower_interface_and_trait_methods() {
        let source = r#"
            <?php
            interface Echoer {
                public function emit($v);
            }
            trait Prefixes {
                public function prefix($v) { return 'p:' . $v; }
            }
            class ShellEchoer implements Echoer {
                use Prefixes;
                public function emit($v) { exec($this->prefix($v)); }
            }
        "#;
        let program_info = lower_php(source, "iface.php").expect("Lowering failed");

        for name in ["Echoer::emit", "Prefixes::prefix", "ShellEchoer::emit"] {
            assert!(
                function(&program_info, name).is_some(),
                "expected to find method '{name}'"
            );
        }
    }

    /// `global $g` makes the name denote the global heap slot, for reads and writes alike, so a
    /// function body and file scope refer to the same location.
    #[test]
    fn test_lower_global_declaration_reads_global_heap() {
        use ctadl_ir::mir::Variable;
        let source = r#"
            <?php
            function readsGlobal() {
                global $g_tainted;
                $local = $g_tainted;
            }
        "#;
        let program_info = lower_php(source, "globals.php").expect("Lowering failed");
        let func = function(&program_info, "readsGlobal").expect("expected 'readsGlobal'");

        // The aliased name reads the global heap slot `g_tainted` as a `Load`.
        let reads_global_slot = loads(func).iter().any(|(_, source, field)| {
            &*field.field == "g_tainted"
                && matches!(*source.variable_ref.variable, Variable::GlobalHeap)
                && source.path.is_empty()
        });
        assert!(
            reads_global_slot,
            "expected `$local = $g_tainted` to load the global slot `g_tainted`"
        );
        let assigns_local = assignments(func)
            .iter()
            .any(|(dest, _)| dest.variable.local() == Some("local"));
        assert!(assigns_local, "expected `$local` to be assigned");
    }

    /// A file-scope assignment is also published to the global heap, since another function can
    /// reach it with `global $x` or `$GLOBALS['x']`.
    #[test]
    fn test_lower_file_scope_assignment_mirrors_to_global_heap() {
        use ctadl_ir::mir::Variable;
        let source = r#"
            <?php
            $g = $_GET['input'];
        "#;
        let program_info = lower_php(source, "mirror.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::mirror.php").expect("expected main");

        // The mirror writes a field of the global heap, which is a `Store`, not an `Assign`.
        let mirrors = stores(main).iter().any(|(dest, field, _)| {
            matches!(*dest.variable_ref.variable, Variable::GlobalHeap)
                && dest.path.is_empty()
                && &*field.field == "g"
        });
        assert!(
            mirrors,
            "expected the file-scope `$g = ..` to also store the global slot `g`"
        );
    }

    /// `$GLOBALS['g']` and a `global $g` must name the same location: `$GLOBALS` *is* the global
    /// symbol table, so it is the heap itself with no field of its own.
    #[test]
    fn test_lower_globals_array_names_the_global_slot() {
        use ctadl_ir::mir::Variable;
        let source = r#"
            <?php
            function viaGlobalsArray() {
                $v = $GLOBALS['g_tainted'];
            }
        "#;
        let program_info = lower_php(source, "globals-array.php").expect("Lowering failed");
        let func = function(&program_info, "viaGlobalsArray").expect("expected function");

        // `$GLOBALS['g_tainted']` is a single load of `g_tainted` off the global heap itself --
        // no intervening `GLOBALS` field.
        let reads_slot = loads(func).iter().any(|(_, source, field)| {
            &*field.field == "g_tainted"
                && matches!(*source.variable_ref.variable, Variable::GlobalHeap)
                && source.path.is_empty()
        });
        assert!(
            reads_slot,
            "expected `$GLOBALS['g_tainted']` to load the global slot `g_tainted`, with no `GLOBALS` field"
        );
        let assigns_v = assignments(func)
            .iter()
            .any(|(dest, _)| dest.variable.local() == Some("v"));
        assert!(assigns_v, "expected `$v` to be assigned");
    }

    /// A by-reference parameter is an out-parameter: it must be declared `ByRef`, and the local
    /// the body assigns has to be copied back onto the formal for the write to reach the caller.
    #[test]
    fn test_lower_by_ref_param_is_declared_and_written_back() {
        use ctadl_ir::mir::{Exp, ParameterType, Variable};
        let source = r#"
            <?php
            function fill(&$out, $v) {
                $out = $v;
            }
        "#;
        let program_info = lower_php(source, "byref.php").expect("Lowering failed");
        let func = function(&program_info, "fill").expect("expected 'fill'");

        let param_types: Vec<_> = func.params.parameters.iter().copied().collect();
        assert!(matches!(
            param_types.as_slice(),
            [ParameterType::ByRef, ParameterType::ByVal]
        ));

        let writes_back = assignments(func).iter().any(|(dest, sources)| {
            matches!(*dest.variable, Variable::Param(idx) if idx.index() == 0)
                && matches!(sources.first(), Some(Exp::AccessPath(ap))
                    if ap.variable_ref.variable.local() == Some("out"))
        });
        assert!(
            writes_back,
            "expected the by-ref local `$out` to be copied back onto formal 0"
        );
    }

    /// A closure becomes its own function, and the expression it appears in evaluates to a pointer
    /// to it, so a later call through the variable can be resolved back to the body.
    #[test]
    fn test_lower_closure_is_a_function_and_a_pointer() {
        use ctadl_ir::mir::{CallObject, Exp};
        let source = r#"
            <?php
            $param = function ($v) { return $v; };
        "#;
        let program_info = lower_php(source, "closure.php").expect("Lowering failed");

        let closure = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name.starts_with("{closure}@closure.php:"))
            .expect("expected the closure to be lowered as its own function");
        assert_eq!(closure.params.parameters.len(), 1);

        let main = function(&program_info, "__php_main__::closure.php").expect("expected main");
        let assigns_pointer = assignments(main).iter().any(|(dest, sources)| {
            dest.variable.local() == Some("param")
                && matches!(sources.first(), Some(Exp::ObjectRef(CallObject::FunctionPtr(name)))
                    if &**name == closure.name)
        });
        assert!(
            assigns_pointer,
            "expected `$param` to be assigned a pointer to the closure"
        );
    }

    /// A `use (..)` capture travels from the enclosing frame to a frame that does not exist yet,
    /// through a global slot private to the closure: written where the closure is created, read
    /// back inside its body.
    #[test]
    fn test_lower_closure_captures_through_a_private_slot() {
        use ctadl_ir::mir::{Exp, Variable};
        let source = r#"
            <?php
            $byValue = function () use ($tainted) {
                $inner = $tainted;
            };
        "#;
        let program_info = lower_php(source, "capture.php").expect("Lowering failed");
        let closure = program_info
            .program
            .functions
            .functions
            .iter()
            .find(|f| f.name.starts_with("{closure}@capture.php:"))
            .expect("expected the closure to be lowered");
        let slot = format!("{}::tainted", closure.name);

        // Inside the closure, the captured name reads the slot with a `Load` off the global heap.
        let reads_slot = loads(closure).iter().any(|(_, source, field)| {
            &*field.field == slot
                && matches!(*source.variable_ref.variable, Variable::GlobalHeap)
                && source.path.is_empty()
        });
        assert!(
            reads_slot,
            "expected the closure body to load the capture slot"
        );

        // At the creation site, the slot is filled from the enclosing variable with a `Store`.
        let main = function(&program_info, "__php_main__::capture.php").expect("expected main");
        let fills_slot = stores(main).iter().any(|(dest, field, value)| {
            matches!(*dest.variable_ref.variable, Variable::GlobalHeap)
                && dest.path.is_empty()
                && &*field.field == slot
                && matches!(value, Exp::AccessPath(ap)
                    if ap.variable_ref.variable.local() == Some("tainted"))
        });
        assert!(
            fills_slot,
            "expected the capture slot to be filled from `$tainted` where the closure is created"
        );
    }

    /// A `foreach` binds its loop variable to an element of the collection. Which element is not
    /// known, so the whole collection is copied: every field of it stays reachable through the
    /// loop variable.
    #[test]
    fn test_lower_foreach_binds_loop_variable_to_collection() {
        use ctadl_ir::mir::Exp;
        let source = r#"
            <?php
            foreach ($values as $v) {
                echo $v;
            }
        "#;
        let program_info = lower_php(source, "foreach.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::foreach.php").expect("expected main");

        let binds = assignments(main).iter().any(|(dest, sources)| {
            dest.variable.local() == Some("v")
                && matches!(sources.first(), Some(Exp::AccessPath(ap))
                    if ap.variable_ref.variable.local() == Some("values") && ap.path.is_empty())
        });
        assert!(
            binds,
            "expected `$v` to be bound to the collection `$values`"
        );
    }

    /// `foreach ($a as &$e)` writes back through the alias, so an assignment to the element inside
    /// the body lands in the collection -- at the same shared element field a subscript with no
    /// static index uses, which is what a later `$a[0]` reads.
    #[test]
    fn test_lower_foreach_by_ref_writes_back_to_element() {
        use ctadl_ir::mir::Exp;
        let source = r#"
            <?php
            foreach ($items as &$item) {
                $item = $tainted;
            }
        "#;
        let program_info = lower_php(source, "foreach-ref.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::foreach-ref.php").expect("expected main");

        // The write-back is a `Store` into the shared element field `[]` of `$items`.
        let writes_back = stores(main).iter().any(|(dest, field, value)| {
            dest.variable_ref.variable.local() == Some("items")
                && dest.path.is_empty()
                && &*field.field == "[]"
                && matches!(value, Exp::AccessPath(ap)
                    if ap.variable_ref.variable.local() == Some("item"))
        });
        assert!(
            writes_back,
            "expected `&$item` to write back into an element of `$items`"
        );
    }

    /// A call through a variable is resolved from the pointers assigned into it, not by inventing
    /// a function named after the variable.
    #[test]
    fn test_lower_dynamic_call_is_a_function_pointer_call() {
        use ctadl_ir::mir::{StatementKind, call::CallStyle};
        let source = r#"
            <?php
            $fn($arg);
        "#;
        let program_info = lower_php(source, "dyn.php").expect("Lowering failed");
        let main = function(&program_info, "__php_main__::dyn.php").expect("expected main");

        let indirect =
            main.blocks
                .iter()
                .flat_map(|bb| bb.statements.iter())
                .any(|stmt| match &stmt.kind {
                    StatementKind::CallAssign { style, .. } => matches!(
                        style,
                        CallStyle::FuncPtrCall { callee, .. }
                            if callee.variable_ref.variable.local() == Some("fn")
                    ),
                    _ => false,
                });
        assert!(
            indirect,
            "expected `$fn(..)` to lower to a function-pointer call"
        );
        assert!(
            function(&program_info, "$fn").is_none(),
            "a call through a variable must not invent a function named after it"
        );
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
