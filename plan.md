# Implementation plan: compositional indexing of native sub-imports - DO-NOT-MERGE

From `intent.md` and `spec.md`.

Read this beside §4.3 of the spec, which is the order of operations inside the app's run. This
file says which files change, in what order, and what test proves each step.

## Decisions taken before starting

Four things the spec left open. All settled:

1. **A sub-index is run by calling `cli::index` recursively**, on a project built from the one
   sub-import, with the same `IndexOptions` and the same `--models`, and the compositional flag
   off. No second copy of the policy plumbing, so a sub-index this run *builds* shares its policy
   (§7/C8) because there is only one code path, not because two were kept in sync. One it merely
   reuses may not; see Phase 4a.
2. **`--compositional-native-sub-imports` + a `bridge` model is refused only when both actually
   apply**: the flag is on, the project has at least one eligible native sub-import, and a model
   file declares a `bridge`. A bridge on a Java-only project still works.
3. **The memory measurement (§8.5) runs against real APKs in `~/apps`.** `FX+File+Explorer` is
   the primary target: ~12 MB, and several mid-size `.so` per ABI, which is the shape that shows
   whether interner residue accumulates across libraries. `TikTok+Lite` (~81 MB, libraries up to
   5 MB) is the heavy follow-up. All output captured to a file.
4. **Everything in §8 lands on this branch**, nightly variants included. The nightly cases need
   `javac`/`dx`/Ghidra to execute, so locally we verify only that they are discovered.

Two places where this plan narrows what the spec says, on purpose — see Phase 4.

---

## Phase 0 — prove the core claim first

Nothing else is worth building if a seeded summary on a body-less function does not compose like a
derived one (§5, §8.1).

**Files:** `ctadl-ascent/tests/compositional_summary.rs` (new). No production code.

**Work:** build an `IndexFacts` by hand with a caller, a `call` row to a `FunctionId` that has
`summary` rows and nothing else — no body, no `formal_param`, no `assign` — and run
`taint_index_with_config`. Assert it produces the same `assign_like` edges as a second fact base
where that callee's body is present.

The rule this pins is `index_engine/mod.rs:1418`:

```
assign_like(func_id, v1, p1, v2, p2) <--
    summary(tgt, n1, dst_path, n2, src_path),
    call(func_id, insn_id, tgt), ...
```

It joins `summary` and `call` and touches nothing else about the callee, and `model_paths` is
built from `facts.summary` directly (`mod.rs:1842`), so a loaded row's access paths enter the path
set with no extra step. Both readings say this should pass. **If it does not, stop and redesign.**

**Done when:** the test passes and the two fact bases give identical `assign_like` sets.

---

## Phase 1 — store and project plumbing

**Files:** `ctadl-import/src/project.rs`, `ctadl-ascent/src/cli/mod.rs`,
`ctadl-ascent/src/models/spec.rs`

The on-disk stamp records the *whole* configuration an index was built under, not the call policy
alone. `CallPolicyRecord` was scoped to what `ctadl query` needed to print (#127); this feature is
the first thing that has to compare two index runs, and a partial record is the wrong tool for
that. The record splits into two groups, and the split is the whole of §7/C8:

- **inputs** — a difference means the index answers a different question. A stale sub-index is
  re-indexed. Imports, model files, shipped defaults, `--summary` projects.
- **policy and engine switches** — compared and warned on, never enforced. Call policy, alias
  rule, hybrid context, and the rest of `IndexOptions`.

1. `IndexConfig` gains `#[serde(default)] pub record: Option<IndexRecord>`. The existing
   `call_policy` field stays so a stamp written before `record` existed still reads; new stamps
   leave it `None`, and `index_call_policy` (`:763`) reads `record.policy` first and falls back
   to it.

   ```rust
   pub struct IndexRecord {
       /// A difference here means the index answers a different question: re-index.
       pub inputs: IndexInputs,
       /// Reported, compared and warned on, never enforced (§7/C8).
       pub policy: CallPolicyRecord,
       pub engine: EngineRecord,
   }

   pub struct IndexInputs {
       /// `(import name, ArtifactImport::hash)` in project order. Nothing is re-hashed.
       pub imports: Vec<(String, String)>,
       /// The `-m` files that could admit at least one of the project's imports (see item 5),
       /// listed for the human report only. Comparison is by `model_digest`.
       pub model_files: Vec<PathBuf>,
       /// SHA-256 over the contents of `model_files`, sorted, via `hash_file_contents`.
       pub model_digest: String,
       /// SHA-256 over `DEFAULT_MODEL_FILES`. `INDEX_FORMAT_VERSION` is a table-format version,
       /// not a build id, so an edited shipped default would otherwise leave every existing
       /// sub-index looking fresh.
       pub default_model_digest: String,
       pub no_default_models: bool,
       pub summary_projects: Vec<String>,
   }

   pub struct EngineRecord {
       pub alias_rule: bool,
       /// `"none"`, `"decision"` or `"collapse"`: a string for the same reason
       /// `CallPolicyRecord::strategy` is one -- `ctadl-import` sits below the engine.
       pub hybrid_context: String,
       pub prune_unreachable_cfg_nodes: bool,
       pub no_jni_bridge: bool,
       pub no_jni_registry: bool,
   }
   ```

   Left out on purpose: `parallelism` and `dump_index_graph`, which do not change the result. An
   import whose `hash` is `None` (imported before that field existed) yields no `inputs`, so such
   an index is never considered fresh.
2. `write_index_config` (`:746`) takes an `IndexRecord` instead of `Option<CallPolicyRecord>`.
   One production caller (`cli/mod.rs:376`) and one test caller (`tests/cli.rs:416`).
3. `CallPolicyRecord`, `EngineRecord` and `IndexInputs` derive `PartialEq, Eq`.
4. New `AnalysisProject::index_record(&self) -> Option<IndexRecord>` and
   `index_is_fresh(&self, inputs: &IndexInputs) -> bool`: true only when the stamp exists,
   `version` equals `INDEX_FORMAT_VERSION`, and `record.inputs == *inputs`. Missing record, older
   version, changed library, changed admitting model, changed defaults — all false. Policy and
   engine are deliberately not consulted here (§4.5, §7/C8); Phase 4a compares them for the
   warning.
5. New `models::spec::admitting_model_files(paths: &[PathBuf], imports: &[ArtifactImport]) ->
   Result<Vec<PathBuf>, Error>`: walks each file with `visit_model_file` (`spec.rs:452`), parses
   every block's `in` with `ProgramScope::parse`, and keeps the file when any block `admits`
   (`spec.rs:134`) any of the imports' `ImportScope`s. A file scoped wholly to `dex` contributes
   nothing to a pcode sub-index's digest, so editing Java models does not re-index every native
   library; an unscoped block admits everything, so such a file always counts.
6. `call_policy_record` (`cli/mod.rs:434`) grows into two: `index_inputs(project,
   summary_projects, models, no_default_models)` and `index_record(inputs, opts, endpoint_files)`.
   Both are `pub(crate)`, because `sub_index::run_one` needs the inputs *before* deciding whether
   to index (Phase 4a) and the record after.
5. `AnalysisProject` gains `#[serde(default)] pub sub_indexes: Vec<String>` (§4.9), set by the run
   that writes the index.
6. New `AnalysisProject::try_create_exact(name, imports, sub_indexes)`: `try_create` without the
   sub-import expansion. `try_create` → `ephemeral` (`:635`) expands every name to itself plus its
   sub-imports, which would put the native libraries straight back into the app project we are
   trying to keep them out of. Still deduplicates, order-preserving.
7. New `pub fn partition_native_sub_imports(names: &[S]) -> (Vec<String>, Vec<String>)`: expands
   as `ephemeral` does, then splits off the eligible ones — sub-import, `language == Pcode`, and
   the project holds at least one of `Dex`/`Apk`/`Jar`/`Jvm`. With no Java half nothing is
   eligible and the caller warns (§4.1).

**Tests** (`ctadl-ascent/tests/cli.rs`, beside `test_project_expands_sub_imports:302`, using the
synthetic APK builder already there):

- an APK import with two pcode sub-imports partitions into `[apk]` + `[lib_a, lib_b]`;
- an `.xapk` keeps its split APKs co-indexed and splits off only their `.so`s;
- a top-level pcode import named beside a Dex import is **not** eligible (§7/C1);
- a project with no Java import yields an empty eligible list;
- `try_create_exact` writes a `project_config.json` listing only the names given (F3);
- `index_is_fresh` is false for each of: no stamp, wrong version, changed import hash, changed
  contents of an admitting model file, changed default-model digest, toggled `no_default_models`,
  changed `--summary` list; true when the inputs agree and only `policy` or `engine` differ;
- `admitting_model_files` (in `models/tests.rs`): a file whose every block is scoped
  `{"language": "dex"}` is excluded for a pcode import and included for a dex one; a file with one
  unscoped block is included for any import; a file scoped `{"import": "<name>"}` is included for
  that import only;
- `index_call_policy` still reads a legacy stamp that has `call_policy` and no `record`.

---

## Phase 2 — read a VMT without reading the program

**Files:** `ctadl-import/src/store.rs`, `ctadl-ascent/src/languages/jni.rs`

- `store.rs`: new `pub fn load_vmt(&ArtifactImport) -> Result<VirtualMethodTable, Error>`, sharing
  the version check with `load_import` (`:83`); `load_import` calls it. Decoding a big library's
  `ir-program.bitcode` is the cost we are avoiding, and the VMT is a small fraction of it.
- `jni.rs`: split `JniObserver::observe` (`:393`) into `observe_vmt(&VirtualMethodTable,
  SlotModel)` plus a one-line `observe` wrapper. No behaviour change.

**Test:** `load_vmt` on an import returns the same VMT `load_import` does, and
`observe_vmt` leaves the observer in the same state as `observe` given the same table.

---

## Phase 3 — teach `jni::link` two things

**File:** `ctadl-ascent/src/languages/jni.rs` (`link`, `:511`)

The resolution logic does not change at all. Two mechanical changes:

1. **Arity from outside.** `link` takes `external_arity: &HashMap<FunctionId, i16>`. The
   incomplete-prototype warning (`:618`) consults it before falling back to
   `facts.compute_num_params()`. Without this the warning fires for every bridged method in
   compositional mode, since the native function has no `formal_param` rows in the app's fact
   base (§4.3).
2. **Report what it bridged.** `link` returns `LinkOutcome { stats: LinkStats, bridged:
   HashMap<FunctionId, &str> }` — the native ids it emitted a bridge to, and their names.
   `LinkStats` stays `Copy` and keeps its `Display`. Eleven test call sites in
   `languages/jni/tests.rs` and one in `cli/mod.rs:258` gain `&Default::default()` and `.stats`.

**Tests:** the existing `jni/tests.rs` cases keep passing unchanged (modulo the two mechanical
edits), plus one new case: with an external arity supplied for a function that has no
`formal_param` rows, no incomplete-prototype warning is produced and the bridge is still emitted.

---

## Phase 4 — the sub-index module

**File:** `ctadl-ascent/src/cli/sub_index.rs` (new), declared from `cli/mod.rs`.
`load_and_map_summaries` (`cli/mod.rs:984`) is not touched (§7/C3).

Module docs in the house style: what it does, what it deliberately does not do, and why the
summary rows stay on the native function rather than being rewritten onto the Java stub (§4.4).

### 4a. Running one sub-index

```rust
pub fn run_one(import: &ArtifactImport, models: &[PathBuf], no_default_models: bool,
               opts: IndexOptions<'_>, force: bool) -> Result<String, Error>
```

- project name = the sub-import's own name, already unique and derived (`app__arm64-v8a__libfoo`);
- build this library's `IndexInputs` with `cli::index_inputs` (Phase 1, item 6): its one import
  and hash, the `-m` files that admit it, the defaults digest, `no_default_models`, and an empty
  `--summary` list;
- skip when `index_is_fresh(&inputs)` and `!force`, logging `reusing existing sub-index`. Before
  skipping, compare `index_record().policy` and `.engine` with this run's and, on a difference,
  warn naming both and `--force-sub-index` (§7/C8, §4.8). Reuse regardless. A record with no
  `policy` (a legacy stamp) has no `inputs` either, so it is never fresh and the question does
  not arise;
- otherwise `AnalysisProject::try_create_exact(name, [name], [])` and `cli::index(...)` with
  `native_sub_imports: &[]`, `dump_index_graph: None`, everything else inherited;
- log `phys_footprint_mb()` before and after on the `[mem cp]` convention (§4.6).

The `IndexFacts`, `IndexResult` and ascent relations are dropped when `cli::index` returns, which
is the whole of the in-process release (F6). The interner residue is accepted and measured, not
designed around.

### 4b. `SubIndexSummaries` — what stays resident

One per library, built before the app's import loop:

```rust
pub struct SubIndexSummaries {
    project: String,
    index_path: PathBuf,
    /// Only the functions the JNI boundary could possibly reach.
    candidates: HashMap<Function, (FunctionId /* sub-index id */, i16 /* arity */)>,
}
```

**Two deliberate narrowings of spec §4.3/§4.4**, both about what stays resident rather than what is
read:

- *§4.3 step 6 says "intern every native function name the sub-indexes know about."* We intern
  only the **candidate** names: the ones in the library's VMT symbol table, plus the `function`
  field of its `RegisterNatives` entries. Those are the only names `link` can ever produce —
  `resolve` (`jni.rs:763`) draws from `obs.symbols` and `attribute_registries` (`:656`) draws from
  registry entries, and nothing else reaches `get_function_id`. Interning a large library's whole
  IdMap would inflate the app's `function_id.parquet` and `STRING_TABLE` for no reachable gain.
  A candidate name absent from the sub-index's IdMap is dropped with a `debug` line — that is the
  signal for a frontend name-mismatch bug.
- *§4.4 says read `function_id.parquet` and `formal_param.parquet` "in full".* They are read in
  full and then **reduced to the candidate set**, and the full `Vec`s are dropped. Residency is
  proportional to the JNI surface, not to the library. The parquet read still interns every name
  it parses; that residue is the same one §4.4 notes for `summary.parquet`, and the same follow-up
  (push the filter into the reader) fixes both if measurement says it matters.

`arity` is the max formal index + 1 over the candidate's `formal_param` rows, which is what the
prototype check in Phase 3 consumes.

### 4c. Loading the rows

```rust
pub fn load_bridged(subs: &[SubIndexSummaries], bridged: &HashMap<FunctionId, &str>,
                    facts: &mut IndexFacts, sites: &IdMap) -> LoadReport
```

For each library: `facts::schema::summary::try_load` on its index path, keep only rows whose
function is both a candidate and in `bridged`, rewrite the `FunctionId` from the sub-index's id to
the app's, push onto `facts.summary` with formal indices and access paths **unchanged**, drop the
`Vec`. `paths.parquet` is never opened (F4, §7/C4): the loaded rows' paths reach `model_paths` on
their own through `index_engine/mod.rs:1842`.

F10: a bridged function with zero retained summary rows **and** zero formal params gets a row on
`facts.external_function`. Both conditions, never one (§7/C7) — `absorbing_functions`
(`query_engine/mod.rs:564`) does not check for summaries, so marking a well-summarized native
would report absorption at a boundary that really carries flow.

**Tests** (`ctadl-ascent/tests/compositional_summary.rs`, §8.2):

- round-trip: index a small pcode program, load its summaries back through `load_bridged`, assert
  the retained rows equal `IndexResult::summary` for those functions — same indices, same paths,
  after the parquet round-trip;
- a function that is not bridged contributes no rows and no paths;
- a bridged function with no summaries and no formals gets an `external_function` row; one with
  summaries does not.

---

## Phase 5 — orchestration and the CLI

**Files:** `ctadl-ascent/src/cli/mod.rs`, `ctadl-ascent/src/main.rs`

`IndexOptions` (`cli/mod.rs:52`) gains two fields, both `Copy`-compatible:

```rust
/// Eligible native sub-imports to summarize instead of co-index. Empty = today's behaviour.
pub native_sub_imports: &'a [String],
pub force_sub_index: bool,
```

`main.rs`: `index_artifacts_to_store` (`:921`) calls `partition_native_sub_imports` when the flag
is on, builds the app project with `try_create_exact` from the co-indexed half, and passes the
eligible half through `IndexOptions`. `ctadl go` (`:650`) reaches this through the same function,
so it inherits the behaviour once the two flags are added to `IndexArgs` (`:278`) and `GoArgs`.
The legacy pcode literal (`:804`) gets `&[]` and `false`. Flag help text is §4.7 verbatim.

`cli::index` (`:95`), following §4.3 — the numbering is the spec's:

| Step | Where | What |
| --- | --- | --- |
| 1 | before `index()` | partition; project built from co-indexed imports only (Phase 1, main.rs) |
| — | `:125`, after `scan_model_files` | refuse `bridge` + eligible sub-imports (decision 2) |
| 2 | before the import loop | `sub_index::run_one` per library, in turn, dropping between |
| 3 | after step 2 | build one `SubIndexSummaries` per library |
| 4 | `:140`–`:230` | the existing import loop, unchanged |
| 5 | before `:258` | `load_vmt` + `observe_vmt` + `observe_registry` per sub-import |
| 6 | before `:258` | intern candidate names; build the `external_arity` map |
| 7 | `:258` | `jni::link`, unchanged logic, now returning `LinkOutcome` |
| 8 | after `:258` | `sub_index::load_bridged`; push `external_function` rows |
| 9 | `:337` onward | unchanged: model codegen, `try_save`, fixpoint, save |

Steps 6 and 8 stay split for the reason §4.3 gives: interning must precede `link` so the bridge can
find the native id, and loading must follow it so we know which functions to load.

`write_index_config` at `:376` now takes the full `IndexRecord` from `cli::index_record`, and the
app project records `sub_indexes`. The app's own record hashes every `-m` file that admits any of
its imports, which for an APK project is all of them.

**Logging** (§4.8) at `info`, one line per phase and per library: the list of eligible
sub-imports; `indexing 'X' (n of m)` or `reusing existing sub-index`; rows and functions per
library; the total loaded for the bridged set; the count marked external. The existing `jni
bridge:` and `jni registry:` lines must keep reporting the same numbers a co-indexed run reports —
that is the cheapest signal the feature works, because a method that fails to link produces no flow
and no error.

**Tests** (`ctadl-ascent/tests/cli.rs`): with the flag on over a synthetic APK with native
sub-imports, the app's `project_config.json` lists only the APK (F3) and `sub_indexes` names one
project per library (§4.9); with the flag off, the run is byte-identical to today (F1).

---

## Phase 6 — failure paths

All in `cli/sub_index.rs` and `cli/mod.rs`; tests in `ctadl-ascent/tests/`.

- **F8:** a sub-index that fails is logged at `warn` and the app run continues without that
  library's summaries. Same degradation as an APK imported without Ghidra. Its bridged functions
  then fail to resolve (no interned names), which the existing `jni bridge:` counts report.
- **§7/C5:** after `link`, when it left `native` methods unresolved **and** some native sub-import
  has no `jni-registry.json`, warn and name the parent's re-import command — the parent's
  `artifact_path`, not the extracted `.so`. A library with no sidecar whose methods all linked by
  symbol does **not** warn. `scan_import` writes nothing when it finds no tables, so there is no
  "scanned, found none" marker to distinguish; conditioning on an actual failure is what keeps this
  from crying wolf.
- **§6.4 / decision 2:** flag + eligible sub-imports + a `bridge` model is a hard error, before any
  indexing happens.
- **§7/C8:** a sub-index current in every way except its `policy` or `engine` record is reused,
  and a `warn` names both and `--force-sub-index`. With `--force-sub-index` it is rebuilt and no
  warning is emitted. A sub-index with a legacy stamp (`call_policy` and no `record`) has no
  inputs to compare, so it is re-indexed like any stale one.
- **Inputs are enforced, not warned:** a sub-index whose admitting model file changed, whose
  shipped defaults digest changed, or whose `no_default_models` flipped is re-indexed and says
  so; one where only a `dex`-scoped model file changed is reused with no warning.

---

## Phase 7 — end-to-end mapping (nightly)

**Files:** `xtask/src/discovery.rs`, `xtask/src/regression.rs`

`Kind::Jni` gains a `compositional: bool`. `discover_jni` (`:286`) emits two more variants beside
the existing three: **`Jni:<stem>+apk-compositional`** and **`Jni:<stem>+split-apks-compositional`**,
same artifacts, same `foo.json` known answers, differing only in the flag. `run_jni`
(`regression.rs:1504`) adds `--compositional-native-sub-imports` to its `index` invocation when the
field is set.

This runs over `JniFlow`, `JniArgShift` and `JniRegister` for free: plain flow, the ABI argument
shift, and `RegisterNatives`. The split-APK variant is the one where the `.so` is a sub-import of
one APK and the Dex is a different top-level import, which exercises the cross-import resolution
`attribute_registries` does.

These fixtures are small enough that nothing in §6 costs a finding, so **a claim that stops holding
is a mapping bug, not accepted precision loss** (N3, §7/C6).

The base `Jni:<stem>` variant imports its two halves separately, so it has no native sub-import and
the flag is a no-op there (§7/C1). Asserting its result is unchanged is cheap insurance.

**Runs locally:** the discovery unit tests in `discovery.rs` (extended to expect the two new
names). **Runs in nightly only:** the cases themselves, which need `javac`, `dx`, a C compiler and
Ghidra.

---

## Phase 8 — documentation

- `README.md`, "The JNI bridge": a paragraph on the flag, what it changes, and the fact that native
  findings move to the sub-index project, which is an ordinary queryable project.
- `docs/debugging.md`: how to read the `compositional:` lines, and where a sub-index lives
  (`projects/<parent>__<abi>__<lib>/index`) so it opens in duckdb like any other.
- Module docs on `cli::sub_index` (written in Phase 4).

---

## Phase 9 — memory (N1, §8.5)

Not assumed, measured. This is the point of the feature.

Target: `~/apps/FX+File+Explorer_9.1.0.8_APKPure.apk`, imported with `--native-abi arm64-v8a`
(several mid-size `.so` — `libavcodec`, `libavutil`, and friends — which is what makes residue
across libraries visible). Then `~/apps/TikTok+Lite+-+Save+Data+%26+Fast_44.0.3_APKPure.apk` if the
first run is informative.

Procedure, using the `measure-process-memory` skill, run under `memory-guard`, **with all output
captured to a file** (`/Volumes/Shampoo` if the logs get large):

1. index co-indexed (flag off), record peak physical footprint;
2. index compositionally (flag on), record peak;
3. from the `[mem cp]` lines, record the footprint after each sub-index is dropped, so the
   baseline's growth across libraries is visible.

**What the numbers have to say:** peak is bounded by `max(app index, largest single sub-index) +
loaded summaries + interner residue`, not by the sum of the parts. If the residue grows into the
same order as a single sub-index, that is the signal to revisit the subprocess variant (§4.6) —
which this design deliberately leaves open, since the per-library run is a single function either
way.

No A/B on precision. §7/C6 accepts the loss without quantifying it, and the flag stays opt-in
because of that.

---

## Build order, short form

0. Core-claim unit test — **gate; stop here if it fails.**
1. `ctadl-import`: `IndexConfig.record` (`IndexRecord`), `sub_indexes`, `try_create_exact`,
   partition, freshness; `admitting_model_files`; `cli::index_inputs` / `index_record`.
2. `load_vmt` + `observe_vmt`.
3. `jni::link`: external arity, `LinkOutcome`.
4. `cli::sub_index`: run one, `SubIndexSummaries`, `load_bridged`, F10.
5. `cli::index` orchestration + both CLI flags + logging.
6. Failure paths and diagnostics.
7. Nightly variants.
8. Docs.
9. Memory measurement on `~/apps`.

Phases 1–3 are independent of each other and each lands with its own tests. Phase 4 needs 1–3.
Phase 5 needs 4.
