# Hybrid set for the `locals` index: implementation and evaluation - DO-NOT-MERGE

Implements `locals-trie-hybrid-ds.md` — a set that is a **linear-probing hashtable** below a size
threshold and, above it, a **Swiss table written here from scratch** (the spec: "implemented like
a `hashbrown::HashTable`… do not actually use the `HashTable` type or any built-in hashtable").
It is evaluated three ways: the structure on its own, the `locals` store built from it, and
`ctadl index` on generated programs.

`SMALL_THRESHOLD` is **64**, the spec's value and the largest the `u64` occupancy bitmask admits;
§6 is the measurement behind it.

Same machine as `locals-trie-benchmark.md`: Apple M1 Ultra (20 cores, 128 GB), macOS 26.5.2
(arm64), rustc 1.94.1, hashbrown 0.16.1, `--release`.

**Two baselines, kept separate.** The *representation* baseline is what shipped before any of
this: a sorted `Vec` per `(F,V)` group promoted to a `HashSet` past 64 leaves
(`locals-trie-benchmark.md` §§3–4). The *increment* baseline is the previous revision of this
work, which was the same hybrid set with `hashbrown::HashTable` as its large half. Everything
below that says "baseline" without qualification means the second one, because that is the only
thing this change touches; the first is carried in the `vec64`/`vec32` columns of §3.

**Headline.** Replacing the library table with one written here takes 8 bytes off *every* group,
promoted or not: the whole set is now **24 B instead of 32**, because a table that keeps no
`growth_left` and counts in `u32` fits in two words instead of four. End to end the `locals` store
is a further **5.8–9.5 % smaller** than the hashbrown-backed version — on top of the 23–35 % that
version already saved against the sorted-`Vec` store — and the fixpoint is **unchanged to 7.7 %
faster**. Standalone, the hand-written table allocates the same bytes as hashbrown at every size
≥ 5 elements, inserts up to 1.9× faster, merges 1.2–1.7× faster, and looks up in the same time.

> **§§1–9 describe increment 1** — the revision in which `HybridSet` was still an `enum` over a
> `Probe` arm and a `SwissTable` arm. **§10 is increment 2**, which removed that enum: the two
> representations became one 16 B structure whose regime is read off its own capacity. Everything
> §§1–9 says about the *algorithms* (both probe schemes, the sizing rules, the promotion
> protocol, the threshold) is unchanged by it; the struct widths and the store bytes it quotes
> are superseded by §10.

---

## 1. What was built

| file | role |
|---|---|
| `ctadl-ascent/src/index_engine/hybrid_set/swiss.rs` | **new** — `SwissTable<T>`, the from-scratch open-addressed table, + 7 unit tests |
| `ctadl-ascent/src/index_engine/hybrid_set.rs` | `HybridSet<T, S>`: probe table below the threshold, `SwissTable` above; 9 unit tests |
| `ctadl-ascent/src/index_engine/locals_trie.rs` | docs only — groups are still `HybridSet<(P,M,Fp)>` and no view changed |
| `ctadl-ascent/benches/hybrid_set.rs` | tier 0 — now five contenders: the raw table is measured on its own against `hashbrown::HashSet` |

### The table

`SwissTable` is the SwissTable design rebuilt: one control byte per bucket holding `h2` (the top
7 bits of the hash), scanned `GROUP_WIDTH = 8` buckets at a time inside a `u64` with the
word-parallel zero-byte trick, over a power-of-two bucket array held at ≤ 87.5 % load, with
quadratic probing over groups. One allocation holds the elements and the control bytes, the
control array carries the `GROUP_WIDTH`-byte mirror that makes a group loadable at any index, and
the element array is indexed *backwards* from the control pointer so a single pointer describes
the whole table — hashbrown's layout, byte for byte.

Three things are deliberately **not** hashbrown:

1. **No tombstones.** A Datalog index only grows. With no `DELETED` state, "empty" is a single
   high-bit test, the first empty bucket in a probe sequence *is* the insertion point (no
   `fix_insert_slot`), and a resize is always a fresh allocation and one linear pass — hashbrown's
   in-place rehash exists only to reclaim tombstones.
2. **Two words, not four.** hashbrown's `RawTable` is `{bucket_mask, ctrl, growth_left, items}`.
   Dropping tombstones makes `growth_left` derivable (`capacity − items`), and `u32` counters
   cover 4 G buckets, so the table is `{ctrl: NonNull<u8>, bucket_mask: u32, items: u32}` = **16 B**.
   That is what shrinks `HybridSet` from 32 B to 24 B: rustc niche-fills the large variant into
   the 16 bytes beside the small variant's non-null pointer. The outer map entry
   `((F,V), Group)` goes **48 B → 40 B**, which is the whole of §4's memory result.
3. **One group implementation**, the portable word-parallel one. hashbrown picks SSE2/NEON/word
   by target feature; on aarch64 — where this is measured — its choice is also 8 bytes wide, so
   the two are equivalent here. On x86-64 hashbrown would scan 16 buckets per step where this
   scans 8 (§8).

Bucket counts follow hashbrown's `capacity_to_buckets` exactly, with one exception: the floor is
**8 buckets, not 4**. A 4-bucket table's control array is shorter than a group, which is what
forces hashbrown's wrap fixup on the insert path; nothing here needs it, because a `HybridSet`
only builds a Swiss table once it holds 65 elements (128 buckets). The cost is confined to
standalone use at 1–3 elements, and it is visible in §3's `swiss` column.

Two unit tests pin the modelling rather than just the behaviour: one grows a real
`hashbrown::HashSet` and a `SwissTable` side by side over 2000 inserts and asserts the bucket
count and the **allocation size** agree at every step; the other pins the sizing table and the
layout formula. The remaining tests are a `BTreeSet` model check across every size through the
first growth steps, absent-element checks at every size to 200, `with_capacity`/`reserve`
no-regrowth, drop-exactly-once through growth / clone / partial `into_iter` / drop, and
`size_of::<SwissTable<_>>() == 2 * size_of::<usize>()`. `cargo test -p ctadl-ascent` is green
(200 lib tests, 16 of them here); `cargo clippy --all-targets` is clean under the workspace's
`undocumented_unsafe_blocks = "deny"`.

### What did not change

`SMALL_THRESHOLD`, the small representation (linear probing with the occupancy map as an inline
`u64` bitmask, slots starting at 1 and doubling, load factor allowed to reach 1.0), the promotion
protocol (one pass over ≤ 64 elements into a right-sized table, elements moved not cloned),
`merge`'s insert-the-smaller-side rule, and every `locals_trie` view. The store's `heap_report`
now reports the group's *actual* allocation rather than an estimate of hashbrown's, since the
structure computes its own layout; `hb_bytes` is still used for the two enclosing hashbrown maps.

---

## 2. Tier 0 — the table on its own, against hashbrown

`cargo bench -p ctadl-ascent --bench hybrid_set`, min of 2 runs. `swiss` is `SwissTable` used at
every size; `hash` is `hashbrown::HashSet` at every size. 24 B elements, 1 M elements per row
spread over `1 M / n` sets, bytes from a counting global allocator.

| n | B/elem swiss / hash | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | 208.0 / **108.0** | 32.7 / **24.2** | 17.9 / **2.9** | 9.6 / **2.3** | 61.9 / **56.7** |
| 3 | 69.3 / **36.0** | **8.3** / 9.1 | 2.8 / **2.6** | 4.8 / **1.9** | **36.3** / 45.2 |
| 5 | **41.6 / 41.6** | **6.3** / 22.1 | 2.8 / **2.7** | **3.3** / 3.9 | **35.4** / 51.8 |
| 8 | **51.0 / 51.0** | **16.3** / 30.4 | 2.9 / **2.7** | **3.3** / 4.1 | **40.8** / 60.0 |
| 16 | **50.5 / 50.5** | **16.8** / 31.4 | 2.8 / **2.7** | **2.6** / 3.2 | **33.2** / 50.3 |
| 32 | **50.2 / 50.2** | **16.3** / 23.6 | **2.9 / 2.9** | **2.4** / 2.6 | **29.1** / 49.9 |
| 48 | **33.5 / 33.5** | **10.2** / 14.8 | 2.8 / **2.6** | **3.3** / 3.7 | **21.9** / 36.0 |
| 64 | **50.1 / 50.1** | **16.4** / 17.4 | 2.9 / **2.7** | **2.2** / 2.5 | **33.5** / 45.1 |
| 128 | **50.1 / 50.1** | 15.7 / **15.1** | **4.0 / 4.0** | **2.7** / 3.2 | 28.4 / **37.7** |
| 1024 | **50.0 / 50.0** | **10.9** / 13.0 | **4.5** / 4.8 | **2.0** / 2.7 | **25.1** / 31.2 |

**Findings.**

1. **The layout is hashbrown's.** From 5 elements up the two allocate *identical* bytes at every
   size in the sweep — 41.6, 51.0, 50.5, 50.2, 33.5, 50.1, 50.0 B/element — which is the same fact
   the lockstep unit test checks against a live hashbrown table for 2000 inserts. Below 5 the
   8-bucket floor costs 2× (208 vs 108 B at one element); that is the deliberate simplification
   of §1, and it is the range the hybrid never gives this table.
2. **Insert is 1.05–1.9× faster than hashbrown** from 8 elements up — and 3.5× at 5 elements,
   where hashbrown grows a table this one's 8-bucket floor already sized — with merge 1.2–1.7×
   faster throughout. The likely reasons are all removals of work hashbrown must do for generality: no `growth_left`
   to maintain, no tombstone handling on the insert path, and equality on whole elements (`T: Eq`)
   instead of an indirect `eq` closure. Lookups are a wash — within ±0.3 ns on hits and misses,
   which is what one expects when the probe sequence and the control-byte test are the same
   algorithm.
3. **Above the 8-bucket floor there is no size where hashbrown is materially better.** Its only
   wins in the table are sub-nanosecond: `ins` at n=128 (15.1 vs 15.7) and hits at 1–64 elements
   (≤ 0.3 ns). Below the floor it wins on everything, which is exactly why the hybrid exists and
   why the floor is allowed to be crude.

---

## 3. Tier 0, continued — the hybrid set against its alternatives

Same run, all five contenders. `vec64` is the representation `locals_trie` shipped before this
work (sorted `Vec` under 64, `HashSet` above); `hash` is hashbrown at every size. Best in bold.

| n | B/elem: hybrid / swiss / vec64 / hash | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | **24.0** / 208.0 / 96.0 / 108.0 | **15.9** / 32.7 / 23.7 / 24.2 | **2.0** / 17.9 / 2.4 / 2.9 | **1.6** / 9.6 / 2.1 / 2.3 | **17.9** / 61.9 / 21.9 / 56.7 |
| 2 | **24.0** / 104.0 / 48.0 / 54.0 | 20.0 / 17.9 / **10.6** / 11.7 | **2.3** / 13.6 / 3.0 / 2.6 | 2.6 / 7.5 / 2.8 / **2.0** | **31.5** / 38.0 / 35.6 / 47.1 |
| 3 | **32.0** / 69.3 / **32.0** / 36.0 | 22.4 / **8.3** / 9.3 / 9.1 | 2.9 / 2.8 / 4.2 / **2.6** | 3.0 / 4.8 / 3.5 / **1.9** | 38.0 / **36.3** / 43.5 / 45.2 |
| 5 | **38.4** / 41.6 / **38.4** / 41.6 | 22.2 / **6.3** / 21.9 / 22.1 | 3.1 / 2.8 / 6.5 / **2.7** | **2.6** / 3.3 / 3.7 / 3.9 | 40.7 / **35.4** / 52.9 / 51.8 |
| 8 | **24.0** / 51.0 / **24.0** / 51.0 | **15.3** / 16.3 / 17.7 / 30.4 | **2.2** / 2.9 / 6.3 / 2.7 | 4.7 / 3.3 / **3.6** / 4.1 | **40.2** / 40.8 / 61.1 / 60.0 |
| 16 | **24.0** / 50.5 / **24.0** / 50.5 | **12.8** / 16.8 / 25.8 / 31.4 | **2.4** / 2.8 / 8.7 / 2.7 | 9.5 / **2.6** / 5.1 / 3.2 | 38.9 / **33.2** / 48.8 / 50.3 |
| 24 | **32.0** / 33.7 / **32.0** / 33.7 | 14.4 / **12.7** / 29.1 / 19.9 | **1.9** / 2.7 / 10.5 / 2.5 | **2.5** / 2.7 / 4.3 / 3.0 | 39.7 / **27.9** / 41.4 / 35.8 |
| 32 | **24.0** / 50.2 / **24.0** / 50.2 | **10.4** / 16.3 / 27.0 / 23.6 | 3.4 / **2.9** / 10.7 / **2.9** | 19.9 / **2.4** / 4.3 / 2.6 | 31.3 / **29.1** / 40.4 / 49.9 |
| 48 | **32.0** / 33.5 / **32.0** / 33.5 | **10.0** / 10.2 / 32.4 / 14.8 | **1.9** / 2.8 / 14.0 / 2.6 | **2.2** / 3.3 / 5.9 / 3.7 | 29.9 / **21.9** / 47.3 / 36.0 |
| 64 | **24.0** / 50.1 / **24.0** / 50.1 | **9.3** / 16.4 / 33.3 / 17.4 | 3.1 / 2.9 / 12.9 / **2.7** | 42.8 / **2.2** / 5.3 / 2.5 | **24.5** / 33.5 / 43.7 / 45.1 |
| 65 | 49.4 / 49.4 / 49.4 / 49.4 | 20.4 / **13.1** / 40.5 / 16.9 | 2.9 / 3.0 / **2.8** / **2.8** | **1.8** / 2.0 / 2.3 / 2.3 | 33.6 / **32.3** / 55.3 / 42.3 |
| 128 | 50.1 / 50.1 / 50.1 / 50.1 | 17.8 / 15.7 / 27.0 / **15.1** | 4.0 / 4.0 / **2.7** / **2.7** | 2.9 / **2.7** / 3.1 / 3.2 | **26.6** / 28.4 / 37.9 / 37.7 |
| 1024 | 50.0 / 50.0 / 50.0 / 50.0 | 16.3 / **10.9** / 14.7 / 13.0 | 6.0 / 4.5 / **4.4** / 4.8 | 2.3 / **2.0** / 2.6 / 2.7 | 27.8 / **25.1** / 36.3 / 31.2 |

**Findings.**

4. **The hybrid is unchanged where it was already winning, which is the point.** Against the
   *previous* revision (hashbrown as the large half), the `hybrid` column is byte-identical at
   every size and within noise on time — 15.9 vs 16.2 ns insert at n=1, 27.8 vs 30.7 merge at
   n=1024, no size differing by more than the run-to-run spread. The change is invisible here by
   construction: the two large representations allocate the same bytes (finding 1) and the small
   one was not touched. What it buys is 8 B per *set*, which this benchmark deliberately
   subtracts (it charges the harness's `Vec` of sets to the harness), so the payoff shows up in
   §4 and §5 rather than here.
5. **Below the threshold nothing beats the probe table on memory**: 24 B/element at 1, 2, 8, 16,
   32 and 64 elements against hashbrown's 108, 54, 51, 50.5, 50.2, 50.1 — 2.1–4.5× — while
   matching the sorted `Vec`, whose 24 B/element it ties without the `Vec`'s O(n) insert.
6. **The known weakness is still there and still bounded**: a *miss* against an exactly-full
   probe table degenerates to a scan — 9.5 ns at 16 slots, 19.9 at 32, **42.8 at 64** — against
   ~2.5 ns for either hash table. Load factor 1.0 is what buys the 24 B/element, and §5 shows
   that the workload manufacturing that shape (group size 63, every group exactly full) is among
   the *fastest* configurations measured.
7. **Promotion costs one insert's worth of latency at the boundary**: at n=65 the hybrid's
   insert is 20.4 ns against the raw table's 13.1, because reaching 65 means filling a 64-slot
   probe table and then moving it. At n=128 the gap is already 17.8 vs 15.7 and by n=1024 it is
   the raw table plus noise.

---

## 4. Tier 1 — the `locals` store (exact bytes, counting allocator)

`cargo bench -p ctadl-ascent --bench locals_trie`, 1 M rows per row of the table, group size
swept, driven exactly as Ascent's semi-naive loop drives it. Times are the **median of 9
interleaved passes** (the two binaries alternating); bytes are deterministic and identical across
passes. `est/real` is `heap_report()` against the allocator.

Semi-naive shape, one new leaf per group per round:

| group | groups | base B/row | new B/row | Δ | +delta base | +delta new | base s | new s | est/real |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 1048576 | 141.4 | **125.4** | **−11.3 %** | 240.7 | 208.7 | 0.203 | 0.186 | 1.00 |
| 2 | 524288 | 82.7 | **74.7** | **−9.7 %** | 182.0 | 158.0 | 0.185 | 0.197 | 1.00 |
| 4 | 262144 | 53.4 | **49.4** | **−7.5 %** | 103.0 | 91.0 | 0.186 | 0.164 | 1.00 |
| 8 | 131072 | 38.7 | **36.7** | −5.2 % | 63.5 | 57.5 | 0.153 | 0.155 | 1.00 |
| 16 | 65536 | 31.3 | **30.3** | −3.2 % | 43.7 | 40.7 | 0.143 | 0.141 | 1.00 |
| 32 | 32768 | 27.7 | **27.2** | −1.8 % | 33.9 | 32.4 | 0.117 | 0.117 | 1.00 |
| 64 | 16384 | 25.8 | **25.6** | −1.0 % | 28.9 | 28.2 | 0.112 | 0.117 | 1.00 |
| 128 | 8192 | 51.0 | 50.9 | −0.2 % | 52.5 | 52.2 | 0.148 | 0.152 | 1.00 |
| 512 | 2048 | 50.2 | 50.2 | −0.1 % | 50.6 | 50.5 | 0.137 | 0.137 | 1.00 |
| 8192 | 128 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.117 | 0.118 | 1.00 |
| 65536 | 16 | 50.0 | 50.0 | −0.0 % | 50.0 | 50.0 | 0.138 | 0.134 | 1.00 |

**Findings.**

8. **The saving is exactly the narrower map entry, and it is largest where groups are smallest.**
   At group size 1 there is one outer-map entry per row; the entry went 48 B → 40 B and hashbrown
   holds ~2 buckets per entry, so the prediction is 16 B/row — the measurement is
   141.4 → 125.4, i.e. **16.0 B/row**. The saving decays as `1/group_size` and is gone by group
   128, where the group's own leaves dominate. Nothing else moved: at every group size ≥ 128 the
   two builds allocate the same bytes to the byte, which is finding 1 again at store scale.
9. **Time is a wash** — median deltas from −12 % to +7 % with no trend, and the sign flips
   between adjacent group sizes. An earlier 3-pass reading of the same data showed the new build
   uniformly 15–20 % *slower*; that was one anomalously fast baseline pass being picked up by a
   `min` statistic across all 17 configurations at once. The 9-pass medians are what the table
   reports, and the honest statement is that the change is time-neutral at store level. §5
   finds the same end to end, with one exception it can defend.
10. **`heap_report()` still matches the allocator to 1.00 at every group size.** It is now a
    stronger statement than before: the group term is no longer an estimate of a library's
    layout but the layout this code computes itself, so the two agreeing means the accounting and
    the allocator agree about the same formula.

---

## 5. Tier 2 — end to end, `ctadl index` on generated programs

`scripts/locals-bench.py`, ~1 M `locals` rows per configuration, **3 interleaved passes** of the
whole sweep (baseline binary and new binary alternating over the same generated programs).
`store MB` is `heap_report()` and is bit-identical across passes; `fixpoint s` is the wall time of
the SCC holding the `locals` rules, reported as the median of the three passes.

| group | max | large | groups | base MB | new MB | store Δ | base s | new s | fixpoint Δ |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 2 | 0 | 857142 | 93.0 | **85.0** | **−8.6 %** | 0.918 | 0.912 | −0.7 % |
| 2 | 3 | 0 | 749997 | 92.9 | **84.9** | **−8.6 %** | 1.026 | 0.965 | **−6.0 %** |
| 4 | 5 | 0 | 590902 | 85.2 | **77.2** | **−9.4 %** | 0.820 | 0.803 | −2.1 % |
| 8 | 9 | 0 | 499989 | 84.2 | **76.2** | **−9.5 %** | 0.658 | 0.665 | +1.0 % |
| 16 | 17 | 0 | 451215 | 59.3 | **55.3** | **−6.7 %** | 0.561 | 0.553 | −1.4 % |
| 32 | 33 | 0 | 425868 | 59.1 | **55.1** | **−6.8 %** | 0.536 | 0.539 | +0.5 % |
| 63 | 64 | 0 | 413174 | 54.7 | **50.7** | **−7.3 %** | 0.523 | 0.523 | −0.1 % |
| **64** | 65 | 3105 | 412965 | 59.3 | **55.3** | **−6.7 %** | 0.563 | 0.520 | **−7.7 %** |
| 65 | 66 | 9174 | 412830 | 68.7 | **64.7** | **−5.8 %** | 0.548 | 0.532 | −2.9 % |
| 128 | 129 | 4671 | 406377 | 69.2 | **65.2** | **−5.8 %** | 0.550 | 0.529 | −3.7 % |
| 512 | 513 | 1170 | 401310 | 69.1 | **65.1** | **−5.8 %** | 0.523 | 0.521 | −0.5 % |
| 2048 | 2049 | 291 | 397797 | 68.8 | **64.8** | **−5.8 %** | 0.527 | 0.504 | −4.3 % |
| 8192 | 8193 | 72 | 393336 | 68.4 | **64.4** | **−5.8 %** | 0.495 | 0.487 | −1.8 % |

Three passes is not enough to separate a few percent from this machine's noise, so four
configurations were resampled to **9 interleaved passes** — group sizes 1 and 2 (no group can
promote: the memory change acting alone) and 64 and 128 (groups promote: the table change acting
too):

| group | base med | base range | new med | new range | median Δ |
|---|---|---|---|---|---|
| 1 | 0.945 | 0.917–0.959 | 0.923 | 0.912–0.990 | −2.3 % |
| 2 | 1.032 | 1.014–1.046 | 0.979 | 0.963–1.038 | **−5.2 %** |
| 64 | 0.547 | 0.531–0.578 | 0.529 | 0.503–0.606 | −3.2 % |
| 128 | 0.551 | 0.530–0.601 | 0.542 | 0.516–0.744 | −1.5 % |

Process peak physical footprint (median of 3, sampled at 20 ms) moves by −7 % to +2 % with the
same lack of trend `locals-trie-benchmark.md` §4 reports: at these sizes the front end dominates
the process and dilutes a store-level change.

**Findings.**

11. **The store is 5.8–9.5 % smaller at every configuration, and the number is exactly 8 B per
    outer-map bucket.** At group size 1 the store holds 857 142 groups in a 1 048 576-bucket
    table: 8 B × 1 048 576 = 8.0 MB, and the measurement is 93.0 → 85.0 MB. From group size 16 up
    the group count is stable near 400 k, the outer table is 524 288 buckets, and every
    configuration saves the same **4.0 MB**. Nothing here depends on the *values* in the map, only
    on there being 400 k–900 k of them — which is the shape `locals-trie-benchmark.md` finding 1
    identified as the store's real cost driver.
12. **The fixpoint is 1–5 % faster by median and never slower; only group size 2 is clearly
    outside the noise.** At 9 passes, 8 of 9 new samples at group size 2 sit below every baseline
    sample (−5.2 %), and no group there can promote — so that win is the smaller outer map alone,
    not the table. Group sizes 64 and 128, where 3105 and 4671 groups do promote, come in at
    −3.2 % and −1.5 % with overlapping distributions: consistent with §2 finding 2 reaching the
    workload, but not separable from noise at this sample count. The 3-pass table above reads
    −7.7 % at group size 64; **that is the 3-pass number and it does not survive resampling** —
    the same failure mode as §4 finding 9, in the other direction.
13. **The two wins are independent, and the one that is certain is the memory one.** It needs no
    group to promote — it is largest at group size 1, where the threshold is unreachable — it is
    deterministic, and it is predicted to the byte by the entry-size arithmetic. Against the
    *original* sorted-`Vec` store, the two revisions together take group size 63 from 82.4 MB to
    50.7 MB — **−38 %** — and group size 128 from 96.8 MB to 65.2 MB.

---

## 6. Why the threshold is 64

Unchanged from the previous revision, whose threshold sweep (16 / 32 / 64, measured at both store
and end-to-end level) was not re-run here because nothing in this increment moves the trade-off:
the promotion cliff is the same ~2× at the same place, since §2 finding 1 says the new table
allocates what the old one did. Re-stated because the spec now names 64 outright.

The threshold chooses **how much of the group-size distribution pays the promotion step**, which
is a hard ~2× on bytes per leaf (24 B/leaf in the probe table against ~50 in the Swiss table) and
is never refunded, because without removals a promoted group never demotes. Measured at 16, 32
and 64 on these workloads, every lower value strictly loses memory — threshold 32 gives back
14 % of the store wherever groups land in 33…64 leaves — and nothing pulls the other way, since
the small representation no longer degrades as it fills (that was the sorted `Vec`'s quadratic
merge, and it is gone). So the right value is the largest the `u64` occupancy bitmask allows: 64.

The one cost of the high threshold is finding 6's worst case, which doubles from 32 to 64 slots
(19.9 → 42.8 ns for a miss against an exactly-full table). Group size 63 manufactures exactly
that shape end to end — every group holding exactly 64 leaves in a full 64-slot probe table — and
it is among the fastest configurations in §5 (0.523 s, against a sweep spanning 0.49–0.97). The
cost is real in the microbenchmark and invisible in the workload.

---

## 7. What this says about `locals-trie-benchmark.md`

Read on its own, that document holds up; four of its claims are worth re-checking against the
structure that has since replaced the one it measured.

* **§1 (hashbrown's minimum is 4 buckets, not 8) still holds and still matters**, for two
  structures now. The `hb_bytes`/`hb_buckets` estimator it fixed is what the enclosing `fwd` and
  `fidx` maps are still priced with, and it is what the new table's lockstep test compares
  against: `hb_buckets` reproduces this table's bucket count at every size ≥ 8, which is only
  true because the 4-bucket floor was modelled correctly.
* **Findings 2, 3 and 4 (the `Large` promotion doubling memory; the threshold being "right for
  time"; `Vec` doubling slack) describe a structure that no longer exists.** Finding 2's *number*
  survives — promotion is still ~2× per leaf, for the same power-of-two/load-factor reason — but
  its framing as "the price of the O(delta) merge" is no longer a trade: the representation below
  the threshold now also has an O(delta) merge, so the 2× buys only the constant factor on
  lookups. Finding 3 is dead: group size 64 was the slowest configuration in that sweep because
  it was the largest un-promoted sorted `Vec`; it is now among the fastest.
* **Finding 5 (the drained `delta`/`new` outer maps are never freed) is untouched and is now the
  largest single item left.** §4's `+delta B/row` column is 208.7 against a `total` of 125.4 at
  group size 1 — the delta and new copies still cost two-thirds again as much as the store they
  feed, and `fwd.drain()` is still why.
* **Its §4 claim of ±1 % run-to-run repeatability does not hold on this machine**, and this
  session reproduced the previous one's experience: a `min` statistic over 3 passes produced a
  clean-looking 15–20 % regression that 9 interleaved passes showed to be zero (finding 9).
  Any future revision of that document should say that time claims need interleaved sampling and
  a median, and that only the byte counts are deterministic.

---

## 8. Where the remaining wins are, in value order

1. **Free the drained `delta`/`new` outer maps.** `absorb` empties them with `fwd.drain()`, which
   does not free a hashbrown table, so whatever the widest iteration reached is held to the end:
   §4 measures the delta and new copies at 208.7 B/row against the store's own 125.4 at group
   size 1. `from.fwd = Map::default()` after the merge releases it. This is
   `locals-trie-benchmark.md` finding 5, still the largest untouched item, and it is now larger
   *relatively* than it was, because the store beside it got smaller.
2. **Soften the promotion cliff, which is now this code's to change.** A promoted group pays
   ~50 B/leaf against the probe table's 24 (§6), and the reason is a power-of-two bucket array
   holding 24 B elements. Holding the leaves in a dense array and putting only a `u32` index in
   the buckets would cost ~24 B/leaf plus ~5 B/bucket ≈ 37 B/leaf at 87.5 % load — roughly 1.7×
   less — at the price of one indirection per lookup. Before, this was ruled out by the spec's
   "implemented like a `hashbrown::HashTable`"; now that the table is written here, it is a
   contained change to `swiss.rs` behind the same API.
3. **A scan-only tier below ~4 elements.** The group histogram this store logs says **98.9–100 %
   of groups hold a single leaf** at every group size ≥ 128 (401 706 of 406 377 at group 128;
   393 264 of 393 336 at group 8192), and 83 % at group size 1. At those sizes comparing is
   cheaper than hashing, and the `Probe` representation is already exactly a packed array — the
   change is to skip the hash when `slots <= 4`. What it would recover is the small-`n` insert
   cost in §3's table: 22.4 ns at 3 elements, against 9.1 for hashbrown.
4. **A 16-wide group on x86-64.** The word-parallel group is the only implementation, so on
   x86-64 this scans 8 buckets per probe step where hashbrown's SSE2 group scans 16. Adding an
   SSE2 `Group` behind the same three methods (`match_byte`, `match_empty`, `match_full`) is
   mechanical; it was left out because it cannot be tested on this machine.
5. **Run the unsafe code under Miri**, which needs a nightly toolchain this environment does not
   have (§9).
6. **`assign_like_trie` still stores `Map<(F,Vs), Vec<(Vd,Pd,Ps)>>`** and was left alone. Same
   shape of problem; the same `HybridSet` drops into it.

---

## 9. Method notes / limitations

* Tier 0 and tier 1 use plain integers at production element sizes (leaf 24 B, key 16 B) so the
  counting allocator sees only the structure under test, as in `locals-trie-benchmark.md` §6.
* The baseline binaries are the previous revision built from the same tree at the same optimization
  settings, kept aside and run alternately with the new ones, so both see the same machine state.
  As a cross-check on comparing sessions at all: this session's baseline store bytes reproduce the
  previous session's *new* column exactly (54.7 MB at group size 63, 69.2 at 128, 93.0 at 1), so
  the "−38 % against the sorted `Vec`" chain in §5 finding 13 is a chain of like-for-like
  measurements, not a mix of runs.
* Sampling differs by tier and is stated per tier: tier 0 is the min of 2 runs (per-element times
  over 1 M elements, so within-run averaging is already heavy), tier 1 the median of 9 interleaved
  passes, tier 2 the median of 3 for the full sweep and of 9 for the four configurations in the
  second table of §5.
* **The group is 8 bytes wide on every platform**, because the word-parallel implementation is the
  only one. On aarch64 that matches hashbrown; on x86-64 hashbrown's SSE2 group scans 16 buckets
  per probe step, so the insert/lookup advantage in §2 should not be assumed to carry there. The
  *bytes* are platform-independent and 8 B/table smaller than hashbrown's on x86-64, where its
  control array carries a 16-byte mirror.
* The unsafe code is covered by unit tests with `debug_assert`s active (bounds, probe termination,
  control-byte invariants) but **not by Miri** — no nightly toolchain is installed in this
  environment. That is the gap a reviewer should close before this leaves DO-NOT-MERGE status.
* Tier 2's timing protocol (interleaved passes, medians) and the generator's shape caveats are as
  in `locals-trie-benchmark.md` §6: generated programs exercise field-sensitive propagation and
  summaries but have no calls, virtual dispatch or aliasing, and the singleton-heavy group
  distribution is partly a property of the generator.

---

## 10. Increment 2 — removing the enum

§§1–9 describe a `HybridSet` that was an `enum Repr<T> { Small(Probe<T>), Large(SwissTable<T>) }`
— plus an `enum IterInner` and an `enum IntoIterInner` behind its two iterators. This increment
removes all three. The two representations are now one structure, [`raw::RawTable`], whose fields
mean one thing below the threshold and another above it, with the regime read off a number the
table already stores.

**Increment baseline.** For this section "baseline" means the §§1–9 revision. Its bytes are the
`new` columns of §4 and the `hybrid` column of §3, measured on the same machine; store bytes are
deterministic and identical across passes (§4), so those comparisons are exact. Its *source* was
not committed, so no baseline binary could be built for this session — the time comparisons below
are this session's tier-0 numbers against the recorded tier-0 numbers, not interleaved A/B, and
§9's warning about un-interleaved timing applies to them in full. No tier-2 run was made.

### What changed

| file | role |
|---|---|
| `ctadl-ascent/src/index_engine/hybrid_set/raw.rs` | **new** — `RawTable<T, SMALL>`, the one structure, both regimes, and the shared `RawIter` |
| `ctadl-ascent/src/index_engine/hybrid_set/swiss.rs` | now the Swiss *mechanism* only (`Group`, `BitMask`, `ProbeSeq`, `h1`/`h2`, sizing); the `SwissTable` type is gone, dissolved into `raw.rs` |
| `ctadl-ascent/src/index_engine/hybrid_set.rs` | `HybridSet<T, S, SMALL>` = `RawTable` + a `BuildHasher`; 12 unit tests |
| `ctadl-ascent/src/index_engine/locals_trie.rs` | docs only — the group type alias and every view are unchanged |
| `ctadl-ascent/benches/hybrid_set.rs` | the `swiss` column is now `HybridSet<Leaf, _, 0>` rather than a separate type |

`cargo test -p ctadl-ascent` is green (200 lib tests, 20 in `index_engine`) in debug and release;
`cargo clippy --all-targets` is clean under the workspace's `undocumented_unsafe_blocks = "deny"`.

| | baseline | now |
|---|---|---|
| representation | `enum Repr { Small(Probe), Large(SwissTable) }` | one `RawTable<T, SMALL>` |
| `size_of::<HybridSet<_>>()` | 24 B | **16 B** |
| outer map entry `((F,V), Group)` | 40 B | **32 B** |
| small metadata | `u64` bitmask **inline in the struct** | `u64` bitmask **at the head of the allocation** |
| small allocation | `slots * 24` | `slots * 24 + 8` |
| large representation | unchanged — same control bytes, sizing, probing | |
| iterators | 2 enums over 3 iterator types | one `RawIter` |
| threshold | `const SMALL_THRESHOLD: usize` | `const SMALL` type parameter, defaulting to 64 |

The pointer convention is what makes it one structure: **both** regimes put their metadata at
`ptr` and their elements *below* it, so `bucket(i) = ptr - (i+1)*size_of::<T>()` — hashbrown's
backwards-indexed element array — addresses either one, and one layout function, one `allocate`,
one `Drop`, one `Clone`, one `rebuild` and one iterator serve both. `is_large()` is `cap > SMALL`:
small slot counts are at most `SMALL` and large bucket counts are at least `2*SMALL`
(`buckets_for(SMALL+1) >= 2*SMALL` for every power-of-two `SMALL`, so the floor never rounds a
table up to reach it), and the two ranges cannot meet. `len` and `capacity` no longer branch at
all; growth and promotion are the same call with a different number.

Making the threshold a type parameter is what lets the `swiss` column of §3 keep existing without
a second table type: it is now `HybridSet<Leaf, _, 0>`, the same structure with the small regime
compiled out. The four hashbrown-parity tests moved onto it unchanged.

### The trade this pays for

Moving the occupancy word out of the struct and into the allocation is what buys the 8 bytes, and
it costs two things:

* **+8 B of heap per small set**, which is why the small rows of the tier-0 table below get
  *worse* on B/elem — that bench charges the harness's `Vec` of sets to the harness, so it sees
  the cost and not the saving. At store level the two land together and the saving wins (§10.2).
* **One memory load per small probe.** The occupancy map used to be a struct field; it is now a
  load from the head of the set's own allocation. It is still *one* load covering the whole table
  — the probe loop after it is register bit tests, and a Swiss table reloads control bytes once
  per group — and for the 1–2-element sets that dominate it shares a cache line with the elements.
  It is not free: hits cost ~0.6–0.9 ns more at every small size.

### 10.1 Tier 0 — the structure on its own

`cargo bench -p ctadl-ascent --bench hybrid_set`, min of 2 runs, same protocol as §§2–3. `before`
is §3's `hybrid` column. B/elem is the set's own allocation only.

| n | B/elem before → now | ins ns | hit ns | miss ns | merge ns |
|---|---|---|---|---|---|
| 1 | 24.0 → 32.0 | 15.9 → 16.8 | 2.0 → 2.9 | 1.6 → 1.9 | 17.9 → 18.9 |
| 2 | 24.0 → 28.0 | 20.0 → 22.2 | 2.3 → 3.2 | 2.6 → 3.0 | 31.5 → 34.7 |
| 3 | 32.0 → 34.7 | 22.4 → 25.1 | 2.9 → 3.6 | 3.0 → 3.3 | 38.0 → 41.8 |
| 5 | 38.4 → 40.0 | 22.2 → 24.3 | 3.1 → 4.1 | 2.6 → 2.9 | 40.7 → 44.7 |
| 8 | 24.0 → 25.0 | 15.3 → 16.3 | 2.2 → 2.7 | 4.7 → 4.7 | 40.2 → 43.6 |
| 16 | 24.0 → 24.5 | 12.8 → 13.8 | 2.4 → 3.1 | 9.5 → 9.4 | 38.9 → 40.8 |
| 32 | 24.0 → 24.3 | 10.4 → 11.1 | 3.4 → 4.1 | 19.9 → 19.3 | 31.3 → 34.2 |
| 64 | 24.0 → 24.1 | 9.3 → 9.5 | 3.1 → 3.7 | 42.8 → 39.6 | 24.5 → 25.3 |
| 65 | 49.4 → 49.4 | 20.4 → 19.0 | 2.9 → 4.6 | 1.8 → 2.0 | 33.6 → 34.7 |
| 128 | 50.1 → 50.1 | 17.8 → 17.2 | 4.0 → 4.7 | 2.9 → 2.7 | 26.6 → 26.3 |
| 1024 | 50.0 → 50.0 | 16.3 → 14.6 | 6.0 → 5.7 | 2.3 → 2.1 | 27.8 → 24.7 |

**Findings.**

14. **The small path costs ~0.6–0.9 ns per lookup and ~1–3 ns per insert**, consistently, at every
    size below the threshold and in both runs. That is the metadata load, and it is the whole of
    the time price. It is the only regression in this increment.
15. **The large path is unchanged**, as it must be — no large-regime code was touched. At n=1024
    the `swiss` column reads 11.0 ns insert / 4.4 hit / 26.5 merge against §2's recorded 10.9 /
    4.5 / 25.1, and B/elem is identical at every size ≥ 65.
16. **The miss worst case did not move**: 9.4 / 19.3 / 39.6 ns at 16 / 32 / 64 slots against the
    baseline's 9.5 / 19.9 / 42.8. Load factor 1.0 is still what buys the 24 B/element and still
    what costs this; §6 finding 6 stands verbatim.
17. **`B/elem` gets worse below the threshold by exactly 8 B per set**, which is the added
    occupancy word — 24.0 → 32.0 at n=1 (one set, one slot), 24.0 → 28.0 at n=2, 24.0 → 24.125 at
    n=64. §10.2 is where that is paid back.

### 10.2 Tier 1 — the `locals` store (exact bytes)

`cargo bench -p ctadl-ascent --bench locals_trie`, 1 M rows, semi-naive shape. Bytes only: they
are deterministic, so this is an exact comparison against §4's `new` column.

| group | groups | before B/row | now B/row | Δ | predicted Δ | +delta before → now |
|---|---|---|---|---|---|---|
| 1 | 1048576 | 125.4 | **117.4** | **−6.4 %** | −8.0 | 208.7 → **184.7** |
| 2 | 524288 | 74.7 | **70.7** | **−5.4 %** | −4.0 | 158.0 → **138.0** |
| 4 | 262144 | 49.4 | **47.4** | −4.0 % | −2.0 | 91.0 → **81.0** |
| 8 | 131072 | 36.7 | **35.7** | −2.7 % | −1.0 | 57.5 → **52.5** |
| 16 | 65536 | 30.3 | **29.8** | −1.7 % | −0.5 | 40.7 → **38.2** |
| 32 | 32768 | 27.2 | **26.9** | −1.1 % | −0.25 | 32.4 → **31.1** |
| 64 | 16384 | 25.6 | **25.5** | −0.4 % | −0.125 | 28.2 → **27.6** |
| 128 | 8192 | 50.9 | **50.7** | −0.4 % | −0.125 | 52.2 → **51.8** |
| 512 | 2048 | 50.2 | 50.2 | −0.0 % | −0.03 | 50.5 → **50.4** |
| 8192 | 128 | 50.0 | 50.0 | −0.0 % | −0.002 | 50.0 → 50.0 |

**Findings.**

18. **The saving is predicted to the byte, and it is a race between two 8-byte terms.** At group
    size 1 the store holds 1 048 576 groups in a 2 097 152-bucket table: the entry went 40 B → 32 B,
    so the map gives back 8 B × 2 buckets/row = **−16 B/row**, and each of the 1 048 576 sets adds
    its occupancy word, **+8 B/row**. Net −8.0, measured 125.4 → 117.4 = **−8.0**. The same
    arithmetic reproduces every row of the table: −4.0 at group 2, −2.0 at group 4, −1.0 at
    group 8. The map term wins because hashbrown holds ~2 buckets per entry while a set is one
    object — the enum's width was being paid 2× per group.
19. **It decays as `1/group_size` and is gone by group 512**, exactly as §4 finding 8 found for
    the previous increment: past group 128 every group is promoted, the added occupancy word does
    not exist (large tables were untouched), and only the outer map's −8 B/entry remains, which is
    0.03 B/row at 2048 groups.
20. **The delta/new copies improve by more than the store does** — 208.7 → 184.7 B/row at group
    size 1, −24 B/row against the store's −8. Those copies hold the same groups in their own outer
    maps, so they pay the −16 B/row map term again while paying the +8 B/row set term only once
    per distinct set. It is the same effect, counted over more maps; `locals-trie-benchmark.md`
    finding 5 (the drained maps are never freed) is why they are there to improve at all, and
    remains item 1 of §8.
21. **`heap_report()` still matches the allocator to 1.00 at every group size**, which is a check
    on the new layout function: the report now prices a small set as `(slots*24).next_multiple_of(8) + 8`
    and a large one as before, and the counting allocator agrees.

### 10.3 What was not done

* **No tier 2 run.** §5's end-to-end sweep needs a baseline binary to interleave against and the
  baseline source is not in git. The tier-1 bytes above are deterministic and the tier-0 times are
  the honest cost statement; the fixpoint effect of this increment is **unmeasured**. The
  prediction from §5 finding 11's arithmetic is a further ~4 MB off the ~400 k-group configurations
  and ~8 MB off group size 1, since the entry shrank by the same 8 B again — but that is
  arithmetic, not a measurement, and the +8 B/set term partly offsets it wherever groups are small.
* **Still no Miri** (§9). This increment adds unsafe code — a second metadata layout over the same
  allocation — so that gap is now larger, not smaller. The `debug_assert`s were extended to cover
  it (regime agreement in `set_ctrl`/`set_occupancy`, the large-cap floor in `allocate`, the
  occupancy/`len` agreement in `raw_iter`) and the full suite is green in debug and release, but
  that is not the same thing.
* **The threshold sweep was not re-run.** §6's argument is unchanged: this increment moves neither
  the promotion cliff nor the small representation's load factor.
