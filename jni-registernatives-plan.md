# Link natives bound through `RegisterNatives` - DO-NOT-MERGE

## Context

CTADL's JNI bridge links a Java `native` method to its implementation by mangled
symbol name: it looks for `Java_com_example_Foo_bar` in the co-indexed shared
library. That is the only convention it knows, and `ctadl-ascent/src/languages/jni.rs`
says so in its own header ("`RegisterNatives` is not handled").

Real Android apps mostly do not use that convention. They call
`env->RegisterNatives(clazz, table, n)` from `JNI_OnLoad`, passing a
`JNINativeMethod[]` — `{const char *name; const char *signature; void *fnPtr;}` —
and the implementations stay hidden, with no exported symbol. So the bridge finds
nothing to link:

| APK | native methods in Dex | `Java_` symbols in all libs | linked today |
| --- | --- | --- | --- |
| Facebook Lite 513 | 535 | 1 | 1 |
| Messenger 563 | (many) | 3 across 11 libs | ≤3 |

The tables are recoverable statically. A prototype scan of the same libraries
finds them without any dataflow analysis:

| Library | `Java_` symbols | table entries recovered |
| --- | --- | --- |
| libsuperpack-jni.so | 1 | 28 |
| libgraphics-core.so | 2 | 52 |
| libbreakpad.so | 0 | 23 |
| Messenger, all 11 libs | 3 | **118** |

Every recovered `fnPtr` in Facebook Lite's `libsuperpack-jni.so` (28 of 28)
already has a Ghidra function at that address in the existing import, so each one
has an IR function to link to.

Outcome: the bridge links dynamically registered implementations as well as
symbol-named ones, so taint crosses the JNI boundary in the apps people actually
analyze.

### Validated against the whole corpus

The design below was originally measured on those two packages. It has since been
re-run over **all 370 `.so` files in all 17 packages in `~/apps`**, with every
recovered entry cross-checked against the `native` methods actually declared in
each app's Dex — ground truth the first pass did not use. Per package, under the
ABI CTADL actually picks (`ABI_PREFERENCE`, `dex-reader/src/apk.rs:71`):

| Package | libs | w/ tables | entries | tier-1 attributed | `Java_` syms |
| --- | ---: | ---: | ---: | ---: | ---: |
| Facebook Lite 513 | 2 | 1 | 28 | 28 (100%) | 1 |
| Messenger 563 (arm64) | 11 | 7 | 118 | 117 (99%) | 3 |
| Messenger 570 (**armeabi-v7a**) | 13 | 7 | 119 | 118 (99%) | 10 |
| TikTok Lite 44 | 44 | 16 | 579 | 567 (98%) | 155 |
| TikTok 46.1.3 | 196 | 60 | 1818 | 1768 (97%) | 9793 |
| Telegram 12.8.3 / 12.9.0 | 2 | 1 | 83 | 83 (100%) | 381 |
| VLC 3.7.0 | 4 | 1 | 34 | 2 (6%) | 120 |
| WhatsApp Messenger 2.26.27 | 4 | 2 | 22 | 22 (100%) | 1 |
| WhatsApp Business 2.26.21 | 3 | 1 | 19 | 19 (100%) | 1 |
| Chrome 149 (armeabi-v7a) | 2 | 1 | 447 | 432 (97%) | 11 |
| FX File Explorer | 5 | 0 | 0 | — | 10 |

Three results settle the open risks:

- **Zero false positives.** Of 4280 entries recovered corpus-wide, every one
  checkable against the Dex matched a declared `native` method on name *and*
  full descriptor — 100% on nine of eleven packages. The two apparent misses are
  not scan errors (see step 4).
- **Tier-1 attribution works at scale**: 97–100% wherever the Java half is
  present, including 1818 entries across 60 libraries in one TikTok project.
- **A globally-unique-name guess tier earns nothing.** Corpus-wide, the number of
  tier-1-unattributed entries whose `(name, descriptor)` is globally unique is
  zero. That tier is cut; see step 4.

Two packaging defects, neither caused by this change, keep most of the corpus
from reaching the feature at all: `.xapk` bundles cannot be imported, and the ABI
preference picks an empty placeholder on Chrome. Both are fixed here — see
"Packaging fixes".

## Design

Four steps. Only the fourth touches the linking logic; the emit machinery
(`port_map`, `emit_bridge`) is reused unchanged.

### 1. Recover the tables from the ELF (import time)

New module `ctadl-ascent/src/languages/jni/registry.rs`, called from the pcode
importer. It needs no Ghidra and no dataflow.

The import's `artifact_path` is not always a file: gate the scan on
`GhidraSource::detect` (`pcode/ghidra.rs:57-70`) returning `Binary` — a
`ghidra://` server URL or a `.gpr` project must skip it — and treat any parse
failure as a quiet no-op.

That no-op is load-bearing, not just Mach-O/PE insurance: **17 of the corpus's
370 `.so` files are not ELF at all** (4.6%).

| File | magic | what it is |
| --- | --- | --- |
| `libdex_df_*.so` (TikTok) | `PK\x03\x04` | ZIP of dex, shipped under `lib/` |
| `libmedia.so` (TikTok) | `\x7fKOM` | custom packed container |
| `liblynxsuit2.so` (TikTok) | `SKCL` | custom packed container |
| `libplaceholder.so` (Chrome) | *(0 bytes)* | empty placeholder |

Treat bad magic, a truncated header, and a zero-length file as three separate
quiet returns.

Walk each writable, non-executable `PROGBITS` section (`.data.rel.ro`, `.data`)
at pointer stride and read three consecutive slots. A slot's value is:

- the addend of a relative dynamic relocation at that offset, if one exists
  (`R_AARCH64_RELATIVE`, `R_X86_64_RELATIVE`, `R_ARM_RELATIVE`,
  `R_386_RELATIVE`); otherwise
- the word stored in the file.

The fallback is what covers `.relr.dyn` and 32-bit `.rel.dyn`, both of which keep
the value in place — so **`RELR` needs no decoder**. Nine of Messenger 563's
eleven libraries use `.relr.dyn`, and the prototype recovered their tables
through this rule alone.

**Select relocation sections by `sh_type`, never by name.** A section *named*
`.rela.dyn` is not always an array of `Elf_Rela`; Android ships two packed
formats under the standard names, with non-standard type numbers:

| Section | `sh_type` | meaning | libs |
| --- | --- | --- | ---: |
| `.rela.dyn` | `4` (`SHT_RELA`) | standard | 263 |
| `.rela.dyn` | `0x60000002` | `SHT_ANDROID_RELA` — packed APS2 | 9 |
| `.relr.dyn` | `0x6FFFFF00` | `SHT_ANDROID_RELR` | 10 |
| `.rel.dyn` | `9` / `0x60000001` | 32-bit, standard / packed | 77 / 1 |

So take only `sh_type == SHT_RELA` and let everything else fall through to the
in-place read. Decoding an APS2 blob as `Elf_Rela` yields garbage addends — and
plausible-looking ones, which is worse. Both paths are load-bearing and each
needs a test: `libsuperpack-jni.so` resolves **every** slot through the reloc
path, while Messenger 563 uses a mix across its libraries.

A triple is a `JNINativeMethod` when all three hold:

- slot 0 points at a NUL-terminated string that is a valid Java method name
  (also allow `<init>`/`<clinit>`);
- slot 1 points at a string that parses as a method descriptor;
- slot 2 points into an executable section.

That test produced **no false positives across all 370 libraries**: every one of
the 4280 recovered entries that could be checked against a Dex matched a declared
`native` method on name and full descriptor. Handle ELF64 (stride 24) and ELF32
(stride 12); skip Mach-O and PE quietly.

Three details the scan gets wrong if left unstated:

- **Advance by entry size on a match, by one pointer otherwise.** Scanning at a
  flat pointer stride yields overlapping candidate triples over a real table.
  Misaligned triples are rejected on their own — a signature string starting `(`
  is not a valid method name — but the advance rule is what makes table
  *contiguity* meaningful, and contiguity is the entire basis of step 4.
- **Mask bit 0 of `fnPtr` on 32-bit ARM.** A Thumb function pointer carries the
  low bit set; unmasked it matches no function entry point. This is the common
  case, not a corner case: **1164 of 1269 32-bit ARM entries (92%) have bit 0
  set**, so without the mask 92% of armeabi-v7a entries map to nothing. Nor is
  ELF32 a secondary path — Messenger 570 and both WhatsApp Messenger builds ship
  `armeabi-v7a` only, and Chrome's sole real library is armeabi-v7a. Give the
  mask its own test and put a 32-bit package in the end-to-end list.
- **Validate the return type.** Reuse `jni::descriptor_params` (`jni.rs:151`,
  `pub`, `Option<Vec<&str>>`, `None` on garbage) rather than writing a second
  descriptor parser — but note it stops at `)` and never validates what follows,
  so `"(I)garbage"` passes. Check that the tail is one well-formed type
  descriptor or `V`. Keep this as cheap defence, but do not expect it to earn its
  keep: ablated across all 370 libraries, the strict and lax rules admit
  *identical* candidate sets (4280 either way). The real filtering is done by the
  executable-section test and the method-name test.

Parse with the `object` crate (`default-features = false`, features
`["read_core", "elf", "std"]`), added to `ctadl-ascent/Cargo.toml`. Version
0.37.3 is already in `Cargo.lock` transitively (via `psm` → `ar_archive_writer`),
so a direct dependency on `object = "0.37"` adds no new version to the tree.

### 2. Map each `fnPtr` to an IR function

Ghidra addresses are `image_base + ELF vaddr`. `import_pcode`
(`ctadl-ascent/src/languages/pcode/mod.rs:31`) already reads the image base via
`PcodeFactsReader::read_image_base` (lines 49-57), and `HFuncData::entry_point`
(`pcode-reader/src/lib.rs:115`, an `Option<PcodeAddress>`) carries every
function's entry address.

Have `Context::process_functions` (`pcode/mod.rs:305`) record
`entry_point -> fq_name` alongside the `self.functions` map it already builds at
line 415, then run the scan in `import_pcode` between `ctx.process`
(`mod.rs:69`) and `ctx.finish` (`mod.rs:70`) — at that point the image base is
known and the function map is fully populated. `fq_name`
(line 377) is exactly the string stored in the `NativeFunction` VMT column, which
is what `link` resolves to a `FunctionId` — so it is the right key to persist.

`query_engine/formatter.rs:2532` already uses `addr - image_base` for relative addresses, so
the identity holds. It assumes the first `PT_LOAD` has `p_vaddr == 0`, true for
Android shared libraries; read that value from the ELF and subtract it rather
than assuming, or at minimum log when it is nonzero. If `read_image_base`
returned `None`, skip the scan and log — do not emit unmapped rows.

**An entry whose address has no function keeps its row, with `function: null`.**
It is counted and logged, not dropped: dropping one breaks the address
contiguity step 4 depends on.

### 3. Persist as a sidecar

Write `jni-registry.json` into the import directory, beside `ir-vmt.bitcode`.
Add `JNI_REGISTRY_FILE` and an `ArtifactImport::jni_registry_path()` next to the
existing `PROGRAM_BITCODE_FILE`/`VMT_BITCODE_FILE` accessors
(`ctadl-ascent/src/project.rs:102-106,317-323`).

Rows carry **two** addresses:

```json
{ "table_addr": 172032, "fn_addr": 68640, "name": "readBytesNative",
  "descriptor": "(JII[BI)V", "function": "readBytesNative" }
```

sorted by `table_addr`. The distinction is load-bearing, not cosmetic: step 4
segments by *table* order, and functions in `.text` are not laid out in table
order, so ordering by `fn_addr` would scramble the runs the whole attribution
rests on. `fn_addr` is kept because it is what a human spot-checks against the
ELF.

A sidecar rather than a new VMT column, so **`IMPORT_FORMAT_VERSION` stays `"5"`**
and every existing import keeps loading. An import without the file contributes
nothing. Follow the `ArtifactImport` conventions (`project.rs:162-202`):
`#[serde(default)]` on every field, so a later addition does not break old stores.

Teach the `inspect` path to print it. That dispatch is **two-sited**:
`main.rs:800-804` gates which filenames reach `cli::inspect_bitcode`, and
`cli/mod.rs:1258,1264` dispatches inside it. Both need a branch — an
unrecognized filename currently falls through to `ArtifactImport::load_by_name`
at `main.rs:808`. Since the sidecar is JSON, not bitcode, the `main.rs` branch
can call a small dedicated print function rather than routing through
`inspect_bitcode`.

**Caveat to document:** `ctadl import --skip-existing` reuses an unchanged
library's import directory (`apk_native.rs:270`) and so will not generate a
sidecar for a library imported before this change. "Re-import to gain it" is only
true without `--skip-existing`.

### 4. Attribute a class, then link (index time)

A table entry carries a name, a descriptor and a function — but **not the class**.
The class lives in the `FindClass` call that precedes `RegisterNatives`.
Recover it from the Dex side instead.

The **table** side of attribution runs **per import**, since `table_addr` order is
only meaningful within one library. The **Java candidate** side is
project-global: build the candidate index from the same deduplicated `natives`
list `link` already builds (`jni.rs:370-375`), which spans every import.
`JavaNative` carries `class_internal`, `simple_name` and `descriptor`, so no new
observation data is needed.

That asymmetry is not incidental — it is what makes split APKs work at all. In an
app bundle the `.so` and the `classes.dex` live in *different* imports, so an
attribution scoped to a single import on both sides would link nothing.

**Tier 1 — contiguous runs (the only tier).** Walk the `table_addr`-ordered entries,
keeping the set of Java classes that declare *every* entry in the current run
(matched on name and full descriptor). When that set goes empty, close the run
and start a new one at the current entry. A closed run whose set is a single
class attributes all its entries to that class.

This works because a `JNINativeMethod[]` is contiguous and one class's table
starts where the previous one ends. Verified exact on `libsuperpack-jni.so`,
where three tables sit adjacent with no gap between them:

```
entries  1-13  -> Lcom/facebook/superpack/SuperpackArchive;
entries 14-19  -> Lcom/facebook/superpack/SuperpackFile;
entries 20-28  -> Lcom/facebook/superpack/AssetDecompressor;
```

Note the runs are *subsets*, not equalities: `SuperpackArchive` declares 14
natives and registers 13. Match by containment, never by count.

The greedy rule alone can **silently merge two adjacent tables**: if table B's
leading entries happen to also be declared by table A's class, the set never
empties and B's entries are attributed to A — a fabricated link, worse than a
miss. Three cheap, exact guards close it:

1. **Split at address gaps.** `table_addr` must equal the previous entry's plus
   the entry size; a gap is a table boundary with certainty. (This alone does not
   separate *adjacent* tables — hence the next two.)
2. **Split on a repeated `(name, descriptor)` within a run.** A
   `JNINativeMethod[]` cannot register the same method twice, so a repeat is
   proof of a boundary.
3. **Cap a run at the number of natives its class declares.** A run attributed to
   A longer than A's native count is impossible; treat it as unattributed and log.

Guard 1 alone is provably insufficient, and `libsuperpack-jni.so` is the proof:
splitting on address gaps alone yields **2** runs for its **3** adjacent tables.
Adding guards 2 and 3 yields exactly **3**, matching the ground truth above. That
is a ready-made unit test and direct evidence all three are needed.

**No guess tier.** An earlier draft proposed a tier 2 that linked an
unattributed entry when exactly one still-unlinked Java native in the project had
that name and descriptor, gated behind `--jni-registry-guess`. It is cut.
Measured across all eleven packages, the number of tier-1-unattributed entries
whose `(name, descriptor)` is globally unique is **zero** — every entry tier 1
fails to attribute either matches no Dex native at all or matches several
classes, and a uniqueness rule rescues neither. It was also the only tier with no
positional evidence, the only one that could invent a cross-class taint path, and
it carried flag surface, a `LinkStats` counter and warning logic. Anything tier 1
does not attribute is counted unattributed and not linked. Record the measurement
in `docs/jni.md` so the idea is not re-proposed without new evidence.

**Attributing nothing is the right answer when the Java half is absent.** VLC
looks like a failure (2 of 34 entries attributed) and is not: its other 32 are
libbluray's BD-J bindings — `getTitleInfosN(J)[Lorg/videolan/TitleInfo;`,
`getBdjoN(…)Lorg/videolan/bdjo/Bdjo;` — well-formed tables whose Java classes
ship outside `classes.dex`. TikTok's 11 unmatched entries are the same shape
(`com/tiktok/ttm/*`, in a feature-split dex). In both the candidate set empties on
every entry, each entry closes its own run, and nothing is misattributed. Make it
a regression test: a library whose Java half is absent must attribute zero
entries and emit zero links.

**Wiring.** `JniObserver` (`jni.rs:250`) gains the registry rows: the index loop
at `cli/mod.rs:201` has the `ArtifactImport` in hand, so add a
`jni_observer.observe_registry(&import)?` call beside the existing `observe`.
That call site sits inside `if !no_jni_bridge` (`cli/mod.rs:200`), so
`--no-jni-bridge` disables the registry too — intended; `--no-jni-registry`
gates separately inside it. The existing `natives`/`symbols` fields are flat
with no import provenance, and the table side of attribution is per-import, so
the registry rows must carry their own import key. `JniObserver::is_empty` (`jni.rs:298-300`)
treats an empty `symbols` as empty; that becomes wrong once registry rows can
link on their own — fix it (it has no callers in the binary today).

`resolve` (`jni.rs:477`) gains a **tier 0** consulting the attribution result.
Tier 0 must run **before** the long-name attempt at `jni.rs:493`, not as a
fallback: the `Ambiguous` arm `continue`s (`jni.rs:403`) before touching
`source_info`, so a fallback-only registry would never rescue overloaded
natives — the case `RegisterNatives` matters most for.

Where a method resolves both ways, **prefer the registration** — that is what the
runtime does — and log when the two disagree. `resolve` must still return exactly
one answer, so that `emit_bridge` (`jni.rs:520`, which mints a *fresh* site per
call) cannot double-bridge such a method.

`Resolution::Found { function: &'a str }` (`jni.rs:332-336`) borrows from
`symbols`; the new tier's strings live in the registry structure, so either pass
it with the same `'a` or widen the field to `Cow<'a, str>` (the consumer at
`jni.rs:420-423` does `function.into()`, which needs `.as_ref()` under `Cow`).
The variant also carries `symbol: String`, printed by the debug log at
`jni.rs:391` — the registry tier must supply one (the entry's name, or a
`registry:` marker).

The ambiguity warning at `jni.rs:395-397` needs updating too: "give the
implementation its long name" is no longer the only remedy.

## Reporting and flags

Extend `LinkStats` (`jni.rs:310`) and its `Display`. Two constraints: the
struct is `Copy`, and `link`'s return value is **discarded** at
`cli/mod.rs:277` — all reporting today happens via `log::info!` inside `link`.
So keep `LinkStats` to flat counters (add `registered` and `unattributed`) and
log the per-library registry line at attribution time rather than storing
per-library strings in the struct:

```
jni registry: 3 table(s), 28 entr(ies) in libsuperpack-jni: 28 attributed to 3 class(es), 0 unattributed
jni bridge: 535 native method(s): 29 linked (28 registered), 506 unresolved, 0 ambiguous
```

**Log only libraries that have tables.** TikTok's config split holds 201
libraries and 60 have tables; a per-library `info` line for the other 141 is
noise.

Introduce **`cli::IndexOptions`** — following the existing `cli::ImportOptions`
(defined at `cli/mod.rs:41`, constructed at `main.rs:691`) — holding
`no_jni_bridge`, `no_jni_registry`, `strategy`,
`prune_unreachable_cfg_nodes`, `alias_rule` and `dump_index_graph`. `cli::index`
(`cli/mod.rs:152`) currently takes nine positional arguments under
`#[allow(clippy::too_many_arguments)]`. It has **five** call sites, not one:
`main.rs:723` plus four integration tests (`tests/sarif_uris.rs:81`,
`tests/bridging_end_to_end.rs:112`, `tests/port_semantics.rs:126`,
`tests/multi_import_sarif.rs:109`). The refactor still retires the allow, but
touches those four test files too.

Add `--no-jni-registry` to `IndexArgs` (`main.rs:219`) and to the one-shot
`GoArgs` struct (`main.rs:311`), forwarded at `main.rs:464`. A **second**
`IndexArgs` literal at `main.rs:602-613` (legacy path, which hardcodes
`no_jni_bridge: false` at `:608`) needs a value for the new field as well.
`--no-jni-registry` ignores the sidecar rows, which gives a clean A/B measurement
of what this change contributes without re-importing. Scanning stays
unconditional at import time — it costs milliseconds. (The pure-Python prototype
scanned all 370 libraries in 15 seconds, dominated by TikTok's 196.)

## Packaging fixes

Neither defect below is caused by this change, but each is the difference between
the feature reaching an app and contributing nothing to it.

### `.xapk` bundles

**Nine of the seventeen packages in `~/apps` are `.xapk`, and CTADL cannot import
them** — Chrome, TikTok 46.1.3, VLC, Telegram ×2, WhatsApp ×3. There is no
`.xapk` arm in the extension table (`main.rs:849-863`), so a bundle falls through
to `file_looks_binary`, which sees NULs in the ZIP and routes the whole thing to
the **Ghidra/pcode frontend** — a slow, confusing failure rather than an error.

An `.xapk` is a ZIP of split APKs: Dex in the base (`com.android.chrome.apk`),
native libraries in `config.<abi>.apk`. CTADL already handles that shape
correctly once unzipped — `cli/mod.rs:92-110` imports a DEX-less native-only
split, and co-indexing joins the halves.

Add `ImportLanguage::Xapk` (`main.rs:180`), `ArtifactLanguage::Xapk`
(`project.rs:819`, plus its `all()`/`name()` lists), and
`Some("xapk") => ImportLanguage::Xapk` in the extension table. New module
`ctadl-ascent/src/languages/xapk.rs`, dispatched from a new arm in `cli::import`
(`cli/mod.rs:74`), which:

1. enumerates top-level `*.apk` entries and extracts each to
   `<import-dir>/splits/<stem>.apk`, reusing the containment discipline at
   `apk_native.rs:210-212` (build the destination from the stem, never the raw
   ZIP entry name);
2. imports each split through the **existing `Apk` path**, named
   `<parent>__<split-stem>` following `apk_native.rs:292-304`;
3. orders dex-bearing splits first, so `cli::index`'s per-import source-span
   scoping stays in import order and the Java half is observed before the native
   half;
4. forwards `ImportOptions` (`--native-abi`, `--skip-existing`,
   `--no-native-libs`) unchanged to each split.

Two constraints that are easy to get wrong:

- **Flatten the sub-import list.** `AnalysisProject::ephemeral`
  (`project.rs:528-545`) expands exactly one level —
  `std::iter::once(name).chain(subs)`, no recursion. So the bundle's
  `sub_imports` must be, for each split, *its own name followed by its own
  `sub_imports`*. Nesting bundle → split → native library without flattening
  silently drops every `.so` at index time, which would look exactly like the bug
  this change exists to fix.
- **Skip resource-only splits; do not fail on them.** They are the majority:
  TikTok 46.1.3 has **23 of 30**, and every other bundle has one or two
  (`config.en.apk`, `config.xxhdpi.apk`). `apk_native::require_native_libs`
  raises `Error::NothingToImport` for a split with neither Dex nor `lib/`
  (`apk_native.rs:65-73`); the bundle importer must catch exactly that, log at
  debug, and continue. Anything else propagates.

The bundle import contributes no program of its own — an empty `ProgramInfo`, the
same shape as a native-only split.

### The Chrome ABI trap

Chrome ships `lib/arm64-v8a/libplaceholder.so` at **0 bytes** and its real code as
`lib/armeabi-v7a/libelements.so`. `preferred_abi` (`dex-reader/src/apk.rs:128-137`)
picks `arm64-v8a`, `looks_like_object_file` (`apk.rs:182`) rejects the empty
file, and the import yields **zero** native libraries — against **447
recoverable entries** under armeabi-v7a.

Fix `preferred_abi` to skip an ABI whose entries all fail `looks_like_object_file`
(which already covers the zero-length case), falling through to the next in
`ABI_PREFERENCE`. Keep the existing "ignoring …; pass `--native-abi`" log
(`apk_native.rs:153-160`) and add the reason when an ABI is skipped this way, so
the choice is never silent. An explicit `--native-abi` must still be honored as
today, including when it names an unusable ABI — `apk_native.rs:109-115`
deliberately reports that rather than falling back.

## Files

| File | Change |
| --- | --- |
| `ctadl-ascent/src/languages/jni/registry.rs` | new: ELF scan, sidecar read/write, run attribution |
| `ctadl-ascent/src/languages/jni.rs` | `mod registry`; observer field; tier 0 in `resolve`; stats; header at :35 |
| `ctadl-ascent/src/languages/pcode/mod.rs` | entry-point map in `process_functions`; call the scan from `import_pcode` |
| `ctadl-ascent/src/languages/xapk.rs` | new: unwrap bundle, import each split, flatten sub-imports, skip resource-only splits |
| `ctadl-ascent/src/project.rs` | `JNI_REGISTRY_FILE`, `jni_registry_path()`; `ArtifactLanguage::Xapk` at :819 |
| `ctadl-ascent/src/cli/mod.rs` | `IndexOptions`; `observe_registry` call; `inspect_bitcode` branch; `Xapk` arm in `import` at :74 |
| `ctadl-ascent/src/main.rs` | `--no-jni-registry` (both `IndexArgs` literals); `IndexOptions` construction; `inspect` filename gate at :800; `ImportLanguage::Xapk` at :180 and the extension table at :849 |
| `dex-reader/src/apk.rs` | `preferred_abi` skips an ABI with no usable object files (:128-137) |
| `ctadl-ascent/tests/{sarif_uris,bridging_end_to_end,port_semantics,multi_import_sarif}.rs` | update `cli::index` call sites to `IndexOptions` |
| `ctadl-ascent/Cargo.toml` | `object` dependency |
| `docs/jni.md` | rewrite the limitation at :260 and the See-also at :296; document the `--skip-existing` caveat, the `.xapk` workflow, the Chrome ABI note, and the measured guess-tier result |
| `docs/model-generators.md` | update the `RegisterNatives` mention at :605 |

## Verification

Use the nix `regression` devShell environment to add these tests.

**Unit tests** — `ctadl-ascent/src/languages/jni/registry/tests.rs`.

Attribution carries the real risk, and it needs no ELF: drive the run
segmentation from synthetic `(table_addr, name, descriptor)` lists. Cover the
Facebook Lite shape (three adjacent tables, subset runs — asserting **3** runs,
not 2, which is the direct regression for guards 2 and 3), a run that stays
multi-class, a gapped table, a library whose entries match no declared native at
all (the VLC shape: zero attributions, zero links), and — for the guards
above — **two adjacent tables whose classes share a leading method**, asserting
the run splits rather than misattributing.

For parsing, build minimal ELF byte buffers in-test: ELF64 with a standard
`SHT_RELA` `.rela.dyn`, ELF64 with the value in place (the `.relr.dyn` path),
ELF32 at stride 12, a Thumb `fnPtr` with bit 0 set, a `.rela.dyn` typed
`SHT_ANDROID_RELA` (`0x60000002`) asserting it is *ignored* in favour of the
in-place value, and — for the quiet no-op — a zero-length file and one starting
`PK\x03\x04`.

For `.xapk`, a fixture bundle holding a dex-bearing split, a native-only split
and a resource-only split, asserting the resource-only split is skipped and that
`AnalysisProject::ephemeral` on the bundle name yields the native library
imports. That last assertion is the flattening regression.

**End-to-end** — `nightly/tests/jni/JniRegister.{java,c}` plus
`jni-register.json`, following `JniFlow` and `JniArgShift`.
`xtask/src/discovery.rs:262` picks the case up from the file names alone,
registering three variants (`Jni:JniRegister`, `+apk`, `+split-apks`); a fourth
(`+bridge`) appears only if a `jni-register.bridge.jsonl` ships. All variants
go through `import_pcode`, so all get a sidecar. Declare a native, register
it from `JNI_OnLoad` with no `JNIEXPORT`, and assert taint flows through it.

Four shape constraints, or the case fails for reasons unrelated to the feature:

- **Give the table external linkage** (file-scope, not `static const`). The
  runner compiles with `-g -O0 -shared -fPIC`
  (`xtask/src/regression.rs:1457-1469`), and an unreferenced `static` array can
  still be dropped. Declare `JNINativeMethod` locally alongside the JNI typedefs
  the existing cases already declare — the regression flake ships no NDK. Nothing
  has to actually call `RegisterNatives`; the table only has to exist in
  `.data.rel.ro`.
- **Give the implementations external linkage but non-`Java_` names.** A `static`
  function reachable only through a data pointer may never become a Ghidra
  function, which would fail step 2 rather than test it. An exported `stash_impl`
  is found by Ghidra, matches no mangled name, and so exercises the registry path
  honestly.
- **Name the library to match `System.loadLibrary`.** The runner builds
  `lib<lowercased-class>.so` (`xtask/src/regression.rs:1461`), so the Java half
  must call `System.loadLibrary("jniregister")`.
- **Give `expected_native_lines` a call site to land on.** Taint on a native
  line is only assertable at a call site; both existing cases use a trivial
  `static jstring keep(jstring s) { return s; }` helper for exactly this.

Do **not** add `"JniRegister"` to the stem arrays in the discovery unit test
(`discovery.rs:403` and `:424`, not `:392`) unless the case ships a
`.bridge.jsonl` — line 409 asserts a `+bridge` variant exists for every stem
listed, so adding it without the bridge file fails the test. The suite itself
needs `nix develop .#regression` (javac, dx, cross-gcc, addr2line, Ghidra).

**Against the real APKs** — the reason for the change. Each line below pins a
specific claim; expected counts come from the corpus table in Context.

```bash
cargo build --release
R=./target/release/ctadl

# 64-bit, reloc path        expect ~29 linked (28 registered), 28 attributed to 3 classes
$R import --name fblite ~/apps/Facebook+Lite_513.0.0.6.105_APKPure.apk && $R index fblite

# 64-bit, mixed paths       expect 118 entries across 7 libs, ~117 attributed
$R import --name m563 ~/apps/Messenger_563.0.0.47.86_APKPure.apk && $R index m563

# 32-bit: Thumb mask        expect 119 entries, ~118 attributed. Fails without the mask.
$R import --name m570 ~/apps/Messenger_570.0.0.34.87_APKPure.apk && $R index m570

# scale                     expect 579 entries across 16 libs, ~567 attributed
$R import --name ttlite '~/apps/TikTok+Lite+-+Save+Data+%26+Fast_44.0.3_APKPure.apk' && $R index ttlite

# xapk + flattening         expect 22 entries; 2 resource-only splits skipped
$R import --name wa ~/apps/WhatsApp+Messenger_2.26.27.85_APKPure.xapk && $R index wa

# xapk + ABI fix            expect 447 entries. Yields 0 without the preferred_abi fix.
$R import --name chrome ~/apps/Google+Chrome_149.0.7827.160_APKPure.xapk && $R index chrome

# xapk at scale             expect ~1818 entries across 60 libs; 23 of 30 splits skipped
$R import --name tt '~/apps/TikTok+-+Videos%2C+Shop+%26+LIVE_46.1.3_APKPure.xapk' && $R index tt

# negative control          FX uses the Java_ convention only; counts must not move
$R import --name fx ~/apps/FX+File+Explorer_9.1.0.8_APKPure.apk && $R index fx
```

For Facebook Lite expect roughly `29 linked (28 registered)` where the count is 1
today, and every one of `libsuperpack-jni.so`'s 28 entries attributed to
`SuperpackArchive`, `SuperpackFile` or `AssetDecompressor`.
`RUST_LOG=ctadl_ascent::languages::jni=debug` prints each pairing; spot-check a
few against the table, for example `SuperpackFile;->readBytesNative(JII[BI)V` ->
`fn_addr` `0x10c20`. Inspect the raw rows with
`ctadl inspect <import-dir>/jni-registry.json`.

Re-import is required throughout: the sidecar is written at import time, and
`--skip-existing` will not create one.

Finally re-run with `--no-jni-registry` and confirm every count falls back to
today's — the clean A/B, and the check that nothing regressed for apps like FX
that never used `RegisterNatives`.

## Out of scope

- **Class attribution from the binary side.** Following `FindClass` through the
  decompiled `JNI_OnLoad` would attribute the leftovers, and would need the
  `JNIEnv` vtable offsets. Tier 1 covers 97–100% of entries wherever the Java
  half is present, so this buys little; revisit if unattributed counts turn out
  high on some corpus unlike this one.
- **`JNIEnv` accessor calls** (`GetStringUTFChars` and friends) — still unmodelled,
  still the next real limit on how far taint travels inside native code.
- **`luaL_Reg` tables**, which the same section-scanning shape would fit.
- **Superpack-compressed payloads.** Facebook Lite ships two `.so` files; the rest
  of its native code is packed in `assets/`. Unpacking it is a separate problem,
  and no amount of JNI linking reaches code that is not in the APK. TikTok's
  `\x7fKOM` and `SKCL` containers are the same problem in a different wrapper.
- **Java halves outside `classes.dex`** — VLC's BD-J stack, TikTok's
  feature-split dex. These are recovered on the native side and correctly left
  unattributed; linking them needs those Java classes imported, which is a
  packaging question rather than a bridge question.
- **Multi-ABI double-linking.** CTADL imports exactly one ABI
  (`apk_native.rs:116-119`), so a native cannot arrive twice from two ABIs; no
  defensive code is needed. The one reachable path is a user importing two ABI
  splits by hand, and `resolve` must already return exactly one answer so
  `emit_bridge` (`jni.rs:520`) cannot mint two sites.
