# What the call-graph rework did to TaintBench - DO-NOT-MERGE

Two commits landed on `main` between 12 and 14 September that change what the
TaintBench suite finds and what it costs to run:

```
0183c42c Andersen style aliasing in query (#128)
a5f4f557 Fully hybrid call graph (#127)
2ba7e014 Delete CLAUDE md (#126)          <- the baseline measured here
```

This document measures all 38 apps at three points: before both commits, with
`#127` only, and with both. Short version:

- **`#127` costs time and memory and finds the same things.** Indexing the suite
  goes from 134 s to 259 s and the worst app's peak memory from 2.8 GB to
  6.5 GB. The suite still passes 38/38.
- **`#128` buys back the time and then some, but loses flows.** Querying the
  suite drops from 297 s to 31 s, and detection drops from 92 of TaintBench's
  191 ground-truth flows to 77. Sixteen apps fail the suite.
- Together they leave the suite **faster overall but heavier**: 471 s to 389 s
  wall-clock, and peak memory up on 26 of the 38 apps.

Some of `#128`'s speedup is simply work not done. `roidsec`'s query goes from
170 s to 8 s and loses two of its six flows; `proxy_samp`'s goes from 6.8 s to
0.2 s and loses three of nine. A query that stops finding paths stops paying for
them, so the time and the recall numbers have to be read together.

On the other side of the ledger, `#128` is more precise. False positives drop
from 8 to 3: `proxy_samp` #16 and #20 and `save_me` #26-28 are no longer
reported, and only `phospy` #3-#5 remain. Those five were the ones
`taintbench-false-positives.md` attributes to two-way copies and to models that
let an object hand taint back; that document's mechanisms 1 and 2 are evidently
narrower now.

## What stopped being detected

Sixteen apps report a regression - a flow in their committed `expected.json`
that is no longer found:

| app | findings lost | now detected |
| --- | --- | --- |
| `remote_control_smack` | 1, 2, 5, 6, 9, 16 | 11/17 (and 11 newly found) |
| `proxy_samp` | 3, 7, 9 | 6/17 |
| `roidsec` | 3, 4 | 4/6 |
| `smssilience_fake_vertu` | 1, 2 | 0/2 |
| `hummingbad_android_samp` | 1, 2 | 0/2 |
| `cajino_baidu` | 1 | 10/12 |
| `dsencrypt_samp` | 1 | 0/1 |
| `exprespam` | 2 | 1/2 |
| `fakebank_android_samp` | 3 | 2/5 |
| `overlay_android_samp` | 3 | 2/4 |
| `overlaylocker2_android_samp` | 7 | 3/7 |
| `samsapo` | 2 | 1/4 |
| `smssend_packageInstaller` | 2 | 2/5 |
| `stels_flashplayer_android_update` | 3 | 1/3 |
| `threatjapan_uracto` | 2 | 0/2 |
| `vibleaker_android_samp` | 2 | 2/4 |

`remote_control_smack` is the one mixed case: it loses six baseline findings and
picks up eleven others, ending at 11/17 against 6/17 before. Everything else in
the table is a straight loss.

Across the suite that is 26 flows lost and 11 gained, 92 down to 77. Three apps
that used to find every flow they had - `dsencrypt_samp`, `hummingbad_android_samp`,
`smssilience_fake_vertu` - now find none.

**The committed `expected.json` files were left alone.** Rebaselining them would
make the suite green and erase the finding; they stay as they are so the check
keeps reporting the sixteen failures until somebody decides the new behaviour is
the intended one.

## Per benchmark

"Before" is `2ba7e014`, "after" is `0183c42c` (`origin/main`). Time is the whole
run for that app - import, index and query - and memory is the largest physical
footprint any one of those three phases reached. Both are the median of three
runs. ⚠️ marks an app that lost a baseline finding.

| app | flows found before | flows found after | time before (s) | time after (s) | time | peak mem before (MB) | peak mem after (MB) | mem |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `backflash` | 6/13 | 6/13 | 0.92 | 0.85 | -8% | 89 | 128 | +43% |
| `beita_com_beita_contact` | 1/3 | 1/3 | 1.50 | 1.48 | -1% | 164 | 223 | +35% |
| `cajino_baidu` ⚠️ | 11/12 | 10/12 | 13.66 | 5.41 | -60% | 612 | 688 | +12% |
| `chulia` | 0/4 | 0/4 | 0.14 | 0.13 | -7% | 16 | 20 | +23% |
| `death_ring_materialflow` | 1/1 | 1/1 | 0.36 | 0.36 | +0% | 46 | 65 | +41% |
| `dsencrypt_samp` ⚠️ | 1/1 | 0/1 | 0.13 | 0.07 | -46% | 10 | 8 | -13% |
| `exprespam` ⚠️ | 2/2 | 1/2 | 0.09 | 0.07 | -22% | 11 | 9 | -22% |
| `fakeappstore` | 1/3 | 1/3 | 0.77 | 0.82 | +6% | 80 | 119 | +49% |
| `fakebank_android_samp` ⚠️ | 3/5 | 2/5 | 0.74 | 0.79 | +7% | 79 | 127 | +62% |
| `fakedaum` | 1/2 | 1/2 | 0.61 | 0.54 | -11% | 77 | 89 | +16% |
| `fakemart` | 0/2 | 0/2 | 0.59 | 0.70 | +19% | 87 | 132 | +52% |
| `fakeplay` | 0/2 | 0/2 | 1.75 | 2.29 | +31% | 215 | 267 | +24% |
| `faketaobao` | 0/4 | 0/4 | 0.47 | 0.59 | +26% | 78 | 117 | +50% |
| `godwon_samp` | 0/6 | 0/6 | 0.05 | 0.06 | +20% | 8 | 8 | -3% |
| `hummingbad_android_samp` ⚠️ | 2/2 | 0/2 | 10.50 | 9.33 | -11% | 1170 | 1444 | +23% |
| `jollyserv` | 0/1 | 0/1 | 1.30 | 1.28 | -2% | 151 | 206 | +37% |
| `overlay_android_samp` ⚠️ | 3/4 | 2/4 | 0.38 | 0.37 | -3% | 44 | 61 | +37% |
| `overlaylocker2_android_samp` ⚠️ | 4/7 | 3/7 | 2.20 | 1.73 | -21% | 161 | 250 | +55% |
| `phospy` | 2/2 | 2/2 | 3.49 | 0.62 | -82% | 86 | 102 | +19% |
| `proxy_samp` ⚠️ | 9/17 | 6/17 | 6.83 | 0.24 | -96% | 91 | 15 | -84% |
| `remote_control_smack` | 6/17 | 11/17 | 133.82 | 125.96 | -6% | 3373 | 3824 | +13% |
| `repane` | 1/1 | 1/1 | 0.07 | 0.07 | +0% | 9 | 9 | -4% |
| `roidsec` ⚠️ | 6/6 | 4/6 | 170.03 | 7.84 | -95% | 1343 | 89 | -93% |
| `samsapo` ⚠️ | 2/4 | 1/4 | 1.49 | 1.18 | -21% | 125 | 197 | +58% |
| `save_me` | 10/25 | 10/25 | 3.45 | 3.93 | +14% | 352 | 508 | +44% |
| `scipiex` | 0/3 | 0/3 | 1.06 | 1.20 | +13% | 124 | 201 | +62% |
| `slocker_android_samp` | 0/5 | 0/5 | 0.69 | 0.85 | +23% | 90 | 142 | +58% |
| `sms_google` | 2/4 | 2/4 | 3.42 | 0.22 | -94% | 61 | 20 | -67% |
| `sms_send_locker_qqmagic` | 5/6 | 5/6 | 8.08 | 0.15 | -98% | 93 | 13 | -86% |
| `smssend_packageInstaller` ⚠️ | 3/5 | 2/5 | 1.20 | 0.79 | -34% | 136 | 123 | -9% |
| `smssilience_fake_vertu` ⚠️ | 2/2 | 0/2 | 0.18 | 0.07 | -61% | 12 | 8 | -29% |
| `smsstealer_kysn_assassincreed_android_samp` | 0/5 | 0/5 | 0.92 | 1.12 | +22% | 126 | 198 | +58% |
| `stels_flashplayer_android_update` ⚠️ | 2/3 | 1/3 | 2.15 | 0.76 | -65% | 125 | 117 | -7% |
| `tetus` | 2/2 | 2/2 | 0.17 | 0.18 | +6% | 22 | 25 | +13% |
| `the_interview_movieshow` | 0/1 | 0/1 | 0.06 | 0.06 | +0% | 8 | 8 | -6% |
| `threatjapan_uracto` ⚠️ | 1/2 | 0/2 | 0.48 | 0.55 | +15% | 74 | 112 | +51% |
| `vibleaker_android_samp` ⚠️ | 3/4 | 2/4 | 28.37 | 11.90 | -58% | 1084 | 1812 | +67% |
| `xbot_android_samp` | 0/3 | 0/3 | 68.76 | 204.65 | +198% | 2789 | 6255 | +124% |
| **all 38** | **92/191** | **77/191** | **471** | **389** | **-17%** | **3373** (max) | **6255** (max) | **+85%** |


Reading the totals: the suite as a whole got 17% faster and the heaviest app got
124% heavier. `xbot_android_samp` is the outlier in both directions - three
times the time and 6.3 GB of peak memory, on an app that finds none of its three
flows either before or after.

## Which of the two commits did what

Splitting the suite by phase separates them cleanly:

| | index (all 38) | query (all 38) | worst app's peak memory |
| --- | ---: | ---: | ---: |
| `2ba7e014` before | 134 s | 243 s | 3.4 GB |
| `a5f4f557` (`#127` only) | 259 s | 297 s | 6.5 GB |
| `0183c42c` (both, `origin/main`) | 264 s | 31 s | 6.1 GB |

- **`#127` is the cost.** Index time roughly doubles and stays there; `#128`
  adds nothing on top (259 s to 264 s). Peak memory nearly doubles at `#127` and
  `#128` gives a little back. Detection at `#127` alone is unchanged except that
  `threatjapan_uracto` picks up one more flow, so the suite passes 38/38 there.
- **`#128` is the recall loss and the query speedup.** Query time falls by 89%
  and every one of the sixteen regressions appears only once `#128` is in.

Two apps show `#127` blowing up the *query* in a way `#128` then repairs:
`fakeplay` goes 0.9 s → 68.5 s → 1.0 s, and `samsapo`'s peak memory goes
125 MB → 1313 MB → 197 MB. Neither is visible in the end-to-end numbers, but
both would bite anything that ships `#127` without `#128`.

`vibleaker_android_samp` is the one app `#127` makes *faster* to index: 23.3 s
down to 8.1 s, though its peak memory still rises from 1.1 GB to 1.8 GB.

### Per phase, per app

Seconds for index and query, MB for peak memory; median of the runs. Columns are
before / `#127` only / both.

| app | index before | index +#127 | index after both | query before | query +#127 | query after both | peak mem before | peak mem +#127 | peak mem after both |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `backflash` | 0.27 | 0.44 | 0.45 | 0.58 | 0.58 | 0.30 | 89 | 127 | 128 |
| `beita_com_beita_contact` | 0.50 | 0.81 | 0.84 | 0.87 | 1.27 | 0.51 | 164 | 348 | 223 |
| `cajino_baidu` | 1.89 | 3.08 | 3.07 | 11.37 | 11.80 | 1.93 | 612 | 701 | 688 |
| `chulia` | 0.04 | 0.06 | 0.06 | 0.09 | 0.10 | 0.06 | 16 | 20 | 20 |
| `death_ring_materialflow` | 0.12 | 0.19 | 0.19 | 0.20 | 0.20 | 0.13 | 46 | 65 | 65 |
| `dsencrypt_samp` | 0.01 | 0.02 | 0.02 | 0.11 | 0.11 | 0.04 | 10 | 10 | 8 |
| `exprespam` | 0.01 | 0.02 | 0.02 | 0.08 | 0.08 | 0.05 | 11 | 11 | 9 |
| `fakeappstore` | 0.30 | 0.50 | 0.51 | 0.39 | 0.40 | 0.23 | 80 | 127 | 119 |
| `fakebank_android_samp` | 0.24 | 0.42 | 0.42 | 0.42 | 0.45 | 0.30 | 79 | 131 | 127 |
| `fakedaum` | 0.17 | 0.29 | 0.28 | 0.40 | 0.48 | 0.21 | 77 | 88 | 89 |
| `fakemart` | 0.28 | 0.43 | 0.43 | 0.23 | 0.23 | 0.19 | 87 | 132 | 132 |
| `fakeplay` | 0.67 | 1.08 | 1.08 | 0.92 | 68.50 | 1.04 | 215 | 753 | 267 |
| `faketaobao` | 0.22 | 0.39 | 0.38 | 0.19 | 0.20 | 0.15 | 78 | 121 | 117 |
| `godwon_samp` | 0.01 | 0.02 | 0.02 | 0.04 | 0.04 | 0.04 | 8 | 8 | 8 |
| `hummingbad_android_samp` | 3.96 | 5.93 | 5.92 | 5.77 | 6.63 | 2.64 | 1170 | 1449 | 1444 |
| `jollyserv` | 0.46 | 0.79 | 0.79 | 0.72 | 1.13 | 0.37 | 151 | 233 | 206 |
| `overlay_android_samp` | 0.12 | 0.19 | 0.18 | 0.22 | 0.22 | 0.15 | 44 | 61 | 61 |
| `overlaylocker2_android_samp` | 0.58 | 1.00 | 1.00 | 1.47 | 1.99 | 0.58 | 161 | 252 | 250 |
| `phospy` | 0.17 | 0.27 | 0.27 | 3.27 | 4.29 | 0.30 | 86 | 107 | 102 |
| `proxy_samp` | 0.03 | 0.04 | 0.04 | 6.79 | 7.62 | 0.19 | 91 | 98 | 15 |
| `remote_control_smack` | 30.42 | 30.40 | 30.85 | 14.08 | 12.69 | 5.75 | 3373 | 3830 | 3824 |
| `repane` | 0.01 | 0.02 | 0.02 | 0.05 | 0.06 | 0.04 | 9 | 9 | 9 |
| `roidsec` | 0.03 | 0.04 | 0.04 | 169.99 | 149.51 | 7.79 | 1343 | 1290 | 89 |
| `samsapo` | 0.42 | 0.70 | 0.70 | 0.93 | 4.89 | 0.34 | 125 | 1313 | 197 |
| `save_me` | 1.54 | 2.46 | 2.46 | 1.57 | 2.56 | 1.13 | 352 | 513 | 508 |
| `scipiex` | 0.45 | 0.72 | 0.72 | 0.49 | 0.49 | 0.36 | 124 | 201 | 201 |
| `slocker_android_samp` | 0.31 | 0.53 | 0.53 | 0.29 | 0.29 | 0.23 | 90 | 139 | 142 |
| `sms_google` | 0.04 | 0.07 | 0.06 | 3.36 | 2.50 | 0.14 | 61 | 54 | 20 |
| `sms_send_locker_qqmagic` | 0.03 | 0.03 | 0.03 | 8.04 | 6.92 | 0.11 | 93 | 81 | 13 |
| `smssend_packageInstaller` | 0.26 | 0.44 | 0.43 | 0.88 | 1.07 | 0.29 | 136 | 206 | 123 |
| `smssilience_fake_vertu` | 0.01 | 0.02 | 0.02 | 0.16 | 0.15 | 0.04 | 12 | 12 | 8 |
| `smsstealer_kysn_assassincreed_android_samp` | 0.42 | 0.72 | 0.72 | 0.39 | 0.40 | 0.29 | 126 | 193 | 198 |
| `stels_flashplayer_android_update` | 0.22 | 0.38 | 0.38 | 1.87 | 1.83 | 0.32 | 125 | 125 | 117 |
| `tetus` | 0.05 | 0.08 | 0.08 | 0.11 | 0.11 | 0.09 | 22 | 25 | 25 |
| `the_interview_movieshow` | 0.01 | 0.02 | 0.02 | 0.04 | 0.04 | 0.03 | 8 | 9 | 8 |
| `threatjapan_uracto` | 0.20 | 0.34 | 0.34 | 0.22 | 0.29 | 0.16 | 74 | 114 | 112 |
| `vibleaker_android_samp` | 23.26 | 8.13 | 8.12 | 4.25 | 4.68 | 2.93 | 1084 | 1795 | 1812 |
| `xbot_android_samp` | 66.55 | 197.86 | 202.29 | 1.85 | 2.33 | 2.00 | 2789 | 6657 | 6255 |

## How this was measured

Three binaries, each built with `nix build .#packages.aarch64-darwin.default`
from the same branch code, differing only in what they sit on top of:

| point | base | branch content |
| --- | --- | --- |
| before | `2ba7e014` | this branch's suite and query-memory work |
| `#127` only | `a5f4f557` | same |
| after | `0183c42c` | same (this is the rebased branch) |

The branch's temporary index counters (`facts::counters`) are left out of all
three. `origin/main` deletes the call-string lattice they count, so the rebase
has to drop them anyway; dropping them from the baseline too keeps the three
points differing only by the commits under test.

Each run is the ordinary suite - `xtask taintbench` over all 38 apps, APKs from
the Nix store - with `ctadl` on `PATH` replaced by a shim that runs the real
binary under `/usr/bin/time -l` and records wall time and **peak memory
footprint** per phase. Footprint rather than RSS: macOS compresses cold pages,
so RSS understates a big run badly.

Runs were sequential on an idle M1 Ultra (20 cores, 128 GB), three repetitions
for the before and after points and two for `#127`-only, alternating the order
so machine drift could not favour one side. Repeats agree closely - suite
wall-clock came in at 493/466/476 s before and 395/389/394 s after - and every
repetition of a configuration produced identical per-finding verdicts.

One check on the baseline: the branch's own pre-rebase state (on `99bbed37`,
five commits older than `2ba7e014`) measures the same to within a second on
every phase, and reproduces the committed `taintbench-run-8baae049.log`
verdict-for-verdict. The five commits in between changed nothing this suite can
see, so `2ba7e014` is a fair stand-in for "the branch as it was".

Raw data, kept outside the repo:

```
/Volumes/Shampoo/ct-taintbench-rebase-20260917/
  logs/suite-<tag>.log        full suite output per run
  logs/measure-<tag>.tsv      tag, app, phase, seconds, peak bytes, max rss, exit
  report.py, compare.py       the scripts that build the tables above
```

The per-phase measurements are also committed here as
`taintbench-timing-20260917.tsv`, so the tables can be rebuilt without the
scratch volume.

## What to decide next

The suite is red and it should stay red until one of these happens:

1. `#128`'s aliasing is adjusted so the 26 lost flows come back, or
2. the loss is accepted as the price of the precision gain and the five fewer
   false positives, and `expected.json` is rebaselined deliberately, with this
   document as the record of what changed.

The memory growth from `#127` is a separate question and does not depend on that
choice. `xbot_android_samp` at 6.3 GB is the app to look at first: it is three
times slower to index and finds nothing either way, so whatever the new call
graph is enumerating there is pure cost.
