//! Function shape (translation §5) and control (§8): one `FunctionData` per method, its
//! blocks one-to-one with the `MethodFlow`'s, and a terminator for each.
//!
//! Signature (§5.1):
//!
//! | CIL                                         | `FunctionData`                    |
//! | ------------------------------------------- | --------------------------------- |
//! | `has_this`                                  | param 0, `ByRef`                  |
//! | reference type, `T&`, `T*`, generic param   | `ByRef`                           |
//! | primitive or value type                     | `ByVal`                           |
//! | return                                      | `ReturnType { arity: 2 }`         |
//!
//! Variables (§5.2):
//!
//! | `Var`       | `VariableRef`        |
//! | ----------- | -------------------- |
//! | `Arg(n)`    | parameter `n`        |
//! | `Local(n)`  | local `loc{n}`       |
//! | `Stack(d)`  | local `s{d}`         |
//! | `Temp(k)`   | local `t{k}`         |
//! | `Caught(h)` | local `caught{h}`    |
//! | `Thrown`    | local `throwval`     |
//! | call result | local `retval`       |
//! | exceptions leaving the method | local `excout` |
//!
//! The frontend's own temporaries are `{prefix}_{n}` (`cell_3`, `c_4`), so they never collide
//! with the flow layer's `t{k}`.
//!
//! Control (§8):
//!
//! | Last `Control` of the block | Terminator                                         |
//! | --------------------------- | -------------------------------------------------- |
//! | `Next` `Goto` `Cond` `Switch` `Leave` `EndFilter` `EndFinally` | `Goto { succs ++ exc_succs }`; none: `Return [empty, excout]` |
//! | `Return`                    | `Return [v or empty, excout]`                      |
//! | `Throw`                     | `Goto { exc_succs }`; unprotected: `Return [empty, excout]` |
//! | `Jump m`                    | call `m` with every parameter, `Return [retval, excout]` |
//!
//! The exceptional slot of every return is `excout`, where the doc has `throwval` at a throw
//! and `empty` elsewhere. An exception leaves a method from more places than a `throw` with
//! no handler: from any call that is not in a `try`, and from a protected point whose
//! handlers may not take it (a typed catch, a filter, a finally). Each such point adds its
//! `throwval` into `excout` (`excout := excout, throwval`), defined empty at entry, and the
//! value reaches the caller through whichever return the method takes. Ending a block after
//! every call instead would give each its own edge out, but would cut the load chains the
//! index composes paths along within a block (§12.3). `cil:taint` found the gap:
//! `Exc.Uncaught` is `Throws(a) + 1`, whose callee's exception had no way out.

use cil_reader::flow::{
    BlockId, BoundIns, Control, Effect, FlowOptions, HandlerKind, InsId, MethodFlow, Var,
};
use cil_reader::model::{MethodBody, MethodDef, MethodSig, TypeSig};
use cil_reader::token::TokenType;
use ctadl_ir::mir::{FieldRef, LocalIdx};
use ctadl_ir::*;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder, SpanLen};

use crate::Collected;
use crate::names::{ModNames, Namer};
use crate::ptr::{Addresses, VarInfo};

/// The flow layer options this frontend builds with: a block ends after every call and
/// throw inside a protected range (§8, "precise exception edges").
pub const FLOW_OPTIONS: FlowOptions = FlowOptions {
    split_after_throwing_in_try: true,
};

/// Per-function lowering state: the one place statements are appended.
pub struct Emitter<'f> {
    pub fdat: &'f mut FunctionData,
    pub block: BasicBlockIdx,
    pub source_info: SourceInfo,
    fresh: u32,
}

impl<'f> Emitter<'f> {
    pub fn new(fdat: &'f mut FunctionData) -> Self {
        Emitter {
            fdat,
            block: BasicBlockIdx::new(0),
            source_info: SourceInfo::default(),
            fresh: 0,
        }
    }

    /// The `VariableRef` for a flow-layer variable (§5.2).
    pub fn var(&mut self, v: Var) -> VariableRef {
        match v {
            // Code that names an argument past its signature does not verify; a local of
            // its own keeps the IR well formed.
            Var::Arg(n) if n as usize >= self.fdat.params.parameters.len() => {
                self.local(&format!("badarg{n}"))
            }
            Var::Arg(n) => VariableRef::new_parameter(ParameterIdx::new(n as usize)),
            Var::Local(n) => self.local(&format!("loc{n}")),
            Var::Stack(d) => self.local(&format!("s{d}")),
            Var::Temp(k) => self.local(&format!("t{k}")),
            Var::Caught(h) => self.local(&format!("caught{h}")),
            Var::Thrown => self.throwval(),
        }
    }

    pub fn exp(&mut self, v: Var) -> Exp {
        Exp::Variable(self.var(v))
    }

    pub fn local(&mut self, name: &str) -> VariableRef {
        let idx: LocalIdx = self.fdat.locals.get_or_intern(name);
        VariableRef::new_local_idx(idx)
    }

    /// A local no other statement names: `{prefix}_{n}`.
    pub fn fresh(&mut self, prefix: &str) -> VariableRef {
        let n = self.fresh;
        self.fresh += 1;
        self.local(&format!("{prefix}_{n}"))
    }

    pub fn retval(&mut self) -> VariableRef {
        self.local("retval")
    }

    pub fn throwval(&mut self) -> VariableRef {
        self.local("throwval")
    }

    /// The exceptions that may leave the method (see the module docs).
    pub fn excout(&mut self) -> VariableRef {
        self.local("excout")
    }

    pub fn push(&mut self, kind: StatementKind) {
        let stmt = Statement::new(kind, self.source_info);
        self.fdat.blocks[self.block].push_back(stmt);
    }

    pub fn assign(&mut self, dst: VariableRef, srcs: impl IntoIterator<Item = Exp>) {
        self.push(StatementKind::assign(dst, srcs));
    }

    pub fn load(&mut self, dst: VariableRef, base: VariableRef, field: FieldRef) {
        self.push(StatementKind::load(
            dst,
            AccessPath::without_fields(base),
            field,
        ));
    }

    pub fn store(&mut self, base: VariableRef, field: FieldRef, value: Exp) {
        self.push(StatementKind::store(
            AccessPath::without_fields(base),
            field,
            value,
        ));
    }
}

/// `ByRef` or `ByVal` for a parameter of this type (§5.1). A generic parameter is `ByRef`
/// because it may be instantiated with a reference type.
pub fn parameter_type(sig: &TypeSig) -> ParameterType {
    match sig.unwrap_modifiers() {
        TypeSig::Named { is_value_type, .. } | TypeSig::GenericInst { is_value_type, .. } => {
            if *is_value_type {
                ParameterType::ByVal
            } else {
                ParameterType::ByRef
            }
        }
        TypeSig::String
        | TypeSig::Object
        | TypeSig::SzArray(_)
        | TypeSig::Array { .. }
        | TypeSig::Var(_)
        | TypeSig::MVar(_)
        | TypeSig::ByRef(_)
        | TypeSig::Ptr(_) => ParameterType::ByRef,
        TypeSig::Void
        | TypeSig::Boolean
        | TypeSig::Char
        | TypeSig::I1
        | TypeSig::U1
        | TypeSig::I2
        | TypeSig::U2
        | TypeSig::I4
        | TypeSig::U4
        | TypeSig::I8
        | TypeSig::U8
        | TypeSig::R4
        | TypeSig::R8
        | TypeSig::IntPtr
        | TypeSig::UIntPtr
        | TypeSig::TypedByRef
        | TypeSig::FnPtr(_) => ParameterType::ByVal,
        TypeSig::CModReq { .. }
        | TypeSig::CModOpt { .. }
        | TypeSig::Pinned(_)
        | TypeSig::Sentinel(_) => unreachable!("unwrap_modifiers strips these"),
    }
}

/// Whether a variable of this declared type holds an address: a `T&` (`ref` parameters and
/// locals, pinned or not) or a `T*`.
pub fn holds_address(sig: &TypeSig) -> bool {
    matches!(sig.unwrap_modifiers(), TypeSig::ByRef(_) | TypeSig::Ptr(_))
}

/// The parameters of a function with this signature: `this` first, if it has one.
pub fn parameters(sig: &MethodSig) -> Vec<ParameterType> {
    let mut params = Vec::with_capacity(sig.params.len() + 1);
    if sig.has_this {
        params.push(ParameterType::ByRef);
    }
    for p in &sig.params {
        if p.is_sentinel() {
            break;
        }
        params.push(parameter_type(p));
    }
    params
}

/// Gives `fdat` the shape of a function with signature `sig` and no blocks.
pub fn shape(fdat: &mut FunctionData, name: String, params: Vec<ParameterType>) {
    fdat.name = name;
    fdat.params = Params {
        parameters: params.into_iter().collect(),
    };
    fdat.return_type = ReturnType { arity: 2 };
}

/// Everything about a method that lowering one of its instructions reads.
pub struct Lower<'x, 'n, 'm, 'c, 'l> {
    pub names: &'x ModNames<'n, 'm, 'c, 'l>,
    pub namer: &'n Namer<'c, 'l>,
    pub module: &'m cil_reader::Module<'m>,
    pub body: &'x MethodBody<'m>,
    pub flow: &'x MethodFlow,
    /// Whether `Arg(n)` holds an address: a byref parameter, or `this` of a value type.
    pub ptr_args: Vec<bool>,
    /// Whether `Local(n)` holds an address: a `ref` local.
    pub ptr_locals: Vec<bool>,
    pub out: &'x mut Collected,
    /// The value-type `New` of the current instruction, whose constructor gets an address.
    pub new_value: Option<Var>,
    /// Within the current block, which variables hold a function pointer to a static method,
    /// and to which: what a delegate constructor needs to bind it through a thunk (§7).
    pub static_fnptrs: hashbrown::HashMap<Var, String>,
    /// Where each instruction input points (`ptr.rs`).
    pub addrs: Addresses,
    /// The instruction being lowered.
    pub cur: InsId,
}

/// Lowers one method with a body into `fdat`, whose shape is already set.
#[allow(clippy::too_many_arguments)]
pub fn lower_body<'n, 'm, 'c, 'l>(
    names: &ModNames<'n, 'm, 'c, 'l>,
    method: &MethodDef<'_>,
    body: &MethodBody<'m>,
    sig: &MethodSig,
    fdat: &mut FunctionData,
    key: &ArtifactKey,
    spans: &mut SourceInfoBuilder,
    out: &mut Collected,
) {
    let flow = MethodFlow::build_with(body, FLOW_OPTIONS);
    for w in &flow.warnings {
        log::debug!("{}: flow: {w}", fdat.name);
    }
    if flow.blocks.is_empty() {
        return;
    }
    let this_is_address = sig.has_this
        && method
            .declaring_type()
            .ok()
            .flatten()
            .is_some_and(|t| t.is_value_type().unwrap_or(false));
    let mut ptr_args = Vec::new();
    if sig.has_this {
        ptr_args.push(this_is_address);
    }
    ptr_args.extend(sig.params.iter().map(holds_address));
    let ptr_locals: Vec<bool> = body.variables.iter().map(holds_address).collect();
    let addrs = Addresses::compute(
        &flow,
        &VarInfo {
            ptr_args: &ptr_args,
            ptr_locals: &ptr_locals,
        },
    );

    let mut lower = Lower {
        names,
        namer: names.namer,
        module: names.module,
        body,
        flow: &flow,
        ptr_args,
        ptr_locals,
        out,
        new_value: None,
        static_fnptrs: Default::default(),
        addrs,
        cur: InsId { block: 0, index: 0 },
    };
    let blocks = &flow.blocks;
    for _ in blocks {
        fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    }
    let mut em = Emitter::new(fdat);
    for (b, block) in blocks.iter().enumerate() {
        em.block = BasicBlockIdx::new(b);
        lower.static_fnptrs.clear();
        if b == 0 {
            em.source_info = span(key, spans, body, block.start, 1);
            let out = em.excout();
            em.assign(out, [empty()]);
        }
        // A catch or filter starts with the exception in `caught{h}`; the binder's own
        // first instruction then copies it to `s0`.
        if let Some(h) = block.handler_entry {
            let handler = &flow.handlers[h as usize];
            if handler.entry == Some(b)
                && matches!(handler.kind, HandlerKind::Catch | HandlerKind::Filter)
            {
                em.source_info = span(key, spans, body, block.start, 1);
                let caught = em.var(Var::Caught(h));
                let thrown = em.throwval();
                em.assign(caught, [Exp::Variable(thrown)]);
            }
        }
        for (index, ins) in block.instrs.iter().enumerate() {
            em.source_info = span(key, spans, body, ins.flow.offset, ins.flow.size.max(1));
            let at = InsId { block: b, index };
            lower.cur = at;
            lower.cross_check(at, ins);
            lower.instruction(&mut em, at, ins);
        }
        let term = lower.terminator(&mut em, b);
        em.fdat.blocks[em.block].terminator = Some(Terminator::new_kind(term));
    }
}

/// The body of a delegate thunk (§7): call `target` with every parameter but the first,
/// the ignored target, and return what it returns.
pub fn thunk_body(fdat: &mut FunctionData, target: &str, target_arity: usize) {
    fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    let mut em = Emitter::new(fdat);
    let args: ThinVec<Exp> = (1..=target_arity)
        .map(|n| Exp::Variable(VariableRef::new_parameter(ParameterIdx::new(n))))
        .collect();
    let retval = em.retval();
    let thrown = em.throwval();
    em.push(StatementKind::CallAssign {
        style: CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(thin_vec![target.to_string()]),
        },
        rets: thin_vec![retval.clone(), thrown.clone()],
        args,
    });
    em.fdat.blocks[em.block].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![Exp::Variable(retval), Exp::Variable(thrown)],
    }));
}

fn span(
    key: &ArtifactKey,
    spans: &mut SourceInfoBuilder,
    body: &MethodBody<'_>,
    offset: u32,
    size: u32,
) -> SourceInfo {
    SourceInfo::new(spans.span_for(
        key.clone(),
        body.code_file_offset() + offset,
        SpanLen::ByteLen(size),
    ))
}

fn empty() -> Exp {
    Exp::new_bytes(Vec::new())
}

impl Lower<'_, '_, '_, '_, '_> {
    /// The terminator of block `b` (§8).
    fn terminator(&mut self, em: &mut Emitter<'_>, b: BlockId) -> TerminatorKind {
        let block = &self.flow.blocks[b];
        let last = block.instrs.last();
        let control = last
            .map(|i| i.flow.control.clone())
            .unwrap_or(Control::Next);
        let mut targets: SmallVec<[BasicBlockIdx; 4]> = SmallVec::new();
        let add = |t: BasicBlockIdx, targets: &mut SmallVec<[BasicBlockIdx; 4]>| {
            if !targets.contains(&t) {
                targets.push(t);
            }
        };
        let out = Exp::Variable(em.excout());
        match control {
            Control::Return => {
                let value = last
                    .and_then(|ins| {
                        ins.effects.iter().find_map(|e| match e {
                            Effect::Return { src: Some(v) } => Some(*v),
                            _ => None,
                        })
                    })
                    .map(|v| {
                        self.cur = InsId {
                            block: b,
                            index: block.instrs.len() - 1,
                        };
                        self.value(em, last.unwrap(), v)
                    })
                    .unwrap_or_else(empty);
                return TerminatorKind::Return {
                    args: smallvec![value, out],
                };
            }
            Control::Throw => {
                for &s in &block.exc_succs {
                    add(BasicBlockIdx::new(s), &mut targets);
                }
            }
            Control::Jump => {
                let thrown = em.throwval();
                return self.jump(em, last.unwrap(), thrown);
            }
            // After an `endfinally`, the exception the finally was entered with goes on to the
            // next handler out (`exc_succs`); one leaving the method is already in `excout`.
            Control::EndFinally
            | Control::Next
            | Control::Goto(_)
            | Control::Cond { .. }
            | Control::Switch(_)
            | Control::Leave(_)
            | Control::EndFilter => {
                for &s in block.succs.iter().chain(&block.exc_succs) {
                    add(BasicBlockIdx::new(s), &mut targets);
                }
            }
        }
        if targets.is_empty() {
            TerminatorKind::Return {
                args: smallvec![empty(), out],
            }
        } else {
            TerminatorKind::Goto { targets }
        }
    }

    /// After a call or a throw: when the exception it may raise can leave the method (no
    /// catch-all handler takes it), add it to `excout`.
    pub fn raise_out(&mut self, em: &mut Emitter<'_>) {
        if self.caught_by_all(self.cur.block) {
            return;
        }
        let (out, thrown) = (em.excout(), em.throwval());
        em.assign(out.clone(), [Exp::Variable(out), Exp::Variable(thrown)]);
    }

    /// Whether every exception raised in block `b` is caught in this method: some handler it
    /// reaches is a catch of `System.Object` or `System.Exception`.
    fn caught_by_all(&self, b: BlockId) -> bool {
        let block = &self.flow.blocks[b];
        self.flow.handlers.iter().any(|h| {
            h.kind == HandlerKind::Catch
                && h.entry.is_some_and(|e| block.exc_succs.contains(&e))
                && h.catch_type.is_some_and(|t| {
                    matches!(
                        t.token.token_type(),
                        Some(TokenType::TypeDef | TokenType::TypeRef | TokenType::TypeSpec)
                    ) && self.names.token_cls(t.token).is_some_and(|c| {
                        c.ends_with("]System.Object") || c.ends_with("]System.Exception")
                    })
                })
        })
    }

    /// `jmp m`: call `m` with every parameter, and return what it returns.
    fn jump(
        &mut self,
        em: &mut Emitter<'_>,
        ins: &BoundIns,
        thrown: VariableRef,
    ) -> TerminatorKind {
        let Some(Effect::Jump { method }) = ins
            .effects
            .iter()
            .find(|e| matches!(e, Effect::Jump { .. }))
        else {
            return TerminatorKind::Return {
                args: smallvec![empty(), empty()],
            };
        };
        let args: ThinVec<Exp> = (0..em.fdat.params.parameters.len())
            .map(|n| Exp::Variable(VariableRef::new_parameter(ParameterIdx::new(n))))
            .collect();
        let retval = em.retval();
        if let Some(key) = self.names.method_key(method.token) {
            let fid = key.fid();
            self.out.callee(&key, parameters(&method.sig));
            em.push(StatementKind::CallAssign {
                style: CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(thin_vec![fid]),
                },
                rets: thin_vec![retval.clone(), thrown],
                args,
            });
            self.raise_out(em);
        }
        TerminatorKind::Return {
            args: smallvec![Exp::Variable(retval), Exp::Variable(em.excout())],
        }
    }
}
