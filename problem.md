# P1: stack slots mix every value in a native function - DO-NOT-MERGE

The largest memory problem on native code. In a big native function, the index's `locals`
relation (which access paths reach which variables) grows to hundreds of millions of rows,
because the stack frame acts as one function-wide mixing pool. Apps with this shape need 5-15x
the memory budget (100x the imported IR), and before any fix they did not finish at all.

This branch is off `main` (merged up to `7c98b73e`). It has S3 (stack-slot promotion, `window:8`
by default) and S5 (full promotion of large functions, behind `ctadl index --mem2reg full`);
`design.md` has S5's design and measurements. `improve-native-handling` has the earlier record:
`things-to-improve.md` there covers P1, S3, S4, M1 and M7.

## Starting point

- **S3** (`b1b52de8` on `improve-native-handling`) is cherry-picked here, with S28 (the regression
  suite fails on a lost flow instead of skipping on Darwin).
- **S5** is on this branch. `ctadl index --mem2reg <policy>` (or `CTADL_MEM2REG`) picks the
  policy: `window:N`, `exact`, `above`, `full[:N]` or `off`.
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

The table above is from before S3 was re-measured on `main`. On this branch (`main` `7c98b73e`
with S3 and S5), on macOS (arm64), one run at a time, peaks from `time -l`:

| App | `--mem2reg` | `locals` | Fixpoint | Peak | Largest function's share |
|---|---|---|---|---|---|
| `uclited` | `window:8` (default) | 41.4 M | 10.2 s | 1.26 GiB | `inflate` 99.3% |
| | `full` | 12.1 M | 3.0 s | 0.54 GiB | `inflate` 97.7% |
| `androidudpbus` | `window:8` (default) | 337.9 M | 166 s | 13.27 GiB | `FUN_00144f04` 71.7% |
| | `exact` | 349.7 M | 143 s | 15.11 GiB | |
| | `full` | 30.9 M | 22 s | 2.09 GiB | `FUN_00156cb8` 35.8% |
| `glxy` | `window:8` (default) | 100.4 M | 44 s | 4.07 GiB | `FUN_0010b408` 90.2% |
| | `exact` | 98.2 M | 44 s | 4.01 GiB | |
| | `full` | 83.7 M | 38 s | 3.64 GiB | `FUN_0010b408` 88.8% |

The shares are from the census build before the merge (`design.md`), on row counts within 5% of
these. `uclited`'s `window:8` row is from before the merge too; its `full` row was re-run after.
The `exact` rows were run after the merge without the census, so they have no share.

`exact` (S3's rejected policy) does not help the apps. On `androidudpbus` it keeps 29% fewer
accesses in memory than `window:8` (121 k against 170 k) but `locals` grows 3.5% and the peak
1.8 GiB: the slots an address escapes from, which `exact` still keeps, are the busy ones, so
promoting their neighbors removes nothing. On `glxy` it cuts 2%. Only `full` moves either app.

What is left of P1:

- **`uclited`:** `inflate` still holds 98% under `full`. Its stack is done; causes 2 and 3 of the
  Analysis below (the loop's SCC, the offsets) are what remain.
- **`glxy`:** promoting `FUN_0010b408` fully cuts only 16%, so its rows are not from the stack.
  P6 is the suspect.
- **`androidudpbus`:** the hub `FUN_00144f04` drops from 243 M rows to 0.2 M. The largest function
  is now `FUN_00156cb8` (11 M rows), below the 1,000-access threshold, so it still runs
  `window:8`; a lower threshold may shrink it.
- **The other apps in the table above** have not been run with `full`.

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
after about 2 minutes, not analyzed here) and `firmware-sweep` `4bbd3a7ad4b4484f` (Netgear
MBM621 `bin/chat`; 1.9 GiB, 17x, 30 s; `locals` 23 M rows). The runner's
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
S28 makes it a FAIL. Then the apps above: peak against budget, `locals` rows, and the share held by the
largest function.

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
