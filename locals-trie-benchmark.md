# The `locals` store, `main` → this branch: baseline benchmark — DO-NOT-MERGE

What this branch does to the `locals` BYODS store
(`ctadl-ascent/src/index_engine/locals_trie.rs`), measured against `main` at three tiers: the
set structure on its own, the store driven exactly as Ascent's semi-naive loop drives it, and
`ctadl index` end to end on generated programs — plus process memory.

Everything below was measured in one session on one machine: Apple M1 Ultra (20 cores, 128 GB),
macOS 26.5.2 (arm64), rustc 1.94.1, hashbrown 0.16.1, `--release` (`lto = "thin"`).

---

## 1. What is compared

`main` was `6da40f4` when this was measured; this branch forked at `097d526`, and

```
git diff 097d526 6da40f4 -- ctadl-ascent/src/index_engine/
```

is **empty** — everything main has added since the fork is front-end work that does not touch
this store. (Main has since advanced to `64975a9`, "Dex shared library import"; the same diff
against that tip is also empty, so nothing below is stale.) So "main's `locals` store" is well
defined, and it is the sorted-`Vec`-then-`HashSet` one:

| | `main` (`6da40f4`) | this branch (`5286e37`) |
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
| `main6` | `6da40f4` | **main itself.** No bench harness, and its `heap_report` predates the estimator fix (§8), so it is used for end-to-end *time* and *process memory*, which no estimator touches. |
| `main` | `9fd4c47` | main's store plus the two things needed to measure it: the bench harnesses and the exact `hb_bytes` (§8). `git diff` against the fork point touches nothing but instrumentation. Every **store-byte** number attributed to main comes from here. |
| `head` | `5286e37` | this branch. |

`main6` and `main` are run side by side end to end in §6–§7, which is what licenses the
substitution: they agree on fixpoint time and process memory to within the run-to-run spread, and
their store estimates differ only by the estimator fix.

---

## 2. Headline

* **Store bytes.** Exactly measured by a counting allocator, the store is **45 % smaller at one
  leaf per group**, falling to parity once groups are large enough that their leaves dominate
  (§5). End to end on generated programs it is **33–41 % smaller at every configuration
  measured** (§6), because a real group distribution is singleton-heavy whatever its maximum.
* **Process memory.** Peak physical footprint is **2.6–14.5 %** below main's at all 13
  end-to-end configurations and peak RSS **3.4–12.2 %** below at all 13 (§7). Both statistics
  agree at every cell, and RSS needs no sampling.
* **Time.** At store level this branch is faster at every group size from 1 to 4096 leaves, by
  **5.1 % to 63 %**, winning **9 of 9** paired passes at 12 of those 13 sizes; at 8192 leaves and
  above the two are level (§5). End to end the effect is much smaller and only partly separable
  from noise: **−7.3 % and −4.5 %** at group sizes 1 and 2 over 9 paired passes (9/9 and 8/9),
  and a wash at 32, 64 and 128 (§6.1). The store is a component of the index phase, not the whole
  of it.
* **The set structure.** Below the threshold it holds 24 B/element like main's sorted `Vec` but
  inserts **1.8–3.5× faster** from 16 elements up (and 1.1–2.6× *slower* at 2–5, where a `Vec`
  push is a memcpy); above it, the from-scratch Swiss table allocates the same bytes as hashbrown
  at every size ≥ 5 elements (§4).
* **The one regression** is a miss against an exactly-full small table, which degenerates to a
  scan: 39.5 ns at 64 slots against ~5 ns for main's binary search (§4 finding 4). It is bounded,
  it is the price of load factor 1.0, and the workload that manufactures it is among the fastest
  configurations measured (§9).

---

## 3. What was built, and how to run it

| file | role |
|---|---|
| `ctadl-ascent/src/index_engine/hybrid_set.rs` | `HybridSet<T, S>` — the set; 13 unit tests |
| `ctadl-ascent/src/index_engine/hybrid_set/raw.rs` | `RawTable<T>` — the one structure both regimes share |
| `ctadl-ascent/src/index_engine/hybrid_set/swiss.rs` | the from-scratch Swiss table's probing/sizing rules; 3 unit tests |
| `ctadl-ascent/src/index_engine/mod.rs` | `hb_buckets` / `hb_bytes` / `HB_GROUP_WIDTH` — the shared, exact hashbrown-size estimator (§8) + 2 unit tests |
| `ctadl-ascent/benches/hybrid_set.rs` | **tier 0** — five set representations under a counting allocator |
| `ctadl-ascent/benches/locals_trie.rs` | **tier 1** — the whole store, driven as Ascent drives it, under a counting allocator |
| `scripts/gen-locals-bench.py` | generates Flowy (`.tnt`) programs with a chosen `(F,V)` group size, path count and function count |
| `scripts/locals-bench.py` | **tier 2** — generate → `ctadl import` → `ctadl index`, parsing store bytes, fixpoint time, peak footprint and peak RSS |
| `HeapReport` additions | `max_group`, `large_groups`, `group_hist` — the store logs its own group-size distribution, which is how the harness verifies the generator hit its target shape |

`cargo test -p ctadl-ascent --lib` is green in both profiles (200 tests, 20 of them in
`index_engine`: 13 for `HybridSet`, 3 for the Swiss layer, 2 for the estimator, 2 for the store's
views), and `cargo clippy --all-targets -p ctadl-ascent` is clean under the workspace's
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
| 1 | **32.0** / 96.0 / 108.0 | **16.8** / 23.3 / 24.3 | **2.9** / 3.7 / 4.8 | **1.9** / 2.1 / 2.3 | **17.8** / 21.4 / 53.4 |
| 2 | **28.0** / 48.0 / 54.0 | 22.4 / **10.6** / 11.4 | 3.2 / 3.0 / **2.6** | 3.0 / 2.8 / **2.0** | **34.7** / 35.9 / 46.0 |
| 3 | 34.7 / **32.0** / 36.0 | 24.9 / 9.5 / **8.8** | 3.5 / 4.3 / **2.6** | 3.3 / 3.5 / **1.9** | 41.9 / 43.5 / **43.4** |
| 5 | 40.0 / **38.4** / 41.6 | 24.5 / 22.3 / **22.2** | 4.2 / 7.0 / **3.1** | **2.9** / 3.7 / 3.8 | **44.8** / 52.7 / 51.3 |
| 8 | 25.0 / **24.0** / 51.0 | **16.2** / 17.3 / 30.6 | **2.7** / 6.2 / 3.3 | 4.7 / **3.6** / 3.7 | **44.2** / 60.7 / 59.8 |
| 16 | 24.5 / **24.0** / 50.5 | **13.8** / 24.1 / 30.1 | **3.0** / 8.2 / 3.4 | 9.4 / 4.9 / **2.8** | **40.3** / 46.3 / 47.9 |
| 24 | 32.3 / **32.0** / 33.7 | **14.8** / 29.4 / 20.9 | 2.9 / 10.8 / **2.5** | **2.7** / 4.3 / 2.9 | 43.0 / 42.0 / **35.7** |
| 31 | 25.0 / **24.8** / 51.9 | **11.5** / 29.1 / 27.6 | 4.3 / 10.6 / **3.6** | 10.3 / 4.3 / **2.6** | **35.1** / 43.3 / 49.3 |
| 32 | 24.2 / **24.0** / 50.2 | **11.1** / 27.3 / 23.7 | 4.1 / 10.6 / **2.6** | 19.2 / 4.3 / **2.6** | **33.6** / 39.6 / 49.7 |
| 33 | 46.8 / **46.5** / 48.7 | **17.9** / 33.5 / 23.0 | 3.7 / 14.7 / **2.6** | **2.6** / 6.3 / **2.6** | **36.9** / 45.4 / 45.2 |
| 40 | 38.6 / **38.4** / 40.2 | **12.4** / 30.8 / 17.6 | 2.7 / 13.5 / **2.6** | 2.6 / 5.7 / **2.4** | **35.8** / 47.8 / 42.6 |
| 48 | 32.2 / **32.0** / 33.5 | **10.3** / 32.2 / 14.6 | **2.4** / 13.1 / 2.6 | **2.6** / 5.6 / 3.9 | **31.0** / 45.6 / 35.4 |
| 64 | 24.1 / **24.0** / 50.1 | **9.4** / 33.1 / 17.2 | 3.7 / 12.9 / **2.7** | 39.5 / 5.3 / **2.4** | **25.3** / 44.6 / 46.5 |
| 65 | **49.4 / 49.4 / 49.4** | 18.9 / 44.6 / **16.8** | 4.6 / **2.7 / 2.7** | **2.1** / 2.3 / 2.3 | **35.1** / 54.3 / 43.6 |
| 128 | **50.1 / 50.1 / 50.1** | 17.2 / 30.1 / **15.1** | 4.5 / 3.5 / **2.7** | **2.6** / 3.1 / 3.1 | **26.3** / 37.5 / 37.6 |
| 1024 | **50.0 / 50.0 / 50.0** | 14.6 / 14.8 / **12.4** | 5.8 / **4.2 / 4.2** | **2.1** / 2.6 / 2.6 | **24.7** / 37.1 / 29.9 |

The Swiss table on its own (`SMALL = 0`, i.e. the large half used at every size) against
hashbrown, which is the comparison the spec's "implement a custom one" clause invites:

| n | B/elem swiss / hash | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | 208.0 / **108.0** | 32.5 / **24.3** | 20.2 / **4.8** | 9.6 / **2.3** | 59.9 / **53.4** |
| 2 | 104.0 / **54.0** | 17.6 / **11.4** | 12.4 / **2.6** | 6.5 / **2.0** | **40.0** / 46.0 |
| 3 | 69.3 / **36.0** | **8.1** / 8.8 | 2.8 / **2.6** | 4.5 / **1.9** | **38.3** / 43.4 |
| 5 | **41.6 / 41.6** | **7.0** / 22.2 | **2.7** / 3.1 | **3.3** / 3.8 | **37.9** / 51.3 |
| 8 | **51.0 / 51.0** | **16.6** / 30.6 | 3.5 / **3.3** | **3.2** / 3.7 | **43.1** / 59.8 |
| 16 | **50.5 / 50.5** | **17.2** / 30.1 | 3.7 / **3.4** | **2.6** / 2.8 | **35.5** / 47.9 |
| 24 | **33.7 / 33.7** | **13.3** / 20.9 | 2.7 / **2.5** | **2.7** / 2.9 | **29.9** / 35.7 |
| 32 | **50.2 / 50.2** | **16.8** / 23.7 | 2.8 / **2.6** | **2.3** / 2.6 | **31.7** / 49.7 |
| 48 | **33.5 / 33.5** | **10.2** / 14.6 | 2.8 / **2.6** | **3.3** / 3.9 | **22.9** / 35.4 |
| 64 | **50.1 / 50.1** | **16.8** / 17.2 | 4.0 / **2.7** | **2.1** / 2.4 | **34.3** / 46.5 |
| 128 | **50.1 / 50.1** | 15.9 / **15.1** | 3.8 / **2.7** | **2.8** / 3.1 | **30.2** / 37.6 |
| 1024 | **50.0 / 50.0** | **11.0** / 12.4 | 4.4 / **4.2** | **2.0** / 2.6 | **26.3** / 29.9 |

**Findings.**

1. **Below the threshold the hybrid matches main's `Vec` on bytes and pulls away from it on
   insert as the group fills.** 24.1 B/element against 24.0 at 64 elements, 24.2 against 24.0 at
   32 — the occupancy word amortizes away — while insert is 9.4 ns against 33.1 and merge 25.3
   against 44.6. Main's `Vec` gets *worse* as the group fills (17.3 → 24.1 → 27.3 → 33.1 ns per
   insert from 8 to 64 elements) because `Vec::insert` shifts; the probe table gets *better*
   (17.3 → 13.8 → 11.1 → 9.4) because more slots mean fewer collisions. That divergence is the
   whole of §5's time result. **At 2–5 elements it goes the other way** — 22.4 ns against the
   `Vec`'s 10.6 at n=2 — because a `Vec` push into spare capacity is a memcpy and the probe table
   still hashes. §10 item 3 is the fix; §5 and §6 show the crossover is well below where the
   store actually lives, since a store's small sets are built by *merge*, not by repeated insert,
   and merge is 1.0–1.8× faster at every size measured.
2. **Against hashbrown below the threshold the win is memory, 2.1–4.5×**: 24–32 B/element where
   hashbrown spends 50–108, because a power-of-two bucket array at ≤ 87.5 % load cannot hold two
   elements cheaply. That is the reason the hybrid exists.
3. **The from-scratch table is hashbrown's layout.** From 5 elements up the two allocate
   *identical* bytes at every size in the sweep — 41.6, 51.0, 50.5, 33.7, 50.2, 33.5, 50.1, 50.0
   B/element — which is what a unit test checks against a live `hashbrown::HashSet` over 2000
   inserts. It inserts up to 3.2× faster and merges 1.1–1.6× faster (no `growth_left` to
   maintain, no tombstone handling, equality on whole elements rather than through a closure);
   hashbrown's only win above the floor is 15.1 vs 15.9 ns of insert at n=128, and lookups agree
   to within 1.3 ns. Below 5 elements the from-scratch table is 2× *worse* on bytes, because its
   bucket floor is 8 rather than 4 — a deliberate simplification, in a range the hybrid never
   gives it.
4. **The known regression: a miss against an exactly-full small table degenerates to a scan** —
   9.4 ns at 16 slots, 19.2 at 32, **39.5 at 64** — against ~5 ns for main's binary search and
   ~2.5 ns for either hash table. Load factor 1.0 is what buys the 24 B/element. §9 is the
   argument that the workload does not pay it.
5. **Promotion costs about one insert of latency at the boundary**: at n=65 the hybrid inserts in
   18.9 ns against the raw Swiss table's 16.6, because reaching 65 means filling a 64-slot probe
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
| 1 | 1048576 | 213.4 | **117.4** | **−45.0 %** | 312.7 | 184.7 | 0.188 | **0.158** | −15.6 % | 0.849 | 9/9 |
| 2 | 524288 | 82.7 | **70.7** | −14.5 % | 182.0 | 138.0 | 0.215 | **0.174** | −19.0 % | 0.818 | 9/9 |
| 4 | 262144 | 53.4 | **47.4** | −11.2 % | 103.0 | 81.0 | 0.208 | **0.157** | −24.3 % | 0.749 | 9/9 |
| 8 | 131072 | 38.7 | **35.7** | −7.8 % | 63.5 | 52.5 | 0.193 | **0.134** | −30.5 % | 0.688 | 9/9 |
| 16 | 65536 | 31.3 | **29.8** | −4.8 % | 43.7 | 38.2 | 0.208 | **0.114** | −45.5 % | 0.548 | 9/9 |
| 32 | 32768 | 27.7 | **26.9** | −2.7 % | 33.9 | 31.1 | 0.219 | **0.104** | −52.5 % | 0.473 | 9/9 |
| **64** | 16384 | 25.8 | **25.5** | −1.5 % | 28.9 | 27.6 | 0.275 | **0.103** | **−62.7 %** | **0.373** | 9/9 |
| 128 | 8192 | 51.0 | **50.7** | −0.5 % | 52.5 | 51.8 | 0.220 | **0.126** | −42.9 % | 0.575 | 9/9 |
| 256 | 4096 | 50.5 | **50.4** | −0.2 % | 51.3 | 50.9 | 0.174 | **0.117** | −32.7 % | 0.671 | 9/9 |
| 512 | 2048 | 50.2 | 50.2 | −0.1 % | 50.6 | 50.4 | 0.138 | **0.112** | −19.1 % | 0.805 | 9/9 |
| 1024 | 1024 | 50.1 | 50.1 | −0.1 % | 50.3 | 50.2 | 0.122 | **0.108** | −11.8 % | 0.871 | 9/9 |
| 2048 | 512 | 50.1 | 50.0 | −0.0 % | 50.2 | 50.1 | 0.111 | **0.104** | −6.7 % | 0.944 | 7/9 |
| 4096 | 256 | 50.0 | 50.0 | −0.0 % | 50.1 | 50.1 | 0.106 | **0.100** | −5.1 % | 0.945 | 9/9 |
| 8192 | 128 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.097 | 0.096 | −1.5 % | 0.987 | 5/9 |
| 16384 | 64 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | **0.090** | 0.096 | +6.2 % | 1.048 | 1/9 |
| 32768 | 32 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.098 | 0.099 | +1.0 % | 1.009 | 2/9 |
| 65536 | 16 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.109 | 0.110 | +0.9 % | 1.004 | 3/9 |

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
8. **Time: this branch wins 9 of 9 paired passes at 12 of the 13 group sizes from 1 to 4096**
   (group 2048 is 7 of 9), by 5.1 % at 4096 leaves up to **63 % at group size 64**. The peak is
   exactly where main is worst: group size 64 is the largest *un-promoted* sorted `Vec`, so every
   insert shifts up to 63 leaves and every delta→total merge re-copies the whole group. Both
   costs are gone. From 8192 leaves per group up the two are level (ratios 0.99–1.05, 1–5 of 9
   passes) — there main was already a `HashSet` and this is the Swiss table against hashbrown,
   which §4 finding 3 says is a wash.
9. **The `delta`/`new` copies still cost more than the store.** `+delta` is 184.7 B/row against
   a `total` of 117.4 at group size 1 — Ascent's delta and new relations hold 1.6× the
   steady-state store — because `absorb` empties them with `fwd.drain()`, which does not free a
   hashbrown table. Whatever the widest iteration reached is held to the end. This is untouched
   by the branch and is the largest single item left (§10).

### The nested design this replaced

The module's forerunner stored `(F,V) -> P -> {(M,Fp)}` as `Map<(F,V), Map<P, Set<(M,Fp)>>>`. The
bench rebuilds it faithfully over identical data, so the "flat beats nested" claim in the module
docs has a number:

| group size | paths | nested B/row | flat B/row | nested/flat |
|---|---|---|---|---|
| 2 | 1 | 173.0 | 70.7 | **2.45** |
| **5** | **2** | **77.1** | **52.0** | **1.48** |
| 10 | 2 | 52.1 | 45.2 | 1.15 |
| 20 | 4 | 48.7 | 41.8 | 1.16 |
| 64 | 16 | 58.2 | 25.5 | **2.28** |
| 1024 | 256 | 56.6 | 50.1 | 1.13 |

At the shape the module docs cite as measured on a real workload — ~5 leaves per group over ~2
distinct `P` — the flat form is **1.48×** smaller, and across the sweep **1.13–2.45×**. It is not
the order of magnitude an older version of the docs implied: flattening removes the inner tables'
slack and then pays some of it back by storing `P` inline on every leaf where the nested form
shared one `P` across a leaf set.

---

## 6. Tier 2 — end to end, `ctadl index` on generated programs

`scripts/locals-bench.py`'s measurement code, ~1 M `locals` rows per configuration, **5
interleaved passes** of the whole sweep with all three binaries alternating over the same
generated programs, order rotated each pass. `store MB` is `heap_report()`; it was identical in
all 5 passes for every cell and is reported as a single value.

| group | rows | groups | max | large | `main6` MB | `main` MB | `head` MB | head vs main |
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

| group | `main6` s | `main` s | `head` s | head vs main6 | paired ratio | head wins |
|---|---|---|---|---|---|---|
| 1 | 1.011 | 1.007 | **0.934** | −7.6 % | 0.922 | 5/5 |
| 2 | 1.090 | 1.104 | **0.974** | −10.6 % | 0.909 | 5/5 |
| 4 | 0.940 | 0.931 | **0.902** | −4.0 % | 0.995 | 3/5 |
| 8 | 0.721 | 0.745 | **0.692** | −4.0 % | 0.971 | 3/5 |
| 16 | 0.643 | 0.628 | **0.598** | −7.0 % | 0.947 | 4/5 |
| 32 | 0.624 | 0.613 | **0.584** | −6.4 % | 0.907 | 4/5 |
| 63 | 0.627 | 0.632 | **0.579** | −7.7 % | 0.915 | 5/5 |
| 64 | 0.693 | 0.637 | **0.581** | −16.2 % | 0.910 | 5/5 |
| 65 | 0.642 | 0.660 | **0.597** | −7.0 % | 0.880 | 5/5 |
| 128 | 0.612 | 0.598 | **0.555** | −9.3 % | 0.915 | 4/5 |
| 512 | 0.637 | 0.660 | **0.558** | −12.4 % | 0.944 | 4/5 |
| 2048 | 0.665 | 0.590 | **0.523** | −21.4 % | 0.918 | 5/5 |
| 8192 | 0.558 | 0.545 | **0.526** | −5.7 % | 0.926 | 4/5 |

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
11. **`main6` and `main` agree, which is what licenses the stand-in.** Their store estimates
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
14. **The fixpoint is faster than main's at all 13 configurations by median** — by 4.0 % to
    21.4 %, with paired ratios 0.88–1.00 and head ahead in 3–5 of 5 passes. **Most of that margin
    does not survive resampling**; §6.1 is what the timing claim should actually be.

### 6.1 Nine-pass resample of the fixpoint

Five configurations at **9 interleaved passes**, reported as the paired per-pass ratio (the ratio
taken within a pass) and the count of passes head won. This is the statistic this machine needs
(§11), and it was taken while the machine was noisier than the 5-pass sweep — the slowest sample
in each column is 1.2–1.4× the median, and those slow samples land in the same passes for all
three binaries, which is exactly what pairing is for.

| group | head/`main6` | head wins | head/`main` | head wins |
|---|---|---|---|---|
| 1 | **0.927** | **9/9** | 0.951 | 7/9 |
| 2 | **0.955** | **8/9** | 0.985 | 5/9 |
| 32 | 0.980 | 5/9 | 0.996 | 5/9 |
| 64 | 0.969 | 6/9 | 0.936 | 7/9 |
| 128 | 0.984 | 5/9 | 1.025 | 4/9 |

15. **End to end, only group sizes 1 and 2 are a defensible timing win**: −7.3 % and −4.5 %
    against actual main, 9 of 9 and 8 of 9 paired passes. Those are the configurations with the
    most outer-map entries per leaf (857 k and 750 k groups), which is where §5's byte saving is
    largest — the same term buying time.
16. **At group sizes 32, 64 and 128 the fixpoint is a wash.** Median ratios sit at 0.97–0.98
    against main6 but head wins only 5–6 of 9 passes, and against `main` one configuration goes
    the other way (1.025 at group 128). The 5-pass sweep's −6 % to −21 % at these sizes is not
    reproducible and **should not be quoted**. This is the same failure mode §11 warns about, and
    it is why the sweep's timing column is presented as a direction rather than a figure.
17. **The tier-1 result and the tier-2 result are not in conflict.** §5 measures the store alone
    driving 1 M rows through the semi-naive loop, where the store *is* the workload and the
    margin is 5–63 %. Here the same store is one component of an index phase that also parses,
    builds SSA, runs codegen and evaluates every other relation; the fixpoint SCC is 0.5–1.1 s of
    which the `locals` store is a fraction. A large win on the component is a small win on the
    whole, and the whole is what §6.1 measures.

---

## 7. Process memory

Peak physical footprint and peak RSS of the whole `ctadl index` run, median of the 5 passes.
Footprint is sampled at 20 ms; RSS is the child's own rusage high-water mark, so it needs no
sampling and is the quieter statistic. Both are whole-process numbers: the front end (parse, SSA,
codegen — large here, because the programs have up to 143 k functions) is identical in all three
binaries and dilutes the ratio, so §6's store figures are the ones to quote for the store.

| group | peak fp `main6` / `main` / `head` MB | head vs `main6` | peak rss `main6` / `main` / `head` MB | head vs `main6` |
|---|---|---|---|---|
| 1 | 1027 / 1033 / **930** | −9.4 % | 1558 / 1564 / **1462** | −6.2 % |
| 2 | 759 / 736 / **653** | −14.0 % | 1289 / 1264 / **1181** | −8.4 % |
| 4 | 711 / 699 / **625** | −12.0 % | 978 / 965 / **892** | −8.8 % |
| 8 | 598 / 581 / **513** | −14.2 % | 861 / 846 / **768** | −10.8 % |
| 16 | 483 / 486 / **452** | −6.4 % | 670 / 673 / **639** | −4.6 % |
| 32 | 495 / 497 / **482** | −2.6 % | 631 / 625 / **609** | −3.4 % |
| 63 | 355 / 344 / **305** | −13.9 % | 486 / 489 / **436** | −10.1 % |
| 64 | 346 / 351 / **302** | −12.6 % | 550 / 539 / **507** | −7.9 % |
| 65 | 410 / 410 / **390** | −4.8 % | 596 / 596 / **566** | −4.9 % |
| 128 | 385 / 387 / **363** | −5.8 % | 505 / 502 / **444** | −12.2 % |
| 512 | 375 / 375 / **320** | −14.5 % | 472 / 469 / **428** | −9.4 % |
| 2048 | 324 / 321 / **311** | −4.2 % | 417 / 413 / **381** | −8.6 % |
| 8192 | 313 / 302 / **290** | −7.4 % | 405 / 405 / **370** | −8.8 % |

**Findings.**

18. **Peak process footprint is 2.6–14.5 % below main's at all 13 configurations, and peak RSS is
    3.4–12.2 % below at all 13.** Every cell goes the same way in both statistics, which is the
    strongest form this measurement takes: RSS needs no sampling, so agreement between the two
    rules out a sampling artifact.
19. **Unlike the timing, the memory result survives resampling.** The five configurations rerun
    at 9 passes (§6.1) give peak footprint 932 / 661 / 482 / 312 / 360 MB against the 5-pass
    sweep's 930 / 653 / 482 / 302 / 363, and peak RSS within 9 MB at all five. Bytes on this
    machine are reproducible in a way that seconds are not.
20. **The process saves more absolute bytes than the `total` store does**, which is the delta
    copies showing up. At group size 63 the store's `total` is 32.6 MB smaller and the process
    footprint is 50 MB smaller; at group size 1, 59.6 MB against 97. The ratio is ~1.6× in both
    cases, and §5's `+delta` column is the reason: Ascent holds `delta` and `new` copies of the
    same store, they shrink by the same rule, and only the `total` is what `heap_report()`
    reports. The *percentages* still shrink at process level, because the front end is in the
    denominator and is identical in all three binaries.

---

## 8. The `hb_bytes` estimator, and hashbrown's minimum table

Both `heap_report()`s price their enclosing hashbrown maps with `index_engine::hb_bytes`. Before
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

Measured this session by the tier-1 bench (`capacity` is hashbrown's own report, so `capacity 3`
*is* a 4-bucket table). `old` is the estimator main still ships; `new` is this branch's:

| elem B | n | capacity | real B | new `hb_bytes` | new est/real | old `hb_bytes` | old est/real |
|---|---|---|---|---|---|---|---|
| 16 = `(M,Fp)`, the old leaf set | 1–3 | 3 | 76 | 76 | **1.00** | 152 | **2.00** |
| 24 = `(P,M,Fp)`, this module's leaf | 1–3 | 3 | 108 | 108 | **1.00** | 216 | **2.00** |
| 40 = `(P, HashSet)`, the old inner-map entry | 1–3 | 3 | 172 | 172 | **1.00** | 344 | **2.00** |
| 24 | 4–7 | 7 | 208 | 208 | 1.00 | 216 | 1.04 |
| 24 | 8–14 | 14 | 408 | 408 | 1.00 | 416 | 1.02 |
| 24 | 15–28 | 28 | 808 | 808 | 1.00 | 816 | 1.01 |

The 8-bucket assumption was baked into shipping instrumentation in three copies —
`HeapReport::hb_bytes` in `locals_trie.rs`, its twin in `assign_like_trie.rs`, and a third in the
bench — each computing `…next_power_of_two().max(8)` and so reporting every hashbrown table of
≤3 elements at **twice its real size**. In this store that is the `fidx` per-function `Set<V>`
for functions with ≤3 flow variables, common in real code. `fidx` is 6–8 % of the store, so the
whole-store error stayed under ~4 %; above 3 elements the old formula was accurate to 1–2 %
(it modelled `buckets*(elem+1) + 16`; the truth is `buckets*elem + buckets + Group::WIDTH`, with
`Group::WIDTH` 8 on aarch64 and 16 on x86-64 SSE2).

**Fixed on this branch.** The three copies are one shared `index_engine::hb_bytes` over
`index_engine::hb_buckets` — `(capacity * 8).div_ceil(7).next_power_of_two().max(4)`, the exact
inverse of hashbrown's `bucket_mask_to_capacity` — priced with an `HB_GROUP_WIDTH` constant that
mirrors hashbrown's own target-feature choice of `Group` rather than hardcoding 16. It reproduces
`calculate_layout_for` exactly for any element aligned to at most `Group::WIDTH` (all of ours),
so **est/real is 1.00 on every row above**, small tables included, and 1.00 against the counting
allocator for the whole store at every group size in §5. Two unit tests hold it there: one grows
real hashbrown tables element by element and asserts `hb_buckets` recovers the bucket count from
every `capacity` hashbrown reports (element sizes 1, 2, 8, 16, 24, 40, 48 B — the narrow ones
cover hashbrown's minimum-capacity lift); the other pins the 4-bucket floor and its byte figures.

Two consequences for reading the rest of this document:

* Byte counts here are aarch64. On x86-64 add 8 B per hashbrown table (`Group::WIDTH` 16 vs 8);
  bucket *counts* are platform-independent, and the estimator derives the width from the same
  cfg hashbrown uses, so it is exact on both.
* Main's own reported store size is inflated by this bug. §6 measures how much: main6 (main's
  estimator) against main (same allocations, exact estimator) on the same programs.

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
worst case — a miss against an exactly-full probe table — which halves from 39.5 ns at 64 slots
to 19.2 at 32. Two things bound that cost:

* It is the *miss* path against a table at load factor 1.0, and load factor 1.0 is what buys the
  24 B/element in the first place.
* The workload that manufactures exactly that shape is not slow. In §5, group size 64 — where
  every group holds exactly 64 leaves in a full 64-slot probe table — is the **fastest**
  configuration in this branch's whole sweep (0.103 s) and the **slowest** in main's (0.275).
  End to end, group size 63 sits at 0.579 s in a sweep spanning 0.52–0.97 (§6).

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
   the one place §4 finding 1 has this branch losing to main: 22.4 ns to insert into a 2-element
   set against the `Vec`'s 10.6.
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
  modified. The one harness change made for this session is a second, laxer regex in
  `scripts/locals-bench.py` so it can also parse main's `heap_report` line, which lacks the
  `groups:` shape suffix this branch added.
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
