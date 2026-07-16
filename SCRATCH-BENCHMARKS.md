# CTADL `index` memory/time benchmarking baseline

*Reusable baseline for optimizing `ctadl index` (the Ascent Datalog phase) peak memory.
Re-run the suite after each change, diff against the "Current baseline" table below, and
update it. Platform: macOS (Darwin), `phys_footprint` accounting, 128 GB machine, one job at
a time. Companion analysis: `memory-investigation.md`. Corpus provenance:
`../ctadl-plans/LOCALS_TRIE_PROTOTYPE.md`.*

**Current baseline commit:** `eb22186` ("Spill to a hash set if the number of values is too
large") — the size-adaptive `locals` group fix on top of the CSR flatten.
**Measured:** 2026-07-08.

---

## 0. How to use this document

1. Make your change, `cargo build --release`.
2. Re-run the suite (§4). Synthetic is seconds; firmware is minutes.
3. Compare against **§2 Current baseline**. The gates are:
   - **`locals` row count must stay byte-identical** (per benchmark, §3) unless you *intend* a
     semantic change — a row-count drift means you changed analysis results, not just storage.
   - **peak `phys_footprint`** is the primary optimization target.
   - **wall time** must not regress (the CSR flatten taught us a storage win can hide an
     O(N²) time blowup — see `memory-investigation.md` §7).
4. Update the §2 table (and the commit/date header) with your new numbers.

---

## 1. What / how to measure (don't re-derive this)

**Use physical footprint, not RSS.** macOS compresses cold pages, so `ps rss` badly
undercounts. Activity Monitor's "Memory" column == `phys_footprint`. (measure-process-memory
skill.)

- **Peak** comes from `/usr/bin/time -l` → the `peak memory footprint` line (raw bytes). This
  *is* the `phys_footprint` high-water mark. Firmware runs are minutes long so this is exact;
  the sub-second synthetic runs are also caught by `time -l` (it reads the kernel's rusage
  high-water, not a poller).
- **Row count** comes from the `RUST_LOG=info` line
  `relation increase: locals: <ROWS>, <F> formals, <R> reached per formal, <P>% of variables reached`.
  `reached per formal` (= rows / formals) is the aliasing-density predictor: it tracks peak RAM
  across a 200× range independent of binary size (`LOCALS_TRIE_PROTOTYPE.md` §10).
- For a **live trajectory / guarded run** (needed only for the pathological targets that can
  blow past RAM), poll `footprint -p <PID> -f bytes` every ~4 s and kill above a cap — see the
  measure-process-memory skill template. `> 8 GB` on the active corpus means something
  regressed; guard there.

---

## 2. Current baseline (commit `eb22186`, 2026-07-08)

### Synthetic (`gen_N`, medians of 3 warm runs)

| benchmark | binary | bitcode | `locals` rows | reached/formal | **peak** | wall |
|---|--:|--:|--:|--:|--:|--:|
| `gen_800`  | 0.281 MB | 3.5 MB | 191,936 | 59.5 | **59.5 MB** | 0.40 s |
| `gen_1600` | 0.283 MB | 6.8 MB | 169,651 | 26.4 | **120.9 MB** | 0.57 s |
| `gen_3200` | 0.537 MB | 14 MB | 339,251 | 26.5 | **246 MB** | 1.16 s |

Peak scales ~linearly with program size (the CTADL analysis is linear on straight-line C —
the super-linear context path is dormant without resolvable indirect/virtual dispatch). These
sparse-group cases never cross the 64-leaf hash-set promotion threshold, so they exercise the
compact sorted-`Vec` representation.

### Firmware (active Karonte corpus)

| benchmark | `locals` rows | reached/formal | **peak** | wall | vs. baseline `73109ea` |
|---|--:|--:|--:|--:|---|
| smbd | 39,074,966 | 1,285 | **2.74 GB** | 48.3 s | mem −63% (7.43 GB), time −67% (148 s) |
| wpa_supplicant | 31,917,658 | 2,346 | **2.11 GB** | 32.3 s | mem −26% (2.85 GB), time −33% (48 s) |
| pluto | 183,288,400 | 23,064 | **7.96 GB** *(7.89/7.96/8.06)* | ~188 s *(190/185)* | mem −1% (8.06 GB), time −30% (268 s) |
| lk_latest | 12,910,766 | 2,813 | **0.92 GB** | 20.5 s | — (added 2026-07-10 @ `243c0a1`; index is tame) |

- **lk_latest** (added 2026-07-10) indexes cheaply — it is here for the **query** phase, not the
  index phase: it is the canonical minimal reproducer of the query-phase `alias_of_field` blowup
  (`ct-find-memory-blowup/query-alias-blowup-isolation.md`). Its index converges at <1 GB, but a
  broad-model `query` on it explodes (source-independently, pre-fix). Its LK-bootloader SSA has a
  single function with a **19,164-variable empty-path copy group** — the largest copy group in the
  corpus and the direct driver of the query `alias_of_field` Θ(C²) closure.
- **pluto** is 3 runs; peak is tight (7.89–8.06 GB). Wall was 190 / **1199** / 185 s — run 2's
  1199 s is a system-contention outlier (identical peak and row count, no memory anomaly), so
  the representative wall is ~185–190 s. This is the dense target whose huge `(F,V)` groups
  promote to the hash-set representation; the O(N²) time regression from the CSR-only build
  (~867 s) is gone and pluto is now faster *and* ≈-memory vs. the baseline.
- smbd/wpa/pluto row counts are **byte-identical** to the pre-fix HEAD — the hash-set spill is
  a pure storage change.
- The trade for the fix is memory on pluto only: peak sits at ~8 GB (hash-set load-factor slack
  on its handful of huge dense groups, ≈ the baseline 8.06 GB) vs. the CSR-only ~5.2 GB median.
  smbd/wpa barely move and stay far under baseline.

---

## 2.1 Current-code re-evaluation (HEAD `46dac56` + import O(N²) fix, 2026-07-11)

Re-ran the whole suite on current `main` (6 commits past the `eb22186` baseline) plus five new
firmware binaries. **Denominator for the memory target is `ir-program.bitcode` — the serialized
instruction set (`peak/ir`).** The stated goal is **≤ 100× the instructions** (two orders of
magnitude). *(In-memory IR ≈ 3.8× the bitcode, so a `peak/ir` of 380× ≈ 100× the in-memory IR —
pick the denominator deliberately when reading these.)*

**Two things changed the numbers since `eb22186`:**
- **`243c0a1` "Match prefix of offset(0) consumes it"** — a soundness/injectivity fix to
  `match_prefix` that legitimately **creates more flows**. It is *not* storage-neutral: it roughly
  **doubles the dense firmware targets** (`locals` rows and peak). This is intended precision, not a
  regression. `lk_latest` is byte-identical (its aliasing doesn't hit the offset-junction case).
- **Import O(N²) fix** (`pcode/mod.rs` `create_name_to_func_mapping`, Θ(B·I)→Θ(B+I)) — unblocked the
  large binaries below (libavcodec, wl_ko, umac_ko, samba_multicall were previously un-importable or
  hung the importer for minutes; `samba_multicall` is the one §3 used to flag "doesn't scale yet").

### Active corpus at HEAD

| benchmark | bitcode | **peak** | **peak/ir** | `locals` rows | reached/formal | vs. `eb22186` |
|---|--:|--:|--:|--:|--:|---|
| gen_800  | 3.6 MB | 61 MB | **17×** | 156,787 | 48.6 | rows drift (gen re-gen; ignore) |
| gen_1600 | 7.1 MB | 116 MB | **16×** | 176,049 | 27.4 | — |
| gen_3200 | 14.3 MB | 222 MB | **16×** | 352,049 | 27.5 | — |
| lk_latest | 15.1 MB | 0.95 GB | **63×** | 12,910,766 | 2,813 | **byte-identical** |
| smbd | 81.6 MB | 6.32 GB | **77×** | 76,000,974 | 2,500 | rows +94%, mem +131% |
| wpa_supplicant | 28.4 MB | 3.26 GB | **115× ✗** | 52,504,035 | 3,859 | rows +65%, mem +54% |
| pluto | 40.7 MB | 15.73 GB | **387× ✗** | 349,862,964 | 44,024 | rows +91%, mem +98% |

### New firmware binaries (2026-07-11, converged unless noted)

| benchmark | class | bitcode | **peak** | **peak/ir** | `locals` rows | reached/formal | converges? |
|---|---|--:|--:|--:|--:|--:|---|
| libndr_standard | Samba NDR | 137.3 MB | 3.52 GB | **26×** | 14,130,211 | 517 | ✅ tame |
| samba_multicall | Samba multicall | 279.1 MB | 35.66 GB | **128× ✗** | 728,091,638 | 5,685 | ✅ (7 min) |
| wl_ko | Broadcom wifi driver | 88.3 MB | 28.56 GB | **324× ✗** | 642,233,835 | 32,176 | ✅ (5.6 min) |
| umac_ko | Qualcomm wifi MAC | 83.5 MB | 29.52 GB | **354× ✗** | 616,570,805 | 30,664 | ✅ (5 min) |
| libavcodec | media codec | 249.0 MB | ≥ 29.5 GB | **≥ 127× ✗** | — (no fixpoint) | — | ❌ non-converging |

**Findings:**
- **The 100× target is broken by *density*, not size.** Memory ≈ `locals_rows × ~44 B`, and
  `locals_rows` is an *analysis* quantity (the field-propagation closure). Every breach is a
  high-`reached/formal` target; every pass is low. The predictor separates pass/fail at
  **`reached/formal ≈ 3,500**: gen (27–49), libndr_standard (517), smbd (2,500), lk (2,813) pass;
  wpa (3,859), samba_multicall (5,685), umac_ko (30,664), wl_ko (32,176), pluto (44,024) breach.
  A pure storage optimization lowers the constant but cannot pull a 3–4×-over target under 100×.
- **New "wireless-driver / codec" blow-up class.** `wl_ko`/`umac_ko` sit at reached/formal ≈ 31k —
  near amuled's pathological 48k — driven by indirect dispatch through large function-pointer
  tables. They are the **super-linear indirect-call targets** open-lever #6 wanted but the old
  corpus lacked. `samba_multicall` is the new scale ceiling: 728 M `locals`, **128,072 formals**
  (2–3× any prior target).
- **`libavcodec` is a new non-terminator** — but an *unbounded-productive* one (memory keeps
  climbing 19→29.5 GB), distinct from minidlna's flat plateau. Bad rule = forward-prop `locals`
  (§3.1).

---

## 2.2 Small blowup reproducers (2026-07-11, HEAD `29044c6`)

A sweep of 12 small (100 KB–1 MB) ARM binaries from the Karonte corpus, chosen from the same
firmware images/classes as the known breachers, looking for **small programs that break the
100× `peak/ir` target fast** (index < 15 s). Three breach; all three are from the **NETGEAR
R9000 Atheros wifi stack** (`NETGEAR/analyzed/R9000/firmware/squashfs-root/lib/modules/3.10.20/`),
the same image as breacher `umac_ko`:

| import name | binary | bitcode | **peak** | **peak/ir** | wall | `locals` rows | reached/formal | %vars |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| **cfg80211_ko** | 474 KB | 9.3 MB | 1.08 GB | **119× ✗** | **11 s** | 15,999,406 | 5,642 | 51% |
| **ath_hal_ko** | 840 KB | 14.8 MB | 1.74 GB | **120× ✗** | 17 s | 26,543,365 | 6,539 | 50% |
| **ath_dev_ko** | 653 KB | 15.2 MB | **16.67 GB** | **1,098× ✗✗** | 185 s | 329,701,247 | **98,242** | **747%** |
| ath_dfs_ko | 103 KB | 2.4 MB | 0.16 GB | 68× | 2 s | 1,515,150 | 3,491 | 16% |

- **`ath_dev_ko` is the strongest small reproducer in the corpus**: a 653 KB kernel module
  whose density (98k reached/formal) matches crtmpserver (99k, worst known) — but it converges
  in ~3 min instead of 43, at 1/4 the memory. **`cfg80211_ko` is the fast-iteration breacher**
  (11 s, 119×).
- **The blowup is resolvent-free.** All three breachers have `resolvent` ≤ 2 and essentially
  zero `context_*` output — the hybrid-inlining machinery is inert. The rows come entirely from
  the forward field-propagation closure + aliasing (same shape as libavcodec §3.1). Locals/assign_like
  amplification: cfg80211 16M/631k = 25×, ath_dev 330M/777k = **424×** — each copy edge spawns
  hundreds of `(formal, path)` facts.
- **`ath_dfs_ko` is the ground-truthing specimen**: 103 KB, 434 formals, 2 s index, yet already
  at the ~3,500 reached/formal density threshold. Small enough to manually audit whether the
  computed flows actually exist in the Pcode.
- The other 8 swept candidates are tame (27–47×): `wil6210_ko` (64/formal), `netusb_tenda` (Tenda
  AC18 NetUSB.ko), `dhd_ko` (Broadcom, TP-Link Archer C3200 — 92/formal despite being wl's
  little sibling), `wpa_txvg1530` (TP-Link TX-VG1530 wpa_supplicant, 213 KB ARM), `hostapd_dir890`
  (DIR-890L), `ipsec_ko_dir880` (DIR-880L, 2,006/formal), `qca_ssdk_ko`, `ath_rate_ko` (R9000).
  Consistent with §2.1: the ~3,500 reached/formal threshold separates pass/fail perfectly across
  all 12.

---

## 2.3 Ground-truthing `ath_dev_ko`: the blowup is redundant representation, not extra flows (2026-07-11)

Motivating question: *"are we introducing extra flows beyond what the Pcode has included?"*
Answer from a full audit of `ath_dev_ko`'s `locals` closure: **no spurious flows — but the row
count is inflated ~20–50× by redundant representation of real flows, dominated by SSA versioning
of the stack-frame pointer.**

**Method.** An independent Python reimplementation of the two context-free forward
field-propagation rules (`index_engine/mod.rs:1010–1019`), run over the *persisted final*
`assign_like` / `paths` / `formal_param` (so it starts from exactly the edges the engine derived).
It reproduces the engine's total **to the row**: `ath_dfs_ko` = **1,515,150**, identical.
Provenance is tracked so any reached `(var, path ← formal, fpath)` fact can be replayed as a
derivation chain back to the seed, with each step labelled by the edge that fired it
(program / param-binding / summary). Scripts in `scratchpad/audit/` (`audit_locals.py`,
`chain.py`, `sweep.py`).

**Every flow is genuine.** Sampled derivation chains in `ath_check_seq_order` (id 367, 153 edges,
151 rows) map step-for-step onto the pretty-printed IR (`ctadl inspect …/ir-program.bitcode`);
variables that are only constant-derived (the return flag `uVar1`, `register:00000020`) are
correctly **not** reached. All three synthetic-flow mechanisms are inert:
- **Offset dual-encoding** — the same field appears both signed (`.[-216]`) and as its unsigned
  32-bit wrap (`.[4294967080]`); 83 of 100 big offsets have a signed twin. This does **not**
  inflate: canonicalizing offsets mod 2³² *raises* `ath_dfs_ko` from 1.52M to 1.99M (+32%), so
  the split is mildly *suppressing* flow (offset arithmetic in `match_prefix` fails across
  encodings), not inventing it.
- **Aliasing rule** — `--alias-rule=false` changes `ath_dfs_ko` by −0.7% (1,515,150→1,504,533)
  and `cfg80211_ko` by −0.06%. Negligible here.
- **Hybrid-inlining context** — `resolvent` ≤ 2, `context_locals` = 0 (per §2.2).

**The amplifier is the stack-frame pointer.** In the hot functions **88–92% of all `locals` rows
have a `%__stack_top` subject.** The frame pointer gets **3,000–5,600 SSA versions** per hot
function (`ath_tx_edma_process` 5,624; `ath_txq_schedule` 5,362), because every frame store is
lowered to `%__stack_top = update(.[slot].deref := v)` — a *new full-aggregate SSA version =
whole-frame copy + one store*. Flow-insensitively, every "frame slot X holds formal *f* via path
*p*" fact then replicates across all later versions. Collapsing the frame-pointer versions to one
representative shows the redundancy directly:

| function | rows | %stack_top | frame rows → 1 rep | overall (collapse all SSA versions) |
|---|--:|--:|--:|--:|
| `ath_tx_start_dma` | 95,875 | 92% | 88,348 → 45 (**1,963×**) | 54× |
| `ath_beacon_config` | 56,892 | 92% | 52,195 → 39 (**1,338×**) | 44× |
| `ath_tx_txqaddbuf` | 26,878 | 88% | 23,703 → 52 (**456×**) | 22× |

(`ath_tx_complete_aggr_rifs`, id 771, alone exceeds 30 M rows.) 8.5% of *all* `assign_like` edges
(66,250) are `%__stack_top ← %__stack_top` empty-path copies — the version chain that carries
the frame field-set forward.

**Takeaway.** The compositional flows are sound: no reachability fact is fabricated beyond the
Pcode. The blowup is a *representation* problem — the same real frame-slot flows are materialized
thousands of times, once per SSA version of the frame pointer. The high-leverage fix is to
canonicalize / single-representative the frame pointer (or model the frame flow-insensitively as
one object) rather than to look for a soundness bug, which this audit rules out.

### 2.3.1 Independent def-use oracle — closing the "ported-rule-bug" gap (2026-07-12)

§2.3's Python model is a faithful *port* of the same R1/R2 rules the engine runs (validated to
the row). That proves **fidelity**, not **soundness**: if the rules themselves fabricated a flow,
the port would reproduce it and the row-match would look identical. To rule out a *ported*
soundness bug we need an oracle built on a different axis — model output vs. the **raw Pcode IR** —
that never touches R1/R2 or the path-composition operators (`match_prefix`/`prepend_onto`/`paths`).

**Oracle** (`scratchpad/audit/oracle.py`). Parses the pretty-printed IR text directly into a
**field-insensitive** variable dataflow graph (`assign`, `update` BASE/VALUE, call arg→result and
— permissively — arg↔arg pointer write-back, `return`→ret-slot), then does plain forward graph
reachability from a formal's entry var (`@pi`). Because field-insensitivity strictly drops
constraints and calls are modeled permissively, this set is a **sound upper bound** on any correct
field-sensitive analysis, projected to **pre-SSA variables** (engine names minus the engine's own
`_N` SSA suffix — the IR `inspect` text is pre-SSA; the `%__stack_top_4940`-style versions are
added by the indexer *after* `inspect`). Three independent checks, on `ath_txq_schedule` (554),
`ath_tx_start_dma` (232), `ath_tx_txqaddbuf` (660):

- **Closure / subset** — every pre-SSA variable the port reaches from a formal lies in the raw-IR
  reachability envelope. **Holds for all formals of all three functions** (only the pseudo-roots
  `$globals` and `%_$ret0` are excepted — interprocedural plumbing, not IR statement vars). The
  port reaches ~3–20% of the envelope (e.g. 142 of 3,810 vars), so it is a strict *under*-approx of
  the permissive bound and never steps outside it.
- **Negative** — of the vars with **no** raw-IR def-use path from the formal (e.g. 372 in func 554,
  1,166–3,467 across seeds), the port reaches **zero**. It declines to reach what the Pcode doesn't
  connect.
- **Edge relation** — every `assign_like` *program* edge the closure propagates over is a real
  def-use **path** in the raw IR: **102,063/102,063, 28,288/28,288, 9,578/9,578 = 100.000%**. About
  ~6% per function are temp-collapsed multi-hop paths (the engine copy-propagates single-use temps,
  e.g. `SRC → %temp → register:00000065`), still fully path-backed; **zero fabricated edges.**

The strict call model initially flagged a handful of "candidates" in `ath_tx_txqaddbuf` (68
calls); switching to the permissive (sound) model resolved every one — they were exactly the
interprocedural pointer-writeback flows the port captures via summaries, i.e. real call-mediated
flow, not fabrication. Because the oracle is derived from raw IR + plain reachability and shares no
code with R1/R2, a ported rule-bug **cannot** launder through it. **Result: no extra flows beyond
the Pcode at pre-SSA-variable granularity** — confirming §2.3 on an independent axis. (Limitation:
this granularity does not independently re-verify field-*offset* composition, e.g. `.[8]`↦`.[16]`;
that is covered by the §2.3 chain-to-IR spot checks, a different property from "extra flows.")

---

## 2.5 Load/Store representation experiment — does memory-instruction IR fix the blowup? (2026-07-12)

**Motivation.** §2.2–2.4 pinned the blowup to *redundant representation*: the frontend models a
field write as the functional `Update` instruction (`s = update(s.f := v)`), which **re-defines the
whole aggregate `s`**. SSA then mints a fresh version of `s` at every write — this is exactly the
`__stack_top_N` amplifier (5,624 distinct SSA versions of the single frame pointer in `ath_dev_ko`).
Hypothesis: replace the functional `Update` with true memory instructions — `Load {dest, source,
path}` and `Store {dest, path, value}`, where **a store defines no variable** (the base is only read
as a location) — so the aggregate is never re-versioned. Ported from branch `load-store-2` (commit
`2815a45`), adapted to current HEAD, all 106 workspace tests pass.

**Headline: the amplifier is gone.** `__stack_top` SSA versions in `ath_dev_ko` drop **5,624 → 1**.
Re-imported (Ghidra re-run) + re-indexed all four small breachers under a phys-footprint guard:

| import | OLD rows | NEW rows | OLD peak | NEW peak | OLD wall | NEW wall | verdict |
|---|--:|--:|--:|--:|--:|--:|:--|
| **ath_dev_ko** (flagship) | 329,701,247 | **59,105,801** | 16.67 GB | **4.03 GB** | 185 s | 119 s | **−82% rows, −76% mem** ✅ |
| **cfg80211_ko** | 15,999,406 | **2,175,993** | 1.08 GB | **0.34 GB** | 11 s | 5 s | **−86% rows, −66% mem** ✅ |
| **ath_dfs_ko** | 1,515,150 | **339,557** | 0.16 GB | **0.11 GB** | 2 s | 1 s | −78% rows ✅ |
| **ath_hal_ko** | 26,543,365 | **225,263,097** | 1.74 GB | **11.11 GB** | 17 s | 110 s | **+748% rows, +538% mem** ❌ |

**The change is NOT a clean win.** It removes the dominant stack-versioning amplifier (3 of 4
breachers improve dramatically, including the worst, `ath_dev_ko`), but `ath_hal_ko` — a hardware
abstraction layer dense in *deep nested field paths* — regresses 8.5×. Its `paths` set is unusually
deep (5,626 paths; 2,536 at depth-3, 240 at depth-4 vs `ath_dev`'s 3,949), and the forward-field-
propagation closure over deep composed paths is what explodes. The representation trades the
stack-versioning blowup for sensitivity to field-path *depth*: it helps versioning-dominated
functions and hurts path-depth-dominated ones. `assign_like`/program-edge ratio is the tell —
0.93–0.94 on the three that improve, **1.22** on `ath_hal_ko`.

**Small precision loss (soundness caveat).** Port-based summary diff (independent `audit_locals`
fixpoint, OLD facts vs NEW facts) over 510 mid-size `ath_dev_ko` functions: **9 summaries lost, 2
gained**, 4 functions affected. Every lost summary is a **2-level pointer-dereference-through-field-
offset chain** — e.g. `ath_check_swretry_req`: `return <- formal1.[32].deref.[4533].deref`
(i.e. `p1->f32->f4533`). The new lowering routes a multi-hop load chain through single-use temps
(`t1 = load p.[32].deref; t2 = load t1.[4533].deref`), and the composed access path
`.[32].deref.[4533].deref` is **not reconstructed across the temp boundary** the way the old
`Update` + copy-propagation rebuilt it, so the deep path never enters the `paths` gate
(`.deref.deref.deref` present in OLD paths, **absent** in NEW). The interprocedural interface is
*mostly* preserved (thousands of summaries; 9 lost), but this is a genuine reduction in deep-pointer
precision, not a wash.

**Assessment.** The Load/Store representation **validates the diagnosis** — eliminating aggregate
re-versioning removes the primary blowup mechanism and gives large wins where that mechanism
dominates. But it is **not a finished fix**: (1) it regresses on field-path-depth-dominated modules
(`ath_hal_ko`), and (2) it slightly reduces precision on chained field-dereferences. Both point at
the same missing capability — a real store/load *matching* (points-to) discipline rather than
syntactic path composition. This is the intended next step: port Datalog pointer analysis in the
style of Smaragdakis & Balatsouras (`points-to-tutorial15.pdf`) into the index/query engine, which
would both bound the path-depth explosion and recover the multi-hop deref chains. The representation
change is a sound *prerequisite* for that work (memory instructions are what a points-to analysis
consumes), not a standalone solution. Reproduce: re-import each breacher (`ctadl import -l pcode
<ELF> -n <name>`, the IR changed so the cache must be rebuilt), then `scratchpad/ls_bench.sh <name>`.

---

## 2.6 "Require Loads" — a single way to read a field (2026-07-12)

Follow-on IR cleanup requested on top of §2.5: the Load/Store port still left **two** ways to read a
field — a `Load` instruction *and* an `Exp::AccessPath` (a field path living on the RHS of an
assign). This makes the representation ambiguous and lets field reads slip back into the
aggregate-copy form the blowup fix was trying to remove. The change makes **loads the only way to
read a field**:

- `Exp::AccessPath(AccessPath)` → **`Exp::Variable(VariableRef)`**. An expression can now only name a
  bare variable; it cannot carry a field path. (`ctadl-ir/src/mir/mod.rs`.)
- A new shared helper `mir::load_access_path(ap, out, fresh)` turns a chain of field accesses into a
  **sequence of `Load` instructions**, one per pointer dereference (a purely symbolic field chain
  with no `deref` stays a single load). Every frontend routes RHS field reads through it: pcode
  threads a statement buffer through `get_exp`; tree-sitter gains an lvalue/rvalue split
  (`flatten_lvalue` returns the raw path for store targets and subscript bases, `emit_loads` lowers
  reads); jvm/dex getfield/getstatic/aload/arraylength become `Load`s; flowy's `parse_ref` +
  `lower_ref` lower field reads in the DSL.
- Consequential simplifications: the SSA copy-coalescer no longer composes field paths (that role
  moved to Load chains); codegen's `cap_path` only propagates through whole-variable copies; and the
  empty (whole-variable) path is now seeded into the `paths` gate explicitly — it used to ride in for
  free on every pathless `Exp::AccessPath`'s empty field-access list, which `Exp::Variable` no longer
  carries (without this, a scalar reached through a load chain like `x=a; y=x.b; z=y.c; sink(z)` was
  silently dropped — caught by `multi_level_alias.tnt`).

**Status:** all 106 unit tests + the frontend/flowy integration suites pass. One demonstration file,
`substitute_prefix_demo.tnt`, is marked a **known limitation** (skipped with an explanation in
`flowy_tests.rs`): it exercises offset-arithmetic path substitution *through a field-to-field copy*
(`result.c.[10] = p_val.a.[60]` with taint at `p_val.a.[100]…`). That worked only because the copy
was a single fact `result.c.[10] <- p_val.a.[60]`, letting `substitute_prefix` jump directly between
two *syntactic* program paths (both in the terminating `paths` gate). Requiring loads splits it into
`t = load p_val.a.[60]; store result.c.[10] := t`, and the intermediate taint path from the
arithmetic (`.[40]…` = `[100]−[60]`) is not syntactic, so the gate stops it. Widening the gate to
admit arithmetic-derived paths would reintroduce the unbounded growth this whole effort is fighting;
the proper fix is the planned points-to analysis (heap-object reasoning instead of syntactic path
composition). This is the same precision gap already flagged in §2.5, now made explicit by a single
skipped demo rather than a silent loss.

**Blowup effect (measured 2026-07-12, guarded index, phys_footprint).** Re-imported to `_rl`/`_ns`
names (`$CLAUDE_JOB_DIR/tmp/rl_bench.sh`, `nosplit_bench.sh`):

| ath_dev_ko | locals rows | peak mem | reached/formal |
|---|--:|--:|--:|
| §2.5 Load/Store (hybrid) | 59M | **4.03 GB** | — |
| require-Loads, **per-deref split** (default) | 177.3M | **8.14 GB** | 52.8k (163.8% of vars) |
| require-Loads, single full-path load (`CTADL_NO_SPLIT_LOADS`) | 229.0M | **13.61 GB** | 68.2k (211.9%) |

| ath_hal_ko | locals rows | peak mem |
|---|--:|--:|
| §2.5 Load/Store | 225M | 11.11 GB |
| require-Loads split (default) | 218.0M | 9.94 GB |
| require-Loads single-load | 225.3M | 10.15 GB |

**Findings.** (1) require-Loads **regresses the flagship ~2× on memory / ~3× on rows vs the §2.5
Load/Store hybrid** (8.14 vs 4.03 GB), and is roughly par on `ath_hal`. (2) Counter-intuitively,
**per-deref splitting is the *better* form** — one long-path temporary (single-load) fans out more
(211.9% of variables reached) than several short-path temporaries (163.8%); split is also the
points-to-normalized shape, so it is the default. (3) The regression is **not** the empty-path gate
seed: re-indexing with `CTADL_NO_EMPTY_PATH=1` gives byte-identical 177.3M rows / 8.30 GB, so the seed
is free on pcode (the empty path is already reachable there) and only matters for small-program
correctness (`multi_level_alias.tnt`). The cost is the **loads-only decomposition itself** — every
field read becomes a taint-carrying temporary, and short-path temporaries broaden forward
propagation.

**Assessment (updated).** "Require Loads" is a clean, unambiguous representation (one way to read a
field) and the correct *input shape* for points-to, but it is **not** itself a memory win over the
§2.5 hybrid — under the current *syntactic* index engine it costs ~2× on the flagship because the
engine still reasons by path composition, and loads-only trades long specific paths for many
broad short-path temporaries. Closing this needs the planned points-to analysis, which replaces
syntactic path fan-out with heap-object reasoning and bounds exactly this propagation. Net: keep the
representation (prerequisite for points-to), but do **not** expect the standalone blowup number to
improve until points-to lands. Env knobs `CTADL_NO_SPLIT_LOADS` and `CTADL_NO_EMPTY_PATH` exist for
A/B measurement; defaults (split on, seed on) are both correct and the better-performing choice.

### 2.6.1 Can codegen "compress sequences of loads" to recover the regression? No — not on pcode (2026-07-12)

The §2.6 regression is driven by base-fact inflation: `assign_like` base rows go ~695K (§2.5 hybrid)
→ **1.93M** (require-Loads) on `ath_dev_ko`. The natural idea is to *compress* it back — when a field
read becomes `t = load a.f; <use t>`, fuse the temporary so the consumer reads `a.f` directly instead
of hopping through `t`. This was tried at **two** levels, both flow-preserving (the index engine is a
flow-insensitive fixpoint over `assign` edges, so fusing a single-use def into its use removes the hop
without changing reachability). All numbers on the `ath_dev_ko` re-import, guarded index,
phys_footprint; compression deltas are relative to this session's require-Loads baseline run
(1.93M base rows / 177.3M locals / 7.73 GB — ~5% run-to-run below the §2.6 table's 8.14 GB, so read
the deltas, not the absolutes):

| variant | `assign_like` base rows | locals | peak mem |
|---|--:|--:|--:|
| require-Loads (baseline) | 1.93M | 177.3M | 7.73 GB |
| **IR copy-fold** — `t=load y.f; x=t` ⇒ `x=load y.f` (SSA copy-coalescer, single-use load into a pure copy) | 1.89M (**−1.8%**) | 176.9M | 7.65 GB (−1%) |
| **codegen composition** — fold any single-use load into its consumer's `assign` fact via `cap_path`, one level (keeps `deref` chains split) | 1.89M (**+0 vs copy-fold**) | 176.9M | 7.63 GB |

**Both are near-useless on pcode, and instrumenting the codegen fold says exactly why.** Over
`ath_dev_ko`'s **120,339** loads, classified by their result temporary:

- **single-use (`use==1`): 1,274 — ~1%.** The other **99% (119,065) are used many times.**
- **appears in a non-expression position: 120,339 — 100%.** Every load result is read as a load
  source, store base, phi operand, or call receiver — never *only* as a plain RHS operand.

That is pcode's nature: **a load produces a pointer value.** A register gets a struct pointer loaded
into it and is then reused as the base for many downstream loads/stores (`t = *p; u = *(t+8);
store (t+16)=v; …`). Such temporaries are genuinely multi-use and feed *other loads/stores*, not a
single scalar operand — so single-use fusion finds essentially nothing to fuse. The idea is sound for
a frontend whose field reads are single-use scalars (C-style `x = a.f` read once), but pcode's
register-based pointer-chasing does not have that shape. The remaining base-fact bloat is intrinsic:
every dereference of a heavily-reused pointer is materialized as its own instruction, and no syntactic
fact-compression removes that under the current engine.

**Disposition.** Both experiments were **reverted** (the codegen fold adds a per-function pre-scan for
zero pcode benefit; the copy-fold gains ~1%). Confirms §2.6: the require-Loads memory number will not
be recovered by compressing facts — it needs the propagation *replaced* by points-to, which reasons
over these load/store instructions directly and never materializes the 177M `locals` rows. `ctadl`
`inspect` + a `CTADL_FOLD_DEBUG`-style temp-use histogram is the quickest way to re-confirm the
multi-use load property on any new pcode target before attempting fact-level compression.

---

## 3. The corpus (provenance + byte-identical fingerprints)

### Active corpus — always run these

Raw firmware ELFs from the **Karonte** dataset (`../karonte/firmware`), 32-bit ARM, imported
once via Ghidra `-l pcode`. There are many same-named variants (smbd alone has ~36), so the
**`locals` row count is the fingerprint** that pins the exact binary. These three are the
canonical corpus:

| benchmark | import name | size | `locals` rows (fingerprint) | path under `../karonte/firmware/` |
|---|---|--:|--:|---|
| pluto | `pluto` | 1.4M | 183,288,400 | `d-link/.../squashfs-root/usr/libexec/ipsec/pluto` |
| smbd | `smbd` | 2.7M | 39,074,966 | `NETGEAR/analyzed/R7800/firmware/squashfs-root/usr/sbin/smbd` |
| wpa_supplicant | `wpa_supplicant` | 1.1M | 31,917,658 | `NETGEAR/.../_XR500-V2.1.0.4.img.extracted/squashfs-root/usr/sbin/wpa_supplicant` |
| lk_latest | `lk_latest` | 4.4M | 12,910,766 | `lk/lk_latest` (LK bootloader, 32-bit ARM; **query-phase blowup target**) |

> **Note on row counts vs. the prototype doc.** `LOCALS_TRIE_PROTOTYPE.md` §3/§7 lists the
> *older* fingerprints (pluto 194,565,083 · smbd 130,139,065 · wpa 35,776,521). The perf
> commits on this branch (`ff411cf` "Optimize statement storage" first) legitimately prune rows
> — pluto 194.6M → 183.3M, smbd 130.1M → 39.1M, wpa 35.8M → 31.9M. Those are the intended new
> fingerprints; the CSR flatten and the hash-set spill are then **byte-identical** on top of
> them (storage-only).

### Dense / pathological baseline (do NOT gate on these)

These are **analysis-semantics blow-ups** in the forward field-propagation closure — a
row-count explosion, *not* a storage problem (the trie still stores them at ~30 B/row with
excellent sharing). A memory optimization lowers the constant but cannot fix the row count.
They are recorded here as a **dense-regime baseline**, not as gates.

Measured at `eb22186`, 2026-07-08, 100 GB memory guard (never tripped), per-target wall cap:

| benchmark | `locals` rows | reached/formal | %vars | **peak** | wall | converges? |
|---|--:|--:|--:|--:|--:|---|
| `amuled` (R7800) | 833,407,668 | 48,706 | 557% | **25.8 GB** | 663 s (~11 min) | ✅ **yes** |
| `crtmpserver` | 1,738,597,770 | 99,144 | 860% | **64.66 GB** | 2,576 s (~43 min) | ✅ **yes** |
| `minidlna` | — (no fixpoint) | — | — | **~28.3 GB** *(flat plateau)* | killed @ 90 min | ❌ non-converging |
| `samba_multicall` † | 728,091,638 | 5,685 | 98% | **35.66 GB** | 422 s (~7 min) | ✅ **yes** |
| `wl_ko` † | 642,233,835 | 32,176 | 292% | **28.56 GB** | 337 s (~5.6 min) | ✅ **yes** |
| `umac_ko` † | 616,570,805 | 30,664 | 284% | **29.52 GB** | 301 s (~5 min) | ✅ **yes** |
| `libavcodec` † | — (no fixpoint; ~670 M partial) | — | — | **≥ 29.5 GB** *(still climbing; §3.1)* | killed @ 92 min | ❌ non-converging |

† Added 2026-07-11 at HEAD `46dac56` (not `eb22186`); importable only after the import O(N²) fix.
`samba_multicall` was previously "doesn't scale yet" — it now imports (279 MB bitcode) and the
index converges. `wl_ko`/`umac_ko` are the new **wireless-driver** dense class (reached/formal ≈
31k, indirect-dispatch driven). `libavcodec` is a new **unbounded-productive non-terminator**
(forward-prop `locals` closure; distinct from minidlna's flat plateau — §3.1).

Provenance of the 2026-07-11 additions (paths under `../karonte/firmware/`, 32-bit ARM, `-l pcode`):

| import name | bitcode | path |
|---|--:|---|
| `libndr_standard` | 137 MB | `NETGEAR/analyzed/R8500/_R8500-V1.0.2.106_1.0.85.chk.extracted/squashfs-root/lib/libndr-standard.so.0` |
| `samba_multicall` | 279 MB | `NETGEAR/analyzed/_XR500-V2.1.0.4.img.extracted/squashfs-root/usr/sbin/samba_multicall` |
| `wl_ko` | 88 MB | `Tenda/analyzed/_US_AC18V1.0BR_V15.03.05.05_multi_TD01.bin.extracted/squashfs-root/lib/modules/2.6.36.4brcmarm/kernel/drivers/net/wl/wl.ko` |
| `umac_ko` | 83 MB | `NETGEAR/analyzed/R9000/firmware/squashfs-root/lib/modules/3.10.20/umac.ko` |
| `libavcodec` | 249 MB | `NETGEAR/analyzed/R7000/fw/_R7000P-V1.3.0.8_1.0.93.chk.extracted/squashfs-root/lib/libavcodec.so.55` |

- **Both amuled and crtmpserver now converge** and land well under the prototype-era figures
  (`LOCALS_TRIE_PROTOTYPE.md` §7/§10: amuled ~1.5 B rows / ~68.8 GB, crtmpserver 1.78 B /
  68 GB / 4,250 s). The branch's row-pruning perf commits + the storage fix cut amuled to
  833 M rows / 25.8 GB and crtmpserver to 1.74 B / 64.66 GB / 43 min — real, measurable
  headroom, though still dense.
- **minidlna does not reach a fixpoint.** Memory climbs to ~28.3 GB by ~700 s then sits
  **~flat** through the 90 min cap (it *is* still growing — 28.33→28.38 GB over the last hour,
  ~50 MB, i.e. glacial; prototype plateau was ~39.4 GB). `locals` rows are unmeasurable (the
  fixpoint never prints). Treat as a termination target, not a memory one.
  - **Profiled at the plateau** (two 15 s `sample`s 30 s apart, `scratchpad/prof_minidlna.sh`):
    **~100% of the single main thread is one Datalog join's index enumeration** —
    `taint_index_with_config` → nested `hashbrown::RawIterRange::fold_impl` (×2 full raw-table
    scans) → `Chain(total,delta)::fold` → `FlatMap::next` → `flatten::and_then_or_clear` (~65%
    of leaves), plus `RelIndexCombined::index_get`. The hot work is **probing a relation's
    combined (total∪delta) index and flattening the returned `(F,V)` group** — which on dense
    targets holds ~tens of thousands of leaves. **The hot rule is now named** via per-rule timing
    (`CTADL_INDEX_TIMEOUT_SECS=1200 ctadl index minidlna` → `scc_times_summary`,
    `scratchpad/prof_minidlna_timeout.sh`): the big recursive SCC's rule ranking is
    **#1 `resolvent` rule 2.2 (`mod.rs:1110-1120`) = 1,398.75 s / 69 %**, #2 forward field-prop
    (`mod.rs:1013`) = 475 s / 24 %, #3 same rule other pairing = 108 s. Rule 2.2 enumerates the
    653 M-row `locals` **by formal** via the inverse index `0_3_4` and re-fires as the `resolvent`
    `SmallestCallString` *lattice* refines — matching the sample fingerprint exactly (combined-index
    `index_get` → `FlatMap`/`flatten` of a huge group, nested probe = the 4-way join `call ⋈
    resolvent ⋈ locals_0_3_4 ⋈ critical_summary`). The earlier `#[inline(never)]` probes
    (`substitute_prefix`/`as_formal`/`isout`/`concat` all cold) correctly *refuted* the
    forward-propagation guess — they came back cold because rule 2.2's body calls **no** domain
    function (only a lattice meet + index probe). `minidlna` timed out at 1,200 s with
    **653,703,142** partial `locals` rows (27,115 reached per formal); one late iteration ran
    ~2,700 s solid (memory dead-flat 28.10 GB) since `run_timeout` only checks at iteration
    boundaries. Full analysis in `memory-investigation.md` §7. *(Infra: the main fixpoint was
    converted from `ascent_run!` to a declared-struct `ascent!` to unlock `run_timeout`; default
    behavior is unchanged — env-gated, `Duration::MAX` when unset. Row counts byte-identical, e.g.
    gen_800 = 191936.)*
  - **Ablation (`--alias-rule=false`, `scratchpad/prof_minidlna_noalias.sh`):** the hot stack is
    **identical** (same join) and it **still doesn't converge** — but the plateau
    **halves to ~14.7 GB** (flattens at ~360 s vs ~700 s). So on minidlna the aliasing rule is a
    ~2× *memory amplifier* (feeds extra `assign_like` edges into the closure) but is not the hot
    path and not the cause of non-termination. Full analysis in `memory-investigation.md` §7.
- **`reached per formal` predicts the regime** across a >200× range, independent of binary
  size: tame ≤ ~2,500 (libndr 508, wpa 2,346), dense pluto ~23k, pathological amuled ~49k,
  crtmpserver ~99k. `%vars reached > 100%` means `locals` has more rows than the program has
  variables — each variable reached by many `(formal, path)` combinations.
- These are **long runs** — use the guarded runner (`scratchpad/bench_patho.sh`: 100 GB kill
  cap + wall cap + footprint trajectory) so a non-converging target can't thrash the machine.

---

## 3.1 `libavcodec` non-termination — unbounded productive forward-prop, NOT a plateau (2026-07-11)

`libavcodec` (NETGEAR R7000, 249 MB bitcode, 23,202 formals) is a **new non-converging target**.
It is a *different* failure mode from minidlna, and the distinction only became clear by comparing
**rule time against tuple-count produced** (a true plateau rule is expensive but emits *few*
tuples; this one is expensive because it emits *many*). Profile with the timeout runner:

```bash
CTADL_INDEX_TIMEOUT_SECS=600 RUST_LOG=info ctadl index libavcodec
#  -> "index run TIMED OUT ..." + "index scc times: ..." (per-rule ranking; #![measure_rule_times])
```

**Memory grows monotonically — it does not plateau** (footprint trajectory across two runs):

| t | 30 s | 6 min | 25 min | 40 min | 92 min |
|---|--:|--:|--:|--:|--:|
| phys_footprint | 6.0 GB | 15.1 GB | 19.3 GB | 23.2 GB | 29.5 GB (killed) |

The steep climb knees at ~6 min / 15 GB, then **keeps rising** (19.3 → 23.2 → 29.5 GB). The one
*apparently* flat stretch (19.31 → 19.37 GB over ~4 min at ~21 min) is **within a single 900 s
semi-naive iteration** whose delta is draining as it finishes — the *next* iteration resumes
climbing (`run_timeout` only checks at iteration boundaries, so a single iteration can run ~900 s).
This is **not** the minidlna shape (minidlna is genuinely flat ~28.3 GB for an hour).

**The hot rule is the forward field-propagation `locals` join — and it is *productive*.** From
`scc_times_summary` (SCC 11, `sum of rule times: 1477.6 s`), cross-referenced with final relation
sizes (the tuple-count test):

| head relation | SCC-11 rule time | final tuples | time / output tuple |
|---|--:|--:|--:|
| **`locals`** (`locals_0_1 ⋈ assign_like_0_3 ⋈ paths_0`, `mod.rs:1010–1019`) | **1,470 s (99.5 %)** | **317,119,309** *(→ ~670 M at 92 min)* | **4.6 µs** *(best selectivity of any derived rule)* |
| `summary` | 5.8 s | 186,606 | 31 µs |
| `assign_like` | 1.3 s | 0 new (16.8 M base) | ∞ (unproductive, but negligible time) |
| func_ptr_assign_like / critical_summary / context_* / resolvent | < 50 ms total | 37,320 / 1,238 / ≤1,905 / 6 | — |

- **Time tracks output, so this is not a bad-selectivity plateau rule.** The forward-prop `locals`
  rule dominates the SCC time *because it emits ~99.9 % of all derived tuples* — 317 M and still
  growing (~670 M at the 92-min kill). Its 4.6 µs / output-tuple is the **best**, not worst,
  selectivity in the SCC. `libavcodec` is a genuinely **unbounded (or astronomically large)
  forward-propagation closure**, not an expensive-but-unproductive spinner.
- **Live samples (mid-run, `sample -mayDie`) confirm the hot code** is exactly this rule's
  per-candidate access-path work: `facts::match_prefix` (top of stack), `tailshare::Seq::intern`,
  SipHash `write` / `hash_one`, `LocalsIndCommon::contains` (the semi-naive dedup probe), and
  `facts::prepend_onto` — i.e. `substitute_prefix` + intern + hash + existence-check, per output.
- **The rule body calls `substitute_prefix`** — the function `243c0a1` widened to create more
  flows (§2.1). So the more-flows precision fix feeds straight into the rule whose closure is
  unbounded here: each `assign_like` edge substitutes access-path prefixes into every reaching
  `(formal, path)` tuple, gated only by `paths()`. On codec function-pointer-table code the
  reachable path set is large enough that the closure has no tractable fixpoint.
- **Contrast — minidlna is the true plateau** (`memory-investigation.md` §7): its hot rule is
  resolvent 2.2 (69 %), which is expensive but adds almost nothing (~35 resolvent rows) → flat
  memory. **Two distinct non-termination modes:** unbounded-productive (`locals` fwd-prop:
  libavcodec, and the driver of the dense breachers) vs. expensive-unproductive (resolvent 2.2:
  minidlna). The rule-time-vs-tuple-count comparison is what separates them.
- **Lever:** for libavcodec / the dense breachers, the target is the forward-prop `locals` closure
  itself — tighten the `paths()` feasibility gate (it admits too many access paths), or bound path
  length, so the productive closure is finite/smaller. For minidlna, the target is instead making
  resolvent 2.2 stop re-firing. A storage change helps neither; both are closure-semantics
  problems (consistent with the density-not-size finding in §2.1).

---

## 3.5 Hybrid-inlining activity (does the benchmark exercise the resolvent subsystem?)

Hybrid inlining is the context-sensitive machinery that resolves indirect/virtual calls: an
indirect call whose receiver is reached by a formal seeds a **`critical_summary`**, which
propagates up call edges as a **`resolvent`** (a `SmallestCallString`-lattice fact), which
instantiates **`context_assign` / `context_locals` / `context_summary`** edges back into the
analysis. Only benchmarks with **resolvable indirect dispatch** exercise it at all; on
straight-line C it is dormant. This matters because the resolvent-propagation rule 2.2
(`mod.rs`) is the hot path on the dense/non-terminating targets (see §3 minidlna, and
`memory-investigation.md` §7). Use this table to pick a benchmark when working on that
subsystem — most of the corpus is useless for it.

Read the numbers off the `RUST_LOG=info` line
`hybrid inlining: critical_summary: <r> (<seeds>/<denom>), resolvent: <R>, context_assign: <ca>, context_locals: <cl>, context_summary: <cs>`.
Baseline `eb22186`:

| benchmark | critical_summary (seeds) | resolvent | context_assign | context_locals | context_summary | **category** |
|---|--:|--:|--:|--:|--:|---|
| `amuled` | 113 | **0** | 0 | 0 | 0 | **none** — resolvent subsystem inert |
| `gen_N`, most `.tnt` | 0 | **0** | 0 | 0 | 0 | **none** — no resolvable dispatch |
| `wpa_supplicant` | 219 | 2 | 0 | 0 | 0 | **trace** — a couple resolvents, no context output |
| `smbd` | 146 | 24 | 5 | 0 | 0 | **trace** — resolvents fire, ~no context materializes |
| `pluto` | 91 | 38 | 134 | 1,922 | 1 | **moderate** — real context edges reach the analysis |
| `minidlna` | **7,991** | 35 † | 78 † | 936 † | 0 † | **dominant** — see caveat below |
| `crtmpserver` | *(pending)* | | | | | *(43-min run; row added when it lands)* |
| `hybrid_inlining.tnt` | 2 | 4 | 4 | 22 | 4 | functional test for the subsystem |

† minidlna values are a **partial timeout snapshot** (`CTADL_INDEX_TIMEOUT_SECS=1200`), not a
fixpoint — it does not converge.

**The final `resolvent` count is a poor proxy for hybrid-inlining *cost*.** minidlna carries only
~35 resolvent rows yet the resolvent-propagation rule is **~69 % of its (non-terminating)
fixpoint CPU** — because `resolvent` is a `SmallestCallString` *lattice* that re-fires while the
rule enumerates the per-formal `locals` group on every call edge (`memory-investigation.md` §7).
So categorize by two independent signals:
- **Activity / output** — `critical_summary` seed count and the `context_*` edges actually
  materialized (what changes the analysis result). Dominant on minidlna (7,991 seeds), moderate
  on pluto, trace elsewhere.
- **Cost** — the resolvent-propagation *rule time* (from `scc_times_summary`), which tracks the
  per-formal group size, not the resolvent row count. Only exposed on dense targets.

> **Semantics caveat (resolvent-rule refactors).** The `context_*` counts here are **not** a
> pure storage fingerprint — a change to the resolvent propagation rules can move them while
> `locals`/`assign_like` stay byte-identical. E.g. narrowing rule 2.2 from path-coincidence to
> precise call-argument linkage (`call_arg_resolvent`, `v.as_call_arg()`) drops over-approximated
> resolvents: **pluto** `resolvent 38→28`, `context_assign 134→128`, `context_locals 1922→1916`;
> **smbd** `resolvent 24→23` (inert); sparse targets unchanged. When touching this subsystem,
> diff the whole `hybrid inlining:` line old-vs-new on pluto (the most sensitive terminating
> target), not just `locals`.

---

## 4. How to reproduce (commands)

Imports are cached under `~/.local/state/ctadl/imports/<name>/`; the active corpus is already
imported, so you only ever re-run `index`. Import once only if the cache is gone:

```bash
# import (slow, once per binary — invokes Ghidra analyzeHeadless itself)
./target/release/ctadl import -l pcode <ELF-path> -n <name>
```

Re-index (cheap; this is the benchmark inner loop). Pass the import name **exactly once**:

```bash
RUST_LOG=info ./target/release/ctadl index <name>
#  -> "relation increase: locals: <ROWS>, ... reached per formal ..."   (row-count gate)
```

The reusable runner (peak footprint + wall + rows, N runs, appends to a log):
`scratchpad/bench.sh <name> <runs>` (in this session's scratchpad). One-liner equivalent:

```bash
RUST_LOG=info /usr/bin/time -l ./target/release/ctadl index <name> 2>&1 \
  | grep -E 'peak memory footprint|real|relation increase: locals:'
```

Run the full suite:

```bash
for n in gen_800 gen_1600 gen_3200; do bench.sh $n 3; done   # synthetic, medians
bench.sh smbd 1; bench.sh wpa_supplicant 1; bench.sh pluto 3 # firmware (pluto is variance-prone)
```

---

## 5. Reference trajectory (where the current numbers came from)

Context for the current baseline, so a future optimization can see the whole arc. Full detail
in `memory-investigation.md`.

**Firmware improvement stack** (`73109ea` baseline → current `eb22186`), peak memory:

| target | baseline `73109ea` | pre-fix HEAD (CSR only) | **current `eb22186`** |
|---|--:|--:|--:|
| smbd | 7.43 GB | 2.37 GB | *(see §2)* |
| wpa_supplicant | 2.85 GB | 1.82 GB | *(see §2)* |
| pluto | 8.06 GB / 268 s | ~5.2 GB / **~867 s** ⚠ | *(see §2)* |

Key episode: the CSR flatten (`8f39ae8`) shrank the `locals` store but its delta→total merge
re-allocated the whole accumulated group every fixpoint iteration → **O(N²)**, invisible on
sparse targets but a **3.2× time regression on dense pluto** (268 s → ~867 s). The fix
(`eb22186`) makes each `(F,V)` group **size-adaptive**: a sorted `Vec` up to
`GROUP_HASHSET_THRESHOLD = 64` leaves, promoting permanently to a `HashSet` above it — restoring
O(delta) merge + O(1) existence on dense groups while small groups keep the compact vec. The
threshold `64` is a tunable knob (memory vs. the last of the speedup).

**Prototype-era baselines** (`LOCALS_TRIE_PROTOTYPE.md` §3, default-Ascent storage vs. the
original trie prototype — different, larger row counts; kept only as historical scale):
smbd 27.5 GB → 9.1 GB, wpa 9.0 GB → 3.5 GB, pluto 25.7 GB → 10.6 GB.

---

## 6. Open levers (ranked, from `memory-investigation.md` §8)

1. Apply the same size-adaptive flat-group treatment to **`assign_like`** (now the largest
   in-fixpoint store).
2. Cheapen the **`alias_of_formal`** pre-pass (~17% of peak) — hand-written transitive closure
   instead of a full Ascent fixpoint over 230k `copy_edge` rows.
3. Defer/stream **`facts.try_save`** (~10% of peak) so its in-memory buffer isn't co-resident
   with the fixpoint.
4. Shrink the **in-memory IR** (~26% of peak; 3.8× expansion of the deserialized bitcode).
5. Do **not** pursue allocator swaps (measured ineffective — `memory-investigation.md` §5).
6. Characterize the **super-linear indirect-call path** (0 rows on the current corpus; the real
   production risk, and the class behind amuled/crtmpserver).
