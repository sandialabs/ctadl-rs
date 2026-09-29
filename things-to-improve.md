# Things to improve

## `locals` blowup on native code: `org.vi_server.androidudpbus` (2026-09-29)

This app does not index in 15 minutes. It hit the timeout at 88 GiB, against a budget of about
2.8 GB (100x its 28.2 MB of IR). Nearly all of the IR is `libudphub.so`, a 567 KB Rust/tokio
library. The Java half is trivial (231 call sites).

**Status:** with pruned SSA and mem2reg (`31594e08`), it indexes in 169 s at 15.0 GB, about 5x over
budget. The stack slots that mem2reg has to leave in memory are still the mixing pool; see
"After mem2reg: what's left".

### What makes `locals` big

- **Size and growth.** `locals` held 10.7 M rows after 20 s of fixpoint, 152 M after 80 s, and
  341 M after 160 s (9.9 GB for the store alone). Rule costs are normal, about 0.2 µs per tuple,
  so the problem is the number of tuples.
- **One function dominates.** `FUN_00144f04`, an async state machine with 803 blocks, holds
  86–92% of `locals`. Almost every variable in it is reached from about the same 1–3k sources:
  paths of formal 0 plus paths into global memory. So rows ≈ (variables in the function) ×
  (all sources).
- **Root cause: stack slots are not in SSA.** Every stack access goes through one SSA variable,
  `%L3_0`, the stack pointer at entry. The function has 55,775 stores into 255 slots and
  105,050 loads from 285 slots. A typical slot receives about 300 distinct values and is read by
  about 590 loads. Each load inherits every value ever stored to its slot, and loaded values are
  stored again into other slots. The stack becomes a function-wide mixing pool, so every
  variable in the function ends up reached from everything.

### Contributing: unpruned SSA

- `ctadl-ir/src/ssa/mod.rs` places phis with minimal Cytron SSA and no liveness pruning.
  `libudphub` gets 5.19 M phis, of which 1.1% are live. The phis take the statement count from
  880k to 5.96 M and the assign edges to 14.6 M.
- An experimental dead-phi pass cut the assign edges to 1.09 M. The index then finished, in
  875 s at 49.8 GB: still about 18x over budget. The stack-slot mixing remains.

### Fixes

1. **Pruned SSA: done** (`2b90a98b`). The index finishes in 849 s at 52.0 GB, about 18x over
   budget. The stack-slot mixing remains.
2. **Stack-slot promotion (mem2reg): done, `window:8` by default** (`31594e08`). The index
   finishes in 169 s at 15.0 GB, about 5x over budget. See the next two sections.

### Stack-slot promotion (mem2reg)

The pass is `ctadl-ir/src/ssa/mem2reg/`. It runs after coalescing and before SSA. It gives each
`__stack_top.[k].deref` slot its own local (`__stack_m16` for offset -16) and rewrites the slot's
loads and stores into copies, so SSA versions the slot like any other local.
`Pipeline::index_default` runs it with `EscapePolicy::Window(8)`. The policy measurements below
used the since-removed `CTADL_MEM2REG=exact|window:N|above` switch.

**Promotion keeps today's behavior except through escaped addresses.** The analysis matches
offsets exactly and adds adjacent offsets together, so distinct offsets are already distinct
locations; overlapping accesses of different widths behave as before. What promotion can lose is
a flow through an escaped frame address. An address `__stack_top.[j]` escapes when it is passed
to a call, stored, copied or returned; the escaped address plus an offset `d` reaches slot `j+d`.
The escape policy decides how far an escaped address is assumed to reach, and those slots stay
in memory:

- `exact`: only slot `j`.
- `window:N`: slots `j .. j+N`.
- `above`: every slot at or above `j`. This is the only policy that can't lose a flow, but it
  promotes almost nothing (0.1% of accesses), because every large function has an escape near
  the bottom of its frame.

**Ghidra can't bound an escaped region.** At the escaped offsets in `FUN_00144f04`, Ghidra's
high variables are 4–8 bytes (`local_5a0` is 8 bytes). It did not recover the Rust structs
behind them.

**Results on `androidudpbus`**, one run per policy:

| Escape policy | Stack accesses promoted (of 584,560) | Wall time | Peak memory |
|---|---|---|---|
| off (pruned SSA only) | 0 | 849 s | 52.0 GB |
| `exact` | 463,765 (79%) | 141 s | 16.8 GB |
| `window:8` | 414,236 (71%) | 163 s | 15.0 GB |
| `window:16` | 345,588 (59%) | 266 s | 21.4 GB |
| `window:64` | 171,216 (29%) | 406 s | 30.1 GB |

The promotion counts match the stack census's predictions exactly, and no function was skipped.
`window:8` peaking below `exact` is probably run-to-run variation.

**Results on the regression suite** (`pcode`, `jni` and `android-native`, 50 cases), with SARIF
output compared against the pass turned off:

- `window:8`, `window:16` and `window:64` lose nothing: the same results and the same flows. In
  the chess app, only the intermediate steps shown for a flow differ.
- `exact` loses real flows:
  - It loses the only flow in `nightly/tests/c/example.c`. `transfer(&x.b, y)` writes
    `out->d`, 4 bytes past the address passed in, and the sink never sees it.
  - It also drops one result in `structret`.
- **The suite hid the first loss.** It still reported "49 passed, 0 failed": `example` showed up
  as a SKIP ("no tainted instructions on Darwin; skipping strict offset check"). Fixed in
  `31594e08`: this is now a FAIL.

**Where escaped addresses go.** 1,714 frame addresses are passed to calls. Of those, 1,697 go to
functions defined in the library (132 of them to `memcpy`) and 17 through function pointers.
Calls are only 28% of all escapes, though; the rest are frame addresses stored to memory (3,118)
or copied (1,291) inside the function.

### Recommendations and status

1. **Use `window:8`: done** (`31594e08`). It replaces the `CTADL_MEM2REG` switch in
   `Pipeline::index_default`. The regression suite passes (50 of 50), and its SARIF matches the
   `window:8` run above.
2. **Make the suite fail on a lost flow: done** (`31594e08`). A pcode case with no
   source-to-sink flow is now a FAIL on every platform, not a SKIP on Darwin. This hasn't been
   seen catching the `example` loss: with the switch removed, reproducing it needs an `exact`
   build.
3. **Find where the remaining 15 GB goes: done.** See the next section. The stack slots that
   stay in memory are still the mixing pool.
4. **Bound escapes better: next.** The kept slots are the heavily used ones, and their
   addresses really do escape, so a tighter window may not free them. A better fit is to
   promote an escaped slot anyway and spill it around its escapes: store the local to the slot
   before a call or escape, and reload it after a call. Direct accesses between escapes would
   then be in SSA, and only the values live at an escape would mix. Not started.
5. **Measure `glxy` and `cpuinfo`: done.** `glxy` finishes (120 s, 21.6 GB) and shows the same
   pattern as `androidudpbus`. `cpuinfo` still times out, and the cause is not native code.

### After mem2reg: what's left (2026-09-29)

| App | Wall time | Peak memory | `locals` rows | `locals` store | Largest function's share |
|---|---|---|---|---|---|
| `androidudpbus` | 169 s | 15.0 GB | 340 M | 8.9 GB | `FUN_00144f04`, 72% |
| `glxy` (hit the 48 GB cap before) | 120 s | 21.6 GB | 425 M | 12.5 GB | `FUN_0010b408`, 97% |
| `cpuinfo` (timed out at 900 s before) | timed out at 1800 s | 40.8 GiB | n/a | n/a | n/a |

- **The kept stack slots are the heavy ones.** In `FUN_00144f04`, 23k stack accesses stay in
  memory across 35 slots; the busiest take about 300 stores and 580 loads each, the same profile
  as before promotion. Its `locals` rows come from about 2,560 paths of formal 0 and 120 global
  paths, and reach about 16k SSA temporaries. In `glxy`'s `FUN_0010b408`, three kept slots
  (-35104, -35080, -35072) take about 765 stores and 1,550 loads each.
- **`cpuinfo` is a Java-side problem.** Its native libraries are tiny (mem2reg touched about 5k
  accesses). Its index has 53,796 CHA call sites and 623,583 paths. There's no census for it,
  because the census is only written when the index completes.

### Data

All output is under `/Volumes/Shampoo/ct-bigapk/small/sweep-3c85988c/`:

- `prof/ladder.log`: time and peak memory for every run.
- `prof/rank-t*.txt`: rules ranked by time.
- `prof/ssa-census.txt`: phi counts per function.
- `prof/census-{base60,dp60}/census/*.tsv`: breakdowns of `locals` rows.
- `prof/stack-slots-FUN_00144f04.txt`: store and load counts per stack slot.

The mem2reg measurements are under `/Volumes/Shampoo/ct-bigapk/small/sweep-2b90a98b-m2r/`:

- `RESULTS.md`: a summary of the sweep.
- `ladder.log`: time and peak memory for every run.
- `udpbus-*/index.err`: the index log for each policy.
- `reg-*/`: the regression output and SARIF for each policy.
- `sarif-diff*.txt` (made by `sarif_diff.py`): result and flow differences from the run with
  the pass off.
- `escape-census.txt`: where escaped frame addresses go (from the
  `ctadl-import/examples/escape_census.rs` example).
- `uncommitted.diff`: the source the binary was built from.

The measurements with `window:8` as the default are under
`/Volumes/Shampoo/ct-bigapk/small/m2r-default-w8/`:

- `RESULTS.md`, `ladder.log`: a summary, and time and peak memory for every run.
- `reg/`, `sarif-diff.txt`: the regression output, and its comparison with the earlier runs.
- `runs/<pkg>/census/*.tsv`, `runs/<pkg>/index.err`: the `locals` breakdown and index log per app.
- `fn_probe-*.txt` (from the uncommitted `ctadl-import/examples/fn_probe.rs`, copied there): memory
  accesses, kept stack slots and calls for one function after the default pipeline.

Experiment switches:

- In commit `fd5130f4` (WIP): `CTADL_DEAD_PHIS=1` turns on the dead-phi pass, and
  `CTADL_LOCALS_CENSUS=<dir>` writes the `locals` breakdown.

On the older a979c371 binary, `com.kaeruct.glxy` hit the 48 GB cap and `com.kgurgul.cpuinfo` timed
out. With `31594e08`, `glxy` finishes and `cpuinfo` still times out; see "After mem2reg: what's
left".
