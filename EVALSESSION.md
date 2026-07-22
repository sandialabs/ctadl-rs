# Eval session — argv/offset cmdi regression fixed via saturating sources - DO-NOT-MERGE

**Date:** 2026-07-21 · **Build under test:** `cta@593ce9a` (branch `firmware-eval-3`)
**Corpus:** 15 Operation Mango cmdi test binaries · **Ground truth:** 24 Mango known bugs (`mango@local`)

## Task

Rerun the 15 Operation Mango binaries on the new ctadl build, adjust
`firmware-eval/models/cmdi-firmware.json5` for failures (using new matching
features from `docs/model-generators.md`), and specifically use the **saturating
sources** feature for the argv source — to recover the argv/offset-taint
regression documented in `firmware-eval/README.md` / `firmware-eval/repro/`.

## Headline result

| Build | TP | FN | extra | recall | note |
|---|---|---|---|---|---|
| `beb327a` | 1 | 23 | 9 | 4.2% | regressed (argv taint broken) |
| `b06b137` | 20 | 4 | 12 | 83.3% | old last-good baseline |
| **`593ce9a`** | **21** | **3** | **4** | **87.5%** | this session — beats baseline on recall *and* precision |

Scored with `bench.py score --addr-tolerance 0`. 15/15 binaries run OK.

## Setup / environment findings

- The shipped `target/release/ctadl` (Jul 20) **predated** the saturating feature
  (`a836dbe`, "Add saturating sources (#70)", Jul 21) — confirmed via `strings`
  and git history. **Rebuilt** `cargo build --release --bin ctadl` at HEAD.
- Could **not** regenerate Mango ground truth: no `podman`/`docker` on the host,
  and the local `firmware-eval/mango-env/.venv` mango is ABI-broken (angr/cffi
  `IRType *` cffi error). **Not needed** — the 24-bug ground truth was intact in
  `firmware-eval/run/results.db` (`mango@local`, plus historical ctadl runs).
- Verified trust in the harness/GT by **reproducing both documented baselines
  exactly** before changing anything: `b06b137` → 20/4/12, `beb327a` → 1/23/9.

## The fix (three parts)

Root cause of the residual gap after saturating alone: argv taint rides at the
**pointer** level; flows that pass *through a string builder* or hit a
**changed SARIF format** were still lost/mismeasured. Diagnosed with
`--sarif-profile debug` (C0002 meet-points), `objdump`, and raw codeFlow dumps.

1. **Model — `saturating: true` on the argv sources**
   (`cmdi-firmware.json5`, the `main` source generator). `argv[i]` is
   `*(argv+8i)`, a sibling offset of the modeled `.deref` path; saturation taints
   the whole subtree so `argv[i]` reconnects to the source. Recovers the *direct*
   `system(argv[1])` cases (`nested`/`simple`/`after_values`). Unit-checked on
   `nested` (0 → 2 `argv_input` C0001 paths) and `repro/argv_offset_taint`.

2. **Model — base-level string-builder propagation**
   (`cmdi-firmware.json5`, `sprintf`/`strcpy`/`memcpy` families). Added
   `{ input: "Argument(n)", output: "Argument(0).deref" }` alongside the existing
   `.deref → .deref` edges. Saturating taint at the pointer level is invisible to
   a `.deref`-only propagation input, so argv-*through*-a-builder cases (`heap`,
   `wrapper`, `sprintf_resolved_and_unresolved`, the argv leg of `multi_input`)
   and the `off_shoot` `read → alter_command → system` flow died at the summary.
   The base-level edge lets the pointer-level taint cross into the destination
   string. Mirrors how Mango models these builders.

3. **Harness — SARIF call-site extraction** (`normalize_ctadl._callsite_addrs`).
   *Necessary for correct scoring* — without it recall read a false 54.2% even
   though detection was correct. Two new-frontend behaviors broke the old
   extractor:
   - Step messages are now prefixed/annotated (`sink call-arg(197, 0).deref in
     system`), so `startswith("call-arg")` matched nothing → fell back to a wrong
     address. Fixed by matching the `call-arg(` **substring**.
   - The frontend emits a degenerate **PLT-thunk twin** per flow whose terminal
     step re-anchors at the callee's `@plt` entry (e.g. heap `system` @0x1225 real
     + twin @0x10a4). "Last call-arg step" grabbed the thunk → spurious
     low-address duplicate. Fixed by preferring the real `call … in <sink>`
     forwarding-call site, which collapses each twin onto its real TP. This alone
     dropped `extra` 20 → 4 with **zero** TP loss (distinct call sites in
     `after_values` 3×system+3×execve and `simple` preserved, matching GT exactly).

## Remaining FN / extra (not model-fixable)

- **`nvram`** (2 FN) — unlinked `ET_REL` object; Ghidra frontend doesn't apply
  relocations → 0 sinks bind. Pre-documented; irrelevant to real firmware.
- **`layered`** (1 FN) — second `system` call site missed (engine gap).
  Pre-documented.
- **4 extra** — extra real NETWORK/FILE source-classes into genuine multi-channel
  sinks in `early_resolve`/`multi_input` that Mango's GT lists once. `cta_advantage`
  / GT granularity, **not** false positives.

## Files changed (not committed)

- `firmware-eval/models/cmdi-firmware.json5` — saturating argv sources +
  base-level string-builder propagation (+47/-… lines, commented).
- `firmware-eval/harness/normalize_ctadl.py` — `_callsite_addrs`: substring match
  + PLT-thunk-twin de-duplication.
- `firmware-eval/README.md` — regression section flipped to **RESOLVED** with the
  three-part fix and new corpus status; `off_shoot` moved out of the FN table;
  extras note updated; old diagnosis preserved under `<details>`.
- `firmware-eval/repro/README.md` — RESOLVED banner.

Nothing committed. The `cta@593ce9a` run is persisted in
`firmware-eval/run/results.db`.

## Reproduce

```sh
# rebuild (needs the saturating feature, a836dbe)
cargo build --release --bin ctadl

# run + score the corpus
cd firmware-eval/run
python3 ../harness/bench.py run   --db results.db --manifest worklist.json --force
python3 ../harness/bench.py score --db results.db --version cta@593ce9a --tool cta \
    --addr-tolerance 0 --show 40
# -> 21 TP / 3 FN / 4 extra (recall 87.5%)

# single-binary regression check
ctadl go -n nested -l pcode --models ../models/cmdi-firmware.json5 \
    /Users/dbueno/proj/operation-mango-public/package/tests/binaries/nested/program
jq '[.runs[0].results[]|select(.ruleId=="C0001.tainted-path")
     |select(.properties.taintLabels|index("argv_input"))]|length' results.sarif   # -> 2
```
