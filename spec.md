# Staged index fixpoint: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines how the index fixpoint (`docs/spec.md` §4.1 step 2, §4.2)
runs, so that its peak memory is that of its largest stage rather than the whole program's.
The rules and the index tables stay as they are. This spec does not sequence
the work.

## Background

- **One run today.** `EngineInput::new` (`ctadl-index/src/index_engine/mod.rs:1342`) turns the
  whole program's `IndexFacts` into seed relations. It precomputes `prog_store`,
  `alias_of_formal` and the program-wide path set (`mod.rs:1414`, `mod.rs:1477`, `mod.rs:1490-1495`). `run_engine!`
  (`mod.rs:1603`) seeds one Ascent program and runs it to fixpoint (`mod.rs:1650`). The result
  is one `IndexResult` (`mod.rs:444`), saved by `try_save` (`mod.rs:458`) as one parquet file per
  table.
- **Engine choice.** Engines are serial, parallel, or serial with LSM stores. They are chosen in
  `ctadl-ascent/src/index_engine.rs:27-62` from `-j` and `--lsm`.
- **What crosses functions.** Every rule in `index_rules` (`mod.rs:965-1317`) joins facts of
  one function, except:
  - `summary` instantiated at callers through `call` (`mod.rs:1171-1175`). `call` is input only
    (`mod.rs:972`).
  - Global-to-global edges written into `GLOBAL_FRAME`'s `assign_like` from `locals`
    (`mod.rs:1235`) and from `alias_flow` (`mod.rs:1263`). The input `global_flow` of a
    `--summary` project is also written there (`mod.rs:1436`).
  - The label exchange: `const_reaches` at global paths goes up to the frame and back down to
    `intent_frame` functions that read the path (`mod.rs:1286-1316`).
  - `global_mention(GLOBAL_FRAME, g) <-- global_mention(_, g)` (`mod.rs:1029`). It only seeds
    the frame's `locals`.
- **`resolved_call` feeds no rule** (head only, `mod.rs:1214`).
- **Monotone.** The rules have no negation, aggregation or lattices.
- **The frame's `locals` feeds no output.**
  - The summary rules need formals, which the frame lacks (`mod.rs:1222`), or exclude the frame
    (`mod.rs:1232`).
  - The frame-edge rule excludes the frame (`mod.rs:1237`).
  - `alias_flow` needs `prog_store`, and the frame has none.
  - `global_flow` is the frame's `assign_like` rows (`global_flow_rows`, `mod.rs:664`). It is
    not a closure.
- **Encoding and ids.**
  - Parquet columns are by content. Paths and strings are written as text and re-interned on
    load (`ctadl-index/src/facts/parquet.rs:552`).
  - `FunctionId` and `InsnId` are counters assigned by codegen (`ctadl-index/src/facts.rs:1912`).
    They are stable only together with the saved `IdMap`.
  - `IndexFacts::try_save` (`mod.rs:149`) does not save `assign`, `const_str_assign`, `summary`,
    `paths`, the `intent_*` facts or `global_flow`.
- **Existing tools.** `ctadl-ir/src/graph/scc/mod.rs:84` computes SCCs with a condensation in
  dependency order. `CTADL_DUMP_RELATIONS` (`ctadl-index/src/index_engine/dump.rs:25`) dumps
  every relation by content, once per process: it refuses to overwrite (`dump.rs:225`). No test
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
  to one function, and for input facts in any order.
- R3. **Same tables.** The index directory has the same files, schemas and encodings as today.
  `INDEX_FORMAT_VERSION` does not change, and every reader (`--summary` and the tests)
  works unchanged. Each row appears once per table.
- R4. **Rules written once.** Every rule keeps its meaning and is written once. The intent rules
  (`const_reaches`, the label exchange, `intent_pair`, `resolved_call`) are their own rule
  source, which the full program includes. Stage 1 runs the full program, under the serial or
  the parallel engine. Stage 3 runs only the intent rules and computes no `locals`.
- R5. **Fact generation unchanged.** Codegen, models, JNI, intent fact generation and
  `--summary` loading produce the same `IndexFacts`.
- R6. **Staged by default.** `ctadl index` runs staged. A flag selects the
  monolithic run, which stays as the reference the tests compare against (R14) and for
  debugging. `--lsm` is accepted only with that flag. Alone, it is an error that says to add the
  flag. Staged workers run only the serial or parallel engine.

### Memory

- R7. **Peak memory.** The fixpoint part of a staged run peaks at the scheduler's footprint plus
  that of the workers it runs at once, each holding one batch and the program-wide path set. The peak does not grow with the number of
  functions, except in three parts:
  - the scheduler's plan, which is O(functions + call edges);
  - stage 2, which holds the frame edges;
  - stage 3, which holds the final `assign_like` of the `intent_frame` functions.

  These hold only outputs, never `locals`, and the benchmark (R15) reports their sizes. Peak footprint of `ctadl index` is the larger of fact generation's and that.
- R8. **Budget.** The user can set the budget. The scheduler packs batches, and runs batches
  concurrently, so that their estimated memory stays within it. A batch that is running when the
  footprint exceeds the budget is stopped, split and rerun, so an underestimate costs time, not
  memory beyond the budget. A single function that exceeds the budget runs alone to completion,
  and is logged.
- R9. **Freed between stages.** Nothing a stage derives (`locals` and the relations built from
  it) outlives the stage. Outputs go to disk as each stage finishes. They are never gathered
  for the whole program in memory, including in the final tables.

### Disk and resumption

- R10. **Self-contained stages.** Every stage reads its inputs only from the stage directory, so
  any stage can run in a fresh process.
- R11. **Callee reads only.** A batch reads only the `summary` rows of its callees, and its own
  functions' facts.
- R12. **Resume.** A run killed at any point, restarted with the same facts and configuration,
  skips completed batches and stages and produces the same tables as an uninterrupted run. If
  the facts or configuration differ, it starts over.
- R13. **No partial index.** Until the final tables are complete, the project reads as having no
  index, as it does today while an index is being written.

### Verification

- R14. **Equivalence tests.** Every integration test and engine test that indexes also runs
  staged, with shuffled facts and the batch floor swept down to one function. It checks R1 by
  comparing the output tables, rendered by content, with those of the monolithic serial engine:
  in-process for tests that index through the library, and with
  `scripts/fact-dump-diff.sh --outputs` for runs of the CLI (D10).
- R15. **Memory benchmark.** A documented procedure runs a large target staged under
  `memguard.sh`. It shows the peak footprint following the largest batch rather than the program
  (R7). It also shows a run killed between stages resuming to the same tables (R12).

### Documentation

- R16. **Overall design updated.** `docs/spec.md` §4.1 and §4.2 describe the staged fixpoint.
  §4.6 describes the stage directory. Code and help text do not cite this spec's numbers.

## Design

### D1. Stage plan

A staged run computes one plan before any fixpoint and saves it in the stage directory (R10,
R12).

- **Graph.** Nodes are the functions with facts: every `FunctionId` in a per-function fact
  (`formal_param`, `actual_param`, `call` as caller, `assign`, `const_str_assign`, seeded
  `summary`, `external_function`). Edges are `call` (caller to callee). `GLOBAL_FRAME` is not a
  node.
- **SCCs.** The graph is condensed with `ctadl-ir`'s `Sccs` (`ctadl-ir/src/graph/scc/mod.rs:84`).
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
  `intent_component`). The intent rules in the full program then derive nothing that reaches an
  output. Stage 3 computes `const_reaches`, `intent_pair` and `resolved_call`. Those rules feed
  nothing else (Background), so leaving the intent facts out changes no stage-1 output.

Before the fixpoint, every `Path` and `Str` the batch reads is interned, as the freeze rule
requires (`mod.rs:31-37`). The path set is interned once per process.

Batch outputs, written when its fixpoint ends:

- `summary` rows of its functions;
- `assign_like` rows of its functions, including instantiated summaries;
- its frame edges.

Its `locals` and the relations derived from it, including the partial frame `locals`, are
dropped. Rows of a function are written by exactly one batch, so `summary` and per-function
`assign` are disjoint across batches. Frame edges may repeat across batches.

Each batch runs in its own worker process (D7). Batches whose callees are all done run
concurrently when their estimates fit the budget together (R8). Each worker runs the serial
engine, except that a worker running alone may use the parallel engine on the run's `-j`
threads.

### D3. Split SCCs

A split SCC's parts run as batches in rounds.

- **Order.** Within a round, parts run in an order that follows the SCC's internal `call` edges,
  callees first as far as the cycle allows. The order is fixed by the plan.
- **Seeds.** Each part is seeded with its callees' summaries:
  - callees outside the SCC: final rows, from earlier batches;
  - callees in other parts: their latest rows, from this round if they have already run in it,
    otherwise from the previous round.
- **Reruns.** A part reruns in a round only if a callee part's summaries grew since the part
  last ran. Otherwise its latest outputs stand.
- **End.** Rounds repeat until no part's `summary` grows. The rules are monotone, so the
  result is the least fixpoint, the same as running the SCC whole. Each part's latest outputs
  are the SCC's outputs.
- **Logging.** The run logs the rounds, part reruns and time of each split SCC.

Parts of one SCC run one at a time. Other ready batches may run beside them within the budget.

### D4. Stage 2: global frame

Stage 2 is the union of every batch's frame edges and the input `global_flow` (`mod.rs:1436`),
deduplicated. It is written as the frame's rows of `assign` and as `global_flow`, matching
`global_flow_rows` (`mod.rs:664`). It runs no fixpoint, because the frame's `locals` feeds no
output (Background).

### D5. Stage 3: intent

- **Rule sources.** The intent rules move out of `index_rules` (`mod.rs:1177-1215`,
  `mod.rs:1286-1316`) into a second `ascent_source!`, `intent_rules`. It holds `const_reaches`,
  `intent_pair`, `resolved_call`, `global_read`, `intent_frame(GLOBAL_FRAME)` and the
  `global_label_*` rules. It declares every relation it reads.
  - The engines' full programs include `intent_rules` first, then `index_rules`, so that
    `index_rules`' storage declarations (`#[ds(...)]` on `assign_like`) win. Ascent keeps a
    relation's last declaration, which the LSM engine already relies on
    (`ctadl-index/engines/lsm/src/lib.rs:20`).
  - A monolithic run therefore computes what it does today.
- **Program.** Stage 3 runs a program that includes only `intent_rules`, on the serial engine.
  It runs only when `intent_frame` is non-empty.
- **Seeds:**
  - the final `assign_like` of the `intent_frame` functions, from stage 1, and the frame edges,
    from stage 2;
  - those functions' `const_str_assign`, `intent_send` and `intent_frame`;
  - `intent_filter`, `intent_component`, the path set and `config`.
- **Outputs.** It writes only `intent_pair` and `resolved_call`, and computes no `locals`. Its
  memory is bounded by its seeds and `const_reaches`.

### D6. Budget and estimates

- The budget is a byte limit, an `IndexConfig` field beside `parallelism` and `storage`, set by
  a CLI flag. It defaults to half of physical memory until measurements suggest better.
- `IndexConfig` also selects the monolithic run (R6). The staged run is the default in
  `IndexConfig::default()` and `IndexOptions::default()`
  (`ctadl-ascent/src/cli/mod.rs:88`), so existing tests run staged unless they ask otherwise.
- **Estimate.** A batch's estimate is a function of its seed sizes: `assign_like` rows,
  instantiated callee-summary rows, variables and formals. It is calibrated against `[mem cp]`
  and `profile.json` measurements. The planner packs by it.
- **Enforcement.** The scheduler sums the running workers' footprints (D7).
  - Over the budget, it starts no new worker. It kills the worker with the largest footprint,
    splits its batch in two (by SCC, or by function within an SCC), and requeues the halves.
  - A worker running a batch of one function is never killed.
  - A killed worker leaves no completion marker, and its files are discarded, so a kill cannot
    change outputs (R1).
- A configured floor bounds how small the planner makes a batch. Its smallest value is one
  function, which splits every recursive SCC. The tests sweep it down to one.

### D7. Workers

- **Scheduler and workers.** `ctadl index` is the scheduler: it plans, launches workers, runs
  stage 2 and assembles the final tables. Each stage-1 batch, each split-SCC round's part, and
  stage 3 runs in a worker: a fresh process running a hidden `ctadl` subcommand that takes the
  stage directory and a batch id. A worker reads its inputs from the stage directory (R10),
  writes its outputs and marker, and exits. Its memory is returned when it exits.
- **Footprint.** Each worker samples its own footprint on a thread and reports it to the
  scheduler over a pipe. On macOS it uses `phys_footprint_mb` (`mod.rs:782`). That returns -1
  elsewhere (`mod.rs:802`), so Linux reads resident set size from `/proc/self/statm`. The
  scheduler needs no access to another process's memory.
- **Path set.** Every worker re-reads and re-interns the program-wide path set before its
  fixpoint (`mod.rs:31-37`). That is a per-batch cost that grows with the program, so the
  planner prefers fewer, larger batches. If it dominates, sharing the path set through a
  memory-mapped file is a later optimization.
- **Worker binary.** The scheduler launches the `ctadl` executable. The CLI uses
  `std::env::current_exe()`. Library callers, including tests, name it in `IndexOptions`;
  integration tests pass `env!("CARGO_BIN_EXE_ctadl")`. Engine tests that call
  `taint_index_with_config` directly, with no binary, use an in-process executor that runs the
  same worker entry point on a thread.
- **Failure.** A worker that exits non-zero, other than by the scheduler's kill, fails the run
  with its stderr. Completed batches stay on disk, for resume.

### D8. Stage directory and files

- **Location.** A directory under the project's `index/`, beside the final tables.
- **Saved once, before stage 1.** The complete `IndexFacts`, including the members
  `IndexFacts::try_save` omits today, the `IdMap`, the path set and the plan. All are in the
  index's parquet encoding (R10). Facts are partitioned by batch, so a batch reads only its own
  facts (R11).
- **Per batch.** Output files `summary`, `assign` and frame edges, in the same encoding. Rows
  are sorted by `FunctionId`, so a reader of one callee's rows can skip row groups by
  statistics.
- **Completion.** Each batch, round and stage writes its files to temporary names, renames them
  into place, then writes its completion marker the same way (R12).
- **Run key.** A SHA-256 stored in the stage directory. It covers:
  - the fact files the run writes into the stage directory, hashed in file-name order;
  - the configuration that changes the plan or the tables: alias rule, deref paths, budget and
    floor.

  It leaves out the engine (serial, parallel, LSM), which changes neither. Fact files are
  written in a total order (batch, `FunctionId`, then the row), so the same facts give the same
  bytes.
- **Resume.** On start, a run writes its facts beside the stored ones, under a temporary name,
  and computes its key. If the key equals the stored key, it deletes the new facts and resumes.
  Otherwise it removes the stage directory and starts over, with the new facts as its facts.

### D9. Final tables

- **Assembly.** After stage 3, each final table is written by streaming the stage files into
  it, one row group at a time, through the existing `Writer` (`parquet.rs:177`).
  - `summary` and `assign` are the concatenation of batch outputs plus the frame rows (D4).
  - `paths` is the path set. `external_function` comes from the facts.
  - `resolved_call` and `intent_pair` come from stage 3.
- **Duplicates.** Only the frame edges need deduplication, done in D4.
- **After the fixpoint.** `cli::index` uses the result after the fixpoint
  (`ctadl-ascent/src/cli/mod.rs:778-813`). A staged run computes each of these uses from the
  stage outputs, not from a whole `IndexResult`:
  - The zero-assignment notes (`push_zero_assignment_notes`, `cli/mod.rs:270`) need three
    things:
    - the set of summarized functions, streamed from the batch `summary` files;
    - the final calls, which are `call` plus stage 3's `resolved_call`;
    - `external_function`, from the facts.
  - The intent-pair counts come from stage 3.
  - `flowy::index_check` (`ctadl-ascent/src/codegen/flowy.rs:92`) reads the final `summary`
    table back. This is the one exception to R9: flowy is a test language whose programs are
    small.
- **Commit.** The index stamp (`write_index_config`) is written last, as today (R13). The stage
  directory is then removed.

### D10. Observability

- `IndexStats` and `profile.json` report summed totals over stages, and each stage's peak
  footprint, row counts and time.
- **Output dump.** With `CTADL_DUMP_RELATIONS` set, both the staged and the monolithic run
  write the seven output tables, rendered by content as the dump renders them today, into
  `outputs/` under the dump directory.
  - The monolithic run still writes its full dump at the top level. It holds scratch relations
    and no `global_flow` or `external_function` file, so it does not compare with a staged run.
  - `fact-dump-diff.sh --outputs` compares only `outputs/`.
- **Stage dumps.** Per-stage scratch is dumped only when `CTADL_DUMP_STAGES` is also set, into
  `stages/<batch>/`. `dump.rs:225` refuses to overwrite, so two batches cannot share a
  directory.
- **In-process tests.** Tests that index through the library compare the two runs' tables with
  a Rust helper. It renders the rows the same way and compares them as sets. This uses no
  environment variable, which is process-wide and shared by tests running on threads of one
  binary.

### D11. Tests

- A seeded shuffle of every `IndexFacts` vector, used by the staged equivalence tests (R14).
- A staged mode for the shared test entry points: `taint_index_with_config` (via
  `IndexConfig`), `flowy::check_with_config` and `cli::index` through `IndexOptions`. Each
  existing test can then run staged and compare outputs with the monolithic serial run.
- A resume test kills the run after a chosen batch (a test hook) and checks the resumed tables.

## Concerns

None open. Settled decisions are folded into the requirements, the design and `intent.md`.
