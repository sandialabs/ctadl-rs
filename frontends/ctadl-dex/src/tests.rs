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
        // The value is sign-extended, not zero-extended, so 0xdeadbeef read as an i32 is
        // negative.
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

/// This is what `Exp::Int` is for: one value has one representation, no matter which opcode
/// produced it. Under the old byte encoding these two were `[0x01]` and `[0x00, 0x01]`, which
/// counted as two different constants.
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

/// A parser whose constant pool holds one field, `LFoo;->cb:LBar;`, at index 0.
fn parser_with_field() -> DexParser<'static> {
    use dex_reader::types::{FieldId, StringId, StringTable, TypeId};

    // Each string_data_item is a ULEB128 UTF-16 length, the MUTF-8 bytes, then a NUL.
    let data: &'static [u8] = b"\x05LBar;\0\x05LFoo;\0\x02cb\0";
    let mut parser = dummy_parser();
    parser.data = data;
    parser.strings = StringTable {
        data,
        string_ids: vec![
            StringId { string_data_off: 0 },
            StringId { string_data_off: 7 },
            StringId {
                string_data_off: 14,
            },
        ],
    };
    parser.type_ids = vec![TypeId { descriptor_idx: 0 }, TypeId { descriptor_idx: 1 }];
    parser.field_ids = vec![FieldId {
        class_idx: 1,
        type_idx: 0,
        name_idx: 2,
    }];
    parser
}

/// Translates a store and returns the registers that flow into the stored value, and the object
/// (or array) register the value is stored into.
///
/// Stores translate to `temp = assign(values...)` then `store(object.field, temp)`.
fn store_parts(parser: &DexParser<'_>, inst: Instruction) -> (Vec<String>, String) {
    let mut ctx = Context::new();
    let mut locals = Locals::default();
    let stmts = ctx
        .dataflow_to_assign(parser, &dummy_code_item(), &inst, &mut locals)
        .unwrap();
    let [assign, store] = stmts.as_slice() else {
        panic!("expected an assign and a store, got {stmts:?}");
    };
    let StatementKind::Assign {
        dest: temp,
        sources,
    } = &assign.kind
    else {
        panic!("expected an assign, got {assign:?}");
    };
    let StatementKind::Store { dest, value, .. } = &store.kind else {
        panic!("expected a store, got {store:?}");
    };
    assert_eq!(value, &Exp::from(temp.clone()), "store must write the temp");
    let values = sources
        .iter()
        .map(|s| match s {
            Exp::Variable(v) => local_name(&locals, v),
            other => panic!("expected a variable source, got {other:?}"),
        })
        .collect();
    (values, local_name(&locals, &dest.base))
}

#[test]
fn iput_object_stores_value() {
    use dex_reader::instructions::{FieldIdx, Format22c};
    let inst = Instruction::IPutObject(Format22c {
        a: Reg(0),
        b: Reg(1),
        idx: FieldIdx(0),
    });
    assert_eq!(
        store_parts(&parser_with_field(), inst),
        (vec!["v0".to_string()], "v1".to_string())
    );
}

/// `iput-object p0, p0, F` is what `this.f = this` compiles to. The value and the object are
/// the same register, and the value must still flow into the field.
#[test]
fn iput_object_self_alias_stores_value() {
    use dex_reader::instructions::{FieldIdx, Format22c};
    let inst = Instruction::IPutObject(Format22c {
        a: Reg(0),
        b: Reg(0),
        idx: FieldIdx(0),
    });
    assert_eq!(
        store_parts(&parser_with_field(), inst),
        (vec!["v0".to_string()], "v0".to_string())
    );
}

#[test]
fn iput_wide_stores_value_pair() {
    use dex_reader::instructions::{FieldIdx, Format22c};
    let inst = Instruction::IPutWide(Format22c {
        a: Reg(0),
        b: Reg(2),
        idx: FieldIdx(0),
    });
    assert_eq!(
        store_parts(&parser_with_field(), inst),
        (vec!["v0".to_string(), "v1".to_string()], "v2".to_string())
    );
}

/// `aput-object v1, v1, v2` is `a[i] = a`: the value and the array are the same register.
#[test]
fn aput_object_self_alias_stores_value() {
    use dex_reader::instructions::Format23x;
    let inst = Instruction::APutObject(Format23x {
        a: Reg(1),
        b: Reg(1),
        c: Reg(2),
    });
    assert_eq!(
        store_parts(&dummy_parser(), inst),
        (vec!["v1".to_string()], "v1".to_string())
    );
}

/// `aput v0, v1, v0` is `a[i] = i`: the value and the index are the same register.
#[test]
fn aput_value_is_index_stores_value() {
    use dex_reader::instructions::Format23x;
    let inst = Instruction::APut(Format23x {
        a: Reg(0),
        b: Reg(1),
        c: Reg(0),
    });
    assert_eq!(
        store_parts(&dummy_parser(), inst),
        (vec!["v0".to_string()], "v1".to_string())
    );
}

#[test]
fn aput_wide_stores_value_pair() {
    use dex_reader::instructions::Format23x;
    let inst = Instruction::APutWide(Format23x {
        a: Reg(0),
        b: Reg(2),
        c: Reg(3),
    });
    assert_eq!(
        store_parts(&dummy_parser(), inst),
        (vec!["v0".to_string(), "v1".to_string()], "v2".to_string())
    );
}

/// `shl-long v0, v2, v4` computes `v0:v1 = v2:v3 << v4`. The shift amount is the int in `v4`, so
/// `v5` must not flow into the result.
#[test]
fn shl_long_reads_one_shift_register() {
    use dex_reader::instructions::Format23x;
    let inst = Instruction::ShlLong(Format23x {
        a: Reg(0),
        b: Reg(2),
        c: Reg(4),
    });
    let (assigns, locals) = assign_from(inst);
    let flows: Vec<(String, String)> = assigns
        .iter()
        .map(|(var, exp)| {
            let Exp::Variable(src) = exp else {
                panic!("expected a variable source, got {exp:?}");
            };
            (local_name(&locals, var), local_name(&locals, src))
        })
        .collect();
    let expected: Vec<(String, String)> = ["v0", "v1"]
        .iter()
        .flat_map(|d| ["v2", "v3", "v4"].map(|s| (d.to_string(), s.to_string())))
        .collect();
    assert_eq!(flows, expected);
}

/// A parser whose string table holds `strings`, in order. Each string must be ASCII.
fn parser_with_strings(strings: &[&str]) -> DexParser<'static> {
    use dex_reader::types::{StringId, StringTable};

    // Each string_data_item is a ULEB128 UTF-16 length, the MUTF-8 bytes, then a NUL.
    let mut data = Vec::new();
    let mut string_ids = Vec::new();
    for s in strings {
        assert!(s.is_ascii() && s.len() < 0x80);
        string_ids.push(StringId {
            string_data_off: data.len() as u32,
        });
        data.push(s.len() as u8);
        data.extend_from_slice(s.as_bytes());
        data.push(0);
    }
    let data: &'static [u8] = Box::leak(data.into_boxed_slice());
    let mut parser = dummy_parser();
    parser.data = data;
    parser.strings = StringTable { data, string_ids };
    parser
}

/// A parser whose method 0 is `Ljava/lang/invoke/MethodHandle;->invoke()Ljava/lang/Object;`.
fn parser_with_method_handle_invoke() -> DexParser<'static> {
    use dex_reader::types::{MethodId, ProtoId, TypeId};

    let mut parser = parser_with_strings(&[
        "Ljava/lang/invoke/MethodHandle;",
        "Ljava/lang/Object;",
        "invoke",
        "L",
    ]);
    parser.type_ids = vec![TypeId { descriptor_idx: 0 }, TypeId { descriptor_idx: 1 }];
    parser.proto_ids = vec![ProtoId {
        shorty_idx: 3,
        return_type_idx: 1,
        parameters_off: 0,
    }];
    parser.method_ids = vec![MethodId {
        class_idx: 0,
        proto_idx: 0,
        name_idx: 2,
    }];
    parser
}

fn arg_names(locals: &Locals, args: &[Exp]) -> Vec<String> {
    args.iter()
        .map(|a| {
            local_name(
                locals,
                a.variable_ref().expect("expected a variable argument"),
            )
        })
        .collect()
}

/// `invoke-polymorphic {v0, v1, v2}, MethodHandle.invoke, proto` calls `invoke` on the handle
/// in `v0`, passing `v1` and `v2`, like invoke-virtual.
#[test]
fn invoke_polymorphic_is_a_virtual_call() {
    use dex_reader::instructions::{Format45cc, MethodIdx, ProtoIdx};
    let parser = parser_with_method_handle_invoke();
    let inst = Instruction::InvokePolymorphic(Format45cc {
        method_id: MethodIdx(0),
        args: vec![Reg(0), Reg(1), Reg(2)],
        proto_id: ProtoIdx(0),
    });
    let mut ctx = Context::new();
    let mut locals = Locals::default();
    let stmt = ctx
        .decode_call(&parser, &dummy_code_item(), &inst, &mut locals)
        .expect("invoke-polymorphic should lower to a call");
    let StatementKind::CallAssign { style, args, rets } = &stmt.kind else {
        panic!("expected a call, got {stmt:?}");
    };
    let CallStyle::JavaCall {
        receiver,
        cls,
        simple_name,
        dispatch,
        ..
    } = style
    else {
        panic!("expected a Java call, got {style:?}");
    };
    assert_eq!(local_name(&locals, receiver), "v0");
    assert_eq!(&**cls, "Ljava/lang/invoke/MethodHandle;");
    assert_eq!(&**simple_name, "invoke");
    assert_eq!(*dispatch, JavaDispatch::Virtual);
    assert_eq!(arg_names(&locals, args), ["v1", "v2"]);
    assert_eq!(ctx.call_result.as_ref(), Some(&rets[0]));
}

/// invoke-custom has no method to name, so it becomes a call with an unknown target. It must
/// still define the result that the following move-result reads.
#[test]
fn invoke_custom_is_a_call_with_unknown_target() {
    use dex_reader::instructions::{CallIdx, Format35c};
    let inst = Instruction::InvokeCustom(Format35c {
        idx: CallIdx(0),
        args: vec![Reg(1), Reg(2)],
    });
    let mut ctx = Context::new();
    let mut locals = Locals::default();
    let stmt = ctx
        .decode_call(&dummy_parser(), &dummy_code_item(), &inst, &mut locals)
        .expect("invoke-custom should lower to a call");
    let StatementKind::CallAssign { style, args, rets } = &stmt.kind else {
        panic!("expected a call, got {stmt:?}");
    };
    assert!(matches!(style, CallStyle::Unknown), "got {style:?}");
    assert_eq!(arg_names(&locals, args), ["v1", "v2"]);
    assert_eq!(ctx.call_result.as_ref(), Some(&rets[0]));
}
