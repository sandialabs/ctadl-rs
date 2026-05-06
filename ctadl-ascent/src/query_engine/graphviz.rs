use rustc_graphviz as dot;
use std::collections::BTreeSet;
use std::io::Write;

use crate::facts::{FlowVariable, FunctionId, Path};

/// A wrapper around taint edges to implement Graphviz traits.
pub struct TaintGraphViz<'a> {
    nodes: &'a [(FunctionId, FlowVariable, Path)],
    edges: &'a [(
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )],
    sources: &'a BTreeSet<(FunctionId, FlowVariable, Path)>,
    sinks: &'a BTreeSet<(FunctionId, FlowVariable, Path)>,
    id_map: &'a crate::facts::IdMap,
}

impl<'a> TaintGraphViz<'a> {
    pub fn new(
        nodes: &'a [(FunctionId, FlowVariable, Path)],
        edges: &'a [(
            FunctionId,
            FlowVariable,
            Path,
            FunctionId,
            FlowVariable,
            Path,
        )],
        sources: &'a BTreeSet<(FunctionId, FlowVariable, Path)>,
        sinks: &'a BTreeSet<(FunctionId, FlowVariable, Path)>,
        id_map: &'a crate::facts::IdMap,
    ) -> Self {
        Self {
            nodes,
            edges,
            sources,
            sinks,
            id_map,
        }
    }

    fn node_to_string(&self, f: &FunctionId, v: &FlowVariable, p: &Path) -> String {
        let func_name = self
            .id_map
            .get_function(*f)
            .map(|f| f.to_string())
            .unwrap_or_else(|| format!("func_{}", f.id));
        format!("{}\\n{}{}", func_name, v, p.to_dot_string())
    }
}

impl<'a> dot::Labeller<'a> for TaintGraphViz<'a> {
    type Node = (FunctionId, FlowVariable, Path);
    type Edge = &'a (
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    );

    fn graph_id(&'a self) -> dot::Id<'a> {
        dot::Id::new("taint_graph").unwrap()
    }

    fn node_id(&'a self, n: &Self::Node) -> dot::Id<'a> {
        let s = self.node_to_string(&n.0, &n.1, &n.2);
        let safe_id = s
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        dot::Id::new(format!("node_{}", safe_id)).unwrap()
    }

    fn node_label(&self, n: &Self::Node) -> dot::LabelText<'a> {
        dot::LabelText::EscStr(self.node_to_string(&n.0, &n.1, &n.2).into())
    }

    fn node_style(&self, n: &Self::Node) -> dot::Style {
        if self.sources.contains(n) || self.sinks.contains(n) {
            dot::Style::Filled
        } else {
            dot::Style::None
        }
    }

    fn node_shape(&'a self, n: &Self::Node) -> Option<dot::LabelText<'a>> {
        if self.sources.contains(n) {
            Some(dot::LabelText::LabelStr("diamond".into()))
        } else if self.sinks.contains(n) {
            Some(dot::LabelText::LabelStr("ellipse".into()))
        } else {
            Some(dot::LabelText::LabelStr("box".into()))
        }
    }

    fn edge_label(&self, _e: &Self::Edge) -> dot::LabelText<'a> {
        dot::LabelText::label("")
    }
}

impl<'a> dot::GraphWalk<'a> for TaintGraphViz<'a> {
    type Node = (FunctionId, FlowVariable, Path);
    type Edge = &'a (
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    );

    fn nodes(&'a self) -> dot::Nodes<'a, Self::Node> {
        self.nodes.iter().cloned().collect()
    }

    fn edges(&'a self) -> dot::Edges<'a, Self::Edge> {
        self.edges.iter().collect()
    }

    fn source(&'a self, e: &Self::Edge) -> Self::Node {
        let (sf, sv, sp, _, _, _) = *e;
        (*sf, sv.clone(), sp.clone())
    }

    fn target(&'a self, e: &Self::Edge) -> Self::Node {
        let (_, _, _, df, dv, dp) = *e;
        (*df, dv.clone(), dp.clone())
    }
}

/// Renders the taint edges into Graphviz DOT format.
pub fn render_taint_graph<W: Write>(
    nodes: &[(FunctionId, FlowVariable, Path)],
    edges: &[(
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    )],
    sources: &BTreeSet<(FunctionId, FlowVariable, Path)>,
    sinks: &BTreeSet<(FunctionId, FlowVariable, Path)>,
    id_map: &crate::facts::IdMap,
    writer: &mut W,
) -> std::io::Result<()> {
    let graph = TaintGraphViz::new(nodes, edges, sources, sinks, id_map);
    dot::render(&graph, writer)
}
