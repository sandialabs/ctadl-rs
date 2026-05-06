use super::*;

impl<G: DirectedGraph> DirectedGraph for &G {
    type Node = G::Node;

    fn num_nodes(&self) -> usize {
        (**self).num_nodes()
    }
}

impl<G: StartNode> StartNode for &G {
    fn start_node(&self) -> Self::Node {
        (**self).start_node()
    }
}

impl<G: Successors> Successors for &G {
    fn successors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        (**self).successors(node)
    }
}

impl<G: Predecessors> Predecessors for &G {
    fn predecessors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        (**self).predecessors(node)
    }
}
