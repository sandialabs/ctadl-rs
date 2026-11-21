# Making CTADL do less work

CTADL's expensive part is usually the amount of data it creates while analyzing a program. Reading the input can be quick, while indexing spends minutes building millions of relationships between variables, fields, calls, and function parameters. A small relationship copied into every caller can become a very large relationship across the program.

Our performance work has mostly followed that observation: avoid reading the same bytes twice, make joins look up matching rows directly, keep shared information once, and stop a query when it has answered the question it was asked.

This document explains the changes, the experiments behind them, and the details needed to implement the ideas again. The same problems show up when analyzing APKs, lifted native code, or any other input lowered into CTADL's intermediate representation. Each measurement belongs to its own experiment. The numbers below are not successive steps in one benchmark, and their speedups should not be multiplied together.

## What the terms mean

An **import** decodes an artifact and lowers it into an intermediate representation, or IR. **Indexing** computes reusable data-flow and call relationships. A **query** follows those relationships from selected sources to selected sinks.

The index uses relations, which are tables of facts. A few names matter here:

| Relation or term | Plain meaning |
| --- | --- |
| `assign_like` | A directed assignment or an assignment obtained by applying a function summary. |
| `locals` | Which function inputs can reach a variable and access path inside that function. |
| `summary` | How a function's inputs affect its outputs, including changes to shared memory. |
| Access path | A route through a value, such as `.options.url`, or a sequence of offsets and dereferences. |
| Call-target tag | A possible function or receiver class attached to a value. |
| Decision | The condition under which an indirect call resolves to a particular target. |
| Fixpoint | The point where repeatedly applying analysis rules produces no new information. |
| Global heap | The shared object used to represent program-wide state. It is passed through function boundaries as an implicit parameter. |

There are three kinds of evidence in this document. The early import and join changes have repeated baseline/candidate measurements and detailed output comparisons. Several later changes are present in the working implementation and have focused regression cases, but do not have a retained, isolated benchmark for their final form. Related native-analysis work has its own recorded experiments and sometimes changes the analysis abstraction. We label those separately rather than treating everything as one result-preserving speedup.

The working tree already includes performance foundations from the base engine: compact interned values, grouped relation stores, pruned SSA, parallel IR passes, and demand-driven query search. The local work builds on these; it did not introduce all of them.

## 1. Read and hash inputs without allocating the whole artifact (processed)

### What was going wrong

Format detection read an entire package into memory just to decide which reader should handle it. The archive reader then opened the package again. A large package with a tiny bytecode member paid for an archive-sized allocation before the useful parsing started.

Artifact hashing separately read each regular file into one large byte buffer. That made hashing memory grow with artifact size even though SHA-256 can consume a stream.

### What we changed

Detection now reads eight signature bytes. If the signature identifies a raw bytecode file, the reader obtains the rest of that file. Otherwise the archive reader handles the package without a preliminary whole-package buffer. Detection still follows the content, so renaming an input does not change which decoder gets it.

Regular-file hashing now opens the input once and feeds SHA-256 through a reusable 64 KiB buffer. The loop retries interrupted reads, hashes only the bytes actually returned, and stops at end of file. Empty files still produce the usual digest. Directory hashing retains its existing ordering and framing.

To reproduce this change, preserve the old digest contract exactly. Replacing a content hash with a timestamp or a file size would change freshness checks. Also keep the distinction between a small detection read and the full read that a decoder may actually need.

### What we saw

In three-run comparisons, a resource-heavy package of roughly 159 MB, containing only about 27 KB of bytecode, had these import results:

| Measure | Before | After |
| --- | ---: | ---: |
| Median peak resident memory | 170.2 MiB | 18.4 MiB |
| Median import time | 546.8 ms | 526.7 ms |

That is about 89.2% less peak resident memory and 3.7% less import time. A larger code-heavy application went from 476.3 to 454.9 MiB during import, about 4.5% less memory.

These measurements tested the I/O changes together with the summary-join change described next. They do not isolate the contribution of hashing from detection. The clearest result is the removal of the large temporary import buffer.

Small APKs did not establish an import-time benefit. Their before/after import medians were 32.8/33.9 ms, 128.1/132.0 ms, and 249.3/250.6 ms. Peak memory was approximately unchanged. Those inputs were only 124 KiB, 547 KiB, and 1.5 MiB, and did not exercise the same format-detection path. Streaming is still useful for bounding hashing memory, but these tests do not establish a large-APK speedup.

### How we checked it

The hash test compares streamed and whole-buffer digests for 0, 1, 65,535, 65,536, 65,537, and 2,000,003 bytes. Those sizes cover empty input, both sides of the buffer boundary, and multiple reads.

The initial import comparison covered nine distinct packaged applications, two extracted raw bytecode inputs, and two renamed inputs, across 21 paired imports. Decoded IR, typed dispatch metadata, import settings, artifact digests, and all five source-information tables matched. Serialized IR was byte-identical in every pair. A later four-application, three-repetition comparison passed the same checks again.

## 2. Pull a small relation out of a large self-join (processed)

### What was going wrong

Consider this code:

```text
object = Global.session
object.address = input
```

The function summary needs to say that `input` reaches `Global.session.address`. The old rule discovered that by joining `locals` with itself, then filtering for the rows that represented a base loaded from the global heap.

Most `locals` rows were irrelevant to that question. One large application had 16.7 million local-flow rows in an early baseline. Scanning them repeatedly to find a much smaller set of global bases was expensive.

### What we changed

We derive a filtered relation first:

```text
global_base(function, variable, global_parameter, global_path)
```

A row belongs there when a whole local variable comes from a nonempty path of the global parameter. The summary rule starts from that smaller relation and probes `locals` by function and variable to find writes through the base. It then combines the global path and stored path using the existing path rules.

The original experiment only introduced the projection. It still generated an unfavorable full scan. The useful version also put the small relation first in the join. That detail matters in a generated rule engine: an equivalent-looking rule can produce a different loop nest.

For a new implementation, retain the original aliasing switch, path filters, store checks, and result tuple. First optimize the exact rule. Any additional change to which stores are summarized needs its own correctness review.

### What we saw

Both builds used four index workers. Medians are from three fresh-store runs per build:

| Workload | Baseline index | Candidate index | Less time |
| --- | ---: | ---: | ---: |
| Large application | 30.141 s | 25.620 s | 15.0% |
| Medium utility | 405.9 ms | 358.5 ms | 11.7% |
| Small browser | 58.5 ms | 58.2 ms | Essentially unchanged |
| APK: small installer | 118.1 ms | 114.9 ms | 2.7% |
| APK: four-DEX application | 639.0 ms | 584.4 ms | 8.5% |
| APK: larger Java application | 1,087.8 ms | 1,015.8 ms | 6.6% |

The rule profiles give stronger evidence of the mechanism than the wall times alone. In one paired large-application run, the two old global-summary rule expansions took 3.863 seconds together; the new expansions took about 51 ms. In the larger Java APK, the old expansions took 71.536 ms. The new ones took 2.693 ms, plus 2.290 ms to derive `global_base`, for 4.983 ms total.

The large application's overall indexing memory remained around 7.8 GiB. Removing one expensive join did not remove the other multi-million-row relations.

The timing spread was substantial: large-application indexing ranged from 28.836–34.183 seconds before and 25.414–55.919 seconds after. The slow candidate repetition also had slower import and query phases. Three repetitions on a shared host support a useful local observation, not a stable percentage for every machine.

Other phases sometimes got slower: the medium utility's import went from 112.6 to 116.9 ms, the resource-heavy package's index from 35.2 to 36.9 ms, and the small browser's query from 44.6 to 46.8 ms. Query execution was unchanged in this comparison. These runs establish neither a query speedup nor an improvement in every phase.

### How we checked it

All ten saved index relations matched as sorted multisets for the first repetition of each of four packaged inputs: 40 table comparisons. All nine paired APK runs passed the same ten-table comparison: another 90 comparisons. Function IDs were resolved to names, and duplicate row counts were preserved.

The APK imports also matched in full decoded IR, dispatch metadata, configuration, and source information. All four DEX members of the multi-DEX sample were imported, totaling 7,621 functions. Its packaged native library was skipped in both builds because the native disassembler was unavailable. Those results cover the DEX portion only. The larger Java sample had 18,056 functions.

Across the two applications with existing source-to-sink answer keys, all 96 checks passed: 16 cases, three repetitions, and two builds, including exact endpoint counts and negative controls. There was no APK-specific query answer key in this experiment. The focused engine suite passed 32 tests, including serial/parallel comparisons.

## 3. Use parallel indexing where there is enough work to pay for it (deferred)

### What we changed

The parallel-index port runs the same flow rules on a selected number of workers. It keeps the same facts and completed call graph. It does not parallelize decoding merely because the index has more workers.

The thread convention is one worker for serial execution, zero for the available CPU count, and a larger number for an explicit worker count. The current command-level selection also chooses the serial engine below 50,000 input assignment facts, even when the caller requests several index workers. The implementation notes record serial being faster and smaller around 45,000 assignments, with parallel becoming useful from about 160,000. That motivated a practical cutoff; it does not establish a universal crossover point.

### What we saw

An earlier comparison ran serial indexing against four-worker indexing on an ARM64 macOS host reporting 18 logical CPUs. Each result is a median of three runs:

| Workload | Serial index | Four-worker index | Serial time / parallel time |
| --- | ---: | ---: | ---: |
| Small browser | 0.077 s | 0.129 s | 0.60× |
| Large application | 117.679 s | 45.213 s | 2.60× |
| Medium utility | 0.509 s | 0.453 s | 1.13× |

The large application's median peak resident memory increased from 7,755 to 8,884 MiB, about 15%. The small browser got slower because scheduling and parallel data structures cost more than they saved.

This comparison predates the later automatic small-input cutoff. It measures explicitly selected serial and parallel engines, not the final cutoff policy. It also does not determine the best thread count. Import code was identical, so differences in import timing cannot be credited to parallel indexing.

### How to implement and validate it

Keep rule definitions shared between the engines. Run the same input facts through serial, two-worker, four-worker, and eight-worker configurations, then compare the complete results. The focused tests exercised callbacks, global state, and dynamic calls at those worker counts.

The measured experiment completed all 18 imports and indexes. Ten saved relations matched as normalized multisets on the first repetition of each workload, including 4,207,973 assignments and 2,117,851 summaries in the large input. Internal relation counts matched across all repetitions, and all 96 answer-key checks passed.

For batch analysis, account for both levels of concurrency. Several imports each launching an all-core index pool can use far more memory and CPU than expected. A per-process worker setting and a batch-process limit need to be chosen together.

## 4. Turn repeated multi-relation scans into exact two-way joins

The small-global-base rewrite led to a broader rule: materialize the stable part of a join once, then let new rows probe it by the full matching key.

CTADL uses incremental rule evaluation. A relation has old rows and newly discovered rows. Both update directions need to be cheap. A rule that performs well when a new tag arrives may still rescan a function's entire tag set whenever a new assignment arrives.

### Critical arguments and resolved sites

The later implementation introduces a few helper relations:

| Helper | What it remembers | Work it avoids |
| --- | --- | --- |
| `critical_site` | The caller's argument vertex for a callee input that can affect indirect dispatch. | Reconstructing call/parameter pairings for every arriving target tag. |
| `critical_arg` | The useful prefix/suffix splits of that critical argument's path. | Repeating path splitting and call lookup for every local-flow row. |
| `critical_reach` | Which caller inputs reach those critical argument paths. | Pairing every local at a formal with every target-resolution decision. |
| `site_resolution` | A resolved target and its decision at a particular call site. | Repeating dispatch resolution for every new summary of that target. |
| `global_write_arg` | Which actual argument path a call writes into which global path. | Rescanning summaries and callers whenever a callable tag arrives. |

For example, join `critical_arg` with `locals` on the caller, argument variable, and exact path first. Only the surviving rows meet target-resolution decisions. Similarly, join a new summary directly to `site_resolution` by target function.

These helpers are filtered projections and remembered join results. They need all the keys that distinguish call sites, formals, paths, and decisions. Leaving out a decision can accidentally turn a conditional callback's effect into an unconditional effect for every caller of its builder.

Focused regressions cover that case: a builder called with two different callbacks must not give the second caller the first callback's flows. The final local forms of these helpers do not have a retained isolated timing comparison, so we describe the avoided work without assigning a percentage to them.

### Call-target propagation by split path

For an assignment:

```text
destination.destination_path = source.source_path
```

a tag at `source.(source_path + rest)` belongs at `destination.(destination_path + rest)`. The expensive version joins tags and assignments by function and tests their paths afterward.

The current local version splits each tagged path into `(key, rest)` and indexes the resulting tag rows by `(function, source_variable, key)`. An assignment now meets only tags whose prefix matches. A separate offset-aware half retains numeric prefix arithmetic. The empty path explicitly has the split `(empty, empty)`, even when it is absent from the registered path set.

A related APK experiment measured two implementations of this idea: splitting every tag, and splitting each distinct tagged path before expanding the matching assignments. These are alternative representations, not two changes to stack blindly:

| Workload and measure | Original join | Split each tag | Split each distinct tagged path |
| --- | ---: | ---: | ---: |
| APK A, default: target-propagation rule | 20.2 s | 0.5 s | 0.3 s |
| APK A, default: whole fixpoint | 63 s | 43 s | 45 s |
| APK A, default: peak footprint | 2.21 GiB | 2.51 GiB | 2.54 GiB |
| APK A, more conservative stack promotion: rule | 1,091 s | 7.1 s | 3.9 s |
| APK A, same setting: whole fixpoint | 1,388 s | 296 s | 289 s |
| APK A, same setting: peak footprint | 13.32 GiB | 17.03 GiB | 16.23 GiB |
| APK B: target-propagation rule | 11.6 s | 0.35 s | 0.24 s |
| APK B: whole fixpoint | 74 s | 59 s | 68 s |
| APK B: peak footprint | 3.30 GiB | 3.68 GiB | 3.36 GiB |

The first representation created 30.8 million split-tag rows in the conservative run. Splitting distinct paths reduced that overhead where several tags shared one path, but still left a 2.9 GiB increase over the original peak. The current local implementation uses the per-tag split: expanding assignments per distinct path can cost more when most paths hold only one target.

The related experiment checked full relation sets on APK B. On APK A it checked relation counts under both settings and the existing finding; that is weaker than full row equality. The focused index and analysis tests passed, but the complete regression suite was not run for that experiment. Some timings overlapped other work, so the rule profiles are more persuasive than small differences between whole-fixpoint times.

## 5. Keep shared global information once

### Avoid turning common method names into a program-wide call cycle

A name-only call such as `get` or `set` can match methods in many unrelated classes. Adding every match as a direct call edge joins otherwise separate libraries into a recursive component. Their summaries then feed one another, and callers inherit the combined global effects.

The current dynamic-name policy keeps the direct name union when there are at most two application targets. Above that threshold, when receiver-based dispatch is available, it defers the application targets to receiver-class resolution. The external stand-in remains a direct target so boundary models still apply. Callable-value resolution remains available independently of the method name.

The implementation notes record a motivating input failing to reach a fixpoint within 20 GB with a threshold of four, while two indexed in about a minute under 5 GB. A threshold of one lost useful two-target call edges because receiver recovery was incomplete. These are observations recorded with the implementation, not a retained repeated comparison of the final build.

The threshold changes the call-resolution abstraction. Record it with the saved index and test the call edges that should survive. Receiver recovery must carry class identity through construction and returns; lowering the threshold without that evidence can simply remove useful analysis.

There is also an exact shortcut below the threshold: once the direct name union already contains every class-hierarchy target, repeating receiver-based resolution of the same name adds no target. We removed that duplicate query, while retaining differently named assigned callables and receiver-only calls. Otherwise the duplicate work makes receiver parameters critical and expands conditional summaries unnecessarily. A focused regression requires both same-name targets and a differently named assigned callable to survive.

### Publish callable values where they are read

Every call carries the global heap. Passing a separate dispatch decision for every global callable through every call edge creates routes that do not distinguish meaningful caller contexts.

The current implementation publishes callable targets by exact global path. Functions consume that information where they actually read or dispatch through that path. A callable returned through the global heap is propagated to a caller only where the caller has dispatch demand for it. Ordinary return values and non-global output parameters still propagate normally.

This needs a complete publication path: direct stores, writes described by summaries, callables returned by callees, and forwarding frames all matter. A stored callback must remain available even if the function that registers it never invokes it. Focused cases check installation followed by dispatch two calls down, unrelated callers, and callbacks inside conditional builders.

This is also a precision issue. Avoiding a redundant global decision must not make a callback's local effects unconditional. The final local implementation has regression coverage for those distinctions, without an isolated end-to-end benchmark.

### Do not propagate receiver tags that nothing can resolve

Before the fixpoint, collect the receiver-class objects named by the dispatch-resolvent table. A class tag absent from that set cannot resolve a call through that table, so the current implementation removes it from the initial tag assignments.

Function-valued tags stay, even when no ordinary resolvent names them: callback registration can invoke them directly. If a future consumer uses class tags for another purpose, this filter must account for that consumer too. The justification is the set of consumers, not the apparent size of the class.

### Store global-to-global moves separately

Suppose a function says:

```text
Global.destination = Global.source
```

Applying that summary at every call site repeats a relationship between the same shared object. It then appears again in summaries of transitive callers. The implementation notes report that these heap-to-heap rows made up 96–98% of summaries in the motivating workloads, with each move held 8–37 times. Those are recorded observations, not a retained repeated benchmark of the final rewrite.

We now keep a separate relation:

```text
heap_move(source_path, destination_path)
```

Implementation details that matter:

1. Extract a move only when both summary endpoints are the global-heap parameter. Argument-to-global writes and global-to-return reads remain function-specific.
2. Stop instantiating heap-to-heap summaries into an ordinary assignment at every call site.
3. Close the move relation under path-aware composition. Handle both a move reading below another move's destination and a move reading an ancestor of that destination. Keep the existing finite path vocabulary.
4. Propagate writes from non-global inputs through the moves. `Global.a = argument` followed by a move from `a` to `b` must still yield the caller-specific effect `Global.b = argument`.
5. Move callable tags through the same relation, so relocating a global function pointer still resolves the later indirect call.
6. Persist the move relation and change the index format version. A query cannot interpret the new, smaller summaries using the old storage contract.

For the composition step, `a → b` followed by `b.field → c` gives `a.field → c`. A move `a → b.field` combined with `b → c` gives `a → c.field`. Admit the derived paths through the same path-set checks used by local propagation. Apply the same two prefix cases when a function writes an argument into the heap; do not turn that argument into a program-wide value.

The forward query stores these move edges once and applies them at heap roots. It starts with functions that have no heap caller, walks downward from them, and also retains functions left outside that reachability set, including isolated call cycles. Heap taint can return upward to a root in the search's unrestricted state, move there, and enter callees again. Applying every move in every function would recreate much of the duplication during the query.

This design relies on CTADL's shared, flow-insensitive global abstraction. It would need a different argument in an engine that distinguishes separate heap instances or execution order. It changes the saved representation, so literal equality with the old summary and assignment tables is not the right acceptance test. Compare reconstructed effects, call targets, and query outcomes instead. Focused cases cover a global copy, a clean unrelated global, argument writes crossing the move, and a relocated callable. No final repeated speed or memory figure is established here.

## 6. Make storage fit both tiny and huge groups

Most relation keys have very few values. A few keys have thousands. Making every group a large hash table wastes memory; scanning every group linearly makes the outliers expensive.

An earlier local change kept assignment rows in insertion order and added hash membership after 64 entries. That removes repeated linear duplicate checks in large groups while preserving traversal order. Its APK checks retained exact multisets across ten saved tables on three inputs. It had no isolated speed benchmark.

The current base engine has a more developed grouped store. Its small sets use compact open addressing and promote to a Swiss-style hash layout above 64 entries. Merges insert the smaller set into the larger one. Groups also promote from a flat set to a map keyed by access path, so looking up one path does not scan thousands of sibling-path rows.

To recreate the useful properties, implement these operations together: membership, insert, merge, exact-path lookup, complete iteration, and conversion back to owned rows. Test promotion boundaries, duplicate insertion, unions in either order, and concurrent insertion in the parallel store. Keep witness ordering explicit in the query layer; a hash container's iteration order is not a stable path-selection policy.

The available synthetic benchmark design holds total local-flow rows roughly constant while sweeping group sizes 1, 2, 4, 8, 16, 32, 64, 65, 128, 512, 2,048, and 8,192. It separately varies distinct paths per group and records bytes per row, total peak memory, fixpoint time, and iteration count. That is a useful way to separate a representation problem from simply having more facts. We do not have a retained result table here establishing a fresh speedup for the current store.

## 7. Make queries answer the requested question once

### Reuse the demand-driven search already in the base

The base engine searches an implicit graph instead of building the entire taint closure and all alias pairs. A search state includes the function, variable, access path, taint level, object scope, and call/return annotation. Edges are expanded as needed.

The object-scope bit is essential. Copying a value forward does not justify flowing backward through every other input of a merge. Field aliasing and value dependence have different directions. Collapsing everything into one undirected copy class can create both false flows and a much larger search.

The local work also distinguishes original program/ABI assignments from summary-induced dependency edges. A summary saying that a return depends on a global field is not proof that the returned object aliases that global. Reverse alias expansion uses genuine assignment evidence, rather than treating every summarized dependency as an alias.

Saturating taint also stays under its selected field. Taint at `.result` can reach `.result.child`, but does not automatically reach the sibling `.headers`. That repair preserves useful descendants without opening every field of the base object. It was checked with positive descendants, copies, global stores and reloads, and negative sibling reads. It is a precision repair; no isolated query speedup was established for it.

### Stop when every requested sink has a witness

For a path query, once every requested sink vertex is reached, walking the rest of the reachable program adds no needed witness. The search now stops there. It retains one breadth-first shortest witness per sink vertex for a source set, with deterministic edge order for tie-breaking.

This helps positive queries with a large irrelevant reachable tail. A negative query may still need to exhaust the graph. A closure-style consumer that asks for every tainted instruction needs its broader result contract preserved; it cannot silently inherit the path-report shortcut.

The result emission keeps source and sink endpoint records, reached call-argument states needed by downstream reporting, and the edges on found witness paths. It avoids persisting the whole search forest as report data.

### Share searches across labels with identical starts

Different source labels sometimes describe the same starting graph nodes. Those labels search the same graph for the same sink set.

Build a canonical start-node vector, including taint level and object scope. Cache the search result by that vector within the query, then emit it separately for each label with the proper endpoint attribution. Compute the last label that uses each vector and release the cached result after that label. This avoids retaining every search result for the lifetime of the query.

The regression includes equal source sets separated by an unrelated label. Both equal-set labels must still report their flows. If graph policy, sink sets, or budgets become label-specific, those settings must become part of the cache key. There is no retained isolated timing table for the final implementation.

### Index dense symbolic prefixes

A global variable may have thousands of outgoing assignments at unrelated field paths. For each tainted path, scanning all of them wastes time.

For source groups with at least 64 edges, the query builds buckets by symbolic source prefix. A taint at `.session.request.url` probes the empty prefix and the successive symbolic prefixes, then examines only those candidate edges.

Numeric paths keep a fallback scan because prefix matching can perform offset arithmetic. Candidate row positions are sorted back into the original edge order, preserving witness selection. A focused test compares every successor and its order against the unindexed scan, including mixed symbolic and numeric cases. This is checked behavior, without a final standalone speedup measurement.

### Keep composed paths useful and finite

Code can store a value under `.slot` and later read `.url` from it without spelling `.slot.url` in any instruction. Rejecting every unregistered concatenation loses real flows. Allowing unrestricted concatenation can create paths such as `.slot.slot.slot.url` around a cycle.

The current implementation keeps the registered path vocabulary and adds concrete endpoint paths and their suffixes. It admits temporary composed paths where an assignment, a nested read, a sink, or a call boundary can consume them. The length bound is twice the longest registered query path. The composition checks also require a useful combination of materialized pieces; a length limit alone still allows too many cyclic combinations.

For stores, permit a materialized result, a materialized residual suffix, or a result that can be split into two materialized pieces. The split need not coincide with the store boundary: `.slot.data` plus `.content` can be valid when the store itself only introduces `.slot`. ABI-related copies retain useful transients until the adjacent function can read them. Inert copy destinations need not receive every temporary composition.

Whole-heap self-moves need an additional restriction: composing them only into materialized paths prevents one heap alias from repeatedly nesting every tainted global path.

Focused cases cover recursive stores, nested load chains, crossing a call boundary before the first read, exact nested sinks, aggregate sinks, and unrelated sibling fields. Aggregate matching reports the descendant actually consumed; it does not create a child-to-parent value edge that another witness could use to invent a sibling flow. These rules define a finite abstraction and need their own completeness tests. They should not be described as a proven equivalence to unbounded paths.

### Treat budgets as incomplete analysis

The current default path-search limit is 20 million states per source label; zero removes the command-line limit. The implementation notes associate that scale with roughly 5 GB and seconds of search, and say the saved motivating cases fit comfortably inside it after the other repairs. Those comments are useful operating observations, not portable resource guarantees.

If the budget is reached, the output marks the search as limited, including a diagnostic and affected labels. A query that timed out or ran out of states did not prove that a flow was absent.

Path-report mode can also restrict callee entry to functions that can reach a sink frame. That relies on the existing call discipline: entering a callee makes the search restricted, so it cannot later return to find a sink elsewhere. It is specific to that search contract.

An optional quick pass tries composed paths for 50,000 states, then a smaller path vocabulary for up to five million states. A shortcut is accepted only if it finds a witness for every requested sink. Exhausting the smaller graph cannot prove a negative; unresolved work falls back to the ordinary bounded search.

## 8. Avoid loading or converting data that a task does not need

### Persist callback provenance for discovery

Policy discovery previously loaded large assignment and call-target tables to reconstruct callback bindings. The index now saves callback-registration provenance, allowing discovery to use the small dedicated table. Older indexes can take the reconstruction fallback when that provenance is absent.

Three-run medians on the same saved indexes changed as follows:

| Discovery workload | Before | After |
| --- | ---: | ---: |
| Callback-heavy utility | 0.69 s | 0.07 s |
| Request-heavy utility | 1.79 s | 0.08 s |

These are discovery-only timings. The discovery change also changed the proposed sink inventory: 3 to 8 in the first input and 3 to 5 in the second. Source counts stayed at 10 and 8 respectively. The timing comparison therefore supports the cost of the new discovery path, without establishing identical discovery output or faster whole-program analysis.

### Cache path text within one serialization batch

Saved fact columns contain repeated access paths. The writer now formats each distinct path once per batch, then reuses that formatted value. The reader similarly parses repeated path strings once per batch.

Keep the cache local to the batch. Preserve empty paths, escaping, Unicode, numeric offsets, repeated rows, and the existing column schema. The equality regression compares cached encoding against ordinary per-row encoding and checks the decoded paths. The writer still clones the resulting strings into its column buffer; this change avoids repeated formatting, not all per-row allocation. No whole-index speedup was measured for it.

### Keep supporting memory work visible

The base engine saves input facts by borrowing them rather than cloning the complete fact base, including large assignment vectors that are not even written in that step. It interns paths, strings, target objects, dispatch keys, and decision sets as compact four-byte handles. Most immutable values are prepared before the fixpoint, keeping interner writes and locks out of hot rule evaluation; decision-set unions remain a separate case.

Pruned SSA adds merge nodes only where the variable is live. Independent functions can run their IR passes on bounded workers. To test a parallel IR pipeline, begin each worker-count run from the same imported program and compare complete transformed IR, including block, statement, merge-node, and load counts. One very large function remains a serial lower bound.

These are existing foundations. The records reviewed here do not provide a separate final A/B percentage for each one.

## 9. Related native-analysis work: fix the abstraction before tuning the container

The following experiments belong to separate native-analysis and engine work recorded in repository history. They are useful for pcode-based analysis and native libraries inside APKs. They are not all present in the current working implementation, and they were not benchmarked together with the earlier import changes.

### Give only real memory locations a global address

A native lifter can attach temporary and register varnodes to a high-level global variable. Their unique-space offsets and register numbers are not global RAM addresses. Treating those numbers as global cells makes unrelated globals share storage.

The repair requires an actual RAM varnode before assigning a global-memory identity. Other temporaries stay ordinary locals. Mapping every temporary back to its associated global address was also tested; it still mixed loaded pointer values with the cell holding them and performed badly on an allocator-heavy input.

Representative measurements for the local-temporary approach were:

| Native workload | Local-flow rows before → after | Peak memory before → after |
| --- | ---: | ---: |
| Compressor | 6.66 million → 0.13 million | 1.73 → 0.10 GB |
| Archive utility A | 41.3 million → 0.21 million | 2.17 → 0.17 GB |
| Archive utility B | 54.7 million → 0.24 million | 3.43 → 0.18 GB |
| Unpacker | 107.5 million → 0.67 million | 7.53 → 0.31 GB |

Most numbers came from experimental variants; the committed repair was remeasured on the compressor at 126,000 local-flow rows, 104.5 MB peak, and 0.36 seconds. Its regression run had 250 passes and three expected failures.

Finding comparisons kept the real flows and removed 24 reports traced to fake shared cells across two archive utilities and one daemon. That is a precision repair with a performance benefit, so identical old findings would be the wrong goal. The compressor itself had no analyzable query endpoints.

### Model allocator and memory-helper behavior at their boundary

Analyzing a statically linked allocator body can make its size argument appear to flow into bin addresses and global free-list state. The resulting dense summary is inherited by hundreds of callers. Similar problems occur when lifted memory-copy code merges address arithmetic with copied data.

The tested approach replaces selected library bodies with explicit boundary models. Allocation size does not become allocated content. Reallocation preserves old contents in the returned memory. Copy helpers carry source contents to destination contents with the right argument order. Named stubs and checked variants need the same treatment as their underlying routine.

This changes how the library is analyzed. It needs semantic models and controls for overrides; it is not permission to skip arbitrary expensive functions.

On named-allocator comparisons, one daemon went from being killed at 28 GiB after 1,322 seconds to completing at 0.50 GiB. Two other inputs went from 5.96 and 8.21 GiB to about 0.09 GiB each. Findings matched on all 14 inputs whose parents completed. Some of those inputs had no matched sinks, so the checks there only established endpoint preservation.

For stripped allocators, the experiment used structural evidence rather than names: header reads immediately before a pointer parameter, repeated writes to shared allocator state, parameter/return shape, and direct call sites. Candidate functions had to write at least three relevant globals with at least 100 stores; a malloc-like candidate also needed at least three direct call sites. Wrapper-induced stores and globals written by more than ten functions were excluded to avoid mistaking cleanup code for an allocator.

With names hidden, the detector recovered the allocator trio in all 11 statically linked named inputs and all 15 stripped inputs, matching the hand-selected candidates. Four other named inputs had only small allocation stubs. The detector missed some small wrappers and aliases without direct callers. Representative stripped-input peaks fell from 22.14 to 0.30 GiB, 16.99 to 0.21 GiB, and 21.81 to 0.31 GiB. One input killed at 32 GiB completed at 0.47 GiB. These comparisons used an older sweep baseline and cannot isolate the detector from every intervening change. The stripped-input query models matched no endpoints, so their findings were not validated.

Separately replacing named copy/fill helper bodies reduced four residual peaks from 3.72 to 0.27 GiB, 1.01 to 0.07 GiB, 0.78 to 0.12 GiB, and 0.26 to 0.08 GiB. Findings matched across all 15 compared inputs. The rest of the copy-function family was not measured.

### Let an unconditional fact cover its conditional copies

One engine experiment found that 97–99.9% of some context-specific relations repeated facts already known without a condition.

The redesign attaches a condition to each fact: either `Always` or a set of target-resolution decisions. Alternative derivations union their decision sets; `Always` covers every conditional copy. Composition intersects the sets of decisions under which the two facts hold, with `Always` as the identity. A resolved call site retains its own condition when applying a summary.

The first implementation put everything into generic lattice storage. It reduced logical duplication but made some workloads dramatically larger. A follow-up store kept unconditional rows in compact tries and conditional rows in side maps. Membership asks whether the stored condition covers the arriving one. Insertion and delta merging must join conditions with the accumulated total, then repropagate a fact when its condition grows.

| Native workload | Original peak | Rules-only peak | Compact conditional-store peak | Original → final time |
| --- | ---: | ---: | ---: | ---: |
| Large context-heavy daemon | 8.22 GiB | 8.83 GiB | 3.02 GiB | 131 → 62 s |
| Second context-heavy daemon | 1.30 GiB | 2.84 GiB | 0.73 GiB | 13 → 11 s |
| Smaller server | 0.51 GiB | 0.87 GiB | 0.27 GiB | 3.9 → 2.8 s |
| File utility | 2.63 GiB | 17.7 GiB | 2.76 GiB | 32 → 42 s |
| Chat utility | 0.39 GiB | 2.42 GiB | 0.42 GiB | 4.4 → 4.9 s |

The sixth compared workload stayed around 0.65 GiB and slowed from 7.6 to 8.3 seconds. Findings matched on all six. The rules change passed 246 regression cases with three expected failures. Randomized tests compared the custom conditional store against a reference lattice in serial and parallel execution; the recorded test did not exercise the process-wide setting that spills a widened condition to `Always`.

This experiment also changed semantics: compatible conditional facts could compose where they previously could not, and conditionally resolved calls stopped applying their effects unconditionally. It belongs in a correctness-aware redesign, with performance measured afterward. Fewer logical rows alone did not guarantee lower memory or faster execution.

## 10. Experiments we tried and dropped

These results are part of the performance work too. They help avoid repeating changes that looked promising in isolation.

| Experiment | What we observed | What to carry forward |
| --- | --- | --- |
| Cache field-path substitutions during indexing | Nine compared tables stayed equal on two inputs, but four alternating runs took a request-heavy input from a 1.71 s median to 2.62 s. A smaller input stayed around 0.56 s. A fresh large-input run exceeded 600 s. The cache was removed. | Lookup, synchronization, and cache maintenance can cost more than recomputation. Measure the real hit distribution. |
| Rewrite field-propagation joins | One scoped test added eight assignment rows despite equal summaries and calls. Another attempt added two assignment rows on an APK. Both were removed. | Full assignment equality catches changes that summary equality misses. |
| Use reverse prefix certificates to dismiss negative queries | Earlier real negative queries still hit 120 s limits. The current reverse-certificate pruning is disabled because it does not carry the directed object-scope bit. | A reverse proof must model every relevant forward-state dimension. Fall back when it cannot prove the result. |
| Version the entire global heap at each write | Every measured native input got worse; summary counts did not shrink. One input created 1,127 heap versions repeating the same 2,825 rows. An allocator-heavy case hit a 24 GiB cap. | Kills do not pay for copying a large shared state into every version. |
| Collapse cyclic numeric offsets to one wildcard region before analysis | One runtime grew from 3.64 GiB to a 10 GiB kill. Another input grew from 40.7 to 114 million local-flow rows and 0.67 to 2.14 GiB. | A smaller path vocabulary can create more mixing and more rows. Counting renamed rows in an existing dump misses this feedback. |

The optional quick forward searches described earlier remain distinct from reverse-certificate pruning: a positive witness can justify early completion, while an incomplete reverse abstraction cannot justify removing a sink.

Diagnostic body skips helped identify several other native costs: a garbage collector linking its running state into its own list, offset growth on loop pointers, recursive summary feedback, and dense helper summaries copied into many callers. These experiments were not all retained as general fixes. For example, skipping one recursive helper changed an input from a 3,000-second timeout at 15.9 GiB to completion in 153 seconds at 4.30 GiB, but that alone does not validate skipping the helper's semantics.

Address-argument write-back and forwarding-thunk experiments reduced another native peak from 9.21 GiB to 6.88 and 7.38 GiB respectively, or 6.31 GiB together. Their full regression and finding checks were still outstanding. Estimates of 3× to 84× reductions from folding offset variants in finished dumps were estimates, not measured engine improvements.

## 11. What the empirical checks do and do not establish

The repeated import/index experiments used release builds, Rust 1.94.1, locked dependencies, and the same machine. Builds finished before timed runs. Each repetition used a fresh store. Later baseline/candidate runs alternated execution order. Filesystem caches were left warm.

The early parallel comparison used an operating-system timing wrapper. Initial sandboxed runs with broken memory instrumentation were excluded. Later ingest comparisons used a monotonic wall clock and child-process resource accounting. Those measurements report peak resident memory. The related native experiments often report macOS peak memory footprint, which is a different measure; the native tables retain that distinction.

The native sweep covered 45 binaries, with detailed growth ratios available for 43 profiles, using a 32 GiB cap and a 3,600-second timeout. Some runs shared the host with other analysis jobs. Follow-up experiments used their stated 24, 28, or 32 GiB caps. Their times are observations from that setup, and incomplete runs are reported as incomplete.

The correctness checks were deliberately broader than comparing a total row count:

1. Decode and compare complete IR. Normalize only function-definition ordering; retain instruction, block, argument, and variable ordering.
2. Compare typed dispatch metadata, import options, content digests, and source-information tables. Serialization ordering alone should not create a false mismatch.
3. Compare saved relations as sorted multisets after normalizing internal identities to stable names. Keep duplicates when they are part of the stored output.
4. Run source-to-sink answer keys with exact endpoint counts, positive witnesses, and negative controls. An unmatched endpoint does not count as a successful negative.
5. Exercise serial and multiple-worker execution. Compare complete facts where available, rather than assuming equal counts imply equal content.
6. For a representation change such as shared heap moves, compare expanded semantic effects and query results. For an abstraction repair, explain which old rows or findings should change and why.

Some later application validations became slower as coverage improved. One completed large index grew from about 10.05 to 13.78 million summaries and from 20.48 to 28.22 million assignments, completing in 504.43 seconds. Later coverage-expanding candidates hit 2,500- and 3,600-second limits. These are not controlled A/B speed comparisons, but they show that frontend coverage can outweigh a local engine improvement. A bounded profile with 70.77 million local-flow rows is a diagnosis of unfinished work, not a completed-index speedup.

The early equality and answer-key successes apply to the versions tested at that time. They do not establish that the final combination of all later changes passes the full large-application corpus. Reduced diagnostic programs, partial indexes, source comments, and targeted regressions each answer narrower questions.

## 12. How to pick this work up again

Start with a tiny input, a medium input, and an input that creates millions of facts. Include a multi-DEX APK and a native program if both analysis paths matter. Keep the models, input bytes, frontend settings, and timeout policy fixed while measuring an optimization.

Measure import, IR passes, fact generation, fixpoint computation, serialization, discovery, and query separately. Record both time and peak memory, plus final relation sizes. A peak after the fixpoint can come from serialization buffers; a fast decoder will not fix an expensive summary closure.

Then follow the evidence:

- If import allocates roughly the package size before decoding, implement signature-only detection and streamed hashing.
- If a rule scans millions of candidates to keep a few matches, introduce a filtered helper and an exact two-way join. Inspect both delta directions.
- If summary instances dominate, count how much information is repeated across callers. Separate truly shared heap moves and retain caller-specific effects.
- If a few keys dominate lookup cost, add exact-path indexing for large groups and keep small groups compact.
- If a path query keeps walking after finding every sink, add correct early completion and share identical searches across labels.
- If paths grow around cycles, define useful finite compositions and test nested positive flows alongside sibling negatives.
- If native lifting creates dense global summaries, inspect memory identities and helper models before changing the storage engine.

Make each change measurable on its own. Keep the complete output comparisons for exact rewrites, and write explicit positive and negative expectations for changes to the abstraction. Report the workloads that got slower alongside the workloads that improved.
