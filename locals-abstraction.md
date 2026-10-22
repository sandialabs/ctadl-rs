# Access-path abstraction for `locals` - DESIGN DRAFT - DO-NOT-MERGE

`locals(f, v, p, a, p4)` says that, in function `f`, `v.p` is reached by `a.p4`, where `a` is a
formal. On dense functions it grows huge. The set of paths is finite and fixed before the
fixpoint (`compute_paths`, `PathSet`), so nothing here is about termination. The aim is fewer
rows and fewer joins. When too many rows have the same shape, we replace them with one starred
row that covers all of them and more. The individual rows are then deleted.

## Why not Infer's star as is

In Infer's `AccessTree`, a key is a path such as `x.f`, and each key has a value (its trace).
When a node gets wider than `max_width`, Infer joins all of its children's values into the
node and marks it `Star` (`AccessTree.ml:314`). A lookup that runs into the star returns the
node's value (`:151`).

The obvious port would make `(f, v)` the base, `p` the tree key, and the set of `(a, p4)`
pairs the value. That does not shrink anything here. Each of the rows `v.q·s ← a.r·s` has its
own `p4`, so joining their values into `v.q*` still leaves one `(a, p4)` per old row. To save
rows, the star has to abstract **both** paths together.

## The shape that blows up

Field propagation already treats a row as covering its extensions *at edges*. Rule 1625 takes
`v2.key ← a.p4` and an edge reading `v2.key·rest`, and derives `… ← a.p4·rest`. The rows that
pile up are mostly ones that share a suffix with the formal path:

    v.q·s1 ← a.r·s1,  v.q·s2 ← a.r·s2,  …      (e.g. v aliases a: q = r = ε)

Every such row is an instance of one **root** `(q, r)`. You get the root by removing the longest
common suffix of `p` and `p4`, comparing whole components. Each row has exactly one minimal root.

## Domain

This is a map lattice. Its keys are `(f, v, a, q, r)`, where `(q, r)` is a minimal root. Each
value is

    L = P(Suffix) ∪ {★}          ⊥ = ∅,  S ⊑ ★,  join = ∪ (★ absorbs)

- A finite set `S` stands for the exact rows `{ v.q·s ← a.r·s | s ∈ S }`. This is today's
  meaning.
- `★` stands for `v.q·s ← a.r·s` for **every** admissible `s`. This is the "*" path.
- **Widening:** once `|S| > W`, the value becomes `★`. `W` is like Infer's `max_width`. It is
  configurable, and off (`∞`) by default, so existing results stay bit-identical.
- **Order on rows:** `(p, p4) ⊑ ★(q, r)` iff there is an `s` with `p = q·s` and `p4 = r·s`.
  Propagation can create a `★` at a root that is not minimal, so the subsumption test tries
  every common-suffix split of `(p, p4)`. There are at most `min(|p|, |p4|) + 1` of them.
- **Offsets:** suffixes split at component boundaries of normalized paths. The wild offset
  matching (`.x.[m]` against `.x.[n]`) is not abstracted in v0.

The `★` value really loses precision. For an edge that reads some `v.q·s3` that was never
observed, the star still produces a flow from `a.r·s3`.

## Encoding in Ascent

The rules have to see a `★` row as a row, and the `locals` store has to hold both kinds of row,
because collapsing deletes exact rows and adds a star row. Ascent gives each relation its own
store, so this cannot be split across two relations (`locals` + `locals_star`). Instead,
**`★` is a sentinel last segment on the formal path**: `★(q, r)` is the row
`locals(f, v, q, a, r·★)`.

- The local path `q` is still admissible. `reach_vp`, `locals_key` and `ext_dst` (rule 1618,
  the hot destination-side step) work on it unchanged, and they carry `r·★` along. That is
  already the correct semantics for a star.
- `r·★` is not in `PathSet`, so every `ps.concat`/`splits` on the formal path, and every exact
  join on it (for example with `resolvent`), skips star rows. **Each such rule needs a twin**,
  listed below.
- The sentinel must be a segment the path grammar cannot spell. `Symbol("*")` is already a
  literal field (`models/json.rs:2964`), so it cannot be used.

There is one relation and one store, and one row per star. No `Path` or `PathSet` changes are
needed beyond `strip_star`/`with_star`. This is the per-element encoding from
`turning-lattice-relations-into-byods.md`, with `★` held in the relation rather than in a
separate `_top` relation.

## Store hooks (`locals_trie`, `c_locals_trie`)

| Hook | Meaning |
|---|---|
| `contains_key(total\|delta, row)` | Exact membership, **or** some `★(q, r)` in that store subsumes the row. Probe `(f, v, q)` → `(a, r·★)` for each common-suffix split. In the ByPath regime each probe is a hash lookup. |
| `insert_if_not_present(new, row)` | Insert as today, and add 1 to `count[(a, q, r)]` for the row's minimal root, kept in the `(f, v)` group. The concurrent store already holds the shard lock here. |
| `merge_delta_to_total_new_to_delta` | Absorb as today. Then, in each `(f, v)` group that was touched, for every root whose count is now over `W`: delete the instances of that root from `total`, `delta` and `new`, and put `★(q, r)` into the next `delta` if `total` lacks it. Also drop rows in `new` that a star in `total ⊔ delta` subsumes. |

- **The merge runs single-threaded on `&mut`** in both engines, so collapsing needs no new
  locking.
- **`PathGroup` needs a `retain`.** It has no way to remove rows today.
- **Deleting from `total` is sound** as long as the twins are complete. Whatever the deleted
  rows already derived stays derived. Later deltas join against the star, which covers more
  than the rows did.
- **The same generics also back `locals_key` and `edge_split`.** Abstraction therefore needs a
  policy parameter on the store, with `NoAbstraction` for those two.
- **`CountingVec` lengths no longer match the row count** once the store inserts and deletes
  rows on its own. All stats and dumps have to go through the store, as the note warns.

## Rule twins

Each twin reads `locals(f, v, key, a, rs)` and binds `r` with `if let Some(r) = rs.strip_star()`.
When the rule probes a concrete local path `P`, the twin joins on every
`(key, rest) ∈ ps.splits(P).exact`, so it can find a star at any prefix of `P`. `prefix_keys`
includes `(P, ε)`. Rule 1.2 already uses this pattern.

| Rule (mod.rs) | Twin |
|---|---|
| 1618 destination step | none; carries `r·★` |
| 1625 formal step, 1638 wild formal step | same body; output `with_star(concat(r, rest))`. The result keeps the destination path `p1` and stays a star. |
| 1703 summary | emit `summary(n1, q, n2, r)`. A summary edge already has shift semantics in callers, so a star summary is just a plain summary row. `isout` must hold for some extension: for `SharedFields` with `q = ε`, it holds for every non-empty suffix. Emitting the row is sound but imprecise. |
| 1717 aliasing summary | needs "some `prog_store` at or below `q`". Precompute prefix closure. **Open.** |
| 1736 critical 1.1, 1826/1842 3.1, 1854 resolved_call | split the receiver path; formal path `concat(r, rest)`, then the exact `resolvent` join as today |
| 1743/1751 critical 1.2, 1809 critical_reach 2.2 | already split; the twin only strips `★` before the `concat` |
| 2053/2058 context `locals` steps | same as 1625/1638 |
| 2080 `locals ← context_locals` | none. `context_locals` stays exact in v0. Rows it copies in are dropped if a star subsumes them. |

## Plan

1. **Measure first.** Add a histogram of `locals` rows per `(f, v, a, minimal root)`, modeled on
   `context_histogram`, and run it on the runaway apps. If the large groups do not have this
   shape, this design does not help. The next tier would then be Infer's one-sided star
   (`v.q* ← a.*`), which needs a summary that havocs the destination and is out of scope here.
2. Add the sentinel and `strip_star`/`with_star`. Write the twins with `W = ∞` and check that
   the output is identical to today's.
3. Store: add the root counters, star subsumption in `contains`, collapse in the merge, and
   `PathGroup::retain`. Do the serial store first, then the concurrent one.
4. Unit tests:
   - A star subsumes later instances.
   - Collapsing deletes rows from all three stores.
   - The star reaches the next `delta`.
   - The parallel engine gives the same result as the serial one.
5. Sweep `W` and record rows, time, and the change in `summary`/`resolved_call` output.

## Open questions

- Should the counters be keyed by the minimal root alone, or should rows arriving under an
  existing non-minimal star be counted too?
- The aliasing rule's `prog_store` under a star: is a prefix closure worth its cost, or should
  the twin use the coarser `prog_store` at `q`?
- Should `context_locals` get the same treatment? It has a `DecisionSet` value, so a star there
  would join sets as well.
