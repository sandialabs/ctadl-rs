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
