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
   threads a [`PathState`] annotation along the edges so call/return matching
   is respected: a `Call` edge enters `Restricted`, and a `Return` edge is only
   traversable while `Free`. The annotation also carries a [`CallString`]
   *context obligation*, so the context-conditional edges of a dynamically
   dispatched site — the contextual assignments and the resolved call/return
   edges — are traversed only under a context they are consistent with.
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
    CallArgId, CallString, FlowEdge, FlowVariable, FormalType, FunctionId, IdMap, InsnSiteId,
    Label, PackedCallArg, PackedInsnSiteId, Path, TaintDirection, TaintLevel, TaintState, isout,
};

use super::{QueryEndpoint, QueryFacts, QueryResult, compute_copy_alias, subsume_resolved_calls};

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

/// The label a search edge carries.
///
/// `FlowEdge` is what gets *persisted* (in `taint_edge.parquet`), and it has no room for a
/// calling context; rather than widen a stored schema for a search-local concern, the search
/// uses its own label and maps back to `FlowEdge` when it emits path edges. `Ctx` names the
/// context the edge holds under; a contextual assign emits as an `Intra` step at the call site,
/// and a contextual call/return keeps its `Call`/`Return`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Step {
    /// An unconditional edge: valid whatever the calling context.
    Flow(FlowEdge),
    /// An edge derived from a context-conditional row, valid only under the call string it
    /// carries. Traversable iff [`refine`] accepts it against the state's current obligation.
    Ctx(CallString, FlowEdge),
}

impl Step {
    /// The persisted [`FlowEdge`] this step reports as. A contextual step reports as the plain
    /// edge of the same kind: the context is how the search *decided* to take it, not part of
    /// the flow a consumer walks.
    fn flow_edge(&self) -> FlowEdge {
        match self {
            Step::Flow(e) => *e,
            Step::Ctx(_, e) => *e,
        }
    }
}

/// The search annotation: the one-bit call/return discipline plus the calling-context
/// obligation accumulated along the path.
///
/// Deliberately *not* the persisted [`TaintState`]: that is a bool column of `taint.parquet`
/// (`facts::schema::taint`), and the context is a search-local concern. `taint` rows are emitted
/// from `state` alone. [`CallString`] is interned and `Copy`, so this stays `Copy` and satisfies
/// [`LazyAnnotation`]'s `Eq + Hash` bound.
///
/// Cost note: a search state is `(node, annotation)`, so a vertex reached under *k* distinct
/// contexts becomes *k* states. Contexts are introduced only by contextual edges and only shrink
/// at returns, so *k* is bounded by the distinct call strings on the rows a search actually
/// touches — zero on a target with no resolvable dispatch, which is the common case.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PathState {
    /// The call/return discipline: `Call` enters `Restricted`, `Return` needs `Free`.
    pub state: TaintState,
    /// What this path has committed to about the stack it is running on. Empty means "no
    /// obligation yet", which is compatible with everything.
    pub ctx: CallString,
}

/// The conjunction of two context obligations, or `None` if they cannot both hold.
///
/// A call string is ordered outermost-first, innermost-last (`push` appends, `pop` takes the
/// last), so the *current* frame sits at the end. `[s1,s2]` and `[s2]` agree — both say "this
/// frame was entered at s2"; the first adds that its caller was entered at s1 — so two
/// obligations are jointly satisfiable exactly when one is a suffix of the other, and their
/// conjunction is the longer (the more refined) of the two. The empty context is a suffix of
/// everything, which is the "no obligation yet" case.
fn refine(ctx: CallString, row: CallString) -> Option<CallString> {
    let (long, short) = if ctx.len() >= row.len() {
        (ctx, row)
    } else {
        (row, ctx)
    };
    if long[long.len() - short.len()..] == short[..] {
        Some(long)
    } else {
        None
    }
}

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
    /// `context_assign` edges, indexed exactly as `assign_by_src` but carrying the call string
    /// each row holds under: `(f, src) -> [(dst, dst_path, src_path, context)]`. These are the
    /// summary instantiations of a resolved indirect call — the rows the index computes and
    /// then has no rule to *use* in the frame that contains the call.
    ctx_assign_by_src:
        HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path, Path, CallString)>>,
    /// The load-shaped `context_assign` rows, indexed by destination: the contextual twin of
    /// `loads_by_dst`. Without it the alias back-flow that a collapse into `assign_like` would
    /// have got for free is simply missing from the contextual rows.
    ctx_loads_by_dst: HashMap<(FunctionId, FlowVariable), Vec<(FlowVariable, Path, CallString)>>,
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
    /// Callees of each call site, for actual-to-formal (call entry) steps.
    callee_by_site: HashMap<PackedInsnSiteId, Vec<FunctionId>>,
    /// Resolved callees of each dynamically dispatched site, with the context each resolution
    /// holds under: the call-entry direction of `resolved_call`. Anchored on the *dispatch*
    /// instruction the index recorded, so the call-arg vertices this fans out from are exactly
    /// the ones that site's `actual_param` rows created — which is what keeps the argument
    /// convention right without the engine having to know the frontend's.
    resolved_by_site: HashMap<PackedInsnSiteId, Vec<(FunctionId, CallString)>>,
    /// The same rows indexed by target: the *return* direction. This is the edge D4b is about —
    /// a flow that starts or ends inside a resolved callee has no summary describing it, so a
    /// summary instantiation, contextual or not, cannot carry it across the site.
    resolved_by_target: HashMap<FunctionId, Vec<(PackedInsnSiteId, CallString)>>,
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

        // The contextual rows get their own two indices, mirroring `assign_by_src` and
        // `loads_by_dst`. They are deliberately NOT added to those maps, nor to the union-find
        // in `compute_copy_alias` below: most `context_assign` rows are empty-path call-arg
        // copies, and a context-conditional copy entering the union-find merges two copy classes
        // *unconditionally* — handing back exactly the imprecision the contexts exist to avoid.
        // They are also not fed to `loads_by_src`, whose only consumer is the saturating rule.
        let mut ctx_assign_by_src: HashMap<
            (FunctionId, FlowVariable),
            Vec<(FlowVariable, Path, Path, CallString)>,
        > = HashMap::default();
        let mut ctx_loads_by_dst: HashMap<
            (FunctionId, FlowVariable),
            Vec<(FlowVariable, Path, CallString)>,
        > = HashMap::default();
        for (f, dst, dp, src, sp, cs) in &facts.context_assign {
            ctx_assign_by_src
                .entry((*f, *src))
                .or_default()
                .push((*dst, *dp, *sp, *cs));
            if dp.is_empty() && !sp.is_empty() {
                ctx_loads_by_dst
                    .entry((*f, *dst))
                    .or_default()
                    .push((*src, *sp, *cs));
            }
        }

        let formal_ty = facts
            .formal_param
            .iter()
            .map(|(f, v, ty)| ((*f, *v), *ty))
            .collect();

        let mut callers_by_callee: HashMap<FunctionId, Vec<PackedInsnSiteId>> = HashMap::default();
        let mut callee_by_site: HashMap<PackedInsnSiteId, Vec<FunctionId>> = HashMap::default();
        for (site, callee) in &facts.call {
            callers_by_callee.entry(*callee).or_default().push(*site);
            callee_by_site.entry(*site).or_default().push(*callee);
        }
        for callees in callee_by_site.values_mut() {
            callees.sort_unstable();
            callees.dedup();
        }

        // The resolved-call edges, in both directions. `subsume_resolved_calls` first drops the
        // conditional rows an unconditional resolution of the same (site, target) already
        // dominates, so a site that resolves both ways costs one context instead of several.
        let mut resolved_by_site: HashMap<PackedInsnSiteId, Vec<(FunctionId, CallString)>> =
            HashMap::default();
        let mut resolved_by_target: HashMap<FunctionId, Vec<(PackedInsnSiteId, CallString)>> =
            HashMap::default();
        for (f, insn, target, cs) in subsume_resolved_calls(&facts.resolved_call) {
            let Ok(site) = PackedInsnSiteId::try_from_parts(f, insn) else {
                continue;
            };
            resolved_by_site.entry(site).or_default().push((target, cs));
            resolved_by_target
                .entry(target)
                .or_default()
                .push((site, cs));
        }
        for targets in resolved_by_site.values_mut() {
            targets.sort_unstable();
            targets.dedup();
        }
        for sites in resolved_by_target.values_mut() {
            sites.sort_unstable();
            sites.dedup();
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
            ctx_assign_by_src,
            ctx_loads_by_dst,
            loads_by_dst,
            loads_by_src,
            formal_ty,
            callers_by_callee,
            callee_by_site,
            resolved_by_site,
            resolved_by_target,
            paths,
            copy_rep,
            copy_members,
            sink_ext_by_var,
        }
    }
}

impl LazySuccessors for TaintSearchGraph {
    type Node = TaintNode;
    type Label = Step;

    fn labeled_successors(&self, node: &TaintNode) -> Vec<(TaintNode, Step)> {
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
                    out.push(((f, *dst, p2, level), Step::Flow(FlowEdge::Intra)));
                }
            }
        }

        // The same step, for the context-conditional rows: a resolved callee's summary
        // instantiated at the dispatch site. This is D4 — the index derives these and, before
        // this rule, had no consumer that made one usable *where it sits*, so a flow consumed in
        // the frame holding the indirect call was dropped. Whether the edge is actually taken is
        // the annotation's call: `Step::Ctx` is traversable only under a compatible context.
        if let Some(edges) = self.ctx_assign_by_src.get(&(f, v)) {
            for (dst, dp, sp, cs) in edges {
                if let Some(p2) = p.substitute_prefix(sp, dp)
                    && self.paths.contains(&p2)
                {
                    out.push(((f, *dst, p2, level), Step::Ctx(*cs, FlowEdge::Intra)));
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
                    out.push(((f, *a, *q, level), Step::Flow(FlowEdge::Intra)));
                } else {
                    let qp = q.concat(&p);
                    if self.paths.contains(&qp) {
                        out.push(((f, *a, qp, level), Step::Flow(FlowEdge::Intra)));
                    }
                }
            }
        }

        // Field-alias back-flow for the contextual rows, the exact mirror of the block above.
        if let Some(loads) = self.ctx_loads_by_dst.get(&(f, v)) {
            for (a, q, cs) in loads {
                if p.is_empty() {
                    out.push(((f, *a, *q, level), Step::Ctx(*cs, FlowEdge::Intra)));
                } else {
                    let qp = q.concat(&p);
                    if self.paths.contains(&qp) {
                        out.push(((f, *a, qp, level), Step::Ctx(*cs, FlowEdge::Intra)));
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
                out.push(((f, *rep, p, level), Step::Flow(FlowEdge::Intra)));
            }
            if let Some(members) = self.copy_members.get(&(f, v)) {
                for m in members {
                    out.push(((f, *m, p, level), Step::Flow(FlowEdge::Intra)));
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
                        Step::Flow(FlowEdge::Intra),
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
                        out.push((
                            (f, v, *q, TaintLevel::Saturating),
                            Step::Flow(FlowEdge::Intra),
                        ));
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
                    Step::Flow(FlowEdge::Return(*site)),
                ));
            }
        }

        // Formal-to-actual across a *dynamically resolved* call (D4b).
        if let Some(fty) = self.formal_ty.get(&(f, v))
            && let Some(formal) = v.as_formal()
            && isout(&formal, *fty, &p)
            && let Some(sites) = self.resolved_by_target.get(&f)
        {
            for (site, cs) in sites {
                let InsnSiteId { func_id, insn_id } = InsnSiteId::try_from(site).unwrap();
                let Ok(call_arg) = PackedCallArg::try_from_parts(insn_id, formal) else {
                    continue;
                };
                let to = (func_id, FlowVariable::call_arg_packed(call_arg), p, level);
                // An unconditional resolution is a plain return: it obeys the pop discipline
                // like any other. A conditional one is a contextual edge, refined rather than
                // popped — its call string describes the frame being returned *into*.
                out.push((
                    to,
                    if cs.is_empty() {
                        Step::Flow(FlowEdge::Return(*site))
                    } else {
                        Step::Ctx(*cs, FlowEdge::Return(*site))
                    },
                ));
            }
        }

        // Actual-to-formal (call entry): taint on a call-arg vertex enters the
        // callee's formal. The `Call` label puts the annotation in `Restricted`.
        if let Some(packed) = v.as_call_arg() {
            let call_arg_id = CallArgId::try_from(packed).unwrap();
            let site = PackedInsnSiteId::try_from_parts(f, call_arg_id.insn_id).unwrap();
            if let Some(callees) = self.callee_by_site.get(&site) {
                let formal_var = FlowVariable::formal_index(call_arg_id.formal());
                // Every target of the site, not just one: a multi-target site is
                // several independent entry edges (D4c).
                for callee in callees {
                    if self.formal_ty.contains_key(&(*callee, formal_var)) {
                        out.push((
                            (*callee, formal_var, p, level),
                            Step::Flow(FlowEdge::Call(site)),
                        ));
                    }
                }
            }

            // Actual-to-formal across a dynamically resolved call.
            if let Some(targets) = self.resolved_by_site.get(&site) {
                let formal_var = FlowVariable::formal_index(call_arg_id.formal());
                for (target, cs) in targets {
                    if self.formal_ty.contains_key(&(*target, formal_var)) {
                        out.push((
                            (*target, formal_var, p, level),
                            if cs.is_empty() {
                                Step::Flow(FlowEdge::Call(site))
                            } else {
                                Step::Ctx(*cs, FlowEdge::Call(site))
                            },
                        ));
                    }
                }
            }
        }

        out
    }
}

/// The same one-bit call/return discipline the formatter's realizable-path search uses (see the
/// [`Annotation`](ctadl_ir::graph::Annotation) impl for [`TaintState`] in the formatter), applied
/// during taint discovery itself — plus the calling-context obligation that makes the
/// context-conditional edges of a resolved indirect call safe to take.
///
/// The three rules, matching the three edge shapes:
///
/// - **Unconditional edge** ([`Step::Flow`]) — an `Intra` step preserves the state, a `Call`
///   step enters `Restricted`, and a `Return` step is only traversable while `Free`. A `Return`
///   additionally *pops*: when the obligation is non-empty its top frame must be this return's
///   site, since the current frame sits at the end of a call string; a mismatch prunes.
/// - **Contextual edge** ([`Step::Ctx`]) — traversable iff [`refine`] can conjoin the row's call
///   string with the current obligation; the new obligation is that refinement. A contextual
///   `Return` still needs `Free`, but it is refined rather than popped: its call string
///   describes the frame it returns *into*, so it is an obligation being acquired, not
///   discharged.
/// - **`Call`** — pushes nothing, contextual or not.
///
/// The asymmetry between pushing and popping is deliberate. `resolvent` and `context_assign` are
/// lattices keyed on their non-context columns, so the recorded call string is a *witness*, not an
/// enumeration: a tuple derivable under two contexts records only the smaller. Pushing on `Call`
/// and testing entry against that witness would reject flows entering through the merged-away site.
/// Popping on `Return` is safe by comparison, because what it can prune are flows that *leave* the
/// frame, and leaving already has a complete mechanism of its own: the index lifts a contextual
/// flow reaching an out-formal into a `context_summary` and pops it into the caller. The residual
/// gap — a flow that both starts inside the frame and returns has no summary counterpart, and the
/// witness may name a different caller — is narrow; the mitigation, should it show up, is to clear
/// the obligation on mismatch instead of pruning.
impl LazyAnnotation<TaintSearchGraph> for PathState {
    fn start() -> Self {
        PathState {
            state: TaintState::Free,
            ctx: CallString::new(),
        }
    }

    fn expand(
        &self,
        _graph: &TaintSearchGraph,
        _from: &TaintNode,
        label: &Step,
        _to: &TaintNode,
    ) -> Option<Self> {
        match label {
            Step::Flow(FlowEdge::Intra) => Some(*self),
            Step::Flow(FlowEdge::Call(_)) => Some(PathState {
                state: TaintState::Restricted,
                ctx: self.ctx,
            }),
            Step::Flow(FlowEdge::Return(site)) => {
                if self.state != TaintState::Free {
                    return None;
                }
                match self.ctx.top() {
                    // No obligation: the plain pre-context behaviour.
                    None => Some(*self),
                    Some(top) if top == *site => {
                        let (popped, _) = self.ctx.pop();
                        Some(PathState {
                            state: TaintState::Free,
                            ctx: popped,
                        })
                    }
                    // Returning through a site the obligation says we did not enter through.
                    Some(_) => None,
                }
            }
            Step::Ctx(row, edge) => {
                let ctx = refine(self.ctx, *row)?;
                match edge {
                    FlowEdge::Intra => Some(PathState {
                        state: self.state,
                        ctx,
                    }),
                    FlowEdge::Call(_) => Some(PathState {
                        state: TaintState::Restricted,
                        ctx,
                    }),
                    FlowEdge::Return(_) => {
                        if self.state != TaintState::Free {
                            return None;
                        }
                        Some(PathState {
                            state: TaintState::Free,
                            ctx,
                        })
                    }
                }
            }
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

        let search = find_annotated_paths_from_set(&graph, starts, |n, _s: &PathState| {
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
            // Emission drops the context along with the level: `taint` is a persisted table
            // whose state column is a bool, and the obligation is a search-local concern.
            taint.push((st.node.0, st.annot.state, st.node.1, st.node.2, ep.clone()));
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
                    // A contextual step reports as the plain edge of the same kind; the
                    // formatter re-walks `taint_edge` with the ordinary `TaintState`
                    // discipline and is unaffected by how the search decided to take it.
                    st.edge.unwrap().flow_edge(),
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
                    sink_tags.insert((
                        st.node.0,
                        st.annot.state,
                        st.node.1,
                        st.node.2,
                        sink.clone(),
                    ));
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
        if let Some(targets) = graph.callee_by_site.get(&site) {
            for target in targets {
                if external.contains(target) {
                    absorbing.insert((*target, src.clone(), call_arg_id.formal()));
                }
            }
        }
    }

    if std::env::var("CTADL_QUERY_SIZES").is_ok() {
        log::info!(
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
    use crate::facts::{FlowVertex, FormalIndex, InsnId, Label};

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

    /// D4c: a call site with several `call` rows is a *multi*-target site, and
    /// the call-entry edge must fan out over every one of them. Building
    /// `callee_by_site` with a plain `insert` kept whichever row loaded last, so
    /// a flow into any other target silently vanished — while the datalog
    /// regime, which joins `call` as a relation, found it. Both targets must get
    /// an entry edge.
    #[test]
    fn multi_target_site_enters_every_callee() {
        let caller = FunctionId::new(0);
        let callee_a = FunctionId::new(1);
        let callee_b = FunctionId::new(2);
        let insn = InsnId::new(7);
        let site = PackedInsnSiteId::try_from_parts(caller, insn).unwrap();
        let formal = FormalIndex::new(0);
        let call_arg = PackedCallArg::try_from_parts(insn, formal).unwrap();
        let arg_var = FlowVariable::call_arg_packed(call_arg);
        let formal_var = FlowVariable::formal_index(formal);

        let facts = QueryFacts {
            // Both targets are reachable from the one site.
            call: vec![(site, callee_a), (site, callee_b)],
            formal_param: vec![
                (callee_a, formal_var, FormalType::ByVal),
                (callee_b, formal_var, FormalType::ByVal),
            ],
            paths: vec![(Path::empty(),)],
            ..Default::default()
        };
        let graph = TaintSearchGraph::new(&facts);

        let succs = graph.labeled_successors(&(caller, arg_var, Path::empty(), TaintLevel::Plain));
        let entered: Vec<FunctionId> = succs
            .iter()
            .filter(|(_, e)| matches!(e, Step::Flow(FlowEdge::Call(s)) if *s == site))
            .map(|(n, _)| n.0)
            .collect();
        assert!(
            entered.contains(&callee_a) && entered.contains(&callee_b),
            "call entry must fan out over both targets, got {entered:?}"
        );
    }

    /// [`refine`] is suffix compatibility, not identity: two obligations are jointly satisfiable
    /// exactly when one is a suffix of the other (the current frame sits at the *end* of a call
    /// string), and their conjunction is the longer one.
    #[test]
    fn refine_conjoins_suffix_compatible_contexts() {
        let s = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let (a, b, c) = (s(1, 1), s(2, 2), s(3, 3));
        let empty = CallString::new();
        let ab = CallString::intern(&[a, b]);
        let b_only = CallString::intern(&[b]);
        let cb = CallString::intern(&[c, b]);
        let a_only = CallString::intern(&[a]);

        // Empty is a suffix of everything: no obligation yet, so anything refines it.
        assert_eq!(refine(empty, ab), Some(ab));
        assert_eq!(refine(ab, empty), Some(ab));
        assert_eq!(refine(empty, empty), Some(empty));
        // `[a,b]` and `[b]` agree that this frame was entered at `b`; the refinement is the
        // longer, which additionally says the caller was entered at `a`.
        assert_eq!(refine(ab, b_only), Some(ab));
        assert_eq!(refine(b_only, ab), Some(ab));
        // Identity.
        assert_eq!(refine(ab, ab), Some(ab));
        // `[a,b]` and `[c,b]` agree on this frame but disagree about the caller: incompatible.
        assert_eq!(refine(ab, cb), None);
        // `[a]` says this frame was entered at `a`; `[b]` says at `b`. Not a suffix either way.
        assert_eq!(refine(a_only, b_only), None);
        // A *prefix* is not a suffix: `[a,b]` vs `[a]` disagree about the current frame.
        assert_eq!(refine(ab, a_only), None);
    }

    /// A `context_assign` row is traversable under a compatible obligation and pruned under an
    /// incompatible one, and traversing it *acquires* the obligation.
    #[test]
    fn a_contextual_edge_is_traversed_only_under_a_compatible_context() {
        let s = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let row = CallString::intern(&[s(1, 1)]);
        let other = CallString::intern(&[s(2, 2)]);
        let graph = TaintSearchGraph::new(&QueryFacts::default());
        let node = (
            FunctionId::new(0),
            FlowVariable::default(),
            Path::empty(),
            TaintLevel::Plain,
        );
        let label = Step::Ctx(row, FlowEdge::Intra);

        // No obligation yet: traversable, and the row's context becomes the obligation.
        let start = <PathState as LazyAnnotation<TaintSearchGraph>>::start();
        let got = start
            .expand(&graph, &node, &label, &node)
            .expect("compatible");
        assert_eq!(got.ctx, row);
        assert_eq!(got.state, TaintState::Free);

        // An obligation the row contradicts prunes the edge outright.
        let conflicting = PathState {
            state: TaintState::Free,
            ctx: other,
        };
        assert!(
            conflicting.expand(&graph, &node, &label, &node).is_none(),
            "a row whose context contradicts the obligation must not be traversable"
        );
    }

    /// The pop discipline on an ordinary return: the obligation names the site the current frame
    /// was entered through, so a return through any other site is not realizable and is pruned,
    /// and a return through *that* site discharges it.
    #[test]
    fn an_ordinary_return_pops_its_site_and_prunes_a_mismatch() {
        let s = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let (entered_at, elsewhere) = (s(1, 1), s(2, 2));
        let outer = s(3, 3);
        let graph = TaintSearchGraph::new(&QueryFacts::default());
        let node = (
            FunctionId::new(0),
            FlowVariable::default(),
            Path::empty(),
            TaintLevel::Plain,
        );
        let st = PathState {
            state: TaintState::Free,
            ctx: CallString::intern(&[outer, entered_at]),
        };

        // Returning through the site the obligation names: allowed, and the frame is popped, so
        // what survives is the obligation about the caller.
        let ok = st
            .expand(
                &graph,
                &node,
                &Step::Flow(FlowEdge::Return(entered_at)),
                &node,
            )
            .expect("returning through the entry site is realizable");
        assert_eq!(ok.ctx, CallString::intern(&[outer]));

        // Returning through any other site contradicts the obligation.
        assert!(
            st.expand(
                &graph,
                &node,
                &Step::Flow(FlowEdge::Return(elsewhere)),
                &node
            )
            .is_none(),
            "a return through a site the context did not enter through must be pruned"
        );

        // And the pre-existing one-bit discipline is unchanged: a `Return` still needs `Free`.
        let restricted = PathState {
            state: TaintState::Restricted,
            ctx: CallString::new(),
        };
        assert!(
            restricted
                .expand(
                    &graph,
                    &node,
                    &Step::Flow(FlowEdge::Return(entered_at)),
                    &node
                )
                .is_none(),
            "a Return out of a Restricted state is still unrealizable"
        );
    }

    /// D4b, end to end through the graph: a `resolved_call` row gives the dispatch site both a
    /// call-entry edge into the resolved target and a return edge back out of it. Neither
    /// exists in `call`, which the fixpoint never extends, so without these the flow cannot
    /// cross the site at all unless the callee's summary happens to describe it.
    #[test]
    fn a_resolved_call_gives_the_site_entry_and_return_edges() {
        let caller = FunctionId::new(0);
        let target = FunctionId::new(1);
        let insn = InsnId::new(7);
        let site = PackedInsnSiteId::try_from_parts(caller, insn).unwrap();
        let in_formal = FormalIndex::new(0);
        let ret_formal = FormalIndex::new(-1);
        let arg_var =
            FlowVariable::call_arg_packed(PackedCallArg::try_from_parts(insn, in_formal).unwrap());
        let in_var = FlowVariable::formal_index(in_formal);
        let ret_var = FlowVariable::formal_index(ret_formal);

        let facts = QueryFacts {
            // Unconditional resolution, so the edges are plain `Step::Flow`.
            resolved_call: vec![(caller, insn, target, CallString::new())],
            formal_param: vec![
                (target, in_var, FormalType::ByVal),
                (target, ret_var, FormalType::ByVal),
            ],
            paths: vec![(Path::empty(),)],
            ..Default::default()
        };
        let graph = TaintSearchGraph::new(&facts);

        // Entry: the call-arg vertex at the dispatch site steps into the target's formal.
        let entry = graph.labeled_successors(&(caller, arg_var, Path::empty(), TaintLevel::Plain));
        assert!(
            entry.iter().any(|(n, e)| n.0 == target
                && n.1 == in_var
                && matches!(e, Step::Flow(FlowEdge::Call(s)) if *s == site)),
            "a resolved site must enter its target; got {entry:?}"
        );

        // Return: the target's out-formal steps back to the call-arg vertex at the same site.
        let exit = graph.labeled_successors(&(target, ret_var, Path::empty(), TaintLevel::Plain));
        assert!(
            exit.iter().any(|(n, e)| n.0 == caller
                && matches!(e, Step::Flow(FlowEdge::Return(s)) if *s == site)),
            "a resolved site must carry a return out of its target; got {exit:?}"
        );
    }

    /// The load-time subsumption: an unconditional resolution of a `(site, target)` pair makes
    /// every conditional row for that pair redundant, so the search does not pay a context for
    /// an edge it could take anyway. Rows for *other* pairs are untouched.
    #[test]
    fn an_unconditional_resolution_subsumes_the_conditional_ones() {
        let s = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let f = FunctionId::new(0);
        let insn = InsnId::new(1);
        let dominated = FunctionId::new(2);
        let conditional_only = FunctionId::new(3);
        let cs1 = CallString::intern(&[s(9, 9)]);
        let cs2 = CallString::intern(&[s(8, 8)]);

        let rows = vec![
            (f, insn, dominated, cs1),
            (f, insn, dominated, CallString::new()),
            (f, insn, dominated, cs2),
            // A different target at the same site, with no unconditional row of its own.
            (f, insn, conditional_only, cs1),
        ];
        let kept = subsume_resolved_calls(&rows);
        assert_eq!(
            kept,
            vec![
                (f, insn, dominated, CallString::new()),
                (f, insn, conditional_only, cs1),
            ],
            "only the unconditional row survives for the dominated pair, and the \
             conditional-only pair is untouched"
        );
    }
}
