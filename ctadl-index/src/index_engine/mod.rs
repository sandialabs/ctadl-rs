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

`Path`, `Str`, `CallTargetObject` and `CallDispatchKey` are 4-byte handles into process-wide
interners that never free. Before the fixpoint starts, building an index interns
every `Path` and `Str` the rules can produce: the path set's members and their splits
([`path_set`]) and the paths the intent rules match by name. The fixpoint only reads them,
without a lock. `profile.json`
reports each interner's size, and how much the read-only ones grew during the fixpoint.

*/

use std::num::NonZeroUsize;
use std::path;
use std::sync::{Arc, LazyLock};

use ascent::ascent_run;
use ascent::ascent_source;
use ctadl_ir::cir::PathSegment;
use derive_builder::Builder;
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;
use streaming_iterator::StreamingIterator;

use crate::error::{Error, ErrorContext};
use crate::facts::{
    CallDispatchKey, CallTargetObject, FlowVariable, FlowVariableKind,
    FlowVertex, FormalIndex, FormalType, FunctionId, GlobalId, IdMap, ImportId, InsnId, InsnSiteId,
    IntentKind, IntentPairKind, PackedInsnSiteId, Path, Root, Str,
};
use crate::index_engine::path_set::{PathSet, PathSetRef};

pub mod c_locals_trie;
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
lsm::lsm_col!(FunctionId, [u32]);
lsm::lsm_col!(FlowVariable, [u64], valid = FlowVariable::is_valid);
lsm::lsm_col!(Path, [std::num::NonZeroU32], valid = Path::is_valid);
lsm::lsm_col!(FormalIndex, [i16]);
lsm::lsm_col!(Root, [u32], valid = Root::is_valid);
lsm::lsm_col!(PackedInsnSiteId, [[u8; 8]]);
lsm::lsm_col!(
    FlowVertex,
    [FlowVariable, Path, crate::facts::VertexPad],
    valid = FlowVertex::is_valid
);

/// An assignment statement. The order is destination vertex then source vertex.
pub type AssignFlow = (PackedInsnSiteId, FlowVertex, FlowVertex);
/// A function's flow from root `src.p2` to root `dst.p1`: `(f, dst, p1, src, p2)`.
pub type FunctionSummary = (FunctionId, Root, Path, Root, Path);
/// A program-level flow between two global paths: `(dst, p1, src, p2)`.
pub type GlobalFlow = (GlobalId, Path, GlobalId, Path);

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
    /// The project's globals, as [`crate::facts::schema::globals::Record`] rows.
    #[builder(default)]
    pub globals: Vec<(GlobalId, ImportId, Option<::source_info::FileSpanId>)>,
    /// Program-level flows between global paths loaded from a `--summary` project, seeded
    /// as edges of [`crate::facts::GLOBAL_FRAME`].
    #[builder(default)]
    pub global_flow: Vec<GlobalFlow>,
}

impl IndexFacts {
    /// Saves the `formal_param`, `actual_param`, `call`, `call_target_assign`, `callee_info`,
    /// `callee_resolvents`, `external_function` and `globals` members. The others aren't saved because
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
        globals::try_save(&dir, self.globals.iter().copied())?;
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
            .external_function(external_function::try_load(&dir)?)
            .globals(globals::try_load(&dir)?);
        Ok(builder.build().unwrap())
    }

    /// The number of globals each import declares, and the globals more than one import
    /// declares.
    pub fn global_declarations(
        &self,
    ) -> (
        std::collections::BTreeMap<ImportId, usize>,
        Vec<(GlobalId, Vec<ImportId>)>,
    ) {
        let mut per_import: std::collections::BTreeMap<ImportId, usize> = Default::default();
        let mut by_global: HashMap<GlobalId, Vec<ImportId>> = HashMap::new();
        for (g, import, _) in &self.globals {
            let imports = by_global.entry(*g).or_default();
            if !imports.contains(import) {
                imports.push(*import);
                *per_import.entry(*import).or_default() += 1;
            }
        }
        let mut shared: Vec<_> = by_global.into_iter().filter(|(_, i)| i.len() > 1).collect();
        shared.sort_unstable_by(|a, b| a.0.content_cmp(&b.0));
        (per_import, shared)
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
            parallelism: Parallelism::Serial,
            storage: Storage::Memory,
            deref_paths: false,
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
/// `assign_like`, `edge_split` and `ext_dst` are in the in-memory [`locals_trie`] stores under
/// both; every other relation is an ordinary in-memory Ascent relation either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Storage {
    /// The in-memory trie of [`locals_trie`].
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
    pub final_resolved_call: usize,
    pub initial_summary: usize,
    pub final_summary: usize,
    pub num_functions: usize,
    pub num_variables: usize,
    /// Distinct variables appearing as the subject of a `locals` row, i.e. reached by
    /// some formal. Bounded above by `num_variables`.
    pub reached_variables: usize,
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
            "call resolution: {} derived (site, target) pair(s)",
            self.final_resolved_call
        );
        log::debug!(
            "relation increase: summary: {:.2} ({}/{}) (ratio over num_functions)",
            ratio(self.final_summary, self.num_functions),
            self.final_summary,
            self.num_functions
        );
    }
}

#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Summary goes from formal parameter index to formal ret index.
    pub summary: Vec<FunctionSummary>,
    pub assign_like: Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)>,
    pub resolved_call: Vec<(FunctionId, InsnId, FunctionId)>,
    pub const_reaches: Vec<(FunctionId, FlowVariable, Path, Str)>,
    pub intent_pair: Vec<(FunctionId, InsnId, FunctionId, IntentPairKind)>,
    pub paths: Vec<(Path,)>,
    pub external_function: Vec<(FunctionId,)>,
    pub global_flow: Vec<GlobalFlow>,
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
        let paths_rows = self.paths.len();
        paths::try_save(&dir, self.paths)?;
        log::debug!(
            "[mem cp]   after paths::try_save ({} rows): {:.1} MB",
            paths_rows,
            phys_footprint_mb()
        );
        external_function::try_save(&dir, self.external_function)?;
        global_flow::try_save(&dir, self.global_flow)?;
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
        let paths = paths::try_load(&dir)?;
        let external_function = external_function::try_load(&dir)?;
        let resolved_call = resolved_call::try_load(&dir)?;
        let intent_pair = intent_pair::try_load(&dir)?;
        let global_flow = global_flow::try_load(&dir)?;
        Ok(IndexResult {
            summary,
            assign_like,
            resolved_call,
            const_reaches: Vec::new(),
            intent_pair,
            paths,
            external_function,
            global_flow,
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

/// One relation's rows, whatever the index engine happens to store them in: a `Vec` under
/// `ascent!`, a `boxcar::Vec` (a lock-free append-only vector) under `ascent_par!`. The relation
/// dump reads both through this trait.
///
/// The count is its own method rather than something taken from the stream, because `boxcar::Vec`
/// reports no upper size bound when iterated: the vector can be appended to mid-iteration, so its
/// iterator cannot be an `ExactSizeIterator`.
pub trait Rows<Row> {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stream the rows, each borrowed for as long as it is looked at.
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

// The parallel-engine case. Dead code under `ascent!` and live under `ascent_par!`; keeping it
// compiled is what lets the switch between the macros touch nothing here.
impl<Row> Rows<Row> for ascent::boxcar::Vec<Row> {
    fn len(&self) -> usize {
        ascent::boxcar::Vec::len(self)
    }

    fn stream(&self) -> Box<dyn StreamingIterator<Item = Row> + '_> {
        Box::new(streaming_iterator::convert_ref(self.iter()))
    }
}

/// The rows, each once: Ascent keeps a seeded field as given, so a row seeded twice stays twice.
pub fn distinct_rows<Row: Copy + Eq + std::hash::Hash>(
    rows: impl IntoIterator<Item = Row>,
) -> impl Iterator<Item = Row> {
    let mut seen = hashbrown::HashSet::new();
    rows.into_iter().filter(move |r| seen.insert(*r))
}

/// The edges of the global frame: the program-level flows between global paths.
pub fn global_flow_rows(assign_like: &[AssignLikeRow]) -> Vec<GlobalFlow> {
    assign_like
        .iter()
        .filter(|(f, ..)| *f == crate::facts::GLOBAL_FRAME)
        .filter_map(|(_, v1, p1, v2, p2)| Some((v1.as_global()?, *p1, v2.as_global()?, *p2)))
        .collect()
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

/// The store-backed relations every storage has, other than the [`TrieStores`].
pub struct StoreSizes {
    pub locals_key: usize,
    /// The bytes each LSM-stored relation spends per row on its tuple, by relation name. Empty
    /// for the storages that hold no relation in LSM stores.
    pub bytes_per_row: Vec<(&'static str, usize)>,
}

/// What `run_engine!` does with the [`locals_trie`] stores of `locals`, `assign_like`,
/// `edge_split` and `ext_dst`, serial or concurrent. Implemented by
/// [`trie_relations!`](crate::trie_relations).
pub trait TrieStores {
    /// Seeds `assign_like` with the program's assignments before the run.
    fn seed_assign_like(&mut self, rows: Vec<AssignLikeRow>);
    /// Takes `assign_like`'s rows out, after the run.
    fn take_assign_like(&mut self) -> Vec<AssignLikeRow>;
    fn trie_sizes(&self) -> TrieSizes;
    /// The heap of the `locals` store.
    fn locals_heap_report(&self) -> locals_trie::HeapReport;
    /// Writes the four relations, whose fields hold no rows, to `dump`.
    fn dump_trie_stores(&self, dump: &mut dump::RelationDump<'_>);
}

/// Row counts of the [`TrieStores`].
pub struct TrieSizes {
    pub locals: usize,
    pub assign_like: usize,
    pub edge_split: usize,
    pub ext_dst: usize,
    /// The distinct `(f, v)` a `locals` row reaches.
    pub reached_variables: usize,
}

/// Implements [`TrieStores`] for a program, serial or concurrent: the stores of both have the
/// same methods.
#[macro_export]
macro_rules! trie_relations {
    ($prog_ty:ty) => {
        impl $crate::index_engine::TrieStores for $prog_ty {
            fn seed_assign_like(&mut self, rows: Vec<$crate::index_engine::AssignLikeRow>) {
                // Stored source first.
                self.__assign_like_ind_common.extend_rows(
                    rows.into_iter().map(|(f, v1, p1, v2, p2)| (f, v2, p2, v1, p1)),
                );
            }
            fn take_assign_like(&mut self) -> Vec<$crate::index_engine::AssignLikeRow> {
                std::mem::take(&mut self.__assign_like_ind_common)
                    .into_rows()
                    .into_iter()
                    .map(|(f, v2, p2, v1, p1)| (f, v1, p1, v2, p2))
                    .collect()
            }
            fn trie_sizes(&self) -> $crate::index_engine::TrieSizes {
                $crate::index_engine::TrieSizes {
                    locals: self.__locals_ind_common.len(),
                    assign_like: self.__assign_like_ind_common.len(),
                    edge_split: self.__edge_split_ind_common.len(),
                    ext_dst: self.__ext_dst_ind_common.len(),
                    reached_variables: self.__locals_ind_common.num_reached_variables(),
                }
            }
            fn locals_heap_report(&self) -> $crate::index_engine::locals_trie::HeapReport {
                self.__locals_ind_common.heap_report()
            }
            fn dump_trie_stores(&self, dump: &mut $crate::index_engine::dump::RelationDump<'_>) {
                dump.relation("locals", |w| {
                    self.__locals_ind_common.for_each_row(|r| w.row(&r))
                });
                dump.relation("assign_like", |w| {
                    self.__assign_like_ind_common.for_each_row(|r| w.row(&r))
                });
                dump.relation("edge_split", |w| {
                    self.__edge_split_ind_common.for_each_row(|r| w.row(&r))
                });
                dump.relation("ext_dst", |w| {
                    self.__ext_dst_ind_common.for_each_row(|r| w.row(&r))
                });
            }
        }
    };
}

/// The vertex at call `insn` of a root, or of a formal: the actual of a formal, and a global
/// itself (G5, [`FlowVariable::call_root`]).
#[macro_export]
macro_rules! call_arg {
    ($insn:expr, $n:expr) => {
        $crate::facts::FlowVariable::call_root($insn, $crate::facts::Root::from($n))
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

/// Computes `alias_of_formal`: the whole-variable copy-closure of the formals and globals over
/// ORIGINAL program copies. A variable `v` aliases formal `i` (in function `f`) when `v` is
/// reachable FROM `f`'s formal `i`, following whole-variable copies (`copy_edge`) present in the
/// original program in the direction of assignment only. The closure is inclusion-based
/// (Andersen-style), not unification-based (Steensgaard-style): a copy `dst = src` lets `dst`
/// inherit `src`'s formal-aliases and never the reverse, so a formal's alias set only ever grows
/// downstream of the formal itself.
///
/// This depends only on `formal_param` and `copy_edge`, so it is computed in its own small fixpoint
/// BEFORE the main ascent. Doing so keeps `copy_edge` out of the main engine entirely (one fewer
/// relation + index there) and lets the aliasing summary rule consume `alias_of_formal` as a plain
/// pre-populated input.
fn compute_alias_of_formal(
    formal_param: &[(FunctionId, FlowVariable, FormalType)],
    mut copy_edge: Vec<(FunctionId, FlowVariable, FlowVariable)>,
) -> Vec<(FunctionId, FlowVariable, Root)> {
    // Base case: a formal whole-aliases itself. A `Copied` formal shares nothing with the caller,
    // so a store through it or its aliases is no summary, and it gets no aliases at all. A
    // global copied in a function aliases itself there too.
    let mut alias_base: Vec<_> = formal_param
        .iter()
        .filter(|(_, _, ty)| *ty != FormalType::Copied)
        .filter_map(|(infunc, v1, _)| v1.as_formal().map(|i| (*infunc, *v1, Root::formal(i))))
        .collect();
    alias_base.extend(
        copy_edge
            .iter()
            .filter_map(|(f, _, src)| src.as_global().map(|g| (*f, *src, Root::global(g)))),
    );
    alias_base.sort_unstable();
    alias_base.dedup();
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
        relation alias_of_formal(FunctionId, FlowVariable, Root) = alias_base;
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

/// Computes `paths`, the admissible paths: the syntactic program paths, the paths the
/// input summaries mention, and each one-level concatenation of the two. Its own small
/// fixpoint, run BEFORE the main ascent, so the main program takes `paths` as a plain input
/// and can also hold it as a [`PathSet`] for lookups that never build a path.
///
/// With `deref_paths`, `deref` ([`PathSegment::Deref`](ctadl_ir::cir::PathSegment::Deref)) is also a model
/// path, so every program path gains its `.deref` extension.
fn compute_paths(
    program_paths: Vec<(Path,)>,
    mut model_paths: Vec<(Path,)>,
    deref_paths: bool,
) -> Vec<(Path,)> {
    if deref_paths {
        let deref = (Path::from_accesses(std::iter::once(PathSegment::Deref)),);
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
    with_value_paths(pre.paths)
}

/// Closes `paths` under the terminal [`PathSegment::Value`](ctadl_ir::cir::PathSegment::Value): `p·#value` is
/// admissible for every admissible `p` and the empty path, and no path has a segment after
/// `#value`. A program that never mentions `#value` keeps its paths as they are.
///
/// Because every [`Path`] is normal, the closure is under the equations of `ctadl_ir::cir::path`
/// too: `p.[k].#value` is the member `p.#value`, and no member is void.
///
/// With [`PathSet::concat`] absorbing `#value` and its adjacent offsets, that is all the index rules need to
/// treat a value-derived fact as weaker than a copy: a row `t.#value <- a.q` cannot be extended
/// by a suffix, so a read of `t.deref` never turns it into `t.deref <- a.q.deref`, and the suffix
/// rules carry `#value` through copies, stores, summaries and call sites as the last segment of
/// the rest.
fn with_value_paths(paths: Vec<(Path,)>) -> Vec<(Path,)> {
    let is_value = |s: &PathSegment| s.is_value();
    if !paths.iter().any(|(p,)| p.iter().any(is_value)) {
        return paths;
    }
    let mut base: Vec<Path> = vec![Path::empty()];
    for (p,) in paths {
        let segments: Vec<PathSegment> = p.iter().cloned().collect();
        match segments.iter().position(is_value) {
            None => base.push(p),
            Some(i) if i + 1 == segments.len() => {
                base.push(Path::from_accesses(segments[..i].iter().cloned()))
            }
            // `#value·rest`: never admissible.
            Some(_) => {}
        }
    }
    base.sort_unstable();
    base.dedup();
    let value = Path::from_accesses([PathSegment::Value]);
    let mut out: Vec<(Path,)> = Vec::with_capacity(2 * base.len());
    for p in base {
        out.push((p,));
        out.push((p.concat(&value),));
    }
    // `p.[k]·#value` and `p·#value` are one path.
    out.sort_unstable();
    out.dedup();
    out
}

/// The exact splits of a label's path. The empty path need not be a member of the path set,
/// but it still has one split: a label on the whole variable meets an edge out of it.
pub fn label_splits<'a>(ps: &'a PathSet, p: &Path) -> &'a [(Path, Path)] {
    const WHOLE: &[(Path, Path)] = &[(Path::EMPTY, Path::EMPTY)];
    if p.is_empty() {
        WHOLE
    } else {
        ps.splits(p).exact
    }
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
    relation const_str_assign(FunctionId, FlowVariable, Path, Str);
    relation intent_send(FunctionId, InsnId, InsnId, FormalIndex, IntentKind);
    relation intent_filter(Str, Str, IntentKind, FunctionId);
    relation intent_component(Str, IntentKind, FunctionId);

    // Analysis drivers:

    // Set of syntactic paths. Computed before the run by `compute_paths`, and read by no
    // rule: it is the output's copy. `path_set` holds the same set frozen as a [`PathSet`],
    // which every rule that tests or splits a path asks.
    relation paths(Path);
    relation path_set(PathSetRef);
    // A function's flow from root `n2.p2` to root `n1.p1` (a formal, the return or a global).
    relation summary(FunctionId, Root, Path, Root, Path);
    relation config(IndexConfig);
    relation intent_frame(FunctionId);

    // Derived:

    // Local reachability: `locals(f, v, p, a, p4)` is `v.p` reached from root `a.p4`. The core,
    // most expensive relation.
    #[ds($crate::index_engine::locals_trie)]
    relation locals(FunctionId, FlowVariable, Path, Root, Path);
    // An edge `v1.p1 = v2.p2` of `f`, stored SOURCE FIRST: `assign_like(f, v2, p2, v1, p1)`.
    // The rules probe an edge by its source, and the store groups on the first three columns.
    // The engine's input and output rows are destination first; `run_engine!` swaps them.
    #[ds($crate::index_engine::locals_trie)]
    relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path);
    // Real program field-stores (`v.p = ...`, non-empty destination path). Gates the aliasing rule.
    relation prog_store(FunctionId, FlowVariable, Path);
    // A variable that whole-aliases a formal or a global through original program copies,
    // precomputed by `compute_alias_of_formal`. Unlike `locals(v, empty, formal, empty)`, it leaves
    // out inter-procedural / summary-derived copies and the write-backs onto the formal at the
    // return. Feeds the aliasing summary rule.
    relation alias_of_formal(FunctionId, FlowVariable, Root);
    relation const_reaches(FunctionId, FlowVariable, Path, Str);

    // Call edges the fixpoint derives: the Android intent pairings.
    relation resolved_call(FunctionId, InsnId, FunctionId);
    relation intent_pair(FunctionId, InsnId, FunctionId, IntentPairKind);

    // Initialize locals with formals
    locals(infunc, v1, p1.clone(), Root::formal(i), p1.clone()) <--
        formal_param(infunc, v1, _),
        if let Some(i) = v1.as_formal(),
        let p1 = Path::empty();

    // Semantics: G2. A global is a root of local reachability like a formal, in every
    // function that mentions it: in its own text, or in an edge instantiated from a callee's
    // summary, or in the global frame, which mentions every global. The flows its paths reach
    // are summaries (with a formal) or program-level edges (with another global), below.
    relation global_mention(FunctionId, GlobalId);
    global_mention(f, g) <--
        assign_like(f, v2, _, v1, _),
        for g in [v1.as_global(), v2.as_global()].into_iter().flatten();
    global_mention(f, g) <-- const_str_assign(f, v, _, _), if let Some(g) = v.as_global();
    global_mention(GLOBAL_FRAME, g) <-- global_mention(_, g);
    locals(f, v, Path::empty(), Root::global(*g), Path::empty()) <--
        global_mention(f, g),
        let v = FlowVariable::global(*g);

    // Forward field propagation, as exact-key joins.
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
    // with the suffix adjusted by `4 - m`. A `#value` component is wild too, since
    // `[m].#value = #value` for every `m`. `Path::prefix_keys` spells out both, and
    // `Path::wild_rest` matches a wild key.

    // The distinct `(f, v, p)` reached, so the splits below run once per path, not once per row
    // (rows outnumber paths thirty to one on a dense binary).
    relation reach_vp(FunctionId, FlowVariable, Path);
    reach_vp(f, v, p) <-- locals(f, v, p, _, _);
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
    // An edge whose source path ends in an offset, keyed without it, for the wild match.
    relation assign_wild(FunctionId, FlowVariable, Path, i64, FlowVariable, Path);
    assign_wild(f, v2, key, m, v1, p1) <--
        assign_like(f, v2, p2, v1, p1),
        path_set(ps),
        if let Some((key, m)) = ps.split_trailing_offset(p2);
    // The exact and wild keys of an edge's source path.
    // Held in the `locals` store's shape: `(f, v2, key)` probed exactly.
    #[ds($crate::index_engine::locals_trie)]
    relation edge_split(FunctionId, FlowVariable, Path, Path, (FlowVariable, Path));
    edge_split(f, v2, key, rest, (*v1, *p1)) <--
        assign_like(f, v2, p2, v1, p1),
        path_set(ps),
        for (key, rest) in ps.splits(p2).exact;
    relation edge_split_wild(FunctionId, FlowVariable, Path, Path, FlowVariable, Path);
    edge_split_wild(f, v2, key, rest, v1, p1) <--
        assign_like(f, v2, p2, v1, p1),
        path_set(ps),
        for (key, rest) in ps.splits(p2).wild;

    // Semantics (`docs/ir.md`; deviations are rows of the approximations register in
    // `docs/limitations.md`). `ext_dst` is S6, the forward rule: the suffix `rest` of an
    // assignment's source carries to its destination. It deviates by computing suffixes only within
    // the admissible path set (the "admissible paths" row of the approximations register, loses)
    // and by rebasing an offset prefix only against an offset or `#value` (`Path::wild_rest`), not
    // against a field (S6 gives `x.[8]`'s suffix of `x.f` as `[-8].f`). It does not distinguish
    // store from copy, so it carries no scope (S7).
    //
    // Destination side: `v1.p1 = v2.p2` and a `locals` path `p23 = p2·rest` give `v1.(p1·rest)`
    // whatever `v2.p23` has. `ext_dst` is that expanded edge, derived once per (edge, path)
    // rather than once per `locals` row, so the reachability step below is an exact join.
    // Stored source first like `assign_like`: `ext_dst(f, v2, p23, v1, p13)`, probed exactly by
    // `(f, v2, p23)`.
    #[ds($crate::index_engine::locals_trie)]
    relation ext_dst(FunctionId, FlowVariable, Path, FlowVariable, Path);
    ext_dst(f, v2, p23, v1, p13) <--
        locals_key(f, v2, key, rest, p23),
        assign_like(f, v2, key, v1, p1),
        path_set(ps),
        if let Some(p13) = ps.concat(p1, None, rest);
    ext_dst(f, v2, p23, v1, p13) <--
        locals_key_wild(f, v2, key, rest, p23),
        assign_wild(f, v2, key, m, v1, p1),
        if let Some((adj, tail)) = rest.wild_rest(*m),
        path_set(ps),
        if let Some(p13) = ps.concat(p1, Some(adj), &tail);
    locals(f, v1, p13, a, p4) <--
        ext_dst(f, v2, p23, v1, p13),
        locals(f, v2, p23, a, p4);

    // Semantics: `edge_split` and `ext_fml` are S6 applied to a label's suffixes, so a label at
    // `a.p4` on `v2.p2` extends by the `rest` the edge reads beyond `p2`. Same deviations as
    // `ext_dst`.
    //
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
    ext_fml(f, v1, p1, v2, p2, adj, tail) <--
        edge_split_wild(f, v2, key, rest, v1, p1),
        locals_wild(f, v2, key, m, p2),
        if let Some((adj, tail)) = rest.wild_rest(*m);
    locals(f, v1, p1, a, p43) <--
        ext_fml(f, v1, p1, v2, p2, adj, rest),
        locals(f, v2, p2, a, p4),
        path_set(ps),
        if let Some(p43) = ps.concat(p4, Some(*adj), rest);

    // Compute assignments from call sites
    //
    // Semantics: S10's copies `p_i = a_i` and `r = ret`, written as edges in both directions
    // between the actual and the call-arg vertex. The reverse edge stands for S8's backward rule
    // through the copy, filtered by `isout` and the formal's `FormalType`: `SharedFields` is S8's
    // "the empty suffix moves forward only". `Shared` also writes the whole variable back
    // ("shared formals" row, adds), and `Copied` writes nothing back ("copied formals" row,
    // loses).
    assign_like(func_id, cv.clone(), Path::empty(), v.clone(), p),
    assign_like(func_id, v.clone(), p, cv.clone(), Path::empty()) <--
        actual_param(call_site_slice, n, vx),
        let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**call_site_slice).unwrap(),
        let cv = call_arg!(insn_id, *n),
        let FlowVertex(v, p, ..) = vx;

    // Compute assignments from summaries: a summary at every site that calls its function.
    assign_like(caller, v2, p2, v1, p1) <--
        summary(callee, n1, p1, n2, p2),
        call(caller, insn, callee),
        let v1 = call_arg!(*insn, *n1),
        let v2 = call_arg!(*insn, *n2);

    const_reaches(func_id, v.clone(), p.clone(), s) <--
        intent_frame(func_id),
        const_str_assign(func_id, v, p, s);

    const_reaches(func_id, v1.clone(), p_new.clone(), s) <--
        intent_frame(func_id),
        const_reaches(func_id, v2, p_context, s),
        assign_like(func_id, v2, p2, v1, p1),
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

    // Compute summaries from local reachability.
    summary(infunc, n1, p1, n2, p2) <--
        locals(infunc, dst_var, p1, n2, p2),
        // join with formal_param here instead of using if so that we don't have to traverse all of
        // locals
        formal_param(infunc, dst_var, formal_ty),
        if let Some(n1) = dst_var.as_root(),
        if n1.isout(*formal_ty, p1),
        if n1 != *n2 || p1 != p2;

    // Semantics: G6. A flow into a global path is a summary when it comes from a formal. A flow
    // between two global paths holds in every instance (G2), so it is no function's summary: it
    // is an edge of the global frame, where the field rules close it.
    summary(f, Root::global(g), p1, n2, p2) <--
        locals(f, v, p1, n2, p2),
        if *f != GLOBAL_FRAME,
        if let Some(g) = v.as_global(),
        if !n2.is_global();
    assign_like(GLOBAL_FRAME, FlowVariable::global(h), p2, v, p1) <--
        locals(f, v, p1, n2, p2),
        if *f != GLOBAL_FRAME,
        if let Some(g) = v.as_global(),
        if let Some(h) = n2.as_global(),
        if g != h || p1 != p2;

    // aliasing summary rule.
    //
    // Semantics: S8, the backward rule, restricted. A store's labels move backward to the
    // formal `v1` copies only when `v1` is a whole-variable copy-chain alias of a formal
    // (`alias_of_formal`) and `v1.p1` is a program store destination (`prog_store`); S8 moves
    // them along every assignment. `l = p.f; l.g = a` reaches `p.f.g` only when `l` is a
    // temporary the load-chain composition pass anchors on `p`, and `y.f = p; y.f.g = a` never
    // reaches `p.g` (the "backward alias rule" row of the register, loses). A store at `#value`
    // stays put, as S8 says.
    // Clause order matters: `locals` FIRST so Ascent drives the join by the `locals` DELTA and
    // probes `alias_of_formal` via its `0_1` index. Writing `alias_of_formal` first instead
    // makes Ascent full-scan all of `alias_of_formal` every fixpoint iteration (a scan whose
    // per-iteration cost is |alias_of_formal|, catastrophic on binaries with many SSA copies).
    //
    // Semantics, globals. `alias_of_formal` starts from the globals too, so a store
    // through a whole copy of a global, `l = $g; l.f = v`, reaches `$g.f`. Between two globals it
    // is an edge of the frame rather than a summary, as above.
    relation alias_flow(FunctionId, Root, Path, Root, Path);
    summary(f, n1, p1, n2, p2) <--
        alias_flow(f, n1, p1, n2, p2),
        if !(n1.is_global() && n2.is_global());
    assign_like(GLOBAL_FRAME, FlowVariable::global(g2), p2, FlowVariable::global(g1), p1) <--
        alias_flow(_, n1, p1, n2, p2),
        if let Some(g1) = n1.as_global(),
        if let Some(g2) = n2.as_global();
    alias_flow(infunc, n1, p1.clone(), n2, bp) <--
        // v1.p1 <- n2.bp  (delta driver)
        locals(infunc, v1, p1, n2, bp),
        if !p1.is_empty(),
        // `#value` moves forward only (S8): a store at `v1.#value` is not shared with v1's alias.
        if !p1.is_value(),
        // v1.p1 is actually stored through in the program (not just reachable): membership
        // probe by (func,var,path). Restricts summaries to genuine aliased writes.
        prog_store(infunc, v1, p1),
        // this is the alias: v1 <- n1, established by original program copies only
        alias_of_formal(infunc, v1, n1),
        config(c),
        if c.alias_rule,
        if n1 != n2 || *p1 != *bp;

    // Semantics: G6, the label exchange. A string label at a global path holds in every
    // instance (G2), so it guards nothing: it goes up to the global frame from any function
    // that holds it, crosses the frame's edges there, and comes down into a function at the
    // global paths it reads (an edge's source) and their extensions. Labels come down by the
    // paths a function reads, not by the globals it mentions: on a binary every function
    // mentions the one memory global, and by mention every label in memory would be copied
    // into every function.
    relation global_read(FunctionId, GlobalId, Path);
    global_read(f, g, p) <--
        assign_like(f, v, p, _, _),
        if *f != GLOBAL_FRAME,
        if let Some(g) = v.as_global();

    intent_frame(GLOBAL_FRAME) <-- intent_frame(f), if *f != GLOBAL_FRAME;
    const_reaches(GLOBAL_FRAME, v, p, s) <--
        const_reaches(f, v, p, s),
        if *f != GLOBAL_FRAME,
        if v.is_global();

    // The frame's labelled paths, keyed by every prefix a read can name.
    relation global_label_key(GlobalId, Path, Path);
    global_label_key(g, key, p) <--
        const_reaches(&GLOBAL_FRAME, v, p, _),
        if let Some(g) = v.as_global(),
        path_set(ps),
        for (key, _) in label_splits(ps, p);
    relation global_label_read(FunctionId, GlobalId, Path);
    global_label_read(f, g, p) <--
        global_label_key(g, key, p),
        global_read(f, g, key);
    const_reaches(f, v, p, s) <--
        global_label_read(f, g, p),
        intent_frame(f),
        let v = FlowVariable::global(*g),
        const_reaches(&GLOBAL_FRAME, v, p, s);
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
    pub alias_of_formal: Vec<(FunctionId, FlowVariable, Root)>,
    pub paths: Vec<(Path,)>,
    pub path_set: PathSetRef,
    pub index_timeout: std::time::Duration,
    pub id_map: Option<&'a IdMap>,
    pub initial_assign: usize,
    pub initial_summary: usize,
    pub initial_formals: usize,
    pub num_functions: usize,
    pub num_variables: usize,
}

impl<'a> EngineInput<'a> {
    pub fn new(mut facts: IndexFacts, config: IndexConfig, id_map: Option<&'a IdMap>) -> Self {
        let parallelism = config.parallelism;
        let storage = config.storage;
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
        let mut assign_like: Vec<_> = std::mem::take(&mut facts.assign)
            .into_iter()
            .map(|(site, dst, src)| {
                let InsnSiteId { func_id, .. } = InsnSiteId::unpack_from_slice(&*site).unwrap();
                (func_id, dst.0, dst.1, src.0, src.1)
            })
            .collect();
        // A `--summary` project's program-level flows are edges of the global frame.
        assign_like.extend(std::mem::take(&mut facts.global_flow).into_iter().map(
            |(dst, p1, src, p2)| {
                (
                    crate::facts::GLOBAL_FRAME,
                    FlowVariable::global(dst),
                    p1,
                    FlowVariable::global(src),
                    p2,
                )
            },
        ));
        log::debug!(
            "[mem cp] + assign_like ({} rows, facts.assign consumed): {:.1} MB",
            assign_like.len(),
            phys_footprint_mb()
        );
        // Model paths = paths introduced by summaries ONLY. Do NOT fold `facts.paths` in here:
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
        ACTION_PATH, COMPONENT_PATH, IndexConfig, TYPE_PATH, label_splits, mime_matches,
    };
    pub use crate::call_arg;
    pub use crate::facts::{
        CallDispatchKey, CallTargetObject, FlowVariable, FlowVertex, FormalIndex, FormalType,
        FunctionId, GLOBAL_FRAME, GlobalId, InsnId, InsnSiteId, IntentKind, IntentPairKind,
        PackedInsnSiteId, Path, Root, Str, isout,
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
                    bytes_per_row: Vec::new(),
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
/// field's type differs by engine (`Vec` vs `boxcar::Vec`, `LocalsIndCommon` vs
/// `CLocalsIndCommon`), so
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
        use $crate::index_engine::TrieStores as _;
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
            initial_summary,
            initial_formals,
            num_functions,
            num_variables,
        } = $input;
        let mut prog = <$prog_ty>::default();
        prog.formal_param = facts.formal_param.into_iter().collect();
        prog.actual_param = facts.actual_param.into_iter().collect();
        prog.call = call.into_iter().collect();
        prog.summary = $crate::index_engine::distinct_rows(facts.summary).collect();
        prog.const_str_assign = facts
            .const_str_assign
            .into_iter()
            .map(|(site_id, vx, value)| {
                let $crate::facts::InsnSiteId { func_id, .. } = $crate::index_engine::unpack_site(&site_id);
                (func_id, vx.0, vx.1, value)
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
        let trie_sizes = prog.trie_sizes();
        log::debug!(
            "[relsizes] byods-backed: assign_like {:?}, locals {:?}, \
             edge_split {:?}, ext_dst {:?}; locals_key size: {}; bytes per row: {:?}",
            trie_sizes.assign_like,
            trie_sizes.locals,
            trie_sizes.edge_split,
            trie_sizes.ext_dst,
            sizes.locals_key,
            sizes.bytes_per_row
        );
        let locals_heap = prog.locals_heap_report();
        let mut profile = $crate::index_engine::profile::IndexProfile {
            reached_fixpoint,
            timeout_secs: (index_timeout != std::time::Duration::MAX).then(|| index_timeout.as_secs()),
            fixpoint_secs,
            bytes_per_row: sizes
                .bytes_per_row
                .iter()
                .map(|&(name, bytes)| (name.to_string(), bytes))
                .collect(),
            ..$crate::index_engine::profile::IndexProfile::from_summaries(
                &prog.relation_sizes_summary(),
                &prog.scc_times_summary(),
                &[
                    ("assign_like", trie_sizes.assign_like),
                    ("locals", trie_sizes.locals),
                    ("edge_split", trie_sizes.edge_split),
                    ("locals_key", sizes.locals_key),
                    ("ext_dst", trie_sizes.ext_dst),
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
            trie_sizes.edge_split,
            prog.edge_split_wild.len(),
            trie_sizes.ext_dst,
            prog.ext_fml.len()
        );
        log::debug!("{locals_heap}");
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
            prog.dump_trie_stores(&mut dump);
            dump.finish(&prog.relation_sizes_summary());
        }

        let assign_like_out = prog.take_assign_like();
        let summary_out: Vec<_> = prog.summary.into_iter().collect();
        let global_flow_out = $crate::index_engine::global_flow_rows(&assign_like_out);

        let stats = $crate::index_engine::IndexStats {
            profile,
            initial_assign,
            final_assign_like: assign_like_out.len(),
            initial_formals,
            final_locals: trie_sizes.locals,
            initial_summary,
            final_summary: summary_out.len(),
            num_functions,
            num_variables,
            reached_variables: trie_sizes.reached_variables,
            final_resolved_call: prog.resolved_call.len(),
        };
        stats.log();

        $crate::index_engine::IndexResult {
            summary: summary_out,
            assign_like: assign_like_out,
            resolved_call: prog.resolved_call.into_iter().collect(),
            const_reaches: prog.const_reaches.into_iter().collect(),
            intent_pair: prog.intent_pair.into_iter().collect(),
            paths: prog.paths.into_iter().collect(),
            external_function: facts.external_function,
            global_flow: global_flow_out,
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

    /// `#value` is terminal: once a program mentions it, every admissible path (and the empty
    /// path) gains a `#value` extension, and no admissible path has a segment after it, not even
    /// one the model-path concatenation builds.
    #[test]
    fn compute_paths_closes_under_terminal_value() {
        let p = |s: &str| (Path::parse(s).unwrap(),);
        let value = (Path::from_accesses([PathSegment::Value]),);
        let program = vec![(Path::empty(),), p(".f"), value];
        let paths = compute_paths(program, vec![], true);
        for want in [".f", ".deref", ".f.deref"] {
            let (q,) = p(want);
            let qv = (q.concat(&value.0),);
            assert!(paths.contains(&qv), "{want}.#value missing: {paths:?}");
        }
        assert!(paths.contains(&value));
        for (q,) in &paths {
            let segs: Vec<_> = q.iter().collect();
            if let Some(i) = segs.iter().position(|s| s.is_value()) {
                assert_eq!(i + 1, segs.len(), "a segment after #value: {q}");
            }
        }
        // A program that never mentions `#value` keeps its paths.
        let plain = vec![(Path::empty(),), p(".f")];
        assert_eq!(compute_paths(plain.clone(), vec![], false), plain);
    }
}
