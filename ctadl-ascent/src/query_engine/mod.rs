//! Taint analysis on the index graph.

// TODO when converting to graphs, add these rules:
//
// taint_edge(infunc, insn, a.direction.clone(), v1, p13.clone(), v2, p23.clone()),
// taint_edge(infunc, insn, a.direction.clone(), v1.clone(), p2.clone(), v2, p2.clone()),
// taint_edge(infunc, insn, a.direction.clone(), formal_var.clone(), p2.clone(), v2, p2.clone()),

use std::path;

use ascent::ascent;
use derive_builder::Builder;
use packed_struct::prelude::*;

use crate::error::Error;
use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, IdMap, InsnId, InsnSiteId,
    Label, PackedInsnSiteId, Path, TaintDirection, TaintEndpoint, TaintState, isout,
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
    pub infunc: FunctionId,
    pub vertex: FlowVertex,
    pub label: Label,
    pub direction: TaintDirection,
}

impl QueryEndpoint {
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
        }
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
    pub assign: Vec<(FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path)>,
    #[builder(default)]
    pub paths: Vec<(Path,)>,
    /// Sources and sinks for query. Data flow is followed forward from sources and backward from
    /// sinks
    #[builder(default)]
    pub endpoints: Vec<(QueryEndpoint,)>,
}

#[derive(Default, Debug, Clone)]
pub struct QueryResult {
    pub taint: Vec<(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)>,
}

impl QueryResult {
    pub fn new() -> Self {
        Self {
            taint: Default::default(),
        }
    }

    pub fn try_save<P: AsRef<path::Path>>(self, dir: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        taint::try_save(&dir, self.taint)?;
        Ok(())
    }

    /// Load the query results from disk. Loads both forward and backward taint
    pub fn try_load<P: AsRef<path::Path>>(dir: P) -> Result<QueryResult, Error> {
        use crate::facts::schema::*;
        let taint = taint::try_load(&dir)?;
        Ok(QueryResult { taint })
    }
}

impl std::fmt::Display for QueryResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (func_id, taint_state, flow_var, path, endpoint) in &self.taint {
            let var_path_str = {
                let var_str = match flow_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", flow_var),
                };
                format!("{}{}", var_str, path.to_dot_string())
            };

            let endpoint_vertex_str = {
                let FlowVertex(endpoint_var, endpoint_path) = &endpoint.vertex;
                let var_str = match endpoint_var {
                    FlowVariable::Local(name) => name.to_string(),
                    _ => format!("{}", endpoint_var),
                };
                format!("{}{}", var_str, endpoint_path.to_dot_string())
            };

            let taint_state_str = format!("{:?}", taint_state);
            writeln!(
                f,
                "[{}] {:<10} {} <-- {}, {} @ {}:{}",
                func_id.id,
                taint_state_str,
                var_path_str,
                endpoint.direction,
                endpoint.label,
                endpoint.infunc.id,
                endpoint_vertex_str
            )?;
        }
        Ok(())
    }
}

/// Taint analysis datalog rules.
///
/// Runs taint analysis given the set of query facts, which include relations from the 'index'
/// phase and a set of taint sources. Returns a relation containing the set of vertices tainted by
/// each taint source.
pub fn taint_analysis(facts: QueryFacts) -> QueryResult {
    ascent! {
        struct QueryEngine;
        macro produce_taint($df:expr, $dts:expr, $dv:expr, $dp:expr, $a:expr, $sf:expr, $sv:expr, $sp:expr) {
            taint($df, $dts, $dv, $dp, $a)
        }
        include_source!(crate::query_engine::ascent_code::taint_analysis_rules);
    }

    let mut engine = QueryEngine {
        formal_param: facts.formal_param,
        call: facts.call,
        assign_like: facts.assign,
        paths: facts.paths,
        sources: facts.endpoints,
        ..Default::default()
    };
    engine.run();

    log::trace!(
        "query result: {}",
        DisplayTaint {
            taint: &engine.taint
        }
    );
    QueryResult {
        taint: engine.taint,
    }
}

pub mod ascent_code {
    ascent::ascent_source! {
            taint_analysis_rules:

        relation formal_param(FunctionId, FlowVariable, FormalType);
        relation call(PackedInsnSiteId, FunctionId);
        relation assign_like(FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path);
        relation paths(Path);
        relation sources(QueryEndpoint);

        relation alias_of_field(FunctionId, FlowVariable, FlowVariable, Path);
        relation taint(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint);

        // Initialize taint with source
        taint(infunc, TaintState::Free, v.clone(), p.clone(), s) <--
            sources(s),
            let QueryEndpoint { infunc, vertex, label, direction } = s,
            let FlowVertex(v, p) = vertex;

        // Propagate taint locally onto fields
        produce_taint!(infunc, ts, v1.clone(), p13.clone(), a.clone(), infunc, v2.clone(), p23.clone()) <--
            taint(infunc, ts, v2, p23, a),
            if a.direction == TaintDirection::Forward,
            assign_like(infunc, _, v1, p1, v2, p2),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        produce_taint!(infunc, ts, v1.clone(), p13.clone(), a.clone(), infunc, v2.clone(), p23.clone()) <--
            taint(infunc, ts, v2, p23, a),
            if a.direction == TaintDirection::Backward,
            assign_like(infunc, _, v2, p2, v1, p1),
            if let Some(p13) = p23.substitute_prefix(p2, p1),
            paths(p13.clone());

        // Formal-to-actual (Return in forward mode, Call in backward mode).
        produce_taint!(func_id, TaintState::Free, v1.clone(), p2.clone(), a.clone(), infunc, v2.clone(), p2.clone()) <--
            taint(infunc, TaintState::Free, v2, p2, a),
            formal_param(infunc, v2, formal_ty),
            if let FlowVariable::Formal(n2) = v2,
            if (a.direction == TaintDirection::Forward && isout(n2, *formal_ty, p2)) ||
                (a.direction == TaintDirection::Backward /* && isin(n2.0) */),
            call(site_id, infunc),
            let InsnSiteId {func_id, insn_id: _} = InsnSiteId::unpack_from_slice(&**site_id).unwrap(),
            let v1 = FlowVariable::CallArg { id: site_id.clone(), formal: n2.clone() };

        // Actual-to-formal (Call in forward mode, Return in backward mode).
        produce_taint!(func, TaintState::Restricted, formal_var.clone(), p2.clone(), a.clone(), infunc, v2.clone(), p2.clone()) <--
            taint(infunc, _, v2, p2, a),
            if let FlowVariable::CallArg { id, formal } = v2,
            call(id, func),
            let formal_var = FlowVariable::Formal(formal.clone()),
            formal_param(func, formal_var, formal_ty),
            if a.direction == TaintDirection::Forward /* && isin(formal)) */ ||
                (a.direction == TaintDirection::Backward && isout(formal, *formal_ty, p2));

        alias_of_field(infunc, x.clone(), a.clone(), p.clone()) <--
            assign_like(infunc, _, x, Path::empty(), a, p),
            if !p.is_empty();
        alias_of_field(infunc, y.clone(), a.clone(), p.clone()) <--
            alias_of_field(infunc, x, a, p),
            assign_like(infunc, _, y, Path::empty(), x, Path::empty());

        // Propagates taint on a variable into its alias.
        produce_taint!(infunc, st, v1.clone(), p.clone(), a.clone(), infunc, v2.clone(), Path::empty()) <--
            taint(infunc, st, v2, Path::empty(), a),
            if a.direction == TaintDirection::Forward,
            alias_of_field(infunc, v2, v1, p);

        produce_taint!(infunc, st, v1.clone(), p12.clone(), a.clone(), infunc, v2.clone(), p2.clone()) <--
            taint(infunc, st, v2, p2, a),
            if a.direction == TaintDirection::Forward,
            alias_of_field(infunc, v2, v1, p1),
            let p12 = p1.concat(p2),
            paths(p12.clone());

        // Backward alias propagation
        produce_taint!(infunc, st, v1.clone(), Path::empty(), a.clone(), infunc, v2.clone(), p.clone()) <--
            taint(infunc, st, v2, p, a),
            if a.direction == TaintDirection::Backward,
            alias_of_field(infunc, v1, v2, p);

        produce_taint!(infunc, st, v2.clone(), p2.clone(), a.clone(), infunc, v1.clone(), p12.clone()) <--
            taint(infunc, st, v1, p12, a),
            if a.direction == TaintDirection::Backward,
            alias_of_field(infunc, v1, v2, p1),
            if let Some(p2) = p12.substitute_prefix(p1, &Path::empty()),
            paths(p2.clone());
    }
}

pub mod formatter;
pub mod graphviz;

struct DisplayTaint<'a> {
    taint: &'a [(FunctionId, TaintState, FlowVariable, Path, QueryEndpoint)],
}

impl<'a> std::fmt::Display for DisplayTaint<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Taint output")?;
        for (func_id, ts, var, path, endpoint) in self.taint {
            // let InsnSiteId {
            //     func_id: site_func_id,
            //     insn_id: site_insn_id,
            // } = InsnSiteId::unpack_from_slice(&**site_id).unwrap();
            writeln!(
                f,
                "  {} {:?} {}{} <- {}",
                func_id.id,
                ts,
                var,
                path.to_dot_string(),
                endpoint,
            )?;
        }
        Ok(())
    }
}

impl std::fmt::Display for QueryEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let QueryEndpoint {
            label,
            direction,
            infunc,
            vertex,
        } = self;
        write!(
            f,
            "{label} {direction} {} {}{}",
            infunc.id,
            vertex.0,
            vertex.1.to_dot_string()
        )?;
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
