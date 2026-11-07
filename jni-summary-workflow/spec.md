# Spec: JNI bridging against native summaries - DO-NOT-MERGE

Source: `intent.md`. Branch: `jni-native`.

## 1. Goal

Support this workflow:

```sh
ctadl import app.apk                         # imports app + sub-imports app__<abi>__libX, app__<abi>__libY
ctadl index xproj app__arm64-v8a__libX       # index X.so on its own
ctadl index appproj app --no-native-libs --summary xproj
ctadl query appproj -m models.json           # Java->X->Java flows are found; Y is never loaded
```

The Dex project never loads or indexes X's code. It uses only X's symbol table (VMT), its
`RegisterNatives` sidecar, and the summaries in X's saved index.

## 2. Current behaviour

| Area | Location | Behaviour |
| --- | --- | --- |
| Sub-import expansion | `ctadl-import/src/project.rs:662` (`AnalysisProject::ephemeral`) | Every named import is expanded to itself plus its `sub_imports`, one level deep. The expanded list is saved in the project's `imports`. For an APK, the sub-imports are its native libraries (`Pcode`). For an XAPK, they are the split APKs **and** their libraries, flattened. |
| Project creation | `ctadl-ascent/src/main.rs:938` | `index` always re-creates the project with `try_create`. |
| Query reload | `main.rs:1019` (`load_or_infer_project`) | Loads the saved `imports` list and does not expand it again. |
| JNI observation | `cli/mod.rs:155-162` | `observe` and `observe_registry` run only for the current project's imports. |
| Link | `cli/mod.rs:259`, `languages/jni.rs` `link` | Requires both the Java stub and the native function to be in the `IdMap` already. Otherwise the native is counted `unresolved`. |
| Port map | `jni.rs` `port_map` | The native side is fixed: declared parameter *k* goes to native formal `2 + k`. It is not aware of the ABI. |
| Prototype check | `jni.rs` `link`, `compute_num_params` | Warns when the native function has fewer recovered formals than the port map needs. It reads arity from `formal_param` facts, so it only works when the native code was codegen'd in this project. |
| Summary import | `cli/mod.rs:337`, `load_and_map_summaries` (`:983`) | Keeps a summary only when its function name is already in the current `IdMap` (`:1032-1041`). |
| VMT on disk | `ir-vmt.bitcode`, `ArtifactImport::vmt_path` | The only reader is `load_import`, which also decodes the whole program. |
| Native signature | `frontends/ctadl-pcode/src/lib.rs:468` | The string is `ret(_, _, …)`, holding only the parameter **count**, varargs and the return type. It is `()` when Ghidra recovered no prototype. |
| Native ABI | none | Not recorded on the import. It can be derived from the ELF header of `artifact_path`, which for an APK sub-import is the extracted `.so` in the store. |

## 3. Requirements

- **R1.** `ctadl index … --no-native-libs` drops every **expanded** sub-import whose language is
  `Pcode`, meaning the native libraries. Other sub-imports, such as an XAPK's split APKs, are
  kept. Imports named explicitly on the command line are always kept.
- **R2.** `query`, `report` and `inspect` on the project see the filtered import list. The saved
  `imports` list provides this, so no separate flag needs to be persisted.
- **R3.** For each `--summary P`, the native half of every import of `P` (its symbol table and
  its `RegisterNatives` table) is fed to the JNI observer **without loading the import's program
  IR**.
- **R4.** A Java `native` stub whose implementation is known only from a summary project is
  linked. The native function is interned in the current `IdMap`, and the bridge facts
  (`call`, `actual_param`, `formal_param`) are emitted exactly as for a co-indexed library.
- **R5.** After R4, `load_and_map_summaries` keeps the summaries of the bridged native
  functions. Summary loading stays after `jni::link`.
- **R6.** Summary projects contribute native **targets** only:
    - A Java `native` declared only in a summary project is not observed.
    - An import present in both the current project and a summary project is observed once.
- **R7.** The native port map depends on the ABI. On 32-bit ABIs it accounts for a
  `long`/`double` argument recovered as two parameters (§4.5).
- **R8.** For every linked native, the parameter count implied by the Dex descriptor and ABI is
  compared with the count Ghidra recovered. The Dex descriptor is authoritative. The code warns
  on a mismatch. This works both when the native is co-indexed and when it comes from a summary.
- **R9.** `index --summary P` warns when `P` may not describe this app's libraries (§4.6).
- **R10.** `--no-jni-bridge` and `--no-jni-registry` keep their current meaning and also apply
  to summary-sourced natives.
- **R11.** `LinkStats` and the `info` line report summary-sourced links and prototype
  mismatches. Regression baselines that pin the link counts are updated (§6).

## 4. Design

### 4.1 `--no-native-libs` (R1, R2)

- `IndexArgs` (`main.rs:288`): add `#[arg(long)] pub no_native_libs: bool`. The doc comment says
  it is the index-time counterpart of `import --no-native-libs` and names the summary workflow.
  Update the two `IndexArgs { … }` literals at `main.rs:661` and `:812` to pass `false`.
- `ctadl run`: no change. It already has `--no-native-libs` and passes it to import, so no
  libraries are imported in the first place.
- Change `ephemeral` and `try_create` to take a `SubImports` enum with values `All` and
  `NoNativeLibs`, instead of adding a bare bool. The filter checks each sub-import's `language`
  from its `ArtifactImport` config. A sub-import whose config fails to load passes through
  unchanged, as it does today. `index_artifacts_to_store` maps the flag, and every other caller
  passes `All`.

### 4.2 Loading only the VMT (R3)

Add `pub fn load_vmt(import: &ArtifactImport) -> Result<VirtualMethodTable, Error>` to
`ctadl-import/src/store.rs`, beside `load_import`. It reuses `refuse_unfinished`, then reads and
decodes `vmt_path()` only. `load_import` calls it internally so the two stay in step.

### 4.3 Observing summary projects (R3, R6, R10)

In `cli::index`, after the import loop and before `jni::link`:

```text
if !no_jni_bridge:
  seen = set(project.imports)
  for P in summary_projects:
    sp = AnalysisProject::try_load_name(P); sp.check_index_config()?
    check_summary_provenance(project, &sp)          # §4.6, warnings only
    for import in sp.iter_imports() where seen.insert(import.name):
      vmt = load_vmt(&import)?
      jni_observer.observe_native_vmt(&vmt, NativeAbi::of(&import), Origin::Summary(P))
      if !no_jni_registry: jni_observer.observe_registry(&import)?
```

`JniObserver` changes:

- Split `observe` into a Java-half method and a native-half method. `observe` keeps calling both,
  and the summary path calls only the native half. This satisfies R6: Java natives from summary
  projects are ignored.
- Change `symbols` to store one `NativeTarget` per function instead of a bare `String`. It holds
  the IR function name, the `NativeProto` (§4.5), the `NativeAbi`, and the origin (`Current` or
  `Summary`).
- The dedup by import name matters. Without it, an import in both projects puts two entries under
  one symbol, and `resolve_by_symbol` reports every such native as **ambiguous**.
- Registry-resolved targets (`attribute_registries`) look up their `NativeTarget` by function
  name, so the ABI and prototype are available for them as well.

Open the summary projects once. `load_and_map_summaries` currently reloads each one; pass the
loaded `AnalysisProject` through instead.

### 4.4 Link (R4, R11)

- Java stub: keep `get_function_id`. A stub missing from the fact base is still `unresolved`.
- Native target: use `source_info.sites.get_or_add_function(...)`. This is a no-op for
  co-indexed targets, which are already present.
- Add `LinkStats.from_summary` and `LinkStats.prototype_mismatch`, both subsets of `linked`, and
  include both in `Display`.

Summary loading stays at `cli/mod.rs:337`, after link. The index engine replays
`summary(tgt, …)` over `call(…, tgt)` during the fixpoint (`index_engine/mod.rs:1503`), so the
bridge needs only the interned id. The `IdMap` filter in `load_and_map_summaries` then admits X's
summaries.

`load_and_map_summaries` needs no logic change. Do update its doc comment and the `--summary`
help text ("filtered to functions that exist in the current project, including native targets
linked by the JNI bridge"). Log how many mapped summaries belong to bridged natives, because a
bridge whose target got zero summaries produces no flow and no error.

Results that pass through a summary-only native are located by the existing per-import binary
location machinery. No SARIF change is planned, but the integration test asserts that such
results render (§5).

### 4.5 ABI-aware port map and prototype check (R7, R8)

**`NativeAbi`** (in `jni.rs`) takes one of the values `Arm64`, `X86_64`, `Arm32`, `X86` or
`Unknown`. `NativeAbi::of(&ArtifactImport)` reads `e_machine` and `EI_CLASS` from the ELF header
at `artifact_path`. `jni_registry.rs:389` already parses these, so share that helper. A non-ELF
file, a missing artifact or a Ghidra-server URL gives `Unknown`, which behaves like the 64-bit
ABIs.

**`NativeProto::parse(sig)`**, where `sig` is the `NativeSignature` string:

| Signature | Parsed |
| --- | --- |
| `()` | `Unknown`: no prototype recovered |
| `ret(_, _, …)` | `params = n`, `vararg` |

**Native slot layouts.** Native formal 0 is always `JNIEnv *` and formal 1 is always
`jobject`/`jclass`. For declared parameter *k*:

| Layout | Used for | Declared `J`/`D` | Everything else |
| --- | --- | --- | --- |
| `Typed` | every ABI when Ghidra's count matches it; the default | 1 native formal | 1 native formal |
| `SplitWide` | `Arm32`, `X86` only | 2 consecutive native formals (lo, hi) | 1 native formal |

`port_map` takes a `NativeSlotModel` next to the existing Java `SlotModel`. It returns one
`(java, native)` pair per native formal. A split wide argument therefore maps its one Java slot
to **both** halves, and taint reaches whichever half the native code reads.

**Layout choice** is made per linked method, from the recovered count `r` and each layout's
expected count (`2 + declared params`, plus one per wide parameter for `SplitWide`):

1. If `r` equals the `Typed` count, use `Typed`.
2. Otherwise, if the ABI is 32-bit and `r` equals the `SplitWide` count, use `SplitWide` and log
   at `debug`.
3. Otherwise, use `Typed` and warn (below).

**Warnings**: one per native method. Each also increments `prototype_mismatch`.

| Condition | Message gist |
| --- | --- |
| `Unknown` prototype | Ghidra recovered no prototype for `<fn>`, so no argument flows. Re-import with types or `-g`. |
| `r` is lower than the chosen layout's count | This is the existing message. Arguments above index `r-1` are dropped. |
| `r` is higher than every layout's count, not vararg | The recovered prototype has `r` params but Dex implies `n` (or `m` split). Arguments may be mis-slotted. |

The checks only report: the link is emitted in every case. `compute_num_params` is no longer used
by `link`, so co-indexed and summary-sourced natives are checked the same way.

### 4.6 Summary provenance check (R9)

Warn, without failing, for each `Pcode` import `S` in summary project `P` when either condition
holds:

- **Not this app's library.** `S.name` is not among the un-filtered `sub_imports` of the current
  project's named imports, read from their `ArtifactImport` configs. This catches another ABI,
  another app, or another app version imported under a different name.
- **Stale.** `S`'s current `hash` differs from the hash recorded when `P` was indexed.
    - This requires a new field on the index config: `IndexConfig.import_hashes:
      BTreeMap<String, String>`, with `#[serde(default)]`. `write_index_config` fills it.
    - An index written before this field existed gets one `info` line ("cannot check
      staleness") rather than a warning.
    - The field is additive and serde-defaulted, so `INDEX_FORMAT_VERSION` does not change.

### 4.7 Files touched

- `ctadl-import/src/project.rs`: `SubImports` filter; `IndexConfig.import_hashes`.
- `ctadl-import/src/store.rs`: `load_vmt`.
- `ctadl-ascent/src/main.rs`: `IndexArgs`, the literals, `index_artifacts_to_store`.
- `ctadl-ascent/src/cli/mod.rs`: summary-project observation, the provenance check, the reused
  project handle, recording import hashes, logging.
- `ctadl-ascent/src/languages/jni.rs`: observer split, `NativeTarget`, `NativeAbi`,
  `NativeProto`, `NativeSlotModel`, `port_map`, `get_or_add_function`, stats. Update the module
  docs: the "How arguments are mapped" table, a new "Linking against a summary project" section,
  and Limitations.
- `frontends/ctadl-pcode/src/jni_registry.rs`: expose the ELF machine and class helper.
- `README.md` / `docs/`: the workflow above.

## 5. Tests

- **Unit** (`languages/jni/tests.rs`):
    - `NativeProto::parse` cases.
    - `NativeAbi` from ELF headers of each supported ABI.
    - `port_map` under `SplitWide` for `(J)V`, `(IJ)V`, `(DJI)J`, static and instance.
    - Layout choice for each branch.
    - One case per warning row.
    - Summary-origin target links through `get_or_add_function`.
    - A duplicate import across the current and summary projects does not become ambiguous.
    - Java natives on the native-only path are ignored.
- **Unit** (`ctadl-import`):
    - `SubImports::NoNativeLibs` on an APK: only the parent remains.
    - The same on an XAPK: the split APKs remain and the libraries are dropped.
    - An explicitly named `Pcode` import is kept.
    - `load_vmt` equals `load_import(..).vmt`.
    - `IndexConfig` without `import_hashes` still loads.
- **Integration** (`ctadl-ascent/tests/bridging_end_to_end.rs` style): the §1 workflow on an APK
  fixture with two libs. Assert that:
    - Java→X→Java taint is found, and the SARIF for it renders with binary locations in X.
    - Y's name appears nowhere in the project `imports` or the `IdMap`.
    - X's program IR is never loaded (assert on the `'…': loading IR` log lines).
    - Flows through X match the full co-index result.
    - `query` over the index does not panic on the bodyless interned natives, including the
      SARIF, graphviz and `inspect_index_facts` paths.
- **Provenance**: summary project built from another ABI's sub-import gives the "not this app"
  warning; re-importing X after indexing `xproj` gives the "stale" warning.
- **Registry case**: the §1 workflow where X binds only through `RegisterNatives`, as in
  `nightly/tests/jni/JniRegister`.
- **32-bit**: an `armeabi-v7a` fixture with a `jlong` parameter, with and without a typed
  prototype. Both must yield the flow. Facebook Lite ships no 32-bit code. The smallest real app
  in `~/apps` with `armeabi-v7a` libraries is `fdroid/org.schabi.newpipe_1015_cb84069.apk`
  (11.5 MB, one library, `libandroidx.graphics.path.so`). Import it with `--native-abi
  armeabi-v7a` for a real-world check of the layout choice.
- **Real app**: `~/apps/Facebook+Lite_513.0.0.6.105_APKPure.apk` (3.4 MB, one `classes.dex`,
  `arm64-v8a` only). This is the smallest APK in `~/apps` with native code, and it matches the
  §1 shape exactly:

  | Role | Library | JNI surface |
  | --- | --- | --- |
  | X | `libsuperpack-jni.so` (195 KB) | Exports one `Java_…` symbol (`Java_com_facebook_superpack_AssetDecompressor_testDecompressorLibraryUsable`) and has `JNI_OnLoad`. Its data holds `RegisterNatives` descriptors such as `(J)Z`, `(JJ)V` and `([BII)V`, so it exercises both the symbol and the registry resolution paths. |
  | Y | `libbreakpad_cpp_helper.so` (7 KB) | No JNI symbols. It must never appear in the Dex project. |

  Run §1 with `X = <app>__arm64-v8a__libsuperpack-jni`. Assert that:
    - `jni bridge` reports `from_summary > 0` and `registered > 0`.
    - It links the same set of natives as a full co-index of the APK with X.
    - The provenance check (§4.6) stays silent.

  The APK is proprietary. Reference it by path and do not commit it. Skip the test when the
  file is absent.
- **Nightly**: add the workflow to `nightly/tests/jni/`. Per `CLAUDE.md`, capture all run output
  to files (under `/Volumes/Shampoo` if large).

## 6. Concerns and known limitations

No open questions remain. Items 1–6 are resolved or accepted as stated.

1. **`armeabi-v7a` register alignment (plan agreed).** AAPCS places a 64-bit argument in an
   even/odd register pair. For `(IJ)V` that is `r0` env, `r1` obj, `r2` int, skip `r3`, then
   the long on the stack. Whether Ghidra, without a prototype, shows the skipped `r3` as a
   parameter is unverified. If it does, `SplitWide` gains a padding slot that no Java port maps
   to, and the expected count includes it. **The 32-bit fixture (§5) is the first task of
   §4.5, and its result decides this.**
2. **Floating-point parameters (accepted limitation).** On `arm64`/`x86_64` (and hard-float
   targets), `float` and `double` arrive in FP registers. Without a prototype, Ghidra may order
   them after the integer parameters rather than in declaration order, and `Typed` would then
   mis-slot them. The count check cannot detect this. Document it in the `jni.rs`
   Limitations; fixing it is out of scope (§7).
3. **The count is the only check.** `NativeSignature` records `_` for each parameter
   (`ctadl-pcode/src/lib.rs:475`), so a mis-slotting that keeps the count right, as in Q2,
   passes silently. Checking types needs the pcode frontend to record them, and an
   `IMPORT_FORMAT_VERSION` bump.
4. **A missing prototype loses the return value.** Ghidra functions without a prototype get
   return arity 0 (`ctadl-pcode/src/lib.rs:436`). X's summaries then hold no return flows, and
   taint cannot come back to Java even over a correct link. The "no prototype" warning covers
   this case, and it is not fixed here.
5. **Only context-free summaries transfer (accepted).** `summary` is copied, but
   `context_summary` and `critical_summary` are not. Flows in X that depend on indirect-call
   resolution are lost. This is an existing limitation of `--summary`; document it in the
   `jni.rs` Limitations.
6. **Stats and baselines change (accepted).** Natives currently counted `unresolved` become
   `linked` when a summary project supplies them, and 32-bit wide-argument links change shape.
   Update regression baselines and dashboards keyed on `LinkStats` in the same PR.

## 7. Out of scope

- Modelling `JNIEnv` accessors. This is an existing limitation in `jni.rs`.
- Recording parameter types in the pcode frontend (concern 3).
- Reordering FP-register parameters in untyped prototypes (concern 2).
- Automatically discovering or indexing summary projects for sub-imports.
- `ctadl run --summary`.
