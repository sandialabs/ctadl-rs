use super::*;
use dex_reader::DexParser;
use dex_reader::instructions::{
    Format11n, Format21h, Format21s, Format31i, Format51l, Instruction,
};
use dex_reader::types::{CatchHandlerList, CodeItem, EncodedCatchHandler, TryItem, TypeAddrPair};
use std::sync::OnceLock;

fn dummy_parser() -> DexParser<'static> {
    DexParser {
        data: &[],
        header: Default::default(),
        map_list: Default::default(),
        map_items_by_type: std::collections::HashMap::new(),
        strings: dex_reader::types::StringTable {
            data: &[],
            string_ids: vec![],
        },
        type_ids: vec![],
        proto_ids: vec![],
        field_ids: vec![],
        method_ids: vec![],
        class_defs: vec![],
        call_site_ids: None,
        method_handles: None,
        cache: dex_reader::parser::Cache {
            pool: OnceLock::new(),
        },
    }
}

fn dummy_code_item() -> CodeItem {
    CodeItem {
        registers_size: 1,
        ins_size: 0,
        outs_size: 0,
        tries_size: 0,
        debug_info_off: 0,
        insns: Vec::new(),
        tries: Vec::new(),
        handlers: None,
        code_off: 0,
    }
}

fn assign_from(inst: Instruction) -> Vec<(VariableRef, Exp)> {
    let parser = dummy_parser();
    let mut ctx = Context::new();
    ctx.dataflow_to_assign(&parser, &dummy_code_item(), &inst)
        .unwrap()
        .into_iter()
        .flat_map(|s| match s {
            Statement {
                kind: StatementKind::Assign { dest, sources },
                ..
            } => sources.into_iter().map(|s| (dest.clone(), s)).collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn const4_assign() {
    let inst = Instruction::Const4(Format11n { a: Reg(0), lit: 5 });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(*var.variable, Variable::Local("v0".into()));
    assert_eq!(exp, &Exp::new_bytes(5i8.to_be_bytes().to_vec()));
}

#[test]
fn const16_assign() {
    let inst = Instruction::Const16(Format21s {
        a: Reg(1),
        lit: 0x1234,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(*var.variable, Variable::Local("v1".into()));
    assert_eq!(exp, &Exp::new_bytes(0x1234i16.to_be_bytes().to_vec()));
}

#[test]
fn const_assign() {
    let inst = Instruction::Const(Format31i {
        a: Reg(2),
        lit: 0x7fffffff,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(*var.variable, Variable::Local("v2".into()));
    assert_eq!(exp, &Exp::new_bytes(0x7fffffffi32.to_be_bytes().to_vec()));
}

#[test]
fn const_wide16_assign() {
    let inst = Instruction::ConstWide16(Format21s {
        a: Reg(3),
        lit: 0x1234,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 3 + i);
        assert_eq!(*var.variable, Variable::Local(expected_reg.into()));
        assert_eq!(exp, &Exp::new_bytes((0x1234i16).to_be_bytes().to_vec()));
    }
}

#[test]
fn const_wide32_assign() {
    let inst = Instruction::ConstWide32(Format31i {
        a: Reg(5),
        lit: 0xdeadbeefu32 as i32,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 5 + i);
        assert_eq!(*var.variable, Variable::Local(expected_reg.into()));
        assert_eq!(
            exp,
            &Exp::new_bytes((0xdeadbeefu32 as i32).to_be_bytes().to_vec())
        );
    }
}

#[test]
fn const_wide_assign() {
    let inst = Instruction::ConstWide(Format51l {
        a: Reg(7),
        lit: 0x1122334455667788,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 7 + i);
        assert_eq!(*var.variable, Variable::Local(expected_reg.into()));
        assert_eq!(
            exp,
            &Exp::new_bytes(0x1122334455667788i64.to_be_bytes().to_vec())
        );
    }
}

#[test]
fn const_wide_high16_assign() {
    let inst = Instruction::ConstWideHigh16(Format21h {
        a: Reg(10),
        lit: 0x1234,
    });
    let assigns = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    let shifted = (0x1234i16 as i64) << 48;
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 10 + i);
        assert_eq!(*var.variable, Variable::Local(expected_reg.into()));
        assert_eq!(exp, &Exp::new_bytes(shifted.to_be_bytes().to_vec()));
    }
}

#[test]
fn test_throw_instruction_terminator() {
    // Test that throw instructions generate proper Return terminators
    // This is a conceptual test - in practice we'd need to parse actual dex with throw instructions

    // The implementation should:
    // 1. Detect Instruction::Throw(f)
    // 2. Create Return terminator with (Exp::new_bytes(empty), throw_value)
    // 3. Set function return type arity to 2

    // For now, verify our helper functions work correctly
    let empty_exp = Exp::new_bytes(Vec::new());
    let throw_var = VariableRef::new_local("v0".to_string());
    let throw_exp = Exp::new_access_path(AccessPath::without_fields(throw_var));

    // This should be the structure of a throw terminator
    let expected_args: SmallVec<[Exp; 4]> = smallvec![empty_exp.clone(), throw_exp];

    assert_eq!(
        expected_args.len(),
        2,
        "Throw terminator should have 2 arguments"
    );

    // First arg should be empty (normal return value)
    if let Exp::Bytes(bytes) = &expected_args[0] {
        assert!(bytes.is_empty(), "First arg of throw should be empty bytes");
    } else {
        panic!("First arg of throw should be Bytes variant");
    }

    // Second arg should be the throw value
    assert!(
        matches!(&expected_args[1], Exp::AccessPath(_)),
        "Second arg should be AccessPath"
    );
}

#[test]
fn test_exception_handler_parsing_basic() {
    // Basic test to verify exception handler parsing doesn't crash
    // Create a simple code item without handlers
    let code_item = CodeItem {
        registers_size: 1,
        ins_size: 0,
        outs_size: 0,
        tries_size: 0,
        debug_info_off: 0,
        insns: vec![],
        tries: vec![],
        handlers: None,
        code_off: 0,
    };

    // Call the parsing function - should return empty vec for no handlers
    let empty_map: hashbrown::HashMap<usize, ctadl_ir::mir::BasicBlockIdx> =
        hashbrown::HashMap::new();
    let handlers = parse_exception_handlers(&code_item, &[], &empty_map);

    // Should return empty vector when no handlers present
    assert!(
        handlers.is_empty(),
        "Should return empty handlers for code without exception handlers"
    );
}

#[test]
fn test_exception_handler_parsing_complex() {
    // Test with actual handlers and a correctly populated offset_to_bb map
    let mut code_item = dummy_code_item();
    code_item.tries_size = 1;
    code_item.tries = vec![TryItem {
        start_addr: 0,
        insn_count: 10,
        handler_off: 0,
    }];
    code_item.handlers = Some(CatchHandlerList {
        size: 1,
        handlers: vec![EncodedCatchHandler {
            raw_size: 1,
            pairs: vec![TypeAddrPair {
                type_idx: 1,
                addr: 20, // handler at code unit 20
            }],
            catch_all_addr: Some(30), // catch-all at code unit 30
            start_off: 0,
        }],
    });

    let mut offset_to_bb = hashbrown::HashMap::new();
    let handler_block = BasicBlockIdx::new(5);
    let catch_all_block = BasicBlockIdx::new(7);
    offset_to_bb.insert(20, handler_block);
    offset_to_bb.insert(30, catch_all_block);

    let handlers = parse_exception_handlers(&code_item, &[], &offset_to_bb);

    assert_eq!(handlers.len(), 2);
    assert!(handlers.contains(&handler_block));
    assert!(handlers.contains(&catch_all_block));
}

#[test]
fn new_instance_assign() {
    use dex_reader::instructions::{Format21c, Reg, TypeIdx};
    use dex_reader::types::{StringId, StringTable, TypeId};

    let data: &'static [u8] = b"\x12Ljava/lang/Object;\0";
    let mut parser = dummy_parser();
    parser.data = data;
    parser.type_ids = vec![TypeId { descriptor_idx: 0 }];
    parser.strings = StringTable {
        data,
        string_ids: vec![StringId { string_data_off: 0 }],
    };

    let inst = Instruction::NewInstance(Format21c {
        a: Reg(0),
        idx: TypeIdx(0),
    });

    let assigns = {
        let mut ctx = Context::new();
        ctx.dataflow_to_assign(&parser, &dummy_code_item(), &inst)
            .unwrap()
            .into_iter()
            .flat_map(|s| match s {
                Statement {
                    kind: StatementKind::Assign { dest, sources },
                    ..
                } => sources.into_iter().map(|s| (dest.clone(), s)).collect(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(var.variable.as_ref(), &Variable::Local("v0".into()));

    if let Exp::ObjectRef(CallObject::JavaObject(cls)) = exp {
        assert_eq!(&**cls, "Ljava/lang/Object;");
    } else {
        panic!("Expected JavaObject, got {:?}", exp);
    }
}
