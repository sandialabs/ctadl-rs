use rustc_graphviz as dot;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::facts::{FlowVariable, FunctionId, Path};

/// Numbers each distinct vertex in first-seen order, yielding the ordinals the graphs use as
/// DOT node identifiers.
fn number_vertices<N: Ord>(vertices: impl IntoIterator<Item = N>) -> BTreeMap<N, usize> {
    let mut ids = BTreeMap::new();
    for v in vertices {
        let next = ids.len();
        ids.entry(v).or_insert(next);
    }
    ids
}

/// An index-graph vertex: `(function, variable, path)`. The variable
/// and path together are an access path.
pub type IndexVertex = (FunctionId, FlowVariable, Path);

/// One row of the `assign` relation: the assignment `dst = src` inside `function`, drawn as the
/// edge `src -> dst`.
pub type AssignRow = (FunctionId, FlowVariable, Path, FlowVariable, Path);

/// Orders two index- or taint-graph vertices by what they denote, so a rendering is
/// reproducible.
pub fn index_vertex_cmp(a: &IndexVertex, b: &IndexVertex) -> Ordering {
    a.0.cmp(&b.0)
        .then_with(|| a.1.content_cmp(&b.1))
        .then_with(|| a.2.content_cmp(&b.2))
}

/// Orders two `assign` rows by their drawn edge: destination vertex first, then source.
pub fn index_edge_cmp(a: &AssignRow, b: &AssignRow) -> Ordering {
    index_vertex_cmp(&(a.0, a.1, a.2), &(b.0, b.1, b.2))
        .then_with(|| index_vertex_cmp(&(a.0, a.3, a.4), &(b.0, b.3, b.4)))
}

pub struct IndexGraphViz<'a> {
    nodes: Vec<IndexVertex>,
    edges: &'a [AssignRow],
    id_map: &'a crate::facts::IdMap,
    /// Collision-free DOT ids, covering the nodes and every edge endpoint.
    node_ids: BTreeMap<IndexVertex, usize>,
}

impl<'a> IndexGraphViz<'a> {
    /// Renders a vertex as `function(<name>)\n<variable><path>`.
    ///
    /// The path is part of both the label and the node id, so vertices are
    /// keyed by the triple `(function, variable, path)` — `local(t)` and
    /// `local(t).x` are distinct nodes. Note that [`Path::empty`] renders as the
    /// empty string: a label with no `.field` suffix denotes the *empty* path, not
    /// a path that was omitted from the rendering.
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
        dot::Id::new(format!("n{}", self.node_ids[n])).unwrap()
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

/// Renders the index (assign-like) graph as Graphviz DOT.
///
/// Nodes come out in [`index_vertex_cmp`] order. Edges come out in slice order.
pub fn render_index_graph<W: Write>(
    assign_like: &[AssignRow],
    id_map: &crate::facts::IdMap,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut nodes = BTreeSet::new();
    for (func_id, dst_var, dst_path, src_var, src_path) in assign_like {
        nodes.insert((*func_id, *dst_var, *dst_path));
        nodes.insert((*func_id, *src_var, *src_path));
    }
    let mut nodes: Vec<IndexVertex> = nodes.into_iter().collect();
    nodes.sort_unstable_by(index_vertex_cmp);
    let node_ids = number_vertices(
        nodes.iter().copied().chain(
            assign_like
                .iter()
                .flat_map(|e| [(e.0, e.1, e.2), (e.0, e.3, e.4)]),
        ),
    );
    let graph = IndexGraphViz {
        nodes,
        edges: assign_like,
        id_map,
        node_ids,
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

    /// Extracts the DOT identifier from each node declaration line, in order.
    fn declared_node_ids(dot: &str) -> Vec<&str> {
        dot.lines()
            .filter(|l| l.contains("label=") && !l.contains("->"))
            .filter_map(|l| l.trim().split('[').next())
            .collect()
    }

    /// Two vertices whose labels differ only in punctuation must still get distinct DOT
    /// ids. `.a.b` and `.a_b` are distinct paths, but sanitizing the label into
    /// DOT's identifier alphabet maps both to `local_t__a_b`; ids derived that way would
    /// make graphviz draw them as one node owning both incident edges.
    #[test]
    fn node_ids_survive_labels_that_differ_only_in_punctuation() {
        let mut ids = IdMap::new();
        let f = ids.get_or_add_function(Function::from(Str::from("foo")));
        let (t, src) = (local("t"), local("src"));
        let (dots, under) = (Path::parse(".a.b").unwrap(), Path::parse(".a_b").unwrap());
        assert_ne!(dots, under, "the two paths must denote distinct vertices");

        let assign_like = vec![
            (f, t, dots, src, Path::empty()),
            (f, t, under, src, Path::empty()),
        ];
        let mut out = Vec::new();
        render_index_graph(&assign_like, &ids, &mut out).unwrap();
        let dot = String::from_utf8(out).unwrap();

        let node_ids = declared_node_ids(&dot);
        assert_eq!(node_ids.len(), 3, "expected 3 declared nodes\n{dot}");
        let distinct: BTreeSet<_> = node_ids.iter().collect();
        assert_eq!(distinct.len(), 3, "node ids collided: {node_ids:?}\n{dot}");
    }
}
