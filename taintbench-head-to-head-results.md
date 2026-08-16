# TaintBench head to head: ascent CTADL vs Souffle CTADL

This compares the TaintBench numbers for the Rust/ascent CTADL in this repo
against the baseline the Souffle CTADL (version 0.14.1) recorded in
`../ctadl-souffle-taintbench/taintbench-summary.md`.

Run on 2026-08-14 at commit `8baae049`, via the pinned Nix check:

```sh
nix build .#checks.aarch64-darwin.taintbench -L
```

The check compiles ctadl from the tree, fetches all 38 APKs by SRI hash, and
runs the suite in the sandbox. Result: **38 passed, 0 skipped, 0 failed.** Every
committed `expected.json` was reproduced exactly, so the numbers below are the
repository's current baseline, not a revision of it.

## Headline

|                                          |  ascent    | Souffle   |
| ---------------------------------------- | ---------- | --------- |
| ground-truth flows detected              | **92/191** |  49/191   |
| distinct source→sink callee pairs        | **59/114** |  24/114   |
| negatives reported (TaintBench's rule)   |  20/45     |  10/45    |
| — of them callee-distinguishable         |  8         |   1       |
| distinct negative pairs reported         |  11/18     |   5/18    |
| precision over labelled flows            |  82.1%     |  83.1%    |
| precision over distinct pairs            |  **84.3%** |  82.8%    |

Ascent finds 43 more of TaintBench's 191 flows — better on 21 apps, worse on 2 —
and reports 10 more of its 45 confirmed non-flows. The two engines' false
positive *rates* are the same to within a point: ascent's extra false positives
are what its extra recall costs, not a worse precision profile.

An earlier version of this table read "false positives: 8 vs 1", which is the
suite's count after the shadowed-negative rule (below) forgives negatives that
are indistinguishable from a detected positive. TaintBench itself makes no such
allowance: a finding flagged `isNegative` is a confirmed non-flow, and reporting
it is a false positive. Counted that way it is 20 vs 10 — a much smaller gap than
8 vs 1 suggests, because 12 of ascent's 20 and 9 of Souffle's 10 are the forgiven
kind.

## Why two recall numbers

TaintBench counts a finding per *call site*. Several apps repeat one flow shape
across many lines: `remote_control_smack` has eleven findings that are all
`Cursor.getString → FileWriter.append` at eleven different lines, and the suite
matches on the callee method pair, so an engine either gets all eleven or none.
The finding count therefore weights some behaviours eleven times and others
once.

The second row collapses each app's positives to their distinct
(source callee, sink callee) pairs — 114 across the suite — and asks how many of
those an engine reaches at least once. It is the same data, weighted by
behaviour rather than by call site. Ascent leads on both, and by a wider margin
on the second (59 vs 24).

## Per app

`pairs` is distinct positive callee pairs detected, as described above. `FP` is
negatives reported under TaintBench's own rule — every `isNegative` finding whose
callee pair the engine reports — with the callee-*indistinguishable* subset in
parentheses. The suite's own count is the number outside parentheses minus the
one inside.

| app | ascent | Souffle | Δ | pairs (a) | pairs (s) | FP (a) | FP (s) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `backflash` | 6/13 | 6/13 | +0 | 2/5 | 2/5 | 3 (3) | 3 (3) |
| `beita_com_beita_contact` | 1/3 | 1/3 | +0 | 1/3 | 1/3 | | |
| `cajino_baidu` | 11/12 | 9/12 | +2 | 7/8 | 5/8 | 3 (3) | 3 (3) |
| `chulia` | 0/4 | 0/4 | +0 | 0/2 | 0/2 | | |
| `death_ring_materialflow` | 1/1 | 1/1 | +0 | 1/1 | 1/1 | | |
| `dsencrypt_samp` | 1/1 | 0/1 | +1 | 1/1 | 0/1 | | |
| `exprespam` | 2/2 | 0/2 | +2 | 2/2 | 0/2 | | |
| `fakeappstore` | 1/3 | 0/3 | +1 | 1/3 | 0/3 | | |
| `fakebank_android_samp` | 3/5 | 1/5 | +2 | 3/5 | 1/5 | | |
| `fakedaum` | 1/2 | 0/2 | +1 | 1/2 | 0/2 | | |
| `fakemart` | 0/2 | 0/2 | +0 | 0/1 | 0/1 | | |
| `fakeplay` | 0/2 | 0/2 | +0 | 0/2 | 0/2 | | |
| `faketaobao` | 0/4 | 0/4 | +0 | 0/1 | 0/1 | | |
| `godwon_samp` | 0/6 | 0/6 | +0 | 0/3 | 0/3 | | |
| `hummingbad_android_samp` | 2/2 | 0/2 | +2 | 2/2 | 0/2 | | |
| `jollyserv` | 0/1 | 0/1 | +0 | 0/1 | 0/1 | | |
| `overlay_android_samp` | 3/4 | 0/4 | +3 | 2/3 | 0/3 | | |
| `overlaylocker2_android_samp` | 4/7 | 3/7 | +1 | 3/5 | 2/5 | | |
| `phospy` | 2/2 | 1/2 | +1 | 2/2 | 1/2 | 3 | 1 |
| `proxy_samp` | 9/17 | 2/17 | +7 | 7/12 | 2/12 | 3 (1) |  |
| `remote_control_smack` | 6/17 | 11/17 | −5 | 3/4 | 1/4 | | |
| `repane` | 1/1 | 1/1 | +0 | 1/1 | 1/1 | | |
| `roidsec` | 6/6 | 0/6 | +6 | 2/2 | 0/2 | | |
| `samsapo` | 2/4 | 1/4 | +1 | 2/4 | 1/4 | 1 (1) | 1 (1) |
| `save_me` | 10/25 | 0/25 | +10 | 3/7 | 0/7 | 3 |  |
| `scipiex` | 0/3 | 0/3 | +0 | 0/3 | 0/3 | | |
| `slocker_android_samp` | 0/5 | 0/5 | +0 | 0/2 | 0/2 | | |
| `sms_google` | 2/4 | 0/4 | +2 | 1/2 | 0/2 | | |
| `sms_send_locker_qqmagic` | 5/6 | 6/6 | −1 | 2/3 | 3/3 | 2 (2) | 2 (2) |
| `smssend_packageInstaller` | 3/5 | 2/5 | +1 | 2/3 | 1/3 | | |
| `smssilience_fake_vertu` | 2/2 | 0/2 | +2 | 2/2 | 0/2 | 2 (2) |  |
| `smsstealer_kysn_assassincreed_android_samp` | 0/5 | 0/5 | +0 | 0/4 | 0/4 | | |
| `stels_flashplayer_android_update` | 2/3 | 0/3 | +2 | 2/3 | 0/3 | | |
| `tetus` | 2/2 | 2/2 | +0 | 1/1 | 1/1 | | |
| `the_interview_movieshow` | 0/1 | 0/1 | +0 | 0/1 | 0/1 | | |
| `threatjapan_uracto` | 1/2 | 0/2 | +1 | 1/2 | 0/2 | | |
| `vibleaker_android_samp` | 3/4 | 2/4 | +1 | 2/3 | 1/3 | | |
| `xbot_android_samp` | 0/3 | 0/3 | +0 | 0/3 | 0/3 | | |
| **total** | **92/191** | **49/191** | **+43** | **59/114** | **24/114** | **20 (12)** | **10 (9)** |

Eleven apps score zero on ascent: `chulia`, `fakemart`, `fakeplay`,
`faketaobao`, `godwon_samp`, `jollyserv`, `scipiex`, `slocker_android_samp`,
`smsstealer_kysn_assassincreed_android_samp`, `the_interview_movieshow`, and
`xbot_android_samp`. All eleven also score zero on Souffle, so nothing about the
comparison turns on them. Souffle scores zero on twelve further apps that ascent
does not.

## Where ascent gains

Six apps go from nothing to every flow: `exprespam`, `hummingbad_android_samp`,
`roidsec`, `smssilience_fake_vertu`, `dsencrypt_samp`, and (with the caveat
below) `phospy`. The largest absolute gains are `save_me` 0→10, `proxy_samp`
2→9, `roidsec` 0→6, and `overlay_android_samp` 0→3.

## Where ascent loses

Two apps, and only one of them is a real loss.

**`remote_control_smack` 11→6 is a counting artifact.** The two engines' matched
sets are completely disjoint. Souffle credits eleven findings, all of them the
single pair `Cursor.getString → FileWriter.append` repeated across eleven lines.
Ascent misses that one pair but detects three others Souffle misses:
`Cursor.getInt`, `Cursor.getLong`, and `LocationManager.getLastKnownLocation`,
each into `FileWriter.append`. By distinct pair, ascent is 3/4 and Souffle is
1/4. The eleven-fold repetition is what turns a one-pattern miss into a −5.

**`sms_send_locker_qqmagic` 6→5 is a genuine regression.** Finding #2,
`SmsMessage.getDisplayOriginatingAddress → SmsManager.sendTextMessage`, is not
found. Both endpoints are recognized as taint source and taint sink; no path
connects them.

Both misses have a `String`-returning source whose value never reaches the sink,
and both sit alongside sibling flows in the same method that *are* found. That
they share a shape is worth one look, since a single cause would recover twelve
findings across the two apps.

## The false positives, by TaintBench's definition

TaintBench ships 45 findings flagged `isNegative` alongside its 191 positives.
A negative is a hand-checked non-flow at a named call site: reporting it is a
false positive, full stop. The suite adds one allowance TaintBench does not — the
shadowed-negative rule, which drops a negative whose source and sink callees are
also some *positive's*, on the grounds that DEX SARIF carries no line
information to tell the two call sites apart. Counting both ways:

| | ascent | Souffle |
| --- | --- | --- |
| negatives whose pair the engine reports (TaintBench's rule) | 20/45 | 10/45 |
| — indistinguishable from a detected positive (suite forgives) | 12 | 9 |
| — callee-distinguishable (suite counts) | **8** | **1** |

The 20-vs-10 number is the honest headline for precision, and it is a different
story from 8-vs-1. Ascent reports twice as many negatives as Souffle while
detecting nearly twice as many positives: 82.1% precision over labelled flows
against Souffle's 83.1%, and 84.3% against 82.8% once each app's flows are
collapsed to distinct callee pairs — where ascent comes out *ahead*. The suite's
8-vs-1 exaggerates the gap because shadowing forgives 9 of Souffle's 10 and only
12 of ascent's 20: an engine that detects fewer positives has fewer positives to
hide its negatives behind, so the rule's benefit is not distributed evenly.

Both precision figures are over TaintBench's *labelled* set only — 191 positives
and 45 negatives. A reported path whose callee pair matches no finding at all is
neither credited nor charged, because TaintBench says nothing about it, and each
engine reports many such paths. So these are not precision over everything the
engines print; they are the precision TaintBench's labels can measure, which is
the most either engine can be held to here.

Per app, with the shadowed subset in parentheses:

| app | negatives | ascent | Souffle |
| --- | --- | --- | --- |
| `backflash` | 11 | 3 (3) | 3 (3) |
| `cajino_baidu` | 3 | 3 (3) | 3 (3) |
| `overlay_android_samp` | 2 | 0 | 0 |
| `overlaylocker2_android_samp` | 12 | 0 | 0 |
| `phospy` | 3 | 3 | 1 |
| `proxy_samp` | 3 | 3 (1) | 0 |
| `samsapo` | 1 | 1 (1) | 1 (1) |
| `save_me` | 6 | 3 | 0 |
| `sms_send_locker_qqmagic` | 2 | 2 (2) | 2 (2) |
| `smssilience_fake_vertu` | 2 | 2 (2) | 0 |
| **total** | **45** | **20 (12)** | **10 (9)** |

The negatives are unevenly distributed: 23 of the 45 sit in `backflash` and
`overlaylocker2_android_samp`, and both engines are clean on all 12 of
`overlaylocker2`'s and on 8 of `backflash`'s 11. Twenty-eight apps carry no
negative at all, so their columns say nothing about precision either way.

The eight callee-distinguishable ones are all recorded in the apps'
`expected.json`, so the check passes; they are measured imprecision, not
forgiven imprecision.

| app | finding | pair |
| --- | --- | --- |
| `phospy` | #3 | `TelephonyManager.getDeviceId → DataOutputStream.write` |
| `phospy` | #4, #5 | `FileInputStream.<init> → DataOutputStream.writeUTF` |
| `proxy_samp` | #16 | `File.<init> → BufferedWriter.write` |
| `proxy_samp` | #20 | `File.<init> → Log.i` |
| `save_me` | #26, #27, #28 | `SQLiteDatabase.query → ContentValues.put` |

The twelve shadowed ones, which the suite does not count and TaintBench would:

| app | findings | pair | shadowing positives |
| --- | --- | --- | --- |
| `backflash` | #9, #10, #18 | `Intent.getStringExtra → OutputStreamWriter.write` | #5–8, #17 |
| `cajino_baidu` | #11, #13, #14 | `TelephonyManager.getDeviceId → BaiduBCS.putObject` | #7 |
| `proxy_samp` | #19 | `WifiManager.getConnectionInfo → Log.i` | #7 |
| `samsapo` | #5 | `Context.getSystemService → Method.invoke` | #3 |
| `sms_send_locker_qqmagic` | #4, #5 | `SmsMessage.getDisplayMessageBody → Context.startService` | #3, #6–8 |
| `smssilience_fake_vertu` | #3, #4 | `TelephonyManager.getLine1Number → PrintWriter.write` | #1 |

Every shadowing positive listed is one ascent detects — that is what makes the
pair reported in the first place. Souffle's 9 are the same rows minus
`proxy_samp` #19 and `smssilience_fake_vertu` #3–4, which it avoids only because
it misses their shadowing positives.

Whether ascent actually walks the negative's call site or only the positive's is
unknown for all twelve, and stays unknown until `C0001` carries call sites (see
the last section of
[`taintbench-false-positives.md`](taintbench-false-positives.md)). What is not
unknown: TaintBench's rule charges them, and so this document does.

See [`taintbench-false-positives.md`](taintbench-false-positives.md) for the
cause of each of the eight.

Net: 43 more true flows for 10 more reported negatives, at the same precision.

## Comparability

The comparison is close to engine-only.

- All 38 apps' `findings.json` are byte-identical between the two repos.
- Both harnesses credit a finding the same way: a connected source→sink path,
  matched on the two endpoints' callee methods, ignoring intermediate steps,
  with the same "shadowed negative" rule for negatives that share a positive's
  pair.
- 37 of 38 `model.json` files are byte-identical. `cajino_baidu` is the
  exception: this repo's model says `"saturating": true` where the Souffle port
  substituted `"all_fields": true`, because Souffle CTADL's model schema has no
  `saturating` key. The Souffle summary flags this as its one edited model and
  notes it still misses the two `File.listFiles` flows; ascent detects one more
  pair there (7/8 vs 5/8), so part of that app's +2 may be the model rather than
  the engine.

The remaining differences: Souffle CTADL is version 0.14.1 with precompiled
Souffle analyses, ascent is this tree at `8baae049`, and this repo additionally
models the ARM C++ unwinder as `skip-analysis` in `native-index.jsonl` so
`remote_control_smack` terminates (see `hybrid-inlining-plateau.md`).

## Reproducing

```sh
nix build .#checks.aarch64-darwin.taintbench -L   # full pinned run
nix build .#checks.x86_64-linux.taintbench -L     # CI
```

The negative recount above needs no run of its own. It is a function of the
committed `findings.json` and `expected.json`: a callee pair is reported iff some
finding carrying it is in `matched_finding_ids` (positive) or
`false_positive_finding_ids` (negative), and a negative is a false positive iff
its pair is reported. Recomputing `false_positive_finding_ids` from that
definition reproduces every committed list in both repos exactly, in all 38 apps,
which is what makes the reconstruction sound. The `findings.json` are
byte-identical between the two repos, so the same recount applies to Souffle's
baseline unchanged.

For one app, with `ctadl` on `PATH`:

```sh
cargo xtask taintbench --apk phospy=/path/to/phospy.apk --filter phospy
```
