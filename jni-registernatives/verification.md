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

**A third finding came later, from the regression suite rather than from this run**:
a `fnPtr` whose implementation is *exported* carries an absolute relocation against
a symbol, which the scan could not read, and one unreadable `fnPtr` costs the whole
table. That is fixed too. Across the corpus it recovers **195 entries in 20
libraries, 14 of which yielded nothing at all before**, and loses none. See "Finding:
absolute relocations against exported symbols" and "Re-run: TikTok Lite".

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

The table is the run as it stood, before the two findings below were acted on. The
veneer fix moved exactly one row: Messenger 563 is now **97 (96)**, so its gain is
1 → 97. Facebook Lite and Messenger 570 were re-run and reproduced their rows
digit for digit.

The exported-symbol fix moves the two TikTok rows and nothing else. TikTok Lite has
been re-run in full and is now **620 entries in 20 libraries, 587 attributed, 33
unattributed, 662 (510)**; TikTok 46.1.3's entry counts are known to rise by 154 but
its attribution is not yet measured. Every other row is unchanged, and that is
measured rather than assumed — re-scanning all 289 extracted libraries produces
byte-identical entries everywhere else.

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

The exported-symbol fix later grew `libTTMachineCore` from 11 entries to 32 and left
all 32 unattributed for the same reason, so the counts in this section are the
original run's; "Re-run: TikTok Lite" has the current ones. The claim itself survived
re-measurement unchanged: still a refusal, still never a wrong class.

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

## Finding: absolute relocations against exported symbols — fixed and re-measured

This one did not come from the corpus. It came from `Jni:JniRegister`, the
end-to-end regression case the original run left unmeasured, which failed on all
three of its packaging variants.

The scan read a `JNINativeMethod` slot two ways: the addend of a *relative* dynamic
relocation at that address, or the word stored in the file. Neither reaches a slot
carrying an **absolute relocation against a symbol**. Those appear whenever the
implementation is *exported* — an exported function in a shared object is
preemptible, so the linker cannot resolve the pointer at link time and emits
`R_AARCH64_ABS64` (or `R_X86_64_64`) against the symbol instead of `R_*_RELATIVE`.
The file leaves the slot zero, the scan reads that zero, it fails the
executable-section test, and the *whole table* is discarded — one unreadable `fnPtr`
rejects the entire triple.

`nightly/tests/jni/JniRegister.c` is that shape deliberately: it gives `stash_impl`
and `fetch_impl` external linkage so Ghidra reliably makes functions of them. Adding
`-fvisibility=hidden` to the fixture flips both relocations to `R_X86_64_RELATIVE`
and all fourteen regression cases pass — which is what isolated the relocation form
as the sole cause. That flag is not the fix; it hides the gap.

The fix extends the slot rule to a third source: for an absolute pointer-width
relocation against a symbol *defined in this object*, the value is
`st_value + addend`. An undefined symbol is refused — it is bound by some other
library at load time and its `st_value` here is zero, which would only move the bug.
Relative relocations keep priority where both apply.

**How much this was costing, measured across the corpus.** Every extracted `.so` in
`~/jni-verify-store` was re-scanned under both builds and the results diffed:

```
libraries scanned: 289        not-ELF: 0 -> 0
libraries with >= 1 entry:   99 -> 113
total entries:             3428 -> 3623   (+195)
entries lost or changed:      0
```

**Nothing was lost or altered; 195 entries were gained across 20 libraries, 14 of
which recovered nothing at all before.** All of it is TikTok and TikTok Lite:
`libmonitorcollector-lib` +55, `libkeva` +26, `libTTMachineCore` +21 in each build,
`libpns_ttmachine_adapter` +14, `libttr` +10 in each, `librex` +9, and a tail of
smaller ones.

The gain was not taken on trust. A separate ELF decoder re-derived all 195 rows from
the file bytes, requiring each to have an `R_AARCH64_ABS64` at the `fnPtr` slot, a
defined symbol, `st_value + addend` equal to the recovered address, a zero in-place
word (so no other rule could have produced it), and a target inside an executable
section: **195 confirmed, 0 discrepancies**.

**The mapping audit for all 195, without re-importing.** Each import already holds
its Ghidra entry points as `facts/HFUNC_EP.facts.gz`, so the audit's rule could be
applied to the new rows directly:

```
whole corpus, fixed build:   rows=3623  mapped=2500 (50 via a veneer)  null=1123
the 195 new rows:            rows=195   mapped=195                     null=0
```

**Every new row lands on a real Ghidra function entry point.** A 100% rate against
the corpus average of 69% is expected rather than suspicious: these rows exist
*because* their implementations are exported, and an exported function is exactly
what Ghidra reliably makes a function of — the same reason `JniRegister.c` gives its
implementations external linkage.

For the 41 new rows whose import still carries `HFUNC_NAME`, Ghidra's function name
at the entry point was compared against the ELF symbol at that address: **all 41
agree** — 36 identical (`ttmNativeCreate`, `ttmNativePutLong`, …) and 5 where the ELF
holds the mangled form and Ghidra the demangled one
(`_Z17GetDeclaredMethodP7_JNIEnv…` against `GetDeclaredMethod`). The addresses do not
merely hit *a* function; they hit the right one.

The remaining 154 could not be name-checked because TikTok 46.1.3's imports have
been pruned to `HFUNC_EP` alone — enough for the mapping audit, not enough to index.

## Re-run: TikTok Lite

Re-imported and re-indexed fresh, same method as the original run. TikTok Lite was
chosen because it is the smaller of the two packages the fix touches and its imports
are intact; TikTok 46.1.3 needs a full re-import, which is still outstanding.

| | before | after |
| --- | ---: | ---: |
| entries | 579 | **620** |
| libraries | 16 | **20** |
| attributed | 567 | **587** |
| unattributed | 12 | **33** |
| linked (registered) | 644 (490) | **662 (510)** |
| A/B `--no-jni-registry` | 154 | **154** |
| ambiguous | 0 | 0 |

Import took 1707s against the original 1606s. The four newly productive libraries are
`libflipped`, `libflock-lib`, `libttr` and `libtttext_lite`.

The mapping audit, `audit.py` unmodified against the new sidecars:

```
rows=620  mapped=543  null=77
mapping errors: badmap=0  missed=0  unfollowed-veneers=0  -> EXACT
of 77 null rows: 0 are a single-B veneer, of which 0 branch to a real Ghidra function
```

**Every one of the 41 new entries is accounted for, and they split cleanly.** Twenty
attributed with none left over — `libttr` +10, `libtttext_lite` +4, `libpsp_engine`
+3, and one each in `libflipped`, `libflock-lib` and `libhelios_bm` — which is exactly
the `registered` gain of 490 → 510. The other twenty-one are all in
`libTTMachineCore`, which went 11 entries to 32 and attributed **zero of them both
before and after**: each entry closes its own run, because the classes declaring
`ttmNativeCreate` and the rest are not in TikTok Lite's `classes.dex`. That is the
feature-split-dex library this document already records, and the fix found 21 more
entries of the same kind. So the whole +21 unattributed is more of a miss that
already existed, not a new failure mode — the unattributed set is still the same two
libraries, `libTTMachineCore` and `libanimax`'s single entry.

**Attribution still never guessed wrong.** No run was attributed to a class that does
not declare every entry in it, and `ambiguous` stayed 0.

Two controls held. The **A/B did not move**, 154 → 154, as it must not: it measures
what the `Java_` convention achieves alone, which no registry change can touch. And
`linked` rose 18 while `registered` rose 20, because two of the newly registered
methods were already linked by symbol — `libpsp_engine`'s implementations carry
`Java_com_tiktok_…` names. That gap is expected, not a loss.

One methodological result worth keeping: the corpus re-scan *predicted* 579 → 620
entries across 16 → 20 libraries, and the real import reproduced it exactly, with the
mapping audit agreeing with the `HFUNC_EP` reconstruction. The Ghidra-free method is
therefore checked against ground truth, which is what the 154 TikTok 46.1.3 rows
currently rest on.

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

**TikTok 46.1.3's attribution.** Its entry count rises by 154 under the
exported-symbol fix, and all 154 rows are mapping-audited, but what they attribute to
is unmeasured. It cannot be recovered cheaply: its 196 native imports were pruned to
`HFUNC_EP.facts.gz` alone, with no IR bitcode, so `ctadl index` has nothing to read
and the full ~2.5 h re-import is the only route.

**The `JniRegister` end-to-end regression case has since been run**, and this is what
turned up the exported-symbol finding. It needs the regression flake's javac, dx,
cross-gcc, addr2line and Ghidra:

```sh
nix develop .#regression -c cargo xtask regression --frontend jni
```

All fourteen cases pass, `Jni:JniRegister`, `+apk` and `+split-apks` among them. It
is the only regression case that reads the built library's own bytes.

The ELF-target concern above did not materialize, and it is worth saying why, since
it is what makes the case meaningful on a macOS worker at all: `pick_toolchain`
prefers an x86_64 Linux cross-compiler over the native one, and the regression flake
supplies `x86_64-unknown-linux-gnu-gcc`. The library under test is therefore an ELF
whatever the host is. That the scan really reads it was checked rather than assumed —
stubbing out the absolute-relocation arm fails all three `JniRegister` variants with
"no code flow connects a source to a sink", which can only happen if the scan is
parsing a real ELF.

## Change made during the run

One word, in `ctadl-ascent/src/languages/apk_native.rs:125`. The new ABI-skip
message read "it has no entry **there** is a loadable object file"; corrected to
"**that** is". User-visible log text, surfaced by the Chrome run.

`cargo clippy --all-targets` reports no warnings, `cargo fmt --check` is clean, and
all 16 registry unit tests pass — including the three-adjacent-tables guard
regression and the Thumb-mask test.

## Changes made since

The registry unit tests now number **22**, the six added since covering the veneer
decoder and the exported-symbol path. The two for the latter are load-bearing,
checked by breaking the code and watching them fail:

- a zero `fnPtr` slot with an `R_AARCH64_ABS64` against a defined symbol recovers
  `st_value + addend`;
- an *undefined* symbol recovers nothing. That test carries an addend of `TEXT_ADDR`
  on purpose: with addend 0 the sum is 0 and the executable-code test rejects it
  anyway, so as first written the assertion held with the `SHN_UNDEF` guard deleted
  and proved nothing.

`cargo fmt --check` is clean and `cargo clippy -p ctadl-ascent --all-targets
--all-features` reports no warnings. Workspace-wide clippy now reports two, in
`jvm-reader/src/flow.rs` and `rustc_graphviz` — code none of this work touches, so
they arrived from elsewhere on the branch between that run and this one.

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

The TikTok Lite re-run for the exported-symbol fix is in `~/jni-verify-store3/`, same
layout, plus the `ttlite_rerun.sh` that drove it — a one-package cut of
`jni-verify-store2/rerun.sh`. `audit.py` runs against it unmodified with
`CTADL_VERIFY_STORE`.

Re-import is required throughout: the sidecar is written at import time, and
`--skip-existing` will not create one.

**One caveat about `~/jni-verify-store/`:** its TikTok 46.1.3 imports have been pruned
to `facts/HFUNC_EP.facts.gz`, dropping the IR bitcode and every other fact. That is
enough for `audit.py` and for the corpus re-scan, and not enough for `ctadl index`.
Anything needing TikTok attribution must re-import the package.

### Corpus re-scan without Ghidra

The whole-corpus entry comparison above needs no import at all, because `scan_bytes`
is pure ELF parsing. It was driven by a temporary `#[ignore]`d test in
`registry/tests.rs` that walks a directory for `*.so`, runs `scan_bytes` on each and
dumps every recovered row; running it under two builds and diffing gives the entry
delta directly. The harness was deleted after use — it is a few dozen lines to
rewrite, and cheap to re-derive, whereas leaving a corpus-dependent test in the tree
is not.

This is the right first move for any future change to the scan: it is seconds rather
than hours, it covers every library at once, and combined with `HFUNC_EP` it answers
the mapping audit too. It cannot answer attribution, which needs the Dex side and
therefore a real index.
