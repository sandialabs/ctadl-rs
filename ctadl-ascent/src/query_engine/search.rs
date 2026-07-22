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

use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasherDefault;

use rustc_hash::FxHasher;

use ctadl_ir::graph::{LazyAnnotation, LazySuccessors, find_annotated_paths_from_set};

/// `hashbrown` maps/sets keyed by the deterministic, fast `FxHasher` rather than
/// the DoS-resistant SipHash the std collections default to — the taint tables
/// hold trusted, program-derived keys, so the faster hash is a free win.
type HashMap<K, V> = hashbrown::HashMap<K, V, BuildHasherDefault<FxHasher>>;
type HashSet<T> = hashbrown::HashSet<T, BuildHasherDefault<FxHasher>>;

use crate::facts::{
    CallArgId, FlowEdge, FlowVariable, FormalType, FunctionId, IdMap, InsnSiteId, Label,
    PackedCallArg, PackedInsnSiteId, Path, TaintDirection, TaintLevel, TaintState, isout,
};

use super::{QueryEndpoint, QueryFacts, QueryResult, compute_copy_alias};

/// A node of the implicit taint graph: a variable and access path within a
/// function, plus the [`TaintLevel`] magnitude carried in-band. The level must
/// live in the node (not the annotation) because successor generation
/// ([`LazySuccessors::labeled_successors`]) sees only the node, and a saturating
/// vertex generates extra edges a plain one does not. The [`TaintState`]
/// call/return discipline stays the annotation — the two are orthogonal.
pub type TaintNode = (FunctionId, FlowVariable, Path, TaintLevel);

/// The level-agnostic vertex identity `(function, variable, access path)`, used
/// for sink matching and emission — where the taint magnitude is invisible (the
/// lattice "collapses" to a plain reached state).
type TaintVertex = (FunctionId, FlowVariable, Path);

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
    /// The same loads `x = a.q` indexed by *base*: `(f, a) -> [(x, q)]`. The
    /// mirror of `loads_by_dst`, consulted only by the saturating rule: reading
    /// any offset `q` off a saturating base `a` taints the load's destination
    /// `x`, regardless of whether `q` matches the tainted path.
    loads_by_src: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path)>>,
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
    /// Sink access paths per `(function, variable)`, from the backward
    /// endpoints. Consulted only by the saturating rule: a saturating vertex
    /// `(v, p)` reaches any sink `(v, q)` whose path `q` extends `p` (reading
    /// further off the saturating value is tainted). This is the sink-side of
    /// saturation — the read the sink performs (`system`'s `Argument(0).deref`)
    /// is external code, so it is not a `load` edge and must be matched here
    /// rather than through `loads_by_src`. Bounded by the number of sinks.
    sink_ext_by_var: HashMap<(FunctionId, FlowVariable), Vec<Path>>,
}

impl TaintSearchGraph {
    pub fn new(facts: &QueryFacts) -> Self {
        let mut assign_by_src: HashMap<
            (FunctionId, FlowVariable),
            Vec<(FlowVariable, Path, Path)>,
        > = HashMap::default();
        let mut loads_by_dst: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path)>> =
            HashMap::default();
        let mut loads_by_src: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path)>> =
            HashMap::default();
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
                loads_by_src
                    .entry((*f, *src))
                    .or_default()
                    .push((*dst, *sp));
            }
        }

        let formal_ty = facts
            .formal_param
            .iter()
            .map(|(f, v, ty)| ((*f, *v), *ty))
            .collect();

        let mut callers_by_callee: HashMap<FunctionId, Vec<PackedInsnSiteId>> = HashMap::default();
        let mut callee_by_site = HashMap::default();
        for (site, callee) in &facts.call {
            callers_by_callee.entry(*callee).or_default().push(*site);
            callee_by_site.insert(*site, *callee);
        }

        let paths = facts.paths.iter().map(|(p,)| *p).collect();

        let mut copy_rep = HashMap::default();
        let mut copy_members: HashMap<(FunctionId, FlowVariable), Vec<FlowVariable>> =
            HashMap::default();
        for (f, member, rep) in compute_copy_alias(&facts.assign) {
            copy_rep.insert((f, member), rep);
            copy_members.entry((f, rep)).or_default().push(member);
        }
        for members in copy_members.values_mut() {
            members.sort_unstable();
        }

        // Sink access paths per variable, for the saturating rule's sink-side
        // read. Only backward endpoints contribute.
        let mut sink_ext_by_var: HashMap<(FunctionId, FlowVariable), Vec<Path>> =
            HashMap::default();
        for (ep,) in &facts.endpoints {
            if ep.direction == TaintDirection::Backward {
                sink_ext_by_var
                    .entry((ep.infunc, ep.vertex.0))
                    .or_default()
                    .push(ep.vertex.1);
            }
        }

        TaintSearchGraph {
            assign_by_src,
            loads_by_dst,
            loads_by_src,
            formal_ty,
            callers_by_callee,
            callee_by_site,
            paths,
            copy_rep,
            copy_members,
            sink_ext_by_var,
        }
    }
}

impl LazySuccessors for TaintSearchGraph {
    type Node = TaintNode;
    type Label = FlowEdge;

    fn labeled_successors(&self, node: &TaintNode) -> Vec<(TaintNode, FlowEdge)> {
        let (f, v, p, level) = *node;
        let mut out = Vec::new();

        // The taint level rides along every existing edge unchanged, so
        // saturating-ness is preserved through precise propagation (e.g. across
        // the bare-base copy `formal(1) -> @p1_0`, making `@p1_0` saturating).

        // Direct assign-like flow with path substitution: taint on `src.p` with
        // `p = sp·rest` flows to `dst.(dp·rest)`, for materialized paths only.
        if let Some(edges) = self.assign_by_src.get(&(f, v)) {
            for (dst, dp, sp) in edges {
                if let Some(p2) = p.substitute_prefix(sp, dp)
                    && self.paths.contains(&p2)
                {
                    out.push(((f, *dst, p2, level), FlowEdge::Intra));
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
                    out.push(((f, *a, *q, level), FlowEdge::Intra));
                } else {
                    let qp = q.concat(&p);
                    if self.paths.contains(&qp) {
                        out.push(((f, *a, qp, level), FlowEdge::Intra));
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
                out.push(((f, *rep, p, level), FlowEdge::Intra));
            }
            if let Some(members) = self.copy_members.get(&(f, v)) {
                for m in members {
                    out.push(((f, *m, p, level), FlowEdge::Intra));
                }
            }
        }

        // Saturating vertex-level read: any subfield/offset read off a
        // saturating vertex is tainted, regardless of the tainted path `p` and
        // without the materialized-paths gate. For every load `x = v.q` off
        // this vertex, taint the destination `x` (at the empty path) and keep it
        // `Saturating`, so saturation fills the whole access-path subtree
        // (recursively). This is what reconnects `@p1_0.[8].deref` to the
        // saturating `@p1_0`. Plain nodes get no such edges, so precise sources
        // (e.g. `getenv`) keep their path-sensitive behavior and cannot
        // over-taint.
        if level == TaintLevel::Saturating {
            if let Some(loads) = self.loads_by_src.get(&(f, v)) {
                for (dst, _q) in loads {
                    out.push((
                        (f, *dst, Path::empty(), TaintLevel::Saturating),
                        FlowEdge::Intra,
                    ));
                }
            }

            // Saturating sink-side read: the sink reads a subfield/offset `q`
            // off this saturating value (`system`'s `Argument(0).deref` reads
            // `.deref` off the tainted pointer). That read is external code, not
            // a `load` edge, so step the same vertex `v` from the tainted path
            // `p` to the (strictly longer) sink path `q` that extends it. The
            // resulting `(v, q)` vertex exact-matches the sink, is emitted as a
            // sink-vertex row, and the `(v, p) -> (v, q)` edge joins the flow
            // graph the formatter walks. Bounded by the sinks on `v`.
            if let Some(qs) = self.sink_ext_by_var.get(&(f, v)) {
                for q in qs {
                    if q.len() > p.len() && q.is_extension_of(&p) {
                        out.push(((f, v, *q, TaintLevel::Saturating), FlowEdge::Intra));
                    }
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
                    (func_id, FlowVariable::call_arg_packed(call_arg), p, level),
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
                    out.push(((*callee, formal_var, p, level), FlowEdge::Call(site)));
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
    // Sinks key on the level-agnostic vertex: the taint magnitude is invisible to
    // sink matching (a saturating flow arriving at a sink is just a flow arriving).
    let mut sink_nodes: HashMap<TaintVertex, Vec<QueryEndpoint>> = HashMap::default();
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
        // endpoint that names it. A saturating source seeds its start node at
        // level `Saturating`; a plain source at `Plain`.
        let mut start_origin: HashMap<TaintNode, u32> = HashMap::default();
        let mut starts: Vec<TaintNode> = Vec::new();
        for (i, ep) in endpoints.iter().enumerate() {
            let level = if ep.saturating {
                TaintLevel::Saturating
            } else {
                TaintLevel::Plain
            };
            let node = (ep.infunc, ep.vertex.0, ep.vertex.1, level);
            if let hashbrown::hash_map::Entry::Vacant(e) = start_origin.entry(node) {
                e.insert(i as u32);
                starts.push(node);
            }
        }

        let search = find_annotated_paths_from_set(&graph, starts, |n, _s: &TaintState| {
            sink_nodes.contains_key(&(n.0, n.1, n.2))
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
            if st.node.1.as_call_arg().is_none()
                && !sink_nodes.contains_key(&(st.node.0, st.node.1, st.node.2))
            {
                continue;
            }
            // Emission drops the level (the "collapse"): every reached state is a
            // plain tainted row, exactly as before.
            let ep = &endpoints[origin[i] as usize];
            taint.push((st.node.0, st.annot, st.node.1, st.node.2, ep.clone()));
        }

        // One (breadth-first shortest) path per sink vertex reached: the
        // targets come back in discovery order, so the first state per node
        // wins. Report the path: its edges become the `taint_edge` graph the
        // formatter walks, and every node on it is tagged with the sink's
        // endpoint — the backward-direction tag that marks the flow's source
        // end as completing a source -> sink flow.
        // Dedup by the level-agnostic vertex: one path per sink vertex, even if
        // the vertex is reached as both `Saturating` and `Plain`.
        let mut reported: HashSet<TaintVertex> = HashSet::default();
        let mut paths_found = 0usize;
        for &t in &search.targets {
            let node = search.states[t as usize].node;
            let vertex = (node.0, node.1, node.2);
            if !reported.insert(vertex) {
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
                for sink in &sink_nodes[&vertex] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FlowVertex, Label};

    /// A single-field access path `.field`.
    fn field_path(field: &str) -> Path {
        Path::from_accesses([ctadl_ir::mir::PathSegment::Symbol(
            internment::ArcIntern::from(field),
        )])
    }

    /// A forward (source) endpoint on `(func, var)` at the empty path.
    fn source(func: u32, var: FlowVariable, saturating: bool) -> QueryEndpoint {
        QueryEndpoint {
            infunc: FunctionId::new(func),
            vertex: FlowVertex(var, Path::empty()),
            label: Label("argv_input".into()),
            direction: TaintDirection::Forward,
            call_site: None,
            saturating,
        }
    }

    /// A backward (sink) endpoint on `(func, var)` at the empty path.
    fn sink(func: u32, var: FlowVariable) -> QueryEndpoint {
        QueryEndpoint {
            infunc: FunctionId::new(func),
            vertex: FlowVertex(var, Path::empty()),
            label: Label("cmdi".into()),
            direction: TaintDirection::Backward,
            call_site: None,
            saturating: false,
        }
    }

    /// A saturating source reconnects an offset/field read off its base to the
    /// read's destination (the `argv[1]` regression, in miniature): the base
    /// `p` is the source, and the sink `q` is read via a *sibling* load `q =
    /// p.field` whose path is never matched by precise, path-sensitive
    /// propagation. Only saturation reconnects it, so a `Saturating` seed
    /// reaches the sink where a `Plain` seed does not.
    #[test]
    fn saturating_source_reaches_offset_read() {
        let f = 0u32;
        let base = FlowVariable::local("base".into());
        let dst = FlowVariable::local("dst".into());
        let q = field_path("field");

        // A load `dst = base.field` (destination path empty, source path `.field`).
        let assign = vec![(FunctionId::new(f), dst, Path::empty(), base, q)];
        // `.field` is materialized; the empty path is implicitly present.
        let paths = vec![(q,), (Path::empty(),)];
        let sink_ep = sink(f, dst);

        let run = |saturating: bool| {
            let facts = QueryFacts {
                assign: assign.clone(),
                paths: paths.clone(),
                endpoints: vec![(source(f, base, saturating),), (sink_ep.clone(),)],
                ..Default::default()
            };
            taint_search(facts, None)
        };

        // Saturating: the base saturates, so reading `.field` off it taints
        // `dst` — a source -> sink path exists.
        let sat = run(true);
        assert!(
            !sat.taint_edge.is_empty(),
            "saturating source should reconnect the offset read to the sink"
        );

        // Plain: path-sensitive propagation never matches the sibling path, so
        // the sink is unreachable.
        let plain = run(false);
        assert!(
            plain.taint_edge.is_empty(),
            "plain source must not reach the offset read (no over-tainting)"
        );
    }

    /// The sink-side of saturation: a saturating pointer flows to a sink that
    /// reads a subfield/offset off it (`system(argv[1])` reads `.deref` off the
    /// tainted pointer). The taint arrives at the sink vertex at the *bare*
    /// path, and only saturation extends it to the sink's `.field` path. A
    /// `Plain` seed, arriving at the same bare vertex, does not.
    #[test]
    fn saturating_source_reaches_extended_sink() {
        let f = 0u32;
        let base = FlowVariable::local("base".into());
        let deref = field_path("field");

        // No loads: the source vertex *is* the sink's variable, but the sink
        // reads `.field` off it while the taint sits at the bare (empty) path.
        let assign: Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)> = vec![];
        let paths = vec![(deref,), (Path::empty(),)];

        let run = |saturating: bool| {
            let src = source(f, base, saturating);
            // Sink on the same vertex, at the extended path `.field`.
            let mut sink_ep = sink(f, base);
            sink_ep.vertex = FlowVertex(base, deref);
            let facts = QueryFacts {
                assign: assign.clone(),
                paths: paths.clone(),
                endpoints: vec![(src,), (sink_ep,)],
                ..Default::default()
            };
            taint_search(facts, None)
        };

        let sat = run(true);
        assert!(
            !sat.taint_edge.is_empty(),
            "saturating source should reach a sink that reads a subfield off it"
        );

        let plain = run(false);
        assert!(
            plain.taint_edge.is_empty(),
            "plain source at the bare path must not reach the `.field` sink"
        );
    }
}
