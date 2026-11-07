# Linking natives bound through `RegisterNatives` — what was built -- DO-NOT-MERGE

Implements `jni-registernatives-plan.md`. This file records what changed, where it
differs from the plan, and what is still unverified.

## The change in one paragraph

CTADL's JNI bridge used to link a Java `native` method to its implementation only
by mangled symbol name. Most Android apps do not use that convention — they bind
their natives at run time from `JNI_OnLoad`, passing a `JNINativeMethod[]` to
`RegisterNatives`, and the implementations keep private, unexported names. CTADL
now recovers those tables straight out of each shared library's data sections when
it imports the library, recovers each entry's declaring class from the Dex side
when it indexes, and consults the result as the bridge's first resolution tier.
Two packaging defects that kept the feature from reaching most real apps are fixed
alongside it: `.xapk` bundles can now be imported, and the ABI preference no longer
picks an empty placeholder.

## Step by step

### 1. Recover the tables from the ELF, at import time

New module `ctadl-ascent/src/languages/jni/registry.rs`. It needs neither Ghidra
nor any dataflow analysis, because a `JNINativeMethod[]` is plain initialized data.

The scan walks every writable, non-executable `PROGBITS` section (`.data.rel.ro`,
`.data`) at pointer stride and accepts three consecutive slots as an entry when the
first points at a valid Java method name, the second at a well-formed method
descriptor, and the third into an executable section. A slot's value is the addend
of a relative dynamic relocation at that offset when one exists, and otherwise the
word stored in the file.

Four details the scan would get wrong if left unstated, all of them implemented:

- **Relocation sections are selected by `sh_type`, never by name.** Android ships
  two packed formats (`SHT_ANDROID_RELA` = `0x60000002`, `SHT_ANDROID_RELR` =
  `0x6fffff00`) under the standard section names, and decoding an APS2 blob as an
  array of `Elf_Rela` yields plausible-looking garbage. Only `SHT_RELA` is read;
  everything else falls through to the in-place read, which is correct for those
  formats anyway. That fallback is also why `RELR` needs no decoder.
- **The scan advances by a whole entry on a match, by one pointer otherwise.** That
  is what makes table *contiguity* meaningful, and contiguity is the entire basis
  of class attribution in step 4.
- **Bit 0 of `fnPtr` is masked on 32-bit ARM.** A Thumb function pointer carries it
  set, and unmasked it matches no function entry point.
- **Every failure is a quiet no-op** — bad magic, a truncated header and a
  zero-length file are three separate returns. This is not just Mach-O insurance:
  real apps ship ZIPs of dex and custom packed containers under `lib/`.

Parsing uses the `object` crate (`default-features = false`, features `read_core`,
`elf`, `std`). Version 0.37.3 was already in `Cargo.lock` transitively, so this
adds no new version to the tree.

### 2. Map each `fnPtr` to an IR function

`Context::process_functions` (`languages/pcode/mod.rs`) now records
`entry_point -> fq_name` alongside the function map it already built. `fq_name` is
exactly the string in the `NativeFunction` VMT column, which is what `link`
resolves to a `FunctionId`, so it is the right key to persist.

`import_pcode` runs the scan between `ctx.process` and `ctx.finish` — the first
point at which both halves of the address translation are known. A Ghidra address
is `image_base + (ELF vaddr - first PT_LOAD p_vaddr)`; that last term is read from
the ELF rather than assumed to be zero, and logged when it is not.

**Branch veneers are followed.** A `fnPtr` is not always the implementation. When
the linker cannot reach it from the table's range it emits a *veneer* — a
four-byte stub holding one `B` — and that stub's address is what reaches
`RegisterNatives`. Ghidra makes no function out of a bare thunk, so the entry
would resolve to nothing. When and only when the pointer itself named no
function, the scan decodes one AArch64 `B` at `fn_addr`, checks the target is
executable, and resolves that instead. The hop is recorded as `veneer_target`;
`fn_addr` keeps the address the table holds, so a spot-check against the ELF still
lines up. A veneer Ghidra *did* make a function of is left alone — that function
is already the right answer, and following it would name the callee instead.

Three deliberate limits, each of them measured rather than assumed:

- **One hop.** Every veneer in the reference corpus branches straight to its
  implementation. A chain would leave the row unresolved, exactly as before.
- **`B` only.** `BL` differs by one bit and would follow a call out of a real
  function body.
- **AArch64 only.** Decoding the four shapes a 32-bit linker emits over every
  unresolved row in the corpus finds one candidate, whose target is not a function
  either. The 32-bit misses are ordinary Thumb function bodies the disassembler did
  not recognize — no branch to follow.

An entry whose address has no function, after that, keeps its row with
`function: null`. It is counted, not dropped: dropping one would punch a hole in
the contiguity step 4 depends on.

### 3. Persist as a sidecar

`jni-registry.json` is written into the import directory beside `ir-vmt.bitcode`,
with `JNI_REGISTRY_FILE` and `ArtifactImport::jni_registry_path()` next to the
existing accessors. Rows carry both `table_addr` (what attribution orders by) and
`fn_addr` (what a human spot-checks against the ELF), sorted by `table_addr`, plus
`veneer_target` on the rows that went through a veneer.

A sidecar rather than a new VMT column, so **`IMPORT_FORMAT_VERSION` stays `"5"`**
and every existing import keeps loading. An import without the file contributes
nothing. Every field carries `#[serde(default)]`, which is what let `veneer_target`
be added later without a version bump: a sidecar written before it reads back with
the field absent, which is `None`, which is what a row that resolved directly says
anyway.

`ctadl inspect <import-dir>/jni-registry.json` prints the rows. That dispatch is
two-sited, and both sites have a branch; since the file is JSON rather than
bitcode, `main.rs` calls a small dedicated printer instead of routing through
`inspect_bitcode`.

### 4. Attribute a class, then link, at index time

A table entry carries a name, a descriptor and a function — but not the class. The
class is recovered from the Dex side.

The **table** side of attribution runs per import, since `table_addr` order means
nothing across libraries. The **Java candidate** side is project-global, built from
the deduplicated `natives` list `link` already had. That asymmetry is what makes
split APKs work at all: in an app bundle the `.so` and the `classes.dex` live in
different imports.

Attribution walks the address-ordered entries keeping the set of Java classes that
declare *every* entry in the current run, matched on name and full descriptor.
When the set empties the run closes and a new one starts. A closed run with exactly
one surviving class attributes all of its entries to it. Matching is by
containment, never by count. Runs also split at an address gap and at a repeated
`(name, descriptor)`.

Anything not attributed is counted and logged, never guessed at. There is no
uniqueness tier; the plan's measurement — zero unattributed entries corpus-wide
whose `(name, descriptor)` is globally unique — is recorded in `docs/jni.md` so the
idea is not re-proposed without new evidence.

`JniObserver` gained the registry rows and an `observe_registry` call beside the
existing `observe`, inside `if !no_jni_bridge` and gated again on
`!no_jni_registry`. `JniObserver::is_empty` no longer treats an empty symbol table
as "nothing to bridge", since a registry is a native half on its own.

`resolve` gained a **tier 0** consulting the attribution result. It runs *before*
the long-name attempt, not as a fallback: the `Ambiguous` arm `continue`s before
reaching any fallback, so a fallback-only registry would never rescue an overloaded
native — the case `RegisterNatives` matters most for. Where a method resolves both
ways, the registration wins (what the runtime does) and the disagreement is logged.
`resolve` still returns exactly one answer, so `emit_bridge` cannot double-bridge.

## Reporting and flags

`LinkStats` gained `registered` (a subset of `linked`) and `unattributed` (table
entries, not methods). It stays `Copy` and flat; per-library reporting is logged at
attribution time, and only for libraries that actually have tables.

```
jni registry: 3 table(s), 28 entr(ies) in app__arm64-v8a__libsuperpack-jni: 28 attributed to 3 class(es), 0 unattributed
jni bridge: 535 native method(s): 29 linked (28 registered), 506 unresolved, 0 ambiguous
```

`cli::index` took nine positional arguments under
`#[allow(clippy::too_many_arguments)]`. It now takes **`cli::IndexOptions`**,
following the existing `ImportOptions`, and the allow is retired. Five call sites
were updated: `main.rs` and four integration tests.

`--no-jni-registry` was added to `IndexArgs` (both literals, including the legacy
path) and to `GoArgs`. It ignores the sidecar rows, which gives a clean A/B for
what this change contributes without re-importing. Scanning stays unconditional at
import time; it costs milliseconds.

## Packaging fixes

### `.xapk` bundles

Nine of the seventeen packages in the reference corpus are `.xapk`, and CTADL could
not import them: with no `.xapk` arm in the extension table, a bundle fell through
to `file_looks_binary`, which sees the NULs in the ZIP and handed the whole thing
to Ghidra.

New `ImportLanguage::Xapk`, `ArtifactLanguage::Xapk`, an extension-table arm, and
`ctadl-ascent/src/languages/xapk.rs`, which extracts each top-level `*.apk` into
`<import-dir>/splits/`, imports each through the existing APK path, and forwards
`ImportOptions` unchanged. Two constraints that are easy to get wrong:

- **The sub-import list is flat.** `AnalysisProject::ephemeral` expands exactly one
  level, so the bundle records, for each split, its own name followed by its own
  sub-imports. Nesting would silently drop every `.so` at index time.
- **Resource-only splits are skipped, not fatal.** They are the majority. The
  bundle importer catches exactly `Error::NothingToImport`, logs at debug, and
  continues; anything else propagates.

Dex-bearing splits are imported first, so the Java half is observed before the
native half.

### The ABI trap

Chrome ships `lib/arm64-v8a/libplaceholder.so` at zero bytes and its real code as
`lib/armeabi-v7a/libelements.so`. Taking the preference order literally selected
the placeholder and yielded an import with no native libraries at all.

`preferred_abi` (in `dex-reader`) now skips an ABI whose every entry fails
`looks_like_object_file` and falls through to the next in the preference order,
returning what it passed over so the caller can say so. It decompresses only the
first four bytes of each candidate entry. An explicit `--native-abi` is still
honored as given, including when it names an unusable ABI.

## Files

| File | Change |
| --- | --- |
| `ctadl-ascent/src/languages/jni/registry.rs` | new: ELF scan, sidecar read/write, run attribution |
| `ctadl-ascent/src/languages/jni/registry/tests.rs` | new: attribution and parsing tests |
| `ctadl-ascent/src/languages/jni.rs` | `mod registry`; observer field; tier 0 in `resolve`; stats; header |
| `ctadl-ascent/src/languages/jni/tests.rs` | new link-level tests for the registry tier |
| `ctadl-ascent/src/languages/pcode/mod.rs` | entry-point map; call the scan from `import_pcode`; `ghidra` visibility |
| `ctadl-ascent/src/languages/xapk.rs` | new: unwrap bundle, import each split, flatten, skip resource-only |
| `ctadl-ascent/src/languages/mod.rs` | `pub mod xapk` |
| `ctadl-ascent/src/project.rs` | `JNI_REGISTRY_FILE`, `jni_registry_path()`, `ArtifactLanguage::Xapk` |
| `ctadl-ascent/src/cli/mod.rs` | `IndexOptions`; `observe_registry`; `inspect_jni_registry`; `Xapk` import arm |
| `ctadl-ascent/src/main.rs` | `--no-jni-registry`; `IndexOptions`; inspect gate; `ImportLanguage::Xapk` |
| `ctadl-ascent/tests/xapk_bundle.rs` | new: bundle import and the flattening regression |
| `ctadl-ascent/tests/{sarif_uris,bridging_end_to_end,port_semantics,multi_import_sarif}.rs` | `cli::index` call sites |
| `ctadl-ascent/Cargo.toml` | `object` dependency |
| `dex-reader/src/apk.rs` | `preferred_abi` skips unusable ABIs; bundle listing/extraction helpers |
| `xtask/src/discovery.rs` | test: a case with no bridge model yields no A/B pair |
| `nightly/tests/jni/JniRegister.{java,c}`, `jni-register.json5` | new end-to-end case |
| `docs/jni.md`, `docs/model-generators.md` | rewritten limitation, new section, packaging notes |

## Where this differs from the plan

**Guard 3 is a backstop, not load-bearing.** The plan presents "cap a run at the
number of natives its class declares" as one of three guards that split runs, and
credits guards 2 and 3 together with turning 2 runs into 3 on
`libsuperpack-jni.so`. Under guards 1 and 2 with containment matching, guard 3
provably cannot fire: a class that declares every *distinct* entry of a run
declares at least as many natives as the run is long, and guard 2 is what
guarantees the entries are distinct. Guard 2 alone recovers the third boundary.

Guard 3 is implemented anyway — it costs one comparison, and the proof above rests
on the Java side never declaring the same signature twice, which is an assumption
about someone else's input. It is tested directly against the rule rather than
through a fixture that cannot reach it, and the code comment says all of this.

**The end-to-end config is `jni-register.json5`, not `.json`.** The file carries
inline commentary explaining its line numbers, and the `.json5` spelling is
unambiguously parseable by both the xtask config reader and the model loader.
Discovery prefers `.json5`, so the case is still found as `Jni:JniRegister`.

**A skipped split's import directory is removed.** The plan says to catch
`NothingToImport` and continue, which leaves behind an import config created just
before the failure. On a real bundle that would put an entry in `ctadl inspect`'s
listing for every language and density the app ships. The bundle importer now
deletes the directory, best-effort.

## Testing

Everything below passes; `cargo clippy --workspace --all-targets` reports no new
warnings and the tree is `cargo fmt` clean.

**Unit tests, `registry/tests.rs`** — attribution is driven from synthetic
`(table_addr, name, descriptor)` lists, since it needs no ELF: the Facebook Lite
shape (three adjacent tables, subset runs, asserting **3** runs and not 2), each
guard in isolation, a run that stays multi-class, the VLC shape (a library whose
Java half is absent attributes zero and links zero), and overloads distinguished by
descriptor. Parsing is driven from ELF byte buffers built in-test: ELF64 through a
standard `SHT_RELA` section, ELF64 with the value in place, ELF32 at stride 12 with
the Thumb bit set, a `fnPtr` outside executable code, the three quiet no-ops, and a
`.rela.dyn` typed `SHT_ANDROID_RELA` asserted to be ignored — the same fixture with
`SHT_RELA` asserts the opposite, so it tests the gate rather than the fixture.

Veneers get five more: the instruction decoder against the word Messenger 563
actually ships (`0x17ff246e` at `0x40e74`, which is `0xa02c`) plus both extremes of
the immediate; `BL`, `RET`, a real `STP` prologue and padding all rejected, since
`BL` is one bit away and following it would leave the function; a `fnPtr` at a
veneer recording its target while `fn_addr` stays put; a `fnPtr` at ordinary code
recording nothing; and a branch leaving executable code refused. The ELF32 test
asserts no veneer target, which is a statement about the machine gate rather than
about the fixture's bytes.

**Link-level tests, `jni/tests.rs`** — a native bound only by registration, a
registration beating a matching symbol with exactly one bridge emitted, a
registration rescuing an overload the short symbol cannot resolve, an unattributed
entry counted and not linked, and a registry counting as a native half on its own.

**`tests/xapk_bundle.rs`** — a fixture bundle with a Dex-bearing split, a
native-only split and a resource-only split, asserting all three are extracted, the
resource-only one is skipped, and the Dex-bearing one is imported first. The
flattening regression is asserted directly against `AnalysisProject::ephemeral`,
both ways round: the flat list reaches the library, the nested one silently does
not.

**`dex-reader`** — the Chrome shape (an ABI whose only entry is zero-length is
skipped), one usable entry keeping an ABI, and the no-usable-ABI fallback.

**End-to-end, `nightly/tests/jni/JniRegister.{java,c}`** — two natives whose
implementations export non-`Java_` names, bound through a file-scope
`JNINativeMethod[]`, with taint entering one and leaving through the other. It
fails outright under the symbol convention alone. Discovery picks it up as
`Jni:JniRegister`, `+apk` and `+split-apks`; no `+bridge` variant, which a new
discovery test asserts. Running it needs `nix develop .#regression`.

## Measured against the real APKs

The verification the plan asks for has now been run, on Ghidra 12.0.4, against
**thirteen of the fourteen packages in `~/apps`** — every one except VLC, which
cannot be imported on this machine for a disk reason explained under "Not done".
Each package was imported fresh (no `--skip-existing`, which would not write a
sidecar) and indexed twice: once normally and once with `--no-jni-registry`, which
gives the A/B directly.

Indexing was stopped once the JNI report printed. Resolution happens before the
flow-relation stage and cannot be changed by it; this was confirmed by running
Facebook Lite both ways and getting byte-identical registry and bridge lines.
Facebook Lite and FX File Explorer additionally completed a full index.

`entries`/`attributed` are table entries; `linked`/`registered` are Java methods.
The two never match exactly, and should not be read as if they should — see
"Reading the numbers" below. `A/B` is `linked` with `--no-jni-registry`, i.e. what
the symbol convention alone achieves.

| Package | entries | libs | attributed | unattr. | linked (registered) | A/B | plan predicted |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Facebook Lite 513 | 28 | 1 | 28 | 0 | **29 (28)** | 1 | 28 entries, 28 attr ✅ |
| FX File Explorer 9.1.0.8 | 0 | 0 | — | — | 10 (0) | 10 | no tables ✅ |
| Chrome 149 (armeabi-v7a) | 457 | 1 | 447 | 10 | 264 (253) | 11 | 447 attr ✅ |
| Telegram 12.7.3 (xapk) | 83 | 1 | 83 | 0 | 458 (79) | 379 | 83 entries, 100% ✅ |
| Telegram 12.8.3 (apk) | 83 | 1 | 83 | 0 | 460 (79) | 381 | 83 entries, 100% ✅ |
| Telegram 12.9.0 (xapk) | 83 | 1 | 83 | 0 | 460 (79) | 381 | 83 entries, 100% ✅ |
| TikTok Lite 44 | 579 | 16 | 567 | 12 | 644 (490) | 154 | 579/16/567 ✅ |
| Messenger 563 (arm64) | 118 | 7 | 117 | 1 | 54 (53) | 1 | 118/7/117 ✅ |
| Messenger 570 (armeabi-v7a) | 119 | 7 | 118 | 1 | 89 (81) | 8 | 119/118 ✅ |
| WhatsApp Msgr 2.26.20 | 19 | 1 | 19 | 0 | 9 (8) | 1 | (not in plan) |
| WhatsApp Msgr 2.26.27 | 22 | 2 | 22 | 0 | 12 (11) | 1 | 22/2/22 ✅ |
| WhatsApp Business 2.26.21 | 19 | 1 | 19 | 0 | 20 (19) | 1 | 19/19 ✅ |
| TikTok 46.1.3 (xapk) | **1818** | **60** | **1768** | 50 | 8785 (1037) | 7748 | 1818/60/1768 ✅ |
| VLC 3.7.0 | \* | \* | \* | \* | \* | \* | *not measurable here — see "Not done"* |

\* VLC is the one package that could not be imported on this machine; its
`libvlc.so` exhausts the disk under Ghidra. Every other package in `~/apps` was
measured.

Totals across the thirteen: **3428 table entries in 99 libraries, 3354 attributed
(97.8%)**, and linked native methods rising from **9077 to 11 294** — 2217 methods
that had no implementation before.

This table records the run as it stood before branch veneers were followed. That
change moved one row of it: Messenger 563 now reads **97 (96)** rather than 54
(53). Everything else, including every entry and attribution count, is unaffected —
re-running Facebook Lite and Messenger 570 reproduced their rows exactly.

**Every entry, library and attribution count the plan predicted was reproduced
exactly** — all eleven packages the plan tabulated, including the two largest.
Facebook Lite's headline claim holds: `29 linked (28 registered)` against `1`
before. So does the spot-check — `SuperpackFile;->readBytesNative(JII[BI)V` sits at
`fn_addr` `0x10c20`, the address the plan names.

The A/B baselines mostly match the plan's `Java_`-symbol counts to the digit —
Chrome 11, Telegram 381, Facebook Lite and all three WhatsApp builds 1 — but not
always: TikTok Lite gives 154 against a predicted 155, and Messenger 570 gives 8
against 10. The two are not the same measurement and need not agree. The plan
counted `Java_` symbols present in the ELF; the A/B counts Java methods that
actually link, so an exported symbol with no matching `native` declaration in the
Dex is in the first number and not the second.

TikTok 46.1.3 is the scale case and it lands on all three of its predicted numbers
at once — 1818 entries, 60 libraries, 1768 attributed — plus `7 split(s) imported,
23 resource-only split(s) skipped`, the 23-of-30 the plan calls for.

`ambiguous` is **0 on every package but one**. That is the tier-0 placement
earning its keep: overloads that the short symbol cannot separate — Facebook
Lite's two `readNative`s, TikTok's three `execute`s — resolve on the descriptor
instead of hitting the `Ambiguous` arm, which `continue`s before any fallback
could run.

The single exception is instructive rather than a failure. TikTok 46.1.3 reports
one ambiguous method, `SysOptimizer;->reservedForJniOffset()V`, whose symbol is
carried by two native functions and which no table registers — so tier 0 has
nothing to offer and the count is 1 with and without `--no-jni-registry`. Its
warning is the rewritten one the plan asks for:

```
symbol 'Java_com_bytedance_sysoptimizer_SysOptimizer_reservedForJniOffset' is
ambiguous (2 native functions carry that name). Give the implementation its long
(descriptor-qualified) name to disambiguate, or bind it with RegisterNatives,
which names the method unambiguously.
```

### The `fnPtr` → function mapping is exact

Every sidecar row was cross-checked against Ghidra's own `HFUNC_EP` entry-point
facts, using the `image_base` each import records in its `import_config.json`
rather than an assumed one. Across all libraries with tables:

```
rows=3428  mapped=2255  null=1173
mapping errors: badmap=0  missed=0  -> EXACT
```

No mapped row points anywhere but a real function entry point, and no null row had
an entry point that was overlooked. Step 2's address arithmetic is correct on both
ELF64 and ELF32, through both the reloc path and the in-place path.

The **Thumb mask** was checked against the raw ELF rather than inferred from
counts. In Messenger 570's `libgraphics-core.so`, all 53 in-place `fnPtr` words
have bit 0 set, and all 53 sidecar addresses equal `raw & ~1`. Every Ghidra entry
point is even, so without the mask all 53 would resolve to nothing; 50 map. The
plan estimated 92% of 32-bit ARM entries carry the bit — here it is 100%.

### Reading the numbers: `registered` < `attributed`, always

Three distinct effects separate table entries from linked methods. None is a
defect, and the gap will never close:

1. **Duplicate registrations.** Telegram's 83 entries are only 78 distinct
   `(name, descriptor)` pairs — `nativeCacheDirectBufferAddress` is registered 4×
   and `nativeDataIsRecorded` 3× across different classes.
2. **Ghidra creates no function at the `fnPtr`.** 1173 of 3428 rows in the run as
   first measured; 50 fewer now that veneers are followed. These keep
   `function: null` by design, exactly as step 2 specifies, and still count for
   attribution because attribution reads the name and descriptor strings.
3. **Branch veneers — since fixed.** Of those 1173 nulls, **62 were a single
   AArch64 `B`** — a veneer jumping to the real implementation — and **50 of the 62
   branched to a function Ghidra did create**. Messenger 563's `libsuperpack-jni`
   was the clean case: all 28 `fnPtr`s sit at a 4-byte stride and every one is a
   `B`, which is why that library mapped 0 of 28 while the *same library* in
   Facebook Lite mapped 28 of 28.

   Step 2 now follows that branch. Re-running the three packages that ship
   `libsuperpack-jni` takes Messenger 563 from 53 mapped entries to 97 and from
   `54 linked (53 registered)` to `97 linked (96 registered)`, and moves neither
   control: Facebook Lite stays 28/28, the armeabi-v7a Messenger 570 stays 14/28.
   The verification doc's "Re-run: following the veneer" has the table.

   The 32-bit half of that finding did not survive measurement. Decoding the four
   shapes a 32-bit linker emits over every null row in the corpus turns up one
   candidate, whose target is not a function either; the 32-bit misses are ordinary
   Thumb function bodies Ghidra did not recognize. So 62 is not a lower bound, it is
   the count, and the decoder is AArch64-only deliberately.

### Unattributed entries are the Java half being absent

74 entries went unattributed across the corpus — 50 of them in TikTok 46.1.3, and
2.2% of 3428 entries overall. Each is the case the plan predicts, with nothing
misattributed:

- **TikTok Lite, 11 of 12** are in `libTTMachineCore`, whose descriptors reference
  `Lcom/tiktok/ttm/TTMParamData;`, `TTMOutput` and `TTMContext` — classes that ship
  in a feature-split dex. The log reads `11 table(s), 11 entries … 0 attributed to
  0 class(es)`: the candidate set empties on every entry, so each entry closes its
  own run and no class is invented. This is the exact regression the plan wanted
  VLC for, occurring spontaneously on a different app.
- **TikTok 46.1.3** repeats the shape at scale. `libbdvideouploader` attributes 18
  and leaves 32; `libkryptonaudio`, `libkryptonaurum`, `libttmverifylite`,
  `libvcnverifylite` and `libNLEEditorJni` attribute *zero* and leave every entry
  unattributed. The 32-entry case is the same shape as VLC's 32 libbluray
  bindings — a whole library's Java half living somewhere the index cannot see.
- Chrome 10, Messenger 563 and 570 one each.

Note the direction of the errors: every miss is a *refusal to guess*, never a
wrong class. Across 3428 entries no run was attributed to a class that did not
declare every entry in it.

### The two packaging fixes both fired

- **`.xapk`.** Seven bundles imported. Chrome: `3 split(s) imported, 1
  resource-only split(s) skipped`; both WhatsApp Messenger builds and Telegram: 2
  skipped; TikTok 46.1.3: `7 split(s) imported, 23 resource-only split(s)
  skipped`.

  **The flattening regression is settled at scale by TikTok.** Its bundle records
  **203 sub-imports as one flat list** — 7 splits plus 196 native libraries, with
  no bundle nested inside itself — and `ctadl index` reports `indexing project
  'tiktok' from 204 import(s)`. Since `AnalysisProject::ephemeral` expands exactly
  one level, a nested list would have silently dropped all 196 libraries, which is
  precisely the failure the plan warns would "look exactly like the bug this change
  exists to fix". Chrome demonstrates the same property with 2 libraries; TikTok
  does it with 196.

  TikTok also exercises the "skip, do not fail" rule beyond resource-only splits:
  three of its feature splits (`df_kakao`, `df_line`, `df_pns_biz`) yield `0 native
  libraries ready` and the import continues.
- **The ABI trap.** Chrome logs `skipping arm64-v8a -- it has no entry that is a
  loadable object file (an empty placeholder, say)` and imports `armeabi-v7a`,
  yielding 457 entries against the 0 it would have produced before.

`ctadl inspect <import-dir>/jni-registry.json` prints the rows as intended (the
path must be inside the store the command is pointed at).

### The non-ELF files are filtered before the scan, not by it

TikTok 46.1.3 ships all three shapes from the plan's non-ELF table, and their
magic numbers are exactly as documented:

| File | magic | bytes |
| --- | --- | ---: |
| `libdex_df_im_enterchat`, `libdex_df_livesdk_module`, `libdex_df_social_fi` | `504b0304` (`PK\x03\x04`) | ~1.3 KB each |
| `liblynxsuit2` | `534b434c` (`SKCL`) | 2,015,232 |
| `libmedia`, `libttc2pa` | `7f4b4f4d` (`\x7fKOM`) | 11,288,576 / 950,272 |

Worth stating precisely, because it differs from the plan's framing: all six are
rejected by `looks_like_object_file` at the APK layer **before extraction** — 201
libraries found, 195 extracted — so they never reach the registry scan at all. The
scan's three quiet returns (bad magic, truncated header, zero length) are
defence-in-depth here rather than the mechanism that handles these files. They
remain load-bearing for a file that passes the APK-layer check and fails later.

### One fix made during verification

`apk_native.rs:125` read "it has no entry **there** is a loadable object file".
Corrected to "**that** is". User-visible log text, surfaced by the Chrome run.

## Not done

**VLC 3.7.0 is the one package in `~/apps` that could not be measured**, for a disk
reason unrelated to this change.

Its tables are not where one would guess. `libvlcjni.so` (94 KB) imports fine and
yields **no sidecar at all** — zero tables — and neither does `libmla.so`. The
entries are inside `libvlc.so` (43 MB), which matches the plan's account that the
32 unattributed entries are libbluray's BD-J bindings, statically linked there.

Ghidra on `libvlc.so` produced **44 GB of facts** and was still growing at roughly
2–3 GB/min when a disk guard stopped it with 19 GB free; an earlier attempt through
the bundle path reached 41 GB. Two runs, neither finishing. The import never
completed, so no sidecar was written. Nothing about the registry scan is
implicated — this is the cost of disassembling VLC's core library, and it would
apply equally without this change. On a machine with ~150 GB free it is worth
retrying:

```bash
ctadl import --name vlccore <extracted>/lib/arm64-v8a/libvlc.so
```

**The behaviour VLC was the regression case for is verified elsewhere, on real
data.** The plan wanted "a library whose Java half is absent must attribute zero
entries and emit zero links". Six libraries do exactly that, unprompted:
TikTok Lite's `libTTMachineCore` (11 entries, 11 runs, 0 attributed) and TikTok
46.1.3's `libkryptonaudio`, `libkryptonaurum`, `libttmverifylite`,
`libvcnverifylite` and `libNLEEditorJni`. `libbdvideouploader` reproduces VLC's
exact shape — a partially-present Java half, 18 attributed and 32 not.

**The `JniRegister` end-to-end case has still not been run.** It needs the
regression flake's javac, dx, cross-gcc, addr2line and Ghidra. It is the only
regression case that reads the built library's own bytes, so it needs an ELF
target; the scan is a quiet no-op on the Mach-O a macOS worker would produce.

Everything the plan lists as out of scope stays out of scope: class attribution
from the binary side (following `FindClass` through `JNI_OnLoad`), `JNIEnv`
accessor calls, `luaL_Reg` tables, Superpack-compressed payloads, and Java halves
that ship outside `classes.dex`. Following a `B` veneer to its target was raised
as a candidate for that list by the first verification run; it was measured, and
then built instead.
