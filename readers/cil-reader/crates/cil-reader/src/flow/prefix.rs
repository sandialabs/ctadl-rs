//! Prefix folding (`docs/cil-flow-api.md` §5.9) and [`MethodBody::flows`].
//!
//! `volatile.` `unaligned.` `tail.` `readonly.` `no.` `constrained.` produce
//! no [`InsFlow`] of their own: they are folded into the next instruction's
//! [`Prefixes`], and its `size` covers them. That is the only state Layer 1
//! carries between instructions.

use super::operands::FlowCx;
use super::{Control, InsFlow, Prefixes, TypeOp, instruction_flow};
use crate::cil::{Code, Ins, RawOperand};
use crate::error::{Error, Result};
use crate::model::{MethodBody, MethodSig};

/// Whether `code` is a prefix opcode.
pub fn is_prefix(code: Code) -> bool {
    matches!(
        code,
        Code::Unaligned
            | Code::Volatile
            | Code::Tail
            | Code::Constrained
            | Code::No
            | Code::Readonly
    )
}

impl Prefixes {
    /// Adds the prefix instruction `ins` to these.
    pub fn add(&mut self, ins: &Ins<'_>) {
        let byte = match ins.operand {
            RawOperand::U8(b) => b,
            RawOperand::I8(b) => b as u8,
            _ => 0,
        };
        match ins.opcode.code {
            Code::Volatile => self.volatile = true,
            Code::Unaligned => self.unaligned = Some(byte),
            Code::Tail => self.tail = true,
            Code::Readonly => self.readonly = true,
            Code::No => self.no = Some(byte),
            Code::Constrained => {
                if let RawOperand::Token(token) = ins.operand {
                    self.constrained = Some(TypeOp { token });
                }
            }
            _ => {}
        }
    }

    /// Whether every prefix here may precede `code` (ECMA-335 §III.2). A
    /// prefix that does not is still recorded; Layer 2 warns about it.
    pub fn apply_to(&self, code: Code) -> bool {
        use Code::*;
        let memory = matches!(
            code,
            Ldind_I1
                | Ldind_U1
                | Ldind_I2
                | Ldind_U2
                | Ldind_I4
                | Ldind_U4
                | Ldind_I8
                | Ldind_I
                | Ldind_R4
                | Ldind_R8
                | Ldind_Ref
                | Stind_Ref
                | Stind_I1
                | Stind_I2
                | Stind_I4
                | Stind_I8
                | Stind_R4
                | Stind_R8
                | Stind_I
                | Ldfld
                | Stfld
                | Ldobj
                | Stobj
                | Initblk
                | Cpblk
        );
        let call = matches!(code, Call | Callvirt | Calli);
        let checked = matches!(
            code,
            Castclass
                | Isinst
                | Unbox
                | Unbox_Any
                | Ldelema
                | Ldelem_I1
                | Ldelem_U1
                | Ldelem_I2
                | Ldelem_U2
                | Ldelem_I4
                | Ldelem_U4
                | Ldelem_I8
                | Ldelem_I
                | Ldelem_R4
                | Ldelem_R8
                | Ldelem_Ref
                | Ldelem_Any
                | Stelem_I
                | Stelem_I1
                | Stelem_I2
                | Stelem_I4
                | Stelem_I8
                | Stelem_R4
                | Stelem_R8
                | Stelem_Ref
                | Stelem_Any
                | Ldfld
                | Stfld
                | Callvirt
                | Ldvirtftn
                | Ldlen
        );
        (!self.volatile || memory || matches!(code, Ldsfld | Stsfld))
            && (self.unaligned.is_none() || memory)
            && (!self.tail || call)
            && (!self.readonly || code == Ldelema)
            && (self.no.is_none() || checked)
            && (self.constrained.is_none() || code == Callvirt)
    }
}

/// The next record of Layer 1 over `instructions` from `*pos`, prefixes
/// folded into the instruction after them. Prefixes at the very end, with no
/// instruction to fold into, give a record of their own: the last prefix,
/// with no effects.
fn fold_one(instructions: &[Ins<'_>], pos: &mut usize, cx: &FlowCx<'_>) -> Option<Result<InsFlow>> {
    let first = instructions.get(*pos)?;
    let mut prefixes = Prefixes::default();
    while let Some(ins) = instructions.get(*pos) {
        *pos += 1;
        if is_prefix(ins.opcode.code) {
            prefixes.add(ins);
            continue;
        }
        return Some(instruction_flow(ins, prefixes, cx).map(|mut flow| {
            flow.size = ins.end_offset() - first.offset;
            flow.offset = first.offset;
            flow
        }));
    }
    let last = &instructions[*pos - 1];
    Some(Ok(InsFlow {
        offset: first.offset,
        size: last.end_offset() - first.offset,
        opcode: last.opcode,
        prefixes,
        inputs: vec![],
        outputs: 0,
        effects: vec![],
        control: Control::Next,
    }))
}

/// The iterator of [`MethodBody::flows`].
struct Flows<'b, 'a> {
    body: &'b MethodBody<'a>,
    sig: MethodSig,
    pos: usize,
    /// An error still to report: the signature's first, the code's last.
    pending: Option<Error>,
    done: bool,
}

impl Iterator for Flows<'_, '_> {
    type Item = Result<InsFlow>;

    fn next(&mut self) -> Option<Result<InsFlow>> {
        if self.done {
            return None;
        }
        if let Some(e) = self.pending.take() {
            self.done = true;
            return Some(Err(e));
        }
        let cx = FlowCx::new(self.body, &self.sig);
        match fold_one(&self.body.instructions, &mut self.pos, &cx) {
            Some(flow) => Some(flow),
            None => {
                self.done = true;
                self.body.decode_error.as_ref().map(|(_, e)| Err(e.clone()))
            }
        }
    }
}

impl<'a> MethodBody<'a> {
    /// Layer 1 over the whole body, with prefixes folded into the next
    /// instruction. Cheap: no CFG, no stack simulation. Fails (once, as the
    /// only item) when the method's signature does not decode, and ends with
    /// the body's `decode_error` when its code did not all decode.
    pub fn flows(&self) -> impl Iterator<Item = Result<InsFlow>> + '_ {
        let (sig, pending) = match self.method().signature() {
            Ok(sig) => (sig, None),
            Err(e) => (placeholder_sig(), Some(e)),
        };
        Flows {
            body: self,
            sig,
            pos: 0,
            pending,
            done: false,
        }
    }
}

fn placeholder_sig() -> MethodSig {
    MethodSig {
        has_this: false,
        explicit_this: false,
        calling_convention: 0,
        generic_arity: 0,
        return_type: crate::model::TypeSig::Void,
        params: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cil::InstructionDecoder;
    use crate::error::Error;
    use crate::flow::operands::{Operands, ResolvedMethod};
    use crate::flow::{Effect, FieldOp, Slot};
    use crate::model::TypeSig;
    use crate::token::MetadataToken;

    struct Fields;

    impl Operands for Fields {
        fn field(&self, token: MetadataToken, is_static: bool) -> Result<FieldOp> {
            Ok(FieldOp {
                token,
                is_static,
                ty: TypeSig::I4,
            })
        }
        fn method(&self, _: MetadataToken) -> Result<ResolvedMethod> {
            Err(Error::Unsupported("stub"))
        }
        fn call_site(&self, _: MetadataToken) -> Result<MethodSig> {
            Err(Error::Unsupported("stub"))
        }
        fn is_value_type(&self, _: &TypeOp) -> Option<bool> {
            None
        }
    }

    fn fold_all(code: &[u8]) -> Vec<InsFlow> {
        let instructions: Vec<_> = InstructionDecoder::new(code).map(|i| i.unwrap()).collect();
        let sig = placeholder_sig();
        let cx = FlowCx::new(&Fields, &sig);
        let mut pos = 0;
        std::iter::from_fn(|| fold_one(&instructions, &mut pos, &cx))
            .map(|f| f.unwrap())
            .collect()
    }

    #[test]
    fn prefixes_fold_into_the_next_instruction() {
        // volatile. unaligned. 1 ldfld <04000001>; nop
        let code = [
            0xfe, 0x13, 0xfe, 0x12, 0x01, 0x7b, 0x01, 0x00, 0x00, 0x04, 0x00,
        ];
        let flows = fold_all(&code);
        assert_eq!(flows.len(), 2);
        let f = &flows[0];
        assert_eq!((f.offset, f.size), (0, 10));
        assert_eq!(f.opcode.code, Code::Ldfld);
        assert_eq!(
            f.prefixes,
            Prefixes {
                volatile: true,
                unaligned: Some(1),
                ..Prefixes::default()
            }
        );
        assert!(f.prefixes.apply_to(Code::Ldfld));
        assert!(matches!(
            f.effects[0],
            Effect::Load {
                dst: Slot::Out(0),
                ..
            }
        ));
        assert_eq!((flows[1].offset, flows[1].size), (10, 1));
        assert!(flows[1].prefixes.is_empty());
    }

    #[test]
    fn constrained_and_no_record_their_operands() {
        // constrained. <1b000002> no. 3 readonly. tail. nop
        let code = [
            0xfe, 0x16, 0x02, 0x00, 0x00, 0x1b, 0xfe, 0x19, 0x03, 0xfe, 0x1e, 0xfe, 0x14, 0x00,
        ];
        let flows = fold_all(&code);
        assert_eq!(flows.len(), 1);
        let p = flows[0].prefixes;
        assert_eq!(
            p.constrained,
            Some(TypeOp {
                token: MetadataToken(0x1b00_0002)
            })
        );
        assert_eq!(p.no, Some(3));
        assert!(p.readonly && p.tail);
        // Stray: none of them applies to nop.
        assert!(!p.apply_to(Code::Nop));
        assert_eq!(flows[0].opcode.code, Code::Nop);
        assert_eq!(flows[0].size, 14);
    }

    #[test]
    fn trailing_prefixes_get_a_record() {
        // nop; tail.
        let flows = fold_all(&[0x00, 0xfe, 0x14]);
        assert_eq!(flows.len(), 2);
        let f = &flows[1];
        assert_eq!((f.offset, f.size), (1, 2));
        assert_eq!(f.opcode.code, Code::Tail);
        assert!(f.prefixes.tail);
        assert!(f.effects.is_empty() && f.inputs.is_empty());
        assert_eq!(f.control, Control::Next);
    }
}
