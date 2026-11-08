# Parallel import: status

Goal (intent.md): make `ctadl import` parallel, leaner on RAM, and keep its RAM use under
control, scaling with cores up to 16.

## Summary

**Done.** Everything is on branch `parallel-import` (not pushed, 60 commits over the intent
commit), one commit per idea; the
agents' work is merged (cherry-picked; pcode as a merge commit). Final checks on the merged
tip:

- `cargo xtask regression --release`, all front ends: **254 passed, 0 failed**, 5 expected
  failures (the ones already marked xfail).
- `cargo test --workspace --release`: all pass. `cargo clippy --workspace --all-targets` and
  `cargo fmt --check`: clean.
- **Byte-identical** to the baseline on all 19 Android apps below (program and source info;
  VMT compared decoded; program re-encoded from the new framed format), on 2 .NET
  assemblies, 10 jars and a firmware binary (same Ghidra facts). Same bytes at `-j 1`,
  `-j 3`, `-j 16`. The baseline used for this has one one-line bug fix applied (see Findings).

### Final sweep, Dex/APK import (`--no-native-libs`, final tip vs baseline, load ~15–30)

| app | before | after | instructions | peak memory |
|---|---|---|---|---|
| WhatsApp Business (XAPK) | 93.9 s | 5.6 s | 1424 G → 200 G | 6.8 → 4.4 GB |
| WhatsApp Messenger (XAPK) | 85.5 s | 5.6 s | 1306 G → 195 G | 6.3 → 4.3 GB |
| Messenger | 46.3 s | 5.1 s | 607 G → 185 G | 6.8 → 4.5 GB |
| TikTok (XAPK, 1.9M functions) | 105.5 s | 21.8 s | 1162 G → 525 G | 20.9 → 14.7 GB |
| TikTok Lite | 13.2 s | 2.8 s | | |
| Telegram (XAPK) | 11.0 s | 3.4 s | | |
| VLC (XAPK) | 8.0 s | 2.8 s | | |
| Chrome (XAPK) | 6.1 s | 0.9 s | | |
| FX File Explorer | 3.6 s | 0.6 s | | |
| 9 F-Droid apps + noto | 1.9–15.3 s | 0.5–4.3 s | | 25–35% lower |

Other front ends (details below): .NET Mono.Android 45 s → 1.6 s; a 67-assembly .NET app
104 s → 4.3 s; JVM rt.jar 17.7 s → 1.7 s, a 105 MB fat jar 229 s *and failing* → 3.6 s;
firmware (pcode) after Ghidra: AC15 httpd 29.6 s → 4.6 s, Telegram's 24 MB native library
454 s → 39 s; Messenger *with* its 13 native libraries 409 s → 97–117 s.

### Scaling with `-j` (best of 2, load 13–19)

| input | 1 | 2 | 4 | 8 | 16 |
|---|---|---|---|---|---|
| WhatsApp split (Dex) | 15.8 s | 9.5 s | 7.0 s | 5.7 s | 5.6 s |
| Mono.Android.dll (.NET) | 6.1 s | 3.8 s | 2.4 s | 1.9 s | 1.6 s |
| soot-trunk.jar (JVM) | 1.7 s | 1.05 s | 0.8 s | 0.56 s | 0.55 s |

Default is min(cores, 16). Past 8 threads the remaining serial steps dominate: placing results
in order (interning spans, whose ids are first-seen order), per-file bookkeeping, and writing
source info as parquet (one thread per table). Single-threaded, the imports are already 4–7×
faster than before.

### What changed, by effect

1. **Merged-class clones** (Dex): each clone was a full lowering of the merged body followed
   by pruning; now one lowering is a template and each clone copies only the blocks its arm
   reaches (4.7× fewer instructions on its own).
2. **Allocation churn**: a Dex import made ~970M allocations (57 GB) for a ~6 GB result.
   Strings, method and field names decoded once and cached; names built on the stack;
   compact source-info interning (~5 bytes per row instead of ~50); exact-size blocks.
   Similar work in the JVM, .NET and pcode front ends, plus several quadratic lookups removed
   (JVM extern stubs, .NET type-by-name, pcode call targets).
3. **Parallel lowering** in every front end on one design: bookkeeping in file order,
   bodies lowered in parallel, results placed in order so every id is assigned exactly as
   before. Parallel too: archive inflation, scans, verification, writing.
4. **Framed program format** (import format **15**; old imports are refused with the usual
   "re-import it"): encoded/decoded in parallel and streamed to disk, ~1.6 GB off the
   WhatsApp peak.
5. **Managed concurrency for native code and XAPK splits**: a shared core + memory budget
   (`CTADL_IMPORT_MEMORY`, default half of RAM), Ghidra heaps sized per library, Ghidra
   out-of-memory detected and retried instead of silently exporting a partial program.

### Findings / bugs fixed on the way

- `ir-vmt.bitcode` differed on every run (hash-map order) — now deterministic.
- JVM imports numbered functions differently run to run — now deterministic.
- Dex `move-result`/`move-exception` could read a local index left over from the *previous*
  method; now reset per body. (This is the one-line fix also applied to the comparison
  baseline, so it is the only intended output change.)
- Fat jars with multi-release duplicate classes failed to import; now one definition is kept.
- Ghidra keeps going after `OutOfMemoryError` and exports a partial program; now retried.
- Ghidra's own output depends on its thread count, so native-library imports are not
  byte-for-byte reproducible across machines (they weren't before either).

### Review of the branch (code-review skill, medium), and what was done about it

- Fixed: Dex placing could drop the first of two definitions of one method in one dex file
  when both fell in the same batch (my bug; such a batch is now placed one method at a time).
- Fixed: XAPK splits and an APK's native libraries kept starting after a failure (Ghidra
  runs for nothing); now nothing new starts once a split or the Dex half fails.
- Fixed: a failed pcode lowering left facts both plain and gzipped.
- Reverted: memory-mapping jars (a jar truncated mid-import would SIGBUS the process).
- Fixed: `ctadl go` no longer caps the process's thread pool for indexing unless `-j` given.
- Cleaned up: one `Locals::get_or_intern_numbered` instead of two front ends' name buffers.
- Known, left as is: the program's store write runs on rayon's global pool rather than the
  import budget's (both sized by `-j`, so at most ~2× oversubscription while native libraries
  are written); and the Dex memory grant is released after lowering, not after the program is
  written, so with several Dex splits the budget can undercount for a few seconds.
- status.md is no longer committed (it names this machine's paths, which your CLAUDE.md keeps
  out of committed files): history was rewritten with `git filter-branch` to drop it; the
  branch before that is kept as `parallel-import-before-filter`.

### Decisions left for you

- `--features jemalloc` (opt-in, on ctadl-ascent): 13–21% less CPU on a Dex import; not
  default because its effect on indexing is unmeasured. Agents saw similar from mimalloc.
- Fact-table gzip level (pcode): now the largest in-process cost after Ghidra; level 1–5
  would cut it 30–60% for 4–77% bigger facts.
- Shrink `Statement` (120 bytes: `CallStyle` inline, `sources: SmallVec<[Exp; 2]>`): ~40% of
  a program's statement memory, for import *and* index; touches ~40 files.
- Parquet source-info writing could encode columns in parallel.
- Branch contains `e96d282 DO-NOT-MERGE: Add intent` at its base, as before.

Scratch space to clean up when done: `/Volumes/Shampoo/parallel-import` (runs, stores, inputs,
bins; includes a 28 GB copy of libtmessages facts under `pcode/facts-tg-plain`), git worktrees
`/Volumes/Shampoo/parallel-import/baseline-src` and `baseline-fix`, and the agents' worktrees
under `~/proj/ctadl-rs/.claude/worktrees/`.

## Details by area

Native libraries and XAPK splits (agent 1, merged): imported concurrently under a shared
budget of cores and memory (`ctadl-frontends/src/budget.rs`; cores = `-j` or min(cores, 16),
memory = `CTADL_IMPORT_MEMORY`, default half of RAM), served strictly in request order; a
request larger than the budget runs alone. Each Ghidra run gets a fixed share of cores by
library size and a heap of 3 GiB + 2 KiB per library byte (max 40 GiB, as before); a Ghidra
run that hits `OutOfMemoryError` — which Ghidra survives, exporting a partial program — is
now detected and retried alone with 40 GiB. `.so` files are streamed to disk instead of all
held in memory; an APK's Dex half runs alongside its native half (one Dex half at a time,
charged against the budget); XAPK splits start importing as soon as each is extracted.

| app (with native libraries) | before | after | peak tree RSS before → after |
|---|---|---|---|
| Messenger (13 libs) | 409 s | 97–117 s | 10.0 → 11.3–12.3 GB |
| DuckDuckGo (9 libs, 17.8 MB) | 888 s | 326 s | 28.1 → 18.9 GB |
| WhatsApp Business XAPK | 236 s | 66 s | 7.5 → 5.0 GB |
| Telegram XAPK | 1887 s | 1805 s | 45.0 → 43.6 GB |

Telegram is bound by one 24 MB library: ~22 min in Ghidra, then ~9 min of single-threaded
pcode lowering at ~33 GB RSS (passed to the pcode agent). Native-library outputs cannot be
byte-identical to the baseline: Ghidra's output depends on its thread count, and the baseline
does not even match itself run to run (agent 1 checked: same files differ between two
baseline runs; facts match as unordered sets except decompile-order ids).

.NET (agent 5, merged): parallel lowering on the Dex design; assemblies, PDBs and types
read in parallel; a type-by-name lookup that scanned every TypeDef row each time (quadratic)
now uses an index; the assembly cache remembers missing assemblies (each miss used to search
the reference directories again) and is shared between threads. Byte-identical at `-j 1` and
`-j 16`; `xtask regression --frontend cil`: 23 passed, 1 expected failure.

| input | before | after (`-j 16`) | `-j 1` | instructions | peak |
|---|---|---|---|---|---|
| Mono.Android.dll | 45 s | 1.8 s | 6.4 s | 363 G → 74 G | 1.60 → 1.48 GB |
| Microsoft.CodeAnalysis.CSharp.dll | 7.2 s | 0.8 s | 2.9 s | 86 G → 31 G | 0.88 → 0.80 GB |
| roslyn-ls directory, 67 assemblies | 104 s | 4.3 s | 19 s | 1270 G → 199 G | 3.79 → 3.32 GB |

JVM (agent 3, merged): the baseline numbered functions differently run to run (extern stubs
came out of a seeded hash map); now sorted, as Dex does. Then parallel lowering on the Dex
design; jar entries inflated and parsed in parallel, and the jar memory-mapped; a linear scan
of every VMT method per extern stub (82% of the old time on a fat jar) replaced by a set; a
jar that defines a class twice (multi-release copies under `META-INF/versions/`) keeps one
definition, with a warning, instead of failing verification. Identical to the
determinism-fixed reference on soot and 9 other jars, `-j 1` = `-j 16`;
`xtask regression --frontend jvm`: 66 passed, 1 expected failure.

| jar | before | after | peak |
|---|---|---|---|
| soot-trunk.jar | 3.3–6.2 s | 0.65 s | 788 → 603 MB |
| HeadlessAndroidScanner (105 MB fat jar) | 229 s, then **failed** | 3.6–4.7 s, 731,708 functions | 4.6 → 3.7–4.0 GB |
| rt.jar | 17.7 s | 1.7 s | 2.16 → 1.79 GB |

pcode in-process (agent 4, merged): DataFusion and Tokio replaced by a streaming
tab-separated reader that reads tables in parallel windows; constant propagation as a
worklist over numbered varnodes instead of a Datalog program (same fixpoint); functions
lowered in parallel, spans and temporaries numbered in the original order; a call-target
search that scanned every function per call replaced by a map (most of a big library's time);
fact compression runs alongside reading, in parallel 1 MiB pieces of one gzip member.
Byte-identical at `-j 1/4/16`; `xtask regression --frontend pcode`: 28 of 28. Checked again
after merging: lowering the same Ghidra facts with the baseline and with the branch gives
identical stores (whole imports can differ only through Ghidra itself).

| binary (time after Ghidra) | before | after | peak |
|---|---|---|---|
| AC15 httpd (1928 functions) | 29.6 s | 4.6 s | 1.95 → 1.58 GB |
| Telegram's libtmessages (24 MB) | 454 s | 39 s | 19.1 → 15.5 GB |
| small firmware binaries (23–183 KB) | 1–3.5 s | 0.3–1.8 s | ±10% |

What dominates a pcode import now, after Ghidra: gzipping the fact tables (level 6). Level
1–5 would cut that by 30–60% for 4–77% larger facts; left for you to decide.

Opt-in: `--features jemalloc` on ctadl-ascent: 13–21% less CPU on a Dex import; not default
because its effect on indexing is unmeasured.

Not done, recommended: shrink `Statement` (120 bytes; `CallStyle` is inline and `sources` is a
`SmallVec<[Exp; 2]>`) — about 40% of a program's statement memory, for import *and* index —
but it touches ~40 files across every front end and the index.

## Where the time and memory went (reading of the code, before measuring)

`ctadl import app.apk` today runs strictly one thing at a time:

1. Dex half (`frontends/ctadl-dex`): reads the whole APK into memory, decompresses every
   `classes*.dex`, and lowers every method on one thread.
2. Native half (`ctadl-frontends/src/apk_native.rs`): extracts *all* the selected ABI's `.so`
   files into memory at once, then runs Ghidra on them one by one. Each Ghidra run is its own
   JVM (`-Xmx40G`), with JVM startup and auto-analysis costs per library.
3. XAPK: split APKs imported one after another.
4. Writing: bitcode-encode the program, then write source info as parquet, serially.
5. The APK file is read from disk up to three times (Dex, native libs, manifest).

## Plan

- [x] Measure baseline (time, peak RSS): see Measurements.
- [x] Native libraries: import several at once, with a scheduler that splits the core budget
      (≤16) between Ghidra runs and bounds total memory.
- [x] XAPK: import splits concurrently under the same budget.
- [x] Dex: lower methods in parallel, assembling results in the same order so output is
      byte-identical to the serial import.
- [x] Pcode lowering / fact reading: parallelize the hot parts.
- [x] Writing: encode and write program/VMT/source info concurrently; free memory early.
- [x] RAM: stop holding all `.so` bytes at once; avoid re-reading the APK; drop big buffers
      as soon as they are used.
- [x] Verify: output identical to baseline (store files compared), tests pass.

## Measurements (Dex, during the night)

Machine: 20 cores, 128 GB, macOS. A benchmark daemon and two agents share it, so load is
20–40 throughout; compare *instructions retired* (`time -l`) first, wall time second.

WhatsApp Business XAPK, `--no-native-libs` (458k functions, 10.7M statements):

| build | wall | instructions | peak footprint | output |
|---|---|---|---|---|
| baseline (e96d282) | 94.6–121 s | 1418 G | 6.5 GB | — |
| + dex-reader caches, alloc cuts (s1) | 106 s* | 1048 G | 6.7 GB | IR + source info identical |
| + merged-method template clones (s2) | 29.6 s | 300 G | 6.1 GB | IR + source info identical |
| + parallel lowering, source-info interning (s3) | 16.2 s | 204 G | 5.9 GB | identical |
| + parallel unpack, scan, verify (s4) | 9.8 s | 205 G | 6.1 GB | identical |
| + framed program encoding (s5) | 8.9 s | 203 G | 4.4 GB | identical (program re-encoded) |
| + parallel placing (s6) | 8.0 s* | 203 G | 4.5 GB | identical |

\* load 55.

TikTok XAPK (1.93M functions, 33M statements): baseline 114 s / 20.9 GB peak → s5 46 s / 13.9 GB.
Of the 46 s, 10 s is unpacking the XAPK's splits before any Dex work (agent 1's area).

Sweep of s3 against the baseline (`--no-native-libs`, wall / instructions / peak):

| app | baseline | s3 |
|---|---|---|
| WhatsApp Messenger | 176 s / 1306 G / 6.3 GB | 13.5 s / 196 G / 5.9 GB |
| Messenger | 51 s / 608 G / 6.8 GB | 12.1 s / 188 G / 6.3 GB |
| TikTok | 114 s / 1161 G / 20.9 GB | 70 s / 533 G / 19.9 GB |
| Telegram | 12.2 s / 136 G / 2.9 GB | 5.0 s / 66 G / 2.7 GB |
| 9 F-Droid apps + noto | 1.9–17 s | 0.6–6 s |

All byte-identical (program, source info; VMT compared decoded) except nextcloud.talk2,
which exposed a latent bug (below).

\* load 37.

## Findings (Dex, during the night)

- 85% of a Dex import was the lowering loop, and two thirds of that was cloning merged-class
  methods: every clone lowered the full merged body, then pruned all but one arm.
- The import made ~970M allocations (57 GB allocated in total) to produce a ~6 GB result. Neither
  mimalloc nor jemalloc helped (85–90 s): the problem was the churn, not the allocator.
  Sources: per-instruction `ArtifactKey` clone and key serialization in `span_for`, Dex
  strings re-decoded on every lookup, method signatures and field names re-formatted per
  instruction, `format!("v{n}")` per operand, a `Vec` per instruction.
- `ir-vmt.bitcode` differed between two runs of the *same* binary: the class hierarchy is a
  `HashMap` serialized in seeded iteration order. Now sorted by class name.

- **Latent bug, fixed:** `call_result`/`catch_result` (what `move-result`/`move-exception`
  read) carried over from the previous method, because only some paths through
  `dataflow_to_assign` clear them. A catch handler lowered before any call in its method read a
  `LocalIdx` of the *previous* method's table, i.e. an unrelated local (seen in
  nextcloud.talk2: `move-exception` read `%retval`). Now reset per body. To keep verification
  exact, sweeps since then compare against the baseline with only that one-line fix applied
  (`/Volumes/Shampoo/parallel-import/baseline-fix`).
- The import format is now 15: `ir-program.bitcode` is framed (globals, then 2048 functions
  per frame), encoded and decoded in parallel and streamed to disk. Old imports are refused
  with the usual "re-import it" message. Verification re-encodes the new file the old way and
  compares bytes.

## How verification works

`/Volumes/Shampoo/parallel-import/scripts/`: `run-import.sh` imports at a fixed store path (so
paths recorded in source info match), `compare-stores.sh` compares every import's files byte
for byte, decoding the VMT (hash maps inside) and re-encoding a framed program the old way,
`sweep.sh` runs baseline vs. new over a list of artifacts.

## Log

- 22:20 Agents started: (1) concurrent native libraries / XAPK splits under a core and
  memory budget; (2) source-info interning and a concurrent store write.
- 22:55 Committed dex-reader caches, ctadl-ir (Locals index, sorted VMT, prune moves),
  ctadl-dex template clones. Tests pass (dex-reader, ctadl-dex, ctadl-ir).
- 23:05 Merged agent 2's source-info work (cherry-picked): compact hash indexes (~5 bytes per
  row instead of ~50), `file_for`/`span_in_file`, concurrent store write.
- 23:10 Parallel lowering committed (three passes: bookkeeping, parallel lowering, ordered
  placing). WhatsApp Business 16 s.
- 23:17 Parallel unpack, merged-class scan and verification; 9.8 s.
- 23:25 Framed program format (import format 15); peak 6.1 → 4.4 GB.
- 23:40 Found and fixed the stale call-result bug; parallel placing; committed.
- 23:50 Sweep of s6 vs the fixed baseline: 18 apps (9 F-Droid, noto, Messenger, Telegram,
  WhatsApp, TikTok, TikTok Lite, FX, Chrome, VLC) all identical. TikTok 145 → 34 s, peak
  20.9 → 14.7 GB; WhatsApp 119 → 8.5 s; Messenger 57 → 6.5 s; peaks down 25–35% everywhere.
- 23:55 JVM front end found nondeterministic in the baseline (function order differs run to
  run on soot-trunk.jar), and a 105 MB fat jar fails in both builds after 4 minutes with
  "multiply-defined function". Agent 3 started on the JVM front end.
- 00:05 Firmware (pcode) import: Ghidra 77 s, then 17 s of serial in-process work (fact
  compression, reading, constant propagation, lowering). Agent 4 started on it.
- 00:10 Memory shape of a Dex import: RSS climbs during lowering to the size of the finished
  program and stays flat through the write, so the peak is now the program itself.
- 00:20 Merged-class scan flattened across dex files (TikTok 3.9 → 2.5 s).
  `cargo xtask regression --release --frontend dex`: 71 passed, 0 failed (includes the
  apk:* import/index/query/report/reimport checks on the new format).
- 01:25 Agent 1 (native libraries / XAPK) merged: 6 commits cherry-picked. WhatsApp Business
  XAPK (Dex only) now 5.8–6.6 s at load ~20.
- 02:05 Agent 5 (.NET) merged: 7 commits. Rechecked against the fixed baseline: identical.
- 02:20 Agent 4 (pcode) merged (merge commit; its `lower_facts` replaces agent 1's body,
  agent 1's `export_facts`/OOM retry kept). Agent 3 (JVM) merged: 11 commits cherry-picked.
  Final sweep over all apps against the fixed baseline started.
- 02:45 Final checks: sweep of 19 apps identical; xtask regression (all front ends) 254/0/5;
  workspace tests, clippy, fmt clean; scaling measured at load ~15.
- 03:00 Code review of the branch; fixes committed (see "Review" above). History rewritten to
  drop status.md. Full xtask regression re-run on the final tip.
- 03:35 Final sweep with the final tip: 19 of 19 apps identical. Done.

## Follow-up: the three items left (agents, 2026-11-08)

### 1. Shrink `Statement` — merged (fast-forward to ff415789e)

Branch `worktree-agent-a12daea44e2dda386` (worktree
`~/proj/ctadl-rs/.claude/worktrees/agent-a12daea44e2dda386`), three commits on 016a6d815:

| commit | change | `Statement` |
|---|---|---|
| b3dba5876 | `Assign` sources: one inline, else a `ThinVec` (`Sources`, 32 B, was 72) | 120 → 112 |
| e10cb7363 | `JavaCall`/`CilCall` name their method as one interned `ArcIntern<JavaMethodRef>`/`<CilMethodRef>`; `FuncPtrCall.signature: Option<Symbol>`; `CallStyle` 88 → 40 | 112 → 80 |
| ff415789e | `Phi` operands in a `ThinVec`; compile-time check `Statement` ≤ 72 B | 80 → 72 |

Why: in wab, 70% of statements are one-source assigns, 16% direct calls, 12% Java calls (7%
phis after indexing). Calls are too common to box `CallStyle` (a Box variant measured worse:
TikTok peak 12.7–13.5 vs 12.2–12.6 GB). Getting to 64 B needs `FuncPtrCall`/`LuaCall` ≤ 24 B;
a partial try gained nothing.

Format unchanged (bitcode encodes the nested struct like flat fields; `Sources` serializes as
the same sequence): stores byte-identical to 016a6d815 on wab, tiktok, Mono.Android, CSharp,
soot, rt.jar, antennapod, schildi. Tests, clippy clean; `xtask regression --release` 255/0/5
(one more than before: the case 016a6d815 added). `cargo fmt --check` flags
`source-info/src/index.rs:68`, which is unchanged from the tip.

| run | instructions | peak footprint | wall |
|---|---|---|---|
| wab import | 195–197 → 194–198 G | 4.20–4.50 → 3.73–3.75 GB | best 7.1 → 7.3 s |
| TikTok import (5 pairs) | median 474 → 471 G | 13.8–14.0 → 12.2–12.6 GB | best 27.9 → 29.3 s (load 15–180) |
| index antennapod | 150.8 → 150.7 G | 3.35 → 3.40 GB | 47 → 32 s |
| index schildi | 538.1 → 538.8 G | 9.13 → 8.95 GB | 220 → 154 s |
| Mono.Android / CSharp / soot / rt / antennapod import | same | 1.42→1.28, 0.84→0.71, 0.57→0.50, 1.66→1.37, 0.58→0.52 GB | |

Import peaks drop 10–17%; index peaks barely move (flow relations dominate, not statements).
TikTok wall slightly slower in most pairs (lowering phase) with equal-or-fewer instructions;
read as load noise, needs a quiet rerun. Indexing wab with the *reference* build ran 41 min at
210 GB peak and was killed (`runs/stmt-idx-ref-wab-1`) — a separate problem worth a look.
Binaries: `bins/stmt-ref`, `bins/stmt-new`, `bins/stmt-box`.

### 2. Parquet source info encoded in parallel — done on a side branch, no wall-time gain today

Branch `pi-parquet`, commit 8239a3f58 (worktree
`~/proj/ctadl-rs/.claude/worktrees/agent-ab88be59577124676`). `source-info/src/parquet_io.rs`:
`ArrowWriter` replaced by `into_serialized_writer()` + `ArrowRowGroupWriterFactory`; each column
chunk of each 1M-row group encoded on its own rayon task and appended in order (a few row groups
in flight); columns built per row group instead of whole-table arrays. New test checks bytes
equal `ArrowWriter`'s at 0/1/1M/2.1M rows on 1/3/8 threads. Stores identical to 016a6d815 on
WhatsApp and TikTok at -j 1/8/16. Tests, clippy clean; regression 255/0/5.

Finding: **source info was never on the critical path.** Timing the three concurrent writes,
the program bitcode write finishes last every time (WhatsApp -j 16: source info 0.92→0.57 s,
program 1.94→1.33 s; TikTok: 6.6→6.3 s vs program 16–19 s). TikTok writes ~2.4 GB to the USB
disk and the program write alone ranged 4.7–38 s with disk load.

| source-info rewrite alone (TikTok) | -j 1 | -j 8 | -j 16 |
|---|---|---|---|
| SSD, before → after | 1.44 → 1.81 s | 1.49 → 0.53 s | 1.51 → 0.35 s |
| Shampoo, before → after (noisy) | 1.9 → 3.4 s | 1.6 → 2.9 s | 1.6 → 3.1 s |

Whole import wall unchanged within noise (WhatsApp 7.2 s at -j 8; TikTok ~31 s at -j 8/16);
instructions equal. TikTok peak 0.5–0.9 GB lower (no whole-table arrays); WhatsApp ±0.2 GB.

Trade-offs: -j 1 writes ~0.4 s slower on TikTok (old writer used 3 threads regardless); source
info now shares the rayon pool with the program encoder; WhatsApp source-info rewrite alone
peaks ~0.26 GB higher at -j 16. Merge mainly for TikTok's peak. **What limits the write phase
past 8 threads is the program bitcode write and disk throughput**, not source info.

### 3. Cheaper placing — done on a side branch

Branch `place-intern` (worktree `~/proj/ctadl-rs/.claude/worktrees/agent-a09230741b5d95771`):
a43f57892 source-info batch interning; b307c299f Dex/JVM/.NET intern each placed batch in one
call; 3dec0786a pcode likewise.

Where placing went (WhatsApp, -j 16, 11.45M spans, 9.9M new): span interning 2.1 s on one
thread; rewriting bodies with final ids 1.1 s (already parallel); everything else ~0.3 s. JVM:
interning 1.66 of 1.84 s. Change: each span index is split into 64 hash shards;
`SourceInfoBuilder::spans_in_files` looks up a whole batch in parallel, then numbers new spans
in batch order on the calling thread (ids still first-seen). Small batches / -j 1 intern one
at a time as before. Dropped: a finer-grained version (~11 parallel steps per batch; 2–3×
slower once load > 40), a separate placing pool, moving Dex's other ~250 ms of placing work.

Byte-identical to 016a6d815 at -j 1/3/16 on wab, tiktok, Mono.Android, CSharp, soot, the
HeadlessAndroidScanner jar. Tests (1979), clippy, fmt clean; regression 255/0/5.

| input | -j | lower+place | placing | wall |
|---|---|---|---|---|
| WhatsApp (Dex phase) | 16 | 3.28 → 2.60 s | 1.67 → 1.00 s | noisy |
| | 8 | 3.55 → 3.05 s | 1.69 → 1.31 s | 6.0–6.6 → 5.6–6.0 s |
| | 1 | 10.1 → 9.6 s | 1.80 → 1.37 s | 17.3 → 16.9 s |
| HeadlessAndroidScanner jar | 16 | 1.93 → 1.27 s | 1.55 → 0.88 s | 5.2 → 5.0 s |
| | 8 | 2.15 → 1.73 s | 1.56 → 0.98 s | 6.2 → 5.0 s |
| | 1 | 9.07 → 8.34 s | 1.67 → 0.86 s | 16.8 → 15.5 s |
| Mono.Android.dll | 16 | 0.65 → 0.56 s | 0.59 → 0.51 s | noisy |

Instructions +0–1% at -j 8/16. Open: `time -l` peak footprint 0.2–0.3 GB (+7%) higher on the
fat jar in all six runs, though sampled RSS is equal or lower; unexplained (maybe allocator
keeping freed shard-table memory on growth).

Findings:
- Next serial step: Dex per-file bookkeeping, ~0.8 s of the 2.6 s Dex phase at -j 16; could be
  a parallel pre-pass (signatures, names, params) with only map/table updates in order.
- .NET placing isn't dominated by spans (callee/extern map merge 0.14 s, interning 0.25 s).
- **pcode lowering at 016a6d815 is not deterministic**, contrary to "byte-identical at -j
  1/4/16" above: same facts, `ir-program.bitcode` differs between two runs at the same -j
  (ac15 at -j 3/16; mke2fs even at -j 1), and output depends on -j (ac15 -j 1 vs 16, cos
  `spans.parquet`). Runs: `pcode/runs/place2-*`. Needs fixing.
- `compare-stores.sh` reports false "DIFF (re-encoded)" when *both* stores are format 15 (its
  fallback re-encodes B the old way). IDENTICAL results are still valid (bytes matched first);
  only DIFF verdicts between two new-format stores are suspect.

## Next steps

- Merge parquet (2) and placing (3) on top of the Statement shrink; both are based on 016a6d815 and both touch `source-info`.
- Fix pcode determinism (above).
- Past 8 threads, the write phase is bound by the program bitcode write and disk throughput;
  then Dex per-file bookkeeping.
- Indexing wab with the tip took 41 min / 210 GB and was killed — investigate separately.
- Scratch: `bins/stmt-*`, `bins/pq-*`, `bins-place/`, `runs/{stmt,pq,place}-*`,
  `pcode/runs/place2-*`, `cargo-target/pi-{stmt,parquet,place}`.
