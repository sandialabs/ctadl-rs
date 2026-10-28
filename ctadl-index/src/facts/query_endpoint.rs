//! [`QueryEndpoint`], a source or sink of a query. The query engine names it as
//! `query_engine::QueryEndpoint`; it lives here because the facts schema stores it.

use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FunctionId, GLOBALS_INDEX, IdMap, InsnSiteId, Label,
    PackedCallArg, PackedInsnSiteId, TaintDirection, TaintEndpoint,
};
use crate::index_engine::func_name;

/// A [`TaintEndpoint`] keyed by [`FunctionId`], plus the call site it is anchored at and
/// whether it saturates.
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
    /// Source-only: when `true`, this source is *saturating* — the seeded vertex is tainted
    /// and reading any subfield/offset off it is also tainted (recursively). Declared per
    /// source in the model (`"saturating": true`), defaulting to `false`. Purely internal to
    /// the search graph: it selects the seed's [`TaintLevel`](crate::facts::TaintLevel) and is
    /// never emitted downstream.
    pub saturating: bool,
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
            // `TaintEndpoint` does not carry the saturating flag; it is a search-graph
            // concern re-derived from the model, so default it here.
            saturating: false,
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
        if *formal == GLOBALS_INDEX {
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
                vertex: FlowVertex::new(FlowVariable::call_arg_packed(call_arg), self.vertex.1),
                label: self.label.clone(),
                direction: self.direction,
                call_site: Some(*site),
                saturating: self.saturating,
            });
        }
        if out.is_empty() { vec![self] } else { out }
    }

    /// Like [`anchored_at_callsites`], but for endpoints that denote a *specific set of call
    /// sites* rather than a whole function. `self.infunc` is the **callee**; a call site
    /// matches when it targets that callee and — when `caller_filter` is `Some` — sits inside
    /// that caller function. Unlike [`anchored_at_callsites`], there is no function-anchored
    /// fallback: if no call site matches, the result is empty (a callsite endpoint that names
    /// no real call site seeds nothing). The globals pseudo-formal cannot be anchored at a
    /// call site and yields an empty result.
    ///
    /// [`anchored_at_callsites`]: Self::anchored_at_callsites
    pub fn anchored_at_callsites_filtered(
        self,
        formal: FormalIndex,
        call: &[(PackedInsnSiteId, FunctionId)],
        caller_filter: Option<FunctionId>,
    ) -> Vec<Self> {
        if *formal == GLOBALS_INDEX {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (site, callee) in call {
            if *callee != self.infunc {
                continue;
            }
            let Ok(insn_site) = InsnSiteId::try_from(site) else {
                continue;
            };
            if let Some(caller) = caller_filter
                && insn_site.func_id != caller
            {
                continue;
            }
            let Ok(call_arg) = PackedCallArg::try_from_parts(insn_site.insn_id, formal) else {
                continue;
            };
            out.push(QueryEndpoint {
                infunc: insn_site.func_id,
                vertex: FlowVertex::new(FlowVariable::call_arg_packed(call_arg), self.vertex.1),
                label: self.label.clone(),
                direction: self.direction,
                call_site: Some(*site),
                saturating: self.saturating,
            });
        }
        out
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
            saturating: _,
        } = self.endpoint;

        let name = func_name(self.id_map, *infunc);

        write!(
            f,
            "{label} {direction} {name}({}) {}{}",
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
