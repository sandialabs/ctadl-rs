# `locals` prefix-sharing (trie) storage — prototype status & resume notes

**Branch:** `path-and-closure-queries`
**Goal:** Cut `ctadl index` peak memory so 1–4 MB firmware binaries analyze without the
~50 GB RAM pain threshold, **with aliasing enabled** (correctness) and **byte-identical
results**. Memory is measured by macOS `phys_footprint` peak (NOT RSS — cold pages compress).

Status: **prototype complete, correctness-verified, measured on 5 benchmarks.** Delivers a
consistent **~2–3× peak-memory reduction**. Next step is *compact nodes* to push toward the
modeled ~4×. This doc is the resume point.

---

## 1. The problem being optimized

`locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is the dominant memory consumer
of the index phase. As a normal Ascent relation it is stored **~6× over**:

- the physical `Vec`, plus
- indices `none`, `0_1`, `0_1_2`, the full existence index `0_1_2_3_4`, and the inverse `0_3_4`.

Every index stores its **value columns inline**, so the full 5-column tuple is replicated
many times. Column widths: `FunctionId`=u32, `FlowVariable`=u64 (tagged), `Path`=8-byte
`tailshare::Seq` handle, `FormalIndex`=i16.

## 2. What the prototype does

A single Ascent **BYODS** ("bring your own data structure") provider replaces all six copies
with **one shared store** (`ind_common`); every logical index becomes a lightweight *view*:

- **forward** `(F,V) → P → {(M,Fp)}` — serves `none`, `0_1`, `0_1_2`, existence, iteration.
  The `(F,V)` and `P` prefixes are stored **once** and shared across all leaves (this is
  where the savings come from).
- **inverse** `(F,M,Fp) → [(V,P)]` — serves the `0_3_4` view. Push-only (each full tuple is
  unique on insert).

The physical `locals` is a `CountingVec` that stores **no tuples** but tracks the row count,
so `prog.locals.len()` (the only post-run consumer of the physical relation, `mod.rs:1167`)
still reports the true size with **zero change** to `mod.rs` call sites.

### Key correctness subtlety (the one that matters)

Ascent's generated per-iteration code calls `merge_delta_to_total_new_to_delta` on **both**
the `ind_common` **and** each index's write target. The reference BYODS impl (`eqrel`)
tolerates this double-merge of the shared store only because union-find merge is
**idempotent**. `locals` is a *plain* relation — a double merge would corrupt semi-naive
evaluation. Fix:

- the **only** real merge lives on the `ind_common` (`LocalsIndCommon::absorb`);
- **all** index write targets (`FullWrite`, `NoopWrite`) have **no-op** `RelIndexMerge`;
- data enters the store solely via the full index's `insert_if_not_present`; partial-index
  `index_insert` is a no-op.

---

## 3. Baselines and improvements

Peak `phys_footprint`, `ctadl index`, aliasing enabled. `baseline` = default Ascent storage;
`+trie` = this prototype. `locals` row counts are **byte-identical** between the two.

| binary            | baseline | + trie   | reduction     | `locals` rows (identical) |
|-------------------|---------:|---------:|---------------|--------------------------:|
| smbd              | 27.49 GB | **9.13 GB**  | **3.01×** (−67%) | 130,139,075 |
| wpa_supplicant    |  9.03 GB | **3.52 GB**  | **2.57×** (−61%) |  35,779,394 |
| pluto             | 25.68 GB | **10.57 GB** | **2.43×** (−59%) | 194,565,083 |
| samba_multicall   | 10.01 GB | **4.31 GB**  | **2.32×** (−57%) |  41,608,096 |
| amuled            |  8.61 GB | **4.35 GB**  | **1.98×** (−50%) |  55,368,800 |

- Wall time is neutral (±6%; pluto 232 s → 246 s).
- The win tracks the **prefix-sharing ratio**: smbd (highest sharing) gains most at 3.0×,
  amuled (lowest) least at 2.0× — consistent with the model.
- The two former worst cases (smbd, pluto) now sit under 11 GB.

## 4. Correctness verification

- **Nix regression suite (the authoritative check):** `nix build .#checks.aarch64-darwin.regression`
  → **33 passed, 0 failed** (ArrayFlow, FieldSensitivity, ObjectSensitivity, funcptr, all
  JVM/DEX reader cases). The README notes the suite is only reliable through Nix (pins
  compilers/Ghidra), so this is the signal that counts.
- **Unit tests:** 103/103 ctadl-ascent lib tests pass (cover the alias and func-ptr flows).
- **Row counts:** identical to baseline on all 5 benchmarks (see table).

### ⚠️ Gotcha that already bit us once — Nix only sees git-tracked files

The Nix flake build copies **only git-known files** into the sandbox. When `locals_trie.rs`
was untracked (`??`), `nix build` failed with `error[E0583]: file not found for module
locals_trie` and 52 cascading errors — even though local `cargo build` succeeded (it reads
straight off disk). **Fix:** `git add ctadl-ascent/src/index_engine/locals_trie.rs`. Any new
file must be `git add`ed before it exists for Nix. (This is exactly why the README says the
regression suite is only reliable through Nix.)

## 5. Files touched

| file | change | tracked? |
|------|--------|----------|
| `ctadl-ascent/src/index_engine/locals_trie.rs` | **new**, ~700 lines — the provider | staged (`A`) |
| `ctadl-ascent/src/index_engine/mod.rs` | `pub mod locals_trie;` (~L55); `#[ds(crate::index_engine::locals_trie)]` on the `locals` relation (~L823) | modified (`M`) |

`mod.rs:1167` (`final_locals: prog.locals.len()`) is **unchanged** — `CountingVec` keeps it correct.
`results.sarif` in the repo root is untracked scratch output; leave it alone.

### Anatomy of `locals_trie.rs` (for orientation)

- `CountingVec<T>` — physical `rel!` type; stores nothing, counts `push`es.
- `DynIter<'a,T>` — clone-able boxed iterator (local copy of byods' private `IteratorFromDyn`).
- `LocalsIndCommon<F,V,P,M,Fp>` — the shared store (`fwd`, `inv`, `len`); `insert`, `contains`,
  `absorb`; impls `Default`/`Clone`/`RelIndexMerge` (the one real merge).
- `NoopWrite`, `FullWrite` — index write targets (no-op merge on both; `FullWrite` does the real inserts).
- `ViewNone/View01/View012/View034/ViewFull` — read views; `RelIndexRead`/`RelIndexReadAll`
  (+ `RelFullIndexRead` on `ViewFull`).
- `marker!` macro → `ToNone/To01/To012/To034/ToFull` (`ToRelIndex` markers).
- 5 provider macros at the bottom: `rel!`→`CountingVec`, `rel_ind_common!`→`LocalsIndCommon`,
  `rel_full_ind!`→`ToFull`, `rel_ind!` dispatched by subset (`[]`,`[0,1]`,`[0,1,2]`,`[0,3,4]`),
  `rel_codegen!`→nothing.

---

## 6. Path forward: from prototype to compact nodes

**Why 2.5× and not the modeled ~4×:** the prototype uses nested `hashbrown` maps with
`HashSet` leaves. That means (a) real per-entry hash-table overhead (control bytes + load-factor
slack, often ~1.5–2×), (b) a min allocation per inner map / per leaf set even when it holds one
element, and (c) the inverse `inv` map is stored in **full** (every `(V,P)` duplicated from the
forward side). The forward-map savings are real but hashbrown's constant factors eat part of it.

The interface (the 5 views + merge contract) does **not** change; only the internal storage of
`LocalsIndCommon` changes. Work items, roughly in priority order:

1. **Sorted-vector leaves instead of `HashSet<(M,Fp)>`.**
   Replace each leaf `Set<(M,Fp)>` with a `SmallVec<[(M,Fp); N]>` kept **sorted**. Most leaves
   are tiny (1–few entries), so inline storage + binary search removes the per-set allocation
   and the hash-table slack. `insert` = binary-search-then-insert returning "was new"; iteration
   is already ordered. This is the single biggest expected win.

2. **Pack `(M,Fp)`.** `FormalIndex`=i16 + `Path`=8-byte handle. Store as a packed 16-byte (or
   `(i16, u64)` `#[repr(C)]`) element rather than a tuple in a hash set, so leaves are dense
   arrays with no padding waste and cache-friendly scans.

3. **Sorted inner P-map instead of `Map<P, …>`.**
   `P` is an 8-byte handle. Replace the inner `Map<P, leaf>` with a sorted
   `Vec<(P, leaf)>` (or a small B-tree-ish structure). Same rationale: most `(F,V)` groups have
   few distinct `P`s, and hashbrown's per-inner-map overhead is paid once per `(F,V)` group,
   which there are many of.

4. **Shrink/eliminate the inverse `inv`.** Options, cheapest first:
   - keep `inv` but store `(V,P)` packed and `shrink_to_fit` after the fixpoint; or
   - make `0_3_4` a **derived** view that scans `fwd` lazily (drop `inv` entirely) — trades the
     `0_3_4` `index_get` from O(1) to O(scan); only worth it if `0_3_4` lookups are rare. Check
     how hot `0_3_4` is before committing (grep the datalog rules that key on `(F,M,Fp)`).
   - or build `inv` **once** at end-of-fixpoint if it's only read after saturation.

5. **Only-if-needed: interned `(F,V)` keys.** If the top-level `Map<(F,V), …>` itself is heavy,
   intern `(F,V)` to a u32 id and key an outer `Vec` by it. Likely unnecessary; measure first.

### How to validate each step

Do them **one at a time**, and after each:

1. `cargo test -p ctadl-ascent` (fast correctness signal, 103 tests).
2. `nix build .#checks.aarch64-darwin.regression` → must stay **33 passed** (remember to
   `git add` any new file first).
3. Re-measure peak `phys_footprint` on **smbd** (highest sharing, biggest absolute baseline)
   and **amuled** (lowest sharing, worst case) — those two bracket the range. Confirm `locals`
   row count still matches the table above (byte-identical results gate).

Target: ~4× reduction / smbd into the ~6–7 GB range, amuled near ~2.2 GB, all row counts
unchanged.

---

## 7. Benchmark corpus + reproducible import/index (DON'T re-derive this)

The benchmarks are raw firmware ELF binaries from the **Karonte** dataset at
`../karonte/firmware` (relative to the repo root). They are **pcode** targets (32-bit ARM/MIPS
ELFs), NOT Java. The canonical variants below were pinned by matching the `locals` row count
against §3 (there are many same-named variants — `smbd` alone has ~36 — so the count is the
fingerprint). **Active corpus = these 3** (imported under these exact names):

| benchmark      | import name | size | arch | rows (current) | §3 rows | Δ | path (under `../karonte/firmware/`) |
|----------------|-------------|-----:|------|---------------:|--------:|--:|-------------------------------------|
| pluto          | `pluto`          | 1.4M | ARM | 194,565,083 | 194,565,083 | **0** (exact) | `d-link/analyzed/DIR-885L_fw_revA_1-13_eu_multi_20170119/_DIR885LA1_FW113b03.bin.extracted/squashfs-root/usr/libexec/ipsec/pluto` |
| smbd           | `smbd`           | 2.7M | ARM | 130,139,048 | 130,139,075 | 27 | `NETGEAR/analyzed/R7800/firmware/squashfs-root/usr/sbin/smbd` |
| wpa_supplicant | `wpa_supplicant` | 1.1M | ARM | 35,776,521  | 35,779,394  | 2,876 | `NETGEAR/analyzed/_XR500-V2.1.0.4.img.extracted/squashfs-root/usr/sbin/wpa_supplicant` |

The tiny Δ (≤2,876 rows out of tens of millions) is the recent perf commits (`prog_store filter`,
`Precompute alias of formal`); pluto reproduces byte-for-byte. **The corpus is deliberately
NOT single-firmware** — earlier "use DIR-885L for everything" and "use Tenda smbd" guesses were
wrong (Tenda smbd = 95.4M, DIR-885L smbd = 41.8M — neither is 130M). Only the exact paths above
reproduce §3.

**Aspirational (not yet handled):**

| benchmark        | size | arch | why aspirational | path |
|------------------|-----:|------|------------------|------|
| samba_multicall  |  15M | ARM  | 15M binary — import/index doesn't scale to this yet; deliberately out of the active corpus. Revisit once memory work lands. | `NETGEAR/analyzed/_XR500-V2.1.0.4.img.extracted/squashfs-root/usr/sbin/samba_multicall` |
| amuled           | 1.3–4.0M | ARM | **Reachability explosion on the current code — see below.** No known-good build; excluded from the active corpus until the blow-up is understood (may be a real regression, not a corpus problem). | see pathological table |

**Pathological cases (recorded for later investigation — do NOT use as the benchmark):**

`amuled` explodes on **every** build tried on this branch (doc §3 expected a tame ~55.4M rows):

| amuled build | size | rows / peak | path |
|--------------|-----:|-------------|------|
| NETGEAR R7800 | 1.3M | **1,502,664,397 rows / ~68.8 GB** (802% of vars reached, 87,819 per formal) | `NETGEAR/analyzed/R7800/firmware/squashfs-root/usr/bin/amuled` |
| NETGEAR R7500 | 1.3M | killed at 21 GB, still climbing (same 1.3M build class) | `NETGEAR/analyzed/R7500/_R7500v2-V1.0.3.16.img.extracted/squashfs-root/usr/bin/amuled` |
| NETGEAR R7000 | 4.0M | killed at 28 GB, still climbing (a *different*, larger build — also explodes) | `NETGEAR/analyzed/R7000/fw/_R7000P-V1.3.0.8_1.0.93.chk.extracted/squashfs-root/usr/sbin/amuled` |

> ⚠️ **Suspected aliasing regression.** The doc's §3 amuled was 55.4M rows / 8.61 GB — tame. That
> *all* amuled builds (both the 1.3M and the distinct 4.0M) now blow up to 1.5B rows / tens of GB,
> while smbd/pluto/wpa still match §3, points at a change on this branch that amuled specifically
> triggers — the recent aliasing/order commits (`prog_store filter`, `Precompute alias of formal`,
> `Try to reorder`) are the prime suspects. Investigate with `git bisect` on amuled's row count
> BEFORE trusting the trie-memory numbers on it. This is independent of the trie work.

**The import/index scheme (this is the whole thing — no Ghidra wrangling needed):**
`ctadl import -l pcode <ELF>` invokes Ghidra `analyzeHeadless` *itself* (see
`ctadl-ascent/src/languages/pcode/ghidra.rs`; it writes `pcode-reader/ExportPcode.java` to a temp
dir and post-scripts it). The CLI help calls `-l pcode` a "Ghidra pcode facts directory" — that is
**misleading**; passing a raw ELF works and is the intended path. Ghidra 12.x comes from Nix.

```bash
# one benchmark, end to end (aliasing is ON by default: IndexConfig{alias_rule:true})
B=smbd
BIN=../karonte/firmware/NETGEAR/analyzed/R7800/firmware/squashfs-root/usr/sbin/smbd
./target/release/ctadl import -l pcode "$BIN" -n "$B"   # Ghidra extract -> import (slow, once)
RUST_LOG=info ./target/release/ctadl index "$B"         # reindex is cheap; re-run for each phase
#   the index run prints:  "locals store estimate: total … | fwd …% | inv …%"   (Phase-0 heap_report)
#   and:                   "relation increase: locals: <ROWS>, …"                (byte-identical gate)
```

⚠️ **Pass the import name exactly ONCE.** `ctadl index <name>` alone uses the project name as the
sole program. `ctadl index <name> <name>` co-indexes `<name>` twice — historically this
double-added its codegen facts. Fixed in `AnalysisProject::try_create` (`project.rs`), which now
order-preserving-dedups the import list, but don't rely on it: one arg is the intended form.

Imports are cached under `~/.local/state/ctadl/imports/<name>/`, so **you only import once**; every
subsequent phase just re-runs `ctadl index` against the cached import. NOTE the import is not
complete until `ir-program.bitcode` exists in that dir — Ghidra prints "Import succeeded" *before*
the facts→bitcode conversion finishes, so indexing too early fails with a missing-file i/o error.

## 8. Phase-0 heap instrumentation (DONE) — where the bytes actually go

`LocalsIndCommon::heap_report()` (in `locals_trie.rs`) estimates fwd-trie vs inverse-map bytes
(allocation-size approximations incl. hashbrown load-factor slack; for *relative* fwd-vs-inv
comparison, not exact accounting) and is logged after the fixpoint (`mod.rs`, right after "index
scc times"). It exists so we optimize by measurement, not guess.

**The fwd/inv split is strongly regime-dependent — measure on the firmware corpus, not Java.**

| index (regime)          | rows   | total     | **fwd** | **inv** | rows/(F,V) group |
|-------------------------|-------:|----------:|--------:|--------:|-----------------:|
| fx (Java, low-share)    | 4.64M  | 1,126 MB  | 89%     | 11%     | 3.3 |
| downloader (Java)       | 3.11M  | 1,015 MB  | 91%     |  9%     | 2.4 |
| **wpa_supplicant** (fw) | 35.8M  | 2,466 MB  | 69%     | **31%** | 50 |
| **smbd** (fw)           | 130.1M | 7,370 MB  | 59%     | **41%** | 143 |
| **pluto** (fw)          | 194.6M | 10,207 MB | 57%     | **43%** | 324 |

**Conclusion — on the firmware corpus that actually matters, `inv` is 31–43% of the store, NOT the
~10% the Java samples show.** Firmware has heavy prefix-sharing (up to ~324 `locals` rows per
`(F,V)` group vs ~3 on Java), so the forward trie's per-node hashbrown overhead amortizes away,
while `inv` stays at a flat, unshared full copy of every `(V,P)` — its share *rises* with sharing.
This **revises §6**, which deprioritized `inv` as a ~10% footnote. Revised priority:

1. **§6.1/§6.2 sorted packed `SmallVec` leaves** — still the safe first win; kills per-leaf
   hashbrown minimum-allocation slack on the many `Set<(M,Fp)>`.
2. **§6.3 sorted-vec inner P-map** — kills the per-`(F,V)` inner-`Map<P,…>` minimum-allocation slack.
3. **`inv` is now a FIRST-CLASS target (promoted from last):** at 31–43% it is the second-largest
   component on firmware. Its floor is the inherent `(V,P)` copy (`rows × ~16 B` ≈ pluto 3.1 GB /
   smbd 2.0 GB), so packing `(V,P)` + `shrink_to_fit` recovers the slack, and the `0_3_4` view it
   serves is a *single cold probe site* (rule 2.2, `mod.rs:1042`, driven by the small `resolvent`
   relation) — making "derive `0_3_4` by scan, drop `inv` entirely" genuinely attractive here.
   Re-measure this decision against the firmware numbers, not the Java ones.

> Byte-rate sanity: firmware runs ~59–72 B/row; the `(V,P)` inverse copy alone is ~16 B/row, i.e.
> ~a quarter of the whole store is the unshared inverse. That is the lever §6 was missing.

## 9. Resume checklist / commands

```bash
# where things stand
git status --short          # expect: ?? results.sarif  (prototype now committed on this branch)

# fast correctness
cargo test -p ctadl-ascent

# authoritative correctness (pins toolchain; ONLY reliable check per README)
nix build .#checks.aarch64-darwin.regression -L   # expect "33 passed, 0 skipped, 0 failed"

# memory measurement: build release, run `ctadl index`, poll phys_footprint peak
#   (see the measure-process-memory skill: use `footprint -p PID -f bytes`, peak, poll ~4s)
#   the per-structure heap_report line (see §8) tells you fwd-vs-inv bytes directly.
```

**Not yet done (needs a decision):** commit the two changes; then start compact nodes at
§6.1. The prototype is not committed — only staged/modified in the working tree.
