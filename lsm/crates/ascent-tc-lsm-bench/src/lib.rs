use std::hash::Hash;
use std::iter::{FlatMap, Repeat, Zip};
use std::sync::atomic::{AtomicUsize, Ordering};

use ascent::ascent;
use ascent::internal::RelIndexMerge;
use ascent_byods_rels::adaptor::bin_rel::ByodsBinRel;
use bytemuck::Pod;
use lsm::{IterAllRefs, MmapLsmMultiMap, ValsRefs};

/// Implementation default for the LSM write-buffer size. This is an ordinary
/// default — it can be changed freely without affecting the benchmarks, which
/// set the limit they depend on explicitly via [`set_memtable_limit`].
const DEFAULT_MEMTABLE_LIMIT: usize = 1024;

/// The write-buffer size used by [`DiskLsmBinRel::default`]. Because Ascent
/// constructs the provider through `Default` (no parameters), this is held in a
/// process-global so the benchmark can configure it before running.
static MEMTABLE_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_MEMTABLE_LIMIT);

/// Sets the LSM write-buffer size used when the provider is created. Benchmarks
/// call this so their results depend on a value they own, not on
/// [`DEFAULT_MEMTABLE_LIMIT`].
pub fn set_memtable_limit(limit: usize) {
    MEMTABLE_LIMIT.store(limit.max(1), Ordering::Relaxed);
}

pub type Node = u64;

pub mod lsm_bin_rel_provider {
    #[doc(hidden)]
    #[macro_export]
    macro_rules! lsm_bin_rel_provider_ind_common {
        ($name: ident, ($col0: ty, $col1: ty), $indices: expr, ser, ()) => {
            $crate::DiskLsmBinRel<$col0, $col1>
        };
    }

    pub use ascent_byods_rels::adaptor::bin_rel_provider::{
        rel, rel_codegen, rel_full_ind, rel_ind,
    };
    pub use lsm_bin_rel_provider_ind_common as rel_ind_common;
}

/// A binary-relation provider backed *only* by the disk LSM, with no in-memory
/// mirror at all. The two maps `by_left` (left → rights) and `by_right`
/// (right → lefts) are the left/right indices the `ByodsBinRel` trait needs.
///
/// Reads are served straight out of [`MmapLsmMultiMap`], whose `get_refs` /
/// `iter_all_refs` hand back `&T` references that point either into the small
/// in-RAM memtable or directly into memory-mapped run files — no decoding into
/// owned `Vec`s. So with a small memtable limit the bulk of the data lives in
/// `mmap`'d pages (off the heap, in the page cache) while the trait still gets
/// the borrowed references it requires.
///
/// Keys and values are constrained to `bytemuck::Pod` so the on-disk bytes can
/// be reinterpreted in place.
#[derive(Debug)]
pub struct DiskLsmBinRel<T0, T1>
where
    T0: Pod + Ord,
    T1: Pod + Ord,
{
    by_left: MmapLsmMultiMap<T0, T1>,
    by_right: MmapLsmMultiMap<T1, T0>,
}

impl<T0, T1> Default for DiskLsmBinRel<T0, T1>
where
    T0: Pod + Ord,
    T1: Pod + Ord,
{
    fn default() -> Self {
        let limit = MEMTABLE_LIMIT.load(Ordering::Relaxed);
        Self {
            by_left: MmapLsmMultiMap::with_memtable_limit(limit),
            by_right: MmapLsmMultiMap::with_memtable_limit(limit),
        }
    }
}

impl<T0, T1> RelIndexMerge for DiskLsmBinRel<T0, T1>
where
    T0: Pod + Ord + Hash + Eq,
    T1: Pod + Ord + Hash + Eq,
{
    fn move_index_contents(from: &mut Self, to: &mut Self) {
        // Drain every pair out of `from` (memtable *and* runs, via the merging
        // borrowing scan) and re-insert into `to`, keeping both stores deduped.
        for (x0, rights) in from.by_left.iter_all_refs() {
            for x1 in rights {
                to.insert(*x0, *x1);
            }
        }
        // Clear both stores in place (deleting their run files) rather than
        // reassigning a fresh `Self`.
        from.by_left.clear();
        from.by_right.clear();
    }
}

impl<T0, T1> ByodsBinRel for DiskLsmBinRel<T0, T1>
where
    T0: Pod + Ord + Hash + Eq,
    T1: Pod + Ord + Hash + Eq,
{
    type T0 = T0;
    type T1 = T1;

    fn contains(&self, x0: &Self::T0, x1: &Self::T1) -> bool {
        self.by_left.contains_value(x0, x1)
    }

    // `iter_all` yields each `(&T0, &T1)` pair. Pairing a borrowed key with each
    // of its values needs no capturing closure: `repeat(key).zip(values)` keeps
    // the iterator type nameable (no boxing / dynamic dispatch beyond what the
    // merging scan already uses).
    type AllIter<'a>
        = FlatMap<
        IterAllRefs<'a, T0, T1>,
        Zip<Repeat<&'a T0>, ValsRefs<'a, T1>>,
        fn((&'a T0, ValsRefs<'a, T1>)) -> Zip<Repeat<&'a T0>, ValsRefs<'a, T1>>,
    >
    where
        Self: 'a;

    fn iter_all(&self) -> Self::AllIter<'_> {
        fn pair<'a, T0, T1>(
            (key, values): (&'a T0, ValsRefs<'a, T1>),
        ) -> Zip<Repeat<&'a T0>, ValsRefs<'a, T1>> {
            std::iter::repeat(key).zip(values)
        }

        self.by_left.iter_all_refs().flat_map(pair::<T0, T1> as fn(_) -> _)
    }

    fn len_estimate(&self) -> usize {
        self.by_left.len()
    }

    type Ind0AllIterValsIter<'a>
        = ValsRefs<'a, T1>
    where
        Self: 'a;
    type Ind0AllIter<'a>
        = IterAllRefs<'a, T0, T1>
    where
        Self: 'a;

    fn ind0_iter_all(&self) -> Self::Ind0AllIter<'_> {
        self.by_left.iter_all_refs()
    }

    fn ind0_len_estimate(&self) -> usize {
        self.by_left.len()
    }

    type Ind0ValsIter<'a>
        = ValsRefs<'a, T1>
    where
        Self: 'a;

    fn ind0_index_get<'a>(&'a self, key: &Self::T0) -> Option<Self::Ind0ValsIter<'a>> {
        self.by_left.get_refs(key)
    }

    type Ind1AllIterValsIter<'a>
        = ValsRefs<'a, T0>
    where
        Self: 'a;
    type Ind1AllIter<'a>
        = IterAllRefs<'a, T1, T0>
    where
        Self: 'a;

    fn ind1_iter_all(&self) -> Self::Ind1AllIter<'_> {
        self.by_right.iter_all_refs()
    }

    fn ind1_len_estimate(&self) -> usize {
        self.by_right.len()
    }

    type Ind1ValsIter<'a>
        = ValsRefs<'a, T0>
    where
        Self: 'a;

    fn ind1_index_get<'a>(&'a self, key: &Self::T1) -> Option<Self::Ind1ValsIter<'a>> {
        self.by_right.get_refs(key)
    }

    fn insert(&mut self, x0: Self::T0, x1: Self::T1) -> bool {
        // Dedup against the store itself (memtable + runs); a present (x0, x1)
        // means the pair already exists.
        if self.contains(&x0, &x1) {
            return false;
        }

        self.by_left.insert(x0, x1);
        self.by_right.insert(x1, x0);
        true
    }

    fn is_empty(&self) -> bool {
        self.by_left.is_empty()
    }
}

ascent! {
    #![ds(crate::lsm_bin_rel_provider)]

    relation edge(Node, Node);
    relation reachable(Node, Node);

    reachable(x, y) <-- edge(x, y);
    reachable(x, z) <-- reachable(x, y), edge(y, z);
}

pub fn chain_edges(nodes: Node) -> Vec<(Node, Node)> {
    (0..nodes.saturating_sub(1))
        .map(|node| (node, node + 1))
        .collect()
}

pub fn run_transitive_closure(edges: Vec<(Node, Node)>) -> usize {
    let mut program = AscentProgram::default();
    // With the custom LSM provider the relation lives in the provider, not the
    // public `edge`/`reachable` Vecs, so feed and count through the index.
    for edge in edges {
        program.__edge_ind_common.insert(edge.0, edge.1);
    }

    program.run();
    program.__reachable_ind_common.len_estimate()
}

ascent! {
    // The same transitive-closure program backed by Ascent's default relation
    // data structures, used as a head-to-head baseline against the disk-backed
    // LSM provider above.
    struct DefaultDsProgram;

    relation edge(Node, Node);
    relation reachable(Node, Node);

    reachable(x, y) <-- edge(x, y);
    reachable(x, z) <-- reachable(x, y), edge(y, z);
}

pub fn run_transitive_closure_default(edges: Vec<(Node, Node)>) -> usize {
    let mut program = DefaultDsProgram::default();
    program.edge = edges;

    program.run();
    program.reachable.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_transitive_closure_over_disk_lsm_relation() {
        let reachable = run_transitive_closure(chain_edges(8));
        assert_eq!(reachable, 28);
    }

    #[test]
    fn default_and_lsm_providers_agree() {
        let edges = chain_edges(8);
        assert_eq!(
            run_transitive_closure_default(edges.clone()),
            run_transitive_closure(edges),
        );
    }
}
