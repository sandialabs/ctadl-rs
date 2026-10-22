# DO-NOT-MERGE: bugs found on this branch

Notes for review only. Remove before merging.

## Union memo ignores the widening bound

`DecisionSet::union` (`ctadl-ascent/src/index_engine/decision.rs`) memoizes results in
`UNION_CACHE`. That is a process-global, direct-mapped table of 2^20 slots, keyed only by the
addresses of the two input sets. `union_uncached` widens the result to ⊤ when it has more than
`WIDEN_BOUND` members, and `WIDEN_BOUND` is also process-global, set per run by `set_widen_bound`.
The bound is not part of the cache key.

Interned sets live for the whole process, so their addresses stay valid across runs. A second
index run in the same process with a different bound can therefore hit an entry from the first
run:

- An entry cached under no bound returns an explicit set where the new bound says ⊤, which
  skips widening.
- An entry cached under a small bound returns ⊤ where the new bound allows the explicit set,
  which forces widening.

**Repro:** in one process, index the fixtures under `decision`, then under `bounded:2`.
`hybrid_context_precision.tnt` produces 132 output lines, against 137 in a fresh process.

**Scope:** only processes that run more than one hybrid-context mode, such as an all-modes test
harness. `ctadl index` runs one mode per process, and the in-tree tests use one config, so
neither is affected. The bug predates this branch.

**Possible fixes (none applied):**

1. Make the bound part of the key. Pack it into an existing slot word (for example
   `len | bound << 32`) and treat a mismatch as a miss. This needs no extra memory.
2. Clear the cache in `set_widen_bound`. This needs no extra memory, but each mode switch
   rewrites the whole 40 MiB table. It is still wrong if runs with different bounds overlap.
3. Cache the unwidened union and widen on read. This interns sets larger than the bound, which
   changes the interning.

The root issue is that `WIDEN_BOUND` and `SPILL` are process-global rather than per-run.
