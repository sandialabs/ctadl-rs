//! Targeted tests of the flow layer on the `flow.il` fixture
//! (`readers/cil-reader/fixtures/flow.il`), one per case of
//! `docs/cil-flow-api.md` §9. Each is a plain `fn()` that panics on failure.

use cil_reader::cil::{Code, RawOperand};
use cil_reader::flow::delegate::{classify, DelegateOp, ModuleTypes, TypeOracle};
use cil_reader::flow::{
    CallKind, CallSite, Effect, Field, InsFlow, Path, Place, Root, Slot, TypeOp,
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
