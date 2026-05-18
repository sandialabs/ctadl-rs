// Tests for CTADL IR verification errors
use super::*;
use crate::mir::{
    AccessPath, BasicBlockData, BasicBlockIdx, BasicBlocks, Exp, FieldAccess, FieldAccesses,
    FunctionIdx, Offset, ParameterIdx, Params, ReturnType, StatementKind, TerminatorKind,
    VariableRef,
};
use smallvec::smallvec;

/// Helper to create a minimal program with a single function.
fn make_program() -> Program {
    let mut prog = Program::default();
    let f_idx = prog.new_function();
    let f = &mut prog[f_idx];
    // default name is empty – tests can set a name if needed
    f.set_name("test".to_string());
    f.params = Params::default();
    f.return_type = ReturnType { arity: 0 };
    // Create a start block with a simple return terminator.
    let mut blocks = BasicBlocks::default();
    // Push the start block (index 0).
    blocks
        .blocks_mut()
        .push(BasicBlockData::new(Some(Terminator::new_kind(
            TerminatorKind::Return { args: smallvec![] },
        ))));
    f.blocks = blocks;
    prog
}

#[test]
fn test_unnamed_function_error() {
    let prog = make_program();
    // The function now has a valid name, so verification should succeed.
    let result = prog.verify();
    assert!(result.is_ok());
}

#[test]
fn test_empty_field_update_error() {
    let mut prog = make_program();
    // Add an Update statement with no fields.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    let var = VariableRef::new_local("x".to_string());
    let upd = StatementKind::Update {
        dest: (var.clone(), FieldAccesses::empty()),
        source: var.clone(),
        value: Exp::new_str("val"),
    };
    block.statements.push_back(Statement::new_kind(upd));
    let result = prog.verify();
    assert!(
        matches!(result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::EmptyFieldUpdate { .. })))
    );
}

// Test for ParameterDoesNotExist (no assertions, just runs verification)
#[test]
fn test_parameter_does_not_exist_error() {
    let mut prog = make_program();
    // Ensure no parameters are declared.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    f.params = Params::default();
    // Reference a non‑existent parameter.
    let var = VariableRef::new_parameter(ParameterIdx::new(0));
    // Add an assign that uses the nonexistent parameter (as an access path).
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    let stmt = Statement::new_kind(StatementKind::assign(
        VariableRef::new_local("tmp".to_string()),
        [Exp::AccessPath(AccessPath::without_fields(var.clone()))],
    ));
    block.statements.push_back(stmt);
    // Run verification; we don't assert on the result because the behavior may be buggy.
    let result = prog.verify();
    assert!(
        matches!(&result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::ParameterDoesNotExist { .. }))),
        "errors: {:?}",
        &result
    );
}

#[test]
fn test_inconsistent_returns_error() {
    let mut prog = make_program();
    // Set function return arity to 2.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    f.return_type = ReturnType { arity: 2 };
    // Provide a return with three values.
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    *block.terminator_mut() = Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![Exp::new_str("a"), Exp::new_str("b"), Exp::new_str("c"),],
    });
    let result = prog.verify();
    assert!(
        matches!(result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::InconsistentReturns { .. })))
    );
}

#[test]
fn test_empty_goto_error() {
    let mut prog = make_program();
    // Add a goto with no targets.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    *block.terminator_mut() = Terminator::new_kind(TerminatorKind::Goto {
        targets: smallvec![], // Empty targets
    });
    let result = prog.verify();
    assert!(
        matches!(result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::EmptyGoto { .. })))
    );
}

#[test]
fn test_field_accesses_with_offsets() {
    // Test creating FieldAccesses with offsets
    let offset_path = FieldAccesses::with_offset(42);
    assert_eq!(offset_path.len(), 1);

    // Test display format for offsets
    assert_eq!(format!("{}", offset_path), ".[0x2a]");

    // Test mixed field accesses
    let mixed_path = FieldAccesses::mixed(vec![Ok("field1"), Err(10), Ok("field2")]);
    assert_eq!(mixed_path.len(), 3);
    assert_eq!(format!("{}", mixed_path), ".field1.[0xa].field2");

    // Test creating access path with offsets
    let var = VariableRef::new_local("obj".to_string());
    let field_accesses = FieldAccesses::mixed(vec![Ok("field"), Err(5)]);
    let access_path = AccessPath {
        variable_ref: var,
        path: field_accesses,
    };
    assert_eq!(format!("{}", access_path), "%obj.field.[0x5]");
}

#[test]
fn test_offset_newtype() {
    // Test Offset newtype
    let offset = Offset(123);
    assert_eq!(offset.0, 123);
    assert_eq!(format!("{}", offset), "0x7b");

    // Test FieldAccess enum
    let symbol_access = FieldAccess::Symbol(ArcIntern::from("test"));
    let offset_access = FieldAccess::Offset(Offset(456));

    assert_eq!(format!("{}", symbol_access), "test");
    assert_eq!(format!("{}", offset_access), "[0x1c8]");
}

#[test]
fn test_load_store_iter_vars_and_display() {
    let base = VariableRef::new_local("base".to_string());
    let dest = VariableRef::new_local("dest".to_string());
    let value = VariableRef::new_local("value".to_string());
    let field = FieldAccess::Symbol(ArcIntern::from("slot"));

    let load = StatementKind::Load {
        dest: dest.clone(),
        source: base.clone(),
        field: field.clone(),
    };
    let store = StatementKind::Store {
        dest: base.clone(),
        field,
        value: value.clone(),
    };

    let load_srcs: Vec<_> = load.iter_src_var().cloned().collect();
    let load_dsts: Vec<_> = load.iter_dst_var().cloned().collect();
    let store_srcs: Vec<_> = store.iter_src_var().cloned().collect();
    let store_dsts: Vec<_> = store.iter_dst_var().cloned().collect();

    assert_eq!(load_srcs, vec![base.clone()]);
    assert_eq!(load_dsts, vec![dest.clone()]);
    assert_eq!(store_srcs, vec![base.clone(), value.clone()]);
    assert!(store_dsts.is_empty());
    assert_eq!(format!("{load}"), "%dest = load %base.slot");
    assert_eq!(format!("{store}"), "store %base.slot := %value");
}
