use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::sync::{Arc, OnceLock};

use smallvec::SmallVec;

use crate::graph::{
    DirectedGraph, Predecessors, StartNode, Successors,
    dominators::{DominanceFrontier, DominatorTree},
};
use crate::index::{idx::Idx, index_vec::IndexVec};
pub use crate::mir::{BasicBlockData, BasicBlockIdx};

/// Set of basic blocks.
///
/// The entry block is always 0.
#[derive(Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BasicBlocks {
    /// List of basic blocks. Each block starts with a nonnegative number of phi nodes followed by
    /// other statements.
    basic_blocks: IndexVec<BasicBlockIdx, BasicBlockData>,
    #[cfg_attr(feature = "serde", serde(skip))]
    cache: Arc<Cache>,
}

impl Clone for BasicBlocks {
    /// Close the blocks with an empty cache
    fn clone(&self) -> Self {
        let BasicBlocks { basic_blocks, .. } = self;
        BasicBlocks {
            basic_blocks: basic_blocks.clone(),
            cache: Default::default(),
        }
    }
}

impl BasicBlocks {
    /// Creates empty basic blocks storage
    pub fn new() -> Self {
        Default::default()
    }

    /// Create default basic blocks of a given length
    pub fn new_len(n: usize) -> Self {
        let basic_blocks = IndexVec::from_elem_n(BasicBlockData::new(None), n);
        Self {
            basic_blocks,
            ..Default::default()
        }
    }

    /// Allocates a new, empty block and returns its index
    pub fn new_block(&mut self) -> BasicBlockIdx {
        self.basic_blocks.push(BasicBlockData::new(None));
        self.basic_blocks.last_index().unwrap()
    }

    #[inline]
    fn predecessors(&self) -> &IndexVec<BasicBlockIdx, SmallVec<[BasicBlockIdx; 4]>> {
        self.cache.predecessors.get_or_init(|| {
            let mut preds = IndexVec::from_elem(SmallVec::new(), &self.basic_blocks);
            for (bb, data) in self.basic_blocks.iter_enumerated() {
                if let Some(term) = &data.terminator {
                    for succ in term.successors() {
                        preds[succ].push(bb);
                    }
                }
            }
            preds
        })
    }

    /// Returns a mutable reference to the basic blocks. Invalidates the CFG cache.
    #[inline]
    pub fn blocks_mut(&mut self) -> &mut IndexVec<BasicBlockIdx, BasicBlockData> {
        self.invalidate_cfg_cache();
        &mut self.basic_blocks
    }

    /// Returns a mutable reference to the basic blocks but does not invalidate this CFG cache.
    /// Calling this is a promise not to change the CFG.
    ///
    /// 1. Don't change the number of basic blocks.
    /// 2. Don't change the successors.
    ///
    /// If you do, call [`BasicBlocks::invalidate_cfg_cache`].
    #[inline]
    pub fn blocks_mut_preserves_cfg(&mut self) -> &mut IndexVec<BasicBlockIdx, BasicBlockData> {
        &mut self.basic_blocks
    }

    /// Returns the dominator tree for the CFG.
    #[inline]
    pub fn dominators(&self) -> &DominatorTree<BasicBlockIdx> {
        self.cache
            .dominators
            .get_or_init(|| DominatorTree::new(&self))
    }

    /// Returns the dominance frontier. Dominance frontier computation depends on dominators, so
    /// this function also computes dominators.
    #[inline]
    pub fn dominance_frontier(&self) -> &DominanceFrontier<BasicBlockIdx> {
        let dominators = self.dominators();
        self.cache
            .dominance_frontier
            .get_or_init(|| dominators.compute_frontier(&self))
    }

    pub fn invalidate_cfg_cache(&mut self) {
        if let Some(cache) = Arc::get_mut(&mut self.cache) {
            // If we only have a single reference to this cache, clear it.
            *cache = Cache::default();
        } else {
            // If we have several references to this cache, overwrite the pointer itself so other
            // users can continue to use their (valid) cache.
            self.cache = Arc::new(Cache::default());
        }
    }
}

impl Successors for BasicBlocks {
    #[inline]
    fn successors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        self.basic_blocks[node].terminator().successors()
    }
}

impl Predecessors for BasicBlocks {
    #[inline]
    fn predecessors(&self, node: Self::Node) -> impl Iterator<Item = Self::Node> {
        self.predecessors()[node].iter().copied()
    }
}

impl StartNode for BasicBlocks {
    #[inline]
    fn start_node(&self) -> BasicBlockIdx {
        BasicBlockIdx::START_BLOCK
    }
}

impl DirectedGraph for BasicBlocks {
    type Node = BasicBlockIdx;

    #[inline]
    fn num_nodes(&self) -> usize {
        self.basic_blocks.len()
    }
}

impl Deref for BasicBlocks {
    type Target = IndexVec<BasicBlockIdx, BasicBlockData>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.basic_blocks
    }
}

impl DerefMut for BasicBlocks {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.blocks_mut()
    }
}

impl Index<BasicBlockIdx> for BasicBlocks {
    type Output = BasicBlockData;
    #[inline]
    fn index(&self, index: BasicBlockIdx) -> &Self::Output {
        &self.basic_blocks[index]
    }
}

impl IndexMut<BasicBlockIdx> for BasicBlocks {
    #[inline]
    fn index_mut(&mut self, index: BasicBlockIdx) -> &mut Self::Output {
        &mut self.blocks_mut()[index]
    }
}

impl fmt::Display for BasicBlocks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bb, data) in self.basic_blocks.iter_enumerated() {
            let annot = if bb == BasicBlockIdx::START_BLOCK {
                " [start]"
            } else {
                ""
            };
            writeln!(f, "begin block_{}{}:", bb.index(), annot)?;
            write!(f, "{data}")?;
            writeln!(f, "end block_{}", bb.index())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cache {
    predecessors: OnceLock<IndexVec<BasicBlockIdx, SmallVec<[BasicBlockIdx; 4]>>>,
    dominators: OnceLock<DominatorTree<BasicBlockIdx>>,
    dominance_frontier: OnceLock<DominanceFrontier<BasicBlockIdx>>,
}
