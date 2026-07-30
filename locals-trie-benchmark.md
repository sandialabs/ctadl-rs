# `locals_trie` benchmark: baseline time and memory - DO-NOT-MERGE

Two-tier benchmark for the `locals` BYODS store
(`ctadl-ascent/src/index_engine/locals_trie.rs`), plus an answer to the question that
prompted it: **does hashbrown actually have an 8-bucket minimum?**

Everything below is measured on this machine: Apple M1 Ultra (20 cores, 128 GB), macOS 26.5.2
(arm64), rustc 1.94.1, hashbrown 0.16.1, `--release` (`lto = "thin"`).

---

## 1. The hashbrown claim: no, the minimum is 4 buckets

The module docs say of the pre-trie nested design:

> the two inner hash levels are nearly empty: each pays hashbrown's 8-bucket minimum
> allocation to hold ~2 elements.

**That is wrong.** hashbrown's minimum table is **4 buckets**, holding up to 3 elements. From
`hashbrown-0.16.1/src/raw/mod.rs:105` (`capacity_to_buckets`), for `cap < 15`:

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

So an element of 16/24/40 B lands on `min_cap = 3` → a **4**-bucket table. 8 buckets appear
only at 4–7 elements, 16 buckets at 8–14. hashbrown 0.14.5 (also in the lock file, via other
dependencies) has the same 4-bucket floor, reached by simpler code with no element-size cases.
The claim is off by 2× exactly in the size range the sentence is about.

Measured (`cargo bench -p ctadl-ascent --bench locals_trie`, first table — `capacity` is
hashbrown's own report, so `capacity 3` *is* a 4-bucket table):

| elem B | n | capacity | real bytes | `hb_bytes` estimate | est/real |
|---|---|---|---|---|---|
| 16 = `(M,Fp)`, old leaf set | 1–3 | 3 | 76 | 152 | **2.00** |
| 24 = `(P,M,Fp)`, this module's leaf | 1–3 | 3 | 108 | 216 | **2.00** |
| 40 = `(P, HashSet)`, old inner-map entry | 1–3 | 3 | 172 | 344 | **2.00** |
| 24 | 4–7 | 7 | 208 | 216 | 1.04 |
| 24 | 8–14 | 14 | 408 | 416 | 1.02 |
| 24 | 15–28 | 28 | 808 | 816 | 1.01 |

Two consequences:

* The same 8-bucket assumption is baked into shipping instrumentation:
  `HeapReport::hb_bytes` computes `…next_power_of_two().max(8)`. Every hashbrown table with
  ≤3 elements is reported at **2× its real size**. In this store that is the `fidx` per-function
  `Set<V>` for functions with ≤3 flow variables — common in real code. `fidx` is 6–8 % of the
  store in the runs below, so the whole-store error stays under ~4 %, and for tables above 3
  elements the formula is accurate to 1–2 % (it models `buckets*(elem+1) + 16`; the truth is
  `buckets*elem + buckets + Group::WIDTH`, and `Group::WIDTH` is 8 on aarch64, 16 on x86-64
  SSE2). Dropping `.max(8)` for `.max(4)` would fix it.
* Byte counts here are aarch64 numbers. On x86-64 add 8 B per table (`Group::WIDTH` 16 vs 8);
  the *bucket counts* are platform-independent.

**The separate claim about magnitude does not hold up either.** The docs attribute ~91 % of the
old store to structural slack, which reads as a ~10× reduction. Measured against a
faithfully-rebuilt `Map<(F,V), Map<P, Set<(M,Fp)>>>` over identical data, the flat design is
**1.1–2.3× smaller**, and at exactly the shape the docs cite (~5 leaves per group over ~2
distinct `P`) it is **1.42×**:

| group size | paths | nested B/row | flat B/row | nested/flat |
|---|---|---|---|---|
| 2 | 1 | 173.0 | 106.7 | 1.62 |
| **5** | **2** | **77.1** | **54.4** | **1.42** |
| 10 | 2 | 52.1 | 46.4 | 1.12 |
| 20 | 4 | 48.7 | 42.4 | 1.15 |
| 64 | 16 | 58.2 | 25.8 | 2.25 |
| 1024 | 256 | 56.6 | 50.1 | 1.13 |

The flat design wins, but not by an order of magnitude: it removes the inner tables' slack and
then pays it back by storing `P` inline on every leaf (24 B/leaf) where the nested form shared
one `P` across its leaf set. The 91 % figure describes how the *old* structure's bytes were
divided, not how much the replacement saves.

---

## 2. What was built

| file | role |
|---|---|
| `scripts/gen-locals-bench.py` | generates Flowy (`.tnt`) programs with a chosen `(F,V)` **group size**, path count and function count |
| `scripts/locals-bench.py` | end-to-end harness: generate → `ctadl import` → `ctadl index`, parse time/memory, print a table (also works against a default-storage build, for A/B) |
| `ctadl-ascent/benches/locals_trie.rs` | structure-level bench: counting global allocator over `LocalsIndCommon` driven exactly as Ascent's semi-naive loop drives it |
| `HeapReport` additions (`locals_trie.rs`) | `max_group`, `large_groups`, `group_hist` — the store now logs its group-size distribution, which is what lets the harness verify the generator hit its target shape |

```bash
cargo build --release                                     # the ctadl the harness measures
scripts/locals-bench.py                                   # default sweep, ~1M rows/config
scripts/locals-bench.py --rows 200000 --group-sizes 4,64,65 --out r.tsv
scripts/gen-locals-bench.py --funcs 50 --group-size 8 --paths 2 -o bench.tnt
cargo bench -p ctadl-ascent --bench locals_trie            # structure tier
```

### How the generator controls group size

The engine seeds `locals(f, ai, ε, i, ε)` per formal and propagates along assignments, so a
variable reached by K distinct formals ends up with a K-leaf `(F,V)` group. Each generated
function therefore takes K parameters, funnels them into one variable, and stores them into
one object over `--paths` distinct fields:

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

This yields ~5K rows per function: K singleton groups (each formal reaches only itself) plus
~4 groups of K leaves (`obj`, `out`, the return port, the summary). `max_group` in the log
confirms the target was hit. That mix — **many tiny groups plus a few large ones** — is the
regime the module is designed for, and it is why the *mean* group size stays ~2.5 even when
`max_group` is 8193.

---

## 3. Tier 1 — structure level (exact bytes, counting allocator)

1 M rows in every row; group size varies and group count varies inversely. `total B` is the
`total` store alone; `+delta B` includes the `delta`/`new` copies Ascent holds at fixpoint.
`est/real` compares `heap_report()` against the allocator.

Semi-naive shape, one new leaf per group per iteration (pessimal for the delta→total merge):

| group | groups | total B | B/row | +delta B/row | peak B | allocs | secs | est/real |
|---|---|---|---|---|---|---|---|---|
| 1 | 1048576 | 223772720 | 213.4 | 312.7 | 390987432 | 1245252 | 0.217 | 1.00 |
| 2 | 524288 | 86720560 | 82.7 | 182.0 | 307710712 | 1720416 | 0.216 | 1.00 |
| 4 | 262144 | 55943216 | 53.4 | 103.0 | 167829600 | 1957978 | 0.207 | 1.00 |
| 8 | 131072 | 40554544 | 38.7 | 63.5 | 90206424 | 2076756 | 0.191 | 1.00 |
| 16 | 65536 | 32860216 | 31.3 | 43.7 | 57686424 | 2136142 | 0.205 | 1.00 |
| 32 | 32768 | 29013056 | 27.7 | 33.9 | 41426712 | 2165832 | 0.219 | 1.00 |
| **64** | 16384 | 27089480 | **25.8** | 28.9 | 33297432 | 2180674 | **0.273** | 1.00 |
| **128** | 8192 | 53456208 | **51.0** | 52.5 | 56952624 | 1680188 | 0.222 | 1.00 |
| 512 | 2048 | 52685728 | 50.2 | 50.6 | 53559792 | 1284336 | 0.138 | 1.00 |
| 8192 | 128 | 52444980 | 50.0 | 50.0 | 52692148 | 1156004 | 0.098 | 1.00 |
| 65536 | 16 | 52431108 | 50.0 | 50.0 | 54075100 | 1311923 | 0.109 | 1.00 |

Single bulk round (structure cost with the merge cost removed): same bytes except at group 2
(106.7 vs 82.7 B/row — see the Vec-slack finding), and 4–5× less time at large group sizes
(0.022 s at 8192 vs 0.098 s).

`heap_report()` matches the allocator to **1.00** across the whole sweep, so the estimate
logged by every real index run is trustworthy at whole-store granularity (the ≤3-element
overestimate above is real but too small a slice to show here).

### Findings

1. **Cost per row is dominated by group *count*, not row count.** 1 leaf/group costs 213 B/row;
   64 leaves/group costs 25.8 B/row — an 8× spread at identical row count. The per-group price
   is the outer-table entry (48 B + load-factor slack) plus one heap allocation.
2. **The `Large` promotion doubles memory per leaf.** Crossing `GROUP_HASHSET_THRESHOLD = 64`
   takes bytes/row from **25.8 → 51.0** (1.97×) and it never comes back: a `Large` group holds
   24 B leaves in a power-of-two bucket array at ≤87.5 % load, ≈50 B/leaf, against the `Vec`'s
   24 B/leaf. That is the price of the O(delta) merge, and it is paid by *every* promoted
   group, including ones that promote and then stop growing.
3. **The threshold is set at the right place for time.** Group size 64 — the largest
   *un-promoted* group — is the slowest configuration in the whole sweep (0.273 s); every
   promoted size above it is faster (0.222 → 0.098 s) despite doing the same number of inserts.
   Raising the threshold would extend the quadratic re-copy region; lowering it would buy time
   at 2× memory for more groups.
4. **`Small` groups pay `Vec` doubling slack, up to 2×, and it depends on how the group was
   built.** `Vec::insert` doubles capacity (a 5-leaf group sits in a 8-slot buffer, a 20-leaf
   group in 32), while `merge_sorted` allocates `with_capacity(exact)`. Hence group 20 costs
   42.4 B/row while the *larger* group 32 costs 27.7, and bulk-built group 2 costs 106.7 B/row
   where the incrementally-merged one costs 82.7. A `shrink_to_fit` on `Small` groups after the
   fixpoint (or before the result is handed back) would recover 10–40 % on non-power-of-two
   groups for one pass over the store.
5. **`delta`/`new` retain their widest outer map forever.** `absorb` uses `fwd.drain()`, which
   empties a hashbrown table without freeing it. In the pessimal sweep that is 1.5× the
   steady-state store at group 1 (312.7 vs 213.4 B/row); it shrinks as groups grow (50.0 vs
   50.0 B/row at 8192) because the delta then touches far fewer keys. Real runs sit between
   these, but the retention is unconditional: whatever the widest iteration reached is held to
   the end. `from.fwd = Map::default()` instead of `drain()` after the merge would release it.

---

## 4. Tier 2 — end-to-end (`ctadl index` on generated Flowy programs)

~1 M `locals` rows per configuration, group size swept. `store MB` is `heap_report()`;
`fixpoint s` is the wall time of the SCC holding the `locals` rules (Ascent's own
`#![measure_rule_times]`); `peak fp` is the process's peak physical footprint, sampled at
20 ms. Repeat runs agree to ±1 % on `fixpoint s` and ±4 % on `peak fp`.

| group | funcs | rows | groups | max | mean | large | store MB | B/row | fixpoint s | iters | peak fp MB | wall s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 142857 | 999999 | 857142 | 2 | 1.17 | 0 | 143.2 | 150 | 1.009 | 5 | 1037.2 | 3.91 |
| 2 | 83333 | 1083329 | 749997 | 3 | 1.44 | 0 | 133.6 | 129 | 1.068 | 6 | 735.8 | 3.39 |
| 4 | 45454 | 1045442 | 590902 | 5 | 1.77 | 0 | 113.6 | 114 | 0.845 | 6 | 696.1 | 2.58 |
| 8 | 23809 | 1023787 | 499989 | 9 | 2.05 | 0 | 110.0 | 113 | 0.706 | 6 | 591.2 | 2.11 |
| 16 | 12195 | 1012185 | 451215 | 17 | 2.24 | 0 | 83.7 | 87 | 0.602 | 6 | 477.0 | 1.82 |
| 32 | 6172 | 1006036 | 425868 | 33 | 2.36 | 0 | 82.7 | 86 | 0.606 | 6 | 489.3 | 1.74 |
| **63** | 3154 | 1002972 | 413174 | **64** | 2.43 | **0** | **82.5** | **86** | 0.624 | 6 | 345.3 | 1.72 |
| **64** | 3105 | 1002915 | 412965 | **65** | 2.43 | **3105** | **87.1** | **91** | 0.615 | 6 | 365.5 | 1.72 |
| **66** | 3012 | 1002996 | 412644 | 67 | 2.43 | **9036** | **96.0** | **100** | 0.621 | 6 | 406.3 | 1.73 |
| 128 | 1557 | 1001151 | 406377 | 129 | 2.46 | 4671 | 96.8 | 101 | 0.587 | 6 | 379.5 | 1.65 |
| 512 | 390 | 999570 | 401310 | 513 | 2.49 | 1170 | 96.6 | 101 | 0.570 | 6 | 364.5 | 1.64 |
| 2048 | 97 | 993571 | 397797 | 2049 | 2.50 | 291 | 96.1 | 101 | 0.560 | 6 | 315.8 | 1.62 |
| 8192 | 24 | 983112 | 393336 | 8193 | 2.50 | 72 | 95.4 | 102 | 0.531 | 6 | 309.6 | 1.60 |

The promotion cliff reproduces end-to-end: `max_group` 64 → 82.5 MB / 86 B/row with zero
promotions; 65 → 87.1 MB (one promoted group per function); 67 → 96.0 MB / 100 B/row (three).
A **+16 % whole-store step** from promoting groups that hold well under half the rows.

Secondary knob (`--paths`, at group size 32, ~1 M rows): 1/2/4 paths all give 82.7 MB and
0.61–0.62 s; 8 and 16 paths give 107.2 / 111.7 MB and 0.68 / 0.72 s — not because paths are
expensive per se, but because more distinct field paths create more SSA variables, hence more
groups (469 k / 518 k vs 426 k). Consistent with finding 1: group count is the cost driver.

### A/B against Ascent's default relation storage

Same programs, same harness, against a build with `#[ds(crate::index_engine::locals_trie)]`
removed from the `locals` declaration (the harness parses either build — with no store, it
falls back to the stats line). Recipe: delete that attribute, then replace the two
`prog.__locals_ind_common` uses in `index_engine/mod.rs` (the `heap_report()` log, and
`num_reached_variables()` → a `HashSet` over `prog.locals`), and `cargo build --release`.

| group | trie fixpoint s | default fixpoint s | trie peak fp MB | default peak fp MB | footprint saved |
|---|---|---|---|---|---|
| 1 | 1.009 | 0.997 | 1037.2 | 1503.8 | 31 % |
| 2 | 1.068 | 1.008 | 735.8 | 1154.0 | 36 % |
| 4 | 0.845 | 0.796 | 696.1 | 1032.7 | 33 % |
| 8 | 0.706 | 0.802 | 591.2 | 929.4 | 36 % |
| 16 | 0.602 | 0.687 | 477.0 | 837.7 | 43 % |
| 32 | 0.606 | 0.724 | 489.3 | 754.0 | 35 % |
| 63 | 0.624 | 0.708 | 345.3 | 700.2 | **51 %** |
| 64 | 0.615 | 0.670 | 365.5 | 704.5 | 48 % |
| 128 | 0.587 | 0.671 | 379.5 | 587.4 | 35 % |
| 512 | 0.570 | 0.659 | 364.5 | 649.8 | 44 % |
| 2048 | 0.560 | 0.666 | 315.8 | 578.0 | 45 % |
| 8192 | 0.531 | 0.619 | 309.6 | 624.4 | 50 % |

The trie cuts peak process footprint by **31–51 %** and is up to 16 % faster from group size 8
up, but 1–6 % slower at group sizes 1–4 (many tiny groups: one allocation and one outer-table
probe per group buys nothing when the group holds a single leaf). Note this is
whole-process footprint, so the front end (parse, SSA, codegen — large here because the
programs have up to 143 k functions) is included in both columns and dilutes the ratio; the
store-only saving is larger.

---

## 5. Suggested follow-ups, in value order

1. **`shrink_to_fit` `Small` groups once at fixpoint** — recovers 10–40 % of the store on
   non-power-of-two groups (finding 4) for one O(groups) pass. No representation change.
2. **Free the drained `delta`/`new` outer maps** (`= Map::default()` rather than leaving a
   drained table) — removes an unconditional retention of the widest iteration's outer table
   (finding 5).
3. **Revisit `GROUP_HASHSET_THRESHOLD` as a memory/time trade, not a time-only one** — it is a
   hard 2× on every promoted group's leaves (finding 2). A promoted group that stops growing
   pays forever; re-demoting on a quiet iteration, or promoting on *growth rate* rather than
   size, would keep the merge win without the standing cost.
4. **Fix `hb_bytes`' `.max(8)` → `.max(4)`** — a 2× overstatement for every ≤3-element table,
   which on real targets is most `fidx` entries (§1).
5. **Correct the module docs** — the "8-bucket minimum" sentence and the "~91 % slack" framing
   both overstate; the measured saving over the nested design is 1.1–2.3× (§1).

## 6. Methodology notes / limitations

* The structure tier substitutes plain integers for the production column types
  (`u32/u64/u64/i16/u64`). Sizes match production exactly — leaf 24 B, key `(F,V)` 16 B,
  group enum 32 B, outer entry 48 B, the same numbers `heap_report` logs — and using the real
  interned types would put *their* allocations into the counter, defeating the measurement.
* The end-to-end tier's `peak fp` is sampled every 20 ms, so a sub-20 ms spike can be missed;
  `peak rss` (also reported) is the child's own rusage high-water mark and needs no sampling.
  Physical footprint, not RSS, is the number that matters on macOS — RSS omits compressed
  pages (see `.claude/skills/measure-process-memory`).
* Row counts across a sweep are held near a target, not exactly equal (the generator's rows
  per function is ~5K+2 for group size K); the actual count is reported per row and all
  per-row figures are normalized by it.
* Generated programs exercise field-sensitive propagation and summaries but have no calls,
  virtual dispatch or aliasing, so `context_locals`, `resolvent` and the hybrid-inlining
  rules stay empty. This isolates the `locals` store; it is not a model of a real target's
  rule mix.
* `mean` group size stays ~2.5 across the sweep because K formals contribute K singleton
  groups for every K-leaf group. That is faithful to real targets (many tiny, few huge) but it
  means the end-to-end tier cannot isolate group-size effects on total bytes the way the
  structure tier (where *every* group has the target size) can. Read the two together.
