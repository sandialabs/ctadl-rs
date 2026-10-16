# Where ceno's memory goes - DO-NOT-MERGE

`ie.equalit.ceno` is the F-Droid app from `taintbench-scaling-20260922.md` that
runs 90 minutes, reaches 96 GiB and never gets to a fixpoint, although it is
smaller on every input measure than `darkcoin`, which finishes in under three
minutes. This note explains where that memory goes and why.

## Short answer

**Almost all of it is spent on hybrid inlining's per-caller context for two
program features working together: the Kotlin coroutine builders
(`launch`, `withContext`, `async`, ...), and R8's horizontal class merging
of Kotlin lambdas.**

- The memory is not in `locals`. At 900 s, `locals` has 102 M rows but takes
  only 2.9 GiB (30 B/row), because its trie is compact. The largest relation is
  `context_locals`, with 66 M rows and 11 GiB, followed by
  `call_target_assign_like` (5.8 GiB), `set_establishes_via` (3.5 GiB) and
  `ctx_edge_split` (3.2 GiB). Together, the contextual relations account for
  about 60% of the counted bytes and about two thirds of rule time.
- The rows sit in very few functions. 10 of the 66 k functions hold 76% of
  `context_locals`. The 1,725 functions that R8 built by merging classes
  (2.6% of all functions) hold 80% of `context_locals`, 60% of `locals` and 64%
  of `call_target_assign_like`. The worst single function is
  `PictureInPictureIntegration$start$1.invokeSuspend`. It is 30 unrelated
  suspend-lambda bodies behind one `$r8$classId` switch, with 1,482 decisions
  and 1.3 billion (row, decision) memberships.
- **The calls to the coroutine builders are what makes the difference.** If
  ctadl stops analyzing the bodies of 10 builder functions and leaves
  everything else alone, ceno reaches a fixpoint in 148 s at 14 GiB. The
  default run was still growing when it hit the limit at 5,400 s and 96 GiB.
  The builders only cause this because the lambdas they start have been
  merged by R8: darkcoin has as many coroutines, is not class-merged, and
  finishes with or without the change.
- A second, separate cost is flow of constants. 40% of `locals` rows only
  record that a variable may hold a static constant: an enum value,
  `Unit.INSTANCE`, a kotlinx sentinel, an `R.id` value, and so on. Only 283
  static fields in the whole app are ever written outside their class
  initializer. If the reads of the others are treated as constants, `locals`
  has 39% fewer rows at the same wall time.

What to change is in [Recommendations](#recommendations). In short:

1. Model the coroutine builders so that they call their block at the call
   site.
2. Treat reads of static fields that are effectively final as constants.
3. Store the lattice relations and `call_target_assign_like` compactly, as
   `locals` already is.

## How this was measured

The recipe was followed: a memory guard, Ascent's `run_timeout`, a tuple count
for every relation, a ladder of timeouts (30, 60, 120, 240, 480, 900 s), rule
times ranked by total and per tuple, and the cost of each relation broken
down by index. That needed three pieces of tooling.

**1. Ascent's timeout discarded the state we wanted to measure.** In
`ascent_macro` 0.8, `run_timeout` returns `false` from inside the loop of
whichever SCC is running. That return comes before the SCC's local
`total`/`delta` stores are moved back into `self`, and before the SCC's
time is recorded. Nearly all of ceno's work happens in a single SCC (scc 4,
the dataflow closure), and that SCC never finishes. So every earlier
`CTADL_INDEX_TIMEOUT_SECS` run lost all of its contents: `locals` reported
10.8 M rows, but its store held nothing, and scc 4 had no timing. The patched
macro is in `vendor/ascent_macro`, attached through `[patch.crates-io]`. On a
timeout it moves the last delta into `total`, breaks out of the loop, runs
the SCC's normal epilogue, and only then returns `false`. The plain `run()`
is unchanged, and `cargo test -p ctadl-ascent` still passes (357 tests, 0
failures).

**2. A byte census of every index.** `index_engine/census.rs` prices every
physical structure of the serial program from its capacity. It is
`census_fields.rs`, generated from `cargo expand` output, that says which
structures exist: 151 of them across 48 relations. With `RUST_LOG=ctadl=debug`
the ranking goes to the log, and `CTADL_INDEX_CENSUS=<file>` also writes it as
TSV. It adds up to 76-88% of process footprint. The rest is the IR/facts
baseline of about 0.9 GiB plus allocator slack.

One thing about Ascent 0.8 storage explains many of the numbers below. A
non-full index is `HashMap<key, Vec<remaining columns>>`, which means the
values are copied inline, not stored as row ids. So every index is another
full copy of the relation. A lattice index is `HashMap<key, HashSet<row>>`,
which allocates one hash set per key.

**3. A relation dump.** `CTADL_INDEX_DUMP=<dir>` writes `locals`,
`context_locals`, `assign_like`, `call_target_assign_like`, `summary`, the
call facts and the function names as TSV, and duckdb then attributes the rows
to functions, variables and access paths.

## Where the memory goes

### The ladder

| stopped at | peak | counted | scc 4 iterations | decision memberships |
| ---: | ---: | ---: | ---: | ---: |
| 30 s | 6.1 GiB | 4.5 GiB | 60 | 0.00 G |
| 60 s | 9.7 GiB | 8.1 GiB | 88 | 0.07 G |
| 120 s | 12.9 GiB | 10.4 GiB | 96 | 0.19 G |
| 240 s | 24.7 GiB | 18.0 GiB | 107 | 1.15 G |
| 480 s | 31.8 GiB | 25.2 GiB | 114 | 3.15 G |
| 900 s | 44.1 GiB | 34.3 GiB | 121 | 6.95 G |

Iterations get slower and slower: the last 7 took 400 s. The number of
decision memberships, meaning pairs (a `context_locals` row, a decision it
holds under), more than doubles every time the timeout doubles. Here is how the
largest relations grow:

| relation | 30 s | 120 s | 480 s | 900 s |
| --- | ---: | ---: | ---: | ---: |
| `context_locals` | 0.8 M / 0.1 G | 10.7 M / 1.9 G | 43.2 M / 7.4 G | **66.0 M / 11.0 G** |
| `call_target_assign_like` | 4.4 M / 1.5 G | 11.6 M / 3.3 G | 16.9 M / 5.5 G | **18.9 M / 5.8 G** |
| `set_establishes_via` | 0.04 M / 0.0 G | 0.35 M / 0.1 G | 13.2 M / 1.8 G | **20.8 M / 3.5 G** |
| `ctx_edge_split` | 0.2 M / 0.0 G | 2.4 M / 0.6 G | 9.7 M / 2.2 G | **16.5 M / 3.2 G** |
| `locals` | 10.5 M / 0.4 G | 32.6 M / 1.0 G | 74.6 M / 2.0 G | 101.6 M / 2.9 G |
| `edge_split` | 8.5 M / 0.6 G | 16.3 M / 0.9 G | 30.8 M / 1.4 G | 41.8 M / 1.8 G |
| `context_assign` | 0.1 M / 0.0 G | 1.0 M / 0.2 G | 3.9 M / 0.8 G | 6.5 M / 1.5 G |

### What each expensive relation costs, index by index (900 s)

| relation | structure (key columns) | keys | bytes | B/row |
| --- | --- | ---: | ---: | ---: |
| `context_locals` (lattice) | `context_locals_indices_0_1_2_3_4` (lattice key -> `HashSet<row>`) | 66.0 M | **5.13 GiB** | 83 |
| | rows `Vec` | - | 3.50 GiB | 57 |
| | `context_locals_indices_0_1_2` | 0.62 M | 1.22 GiB | 20 |
| | `context_locals_indices_none` (a single set holding every row id) | 1 | 1.13 GiB | 18 |
| `call_target_assign_like` | `call_target_assign_like_indices_0_1_2_3` (full) | 18.9 M | 1.53 GiB | 87 |
| | rows `Vec` | - | 1.50 GiB | 85 |
| | `call_target_assign_like_indices_0` (per function; values inline) | 12 k | 1.03 GiB | 59 |
| | `call_target_assign_like_indices_0_1_2` | 2.1 M | 0.86 GiB | 49 |
| | `call_target_assign_like_indices_0_1` | 0.37 M | 0.84 GiB | 48 |
| `set_establishes_via` | `set_establishes_via_indices_0_1_2_3_4` (full) | 20.8 M | 1.53 GiB | 79 |
| | rows `Vec` | - | 1.50 GiB | 77 |
| | `set_establishes_via_indices_0_1` | 22 k | 0.45 GiB | 23 |
| `ctx_edge_split` (lattice) | `ctx_edge_split_indices_0_1_2_3_4_5` | 16.5 M | 1.78 GiB | 116 |
| | rows `Vec` | - | 1.13 GiB | 73 |
| | `ctx_edge_split_indices_0_1_2` | 0.20 M | 0.29 GiB | 19 |
| `locals` (trie) | one store | 1.6 M groups | 2.87 GiB | **30** |
| `edge_split` (trie) | one store | 3.2 M groups | 1.83 GiB | 47 |

Two representation costs are visible here and could be removed without
changing any result:

- **The key index of a lattice relation keeps one hash set per row.** In
  `context_locals_indices_0_1_2_3_4`, each lattice key maps to exactly one
  row, yet Ascent stores a `HashSet<usize>` for it: 83 B/row where a `usize`
  would do. That is 5.1 GiB in `context_locals` and 1.8 GiB in
  `ctx_edge_split`.
- **`call_target_assign_like` is a plain Ascent relation stored five times,**
  at 327 B/row. The tuple has a 24-byte `CallTargetObject` holding an
  `ArcIntern<str>`, and the same tuple sits in the rows `Vec`, the full index
  and three probe indices. For comparison, the `locals` trie takes 30 B/row.

### Time, ranked the same way (900 s, scc 4)

| head relation | rule time | share | rows | µs/row |
| --- | ---: | ---: | ---: | ---: |
| `context_locals` | 537 s | **61%** | 66.0 M | **8.13** |
| `call_target_assign_like` | 128 s | 15% | 18.9 M | 6.78 |
| `locals` | 94 s | 11% | 101.6 M | 0.92 |
| `context_assign` | 24 s | 3% | 6.5 M | 3.74 |
| `set_establishes_via` | 19 s | 2% | 20.8 M | 0.90 |

A row of `context_locals` costs 9x as much as a row of `locals`. The reason is
that `context_locals` is a lattice: whenever a row's decision set grows, the
row goes back into the delta and is propagated again. At 480 s the runs had
done 626 M set unions, and 252 M of them grew a set, which is about 6
re-propagations for every row. `call_target_assign_like` is expensive for a
different reason. Its transitive rule lists `tag_closure_func` first, so in
the variant driven by the `assign_like` delta, the rule walks every
`call_target_assign_like` row of every tagged function on each iteration.

## What in the program produces it

### Functions: Kotlin lambdas that R8 merged, and the Flow and coroutine plumbing

Concentration by function (480 s dump):

| relation | functions with rows | top 10 hold | top 100 hold |
| --- | ---: | ---: | ---: |
| `context_locals` | 1,036 | **75.9%** | 97.6% |
| `locals` | 66,106 | 40.0% | 71.7% |
| `call_target_assign_like` | 12,237 | 34.5% | 64.6% |

The top 10 by `context_locals` are all coroutine bodies or Flow operators:
`PictureInPictureIntegration$start$1.invokeSuspend` / `.invoke`,
`WindowFeature$start$1.invokeSuspend`, `FlowExtKt$flowWithLifecycle$1.*`,
`FlowKt__MergeKt$flatMapConcat$$inlined$map$1.collect`,
`FlowKt__ShareKt$launchSharing$1.invokeSuspend`, `SafeFlow.collect`,
`BrowserApplication$onCreate$7.invokeSuspend`, `AbstractCoroutine.start`,
Room's `TransactorKt.startUndispatched`.

Looking at these functions in the IR shows why they are dense. **R8
horizontal class merging** has fused unrelated lambda classes into one class.
Every method of the merged class loads `this.$r8$classId` and switches on it.
R8 allows at most 30 members per group. In ceno, 1,007 merged classes
contribute 1,725 merged methods. 85 merged `invokeSuspend` methods contain
446 suspend-lambda bodies, and 422 merged `invoke` methods contain 2,776
lambda bodies. The analysis does not track which way the switch goes. So a
flow, or a decision, that reaches one branch reaches all of them.

Comparing like with like shows the effect:

| | functions | `locals`/fn | rows per variable | `context_locals`/fn |
| --- | ---: | ---: | ---: | ---: |
| `invokeSuspend`, not merged | 275 | 5,934 | 168 | 60 |
| `invokeSuspend`, merged | 82 | **226,032** | **958** | **242,655** |
| `invoke`, not merged | 277 | 3,715 | 99 | 115 |
| `invoke`, merged | 415 | **47,985** | **403** | **17,022** |

A merged `invokeSuspend` contains about 5 bodies on average, but it is about
7x denser per body in `locals` and about 800x denser per body in
`context_locals`. Merging does not add the bodies' costs together; it
multiplies them.

Of the five F-Droid apps, only ceno is class-merged: 1,007 merged classes,
against 0 in darkcoin, greenbits and komodo. Darkcoin has more
coroutine code than ceno (1,221 `invokeSuspend`, 1,282 hybrid sites with a
coroutine shape) and finishes in 167 s. That is the answer to the question
the scaling report left open: what separates ceno from darkcoin.

### Instructions: continuation fields and static sentinels

Inside a merged `invokeSuspend`:

- `%v1 = load this.$r8$classId; goto <30 targets>` selects among the fused
  bodies.
- `store this.L$0 := ...`, `store this.label := ...` and `this.this$0` are
  where each body keeps its locals across a suspension. R8 retyped these
  fields to `Object`, so all 30 bodies share them. Every value that any body
  spills can be reloaded by every other body.
- `DistinctFlowImpl.collect(collector, this)` passes the continuation
  (`this`) into Flow code, and that code eventually resumes it. This is the
  hybrid site where decisions arrive.
- `load $globals.<Unit.INSTANCE>`, `<CoroutineSingletons.COROUTINE_SUSPENDED>`,
  `<AwaitKt.SEALED>`, ... are reads of sentinel values, each of which becomes
  a formal-sourced flow (see below).

Within that one function the dataflow is nearly a clique. Its 1,370 variables
face 968 distinct (formal, path) sources, and the busiest variables each reach
more than 500 of them.

### Data flow: decisions pile up where the builders meet

The hybrid sites in the functions that hold `context_locals` rows use these
dispatch keys: `Flow.collect` (the functions containing it hold 56% of the
rows), `StateFlow.getValue` (36%), `FlowCollector.emit` (28%) and
`Function2.invoke` (25%). The CoroutineContext methods `getContext`,
`getCoroutineContext`, `get(Key)` and `plus` appear in functions holding
10-20% each. These shares overlap.

The chain that produces the decisions starts at the builders:
`launch$default` (218 call sites in 165 functions), `withContext` (95 sites),
`async$default` (21), `coroutineScope` (15) and `runBlocking`. Each of these
receives a lambda object as its `block` argument and passes it through the
shared kotlinx machinery until the machinery calls
`create`/`invoke`/`invokeSuspend` on it. Because a single `launch` body serves
every caller, hybrid inlining has to record a decision for each lambda that
reaches the builder's `block` formal, and then carry those decisions down
through the machinery. Once a decision resolves to a merged class, the
class's merged body brings in all 30 of its lambdas.

### Constant statics: half of `locals`

49% of `locals` rows come from the `$globals` pseudo-formal, that is, from
static-field reads, which codegen threads through every call. Classified:

| kind of static | share of global-sourced rows |
| --- | ---: |
| enum constant / self-typed singleton | 42.7% |
| kotlin(x) internal static (almost all sentinels: `AwaitKt.SEALED`, `COMPLETING_*`, `BufferedChannelKt.SUSPEND`, `SelectKt.STATE_*`, ...) | 30.4% |
| other static (mostly constants such as `EMPTY_OBJECTS`, `Composer.Empty`, `$VALUES`; a few real caches) | 16.8% |
| `R$*` resource ids | 5.0% |
| `Boolean.TRUE/FALSE` | 2.2% |
| primitive statics | 1.7% |

Across the whole app, only 283 static fields are stored outside their own
class's `<clinit>`. A constant cannot carry taint, but under the current
lowering it flows up from a function deep inside kotlinx, through that
function's summary (`ret <- $globals.<SEALED>`), into every caller, and then
spreads through the dense functions described above. R8 has also retyped the
kotlinx `Symbol` sentinels as `io.sentry.BaggageHeader`, which is why that
name shows up in the paths.

## Experiments that show cause, not just correlation

All runs were stopped at 480 s unless stated otherwise. "Fixpoint" means the
run finished within the timeout.

| change | bodies not analyzed | outcome | peak |
| --- | ---: | --- | ---: |
| default | 0 | times out | 31.8 GiB |
| `--hybrid-context none` | 0 | times out; `call_target_assign_like` grows to 43 M / 12 GiB, the context-free relations double, fewer iterations (76 vs 114) | 26.4 GiB |
| `--hybrid-context spill:1` | 0 | times out; `call_target_assign_like` 38 M / 11 GiB | 36.6 GiB |
| constant statics (`CTADL_CONST_STATICS=1`) | 0 | times out; `locals` -39%, `edge_split` -27%, 5 more iterations; contextual relations unchanged | 27.3 GiB |
| skip kotlinx.coroutines core + `kotlin/coroutines` | 763 | **fixpoint, 114 s** | **8.3 GiB** |
|   `kotlin/coroutines/` only | 73 | times out, no change | 30.0 GiB |
|   `channels`/`selects`/`sync` only | 161 | times out, no change | 32.1 GiB |
|   `internal`/`scheduling`/`intrinsics` only | 168 | times out, **worse** (`locals` 231 M) | 44.9 GiB |
|   top-level `kotlinx/coroutines/[A-Z]*` | 361 | **fixpoint, 96 s** | **8.4 GiB** |
|     Job state machine (`JobSupport*`, nodes; 240 s) | 156 | times out | 25.2 GiB |
|     continuation dispatch (`CancellableContinuationImpl`, dispatchers; 240 s) | 128 | times out | 21.4 GiB |
|     all of R8's `AwaitKt` facade (240 s) | 37 | **fixpoint, 139 s** | 12.3 GiB |
|       only the 10 block-taking builders (240 s) | **10** | **fixpoint, 148 s** | **14.0 GiB** |
|       the other 27 `AwaitKt` functions (240 s) | 27 | times out | 27.8 GiB |

Three conclusions follow.

1. **The call-graph context knob moves the cost; it does not remove it.**
   With `none` or `spill:1`, the flows that were stored per caller get stored
   in `call_target_assign_like` and in the context-free closure instead. This
   is the same effect the scaling report saw on darkcoin.
2. **Ten functions are the lever.** When the builders are taken out, ceno
   stops being a non-terminating 96 GiB run and becomes a 2.5-minute, 14 GiB
   run. The skipped version reaches a fixpoint with 33 M `locals` rows at
   18.4 rows per variable, and 2,168 (site, target) resolutions over 6,004
   indirect sites.
3. **Constant statics are a real saving, but they are not the cause of the
   blowup.** They remove about 40% of the context-free rows, and the saving
   adds to the one from the builders. At the fixpoint reached with the
   coroutine bodies skipped, 49.6% of `locals` is still constant-static flow.

### Does this generalize?

The same builder model was tried on the two other large apps that use
coroutines:

| app | R8-merged classes | builder bodies skipped | default | builders skipped |
| --- | ---: | ---: | --- | --- |
| ceno | 1,007 | 10 | times out (5,400 s, 96 GiB) | **fixpoint, 148 s, 14 GiB** |
| darkcoin | 0 | 15 | fixpoint, 167 s, 16.5 GiB | fixpoint, 163 s, 16.6 GiB; `locals` -12% |
| greenbits | 0 | 20 | times out (killed at 116 GiB) | still times out at 900 s, 67 GiB, no change |

So the builders are only catastrophic in combination with class merging. In
darkcoin, `context_locals` never grows past 2 M rows.

**Greenbits fails in the same way but on a different funnel.** Its 900 s
census, which is the same with or without the builder model, has
`call_target_assign_like` at 48.5 M rows and 13 GiB, and `context_locals` at
35 M rows holding 15.4 billion memberships. Those are concentrated in
**Jackson databind**. `ObjectMapper._convert` alone has 7,899 decisions, sets
up to 2,820 wide and 4.6 billion memberships. Behind it come
`BeanPropertyWriter.serializeAsElement`, `ObjectMapper.readValue`,
`_readMapAndClose` and `_readTreeAndClose`. In general, the pattern is one
shared library function that thousands of decisions reach, that hybrid
inlining keeps unmerged per caller, and that sits over a body dense enough to
multiply them. Greenbits deserves its own deep dive, and the tooling here
applies to it unchanged. The likely lever there is modelling Jackson's
reflective (de)serialization entry points.

## Recommendations

### Calls that should be handled differently

**1. Model the coroutine builders as calls to their block, not as inlined
library code.** Today these calls go through ordinary CHA and direct-call
edges into kotlinx bodies, and hybrid inlining resolves the lambda deep
inside the machinery, one decision per lambda:
`launch`, `launch$default`, `async`, `async$default`, `withContext`,
`coroutineScope`, `runBlocking*`, `withTimeout*`. They live in
`BuildersKt`, `BuildersKt__Builders_commonKt`, `BuildersKt__BuildersKt`,
`CoroutineScopeKt` and `TimeoutKt`, or, in an R8-merged app, in `AwaitKt`.
The right model is "`launch(scope, ctx, start, block)` behaves like
`block.invoke(scope, <continuation>)` at the call site". At almost every such
site the lambda is allocated right there in the caller, so the local-dispatch
rule (`callee_info` x `call_target_assign_like` x `callee_resolvents`)
resolves the call exactly, without any context. The model language cannot say
this yet. A `bridge` must name a fixed target, and `forward_self`, which is
the nearest shape, is documented as schema-only. So this needs a small new
model kind, something like `forward_arg { argument, name, descriptor, args }`.
Skipping the builders, as the experiment did, is not an acceptable
substitute, because it loses flows like `val x = source(); launch { sink(x) }`.

**2. Treat reads of effectively final static fields as constants.** Only
static fields that some method other than their own `<clinit>` writes should
flow through `$globals`. The experiment is in `codegen/mod.rs` behind
`CTADL_CONST_STATICS=1`: a whole-program pre-pass plus a skip in `Load`,
which must also stay out of `cap_path`, or else a load-chain temporary puts
the read straight back. It loses a flow only when a `<clinit>` stores taint
into a static final, for example `static final String K = getenv(..)`.
TaintBench can measure that cost.

**3. Stop propagating call-target tags that no site can use.** 42% of
`call_target_assign_like` rows, about 7 M, carry a target class that
implements none of the (name, descriptor) pairs dispatched at any hybrid
site. `StringBuilder` alone has 867 k rows spread over 3,504 functions.
Codegen already collects `deferred_dispatch`; it could seed
`call_target_assign` only for classes that have a `callee_resolvents` entry.

**4. Do not use `skip-analysis` on kotlinx `internal`/`scheduling`.**
That makes things worse (`locals` goes from 75 M to 231 M). Those bodies are
what keep some dispatch precise.

**5. A de-merging pass for R8 classes, as a follow-up.** Clone each method
that switches on `$r8$classId` once per id, and tag each allocation followed
by `<init>(..., const k)` as `C#k`. This would undo, at its source, the per-body
density increase: about 7x in `locals` and about 800x in `context_locals`. It is more work than recommendation 1, and
recommendation 1 alone already gets ceno to a fixpoint, so it is listed
second.

### The engine

**6. Store `context_locals`/`ctx_edge_split` and `call_target_assign_like`
as BYODS relations, the way `locals` is.** A lattice key index should map to a
single row, not to a `HashSet<row>`. Doing that alone saves 6.9 GiB at 900 s.
`call_target_assign_like` at 327 B/row in five copies is a natural fit for the
`assign_like` trie shape.

**7. Reorder the transitive `call_target_assign_like` rule** so that no
variant has to scan every tagged function each iteration. It takes 15% of
rule time at 6.8 µs per row.

**8. Keep the patched `run_timeout`** (or send the fix to Ascent upstream).
Without it, `CTADL_INDEX_TIMEOUT_SECS` cannot characterize any run that does
not finish.

## Caveats

- The runs that were stopped by a timeout compare states after the same wall
  time, not results at a fixpoint. When a change makes a relation smaller at
  480 s, part of that may be that the run got further. Where it matters, the
  iteration counts are reported.
- The `skip-analysis` experiments show cause for memory. They say nothing
  about recall. The builder skip drops real flows through coroutine launches
  by design, and none of the fixes above has been checked on TaintBench yet.
  Most TaintBench apps predate Kotlin coroutines, so the suite may not
  exercise them at all.
- Two apps ran at the same time for some of the comparisons. Wall-clock
  timeouts therefore stopped them at slightly different points (for example
  the 480 s dump has 40 M `context_locals` rows against 43 M in the solo
  run). The attribution percentages hardly change.

## Code changes in this worktree (uncommitted)

- `vendor/ascent_macro` plus `[patch.crates-io]` in `Cargo.toml`: the
  `run_timeout` fix.
- `index_engine/census.rs`, `census_fields.rs` and the `log_census` /
  `dump_relations` macros in `index_engine/mod.rs`, which provide
  `CTADL_INDEX_CENSUS`, `CTADL_INDEX_DUMP` and the `[census]` log lines.
- `heap_stats` on `immortal::Interner`, `tailshare::interner_heap_stats`,
  `decision::set_heap_stats`, `AssignTrie::heap_totals` and
  `for_each_row` on both tries.
- The `CTADL_CONST_STATICS=1` experiment in `codegen/mod.rs`
  (`mutable_static_fields`, `is_const_static`).

## Raw data

```
/Volumes/Shampoo/ceno-deepdive/
  runs.log                         every run: args, exit, wall, peak
  runs/ie.equalit.ceno.<tag>/      index.err (full debug log, rule times, census), census.tsv, dump/
    t30 ... t900                   the timeout ladder
    dump480, const3_480, skipcoro480   runs with relation dumps (TSV + parquet)
    none480, spill480              --hybrid-context comparisons
    skip{A,B,C,D}_480, skip{B1,B2,B3,B2a,B2b}_240   the skip bisection
  runs/<darkcoin|greenbits>.*      the generalization runs
  models/                          the skip-analysis model files used
  ir/all.ir, ir/r8_merged_funcs.txt, ir/r8_arity.tsv   ceno IR and the class-merge inventory
  run.sh, cmp.py, ladder_table.py, rules.py, gen_census.py
```

Two sets of runs in that directory are not results, and should not be read
as results:

- `const480` and `const2_480` ran with an owner-parsing bug that put every
  stored static field in the "mutable" set. `const3_480` is the valid run.
- The `*.skipB2a*` runs on darkcoin and greenbits matched no function,
  because in apps without merging the builders are not in `AwaitKt`. The
  valid runs are `*.skipbuild*`. Since those two runs changed nothing, they
  are effectively default runs, and that is how the greenbits census above
  was obtained.
