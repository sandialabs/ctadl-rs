use crate::index::idx::Idx;
use crate::mir::builder::BasicBlockBuilder;
use crate::mir::call::CallStyle;
use crate::mir::{BasicBlockData, BasicBlockIdx, Exp, ParameterIdx, StatementIdx};
use smallvec::smallvec;

#[test]
fn test_builder_basic_operations() {
    let mut block_data = BasicBlockData::new(None);
    let mut builder = BasicBlockBuilder::new(&mut block_data);

    // Test insertion point management
    assert_eq!(builder.get_insertion_point(), 0);
    builder.set_insertion_point(5);
    assert_eq!(builder.get_insertion_point(), 5);

    // Test creating a local variable
    let var_x = builder.new_local_var("x");
    let var_y = builder.new_local_var("y");

    // Test creating an assignment
    let stmt_idx = builder.create_assign(var_x.clone(), vec![var_y.clone().into()]);
    assert_eq!(stmt_idx.index(), 5); // Should be at insertion point 5
    assert_eq!(builder.get_insertion_point(), 6); // Should increment after insertion

    // Test creating a return terminator
    builder.create_ret(vec![var_x.clone().into()]);

    // Verify the block data
    assert_eq!(block_data.statements.len(), 1);
    assert!(block_data.terminator.is_some());
}

#[test]
fn test_builder_insertion_at_positions() {
    let mut block_data = BasicBlockData::new(None);
    let mut builder = BasicBlockBuilder::new(&mut block_data);

    let var_a = builder.new_local_var("a");
    let var_b = builder.new_local_var("b");
    let var_c = builder.new_local_var("c");

    // Insert at position 0
    builder.set_insertion_point(0);
    builder.insert_statement(crate::mir::Statement::new_kind(
        crate::mir::StatementKind::assign(var_a.clone(), vec![var_b.clone().into()]),
    ));

    // Insert at position 1
    builder.set_insertion_point(1);
    builder.insert_statement(crate::mir::Statement::new_kind(
        crate::mir::StatementKind::assign(var_b.clone(), vec![var_c.clone().into()]),
    ));

    // Insert at position 0 (should shift others)
    builder.set_insertion_point(0);
    builder.insert_statement(crate::mir::Statement::new_kind(
        crate::mir::StatementKind::Nop,
    ));

    assert_eq!(block_data.statements.len(), 3);

    // Verify order: Nop, a = b, b = c
    match block_data.statements[StatementIdx::new(0)].kind {
        crate::mir::StatementKind::Nop => {}
        _ => panic!("Expected Nop at position 0"),
    }

    match &block_data.statements[StatementIdx::new(1)].kind {
        crate::mir::StatementKind::Assign { dest, sources } => {
            assert_eq!(dest.to_string(), "%a");
            assert_eq!(sources.len(), 1);
        }
        _ => panic!("Expected Assign at position 1"),
    }
}

#[test]
fn test_builder_call_statement() {
    let mut block_data = BasicBlockData::new(None);
    let mut builder = BasicBlockBuilder::new(&mut block_data);

    let var_result = builder.new_local_var("result");
    let var_arg1 = builder.new_local_var("arg1");
    let var_arg2 = builder.new_local_var("arg2");

    // Create a call statement
    let call_style = CallStyle::DirectCall {
        call_edges: crate::mir::call::CallEdges::Explicit(smallvec!["test".to_string()]),
    };

    builder.create_call(
        call_style,
        vec![var_result.clone()],
        vec![var_arg1.clone().into(), var_arg2.clone().into()],
    );

    assert_eq!(block_data.statements.len(), 1);

    match &block_data.statements[StatementIdx::new(0)].kind {
        crate::mir::StatementKind::CallAssign { rets, args, .. } => {
            assert_eq!(rets.len(), 1);
            assert_eq!(args.len(), 2);
        }
        _ => panic!("Expected CallAssign statement"),
    }
}

#[test]
fn test_builder_terminators() {
    let mut block_data = BasicBlockData::new(None);
    let mut builder = BasicBlockBuilder::new(&mut block_data);

    let var_x = builder.new_local_var("x");

    // Test return terminator
    builder.create_ret(vec![var_x.clone().into()]);

    assert!(block_data.terminator.is_some());

    // Test goto terminator
    let mut block_data2 = BasicBlockData::new(None);
    let mut builder2 = BasicBlockBuilder::new(&mut block_data2);
    builder2.create_goto(vec![BasicBlockIdx::new(1), BasicBlockIdx::new(2)]);

    assert!(block_data2.terminator.is_some());
}

#[test]
fn test_builder_convenience_methods() {
    let mut block_data = BasicBlockData::new(None);
    let builder = BasicBlockBuilder::new(&mut block_data);

    // Test variable creation
    let local_var = builder.new_local_var("test");
    assert_eq!(local_var.to_string(), "%test");

    let param_var = builder.new_param_var(ParameterIdx::new(0));
    assert_eq!(param_var.to_string(), "@p0");

    let global_var = builder.new_global_var();
    assert_eq!(global_var.to_string(), "$globals");

    // Test access path creation
    let access_path = builder.new_access_path(local_var.clone(), vec!["field1", "field2"]);
    assert_eq!(access_path.to_string(), "%test.field1.field2");

    // Test expression creation
    let str_exp = builder.new_str_exp("hello");
    let bytes_exp = builder.new_bytes_exp(vec![1, 2, 3]);

    match str_exp {
        Exp::Str(s) => assert_eq!(format!("{}", s), "hello"),
        _ => panic!("Expected string expression"),
    }

    match bytes_exp {
        Exp::Bytes(bytes) => assert_eq!(bytes, vec![1, 2, 3]),
        _ => panic!("Expected bytes expression"),
    }
}
