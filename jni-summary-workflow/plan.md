# Plan: JNI bridging against native summaries - DO-NOT-MERGE

Implements `spec.md`. One PR on `jni-native` with one commit per step. Each commit builds and
passes `cargo test`. Run `cargo xtask regression --frontend jni` at steps 5, 8 and 10. Capture
all output to files (`/Volumes/Shampoo/ct-jni-native/<step>/`).

## Decisions

- **32-bit fixture**: add i686 gcc through nix (`pkgsCross.gnu32`). The fixture is x86, not
  armv7. It tests `SplitWide` but **cannot** settle spec concern 1 (AAPCS even/odd pairing),
  because cdecl has no padding slot. The manual NewPipe check (step 11) settles concern 1. If it
  is inconclusive, `SplitWide` ships without a padding slot and the gap is documented.
- **Workflow regression**: add a new `Packaging::SummaryApk` variant in xtask. Every existing
  `nightly/tests/jni` case also runs through it.
- **Real apps** (Facebook Lite, NewPipe): checked by hand in this session, not committed.

## Steps

### 1. `--no-native-libs` (R1, R2)
- `ctadl-import/src/project.rs`: add `pub enum SubImports { All, NoNativeLibs }`.
  `ephemeral`/`try_create` take it. The filter drops expanded `Pcode` sub-imports, keeps
  explicitly named imports, and passes through sub-imports whose config fails to load.
- `ctadl-ascent/src/main.rs`: add `IndexArgs.no_native_libs` with its doc comment. Set it to
  `false` in the literals at `:661` and `:812`. `index_artifacts_to_store` maps the flag, and
  other callers pass `All`.
- Tests (`ctadl-import` unit): APK → only the parent remains. XAPK → the splits remain and the
  libs are dropped. A named `Pcode` import is kept.

### 2. `load_vmt` (R3)
- `ctadl-import/src/store.rs`: add `load_vmt(&ArtifactImport)`, which calls `refuse_unfinished`
  and decodes `vmt_path()` only. `load_import` calls it.
- Test: `load_vmt(i) == load_import(i, Skip).vmt` on a Lua fixture.

### 3. `IndexConfig.import_hashes` (R9 prerequisite)
- `ctadl-import/src/project.rs`: add `#[serde(default)] import_hashes: BTreeMap<String,String>`.
  `write_index_config` fills it. `INDEX_FORMAT_VERSION` does not change.
- Test: an old `IndexConfig` JSON without the field still deserializes.

### 4. Observer split and `NativeTarget` (R6 groundwork, no behaviour change)
- `frontends/ctadl-pcode/src/jni_registry.rs`: expose the ELF `(e_machine, class)` helper used
  near `:389`.
- `ctadl-ascent/src/languages/jni.rs`:
  - Add `NativeAbi` (with `::of(&ArtifactImport)`), `NativeProto::parse`, and
    `Origin { Current, Summary(String) }`.
  - Change `symbols` to `BTreeMap<String, Vec<NativeTarget>>`.
  - Split `observe` into `observe_java` and `observe_native_vmt`, and keep `observe` calling both.
  - `attribute_registries` looks up the `NativeTarget` by name.
- `cli/mod.rs:155`: pass `NativeAbi::of(&import)` and `Origin::Current`.
- Tests (`languages/jni/tests.rs`): `NativeProto::parse` cases; `NativeAbi` from each ABI's ELF
  header (arm64, x86_64, arm32, x86) and from a non-ELF file. The existing jni tests must stay
  green.

### 5. Summary observation and link through `get_or_add_function` (R3–R6, R10, R11)
- `cli/mod.rs`:
  - Open each `--summary` project once, before `jni::link`, and run `check_index_config`.
  - Dedup imports against `project.imports`, then call `load_vmt` → `observe_native_vmt(…,
    Summary(P))`, plus `observe_registry` unless `--no-jni-registry`. Skip all of this under
    `--no-jni-bridge`.
  - Pass the loaded projects to `load_and_map_summaries`, which no longer reloads them.
  - Log how many mapped summaries belong to bridged natives, and warn per bridged target that
    got zero.
- `jni.rs` `link`: the native side uses `get_or_add_function`. Add `LinkStats.from_summary` and
  `LinkStats.prototype_mismatch` (a placeholder until step 7), and include them in `Display`.
- Update the `load_and_map_summaries` doc comment and the `--summary` help text.
- Tests (unit):
  - A summary-origin target links and is interned.
  - An import in both the current and a summary project does not become `ambiguous`.
  - A Java native seen only on the native-only path is ignored.
  - `--no-jni-bridge` skips summary observation.

### 6. `Packaging::SummaryApk` in xtask (end-to-end proof of R1–R6)
- `xtask/src/discovery.rs`: add the variant and its case-name suffix (`+summary`).
- `xtask/src/regression.rs` `run_jni`:
  - Build `Y` as a trivial `libdummy.so` with no JNI.
  - Write one APK containing `classes.dex`, `lib/<abi>/X.so` and `lib/<abi>/Y.so`, then
    `import` it.
  - `index xproj <app>__<abi>__<X>`, then `index app <app> --no-native-libs --summary xproj`,
    then `query`, all with output captured.
  - Keep the existing `expected_lines` and `expected_native_lines` claims.
  - New assertions:
    - Y is absent from the project `imports` and from the IdMap dump.
    - There is no `'<X>': loading IR` line in the app index log.
    - `jni bridge` reports `from_summary > 0`.
    - SARIF renders binary locations in X.
    - `inspect` and graphviz do not panic.
- `JniRegister` under `+summary` covers the registry-only case.
- Test: a discovery unit test yields the `+summary` case.

### 7. ABI-aware port map and prototype check (R7, R8)
- `jni.rs`:
  - Add `NativeSlotModel { Typed, SplitWide }`. `port_map` takes it and maps a split wide
    param to both halves.
  - Choose the layout per linked method (spec §4.5, steps 1–3) and emit the three warnings,
    each incrementing `prototype_mismatch`.
  - `link` stops using `compute_num_params` and reads the recovered count from `NativeTarget`.
  - Module docs: update the port-mapping table and add a "Linking against a summary project"
    section. Add Limitations for FP ordering, count-only checks and context-free summaries.
- Tests (unit):
  - `port_map` with `SplitWide` for `(J)V`, `(IJ)V` and `(DJI)J`, both static and instance.
  - Each branch of the layout choice.
  - One test per warning row.
  - A co-indexed target and a summary target give the same check result.

### 8. 32-bit regression fixture (R7)
- `flake.nix`: add `pkgs.pkgsCross.gnu32.stdenv.cc` to the dev shell.
- `xtask/src/regression.rs`: `pick_toolchain(abi)` selects i686 for cases that ask for it. The
  lib entry becomes `lib/x86/…`.
- `nightly/tests/jni/JniWide.{java,c,json}`: an `(IJ)J` native passes a tainted `jlong` through
  to a sink. Two variants:
  - typed: compiled `-g`, so Ghidra gets a prototype;
  - untyped: stripped.

  Both must find the flow, and the untyped variant logs `SplitWide` at `debug`.
- Run it under `Separate` and `+summary`.

### 9. Summary provenance check (R9)
- `cli/mod.rs`: add `check_summary_provenance(project, &sp)`, which only warns:
  - "not this app's library": `S` is missing from the un-filtered `sub_imports` of the named
    imports;
  - "stale": the hash differs from `sp`'s `import_hashes`;
  - `info` when `sp` has no hashes.
- Tests (unit, Lua or fake-config projects): each warning fires, and a matching project stays
  silent.
- xtask `+summary`: assert that no provenance warning appears.

### 10. Baselines and docs (R11)
- Update the regression and dashboard baselines that pin `LinkStats`.
- `README.md` and `docs/`: add the §1 workflow and the `--no-native-libs` and `--summary`
  semantics.
- Full `cargo test` and `cargo xtask regression` (all frontends), with output captured.

### 11. Manual real-app checks (not committed)
Capture everything to `/Volumes/Shampoo/ct-jni-native/realapps/`.
- **Facebook Lite** (`~/apps/Facebook+Lite_513.0.0.6.105_APKPure.apk`):
  - Run §1 with `X = …__arm64-v8a__libsuperpack-jni`.
  - Expect `from_summary > 0`, `registered > 0` and no provenance warning.
  - The set of linked natives must equal the one from a full co-index with X, compared by
    diffing the `jni bridge:` debug lines.
- **NewPipe** (`~/apps/fdroid/org.schabi.newpipe_1015_cb84069.apk`):
  - Import with `--native-abi armeabi-v7a` and run §1.
  - Record the layout choice and the warnings.
  - Inspect Ghidra's recovered params for any `jlong` native to settle concern 1. If a padding
    slot shows up, add it to `SplitWide` (in step 7's code plus a unit test) before merging.
