# `ctadl report` — requirements and design - DO-NOT-MERGE

Status: **phases 1 and 2 implemented**. Derived from `intent.md`; corrected in place against
`plan.md` and against what implementing and measuring it actually showed. Corrections are
marked **Corrected:** and say what was wrong, so the original reasoning stays legible.

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

**Corrected in phase 2: the fallback reported the wrong error whenever the import existed but
could not be read.** `load_or_infer_project` tried the project, then the import, and on a
double failure raised the *project* error -- "no such file: projects/<name>/project_config.json"
-- discarding the import's. That is right when the name names nothing, and misleading when it
names a real import that a format bump has made stale: the import's error says which format it
is and to re-import it, and the project's says a file is missing, which sends the reader
looking in the wrong directory. Phase 2's `IMPORT_FORMAT_VERSION` bump turns that from a corner
case into the path *every* stored import takes once, which is how it was found. The fallback
now surfaces the import's error when an import of that name exists, and the project's only when
neither does. `ctadl query` shares the helper and gets the same fix.

## 3. Command surface

```
ctadl report <name> [--output FILE] [--format text|json] [--top N] [--no-recursion]
```

- `<name>` — a project name or an import name (see above).
- `--format` — `text` (default, for reading) or `json` (for tracking numbers over
  time). Text goes to stdout, as `docs/debugging.md` requires; progress and warnings
  stay on stderr.
- `--output` — write to a file instead of stdout. Defaults to `-`, meaning stdout, the
  way `write_sarif` spells it.
- `--top N` — how many worst call sites to list. Default 10, per `intent.md`.

**Corrected: `--section` was dropped, and `--no-recursion` took its place.** The general
flag was speculative — it assumed the report had several parts worth choosing between, and
measurement says it has one. Recursion is the only section that materializes the CHA call
graph, which is not the size of the program: TikTok's 5.2 M virtual sites expand to **1.21
billion** deduplicated edges, and building and Tarjan-ing that is 34 s of an 89 s run. Every
other section is a pass over a table. A selector over the cheap ones would have been a menu;
a switch for the one costly one pays for itself. See §6.5 for what it does *not* save.

The JSON form is a stable object, so a nightly job can diff two runs without parsing prose.
The text form is the same data rendered for a human. The shape is:

```
{ "tier": "static",
  "name": "<the name given>",
  "programs": [ { "import": ..., "language": ..., "functions": ...,
                  "census": {...}, "virtual_targets": {...}, "worst_signatures": {...},
                  "rta": {...}, "hard_cases": [...], "kotlin_lambdas": {...},
                  "functional_interfaces": {...}, "fan_in": {...}, "recursion": {...} } ],
  "empty_imports": [ "<imports with no functions>" ] }
```

One key per section inside each program, and a section that does not apply to that program
is **absent** rather than zero — a Pcode import has `census` and nothing else. `programs` is
an array because a name expands to a project's imports and each is measured separately; see
§5.3.

**Added in phase 2: a `by_dispatch` breakdown inside four of those sections**, and the same
rule governs it. `census.by_dispatch` is an object with a count per dispatch kind;
`virtual_targets`, `worst_signatures` and `rta` each carry a `by_dispatch` *array* with one
entry per kind that occurs, each naming its `dispatch` and repeating that section's numbers
over that kind's sites alone. A kind with no sites has no entry, a non-Java program has no
`by_dispatch` at all, and `recursion` carries `without_interface_edges` — the same graph with
the interface edges removed — rather than a per-kind list, because a graph is not partitioned
by the kinds of the calls that built it.

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
| 9 | Fan-out per site and fan-in per method | Import (CHA graph) | 1 |
| 10 | Recursion and strongly connected components | Import (CHA graph) | 1 |
| 11 | Kotlin lambda call sites, and how many reach a single body | Import | 1 (see §6.2) |
| 12 | Interface calls kept separate from class-virtual calls | Import (new IR field) | 2, done (see §6.2) |
| 12b | Functional interfaces in general: one interface, one abstract method | Import (new VMT columns) | 2, done (see §6.2) |
| 13 | How many call frames away the receiver's allocation is | **New index output** | 3 (see §6.3) |
| 14 | Analysis time, peak memory, inlined-code growth | Partly available | 3 (see §6.4) |

**Added to section 5: the same share counted by *signature*, not only by site.** The intent
asks what fraction of the edges the top 10 or 100 worst *call sites* own, "to justify
special-casing them". Measured, that number is necessarily tiny — ten instructions out of
millions — and on the large apps it runs from 0.06% to 1.6%. That is a real answer, but not
the one the justification needs, because nobody special-cases call site number 4,192. They
special-case `Object.equals`, and that covers every site dispatching on it. Counted that way
the answer inverts: on `com.noto_54.apk`, ten signatures own **87.3%** of all call edges
where ten sites own 0.4%.

Two lists come with it, because "worst" splits in two once sites are grouped. Ranked by
*target count* the top of `com.noto` is `Object.toString` at 804 targets and an obfuscated
`BaseContinuationImpl` method at 694 — but the latter has exactly one call site, so it costs
693 excess edges. Ranked by what it contributes, the list picks up `Iterator.next`, which has
only 73 targets across 1,576 sites and so costs 113,472. The report prints both and says
which share is computed over which.

**The denominator is the excess, not the total.** Ranking by raw edge count and dividing by
the total edge count is wrong in a way that only shows up on a small app: it puts
`StringBuilder.append` at the top of every TaintBench report — one target, several hundred
sites — and nothing about a monomorphic signature can be special-cased. Every resolved site
needs one edge; that is the call, not the imprecision. So the report ranks and divides by
`sites x (targets - 1)`, which is zero for a monomorphic signature however often it is
called, and states the excess total beside the shares so the denominator is visible.

**Corrected: sections 9 and 10 need only an import.** They were listed as wanting the index,
and they do not — they want the *CHA* graph, which is exactly the graph hybrid inlining has
to contend with, and which the import already determines. The real call graph an index
records is a strictly less interesting object here; see §5.4.

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

**Corrected: `cli::inspect_index_facts` supersedes nothing and was left alone.** It computes
a top-50 busiest-call-site list and a target-count distribution over the index's `call`
table. Under the default `mixed` strategy that table holds only the sites CHA already
resolved to exactly one target, so every row in it has one target and the "distribution" is
the constant 1. There was nothing to move.

**Corrected: `codegen::run_cha` was not moved into a `codegen::cha` module.** RTA is
computed by a second rule inside the same `ascent_run!`, guarded by `if rta`, so it shares
the subtype closure and the inherited-method table with CHA and costs nothing when off.
`ClassHierarchyAnalysis`, `run_cha` and `InstantiationFinder` became `pub(crate)` where they
stand. Codegen keeps calling the plain constructor, so the index it writes is unchanged.

**Corrected: the statistics helpers are weighted.** `percentile`, `Distribution` and
`top_n_share` all take `(value, weight)` pairs rather than plain slices, for the reason in
§5.3: the report stores one entry per *signature*, and expanding those back to one entry per
site would cost tens of millions of `usize`s to compute a number the grouped form gives
exactly.

### 5.3 How the static tier computes a report

1. Load each import of the project (sub-imports included) with `load_import`, the
   same call `inspect` and `dump_ir` make. Every import that has functions is measured
   **separately**; see the correction below.
2. Collect allocated classes with the existing `InstantiationFinder`, over every
   function including skipped ones.
3. Run CHA **once**, with the RTA rule switched on in the same Datalog program.
4. Walk every function and every statement. For each `CallAssign`, record the call kind
   and — for a `JavaCall` — bump the site count of its `(class, name, descriptor)`
   signature, and remember the signature against its caller for the graph sections.
5. Aggregate into the sections above, joining that table against the CHA and RTA maps.

**Corrected: the walk stores one row per *signature*, not per call site.**
`ClassHierarchyAnalysis::resolvents` is keyed `(class, name, descriptor)`, so every
`Object.equals` site in a program shares one target set and a per-site top-10 would print
ten copies of one row. The report therefore carries the number of sites per signature, and
every per-site figure is the weighted expansion of that table. This is also what makes the
report cheap: the walk holds one entry per distinct signature rather than one per site.

**Corrected: one CHA run, not two.** See §5.2.

**Corrected: the report measures every non-empty import, not "the first" one.** Naming a
project expands it to that project's imports, and for an `.xapk` the parent import carries
**no functions at all** — its code is in one sub-import per split APK. TikTok's parent has
zero functions and its `com.zhiliaoapp.musically` split has 1,868,340. A report that took
the first import would have printed zeros for a two-million-function app. Measuring them
separately is also the only sound choice: the class hierarchy is per program, and resolving
a call in one import against another's hierarchy would be making the answer up.

### 5.4 How the resolved tier adds to it

**Corrected: the resolved tier moved to phase 3, because in phase 1 it adds nothing.**

Under the default `CallResolutionStrategy::Mixed`, codegen emits a `call` edge only where
CHA resolved to exactly one target, pushes a site with two or more to `callee_info`, and
drops a site with zero **silently** — no edge, no `callee_info`, only a `log::trace!`.
Measured on an existing `vlc` index: 570,387 sites in `call`, every one with a single
target; 139,695 in `callee_info`; overlap zero.

So each thing this section proposed to read is already derivable from the import. The
deferred-site count is "signatures with two or more CHA targets". Fan-in over `call` is
fan-in over the monomorphic subset, which is the easy part of the graph rather than the
expensive one. SCCs over `call` cannot see recursion through any virtual call at all.
And `callee_resolvents` is keyed `(object, context, target_id)` with no call site in it,
so it cannot answer a per-site question; its top entries are `<init>`/`<clinit>`, which
are `DirectCall` on Dex anyway.

What an index would genuinely add is §6.3, and that needs a new output relation first.

### 5.5 Non-Java languages

The measurements are written against the Java/Dex model because that is what the
intent asks for. Lua has a comparable virtual-dispatch story (`LuaCall`, its own CHA
arm) and gets the same treatment where it is meaningful. Pcode and C have no class
hierarchy: for them the report prints the call census and the indirect-call count and
skips the type-resolution sections. It must not print zeros that look like findings.

### 5.6 Testing

- One synthetic case in `codegen/tests.rs`, built with `ctadl_ir::mir::builder` and an
  explicit `VirtualMethodTable::Java`: interface `I` with three implementers, a function
  that allocates an `A` and calls `I.m()`. CHA must find three targets and RTA exactly
  one. The allocated set comes from `InstantiationFinder` over the real body rather than
  being written by hand, so a finder that stopped seeing `new` makes RTA look broken
  rather than brilliant.
- **Phase 2:** one synthetic case in `report/callgraph.rs` covering the whole split at once —
  an interface with one abstract method and two implementers, a class with a subclass, and one
  call of each dispatch kind from one caller. It asserts the census split, the per-kind target
  rows, the per-kind excess rankings, that the one functional interface is found without
  matching any name, and that the interface-free graph is smaller than the full one. Every
  number in it is small enough to check by hand, which is why it exists beside the APK checks.
- Numeric unit tests in `stats.rs`, including a weighted case checked against the same
  sample written out one entry per unit of weight.
- Two `xtask` cases on the real Dex fixture, sharing the import the `apk:*` family
  already pays for: `apk:report` (it runs, names its tier, carries every section, and
  four aggregate counts are pinned to this APK) and `apk:report-invariants` (the numbers
  add up, and two runs are byte-identical).
- **Phase 2** adds to both rather than adding a case. `apk:report` pins the site count of each
  dispatch kind on `com.noto` (76,791 virtual, 21,153 interface, 1,607 super, 0 unknown), which
  is the only assertion that would fail if a frontend stopped reading the invoke opcode —
  every other one still passes when all three collapse into one. `apk:report-invariants`
  checks that the kinds partition the sites and that the per-kind edge, excess and RTA totals
  sum back to the pooled ones, and that removing interface edges cannot grow the graph. Not
  the SCC *count*: cutting edges can break one huge component into several smaller ones, so
  that number is free to rise while every other one falls, which is what the fixture shows.

**Corrected: no case was added to `ctadl-ascent/tests/cli.rs`.** That file's own rule is
synthetic input, temp store, milliseconds; a report worth asserting on needs a real
artifact, and real artifacts belong in `xtask`.

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

Turning it on for the report was a small change: one extra rule in the same `ascent_run!`,
guarded by `if rta`. Two cautions:

- The set of allocated classes comes from `new`-like expressions in *imported* code
  only. Objects created by library code we did not import, by reflection, or by
  deserialization are invisible. RTA will therefore drop targets that are genuinely
  reachable. The RTA number is a *lower bound* and the report must label it that way.
- We are not proposing to change how `ctadl index` resolves calls. The RTA count is a
  measurement, not a new strategy. If it later looks good, that is a separate change
  with its own soundness argument.

### 6.2 Interface calls could not be told apart from class-virtual calls

`intent.md` is explicit that these must never be averaged together, and this was the
one requirement we could not meet without changing the IR.

Two facts were lost at import:

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

**Done, and the fix is what was proposed plus one column.** `CallStyle::JavaCall` carries
`dispatch: JavaDispatch`, read straight off the invoke opcode in the dex frontend and off
`CallKind` in the jvm one. The java VMT gained `interfaces` (which types the import declares
`interface`) and `abstract_methods` (the body-less declarations). `IMPORT_FORMAT_VERSION` is
`"7"`, so everything in a store has to be re-imported.

Three things the implementation settled that the proposal left open:

**A fourth kind was needed.** `Virtual`/`Interface`/`Super` does not cover a JVM
`invokedynamic` with a receiver, which has no dispatch instruction to read at all — where it
goes is decided by a bootstrap method at run time. That is `Unknown`, and it is reported as
its own row rather than folded into `Virtual`, because it is a gap in what was recorded and
not a fourth way of dispatching. On dex it is always zero.

**The second VMT column was not in the proposal, and the general functional-interface case
needs it.** The tail of this section said the general SAM case had to wait on "knowing which
types are interfaces and which of their methods are abstract". The first half is the
`interfaces` column; the second is not derivable from anything else, because the CHA resolvent
map keyed on an interface holds every method of every *implementer* rather than the
interface's own — on `com.noto`, `Ljava/lang/Object;.toString` is a resolvent key of any
interface whose implementers override it. Abstract declarations are absent from the VMT's
`methods` column for the same reason natives were before version `5`: no body, no function to
name. Adding the column while the wire format was being bumped anyway cost one push per
frontend and avoided a second re-import of the corpus later.

**Nothing resolves differently.** The dispatch field is not read by `codegen`: an
`invoke-super` is still resolved as though the receiver's type were unknown, which
over-approximates it — a super call's target is fixed at the named class. Narrowing it is a
change to what the index resolves to, with its own soundness argument, and is deliberately not
part of recording the distinction. The report measures what the over-approximation costs
instead (§7.5). `run_cha` is now passed the real `interface_type` set, which changes not one
resolvent: the merged `hierarchy` already carries every interface subtype edge, so the only
thing the set adds is `class_or_interface` membership for an interface that declares no method
and appears in no hierarchy, and such a type derives no resolvent. The 153-case regression
suite passes unchanged, which is the evidence for that argument rather than the argument
itself.

The same gap limits one part of section 11. Kotlin needs nothing special from CTADL —
it compiles to the JVM and Dex bytecode the frontends already read — and a Kotlin
lambda call site is in principle recognisable from what an import already carries: the
declared receiver type is `kotlin/jvm/functions/FunctionN`, and its target count comes from
CHA like any other virtual call.

**Corrected: on a real release APK that recognition is unreliable, in both directions.**
Measured, both discriminators fail, in different ways and on different apps.

TikTok's main split keeps `kotlin/jvm/functions/FunctionN` at its call sites, and there the
**type test undercounts**: 31,622 sites by receiver type against 57,383 by method name, so
25,761 real lambda sites carry some other declared type. `Facebook+Lite` has almost no
`kotlin` type names at all (1,016 types, everything else `LX/000;`, the rest loaded at
runtime) and finds 2 by type against 28 by name.

`com.noto_54.apk` is the harder case: the **type test finds nothing**. Its type pool *does*
contain `Lkotlin/Function0;` through `Function2;`, but no call site dispatches on one,
because the obfuscator repackaged the interfaces that actually carry `invoke` — its worst
signatures are `Lu7/p;.R(...)`, `Lu7/l;.U(...)` and `Lu7/a;.k0()`, which are those
interfaces — and renamed their methods with them. Its 141 `invoke`/`invokeSuspend` sites are
therefore a floor, not a count, and a receiver-type list of any length would have reported
zero.

So the report matches **both** discriminators — the receiver type against a documented list
of prefixes with the arity digits stripped, and the method name against `invoke` and
`invokeSuspend` — and prints each count, their agreement and their disagreement. Obfuscation
then shows up as data rather than as a silent zero, and the report says outright that a zero
here is what a repackaged app looks like, not evidence that there are no lambdas. It also
lists the receiver types it did match, so a stale prefix list is visible. What has to wait is the *general* case — any
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

**Corrected: this is phase 3 along with the rest of the resolved tier, and phase 1 does not
touch `IndexConfig`.** The report's *own* cost, which is a different question and the one
that mattered for phase 1, is measured in §6.5.

### 6.5 Cost of the report itself

**Measured, on `~/apps`, before phase 1 was called done.** Release binary, each run under a
hard memory cap with `/usr/bin/time -l` inside it, so the peaks are kernel high-water marks.
`ctadl inspect`, which does nothing but `load_import`, is the floor the report starts from.

| program | functions | virtual sites | load floor | report wall | report peak | CHA graph edges |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `tiktok__com.zhiliaoapp.musically` | 1,868,340 | 5,211,407 | 12 s / 9.7 GiB | 85 s | 23.9 GiB | 1,213,564,613 |
| `whatsapp__com.whatsapp.w4b` | 379,725 | 1,088,334 | 3 s / 2.5 GiB | 14 s | 5.7 GiB | 138,348,205 |
| `telegram` | 222,257 | 568,049 | 2 s / 1.2 GiB | 5 s | 2.4 GiB | 16,281,935 |
| `vlc__org.videolan.vlc` | 242,243 | 380,849 | 1 s / 0.9 GiB | 5 s | 2.2 GiB | 10,775,256 |
| `com.noto_54.apk` (the fixture) | 50,642 | 99,551 | — | 0.8 s | 0.6 GiB | 1,671,410 |

Three things follow.

**The report is affordable at the top of this corpus**, and it is not the import that costs.
TikTok decodes in 12 s and 9.7 GiB; the report takes 89 s and 24.9 GiB. So the plan's guess
that the import would dominate was wrong, which is why there is no general `--section` flag:
paying for only part of the report cannot save the part that costs.

**What sets the peak is CHA, not the call graph.** Running the same report with
`--no-recursion` on TikTok gives 55 s and 24.1 GiB — the graph section costs **34 s of wall
time and 0.9 GiB of peak**, not the 14 GiB one might guess from 1.21 billion edges. The
reason is ordering: the peak is reached inside `run_cha`, whose intermediate relations
(`cha_subtype`'s transitive closure, `cha_super_method`, and now `rta_resolve` beside
`cha_resolve`) are the largest objects in the run, and they are freed before the graph is
built. The graph then fits underneath a high-water mark that was already set. So
`--no-recursion` buys wall time, not headroom, and the doc comments say so rather than
implying a memory saving it does not deliver.

**The edge count is logged before Tarjan runs**, so a run that is about to spend half its
time in that section says so first. Node and SCC indices in the graph are `u32` rather than
`usize`: at a billion edges the successor lists and `Sccs`'s own concatenated successor array
are each one machine word per edge. That halves the graph's own footprint, though — for the
ordering reason above — it does not move the measured peak.

### 6.6 The name in `intent.md` says "import", the data mostly wants an index

Resolving one name to either an import or a project (§3) keeps the command simple, and the
report opens with a line naming the tier it ran at.

**Corrected: the premise was wrong, and the outcome is better than it predicted.** Fan-in and
recursion do *not* need an index — they want the CHA graph (§5.4) — and inlining depth is
phase 3. So in this version no section is marked "needs an index" and nobody is surprised.
The opening line still names the tier, because a reader who has not read §5.4 will assume
an index would have added something.

What the shared name resolution *did* turn up is §5.3's correction: expanding a name to a
project pulls in the sub-imports, and for an `.xapk` the parent among them is empty. Naming
one import is not the same as naming one program.

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

**Corrected: the harness takes a directory, and the static tier does hold on the largest app
in `~/apps`** — 85 s and 24 GB for a 1.9-million-function program, against a 12 s and 9.7 GiB
floor to decode it at all (§6.5). Since there is no resolved tier in this version, the
question of whether indexing that app terminates is deferred with it to phase 3.

One practical note the corpus taught us: a `.xapk` does not import as "a program plus its
native libraries". It imports as *one program per split APK*, plus an empty parent. For
TikTok that is thirty sub-imports, of which one has 1.87 M functions, two have single digits,
and the rest have none. Any harness or report that assumes one artifact means one program is
wrong on this corpus.

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

**`~/apps/fdroid` (added in phase 2)** — nine free-software Android apps from the official
F-Droid repository, 11–176 MB, 11–88 MB of dex apiece: DuckDuckGo, Nextcloud Talk and Files,
OsmAnd, an Element fork, Session, AntennaPod, Wikipedia and NewPipe.

It exists because the other two corpora cannot answer one question. Every measurement here that
matches a *name* is defeated by a release APK, and a zero from such a test is indistinguishable
from a correct zero (§6.2 is the whole argument). These apps are built from source without an
obfuscator, so `kotlin/jvm/functions/FunctionN` and real class names survive, and a test can be
checked against apps where it is known to be readable. §7.5 uses it exactly that way, and it
overturned one of this document's guesses.

It is also the one corpus that is **reproducible by someone else**, which §6.7 says the large
apps are not. The F-Droid index publishes a sha256 per APK; `~/apps/fdroid/.fdroid-manifest.json`
records the package, version, URL, hash, licence and dex size of each, and every file was
verified against it on download. A dotfile because `report_eval::discover` skips dotfiles as
metadata rather than offering them to `ctadl import`. Nothing here needs to be committed to the
repo, and a Nix fixed-output derivation like `nix/taintbench.nix` could fetch the whole corpus
from that manifest -- which would make it the first large-app corpus a check could depend on.

It sits in a subdirectory rather than beside the fourteen, so that `--apks ~/apps` keeps
denoting exactly the corpus §7.4 and §7.5 report medians over. Sweep it with
`--apks ~/apps/fdroid`.

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

`cargo xtask report-eval --apks <dir>`, next to `regression`. For each artifact in the
directory it imports into a scratch store, runs `ctadl report --format json`, saves the raw
JSON *and* the text form per app, records wall time and peak footprint for each phase, and
prints a summary table across apps. It reuses `xtask::exec` and the `ctadl` build helpers;
it is not a second harness.

Keeping the per-app JSON matters more than the table. It is what lets a later change
be compared against today's numbers instead of re-argued from memory.

Details worth stating:

- `--apks` is **required**, with no default. A baked-in path would make a result from one
  machine's private corpus look reproducible.
- It runs each report twice and asserts the two are byte-identical, which is §7.2 question 2
  discharged per app rather than argued.
- Peak memory comes from `/usr/bin/time -l` wrapped around each command, so it is a kernel
  high-water mark; where that is unavailable the column is absent rather than filled in with
  a number measured a different way.
- Native libraries are **not** imported by default. They go through Ghidra, which is a much
  larger and quite different measurement from the Dex call graph; `--native-libs` turns it on.
- Scratch stores are deleted per app unless `--keep-stores` is passed. A 200 MB app imports
  to a couple of gigabytes.
- An `.xapk` is several programs (see §6.7), so the summary row sums the counts across them
  and takes the distribution columns from the largest program by function count. A weighted
  merge of two programs' percentiles would need their full tables, which the JSON
  deliberately does not carry.

### 7.4 Results

Phase 1, both corpora, release binary. Raw per-app JSON, the text reports and the logs are
kept outside the repo (`report-eval --out`); every number below names the file it came from.
**52 of 52 apps reported successfully and none was excluded.**

#### Q1 — does the static tier hold up at scale?

Yes. Worst case in the corpus is TikTok's 213 MB `.xapk`: 89 s and 19.3 GiB to import all
thirty splits, then 95 s and 26.8 GiB to report on them — against a floor of 12 s and 9.7 GiB
just to decode the largest split at all. Every other large app is under 30 s a phase. See
§6.5 for the breakdown and for why `--no-recursion` saves time rather than memory.

#### Q2 — are the numbers stable?

Yes, everywhere. `report-eval` runs each report twice and compares the JSON byte for byte:
**38 of 38** TaintBench apps and **14 of 14** large apps identical. That is what building on
the pre-fixpoint tables buys, and it means a nightly diff shows real movement rather than
noise.

#### Q3 — do the distributions behave as the intent predicts?

Yes, and the prediction is an understatement on the large apps.

| | TaintBench (38) | `~/apps` (14) |
| --- | --- | --- |
| sites with exactly one target | 76–99% (median 90%) | 59–86% (median 71%) |
| p50 targets per site | 1 everywhere | 1 everywhere |
| p90 | 1–4 (median 1) | 2–40 (median 18) |
| p99 | 1–48 (median 4) | 46–15,754 (median 976) |
| worst site | 2–274 (median 12) | 46–21,257 |
| mean | 1.0–3.0 | 3.5–278 |

The intent's "a handful of huge call sites hides behind a small mean" is exactly right, and
inverted: on TikTok the *median* site has one target while the mean is 278 and the 99th
percentile is 15,754. Reporting the mean alone would have said nothing true about either end.

The named hard cases are where the intent said they would be. Pooling the per-app top-ten
lists, `Object.toString` and `Object.hashCode` appear in **14 of 14** large apps,
`Iterator.next` in 14, `Object.equals` and `Iterator.hasNext` in 13, `Runnable.run` in 11,
and `kotlin/jvm/functions/Function1.invoke` and `Function0.invoke` in 8. TaintBench adds the
Android lifecycle methods — `Activity.onCreate` in 18 of 38, `Parcelable$Creator.createFromParcel`
in 11 — which the intent did not anticipate and which are the same shape of problem.

RTA is the one prediction that does not hold up. It drops 9–77% of edges on TaintBench
(median 33%) but only 0.5–17% on the large apps (median **3.7%**) — and remember it is a
lower bound, so the true saving is smaller still. The large apps allocate most of what they
declare; there is little for RTA to remove. It is not the lever.

#### Q4 — how concentrated is the imprecision?

This is the question the feature exists to answer, and the answer depends entirely on how
"the worst call sites" is counted.

Counted as **instructions**, the imprecision is not concentrated at all: the ten worst call
sites own 0.01–1.6% of all call edges on the large apps (median 0.18%). Ten instructions out
of five million cannot own much, so special-casing individual sites is not a strategy.

Counted as **signatures**, it is concentrated almost completely. The ten `(class, name,
descriptor)` signatures contributing the most excess own **76–97% of it on every large app,
median 94.6%**; a hundred signatures own 94–99.5%. TaintBench, whose apps are small enough
that the framework classes are not in the dex at all, is looser at 18–100% (median 61%).

So the answer is yes with a correction to the question: special-casing beats improving the
whole analysis, but the unit is the *method signature*, not the call site. Ten of them —
`Object.toString`, `Object.hashCode`, `Object.equals`, `Iterator.next`, `Iterator.hasNext`,
`Runnable.run`, and Kotlin's `FunctionN.invoke` — account for nearly all of it, on every real
app measured.

Two findings arrived alongside that. The CHA graph has one enormous cycle in it: on the large
apps 3.5–48% of all functions sit in a single strongly connected component (TikTok's largest
is 693,372 functions, 37% of the program), which is what an inlining-based approach has to
terminate against. And the zero-target sites — where the graph is silently unsound — are a
small but non-zero 0.02–0.5% of virtual sites everywhere.

### 7.5 Results, phase 2

All three corpora, release binary, everything re-imported (the format bump left nothing in a
store readable). **61 of 61 apps reported successfully and none was excluded**, and
`report-eval`'s double run says all 61 are byte-identical, so §7.2 question 2 still holds with
the new sections in place.

The third corpus is new in this phase and is described in §7.1: nine unobfuscated F-Droid apps,
which exist to be a **control group**. Every measurement in this feature that reads a *name* --
the Kotlin receiver-type list, and by extension anything that would key on a package -- is
defeated by a release APK, and until now there was no way to tell a test that is wrong from a
test that is merely blindfolded.

#### Does the split change the picture? Yes, and pooling was hiding it

Interface dispatch is a **minority of the sites and a majority of the problem**. On the 14
large apps it is 11.4–24.5% of virtual call sites (median 18.2%), and the two populations do
not resemble each other at all:

| | interface | class-virtual | super |
| --- | --- | --- | --- |
| sites resolving to exactly one target | 7.5–39.2% (median **16.0%**) | 76.8–93.5% (median **81.4%**) | 0.6–20.9% (median 4.4%) |
| p99 targets per site | 12–15,265 (median 1,258) | 4–15,754 (median 308) | — |
| share of the program's excess edges | 6.8–85.7% (median 31.4%) | 13.4–92.3% (median 67.6%) | 0.2–5.7% (median 1.0%) |

The monomorphic fraction is the one to look at. Phase 1 reported 59–86% of virtual sites
resolving to a single target and called that "how much work is already done". Split, that
number belongs almost entirely to `invoke-virtual`: five sixths of class-virtual sites are
already exact, and five sixths of interface sites are not. Reporting one figure for both
described neither, which is what `intent.md` said would happen.

`invoke-super` is the surprise. It is 1–2% of sites, and **almost none of them resolve to one
target** — median 4.4%, against 81.4% for `invoke-virtual`. That is not a property of super
calls; it is CTADL resolving them as though the receiver's type were unknown when the
instruction names the class to start from. The excess it produces is small (median 1.0% of the
program's) because the sites are few, but it is close to pure waste, and it is the cheapest
precision left on the table. On TaintBench, where there is less other code, it reaches **100%
of one app's excess**.

The phase-1 finding that ten signatures own the imprecision survives the split and sharpens.
Ranked *within* a kind, the worst ten own 70–100% of that kind's excess (median 90%) for
interfaces and 90–100% (median ~100%) for class-virtual. And of the ten pooled worst
signatures per app, **83 of 140 across the corpus are dominantly interface-dispatched** —
phase 1 guessed "half of the ten", and measured it is six of ten in the median app.

#### The finding nobody asked for: the giant cycle is interface dispatch

Phase 1 found that 3.5–48% of a large app's functions sit inside one strongly connected
component of the CHA call graph, and called it what inlining has to terminate against.
Deleting every interface-dispatched edge and re-running Tarjan says most of it is not really
there:

| | with interface edges | without |
| --- | --- | --- |
| functions inside a cycle | 3.5–48.0% (median 37.6%) | 2.3–23.3% (median **6.4%**) |
| largest single cycle | — | 1.1–74.6% of its former size (median **26.3%**) |
| deduplicated edges | — | 24–93% of them left (median 73%) |

Chrome is the extreme: its largest cycle goes from 6,012 functions to **68**. Telegram's from
80,851 to 2,889. The WhatsApp family barely moves (76,951 of 179,172), so this is not
universal — but on ten of fourteen apps the giant component is mostly an artifact of
resolving interface calls to every implementer, rather than a fact about how the program
calls itself. That makes it a resolution problem, which is fixable, instead of a program
property, which is not.

#### Functional interfaces, without matching a name

The general SAM test finds 34–8,623 functional interfaces per large app (median 1,539) where
the Kotlin receiver-type list found **zero** on `com.noto` and 2 on `Facebook Lite`. It works
on obfuscated apps because it matches no names: Chrome's worst functional-interface signatures
are `Lye8;.d()Lw6h;` and `Looj;.F0(...)`, which no prefix list could have recognised. Only
2.3–29.2% of those sites (median 4.3%) resolve to a single body, so treating lambdas specially
is not a solved case.

It comes with its own honest limit, which the report prints beside it: 27–82% of interface
call sites (median 49.4%) name a type the import never declares — `java/util/Iterator` in an
app that does not ship the framework — so the SAM numbers are over the remainder. That is a
coverage figure, not a failure, and it is the one number a reader would otherwise have to
guess at.

#### TaintBench, and what small apps do differently

The direction is the same and the magnitude is not, for the reason phase 1 gave: the framework
classes are not in these dexes. Interface dispatch is 0–26.7% of sites (median 10.6%), and its
monomorphic fraction is 20.8–100% (median 52.8%) against 82.8–100% (median 96.3%) for
class-virtual — still a gap, a third the size. Cycles are small either way (median 2.9% of
functions, 1.7% without interface edges). One app has no interface dispatch at all, which the
report shows as an absent row rather than a zero.

#### The control group: what changes when the names survive

The nine F-Droid apps say the same thing about dispatch, more strongly, and correct one thing
this spec had guessed at.

The interface/class-virtual gap is **wider** on clean apps, not narrower: 3.0–20.0% of
interface sites resolve to one target (median **8.7%**) against 82.2–94.0% for class-virtual
(median **91.1%**). Interface dispatch is 13.4–28.0% of sites (median 15.5%) and owns 17.3–74.4%
of the excess (median **41.6%**, against 31.4% on the obfuscated corpus). So none of the
phase-2 finding is an artifact of obfuscated type names.

The cycle result is stronger too. Functions inside a cycle go from 19.6–31.3% (median 26.1%) to
2.7–11.9% (median **4.3%**) once interface edges are removed, and the largest cycle falls to
3.7–41.0% of its former size (median **7.3%**, against 26.3% on the obfuscated corpus).
DuckDuckGo's largest cycle goes from **115,062 functions to 5,067**.

One number reverses, and it is worth stating rather than burying: on these apps the interface
p99 (median 606) sits *below* the class-virtual p99 (median 1,144), where on the obfuscated
corpus it was above. The tail of the worst class-virtual signatures -- `Object.toString` and
friends, which every class in a large app overrides -- is longer here than the interface tail.
The monomorphic fraction, not the p99, is what separates the two populations consistently.

**Corrected: the Kotlin receiver-type undercount is not caused by obfuscation.** §6.2 recorded
that TikTok's type test found 31,622 sites against the name test's 57,383 and reasoned that
"25,761 real lambda sites carry some other declared type", leaving open why. Measured on apps
whose names are intact, the type test still finds only 22–75% of what the name test does
(median **46%**) -- statistically the same shortfall as the obfuscated corpus's median of 50%.
What obfuscation changes is whether the test finds *anything*: it returned zero on one of
fourteen release APKs and on none of nine F-Droid ones. The undercount itself is something
else -- `invokeSuspend` on generated continuation classes, and lambdas that implement a more
specific interface than `FunctionN` -- and no prefix list will fix it. Matching both
discriminators and printing the disagreement remains the right design; the reason is now
measured rather than assumed.

**And the dispatch field cross-checks clean.** A Kotlin lambda call is an interface dispatch by
construction, so every site the receiver-type test matches must be one. Across both Java
corpora that is **91,050 of 91,050 sites -- 100%, with no exceptions in 23 apps**. The frontends
are reading the invoke opcode correctly.

#### Cost

Import is unchanged: TikTok's `.xapk` still takes 89.8 s and 19.3 GB, so carrying a dispatch
field and two VMT columns costs nothing measurable at import. The report goes from 95 s to
**113.6 s** at the same 26.9 GB peak. The +19 s is the second Tarjan over the interface-free
graph; it shares the successor array with the first, which is why the peak did not move. Every
other app in the corpus reports in under 20 s.

## 8. Suggested order of work

1. **Phase 1** — the command, the static tier, fan-in and SCCs *over the CHA graph*, RTA
   measurement, Kotlin lambda sites, text and JSON output, tests, and the evaluation
   harness. Sections 1–11. No IR changes. **Done.**
2. **Phase 2** — the `JavaCall` dispatch field and interface flags in the VMT, which
   also opens up general functional interfaces (§6.2). Section 12. **Done.**
3. **Phase 3** — the resolved tier as a whole: allocation depth from the index (§6.3), cost
   numbers recorded at index time (§6.4), and fan-in over the real `call` graph beside the
   CHA one. Sections 13–14. **Corrected: the resolved tier moved here from phase 1**, for
   the reason in §5.4 — in phase 1 it would have added nothing an import does not already
   determine.

The evaluation harness (§7.3) belongs in phase 1, not after it. Its first job is to
tell us whether the static tier survives a 200 MB app, and that answer shapes
everything else.

**Corrected: that first job was done before the harness, and before any new code**, with
`ctadl import` and `ctadl inspect` alone. It was the right order: it is what found the empty
`.xapk` parent (§5.3), which would otherwise have been discovered by shipping a report that
printed zeros for TikTok.

Phase 1 is self-contained and answers the original question — where the call-graph
imprecision is concentrated, and whether the worst handful of sites explain it — well
enough to decide whether phases 2 and 3 are worth their cost.

**It did answer it** (§7.4): a handful of call *sites* explains nothing, a handful of method
*signatures* explains nearly everything, and RTA is not the lever on a real app. What that
implies for phase 2 is worth stating, because it is not what phase 2 was scoped for. Half of
the ten signatures that own the imprecision are interface calls — `Iterator.next`,
`Iterator.hasNext`, `Runnable.run`, `FunctionN.invoke` — so the interface-versus-virtual
split of §6.2 is no longer only a reporting nicety; it is a distinction the thing that would
be special-cased is drawn along.

**Phase 2 confirmed that reading and added one finding nobody asked for** (§7.5). Interface
dispatch is a fifth of the call sites and a much larger share of the imprecision, and its
sites are nothing like the class-virtual ones — where the two are pooled, the pooled number
describes neither. The unasked-for one is about recursion: deleting every interface-dispatched
edge takes the functions inside a cycle from a median 37.6% of a large app to 6.4%, and
DuckDuckGo's largest cycle from 115,062 functions to 5,067. The giant strongly connected
component phase 1 found is mostly built out of interface calls, which makes it a resolution
problem rather than a fact about the program.

It also turned up the cheapest precision left in the codebase, which is not what it was
scoped for. `invoke-super` is 1–2% of call sites and **4.4% of them resolve to one target**,
against 81.4% for `invoke-virtual` — because CTADL resolves a super call as though the
receiver's type were unknown when the instruction names the class to start from. Fixing that
is a resolution change and needs its own soundness argument, so it belongs with phase 3
rather than here; it is now measured, which is what phase 2 was for.
