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

// A label-free view: every edge carries `()`. Enough to exercise the annotated
// search with annotations that don't care about edge labels.
impl LabeledSuccessors for TestGraph {
    type Label = ();

    fn labeled_successors(&self, node: usize) -> impl Iterator<Item = (Self::Node, ())> {
        self.successors[&node].iter().map(|&n| (n, ()))
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
    fn expand(&self, _graph: &TestGraph, _from: usize, _label: &(), _to: usize) -> Option<Self> {
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
    fn expand(&self, _graph: &TestGraph, _from: usize, _label: &(), _to: usize) -> Option<Self> {
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

    let path = find_annotated_path_to_set(&g, 0, |n, _a: &Hops| n == 3).expect("path exists");
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
    let path =
        find_annotated_path_to_set(&g, 0, |n, Hops(h)| n == 4 && *h == 3).expect("path exists");
    let nodes: Vec<usize> = path.iter().map(|(n, _)| *n).collect();
    assert_eq!(nodes, vec![0, 2, 3, 4]);
    assert_eq!(path.last().unwrap().1, Hops(3));
}

#[test]
fn find_annotated_path_expand_prunes_edges() {
    // 0 -> 1 -> 2 -> 3.
    let g = TestGraph::new(0, &[(0, 1), (1, 2), (2, 3)]);

    // Within budget: node 2 sits 2 hops out and is reachable.
    let path = find_annotated_path_to_set(&g, 0, |n, _a: &Budget| n == 2).expect("path exists");
    assert_eq!(
        path.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    // Node 3 needs a 3rd edge, which the budget prunes -> unreachable.
    assert!(find_annotated_path_to_set(&g, 0, |n, _a: &Budget| n == 3).is_none());
}

/// An edge label mirroring the taint use case: an edge is a call, a return, or
/// an intraprocedural step. Calls and returns are tagged with the call site so a
/// return can be matched against the call that must have opened it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Edge {
    Call(u32),
    Return(u32),
    Intra,
}

/// A graph whose edges carry an [`Edge`] label.
struct LabeledGraph {
    num_nodes: usize,
    successors: HashMap<usize, Vec<(usize, Edge)>>,
}

impl LabeledGraph {
    fn new(num_nodes: usize, edges: &[(usize, usize, Edge)]) -> Self {
        let mut successors: HashMap<usize, Vec<(usize, Edge)>> = HashMap::default();
        for node in 0..num_nodes {
            successors.entry(node).or_default();
        }
        for &(src, dst, label) in edges {
            successors.entry(src).or_default().push((dst, label));
        }
        Self {
            num_nodes,
            successors,
        }
    }
}

impl DirectedGraph for LabeledGraph {
    type Node = usize;

    fn num_nodes(&self) -> usize {
        self.num_nodes
    }
}

impl LabeledSuccessors for LabeledGraph {
    type Label = Edge;

    fn labeled_successors(&self, node: usize) -> impl Iterator<Item = (Self::Node, Edge)> {
        self.successors[&node].iter().cloned()
    }
}

/// The call stack accumulated along a path. A `Return(s)` edge is only
/// traversable when `s` matches the call on top of the stack, so the search only
/// admits realizable (call/return-balanced) paths.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct CallStack(Vec<u32>);

impl Annotation<LabeledGraph> for CallStack {
    fn start() -> Self {
        CallStack(Vec::new())
    }

    fn expand(
        &self,
        _graph: &LabeledGraph,
        _from: usize,
        label: &Edge,
        _to: usize,
    ) -> Option<Self> {
        match label {
            Edge::Intra => Some(self.clone()),
            Edge::Call(site) => {
                let mut stack = self.0.clone();
                stack.push(*site);
                Some(CallStack(stack))
            }
            Edge::Return(site) => match self.0.last() {
                // A return must match the call on top of the stack; a mismatch
                // is an unrealizable path and prunes the edge.
                Some(top) if top == site => {
                    let mut stack = self.0.clone();
                    stack.pop();
                    Some(CallStack(stack))
                }
                Some(_) => None,
                // Returning with nothing on the stack is allowed: we may enter a
                // callee and return to an unknown caller.
                None => Some(self.clone()),
            },
        }
    }
}

#[test]
fn find_annotated_path_matches_calls_and_returns() {
    // Balanced route 0 =call1=> 1 -> 2 =return1=> 3, plus a shortcut
    // 1 =return2=> 3 whose return does not match the pending call1.
    let g = LabeledGraph::new(
        4,
        &[
            (0, 1, Edge::Call(1)),
            (1, 2, Edge::Intra),
            (2, 3, Edge::Return(1)),
            (1, 3, Edge::Return(2)),
        ],
    );

    // The mismatched shortcut edge is pruned, so the only path to 3 is the
    // balanced one, and it arrives with an empty (balanced) stack.
    let path = find_annotated_path_to_set(&g, 0, |n, _s: &CallStack| n == 3).expect("path exists");
    let nodes: Vec<usize> = path.iter().map(|(n, _)| *n).collect();
    assert_eq!(nodes, vec![0, 1, 2, 3]);
    assert_eq!(path.last().unwrap().1, CallStack(vec![]));
}

#[test]
fn find_annotated_path_prunes_unrealizable_only_route() {
    // The only route to 3 opens call1 but returns via a mismatched return2.
    let g = LabeledGraph::new(4, &[(0, 1, Edge::Call(1)), (1, 3, Edge::Return(2))]);

    // No realizable path reaches 3.
    assert!(find_annotated_path_to_set(&g, 0, |n, _s: &CallStack| n == 3).is_none());

    // But node 1 (reached with call1 still pending) is fine.
    let path = find_annotated_path_to_set(&g, 0, |n, _s: &CallStack| n == 1).expect("path exists");
    assert_eq!(path.last().unwrap().1, CallStack(vec![1]));
}

/// A lazy view of [`LabeledGraph`]: edges are computed on demand from the
/// successor table, exercising the [`LazySuccessors`] search path.
impl LazySuccessors for LabeledGraph {
    type Node = usize;
    type Label = Edge;

    fn labeled_successors(&self, node: &usize) -> Vec<(usize, Edge)> {
        self.successors[node].clone()
    }
}

/// One-bit call/return matching, the taint use case: a return is only
/// traversable when no call is pending.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pending {
    Free,
    InCall,
}

impl LazyAnnotation<LabeledGraph> for Pending {
    fn start() -> Self {
        Pending::Free
    }

    fn expand(
        &self,
        _graph: &LabeledGraph,
        _from: &usize,
        label: &Edge,
        _to: &usize,
    ) -> Option<Self> {
        match label {
            Edge::Intra => Some(*self),
            Edge::Call(_) => Some(Pending::InCall),
            Edge::Return(_) => match self {
                Pending::Free => Some(Pending::Free),
                Pending::InCall => None,
            },
        }
    }
}

#[test]
fn find_annotated_paths_from_set_shares_visited_and_finds_all_targets() {
    // Two starts converge on 2, which fans out to targets 3 and 4; 5 is only
    // reachable through an unrealizable call-then-return route.
    let g = LabeledGraph::new(
        7,
        &[
            (0, 2, Edge::Intra),
            (1, 2, Edge::Intra),
            (2, 3, Edge::Intra),
            (2, 4, Edge::Intra),
            (2, 6, Edge::Call(9)),
            (6, 5, Edge::Return(9)),
        ],
    );

    let targets = [3usize, 4, 5];
    let search =
        find_annotated_paths_from_set(&g, [0usize, 1], |n, _a: &Pending| targets.contains(n));

    // 3 and 4 are found; 5 is pruned (return while a call is pending).
    let found: Vec<usize> = search
        .targets
        .iter()
        .map(|&t| search.states[t as usize].node)
        .collect();
    assert_eq!(found, vec![3, 4]);

    // Each target's path starts at one of the starts and is a real walk.
    for &t in &search.targets {
        let path = search.path_to(t);
        let first = search.states[path[0] as usize].node;
        assert!(first == 0 || first == 1);
        for w in path.windows(2) {
            let from = search.states[w[0] as usize].node;
            let to = search.states[w[1] as usize].node;
            assert!(g.successors[&from].iter().any(|(s, _)| *s == to));
            // The traversed edge label is recorded on the child state.
            assert!(search.states[w[1] as usize].edge.is_some());
        }
    }

    // The visited set is shared across starts: node 2 (and everything past it)
    // is explored once, so the state count stays linear in the reachable
    // subgraph. Reached states: 0, 1, 2, 3, 4, 6 (5 is pruned).
    assert_eq!(search.states.len(), 6);

    // A start that is itself a target is reported as a trivial path.
    let search = find_annotated_paths_from_set(&g, [3usize], |n, _a: &Pending| *n == 3);
    assert_eq!(search.targets.len(), 1);
    assert_eq!(search.path_to(search.targets[0]).len(), 1);
}

#[test]
fn find_annotated_paths_from_set_revisits_node_under_new_annotation() {
    // 0 =call=> 1 =return=> 2: unrealizable. But 3 -> 1 (intra) reaches 1 Free,
    // and from that state the return to 2 is traversable. The node 1 is thus
    // reached under two annotations, and only the Free one reaches 2.
    let g = LabeledGraph::new(
        4,
        &[
            (0, 1, Edge::Call(7)),
            (3, 1, Edge::Intra),
            (1, 2, Edge::Return(7)),
        ],
    );

    let search = find_annotated_paths_from_set(&g, [0usize, 3], |n, _a: &Pending| *n == 2);
    assert_eq!(search.targets.len(), 1);
    let path = search.path_to(search.targets[0]);
    let nodes: Vec<usize> = path
        .iter()
        .map(|&i| search.states[i as usize].node)
        .collect();
    assert_eq!(nodes, vec![3, 1, 2]);
    // Node 1 appears twice among the states: once InCall (from 0), once Free
    // (from 3).
    let annots_at_1: Vec<Pending> = search
        .states
        .iter()
        .filter(|s| s.node == 1)
        .map(|s| s.annot)
        .collect();
    assert_eq!(annots_at_1.len(), 2);
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
