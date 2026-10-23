# P1: stack slots mix every value in a native function - DO-NOT-MERGE

The largest memory problem on native code. In a big native function, the index's `locals`
relation (which access paths reach which variables) grows to hundreds of millions of rows,
because the stack frame acts as one function-wide mixing pool. Apps with this shape need 5-15x
the memory budget (100x the imported IR), and before any fix they did not finish at all.

This branch is off `main`. The partial fix (S3 below) and the measurements are on
`improve-native-handling`; `things-to-improve.md` there has the full record (P1, S3, S4, M1, M7).

## Starting point

`main` lacks S3 (stack-slot promotion), which takes the worst app from 52 GB to 15 GB. Work on the
open part of P1 (the escaped slots, S4) should start from it:

```sh
git cherry-pick b47ed139 b1b52de8
```

Hashes are on `improve-native-handling` as rebased onto `main` `076f6a60`; the subjects are given
too, since a later rebase changes the hashes. Each cherry-pick below was tried on `076f6a60`.

- `b1b52de8` "Promote stack slots to locals before SSA (mem2reg), window:8 by default": S3, its
  tests, and S28 (the regression suite fails on a lost flow instead of skipping on Darwin).
- It applies cleanly after `b47ed139` (below). On its own it conflicts in
  `ctadl-ir/src/ssa/mod.rs`, only on `b47ed139`'s dead-phi lines: keep `main`'s side and add the
  `if let Some(policy) = p.mem2reg { promote_stack_slots(program, policy); }` call before
  `transform_program`.
- Optional, for native apps that also link libc++abi's demangler (P4, not P1):
  `bb9cc766` "Model __cxa_demangle and skip the helpers only it reaches" (S6).
- `main` changed SSA (#154, every parameter written back at its entry version) and C pointers
  (#150) since S3 was measured. The cherry-pick is clean, but the M1/M7 numbers above have not been
  re-measured on top of them; re-run `androidudpbus` and `glxy` first.

**Optional measurement tooling** (none of it is on `main`):

```sh
git cherry-pick b47ed139   # vendors a patched ascent_macro (a timed-out run keeps the running
                           # scc's contents) and adds CTADL_LOCALS_CENSUS and CTADL_DEAD_PHIS
git cherry-pick 5858fa78   # "Log the size of every relation and index" ([idxsizes])
git cherry-pick 918c771a   # "Count each rule's rows derived and new, and log each iteration"
                           # (per-rule counters, CTADL_ITER_LOG)
git cherry-pick c1ee3d61 f9e5b24a   # scripts/blowup-rank.py, and the Windows memguard/ladder
```

These apply in that order; `f9e5b24a` conflicts only in `things-to-improve.md`, which `main`
does not have (drop it). `b47ed139` is titled "WIP-EXPERIMENT: dead-phi pass, locals
census, SSA/stack census examples"; its experiment hooks are off unless their variables are set.
On Linux, `systemd-run --user --scope -p MemoryMax=<N>G -p MemorySwapMax=0` stands in for
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
with about 60 stores and 175 loads per slot, 899 stores and 2,629 loads in all. The two `locals`
rules joined on `ext_dst` take 21 s of the 36 s fixpoint. The import takes 18 s.

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

**S4, not started: promote escaped slots anyway and spill them around their escapes.** Store the
local back to the slot before each escape or call, and reload it after a call. Direct accesses
between escapes are then in SSA, and only the values live at an escape mix. Where escapes go, in
`androidudpbus`: 1,714 frame addresses passed to calls (1,697 to functions in the library, 132 of
them to `memcpy`, 17 through function pointers), 3,118 stored to memory and 1,291 copied inside the
function. The calls are only 28%, so the spill points have to cover stores and copies too.

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

**How to check a fix:** the regression families `pcode`, `jni` and `android-native` must keep
every flow. Watch for silent losses: `exact` lost `example.c`'s only flow while the suite still
passed, because on `main` that case is a SKIP on Darwin ("no tainted instructions on Darwin");
`improve-native-handling` makes it a FAIL (S28). Then the apps above: peak against budget, `locals` rows, and the share held by the
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
