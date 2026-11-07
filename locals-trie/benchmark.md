# The `locals` store, `main` → this branch: baseline benchmark — DO-NOT-MERGE

What this branch does to the `locals` BYODS store
(`ctadl-ascent/src/index_engine/locals_trie.rs`), measured against `main` at three tiers: the
set structure on its own, the store driven exactly as Ascent's semi-naive loop drives it, and
`ctadl index` end to end on generated programs — plus process memory.

Everything below was **re-measured after rebasing this branch onto `e27e1466`**, in one session on
one machine: Apple M1 Ultra (20 cores, 128 GB), macOS 26.6 (arm64), rustc 1.94.1, hashbrown
0.16.1, `--release` (`lto = "thin"`). Where a number moved from the pre-rebase measurement, §12
says so; the store's bytes did not move at all.

---

## 1. What is compared

This branch is now rebased onto `e27e1466` ("Add human output", #90), so main and the fork point
are the same commit. Main advanced `6da40f4` → `e27e1466` since the previous measurement, and

```
git diff 6da40f4 e27e1466 -- ctadl-ascent/src/index_engine/
```

is **no longer empty**, as it was then. But every line of it swaps a `log::info!` for a
`log::debug!` in `mod.rs`, 22 times over. The Ascent program, the rules and the store are
byte-identical, so "main's `locals` store" is still well defined, and it is still the
sorted-`Vec`-then-`HashSet` one. What that change does break is the tier-2 harness, which read
those log lines at INFO (§11).

| | `main` (`e27e1466`) | this branch (`6547f3d0`) |
|---|---|---|
| a `(F,V)` group | `enum Group { Small(sorted Vec<(P,M,Fp)>), Large(hashbrown::HashSet) }` | one `HybridSet<(P,M,Fp)>` |
| below 64 leaves | sorted `Vec`, binary search, merge re-copies the group | linear-probe table over bare slots + a `u64` occupancy word, O(delta) merge |
| above 64 leaves | `hashbrown::HashSet` | a Swiss table written in this crate (`hybrid_set/swiss.rs`) |
| the group struct | 32 B (enum over `Vec` 24 B and `HashSet` 32 B) | **16 B** (`{ptr, cap, len}`, regime read off `cap`) |
| outer map entry `((F,V), Group)` | 48 B | **32 B** |
| representation switch | a tagged union, two of every impl | one structure, no discriminant |

### The spec this implements

Carried over from the design note that drove the work, so the constraints the code is written
against stay recorded:

> Implement a custom hybrid data structure to support a Datalog index on a key `K` with
> associated record values `V` — i.e. like a `Map<K, Set<V>>`. In real workloads the number of
> values per `K` varies across keys: some keys have a couple of values, some have thousands. The
> map needs to support the Ascent traits but the basic operations are `insert`, `contains`,
> `get_all_values_matching_key`, and `a.merge(b)`.
>
> Focusing on the `Set<V>`: a set that, below a threshold of records, behaves like a linear-probe
> hashtable, and above it is implemented like a `hashbrown::HashTable` — **do not** actually use
> the `HashTable` type or any built-in hashtable; implement a custom one modelled after
> hashbrown. Initial threshold 64. Organize it so that transitioning the threshold is efficient.

The "no built-in hashtable" clause is why `swiss.rs` exists rather than a `hashbrown::HashTable`
field, and §4 measures what that cost or bought.

### The three binaries

Time and memory comparisons need binaries that exist in git. Three were built, one `git worktree`
each, private `CARGO_TARGET_DIR`, same toolchain and profile:

| name | commit | what it is |
|---|---|---|
| `maint` | `e27e1466` | **main itself.** No bench harness, and its `heap_report` predates the estimator fix (§8), so it is used for end-to-end *time* and *process memory*, which no estimator touches. |
| `maini` | `ee9b4c71` | main's store plus the two things needed to measure it: the bench harnesses and the exact `hb_bytes` (§8). It is `e27e1466` with the two instrumentation commits cherry-picked on top, and is tagged `bench/main-instrumented` so this stays reproducible; `git diff e27e1466 bench/main-instrumented -- ctadl-ascent/src/index_engine/` touches nothing but instrumentation. Every **store-byte** number attributed to main comes from here. |
| `head` | `6547f3d0` | this branch. |

(These were `main6` / `main` / `head` before the rebase, at `6da40f4` / `9fd4c47` / `5286e37`.)

`maint` and `maini` are run side by side end to end in §6–§7, which is what licenses the
substitution: they agree on fixpoint time and process memory to within the run-to-run spread, and
their store estimates differ only by the estimator fix.

---

## 2. Headline

* **Store bytes.** Exactly measured by a counting allocator, the store is **45 % smaller at one
  leaf per group**, falling to parity once groups are large enough that their leaves dominate
  (§5). End to end on generated programs it is **33–41 % smaller at every configuration
  measured** (§6), because a real group distribution is singleton-heavy whatever its maximum.
* **Process memory.** Peak RSS is **1.7–11.3 %** below main's at 12 of the 13 end-to-end
  configurations — and **2.3 % above** at group size 1, which is the one regression this
  re-measurement found (§7 finding 18a). That gap sits entirely in front-end parquet decoding,
  before the store holds anything: it comes from a bimodal allocation that all three binaries
  exhibit, and head drew the expensive mode in every run. Where the peak is contaminated this way
  — group sizes 1 and 2 — read the mode-robust statistic instead: the footprint grown across the
  fixpoint is **17.8–51.8 %** below main's at all four configurations where it can be computed,
  9 of 9 paired passes each (§7 finding 19).
* **Time.** At store level this branch is faster at every group size from 1 to 4096 leaves, by
  **6.1 % to 63 %**, winning **9 of 9** paired passes at 9 of those 13 sizes and 6–8 of 9 at the
  rest; at 8192 leaves and above the two are level (§5). End to end the effect is much smaller
  and only partly separable from noise: **−3.0 %, −5.7 % and −7.1 %** at group sizes 1, 2 and 64
  over 9 paired passes (8/9, 9/9, 8/9), and a wash at 32 and 128 (§6.1). The store is a component
  of the index phase, not the whole of it.
* **The set structure.** Below the threshold it holds 24 B/element like main's sorted `Vec` but
  inserts **1.9–3.6× faster** from 16 elements up (and 1.1–2.4× *slower* at 2–5, where a `Vec`
  push is a memcpy); above it, the from-scratch Swiss table allocates the same bytes as hashbrown
  at every size ≥ 5 elements (§4).
* **The structural regression** is a miss against an exactly-full small table, which degenerates
  to a scan: 44.8 ns at 64 slots against ~6 ns for main's binary search (§4 finding 4). It is
  bounded, it is the price of load factor 1.0, and the workload that manufactures it is among the
  fastest configurations measured (§9).

---

## 3. What was built, and how to run it

| file | role |
|---|---|
| `ctadl-ascent/src/index_engine/hybrid_set.rs` | `HybridSet<T, S>` — the set; 13 unit tests |
| `ctadl-ascent/src/index_engine/hybrid_set/raw.rs` | `RawTable<T>` — the one structure both regimes share |
| `ctadl-ascent/src/index_engine/hybrid_set/swiss.rs` | the from-scratch Swiss table's probing/sizing rules; 3 unit tests |
| `ctadl-ascent/src/index_engine/locals_trie.rs` | `hb_buckets` / `hb_bytes` / `HB_GROUP_WIDTH` — the shared, exact hashbrown-size estimator (§8) |
| `ctadl-ascent/benches/hybrid_set.rs` | **tier 0** — five set representations under a counting allocator |
| `ctadl-ascent/benches/locals_trie.rs` | **tier 1** — the whole store, driven as Ascent drives it, under a counting allocator |
| `scripts/gen-locals-bench.py` | generates Flowy (`.tnt`) programs with a chosen `(F,V)` group size, path count and function count |
| `scripts/locals-bench.py` | **tier 2** — generate → `ctadl import` → `ctadl index`, parsing store bytes, fixpoint time, peak footprint and peak RSS |
| `HeapReport` additions | `max_group`, `large_groups`, `group_hist` — the store logs its own group-size distribution, which is how the harness verifies the generator hit its target shape |

`cargo test -p ctadl-ascent --lib` is green in both profiles (254 tests, 5 ignored, 18 of them in
`index_engine`: 13 for `HybridSet`, 3 for the Swiss layer, 2 for the store's views — the count
rose from 198 because the rebase brought main's new tests, not this branch's), and
`cargo clippy --all-targets -p ctadl-ascent` is clean under the workspace's
`undocumented_unsafe_blocks = "deny"`.

```bash
cargo build --release
cargo bench -p ctadl-ascent --bench hybrid_set    # tier 0   (add `-- --tsv` for machine-readable)
cargo bench -p ctadl-ascent --bench locals_trie   # tier 1
scripts/locals-bench.py                           # tier 2, default sweep, ~1M rows/config
scripts/locals-bench.py --rows 200000 --group-sizes 4,64,65 --out r.tsv
```

### How the generator controls group size

The engine seeds `locals(f, ai, ε, i, ε)` per formal and propagates along assignments, so a
variable reached by K distinct formals ends up with a K-leaf `(F,V)` group. Each generated
function takes K parameters, funnels them into one variable, and stores them into one object over
`--paths` distinct fields:

```
def bench0(a0, a1, a2, a3, a4, a5) : 1 {
start:
  c0 = a0, a2, a4;      // Flowy multi-source assign
  obj.p0 = c0;
  c1 = a1, a3, a5;
  obj.p1 = c1;
  out = obj;
  return out;
}
```

That yields ~5K rows per function: K singleton groups (each formal reaches only itself) plus ~4
groups of K leaves (`obj`, `out`, the return port, the summary). `max_group` in the log confirms
the target was hit. The mix — **many tiny groups plus a few large ones** — is the regime the
module is designed for, and it is why the *mean* group size stays ~2.5 even when `max_group` is
8193.

---

## 4. Tier 0 — the set on its own

`cargo bench -p ctadl-ascent --bench hybrid_set`, median of 5 passes. 24 B elements, 1 M elements
per row spread over `1 M / n` sets, bytes from a counting global allocator. `hybrid` is this
branch's `HybridSet`; `vec64` is main's representation (sorted `Vec` under 64, `HashSet` above);
`hash` is `hashbrown::HashSet` at every size.

**B/elem is the set's own allocation only** — the 16 B (main: 32 B) struct lives in the enclosing
map entry and is charged in §5, not here. So this table understates the change: it shows the +8 B
occupancy word this branch moved *into* the allocation and not the −16 B it took out of the map
entry.

| n | B/elem hybrid / vec64 / hash | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | **32.0** / 96.0 / 108.0 | **17.7** / 25.0 / 25.5 | **2.7** / 4.7 / 6.4 | **2.0** / 2.8 / 3.2 | **19.2** / 23.0 / 56.3 |
| 2 | **28.0** / 48.0 / 54.0 | 23.6 / **11.3** / 12.2 | 3.6 / 3.2 / **2.8** | 3.1 / 3.0 / **1.9** | **37.2** / 37.9 / 49.3 |
| 3 | 34.7 / **32.0** / 36.0 | 24.1 / 10.1 / **9.4** | 3.3 / 4.5 / **2.7** | 3.5 / 3.8 / **1.7** | **45.0** / 46.4 / 46.5 |
| 5 | 40.0 / **38.4** / 41.6 | 25.8 / **23.6** / 24.9 | 4.6 / 7.5 / **2.7** | **3.1** / 3.9 / 4.4 | **47.7** / 56.9 / 55.7 |
| 8 | 25.0 / **24.0** / 51.0 | **17.7** / 24.9 / 32.4 | **2.9** / 6.6 / 3.9 | 5.2 / **3.8** / 5.2 | **46.5** / 65.7 / 64.3 |
| 16 | 24.5 / **24.0** / 50.5 | **12.8** / 25.6 / 31.5 | **3.0** / 8.8 / 3.7 | 10.5 / 6.1 / **3.1** | **43.3** / 64.0 / 50.6 |
| 24 | 32.3 / **32.0** / 33.7 | **13.2** / 38.6 / 21.4 | **2.6** / 11.8 / 2.7 | **2.9** / 5.9 / 4.2 | 45.0 / 46.8 / **42.7** |
| 31 | 25.0 / **24.8** / 51.9 | **12.2** / 30.9 / 29.1 | **4.6** / 11.5 / 5.1 | 11.2 / 4.8 / **2.8** | **37.4** / 47.8 / 59.7 |
| 32 | 24.2 / **24.0** / 50.2 | **11.7** / 28.9 / 25.3 | 4.4 / 11.4 / **3.8** | 21.1 / 4.9 / **3.2** | **37.4** / 47.9 / 52.5 |
| 33 | 46.8 / **46.5** / 48.7 | **19.0** / 35.1 / 24.3 | **3.5** / 15.5 / 3.6 | 3.1 / 8.5 / **2.9** | **39.5** / 51.1 / 48.8 |
| 40 | 38.6 / **38.4** / 40.2 | **13.2** / 36.8 / 23.1 | **3.0** / 13.8 / 3.0 | 2.9 / 7.3 / **2.4** | **38.4** / 55.7 / 51.4 |
| 48 | 32.2 / **32.0** / 33.5 | **10.9** / 39.3 / 15.7 | **2.6** / 14.2 / 2.9 | **2.7** / 6.6 / 4.1 | **34.3** / 50.2 / 37.9 |
| 64 | 24.1 / **24.0** / 50.1 | **10.0** / 35.1 / 23.1 | **3.9** / 13.8 / 4.6 | 44.8 / 6.3 / **2.3** | **26.8** / 48.8 / 58.3 |
| 65 | **49.4 / 49.4 / 49.4** | **15.9** / 45.1 / 18.0 | 3.8 / **3.0 / 3.0** | 2.7 / 2.5 / **2.4** | **36.9** / 60.8 / 49.1 |
| 128 | **50.1 / 50.1 / 50.1** | 18.0 / 28.3 / **16.1** | 5.2 / **3.0 / 3.0** | **3.4** / 3.8 / 3.5 | **28.0** / 40.8 / 43.7 |
| 1024 | **50.0 / 50.0 / 50.0** | 15.4 / 15.3 / **13.4** | 8.4 / 6.6 / **6.3** | **2.7** / 3.4 / 2.8 | **26.4** / 41.6 / 31.6 |

The Swiss table on its own (`SMALL = 0`, i.e. the large half used at every size) against
hashbrown, which is the comparison the spec's "implement a custom one" clause invites:

| n | B/elem swiss / hash | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | 208.0 / **108.0** | 33.2 / **25.5** | 19.2 / **6.4** | 11.6 / **3.2** | 65.7 / **56.3** |
| 2 | 104.0 / **54.0** | 20.0 / **12.2** | 14.4 / **2.8** | 9.4 / **1.9** | **42.4** / 49.3 |
| 3 | 69.3 / **36.0** | **8.6** / 9.4 | 3.0 / **2.7** | 8.1 / **1.7** | **40.6** / 46.5 |
| 5 | **41.6 / 41.6** | **7.3** / 24.9 | 2.9 / **2.7** | 5.9 / **4.4** | **40.4** / 55.7 |
| 8 | **51.0 / 51.0** | **17.4** / 32.4 | 4.1 / **3.9** | **4.7** / 5.2 | **52.3** / 64.3 |
| 16 | **50.5 / 50.5** | **18.1** / 31.5 | 4.1 / **3.7** | 3.4 / **3.1** | **37.4** / 50.6 |
| 24 | **33.7 / 33.7** | **14.1** / 21.4 | 2.9 / **2.7** | **3.5** / 4.2 | **31.1** / 42.7 |
| 32 | **50.2 / 50.2** | **19.7** / 25.3 | **3.7** / 3.8 | 3.5 / **3.2** | **34.8** / 52.5 |
| 48 | **33.5 / 33.5** | **10.8** / 15.7 | 2.9 / **2.9** | 4.2 / **4.1** | **27.2** / 37.9 |
| 64 | **50.1 / 50.1** | **17.8** / 23.1 | 4.7 / **4.6** | 2.6 / **2.3** | **38.7** / 58.3 |
| 128 | **50.1 / 50.1** | 16.9 / **16.1** | 4.6 / **3.0** | **3.5** / 3.5 | **32.1** / 43.7 |
| 1024 | **50.0 / 50.0** | **12.7** / 13.4 | 7.8 / **6.3** | 2.8 / **2.8** | **28.3** / 31.6 |

**Findings.**

1. **Below the threshold the hybrid matches main's `Vec` on bytes and pulls away from it on
   insert as the group fills.** 24.1 B/element against 24.0 at 64 elements, 24.2 against 24.0 at
   32 — the occupancy word amortizes away — while insert is 10.0 ns against 35.1 and merge 26.8
   against 48.8. Main's `Vec` gets *worse* as the group fills (24.9 → 25.6 → 28.9 → 35.1 ns per
   insert from 8 to 64 elements) because `Vec::insert` shifts; the probe table gets *better*
   (17.7 → 12.8 → 11.7 → 10.0) because more slots mean fewer collisions. That divergence is the
   whole of §5's time result. **At 2–5 elements it goes the other way** — 23.6 ns against the
   `Vec`'s 11.3 at n=2 — because a `Vec` push into spare capacity is a memcpy and the probe table
   still hashes. §10 item 3 is the fix; §5 and §6 show the crossover is well below where the
   store actually lives, since a store's small sets are built by *merge*, not by repeated insert,
   and merge is 1.0–1.8× faster at every size measured.
2. **Against hashbrown below the threshold the win is memory, 2.1–4.5×**: 24–32 B/element where
   hashbrown spends 50–108, because a power-of-two bucket array at ≤ 87.5 % load cannot hold two
   elements cheaply. That is the reason the hybrid exists.
3. **The from-scratch table is hashbrown's layout.** From 5 elements up the two allocate
   *identical* bytes at every size in the sweep — 41.6, 51.0, 50.5, 33.7, 50.2, 33.5, 50.1, 50.0
   B/element — which is what a unit test checks against a live `hashbrown::HashSet` over 2000
   inserts. It inserts up to 3.4× faster and merges 1.1–1.5× faster (no `growth_left` to
   maintain, no tombstone handling, equality on whole elements rather than through a closure);
   hashbrown's only wins above the floor are insert at n=128 (16.1 vs 16.9 ns) and n=31 (29.1 vs
   31.2), and lookups agree to within 1.6 ns. Below 5 elements the from-scratch table is 2×
   *worse* on bytes, because its
   bucket floor is 8 rather than 4 — a deliberate simplification, in a range the hybrid never
   gives it.
4. **The known regression: a miss against an exactly-full small table degenerates to a scan** —
   10.5 ns at 16 slots, 21.1 at 32, **44.8 at 64** — against ~5–6 ns for main's binary search and
   ~2.5 ns for either hash table. Load factor 1.0 is what buys the 24 B/element. §9 is the
   argument that the workload does not pay it.
5. **Promotion costs about one insert of latency at the boundary**: at n=65 the hybrid inserts in
   15.9 ns against the raw Swiss table's 14.2, because reaching 65 means filling a 64-slot probe
   table and then moving it. By n=1024 it is the raw table plus noise.

---

## 5. Tier 1 — the store, exact bytes and time (counting allocator)

`cargo bench -p ctadl-ascent --bench locals_trie` from the `main` and `head` bench binaries,
interleaved, **9 passes**, order rotated each pass. 1 M rows in every row of the table; group size
varies and group count varies inversely. Byte columns were **bit-identical across all 9 passes**
for both binaries and are reported as single values; times are medians. `total B/row` is the
`total` store alone, `+delta B/row` includes the `delta`/`new` copies Ascent holds at fixpoint.

Semi-naive shape, one new leaf per group per iteration (pessimal for the delta→total merge):

| group | groups | main B/row | head B/row | Δ bytes | +delta main | +delta head | main s | head s | Δ time | paired ratio | head wins |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 1048576 | 213.4 | **117.4** | **−45.0 %** | 312.7 | 184.7 | 0.191 | **0.157** | −17.7 % | 0.832 | 9/9 |
| 2 | 524288 | 82.7 | **70.7** | −14.5 % | 182.0 | 138.0 | 0.217 | **0.176** | −18.7 % | 0.816 | 9/9 |
| 4 | 262144 | 53.4 | **47.4** | −11.2 % | 103.0 | 81.0 | 0.209 | **0.161** | −23.0 % | 0.758 | 9/9 |
| 8 | 131072 | 38.7 | **35.7** | −7.8 % | 63.5 | 52.5 | 0.194 | **0.141** | −27.4 % | 0.685 | 9/9 |
| 16 | 65536 | 31.3 | **29.8** | −4.8 % | 43.7 | 38.2 | 0.206 | **0.114** | −44.7 % | 0.548 | 9/9 |
| 32 | 32768 | 27.7 | **26.9** | −2.7 % | 33.9 | 31.1 | 0.219 | **0.106** | −51.6 % | 0.478 | 9/9 |
| **64** | 16384 | 25.8 | **25.5** | −1.5 % | 28.9 | 27.6 | 0.276 | **0.103** | **−62.7 %** | **0.377** | 9/9 |
| 128 | 8192 | 51.0 | **50.7** | −0.5 % | 52.5 | 51.8 | 0.218 | **0.125** | −42.7 % | 0.576 | 9/9 |
| 256 | 4096 | 50.5 | **50.4** | −0.2 % | 51.3 | 50.9 | 0.178 | **0.117** | −34.1 % | 0.677 | 9/9 |
| 512 | 2048 | 50.2 | 50.2 | −0.1 % | 50.6 | 50.4 | 0.138 | **0.111** | −19.6 % | 0.790 | 8/9 |
| 1024 | 1024 | 50.1 | 50.1 | −0.1 % | 50.3 | 50.2 | 0.123 | **0.108** | −12.7 % | 0.865 | 8/9 |
| 2048 | 512 | 50.1 | 50.0 | −0.0 % | 50.2 | 50.1 | 0.113 | **0.104** | −7.5 % | 0.935 | 8/9 |
| 4096 | 256 | 50.0 | 50.0 | −0.0 % | 50.1 | 50.1 | 0.106 | **0.099** | −6.1 % | 0.946 | 6/9 |
| 8192 | 128 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.098 | 0.097 | −1.0 % | 0.952 | 5/9 |
| 16384 | 64 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | **0.092** | 0.096 | +3.8 % | 1.007 | 3/9 |
| 32768 | 32 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.100 | 0.099 | −1.3 % | 1.004 | 3/9 |
| 65536 | 16 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.109 | 0.112 | +2.9 % | 1.007 | 3/9 |

`heap_report()` matches the allocator to **1.00 for both binaries at every group size**, so the
estimate logged by every real index run is trustworthy at whole-store granularity.

**Findings.**

6. **The byte saving is one line of arithmetic and it is concentrated where groups are small.**
   The outer map entry went 48 B → 32 B and hashbrown holds ~2 buckets per entry, so the map
   gives back 32 B/group; a small set pays +8 B for the occupancy word now inside its own
   allocation; net −24 B per group, i.e. −24/group_size B/row. Predicted −12.0 / −6.0 / −3.0 /
   −1.5 / −0.75 / −0.375 at group sizes 2…64; measured −12.0 / −6.0 / −3.0 / −1.5 / −0.8 / −0.3.
   Above the threshold the set term vanishes and only the map's term survives: −0.3 B/row at
   group 128, nothing by group 512. Group size 1 is off this line — it is −96.0 B/row, four times
   the rule — for the reason in finding 7.
7. **At one leaf per group the saving is 45 %, and two-thirds of it is `Vec`'s minimum capacity,
   not the hash table.** Rust's `RawVec` allocates 4 slots for the first push of a 24 B element,
   so a one-leaf group costs **96 B** in main's `Vec` against 32 B in a probe table (tier 0
   measures exactly that: 96.0 vs 32.0 B/elem at n=1). That is 64 of the 96 B/row; the map entry
   is the other 32. From group size 2 up, main's sorted `Vec` was already 24 B/leaf, so only the
   entry term is left, which is finding 6.
8. **Time: this branch is ahead at every group size from 1 to 4096**, winning 9 of 9 paired
   passes at 9 of those 13 sizes and 6–8 of 9 at the rest, by 6.1 % at 4096 leaves up to **63 %
   at group size 64**. The peak is exactly where main is worst: group size 64 is the largest
   *un-promoted* sorted `Vec`, so every insert shifts up to 63 leaves and every delta→total merge
   re-copies the whole group. Both costs are gone. From 8192 leaves per group up the two are
   level (ratios 0.95–1.01, 3–5 of 9 passes) — there main was already a `HashSet` and this is the
   Swiss table against hashbrown, which §4 finding 3 says is a wash.
9. **The `delta`/`new` copies still cost more than the store.** `+delta` is 184.7 B/row against
   a `total` of 117.4 at group size 1 — Ascent's delta and new relations hold 1.6× the
   steady-state store — because `absorb` empties them with `fwd.drain()`, which does not free a
   hashbrown table. Whatever the widest iteration reached is held to the end. This is untouched
   by the branch and is the largest single item left (§10).

---

## 6. Tier 2 — end to end, `ctadl index` on generated programs

`scripts/locals-bench.py`'s measurement code, ~1 M `locals` rows per configuration, **5
interleaved passes** of the whole sweep with all three binaries alternating over the same
generated programs, order rotated each pass. `store MB` is `heap_report()`; it was identical in
all 5 passes for every cell and is reported as a single value.

| group | rows | groups | max | large | `maint` MB | `maini` MB | `head` MB | head vs `maini` |
|---|---|---|---|---|---|---|---|---|
| 1 | 999999 | 857142 | 2 | 0 | 143.2 | 142.1 | **83.6** | **−41.2 %** |
| 2 | 1083329 | 749997 | 3 | 0 | 133.6 | 133.0 | **82.6** | −37.9 % |
| 4 | 1045442 | 590902 | 5 | 0 | 113.6 | 113.3 | **73.7** | −35.0 % |
| 8 | 1023787 | 499989 | 9 | 0 | 110.0 | 109.9 | **72.1** | −34.4 % |
| 16 | 1012185 | 451215 | 17 | 0 | 83.7 | 83.6 | **54.7** | −34.6 % |
| 32 | 1006036 | 425868 | 33 | 0 | 82.7 | 82.6 | **54.3** | −34.3 % |
| **63** | 1002972 | 413174 | 64 | **0** | 82.5 | 82.4 | **49.9** | **−39.4 %** |
| **64** | 1002915 | 412965 | 65 | **3105** | 87.1 | 87.1 | **54.5** | −37.4 % |
| **65** | 1003024 | 412830 | 66 | **9174** | 96.5 | 96.4 | **63.8** | −33.8 % |
| 128 | 1001151 | 406377 | 129 | 4671 | 96.8 | 96.8 | **64.2** | −33.7 % |
| 512 | 999570 | 401310 | 513 | 1170 | 96.6 | 96.6 | **64.2** | −33.5 % |
| 2048 | 993571 | 397797 | 2049 | 291 | 96.1 | 96.1 | **63.9** | −33.5 % |
| 8192 | 983112 | 393336 | 8193 | 72 | 95.4 | 95.4 | **63.4** | −33.5 % |

Fixpoint time — the wall time of the SCC holding the `locals` rules, from Ascent's own
`#![measure_rule_times]`. Medians of the same 5 passes, plus the paired per-pass ratio against
actual main:

| group | `maint` s | `maini` s | `head` s | head vs `maint` | paired ratio | head wins |
|---|---|---|---|---|---|---|
| 1 | 0.982 | 0.983 | **0.941** | −4.2 % | 0.958 | 5/5 |
| 2 | 1.036 | 1.032 | **0.979** | −5.5 % | 0.944 | 5/5 |
| 4 | **0.822** | 0.825 | 0.833 | +1.3 % | 1.002 | 2/5 |
| 8 | 0.676 | 0.673 | **0.664** | −1.9 % | 0.982 | 5/5 |
| 16 | 0.588 | 0.583 | **0.585** | −0.4 % | 0.997 | 3/5 |
| 32 | 0.575 | 0.579 | **0.566** | −1.5 % | 0.985 | 3/5 |
| 63 | 0.579 | 0.579 | **0.543** | −6.2 % | 0.937 | 5/5 |
| 64 | 0.584 | 0.585 | **0.546** | −6.7 % | 0.933 | 5/5 |
| 65 | 0.586 | 0.583 | **0.562** | −4.1 % | 0.961 | 5/5 |
| 128 | 0.564 | 0.573 | **0.551** | −2.3 % | 0.979 | 5/5 |
| 512 | 0.548 | 0.548 | **0.536** | −2.2 % | 0.979 | 4/5 |
| 2048 | 0.539 | 0.534 | **0.533** | −1.1 % | 0.987 | 4/5 |
| 8192 | 0.515 | 0.519 | **0.508** | −1.3 % | 0.986 | 4/5 |

The group-size distribution the store logs, which is what makes the byte result not decay the way
§5 says it should (`log2hist` bucket 0 is groups holding exactly one leaf):

| group | groups | single-leaf groups | share |
|---|---|---|---|
| 1 | 857142 | 714285 | 83.3 % |
| 2 | 749997 | 499998 | 66.7 % |
| 8 | 499989 | 428562 | 85.7 % |
| 32 | 425868 | 407352 | 95.7 % |
| 128 | 406377 | 401706 | 98.9 % |
| 8192 | 393336 | 393264 | 100.0 % |

**Findings.**

10. **The store is 33.5–41.2 % smaller than main's at every configuration measured.** No
    configuration goes the other way, and the range is narrow — the sweep changes `max_group` by
    four orders of magnitude and the saving moves by 8 points.
11. **`maint` and `maini` agree, which is what licenses the stand-in.** Their store estimates
    differ by 1.1 MB (0.8 %) at group size 1, 0.6 MB at group 2 and ≤ 0.3 MB everywhere else —
    the whole of main's `hb_bytes` overcount (§8), showing up exactly where the store holds the
    most tiny `fidx` tables. Their fixpoint medians and process memory sit on either side of each
    other with no consistent ordering. Every store-byte comparison above would move by less than
    a point if main's own estimator were used instead.
12. **The saving does not decay with group size the way §5 says it should**, because the
    generator's real group distribution is singleton-heavy: `mean` stays ~2.5 whatever
    `max_group` is, and 67–100 % of groups hold exactly one leaf. So the store keeps ~400 k
    groups at every configuration from group size 16 up, keeps paying ~2 outer buckets each, and
    keeps getting ~29–33 MB back. §5's decay is what happens when *every* group is large; this is
    what happens on the shape a real target has.
13. **The promotion cliff reproduces end to end, and it is smaller than main's.** Group size 63
    (max 64 leaves, zero promotions) → 49.9 MB; 64 (one promoted group per function) → 54.5;
    65 (three) → 63.8. That is +27.9 % across the cliff, against main's 82.4 → 96.4 = +17.0 %:
    the cliff is *relatively* steeper here precisely because the un-promoted side got so much
    cheaper. In absolute terms the two pay the same to cross it: 13.9 MB here, 14.0 on main.
14. **The fixpoint is faster than main's at 12 of the 13 configurations by median** — by 0.4 % to
    6.7 %, with paired ratios 0.93–1.00 and head ahead in 2–5 of 5 passes. Group size 4 goes the
    other way (+1.3 %, 2 of 5). These margins are far smaller than the pre-rebase measurement's
    −4 % to −21 %, which §6.1 had already flagged as mostly unreproducible; the 9-pass numbers
    below are what the timing claim should be.

### 6.1 Nine-pass resample of the fixpoint

Five configurations at **9 interleaved passes**, reported as the paired per-pass ratio (the ratio
taken within a pass) and the count of passes head won. This is the statistic this machine needs
(§11). The machine was quiet for this resample — the slowest sample in each column is 1.01–1.14×
its median, against 1.2–1.4× in the pre-rebase session — so the paired ratios and the medians
agree here, which they did not before.

| group | head/`maint` | head wins | head/`maini` | head wins |
|---|---|---|---|---|
| 1 | **0.970** | **8/9** | 0.971 | **9/9** |
| 2 | **0.943** | **9/9** | 0.951 | **9/9** |
| 32 | 0.986 | 6/9 | 0.983 | 7/9 |
| 64 | **0.929** | **8/9** | 0.933 | 8/9 |
| 128 | 0.972 | 7/9 | 0.978 | 7/9 |

15. **End to end, group sizes 1, 2 and 64 are a defensible timing win**: −3.0 %, −5.7 % and
    −7.1 % against actual main, 8 of 9, 9 of 9 and 8 of 9 paired passes. Group sizes 1 and 2 are
    the configurations with the most outer-map entries per leaf (857 k and 750 k groups), which is
    where §5's byte saving is largest — the same term buying time. Group size 64 is where main's
    sorted `Vec` is at its worst (§5 finding 8), the tier-1 effect showing through end to end.
16. **At group sizes 32 and 128 the fixpoint is a wash.** Median ratios sit at 0.97–0.99 and head
    wins 6–7 of 9 passes — a direction, not a figure. This is the failure mode §11 warns about,
    and it is why the sweep's timing column should not be quoted at these sizes.
17. **The tier-1 result and the tier-2 result are not in conflict.** §5 measures the store alone
    driving 1 M rows through the semi-naive loop, where the store *is* the workload and the
    margin is 5–63 %. Here the same store is one component of an index phase that also parses,
    builds SSA, runs codegen and evaluates every other relation; the fixpoint SCC is 0.5–1.1 s of
    which the `locals` store is a fraction. A large win on the component is a small win on the
    whole, and the whole is what §6.1 measures.

---

## 7. Process memory

Peak physical footprint and peak RSS of the whole `ctadl index` run, median of the 5 passes, with
the five configurations of §6.1 re-measured at 9 passes as a check.
Footprint is sampled at 20 ms; RSS is the child's own rusage high-water mark, so it needs no
sampling and is the quieter statistic. Both are whole-process numbers: the front end (parse, SSA,
codegen — large here, because the programs have up to 143 k functions) is identical in all three
binaries and dilutes the ratio, so §6's store figures are the ones to quote for the store.

| group | peak fp `maint` / `maini` / `head` MB | head vs `maint` | peak rss `maint` / `maini` / `head` MB | head vs `maint` |
|---|---|---|---|---|
| **1** | 924 / 931 / **959** | **+3.7 %** | 1458 / 1465 / **1491** | **+2.3 %** |
| 2 | 756 / 763 / **669** | −11.5 % | 1284 / 1280 / **1197** | −6.7 % |
| 4 | 668 / 707 / **635** | −5.1 % | 936 / 975 / **902** | −3.7 % |
| 8 | 599 / 598 / **528** | −11.9 % | 805 / 862 / **791** | −1.7 % |
| 16 | 470 / 480 / **452** | −3.7 % | 657 / 668 / **640** | −2.7 % |
| 32 | 491 / 488 / **479** | −2.5 % | 625 / 628 / **605** | −3.1 % |
| 63 | 359 / 381 / **304** | −15.4 % | 487 / 484 / **432** | −11.3 % |
| 64 | 364 / 364 / **292** | −19.9 % | 537 / 536 / **503** | −6.4 % |
| 65 | 414 / 414 / **380** | −8.2 % | 587 / 588 / **559** | −4.8 % |
| 128 | 385 / 380 / **360** | −6.4 % | 497 / 496 / **442** | −11.0 % |
| 512 | 374 / 374 / **320** | −14.4 % | 454 / 454 / **417** | −8.2 % |
| **2048** | 320 / 318 / 320 | **+0.1 %** | 410 / 410 / **373** | −9.0 % |
| **8192** | 295 / 295 / 301 | **+2.1 %** | 400 / 400 / **363** | −9.3 % |

**Findings.**

18. **Peak RSS is 1.7–11.3 % below main's at 12 of 13 configurations; group size 1 is 2.3 %
    *above*.** Peak footprint agrees at 10 of 13 and disagrees at three: group 1 (+3.7 %), 2048
    (+0.1 %) and 8192 (+2.1 %). The pre-rebase measurement had both statistics below main at all
    13, which is no longer true — findings 18a and 18b are what changed.
18a. **The group-size-1 regression is real and reproducible, and it is not the store.** Head's
    peak is above main's in 7 of 9 paired passes on both statistics, so it is not noise. But the
    `[mem cp]` checkpoints put the whole gap *before the store is populated*:
    head reaches "about to enter ascent_run" at 723–740 MB against main's 606–610, and the
    divergence is already there at "entry (facts loaded)", which is pure front-end parquet
    decoding — identical code in both binaries. That checkpoint is **bimodal**: over five
    interleaved fresh runs it reads either ~414 MB or ~545 MB, and **all three binaries show both
    modes** — `maint` (unmodified main) drew the high mode in 1 of 5, `maini` in 3 of 5, head in
    5 of 5. Compared within the same mode, head *wins*: ending the fixpoint at 912–932 MB against
    main's 1036–1044 in the high mode, i.e. ~120 MB lower, which is what §5 predicts. Head drew
    the expensive mode in every run measured this session; why the draw correlates with the
    binary at this configuration is not explained. Group size 2 is the same effect with the signs
    reversed — main drew the high mode in 6 of 9 passes and head in none — which is why its
    footprint reads −11.5 % over five passes and −24.8 % over nine, while its RSS reads −6.7 %
    and +1.3 % on the same two samples. Group size 2's process memory should not be quoted
    either.
18b. **At group sizes 2048 and 8192 the two statistics contradict each other** — RSS −9.0 % and
    −9.3 %, footprint +0.1 % and +2.1 % — with no bimodality in the checkpoints to explain it.
    Neither direction should be quoted at those two sizes.
19. **The mode-robust statistic still favours this branch.** The footprint *grown across the
    fixpoint* (`fp_after − fp_before`) cancels whatever mode the front end drew, because both
    endpoints are inside it. Over the nine-pass resample it is below main's at all four
    configurations whose checkpoints hold one mode throughout — **−38.0 %** at group size 1,
    −20.3 % at 32, −17.8 % at 64, **−51.8 %** at 128 — winning 9 of 9 paired passes at each. The
    fifth, group size 2, cannot be computed: main switched modes mid-sweep, so its own two
    endpoints come from different regimes. That statistic is the process-level shadow of §5's
    byte result, and it is the one to quote for the store when the peak is contaminated.
20. **The process saves more absolute bytes than the `total` store does**, which is the delta
    copies showing up. At group size 63 the store's `total` is 32.5 MB smaller and the process
    footprint is 55 MB smaller; at group size 1 the store is 58.5 MB smaller and the fixpoint's
    own growth is 117 MB smaller (finding 19 — the peak cannot be used there). The ratio is
    ~1.7–2.0×, and §5's `+delta` column is the reason: Ascent holds `delta` and `new` copies of
    the same store, they shrink by the same rule, and only the `total` is what `heap_report()`
    reports. The *percentages* still shrink at process level, because the front end is in the
    denominator and is identical in all three binaries.

---

## 8. The `hb_bytes` estimator, and hashbrown's minimum table

Both `heap_report()`s price their enclosing hashbrown maps with `locals_trie::hb_bytes`. Before
this branch that estimator was wrong in a way worth recording, because the module docs repeated
its error as a design justification.

The docs said of the pre-trie nested design:

> the two inner hash levels are nearly empty: each pays hashbrown's 8-bucket minimum allocation
> to hold ~2 elements.

**hashbrown's minimum table is 4 buckets, not 8.** From `hashbrown-0.16.1/src/raw/mod.rs:105`
(`capacity_to_buckets`), for `cap < 15`:

```rust
let min_cap = match (Group::WIDTH, table_layout.size) {
    (16, 0..=1) => 14,
    (16, 2..=3) => 7,
    (8,  0..=1) => 7,
    _ => 3,                     // every element size >= 4 B, i.e. all of ours
};
let cap = min_cap.max(cap);
let buckets = if cap < 4 { 4 } else if cap < 8 { 8 } else { 16 };
```

An element of 16/24/40 B lands on `min_cap = 3` → a **4**-bucket table. 8 buckets appear only at
4–7 elements, 16 at 8–14. hashbrown 0.14.5 (also in the lock file, via other dependencies) has
the same floor. The claim was off by 2× exactly in the size range the sentence is about.

The 8-bucket assumption was baked into shipping instrumentation in three copies —
`HeapReport::hb_bytes` in `locals_trie.rs`, its twin in `assign_like_trie.rs`, and a third in the
bench — each computing `…next_power_of_two().max(8)` and so reporting every hashbrown table of
≤3 elements at **twice its real size**. In this store that is the `fidx` per-function `Set<V>`
for functions with ≤3 flow variables, common in real code. `fidx` is 6–8 % of the store, so the
whole-store error stayed under ~4 %; above 3 elements the old formula was accurate to 1–2 %
(it modelled `buckets*(elem+1) + 16`; the truth is `buckets*elem + buckets + Group::WIDTH`, with
`Group::WIDTH` 8 on aarch64 and 16 on x86-64 SSE2).

**Fixed on this branch.** The three copies are one shared `locals_trie::hb_bytes` over
`locals_trie::hb_buckets` — `(capacity * 8).div_ceil(7).next_power_of_two().max(4)`, the exact
inverse of hashbrown's `bucket_mask_to_capacity` — priced with an `HB_GROUP_WIDTH` constant that
mirrors hashbrown's own target-feature choice of `Group` rather than hardcoding 16. It reproduces
`calculate_layout_for` exactly for any element aligned to at most `Group::WIDTH` (all of ours),
small tables included, and is 1.00 against the counting allocator for the whole store at every
group size in §5.

**Its test coverage is now partial.** `hybrid_set`'s `bucket_counts_track_hashbrown` grows a real
`hashbrown::HashSet` to 2000 elements and asserts `hb_bytes` agrees with the allocation at every
capacity it steps through — but only for 8 B elements and only for tables of 8 buckets or more,
since that test's subject is the Swiss table and the Swiss table has no 4-bucket regime. **The
4-bucket floor, which is the whole subject of this section, has no test.** Two that pinned it
directly, and the bench that printed estimate against real allocation at each element size, were
both removed after this session. The claim above rests on the hashbrown source read and on the
whole-store `est/real` of 1.00 in §5.

Two consequences for reading the rest of this document:

* Byte counts here are aarch64. On x86-64 add 8 B per hashbrown table (`Group::WIDTH` 16 vs 8);
  bucket *counts* are platform-independent, and the estimator derives the width from the same
  cfg hashbrown uses, so it is exact on both.
* Main's own reported store size is inflated by this bug. §6 measures how much: `maint` (main's
  estimator) against `maini` (same allocations, exact estimator) on the same programs.

---

## 9. Why the threshold is 64

`SMALL_THRESHOLD` is 64 — the spec's value, and the largest a `u64` occupancy bitmask admits.
It is a `const` parameter of `HybridSet`, not a hardcoded constant, so it can be swept without
editing the structure.

The threshold decides **how much of the group-size distribution pays the promotion step**, and
promotion is a hard ~2× on bytes per leaf that is never refunded, because without removals a
promoted set never demotes. §4 measures the step directly: 24.1 B/element at 64 elements,
**49.4 at 65**. §6 reproduces it end to end at the same place.

Nothing pulls the other way on memory, so a lower threshold strictly loses: it would move every
set of 33–64 leaves from 24 to ~50 B/element. What a lower threshold would buy is finding 4's
worst case — a miss against an exactly-full probe table — which halves from 44.8 ns at 64 slots
to 21.1 at 32. Two things bound that cost:

* It is the *miss* path against a table at load factor 1.0, and load factor 1.0 is what buys the
  24 B/element in the first place.
* The workload that manufactures exactly that shape is not slow. In §5, group size 64 — where
  every group holds exactly 64 leaves in a full 64-slot probe table — is the **fastest**
  configuration in this branch's whole sweep (0.103 s) and the **slowest** in main's (0.276).
  End to end, group size 63 sits at 0.543 s in a sweep spanning 0.51–0.98 (§6).

Under main's representation the trade ran the other way: the largest un-promoted group was the
slowest configuration in the whole sweep, because a sorted `Vec` degrades as it fills. §5 finding
8 is that effect, and it is why main's threshold looked "correct for time" while this one is
free to be chosen for memory.

---

## 10. What is left, in value order

1. **Free the drained `delta`/`new` outer maps.** `absorb` empties them with `fwd.drain()`, which
   does not free a hashbrown table, so whatever the widest iteration reached is held to the end:
   §5 measures the delta and new copies at 184.7 B/row against the store's own 117.4 at group
   size 1. `from.fwd = Map::default()` after the merge releases it. This is untouched by the
   branch and is now larger *relatively* than it was, because the store beside it got smaller.
2. **Soften the promotion cliff.** A promoted set pays ~50 B/leaf against the probe table's 24
   (§9), because a power-of-two bucket array holds 24 B elements at ≤ 87.5 % load. Holding the
   leaves in a dense array and putting only a `u32` index in the buckets would cost ~24 B/leaf
   plus ~5 B/bucket ≈ 37 B/leaf — roughly 1.4× less — at the price of one indirection per lookup.
   The spec's "implemented like a `hashbrown::HashTable`" used to rule this out; now that the
   table is written here, it is a contained change to `swiss.rs` behind the same API.
3. **A scan-only tier below ~4 elements.** §6's group histogram says **67–100 % of groups hold a
   single leaf** at every configuration measured (99–100 % once `max_group` is 128 or more). At
   those sizes comparing is cheaper than hashing, and the small representation is already exactly
   a packed array — the change is to skip the hash when `slots <= 4`. What it would recover is
   the one place §4 finding 1 has this branch losing to main: 23.6 ns to insert into a 2-element
   set against the `Vec`'s 11.3.
4. **A 16-wide group on x86-64.** The word-parallel group is the only implementation, so on
   x86-64 this scans 8 buckets per probe step where hashbrown's SSE2 group scans 16. Adding an
   SSE2 `Group` behind the same three methods (`match_byte`, `match_empty`, `match_full`) is
   mechanical; it was left out because it cannot be tested on this machine.
5. **Run the unsafe code under Miri**, which needs a nightly toolchain this environment does not
   have (§11).
6. **`assign_like_trie` still stores `Map<(F,Vs), Vec<(Vd,Pd,Ps)>>`** and was left alone. Same
   shape of problem; the same `HybridSet` drops into it.

---

## 11. Method notes and limitations

* **Binaries.** One `git worktree` per commit, private `CARGO_TARGET_DIR`, `cargo build
  --release`, same rustc 1.94.1 and the workspace's `lto = "thin"`. No measured source was
  modified. Two harness changes exist in `scripts/locals-bench.py`: a second, laxer regex so it
  can also parse main's `heap_report` line, which lacks the `groups:` shape suffix this branch
  added; and, for the re-measurement, `RUST_LOG=info,ctadl_ascent::index_engine=debug`, because
  main `e27e1466` moved the store estimate, the SCC times and the `[mem cp]` lines from INFO to
  DEBUG (§1). Only that one module is raised, so no other module's DEBUG output lands in the
  fixpoint's hot path and taxes the time being measured.
* **Interleaving.** Every comparison alternates binaries over the same generated programs, one
  configuration at a time, rotating order each pass, so no binary systematically gets a warmer
  machine. Tiers 0 and 1 use the same rotation over the bench binaries.
* **Sampling.** Tier 0: 5 passes, median (each number is already an average over 1 M elements).
  Tier 1: 9 interleaved passes of both bench binaries. Tier 2: 5 interleaved passes of all three
  `ctadl` binaries over the whole sweep, then 9 interleaved passes over 5 configurations.
* **Statistics.** Byte columns are deterministic and were verified identical across every pass
  before being reported as single values. Time is reported as a median plus, where it matters, a
  **paired per-pass ratio** — the ratio taken within a pass, so a slow pass that hits all
  binaries at once cannot move it. That is the statistic this machine needs, and this session
  demonstrates why: the 5-pass end-to-end sweep read −6 % to −21 % at group sizes 32, 64 and 128,
  and nine paired passes flattened all three to a wash (§6.1). **Do not quote an end-to-end time
  difference from fewer than ~9 paired passes.** Bytes need no such caution — they were identical
  in every pass of every tier.
* **Structure tiers use plain integers** at the production element sizes (leaf 24 B, key `(F,V)`
  16 B, outer entry 48 B on main / 32 B here — the same numbers `heap_report` logs). Using the
  real interned types would put *their* allocations into the counting allocator, defeating the
  measurement.
* **The group is 8 bytes wide on every platform**, because the word-parallel implementation is
  the only one. On aarch64 that matches hashbrown; on x86-64 hashbrown's SSE2 group scans 16
  buckets per probe step, so §4's insert/lookup margins should not be assumed to carry there.
  The *bytes* are platform-independent and 8 B/table smaller than hashbrown's on x86-64.
* **The unsafe code is not covered by Miri.** It is covered by unit tests with `debug_assert`s
  active (bounds, probe termination, control-byte and occupancy invariants, regime agreement),
  green in debug and release, but no nightly toolchain is installed here. That is the gap a
  reviewer should close before this leaves DO-NOT-MERGE status.
* **The end-to-end tier's `peak fp` is sampled every 20 ms**, so a sub-20 ms spike can be missed;
  `peak rss` is the child's own rusage high-water mark and needs no sampling. Physical footprint,
  not RSS, is the number that matters on macOS — RSS omits compressed pages (see
  `.claude/skills/measure-process-memory`).
* **Whole-process peaks are contaminated by a bimodal front end** and, at group sizes 1 and 2,
  that mode is worth more than the whole store saving. Fact loading settles at either ~414 MB or
  ~545 MB of footprint, per process, in *all three* binaries; the two modes then carry through to
  the peak. §7 finding 18a is the evidence. Where a comparison must be robust to it, use the
  footprint grown across the fixpoint (`fp_after − fp_before`), which cancels the mode, or read
  the store's own bytes from §6. This did not show up in the pre-rebase measurement, and it is
  the reason two of §7's cells changed sign.
* **Row counts across a sweep are held near a target, not exactly equal** (the generator produces
  ~5K+2 rows per function for group size K); the actual count is reported per row and all per-row
  figures are normalized by it.
* **Generated programs exercise field-sensitive propagation and summaries but have no calls,
  virtual dispatch or aliasing**, so `context_locals`, `resolvent` and the hybrid-inlining rules
  stay empty. This isolates the `locals` store; it is not a model of a real target's rule mix.
* **`mean` group size stays ~2.5 across the end-to-end sweep** because K formals contribute K
  singleton groups for every K-leaf group. That is faithful to real targets (many tiny, few huge)
  but it means the end-to-end tier cannot isolate group-size effects on total bytes the way the
  structure tier (where *every* group has the target size) can. Read the two together.

---

## 12. What moved when this was re-measured on the rebase

Everything above was re-run after rebasing onto `e27e1466`, with the same generators, the same
pass counts and the same three-binary interleaving. What follows is only what differs between the
two sessions, so a reader who knows the old numbers does not have to diff the tables.

**Nothing moved on bytes.** Every store-byte figure in §4, §5 and §6 came back *identical* —
tier-1 `total B/row` bit-for-bit at all 17 group sizes, tier-2 `store MB` to the reported decimal
at all 13 configurations, and the group histograms to the individual group. The rebase brought no
change to the store, and the byte result does not depend on machine state.

**One regression, and it is not the store.** Peak process memory at group size 1 went from 9.4 %
*below* main to 3.7 % *above* on footprint, and 6.2 % below to 2.3 % above on RSS (§7). The cause
is a bimodal front-end allocation of ~130 MB that all three binaries exhibit and that head drew
the expensive side of in every run; measured within one mode, or measured as the footprint grown
across the fixpoint, this branch is still ~120 MB ahead (§7 finding 18a, 19). No source change on
either side explains it, and it was not visible in the previous session.

**Two cells where footprint and RSS now contradict each other**, group sizes 2048 and 8192: RSS
−9 %, footprint +0.1 % and +2.1 % (§7 finding 18b). Previously both read the same way. Neither
direction should be quoted there.

**End-to-end timing got a little better and a lot more consistent.** The 9-pass paired ratios
moved 0.927 → 0.970 at group size 1, 0.955 → 0.943 at 2, 0.980 → 0.986 at 32, 0.969 → **0.929**
at 64 and 0.984 → 0.972 at 128; group size 64 is now a defensible win (8 of 9 passes) where it
was a wash, and no configuration reads above 1.000 against either main binary, where the previous
session had 1.025 at group 128. The 5-pass sweep's absolute fixpoint times fell across the board
(e.g. 1.011 → 0.982 s at group size 1), so the *margins* shrank even where the ratios held.

**Tier-1 time is unchanged in shape, marginally noisier at the tail.** Head still wins by 6.1 %
to 63 % from group size 1 to 4096; the 9-of-9 sweeps thinned to 6–8 of 9 at group sizes 512–4096,
and the one adverse cell moved from +6.2 % at group 16384 to +3.8 % (§5 finding 8).

**Tier-0's known regression got slightly worse**: the miss against an exactly-full 64-slot table
is 44.8 ns, against 39.5 before (§4 finding 4). The bytes at that size are unchanged, so this is
the same trade at the same price, measured on a slightly slower day.
