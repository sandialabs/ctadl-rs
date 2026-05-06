/*!
Topological sort.
*/

use std::collections::VecDeque;

use super::*;

use crate::index::{idx::Idx, index_vec::IndexVec};
use crate::indexvec;

#[cfg(test)]
mod tests;

/// Error type returned when a cycle is detected during topological sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleError;

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cycle detected in graph")
    }
}

impl std::error::Error for CycleError {}

/// Stores the result of a topological sort operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologicalSort<N: Idx> {
    nodes: Vec<N>,
}

impl<N: Idx> TopologicalSort<N> {
    /// Creates a new topological sort for the given graph.
    ///
    /// This implementation uses Kahn's algorithm, which processes vertices with
    /// zero in-degree iteratively using an explicit queue.
    ///
    /// # Arguments
    ///
    /// * `graph` - A directed graph that implements `Successors` and `Predecessors`
    ///
    /// # Returns
    ///
    /// * `Ok(TopologicalSort<G::Node>)` - A struct containing nodes in topological order
    /// * `Err(CycleError)` - If the graph contains a cycle
    ///
    /// # Complexity
    ///
    /// * Time: O(V + E) where V is the number of vertices and E is the number of edges
    /// * Space: O(V) for the in-degree array and processing queue
    pub fn new(graph: &impl Successors<Node = N>) -> Result<Self, CycleError> {
        let num_nodes = graph.num_nodes();

        // Phase 1: Calculate in-degree for all nodes
        let mut in_degree: IndexVec<N, usize> = indexvec![0; num_nodes];
        for node in graph.iter_nodes() {
            for successor in graph.successors(node) {
                in_degree[successor] += 1;
            }
        }

        // Phase 2: Initialize queue with zero in-degree nodes
        let mut queue = VecDeque::new();
        for (node_idx, &degree) in in_degree.iter_enumerated() {
            if degree == 0 {
                queue.push_back(node_idx);
            }
        }

        // Phase 3: Process queue iteratively
        let mut result = Vec::with_capacity(num_nodes);

        while let Some(current) = queue.pop_front() {
            result.push(current);

            // Decrement in-degree of successors, effectively removing the edge from `current` to
            // `successor`. If this results in successor having no indegree, it gets put next in
            // the topological order (after what is in the queue).
            for successor in graph.successors(current) {
                in_degree[successor] -= 1;
                if in_degree[successor] == 0 {
                    queue.push_back(successor);
                }
            }
        }

        // Phase 4: Check for cycles
        if result.len() != num_nodes {
            return Err(CycleError);
        }

        Ok(TopologicalSort { nodes: result })
    }

    /// Copies the result to a vec
    pub fn to_vec(&self) -> Vec<N> {
        self.nodes.clone()
    }

    /// A view of the results as a slice
    pub fn as_slice(&self) -> &[N] {
        &self.nodes
    }

    /// Returns an iterator that yields references to the nodes in topological order.
    pub fn iter(&self) -> impl Iterator<Item = &N> {
        self.nodes.iter()
    }

    /// Returns the number of nodes in the sorted result.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if there are no nodes in the sorted result.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns a reference to the node at the given position.
    pub fn get(&self, index: usize) -> Option<&N> {
        self.nodes.get(index)
    }
}

impl<N: Idx> IntoIterator for TopologicalSort<N> {
    type Item = N;
    type IntoIter = std::vec::IntoIter<N>;

    fn into_iter(self) -> Self::IntoIter {
        self.nodes.into_iter()
    }
}
