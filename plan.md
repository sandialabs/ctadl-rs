# `ctadl report` — implementation plan, phase 1 - DO-NOT-MERGE

Status: approved, ready to implement. Derived from `intent.md` and `spec.md`, corrected against
measurements of this tree and of the indexes in `~/.local/state/ctadl`, then simplified after
review (see "What review changed").

## Context

`intent.md` asks a planning question, not a bug-finding one: **is it worth making call
resolution more precise, and where?** The concrete sub-question is whether a handful of
terrible call sites explain most of the call-graph imprecision — if so, special-case them;
if not, the whole analysis has to get better. Nothing in CTADL answers this today. The
numbers that exist are buried in `log::debug!` inside `cli::inspect_index_facts`.

`spec.md` designs a `ctadl report <name>` subcommand at two tiers (static = needs only an
import; resolved = needs an index). This plan implements phase 1 as **static tier only**.

### What measurement changed about the design

**1. `call.parquet` is monomorphic-only, and the resolved tier adds nothing in phase 1.**
Default strategy is `CallResolutionStrategy::Mixed` (`main.rs:274`). Under it codegen
(`codegen/mod.rs:535-563`) emits a `call` edge when CHA resolves to exactly one target,
pushes `callee_info` when it resolves to two or more, and **silently drops** a site with zero
targets — no edge, no `callee_info`, only a `log::trace!`. Measured on the existing `vlc`
index: 570,387 sites in `call` (all with exactly one target), 139,695 in `callee_info`,
overlap 0.

So everything the index could tell the report is derivable from the import: the deferred-site
count is "sites with CHA >= 2", fan-in over `call` is fan-in over the monomorphic subset, and
SCCs over `call` cannot see recursion through any virtual call. Fan-in and SCCs over the
**CHA graph** are the numbers hybrid inlining actually has to contend with, and they need no
index. **Consequence:** the resolved tier moves to phase 3, where the index genuinely adds
something (allocation depth, `spec.md` §6.3). `IndexConfig` is untouched in phase 1.

**2. CHA is keyed by signature, not by call site.** `ClassHierarchyAnalysis::resolvents` is
`BTreeMap<(class, name, descriptor), targets>` (`codegen/mod.rs:942-946`), so every
`Object.equals` site has the same target count and a per-site top-10 would be ten copies of
one row. The report therefore aggregates by **signature key** and carries the number of sites
per key; every per-site distribution is the weighted expansion of the per-key table. This is
also what makes the report cheap: the walk holds one entry per distinct key, not one per
site. (`callee_resolvents.parquet` is keyed `(object, context, target_id)` with no call site
and its top entries are `<init>`/`<clinit>`, which are `DirectCall` on Dex
(`frontends/ctadl-dex/src/lib.rs:521-566`) — another reason to walk the IR, not the index.)

**3. Kotlin receiver-type matching is unreliable on release APKs.** In three dex string pools:
`vlc` keeps `kotlin/jvm/functions/FunctionN`; `com.noto_54.apk` repackages to
`Lkotlin/FunctionN;`; `Facebook+Lite` has zero `kotlin` types (everything is `LX/000;`,
1,016 types, rest loaded at runtime). But `invoke`/`invokeSuspend` rank 3rd and 5th in
`vlc`'s worst-signature list by name alone. Decision: match **both** discriminators and
report their disagreement, so obfuscation shows up as data rather than a silent zero.

**4. The real cost is `load_import`, not the index tables.** `IndexFacts::try_load` reads only
the seven pre-fixpoint tables (`index_engine/mod.rs:174-238`). `vlc` is a 159 MB program
bitcode plus a 53 MB VMT decoded whole into memory, and TikTok is larger. That is the
make-or-break number and it is measured **first**, with existing commands.

### What review changed

- Resolved tier, `IndexConfig` cost fields, and the index-cost probe: out of phase 1.
- Top-N and all storage by signature key, not call site.
- Scale measurement moved from last to first, using `ctadl inspect`, before new code.
- One synthetic CHA/RTA correctness test added next to `run_cha`.
- No `codegen/cha.rs` move; RTA computed in the same Datalog run as CHA.
- xtask checks reduced from five to two; `--section` dropped pending step 0.

### What implementing it changed

Five things. The first was found by step 0, before any code, which is what step 0 was for.

1. **"The report takes its first import" is wrong** (step 4). An `.xapk` imports as one
   program per split APK plus an **empty parent**, and `ephemeral()` puts the parent first.
   TikTok's parent has 0 functions and its `com.zhiliaoapp.musically` split has 1,868,340.
   The report measures **every non-empty import** instead, one CHA per program, and names
   the empty ones rather than dropping them.
2. **`--section` is out, `--no-recursion` is in** (step 4). Step 0's rule was to add a
   section selector only if the import is *not* the dominant cost. It is not: the import
   decodes TikTok in 12 s and 9.7 GiB, and the whole report takes 89 s and 24.9 GiB. But the
   addition is one section, not many -- recursion materializes the CHA graph, 1.21 *billion*
   deduplicated edges on that app, and skipping it gives 55 s and 24.1 GiB. So the flag buys
   38% of the wall time and almost no memory: the peak is set earlier, inside `run_cha`,
   whose relations are freed before the graph is built. A selector over the cheap sections
   would have been a menu.
3. **Fan-in does not need the graph** (step 3, sections 7 and 8). It counts *sites*, so it
   comes from the weighted signature table directly. Only recursion builds the graph, which
   is what makes gating exactly that section possible.
4. **`top_n_share` is weighted too** (step 2). The plan specified `&[usize]`, but the ten
   worst *sites* in a real program routinely share one signature, so the helper has to be
   able to take an entry partially. Same `(value, weight)` shape as its neighbours.
5. **The Kotlin measurement is right but the reasoning behind it was not.** The plan
   expected `com.noto` to repackage the receiver type *to* `Lkotlin/FunctionN;`. Its type
   pool does contain those names -- but no call site dispatches on one, because the
   obfuscator repackaged the interfaces that actually carry `invoke` (its worst signatures,
   `Lu7/p;.R(...)` and `Lu7/l;.U(...)`, *are* those interfaces) and renamed their methods
   too. So the type test finds zero there and the name test's 141 sites are a floor. On
   TikTok, where `kotlin/jvm/functions/FunctionN` does survive, the type test still
   undercounts: 31,622 against 57,383 by name. Matching both and reporting the disagreement,
   as planned, is what makes either failure visible; the text output now says so outright.

6. **A share by signature was added beside the share by site, over the excess rather than
   the total** (step 3, section 3). The intent asks what the top 10 or 100 *sites* own "to
   justify special-casing them", and measured that is 0.06%-1.6% -- necessarily so, at ten
   instructions out of millions. But nobody special-cases a call instruction; they
   special-case `Object.equals`. Counted by signature the answer inverts: ten signatures own
   **91.3%** of `com.noto`'s excess where ten sites own 0.4%.

   The *excess* part was the second correction, and it came from reading a TaintBench report.
   Ranking signatures by raw edge count puts `StringBuilder.append` -- one target, hundreds
   of sites -- at the top of every small app, and nothing about a monomorphic signature can
   be special-cased. Every resolved site needs one edge; that is the call, not the
   imprecision. So the ranking and the denominator are both `sites x (targets - 1)`.

---

## Work, in order

Each step leaves the tree building and the existing suite green.

### 0. Measure the static tier's floor with existing commands
No new code. For the four largest entries in `~/apps` (TikTok 213 MB xapk, Telegram, VLC,
WhatsApp Business) run `ctadl import --frontend dex` then `ctadl inspect`, each under the
`memory-guard` skill's `memguard.sh` with a hard cap, timing and peak footprint recorded.
Per `CLAUDE.md`, capture every run's full output to a file (under `/Volumes/Shampoo` if the
scratch dir is short on space); quote every path — the filenames carry `+` and
percent-encoding.

`inspect` calls `load_import` on the whole program (`cli/mod.rs:935`), so its peak is the
report's floor. This answers `spec.md` §7.2 question 1 before a line is written, and decides:
whether `.xapk` import works at all, the RAM budget for step 3's graph, and whether
`--section` is needed (it is not in this plan; add it only if this step says the import
is *not* the dominant cost). `Facebook+Lite` is excluded as a scale sample — 1,016 types and
runtime dex loading.

*Proves it:* a table in the step's output file: app, import seconds, inspect seconds, peak MB.

### 1. RTA inside `run_cha`, in place
`ctadl-ascent/src/codegen/mod.rs`. No module move. Make `ClassHierarchyAnalysis` (:942),
`run_cha` (:1087) and `InstantiationFinder` (:150) `pub(crate)`; `report/` is in the same
crate. Add `rta: bool` to `run_cha` and a second output relation:

```
rta_resolve(sup, m, d, id) <--
    cha_super_method(sub, m, d, id),
    cha_subtype_reflexive(sub, sup),
    instantiated_class(sub),
    if rta;
```

`run_cha` is `ascent_run!` (:1094), which captures locals, so `if rta` costs nothing when
false. It returns both maps; `ClassHierarchyAnalysis` gains `rta_resolvents` (empty unless
asked). One Datalog run shares the subtype closure — no second CHA. `codegen` keeps calling
with `rta: false`, so index output is byte-identical.

`instantiated_classes` must be collected over **every** function including skipped ones, for
the reason documented at `codegen/mod.rs:68-71`.

*Proves it:* one new test in `codegen/tests.rs`, built like its neighbours with the mir
builder and an explicit `VirtualMethodTable::Java`: interface `I` with three implementers
`A`, `B`, `C`, one function that does `new A` and calls `I.m()`. Assert CHA gives 3 targets
and RTA gives 1. Plus `cargo test -p ctadl-ascent` and `cargo xtask regression`, unchanged
(the engine-parity test at `index_engine/mod.rs:1714` covers the codegen path).

### 2. Statistics helpers
`ctadl-ascent/src/stats.rs`. Because storage is per key, the helpers are **weighted**:
`percentile(sorted: &[(usize, usize)], q: f64) -> Option<usize>` over `(value, weight)`
pairs, `Distribution { count, total, mean, p50, p90, p99, max }` with
`from_weighted(&mut [(usize, usize)])`, and `top_n_share(sorted_desc: &[usize], n) -> f64`.
Keep the module's style: free functions, `usize`, sorted-slice precondition documented. This
module currently has zero callers in the workspace (`cli::inspect` re-implements median
inline at `cli/mod.rs:965-974`); the report is its first.

*Proves it:* unit tests in-module, numeric only. Include a weighted case where the same
answer is checked against the expanded unweighted vector.

### 3. Static measurements
New `ctadl-ascent/src/report/callgraph.rs`. One walk over `program.functions` → `blocks` →
`statements` matching `StatementKind::CallAssign { style, .. }` — the shape `cli::inspect`
uses at `cli/mod.rs:933-961`. It produces:

- a census by `CallStyle` discriminant (direct / Java virtual / func-ptr / Lua / unknown);
- `HashMap<(cls, name, descriptor), usize>`: sites per Java signature key;
- per function, the set of distinct keys it calls (for fan-in and SCCs).

Every other number is a join of that map with `cha.resolvents` and `cha.rta_resolvents`.
Sections, all static, all `serde::Serialize`:

1. Call census by kind. `invoke-super` is folded into `JavaCall` by the frontend and the
   report says so.
2. Targets per virtual site: the weighted `Distribution` (p50/p90/p99/max), fraction with
   exactly one target, zero-target sites (which codegen drops silently — the unsound spot),
   and the count with >= 2 targets, labelled "handed to hybrid inlining under `mixed`".
3. Top-N **signature keys** by CHA target count, each with its site count and RTA count.
   Edge share of the top-10 and top-100 keys.
4. RTA versus CHA: total targets dropped, and the distribution of the per-key gap. RTA is
   labelled a lower bound everywhere it prints (`spec.md` §6.1: allocated-class set comes
   from imported code only).
5. Named hard cases: `equals`, `hashCode`, `toString` matched exactly by name and
   descriptor, which survives obfuscation.
6. Kotlin lambdas by both discriminators — receiver type in `{kotlin/jvm/functions/FunctionN,
   kotlin/FunctionN}` and method name in `{invoke, invokeSuspend}` — as two counts plus their
   disagreement, and how many of those keys resolve to a single body.
7. Fan-in per method over the CHA graph: for each key, each target gains that key's site
   count. Distribution plus top-N methods.
8. Recursion: SCCs over the CHA call graph, edges deduped per (caller function, target),
   via `ctadl_ir::graph::scc::Sccs` (`ctadl-ir/src/graph/scc/mod.rs:120`) behind a small
   `DirectedGraph + Successors` adapter over a dense `usize` remap. Do not write a new
   Tarjan. This is the recursion **upper** bound and the one inlining faces. Print the
   deduped edge count *before* building the graph; it can reach tens of millions on `vlc`
   and is the one section whose cost step 6 must record separately.

Non-Java programs (Pcode, C) get sections 1 and the func-ptr count only, with the rest
absent from the output rather than zero (`spec.md` §5.5). Lua gets the same treatment as Java
through `lua_resolvents_by_method`.

A `debug_assert!(rta.len() <= cha.len())` sits where the two maps are joined; that is the
per-key invariant, checked on every key, which no output-level test could do.

### 4. Command surface
New `ctadl-ascent/src/report/mod.rs` (`ReportOptions { format, top }` in the style of
`IndexOptions`, `cli/mod.rs:46-81`; `report(import, opts) -> Result<Report, Error>`) and
`report/render.rs` (text). `lib.rs` gains `pub mod report;`; `cli/mod.rs` a thin
`pub fn report(...)` per the module contract at `cli/mod.rs:1-10`; `main.rs`
`Command::Report(ReportArgs)`, a `ReportFormat` `ValueEnum`, and a `report_project` adapter
next to `query_project` (:763).

Name resolution reuses `load_or_infer_project` (`main.rs:800-810`) in place. If the name is
a project, the report takes its first import and ignores the index; the opening line names the
static tier and says an index is not consulted in this version. Never call `index_path()` —
on an ephemeral project it creates the directory (`ctadl-import/src/project.rs:528-533`).

No `--section` flag unless step 0 demands it. `--output` defaults to `-` (stdout) as
`write_sarif` does (`query_engine/formatter.rs:1933-1956`), with the "wrote <file>" line
suppressed for `-` (`cli/mod.rs:564-566`). Text and JSON both to stdout; progress on stderr
through `log`. Per `docs/debugging.md:36-38`, nothing that scales with call sites is logged
above `debug`. JSON is one key per section, absent keys for sections that do not apply.

### 5. Tests — the real APK, two checks
`xtask/src/apk.rs`, added to `CHECKS` (:45), sharing the one com.noto import the module
already pays for (~13 s, ~50k functions, two `classes*.dex`):

- `apk:report` — `ctadl report app` runs, its first line names the static tier, `--format json`
  parses with one key per applicable section, and a handful of aggregate counts pinned to
  com.noto (total sites, virtual sites, total CHA edges, total RTA edges) with a comment
  saying what to do when the frontend legitimately moves them.
- `apk:report-invariants` — from the JSON: direct + virtual + other == total; sum of per-key
  `sites × cha` == total edges; RTA edges <= CHA edges; top-10 share <= top-100 share <= 1;
  p50 <= p90 <= p99 <= max; zero-target + one-target + multi == virtual sites. Then run the
  JSON form a second time and assert the two outputs are identical
  (`docs/debugging.md:69-73`).

Count-level and set-level only, never a byte-diff of the text rendering
(`docs/debugging.md:113-117`). No new cases in `ctadl-ascent/tests/cli.rs` (its rule at
:11-13: milliseconds, synthetic, real artifacts belong in `xtask`).

Update `nightly/README.md`'s check table; its claim that `tests/cli.rs` reads the APK is
already stale (`xtask/tests/dex/README.md:23` says the same) — fix it while there.

### 6. Evaluation harness
New `xtask/src/report_eval.rs` plus a `report-eval` arm in `xtask/src/main.rs`'s dispatch
(:47-57). Takes a **directory** of APKs, never a hard-coded path (`spec.md` §6.7). For each
app: import, `ctadl report --format json --output <app>.json`, record wall time and peak
footprint per phase, and print a cross-app summary table. Reuses `xtask::exec` (`which`,
`run_checked`, `capture_stdout`, `run_with_timeout`, `fresh_dir`). All output captured to
files. Keeping the per-app JSON is the point; the table is secondary.

With step 0 having settled whether the import fits, this step's job is `spec.md` §7.2
questions 2–4 on both corpora: stability, whether the distributions have the predicted long
tail, and how concentrated the imprecision is. It also records the SCC section's cost
separately (live risk 2).

### 7. Correct `spec.md`
Known wrong or stale after this plan: §4 table (sections 9–10 need only an import; 14 stays
phase 3), §5.2 (the `inspect_index_facts` logic is vacuous under `mixed`), §5.3 (per-key
aggregation, one CHA run), §5.4 (the resolved tier is phase 3; `call` gives fan-in on
monomorphic edges only; `callee_resolvents` is not per site), §5.6 (tests are one synthetic
`codegen` case plus `xtask`), §6.2 (Kotlin receiver-type matching fails on obfuscated and
repackaged apps), §8 (phasing).

### Deferred
**Phase 2** (interface-vs-virtual, `spec.md` §6.2): a `dispatch` field on
`CallStyle::JavaCall` and an is-interface bit in the VMT. Confirmed missing: the Dex frontend
collapses `invoke-virtual`/`-super`/`-interface` into one `JavaCall`
(`frontends/ctadl-dex/src/lib.rs:525-537`), and `run_cha` is called with empty
`interface_type`/`super_interface` (`codegen/mod.rs:969-970`), so its interface rules are
dead. That is an `ir-vmt.bitcode` wire change: bump `IMPORT_FORMAT_VERSION` to `"7"`
(`ctadl-import/src/project.rs:99`) **and** the pinned copies at `xtask/src/apk.rs:62`,
`ctadl-import/tests/open_import.rs:157`, `ctadl-ascent/tests/store_relocation.rs:124`.
Until then the report prints one combined virtual figure and says interfaces are folded in.

**Phase 3** (the resolved tier): allocation depth needs `resolvent`'s `SmallestCallString`
(`index_engine/mod.rs:1001-1005`) promoted to an output relation and a new Parquet table.
Alongside it: fan-in over the real `call` graph via `IndexFacts::try_load` +
`facts::IdMap::try_load`, gated on `project.has_index()`; `IndexConfig` (:151) gaining
`strategy`, `index_seconds`, `peak_footprint_mb` as `#[serde(default)]` options (no
`INDEX_FORMAT_VERSION` bump — `check_index_config` at :683 compares only `version`), filled
by `cli::index` from `phys_footprint_mb` (`index_engine/mod.rs:887`); and a first
measurement of what indexing com.noto costs, to decide whether a resolved-tier `apk:` check
is affordable. `IndexStats`' `hybrid_context_*` counts are reported under their real names,
not as code size.

---

## Files touched

**New:** `ctadl-ascent/src/report/{mod,callgraph,render}.rs`, `xtask/src/report_eval.rs`.

**Modified:** `ctadl-ascent/src/codegen/mod.rs`, `ctadl-ascent/src/codegen/tests.rs`,
`ctadl-ascent/src/stats.rs`, `ctadl-ascent/src/lib.rs`, `ctadl-ascent/src/cli/mod.rs`,
`ctadl-ascent/src/main.rs`, `xtask/src/apk.rs`, `xtask/src/main.rs`, `nightly/README.md`,
`xtask/tests/dex/README.md`, `spec.md`.

Not touched in phase 1: `ctadl-import/src/project.rs`, anything under `index_engine/`,
`facts/`.

---

## Verification

```sh
# step 0, before any code — output captured to files, run under memguard.sh
ctadl import --frontend dex tiktok "$HOME/apps/TikTok+-+Videos%2C+Shop+%26+LIVE_46.1.3_APKPure.xapk"
ctadl inspect tiktok

# unchanged behaviour
cargo test --workspace            # includes the new codegen CHA=3/RTA=1 case
cargo xtask regression

# the new checks, on the real APK
cargo xtask regression --frontend dex --filter apk:

# by hand, on the committed fixture
ctadl import --frontend dex noto xtask/tests/dex/com.noto_54.apk
ctadl report noto                          # text; first line names the static tier
ctadl report noto --format json | jq 'keys'
ctadl report noto --format json > a.json && ctadl report noto --format json > b.json && diff a.json b.json

# the corpus
cargo xtask report-eval --apks ~/apps      # per-app JSON + table, outputs to files
```

What each answers: step 0 is the make-or-break memory number; the first block is "nothing
regressed" and "RTA is right on a case you can check by hand"; `apk:` is invariants, pinned
counts and reproducibility; `report-eval` is `spec.md` §7.2 questions 2–4.

## Live risks

1. **`load_import` memory on the largest apps.** Now measured in step 0 before any code, so
   it shapes the design instead of being discovered at the end.
2. **CHA graph size for fan-in and SCCs** (section 8). Deduped per (function, target) but still
   potentially tens of millions of edges on `vlc`. The edge count prints first, and if step 6
   shows this section dominates, it becomes opt-in.
3. **Step 1 touches the shared CHA path.** Mitigated by `rta: false` in codegen, the `if rta`
   guard, the engine-parity test, and byte-identical index output.
4. **RTA is a lower bound** and must be labelled so everywhere it prints, or it reads as
   precision rather than measurement.
5. **Pinned com.noto counts are a maintenance cost.** They are the only thing that catches a
   silent resolution regression, so they stay, with a comment saying how to re-pin.
