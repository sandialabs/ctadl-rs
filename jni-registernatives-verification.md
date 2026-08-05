# Verifying `RegisterNatives` linking against the real APKs - DO-NOT-MERGE

What was run, what it measured, and what it found. The companion to
`jni-registernatives-plan.md` (the design) and
`jni-registernatives-implementation.md` (what was built).

Run on 2026-08-05, Ghidra 12.0.4, macOS/arm64, from commit `97571678` plus one
one-word log fix made during the run.

## The short version

Thirteen of the fourteen packages in `~/apps` were imported and indexed. **Every
entry, library and attribution count the plan predicted was reproduced exactly**,
including both TikTok builds. VLC is the one package that could not be measured,
for a disk reason unrelated to this change.

Nothing was found wrong with the feature. Two things were found worth recording:
branch veneers cost more `fnPtr` mappings than expected, and the plan's account of
how non-ELF files are handled is not quite how they are actually handled.

**The veneer finding has since been fixed**, and the three packages it touches
re-run. Messenger 563 goes from `54 linked (53 registered)` to
`97 linked (96 registered)`; the two controls do not move. See "Finding: branch
veneers" and "Re-run: following the veneer".

## Method

Each package was imported fresh — never `--skip-existing`, which would not write a
sidecar — and indexed twice: once normally, once with `--no-jni-registry`. The
second run is the A/B: it is what the `Java_`-symbol convention achieves alone, so
the difference is exactly what this change contributes.

Indexing was stopped once the JNI report printed. Resolution happens before the
flow-relation stage and nothing after it can change the answer. This was not
assumed — Facebook Lite was run both ways and produced byte-identical registry and
bridge lines (4 s versus 12 s). Facebook Lite and FX File Explorer also completed a
full index, so the whole pipeline is known to run to completion.

Two checks were added beyond what the plan asks for, because counts alone cannot
tell a correct mapping from a lucky one:

- **Mapping audit.** Every sidecar row was cross-checked against Ghidra's own
  `HFUNC_EP` entry-point facts, using the `image_base` each import records in its
  `import_config.json` rather than a value inferred from the data. A mapped row
  must land on a real entry point; a `function: null` row must not have had one
  available.
- **Raw-ELF checks.** The Thumb mask and the veneer finding were confirmed by
  decoding instructions out of the shipped `.so` files, not by reading counts.

## Results

`entries`/`attributed` count table entries. `linked`/`registered` count Java
methods. The two are different measurements and never match exactly — see
"Why `registered` is always below `attributed`".

| Package | entries | libs | attributed | unattr. | linked (registered) | A/B | predicted |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Facebook Lite 513 | 28 | 1 | 28 | 0 | **29 (28)** | 1 | 28 ✅ |
| FX File Explorer 9.1.0.8 | 0 | 0 | — | — | 10 (0) | 10 | no tables ✅ |
| Chrome 149 (armeabi-v7a) | 457 | 1 | 447 | 10 | 264 (253) | 11 | 447 ✅ |
| Telegram 12.7.3 (xapk) | 83 | 1 | 83 | 0 | 458 (79) | 379 | 83 ✅ |
| Telegram 12.8.3 (apk) | 83 | 1 | 83 | 0 | 460 (79) | 381 | 83 ✅ |
| Telegram 12.9.0 (xapk) | 83 | 1 | 83 | 0 | 460 (79) | 381 | 83 ✅ |
| TikTok Lite 44 | 579 | 16 | 567 | 12 | 644 (490) | 154 | 579/16/567 ✅ |
| Messenger 563 (arm64) | 118 | 7 | 117 | 1 | 54 (53) | 1 | 118/7/117 ✅ |
| Messenger 570 (armeabi-v7a) | 119 | 7 | 118 | 1 | 89 (81) | 8 | 119/118 ✅ |
| WhatsApp Msgr 2.26.20 | 19 | 1 | 19 | 0 | 9 (8) | 1 | — |
| WhatsApp Msgr 2.26.27 | 22 | 2 | 22 | 0 | 12 (11) | 1 | 22/2/22 ✅ |
| WhatsApp Business 2.26.21 | 19 | 1 | 19 | 0 | 20 (19) | 1 | 19/19 ✅ |
| TikTok 46.1.3 (xapk) | **1818** | **60** | **1768** | 50 | 8785 (1037) | 7748 | 1818/60/1768 ✅ |
| VLC 3.7.0 | — | — | — | — | — | — | not measurable here |

Totals across the thirteen: **3428 table entries in 99 libraries, 3354 attributed
(97.8%)**, and linked native methods rising from **9077 to 11 294** — 2217 methods
that had no implementation before, a 1.24× gain overall and far larger on the apps
that use `RegisterNatives` heavily (Messenger 563: 1 → 54; TikTok Lite: 154 → 644).

The table is the run as it stood, before the veneer finding below was acted on.
That fix moved exactly one row: Messenger 563 is now **97 (96)**, so its gain is
1 → 97. Facebook Lite and Messenger 570 were re-run and reproduced their rows
digit for digit.

The A/B baselines mostly match the plan's `Java_`-symbol counts to the digit —
Chrome 11, Telegram 381, Facebook Lite and all three WhatsApp builds 1 — but not
always: TikTok Lite gives 154 against a predicted 155, Messenger 570 gives 8
against 10. The two are not the same measurement. The plan counted `Java_` symbols
present in the ELF; the A/B counts Java methods that actually link, so an exported
symbol with no matching `native` declaration in the Dex appears in the first number
and not the second.

## What the run proves

### The `fnPtr` → function mapping is exact

```
rows=3428  mapped=2255  null=1173
mapping errors: badmap=0  missed=0  -> EXACT
```

Across every library with tables: no mapped row points anywhere but a real
function entry point, and no null row had an entry point that was overlooked.
Step 2's address arithmetic — `image_base + (ELF vaddr − first PT_LOAD p_vaddr)` —
is correct on ELF64 and ELF32, through both the relocation path and the in-place
path.

The audit was tightened when veneers were fixed: a mapped row must sit on an entry
point at `fn_addr`, or at `veneer_target` when it has one, and a null row must hold
no followable branch either. On the re-run it still reports `badmap=0 missed=0
unfollowed=0`, so the extra rows are resolved *through* the veneer rather than
merely resolved.

### The Thumb mask works, checked against the bytes

In Messenger 570's `libgraphics-core.so`, **all 53** in-place `fnPtr` words carry
bit 0 set, and **all 53** sidecar addresses equal `raw & ~1`. Every Ghidra entry
point is even, so without the mask all 53 would resolve to nothing; 50 map. The
plan estimated 92% of 32-bit ARM entries carry the bit; in this library it is 100%.

### Attribution never guessed wrong

74 of 3428 entries (2.2%) went unattributed, and **every miss is a refusal to
attribute, never a wrong class**. No run was ever attributed to a class that did
not declare every entry in it.

Each miss is the case the plan predicts — the Java half living somewhere the index
cannot see:

- TikTok Lite's `libTTMachineCore`: 11 entries, 11 runs, 0 attributed. Descriptors
  reference `Lcom/tiktok/ttm/TTMParamData;`, `TTMOutput`, `TTMContext` — a
  feature-split dex.
- TikTok 46.1.3: `libkryptonaudio`, `libkryptonaurum`, `libttmverifylite`,
  `libvcnverifylite`, `libNLEEditorJni` all attribute zero;
  `libbdvideouploader` attributes 18 and leaves 32.
- Chrome 10; Messenger 563 and 570 one each.

**This is the regression VLC was wanted for**, occurring unprompted on six
libraries across two other apps. `libbdvideouploader`'s 18/32 split is the same
shape as VLC's libbluray bindings.

### Overloads resolve; the ambiguity warning is the new one

`ambiguous` is **0 on every package but one**. Overloads the short symbol cannot
separate — Facebook Lite's two `readNative`s, TikTok's three `execute`s — resolve
on the descriptor rather than hitting the `Ambiguous` arm, which `continue`s before
any fallback could run. This is the tier-0 placement earning its keep.

The one exception is informative. TikTok 46.1.3 reports a single ambiguous method,
`SysOptimizer;->reservedForJniOffset()V`, whose symbol is carried by two native
functions and which **no table registers** — so tier 0 has nothing to offer and the
count is 1 with and without `--no-jni-registry`. Its warning is the rewritten text
the plan asks for, ending "…or bind it with RegisterNatives, which names the method
unambiguously."

### Both packaging fixes fired

**The ABI trap.** Chrome logs `skipping arm64-v8a -- it has no entry that is a
loadable object file (an empty placeholder, say)` and imports `armeabi-v7a`,
yielding 457 entries against the 0 it would have produced before.

**`.xapk`.** Seven bundles imported. The flattening regression is settled at scale
by TikTok 46.1.3: its bundle records **203 sub-imports as one flat list** (7 splits
plus 196 native libraries, no bundle nested in itself) and `ctadl index` reports
`indexing project 'tiktok' from 204 import(s)`. Because
`AnalysisProject::ephemeral` expands exactly one level, a nested list would have
silently dropped all 196 libraries — the failure the plan warns would "look exactly
like the bug this change exists to fix". Chrome shows the same property with 2
libraries; TikTok shows it with 196.

TikTok also exercises "skip, do not fail" beyond resource-only splits: three
feature splits (`df_kakao`, `df_line`, `df_pns_biz`) yield `0 native libraries
ready` and the import continues.

## Why `registered` is always below `attributed`

Three effects separate table entries from linked methods. None is a defect and the
gap will not close.

1. **Duplicate registrations.** Telegram's 83 entries are only 78 distinct
   `(name, descriptor)` pairs — `nativeCacheDirectBufferAddress` is registered 4×
   and `nativeDataIsRecorded` 3×, across different classes.
2. **Ghidra creates no function at the `fnPtr`.** 1173 of 3428 rows as measured
   here. 50 of those are branch veneers and now resolve — see below — leaving 1123.
   The rest keep `function: null` by design and still count for attribution, which
   reads the name and descriptor strings rather than the pointer.
3. **Branch veneers** — see below. This one has since been closed.

## Finding: branch veneers — fixed and re-measured

`libsuperpack-jni` mapped **0 of 28** in Messenger 563 and **28 of 28** in Facebook
Lite. Same library name, opposite result. Chasing it into the ELF:

- The 28 `fn_addr`s sit at a perfect 4-byte stride across 124 bytes.
- Every one decodes as a single AArch64 `B` — a veneer branching to the real
  implementation (`0x40e74 → 0xa02c`, and so on). Messenger links this library with
  veneers; Facebook Lite does not.
- Ghidra creates no function object at a bare 4-byte thunk, so the row got
  `function: null`.
- **21 of the 28 branch targets *are* Ghidra functions.**

Corpus-wide, **62 null rows are a single-`B` veneer, and 50 of those branch to a
function Ghidra did create.** This was never a defect in the scan — the veneer
address genuinely *is* the pointer `RegisterNatives` receives — but it was a
bounded, measured improvement, and it has now been made: the scan decodes one
AArch64 `B` at an unresolved `fnPtr` and resolves the target instead, recording the
hop as `veneer_target` and leaving `fn_addr` alone. See "Re-run: following the
veneer" below for what it produced.

**The Thumb-2 caveat was wrong, and the corpus says so.** The earlier draft called
62 a lower bound on the grounds that a 32-bit veneer would go uncounted. Decoding
the four shapes a 32-bit linker actually emits — A32 `B`, A32 `LDR pc,[pc,#-4]`,
Thumb `B`, Thumb `B.W` — over every null row in the corpus finds **one** candidate,
in Chrome, and its target is not a function either. The 32-bit misses are something
else entirely: the pointers in Messenger 570's and WhatsApp's `libsuperpack` are
ordinary Thumb function bodies (`push {r4-r7,lr}`, `mov r0,r2; b.w …`) that Ghidra
simply did not recognize as functions. No branch to follow, so 62 is the whole of
it, and the decoder is AArch64-only on purpose.

## Re-run: following the veneer

Re-imported and re-indexed fresh, same method as the original run, on the three
packages that ship `libsuperpack-jni` — the two that the finding is about and the
32-bit build as a control.

| Package | library | mapped before | mapped after | linked (registered) before | after |
| --- | --- | ---: | ---: | ---: | ---: |
| Messenger 563 (arm64) | `libsuperpack-jni` | 0/28 | **21/28** | 54 (53) | **97 (96)** |
| Messenger 563 (arm64) | `libbreakpad` | 0/23 | **18/23** | — | — |
| Messenger 563 (arm64) | `libappcomponentfactory-jni` | 0/3 | **3/3** | — | — |
| Messenger 563 (arm64) | `libdextricks-early`, `libdistract-config` | 0/1 each | **1/1** each | — | — |
| Facebook Lite 513 (arm64) | `libsuperpack-jni` | 28/28 | 28/28 | 29 (28) | 29 (28) |
| Messenger 570 (armeabi-v7a) | `libsuperpack-jni` | 14/28 | 14/28 | 89 (81) | 89 (81) |

Messenger 563 is the whole finding realized: **44 more of its 118 entries resolve
(53 → 97), and linked native methods go 54 → 97**, against an A/B baseline of 1.
Every one of the 44 is logged as going through a veneer:

```
jni registry: 28 RegisterNatives entries recovered from 'm563__arm64-v8a__libsuperpack-jni'
  (21 with a function, 21 through a branch veneer)
```

The two controls do not move at all. Facebook Lite, linked without veneers, stays
28/28 and `29 linked (28 registered)`; Messenger 570, 32-bit, stays 14/28 and
`89 linked (81 registered)`. Attribution is untouched everywhere — Messenger 563
still reports 118 entries, 117 attributed, and `ambiguous` is still 0 on all three.

The mapping audit was re-run with the veneer rule added — a mapped row must sit on
an entry point at `fn_addr`, or at `veneer_target` when it has one, and no null row
may hold a followable branch:

```
rows=265  mapped=208 (44 through a branch veneer)  null=57
mapping errors: badmap=0  missed=0  unfollowed-veneers=0  -> EXACT
of 57 null rows: 12 are a single-B veneer, of which 0 branch to a real Ghidra function
```

That last line is the fix stated from the other side. Of the 62 single-`B` rows the
corpus holds, 50 branch to a real function. All 12 that do not live in Messenger
563, and there they are exactly what still reads `function: null` — 7 in
`libsuperpack-jni`, 5 in `libbreakpad`. Nothing followable is left behind in the
three packages re-run.

The other 6 followable rows are in TikTok 46.1.3 (`libbdvideouploader`, `liblynx`,
`libshadowhook`, `libttmplayer`, `libvideodec`). Those packages were not re-imported
— this run was scoped to `libsuperpack-jni` — so 6 of the corpus's 50 are predicted
rather than measured.

## Finding: non-ELF files are filtered before the scan, not by it

TikTok 46.1.3 ships all three shapes from the plan's non-ELF table, with the
documented magic numbers:

| File | magic | bytes |
| --- | --- | ---: |
| `libdex_df_im_enterchat`, `libdex_df_livesdk_module`, `libdex_df_social_fi` | `504b0304` (`PK\x03\x04`) | ~1.3 KB each |
| `liblynxsuit2` | `534b434c` (`SKCL`) | 2 015 232 |
| `libmedia`, `libttc2pa` | `7f4b4f4d` (`\x7fKOM`) | 11 288 576 / 950 272 |

All six are rejected by `looks_like_object_file` **at the APK layer, before
extraction** — 201 libraries found, 195 extracted — so they never reach the
registry scan. The scan's three quiet returns are defence-in-depth here rather than
the mechanism that handles these files. They remain load-bearing for anything that
passes the APK-layer check and fails later.

## Not measured

**VLC 3.7.0.** Its tables are not in `libvlcjni.so` — that library imports fine and
yields no sidecar at all, meaning zero tables — nor in `libmla.so`. They are inside
`libvlc.so` (43 MB), consistent with the plan's account that the 32 unattributed
entries are libbluray's BD-J bindings, statically linked there.

Ghidra on `libvlc.so` produced **44 GB of facts** and was still growing at roughly
2–3 GB/min when a disk guard stopped it with 19 GB free; an earlier attempt through
the bundle path reached 41 GB. Two runs, neither finishing, so no sidecar was
written. Nothing about the registry scan is implicated — this is the cost of
disassembling VLC's core library, and it would apply equally without this change.
Worth retrying on a machine with ~150 GB free:

```bash
ctadl import --name vlccore <extracted>/lib/arm64-v8a/libvlc.so
```

**The `JniRegister` end-to-end regression case.** Still not run; it needs the
regression flake's javac, dx, cross-gcc, addr2line and Ghidra (`nix develop
.#regression`). It is the only regression case that reads the built library's own
bytes, so it needs an ELF target — the scan is a quiet no-op on the Mach-O a macOS
worker would produce.

## Change made during the run

One word, in `ctadl-ascent/src/languages/apk_native.rs:125`. The new ABI-skip
message read "it has no entry **there** is a loadable object file"; corrected to
"**that** is". User-visible log text, surfaced by the Chrome run.

`cargo clippy --all-targets` reports no warnings, `cargo fmt --check` is clean, and
all 16 registry unit tests pass — including the three-adjacent-tables guard
regression and the Thumb-mask test.

## Reproducing

Raw per-package logs, the results file and the analysis scripts are in
`~/jni-verify-store/`:

- `results.txt` — import/index/A-B lines per package
- `summarize.py` — regenerates the results table from the logs
- `audit.py` — the mapping audit (sidecar rows vs `HFUNC_EP`, veneer classification)

`audit.py` reads both sidecar shapes: a row written before veneer following has no
`veneer_target`, which leaves its checks exactly as they were. Point it at another
store with `CTADL_VERIFY_STORE`.

The veneer re-run is in `~/jni-verify-store2/`, same layout — Facebook Lite,
Messenger 563 and Messenger 570, each imported fresh and indexed twice — plus
`rerun.sh`, which drove it, and `classify.py`, which decodes every unresolved
`fn_addr` against the six veneer encodings and is where the AArch64-only decision
comes from.

Re-import is required throughout: the sidecar is written at import time, and
`--skip-existing` will not create one.
