//! The per-opcode table of Layer 1 (`docs/cil-flow-api.md` §5): what each
//! CIL instruction reads and writes, as [`Effect`]s over positional
//! [`Slot`]s.
//!
//! The `match` on [`Code`] has no `_` arm, so a new opcode is a compile
//! error here rather than a silent gap (invariant 7.6).

use super::call;
use super::operands::FlowCx;
use super::{
    CmpOp, Const, Control, ConvTarget, ConvType, Effect, Field, InsFlow, NewKind, Op, OpFlags,
    OpaqueReason, Path, Place, Prefixes, Role, Root, Slot, Transfer, TypeOp,
};
use crate::cil::{Code, Ins, RawOperand};
use crate::error::Result;
use crate::model::TypeSig;
use crate::token::MetadataToken;

/// Layer 1 for one instruction, given its signature context. `prefixes` are
/// the prefixes folded into it. Unresolvable operands give an
/// [`Effect::Opaque`] rather than an error.
pub fn instruction_flow(ins: &Ins<'_>, prefixes: Prefixes, cx: &FlowCx<'_>) -> Result<InsFlow> {
    let mut b = Builder::new(ins, prefixes);
    if ins.opcode.is_undefined() {
        // Cecil's default(OpCode): a nop whose InlineBrTarget operand it
        // reads as a branch.
        b.effects.push(Effect::Opaque {
            srcs: vec![],
            dsts: vec![],
            reason: OpaqueReason::UndefinedOpcode,
        });
        b.control = Control::Goto(target(ins));
        return Ok(b.finish());
    }
    table(&mut b, ins, prefixes, cx);
    Ok(b.finish())
}

/// An [`InsFlow`] under construction.
pub(super) struct Builder {
    pub offset: u32,
    pub size: u32,
    pub opcode: crate::cil::OpCode,
    pub prefixes: Prefixes,
    pub inputs: Vec<Role>,
    pub outputs: u8,
    pub effects: Vec<Effect<Slot>>,
    pub control: Control,
}

impl Builder {
    fn new(ins: &Ins<'_>, prefixes: Prefixes) -> Builder {
        Builder {
            offset: ins.offset,
            size: ins.size(),
            opcode: ins.opcode,
            prefixes,
            inputs: Vec::new(),
            outputs: 0,
            effects: Vec::new(),
            control: Control::Next,
        }
    }

    /// Declares the popped operands, In(0) first.
    pub fn pops(&mut self, roles: &[Role]) {
        self.inputs = roles.to_vec();
    }

    /// Declares `n` pushed values.
    pub fn pushes(&mut self, n: u8) {
        self.outputs = n;
    }

    pub fn effect(&mut self, e: Effect<Slot>) {
        self.effects.push(e);
    }

    /// The unresolvable-operand fallback: every input to every output.
    pub fn opaque(&mut self, token: MetadataToken) {
        self.effects.push(Effect::Opaque {
            srcs: (0..self.inputs.len() as u8).map(Slot::In).collect(),
            dsts: (0..self.outputs).map(Slot::Out).collect(),
            reason: OpaqueReason::UnresolvedToken(token),
        });
    }

    fn finish(self) -> InsFlow {
        InsFlow {
            offset: self.offset,
            size: self.size,
            opcode: self.opcode,
            prefixes: self.prefixes,
            inputs: self.inputs,
            outputs: self.outputs,
            effects: self.effects,
            control: self.control,
        }
    }
}

const fn i(n: u8) -> Slot {
    Slot::In(n)
}

const fn o(n: u8) -> Slot {
    Slot::Out(n)
}

fn target(ins: &Ins<'_>) -> u32 {
    match ins.operand {
        RawOperand::Target(t) => t,
        _ => ins.end_offset(),
    }
}

pub(super) fn token(ins: &Ins<'_>) -> MetadataToken {
    match ins.operand {
        RawOperand::Token(t) | RawOperand::String(t) | RawOperand::CallSite(t) => t,
        _ => MetadataToken::ZERO,
    }
}

fn type_op(ins: &Ins<'_>) -> TypeOp {
    TypeOp { token: token(ins) }
}

fn arg(ins: &Ins<'_>) -> u16 {
    match ins.operand {
        RawOperand::Arg(n) => n,
        _ => 0,
    }
}

fn var(ins: &Ins<'_>) -> u16 {
    match ins.operand {
        RawOperand::Var(n) => n,
        _ => 0,
    }
}

fn copy(dst: Slot, src: Slot) -> Effect<Slot> {
    Effect::Assign {
        dst,
        srcs: vec![src],
        transfer: Transfer::Copy,
    }
}

fn path(root: Slot, field: Field<Slot>) -> Path<Slot> {
    Path {
        root: Root::Slot(root),
        field,
    }
}

fn konst(b: &mut Builder, value: Const) {
    b.pushes(1);
    b.effect(Effect::Const { dst: o(0), value });
}

fn ldarg(b: &mut Builder, n: u16) {
    b.pushes(1);
    b.effect(copy(o(0), Slot::Arg(n)));
}

fn ldloc(b: &mut Builder, n: u16) {
    b.pushes(1);
    b.effect(copy(o(0), Slot::Local(n)));
}

fn stloc(b: &mut Builder, n: u16) {
    b.pops(&[Role::Value]);
    b.effect(copy(Slot::Local(n), i(0)));
}

fn binary(b: &mut Builder, op: Op) {
    b.pops(&[Role::Lhs, Role::Rhs]);
    b.pushes(1);
    b.effect(Effect::Assign {
        dst: o(0),
        srcs: vec![i(0), i(1)],
        transfer: Transfer::Derived(op),
    });
}

fn unary(b: &mut Builder, role: Role, transfer: Transfer) {
    b.pops(&[role]);
    b.pushes(1);
    b.effect(Effect::Assign {
        dst: o(0),
        srcs: vec![i(0)],
        transfer,
    });
}

fn conv(b: &mut Builder, ty: ConvType, ovf: bool, un: bool) {
    unary(
        b,
        Role::Value,
        Transfer::Derived(Op::Conv(ConvTarget {
            ty,
            flags: OpFlags { ovf, un },
        })),
    );
}

const PLAIN: OpFlags = OpFlags {
    ovf: false,
    un: false,
};
const OVF: OpFlags = OpFlags {
    ovf: true,
    un: false,
};
const UN: OpFlags = OpFlags {
    ovf: false,
    un: true,
};
const OVF_UN: OpFlags = OpFlags {
    ovf: true,
    un: true,
};

fn cond1(b: &mut Builder, ins: &Ins<'_>) {
    b.pops(&[Role::Condition]);
    b.effect(Effect::Test { srcs: vec![i(0)] });
    b.control = Control::Cond {
        taken: vec![target(ins)],
    };
}

fn cond2(b: &mut Builder, ins: &Ins<'_>) {
    b.pops(&[Role::Lhs, Role::Rhs]);
    b.effect(Effect::Test {
        srcs: vec![i(0), i(1)],
    });
    b.control = Control::Cond {
        taken: vec![target(ins)],
    };
}

fn ldind(b: &mut Builder) {
    b.pops(&[Role::Address]);
    b.pushes(1);
    b.effect(Effect::Load {
        dst: o(0),
        src: path(i(0), Field::Deref),
    });
}

fn stind(b: &mut Builder) {
    b.pops(&[Role::Address, Role::Value]);
    b.effect(Effect::Store {
        dst: path(i(0), Field::Deref),
        src: i(1),
    });
}

fn ldelem(b: &mut Builder) {
    b.pops(&[Role::Object, Role::Index]);
    b.pushes(1);
    b.effect(Effect::Load {
        dst: o(0),
        src: path(
            i(0),
            Field::Element {
                indices: vec![i(1)],
            },
        ),
    });
}

fn stelem(b: &mut Builder) {
    b.pops(&[Role::Object, Role::Index, Role::Value]);
    b.effect(Effect::Store {
        dst: path(
            i(0),
            Field::Element {
                indices: vec![i(1)],
            },
        ),
        src: i(2),
    });
}

/// ldfld, stfld, ldflda, ldsfld, stsfld, ldsflda.
fn field_access(b: &mut Builder, ins: &Ins<'_>, cx: &FlowCx<'_>) {
    let is_static = matches!(ins.opcode.code, Code::Ldsfld | Code::Stsfld | Code::Ldsflda);
    match ins.opcode.code {
        Code::Ldfld | Code::Ldflda => {
            b.pops(&[Role::Object]);
            b.pushes(1);
        }
        Code::Stfld => b.pops(&[Role::Object, Role::Value]),
        Code::Ldsfld | Code::Ldsflda => b.pushes(1),
        _ => b.pops(&[Role::Value]), // stsfld
    }
    let tok = token(ins);
    let f = match cx.operands.field(tok, is_static) {
        Ok(f) => f,
        Err(_) => return b.opaque(tok),
    };
    let e = match ins.opcode.code {
        Code::Ldfld => Effect::Load {
            dst: o(0),
            src: path(i(0), Field::Named(f)),
        },
        Code::Stfld => Effect::Store {
            dst: path(i(0), Field::Named(f)),
            src: i(1),
        },
        Code::Ldflda => Effect::AddressOf {
            dst: o(0),
            place: Place::Field(i(0), f),
        },
        Code::Ldsfld => Effect::Load {
            dst: o(0),
            src: Path {
                root: Root::Static,
                field: Field::Named(f),
            },
        },
        Code::Ldsflda => Effect::AddressOf {
            dst: o(0),
            place: Place::Static(f),
        },
        _ => Effect::Store {
            dst: Path {
                root: Root::Static,
                field: Field::Named(f),
            },
            src: i(0),
        },
    };
    b.effect(e);
}

fn table(b: &mut Builder, ins: &Ins<'_>, prefixes: Prefixes, cx: &FlowCx<'_>) {
    use Code::*;
    match ins.opcode.code {
        // §5.1 Locals, arguments, constants, stack
        Nop | Break => {}
        Ldarg_0 => ldarg(b, 0),
        Ldarg_1 => ldarg(b, 1),
        Ldarg_2 => ldarg(b, 2),
        Ldarg_3 => ldarg(b, 3),
        Ldarg_S | Ldarg => ldarg(b, arg(ins)),
        Ldloc_0 => ldloc(b, 0),
        Ldloc_1 => ldloc(b, 1),
        Ldloc_2 => ldloc(b, 2),
        Ldloc_3 => ldloc(b, 3),
        Ldloc_S | Ldloc => ldloc(b, var(ins)),
        Stloc_0 => stloc(b, 0),
        Stloc_1 => stloc(b, 1),
        Stloc_2 => stloc(b, 2),
        Stloc_3 => stloc(b, 3),
        Stloc_S | Stloc => stloc(b, var(ins)),
        Starg_S | Starg => {
            b.pops(&[Role::Value]);
            b.effect(copy(Slot::Arg(arg(ins)), i(0)));
        }
        Ldarga_S | Ldarga => {
            b.pushes(1);
            b.effect(Effect::AddressOf {
                dst: o(0),
                place: Place::Arg(arg(ins)),
            });
        }
        Ldloca_S | Ldloca => {
            b.pushes(1);
            b.effect(Effect::AddressOf {
                dst: o(0),
                place: Place::Local(var(ins)),
            });
        }
        Ldnull => konst(b, Const::Null),
        Ldc_I4_M1 => konst(b, Const::I4(-1)),
        Ldc_I4_0 => konst(b, Const::I4(0)),
        Ldc_I4_1 => konst(b, Const::I4(1)),
        Ldc_I4_2 => konst(b, Const::I4(2)),
        Ldc_I4_3 => konst(b, Const::I4(3)),
        Ldc_I4_4 => konst(b, Const::I4(4)),
        Ldc_I4_5 => konst(b, Const::I4(5)),
        Ldc_I4_6 => konst(b, Const::I4(6)),
        Ldc_I4_7 => konst(b, Const::I4(7)),
        Ldc_I4_8 => konst(b, Const::I4(8)),
        Ldc_I4_S | Ldc_I4 => {
            let v = match ins.operand {
                RawOperand::I8(v) => v as i32,
                RawOperand::I32(v) => v,
                _ => 0,
            };
            konst(b, Const::I4(v))
        }
        Ldc_I8 => konst(
            b,
            Const::I8(match ins.operand {
                RawOperand::I64(v) => v,
                _ => 0,
            }),
        ),
        Ldc_R4 => konst(
            b,
            Const::R4(match ins.operand {
                RawOperand::F32(v) => v,
                _ => 0.0,
            }),
        ),
        Ldc_R8 => konst(
            b,
            Const::R8(match ins.operand {
                RawOperand::F64(v) => v,
                _ => 0.0,
            }),
        ),
        Ldstr => konst(b, Const::String(token(ins))),
        Ldtoken => konst(b, Const::Token(token(ins))),
        Sizeof => konst(b, Const::SizeOf(type_op(ins))),
        Arglist => konst(b, Const::ArgList),
        Dup => {
            b.pops(&[Role::Value]);
            b.pushes(2);
            b.effect(copy(o(0), i(0)));
            b.effect(copy(o(1), i(0)));
        }
        Pop => b.pops(&[Role::Discarded]),

        // §5.2 Arithmetic, logic, conversion, comparison
        Add => binary(b, Op::Add(PLAIN)),
        Add_Ovf => binary(b, Op::Add(OVF)),
        Add_Ovf_Un => binary(b, Op::Add(OVF_UN)),
        Sub => binary(b, Op::Sub(PLAIN)),
        Sub_Ovf => binary(b, Op::Sub(OVF)),
        Sub_Ovf_Un => binary(b, Op::Sub(OVF_UN)),
        Mul => binary(b, Op::Mul(PLAIN)),
        Mul_Ovf => binary(b, Op::Mul(OVF)),
        Mul_Ovf_Un => binary(b, Op::Mul(OVF_UN)),
        Div => binary(b, Op::Div(PLAIN)),
        Div_Un => binary(b, Op::Div(UN)),
        Rem => binary(b, Op::Rem(PLAIN)),
        Rem_Un => binary(b, Op::Rem(UN)),
        And => binary(b, Op::And),
        Or => binary(b, Op::Or),
        Xor => binary(b, Op::Xor),
        Shl => binary(b, Op::Shl),
        Shr => binary(b, Op::Shr(PLAIN)),
        Shr_Un => binary(b, Op::Shr(UN)),
        Neg => unary(b, Role::Value, Transfer::Derived(Op::Neg)),
        Not => unary(b, Role::Value, Transfer::Derived(Op::Not)),
        Conv_I1 => conv(b, ConvType::I1, false, false),
        Conv_I2 => conv(b, ConvType::I2, false, false),
        Conv_I4 => conv(b, ConvType::I4, false, false),
        Conv_I8 => conv(b, ConvType::I8, false, false),
        Conv_U1 => conv(b, ConvType::U1, false, false),
        Conv_U2 => conv(b, ConvType::U2, false, false),
        Conv_U4 => conv(b, ConvType::U4, false, false),
        Conv_U8 => conv(b, ConvType::U8, false, false),
        Conv_I => conv(b, ConvType::I, false, false),
        Conv_U => conv(b, ConvType::U, false, false),
        Conv_R4 => conv(b, ConvType::R4, false, false),
        Conv_R8 => conv(b, ConvType::R8, false, false),
        Conv_R_Un => conv(b, ConvType::R, false, true),
        Conv_Ovf_I1 => conv(b, ConvType::I1, true, false),
        Conv_Ovf_I2 => conv(b, ConvType::I2, true, false),
        Conv_Ovf_I4 => conv(b, ConvType::I4, true, false),
        Conv_Ovf_I8 => conv(b, ConvType::I8, true, false),
        Conv_Ovf_U1 => conv(b, ConvType::U1, true, false),
        Conv_Ovf_U2 => conv(b, ConvType::U2, true, false),
        Conv_Ovf_U4 => conv(b, ConvType::U4, true, false),
        Conv_Ovf_U8 => conv(b, ConvType::U8, true, false),
        Conv_Ovf_I => conv(b, ConvType::I, true, false),
        Conv_Ovf_U => conv(b, ConvType::U, true, false),
        Conv_Ovf_I1_Un => conv(b, ConvType::I1, true, true),
        Conv_Ovf_I2_Un => conv(b, ConvType::I2, true, true),
        Conv_Ovf_I4_Un => conv(b, ConvType::I4, true, true),
        Conv_Ovf_I8_Un => conv(b, ConvType::I8, true, true),
        Conv_Ovf_U1_Un => conv(b, ConvType::U1, true, true),
        Conv_Ovf_U2_Un => conv(b, ConvType::U2, true, true),
        Conv_Ovf_U4_Un => conv(b, ConvType::U4, true, true),
        Conv_Ovf_U8_Un => conv(b, ConvType::U8, true, true),
        Conv_Ovf_I_Un => conv(b, ConvType::I, true, true),
        Conv_Ovf_U_Un => conv(b, ConvType::U, true, true),
        // The value is unchanged or an exception is thrown.
        Ckfinite => unary(b, Role::Value, Transfer::Copy),
        Ceq => binary(b, Op::Cmp(CmpOp::Eq, PLAIN)),
        Cgt => binary(b, Op::Cmp(CmpOp::Gt, PLAIN)),
        Cgt_Un => binary(b, Op::Cmp(CmpOp::Gt, UN)),
        Clt => binary(b, Op::Cmp(CmpOp::Lt, PLAIN)),
        Clt_Un => binary(b, Op::Cmp(CmpOp::Lt, UN)),

        // §5.3 Branches and control
        Br_S | Br => b.control = Control::Goto(target(ins)),
        Brfalse_S | Brtrue_S | Brfalse | Brtrue => cond1(b, ins),
        Beq_S | Bge_S | Bgt_S | Ble_S | Blt_S | Bne_Un_S | Bge_Un_S | Bgt_Un_S | Ble_Un_S
        | Blt_Un_S | Beq | Bge | Bgt | Ble | Blt | Bne_Un | Bge_Un | Bgt_Un | Ble_Un | Blt_Un => {
            cond2(b, ins)
        }
        Switch => {
            b.pops(&[Role::Condition]);
            b.effect(Effect::Test { srcs: vec![i(0)] });
            b.control = Control::Switch(match &ins.operand {
                RawOperand::Switch(ts) => ts.iter().collect(),
                _ => vec![],
            });
        }
        Leave | Leave_S => b.control = Control::Leave(target(ins)),
        Endfinally => b.control = Control::EndFinally,
        Endfilter => {
            b.pops(&[Role::Condition]);
            b.effect(Effect::Test { srcs: vec![i(0)] });
            b.control = Control::EndFilter;
        }
        Ret => {
            if cx.method_sig.return_type.unwrap_modifiers() == &TypeSig::Void {
                b.effect(Effect::Return { src: None });
            } else {
                b.pops(&[Role::ReturnValue]);
                b.effect(Effect::Return { src: Some(i(0)) });
            }
            b.control = Control::Return;
        }
        Throw => {
            b.pops(&[Role::Exception]);
            b.effect(Effect::Throw { src: i(0) });
            b.control = Control::Throw;
        }
        Rethrow => {
            b.effect(Effect::Throw { src: Slot::Caught });
            b.control = Control::Throw;
        }
        Jmp => {
            let tok = token(ins);
            match cx.operands.method(tok) {
                Ok(m) => b.effect(Effect::Jump { method: m.op }),
                Err(_) => b.opaque(tok),
            }
            b.control = Control::Jump;
        }

        // §5.4 Fields
        Ldfld | Stfld | Ldflda | Ldsfld | Stsfld | Ldsflda => field_access(b, ins, cx),

        // §5.5 Arrays (multi-dimensional ones are calls, in call.rs)
        Newarr => {
            b.pops(&[Role::Size]);
            b.pushes(1);
            b.effect(Effect::New {
                dst: o(0),
                kind: NewKind::Array,
                ty: Some(type_op(ins)),
                sizes: vec![i(0)],
            });
        }
        Ldlen => {
            b.pops(&[Role::Object]);
            b.pushes(1);
            b.effect(Effect::Load {
                dst: o(0),
                src: path(i(0), Field::Length),
            });
        }
        Ldelem_I1 | Ldelem_U1 | Ldelem_I2 | Ldelem_U2 | Ldelem_I4 | Ldelem_U4 | Ldelem_I8
        | Ldelem_I | Ldelem_R4 | Ldelem_R8 | Ldelem_Ref | Ldelem_Any => ldelem(b),
        Stelem_I | Stelem_I1 | Stelem_I2 | Stelem_I4 | Stelem_I8 | Stelem_R4 | Stelem_R8
        | Stelem_Ref | Stelem_Any => stelem(b),
        Ldelema => {
            b.pops(&[Role::Object, Role::Index]);
            b.pushes(1);
            b.effect(Effect::AddressOf {
                dst: o(0),
                place: Place::Element(i(0), vec![i(1)]),
            });
        }

        // §5.6 Indirect access and value types
        Ldind_I1 | Ldind_U1 | Ldind_I2 | Ldind_U2 | Ldind_I4 | Ldind_U4 | Ldind_I8 | Ldind_I
        | Ldind_R4 | Ldind_R8 | Ldind_Ref | Ldobj => ldind(b),
        Stind_Ref | Stind_I1 | Stind_I2 | Stind_I4 | Stind_I8 | Stind_R4 | Stind_R8 | Stind_I
        | Stobj => stind(b),
        Cpobj => {
            b.pops(&[Role::Address, Role::SrcAddress]);
            b.effect(Effect::Load {
                dst: Slot::Temp(0),
                src: path(i(1), Field::Deref),
            });
            b.effect(Effect::Store {
                dst: path(i(0), Field::Deref),
                src: Slot::Temp(0),
            });
        }
        Initobj => {
            b.pops(&[Role::Address]);
            b.effect(Effect::Clear { addr: i(0) });
        }
        Cpblk => {
            b.pops(&[Role::Address, Role::SrcAddress, Role::Size]);
            b.effect(Effect::Load {
                dst: Slot::Temp(0),
                src: path(i(1), Field::Block { size: i(2) }),
            });
            b.effect(Effect::Store {
                dst: path(i(0), Field::Block { size: i(2) }),
                src: Slot::Temp(0),
            });
        }
        Initblk => {
            b.pops(&[Role::Address, Role::Value, Role::Size]);
            b.effect(Effect::Store {
                dst: path(i(0), Field::Block { size: i(2) }),
                src: i(1),
            });
        }
        Localloc => {
            b.pops(&[Role::Size]);
            b.pushes(1);
            b.effect(Effect::New {
                dst: o(0),
                kind: NewKind::StackAlloc,
                ty: None,
                sizes: vec![i(0)],
            });
        }

        // §5.7 Objects, casts, boxing, typed references
        Castclass | Isinst => unary(b, Role::Object, Transfer::Cast(type_op(ins))),
        Box => unary(b, Role::Value, Transfer::Box(type_op(ins))),
        // A pointer into the box, which aliases it.
        Unbox => unary(b, Role::Object, Transfer::Unbox(type_op(ins))),
        Unbox_Any => {
            let t = type_op(ins);
            let transfer = match cx.operands.is_value_type(&t) {
                Some(false) => Transfer::Cast(t),
                Some(true) | None => Transfer::Unbox(t),
            };
            unary(b, Role::Object, transfer)
        }
        Mkrefany => unary(
            b,
            Role::Address,
            Transfer::Derived(Op::MkRefAny(type_op(ins))),
        ),
        Refanyval => unary(
            b,
            Role::Value,
            Transfer::Derived(Op::RefAnyVal(type_op(ins))),
        ),
        Refanytype => unary(b, Role::Value, Transfer::Derived(Op::RefAnyType)),

        // §5.8 Calls and function pointers
        Call | Callvirt | Newobj | Calli => call::call(b, ins, prefixes, cx),
        Ldftn | Ldvirtftn => {
            if ins.opcode.code == Ldvirtftn {
                b.pops(&[Role::Object]);
            }
            b.pushes(1);
            let tok = token(ins);
            match cx.operands.method(tok) {
                Ok(m) => b.effect(Effect::FunctionPtr {
                    dst: o(0),
                    method: m.op,
                    receiver: (ins.opcode.code == Ldvirtftn).then_some(i(0)),
                }),
                Err(_) => b.opaque(tok),
            }
        }

        // §5.9 Prefixes: folded into the next instruction by
        // MethodBody::flows. Met alone (at the end of a body, or through
        // instruction_flow), they do nothing.
        Unaligned | Volatile | Tail | Constrained | No | Readonly => {}
    }
}
