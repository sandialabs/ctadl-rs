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

## Design

Four steps. Only the fourth touches the linking logic; the emit machinery
(`port_map`, `emit_bridge`) is reused unchanged.

### 1. Recover the tables from the ELF (import time)

New module `ctadl-ascent/src/languages/jni/registry.rs`, called from the pcode
importer. It needs no Ghidra and no dataflow.

Walk each writable, non-executable `PROGBITS` section (`.data.rel.ro`, `.data`)
at pointer stride and read three consecutive slots. A slot's value is:

- the addend of a relative dynamic relocation at that offset, if one exists
  (`.rela.dyn`, `.rela.plt` — `R_AARCH64_RELATIVE`, `R_X86_64_RELATIVE`,
  `R_ARM_RELATIVE`, `R_386_RELATIVE`); otherwise
- the word stored in the file.

The fallback is what covers `.relr.dyn` and 32-bit `.rel.dyn`, both of which keep
the value in place — so **`RELR` needs no decoder**. Nine of Messenger's eleven
libraries use `.relr.dyn`, and the prototype recovered their tables through this
rule alone.

A triple is a `JNINativeMethod` when all three hold:

- slot 0 points at a NUL-terminated string that is a valid Java method name
  (also allow `<init>`/`<clinit>`);
- slot 1 points at a string that parses as a method descriptor;
- slot 2 points into an executable section.

That test produced no false positives across the 13 libraries tried. Handle
ELF64 (stride 24) and ELF32 (stride 12); skip Mach-O and PE quietly.

Three details the scan gets wrong if left unstated:

- **Advance by entry size on a match, by one pointer otherwise.** Scanning at a
  flat pointer stride yields overlapping candidate triples over a real table.
  Misaligned triples are rejected on their own — a signature string starting `(`
  is not a valid method name — but the advance rule is what makes table
  *contiguity* meaningful, and contiguity is the entire basis of step 4.
- **Validate the return type.** Reuse `jni::descriptor_params` (`jni.rs:145`,
  `pub`, `Option<Vec<&str>>`, `None` on garbage) rather than writing a second
  descriptor parser — but note it stops at `)` and never validates what follows,
  so `"(I)garbage"` passes. A few lines checking the tail is one well-formed type
  descriptor or `V` cost nothing and materially strengthen the only filter doing
  real work.
- **Mask bit 0 of `fnPtr` on 32-bit ARM.** A Thumb function pointer carries the
  low bit set; unmasked it matches no function entry point. armeabi-v7a is common
  enough that this is not hypothetical.

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
line 415, then run the scan in `import_pcode` after `ctx.process`. `fq_name`
(line 377) is exactly the string stored in the `NativeFunction` VMT column, which
is what `link` resolves to a `FunctionId` — so it is the right key to persist.

`formatter.rs:2532` already uses `addr - image_base` for relative addresses, so
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
`main.rs:799-801` gates which filenames reach `cli::inspect_bitcode`, and
`cli/mod.rs:1256,1262` dispatches inside it. Both need a branch.

**Caveat to document:** `ctadl import --skip-existing` reuses an unchanged
library's import directory (`apk_native.rs:270`) and so will not generate a
sidecar for a library imported before this change. "Re-import to gain it" is only
true without `--skip-existing`.

### 4. Attribute a class, then link (index time)

A table entry carries a name, a descriptor and a function — but **not the class**.
The class lives in the `FindClass` call that precedes `RegisterNatives`.
Recover it from the Dex side instead, in two tiers.

Attribution runs **per import**, since `table_addr` order is only meaningful
within one library. Build the candidate index from the same deduplicated
`natives` list `link` already builds (`jni.rs:370-375`); `JavaNative` carries
`class_internal`, `simple_name` and `descriptor`, so no new observation data is
needed.

**Tier 1 — contiguous runs (always on).** Walk the `table_addr`-ordered entries,
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

**Tier 2 — globally unique name and descriptor (opt-in, off by default).** For an
entry no run attributed, link it if exactly one still-unlinked Java native method
in the whole project has that name and descriptor. This is the only tier with no
positional evidence, and a wrong guess invents a cross-class taint path — so gate
it behind `--jni-registry-guess`, count it separately in `LinkStats`, and log a
warning naming both sides. Anything left over is counted unattributed and not
linked.

**Wiring.** `JniObserver` (`jni.rs:249`) gains the registry rows: the index loop
at `cli/mod.rs:201` has the `ArtifactImport` in hand, so add a
`jni_observer.observe_registry(&import)?` call beside the existing `observe`.
`resolve` (`jni.rs:477`) gains a **tier 0** consulting the attribution result.

Where a method resolves both ways, **prefer the registration** — that is what the
runtime does — and log when the two disagree. `resolve` must still return exactly
one answer, so that `emit_bridge` (`jni.rs:520`, which mints a *fresh* site per
call) cannot double-bridge such a method.

`Resolution::Found { function: &'a str }` (`jni.rs:333`) borrows from `symbols`;
the new tier's strings live in the registry structure, so either pass it with the
same `'a` or widen the field to `Cow<'a, str>`.

The ambiguity warning at `jni.rs:398` needs updating too: "give the
implementation its long name" is no longer the only remedy.

## Reporting and flags

Extend `LinkStats` (`jni.rs:309`) and its `Display`:

```
jni registry: 3 table(s), 28 entr(ies) in libsuperpack-jni: 28 attributed to 3 class(es), 0 unattributed
jni bridge: 535 native method(s): 29 linked (28 registered, 0 guessed), 506 unresolved, 0 ambiguous
```

Introduce **`cli::IndexOptions`** — following the existing `cli::ImportOptions`
(`main.rs:691`) — holding `no_jni_bridge`, `no_jni_registry`,
`jni_registry_guess`, `strategy`, `prune_unreachable_cfg_nodes`, `alias_rule` and
`dump_index_graph`. `cli::index` (`cli/mod.rs:152`) currently takes nine
positional arguments under `#[allow(clippy::too_many_arguments)]` and has exactly
**one** call site, `main.rs:723`, so the refactor is contained and retires the
allow.

Add `--no-jni-registry` and `--jni-registry-guess` to `IndexArgs`
(`main.rs:249`) and to the one-shot args struct (`main.rs:327`), forwarded at
`main.rs:464`. `--no-jni-registry` ignores the sidecar rows, which gives a clean
A/B measurement of what this change contributes without re-importing. Scanning
stays unconditional at import time — it costs milliseconds.

## Files

| File | Change |
| --- | --- |
| `ctadl-ascent/src/languages/jni/registry.rs` | new: ELF scan, sidecar read/write, run attribution |
| `ctadl-ascent/src/languages/jni.rs` | `mod registry`; observer field; tier 0 in `resolve`; stats; header at :35 |
| `ctadl-ascent/src/languages/pcode/mod.rs` | entry-point map in `process_functions`; call the scan from `import_pcode` |
| `ctadl-ascent/src/project.rs` | `JNI_REGISTRY_FILE`, `jni_registry_path()` |
| `ctadl-ascent/src/cli/mod.rs` | `IndexOptions`; `observe_registry` call; `inspect_bitcode` branch |
| `ctadl-ascent/src/main.rs` | two new flags; `IndexOptions` construction; `inspect` filename gate at :799 |
| `ctadl-ascent/Cargo.toml` | `object` dependency |
| `docs/jni.md` | rewrite the limitation at :260 and the See-also at :296; document the `--skip-existing` caveat |
| `docs/model-generators.md` | update the `RegisterNatives` mention at :605 |

## Verification

**Unit tests** — `ctadl-ascent/src/languages/jni/registry/tests.rs`.

Attribution carries the real risk, and it needs no ELF: drive the run
segmentation from synthetic `(table_addr, name, descriptor)` lists. Cover the
Facebook Lite shape (three adjacent tables, subset runs), a run that stays
multi-class, a gapped table, the tier-2 unique fallback, and — for the guards
above — **two adjacent tables whose classes share a leading method**, asserting
the run splits rather than misattributing.

For parsing, build minimal ELF byte buffers in-test: ELF64 with `.rela.dyn`,
ELF64 with the value in place (the `.relr.dyn` path), ELF32 at stride 12, and a
Thumb `fnPtr` with bit 0 set.

**End-to-end** — `nightly/tests/jni/JniRegister.{java,c}` plus
`jni-register.json`, following `JniFlow` and `JniArgShift`.
`xtask/src/discovery.rs:262` picks the case up from the file names alone,
registering three variants (`Jni:JniRegister`, `+apk`, `+split-apks`); all three
go through `import_pcode`, so all three get a sidecar. Declare a native, register
it from `JNI_OnLoad` with no `JNIEXPORT`, and assert taint flows through it.

Two shape constraints, or the case fails for reasons unrelated to the feature:

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

Add `"JniRegister"` to the hardcoded array in the discovery unit test at
`discovery.rs:392` so the case is covered without a toolchain. The suite itself
needs `nix develop .#regression` (javac, dx, cross-gcc, addr2line, Ghidra).

**Against the real APKs** — the reason for the change:

```bash
cargo build --release
./target/release/ctadl import --name fblite ~/apps/Facebook+Lite_513.0.0.6.105_APKPure.apk
./target/release/ctadl index fblite
```

Expect roughly `29 linked (28 registered)` where the count is 1 today, and every
one of `libsuperpack-jni.so`'s 28 entries attributed to `SuperpackArchive`,
`SuperpackFile` or `AssetDecompressor`. `RUST_LOG=ctadl_ascent::languages::jni=debug`
prints each pairing; spot-check a few against the table, for example
`SuperpackFile;->readBytesNative(JII[BI)V` -> `fn_addr` `0x10c20`. Inspect the raw
rows with `ctadl inspect <import-dir>/jni-registry.json`.

Then Messenger (`m563`), where the prototype found 118 entries across 11
libraries against 3 exported symbols, nine of them on the `.relr.dyn` path.
Re-import is required for both: the sidecar is written at import time, and
`--skip-existing` will not create one.

Finally re-run with `--no-jni-registry` and confirm the counts fall back to
today's, then with `--jni-registry-guess` to measure what tier 2 would add before
deciding whether it ever earns being on by default.

## Out of scope

- **Class attribution from the binary side.** Following `FindClass` through the
  decompiled `JNI_OnLoad` would attribute the leftovers tier 2 has to guess at,
  and would need the `JNIEnv` vtable offsets. The Dex-side tiers cover the cases
  measured here; revisit if unattributed counts turn out high.
- **`JNIEnv` accessor calls** (`GetStringUTFChars` and friends) — still unmodelled,
  still the next real limit on how far taint travels inside native code.
- **`luaL_Reg` tables**, which the same section-scanning shape would fit.
- **Superpack-compressed payloads.** Facebook Lite ships two `.so` files; the rest
  of its native code is packed in `assets/`. Unpacking it is a separate problem,
  and no amount of JNI linking reaches code that is not in the APK.
