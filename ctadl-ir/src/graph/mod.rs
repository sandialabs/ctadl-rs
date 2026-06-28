/*!
Graph traits.

Much of this code is inspired by or cribbed from
<https://doc.rust-lang.org/beta/nightly-rustc/src/rustc_data_structures/graph/mod.rs.html>.
*/
use crate::index::{idx::Idx, index_vec::IndexVec};
use bit_set::BitSet;

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

/// Find a path from start to end.
///
/// # Preconditions
///
/// - `start` and `end` exist in the graph.
pub fn find_path<G: Successors>(graph: &G, start: G::Node, end: G::Node) -> Option<Vec<G::Node>> {
    find_path_to_set(graph, start, |n| end == n)
}

/// Find a path from `start` to the nearest node satisfying `is_target`,
/// following successors. The returned path includes both `start` and the target
/// node it reaches; `None` if no target is reachable.
///
/// This generalizes [`find_path`] to a set of acceptable endpoints; pass
/// `|n| n == end` to recover single-target search.
///
/// # Preconditions
///
/// - `start` exists in the graph.
pub fn find_path_to_set<G: Successors>(
    graph: &G,
    start: G::Node,
    is_target: impl Fn(G::Node) -> bool,
) -> Option<Vec<G::Node>> {
    use hashbrown::hash_set::HashSet;
    let mut visited = HashSet::new();
    let mut parent = IndexVec::from_elem_n(None, graph.num_nodes());
    let mut queue = Vec::new();
    let mut path = Vec::new();

    let mut end = start;
    queue.push(start);

    while let Some(n) = queue.pop() {
        if visited.insert(n) {
            if is_target(n) {
                end = n;
                break;
            }
            for m in graph.successors(n) {
                if !visited.contains(&m) {
                    parent.insert(m, n);
                    queue.push(m);
                }
            }
        }
    }

    if !is_target(end) {
        None
    } else {
        path.push(end);
        while let Some(p) = parent.remove(end) {
            path.push(p);
            end = p;
        }
        path.reverse();
        Some(path)
    }
}

/// Assigns a depth-first numbering to each graph node and returns the nodes in dfs numbered order.
///
/// # Preconditions
///
/// The graph is non-empty.
pub fn reachable<G>(graph: &G) -> IndexVec<G::Node, G::Node>
where
    G: DirectedGraph + StartNode + Predecessors + Successors,
{
    let mut nodes = vec![graph.start_node()];
    let mut seen = BitSet::new();
    let mut reach: IndexVec<G::Node, G::Node> = IndexVec::new();

    while let Some(n) = nodes.pop() {
        if seen.insert(n.index()) {
            reach.push(n);

            for w in graph.successors(n) {
                nodes.push(w);
            }
        }
    }

    reach
}

/// Returns true if every node is reachable from the start node
///
/// # Preconditions
///
/// The graph is non-empty.
pub fn is_connected<G>(graph: &G) -> bool
where
    G: DirectedGraph + StartNode + Predecessors + Successors,
{
    let mut nodes = vec![graph.start_node()];
    let mut seen = BitSet::new();
    let mut dfs_counter = G::Node::new(0);

    while let Some(n) = nodes.pop() {
        if seen.insert(n.index()) {
            dfs_counter = dfs_counter.plus(1);

            for w in graph.successors(n) {
                nodes.push(w);
            }
        }
    }

    dfs_counter.index() == graph.num_nodes()
}
