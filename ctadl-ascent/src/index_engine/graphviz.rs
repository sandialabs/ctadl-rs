use rustc_graphviz as dot;
use std::io::Write;

use crate::facts::{FlowVariable, FunctionId, Heap, IdMap, Path};

/// A wrapper around pointer analysis relations to implement Graphviz traits.
pub struct ObjectGraphViz<'a> {
    vtx_points_to: &'a [(FunctionId, FlowVariable, Path, Heap)],
    fld_points_to: &'a [(FunctionId, Heap, Path, Heap)],
    id_map: &'a IdMap,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub enum ObjectNode {
    Vertex(FunctionId, FlowVariable, Path),
    Heap(FunctionId, Heap),
    Field(FunctionId, Heap, Path),
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ObjectEdge {
    PointsTo { from: ObjectNode, to: ObjectNode },
    HasField { from: ObjectNode, to: ObjectNode },
}

impl<'a> ObjectGraphViz<'a> {
    pub fn new(
        vtx_points_to: &'a [(FunctionId, FlowVariable, Path, Heap)],
        fld_points_to: &'a [(FunctionId, Heap, Path, Heap)],
        id_map: &'a IdMap,
    ) -> Self {
        Self {
            vtx_points_to,
            fld_points_to,
            id_map,
        }
    }

    fn func_name(&self, f: FunctionId) -> String {
        self.id_map
            .get_function(f)
            .map(|f| f.to_string())
            .unwrap_or_else(|| format!("func_{}", f.id))
    }

    fn node_to_label(&self, n: &ObjectNode) -> String {
        match n {
            ObjectNode::Vertex(f, v, p) => {
                format!("{}\\n{}{}", self.func_name(*f), v, p.to_dot_string())
            }
            ObjectNode::Heap(f, h) => {
                format!(
                    "{}\\nHeap@{}{}",
                    self.func_name(*f),
                    h.formal_index,
                    h.path.to_dot_string()
                )
            }
            ObjectNode::Field(_f, h, p) => {
                format!(
                    "Heap@{}{}{}",
                    h.formal_index,
                    h.path.to_dot_string(),
                    p.to_dot_string()
                )
            }
        }
    }
}

impl<'a> dot::Labeller<'a> for ObjectGraphViz<'a> {
    type Node = ObjectNode;
    type Edge = ObjectEdge;

    fn graph_id(&'a self) -> dot::Id<'a> {
        dot::Id::new("object_graph").unwrap()
    }

    fn node_id(&'a self, n: &Self::Node) -> dot::Id<'a> {
        let s = match n {
            ObjectNode::Vertex(f, v, p) => format!("v_{}_{}_{}", f.id, v, p.to_dot_string()),
            ObjectNode::Heap(f, h) => {
                format!("h_{}_{}_{}", f.id, h.formal_index, h.path.to_dot_string())
            }
            ObjectNode::Field(f, h, p) => format!(
                "f_{}_{}_{}_{}",
                f.id,
                h.formal_index,
                h.path.to_dot_string(),
                p.to_dot_string()
            ),
        };
        let safe_id = s
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        dot::Id::new(format!("node_{}", safe_id)).unwrap()
    }

    fn node_label(&self, n: &Self::Node) -> dot::LabelText<'a> {
        dot::LabelText::EscStr(self.node_to_label(n).into())
    }

    fn node_style(&self, n: &Self::Node) -> dot::Style {
        match n {
            ObjectNode::Heap(_, _) => dot::Style::Filled,
            _ => dot::Style::None,
        }
    }

    fn node_shape(&'a self, n: &Self::Node) -> Option<dot::LabelText<'a>> {
        match n {
            ObjectNode::Heap(_, _) => Some(dot::LabelText::LabelStr("box".into())),
            ObjectNode::Vertex(_, _, _) => Some(dot::LabelText::LabelStr("ellipse".into())),
            ObjectNode::Field(_, _, _) => Some(dot::LabelText::LabelStr("plaintext".into())),
        }
    }

    fn edge_style(&self, e: &Self::Edge) -> dot::Style {
        match e {
            ObjectEdge::HasField { .. } => dot::Style::Dashed,
            ObjectEdge::PointsTo { .. } => dot::Style::None,
        }
    }
}

impl<'a> dot::GraphWalk<'a> for ObjectGraphViz<'a> {
    type Node = ObjectNode;
    type Edge = ObjectEdge;

    fn nodes(&'a self) -> dot::Nodes<'a, Self::Node> {
        let mut nodes = std::collections::BTreeSet::new();
        for (f, v, p, h) in self.vtx_points_to {
            nodes.insert(ObjectNode::Vertex(*f, v.clone(), p.clone()));
            nodes.insert(ObjectNode::Heap(*f, h.clone()));
            if !p.is_empty() {
                nodes.insert(ObjectNode::Vertex(*f, v.clone(), Path::empty()));
            }
        }
        for (f, base_h, fld_p, h) in self.fld_points_to {
            nodes.insert(ObjectNode::Heap(*f, base_h.clone()));
            nodes.insert(ObjectNode::Field(*f, base_h.clone(), fld_p.clone()));
            nodes.insert(ObjectNode::Heap(*f, h.clone()));
        }
        nodes.into_iter().collect()
    }

    fn edges(&'a self) -> dot::Edges<'a, Self::Edge> {
        let mut edges = Vec::new();
        for (f, v, p, h) in self.vtx_points_to {
            let from = ObjectNode::Vertex(*f, v.clone(), p.clone());
            let to = ObjectNode::Heap(*f, h.clone());
            edges.push(ObjectEdge::PointsTo { from, to });

            if !p.is_empty() {
                let base = ObjectNode::Vertex(*f, v.clone(), Path::empty());
                let field = ObjectNode::Vertex(*f, v.clone(), p.clone());
                edges.push(ObjectEdge::HasField {
                    from: base,
                    to: field,
                });
            }
        }
        for (f, base_h, fld_p, h) in self.fld_points_to {
            let base = ObjectNode::Heap(*f, base_h.clone());
            let field = ObjectNode::Field(*f, base_h.clone(), fld_p.clone());
            let to = ObjectNode::Heap(*f, h.clone());
            edges.push(ObjectEdge::HasField {
                from: base,
                to: field.clone(),
            });
            edges.push(ObjectEdge::PointsTo { from: field, to });
        }
        edges.into_iter().collect()
    }

    fn source(&'a self, e: &Self::Edge) -> Self::Node {
        match e {
            ObjectEdge::PointsTo { from, .. } => from.clone(),
            ObjectEdge::HasField { from, .. } => from.clone(),
        }
    }

    fn target(&'a self, e: &Self::Edge) -> Self::Node {
        match e {
            ObjectEdge::PointsTo { to, .. } => to.clone(),
            ObjectEdge::HasField { to, .. } => to.clone(),
        }
    }
}

/// Renders the object graph into Graphviz DOT format.
pub fn render_object_graph<W: Write>(
    vtx_points_to: &[(FunctionId, FlowVariable, Path, Heap)],
    fld_points_to: &[(FunctionId, Heap, Path, Heap)],
    id_map: &IdMap,
    writer: &mut W,
) -> std::io::Result<()> {
    let graph = ObjectGraphViz::new(vtx_points_to, fld_points_to, id_map);
    dot::render(&graph, writer)
}
