/*! Demand-driven taint search over the program tables.

This is the query engine's search regime: instead of materializing the taint
closure with a datalog fixpoint (whose intermediate relations — all-pairs
aliases, the 7-wide `taint_edge` — blow up on firmware-sized copy groups), it
runs one annotated graph search per source *label* directly over the program
tables. Nothing is materialized beyond the states the search actually reaches.

The regime:

1. Aliasing is precomputed efficiently: union-find over the empty-path copy
   edges gives each copy-connected variable group a representative
   ([`super::compute_copy_alias`]), and field loads are indexed by destination
   for demand-driven field-alias back-flow. No alias closure is materialized.
2. Sources are partitioned by label: all source endpoints sharing a label form
   one start *set* and participate in the same search, sharing a visited set.
3. Each set runs [`find_annotated_paths_from_set`] — the multi-start variant of
   the formatter's realizable-path search — over the implicit graph whose edges
   are expanded on demand from `assign_like`, `formal_param`, and `call`
   ([`TaintSearchGraph`]). Expansion reaches the aliases of a node (its copy
   class, routed through the union-find representative; the bases of the loads
   that defined it) as well as its direct assignment successors. The search
   threads a [`TaintState`] annotation along the edges so call/return matching
   is respected: a `Call` edge enters `Restricted`, and a `Return` edge is only
   traversable while `Free`.
4. Sink endpoints are the search targets. Every sink reached gets a
   (breadth-first shortest) path from the source set, and found paths are
   reported through the existing means: the returned [`QueryResult`] carries
   the reached states as `taint` rows and the search forest as `taint_edge`
   rows, which the SARIF formatter walks exactly as before.
*/

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ctadl_ir::graph::{LazyAnnotation, LazySuccessors, find_annotated_paths_from_set};

use crate::facts::{
    CallArgId, FlowEdge, FlowVariable, FormalType, FunctionId, IdMap, InsnSiteId, Label,
    PackedCallArg, PackedInsnSiteId, Path, TaintDirection, TaintState, isout,
};

use super::{QueryEndpoint, QueryFacts, QueryResult, compute_copy_alias};

/// A node of the implicit taint graph: a variable and access path within a
/// function. The taint state is not part of the node — it is the annotation the
/// search threads along the edges.
pub type TaintNode = (FunctionId, FlowVariable, Path);

/// The implicit taint dataflow graph: edges are computed on demand from indexed
/// program tables, never materialized. Edge expansion mirrors the closure
/// engine's forward propagation rules — direct assigns with path substitution,
/// field-alias back-flow from loads, copy-class equalization through the
/// union-find representative, and formal/actual call-boundary steps — so a
/// search over this graph visits exactly the vertices forward taint
/// propagation would have tainted.
pub struct TaintSearchGraph {
    /// `assign_like` edges indexed by source variable:
    /// `(f, src) -> [(dst, dst_path, src_path)]`.
    assign_by_src: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path, Path)>>,
    /// Field/offset loads `x = a.q` (destination path empty, `q` non-empty)
    /// indexed by destination: `(f, x) -> [(a, q)]`. A load makes `x` an
    /// *alias* of `a.q`, so taint on `x` also lives at `a.q` — the back-flow
    /// direction the direct `assign_by_src` edges don't cover.
    loads_by_dst: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path)>>,
    /// `formal_param`, keyed by `(function, formal variable)`.
    formal_ty: HashMap<(FunctionId, FlowVariable), FormalType>,
    /// Call sites indexed by callee, for formal-to-actual (function exit) steps.
    callers_by_callee: HashMap<FunctionId, Vec<PackedInsnSiteId>>,
    /// Callee of each call site, for actual-to-formal (call entry) steps.
    callee_by_site: HashMap<PackedInsnSiteId, FunctionId>,
    /// The materialized access paths; a step producing a non-materialized path
    /// is dropped, the same gate the closure engine's `paths(p)` premises apply.
    paths: HashSet<Path>,
    /// Copy-class member -> representative (`member != rep`), from union-find
    /// over the empty-path copy edges.
    copy_rep: HashMap<(FunctionId, FlowVariable), FlowVariable>,
    /// Copy-class representative -> members (excluding the rep itself), sorted
    /// so successor order — and therefore search tie-breaking — is
    /// deterministic.
    copy_members: HashMap<(FunctionId, FlowVariable), Vec<FlowVariable>>,
}

impl TaintSearchGraph {
    pub fn new(facts: &QueryFacts) -> Self {
        let mut assign_by_src: HashMap<
            (FunctionId, FlowVariable),
            Vec<(FlowVariable, Path, Path)>,
        > = HashMap::new();
        let mut loads_by_dst: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path)>> =
            HashMap::new();
        for (f, dst, dp, src, sp) in &facts.assign {
            assign_by_src
                .entry((*f, *src))
                .or_default()
                .push((*dst, *dp, *sp));
            if dp.is_empty() && !sp.is_empty() {
                loads_by_dst
                    .entry((*f, *dst))
                    .or_default()
                    .push((*src, *sp));
            }
        }

        let formal_ty = facts
            .formal_param
            .iter()
            .map(|(f, v, ty)| ((*f, *v), *ty))
            .collect();

        let mut callers_by_callee: HashMap<FunctionId, Vec<PackedInsnSiteId>> = HashMap::new();
        let mut callee_by_site = HashMap::new();
        for (site, callee) in &facts.call {
            callers_by_callee.entry(*callee).or_default().push(*site);
            callee_by_site.insert(*site, *callee);
        }

        let paths = facts.paths.iter().map(|(p,)| *p).collect();

        let mut copy_rep = HashMap::new();
        let mut copy_members: HashMap<(FunctionId, FlowVariable), Vec<FlowVariable>> =
            HashMap::new();
        for (f, member, rep) in compute_copy_alias(&facts.assign) {
            copy_rep.insert((f, member), rep);
            copy_members.entry((f, rep)).or_default().push(member);
        }
        for members in copy_members.values_mut() {
            members.sort_unstable();
        }

        TaintSearchGraph {
            assign_by_src,
            loads_by_dst,
            formal_ty,
            callers_by_callee,
            callee_by_site,
            paths,
            copy_rep,
            copy_members,
        }
    }
}

impl LazySuccessors for TaintSearchGraph {
    type Node = TaintNode;
    type Label = FlowEdge;

    fn labeled_successors(&self, node: &TaintNode) -> Vec<(TaintNode, FlowEdge)> {
        let (f, v, p) = *node;
        let mut out = Vec::new();

        // Direct assign-like flow with path substitution: taint on `src.p` with
        // `p = sp·rest` flows to `dst.(dp·rest)`, for materialized paths only.
        if let Some(edges) = self.assign_by_src.get(&(f, v)) {
            for (dst, dp, sp) in edges {
                if let Some(p2) = p.substitute_prefix(sp, dp)
                    && self.paths.contains(&p2)
                {
                    out.push(((f, *dst, p2), FlowEdge::Intra));
                }
            }
        }

        // Field-alias back-flow: a load `x = a.q` makes `x` an alias of `a.q`,
        // so taint on `x.p` also lives at `a.(q·p)`. The empty-`p` step is
        // ungated (its path `q` is already materialized, coming off an assign);
        // an extended path is subject to the materialized-paths gate. The
        // closure engine additionally gated its alias seeding on the base being
        // tainted — a flooding mitigation the demand-driven expansion doesn't
        // need, since only reached states are ever expanded.
        if let Some(loads) = self.loads_by_dst.get(&(f, v)) {
            for (a, q) in loads {
                if p.is_empty() {
                    out.push(((f, *a, *q), FlowEdge::Intra));
                } else {
                    let qp = q.concat(&p);
                    if self.paths.contains(&qp) {
                        out.push(((f, *a, qp), FlowEdge::Intra));
                    }
                }
            }
        }

        // Copy-class equalization: an empty-path copy group shares taint at
        // every path. Routed through the union-find representative so a group
        // of C variables costs O(C) edges (member -> rep, rep -> members)
        // rather than the Θ(C²) all-pairs closure: any two members connect in
        // two hops.
        if p.is_empty() || self.paths.contains(&p) {
            if let Some(rep) = self.copy_rep.get(&(f, v)) {
                out.push(((f, *rep, p), FlowEdge::Intra));
            }
            if let Some(members) = self.copy_members.get(&(f, v)) {
                for m in members {
                    out.push(((f, *m, p), FlowEdge::Intra));
                }
            }
        }

        // Formal-to-actual (function exit): taint on an out-flowing formal
        // continues at the corresponding call-arg vertex of every caller. The
        // `Return` label makes the edge traversable only in the `Free` state,
        // enforcing call/return matching exactly as the closure engine's
        // `TaintState::Free` premise did.
        if let Some(fty) = self.formal_ty.get(&(f, v))
            && let Some(formal) = v.as_formal()
            && isout(&formal, *fty, &p)
            && let Some(sites) = self.callers_by_callee.get(&f)
        {
            for site in sites {
                let InsnSiteId { func_id, insn_id } = InsnSiteId::try_from(site).unwrap();
                let call_arg = PackedCallArg::try_from_parts(insn_id, formal).unwrap();
                out.push((
                    (func_id, FlowVariable::call_arg_packed(call_arg), p),
                    FlowEdge::Return(*site),
                ));
            }
        }

        // Actual-to-formal (call entry): taint on a call-arg vertex enters the
        // callee's formal. The `Call` label puts the annotation in `Restricted`.
        if let Some(packed) = v.as_call_arg() {
            let call_arg_id = CallArgId::try_from(packed).unwrap();
            let site = PackedInsnSiteId::try_from_parts(f, call_arg_id.insn_id).unwrap();
            if let Some(callee) = self.callee_by_site.get(&site) {
                let formal_var = FlowVariable::formal_index(call_arg_id.formal());
                if self.formal_ty.contains_key(&(*callee, formal_var)) {
                    out.push(((*callee, formal_var, p), FlowEdge::Call(site)));
                }
            }
        }

        out
    }
}

/// The same one-bit call/return discipline the formatter's realizable-path
/// search uses (see the [`Annotation`](ctadl_ir::graph::Annotation) impl for
/// [`TaintState`] in the formatter), applied during taint discovery itself: an
/// `Intra` step preserves the state, a `Call` step enters `Restricted`, and a
/// `Return` step is only traversable while `Free`.
impl LazyAnnotation<TaintSearchGraph> for TaintState {
    fn start() -> Self {
        TaintState::Free
    }

    fn expand(
        &self,
        _graph: &TaintSearchGraph,
        _from: &TaintNode,
        label: &FlowEdge,
        _to: &TaintNode,
    ) -> Option<Self> {
        match label {
            FlowEdge::Intra => Some(*self),
            FlowEdge::Call(_) => Some(TaintState::Restricted),
            FlowEdge::Return(_) => match self {
                TaintState::Free => Some(TaintState::Free),
                TaintState::Restricted => None,
            },
        }
    }
}

/// Runs the demand-driven taint search and packages the outcome as a
/// [`QueryResult`], so all downstream reporting (SARIF profiles, flowy checks)
/// works unchanged.
///
/// Only what downstream consumers actually read is materialized — the search
/// visits its states and drops them, rather than persisting the whole closure
/// the way the fixpoint engine did:
///
/// - `taint` holds a seed row per endpoint; a row per reached *call-arg* state
///   (tagged with the source endpoint the state descends from) — these drive
///   the `tainted_insn`/`absorbing_functions` projections and the formatter's
///   source/sink pairing, whose detail nodes are always call-arg vertices; a
///   row per reached *sink-vertex* state (a flow arrived at the sink); and, for
///   every node on a found source -> sink path, a row tagged with the sink
///   endpoint — the backward-direction tag that marks the flow's source end.
/// - `taint_edge` holds the found source -> sink paths (one breadth-first
///   shortest path per sink vertex per label search). Interior search-forest
///   edges are not emitted: a directed source -> sink walk over forest edges
///   can only descend a sink's unique root-path, so path-union edges carry
///   exactly the same source/sink connectivity the full forest would.
/// - `tainted_insn` / `absorbing_functions` are the same projections of `taint`
///   the closure engine derived (call-arg rows are all retained, so these are
///   identical to a full-closure emission).
pub fn taint_search(facts: QueryFacts, id_map: Option<&IdMap>) -> QueryResult {
    let graph = TaintSearchGraph::new(&facts);

    // Partition the sources by label: endpoints sharing a label participate in
    // the same search. Sinks (backward endpoints) are the targets of every
    // search, keyed by the exact vertex they name.
    let mut source_sets: BTreeMap<Label, Vec<QueryEndpoint>> = BTreeMap::new();
    let mut sink_nodes: HashMap<TaintNode, Vec<QueryEndpoint>> = HashMap::new();
    for (ep,) in &facts.endpoints {
        match ep.direction {
            TaintDirection::Forward => {
                source_sets
                    .entry(ep.label.clone())
                    .or_default()
                    .push(ep.clone());
            }
            TaintDirection::Backward => {
                sink_nodes
                    .entry((ep.infunc, ep.vertex.0, ep.vertex.1))
                    .or_default()
                    .push(ep.clone());
            }
        }
    }

    let mut taint: Vec<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)> = Vec::new();
    // Seed rows for every endpoint (both directions), as the closure engine's
    // seed rule emitted.
    for (ep,) in &facts.endpoints {
        taint.push((
            ep.infunc,
            TaintState::Free,
            ep.vertex.0,
            ep.vertex.1,
            ep.clone(),
        ));
    }

    // The edges of the found source -> sink paths, deduped (paths share
    // prefixes) and ordered for deterministic downstream node interning.
    let mut taint_edge: BTreeSet<(
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )> = BTreeSet::new();
    // Sink-endpoint tags for on-path nodes; a set because paths to different
    // sinks share prefixes.
    let mut sink_tags: BTreeSet<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)> =
        BTreeSet::new();

    let mut states_total = 0usize;
    for (label, endpoints) in &source_sets {
        // The start set: each distinct vertex once, attributed to the first
        // endpoint that names it.
        let mut start_origin: HashMap<TaintNode, u32> = HashMap::new();
        let mut starts: Vec<TaintNode> = Vec::new();
        for (i, ep) in endpoints.iter().enumerate() {
            let node = (ep.infunc, ep.vertex.0, ep.vertex.1);
            if !start_origin.contains_key(&node) {
                start_origin.insert(node, i as u32);
                starts.push(node);
            }
        }

        let search = find_annotated_paths_from_set(&graph, starts, |n, _s: &TaintState| {
            sink_nodes.contains_key(n)
        });
        states_total += search.states.len();

        // Which source endpoint each state descends from: parents precede their
        // children in discovery order, so one forward pass propagates it.
        let mut origin: Vec<u32> = Vec::with_capacity(search.states.len());
        for st in &search.states {
            origin.push(match st.parent {
                None => start_origin[&st.node],
                Some(parent) => origin[parent as usize],
            });
        }

        for (i, st) in search.states.iter().enumerate() {
            if st.parent.is_none() {
                // Start states are already covered by the endpoint seed rows.
                continue;
            }
            // Emit only the states downstream consumers read: call-arg vertices
            // (instruction-level projections and the formatter's pairing nodes)
            // and sink vertices (a flow arrived). Interior states are search
            // bookkeeping, not results.
            if st.node.1.as_call_arg().is_none() && !sink_nodes.contains_key(&st.node) {
                continue;
            }
            let ep = &endpoints[origin[i] as usize];
            taint.push((st.node.0, st.annot, st.node.1, st.node.2, ep.clone()));
        }

        // One (breadth-first shortest) path per sink vertex reached: the
        // targets come back in discovery order, so the first state per node
        // wins. Report the path: its edges become the `taint_edge` graph the
        // formatter walks, and every node on it is tagged with the sink's
        // endpoint — the backward-direction tag that marks the flow's source
        // end as completing a source -> sink flow.
        let mut reported: HashSet<TaintNode> = HashSet::new();
        let mut paths_found = 0usize;
        for &t in &search.targets {
            let node = search.states[t as usize].node;
            if !reported.insert(node) {
                continue;
            }
            paths_found += 1;
            let path = search.path_to(t);
            for w in path.windows(2) {
                let from = &search.states[w[0] as usize].node;
                let st = &search.states[w[1] as usize];
                taint_edge.insert((
                    st.edge.unwrap(),
                    from.0,
                    from.1,
                    from.2,
                    st.node.0,
                    st.node.1,
                    st.node.2,
                ));
            }
            for i in path {
                let st = &search.states[i as usize];
                for sink in &sink_nodes[&node] {
                    sink_tags.insert((st.node.0, st.annot, st.node.1, st.node.2, sink.clone()));
                }
            }
        }
        log::debug!(
            "taint search: label '{label}' with {} source endpoint(s): {} states, {} sink vertices reached",
            endpoints.len(),
            search.states.len(),
            paths_found,
        );
    }
    taint.extend(sink_tags);

    // Instruction-level projections of the taint rows, mirroring the closure
    // engine's `tainted_var_at_insn` and `absorbing_functions` rules.
    let external: HashSet<FunctionId> = facts.external_function.iter().map(|(f,)| *f).collect();
    let mut tainted_insn: BTreeSet<(PackedInsnSiteId, Label, FlowVariable, Path)> = BTreeSet::new();
    let mut absorbing: BTreeSet<(FunctionId, QueryEndpoint, crate::facts::FormalIndex)> =
        BTreeSet::new();
    for (f, _ts, v, p, src) in &taint {
        let Some(packed) = v.as_call_arg() else {
            continue;
        };
        let call_arg_id = CallArgId::try_from(packed).unwrap();
        let site = PackedInsnSiteId::try_from_parts(*f, call_arg_id.insn_id).unwrap();
        if !v.is_globals() && *call_arg_id.formal() >= 0 {
            tainted_insn.insert((site, src.label.clone(), *v, *p));
        }
        if let Some(target) = graph.callee_by_site.get(&site)
            && external.contains(target)
        {
            absorbing.insert((*target, src.clone(), call_arg_id.formal()));
        }
    }

    if std::env::var("CTADL_QUERY_SIZES").is_ok() {
        eprintln!(
            "QUERY_SIZES taint={} taint_edge={} states={} searches={} tainted_var_at_insn={} assign_like={} paths={} sources={}",
            taint.len(),
            taint_edge.len(),
            states_total,
            source_sets.len(),
            tainted_insn.len(),
            facts.assign.len(),
            facts.paths.len(),
            facts.endpoints.len(),
        );
    }

    log::trace!(
        "query result: {}",
        super::DisplayTaint {
            taint: &taint,
            id_map,
        }
    );

    QueryResult {
        taint,
        taint_edge: taint_edge.into_iter().collect(),
        tainted_insn: tainted_insn.into_iter().collect(),
        absorbing_functions: absorbing.into_iter().collect(),
    }
}
