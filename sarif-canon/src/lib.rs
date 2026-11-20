//! The canonical form of the results of a SARIF run, so that two SARIF files can be compared
//! result by result with [`multiset-diff`].
//!
//! A raw run ([`types::RawRun`]) is what a parser keeps of one SARIF run, over an atom type `A`
//! that stands for interned strings. Its canonical results ([`types::CanonResult`]) replace
//! references by what they name, drop per-run numbers, and sort every collection whose order
//! SARIF gives no meaning, keeping the order of a thread flow's steps. The crate knows nothing
//! of JSON.
//!
//! The code is verified with Verus, for inputs of any size, against `multiset-diff`'s specs
//! and theorems, so verifying it needs that crate's exported specs:
//!
//! ```text
//! verus --crate-type=lib --edition=2024 --crate-name multiset_diff \
//!   --export $T/md.vir --compile -o $T/libmultiset_diff.rlib multiset-diff/src/lib.rs
//! verus --crate-type=lib --edition=2024 --crate-name sarif_canon \
//!   --import multiset_diff=$T/md.vir --extern multiset_diff=$T/libmultiset_diff.rlib \
//!   sarif-canon/src/lib.rs
//! ```
//!
//! or `nix build .#checks.<system>.verus`. A plain `cargo build` erases the specs and proofs,
//! leaving the code.
//!
//! What is proved: the order on each canonical type is total and `Equal` exactly on equal
//! values ([`order`]); the merge sort's output is a sorted permutation of its input, and the
//! sorted permutation is unique ([`sort`]); and the primitives meet their contracts and are
//! unchanged by renumbering a table or permuting a bag ([`prims`]); and [`canon_run`] and
//! [`canon_runs`] return exactly the canonical form `spec_canon_runs` defines, error included,
//! sorted by the order on results ([`canon`]). End to end (`theorems`):
//! `theorem_canon_invariant`, that any number of steps each permuting unordered collections
//! or renumbering a run's `logicalLocations` with every reference into it leaves the canonical
//! form, or its absence, unchanged; and `theorem_diff_empty_iff_equal`, that on sorted
//! canonical results `multiset_diff` reports exactly the results whose numbers of occurrences
//! differ, so nothing exactly when the two multisets are equal. `smoke` checks the statements
//! against concrete runs. Every proof is complete.
//!
//! [`multiset-diff`]: ::multiset_diff
//! [`canon_run`]: canon::canon_run
//! [`canon_runs`]: canon::canon_runs

use vstd::prelude::*;

// The specs exist only when Verus compiles the crate.
#[cfg(verus_keep_ghost)]
#[allow(unused_imports)]
use multiset_diff::{computes, eqv, le, lt, sorted, total_order};

pub mod canon;
pub mod order;
pub mod prims;
pub mod sort;
pub mod types;
pub mod u32_atoms;

// Statements and proofs only, which a plain build erases.
#[cfg(verus_keep_ghost)]
mod smoke;
#[cfg(verus_keep_ghost)]
mod theorems;

verus! {

spec fn u32_ord(x: u32, y: u32) -> std::cmp::Ordering {
    if x < y {
        std::cmp::Ordering::Less
    } else if x > y {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

/// Uses `multiset-diff`'s specs and one of its theorems, so the check fails if they cannot be
/// imported.
proof fn smoke_imports_multiset_diff(a: &[u32], b: &[u32])
    requires
        sorted(|x: u32, y: u32| u32_ord(x, y), a@),
        sorted(|x: u32, y: u32| u32_ord(x, y), b@),
        a@.len() <= usize::MAX,
        b@.len() <= usize::MAX,
{
    let ord = |x: u32, y: u32| u32_ord(x, y);
    assert(total_order(ord));
    multiset_diff::proofs::theorem_no_value_twice(ord, a, b);
}

} // verus!

#[cfg(test)]
mod test_util {
    use vstd::prelude::*;

    verus! {

    /// A ghost argument for tests, which a plain build erases.
    pub fn ghost<T>() -> Ghost<T> {
        Ghost(arbitrary())
    }

    } // verus!
}
