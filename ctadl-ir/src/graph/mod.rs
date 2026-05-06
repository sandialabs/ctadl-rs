/*!
Graph traits.

Much of this code is inspired by or cribbed from
<https://doc.rust-lang.org/beta/nightly-rustc/src/rustc_data_structures/graph/mod.rs.html>.
*/
use crate::index::idx::Idx;

pub mod dominators;
pub mod reference;
pub mod reversed;
pub mod scc;
pub mod sort;

#[cfg(test)]
mod tests;

pub trait DirectedGraph {
    type Node: Idx;

    /// Returns the total number of nodes in this graph.
    ///
    /// Several graph algorithm implementations assume that every node ID is
    /// strictly less than the number of nodes, i.e. nodes are densely numbered.
    /// That assumption allows them to use `num_nodes` to allocate per-node
    /// data structures, indexed by node.
    fn num_nodes(&self) -> usize;

    /// Iterates over all nodes of a graph in ascending numeric order.
    ///
    /// Assumes that nodes are densely numbered, i.e. every index in
    /// `0..num_nodes` is a valid node.
    fn iter_nodes(&self) -> impl DoubleEndedIterator<Item = Self::Node> + ExactSizeIterator {
        (0..self.num_nodes()).map(<Self::Node as Idx>::new)
    }
}

pub trait NumEdges: DirectedGraph {
    fn num_edges(&self) -> usize;
}

pub trait StartNode: DirectedGraph {
    fn start_node(&self) -> Self::Node;
}

pub trait ExitNode: DirectedGraph {
    fn exit_node(&self) -> Self::Node;
}

pub trait Successors: DirectedGraph {
    fn successors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node>;
}

pub trait Predecessors: DirectedGraph {
    fn predecessors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node>;
}

/// Alias for [`DirectedGraph`] + [`StartNode`] + [`Predecessors`] + [`Successors`].
pub trait ControlFlowGraph: DirectedGraph + StartNode + Predecessors + Successors {}

impl<T> ControlFlowGraph for T where T: DirectedGraph + StartNode + Predecessors + Successors {}

pub fn find_path<G: Successors>(graph: &G, start: G::Node, end: G::Node) -> Option<Vec<G::Node>> {
    use hashbrown::hash_set::HashSet;
    let mut visited = HashSet::new();
    let mut path = Vec::new();
    fn dfs<G: Successors>(
        graph: &G,
        curr: G::Node,
        target: G::Node,
        visited: &mut HashSet<G::Node>,
        path: &mut Vec<G::Node>,
    ) -> bool {
        if curr == target {
            path.push(curr);
            return true;
        }
        if !visited.insert(curr) {
            return false;
        }
        path.push(curr);
        for succ in graph.successors(curr) {
            if dfs(graph, succ, target, visited, path) {
                return true;
            }
        }
        path.pop();
        false
    }
    if dfs(graph, start, end, &mut visited, &mut path) {
        Some(path)
    } else {
        None
    }
}
