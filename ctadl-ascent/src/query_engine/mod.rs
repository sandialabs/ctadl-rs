/*! Taint analysis on the index graph

Taint analysis is a matter of following direct intraprocedural flow and stitching edges
between calls and returns.

We want to enable some queries:

- Path queries. Finds a path from each source to each sink.
- Closure queries. Finds all the vertices/instructions tainted by each source or sink.

The default regime ([`search`]) is a demand-driven graph search: sources are partitioned
by label, and each label set runs one multi-start realizable-path search directly over
the program tables (aliasing consulted through a union-find of the copy classes), with
sinks as targets. Only the states a search reaches exist at all, and only what the
reporting consumes is materialized. The datalog closure engi¬ne
([`taint_analysis_datalog`]) computes the same taint as a fixpoint and materializes it
in full; it remains available via `CTADL_QUERY_DATALOG=1`.
*/

use ascent::ascent;
use derive_builder::Builder;
use packed_struct::prelude::*;

use crate::facts::{
    CallArgId, FlowEdge, FlowVariable, FlowVariableKind, FlowVertex, FormalIndex, FormalType,
    FunctionId, IdMap, InsnSiteId, Label, PackedCallArg, PackedInsnSiteId, Path, TaintDirection,
    TaintEndpoint, TaintState, isout,
};

// same as a TaintEndpoint but with a functionId
#[derive(
    Clone,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct QueryEndpoint {
    /// The function the vertex lives in. For a call-site-anchored endpoint this is the
    /// *caller* (the function containing `call_site`); for a function-anchored endpoint it
    /// is the function the source/sink was declared on.
    pub infunc: FunctionId,
    pub vertex: FlowVertex,
    pub label: Label,
    pub direction: TaintDirection,
    /// The call site this endpoint is anchored at, if any. When `Some`, `vertex` is a
    /// call-arg vertex in `infunc` and `call_site`'s function is `infunc`. `None` denotes a
    /// function-anchored endpoint (no usable call site: a local/global port, or a function
    /// with no callers). It is human-facing metadata; the taint machinery seeds and searches
    /// from `infunc`/`vertex` alone.
    pub call_site: Option<PackedInsnSiteId>,
}

impl QueryEndpoint {
    pub fn display<'a>(&'a self, id_map: Option<&'a IdMap>) -> QueryEndpointDisplay<'a> {
        QueryEndpointDisplay {
            endpoint: self,
            id_map,
        }
    }

    pub fn to_taint_endpoint(self, sites: &IdMap) -> TaintEndpoint {
        TaintEndpoint {
            infunc: sites.get_function(self.infunc).unwrap().clone(),
            vertex: self.vertex,
            label: self.label,
            direction: self.direction,
        }
    }

    pub fn from_taint_endpoint(sites: &IdMap, endpoint: TaintEndpoint) -> Self {
        QueryEndpoint {
            infunc: sites.get_function_id(endpoint.infunc).unwrap(),
            vertex: endpoint.vertex,
            label: endpoint.label,
            direction: endpoint.direction,
            call_site: None,
        }
    }

    /// Fans a function-anchored endpoint into one endpoint per call site of `infunc`,
    /// re-anchoring each at the call-arg vertex (for `formal`) that the call passes. `call`
    /// is the static call graph (`site -> callee`). The endpoint is returned unchanged when
    /// it can't be re-anchored: `formal` is the globals pseudo-formal, or `infunc` has no
    /// callers. `formal` is supplied by the caller because the endpoint's own vertex may have
    /// been SSA-versioned into a local even when it denotes a parameter; the formal index
    /// names the parameter the call boundary maps it to. This is how query endpoints come to
    /// denote "a vertex at a particular call site," letting the formatter distinguish flows
    /// that share a formal but differ by call site.
    pub fn anchored_at_callsites(
        self,
        formal: FormalIndex,
        call: &[(PackedInsnSiteId, FunctionId)],
    ) -> Vec<Self> {
        // The globals pseudo-formal does not cross a call boundary as an argument.
        if *formal == crate::codegen::GLOBALS_INDEX {
            return vec![self];
        }
        let mut out = Vec::new();
        for (site, callee) in call {
            if *callee != self.infunc {
                continue;
            }
            let Ok(insn_site) = InsnSiteId::try_from(site) else {
                continue;
            };
            let Ok(call_arg) = PackedCallArg::try_from_parts(insn_site.insn_id, formal) else {
                continue;
            };
            out.push(QueryEndpoint {
                infunc: insn_site.func_id,
                vertex: FlowVertex(FlowVariable::call_arg_packed(call_arg), self.vertex.1),
                label: self.label.clone(),
                direction: self.direction,
                call_site: Some(*site),
            });
        }
        if out.is_empty() { vec![self] } else { out }
    }
}

#[derive(Default, Debug, Clone, Builder)]
pub struct QueryFacts {
    #[builder(default)]
    pub formal_param: Vec<(FunctionId, FlowVariable, FormalType)>,
    #[builder(default)]
    pub actual_param: Vec<(PackedInsnSiteId, FormalIndex, FlowVertex)>,
    #[builder(default)]
    pub call: Vec<(PackedInsnSiteId, FunctionId)>,
    #[builder(default)]
    pub assign: Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)>,
    #[builder(default)]
    pub paths: Vec<(Path,)>,
    /// External (unmodeled) functions. Used to derive `absorbing_functions`: an
    /// external function that receives tainted data as an argument absorbs it.
    #[builder(default)]
    pub external_function: Vec<(FunctionId,)>,
    /// Sources and sinks for query. Data flow is followed forward from sources and backward from
    /// sinks
    #[builder(default)]
    pub endpoints: Vec<(QueryEndpoint,)>,
}

#[derive(Default, Debug, Clone)]
pub struct QueryResult {
    pub taint: Vec<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)>,
    /// Edges of the taint graph, in execution / data-flow order (source-then-destination):
    /// `(edge, src_func, src_var, src_path, dst_func, dst_var, dst_path)`. `edge` is the
    /// [`FlowEdge`] classifying the step as `Intra`, `Call`, or `Return`; call/return edges
    /// carry the call instruction that anchors them.
    pub taint_edge: Vec<(
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )>,
    /// Tainted call-argument vertices keyed by call instruction, for
    /// instruction-level reporting: `(site, label, variable, access path)`.
    /// Derived from the taint closure in the same pass.
    pub tainted_insn: Vec<(PackedInsnSiteId, Label, FlowVariable, Path)>,
    /// External functions that receive tainted data as an argument (they "absorb"
    /// the taint): `(function, tainting endpoint, formal index)`. Derived from the
    /// taint closure in the same pass.
    pub absorbing_functions: Vec<(FunctionId, QueryEndpoint, FormalIndex)>,
}

impl QueryResult {
    pub fn new() -> Self {
        Self {
            taint: Default::default(),
            taint_edge: Default::default(),
            tainted_insn: Default::default(),
            absorbing_functions: Default::default(),
        }
    }

    pub fn display<'a>(&'a self, id_map: Option<&'a IdMap>) -> QueryResultDisplay<'a> {
        QueryResultDisplay {
            result: self,
            id_map,
        }
    }
}

impl std::fmt::Display for QueryResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.display(None).fmt(f)
    }
}

pub struct QueryResultDisplay<'a> {
    result: &'a QueryResult,
    id_map: Option<&'a IdMap>,
}

impl<'a> std::fmt::Display for QueryResultDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (func_id, taint_state, flow_var, path, endpoint) in &self.result.taint {
            let var_path_str = {
                let var_str = match flow_var.kind() {
                    FlowVariableKind::Local(name) => name.to_string(),
                    _ => format!("{}", flow_var),
                };
                format!("{}{}", var_str, path.to_dot_string())
            };

            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            let taint_state_str = format!("{:?}", taint_state);
            writeln!(
                f,
                "{}({}): {:<10} {} <-- {}",
                func_name,
                func_id.id,
                taint_state_str,
                var_path_str,
                endpoint.display(self.id_map),
            )?;
        }
        Ok(())
    }
}

/// A union-find key: a variable within a function. Empty-path copy edges
/// (`y = x`) are intraprocedural, so the function id is part of the key.
type CopyKey = (FunctionId, FlowVariable);

/// Union-find `find` with path compression.
fn copy_find(parent: &mut std::collections::HashMap<CopyKey, CopyKey>, x: CopyKey) -> CopyKey {
    let mut root = x.clone();
    while let Some(p) = parent.get(&root) {
        if *p == root {
            break;
        }
        root = p.clone();
    }
    // Path-compress everything on the way to the root.
    let mut cur = x;
    while cur != root {
        let next = parent.get(&cur).cloned().unwrap_or_else(|| root.clone());
        parent.insert(cur, root.clone());
        cur = next;
    }
    root
}

/// Precomputes the copy-class equivalence consumed by the `copy_alias` relation.
///
/// Runs union-find over the empty-path copy edges (`assign_like(f, dst, ∅, src, ∅)`,
/// i.e. register-to-register moves `dst = src`) and returns, for every variable that
/// is *not* its group's representative, the pair `(f, member, representative)`
/// (`member != representative`). Singletons — variables in no copy edge — never enter
/// the union-find and so produce no rows. This is the O(C) replacement for the old
/// Θ(C²) all-pairs empty-path `alias_of_field` closure: instead of materializing
/// every pair of a C-variable copy group, we hand the taint engine one edge per
/// non-representative member and let taint equalize through the representative.
fn compute_copy_alias(
    assign: &[(FunctionId, FlowVariable, Path, FlowVariable, Path)],
) -> Vec<(FunctionId, FlowVariable, FlowVariable)> {
    use std::collections::HashMap;
    let mut parent: HashMap<CopyKey, CopyKey> = HashMap::new();
    for (f, dst, dp, src, sp) in assign {
        if !dp.is_empty() || !sp.is_empty() {
            continue;
        }
        let a: CopyKey = (*f, dst.clone());
        let b: CopyKey = (*f, src.clone());
        parent.entry(a.clone()).or_insert_with(|| a.clone());
        parent.entry(b.clone()).or_insert_with(|| b.clone());
        let ra = copy_find(&mut parent, a);
        let rb = copy_find(&mut parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let keys: Vec<CopyKey> = parent.keys().cloned().collect();
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        let root = copy_find(&mut parent, k.clone());
        if root != k {
            // dst and src of a copy edge share a function, so k.0 == root.0.
            out.push((k.0, k.1, root.1));
        }
    }
    out
}

/// Runs the query-phase taint analysis.
///
/// The default regime is the demand-driven graph search ([`search::taint_search`]):
/// sources are partitioned by label, and each label set runs one multi-start
/// realizable-path search directly over the program tables, with aliasing
/// consulted through a union-find of the copy classes. Set `CTADL_QUERY_DATALOG=1`
/// to fall back to the datalog closure engine ([`taint_analysis_datalog`]).
pub fn taint_analysis(facts: QueryFacts, id_map: Option<&IdMap>) -> QueryResult {
    if std::env::var("CTADL_QUERY_DATALOG").is_ok() {
        taint_analysis_datalog(facts, id_map)
    } else {
        search::taint_search(facts, id_map)
    }
}

/// Taint analysis datalog rules.
///
/// Runs taint analysis given the set of query facts, which include relations from the 'index'
/// phase and a set of taint sources. Returns a relation containing the set of vertices tainted by
/// each taint source. This is the closure (fixpoint) engine; the default regime is the
/// demand-driven search in [`search`], which materializes only what its searches reach.
pub fn taint_analysis_datalog(facts: QueryFacts, id_map: Option<&IdMap>) -> QueryResult {
    ascent! {
        struct QueryEngine;
        // Besides recording `taint`, every propagation rule records a `taint_edge` for
        // the taint graph. The graph is oriented in execution / data-flow order, but
        // taint is discovered both forward (from sources) and backward (from sinks). A
        // forward step already runs source -> derived in execution order; a backward step
        // discovers the *upstream* vertex, so the edge must be reversed to keep it in
        // execution order. Each rule head emits the derived vertex `(func, var, path)`
        // paired with the source vertex it came from as a direction-tagged
        // `taint_edge_directed`; the two rules below orient it into `taint_edge`.
        // Orientation can't be chosen in a rule head, so it is deferred to those rules.
        relation formal_param(FunctionId, FlowVariable, FormalType);
        relation call(PackedInsnSiteId, FunctionId);
        relation assign_like(FunctionId, FlowVariable, Path, FlowVariable, Path);
        relation paths(Path);
        relation sources(QueryEndpoint);

        relation alias_of_field(FunctionId, FlowVariable, FlowVariable, Path);
        relation taint(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint);

        // Copy-class equivalence, precomputed by union-find over empty-path copy
        // edges (`y = x`, both paths empty) in `compute_copy_alias`. `copy_alias(f,
        // member, rep)` maps every non-representative variable of a copy-connected
        // group to its representative (`member != rep`). This replaces the old
        // all-pairs empty-path `alias_of_field` closure (rules R2/R3/R4), which
        // materialized Θ(C²) rows for a group of C copy-connected variables — the
        // query-phase memory blowup. Register-to-register moves in firmware SSA form
        // groups of C≈10⁴, so Θ(C²) reached 10⁸–10⁹ rows. Union-find collapses that
        // to O(C): 2 edges (member↔rep) per variable, and taint equalizes across the
        // group by routing through the representative in two hops (see the equalize
        // rules just below).
        relation copy_alias(FunctionId, FlowVariable, FlowVariable);

        // Initialize taint with source
        taint(infunc, TaintState::Free, v.clone(), p.clone(), s) <--
            sources(s),
            let QueryEndpoint { infunc, vertex, label, direction, call_site: _ } = s,
            let FlowVertex(v, p) = vertex;

        // Propagate taint locally onto fields
        taint(infunc, ts, v1.clone(), p13.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v1.clone(), p13.clone(), infunc, v2.clone(), p23.clone(), a.direction) <--
            taint(infunc, ts, v2, p23, a),
            if a.direction == TaintDirection::Forward,
            assign_like(infunc, v1, p1, v2, p2),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        taint(infunc, ts, v1.clone(), p13.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v1.clone(), p13.clone(), infunc, v2.clone(), p23.clone(), a.direction) <--
            taint(infunc, ts, v2, p23, a),
            if a.direction == TaintDirection::Backward,
            assign_like(infunc, v2, p2, v1, p1),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        // Formal-to-actual (Return in forward mode, Call in backward mode).
        taint(func_id, TaintState::Free, v1.clone(), p2.clone(), a.clone()),
        taint_edge_directed(if a.direction == TaintDirection::Forward { FlowEdge::Return(*site_id) } else { FlowEdge::Call(*site_id) }, func_id, v1.clone(), p2.clone(), infunc, v2.clone(), p2.clone(), a.direction) <--
            taint(infunc, TaintState::Free, v2, p2, a),
            formal_param(infunc, v2, formal_ty),
            if let Some(n2) = v2.as_formal(),
            if (a.direction == TaintDirection::Forward && isout(&n2, *formal_ty, p2)) ||
                (a.direction == TaintDirection::Backward /* && isin(n2.0) */),
            call(site_id, infunc),
            let InsnSiteId {func_id, insn_id} = InsnSiteId::unpack_from_slice(&**site_id).unwrap(),
            let call_arg_packed = PackedCallArg::try_from_parts(insn_id, n2).unwrap(),
            let v1 = FlowVariable::call_arg_packed(call_arg_packed);

        // Actual-to-formal (Call in forward mode, Return in backward mode).
        taint(func, TaintState::Restricted, formal_var.clone(), p2.clone(), a.clone()),
        taint_edge_directed(if a.direction == TaintDirection::Forward { FlowEdge::Call(site_id) } else { FlowEdge::Return(site_id) }, func, formal_var.clone(), p2.clone(), infunc, v2.clone(), p2.clone(), a.direction) <--
            taint(infunc, sts, v2, p2, a),
            if let Some(packed) = v2.as_call_arg(),
            let CallArgId { insn_id, formal: formal_raw } = CallArgId::try_from(packed).unwrap(),
            let formal = FormalIndex::from(formal_raw),
            let site_id = PackedInsnSiteId::try_from_parts(*infunc, insn_id).unwrap(),
            call(site_id, func),
            let formal_var = FlowVariable::formal_index(formal),
            formal_param(func, formal_var, formal_ty),
            if a.direction == TaintDirection::Forward /* && isin(formal)) */ ||
                (a.direction == TaintDirection::Backward && isout(&formal, *formal_ty, p2));

        // R1: a field/offset read `x = a.p` seeds a field alias (`x` holds `a`'s
        // field `p`). Non-empty path only; the empty-path copy case is handled by
        // the copy-class equalize rules below, not here. Gated on the base `a` being
        // tainted ("aliases of tainted things only"). This produces O(loads) rows,
        // not Θ(C²): it is not the blowup. Copies of `x` need not be enumerated here
        // — the equalize rules route their taint through `x`'s representative.
        alias_of_field(infunc, x.clone(), a.clone(), p.clone()) <--
            taint(infunc, _, a, _, _),
            assign_like(infunc, x, Path::empty(), a, p),
            if !p.is_empty();

        // Copy-class taint equalization — the union-find replacement for the old
        // all-pairs empty-path alias closure. A copy-connected group shares taint,
        // routed through its representative so the cost is O(C) rather than Θ(C²):
        // every member's taint collapses onto the rep, and the rep's taint expands
        // to every member (two hops connect any two members). Empty-path taint is
        // equalized unconditionally (matching the old ungated `alias_of_field(_, _,
        // ∅)` propagation); non-empty-path taint is equalized only for materialized
        // paths (matching the old `paths(p12)` gate). State and endpoint ride along
        // unchanged, exactly as the old alias propagation carried them.
        taint(infunc, ts, rep.clone(), Path::empty(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, rep.clone(), Path::empty(), infunc, v.clone(), Path::empty(), a.direction) <--
            taint(infunc, ts, v, Path::empty(), a),
            copy_alias(infunc, v, rep);
        taint(infunc, ts, v.clone(), Path::empty(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v.clone(), Path::empty(), infunc, rep.clone(), Path::empty(), a.direction) <--
            taint(infunc, ts, rep, Path::empty(), a),
            copy_alias(infunc, v, rep);
        taint(infunc, ts, rep.clone(), p.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, rep.clone(), p.clone(), infunc, v.clone(), p.clone(), a.direction) <--
            taint(infunc, ts, v, p, a),
            if !p.is_empty(),
            copy_alias(infunc, v, rep),
            paths(p.clone());
        taint(infunc, ts, v.clone(), p.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v.clone(), p.clone(), infunc, rep.clone(), p.clone(), a.direction) <--
            taint(infunc, ts, rep, p, a),
            if !p.is_empty(),
            copy_alias(infunc, v, rep),
            paths(p.clone());

        // Propagates taint on a variable into its alias.
        taint(infunc, st, v1.clone(), p.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v1.clone(), p.clone(), infunc, v2.clone(), Path::empty(), a.direction) <--
            taint(infunc, st, v2, Path::empty(), a),
            if a.direction == TaintDirection::Forward,
            alias_of_field(infunc, v2, v1, p);

        taint(infunc, st, v1.clone(), p12.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v1.clone(), p12.clone(), infunc, v2.clone(), p2.clone(), a.direction) <--
            taint(infunc, st, v2, p2, a),
            if a.direction == TaintDirection::Forward,
            alias_of_field(infunc, v2, v1, p1),
            let p12 = p1.concat(p2),
            paths(p12.clone());

        // Backward alias propagation
        taint(infunc, st, v1.clone(), Path::empty(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v1.clone(), Path::empty(), infunc, v2.clone(), p.clone(), a.direction) <--
            taint(infunc, st, v2, p, a),
            if a.direction == TaintDirection::Backward,
            alias_of_field(infunc, v1, v2, p);

        taint(infunc, st, v2.clone(), p2.clone(), a.clone()),
        taint_edge_directed(FlowEdge::Intra, infunc, v2.clone(), p2.clone(), infunc, v1.clone(), p12.clone(), a.direction) <--
            taint(infunc, st, v1, p12, a),
            if a.direction == TaintDirection::Backward,
            alias_of_field(infunc, v1, v2, p1),
            if let Some(p2) = p12.substitute_prefix(p1, &Path::empty()),
            paths(p2.clone());

        // Direction-tagged edge as produced by the propagation rules above: destination
        // vertex `(func, var, path)` derived from source vertex `(func, var, path)`,
        // classified by `edge` (the execution-order [`FlowEdge`]), tagged with the
        // direction of the endpoint the propagation belongs to.
        relation taint_edge_directed(FlowEdge, FunctionId, FlowVariable, Path, FunctionId, FlowVariable, Path, TaintDirection);
        // The taint graph, in execution / data-flow order (source-then-destination):
        // `(edge, src_func, src_var, src_path, dst_func, dst_var, dst_path)`.
        relation taint_edge(FlowEdge, FunctionId, FlowVariable, Path, FunctionId, FlowVariable, Path);

        // Forward: the produced edge already runs source -> derived in execution order.
        taint_edge(*edge, *sf, sv.clone(), sp.clone(), *df, dv.clone(), dp.clone()) <--
            taint_edge_directed(edge, df, dv, dp, sf, sv, sp, dir),
            if *dir == TaintDirection::Forward;

        // Backward: the produced (derived) vertex is upstream, so reverse the edge to
        // keep it in execution order. The edge classification already describes the
        // execution-order step, so it is unchanged.
        taint_edge(*edge, *df, dv.clone(), dp.clone(), *sf, sv.clone(), sp.clone()) <--
            taint_edge_directed(edge, df, dv, dp, sf, sv, sp, dir),
            if *dir == TaintDirection::Backward;

        // Instruction-level facts derived from the taint closure in this same pass
        // (they used to be recomputed by a second engine in the formatter). Both are
        // non-recursive projections of `taint`.
        relation external_function(FunctionId);
        relation absorbing_functions(FunctionId, QueryEndpoint, FormalIndex);
        relation tainted_var_at_insn(PackedInsnSiteId, Label, FlowVariable, Path);

        // An external (unmodeled) function that receives tainted data as an argument
        // absorbs it.
        absorbing_functions(target, src, formal.clone()) <--
            taint(infunc, _, v, _, src),
            if let Some(packed) = v.as_call_arg(),
            let call_arg_id = CallArgId::try_from(packed).unwrap(),
            let formal = call_arg_id.formal(),
            let id = PackedInsnSiteId::try_from_parts(*infunc, call_arg_id.insn_id).unwrap(),
            call(id, target),
            external_function(target);

        // Tainted call-argument vertices, keyed by the call instruction.
        tainted_var_at_insn(id, label, v2, p2) <--
            taint(infunc, _, v2, p2, src),
            if !v2.is_globals(),
            if let Some(packed) = v2.as_call_arg(),
            let call_arg_id = CallArgId::try_from(packed).unwrap(),
            let id = PackedInsnSiteId::try_from_parts(*infunc, call_arg_id.insn_id).unwrap(),
            if *call_arg_id.formal() >= 0,
            let label = src.label.clone();
    }

    let copy_alias = compute_copy_alias(&facts.assign);

    let mut engine = QueryEngine {
        formal_param: facts.formal_param,
        call: facts.call,
        assign_like: facts.assign,
        paths: facts.paths,
        external_function: facts.external_function,
        sources: facts.endpoints,
        copy_alias,
        ..Default::default()
    };
    engine.run();

    if std::env::var("CTADL_QUERY_SIZES").is_ok() {
        eprintln!(
            "QUERY_SIZES taint={} taint_edge={} taint_edge_directed={} alias_of_field={} tainted_var_at_insn={} assign_like={} paths={} sources={}",
            engine.taint.len(),
            engine.taint_edge.len(),
            engine.taint_edge_directed.len(),
            engine.alias_of_field.len(),
            engine.tainted_var_at_insn.len(),
            engine.assign_like.len(),
            engine.paths.len(),
            engine.sources.len(),
        );
    }

    log::trace!(
        "query result: {}",
        DisplayTaint {
            taint: &engine.taint,
            id_map,
        }
    );
    QueryResult {
        taint: engine.taint,
        taint_edge: engine.taint_edge,
        tainted_insn: engine.tainted_var_at_insn.into_iter().collect(),
        absorbing_functions: engine.absorbing_functions.into_iter().collect(),
    }
}

pub mod formatter;
pub mod search;

struct DisplayTaint<'a> {
    taint: &'a [(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)],
    id_map: Option<&'a IdMap>,
}

impl<'a> std::fmt::Display for DisplayTaint<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Taint output")?;
        for (func_id, ts, var, path, endpoint) in self.taint {
            let func_name = self
                .id_map
                .and_then(|m| m.get_function(*func_id))
                .map(|f| f.0.as_ref())
                .unwrap_or("unknown");

            writeln!(
                f,
                "  {}({}) {:?} {}{} <- {}",
                func_name,
                func_id.id,
                ts,
                var,
                path.to_dot_string(),
                endpoint.display(self.id_map),
            )?;
        }
        Ok(())
    }
}

impl std::fmt::Display for QueryEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.display(None).fmt(f)
    }
}

pub struct QueryEndpointDisplay<'a> {
    endpoint: &'a QueryEndpoint,
    id_map: Option<&'a IdMap>,
}

impl<'a> std::fmt::Display for QueryEndpointDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let QueryEndpoint {
            label,
            direction,
            infunc,
            vertex,
            call_site,
        } = self.endpoint;

        let func_name = self
            .id_map
            .and_then(|m| m.get_function(*infunc))
            .map(|f| f.0.as_ref())
            .unwrap_or("unknown");

        write!(
            f,
            "{label} {direction} {func_name}({}) {}{}",
            infunc.id,
            vertex.0,
            vertex.1.to_dot_string()
        )?;
        if let Some(site) = call_site {
            write!(f, " @{site}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // use super::*;

    // #[test]
    // fn test_read_no_taint() {
    //     // this should not throw an exception
    //     let _result = QueryResult::new()
    //         .load(path::PathBuf::from("/tmp"))
    //         .unwrap();
    //     assert!(true);
    // }
}
