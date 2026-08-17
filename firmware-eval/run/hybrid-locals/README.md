# hybrid-locals — is the custom `locals` / `assign_like` data structure worth it?

The experiment behind `../../../hybrid-locals-experiment.md`. Two builds of the same
commit, differing only in whether `locals` and `assign_like` use the hybrid
trie-of-`HybridSet` store (`#[ds(...)]`, commit `de45c4b0`) or Ascent's built-in
relation storage; 100 firmware binaries; index-phase wall time and peak physical
footprint.

| Path | What it is |
|---|---|
| `select_corpus.py` | Picks the 100 binaries mechanically. Its docstring is the selection rule |
| `corpus.json` | Generated. The 100 binaries with their prior-campaign cost profile |
| `pilot_corpus.json`, `pilot/` | The 15-binary calibration pilot: index-phase cost across the whole `go`-cost range. This is what told us `go` wall is mostly Ghidra lift, and where the index phase starts to matter |
| `stats_pass.py` | A separate **unmeasured** pass that re-indexes each substantive benchmark with debug logging, to record how many rows `locals` reaches. Into `runs/stats/` |
| `control.patch` | **The control condition, exactly.** `git apply` it to HEAD, rebuild, and you have `bin/ctadl-control` |
| `bin/ctadl-hybrid`, `bin/ctadl-control` | The two binaries actually measured (gitignored — rebuild from `control.patch`) |
| `run_one.py` | One binary, one condition: shared import, then a guarded, measured `ctadl index` |
| `campaign.py` | Runs the corpus, both conditions, sequentially, paired and order-alternating. Resumable |
| `analyze.py` | `runs/aggregate.json`, `runs/TABLE.md`, `runs/raw.csv` |
| `plot.py` | The four deck figures, light and dark, PNG + SVG, into `runs/figs/` |
| `runs/results/<cond>/<label>.json` | **Raw measurements**, one file per binary per condition, plus the run log |

## The two conditions

Control is HEAD with the two `#[ds(...)]` attributes deleted from the `ascent!`
program in `ctadl-ascent/src/index_engine/mod.rs`. That alone does not compile: the
code after the fixpoint reads the trie's own API (`FromRows::from_rows`,
`into_vec`, `heap_report`, `num_reached_variables`). `control.patch` carries those
four mechanical adaptations, and nothing else — see the comments in the patch. The
seeding one matters for fairness: `AssignTrie::from_rows` dedups on insert, so the
control dedups its seed too rather than starting the fixpoint from a different
relation.

## What is measured, and what is not

`ctadl go` on a small firmware binary is almost entirely the Ghidra lift. So the
lift is done **once per binary** into a pristine store, both conditions index a
fresh copy of it, and only `ctadl index` is inside the timed region. Store copy,
result fingerprinting and log parsing all happen outside it.

Memory is macOS **physical footprint** — never RSS, which undercounts badly because
macOS compresses cold pages. Peak comes from `/usr/bin/time -l`'s `peak memory
footprint` (the kernel's own high-water mark); wall comes from its `real`. A 1 s
`footprint -p` poll runs alongside purely to enforce the memory cap and to supply
the numbers if a job is killed.

## Equivalence

A storage change must not change results. After each measured run the harness
fingerprints the index the run produced: row count per relation from parquet
metadata, plus an order-independent sha256 over the sorted rows for relations under
the row cap. `analyze.py` compares the two conditions relation by relation and
reports PASS/FAIL. (Spot-checked separately with `RUST_LOG=…index_engine=debug`:
identical `locals`, `assign_like` and `summary` row counts.)

## Run it

```sh
cargo build --release --bin ctadl && cp target/release/ctadl <here>/bin/ctadl-hybrid
git apply firmware-eval/run/hybrid-locals/control.patch
cargo build --release --bin ctadl && cp target/release/ctadl <here>/bin/ctadl-control
git checkout ctadl-ascent/src/index_engine/mod.rs

cd firmware-eval/run/hybrid-locals
python3 select_corpus.py                    # regenerate corpus.json (checked in)
JOB_TIMEOUT=1200 JOB_MEMCAP_GB=48 python3 campaign.py
python3 analyze.py
python3 plot.py
```

`campaign.py` skips any job with an existing `runs/results/<cond>/<label>.json`, so
it can be interrupted and restarted; `--force` redoes them. `--only LABEL…` runs a
subset. Imports are cached in `runs/imports/` and shared by both conditions.
