/*!
Dominators computation based on Lengauer-Tarjan.

Based on code from:
- <https://github.com/reactorlabs/rir/blob/d1081b5b82120af480844fff9227d29e4aa395f7/rir/src/compiler/analysis/cfg.cpp>
- <https://myhee.com/publications/2020-seminar-dominators-slides-notes.pdf>

It is by far the most helpful explanation and implementation of the code I have found.
*/
use std::collections::HashSet;
use std::sync::OnceLock;

use bit_set::BitSet;
use smallvec::SmallVec;

use crate::graph::{DirectedGraph, Predecessors, StartNode, Successors};
use crate::index::{
    idx::{Idx, IdxOrdered},
    index_vec::IndexVec,
};
use crate::indexvec;

#[cfg(test)]
mod tests;

/// Dominator tree. The dominator tree is a tree whose root is the start node and each other node's
/// parent is its immediate dominator.
///
/// It's stored in an array that maps a child to its parent.
#[derive(Debug, Clone)]
pub struct DominatorTree<N: Idx> {
    start_node: N,
    /// Maps a node to its parent in the dominator tree.
    idom: IndexVec<N, N>,
    /// The depth-first numbering of the tree. Can be used to iterate over elements of the
    /// dominator tree in pre- or post-order.
    dfnum: IndexVec<N, N>,
    cache: Cache<N>,
}

/// Dominance frontier.
#[derive(Debug, Clone)]
pub struct DominanceFrontier<N: Idx> {
    df: IndexVec<N, BitSet>,
}

#[derive(Debug, Clone)]
struct DomImpl<N: IdxOrdered> {
    dfnum: IndexVec<N, N>,
    // This is the only storage that's indexed by dfnum rather than original graph node.
    vertex: IndexVec<N, N>,
    parent: IndexVec<N, Option<N>>,
    // Initially, each node's semidominator is itself, since that seems safe.
    semi: IndexVec<N, N>,
    samedom: IndexVec<N, Option<N>>,
    bucket: IndexVec<N, HashSet<N>>,
    ancestor: IndexVec<N, Option<N>>,
    // Initially, each best is itself, since that's what link would do if it really initialized
    // everything.
    best: IndexVec<N, N>,
    dom: IndexVec<N, N>,
}

impl<N: IdxOrdered> DominatorTree<N> {
    /// Computes dominator tree for the given graph using the Lengauer-Tarjan algorithm.
    ///
    /// Assumes that every node is reachable from the start node. If unreachable nodes are found,
    /// an assert is triggered.
    pub fn new<G>(graph: &G) -> DominatorTree<N>
    where
        G: DirectedGraph<Node = N> + StartNode + Predecessors + Successors,
    {
        let mut dimpl = DomImpl::<N>::new(graph);
        dimpl.dfs(graph);
        dimpl.semidom(graph);
        let dfnum = dimpl.dfnum.clone();
        let idom = dimpl.dom(graph);

        DominatorTree {
            start_node: graph.start_node(),
            idom,
            dfnum,
            cache: Cache::new(),
        }
    }
}

impl<N: IdxOrdered> DomImpl<N> {
    fn new<G>(graph: &G) -> Self
    where
        G: DirectedGraph<Node = N> + StartNode + Predecessors + Successors,
    {
        let num_nodes = graph.num_nodes();
        Self {
            dfnum: indexvec![N::new(0); num_nodes],
            vertex: indexvec![N::new(0); num_nodes],
            parent: indexvec![None; num_nodes],
            semi: (0..graph.num_nodes()).map(N::new).collect(),
            bucket: indexvec![HashSet::new(); num_nodes],
            samedom: indexvec![None; num_nodes],
            dom: indexvec![N::new(0); num_nodes],
            ancestor: indexvec![None; num_nodes],
            best: (0..graph.num_nodes()).map(N::new).collect(),
        }
    }

    /// Assigns a depth-first numbering to each graph node and records a spanning tree.
    fn dfs<G>(&mut self, graph: &G)
    where
        G: DirectedGraph<Node = N> + StartNode + Predecessors + Successors,
    {
        let mut dfs_counter = N::new(0);
        // node x parent
        let mut nodes = vec![(graph.start_node(), None)];
        let mut seen = BitSet::new();

        while let Some((n, p)) = nodes.pop() {
            if seen.insert(n.index()) {
                let dfs_n = dfs_counter;
                dfs_counter = dfs_counter.plus(1);
                self.dfnum[n] = dfs_n;
                self.vertex[dfs_n] = n;
                self.parent[n] = p;

                for w in graph.successors(n) {
                    nodes.push((w, Some(n)));
                }
            }
        }

        // The dfs numbering hits each node exactly once. If that isn't the same as the number of
        // nodes, then the graph has unreachable nodes.
        assert_eq!(
            dfs_counter.index(),
            graph.num_nodes(),
            "Graph contains nodes not reachable from entry"
        );
    }

    /// Find semidominators. Semidominators are *candidate* dominators.
    fn semidom<G>(&mut self, graph: &G)
    where
        G: DirectedGraph<Node = N> + StartNode + Predecessors + Successors,
    {
        // Iterate bottom up. The order is important because then we know that when we are
        // processing `n`, nodes with higher dfnums have already been processed.
        for i in graph.iter_nodes().rev() {
            if i == graph.start_node() {
                continue;
            }
            let n = self.vertex[i];
            let p = self.parent[n].unwrap();

            // Parent of n is semidominator candidate
            let mut s = p;

            // Find semidominator of w using Theorem 4.
            for v in graph.predecessors(n) {
                let s1 = {
                    if self.dfnum[v] <= self.dfnum[n] {
                        // Semidominator theorem clause 1
                        v
                    } else {
                        // Semidominator theorem clause 2
                        let u = self.find_candidate_ancestor(v);
                        self.semi[u]
                    }
                };
                if self.dfnum[s1] < self.dfnum[s] {
                    // Take the lowest dfnum as the new candidate semidominator
                    s = s1;
                }
            }

            self.semi[n] = s;
            self.bucket[s].insert(n);
            self.link(p, n);

            for v in self.bucket[p].clone() {
                let y = self.find_candidate_ancestor(v);
                if self.semi[y] == self.semi[v] {
                    // Dominator theorem clause 1
                    self.dom[v] = p;
                } else {
                    // Dominator theorem clause 2
                    self.samedom[v] = Some(y);
                }
            }
            self.bucket[p].clear();
        }
    }

    /// Assign final dominators. Returns the dominator tree as a mapping of node to its parent.
    fn dom<G>(mut self, graph: &G) -> IndexVec<N, N>
    where
        G: DirectedGraph<Node = N> + StartNode + Predecessors + Successors,
    {
        for i in graph.iter_nodes().rev() {
            if i == graph.start_node() {
                continue;
            }
            let n = self.vertex[i];
            // Perform deferred dominator calculations
            if let Some(y) = self.samedom[n] {
                // idom(n) = idom(y) and we had previously assigned `y` to samedom(n)
                self.dom[n] = self.dom[y];
            }
        }
        self.dom
    }

    /// Finds an ancestor of `v` that is a candidate semidominator.
    ///
    /// # Precondition
    ///
    /// `v` has an ancestor.
    fn find_candidate_ancestor(&mut self, v: N) -> N {
        // This is ancestorWithLowestSemi in the C++ code
        let Some(a) = self.ancestor[v] else {
            panic!("`v` has no ancestor")
        };
        if let Some(ancestor_a) = self.ancestor[a] {
            let b = self.find_candidate_ancestor(a);
            // Compress the path as we walk up the forest. We want to skip a which is currently v's
            // ancestor.
            self.ancestor[v] = Some(ancestor_a);
            // `b` might be the new "best" node, i.e., the node whose semidominator has the lowest
            // dfnum. Up date `best` if this is the case.
            let cur_best = self.best[v];
            if self.dfnum[self.semi[b]] < self.dfnum[self.semi[cur_best]] {
                self.best[v] = b;
            }
        }
        self.best[v]
    }

    /// Add edge `(child, parent)` to spanning forest.
    #[inline]
    fn link(&mut self, parent: N, child: N) {
        // Initially `ancestor[n] points to `n`'s parent.
        self.ancestor[child] = Some(parent);
        // Initially only `n` is in the path to its ancestor, so it is our current best.
        self.best[child] = child;
    }
}

impl<N: IdxOrdered> DominatorTree<N> {
    /// Dominance frontier computation.
    pub fn compute_frontier<G>(&self, graph: &G) -> DominanceFrontier<N>
    where
        G: DirectedGraph<Node = N> + Successors,
    {
        let mut df: IndexVec<N, BitSet> = IndexVec::from_elem_n(BitSet::new(), graph.num_nodes());
        for x in self.iter_postorder() {
            for y in graph.successors(x) {
                // Local contribution
                if self.idom(y) != x {
                    df[x].insert(y.index());
                }
            }
            // As we walk bottom up, lower nodes get filled and we use them to fill higher nodes.
            for z in self.successors(x) {
                for y in df[z].clone().into_iter().map(N::new) {
                    // Up
                    if self.idom(y) != x {
                        df[x].insert(y.index());
                    }
                }
            }
        }
        DominanceFrontier { df }
    }
}

impl<N: IdxOrdered> DominanceFrontier<N> {
    /// Iterate over the nodes of the dominanco frontier for `n`
    #[inline]
    pub fn iter(&self, n: N) -> impl Iterator<Item = N> {
        self.df[n].iter().map(N::new)
    }

    /// Return the dominance frontier set for `n`
    #[inline]
    pub fn frontier(&self, n: N) -> &BitSet {
        &self.df[n]
    }
}

impl<N: Idx> DominatorTree<N> {
    /// Get the immediate dominator of the node n.
    ///
    /// # Panics
    ///
    /// If the index is out of bounds.
    #[inline]
    pub fn idom(&self, n: N) -> N {
        self.idom[n]
    }
}

impl<N: Idx> DirectedGraph for DominatorTree<N> {
    type Node = N;
    #[inline]
    fn num_nodes(&self) -> usize {
        self.idom.len()
    }
}

impl<N: Idx> StartNode for DominatorTree<N> {
    #[inline]
    fn start_node(&self) -> N {
        self.start_node
    }
}

impl<N: Idx> Predecessors for DominatorTree<N> {
    #[inline]
    fn predecessors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        // It's a tree so there's only one predecessor per node. Start node has no predecessor.
        let preds = if node != self.start_node() {
            vec![self.idom[node]]
        } else {
            vec![]
        };
        preds.into_iter()
    }
}

impl<N: Idx> Successors for DominatorTree<N> {
    #[inline]
    fn successors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        let v = self.cache.successors.get_or_init(|| {
            let mut succs = IndexVec::from_elem(SmallVec::new(), &self.idom);
            for (node, pred) in self.idom.iter_enumerated() {
                if node != self.start_node() {
                    succs[*pred].push(node)
                }
            }
            succs
        });
        v[node].iter().cloned()
    }
}

impl<N: IdxOrdered> DominatorTree<N> {
    /// Iterate over the nodes in the dominator tree bottom up. This consults the dfs ordering of
    /// the nodes.
    pub fn iter_postorder(
        &self,
    ) -> impl Iterator<Item = <DominatorTree<N> as DirectedGraph>::Node> {
        use std::cmp::Ordering::*;
        let mut nodes: Vec<_> = self.iter_nodes().collect();
        // We want a descending order by dfs number. Sort is ascending. So we have to compare the
        // dfs numbers then invert the ordering.
        nodes.sort_by(|n, m| match <N>::cmp(&self.dfnum[*n], &self.dfnum[*m]) {
            Equal => Equal,
            Less => Greater,
            Greater => Less,
        });
        nodes.into_iter()
    }

    pub fn iter_preorder(&self) -> impl Iterator<Item = <DominatorTree<N> as DirectedGraph>::Node> {
        let mut nodes: Vec<_> = self.iter_nodes().collect();
        // We want ascending order by dfs number.
        nodes.sort_by(|n, m| <N>::cmp(&self.dfnum[*n], &self.dfnum[*m]));
        nodes.into_iter()
    }
}

#[derive(Debug, Clone)]
struct Cache<N: Idx> {
    successors: OnceLock<IndexVec<N, SmallVec<[N; 4]>>>,
}

impl<N: Idx> Cache<N> {
    #[inline]
    fn new() -> Self {
        Self {
            successors: OnceLock::new(),
        }
    }
}
