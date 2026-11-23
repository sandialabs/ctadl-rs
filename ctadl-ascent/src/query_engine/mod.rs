/*! Taint analysis on the index graph

Taint analysis is a matter of following direct intraprocedural flow and stitching edges
between calls and returns.

We want to enable some queries:

- Path queries. Finds a path from each source to each sink.
- Closure queries. Finds all the vertices/instructions tainted by each source or sink.

The query ([`search`]) is a demand-driven graph search: sources are partitioned by label,
and each label set runs one multi-start realizable-path search directly over the program
tables, with sinks as targets. Only the states a search reaches exist at all, and only what
the reporting consumes is materialized. It is the one query engine, measured against the
reference implementation of the IR's semantics (`ctadl_ir::semantics`) by
`tests/integration/reference_parity.rs`.
*/

use derive_builder::Builder;

use crate::facts::{
    FlowEdge, FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, IdMap, Label,
    PackedInsnSiteId, Path, TaintState,
};
use crate::index_engine::{func_name, vertex_str};

pub use crate::facts::{QueryEndpoint, QueryEndpointDisplay};

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

/// Every relation is in content order, never handle order: the formatter numbers graph nodes
/// and orders results by row order.
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
    /// instruction-level reporting: `(site, label, variable, path)`.
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
            let var_path_str = vertex_str(flow_var, path);

            let name = func_name(self.id_map, *func_id);

            let taint_state_str = format!("{:?}", taint_state);
            writeln!(
                f,
                "{}({}): {:<10} {} <-- {}",
                name,
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
    let mut root = x;
    while let Some(p) = parent.get(&root) {
        if *p == root {
            break;
        }
        root = *p;
    }
    // Path-compress everything on the way to the root.
    let mut cur = x;
    while cur != root {
        let next = parent.get(&cur).copied().unwrap_or(root);
        parent.insert(cur, root);
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
/// the union-find and so produce no rows. This is O(C) where an all-pairs empty-path
/// alias closure would be Θ(C²): instead of materializing every pair of a C-variable
/// copy group, we hand the taint engine one edge per non-representative member and let
/// taint equalize through the representative. SSA register moves form copy groups of
/// C≈10⁴ variables, where all pairs would be 10⁸ rows or more.
///
/// Copies that WRITE a formal are skipped, for the same reason the index's
/// [`crate::index_engine`] `compute_alias_of_formal` drops them: to avoid sharing taint
/// in both directions on everything that passes through a parameter slot.
pub(crate) fn compute_copy_alias(
    assign: &[(FunctionId, FlowVariable, Path, FlowVariable, Path)],
) -> Vec<(FunctionId, FlowVariable, FlowVariable)> {
    use std::collections::HashMap;
    let mut parent: HashMap<CopyKey, CopyKey> = HashMap::new();
    for (f, dst, dp, src, sp) in assign {
        if !dp.is_empty() || !sp.is_empty() {
            continue;
        }
        // A copy that writes a formal; see the doc above. Union-find is direction-blind, so
        // this edge has to be dropped at construction; there is no way to union "one way".
        if dst.as_formal().is_some() {
            continue;
        }
        let a: CopyKey = (*f, *dst);
        let b: CopyKey = (*f, *src);
        parent.entry(a).or_insert(a);
        parent.entry(b).or_insert(b);
        let ra = copy_find(&mut parent, a);
        let rb = copy_find(&mut parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let keys: Vec<CopyKey> = parent.keys().cloned().collect();
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        let root = copy_find(&mut parent, k);
        if root != k {
            // dst and src of a copy edge share a function, so k.0 == root.0.
            out.push((k.0, k.1, root.1));
        }
    }
    out
}

/// Runs the query-phase taint analysis: the demand-driven graph search
/// ([`search::taint_search`]). Sources are partitioned by label, and each label set runs one
/// multi-start realizable-path search directly over the program tables, with aliasing followed
/// directionally along the copy graph (see [`search::ObjectScope`]).
pub fn taint_analysis(facts: QueryFacts, id_map: Option<&IdMap>) -> QueryResult {
    search::taint_search(facts, id_map)
}

mod endpoints;
pub mod formatter;
pub mod search;

pub use endpoints::{BuiltEndpoints, build_query_endpoints};

struct DisplayTaint<'a> {
    taint: &'a [(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)],
    id_map: Option<&'a IdMap>,
}

impl<'a> std::fmt::Display for DisplayTaint<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Taint output")?;
        for (func_id, ts, var, path, endpoint) in self.taint {
            let name = func_name(self.id_map, *func_id);

            writeln!(
                f,
                "  {}({}) {:?} {}{} <- {}",
                name,
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
