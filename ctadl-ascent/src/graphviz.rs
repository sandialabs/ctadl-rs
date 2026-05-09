use rustc_graphviz as dot;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::facts::{FlowVariable, FunctionId, Path};

/// A taint-graph vertex: `(function, variable, access-path)`.
pub type TaintNode = (FunctionId, FlowVariable, Path);

/// A taint-graph edge already oriented in *data-flow* direction
/// (`src → dst`, i.e. taint flows from `src` into `dst`), tagged with which
/// cone(s) produced it.
pub type ConeEdge = (
    FunctionId,
    FlowVariable,
    Path,
    FunctionId,
    FlowVariable,
    Path,
    Cone,
);

/// Which taint cone a node or edge belongs to.
///
/// The taint analysis is meet-in-the-middle: a *forward* cone grows from each
/// source, a *backward* cone grows from each sink. A vertex/edge in `Both`
/// cones — a "meet" — is exactly one that lies on a complete source→sink path.
/// Reading reachability off the graph is then visual: follow the forward cone
/// out of a source diamond and see whether it reaches the meet (`Both`) region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cone {
    /// Reachable from a source (forward propagation only).
    Forward,
    /// Reaches a sink (backward propagation only).
    Backward,
    /// On a full source→sink path (seen in both cones).
    Both,
}

impl Cone {
    /// Combine two cone memberships: agreeing keeps the cone, disagreeing
    /// promotes to `Both` (a meet).
    pub fn join(self, other: Cone) -> Cone {
        if self == other { self } else { Cone::Both }
    }

    /// Fill color for a node in this cone (used unless overridden by a
    /// source/sink shape color).
    fn node_fill(self) -> &'static str {
        match self {
            Cone::Forward => "lightblue",
            Cone::Backward => "mistyrose",
            Cone::Both => "palegreen",
        }
    }
}

/// A wrapper around the (oriented) taint edges to implement Graphviz traits.
pub struct TaintGraphViz<'a> {
    /// Every node, mapped to its cone membership. Also the node set for the walk.
    node_cone: BTreeMap<TaintNode, Cone>,
    /// Data-flow-oriented, deduplicated edges.
    edges: &'a [ConeEdge],
    /// Declared source vertices (drawn as diamonds).
    sources: &'a BTreeSet<TaintNode>,
    /// Declared sink vertices (drawn as ellipses).
    sinks: &'a BTreeSet<TaintNode>,
    id_map: &'a crate::facts::IdMap,
}

impl<'a> TaintGraphViz<'a> {
    pub fn new(
        node_cone: BTreeMap<TaintNode, Cone>,
        edges: &'a [ConeEdge],
        sources: &'a BTreeSet<TaintNode>,
        sinks: &'a BTreeSet<TaintNode>,
        id_map: &'a crate::facts::IdMap,
    ) -> Self {
        Self {
            node_cone,
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
    type Node = TaintNode;
    type Edge = &'a ConeEdge;

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
        let mut s = self.node_to_string(&n.0, &n.1, &n.2);
        let ep = if self.sources.contains(n) {
            "\nSOURCE"
        } else if self.sinks.contains(n) {
            "\nSINK"
        } else {
            ""
        };
        s.push_str(ep);
        dot::LabelText::EscStr(s.into())
    }

    fn node_style(&self, _n: &Self::Node) -> dot::Style {
        // All nodes are filled; the color (set in `node_attrs`) carries the
        // cone / source / sink signal.
        dot::Style::Filled
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

    fn node_attrs(&'a self, n: &Self::Node) -> Vec<(Cow<'a, str>, dot::LabelText<'a>)> {
        // Source/sink shapes get their own fill so the endpoints stand out;
        // everything else is colored by cone membership (meet = palegreen).
        let color = if self.sources.contains(n) {
            "gold"
        } else if self.sinks.contains(n) {
            "orange"
        } else {
            self.node_cone
                .get(n)
                .map(|c| c.node_fill())
                .unwrap_or("white")
        };
        vec![("fillcolor".into(), dot::LabelText::label(color))]
    }

    fn edge_label(&self, _e: &Self::Edge) -> dot::LabelText<'a> {
        dot::LabelText::label("")
    }

    fn edge_attrs(&'a self, e: &Self::Edge) -> Vec<(Cow<'a, str>, dot::LabelText<'a>)> {
        // Color edges by cone: forward = blue, backward = red+dashed (it points
        // *against* data flow in discovery terms but is drawn data-flow here),
        // meet = bold dark green (an edge on a real source→sink path).
        match e.6 {
            Cone::Forward => vec![("color".into(), dot::LabelText::label("blue"))],
            Cone::Backward => vec![
                ("color".into(), dot::LabelText::label("red")),
                ("style".into(), dot::LabelText::label("dashed")),
            ],
            Cone::Both => vec![
                ("color".into(), dot::LabelText::label("darkgreen")),
                ("penwidth".into(), dot::LabelText::label("2.0")),
            ],
        }
    }
}

impl<'a> dot::GraphWalk<'a> for TaintGraphViz<'a> {
    type Node = TaintNode;
    type Edge = &'a ConeEdge;

    fn nodes(&'a self) -> dot::Nodes<'a, Self::Node> {
        self.node_cone.keys().cloned().collect()
    }

    fn edges(&'a self) -> dot::Edges<'a, Self::Edge> {
        self.edges.iter().collect()
    }

    // Edges are pre-oriented in data-flow direction, so the arrow goes
    // src → dst — the same orientation `find_path` walks (source → sink).
    fn source(&'a self, e: &Self::Edge) -> Self::Node {
        let (sf, sv, sp, _, _, _, _) = *e;
        (*sf, *sv, *sp)
    }

    fn target(&'a self, e: &Self::Edge) -> Self::Node {
        let (_, _, _, df, dv, dp, _) = *e;
        (*df, *dv, *dp)
    }
}

/// Renders the (cone-classified, data-flow-oriented) taint graph into Graphviz
/// DOT format. `node_cone` maps every node to its cone; `edges` must already be
/// oriented src→dst in data-flow direction.
pub fn render_taint_graph<W: Write>(
    node_cone: BTreeMap<TaintNode, Cone>,
    edges: &[ConeEdge],
    sources: &BTreeSet<TaintNode>,
    sinks: &BTreeSet<TaintNode>,
    id_map: &crate::facts::IdMap,
    writer: &mut W,
) -> std::io::Result<()> {
    let graph = TaintGraphViz::new(node_cone, edges, sources, sinks, id_map);
    dot::render(&graph, writer)
}

pub struct IndexGraphViz<'a> {
    nodes: Vec<(FunctionId, FlowVariable, Path)>,
    edges: &'a [(FunctionId, FlowVariable, Path, FlowVariable, Path)],
    id_map: &'a crate::facts::IdMap,
}

impl<'a> IndexGraphViz<'a> {
    fn node_to_string(&self, f: &FunctionId, v: &FlowVariable, p: &Path) -> String {
        let func_name = self
            .id_map
            .get_function(*f)
            .map(|f| f.to_string())
            .unwrap_or_else(|| format!("func_{}", f.id));
        format!("{}\\n{}{}", func_name, v, p.to_dot_string())
    }
}

impl<'a> dot::Labeller<'a> for IndexGraphViz<'a> {
    type Node = (FunctionId, FlowVariable, Path);
    type Edge = &'a (FunctionId, FlowVariable, Path, FlowVariable, Path);

    fn graph_id(&'a self) -> dot::Id<'a> {
        dot::Id::new("index_graph").unwrap()
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

    fn edge_label(&self, _e: &Self::Edge) -> dot::LabelText<'a> {
        dot::LabelText::label("")
    }
}

impl<'a> dot::GraphWalk<'a> for IndexGraphViz<'a> {
    type Node = (FunctionId, FlowVariable, Path);
    type Edge = &'a (FunctionId, FlowVariable, Path, FlowVariable, Path);

    fn nodes(&'a self) -> dot::Nodes<'a, Self::Node> {
        self.nodes.iter().cloned().collect()
    }

    fn edges(&'a self) -> dot::Edges<'a, Self::Edge> {
        self.edges.iter().collect()
    }

    fn source(&'a self, e: &Self::Edge) -> Self::Node {
        (e.0, e.3, e.4)
    }

    fn target(&'a self, e: &Self::Edge) -> Self::Node {
        (e.0, e.1, e.2)
    }
}

pub fn render_index_graph<W: Write>(
    assign_like: &[(FunctionId, FlowVariable, Path, FlowVariable, Path)],
    id_map: &crate::facts::IdMap,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut nodes = BTreeSet::new();
    for (func_id, dst_var, dst_path, src_var, src_path) in assign_like {
        nodes.insert((*func_id, *dst_var, *dst_path));
        nodes.insert((*func_id, *src_var, *src_path));
    }
    let graph = IndexGraphViz {
        nodes: nodes.into_iter().collect(),
        edges: assign_like,
        id_map,
    };
    dot::render(&graph, writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Function, IdMap, Str};

    /// Counts node declarations and edge declarations in rendered DOT output.
    ///
    /// rustc_graphviz emits one statement per line: an edge line contains the
    /// `->` operator, while a node line is any other statement carrying a
    /// `label=` attribute (the `digraph {`/`}` framing lines have neither).
    fn count_nodes_edges(dot: &str) -> (usize, usize) {
        let edges = dot.lines().filter(|l| l.contains("->")).count();
        let nodes = dot
            .lines()
            .filter(|l| l.contains("label=") && !l.contains("->"))
            .count();
        (nodes, edges)
    }

    fn local(name: &str) -> FlowVariable {
        FlowVariable::local(Str::from(name))
    }

    #[test]
    fn index_graph_node_and_edge_counts() {
        let mut ids = IdMap::new();
        let f = ids.get_or_add_function(Function::from(Str::from("foo")));

        // A chain a -> b -> c inside one function: tuples are
        // (func, dst_var, dst_path, src_var, src_path), so each row is the
        // assignment `dst = src`, drawn as an edge src -> dst.
        let (a, b, c) = (local("a"), local("b"), local("c"));
        let assign_like = vec![
            (f, b, Path::empty(), a, Path::empty()),
            (f, c, Path::empty(), b, Path::empty()),
        ];

        let mut out = Vec::new();
        render_index_graph(&assign_like, &ids, &mut out).unwrap();
        let dot = String::from_utf8(out).unwrap();

        let (nodes, edges) = count_nodes_edges(&dot);
        // Three distinct vertices (a, b, c), two assignment edges.
        assert_eq!(nodes, 3, "index graph nodes\n{dot}");
        assert_eq!(edges, 2, "index graph edges\n{dot}");
        assert!(dot.contains("digraph index_graph"));
    }

    #[test]
    fn index_graph_dedups_shared_vertices() {
        let mut ids = IdMap::new();
        let f = ids.get_or_add_function(Function::from(Str::from("foo")));

        // Two edges sharing the destination `b`: a -> b and c -> b. The shared
        // vertex must be emitted once, so 3 nodes / 2 edges (not 4 / 2).
        let (a, b, c) = (local("a"), local("b"), local("c"));
        let assign_like = vec![
            (f, b, Path::empty(), a, Path::empty()),
            (f, b, Path::empty(), c, Path::empty()),
        ];

        let mut out = Vec::new();
        render_index_graph(&assign_like, &ids, &mut out).unwrap();
        let dot = String::from_utf8(out).unwrap();

        let (nodes, edges) = count_nodes_edges(&dot);
        assert_eq!(nodes, 3, "index graph nodes\n{dot}");
        assert_eq!(edges, 2, "index graph edges\n{dot}");
    }

    #[test]
    fn taint_graph_node_and_edge_counts() {
        let mut ids = IdMap::new();
        let f = ids.get_or_add_function(Function::from(Str::from("foo")));

        // Meet-in-the-middle shape: source a (forward) -> meet b (both) ->
        // sink c (backward). Edge tuples are
        // (src_func, src_var, src_path, dst_func, dst_var, dst_path, cone).
        let (a, b, c) = (local("a"), local("b"), local("c"));
        let na = (f, a, Path::empty());
        let nb = (f, b, Path::empty());
        let nc = (f, c, Path::empty());

        let mut node_cone = BTreeMap::new();
        node_cone.insert(na, Cone::Forward);
        node_cone.insert(nb, Cone::Both);
        node_cone.insert(nc, Cone::Backward);

        let edges: Vec<ConeEdge> = vec![
            (f, a, Path::empty(), f, b, Path::empty(), Cone::Forward),
            (f, b, Path::empty(), f, c, Path::empty(), Cone::Backward),
        ];

        let sources: BTreeSet<TaintNode> = [na].into_iter().collect();
        let sinks: BTreeSet<TaintNode> = [nc].into_iter().collect();

        let mut out = Vec::new();
        render_taint_graph(node_cone, &edges, &sources, &sinks, &ids, &mut out).unwrap();
        let dot = String::from_utf8(out).unwrap();

        let (nodes, edges_n) = count_nodes_edges(&dot);
        assert_eq!(nodes, 3, "taint graph nodes\n{dot}");
        assert_eq!(edges_n, 2, "taint graph edges\n{dot}");
        assert!(dot.contains("digraph taint_graph"));
        // Source is a diamond, sink an ellipse — sanity-check role shapes survive.
        assert!(dot.contains("diamond"), "expected a source diamond\n{dot}");
        assert!(dot.contains("ellipse"), "expected a sink ellipse\n{dot}");
    }
}
