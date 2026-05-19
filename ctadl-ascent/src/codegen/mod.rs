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
use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;

use internment::ArcIntern;
use smallvec::SmallVec;

use crate::facts as fx;
use crate::facts::{FlowVariable, FlowVertex, FormalIndex};
use crate::index_engine::{IndexFacts, source_info::IndexSourceInfo};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::{call::VirtualMethodTable, visit::Visitor, *};

#[cfg(test)]
mod tests;

pub mod flowy;
pub mod models;

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
    let mut instantiated_classes = HashSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    for f in program_info.program.functions.iter() {
        finder.visit_function_data(FunctionIdx::new(0), f);
    }

    let cha = ClassHierarchyAnalysis::new(&program_info.vmt, instantiated_classes);
    for ((cls, name, desc), targets) in &cha.java_resolvents {
        for target in targets {
            let func_id = source_info
                .sites
                .get_or_add_function(fx::Function(target.clone()));
            facts
                .java_resolvents
                .push((cls.clone(), name.clone(), desc.clone(), func_id));
        }
    }
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
    let mut instantiated_classes = HashSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    finder.visit_function_data(FunctionIdx::new(0), function_data);

    let cha = ClassHierarchyAnalysis::new(&VirtualMethodTable::Unknown, instantiated_classes);
    for ((cls, name, desc), targets) in &cha.java_resolvents {
        for target in targets {
            let func_id = source_info
                .sites
                .get_or_add_function(fx::Function(target.clone()));
            facts
                .java_resolvents
                .push((cls.clone(), name.clone(), desc.clone(), func_id));
        }
    }
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
    match v {
        FlowVariable::Formal(idx) => {
            // *this* is how you get it to deref?
            let idx: &i16 = idx;
            *idx == GLOBALS_INDEX
        }
        // This has to be kept in sync with the name given to globals in the CodegenVisitor
        FlowVariable::Local(name) => name.starts_with("$globals_"),
        _ => false,
    }
}

struct InstantiationFinder<'a> {
    instantiated_classes: &'a mut HashSet<Symbol>,
}

impl Visitor for InstantiationFinder<'_> {
    fn visit_exp(&mut self, exp: &Exp) {
        if let Exp::ObjectRef(CallObject::JavaObject(cls)) = exp {
            self.instantiated_classes.insert(cls.0.clone());
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
    paths_dedup: HashSet<(fx::Path,)>,
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
        }
    }

    /// Gens the dedup'd paths to the facts
    fn finish(&mut self) {
        let mut paths = std::mem::take(&mut self.paths_dedup);
        self.facts.paths.extend(paths.drain());
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
        self.function = Some(self.source_info.sites.get_or_add_function(func));
        // Gens global param
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::Formal(GLOBALS_INDEX.into()),
            fx::FormalType::ByRef,
        ));
        // Gens return parameter
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::Formal(RETURN_INDEX.into()),
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
        let mut cap_path: HashMap<VariableRef, fx::Path> = HashMap::new();
        for statement in &data.statements {
            if let StatementKind::Assign { dest, sources } = &statement.kind {
                for src in sources {
                    if let Exp::AccessPath(ap) = src
                        && !ap.path.is_empty()
                    {
                        let mut path = cap_path.get(&ap.variable_ref).cloned().unwrap_or_default();
                        path.extend_merging(ap.path.iter().cloned());
                        self.paths_dedup.insert((path.clone(),));
                        cap_path.insert(dest.clone(), path);
                    }
                }
            }
        }
        self.super_basic_block_data(function, block, data);
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
                        let target = fx::Function(name.clone());
                        let target = self.source_info.sites.get_or_add_function(target);
                        self.facts.func_ptr_assign.push((
                            site,
                            FlowVertex(dest.clone(), fx::Path::empty()),
                            target,
                        ));
                    }
                    if let Exp::ObjectRef(CallObject::JavaObject(cls)) = src {
                        let dest = self.trans_variable_ref(dest);
                        self.facts.java_obj_assign.push((
                            site,
                            FlowVertex(dest.clone(), fx::Path::empty()),
                            cls.0.clone(),
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
            Load {
                dest,
                source,
                field,
            } => {
                let dest = self.trans_variable_ref(dest);
                let source = self.trans_variable_ref(source);
                let path = fx::Path::from(field);
                self.paths_dedup.insert((path.clone(),));
                self.facts.assign.push((
                    site,
                    FlowVertex(dest, fx::Path::empty()),
                    FlowVertex(source, path),
                ));
            }
            Store {
                dest: base,
                field,
                value,
            } => {
                let base = self.trans_variable_ref(base);
                let value = self.trans_variable_ref(value);
                let path = fx::Path::from(field);
                self.paths_dedup.insert((path.clone(),));
                self.facts.assign.push((
                    site,
                    FlowVertex(base, path),
                    FlowVertex(value, fx::Path::empty()),
                ));
            }
            Phi {
                dest: out,
                operands,
            } => {
                let dst = FlowVertex(self.trans_variable_ref(out).clone(), fx::Path::empty());
                for (_, op) in operands {
                    let src = FlowVertex(self.trans_variable_ref(op).clone(), fx::Path::empty());
                    //log::trace!("{p}: {dv:#?} {sv:#?}");
                    self.facts.assign.push((site, dst.clone(), src));
                }
            }
            ParamFlow { params, global } => {
                for (i, op) in params.iter().enumerate() {
                    // assign current version of formal back to the formal itself so we can track
                    // data flow
                    let dst = FlowVertex(
                        FlowVariable::Formal(i.try_into().unwrap()),
                        fx::Path::empty(),
                    );
                    let src = FlowVertex(self.trans_variable_ref(op), fx::Path::empty());
                    self.facts.assign.push((site, dst, src));
                }
                // assign current version of global back to the auxparam global
                let dst = FlowVertex(
                    FlowVariable::Formal(GLOBALS_INDEX.into()),
                    fx::Path::empty(),
                );
                let src = FlowVertex(self.trans_variable_ref(global), fx::Path::empty());
                self.facts.assign.push((site, dst, src));
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
                        args.insert(
                            0,
                            Exp::new_access_path(AccessPath::without_fields(receiver.clone())),
                        );
                        let resolvents = self.cha.java_resolvents(
                            cls.clone(),
                            simple_name.clone(),
                            descriptor.clone(),
                        );
                        match self.strategy {
                            CallResolutionStrategy::Cha => {
                                log::trace!(
                                    "java: CHA resolve {cls}.{simple_name}({descriptor}) with {} targets",
                                    resolvents.len()
                                );
                                for target in resolvents {
                                    let target = fx::Function(target);
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                }
                            }
                            CallResolutionStrategy::Hi => {
                                self.facts.java_call.push((
                                    site,
                                    FlowVertex(recv_var.clone(), fx::Path::empty()),
                                    simple_name.clone(),
                                    descriptor.clone(),
                                ));
                                log::trace!(
                                    "java: HI resolve {cls}.{simple_name}({descriptor}) (deferred)"
                                );
                            }
                            CallResolutionStrategy::Mixed => {
                                if resolvents.len() == 1 {
                                    let mut resolvents = resolvents;
                                    let target = resolvents.next().unwrap();
                                    log::trace!(
                                        "java: exact resolve {cls}.{simple_name}({descriptor}) to {target}"
                                    );
                                    let target = fx::Function(target);
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                } else if resolvents.len() == 0 {
                                    log::trace!(
                                        "java: no resolvents {cls}.{simple_name}({descriptor})",
                                    );
                                } else {
                                    self.facts.java_call.push((
                                        site,
                                        FlowVertex(recv_var.clone(), fx::Path::empty()),
                                        simple_name.clone(),
                                        descriptor.clone(),
                                    ));
                                    log::trace!(
                                        "java: hybrid resolve {cls}.{simple_name}({descriptor}) with {} targets",
                                        resolvents.len()
                                    );
                                }
                            }
                        }
                    }
                    CallStyle::FuncPtrCall { callee, .. } => {
                        let vertex = self.trans_access_path(callee);
                        self.facts.indirect_call.push((site, vertex));
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
                        let target = fx::Function(name.clone());
                        let target = self.source_info.sites.get_or_add_function(target);
                        let call_arg_var = FlowVariable::CallArg {
                            id: site,
                            formal: formal_index,
                        };
                        self.facts.func_ptr_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            target,
                        ));
                    }

                    if let Exp::ObjectRef(CallObject::JavaObject(cls)) = arg_exp {
                        let call_arg_var = FlowVariable::CallArg {
                            id: site,
                            formal: formal_index,
                        };
                        self.facts.java_obj_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            cls.0.clone(),
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
                        FlowVariable::Formal(GLOBALS_INDEX.into()),
                        fx::Path::empty(),
                    ),
                ));
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
                let dv = FlowVariable::Formal(i.into());
                let dpath = fx::Path::empty();
                self.facts
                    .assign
                    .push((site, FlowVertex(dv.clone(), dpath), src));
            }
        }
    }

    // Generates access paths
    #[inline]
    fn visit_field_accesses(&mut self, fields: &FieldAccesses) {
        self.super_field_accesses(fields);
        self.paths_dedup.insert((fields.into(),));
        if !fields.is_empty() {
            // Handle the first field access, which can be either Symbol or Offset
            let first_field_access = &fields[0];
            let first_field = match first_field_access {
                FieldAccess::Symbol(symbol) => {
                    FieldAccesses::from_iter(std::iter::once(symbol.as_ref() as &str))
                }
                FieldAccess::Offset(offset) => FieldAccesses::with_offset(offset.0),
            };
            // Insert just the first field to make sure we catch globals
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
            Exp::AccessPath(ap) => Some(self.trans_access_path(ap)),
            Exp::ObjectRef(_) => None,
            _ => None,
        }
    }

    #[inline]
    fn trans_access_path(&mut self, ap: &AccessPath) -> FlowVertex {
        let v = self.trans_variable_ref(&ap.variable_ref);
        let fields = &ap.path;
        FlowVertex(v.clone(), fields.into())
    }

    #[inline]
    fn trans_variable_ref(&mut self, v: &VariableRef) -> FlowVariable {
        match (v.variable.as_ref(), v.version) {
            // The one global heap maps to the globals index
            (Variable::GlobalHeap, None) => FlowVariable::Formal(GLOBALS_INDEX.into()),
            // A versioned global heap is a local variable
            (Variable::GlobalHeap, Some(version)) => {
                FlowVariable::Local(ArcIntern::from(format!("$globals_{}", version)))
            }
            _ => v.try_into().unwrap(),
        }
    }
}

#[derive(Debug, Default)]
struct ClassHierarchyAnalysis {
    java_resolvents: HashMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>>,
}

impl ClassHierarchyAnalysis {
    fn new(vmt: &VirtualMethodTable, instantiated_classes: HashSet<Symbol>) -> Self {
        match vmt {
            VirtualMethodTable::Java { methods, hierarchy } => {
                let method_implemented = methods
                    .iter()
                    .cloned()
                    .map(|(a, b, c, d)| (a.into(), b.into(), c.into(), d.into()))
                    .collect();
                let direct_superclass = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| {
                        sups.into_iter()
                            .map(|sup| (sup.clone().into(), sub.clone().into()))
                    })
                    .collect();
                let interface_type = Default::default();
                let super_interface = Default::default();
                let instantiated_classes_vec =
                    instantiated_classes.into_iter().map(|s| (s,)).collect();
                let java_resolvents = run_cha(
                    method_implemented,
                    direct_superclass,
                    interface_type,
                    super_interface,
                    instantiated_classes_vec,
                );
                Self { java_resolvents }
            }
            _ => {
                log::warn!("Unsupported virtual method table");
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
        self.java_resolvents
            .get(&(cls, name, descriptor))
            .map(|syms| syms.as_slice())
            .unwrap_or(&[])
            .iter()
            .cloned()
    }
}

fn run_cha(
    method_implemented: Vec<(Symbol, Symbol, Symbol, Symbol)>,
    direct_superclass: Vec<(Symbol, Symbol)>,
    interface_type: Vec<(Symbol,)>,
    super_interface: Vec<(Symbol, Symbol)>,
    instantiated_classes: Vec<(Symbol,)>,
) -> HashMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>> {
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
    let mut result: HashMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>> = HashMap::new();
    for (c, n, d, id) in prog.cha_resolve.into_iter() {
        result.entry((c, n, d)).or_default().push(id);
    }
    result
}
