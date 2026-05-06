use std::collections::HashSet;

use super::*;
use crate::graph::tests::TestGraph;

#[test]
fn test_dominators_lengaur_paper() {
    let _names = &[
        "R", "C", "F", "I", "K", "G", "J", "B", "E", "H", "A", "D", "L",
    ];
    // I've chosen the order for edges in the graph (and therefore successors) to be the order of
    // DFS numbering in the papers... so that when we run DFS on it, we should get those numbers
    // out. The nodes are renumbered from 0 so the graph is connected.
    let g = TestGraph::new(
        0,
        &[
            // non-tree edges
            (0, 10),
            (4, 3),
            (4, 0),
            (5, 3),
            (6, 3),
            (7, 11),
            (9, 8),
            (9, 4),
            (12, 9),
            // tree edges
            (0, 7),
            (0, 1),
            (1, 5),
            (1, 2),
            (2, 3),
            (3, 4),
            (5, 6),
            (7, 10),
            (7, 8),
            (8, 9),
            (10, 11),
            (11, 12),
        ],
    );
    let tree = DominatorTree::new(&g);
    test_tree(&tree, 12, 11);
    test_tree(&tree, 6, 5);
    test_tree(&tree, 5, 1);
    test_tree(&tree, 2, 1);
    test_tree(&tree, 9, 0);
    test_tree(&tree, 3, 0);
    test_tree(&tree, 4, 0);
    test_tree(&tree, 1, 0);
    test_tree(&tree, 8, 0);
    test_tree(&tree, 10, 0);
    test_tree(&tree, 11, 0);
    test_tree(&tree, 7, 0);
}

#[test]
fn test_dominators_appel_palsberg_book() {
    let g = TestGraph::new(
        0,
        &[
            // non-tree edges
            (2, 1),
            (3, 1),
            (8, 7),
            (9, 4),
            (9, 11),
            (5, 6),
            // tree edges
            (0, 1),
            (1, 3),
            (1, 2),
            (3, 5),
            (3, 4),
            (4, 6),
            (4, 7),
            (7, 8),
            (8, 9),
            (6, 10),
            (10, 11),
        ],
    );
    let tree = DominatorTree::new(&g);
    test_tree(&tree, 1, 0);
    test_tree(&tree, 2, 1);
    test_tree(&tree, 3, 1);
    test_tree(&tree, 4, 3);
    test_tree(&tree, 5, 3);
    test_tree(&tree, 6, 3);
    test_tree(&tree, 11, 3);
    test_tree(&tree, 10, 6);
    test_tree(&tree, 7, 4);
    test_tree(&tree, 8, 7);
    test_tree(&tree, 9, 8);
}

#[test]
fn test_dominator_tree_bottom_up() {
    let g = TestGraph::new(
        0,
        &[
            // non-tree edges
            (2, 1),
            (3, 1),
            (8, 7),
            (9, 4),
            (9, 11),
            (5, 6),
            // tree edges
            (0, 1),
            (1, 3),
            (1, 2),
            (3, 5),
            (3, 4),
            (4, 6),
            (4, 7),
            (7, 8),
            (8, 9),
            (6, 10),
            (10, 11),
        ],
    );
    let tree = DominatorTree::new(&g);
    let mut seen = HashSet::new();
    eprintln!("tree: {tree:#?}");
    for n in tree.iter_postorder() {
        for s in tree.successors(n) {
            assert!(seen.contains(&s), "{n:#?}: {s:#?} not in {seen:#?}");
        }
        seen.insert(n);
    }
    assert_eq!(seen.len(), 12);
}

#[test]
fn test_dominance_frontier_cytron_paper() {
    let g = TestGraph::new(
        0,
        &[
            // 0 is entry node
            (0, 1),
            (1, 2),
            (2, 3),
            (2, 7),
            (3, 4),
            (3, 5),
            (4, 6),
            (5, 6),
            (6, 8),
            (7, 8),
            (8, 9),
            (9, 10),
            (9, 11),
            (10, 11),
            (11, 9),
            (11, 12),
            (12, 2),
            // 13 is the exit node
            (12, 13),
            (0, 13),
        ],
    );
    let dt = DominatorTree::new(&g);
    let df = dt.compute_frontier(&g);
    assert_eq!(df.frontier(1), &[13].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(2), &[2, 13].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(3), &[8].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(4), &[6].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(5), &[6].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(6), &[8].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(7), &[8].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(8), &[2, 13].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(9), &[13, 2, 9].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(10), &[11].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(11), &[13, 2, 9].into_iter().collect::<BitSet>());
    assert_eq!(df.frontier(12), &[2, 13].into_iter().collect::<BitSet>());
}

fn test_tree(tree: &DominatorTree<usize>, succ: usize, pred: usize) {
    assert_eq!(tree.predecessors(succ).next(), Some(pred));
    let succs: Vec<_> = tree.successors(pred).collect();
    assert!(succs.contains(&succ));
}
