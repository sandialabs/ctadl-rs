/*!
The plan of a staged run: which functions each stage-1 batch runs, in what order, and what each
is estimated to cost.

The call graph's nodes are the functions with facts and its edges are `call`, caller to callee.
Its SCCs are taken callees first, and consecutive whole SCCs are packed into batches whose
estimates fit the budget, so every batch's callees outside it are in earlier batches. A
recursive SCC over the budget, or with more functions than the floor, is split into parts,
each a batch of its own; the parts of one SCC run in rounds until their summaries stop growing.

The plan depends only on the facts and the configuration, never on the order of the facts:
functions are ordered by [`FunctionId`] wherever order matters.
*/

use std::fs;

use ctadl_ir::graph::{DirectedGraph, Successors, scc::Sccs};
use hashbrown::{HashMap, HashSet};
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorContext};
use crate::facts::{FlowVariable, FunctionId, GLOBAL_FRAME};
use crate::index_engine::{IndexFacts, unpack_site};
use crate::staged::store::{StageDir, write_atomic};

// The estimate's coefficients. A function's estimate is
//
//     SEED_ROW_BYTES × (assign rows + instantiated callee-summary rows)
//       + FORMAL_BYTES × formals + VARIABLE_BYTES × variables,
//
// and a batch's is the sum of its functions' plus one worker's fixed cost. They were fitted to
// whole-program serial runs, the only measurements there are before staging. Fit them again to
// measured batch peaks.

/// Bytes per seed row of `assign_like`: an `assign` fact, or a callee's summary row instantiated
/// at a call site. The fixpoint's whole growth divided by the `assign` rows it started from was
/// 1976 B and 1998 B on two Android apps of 4.9M and 1.3M `assign` rows. Fitting formals as well
/// gave them no share of it.
pub const SEED_ROW_BYTES: u64 = 2048;
/// Bytes per formal. Not fitted: the seed rows explain the measured growth.
pub const FORMAL_BYTES: u64 = 256;
/// Bytes per variable. Not fitted, as for formals.
pub const VARIABLE_BYTES: u64 = 64;
/// The summary rows a function is expected to have per formal, in thousandths, beyond those its
/// facts seed. Measured 290, 410 and 890 on the two apps and a synthetic program.
pub const SUMMARY_MILLIROWS_PER_FORMAL: u64 = 400;
/// Bytes a worker holds per member of the program's path set: the frozen path set's share
/// (measured 91 and 101 B) and the interned path's (66 and 76 B).
pub const PATH_BYTES: u64 = 176;
/// A worker's footprint before it reads anything. Not fitted: a whole index run of a small
/// synthetic program peaked at 80 MB, which bounds it.
pub const WORKER_BASE_BYTES: u64 = 32 << 20;

/// What every worker holds whatever its batch: [`WORKER_BASE_BYTES`] and a path set of `paths`
/// members.
pub fn worker_bytes(paths: usize) -> u64 {
    WORKER_BASE_BYTES.saturating_add(PATH_BYTES.saturating_mul(paths as u64))
}

/// The call graph of the functions with facts.
#[derive(Debug)]
pub struct CallGraph {
    /// Every function with a per-function fact, sorted. A node is an index here.
    pub functions: Vec<FunctionId>,
    /// Node `n`'s callees are `callees[callees_at[n]..callees_at[n + 1]]`, sorted and distinct.
    callees_at: Vec<u32>,
    callees: Vec<u32>,
    /// Each node's estimate, in bytes.
    pub estimates: Vec<u64>,
}

impl CallGraph {
    /// The node of `f`, if it has facts.
    pub fn node(&self, f: FunctionId) -> Option<u32> {
        self.functions.binary_search(&f).ok().map(|n| n as u32)
    }

    /// The callees of node `n`, in [`FunctionId`] order.
    pub fn callees(&self, n: u32) -> &[u32] {
        let n = n as usize;
        &self.callees[self.callees_at[n] as usize..self.callees_at[n + 1] as usize]
    }
}

impl DirectedGraph for CallGraph {
    type Node = u32;

    fn num_nodes(&self) -> usize {
        self.functions.len()
    }
}

impl Successors for CallGraph {
    fn successors(&self, node: u32) -> impl Iterator<Item = u32> {
        self.callees(node).iter().copied()
    }
}

/// Per-function counts the estimate reads.
#[derive(Clone, Copy, Default)]
struct Counts {
    assign: u64,
    formals: u64,
    variables: u64,
    seeded_summary: u64,
}

/// The call graph of `facts`, with an estimate per function.
///
/// The nodes are the functions in `formal_param`, `actual_param`, `call` (as callers),
/// `assign`, `const_str_assign`, `summary` and `external_function`, less the global frame. The
/// edges are `call`, between two such functions.
pub fn call_graph(facts: &IndexFacts) -> CallGraph {
    let mut counts: HashMap<FunctionId, Counts> = HashMap::new();
    for (f, _, _) in &facts.formal_param {
        counts.entry(*f).or_default().formals += 1;
    }
    for (site, _, _) in &facts.actual_param {
        counts.entry(unpack_site(site).func_id).or_default();
    }
    for (site, _) in &facts.call {
        counts.entry(unpack_site(site).func_id).or_default();
    }
    for (site, _, _) in &facts.assign {
        counts.entry(unpack_site(site).func_id).or_default().assign += 1;
    }
    for (site, _, _) in &facts.const_str_assign {
        counts.entry(unpack_site(site).func_id).or_default();
    }
    for (f, ..) in &facts.summary {
        counts.entry(*f).or_default().seeded_summary += 1;
    }
    for (f,) in &facts.external_function {
        counts.entry(*f).or_default();
    }
    counts.remove(&GLOBAL_FRAME);
    let mut functions: Vec<FunctionId> = counts.keys().copied().collect();
    functions.sort_unstable();
    let node = |f: FunctionId| functions.binary_search(&f).ok().map(|n| n as u32);

    // Distinct variables per function, as the engine counts them: formals and both sides of
    // every assignment.
    let mut variables: Vec<(u32, FlowVariable)> = facts
        .formal_param
        .iter()
        .filter_map(|(f, v, _)| Some((node(*f)?, *v)))
        .chain(facts.assign.iter().flat_map(|(site, dst, src)| {
            let n = node(unpack_site(site).func_id);
            [(n, dst.0), (n, src.0)]
                .into_iter()
                .filter_map(|(n, v)| Some((n?, v)))
        }))
        .collect();
    variables.sort_unstable();
    variables.dedup();
    let mut counts: Vec<Counts> = functions.iter().map(|f| counts[f]).collect();
    for (n, _) in variables {
        counts[n as usize].variables += 1;
    }

    // A callee's summary rows are estimated from its own facts alone, so a callee in its
    // caller's SCC, whose summary depends on the caller's, needs no iteration.
    let summary_rows =
        |c: &Counts| c.seeded_summary + (c.formals * SUMMARY_MILLIROWS_PER_FORMAL).div_ceil(1000);
    let mut instantiated = vec![0u64; functions.len()];
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(facts.call.len());
    for (site, target) in &facts.call {
        let (Some(caller), Some(callee)) = (node(unpack_site(site).func_id), node(*target)) else {
            continue;
        };
        instantiated[caller as usize] += summary_rows(&counts[callee as usize]);
        edges.push((caller, callee));
    }
    edges.sort_unstable();
    edges.dedup();
    let mut callees_at = Vec::with_capacity(functions.len() + 1);
    let mut at = 0;
    for n in 0..functions.len() as u32 {
        callees_at.push(at as u32);
        while at < edges.len() && edges[at].0 == n {
            at += 1;
        }
    }
    callees_at.push(edges.len() as u32);
    let callees = edges.into_iter().map(|(_, callee)| callee).collect();

    let estimates = counts
        .iter()
        .zip(&instantiated)
        .map(|(c, inst)| {
            SEED_ROW_BYTES
                .saturating_mul(c.assign + inst)
                .saturating_add(FORMAL_BYTES.saturating_mul(c.formals))
                .saturating_add(VARIABLE_BYTES.saturating_mul(c.variables))
        })
        .collect();
    CallGraph {
        functions,
        callees_at,
        callees,
        estimates,
    }
}

pub type BatchId = u32;

/// A batch's place in a split SCC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// The split SCC, an index into [`Plan::split_sccs`].
    pub scc: u32,
    /// The part's place in the SCC's order.
    pub index: u32,
}

/// One stage-1 batch: whole SCCs, or one part of a split SCC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: BatchId,
    /// Sorted.
    pub functions: Vec<FunctionId>,
    /// The callees outside the batch, sorted, each with the batch that holds it.
    pub callees: Vec<(FunctionId, BatchId)>,
    /// The batches that must be done before this one starts: those that hold its callees, less
    /// the other parts of its own split SCC. Sorted, and all earlier than this one.
    pub deps: Vec<BatchId>,
    /// Bytes, including one worker's fixed cost.
    pub estimate: u64,
    /// Set when the batch is a part of a split SCC.
    pub part: Option<Part>,
}

impl Batch {
    /// Where `stage` holds the descriptor of batch `id`.
    pub fn descriptor_file(stage: &StageDir, id: BatchId) -> std::path::PathBuf {
        stage.batch_dir(id).join("batch.json")
    }

    /// Reads the descriptor of batch `id`, which [`Plan::save`] wrote.
    pub fn load(stage: &StageDir, id: BatchId) -> Result<Self, Error> {
        read_json(&Self::descriptor_file(stage, id))
    }
}

/// A recursive SCC split into parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitScc {
    /// Its parts, in the order they run in a round: callees first, as far as the cycles allow.
    pub parts: Vec<BatchId>,
}

/// The batches of a staged run, in an order that runs every batch after its callees'.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The memory budget, in bytes.
    pub budget: u64,
    /// The most functions a part of a split SCC holds. A recursive SCC with more is split.
    pub floor: usize,
    /// What every worker holds whatever its batch, counted once in each batch's estimate.
    pub worker_bytes: u64,
    pub batches: Vec<Batch>,
    pub split_sccs: Vec<SplitScc>,
    /// The batches that hold an `intent_frame` function, sorted.
    pub intent_batches: Vec<BatchId>,
}

impl Plan {
    /// Plans `facts` under `budget` bytes, splitting recursive SCCs down to parts of `floor`
    /// functions (at least one), for a program whose path set has `paths` members.
    ///
    /// A batch's estimate fits the budget, except a batch of one function that does not fit
    /// alone.
    pub fn new(facts: &IndexFacts, budget: u64, floor: usize, paths: usize) -> Self {
        let graph = call_graph(facts);
        let mut plan = Planner::new(&graph, budget, floor.max(1), worker_bytes(paths)).run();
        let batch_of = plan.batch_of(&graph);
        let mut intent: Vec<BatchId> = facts
            .intent_frame
            .iter()
            .filter_map(|(f,)| Some(batch_of[graph.node(*f)? as usize]))
            .collect();
        intent.sort_unstable();
        intent.dedup();
        plan.intent_batches = intent;
        plan
    }

    /// Node to batch.
    fn batch_of(&self, graph: &CallGraph) -> Vec<BatchId> {
        let mut batch_of = vec![0; graph.functions.len()];
        for batch in &self.batches {
            for f in &batch.functions {
                batch_of[graph.node(*f).unwrap() as usize] = batch.id;
            }
        }
        batch_of
    }

    /// Writes the plan to `stage`'s plan file, and each batch's descriptor to its batch
    /// directory, so that a worker reads only its own.
    pub fn save(&self, stage: &StageDir) -> Result<(), Error> {
        for batch in &self.batches {
            write_json(&Batch::descriptor_file(stage, batch.id), batch)?;
        }
        write_json(&stage.plan_file(), self)
    }

    /// Reads the plan [`Plan::save`] wrote.
    pub fn load(stage: &StageDir) -> Result<Self, Error> {
        read_json(&stage.plan_file())
    }

    /// The plan as JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a plan serializes")
    }
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<(), Error> {
    write_atomic(path, |tmp| {
        let json = serde_json::to_vec(value).expect("a plan serializes");
        fs::write(tmp, json)
            .map_err(Error::from)
            .err_context(|| format!("writing {}", path.display()))
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Result<T, Error> {
    let json = fs::read(path)
        .map_err(Error::from)
        .err_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&json).map_err(|e| Error::Context {
        context: format!("reading {}", path.display()),
        source: Box::new(e),
    })
}

/// Packs the SCCs of a call graph into batches.
struct Planner<'a> {
    graph: &'a CallGraph,
    budget: u64,
    floor: usize,
    worker: u64,
    /// The nodes of each batch so far, in [`FunctionId`] order, and its part, if any.
    batches: Vec<(Vec<u32>, Option<Part>)>,
    split_sccs: Vec<SplitScc>,
}

impl<'a> Planner<'a> {
    fn new(graph: &'a CallGraph, budget: u64, floor: usize, worker: u64) -> Self {
        Self {
            graph,
            budget,
            floor,
            worker,
            batches: Vec::new(),
            split_sccs: Vec::new(),
        }
    }

    fn estimate(&self, nodes: &[u32]) -> u64 {
        nodes.iter().fold(self.worker, |sum, n| {
            sum.saturating_add(self.graph.estimates[*n as usize])
        })
    }

    fn fits(&self, sum: u64) -> bool {
        sum <= self.budget
    }

    fn run(mut self) -> Plan {
        let graph = self.graph;
        let sccs: Sccs<u32, u32> = Sccs::new(graph);
        // The members of each SCC, in node order, which is FunctionId order.
        let mut members_at = vec![0u32; sccs.num_sccs() + 1];
        for n in 0..graph.functions.len() as u32 {
            members_at[sccs.scc(n) as usize + 1] += 1;
        }
        for i in 1..members_at.len() {
            members_at[i] += members_at[i - 1];
        }
        let mut members = vec![0u32; graph.functions.len()];
        let mut next = members_at.clone();
        for n in 0..graph.functions.len() as u32 {
            let s = sccs.scc(n) as usize;
            members[next[s] as usize] = n;
            next[s] += 1;
        }

        let mut open: Vec<u32> = Vec::new();
        let mut open_sum = self.worker;
        // Callees first.
        for scc in sccs.all_sccs() {
            let s = scc as usize;
            let scc_nodes = &members[members_at[s] as usize..members_at[s + 1] as usize];
            let sum = self.estimate(scc_nodes) - self.worker;
            if scc_nodes.len() > 1
                && (scc_nodes.len() > self.floor || !self.fits(self.worker.saturating_add(sum)))
            {
                self.close(&mut open, &mut open_sum);
                self.split(&sccs, scc, scc_nodes);
                continue;
            }
            if !open.is_empty() && !self.fits(open_sum.saturating_add(sum)) {
                self.close(&mut open, &mut open_sum);
            }
            open.extend_from_slice(scc_nodes);
            open_sum = open_sum.saturating_add(sum);
        }
        self.close(&mut open, &mut open_sum);
        self.finish()
    }

    fn close(&mut self, open: &mut Vec<u32>, open_sum: &mut u64) {
        if !open.is_empty() {
            let mut nodes = std::mem::take(open);
            nodes.sort_unstable();
            self.batches.push((nodes, None));
        }
        *open_sum = self.worker;
    }

    /// Splits a recursive SCC into contiguous runs of a callees-first order of it: the
    /// postorder of a depth-first search over its internal edges, from its lowest function,
    /// callees in [`FunctionId`] order. A run of such an order cuts fewer of the SCC's edges
    /// than an arbitrary part would, and every edge between parts costs rounds.
    fn split(&mut self, sccs: &Sccs<u32, u32>, scc: u32, nodes: &[u32]) {
        let graph = self.graph;
        let order = {
            let mut visited: HashSet<u32> = HashSet::with_capacity(nodes.len());
            let mut order = Vec::with_capacity(nodes.len());
            let mut stack: Vec<(u32, usize)> = Vec::new();
            for &root in nodes {
                if !visited.insert(root) {
                    continue;
                }
                stack.push((root, 0));
                while let Some((n, i)) = stack.last_mut() {
                    let callees = graph.callees(*n);
                    match callees.get(*i) {
                        Some(&c) => {
                            *i += 1;
                            if sccs.scc(c) == scc && visited.insert(c) {
                                stack.push((c, 0));
                            }
                        }
                        None => {
                            order.push(*n);
                            stack.pop();
                        }
                    }
                }
            }
            order
        };
        let id = self.split_sccs.len() as u32;
        let mut parts = Vec::new();
        let mut part: Vec<u32> = Vec::new();
        let mut sum = self.worker;
        for n in order {
            let est = graph.estimates[n as usize];
            if !part.is_empty() && (part.len() >= self.floor || !self.fits(sum.saturating_add(est)))
            {
                parts.push(std::mem::take(&mut part));
                sum = self.worker;
            }
            part.push(n);
            sum = sum.saturating_add(est);
        }
        parts.push(part);
        let first = self.batches.len() as BatchId;
        for (index, mut nodes) in parts.into_iter().enumerate() {
            nodes.sort_unstable();
            let part = Part {
                scc: id,
                index: index as u32,
            };
            self.batches.push((nodes, Some(part)));
        }
        self.split_sccs.push(SplitScc {
            parts: (first..self.batches.len() as BatchId).collect(),
        });
    }

    /// Records each batch's callees outside it and the batches it depends on.
    fn finish(self) -> Plan {
        let graph = self.graph;
        let mut batch_of = vec![0 as BatchId; graph.functions.len()];
        for (id, (nodes, _)) in self.batches.iter().enumerate() {
            for n in nodes {
                batch_of[*n as usize] = id as BatchId;
            }
        }
        let batches = self
            .batches
            .iter()
            .enumerate()
            .map(|(id, (nodes, part))| {
                let id = id as BatchId;
                let mut callees: Vec<(FunctionId, BatchId)> = nodes
                    .iter()
                    .flat_map(|n| graph.callees(*n))
                    .filter(|c| batch_of[**c as usize] != id)
                    .map(|c| (graph.functions[*c as usize], batch_of[*c as usize]))
                    .collect();
                callees.sort_unstable();
                callees.dedup();
                let same_scc = |b: BatchId| match (part, &self.batches[b as usize].1) {
                    (Some(p), Some(q)) => p.scc == q.scc,
                    _ => false,
                };
                let mut deps: Vec<BatchId> = callees
                    .iter()
                    .map(|(_, b)| *b)
                    .filter(|b| !same_scc(*b))
                    .collect();
                deps.sort_unstable();
                deps.dedup();
                debug_assert!(deps.iter().all(|d| *d < id), "callees come first");
                Batch {
                    id,
                    functions: nodes.iter().map(|n| graph.functions[*n as usize]).collect(),
                    callees,
                    deps,
                    estimate: self.estimate(nodes),
                    part: *part,
                }
            })
            .collect();
        Plan {
            budget: self.budget,
            floor: self.floor,
            worker_bytes: self.worker,
            batches,
            split_sccs: self.split_sccs,
            intent_batches: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FlowVertex, InsnId, InsnSiteId, PackedInsnSiteId, Path, Str};
    use crate::staged::shuffle::shuffle_facts;

    const MB: u64 = 1 << 20;

    fn site(f: u32, insn: u64) -> PackedInsnSiteId {
        InsnSiteId {
            func_id: FunctionId::new(f),
            insn_id: InsnId::new(insn),
        }
        .try_into()
        .unwrap()
    }

    fn local(name: &str) -> FlowVertex {
        FlowVertex::new(FlowVariable::local(Str::from(name)), Path::empty())
    }

    /// Facts for functions `0..n` with `assign_rows` assignments each and the given calls,
    /// and no formals, so that no callee summary is expected and every function has the same
    /// estimate.
    fn facts(n: u32, assign_rows: u64, calls: &[(u32, u32)]) -> IndexFacts {
        let mut facts = IndexFacts::default();
        for f in 0..n {
            for i in 0..assign_rows {
                facts.assign.push((site(f, i), local("x"), local("y")));
            }
        }
        for (i, (caller, callee)) in calls.iter().enumerate() {
            facts
                .call
                .push((site(*caller, 1000 + i as u64), FunctionId::new(*callee)));
        }
        facts
    }

    /// The functions of each batch, by id.
    fn functions(plan: &Plan) -> Vec<Vec<u32>> {
        plan.batches
            .iter()
            .map(|b| b.functions.iter().map(|f| f.id).collect())
            .collect()
    }

    /// Checks what holds of every plan: batches number from 0 in order, every function is in
    /// exactly one batch, and every callee is outside its batch and in the batch named.
    fn check(plan: &Plan) {
        let mut seen = std::collections::BTreeMap::new();
        for (i, batch) in plan.batches.iter().enumerate() {
            assert_eq!(batch.id as usize, i);
            assert!(batch.functions.is_sorted_by(|a, b| a < b));
            for f in &batch.functions {
                assert!(
                    seen.insert(*f, batch.id).is_none(),
                    "{f:?} is in two batches"
                );
            }
        }
        for batch in &plan.batches {
            for (f, b) in &batch.callees {
                assert_ne!(*b, batch.id);
                assert_eq!(seen[f], *b);
            }
            assert!(batch.deps.iter().all(|d| *d < batch.id));
        }
    }

    /// The budget that fits `k` functions of `facts`, all of one estimate, in a batch.
    fn budget_for(facts: &IndexFacts, k: u64) -> u64 {
        let graph = call_graph(facts);
        let est = graph.estimates[0];
        assert!(graph.estimates.iter().all(|e| *e == est));
        worker_bytes(0) + k * est
    }

    #[test]
    fn chain_is_callees_first() {
        let (a, b, c) = (0, 1, 2);
        let facts = facts(3, 10, &[(a, b), (b, c)]);
        let plan = Plan::new(&facts, budget_for(&facts, 1), usize::MAX, 0);
        check(&plan);
        assert_eq!(functions(&plan), vec![vec![c], vec![b], vec![a]]);
        assert_eq!(plan.batches[1].deps, vec![0]);
        assert_eq!(plan.batches[2].deps, vec![1]);
        assert_eq!(plan.batches[2].callees, vec![(FunctionId::new(b), 1)]);
    }

    #[test]
    fn diamond_packs_under_budget() {
        let (a, b, c, d) = (0, 1, 2, 3);
        let facts = facts(4, 10, &[(a, b), (a, c), (b, d), (c, d)]);
        let budget = budget_for(&facts, 2);
        let plan = Plan::new(&facts, budget, usize::MAX, 0);
        check(&plan);
        assert_eq!(plan.batches.len(), 2);
        assert!(plan.batches.iter().all(|b| b.estimate <= budget));
        assert!(functions(&plan)[0].contains(&d));
        assert!(functions(&plan)[1].contains(&a));
        assert_eq!(plan.batches[1].deps, vec![0]);
    }

    #[test]
    fn cycle_splits_at_floor_one() {
        let (a, b, c) = (0, 1, 2);
        let facts = facts(3, 10, &[(a, b), (b, c), (c, a)]);
        let plan = Plan::new(&facts, u64::MAX, 1, 0);
        check(&plan);
        // The postorder of a search from `a`: `c` returns first, then `b`, then `a`.
        assert_eq!(functions(&plan), vec![vec![c], vec![b], vec![a]]);
        assert_eq!(
            plan.split_sccs,
            vec![SplitScc {
                parts: vec![0, 1, 2]
            }]
        );
        for (i, batch) in plan.batches.iter().enumerate() {
            assert_eq!(
                batch.part,
                Some(Part {
                    scc: 0,
                    index: i as u32
                })
            );
            assert!(batch.deps.is_empty(), "parts of one SCC are not deps");
        }
        assert_eq!(plan.batches[0].callees, vec![(FunctionId::new(a), 2)]);

        // Over the floor, the SCC stays whole.
        let plan = Plan::new(&facts, u64::MAX, 3, 0);
        assert_eq!(functions(&plan), vec![vec![a, b, c]]);
        assert!(plan.split_sccs.is_empty());
        // Floor 2 cuts the same order into runs of two.
        let plan = Plan::new(&facts, u64::MAX, 2, 0);
        assert_eq!(functions(&plan), vec![vec![b, c], vec![a]]);
        // A budget for one function splits it whatever the floor.
        let plan = Plan::new(&facts, budget_for(&facts, 1), usize::MAX, 0);
        assert_eq!(functions(&plan), vec![vec![c], vec![b], vec![a]]);
        assert_eq!(plan.split_sccs.len(), 1);
    }

    #[test]
    fn function_without_formals_is_planned() {
        let mut facts = facts(1, 3, &[]);
        facts.external_function.push((FunctionId::new(7),));
        facts.formal_param.push((
            FunctionId::new(9),
            FlowVariable::formal_index(0i16.into()),
            Default::default(),
        ));
        // The global frame is never planned.
        let root = crate::facts::Root::formal(0i16.into());
        let path = Path::empty();
        facts.summary.push((GLOBAL_FRAME, root, path, root, path));
        let plan = Plan::new(&facts, u64::MAX, usize::MAX, 0);
        check(&plan);
        assert_eq!(functions(&plan), vec![vec![0, 7, 9]]);
    }

    /// A program with chains, a diamond, cycles of several sizes, formals, seeded summaries,
    /// intent functions and duplicate calls.
    fn varied_facts() -> IndexFacts {
        let mut calls = Vec::new();
        for f in 0..60u32 {
            calls.push((f, (f * 7 + 3) % 60));
            if f % 3 == 0 {
                calls.push((f, (f + 1) % 60));
                calls.push((f, (f + 1) % 60));
            }
            if f % 5 == 0 {
                calls.push((f, f / 2));
            }
        }
        let mut facts = facts(60, 0, &calls);
        for f in 0..60u32 {
            for i in 0..(f % 7) as u64 {
                let v = format!("v{}", (i + f as u64) % 4);
                facts.assign.push((site(f, i), local(&v), local("w")));
            }
            for i in 0..(f % 4) as i16 {
                let var = FlowVariable::formal_index(i.into());
                facts
                    .formal_param
                    .push((FunctionId::new(f), var, Default::default()));
            }
            if f % 11 == 0 {
                facts.intent_frame.push((FunctionId::new(f),));
            }
        }
        facts
    }

    #[test]
    fn plan_ignores_fact_order() {
        let facts = varied_facts();
        let budget = worker_bytes(1000) + 40 * 1024;
        let reference = Plan::new(&facts, budget, 2, 1000);
        check(&reference);
        assert!(reference.batches.len() > 3);
        assert!(!reference.split_sccs.is_empty());
        assert!(!reference.intent_batches.is_empty());
        let reference = reference.to_json();
        for seed in 1..=10 {
            let mut shuffled = facts.clone();
            shuffle_facts(&mut shuffled, seed);
            assert_ne!(shuffled.call, facts.call);
            let plan = Plan::new(&shuffled, budget, 2, 1000);
            assert_eq!(plan.to_json(), reference, "seed {seed}");
        }
    }

    #[test]
    fn oversize_function_is_alone() {
        let mut facts = facts(5, 10, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let budget = budget_for(&facts, 3);
        for i in 0..100 {
            facts
                .assign
                .push((site(2, 100 + i), local("x"), local("y")));
        }
        let plan = Plan::new(&facts, budget, usize::MAX, 0);
        check(&plan);
        assert_eq!(functions(&plan), vec![vec![3, 4], vec![2], vec![0, 1]]);
        assert!(plan.batches[1].estimate > budget);
        assert!(plan.batches[0].estimate <= budget);
        assert!(plan.batches[2].estimate <= budget);
    }

    /// The path set is a cost per batch, so it fits fewer functions in each.
    #[test]
    fn path_set_counts_once_per_batch() {
        let facts = facts(4, 10, &[]);
        let est = call_graph(&facts).estimates[0];
        // A path set that costs two functions' worth.
        let paths = (2 * est).div_ceil(PATH_BYTES) as usize;
        let budget = worker_bytes(paths) + 2 * est;
        assert_eq!(Plan::new(&facts, budget, usize::MAX, 0).batches.len(), 1);
        let plan = Plan::new(&facts, budget, usize::MAX, paths);
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(plan.worker_bytes, worker_bytes(paths));
        assert!(plan.worker_bytes > 32 * MB);
    }

    /// The plan and each batch's descriptor read back from the stage directory as written.
    #[test]
    fn plan_round_trips_through_stage_dir() {
        let facts = varied_facts();
        let plan = Plan::new(&facts, worker_bytes(0) + 40 * 1024, 2, 0);
        let index = tempfile::tempdir().unwrap();
        let stage = StageDir::new(index.path());
        stage.create().unwrap();
        plan.save(&stage).unwrap();
        assert_eq!(Plan::load(&stage).unwrap(), plan);
        for batch in &plan.batches {
            assert_eq!(&Batch::load(&stage, batch.id).unwrap(), batch);
        }
    }
}
