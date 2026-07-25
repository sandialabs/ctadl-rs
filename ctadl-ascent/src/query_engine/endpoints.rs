//! Stage 2 of source/sink matching: map name-based matched elements → `QueryEndpoint`s.
//!
//! Stage 1 (`models::json::ModelGeneratorIngest`) matches MIR elements — function
//! names, signatures, arity, regexes — and emits the name-based columnar
//! [`EndpointBatch`](crate::models::EndpointBatch)/[`EndpointRow`](crate::models::EndpointRow)
//! intermediate. Stage 2, here, consumes that intermediate together with the index facts
//! and resolves it into concrete [`QueryEndpoint`](super::QueryEndpoint)s for the query
//! engine.
//!
//! This is not a 1:1 conversion. In addition to resolving function names →
//! [`FunctionId`](crate::facts::FunctionId), it performs two index-dependent expansions:
//! - **call-site fan-out** — a function-anchored endpoint is fanned into one endpoint per
//!   matching call site (via [`QueryEndpoint::anchored_at_callsites`] /
//!   [`QueryEndpoint::anchored_at_callsites_filtered`]), needing `facts.call`;
//! - **sink wildcard expansion** — a wildcard sink port is expanded into the concrete
//!   access paths that live on the argument's copy class (via [`super::compute_copy_alias`]),
//!   needing `assign_like`.
//!
//! Consuming index facts here is therefore expected, not a smell.

use std::collections::{BTreeSet, HashMap};

use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::facts::{self, FlowVariable, FlowVertex, Label};
use crate::index_engine::IndexFacts;

/// The outcome of Stage 2: the resolved endpoints, the formals they register, and the
/// names that could not be resolved at all.
///
/// The last field exists because dropping an endpoint here is otherwise invisible: Stage 1
/// matched the name against the *model's* view of the program, but if the index does not
/// contain that function the endpoint silently disappears and the query quietly narrows.
/// Reporting the names lets `cli::query` emit the `CTADL0005` SARIF notification.
pub struct BuiltEndpoints {
    pub endpoints: Vec<(crate::query_engine::QueryEndpoint,)>,
    pub formals: Vec<(facts::FunctionId, facts::FlowVariable, facts::FormalType)>,
    /// Names Stage 1 matched that the index does not contain, deduplicated.
    pub unresolved_functions: BTreeSet<String>,
}

/// Turn the name-based model endpoint table (Stage 1) into resolved, expanded
/// `QueryEndpoint`s (Stage 2). See the module docs for the two expansions performed.
pub fn build_query_endpoints(
    batch: &crate::models::EndpointBatch,
    facts: &IndexFacts,
    idmap: &facts::IdMap,
    assign_like: &[(
        facts::FunctionId,
        FlowVariable,
        facts::Path,
        FlowVariable,
        facts::Path,
    )],
) -> BuiltEndpoints {
    use crate::models::FormalIndexTypeTag;
    let ap_map = batch.aps.build_ap_map();
    let func_num_params = facts.compute_arg_arity();

    // Field access paths that actually occur on each `(function, variable)` vertex
    // in the index graph, keyed by the vertex's *copy class* representative rather
    // than the vertex itself.
    //
    // A wildcard port is anchored at a call-arg vertex (`call-arg(site, i)`), but the
    // frontend records field paths on the local that the call passes, not on the
    // synthesized call-arg vertex: `t.headers = h; sink(t)` yields the vertex
    // `local(t).headers` plus the empty-path copy edges `call-arg ↔ local(t)`. Keying
    // by vertex therefore finds only the empty path and expands nothing. Copy classes
    // are exactly the sets of vertices holding the same value, so a path seen on any
    // member is a real path of the argument — and the copy chain between the actual
    // and the call-arg vertex may be several hops long, which is why this is the
    // union-find closure and not a one-hop lookup.
    let mut copy_rep: HashMap<(facts::FunctionId, FlowVariable), FlowVariable> = HashMap::new();
    for (func, member, rep) in crate::query_engine::compute_copy_alias(assign_like) {
        copy_rep.insert((func, member), rep);
    }
    // Resolves a vertex to its copy-class representative; a vertex in no copy edge is
    // its own representative.
    let rep_of = |func: facts::FunctionId, v: FlowVariable| -> FlowVariable {
        copy_rep.get(&(func, v)).copied().unwrap_or(v)
    };
    let mut vertex_paths: HashMap<(facts::FunctionId, FlowVariable), BTreeSet<facts::Path>> =
        HashMap::new();
    // All local (SSA-versioned) vertices that actually occur per function, used to resolve a
    // `Variable(name)` base index to an existing versioned vertex (see the `Local` arm below).
    let mut local_vars: HashMap<facts::FunctionId, BTreeSet<FlowVariable>> = HashMap::new();
    for (func, v1, p1, v2, p2) in assign_like {
        vertex_paths
            .entry((*func, rep_of(*func, *v1)))
            .or_default()
            .insert(*p1);
        vertex_paths
            .entry((*func, rep_of(*func, *v2)))
            .or_default()
            .insert(*p2);
        if v1.is_local() {
            local_vars.entry(*func).or_default().insert(*v1);
        }
        if v2.is_local() {
            local_vars.entry(*func).or_default().insert(*v2);
        }
    }

    let mut out_eps = Vec::new();
    let mut out_formals = Vec::new();
    let mut unresolved_functions = BTreeSet::new();
    for crate::models::EndpointRow {
        function: func_name,
        selector_ty,
        index: idx_opt,
        path_id,
        label: label_str,
        direction,
        wildcard,
        saturating,
        in_function,
        callsite_scoped,
        local_index,
    } in batch.iter_endpoints()
    {
        // Resolve function name → FunctionId; skip if not present. For a callsite endpoint
        // this is the *callee*.
        let infunc = match idmap.get_function_id(crate::facts::Function(func_name.into())) {
            Some(id) => id,
            None => {
                unresolved_functions.insert(func_name.to_string());
                continue;
            }
        };

        // For a callsite-scoped endpoint, resolve the caller filter (the containing
        // function). `None` in_function means "any caller"; an unresolvable name means no
        // callsite can match, so skip the endpoint entirely.
        let caller_filter = if callsite_scoped {
            match in_function {
                Some(name) => match idmap.get_function_id(crate::facts::Function(name.into())) {
                    Some(id) => Some(id),
                    None => {
                        unresolved_functions.insert(name.to_string());
                        continue;
                    }
                },
                None => None,
            }
        } else {
            None
        };

        // Map selector tag to variables.
        let vars = match selector_ty {
            FormalIndexTypeTag::Index => {
                let i16_val = idx_opt.expect("index missing");
                vec![FlowVariable::formal_index(i16_val.into())]
            }
            FormalIndexTypeTag::Return => {
                vec![FlowVariable::formal_index(RETURN_INDEX.into())]
            }
            FormalIndexTypeTag::Global => {
                vec![FlowVariable::formal_index(GLOBALS_INDEX.into())]
            }
            FormalIndexTypeTag::AnyArgument => func_num_params
                .get(&infunc)
                .map(|n| {
                    (0..*n)
                        .map(|i| FlowVariable::formal_index(i.into()))
                        .collect()
                })
                .unwrap_or_default(),
            // `Variable(name)` — the base `LocalIdx` was resolved to `local_index` in Stage 1.
            // Graph local vertices are SSA-versioned (`%L{idx}_{version}`); a bare `%L{idx}` is
            // not a vertex. Seed exactly ONE vertex: the lowest existing SSA version (version 0,
            // the incoming value, when present; otherwise the first real def). The min is over
            // the parsed integer suffix, not lexical order (`_10 < _2` lexically). Seeding the
            // first version treats the local as a source/sink at its incoming definition; a
            // source's taint then propagates to later versions along SSA def-use edges.
            FormalIndexTypeTag::Local => {
                let li = local_index.expect("local_index missing for Local selector");
                // Trailing '_' disambiguates `%L12_*` from `%L123_*`.
                let prefix = format!("%L{li}_");
                let seed = local_vars
                    .get(&infunc)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| {
                        let s = v.as_local()?;
                        let ver: u32 = s.as_str().strip_prefix(&prefix)?.parse().ok()?;
                        Some((ver, *v))
                    })
                    .min_by_key(|(ver, _)| *ver)
                    .map(|(_, v)| v);
                match seed {
                    Some(v) => vec![v],
                    None => {
                        // Stage 1 resolved this name against the *pre-optimization* program, but
                        // the graph is built after `eliminate_dead_temps` / `coalesce_copies` /
                        // `propagate_copies` have run. A local that was fused into another or
                        // dropped as dead has no `%L{li}_{version}` vertex left, as does one that
                        // never flows anywhere. Either way the endpoint silently disappears, so
                        // say so loudly rather than at debug level.
                        log::warn!(
                            "Variable selector: local %L{li} has no versioned vertex in \
                             '{func_name}', so this source/sink seeds nothing (the local may have \
                             been coalesced or eliminated before indexing, or it never \
                             participates in a flow)"
                        );
                        vec![]
                    }
                }
            }
        };

        let ap: facts::Path = ap_map[&path_id].iter().cloned().collect();

        // A wildcard sink port denotes the whole subtree beneath `ap` on the sink
        // call's argument: it matches every concrete access path, rooted at that
        // argument, that extends the port. Sinks seed *backward* taint and the
        // formatter resolves each endpoint vertex to a graph node by exact `Path`
        // equality, so the wildcard cannot be left abstract -- it is expanded below
        // into concrete paths.
        let expand_wildcard = wildcard && direction == facts::TaintDirection::Backward;

        // Build label and direction.
        let lbl = Label(label_str.into());

        for var in vars {
            // Register the model function's formal so taint can cross the call boundary
            // for flow-through, independent of how the endpoint is anchored below.
            if var.is_formal() {
                out_formals.push((infunc, var, facts::FormalType::ByRef));
            }
            // Anchor at the call sites of `infunc` (the modeled function) so flows that
            // share a formal but differ by call site stay distinct. Anchoring uses the
            // port path; wildcard expansion (if any) then happens per anchored vertex.
            let base = crate::query_engine::QueryEndpoint {
                infunc,
                vertex: FlowVertex(var, ap),
                label: lbl.clone(),
                direction,
                call_site: None,
                saturating,
            };
            let fanned = match var.as_formal() {
                Some(formal) if callsite_scoped => {
                    // Anchor only at the matching call sites (callee is `infunc`, caller is
                    // constrained by `caller_filter`); no function-anchored fallback.
                    base.anchored_at_callsites_filtered(formal, &facts.call, caller_filter)
                }
                Some(formal) => base.anchored_at_callsites(formal, &facts.call),
                None => vec![base],
            };
            for ep in fanned {
                if !expand_wildcard {
                    out_eps.push((ep,));
                    continue;
                }
                // Seed every concrete field path that lives on THIS call's argument
                // (i.e. on any member of its copy class) and extends the port, always
                // including the port path itself.
                let mut seeded_port = false;
                if let Some(paths) = vertex_paths.get(&(ep.infunc, rep_of(ep.infunc, ep.vertex.0)))
                {
                    for p in paths {
                        if !p.is_extension_of(&ap) {
                            continue;
                        }
                        if *p == ap {
                            seeded_port = true;
                        }
                        out_eps.push((crate::query_engine::QueryEndpoint {
                            vertex: FlowVertex(ep.vertex.0, *p),
                            ..ep.clone()
                        },));
                    }
                }
                if !seeded_port {
                    out_eps.push((ep,));
                }
            }
        }
    }
    BuiltEndpoints {
        endpoints: out_eps,
        formals: out_formals,
        unresolved_functions,
    }
}
