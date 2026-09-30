# Things to improve - DO-NOT-MERGE

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
   pattern as `androidudpbus`. `cpuinfo` still times out, and the cause is not native code; see "`cpuinfo` blowup". With
   `a7d716e8` it finishes in 82 s at 12.4 GB.

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
  accesses). Its index has 53,796 CHA call sites and 623,583 paths. The cause is R8-merged
  Kotlin lambdas; see "`cpuinfo` blowup". Splitting them at import (`a7d716e8`) fixes it.

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
left". With `a7d716e8`, `cpuinfo` finishes too; see "`cpuinfo` blowup".

## `cpuinfo` blowup: R8-merged Kotlin lambdas (2026-09-30)

`com.kgurgul.cpuinfo` does not index in 30 minutes. It is not a memory blowup (28.9 GB at 640 s)
but a fixpoint that never converges: after 640 s, scc 4 is still adding rows and minting
decisions, and every iteration costs more per new row than the last.

**Status:** fixed. Splitting the merged classes at import (recommendation 2, `a7d716e8`) makes the
index converge. With the keyed call-target rule (recommendation 1, `76982fc4`) as well, it takes
82 s and peaks at 12.4 GB. See "Results".

### Where the time goes

Timeout ladder (10 to 640 s, `CTADL_INDEX_TIMEOUT_SECS`, default `HybridContext::Decision`):

| Rung | Iterations | Peak | `locals` | `context_locals` | `call_target_assign_like` | Decisions |
|---|---|---|---|---|---|---|
| 10 s | 41 | 2.5 GB | 3.1 M | 0.14 M | 1.1 M | 5.8 k |
| 40 s | 95 | 6.9 GB | 15.2 M | 4.3 M | 5.8 M | 10.1 k |
| 160 s | 118 | 16.0 GB | 34.9 M | 20.0 M | 7.8 M | 16.2 k |
| 640 s | 127 | 28.9 GB | 56.9 M | 39.0 M | 8.7 M | 18.6 k |

Rule costs per tuple look normal, so the signal is the marginal cost, rule time per new row
between rungs. Two relations blow up:

- **`context_locals`**: 1.2 µs per new row at 10–20 s, 26.8 µs at 320–640 s. At 640 s its rules
  are 64% of rule time. It is a lattice relation, and its decision sets grow one decision at a
  time: 221 M unions grew a set for 39 M rows, about 6 updates per row, and every update sends
  the row through the whole contextual closure again. `set_establishes_via` grows the same way
  (0.8 M, 3.8 M, 13.1 M at 160, 320, 640 s), since every grown set is unfolded again in full.
- **`call_target_assign_like`**: 0.9 µs per new row, rising to 87.7 µs. Its transitive rule joins
  on `(f, v2)` and only then tests `substitute_prefix`. At 160 s it visited 2.04 B pairs; 1.6%
  matched the prefix and 0.9% were program paths. This is the wasted join that the `locals`
  rules' split keys were built to avoid (see the comment above `reach_vp`).

### The cause: merged lambda classes are dispatch hubs

The functions that hold the most `context_locals` rows are all R8-merged Kotlin suspend lambdas:
`Li;->r` and `Li;->e`, `Lp8;->r`, `Lk;->r`, `Lt40;->r`. (`nv2` is `SuspendLambda`, `vk` is
`BaseContinuationImpl`, `yp0` and `up0` are `Function2` and `Function1`.)

- **Many lambdas share one method.** `Li;` merges 29 lambdas behind a synthetic `int i` class
  id: `r` (`invokeSuspend`) and `e` (`invoke`) switch on it, and the captured state of all 29 is
  in the untyped `Object` fields `k` and `l`. The analysis does not see the switch, so `r` is the
  union of all 29 bodies, and the virtual calls in it, on casts of `this.l`, dispatch on
  whatever any of the 62 construction sites stored there. At 320 s `Li;->r` had 1,734 decisions,
  7,165 distinct decision sets, a largest set of 318 and 726 M memberships.
- **The union is instantiated 30 times over.** Each of `Li;->e`'s 30 switch cases calls
  `this.p(..).r(Unit)`, so `r`'s union summary lands at 30 identical call sites. Each site's
  receiver vertex holds 3,129 call-target tags and has 20,181 out-edges: 63 M pairs per vertex,
  1.9 B of the 3.2 B pairs that a full re-derivation of `call_target_assign_like` would visit.
- **75 `SuspendLambda` subclasses carry a class id**, so the pattern recurs across the app.

### Hybrid context mode is not the fix

Both other modes also fail to finish on their own, just differently:

- `none` timed out at 1200 s (31.5 GB). With no context, every caller gets the union of the
  resolved callees' summaries: `assign_like` reached 59 M rows (105x its input), `summary`
  10.2 M, `locals` 217 M. `call_target_assign_like` alone took 45% of rule time.
- `collapse` hit a 30 GiB cap at 822 s, about the same memory trajectory as `decision`. Profiled
  alone at 600 s (27.2 GB), it removes the churn (9.3 M set-growing unions, against 221 M) but
  not the volume: `context_locals` reached 55 M rows, nearly all of them ⊤, in the same hub
  functions (`Li;->e` 10.4 M, `Li;->r` 7.9 M, `Lk;->r` 6.2 M). A ⊤ summary is applied at every
  caller that establishes any decision, so decisions doubled, to 38.8 k.
  `call_target_assign_like` was again the most expensive rule, at 24% of rule time.

### Recommendations

1. **Key the `call_target_assign_like` transitive rule on the path prefix: done** (`76982fc4`).
   Each tag's path is split once at every prefix (`cta_key`, `cta_key_wild`), so the join only
   retrieves edges whose source path is that prefix. The step emits the new tag's exact keys
   itself. A first version derived them in a separate rule, which cost an extra iteration per
   edge and fell behind the baseline. On its own this doesn't make `cpuinfo` converge, because
   `context_locals` dominates.
2. **Handle R8 class-merged lambdas at import: done** (`a7d716e8`). `ctadl-dex/src/merged.rs`
   finds a `final int` field that each constructor stores from an argument or a constant, and
   that methods load from `this` and switch on. It uses a must-analysis of register values, so
   `move-object v4, p0` and a constant id in the constructor are handled. Each id gets a
   subclass `C$r8id<k>` with clones of the switching methods, each switch narrowed to arm `k`.
   Construction sites with a known id allocate the subclass. The `synthetic` flag is not
   required, since `final` is what makes the split sound. This fixes `cpuinfo`.
3. **Bound decision-set churn.** Not needed for `cpuinfo` now: with the split it mints 7.2 k
   decisions and `context_locals` holds 1.1 M rows. Not measured.

### Results (2026-09-30)

Each configuration was measured on a fresh import made with its own binary. `rec2` alone is
`1a5de8c2` (branch `r8-merged-lambdas`, the baseline plus recommendation 2).

| Build | Result | Wall time | Peak memory | Iterations | `locals` | `context_locals` | Join pairs (transitive rule) |
|---|---|---|---|---|---|---|---|
| baseline `62ffedde` | cut off at 1800 s | 1894 s | 41.5 GB | 134 | 74.6 M | 58.0 M | 18.6 B (0.59% prefix matches) |
| rec 1 `76982fc4` | cut off at 1800 s | 1895 s | 50.4 GB | 137 | 84.2 M | 68.2 M | 122 M |
| rec 2 `1a5de8c2` | fixpoint | 116–119 s | 7.6–7.8 GB | 1,316 | 27.0 M | 1.08 M | 581 M (42% prefix matches) |
| both `a7d716e8` | fixpoint | 82–85 s | 12.4 GB | 1,316 | 27.0 M | 1.08 M | 246 M |

- **Rec 1 does its job but is not enough.**
  - At 160 s it visits 30.7 M pairs instead of 1.69 B. The call-target rules take 17.6 s instead
    of 24.4 s, and the run gets further in the same time: 117 iterations against 115, and 32.6 M
    `locals` rows against 29.0 M.
  - At 1800 s the call-target rules take 32 s, down from 210 s, but `context_locals` still takes
    1,433 s. That is the churn rec 2 removes.
- **Rec 2 removes the cause.**
  - 620 classes split into 3,562 ids, 1,068 methods are cloned per id, and 4,296 construction
    sites are retagged.
  - Decisions fall from 22 k (still growing) to 7.2 k, and `context_locals` from 58 M rows (still
    growing) to 1.1 M.
  - Before pruning, the Java IR grows from 598 k to 2.2 M assignments, because each clone carries
    every arm until SSA prunes the unreachable ones. The import time is unchanged at 37–39 s.
- **Together, rec 1 cuts rec 2's time by 30%.**
  - With the split, the unkeyed call-target rule is 46% of the fixpoint's rule time: 43 s of
    92 s. Keyed, it takes 15 s of 58 s.
  - It costs memory: `cta_key` holds 15.4 M rows, and the peak rises from 7.6 GB to 12.4 GB.
    Moving `cta_key` into a BYODS store the way `locals_key` is held is the obvious next step.
- **Neither change loses a result.**
  - The regression suite's SARIF is identical across all four builds, all 324 files.
  - `ClassIdMergedFlow`, new with rec 2, has the merged shape with five ids, more than CHA
    resolves statically. Under dex, the baseline reports a false positive at line 30 and rec 2
    reports only the two real flows. The jvm frontend shares the config and has no split, so the
    false positive is not asserted.
  - Rec 2 re-pins the xtask apk report counts, because `com.noto` has 137 merged classes, and it
    bumps `IMPORT_FORMAT_VERSION` to 10.
- **Two sources of nondeterminism turned up while checking that rec 1 leaves results alone.**
  - The decision-set relations (`set_*`, `context_summary_set`) differ between two runs of the
    same binary.
  - The pcode import is not deterministic. One run's import of `FUN_00105b18` in
    `libcpuinfo-libs.so` had one assignment and two locals in a different order, which changed
    6 `assign_like` edges. All 25,250 Java functions have identical edge counts in rec 2 and
    both.

### Beyond `cpuinfo` (2026-09-30)

I imported and indexed the Dex half of every APK in the corpus. The results are in
`/Volumes/Shampoo/ct-bigapk/small/r8-general/RESULTS.md`.

- **Merged classes are common.** 12 of 15 apps have at least one. `ceno` has 986 classes split
  into 5,812 ids, `greenbits` has 809 into 6,979, `cpuinfo` 620, `chess` 173, `pincredible`
  150 and `komodo` 83.
- **Rec 2 generalizes where merged classes are the hub.**
  - `ie.equalit.ceno` goes from hitting the 28 GiB cap at 599 s to finishing in 110 s at
    13.9 GB. `cpuinfo`'s Dex half goes from the cap at 819 s to 96 s.
  - Small apps are unchanged.
  - On `darkcoin`, which has 8 merged classes, rec 2 alone takes 169 s at 17.4 GB against the
    baseline's 179 s at 18.0 GB.
- **Rec 1 does not generalize as written.** It pays off only where the unkeyed join is mostly
  wasted, as on `cpuinfo`, where 0.6% of pairs matched the prefix.
  - On `darkcoin` 34% matched, so rec 1 saves no time. `cta_key` adds 44 M rows, and the peak
    goes from 18.0 GB to 27.2 GB.
  - On `greenbits` it cuts the pairs from 1.7 B to 90 M. But the keyed rule is slower (31 s
    against 22 s at 240 s), and it adds 8 GB.
  - It needs a compact `cta_key` store, like `locals_key`'s BYODS trie, or a switch to the keyed
    join only at high-fan-out vertices. Until then it is a net loss outside `cpuinfo`.
- **`greenbits` blows up for another reason.** It hits a 55 GiB cap under both builds, with
  87 k decisions at 240 s, and the split raises that to 110 k. Its `context_locals` hubs are
  Jackson databind's generic serializers, with up to 2 k decisions per function: for example
  `ObjectMapper._convert`, `DefaultSerializerProvider.serializeValue` and
  `ObjectWriter$Prefetch.serialize`. The cause is not Jackson, and bounding decision churn is
  not the fix; see "`greenbits` blowup".

### Data

All output is under `/Volumes/Shampoo/ct-bigapk/small/cpuinfo-blowup/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `runs/t<N>/`: the ladder rungs; `index.err` has the rule times, relation sizes and the
  `context_locals by function` census; `rank.txt` is from `rank.py`.
- `marginal.txt` (from `marginal.py`): rule time, rows and marginal cost per head relation.
- `runs/probe-t160/`: the `call_target_assign_like` join counters and fan-out census, from the
  binary `ctadl-probe` built with `probe.diff` (uncommitted).
- `runs/hc-none/`, `runs/hc-collapse*/`: the hybrid context A/B.
- `smali/out/`: the app's baksmali output (`i.smali` is the merged lambda class `Li;`).

The measurements of recommendations 1 and 2 are under
`/Volumes/Shampoo/ct-bigapk/small/cpuinfo-recs/`:

- `RESULTS.md`, `ladder.log`: a summary, and time and peak memory for every run.
- `bin/`: every binary measured, with its commit or diff.
- `runs/<build>/import/`: the import each build made. `runs/<build>/t<N>/`: the index runs.
  `index.err` is the full debug log, and `rank.txt` comes from `../cpuinfo-blowup/rank.py`.
- `reg-<build>/`, `sarif-diff-*.txt` (made by `sarif_diff.py`): the regression suite for each
  build, and their comparisons.
- `case-{base,rec2}/`: the `ClassIdMergedFlow` case under each binary.
- `runs/{rec2,both}/t1800/index-graph.dot`, `dot_edges.py`, `index-graph-perfn-rec2-both.txt`:
  the index graphs and their comparison.
- `repin/`: how the apk report counts were re-pinned.

## `greenbits` blowup: reused parameter registers written back to the formals (2026-09-30)

`com.greenaddress.greenbits_android_wallet` (Dex only, 215 k functions after the R8 split) does not
index. With `92fac26b` (recommendations 1 and 2 of "`cpuinfo` blowup") it hits a 55 GiB guard at
440 s, about 400 s into the fixpoint. No fixpoint in sight: 97 iterations at 320 s, against the
1,316 `cpuinfo` needs.

**Status:** fixed. SSA now writes back each parameter's entry version for JVM bytecode imports,
and the index reaches a fixpoint in 399 iterations at 12.4 GB. See "The cause" and "Results".

### The cause

R8 reuses parameter registers, and SSA's exit param-flow wrote each parameter's *exit* version
back to its formal. `UTF8JsonGenerator.writeBinary(Base64Variant, byte[], int, int)` ends with

```smali
    iget-object p1, p0, ...->_outputBuffer:[B   # p1, the Base64Variant, := this._outputBuffer
    iget-byte   p0, p0, ...->_quoteChar:B       # p0, this, := this._quoteChar
    aput-byte   p0, p1, p2
    return-void
```

so the write-back `formal(0) <- @p0_2 <- @p0_1 <- @p0_0._quoteChar` gave the summaries
`this <- this._quoteChar` and `variant <- this._outputBuffer`. A caller cannot see its argument
rebound, so both are impossible. They produced call-target decisions like this:

1. `JsonGenerator.writeBinary(byte[], int, int)` calls `this.writeBinary(variant, data, off,
   len)`, which resolves to `UTF8JsonGenerator`, `WriterBasedJsonGenerator` and `TokenBuffer`.
   The UTF8 summary lands on the call-arg vertex as `arg0 <- arg0._quoteChar`, and the
   formal-side `locals` rule gives `locals(arg0, ε, 0, ._quoteChar)`.
2. `TokenBuffer.writeBinary` has a genuine `critical_summary` at `(0, ε)`, since its `this`
   reaches virtual calls. Rule 1.2 joins it with the `locals` row from step 1, at the same
   call-arg, and derives `critical_summary(JsonGenerator.writeBinary, 0, ._quoteChar)`.
3. Rule 1.2 carries it up through `ByteArraySerializer`, `_serialize` and
   `ObjectWriter$Prefetch.serialize`, and rules 2.1/2.2 mint one decision per serializer class
   that reaches the byte.

Traced with the probe's `CTADL_FOCUS` dump (see "Data"). The analysis below was written before
the cause was known, and attributes these rows to value flow:

- `locals` and `assign_like` are value-flow relations: a value computed from `x` counts as
  coming from `x`.
- Rule 1.1 asks `locals` which formal paths reach the receiver of a critical call. In
  `DefaultSerializerProvider._serialize` the only critical site is `p3.serialize(..)`, but
  `critical_summary` also lists `arg1`, the value being serialized, since Jackson looks the
  serializer up by `value.getClass()`. In `ObjectWriter$Prefetch.serialize` the critical paths
  include `gen._quoteChar` (a `byte`), `gen._outputTail` (an `int`), `HEX_BYTES_UPPER[]` and a
  `$SwitchMap` `int[]`.
- `call_target_assign_like` walks `assign_like`, so object classes reach those paths. A decision
  is minted per (formal path, class). `Prefetch.serialize` gets about 23 paths x 37 serializer
  classes = 847 decisions, each with its own conditional summary, and each row's decision set
  grows one member at a time.

**Most decisions are provably impossible.** A probe dumped every decision, and each was checked
against the class hierarchy in the app's smali (dex formals are registers, so `J` and `D` take two):

| Rung | Decisions | Class not a subtype of the declared type | Object in a primitive slot | Compatible | Undecidable | `context_locals` memberships held by impossible decisions |
|---|---|---|---|---|---|---|
| 40 s | 44 k | 40.9% | 0.2% | 55.0% | 3.9% | 10.5% |
| 80 s | 178 k | 59.5% | 6.0% | 30.1% | 4.3% | 17.9% |
| 160 s | 448 k | 57.7% | 6.7% | 17.6% | 18.1% | 37.8% |
| 320 s | 585 k | 53.5% | 5.5% | 16.8% | 24.1% | 50.3% |

Decisions here are `resolvent` rows, one per (function, formal.path, target). "Undecidable" is
`Object`-typed or framework-typed. Examples: a `JsonGenerator` formal "holds"
`StdDelegatingSerializer`, `ObjectMapper` "holds" `BeanDeserializer`, and a `byte[]` element holds a
serializer. The worst functions aren't Jackson: Compose's `SpanStyleKt.fastMerge` (28.5 k of 29.8 k
decisions impossible, objects in `long` slots), `TextStyle.merge`, and kotlinx `JobSupport`.

**It is not the context machinery.** At 240 s, `--hybrid-context none` is worse than `decision`:
85 M `locals` rows against 53 M, 27 M `assign_like` against 21 M, 39.7 GB against 35.4 GB. Neither
converges. The decisions make it worse, but the context-free closure is too big on its own,
because the same impossible call targets resolve calls and instantiate summaries.

### Where the time and memory go

Timeout ladder (10 to 320 s, 55 GiB guard, default `decision`):

| Rung | Iterations | Peak | `locals` | `call_target_assign_like` | `cta_key` | `context_locals` | Decisions | Set unions that grew |
|---|---|---|---|---|---|---|---|---|
| 10 s | 1 | 9.9 GB | 0.8 M | 0.08 M | 0 | 0 | n/a | n/a |
| 40 s | 21 | 13.5 GB | 7.3 M | 1.2 M | 1.6 M | 0.02 M | 13 k | 12 k |
| 80 s | 53 | 18.2 GB | 22.6 M | 5.3 M | 9.2 M | 0.58 M | 44 k | 0.3 M |
| 160 s | 79 | 26.7 GB | 41.9 M | 13.0 M | 25.8 M | 4.05 M | 100 k | 40 M |
| 320 s | 97 | 42.2 GB | 69.2 M | 23.5 M | 52.8 M | 11.7 M | 120 k | 201 M |

"Decisions" here counts distinct interned `Decision`s (formal.path = target, without the
function); the table above counts `resolvent` rows.

- **Time.** At 320 s, the `call_target_assign_like`/`cta_key` step is the most expensive rule (14%
  of rule time), followed by `context_locals` (11%, 7% and 7% for its three largest rules) and
  `locals` (7%, 7% and 5%).
  - The context-free relations cost the same per new row at every rung: `locals` 0.9 µs,
    `call_target_assign_like` 2.1 µs. They are expensive by volume.
  - `context_locals` costs more per new row as the sets grow: 1.4, 1.6, 6.2 and 9.2 µs from
    20→40 s to 160→320 s. `context_assign` costs 14.6 µs per new row. This is the churn from
    "`cpuinfo` blowup", driven by the impossible decisions.
- **Memory: call-target tags take about half.** At 320 s the footprint is 39.0 GB.
  - The per-index census accounts for 34.3 GB: 25.9 GB in Ascent's default containers and 8.4 GB
    in BYODS stores.
  - `call_target_assign_like` and `cta_key` hold 17.0 GB of that and account for 17 of the
    30 GB the footprint grows from 10 s to 320 s.
  - Every contextual relation together holds 5.3 GB; `locals` is 2.4 GB.

| Relation (320 s) | Rows | Total | Row store | Indices |
|---|---|---|---|---|
| `cta_key` | 52.8 M | 10.13 GB | 3.76 GB | `cta_key_indices_0_1_2_3_4` 3.35 GB (full), `cta_key_indices_0_1_2` 3.03 GB (4.7 M keys) |
| `call_target_assign_like` | 23.5 M | 6.91 GB | 1.61 GB | `_indices_0_1_2_3` 1.44 GB (full), `_indices_0` 1.35 GB (48 k keys), `_indices_0_1_2` 1.33 GB (4.4 M keys), `_indices_0_1` 1.18 GB (0.84 M keys) |
| `edge_split` (BYODS) | 38.5 M | 2.58 GB | | trie |
| `locals` (BYODS) | 69.1 M | 2.42 GB | | trie |
| `context_locals` | 11.7 M | 1.89 GB | 0.94 GB | `_indices_0_1_2_3_4` 0.60 GB, `_indices_0_1_2` 0.21 GB, `_indices_none` 0.13 GB |
| `assign_like` (BYODS) | 23.8 M | 1.60 GB | | trie |
| `establishes_via` | 5.9 M | 0.96 GB | 0.20 GB | 4 indices, 0.16-0.21 GB each |
| `set_establishes_via` | 4.2 M | 0.87 GB | 0.40 GB | `_indices_0_1_2_3_4` 0.36 GB, `_indices_0_1` 0.10 GB |

`call_target_assign_like` is stored five times: the row store plus four indices, one of which
repeats the whole row. `cta_key` is stored three times, and it is recommendation 1's relation.
Recommendation 1 is still a net memory loss here, as "Beyond `cpuinfo`" found.

### Fix: write back entry versions for JVM bytecode

`ParamWriteBack::Entry` in `ctadl-ir/src/ssa/mod.rs` builds the exit param-flow without the
parameters and fills in their version 0 after renaming. `ctadl index` chooses it per import
(`ArtifactLanguage::param_write_back`): `Entry` for jvm, jar, dex, apk and xapk, and `Exit` for
the rest.

The C front end needs `Exit`: it lowers `*out = source()` to `assign @p0 = %t0`, since a pointer
parameter stands for its pointee, and the write-back is how `C:outparam` gets its flow. Applied
to every language, `Entry` lost that flow.

### Results

Same import and probe binary, at the 55 GiB guard, on a machine under load (so wall times are not
comparable):

| Build | Result | Iterations | Peak | `locals` | `critical_summary` | Decisions | `context_locals` | Default containers |
|---|---|---|---|---|---|---|---|---|
| `92fac26b` (320 s rung) | still growing | 97 | 42.2 GB | 69.1 M | 387 k | 585 k | 11.7 M | 25.9 GB |
| `92fac26b` | killed at 55 GiB, 440 s | | 55 GiB | | | | | |
| entry write-back | fixpoint | 399 | 12.4 GB | 7.5 M | 64 k | 36 k | 77 k | 2.6 GB |

- `call_target_assign_like` falls from 23.5 M rows to 0.97 M, and `cta_key` from 52.8 M to 1.6 M.
- `_quoteChar` is in no `critical_summary` row. The Jackson hubs keep at most one decision:
  `Prefetch.serialize` had 847.
- `typecheck.py`: 92.4% of decisions are compatible (17% before), 0.003% put an object in a
  primitive slot (6%), and impossible decisions hold 1.7% of `context_locals` memberships (44%).
  Of the 5.8% (2,123) "not a subtype" left, 799 are `$r8id` classes, which the split creates at
  import and the smali does not declare, so the script may not know their supertypes. The rest
  are not examined yet; the worst functions are Kotlin `bootstrap()` view models and Koin
  `module`, not Jackson.
- The regression suite loses no real flow. Its SARIF is identical in 329 of 330 files. In the
  chess app, the `getMyMove -> StringBuilder.append` flow into `Lt2/l;->F` was the same
  artifact: `F` reuses its `String` parameter's register for `getMyMove()`'s result, and the exit
  write-back fed that into its own incoming `String`. The fixture now scopes the sink to
  `Lx2/c;->y`, where the move really is appended; that passes with and without the change.

### Fixes considered before the cause was known

With the write-back fixed, none of these is needed for greenbits. 1 and 2 are still sound
precision filters.

1. **Filter call-target tags by static type.** This removes the cause, and it shrinks both halves:
   the call-target relations that hold half the memory, and the decisions.
   - First step, with facts we have: drop a tag at a `ByVal` formal and at a field or array
     element of primitive type (field types are already in the path symbols). That covers the
     5.5-6.7% primitive cases.
   - Full fix: export a subtype fact from the dex import (the frontend already reads `.super`
     and `.implements`) and the static type of each vertex, and drop a tag wherever
     `target <: declared type` fails. That covers the 54-60%. A `check-cast` should narrow the
     same way.
2. **Only mint a decision whose target can resolve a critical call.** Add the site's dispatch key
   to `critical_summary`, carry it up through rule 1.2, and require
   `callee_resolvents(target, key, _)` in rules 2.1 and 2.2.
   - A target that answers none of the calls its decision exists for never produces a
     `context_assign` (rule 3.1 needs exactly that join), so this should change no results and
     needs no new facts.
   - It removes the `JsonGenerator` and `byte[]` cases, but not method names every class has,
     such as `toString`.
   - It prunes decisions only, not tags, so on its own it leaves the memory.
   - Effect not measured.
3. **Store call-target tags compactly.** Independent of precision: put `call_target_assign_like`
   and `cta_key` in a BYODS trie, as `locals` and `assign_like` are (37 and 67 B/row, against
   294 and 192 B/row here), or drop recommendation 1's `cta_key` outside high-fan-out vertices.
4. **Build critical summaries from identity flows only.** Rule 1.1 should ask which formal path
   the receiver object comes from: copies, field loads and stores, and returns, not computed
   values. This is the most accurate fix and the most work: it needs an identity-flow subset of
   `locals`.
5. **Not a fix: bounding decision sets.** `bounded:k` and `spill:k` cap the churn, but `none`
   shows the context-free closure does not converge either.
6. **Not a general fix: a Jackson model.** It would help this app, but the Compose and coroutine
   hubs show the problem is general.

### Data

All output is under `/Volumes/Shampoo/ct-bigapk/small/greenbits-probe/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `bin/ctadl` (with `bin/ctadl.diff`, uncommitted, on `92fac26b`) is the probe binary for the
  ladder. `bin/ctadl-v1` made `runs/{decision,none}-t240`. `src/` is the probe's detached git
  worktree, and `run.sh` indexes the r8-general `both` import. The probe:
  - `CTADL_DECISION_CENSUS=<dir>` writes `decisions.tsv`, `critical_summary.tsv`,
    `callee_info.tsv`, `context_sets.tsv` and `establishes_{direct,via}.tsv`. This part is
    uncommitted.
  - The vendored Ascent macro generates `index_sizes_summary()`: keys, entries and approximate
    shallow bytes per relation and per index, logged as `[idxsizes]`. It also logs heap reports
    for the `locals_key`, `edge_split` and `ext_dst` stores. Committed in `429a66fa`, so every
    index run now logs both.
- `runs/t{10,20,40,80,160,320}/`: the ladder. `index.err` is the full log, and `census/` is the
  decision dump. `runs/t{80,320}/rank.txt` come from `../cpuinfo-blowup/rank.py`, and
  `runs/t320/idx.txt` from `idx.py`.
- `runs/guard55/`: no timeout, killed at the 55 GiB guard at 440 s.
- `runs/{decision,none}-t240/`: the hybrid-context A/B.
- `marginal.txt` (from `../cpuinfo-blowup/marginal.py`), `memgrowth.txt`: marginal cost and
  memory per relation across the rungs.
- `typecheck.py <decisions.tsv>`: classifies each decision against `smali/out`, the app's
  baksmali output. `runs/*/typecheck.txt` holds its output.
- `bin/ctadl-focus` (`bin/ctadl-focus.diff`): the probe plus `CTADL_FOCUS=<dir>:<substr>|...`,
  which dumps `call`, `summary`, `assign_like` and `locals` rows for the named functions (serial
  engine only). `runs/focus-t40/focus/*.tsv` is the trace in "The cause".
- `bin/ctadl-entry` (`bin/ctadl-entry.diff`): the probe with the entry write-back for every
  language. `runs/entry-guard55/`: the greenbits run in "Results", with `typecheck.txt` and
  `idx.txt`. `runs/scoped-guard55/`: the same run with the per-language build; identical.

The fix's measurements are under `/Volumes/Shampoo/ct-bigapk/small/paramflow-entry/`:

- `bin/ctadl-{base,entry,scoped}`: the probe (`92fac26b`), the probe with the write-back for every
  language, and the branch build with it for JVM bytecode only (`bin/ctadl-scoped.diff`).
- `reg-{base,entry,scoped}/`, `sarif-diff.txt`, `sarif-diff-scoped.txt`: the regression suite and
  its comparisons. `chess-repin-{base,scoped}/`: the chess case with the re-scoped sink.
- `chess-app-ir.txt`: the chess app's IR dump, for the `Lt2/l;->F` and `Lx2/c;->y` bodies.
