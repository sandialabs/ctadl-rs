# Continuous benchmarking - DO-NOT-MERGE

A daemon that follows `origin/main` of `~/proj/ctadl-rs`, builds every commit, measures it,
and flags regressions and improvements in performance and in result quality. It is built to run
unattended on a shared workstation without getting in anyone's way.

```sh
launchd/install.sh                 # start the daemon and the web page (http://127.0.0.1:8787/)
launchd/install.sh --uninstall
./run.sh status                    # queue, running work, sweep progress, recent events
./run.sh report [SHA]              # what changed at a commit vs. the commits before it
./run.sh enqueue SHA...            # measure specific commits too
```

Everything it writes goes under `/Volumes/Shampoo/ctadl-bench` (`paths.root` in
`bench.toml`): `bench.db` (SQLite), `builds/<sha>/`, `work/<sha>/<suite>/<bench>/` (per-run
logs, `time -l` output, SARIF, scores), the cargo target dirs, and `tmp/` and `home/` for
scratch. Logs are kept; a benchmark's ctadl store is deleted as soon as its last phase is over.

## What runs

Per commit, a **pass** (`bench.toml` `[suites.*]`):

| suite | benchmarks | quality measured |
|---|---|---|
| taintbench | 38 TaintBench apps (APKs pinned by the flake) | ground-truth flows found, known false positives (`xtask taintbench-score`) |
| android-icc | 3 DroidBench ICC cases | flow present as expected |
| fdroid | newest version of each app in `~/apps/fdroid` (`suites/fdroid.json`) | reported paths, `ctadl report` numbers, with the `suites/android-privacy.json` model |
| firmware7 | 45 network-facing binaries of the 7 Mango devices (`suites/firmware7.json`) | cmdi findings, by source class |
| apps | newest version of each app in `~/apps` (`suites/apps.json`) | same as fdroid |

Each benchmark is import -> index -> query (plus `report` for apps), one measured unit per
phase. Besides counts, each query's set of reported paths/findings is stored as a digest, so a
change in *which* flows are reported is flagged even when the number holds still.

And continuously, a **firmware sweep** over every sink-bearing executable in the Operation
Mango dataset - 32,559 binaries (`suites/firmware-sweep.json`) - pinned to one commit, run in
parallel whenever no pass work is waiting, restarted at the newest commit when complete.

The JSON lists are frozen with the `suites/gen_*.py` scripts; regenerate on purpose.
`suites/android-privacy.json`, the fdroid and apps query model, is the framework-class rules of
the TaintBench app models (`gen_android_model.py`): those suites have no ground truth, so they
are queried for the same kinds of leak TaintBench scores.

## Staying out of the way

The activity gate (`ctbench/activity.py`) decides when the machine is in use. Keyboard idle
time is useless over ssh/mosh, so it reads the access time of every tty with a process attached
(any keystroke over any transport updates it), plus development processes not started by the
daemon, and foreign CPU use averaged over a minute. System daemons our own work wakes
(Spotlight, XProtect, `sysmond`; `[gate] ignore_cpu`) do not count as foreign.

* **p tier** (normal priority, performance cores) only after 15 minutes of quiet. Only p-tier runs record wall time, and only if never moved.
* **e tier**: `taskpolicy -b`, i.e. the efficiency cores and throttled I/O. Instruction
  counts, cycles and peak memory are recorded on either tier; instruction counts agree across
  tiers to well under 1%.
* User returns during p-tier work: every run started on the p tier (builds, imports and sweep
  units included) is killed and requeued, so the cores are free at once; e-tier work that was
  promoted is demoted back. After 3 such kills a unit runs on the e tier.
* Memory pressure: sweep workers are shed (youngest first); other work is paused.
* Commit passes and builds run alone; the sweep uses 12 workers when quiet, 3 otherwise.
* Every benchmark run is capped at 16 GiB by memguard (builds at 48 GiB); a run over the cap
  is killed and recorded as `oom`.

## Index memory budget

An index run may use 100 bytes of peak memory per byte of IR instructions
(`[budget] index_mem_per_ir_byte`). The IR's size is that of `imports/*/ir-program.bitcode`,
the import's encoded program, recorded as `ir_program_b` on both import and index runs. Each
index run stores `mem_budget_ratio` = peak footprint / (100 x IR bytes). Over 1 is a blowup: it
is logged as an `over-budget` event, reported as a regression when the benchmark was under budget
before (or new), and as `over-budget` while it stays over. An index run memguard killed counts at
least its peak, so its ratio is a lower bound (shown with `≥`). Sweep runs record the ratio too;
the sweep table counts the binaries over budget.

Builds run `cargo build --release` inside the commit's own `nix develop` shell, as the daemon's
child, so they move between tiers too; the shared target dir makes them incremental.

## Detecting changes (`ctbench/report.py`)

A commit's value is compared with the median of the previous five measured commits. Performance
metrics are flagged past a threshold (instructions 0.5%, wall time 5%, peak memory 3%) that is
also more than 3x the baseline's own spread; quality metrics and digests on any change; a unit
that used to succeed and now fails is a regression.

## Caveats

* `time -l` counts instructions for ctadl only. A pcode import's Ghidra JVM shows up in user
  and system time but not in the instruction count.
* The bench branch carries `taintbench`'s engine changes, but never measures them: the measured
  `ctadl` is always built from a clean checkout of the tracked commit. The bench branch's own
  build is used only for `xtask taintbench-score`.
