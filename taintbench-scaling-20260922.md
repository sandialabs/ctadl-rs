# What it costs to analyze a big app - DO-NOT-MERGE

This is the rebase of the `taintbench` branch onto `origin/main` (`e3795fd8`), a
re-run of the whole 38-app suite on top of it, a sweep of every call-graph
setting ctadl exposes, and a set of runs on real F-Droid apps 10x to 40x bigger
than anything in TaintBench.

The question behind it was: **we want to process large apps - what actually
stops us, memory or time?**

## Short answer

**Memory, and it is the plain dataflow closure that eats it - not the call
graph.** The relation called `locals`, which records "this variable can hold
taint from this formal along this access path", is where the memory goes. On the
biggest app that finishes it holds 34 million rows and costs 16.5 GB. On the two
apps that do not finish it never stops growing.

Three things follow, and the third is the one that matters:

1. **Nothing about an app's size tells you whether it will finish.** `ceno`
   (11 MB of dex) runs out of our 116 GiB budget; `darkcoin` (27 MB, 2.6x more
   functions) finishes in under three minutes at 16.5 GB.
2. **The rebase is free and helps.** `origin/main` picked up the `blowup2` index
   fixes, so the suite gets faster and lighter with no change in what it finds.
3. **The cheapest call-graph configuration does not rescue a single large app.**
   It fixes one specific pathology very well and is a net *loss* on a healthy
   large app. Call-graph tuning is not the lever for scale.

## The rebase

`git rebase origin/main` replayed all 26 branch commits with no conflicts.

The branch was sitting on `0183c42c` (`#128`). `origin/main` is now `e3795fd8`,
five commits later: `#130`-`#133` plus a squashed commit adding a
`resolved_call` relation. The useful part is that **`origin/main` now contains
the `blowup2` index fixes** - it differs from `28f949ea` only by that 214-line
addition. The branch gets them for free, and that is most of what changed:

| | suite wall | index | query | worst app's peak |
| --- | ---: | ---: | ---: | ---: |
| `0183c42c` (what the branch was on) | 389 s | 264 s | 31 s | 6.56 GB |
| `28f949ea` (`origin/blowup2`) | 272 s | 147 s | 31 s | 4.91 GB |
| **`e3795fd8` (now)** | **269 s** | **146 s** | **31 s** | **5.05 GB** |

`xbot_android_samp`, the app the fixes were aimed at, goes from 202 s to 85 s.

**What the suite finds did not change at all**: 22 pass, 16 fail, 77 of 191
ground-truth flows, 3 false positives - the same verdicts, app for app, as the
committed baseline. So the suite is still red for exactly the reason
`taintbench-callgraph-rework.md` records (`#128`'s aliasing), and this rebase
neither helps nor hurts that. The `expected.json` files are still untouched.

## The cheapest call-graph configuration

ctadl has two independent knobs here, and they behave completely differently.

### Knob 1: how much context the hybrid inliner keeps (`--hybrid-context`)

This decides what happens *after* the analysis has worked out which function an
indirect call goes to. `decision` (the default) keeps each resolved flow tagged
with the exact set of callers it holds under. The cheaper settings give that up
by degrees, and `none` gives it up entirely - it still derives the call graph
context-sensitively but then runs the dataflow over it context-insensitively, so
no `context_*` relation is ever built.

Every setting below was run over all 38 apps:

| setting | suite index | worst peak | xbot index | xbot peak | flows | FPs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decision` (default) | 146.3 s | 5.05 GB | 85.1 s | 5.05 GB | 77/191 | 3 |
| `collapse` | 81.1 s | 4.26 GB | 19.4 s | 4.26 GB | 77/191 | 3 |
| `bounded:4` | 80.9 s | 4.37 GB | 19.2 s | 4.37 GB | 77/191 | 3 |
| `spill:1` | 72.6 s | 3.91 GB | 11.6 s | 1.67 GB | 77/191 | 3 |
| `none` | 73.5 s | 3.98 GB | 12.3 s | 1.61 GB | 77/191 | 3 |

Halving the suite's index time for nothing looks like an easy win, and on this
suite it is - **all five settings produce identical results**, finding-for-finding
and false-positive-for-false-positive.

But the per-app numbers say something much narrower. Of the 38 apps, **37 do not
move at all** - their index time ratio between `decision` and `none` is 1.00.
The entire suite-level saving is one app:

| app | index (default) | index (none) | peak (default) | peak (none) |
| --- | ---: | ---: | ---: | ---: |
| `xbot_android_samp` | 85.1 s | **12.3 s** | 5.05 GB | **1.61 GB** |
| every other app | - | x1.00 | - | x1.00 |

So the honest description is not "the cheap setting makes things faster". It is
**"the contextual machinery is free on ordinary programs and catastrophic on a
particular kind of program, and the cheap setting removes that tail risk."**
`xbot` embeds Rhino, a JavaScript interpreter, whose dispatch loop is reached by
thousands of distinct call-site decisions that all meet at the same rows.

Worth noting: `spill:1` is as cheap as `none` while keeping context everywhere a
row has not widened. If a cheap default is wanted, that is the better-argued one
- it gives up precision only where the blowup actually happens.

### Knob 2: how many calls are resolved by class hierarchy (`--cha-threshold`, `--strategy`)

This one has no cheap setting. Pushing work *away* from CHA is cheap only
because it stops doing the analysis:

| setting | suite index | flows found | apps passing |
| --- | ---: | ---: | ---: |
| default (`mixed`, threshold 4) | 146 s | 77/191 | 22/38 |
| `--cha-threshold 0` | 129 s | **0/191** | 10/38 |
| `--strategy hi` | 127 s | **0/191** | 10/38 |
| `--strategy cha` | see below | 77/191 | - |

`--cha-threshold 0` and `--strategy hi` both find **zero connected source-sink
paths across all 38 apps** (89 at default). Hybrid inlining can only resolve a
call site when a concrete target *flows* to it; an ordinary virtual call on an
object allocated elsewhere never resolves that way. CHA is load-bearing for
recall on Java, and removing it does not buy speed, it buys an empty answer.

`--strategy cha` fails the other way, as expected - it over-approximates edges
and the fixpoint pays. On `xbot` I stopped it by hand at **30 minutes and
26.9 GB, still climbing**, against 85 s and 5.05 GB at the default. The suite run
took 2138 s with that one app killed.

So the whole knob is a dead end: the default threshold of 4 is already sitting
where it should be.

## Large apps

Five F-Droid apps, dex only (`--no-native-libs`), under a 116 GiB guard and a
90-minute timeout, on an M1 Ultra with 128 GB.

### Default configuration

| app | dex | import | index | peak | outcome |
| --- | ---: | ---: | ---: | ---: | --- |
| `org.openttd.fdroid` | 0.9 MB | 3 s | 2 s | 0.03 GiB | finished |
| `com.komodoplatform.komodowallet` | 4.4 MB | 2 s | 13 s | 1.59 GiB | finished |
| `ie.equalit.ceno` | 11.3 MB | 5 s | **5401 s** | **95.67 GiB** | **timed out** |
| `hashengineering.darkcoin.wallet` | 27.1 MB | 6 s | 167 s | 16.51 GiB | finished |
| `com.greenaddress.greenbits_android_wallet` | 34.9 MB | 8 s | **2094 s** | **116.16 GiB** | **killed at the cap** |

Read the `ceno` and `darkcoin` rows next to each other. **The app with less than
half the dex is the one that fails.** That is not a rounding-error difference:
`darkcoin` finishes in under three minutes while `ceno` burns 90 minutes and
96 GiB without reaching a fixpoint.

Import is never the problem on the dex path - 8 seconds at worst. All of the
cost is `index`.

### The same ladder under the cheapest setting

| app | index (default) | index (`none`) | peak (default) | peak (`none`) | outcome |
| --- | ---: | ---: | ---: | ---: | --- |
| `openttd` | 2 s | 2 s | 0.03 | 0.03 GiB | finished |
| `komodo` | 13 s | 13 s | 1.59 | 1.53 GiB | finished |
| `ceno` | 5401 s | 5402 s | 95.67 | 80.69 GiB | **still times out** |
| `darkcoin` | 167 s | **237 s** | 16.51 | **18.64 GiB** | finished, but worse |
| `greenbits` | 2094 s | 4903 s | 116.16 | 116.12 GiB | **still killed at the cap** |

**Not one of the failures is rescued.** `ceno` still times out, 16% lighter.
`greenbits` still hits the cap - it takes 2.3x longer to get there, which means
the setting slowed the growth without bounding it.

And on the one big app that was healthy, the cheap setting is a **net loss**:
`darkcoin` gets 42% slower and 13% heavier. The counters say why - dropping
context sensitivity made the context-free relation bigger:

| `darkcoin` | `locals` rows | rows per variable | index | peak |
| --- | ---: | ---: | ---: | ---: |
| default | 33,848,247 | 9.54 | 167 s | 16.51 GiB |
| `--hybrid-context none` | **80,370,782** | **22.65** | 237 s | 18.64 GiB |

That is the trade in one line: sharing flows between callers is cheaper per row
and produces far more rows. On `xbot` the first effect dominates; on `darkcoin`
the second does.

## So what is the bottleneck

It is the size of the `locals` relation, and nothing about the input predicts
it. Here is every input measure alongside the outcome:

| app | functions | calls | indirect sites | formals | outcome |
| --- | ---: | ---: | ---: | ---: | --- |
| `openttd` | 6,377 | 20,200 | 262 | 23,061 | 2 s |
| `komodo` | 30,091 | 116,844 | 2,177 | 114,289 | 13 s |
| **`ceno`** | **66,106** | **303,267** | **6,183** | **258,694** | **fails** |
| `darkcoin` | 169,485 | 614,103 | 19,543 | 653,018 | 167 s |
| **`greenbits`** | **208,162** | **751,418** | **21,184** | **814,739** | **fails** |

`ceno` is smaller than `darkcoin` on *every one of these columns* - 2.6x fewer
functions, 2.0x fewer calls, 3.2x fewer indirect call sites, 2.5x fewer formals -
and `darkcoin` is the one that finishes. There is no threshold here to find.

What separates them is how far taint spreads once it starts, which the counters
measure directly:

| app | `locals` rows | rows per variable | variables reached |
| --- | ---: | ---: | ---: |
| `openttd` | 581,318 | 4.32 | 80.1% |
| `komodo` | 2,791,734 | 3.68 | 84.9% |
| `darkcoin` | 33,848,247 | 9.54 | **99.0%** |

`darkcoin` reaches 99% of its variables at 9.5 rows each, and that already costs
16.5 GB - about 530 bytes for every `locals` row, taking the whole footprint and
attributing all of it to that one relation, so read it as a ceiling on the
per-row cost rather than a measurement of it. It is close to the edge, and it is
the *good* case. An app whose access paths are a little deeper or whose
containers are a little more shared goes over, and that is `ceno`.

**The practical reading:** ctadl's limit is not a code-size limit, it is a limit
on how densely taint spreads through the program. Reducing per-row cost or
bounding access-path depth would move the ceiling; tuning the call graph will
not, because the call graph is not what is large.

## What I would do next

The call-graph knobs are measured out now and none of them is the answer, so the
next moves are in the dataflow:

1. **Look at what is in `locals` on `ceno`.** Several hundred bytes a row is a
   lot. Whether the win is in representation (interning access paths, a narrower
   row) or in the row count itself is a question one dump would answer, and
   `ceno` times out at 90 minutes rather than dying, so a run with a row-count
   trace would get there.
2. **Bound access-path depth and measure what it costs in recall.** This is the
   obvious lever on the row count and TaintBench can price it, which is what the
   suite is good for.
3. **Consider `spill:1` as the default** rather than `decision`. It is free on
   this suite, it removes the `xbot` pathology, and unlike `none` it does not
   inflate the context-free relation - `none`'s regression on `darkcoin` is a
   real argument against making *that* the default.
4. Leave `--cha-threshold` alone.

None of this touches the `#128` recall question, which is still open and still
the reason the suite is red.

## How this was measured

One binary, built with `nix build .#packages.aarch64-darwin.default` from the
rebased branch (`origin/main` `e3795fd8` + the 26 branch commits).

The suite runs are the ordinary `xtask taintbench` over all 38 apps, with
`ctadl` on `PATH` replaced by a shim that runs the real binary under
`/usr/bin/time -l` and records wall time and **peak physical footprint** per
phase (footprint, not RSS: macOS compresses cold pages, so RSS understates a big
run badly). The shim also injects the per-configuration index flags, which is
how the configurations were varied without touching xtask. Default was run 3
times, `collapse`/`none`/`cha0` twice, the rest once; repeats agree within ~1%
(default: 275/272/274 s).

The F-Droid runs use the same binary under
`~/.claude/skills/memory-guard/memguard.sh` at a 116 GiB cap, with a 90-minute
timeout per phase, sampling the process tree's footprint every 5 s. The APKs are
the ones already fetched under `/Volumes/Shampoo/ct-bigapk/apks`, chosen in the
earlier `ctadl-capacity-limits.md` work by reading each candidate's zip central
directory over HTTP range requests, so dex and native sizes were known before
downloading.

Relation counts come from `RUST_LOG=warn,ctadl=debug`, which makes
`IndexStats::log` report them at the end of `index`; the input counts come from
`duckdb` over the `*.parquet` files in each project's index directory.

One run per configuration on the big apps, and the two failures are floors, not
measurements - `ceno` and `greenbits` did not finish, so their real cost is
unknown and larger.

Raw data, kept outside the repo:

```
/Volumes/Shampoo/ct-tb-20260922/
  logs/measure-<tag>.tsv        tag, app, phase, seconds, peak bytes, max rss, exit
  logs/suite-<tag>.log          full suite output per run
  logs/perapp.txt               the per-app default-vs-none table
  bigapk/logs/runs.log          one block per F-Droid run, with relation counters
  bigapk/runs/<pkg>.dex.<cfg>/  per-run import/index stdout+stderr, memory series, store
  run-config.sh, sweep-all.sh   the suite runners
  run-big.sh, ladder.sh         the F-Droid runners
  shim/ctadl                    the measurement shim
```

The per-phase suite measurements are also committed here as
`taintbench-timing-20260922.tsv`, so the tables above can be rebuilt without the
scratch volume.
