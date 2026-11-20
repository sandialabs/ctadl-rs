//! `canon_runs` and `cmp_result` with `u32` atoms under `u32`'s usual order, for unverified
//! callers, which cannot supply the ghost atom order the generic forms take.
//!
//! `canon_runs_u32`'s output is sorted by exactly the order `cmp_result_u32` computes, so a
//! caller that hands both to `multiset_diff` meets its precondition.

use std::cmp::Ordering;

use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
use multiset_diff::{computes, sorted, total_order};

use crate::canon::canon_runs;
#[cfg(verus_keep_ghost)]
use crate::canon::{result_view, spec_canon_runs};
use crate::order::cmp_result;
#[cfg(verus_keep_ghost)]
use crate::order::{equal_means_eq, lemma_ord_result_exec, ord_result_exec};
#[cfg(verus_keep_ghost)]
use crate::prims::lift_vec;
use crate::types::{CanonResult, Error, RawRun};

verus! {

/// `u32`'s usual order, the atom order of this module.
pub open spec fn ord_u32() -> spec_fn(u32, u32) -> Ordering {
    |x: u32, y: u32|
        if x < y {
            Ordering::Less
        } else if x > y {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
}

/// The order on canonical results with `u32` atoms.
pub open spec fn ord_result_u32() -> spec_fn(CanonResult<u32>, CanonResult<u32>) -> Ordering {
    ord_result_exec(ord_u32())
}

/// `ord_u32` and `ord_result_u32` are total orders, and each is `Equal` exactly on equal
/// values (for results, equal views).
pub proof fn lemma_ord_u32()
    ensures
        total_order(ord_u32()),
        equal_means_eq(ord_u32()),
        total_order(ord_result_u32()),
        forall|x: CanonResult<u32>, y: CanonResult<u32>|
            #[trigger] ord_result_u32()(x, y) == Ordering::Equal <==> x@ == y@,
{
    lemma_ord_result_exec(ord_u32());
}

fn cmp_u32(x: &u32, y: &u32) -> (o: Ordering)
    ensures
        o == ord_u32()(*x, *y),
{
    if *x < *y {
        Ordering::Less
    } else if *x > *y {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// The canonical results of all of `runs`, pooled and sorted by `cmp_result_u32`.
pub fn canon_runs_u32(runs: &[RawRun<u32>], endpoints_rule: Option<u32>) -> (r: Result<
    Vec<CanonResult<u32>>,
    Error,
>)
    ensures
        lift_vec(result_view(), r) == spec_canon_runs(
            ord_u32(),
            runs@.map_values(|x: RawRun<u32>| x@),
            endpoints_rule,
        ),
        r is Ok ==> sorted(ord_result_u32(), r->Ok_0@),
{
    let cmp = |x: &u32, y: &u32| -> (o: Ordering)
        ensures
            o == ord_u32()(*x, *y),
        { cmp_u32(x, y) };
    proof {
        lemma_ord_u32();
        assert(computes(cmp, ord_u32()));
    }
    canon_runs(runs, endpoints_rule, &cmp, Ghost(ord_u32()))
}

/// The order `canon_runs_u32` sorts by.
pub fn cmp_result_u32(x: &CanonResult<u32>, y: &CanonResult<u32>) -> (o: Ordering)
    ensures
        o == ord_result_u32()(*x, *y),
{
    let cmp = |x: &u32, y: &u32| -> (o: Ordering)
        ensures
            o == ord_u32()(*x, *y),
        { cmp_u32(x, y) };
    proof {
        assert(computes(cmp, ord_u32()));
    }
    cmp_result(x, y, &cmp, Ghost(ord_u32()))
}

} // verus!
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Props, RawResult};

    fn result(rule: u32) -> RawResult<u32> {
        RawResult {
            rule: Some(rule),
            kind: 1,
            level: 2,
            locations: Vec::new(),
            code_flows: Vec::new(),
            props: Props {
                labels: None,
                vertex: None,
                vertices: None,
                source_fns: None,
                sink_fns: None,
                tainted_formals: None,
                import: None,
            },
        }
    }

    #[test]
    fn sorted_by_cmp_result_u32() {
        let run = RawRun {
            logical: Vec::new(),
            results: vec![result(3), result(1), result(2), result(1)],
        };
        let r = canon_runs_u32(&[run], None).unwrap();
        let rules: Vec<_> = r.iter().map(|c| c.rule).collect();
        assert_eq!(rules, [Some(1), Some(1), Some(2), Some(3)]);
        assert!(r.windows(2).all(|w| cmp_result_u32(&w[0], &w[1]).is_le()));
        assert_eq!(cmp_result_u32(&r[0], &r[1]), Ordering::Equal);
    }
}
