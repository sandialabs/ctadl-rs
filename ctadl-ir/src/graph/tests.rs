use std::cmp::max;
use std::collections::HashMap;

use super::*;

pub(super) struct TestGraph {
    num_nodes: usize,
    start_node: usize,
    successors: HashMap<usize, Vec<usize>>,
    predecessors: HashMap<usize, Vec<usize>>,
}

impl TestGraph {
    pub(super) fn new(start_node: usize, edges: &[(usize, usize)]) -> Self {
        let mut graph = TestGraph {
            num_nodes: start_node + 1,
            start_node,
            successors: HashMap::default(),
            predecessors: HashMap::default(),
        };
        for &(source, target) in edges {
            graph.num_nodes = max(graph.num_nodes, source + 1);
            graph.num_nodes = max(graph.num_nodes, target + 1);
            graph.successors.entry(source).or_default().push(target);
            graph.predecessors.entry(target).or_default().push(source);
        }
        for node in 0..graph.num_nodes {
            graph.successors.entry(node).or_default();
            graph.predecessors.entry(node).or_default();
        }
        graph
    }
}

impl DirectedGraph for TestGraph {
    type Node = usize;

    fn num_nodes(&self) -> usize {
        self.num_nodes
    }
}

impl StartNode for TestGraph {
    fn start_node(&self) -> usize {
        self.start_node
    }
}

impl Predecessors for TestGraph {
    fn predecessors(&self, node: usize) -> impl Iterator<Item = Self::Node> {
        self.predecessors[&node].iter().cloned()
    }
}

impl Successors for TestGraph {
    fn successors(&self, node: usize) -> impl Iterator<Item = Self::Node> {
        self.successors[&node].iter().cloned()
    }
}

#[test]
fn find_path_to_set_reaches_nearest_target() {
    // 0 -> 1 -> 2 -> 3, plus a 0 -> 4 branch.
    let g = TestGraph::new(0, &[(0, 1), (1, 2), (2, 3), (0, 4)]);

    // Reaches a target in the set; the returned path ends at that target.
    let path = find_path_to_set(&g, 0, |n| n == 3 || n == 4).expect("path exists");
    assert_eq!(*path.first().unwrap(), 0);
    let end = *path.last().unwrap();
    assert!(end == 3 || end == 4);
    // Path is a real walk: each step is a successor of the previous.
    for w in path.windows(2) {
        assert!(g.successors(w[0]).any(|s| s == w[1]));
    }

    // No reachable target -> None.
    assert!(find_path_to_set(&g, 4, |n| n == 3).is_none());

    // start is itself a target -> trivial single-node path.
    assert_eq!(find_path_to_set(&g, 2, |n| n == 2), Some(vec![2]));

    // Single-target predicate recovers `find_path`.
    assert_eq!(find_path_to_set(&g, 0, |n| n == 3), find_path(&g, 0, 3));
}

/// Counts the number of edges traversed to reach a node.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Hops(usize);

impl Annotation<TestGraph> for Hops {
    fn start() -> Self {
        Hops(0)
    }
    fn expand(&self, _graph: &TestGraph, _from: usize, _to: usize) -> Option<Self> {
        Some(Hops(self.0 + 1))
    }
}

/// Counts hops but refuses to expand past a budget of 2 edges, pruning any
/// deeper edge.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Budget(usize);

impl Annotation<TestGraph> for Budget {
    fn start() -> Self {
        Budget(0)
    }
    fn expand(&self, _graph: &TestGraph, _from: usize, _to: usize) -> Option<Self> {
        if self.0 >= 2 {
            None
        } else {
            Some(Budget(self.0 + 1))
        }
    }
}

#[test]
fn find_annotated_path_threads_annotation() {
    // 0 -> 1 -> 2 -> 3.
    let g = TestGraph::new(0, &[(0, 1), (1, 2), (2, 3)]);

    let path =
        find_annotated_path_to_set(&g, 0, |n, _a: &Hops| n == 3).expect("path exists");
    let nodes: Vec<usize> = path.iter().map(|(n, _)| *n).collect();
    assert_eq!(nodes, vec![0, 1, 2, 3]);
    // The annotation counts edges traversed: 0 hops at the start, 3 at the goal.
    let hops: Vec<usize> = path.iter().map(|(_, Hops(h))| *h).collect();
    assert_eq!(hops, vec![0, 1, 2, 3]);

    // start is itself a target -> trivial single-node path carrying the start
    // annotation.
    let trivial = find_annotated_path_to_set(&g, 2, |n, _a: &Hops| n == 2);
    assert_eq!(trivial, Some(vec![(2, Hops(0))]));
}

#[test]
fn find_annotated_path_target_depends_on_annotation() {
    // Two routes to the goal 4: a short one 0 -> 1 -> 4 (2 hops) and a long one
    // 0 -> 2 -> 3 -> 4 (3 hops).
    let g = TestGraph::new(0, &[(0, 1), (1, 4), (0, 2), (2, 3), (3, 4)]);

    // Accept the goal only when reached in exactly 3 hops. A node-keyed search
    // would mark node 4 visited via the 2-hop route and give up; keying on
    // `(node, annotation)` lets the 3-hop route through.
    let path = find_annotated_path_to_set(&g, 0, |n, Hops(h)| n == 4 && *h == 3)
        .expect("path exists");
    let nodes: Vec<usize> = path.iter().map(|(n, _)| *n).collect();
    assert_eq!(nodes, vec![0, 2, 3, 4]);
    assert_eq!(path.last().unwrap().1, Hops(3));
}

#[test]
fn find_annotated_path_expand_prunes_edges() {
    // 0 -> 1 -> 2 -> 3.
    let g = TestGraph::new(0, &[(0, 1), (1, 2), (2, 3)]);

    // Within budget: node 2 sits 2 hops out and is reachable.
    let path =
        find_annotated_path_to_set(&g, 0, |n, _a: &Budget| n == 2).expect("path exists");
    assert_eq!(path.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![0, 1, 2]);

    // Node 3 needs a 3rd edge, which the budget prunes -> unreachable.
    assert!(find_annotated_path_to_set(&g, 0, |n, _a: &Budget| n == 3).is_none());
}

#[test]
fn find_path_to_set_handles_deep_graph() {
    // A long linear chain: 0 -> 1 -> ... -> N. A recursive DFS would overflow
    // the stack here; the iterative implementation must not.
    const N: usize = 200_000;
    let edges: Vec<(usize, usize)> = (0..N).map(|i| (i, i + 1)).collect();
    let g = TestGraph::new(0, &edges);

    let path = find_path_to_set(&g, 0, |n| n == N).expect("path exists");
    assert_eq!(*path.first().unwrap(), 0);
    assert_eq!(*path.last().unwrap(), N);
    assert_eq!(path.len(), N + 1);

    // Unreachable target in a deep graph still terminates with None.
    let g2 = TestGraph::new(0, &(0..N).map(|i| (i, i + 1)).collect::<Vec<_>>());
    assert!(find_path_to_set(&g2, 0, |n| n == N + 5).is_none());
}
