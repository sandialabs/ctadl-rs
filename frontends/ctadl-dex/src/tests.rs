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
    assign_from_in(&dummy_parser(), &mut Context::new(), inst)
}

/// [`assign_from`], against `parser` and in `ctx`.
fn assign_from_in(
    parser: &DexParser<'_>,
    ctx: &mut Context,
    inst: Instruction,
) -> (Vec<(VariableRef, Exp)>, Locals) {
    let mut locals = Locals::default();
    let assigns = ctx
        .dataflow_to_assign(parser, &dummy_code_item(), &inst, &mut locals)
        .unwrap()
        .into_iter()
        .flat_map(|s| match s {
            Statement {
                kind: StatementKind::Assign { dest, sources },
                ..
            } => sources
                .into_iter()
                .map(|s| (dest.base.clone(), s))
                .collect(),
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
/// produced it.
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
                } => sources
                    .into_iter()
                    .map(|s| (dest.base.clone(), s))
                    .collect(),
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
    let StatementKind::Assign {
        dest,
        sources: stored,
    } = &store.kind
    else {
        panic!("expected a store, got {store:?}");
    };
    assert!(!dest.is_pathless(), "expected a store, got {store:?}");
    let value = &stored[0];
    assert_eq!(value, &Exp::from(temp.clone()), "store must write the temp");
    let values = sources
        .iter()
        .map(|s| match s {
            Exp::Path(ap) if ap.is_pathless() => local_name(&locals, &ap.base),
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

/// The flows of `inst` as `(dest, source)` names, where every source must be the value of a
/// register (S19).
fn value_flows(inst: Instruction) -> Vec<(String, String)> {
    let (assigns, locals) = assign_from(inst);
    assigns
        .iter()
        .map(|(var, exp)| {
            let Exp::Value(src) = exp else {
                panic!("expected the value of a register, got {exp:?}");
            };
            assert!(src.is_pathless(), "{src:?}");
            (local_name(&locals, var), local_name(&locals, &src.base))
        })
        .collect()
}

/// `shl-long v0, v2, v4` computes `v0:v1 = v2:v3 << v4`. The shift amount is the int in `v4`, so
/// `v5` must not flow into the result.
#[test]
fn shl_long_reads_one_shift_register() {
    use dex_reader::instructions::Format23x;
    let flows = value_flows(Instruction::ShlLong(Format23x {
        a: Reg(0),
        b: Reg(2),
        c: Reg(4),
    }));
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

/// `invoke-polymorphic {v0, v1, v2}, MethodHandle.invoke, proto` calls whatever the handle in
/// `v0` refers to: an indirect call through `v0`, passing `v1` and `v2` (S11).
#[test]
fn invoke_polymorphic_is_an_indirect_call_through_the_handle() {
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
    let CallStyle::FuncPtrCall { callee, .. } = style else {
        panic!("expected an indirect call, got {style:?}");
    };
    assert!(callee.segments.is_empty(), "{callee}");
    assert_eq!(local_name(&locals, &callee.base), "v0");
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

/// const-method-handle and const-method-type define `vAA` as an object of their class.
#[test]
fn const_method_handle_and_type_assign_an_object() {
    use dex_reader::instructions::{Format21c, MethodHandleIdx, ProtoIdx};
    for (inst, cls) in [
        (
            Instruction::ConstMethodHandle(Format21c {
                a: Reg(0),
                idx: MethodHandleIdx(0),
            }),
            "Ljava/lang/invoke/MethodHandle;",
        ),
        (
            Instruction::ConstMethodType(Format21c {
                a: Reg(0),
                idx: ProtoIdx(0),
            }),
            "Ljava/lang/invoke/MethodType;",
        ),
    ] {
        let (assigns, locals) = assign_from(inst);
        assert_eq!(assigns.len(), 1);
        let (var, exp) = &assigns[0];
        assert_eq!(local_name(&locals, var), "v0");
        let Exp::ObjectRef(CallObject::JavaObject(got)) = exp else {
            panic!("expected a JavaObject, got {exp:?}");
        };
        assert_eq!(&**got, cls);
    }
}

/// Arithmetic, conversions, comparisons and `instance-of` supply values only (S19).
#[test]
fn arithmetic_and_comparisons_supply_values_only() {
    use dex_reader::instructions::{Format12x, Format22b, Format22c, Format23x};
    let pair = |d: &str, s: &str| (d.to_string(), s.to_string());
    assert_eq!(
        value_flows(Instruction::AddInt(Format23x {
            a: Reg(0),
            b: Reg(1),
            c: Reg(2),
        })),
        [pair("v0", "v1"), pair("v0", "v2")]
    );
    assert_eq!(
        value_flows(Instruction::AddInt2Addr(Format12x {
            a: Reg(0),
            b: Reg(1),
        })),
        [pair("v0", "v0"), pair("v0", "v1")]
    );
    assert_eq!(
        value_flows(Instruction::AddIntLit8(Format22b {
            a: Reg(0),
            b: Reg(1),
            c: 3,
        })),
        [pair("v0", "v1")]
    );
    assert_eq!(
        value_flows(Instruction::NegInt(Format12x {
            a: Reg(0),
            b: Reg(1),
        })),
        [pair("v0", "v1")]
    );
    // A conversion is a cast: a copy, into both halves of the wide destination.
    let (assigns, locals) = assign_from(Instruction::IntToLong(Format12x {
        a: Reg(0),
        b: Reg(2),
    }));
    let copies: Vec<(String, String)> = assigns
        .iter()
        .map(|(var, exp)| {
            let src = exp
                .variable_ref()
                .expect("a conversion copies its register");
            (local_name(&locals, var), local_name(&locals, src))
        })
        .collect();
    assert_eq!(copies, [pair("v0", "v2"), pair("v1", "v2")]);
    assert_eq!(
        value_flows(Instruction::CmpLong(Format23x {
            a: Reg(0),
            b: Reg(2),
            c: Reg(4),
        })),
        [
            pair("v0", "v2"),
            pair("v0", "v3"),
            pair("v0", "v4"),
            pair("v0", "v5")
        ]
    );
    assert_eq!(
        value_flows(Instruction::InstanceOf(Format22c {
            a: Reg(0),
            b: Reg(1),
            idx: TypeIdx(0),
        })),
        [pair("v0", "v1")]
    );
}

/// A move is a copy, not a value.
#[test]
fn move_is_a_copy() {
    use dex_reader::instructions::Format12x;
    let (assigns, locals) = assign_from(Instruction::MoveObject(Format12x {
        a: Reg(0),
        b: Reg(1),
    }));
    assert_eq!(assigns.len(), 1);
    let (var, exp) = &assigns[0];
    assert_eq!(local_name(&locals, var), "v0");
    let src = exp.variable_ref().expect("a copy of a register");
    assert_eq!(local_name(&locals, src), "v1");
}

/// A `const-method-handle` that invokes a method is a function reference to it, named as its
/// `FunctionData` is, and the method becomes a callee so that it gets a declaration (S11). A
/// constructor or field handle stays a `MethodHandle` object.
#[test]
fn const_method_handle_with_a_method_is_a_function_reference() {
    use dex_reader::instructions::{Format21c, MethodHandleIdx};
    use dex_reader::types::MethodHandle;

    let mut parser = parser_with_method_handle_invoke();
    // (method_handle_type, function reference?)
    let kinds = [
        (0x04, true),  // invoke-static
        (0x05, true),  // invoke-instance
        (0x07, true),  // invoke-direct
        (0x08, true),  // invoke-interface
        (0x06, false), // invoke-constructor
        (0x03, false), // instance-get
    ];
    parser.method_handles = Some(
        kinds
            .iter()
            .map(|&(method_handle_type, _)| MethodHandle {
                method_handle_type,
                field_or_method_id: 0,
            })
            .collect(),
    );
    let method = parser.method_ids[0];
    let name = parser.method_signature(&method).unwrap();
    for (i, (kind, is_ref)) in kinds.into_iter().enumerate() {
        let mut ctx = Context::new();
        let (assigns, _) = assign_from_in(
            &parser,
            &mut ctx,
            Instruction::ConstMethodHandle(Format21c {
                a: Reg(0),
                idx: MethodHandleIdx(i as u32),
            }),
        );
        assert_eq!(assigns.len(), 1);
        let expected = if is_ref {
            CallObject::FunctionPtr(name.as_str().into())
        } else {
            CallObject::JavaObject(JavaClass("Ljava/lang/invoke/MethodHandle;".into()))
        };
        assert_eq!(
            assigns[0].1,
            Exp::new_object_ref(expected),
            "kind {kind:#x}"
        );
        assert_eq!(ctx.call_to.contains(&method), is_ref, "kind {kind:#x}");
    }
}

/// A static field is a field of its class's global (R10): `sput-object Foo.cb` is the store
/// `$LFoo;.cb = v`, and `sget-object Foo.cb` the load `x = $LFoo;.cb`, the class declared once
/// by both.
#[test]
fn a_static_field_is_a_field_of_its_class_global() {
    use dex_reader::instructions::{FieldIdx, Format21c};
    let parser = parser_with_field();
    let mut ctx = Context::new();
    let mut locals = Locals::default();
    let mut stmts = Vec::new();
    for inst in [
        Instruction::SPutObject(Format21c {
            a: Reg(0),
            idx: FieldIdx(0),
        }),
        Instruction::SGetObject(Format21c {
            a: Reg(0),
            idx: FieldIdx(0),
        }),
    ] {
        stmts.extend(
            ctx.dataflow_to_assign(&parser, &dummy_code_item(), &inst, &mut locals)
                .unwrap(),
        );
    }
    let foo = ctx.globals.lookup("LFoo;").expect("the class is a global");
    assert_eq!(ctx.globals.len(), 1);
    let field = AccessPath::new(VariableRef::global(foo), [PathSegment::field("cb")]);
    let store = stmts
        .iter()
        .any(|s| matches!(&s.kind, StatementKind::Assign { dest, .. } if *dest == field));
    let load = stmts.iter().any(|s| {
        matches!(&s.kind, StatementKind::Assign { dest, sources }
            if dest.is_pathless() && sources.iter().any(|e| e.access_path() == Some(&field)))
    });
    assert!(store && load, "{stmts:?}");
}

/// A class's global is declared at its `class_def_item` (R27): on the committed APK, every
/// global the program uses that one of its dex files defines has a span of
/// [`dex_reader::types::ClassDef::ITEM_LEN`] bytes in that dex file whose first word is the
/// class's type index.
#[test]
fn a_class_global_is_declared_at_its_class_def_item() {
    let apk = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../xtask/tests/dex/com.noto_54.apk");
    let bytes = std::fs::read(&apk).expect("the APK");
    let dexes = dex_reader::apk::APKParser::new(&bytes).expect("the APK parses");
    let parsers = dexes.dex_parsers_with_filenames();
    let info = crate::import_apk(&apk)
        .expect("the APK imports")
        .program_info;

    let mut located = 0;
    for (_, global) in info.program.globals.iter_enumerated() {
        let Some(span) = global.span else {
            continue;
        };
        let (artifact, span) = info.source_info.resolve(span).expect("the span resolves");
        assert_eq!(
            span.len,
            source_info::SpanLen::ByteLen(dex_reader::types::ClassDef::ITEM_LEN)
        );
        let (_, parser) = &parsers[artifact.sub_artifact_id as usize];
        let at = span.start as usize;
        let class_idx = u32::from_le_bytes(parser.data[at..at + 4].try_into().unwrap());
        let class_def = parser
            .class_defs
            .iter()
            .find(|c| c.class_idx == class_idx)
            .expect("the span is at a class_def_item");
        assert_eq!(class_def.offset, span.start);
        assert_eq!(parser.class_name(class_def).unwrap(), *global.name);
        located += 1;
    }
    // The app's own classes hold statics; the framework's, which it only uses, are not in it.
    assert!(located > 0, "no global is located");
    assert!(
        located < info.program.globals.len(),
        "every global is located"
    );
}
