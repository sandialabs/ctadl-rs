# `ctadl report` — implementation plan, phase 1 - DO-NOT-MERGE

Status: approved, ready to implement. Derived from `intent.md` and `spec.md`, and corrected
against measurements of this tree and of the indexes in `~/.local/state/ctadl`.

## Context

`intent.md` asks a planning question, not a bug-finding one: **is it worth making call
resolution more precise, and where?** The concrete sub-question is whether a handful of
terrible call sites explain most of the call-graph imprecision — if so, special-case them;
if not, the whole analysis has to get better. Nothing in CTADL answers this today. The
numbers that exist are buried in `log::debug!` inside `cli::inspect_index_facts`.

`spec.md` designs a `ctadl report <name>` subcommand for it, at two tiers (static = needs
only an import; resolved = needs an index). This plan implements phase 1 of that design
with four corrections, each measured against this repo and against existing indexes in
`~/.local/state/ctadl` rather than inferred.

### What measurement changed about the design

**1. `call.parquet` is monomorphic-only, so the resolved tier cannot supply fan-out.**
Default strategy is `CallResolutionStrategy::Mixed` (`main.rs:274`). Under it, codegen emits
a `call` edge only when CHA resolves to exactly one target and defers everything else to
`callee_info`. Measured on the existing `vlc` index:

| table | distinct sites | targets/site |
| --- | --- | --- |
| `call.parquet` | 570,387 | exactly 1, max 1 |
| `callee_info.parquet` | 139,695 | deferred to hybrid inlining |
| overlap | 0 | — |

So `spec.md` §5.4 ("from `call` we get ... fan-out") is vacuous, and the
`inspect_index_facts` top-50-busiest-sites logic that §5.2 proposes to promote computes
"every site has 1 target" on any Java program. Fan-**in** over the same table is fine and
directly answers an intent requirement: 126,838 methods, mean 4.5, p99 34, max 25,417
(`Lkotlin/jvm/internal/Intrinsics;->checkNotNullParameter`).

**Consequence:** the static tier owns every fan-out / CHA / RTA / distribution number. The
resolved tier contributes fan-in, SCCs, and the deferred-site count only. And because
`index_config.json` today holds only `{"version":"3"}`, the report cannot tell which
strategy built the index it reads — so the strategy has to be recorded.

**2. `callee_resolvents` is not per-call-site.** Its schema is
`(object, context, target_id)` (`facts/schema.rs:89-99`) — allocated type × method
signature, with no call site and no declared receiver type. Per-site CHA counts are
obtainable only from the import plus `run_cha`. Its top entries on `vlc` are `<init>`
(10,885 targets for one signature) and `<clinit>` (5,318); the Dex frontend lowers
`invoke-direct` (constructors) to `CallStyle::DirectCall`
(`frontends/ctadl-dex/src/lib.rs:521-566`), so those are not virtual call sites at all.
Reporting them as the worst sites would be an artifact. Walking the IR avoids this by
construction — `<init>` never appears in the virtual census.

**3. Kotlin receiver-type matching is unreliable on release APKs.** Measured in three dex
string pools: `vlc` keeps `kotlin/jvm/functions/FunctionN`; `com.noto_54.apk` repackages to
`Lkotlin/FunctionN;`; `Facebook+Lite` has zero `kotlin` types at all (everything is
`LX/000;`, and only 1,016 types — it loads the rest at runtime). But `invoke` and
`invokeSuspend` rank 3rd and 5th in `vlc`'s worst-signature list by method name alone.
Decision: match **both** discriminators and report coverage, so obfuscation shows up as
data rather than as a silent zero.

**4. Two cost concerns in the spec turn out not to be real.**
`IndexFacts::try_load` reads only the seven pre-fixpoint tables, never the 190 MB
`assign.parquet` (`index_engine/mod.rs:174-238`; its doc comment at :172 is stale and says
three tables). And large apps *have* been indexed here — `vlc` 309 MB, `chrome` 75 MB — so
§6.7's worry that the resolved tier may only ever run on TaintBench is weaker than written.
The real cost is the static tier's `load_import`: `vlc` is a 159 MB program bitcode plus a
53 MB VMT, decoded whole into memory.

---

## Work, in order

Each step leaves the tree building and the existing suite green.

### 1. Extract CHA and add the RTA switch
`ctadl-ascent/src/codegen/mod.rs` → new `ctadl-ascent/src/codegen/cha.rs`.

Move `ChaLanguage` (:930), `ClassHierarchyAnalysis` (:942), `run_cha` (:1087),
`emit_callee_resolvents` (:1060) and `InstantiationFinder` (:150) into `cha.rs` with a
`pub(crate)` surface. Add an `rta: bool` parameter to `run_cha`, selecting a `cha_resolve`
rule gated on `instantiated_class(sub)` — the rule is already written and commented out at
`codegen/mod.rs:1136-1139`; enabling it needs the trailing `;` on the
`cha_subtype_reflexive` line turned into a `,`. `codegen` keeps calling with `rta: false`,
so index behaviour is unchanged.

The `instantiated_classes` set must be collected over **every** function including skipped
ones, for the reason `codegen_program` documents at `codegen/mod.rs:68-71`.

*Proves it:* `cargo test -p ctadl-ascent` (the `codegen/tests.rs` fixtures and the
serial/parallel engine-parity test at `index_engine/mod.rs:1714` both exercise this path)
and `cargo xtask regression`, both unchanged.

### 2. Statistics helpers
`ctadl-ascent/src/stats.rs`. Add `percentile(sorted: &[usize], q: f64) -> Option<usize>`, a
`Distribution { count, total, mean, p50, p90, p99, max }` with a constructor taking
`&mut [usize]`, and `top_n_share(sorted_desc: &[usize], n: usize) -> f64`. Keep the
module's style: free functions, `usize`, `median`'s sorted-slice precondition documented
rather than enforced. Note this module currently has **zero callers** anywhere in the
workspace (`cli::inspect` re-implements median inline at `cli/mod.rs:965-974`); the report
is its first.

*Proves it:* unit tests in-module, numeric only — no program fixtures.

### 3. Static tier measurements
New `ctadl-ascent/src/report/callgraph.rs`.

One walk over `program.functions` → `blocks` → `statements`, matching
`StatementKind::CallAssign { style, .. }` — the same shape `cli::inspect` uses at
`cli/mod.rs:933-961`. Per site store ids only (`FunctionIdx`, `BasicBlockIdx`,
`StatementIdx`, the `CallStyle` discriminant, and for a `JavaCall` the `(cls, simple_name,
descriptor)` symbols plus CHA and RTA counts); resolve names only for the top-N lists that
print. ~710k sites on `vlc`, so the rows are not the cost — `load_import` is.

Sections, all static: call census by `CallStyle`; CHA targets per virtual site and the
top-N; fraction resolving to exactly one; the `Distribution`; top-10 / top-100 edge share;
zero-target sites; RTA-versus-CHA targets dropped; named hard cases (`equals`, `hashCode`,
`toString`, matched exactly by name and descriptor, which survives obfuscation); Kotlin
sites by **both** discriminators (receiver type in `{kotlin/jvm/functions/FunctionN,
kotlin/FunctionN}` and method name in `{invoke, invokeSuspend}`), reported as two counts
plus their disagreement; and SCCs over the CHA call graph as the recursion *upper* bound.

Every structure derives `serde::Serialize`. RTA counts are labelled a lower bound, per
`spec.md` §6.1 — the allocated-class set comes from imported code only.

### 4. Tier selection and the resolved tier
New `ctadl-ascent/src/report/mod.rs`: `ReportOptions { format, top, sections }` in the style
of `IndexOptions` (`cli/mod.rs:46-81`), and `report(project, opts) -> Result<Report, Error>`.

Gate on `project.has_index()` before ever calling `index_path()` — `AnalysisProject::ephemeral`
documents at `ctadl-import/src/project.rs:528-533` that `index_path` creates the directory
and must not be called on an ephemeral project, and the import-only path goes through
`ephemeral`. Resolved tier loads `IndexFacts::try_load` plus `facts::IdMap::try_load` and
adds: fan-in per method; the deferred-site count from `callee_info` compared against the
static tier's CHA counts ("how much hybrid inlining had to do"); and SCCs over `call` as
the recursion *lower* bound, wrapping a dense `FunctionId`→`usize` remap in a small
`DirectedGraph + Successors` adapter for `ctadl_ir::graph::scc::Sccs<usize, usize>`
(`Idx` is implemented for `usize` at `ctadl-ir/src/index/idx.rs:30`). Do not write a new
Tarjan.

The report opens with one line naming the tier, the strategy the index used, and what
indexing would add — `spec.md` §6.6. Under `mixed`, it must state that resolved SCCs see
only monomorphic edges, so recursion through a virtual call is invisible there.

### 5. Rendering and CLI surface
New `ctadl-ascent/src/report/render.rs` (text). `ctadl-ascent/src/lib.rs` gains
`pub mod report;`. `ctadl-ascent/src/cli/mod.rs` gains a thin `pub fn report(...)`, matching
the module doc's contract at `cli/mod.rs:1-10`. `ctadl-ascent/src/main.rs` gains
`Command::Report(ReportArgs)`, a `ReportFormat` `ValueEnum`, and a `report_project` adapter
next to `query_project` (:763).

`load_or_infer_project` (`main.rs:792-810`) is private to `main.rs` and is exactly the
name-resolution `spec.md` §2 asks for; reuse it in place rather than moving it.

Output follows the existing convention, not a new one: `--output` defaults to `-` meaning
stdout, as `write_sarif` does at `query_engine/formatter.rs:1933-1956`, and the "wrote
<file>" line is suppressed for `-` so it cannot corrupt a pipe (`cli/mod.rs:564-566`).
Text and JSON both go to stdout; progress and warnings stay on stderr through `log`. Per
`docs/debugging.md:36-38`, nothing that scales with call sites may be logged above `debug`.

### 6. Record what the index cost and how it resolved
`ctadl-import/src/project.rs`: `IndexConfig` (:151) gains `strategy`, `index_seconds` and
`peak_footprint_mb`, each `Option` with `#[serde(default)]` so an existing index still
reads and `INDEX_FORMAT_VERSION` does **not** need a bump (`check_index_config` at :683
compares only `version`). `cli::index` fills them at `cli/mod.rs:315` using the existing
`phys_footprint_mb` (`index_engine/mod.rs:887`). This is the §6.4 cost block and it is what
makes the resolved tier able to state its own strategy.

`IndexStats`' `hybrid_context_*` fields have no on-disk channel at all today and are
reported under their real names as relation counts, not dressed up as code size.

### 7. Tests — real APKs only
`xtask/src/apk.rs`. Add to `CHECKS` (:45), reusing the **one shared com.noto import** the
module already pays for (~13 s, ~50k functions, two `classes*.dex`, no toolchain needed):

- `apk:report` — `ctadl report app` runs, names the static tier, and reports a nonzero call
  census.
- `apk:report-json` — `--format json` parses, carries one key per section, and the schema is
  present even for sections that found nothing.
- `apk:report-invariants` — the assertions that must hold on **any** program: RTA targets ⊆
  CHA targets per site; the sum of per-site target counts equals the reported total edges;
  direct + virtual + other equals the total call census; top-10 share ≤ top-100 share ≤ 1;
  p50 ≤ p90 ≤ p99 ≤ max; zero-target sites ≤ virtual sites.
- `apk:report-stable` — two reports over the same import are identical, which
  `docs/debugging.md:69-73` makes an assertion rather than a hope.
- A small number of aggregate counts pinned to com.noto (total call sites, virtual sites,
  total CHA edges), so a change in resolution moves them loudly. Set-level and count-level
  only — never a byte-diff of rendered output (`docs/debugging.md:113-117`).

Resolved-tier coverage needs an indexed real app. com.noto has never been indexed in CI, so
**step 7a is to measure that cost first**; if it is affordable it becomes one more `apk:*`
check sharing a single index, and if it is not, the resolved tier is covered by
`report-eval` on TaintBench instead and that is recorded as the reason.

Update `nightly/README.md`'s check table. Its §"Checks that are not taint cases" claim that
`ctadl-ascent/tests/cli.rs` reads the APK is already stale (`xtask/tests/dex/README.md:23`
says the same); fix it while there.

No new cases in `ctadl-ascent/tests/cli.rs` — its stated rule is milliseconds and synthetic,
and "a case that needs a real artifact belongs in `xtask`" (`tests/cli.rs:11-13`).

### 8. Evaluation harness
New `xtask/src/report_eval.rs`, plus a `report-eval` arm in `xtask/src/main.rs`'s hand-rolled
dispatch (:47-57) — `regression` is the only subcommand on this branch. Takes a **directory**
of APKs as an argument, never a hard-coded path (`spec.md` §6.7); quotes every path, since
`~/apps` filenames carry `+` and percent-encoding. For each app: import, `ctadl report
--format json`, save the per-app JSON, print a cross-app summary table. Reuses `xtask::exec`
(`which`, `run_checked`, `capture_stdout`, `run_with_timeout`, `fresh_dir`).

Keeping the per-app JSON is the point; the table is secondary.

Its first job is §7.2 question 1 — does the static tier survive the 213 MB TikTok XAPK.
Run that under a hard memory cap rather than watching it (the `memory-guard` skill is set up
for exactly this). Note `Facebook+Lite` is a poor scale sample despite being an APK: 1,016
types and runtime dex loading mean a static import sees almost nothing.

### 9. Correct `spec.md`
It is in-tree and now known wrong in three places: §5.2 (the `inspect_index_facts` logic is
vacuous under `mixed`, not reusable output), §5.4 (`call` gives fan-in, not fan-out; and
`callee_resolvents` is not keyed by call site), §6.2 (receiver-type matching for Kotlin
fails on obfuscated and repackaged apps). Also drop §5.6's synthetic-program unit tests in
favour of step 7.

### Deferred, unchanged from `spec.md` §8
Phase 2 (interface-vs-virtual, §6.2) needs a `dispatch` field on `CallStyle::JavaCall` and
an is-interface bit in the VMT. Both are confirmed missing: the Dex frontend collapses
`invoke-virtual`/`-super`/`-interface` to one `JavaCall` (`frontends/ctadl-dex/src/lib.rs:525-537`),
and `run_cha` is called with empty `interface_type`/`super_interface`
(`codegen/mod.rs:969-970`), making its two interface rules dead. That is an `ir-vmt.bitcode`
wire change: bump `IMPORT_FORMAT_VERSION` to `"7"` (`ctadl-import/src/project.rs:99`) **and**
the pinned copies at `xtask/src/apk.rs:62`, `ctadl-import/tests/open_import.rs:157`,
`ctadl-ascent/tests/store_relocation.rs:124`. Until then the report prints one combined
virtual figure and says interfaces are folded in.

Phase 3 (allocation depth, §6.3) needs `resolvent`'s `SmallestCallString`
(`index_engine/mod.rs:1001-1005`) promoted to an output relation and a new Parquet table.
It is internal today and only reaches a `log::trace!` and a stats counter.

---

## Files touched

**New:** `ctadl-ascent/src/report/{mod,callgraph,render}.rs`,
`ctadl-ascent/src/codegen/cha.rs`, `xtask/src/report_eval.rs`.

**Modified:** `ctadl-ascent/src/codegen/mod.rs`, `ctadl-ascent/src/stats.rs`,
`ctadl-ascent/src/lib.rs`, `ctadl-ascent/src/cli/mod.rs`, `ctadl-ascent/src/main.rs`,
`ctadl-import/src/project.rs`, `xtask/src/apk.rs`, `xtask/src/main.rs`,
`nightly/README.md`, `xtask/tests/dex/README.md`, `spec.md`.

---

## Verification

```sh
# unchanged behaviour first
cargo test --workspace
cargo xtask regression

# the new checks, on the real APK
cargo xtask regression --frontend dex --filter apk:

# by hand, static tier, on the committed fixture
ctadl import --frontend dex noto xtask/tests/dex/com.noto_54.apk
ctadl report noto                  # text, stdout, says "static tier"
ctadl report noto --format json | jq 'keys'
ctadl report noto --format json > a.json && ctadl report noto --format json > b.json && diff a.json b.json

# resolved tier, against an index that already exists
ctadl report vlc | head -40        # expect fan-in max ~25417, and the mixed-strategy caveat

# scale, under a cap
cargo xtask report-eval --apks ~/apps     # memory-guard the TikTok case
```

What each answers: the first block is "nothing regressed"; `apk:` is the invariant and
pinned-count suite; the `diff` is reproducibility; `vlc` is the only resolved-tier check
available before step 7a settles; `report-eval` is §7.2 question 1, the make-or-break
number.

## Live risks

1. **Static tier memory on the largest apps.** `load_import` decodes the whole program;
   `vlc` is 159 MB of bitcode plus a 53 MB VMT and TikTok is larger. This is measured in
   step 8, and it is the one result that could force `--section` to become a real cost
   lever rather than a convenience. Highest-risk step.
2. **Step 1 touches the shared CHA path.** Moving `run_cha` and adding a parameter is
   mechanical, but `codegen` is on the index hot path. Mitigated by keeping `rta: false`
   for codegen and by the existing engine-parity test.
3. **RTA is a lower bound**, and will drop genuinely reachable targets (library,
   reflection, deserialization allocations are invisible). It must be labelled everywhere it
   appears, or it will be read as precision rather than as a measurement.
4. **Pinned com.noto counts are a maintenance cost.** They are the only thing that catches a
   silent resolution regression, so they stay — but they need a comment saying what to do
   when the frontend legitimately changes them.
