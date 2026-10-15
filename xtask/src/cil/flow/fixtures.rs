//! Targeted tests of the flow layer on the `flow.il` fixture
//! (`readers/cil-reader/fixtures/flow.il`), one per case of
//! `docs/cil-flow-api.md` §9. Each is a plain `fn()` that panics on failure.

use cil_reader::cil::{Code, RawOperand};
use cil_reader::flow::check::layer2_violations;
use cil_reader::flow::delegate::{classify, DelegateOp, ModuleTypes, TypeOracle};
use cil_reader::flow::{
    CallKind, CallSite, Effect, Field, FlowOptions, FlowWarning, HandlerKind, InsFlow, InsId,
    MethodFlow, Path, Place, Root, Slot, StackKind, TypeOp, Var,
};
use cil_reader::model::{MethodBody, MethodDef};
use cil_reader::Module;

use super::read_fixture;

type Test = (&'static str, fn());

pub const TESTS: &[Test] = &[
    ("reader_helpers", reader_helpers),
    ("out_param", out_param),
    ("ref_param", ref_param),
    ("constrained_callvirt", constrained_callvirt),
    ("delegates", delegates),
    ("delegate_oracle", delegate_oracle),
    ("multi_dimensional_arrays", multi_dimensional_arrays),
    ("calli", calli),
    ("varargs", varargs),
    ("prefixes", prefixes),
    ("struct_ops", struct_ops),
    ("names", names),
    ("l2_out_param", l2_out_param),
    ("l2_struct_fields", l2_struct_fields),
    ("l2_ref_param", l2_ref_param),
    ("l2_filter", l2_filter),
    ("l2_catch_finally", l2_catch_finally),
    ("l2_split_after_throwing", l2_split_after_throwing),
    ("l2_newobj_args", l2_newobj_args),
    ("l2_dup_stfld", l2_dup_stfld),
    ("l2_address_join", l2_address_join),
    ("l2_height_mismatch", l2_height_mismatch),
    ("l2_kinds", l2_kinds),
    ("l2_whole_fixture", l2_whole_fixture),
    ("client_without_code", client_without_code),
];

const FLOW: &str = "flow.il.dll";

/// Runs `f` on the module of the flow fixture.
fn with_module(f: impl FnOnce(&Module<'_>)) {
    let data = read_fixture(FLOW);
    let module = Module::read(&data).unwrap();
    f(&module);
}

fn method<'a>(module: &'a Module<'a>, ty: &str, name: &str) -> MethodDef<'a> {
    let ty = module
        .get_type(ty)
        .unwrap()
        .unwrap_or_else(|| panic!("no type {ty}"));
    ty.methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == name)
        .unwrap_or_else(|| panic!("no method {name}"))
}

fn flows(body: &MethodBody<'_>) -> Vec<InsFlow> {
    body.flows().map(|f| f.unwrap()).collect()
}

/// The record of the first instruction with opcode `code`.
fn find(flows: &[InsFlow], code: Code) -> &InsFlow {
    flows
        .iter()
        .find(|f| f.opcode.code == code)
        .unwrap_or_else(|| panic!("no {code:?}"))
}

fn call_site(flow: &InsFlow) -> &CallSite<Slot> {
    flow.effects
        .iter()
        .find_map(|e| match e {
            Effect::Call(site) => Some(site),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} has no call", flow.opcode))
}

fn reader_helpers() {
    let data = read_fixture(FLOW);
    let module = Module::read(&data).unwrap();
    // Ins::size tiles the code, and code_file_offset points at it.
    let m = method(&module, "Flow", "Blocks");
    let body = m.body().unwrap();
    let mut end = 0;
    for ins in &body.instructions {
        assert_eq!(ins.offset, end);
        end = ins.end_offset();
    }
    assert_eq!(end, body.code_size);
    let start = body.code_file_offset() as usize;
    // ldc.i4.s 16; localloc (fe 0f)
    assert_eq!(&data[start..start + 4], &[0x1f, 16, 0xfe, 0x0f]);
    // user_string
    let m = method(&module, "Flow", "CallVarArgs");
    let body = m.body().unwrap();
    let ldstr = body
        .instructions
        .iter()
        .find(|i| i.opcode.code == Code::Ldstr)
        .unwrap();
    let RawOperand::String(tok) = ldstr.operand else {
        panic!()
    };
    assert_eq!(body.user_string(tok).unwrap(), "x");
    // MethodDef flags
    let twice = method(&module, "Holder", "Twice");
    assert!(twice.is_virtual().unwrap() && twice.is_newslot().unwrap());
    assert!(!twice.is_abstract().unwrap() && !twice.is_final().unwrap());
    let stat = method(&module, "Flow", "Twice");
    assert!(!stat.is_virtual().unwrap());
}

fn out_param() {
    with_module(|module| {
        let body = method(module, "Flow", "OutParam").body().unwrap();
        let flows = flows(&body);
        let site = call_site(find(&flows, Code::Call));
        assert_eq!(site.kind, CallKind::Static);
        assert_eq!(site.args, [Slot::In(0), Slot::In(1)]);
        assert_eq!(site.byref_args, [1]);
        assert_eq!(site.result, Some(Slot::Out(0)));
        assert_eq!(
            find(&flows, Code::Ldloca_S).effects,
            [Effect::AddressOf {
                dst: Slot::Out(0),
                place: Place::Local(0)
            }]
        );
    });
}

fn ref_param() {
    with_module(|module| {
        let body = method(module, "Flow", "Inc").body().unwrap();
        let flows = flows(&body);
        let store = find(&flows, Code::Stind_I4);
        assert_eq!(
            store.effects,
            [Effect::Store {
                dst: Path {
                    root: Root::Slot(Slot::In(0)),
                    field: Field::Deref
                },
                src: Slot::In(1)
            }]
        );
    });
}

fn constrained_callvirt() {
    with_module(|module| {
        let body = method(module, "Flow", "Constrained").body().unwrap();
        let flows = flows(&body);
        let flow = find(&flows, Code::Callvirt);
        let site = call_site(flow);
        let CallKind::Virtual {
            constrained: Some(t),
        } = site.kind
        else {
            panic!("{:?}", site.kind)
        };
        assert_eq!(body.type_name(&t).unwrap(), "T");
        assert_eq!(flow.prefixes.constrained, Some(t));
        // The record covers the prefix.
        assert_eq!(flow.offset, 2);
        assert_eq!(flow.size, 11);
        assert_eq!(site.receiver, Some(Slot::In(0)));
    });
}

fn delegates() {
    with_module(|module| {
        let body = method(module, "Flow", "Delegates").body().unwrap();
        let flows = flows(&body);
        let ctor = call_site(find(&flows, Code::Newobj));
        assert_eq!(
            classify(ctor, &body),
            Some(DelegateOp::Ctor {
                target: Slot::In(0),
                fnptr: Slot::In(1)
            })
        );
        let invoke = call_site(find(&flows, Code::Callvirt));
        assert_eq!(classify(invoke, &body), Some(DelegateOp::Invoke));
        // Not a delegate: Holder's constructor in NewobjArgs.
        let body = method(module, "Flow", "NewobjArgs").body().unwrap();
        let flows = self::flows(&body);
        assert_eq!(classify(call_site(find(&flows, Code::Newobj)), &body), None);
        // ldvirtftn's receiver is reported.
        let body = method(module, "Flow", "Lambdas").body().unwrap();
        let flows = self::flows(&body);
        let Effect::FunctionPtr { receiver, .. } = &find(&flows, Code::Ldvirtftn).effects[0] else {
            panic!()
        };
        assert_eq!(*receiver, Some(Slot::In(0)));
    });
}

fn delegate_oracle() {
    with_module(|module| {
        let types = ModuleTypes::new(module);
        let fn_ty = module.get_type("Fn").unwrap().unwrap();
        let holder = module.get_type("Holder").unwrap().unwrap();
        let point = module.get_type("Point").unwrap().unwrap();
        let op = |t: cil_reader::model::TypeDef<'_>| TypeOp { token: t.token() };
        assert_eq!(types.is_delegate(&op(fn_ty)), Some(true));
        assert_eq!(types.is_delegate(&op(holder)), Some(false));
        assert_eq!(types.is_delegate(&op(point)), Some(false));
    });
}

fn multi_dimensional_arrays() {
    with_module(|module| {
        let body = method(module, "Flow", "MultiArray").body().unwrap();
        let flows = flows(&body);
        let effects: Vec<String> = flows
            .iter()
            .filter(|f| matches!(f.opcode.code, Code::Call | Code::Newobj))
            .map(|f| {
                f.effects
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .collect();
        assert_eq!(effects.len(), 4);
        assert!(effects[0].starts_with("New o0 Array("), "{}", effects[0]);
        assert!(effects[0].ends_with("sizes=[i0, i1]"), "{}", effects[0]);
        assert_eq!(effects[1], "Store i0[i1, i2] := i3");
        assert_eq!(effects[2], "Load o0 := i0[i1, i2]");
        assert_eq!(effects[3], "AddressOf o0 = &i0[i1, i2]");
    });
}

fn calli() {
    with_module(|module| {
        let body = method(module, "Flow", "Calli").body().unwrap();
        let flows = flows(&body);
        let site = call_site(find(&flows, Code::Calli));
        assert_eq!(
            site.kind,
            CallKind::Indirect {
                callee: Slot::In(1)
            }
        );
        assert_eq!(site.method, None);
        assert_eq!(site.args, [Slot::In(0)]);
        assert_eq!(site.result, Some(Slot::Out(0)));
    });
}

fn varargs() {
    with_module(|module| {
        let body = method(module, "Flow", "CallVarArgs").body().unwrap();
        let flows = flows(&body);
        let site = call_site(find(&flows, Code::Call));
        assert_eq!(site.args.len(), 3);
        assert!(site.sig.params[1].is_sentinel());
        let body = method(module, "Flow", "VarArgs").body().unwrap();
        let flows = self::flows(&body);
        assert_eq!(
            find(&flows, Code::Arglist).effects[0].to_string(),
            "Const o0 = ArgList"
        );
    });
}

fn prefixes() {
    with_module(|module| {
        let body = method(module, "Flow", "Prefixes").body().unwrap();
        let flows = flows(&body);
        assert!(find(&flows, Code::Ldelema).prefixes.readonly);
        assert!(find(&flows, Code::Ldfld).prefixes.volatile);
        let call = find(&flows, Code::Call);
        assert!(call.prefixes.tail && call_site(call).prefixes.tail);
        for f in &flows {
            assert!(f.prefixes.apply_to(f.opcode.code), "{}", f.opcode);
        }
        let body = method(module, "Flow", "Blocks").body().unwrap();
        let flows = self::flows(&body);
        assert_eq!(find(&flows, Code::Cpblk).prefixes.unaligned, Some(1));
    });
}

fn struct_ops() {
    with_module(|module| {
        let body = method(module, "Flow", "ClearCopy").body().unwrap();
        let flows = flows(&body);
        let rendered = |code| {
            find(&flows, code)
                .effects
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(rendered(Code::Initobj), ["Clear i0"]);
        assert_eq!(
            rendered(Code::Cpobj),
            ["Load t0 := i1.*", "Store i0.* := t0"]
        );
        assert_eq!(rendered(Code::Ldobj), ["Load o0 := i0.*"]);
        assert_eq!(rendered(Code::Stobj), ["Store i0.* := i1"]);
        // newobj of a struct is a ValueType allocation.
        let body = method(module, "Flow", "NestedField").body().unwrap();
        let flows = self::flows(&body);
        assert!(matches!(
            find(&flows, Code::Ldflda).effects[0],
            Effect::AddressOf {
                place: Place::Field(Slot::In(0), _),
                ..
            }
        ));
    });
}

fn names() {
    with_module(|module| {
        let body = method(module, "Flow", "Prefixes").body().unwrap();
        let flows = flows(&body);
        let Effect::Load { src, .. } = &find(&flows, Code::Ldfld).effects[0] else {
            panic!()
        };
        let Field::Named(f) = &src.field else {
            panic!()
        };
        assert_eq!(body.field_name(f).unwrap(), "System.Int32 Holder::g");
        let site = call_site(find(&flows, Code::Call));
        let m = site.method.as_ref().unwrap();
        assert_eq!(
            body.method_name(m).unwrap(),
            "System.Int32 Flow::Twice(System.Int32)"
        );
        assert_eq!(
            body.type_name(&body.declaring_type(m).unwrap()).unwrap(),
            "Flow"
        );
    });
}

// ---- Layer 2 ---------------------------------------------------------------

/// Layer 2 of `Flow::name`, checked against §7.7–7.9.
fn method_flow(module: &Module<'_>, name: &str) -> (MethodFlow, Vec<String>) {
    method_flow_with(module, name, FlowOptions::default())
}

fn method_flow_with(
    module: &Module<'_>,
    name: &str,
    options: FlowOptions,
) -> (MethodFlow, Vec<String>) {
    let body = method(module, "Flow", name).body().unwrap();
    let mf = MethodFlow::build_with(&body, options);
    let violations = layer2_violations(&mf);
    assert!(violations.is_empty(), "{name}: {violations:?}");
    (mf, Vec::new())
}

/// The id of the first instruction with opcode `code`.
fn at(mf: &MethodFlow, code: Code) -> InsId {
    mf.instructions()
        .find(|(_, ins)| ins.flow.opcode.code == code && !ins.is_handler_entry())
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no {code:?}"))
}

fn bound(mf: &MethodFlow, id: InsId) -> Vec<String> {
    mf.ins(id).effects.iter().map(|e| e.to_string()).collect()
}

fn l2_out_param() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "OutParam");
        let call = at(&mf, Code::Call);
        assert_eq!(mf.resolve_address(call, 1), Some(&Place::Local(0)));
        assert_eq!(mf.resolve_address(call, 0), None);
        assert!(mf.address_taken().contains(Var::Local(0)));
        assert!(!mf.address_escapes_resolution(at(&mf, Code::Ldloca_S)));
        assert_eq!(
            bound(&mf, call),
            ["Call Static 0a000002 args=[s0, s1] result=s0 thrown=thrown byref=[1]"]
        );
        assert!(mf.warnings.is_empty(), "{:?}", mf.warnings);
    });
}

fn l2_struct_fields() {
    with_module(|module| {
        // ldloca + stfld/ldfld: the base is a pointer to the local.
        let (mf, _) = method_flow(module, "StructField");
        let stfld = at(&mf, Code::Stfld);
        assert_eq!(mf.resolve_address(stfld, 0), Some(&Place::Local(0)));
        assert_eq!(mf.ins(stfld).input_kinds[0], StackKind::ManagedPtr);
        let ldfld = at(&mf, Code::Ldfld);
        assert_eq!(mf.resolve_address(ldfld, 0), Some(&Place::Local(0)));
        // ldarg.0; ldflda p; ldfld Y: the address is arg0.p.
        let (mf, _) = method_flow(module, "NestedField");
        let ldfld = at(&mf, Code::Ldfld);
        let Some(Place::Field(Var::Arg(0), f)) = mf.resolve_address(ldfld, 0) else {
            panic!("{:?}", mf.resolve_address(ldfld, 0))
        };
        let body = method(module, "Flow", "NestedField").body().unwrap();
        assert_eq!(body.field_name(f).unwrap(), "Point Holder::p");
        // initobj/cpobj through ldloca.
        let (mf, _) = method_flow(module, "ClearCopy");
        assert_eq!(
            mf.resolve_address(at(&mf, Code::Initobj), 0),
            Some(&Place::Local(0))
        );
        let cpobj = at(&mf, Code::Cpobj);
        assert_eq!(mf.resolve_address(cpobj, 0), None); // the byref parameter
        assert_eq!(mf.resolve_address(cpobj, 1), Some(&Place::Local(0)));
    });
}

fn l2_ref_param() {
    with_module(|module| {
        // A byref parameter is not traceable.
        let (mf, _) = method_flow(module, "Inc");
        let stind = at(&mf, Code::Stind_I4);
        assert_eq!(mf.resolve_address(stind, 0), None);
        assert_eq!(mf.ins(stind).input_kinds[0], StackKind::ManagedPtr);
        let (mf, _) = method_flow(module, "CallInc");
        assert_eq!(
            mf.resolve_address(at(&mf, Code::Call), 0),
            Some(&Place::Local(0))
        );
    });
}

fn l2_filter() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "Filter");
        assert_eq!(mf.handlers.len(), 1);
        let h = &mf.handlers[0];
        assert_eq!(h.kind, HandlerKind::Filter);
        let (filter, handler) = (h.entry.unwrap(), h.handler_block.unwrap());
        assert_ne!(filter, handler);
        for b in [filter, handler] {
            let block = &mf.blocks[b];
            assert_eq!(block.entry_height, 1);
            assert_eq!(block.entry_kinds, [StackKind::ObjRef]);
            assert!(block.instrs[0].is_handler_entry());
            assert_eq!(
                bound(&mf, InsId { block: b, index: 0 }),
                ["Assign s0 := [caught0] Copy"]
            );
        }
        // The try block's exception edge goes to the filter; endfilter
        // goes on to the handler.
        assert!(mf.blocks[0].exc_succs.contains(&filter));
        let endfilter = at(&mf, Code::Endfilter);
        assert_eq!(mf.blocks[endfilter.block].succs, [handler]);
        assert_eq!(bound(&mf, endfilter), ["Test [s0]"]);
    });
}

fn l2_catch_finally() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "CatchFinally");
        let catch = mf
            .handlers
            .iter()
            .position(|h| h.kind == HandlerKind::Catch)
            .unwrap();
        let finally = &mf.handlers[mf
            .handlers
            .iter()
            .position(|h| h.kind == HandlerKind::Finally)
            .unwrap()];
        let catch_entry = mf.handlers[catch].entry.unwrap();
        let finally_entry = finally.entry.unwrap();
        assert_eq!(mf.blocks[catch_entry].entry_height, 1);
        assert_eq!(mf.blocks[finally_entry].entry_height, 0);
        assert!(!mf.blocks[finally_entry].instrs[0].is_handler_entry());
        // rethrow throws what the catch caught.
        assert_eq!(
            bound(&mf, at(&mf, Code::Rethrow)),
            [format!("Throw caught{catch}")]
        );
        // The try body reaches both handlers; the catch reaches the finally.
        assert!(mf.blocks[0].exc_succs.contains(&catch_entry));
        assert!(mf.blocks[0].exc_succs.contains(&finally_entry));
        assert!(mf.blocks[catch_entry].exc_succs.contains(&finally_entry));
        // leave END goes through the finally, which goes on to END.
        let ret = mf.block_at(mf.ins(at(&mf, Code::Ret)).flow.offset).unwrap();
        let leaves: Vec<InsId> = mf
            .instructions()
            .filter(|(_, i)| matches!(i.flow.opcode.code, Code::Leave_S))
            .map(|(id, _)| id)
            .collect();
        let outer = *leaves.last().unwrap();
        let succs = &mf.blocks[outer.block].succs;
        assert!(
            succs.contains(&ret) && succs.contains(&finally_entry),
            "{succs:?}"
        );
        let endfinally = at(&mf, Code::Endfinally);
        assert_eq!(mf.blocks[endfinally.block].succs, [ret]);
        assert_eq!(mf.blocks[ret].entry_height, 0);
    });
}

fn l2_split_after_throwing() {
    with_module(|module| {
        let (plain, _) = method_flow(module, "CatchFinally");
        let (split, _) = method_flow_with(
            module,
            "CatchFinally",
            FlowOptions {
                split_after_throwing_in_try: true,
            },
        );
        assert!(split.blocks.len() > plain.blocks.len());
        // Every call in a try range ends its block.
        for (id, ins) in split.instructions() {
            if ins.effects.iter().any(|e| matches!(e, Effect::Call(_)))
                && split
                    .handlers
                    .iter()
                    .any(|h| (h.try_start..h.try_end).contains(&ins.flow.offset))
            {
                assert_eq!(id.index + 1, split.blocks[id.block].instrs.len());
                assert!(!split.blocks[id.block].exc_succs.is_empty());
            }
        }
    });
}

fn l2_newobj_args() {
    with_module(|module| {
        // ldnull; ldarg.0; ldc.i4.1; newobj Holder(object, int32): height 3.
        let (mf, _) = method_flow(module, "NewobjArgs");
        let newobj = at(&mf, Code::Newobj);
        assert_eq!(mf.ins(newobj).height, 3);
        assert_eq!(mf.ins(newobj).inputs, [Var::Stack(1), Var::Stack(2)]);
        assert_eq!(mf.ins(newobj).outputs, [Var::Stack(1)]);
        assert_eq!(
            bound(&mf, newobj),
            [
                "New t0 Object(02000003)",
                "Call Ctor 06000003 recv=t0 args=[s1, s2] thrown=thrown",
                "Assign s1 := [t0] Copy",
            ]
        );
        assert_eq!(
            bound(&mf, at(&mf, Code::Stsfld)),
            ["Store static.<static 04000006> := s1"]
        );
        assert_eq!(bound(&mf, at(&mf, Code::Ret)), ["Return s0"]);
    });
}

fn l2_dup_stfld() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "DupStfld");
        assert_eq!(
            bound(&mf, at(&mf, Code::Dup)),
            ["Assign s0 := [s0] Copy", "Assign s1 := [s0] Copy"]
        );
        assert_eq!(
            bound(&mf, at(&mf, Code::Stfld)),
            ["Store s1.<04000003> := s2"]
        );
        assert_eq!(bound(&mf, at(&mf, Code::Ret)), ["Return s0"]);
        assert_eq!(
            mf.ins(at(&mf, Code::Stfld)).input_kinds,
            [StackKind::ObjRef, StackKind::ObjRef]
        );
    });
}

fn l2_address_join() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "AddressJoin");
        let stores: Vec<InsId> = mf
            .instructions()
            .filter(|(_, i)| i.flow.opcode.code == Code::Stind_I4)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(stores.len(), 2);
        // c ? &a : &b — two places: unresolved, and both addresses escape.
        assert_eq!(mf.resolve_address(stores[0], 0), None);
        // c ? &a : &a — one place.
        assert_eq!(mf.resolve_address(stores[1], 0), Some(&Place::Local(0)));
        let loads: Vec<InsId> = mf
            .instructions()
            .filter(|(_, i)| i.flow.opcode.code == Code::Ldloca_S)
            .map(|(id, _)| id)
            .collect();
        let escapes: Vec<bool> = loads
            .iter()
            .map(|&id| mf.address_escapes_resolution(id))
            .collect();
        assert_eq!(escapes, [true, true, false, false]);
        let taken: Vec<Var> = mf.address_taken().iter().collect();
        assert_eq!(taken, [Var::Local(0), Var::Local(1)]);
    });
}

fn l2_height_mismatch() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "HeightMismatch");
        let pop = at(&mf, Code::Pop);
        let join = &mf.blocks[pop.block];
        assert_eq!(join.entry_height, 1);
        assert_eq!(join.preds.len(), 2);
        assert!(mf.warnings.contains(&FlowWarning::HeightMismatch {
            offset: join.start,
            heights: (1, 2)
        }));
        // Still bound: the pop takes the kept depth.
        assert_eq!(mf.ins(pop).inputs, [Var::Stack(0)]);
    });
}

fn l2_kinds() {
    with_module(|module| {
        let (mf, _) = method_flow(module, "Misc");
        let kinds = |code| mf.ins(at(&mf, code)).input_kinds.clone();
        assert_eq!(kinds(Code::Ldlen), [StackKind::ObjRef]);
        assert_eq!(
            kinds(Code::Stelem_I4),
            [StackKind::ObjRef, StackKind::Int32, StackKind::Int32]
        );
        // ldlen pushes a native int, conv.i4 makes it an int32.
        assert_eq!(kinds(Code::Conv_I4), [StackKind::NativeInt]);
        assert_eq!(kinds(Code::Add), [StackKind::Int32, StackKind::Int32]);
        assert_eq!(kinds(Code::Box), [StackKind::Int32]);
        assert_eq!(kinds(Code::Ckfinite), [StackKind::Float]);
        assert_eq!(kinds(Code::Mkrefany), [StackKind::ManagedPtr]);
        let (mf, _) = method_flow(module, "Blocks");
        assert_eq!(
            mf.ins(at(&mf, Code::Stloc_0)).input_kinds,
            [StackKind::NativeInt]
        );
        let (mf, _) = method_flow(module, "MultiArray");
        assert_eq!(
            mf.ins(at(&mf, Code::Ldind_I4)).input_kinds,
            [StackKind::ManagedPtr]
        );
        assert_eq!(mf.ins(at(&mf, Code::Pop)).input_kinds, [StackKind::Int32]);
    });
}

fn l2_whole_fixture() {
    // Every method of the fixture holds §7.7–7.9, and only HeightMismatch
    // warns.
    with_module(|module| {
        for ty in module.types().unwrap() {
            for m in ty.methods().unwrap() {
                if !m.has_body().unwrap() || m.rva().unwrap() == 0 {
                    continue;
                }
                let body = m.body().unwrap();
                let mf = MethodFlow::build(&body);
                let name = m.name().unwrap();
                assert_eq!(layer2_violations(&mf), Vec::<String>::new(), "{name}");
                if name != "HeightMismatch" {
                    assert!(mf.warnings.is_empty(), "{name}: {:?}", mf.warnings);
                }
            }
        }
    });
}

/// The acceptance criterion of §9: a client that matches only on Effect,
/// CallKind and Control (see `client.rs`) answers a real question.
fn client_without_code() {
    with_module(|module| {
        let reaching = |name: &str| {
            let m = method(module, "Flow", name);
            let params = m.signature().unwrap().params.len() as u16;
            let mf = MethodFlow::build(&m.body().unwrap());
            super::client::args_reaching_return(&mf, params)
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(reaching("Twice"), [0]);
        // Through int.TryParse's out parameter: byref_args + resolve_address.
        assert_eq!(reaching("OutParam"), [0]);
        assert_eq!(reaching("Calli"), [0]);
        assert_eq!(reaching("NestedField"), [0]);
        // Through the address of the argument, as the call's receiver.
        assert_eq!(reaching("Constrained"), [0]);
        assert_eq!(reaching("Misc"), [0]);
        // The argument only decides branches: no implicit flow here.
        assert_eq!(reaching("AddressJoin"), Vec::<u16>::new());
    });
}
