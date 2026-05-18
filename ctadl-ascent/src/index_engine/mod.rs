/*!
Computes a compositional, global data flow graph.

# Input

[`IndexFacts`] is the input format. It is a flat, relational format. Generating this input format
requires mapping function names and instructions into instruction sites. An instruction site is a
globally unique ID for each instruction. It is composed of a function ID and an instruction ID
packade into a 64-bit integer. Instruction sites may also be associated with source info.
Generating facts is typically done with a mutable reference to [`IndexFacts`]. Generating IDs is
done with the help of the [`source_info::IndexSourceInfo`] API. The source info may be persisted,
but it is not used during indexing (only for fact generation):

```
use ctadl_ascent::index_engine::{IndexFacts, taint_index};
use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
use ctadl_ascent::facts::Function;
let mut facts = IndexFacts::default();
let mut source_info = IndexSourceInfo::default();
// ... generate facts
let name_id = source_info.sites.get_or_add_function(Function("name".into()));
// ... save the source_info
let result = taint_index(facts);
```

# Data Flow Analysis

The code handles direct data flows and some aliased data flows. The aliased flows look like,
e.g.:

```text
this = 0;
this.f = 1;
```
is a write to an alias of a formal. The aliasing rule turns this into a summary where 1 flows
to 0.f.

*/

use std::path;

use ascent::ascent_run;
use derive_builder::Builder;
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;

use crate::error::Error;
use crate::facts::{
    CallString, FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, Heap, InsnId,
    InsnSiteId, PackedInsnSiteId, Path, isout, match_prefix,
};
use ctadl_ir::Symbol;

pub mod graphviz;
pub mod source_info;

/// An assignment statement. The order is destination vertex then source vertex.
pub type AssignFlow = (PackedInsnSiteId, FlowVertex, FlowVertex);
pub type FunctionSummary = (FunctionId, FormalIndex, Path, FormalIndex, Path);

/// Program facts for indexing.
#[derive(Default, Debug, Clone, Builder)]
pub struct IndexFacts {
    /// A formal parameter is a parameter in the function's signature.
    #[builder(default)]
    pub formal_param: Vec<(FunctionId, FlowVariable, FormalType)>,
    /// An actual parameter is the value passed for an argument at a call site
    #[builder(default)]
    pub actual_param: Vec<(PackedInsnSiteId, FormalIndex, FlowVertex)>,
    /// Stores the call graph as a relation of call site to target function
    #[builder(default)]
    pub call: Vec<(PackedInsnSiteId, FunctionId)>,
    /// Assignments from source to destination vertices
    #[builder(default)]
    pub assign: Vec<AssignFlow>,
    #[builder(default)]
    pub func_ptr_assign: Vec<(PackedInsnSiteId, FlowVertex, FunctionId)>,
    #[builder(default)]
    pub java_obj_assign: Vec<(PackedInsnSiteId, FlowVertex, Symbol)>,
    #[builder(default)]
    pub java_call: Vec<(PackedInsnSiteId, FlowVertex, Symbol, Symbol)>,
    #[builder(default)]
    pub java_resolvents: Vec<(Symbol, Symbol, Symbol, FunctionId)>,
    #[builder(default)]
    pub indirect_call: Vec<(PackedInsnSiteId, FlowVertex)>,
    #[builder(default)]
    pub summary: Vec<FunctionSummary>,
    #[builder(default)]
    pub paths: Vec<(Path,)>,
}

impl IndexFacts {
    /// Saves the `formal_param`, `actual_param`, and `call` members. The others aren't saved
    /// because they are computed as part of an [`IndexResult`].
    pub fn try_save<P: AsRef<path::Path>>(self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        formal_param::try_save(
            &dir,
            self.formal_param.into_iter().map(|(func_id, var, ty)| {
                let FlowVariable::Formal(i) = var else {
                    panic!("formal_param variable is not a formal")
                };
                (func_id, i, ty)
            }),
        )?;
        actual_param::try_save(
            &dir,
            self.actual_param
                .into_iter()
                .map(|(site_id, formal_index, vertex)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, formal_index, variable, path)
                }),
        )?;
        call::try_save(
            &dir,
            self.call.into_iter().map(|(site_id, target)| {
                let InsnSiteId { func_id, insn_id } =
                    InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                (func_id, insn_id, target)
            }),
        )?;
        java_obj_assign::try_save(
            &dir,
            self.java_obj_assign
                .into_iter()
                .map(|(site_id, vertex, class_name)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, variable, path, class_name)
                }),
        )?;
        java_call::try_save(
            &dir,
            self.java_call
                .into_iter()
                .map(|(site_id, vertex, name, desc)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, variable, path, name, desc)
                }),
        )?;
        java_resolvents::try_save(&dir, self.java_resolvents)?;
        Ok(())
    }

    /// Loads `formal_param, `actual_param`, and `call`, the members saved by
    /// [`IndexFacts::try_save`].
    pub fn try_load<P: AsRef<path::Path>>(dir: P) -> Result<Self, Error> {
        use crate::facts::schema::*;
        let mut builder = IndexFactsBuilder::default();
        builder
            .formal_param(
                formal_param::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, i, ty)| {
                        let var = FlowVariable::Formal(i);
                        (func_id, var, ty)
                    })
                    .collect(),
            )
            .actual_param(
                actual_param::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, formal_index, variable, path)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (
                            site_id.try_into().expect("error packing site_id"),
                            formal_index,
                            FlowVertex(variable, path),
                        )
                    })
                    .collect(),
            )
            .call(
                call::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, target)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (site_id.try_into().expect("error packing site_id"), target)
                    })
                    .collect(),
            )
            .java_obj_assign(
                java_obj_assign::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, variable, path, class_name)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (
                            site_id.try_into().expect("error packing site_id"),
                            FlowVertex(variable, path),
                            class_name,
                        )
                    })
                    .collect(),
            )
            .java_call(
                java_call::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, variable, path, name, desc)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (
                            site_id.try_into().expect("error packing site_id"),
                            FlowVertex(variable, path),
                            name,
                            desc,
                        )
                    })
                    .collect(),
            )
            .java_resolvents(java_resolvents::try_load(&dir)?);
        Ok(builder.build().unwrap())
    }

    /// Computes the number of parameters for each function found
    pub fn compute_num_params(&self) -> HashMap<FunctionId, i16> {
        let mut func_num_params: HashMap<FunctionId, i16> = HashMap::new();
        for (func, var, _) in self.formal_param.iter() {
            let i: i16 = match var {
                FlowVariable::Formal(i) => **i,
                _ => {
                    //log::warn!("not a good formal: {:?}", var);
                    continue;
                }
            };
            func_num_params
                .entry(*func)
                .and_modify(|m| *m = (*m).max(i + 1))
                .or_insert(i + 1);
        }
        func_num_params
    }
}

#[derive(Debug, Clone, Builder, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IndexConfig {
    pub alias_rule: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig { alias_rule: true }
    }
}

#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Summary goes from formal parameter index to formal ret index.
    pub summary: Vec<FunctionSummary>,
    pub assign_like: Vec<(FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path)>,
    pub java_obj_assign_like: Vec<(FunctionId, InsnId, FlowVariable, Path, Symbol)>,
    pub paths: Vec<(Path,)>,

    // --- Pointer Analysis Results ---
    pub vtx_points_to: Vec<(FunctionId, FlowVariable, Path, Heap)>,
    pub fld_points_to: Vec<(FunctionId, Heap, Path, Heap)>,
}

impl IndexResult {
    pub fn try_save<P: AsRef<path::Path>>(self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        summary::try_save(&dir, self.summary)?;
        assign::try_save(&dir, self.assign_like)?;
        paths::try_save(&dir, self.paths)?;
        Ok(())
    }

    pub fn try_load<P: AsRef<path::Path>>(dir: P) -> Result<Self, Error> {
        use crate::facts::schema::*;
        let summary = summary::try_load(&dir)?;
        let assign_like = assign::try_load(&dir)?;
        let paths = paths::try_load(&dir)?;
        Ok(IndexResult {
            summary,
            assign_like,
            java_obj_assign_like: Vec::new(),
            paths,
            vtx_points_to: Vec::new(),
            fld_points_to: Vec::new(),
        })
    }
}

impl std::fmt::Display for IndexResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Summary:")?;
        for s in &self.summary {
            writeln!(f, "{:?}", s)?;
        }
        writeln!(f, "\nAssign-like:")?;
        for (func_id, insn_id, dest_var, dest_path, src_var, src_path) in &self.assign_like {
            let dest_str = {
                let var_str = match dest_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", dest_var),
                };
                format!("{}{}", var_str, dest_path.to_dot_string())
            };
            let src_str = {
                let var_str = match src_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", src_var),
                };
                format!("{}{}", var_str, src_path.to_dot_string())
            };

            writeln!(
                f,
                "{:?}:{:?}: {} = {}",
                func_id.id, insn_id.id, dest_str, src_str
            )?;
        }
        writeln!(f, "\nPaths:")?;
        for (p,) in &self.paths {
            writeln!(f, "{}", p)?;
        }
        Ok(())
    }
}

struct PointerAnalysisRelations<'a> {
    vtx_points_to: &'a [(FunctionId, FlowVariable, Path, Heap)],
    fld_points_to: &'a [(FunctionId, Heap, Path, Heap)],
}

impl<'a> std::fmt::Display for PointerAnalysisRelations<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Vtx Points-To ({}):", self.vtx_points_to.len())?;
        for (func_id, var, path, heap) in self.vtx_points_to {
            let var_str = match var {
                FlowVariable::Local(name) => name.to_string(),
                _ => format!("{}", var),
            };
            writeln!(
                f,
                "  {}: {}{} -> {:?}",
                func_id.id,
                var_str,
                path.to_dot_string(),
                heap
            )?;
        }

        writeln!(f, "\nFld Points-To ({}):", self.fld_points_to.len())?;
        for (func_id, base_heap, fld_path, heap) in self.fld_points_to {
            writeln!(
                f,
                "  {}: {:?}{} -> {:?}",
                func_id.id,
                base_heap,
                fld_path.to_dot_string(),
                heap
            )?;
        }

        let mut heaps = std::collections::BTreeSet::new();
        for (_, _, _, heap) in self.vtx_points_to {
            heaps.insert(heap);
        }
        for (_, base_heap, _, heap) in self.fld_points_to {
            heaps.insert(base_heap);
            heaps.insert(heap);
        }

        writeln!(f, "\nHeaps ({}):", heaps.len())?;
        for heap in heaps {
            writeln!(f, "  {:?}", heap)?;
        }

        Ok(())
    }
}

struct HybridInliningRelations<'a> {
    critical_summary: &'a [(FunctionId, FormalIndex, Path, PackedInsnSiteId)],
    resolvent: &'a [(
        CallString,
        FunctionId,
        FormalIndex,
        Path,
        PackedInsnSiteId,
        FunctionId,
    )],
    func_ptr_assign_like: &'a [(FunctionId, InsnId, FlowVariable, Path, FunctionId)],
    context_assign: &'a [(
        CallString,
        FunctionId,
        InsnId,
        FlowVariable,
        Path,
        FlowVariable,
        Path,
    )],
    context_locals: &'a [(
        CallString,
        FunctionId,
        FlowVariable,
        Path,
        FormalIndex,
        Path,
    )],
    context_summary: &'a [(CallString, FunctionId, FormalIndex, Path, FormalIndex, Path)],
}

impl<'a> std::fmt::Display for HybridInliningRelations<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Critical Summary ({}):", self.critical_summary.len())?;
        for (func_id, formal_index, path, site_id) in self.critical_summary {
            let InsnSiteId {
                func_id: site_func_id,
                insn_id: site_insn_id,
            } = InsnSiteId::unpack_from_slice(&**site_id).unwrap();
            writeln!(
                f,
                "  {}: arg{} {} -> site {}:{}",
                func_id.id,
                formal_index,
                path.to_dot_string(),
                site_func_id.id,
                site_insn_id.id
            )?;
        }

        writeln!(f, "\nResolvent ({}):", self.resolvent.len())?;
        for (cs, func_id, formal_index, path, site_id, tgt) in self.resolvent {
            let InsnSiteId {
                func_id: site_func_id,
                insn_id: site_insn_id,
            } = InsnSiteId::unpack_from_slice(&**site_id).unwrap();
            writeln!(
                f,
                "  {} {}: arg{} {} -> site {}:{} resolves to {}",
                cs,
                func_id.id,
                formal_index,
                path.to_dot_string(),
                site_func_id.id,
                site_insn_id.id,
                tgt.id
            )?;
        }

        writeln!(
            f,
            "\nFunc Ptr Assign-Like ({}):",
            self.func_ptr_assign_like.len()
        )?;
        for (func_id, insn_id, var, path, tgt) in self.func_ptr_assign_like {
            let var_str = match var {
                FlowVariable::Local(name) => name.to_string(),
                _ => format!("{}", var),
            };
            writeln!(
                f,
                "  {}:{}: {}{} = ptr {}",
                func_id.id,
                insn_id.id,
                var_str,
                path.to_dot_string(),
                tgt.id
            )?;
        }

        writeln!(f, "\nContext Assign ({}):", self.context_assign.len())?;
        for (cs, func_id, insn_id, dest_var, dest_path, src_var, src_path) in self.context_assign {
            let dest_str = {
                let var_str = match dest_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", dest_var),
                };
                format!("{}{}", var_str, dest_path.to_dot_string())
            };
            let src_str = {
                let var_str = match src_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", src_var),
                };
                format!("{}{}", var_str, src_path.to_dot_string())
            };
            writeln!(
                f,
                "  {} {}:{}: {} = {}",
                cs, func_id.id, insn_id.id, dest_str, src_str
            )?;
        }

        writeln!(f, "\nContext Locals ({}):", self.context_locals.len())?;
        for (cs, func_id, var, path, formal_idx, formal_path) in self.context_locals {
            let var_str = match var {
                FlowVariable::Local(name) => name.to_string(),
                _ => format!("{}", var),
            };
            writeln!(
                f,
                "  {} {}: {}{} from arg{}{}",
                cs,
                func_id.id,
                var_str,
                path.to_dot_string(),
                formal_idx,
                formal_path.to_dot_string()
            )?;
        }

        writeln!(f, "\nContext Summary ({}):", self.context_summary.len())?;
        for (cs, func_id, dest_idx, dest_path, src_idx, src_path) in self.context_summary {
            writeln!(
                f,
                "  {} {}: arg{}{} = arg{}{}",
                cs,
                func_id.id,
                dest_idx,
                dest_path.to_dot_string(),
                src_idx,
                src_path.to_dot_string()
            )?;
        }
        Ok(())
    }
}

/// Creates a data flow graph for taint analysis.
pub fn taint_index(facts: IndexFacts) -> IndexResult {
    taint_index_with_config(facts, IndexConfig::default())
}

pub fn taint_index_with_config(facts: IndexFacts, config: IndexConfig) -> IndexResult {
    // Access paths may be introduced in summaries, so include those.
    use hashbrown::hash_set::HashSet;
    let summary_paths: HashSet<_> = facts
        .summary
        .iter()
        .flat_map(|(_, _, p1, _, p2)| [(p1.clone(),), (p2.clone(),)])
        .collect();
    let call = facts
        .call
        .iter()
        .map(|(site, target)| {
            let InsnSiteId { func_id, insn_id } = InsnSiteId::unpack_from_slice(&**site).unwrap();
            (func_id, insn_id, *target)
        })
        .collect();
    let config = vec![(config,)];
    let prog = ascent_run! {
        #![measure_rule_times]
        // Facts:

        relation formal_param(FunctionId, FlowVariable, FormalType) = facts.formal_param;
        relation actual_param(PackedInsnSiteId, FormalIndex, FlowVertex) = facts.actual_param;
        relation call(FunctionId, InsnId, FunctionId) = call;
        relation assign(PackedInsnSiteId, FlowVertex, FlowVertex) = facts.assign;
        // func:insn: v = ptr<function_id>
        relation func_ptr_assign(PackedInsnSiteId, FlowVertex, FunctionId) = facts.func_ptr_assign;
        relation java_obj_assign(PackedInsnSiteId, FlowVertex, Symbol) = facts.java_obj_assign;
        relation java_call(PackedInsnSiteId, FlowVertex, Symbol, Symbol) = facts.java_call;
        relation java_resolvents(Symbol, Symbol, Symbol, FunctionId) = facts.java_resolvents;
        relation indirect_call(PackedInsnSiteId, FlowVertex) = facts.indirect_call;

        // Analysis drivers:

        // Set of syntactic access paths
        relation paths(Path);
        relation summary(FunctionId, FormalIndex, Path, FormalIndex, Path) = facts.summary;
        relation config(IndexConfig) = config;

        // Derived:

        relation locals(FunctionId, FlowVariable, Path, FormalIndex, Path);
        relation assign_like(FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path);
        relation java_obj_assign_like(FunctionId, InsnId, FlowVariable, Path, Symbol);
        relation model_paths(Path) = summary_paths.into_iter().collect();
        relation program_paths(Path);

        // Hybrid Inlining relations:
        relation critical_summary(FunctionId, FormalIndex, Path, PackedInsnSiteId);
        // Resolvent reaches the formals of Function.
        relation resolvent(CallString, FunctionId, FormalIndex, Path, PackedInsnSiteId, FunctionId);
        relation func_ptr_assign_like(FunctionId, InsnId, FlowVariable, Path, FunctionId);
        relation context_assign(CallString, FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path);
        relation context_locals(CallString, FunctionId, FlowVariable, Path, FormalIndex, Path);
        relation context_summary(CallString, FunctionId, FormalIndex, Path, FormalIndex, Path);

        // Pointer Analysis derived relations
        relation pointer_vtx_points_to(FunctionId, FlowVariable, Path, Heap);
        relation pointer_fld_points_to(FunctionId, Heap, Path, Heap);

        // Sets up paths from input program with static info. Paths must remain finite so we
        // shouldn't add paths from constructed summaries directly.
        program_paths(p) <-- actual_param(_, _, vx), let FlowVertex(_, p) = vx;
        program_paths(p1), program_paths(p2) <-- assign(_, vx, vy), let FlowVertex(_, p1) = vx, let FlowVertex(_, p2) = vy;
        paths(p) <-- program_paths(p);
        paths(p) <-- model_paths(p);

        // Combine model paths with program paths (one level only to ensure termination)
        paths(p1.concat(p2)) <-- model_paths(p1), program_paths(p2);
        paths(p2.concat(p1)) <-- program_paths(p2), model_paths(p1);


        // Pointer Analysis Rules (Context-Insensitive Andersen Style)
        // 1. Alloc. Each formal param gets an object.
        pointer_vtx_points_to(m.clone(), v.clone(), Path::empty(), h) <--
            formal_param(m, v, ty), let h = Heap::new(v.formal().unwrap());

        // 2. Alloc fields of formals if those fields are used
        pointer_vtx_points_to(m.clone(), v, assign_path.clone(), h.clone()),
        pointer_fld_points_to(m.clone(), base_h.clone(), Path(suffix.clone()), h.clone()) <--
            pointer_vtx_points_to(m, v, path, base_h),
            (assign_like(m, _, _, _, v, assign_path) | assign_like(m, _, v, assign_path, _, _)),
            if let Some(suffix) = match_prefix(assign_path, path),
            if let Some(end) = suffix.back() && end.is_symbol(),
            let h = Heap::with_path(base_h.index(), assign_path.clone());

        // 3. Propagation and Assignments
        // x.p = y.f;
        // y.f.g -> h
        // ==>
        // x.p.g -> h
        pointer_vtx_points_to(m.clone(), to.clone(), to_path.clone(), h.clone()) <--
            assign_like(m, _, to, dst_path, from, src_path),
            pointer_vtx_points_to(m, from, from_path, h),
            if let Some(to_path) = from_path.substitute_prefix(src_path, dst_path),
            paths(&to_path);

        // This propagates in the reverse direction of assignment
        // x.p = y.f;
        // x.p.g -> h
        // ==>
        // y.f.g -> h
        // pointer_vtx_points_to(m.clone(), from.clone(), from_path.clone(), h.clone()) <--
        //     assign_like(m, _, to, dst_path, from, src_path),
        //     pointer_vtx_points_to(m, to, to_path, h),
        //     if let Some(from_path) = to_path.substitute_prefix(dst_path, src_path),
        //     paths(&from_path);

        // Store
        // x.p.q = y.f;
        // y.f -> h /\ x.p -> base_h
        // ==>
        // base_h|.q -> h
        pointer_fld_points_to(m.clone(), base_h.clone(), Path(fld.clone()), h.clone()) <--
            assign_like(m, _, base, dst_path, from, src_path),
            pointer_vtx_points_to(m, from, src_path, h),
            pointer_vtx_points_to(m, base, base_path, base_h),
            if let Some(fld) = match_prefix(dst_path, base_path) && fld.len() == 1,
            paths(&Path(fld.clone()));

        // Load
        // x.p = y.f.q;
        // y.f -> -> base_h /\ base_h|.q -> h
        // ==>
        // x.p -> h
        pointer_vtx_points_to(m.clone(), to, dst_path.clone(), h.clone()) <--
            assign_like(m, _, to, dst_path, base, src_path),
            pointer_vtx_points_to(m, base, base_path, base_h),
            if let Some(fld) = match_prefix(src_path, base_path) && fld.len() == 1 ,
            pointer_fld_points_to(m, base_h, Path(fld.clone()), h),
            paths(&Path(fld.clone()));

        locals(m.clone(), v, p, n, pn.clone()) <--
            pointer_vtx_points_to(m, v, p, h),
            let n = h.formal_index,
            let pn = &h.path;

        // Flow from heap to formal
        summary(m, n1, p1.clone(), n2, p2.clone()) <--
            pointer_vtx_points_to(m, dst_var, p1, h2),
            formal_param(m, dst_var, formal_ty),
            if let FlowVariable::Formal(n1) = dst_var,
            let n2 = h2.formal_index,
            let p2 = &h2.path,
            if isout(n1, *formal_ty, p1),
            if *n1 != n2 || p1 != p2;
        // Flow from formal to heap
        summary(m, h.formal_index, h.path.clone(), src_n, src_path.clone()) <--
            pointer_vtx_points_to(m, src_var, src_path, h),
            formal_param(m, src_var, formal_ty),
            if let FlowVariable::Formal(src_n) = src_var,
            if isout(&h.formal_index, *formal_ty, src_path),
            if *h.formal_index != **src_n;

        // Initialize assigns from program
        assign_like(func_id, insn_id, v1, p1, v2, p2) <--
            assign(site_id_slice, dst, src),
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**site_id_slice).unwrap(),
            let FlowVertex(v1, p1) = dst,
            let FlowVertex(v2, p2) = src;

        // Compute assignments from call sites
        assign_like(func_id, insn_id, v.clone(), p, cv.clone(), Path::empty()),
        assign_like(func_id, insn_id, cv.clone(), Path::empty(), v.clone(), p) <--
            actual_param(call_site_slice, n, vx),
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**call_site_slice).unwrap(),
            let cv = FlowVariable::CallArg { id: call_site_slice.clone(), formal: n.clone() },
            let FlowVertex(v, p) = vx;

        // Compute assignments from summaries
        assign_like(func_id, insn_id, v1, p1, v2, p2) <--
            summary(tgt, n1, dst_path, n2, src_path),
            call(func_id, insn_id, tgt),
            let site_id = InsnSiteId { func_id: *func_id, insn_id: *insn_id },
            let call_site_id = PackedInsnSiteId::try_from(site_id).unwrap(),
            let v1 = FlowVariable::CallArg { id: call_site_id, formal: n1.clone() },
            let p1 = dst_path.clone(),
            let v2 = FlowVariable::CallArg { id: call_site_id, formal: n2.clone() },
            let p2 = src_path.clone();

        // Hybrid Inlining Rules:
        // Phase 1: propagate up the stack from indirect calls
        // Phase 2: propagate resolvents back down (this requires call strings)
        // Phase 3: propagate conditional summaries up till they're unconditional

        // 1.1: Base Critical Summary. Indirect call or Java Call found.
        critical_summary(func_id, n, p_n, site_id) <--
            (indirect_call(site_id, vx) | java_call(site_id, vx, _, _)),
            let FlowVertex(v, p_call) = vx,
            let InsnSiteId {func_id, ..} = InsnSiteId::unpack_from_slice(&**site_id).unwrap(),
            locals(func_id, v, p_call, n, p_n);

        // 1.2: Propagate Critical Summary
        critical_summary(caller_func_id, n, p_n, critical_site_id) <--
            critical_summary(tgt, n_tgt, p_tgt, critical_site_id),
            call(caller_func_id, caller_insn_id, tgt),
            let cs_id = PackedInsnSiteId::try_from_parts(*caller_func_id, *caller_insn_id).unwrap(),
            let arg = FlowVariable::CallArg { id: cs_id, formal: *n_tgt },
            locals(caller_func_id, arg, p_tgt, n, p_n);

        // 2.1: Base Resolvent. Resolvent object locally reaches a critical summary, so instantiate
        //   resolvent in parameters of summary
        resolvent(new_cs.clone(), tgt, n_tgt, p_tgt.clone(), critical_site_id, ptr_tgt) <--
            critical_summary(tgt, n_tgt, p_tgt, critical_site_id),
            call(call_func_id, insn_id, tgt),
            let call_site_id = PackedInsnSiteId::try_from_parts(*call_func_id, *insn_id).unwrap(),
            let arg = FlowVariable::CallArg { id: call_site_id, formal: *n_tgt },
            (func_ptr_assign_like(call_func_id, _, arg, p_tgt, ptr_tgt) |
             (java_obj_assign_like(call_func_id, _, arg, p_tgt, cls),
              java_call(critical_site_id, _, method_name, method_desc),
              java_resolvents(cls, method_name, method_desc, ptr_tgt))),
            let cs = CallString::new(),
            if let Some(new_cs) = cs.push(call_site_id);

        // 2.2: Propagate Resolvent
        resolvent(new_cs.clone(), tgt, n_tgt, p_tgt.clone(), critical_site_id, ptr_tgt) <--
            resolvent(cs, func_id, n, p, critical_site_id, ptr_tgt),
            call(func_id, insn_id, tgt),
            critical_summary(tgt, _, _, critical_site_id),
            let call_site_id = PackedInsnSiteId::try_from_parts(*func_id, *insn_id).unwrap(),
            locals(func_id, v_tgt, p_tgt, n, p),
            if let FlowVariable::CallArg { id: tgt_site, formal: n_tgt } = v_tgt,
            if let Some(new_cs) = cs.push(call_site_id);

        // 3.1: Contextual Assignment (instantiate)
        context_assign(cs.clone(), func_id, insn_id, v1.clone(), p1.clone(), v2.clone(), p2.clone()) <--
            resolvent(cs, func_id, n, p, critical_site_id, ptr_tgt),
            locals(func_id, v_rec, p_v, n, p),
            let vx_rec = FlowVertex(v_rec.clone(), p_v.clone()),
            summary(ptr_tgt, n1, p1_sum, n2, p2_sum),
            (java_call(critical_site_id, vx_rec, _, _) | indirect_call(critical_site_id, vx_rec)),
            let v1 = FlowVariable::CallArg { id: critical_site_id.clone(), formal: n1.clone() },
            let p1 = p1_sum.clone(),
            let v2 = FlowVariable::CallArg { id: critical_site_id.clone(), formal: n2.clone() },
            let p2 = p2_sum.clone(),
            let InsnSiteId {func_id: call_func_id, insn_id} = InsnSiteId::unpack_from_slice(&**critical_site_id).unwrap();

        // 3.2: Contextual Locals Initialization and Propagation
        context_locals(cs.clone(), func_id, v1.clone(), p13.clone(), n.clone(), pn.clone()) <--
            context_assign(cs, func_id, _, v1, p1, v2, p2),
            locals(func_id, v2, p23, n, pn),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        context_locals(cs.clone(), func_id, v2.clone(), p23.clone(), n.clone(), pn.clone()) <--
            context_assign(cs, func_id, _, v1, p1, v2, p2),
            locals(func_id, v1, p13, n, pn),
            if let Some(p23) = p13.substitute_prefix(p1, p2),
            paths(p23.clone());

        context_locals(cs.clone(), func_id, v1.clone(), p13.clone(), n.clone(), pn.clone()) <--
            context_locals(cs, func_id, v2, p23, n, pn),
            assign_like(func_id, _, v1, p1, v2, p2),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        context_locals(cs.clone(), func_id, v1.clone(), p1.clone(), n.clone(), pn3.clone()) <--
            context_locals(cs, func_id, v2, p2, n, pn),
            assign_like(func_id, _, v1, p1, v2, p23),
            if let Some(pn3) = p23.substitute_prefix(p2, pn),
            paths(pn3.clone());

        // 3.3: Contextual Summary Creation
        context_summary(cs.clone(), func_id, n1.clone(), p1.clone(), n2.clone(), p2.clone()) <--
            context_locals(cs, func_id, dst_var, p1, n2, p2),
            formal_param(func_id, dst_var, formal_ty),
            if let FlowVariable::Formal(n1) = dst_var,
            if isout(n1, *formal_ty, p1),
            if n1 != n2 || p1 != p2;

        context_summary(cs.clone(), func_id, n1.clone(), ap3.clone(), n2.clone(), bp.clone()) <--
            context_locals(cs, func_id, v1, p1, n1, ap),
            locals(func_id, v1, p13, n2, bp),
            if let Some(ap3) = p13.substitute_prefix_with_nonempty_suffix(p1, ap),
            paths(ap3.clone()),
            if n1 != n2 || ap3 != *bp;

        context_summary(cs.clone(), func_id, n1.clone(), ap3.clone(), n2.clone(), bp.clone()) <--
            locals(func_id, v1, p1, n1, ap),
            context_locals(cs, func_id, v1, p13, n2, bp),
            if let Some(ap3) = p13.substitute_prefix_with_nonempty_suffix(p1, ap),
            paths(ap3.clone()),
            if n1 != n2 || ap3 != *bp;

        context_summary(cs.clone(), func_id, n1.clone(), ap3.clone(), n2.clone(), bp.clone()) <--
            context_locals(cs, func_id, v1, p1, n1, ap),
            context_locals(cs, func_id, v1, p13, n2, bp),
            if let Some(ap3) = p13.substitute_prefix_with_nonempty_suffix(p1, ap),
            paths(ap3.clone()),
            if n1 != n2 || ap3 != *bp;

        // 3.4: Instantiate Summaries and pop call string
        context_assign(new_cs.clone(), func_id, insn_id, v1.clone(), p1.clone(), v2.clone(), p2.clone()) <--
            context_summary(cs, tgt, n1, p1_sum, n2, p2_sum),
            let (new_cs, popped) = cs.pop(),
            if let Some(call_site_id) = popped,
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&*call_site_id).unwrap(),
            call(func_id, insn_id, tgt),
            let v1 = FlowVariable::CallArg { id: call_site_id.clone(), formal: n1.clone() },
            let p1 = p1_sum.clone(),
            let v2 = FlowVariable::CallArg { id: call_site_id.clone(), formal: n2.clone() },
            let p2 = p2_sum.clone();

        // 3.5
        summary(func_id, n1.clone(), p1.clone(), n2.clone(), p2.clone()) <--
            context_summary(CallString::new(), func_id, n1, p1, n2, p2);

        assign_like(func_id, insn_id, v1.clone(), p1.clone(), v2.clone(), p2.clone()) <--
            context_assign(CallString::new(), func_id, insn_id, v1, p1, v2, p2);

        // Function Pointer Propagation
        func_ptr_assign_like(func_id, insn_id, v.clone(), p.clone(), tgt) <--
            func_ptr_assign(site_id, vx, tgt), let FlowVertex(v, p) = vx,
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**site_id).unwrap();

        func_ptr_assign_like(func_id, insn_id, v1.clone(), p_new.clone(), tgt) <--
            func_ptr_assign_like(func_id, _, v2, p_context, tgt),
            assign_like(func_id, insn_id, v1, p1, v2, p2),
            if let Some(p_new) = p_context.substitute_prefix(p2, p1),
            paths(p_new.clone());

        // Java Object Propagation
        java_obj_assign_like(func_id, insn_id, v.clone(), p.clone(), tgt) <--
            java_obj_assign(site_id, vx, tgt), let FlowVertex(v, p) = vx,
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**site_id).unwrap();

        java_obj_assign_like(func_id, insn_id, v1.clone(), p_new.clone(), tgt) <--
            java_obj_assign_like(func_id, _, v2, p_context, tgt),
            assign_like(func_id, insn_id, v1, p1, v2, p2),
            if let Some(p_new) = p_context.substitute_prefix(p2, p1),
            paths(p_new.clone());

        // Pointer analysis relations:

    };
    log::info!("index scc times: {}", prog.scc_times_summary());
    log::trace!(
        "hybrid inlining relations:\n{}",
        HybridInliningRelations {
            critical_summary: &prog.critical_summary,
            resolvent: &prog.resolvent,
            func_ptr_assign_like: &prog.func_ptr_assign_like,
            context_assign: &prog.context_assign,
            context_locals: &prog.context_locals,
            context_summary: &prog.context_summary,
        }
    );
    log::trace!(
        "pointer analysis relations:\n{}",
        PointerAnalysisRelations {
            vtx_points_to: &prog.pointer_vtx_points_to,
            fld_points_to: &prog.pointer_fld_points_to,
        }
    );
    let result = IndexResult {
        summary: prog.summary,
        assign_like: prog.assign_like,
        java_obj_assign_like: prog.java_obj_assign_like,
        paths: prog.paths,
        vtx_points_to: prog.pointer_vtx_points_to,
        fld_points_to: prog.pointer_fld_points_to,
    };
    log::trace!("index result: {}", result);
    result
}
