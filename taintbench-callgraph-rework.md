# What the call-graph rework did to TaintBench - DO-NOT-MERGE

Two commits landed on `main` between 12 and 14 September that change what the
TaintBench suite finds and what it costs to run:

```
0183c42c Andersen style aliasing in query (#128)
a5f4f557 Fully hybrid call graph (#127)
2ba7e014 Delete CLAUDE md (#126)          <- the baseline measured here
```

This document measures all 38 apps at four points - neither commit, each one on
its own, and both - so the two can be told apart. `#128` applies to `2ba7e014`
without `#127` cleanly and compiles, which is what makes the fourth point
possible.

Short version:

- **`#127` costs time and memory and finds the same things.** Indexing the suite
  goes from 134 s to 259 s and the worst app's peak memory from 2.8 GB to
  6.5 GB. The suite still passes 38/38 with `#127` alone.
- **`#128` is where the recall goes.** Querying the suite drops from 243 s to
  30 s, and detection drops from 92 of TaintBench's 191 ground-truth flows to
  76. It does this with or without `#127`.
- Together they leave the suite **faster overall but heavier**: 471 s to 389 s
  wall-clock, detection at 77, and peak memory up on 26 of the 38 apps.
- **`#128` on its own is the cheapest of the four**, by a wide margin: 258 s for
  the suite at 3.4 GB peak, against 389 s and 6.3 GB for `main`. It finds 76
  flows where `main` finds 77.
- **`origin/blowup2` (`28f949ea`) removes most of `#127`'s cost.** Two commits on
  top of `main` bring suite index time back from 264 s to 147 s - within 9% of
  the pre-`#127` baseline - and the worst app's peak memory from 6.3 GB to
  4.6 GB, finding exactly what `main` finds. It is the fifth point measured
  here.

The two effects barely interact. `#127` adds about 125 s of index time and
doubles peak memory whether or not `#128` is present; `#128` removes about 210 s
of query time and 16 flows whether or not `#127` is present. The one place they
touch is `fakedaum` #2, which `#128` alone loses and `#127` puts back.

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

Sixteen apps report a regression on `main` - a flow in their committed
`expected.json` that is no longer found:

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

With `#128` alone it is the same sixteen plus `fakedaum` (#2), seventeen in all.

`remote_control_smack` is the one mixed case: it loses six baseline findings and
picks up eleven others, ending at 11/17 against 6/17 before. Everything else in
the table is a straight loss.

Across the suite that is 26 flows lost and 11 gained, 92 down to 77. Three apps
that used to find every flow they had - `dsencrypt_samp`, `hummingbad_android_samp`,
`smssilience_fake_vertu` - now find none.

**The committed `expected.json` files were left alone.** Rebaselining them would
make the suite green and erase the finding; they stay as they are so the check
keeps reporting the failures until somebody decides the new behaviour is the
intended one.

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

The four points, each the median of its runs:

| | index (all 38) | query (all 38) | worst app's peak | flows found | false positives |
| --- | ---: | ---: | ---: | ---: | ---: |
| `2ba7e014`, neither | 134 s | 243 s | 3.4 GB | 92/191 | 8 |
| `#127` only | 259 s | 297 s | 6.5 GB | 93/191 | 8 |
| `#128` only | 134 s | 30 s | 3.4 GB | 76/191 | 3 |
| both (`origin/main`) | 264 s | 31 s | 6.1 GB | 77/191 | 3 |
| both + `28f949ea` (`origin/blowup2`) | 147 s | 31 s | 4.6 GB | 77/191 | 3 |

Read down the columns and the two commits separate almost perfectly:

- **`#127` is the cost and nothing else.** It adds ~125 s of index time and
  roughly doubles peak memory, in both the presence and the absence of `#128`,
  and it does not cost a single flow - detection goes *up* by one (both 92 → 93
  and 76 → 77 are `threatjapan_uracto` #1, with `fakedaum` #2 the same story in
  the `#128` column).
- **`#128` is the recall loss and the query speedup**, also in both the presence
  and the absence of `#127`: query time down ~87%, 16 flows gone, false
  positives 8 → 3.

Two apps show `#127` blowing up the *query* in a way `#128` then masks:
`fakeplay` goes 0.9 s → 68.5 s → 1.0 s, and `samsapo`'s peak memory goes
125 MB → 1313 MB → 197 MB. Neither is visible in the end-to-end numbers for
`main`, but both would bite anything that ships `#127` without `#128`.

`vibleaker_android_samp` is the one app `#127` makes *faster* to index: 23.3 s
down to 8.1 s, though its peak memory still rises from 1.1 GB to 1.8 GB.

The uncomfortable row is the third one. `#128` by itself runs the suite in 258 s
at 3.4 GB and finds 76 flows; `main` runs it in 389 s at 6.3 GB and finds 77.
Whatever `#127` is for, this suite does not show it paying for itself.

## How `#127`'s cost is shaped

The suite total hides two different things, and they want separating.

**For almost every app it is a flat multiplier, not worse scaling.** Across the
24 apps whose index takes more than 0.05 s and which behave uniformly, `#127`
multiplies index time by a median of **1.65x**, in a tight band from 1.00x to
1.77x. That multiplier has no relationship to app size: the correlation between
an app's slowdown and the log of its APK size is **r = -0.03**. A cost that grows
with the input would show up here as a slowdown that climbs with size, and it
does not.

(APK size is a weak proxy for analysis work in any case - index time at the
baseline fits size with an R^2 of only 0.47, and apps of similar size differ by
four orders of magnitude in index time - so the suite cannot say much about the
*exponent*. What it can say cleanly is that the multiplier is flat.)

**Two apps break the pattern, in opposite directions.**

| app | index before | index +`#127` | | peak memory |
| --- | ---: | ---: | --- | --- |
| `vibleaker_android_samp` (5.4 MB, the largest APK) | 23.3 s | 8.1 s | x0.35 | 1.1 -> 1.8 GB |
| `xbot_android_samp` | 66.6 s | 197.9 s | x2.97 | 2.8 -> 6.7 GB |

So `#127` changed *which* apps are expensive rather than lowering the cost
overall: a 3x win on the largest app and a 3x loss on the most expensive one.
Both come out of the same mechanism - see "Where the constant factor comes from"
below.

What `xbot` costs it is not more hybrid inlining - by `#127`'s own counters the
critical summaries go down (6492 -> 5475) and the resolvent with them
(8884 -> 6780). It is the propagation relations getting bigger:
`context_assign` 132k -> 396k rows, `context_locals` 8.2M -> 10.7M,
`assign_like` 979k -> 1.28M. The new call graph feeds more edges into the same
closure. `xbot` embeds Rhino (`org.mozilla.javascript`), and 87% of its 18813
virtual call sites resolve by CHA rather than by inlining.

## Where the constant factor comes from

The 1.65x is not `#127` computing more answers. It is `#127` building a new
layer of intermediate structure to compute the same answers a different way.

**The outputs barely move.** Median across ten apps spanning the suite
(`backflash`, `beita_com_beita_contact`, `cajino_baidu`, `fakeplay`,
`hummingbad_android_samp`, `jollyserv`, `overlaylocker2_android_samp`,
`samsapo`, `save_me`, `scipiex`), `#127` against the baseline:

| relation | ratio |
| --- | ---: |
| `assign_like` (final) | **x1.01** |
| `locals` (final) | x1.18 |
| `summary` | x1.37 |
| `call_target_assign_like` | **x0.69** |
| `context_assign` | **x0.12** |
| `context_locals` | **x0.14** |
| `critical_summary` | **x0.15** |
| `resolvent` | x0.50 |
| index time | **x1.62** |

`assign_like` is flat, and everything contextual *collapses* - an eighth of the
contextual assigns, a seventh of the contextual locals, a seventh of the
critical summaries, and a third fewer call-target assigns. The call graph got
smaller and more precise, exactly as intended. Time still went up 62%.

**What went up is machinery that did not exist before.** `#127` adds
`index_engine/{path_group,path_set,decision}.rs` - none of the three is present
at `2ba7e014` - and four new relations in the propagation rules (`reach_vp`,
`locals_key`, `edge_split`, `ext_dst`; also absent at `2ba7e014`). Rather than
joining on the vertex and testing the path prefix afterwards, every path is
split once at each point it could match a prefix, the join is keyed on the
split, and the extension is looked up. That is a better plan asymptotically, and
the splits have to be built, stored and indexed:

| app | `locals` rows | new relations, summed | ratio |
| --- | ---: | ---: | ---: |
| `backflash` | 69715 | 336555 | x4.83 |
| `beita_com_beita_contact` | 125226 | 633431 | x5.06 |
| `cajino_baidu` | 432968 | 2017431 | x4.66 |
| `hummingbad_android_samp` | 870255 | 3906682 | x4.49 |
| `samsapo` | 166967 | 524827 | x3.14 |
| `save_me` | 483518 | 1773995 | x3.67 |

Between three and five times as many intermediate rows as the relation they
serve, on every app, in fixed proportion to the program. That is the shape of a
flat multiplier.

**The profile agrees.** Sampling `hummingbad`'s index at 1 ms under both builds
and attributing leaf samples:

| component | before | after |
| --- | ---: | ---: |
| allocation (`libsystem_malloc`) | 352 | 1038 |
| `path_group` + `path_set` | **0** | **426** |
| `assign_like_trie` | 24 | 209 |
| `locals_trie` | 193 | 301 |
| `index_engine` (rule bodies) | 356 | 520 |
| `hybrid_set` | 30 | 126 |

Allocation nearly triples and is the largest single bucket; the new path
machinery goes from nothing to 11% of the profile; trie insertion grows with it.
No pre-existing component dominates the delta - it is spread across building and
indexing the new rows.

**The same mechanism explains both outliers.** What varies between apps is
whether the contextual work `#127` saves is big enough to pay for the splitting
it adds:

| app | `critical_summary` | `context_locals` | index |
| --- | --- | --- | --- |
| `vibleaker` (x0.35, the win) | 7155 -> **1394** | 7613 -> 5525 | 23.3 s -> 8.1 s |
| `xbot` (x2.97, the loss) | 6492 -> 5475 | 8.2M -> **10.7M** | 66.6 s -> 197.9 s |

`vibleaker` was paying for critical summaries, and `#127` removes four fifths of
them. `xbot`'s contextual closure *grew* by 2.5M rows instead of shrinking, so
it pays the new machinery and gets nothing back. A typical small app is neither
case: it has little contextual work to save, so it just pays.

That is also why `origin/blowup2` does not move the constant. On
`hummingbad`, `cajino_baidu` and `backflash` its propagation-relation counts are
**byte-identical** to `#127`'s - the machinery is untouched. The fixes go after
`xbot`'s contextual blowup, which is the tail, not the tax.

The reading this supports: `#127` buys a cheaper contextual closure with a fixed
per-program cost. On inputs where the contextual closure is the bottleneck that
is a win, and `vibleaker` - the largest APK here - is the one app in the suite
big enough to show it. On 27 KB to 5.4 MB of Android malware it mostly is not
the bottleneck, so what the suite sees is the bill.

## What the blowup fixes change

`origin/blowup2` is two commits on top of `origin/main`:

```
28f949ea fix 2
208d85a7 more
0183c42c Andersen style aliasing in query (#128)   <- origin/main
```

They go after exactly the tail described above, and they get most of it:

| | index (all 38) | vs baseline | worst app's peak | vs baseline |
| --- | ---: | ---: | ---: | ---: |
| `2ba7e014`, neither | 134 s | - | 3.4 GB | - |
| `origin/main` | 264 s | x1.96 | 6.1 GB | x1.85 |
| `origin/blowup2` | 147 s | **x1.09** | 4.6 GB | **x1.39** |

`xbot` alone accounts for nearly all of it: 202 s down to 84 s and 6.3 GB down
to 4.6 GB. No other app moves by more than 2%, and the per-finding verdicts are
**identical to `main`** - same 22 passes, same 16 failures, same 77 flows, same
3 false positives. These are index-side fixes; they do not touch `#128`'s
recall loss, and nothing suggests they were meant to.

What they do not remove is the flat multiplier. The per-app median index
slowdown against the pre-`#127` baseline is 1.65x at `#127`, 1.66x at `main`,
and **1.64x** with the fixes - unchanged. Peak memory tells the same story: the
worst app comes down from x1.85 to x1.39, but the per-app median stays at x1.24
and 26 of the 38 apps are still more than 10% heavier than before `#127`.

So on this suite the fixes turn `#127` from "doubles the suite's index time"
into "costs 9% overall, and 1.65x on a typical small app". The tail is handled;
the constant factor is not.

### Total time per app, seconds

| app | none | #127 | #128 | both | both+fixes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `backflash` | 0.92 | 1.10 | 0.65 | 0.85 | 0.81 |
| `beita_com_beita_contact` | 1.50 | 2.21 | 1.10 | 1.48 | 1.43 |
| `cajino_baidu` | 13.66 | 15.29 | 4.13 | 5.41 | 5.42 |
| `chulia` | 0.14 | 0.16 | 0.11 | 0.13 | 0.13 |
| `death_ring_materialflow` | 0.36 | 0.43 | 0.29 | 0.36 | 0.36 |
| `dsencrypt_samp` | 0.13 | 0.14 | 0.06 | 0.07 | 0.07 |
| `exprespam` | 0.09 | 0.10 | 0.06 | 0.07 | 0.07 |
| `fakeappstore` | 0.77 | 0.97 | 0.60 | 0.82 | 0.82 |
| `fakebank_android_samp` | 0.74 | 0.94 | 0.70 | 0.79 | 0.79 |
| `fakedaum` | 0.61 | 0.81 | 0.38 | 0.54 | 0.53 |
| `fakemart` | 0.59 | 0.74 | 0.54 | 0.70 | 0.69 |
| `fakeplay` | 1.75 | 69.74 | 1.25 | 2.29 | 2.29 |
| `faketaobao` | 0.47 | 0.64 | 0.44 | 0.59 | 0.59 |
| `godwon_samp` | 0.05 | 0.06 | 0.05 | 0.06 | 0.06 |
| `hummingbad_android_samp` | 10.50 | 13.33 | 7.29 | 9.33 | 9.36 |
| `jollyserv` | 1.30 | 2.04 | 0.94 | 1.28 | 1.27 |
| `overlay_android_samp` | 0.38 | 0.45 | 0.31 | 0.37 | 0.38 |
| `overlaylocker2_android_samp` | 2.20 | 3.14 | 1.29 | 1.73 | 1.74 |
| `phospy` | 3.49 | 4.61 | 0.53 | 0.62 | 0.62 |
| `proxy_samp` | 6.83 | 7.67 | 0.23 | 0.24 | 0.24 |
| `remote_control_smack` | 133.82 | 132.55 | 126.00 | 125.96 | 126.96 |
| `repane` | 0.07 | 0.08 | 0.07 | 0.07 | 0.07 |
| `roidsec` | 170.03 | 149.56 | 7.81 | 7.84 | 7.89 |
| `samsapo` | 1.49 | 5.73 | 0.90 | 1.18 | 1.19 |
| `save_me` | 3.45 | 5.36 | 2.96 | 3.93 | 3.95 |
| `scipiex` | 1.06 | 1.34 | 0.93 | 1.20 | 1.21 |
| `slocker_android_samp` | 0.69 | 0.91 | 0.63 | 0.85 | 0.85 |
| `sms_google` | 3.42 | 2.58 | 0.20 | 0.22 | 0.22 |
| `sms_send_locker_qqmagic` | 8.08 | 6.96 | 0.14 | 0.15 | 0.15 |
| `smssend_packageInstaller` | 1.20 | 1.58 | 0.60 | 0.79 | 0.78 |
| `smssilience_fake_vertu` | 0.18 | 0.18 | 0.06 | 0.07 | 0.07 |
| `smsstealer_kysn_assassincreed_android_samp` | 0.92 | 1.24 | 0.80 | 1.12 | 1.12 |
| `stels_flashplayer_android_update` | 2.15 | 2.28 | 0.60 | 0.76 | 0.76 |
| `tetus` | 0.17 | 0.20 | 0.15 | 0.18 | 0.18 |
| `the_interview_movieshow` | 0.06 | 0.08 | 0.05 | 0.06 | 0.06 |
| `threatjapan_uracto` | 0.48 | 0.69 | 0.40 | 0.55 | 0.55 |
| `vibleaker_android_samp` | 28.37 | 13.65 | 26.89 | 11.90 | 11.90 |
| `xbot_android_samp` | 68.76 | 200.55 | 68.31 | 204.65 | 86.76 |
| **all 38** | **470.88** | **650.09** | **258.43** | **389.21** | **272.34** |

### Peak memory per app, MB

| app | none | #127 | #128 | both | both+fixes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `backflash` | 89 | 127 | 90 | 128 | 128 |
| `beita_com_beita_contact` | 164 | 348 | 137 | 223 | 219 |
| `cajino_baidu` | 612 | 701 | 453 | 688 | 687 |
| `chulia` | 16 | 20 | 14 | 20 | 19 |
| `death_ring_materialflow` | 46 | 65 | 46 | 65 | 64 |
| `dsencrypt_samp` | 10 | 10 | 9 | 8 | 8 |
| `exprespam` | 11 | 11 | 9 | 9 | 9 |
| `fakeappstore` | 80 | 127 | 74 | 119 | 124 |
| `fakebank_android_samp` | 79 | 131 | 80 | 127 | 128 |
| `fakedaum` | 77 | 88 | 57 | 89 | 88 |
| `fakemart` | 87 | 132 | 89 | 132 | 130 |
| `fakeplay` | 215 | 753 | 183 | 267 | 267 |
| `faketaobao` | 78 | 121 | 77 | 117 | 117 |
| `godwon_samp` | 8 | 8 | 8 | 8 | 8 |
| `hummingbad_android_samp` | 1170 | 1449 | 892 | 1444 | 1437 |
| `jollyserv` | 151 | 233 | 131 | 206 | 206 |
| `overlay_android_samp` | 44 | 61 | 44 | 61 | 61 |
| `overlaylocker2_android_samp` | 161 | 252 | 161 | 250 | 251 |
| `phospy` | 86 | 107 | 61 | 102 | 102 |
| `proxy_samp` | 91 | 98 | 15 | 15 | 15 |
| `remote_control_smack` | 3373 | 3830 | 3433 | 3824 | 3809 |
| `repane` | 9 | 9 | 9 | 9 | 9 |
| `roidsec` | 1343 | 1290 | 89 | 89 | 89 |
| `samsapo` | 125 | 1313 | 127 | 197 | 202 |
| `save_me` | 352 | 513 | 359 | 508 | 507 |
| `scipiex` | 124 | 201 | 128 | 201 | 201 |
| `slocker_android_samp` | 90 | 139 | 89 | 142 | 140 |
| `sms_google` | 61 | 54 | 17 | 20 | 20 |
| `sms_send_locker_qqmagic` | 93 | 81 | 12 | 13 | 12 |
| `smssend_packageInstaller` | 136 | 206 | 82 | 123 | 125 |
| `smssilience_fake_vertu` | 12 | 12 | 8 | 8 | 8 |
| `smsstealer_kysn_assassincreed_android_samp` | 126 | 193 | 126 | 198 | 198 |
| `stels_flashplayer_android_update` | 125 | 125 | 78 | 117 | 116 |
| `tetus` | 22 | 25 | 17 | 25 | 25 |
| `the_interview_movieshow` | 8 | 9 | 8 | 8 | 8 |
| `threatjapan_uracto` | 74 | 114 | 72 | 112 | 112 |
| `vibleaker_android_samp` | 1084 | 1795 | 1089 | 1812 | 1806 |
| `xbot_android_samp` | 2789 | 6657 | 3133 | 6255 | 4685 |
| **all 38** | **3373** | **6657** | **3433** | **6255** | **4685** |

### Index time per app, seconds

| app | none | #127 | #128 | both | both+fixes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `backflash` | 0.27 | 0.44 | 0.27 | 0.45 | 0.45 |
| `beita_com_beita_contact` | 0.50 | 0.81 | 0.52 | 0.84 | 0.82 |
| `cajino_baidu` | 1.89 | 3.08 | 1.88 | 3.07 | 3.09 |
| `chulia` | 0.04 | 0.06 | 0.04 | 0.06 | 0.06 |
| `death_ring_materialflow` | 0.12 | 0.19 | 0.12 | 0.19 | 0.19 |
| `dsencrypt_samp` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `exprespam` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `fakeappstore` | 0.30 | 0.50 | 0.29 | 0.51 | 0.50 |
| `fakebank_android_samp` | 0.24 | 0.42 | 0.25 | 0.42 | 0.42 |
| `fakedaum` | 0.17 | 0.29 | 0.17 | 0.28 | 0.28 |
| `fakemart` | 0.28 | 0.43 | 0.28 | 0.43 | 0.43 |
| `fakeplay` | 0.67 | 1.08 | 0.66 | 1.08 | 1.09 |
| `faketaobao` | 0.22 | 0.39 | 0.23 | 0.38 | 0.38 |
| `godwon_samp` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `hummingbad_android_samp` | 3.96 | 5.93 | 3.96 | 5.92 | 5.95 |
| `jollyserv` | 0.46 | 0.79 | 0.46 | 0.79 | 0.78 |
| `overlay_android_samp` | 0.12 | 0.19 | 0.12 | 0.18 | 0.19 |
| `overlaylocker2_android_samp` | 0.58 | 1.00 | 0.59 | 1.00 | 1.01 |
| `phospy` | 0.17 | 0.27 | 0.17 | 0.27 | 0.27 |
| `proxy_samp` | 0.03 | 0.04 | 0.03 | 0.04 | 0.04 |
| `remote_control_smack` | 30.42 | 30.40 | 30.29 | 30.85 | 31.52 |
| `repane` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `roidsec` | 0.03 | 0.04 | 0.03 | 0.04 | 0.04 |
| `samsapo` | 0.42 | 0.70 | 0.42 | 0.70 | 0.70 |
| `save_me` | 1.54 | 2.46 | 1.54 | 2.46 | 2.50 |
| `scipiex` | 0.45 | 0.72 | 0.45 | 0.72 | 0.73 |
| `slocker_android_samp` | 0.31 | 0.53 | 0.31 | 0.53 | 0.53 |
| `sms_google` | 0.04 | 0.07 | 0.04 | 0.06 | 0.06 |
| `sms_send_locker_qqmagic` | 0.03 | 0.03 | 0.02 | 0.03 | 0.03 |
| `smssend_packageInstaller` | 0.26 | 0.44 | 0.25 | 0.43 | 0.42 |
| `smssilience_fake_vertu` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `smsstealer_kysn_assassincreed_android_samp` | 0.42 | 0.72 | 0.41 | 0.72 | 0.72 |
| `stels_flashplayer_android_update` | 0.22 | 0.38 | 0.23 | 0.38 | 0.38 |
| `tetus` | 0.05 | 0.08 | 0.05 | 0.08 | 0.08 |
| `the_interview_movieshow` | 0.01 | 0.02 | 0.01 | 0.02 | 0.02 |
| `threatjapan_uracto` | 0.20 | 0.34 | 0.20 | 0.34 | 0.34 |
| `vibleaker_android_samp` | 23.26 | 8.13 | 23.22 | 8.12 | 8.16 |
| `xbot_android_samp` | 66.55 | 197.86 | 66.44 | 202.29 | 84.41 |
| **all 38** | **134.28** | **258.93** | **133.99** | **263.78** | **146.69** |


## How this was measured

Five binaries, each built with `nix build .#packages.aarch64-darwin.default`
from the same branch code, differing only in what they sit on top of:

| point | base | branch content |
| --- | --- | --- |
| neither | `2ba7e014` | this branch's suite and query-memory work |
| `#127` only | `a5f4f557` | same |
| `#128` only | `2ba7e014` + `0183c42c` cherry-picked (applies clean) | same |
| both | `0183c42c` | same (this is the rebased branch) |
| both + fixes | `28f949ea` (`origin/blowup2`) | same |

The branch's temporary index counters (`facts::counters`) are left out of all
five. `origin/main` deletes the call-string lattice they count, so the rebase
has to drop them anyway; dropping them everywhere keeps the four points
differing only by the commits under test.

Each run is the ordinary suite - `xtask taintbench` over all 38 apps, APKs from
the Nix store - with `ctadl` on `PATH` replaced by a shim that runs the real
binary under `/usr/bin/time -l` and records wall time and **peak memory
footprint** per phase. Footprint rather than RSS: macOS compresses cold pages,
so RSS understates a big run badly.

Runs were sequential on an idle M1 Ultra (20 cores, 128 GB): three repetitions
of the neither, both, and both-plus-fixes points, two of each single-commit
point, alternating the order so machine drift could not favour one side. Repeats
agree closely - suite wall-clock came in at 493/466/476 s for neither,
395/389/394 s for both, 278/277/277 s with the fixes, and 264/263 s for `#128`
alone - and every repetition of a configuration produced identical per-finding
verdicts.

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
  sweep/<cfg>/<app>/index.log RUST_LOG=debug index, for the relation counters
  prof/<cfg>/<app>/sample.txt `sample` profile of one index run
  report.py, report5.py       the scripts that build the tables above
  counter-sweep.sh, profile.sh how the last two were produced
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

`#127` is a separate question, and it looks different with `origin/blowup2`
measured. The headline cost - suite index time doubling - is mostly gone: the
fixes bring it back to 9% over the pre-`#127` baseline, and `xbot` from 3x to
1.27x. What remains is a 1.65x median index slowdown on ordinary apps and a
1.24x median memory increase, which the fixes leave untouched, in exchange for
one extra flow on this suite.

Whether that trade is worth it is not a question TaintBench can answer. These
are 27 KB to 5.4 MB APKs and the suite's whole index is 147 s; a call graph
built for scale should be judged on inputs where the old one hurts. Two
measurements would say more than anything here:

- **`remote_control_smack` with the unwinder model off.** It is the one app in
  the suite that ever blew up - hours, until `native-index.jsonl` marked the ARM
  unwinder `skip-analysis` (see `hybrid-inlining-plateau.md`). Its index time is
  flat across every point measured here *because* that model is on. If `#127`
  makes the model unnecessary, that is the scalability win, and nothing in this
  document can see it.
- **`xtask report-eval --apks <dir>` over a real corpus.** That is what the
  subcommand is for, and it is where an app large enough to separate a 1.65x
  constant from a better growth curve would come from.
