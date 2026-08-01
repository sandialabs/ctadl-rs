# CTADL vs Operation Mango — Large-Scale Evaluation (paper Table 6) - DO-NOT-MERGE

Compares CTADL's command-injection taint analysis against Operation Mango's
**Table 6 (Large Scale Evaluation)** over the full `large_dataset` firmware corpus.
**Comparison dimension: findings** (CTADL alerts vs Mango TruPoCs), not runtime.

## Corpus & population

Dataset: `operation-mango-public/firmware/large_dataset` (pre-extracted `fs/` rootfs
per image). Population reproduces Mango's `FirmwareFinder` selection (ELF, exclude
`file`→shared-object, skip symlinks + `busybox`, sha256-dedup) **gated to binaries
containing a cmdi sink** (Mango's `has_sinks` gate). Built by `scan_pop.py`.

| | count |
|---|--:|
| firmware images | 1,684 (paper Table 6: 1,698 — same corpus) |
| unique sink-bearing executables (global sha256-dedup) — **what CTADL runs** | **32,311** |
| firmware×binary sink-bearing instances (per-image attribution) | 63,523 |

Per-vendor population is in `pop.json`; the runnable list in `pop.json.worklist.json`;
the sha→image attribution map in `pop.json.attrib.json`.

## Pipeline

- `scan_pop.py`  — enumerate population + emit worklist + attribution (ELF-magic scan,
  skips `dev/proc/sys` and `.extracted` carvings; regular files only).
- `run_one.py`   — run CTADL on ONE binary (`ctadl go -l pcode --models cmdi-firmware.json5`,
  Ghidra 12.0.4 lift → Datalog taint). **Guards:** 600 s wall + 24 GB physical-footprint
  cap (`footprint -f bytes`, summed over the job's process group). Classifies
  `ok | no_findings | timeout | oom | crash` — timeout/oom mirror Mango's Error/OOM columns.
  Isolated per-job store, deleted after SARIF is parsed. Findings via the harness
  `normalize_ctadl.parse_sarif`.
- `campaign.py`  — resumable parallel controller (12 workers). Skips any sha with an
  existing `campaign/results/<sha>.json`, so it can be killed/restarted freely.
- `aggregate.py` — join results with the attribution map → Table-6-shaped per-vendor
  comparison (CTADL alerts / alert-bins vs Mango TruPoCs / Error / OOM). Safe to run
  mid-campaign for a live snapshot.

## Run

```sh
cd firmware-eval/run/large_scale
export GHIDRA_HOME=/nix/store/30m9yjgksz2971r3x1gmzjcigfj538bm-ghidra-12.0.4/lib/ghidra
NWORK=12 JOB_TIMEOUT=600 JOB_MEMCAP_GB=24 CAMPAIGN_TMP=/private/tmp \
  python3 campaign.py pop.json.worklist.json campaign

# live progress snapshot (any time):
python3 aggregate.py
```

## Comparability notes

- **CTADL is unranked; Mango's TruPoC = closures with rank ≥ 7.** So "CTADL alerts" is
  conceptually Mango's *raw* hit column, not TruPoCs — expect CTADL alerts ≫ TruPoCs
  where CTADL works. Same framing as the Table 3 comparison (`../TABLE3_COMPARISON.md`).
- **Do not compare the "Total binaries" column directly.** Mango's Table 6 total (770,374)
  counts its raw binwalk output including recursive `.extracted` carvings; this population
  is the clean, deduped, sink-gated set (32,311 unique). The comparable axis is *findings*.
- Model: `firmware-eval/models/cmdi-firmware.json5` (name-based sink/source matching for
  stripped firmware). NB: fixed a committed parse-breaking typo (`ate//` on line 44).

## Fixes made in this session

1. `cmdi-firmware.json5` line 44 `ate//` → `//` (was breaking JSON5 parse → 0 findings).
