//! Per-opcode tests of Layer 1, one per family of the per-opcode table,
//! on hand-assembled IL bytes and a stub [`Operands`]: no assembly needed.

use super::check::layer1_violations;
use super::operands::{ArrayMethod, FlowCx, Operands, ResolvedMethod};
use super::*;
use crate::cil::opcodes::{self, OpCode};
use crate::cil::{InstructionDecoder, OperandType};
use crate::error::{Error, Result};
use crate::model::{MethodSig, TypeSig};

// ---- a tiny IL encoder -----------------------------------------------------

/// An operand for [`enc`].
#[derive(Clone)]
enum A {
    None,
    I(i64),
    R(f64),
    Tok(u32),
    /// A branch target, relative to the end of the instruction.
    Rel(i32),
    Switch(Vec<i32>),
}

/// Encodes one instruction.
fn enc(op: OpCode, a: A) -> Vec<u8> {
    let mut out = if op.op1 == 0xff {
        vec![op.op2]
    } else {
        vec![op.op1, op.op2]
    };
    use OperandType as O;
    match (op.operand_type, a) {
        (O::InlineNone, A::None) => {}
        (O::ShortInlineI | O::ShortInlineVar | O::ShortInlineArg, A::I(v)) => out.push(v as u8),
        (O::InlineVar | O::InlineArg, A::I(v)) => out.extend((v as u16).to_le_bytes()),
        (O::InlineI, A::I(v)) => out.extend((v as i32).to_le_bytes()),
        (O::InlineI8, A::I(v)) => out.extend(v.to_le_bytes()),
        (O::ShortInlineR, A::R(v)) => out.extend((v as f32).to_le_bytes()),
        (O::InlineR, A::R(v)) => out.extend(v.to_le_bytes()),
        (O::ShortInlineBrTarget, A::Rel(d)) => out.push(d as i8 as u8),
        (O::InlineBrTarget, A::Rel(d)) => out.extend(d.to_le_bytes()),
        (O::InlineSwitch, A::Switch(ts)) => {
            out.extend((ts.len() as u32).to_le_bytes());
            for t in ts {
                out.extend(t.to_le_bytes());
            }
        }
        (
            O::InlineTok
            | O::InlineType
            | O::InlineMethod
            | O::InlineField
            | O::InlineString
            | O::InlineSig,
            A::Tok(t),
        ) => out.extend(t.to_le_bytes()),
        (ty, _) => panic!("operand does not fit {ty:?}"),
    }
    out
}

// ---- a stub resolver ---------------------------------------------------------

const FIELD: u32 = 0x0400_0001;
const STRUCT_TYPE: u32 = 0x0200_0002;
const CLASS_TYPE: u32 = 0x0200_0003;
const ARRAY_SPEC: u32 = 0x1b00_0001;
const GENERIC_PARAM_SPEC: u32 = 0x1b00_0002;
const BAD: u32 = 0x0a00_dead;

const STATIC_VOID_2: u32 = 0x0600_0001; // static void M(int, ref int)
const INSTANCE_INT_1: u32 = 0x0600_0002; // instance int M(int)
const CLASS_CTOR: u32 = 0x0600_0003; // instance void C::.ctor(int, int)
const STRUCT_CTOR: u32 = 0x0600_0004; // instance void S::.ctor(int)
const ARRAY_GET: u32 = 0x0a00_0001; // instance int32 int32[,]::Get(int, int)
const ARRAY_SET: u32 = 0x0a00_0002; // instance void int32[,]::Set(int, int, int32)
const ARRAY_ADDRESS: u32 = 0x0a00_0003; // instance int32& int32[,]::Address(int, int)
const ARRAY_CTOR: u32 = 0x0a00_0004; // instance void int32[,]::.ctor(int, int)
const VARARG: u32 = 0x0a00_0005; // vararg void M(int, ..., string)
const CALLI_SIG: u32 = 0x1100_0001; // instance int (string, int)

fn sig(has_this: bool, ret: TypeSig, params: Vec<TypeSig>) -> MethodSig {
    MethodSig {
        has_this,
        explicit_this: false,
        calling_convention: 0,
        generic_arity: 0,
        return_type: ret,
        params,
    }
}

struct Stub;

impl Operands for Stub {
    fn field(&self, token: MetadataToken, is_static: bool) -> Result<FieldOp> {
        if token.0 == BAD {
            return Err(Error::Unsupported("stub"));
        }
        Ok(FieldOp {
            token,
            is_static,
            ty: TypeSig::I4,
        })
    }

    fn method(&self, token: MetadataToken) -> Result<ResolvedMethod> {
        let byref_int = TypeSig::ByRef(Box::new(TypeSig::I4));
        let (s, declaring, vt, arr) = match token.0 {
            STATIC_VOID_2 => (
                sig(false, TypeSig::Void, vec![TypeSig::I4, byref_int]),
                CLASS_TYPE,
                Some(false),
                None,
            ),
            INSTANCE_INT_1 => (
                sig(true, TypeSig::I4, vec![TypeSig::I4]),
                CLASS_TYPE,
                Some(false),
                None,
            ),
            CLASS_CTOR => (
                sig(true, TypeSig::Void, vec![TypeSig::I4, TypeSig::I4]),
                CLASS_TYPE,
                Some(false),
                None,
            ),
            STRUCT_CTOR => (
                sig(true, TypeSig::Void, vec![TypeSig::I4]),
                STRUCT_TYPE,
                Some(true),
                None,
            ),
            ARRAY_GET => (
                sig(true, TypeSig::I4, vec![TypeSig::I4, TypeSig::I4]),
                ARRAY_SPEC,
                Some(false),
                Some(ArrayMethod::Get),
            ),
            ARRAY_SET => (
                sig(true, TypeSig::Void, vec![TypeSig::I4; 3]),
                ARRAY_SPEC,
                Some(false),
                Some(ArrayMethod::Set),
            ),
            ARRAY_ADDRESS => (
                sig(true, byref_int, vec![TypeSig::I4, TypeSig::I4]),
                ARRAY_SPEC,
                Some(false),
                Some(ArrayMethod::Address),
            ),
            ARRAY_CTOR => (
                sig(true, TypeSig::Void, vec![TypeSig::I4, TypeSig::I4]),
                ARRAY_SPEC,
                Some(false),
                Some(ArrayMethod::Ctor),
            ),
            VARARG => {
                let mut s = sig(
                    false,
                    TypeSig::Void,
                    vec![TypeSig::I4, TypeSig::Sentinel(Box::new(TypeSig::String))],
                );
                s.calling_convention = crate::model::sig::calling_convention::VAR_ARG;
                (s, CLASS_TYPE, Some(false), None)
            }
            _ => return Err(Error::Unsupported("stub")),
        };
        Ok(ResolvedMethod {
            op: MethodOp {
                token,
                sig: s,
                generic_args: vec![],
            },
            declaring: Some(TypeOp {
                token: MetadataToken(declaring),
            }),
            declaring_is_value_type: vt,
            array: arr,
        })
    }

    fn call_site(&self, token: MetadataToken) -> Result<MethodSig> {
        if token.0 == CALLI_SIG {
            Ok(sig(true, TypeSig::I4, vec![TypeSig::String, TypeSig::I4]))
        } else {
            Err(Error::Unsupported("stub"))
        }
    }

    fn is_value_type(&self, ty: &TypeOp) -> Option<bool> {
        match ty.token.0 {
            STRUCT_TYPE => Some(true),
            CLASS_TYPE | ARRAY_SPEC => Some(false),
            _ => None,
        }
    }
}

fn void_method() -> MethodSig {
    sig(false, TypeSig::Void, vec![])
}

/// Layer 1 of the first instruction of `code`, checked against the Layer 1
/// invariants of [`super::check`].
fn flow_with(code: &[u8], prefixes: Prefixes, method_sig: &MethodSig) -> InsFlow {
    let ins = InstructionDecoder::new(code).next().unwrap().unwrap();
    let cx = FlowCx::new(&Stub, method_sig);
    let flow = instruction_flow(&ins, prefixes, &cx).unwrap();
    let violations = layer1_violations(&ins, &flow, &cx);
    assert!(violations.is_empty(), "{}: {violations:?}", ins.opcode);
    flow
}

fn flow(op: OpCode, a: A) -> InsFlow {
    flow_with(&enc(op, a), Prefixes::default(), &void_method())
}

fn effects(f: &InsFlow) -> Vec<String> {
    f.effects.iter().map(|e| e.to_string()).collect()
}

/// Asserts the roles, the push count and the effects (as rendered).
#[track_caller]
fn check(f: &InsFlow, roles: &[Role], outputs: u8, expected: &[&str]) {
    assert_eq!(f.inputs, roles, "{}", f.opcode);
    assert_eq!(f.outputs, outputs, "{}", f.opcode);
    assert_eq!(effects(f), expected, "{}", f.opcode);
}

use Role::*;
use opcodes as op;

// ---- Locals, arguments, constants, stack ------------------------------------

#[test]
fn locals_arguments_constants_stack() {
    check(&flow(op::NOP, A::None), &[], 0, &[]);
    check(&flow(op::BREAK, A::None), &[], 0, &[]);
    check(
        &flow(op::LDARG_2, A::None),
        &[],
        1,
        &["Assign o0 := [arg2] Copy"],
    );
    check(
        &flow(op::LDARG, A::I(300)),
        &[],
        1,
        &["Assign o0 := [arg300] Copy"],
    );
    check(
        &flow(op::LDLOC_S, A::I(9)),
        &[],
        1,
        &["Assign o0 := [loc9] Copy"],
    );
    check(
        &flow(op::STLOC_1, A::None),
        &[Value],
        0,
        &["Assign loc1 := [i0] Copy"],
    );
    check(
        &flow(op::STARG_S, A::I(1)),
        &[Value],
        0,
        &["Assign arg1 := [i0] Copy"],
    );
    check(
        &flow(op::LDARGA_S, A::I(1)),
        &[],
        1,
        &["AddressOf o0 = &arg1"],
    );
    check(
        &flow(op::LDLOCA, A::I(4)),
        &[],
        1,
        &["AddressOf o0 = &loc4"],
    );
    check(&flow(op::LDNULL, A::None), &[], 1, &["Const o0 = Null"]);
    check(
        &flow(op::LDC_I4_M1, A::None),
        &[],
        1,
        &["Const o0 = I4(-1)"],
    );
    check(
        &flow(op::LDC_I4_S, A::I(-5)),
        &[],
        1,
        &["Const o0 = I4(-5)"],
    );
    check(
        &flow(op::LDC_I4, A::I(70000)),
        &[],
        1,
        &["Const o0 = I4(70000)"],
    );
    check(
        &flow(op::LDC_I8, A::I(1 << 40)),
        &[],
        1,
        &["Const o0 = I8(1099511627776)"],
    );
    assert_eq!(
        flow(op::LDC_R4, A::R(1.5)).effects,
        [Effect::Const {
            dst: Slot::Out(0),
            value: Const::R4(1.5)
        }]
    );
    assert_eq!(
        flow(op::LDC_R8, A::R(-2.25)).effects,
        [Effect::Const {
            dst: Slot::Out(0),
            value: Const::R8(-2.25)
        }]
    );
    check(
        &flow(op::LDSTR, A::Tok(0x7000_0001)),
        &[],
        1,
        &["Const o0 = String(70000001)"],
    );
    check(
        &flow(op::LDTOKEN, A::Tok(0x0200_0002)),
        &[],
        1,
        &["Const o0 = Token(02000002)"],
    );
    check(
        &flow(op::SIZEOF, A::Tok(STRUCT_TYPE)),
        &[],
        1,
        &["Const o0 = SizeOf(02000002)"],
    );
    check(&flow(op::ARGLIST, A::None), &[], 1, &["Const o0 = ArgList"]);
    check(
        &flow(op::DUP, A::None),
        &[Value],
        2,
        &["Assign o0 := [i0] Copy", "Assign o1 := [i0] Copy"],
    );
    check(&flow(op::POP, A::None), &[Discarded], 0, &[]);
}

// ---- Arithmetic, logic, conversion, comparison ------------------------------

#[test]
fn arithmetic_logic_conversion_comparison() {
    let bin = |o, name: &str| {
        check(
            &flow(o, A::None),
            &[Lhs, Rhs],
            1,
            &[&format!("Assign o0 := [i0, i1] Derived({name})")],
        )
    };
    bin(op::ADD, "add");
    bin(op::ADD_OVF_UN, "add.ovf.un");
    bin(op::SUB_OVF, "sub.ovf");
    bin(op::MUL, "mul");
    bin(op::DIV_UN, "div.un");
    bin(op::REM, "rem");
    bin(op::AND, "and");
    bin(op::OR, "or");
    bin(op::XOR, "xor");
    bin(op::SHL, "shl");
    bin(op::SHR_UN, "shr.un");
    bin(op::CEQ, "cmp.Eq");
    bin(op::CGT_UN, "cmp.Gt.un");
    bin(op::CLT, "cmp.Lt");
    check(
        &flow(op::NEG, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(neg)"],
    );
    check(
        &flow(op::NOT, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(not)"],
    );
    check(
        &flow(op::CONV_I1, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(conv.I1)"],
    );
    check(
        &flow(op::CONV_OVF_U2_UN, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(conv.ovf.un.U2)"],
    );
    check(
        &flow(op::CONV_R_UN, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(conv.un.R)"],
    );
    check(
        &flow(op::CKFINITE, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Copy"],
    );
}

// ---- Branches and control ---------------------------------------------------

#[test]
fn branches_and_control() {
    // Targets are absolute: the instruction starts at 0.
    let f = flow(op::BR_S, A::Rel(3));
    check(&f, &[], 0, &[]);
    assert_eq!(f.control, Control::Goto(5));
    let f = flow(op::BRTRUE, A::Rel(-5));
    check(&f, &[Condition], 0, &["Test [i0]"]);
    assert_eq!(f.control, Control::Cond { taken: vec![0] });
    let f = flow(op::BLE_UN_S, A::Rel(0));
    check(&f, &[Lhs, Rhs], 0, &["Test [i0, i1]"]);
    assert_eq!(f.control, Control::Cond { taken: vec![2] });
    let f = flow(op::SWITCH, A::Switch(vec![0, 4]));
    check(&f, &[Condition], 0, &["Test [i0]"]);
    assert_eq!(f.control, Control::Switch(vec![13, 17]));
    let f = flow(op::LEAVE_S, A::Rel(1));
    check(&f, &[], 0, &[]);
    assert_eq!(f.control, Control::Leave(3));
    let f = flow(op::ENDFINALLY, A::None);
    assert_eq!(f.control, Control::EndFinally);
    let f = flow(op::ENDFILTER, A::None);
    check(&f, &[Condition], 0, &["Test [i0]"]);
    assert_eq!(f.control, Control::EndFilter);

    let f = flow(op::RET, A::None);
    check(&f, &[], 0, &["Return"]);
    assert_eq!(f.control, Control::Return);
    let returns_int = sig(false, TypeSig::I4, vec![]);
    let f = flow_with(&enc(op::RET, A::None), Prefixes::default(), &returns_int);
    check(&f, &[ReturnValue], 0, &["Return i0"]);

    let f = flow(op::THROW, A::None);
    check(&f, &[Exception], 0, &["Throw i0"]);
    assert_eq!(f.control, Control::Throw);
    let f = flow(op::RETHROW, A::None);
    check(&f, &[], 0, &["Throw caught"]);
    assert_eq!(f.control, Control::Throw);
    let f = flow(op::JMP, A::Tok(STATIC_VOID_2));
    check(&f, &[], 0, &["Jump 06000001"]);
    assert_eq!(f.control, Control::Jump);
}

// ---- Fields -----------------------------------------------------------------

#[test]
fn fields() {
    check(
        &flow(op::LDFLD, A::Tok(FIELD)),
        &[Object],
        1,
        &["Load o0 := i0.<04000001>"],
    );
    check(
        &flow(op::STFLD, A::Tok(FIELD)),
        &[Object, Value],
        0,
        &["Store i0.<04000001> := i1"],
    );
    check(
        &flow(op::LDFLDA, A::Tok(FIELD)),
        &[Object],
        1,
        &["AddressOf o0 = &i0.<04000001>"],
    );
    check(
        &flow(op::LDSFLD, A::Tok(FIELD)),
        &[],
        1,
        &["Load o0 := static.<static 04000001>"],
    );
    check(
        &flow(op::STSFLD, A::Tok(FIELD)),
        &[Value],
        0,
        &["Store static.<static 04000001> := i0"],
    );
    check(
        &flow(op::LDSFLDA, A::Tok(FIELD)),
        &[],
        1,
        &["AddressOf o0 = &static.<static 04000001>"],
    );
    // The field operand is complete: is_static from the opcode, the type
    // from the signature.
    let f = flow(op::STSFLD, A::Tok(FIELD));
    let Effect::Store { dst, .. } = &f.effects[0] else {
        panic!()
    };
    assert_eq!(
        dst.field,
        Field::Named(FieldOp {
            token: MetadataToken(FIELD),
            is_static: true,
            ty: TypeSig::I4
        })
    );
}

#[test]
fn unresolved_operands_are_opaque() {
    check(
        &flow(op::LDFLD, A::Tok(BAD)),
        &[Object],
        1,
        &["Opaque [i0] -> [o0] (unresolved 0a00dead)"],
    );
    check(
        &flow(op::STSFLD, A::Tok(BAD)),
        &[Value],
        0,
        &["Opaque [i0] -> [] (unresolved 0a00dead)"],
    );
    check(
        &flow(op::LDVIRTFTN, A::Tok(BAD)),
        &[Object],
        1,
        &["Opaque [i0] -> [o0] (unresolved 0a00dead)"],
    );
    // A call whose signature does not resolve: its operands are unknown.
    check(
        &flow(op::CALLVIRT, A::Tok(BAD)),
        &[],
        0,
        &["Opaque [] -> [] (unresolved 0a00dead)"],
    );
    check(
        &flow(op::CALLI, A::Tok(BAD)),
        &[],
        0,
        &["Opaque [] -> [] (unresolved 0a00dead)"],
    );
}

// ---- Arrays -----------------------------------------------------------------

#[test]
fn arrays() {
    check(
        &flow(op::NEWARR, A::Tok(CLASS_TYPE)),
        &[Size],
        1,
        &["New o0 Array(02000003) sizes=[i0]"],
    );
    check(
        &flow(op::LDLEN, A::None),
        &[Object],
        1,
        &["Load o0 := i0.length"],
    );
    for o in [op::LDELEM_I4, op::LDELEM_REF] {
        check(
            &flow(o, A::None),
            &[Object, Index],
            1,
            &["Load o0 := i0[i1]"],
        );
    }
    check(
        &flow(op::LDELEM_ANY, A::Tok(CLASS_TYPE)),
        &[Object, Index],
        1,
        &["Load o0 := i0[i1]"],
    );
    for o in [op::STELEM_I1, op::STELEM_REF] {
        check(
            &flow(o, A::None),
            &[Object, Index, Value],
            0,
            &["Store i0[i1] := i2"],
        );
    }
    check(
        &flow(op::STELEM_ANY, A::Tok(CLASS_TYPE)),
        &[Object, Index, Value],
        0,
        &["Store i0[i1] := i2"],
    );
    check(
        &flow(op::LDELEMA, A::Tok(CLASS_TYPE)),
        &[Object, Index],
        1,
        &["AddressOf o0 = &i0[i1]"],
    );
}

#[test]
fn multi_dimensional_arrays() {
    check(
        &flow(op::CALL, A::Tok(ARRAY_GET)),
        &[Receiver, Index, Index],
        1,
        &["Load o0 := i0[i1, i2]"],
    );
    check(
        &flow(op::CALL, A::Tok(ARRAY_SET)),
        &[Receiver, Index, Index, Value],
        0,
        &["Store i0[i1, i2] := i3"],
    );
    check(
        &flow(op::CALL, A::Tok(ARRAY_ADDRESS)),
        &[Receiver, Index, Index],
        1,
        &["AddressOf o0 = &i0[i1, i2]"],
    );
    check(
        &flow(op::NEWOBJ, A::Tok(ARRAY_CTOR)),
        &[Size, Size],
        1,
        &["New o0 Array(1b000001) sizes=[i0, i1]"],
    );
    // .ctor as a plain call is not an allocation: an ordinary call.
    check(
        &flow(op::CALL, A::Tok(ARRAY_CTOR)),
        &[Receiver, Arg(0), Arg(1)],
        0,
        &["Call Instance 0a000004 recv=i0 args=[i1, i2] thrown=thrown"],
    );
}

// ---- Indirect access and value types ----------------------------------------

#[test]
fn indirect_access_and_value_types() {
    for o in [op::LDIND_I4, op::LDIND_REF] {
        check(&flow(o, A::None), &[Address], 1, &["Load o0 := i0.*"]);
    }
    check(
        &flow(op::LDOBJ, A::Tok(STRUCT_TYPE)),
        &[Address],
        1,
        &["Load o0 := i0.*"],
    );
    for o in [op::STIND_I1, op::STIND_REF] {
        check(
            &flow(o, A::None),
            &[Address, Value],
            0,
            &["Store i0.* := i1"],
        );
    }
    check(
        &flow(op::STOBJ, A::Tok(STRUCT_TYPE)),
        &[Address, Value],
        0,
        &["Store i0.* := i1"],
    );
    check(
        &flow(op::CPOBJ, A::Tok(STRUCT_TYPE)),
        &[Address, SrcAddress],
        0,
        &["Load t0 := i1.*", "Store i0.* := t0"],
    );
    check(
        &flow(op::INITOBJ, A::Tok(STRUCT_TYPE)),
        &[Address],
        0,
        &["Clear i0"],
    );
    check(
        &flow(op::CPBLK, A::None),
        &[Address, SrcAddress, Size],
        0,
        &["Load t0 := i1.*[..i2]", "Store i0.*[..i2] := t0"],
    );
    check(
        &flow(op::INITBLK, A::None),
        &[Address, Value, Size],
        0,
        &["Store i0.*[..i2] := i1"],
    );
    check(
        &flow(op::LOCALLOC, A::None),
        &[Size],
        1,
        &["New o0 StackAlloc sizes=[i0]"],
    );
}

// ---- Objects, casts, boxing, typed references -------------------------------

#[test]
fn objects_casts_boxing_typed_references() {
    check(
        &flow(op::NEWOBJ, A::Tok(CLASS_CTOR)),
        &[Arg(0), Arg(1)],
        1,
        &[
            "New o0 Object(02000003)",
            "Call Ctor 06000003 recv=o0 args=[i0, i1] thrown=thrown",
        ],
    );
    check(
        &flow(op::NEWOBJ, A::Tok(STRUCT_CTOR)),
        &[Arg(0)],
        1,
        &[
            "New o0 ValueType(02000002)",
            "Call Ctor 06000004 recv=o0 args=[i0] thrown=thrown",
        ],
    );
    check(
        &flow(op::CASTCLASS, A::Tok(CLASS_TYPE)),
        &[Object],
        1,
        &["Assign o0 := [i0] Cast(02000003)"],
    );
    check(
        &flow(op::ISINST, A::Tok(CLASS_TYPE)),
        &[Object],
        1,
        &["Assign o0 := [i0] Cast(02000003)"],
    );
    check(
        &flow(op::BOX, A::Tok(STRUCT_TYPE)),
        &[Value],
        1,
        &["Assign o0 := [i0] Box(02000002)"],
    );
    check(
        &flow(op::UNBOX, A::Tok(STRUCT_TYPE)),
        &[Object],
        1,
        &["Assign o0 := [i0] Unbox(02000002)"],
    );
    // unbox.any: Unbox for a value type, Cast for a reference type, and
    // Unbox when that is unknown (a generic parameter).
    check(
        &flow(op::UNBOX_ANY, A::Tok(STRUCT_TYPE)),
        &[Object],
        1,
        &["Assign o0 := [i0] Unbox(02000002)"],
    );
    check(
        &flow(op::UNBOX_ANY, A::Tok(CLASS_TYPE)),
        &[Object],
        1,
        &["Assign o0 := [i0] Cast(02000003)"],
    );
    check(
        &flow(op::UNBOX_ANY, A::Tok(GENERIC_PARAM_SPEC)),
        &[Object],
        1,
        &["Assign o0 := [i0] Unbox(1b000002)"],
    );
    check(
        &flow(op::MKREFANY, A::Tok(STRUCT_TYPE)),
        &[Address],
        1,
        &["Assign o0 := [i0] Derived(mkrefany 02000002)"],
    );
    check(
        &flow(op::REFANYVAL, A::Tok(STRUCT_TYPE)),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(refanyval 02000002)"],
    );
    check(
        &flow(op::REFANYTYPE, A::None),
        &[Value],
        1,
        &["Assign o0 := [i0] Derived(refanytype)"],
    );
}

// ---- Calls and function pointers --------------------------------------------

#[test]
fn calls_and_function_pointers() {
    check(
        &flow(op::CALL, A::Tok(STATIC_VOID_2)),
        &[Arg(0), Arg(1)],
        0,
        &["Call Static 06000001 args=[i0, i1] thrown=thrown byref=[1]"],
    );
    check(
        &flow(op::CALL, A::Tok(INSTANCE_INT_1)),
        &[Receiver, Arg(0)],
        1,
        &["Call Instance 06000002 recv=i0 args=[i1] result=o0 thrown=thrown"],
    );
    check(
        &flow(op::CALLVIRT, A::Tok(INSTANCE_INT_1)),
        &[Receiver, Arg(0)],
        1,
        &["Call Virtual 06000002 recv=i0 args=[i1] result=o0 thrown=thrown"],
    );
    check(
        &flow(op::CALLI, A::Tok(CALLI_SIG)),
        &[Receiver, Arg(0), Arg(1), Callee],
        1,
        &["Call Indirect{callee=i3} recv=i0 args=[i1, i2] result=o0 thrown=thrown"],
    );
    // A vararg call site: the arguments after the sentinel are included.
    check(
        &flow(op::CALL, A::Tok(VARARG)),
        &[Arg(0), Arg(1)],
        0,
        &["Call Static 0a000005 args=[i0, i1] thrown=thrown"],
    );
    check(
        &flow(op::LDFTN, A::Tok(INSTANCE_INT_1)),
        &[],
        1,
        &["FunctionPtr o0 = 06000002"],
    );
    check(
        &flow(op::LDVIRTFTN, A::Tok(INSTANCE_INT_1)),
        &[Object],
        1,
        &["FunctionPtr o0 = 06000002 receiver=i0"],
    );
}

#[test]
fn call_prefixes_reach_the_call_site() {
    let constrained = Prefixes {
        constrained: Some(TypeOp {
            token: MetadataToken(GENERIC_PARAM_SPEC),
        }),
        ..Prefixes::default()
    };
    let f = flow_with(
        &enc(op::CALLVIRT, A::Tok(INSTANCE_INT_1)),
        constrained,
        &void_method(),
    );
    let Effect::Call(site) = &f.effects[0] else {
        panic!()
    };
    assert_eq!(
        site.kind,
        CallKind::Virtual {
            constrained: Some(TypeOp {
                token: MetadataToken(GENERIC_PARAM_SPEC)
            })
        }
    );
    assert_eq!(site.prefixes, constrained);
    assert_eq!(f.prefixes, constrained);

    let tail = Prefixes {
        tail: true,
        ..Prefixes::default()
    };
    let f = flow_with(&enc(op::CALL, A::Tok(STATIC_VOID_2)), tail, &void_method());
    let Effect::Call(site) = &f.effects[0] else {
        panic!()
    };
    assert!(site.prefixes.tail);
    assert_eq!(site.kind, CallKind::Static);
}

// ---- Undefined opcodes and the whole table ----------------------------------

#[test]
fn undefined_opcodes_are_opaque_branches() {
    // 0x24 is a hole in the one-byte table.
    let f = flow_with(&[0x24, 2, 0, 0, 0], Prefixes::default(), &void_method());
    check(&f, &[], 0, &["Opaque [] -> [] (undefined opcode)"]);
    assert_eq!(f.control, Control::Goto(7));
    assert_eq!(f.size, 5);
}

/// A plausible operand for any opcode.
fn any_operand(o: OpCode) -> A {
    use OperandType as O;
    match o.operand_type {
        O::InlineNone => A::None,
        O::ShortInlineI | O::ShortInlineVar | O::ShortInlineArg | O::InlineVar | O::InlineArg => {
            A::I(1)
        }
        O::InlineI | O::InlineI8 => A::I(7),
        O::ShortInlineR | O::InlineR => A::R(0.5),
        O::ShortInlineBrTarget | O::InlineBrTarget => A::Rel(0),
        O::InlineSwitch => A::Switch(vec![0, 0]),
        O::InlineField => A::Tok(FIELD),
        O::InlineMethod => A::Tok(match o.code {
            crate::cil::Code::Newobj => CLASS_CTOR,
            _ => INSTANCE_INT_1,
        }),
        O::InlineSig => A::Tok(CALLI_SIG),
        O::InlineType | O::InlineTok => A::Tok(STRUCT_TYPE),
        O::InlineString => A::Tok(0x7000_0001),
        O::InlinePhi => A::None,
    }
}

#[test]
fn every_opcode_holds_the_invariants() {
    let mut n = 0;
    for o in opcodes::ONE_BYTE.iter().chain(&opcodes::TWO_BYTE).flatten() {
        let f = flow_with(
            &enc(*o, any_operand(*o)),
            Prefixes::default(),
            &void_method(),
        );
        assert_eq!(f.opcode, *o);
        assert_eq!(f.size as usize, enc(*o, any_operand(*o)).len());
        n += 1;
    }
    assert_eq!(n, 219);
}

#[test]
fn every_opcode_holds_the_invariants_with_unresolvable_tokens() {
    for o in opcodes::ONE_BYTE.iter().chain(&opcodes::TWO_BYTE).flatten() {
        let a = match any_operand(*o) {
            A::Tok(_) => A::Tok(BAD),
            a => a,
        };
        flow_with(&enc(*o, a), Prefixes::default(), &void_method());
    }
}

#[test]
fn the_checker_catches_each_violation() {
    let code = enc(op::LDFLD, A::Tok(FIELD));
    let ins = InstructionDecoder::new(&code).next().unwrap().unwrap();
    let method_sig = void_method();
    let cx = FlowCx::new(&Stub, &method_sig);
    let good = instruction_flow(&ins, Prefixes::default(), &cx).unwrap();
    let violations = |f: &InsFlow| layer1_violations(&ins, f, &cx);
    assert!(violations(&good).is_empty());

    let starts = |f: &InsFlow, prefix: &str| {
        let v = violations(f);
        assert!(v.iter().any(|m| m.starts_with(prefix)), "{prefix}: {v:?}");
    };
    // 7.1: a wrong pop count.
    let mut f = good.clone();
    f.inputs.push(Value);
    starts(&f, "7.1");
    // 7.2 and 7.3: no effect uses the input or defines the output.
    let mut f = good.clone();
    f.effects.clear();
    starts(&f, "7.2");
    starts(&f, "7.3");
    // 7.3: the output defined twice.
    let mut f = good.clone();
    f.effects.push(good.effects[0].clone());
    starts(&f, "7.3");
    // 7.4: a temporary read before it is written.
    let mut f = good.clone();
    f.effects.insert(
        0,
        Effect::Test {
            srcs: vec![Slot::Temp(0)],
        },
    );
    starts(&f, "7.4");
    // 7.5: an object used as an index.
    let mut f = good.clone();
    f.inputs[0] = Index;
    starts(&f, "7.5");
}
