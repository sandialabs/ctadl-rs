//! A canonical result printed as compact JSON, its atoms replaced by their strings.
//!
//! Keys follow SARIF's names. A field that is `None`, and a collection the canonical form
//! reads as empty when absent, are left out; a property that is present but empty is kept,
//! since the canonical form tells the two apart. A step's `taintVertex`, `callee` and
//! `summarized` go under its `properties`, which is left out when it has none of them.

use sarif_canon::types::{
    Address, CanonCodeFlow, CanonLocation, CanonResult, CanonStep, CanonThreadFlow, Keyed,
    Physical, Props, Region,
};
use serde_json::{Map, Value};

use crate::Atoms;

/// `r` as one line of JSON.
pub(crate) fn result(r: &CanonResult<u32>, atoms: &Atoms) -> String {
    Value::Object(Json { atoms }.result(r)).to_string()
}

struct Json<'a> {
    atoms: &'a Atoms,
}

impl Json<'_> {
    fn atom(&self, a: u32) -> Value {
        Value::String(self.atoms.get(a).to_string())
    }

    fn atoms(&self, xs: &[u32]) -> Value {
        Value::Array(xs.iter().map(|&a| self.atom(a)).collect())
    }

    fn keyed(&self, xs: &[Keyed<u32>]) -> Value {
        Value::Array(
            xs.iter()
                .map(|k| Value::Array(vec![self.atom(k.key), self.atoms(&k.values)]))
                .collect(),
        )
    }

    fn result(&self, r: &CanonResult<u32>) -> Map<String, Value> {
        let mut m = Map::new();
        if let Some(rule) = r.rule {
            m.insert("ruleId".into(), self.atom(rule));
        }
        m.insert("kind".into(), self.atom(r.kind));
        m.insert("level".into(), self.atom(r.level));
        if !r.locations.is_empty() {
            let ls = r.locations.iter().map(|l| self.location(l)).collect();
            m.insert("locations".into(), Value::Array(ls));
        }
        if !r.code_flows.is_empty() {
            let cs = r.code_flows.iter().map(|c| self.code_flow(c)).collect();
            m.insert("codeFlows".into(), Value::Array(cs));
        }
        let props = self.props(&r.props);
        if !props.is_empty() {
            m.insert("properties".into(), Value::Object(props));
        }
        m
    }

    fn code_flow(&self, c: &CanonCodeFlow<u32>) -> Value {
        let ts = c.thread_flows.iter().map(|t| self.thread_flow(t)).collect();
        let mut m = Map::new();
        m.insert("threadFlows".into(), Value::Array(ts));
        Value::Object(m)
    }

    /// `{"locations": [steps]}`, or, cut to its endpoints, `{"endpoints": [first, last]}`.
    fn thread_flow(&self, t: &CanonThreadFlow<u32>) -> Value {
        let mut m = Map::new();
        match t {
            CanonThreadFlow::Steps(steps) => {
                let ss = steps.iter().map(|s| self.step(s)).collect();
                m.insert("locations".into(), Value::Array(ss));
            }
            CanonThreadFlow::Endpoints(first, last) => {
                let ends = vec![self.step(first), self.step(last)];
                m.insert("endpoints".into(), Value::Array(ends));
            }
        }
        Value::Object(m)
    }

    fn step(&self, s: &CanonStep<u32>) -> Value {
        let mut m = Map::new();
        if !s.kinds.is_empty() {
            m.insert("kinds".into(), self.atoms(&s.kinds));
        }
        if let Some(l) = &s.location {
            m.insert("location".into(), self.location(l));
        }
        let mut props = Map::new();
        if let Some(v) = s.vertex {
            props.insert("taintVertex".into(), self.atom(v));
        }
        if let Some(c) = s.callee {
            props.insert("callee".into(), self.atom(c));
        }
        if let Some(b) = s.summarized {
            props.insert("summarized".into(), Value::Bool(b));
        }
        if !props.is_empty() {
            m.insert("properties".into(), Value::Object(props));
        }
        Value::Object(m)
    }

    fn location(&self, l: &CanonLocation<u32>) -> Value {
        let mut m = Map::new();
        if let Some(p) = &l.physical {
            m.insert("physicalLocation".into(), self.physical(p));
        }
        if !l.logical.is_empty() {
            m.insert("logicalLocations".into(), self.atoms(&l.logical));
        }
        Value::Object(m)
    }

    fn physical(&self, p: &Physical<u32>) -> Value {
        let mut m = Map::new();
        let mut artifact = Map::new();
        if let Some(base) = p.base {
            artifact.insert("uriBaseId".into(), self.atom(base));
        }
        if let Some(uri) = p.uri {
            artifact.insert("uri".into(), self.atom(uri));
        }
        if !artifact.is_empty() {
            m.insert("artifactLocation".into(), Value::Object(artifact));
        }
        let region = region(&p.region);
        if !region.is_empty() {
            m.insert("region".into(), Value::Object(region));
        }
        if let Some(a) = &p.address {
            m.insert("address".into(), Value::Object(self.address(a)));
        }
        Value::Object(m)
    }

    fn address(&self, a: &Address<u32>) -> Map<String, Value> {
        let mut m = Map::new();
        number(&mut m, "absoluteAddress", a.absolute);
        number(&mut m, "relativeAddress", a.relative);
        if let Some(kind) = a.kind {
            m.insert("kind".into(), self.atom(kind));
        }
        m
    }

    fn props(&self, p: &Props<u32>) -> Map<String, Value> {
        let mut m = Map::new();
        let mut put = |key: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(key.into(), v);
            }
        };
        put("taintLabels", p.labels.as_deref().map(|xs| self.atoms(xs)));
        put("taintVertex", p.vertex.map(|a| self.atom(a)));
        put(
            "taintVertices",
            p.vertices.as_deref().map(|xs| self.keyed(xs)),
        );
        put(
            "sourceFunctions",
            p.source_fns.as_deref().map(|xs| self.atoms(xs)),
        );
        put(
            "sinkFunctions",
            p.sink_fns.as_deref().map(|xs| self.atoms(xs)),
        );
        put(
            "taintedFormals",
            p.tainted_formals.as_deref().map(|xs| self.keyed(xs)),
        );
        put("import", p.import.map(|a| self.atom(a)));
        m
    }
}

fn region(r: &Region) -> Map<String, Value> {
    let mut m = Map::new();
    number(&mut m, "startLine", r.start_line);
    number(&mut m, "startColumn", r.start_column);
    number(&mut m, "endLine", r.end_line);
    number(&mut m, "endColumn", r.end_column);
    number(&mut m, "byteOffset", r.byte_offset);
    number(&mut m, "byteLength", r.byte_length);
    m
}

fn number(m: &mut Map<String, Value>, key: &str, n: Option<i64>) {
    if let Some(n) = n {
        m.insert(key.into(), n.into());
    }
}
