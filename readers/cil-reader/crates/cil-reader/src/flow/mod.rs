//! Per-instruction dataflow for CIL: what each instruction reads, what it
//! writes, and how values move between them.
//!
//! Two layers share one effect vocabulary, [`Effect`], generic over the slot
//! type:
//!
//! - Layer 1, [`InsFlow`]: one instruction at a time, with operands named by
//!   position ([`Slot::In`], [`Slot::Out`]). A pure function of the
//!   instruction and its resolved operands; no stack simulation. See
//!   [`MethodBody::flows`](crate::model::MethodBody::flows) and
//!   [`instruction_flow`].
//! - Layer 2, [`MethodFlow`]: the CFG and stack heights of a whole method,
//!   with stack positions bound to depth-named variables ([`Var`]).
//!
//! The layer reports facts and makes no policy choices: every operand an
//! instruction reads appears in some effect, with its [`Role`].

mod call;
pub mod check;
pub mod delegate;
mod edges;
mod method;
mod opcode;
mod operands;
mod prefix;

pub use edges::{Edge, Loc};
pub use method::{
    BitSet, Block, BlockId, BoundIns, FlowOptions, FlowWarning, Handler, HandlerKind, InsId,
    MethodFlow, StackKind, Var,
};
pub use opcode::instruction_flow;
pub use operands::{ArrayMethod, FlowCx, Operands, ResolvedMethod};
pub use prefix::is_prefix;

use crate::cil::OpCode;
use crate::model::{MethodSig, TypeSig};
use crate::token::MetadataToken;
use std::fmt;

/// A position an instruction reads or writes, before stack simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// The i-th operand this instruction pops, in ECMA operand order: In(0) is
    /// the *deepest* (first pushed). For `stfld`, In(0) = object, In(1) = value.
    In(u8),
    /// The i-th value this instruction pushes (only `dup` pushes two).
    Out(u8),
    /// Argument slot n (0 is `this` on instance methods), as ldarg/starg number it.
    Arg(u16),
    /// Local variable slot n.
    Local(u16),
    /// A temporary that lives only inside this instruction (cpobj, cpblk).
    Temp(u8),
    /// The exception this instruction raises if it completes abruptly: the
    /// second result of a call, or the operand of throw. In Layer 2 it binds to
    /// [`Var::Thrown`].
    Thrown,
    /// The exception the enclosing catch/filter handler caught (`rethrow`).
    Caught,
}

/// One step of a memory access.
#[derive(Debug, Clone, PartialEq)]
pub enum Field<S> {
    /// An instance or static field (`ldfld`, `stsfld`, ...).
    Named(FieldOp),
    /// An array element: `ldelem*`, `stelem*`, `ldelema`, and the runtime
    /// `Get`/`Set`/`Address` methods of multi-dimensional arrays. `indices`
    /// holds one slot per dimension, in order.
    Element { indices: Vec<S> },
    /// Array length (`ldlen`).
    Length,
    /// Through a managed or unmanaged pointer (`ldind*`, `stind*`, `ldobj`,
    /// `stobj`, `cpobj`).
    Deref,
    /// Through a pointer, a block of `size` bytes (`cpblk`, `initblk`). A
    /// client that does not care about the extent reads it as [`Field::Deref`].
    Block { size: S },
}

/// The root of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Root<S> {
    Slot(S),
    /// The static-field base: one root shared by every static field.
    Static,
}

/// A memory access: a root and one step from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Path<S> {
    pub root: Root<S>,
    pub field: Field<S>,
}

/// What an address-producing instruction points at (ldloca, ldarga, ldflda,
/// ldsflda, ldelema, Address on a multi-dimensional array).
#[derive(Debug, Clone, PartialEq)]
pub enum Place<S> {
    Local(u16),
    Arg(u16),
    Field(S, FieldOp),
    Static(FieldOp),
    /// Array and indices, as in [`Field::Element`].
    Element(S, Vec<S>),
}

/// How a value reaches its destination. In every kind the destination depends
/// on the sources. The kinds differ in what else carries over: object
/// *identity* (and therefore aliasing and field structure) or only contents.
#[derive(Debug, Clone, PartialEq)]
pub enum Transfer {
    /// Same value: ldloc, stloc, dup, starg, ckfinite. Identity is preserved.
    Copy,
    /// Same object, viewed as another type: castclass, isinst, unbox.any of a
    /// reference type. Identity is preserved.
    Cast(TypeOp),
    /// A value type boxed into a fresh heap object. Contents are preserved. The
    /// client may also treat it as an allocation of the type.
    Box(TypeOp),
    /// A boxed object unwrapped (unbox.any to a value type, unbox). Contents
    /// are preserved.
    Unbox(TypeOp),
    /// A new value computed from the inputs: arithmetic, conversions,
    /// comparisons, mkrefany, refanytype. The result depends on the inputs but
    /// is not the same object and has none of their fields.
    Derived(Op),
}

/// One thing an instruction does. An instruction's effects run in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect<S> {
    /// dst := srcs   (several srcs only for Derived binary operations)
    Assign {
        dst: S,
        srcs: Vec<S>,
        transfer: Transfer,
    },
    /// dst := a constant. No inputs, but it is a definition.
    Const {
        dst: S,
        value: Const,
    },
    /// dst := root.field
    Load {
        dst: S,
        src: Path<S>,
    },
    /// root.field := src
    Store {
        dst: Path<S>,
        src: S,
    },
    /// dst := &place
    AddressOf {
        dst: S,
        place: Place<S>,
    },
    /// dst := a fresh object, array, or stack block. `sizes` holds the
    /// operands that size it: one for newarr and localloc, one per `.ctor`
    /// argument for a multi-dimensional array, none otherwise.
    New {
        dst: S,
        kind: NewKind,
        ty: Option<TypeOp>,
        sizes: Vec<S>,
    },
    /// dst := a function pointer to `method`. `receiver` is set for ldvirtftn,
    /// whose target depends on the receiver's runtime type.
    FunctionPtr {
        dst: S,
        method: MethodOp,
        receiver: Option<S>,
    },
    /// The place at `addr` is reset to its default value (initobj). Whether
    /// that kills anything (a strong update) is the client's choice.
    Clear {
        addr: S,
    },
    Call(CallSite<S>),
    /// Normal method exit with an optional value (ret).
    Return {
        src: Option<S>,
    },
    /// Abrupt exit: Thrown := src (throw, rethrow with src = Caught).
    Throw {
        src: S,
    },
    /// `jmp`: exit by transferring every argument slot to `method`.
    Jump {
        method: MethodOp,
    },
    /// The inputs decide control flow: brtrue, beq, switch, endfilter.
    /// Reported so a client can model implicit flow. Ignoring it is also a
    /// valid choice.
    Test {
        srcs: Vec<S>,
    },
    /// Fallback when an operand does not resolve or an opcode is undefined.
    /// The reader does not know the semantics, so it lists every input and
    /// every output. Relating all of them (`Transfer::Derived(Op::Opaque)`) is
    /// the conservative reading.
    Opaque {
        srcs: Vec<S>,
        dsts: Vec<S>,
        reason: OpaqueReason,
    },
}

/// What a [`Effect::New`] allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NewKind {
    Object,
    ValueType,
    Array,
    /// `localloc`.
    StackAlloc,
}

/// The value of a [`Effect::Const`].
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    I4(i32),
    I8(i64),
    R4(f32),
    R8(f64),
    Null,
    /// ldstr; text via [`MethodBody::user_string`](crate::model::MethodBody::user_string).
    String(MetadataToken),
    /// ldtoken.
    Token(MetadataToken),
    /// sizeof.
    SizeOf(TypeOp),
    /// arglist: the handle to a varargs method's extra arguments.
    ArgList,
}

/// `.ovf` (overflow-checked) and `.un` (unsigned) variants of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OpFlags {
    pub ovf: bool,
    pub un: bool,
}

/// The operation behind a [`Transfer::Derived`].
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Add(OpFlags),
    Sub(OpFlags),
    Mul(OpFlags),
    Div(OpFlags),
    Rem(OpFlags),
    And,
    Or,
    Xor,
    Shl,
    Shr(OpFlags),
    Neg,
    Not,
    Conv(ConvTarget),
    /// ceq/cgt/clt and their `.un` forms.
    Cmp(CmpOp, OpFlags),
    MkRefAny(TypeOp),
    RefAnyType,
    RefAnyVal(TypeOp),
    /// The unknown operation of an [`Effect::Opaque`], in the edge view.
    Opaque,
}

/// The comparison of a `ceq`/`cgt`/`clt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Eq,
    Gt,
    Lt,
}

/// The target of a `conv.*` instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConvTarget {
    pub ty: ConvType,
    pub flags: OpFlags,
}

/// The type a `conv.*` converts to. `R` is the native float type `F` of
/// `conv.r.un`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConvType {
    I1,
    I2,
    I4,
    I8,
    U1,
    U2,
    U4,
    U8,
    I,
    U,
    R4,
    R8,
    R,
}

/// Why an instruction's effects are [`Effect::Opaque`].
#[derive(Debug, Clone, PartialEq)]
pub enum OpaqueReason {
    /// A field, method or call-site token that does not resolve in this module.
    UnresolvedToken(MetadataToken),
    /// A byte that names no opcode ([`crate::cil::UNDEFINED`]).
    UndefinedOpcode,
}

/// A field operand. `is_static` comes from the opcode (`ldsfld` vs `ldfld`),
/// not from resolving the field, so a `MemberRef` into an assembly that is not
/// loaded still produces a complete effect. `ty` is the field's declared type.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldOp {
    pub token: MetadataToken,
    pub is_static: bool,
    pub ty: TypeSig,
}

/// A type operand: a TypeDef, TypeRef or TypeSpec token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeOp {
    pub token: MetadataToken,
}

/// A method operand.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodOp {
    /// MethodDef, MemberRef or MethodSpec, as the instruction names it.
    pub token: MetadataToken,
    /// The signature: of the MethodDef, of the MemberRef (for a vararg call
    /// site, with the extra arguments), or of the method a MethodSpec
    /// instantiates.
    pub sig: MethodSig,
    /// The MethodSpec instantiation, if any.
    pub generic_args: Vec<TypeSig>,
}

/// A call: `call`, `callvirt`, `newobj` or `calli`.
#[derive(Debug, Clone, PartialEq)]
pub struct CallSite<S> {
    pub kind: CallKind<S>,
    /// Callee. `None` only for calli, which has a call-site signature instead.
    pub method: Option<MethodOp>,
    pub sig: MethodSig,
    /// Instance calls: In(0). newobj: Out(0), the freshly allocated object.
    pub receiver: Option<S>,
    /// Declared parameters in order, receiver excluded (varargs extras included).
    pub args: Vec<S>,
    /// Non-void result. Never set for newobj, whose "result" is the receiver.
    pub result: Option<S>,
    /// Exceptional result. Always Slot::Thrown in Layer 1.
    pub thrown: S,
    /// Indices into `args` of `ByRef` (`ref`/`out`/`in`) parameters. These are
    /// *outputs* of the call too, which matters for TryParse and TryGetValue.
    pub byref_args: Vec<u16>,
    pub prefixes: Prefixes,
}

/// How a [`CallSite`] dispatches.
#[derive(Debug, Clone, PartialEq)]
pub enum CallKind<S> {
    /// `call` of a static method.
    Static,
    /// `call` of an instance method: non-virtual, exact target (base.M(), struct methods).
    Instance,
    /// `callvirt`. `constrained` holds the `constrained.` prefix's type.
    Virtual { constrained: Option<TypeOp> },
    /// `newobj`. It comes right after the `New` effect for Out(0).
    Ctor,
    /// `calli`. The function pointer is the last popped operand.
    Indirect { callee: S },
}

/// The prefixes folded into an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Prefixes {
    pub volatile: bool,
    /// `unaligned. n`.
    pub unaligned: Option<u8>,
    pub tail: bool,
    pub readonly: bool,
    /// `no. n`: the checks (type, range, null) the instruction may skip.
    pub no: Option<u8>,
    pub constrained: Option<TypeOp>,
}

impl Prefixes {
    pub fn is_empty(&self) -> bool {
        *self == Prefixes::default()
    }
}

/// The per-instruction record of Layer 1.
#[derive(Debug, Clone, PartialEq)]
pub struct InsFlow {
    pub offset: u32,
    /// Including folded prefixes.
    pub size: u32,
    /// The instruction itself, never a prefix — except for prefixes at the
    /// very end of the body, which have no instruction to fold into and are
    /// reported as the last prefix with no effects.
    pub opcode: OpCode,
    pub prefixes: Prefixes,
    /// One role per popped operand: `inputs[i]` describes `In(i)`.
    pub inputs: Vec<Role>,
    /// Number of Out slots.
    pub outputs: u8,
    pub effects: Vec<Effect<Slot>>,
    pub control: Control,
}

/// What an input operand *is*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Value,
    Object,
    Receiver,
    /// The n-th declared parameter of a call.
    Arg(u16),
    Index,
    Address,
    SrcAddress,
    Size,
    Lhs,
    Rhs,
    Condition,
    Callee,
    Exception,
    ReturnValue,
    Discarded,
}

/// Where control goes after an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Next,
    Goto(u32),
    /// A conditional branch to `taken`, or else to the next instruction.
    Cond {
        taken: Vec<u32>,
    },
    /// `switch`: one of the targets, or else the next instruction.
    Switch(Vec<u32>),
    /// Empties the stack; runs enclosed finally blocks.
    Leave(u32),
    EndFinally,
    EndFilter,
    Return,
    Throw,
    Jump,
}

impl Control {
    /// Whether control can continue with the next instruction.
    pub fn falls_through(&self) -> bool {
        matches!(
            self,
            Control::Next | Control::Cond { .. } | Control::Switch(_)
        )
    }

    /// Explicit branch targets (not the fall-through).
    pub fn targets(&self) -> &[u32] {
        match self {
            Control::Goto(t) | Control::Leave(t) => std::slice::from_ref(t),
            Control::Cond { taken } | Control::Switch(taken) => taken,
            _ => &[],
        }
    }
}

// ---- map -------------------------------------------------------------------

impl<S> Field<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Field<T> {
        self.map_with(&mut f)
    }

    fn map_with<T>(self, f: &mut impl FnMut(S) -> T) -> Field<T> {
        match self {
            Field::Named(op) => Field::Named(op),
            Field::Element { indices } => Field::Element {
                indices: indices.into_iter().map(&mut *f).collect(),
            },
            Field::Length => Field::Length,
            Field::Deref => Field::Deref,
            Field::Block { size } => Field::Block { size: f(size) },
        }
    }
}

impl<S> Root<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Root<T> {
        match self {
            Root::Slot(s) => Root::Slot(f(s)),
            Root::Static => Root::Static,
        }
    }
}

impl<S> Path<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Path<T> {
        self.map_with(&mut f)
    }

    fn map_with<T>(self, f: &mut impl FnMut(S) -> T) -> Path<T> {
        Path {
            root: self.root.map(&mut *f),
            field: self.field.map_with(f),
        }
    }
}

impl<S> Place<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Place<T> {
        self.map_with(&mut f)
    }

    fn map_with<T>(self, f: &mut impl FnMut(S) -> T) -> Place<T> {
        match self {
            Place::Local(n) => Place::Local(n),
            Place::Arg(n) => Place::Arg(n),
            Place::Field(base, op) => Place::Field(f(base), op),
            Place::Static(op) => Place::Static(op),
            Place::Element(base, indices) => {
                let base = f(base);
                Place::Element(base, indices.into_iter().map(&mut *f).collect())
            }
        }
    }
}

impl<S> CallKind<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> CallKind<T> {
        match self {
            CallKind::Static => CallKind::Static,
            CallKind::Instance => CallKind::Instance,
            CallKind::Virtual { constrained } => CallKind::Virtual { constrained },
            CallKind::Ctor => CallKind::Ctor,
            CallKind::Indirect { callee } => CallKind::Indirect { callee: f(callee) },
        }
    }
}

impl<S> CallSite<S> {
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> CallSite<T> {
        self.map_with(&mut f)
    }

    fn map_with<T>(self, f: &mut impl FnMut(S) -> T) -> CallSite<T> {
        // Operand order: receiver, args, callee; then the outputs.
        let receiver = self.receiver.map(&mut *f);
        let args = self.args.into_iter().map(&mut *f).collect();
        let kind = self.kind.map(&mut *f);
        CallSite {
            kind,
            method: self.method,
            sig: self.sig,
            receiver,
            args,
            result: self.result.map(&mut *f),
            thrown: f(self.thrown),
            byref_args: self.byref_args,
            prefixes: self.prefixes,
        }
    }
}

impl<S> Effect<S> {
    /// The same effect with every slot replaced by `f(slot)`. This is how
    /// Layer 2 binds slots to variables.
    pub fn map<T>(self, mut f: impl FnMut(S) -> T) -> Effect<T> {
        let f = &mut f;
        match self {
            Effect::Assign {
                dst,
                srcs,
                transfer,
            } => {
                let srcs = srcs.into_iter().map(&mut *f).collect();
                Effect::Assign {
                    dst: f(dst),
                    srcs,
                    transfer,
                }
            }
            Effect::Const { dst, value } => Effect::Const { dst: f(dst), value },
            Effect::Load { dst, src } => {
                let src = src.map_with(f);
                Effect::Load { dst: f(dst), src }
            }
            Effect::Store { dst, src } => {
                let dst = dst.map_with(f);
                Effect::Store { dst, src: f(src) }
            }
            Effect::AddressOf { dst, place } => {
                let place = place.map_with(f);
                Effect::AddressOf { dst: f(dst), place }
            }
            Effect::New {
                dst,
                kind,
                ty,
                sizes,
            } => {
                let sizes = sizes.into_iter().map(&mut *f).collect();
                Effect::New {
                    dst: f(dst),
                    kind,
                    ty,
                    sizes,
                }
            }
            Effect::FunctionPtr {
                dst,
                method,
                receiver,
            } => {
                let receiver = receiver.map(&mut *f);
                Effect::FunctionPtr {
                    dst: f(dst),
                    method,
                    receiver,
                }
            }
            Effect::Clear { addr } => Effect::Clear { addr: f(addr) },
            Effect::Call(site) => Effect::Call(site.map_with(f)),
            Effect::Return { src } => Effect::Return { src: src.map(f) },
            Effect::Throw { src } => Effect::Throw { src: f(src) },
            Effect::Jump { method } => Effect::Jump { method },
            Effect::Test { srcs } => Effect::Test {
                srcs: srcs.into_iter().map(f).collect(),
            },
            Effect::Opaque { srcs, dsts, reason } => {
                let srcs = srcs.into_iter().map(&mut *f).collect();
                Effect::Opaque {
                    srcs,
                    dsts: dsts.into_iter().map(f).collect(),
                    reason,
                }
            }
        }
    }

    /// Calls `f` on every slot this effect reads, in operand order. A store's
    /// root, an element's indices and an address's base are reads; memory
    /// is not a slot, so what a store writes through a path is not a write.
    pub fn for_each_read(&self, mut f: impl FnMut(&S)) {
        let path = |p: &Path<S>, f: &mut dyn FnMut(&S)| {
            if let Root::Slot(s) = &p.root {
                f(s);
            }
            match &p.field {
                Field::Element { indices } => indices.iter().for_each(&mut *f),
                Field::Block { size } => f(size),
                Field::Named(_) | Field::Length | Field::Deref => {}
            }
        };
        match self {
            Effect::Assign { srcs, .. } | Effect::Test { srcs } | Effect::Opaque { srcs, .. } => {
                srcs.iter().for_each(f)
            }
            Effect::Const { .. } | Effect::Jump { .. } => {}
            Effect::Load { src, .. } => path(src, &mut f),
            Effect::Store { dst, src } => {
                path(dst, &mut f);
                f(src);
            }
            Effect::AddressOf { place, .. } => match place {
                Place::Field(base, _) => f(base),
                Place::Element(base, indices) => {
                    f(base);
                    indices.iter().for_each(f);
                }
                Place::Local(_) | Place::Arg(_) | Place::Static(_) => {}
            },
            Effect::New { sizes, .. } => sizes.iter().for_each(f),
            Effect::FunctionPtr { receiver, .. } => receiver.iter().for_each(f),
            Effect::Clear { addr } => f(addr),
            Effect::Call(site) => {
                // newobj's receiver is the object the New before it wrote:
                // the constructor reads it.
                site.receiver.iter().for_each(&mut f);
                site.args.iter().for_each(&mut f);
                if let CallKind::Indirect { callee } = &site.kind {
                    f(callee);
                }
            }
            Effect::Return { src } => src.iter().for_each(f),
            Effect::Throw { src } => f(src),
        }
    }

    /// Calls `f` on every slot this effect writes.
    pub fn for_each_write(&self, mut f: impl FnMut(&S)) {
        match self {
            Effect::Assign { dst, .. }
            | Effect::Const { dst, .. }
            | Effect::Load { dst, .. }
            | Effect::AddressOf { dst, .. }
            | Effect::New { dst, .. }
            | Effect::FunctionPtr { dst, .. } => f(dst),
            Effect::Opaque { dsts, .. } => dsts.iter().for_each(f),
            Effect::Call(site) => {
                site.result.iter().for_each(&mut f);
                f(&site.thrown);
            }
            Effect::Store { .. }
            | Effect::Clear { .. }
            | Effect::Return { .. }
            | Effect::Jump { .. }
            | Effect::Test { .. } => {}
            Effect::Throw { .. } => {}
        }
    }
}

impl InsFlow {
    /// The offset of the byte after this instruction.
    pub fn end_offset(&self) -> u32 {
        self.offset + self.size
    }

    /// Whether any effect is [`Effect::Opaque`].
    pub fn is_opaque(&self) -> bool {
        self.effects
            .iter()
            .any(|e| matches!(e, Effect::Opaque { .. }))
    }
}

// ---- Display ---------------------------------------------------------------

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Slot::In(n) => write!(f, "i{n}"),
            Slot::Out(n) => write!(f, "o{n}"),
            Slot::Arg(n) => write!(f, "arg{n}"),
            Slot::Local(n) => write!(f, "loc{n}"),
            Slot::Temp(n) => write!(f, "t{n}"),
            Slot::Thrown => f.write_str("thrown"),
            Slot::Caught => f.write_str("caught"),
        }
    }
}

fn tok(t: MetadataToken) -> String {
    format!("{:08x}", t.raw())
}

fn list<S: fmt::Display>(items: &[S]) -> String {
    items
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for FieldOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            if self.is_static { "static " } else { "" },
            tok(self.token)
        )
    }
}

impl fmt::Display for TypeOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&tok(self.token))
    }
}

impl fmt::Display for MethodOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&tok(self.token))
    }
}

impl<S: fmt::Display> fmt::Display for Field<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Field::Named(op) => write!(f, ".<{op}>"),
            Field::Element { indices } => write!(f, "[{}]", list(indices)),
            Field::Length => f.write_str(".length"),
            Field::Deref => f.write_str(".*"),
            Field::Block { size } => write!(f, ".*[..{size}]"),
        }
    }
}

impl<S: fmt::Display> fmt::Display for Root<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Root::Slot(s) => s.fmt(f),
            Root::Static => f.write_str("static"),
        }
    }
}

impl<S: fmt::Display> fmt::Display for Path<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.root, self.field)
    }
}

impl<S: fmt::Display> fmt::Display for Place<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Place::Local(n) => write!(f, "loc{n}"),
            Place::Arg(n) => write!(f, "arg{n}"),
            Place::Field(base, op) => write!(f, "{base}.<{op}>"),
            Place::Static(op) => write!(f, "static.<{op}>"),
            Place::Element(base, indices) => write!(f, "{base}[{}]", list(indices)),
        }
    }
}

impl fmt::Display for OpFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ovf {
            f.write_str(".ovf")?;
        }
        if self.un {
            f.write_str(".un")?;
        }
        Ok(())
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::Add(fl) => write!(f, "add{fl}"),
            Op::Sub(fl) => write!(f, "sub{fl}"),
            Op::Mul(fl) => write!(f, "mul{fl}"),
            Op::Div(fl) => write!(f, "div{fl}"),
            Op::Rem(fl) => write!(f, "rem{fl}"),
            Op::And => f.write_str("and"),
            Op::Or => f.write_str("or"),
            Op::Xor => f.write_str("xor"),
            Op::Shl => f.write_str("shl"),
            Op::Shr(fl) => write!(f, "shr{fl}"),
            Op::Neg => f.write_str("neg"),
            Op::Not => f.write_str("not"),
            Op::Conv(t) => write!(f, "conv{}.{:?}", t.flags, t.ty),
            Op::Cmp(op, fl) => write!(f, "cmp.{op:?}{fl}"),
            Op::MkRefAny(t) => write!(f, "mkrefany {t}"),
            Op::RefAnyType => f.write_str("refanytype"),
            Op::RefAnyVal(t) => write!(f, "refanyval {t}"),
            Op::Opaque => f.write_str("opaque"),
        }
    }
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transfer::Copy => f.write_str("Copy"),
            Transfer::Cast(t) => write!(f, "Cast({t})"),
            Transfer::Box(t) => write!(f, "Box({t})"),
            Transfer::Unbox(t) => write!(f, "Unbox({t})"),
            Transfer::Derived(op) => write!(f, "Derived({op})"),
        }
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Const::I4(v) => write!(f, "I4({v})"),
            Const::I8(v) => write!(f, "I8({v})"),
            // Bit patterns, so the dump is exact.
            Const::R4(v) => write!(f, "R4(0x{:08x})", v.to_bits()),
            Const::R8(v) => write!(f, "R8(0x{:016x})", v.to_bits()),
            Const::Null => f.write_str("Null"),
            Const::String(t) => write!(f, "String({})", tok(*t)),
            Const::Token(t) => write!(f, "Token({})", tok(*t)),
            Const::SizeOf(t) => write!(f, "SizeOf({t})"),
            Const::ArgList => f.write_str("ArgList"),
        }
    }
}

impl fmt::Display for Prefixes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.volatile {
            parts.push("volatile".to_string());
        }
        if let Some(n) = self.unaligned {
            parts.push(format!("unaligned({n})"));
        }
        if self.tail {
            parts.push("tail".to_string());
        }
        if self.readonly {
            parts.push("readonly".to_string());
        }
        if let Some(n) = self.no {
            parts.push(format!("no({n})"));
        }
        if let Some(t) = self.constrained {
            parts.push(format!("constrained({t})"));
        }
        f.write_str(&parts.join(" "))
    }
}

impl<S: fmt::Display> fmt::Display for CallSite<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            CallKind::Static => f.write_str("Call Static")?,
            CallKind::Instance => f.write_str("Call Instance")?,
            CallKind::Virtual { constrained: None } => f.write_str("Call Virtual")?,
            CallKind::Virtual {
                constrained: Some(t),
            } => write!(f, "Call Virtual{{constrained={t}}}")?,
            CallKind::Ctor => f.write_str("Call Ctor")?,
            CallKind::Indirect { callee } => write!(f, "Call Indirect{{callee={callee}}}")?,
        }
        if let Some(m) = &self.method {
            write!(f, " {m}")?;
        }
        if let Some(r) = &self.receiver {
            write!(f, " recv={r}")?;
        }
        write!(f, " args=[{}]", list(&self.args))?;
        if let Some(r) = &self.result {
            write!(f, " result={r}")?;
        }
        write!(f, " thrown={}", self.thrown)?;
        if !self.byref_args.is_empty() {
            write!(f, " byref={:?}", self.byref_args)?;
        }
        if !self.prefixes.is_empty() {
            write!(f, " prefixes=[{}]", self.prefixes)?;
        }
        Ok(())
    }
}

impl<S: fmt::Display> fmt::Display for Effect<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::Assign {
                dst,
                srcs,
                transfer,
            } => write!(f, "Assign {dst} := [{}] {transfer}", list(srcs)),
            Effect::Const { dst, value } => write!(f, "Const {dst} = {value}"),
            Effect::Load { dst, src } => write!(f, "Load {dst} := {src}"),
            Effect::Store { dst, src } => write!(f, "Store {dst} := {src}"),
            Effect::AddressOf { dst, place } => write!(f, "AddressOf {dst} = &{place}"),
            Effect::New {
                dst,
                kind,
                ty,
                sizes,
            } => {
                write!(f, "New {dst} {kind:?}")?;
                if let Some(t) = ty {
                    write!(f, "({t})")?;
                }
                if !sizes.is_empty() {
                    write!(f, " sizes=[{}]", list(sizes))?;
                }
                Ok(())
            }
            Effect::FunctionPtr {
                dst,
                method,
                receiver,
            } => {
                write!(f, "FunctionPtr {dst} = {method}")?;
                if let Some(r) = receiver {
                    write!(f, " receiver={r}")?;
                }
                Ok(())
            }
            Effect::Clear { addr } => write!(f, "Clear {addr}"),
            Effect::Call(site) => site.fmt(f),
            Effect::Return { src: None } => f.write_str("Return"),
            Effect::Return { src: Some(s) } => write!(f, "Return {s}"),
            Effect::Throw { src } => write!(f, "Throw {src}"),
            Effect::Jump { method } => write!(f, "Jump {method}"),
            Effect::Test { srcs } => write!(f, "Test [{}]", list(srcs)),
            Effect::Opaque { srcs, dsts, reason } => {
                let reason = match reason {
                    OpaqueReason::UnresolvedToken(t) => format!("unresolved {}", tok(*t)),
                    OpaqueReason::UndefinedOpcode => "undefined opcode".to_string(),
                };
                write!(f, "Opaque [{}] -> [{}] ({reason})", list(srcs), list(dsts))
            }
        }
    }
}

impl fmt::Display for Control {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = |t: &u32| format!("IL_{t:04x}");
        match self {
            Control::Next => f.write_str("next"),
            Control::Goto(t) => write!(f, "goto {}", label(t)),
            Control::Cond { taken } => write!(
                f,
                "cond {}",
                taken.iter().map(label).collect::<Vec<_>>().join(", ")
            ),
            Control::Switch(ts) => write!(
                f,
                "switch ({})",
                ts.iter().map(label).collect::<Vec<_>>().join(", ")
            ),
            Control::Leave(t) => write!(f, "leave {}", label(t)),
            Control::EndFinally => f.write_str("endfinally"),
            Control::EndFilter => f.write_str("endfilter"),
            Control::Return => f.write_str("return"),
            Control::Throw => f.write_str("throw"),
            Control::Jump => f.write_str("jump"),
        }
    }
}

#[cfg(test)]
mod opcode_tests;
#[cfg(test)]
mod tests;
