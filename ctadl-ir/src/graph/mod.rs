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

/// A graph whose edges carry a label. Like [`Successors`], but each successor is
/// paired with the [`Label`](LabeledSuccessors::Label) of the edge leading to
/// it. Used by [`find_annotated_path_to_set`] to hand the traversed edge's label
/// to an [`Annotation`].
pub trait LabeledSuccessors: DirectedGraph {
    /// Data attached to each edge (e.g. a call/return marker). The label is what
    /// an [`Annotation`] inspects to decide how the annotation evolves across the
    /// edge, or whether the edge is traversable at all.
    type Label;

    /// Iterates the outgoing edges of `node` as `(successor, edge_label)` pairs.
    fn labeled_successors(
        &self,
        node: Self::Node,
    ) -> impl Iterator<Item = (Self::Node, Self::Label)>;
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

/// An annotation threaded along a path during [`find_annotated_path_to_set`].
///
/// The search begins at its `start` node carrying [`Annotation::start`].
/// Whenever it expands a labeled edge `from -> to`, the annotation carried by
/// `from` produces the annotation carried by `to` via [`Annotation::expand`],
/// which sees the edge's [`Label`](LabeledSuccessors::Label) and may also prune
/// the edge. Because the annotation is path-dependent, the same graph node can
/// be reached with several distinct annotations; the search treats each
/// `(node, annotation)` pair as its own state, hence the `Eq + Hash` bound.
pub trait Annotation<G: LabeledSuccessors>: Clone + Eq + std::hash::Hash {
    /// The annotation carried by the node the search starts from.
    fn start() -> Self;

    /// Produce the annotation carried by `to` when the search expands the edge
    /// `from -> to` (whose label is `label`), given `self`, the annotation
    /// carried by `from`. Returning `None` prunes the edge, so `to` is not
    /// reached along this path.
    fn expand(&self, graph: &G, from: G::Node, label: &G::Label, to: G::Node)
    -> Option<Self>;
}

/// Find a path from `start` to the nearest node whose `(node, annotation)` state
/// satisfies `is_target`, following successors and threading an [`Annotation`]
/// along each path.
///
/// This generalizes [`find_path_to_set`]: alongside each search node it carries
/// an annotation, seeded with [`Annotation::start`] at `start` and advanced by
/// [`Annotation::expand`] across every labeled edge (which may also prune
/// edges). The target test sees both the node and its annotation, so set
/// membership can depend on *how* a node was reached. The returned path pairs
/// each node with the annotation it carried, and includes both `start` and the
/// target it reaches; `None` if no target state is reachable.
///
/// # Preconditions
///
/// - `start` exists in the graph.
pub fn find_annotated_path_to_set<G, A>(
    graph: &G,
    start: G::Node,
    is_target: impl Fn(G::Node, &A) -> bool,
) -> Option<Vec<(G::Node, A)>>
where
    G: LabeledSuccessors,
    A: Annotation<G>,
{
    use hashbrown::hash_map::HashMap;
    use hashbrown::hash_set::HashSet;

    let mut visited: HashSet<(G::Node, A)> = HashSet::new();
    let mut parent: HashMap<(G::Node, A), (G::Node, A)> = HashMap::new();
    let mut queue: Vec<(G::Node, A)> = Vec::new();

    let mut end: Option<(G::Node, A)> = None;
    queue.push((start, A::start()));

    while let Some(state) = queue.pop() {
        if visited.insert(state.clone()) {
            let (n, a) = &state;
            if is_target(*n, a) {
                end = Some(state);
                break;
            }
            for (m, label) in graph.labeled_successors(*n) {
                if let Some(b) = a.expand(graph, *n, &label, m) {
                    let next = (m, b);
                    if !visited.contains(&next) {
                        parent.insert(next.clone(), state.clone());
                        queue.push(next);
                    }
                }
            }
        }
    }

    let mut end = end?;
    let mut path = vec![end.clone()];
    while let Some(p) = parent.remove(&end) {
        path.push(p.clone());
        end = p;
    }
    path.reverse();
    Some(path)
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
