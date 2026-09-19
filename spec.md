# Compositional indexing of native sub-imports - DO-NOT-MERGE

Requirements and design for indexing an Android app's native libraries separately and feeding
their summaries into the app's index, instead of co-indexing everything in one job.

Source: `intent.md`. Status: proposal, not yet implemented. The branch is marked DO-NOT-MERGE.

Scope: **native sub-imports only** — the `.so` files an APK import extracts out of itself. Java
code is always co-indexed, and so is native code that arrived any other way. See §4.1.

---

## 1. Goal

Importing an APK also imports the `.so` files inside it, each as its own sub-import. Today
`ctadl index app app` loads the Dex **and** every native library into one fact base and runs one
fixpoint over all of it. That is precise, and on a large app it does not fit in memory.

CTADL's analysis is compositional: a function's effect on data flow is captured by its summary,
and the index engine already replays a callee's summary at every call site that targets it. So we
can index each native library on its own, keep its summary table, and give the app's index only
the summaries of the functions it actually calls across the JNI boundary.

The result should be:

- one index per native library, written to the store the normal way;
- an app index that never loads native IR, and whose peak memory is the app's own cost plus a
  small amount for the loaded summaries;
- taint that still crosses the JNI boundary in both directions, including for methods bound by
  `RegisterNatives`;
- all of it behind one CLI flag, off by default.

---

## 2. How it works today

Three pieces matter. All of them are reused; none is replaced.

**How native code gets into a project.** Two routes. Only the first is in scope, and the second
is worth knowing about because it is what the flag will *not* do (§7/C1).

- *As a sub-import.* `ArtifactImport::sub_imports` (`ctadl-import/src/project.rs:267`) lists
  imports derived from another one, and `AnalysisProject::ephemeral`
  (`ctadl-import/src/project.rs:635`) expands each named import to *itself followed by its
  sub-imports*, one level, no recursion. That is what makes `ctadl index app app` co-index the
  `.so` files. Only two importers produce sub-imports: `apk_native.rs` (the libraries under
  `lib/<abi>/` in an APK) and `xapk.rs` (the split APKs of an app bundle, flattened, each
  followed by its own libraries). `record_sub_imports` is called from the `Apk` and `Xapk` arms
  of `ctadl_frontends::import_artifact` and nowhere else.
- *As a top-level import named on the command line.* `ctadl import -l pcode libapp.so` then
  `ctadl index app app_dex app_native` — the README's workflow for when the two halves are
  separate files.

The JNI bridge does not care which route a library took; it resolves across whatever imports the
project holds. The restriction to route one is a scope decision, not a technical one.

**The JNI bridge** (`ctadl-ascent/src/languages/jni.rs`). During the import loop,
`JniObserver::observe` reads each import's VMT: from a Java import it collects the methods
declared `native`, from a pcode import it collects the native symbol table.
`JniObserver::observe_registry` reads the `jni-registry.json` sidecar written at import time.
After the loop, `jni::link` (`jni.rs:511`) resolves each Java `native` method to one native
function — a recovered `RegisterNatives` binding first, then the mangled long symbol name, then
the short one — and `emit_bridge` (`jni.rs:843`) writes, inside the bodyless Java stub:

- one fresh call site targeting the native function (`facts.call`),
- one `actual_param` row per mapped port, using `port_map` to shift across the JNI ABI,
- the `formal_param` rows the Java stub needs, since a Dex `native` method declares none.

**The summary rule.** In the index datalog (`ctadl-ascent/src/index_engine/mod.rs`, "Compute
assignments from summaries", line 1418):

```
assign_like(func_id, v1, p1, v2, p2) <--
    summary(tgt, n1, dst_path, n2, src_path),
    call(func_id, insn_id, tgt),
    ...
```

**This rule is the whole basis of the design.** It needs the callee's `summary` rows and a `call`
row pointing at it. It does **not** need the callee's body, its `formal_param` rows, or anything
else about it. A `FunctionId` that exists only as a name in the `IdMap` and a set of summary rows
composes at call sites exactly like a fully analyzed function.

Also relevant: `ctadl index` already has `-s/--summary NAME`, which calls `load_and_map_summaries`
(`ctadl-ascent/src/cli/mod.rs:984`). It reads another project's `summary.parquet` and `IdMap`,
and copies each row across **when the function name already exists in the current project**. That
is the right shape but the wrong policy for us: in compositional mode the native function
deliberately does *not* exist in the app's fact base, so every row would be discarded. It also
runs *after* `jni::link`, which is too late.

---

## 3. Requirements

### Functional

- **F1.** A CLI flag on `ctadl index` turns on compositional indexing of the project's native
  sub-imports. Default off; behaviour with the flag absent is byte-identical to today. `ctadl go`
  should mirror it, since it forwards to the same `IndexArgs`.
- **F2.** With the flag on, each eligible native sub-import is indexed as its own project, saved to
  the store in the normal layout, and is queryable on its own afterwards.
- **F3.** The app index does not load any eligible native sub-import's IR. Its `project_config.json`
  lists only the imports it actually co-indexed.
- **F4.** The app index loads, from each sub-index, only the summary rows for the native
  functions reached across the JNI boundary — not the whole summary table, and none of the other
  index tables except what is needed to resolve names and arities.
- **F5.** JNI matching is applied when loading. Both binding mechanisms work: `Java_…` name
  mangling (long name then short name) and recovered `RegisterNatives` tables. Argument ports are
  mapped across the JNI ABI shift exactly as the co-indexed path does.
- **F6.** The fact base and relations of a sub-index run are dropped before the next sub-index
  starts, and before the app's own index runs. Release is in-process and therefore partial: the
  process-global interners keep the names and paths they saw (§4.6). That residue is accepted.
- **F7.** Reading and writing the store goes through the existing APIs (`ArtifactImport`,
  `AnalysisProject`, `facts::schema::*`, `IdMap`, `load_import`). No new on-disk layout and no
  hand-rolled path building.
- **F8.** A sub-index that fails does not abort the run. It is reported at `warn` and the app
  index proceeds without that library's summaries — the same degradation as an APK imported
  without Ghidra.
- **F9.** The run reports, at `info`, what it did: which native sub-imports were indexed, how many
  summary rows each contributed, how many were loaded, and how many native methods linked.

### Non-functional

- **N1.** Peak resident memory of a compositional run is bounded by
  `max(app index, largest single sub-index) + loaded summaries + interner residue`, not by the sum
  of the parts. This is the point of the feature and must be measured, not assumed; the measurement
  in §8.5 is also what tells us how big the interner residue actually is.
- **N2.** Wall-clock time may grow — several fixpoints instead of one — but not
  catastrophically. Expect it to be roughly the sum of the parts, minus whatever the co-indexed run
  spent on cross-boundary work.
- **N3.** Findings on the JNI regression cases must be identical to the co-indexed run. Where they
  cannot be (see §6), the difference must be known, documented, and pinned by a test.

### Out of scope

- Compositional indexing of anything that is not a native sub-import. Java imports — including
  the split APKs of an app bundle, which are sub-imports — are always co-indexed, and so is a
  pcode artifact the user imported and named. See §4.1.
- Recursive sub-imports (a sub-import with sub-imports of its own). The existing list is flat.
- Native → Java callbacks (`CallVoidMethod` and friends). Not modelled today either.
- Caching or reuse of a sub-index across different store roots or machines.

---

## 4. Design

### 4.1 Which imports qualify

**The rule: an import is eligible when it is a sub-import, its language is `Pcode`, and the
project also holds at least one Java import (`Dex`, `Apk`, `Jar` or `Jvm`).** Everything else is
co-indexed as it is today.

All three clauses do work.

*Sub-import* is the intent's scope. A pcode import the user named on the command line is
co-indexed as it is today; see §7/C1 for what that costs.

*Language* is what keeps Java code co-indexed. `ArtifactImport::sub_imports` is a flat list with
two very different kinds of thing in it:

| Producer | Sub-import contents | Can it be summarized? |
| --- | --- | --- |
| `apk_native.rs` | one pcode import per `lib/<abi>/*.so` | **Yes.** It meets the app only across the JNI boundary, which the bridge already models with an explicit port map. |
| `xapk.rs` | one import per split APK, plus each split's own native libraries, flattened | **No** for the split APKs. A split holds the app's own Dex. It shares a class hierarchy and a call graph with the base APK; summarizing it would break virtual dispatch and CHA. Its `.so` sub-imports are eligible; the splits themselves are not. |

A language test handles the bundle case for free: the splits are `Apk`, so they are co-indexed;
their libraries are `Pcode`, so they are summarized. No special case needed.

*The Java-half guard* keeps the flag from emptying a native-only project. A split APK with no
`classes*.dex` is a normal import — it becomes a parent with an empty Java program and one native
sub-import per library (`apk_native.rs`, "Native-only APKs"). So `ctadl index p native_split`
names a project whose only real code is an eligible sub-import, and summarizing it would leave an
empty index. When no Java import is present the flag is a no-op and says so at `warn`. Naming
both halves (`ctadl index p dex_split native_split`) has a Java import and works normally.

### 4.2 Sub-index projects

Each eligible sub-import is indexed into its own `AnalysisProject`, created with
`AnalysisProject::try_create(name, [name])`. The project name is the sub-import's own name, which
is already globally unique and derived from the parent (`app__arm64-v8a__libfoo`,
`apk_native.rs:345`).

The sub-index is an ordinary index. It gets the default native propagation models, the same
`--models` files, and the same call-resolution options as the app's run, so the two halves are
analyzed under one policy. It writes `summary.parquet`, `function_id.parquet`,
`formal_param.parquet` and the rest through `IndexFacts::try_save` / `IndexResult::try_save`, and
stamps `index_config.json` last, as usual. Nothing new on disk.

Consequence worth advertising: `ctadl query app__arm64-v8a__libfoo -m models.json5` works, and
answers questions about that library alone.

### 4.3 Order of operations in the app's run

`cli::index` changes as follows. Steps marked **new** are additions; everything else already
exists and keeps its current place.

1. **new** — Expand the named imports as today, then partition them into co-indexed ones and
   eligible native ones (§4.1). Build the app's `AnalysisProject` from the co-indexed set only.
2. **new** — For each eligible sub-import, in turn:
   a. skip it if a fresh sub-index already exists (§4.5);
   b. otherwise index it into its own project;
   c. drop everything and record the peak (§4.6).
3. **new** — Read each sub-index's `IdMap` and `formal_param` table and build a
   `SubIndexSummaries` handle per library. Do not read `summary.parquet` yet.
4. Run the existing import loop over the co-indexed imports: IR load, JNI observation, model
   matching, SSA, codegen. Unchanged.
5. **new, before `jni::link`** — For each sub-import, load its VMT (§4.4) and feed it to the
   `JniObserver` with `observe`, and its registry sidecar with `observe_registry`. This is the
   only thing the app's run needs from the native side to resolve the boundary.
6. **new, before `jni::link`** — Intern every native function name that the sub-indexes know
   about into `source_info.sites`, via `IdMap::get_or_add_function`.
7. Run `jni::link` unchanged. It now resolves Java stubs against symbols and registry entries from
   step 5, finds the interned native ids from step 6, and emits its `call` / `actual_param` /
   `formal_param` rows exactly as it does today.
8. **new** — Collect the set of native `FunctionId`s that `jni::link` actually targeted, and load
   only those functions' summary rows out of each sub-index into `facts.summary` (§4.4).
9. Everything after this — model codegen, `facts.try_save`, `taint_index_with_config`, saving the
   result — is unchanged.

Steps 6 and 8 are deliberately split. Interning has to happen before `link` so the bridge can find
the native id; loading the rows has to happen after, so we know which functions to load.

Step 5 feeds `SlotModel::for_language` (`jni.rs:222`) exactly the way the import loop does, so
port mapping is unchanged. In scope that means the Dex register-slot model, since only an APK
parent produces native sub-imports.

Note that `jni::link` needs no change at all. The one edit near it is the existing "incomplete
prototype" diagnostic, which compares the port map against `facts.compute_num_params()`. In
compositional mode the native function has no `formal_param` rows in the app's fact base, so the
check would fire for every bridged method. Fix: take the native arity from the sub-index's
`formal_param.parquet` (already loaded in step 3) when the function came from a sub-index.

### 4.4 Loading summaries and mapping them across the boundary

**Decision: keep the summary rows on the native function, not on the Java stub.**

The native function is interned into the app's `IdMap` and given its summary rows verbatim —
same formal indices, same access paths, no rewriting. The mapping to Java is done entirely by the
bridge's `actual_param` rows, which already speak native formal indices (`port_map` returns
`(java_index, native_index)` pairs, and `emit_bridge` writes the native index into the
`actual_param` row). The summary rule then joins the two on that index and produces the right
`assign_like` edges.

The alternative — rewriting each summary row onto the Java stub with Java formal indices —
composes `port_map` with the summary by hand, has to decide what to do with rows mentioning a
native port that maps to nothing (`JNIEnv *`), destroys the ability to say *which* native function
a flow went through, and would break the declarative `bridge` model path. It is not worth it.

Keeping the rows on the native function means a compositional run and a co-indexed run agree on
the shape of the fact base at the boundary. The co-indexed run derives the native function's
summary in the app's own fixpoint; the compositional run loads the same rows from disk. That is
the claim the regression tests in §8 pin.

**What gets loaded, per native sub-import:**

| Table | Read? | Used for |
| --- | --- | --- |
| `function_id.parquet` (`IdMap::try_load`) | yes, in full | resolving sub-index `FunctionId` to a name, and back to an app-side id |
| `formal_param.parquet` | yes, in full | native arity, for the incomplete-prototype check |
| `summary.parquet` | yes, filtered to bridged functions | the summaries themselves |
| `ir-vmt.bitcode` (from the *import*, not the index) | yes | the native symbol table for JNI resolution |
| `jni-registry.json` (from the import) | yes | `RegisterNatives` bindings |
| `assign.parquet`, `paths.parquet`, `actual_param.parquet`, `call.parquet`, source info | **no** | not needed; these are the large ones |
| `ir-program.bitcode` | **no** | the native IR never enters the app run |

The VMT read needs a small new reader. `load_import` (`ctadl-import/src/store.rs:83`) decodes the
program *and* the VMT; we want the VMT alone, which for a large library is a fraction of the cost.
Add `ctadl_import::store::load_vmt(&ArtifactImport) -> Result<VirtualMethodTable, Error>`
alongside it, sharing the same version check, and have `load_import` call it. `JniObserver::observe`
takes a `&ProgramInfo` today and reads only `.vmt`; give it a sibling
`observe_vmt(&VirtualMethodTable, SlotModel)` and make `observe` a one-line wrapper.

Filtering `summary.parquet` to the bridged functions: the existing
`facts::schema::summary::try_load` reads the whole table into a `Vec`. For a first
implementation, call it and filter in memory, then drop the `Vec` — correct, and the peak is one
library's summary table, which is far smaller than its `assign` table. If measurement shows that
peak matters, push the filter into the parquet reader as a follow-up; do not do it up front.

**Which functions count as bridged.** The set is exactly what `jni::link` resolved. Two things
feed it and both are already handled by existing code: the mangled-symbol tiers in
`resolve_by_symbol`, and the `RegisterNatives` attribution in `attribute_registries`. Because we
hand the observer the same VMT and the same registry sidecar it would have seen in a co-indexed
run, resolution is identical — including the long-name-then-short-name order, the overload
ambiguity refusal, and the registration-beats-symbol rule.

**Transitivity is free.** A summary is the whole compositional effect of a function, including
everything it calls inside the library. Loading the entry point's summary therefore covers the
library's internals; nothing deeper needs loading.

### 4.5 Reusing a sub-index

A sub-index depends only on the sub-import's contents and the index options. Re-indexing an
unchanged library on every app run would make the feature painful to iterate on, so: skip a
sub-index when a fresh one already exists.

Freshness needs a stamp. `IndexConfig` (`ctadl-import/src/project.rs`) currently records
`version` and an optional `call_policy`. Add one more optional field:

```rust
pub struct IndexConfig {
    pub version: String,
    #[serde(default)] pub call_policy: Option<CallPolicyRecord>,
    /// What this index was built from: the imports' content hashes, in project order.
    #[serde(default)] pub inputs: Option<InputDigest>,
}
```

A sub-index is fresh when its `index_config.json` exists, its `version` matches
`INDEX_FORMAT_VERSION`, its `inputs` digest matches the sub-import's recorded
`ArtifactImport::hash`, and its `call_policy` matches the policy this run would use. Anything else
— missing field, older index, changed flags — means re-index.

Because the new field is `#[serde(default)]` and no table encoding changes, this does **not**
require bumping `INDEX_FORMAT_VERSION`. An older index simply reads back with `inputs: None` and
is treated as not fresh.

Give the flag an escape hatch (`--force-sub-index`, or reuse a general `--force`) that re-indexes
regardless.

### 4.6 Freeing memory between sub-indexes

The requirement is F6/N1. **Decided: in-process.** The subprocess alternative is described below
and is not being built.

**In-process — what gets built.** Each sub-index runs inside its own scope; the `IndexFacts`, the
`IndexResult` and the ascent relations are dropped at the end of it. This releases the bulk — the
`assign` / `locals` / `assign_like` data, which is what actually gets large.

It does **not** release everything, and this needs saying plainly. CTADL has two process-global
interners that never shrink:

- `facts::STRING_TABLE` (`ctadl-ascent/src/facts.rs:27`) — a `HashMap` plus a `FrozenVec` of
  `immortal::StringRef`. Function names and symbols go in and stay.
- The `tailshare` sequence interner behind `Path`, which is built on `immortal::Interner` and
  interns with `Box::leak` (`immortal/src/lib.rs`).

So every distinct function name and every distinct access path derived while indexing a library
stays resident for the rest of the process. Across a dozen libraries that residue is real, and on
a path-heavy binary the `Path` interner is the bigger half. On top of that, freed heap is not
necessarily returned to the OS by the allocator.

**This is accepted.** The interners hold names and paths, not the `assign`-shaped data that makes a
co-indexed run infeasible, so dropping the fact base still gets the bulk of the win that motivates
the feature. Keeping everything in one process also keeps the orchestration in §4.3 straightforward
— no argv reconstruction, no child exit-code plumbing, no second copy of the policy flags — and
lets the unit tests in §8 run the sub-index path and assert on the fact base directly. The residue
is a known quantity to be measured (§8.5), not a defect to design around.

**Subprocess — the alternative, not being built.** Run each sub-index by re-executing the ctadl
binary — `std::env::current_exe()`, `--store <root>`, `index <sub-import>`, plus the same policy
flags — and wait for it. When the child exits, the OS reclaims everything, interners included, and
the parent process's footprint is untouched. It costs a process spawn per library (negligible next to
a fixpoint), and it makes indexing several libraries in parallel a later one-line change. It stays
on the table as a follow-up if §8.5 shows the residue actually hurts on a real app; nothing in this
design forecloses it, since the per-library run is a single function either way.

Either way, log `phys_footprint_mb()` before and after each sub-index at `debug`, on the `[mem cp]`
convention already used throughout `index_engine`. With the in-process choice those numbers are the
only visibility into the residue, so they are not optional.

### 4.7 CLI

On `ctadl index`:

```
--compositional-native-sub-imports
        Index each native sub-import on its own and load only its summaries.

        An APK's native libraries are normally co-indexed with the app, which is precise
        but loads every program into one job. With this flag each library is indexed as
        its own project and the app's index loads only the summaries of the functions
        its `native` methods call. Java code is co-indexed either way, including the
        split APKs of an app bundle.

        Applies only to native code the importer extracted for itself. A pcode artifact
        you imported and named yourself is co-indexed as usual. The flag has no effect on
        a project with no Java half.

--force-sub-index
        Re-index every native sub-import even when an up-to-date sub-index already exists.
```

Mirror `--compositional-native-sub-imports` on `ctadl go`, which forwards to `IndexArgs`
(`ctadl-ascent/src/main.rs:650`). Add the field to the two other `IndexArgs` literals
(`main.rs:804`, the legacy pcode path) as `false`.

Interaction with the existing `-s/--summary NAME`: both end in `facts.summary`, and both may be
given. `--summary` keeps its current meaning — copy rows for functions that already exist in this
project, by name. The new path is separate and does not change it.

### 4.8 Logging

One line per phase and per sub-import, per `docs/debugging.md`. At `info`:

```
compositional: 3 native sub-import(s): app__arm64-v8a__libfoo, app__arm64-v8a__libcrypto, ...
compositional: indexing 'app__arm64-v8a__libfoo' (1 of 3)
compositional: 'app__arm64-v8a__libfoo': 4812 summary row(s) over 1190 function(s)
compositional: 'app__arm64-v8a__libcrypto': reusing existing sub-index
compositional: loaded 96 summary row(s) for 12 bridged function(s) from 3 sub-index(es)
jni bridge: 14 native method(s): 12 linked (9 registered), 1 unresolved, 1 ambiguous
```

The `jni bridge` and `jni registry` lines are the existing ones and must keep reporting the same
numbers they would in a co-indexed run. That is the cheapest signal that the feature is working:
a method that fails to link produces no flow and no error.

At `debug`: per-function summary row counts, and the `[mem cp]` footprint around each sub-index.

### 4.9 On-disk changes

- `IndexConfig` gains `inputs` (`#[serde(default)]`). No format-version bump.
- `AnalysisProject` gains `sub_indexes: Vec<String>` (`#[serde(default)]`) recording which
  sub-index projects fed this one, so `ctadl inspect` and `ctadl query` can report it and a later
  query can warn when a sub-index has gone stale.
- Nothing else. No new files, no new directories, no change to `IMPORT_FORMAT_VERSION` or
  `INDEX_FORMAT_VERSION`.

---

## 5. Code changes, by file

| File | Change |
| --- | --- |
| `ctadl-import/src/store.rs` | **new** `load_vmt(&ArtifactImport)`; `load_import` calls it. |
| `ctadl-import/src/project.rs` | `IndexConfig.inputs`; `AnalysisProject.sub_indexes`; helper to test sub-index freshness; helper to partition a name list into co-indexed vs. eligible native sub-imports (by `ArtifactLanguage`, plus the Java-half guard). |
| `ctadl-ascent/src/languages/jni.rs` | `JniObserver::observe_vmt(&VirtualMethodTable, SlotModel)`; `observe` becomes a wrapper. `link` gains an optional map of externally supplied native arities for the prototype check, and returns (or records) the set of native `FunctionId`s it bridged. |
| `ctadl-ascent/src/cli/mod.rs` | `IndexOptions.compositional_native_sub_imports` and `.force_sub_index`; the orchestration in §4.3; a new `sub_index` module holding the per-library run, the `SubIndexSummaries` loader and the filtered summary load. `load_and_map_summaries` is left alone. |
| `ctadl-ascent/src/main.rs` | the two new flags on `IndexArgs` and `GoArgs`; wire through; fill the other `IndexArgs` literals. |
| `README.md`, `docs/debugging.md` | §9. |
| `nightly/`, `xtask/src/discovery.rs` | §8. |

Nothing in `index_engine`, `codegen` or `query_engine` needs to change. That is the main
structural claim of this design and is worth checking early: if it turns out that a seeded summary
on a body-less function does *not* behave like a derived one, the design needs revisiting before
anything else is built. §8 puts that check first.

---

## 6. What you give up

Compositional analysis is not free. These are expected differences from a co-indexed run, not
bugs, and the docs should say so.

1. **No context at the boundary.** A co-indexed run can let hybrid inlining specialize a native
   function's flows per calling context (`--hybrid-context decision`). A loaded summary is
   context-free: every Java caller of the same native method shares it. In practice a native
   method usually has one caller, so this rarely bites.

2. **Path composition is exact-match only across the seam.** Summary access paths enter the
   fixpoint as `model_paths`, which combine with program paths at *one* level
   (`compute_paths`, `index_engine/mod.rs:1199`). A flow needing a residue two levels deep past
   the boundary is dropped, silently. This is the same limitation the declarative `bridge` model
   already documents.

3. **Native → Java callbacks are still not modelled**, and now the native code is not even in the
   index, so nothing could be added later by a query. Unchanged in practice: `JNIEnv` vtable calls
   are unresolvable today.

4. **`bridge` models cannot name a compositionally-indexed native function.** Model matching runs
   against a `ProgramMatchIndex` built from loaded IR, and the native IR is not loaded. A user who
   joins a boundary by hand with `--no-jni-bridge` and a `bridge` model must not use
   `--compositional-native-sub-imports`. This should be a hard error, not a silent miss.

5. **Native results vanish from the SARIF.** The native half is not in the app index, so a
   finding is located in the Dex, and the code-flow stops at the bridge site. Where the native
   library itself is interesting, query its own sub-index project.

6. **More index format surface to keep in sync.** The app index is now only as good as the
   sub-indexes it read. If a sub-index is re-indexed under different flags the app index is stale and
   nothing currently notices. §4.9 records `sub_indexes` so a warning can be added.

---

## 7. Concerns and contradictions

Flagging these explicitly; one of them changes what gets built.

**C1 — the flag does nothing for the separate-files workflow. ACCEPTED: this is the intended
behaviour and nothing is built for it.** Not a contradiction: the intent is Android and native
sub-imports, and those line up, because an APK's native code always arrives as a sub-import. This
is just the consequence, written down so nobody is surprised by it.

The README documents a second way to analyze both halves, for when they are separate files:

```
ctadl import app.dex            --name app_dex
ctadl import -l pcode libapp.so --name app_native
ctadl index  app app_dex app_native
```

Here `app_native` is a top-level import, not a sub-import, so the flag skips it and the run
co-indexes exactly as it does today — no error, no warning, just no benefit. That also means the
base `Jni:Foo` nightly variant, which imports its two halves separately, cannot be a compositional
test case; only the `+apk` and `+split-apks` variants can (§8).

Extending eligibility to a named pcode import would be a small change — the JNI bridge already
resolves across imports without caring how they arrived, and the Java-half guard already exists —
but it is past what the intent asks for. **Decided: do nothing for the separate-files workflow.**
A named pcode import stays co-indexed, the flag stays silently a no-op there, and no error, warning
or eligibility extension is added for it. The only obligation this creates is documentary: §4.7's
flag help and §9's README paragraph say that the flag applies to native code the importer extracted
for itself, and the cheap-insurance test in §8.4 pins that the base `Jni:Foo` variant is unchanged.

**C2 — "free RAM after each sub-index" is not achievable in-process. RESOLVED: in-process, and the
partial release is accepted.** Two global interners leak by construction (§4.6), so dropping the
fact base leaves a residue proportional to the distinct names and paths of every library indexed so
far. That is fine: what the interners retain is names and paths, not the `assign`-shaped data that
makes co-indexing infeasible, and the simpler orchestration and directly testable sub-index path are
worth it. F6 is worded to match (release is of the fact base, not of the process). The subprocess
variant is documented in §4.6 as a follow-up if §8.5 measures the residue as a real cost.

**C3 — the existing `--summary` flag does the opposite of what we need.**
`load_and_map_summaries` keeps a row only when the function *already exists* in the target
project; compositional mode needs rows for functions that deliberately do not. This is why the new
path is separate rather than a tweak to the existing one. Worth deciding whether the two should
eventually merge; this spec keeps them apart to avoid changing `--summary`'s behaviour.

**C4 — summary paths can blow up the path set.** Every distinct path in a loaded summary becomes a
`model_path`, and `compute_paths` concatenates every model path with every program path. Loading
the summaries of a handful of bridged functions is fine. Loading a whole library's summary table
would be a `|model| × |program|` self-join. This is the concrete reason F4 says "filtered to the
bridged functions" and not "load the sub-index's summaries". It must not be relaxed without
measuring.

**C5 — resolution quality depends on `jni-registry.json`, which is written at import time.** A
library imported before the registry scanner existed has no sidecar, and `import --skip-existing`
will not create one. In compositional mode this is more visible than today, because that library's
methods will link by symbol name alone and most Android apps export almost no `Java_…` symbols.
The failure is silent — fewer links, no error. Mitigation: when a native sub-import has no
`jni-registry.json`, say so at `warn` and name the re-import command.

**C6 — precision loss has not been quantified.** The design argues that loading a summary is
equivalent to deriving it, and the argument is sound for the context-free case. It has not been
measured on a real app. The nightly JNI cases (§8) pin equivalence on small fixtures; a real APK
A/B is the thing that would justify turning this on by default, and it is not part of this work.

**C7 — `external_function` for a bridged native.** Today a call to an unmodelled external
function makes the query report absorption. A compositionally-summarized native function is not
external — the sub-index analyzed it — so this spec does not mark it so. But a native function
whose prototype Ghidra failed to recover produces no useful summary and will now look "analyzed,
no flow" instead of "unknown". **Open:** mark it `external_function` when the sub-index has zero
summary rows *and* zero formal params for it.

**C8 — index policy must match across the seam.** A sub-index built with a different
`--strategy` or `--cha-threshold` answers a different question. §4.2 passes the app's options
down, and §4.5 makes a policy mismatch force a re-index. A user who indexes a sub-import by hand
first, with other flags, gets it re-indexed; that is the intended behaviour, but it will surprise
someone.

**C9 — store concurrency.** Two app runs sharing a native sub-import would race on the same
sub-index project directory. The store has no locking today. Low priority, but the failure mode is
a corrupt index rather than an error, so it should at least be written down.

---

## 8. Testing

**Build order matters here.** Do step 1 before writing any of the orchestration.

1. **The core claim, as a unit test** (`ctadl-ascent/tests/`). Build a fact base by hand with a
   caller, a `call` row to a function id that has `summary` rows and nothing else, and assert the
   fixpoint produces the same `assign_like` edges as a run where that function's body was present.
   If this fails, stop and redesign.

2. **Summary round-trip** (`ctadl-ascent/tests/`). Index a small pcode program, load its summaries
   back through the new loader, and assert the rows match `IndexResult::summary` for the selected
   functions — same indices, same paths, after the parquet round-trip.

3. **Eligibility and project shape** (`ctadl-ascent/tests/cli.rs`, beside
   `test_project_expands_sub_imports`). Four claims: an APK import with native sub-imports builds
   an app project listing only the APK plus one sub-index project per library; an `.xapk` keeps
   its Java splits co-indexed and summarizes only their `.so`s; a top-level pcode import named
   beside a Dex import is **not** summarized (§7/C1); and a project with no Java half leaves the
   flag a no-op. Uses the synthetic APK builder already in that file.

4. **End-to-end equivalence** (nightly, `xtask/src/discovery.rs`). Add
   **`Jni:Foo+apk-compositional`** beside `Jni:Foo+apk`: the same APK packaging, indexed with the
   flag, making the same claims from the same `foo.json` known answer. This runs over `JniFlow`,
   `JniArgShift` and `JniRegister` — plain flow, the ABI argument shift, and `RegisterNatives` —
   for free, and it is the test that says the feature preserves findings. A
   `+split-apks-compositional` variant is worth adding too: it is the case where the `.so` is a
   sub-import of one APK and the Dex is a different top-level import, which exercises the
   cross-import resolution the bridge does in `attribute_registries`.

   Note what cannot be tested this way: the base `Jni:Foo` variant imports its halves separately,
   so it has no native sub-import and the flag is a no-op on it (§7/C1). That is correct
   behaviour, and a test asserting the co-indexed result is unchanged there is cheap insurance.

5. **Memory** (nightly or a manual measurement). Index `xtask/tests/dex/com.noto_54.apk`-style
   input both ways and compare peak physical footprint. That APK ships no native libraries, so a
   real target with `lib/<abi>` is needed; if none is checked in, this is a manual measurement, run
   with the `measure-process-memory` skill and its output captured to a file.

   Because release is in-process (§4.6), this measurement also has to report the interner residue:
   record the `[mem cp]` footprint after each sub-index is dropped, so the baseline's growth across
   a multi-library app is visible. A residue that grows into the same order as a single sub-index is
   the signal to revisit the subprocess variant.

6. **Failure paths.** A native sub-import whose index fails leaves the app run succeeding with a
   warning (F8). A sub-index that is stale gets re-indexed. `--compositional-native-sub-imports` together
   with a `bridge` model is refused (§6.4).

---

## 9. Documentation

- `README.md`, "The JNI bridge" section: a short paragraph on the flag, what it changes, and the
  fact that native results move to the sub-index project.
- `docs/debugging.md`: how to read the `compositional:` lines, and where a sub-index lives
  (`projects/<parent>__<abi>__<lib>/index`) so it can be opened in duckdb like any other.
- Module docs on the new `cli::sub_index` module, in the house style: what it does, what it
  deliberately does not do, and the reason the summary rows stay on the native function (§4.4).

---

## 10. Open questions for the team

1. ~~C2 — subprocess per sub-index, or in-process with partial memory release?~~ **Resolved:
   in-process.** The interner residue is accepted; see §4.6 and §7/C2. Nothing blocks
   implementation.
2. C7 — should a bridged native with an empty summary be marked `external_function`?
3. ~~C1 — is the separate-files workflow (`ctadl index app app_dex app_native`) meant to be left
   out?~~ **Resolved: yes, left out.** The flag is silently a no-op there by design; nothing is
   built for that workflow.
4. Should `--compositional-native-sub-imports` eventually become the default for APKs above some
   size, or stay opt-in? (Needs C6 measured first.)
5. Is a sub-index meant to be a first-class, user-visible project, or an implementation detail that
   `ctadl inspect` hides? This spec makes it first-class and queryable.
