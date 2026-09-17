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

use std::num::NonZeroUsize;
use std::path;
use std::sync::Arc;

use ascent::ascent;
use ascent::ascent_par;
use ascent::ascent_run;
use ascent::ascent_source;
use derive_builder::Builder;
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;
use streaming_iterator::StreamingIterator;

use crate::error::Error;
use crate::facts::{
    CallArgId, CallDispatchKey, CallTargetObject, FlowVariable, FlowVariableKind, FlowVertex,
    FormalIndex, FormalType, FunctionId, IdMap, InsnId, InsnSiteId, PackedCallArg,
    PackedInsnSiteId, Path, isout,
};
use crate::index_engine::assign_like_trie::FromRows;
pub use crate::index_engine::decision::{Decision, DecisionId, DecisionSet};
use crate::index_engine::path_set::{PathSet, PathSetRef};

pub mod assign_like_trie;
pub mod c_assign_like_trie;
pub mod c_locals_trie;
pub mod decision;
pub mod hybrid_set;
pub mod locals_trie;
pub mod path_group;
pub mod path_set;
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
    /// A call target (function pointer or Java object) stored at a vertex by an
    /// assignment. The union of what were the separate `func_ptr_assign` and
    /// `java_obj_assign` relations; the variant of [`CallTargetObject`] distinguishes
    /// the C-style function-pointer case from the Java-object case.
    #[builder(default)]
    pub call_target_assign: Vec<(PackedInsnSiteId, FlowVertex, CallTargetObject)>,
    /// An indirect / virtual call site awaiting resolution
    #[builder(default)]
    pub callee_info: Vec<(PackedInsnSiteId, FlowVertex, CallDispatchKey)>,
    /// How a stored call target ([`CallTargetObject`]) resolves, under a given
    /// [`CallDispatchKey`], to a concrete callee.
    #[builder(default)]
    pub callee_resolvents: Vec<(CallTargetObject, CallDispatchKey, FunctionId)>,
    #[builder(default)]
    pub summary: Vec<FunctionSummary>,
    #[builder(default)]
    pub paths: Vec<(Path,)>,
    #[builder(default)]
    pub external_function: Vec<(FunctionId,)>,
}

impl IndexFacts {
    /// Saves the `formal_param`, `actual_param`, and `call` members. The others aren't saved
    /// because they are computed as part of an [`IndexResult`].
    ///
    /// Borrows `self`: only the small tuples actually serialized here are cloned. This avoids
    /// deep-copying the entire fact base (dominated by `assign`, which isn't even written by
    /// this method) at the call site, which would otherwise stack a full transient copy on top
    /// of the memory-peak indexing run.
    pub fn try_save<P: AsRef<path::Path>>(&self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        formal_param::try_save(
            &dir,
            self.formal_param.iter().map(|(func_id, var, ty)| {
                let Some(i) = var.as_formal() else {
                    panic!("formal_param variable is not a formal")
                };
                (*func_id, i, *ty)
            }),
        )?;
        actual_param::try_save(
            &dir,
            self.actual_param
                .iter()
                .map(|(site_id, formal_index, vertex)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&**site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, *formal_index, *variable, *path)
                }),
        )?;
        call::try_save(
            &dir,
            self.call.iter().map(|(site_id, target)| {
                let InsnSiteId { func_id, insn_id } =
                    InsnSiteId::unpack_from_slice(&**site_id).unwrap();
                (func_id, insn_id, *target)
            }),
        )?;
        call_target_assign::try_save(
            &dir,
            self.call_target_assign
                .iter()
                .map(|(site_id, vertex, target)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&**site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, *variable, *path, target.clone())
                }),
        )?;
        callee_info::try_save(
            &dir,
            self.callee_info
                .iter()
                .map(|(site_id, vertex, dispatch_key)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&**site_id).unwrap();
                    let FlowVertex(variable, path) = vertex;
                    (func_id, insn_id, *variable, *path, dispatch_key.clone())
                }),
        )?;
        callee_resolvents::try_save(&dir, self.callee_resolvents.iter().cloned())?;
        external_function::try_save(&dir, self.external_function.iter().copied())?;
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
                        let var = FlowVariable::formal_index(i);
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
            .call_target_assign(
                call_target_assign::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, variable, path, target)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (
                            site_id.try_into().expect("error packing site_id"),
                            FlowVertex(variable, path),
                            target,
                        )
                    })
                    .collect(),
            )
            .callee_info(
                callee_info::try_load(&dir)?
                    .into_iter()
                    .map(|(func_id, insn_id, variable, path, context)| {
                        let site_id = InsnSiteId { func_id, insn_id };
                        (
                            site_id.try_into().expect("error packing site_id"),
                            FlowVertex(variable, path),
                            context,
                        )
                    })
                    .collect(),
            )
            .callee_resolvents(callee_resolvents::try_load(&dir)?)
            .external_function(external_function::try_load(&dir)?);
        Ok(builder.build().unwrap())
    }

    /// Computes the number of parameters for each function found
    pub fn compute_num_params(&self) -> HashMap<FunctionId, i16> {
        let mut func_num_params: HashMap<FunctionId, i16> = HashMap::new();
        for (func, var, _) in self.formal_param.iter() {
            let i: i16 = match var.kind() {
                FlowVariableKind::Formal(i) => *i,
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

    /// Like [`compute_num_params`], but widens each function's arity to the
    /// maximum *actual* argument index passed at any of its call sites (unioned
    /// with the declared formals). This lets `AnyArgument` ("*") range over the
    /// arguments actually passed -- including variadic arguments, which have no
    /// declared formal -- rather than the callee's fixed signature. Matching on
    /// actuals (not formals) is what makes a model like `sprintf: Argument(*) ->
    /// Argument(0)` cover the variadic command-string pieces.
    pub fn compute_arg_arity(&self) -> HashMap<FunctionId, i16> {
        let mut arity = self.compute_num_params();
        // call site -> target function
        let mut target: HashMap<PackedInsnSiteId, FunctionId> = HashMap::new();
        for (site, func) in self.call.iter() {
            target.insert(*site, *func);
        }
        for (site, idx, _) in self.actual_param.iter() {
            let i: i16 = **idx;
            if i < 0 {
                // negative indices are sentinels (globals/return), not arg positions
                continue;
            }
            if let Some(func) = target.get(site) {
                arity
                    .entry(*func)
                    .and_modify(|m| *m = (*m).max(i + 1))
                    .or_insert(i + 1);
            }
        }
        arity
    }
}

#[derive(Debug, Clone, Builder, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IndexConfig {
    /// Enables the aliasing summary rule.
    pub alias_rule: bool,
    /// How a critical call site's resolved summary is instantiated. See [`HybridContext`].
    pub hybrid_context: HybridContext,
    /// How rule 3.2 finds the callers a conditional summary applies at. See [`ContextJoin`].
    pub context_join: ContextJoin,
    /// Which engine computes the flow relation, and on how many threads. Serial by default; see
    /// [`Parallelism::from_jobs`] for the `-j N` convention.
    pub parallelism: Parallelism,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            alias_rule: true,
            hybrid_context: HybridContext::default(),
            context_join: ContextJoin::default(),
            parallelism: Parallelism::Serial,
        }
    }
}

/// How rule 3.2 pairs a function's conditional summaries (`context_summary`, one row per
/// summary row, keyed by a [`DecisionSet`]) with the calls that established a decision at it
/// (`establishes_direct` / `establishes_via`, one row per call site and decision). All three
/// compute the same fixpoint; they differ in how much of the pairing is wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ContextJoin {
    /// Join both sides on the function alone and test `ds.contains(d)` on every pair. The
    /// work is `|summaries(f)| x |establishing calls(f)|` per function, most of which the
    /// membership test throws away (`docs/bug-context-assign-blowup.md`).
    Scan,
    /// Factor the pairing through the distinct decision sets the summaries carry: unfold each
    /// set's members once (`set_member`), join those with the establishing calls on
    /// `(f, d)`, and join the result back to the summary rows on `(f, set)`. Every join is an
    /// exact probe, and the unfolding is per distinct set, not per summary row.
    #[default]
    Sets,
    /// Unfold every summary row per decision in its set (`context_summary_d`) and join the
    /// establishing calls on `(f, d)`. Exact probes too, but the unfolded relation has one row
    /// per (summary row, decision): more memory than `Sets` wherever rows share a set.
    Unfold,
}

impl std::str::FromStr for ContextJoin {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "scan" => Ok(ContextJoin::Scan),
            "sets" => Ok(ContextJoin::Sets),
            "unfold" => Ok(ContextJoin::Unfold),
            other => Err(format!(
                "unknown context join '{other}'; expected 'scan', 'sets' or 'unfold'"
            )),
        }
    }
}

impl std::fmt::Display for ContextJoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ContextJoin::Scan => "scan",
            ContextJoin::Sets => "sets",
            ContextJoin::Unfold => "unfold",
        })
    }
}

/// What the hybrid-inlining rules do once a resolvent has decided which function a critical
/// (indirect / virtual) call site invokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum HybridContext {
    /// The decided callee's summary is instantiated at the site as *contextual* edges keyed by
    /// the [`Decision`] that resolved it, closed into a summary of the enclosing function
    /// conditioned on that decision, and applied one call site at a time at every caller whose
    /// route established the decision, until it lands as a plain edge in a function that held
    /// the call target. Two callers passing different targets get different flows.
    ///
    /// The key of a contextual row is a [`DecisionSet`], a lattice: a row holds under every
    /// decision in its set, and rows that several decisions reach are shared rather than
    /// repeated, so the contextual relations are bounded by the context-free ones -- one row per
    /// `(f, v, p, a, p4)` -- whatever the number of decisions reaching a function. Complete,
    /// deterministic (the fixpoint does not depend on the order routes are found in), and the
    /// default.
    #[default]
    Decision,
    /// As `Decision`, with the set collapsed to ⊤ -- "every decision of this function" -- the
    /// moment a row is reached by two different decisions. A conditional summary row under ⊤
    /// is applied at every caller that establishes any decision of the function. Coarser than
    /// `Decision` exactly where two decisions' flows meet at a row and a third decision does
    /// not share the flow; no parameter, and still a lattice, so the fixpoint is unique. A row
    /// changes at most once after it is created, where an exact set changes once per decision
    /// that reaches it later, and each change re-propagates everything downstream: on a
    /// function that thousands of decisions reach through a feedback loop that is the
    /// difference between minutes and the context-free closure's own cost.
    Collapse,
    /// As `Decision`, with the set widened to ⊤ the moment an exact union has more than `k`
    /// members. `Bounded(1)` is `Collapse` on the same lattice (`⊥ < {d} < ⊤`), and every
    /// `k` bounds the number of times a row can change to `k + 1`. Coarser than `Decision`
    /// exactly where more than `k` decisions share a row and another decision does not.
    Bounded(usize),
    /// As `Bounded(k)`, but a row whose set widens to ⊤ leaves the contextual closure: a ⊤
    /// edge becomes a plain `assign_like` edge and a ⊤ local a plain `locals` row, so the flow
    /// is shared by every caller through the ordinary `summary` (as under `None`), and nothing
    /// contextual is derived from it. Coarser than `Bounded(k)` downstream of the widened row
    /// (`Bounded` still conditions the callers' inherited flows on their own decisions), but
    /// no ⊤ summary is ever applied at every establishing caller, which is what makes
    /// `Bounded` feed back on itself.
    Spill(usize),
    /// The decided callee's summary is instantiated at the site as plain edges, exactly as a
    /// directly resolved call's would be. The resolvent machinery still decides *which* callees
    /// a site can have -- a target that never flows to the site is never instantiated -- but the
    /// effect is shared by every caller: the context-sensitive fixpoint derives a call graph,
    /// and the dataflow is computed over it context-insensitively. No `context_*` relation is
    /// ever populated.
    ///
    /// Bounded by the context-free analysis; but it unions the decisions at a site, which is
    /// what hybrid inlining exists to avoid (`tests/tnt/hybrid_inlining.tnt`).
    None,
}

impl std::str::FromStr for HybridContext {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "decision" => Ok(HybridContext::Decision),
            "collapse" => Ok(HybridContext::Collapse),
            "none" => Ok(HybridContext::None),
            other => match (
                other.strip_prefix("bounded:").map(str::parse::<usize>),
                other.strip_prefix("spill:").map(str::parse::<usize>),
            ) {
                (Some(Ok(k)), _) if k > 0 => Ok(HybridContext::Bounded(k)),
                (_, Some(Ok(k))) if k > 0 => Ok(HybridContext::Spill(k)),
                _ => Err(format!(
                    "unknown hybrid context '{other}'; expected 'decision', 'collapse', \
                     'bounded:K', 'spill:K' (K >= 1) or 'none'"
                )),
            },
        }
    }
}

/// Number of threads for the index engine
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Parallelism {
    #[default]
    Serial,
    Threads(NonZeroUsize),
}

impl Parallelism {
    /// The `-j N` convention: `1` is the serial engine, `0` means every core the OS reports, and
    /// anything else is the parallel engine on that many threads.
    ///
    /// A one-core machine asked for `0` gets the serial engine, since the parallel one buys
    /// nothing there but its overhead.
    pub fn from_jobs(jobs: usize) -> Self {
        let jobs = if jobs == 0 {
            std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
        } else {
            jobs
        };
        match NonZeroUsize::new(jobs) {
            Some(n) if n.get() > 1 => Parallelism::Threads(n),
            _ => Parallelism::Serial,
        }
    }
}

impl std::fmt::Display for Parallelism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Parallelism::Serial => write!(f, "serial"),
            Parallelism::Threads(n) => write!(f, "parallel on {n} threads"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub initial_assign: usize,
    pub final_assign_like: usize,
    pub initial_formals: usize,
    pub final_locals: usize,
    pub initial_call_target_assign: usize,
    pub final_call_target_assign_like: usize,
    pub initial_summary: usize,
    pub final_summary: usize,
    pub num_functions: usize,
    pub num_variables: usize,
    /// Distinct variables appearing as the subject of a `locals` row, i.e. reached by
    /// some formal. Bounded above by `num_variables`.
    pub reached_variables: usize,
    pub hybrid_critical_summary: usize,
    pub hybrid_resolvent: usize,
    pub hybrid_context_assign: usize,
    pub hybrid_context_locals: usize,
    pub hybrid_context_summary: usize,
}

impl IndexStats {
    pub fn log(&self) {
        let ratio =
            |final_val: usize, initial_val: usize| (final_val as f64) / (initial_val.max(1) as f64);

        log::debug!(
            "relation increase: assign_like: {:.2} ({}/{})",
            ratio(self.final_assign_like, self.initial_assign),
            self.final_assign_like,
            self.initial_assign
        );
        log::debug!(
            "relation increase: locals: {}, {} formals, {:.2} reached per formal, {:.1}% of variables reached ({}/{}), {:.2} rows per variable",
            self.final_locals,
            self.initial_formals,
            ratio(self.final_locals, self.initial_formals),
            100.0 * ratio(self.reached_variables, self.num_variables),
            self.reached_variables,
            self.num_variables,
            ratio(self.final_locals, self.num_variables)
        );
        log::debug!(
            "relation increase: call_target_assign_like: {:.2} ({}/{})",
            ratio(
                self.final_call_target_assign_like,
                self.initial_call_target_assign
            ),
            self.final_call_target_assign_like,
            self.initial_call_target_assign
        );
        log::debug!(
            "relation increase: summary: {:.2} ({}/{}) (ratio over num_functions)",
            ratio(self.final_summary, self.num_functions),
            self.final_summary,
            self.num_functions
        );
        log::debug!(
            "hybrid inlining: critical_summary: {:.2} ({}/{}), resolvent: {}, context_assign: {:.2} ({}/{}) (ratio over final assign_like), context_locals: {:.2} ({}/{}) (ratio over final locals), context_summary: {}",
            ratio(self.hybrid_critical_summary, self.num_functions),
            self.hybrid_critical_summary,
            self.num_functions,
            self.hybrid_resolvent,
            ratio(self.hybrid_context_assign, self.final_assign_like),
            self.hybrid_context_assign,
            self.final_assign_like,
            ratio(self.hybrid_context_locals, self.final_locals),
            self.hybrid_context_locals,
            self.final_locals,
            self.hybrid_context_summary
        );
    }
}

#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Summary goes from formal parameter index to formal ret index.
    pub summary: Vec<FunctionSummary>,
    pub assign_like: Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)>,
    pub call_target_assign_like: Vec<(FunctionId, FlowVariable, Path, CallTargetObject)>,
    pub paths: Vec<(Path,)>,
    pub external_function: Vec<(FunctionId,)>,
    pub stats: IndexStats,
}

impl IndexResult {
    pub fn try_save<P: AsRef<path::Path>>(self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        // `[mem cp]` around each table's serialize: the fixpoint's own checkpoints stop at
        // `ascent_run returned`, but on path-heavy targets (e.g. JVM `fb`) the true peak is
        // here, in the parquet writer, not in the fixpoint. Report row counts too so peak
        // bytes can be attributed to a specific table.
        log::debug!(
            "[mem cp] result.try_save start (summary={} assign_like={} paths={} ext={}): {:.1} MB",
            self.summary.len(),
            self.assign_like.len(),
            self.paths.len(),
            self.external_function.len(),
            phys_footprint_mb()
        );
        summary::try_save(&dir, self.summary)?;
        log::debug!(
            "[mem cp]   after summary::try_save: {:.1} MB",
            phys_footprint_mb()
        );
        let assign_like_rows = self.assign_like.len();
        assign::try_save(&dir, self.assign_like)?;
        log::debug!(
            "[mem cp]   after assign::try_save ({} rows): {:.1} MB",
            assign_like_rows,
            phys_footprint_mb()
        );
        let paths_rows = self.paths.len();
        paths::try_save(&dir, self.paths)?;
        log::debug!(
            "[mem cp]   after paths::try_save ({} rows): {:.1} MB",
            paths_rows,
            phys_footprint_mb()
        );
        external_function::try_save(&dir, self.external_function)?;
        log::debug!(
            "[mem cp] result.try_save done: {:.1} MB",
            phys_footprint_mb()
        );
        Ok(())
    }

    pub fn try_load<P: AsRef<path::Path>>(dir: P) -> Result<Self, Error> {
        use crate::facts::schema::*;
        let summary = summary::try_load(&dir)?;
        let assign_like = assign::try_load(&dir)?;
        let paths = paths::try_load(&dir)?;
        let external_function = external_function::try_load(&dir)?;
        Ok(IndexResult {
            summary,
            assign_like,
            call_target_assign_like: Vec::new(),
            paths,
            external_function,
            stats: IndexStats::default(),
        })
    }
}

impl std::fmt::Display for IndexResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.display(None).fmt(f)
    }
}

pub struct IndexResultDisplay<'a> {
    result: &'a IndexResult,
    id_map: Option<&'a IdMap>,
}

impl<'a> std::fmt::Display for IndexResultDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Summary:")?;
        for s in &self.result.summary {
            if let Some(id_map) = self.id_map {
                let func_name = id_map
                    .get_function(s.0)
                    .map(|f| f.0.as_ref())
                    .unwrap_or("unknown");
                writeln!(f, "{}({}): {:?}", func_name, s.0.id, s)?;
            } else {
                writeln!(f, "{:?}", s)?;
            }
        }
        writeln!(f, "\nAssign-like:")?;
        for (func_id, dest_var, dest_path, src_var, src_path) in &self.result.assign_like {
            let dest_str = {
                let var_str = if let Some(name) = dest_var.as_local() {
                    name.to_string()
                } else {
                    format!("{}", dest_var)
                };
                format!("{}{}", var_str, dest_path.to_dot_string())
            };
            let src_str = {
                let var_str = if let Some(name) = src_var.as_local() {
                    name.to_string()
                } else {
                    format!("{}", src_var)
                };
                format!("{}{}", var_str, src_path.to_dot_string())
            };

            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "{}({}): {} = {}",
                func_name, func_id.id, dest_str, src_str
            )?;
        }
        writeln!(f, "\nPaths:")?;
        for (p,) in &self.result.paths {
            writeln!(f, "{}", p)?;
        }
        Ok(())
    }
}

impl IndexResult {
    pub fn display<'a>(&'a self, id_map: Option<&'a IdMap>) -> IndexResultDisplay<'a> {
        IndexResultDisplay {
            result: self,
            id_map,
        }
    }
}

/// One relation's rows, whatever the index engine happens to store them in.
///
/// Serial Ascent keeps a plain relation in a `Vec<Row>`. Parallel Ascent keeps a plain relation in
/// a `boxcar::Vec<Row>` (a lock-free append-only vector) and a *lattice* in a
/// `boxcar::Vec<RwLock<Row>>`. The three differ in how a row is reached — directly by reference,
/// or through a read guard — so [`HybridInliningRelations`] reads them through this trait instead
/// of a slice. That keeps the trace-dump call site identical under `ascent!` and `ascent_par!`,
/// and it copies nothing in any of the three cases.
///
/// The count is its own method rather than something taken from the stream, because `boxcar::Vec`
/// reports no upper size bound when iterated: the vector can be appended to mid-iteration, so its
/// iterator cannot be an `ExactSizeIterator`. Nothing appends here — the fixpoint is over before we
/// dump — but the type cannot know that.
trait Rows<Row> {
    fn len(&self) -> usize;

    /// Stream the rows, each borrowed for as long as it is looked at.
    ///
    /// A [`StreamingIterator`] rather than an `Iterator` because a lattice row lives behind an
    /// `RwLock`: the only way to hand out a plain `&Row` without copying is to keep that row's read
    /// guard alive for exactly the span of the borrow, which is what a streaming iterator's
    /// `advance`/`get` split expresses and a plain iterator cannot. Yielding `&Row` uniformly is
    /// also what keeps this trait object-safe, so [`HybridInliningRelations`] needs no type
    /// parameters.
    fn stream(&self) -> Box<dyn StreamingIterator<Item = Row> + '_>;
}

impl<Row> Rows<Row> for Vec<Row> {
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn stream(&self) -> Box<dyn StreamingIterator<Item = Row> + '_> {
        Box::new(streaming_iterator::convert_ref(self.as_slice().iter()))
    }
}

// The two parallel-engine cases. They are dead code under `ascent!` and live under `ascent_par!`;
// keeping both compiled is what lets the switch between the macros touch nothing here.
impl<Row> Rows<Row> for ascent::boxcar::Vec<Row> {
    fn len(&self) -> usize {
        ascent::boxcar::Vec::len(self)
    }

    fn stream(&self) -> Box<dyn StreamingIterator<Item = Row> + '_> {
        Box::new(streaming_iterator::convert_ref(self.iter()))
    }
}

/// A lattice's rows, each held under its own read guard while it is being looked at.
struct GuardedRows<'a, I, Row> {
    inner: I,
    guard: Option<std::sync::RwLockReadGuard<'a, Row>>,
}

impl<'a, I, Row> StreamingIterator for GuardedRows<'a, I, Row>
where
    I: Iterator<Item = &'a std::sync::RwLock<Row>>,
    Row: 'a,
{
    type Item = Row;

    fn advance(&mut self) {
        // Dropping the previous guard here is what bounds each row's lock to its own turn.
        self.guard = self.inner.next().map(|lock| lock.read().unwrap());
    }

    fn get(&self) -> Option<&Row> {
        self.guard.as_deref()
    }
}

impl<Row> Rows<Row> for ascent::boxcar::Vec<std::sync::RwLock<Row>> {
    fn len(&self) -> usize {
        ascent::boxcar::Vec::len(self)
    }

    fn stream(&self) -> Box<dyn StreamingIterator<Item = Row> + '_> {
        Box::new(GuardedRows {
            inner: self.iter(),
            guard: None,
        })
    }
}

type CriticalSummaryRow = (FunctionId, FormalIndex, Path);
type ResolventRow = (FunctionId, FormalIndex, Path, CallTargetObject, DecisionId);
type CallTargetAssignLikeRow = (FunctionId, FlowVariable, Path, CallTargetObject);
type ContextAssignRow = (
    FunctionId,
    FlowVariable,
    Path,
    FlowVariable,
    Path,
    DecisionSet,
);
type ContextLocalsRow = (
    FunctionId,
    FlowVariable,
    Path,
    FormalIndex,
    Path,
    DecisionSet,
);
type ContextSummaryRow = (
    FunctionId,
    FormalIndex,
    Path,
    FormalIndex,
    Path,
    DecisionSet,
);

/// Where the contextual rows are, for the debug log: per function, the rows of
/// `context_locals`, the decisions reaching it (`resolvent`), the distinct decision sets its
/// rows hold and the sum of their sizes -- the last is what a run keyed by decision instead of
/// by decision set would hold as rows. The top functions by row count are listed.
fn context_histogram(
    context_locals: &dyn Rows<ContextLocalsRow>,
    resolvent: &dyn Rows<ResolventRow>,
    id_map: Option<&IdMap>,
) -> String {
    use std::fmt::Write as _;
    #[derive(Default)]
    struct Per {
        rows: usize,
        memberships: usize,
        max_set: usize,
        sets: hashbrown::HashSet<DecisionSet>,
        decisions: usize,
        top: usize,
    }
    let mut per: HashMap<FunctionId, Per> = HashMap::new();
    let mut all_sets: hashbrown::HashSet<DecisionSet> = hashbrown::HashSet::new();
    let mut rows = context_locals.stream();
    while let Some((f, _, _, _, _, ds)) = rows.next() {
        let e = per.entry(*f).or_default();
        e.rows += 1;
        e.memberships += ds.len();
        e.max_set = e.max_set.max(ds.len());
        e.top += usize::from(ds.is_top());
        e.sets.insert(*ds);
        all_sets.insert(*ds);
    }
    let mut rows = resolvent.stream();
    while let Some((f, _, _, _, _)) = rows.next() {
        per.entry(*f).or_default().decisions += 1;
    }

    let total_rows: usize = per.values().map(|p| p.rows).sum();
    let total_memberships: usize = per.values().map(|p| p.memberships).sum();
    let total_set_elements: usize = all_sets.iter().map(|s| s.len()).sum();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "context_locals by function: rows={} memberships={} (rows a per-decision keying would hold) \
         distinct sets={} (elements {}) functions={}; {}",
        total_rows,
        total_memberships,
        all_sets.len(),
        total_set_elements,
        per.values().filter(|p| p.rows > 0).count(),
        decision::stats()
    );
    let mut top: Vec<(&FunctionId, &Per)> = per.iter().filter(|(_, p)| p.rows > 0).collect();
    top.sort_by_key(|(_, p)| std::cmp::Reverse(p.rows));
    for (f, p) in top.into_iter().take(12) {
        let name = id_map
            .and_then(|m| m.get_function(*f))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");
        let _ = writeln!(
            out,
            "  rows={:>9} top={:>9} memberships={:>10} sets={:>6} max_set={:>5} decisions={:>5} {}({})",
            p.rows,
            p.top,
            p.memberships,
            p.sets.len(),
            p.max_set,
            p.decisions,
            name,
            f.id
        );
    }
    out
}

/// Sizing for `NEXT-STEPS.md` item 5: contextual flows that would need a contextual edge to
/// compose with a contextual local. Rules 3.3a/3.3b compose `context_assign` only with the
/// context-free `locals`, and `context_locals` only with the context-free `assign_like`, so a
/// `context_locals` row that sits at the source vertex of a `context_assign` edge and has no
/// context-free `locals` twin is a composition the engine never derives. Counts those rows, split
/// by whether the row's set and the edge's set share a decision (`shared`: the composition would
/// hold under the intersection, a sound single-decision key) or not (`disjoint`: it needs a
/// conjunction of decisions). Exact-split matches only, so a lower bound on the wild/offset cases.
/// An exact `(f, v, p, a, p4)` probe of the `locals` store, serial or concurrent.
trait LocalsProbe {
    fn has(&self, f: &FunctionId, v: &FlowVariable, p: &Path, a: &FormalIndex, p4: &Path) -> bool;
}
impl LocalsProbe
    for locals_trie::LocalsIndCommon<FunctionId, FlowVariable, Path, FormalIndex, Path>
{
    fn has(&self, f: &FunctionId, v: &FlowVariable, p: &Path, a: &FormalIndex, p4: &Path) -> bool {
        self.contains(f, v, p, a, p4)
    }
}
impl LocalsProbe
    for c_locals_trie::CLocalsIndCommon<FunctionId, FlowVariable, Path, FormalIndex, Path>
{
    fn has(&self, f: &FunctionId, v: &FlowVariable, p: &Path, a: &FormalIndex, p4: &Path) -> bool {
        self.contains(f, v, p, a, p4)
    }
}

fn dropped_compositions(
    context_assign: &dyn Rows<ContextAssignRow>,
    context_locals: &dyn Rows<ContextLocalsRow>,
    locals: &dyn LocalsProbe,
    path_set: &PathSet,
    id_map: Option<&IdMap>,
) -> String {
    use std::fmt::Write as _;
    // Source vertex of every contextual edge, with the union of the sets the edges hold under.
    let mut edge_src: HashMap<(FunctionId, FlowVariable, Path), DecisionSet> = HashMap::new();
    let mut rows = context_assign.stream();
    while let Some((f, _, _, v2, p2, ds)) = rows.next() {
        let e = edge_src
            .entry((*f, *v2, *p2))
            .or_insert_with(DecisionSet::empty);
        *e = e.union(*ds);
    }
    #[derive(Default)]
    struct Per {
        rows: usize,
        shared: usize,
        disjoint: usize,
        vertices: hashbrown::HashSet<(FlowVariable, Path)>,
    }
    let mut per: HashMap<FunctionId, Per> = HashMap::new();
    let mut at_edge = 0usize;
    let mut rows = context_locals.stream();
    while let Some((f, v, p, a, p4, ds)) = rows.next() {
        let mut hit: Option<DecisionSet> = None;
        for (key, _) in &path_set.splits(p).exact {
            if let Some(e) = edge_src.get(&(*f, *v, *key)) {
                hit = Some(hit.map_or(*e, |h| h.union(*e)));
            }
        }
        let Some(e) = hit else { continue };
        at_edge += 1;
        if locals.has(f, v, p, a, p4) {
            continue;
        }
        let per = per.entry(*f).or_default();
        per.rows += 1;
        if ds.intersection(e).is_empty() {
            per.disjoint += 1;
        } else {
            per.shared += 1;
        }
        per.vertices.insert((*v, *p));
    }
    let rows: usize = per.values().map(|p| p.rows).sum();
    let shared: usize = per.values().map(|p| p.shared).sum();
    let disjoint: usize = per.values().map(|p| p.disjoint).sum();
    let vertices: usize = per.values().map(|p| p.vertices.len()).sum();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "dropped compositions (context_locals rows at a context_assign source with no context-free \
         locals twin): rows={} shared={} disjoint={} vertices={} functions={}; context_locals rows \
         at a context_assign source={}; edge sources={}",
        rows,
        shared,
        disjoint,
        vertices,
        per.len(),
        at_edge,
        edge_src.len()
    );
    let mut top: Vec<(&FunctionId, &Per)> = per.iter().collect();
    top.sort_by_key(|(_, p)| std::cmp::Reverse(p.rows));
    for (f, p) in top.into_iter().take(12) {
        let name = id_map
            .and_then(|m| m.get_function(*f))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");
        let _ = writeln!(
            out,
            "  rows={:>8} shared={:>8} disjoint={:>8} vertices={:>6} {}({})",
            p.rows,
            p.shared,
            p.disjoint,
            p.vertices.len(),
            name,
            f.id
        );
    }
    out
}

struct HybridInliningRelations<'a> {
    critical_summary: &'a dyn Rows<CriticalSummaryRow>,
    resolvent: &'a dyn Rows<ResolventRow>,
    call_target_assign_like: &'a dyn Rows<CallTargetAssignLikeRow>,
    context_assign: &'a dyn Rows<ContextAssignRow>,
    context_locals: &'a dyn Rows<ContextLocalsRow>,
    context_summary: &'a dyn Rows<ContextSummaryRow>,
    id_map: Option<&'a IdMap>,
}

impl std::fmt::Display for HybridInliningRelations<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Critical Summary ({}):", self.critical_summary.len())?;
        let mut rows = self.critical_summary.stream();
        while let Some((func_id, formal_index, path)) = rows.next() {
            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {}({}): arg{} {}",
                func_name,
                func_id.id,
                formal_index,
                path.to_dot_string(),
            )?;
        }

        writeln!(f, "\nResolvent ({}):", self.resolvent.len())?;
        let mut rows = self.resolvent.stream();
        while let Some((func_id, formal_index, path, resolvent, _)) = rows.next() {
            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {}({}): arg{} {} resolves to {}",
                func_name,
                func_id.id,
                formal_index,
                path.to_dot_string(),
                resolvent
            )?;
        }

        writeln!(
            f,
            "\nCall Target Assign-Like ({}):",
            self.call_target_assign_like.len()
        )?;
        let mut rows = self.call_target_assign_like.stream();
        while let Some((func_id, var, path, tgt)) = rows.next() {
            let var_str = if let Some(name) = var.as_local() {
                name.to_string()
            } else {
                format!("{}", var)
            };

            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");
            let tgt_str = match tgt {
                CallTargetObject::FunctionId(tgt) => {
                    let tgt_name = self
                        .id_map
                        .and_then(|m| m.get_function(*tgt))
                        .map(|f| f.0.as_ref())
                        .unwrap_or("unknown");
                    format!("ptr {}({})", tgt_name, tgt.id)
                }
                CallTargetObject::Symbol(cls) => format!("java<{cls}>"),
                CallTargetObject::LuaClass(cls) => format!("lua<{cls}>"),
            };

            writeln!(
                f,
                "  {}({}): {}{} = {}",
                func_name,
                func_id.id,
                var_str,
                path.to_dot_string(),
                tgt_str
            )?;
        }

        writeln!(f, "\nContext Assign ({}):", self.context_assign.len())?;
        let mut rows = self.context_assign.stream();
        while let Some((func_id, dest_var, dest_path, src_var, src_path, ds)) = rows.next() {
            let cs = ds.to_string();
            let dest_str = {
                let var_str = if let Some(name) = dest_var.as_local() {
                    name.to_string()
                } else {
                    format!("{}", dest_var)
                };
                format!("{}{}", var_str, dest_path.to_dot_string())
            };
            let src_str = {
                let var_str = if let Some(name) = src_var.as_local() {
                    name.to_string()
                } else {
                    format!("{}", src_var)
                };
                format!("{}{}", var_str, src_path.to_dot_string())
            };

            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {} {}({}): {} = {}",
                cs, func_name, func_id.id, dest_str, src_str
            )?;
        }

        writeln!(f, "\nContext Locals ({}):", self.context_locals.len())?;
        let mut rows = self.context_locals.stream();
        while let Some((func_id, var, path, formal_idx, formal_path, ds)) = rows.next() {
            let cs = ds.to_string();
            let var_str = if let Some(name) = var.as_local() {
                name.to_string()
            } else {
                format!("{}", var)
            };

            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {} {}({}): {}{} from arg{}{}",
                cs,
                func_name,
                func_id.id,
                var_str,
                path.to_dot_string(),
                formal_idx,
                formal_path.to_dot_string()
            )?;
        }

        writeln!(f, "\nContext Summary ({}):", self.context_summary.len())?;
        let mut rows = self.context_summary.stream();
        while let Some((func_id, dest_idx, dest_path, src_idx, src_path, ds)) = rows.next() {
            let cs = ds.to_string();
            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {} {}({}): arg{}{} = arg{}{}",
                cs,
                func_name,
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

/// Returns a FlowVariable for the call argument from the instruction side ID and the formal number
macro_rules! call_arg {
    ($insn:expr, $n:expr) => {
        crate::facts::FlowVariable::call_arg_packed(
            crate::facts::PackedCallArg::try_from_parts($insn, $n).unwrap(),
        )
    };
}

/// Creates a data flow graph for taint analysis.
/// Reads this process's current physical footprint (macOS `phys_footprint`, the same number
/// Activity Monitor's "Memory" column and `footprint(1)` report) in MB. Used for the
/// `[mem cp]` derivation checkpoints so we can attribute the pre-fixpoint memory spike to the
/// individual transient buffers in-process (an external sampler can't see sub-second phases).
/// Returns -1.0 on non-macOS or on error.
#[cfg(target_os = "macos")]
pub(crate) fn phys_footprint_mb() -> f64 {
    // SAFETY: `proc_pid_rusage` fills a caller-provided `rusage_info_v2` for our own pid. The C
    // API takes `rusage_info_t *` (== `void **`); per the header contract we pass our struct
    // pointer reinterpret-cast to that type (NOT the address of a local `void*` — that double
    // indirection writes past the stack and leaves the struct zeroed).
    unsafe {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr() as *mut libc::rusage_info_t,
        );
        if rc == 0 {
            info.assume_init().ri_phys_footprint as f64 / (1024.0 * 1024.0)
        } else {
            -1.0
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn phys_footprint_mb() -> f64 {
    -1.0
}

pub fn taint_index(facts: IndexFacts) -> IndexResult {
    taint_index_with_config(facts, IndexConfig::default(), None)
}

/// Computes `alias_of_formal`: the whole-variable copy-closure of the formals over ORIGINAL program
/// copies. A variable `v` aliases formal `i` (in function `f`) when `v` is reachable FROM `f`'s
/// formal `i`, following whole-variable copies (`copy_edge`) present in the original program in the
/// direction of assignment only. The closure is inclusion-based (Andersen-style), not
/// unification-based (Steensgaard-style): a copy `dst = src` lets `dst` inherit `src`'s
/// formal-aliases and never the reverse, so a formal's alias set only ever grows downstream of the
/// formal itself.
///
/// This depends only on `formal_param` and `copy_edge`, so it is computed in its own small fixpoint
/// BEFORE the main ascent. Doing so keeps `copy_edge` out of the main engine entirely (one fewer
/// relation + index there) and lets the aliasing summary rule consume `alias_of_formal` as a plain
/// pre-populated input.
fn compute_alias_of_formal(
    formal_param: &[(FunctionId, FlowVariable, FormalType)],
    mut copy_edge: Vec<(FunctionId, FlowVariable, FlowVariable)>,
) -> Vec<(FunctionId, FlowVariable, FormalIndex)> {
    // Base case: a formal whole-aliases itself.
    let alias_base: Vec<_> = formal_param
        .iter()
        .filter_map(|(infunc, v1, _)| v1.as_formal().map(|i| (*infunc, *v1, i)))
        .collect();
    // Keep only the copies that flow AWAY from a formal.
    let before = copy_edge.len();
    copy_edge.retain(|(_, dst, _)| dst.as_formal().is_none());
    log::debug!(
        "alias_of_formal: dropped {} formal-destination copy edges ({} of {} retained)",
        before - copy_edge.len(),
        copy_edge.len(),
        before
    );
    let pre = ascent_run! {
        relation alias_of_formal(FunctionId, FlowVariable, FormalIndex) = alias_base;
        relation copy_edge(FunctionId, FlowVariable, FlowVariable) = copy_edge;
        // A destination of an original copy inherits the source's formal-aliases. Recursive
        // relation FIRST so the join drives by the `alias_of_formal` delta and probes `copy_edge`
        // via its `0_2` index, rather than re-scanning all of `copy_edge` each iteration.
        alias_of_formal(infunc, dst, i) <--
            alias_of_formal(infunc, src, i),
            copy_edge(infunc, dst, src);
    };
    pre.alias_of_formal
}

/// Computes `paths`, the admissible access paths: the syntactic program paths, the paths the
/// input summaries mention, and each one-level concatenation of the two. Its own small
/// fixpoint, run BEFORE the main ascent, so the main program takes `paths` as a plain input
/// and can also hold it as a [`PathSet`] for lookups that never build a path.
fn compute_paths(program_paths: Vec<(Path,)>, model_paths: Vec<(Path,)>) -> Vec<(Path,)> {
    let pre = ascent_run! {
        relation program_paths(Path) = program_paths;
        relation model_paths(Path) = model_paths;
        relation paths(Path);
        paths(p) <-- program_paths(p);
        paths(p) <-- model_paths(p);
        // Combine model paths with program paths (one level only to ensure termination)
        paths(p1.concat(p2)) <-- model_paths(p1), program_paths(p2);
        paths(p2.concat(p1)) <-- program_paths(p2), model_paths(p1);
    };
    pre.paths
}

ascent_source! {
    /// The index datalog: every relation and rule of the index engine, written exactly once.
    index_rules:
    // Facts:

    relation formal_param(FunctionId, FlowVariable, FormalType);
    relation actual_param(PackedInsnSiteId, FormalIndex, FlowVertex);
    relation call(FunctionId, InsnId, FunctionId);
    // A call target stored at a vertex: a function pointer (`v = ptr<function_id>`,
    // `CallTargetObject::FunctionId`) or a Java object (`x = new Foo()`,
    // `CallTargetObject::Symbol`).
    relation call_target_assign(FunctionId, FlowVertex, CallTargetObject);
    relation callee_info(FunctionId, InsnId, FlowVariable, Path, CallDispatchKey);
    relation callee_resolvents(CallTargetObject, CallDispatchKey, FunctionId);

    // Analysis drivers:

    // Set of syntactic access paths. Computed before the run by `compute_paths`; `path_set`
    // holds the same set as a [`PathSet`], for the rules that test a concatenation.
    relation paths(Path);
    relation path_set(PathSetRef);
    relation summary(FunctionId, FormalIndex, Path, FormalIndex, Path);
    relation config(IndexConfig);

    // Derived:

    // Local reachability. The core, most expensive relation.
    #[ds($crate::index_engine::locals_trie)]
    relation locals(FunctionId, FlowVariable, Path, FormalIndex, Path);
    #[ds($crate::index_engine::assign_like_trie)]
    relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path);
    // Real program field-stores (`v.p = ...`, non-empty destination path). Gates the aliasing rule.
    relation prog_store(FunctionId, FlowVariable, Path);
    // A variable that whole-aliases a formal purely through original program copies. This is
    // the copy-closure of `locals(v, empty, formal, empty)` restricted to original assigns,
    // followed only in the direction of assignment: it finds strictly fewer aliases (drops
    // inter-procedural / summary-derived copies, and the `ParamFlow` write-backs onto the formal
    // node that would make the closure unification-like) but every alias reachable forward from a
    // formal through original program assignments. Feeds the aliasing summary rule.
    // Precomputed above in its own fixpoint (see `compute_alias_of_formal`).
    relation alias_of_formal(FunctionId, FlowVariable, FormalIndex);
    // Call targets (function pointers and Java objects) propagated across `assign_like`
    // to the receiver vertices of critical calls; the union of what were the separate
    // `func_ptr_assign_like` and `java_obj_assign_like` relations.
    relation call_target_assign_like(FunctionId, FlowVariable, Path, CallTargetObject);

    // Hybrid Inlining relations: critical_summary(f, n, p). f(n.p = obj) invokes obj at some
    // critical call site. The critical site itself is not tracked here; it can be recovered by
    // the Contextual Assignment rule.
    relation critical_summary(FunctionId, FormalIndex, Path);
    // Critical call occurs inside this function
    relation critical_call(FunctionId);
    // A decision at a function: its formal `n.p` holds the call target, put there by some
    // caller (2.1) or handed down from a caller's own decision (2.2). One row per decision,
    // however many routes establish it; the last column is the decision's interned id. Derived
    // from the two `establishes_*` relations below, which keep the one fact about the route
    // rule 3.2 needs: which caller, at which site, and under which of its own decisions.
    relation resolvent(FunctionId, FormalIndex, Path, CallTargetObject, DecisionId);
    // `caller` establishes decision `d` at `f` through its call site `insn` by holding the
    // target itself (2.1's shape) ...
    relation establishes_direct(FunctionId, DecisionId, FunctionId, InsnId);
    // ... or by passing its own formal, on which it holds decision `up` (2.2's shape). Two
    // relations rather than an `Option` column: a literal in a clause would make Ascent index
    // that column alone.
    relation establishes_via(FunctionId, DecisionId, FunctionId, InsnId, DecisionId);
    // Assignment due to instantiating a summary at a critical site. The lattice column is the
    // set of decisions the edge holds under: a seed (3.1) holds under the one decision that
    // resolved the site, an inherited summary (3.2) under the caller's own decision, and rows
    // that several decisions produce hold their union. One row per edge.
    lattice context_assign(FunctionId, FlowVariable, Path, FlowVariable, Path, DecisionSet);
    // Context-carrying field-sensitive reachability, `(f, v, p, a, p4)` holds under every
    // decision in its set. Seeded from `context_assign` and propagated by the forward-field
    // rules below, which carry the set along and union it where flows meet; so the relation
    // has one row per `(f, v, p, a, p4)` -- at most the context-free `locals` of a run that
    // instantiated every decision unconditionally -- whatever the number of decisions reaching
    // `f`. Rare on C targets (0 rows when there is no resolvable indirect/virtual dispatch).
    lattice context_locals(FunctionId, FlowVariable, Path, FormalIndex, Path, DecisionSet);
    // A function's summary row that holds only under the decisions in its set. One row per
    // summary row, like `context_locals`: unfolding the set per decision would multiply the
    // rows by the decisions reaching the function (8 M rows on xbot, for 75 k summary rows).
    lattice context_summary(FunctionId, FormalIndex, Path, FormalIndex, Path, DecisionSet);

    // Initialize locals with formals (context-free)
    locals(infunc, v1, p1.clone(), i, p1.clone()) <--
        formal_param(infunc, v1, _),
        if let Some(i) = v1.as_formal(),
        let p1 = Path::empty();

    // Forward field propagation (context-free), as exact-key joins.
    //
    // A local propagation step extends a reachability row `v2.p23 <- a.p4` across an edge
    // `v1.p1 = v2.p2` whose source path `p2` is a prefix of `p23`, deriving `v1.(p1·rest)` for
    // the `rest` after the prefix -- or, in the other direction, extends the formal side when
    // the edge reads a longer path than the row holds. Joining that on `(f, v2)` alone and
    // testing the prefix afterwards visits every edge at the vertex for every row at it: on a
    // dense function that is hundreds of pairs per row, a derived path allocated for a tenth
    // of them and a tenth of those admissible -- billions of pairs per iteration for millions
    // of rows, and the fixpoint never arrives. So every path is split ONCE at each point it
    // could match a prefix, the join is keyed on the split, and the extension is tested by
    // lookup (`path_set`) rather than by building it. Every retrieved pair matches, and nothing
    // is allocated for a pair that fails.
    //
    // A prefix may end in an offset that matches any offset (`match_prefix`'s arithmetic on its
    // last component), so each side carries a `wild` key beside its exact keys: the exact key
    // `.x.[4]` and the wild key `(.x, [4]·rest)`, matched by an edge path `.x.[m]` for `m != 4`
    // with the suffix adjusted by `4 - m`. `Path::prefix_keys` spells out both.

    // The distinct `(f, v, p)` reached in either half, context-free or contextual, so the
    // splits below run once per path, not once per row (rows outnumber paths thirty to one on
    // a dense binary). The contextual closure (3.3a) walks the same expanded edges, so its
    // paths must be keyed too.
    relation reach_vp(FunctionId, FlowVariable, Path);
    reach_vp(f, v, p) <-- locals(f, v, p, _, _);
    reach_vp(f, v, p) <-- context_locals(f, v, p, _, _, _);
    // The exact and wild keys of a `locals` path. Two relations rather than a flag column: a
    // literal in a clause makes Ascent index that column alone, and the planner then drives
    // whole-relation scans off a two-key index.
    // Held in the `locals` store: same shape, `(f, v, key)` probed exactly, `(rest, p)` leaves.
    #[ds($crate::index_engine::locals_trie)]
    relation locals_key(FunctionId, FlowVariable, Path, Path, Path);
    locals_key(f, v, key, rest, p) <--
        reach_vp(f, v, p),
        path_set(ps),
        for (key, rest) in &ps.splits(p).exact;
    relation locals_key_wild(FunctionId, FlowVariable, Path, Path, Path);
    locals_key_wild(f, v, key, rest, p) <--
        reach_vp(f, v, p),
        path_set(ps),
        for (key, rest) in &ps.splits(p).wild;
    // A `locals` path ending in an offset, keyed without it, for the wild match.
    relation locals_wild(FunctionId, FlowVariable, Path, i64, Path);
    locals_wild(f, v, key, m, p) <--
        reach_vp(f, v, p),
        if let Some((key, m)) = p.split_trailing_offset();
    // An edge whose source path ends in an offset, keyed without it, for the wild match.
    relation assign_wild(FunctionId, FlowVariable, Path, i64, FlowVariable, Path);
    assign_wild(f, v2, key, m, v1, p1) <--
        assign_like(f, v1, p1, v2, p2),
        if let Some((key, m)) = p2.split_trailing_offset();
    // The exact and wild keys of an edge's source path. The destination vertex rides as one
    // tuple column so that the relation fits the `locals` store's five-column shape.
    #[ds($crate::index_engine::locals_trie)]
    relation edge_split(FunctionId, FlowVariable, Path, Path, (FlowVariable, Path));
    edge_split(f, v2, key, rest, (*v1, *p1)) <--
        assign_like(f, v1, p1, v2, p2),
        path_set(ps),
        for (key, rest) in &ps.splits(p2).exact;
    relation edge_split_wild(FunctionId, FlowVariable, Path, Path, FlowVariable, Path);
    edge_split_wild(f, v2, key, rest, v1, p1) <--
        assign_like(f, v1, p1, v2, p2),
        path_set(ps),
        for (key, rest) in &ps.splits(p2).wild;

    // Destination side: `v1.p1 = v2.p2` and a `locals` path `p23 = p2·rest` give `v1.(p1·rest)`
    // whatever `v2.p23` has. `ext_dst` is that expanded edge, derived once per (edge, path)
    // rather than once per `locals` row, so the reachability step below is an exact join.
    // Held in the `assign_like` store: it is an edge, probed exactly by `(f, v2, p23)`.
    #[ds($crate::index_engine::assign_like_trie)]
    relation ext_dst(FunctionId, FlowVariable, Path, FlowVariable, Path);
    ext_dst(f, v1, p13, v2, p23) <--
        locals_key(f, v2, key, rest, p23),
        assign_like(f, v1, p1, v2, key),
        path_set(ps),
        if let Some(p13) = ps.concat(p1, None, rest);
    ext_dst(f, v1, p13, v2, p23) <--
        locals_key_wild(f, v2, key, rest, p23),
        assign_wild(f, v2, key, m, v1, p1),
        if let Some(n) = rest.head_offset(),
        if n != *m,
        path_set(ps),
        if let Some(p13) = ps.concat(p1, Some(n - *m), &rest.tail());
    locals(f, v1, p13, a, p4) <--
        ext_dst(f, v1, p13, v2, p23),
        locals(f, v2, p23, a, p4);

    // Formal side: `v1.p1 = v2.p23` with `p23 = p2·rest` and `v2.p2` reached from `a.p4` give
    // `v1.p1` reached from `a.(p4·rest)`. The split is on the edge's source path, and the
    // `locals` probe is exact.
    locals(f, v1, p1, a, p43) <--
        edge_split(f, v2, key, rest, dst),
        locals(f, v2, key, a, p4),
        let (v1, p1) = dst,
        path_set(ps),
        if let Some(p43) = ps.concat(p4, None, rest);
    // The wild half: the `locals` path is `key.[m]`, the edge reads `key.[n]·tail`.
    relation ext_fml(FunctionId, FlowVariable, Path, FlowVariable, Path, i64, Path);
    ext_fml(f, v1, p1, v2, p2, n - *m, rest.tail()) <--
        edge_split_wild(f, v2, key, rest, v1, p1),
        locals_wild(f, v2, key, m, p2),
        if let Some(n) = rest.head_offset(),
        if n != *m;
    locals(f, v1, p1, a, p43) <--
        ext_fml(f, v1, p1, v2, p2, adj, rest),
        locals(f, v2, p2, a, p4),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, Some(*adj), rest);

    // Compute assignments from call sites
    assign_like(func_id, v.clone(), p, cv.clone(), Path::empty()),
    assign_like(func_id, cv.clone(), Path::empty(), v.clone(), p) <--
        actual_param(call_site_slice, n, vx),
        let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**call_site_slice).unwrap(),
        let cv = call_arg!(insn_id, *n),
        let FlowVertex(v, p) = vx;

    // Compute assignments from summaries
    assign_like(func_id, v1, p1, v2, p2) <--
        summary(tgt, n1, dst_path, n2, src_path),
        call(func_id, insn_id, tgt),
        let v1 = call_arg!(*insn_id, *n1),
        let p1 = dst_path,
        let v2 = call_arg!(*insn_id, *n2),
        let p2 = src_path;

    // Compute context-free summaries from local reachability.
    summary(infunc, n1, p1, n2, p2) <--
        locals(infunc, dst_var, p1, n2, p2),
        // join with formal_param here instead of using if so that we don't have to traverse all of
        // locals
        formal_param(infunc, dst_var, formal_ty),
        if let Some(n1) = dst_var.as_formal(),
        if isout(&n1, *formal_ty, p1),
        if n1 != *n2 || p1 != p2;

    // aliasing summary rule (context-free flows only).
    // Clause order matters: `locals` FIRST so Ascent drives the join by the `locals` DELTA and
    // probes `alias_of_formal` via its `0_1` index. Writing `alias_of_formal` first instead
    // makes Ascent full-scan all of `alias_of_formal` every fixpoint iteration (a scan whose
    // per-iteration cost is |alias_of_formal|, catastrophic on binaries with many SSA copies).
    summary(infunc, n1, p1.clone(), n2, bp) <--
        // v1.p1 <- n2.bp  (delta driver)
        locals(infunc, v1, p1, n2, bp),
        if !p1.is_empty(),
        // v1.p1 is actually stored through in the program (not just reachable): membership
        // probe by (func,var,path). Restricts summaries to genuine aliased writes.
        prog_store(infunc, v1, p1),
        // this is the alias: v1 <- n1, established by original program copies only
        alias_of_formal(infunc, v1, n1),
        config(c),
        if c.alias_rule,
        if n1 != n2 || *p1 != *bp;

    // Hybrid Inlining Rules:
    // Phase 1: propagate up the stack from indirect calls
    // Phase 2: propagate resolvents back down, one row per decision and one hop of route
    // Phase 3: propagate conditional summaries up till they're unconditional

    // 1.1: Base Critical Summary. An indirect / virtual call site found. (context-free)
    critical_summary(func_id, n, p_n) <--
        callee_info(func_id, _, v, p_call, _),
        locals(func_id, v, p_call, n, p_n);

    // 1.2: Propagate Critical Summary (context-free) up the stack.
    critical_summary(caller_func_id, n, p_n) <--
        call(caller_func_id, caller_insn_id, tgt),
        critical_summary(tgt, n_tgt, p_tgt),
        let arg = call_arg!(*caller_insn_id, *n_tgt),
        locals(caller_func_id, arg, p_tgt, n, p_n);

    // 2.1: Base Resolvent. A stored call target locally reaches a critical summary, so
    // instantiate the resolvent in the parameters of the summary. The target is carried
    // opaquely as a `CallTargetObject`; its variant is only tested later at call resolution.
    establishes_direct(f, d, caller, call_insn) <--
        critical_summary(f, n, p),
        call(caller, call_insn, f),
        let arg = call_arg!(*call_insn, *n),
        call_target_assign_like(caller, arg, p, cto),
        let d = DecisionId::of(Decision { formal: *n, path: *p, target: cto.clone() });
    resolvent(f, dec.formal, dec.path, dec.target.clone(), d) <--
        establishes_direct(f, d, _, _),
        let dec = d.get();

    // Tracks resolvent to the call arg: the caller's decision `up` reaches the call arg `arg_p`
    // at path `p`.
    relation call_arg_resolvent(PackedCallArg, Path, CallTargetObject, DecisionId);
    call_arg_resolvent(arg_p, p, obj, up) <--
        resolvent(f, n2, p2, obj, up),
        locals(f, v, p, n2, p2),
        if let Some(arg_p) = v.as_call_arg();

    // 2.2: Propagate Resolvent down the critical summaries. Finite by construction: `resolvent`
    // has one row per (function, formal, path, target), and nothing about the route is kept
    // beyond the one hop `establishes_via` records.
    establishes_via(f, d, caller, arg.insn_id, up) <--
        call_arg_resolvent(arg_p, p, resolvent_obj, up),
        let arg = CallArgId::unpack_from_slice(&**arg_p).unwrap(),
        call(caller, arg.insn_id, f),
        let n = FormalIndex::new(arg.formal),
        critical_summary(f, n, p),
        let d = DecisionId::of(Decision { formal: n, path: *p, target: resolvent_obj.clone() });
    resolvent(f, dec.formal, dec.path, dec.target.clone(), d) <--
        establishes_via(f, d, _, _, _),
        let dec = d.get();

    // 3.1: Contextual Assignment (seed). Given a resolvent that reaches a call site,
    // instantiate a contextual assignment doing normal summary instantiation. The resolvent
    // object itself and the dispatch key from the call site are used to determine the resolvent
    // function. The edge holds under the one decision that resolved the site.
    context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), ds) <--
        callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
        locals(caller, v_rec, p_rec, n, p),
        resolvent(caller, n, p, resolvent_obj, d),
        callee_resolvents(resolvent_obj, dispatch_key, resolvent_func),
        config(c),
        if c.hybrid_context != HybridContext::None,
        let ds = d.singleton(c.hybrid_context == HybridContext::Collapse),
        summary(resolvent_func, n1_sum, p1_sum, n2_sum, p2_sum),
        let v2 = call_arg!(*call_insn, *n2_sum),
        let v1 = call_arg!(*call_insn, *n1_sum);

    // 3.1 without context (`HybridContext::None`): the same instantiation, as a plain edge.
    // No context is recorded, so the effect is shared by every caller that reaches the site;
    // rules 3.2-3.4 then have nothing to do. Same body shape as the local-dispatch bypass
    // below, with the target arriving through a resolvent instead of a local store.
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone()) <--
        callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
        locals(caller, v_rec, p_rec, n, p),
        resolvent(caller, n, p, resolvent_obj, _),
        callee_resolvents(resolvent_obj, dispatch_key, resolvent_func),
        summary(resolvent_func, n1_sum, p1_sum, n2_sum, p2_sum),
        config(c),
        if c.hybrid_context == HybridContext::None,
        let v2 = call_arg!(*call_insn, *n2_sum),
        let v1 = call_arg!(*call_insn, *n1_sum);

    // 3.2: apply a conditional summary at the callers whose route established its decision.
    // The two rules mirror 2.1 and 2.2, which is what makes them complete: a caller that holds
    // the target itself (2.1's shape) applies the summary unconditionally, and a caller that
    // received the target through its own formal (2.2's shape) inherits the summary conditioned
    // on that formal, and the walk continues at its callers. Every caller on every route gets
    // it, whichever was found first, so the result does not depend on iteration order.
    //
    // How the summaries are paired with the establishing calls is [`ContextJoin`]; the three
    // variants below compute the same rows.
    //
    // `Scan`: join on the callee alone and test the decision by membership. Both semi-naive
    // variants are exact probes by function, but the pairing is `|summaries(f)| x
    // |establishing calls(f)|` and the test throws most of it away.
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone()) <--
        config(c),
        if c.context_join == ContextJoin::Scan,
        context_summary(f, n1, p1_sum, n2, p2_sum, ds),
        establishes_direct(f, d, caller, insn),
        if ds.contains(*d),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), up.singleton(ds.is_collapsing())) <--
        config(c),
        if c.context_join == ContextJoin::Scan,
        context_summary(f, n1, p1_sum, n2, p2_sum, ds),
        establishes_via(f, d, caller, insn, up),
        if ds.contains(*d),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    // `Sets`: the distinct sets a function's conditional summaries hold under (sets are
    // interned and shared across rows, so there are far fewer sets than rows), each unfolded
    // once into its members. The establishing calls are then joined on `(f, d)`, exactly, and
    // the result is joined back to the summary rows on `(f, set)`, exactly. A set that grows
    // (a lattice update) re-emits its summary row into the delta, which re-derives the set's
    // row here; the stale `(f, old set)` rows stay and are harmless (they name a subset).
    // ⊤ has no members to unfold: a function whose summaries hold under ⊤ takes every
    // establishing call, through `context_summary_top`, keyed by the function alone.
    relation context_summary_set(FunctionId, DecisionSet);
    context_summary_set(f, *ds) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        context_summary(f, _, _, _, _, ds);
    relation set_member(FunctionId, DecisionId, DecisionSet);
    set_member(f, d, *ds) <--
        context_summary_set(f, ds),
        if !ds.is_top(),
        for d in ds.ids();
    relation context_summary_top(FunctionId, DecisionSet);
    context_summary_top(f, *ds) <--
        context_summary_set(f, ds),
        if ds.is_top();
    relation set_establishes_direct(FunctionId, DecisionSet, FunctionId, InsnId);
    set_establishes_direct(f, *ds, caller, insn) <--
        set_member(f, d, ds),
        establishes_direct(f, d, caller, insn);
    set_establishes_direct(f, *ds, caller, insn) <--
        context_summary_top(f, ds),
        establishes_direct(f, _, caller, insn);
    relation set_establishes_via(FunctionId, DecisionSet, FunctionId, InsnId, DecisionId);
    set_establishes_via(f, *ds, caller, insn, up) <--
        set_member(f, d, ds),
        establishes_via(f, d, caller, insn, up);
    set_establishes_via(f, *ds, caller, insn, up) <--
        context_summary_top(f, ds),
        establishes_via(f, _, caller, insn, up);
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone()) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        context_summary(f, n1, p1_sum, n2, p2_sum, ds),
        set_establishes_direct(f, ds, caller, insn),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), up.singleton(ds.is_collapsing())) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        context_summary(f, n1, p1_sum, n2, p2_sum, ds),
        set_establishes_via(f, ds, caller, insn, up),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    // `Unfold`: one row per (summary row, decision in its set), joined with the establishing
    // calls on `(f, d)`. Monotone (a set only grows), so a plain relation. ⊤ unfolds to every
    // decision at the function, through `resolvent`, gated by the small `top_summary_func`
    // so a new resolvent never scans the summaries.
    relation context_summary_d(FunctionId, DecisionId, FormalIndex, Path, FormalIndex, Path);
    context_summary_d(f, d, n1, p1, n2, p2) <--
        config(c),
        if c.context_join == ContextJoin::Unfold,
        context_summary(f, n1, p1, n2, p2, ds),
        if !ds.is_top(),
        for d in ds.ids();
    relation top_summary_func(FunctionId);
    top_summary_func(f) <--
        config(c),
        if c.context_join == ContextJoin::Unfold,
        context_summary(f, _, _, _, _, ds),
        if ds.is_top();
    context_summary_d(f, d, n1, p1, n2, p2) <--
        top_summary_func(f),
        resolvent(f, _, _, _, d),
        context_summary(f, n1, p1, n2, p2, ds),
        if ds.is_top();
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone()) <--
        context_summary_d(f, d, n1, p1_sum, n2, p2_sum),
        establishes_direct(f, d, caller, insn),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), up.singleton(c.hybrid_context == HybridContext::Collapse)) <--
        config(c),
        context_summary_d(f, d, n1, p1_sum, n2, p2_sum),
        establishes_via(f, d, caller, insn, up),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    // 3.3a: We have to reason about contextual local reachability. This involves reasoning
    // about flows composed of hops, some of which are non-contextual and some of which are
    // contextual. The following rules extend contextual flows with built-in assigns, walking
    // the same expanded edges (`ext_dst`, `edge_split`, `ext_fml`) as the context-free closure
    // — `reach_vp` keys the contextual paths too — so every join here is exact. The decision
    // set rides along unchanged; the lattice unions it into the row it lands on.
    context_locals(f, v1, p13, a, p4, *ds) <--
        ext_dst(f, v1, p13, v2, p23),
        context_locals(f, v2, p23, a, p4, ds),
        if !decision::spills(*ds);
    context_locals(f, v1, p1, a, p43, *ds) <--
        edge_split(f, v2, key, rest, dst),
        context_locals(f, v2, key, a, p4, ds),
        if !decision::spills(*ds),
        let (v1, p1) = dst,
        path_set(ps),
        if let Some(p43) = ps.concat(p4, None, rest);
    context_locals(f, v1, p1, a, p43, *ds) <--
        ext_fml(f, v1, p1, v2, p2, adj, rest),
        context_locals(f, v2, p2, a, p4, ds),
        if !decision::spills(*ds),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, Some(*adj), rest);

    // 3.3b: The following rules extend non-contextual flows with contextual assigns. A
    // contextual assign is an edge, so it gets the same keys an `assign_like` edge gets, with
    // its decision set riding along as a lattice (so a set that grows updates the split rather
    // than adding a stale twin), and the `locals` probes are exact.
    lattice ctx_edge_wild(FunctionId, FlowVariable, Path, i64, FlowVariable, Path, DecisionSet);
    ctx_edge_wild(f, v2, key, m, v1, p1, *ds) <--
        context_assign(f, v1, p1, v2, p2, ds),
        if !decision::spills(*ds),
        if let Some((key, m)) = p2.split_trailing_offset();
    lattice ctx_edge_split(FunctionId, FlowVariable, Path, Path, FlowVariable, Path, DecisionSet);
    ctx_edge_split(f, v2, key, rest, v1, p1, *ds) <--
        context_assign(f, v1, p1, v2, p2, ds),
        if !decision::spills(*ds),
        path_set(ps),
        for (key, rest) in &ps.splits(p2).exact;
    lattice ctx_edge_split_wild(FunctionId, FlowVariable, Path, Path, FlowVariable, Path, DecisionSet);
    ctx_edge_split_wild(f, v2, key, rest, v1, p1, *ds) <--
        context_assign(f, v1, p1, v2, p2, ds),
        if !decision::spills(*ds),
        path_set(ps),
        for (key, rest) in &ps.splits(p2).wild;
    lattice ctx_ext_dst(FunctionId, FlowVariable, Path, FlowVariable, Path, DecisionSet);
    ctx_ext_dst(f, v1, p13, v2, p23, *ds) <--
        locals_key(f, v2, key, rest, p23),
        context_assign(f, v1, p1, v2, key, ds),
        if !decision::spills(*ds),
        path_set(ps),
        if let Some(p13) = ps.concat(p1, None, rest);
    ctx_ext_dst(f, v1, p13, v2, p23, *ds) <--
        locals_key_wild(f, v2, key, rest, p23),
        ctx_edge_wild(f, v2, key, m, v1, p1, ds),
        if let Some(n) = rest.head_offset(),
        if n != *m,
        path_set(ps),
        if let Some(p13) = ps.concat(p1, Some(n - *m), &rest.tail());
    lattice ctx_ext_fml(FunctionId, FlowVariable, Path, FlowVariable, Path, i64, Path, DecisionSet);
    ctx_ext_fml(f, v1, p1, v2, p2, n - *m, rest.tail(), *ds) <--
        ctx_edge_split_wild(f, v2, key, rest, v1, p1, ds),
        locals_wild(f, v2, key, m, p2),
        if let Some(n) = rest.head_offset(),
        if n != *m;
    context_locals(f, v1, p13, a, p4, *ds) <--
        ctx_ext_dst(f, v1, p13, v2, p23, ds),
        locals(f, v2, p23, a, p4);
    context_locals(f, v1, p1, a, p43, *ds) <--
        ctx_edge_split(f, v2, key, rest, v1, p1, ds),
        locals(f, v2, key, a, p4),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, None, rest);
    context_locals(f, v1, p1, a, p43, *ds) <--
        ctx_ext_fml(f, v1, p1, v2, p2, adj, rest, ds),
        locals(f, v2, p2, a, p4),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, Some(*adj), rest);

    // 3.4: a context-specific flow that reaches an out-formal becomes a conditional summary
    // under the decisions in its set; rule 3.2 applies it at the callers.
    context_summary(func_id, n1.clone(), p1.clone(), n2.clone(), p2.clone(), *ds) <--
        context_locals(func_id, dst_var, p1, n2, p2, ds),
        if !decision::spills(*ds),
        formal_param(func_id, dst_var, formal_ty),
        if let Some(n1) = dst_var.as_formal(),
        if isout(&n1, *formal_ty, p1),
        if n1 != *n2 || p1 != p2;

    // 3.5 (`HybridContext::Spill`): a row widened to ⊤ leaves the contextual closure and joins
    // the context-free one, which shares it with every caller through `summary`. The rules
    // above stop at a spilled row, so nothing contextual is derived from it.
    assign_like(f, v1, p1, v2, p2) <--
        context_assign(f, v1, p1, v2, p2, ds),
        if decision::spills(*ds);
    locals(f, v, p, a, p4) <--
        context_locals(f, v, p, a, p4, ds),
        if decision::spills(*ds);

    // Local virtual / indirect call and resolvent, bypassing the resolvent / summary machinery
    assign_like(func_id, v1.into(), p1, v2.into(), p2) <--
        callee_info(func_id, insn_id, arg, arg_p, dispatch_key),
        call_target_assign_like(func_id, arg, arg_p, cto),
        callee_resolvents(cto, dispatch_key, resolve_tgt),
        let call_site_id = PackedInsnSiteId::try_from_parts(*func_id, *insn_id).unwrap(),
        summary(resolve_tgt, n1, p1, n2, p2),
        let n2_id = PackedCallArg::try_from_parts(*insn_id, *n2).unwrap(),
        let n1_id = PackedCallArg::try_from_parts(*insn_id, *n1).unwrap(),
        let v2 = FlowVariableKind::CallArg(n2_id),
        let v1 = FlowVariableKind::CallArg(n1_id);

    // Functions whose tag closure we bother to compute. `critical_call` alone is too narrow:
    // a pure factory (`h = lookup()`, `Account.new`) contains no indirect call and calls
    // nothing with a critical summary, so its closure would be empty and the tag would never
    // reach its own out-formal for the return-direction rule below to pick up. A called
    // function that holds a call-target fact is exactly the shape that can export a tag.
    // `critical_call` itself keeps its narrower meaning for its other consumers.
    relation tag_closure_func(FunctionId);
    tag_closure_func(f) <-- critical_call(f);
    tag_closure_func(f) <-- call_target_assign(f, _, _), call(_, _, f);

    // Call Target Propagation (function pointers and Java objects alike). The stored
    // target is carried opaquely as a `CallTargetObject`; the variant is only tested
    // downstream, where the call is actually resolved.
    call_target_assign_like(func_id, v.clone(), p.clone(), tgt) <--
        call_target_assign(func_id, vx, tgt), let FlowVertex(v, p) = vx,
        tag_closure_func(func_id);

    call_target_assign_like(func_id, v1.clone(), p_new.clone(), tgt) <--
        // This results in large reduction on some test cases
        tag_closure_func(func_id),
        call_target_assign_like(func_id, v2, p_context, tgt),
        assign_like(func_id, v1, p1, v2, p2),
        if let Some(p_new) = p_context.substitute_prefix(p2, p1),
        paths(&p_new);

    // Return-direction call-target propagation. Rule 2.1 pushes a caller's tag DOWN onto a
    // callee's formal; this is its missing twin, carrying a tag a callee holds on an
    // out-formal UP to the corresponding call-arg vertex in each caller. Without it a target
    // manufactured inside a callee (a returned function pointer, a returned object whose
    // concrete type drives dispatch) dies at the return boundary, and the tag only ever
    // crosses a return when the returned value is reachable from an in-formal (pass-through,
    // via `summary`).
    //
    // Clause order is load-bearing, same reason as the comment at the aliasing summary rule:
    // drive on the `call_target_assign_like` delta and probe `formal_param` by (func, var)
    // second, so we prune to tagged out-formals before fanning out over callers. Both probes
    // reuse indices existing rules already require -- `formal_param` by (func, var), `call` by
    // its target column -- so no new indices are built.
    //
    // No context is needed: the head names the specific `insn`, so each call site of a
    // factory gets its own tagged vertex; context sensitivity is inherent to this direction.
    // `p` rides through unchanged (no `substitute_prefix`), so no path growth and no `paths`
    // gate. `isout` holds for every negative formal index, so RETURN_INDEX and the multi-return
    // slots all qualify, and by-ref out-params (a callee installing a target into a
    // caller-owned object) fall out for free.
    //
    // Deliberately NOT seeding `resolvent` here: that is a callee-frame relation keyed on the
    // callee's formals, so a tuple for a frame whose receiver is a local has no consumer, and
    // it would bypass rule 2.1's record of the establishing site. Downstream needs no
    // change -- the transitive rule above walks this tuple from `call_arg(insn, -1)` to the
    // receiver over the symmetric call-site `assign_like` edges, the local-dispatch bypass
    // resolves the indirect call exactly, and if the receiver is passed onward rule 2.1
    // derives the resolvent with its establishing site recorded.
    call_target_assign_like(caller, cv, p.clone(), tgt) <--
        call_target_assign_like(callee, v, p, tgt),
        formal_param(callee, v, formal_ty),
        if let Some(n) = v.as_formal(),
        if isout(&n, *formal_ty, p),
        call(caller, insn, callee),
        critical_call(caller),
        let cv = call_arg!(*insn, n);

    critical_call(func_id) <-- callee_info(func_id, _, _, _, _);
    critical_call(func_id) <--
        critical_summary(tgt, _, _),
        call(func_id, _, tgt);
}

// The `ascent!` datalog block below expands to code that trips several style lints
// (`.clone()` on Copy types, auto-borrows, unit-valued lets, and Default field
// reassignment). These are artifacts of the macro's generated code, not the
// hand-written rules, so silence them for this function.
#[allow(
    clippy::clone_on_copy,
    clippy::needless_borrow,
    clippy::let_unit_value,
    clippy::field_reassign_with_default
)]
pub fn taint_index_with_config(
    facts: IndexFacts,
    config: IndexConfig,
    id_map: Option<&IdMap>,
) -> IndexResult {
    let parallelism = config.parallelism;
    // The widening bound is process-global (sets are interned process-wide); one kind of set
    // per run.
    decision::set_widen_bound(match config.hybrid_context {
        HybridContext::Bounded(k) | HybridContext::Spill(k) => k,
        _ => 0,
    });
    decision::set_spill(matches!(config.hybrid_context, HybridContext::Spill(_)));
    use hashbrown::hash_set::HashSet;
    let num_functions = facts
        .formal_param
        .iter()
        .map(|(f, _, _)| *f)
        .chain(facts.external_function.iter().map(|(f,)| *f))
        .collect::<HashSet<_>>()
        .len();

    let num_variables = facts
        .formal_param
        .iter()
        .map(|(f, v, _)| (*f, *v))
        .chain(facts.assign.iter().flat_map(|(site, v1, v2)| {
            let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&**site).unwrap();
            [(func_id, v1.0), (func_id, v2.0)]
        }))
        .collect::<HashSet<_>>()
        .len();

    let initial_assign = facts.assign.len();
    let initial_call_target_assign = facts.call_target_assign.len();
    let initial_summary = facts.summary.len();
    let initial_formals = facts.formal_param.len();

    log::debug!(
        "[mem cp] entry (facts loaded): {:.1} MB | assign={} summary={} formals={}",
        phys_footprint_mb(),
        initial_assign,
        initial_summary,
        initial_formals
    );

    // The assign-derived paths and `facts.paths` overlap heavily, so unify them
    // in a set to drop duplicates before they seed the `program_paths` relation.
    let mut program_paths: HashSet<_> = facts
        .assign
        .iter()
        .flat_map(|(_, dst, src)| std::iter::once(dst.1).chain(std::iter::once(src.1)))
        .map(|p| (p,))
        .collect();
    // Codegen's paths are syntactic program paths too, including composed ones that no single
    // `assign` edge carries (a field read through a pointer is lowered to a chain of loads through
    // temporaries, so only the per-hop paths land on edges). Dropping them under-approximates the
    // propagation gate and silently kills flows.
    program_paths.extend(facts.paths.iter().cloned());
    log::debug!(
        "[mem cp] + program_paths ({} rows): {:.1} MB",
        program_paths.len(),
        phys_footprint_mb()
    );
    // Whole-variable copies (`x = y`, both sides empty path) from the ORIGINAL program
    // assignments only -- NOT the derived `assign_like` closure (which also contains
    // inter-procedural argument copies and summary-induced edges). Used to compute
    // `alias_of_formal` for the aliasing summary rule.
    let copy_edge: Vec<_> = facts
        .assign
        .iter()
        .filter(|(_, dst, src)| dst.1.is_empty() && src.1.is_empty())
        .map(|(site, dst, src)| {
            let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&**site).unwrap();
            (func_id, dst.0, src.0)
        })
        .collect();
    log::debug!(
        "[mem cp] + copy_edge ({} rows): {:.1} MB",
        copy_edge.len(),
        phys_footprint_mb()
    );
    // Real field-stores from the ORIGINAL program: an assignment whose DESTINATION access path is
    // non-empty (`v.p = ...`). Used to gate the aliasing rule so it only summarizes aliases that are
    // actually stored through, killing spurious summaries from mere reachability.
    let prog_store: Vec<_> = facts
        .assign
        .iter()
        .filter(|(_, dst, _)| !dst.1.is_empty())
        .map(|(site, dst, _)| {
            let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&**site).unwrap();
            (func_id, dst.0, dst.1)
        })
        .collect();
    log::debug!(
        "[mem cp] + prog_store ({} rows): {:.1} MB",
        prog_store.len(),
        phys_footprint_mb()
    );
    let assign_like: Vec<_> = facts
        .assign
        .into_iter()
        .map(|(site, dst, src)| {
            let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&*site).unwrap();
            (func_id, dst.0, dst.1, src.0, src.1)
        })
        .collect();
    log::debug!(
        "[mem cp] + assign_like ({} rows, facts.assign consumed): {:.1} MB",
        assign_like.len(),
        phys_footprint_mb()
    );
    // Model paths = access paths introduced by summaries ONLY. Do NOT fold `facts.paths` in here:
    // those are program paths (already in `program_paths`), and the one-level concat rules below
    // combine `model_paths` with `program_paths`. Adding `facts.paths` makes model_paths ≈
    // program_paths, turning that concat into a program×program self-join (|facts.paths|² rows).
    let summary_paths: HashSet<_> = facts
        .summary
        .iter()
        .flat_map(|(_, _, p1, _, p2)| [(*p1,), (*p2,)])
        .collect();
    log::debug!(
        "[mem cp] + summary_paths ({} rows): {:.1} MB",
        summary_paths.len(),
        phys_footprint_mb()
    );
    let call: Vec<_> = facts
        .call
        .iter()
        .map(|(site, target)| {
            let InsnSiteId { func_id, insn_id } = InsnSiteId::unpack_from_slice(&**site).unwrap();
            (func_id, insn_id, *target)
        })
        .collect();
    let config_val = vec![(config,)];

    // Precompute `alias_of_formal` in its own small fixpoint, BEFORE the main ascent -- see
    // `compute_alias_of_formal`. This is what lets `copy_edge` stay out of the main engine.
    let alias_of_formal = compute_alias_of_formal(&facts.formal_param, copy_edge);
    log::debug!(
        "[mem cp] + alias_of_formal ({} rows), about to enter ascent_run: {:.1} MB",
        alias_of_formal.len(),
        phys_footprint_mb()
    );
    // `paths` is closed before the run (see `compute_paths`), and held twice: as the `paths`
    // relation, and as a `PathSet` for the local propagation rules' lookups.
    let mut all_program_paths = program_paths;
    all_program_paths.extend(
        facts
            .actual_param
            .iter()
            .map(|(_, _, FlowVertex(_, p))| (*p,)),
    );
    // The receiver access path of an indirect / virtual call is also a syntactic program path.
    // Registering it lets `call_target_assign_like` propagate a stored target across an SSA
    // version of the receiver (the transitive rules gate on `paths(p_new)`). Without this, a
    // second store into the same aggregate (`o.a = id; o.b = id; o.a(s)` or
    // `fps[0]=id; fps[1]=id; fps[0](s)`) creates a new receiver version whose call path was
    // never an `actual_param`, so the binding fails to reach the call and taint is dropped.
    all_program_paths.extend(
        facts
            .callee_info
            .iter()
            .map(|(_, FlowVertex(_, p), _)| (*p,)),
    );
    let paths = compute_paths(
        all_program_paths.into_iter().collect(),
        summary_paths.into_iter().collect(),
    );
    let path_set = PathSetRef(Arc::new(PathSet::from_paths(paths.iter().map(|(p,)| *p))));
    log::debug!(
        "[mem cp] + paths ({} rows), about to enter ascent_run: {:.1} MB",
        paths.len(),
        phys_footprint_mb()
    );
    log::info!("index engine: {parallelism}");

    ascent! {
        #![measure_rule_times]
        #![generate_run_timeout]
        struct IndexProg;
        include_source!(index_rules);
    }
    ascent_par! {
        #![measure_rule_times]
        #![generate_run_timeout]
        struct ParIndexProg;
        include_source!(index_rules);
    }

    // Optional wall-clock cap on the fixpoint, gated by env var so normal runs are unaffected
    // (default `Duration::MAX` == run to fixpoint, identical to the old `ascent_run!`). Setting
    // `CTADL_INDEX_TIMEOUT_SECS=<n>` stops the semi-naive loop at the first iteration boundary
    // past <n>s and returns; the accumulated per-rule / per-scc times are then logged via
    // `scc_times_summary()` below. This is the tool for profiling which rule(s) dominate a
    // non-terminating dense-regime run (e.g. the minidlna plateau) as if it hit a fixpoint.
    let index_timeout = std::env::var("CTADL_INDEX_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::MAX);

    // Everything from seeding the inputs to extracting the result is written once, here, and
    // expanded once per engine. The two program types spell their fields identically, but each
    // field's type differs by engine (`Vec` vs `boxcar::Vec`, `AssignTrie` vs `CAssignTrie`), so
    // no one function signature covers both and a trait would only restate every field. The body
    // captures this function's locals directly; the program type is its only parameter.
    //
    // Relation inputs that used to be inline `= <init>` initializers are set here instead,
    // because the declared-struct `Default::default()` where ascent would otherwise place them
    // has no access to these locals. They are `collect`ed rather than assigned, which costs
    // nothing under `ascent!` (`Vec` to `Vec` reuses the allocation) and is what `ascent_par!`
    // needs, where a plain relation's physical store is a `boxcar::Vec` (a lock-free append-only
    // vector) instead of a `std::vec::Vec`.
    macro_rules! run_engine {
        ($prog_ty:ty) => {{
            let mut prog = <$prog_ty>::default();
        prog.formal_param = facts.formal_param.into_iter().collect();
        prog.actual_param = facts.actual_param.into_iter().collect();
        prog.call = call.into_iter().collect();
        prog.call_target_assign = facts
            .call_target_assign
            .into_iter()
            .map(|(site_id, vx, obj)| {
                let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                (func_id, vx, obj)
            })
            .collect();
        prog.callee_info = facts
            .callee_info
            .into_iter()
            .map(|(site_id, vx, dispatch_key)| {
                let InsnSiteId { func_id, insn_id } = InsnSiteId::unpack_from_slice(&*site_id).unwrap();
                (func_id, insn_id, vx.0, vx.1, dispatch_key)
            })
            .collect();
        prog.callee_resolvents = facts.callee_resolvents.into_iter().collect();
        prog.summary = facts.summary.into_iter().collect();
        prog.config = config_val.into_iter().collect();
        // Seeding goes through the `FromRows` trait rather than naming a store type, so this line is
        // the same under `ascent!` and `ascent_par!`: the field's type selects the serial `AssignTrie`
        // or the concurrent `CAssignTrie` impl.
        prog.__assign_like_ind_common = FromRows::from_rows(assign_like);
        prog.prog_store = prog_store.into_iter().collect();
        prog.alias_of_formal = alias_of_formal.into_iter().collect();
        prog.paths = paths.iter().cloned().collect();
        prog.path_set = vec![(path_set.clone(),)].into_iter().collect();

        let reached_fixpoint = prog.run_timeout(index_timeout);
        if !reached_fixpoint {
            log::warn!(
                "index run TIMED OUT after {:?} without reaching fixpoint; results and stats below are PARTIAL",
                index_timeout
            );
        }
        log::debug!(
            "[mem cp] ascent_run returned (transient input buffers dropped): {:.1} MB",
            phys_footprint_mb()
        );
        log::debug!("index scc times: {}", prog.scc_times_summary());
        // Every relation's tuple count, straight from ascent's own generated summary. This is
        // the census the rule-time ranking is normalized against (time per tuple), so it has to
        // cover ALL relations, not the hand-picked few the `propagation relations:` line below
        // lists. Logged BEFORE the `assign_like` store is drained into the output Vec, so the
        // counts describe the fixpoint's state at the moment it stopped -- which, under
        // `CTADL_INDEX_TIMEOUT_SECS`, is the partial state we are trying to characterize.
        //
        // Three relations read 0 here and are reported separately on the next line: `assign_like`,
        // `locals`, `edge_split`, `locals_key` and `ext_dst` live in BYODS stores, so their
        // physical `SeedVec` relation holds no tuples.
        log::debug!(
            "[relsizes] index relation sizes:\n{}",
            prog.relation_sizes_summary()
        );
        log::debug!(
            "[relsizes] byods-backed: assign_like size: {}\nlocals size: {}\nedge_split size: {}\nlocals_key size: {}\next_dst size: {}",
            prog.__assign_like_ind_common.len(),
            prog.__locals_ind_common.len(),
            prog.__edge_split_ind_common.len(),
            prog.__locals_key_ind_common.len(),
            prog.__ext_dst_ind_common.len()
        );
        log::debug!(
            "[mem cp] after relation census (nothing drained yet): {:.1} MB",
            phys_footprint_mb()
        );
        log::debug!(
            "propagation relations: reach_vp={} locals_key={} locals_key_wild={} locals_wild={} \
             assign_wild={} edge_split={} edge_split_wild={} ext_dst={} ext_fml={}",
            prog.reach_vp.len(),
            prog.__locals_key_ind_common.len(),
            prog.locals_key_wild.len(),
            prog.locals_wild.len(),
            prog.assign_wild.len(),
            prog.__edge_split_ind_common.len(),
            prog.edge_split_wild.len(),
            prog.__ext_dst_ind_common.len(),
            prog.ext_fml.len()
        );
        // Phase-0 instrumentation: attribute the `locals` store's peak bytes to fwd vs inv.
        log::debug!("{}", prog.__locals_ind_common.heap_report());
        log::debug!("{}", prog.__assign_like_ind_common.heap_report());
        // The formatter reads these through the `Rows` trait, so this call is the same under `ascent!`
        // (plain `Vec`s) and `ascent_par!` (`boxcar::Vec`s, with lattices as
        // `boxcar::Vec<RwLock<..>>`): each field's own type selects the impl, and nothing is copied in
        // either case.
        log::trace!(
            "hybrid inlining relations:\n{}",
            HybridInliningRelations {
                critical_summary: &prog.critical_summary,
                resolvent: &prog.resolvent,
                call_target_assign_like: &prog.call_target_assign_like,
                context_assign: &prog.context_assign,
                context_locals: &prog.context_locals,
                context_summary: &prog.context_summary,
                id_map,
            }
        );

        // `assign_like` is stored in the BYODS trie (`__assign_like_ind_common`); its physical
        // relation is a `SeedVec` holding no tuples. Reconstruct the saved output Vec from the store,
        // and take the row count from the store rather than the (empty) physical relation. Take the
        // store by value so it drains (frees) as the output Vec fills — this reconstruction is the
        // run's peak, so a draining rebuild keeps the transient to ~1×.
        let assign_like_out = std::mem::take(&mut prog.__assign_like_ind_common).into_vec();

        // `locals` lives in the trie (`__locals_ind_common`); its physical relation holds no
        // tuples, so the distinct subjects come from the store's `(F, V)` outer keys — the same
        // (func, var) key `num_variables` counts, so the two are directly comparable.
        let reached_variables = prog.__locals_ind_common.num_reached_variables();

        let stats = IndexStats {
            initial_assign,
            final_assign_like: assign_like_out.len(),
            initial_formals,
            final_locals: prog.locals.len(),
            initial_call_target_assign,
            final_call_target_assign_like: prog.call_target_assign_like.len(),
            initial_summary,
            final_summary: prog.summary.len(),
            num_functions,
            num_variables,
            reached_variables,
            hybrid_critical_summary: prog.critical_summary.len(),
            hybrid_resolvent: prog.resolvent.len(),
            hybrid_context_assign: prog.context_assign.len(),
            hybrid_context_locals: prog.context_locals.len(),
            hybrid_context_summary: prog.context_summary.len(),
        };
        stats.log();
        if stats.hybrid_context_locals > 0 {
            log::debug!(
                "{}",
                context_histogram(&prog.context_locals, &prog.resolvent, id_map).trim_end()
            );
            log::debug!(
                "{}",
                dropped_compositions(
                    &prog.context_assign,
                    &prog.context_locals,
                    &prog.__locals_ind_common,
                    &path_set.0,
                    id_map
                )
                .trim_end()
            );
        }

        let result = IndexResult {
            summary: prog.summary.into_iter().collect(),
            assign_like: assign_like_out,
            call_target_assign_like: prog.call_target_assign_like.into_iter().collect(),
            paths: prog.paths.into_iter().collect(),
            external_function: facts.external_function,
            stats,
        };
            result
        }};
    }

    let result = match parallelism {
        Parallelism::Serial => run_engine!(IndexProg),
        Parallelism::Threads(threads) => {
            // A dedicated pool rather than rayon's global one, so `-j N` means exactly N index
            // threads no matter what else in the process has touched rayon. The parallel program
            // runs its rules with `rayon::scope` / `par_iter`, which pick up the pool they are
            // `install`ed in.
            let pool = ascent::rayon::ThreadPoolBuilder::new()
                .num_threads(threads.get())
                .thread_name(|i| format!("ctadl-index-{i}"))
                .build()
                .expect("spawning the index thread pool");
            pool.install(move || run_engine!(ParIndexProg))
        }
    };
    log::trace!("index result: {}", result.display(id_map));
    log::debug!(
        "flow variable size: {}",
        std::mem::size_of::<FlowVariable>()
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{CallResolutionStrategy, codegen_program};
    use crate::index_engine::source_info::IndexSourceInfo;
    use ctadl_ir::ProgramInfo;

    /// Lowers one C translation unit. This function and [`index_program`] are test helpers
    /// that belong to the engine. `ctadl-c` has almost the same helpers, but the engine keeps
    /// its own thirty lines instead of reaching into a front end's private code. The crates
    /// depend on each other in the other direction, and sharing them would need a
    /// dev-dependency cycle in both directions.
    fn program_from_string(src: &str) -> (ctadl_ir::Program, String) {
        let (program, failed, dump) =
            ctadl_c::parse_c_program(src).expect("Failed to parse C program.");
        assert!(!failed, "Input Program failed to parse without error");
        assert!(
            !dump.contains("<no terminator>"),
            "Parsed IR contains a block with no terminator:\n{dump}"
        );
        (program, dump)
    }

    /// Runs SSA, then code generation, then the taint index. Returns the facts the caller
    /// needs in order to run it again.
    fn index_program(program: ctadl_ir::Program) -> (IndexFacts, IndexSourceInfo) {
        let mut program_info = ProgramInfo {
            program,
            ..Default::default()
        };
        program_info.program.verify().unwrap();
        ctadl_ir::ssa::transform_program(&mut program_info.program, true);
        let mut facts = IndexFacts::default();
        let mut source_info = IndexSourceInfo::default();
        codegen_program(
            program_info,
            &mut facts,
            &mut source_info,
            CallResolutionStrategy::Mixed,
            Default::default(),
            &Default::default(),
        );
        (facts, source_info)
    }

    /// A program that touches every part of the rules: direct calls, summaries, field-sensitive
    /// flows, and enough indirect calls (function-pointer parameter, function pointer in a
    /// struct field, one target reaching two critical sites) that the hybrid-inlining relations
    /// and their decision sets all get rows.
    const SRC: &str = r"
        struct ops { int (*f)(int); int tag; };
        int id(int p) { return p; }
        int twice(int p) { return p + p; }
        int apply(int (*f)(int), int x) { return f(x); }
        int through(struct ops *o, int x) { return o->f(x); }
        int wrap(int a, int b) {
            struct ops o;
            o.f = id;
            o.tag = a;
            int r = apply(twice, b);
            int s = through(&o, r);
            return s + apply(id, a) + o.tag;
        }";

    /// The output relations of one run, each sorted, so two runs compare as sets.
    #[allow(clippy::type_complexity)]
    fn canonical(
        result: IndexResult,
    ) -> (
        Vec<FunctionSummary>,
        Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)>,
        Vec<(FunctionId, FlowVariable, Path, CallTargetObject)>,
        Vec<(Path,)>,
        Vec<(FunctionId,)>,
    ) {
        let IndexResult {
            mut summary,
            mut assign_like,
            mut call_target_assign_like,
            mut paths,
            mut external_function,
            stats: _,
        } = result;
        summary.sort();
        assign_like.sort();
        call_target_assign_like.sort();
        paths.sort();
        external_function.sort();
        (
            summary,
            assign_like,
            call_target_assign_like,
            paths,
            external_function,
        )
    }

    /// The two engines are one rule text under two macros, so they must agree on every
    /// relation they return, and on the sizes of the internal ones they do not. Run the parallel
    /// engine on more threads than the program has functions so the rules actually interleave.
    #[test_log::test]
    fn parallel_engine_matches_serial() {
        let (program, _) = program_from_string(SRC);
        let (facts, source_info) = index_program(program);
        let id_map = Some(&source_info.sites);

        let config = IndexConfig::default();
        let serial = taint_index_with_config(facts.clone(), config.clone(), id_map);
        let parallel = taint_index_with_config(
            facts,
            IndexConfig {
                parallelism: Parallelism::Threads(NonZeroUsize::new(8).unwrap()),
                ..config
            },
            id_map,
        );

        // The internal relations first: a count that differs points straight at the rule group
        // that diverged, which the output diff below would only show as a symptom.
        let (s, p) = (&serial.stats, &parallel.stats);
        assert_eq!(s.final_locals, p.final_locals, "locals");
        assert_eq!(
            s.reached_variables, p.reached_variables,
            "reached variables"
        );
        assert_eq!(
            s.hybrid_critical_summary, p.hybrid_critical_summary,
            "critical_summary"
        );
        assert_eq!(s.hybrid_resolvent, p.hybrid_resolvent, "resolvent");
        assert_eq!(
            s.hybrid_context_assign, p.hybrid_context_assign,
            "context_assign"
        );
        assert_eq!(
            s.hybrid_context_locals, p.hybrid_context_locals,
            "context_locals"
        );
        assert_eq!(
            s.hybrid_context_summary, p.hybrid_context_summary,
            "context_summary"
        );
        // The fixture only proves anything if the hybrid machinery actually ran.
        assert!(s.hybrid_resolvent > 0, "fixture derived no resolvents");
        assert!(
            s.hybrid_context_assign > 0,
            "fixture derived no context_assign rows"
        );

        assert_eq!(canonical(serial), canonical(parallel));
    }

    /// The three context joins are three ways to pair the same two relations, so they must
    /// agree on every relation, internal and returned. Same fixture and same checks as the
    /// engine test above.
    #[test_log::test]
    fn context_joins_agree() {
        let (program, _) = program_from_string(SRC);
        let (facts, source_info) = index_program(program);
        let id_map = Some(&source_info.sites);
        let run = |context_join| {
            taint_index_with_config(
                facts.clone(),
                IndexConfig {
                    context_join,
                    ..IndexConfig::default()
                },
                id_map,
            )
        };
        let scan = run(ContextJoin::Scan);
        for other in [ContextJoin::Sets, ContextJoin::Unfold] {
            let r = run(other);
            let (s, p) = (&scan.stats, &r.stats);
            assert_eq!(s.final_locals, p.final_locals, "{other}: locals");
            assert_eq!(s.hybrid_resolvent, p.hybrid_resolvent, "{other}: resolvent");
            assert_eq!(
                s.hybrid_context_assign, p.hybrid_context_assign,
                "{other}: context_assign"
            );
            assert_eq!(
                s.hybrid_context_locals, p.hybrid_context_locals,
                "{other}: context_locals"
            );
            assert_eq!(
                s.hybrid_context_summary, p.hybrid_context_summary,
                "{other}: context_summary"
            );
            assert!(
                s.hybrid_context_assign > 0,
                "fixture derived no context_assign rows"
            );
            assert_eq!(
                canonical(taint_index_with_config(
                    facts.clone(),
                    IndexConfig {
                        context_join: ContextJoin::Scan,
                        ..IndexConfig::default()
                    },
                    id_map
                )),
                canonical(r),
                "{other}"
            );
        }
    }

    #[test]
    fn jobs_convention() {
        assert_eq!(Parallelism::from_jobs(1), Parallelism::Serial);
        assert_eq!(
            Parallelism::from_jobs(3),
            Parallelism::Threads(NonZeroUsize::new(3).unwrap())
        );
        // `0` is "all cores": serial on a single core, parallel on that many otherwise.
        let cores = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let expected = if cores > 1 {
            Parallelism::Threads(NonZeroUsize::new(cores).unwrap())
        } else {
            Parallelism::Serial
        };
        assert_eq!(Parallelism::from_jobs(0), expected);
    }
}
