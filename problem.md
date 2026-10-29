# P1: stack slots mix every value in a native function - DO-NOT-MERGE

The largest memory problem on native code. In a big native function, the index's `locals`
relation (which access paths reach which variables) grows to hundreds of millions of rows,
because the stack frame acts as one function-wide mixing pool. Apps with this shape need 5-15x
the memory budget (100x the imported IR), and before any fix they did not finish at all.

This branch is off `main` (merged up to `4490f7bf`; `ebd22e01` before it interns the index
engine's values as 4-byte handles, #170). It has S3 (stack-slot promotion, `window:8`
by default) and S5 (full promotion of large functions, behind `ctadl index --mem2reg full`); S5's
design and first measurements are under S5 below. It also has S29's frame updates (steps 1, 2, 3
and 5) and a pcode frontend fix found through them: taking a frame address no longer writes the
frame. That fix alone brings `ntfs-3g` from 30x over budget to budget and cuts `androidudpbus`'s
peak by a quarter at the default (S29). Merging #170 then cuts every measured peak by 22-61%
and changes no relation count or finding (S29, "After merging `main`'s interning"). With S29's
step 3, mem2reg on frame updates, `--frame update` beats the default on all four binaries but
`uclited`, where it ties, and brings `androidudpbus` under budget (S29, "Step 3"). The default is
still `--frame store`. `improve-native-handling` has the earlier record:
`things-to-improve.md` there covers P1, S3, S4, M1 and M7.

## Starting point

- **S3** (`b1b52de8` on `improve-native-handling`) is cherry-picked here, with S28 (the regression
  suite fails on a lost flow instead of skipping on Darwin).
- **S5** is on this branch. `ctadl index --mem2reg <policy>` (or `CTADL_MEM2REG`) picks the
  policy: `window:N`, `exact`, `above`, `full[:N]` or `off`.
- **S29 steps 1, 2, 3 and 5** are on this branch: pcode writes the frame as `Update`,
  `ctadl index --frame store` (the default; `CTADL_FRAME`) coarsens it back to `Store`, and
  under `--frame update` mem2reg promotes the slots no escape reaches and an update overwrites
  the slot it writes. See S29.
- **The PTRSUB fix** (`6b0ab678`): the pcode frontend no longer lowers taking a frame address
  as a write into the frame. It changes the default IR; see S29.
- **Optional, for native apps that also link libc++abi's demangler** (P4, not P1): `bb9cc766`
  "Model __cxa_demangle and skip the helpers only it reaches" (S6).

**Optional measurement tooling** (none of it is on `main`):

```sh
git cherry-pick b47ed139   # vendors a patched ascent_macro (a timed-out run keeps the running
                           # scc's contents) and adds CTADL_LOCALS_CENSUS and CTADL_DEAD_PHIS
git cherry-pick 5858fa78   # "Log the size of every relation and index" ([idxsizes])
git cherry-pick 918c771a   # "Count each rule's rows derived and new, and log each iteration"
                           # (per-rule counters, CTADL_ITER_LOG)
git cherry-pick c1ee3d61 f9e5b24a   # scripts/blowup-rank.py, and the Windows memguard/ladder
```

These were written against `076f6a60`, before `main` moved the index engine into `ctadl-index`
and ran the SSA passes per function on threads, so expect conflicts. For the census alone, the
S5 measurements applied `b47ed139`'s `CTADL_LOCALS_CENSUS` hunks by hand in a scratch worktree.
`b47ed139` is titled "WIP-EXPERIMENT: dead-phi pass, locals census, SSA/stack census examples";
its experiment hooks are off unless their variables are set. On Linux,
`systemd-run --user --scope -p MemoryMax=<N>G -p MemorySwapMax=0` stands in for
`scripts/memguard.ps1`. `main` already writes `profile.json` (#159), which covers relation counts
and scc/rule times without the patched macro.

## The problem

Ghidra's p-code addresses every stack slot through one SSA variable, the stack pointer at
function entry (`%L3_0`). A store `*(sp + 16) = x` and a load `y = *(sp + 16)` are a field store
and a field load on that one variable, at the path `.[16].deref`.

- **Each load inherits every value ever stored to its slot.** SSA versions registers, not memory,
  so a slot written 300 times hands all 300 values to each of its loads.
- **Loaded values are stored again into other slots,** so values spread from slot to slot until
  every variable in the function is reached from nearly everything.
- **So `locals` rows ≈ (variables in the function) × (all sources).** In `androidudpbus`'s
  `FUN_00144f04` (a Rust/tokio async state machine, 803 blocks), 55,775 stores go into 255 slots
  and 105,050 loads read 285 slots. A typical slot receives about 300 distinct values and is read
  by about 590 loads. That one function holds 86-92% of the app's `locals`, and almost every
  variable in it is reached from the same 1-3 k sources (paths of formal 0 plus paths into global
  memory).
- **The cost is volume, not cost per row.** The `locals` rules run at 0.01-0.3 µs per row, flat
  over time; there are just 250-530 M rows.

The same shape appears on the JVM side as P28 (the globals slot); see that branch.

## Measurements

Before any fix, `androidudpbus` (567 KB of Rust in `libudphub.so`) did not index in 15 minutes and
reached 88 GiB; `locals` held 10.7 M rows after 20 s of fixpoint, 152 M after 80 s and 341 M after
160 s.

With pruned SSA (on `main`) and S3 (on `improve-native-handling`), each APK imported whole (Dex and
arm64 libraries), 1800 s timeout, 55 GiB guard:

| App | IR | Peak | Against budget | Result |
|---|---|---|---|---|
| `com.kaeruct.glxy` (`libgdx`) | 15.1 MB | 21.5 GB | 14.3x | fixpoint, 117 s |
| `com.github.msx80.retrodrawing` (`libgdx`) | 15.4 MB | 20.0 GB | 13.0x | fixpoint, 236 s |
| `com.zoffcc.applications.avifview` | 51.7 MB | 59 GB or more | 11.5x | guard hit, 611 s |
| `app.halma` (`libgdx`) | 31.5 MB | 22.6 GB | 7.2x | fixpoint, 190 s |
| `org.vi_server.androidudpbus` (Rust) | 28.2 MB | 15.8 GB | 5.6x | fixpoint, 173 s |
| `org.mattvchandler.a2050` | 60.2 MB | 33.9 GB | 5.6x | fixpoint, 889 s (also P6) |
| `fir.tube` | 27.1 MB | 12.4 GB | 4.6x | fixpoint, 93 s |

- The three `libgdx` apps, two different builds of the library, all peak at 20-23 GB whatever the
  rest of the app is: `libgdx`'s own code is the cause. In `glxy`, `FUN_0010b408` holds 97% of
  `locals` (425 M rows, 12.5 GB).
- **WhatsApp 2.17.79 with its native libraries** (see Samples) indexes on
  `improve-native-handling` at 7.34 GB in 484 s, against 2.7 GB and 62 s Dex only. `locals`
  holds 186 M rows over 2.06 M distinct `(function, variable, path)`, 90 rows per path (1.4 on
  Java code), and the `locals` join takes 154 s of the 438 s main loop. That is this shape; which
  function holds the rows was not checked.

## Current state

**Since the PTRSUB fix** (S29, "Taking a frame address no longer writes the frame") and merging
`main`'s interning (#170), the default (`--frame store`, `window:8`) is, same settings as the
table below:

| App | `locals` | Fixpoint | Peak | Against budget | Before #170 | Before the fix |
|---|---|---|---|---|---|---|
| `uclited` | 40.4 M | 9.9 s | 0.65 GiB | 3.5x over | 1.24 GiB, 6.7x | 41.4 M, 1.21 GiB |
| `ntfs-3g` | 5.0 M | 2.9 s | 0.39 GiB | **under (0.8x)** | 0.50 GiB, 1.0x | 283.8 M, 14.93 GiB |
| `glxy` | 85.0 M | 36 s | 2.25 GiB | 1.6x over | 3.79 GiB, 2.7x | 100.4 M, 4.10 GiB |
| `androidudpbus` | 289.6 M | 93 s | 6.53 GiB | 2.5x over | 9.88 GiB, 3.8x | 337.9 M, 13.31 GiB |

The rest of this section, and the firmware sections after it, are from before the fix and before
#170, so their peaks are higher than today's; #170 changes no row count. `exact`, `full` and
`full:0` have not been re-run with either, nor have the other binaries.

**Under `--frame update`** (S29, "Step 3"), `androidudpbus` falls to 41.9 M rows and 2.25 GiB,
**under budget (0.86x)**, and `glxy` to 76.6 M and 2.12 GiB (1.5x). `ntfs-3g` falls to 4.3 M and
`uclited` stays at 40.7 M. Every finding is the same.

The table above (Measurements) is from before S3 was re-measured on `main`. On this branch (`main` `7c98b73e`
with S3 and S5), on macOS (arm64), one run at a time, peaks from `time -l`. The budget is 100x
the imported IR, as in the table above (`uclited` 2.0 MB, `androidudpbus` 28.2 MB, `glxy` 15.1 MB):

| App | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget | Largest function's share |
|---|---|---|---|---|---|---|
| `uclited` | `window:8` (default) | 41.4 M | 10.2 s | 1.26 GiB | 6.8x over | `inflate` 99.3% |
| | `full` | 12.1 M | 3.0 s | 0.54 GiB | 2.9x over | `inflate` 97.7% |
| | `full:0` | 11.8 M | 2.7 s | 0.52 GiB | 2.8x over | |
| `androidudpbus` | `window:8` (default) | 337.9 M | 166 s | 13.27 GiB | 5.1x over | `FUN_00144f04` 71.7% |
| | `exact` | 349.7 M | 143 s | 15.11 GiB | 5.8x over | |
| | `full` | 30.9 M | 22 s | 2.09 GiB | **under (0.8x)** | `FUN_00156cb8` 35.8% |
| | `full:0` | 23.2 M | 16 s | 1.87 GiB | **under (0.7x)** | |
| `glxy` | `window:8` (default) | 100.4 M | 44 s | 4.07 GiB | 2.9x over | `FUN_0010b408` 90.2% |
| | `exact` | 98.2 M | 44 s | 4.01 GiB | 2.9x over | |
| | `full` | 83.7 M | 38 s | 3.64 GiB | 2.6x over | `FUN_0010b408` 88.8% |
| | `full:0` | 83.1 M | 37 s | 3.64 GiB | 2.6x over | |

The shares are from the census build before the merge (S5 below), on row counts within 5% of
these. `uclited`'s `window:8` row is from before the merge too; its `full` row was re-run after.
The `exact` and `full:0` rows were run after the merge without the census, so they have no share.

`exact` (S3's rejected policy) does not help the apps. On `androidudpbus` it keeps 29% fewer
accesses in memory than `window:8` (121 k against 170 k) but `locals` grows 3.5% and the peak
1.8 GiB: the slots an address escapes from, which `exact` still keeps, are the busy ones, so
promoting their neighbors removes nothing. On `glxy` it cuts 2%. Only `full` moves either app.

`full:0` promotes every function fully, not just those above 1,000 accesses. It adds little over
`full`: `androidudpbus` falls a further 25% (30.9 M to 23.2 M, with 505 functions promoted fully
instead of 27), `uclited` 3% and `glxy` 1%. It also names many more functions in
`notifications.log` (496 in `androidudpbus`, 103 in `uclited`, 34 in `glxy`), so `full`'s threshold
keeps most of the gain with far fewer functions treated unsoundly.

What is left of P1 (before the fix, except where it says):

- **`ntfs-3g`:** at budget under the default since the fix (under it since #170), so it is no
  longer a P1 sample.
- **`uclited`:** the fix hardly moves it. `inflate` still holds 98% under `full`. Its stack is done; causes 2 and 3 of the
  Analysis below (the loop's SCC, the offsets) are what remain.
- **`glxy`:** promoting `FUN_0010b408` fully cuts only 16%, so its rows are not from the stack.
  P6 is the suspect. The fix cuts its default 15% (100.4 M to 85.0 M).
- **`androidudpbus`:** the hub `FUN_00144f04` drops from 243 M rows to 0.2 M. The largest function
  is now `FUN_00156cb8` (11 M rows), below the 1,000-access threshold, so it still runs
  `window:8`. Promoting every function fully (`full:0`) cuts the app a further 25%; it is already
  under budget without that. The fix takes its default from 5.1x to 3.8x over budget, and #170
  to 2.5x.
- **The other apps in the table above** have not been run with `full`.

### Other firmware binaries

Measured before the PTRSUB fix (S29), and not re-run since; it took `ntfs-3g` from 30x over
budget to budget, so these may move too.

Over-budget binaries from the same runner (How to reproduce), run the same way as `uclited`: the
sweep's model, a memguard cap (16 GiB for `chat` and `nginx`, 24 GiB for the rest), peaks from
`time -l`, on this branch at `0145608b`. The last three are `firmware-sweep` benchmarks
`d586b9453407b65a`, `fb2ce71d346450a4` and `25c31637a1f58427`, chosen as the most over budget
of those that finished in the runner (64x, 57x and 44x at `dfccbcab`), one per shape.

| Binary | IR | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget |
|---|---|---|---|---|---|---|
| Netgear MBM621 `bin/chat` (MIPS32 BE, static, not stripped) | 1.2 MB | `off` | 12.9 M | 17.1 s | 1.34 GiB | 12.1x over |
| | | `window:8` (default) | 18.4 M | 16.8 s | 1.48 GiB | 13.4x over |
| | | `full` | 18.2 M | 17.7 s | 1.46 GiB | 13.2x over |
| Tenda AC15 `usr/bin/nginx` (ARM, stripped) | 11.4 MB | `window:8` (default) | | killed at 106 s | 16 GiB cap | 15x or more |
| | | `full` | | killed at 108 s | 16 GiB cap | 15x or more |
| D-Link DAP-2690 `usr/sbin/captival_usockc` (MIPS32 BE, static, debug info) | 2.1 MB | `off` | 41.7 M | 94.1 s | 13.15 GiB | 65.9x over |
| | | `window:8` (default) | 28.3 M | 56.8 s | 6.87 GiB | 34.4x over |
| | | `full` | 27.3 M | 57.2 s | 6.85 GiB | 34.3x over |
| Netgear MVBR1210C `usr/sbin/sdkmips` (MIPS32 LE, static, stripped) | 2.9 MB | `off` | 19.8 M | 80.0 s | 9.05 GiB | 34.1x over |
| | | `window:8` (default) | 8.6 M | 37.6 s | 6.79 GiB | 25.6x over |
| | | `full` | 8.6 M | 44.2 s | 6.73 GiB | 25.3x over |
| D-Link DPH-128MS `usr/sbin/pppd` (MIPS BE, dynamic, stripped) | 3.5 MB | `off` | 31.4 M | 107.5 s | 14.32 GiB | 43.5x over |
| | | `window:8` (default) | 4.4 M | 2.0 s | 0.55 GiB | 1.7x over |
| | | `full` | 4.4 M | 1.9 s | 0.56 GiB | 1.7x over |

- **`chat` is not a P1 shape.** `full` promotes one function (`_fpmaxtostr`, notified) and moves
  nothing; promotion of any kind *adds* rows (`off` has 30% fewer than `window:8`). What holds its
  rows was not checked. The import takes 23 s.
- **Nor, by the evidence so far, is `nginx`.** `window:8` already promotes 77,878 of its 81,135
  stack accesses (1,125 functions); `full` promotes 15 functions fully, keeps 701 accesses in
  memory, and reaches the cap at the same time. A run stopped at 60 s
  (`CTADL_INDEX_TIMEOUT_SECS=60`) peaked at 10.6 GB, but on this branch a timed-out run loses the
  running scc's contents, so its `profile.json` has no `locals` count and no rule times; the
  relations it does have are small (`reach_vp` 0.62 M, `locals_key_wild` 0.56 M,
  `edge_split_wild` 0.44 M, `call_target_assign_like` 0.23 M). Finding what grows needs the
  patched `ascent_macro` (`b47ed139`) or a dump. The import takes 40 s.
- **S3 already does the work on the other three; `full` adds nothing.** `window:8` halves
  `captival_usockc`'s and `sdkmips`'s peaks and takes `pppd` from 14.3 GiB and 108 s to 0.55 GiB
  and 2 s, nearly to budget. `full` promotes 5, 3 and 1 functions fully (all notified:
  `_vfprintf_internal`, `__dns_lookup` and others from uClibc, `vslprintf` in `pppd`) and moves
  `locals` by at most 3.5%: under `window:8` they keep only 937, 1,289 and 1,804 accesses in
  memory.
- **What is left of `captival_usockc` and `sdkmips` is not the stack.** Both still peak near 7 GiB,
  25-35x over budget. In `captival_usockc` `locals` (28 M) and `edge_split` (15 M) lead; in
  `sdkmips` `edge_split` (14 M) outgrows `locals` (8.6 M), with `ctx_edge_split` and
  `context_locals` next. The `locals` and `context_locals` joins on `edge_split` take the most rule
  time in both. Which functions hold the rows was not checked. Each import takes 22-25 s.

#### A screen for stack hubs

The mem2reg counts do not find a hub: under `window:8`, `uclited` keeps 931 stack accesses in
memory and `full` promotes 304 more, no different from binaries without one. What does find one is
indexing under both policies and comparing. The 40 most over-budget distinct `firmware-sweep`
binaries (at least 22x in the runner, more than 0.5 MB of IR, `uclited`'s copies left out) were
each indexed under `window:8` and `full` for 30 s, six at a time, against `uclited` (1.21 GiB to
0.53 GiB, `locals` 41.4 M to 12.1 M). 34 change by 5% or less. The ones that fall, run to the end
(three at a time, so the times are loose):

| Binary | IR | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget |
|---|---|---|---|---|---|---|
| D-Link COVR-2600R `sbin/ntfs-3g` (ARM, dynamic) | 5.4 MB | `window:8` (default) | 283.8 M | 227 s | 15.04 GiB | 29.9x over |
| | | `full` | 199.3 M | 96 s | 10.22 GiB | 20.3x over |
| D-Link DAP-2310B `sbin/wifitool` (MIPS32 BE, static, debug info) | 1.9 MB | `window:8` (default) | 47.7 M | 168 s | 4.56 GiB | 26.0x over |
| | | `full` | 36.1 M | 125 s | 3.91 GiB | 22.3x over |
| Linksys MX5300 `bin/ntfslabel` (ARM, dynamic) | 6.0 MB | `window:8` (default) | 28.5 M | 13.7 s | 1.68 GiB | 3.0x over |
| | | `full` | 15.0 M | 6.7 s | 1.05 GiB | 1.9x over |

- **`ntfs-3g` looked like the best new sample:** the largest stack effect in absolute terms (85 M
  rows, 4.8 GiB, 131 s) and still 20x over budget under `full`. `firmware-sweep` benchmark
  `46ad36214ff2f30b`. Its hub is `FUN_0001f97c` (below). Since the PTRSUB fix it is at budget
  under the default: most of what is described here was that artifact.
- **The Linksys ntfs-3g tools share the shape on a smaller scale:** `ntfslabel` halves under `full`
  (11 functions promoted fully), as do EA6350v4 `ntfsdebug` (21.8 M to 11.5 M) and EA6900v2
  `ntfslabel` (19.6 M to 11.1 M) in the 30 s screen.
- **`wifitool` falls 24%:** `full` promotes `FUN_0040d740` and three uClibc functions.

#### `ntfs-3g`'s hub: `FUN_0001f97c`

Measured before the frontend stopped writing taken frame addresses into the frame (S29, "Taking
a frame address no longer writes the frame"). With that fix the whole binary indexes in 5.0 M
rows and 0.50 GiB under `window:8`, so what follows describes the artifact as much as the hub.

`full` promotes 5 functions fully. Skipping each one's body in turn (a `skip-analysis` model, as
for `inflate` in How to reproduce) finds the hub:

| Skipped | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget |
|---|---|---|---|---|---|
| nothing | `window:8` | 283.8 M | 227 s | 15.04 GiB | 29.9x over |
| `FUN_0001f97c` | `window:8` | 18.3 M | 23 s | 1.79 GiB | 3.6x over |
| nothing | `full` | 199.3 M | 96 s | 10.22 GiB | 20.3x over |
| `FUN_0001f97c` | `full` | 17.9 M | 18 s | 1.72 GiB | 3.4x over |
| `FUN_0003c4f4` | `full` | 195.3 M | 97 s | 9.93 GiB | |
| `FUN_00016d24`, `FUN_0002aa84` or `FUN_00042810` | `full` | 199.2-199.3 M | 96-98 s | 10.2 GiB | |

The skip runs ran five at a time, so their times are loose.

- **One function holds 94% of `locals` under `window:8` and 91% under `full`.** Without it the
  binary is 3.4-3.6x over budget, whatever the policy.
- **Its shape is `inflate`'s:** 293 blocks, 7 parameters (three passed on the stack), calls itself
  twice, and 2,509 direct stack accesses (901 stores, 1,608 loads). Six adjacent 4-byte slots,
  `__stack_top.[-64]` through `[-44]`, take 2,301 of them (830 stores, 1,471 loads, about 380
  each), and their addresses are never taken.
- **So, as in `inflate`, promotion is not enough.** `full` promotes those slots (they escape
  nowhere, so no reloads) and the function still holds 181 M rows. What remains is not the stack;
  `inflate`'s causes 2 and 3 (Analysis) are the first suspects. Not yet checked.
- **To look at it:** `ctadl inspect <project> --dump-ir --function FUN_0001f97c`.

## S5: full stack-slot promotion, behind a flag

`--mem2reg full` (off by default; the default stays `window:8`) promotes every directly accessed
slot, whatever escapes. Direct stores go only to the local, so memory holds only what callees and
copies of the address write. After a call that is passed an address reaching slot `k` (under
`window:8`), the local is reloaded from the slot: `__stack_mK = load __stack_top.[k].deref`.

**Scope.** A function is promoted fully only when it has more than N direct stack accesses
(`--mem2reg full:N`, default 1,000; mem2reg already counts them), and gets `window:8` otherwise.
The functions that hold most of `locals` (`inflate`, about 3.5 k accesses; `FUN_00144f04`, about
160 k) are far above any function of ordinary size, so the unsoundness stays confined to them. A
model mode overrides the threshold for one function: `mem2reg-full` forces full promotion,
`mem2reg-window` forbids it.

The reload costs little (memory holds only callee writes, not every direct store) and keeps the
common out-parameter case, which is `example.c`:

```c
struct foo x; transfer(&x.b, source()); sink(x.b.d);   // transfer writes out->d; the reload brings it back
```

**What it loses.** The callee, or anything else holding the escaped address, can no longer see
values the function stored directly:

- **Inputs passed by pointer:** `s.a = source(); f(&s);`, where `f` reads `s->a`.
- **Stored or copied addresses** (`p = &buf; ...; *p`): reads see only writes through the address,
  not direct stores, and the reverse.

There is no write-back before calls, by choice. It would restore the inputs-by-pointer case, but
memory is flow-insensitive, so a value written back before one call reaches every read of the
slot. That is S4's write-through again, and it would bring back S4's cost (P24).

**Notifications.** The notifications log is high signal and low noise, like
`log_once::warn_once!` (already used for unsupported pcode mnemonics): each fact is stated once,
never once per site.

- **One line per function, and only when it matters:** the function was promoted fully *and* some
  promoted slot is reached (under `window:8`) by an escape that full promotion stops modeling:
  a call input, a stored or copied address, a return. Escapes that the reload covers (callee
  writes) are not reported.
- **A fixed message key,** for example `mem2reg full: <function>: frame address escapes (call
  input, stored); flows through those slots may be missed`. The kinds are a sorted set, with no
  counts or sites in the key, so it dedupes like `warn_once!`. Counts and sample sites go in
  the debug log only.
- **Written to `index/notifications.log`** next to `profile.json`, and copied by `ctadl query`
  into SARIF's `toolExecutionNotifications` at warning level. A missing flow can then be traced
  to a function the analysis treated unsoundly, without reading through noise.

**The bar it had to clear:** `uclited` within about 1.5x of `exact`'s 15 M `locals` rows, and
`androidudpbus` well below S3's 15.8 GB peak. `glxy` was only reported (its suspect is P6, not the
stack).

### First measurements, before merging `main` `7c98b73e`

Measured on `main` `076f6a60` with S3 (`b1b52de8`) cherry-picked, on macOS (arm64, 128 GB), one
run at a time. Peaks are `time -l`'s peak memory footprint. Fixpoint times are from runs with the
machine to themselves. The share of `locals` comes from a separate build that adds `b47ed139`'s
`CTADL_LOCALS_CENSUS`, which is not on this branch. `glxy` and `androidudpbus` are the F-Droid
APKs in Samples, imported whole, with the default models.

| App | `--mem2reg` | `locals` | Fixpoint | Peak | Largest function |
|---|---|---|---|---|---|
| `uclited` | `off` | 188.8 M | 35.4 s | 10.89 GiB | `inflate` 99.8% |
| | `window:8` | 41.4 M | 10.2 s | 1.26 GiB | `inflate` 99.3% |
| | `exact` | 14.9 M | 3.7 s | 0.63 GiB | `inflate` 98.1% |
| | `full` | 12.1 M | 3.1 s | 0.54 GiB | `inflate` 97.7% |
| | `full:0` | 11.5 M | 3.0 s | 0.52 GiB | |
| `androidudpbus` | `window:8` | 339.5 M | 187 s | 13.25 GiB | `FUN_00144f04` 71.7% |
| | `full` | 31.6 M | 21 s | 2.26 GiB | `FUN_00156cb8` 35.8% |
| `glxy` | `window:8` | 105.0 M | 45 s | 4.31 GiB | `FUN_0010b408` 90.2% |
| | `full` | 88.3 M | 37 s | 3.78 GiB | `FUN_0010b408` 88.8% |

- **It cleared the bar.** `uclited` comes in below `exact` (`exact` still keeps the slot an
  address escapes from, and that slot takes every direct store). `androidudpbus` falls 5.9x in
  peak and 10.7x in `locals`. Its hub `FUN_00144f04` goes from 243.4 M rows to 0.21 M.
- **The baseline moved.** On `076f6a60`, `window:8` peaks at 13.25 GiB on `androidudpbus` (15.8 GB
  in Measurements) and 4.31 GiB on `glxy` (21.5 GB).
- **Scope.** `full` (threshold 1000) promotes 2 functions fully in `uclited`, 27 in
  `androidudpbus` and 3 in `glxy`. Every one of them gets a notification, mostly `(call input,
  copied, stored)`. The query's SARIF carries them as `CTADL0104.index-notification` warnings.

After merging `7c98b73e` (SSA passes per function on threads, mem2reg among them), mem2reg's
counts are identical; the rows fall 1-5% from `main`'s own changes. Current state has those runs.

**Regression suite** (`cargo xtask regression --frontend pcode,jni`): the default passes 46 of 46.
Under `full:0`, the first version lost one flow, `Jni:JniWide+summary`, and no notification the
query could see named it. Two causes, both fixed:

- **A returned slot.** The pcode frontend returns a value held in a stack slot as the slot's
  address (`keep` ends in `return %__stack_top.[4]`). Promoting that slot dropped `keep`'s
  argument-to-return flow, and with it `Java_JniWide_nativeHigh`'s summary. Full promotion now
  leaves a slot that a returned address reaches in memory, as `window:8` does. None of the
  large functions above returns a frame address, so their numbers do not change.
- **A summary project's notifications.** `keep`'s notification was in the summary project's
  `notifications.log`, but the query runs on the app's index. An index now carries the
  notifications of each summary project it loads, prefixed `summary project '<name>':`.

After the fixes, `full:0` passes 46 of 46, and 7 cases' indexes name a function (`main` or
`handler`).

## S29: the frame as `Update`, coarsened by default

Steps 1, 2, 3 and 5 of S29 (Ideas for solutions) are done. The current numbers are under
"Step 3" for `update` and "After merging `main`'s interning" for the default; "Taking a frame
address no longer writes the frame" has the same runs before that merge and before step 3, and
"What keeping `Update` costs" is from before steps 3 and 5 and that fix.

- **The frontend** writes into the frame itself, `__stack_top.[k].deref := v`, as
  `__stack_top = update (__stack_top.[k].deref := v)`. A write through anything else (a copy of
  the frame address, a loaded pointer) is a `Store`, as before.
- **The pipeline's first pass**, `coarsen_frame_updates`, turns each frame update back into the
  store it stands for, before dead-temp elimination, coalescing and mem2reg, which then see the IR
  they saw before. `ctadl index --frame store|update` (`CTADL_FRAME`) picks the default, `store`;
  model modes `frame-store` and `frame-update` pick one for one function.
- **mem2reg** (step 3) treats a frame update as a direct store to its slot. A slot it promotes
  becomes a local, and its updates become copies; a slot an escape reaches (under `--mem2reg`'s
  policy) keeps its updates, so SSA versions the frame for the slots left in it.

### Step 3: mem2reg on `Update`

Before step 3, mem2reg left alone every function that defined `__stack_top`, so under `update` it
promoted nothing: `androidudpbus` skipped 466 functions and promoted 4 slots, against 3,102 under
`store`. Now a frame update, `__stack_top = update (__stack_top.[k].deref := v)`, is a direct
store to slot `k` like any other (`bdcc187e`). Promoting the slot rewrites it into
`__stack_m<k> = v`; a slot an escape reaches keeps its updates. Any other definition of
`__stack_top`, or an update that copies the frame into another variable, still skips the
function. Under `--frame store` there are no updates left when mem2reg runs, so the default is
unchanged: every relation count and SARIF result is equal to the run before step 3.

Same settings as "After merging `main`'s interning" below, on the imports after `4490f7bf`:

| App | `--frame` | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget | `update` before step 3 |
|---|---|---|---|---|---|---|---|
| `uclited` | `store` | `window:8` (default) | 40.4 M | 9.4 s | 0.66 GiB | 3.5x over | |
| | `update` | `window:8` | 40.7 M | 9.3 s | 0.67 GiB | 3.6x over | 211.9 M, 5.03 GiB |
| | `update` | `above` | 115.7 M | 21.4 s | 1.68 GiB | 9.0x over | |
| `ntfs-3g` | `store` | `window:8` (default) | 5.0 M | 2.8 s | 0.40 GiB | under (0.8x) | |
| | `update` | `window:8` | 4.3 M | 2.2 s | 0.32 GiB | under (0.6x) | 15.9 M, 0.60 GiB |
| | `update` | `above` | 11.7 M | 4.5 s | 0.48 GiB | under (0.95x) | |
| `glxy` | `store` | `window:8` (default) | 85.0 M | 36 s | 2.25 GiB | 1.6x over | |
| | `update` | `window:8` | 76.6 M | 32 s | 2.12 GiB | 1.5x over | 86.6 M, 2.28 GiB |
| | `update` | `above` | 84.6 M | 34 s | 2.26 GiB | 1.6x over | |
| `androidudpbus` | `store` | `window:8` (default) | 289.6 M | 98 s | 6.61 GiB | 2.5x over | |
| | `update` | `window:8` | **41.9 M** | 44 s | **2.25 GiB** | **under (0.86x)** | 283.6 M, 13.65 GiB |
| | `update` | `above` | 266.9 M | 680 s | 13.77 GiB | 5.2x over | |

"Before step 3" is the `update` row of "After merging `main`'s interning" (`glxy`'s from before
`4490f7bf`, which moves it by at most 90 rows a relation).

- **`update` with `window:8` now beats the default** on `androidudpbus` (7x fewer rows, 2.9x less
  peak, under budget), `glxy` (10% fewer rows) and `ntfs-3g` (13%), and ties on `uclited`, whose
  rows are `inflate`'s loop, not its stack (Analysis). mem2reg promotes exactly the slots it
  promotes under `store` (416,021 accesses on `androidudpbus`); what changes is the 164,771 it
  keeps, which are now versioned and overwritten instead of one pool per slot. Only 59,359 frame
  updates are left there, against 205,273 before.
- **The versions × paths volume is gone with them:** `androidudpbus`'s distinct
  `(function, variable, path)` in `locals` are 2.6 M (17.3 M before step 3, 1.0 M under `store`),
  `locals_key` 8.4 M (58.5 M, 2.8 M), `reach_vp` 2.6 M (17.3 M, 1.0 M).
- **Every finding is the same** as the default's on all four binaries (79, 75, 1 and 1, by rule,
  message and location), under `window:8` and `above` alike. The regression suite (`pcode`,
  `jni`) passes 46 of 46 under `--frame store` and `CTADL_FRAME=update`.
- **`above` costs what skipping cost.** It keeps every slot at or above an escape, which in these
  functions is nearly all of them: on `androidudpbus` it promotes 12,289 accesses and keeps
  568,503, so the rows and peak return to before step 3. It is the only policy that loses no
  flow, so `update` does not yet replace `window:8`'s guess with something sound at an
  affordable cost; it makes the slots `window:8` keeps precise.
- **Not measured:** `full` and `full:0` under `update`, and the other binaries.

### Step 5: the overwrite rule

Codegen lowers `x = update (y.w := v)` to the write `x.w <- v` and a new fact,
`update_sans_path(x, y, w)`: `x` is `y` except at `w` and the paths under it. It is not an
`assign_like` edge. The index derives `ext_dst(x, p, y, p)` for each path `p` of `y` that is not
`w` or under it, which drives both `locals` and `context_locals`; call targets cross it the same
way. The index saves it (`update_sans_path.parquet`, index format 6), and both query engines
follow it. Paths above `w` (the whole variable) still carry everything. It applies to every
`Update`, flowy's included; `tests/tnt/update.tnt` checks the overwrite.

- **The default is unchanged:** with `--frame store` there are no updates; the regression suite
  passes 46 of 46.
- **Under `--frame update` the rule first lost two flows, `structptr` and `defaultmodels`,** to a
  frontend artifact; fixed below. With the fix the suite passes 46 of 46 under both settings.

### Taking a frame address no longer writes the frame

Taking a stack address, `unique = PTRSUB(RSP, -16)` for `&h`, gives a temporary that constant
propagation resolves to `__stack_top + -16`. As the PTRSUB's destination, `convert_vnode` turned
that temporary into the slot's address, and the write became a write of the slot's address into
the slot itself: `__stack_top.[-16].deref := __stack_top.[-16]`, before each call passed `&h`. The
stack pointer, and a register holding `p + 1` for a `p` into the frame, did the same. As a
`Store` each only added a value; as an overwrite it killed what a callee had written to the slot
(`set_data(&h, source())` in `structptr`, `strcpy(buf, ...)` in `defaultmodels`).

`push_assign_or_store` now drops a write whose destination is a frame address
(`__stack_top.[k]`, no `.deref`): every read of such a varnode resolves to the address anyway. It
removes 499, 1,007, 3,265 and 293 frame writes from `uclited`, `ntfs-3g`, `androidudpbus` and
`glxy`, and changes nothing else in their IR (`6b0ab678`).

It changes today's default too, by a lot where the artifact fed a hub. Same settings as below,
110 GiB guard; the frontend's IR differs from the earlier runs only by the dropped writes:

| App | `--frame` | `--mem2reg` | `locals` | Fixpoint | Peak | Against budget | Before the fix |
|---|---|---|---|---|---|---|---|
| `uclited` | `store` | `window:8` (default) | 40.4 M | 10.1 s | 1.24 GiB | 6.7x over | 41.4 M, 1.21 GiB |
| | `update` | `window:8`, skips | 211.9 M | 41.7 s | 12.95 GiB | 69.5x over | 212.2 M, 13.39 GiB |
| | `store` | `off` | 188.8 M | 31.7 s | 10.96 GiB | 58.8x over | 188.8 M, 10.94 GiB |
| `ntfs-3g` | `store` | `window:8` (default) | **5.0 M** | 2.8 s | **0.50 GiB** | **1.0x** | 283.8 M, 14.93 GiB |
| | `update` | `window:8`, skips | 15.9 M | 6.4 s | 0.88 GiB | 1.7x over | 667.9 M, 32.80 GiB |
| | `store` | `off` | 188.6 M | 147 s | 11.58 GiB | 23.0x over | 676.3 M, 47.70 GiB |
| `glxy` | `store` | `window:8` (default) | 85.0 M | 36 s | 3.79 GiB | 2.7x over | 100.4 M, 4.10 GiB |
| | `update` | `window:8`, skips | 86.6 M | 35 s | 3.81 GiB | 2.7x over | 89.7 M, 3.85 GiB |
| | `store` | `off` | 93.6 M | 44 s | 4.26 GiB | 3.0x over | 109.3 M, 4.61 GiB |
| `androidudpbus` | `store` | `window:8` (default) | 289.6 M | 93 s | 9.88 GiB | 3.8x over | 337.9 M, 13.31 GiB |
| | `update` | `window:8`, skips | 283.6 M | 677 s | 22.36 GiB | 8.5x over | no fixpoint in 1800 s, 92 GiB |
| | `store` | `off` | 1,544.3 M | 677 s | 47.00 GiB | 17.9x over | 1,639.3 M, 47.88 GiB |

"Before the fix" is the table further below; its `update` rows are also from before step 5.
Every run reports the same findings as `window:8` before the fix (79, 75, 1 and 1).

- **`ntfs-3g`'s blowup was the artifact.** Its default falls 57x in rows and 30x in peak, to
  budget. A slot that held its own address handed the frame address to everything read from it,
  and through the address the rest of the frame. Whether all of the drop is in the hub
  `FUN_0001f97c` (Current state) was not checked.
- **`androidudpbus` falls 14% in rows and 26% in peak** at the default, and `glxy` 15% and 8%.
  `uclited` hardly moves.
- **With the fix and the overwrite rule, `update` beats `off` everywhere,** which is its fair
  comparison while mem2reg skips its functions: 12x fewer rows on `ntfs-3g`, 5.4x on
  `androidudpbus`, 8% on `glxy`. Only `uclited` is still above `off` (12%).
- **It does not yet beat today's default** except to tie on `glxy`. On `androidudpbus` its
  `locals` equals the default's, but there are 17 M distinct `(variable, path)` against 1 M (16
  rows per path rather than 284), and the relations that key each path grow with them:
  `locals_key` 58.5 M against 2.8 M, `ext_dst` 17.8 M against 1.8 M, `reach_vp` 17.3 M against
  1.0 M. That is the versions × paths volume: each frame version carries its own copy of every
  slot it does not write. Step 3, promoting the slots no escape reaches, is what removes it
  (see "Step 3" above).

### After merging `main`'s interning (#170)

`main` `ebd22e01` interns paths, strings, call targets and decision sets as 4-byte handles. The
merge needed one change for this branch: `update_sans_path` joins the relation dump
(`CTADL_DUMP_RELATIONS`). Built before (`6b0ab678`) and after (`284d2637`) the merge, on the same
imports, same settings as the table above:

| App | `--frame` | `--mem2reg` | Fixpoint | Peak | Against budget | Before #170 |
|---|---|---|---|---|---|---|
| `uclited` | `store` | `window:8` (default) | 9.9 s | 0.65 GiB | 3.5x over | 10.1 s, 1.24 GiB |
| | `update` | `window:8`, skips | 41.4 s | 5.03 GiB | 27.0x over | 41.7 s, 12.95 GiB |
| | `store` | `off` | 34.0 s | 5.08 GiB | 27.3x over | 31.7 s, 10.96 GiB |
| `ntfs-3g` | `store` | `window:8` (default) | 2.9 s | 0.39 GiB | under (0.8x) | 2.8 s, 0.50 GiB |
| | `update` | `window:8`, skips | 5.9 s | 0.60 GiB | 1.2x over | 6.4 s, 0.88 GiB |
| | `store` | `off` | **230 s** | 8.11 GiB | 16.1x over | **147 s**, 11.58 GiB |
| `glxy` | `store` | `window:8` (default) | 36 s | 2.25 GiB | 1.6x over | 36 s, 3.79 GiB |
| | `update` | `window:8`, skips | 34 s | 2.28 GiB | 1.6x over | 35 s, 3.81 GiB |
| | `store` | `off` | 41 s | 2.46 GiB | 1.7x over | 44 s, 4.26 GiB |
| `androidudpbus` | `store` | `window:8` (default) | 93 s | 6.53 GiB | 2.5x over | 93 s, 9.88 GiB |
| | `update` | `window:8`, skips | 652 s | 13.65 GiB | 5.2x over | 677 s, 22.36 GiB |
| | `store` | `off` | 651 s | 21.83 GiB | 8.3x over | 677 s, 47.00 GiB |

- **Nothing the index derives changes:** all 57 relations' counts in `profile.json` are equal in
  every run, and the findings are identical (79, 75, 1 and 1). No path, string or call target is
  interned during the fixpoint (`fixpoint_interned` is 0), so the overwrite rule's
  `is_extension_of` test stays read-only.
- **Peaks fall 22-61%,** least on `ntfs-3g`'s default and most on `uclited` with `update`. The S29 comparisons keep their order: `update` still beats `off` everywhere but
  `uclited`, and on `androidudpbus` costs 2.1x the default's peak (2.3x before).
- **Time is unchanged within 8%, except `ntfs-3g` with `off`, 56% slower** (and 57%, 142 s to
  223 s, on a second pair of runs). All of it is the formal-side `locals` rule, the `edge_split` join that
  concatenates `p4 · rest` through the path set: 83 s to 157 s (13 s to 20 s for its other
  delta). That rule is the same on `main`, so it is #170's concatenation cost on this shape, not
  the merge. No default run shows it.
- **The regression suite** passes 46 of 46 under `--frame store` and `CTADL_FRAME=update`.

Merging `4490f7bf` next (#168, splitting R8-merged classes in the Dex frontend) bumps the import
format to 10, so every import must be re-imported (`CTADL_REUSE_FACTS=1 ctadl import --force`
reuses Ghidra's facts). At the default, `uclited`, `ntfs-3g` and `androidudpbus` are unchanged
(every relation count and finding). `glxy` has one merged class, which the frontend splits into
4 ids: 29 relations move by at most 90 rows (`locals` 84,999,293 to 84,999,254), its one finding
is the same, and its peak is 2.28 GiB (2.25 before, within noise). The suite passes 46 of 46
under both `--frame` settings.

### Coarsening gives today's results

Checked against the branch before S29 (`3ff13aa0`) on `uclited`, `ntfs-3g`, `androidudpbus` and
`glxy`:

- **The IR is the same.** Each binary's import, with each frame update rewritten textually into a
  store, is line for line the old import (`ctadl inspect --dump-ir`): 4,950, 17,259, 208,538 and
  11,088 frame updates, and no other `Update`. Both imports have to read the same Ghidra facts:
  two Ghidra runs on the same binary give different facts (the order of instructions with the
  same index in a block, and of a block's out-edges), so separate imports differ in a few
  functions either way. Import once, copy the import's `facts` into the other store, and re-import
  there with `CTADL_REUSE_FACTS=1 ctadl import --force`.
- **The index is the same:** all 56 relations' counts in `profile.json` are equal, and so are the
  findings (79, 75, 1 and 1).
- **The regression suite** (`cargo xtask regression --frontend pcode,jni`) passes 46 of 46, with
  `--frame store` and with `CTADL_FRAME=update` alike.

### What keeping `Update` costs (before step 5 and the PTRSUB fix)

Same machine and settings as Current state (macOS arm64, one run at a time, peaks from `time -l`,
budget 100x the IR: `uclited` 2.0 MB, `ntfs-3g` 5.4 MB, `glxy` 15.1 MB, `androidudpbus` 28.2 MB),
55 GiB guard, 1800 s timeout. `store` is today; `update` keeps every frame update, so mem2reg
skips those functions, and its fair comparison is `store` with `--mem2reg off`:

| App | `--frame` | `--mem2reg` | `locals` | `edge_split` | Fixpoint | Peak | Against budget |
|---|---|---|---|---|---|---|---|
| `uclited` | `store` | `window:8` (default) | 41.4 M | 0.2 M | 10.6 s | 1.21 GiB | 6.5x over |
| | `store` | `off` | 188.8 M | 0.7 M | 40.5 s | 10.94 GiB | 58.7x over |
| | `update` | `window:8`, skips 115 functions | 212.2 M | 0.7 M | 42.2 s | 13.39 GiB | 71.9x over |
| `ntfs-3g` | `store` | `window:8` (default) | 283.8 M | 68.5 M | 220 s | 14.93 GiB | 29.7x over |
| | `store` | `off` | 676.3 M | 233.9 M | 1187 s | 47.70 GiB | 94.8x over |
| | `update` | `window:8`, skips 288 | 667.9 M | 142.9 M | 377 s | 32.80 GiB | 65.2x over |
| `glxy` | `store` | `window:8` (default) | 100.4 M | 3.3 M | 44 s | 4.10 GiB | 2.9x over |
| | `store` | `off` | 109.3 M | 3.7 M | 50 s | 4.61 GiB | 3.3x over |
| | `update` | `window:8`, skips 44 | 89.7 M | 3.3 M | 40 s | 3.85 GiB | 2.7x over |
| `androidudpbus` | `store` | `window:8` (default) | 337.9 M | 14.2 M | 161 s | 13.31 GiB | 5.1x over |
| | `store` | `off` | 1,639.3 M | 25.1 M | 853 s | 47.88 GiB | 18.2x over |
| | `update` | `window:8`, skips 503 | | | timed out at 1800 s | 91.8 GiB or more | 35x or more |

The `store`/`window:8` rows are equal to the pre-S29 runs (`3ff13aa0`) in rows; their times
and peaks are within 2%. Another index ran beside the last 6 minutes of `androidudpbus`'s `off`
run, so its time is loose. Every run that finished reports the same findings as `window:8`.

- **Without steps 3 and 5, `update` is a precision change that does not pay for itself.** On
  `uclited` it is 12% more `locals` and 22% more peak than `off`; against today's default it is
  5.1x the rows and 11x the peak, because it also loses mem2reg. On `androidudpbus` it
  outgrows `off` by far: still short of the fixpoint at the 1800 s timeout, at 91.8 GiB (under a
  110 GiB guard; the 55 GiB guard killed it at 970 s), against `off`'s 47.9 GiB and 853 s. A
  timed-out run on this branch keeps no `locals` count.
- **On `ntfs-3g` it beats `off`, though not today.** `locals` is the same as `off`'s (-1%), but
  the context relations are 39-44% smaller (`edge_split` 143 M against 234 M, `assign_like` 35 M
  against 56 M, `summary` 4.1 M against 7.2 M), so the peak is 31% lower and the fixpoint 3.1x
  faster.
- **`glxy` is cheaper under `update` than today:** 11% fewer `locals` than `window:8` and 6%
  less peak. Its hub's rows are mostly not from the stack (Current state), so losing mem2reg
  costs it little.
- **Why `update` costs rows:** lowered as today, each version `st_n` is a copy of `st_{n-1}` plus
  one slot, so every path the frame ever held rides the copies into every later version, and a
  loop-head phi merges them. The frame still mixes, now with one variable per version, so the
  rows grow with versions × paths. The overwrite rule (step 5) is what stops a slot's old value
  from riding the copy past a write to it; mem2reg on `Update` (step 3) is what turns the slots no
  escape reaches back into scalars. These numbers are the baseline those two steps have to beat.
- **To reproduce:** import each binary with this branch, copy each pcode import's `facts`
  directory into a store for the old build and re-import there with
  `CTADL_REUSE_FACTS=1 ctadl import --force`, then compare `ctadl inspect --dump-ir` (frame
  updates rewritten into stores) and each `ctadl index` run's `profile.json`. Compare findings by
  rule, message and logical location name: the messages' site numbers (`call-arg(520, 0)`) and
  the location indexes change with the IR.

## Analysis: `uclited` has three causes, not one

`uclited` (see How to reproduce) was taken apart with the `index-instrument` branch's
instrumentation (`804d35cc`, "Index blowup instrumentation"; `INVESTIGATING.md` there is the
recipe), with S3 (`b1b52de8`) cherry-picked on top for the promotion rows. zlib's `inflate` holds
98-99% of `locals` in every configuration. It is a decoder state machine: a 32-way `switch` at the
loop head (block 10), reached by about 34 back edges.

| `uclited` on `076f6a60` | `locals` | Fixpoint | Peak (`time -l`) | Budget |
|---|---|---|---|---|
| `main` | 189 M | 35 s | 10.4 GiB | 56x |
| S3, `window:8` | 41 M | 10 s | 1.36 GB | 6.8x |
| every slot promoted (`exact`) | 15 M | 4 s | 0.68 GB | 3.4x |
| `inflate` skipped | 0.46 M | 0.3 s | 0.02 GiB | |

The `exact` row is the most any stack-slot fix (S4 included) can reach, and it is still 32x the
rest of the program. Three causes multiply:

1. **Stack slots (12.6x, P1 proper).** Most of the stores into the 17 slots are harmless: Ghidra's
   phis on a slot, lowered as loads of that slot merged and stored straight back (about 540 of the
   899 stores are such merges, 270 more are a load stored back to its own slot). A few slots take
   17-20 real values. The tell is `flush`, an `int`: on `main` 625 of `inflate`'s 1,013 sources are
   paths *into* it (`@p1.[k].deref`, `@p1.deref.[k].deref`, ...), because its value reaches pointers
   through the slots. With every slot promoted they are gone. S3 recovers 4.6x of the 12.6x; the
   rest (41 M to 15 M) is the escaped slots, S4's target.
2. **The state-machine loop is one cycle through most of the function.** With every slot
   promoted, `inflate`'s `assign_like` graph (variables only, paths dropped) has one strongly
   connected component of 4,941 of its 8,313 variables, and 79% of them are downstream of it. The
   cycle is ordinary SSA: loop-carried values (input and output pointers, bit buffer, counts)
   merge in the phis at the head. A binary operator is lowered as a merge of its operands
   (`x = a op b` becomes `assign x = a, b`), so a pointer combined with an integer (a table lookup
   indexed by the bit buffer) shares paths with it. Of 364 sources, the median reaches 54% of
   `inflate`'s 65,750 `(v, p)` and the top tenth 98%.
3. **Offsets pile up around the loop.** 242 of those 364 sources are four shapes
   (`@p0.deref.[k]`, `@p0.deref.[k].deref`, `@p0.deref.deref.[k]`, `@p0.deref.deref.[k].deref`)
   at about 60 offsets each: every byte 0-32, then sparse up to 152, plus 5552 and 4433408. The
   input pointer is incremented in the loop (`__src' = __src.[1]`), so each trip around the cycle
   adds 1 to the source path's offset. The only bound is the program-wide `path_set`: offsets 7,
   15, 142 and 143 never occur in `inflate`, only in other functions. Every variant of a shape
   reaches the same 64,746 `(v, p)`, so they multiply rows without adding information. Collapsing
   them would cut `locals` about 3x (estimated, not tried).

How the rows were attributed:

- `main`: a run stopped at 10 s (`CTADL_INDEX_TIMEOUT_SECS=10 CTADL_INDEX_DUMP=<dir>`; 35 M rows
  by the time it dumped, 1.9 GB of TSV), counted by function, variable, path shape and source.
- `exact`: a complete run, dumped the same way.
- The `exact` and `off` rows used a scratch `CTADL_MEM2REG=off|exact|window:N` override of
  `Pipeline::index_default`'s policy, which is now `ctadl index --mem2reg`.
- The SCC was computed over the `exact` dump's `assign_like.tsv`.
- The 10.4 GiB peak is from memguard. The others are from `time -l`: memguard samples too rarely
  on runs this short (it read 1.04 GiB for `window:8`).

## How to reproduce

### Quick: `uclited` from the firmware sweep (about a minute)

The benchmark runner (`bench/` in `ct-bench`) flags this binary as over its memory budget:
`firmware-sweep` benchmark `40c87ee3d0be0b74`, TP-Link `TL_WPA8630P_EU_UK_V2.1_171117`
`fs/usr/bin/uclited` in the Operation Mango `large_dataset` (MIPS32 big-endian, stripped, with
zlib linked in), SHA-256 `40c87ee3d0be0b7429e9c4bf2ef067223ef7fa3340ef9d8e24dee7f2699e7320`.
The sweep measured 10.9 GiB at `dfccbcab` (59x the budget). On `main` `076f6a60`:

| | Peak | Fixpoint | `locals` rows |
|---|---|---|---|
| as is | 10.4 GiB (56x budget, 2.0 MB IR) | 36 s | 189 M |
| zlib's `inflate` skipped | 0.02 GiB | 0.3 s | 0.46 M |

`inflate` (381 blocks) is the P1 function. Its loop keeps the decoder state in 17 stack slots
(`%__stack_top.[-96]` through `[-44]`; here the slots hang off `__stack_top` rather than `%L3_0`),
with about 60 stores and 175 loads per slot, 899 stores and 2,629 loads in all (most of them
Ghidra's phis on the slot; see Analysis). The two `locals` rules joined on `ext_dst` take 21 s of
the 36 s fixpoint. The import takes 18 s.

```sh
cargo build --release
export GHIDRA_HOME=...                    # Ghidra, JDK 21
M=<ct-bench>/firmware-eval/models/cmdi-firmware.json5   # the sweep's model
target/release/ctadl --store tmp/p1/store import -l pcode -n uclited <path>/uclited
target/release/ctadl --store tmp/p1/store index uclited -m $M   # under a 16 GiB cap
# profile.json: locals ~189 M rows, the ext_dst locals rules on top
cat tmp/p1/store/projects/uclited/index/profile.json
# confirm the hub function: skip inflate's body and locals falls ~400x
cat > tmp/p1/skip-inflate.json5 <<'EOF'
{ "model_generators": [ { "find": "methods",
    "where": [{ "constraint": "signature_match", "name": "inflate" }],
    "model": { "modes": ["skip-analysis"] } } ] }
EOF
target/release/ctadl --store tmp/p1/store index uclited -m $M -m tmp/p1/skip-inflate.json5
target/release/ctadl --store tmp/p1/store inspect uclited --dump-ir --function inflate
```

Other over-budget binaries from the same runner, if one more native shape is needed:
`firmware7/AC15-usr-bin-nginx` (Tenda, ARM; at least 15x the budget, killed at the 16 GiB cap
after about 2 minutes) and `firmware-sweep` `4bbd3a7ad4b4484f` (Netgear MBM621 `bin/chat`; 1.9 GiB,
17x, 30 s; `locals` 23 M rows). Neither looks like a stack problem; see Other firmware binaries. The runner's
`over-budget` events (`events` table of `bench.db`, or `./run.sh report`) list the rest.

### Full apps

Needs Ghidra (`GHIDRA_HOME` set, JDK 21) so that `ctadl import` disassembles the APK's `.so` files.
Use a memory cap: without one these runs can take the whole machine.

```sh
cargo build --release
export XDG_STATE_HOME=$PWD/tmp/p1/state            # a scratch store
target/release/ctadl import -l apk --name udpbus org.vi_server.androidudpbus_1.apk
# Linux: cap memory (no swap); a cgroup kill leaves the log up to that point
systemd-run --user --scope -p MemoryMax=32G -p MemorySwapMax=0 \
  env RUST_LOG='warn,ctadl=debug,ctadl_ir::ssa=info' \
  target/release/ctadl index udpbus 2> tmp/p1/udpbus.log
```

What to look for:

- `profile.json` in the index directory (`$XDG_STATE_HOME/ctadl/projects/<name>/index/`): every
  relation's tuple count, each scc's iterations and rule times, and whether the run reached the
  fixpoint or stopped at `CTADL_INDEX_TIMEOUT_SECS`. The same numbers are in the debug log.
- In `profile.json` or the log's scc time summary: the main fixpoint scc dominates, and the `locals` rules (driven by
  `ext_dst` and `edge_split`) take most of its rule time.
- The relation counts (`profile.json`, or the census after `ascent_run` in the log): `locals` in
  the hundreds of millions of rows.
- To get a profile from a run that does not converge, set `CTADL_INDEX_TIMEOUT_SECS=<n>` and
  repeat with growing `n`. On `main` a timed-out run loses the running scc's contents and timing;
  `improve-native-handling` vendors a patched `ascent_macro` that keeps them, adds per-rule
  derived/new row counters, and `scripts/blowup-rank.py` ranks the result.
- To see the slots, `ctadl inspect udpbus --dump-ir --function FUN_00144f04` (for `androidudpbus`)
  and count the stores and loads on `%L3_0.[k].deref` paths.

For comparison, the same APK with `--no-native-libs` (Dex only) indexes in seconds.

## Ideas for solutions

**S3, done on `improve-native-handling` (`b1b52de8`): stack-slot promotion (mem2reg).** Before
SSA, give each `__stack_top.[k].deref` slot its own local (`__stack_m16` for offset -16) and
rewrite its loads and stores into copies, so SSA versions the slot like a register. A slot whose
address escapes (passed to a call, stored, copied or returned) must stay in memory, since the
escaped address plus an offset can reach it. The escape policy decides how far an escaped address
reaches:

- `exact` (only the slot itself): loses real flows (`nightly/tests/c/example.c`, one `structret`
  result). Rejected.
- `window:8` (slots `j .. j+8`): loses nothing on the suite, the fastest safe policy. The default.
- `above` (every slot above `j`): promotes almost nothing. Rejected.

On `androidudpbus` S3 took 849 s / 52 GB down to 163 s / 15 GB. It leaves the apps above 5-15x
over budget, because **the slots that stay in memory are the busiest ones**: in `FUN_00144f04`,
23 k accesses stay in memory across 35 slots, each taking about 300 stores and 580 loads, the same
profile as before. Ghidra can't bound these regions (it recovers 4-8 byte high variables, not the
Rust structs behind them).

**S4, tried on `improve-native-handling` (`aa0b4cda`) and dropped: promote escaped slots and
write them through.** Every store to an escaped slot also went to memory, and the local was
reloaded after each call. It doubled `androidudpbus`'s `locals`: writing every direct store
through brings back, for every slot, the deep paths memory had been dropping (P24). Where escapes go, in
`androidudpbus`: 1,714 frame addresses passed to calls (1,697 to functions in the library, 132 of
them to `memcpy`, 17 through function pointers), 3,118 stored to memory and 1,291 copied inside the
function. The calls are only 28%, so the spill points have to cover stores and copies too.

**S5, on this branch: full promotion of large functions (`--mem2reg full`, off by default).** In a
function with more than 1,000 direct stack accesses, promote every directly accessed slot, escaped
or not. Direct stores go only to the local; after a call passed an address that reaches the slot
(under `window:8`), reload the local from it. Nothing is written back before a call, so a callee or
a copy of the address no longer sees what the function stored directly. Each function where that
may lose a flow is named in the index's `notifications.log` and, as a `CTADL0104` warning, in the
query's SARIF. A slot a returned address reaches stays in memory (the pcode frontend returns a
stack-held value as the slot's address). Model modes `mem2reg-full` and `mem2reg-window` force or
forbid it for one function. See Current state for what it does to the apps.

**S29, steps 1, 2, 3 and 5 on this branch: the pcode frontend emits `Update` for the frame, and
passes coarsen it.** Step 4 is SSA itself; what is done and what it costs is under S29 below. A frontend should emit the most precise IR, since a later pass can drop
information but not recover it. Today pcode writes the frame with `Store`, which defines nothing, so SSA sees one
frame for the whole function, and mem2reg has to rebuild the order of stack accesses and guess
how far an escaped address reaches (`window:8`). `Update` (restored in #118, emitted today only by
flowy) defines its destination. A frame write emitted as `st = update (st.[k].deref := v)` makes
the frame a value SSA can version: each write becomes a fresh `st_n`, each load and call reads
the version that reaches it, and joins get `st_k = phi(...)`. The pipeline:

1. The pcode frontend emits `Update` for every write to `__stack_top`.
2. Optionally, per function, coarsen to `Store`: `st = update (st.[k].deref := v)` becomes
   `store st.[k].deref := v`, a syntactic rewrite. It must run before SSA, after which the versions
   are separate variables. This replaces `full:N`'s threshold as the cost valve: keep `Update` where
   it is affordable, coarsen in a hub if the versions cost too much. Model modes can force either.
   Coarsening every function gives exactly the IR pcode emits today, so today's pipeline is this
   one with step 2 set to "coarsen all". That is the first check of a frontend change (the same
   rows and flows as today) and the default until `Update` proves itself. Writes through a copy of
   the frame address (`%stack = %__stack_top`, then `store %stack.[k].deref`) do not write `st` and
   stay `Store`s either way.
3. Optionally, promote the slots no escape reaches (mem2reg). With a versioned frame an escape is a
   use of one `st_n`, so no window is guessed; promotion only saves cost, since a scalar is
   cheaper than a frame version. mem2reg skips any function that defines `__stack_top` today, so
   it has to learn `Update` first.
4. SSA conversion.
5. The index lowers `Update` with an overwrite rule. Today it lowers `x = update (y.f := v)` to the
   copy `x <- y` plus the write `x.f <- v`, so a slot's old value rides the copy into every later
   version. The copy has to leave out the written path (a constant, so a fixed filter on the copy
   rule, not general negation). Without it a loop-head phi merges every value a slot ever held, and
   the frame mixes again.

What each form finds in

```c
void f(void) {
  int x, y;           // x at frame offset -8, y at -4
  x = source_a();
  sink1(x);           // truly reached by: a
  x = 0;
  sink2(x);           // truly reached by: nothing
  y = source_c();
  use(&y);            // use reads *p; truly reached by: c
  x = source_b();     // truly reaches nothing
}
```

| | `sink1` (a) | `sink2` (none) | `use` (c) | Escape guess |
|---|---|---|---|---|
| `Store` (today) | a, **b** | **a, b** | c | none |
| `Update` + SSA, lowered as today | a | **a** | c | none |
| `Update` + SSA, with the overwrite rule | a | none | c | none |
| promoted, `window:8` | a | none | c | right here; silently wrong if `use` read `p[-1]` |
| promoted, `full` | a | none | **none** (notified) | none, but loses inputs passed by pointer |

With `Store`, every load of a slot sees every store to it, earlier or later. Under `Update` and
SSA, `use` is handed the exact version `st_3` that holds `y`, and a callee or a copied address
that writes through it writes into `st_3`, which the later copies carry forward.

What it would and would not do:

- **Precision and soundness, not memory, are what it buys.** It replaces `window:8`'s guess and
  `full`'s lost flows. It does not fix `inflate`, which holds 98% of `uclited`'s `locals` with
  every slot promoted, so what is left there is not the stack.
- **It already paid once, indirectly.** The overwrite rule exposed the frontend writing taken
  frame addresses into the frame, and fixing that brought `ntfs-3g` to budget at the default (S29
  above).
- **The cost is measured** (S29 above). With steps 1, 2, 3 and 5 and the PTRSUB fix, `update`
  with `window:8` beats today's default on `androidudpbus` (under budget), `glxy` and `ntfs-3g`,
  and ties on `uclited`, with the same findings. Step 2 is the valve, and it stays on (coarsen
  all) until `update` beats today; it now does on these four.
- **Next:** decide whether `--frame update` becomes the default, after the regression suite's
  wider frontends (`android-native`) and more native binaries; and measure `full` under
  `update`. Making the escaped slots sound (`above`) at an affordable cost needs a better bound
  on what an escaped address reaches than "every slot above it".

**A mitigation on `main`: `ctadl index --lsm`** (experimental, #155) keeps `locals` and the other
large relations in memory-mapped run files, read through the page cache, so a run that would not
fit in RAM can finish, more slowly. It changes where the rows live, not how many there are, and it
has not been tried on these apps.

**Other directions, not explored:**

- Model `memcpy` and similar copies precisely (offset ranges rather than whole-slot mixing).
- A field-insensitive collapse for the few hub functions that hold most of `locals` (bounded
  precision loss, bounded rows), triggered by a size threshold.
- Summarize such a function with a coarse summary and skip its body, as S6 (the
  `skip-exclusive-callees` model mode, on `improve-native-handling`) does for libc++abi's
  demangler.

**How to check a fix:** the regression families `pcode` and `jni` (which includes the APK
packagings) must keep every flow: `cargo xtask regression --frontend pcode,jni`. For a policy
that may lose flows, `--mem2reg full:0` runs every case under it, and reports a lost flow as an
XFAIL when an index notification names a function, and as a FAIL when none does. Watch for silent losses: `exact` lost `example.c`'s only flow while the suite still
passed, because on `main` that case is a SKIP on Darwin ("no tainted instructions on Darwin");
S28 makes it a FAIL. For a change to frame updates, run the suite again with `CTADL_FRAME=update`:
the default coarsens every update, so it cannot see one. Then the apps above: peak against
budget, `locals` rows, and the share held by the largest function.

## Samples

All from F-Droid (`https://f-droid.org/repo/<file>`). The measurements above used the versions in
the original corpus, which may be older than these; the problem is in the libraries, which these
versions still carry.

| App | Version | File | SHA-256 | Size |
|---|---|---|---|---|
| `org.vi_server.androidudpbus` | 1.0 | `org.vi_server.androidudpbus_1.apk` | `e6085f344fcd764625a9f4491c0aa6803256d754feb3bf1d0797ad2169cfc87f` | 0.5 MB |
| `com.kaeruct.glxy` | 2.3 | `com.kaeruct.glxy_23.apk` | `05838298c9d70edff22dcc3590c287a36bf9cd62fc73b64beb2bbc24ba281bca` | 1.3 MB |
| `com.github.msx80.retrodrawing` | 0.0.4 | `com.github.msx80.retrodrawing_4.apk` | `ebe6d46ae45d16ba78b6ea5196d69251caec0b6327da13a6ed518a5cf357f414` | 1.4 MB |
| `app.halma` | 15.0 | `app.halma_15.apk` | `b994337244e2136b54f96f75038fe70d9562635cce50af6387d906ee048d0ec0` | 3.8 MB |
| `fir.tube` | 11.0 | `fir.tube_11.apk` | `4e88fc405193043e0b09d9111e1f833c4668d803fba2cabedef048e934598f8b` | 1.2 MB |
| `com.zoffcc.applications.avifview` | 1.0.4 | `com.zoffcc.applications.avifview_10004.apk` | `f17aafcaf79285557051aa09e380051a0f4a0e10e60fda7b49608f92ed8ed8da` | 5.2 MB |
| `org.mattvchandler.a2050` | 1.0.10 | `org.mattvchandler.a2050_190010010.apk` | `98a0e75e589c319093db56cf98bfa32d920b9436a9cbe7c30b32dcf7a4a6d284` | 5.1 MB |

Not on F-Droid: `com.whatsapp_2.17.79-451676_minAPI14.apk` (29 MB, 4.1 MB `libwhatsapp.so`),
SHA-256 `84444d1d222dcc9b8b7aceccf59bd71038be5e84336d888047705da56cedc00a`.

For a quick run, use `uclited` (How to reproduce). Of the apps, start with `androidudpbus` (smallest, the best-studied function) and `glxy` (the `libgdx` shape).

```sh
mkdir -p samples && cd samples
for f in org.vi_server.androidudpbus_1.apk com.kaeruct.glxy_23.apk; do
  curl -fLO https://f-droid.org/repo/$f
done
sha256sum *.apk   # compare against the table
```
