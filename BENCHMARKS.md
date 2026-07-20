# CTADL `index` memory/time benchmarking baseline - DO-NOT-MERGE

*Reusable baseline for optimizing `ctadl index` (the Ascent Datalog phase) peak memory.
Re-run the suite after each change, diff against §2, and update it. Platform: macOS
(Darwin), `phys_footprint` accounting, 20-core / 128 GB machine, one job at a time.*

**Baseline:** `main` @ `87bdad1` vs `find-memory-blowup-2` @ `a35437e` (synthetic + firmware
tables re-measured 2026-07-17 on branch `5c570cb`; `main` binary unchanged at `87bdad1`).
**Measured:** 2026-07-16/17, Ghidra 12.0.4 (the flake's pinned `ghidra-bin`), idle machine.

> ### ⚠ Read this before comparing against any older copy of this document
>
> Every number in this file was re-measured from scratch on 2026-07-16/17. The previous
> tables (§2 "commit `eb22186`", §2.1 "HEAD `46dac56`", §2.2 "HEAD `29044c6`", §2.5/§2.6
> Load-Store and require-Loads tables, §5 "reference trajectory") have been **deleted**
> because **they were measured on a lineage that never landed.** `eb22186`, `46dac56`,
> `29044c6`, `243c0a1`, `8f39ae8`, `73109ea` all still exist as git objects but are on
> **no branch** — `git for-each-ref --contains 29044c6` is empty, and
> `git merge-base --is-ancestor <c> main` is false for every one of them. So the whole
> optimization stack those tables describe (CSR flatten, size-adaptive hash-set spill,
> the import O(N²) fix, the `243c0a1` `match_prefix` precision fix) **is not in `main`
> and not in this branch.** Those tables never described code you can check out.
>
> **The corpus itself did NOT change.** An earlier revision of this warning claimed the
> fingerprints had moved because Ghidra 12.0.4 recovers denser code. **That was wrong, and
> it is retracted.** Formal counts — a direct fingerprint of what Ghidra recovered — are
> *identical* to the old tables on every target that records one:
>
> | target | old table (rows ÷ reached/formal) | re-measured 2026-07-17 |
> |---|--:|--:|
> | `ath_dfs` | 434 (stated explicitly) | **434** |
> | `cfg80211` | 15,999,406 ÷ 5,642 = 2,836 | **2,836** |
> | `ath_dev` | 329,701,247 ÷ 98,242 = 3,356 | **3,356** |
>
> Bitcode sizes corroborate (old → today's branch): `cfg80211` 9.3 → 9.67 MB, `ath_hal`
> 14.8 → 14.3 MB, `ath_dfs` 2.4 → 2.56 MB. Same binaries, same Ghidra recovery, same facts.
>
> Consequence — **the opposite of what the retracted claim implied**: the old absolute
> numbers *are* directly comparable, and today's `main` is a **large regression against the
> unlanded lineage on identical inputs** (`ath_dfs`: 1.52 M rows / 2 s → 2.31 M rows /
> **95 s**, same 434 formals). See §5.
>
> The durable *qualitative* findings from that work are preserved in §6, flagged as
> unverified against the current corpus.

---

## 0. How to use this document

1. Make your change, `cargo build --release`.
2. Re-run the suite (§4). Synthetic is seconds; the firmware targets are minutes to hours,
   and several do not converge at all — run them guarded.
3. Compare against **§2**. The gates:
   - **`locals` row count** must not drift unless you *intend* a semantic change — a drift
     means you changed analysis results, not just storage.
   - **peak `phys_footprint`** is the primary optimization target.
   - **wall time** must not regress (a storage win can hide an O(N²) time blowup).
4. Update §2 (and the commit/date header) with your new numbers.

**Treat the Ghidra version as part of the measurement**, and record it — but note that
going from the pre-2026 imports to 12.0.4 turned out to change *nothing* observable here
(identical formal counts, bitcode within ~10%; see the ⚠ block). So a Ghidra bump is a
reason to re-check the fingerprints, not an automatic reason to void them.

---

## 1. What / how to measure (don't re-derive this)

**Use physical footprint, not RSS.** macOS compresses cold pages, so `ps rss` badly
undercounts. Activity Monitor's "Memory" column == `phys_footprint`.

- **Peak** comes from `/usr/bin/time -l` → the `peak memory footprint` line (raw bytes).
  This *is* the `phys_footprint` high-water mark.
- **Row count** comes from the `RUST_LOG=info` line
  `relation increase: locals: <ROWS>, <F> formals, <R> reached per formal, <P>% of variables reached`.
  `reached per formal` (= rows / formals) is the aliasing-density predictor.
- **Guarded runs are mandatory on this corpus** — most firmware targets will consume all
  128 GB if unguarded. Poll `footprint -p <PID> -f bytes` every ~4 s and kill above a cap
  (`.scratch/bench/bench.sh`).
  - **Gotcha:** `footprint -p <pid> -f bytes` prints several lines; the value you want is
    `phys_footprint:`, *not* the last line. Parse it explicitly:
    `footprint -p "$pid" -f bytes | awk '/phys_footprint:/ {print $2; exit}'`.
    A guard that greps the wrong line silently never fires.

---

## 2. Baseline: `main` (`87bdad1`) vs `find-memory-blowup-2` (`a35437e`)

Ghidra 12.0.4, one pre-analyzed `.gpr` per binary imported by **each** side separately (the
IR differs — "Require Loads"), so the facts are identical and only ctadl differs. Idle
machine. Guard: 85 GB / 3 h wall unless noted. `br` = the branch.

### Synthetic (`gen_N`) — regenerated; see the §3 caveat

Re-measured 2026-07-17 with the current branch build (`br` = `5c570cb`, `main` = `87bdad1`);
imports rebuilt from the same `.gpr`s. Row counts and reached/formal are identical to the
previous run; peak/wall move within noise.

| benchmark | side | bitcode | `locals` rows | reached/formal | %vars reached | **peak** | wall |
|---|---|--:|--:|--:|--:|--:|--:|
| `gen_800`  | main | 2.17 MB | 146,667 | 30.49 | 2.58% | 71.6 MB | 2.41 s |
| `gen_800`  | **br** | 1.97 MB | 158,170 | 32.88 | **74.4%** (46,550/62,604) | **66.6 MB** | **0.36 s** |
| `gen_1600` | main | 4.38 MB | 293,067 | 30.49 | 2.58% | 116.4 MB | 4.93 s |
| `gen_1600` | **br** | 3.98 MB | 315,770 | 32.86 | **74.4%** (92,950/125,004) | 116.3 MB | **0.83 s** |
| `gen_3200` | main | 8.81 MB | 585,867 | 30.50 | 2.58% | 220.4 MB | 10.16 s |
| `gen_3200` | **br** | 8.02 MB | 630,970 | 32.84 | **74.4%** (185,750/249,804) | **197.9 MB** | **2.17 s** |

Sparse best case (reached/formal ≈ 30). Branch: **~4-5× faster**, memory flat, rows +8%.
Hybrid inlining is inert on both sides for all `gen_N` (`critical_summary: 0, resolvent: 0`)
— the property §6 requires of this benchmark.

> **⚠ The two sides' `%vars reached` are NOT on the same basis — do not compare them
> directly.** The current branch build (`5c570cb`) changed the accounting: `br` now prints
> `74.4% of variables reached (<reached>/<total>), <rows-per-var>`, i.e. reached variables ÷
> total variables in the IR. `main` (`87bdad1`) still prints the old `2.58% of variables
> reached` with no fraction — a different, much smaller denominator convention. The 2.58%
> figure is stable across all three sizes; the branch figure is a flat **74.4%** across all
> three (rows-per-variable a flat 2.53). The firmware %vars in §2's next table were taken
> from the *old* `main`-style formula and are likewise not comparable to the new branch
> figures. The firmware %vars **have now been re-measured** on `5c570cb` for the three
> converging targets — see the firmware table and its follow-up note. The old `br` %vars
> values there turned out to be rows-per-variable; the true reached fractions land in the
> 49–62% band.

### Firmware

Branch (`br`) re-measured 2026-07-17 on `5c570cb`; `%vars` for `br` is now the **true
reached fraction** under the new formula, with rows-per-variable in parentheses. `main`
rows are unchanged (`87bdad1`) — its `%vars` is the old-formula figure and is **not**
comparable to the branch fraction (see note below). `ath_dev`/`cfg80211` `main` were not
re-run: they explode past 85 GB (KILLED, no `%vars` to obtain).

| benchmark | side | bitcode | `locals` rows | reached/formal | %vars | **peak** | wall | converges? |
|---|---|--:|--:|--:|--:|--:|--:|---|
| `ath_dfs`  | main | 2.29 MB | 2,313,189 | 5,330 | 27% (old) | 0.20 GB | 85.2 s | ✅ |
| `ath_dfs`  | **br** | 2.56 MB | **1,072,748** | 2,472 | **62.1%** (4.50 rows/var) | 0.23 GB | **2.34 s** | ✅ |
| `ath_dev`  | main | 13.6 MB | — | — | — | **≥91.6 GB — KILLED** | — | ❌ |
| `ath_dev`  | **br** | 15.3 MB | **41,412,939** | 12,340 | **57.2%** (39.63 rows/var) | **2.98 GB** | **48.1 s** | ✅ |
| `cfg80211` | main | 8.26 MB | — | — | — | **≥92.9 GB — KILLED** | — | ❌ |
| `cfg80211` | **br** | 9.67 MB | 730,109,557 | 257,443 | **48.7%** (954.42 rows/var) | **28.4 GB** | 429 s | ✅ |
| `ath_hal`  | main | 13.0 MB | — | — | — | **≥94.4 GB — KILLED** | — | ❌ |
| `ath_hal`  | **br** | 14.3 MB | — | — | — | **≥92.8 GB — KILLED** | — | ❌ |
| `wpa_supplicant` | main | 28.1 MB | — | — | — | **≥92.1 GB — KILLED** | — | ❌ |
| `wpa_supplicant` | **br** | 30.7 MB | — | — | — | **≥92.4 GB — KILLED** | — | ❌ |
| `lk_latest` | main | 10.2 MB | — | — | — | — | **killed @ 3 h wall** | ❌ |
| `lk_latest` | **br** | 11.7 MB | — | — | — | **≥91.3 GB — KILLED** | — | ❌ |
| `smbd` | main | 59.1 MB | — | — | — | — | **killed @ 3 h wall** | ❌ |
| `smbd` | **br** | 69.8 MB | — | — | — | — | **killed @ 3 h wall** | ❌ |
| `pluto` | main | 29.7 MB | *(not completed — run stopped)* | | | | | |

**"KILLED" = tripped the 85 GB footprint guard.** The figure is where the 4 s poller caught
it, i.e. a *lower bound* on peak, not a converged peak.

> **The old `br` `%vars` values were mislabeled rows-per-variable, not reached-fractions.**
> The 2026-07-17 re-run on `5c570cb` makes this unambiguous: every previous `br` `%vars`
> entry equals the new build's *rows-per-variable* figure, not its reached fraction —
> `ath_dfs` old 4.5% = new 4.50 rows/var (true reached 62.1%); `ath_dev` old 40% = 39.63
> rows/var (true 57.2%); `cfg80211` old 954% = 954.42 rows/var (true 48.7%). The percentages
> now in the table are the genuine reached fractions. Note the true fractions cluster in the
> 49–62% band and do **not** track the blowup — `cfg80211`, by far the densest target
> (257k reached/formal, 954 rows/var), has the *lowest* reached fraction of the three. So
> `%vars` reached is not a blowup predictor; **`reached/formal` and rows-per-variable are.**
> `main`'s `%vars` (e.g. `ath_dfs` 27%) is the old-formula number on the unchanged `main`
> binary and is on yet another basis — do not compare it to the branch fractions.

### What this says

- **The branch strictly dominates `main` on this corpus.** It is faster on every target
  that finishes, and there is **no target where `main` converges and the branch does not**.
- **The branch converges on two targets where `main` explodes past 90 GB.** `ath_dev` is
  the headline: `main` is killed at **≥91.6 GB**, the branch finishes in **48 s at 2.98 GB**.
  `cfg80211`: `main` killed at ≥92.9 GB, branch converges at 28.4 GB.
- **But the branch does not fix the blowup class.** `ath_hal`, `wpa_supplicant`,
  `lk_latest` and `smbd` still blow past 85 GB or a 3 h wall on **both** sides. The branch
  moves the boundary; it does not remove it.
- **Memory is a trade, not a free win.** On the small converging target (`ath_dfs`) the
  branch costs **+16% peak** (0.20 → 0.23 GB) while cutting rows 54%. Rows are down and
  base facts are up (below). On the dense targets that trade pays enormously; on tame ones
  it is a mild loss.
- **`reached/formal` still separates the regimes, and it is comparable to the old document**
  — same corpus, same formal counts. That makes the shift alarming rather than merely
  incommensurable: `cfg80211` converges at **257,443** reached/formal against the old
  table's **5,642** for the same 2,836 formals, i.e. 2.6× past the worst value the corpus
  had ever produced (crtmpserver, 99k). Density this far above the old envelope on unchanged
  input is an engine result, not a corpus property — see §5.

### Mechanism (`ath_dfs`, main → br)

*The `locals`/`assign_like`/`copy_edge`/`program_paths` counts below are stable across the
2026-07-17 re-run (they are semantic). The `mem after SSA` / `mem @ entry` deep-profiling
figures are from the earlier `a35437e` run and were **not** re-measured on `5c570cb`; the
`+16%` reflects the current peak delta (0.20 → 0.23 GB).*

| quantity | main | br | |
|---|--:|--:|---|
| `locals` rows | 2,313,189 | 1,072,748 | **−54%** — the closure shrinks |
| `assign_like` base rows | 161,027 | 454,148 | **+182%** — the cost |
| `program_paths` seeded rows | 322,598 | 560 | **576× fewer** (`7ca59b2`) |
| `copy_edge` | 132,641 | 416,543 | |
| depth-3 paths | 28 | 14 | shallower path set |
| mem after SSA transform | 31.4 MB | 72.3 MB | |
| mem @ entry (facts loaded) | 70.3 MB | 108.7 MB | the +16% peak, explained |

Two changes on the branch act independently, and **their contributions are not separated by
these measurements** (see §5):

1. **"Require Loads"** (`30b2849`) — a field write no longer re-defines the whole aggregate,
   so the frame pointer stops being re-versioned per store. This is what cuts `locals`. It
   pays for that with ~3× the `assign_like` base facts (every field read becomes a
   taint-carrying temporary), which is why entry/peak memory rises on small targets.
2. **`7ca59b2` "program_paths set"** — a one-line `Vec` → `HashSet`. The seed vector was
   assigned straight into the Ascent relation, so ~576× duplicate paths entered the
   fixpoint's gate relation and made the forward-propagation join re-fire per duplicate.
   `locals` dedups, so this costs *time*, not rows.

---

## 3. The corpus

**All previous imports were gone and were rebuilt on 2026-07-16 with Ghidra 12.0.4.** This
turned out to be a non-event: the rebuilt facts match the old tables exactly (identical
formal counts on every target that records one — see the ⚠ block), so row counts in older
copies of this document remain comparable to §2. The deltas here come from the engine, not
the importer.

### Provenance (32-bit ARM, `-l pcode`, paths under `../karonte/firmware/`)

| import | binary | path |
|---|--:|---|
| `ath_dfs` | 103 KB | `NETGEAR/analyzed/R9000/firmware/squashfs-root/lib/modules/3.10.20/ath_dfs.ko` |
| `cfg80211` | 474 KB | `…/3.10.20/cfg80211.ko` |
| `ath_dev` | 653 KB | `…/3.10.20/ath_dev.ko` |
| `ath_hal` | 840 KB | `…/3.10.20/ath_hal.ko` |
| `lk_latest` | 4.4 MB | `lk/lk_latest` (LK bootloader) |
| `wpa_supplicant` | 1.1 MB | `NETGEAR/analyzed/_XR500-V2.1.0.4.img.extracted/squashfs-root/usr/sbin/wpa_supplicant` |
| `smbd` | 2.7 MB | `NETGEAR/analyzed/R7800/firmware/squashfs-root/usr/sbin/smbd` |
| `pluto` | 1.4 MB | `d-link/analyzed/_DIR890LA1_FW111b02_20170519_beta01.bin.extracted/squashfs-root/usr/libexec/ipsec/pluto` |

`pluto` had **five** same-named candidates in the Karonte tree; the DIR-890L one above is
the choice made here — `pluto` never completed on either side, so no row count exists to
check it against the old document's. (Now that the corpus is known to be unchanged, a
completed `pluto` run *would* be a usable fingerprint for identifying the original binary.)

### ⚠ The synthetic `gen_N` benchmarks are a **reconstruction**

**The original generator was lost** — it exists nowhere on this machine, and
`ctadl-plans` has no commits to recover it from. `.scratch/bench/gen/gen.py` is a
reconstruction from the properties the old docs recorded (straight-line C, a 4-field
word-sized `struct box` at offsets 0/4/8/12, ~4 formals/function, no indirect dispatch),
compiled with `cc -O1 -fno-inline` and imported through Ghidra like any other binary.

It reproduces the documented behaviour closely (gen_800: 146,667 rows here vs 156,787
recorded; `resolvent: 0` on both sides as required) but it is **not the original program**.
Treat `gen_N` rows as a self-consistent A/B, not as continuous with any historical figure.

### Targets dropped from this baseline

`amuled`, `crtmpserver`, `minidlna`, `libavcodec`, `samba_multicall`, `wl_ko`, `umac_ko`,
`libndr_standard` are **not** re-measured. Even on the unlanded lineage these already took
11 min to >90 min or never converged; against today's `main` — which needs 95 s for an
`ath_dfs` the lineage did in 2 s, and blows past 91 GB on a 653 KB `ath_dev` — they are not
runnable as a routine gate. Re-add them only with a guard and an explicit time budget.

---

## 4. How to reproduce

Everything below lives in `.scratch/bench/` (gitignored). `env.sh` pins the JDK, Ghidra,
and both ctadl binaries.

**Analyze each binary into a reusable Ghidra project once.** This is the key trick: both
sides then import from the *same* project via `-process`, which skips Ghidra's expensive
auto-analysis and guarantees identical facts.

```bash
.scratch/bench/mkproj.sh <name> <binary-path>     # → .scratch/bench/ghidra/<name>/<name>.gpr
```

**Import + index both sides:**

```bash
.scratch/bench/suite.sh <name> [cap_gb] [wall_s]  # imports <name>_main / <name>_br, then benches each
```

Under the hood, per side:

```bash
ctadl import -l pcode .scratch/bench/ghidra/<name> -n <name>_<side>
RUST_LOG=info /usr/bin/time -l ctadl index <name>_<side>
```

**Full sweep:** `.scratch/bench/runall2.sh` (85 GB guard, 3 h wall, cheap → expensive).
Raw logs land in `.scratch/bench/logs/`; the collected table is `.scratch/bench/RESULTS.md`.

Notes:
- `main` has no `CTADL_REUSE_FACTS`; the branch does (skips Ghidra and re-runs only
  facts→IR). The `.gpr` route is used instead because it works symmetrically on both.
- Ghidra needs a JDK 21 and `GHIDRA_HOME`; neither is on `PATH` by default here. See
  `env.sh`.
- **Do not measure on a busy machine.** `ctadl index` is single-threaded, but a
  `cargo build -j20` next to it visibly inflates wall time. `runall.sh` gates on an idle
  machine for this reason.

---

## 5. Open questions this baseline does **not** answer

- **The branch's two wins are not attributed.** "Require Loads" and the `program_paths`
  dedup (`7ca59b2`) were measured only together. The clean experiment is `main` + `7ca59b2`
  alone (a 4-line commit) — that isolates how much of the 34× `ath_dfs` speedup is just the
  duplicate-gate bug, and how much is the representation change. **Worth doing before
  attributing credit to Require-Loads.** If most of the time win is `7ca59b2`, that fix is
  independently mergeable to `main` and much lower-risk than the IR change.
- **`ath_hal` blows up on both sides** (≥94 GB / ≥93 GB). The old §2.5 analysis predicted
  exactly this shape — `ath_hal` is dense in *deep nested field paths* rather than in stack
  versioning, so removing the versioning amplifier doesn't help it. That prediction now has
  a much starker confirmation than the 8.5× regression it was based on.
- **`pluto` was never completed** on either side; `smbd`/`lk_latest` hit wall caps without
  producing a row count. Their peaks are unknown.
- **Why is `main` so much worse than the unlanded lineage on identical facts?** This is now
  the highest-leverage open question. The corpus is unchanged (see the ⚠ block), so the old
  tables are a legitimate baseline, and today's `main` regresses hard against them. The
  evidence separates by *type*:
  - **Row counts are semantic** — CSR flattening and hash-set spilling cannot change the
    fixpoint's least model, so row deltas must come from the missing `243c0a1` `match_prefix`
    precision fix. `ath_dfs` on `main`: 1.52 M → 2.31 M rows (+53%) at the same 434 formals.
  - **Time/memory at comparable row counts** points at the missing data-structure work.
    `ath_dfs` on `main` went 2 s → **95 s** for only 53% more rows — a 47× slowdown rows do
    not explain.

  Recovering `243c0a1` and the CSR/spill commits from the dangling objects and testing them
  against `main` is probably worth more than any further A/B of this branch.
- **The branch diverges from the lineage in *both directions*, on identical facts.** On
  `ath_dev` the branch yields 41.4 M rows / 2.98 GB against the lineage's 329.7 M / 16.67 GB
  (**8× fewer** rows); on `cfg80211` it yields 730 M vs 16.0 M (**45× more**). Same binaries,
  same Ghidra facts, same formal counts. No current hypothesis explains a swing that large in
  opposite directions on two modules from the same firmware image. Until it is explained,
  treat per-target results here as not generalizing.
- **Is Ghidra 12.0.4 recovering *good* code?** Still unaudited, but **no longer implicated in
  the blowup** — it produces the same facts the old tables were measured on, so it cannot be
  the cause of any delta in this document.

---

## 6. Prior findings, preserved (⚠ measured on the unlanded lineage)

These are the durable *qualitative* results of the earlier investigation. The **corpus** they
used is the same one measured here, so their row counts are comparable — but the **engine**
was the dangling lineage, which has fixes neither side under test has. Read the figures as
describing that engine, not `main`.

- **The blowup is redundant representation, not spurious flows.** A full audit of
  `ath_dev_ko`'s `locals` closure — an independent Python reimplementation of the two
  forward field-propagation rules, reproducing the engine's total to the row — found **no
  fabricated flows**. 88-92% of `locals` rows in hot functions had a `%__stack_top` subject,
  because every frame store was lowered to `%__stack_top = update(.[slot].deref := v)`, a
  whole-aggregate re-definition minting a new SSA version (up to 5,624 per function).
  Collapsing those versions to one representative shrank frame rows up to **1,963×**.
  This diagnosis is what "Require Loads" acts on, and §2's `ath_dev` result (91.6 GB → 2.98 GB)
  is strong independent support for it.
- **An independent def-use oracle confirmed soundness on a different axis.** Built from the
  raw pretty-printed IR with plain field-insensitive reachability (sharing no code with the
  propagation rules), it found: every propagated `assign_like` program edge is a real def-use
  path in the IR (102,063/102,063 on one function, 100.000%); the closure never reaches a
  variable the raw IR doesn't connect; and it stays a strict *under*-approximation of a sound
  permissive bound. A ported rule-bug cannot launder through it.
- **Two distinct non-termination modes**, separated by comparing rule time against tuples
  produced: *unbounded-productive* (the forward-prop `locals` join — expensive because it
  emits ~99.9% of all derived tuples; `libavcodec`) vs. *expensive-unproductive* (the
  resolvent rule 2.2 — ~69% of CPU while adding ~35 rows; `minidlna`'s flat plateau).
  A storage change helps neither.
- **Fact-level compression cannot recover the require-Loads base-fact cost on pcode.** Over
  `ath_dev_ko`'s 120,339 loads, only ~1% were single-use and 100% appeared in a non-expression
  position — because in pcode a load produces a *pointer* that is reused as the base for many
  downstream loads/stores. Both an IR copy-fold and a codegen composition fold were tried and
  reverted (~1% and 0%).
- **Aliasing is a memory amplifier, not the hot path.** `--alias-rule=false` roughly halved
  minidlna's plateau (~28.3 → ~14.7 GB) without changing the hot stack or fixing
  non-termination.
- **Allocator swaps were measured ineffective.** Don't re-litigate.
- **`gen_N` can never surface the blowup** — it is permanently in the sparse best case
  (reached/formal ≈ 30) with the resolvent subsystem inert. It is a time/regression canary
  only. Confirmed again in §2.
- **The proposed direction** was to replace syntactic path composition with a real
  Datalog points-to analysis (Smaragdakis & Balatsouras style), for which the Load/Store IR
  is a prerequisite rather than a solution. §2 is consistent with that: the representation
  change alone moves the boundary without removing the blowup class.
