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
| false positives                          |  8         |   1       |

Ascent finds 43 more of TaintBench's 191 flows — better on 21 apps, worse on 2 —
and pays for it with 7 more false positives.

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

`pairs` is distinct positive callee pairs detected, as described above.

| app | ascent | Souffle | Δ | pairs (a) | pairs (s) | FP (a) | FP (s) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `backflash` | 6/13 | 6/13 | +0 | 2/5 | 2/5 | | |
| `beita_com_beita_contact` | 1/3 | 1/3 | +0 | 1/3 | 1/3 | | |
| `cajino_baidu` | 11/12 | 9/12 | +2 | 7/8 | 5/8 | | |
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
| `proxy_samp` | 9/17 | 2/17 | +7 | 7/12 | 2/12 | 2 | |
| `remote_control_smack` | 6/17 | 11/17 | −5 | 3/4 | 1/4 | | |
| `repane` | 1/1 | 1/1 | +0 | 1/1 | 1/1 | | |
| `roidsec` | 6/6 | 0/6 | +6 | 2/2 | 0/2 | | |
| `samsapo` | 2/4 | 1/4 | +1 | 2/4 | 1/4 | | |
| `save_me` | 10/25 | 0/25 | +10 | 3/7 | 0/7 | 3 | |
| `scipiex` | 0/3 | 0/3 | +0 | 0/3 | 0/3 | | |
| `slocker_android_samp` | 0/5 | 0/5 | +0 | 0/2 | 0/2 | | |
| `sms_google` | 2/4 | 0/4 | +2 | 1/2 | 0/2 | | |
| `sms_send_locker_qqmagic` | 5/6 | 6/6 | −1 | 2/3 | 3/3 | | |
| `smssend_packageInstaller` | 3/5 | 2/5 | +1 | 2/3 | 1/3 | | |
| `smssilience_fake_vertu` | 2/2 | 0/2 | +2 | 2/2 | 0/2 | | |
| `smsstealer_kysn_assassincreed_android_samp` | 0/5 | 0/5 | +0 | 0/4 | 0/4 | | |
| `stels_flashplayer_android_update` | 2/3 | 0/3 | +2 | 2/3 | 0/3 | | |
| `tetus` | 2/2 | 2/2 | +0 | 1/1 | 1/1 | | |
| `the_interview_movieshow` | 0/1 | 0/1 | +0 | 0/1 | 0/1 | | |
| `threatjapan_uracto` | 1/2 | 0/2 | +1 | 1/2 | 0/2 | | |
| `vibleaker_android_samp` | 3/4 | 2/4 | +1 | 2/3 | 1/3 | | |
| `xbot_android_samp` | 0/3 | 0/3 | +0 | 0/3 | 0/3 | | |
| **total** | **92/191** | **49/191** | **+43** | **59/114** | **24/114** | **8** | **1** |

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

## The false positives

Ascent reports 8 callee-distinguishable negatives against Souffle's 1. All eight
are recorded in the apps' `expected.json`, so the check passes; they are
measured imprecision, not forgiven imprecision.

| app | finding | pair |
| --- | --- | --- |
| `phospy` | #3 | `TelephonyManager.getDeviceId → DataOutputStream.write` |
| `phospy` | #4, #5 | `FileInputStream.<init> → DataOutputStream.writeUTF` |
| `proxy_samp` | #16 | `File.<init> → BufferedWriter.write` |
| `proxy_samp` | #20 | `File.<init> → Log.i` |
| `save_me` | #26, #27, #28 | `SQLiteDatabase.query → ContentValues.put` |

See [`taintbench-false-positives.md`](taintbench-false-positives.md) for the
cause of each.

Net: 43 more true flows for 7 more false ones.

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

For one app, with `ctadl` on `PATH`:

```sh
cargo xtask taintbench --apk phospy=/path/to/phospy.apk --filter phospy
```
