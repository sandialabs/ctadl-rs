# Staged index fixpoint: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines how the index fixpoint (`docs/spec.md` §4.1 step 2, §4.2)
runs, so that its peak memory is that of its largest stage rather than the whole program's.
The rules, the index tables and the query side stay as they are. This spec does not sequence
the work.

## Background

- **One run today.** `EngineInput::new` (`ctadl-index/src/index_engine/mod.rs:1373`) turns the
  whole program's `IndexFacts` into seed relations. It precomputes `prog_store`,
  `alias_of_formal` and the program-wide path set (`mod.rs:1514-1526`). `run_engine!`
  (`mod.rs:1634`) seeds one Ascent program and runs it to fixpoint (`mod.rs:1681`). The result
  is one `IndexResult` (`mod.rs:476`), saved by `try_save` (`mod.rs:490`) as one parquet file per
  table.
- **Engine choice.** Engines are serial, parallel, or serial with LSM stores. They are chosen in
  `ctadl-ascent/src/index_engine.rs:27-64` from `-j` and `--lsm`.
- **What crosses functions.** Every rule in `index_rules` (`mod.rs:996-1348`) joins facts of
  one function, except:
  - `summary` instantiated at callers through `call` (`mod.rs:1202-1206`). `call` is input only
    (`mod.rs:1003`).
  - Global-to-global edges written into `GLOBAL_FRAME`'s `assign_like` from `locals`
    (`mod.rs:1266`) and from `alias_flow` (`mod.rs:1294`). The input `global_flow` of a
    `--summary` project is also written there (`mod.rs:1467`).
  - The label exchange: `const_reaches` at global paths goes up to the frame and back down to
    `intent_frame` functions that read the path (`mod.rs:1320-1347`).
  - `global_mention(GLOBAL_FRAME, g) <-- global_mention(_, g)` (`mod.rs:1060`). It only seeds
    the frame's `locals`.
- **`resolved_call` feeds no rule** (head only, `mod.rs:1245`). The query side uses `call ∪
  resolved_call` (`mod.rs:624`).
- **Monotone.** The rules have no negation, aggregation or lattices.
- **The frame's `locals` feeds no output.**
  - The summary rules need formals, which the frame lacks (`mod.rs:1249`), or exclude the frame
    (`mod.rs:1261`).
  - The frame-edge rule excludes the frame (`mod.rs:1266`).
  - `alias_flow` needs `prog_store`, and the frame has none.
  - `global_flow` is the frame's `assign_like` rows (`global_flow_rows`, `mod.rs:696`). It is
    not a closure: the query closes it by search (`ctadl-ascent/src/query_engine/search.rs:29`).
- **Encoding and ids.**
  - Parquet columns are by content. Paths and strings are written as text and re-interned on
    load (`ctadl-index/src/facts/parquet.rs:553`).
  - `FunctionId` and `InsnId` are counters assigned by codegen (`ctadl-index/src/facts.rs:2103`).
    They are stable only together with the saved `IdMap`.
  - `IndexFacts::try_save` (`mod.rs:149`) does not save `assign`, `const_str_assign`, `summary`,
    `paths`, the `intent_*` facts or `global_flow`.
- **Existing tools.** `ctadl-ir/src/graph/scc/mod.rs:119` computes SCCs with a condensation in
  dependency order. `CTADL_DUMP_RELATIONS` (`ctadl-index/src/index_engine/dump.rs:25`) dumps
  every relation by content, once per process: it refuses to overwrite (`dump.rs:242`). No test
  shuffles facts.

## Terms

| Term | Meaning |
| --- | --- |
| **Monolithic run** | Today's single fixpoint over the whole program. |
| **Staged run** | The index computed as the sequence of stages below. |
| **Batch** | A set of functions run as one stage-1 fixpoint: whole SCCs of `call`, or one part of a split SCC. |
| **Frame edges** | The global-to-global `assign_like` rows of `GLOBAL_FRAME`. |
| **Stage directory** | Where a staged run keeps its inputs, plan, per-batch outputs and completion markers until the final tables are written. |
| **Budget** | The memory a staged run may use for the batches it runs at once. |

## Requirements

### Results

- R1. **Same outputs.** A staged run writes the same rows as a monolithic serial run, compared as
  sets, for every index table: `summary`, `assign`, `paths`, `external_function`,
  `global_flow`, `resolved_call` and `intent_pair`.
- R2. **Independent of batching and order.** R1 holds for any batch plan the budget allows, down
  to the minimum batch size (C3), and for input facts in any order.
- R3. **Same tables.** The index directory has the same files, schemas and encodings as today.
  `INDEX_FORMAT_VERSION` does not change, and every reader (query, `--summary`, inspection)
  works unchanged. Each row appears once per table.
- R4. **Rules unchanged.** Every stage runs `index_rules` as written, under the serial or the
  parallel engine.
- R5. **Fact generation and query unchanged.** Codegen, models, JNI, intent fact generation and
  `--summary` loading produce the same `IndexFacts`. The query side reads the same tables.

### Memory

- R6. **Peak memory.** The fixpoint part of a staged run peaks at the largest set of batches it
  runs at once, plus the program-wide path set. The peak does not grow with the number of
  functions. Peak footprint of `ctadl index` is the larger of fact generation's and that.
- R7. **Budget.** The user can set the budget. The scheduler packs batches, and runs batches
  concurrently, so that their estimated memory stays within it. A single function whose estimate
  exceeds the budget still runs, alone, and is logged.
- R8. **Freed between stages.** Nothing a stage derives (`locals` and the relations built from
  it) outlives the stage. Outputs go to disk as each stage finishes. They are never gathered
  for the whole program in memory, including in the final tables.

### Disk and resumption

- R9. **Self-contained stages.** Every stage reads its inputs only from the stage directory, so
  any stage can run in a fresh process.
- R10. **Callee reads only.** A batch reads only the `summary` rows of its callees, and its own
  functions' facts.
- R11. **Resume.** A run killed at any point, restarted with the same facts and configuration,
  skips completed batches and stages and produces the same tables as an uninterrupted run. If
  the facts or configuration differ, it starts over.
- R12. **No partial index.** Until the final tables are complete, the project reads as having no
  index, as it does today while an index is being written.

### Verification

- R13. **Equivalence tests.** Every integration test and engine test that indexes also runs
  staged, with shuffled facts and several batch sizes down to the minimum. It checks R1 by
  comparing `CTADL_DUMP_RELATIONS` output relations with those of the monolithic serial engine
  (`scripts/fact-dump-diff.sh`).
- R14. **Memory benchmark.** A documented procedure runs a large target staged under
  `memguard.sh`. It shows the peak footprint following the largest batch rather than the program
  (R6). It also shows a run killed between stages resuming to the same tables (R11).

### Documentation

- R15. **Overall design updated.** `docs/spec.md` §4.1 and §4.2 describe the staged fixpoint.
  §4.6 describes the stage directory. Code and help text do not cite this spec's numbers.

## Design

### D1. Stage plan

A staged run computes one plan before any fixpoint and saves it in the stage directory (R9,
R11).

- **Graph.** Nodes are the functions with facts: `formal_param`, `external_function`, or a seeded
  `summary`. Edges are `call` (caller to callee). `GLOBAL_FRAME` is not a node.
- **SCCs.** The graph is condensed with `ctadl-ir`'s `Sccs` (`ctadl-ir/src/graph/scc/mod.rs:119`).
  SCCs are taken in dependency order, callees first.
- **Packing.** Whole SCCs are packed into batches by estimated memory (D6), up to the budget.
  A batch's callees outside it are in earlier batches. An SCC over the budget is split into parts
  (D3).
- **Determinism.** The plan depends only on the facts and the configuration, not on fact order,
  so a shuffled run and a resumed run get the same plan. Functions are ordered by `FunctionId`
  wherever order matters.

### D2. Stage 1: batches

Batch inputs:

- **Facts.** The batch's functions' per-function facts: `formal_param`, `actual_param`,
  `assign` (as `assign_like`), `prog_store`, `alias_of_formal`, `const_str_assign`, and seeded
  `summary` rows of its functions.
- **Callee summaries.** `call` rows whose caller is in the batch, plus the `summary` rows of
  their callees outside the batch, read from earlier batches' outputs. The batch's summaries of
  its own functions are not seeded.
- **Shared.** The program-wide `paths`/`path_set` and `config`.
- **Not seeded:** the intent facts (`intent_frame`, `intent_send`, `intent_filter`,
  `intent_component`). Stage 3 computes `const_reaches`, `intent_pair` and `resolved_call`. Those
  rules feed nothing else (Background), so leaving them out changes no stage-1 output.

Before the fixpoint, every `Path` and `Str` the batch reads is interned, as the freeze rule
requires (`mod.rs:30-37`). The path set is interned once per process.

Batch outputs, written when its fixpoint ends:

- `summary` rows of its functions;
- `assign_like` rows of its functions, including instantiated summaries;
- its frame edges.

Its `locals` and the relations derived from it, including the partial frame `locals`, are
dropped. Rows of a function are written by exactly one batch, so `summary` and per-function
`assign` are disjoint across batches. Frame edges may repeat across batches.

Batches whose callees are all done run concurrently when their estimates fit the budget
together (R7). Each runs the serial engine. A batch that runs alone may use the parallel
engine on the run's `-j` threads.

### D3. Split SCCs

A split SCC's parts run as batches in rounds. In each round, a part is seeded with its
callees' summaries:

- callees outside the SCC: final rows, from earlier batches;
- callees in the other parts: the rows of the previous round.

Rounds repeat until no part's `summary` grows. The rules are monotone, so the result is the
least fixpoint, the same as running the SCC whole. The last round's outputs are the SCC's
outputs. A part need only rerun when a callee part's summaries grew.

### D4. Stage 2: global frame

Stage 2 is the union of every batch's frame edges and the input `global_flow` (`mod.rs:1467`),
deduplicated. It is written as the frame's rows of `assign` and as `global_flow`, matching
`global_flow_rows` (`mod.rs:696`). It runs no fixpoint, because the frame's `locals` feeds no
output (Background, C1).

### D5. Stage 3: intent

Stage 3 runs only when `intent_frame` is non-empty. It runs `index_rules` seeded with:

- the final `assign_like` of the `intent_frame` functions, from stage 1, and the frame edges,
  from stage 2;
- those functions' `const_str_assign`, `intent_send` and `intent_frame`;
- `intent_filter`, `intent_component`, the path set and `config`.

It writes only `intent_pair` and `resolved_call`. It does not seed `formal_param`, `summary`,
`call` or `actual_param`, and it writes no `assign` or `summary`, so it adds no rows that stage 1
owns. Its cost is in C2.

### D6. Budget and estimates

- The budget is a configured byte limit, `IndexConfig` gaining a field beside `parallelism` and
  `storage`.
- A batch's estimate is a function of its seed sizes: `assign_like` rows, plus instantiated
  callee-summary rows, variables and formals. It is calibrated against `[mem cp]` and
  `profile.json` measurements. C5 covers the choice of estimator.
- The minimum batch size is a planner floor, below which a batch is never split (C3). The
  tests set it directly.

### D7. Stage directory and files

- **Location.** A directory under the project's `index/`, beside the final tables.
- **Saved once, before stage 1.** The complete `IndexFacts`, including the members
  `IndexFacts::try_save` omits today, the `IdMap`, the path set and the plan. All are in the
  index's parquet encoding (R9). Facts are partitioned by batch, so a batch reads only its own
  facts (R10).
- **Per batch.** Output files `summary`, `assign` and frame edges, in the same encoding. Rows
  are sorted by `FunctionId`, so a reader of one callee's rows can skip row groups by
  statistics.
- **Completion.** Each batch, round and stage writes its files to temporary names, renames them
  into place, then writes its completion marker the same way (R11).
- **Run key.** A key over the facts and the configuration (engine, budget, alias rule, deref
  paths) is stored in the directory. A resumed run with a different key starts over.

### D8. Final tables

- **Assembly.** After stage 3, each final table is written by streaming the stage files into
  it, one row group at a time, through the existing `Writer` (`parquet.rs:174`).
  - `summary` and `assign` are the concatenation of batch outputs plus the frame rows (D4).
  - `paths` is the path set. `external_function` comes from the facts.
  - `resolved_call` and `intent_pair` come from stage 3.
- **Duplicates.** Only the frame edges need deduplication, done in D4.
- **Commit.** The index stamp (`write_index_config`) is written last, as today (R12). The stage
  directory is then removed.

### D9. Observability

- `IndexStats` and `profile.json` report summed totals over stages, and each stage's peak
  footprint, row counts and time.
- `CTADL_DUMP_RELATIONS` under a staged run dumps the output relations of the final tables, so
  `fact-dump-diff.sh` compares a staged and a monolithic run on outputs. Per-stage scratch is
  dumped, when asked, into one subdirectory per stage, since `dump.rs:242` refuses to
  overwrite.

### D10. Tests

- A seeded shuffle of every `IndexFacts` vector, used by the staged equivalence tests (R13).
- A staged mode for the shared test entry points: `taint_index_with_config` (via
  `IndexConfig`), `flowy::check_with_config` and `cli::index` through `IndexOptions`. Each
  existing test can then run staged and compare outputs with the monolithic serial run.
- A resume test kills the run after a chosen batch (a test hook) and checks the resumed tables.

## Concerns

- C1. **Stage 2 has no closure to compute.** The intent says stage 2 writes "the frame's
  closure" as `global_flow`. In fact `global_flow` is the frame's unclosed edges
  (`mod.rs:696`), the frame's `locals` feeds no output, and the query closes the edges by search.
  - (a) Stage 2 is a deduplicating union with no fixpoint (D4). This is cheapest, and the
    outputs match.
  - (b) Run the frame fixpoint anyway. This reproduces the frame's `locals` for scratch dumps,
    at a cost that may be large on binaries, where every function touches the memory global.
  - The spec assumes (a), and that the intent's wording should change to match.
- C2. **Stage 3 recomputes work under "rules as written".**
  - Seeding the frame edges makes `global_mention(GLOBAL_FRAME, _)` (`mod.rs:1060`) compute the
    frame's whole `locals` closure: the work C1 avoids, which no output needs.
  - Seeding the `intent_frame` functions' `assign_like` recomputes their global-rooted `locals`.
  - On an Android app with many `intent_frame` functions, stage 3 may be neither small nor
    cheap.
  - (a) Accept it: stage 3 is bounded by the frame plus those functions.
  - (b) Factor the intent rules into their own `ascent_source` that both the full program and
    stage 3 include. The rules are still written once, but stage 3 runs only them.
  - (c) Fold the label exchange into stage 1. This is not possible: it is cyclic through the
    whole frame.
  - The spec assumes (a), with (b) if measurements show stage 3 dominating.
- C3. **What is the minimum batch size?** The intent was edited from "down to one function" to
  "down to a minimum", without saying what the minimum is.
  - One function: the strongest test of D3, since every SCC is split.
  - One SCC: batches never split an SCC in tests, so D3 is tested only by SCCs over the budget.
  - A configured floor: tests pick it.
  - The spec assumes a configured floor whose smallest value is one function.
- C4. **Default or opt-in.** The intent does not say whether staging replaces the monolithic
  run.
  - Make it the default, keeping the monolithic run as the reference for tests (R13).
  - Add it behind a flag, with the CLI otherwise unchanged.
  - The spec assumes a flag at first (`IndexConfig`, CLI `--staged` or a budget flag), with the
    monolithic run the default.
- C5. **Estimating a batch's memory.** No estimator exists. Seed sizes are a weak proxy, because
  the blowup is in `locals`, which the seeds do not predict. A batch could overshoot the budget
  by a large factor.
  - Estimate from seed sizes, calibrated (D6).
  - Budget in seed rows rather than bytes, and say so.
  - Measure and re-split: abort a batch that exceeds the budget and rerun it split. This needs a
    per-batch memory guard.
  - The spec assumes calibrated seed sizes, with the budget documented as approximate.
- C6. **In-process or a process per batch.** R9 requires that a stage *can* run in a fresh
  process. Running every batch in-process is simpler, but the allocator may not return freed
  pages, so the footprint might track the high-water mark rather than the current batch.
  Interners never free either, but they hold only the program-wide paths and strings.
  - Run in-process, and verify with R14 that the footprint falls between batches.
  - Use a worker subprocess per batch. Memory is reclaimed for certain, at a process-start and
    fact-load cost per batch.
  - The spec assumes in-process, falling back to subprocesses if R14 shows the footprint does not
    fall.
- C7. **LSM storage.** The intent names only the serial and parallel engines. `--lsm` is another
  way to bound memory.
  - Staging rejects `--lsm`.
  - Each batch may run under LSM storage.
  - The spec assumes staging rejects `--lsm`.
- C8. **Cost of split SCCs.** Each round reruns its parts from scratch, so an SCC that needs k
  rounds costs about k times its monolithic work, and a deep cycle needs many rounds. Rerunning
  only parts whose callee summaries grew (D3) limits this, but bounds nothing. The spec accepts
  it as the price of fitting the budget, and logs rounds per split SCC.
