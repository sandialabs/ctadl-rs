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

## Layout

Each app is a directory under `apps/<name>/` holding four files:

| file             | role                                                              |
| ---------------- | ---------------------------------------------------------------- |
| `findings.json`  | TaintBench ground truth, copied verbatim from the app's upstream repo. |
| `model.json`     | ctadl query model (`model_generators`) — the sources/sinks to mark. |
| `expected.json`  | Baseline: the finding IDs ctadl currently detects (see below).   |
| `app.json`       | APK coordinates (`url` + SRI `sha256`) and provenance, read by `nix/taintbench.nix`. |

Adding an app is data-only: drop in these four files and `nix/taintbench.nix`
picks it up (`builtins.readDir`). No code or list to edit. `app.json`'s `sha256` is an SRI
hash; get it with `nix store prefetch-file --json <url> | jq -r .hash`.

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

`expected.json` lists the finding IDs ctadl detects today:

```json
{ "matched_finding_ids": [1] }
```

The check **fails on a regression** — a baseline finding that stops being
detected — and on a **false positive** — any finding flagged `isNegative` in the
ground truth that gets reported. Newly detected findings do **not** fail the
check; they are reported as improvements with the suggested new baseline, which
you then commit to `expected.json`. To (re)establish a baseline, run the suite
and read the matched IDs off the report.

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
