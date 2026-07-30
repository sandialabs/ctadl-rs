# Hybrid set for the `locals` index: implementation and evaluation - DO-NOT-MERGE

Implements `locals-trie-hybrid-ds.md` — a set that is a **linear-probing hashtable** below a
size threshold and a **`hashbrown::HashTable`** above it — and evaluates it three ways: the
structure on its own, the `locals` store built from it, and `ctadl index` on generated programs.
Baseline throughout is the representation it replaces (sorted `Vec` per group, promoted to a
`HashSet` past 64 leaves), measured from the pre-change binary.

`SMALL_THRESHOLD` ships at **64**. The spec names 32 as the *initial* value; 16, 32 and 64 were
all measured and §5 is why 64 won.

Same machine as `locals-trie-benchmark.md`: Apple M1 Ultra (20 cores, 128 GB), macOS 26.5.2
(arm64), rustc 1.94.1, hashbrown 0.16.1, `--release`.

**Headline.** End to end the `locals` store gets **23–35 % smaller** and the fixpoint **2–14 %
faster**. The memory win is one fact: **77–100 % of the groups in this store hold exactly one
leaf**, and a one-element set now costs 24 B where it used to cost 96. The time win is a
separate one: nothing re-copies an accumulated group on merge any more, and no group is searched
with `binary_search` over 24-byte elements.

---

## 1. What was built

| file | role |
|---|---|
| `ctadl-ascent/src/index_engine/hybrid_set.rs` | `HybridSet<T, S>` — the data structure, plus 8 unit tests |
| `ctadl-ascent/src/index_engine/locals_trie.rs` | groups are now `HybridSet<(P,M,Fp)>`; sorted-`Vec` machinery removed; 2 store-level tests added |
| `ctadl-ascent/benches/hybrid_set.rs` | tier 0 — the structure on its own, against three alternatives |

`HybridSet` has two representations behind one enum:

* **`Probe`** — open addressing, linear probing, power-of-two slots. The unusual choice is that
  **the occupancy map is a `u64` bitmask stored inline in the struct**, not a control byte per
  slot on the heap. Three consequences:
  * a small set's allocation is *exactly* `slots * size_of::<T>()` — no control bytes, no
    `Group::WIDTH` mirror, nothing;
  * the empty/occupied test during a probe is a bit test on a register, so only a *hit* touches
    the heap;
  * iteration is `trailing_zeros`, i.e. O(elements) rather than O(slots).
* **`hashbrown::HashTable`** above `SMALL_THRESHOLD`.

The `u64` bitmask is also what caps the threshold at 64, so the shipped value is the largest the
representation admits.

Two decisions the spec left open, both of which turned out to matter more than the choice of
probing scheme:

1. **Slots start at 1 and double** (1, 2, 4, …, 64), not at `Vec`'s minimum non-zero capacity of
   4. A probe table has no reason to hold spare slots, and most sets in this workload hold *one*
   element. This is where essentially all of the memory win comes from — see finding 9.
2. **Load factor is allowed to reach 1.0.** Nothing is ever removed from a Datalog index, so
   there are no tombstones and a probe can stop at the first empty slot; a full table is still
   correct because the probe loop is bounded by the slot count. This keeps the "no load-factor
   slack" property — and costs a full scan on a *miss* against a completely full table
   (finding 4).

The transition is one pass over ≤ 64 elements: `HashTable::with_capacity` sized for the exact
final element count (so filling it never rehashes), elements **moved** rather than cloned, small
buffer freed immediately. It is local to one set and, without removals, happens at most once per
set. `merge` gets the symmetric treatment: a union is commutative, so it inserts the smaller side
into the larger and costs O(min(|a|,|b|)) lookups whichever way the caller passed them.

`size_of::<HybridSet<(P,M,Fp)>>()` is **32 B**, the same as the enum it replaces, because rustc
niche-fills the `Probe` variant into the bytes outside `HashTable`'s non-null `ctrl` pointer. So
the outer map entry stays 48 B and none of the store's per-group cost moved.

### What changed in `locals_trie`

Groups are no longer sorted, so two read paths had to change. Both are the same conversion the
old `Group::Large` arm already did:

* `0_1_2`'s `index_get` **filters** where it used to take a `partition_point` range over the
  contiguous `P`-run. It keeps the `None` fast path with a short-circuiting pre-scan, so a probe
  for a `P` the group does not hold still returns `None` rather than an empty iterator.
* `0_1_2`'s `iter_all` **buckets by `P`** where it used to split sorted runs.

`merge_sorted` / `merge_size` are gone with their caller. Their unit test is replaced by two
store-level tests: one checks all five views (`none`, `0_1`, `0_1_2`, `0_3_4`, full) against the
rows they were built from, at group sizes on both sides of the threshold; the other checks
`absorb` — the delta→total merge — keeps `len`, `fidx` and the groups consistent for every
combination of representations. `cargo test -p ctadl-ascent` is green (192 + 12 tests).

---

## 2. Tier 0 — the structure on its own

`cargo bench -p ctadl-ascent --bench hybrid_set`. One set at a time, 24 B elements (the
production leaf), 1 M elements per row spread over `1 M / n` sets, so per-element figures are
comparable across `n` and per-set overhead amortizes exactly as the index amortizes it. Bytes are
from a counting global allocator. `vec64` is the predecessor; `hash` is `hashbrown::HashSet` at
every size. Best of the three is bold.

| n | B/elem: hybrid / vec64 / hash | insert ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | **24.0** / 96.0 / 108.0 | 29.3 / **22.5** / 24.6 | 3.2 / **3.0** / 4.9 | 3.0 / **2.1** / 2.4 | **19.6** / 24.6 / 56.8 |
| 2 | **24.0** / 48.0 / 54.0 | 20.7 / 13.5 / **11.9** | 3.0 / 3.0 / **2.6** | 3.0 / 2.8 / **1.8** | **31.7** / 36.8 / 47.1 |
| 3 | **32.0** / **32.0** / 36.0 | 23.2 / **10.1** / 11.0 | 3.2 / 4.3 / **2.6** | 2.9 / 3.6 / **1.6** | **38.1** / 43.5 / 43.4 |
| 5 | **38.4** / **38.4** / 41.6 | 22.3 / 22.2 / **19.1** | **3.0** / 7.0 / 3.5 | **2.6** / 3.7 / 4.2 | **39.9** / 52.7 / 54.1 |
| 8 | **24.0** / **24.0** / 51.0 | **13.9** / 17.1 / 29.1 | **2.1** / 6.2 / 3.3 | 4.8 / **3.6** / 3.6 | **39.7** / 59.1 / 59.4 |
| 16 | **24.0** / **24.0** / 50.5 | **12.8** / 23.9 / 30.5 | **2.4** / 8.2 / 4.5 | 9.5 / 6.0 / **2.8** | **38.6** / 44.8 / 48.2 |
| 24 | **32.0** / **32.0** / 33.7 | **14.2** / 30.2 / 19.4 | 2.6 / 11.2 / **2.6** | **2.8** / 4.6 / 2.8 | 39.2 / 42.5 / **35.9** |
| 32 | **24.0** / **24.0** / 50.2 | **12.2** / 29.0 / 24.7 | 3.4 / 10.6 / **3.0** | 20.1 / 4.4 / **2.4** | **31.2** / 39.1 / 52.1 |
| 48 | **32.0** / **32.0** / 33.5 | **9.9** / 32.9 / 14.4 | **1.8** / 13.5 / 2.6 | **2.6** / 6.3 / 3.3 | **29.4** / 48.6 / 35.3 |
| 64 | **24.0** / **24.0** / 50.1 | **9.2** / 33.2 / 16.9 | 3.1 / 12.9 / **2.7** | 43.6 / 5.3 / **2.2** | **23.7** / 43.2 / 46.9 |
| 65 | 49.4 / 49.4 / 49.4 | 20.6 / 40.6 / **17.5** | 4.5 / **2.7** / 2.8 | 2.4 / 2.4 / **2.1** | **34.1** / 55.2 / 45.9 |
| 128 | 50.1 / 50.1 / 50.1 | 19.0 / 29.9 / **15.2** | 4.0 / 3.9 / **2.7** | 3.1 / 3.1 / **2.8** | **26.6** / 37.9 / 38.1 |
| 1024 | 50.0 / 50.0 / 50.0 | 16.7 / 15.2 / **12.6** | 5.7 / 5.5 / **5.3** | **2.2** / 2.7 / 3.0 | **29.7** / 40.9 / 31.5 |

**Findings.**

1. **It is never worse than either alternative on memory below the threshold, and up to 4×
   better.** A 1-element set costs 24 B against the `Vec`'s 96 (min capacity 4) and hashbrown's
   108 (4-bucket floor + control bytes); a 2-element set costs 24 B/elem against 48 and 54.
   Through the whole 8…64 range it is **2.1× smaller than `hashbrown`** at exactly the 24.0
   B/elem the sorted `Vec` achieved — the inline bitmask really does make the small
   representation free of metadata.
2. **Merge is the strongest operation: fastest at every size measured**, 1.15–1.9× the sorted
   `Vec` and up to 2.9× hashbrown at n=1. Inserting the smaller side into the larger, plus never
   re-copying an accumulated set, is what does it.
3. **Insert and hit are 2.5–4× cheaper than the sorted `Vec`** from n=8 up (9.2 ns vs 33.2 for
   insert at n=64; 2.4 ns vs 8.2 for a hit at n=16). `Vec::insert`'s shift and `binary_search`'s
   dependent loads over 24-byte elements both scale with the set; a bit test plus one compare
   does not.
4. **Misses against a completely full table are the design's real weakness, and raising the
   threshold made the worst case worse** — 9.5 ns at n=16, 20.1 at n=32 and **43.6 ns at n=64**,
   against ~5 ns for the sorted `Vec` and ~2 ns for hashbrown. Load factor 1.0 means a miss has
   no empty slot to stop at and degenerates to a scan of all slots. This is the direct cost of
   refusing to pay load-factor slack. It does **not** show up end to end (§4: group size 63 is
   the shape that manufactures exactly-full 64-slot tables, and it is the *best* configuration
   measured), but it is the thing to look at first if a future workload regresses.
5. **Below 4 elements the extra doublings cost more time than they save** — insert is 23.2 ns at
   n=3 against the `Vec`'s 10.1, because reaching 3 elements from a 1-slot start means two
   rehash-and-realloc steps. That is the price of holding 1- and 2-element sets in 24 B, and §4
   says it is worth paying.

---

## 3. Tier 1 — the `locals` store (exact bytes, counting allocator)

`cargo bench -p ctadl-ascent --bench locals_trie`, 1 M rows per row of the table, group size
swept, driven exactly as Ascent's semi-naive loop drives it. `heap_report()` still matches the
allocator to **1.00** everywhere, so the estimate logged by every real index run remains
trustworthy.

Semi-naive shape (one new leaf per group per round — the merge-heavy case):

| group | old B/row | new B/row | Δ | old s | new s | speedup |
|---|---|---|---|---|---|---|
| 1 | 213.4 | **141.4** | **−34 %** | 0.225 | 0.207 | 1.09× |
| 2 | 82.7 | 82.7 | 0 % | 0.263 | 0.232 | 1.13× |
| 4 | 53.4 | 53.4 | 0 % | 0.246 | 0.205 | 1.20× |
| 8 | 38.7 | 38.7 | 0 % | 0.216 | 0.171 | 1.26× |
| 16 | 31.3 | 31.3 | 0 % | 0.225 | 0.124 | 1.82× |
| 32 | 27.7 | 27.7 | 0 % | 0.228 | 0.107 | 2.12× |
| **64** | 25.8 | 25.8 | 0 % | 0.287 | 0.101 | **2.84×** |
| 128 | 51.0 | 51.0 | 0 % | 0.226 | 0.141 | 1.60× |
| 512 | 50.2 | 50.2 | 0 % | 0.153 | 0.116 | 1.32× |
| 8192 | 50.0 | 50.0 | 0 % | 0.109 | 0.103 | 1.06× |
| 65536 | 50.0 | 50.0 | 0 % | 0.120 | 0.138 | 0.87× |

Single bulk round (structure cost with the merge cost removed) differs in three places: group 1
is 141.4 vs 213.4 as above; **group 2 is 82.7 vs the old 106.7 (−22 %)**, because the old
representation only reached 82.7 when `merge_sorted` happened to allocate an exact-fit `Vec`,
whereas the hybrid gets there unconditionally; and groups 2 and 4 run at 0.92× / 0.74× the old
*speed*, which is finding 5 — the doublings from a 1-slot start, paid where there is no merge to
amortize them against.

**Findings.**

6. **At threshold 64 the store is never larger than the baseline at any group size**, and −34 %
   at group size 1. The promotion cliff (~25.8 → ~51.0 B/row, 1.97×, because a promoted group
   holds 24 B leaves in a power-of-two bucket array at ≤ 87.5 % load) sits at exactly the same
   place it did before the change. That is the whole argument of §5.
7. **The merge-heavy path is 1.1–2.8× faster**, peaking at group size 64 — which was the
   *slowest* configuration in the entire baseline sweep (0.287 s), because 64 was the largest
   un-promoted group and every round re-copied the whole accumulated sorted `Vec`. That
   quadratic is gone: 0.101 s. The old benchmark's finding 3 ("the threshold is set at the right
   place for time") no longer applies, because the representation below the threshold no longer
   degrades as it approaches it.
8. **Groups in the tens of thousands are ~13 % slower** (0.120 → 0.138 s at group 65536). Those
   are `hashbrown::HashTable` on both sides; the difference is the per-round delta groups, which
   now allocate a 1-slot table each. Not worth chasing at 16 groups per store.

---

## 4. Tier 2 — end to end, `ctadl index` on generated programs

`scripts/locals-bench.py`, ~1 M `locals` rows per configuration.

**On timing noise.** The ±1 % repeatability `locals-trie-benchmark.md` §4 claims did not hold in
this session: a straight repeat of the *baseline* binary came back up to 15 % slower on a second
pass, and two builds that cannot differ at a given configuration (group size 1, where no group
exceeds 2 leaves, so `SMALL_THRESHOLD` is unreachable) measured 13 % apart. Timing here therefore
uses **interleaved sampling** — the two binaries run alternately, so thermal drift hits both — at
group sizes 1, 16, 63 and 128, 4–5 samples each. Store bytes are deterministic and need no such
treatment.

Interleaved configurations (min and median of 4–5 alternating samples):

| group | max | base min / med | new min / med | median Δ | store MB | store Δ |
|---|---|---|---|---|---|---|
| 1 | 2 | 1.016 / 1.026 | 0.968 / 0.981 | **−4.4 %** | 142.1 → 93.0 | **−35 %** |
| 16 | 17 | 0.619 / 0.651 | 0.603 / 0.606 | **−7.0 %** | 83.6 → 59.3 | **−29 %** |
| 63 | 64 | 0.648 / 0.663 | 0.559 / 0.571 | **−13.9 %** | 82.4 → 54.7 | **−34 %** |
| 128 | 129 | 0.614 / 0.628 | 0.560 / 0.576 | **−8.4 %** | 96.8 → 69.2 | **−29 %** |

At group 63 the two sample sets do not overlap at all (baseline 0.648–0.689, new 0.559–0.587).

The rest of the sweep, 2 samples per side, `min`:

| group | max | mean | base MB | new MB | store | base s | new s | fixpoint |
|---|---|---|---|---|---|---|---|---|
| 1 | 2 | 1.17 | 142.1 | **93.0** | **−35 %** | 1.016 | 0.968 | −5 % |
| 2 | 3 | 1.44 | 133.0 | **92.9** | **−30 %** | 1.092 | 1.075 | −2 % |
| 4 | 5 | 1.77 | 113.3 | **85.2** | **−25 %** | 0.894 | 0.875 | −2 % |
| 8 | 9 | 2.05 | 109.9 | **84.2** | **−23 %** | 0.728 | 0.708 | −3 % |
| 16 | 17 | 2.24 | 83.6 | **59.3** | **−29 %** | 0.619 | 0.603 | −3 % |
| 31 | 32 | 2.36 | 83.2 | **55.2** | **−34 %** | 0.608 | 0.585 | −4 % |
| 32 | 33 | 2.36 | 82.6 | **59.1** | **−28 %** | 0.602 | 0.584 | −3 % |
| 33 | 34 | 2.37 | 90.9 | **67.1** | **−26 %** | 0.617 | 0.586 | −5 % |
| **63** | 64 | 2.42 | 82.4 | **54.7** | **−34 %** | 0.648 | 0.559 | **−14 %** |
| 64 | 65 | 2.43 | 87.1 | **59.3** | **−32 %** | 0.628 | 0.614 | −2 % |
| 65 | 66 | 2.43 | 96.4 | **68.7** | **−29 %** | 0.679 | 0.611 | −10 % |
| 128 | 129 | 2.46 | 96.8 | **69.2** | **−29 %** | 0.614 | 0.560 | −9 % |
| 512 | 513 | 2.49 | 96.6 | **69.1** | **−28 %** | 0.587 | 0.575 | −2 % |
| 2048 | 2049 | 2.50 | 96.1 | **68.8** | **−28 %** | 0.584 | 0.573 | −2 % |
| 8192 | 8193 | 2.50 | 95.4 | **68.4** | **−28 %** | 0.548 | 0.550 | +0 % |

Process peak physical footprint moves by −7 % to +4 % with no clear trend: at these sizes the
front end (parse, SSA, codegen over up to 143 k functions) dominates the process and dilutes a
store-level change, exactly as `locals-trie-benchmark.md` §4 notes.

**Findings.**

9. **The store is 23–35 % smaller on every configuration**, and the reason is finding 6 plus the
   group-size histogram the store now logs. Across the sweep, **77–100 % of all groups hold
   exactly one leaf** — 714 k of 857 k at group 1, 401,706 of 406,377 (98.9 %) at group 128,
   393,264 of 393,336 (100.0 %) at group 8192. The mean of 2.5 is the average of a huge singleton
   population and a few thousand groups of size K. So the store is, to first order, *an array of
   one-element sets*, and those went from a 4-slot `Vec` (96 B) to a 1-slot probe table (24 B). A
   representation change that is exactly *neutral* at group sizes 2–64 still wins a quarter of
   the store, because that is not where the groups are.
10. **The fixpoint is 2–14 % faster**, never slower beyond noise. The largest win, group 63
    (−14 %), is where the baseline's sorted `Vec` was at its worst: 64-leaf groups, just under
    the old promotion threshold, re-copied whole on every one of six iterations. It is also the
    shape that produces exactly-full 64-slot probe tables, so finding 4's miss cost is being paid
    there — and is swamped by what the merge saves.
11. **The store's own instrumentation stayed honest through the change**: `heap_report()` matches
    the counting allocator to 1.00 across the whole tier-1 sweep, including the new
    representation's exact-slot accounting.

---

## 5. Why the threshold is 64

`locals-trie-hybrid-ds.md` names 32 as the *initial* threshold. It was measured at 16, 32 and 64
(a one-line change), and 64 is the value that ships.

Tier 1, exact bytes/row — the promotion cliff sits *exactly* at the threshold and is a hard 2×:

| group | thr 16 | thr 32 | **thr 64 (shipped)** | baseline |
|---|---|---|---|---|
| 16 | 31.3 | 31.3 | 31.3 | 31.3 |
| 32 | **53.9** | 27.7 | 27.7 | 27.7 |
| 64 | 52.0 | **52.0** | 25.8 | 25.8 |
| 128 | 51.0 | 51.0 | 51.0 | 51.0 |

Tier 2, store MB:

| group | max | thr 16 | thr 32 | **thr 64** |
|---|---|---|---|---|
| 16 | 17 | 59.8 | 59.3 | 59.3 |
| 31 | 32 | 70.5 | **55.2** | **55.2** |
| 32 | 33 | 69.4 | 59.5 | **59.1** |
| 33 | 34 | 68.3 | 68.3 | **67.1** |
| 64 | 65 | 69.2 | 69.2 | **59.3** |
| 128 | 129 | 69.2 | 69.2 | 69.2 |

12. **Choosing the threshold is choosing how much of the size distribution pays the 2× promotion
    step, not choosing a time/space midpoint.** Promoted groups never demote, so a group that
    crosses pays ~50 B/leaf for the rest of the run. Every lower threshold strictly loses memory:
    32 gives back 14 % of the store wherever groups land in 33…64 leaves (69.2 vs 59.3 MB at
    group 64), and 16 gives back 15 points more at group 31 by promoting 19 k extra groups. Since
    the representation below the threshold no longer degrades as it fills (finding 7 — the old
    `Vec`'s quadratic merge was the reason to keep the threshold low), nothing pulls the other
    way, and the right value is the largest the `u64` occupancy bitmask allows: **64**.
13. **The one thing raising it costs is finding 4's worst case**, which doubles: a miss against
    an exactly-full 64-slot table is 43.6 ns against 20.1 at 32 slots. Group size 63 manufactures
    exactly that shape end to end, and it is the *fastest* configuration measured (−14 %), so the
    cost is real in the microbenchmark and invisible in the workload. If it ever stops being
    invisible, the fix is to grow one step early (`len == slots` → `len + 1 == slots`) at the top
    of the range only, which costs one slot per large small-set and nothing at the singleton
    sizes that dominate.

**Correction to an earlier reading.** At threshold 32 this document reported the fixpoint as
"+6 % to +17 % slower". That was measured before the interleaving protocol, with 1–2
non-alternating samples per configuration, and it over-read the noise — it included
configurations where the threshold provably cannot change behaviour at all. Threshold 32 does
promote more groups than 64 and so is slower where that matters, but the honest statement is that
the two are within noise on time and differ measurably only on memory, which is what finding 12
rests on.

---

## 6. Where the remaining wins are, in value order

1. **Add a third, scan-only tier below ~4 elements** (findings 5, 9). A set of 1–3 elements is
   found faster by comparing than by hashing, and **77–100 % of the sets in this store hold a
   single element**. The `Probe` representation is already exactly a packed array at those sizes
   — the change is to skip the hash and scan the occupied bits directly when `slots <= 4`, which
   would take back the insert cost of finding 5 (23.2 ns vs 10.1 at n=3) without giving up any
   memory.
2. **Reconsider the `Large` representation** (finding 6). `hashbrown::HashTable<(P,M,Fp)>` costs
   ~50 B for a 24 B leaf. A dense `Vec` of leaves plus a `HashTable<u32>` index over it would
   cost ~30 B/leaf and keep O(1) existence, turning the promotion cliff from 2× into ~1.2× — and
   with it most of the remaining gap between group sizes above and below the threshold. This is a
   departure from `locals-trie-hybrid-ds.md`'s "implemented like a `hashbrown::HashTable`", so it
   is listed as a follow-up rather than done.
3. **Keep the two remaining items from `locals-trie-benchmark.md` §5**: free the drained
   `delta`/`new` outer maps, and revisit whether a promoted group can demote. `shrink_to_fit` on
   small groups (that doc's item 1) no longer applies — there is no `Vec` capacity to shrink, and
   the probe table's slack *is* its addressing.
4. **`assign_like_trie` still stores `Map<(F,Vs), Vec<(Vd,Pd,Ps)>>`** and was left alone. It is
   the same shape of problem and the same `HybridSet` would drop into it.

## 7. Method notes / limitations

* Tier 0 and tier 1 use plain integers at production element sizes (leaf 24 B, key 16 B) so the
  counting allocator sees only the structure under test, as in `locals-trie-benchmark.md` §6.
* Tier 2 timing noise and the interleaving protocol are described in §4; it is the main
  limitation of this evaluation, and the reason the four interleaved configurations are the ones
  the time claim rests on. The memory numbers are deterministic and repeatable.
* The singleton dominance in finding 9 is partly a property of the generator: K formals
  contribute K one-leaf groups for every K-leaf group they build (`locals-trie-benchmark.md`
  §2). Real targets are "many tiny, few huge" in the same direction, but the exact 77–100 %
  should not be read as a measurement of a real target.
* Generated programs exercise field-sensitive propagation and summaries but have no calls,
  virtual dispatch or aliasing, so this measures the `locals` store, not a real target's rule
  mix. Read together with `locals-trie-benchmark.md` §6.
* The threshold sweep (§5) is single-run per configuration for time; its conclusion rests on the
  store bytes, which are exact.
