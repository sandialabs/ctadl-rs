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

use ascent::ascent;
use ascent::ascent_run;
use derive_builder::Builder;
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;
use streaming_iterator::StreamingIterator;

use crate::error::Error;
use crate::facts::{
    CallArgId, CallDispatchKey, CallString, CallTargetObject, FlowVariable, FlowVariableKind,
    FlowVertex, FormalIndex, FormalType, FunctionId, IdMap, InsnId, InsnSiteId, PackedCallArg,
    PackedInsnSiteId, Path, SmallestCallString, isout,
};
use crate::index_engine::assign_like_trie::FromRows;

pub mod assign_like_trie;
pub mod c_assign_like_trie;
pub mod c_locals_trie;
pub mod hybrid_set;
pub mod locals_trie;
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
    /// Functions a `modes: ["skip-analysis"]` model marked. The indexer derives nothing from
    /// their bodies; their `summary` rows are whatever the model gave them.
    ///
    /// Not saved with the other facts, and deliberately: it is a *model* input, and the model
    /// files are re-read on every `ctadl index`. Nothing loads the fact base back to re-run the
    /// fixpoint.
    #[builder(default)]
    pub skip_analysis: Vec<(FunctionId,)>,
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
    pub alias_rule: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig { alias_rule: true }
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
type ResolventRow = (
    FunctionId,
    FormalIndex,
    Path,
    CallTargetObject,
    SmallestCallString,
);
type CallTargetAssignLikeRow = (FunctionId, FlowVariable, Path, CallTargetObject);
type ContextAssignRow = (
    FunctionId,
    FlowVariable,
    Path,
    FlowVariable,
    Path,
    SmallestCallString,
);
type ContextLocalsRow = (
    FunctionId,
    FlowVariable,
    Path,
    FormalIndex,
    Path,
    SmallestCallString,
);
type ContextSummaryRow = (
    FunctionId,
    FormalIndex,
    Path,
    FormalIndex,
    Path,
    SmallestCallString,
);

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
        while let Some((func_id, formal_index, path, resolvent, cs)) = rows.next() {
            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");
            let cs = match cs {
                SmallestCallString::Value(cs) => cs.to_string(),
                SmallestCallString::Bottom => "⊥".to_string(),
            };

            writeln!(
                f,
                "  {} {}({}): arg{} {} resolves to {}",
                cs,
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
        while let Some((func_id, dest_var, dest_path, src_var, src_path, cs)) = rows.next() {
            let cs = match cs {
                SmallestCallString::Value(cs) => cs.to_string(),
                SmallestCallString::Bottom => "⊥".to_string(),
            };
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
        while let Some((func_id, var, path, formal_idx, formal_path, cs)) = rows.next() {
            let cs = match cs {
                SmallestCallString::Value(cs) => cs.to_string(),
                SmallestCallString::Bottom => "⊥".to_string(),
            };
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
        while let Some((func_id, dest_idx, dest_path, src_idx, src_path, cs)) = rows.next() {
            let cs = match cs {
                SmallestCallString::Value(cs) => cs.to_string(),
                SmallestCallString::Bottom => "⊥".to_string(),
            };
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
/// copies. A variable `v` aliases formal `i` (in function `f`) when `v` is reachable from `f`'s
/// formal purely through whole-variable copies (`copy_edge`) present in the original program.
///
/// This depends only on `formal_param` and `copy_edge`, so it is computed in its own small fixpoint
/// BEFORE the main ascent. Doing so keeps `copy_edge` out of the main engine entirely (one fewer
/// relation + index there) and lets the aliasing summary rule consume `alias_of_formal` as a plain
/// pre-populated input.
fn compute_alias_of_formal(
    formal_param: &[(FunctionId, FlowVariable, FormalType)],
    copy_edge: Vec<(FunctionId, FlowVariable, FlowVariable)>,
) -> Vec<(FunctionId, FlowVariable, FormalIndex)> {
    // Base case: a formal whole-aliases itself.
    let alias_base: Vec<_> = formal_param
        .iter()
        .filter_map(|(infunc, v1, _)| v1.as_formal().map(|i| (*infunc, *v1, i)))
        .collect();
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

// The `ascent!` datalog block below expands to code that trips several style lints
// (`.clone()` on Copy types, auto-borrows, unit-valued lets, Default field
// reassignment, and a unit binding per negation clause -- `!rel(..)` desugars to
// `agg () = not() in rel(..)`). These are artifacts of the macro's generated code, not the
// hand-written rules, so silence them for this function.
#[allow(
    clippy::clone_on_copy,
    clippy::needless_borrow,
    clippy::let_unit_value,
    clippy::unused_unit,
    clippy::field_reassign_with_default
)]
pub fn taint_index_with_config(
    facts: IndexFacts,
    config: IndexConfig,
    id_map: Option<&IdMap>,
) -> IndexResult {
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
    let call = facts
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

    ascent! {
        #![measure_rule_times]
        #![generate_run_timeout]
        struct IndexProg;
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

        // Set of syntactic access paths
        relation paths(Path);
        relation summary(FunctionId, FormalIndex, Path, FormalIndex, Path);
        relation config(IndexConfig);
        // Functions a `modes: ["skip-analysis"]` model marked. An input relation: nothing derives
        // it, so its negation below sits in an earlier stratum and costs the fixpoint nothing.
        relation skip_analysis(FunctionId);

        // Derived:

        // Local reachability. The core, most expensive relation.
        #[ds(crate::index_engine::locals_trie)]
        relation locals(FunctionId, FlowVariable, Path, FormalIndex, Path);
        #[ds(crate::index_engine::assign_like_trie)]
        relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path);
        // Real program field-stores (`v.p = ...`, non-empty destination path). Gates the aliasing rule.
        relation prog_store(FunctionId, FlowVariable, Path);
        // A variable that whole-aliases a formal purely through original program copies. This is
        // the copy-closure of `locals(v, empty, formal, empty)` restricted to original assigns:
        // it finds strictly fewer aliases (drops inter-procedural / summary-derived copies) but
        // ALL aliases established by original program assignments. Feeds the aliasing summary rule.
        // Precomputed above in its own fixpoint (see the `alias_of_formal` let-binding).
        relation alias_of_formal(FunctionId, FlowVariable, FormalIndex);
        // Call targets (function pointers and Java objects) propagated across `assign_like`
        // to the receiver vertices of critical calls; the union of what were the separate
        // `func_ptr_assign_like` and `java_obj_assign_like` relations.
        relation call_target_assign_like(FunctionId, FlowVariable, Path, CallTargetObject);
        relation model_paths(Path);
        relation program_paths(Path);

        // Hybrid Inlining relations: critical_summary(f, n, p). f(n.p = obj) invokes obj at some
        // critical call site. The critical site itself is not tracked here; it can be recovered by
        // the Contextual Assignment rule.
        relation critical_summary(FunctionId, FormalIndex, Path);
        // Critical call occurs inside this function
        relation critical_call(FunctionId);
        // Resolvent reaches the formals of Function. The call string is a lattice value so the
        // remaining columns functionally determine at most one call string: (func_id, formal_index,
        // path, object) -> call-string. This way we don't get the same object resolved through
        // multiple stack configuration paths. Invariant: the call string is non-empty.
        lattice resolvent(FunctionId, FormalIndex, Path, CallTargetObject, SmallestCallString);
        // Assignment due to instantiating a summary from a resolvent. Invariant: call string is
        // non-empty.
        lattice context_assign(FunctionId, FlowVariable, Path, FlowVariable, Path, SmallestCallString);
        // Context-carrying field-sensitive reachability. Seeded from `context_assign` and
        // propagated by its own forward-field rules below. Rare on C targets (0 rows when there is
        // no resolvable indirect/virtual dispatch); the lattice column keeps the smallest call
        // string per (func,var,path,formal,fpath). Invariant: call string is non-empty.
        lattice context_locals(FunctionId, FlowVariable, Path, FormalIndex, Path, SmallestCallString);
        // Invariant: call string is non-empty.
        lattice context_summary(FunctionId, FormalIndex, Path, FormalIndex, Path, SmallestCallString);

        // Sets up paths from input program with static info. Paths must remain finite so we
        // shouldn't add paths from constructed summaries directly.
        program_paths(p) <-- actual_param(_, _, vx), let FlowVertex(_, p) = vx;
        // The receiver access path of an indirect / virtual call is also a syntactic
        // program path. Register it so that `call_target_assign_like`
        // can propagate a stored target across an SSA version of the receiver (the
        // transitive rules gate on `paths(p_new)`). Without this, a second store into the
        // same aggregate (`o.a = id; o.b = id; o.a(s)` or `fps[0]=id; fps[1]=id; fps[0](s)`)
        // creates a new receiver version whose call path was never an `actual_param`, so the
        // binding fails to reach the call and taint is dropped (F2).
        program_paths(p) <-- callee_info(_, _, _, p, _);
        paths(p) <-- program_paths(p);
        paths(p) <-- model_paths(p);

        // Combine model paths with program paths (one level only to ensure termination)
        paths(p1.concat(p2)) <-- model_paths(p1), program_paths(p2);
        paths(p2.concat(p1)) <-- program_paths(p2), model_paths(p1);

        // Initialize locals with formals (context-free).
        //
        // The `!skip_analysis` guard is the ONE place a `modes: ["skip-analysis"]` model takes
        // effect, and everything else follows from it. `locals` is what every body-derived
        // relation drives on -- the two forward-field rules below, both `summary` rules, rule 1.1
        // for `critical_summary`, and rule 3.3b for `context_locals` -- so refusing the seed
        // leaves a skipped function with no `locals` at all and therefore no derivation from its
        // body. What survives is exactly the model: `facts.summary` rows are an input, and the
        // call-site instantiation rule reads `summary(tgt, ..)` without touching `locals(tgt, ..)`,
        // so callers still compose with the hand-written behaviour.
        //
        // Guarding here rather than on each of those rules is not just economy: the guard sits on
        // a rule that fires once per formal, off the hot path entirely, where a guard on the
        // forward-field rules would be tested billions of times.
        locals(infunc, v1, p1.clone(), i, p1.clone()) <--
            formal_param(infunc, v1, _),
            if let Some(i) = v1.as_formal(),
            !skip_analysis(infunc),
            let p1 = Path::empty();

        // Forward field propagation (context-free).
        locals(infunc, v1, p13.clone(), a, p4) <--
            locals(infunc, v2, p23, a, p4),
            assign_like(infunc, v1, p1, v2, p2),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(&p13);
        locals(infunc, v1, p1, a, p43.clone()) <--
            locals(infunc, v2, p2, a, p4),
            assign_like(infunc, v1, p1, v2, p23),
            if let Some(p43) = p23.substitute_prefix(p2, p4),
            paths(&p43);

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
        // Phase 2: propagate resolvents back down (this requires call strings)
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
        resolvent(f, n, p.clone(), cto.clone(), cs_lat) <--
            critical_summary(f, n, p),
            call(caller, call_insn, f),
            let arg = call_arg!(*call_insn, *n),
            call_target_assign_like(caller, arg, p, cto),
            let call_site_id = PackedInsnSiteId::try_from_parts(*caller, *call_insn).unwrap(),
            if let Some(new_cs) = CallString::new().push(call_site_id),
            let cs_lat = SmallestCallString::Value(new_cs);

        // Tracks resolvent to the call arg. Invariant: `call_arg_resolvent` call string is
        // non-empty.
        relation call_arg_resolvent(PackedCallArg, Path, CallTargetObject, SmallestCallString);
        call_arg_resolvent(arg_p, p, obj, cs_lat) <--
            resolvent(f, n2, p2, obj, cs_lat),
            locals(f, v, p, n2, p2),
            if let Some(arg_p) = v.as_call_arg();

        // 2.2: Propagate Resolvent down the critical summaries, pushing call site
        resolvent(f, n, p.clone(), resolvent_obj, SmallestCallString::Value(new_cs)) <--
            call_arg_resolvent(arg_p, p, resolvent_obj, cs_lat),
            let arg = CallArgId::unpack_from_slice(&**arg_p).unwrap(),
            call(caller, arg.insn_id, f),
            let n = FormalIndex::new(arg.formal),
            critical_summary(f, n, p),
            if let SmallestCallString::Value(cs) = cs_lat,
            let call_site_id = PackedInsnSiteId::try_from_parts(*caller, arg.insn_id).unwrap(),
            if let Some(new_cs) = cs.push(call_site_id);

        // 3.1: Contextual Assignment (seed). Given a resolvent that reaches a call site,
        // instantiate a contextual assignment doing normal summary instantiation. The resolvent
        // object itself and the dispatch key from the call site are used to determine the resolvent
        // function. The context of the resolvent is associated with the assign.
        context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), cs_lat.clone()) <--
            callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
            locals(caller, v_rec, p_rec, n, p),
            resolvent(caller, n, p, resolvent_obj, cs_lat),
            callee_resolvents(resolvent_obj, dispatch_key, resolvent_func),
            summary(resolvent_func, n1_sum, p1_sum, n2_sum, p2_sum),
            let v2 = call_arg!(*call_insn, *n2_sum),
            let v1 = call_arg!(*call_insn, *n1_sum);

        // 3.2: Contextual assignment (chain). Instantiate contextual summaries and pop call string,
        // either creating a new contextual assign or a bare, uncontextual assign
        context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), new_cs_lat) <--
            context_summary(f, n1, p1_sum, n2, p2_sum, cs_lat),
            if let SmallestCallString::Value(cs) = cs_lat,
            if let (new_cs, Some(call_site_id)) = cs.pop(),
            if !new_cs.is_empty(), // keep call string invariant
            let InsnSiteId {func_id: caller, insn_id} = InsnSiteId::unpack_from_slice(&*call_site_id).unwrap(),
            call(caller, insn_id, f),
            let v1 = call_arg!(insn_id, *n1),
            let v2 = call_arg!(insn_id, *n2),
            let new_cs_lat = SmallestCallString::Value(new_cs);

        assign_like(caller, v1.clone(), p1_sum.clone(), v2.clone(), p2_sum.clone()) <--
            context_summary(tgt, n1, p1_sum, n2, p2_sum, cs_lat),
            if let SmallestCallString::Value(cs) = cs_lat,
            if let (new_cs, Some(call_site_id)) = cs.pop(),
            if new_cs.is_empty(),
            let InsnSiteId {func_id: caller, insn_id} = InsnSiteId::unpack_from_slice(&*call_site_id).unwrap(),
            call(caller, insn_id, tgt),
            let v1 = call_arg!(insn_id, *n1),
            let v2 = call_arg!(insn_id, *n2);

        // 3.3a: We have to reason about contextual local reachability. This involves reasoning
        // about flows composed of hops, some of which are non-contextual and some of which are
        // contextual. The following two rules extend contextual flows with built-in assigns using
        // the same shape as forward/backward propagation.
        context_locals(infunc, v1, p13.clone(), a, p4, cs_lat.clone()) <--
            context_locals(infunc, v2, p23, a, p4, cs_lat),
            assign_like(infunc, v1, p1, v2, p2),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(&p13);
        context_locals(infunc, v1, p1, a, p43.clone(), cs_lat.clone()) <--
            context_locals(infunc, v2, p2, a, p4, cs_lat),
            assign_like(infunc, v1, p1, v2, p23),
            if let Some(p43) = p23.substitute_prefix(p2, p4),
            paths(&p43);

        // 3.3b: The following two rules extend non-contextual flows with contextual assigns.
        context_locals(func_id, v1.clone(), p13.clone(), a.clone(), p4.clone(), cs_lat.clone()) <--
            context_assign(func_id, v1, p1, v2, p2, cs_lat),
            locals(func_id, v2, p23, a, p4),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(&p13);
        context_locals(func_id, v1.clone(), p1.clone(), a.clone(), p43.clone(), cs_lat.clone()) <--
            context_assign(func_id, v1, p1, v2, p23, cs_lat),
            locals(func_id, v2, p2, a, p4),
            if let Some(p43) = p23.substitute_prefix(p2, p4),
            paths(&p43);

        // 3.4: a context-specific flow that reaches an out-formal becomes a conditional summary
        // tagged with its call string; contextual assignment (chain) pops it back to the caller.
        context_summary(func_id, n1.clone(), p1.clone(), n2.clone(), p2.clone(), cs_lat.clone()) <--
            context_locals(func_id, dst_var, p1, n2, p2, cs_lat),
            formal_param(func_id, dst_var, formal_ty),
            if let Some(n1) = dst_var.as_formal(),
            if isout(&n1, *formal_ty, p1),
            if n1 != *n2 || p1 != p2;

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
        // No call string is needed: the head names the specific `insn`, so each call site of a
        // factory gets its own tagged vertex; context sensitivity is inherent to this direction.
        // `p` rides through unchanged (no `substitute_prefix`), so no path growth and no `paths`
        // gate. `isout` holds for every negative formal index, so RETURN_INDEX and the multi-return
        // slots all qualify, and by-ref out-params (a callee installing a target into a
        // caller-owned object) fall out for free.
        //
        // Deliberately NOT seeding `resolvent` here: that is a callee-frame relation keyed on the
        // callee's formals, so a tuple for a frame whose receiver is a local has no consumer, and
        // it would bypass the `CallString::new().push(..)` construction. Downstream needs no
        // change -- the transitive rule above walks this tuple from `call_arg(insn, -1)` to the
        // receiver over the symmetric call-site `assign_like` edges, the local-dispatch bypass
        // resolves the indirect call exactly, and if the receiver is passed onward rule 2.1
        // derives the resolvent with a properly constructed call string.
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

    // Declared-struct form (was `ascent_run!`) so we get `run_timeout`. Relation inputs that
    // used to be inline `= <init>` initializers are set here instead, because the declared-struct
    // `Default::default()` where ascent would otherwise place them has no access to these locals.
    //
    // The inputs are `collect`ed rather than assigned, which costs nothing here (`Vec` to `Vec`
    // reuses the allocation) and is what `ascent_par!` needs, where a plain relation's physical
    // store is a `boxcar::Vec` (a lock-free append-only vector) instead of a `std::vec::Vec`. So
    // these lines hold for either macro.
    let mut prog = IndexProg::default();
    prog.formal_param = facts.formal_param.into_iter().collect();
    prog.actual_param = facts.actual_param.into_iter().collect();
    prog.call = call;
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
    prog.skip_analysis = facts.skip_analysis.into_iter().collect();
    prog.config = config_val.into_iter().collect();
    // Seeding goes through the `FromRows` trait rather than naming a store type, so this line is
    // the same under `ascent!` and `ascent_par!`: the field's type selects the serial `AssignTrie`
    // or the concurrent `CAssignTrie` impl.
    prog.__assign_like_ind_common = FromRows::from_rows(assign_like);
    prog.prog_store = prog_store.into_iter().collect();
    prog.alias_of_formal = alias_of_formal.into_iter().collect();
    prog.model_paths = summary_paths.into_iter().collect();
    prog.program_paths = program_paths.into_iter().collect();

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

    let result = IndexResult {
        summary: prog.summary.into_iter().collect(),
        assign_like: assign_like_out,
        call_target_assign_like: prog.call_target_assign_like.into_iter().collect(),
        paths: prog.paths.into_iter().collect(),
        external_function: facts.external_function,
        stats,
    };
    log::trace!("index result: {}", result.display(id_map));
    log::debug!(
        "flow variable size: {}",
        std::mem::size_of::<FlowVariable>()
    );
    result
}
