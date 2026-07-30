/*! Generates Datalog code from CTADL IR.

# Introduction

Codegen is the process of generating Datalog code from CTADL IR. CTADL IR, the input, is expressed
as a [`ctadl_ir::mir::ProgramInfo`] [`crate::index_engine::IndexFacts`] is the output type. This
phase provides a hook in [`crate::models::codegen`] to run arbitrary code that generates models for
indexing.

# Details

Some notes about choices made in the design of generating code:

Parameters in IR are mapped to the same indices in the Datalog. Return values are mapped to index
-1, -2, -3, etc. The global heap is mapped to [`GLOBALS_INDEX`], which is [`i16::MIN`].

*/
use std::collections::{BTreeMap, BTreeSet};

use smallvec::SmallVec;

use crate::facts as fx;
use crate::facts::{FlowVariable, FlowVariableKind, FlowVertex, FormalIndex, Str};
use crate::index_engine::{IndexFacts, source_info::IndexSourceInfo};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::{call::VirtualMethodTable, visit::Visitor, *};

#[cfg(test)]
mod tests;

pub mod flowy;
pub mod model_matches;

/// Strategy for resolving virtual calls
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CallResolutionStrategy {
    /// Every call is resolved with Class Hierarchy Analysis.
    Cha,
    /// Every call is resolved with hybrid inlining (no calls resolved with CHA).
    Hi,
    /// CHA for easy calls, hybrid inlining otherwise.
    #[default]
    Mixed,
}

/// Generate code for a program in SSA form (see [`ctadl_ir::ssa::transform`]).
#[inline]
pub fn codegen_program(
    mut program_info: ProgramInfo,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
    strategy: CallResolutionStrategy,
) {
    let mut instantiated_classes = BTreeSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    for f in program_info.program.functions.iter() {
        finder.visit_function_data(FunctionIdx::new(0), f);
    }

    let cha = ClassHierarchyAnalysis::new(&program_info.vmt, instantiated_classes);
    emit_callee_resolvents(&cha, facts, source_info);
    let mut v = CodegenVisitor::new(cha, facts, source_info, strategy);
    for f in program_info.program.functions.drain(..) {
        v.visit_function_data(FunctionIdx::new(0), &f);
    }
    v.finish_with_vmt(&program_info.vmt);
}

/// Generate code for a function in SSA form (see [`ctadl_ir::ssa::transform`]).
///
/// TODO this function doesn't do any class hierarchy analysis which seems like a bug waiting to
/// happen. It also doesn't apply any codegen models
#[inline]
pub fn codegen_function(
    function_data: &FunctionData,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    let function_data_owned = function_data.clone();
    let function_data = &function_data_owned;
    let mut instantiated_classes = BTreeSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    finder.visit_function_data(FunctionIdx::new(0), function_data);

    let cha = ClassHierarchyAnalysis::new(&VirtualMethodTable::Unknown, instantiated_classes);
    emit_callee_resolvents(&cha, facts, source_info);
    log::trace!("codegen for {}", function_data.name);
    let mut v = CodegenVisitor::new(cha, facts, source_info, CallResolutionStrategy::Mixed);
    v.visit_function_data(FunctionIdx::new(0), function_data);
    v.finish();
}

/// For passing globals reference in parameter list
pub const GLOBALS_INDEX: i16 = i16::MIN;

/// Start of return values. If there's more than one return value, uses -2, -3, etc
pub const RETURN_INDEX: i16 = -1i16;

pub fn variable_is_globals(v: &FlowVariable) -> bool {
    match v.kind() {
        FlowVariableKind::Formal(idx) => *idx == GLOBALS_INDEX,
        // This has to be kept in sync with the name given to globals in the CodegenVisitor
        FlowVariableKind::Local(name) => name.starts_with("$globals_"),
        _ => false,
    }
}

/// The `call_target_assign` payload for an *object-valued* expression: the class of an
/// allocation-site tag, in the receiving language's own [`fx::CallTargetObject`] variant.
/// `None` for anything else, including a `FunctionPtr` ref — those carry an interned function
/// id and so are handled at each site, which needs the id for `funcptr_targets` too.
fn call_target_object(exp: &Exp) -> Option<fx::CallTargetObject> {
    match exp {
        Exp::ObjectRef(CallObject::JavaObject(cls)) => {
            Some(fx::CallTargetObject::Symbol(cls.0.clone()))
        }
        Exp::ObjectRef(CallObject::LuaClass(cls)) => {
            Some(fx::CallTargetObject::LuaClass(cls.clone()))
        }
        _ => None,
    }
}

struct InstantiationFinder<'a> {
    instantiated_classes: &'a mut BTreeSet<Symbol>,
}

impl Visitor for InstantiationFinder<'_> {
    fn visit_exp(&mut self, exp: &Exp) {
        match exp {
            Exp::ObjectRef(CallObject::JavaObject(cls)) => {
                self.instantiated_classes.insert(cls.0.clone());
            }
            Exp::ObjectRef(CallObject::LuaClass(cls)) => {
                self.instantiated_classes.insert(cls.clone());
            }
            _ => {}
        }
        self.super_exp(exp);
    }
}

#[derive(Debug)]
struct CodegenVisitor<'a> {
    /// Output facts
    facts: &'a mut IndexFacts,
    source_info: &'a mut IndexSourceInfo,
    cha: ClassHierarchyAnalysis,
    strategy: CallResolutionStrategy,
    /// Name of the function under translation (set in visit_function_data)
    function: Option<fx::FunctionId>,
    /// We may see the same access path multiple times so we dedup them with this set
    paths_dedup: BTreeSet<(fx::Path,)>,
    /// Per-block map from a load-chain temporary to the (root variable, composed field path) it
    /// stands for (`t2 = load t1.b` where `t1 = load x.a` ⟹ `t2 ↦ (x, .a.b)`). Populated in the
    /// pre-pass over each block ([`Self::visit_basic_block_data`]) and used to *re-anchor* a
    /// `Store` through such a temporary (`store t2.c := v`) back onto the formal path it addresses
    /// (`x.a.b.c := v`), so a write through a loaded pointer is recorded at the object it names
    /// rather than at the temporary. Cleared per block.
    cap_path: BTreeMap<VariableRef, (VariableRef, fx::Path)>,
    /// Distinct functions stored as C-style call targets (`CallTargetObject::FunctionId`)
    /// anywhere in this codegen unit. Each gets an identity `callee_resolvents(FunctionId(f),
    /// C, f)` fact emitted in [`Self::finish`] — the function-pointer analogue of the CHA
    /// `callee_resolvents`, which is what lets the unified resolution rules resolve a reached
    /// function pointer to itself without a `C`-specific rule in the index engine.
    funcptr_targets: BTreeSet<fx::FunctionId>,
}

impl<'a> CodegenVisitor<'a> {
    /// Codegen visitor. Generates facts into the index facts. Call the visitor to generate the
    /// facts. You must call [`CodegenVisitor::finish`] to get all the facts.
    #[inline]
    fn new(
        cha: ClassHierarchyAnalysis,
        facts: &'a mut IndexFacts,
        source_info: &'a mut IndexSourceInfo,
        strategy: CallResolutionStrategy,
    ) -> Self {
        Self {
            function: None,
            facts,
            cha,
            source_info,
            strategy,
            paths_dedup: Default::default(),
            cap_path: Default::default(),
            funcptr_targets: Default::default(),
        }
    }

    /// Gens the dedup'd paths to the facts
    fn finish(&mut self) {
        // The empty path (whole-variable flow) must always be in the `paths` gate so the
        // forward field-propagation rules can reach a scalar. Previously this was implied by
        // every pathless `Exp::AccessPath` carrying an (empty) field-access list that the
        // visitor inserted; now that a pathless read is `Exp::Variable` (no field-access list),
        // insert it explicitly. It is trivially bounded, so it does not affect termination.
        if std::env::var_os("CTADL_NO_EMPTY_PATH").is_none() {
            self.paths_dedup.insert((fx::Path::empty(),));
        }
        let paths = std::mem::take(&mut self.paths_dedup);
        self.facts.paths.extend(paths);
        let funcptr_targets = std::mem::take(&mut self.funcptr_targets);
        self.facts
            .callee_resolvents
            .extend(funcptr_targets.into_iter().map(|f| {
                (
                    fx::CallTargetObject::FunctionId(f),
                    fx::CallDispatchKey::C,
                    f,
                )
            }));
    }

    /// Does finish and also runs a datalog modeling pass
    #[inline]
    fn finish_with_vmt(&mut self, vmt: &VirtualMethodTable) {
        self.finish();
        crate::models::codegen::load_models(vmt, self.facts, &self.source_info.sites);
    }
}

impl Visitor for CodegenVisitor<'_> {
    #[inline]
    fn visit_function_data(&mut self, idx: FunctionIdx, function: &FunctionData) {
        let func = fx::Function(function.name.clone().into());
        let func_id = self.source_info.sites.get_or_add_function(func);
        self.function = Some(func_id);
        if function.blocks.is_empty() {
            self.facts.external_function.push((func_id,));
        }
        // Gens global param
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::formal_index(GLOBALS_INDEX.into()),
            fx::FormalType::ByRef,
        ));
        // Gens return parameter
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::formal_index(RETURN_INDEX.into()),
            fx::FormalType::ByRef,
        ));
        self.super_function_data(idx, function);
    }

    #[inline]
    fn visit_basic_block_data(
        &mut self,
        function: FunctionIdx,
        block: BasicBlockIdx,
        data: &BasicBlockData,
    ) {
        // Pre-pass: capture, for each load-chain temporary, the (root variable, composed field
        // path) it stands for. Used below to seed the `paths` gate and, in
        // `visit_statement_kind`, to re-anchor a `Store` through such a temporary onto the formal
        // path it addresses.
        self.cap_path.clear();
        for statement in &data.statements {
            match &statement.kind {
                StatementKind::Assign { dest, sources } if sources.len() == 1 => {
                    match &sources[0] {
                        // A whole-variable copy `dest = v` carries the captured (root, path) of `v`
                        // forward, so a Load chain that flows through a copy still composes.
                        Exp::Variable(v) => {
                            if let Some(cap) = self.cap_path.get(v).cloned() {
                                self.cap_path.insert(dest.clone(), cap);
                            }
                        }
                        // An address copy `dest = v.[k]` (pointer arithmetic) names the field
                        // path `<captured path of v> ++ [k]` rooted at v's root; record it so a
                        // later Load/Store through `dest` composes onto it and the composed path
                        // enters the `paths` gate.
                        Exp::AccessPath(ap) => {
                            let (root, base_path) = self
                                .cap_path
                                .get(&ap.variable_ref)
                                .cloned()
                                .unwrap_or_else(|| (ap.variable_ref.clone(), fx::Path::empty()));
                            let path = fx::Path::from_accesses(
                                base_path
                                    .iter()
                                    .cloned()
                                    .chain(ap.path.iter().cloned().map(PathSegment::from)),
                            );
                            self.paths_dedup.insert((path,));
                            self.cap_path.insert(dest.clone(), (root, path));
                        }
                        _ => {}
                    }
                }
                StatementKind::Load {
                    dest,
                    source,
                    field,
                } => {
                    // The effective field path read is the captured path of the source base
                    // variable, then the source's own (offset) address arithmetic, then the
                    // loaded field, all rooted at the source's root variable.
                    let (root, base_path) = self
                        .cap_path
                        .get(&source.variable_ref)
                        .cloned()
                        .unwrap_or_else(|| (source.variable_ref.clone(), fx::Path::empty()));
                    let path = fx::Path::from_accesses(
                        base_path
                            .iter()
                            .cloned()
                            .chain(source.path.iter().cloned().map(PathSegment::from))
                            .chain(std::iter::once(PathSegment::Symbol(
                                field.symbol_ref().clone(),
                            ))),
                    );
                    self.paths_dedup.insert((path,));
                    self.cap_path.insert(dest.clone(), (root, path));
                }
                StatementKind::Store { dest, field, .. } => {
                    // The full written field path is the captured path of the destination base
                    // variable, then the dest's own (offset) address arithmetic, then the written
                    // field.
                    let base_path = self
                        .cap_path
                        .get(&dest.variable_ref)
                        .map(|(_, p)| *p)
                        .unwrap_or_default();
                    let path = fx::Path::from_accesses(
                        base_path
                            .iter()
                            .cloned()
                            .chain(dest.path.iter().cloned().map(PathSegment::from))
                            .chain(std::iter::once(PathSegment::Symbol(
                                field.symbol_ref().clone(),
                            ))),
                    );
                    self.paths_dedup.insert((path,));
                }
                _ => {}
            }
        }
        self.super_basic_block_data(function, block, data);
        self.cap_path.clear();
    }

    /// Generates formal parameters
    #[inline]
    fn visit_params(&mut self, params: &Params) {
        self.super_params(params);
        for (i, &p) in params.iter_enumerated() {
            let i = i.try_into().unwrap();
            self.facts
                .formal_param
                .push((self.function.unwrap(), i, p.into()));
        }
    }

    /// Generates assignments for locals and out-parameters
    #[inline]
    fn visit_statement(&mut self, statement: &Statement, location: Location) {
        use StatementKind::*;
        self.super_statement(statement, location);
        let statement_kind = &statement.kind;
        let site = {
            let insn_site_id = self.source_info.add_insn_site(self.function.unwrap());
            insn_site_id.try_into().unwrap()
        };
        self.source_info
            .add_instruction_span(site, statement.source_info.span_id);
        match statement_kind {
            Assign { dest, sources } => {
                for src in sources {
                    if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = src {
                        let dest = self.trans_variable_ref(dest);
                        let target = fx::Function(name.clone().into());
                        let target = self.source_info.sites.get_or_add_function(target);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(dest, fx::Path::empty()),
                            fx::CallTargetObject::FunctionId(target),
                        ));
                        self.funcptr_targets.insert(target);
                    }
                    if let Some(object) = call_target_object(src) {
                        let dest = self.trans_variable_ref(dest);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(dest, fx::Path::empty()),
                            object,
                        ));
                    }
                    let Some(src) = self.trans_exp(src) else {
                        continue;
                    };
                    let dest = self.trans_variable_ref(dest);
                    self.facts
                        .assign
                        .push((site, FlowVertex(dest, fx::Path::empty()), src));
                }
            }
            Phi {
                dest: out,
                operands,
            } => {
                let dst = FlowVertex(self.trans_variable_ref(out), fx::Path::empty());
                let mut seen_phi = BTreeSet::new();
                for (_, op) in operands {
                    let src = FlowVertex(self.trans_variable_ref(op), fx::Path::empty());
                    if seen_phi.insert(src.clone()) {
                        self.facts.assign.push((site, dst.clone(), src));
                    }
                }
            }
            ParamFlow { params, global } => {
                let mut seen_param = BTreeSet::new();
                for (i, op) in params.iter().enumerate() {
                    // assign current version of formal back to the formal itself so we can track
                    // data flow
                    let dst = FlowVariable::formal_index(i.try_into().unwrap());
                    let src = self.trans_variable_ref(op);
                    if seen_param.insert((dst, src)) {
                        let dst = FlowVertex(dst, fx::Path::empty());
                        let src = FlowVertex(src, fx::Path::empty());
                        self.facts.assign.push((site, dst, src));
                    }
                }
                // assign current version of global back to the auxparam global
                let dst = FlowVariable::formal_index(GLOBALS_INDEX.into());
                let src = self.trans_variable_ref(global);
                if seen_param.insert((dst, src)) {
                    let dst = FlowVertex(dst, fx::Path::empty());
                    let src = FlowVertex(src, fx::Path::empty());
                    self.facts.assign.push((site, dst, src));
                }
            }
            CallAssign { rets, args, style } => {
                let mut args = args.clone();
                match style {
                    CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(targets),
                    } => {
                        for target in targets {
                            let target = fx::Function(target.clone().into());
                            let target = self.source_info.sites.get_or_add_function(target);
                            self.facts.call.push((site, target));
                        }
                    }
                    CallStyle::JavaCall {
                        receiver,
                        cls,
                        simple_name,
                        descriptor,
                    } => {
                        let recv_var = self.trans_variable_ref(receiver);
                        // add receiver as actual arg 0
                        args.insert(0, Exp::Variable(receiver.clone()));
                        let resolvents = self.cha.java_resolvents(
                            cls.clone(),
                            simple_name.clone(),
                            descriptor.clone(),
                        );
                        match self.strategy {
                            CallResolutionStrategy::Cha => {
                                log::trace!(
                                    "java: CHA resolve {cls}.{simple_name}{descriptor} with {} targets",
                                    resolvents.len()
                                );
                                for target in resolvents {
                                    let target = fx::Function(target.into());
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                }
                            }
                            CallResolutionStrategy::Hi => {
                                self.facts.callee_info.push((
                                    site,
                                    FlowVertex(recv_var, fx::Path::empty()),
                                    fx::CallDispatchKey::Java(
                                        simple_name.clone(),
                                        descriptor.clone(),
                                    ),
                                ));
                                log::trace!(
                                    "java: HI resolve {cls}.{simple_name}{descriptor} (deferred)"
                                );
                            }
                            CallResolutionStrategy::Mixed => {
                                if resolvents.len() == 1 {
                                    let mut resolvents = resolvents;
                                    let target = resolvents.next().unwrap();
                                    log::trace!(
                                        "java: exact resolve {cls}.{simple_name}{descriptor} to {target}"
                                    );
                                    let target = fx::Function(target.into());
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                } else if resolvents.len() == 0 {
                                    log::trace!(
                                        "java: no resolvents {cls}.{simple_name}{descriptor}",
                                    );
                                } else {
                                    self.facts.callee_info.push((
                                        site,
                                        FlowVertex(recv_var, fx::Path::empty()),
                                        fx::CallDispatchKey::Java(
                                            simple_name.clone(),
                                            descriptor.clone(),
                                        ),
                                    ));
                                    log::trace!(
                                        "java: hybrid resolve {cls}.{simple_name}{descriptor} with {} targets",
                                        resolvents.len()
                                    );
                                }
                            }
                        }
                    }
                    CallStyle::LuaCall { receiver, method } => {
                        // A Lua receiver has no static type, so there is no declared class to key
                        // CHA on: the static resolvent set is every class method of this name
                        // across the recovered `__index` hierarchy, and the receiver's actual
                        // class comes from its allocation tag at analysis time. The receiver is
                        // already actual arg 0 (inserted by the frontend), so it is not
                        // re-inserted here.
                        let recv_var = self.trans_variable_ref(receiver);
                        let resolvents = self.cha.lua_resolvents_by_method(method);
                        // Deferred, context-sensitive resolution: the receiver's object facts
                        // (`call_target_assign`) join the Lua CHA `callee_resolvents` under this
                        // dispatch key at analysis time.
                        let deferred = (
                            site,
                            FlowVertex(recv_var, fx::Path::empty()),
                            fx::CallDispatchKey::Lua(method.clone()),
                        );
                        match self.strategy {
                            CallResolutionStrategy::Cha => {
                                log::trace!(
                                    "lua: CHA resolve {receiver}:{method} with {} targets",
                                    resolvents.len()
                                );
                                for target in &resolvents {
                                    let target = fx::Function(target.clone().into());
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                }
                            }
                            CallResolutionStrategy::Hi => {
                                self.facts.callee_info.push(deferred);
                                log::trace!("lua: HI resolve {receiver}:{method} (deferred)");
                            }
                            CallResolutionStrategy::Mixed => {
                                if resolvents.is_empty() {
                                    log::trace!("lua: no resolvents {receiver}:{method}");
                                } else {
                                    // Unlike the `JavaCall` arm above, an ambiguous Lua call keeps
                                    // its static edges instead of deferring to `callee_info`
                                    // alone. A `JavaCall` can defer safely because it carries a
                                    // declared receiver class, so hybrid inlining always has a
                                    // type to fall back on; a Lua receiver has none, and when no
                                    // dataflow reaches its allocation tag -- a module singleton,
                                    // a `self` handed in from opaque code, a computed metatable
                                    // -- the deferred path resolves to nothing at all and the
                                    // call site simply loses its callees. Measured on Prosody
                                    // 13.0.6 (examples/prosody): deferring alone drops
                                    // `--strategy mixed` from 2865 matched sinks / 806
                                    // tainted-path findings to 2145 / 263. So the sound CHA set
                                    // is always emitted, with `callee_info` on top to add the
                                    // context-sensitive resolution for the receivers whose class
                                    // the engine CAN reach.
                                    for target in &resolvents {
                                        let target = fx::Function(target.clone().into());
                                        let target =
                                            self.source_info.sites.get_or_add_function(target);
                                        self.facts.call.push((site, target));
                                    }
                                    // `callee_info` rides along even when the static set is a
                                    // singleton. The two are not redundant: a `call` edge is
                                    // resolved through the callee's summary, while the deferred
                                    // path instantiates it context-sensitively with a call
                                    // string, and each finds flows the other merges away.
                                    self.facts.callee_info.push(deferred);
                                    log::trace!(
                                        "lua: hybrid resolve {receiver}:{method} with {} target(s) + deferred",
                                        resolvents.len()
                                    );
                                }
                            }
                        }
                    }
                    CallStyle::FuncPtrCall { callee, .. } => {
                        let vertex = self.trans_access_path(callee);
                        self.facts
                            .callee_info
                            .push((site, vertex, fx::CallDispatchKey::C));
                    }
                    _ => log::warn!("unhandled call style: {style:?}"),
                }
                // pass parameters
                for (i, arg_exp) in args.iter().enumerate() {
                    let index: Result<i8, _> = i.try_into();
                    let Ok(idx_i8) = index else {
                        log::warn!("found > 127 parameters in function call; skipping rest");
                        break;
                    };
                    let formal_index = FormalIndex::new(idx_i8.into());

                    if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = arg_exp {
                        let target = fx::Function(name.clone().into());
                        let target = self.source_info.sites.get_or_add_function(target);
                        let call_arg_packed = fx::PackedCallArg::try_from_parts(
                            fx::InsnSiteId::try_from(site).unwrap().insn_id,
                            formal_index,
                        )
                        .unwrap();
                        let call_arg_var = FlowVariable::call_arg_packed(call_arg_packed);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            fx::CallTargetObject::FunctionId(target),
                        ));
                        self.funcptr_targets.insert(target);
                    }

                    if let Some(object) = call_target_object(arg_exp) {
                        let call_arg_packed = fx::PackedCallArg::try_from_parts(
                            fx::InsnSiteId::try_from(site).unwrap().insn_id,
                            formal_index,
                        )
                        .unwrap();
                        let call_arg_var = FlowVariable::call_arg_packed(call_arg_packed);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            object,
                        ));
                    }

                    let Some(arg) = self.trans_exp(arg_exp) else {
                        continue;
                    };
                    self.facts.actual_param.push((site, formal_index, arg))
                }
                // pass return values
                // This will be bad if there are more than 32K return values
                for (i, ret) in rets.iter().enumerate().map(|(i, r)| (i + 1, r)) {
                    let i: i16 = i.try_into().unwrap();
                    let i = -i;
                    let ret = self.trans_variable_ref(ret);
                    self.facts.actual_param.push((
                        site,
                        i.into(),
                        FlowVertex(ret, fx::Path::empty()),
                    ));
                }
                // pass globals
                self.facts.actual_param.push((
                    site,
                    GLOBALS_INDEX.into(),
                    FlowVertex(
                        FlowVariable::formal_index(GLOBALS_INDEX.into()),
                        fx::Path::empty(),
                    ),
                ));
            }
            Load {
                dest,
                source,
                field,
            } => {
                let dest = self.trans_variable_ref(dest);
                // Re-anchor a read through a load-chain temporary onto the root composed path it
                // addresses, the read-side mirror of the `Store` arm.
                let (root_var, base_path) = self
                    .cap_path
                    .get(&source.variable_ref)
                    .cloned()
                    .unwrap_or_else(|| (source.variable_ref.clone(), fx::Path::empty()));
                let source_var = self.trans_variable_ref(&root_var);
                // The read path is the captured chain path, then the source's own (offset) address
                // arithmetic, then the loaded field.
                let path = fx::Path::from_accesses(
                    base_path
                        .iter()
                        .cloned()
                        .chain(source.path.iter().cloned().map(PathSegment::from))
                        .chain(std::iter::once(PathSegment::Symbol(
                            field.symbol_ref().clone(),
                        ))),
                );
                self.paths_dedup.insert((path,));
                // dest <- root.<composed path>
                self.facts.assign.push((
                    site,
                    FlowVertex(dest, fx::Path::empty()),
                    FlowVertex(source_var, path),
                ));
            }
            Store { dest, field, value } => {
                // Re-anchor a store through a load-chain temporary onto the formal path it
                // addresses: `v.f2.nf1.y = rhs` lowers to `t1 = load v.f2; t2 = load t1.nf1;
                // store t2.y := rhs`, and the write must be recorded at `v.f2.nf1.y`, not at the
                // temporary `t2.y` (which no summary can name). The pre-pass captured `t2 ↦
                // (v, .f2.nf1)`; compose that root + captured path with this store's offsets and
                // field. When the dest base is not a load-chain temporary, the root is the base
                // itself and the captured path is empty (an ordinary field/offset store).
                let (root_var, base_path) = self
                    .cap_path
                    .get(&dest.variable_ref)
                    .cloned()
                    .unwrap_or_else(|| (dest.variable_ref.clone(), fx::Path::empty()));
                let dest_var = self.trans_variable_ref(&root_var);
                // The written path is the captured chain path, then the dest's (offset) address
                // arithmetic, then the field.
                let path = fx::Path::from_accesses(
                    base_path
                        .iter()
                        .cloned()
                        .chain(dest.path.iter().cloned().map(PathSegment::from))
                        .chain(std::iter::once(PathSegment::Symbol(
                            field.symbol_ref().clone(),
                        ))),
                );
                self.paths_dedup.insert((path,));
                // dest.field <- value
                let dest = FlowVertex(dest_var, path);
                // A function pointer / Java object stored INTO A FIELD (`o.op = id`).
                // This is the field-store form of the `Assign` arm's object-ref handling:
                // record the store at its field path so indirect-call resolution can follow
                // it to the call site. Must run before `value` is lowered, since trans_exp()
                // returns None for an ObjectRef and would otherwise drop the binding (F1).
                if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = value {
                    let target = fx::Function(name.clone().into());
                    let target = self.source_info.sites.get_or_add_function(target);
                    self.facts.call_target_assign.push((
                        site,
                        dest.clone(),
                        fx::CallTargetObject::FunctionId(target),
                    ));
                    self.funcptr_targets.insert(target);
                }
                if let Some(object) = call_target_object(value) {
                    self.facts
                        .call_target_assign
                        .push((site, dest.clone(), object));
                }
                if let Some(value) = self.trans_exp(value) {
                    self.facts.assign.push((site, dest, value));
                }
            }
            Nop => (),
        }
    }

    // Generates assignments to aux formals from return instructions
    #[inline]
    fn visit_terminator_kind(&mut self, terminator: &TerminatorKind, location: Location) {
        self.super_terminator_kind(terminator, location);
        let site = {
            let insn_site_id = self.source_info.add_insn_site(self.function.unwrap());
            insn_site_id.try_into().unwrap()
        };
        if let TerminatorKind::Return { args } = terminator {
            // assigns for return values. This will be bad if there are more than 32K return values
            for (i, arg) in args.iter().enumerate().map(|(i, arg)| (i + 1, arg)) {
                let i: i16 = i.try_into().unwrap();
                let i = -i;
                let Some(src) = self.trans_exp(arg) else {
                    continue;
                };
                let dv = FlowVariable::formal_index(i.into());
                let dpath = fx::Path::empty();
                self.facts.assign.push((site, FlowVertex(dv, dpath), src));
            }
        }
    }

    // Generates access paths
    #[inline]
    fn visit_field_accesses(&mut self, fields: &FieldAccesses) {
        self.super_field_accesses(fields);
        self.paths_dedup.insert((fields.into(),));
        if let Some(FieldAccess::Offset(offset)) = fields.first() {
            // Insert just the first (offset) field to make sure we catch globals.
            let first_field = FieldAccesses::with_offset(offset.0);
            self.paths_dedup.insert(((&first_field).into(),));
        }
    }
}

impl CodegenVisitor<'_> {
    /// Translate an expression into a flow vertex. If the expression is a constant, None is
    /// returned. Otherwise the vertex is returned.
    #[inline]
    fn trans_exp(&mut self, exp: &Exp) -> Option<FlowVertex> {
        match exp {
            Exp::Variable(v) => Some(FlowVertex(self.trans_variable_ref(v), fx::Path::empty())),
            // An address expression `x.[k]` flows structurally from x's offset field.
            Exp::AccessPath(ap) => Some(self.trans_access_path(ap)),
            Exp::ObjectRef(_) => None,
            _ => None,
        }
    }

    #[inline]
    fn trans_access_path(&mut self, ap: &AccessPath) -> FlowVertex {
        let v = self.trans_variable_ref(&ap.variable_ref);
        let fields = &ap.path;
        FlowVertex(v, fields.into())
    }

    #[inline]
    fn trans_variable_ref(&mut self, v: &VariableRef) -> FlowVariable {
        match (v.variable.as_ref(), v.version) {
            // The one global heap maps to the globals index
            (Variable::GlobalHeap, None) => FlowVariable::formal_index(GLOBALS_INDEX.into()),
            // A versioned global heap is a local variable
            (Variable::GlobalHeap, Some(version)) => {
                FlowVariable::local(Str::from(format!("$globals_{}", version)))
            }
            _ => v.try_into().unwrap(),
        }
    }
}

/// Which frontend's call-resolution scheme [`ClassHierarchyAnalysis::resolvents`] is keyed for.
/// The hierarchy computation itself ([`run_cha`]) is language-neutral; this only decides the
/// `(CallTargetObject, CallDispatchKey)` pair the resolvents are emitted under, so that a Lua
/// import and a JVM import sharing one fact base cannot collide in that key space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ChaLanguage {
    #[default]
    Java,
    Lua,
}

#[derive(Debug, Default)]
struct ClassHierarchyAnalysis {
    language: ChaLanguage,
    /// `(class, method simple name, descriptor) -> targets`. The descriptor is a fixed empty
    /// sentinel for [`ChaLanguage::Lua`], which has no overloading.
    resolvents: BTreeMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>>,
}

impl ClassHierarchyAnalysis {
    fn new(vmt: &VirtualMethodTable, instantiated_classes: BTreeSet<Symbol>) -> Self {
        match vmt {
            VirtualMethodTable::Java {
                methods, hierarchy, ..
            } => {
                let method_implemented = methods
                    .iter()
                    .cloned()
                    .map(|(a, b, c, d)| (a.into(), b.into(), c.into(), d.into()))
                    .collect();
                let mut direct_superclass: Vec<(Symbol, Symbol)> = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| {
                        sups.into_iter()
                            .map(|sup| (sup.clone().into(), sub.clone().into()))
                    })
                    .collect();
                // Sort for determinism
                direct_superclass.sort_unstable();
                let interface_type = Default::default();
                let super_interface = Default::default();
                let instantiated_classes_vec =
                    instantiated_classes.into_iter().map(|s| (s,)).collect();
                let resolvents = run_cha(
                    method_implemented,
                    direct_superclass,
                    interface_type,
                    super_interface,
                    instantiated_classes_vec,
                );
                Self {
                    language: ChaLanguage::Java,
                    resolvents,
                }
            }
            // Lua mirrors the Java arm: a Lua method is a `method_implemented` with a fixed
            // empty descriptor sentinel, and each `__index` parent is a `direct_superclass`.
            // The shared `run_cha` then computes the `__index`-chain resolvents unchanged.
            VirtualMethodTable::Lua {
                methods, hierarchy, ..
            } => {
                let empty_desc = Symbol::from("");
                let method_implemented = methods
                    .iter()
                    .cloned()
                    .map(|(cls, name, id)| (cls, name, empty_desc.clone(), id))
                    .collect();
                let mut direct_superclass: Vec<(Symbol, Symbol)> = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| sups.iter().map(|sup| (sup.clone(), sub.clone())))
                    .collect();
                // Sort for determinism
                direct_superclass.sort_unstable();
                let instantiated_classes_vec =
                    instantiated_classes.into_iter().map(|s| (s,)).collect();
                let resolvents = run_cha(
                    method_implemented,
                    direct_superclass,
                    Default::default(),
                    Default::default(),
                    instantiated_classes_vec,
                );
                Self {
                    language: ChaLanguage::Lua,
                    resolvents,
                }
            }
            _ => {
                log::warn!("CHA: unsupported virtual method table");
                Self::default()
            }
        }
    }

    fn java_resolvents(
        &self,
        cls: Symbol,
        name: Symbol,
        descriptor: Symbol,
    ) -> impl ExactSizeIterator<Item = Symbol> + '_ {
        self.resolvents
            .get(&(cls, name, descriptor))
            .map(|syms| syms.as_slice())
            .unwrap_or(&[])
            .iter()
            .cloned()
    }

    /// The deduplicated set of CHA targets for a Lua method name, unioned across every class in the
    /// recovered hierarchy. This is the *static* resolvent set for a Lua call site: unlike Java
    /// there is no declared receiver class to key on, so the name alone is all a purely static
    /// resolution has. (The `""` descriptor sentinel matches the Lua CHA arm.)
    ///
    /// A uniquely-named method — the common case — yields a singleton, which is an exact call
    /// edge. When the name is shared across unrelated classes the union is sound but imprecise,
    /// which is why [`CallResolutionStrategy::Mixed`] defers to `callee_info` instead of
    /// emitting it (see the [`CallStyle::LuaCall`] codegen arm).
    fn lua_resolvents_by_method(&self, method: &Symbol) -> BTreeSet<Symbol> {
        self.resolvents
            .iter()
            .filter(|((_cls, name, _desc), _)| name == method)
            .flat_map(|(_, targets)| targets.iter().cloned())
            .collect()
    }
}

/// Emits the `callee_resolvents` rows for a completed CHA, under the language's own
/// `(CallTargetObject, CallDispatchKey)` pair: `(Symbol(cls), Java(name, desc))` for JVM/Dex,
/// `(LuaClass(cls), Lua(name))` for Lua. The `""` descriptor the Lua CHA arm feeds `run_cha`
/// stays an implementation detail of that arm and never reaches a fact.
fn emit_callee_resolvents(
    cha: &ClassHierarchyAnalysis,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    for ((cls, name, desc), targets) in &cha.resolvents {
        let (object, key) = match cha.language {
            ChaLanguage::Java => (
                fx::CallTargetObject::Symbol(cls.clone()),
                fx::CallDispatchKey::Java(name.clone(), desc.clone()),
            ),
            ChaLanguage::Lua => (
                fx::CallTargetObject::LuaClass(cls.clone()),
                fx::CallDispatchKey::Lua(name.clone()),
            ),
        };
        for target in targets {
            let func_id = source_info
                .sites
                .get_or_add_function(fx::Function(target.clone().into()));
            facts
                .callee_resolvents
                .push((object.clone(), key.clone(), func_id));
        }
    }
}

fn run_cha(
    method_implemented: Vec<(Symbol, Symbol, Symbol, Symbol)>,
    direct_superclass: Vec<(Symbol, Symbol)>,
    interface_type: Vec<(Symbol,)>,
    super_interface: Vec<(Symbol, Symbol)>,
    instantiated_classes: Vec<(Symbol,)>,
) -> BTreeMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>> {
    let prog = ascent::ascent_run! {
        // input relations
        relation method_implemented(Symbol, Symbol, Symbol, Symbol) = method_implemented;
        relation interface_type(Symbol) = interface_type;
        relation super_interface(Symbol, Symbol) = super_interface;
        // sup, sub
        relation direct_superclass(Symbol, Symbol) = direct_superclass;
        relation instantiated_class(Symbol) = instantiated_classes;

        // internal relations
        relation cha_direct_subtype(Symbol, Symbol);
        relation cha_subtype(Symbol, Symbol);
        relation cha_subtype_reflexive(Symbol, Symbol);
        // maps triple to methods (inherited)
        relation cha_super_method(Symbol, Symbol, Symbol, Symbol);
        // output: static type resolves to possible methods
        relation cha_resolve(Symbol, Symbol, Symbol, Symbol);

        cha_direct_subtype(sub, sup) <-- direct_superclass(sup, sub);
        cha_direct_subtype(cls, iface) <-- super_interface(iface, cls), !interface_type(cls);
        cha_subtype(sub, sup) <-- cha_direct_subtype(sub, sup);
        cha_subtype(sub, sup) <-- cha_subtype(sub, mid), cha_direct_subtype(mid, sup);

        relation class_or_interface(Symbol);
        class_or_interface(c) <-- method_implemented(c, _, _, _);
        class_or_interface(c) <-- direct_superclass(c, _);
        class_or_interface(c) <-- direct_superclass(_, c);
        class_or_interface(c) <-- interface_type(c);
        class_or_interface(c) <-- super_interface(c, _);
        class_or_interface(c) <-- super_interface(_, c);
        class_or_interface(c) <-- instantiated_class(c);

        cha_subtype_reflexive(c, c) <-- class_or_interface(c);
        cha_subtype_reflexive(sub, sup) <-- cha_subtype(sub, sup);

        cha_super_method(c, m, d, id) <-- method_implemented(c, m, d, id);
        cha_super_method(c, m, d, id) <--
            cha_super_method(c2, m, d, id),
            cha_direct_subtype(c, c2),
            !method_implemented(c, m, d, _);

        cha_resolve(sup, m, d, id) <--
            cha_super_method(sub, m, d, id),
            cha_subtype_reflexive(sub, sup);
            // RTA rule: Only resolve if there is an instantiated subtype
            //instantiated_class(sub);
    };
    let mut rows: Vec<_> = prog.cha_resolve.into_iter().collect();
    // Sort for determinism
    rows.sort_unstable();
    let mut result: BTreeMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>> = BTreeMap::new();
    for (c, n, d, id) in rows {
        log::trace!("Adding entry: {c}, {n}, {d} -> {id}");
        result.entry((c, n, d)).or_default().push(id);
    }
    result
}
