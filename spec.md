# `ctadl report` — requirements and design - DO-NOT-MERGE

Status: draft, for review. Derived from `intent.md`.

## 1. What this is

`ctadl report` is a new subcommand that measures a program CTADL has already read in
and prints what it found. The first version measures the **call graph**: how many
calls there are, how many places each virtual call could go, and where the
imprecision is concentrated.

The point is not to find bugs. The point is to answer a planning question: *is it
worth making call resolution more precise, and where?* Every number in the report
should be traceable to a decision — special-case the worst 10 call sites, or push
hybrid inlining one frame deeper, or leave it alone.

## 2. Where the data lives

This decides most of the design, so it comes first.

CTADL runs in two stages that matter here:

**Import** (`ctadl import`) writes an IR program and a virtual method table (VMT) to
the store. From these alone we can:

- walk every call site and see its kind (`CallStyle::DirectCall`, `JavaCall`,
  `FuncPtrCall`, `LuaCall`, `Unknown`);
- for a `JavaCall`, read the declared receiver class, method name, and descriptor;
- rebuild the class hierarchy and run CHA (`codegen::run_cha`), so we can count
  the possible targets of each virtual call.

That is enough for most of the intended measurements, and it costs one CHA run — far
less than indexing.

**Index** (`ctadl index`) runs the analysis and saves several tables as Parquet under
the project's `index/` directory. The ones we need are `call` (the resolved call
graph), `callee_info` (virtual sites left for the analysis to resolve),
`callee_resolvents` (the CHA table), `call_target_assign` (which objects are stored
where), and `function_id` (the id-to-name map). `docs/debugging.md` records that all
of these are written *before* the fixpoint and are byte-stable across runs, so a
report built on them is reproducible.

**Design consequence.** The report runs at one of two tiers and says which one it
used:

| Tier | Needs | Gives |
| --- | --- | --- |
| Static | An import | Call census, CHA and RTA target counts, distributions, hot spots |
| Resolved | An index | The above, plus the graph the analysis actually built: fan-in, recursion, hybrid-inlining depth |

If a section needs the index and there is none, the report prints a one-line note
saying so and carries on. It does not fail. This matches what `ctadl query` already
does when a model file is checked before indexing.

`intent.md` says the command "takes an import name". We keep that, and resolve the
name the way `query` does today (`load_or_infer_project`): try the project first, fall
back to the import of the same name. One name, both tiers, no new naming rules.

## 3. Command surface

```
ctadl report <name> [--output FILE] [--format text|json] [--top N] [--section SECTION]...
```

- `<name>` — a project name or an import name (see above).
- `--format` — `text` (default, for reading) or `json` (for tracking numbers over
  time). Text goes to stdout, as `docs/debugging.md` requires; progress and warnings
  stay on stderr.
- `--output` — write to a file instead of stdout.
- `--top N` — how many worst call sites to list. Default 10, per `intent.md`.
- `--section` — repeatable; restrict the report to named sections. Default: all.

The JSON form is a stable object with one key per section, so a nightly job can diff
two runs without parsing prose. The text form is the same data rendered for a human.

## 4. What gets measured

Sections, in report order. The "Needs" column says what has to exist before the
number is real; anything past "Import" is called out in §6.

| # | Measurement (from `intent.md`) | Needs | Phase |
| --- | --- | --- | --- |
| 1 | Call sites, split into direct and indirect/virtual | Import | 1 |
| 2 | CHA target count per virtual call site; top-N worst sites | Import | 1 |
| 3 | Fraction of virtual sites with exactly one target | Import | 1 |
| 4 | Full distribution of targets per site: median, p90, p99, max | Import | 1 |
| 5 | Share of all call edges owned by the top 10 / top 100 sites | Import | 1 |
| 6 | Sites resolving to zero targets | Import | 1 |
| 7 | Targets dropped when restricting to allocated types (RTA vs CHA) | Import | 1 (see §6.1) |
| 8 | Named hard cases: `equals`/`hashCode`/`toString` and friends | Import | 1 |
| 9 | Fan-out per site and fan-in per method | Import (CHA graph) / Index (real graph) | 1 |
| 10 | Recursion and strongly connected components | Index | 1 |
| 11 | Kotlin lambda call sites, and how many reach a single body | Import | 1 (see §6.2) |
| 12 | Interface calls kept separate from class-virtual calls | **New IR field** | 2 (see §6.2) |
| 13 | How many call frames away the receiver's allocation is | **New index output** | 3 (see §6.3) |
| 14 | Analysis time, peak memory, inlined-code growth | Partly available | 3 (see §6.4) |

Definitions we will state in the report itself, because they are easy to argue about:

- A **call site** is one call instruction in one function.
- A **direct call** is one whose target is named in the instruction. On Dex this is
  `invoke-static` and `invoke-direct`. Note that `invoke-super` is lowered to a
  `JavaCall` today and so counts as virtual; the report will show it as its own row
  so the number is not silently wrong.
- A **virtual call** is one whose target depends on the receiver's runtime type.
- A **target** (or resolvent) is one method a virtual call could reach under the
  named resolution strategy.
- An **edge** is one (call site, target) pair. Total edges is the sum of target
  counts, which is what a client of the call graph actually pays for.

## 5. Design

### 5.1 Code layout

New module `ctadl-ascent/src/report/`:

- `mod.rs` — the entry point `report(project_or_import, options)`, tier selection,
  section dispatch.
- `callgraph.rs` — the measurements themselves. Takes a program, a VMT, and
  optionally loaded index facts; returns a plain data structure.
- `render.rs` — text rendering.
- The data structure derives `serde::Serialize` for `--format json`.

`ctadl-ascent/src/cli/mod.rs` gets a thin `pub fn report(...)` wrapper, matching how
`index`, `query`, and `inspect` are exposed. `main.rs` gets `Command::Report` and a
`ReportArgs` struct. No other command changes.

### 5.2 Reuse, not reinvention

Three pieces already exist and should be used rather than duplicated:

- **CHA.** `codegen::run_cha` and `ClassHierarchyAnalysis` already compute exactly the
  target sets we want to count. They are private to `codegen`. Move them into
  `codegen::cha` with a `pub(crate)` surface and add an `rta: bool` parameter (§6.1).
  Codegen's behaviour must not change: it keeps calling with `rta: false`.
- **Statistics.** `ctadl-ascent/src/stats.rs` has `median`, `quartiles`, and `modes`.
  Add `percentile`, a `Distribution` summary (count, mean, p50, p90, p99, max), and a
  `top_n_share` helper. Same module, same style.
- **SCC.** `ctadl_ir::graph::scc::Sccs` is generic over a `Successors` graph. Wrap the
  resolved call graph in a small adapter and use it for section 10. Do not write a new
  Tarjan.

There is also existing code that this command supersedes: `cli::inspect_index_facts`
already computes the top-50 busiest call sites and a target-count distribution, but
only at `log::debug!`. That logic moves into `report/callgraph.rs` and becomes real
output. `inspect_index_facts` keeps its other debug dumps.

### 5.3 How the static tier computes a report

1. Load the import (and its sub-imports, e.g. an APK's native libraries) with
   `load_import`, the same call `inspect` and `dump_ir` make.
2. Collect allocated classes with the existing `InstantiationFinder`.
3. Run CHA twice: once plain, once with the RTA restriction. Two runs of the same
   Datalog on the same inputs; the cost is roughly double one CHA, which is small.
4. Walk every function and every statement. For each `CallAssign`, record: the
   function, the instruction index, the call kind, and — for a `JavaCall` — the
   declared class, name, descriptor, CHA target count, and RTA target count.
5. Aggregate into the sections above.

Step 4 produces one row per call site. On a large APK that is millions of rows if
stored naively, so the row is kept small (ids, not strings) and names are resolved
only for the top-N lists that get printed.

### 5.4 How the resolved tier adds to it

Load the index tables with `IndexFacts::try_load(project.index_path())`. This reads
Parquet; it does not re-run the analysis. From `call` we get the resolved graph
(fan-in, fan-out, SCCs); from `callee_info` we get the sites the analysis deferred to
hybrid inlining; from `callee_resolvents` we get the CHA table as the index saw it.
Comparing the deferred sites against the static tier's CHA counts is the direct
answer to "how much did hybrid inlining have to do".

### 5.5 Non-Java languages

The measurements are written against the Java/Dex model because that is what the
intent asks for. Lua has a comparable virtual-dispatch story (`LuaCall`, its own CHA
arm) and gets the same treatment where it is meaningful. Pcode and C have no class
hierarchy: for them the report prints the call census and the indirect-call count and
skips the type-resolution sections. It must not print zeros that look like findings.

### 5.6 Testing

- Unit tests in `report/` on small synthetic programs built with
  `ctadl_ir::mir::builder`: a class hierarchy with a known number of overrides, so the
  CHA and RTA counts are checkable by hand.
- A test in `ctadl-ascent/tests/cli.rs` following that file's rules — synthetic input,
  temp store, milliseconds — asserting that a report on a tiny C import runs, produces
  valid JSON, and reports the static tier.
- An `xtask` case on a real Dex fixture (`xtask/tests/dex/`) asserting stable
  aggregate counts. Assert set-level and count-level properties only; per
  `docs/debugging.md`, never diff rendered output byte-wise.

Tests stay fast and synthetic. Measuring the command on real apps is a separate
activity with its own harness; see §7.

## 6. Areas of concern

These are the parts where the intent asks for something the codebase does not yet
support. Each says what is missing and what we propose.

### 6.1 RTA does not exist yet

`intent.md` asks for CHA **and** RTA counts, and for the number of targets RTA throws
away. CTADL has no RTA today. The rule is written and commented out in
`codegen::run_cha`, and the input it needs — the set of classes the program actually
allocates — is already computed and already passed in.

Turning it on for the report is a small change. Two cautions:

- The set of allocated classes comes from `new`-like expressions in *imported* code
  only. Objects created by library code we did not import, by reflection, or by
  deserialization are invisible. RTA will therefore drop targets that are genuinely
  reachable. The RTA number is a *lower bound* and the report must label it that way.
- We are not proposing to change how `ctadl index` resolves calls. The RTA count is a
  measurement, not a new strategy. If it later looks good, that is a separate change
  with its own soundness argument.

### 6.2 Interface calls cannot be told apart from class-virtual calls

`intent.md` is explicit that these must never be averaged together, and this is the
one requirement we cannot meet without changing the IR.

Two facts are lost at import:

- The Dex frontend maps `invoke-virtual`, `invoke-super`, and `invoke-interface` all
  to `CallStyle::JavaCall`, keeping no record of which one it was
  (`frontends/ctadl-dex/src/lib.rs`).
- The VMT's `hierarchy` map merges a class's superclass and its interfaces into one
  parent list, and `run_cha` is called with an empty `interface_type` set, so nothing
  downstream knows which parents are interfaces.

Proposed fix, in phase 2: add a small `dispatch` field to `CallStyle::JavaCall`
(`Virtual` / `Interface` / `Super`) and record whether each class is an interface in
the VMT. Both are additive; both change the IR encoding, which means a bump of
`IMPORT_FORMAT_VERSION` and a re-import of anything already in the store. The
alternative — guessing from the class name — is not worth doing.

Until then the report prints one combined virtual-call figure and states plainly that
interfaces are folded in.

The same gap limits one part of section 11. Kotlin needs nothing special from CTADL —
it compiles to the JVM and Dex bytecode the frontends already read — and a Kotlin
lambda call site is recognisable from what an import already carries: the declared
receiver type is `kotlin/jvm/functions/FunctionN`, and its target count comes from
CHA like any other virtual call. That is phase 1 work against a short, documented
list of type names, and the report states which names it matched so a stale list is
visible rather than silent. What has to wait is the *general* case — any
single-abstract-method interface, not just Kotlin's — because finding those means
knowing which types are interfaces and which of their methods are abstract, which is
exactly the data missing above. The `equals`/`hashCode` case (section 8) needs none of
this; those are matched exactly by name and descriptor.

### 6.3 Allocation depth needs a new index output

"How many call frames away the allocation is" maps precisely onto something CTADL
already computes: hybrid inlining carries a call string while it propagates objects
back through call arguments (`resolvent` and `call_arg_resolvent` in
`index_engine/mod.rs`), and the length of that call string is exactly the depth we
want. The problem is that these are internal relations, discarded when the fixpoint
finishes.

Getting the number means adding an output relation and a new Parquet table, i.e.
touching the index engine and its saved schema. That is real work and real risk on
the hot path, so it is phase 3, after the cheaper measurements have shown whether
this one is worth it.

This section no longer depends on classifying every receiver by origin, which is no
longer asked for. It reports only on receivers hybrid inlining actually traced back
to an allocation, so the denominator is "receivers we resolved this way", not "all
virtual call sites". The report must say so, or the depth histogram will read as
covering more sites than it does.

### 6.4 Cost numbers are only partly available

Analysis time and peak memory are straightforward: the codebase already has
`phys_footprint_mb`, and the report can time its own phases. "Inlined-code size
growth" is harder — `IndexStats` carries `hybrid_context_assign`,
`hybrid_context_locals`, and `hybrid_context_summary`, which are the closest thing to
a size-of-inlining measure, but they count relation rows, not code. We propose to
report those rows under their real names and not dress them up as code size.

Note also that these numbers describe *an index run*, not the report run. They can
only be reported for an index the report did not perform. Either the index writes
them into its config for the report to read, or the report says nothing about them.
We prefer the first; it is a small addition to `index_config.json`.

### 6.5 Cost of the report itself

A report on a large APK walks every statement of every function and holds one row per
call site. That is affordable, but it is not free, and the static tier also runs CHA a
second time for RTA. We should measure the report's own footprint on a real APK before
calling phase 1 done, and make `--section` a real way to pay for only what you asked
for.

### 6.6 The name in `intent.md` says "import", the data mostly wants an index

Most of the more interesting measurements — fan-in on the real graph, recursion,
inlining depth — need an index. Resolving one name to either an import or a
project (§3) keeps the command simple, but users will run it on an import, see half
the sections marked "needs an index", and be surprised. The report must open with a
clear line naming the tier it ran at and what would be added by indexing first.

### 6.7 The two evaluation corpora are not equal

`intent.md` names two bodies of apps to evaluate on, and they are good for different
things — see §7. Two limits are worth stating up front.

TaintBench is reproducible and CI-friendly: 38 small APKs fetched by URL and SHA-256
hash, never committed. The large apps in `~/apps` are neither. They are one machine's
local files, with no manifest, no hashes, and no clear licence to redistribute, so no
check can depend on them and no result from them is reproducible by someone else. The
harness must therefore take a directory of APKs as an argument rather than hard-coding
a path, and any numbers we quote from `~/apps` must name the exact file and version
they came from.

The second limit is that the resolved tier may only ever be evaluated on TaintBench.
Indexing a 200 MB app is a much larger undertaking than reporting on it, and it may
simply not finish. If that turns out to be so, it is a finding rather than a failure:
it is the argument for the static tier existing at all.

## 7. Evaluation

### 7.1 The corpora

**TaintBench** — 38 real Android malware APKs, small, under
`../ct-taintbench/taintbench/apps/`. Each app directory carries its APK coordinates
(`app.json`: URL plus SRI hash), a query model, ground-truth findings, and a baseline.
The APKs are fetched by Nix as fixed-output derivations (`nix/taintbench.nix`) and the
suite runs as `cargo xtask taintbench`, exposed as `checks.<system>.taintbench`.

These are small enough to *index*, which makes them the only corpus where both tiers
get exercised routinely, and there are enough of them to catch a section that crashes
on some unusual shape.

*Note:* that harness lives on the `taintbench` branch, not on this one. Evaluating on
it means either landing that branch first or borrowing its app data and fetch rule.
Do not copy the corpus into this branch.

**`~/apps`** — 14 real, large apps, from a 3 MB Facebook Lite to a 213 MB TikTok, a
mix of `.apk` and `.xapk`. This is the scale test, and the one that decides whether
the static tier is genuinely useful.

Two practical notes: the filenames contain `+` and percent-encoded characters, so
every path must be quoted rather than assumed tidy; and the `.xapk` bundles import as
several programs at once (Dex plus native libraries), which exercises the sub-import
and non-Java handling of §5.5 on real input rather than a fixture.

### 7.2 What the evaluation has to answer

1. **Does the static tier hold up at scale?** Time and peak memory for
   `ctadl import` followed by `ctadl report` on the largest app. This is the
   make-or-break number: if it holds, the report is useful on apps that cannot be
   indexed at all. Run it under a memory cap rather than watching it by hand.
2. **Are the numbers stable?** Two reports over the same import must agree. The tables
   we build on are documented as byte-stable, so this is an assertion, not a hope.
3. **Do the distributions behave as the intent predicts?** A small mean hiding a long
   tail; `equals`, `hashCode`, and Kotlin `FunctionN` sites near the top of the worst
   list; an RTA-versus-CHA gap large enough to be worth having.
4. **How concentrated is the imprecision?** The share of all call edges owned by the
   worst 10 and worst 100 sites, on real apps. This is the number that decides whether
   special-casing beats improving the whole analysis, which is the question the
   feature exists to answer.

### 7.3 How to run it

Add `cargo xtask report-eval`, next to `regression` and `taintbench`, taking a
directory of APKs (or the TaintBench apps directory). For each app it imports, runs
`ctadl report --format json`, saves the raw JSON per app, and prints a summary table
across apps. It reuses `xtask::exec` and the existing discovery helpers; it is not a
second harness.

Keeping the per-app JSON matters more than the table. It is what lets a later change
be compared against today's numbers instead of re-argued from memory.

## 8. Suggested order of work

1. **Phase 1** — the command, the static tier, index-tier fan-in and SCCs, RTA
   measurement, Kotlin lambda sites, text and JSON output, tests. Sections 1–11. No
   IR changes.
2. **Phase 2** — the `JavaCall` dispatch field and interface flags in the VMT, which
   also opens up general functional interfaces (§6.2). Section 12.
3. **Phase 3** — allocation depth from the index (§6.3) and cost numbers recorded at
   index time (§6.4). Sections 13–14.

The evaluation harness (§7.3) belongs in phase 1, not after it. Its first job is to
tell us whether the static tier survives a 200 MB app, and that answer shapes
everything else.

Phase 1 is self-contained and answers the original question — where the call-graph
imprecision is concentrated, and whether the worst handful of sites explain it — well
enough to decide whether phases 2 and 3 are worth their cost.
