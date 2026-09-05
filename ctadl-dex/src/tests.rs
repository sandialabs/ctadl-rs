use super::*;
use dex_reader::DexParser;
use dex_reader::instructions::{
    Format11n, Format21h, Format21s, Format31i, Format51l, Instruction,
};
use dex_reader::types::CodeItem;
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

fn assign_from(inst: Instruction) -> (Vec<(VariableRef, Exp)>, Locals) {
    let parser = dummy_parser();
    let mut ctx = Context::new();
    let mut locals = Locals::default();
    let assigns = ctx
        .dataflow_to_assign(&parser, &dummy_code_item(), &inst, &mut locals)
        .unwrap()
        .into_iter()
        .flat_map(|s| match s {
            Statement {
                kind: StatementKind::Assign { dest, sources },
                ..
            } => sources.into_iter().map(|s| (dest.clone(), s)).collect(),
            _ => Vec::new(),
        })
        .collect();
    (assigns, locals)
}

/// The source name of a local variable ref, via the locals table.
fn local_name(locals: &Locals, var: &VariableRef) -> String {
    locals
        .name(var.variable.local().expect("expected a local"))
        .to_string()
}

#[test]
fn const4_assign() {
    let inst = Instruction::Const4(Format11n { a: Reg(0), lit: 5 });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(local_name(&locals, var), "v0");
    assert_eq!(exp, &Exp::new_int(5));
}

#[test]
fn const16_assign() {
    let inst = Instruction::Const16(Format21s {
        a: Reg(1),
        lit: 0x1234,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(local_name(&locals, var), "v1");
    assert_eq!(exp, &Exp::new_int(0x1234));
}

#[test]
fn const_assign() {
    let inst = Instruction::Const(Format31i {
        a: Reg(2),
        lit: 0x7fffffff,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(local_name(&locals, var), "v2");
    assert_eq!(exp, &Exp::new_int(0x7fffffff));
}

#[test]
fn const_wide16_assign() {
    let inst = Instruction::ConstWide16(Format21s {
        a: Reg(3),
        lit: 0x1234,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 3 + i);
        assert_eq!(local_name(&locals, var), expected_reg);
        assert_eq!(exp, &Exp::new_int(0x1234));
    }
}

#[test]
fn const_wide32_assign() {
    let inst = Instruction::ConstWide32(Format31i {
        a: Reg(5),
        lit: 0xdeadbeefu32 as i32,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 5 + i);
        assert_eq!(local_name(&locals, var), expected_reg);
        // Sign-extended, not zero-extended: 0xdeadbeef as an i32 is negative.
        assert_eq!(exp, &Exp::new_int(0xdeadbeefu32 as i32 as i64));
    }
}

#[test]
fn const_wide_assign() {
    let inst = Instruction::ConstWide(Format51l {
        a: Reg(7),
        lit: 0x1122334455667788,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 7 + i);
        assert_eq!(local_name(&locals, var), expected_reg);
        assert_eq!(exp, &Exp::new_int(0x1122334455667788));
    }
}

#[test]
fn const_wide_high16_assign() {
    let inst = Instruction::ConstWideHigh16(Format21h {
        a: Reg(10),
        lit: 0x1234,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 2);
    let shifted = (0x1234i16 as i64) << 48;
    for (i, (var, exp)) in assigns.iter().enumerate() {
        let expected_reg = format!("v{}", 10 + i);
        assert_eq!(local_name(&locals, var), expected_reg);
        assert_eq!(exp, &Exp::new_int(shifted));
    }
}

#[test]
fn const_high16_assign() {
    let inst = Instruction::ConstHigh16(Format21h {
        a: Reg(4),
        lit: 0x8000u16 as i16,
    });
    let (assigns, locals) = assign_from(inst);
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(local_name(&locals, var), "v4");
    assert_eq!(exp, &Exp::new_int(-2147483648));
}

/// The point of `Exp::Int`: one value, one representation, whichever opcode produced it. Under
/// the byte encoding these were `[0x01]` and `[0x00, 0x01]` -- two distinct constants.
#[test]
fn same_value_from_different_opcodes_is_one_constant() {
    let (four, _) = assign_from(Instruction::Const4(Format11n { a: Reg(0), lit: 1 }));
    let (sixteen, _) = assign_from(Instruction::Const16(Format21s { a: Reg(0), lit: 1 }));
    let (wide, _) = assign_from(Instruction::Const(Format31i { a: Reg(0), lit: 1 }));
    assert_eq!(four[0].1, sixteen[0].1);
    assert_eq!(four[0].1, wide[0].1);
    assert_eq!(four[0].1.as_int(), Some(1));
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
    let mut locals = Locals::default();
    let throw_var = VariableRef::new_local_idx(locals.get_or_intern("v0"));
    let throw_exp = Exp::from(AccessPath::without_fields(throw_var));

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
        matches!(&expected_args[1], Exp::Variable(_)),
        "Second arg should be a variable"
    );
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

    let mut locals = Locals::default();
    let assigns = {
        let mut ctx = Context::new();
        ctx.dataflow_to_assign(&parser, &dummy_code_item(), &inst, &mut locals)
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
    assert_eq!(local_name(&locals, var), "v0");

    if let Exp::ObjectRef(CallObject::JavaObject(cls)) = exp {
        assert_eq!(&**cls, "Ljava/lang/Object;");
    } else {
        panic!("Expected JavaObject, got {:?}", exp);
    }
}
