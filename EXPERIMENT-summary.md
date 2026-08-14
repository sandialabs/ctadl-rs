# Head to head: ctadl-souffle vs ctadl-rs on firmware — DO-NOT-MERGE

**Date:** 2026-08-13 · **Old:** `ctadl-souffle` 0.14.1 (Python + Souffle 2.3, `../ctadl`) ·
**New:** `ctadl-rs` 0.1.2 (this repo, branch `head-to-head-vs-ctadl-souffle-firmware`)
**Corpus:** 5 binaries from `operation-mango-public/firmware/7_firmware`
**Everything lives in** `firmware-eval/run/head2head/`; raw data in `firmware-eval/run/head2head/results/`.

---

## TL;DR

On the same 5 firmware binaries, with the **same models**, the **same Ghidra**, and
each engine's own defaults suppressed:

| | old (souffle) | new (rs) | old / new |
|---|--:|--:|--:|
| import size, total | 573 MB | 99 MB | **5.8×** |
| index size, total | 703 MB | 32 MB | **22.0×** |
| function summaries, total | 841 | 4,752 | 0.18× |
| SARIF taint paths, total | **2** | **141** | 0.01× |
| binaries with ≥1 path | 1 of 5 | 4 of 5 | |

The new engine builds an index **22× smaller**, derives **5.6× more function
summaries** from it, and reports **141 paths where the old engine reports 2**.
Every path the old engine found, the new engine also found — the old engine's
findings are a strict subset (0 old-only endpoint pairs on all 5 binaries).

![results](firmware-eval/run/head2head/results/head2head.png)

---

## What was held equal

The comparison is only worth something if the two engines differ in *engine* and
nothing else. Three things were equalized.

**1. One Ghidra.** Both engines lift the binary themselves, so a Ghidra version
skew would show up as an analysis difference that is really a frontend
difference. `nix develop .#head2head` (added to the root `flake.nix`) puts both
tools and **one** `ghidra-bin` 12.0.4 on `PATH` and exports `GHIDRA_HOME`, which
is how both frontends locate it.

**2. One model set, with both engines' defaults off.** Neither engine runs its
shipped models. `firmware-eval/run/head2head/models/build_models.py` generates
one shared set from a single source of truth and emits it in each engine's
syntax:

- **49 propagation (library) generators** — the union of ctadl-rs's
  `native-index.jsonl`, ctadl-souffle's `pcode/default-index.json`, and the
  string-builder models in `firmware-eval/models/cmdi-firmware.json5`.
- **12 endpoint generators** — the command-injection sources and sinks from
  `cmdi-firmware.json5` (Operation Mango's source/sink set).

Union rather than intersection for a mechanical reason: ctadl-souffle has no
`--no-default-models`, so `index --models F` *adds* F to its built-in defaults.
Its defaults are unavoidable, so the only set both engines can end up with is one
that contains them. ctadl-rs is then run with `--no-default-models`, and neither
engine contributes anything of its own.

**3. Same phases, same measurements.** import → index → query for both, each
phase guarded by a wall timeout and a physical-footprint cap, index size measured
after index and *before* query (ctadl-souffle writes query results back into
`ctadlir.db`, which would otherwise inflate it).

### What could not be held equal — the model translation

The two engines spell access paths differently, so the shared set is a faithful
translation, not the same bytes. All of the differences:

| ctadl-rs | ctadl-souffle | why |
|---|---|---|
| `.deref` | `.*` | each engine's spelling for "the bytes at this pointer"; `.*` is what souffle's pcode frontend actually emits for a dereference |
| source `saturating: true` | `all_fields: true` | "all of this is attacker-controlled, however the callee indexes in" |
| sink `wildcard` (default) | dropped, or `all_fields: true` on a sink whose port carries no field | rs matches every extension of the port; souffle's nearest equivalent |

`where` clauses need no translation: `signature_match` + `names` and `name` +
`pattern` (a regex) match the function's short name identically in both.

One translation caveat worth recording: souffle expands `Argument(*)` only to a
function's *declared* arity, and Ghidra declares `sprintf` with arity 2 — so
`Argument(*)` never reaches a format argument there. Expanding it to explicit
indices `Argument(0..7)` was tested and changed nothing (still 0 paths on
`arp_check`), so the shipped model keeps `Argument(*)`.

---

## The corpus

Five binaries, chosen for spread rather than for a favourable result — 3 vendors,
2 architectures, 18 K to 311 K. These are the daemons command injection actually
lives in. The upper size bound is set by ctadl-souffle: the megabyte-class
`httpd` binaries in this corpus are out of its reach in any reasonable budget.

| label | device | arch | size |
|---|---|---|--:|
| `r7000_arp_check` | Netgear R7000 | ARM | 18 K |
| `dlink878_nvram_daemon` | D-Link DIR-878 | MIPS | 23 K |
| `r7000_rc` | Netgear R7000 | ARM | 112 K |
| `r6400_acos_service` | Netgear R6400v2 | ARM | 138 K |
| `ac15_netctrl` | Tenda AC15 | ARM | 311 K |

All 10 jobs (5 binaries × 2 engines × 3 phases) completed; nothing timed out,
crashed, or hit the memory cap.

---

## Results

| binary | engine | import | index | summaries | SARIF paths | wall |
|---|---|--:|--:|--:|--:|--:|
| `r7000_arp_check` | old | 29.6 M | 46.6 M | 76 | 0 | 20 s |
| | **new** | **4.8 M** | **2.0 M** | **198** | **36** | 20 s |
| `dlink878_nvram_daemon` | old | 15.7 M | 30.3 M | 71 | 0 | 26 s |
| | **new** | **2.3 M** | **224 K** | **159** | **0** | 26 s |
| `r7000_rc` | old | 193.5 M | 222.1 M | 157 | 0 | 46 s |
| | **new** | **34.0 M** | **15.1 M** | **295** | **31** | 31 s |
| `r6400_acos_service` | old | 126.1 M | 154.4 M | 128 | 2 | 36 s |
| | **new** | **22.7 M** | **6.1 M** | **310** | **72** | 26 s |
| `ac15_netctrl` | old | 181.9 M | 216.9 M | 409 | 0 | 46 s |
| | **new** | **31.0 M** | **7.0 M** | **3790** | **2** | 37 s |

Full table with endpoints: `firmware-eval/run/head2head/results/TABLE.md`.
Every number above, plus a record per path, is in `results/aggregate.json`.

### Size

The old engine's import is **5.5–7×** larger on every binary (Souffle reads
text `.facts` files; the new engine writes a binary IR). The index gap is wider
still — **15×** on `rc`, **23×** on `arp_check`, **139×** on `nvram_daemon` — and
it widens as the binary gets smaller, which says the old engine's index carries a
fixed cost the new one does not. The old engine's index is a SQLite database
holding the full materialized relation set; the new engine's is a directory of
Parquet files.

### Summaries

The new engine derives more compositional summaries from the same program on
every binary — 2.0× to 2.6× on four of them, and **9.3×** on `netctrl` (3,790 vs
409), the largest binary in the set. This is the mechanism behind the path
counts: summaries are what carry taint across a call.

### SARIF paths

Paths were compared as `source → sink` endpoint pairs, the coarsest join that is
meaningful across engines:

| binary | pairs old | pairs new | both | old only | new only |
|---|--:|--:|--:|--:|--:|
| `r7000_arp_check` | 0 | 3 | 0 | 0 | 3 |
| `dlink878_nvram_daemon` | 0 | 0 | 0 | 0 | 0 |
| `r7000_rc` | 0 | 4 | 0 | 0 | 4 |
| `r6400_acos_service` | 1 | 4 | **1** | 0 | 3 |
| `ac15_netctrl` | 0 | 1 | 0 | 0 | 1 |

The one pair both engines report is `fgets → system` in `acos_service`. The new
engine additionally reports `acosNvramConfig_get → system`, `nvram_get → system`,
`getenv → system`, `recv → system`, `main → system`, and `fgets → doSystemCmd` —
in other words the NVRAM- and network-sourced flows, which is where router
command injection lives. **There is no binary on which the old engine reports
something the new engine misses.**

### Is the old engine simply misconfigured?

No — worth checking, because "0 paths" is exactly what a broken model set looks
like. `controls.py` runs both engines, with these same shared models, over
Operation Mango's synthetic test binaries:

| binary | old (souffle) | new (rs) |
|---|--:|--:|
| `nested` | **3** | 2 |
| `simple` | **6** | 4 |
| `heap` | 0 | 2 |
| `wrapper` | 0 | 2 |
| `off_shoot` | 0 | 3 |

The old engine binds the models, propagates taint, and reports *more* paths than
the new engine on `nested` and `simple`. It is working. It binds endpoints on the
firmware binaries too: a diagnostic run on `arp_check` (`--format summary`, plus
the `TaintSourceVertex` / `LeakingSinkVertex` / `CTADLStats` relations in
`ctadlir.db`) showed 12 source vertices — `acosNvramConfig_get`,
`acosNvramConfig_read`, `fgets` — and 2 `system` sink vertices, with 374 vertices
tainted across 4 functions. The taint simply never arrives at the sink's
argument. Neither `--star`, `--dynamic-access-paths-max-length 3`,
`--compute-slices all`, expanding `Argument(*)` to explicit indices, nor dropping
`.*` from the ports changed that. Per the experiment's instructions this was left
alone rather than tuned around; it comes out in the path counts above.

---

## Reproducing this

```sh
# 1. the environment: both engines, one Ghidra
nix develop .#head2head

cd firmware-eval/run/head2head

# 2. regenerate the shared model set (already checked in; this proves it)
python3 models/build_models.py

# 3. pin tool paths so a campaign does not re-evaluate the flake per job
#    (env.sh is generated from inside the devShell; it is checked in)
source env.sh

# 4. the campaign: 5 binaries x 2 engines x 3 phases, sequential, ~6 min
python3 campaign.py                 # --force to redo completed jobs

# 5. the configuration control
python3 controls.py

# 6. aggregate -> results/aggregate.json + results/TABLE.md
python3 analyze.py

# 7. the graph -> results/head2head.png and results/head2head-dark.png
python3 plot.py
```

Jobs are run one at a time on purpose: wall time and peak memory are recorded per
phase, and concurrent jobs on a shared machine would make both meaningless.
`campaign.py` is resumable — a job with an existing `results/<label>/<engine>.json`
is skipped.

### The graph

`plot.py` draws three panels, old beside new for each binary:

1. **on-disk footprint** — a *stacked* bar, import size with index size on top, so
   the total cost of analyzing a binary is the bar height and the split is visible
   inside it;
2. **function summaries**;
3. **SARIF paths**.

Colour carries engine identity and nothing else (old blue, new orange, in every
panel); inside a stacked bar the two phases are separated by lightness within
that engine's own hue plus a surface-coloured gap. The palette passes the CVD and
contrast checks in both modes. `results/TABLE.md` is the table view of the same
numbers, and a dark-mode PNG is generated alongside the light one.

---

## Caveats

- **Five binaries.** Enough to be consistent, not enough to be a distribution.
  The direction is uniform across vendors, architectures, and a 17× size range,
  but the magnitudes are not population estimates.
- **Wall time is not a headline.** It is recorded (the new engine is faster on
  the three larger binaries, tied on the two small ones) but the phases include
  Ghidra, which dominates on small inputs and is identical for both.
- **Path counts are unranked and unverified.** Neither engine ranks its output
  and no manual verification was done, so "141 vs 2" is recall-shaped, not
  precision-shaped. What *is* verified is the containment: the old engine found
  nothing the new engine missed.
- **The model translation is a judgement call.** The `.deref` ↔ `.*` mapping and
  the `saturating` ↔ `all_fields` mapping are each engine's idiom for the same
  intent, not provably identical semantics. The control experiment is what makes
  the translation credible: under it, the old engine outperforms the new one on
  the direct cases.
- **Nothing is committed.** All of it is working-tree changes on
  `head-to-head-vs-ctadl-souffle-firmware`, marked DO-NOT-MERGE.
