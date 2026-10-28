# P9 / P24: R8-merged Kotlin lambdas

R8 (Android's optimizer) merges many small classes, above all Kotlin lambdas and coroutine
`SuspendLambda`s, into one class whose methods switch on a synthetic class-id field. The analysis
does not see the switch, so each merged method behaves as the union of every lambda behind it, and
virtual calls inside it dispatch on whatever any of those lambdas captured. These methods become
dispatch hubs for the hybrid-context machinery, and the index stops converging (P9). Splitting the
classes back apart at import fixes that, but done naively the split stores every lambda body once
per id (P24).

This branch is off `main`, which has neither fix. Both fixes (S10, S29) and the measurements are on
`improve-native-handling`; `things-to-improve.md` there has the full record (P9, P15, P16, P24,
P25, S10, S11, S29, S30, M2, M3, M9).

## Starting point

Both fixes exist and apply cleanly to `main`, so this branch starts by bringing them in and
re-checking them on the current `main`:

```sh
git cherry-pick 9023a11c fb76eb0e
```

Hashes are on `improve-native-handling` as rebased onto `main` `076f6a60`; the subjects are given
too, since a later rebase changes the hashes. Each cherry-pick below was tried on `076f6a60`.

- `9023a11c` "Split R8 class-merged classes back apart at dex import" (S10), with the
  `class-id-merged-flow` regression fixture.
- `fb76eb0e` "Prune each R8 clone at import" (S29), which needs S10.
- Both move `com.noto`'s pinned `ctadl report` counts in `xtask/src/apk.rs`. Re-pin by the
  procedure in the comment there (import, `ctadl report app --format json`). On
  `improve-native-handling` they are 181,441 sites, 101,263 virtual, 2,141,436 CHA edges and
  2,075,579 RTA edges (`f265289b`); recompute them on this branch rather than copying.
- Optional, S11 (`35e54141` "Key the call-target transitive rule on the tag's path prefix"): it
  needs the probe counters from `e00e3dd6` "More recs", which needs `b47ed139` (below).
  `e00e3dd6` conflicts only in `things-to-improve.md`, which `main` does not have; drop that file.
  S11 helps `cpuinfo` but costs memory elsewhere (P17).
- `main`'s Dex frontend changed since these were measured (#153 fixed reader and frontend
  dataflow bugs), so re-run `cpuinfo` and NewPipe before relying on the numbers above.

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

## Status on current `main` (2026-10-27)

This branch now carries S10 and S29, ported onto `main` `7c98b73e` (merged in as `f6ce4131`), and
`com.noto`'s report totals re-pinned for them. Measured on a Linux machine with 62 GB of RAM, each
app imported Dex only and indexed under a 32 GB cap, without the fix (`main`) and with it.

**P9's non-convergence does not reproduce on current `main`, not even on the app it was found in.**
`cpuinfo` 6.7.0, the sample listed below, has no merged classes at all. 6.6.1
(`com.kgurgul.cpuinfo_60601.apk`, SHA-256
`54cc5595b9a07e8c92b9949642794cf2503bbe42d7711a0ab2c6d6097ea0c3e3`) is the version M2 measured: the
split finds exactly M2's 620 classes, 3,562 ids, 1,068 switching methods and 4,296 construction
sites. On `main` without the fix it reaches a fixpoint in 38 s at 1.73 GB, where M2's baseline ran
past 1800 s and 41 GB with 58 M `context_locals` rows and growing. Something on `main` since M2
defused the hubs' churn; it has not been bisected. Candidates: #154 (every parameter written back at
its entry version, `main`'s form of S16), #153 (Dex dataflow fixes), #162 (the caller's heap formal
shared across calls).

**P24 does not exist on `main`:** without the split there are no clones. It is the split's own cost,
and S29 keeps it to about +12% of the stored program.

| App | Merged classes (ids) | Peak | Fixpoint | Main loop | `context_locals` | Call-target tags | Stored program |
|---|---|---|---|---|---|---|---|
| `cpuinfo` 6.6.1 | 620 (3,562) | 1.73 → 1.94 GB | 25 → 17 s | 337 → 192 it | 54 k → 33 k | 456 k → 509 k | |
| `com.noto` | 137 (528) | 2.19 → 2.20 GB | 32 → 35 s | 251 → 251 it | 41 k → 39 k | 756 k → 766 k | 30 → 31 MB |
| NewPipe 0.29.1 | 955 (4,889) | 4.02 → 4.17 GB | 80 → 85 s | 467 → 467 it | 1.02 M → 0.99 M | 1.65 M → 1.71 M | 64 → 72 MB |
| CENO 2.11.7 | 996 (5,877) | 3.87 → 3.56 GB | 66 → 51 s | 366 → 215 it | 63 k → 25 k | 1.82 M → 0.89 M | 63 → 73 MB |
| OsmAnd~ | 7 (68) | 12.90 → 12.91 GB | 369 → 321 s | | 938 k → 937 k | 3.50 M → 3.50 M | |
| Element | 10 (206) | 16.05 → 16.15 GB | 440 → 397 s | | 221 k → 221 k | 4.39 M → 4.39 M | |

(Single runs, not back-to-back pairs; times vary by 10-30% on this machine, relation sizes don't.)

**Results.** The broad query (`scripts/broad-android-model.json` on `improve-native-handling`)
reports the same sources everywhere and, on `com.noto`, the same results, with instruction ids
masked (the split renumbers them). On the two merge-heavy apps the split removes about half the
tainted instructions and adds none:

| App | Tainted-instruction locations | Sinks | Gained |
|---|---|---|---|
| `cpuinfo` 6.6.1 | 20,904 → 1,850 | 65 → 65 | 0 |
| NewPipe | 33,177 → 17,019 | 359 → 358 | 0 |
| CENO | 29,728 → 15,316 | 205 → 204 | 0 |

**The removed results are union flows, in every case traced.** Each sampled lost location was made a
sink (that call's argument, in that caller) and queried on both indexes. 29 of 29 traceable samples
(11 NewPipe, 12 CENO, 6 `cpuinfo`; three more were not traceable this way) have a path before the split that runs
through classes the split separates, and no path after it (CENO's one exception keeps 4 of 2,708
paths, to another call of the same callee in the same union method). The same few merged lambdas
recur as junctions: `ZipFilesKt$$ExternalSyntheticLambda3`, `Data$$ExternalSyntheticLambda0`,
`FeedFragment$$ExternalSyntheticLambda10`, `Player$$ExternalSyntheticLambda4` in NewPipe;
`ImageKt$$ExternalSyntheticLambda0`, `Regex$$ExternalSyntheticLambda0`, `WindowFeature$start$1` in
CENO; in `cpuinfo`, `Li;`, the hub M2 found. One example: `Cursor.getString` taint enters `ZipFilesKt$$ExternalSyntheticLambda3.invoke`,
then `Data$$ExternalSyntheticLambda0.invoke`, and is stored into `Unit.INSTANCE` and
`EmptyList.INSTANCE`, which no arm of either lambda writes. Each lost sink is a sink endpoint
expanded over an access path that existed only through such edges (NewPipe: `FeedGroupIcon`'s enum
entries into a crash report's `Log.w`; CENO: `BrowserState.tabs` into a reader-view listener's
`putString`); after the split the path is gone from the index, so the endpoint is not generated.

**Checks.** `cargo test` passes for `ctadl-ascent` (370), `ctadl-dex`, `ctadl-import` and
`dex-reader` (86). `cargo xtask regression --frontend dex,jvm`: 133 passed, 6 skipped (no `smali`
or ASM here), 0 failed, including the Dex `ClassIdMergedFlow` fixture, which asserts the split keeps
only its two real flows. (`dx` came from Ubuntu's `dalvik-exchange` package, symlinked as `dx`.)

**S30 (P25), on this branch.** A merged class is *closed* when it is `final` and every
`new-instance` of it in the program allocates a clone (a known id, or the method's own id inside a
cloned method): then no object is ever the class itself, and its merged methods never run. Their
bodies are lowered empty (they return at once). They stay in the dispatch table, so a call's
targets, and so whether the index resolves it by class hierarchy or by call-target tags, do not
change; a call resolved by class hierarchy just no longer takes on the union of every arm. A first
version left the merged methods out of the table instead: a merged class with 4 ids then fell to
4 targets, so its calls moved from call-target tags to the coarser class-hierarchy resolution, and
CENO and NewPipe gained 146 and 207 tainted locations.

85-94% of merged classes are closed (`cpuinfo` 544 of 620, `com.noto` 129 of 137, NewPipe 818 of
955, CENO 848 of 996). The S10+S29 index has no allocation of the plain class for any of them,
which is the premise. S10+S29 → S10+S29+S30:

| App | Tainted-instruction locations | Sinks | Gained | `locals` | Call-target tags | Peak |
|---|---|---|---|---|---|---|
| `cpuinfo` 6.6.1 | 1,850 → 617 | 65 → 47 | 0 | −9% | −16% | 1.94 → 1.74 GB |
| `com.noto` | identical | identical | 0 | −1% | −1% | 2.20 → 2.19 GB |
| CENO | 15,316 → 14,863 | 204 → 165 | 0 | −4% | −8% | 3.56 → 3.49 GB |
| NewPipe | 17,019 → 16,868 | 358 → 346 | 0 | −2% | −4% | 4.17 → 4.09 GB |

Main-loop iterations are unchanged. The `dex`/`jvm` regression families pass (`ClassIdMergedFlow`
included) with `com.noto`'s report counts re-pinned, since the emptied bodies drop their calls. Not
seen: an object of a merged class built by reflection.

**Detector coverage.** Three layers, measured on `cpuinfo` 6.6.1, `com.noto`, NewPipe and CENO:

- *Recognized but not split*: only classes with a single id (4-19 per app), which have no union.
  No class reaches the 64-id cap.
- *Split but not closed* (S30 cannot apply): this was 6-15% of split classes, nearly all for
  allocations whose constructor call the analysis did not find. Inside a `try`, a `new-instance`
  ends its basic block, and the analysis dropped a block's last result at the block's end (right
  for an instruction whose result a handler could see, but a handler never sees an uninitialized
  object), so the `<init>` in the next block, and the constant id it passes, was lost (96
  allocations in `cpuinfo`, 229 in NewPipe). Fixed: the allocated object now reaches every
  successor. An interprocedural constant propagation, the first idea, would have found nothing:
  only 2-7 allocations per app have an id the analysis does not know, none of them a parameter.
  After the fix 96-98% of split classes are closed (`cpuinfo` 609 of 624, `com.noto` 137 of 140,
  NewPipe 929 of 965, CENO 993 of 1,011), a few more classes reach two ids and are split, and the
  premise holds for all of them (no allocation of a plain emptied class).
- *Never recognized*: a scan of every class for a virtual method that compares an `int` field of
  `this`, written only in constructors, against constants (by `switch` or `if`), with at least two
  constant ids passed at construction sites, finds 98-99% of what S10 splits, plus 3, 3 and 12
  classes it does not. None is a merged lambda: an abstract base class whose subclasses pass the
  id to `super(..)`, a bit-flags field, and named library classes testing a mode or type field
  (`ExoPlaybackException.type`, `FocusTargetNode.focusability`, `jsoup`'s `Token.type`). Shapes
  outside the scan's rule (an id derived by arithmetic, or kept in a non-`int` field) were not
  looked for.

With the fix, beyond S30 (only losses, iterations unchanged):

| App | Tainted-instruction locations | Sinks |
|---|---|---|
| `cpuinfo` 6.6.1 | 617 → 617 | 47 → 47 |
| `com.noto` | identical | identical |
| CENO | 14,863 → 10,237 | 165 → 161 |
| NewPipe | 16,868 → 7,553 | 346 → 341 |

Four traced NewPipe losses all run through two merged lambdas,
`Recomposer$$ExternalSyntheticLambda4` and `ComponentRegistry$Builder$$ExternalSyntheticLambda0`,
into Rhino's `ScriptRuntime`; the allocations of those lambdas now build clones. Overall, on
`main` → this branch, the broad query's tainted instructions go from 20,904 to 617 on `cpuinfo`
6.6.1, 33,177 to 7,553 on NewPipe and 29,728 to 10,237 on CENO, with no result gained.

**Regression tests.** `nightly/tests/java/ClassIdMergedTryFlow` (a negative case: a 5-id merged
class's object built inside a `try` must not reach every arm) fails with the scan from before the
`try` fix and passes after it; the jvm frontend does not split, so `Jvm:ClassIdMergedTryFlow` is a
known failure. `apk:merged-classes` (in `cargo xtask regression`, beside the other `apk:*`
checks) imports `com.noto` and pins the split's guardrail line, 140 classes split, 3 not closed,
0 over the id cap, 3 of 667 allocations with no known id, and fails if the guardrail warns.
`com.noto`'s `apk:report` counts moved with every change above and stay pinned. While building
the guardrail, a miscount (more allocations with a known id than allocations) exposed one more
bug: a subclass object initialized through a merged superclass's constructor was retagged as the
superclass's clone. Fixed; results unchanged on the four apps.

**Guardrail.** Coarse checks that log `merged classes guardrail:` lines at `info` and warn when
the split is visibly failing:

| Where | Warns when | Measured: split working | Measured: broken |
|---|---|---|---|
| import | over 10% of merged classes exceed the 64-id cap | 0 | (not seen) |
| import | over 10% of split classes keep live merged methods | 2-4% | 12-15% (before the `try` fix) |
| import | over 2% of allocations of split classes have no known id | 0.1% | 2-4% (before the `try` fix) |
| import | clones hold over 35% of the program's statements | 2-24% | 43% (NewPipe before S29) |
| index | over 300 call edges reach large (50+ statements) live merged methods | 0-8 | 195-1,073 (before S30, estimated from the index tables) |

Checked: on all four apps the current branch logs no warning; with the `try` fix disabled, both
allocation and closure warnings fire on `cpuinfo` and NewPipe.

**What this means for the branch.** The split is now mainly a precision fix (about half of the
tainted results on merge-heavy apps are union artifacts) and a cost reduction where merging is
heavy (CENO, `cpuinfo`), rather than a fix for non-convergence: on `cpuinfo` 91% of the tainted
instructions are union artifacts. Open: which `main` change defused the non-convergence (a bisect
is cheap: `cpuinfo` 6.6.1 indexes in under a minute); P25/S30 for the classes that are not closed (union methods left for unknown ids); classes the detector does not split
(`merged classes: not splitting` at debug level).

## The problem

### P9: merged lambdas are dispatch hubs

In `com.kgurgul.cpuinfo` (the version measured on 2026-09-30) the functions holding the most
`context_locals` rows were all R8-merged Kotlin suspend lambdas: `Li;->r` and `Li;->e`, `Lp8;->r`,
`Lk;->r`, `Lt40;->r` (here `nv2` is `SuspendLambda`, `vk` is `BaseContinuationImpl`, `yp0` and
`up0` are `Function2` and `Function1`).

- **Many lambdas share one method.** `Li;` merges 29 lambdas behind a synthetic `int i` class id.
  `r` (`invokeSuspend`) and `e` (`invoke`) switch on it, and the captured state of all 29 lives in
  the untyped `Object` fields `k` and `l`. `r` is analyzed as the union of all 29 bodies, and its
  virtual calls, on casts of `this.l`, dispatch on whatever any of 62 construction sites stored
  there. At 320 s `Li;->r` had 1,734 decisions, 7,165 distinct decision sets (largest 318) and
  726 M memberships.
- **The union is instantiated many times over.** Each of `Li;->e`'s 30 switch cases calls
  `this.p(..).r(Unit)`, so `r`'s union summary lands at 30 identical call sites. Each site's
  receiver holds 3,129 call-target tags and has 20,181 out-edges: 63 M pairs per vertex, 1.9 B of
  the 3.2 B pairs a full re-derivation of `call_target_assign_like` would visit.
- **It recurs:** 75 `SuspendLambda` subclasses in `cpuinfo` carry a class id; merged classes are
  in 12 of the 15 apps of the original corpus.
- **It drives two more problems.** The union summaries make the decision sets churn (P15:
  `context_locals` grows a set one decision at a time, 221 M unions for 39 M rows, and every update
  re-runs the row through the contextual closure) and make the call-target join visit billions of
  pairs (P16).

### P24: the split stores every arm in every clone

The fix for P9 (S10, below) clones each switching method once per class id and narrows only the
switch, so each clone keeps every other id's arm as unreachable code. A class with n ids stores the
merged body n times, and the body grows with n: quadratic in the ids (capped at 64). The fixpoint
never sees the dead arms (the index prunes unreachable blocks before SSA), but the import, the
stored program and the index's load all pay for them, and `ctadl report` counts them.

On `org.schabi.newpipe` 0.29.1: 955 merged classes split into 4,889 ids, 1,671 switching methods
into 9,451 clones holding 1,140,297 instructions, exactly ids × the original for every method. That
was 43% of the imported IR in 11% of the functions (the largest, a Compose resources lambda, was 30
ids × 2,648 instructions). 214 k of NewPipe's 528 k call sites, and 2.9 M of its 6.5 M CHA edges,
could never execute.

### P25 (related, precision only): small merged classes stay merged at CHA sites

The split keeps the merged class `C` and its union methods for objects built with an id the import
can't determine, and a CHA answer can't tell `C`'s method from its overrides in `C$r8id<k>`. So a
site resolved by CHA reaches the union and every clone. On NewPipe that is 1,018 sites, 0.7% of call
edges; the union's summary already holds every clone's flows, so nothing is lost, only precision.

## Measurements

**`cpuinfo` without a fix** (as on `main`): not a memory blowup but a fixpoint that never
converges. After 640 s, scc 4 is still adding rows and minting decisions, and each iteration costs
more per new row than the last.

| Rung (`CTADL_INDEX_TIMEOUT_SECS`) | Iterations | Peak | `locals` | `context_locals` | `call_target_assign_like` | Decisions |
|---|---|---|---|---|---|---|
| 10 s | 41 | 2.5 GB | 3.1 M | 0.14 M | 1.1 M | 5.8 k |
| 40 s | 95 | 6.9 GB | 15.2 M | 4.3 M | 5.8 M | 10.1 k |
| 160 s | 118 | 16.0 GB | 34.9 M | 20.0 M | 7.8 M | 16.2 k |
| 640 s | 127 | 28.9 GB | 56.9 M | 39.0 M | 8.7 M | 18.6 k |

At 1800 s it was still running at 41.5 GB. The other hybrid-context modes don't finish either
(`--hybrid-context none`: 31.5 GB at 1200 s, every caller gets the union of all resolved callees'
summaries; `collapse`: removes the churn but not the volume).

**With S10:** fixpoint in 116-119 s at 7.6-7.8 GB; decisions fall from 22 k (still growing) to
7.2 k, and `context_locals` from 58 M (still growing) to 1.1 M. The regression suite's SARIF is
unchanged, and a fixture added with S10 (`nightly/tests/java/class-id-merged-flow.json`, on
`improve-native-handling`) loses a false positive.

**With S29 on NewPipe:** stored program 112.4 → 74.9 MB, import peak 1.35 → 1.05 GB, the index's
loaded IR 521 → 360 MB. Query results on `com.noto` unchanged.

## How to reproduce

On `main` (no fix), Dex only:

```sh
cargo build --release
export XDG_STATE_HOME=$PWD/tmp/p9/state
target/release/ctadl import -l apk --no-native-libs --name cpuinfo com.kgurgul.cpuinfo_60700.apk
# A timeout gives a profile of a run that would not finish; repeat with 10, 40, 160, ...
CTADL_INDEX_TIMEOUT_SECS=160 RUST_LOG='warn,ctadl=debug,ctadl_ir::ssa=info' \
  systemd-run --user --scope -p MemoryMax=32G -p MemorySwapMax=0 \
  target/release/ctadl index cpuinfo 2> tmp/p9/cpuinfo-160.log
```

What to look for:

- `profile.json` in the index directory (`$XDG_STATE_HOME/ctadl/projects/<name>/index/`): every
  relation's tuple count, each scc's iterations and rule times, and whether the run reached the
  fixpoint or stopped at `CTADL_INDEX_TIMEOUT_SECS`. The same numbers are in the debug log.
- `context_locals` and the decision count still climbing between rungs, and rule time per new row
  rising (compare the rungs' `profile.json` files).
- The hubs: find the functions holding the most `context_locals` rows; they should be methods of
  R8-merged classes. To recognize one in `ctadl inspect cpuinfo --dump-ir`: a class with a
  `final int` field that every constructor stores from an argument or a constant, and methods that
  load it from `this` and `switch` on it. Class names change from build to build, so the names
  above may not exist in this version.
- On `main` a timed-out run loses the running scc's contents and timing;
  `improve-native-handling` vendors a patched `ascent_macro` that keeps them and adds per-rule
  row counters (`scripts/blowup-rank.py` ranks them).

For P24, on `improve-native-handling` at a commit with S10 but before S29 (`9023a11c`), import
NewPipe and compare `ir-program.bitcode` in the store with an import made after S29 (`fb76eb0e`);
`ctadl inspect <name> --dump-ir` shows each `C$r8id<k>` clone carrying every arm. The repo's own
`xtask/tests/dex/com.noto_54.apk` (137 merged classes) shows the same at small scale.

## Ideas for solutions

**S10, done on `improve-native-handling` (`9023a11c`): split R8-merged classes at import.**
`frontends/ctadl-dex/src/merged.rs` finds a `final int` field that each constructor stores from an
argument or a constant, and that methods load from `this` and switch on (a must-analysis of
register values handles `move-object v4, p0` and constant ids). Each id gets a subclass
`C$r8id<k>` with clones of the switching methods, each switch narrowed to arm `k`, and construction
sites with a known id allocate the subclass. `final` is what makes it sound; the `synthetic` flag
is not required. Fixes `cpuinfo` (above).

**S29, done on `improve-native-handling` (`fb76eb0e`): prune each clone at import.** Run the pass
the index already runs before SSA (`ctadl_ir::ssa::prune_unreachable_nodes`) on each clone right
after lowering it, so a clone stores only its own arm. Fixes P24 (above).

**S11 (keyed call-target join, on `improve-native-handling`):** splits each tag's path at every
prefix so the transitive join only retrieves matching edges. Helps `cpuinfo` with S10 (82 s
instead of 117 s), but costs memory everywhere else (P17); measured trade-offs are in
`things-to-improve.md`.

**Open:**

- **S30 (P25):** when every construction site of `C` has a known id, nothing is allocated as `C`
  itself, so leave `C`'s union methods out of the CHA targets (or don't emit them). Needs a
  soundness check for `super` calls into `C` and for reflection. Small gain (1,018 sites on
  NewPipe).
- **Merged classes S10 does not recognize.** The detector needs a `final int` id set in every
  constructor. Other R8 merging shapes (ids passed through a factory, non-`final` fields, merged
  interfaces) would fall back to the union; nobody has counted how many remain.
- **Bounding decision-set churn generally (S14):** `--hybrid-context bounded:k` and `spill:k`
  exist; rejected for `cpuinfo` (S10 removed the need), never measured as a backstop.

**How to check a fix:** `cpuinfo` converges, with decisions and `context_locals` near the S10
numbers; NewPipe's stored program and import peak; the regression suite's SARIF unchanged, and
`class-id-merged-flow` reporting only its two real flows.

## Samples

From F-Droid (`https://f-droid.org/repo/<file>`), except `com.noto`, which is in the repo.

| App | Version | File | SHA-256 | Size |
|---|---|---|---|---|
| `com.kgurgul.cpuinfo` | 6.7.0 | `com.kgurgul.cpuinfo_60700.apk` | `add5b106513c85c99689134959f1ed522bbd9ef54478d9e522b7d5658b33849d` | 2.8 MB |
| `org.schabi.newpipe` | 0.29.1 | `org.schabi.newpipe_1015.apk` | `1d66d19eedbab969b1be2e44e0ea35f5f8921cbe83b5c5ac810767ca1a84b7e8` | 11.5 MB |
| `com.noto` | 2.2.3 | `xtask/tests/dex/com.noto_54.apk` (in the repo) | `282c78c15e55e7711a9bb2f351ce5e70d3552327470f0cbadaa634e269ea3028` | 6.7 MB |

- `cpuinfo` 6.7.0 is newer than the version measured; check that it still has merged suspend
  lambdas before relying on it (it is the same app and toolchain, so it very likely does).
- NewPipe 0.29.1 build 1015 is the version measured in M9. F-Droid also lists
  `org.schabi.newpipe_1015_cb84069.apk` (SHA-256
  `18447bfb1e06d113edc88df93f471827280de06f6f3d4dc42f56f28b9c1bab79`), the same version built
  separately; which of the two M9 used was not recorded.

```sh
mkdir -p samples && cd samples
for f in com.kgurgul.cpuinfo_60700.apk org.schabi.newpipe_1015.apk; do
  curl -fLO https://f-droid.org/repo/$f
done
sha256sum *.apk   # compare against the table
```
