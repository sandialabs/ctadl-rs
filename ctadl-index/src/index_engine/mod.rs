/*!
Computes a compositional, global data flow graph.

# Input

[`IndexFacts`] is the input format. It is a flat, relational format. Generating this input format
requires mapping function names and instructions into instruction sites. An instruction site is a
globally unique ID for each instruction. It is composed of a function ID and an instruction ID
packed into a 64-bit integer. Instruction sites may also be associated with source info.
Generating facts is typically done with a mutable reference to [`IndexFacts`]. Generating IDs is
done with the help of the [`source_info::IndexSourceInfo`] API. The source info may be persisted,
but it is not used during indexing (only for fact generation).

The rules are [`index_rules`]. Each engine is a crate of its own that runs them:
`ctadl-index-serial`, `ctadl-index-par` and `ctadl-index-lsm`. `ctadl_ascent::index_engine` picks
one and runs it.

# Data Flow Analysis

The code handles direct data flows and some aliased data flows. The aliased flows look like,
e.g.:

```text
this = 0;
this.f = 1;
```
is a write to an alias of a formal. The aliasing rule turns this into a summary where 1 flows
to 0.f.

# Interned values

`Path`, `Str`, `CallTargetObject`, `CallDispatchKey` and [`DecisionSet`] are 4-byte handles into
process-wide interners that never free. Before the fixpoint starts, building an index interns
every `Path`, `Str`, `CallTargetObject` and `CallDispatchKey` the rules can produce: the path
set's members and their splits ([`path_set`]), the paths the intent rules match by name, and
`CallDispatchKey::c()`. The fixpoint only reads them, without a lock. Decision sets are the
exception: lattice joins intern new ones during the fixpoint ([`decision`]). `profile.json`
reports each interner's size, and how much the read-only ones grew during the fixpoint.

*/

use std::num::NonZeroUsize;
use std::path;
use std::sync::{Arc, LazyLock};

use ascent::ascent_run;
use ascent::ascent_source;
use ctadl_ir::mir::PathSegment;
use derive_builder::Builder;
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;
use streaming_iterator::StreamingIterator;

use crate::error::{Error, ErrorContext};
use crate::facts::{
    CallDispatchKey, CallTargetObject, CallTargetObjectKind, FlowVariable, FlowVariableKind,
    FlowVertex, FormalIndex, FormalType, FunctionId, IdMap, InsnId, InsnSiteId, IntentKind,
    IntentPairKind, PackedInsnSiteId, Path, Str,
};
pub use crate::index_engine::decision::{Cond, Decision, DecisionId, DecisionSet};
use crate::index_engine::path_set::{PathSet, PathSetRef};

pub mod assign_like_trie;
pub mod c_assign_like_trie;
pub mod c_cond_trie;
pub mod c_locals_trie;
pub mod cond_trie;
pub mod decision;
pub mod dump;
pub mod hybrid_set;
pub mod locals_trie;
pub mod path_group;
pub mod path_set;
pub mod profile;
pub mod source_info;

// Column encodings for the relations that `Storage::Lsm` moves onto the `lsm` provider. All of
// them are `Copy`, fixed-width and free of padding. `Path` and a local `FlowVariable`'s `Str` are
// 4-byte ids into interners that never free, so their bits stay valid for the whole process,
// which is as long as the provider's run files live.
lsm::lsm_col!(FunctionId, 1, [u32]);
lsm::lsm_col!(FlowVariable, 1, [u64], valid = FlowVariable::is_valid);
lsm::lsm_col!(Path, 1, [std::num::NonZeroU32], valid = Path::is_valid);
lsm::lsm_col!(FormalIndex, 1, [i16]);
lsm::lsm_col!(PackedInsnSiteId, 1, [[u8; 8]]);
lsm::lsm_col!(
    FlowVertex,
    2,
    [FlowVariable, Path, crate::facts::VertexPad],
    valid = FlowVertex::is_valid
);

/// An assignment statement. The order is destination vertex then source vertex.
pub type AssignFlow = (PackedInsnSiteId, FlowVertex, FlowVertex);
/// A copy of a whole variable except one path; see [`IndexFacts::update_sans_path`].
pub type UpdateSansPath = (PackedInsnSiteId, FlowVariable, FlowVariable, Path);
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
    /// The copy half of a functional update `x = update (y.w := v)`: `x` is `y` except at
    /// `w` and the paths under it, as `(site, x, y, w)`. The write `x.w <- v` is an `assign`.
    #[builder(default)]
    pub update_sans_path: Vec<UpdateSansPath>,
    /// A call target ([`CallTargetObject`]) stored at a vertex by an assignment.
    #[builder(default)]
    pub call_target_assign: Vec<(PackedInsnSiteId, FlowVertex, CallTargetObject)>,
    /// A string constant stored at a vertex by an assignment, store, or call argument.
    #[builder(default)]
    pub const_str_assign: Vec<(PackedInsnSiteId, FlowVertex, Str)>,
    /// Observed Java call sites: declared class, simple method name, descriptor.
    #[builder(default)]
    pub android_call_site: Vec<(PackedInsnSiteId, Str, Str, Str)>,
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
    /// Functions where string-constant propagation is worth computing.
    #[builder(default)]
    pub intent_frame: Vec<(FunctionId,)>,
    #[builder(default)]
    pub intent_send: Vec<(FunctionId, InsnId, InsnId, FormalIndex, IntentKind)>,
    #[builder(default)]
    pub intent_filter: Vec<(Str, Str, IntentKind, FunctionId)>,
    #[builder(default)]
    pub intent_component: Vec<(Str, IntentKind, FunctionId)>,
    #[builder(default)]
    pub external_function: Vec<(FunctionId,)>,
}

impl IndexFacts {
    /// Saves the `formal_param`, `actual_param`, `call`, `call_target_assign`, `callee_info`,
    /// `callee_resolvents` and `external_function` members. The others aren't saved because
    /// they are computed as part of an [`IndexResult`].
    ///
    /// Borrows `self`: only the tuples serialized here are cloned, and the rest of the fact
    /// base (dominated by `assign`, which isn't written) is not copied.
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
                    let FlowVertex(variable, path, ..) = vertex;
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
                    let FlowVertex(variable, path, ..) = vertex;
                    (func_id, insn_id, *variable, *path, *target)
                }),
        )?;
        callee_info::try_save(
            &dir,
            self.callee_info
                .iter()
                .map(|(site_id, vertex, dispatch_key)| {
                    let InsnSiteId { func_id, insn_id } =
                        InsnSiteId::unpack_from_slice(&**site_id).unwrap();
                    let FlowVertex(variable, path, ..) = vertex;
                    (func_id, insn_id, *variable, *path, *dispatch_key)
                }),
        )?;
        callee_resolvents::try_save(&dir, self.callee_resolvents.iter().cloned())?;
        external_function::try_save(&dir, self.external_function.iter().copied())?;
        Ok(())
    }

    /// Loads the members saved by [`IndexFacts::try_save`].
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
                            FlowVertex::new(variable, path),
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
                            FlowVertex::new(variable, path),
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
                            FlowVertex::new(variable, path),
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
                _ => continue,
            };
            func_num_params
                .entry(*func)
                .and_modify(|m| *m = (*m).max(i + 1))
                .or_insert(i + 1);
        }
        func_num_params
    }

    /// Like [`Self::compute_num_params`], but widens each function's arity to the
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
    /// Where the large relations are stored. In memory by default.
    pub storage: Storage,
    /// When set, every program path `p` also admits `p.deref`, as if a loaded summary mentioned
    /// `.deref`.
    pub deref_paths: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            alias_rule: true,
            hybrid_context: HybridContext::default(),
            context_join: ContextJoin::default(),
            parallelism: Parallelism::Serial,
            storage: Storage::Memory,
            deref_paths: false,
        }
    }
}

/// How rule 3.2 pairs a function's conditional summaries (the `summary` rows under a
/// [`DecisionSet`]) with the calls that established a decision at it
/// (`establishes_direct` / `establishes_via`, one row per call site and decision). All three
/// compute the same fixpoint; they differ in how much of the pairing is wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ContextJoin {
    /// Join both sides on the function alone and test `ds.contains(d)` on every pair. The
    /// work is `|summaries(f)| x |establishing calls(f)|` per function, most of which the
    /// membership test throws away.
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
    /// The decided callee's summary is instantiated at the site as edges conditioned on the
    /// [`Decision`] that resolved it, closed into a summary of the enclosing function
    /// conditioned on that decision, and applied one call site at a time at every caller whose
    /// route established the decision, until it lands as a plain edge in a function that held
    /// the call target. Two callers passing different targets get different flows.
    ///
    /// A row's condition is a [`Cond`], a lattice: `Always`, or a [`DecisionSet`] under every
    /// decision of which the row holds. Rows that several decisions reach are shared rather
    /// than repeated, and a row that also holds unconditionally is one `Always` row, so there is
    /// one row per `(f, v, p, a, p4)` whatever the number of decisions reaching a function.
    /// Complete,
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
    /// As `Bounded(k)`, but a row whose set widens to ⊤ is `Always` instead, so the flow is
    /// shared by every caller through the unconditional `summary` (as under `None`), and
    /// nothing conditional is derived from it. Coarser than `Bounded(k)` downstream of the widened row
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

/// Where the index engine keeps `locals_key` and the next largest few relations. `locals`,
/// `assign_like`, `edge_split` and `ext_dst` carry a [`Cond`] and are in the in-memory
/// [`cond_trie`] stores under both; every other relation is an ordinary in-memory Ascent relation
/// either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Storage {
    /// The in-memory tries of [`locals_trie`] and [`assign_like_trie`].
    #[default]
    Memory,
    /// The memory-mapped LSM maps of the `lsm` crate, which hold each index of a relation in
    /// run files under `$TMPDIR` and only a write buffer in memory. Serial engine only.
    Lsm,
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
    pub profile: profile::IndexProfile,
    pub initial_assign: usize,
    pub final_assign_like: usize,
    pub initial_formals: usize,
    pub final_locals: usize,
    pub initial_call_target_assign: usize,
    pub final_call_target_assign_like: usize,
    pub initial_callee_info: usize,
    pub final_resolved_call: usize,
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
            "call resolution: {} (site, target) pair(s) over {} indirect call site(s)",
            self.final_resolved_call,
            self.initial_callee_info
        );
        log::debug!(
            "relation increase: summary: {:.2} ({}/{}) (ratio over num_functions)",
            ratio(self.final_summary, self.num_functions),
            self.final_summary,
            self.num_functions
        );
        log::debug!(
            "hybrid inlining: critical_summary: {:.2} ({}/{}), resolvent: {}, conditional assign_like: {:.2} ({}/{}) (ratio over final assign_like), conditional locals: {:.2} ({}/{}) (ratio over final locals), conditional summary: {}",
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
    /// [`IndexFacts::update_sans_path`], keyed by function.
    pub update_sans_path: Vec<(FunctionId, FlowVariable, FlowVariable, Path)>,
    pub call_target_assign_like: Vec<(FunctionId, FlowVariable, Path, CallTargetObject)>,
    pub resolved_call: Vec<(FunctionId, InsnId, FunctionId)>,
    pub const_reaches: Vec<(FunctionId, FlowVariable, Path, Str)>,
    pub intent_pair: Vec<(FunctionId, InsnId, FunctionId, IntentPairKind)>,
    pub paths: Vec<(Path,)>,
    pub external_function: Vec<(FunctionId,)>,
    pub stats: IndexStats,
}

impl IndexResult {
    pub fn try_save<P: AsRef<path::Path>>(self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        // `[mem cp]` around each table's serialize: the fixpoint's own checkpoints stop at
        // `ascent_run returned`, but on path-heavy targets the true peak is
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
        update_sans_path::try_save(&dir, self.update_sans_path)?;
        let paths_rows = self.paths.len();
        paths::try_save(&dir, self.paths)?;
        log::debug!(
            "[mem cp]   after paths::try_save ({} rows): {:.1} MB",
            paths_rows,
            phys_footprint_mb()
        );
        external_function::try_save(&dir, self.external_function)?;
        resolved_call::try_save(&dir, self.resolved_call)?;
        intent_pair::try_save(&dir, self.intent_pair)?;
        // After the tables, so the sizes include anything writing them interned.
        let mut profile = self.stats.profile;
        profile.memory.read_interned();
        let profile_path = dir.as_ref().join(profile::PROFILE_FILE);
        let file = std::fs::File::create(&profile_path)
            .err_context(|| format!("creating {}", profile_path.display()))?;
        serde_json::to_writer_pretty(std::io::BufWriter::new(file), &profile)
            .err_context(|| format!("writing {}", profile_path.display()))?;
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
        let update_sans_path = update_sans_path::try_load(&dir)?;
        let paths = paths::try_load(&dir)?;
        let external_function = external_function::try_load(&dir)?;
        let resolved_call = resolved_call::try_load(&dir)?;
        let intent_pair = intent_pair::try_load(&dir)?;
        Ok(IndexResult {
            summary,
            assign_like,
            update_sans_path,
            call_target_assign_like: Vec::new(),
            resolved_call,
            const_reaches: Vec::new(),
            intent_pair,
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

/// The name of `f` in `id_map`, or `"unknown"` when there is no map or `f` is not in it.
pub fn func_name(id_map: Option<&IdMap>, f: FunctionId) -> &str {
    id_map
        .and_then(|m| m.get_function(f))
        .map(|f| f.0.as_ref())
        .unwrap_or("unknown")
}

/// `v` followed by `p` in dot notation, with a local variable shown by its name.
pub fn vertex_str(v: &FlowVariable, p: &Path) -> String {
    let var_str = if let Some(name) = v.as_local() {
        name.to_string()
    } else {
        format!("{}", v)
    };
    format!("{}{}", var_str, p.to_dot_string())
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
                writeln!(f, "{}({}): {:?}", func_name(Some(id_map), s.0), s.0.id, s)?;
            } else {
                writeln!(f, "{:?}", s)?;
            }
        }
        writeln!(f, "\nAssign-like:")?;
        for (func_id, dest_var, dest_path, src_var, src_path) in &self.result.assign_like {
            let dest_str = vertex_str(dest_var, dest_path);
            let src_str = vertex_str(src_var, src_path);

            let name = func_name(self.id_map, *func_id);

            writeln!(f, "{}({}): {} = {}", name, func_id.id, dest_str, src_str)?;
        }
        writeln!(f, "\nPaths:")?;
        for (p,) in &self.result.paths {
            writeln!(f, "{}", p)?;
        }
        Ok(())
    }
}

impl IndexResult {
    /// The final call graph: `call`, the pre-fixpoint relation codegen emitted, plus every call
    /// the fixpoint resolved or derived (virtual dispatch, Android intent pairings).
    pub fn final_call(
        &self,
        call: &[(PackedInsnSiteId, FunctionId)],
    ) -> Vec<(PackedInsnSiteId, FunctionId)> {
        call.iter()
            .copied()
            .chain(self.resolved_call.iter().map(|(func_id, insn_id, target)| {
                (
                    PackedInsnSiteId::try_from_parts(*func_id, *insn_id).unwrap(),
                    *target,
                )
            }))
            .collect()
    }

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
pub trait Rows<Row> {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

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

/// Builds a lattice relation's field from its input rows, once each: a `Vec` under `ascent!`, a
/// vector of locked rows under `ascent_par!`.
pub trait FromLatticeRows<Row> {
    fn from_lattice_rows(rows: impl Iterator<Item = Row>) -> Self;
}

impl<Row: Copy + Eq + std::hash::Hash> FromLatticeRows<Row> for Vec<Row> {
    fn from_lattice_rows(rows: impl Iterator<Item = Row>) -> Self {
        distinct(rows).collect()
    }
}

impl<Row: Copy + Eq + std::hash::Hash> FromLatticeRows<Row>
    for ascent::boxcar::Vec<std::sync::RwLock<Row>>
{
    fn from_lattice_rows(rows: impl Iterator<Item = Row>) -> Self {
        distinct(rows).map(std::sync::RwLock::new).collect()
    }
}

/// Ascent keeps a seeded field as given, and a lattice row seeded twice stays twice.
fn distinct<Row: Copy + Eq + std::hash::Hash>(
    rows: impl Iterator<Item = Row>,
) -> impl Iterator<Item = Row> {
    let mut seen = hashbrown::HashSet::new();
    rows.filter(move |r| seen.insert(*r))
}

pub type CriticalSummaryRow = (FunctionId, FormalIndex, Path);
pub type ResolventRow = (FunctionId, FormalIndex, Path, CallTargetObject, DecisionId);
pub type CallTargetAssignLikeRow = (FunctionId, FlowVariable, Path, CallTargetObject);
pub type AssignLikeCondRow = (FunctionId, FlowVariable, Path, FlowVariable, Path, Cond);
pub type LocalsCondRow = (FunctionId, FlowVariable, Path, FormalIndex, Path, Cond);
pub type SummaryCondRow = (FunctionId, FormalIndex, Path, FormalIndex, Path, Cond);

/// The `Always` rows of a lattice relation of five columns and a [`Cond`], without the
/// condition: the facts that hold in every calling context, which is what the output keeps.
pub fn always_rows<A: Copy, B: Copy, C: Copy, D: Copy, E: Copy>(
    rows: &dyn Rows<(A, B, C, D, E, Cond)>,
) -> Vec<(A, B, C, D, E)> {
    let mut out = Vec::new();
    let mut it = rows.stream();
    while let Some((a, b, c, d, e, cond)) = it.next() {
        if cond.is_always() {
            out.push((*a, *b, *c, *d, *e));
        }
    }
    out
}

/// How many rows of a relation of five columns and a [`Cond`] are conditional.
pub fn conditional_count<A, B, C, D, E>(rows: &dyn Rows<(A, B, C, D, E, Cond)>) -> usize {
    let mut n = 0;
    let mut it = rows.stream();
    while let Some((.., cond)) = it.next() {
        n += usize::from(!cond.is_always());
    }
    n
}

/// Where the conditional rows are, for the debug log: per function, the conditional rows of
/// `locals`, the decisions reaching it (`resolvent`), the distinct decision sets its
/// rows hold and the sum of their sizes -- the last is what a run keyed by decision instead of
/// by decision set would hold as rows. The top functions by row count are listed.
pub fn context_histogram(
    locals: &dyn Rows<LocalsCondRow>,
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
    let mut rows = locals.stream();
    while let Some((f, _, _, _, _, cond)) = rows.next() {
        let Some(ds) = cond.set() else { continue };
        let ds = &ds;
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
        "conditional locals by function: rows={} memberships={} (rows a per-decision keying would hold) \
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
        let name = func_name(id_map, *f);
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

/// What `run_engine!` does with the stores of the large relations, which differ by [`Storage`]:
/// each relation's tuples are in its `ind_common`, a trie or an `LsmStore`, and an LSM
/// relation's field counts its rows and, when seeded, takes its input rows.
pub trait EngineStores {
    /// Row counts of the store-backed relations.
    fn store_sizes(&self) -> StoreSizes;
    /// Writes the store-backed relations, whose fields hold no rows, to `dump`.
    fn dump_stores(&self, dump: &mut dump::RelationDump<'_>);
}

pub type AssignLikeRow = (FunctionId, FlowVariable, Path, FlowVariable, Path);

/// The store-backed relations every storage has, other than the [`CondStores`].
pub struct StoreSizes {
    pub locals_key: usize,
}

/// What `run_engine!` does with the [`cond_trie`] stores of `locals`, `assign_like`, `edge_split`
/// and `ext_dst`, serial or concurrent. Implemented by [`cond_stores!`](crate::cond_stores).
pub trait CondStores {
    /// Seeds `assign_like` with the program's assignments, `Always`, before the run.
    fn seed_assign_like(&mut self, rows: Vec<AssignLikeRow>);
    /// Takes `assign_like`'s `Always` rows out, after the run.
    fn take_assign_like(&mut self) -> Vec<AssignLikeRow>;
    fn cond_sizes(&self) -> CondSizes;
    /// The heap of the `locals` store's `Always` rows.
    fn locals_heap_report(&self) -> locals_trie::HeapReport;
    /// The heap of the conditional rows, one line.
    fn cond_heap_report(&self) -> String;
    fn locals_rows(&self) -> Box<dyn Rows<LocalsCondRow> + '_>;
    fn assign_like_rows(&self) -> Box<dyn Rows<AssignLikeCondRow> + '_>;
    /// Writes the four relations, whose fields hold no rows, to `dump`.
    fn dump_cond_stores(&self, dump: &mut dump::RelationDump<'_>);
}

/// Row counts of the [`CondStores`]: every row, and the conditional ones.
pub struct CondSizes {
    pub locals: (usize, usize),
    pub assign_like: (usize, usize),
    pub edge_split: (usize, usize),
    pub ext_dst: (usize, usize),
    /// The distinct `(f, v)` an `Always` `locals` row reaches.
    pub reached_variables: usize,
}

/// Implements [`CondStores`] for a program whose four conditional relations are in the stores
/// of `$m`, [`cond_trie`] or [`c_cond_trie`].
#[macro_export]
macro_rules! cond_stores {
    ($prog_ty:ty, $m:ident) => {
        impl $crate::index_engine::CondStores for $prog_ty {
            fn seed_assign_like(&mut self, rows: Vec<$crate::index_engine::AssignLikeRow>) {
                self.__assign_like_ind_common =
                    $crate::index_engine::assign_like_trie::FromRows::from_rows(rows);
            }
            fn take_assign_like(&mut self) -> Vec<$crate::index_engine::AssignLikeRow> {
                $crate::index_engine::$m::assign::into_always_vec(std::mem::take(
                    &mut self.__assign_like_ind_common,
                ))
            }
            fn cond_sizes(&self) -> $crate::index_engine::CondSizes {
                $crate::index_engine::CondSizes {
                    locals: (
                        self.__locals_ind_common.len(),
                        self.__locals_ind_common.conditional_len(),
                    ),
                    assign_like: (
                        self.__assign_like_ind_common.len(),
                        self.__assign_like_ind_common.conditional_len(),
                    ),
                    edge_split: (
                        self.__edge_split_ind_common.len(),
                        self.__edge_split_ind_common.conditional_len(),
                    ),
                    ext_dst: (
                        self.__ext_dst_ind_common.len(),
                        self.__ext_dst_ind_common.conditional_len(),
                    ),
                    reached_variables: self.__locals_ind_common.always().num_reached_variables(),
                }
            }
            fn locals_heap_report(&self) -> $crate::index_engine::locals_trie::HeapReport {
                self.__locals_ind_common.always().heap_report()
            }
            fn cond_heap_report(&self) -> String {
                let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
                format!(
                    "conditional rows: locals {} ({:.1} MB), assign_like {} ({:.1} MB), \
                     edge_split {} ({:.1} MB), ext_dst {} ({:.1} MB)",
                    self.__locals_ind_common.conditional_len(),
                    mb(self.__locals_ind_common.cond_heap_bytes()),
                    self.__assign_like_ind_common.conditional_len(),
                    mb(self.__assign_like_ind_common.cond_heap_bytes()),
                    self.__edge_split_ind_common.conditional_len(),
                    mb(self.__edge_split_ind_common.cond_heap_bytes()),
                    self.__ext_dst_ind_common.conditional_len(),
                    mb(self.__ext_dst_ind_common.cond_heap_bytes()),
                )
            }
            fn locals_rows(
                &self,
            ) -> Box<dyn $crate::index_engine::Rows<$crate::index_engine::LocalsCondRow> + '_> {
                Box::new($crate::index_engine::$m::locals::AsRows(
                    &self.__locals_ind_common,
                ))
            }
            fn assign_like_rows(
                &self,
            ) -> Box<dyn $crate::index_engine::Rows<$crate::index_engine::AssignLikeCondRow> + '_>
            {
                Box::new($crate::index_engine::$m::assign::AsRows(
                    &self.__assign_like_ind_common,
                ))
            }
            fn dump_cond_stores(&self, dump: &mut $crate::index_engine::dump::RelationDump<'_>) {
                use $crate::index_engine::dump::dump_rows;
                use $crate::index_engine::$m::{assign, locals};
                dump_rows(&locals::AsRows(&self.__locals_ind_common), dump, "locals");
                dump_rows(
                    &assign::AsRows(&self.__assign_like_ind_common),
                    dump,
                    "assign_like",
                );
                dump_rows(
                    &locals::AsRows(&self.__edge_split_ind_common),
                    dump,
                    "edge_split",
                );
                dump_rows(&assign::AsRows(&self.__ext_dst_ind_common), dump, "ext_dst");
            }
        }
    };
}

pub struct HybridInliningRelations<'a> {
    pub critical_summary: &'a dyn Rows<CriticalSummaryRow>,
    pub resolvent: &'a dyn Rows<ResolventRow>,
    pub call_target_assign_like: &'a dyn Rows<CallTargetAssignLikeRow>,
    pub assign_like: &'a dyn Rows<AssignLikeCondRow>,
    pub locals: &'a dyn Rows<LocalsCondRow>,
    pub summary: &'a dyn Rows<SummaryCondRow>,
    pub id_map: Option<&'a IdMap>,
}

impl std::fmt::Display for HybridInliningRelations<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Critical Summary ({}):", self.critical_summary.len())?;
        let mut rows = self.critical_summary.stream();
        while let Some((func_id, formal_index, path)) = rows.next() {
            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {}({}): arg{} {}",
                name,
                func_id.id,
                formal_index,
                path.to_dot_string(),
            )?;
        }

        writeln!(f, "\nResolvent ({}):", self.resolvent.len())?;
        let mut rows = self.resolvent.stream();
        while let Some((func_id, formal_index, path, resolvent, _)) = rows.next() {
            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {}({}): arg{} {} resolves to {}",
                name,
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
            let name = func_name(self.id_map, *func_id);
            let tgt_str = match tgt.kind() {
                CallTargetObjectKind::FunctionId(tgt) => {
                    let tgt_name = func_name(self.id_map, *tgt);
                    format!("ptr {}({})", tgt_name, tgt.id)
                }
                CallTargetObjectKind::Symbol(cls) => format!("java<{cls}>"),
                CallTargetObjectKind::LuaClass(cls) => format!("lua<{cls}>"),
                CallTargetObjectKind::CilClass(cls) => format!("cil<{cls}>"),
            };

            writeln!(
                f,
                "  {}({}): {} = {}",
                name,
                func_id.id,
                vertex_str(var, path),
                tgt_str
            )?;
        }

        writeln!(
            f,
            "\nConditional Assign ({}):",
            conditional_count(self.assign_like)
        )?;
        let mut rows = self.assign_like.stream();
        while let Some((func_id, dest_var, dest_path, src_var, src_path, cond)) = rows.next() {
            if cond.is_always() {
                continue;
            }
            let cs = cond.to_string();
            let dest_str = vertex_str(dest_var, dest_path);
            let src_str = vertex_str(src_var, src_path);

            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {} {}({}): {} = {}",
                cs, name, func_id.id, dest_str, src_str
            )?;
        }

        writeln!(
            f,
            "\nConditional Locals ({}):",
            conditional_count(self.locals)
        )?;
        let mut rows = self.locals.stream();
        while let Some((func_id, var, path, formal_idx, formal_path, cond)) = rows.next() {
            if cond.is_always() {
                continue;
            }
            let cs = cond.to_string();
            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {} {}({}): {} from arg{}{}",
                cs,
                name,
                func_id.id,
                vertex_str(var, path),
                formal_idx,
                formal_path.to_dot_string()
            )?;
        }

        writeln!(
            f,
            "\nConditional Summary ({}):",
            conditional_count(self.summary)
        )?;
        let mut rows = self.summary.stream();
        while let Some((func_id, dest_idx, dest_path, src_idx, src_path, cond)) = rows.next() {
            if cond.is_always() {
                continue;
            }
            let cs = cond.to_string();
            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {} {}({}): arg{}{} = arg{}{}",
                cs,
                name,
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

/// Returns a FlowVariable for the call argument from the instruction site ID and the formal number
#[macro_export]
macro_rules! call_arg {
    ($insn:expr, $n:expr) => {
        $crate::facts::FlowVariable::call_actual($insn, $n)
    };
}

/// Reads this process's current physical footprint (macOS `phys_footprint`, the same number
/// Activity Monitor's "Memory" column and `footprint(1)` report) in MB. Used for the
/// `[mem cp]` derivation checkpoints so we can attribute the pre-fixpoint memory spike to the
/// individual transient buffers in-process (an external sampler can't see sub-second phases).
/// Returns -1.0 on non-macOS or on error.
#[cfg(target_os = "macos")]
pub fn phys_footprint_mb() -> f64 {
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
pub fn phys_footprint_mb() -> f64 {
    -1.0
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
    // Base case: a formal whole-aliases itself. A `Copied` formal shares nothing with the caller,
    // so a store through it or its aliases is no summary, and it gets no aliases at all.
    let alias_base: Vec<_> = formal_param
        .iter()
        .filter(|(_, _, ty)| *ty != FormalType::Copied)
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

/// `<component>`, the path of an intent's target component.
pub static COMPONENT_PATH: LazyLock<Path> =
    LazyLock::new(|| Path::from_symbol_names(["<component>"]));
/// `<action>`, the path of an intent's action.
pub static ACTION_PATH: LazyLock<Path> = LazyLock::new(|| Path::from_symbol_names(["<action>"]));
/// `<type>`, the path of an intent's MIME type.
pub static TYPE_PATH: LazyLock<Path> = LazyLock::new(|| Path::from_symbol_names(["<type>"]));

/// Computes `paths`, the admissible access paths: the syntactic program paths, the paths the
/// input summaries mention, and each one-level concatenation of the two. Its own small
/// fixpoint, run BEFORE the main ascent, so the main program takes `paths` as a plain input
/// and can also hold it as a [`PathSet`] for lookups that never build a path.
///
/// With `deref_paths`, `deref` ([`DEREF_FIELD`](ctadl_ir::mir::DEREF_FIELD)) is also a model
/// path, so every program path gains its `.deref` extension.
fn compute_paths(
    program_paths: Vec<(Path,)>,
    mut model_paths: Vec<(Path,)>,
    deref_paths: bool,
) -> Vec<(Path,)> {
    if deref_paths {
        let deref = (Path::from_accesses(std::iter::once(PathSegment::deref())),);
        if !model_paths.contains(&deref) {
            model_paths.push(deref);
        }
    }
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

/// The exact splits of a call-target tag's path. The empty path need not be a member of the
/// path set, but it still has one split: a tag on the whole variable meets an edge out of it.
pub fn tag_splits<'a>(ps: &'a PathSet, p: &Path) -> &'a [(Path, Path)] {
    const WHOLE: &[(Path, Path)] = &[(Path::EMPTY, Path::EMPTY)];
    if p.is_empty() { WHOLE } else { ps.splits(p).exact }
}

pub fn mime_matches(filter: &str, actual: &str) -> bool {
    if filter == actual || filter == "*/*" {
        return true;
    }
    let Some((filter_top, filter_sub)) = filter.split_once('/') else {
        return false;
    };
    let Some((actual_top, _actual_sub)) = actual.split_once('/') else {
        return false;
    };
    filter_sub == "*" && filter_top == actual_top
}

ascent_source! {
    /// The index datalog: every relation and rule of the index engine, written exactly once.
    index_rules:
    // Facts:

    relation formal_param(FunctionId, FlowVariable, FormalType);
    relation actual_param(PackedInsnSiteId, FormalIndex, FlowVertex);
    relation call(FunctionId, InsnId, FunctionId);
    // A call target (`CallTargetObject`) stored at a vertex.
    relation call_target_assign(FunctionId, FlowVertex, CallTargetObject);
    relation const_str_assign(FunctionId, FlowVariable, Path, Str);
    // `x = update (y.w := v)`'s copy: `x` is `y` except at `w` and below (`IndexFacts`).
    relation update_sans_path(FunctionId, FlowVariable, FlowVariable, Path);
    relation callee_info(FunctionId, InsnId, FlowVariable, Path, CallDispatchKey);
    relation callee_resolvents(CallTargetObject, CallDispatchKey, FunctionId);
    relation intent_send(FunctionId, InsnId, InsnId, FormalIndex, IntentKind);
    relation intent_filter(Str, Str, IntentKind, FunctionId);
    relation intent_component(Str, IntentKind, FunctionId);

    // Analysis drivers:

    // Set of syntactic access paths. Computed before the run by `compute_paths`, and read by no
    // rule: it is the output's copy. `path_set` holds the same set frozen as a [`PathSet`],
    // which every rule that tests or splits a path asks.
    relation paths(Path);
    relation path_set(PathSetRef);
    // A function's flow from formal `n2.p2` to out-formal `n1.p1`, under its condition: `Always`
    // for one that holds in every calling context, or the decisions it holds under (rule 3.4's
    // conditional summaries). The loaded models are `Always`.
    lattice summary(FunctionId, FormalIndex, Path, FormalIndex, Path, Cond);
    relation config(IndexConfig);
    relation intent_frame(FunctionId);

    // Derived:

    // Local reachability, under its condition. The core, most expensive relation.
    //
    // Every fact of `locals`, `assign_like` and `summary` carries a [`Cond`]: `Always`, or the
    // decisions it holds under. One fact is one row, however it is derived, so a flow that
    // holds unconditionally is never also kept under a decision. A fact composed of two holds
    // under both conditions (`Cond::and`); one derived two ways holds under either (the
    // lattice join). The rules that do not reason about context (call-target propagation,
    // the critical summaries and resolvents, the aliasing summary, `const_reaches`) and the
    // outputs read only `Always` facts.
    //
    // They are plain relations to Ascent, which refuses a custom store on a `lattice`; the store
    // keeps one row per fact and joins its conditions (`cond_trie`).
    #[ds($crate::index_engine::cond_trie::locals)]
    relation locals(FunctionId, FlowVariable, Path, FormalIndex, Path, Cond);
    #[ds($crate::index_engine::cond_trie::assign)]
    relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path, Cond);
    // Real program field-stores (`v.p = ...`, non-empty destination path). Gates the aliasing rule.
    relation prog_store(FunctionId, FlowVariable, Path);
    // A variable that whole-aliases a formal through original program copies, precomputed by
    // `compute_alias_of_formal`. Unlike `locals(v, empty, formal, empty)`, it leaves out
    // inter-procedural / summary-derived copies and the `ParamFlow` write-backs onto the formal.
    // Feeds the aliasing summary rule.
    relation alias_of_formal(FunctionId, FlowVariable, FormalIndex);
    // Call targets propagated across `assign_like` to the receiver vertices of critical calls.
    relation call_target_assign_like(FunctionId, FlowVariable, Path, CallTargetObject);
    relation const_reaches(FunctionId, FlowVariable, Path, Str);

    // Which indirect / virtual call sites resolved to what
    relation resolved_call(FunctionId, InsnId, FunctionId);
    // The function-pointer subset of `resolved_call` (C, Lua, pcode, Flowy): edges a call target
    // crosses as it crosses a direct `call`. Java and Dex virtual dispatch stays out.
    relation fnptr_call(FunctionId, InsnId, FunctionId);
    relation intent_pair(FunctionId, InsnId, FunctionId, IntentPairKind);

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
    // The condition under which call site `insn` of `caller` calls `callee`: `Always` for a
    // direct call or a target the caller holds itself, the resolving decision for a site
    // resolved through a resolvent (rule 3.1). A summary instantiated at the site holds under
    // it.
    lattice call_site(FunctionId, InsnId, FunctionId, Cond);

    // Initialize locals with formals
    locals(infunc, v1, p1.clone(), i, p1.clone(), Cond::Always) <--
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

    // The distinct `(f, v, p)` reached under any condition, so the splits below run once per
    // path, not once per row (rows outnumber paths thirty to one on a dense binary).
    relation reach_vp(FunctionId, FlowVariable, Path);
    reach_vp(f, v, p) <-- locals(f, v, p, _, _, _);
    // The exact and wild keys of a `locals` path. Two relations rather than a flag column: a
    // literal in a clause makes Ascent index that column alone, and the planner then drives
    // whole-relation scans off a two-key index.
    // Held in the `locals` store: same shape, `(f, v, key)` probed exactly, `(rest, p)` leaves.
    #[ds($crate::index_engine::locals_trie)]
    relation locals_key(FunctionId, FlowVariable, Path, Path, Path);
    locals_key(f, v, key, rest, p) <--
        reach_vp(f, v, p),
        path_set(ps),
        for (key, rest) in ps.splits(p).exact;
    relation locals_key_wild(FunctionId, FlowVariable, Path, Path, Path);
    locals_key_wild(f, v, key, rest, p) <--
        reach_vp(f, v, p),
        path_set(ps),
        for (key, rest) in ps.splits(p).wild;
    // A `locals` path ending in an offset, keyed without it, for the wild match.
    relation locals_wild(FunctionId, FlowVariable, Path, i64, Path);
    locals_wild(f, v, key, m, p) <--
        reach_vp(f, v, p),
        path_set(ps),
        if let Some((key, m)) = ps.split_trailing_offset(p);
    // An edge whose source path ends in an offset, keyed without it, for the wild match. The
    // edges and the expanded edges below carry the edge's condition.
    lattice assign_wild(FunctionId, FlowVariable, Path, i64, FlowVariable, Path, Cond);
    assign_wild(f, v2, key, m, v1, p1, *c) <--
        assign_like(f, v1, p1, v2, p2, c),
        path_set(ps),
        if let Some((key, m)) = ps.split_trailing_offset(p2);
    // The exact and wild keys of an edge's source path.
    // Held in the `locals` store's shape: `(f, v2, key)` probed exactly.
    #[ds($crate::index_engine::cond_trie::locals)]
    relation edge_split(FunctionId, FlowVariable, Path, Path, (FlowVariable, Path), Cond);
    edge_split(f, v2, key, rest, (*v1, *p1), *c) <--
        assign_like(f, v1, p1, v2, p2, c),
        path_set(ps),
        for (key, rest) in ps.splits(p2).exact;
    lattice edge_split_wild(FunctionId, FlowVariable, Path, Path, FlowVariable, Path, Cond);
    edge_split_wild(f, v2, key, rest, v1, p1, *c) <--
        assign_like(f, v1, p1, v2, p2, c),
        path_set(ps),
        for (key, rest) in ps.splits(p2).wild;

    // Destination side: `v1.p1 = v2.p2` and a `locals` path `p23 = p2·rest` give `v1.(p1·rest)`
    // whatever `v2.p23` has. `ext_dst` is that expanded edge, derived once per (edge, path)
    // rather than once per `locals` row, so the reachability step below is an exact join.
    // Held in the `assign_like` store's shape: an edge, probed exactly by `(f, v2, p23)`.
    #[ds($crate::index_engine::cond_trie::assign)]
    relation ext_dst(FunctionId, FlowVariable, Path, FlowVariable, Path, Cond);
    ext_dst(f, v1, p13, v2, p23, *c) <--
        locals_key(f, v2, key, rest, p23),
        assign_like(f, v1, p1, v2, key, c),
        path_set(ps),
        if let Some(p13) = ps.concat(p1, None, rest);
    ext_dst(f, v1, p13, v2, p23, *c) <--
        locals_key_wild(f, v2, key, rest, p23),
        assign_wild(f, v2, key, m, v1, p1, c),
        if let Some(n) = rest.head_offset(),
        if n != *m,
        path_set(ps),
        if let Some(p13) = ps.concat(p1, Some(n - *m), &rest.tail());
    // An update's copy carries every path of the source except the written one and those under
    // it, which is what makes the update overwrite. A path above the written one (the whole
    // variable) still carries everything. It needs no formal-side rule: its source path is
    // empty, so `edge_split` would give only the empty-path row this gives.
    ext_dst(f, x, p, y, p, Cond::Always) <--
        reach_vp(f, y, p),
        update_sans_path(f, x, y, w),
        if !p.is_extension_of(w);
    locals(f, v1, p13, a, p4, c) <--
        ext_dst(f, v1, p13, v2, p23, c1),
        locals(f, v2, p23, a, p4, c2),
        if let Some(c) = c1.and(*c2);

    // Formal side: `v1.p1 = v2.p23` with `p23 = p2·rest` and `v2.p2` reached from `a.p4` give
    // `v1.p1` reached from `a.(p4·rest)`. The split is on the edge's source path, and the
    // `locals` probe is exact.
    locals(f, v1, p1, a, p43, c) <--
        edge_split(f, v2, key, rest, dst, c1),
        locals(f, v2, key, a, p4, c2),
        if let Some(c) = c1.and(*c2),
        let (v1, p1) = dst,
        path_set(ps),
        if let Some(p43) = ps.concat(p4, None, rest);
    // The wild half: the `locals` path is `key.[m]`, the edge reads `key.[n]·tail`.
    lattice ext_fml(FunctionId, FlowVariable, Path, FlowVariable, Path, i64, Path, Cond);
    ext_fml(f, v1, p1, v2, p2, n - *m, rest.tail(), *c) <--
        edge_split_wild(f, v2, key, rest, v1, p1, c),
        locals_wild(f, v2, key, m, p2),
        if let Some(n) = rest.head_offset(),
        if n != *m;
    locals(f, v1, p1, a, p43, c) <--
        ext_fml(f, v1, p1, v2, p2, adj, rest, c1),
        locals(f, v2, p2, a, p4, c2),
        if let Some(c) = c1.and(*c2),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, Some(*adj), rest);

    // Compute assignments from call sites
    assign_like(func_id, v.clone(), p, cv.clone(), Path::empty(), Cond::Always),
    assign_like(func_id, cv.clone(), Path::empty(), v.clone(), p, Cond::Always) <--
        actual_param(call_site_slice, n, vx),
        let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**call_site_slice).unwrap(),
        let cv = call_arg!(insn_id, *n),
        let FlowVertex(v, p, ..) = vx;

    // Compute assignments from summaries: an unconditional summary at every site that calls
    // its function, under the site's condition. A conditional one is applied by rule 3.2.
    call_site(caller, insn, callee, Cond::Always) <-- call(caller, insn, callee);
    assign_like(caller, v1, p1, v2, p2, *sc) <--
        summary(callee, n1, p1, n2, p2, c),
        if c.is_always(),
        call_site(caller, insn, callee, sc),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    const_reaches(func_id, v.clone(), p.clone(), s) <--
        intent_frame(func_id),
        const_str_assign(func_id, v, p, s);

    const_reaches(func_id, v1.clone(), p_new.clone(), s) <--
        intent_frame(func_id),
        const_reaches(func_id, v2, p_context, s),
        assign_like(func_id, v1, p1, v2, p2, c),
        if c.is_always(),
        path_set(ps),
        if let Some(p_new) = ps.substitute_prefix(p_context, p2, p1);

    intent_pair(func_id, bridge_insn, recv, IntentPairKind::Explicit) <--
        intent_send(func_id, bridge_insn, send_insn, n, kind),
        let component_path = *COMPONENT_PATH,
        let arg = call_arg!(*send_insn, *n),
        const_reaches(func_id, arg, component_path, cls),
        intent_component(cls, kind, recv);

    intent_pair(func_id, bridge_insn, recv, IntentPairKind::Implicit) <--
        intent_send(func_id, bridge_insn, send_insn, n, kind),
        let action_path = *ACTION_PATH,
        let arg = call_arg!(*send_insn, *n),
        const_reaches(func_id, arg, action_path, action),
        intent_filter(action, filter_mime, kind, recv),
        if filter_mime.as_ref().is_empty();

    intent_pair(func_id, bridge_insn, recv, IntentPairKind::Implicit) <--
        intent_send(func_id, bridge_insn, send_insn, n, kind),
        let action_path = *ACTION_PATH,
        let type_path = *TYPE_PATH,
        let arg = call_arg!(*send_insn, *n),
        const_reaches(func_id, arg, action_path, action),
        const_reaches(func_id, arg, type_path, mime),
        intent_filter(action, filter_mime, kind, recv),
        if !filter_mime.as_ref().is_empty(),
        if mime_matches(filter_mime.as_ref(), mime.as_ref());

    resolved_call(func_id, bridge_insn, recv) <--
        intent_pair(func_id, bridge_insn, recv, _);

    // Compute summaries from local reachability, under the flow's condition (rule 3.4 for a
    // conditional one).
    summary(infunc, n1, p1, n2, p2, *c) <--
        locals(infunc, dst_var, p1, n2, p2, c),
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
    summary(infunc, n1, p1.clone(), n2, bp, Cond::Always) <--
        // v1.p1 <- n2.bp  (delta driver)
        locals(infunc, v1, p1, n2, bp, lc),
        if lc.is_always(),
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
        locals(func_id, v, p_call, n, p_n, c),
        if c.is_always();

    // 1.2: Propagate Critical Summary (context-free) up the stack. The caller may pass only a
    // prefix `key` of the callee's path (`fwd(s) { call(s); }` never touches `s->op`), so the
    // argument is matched at every split `p_tgt = key · rest` and `rest` is put back on.
    critical_summary(caller_func_id, n, p_nr) <--
        call(caller_func_id, caller_insn_id, tgt),
        critical_summary(tgt, n_tgt, p_tgt),
        let arg = call_arg!(*caller_insn_id, *n_tgt),
        path_set(ps),
        for (key, rest) in ps.splits(p_tgt).exact,
        locals(caller_func_id, arg, key, n, p_n, c),
        if c.is_always(),
        if let Some(p_nr) = ps.concat(p_n, None, rest);
    critical_summary(caller_func_id, n, p_nr) <--
        fnptr_call(caller_func_id, caller_insn_id, tgt),
        critical_summary(tgt, n_tgt, p_tgt),
        let arg = call_arg!(*caller_insn_id, *n_tgt),
        path_set(ps),
        for (key, rest) in ps.splits(p_tgt).exact,
        locals(caller_func_id, arg, key, n, p_n, c),
        if c.is_always(),
        if let Some(p_nr) = ps.concat(p_n, None, rest);

    // 2.1: Base Resolvent. A stored call target locally reaches a critical summary, so
    // instantiate the resolvent in the parameters of the summary. The target is carried
    // opaquely as a `CallTargetObject`; its variant is only tested later at call resolution.
    establishes_direct(f, d, caller, call_insn) <--
        critical_summary(f, n, p),
        call(caller, call_insn, f),
        let arg = call_arg!(*call_insn, *n),
        call_target_assign_like(caller, arg, p, cto),
        let d = DecisionId::of(Decision { formal: *n, path: *p, target: cto.clone() });
    establishes_direct(f, d, caller, call_insn) <--
        critical_summary(f, n, p),
        fnptr_call(caller, call_insn, f),
        let arg = call_arg!(*call_insn, *n),
        call_target_assign_like(caller, arg, p, cto),
        let d = DecisionId::of(Decision { formal: *n, path: *p, target: cto.clone() });
    resolvent(f, dec.formal, dec.path, dec.target.clone(), d) <--
        establishes_direct(f, d, _, _),
        let dec = d.get();

    // 2.2: Propagate Resolvent down the critical summaries. Finite by construction: `resolvent`
    // has one row per (function, formal, path, target), and nothing about the route is kept
    // beyond the one hop `establishes_via` records.
    //
    // Two exact two-way joins. `critical_arg` names, in the caller `g`, the call-arg vertex
    // `arg` (of site `insn`, formal `n`) whose callee `f` has a critical summary at `n.p`: the
    // only vertices a resolvent can be passed on through. `critical_reach` is those vertices'
    // rows of `locals`, joined on `(g, arg, p)`, which is an index `locals` already has for the
    // field rules; and only it meets `resolvent`. Joining `resolvent` with `locals` on the
    // formal and testing `as_call_arg` and `critical_summary` afterwards would pair every local
    // reaching a formal with every resolvent at it and keep a few percent of the pairs.
    // Materializing the call-arg rows of `locals` instead would copy a large share of
    // `locals`; keying the filter by the vertex needs nothing new.
    // Keyed by every split `p = key · rest`, as in 1.2: the caller may reach only a prefix.
    relation critical_arg(FunctionId, FlowVariable, Path, Path, Path, FunctionId, InsnId, FormalIndex);
    critical_arg(g, arg, *key, *rest, *p, f, insn, n) <--
        critical_summary(f, n, p),
        call(g, insn, f),
        let arg = call_arg!(*insn, *n),
        path_set(ps),
        for (key, rest) in ps.splits(p).exact;
    critical_arg(g, arg, *key, *rest, *p, f, insn, n) <--
        critical_summary(f, n, p),
        fnptr_call(g, insn, f),
        let arg = call_arg!(*insn, *n),
        path_set(ps),
        for (key, rest) in ps.splits(p).exact;
    relation critical_reach(FunctionId, FormalIndex, Path, FunctionId, InsnId, FormalIndex, Path);
    critical_reach(g, n2, p2r, f, insn, n, p) <--
        locals(g, arg, key, n2, p2, c),
        if c.is_always(),
        critical_arg(g, arg, key, rest, p, f, insn, n),
        path_set(ps),
        if let Some(p2r) = ps.concat(p2, None, rest);
    establishes_via(f, d, g, insn, up) <--
        critical_reach(g, n2, p2, f, insn, n, p),
        resolvent(g, n2, p2, obj, up),
        let d = DecisionId::of(Decision { formal: *n, path: *p, target: obj.clone() });
    resolvent(f, dec.formal, dec.path, dec.target.clone(), d) <--
        establishes_via(f, d, _, _, _),
        let dec = d.get();

    // 3.1: a resolvent that reaches a call site resolves it, under the one decision that
    // resolved it. The target's unconditional summary is then instantiated there like any
    // other callee's, under that decision. `HybridContext::None` records no context, so the
    // site is resolved for every caller that reaches it.
    call_site(caller, call_insn, resolvent_func, sc) <--
        callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
        locals(caller, v_rec, p_rec, n, p, lc),
        if lc.is_always(),
        resolvent(caller, n, p, resolvent_obj, d),
        callee_resolvents(resolvent_obj, dispatch_key, resolvent_func),
        config(c),
        let sc = match c.hybrid_context {
            HybridContext::None => Cond::Always,
            h => Cond::under(d.singleton(h == HybridContext::Collapse)),
        };

    // The resolution the rule above performs, recorded.
    resolved_call(caller, call_insn, resolvent_func) <--
        callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
        locals(caller, v_rec, p_rec, n, p, lc),
        if lc.is_always(),
        resolvent(caller, n, p, resolvent_obj, _),
        callee_resolvents(resolvent_obj, dispatch_key, resolvent_func);

    resolved_call(func_id, insn_id, resolve_tgt) <--
        callee_info(func_id, insn_id, arg, arg_p, dispatch_key),
        call_target_assign_like(func_id, arg, arg_p, cto),
        callee_resolvents(cto, dispatch_key, resolve_tgt);

    fnptr_call(caller, insn, callee) <--
        resolved_call(caller, insn, callee),
        callee_info(caller, insn, _, _, key),
        if *key == CallDispatchKey::c();

    // 3.2: apply a conditional summary at the callers whose route established its decision.
    // The two rules mirror 2.1 and 2.2, which is what makes them complete: a caller that holds
    // the target itself (2.1's shape) applies the summary under the site's own condition, and a
    // caller that received the target through its own formal (2.2's shape) inherits the summary
    // conditioned on that formal as well, and the walk continues at its callers. Every caller
    // on every route gets it, whichever was found first, so the result does not depend on
    // iteration order.
    //
    // How the summaries are paired with the establishing calls is [`ContextJoin`]; the three
    // variants below compute the same rows.
    //
    // `Scan`: join on the callee alone and test the decision by membership. Both semi-naive
    // variants are exact probes by function, but the pairing is `|summaries(f)| x
    // |establishing calls(f)|` and the test throws most of it away.
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), *sc) <--
        config(c),
        if c.context_join == ContextJoin::Scan,
        summary(f, n1, p1_sum, n2, p2_sum, sum_c),
        if let Some(ds) = sum_c.set(),
        establishes_direct(f, d, caller, insn),
        if ds.contains(*d),
        call_site(caller, insn, f, sc),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), ac) <--
        config(c),
        if c.context_join == ContextJoin::Scan,
        summary(f, n1, p1_sum, n2, p2_sum, sum_c),
        if let Some(ds) = sum_c.set(),
        establishes_via(f, d, caller, insn, up),
        if ds.contains(*d),
        call_site(caller, insn, f, sc),
        if let Some(ac) = Cond::under(up.singleton(ds.is_collapsing())).and(*sc),
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
    context_summary_set(f, ds) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        summary(f, _, _, _, _, sum_c),
        if let Some(ds) = sum_c.set();
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
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), *sc) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        summary(f, n1, p1_sum, n2, p2_sum, sum_c),
        if let Some(ds) = sum_c.set(),
        set_establishes_direct(f, ds, caller, insn),
        call_site(caller, insn, f, sc),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), ac) <--
        config(c),
        if c.context_join == ContextJoin::Sets,
        summary(f, n1, p1_sum, n2, p2_sum, sum_c),
        if let Some(ds) = sum_c.set(),
        set_establishes_via(f, ds, caller, insn, up),
        call_site(caller, insn, f, sc),
        if let Some(ac) = Cond::under(up.singleton(ds.is_collapsing())).and(*sc),
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
        summary(f, n1, p1, n2, p2, sum_c),
        if let Some(ds) = sum_c.set(),
        if !ds.is_top(),
        for d in ds.ids();
    relation top_summary_func(FunctionId);
    top_summary_func(f) <--
        config(c),
        if c.context_join == ContextJoin::Unfold,
        summary(f, _, _, _, _, sum_c),
        if sum_c.set().is_some_and(DecisionSet::is_top);
    context_summary_d(f, d, n1, p1, n2, p2) <--
        top_summary_func(f),
        resolvent(f, _, _, _, d),
        summary(f, n1, p1, n2, p2, sum_c),
        if sum_c.set().is_some_and(DecisionSet::is_top);
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), *sc) <--
        context_summary_d(f, d, n1, p1_sum, n2, p2_sum),
        establishes_direct(f, d, caller, insn),
        call_site(caller, insn, f, sc),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);
    assign_like(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), ac) <--
        config(c),
        context_summary_d(f, d, n1, p1_sum, n2, p2_sum),
        establishes_via(f, d, caller, insn, up),
        call_site(caller, insn, f, sc),
        if let Some(ac) = Cond::under(up.singleton(c.hybrid_context == HybridContext::Collapse)).and(*sc),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    // The contextual closure is the propagation rules above: a conditional edge or flow
    // composes with any other under `Cond::and`, and a conditional flow that reaches an
    // out-formal is a conditional summary (3.4) through the summary rule. Under
    // `HybridContext::Spill` a set widened to ⊤ is `Always` (3.5), so it joins the
    // unconditional facts and every caller shares it through `summary`.

    // Local virtual / indirect call: the caller holds the target itself, so the site is
    // resolved in every context, bypassing the resolvent machinery.
    call_site(func_id, insn_id, resolve_tgt, Cond::Always) <--
        callee_info(func_id, insn_id, arg, arg_p, dispatch_key),
        call_target_assign_like(func_id, arg, arg_p, cto),
        callee_resolvents(cto, dispatch_key, resolve_tgt);

    // Functions whose tag closure we bother to compute. `critical_call` alone is too narrow:
    // a pure factory (`h = lookup()`, `Account.new`) contains no indirect call and calls
    // nothing with a critical summary, so its closure would be empty and the tag would never
    // reach its own out-formal for the return-direction rule below to pick up. A called
    // function that holds a call-target fact is exactly the shape that can export a tag.
    // `critical_call` itself keeps its narrower meaning for its other consumers.
    relation tag_closure_func(FunctionId);
    tag_closure_func(f) <-- critical_call(f);
    tag_closure_func(f) <-- call_target_assign(f, _, _), call(_, _, f);
    tag_closure_func(f) <-- call_target_assign(f, _, _), fnptr_call(_, _, f);
    // A caller of a function that exports a tag on an out-formal walks its tags too: the
    // target may have been installed several frames down (`a(&o)` -> `b(o)` storing
    // `o->f`) and has to climb through frames that make no indirect call of their own
    // before it meets its call site. Driven by a tag actually reaching an out-formal, so only
    // frames a tag enters are admitted.
    tag_closure_func(caller) <--
        call_target_assign_like(callee, v, p, _),
        formal_param(callee, v, formal_ty),
        if let Some(n) = v.as_formal(),
        if isout(&n, *formal_ty, p),
        call(caller, _, callee);

    // Call Target Propagation (every kind of target alike). The stored
    // target is carried opaquely as a `CallTargetObject`; the variant is only tested
    // downstream, where the call is actually resolved.
    call_target_assign_like(func_id, v.clone(), p.clone(), tgt) <--
        call_target_assign(func_id, vx, tgt), let FlowVertex(v, p, ..) = vx,
        tag_closure_func(func_id);

    // An edge `v1.p1 = v2.p2` carries a tag on `v2.(p2·rest)` to `v1.(p1·rest)`. As with
    // `locals_key`, the tag's path is split once into `(p2, rest)`, so the step is a two-way
    // join that probes the edge exactly by `(f, v2, p2)` from whichever side is new.
    relation call_target_key(FunctionId, FlowVariable, Path, Path, CallTargetObject);
    call_target_key(f, v, key, rest, tgt) <--
        tag_closure_func(f),
        call_target_assign_like(f, v, p, tgt),
        path_set(ps),
        for (key, rest) in tag_splits(ps, p);
    relation call_target_key_wild(FunctionId, FlowVariable, Path, Path, CallTargetObject);
    call_target_key_wild(f, v, key, rest, tgt) <--
        tag_closure_func(f),
        call_target_assign_like(f, v, p, tgt),
        path_set(ps),
        for (key, rest) in ps.splits(p).wild;
    call_target_assign_like(f, v1.clone(), p_new, tgt) <--
        call_target_key(f, v2, key, rest, tgt),
        assign_like(f, v1, p1, v2, key, c),
        if c.is_always(),
        path_set(ps),
        if let Some(p_new) = ps.concat(p1, None, rest);
    // The wild half: the edge reads `key.[m]`, the tag sits on `key.[n]·tail`.
    call_target_assign_like(f, v1.clone(), p_new, tgt) <--
        call_target_key_wild(f, v2, key, rest, tgt),
        assign_wild(f, v2, key, m, v1, p1, c),
        if c.is_always(),
        if let Some(n) = rest.head_offset(),
        if n != *m,
        path_set(ps),
        if let Some(p_new) = ps.concat(p1, Some(n - *m), &rest.tail());

    call_target_assign_like(func_id, x.clone(), p.clone(), tgt) <--
        tag_closure_func(func_id),
        call_target_assign_like(func_id, y, p, tgt),
        update_sans_path(func_id, x, y, w),
        if !p.is_extension_of(w);

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
        let cv = call_arg!(*insn, n);
    call_target_assign_like(caller, cv, p.clone(), tgt) <--
        call_target_assign_like(callee, v, p, tgt),
        formal_param(callee, v, formal_ty),
        if let Some(n) = v.as_formal(),
        if isout(&n, *formal_ty, p),
        fnptr_call(caller, insn, callee),
        let cv = call_arg!(*insn, n);

    critical_call(func_id) <-- callee_info(func_id, _, _, _, _);
    critical_call(func_id) <--
        critical_summary(tgt, _, _),
        call(func_id, _, tgt);
}

/// What one run of an index engine starts from: the facts, plus what is derived from them before
/// the fixpoint. Each engine crate (`ctadl-index-serial`, `ctadl-index-par` and
/// `ctadl-index-lsm`) hands it to [`run_engine!`](crate::run_engine).
pub struct EngineInput<'a> {
    /// The facts, with `assign` taken out: it is in `assign_like`.
    pub facts: IndexFacts,
    pub call: Vec<(FunctionId, InsnId, FunctionId)>,
    pub config: IndexConfig,
    pub assign_like: Vec<AssignLikeRow>,
    pub prog_store: Vec<(FunctionId, FlowVariable, Path)>,
    pub alias_of_formal: Vec<(FunctionId, FlowVariable, FormalIndex)>,
    pub paths: Vec<(Path,)>,
    pub path_set: PathSetRef,
    pub index_timeout: std::time::Duration,
    pub id_map: Option<&'a IdMap>,
    pub initial_assign: usize,
    pub initial_call_target_assign: usize,
    pub initial_summary: usize,
    pub initial_formals: usize,
    pub num_functions: usize,
    pub num_variables: usize,
}

impl<'a> EngineInput<'a> {
    pub fn new(mut facts: IndexFacts, config: IndexConfig, id_map: Option<&'a IdMap>) -> Self {
        let parallelism = config.parallelism;
        let storage = config.storage;
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
        // Assignments from the original program whose destination path is non-empty.
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
        let assign_like: Vec<_> = std::mem::take(&mut facts.assign)
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
                let InsnSiteId { func_id, insn_id } =
                    InsnSiteId::unpack_from_slice(&**site).unwrap();
                (func_id, insn_id, *target)
            })
            .collect();
        let deref_paths = config.deref_paths;

        let alias_of_formal = compute_alias_of_formal(&facts.formal_param, copy_edge);
        log::debug!(
            "[mem cp] + alias_of_formal ({} rows), about to enter ascent_run: {:.1} MB",
            alias_of_formal.len(),
            phys_footprint_mb()
        );
        let mut all_program_paths = program_paths;
        all_program_paths.extend(
            facts
                .actual_param
                .iter()
                .map(|(_, _, FlowVertex(_, p, ..))| (*p,)),
        );
        // The receiver access path of an indirect / virtual call is also a syntactic program path.
        // Registering it lets `call_target_assign_like` propagate a stored target across an SSA
        // version of the receiver (the transitive rules gate on the path set). Without this, a
        // second store into the same aggregate (`o.a = id; o.b = id; o.a(s)` or
        // `fps[0]=id; fps[1]=id; fps[0](s)`) creates a new receiver version whose call path was
        // never an `actual_param`, so the binding fails to reach the call and taint is dropped.
        all_program_paths.extend(
            facts
                .callee_info
                .iter()
                .map(|(_, FlowVertex(_, p, ..), _)| (*p,)),
        );
        let paths = compute_paths(
            all_program_paths.into_iter().collect(),
            summary_paths.into_iter().collect(),
            deref_paths,
        );
        let path_set = PathSetRef(Arc::new(PathSet::from_paths(paths.iter().map(|(p,)| *p))));
        debug_assert!(
            path_set.len() == paths.len() && paths.iter().all(|(p,)| path_set.contains(p)),
            "the paths relation and the path set differ"
        );
        // The paths the intent rules match by name: intern them here, not mid-run.
        for p in [&COMPONENT_PATH, &ACTION_PATH, &TYPE_PATH] {
            LazyLock::force(p);
        }
        log::debug!(
            "[mem cp] + paths ({} rows), about to enter ascent_run: {:.1} MB",
            paths.len(),
            phys_footprint_mb()
        );
        log::info!("index engine: {parallelism}, {storage:?} storage");

        // Optional wall-clock cap on the fixpoint, gated by env var so normal runs are unaffected
        // (default `Duration::MAX`: run to fixpoint). Setting `CTADL_INDEX_TIMEOUT_SECS=<n>` stops
        // the semi-naive loop at the first iteration boundary past <n>s and returns; the
        // accumulated per-rule / per-scc times are then logged via `scc_times_summary()` in
        // `run_engine!`.
        // This is the tool for profiling which rule(s) dominate a run that does not terminate, as
        // if it hit a fixpoint.
        let index_timeout = std::env::var("CTADL_INDEX_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
            .unwrap_or(std::time::Duration::MAX);

        EngineInput {
            facts,
            call,
            config,
            assign_like,
            prog_store,
            alias_of_formal,
            paths,
            path_set,
            index_timeout,
            id_map,
            initial_assign,
            initial_call_target_assign,
            initial_summary,
            initial_formals,
            num_functions,
            num_variables,
        }
    }
}

/// The names `index_rules` uses. An included `ascent_source` resolves its names where it is
/// included, so each engine crate glob-imports this.
pub mod rules_prelude {
    pub use super::{
        ACTION_PATH, COMPONENT_PATH, Cond, ContextJoin, Decision, DecisionId, DecisionSet,
        HybridContext, IndexConfig, TYPE_PATH, decision, mime_matches, tag_splits,
    };
    pub use crate::call_arg;
    pub use crate::facts::{
        CallDispatchKey, CallTargetObject, FlowVariable, FlowVertex, FormalIndex, FormalType,
        FunctionId, InsnId, InsnSiteId, IntentKind, IntentPairKind, PackedInsnSiteId, Path, Str,
        isout,
    };
    pub use crate::index_engine::path_set::PathSetRef;
    pub use packed_struct::PackedStructSlice;
}

/// The function and instruction of a packed site, for [`run_engine!`](crate::run_engine), which
/// expands where `packed_struct`'s traits need not be in scope.
pub fn unpack_site(site: &PackedInsnSiteId) -> InsnSiteId {
    InsnSiteId::unpack_from_slice(&**site).unwrap()
}

/// Implements [`EngineStores`] for a program whose `locals_key` is stored in the serial or
/// concurrent trie (`locals_trie` or `c_locals_trie`).
#[macro_export]
macro_rules! trie_stores {
    ($prog_ty:ty) => {
        impl $crate::index_engine::EngineStores for $prog_ty {
            fn store_sizes(&self) -> $crate::index_engine::StoreSizes {
                $crate::index_engine::StoreSizes {
                    locals_key: self.__locals_key_ind_common.len(),
                }
            }
            fn dump_stores(&self, dump: &mut $crate::index_engine::dump::RelationDump<'_>) {
                dump.relation("locals_key", |w| {
                    self.__locals_key_ind_common.for_each_row(|r| w.row(&r))
                });
            }
        }
    };
}

/// Runs one engine: seeds the program `$prog_ty` from an [`EngineInput`], runs it to fixpoint, logs
/// what it found, and returns the [`IndexResult`].
///
/// Everything from seeding the inputs to extracting the result is written once, here, and
/// expanded once per engine crate. The program types spell their fields identically, but each
/// field's type differs by engine (`Vec` vs `boxcar::Vec`, `AssignTrie` vs `CAssignTrie`), so
/// no one function signature covers them all and a trait would only restate every field.
//
// Relation inputs are set here rather than as inline `= <init>` initializers, because the
// declared struct's `Default::default()`, where ascent would place those, has no access to
// these locals. They are `collect`ed rather than assigned, which costs
// nothing under `ascent!` (`Vec` to `Vec` reuses the allocation) and is what `ascent_par!`
// needs, where a plain relation's physical store is a `boxcar::Vec` (a lock-free append-only
// vector) instead of a `std::vec::Vec`.
#[macro_export]
macro_rules! run_engine {
    ($prog_ty:ty, $input:expr) => {{
        use $crate::index_engine::CondStores as _;
        use $crate::index_engine::EngineStores as _;
        let $crate::index_engine::EngineInput {
            facts,
            call,
            config,
            assign_like,
            prog_store,
            alias_of_formal,
            paths,
            path_set,
            index_timeout,
            id_map,
            initial_assign,
            initial_call_target_assign,
            initial_summary,
            initial_formals,
            num_functions,
            num_variables,
        } = $input;
        let mut prog = <$prog_ty>::default();
        prog.formal_param = facts.formal_param.into_iter().collect();
        prog.actual_param = facts.actual_param.into_iter().collect();
        prog.call = call.into_iter().collect();
        prog.call_target_assign = facts
            .call_target_assign
            .into_iter()
            .map(|(site_id, vx, obj)| {
                let $crate::facts::InsnSiteId { func_id, .. } = $crate::index_engine::unpack_site(&site_id);
                (func_id, vx, obj)
            })
            .collect();
        prog.callee_info = facts
            .callee_info
            .into_iter()
            .map(|(site_id, vx, dispatch_key)| {
                let $crate::facts::InsnSiteId { func_id, insn_id } = $crate::index_engine::unpack_site(&site_id);
                (func_id, insn_id, vx.0, vx.1, dispatch_key)
            })
            .collect();
        prog.callee_resolvents = facts.callee_resolvents.into_iter().collect();
        prog.summary = $crate::index_engine::FromLatticeRows::from_lattice_rows(
            facts
                .summary
                .into_iter()
                .map(|(f, n1, p1, n2, p2)| (f, n1, p1, n2, p2, $crate::index_engine::Cond::Always)),
        );
        prog.const_str_assign = facts
            .const_str_assign
            .into_iter()
            .map(|(site_id, vx, value)| {
                let $crate::facts::InsnSiteId { func_id, .. } = $crate::index_engine::unpack_site(&site_id);
                (func_id, vx.0, vx.1, value)
            })
            .collect();
        prog.update_sans_path = facts
            .update_sans_path
            .into_iter()
            .map(|(site_id, x, y, w)| {
                let $crate::facts::InsnSiteId { func_id, .. } = $crate::index_engine::unpack_site(&site_id);
                (func_id, x, y, w)
            })
            .collect();
        prog.intent_frame = facts.intent_frame.into_iter().collect();
        prog.intent_send = facts.intent_send.into_iter().collect();
        prog.intent_filter = facts.intent_filter.into_iter().collect();
        prog.intent_component = facts.intent_component.into_iter().collect();
        prog.config = vec![(config,)].into_iter().collect();
        prog.seed_assign_like(assign_like);
        prog.prog_store = prog_store.into_iter().collect();
        prog.alias_of_formal = alias_of_formal.into_iter().collect();
        prog.paths = paths.iter().cloned().collect();
        prog.path_set = vec![(path_set.clone(),)].into_iter().collect();
        // `fnptr_call` compares against `C`'s handle: intern it here, not mid-run.
        $crate::facts::CallDispatchKey::c();

        let interned_before = $crate::index_engine::profile::MemoryProfile::frozen_interner_entries();
        let fixpoint_start = std::time::Instant::now();
        let reached_fixpoint = prog.run_timeout(index_timeout);
        let fixpoint_secs = fixpoint_start.elapsed().as_secs_f64();
        let interned_after = $crate::index_engine::profile::MemoryProfile::frozen_interner_entries();
        if path_set.split_misses() > 0 {
            log::warn!(
                "the path set was asked to split {} non-member path(s); their wild rows are missing",
                path_set.split_misses()
            );
        }
        if !reached_fixpoint {
            log::warn!(
                "index run TIMED OUT after {:?} without reaching fixpoint; results and stats below are PARTIAL",
                index_timeout
            );
        }
        log::debug!(
            "[mem cp] ascent_run returned (transient input buffers dropped): {:.1} MB",
            $crate::index_engine::phys_footprint_mb()
        );
        log::debug!("index scc times: {}", prog.scc_times_summary());
        // Every relation's tuple count, straight from ascent's own generated summary. This
        // is the census the rule-time ranking is normalized against (time per tuple), so it
        // has to cover ALL relations, not the hand-picked few the `propagation relations:`
        // line below lists. Logged BEFORE the `assign_like` store is drained into the output
        // Vec, so the counts describe the fixpoint's state at the moment it stopped --
        // which, under `CTADL_INDEX_TIMEOUT_SECS`, is the partial state we are trying to
        // characterize.
        //
        // `locals_key` reads 0 here and is reported separately on the next line: it lives in a
        // BYODS store, so its physical relation holds no tuples.
        log::debug!(
            "[relsizes] index relation sizes:\n{}",
            prog.relation_sizes_summary()
        );
        let sizes = prog.store_sizes();
        let cond_sizes = prog.cond_sizes();
        log::debug!(
            "[relsizes] byods-backed (rows, conditional): assign_like {:?}, locals {:?}, \
             edge_split {:?}, ext_dst {:?}; locals_key size: {}",
            cond_sizes.assign_like,
            cond_sizes.locals,
            cond_sizes.edge_split,
            cond_sizes.ext_dst,
            sizes.locals_key
        );
        let locals_heap = prog.locals_heap_report();
        let mut profile = $crate::index_engine::profile::IndexProfile {
            reached_fixpoint,
            timeout_secs: (index_timeout != std::time::Duration::MAX).then(|| index_timeout.as_secs()),
            fixpoint_secs,
            ..$crate::index_engine::profile::IndexProfile::from_summaries(
                &prog.relation_sizes_summary(),
                &prog.scc_times_summary(),
                &[
                    ("assign_like", cond_sizes.assign_like.0),
                    ("locals", cond_sizes.locals.0),
                    ("edge_split", cond_sizes.edge_split.0),
                    ("locals_key", sizes.locals_key),
                    ("ext_dst", cond_sizes.ext_dst.0),
                ],
            )
        };
        log::debug!(
            "[mem cp] after relation census (nothing drained yet): {:.1} MB",
            $crate::index_engine::phys_footprint_mb()
        );
        log::debug!(
            "propagation relations: reach_vp={} locals_key={} locals_key_wild={} locals_wild={} \
             assign_wild={} edge_split={} edge_split_wild={} ext_dst={} ext_fml={}",
            prog.reach_vp.len(),
            sizes.locals_key,
            prog.locals_key_wild.len(),
            prog.locals_wild.len(),
            prog.assign_wild.len(),
            cond_sizes.edge_split.0,
            prog.edge_split_wild.len(),
            cond_sizes.ext_dst.0,
            prog.ext_fml.len()
        );
        log::debug!("{locals_heap}");
        log::debug!("{}", prog.cond_heap_report());
        profile.memory.locals = Some(locals_heap);
        profile.memory.fixpoint_interned = interned_after
            .into_iter()
            .map(|(name, n)| (name, n - interned_before[name]))
            .collect();
        profile.memory.interned.insert(
            "PathSet",
            $crate::index_engine::profile::Stats {
                entries: path_set.len(),
                bytes: path_set.bytes(),
            },
        );
        profile.split_misses = path_set.split_misses();
        if let Some(mut dump) = $crate::index_engine::dump::RelationDump::from_env(id_map) {
            $crate::dump_relations!(&mut dump, prog);
            prog.dump_stores(&mut dump);
            prog.dump_cond_stores(&mut dump);
            dump.finish(&prog.relation_sizes_summary());
        }
        log::trace!(
            "hybrid inlining relations:\n{}",
            $crate::index_engine::HybridInliningRelations {
                critical_summary: &prog.critical_summary,
                resolvent: &prog.resolvent,
                call_target_assign_like: &prog.call_target_assign_like,
                assign_like: &*prog.assign_like_rows(),
                locals: &*prog.locals_rows(),
                summary: &prog.summary,
                id_map,
            }
        );

        let assign_like_out = prog.take_assign_like();
        let summary_out = $crate::index_engine::always_rows(&prog.summary);

        let stats = $crate::index_engine::IndexStats {
            profile,
            initial_assign,
            final_assign_like: assign_like_out.len(),
            initial_formals,
            final_locals: cond_sizes.locals.0 - cond_sizes.locals.1,
            initial_call_target_assign,
            final_call_target_assign_like: prog.call_target_assign_like.len(),
            initial_summary,
            final_summary: summary_out.len(),
            num_functions,
            num_variables,
            reached_variables: cond_sizes.reached_variables,
            initial_callee_info: prog.callee_info.len(),
            final_resolved_call: prog.resolved_call.len(),
            hybrid_critical_summary: prog.critical_summary.len(),
            hybrid_resolvent: prog.resolvent.len(),
            hybrid_context_assign: cond_sizes.assign_like.1,
            hybrid_context_locals: cond_sizes.locals.1,
            hybrid_context_summary: $crate::index_engine::conditional_count(&prog.summary),
        };
        stats.log();
        if stats.hybrid_context_locals > 0 {
            log::debug!(
                "{}",
                $crate::index_engine::context_histogram(&*prog.locals_rows(), &prog.resolvent, id_map).trim_end()
            );
        }

        $crate::index_engine::IndexResult {
            summary: summary_out,
            assign_like: assign_like_out,
            update_sans_path: prog.update_sans_path.into_iter().collect(),
            call_target_assign_like: prog.call_target_assign_like.into_iter().collect(),
            resolved_call: prog.resolved_call.into_iter().collect(),
            const_reaches: prog.const_reaches.into_iter().collect(),
            intent_pair: prog.intent_pair.into_iter().collect(),
            paths: prog.paths.into_iter().collect(),
            external_function: facts.external_function,
            stats,
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// With `deref_paths` (a native-language import), `compute_paths` mints `<program
    /// path>.deref` on its own. The store `o->p = b` carries the pointee only onto `p.deref`,
    /// which no single frame spells; with no summary loaded there is no model path to
    /// concatenate, so without the flag the flow through a pointer stored in a field is lost
    /// (`ctadl-ascent/tests/c/ptrfield.c`).
    #[test]
    fn compute_paths_admits_deref_of_every_program_path_for_native_imports() {
        let p = |s: &str| (Path::parse(s).unwrap(),);
        let program = vec![(Path::empty(),), p(".p"), p(".q")];
        let paths = compute_paths(program.clone(), vec![], true);
        assert!(paths.contains(&p(".p.deref")), "{paths:?}");
        assert!(paths.contains(&p(".q.deref")), "{paths:?}");

        // Off (a Java or Lua import): nothing is invented, the path set is unchanged.
        let paths = compute_paths(program.clone(), vec![], false);
        assert_eq!(paths.len(), program.len(), "{paths:?}");
        assert!(!paths.contains(&p(".p.deref")));

        // A loaded summary that already mentions `.deref` makes the flag a no-op.
        let with_model = compute_paths(program.clone(), vec![p(".deref")], false);
        let with_flag = compute_paths(program, vec![], true);
        assert_eq!(with_model, with_flag);
    }
}
