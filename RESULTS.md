# Index blowup: `usb_modeswitch` (firmware-sweep `0f99bc28e9897893`)

## Benchmark

- Binary: `operation-mango-public/.../asus/FW_WL_330N3G_91216/fs/bin/usb_modeswitch` (MIPS, 186
  functions, IR 1.83 MB). Model `firmware-eval/models/cmdi-firmware.json5`.
- Bench DB (commit `dfccbcab`): index `ok`, 64.7 s wall, peak 1.87 GB, `mem_budget_ratio` 10.2
  (budget = 100 × IR = 183 MB).
- Rerun on this branch: 11 s wall. Process memory goes from 47.5 MB on entering `ascent_run` to
  1785.5 MB when it returns, and drops to 679 MB once the program is freed.

## Summary

All of the growth is in one relation, `locals`: 32.3M rows, against 106k `assign_like` edges
and 186 functions. Memory grows linearly with it, at about 53 B per row of process memory at
every timeout step (8.1M rows: 471 MB; 14.7M: 800 MB; 20.6M: 1047 MB; 28.6M: 1507 MB). The
fixpoint converges: 160 iterations, but `locals` reaches its final size by iteration ~70
(9.6 s), and the remaining 90 iterations add fewer than 50k rows. These are distinct facts,
not redundant derivation: the rules that build `locals` have duplicate ratios of 2–2.6
(`ext_dst` side) and 52 (`edge_split` side, which yields only 0.2M rows).

The rows come from the **global heap pseudo-formal** (`GLOBALS_INDEX` = `-32768`), which holds
99.8% of all rows. Two causes multiply:

1. **Every call site holds a full copy of the function's heap: 86% of rows.** Codegen passes
   the heap to every call as `actual_param(site, GLOBALS_INDEX, formal(GLOBALS_INDEX))`
   (`codegen/mod.rs`, "pass globals"). The engine turns each actual parameter into two
   `assign_like` edges with empty paths, one in each direction. So `call-arg(site, -32768)`,
   `formal(-32768)` and `local($globals_0)` form an empty-path copy cycle, and each member
   holds exactly the same `(p, formal, fp)` set; I diffed them, and they are identical.
   `main` has 96 call sites, each carrying the heap's 276,927 rows: 26.6M rows, 82% of the
   whole relation, in one function. Across all functions, call-site heap copies hold 27.7M of
   the 32.3M rows.

2. **The heap's own reachability is nearly dense: ~4.3M rows, and the size of each copy
   above.** In `main`, the heap has 1,349 cell paths and 1,019 source paths (718 distinct
   global addresses). Most cells are reached from 300–600 sources: 276,927 pairs, of which
   99.6% join unrelated cells. This comes from flow-insensitive mixing through hub globals:
   - In `readConfigFile`, the hub global `.[0x452578]` is written from ~46 different `.data`
     config cells (`.[0x10000xxx]`, one per `ReadParseParam` call; there are 46) and read
     back into locals that are stored into other cells. Within a function there is no ordering,
     so every read of the hub sees every write, and each destination cell gets 24–27 sources.
   - The effects from all callees are applied at call-site heap vertices, which (cause 1) alias
     the heap formal. So within the caller, each callee's global effect applies before and
     after every other call. `readArguments`' summary (which inlines `readConfigFile`'s)
     contributes 10,943 cell-to-cell edges to `main`'s heap. Of these, 8,448 lie on cycles
     among cells: the cell graph's largest SCC has 223 cells, and the next ones 101, 61, 43
     and 42. Closing over those cycles is what makes the heap dense.
   - The 10.5k ordinary locals of `main` that load from heap cells inherit each cell's
     200–600 sources: 3.8M rows.

Minor contributors:

- `main` has no callers but still derives a 276k-row global summary (its whole heap closure),
  most of `summary`'s 300k rows. Priced at ~39 MB with its two indices.
- Every external libc stub (`strtol`, `strtok`, `strstr`, …) and `ReadParseParam` carry the
  same three G→G summary rows over `.[0x450BD0]` / `.[0x10000010]` / `.[0x450BD0].[4]`. They
  are instantiated at every call (92 sites in `main` alone), but they are cheap.

## Evidence

### Where the time and rows go (full run)

SCC 5 (the reachability closure) takes 9.5 s of the run, over 160 iterations.

| rule | produced | new | dup | time |
|---|---|---|---|---|
| `locals <-- ext_dst total, locals delta` | 56.0M | 27.6M | 2.03 | 4.14 s |
| `locals <-- edge_split total, locals delta` | 11.6M | 0.22M | 52.5 | 1.44 s |
| `locals <-- ext_dst delta, locals total+delta` | 11.6M | 4.5M | 2.58 | 0.72 s |
| `reach_vp <-- locals delta` | 32.3M | 0.25M | 129 | 0.48 s |
| `summary <-- locals delta, …, alias_of_formal` | 25k | 12k | 2.1 | 0.28 s |

Relation sizes: `locals` 32.3M, `ext_dst` 515k, `locals_key` 733k, `summary` 300k,
`reach_vp` 250k, `edge_split` 231k, `assign_like` 106k (store) / 35k (Ascent rows).

### Storage

- The `locals` trie store is estimated at 894 MB (29 B/row): 35,434 `(F,V)` groups, 249,694
  `(F,V,P)` entries, and 32.3M leaves. The largest group has 276,927 leaves, one per heap
  vertex in `main`.
- Every Ascent-side index together (`[indexsizes]`, priced): 112 MB. The biggest are
  `summary`'s three fields (39 MB), `reach_vp` (26 MB) and `locals_key_wild` (27 MB). No
  Ascent index is large enough to be worth dropping. The trie stores (`locals`,
  `locals_key`, `ext_dst`, `edge_split`, `assign_like`) are not priced.
- Accounting for the 1785 MB: the `locals` trie at ~894 MB, priced indices at 112 MB, and the
  baseline at ~47 MB. The remaining ~730 MB is the unpriced stores (`ext_dst`, `locals_key`,
  `edge_split`) plus allocator slack and the per-iteration delta/new buffers. All of it scales
  with `locals`.

### Growth over iterations (`CTADL_ASCENT_TRACE=1`)

`assign_like` stops growing at ~34k by iteration 20. `locals` then grows in bursts, each
following a jump in `summary` (iteration 13: +0.66M; iteration 25: +3.6M; iterations 26–50:
0.5–1.4M each), as callee global summaries reach `main` and are copied to all 96 call sites.
`summary` reaches 300k by iteration ~60, and `locals` stops growing soon after.

### Attribution (`CTADL_INDEX_DUMP`)

| | rows |
|---|---|
| formal `-32768` (global heap) | 32,277,779 (99.8%) |
| function `main` | 30,947,624 (95.7%) |
| `readConfigFile` | 878,074 |
| `readArguments` | 350,400 |
| on `call-arg(*, -32768)` vertices, all functions | 27,686,860 (85.6%) |
| on `formal(-32768)` / `local($globals_0)` | 301,948 / 301,896 |
| everything else | 4,041,773 |

## What would fix it

1. **Collapse empty-path copy cycles** (or don't make the heap actual parameter a two-way
   alias). An offline estimate on the dump: merging each SCC of empty-path `assign_like`
   edges into one vertex removes 28.0M of 32.3M rows (86.6%), and 99.9% of the saving is in
   heap classes. That leaves 4.32M rows, roughly 230 MB at the observed 53 B/row, or ~1.25× the
   budget instead of ~10×. The narrowest form of the fix: in codegen, or in the
   actual-parameter rule, map `call-arg(site, GLOBALS_INDEX)` to `formal(GLOBALS_INDEX)`
   itself, instead of creating a fresh variable joined to it in both directions. Callee
   global effects then apply directly to the heap formal, which already happens through the
   copy cycle, so the result should be unchanged.
2. **The dense heap** (the remaining 4.3M rows) comes from flow-insensitive heap mixing
   through hub cells and cell cycles. Fixing that is a precision change (some flow-sensitivity
   or SSA for global cells), not a storage change.
3. Optionally, skip deriving global summaries for functions with no callers (`main`: 276k
   rows), if nothing downstream reads them.

## Experiment: change only `call_arg!` (2026-10-21)

The index engine's `call_arg!` now returns `formal(GLOBALS_INDEX)` for the global heap
slot and constructs the original per-site vertex for every other slot. No codegen,
propagation, query, or summary rules were changed. In particular, codegen already passes
the heap formal; it is the actual-parameter rule's use of `call_arg!` that creates the copy.

### Benchmark impact

Compared the preserved pre-change release binary with a release build of the macro-only
change, using the benchmark database's index argv and separate stores copied from the same
existing import. Both use serial in-memory indexing and the same firmware model. The table
reports medians of three alternating before/after runs; timings include ordinary shared-machine
load. Peak footprint is `/usr/bin/time -l`'s `peak memory footprint`, not sampled RSS.

| Metric | Before | Macro only |
|---|---:|---:|
| Index command wall time | 11.36 s | 2.46 s |
| Fixpoint time | 10.934 s | 2.092 s |
| Peak physical footprint | 1,865,664,672 B | 354,878,352 B |
| Footprint when `ascent_run` returns | 1779.1 MiB | 334.2 MiB |
| Reachability SCC iterations | 160 | 80 |
| `locals` | 32,332,477 | 4,645,617 |
| `locals_key` | 733,392 | 75,132 |
| `reach_vp` | 249,694 | 49,574 |
| `ext_dst` | 515,220 | 116,793 |
| `assign_like` | 106,130 | 98,907 |
| `summary` (stored rows) | 300,003 | 300,003 |

This removes exactly 27,686,860 `locals` rows (85.6%), reduces peak footprint by 81.0%,
and makes the index command 4.62 times faster. It leaves more rows than the general SCC
estimate because it does not merge the heap formal with `$globals_0` or other copy classes.
The measured peak is still about 1.9 times the 183 MB budget, rather than the estimate's
roughly 1.25 times.

For this benchmark, sorting and comparing the full `locals` dumps shows that the new relation
is **exactly** the old relation with all `call-arg(*, -32768)` rows removed: no surviving
vertex gains or loses a fact. Set comparisons of saved Parquet facts also find identical
`summary`, `call`, `resolved_call`, `call_target_assign`, `actual_param`, and `formal_param`
relations. There are 299,963 distinct summary facts in both runs (300,003 stored rows).
This establishes the observed benchmark equivalence, not equivalence for all programs.

### Test results and semantic limitation

`nix develop --command ... cargo test --workspace --no-fail-fast` completed with
1,258 tests passing, two failing, and four ignored. The failures are both in `ctadl-c`:

- `an_argument_of_a_call_through_a_global_field_reaches_the_callee_parameter`
- `an_argument_of_a_statement_expression_callee_reaches_the_callee_parameter`

Both lose the expected `wrap` summary from argument 1 to the global `.taken` cell.
Running the C examples extracted from those tests through both release binaries confirms
that the baseline produces this summary and the macro-only binary does not.

The cause is the local indirect-call bypass rule in `index_engine/mod.rs`: it constructs
`FlowVariableKind::CallArg` directly instead of using `call_arg!`. Its global summary effects
therefore still land on per-site heap vertices, while the actual-parameter rule now connects
the actual heap only to its formal. Those effects are disconnected. A consistent implementation
must also canonicalize that rule's global endpoints; the macro-only change is not a passing
or semantics-preserving fix for the whole analyzer.

The full regression suite completed with **240 passed, four failed, two skipped, and two
expected failures**, out of 248 cases. The four failures are:

- `C:globalflow`
- `C:globalstruct`
- `CallResultFieldStore` (Dex)
- `Jvm:CallResultFieldStore`

All four report that no flow connects source to sink. Re-running these four cases with
`ctadl-before` passes all four, confirming that they are introduced by this change.
They expose a second integration issue beyond the indirect-call bypass: the query engine
still implements call/return transitions using per-site global `CallArg` vertices.
For example, `query_engine/search.rs`'s formal-to-actual step returns from the callee's heap
formal to `call-arg(site, GLOBALS_INDEX)`, which no longer has the symmetric actual-parameter
edges connecting it to the caller's heap. Preserving index summaries therefore does not
by itself preserve end-to-end query results. A complete fix must also handle these query
boundaries consistently, while retaining their call-site labels.

The initial `cargo xtask regression --release` invocation used a debug harness and was
stopped during the expensive CIL corpus sweep. The completed run used an optimized harness
in the same Nix environment:

```sh
export CARGO_TARGET_DIR=/Volumes/Shampoo/cargo-target/ct-index-instrument
export TMPDIR=/Volumes/Shampoo/ct-index-instrument/call-arg-globals/tmp
export CTADL_BIN=/Volumes/Shampoo/ct-index-instrument/call-arg-globals/ctadl-macro-only
cargo run --release -p xtask -- regression --release
```

`rustfmt --check --edition 2024 --config skip_children=true
ctadl-ascent/src/index_engine/mod.rs` and `git diff --check` pass.
`cargo fmt --all -- --check` reports existing formatting differences in the unchanged
`index_engine/instrument.rs`; no unrelated formatting changes were made. The code change
remains confined to `call_arg!`; the failing tests have not been weakened or skipped.

Experiment artifacts are under `/Volumes/Shampoo/ct-index-instrument/call-arg-globals/`:
`benchmark.json` and `paired-*.log` / `paired-*.profile.json`, `tests-all.log`,
`regression.log`, `regression-before-{globals,field-store}.log`, `after-dump/`,
`locals-comparison.json`, `saved-facts-comparison.json`,
and the two extracted C examples with their before/after dumps. `benchmark.py` records the
exact measured commands; `ctadl-before` and `ctadl-macro-only` preserve the compared binaries.

## Reproducing

Working data is in `/Volumes/Shampoo/tmp/inv-0f99`: `full.log`, `t{2,4,6,8}.log`, `trace.log`,
`dump/`. The import and index argv come from `units.spec` for bench `0f99bc28e9897893`. The
copy-cycle estimate was computed with a short Python script: Tarjan SCC over empty-path
`assign_like` edges per function, then the sum of `locals` rows on every member except the
largest one in each SCC.
