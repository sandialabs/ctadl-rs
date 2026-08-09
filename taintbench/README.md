# TaintBench regression suite - DO-NOT-MERGE

[TaintBench](https://github.com/TaintBench) is an open benchmark of real Android
malware apps, each shipped with a hand-curated set of ground-truth taint flows.
This suite runs `ctadl` on an app's APK and checks how many of those flows we
reproduce. It is **separate from the nightly `regression` suite** and much
lighter: ctadl imports the prebuilt APK directly, so no javac/dx/Ghidra/Android
SDK is needed — only `ctadl`.

## Running

```sh
nix build .#checks.aarch64-darwin.taintbench    # macOS
nix build .#checks.x86_64-linux.taintbench       # Linux / CI
```

The check fetches each app's APK (a fixed-output derivation; the APK is never
committed), analyzes it, and prints a per-app, per-finding report. To iterate
locally without Nix, put `ctadl` on `PATH` and run the task directly:

```sh
cargo xtask taintbench --apk beita_com_beita_contact=/path/to/app.apk
cargo xtask taintbench --filter beita        # only matching apps
```

## The apps

Thirty-eight TaintBench apps run today — every app in the benchmark except
`chat_hook`:

- `backflash`
- `beita_com_beita_contact`
- `cajino_baidu`
- `chulia`
- `death_ring_materialflow`
- `dsencrypt_samp`
- `exprespam`
- `fakeappstore`
- `fakebank_android_samp`
- `fakedaum`
- `fakemart`
- `fakeplay`
- `faketaobao`
- `godwon_samp`
- `hummingbad_android_samp`
- `jollyserv`
- `overlay_android_samp`
- `overlaylocker2_android_samp`
- `phospy`
- `proxy_samp`
- `remote_control_smack`
- `repane`
- `roidsec`
- `samsapo`
- `save_me`
- `scipiex`
- `slocker_android_samp`
- `sms_google`
- `sms_send_locker_qqmagic`
- `smssend_packageInstaller`
- `smssilience_fake_vertu`
- `smsstealer_kysn_assassincreed_android_samp`
- `stels_flashplayer_android_update`
- `tetus`
- `the_interview_movieshow`
- `threatjapan_uracto`
- `vibleaker_android_samp`
- `xbot_android_samp`

All thirty-eight run by default. `remote_control_smack` used to be **excluded**,
because `ctadl index` on it ran for hours where every other app finishes in
minutes. The cause was the ARM C++ exception unwinder in its native libraries:
the three `__aeabi_unwind_cpp_pr*` personality routines each summarized to about
50,000 rows against a median of 1 row per function, and they are legitimate
indirect-call targets, so hybrid inlining instantiated those summaries and the
result multiplied. `native-index.jsonl` now models the unwinder with
`modes: ["skip-analysis"]` and the app reaches a fixpoint in about 30 s. See
`../hybrid-inlining-plateau.md`.

An app is excluded by an `excluded` key in its `app.json` whose value says why:

```json
{ "excluded": "ctadl index runs for hours on this app ..." }
```

`nix/taintbench.nix` then never fetches its APK, and the report shows
`SKIP <name> (excluded: <reason>)` rather than a bare missing-APK skip. Naming
its APK explicitly still runs it, which is how a baseline for an excluded app
gets established. No app carries the key today:

```sh
cargo xtask taintbench --apk remote_control_smack=/path/to/app.apk
```

Ten of them — `backflash`, `beita_com_beita_contact`, `cajino_baidu`, `chulia`,
`death_ring_materialflow`, `dsencrypt_samp`, `exprespam`, `fakeappstore`,
`fakebank_android_samp`, and `hummingbad_android_samp` — carry hand-written
models. The other twenty-eight carry models derived mechanically from their own
`findings.json`: every framework method named in a finding's source IR is marked
a source (on its `Return`, or on `Argument(0)` for a constructor, whose result is
the object being built), every method named in a sink IR is marked a sink on
`Argument(*)`, and a shared block of framework propagations — the apache-http
request plumbing above all — is appended so the taint can reach the exfiltration
call. Sharpening one of these by hand is welcome; re-run the suite and commit the
new baseline.

## Layout

Each app is a directory under `apps/<name>/` holding four files:

| file             | role                                                              |
| ---------------- | ---------------------------------------------------------------- |
| `findings.json`  | TaintBench ground truth, copied verbatim from the app's upstream repo. |
| `model.json`     | ctadl model (`model_generators`) — the sources/sinks to mark, plus any propagations the app's flows need. |
| `expected.json`  | Baseline: the finding IDs ctadl currently detects, and the negatives it currently reports anyway (see below). |
| `app.json`       | APK coordinates (`url` + SRI `sha256`) and provenance, read by `nix/taintbench.nix`; an optional `excluded` reason keeps the app out of the default run. |

`model.json` goes to **both** `ctadl index` and `ctadl query`: index consumes its
`propagation` models (they become function summaries) and query its sources and
sinks. Each phase warns about the part it ignores. Several apps exfiltrate
through framework classes that have no body in the APK — the apache-http
request plumbing, say — and need a summary for the taint to reach the sink.

Adding an app is data-only: drop in these four files and `nix/taintbench.nix`
picks it up (`builtins.readDir`). No code or list to edit — only the app list
above, which is written by hand. `app.json`'s `sha256` is an SRI hash; get it
with `nix store prefetch-file --json <url> | jq -r .hash`. Remember to `git add`
the new directory: the flake reads its source from git, so an untracked app is
silently skipped by `nix build` while `cargo xtask taintbench` still runs it.

## How a finding is matched

A TaintBench finding is a labelled `source` and `sink`, each given as
`(className, methodName, lineNo, targetName)` plus the called framework method's
IR signature. **ctadl must find a connected source→sink path, but we do not
check that its intermediate steps mirror TaintBench's** (matching whole paths is
brittle and was explicitly out of scope). We match a path on the *callee method*
of its two endpoints:

1. `model.json` marks the framework methods named in the findings' source/sink
   IR signatures as ctadl sources/sinks (e.g. `ContentResolver.query`'s return
   is a source; `Transport.sendMessage`'s arguments are a sink).
2. `ctadl query --sarif-profile agent` emits a `tainted-path` result for each
   connected flow, carrying the source and sink callee (`sourceCallee` /
   `sinkCallee`, e.g. `Ljava/io/DataOutputStream;->write([BII)V`).
3. xtask parses the callee `(class, method)` from each finding's IR
   (`<Class: ret method(args)>`) and from the SARIF callees, and a finding is
   **detected** when ctadl reports a connected path whose source callee and sink
   callee match the finding's. Type signatures are ignored, so e.g.
   `FileInputStream.<init>(String)` and `(File)` both match.

The per-endpoint `taint-source` / `taint-sink` results are also parsed, but only
to populate the diagnostic `source:`/`sink:` HIT columns in the report — the
match itself requires a *connected* path. (A finding can have its source and
sink both recognized as endpoints yet not match, when ctadl finds no path
between them.)

## Pass criterion: baseline snapshot

`expected.json` lists the finding IDs ctadl detects today, and the `isNegative`
findings it reports anyway:

```json
{ "matched_finding_ids": [1], "false_positive_finding_ids": [7] }
```

The check **fails on a regression** — a baseline finding that stops being
detected — and on a **new false positive** — a finding flagged `isNegative` in
the ground truth that gets reported and is not already listed in
`false_positive_finding_ids`. Newly detected findings and false positives that
go away do **not** fail the check; they are reported as improvements with the
suggested new baseline, which you then commit to `expected.json`. To
(re)establish a baseline, run the suite and read the IDs off the report.

Listing a false positive is not forgiving it. It is imprecision we have measured
and written down, so the report shows the count and any *new* one still fails.
Omit the key entirely when an app has none.

### Shadowed negatives

A TaintBench `isNegative` finding is a *specific call site* that looks like a
flow but isn't. Some apps (e.g. `cajino_baidu`) carry a negative whose source
and sink call the **same framework methods** as a genuine positive finding,
differing only by line. Because we match on the callee *method* (DEX SARIF
carries no reliable line/byteOffset to tell two call sites apart), such a
negative is indistinguishable from the positive: ctadl reporting that
source→sink pair means it found the positive, not the negative. We therefore
**do not** count a negative as a false positive when its callee pair is also a
positive finding's pair — the report marks it `MATCH(shadowed-by-positive)`. The
false-positive check only has teeth for negatives that are callee-distinguishable
from every positive.

A related case survives the shadowing rule and shows up as a real false
positive: an app whose positives *cross*, so that no single positive has the
negative's pair but each of its endpoints belongs to some positive. `phospy` is
the clearest one — its device ID goes to `writeUTF` and its file contents to
`write`, and TaintBench's negatives are the two crossed combinations. ctadl
reports both, because the same output stream carries both values. Those go in
`false_positive_finding_ids`.
