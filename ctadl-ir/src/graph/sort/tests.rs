use super::super::tests::TestGraph;
use super::*;
use std::collections::HashMap;

#[test]
fn test_simple_dag() {
    // Create a simple DAG: 0 -> 1 -> 2
    let graph = TestGraph::new(0, &[(0, 1), (1, 2)]);
    let result = TopologicalSort::new(&graph).unwrap();

    // Valid topological orders: [0, 1, 2]
    assert_eq!(result.as_slice(), &vec![0, 1, 2]);
}

#[test]
fn test_branching_dag() {
    // Create a branching DAG: 0 -> 1, 0 -> 2, 1 -> 3, 2 -> 3
    let graph = TestGraph::new(0, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
    let result = TopologicalSort::<usize>::new(&graph).unwrap();
    let result = result.as_slice();

    // Valid topological orders: [0, 1, 2, 3] or [0, 2, 1, 3]
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], 0); // 0 must come first (no predecessors)
    assert_eq!(result[3], 3); // 3 must come last (no successors)
    assert!(result.contains(&1));
    assert!(result.contains(&2));
}

#[test]
fn test_single_node() {
    let graph = TestGraph::new(0, &[]);
    let result = TopologicalSort::<usize>::new(&graph).unwrap();
    assert_eq!(result.as_slice(), &vec![0]);
}

#[test]
fn test_disconnected_dag() {
    // Create a DAG with disconnected components: 0 -> 1, 2 -> 3
    let graph = TestGraph::new(0, &[(0, 1), (2, 3)]);
    let result = TopologicalSort::new(&graph).unwrap();
    let result = result.as_slice();

    assert_eq!(result.len(), 4);
    // 0 and 2 should come before their respective successors
    let pos_0 = result.iter().position(|&x| x == 0).unwrap();
    let pos_1 = result.iter().position(|&x| x == 1).unwrap();
    let pos_2 = result.iter().position(|&x| x == 2).unwrap();
    let pos_3 = result.iter().position(|&x| x == 3).unwrap();

    assert!(pos_0 < pos_1);
    assert!(pos_2 < pos_3);
}

#[test]
fn test_cycle_detection() {
    // Create a graph with a cycle: 0 -> 1 -> 2 -> 0
    let graph = TestGraph::new(0, &[(0, 1), (1, 2), (2, 0)]);
    let result = TopologicalSort::new(&graph);
    assert!(result.is_err());
    assert!(matches!(result, Err(CycleError)));
}

#[test]
fn test_self_loop() {
    // Create a graph with a self-loop: 0 -> 0
    let graph = TestGraph::new(0, &[(0, 0)]);
    let result = TopologicalSort::new(&graph);
    assert!(result.is_err());
    assert!(matches!(result, Err(CycleError)));
}

#[test]
fn test_topological_sort_struct_methods() {
    // Test the TopologicalSort struct methods
    let graph = TestGraph::new(0, &[(0, 1), (1, 2)]);
    let sort = TopologicalSort::<usize>::new(&graph).unwrap();

    // Test iter()
    let iter_result: Vec<_> = sort.iter().cloned().collect();
    assert_eq!(iter_result, vec![0, 1, 2]);

    // Test len()
    assert_eq!(sort.len(), 3);

    // Test is_empty()
    assert!(!sort.is_empty());

    // Test get()
    assert_eq!(sort.get(0), Some(&0));
    assert_eq!(sort.get(2), Some(&2));
    assert_eq!(sort.get(3), None);
}

#[test]
fn test_complex_dag() {
    // Create a more complex DAG
    let edges = vec![(0, 1), (0, 2), (1, 3), (1, 4), (2, 4), (3, 5), (4, 5)];
    let graph = TestGraph::new(0, &edges);
    let result: Vec<_> = TopologicalSort::new(&graph).unwrap().into_iter().collect();

    assert_eq!(result.len(), 6);

    // Verify topological ordering constraints
    let positions: HashMap<_, _> = result
        .iter()
        .enumerate()
        .map(|(i, &node)| (node, i))
        .collect();

    assert!(positions[&0] < positions[&1]); // 0 -> 1
    assert!(positions[&0] < positions[&2]); // 0 -> 2
    assert!(positions[&1] < positions[&3]); // 1 -> 3
    assert!(positions[&1] < positions[&4]); // 1 -> 4
    assert!(positions[&2] < positions[&4]); // 2 -> 4
    assert!(positions[&3] < positions[&5]); // 3 -> 5
    assert!(positions[&4] < positions[&5]); // 4 -> 5
}
