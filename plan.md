# Staged index fixpoint: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order in which the staged index fixpoint is built,
the files each step changes and the tests that prove it. R* and D* numbers are the spec's. Where
this plan and the spec disagree, the spec wins. Each phase builds, passes the gate named in it, and is one commit.

Code, help text and docs written by these phases must not cite the spec's or the plan's numbers
(R16).

## Status

| Phase | State | Notes |
| --- | --- | --- |
| 0 | done | targets, dumps and numbers in `/Volumes/Shampoo/staged-index-baseline/NOTES.md`. Large target: DuckDuckGo (`com.duckduckgo.mobile.android`, `--no-native-libs`), serial 160 s, 12.4 GB peak |
| 1 | done | gate passes; all 33 relations identical to the Phase 0 dumps: java, noto, cil, pcode, synth and fx serial, `-j 8` and `--lsm`, ddg serial (logs in `/Volumes/Shampoo/staged-index-baseline/phase1/`). `run_intent` takes no config: `intent_rules` reads none |
| 2 | done | all 33 relations identical to the Phase 0 dumps: java, noto, cil, pcode, synth and fx serial and `-j 8`, ddg serial (logs in `/Volumes/Shampoo/staged-index-baseline/phase2/`). Peak not lowered: the fixpoint sets it (ddg 12494 MB vs 12412 MB). Freeing stores lowers only what is held afterwards (2842 vs 3373 MB at `result.try_save`), and moving `paths` saves about 40 MB. `global_flow` rows are appended to `assign_like` after `from_slice`. `seed_paths` is nearly redundant, since a batch passes empty `paths` |
| 3 | done | stage-directory layout files: `paths.parquet`, `plan.json`, `key`, `batches/<id>/batch.json`. `Writer::write_iter_grouped_by` cuts a row group at each key change. Filtered reads skip row groups by `func_id` statistics and also drop rows that don't match the filter |
| 4 | done | `Plan::new(facts, budget, floor, paths)`, where `paths` is the size of the computed path set, because `facts.paths` is about 1/30 of it. Coefficients are named constants in `staged/plan.rs`, fitted from `[mem cp]` lines (`profile.json`'s `bytes_per_row` is empty for serial runs). Adds `Plan.intent_batches` |
| prep | done | all five items; engines agree on `global_flow` and `intent_pair`. The notes take a `CallFacts` (static calls, indirect call sites, declared functions), which Phase 10 can keep when it drops the facts. `IndexResult::final_call` has no callers. `docs/debugging.md` already describes `--outputs`. statm is not yet built on Linux (Phase 7) |
| 5a | done | stage directory and stage 1 in process; `Plan::whole_sccs` keeps every SCC whole. A batch's `assign` and `const_str_assign` rows come back with instruction 0, since their files drop it and no rule reads it. Checkpoint, ddg with a 2 GB budget, run in process by an uncommitted env var in `cli::index`: 57 batches, all run in 35 s; a batch ends at about 2.1 GB footprint, the whole process included. Peak 5.26 GB (monolithic 12.4 GB), wall 219 s (160 s), but no `[mem cp]` checkpoint is above 2.3 GB, so the peak falls between them. Logs in `/Volumes/Shampoo/staged-index-baseline/phase5a/` |
| 5b | done | ddg staged with a 2 GB budget against `--outputs` of a monolithic run of the same build: all 7 tables are the same set. Peak 5.37 GB, wall 247 s; the stage-boundary `[mem cp]` lines stay at or under 2.35 GB, and memguard's 2 s polls saw at most 4.37 GiB, so the peak is a short spike. Candidates: `compute_paths` with the facts alive, and the in-process load and save of the whole result, which the CLI path will not do. Frame: 3247 edges; intent stage: 171286 `assign` rows in, 0.26 s. Logs in `/Volumes/Shampoo/staged-index-baseline/phase5b/` |
| 6 | done | ddg, 2 GB budget, in process, against 5b's monolithic `--outputs`: all 7 tables are the same set, with the default floor (no SCC splits; 57 batches, peak 5.38 GB, wall 272 s) and with floor 1 (183 split SCCs of 646 functions, 991 part runs, at most 7 rounds, largest SCC 50 functions in 2 rounds and 62 part runs; 34 s in split SCCs; peak 5.34 GB, wall 313 s). The ring and mutual-recursion tests rotate three formals instead of extending fields: the path set holds only paths in the facts, so a field-extending ring closes in one round. Every part of an SCC has a callee in another part, so `part_not_rerun_when_callees_stable` checks a cycle whose rerun finds no growth. A split SCC's latest runs move to `batches/<id>/` when it ends, so the frame, intent and assembly readers are unchanged. Logs in `/Volumes/Shampoo/staged-index-baseline/phase6/` |
| 7a | done | `ctadl index-worker` runs a batch, a round's part or the intent stage; `Process` starts one per job, at most `-j` at once, and a job alone with nothing else ready gets every thread. The scheduler starts the first ready batch or split SCC while its estimate fits the budget beside the running ones, so the floor-1 split SCCs of `process_executor_matches_in_process` run beside other batches. `Process` takes no store: a worker touches only the stage directory. The intent stage runs in a worker only when `plan.intent_batches` is non-empty. Test hooks are one variable, `CTADL_STAGED_TEST_HOOK` (`fail=`, `noise=`), passed through `Process::env`; 7b's footprint hook can join it. Linux: the staged tests and `statm_parses` pass in `ghidra-box`. Checkpoint, ddg with a 2 GB budget on workers, serial, against 5b's monolithic `--outputs`: all 7 tables are the same set, peak 4.82 GiB (memguard, process tree), but wall 2633 s (in process 272 s): each of the 58 workers reads the path set in about 40 s and 1.03 GB, which is 97% of worker time and half the budget. A shared, memory-mapped path set is now needed: see [After 7a: the path set](#after-7a-the-path-set). Logs in `/Volumes/Shampoo/staged-index-baseline/phase7a/` |

## Order

| Wave | Tracks | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1 | splits the rule source (`ctadl-index/src/index_engine/mod.rs`, engines) |
| 2 | A: 2. B: 3 then 4. C: prep | three worktrees. A refactors `EngineInput` (`mod.rs:1364-1827`); B adds the stage-file layer, planner and shuffle (new `ctadl-index/src/staged/`, `facts/parquet.rs`, `facts/schema.rs`); C does the prep below. C's only edit to `mod.rs` is `phys_footprint_mb` (`:802-805`), and it merges before A rebases |
| 3 | 5a, then 5b | 5a: write the stage directory and run stage 1 in process, compared on `summary` and `assign`. 5b: stage 2, stage 3 and assembly, compared on all seven tables. Needs A, B and C's `compare.rs`. Checkpoint after 5a: ddg staged in process, peak against the largest batch |
| 4 | 6 | split SCCs in rounds, floor down to one function. Checkpoint: rounds on the ring test and on real split SCCs |
| 5 | 7a | the `index-worker` subcommand, the `Process` executor, footprint over stdout; launches gated on estimates only |
| 6 | 7b, 8, 9 | 7b: kills, batch splitting, α correction (`executor.rs`, `plan.rs`). 8: the in-process equivalence harness (`ctadl-ascent/src/index_engine.rs`, CI). 9: resume (`run.rs`, `store.rs`), which needs only 7a's scheduler. Checkpoint after 7b: ddg under a budget |
| 7 | 10 | the CLI runs staged on request: `cli::index` wiring, flags, `EngineRecord`, profile stages, the `index_options()` test helper |
| 8 | 11 | staged becomes the default |
| 9 | 12, 13 | 12 is `docs/spec.md`; 13 is the benchmark procedure (`docs/debugging.md`) and calibration (`ctadl-index/src/staged/plan.rs`) |

Prep (track C) pulls forward the parts of later phases that do not depend on staging, one commit
each. The phase sections below still describe them where they were; those phases skip them.
- From Phase 8: `canonical()` and `assert_same_index` gain `global_flow` and `intent_pair`.
- From Phase 5: `ctadl-ascent/src/staged/compare.rs`, the set-by-content table comparison (D10),
  tested on two monolithic runs.
- From Phase 10: `dump_outputs` and `fact-dump-diff.sh --outputs`, so Phases 5-9 can compare
  tables on real projects before the CLI runs staged.
- From Phase 10: `push_zero_assignment_notes` takes the summarized-function set, the resolved
  calls and the external functions instead of `&IndexResult` (D9).
- From Phase 7: the non-macOS `phys_footprint_mb` reads `/proc/self/statm`.

Why this order:
- **Rules first.** Phases 1 and 2 change no results. A regression after them has one cause, and
  `fact-dump-diff.sh` against the Phase 0 dumps proves it.
- **Layers before use.** Phases 3 and 4 are additive: new code tested alone, with no callers.
- **Correctness before scale.** Phase 5 gets the staged run right in one process, where it is
  easy to test. Phase 6 adds the hard case, split SCCs. Phase 7 adds processes and the budget,
  which change memory and not outputs. Phase 9 adds resumption on top.
- **The harness before the flip.** Phase 8 can compare every engine test's staged and
  monolithic outputs before Phase 11 makes staged the default. So the flip, which reroutes
  every test, is a one-line change whose effect is already measured.
- **Shared files in wave 6.** Phase 7b changes how a staged run is executed
  (`ctadl-ascent/src/staged/`). Phase 8 only calls it through `taint_index_with_config`
  (`ctadl-ascent/src/index_engine.rs`), which Phase 7 does not touch.

Test commands used throughout:

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test --workspace
    cargo xtask regression            # in the Nix dev shell
    scripts/fact-dump-diff.sh compare REF -- <ctadl> index <project> [flags]

Build products go wherever the developer's environment says (`CARGO_TARGET_DIR`). Measurement
output goes to a directory outside the repository.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record the reference outputs and numbers on the unchanged tree.

**Measurements.**
1. A release build of `ctadl`.
2. Relation dumps with `scripts/fact-dump-diff.sh record` for:
   - one project per front end the regression suite covers, taken from its cases;
   - a synthetic large target from
     `scripts/gen-locals-bench.py --funcs 2000 --group-size 8 --paths 2`;
   - the largest real target at hand, such as an APK from the `report-eval` corpus.

   Phases 1 and 2 must reproduce these dumps exactly.
3. On the two large targets, under `memguard.sh` and `/usr/bin/time -l`:
   - the peak footprint and wall time of `ctadl index` (serial, then `-j 8`);
   - `RUST_LOG=debug` `[mem cp]` lines and `profile.json`.

   Phase 13 and the acceptance checks compare against these.
4. Wall time of `cargo test --workspace`, so the cost of Phase 8's harness can be judged.

All output goes outside the repository. Note the targets, commands and numbers in the Phase 0
commit message, or in a file outside the repository that the message names.

**Gate.** None. The commit is empty, or skipped if nothing is written.

## Phase 1: intent rules as their own source (R4, D5)

Depends on: Phase 0.

**What.** Move the intent rules out of `index_rules` into a second `ascent_source!`,
`intent_rules`. The full programs include `intent_rules` first, then `index_rules`, so that
`index_rules`' declarations win: ascent keeps the last declaration of a relation
(`dedup_all_keep_last_by` in `ascent_macro`), as the LSM engine already relies on
(`ctadl-index/engines/lsm/src/lib.rs:20-25`). Add a stage-3 program that includes only
`intent_rules`. This changes no behavior.

**Files.**
- `ctadl-index/src/index_engine/mod.rs:965-1317`. `index_rules` loses:
  - the intent rules at `:1177-1215` (`const_reaches` ×2, `intent_pair` ×3, `resolved_call`);
  - the label exchange at `:1282-1316` (`global_read`, `intent_frame(GLOBAL_FRAME)`, frame
    `const_reaches`, `global_label_key`, `global_label_read`, `const_reaches` down);
  - the declarations only those rules use: `intent_send`, `intent_filter`, `intent_component`,
    `intent_frame`, `const_reaches`, `resolved_call`, `intent_pair`.

  `const_str_assign` stays declared in `index_rules`, since `global_mention` reads it (`:1028`).
  - The new `intent_rules` holds those rules. It declares every relation it reads:
    `const_str_assign`, `assign_like` (no `#[ds]`), `path_set`, `intent_*`, `const_reaches`,
    `resolved_call`, `intent_pair`, `global_read`, `global_label_key`, `global_label_read`.
  - Field types are spelled exactly as in `index_rules`, through `rules_prelude`
    (`:1544-1557`). Ascent dedups declarations by name and field types compared token by token,
    so a different spelling would declare the relation twice.
  - Update the module doc (`:14-16`) and the `index_rules` doc line.
- `ctadl-index/engines/{serial,par,lsm}/src/lib.rs`: `include_source!(…intent_rules);` before
  `include_source!(…index_rules);`.
- `ctadl-index/engines/serial/src/lib.rs`, additive:
  - `ascent! { struct IntentProg; include_source!(ctadl_index::index_engine::intent_rules); }`;
  - `pub fn run_intent(input: IntentInput) -> IntentOutput`. It seeds `assign_like` source
    first (`(f, v2, p2, v1, p1)`, as `seed_assign_like` does, `mod.rs:723-728`) and returns
    `intent_pair` and `resolved_call`.
  - `IntentInput` and `IntentOutput` are plain structs in `ctadl-index/src/index_engine/mod.rs`,
    beside `EngineInput`.
- `ctadl-index/src/index_engine/dump.rs:306-316`. `dump_relations!` keeps listing the moved
  relations. They are still fields of the full programs, so the dump is unchanged.

**Tests.**
- New, `ctadl-ascent/src/index_engine_tests.rs`: `intent_program_matches_full_program`. On an
  intent fixture (the Android intent facts used by `cli.rs`' intent tests, or a hand-built
  `IndexFacts` with `intent_frame`, `intent_send`, `intent_component` and a global label path),
  it runs the serial engine. It then seeds `run_intent` with:
  - the result's `assign_like` rows of the `intent_frame` functions;
  - the frame rows;
  - those functions' `const_str_assign`, `intent_send` and `intent_frame`;
  - `intent_filter`, `intent_component`, the path set and the config.

  It asserts equal `intent_pair` and `resolved_call` sets.
- Existing, unchanged: all of `cargo test --workspace`, including
  `parallel_engine_matches_serial`, `lsm_storage_matches_memory` and `fixpoint_interns_nothing`.
- Regression: `fact-dump-diff.sh compare` against every Phase 0 dump, serial, `-j 8` and
  `--lsm`. Every relation must be identical.

**Gate.** fmt, clippy and `cargo test --workspace`, plus the dump comparisons above.

## Phase 2: engine input from a slice (D2)

Depends on: Phase 1.

**What.** Split `EngineInput::new` (`ctadl-index/src/index_engine/mod.rs:1384-1583`) into two
parts:
1. **`ProgramPaths::from_facts(&IndexFacts, deref_paths)`.** The program-wide part:
   `program_paths`, `summary_paths`, the `actual_param` paths, then `compute_paths` and
   `PathSet::from_paths` (`:1419-1537`).
2. **`EngineInput::from_slice(slice, paths, config, id_map)`.** The per-function part: `copy_edge`,
   `prog_store`, `alias_of_formal`, `assign` to `assign_like`, `call`, counts. It takes the
   facts of some functions and seeds them. `compute_alias_of_formal` (`:819`) is already per
   function: it joins only within one `FunctionId`.

`ProgramPaths { paths: Vec<(Path,)>, path_set: PathSetRef }` also has `from_paths(Vec<Path>)`,
which runs `PathSet::from_paths` only, with no `compute_paths`. `from_facts` calls it, and
Phase 5 calls it on the rows read back from the stage directory's `paths`. A batch never
recomputes the path set from facts.

`EngineInput::new` becomes `from_slice(facts, ProgramPaths::from_facts(&facts, …), …)`.
`from_slice` takes `ProgramPaths` by value.
- **The monolithic run** moves the whole thing in.
- **A staged batch** gets a `ProgramPaths` with an empty `paths` vector and an `Arc` clone of
  `path_set`. No batch copies the program's path vector.
- **`seed_paths`.** `EngineInput` gains `seed_paths: bool`. When it is false, `run_engine!`
  leaves the `paths` relation empty (no rule reads it, `mod.rs:979-982`), and
  `IndexResult.paths` is empty. Assembly writes `paths` once, from the scheduler's copy. When it
  is true, `run_engine!` moves `paths` into the relation (`paths.into_iter()` instead of
  `paths.iter().cloned()` at `mod.rs:1687`), so the monolithic run stops holding the paths
  twice during the fixpoint.
- **Program-wide facts.** `from_slice` expects `facts.global_flow` and `facts.paths` empty:
  both are program-wide, and go to stage 2 and to `ProgramPaths`. A debug assertion checks
  this. The monolithic `new` moves `global_flow` into the frame rows before calling
  `from_slice`, as `mod.rs:1477-1488` does today.

`run_engine!` also drops the `locals` store and the other derived stores (`std::mem::take` on
their `ind_common` fields) before `take_assign_like` (`mod.rs:731`, `:1798`). Today the
`assign_like` `Vec` is built while `locals`, the largest relation, is still alive. The
relation census and the dump run before this, so they are unchanged.

Changes no behavior. The monolithic run's peak may drop by one copy of `paths` and by the
overlap of `locals` with the `assign_like` copy.

**Files.**
- `ctadl-index/src/index_engine/mod.rs`:
  - `:1364-1584`: the split.
  - `run_engine!` at `:1666-1688`: honor `seed_paths`.
  - `run_engine!` at `:1817-1827`: take `paths` from the input when it was not seeded.
- `ctadl-ascent/src/index_engine.rs:33`: no change, since `new` keeps its signature.

**Tests.**
- New, `ctadl-index/src/index_engine/mod.rs` tests module: `slices_union_to_whole_input`. On a
  small hand-built `IndexFacts` with two functions and no calls, it checks that `from_slice` on
  each function's facts gives `prog_store`, `alias_of_formal` and `assign_like` whose unions
  equal `new`'s.
- New, same module: `program_paths_from_paths_matches_from_facts`. `from_paths` on
  `from_facts`' `paths` gives a set with the same members.
- Existing: `cargo test --workspace` unchanged.
- Regression: `fact-dump-diff.sh compare` against the Phase 0 dumps, identical, serial and
  `-j 8`. Peak footprint on the Phase 0 synthetic target is within noise of baseline.

**Gate.** fmt, clippy, `cargo test --workspace`, the dump comparisons.

## Phase 3: stage files (D8, R10, R11)

Depends on: Phase 0.

**What.** This is the on-disk layer of the stage directory. It is additive, with no callers.

**Files.**
- New `ctadl-index/src/staged/mod.rs` (declared in `ctadl-index/src/lib.rs:7`) and
  `staged/store.rs`. Only this phase edits `staged/mod.rs`, and it declares the submodules that
  phase 4 adds (`plan`, `shuffle`) as empty files, so Phase 4 does not edit it.
  - `StageDir` holds the directory layout under `<index>/staging/`: `facts/`, `paths`, `plan`,
    `key`, `batches/<id>/`, `rounds/`, `frame/`, `intent/`, and `done/` markers.
  - `write_atomic(path, f)` writes to a temporary name and renames into place.
  - `mark_done(id)` and `is_done(id)` write and test a marker the same way.
  - `RunKey` is a SHA-256 over the saved fact files' bytes, in a fixed file order, plus the
    serialized engine config (alias rule, deref paths, budget, floor). The engine kind is left
    out, since it does not change results. `ctadl-index/Cargo.toml` gains `sha2 = "0.10"`,
    which `source-info` already uses.
- `ctadl-index/src/facts/parquet.rs:144-172`. Add `Reader::rows(columns)`, an iterator that decodes one row group at a time and so can
  feed `Writer::write_iter`, and `Reader::for_each_batch(columns, f)`, which
  streams one row group at a time. Add a variant that skips row groups whose `func_id`
  min/max statistics miss a sorted set of `FunctionId`s. Batch files are written sorted by
  `FunctionId`. `Writer` gains a row-group size option (default `ROWS_PER_GROUP`,
  `parquet.rs:175`), which stage files set to 4K rows.
- `ctadl-index/src/facts/schema.rs`. Add schemas for the facts `IndexFacts::try_save` omits that
  a batch needs:
  - `const_str_assign` keyed `(func_id, var, path, str)`;
  - `intent_send`, `intent_filter`, `intent_component`, `intent_frame`;
  - `fact_summary`, the seeded summaries, in `summary`'s encoding;
  - `frame_edge`, in `global_flow`'s encoding.

  `assign` for a batch uses the existing `assign` schema (`schema.rs:62`). All are in the
  index's parquet encoding (`parquet.rs:554`). `INDEX_FORMAT_VERSION`
  (`ctadl-import/src/project.rs:187`) does not change: no final table changes (R3).

**Tests.**
- New, `ctadl-index/src/facts/schema.rs` tests: round-trip each new schema across two row groups
  (as `schema.rs:217` does).
- New, `ctadl-index/src/staged/store.rs` tests:
  - `filtered_reader_skips_row_groups`: 3 row groups by `FunctionId` ranges; a filter on one
    range decodes one group.
  - `atomic_write_leaves_no_partial_file`: a closure that fails leaves neither the file nor its
    marker.
  - `run_key_changes_with_facts_and_config`: one changed byte or config field gives another key.

**Gate.** fmt, clippy, `cargo test -p ctadl-index`, `cargo test --workspace`.

## Phase 4: plan and shuffle (D1, D6 estimate and floor, D11 shuffle)

Depends on: Phase 3.

**What.** The planner and the seeded fact shuffle. They are additive, with no callers.

**Files.**
- `ctadl-index/src/staged/plan.rs`:
  - `call_graph(&IndexFacts)`: nodes are every `FunctionId` in a per-function fact (D1), edges
    are `call` from caller to callee, and `GLOBAL_FRAME` is not a node. Nodes are renumbered
    densely in `FunctionId` order, so `Sccs::new` (`ctadl-ir/src/graph/scc/mod.rs:119`) can take
    them. Follow `ChaCallGraph` (`ctadl-ascent/src/report/callgraph.rs:1745-1764`).
  - SCCs are taken from `Sccs::all_sccs()`, which yields callees first (`scc/mod.rs:147`).
  - `Estimate` per function:
    - inputs: `assign` rows, instantiated callee-summary rows (the sum over its `call` sites of
      the callee's estimated summary rows), variables and formals. Callees in the same SCC are
      estimated from their own facts alone, with no iteration, since their summaries depend on
      the caller's;
    - initial coefficients: bytes per row from `profile.json`'s `bytes_per_row` and Phase 0's
      `[mem cp]` lines;
    - a fixed per-worker term for the path set, which every worker holds whatever its size:
      `profile.memory.interned["PathSet"]` (`mod.rs:1783-1789`) plus the interned split keys
      `PathSet::from_paths` adds (`path_set.rs:130-168`). Packing counts it once per batch, so
      more, smaller batches cost more, and the planner prefers fewer, larger ones (D7);
    - Phase 13 recalibrates them.
  - `Plan::new(facts, budget, floor)`:
    - It packs consecutive whole SCCs, callees first, into batches up to the budget. A batch's
      out-of-batch callees are in earlier batches.
    - An SCC over the budget, or larger than the floor allows, is split into parts. With floor
      1, every function of a recursive SCC is its own part.
    - The parts are contiguous runs of one callees-first order of the SCC: the postorder of a
      DFS over its internal edges, from its lowest `FunctionId`, children in `FunctionId` order
      (D3). Contiguous runs of a DFS order cut fewer internal edges than an arbitrary split,
      and every edge between parts costs rounds (Phase 6).
    - The plan records, for each batch, its functions and its out-of-batch callees and the
      batches that hold them, and the batches each batch depends on, so concurrency (D2) can be
      read from it.
    - It is serialized to JSON in the stage directory, once for the scheduler, and as one
      small descriptor per batch (its functions, its callees' batches, its estimate), so a
      worker reads only its own descriptor, not the whole plan.
  - Determinism: every order is by `FunctionId`, never by fact order.
- `ctadl-index/src/staged/shuffle.rs`:
  - `shuffle_facts(&mut IndexFacts, seed)` is a Fisher–Yates shuffle of every vector, with an
    LCG. The repository's style avoids a `rand` dependency; copy the shape of
    `ctadl-index/src/facts.rs:2000-2006`.
  - It is `pub`, not `cfg(test)`, because the `ctadl-c` and `ctadl-ascent` tests use it from
    other crates.

**Tests.** All new, in `ctadl-index/src/staged/plan.rs`:
- `chain_is_callees_first`: `a→b→c` gives batches ordered `c, b, a` with budget for one
  function each.
- `diamond_packs_under_budget`: 4 functions, a budget fitting 2, gives 2 batches, and every
  batch's callees are in earlier batches.
- `cycle_splits_at_floor_one`: an SCC `{a,b,c}` (a→b→c→a) with floor 1 gives 3 parts in one
  split SCC, ordered by the internal edges.
- `function_without_formals_is_planned`: a function with only `assign` rows is in some batch
  (D1).
- `plan_ignores_fact_order`: shuffled facts (seeds 1..10) give byte-identical plan JSON.
- `oversize_function_is_alone`: one function over the budget gets a batch of its own.

**Gate.** fmt, clippy, `cargo test -p ctadl-index`, `cargo test --workspace`.

## Phase 5: staged run in one process (R1, R2 for whole SCCs, R4, R9-R11, D2, D4, D5, D9)

Depends on: Phases 1, 2, 3, 4.

**What.** Add a staged run reachable through `taint_index_with_config`, with
`IndexConfig.staged`. Default stays monolithic, so no existing behavior changes. A staged run:
1. Writes the stage directory (D8):
   - **Facts.** One file per relation, partitioned by batch. Each fact `Vec` is sorted in
     place by `(batch, FunctionId)`, which is O(N log N) with no copy, and written with a row
     group cut at each batch boundary. A batch selects its rows with the filtered reader. This
     avoids both filtering once per batch (O(B·N)) and holding a writer open per batch.
   - **Other files.** The `IdMap`, the path set (as `paths`), the plan and the key.
   - **Then the facts are dropped.** The scheduler keeps only what the notes need (D9:
     `call`, the indirect calls, `external_function`, the declared functions), so stage 1
     does not run beside the whole program's facts (R7).
2. Runs stage 1 batch by batch in plan order, on the in-process executor (D7).
   - **Path set.** Each batch runs `EngineInput::from_slice` with `seed_paths: false`, and
     with a `ProgramPaths` that shares the path set. The path set is built once per process,
     with `ProgramPaths::from_paths` on the stage directory's `paths`, and cached in-process
     by run key.
   - **Seeds.** The batch's facts, and the `call` rows whose caller is in the batch. Its
     out-of-batch callees' `summary` rows are streamed from their batches' files with the
     filtered reader, then go through `distinct_rows` together with the batch's own seeded
     summaries, as `run_engine!` does today (`mod.rs:1670`). Intent facts and `global_flow`
     are left empty.
   - **Outputs.** Rows are filtered to the batch's functions, so the seeded callee rows drop
     out and batch outputs are disjoint:
     - its functions' `summary` rows;
     - its functions' `assign_like` rows;
     - its `GLOBAL_FRAME` rows, written separately as frame edges.

     All are written atomically, sorted by `FunctionId`, then marked done.
   - **Timeout.** A batch whose fixpoint hits `CTADL_INDEX_TIMEOUT_SECS` fails the run instead
     of writing partial rows.
   - **Dumps.** `run_engine!`'s `CTADL_DUMP_RELATIONS` dump is suppressed in batches here, not
     in Phase 10, since a second batch in one process would panic on `create_new`
     (`dump.rs:225`). Phase 10 adds the `outputs/` and `stages/` dumps.
   - **Row groups.** Stage files are written with 4K-row groups (a `Writer` option), not the
     64K default (`parquet.rs:175`). A batch reading one popular callee's rows then decodes
     4K rows of text paths, not a whole file.
3. Stage 2 (D4) streams every batch's frame rows plus the input `global_flow` through a hash
   set and writes `frame/` atomically. It holds the distinct frame rows: the same rows the
   monolithic run holds, bounded by pairs of global paths, not by functions. It is not
   counted against the budget. Phase 13 reports its size.
4. Stage 3 (D5) runs `run_intent` (Phase 1) only if `intent_frame` is non-empty, seeded as D5
   says. It opens only the batches that hold `intent_frame` functions, which the plan names.
   Its size, like stage 2's, is reported in Phase 13 and is outside the budget.
5. Assembly (D9) streams the batch files into the final tables, table by table, through the
   per-table `schema::*::try_save(dir, iter)` (`schema.rs:23`), which feeds
   `Writer::write_iter` (`parquet.rs:192`). It does not use `IndexResult::try_save`, which owns
   whole `Vec`s. Phase 3's reader gains an iterator form, one decoded record batch at a time,
   so it can feed `write_iter` without collecting. `profile.json` is written separately.
   The tables:
   - `summary` and `assign` are the batch files plus the frame rows;
   - `paths` is from `ProgramPaths`;
   - `external_function` comes from the facts;
   - `resolved_call` and `intent_pair` come from stage 3;
   - `global_flow` is the frame rows.

   `taint_index_with_config` then returns `IndexResult::try_load` of a temporary index
   directory. That is fine for its in-process callers, which are tests and small inputs; the CLI
   path (Phase 10) never loads the result.

In this phase the plan never splits an SCC: the floor is clamped to whole SCCs until Phase 6.

**Files.**
- `ctadl-index/src/index_engine/mod.rs:319-342`. `IndexConfig` gains:
  - `staged: Option<StagedConfig>`, where `StagedConfig { budget: u64, floor: usize }`;
  - `None` is monolithic.

  The default stays `None`. It must keep `Eq`, `Ord` and `Hash` (`:319`), since `config` is a
  relation.
- `ctadl-index/src/index_engine/dump.rs:115-124`. `DumpCol for IndexConfig` leaves out `staged`,
  like `parallelism`, so staged and monolithic dumps compare.
- `ctadl-ascent/src/cli/mod.rs:762-767`: the literal gains `staged: None` (wired in Phase 10).
- `ctadl-ascent/src/index_engine_tests.rs:242-250`: literals as needed.
- New `ctadl-ascent/src/staged/{mod.rs,run.rs,batch.rs,executor.rs}` (declared in
  `ctadl-ascent/src/lib.rs`), which hold the engine-dependent part:
  - `run.rs`: `run_staged(facts, config, id_map, stage_dir, index_dir)`.
  - `batch.rs`: `run_batch(stage_dir, batch_id)`, the worker entry point. It runs
    `ctadl_index_serial::run`.
  - `executor.rs`: the `Executor` trait, with `InProcess`, which runs batches one at a time on
    the calling thread.
- `ctadl-index/src/staged/store.rs`: readers and writers per stage file, if Phase 3 left any
  out. `KeyConfig` drops `engine`, as the spec's run key says. Fact files are sorted totally
  (batch, `FunctionId`, then the row), so the same facts give the same bytes and the same key.
  `StagedConfig`'s default floor is unbounded (`usize::MAX`).
- `ctadl-ascent/src/index_engine.rs:27-71`: dispatch on `config.staged` first.

**Tests.**
- New, `ctadl-ascent/src/staged/tests.rs`. Each test compares the staged run's seven tables, as
  sets by content (D10), with the monolithic serial run's:
  - `staged_matches_monolithic_chain`, `_diamond`, `_globals`: hand-built facts, including a
    global-to-global flow through two batches and a function without formals (D1).
  - `staged_matches_monolithic_intent`: the Phase 1 intent fixture.
  - `staged_matches_monolithic_summary_project`: hand-built facts with an input `global_flow`
    row, as a `--summary` project loads it (folded into the frame at `mod.rs:1477-1488`), and a
    function of the consumer that reads one of its globals.
  - `staged_matches_monolithic_flowy`: every `.tnt` under `ctadl-ascent/tests/tnt/` that has
    no recursion. A tight budget gives several batches, and facts are shuffled with seeds 1..3.
  - `batch_reads_only_callees`: a counting reader asserts that a batch opens only the
    `summary` files of batches holding its callees (R11).
- New: `output_comparison` helper (D10), in `ctadl-ascent/src/staged/compare.rs`. Phases 8 and
  10 reuse it.
- Existing: `cargo test --workspace` unchanged (default monolithic).

**Gate.** fmt, clippy, `cargo test --workspace`.

## Phase 6: split SCCs (D3, R2 down to one function)

Depends on: Phase 5.

**What.** Run a split SCC's parts in rounds, as D3 says.
- **Seeds.** Each part reads callee summaries outside the SCC from earlier batches. It reads
  those of other parts from each part's latest done run, which a small per-SCC index file
  names, so no part searches back through rounds.
- **Reruns.** A part reruns only if a callee part's `summary` grew since it last ran. Growth is
  a row count, kept per `FunctionId` rather than per part, so it survives Phase 7 splitting a
  part. Counting is sound only under three conditions, which the phase enforces:
  - Every run reached its fixpoint. In a staged worker, `run_engine!` hitting
    `CTADL_INDEX_TIMEOUT_SECS` (`mod.rs:1560`, `:1701-1706`) fails the run instead of
    returning partial rows. Phase 5 already applies this to every batch.
  - Seeds only grow.
  - A function's count never falls. A debug assertion checks it, and the tests check that the
    new rows include the old ones.
- **Estimates.** A part is re-estimated from its actual seed counts before each rerun, since
  later rounds seed it with more callee summaries than the plan saw.
- **End.** Rounds stop when nothing grows. A part's outputs are written to
  `rounds/<scc>/<part>/<round>/` and marked done. Its earlier round is then deleted, so disk
  holds one run per part, and assembly reads only that run. The SCC is done when the last
  round is.
- **Logging.** Rounds, reruns, part runs and time per round are logged at `info` for each split
  SCC.
- **Floor.** The floor is no longer clamped.

**Files.**
- `ctadl-ascent/src/staged/run.rs`, `batch.rs`: the round loop, and batch inputs that name a
  round.
- `ctadl-index/src/staged/store.rs`: round paths and markers.
- `ctadl-index/src/staged/plan.rs`: remove the clamp.

**Tests.**
- New, `ctadl-ascent/src/staged/tests.rs`:
  - `ring_rounds_are_recorded`: a synthetic ring of n functions (n = 16, 64) with
    field-extending recursion, floor 1. It equals monolithic, and logs the rounds and part
    runs. This is the worst case for rounds, and records its cost. The test asserts no bound;
    Phase 13 reports the numbers on real targets.
  - `split_scc_matches_monolithic`: mutual recursion `a↔b` with field flow that needs two
    rounds to close. Floor 1 equals monolithic, and the log shows at least 2 rounds.
  - `staged_matches_monolithic_flowy_floor_sweep`: every `.tnt` fixture, floors {default, 4, 2,
    1}, shuffled seeds 1..3.
  - `part_not_rerun_when_callees_stable`: a 3-part SCC where one part has no in-SCC callee runs
    once.
- Existing: Phase 5's tests unchanged.

**Gate.** fmt, clippy, `cargo test --workspace`.

## Phase 7: worker processes and the budget (R7, R8, D2, D6 enforcement, D7)

Depends on: Phase 6.

**What.** Add the `Process` executor and the scheduler loop.
- **Workers.** A hidden subcommand `ctadl index-worker --store S <stage_dir> <batch_id>` runs
  `run_batch`. It is the first `#[command(hide = true)]` in `main.rs:30-60`. The worker:
  - calls `ctadl_ascent::init()`;
  - starts a thread that samples its footprint every 100 ms and writes it to stdout, a line
    per sample (`phys_footprint_mb`; on Linux, resident set size from `/proc/self/statm`).
    The scheduler drains each worker's stdout on a thread of its own. Worker stderr goes to a
    file in the stage directory, never to a pipe: debug logging would fill a pipe and block
    the worker at 0% CPU.
  - logs the time and bytes of loading the path set;
  - exits.
- **Scheduler.** It keeps a ready set from the plan's dependencies, in plan order.
  - **Launching.** It launches the head of the queue while the sum of the running estimates
    plus the head's fits the budget. It does not skip a large head to launch smaller ones
    behind it, which would starve the head. When nothing is running, it always launches the
    head, even over the budget: a single function over the budget runs alone (R8).
  - **Over the budget.** It sums the latest reported footprints. A killed worker leaves the
    sum at once, so a stale sample cannot trigger a second kill. Over the budget, it starts
    nothing new, then:
    - It kills the largest worker that holds more than one function.
    - If the overrun comes from a single-function worker, it kills and requeues the others,
      so that one runs alone.
    - A killed batch's files are discarded.
  - **Splitting.** A killed batch is split in two only if its own footprint, less the
    per-worker path-set term (Phase 4), is over the budget alone. Otherwise it is requeued
    whole, and the scheduler runs fewer workers at once.
  - **Correction.** After a kill, it raises a scale factor α = max(observed / estimated) and
    applies it to every queued estimate, so a uniform underestimate costs one kill, not a
    cascade down to single functions.
  - **Facts.** It holds only batch ids, dependencies and estimates while workers run. The
    `IndexFacts` and the call graph are dropped once the stage directory is written.
- **Engines.** A worker running alone may use the parallel engine with the run's `-j` threads
  (D2). Otherwise workers are serial.
- **Failure.** A worker exiting non-zero, other than by a kill, fails the run with its stderr.
- **In-process.** `InProcess` stays for `taint_index_with_config`, which has no binary. It runs
  batches one at a time, and does no budget enforcement.

**Files.**
- `ctadl-ascent/src/staged/executor.rs`: `Process { exe, store, jobs }`; the scheduler loop.
- `ctadl-ascent/src/staged/footprint.rs`: the sampler and the Linux reader.
- `ctadl-index/src/index_engine/mod.rs:802-805`: the non-macOS `phys_footprint_mb` reads
  `/proc/self/statm`. A one-function change.
- `ctadl-index/src/staged/plan.rs`: `split_batch(batch) -> (Batch, Batch)` (by SCC, or by
  function within an SCC, which makes it a split SCC).
- `ctadl-ascent/src/main.rs:30-60`, `:545-585`: the hidden `IndexWorker` variant and its arm.
- `ctadl-ascent/src/cli/mod.rs:64-102`: `IndexOptions` gains
  `worker_exe: Option<&'static std::path::Path>`, which keeps `Copy`. `None` means in-process.
  Update the exhaustive destructure at `cli/mod.rs:381-391`. It is not used until Phase 10.

**Tests.**
- New, `ctadl-ascent/tests/integration/staged_workers.rs` (registered in
  `tests/integration/main.rs`), using `env!("CARGO_BIN_EXE_ctadl")`:
  - `process_executor_matches_in_process`: a flowy fixture through the `Process` executor
    equals the in-process run.
  - `over_budget_batch_is_split_and_rerun`: a test hook forces a footprint report over the
    budget for the first batch. The batch is killed, split and rerun, and the outputs equal
    monolithic.
  - `single_function_is_never_killed`: the same hook on a one-function batch lets it finish,
    and a log line says so.
  - `uniform_underestimate_kills_few`: a hook scales every estimate by 1/2. The kills number
    at most about log₂(batches), and no batch that fits alone ends up split.
  - `verbose_worker_does_not_block`: with `RUST_LOG=trace`, a worker writing over 1 MB to
    stderr completes.
  - `worker_failure_fails_run`: a hook makes the worker exit 3; the run fails with its stderr,
    and completed batches stay on disk.
- New, `ctadl-ascent/src/staged/footprint.rs`: `statm_parses` (Linux only, `cfg`).
- Run on Linux in the `ghidra-box` VM: `cargo test --workspace -- staged`.

**Gate.** fmt, clippy, `cargo test --workspace`, and the Linux run above.

### After 7a: the path set

7a's ddg checkpoint (Status, row 7a) produced the same tables as monolithic, but took 2633 s
on workers against 272 s in process. Almost all of the difference is the path set.

**Where the time goes.** A batch gets its path set from `path_set` (`ctadl-ascent/src/staged/batch.rs:132-157`).
It reads `paths.parquet` into a `Vec<Path>` and rebuilds the frozen set with
`ProgramPaths::from_paths`, which interns every path and its splits. A process caches the
result by run key (`batch.rs:127-129`).
- In process, the scheduler lends its own path set (`share_path_set`, `batch.rs:161`), so no
  batch reads it. That is why the in-process run is fast.
- `Process` drops the lent path set (the trait default, `executor.rs:97-101`), so each worker
  is a new process with an empty cache and reads the path set again. On ddg that is about 40 s
  and 1.03 GB per worker, 97% of worker time, 58 times.

**How to measure it.** Each worker logs
`read the path set of N paths in X s, Y MB` (`batch.rs:150`) at info level, in its log under
the stage directory's logs (`executor.rs:231`). Summing X over the worker logs gives the
time spent loading. The CLI does not run staged until Phase 10 (`cli/mod.rs:434` ignores
`worker_exe`), so the checkpoint used a change that was not committed. Rerunning it needs
the same kind of change.

**What takes the time.** `cargo run --release -p ctadl-index --example path_set_load -- <paths.parquet> --steps`
times the load alone, outside a run. On the largest path set at hand (Chrome's `paths.parquet`,
7.9M paths, 26.7M segments, depth at most 10), the load takes 32 s and 1.1 GB, so ddg's 40 s
is not a fluke:
- Decoding and interning the paths: 11 s, about 400 ns per segment. A `Path` column is a
  string (`ctadl-index/src/facts/parquet.rs:862-895`). With `--decode`, parsing the strings
  takes 3.3 s, and consing every segment takes 7.1 s even when every node is already interned
  (26.7M lookups, about 265 ns each). There are 8.0M distinct nodes, about one per path, since
  the path set holds each member's tails. An encoding that stores each path as its head segment
  and its tail's row would cons once per path and parse no strings, so it would cut the 11 s to
  about 2-3 s. It does not touch `from_paths`.
- `PathSet::from_paths`: 20 s, and almost all of that is the splits. `Path::prefix_keys` takes
  18 s for 34.6M splits, and `prefix_keys_wild` 6 s (each timed alone, so with interning).
  Sorting takes 0.1 s.
- A prefix key is a new cons list. `prefix_keys` builds each one from scratch with
  `Path::from_accesses`, so a path of depth d does about d²/2 interner lookups
  (`ctadl-index/src/facts.rs:417-428`). Each lookup is a hash probe into a sharded table of
  tens of millions of nodes (`immortal/src/lib.rs:34-59`), which is mostly cache misses. The
  time stays the same when every key is already interned: `from_paths` took 20 s after the
  step timings had interned them all. So the cost is the lookups, not new nodes.

**What it does to the budget.** The scheduler and the planner charge each worker
`worker_bytes(paths)` = `WORKER_BASE_BYTES` + `PATH_BYTES` (176 B) per path
(`ctadl-index/src/staged/plan.rs:47-58`). On ddg the measured 1.03 GB is half of a 2 GB budget,
before the batch's own rows, and it is paid once per worker, so two workers at once hold
2 GB of path set alone. Check `PATH_BYTES` against the logged MB and path count:
its 176 B was measured in process.

**Why it is not deferred.** Spec D7 ("Path set", `spec.md:265-268`) expected the cost to
stay small enough that the planner's preference for fewer, larger batches would cover it. It
said a memory-mapped path set shared across workers would come later "if it dominates". It
now does. With a shared path set, a worker would map the paths and splits the scheduler
built, instead of reading and interning them again (interners: `ctadl-index/src/index_engine/mod.rs:31-37`).

**Open.** Whether this becomes its own phase before 7b, or part of 7b. It matters for 7b
because 7b's split decision subtracts the per-worker path-set term (Phase 7, "Splitting").
While that term is half the budget, the ddg checkpoint after 7b would mostly measure the path
set.

## Phase 8: equivalence harness for in-process tests (R14, part)

Depends on: Phase 6.

**What.** Add a check mode, set by `CTADL_INDEX_CHECK_STAGED=1`, read once. In it,
`taint_index_with_config` runs the monolithic serial engine as usual, and also runs staged at
floors {default, 1}, each on facts shuffled with seed 1. Any output difference fails with the
first differing rows of each table. The comparison is Phase 5's helper. This covers every test
that indexes through `taint_index`, `taint_index_with_config` or `flowy::check_with_config`:
- the engine tests;
- `codegen/tests.rs`;
- the `ctadl-c` suite (`frontends/ctadl-c/src/test_utils.rs:568`);
- `flowy_tests.rs`;
- `dispatch_models.rs`.

Decided: the mode is opt-in. A plain `cargo test --workspace` runs each test once, which is
staged after Phase 11. CI runs a second step with the variable set, so every push checks R14.
Phase 0's test-suite wall time is the reference for that step's cost.

**Files.**
- `ctadl-ascent/src/index_engine.rs:27-71`: the check mode.
- `ctadl-ascent/src/index_engine_tests.rs:71-182`. `canonical()` and `assert_same_index` gain
  `global_flow` and `intent_pair`. They miss them today, and R1 needs them.
- `.github/workflows/test.yml`: a step
  `CTADL_INDEX_CHECK_STAGED=1 cargo test --workspace`.
- `.gitlab-ci.yml:104`: the same.

**Tests.**
- New, `ctadl-ascent/src/index_engine_tests.rs`: `check_mode_catches_a_difference`. A test-only
  hook drops one staged `summary` row, and the check panics naming `summary`.
- Gate run: `CTADL_INDEX_CHECK_STAGED=1 cargo test --workspace` passes.

**Gate.** fmt, clippy, `cargo test --workspace`, and the check-mode run.

## Phase 9: resume (R12, R13, D8 completion and run key)

Depends on: Phase 7.

**What.** On start, a staged run compares the stored `RunKey` with the new one:
- If they are equal, it skips every batch, round and stage that has its marker, and redoes the
  rest.
- If they differ, or there is no key, it removes `staging/` and starts over.
- Files without a marker are discarded on start.

The stage directory is removed after the stamp is written (D9).

**Files.**
- `ctadl-ascent/src/staged/run.rs`: the resume logic.
- `ctadl-index/src/staged/store.rs`: `discard_unmarked()`.
- A test hook, `CTADL_STAGED_STOP_AFTER=<batch id>`, makes the scheduler exit with a distinct
  status after that batch's marker. It is read only in the scheduler.

**Tests.** New, in `ctadl-ascent/tests/integration/staged_workers.rs`:
- `killed_run_resumes_to_same_tables`: stop after batch 1 and rerun. The tables equal an
  uninterrupted run's, and the log shows batch 0 and batch 1 skipped.
- `changed_config_starts_over`: stop, rerun with `--alias-rule=false`. Nothing is skipped.
- `unmarked_files_are_discarded`: plant a file without a marker; it is not read.

**Gate.** fmt, clippy, `cargo test --workspace`.

## Phase 10: the CLI runs staged (R6 flags, R7, R9, R13, D6 flag, D9, D10)

Depends on: Phases 8, 9.

**What.** Wire the staged run into `cli::index` and `ctadl index`. The default is
still monolithic until Phase 11.

**Files.**
- `ctadl-ascent/src/cli/mod.rs:374-845` (`index`):
  - **Staged run.** When `opts` selects staged:
    - It builds `StagedConfig`.
    - It plans and writes the stage directory under `project.index_path()`.
    - It drops `facts` before stage 1, keeping only the small `static_calls`, `indirect_calls`
      and `declared` that it already clones (`:769-777`).
    - It runs with the `Process` executor (`opts.worker_exe`) and assembles straight into the
      index directory.
  - **Notes.** `push_zero_assignment_notes` (`:270-334`) takes a summarized-function set, the
    resolved calls and the external functions instead of `&IndexResult` (D9). The monolithic
    path derives the same three from its result.
  - **Flowy.** `index_check` reads `IndexResult::try_load(&path)` (D9).
  - **Stamp.** `write_index_config` stays last (`:831`). The stage directory is under
    `index/staging/`, so `index_tables` (`:1514-1534`), which lists only `index/*.parquet`,
    does not count stage files, and `has_index` (`ctadl-import/src/project.rs:934`) stays false
    until the stamp (R13).
- `ctadl-ascent/src/main.rs`:
  - `IndexArgs` (`:347-492`) gains `--monolithic` and `--index-budget <SIZE>`. The budget uses
    `parse_size` (`:511-534`); its default is half of physical memory (D6), from
    `sysctl hw.memsize` or `/proc/meminfo`.
  - `--lsm` gains `requires = "monolithic"`, with an error message that says to add the flag
    (R6). Its doc at `:445-453` and the `-j` doc at `:436-443` change to describe staging.
  - `index_artifacts_to_store` (`:773-815`) passes
    `worker_exe: Some(Box::leak(std::env::current_exe()?.into_boxed_path()))`.
- `ctadl-import/src/project.rs:235-244`. `EngineRecord` gains `#[serde(default)] staged: bool`,
  for traceability. It is additive, so `INDEX_FORMAT_VERSION` stays (`:187`).
- `ctadl-index/src/index_engine/profile.rs`, `mod.rs:394-442`. `IndexStats` and `IndexProfile`
  sum over batches. `profile.json` gains `stages: [{id, peak_mb, rows, secs}]` (D10).
- `ctadl-index/src/index_engine/dump.rs`. Add `dump_outputs(dir, index_dir, id_map)` (D10). It
  is called by `cli::index` after the tables are written, in both modes, when
  `CTADL_DUMP_RELATIONS` is set. Under staged, `run_engine!`'s full dump is suppressed, unless
  `CTADL_DUMP_STAGES` is set, in which case it goes to `stages/<batch>/`.
- `scripts/fact-dump-diff.sh`: `--outputs` compares only `outputs/`. Update the usage text
  (`:3-5`, `:7-20`).
- `docs/debugging.md:171-208`: the examples gain `--monolithic` for the reference and for
  `--lsm`, and describe `--outputs`.
- `ctadl-ascent/src/cli/mod.rs`. `index()` runs the staged check of Phase 8 here too, when
  `CTADL_INDEX_CHECK_STAGED=1`: it reruns monolithic serial into a temporary directory and
  compares with `compare.rs`. That brings the `cli::index` integration tests (`cli.rs`,
  `jni_summary.rs`, `determinism.rs`) under R14.
- Test call sites that build `IndexOptions` with `..Default::default()` get
  `worker_exe: Some(Path::new(env!("CARGO_BIN_EXE_ctadl")))` through a helper in
  `tests/integration/common.rs`. The helper `common::index_options()` replaces bare
  `IndexOptions::default()` at its call sites: `cli.rs:403`, `:829`, `:1125`,
  `determinism.rs:178` and `jni_summary.rs`'s seven calls (`:42-271`).

**Tests.**
- New, `ctadl-ascent/tests/integration/cli.rs`:
  - `lsm_without_monolithic_is_an_error`: the message names `--monolithic`.
  - `staged_index_reads_as_absent_until_stamp`: a run stopped by `CTADL_STAGED_STOP_AFTER` has
    `has_index() == false` and `IndexStatus::Unfinished` (as `cli.rs:1052-1077`).
  - `staged_cli_matches_monolithic`: `cli::index` staged and `--monolithic` give equal tables
    (D10 helper), with equal notifications.
- New, `scripts/` check: a `fact-dump-diff.sh compare --outputs` run of a regression project,
  staged against a `--monolithic` reference, in the gate below.
- Existing: every integration test, unchanged, still monolithic by default.

**Gate.**
- fmt, clippy, `cargo test --workspace`, `CTADL_INDEX_CHECK_STAGED=1 cargo test --workspace`.
- `scripts/fact-dump-diff.sh compare --outputs REF -- ctadl index <project>` with
  `REF` recorded by `ctadl index <project> --monolithic`, for one project per front end.

## Phase 11: staged by default (R6)

Depends on: Phase 10.

**What.** `IndexConfig::default()` (`ctadl-index/src/index_engine/mod.rs:333-342`) and
`IndexOptions::default()` (`ctadl-ascent/src/cli/mod.rs:88-102`) select staged, with the
default budget and floor. `--monolithic` selects the old run. Every existing test now indexes
staged.

Tests that compare engines must name the reference:
- `parallel_engine_matches_serial` and `lsm_storage_matches_memory`
  (`index_engine_tests.rs:109`, `:130`) set `staged: None` on both sides.
- `fixpoint_interns_nothing` (`:221`) adds a staged config to its three. It measures interning
  during the fixpoint, and a batch's re-interning happens before it.

`xtask/src/regression.rs:712-721` (`ctadl_command_with`) gains `--monolithic` when
`XTASK_INDEX_MONOLITHIC=1`, so the suite can run either way.

**Files.** The two defaults; `ctadl-ascent/src/index_engine_tests.rs`;
`xtask/src/regression.rs:712-721`; `README.md` where it describes indexing, if anywhere.

**Tests.**
- Existing: `cargo test --workspace` passes with staged as the default.
  `CTADL_INDEX_CHECK_STAGED=1 cargo test --workspace` passes.
- Regression: `cargo xtask regression` passes staged, with the same outcomes as Phase 0. It is
  also run once with `XTASK_INDEX_MONOLITHIC=1`.

**Gate.** All of the above.

## Phase 12: overall design doc (R16)

Depends on: Phase 11.

**What.** `docs/spec.md`:
- §4.1 step 2 (`:125-129`) and §4.2's Engines bullet (`:133-136`) describe the staged fixpoint:
  stages, batches by SCC under a budget, the global frame and intent stages, and `--monolithic`
  as the reference.
- §4.6 (`:198-203`) describes `index/staging/`, its markers and resumption.

No spec or plan numbers.

**Files.** `docs/spec.md`.

**Tests.** None. Proofread against the code as built.

**Gate.** `cargo test --workspace` (unaffected; doc tests only if a doc is included).

## Phase 13: memory benchmark and calibration (R7, R8, R15, D6)

Depends on: Phase 11.

**What.**
- **Procedure.** Document it in `docs/debugging.md`, in a new section after the dump section.
  It names:
  - the synthetic generator command (Phase 0's);
  - the real target class;
  - `memguard.sh` with a limit above the budget;
  - `--index-budget` set below the monolithic peak.
- **Calibration.** Run it, with `RUST_LOG=debug` and `profile.json`, and fit the planner's
  estimate coefficients to the measured per-batch peaks. Commit the coefficients.
- **Measurements.** These are recorded in the commit message, not in the repository:
  - the monolithic peak (Phase 0);
  - the staged peak, and the scheduler's own peak;
  - the largest batch's peak;
  - the stage-2 and stage-3 peaks;
  - wall time, staged against monolithic;
  - the number of worker runs, and the share of total time spent loading the path set;
  - for each split SCC, the rounds and part runs;
  - the frame's `locals` rows per batch (see Deferred).
- **Resume.** Run the resume check of R15 on the large target: kill the scheduler with
  `CTADL_STAGED_STOP_AFTER` midway, rerun, and compare the tables with
  `fact-dump-diff.sh compare --outputs`.

**Files.** `docs/debugging.md`; `ctadl-index/src/staged/plan.rs` (coefficients).

**Tests.** `cargo test -p ctadl-index` (the plan tests still pass with the new coefficients).

**Gate.** fmt, clippy, `cargo test --workspace`; the documented procedure runs as written.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `ctadl-index/src/index_engine/mod.rs` | `intent_rules`; `IntentInput`/`IntentOutput`; `ProgramPaths`, `EngineInput::from_slice`, `seed_paths`; `IndexConfig.staged`; Linux footprint; stats; default | 1, 2, 5, 7, 10, 11 |
| `ctadl-index/engines/{serial,par,lsm}/src/lib.rs` | include `intent_rules`; serial gains `IntentProg`, `run_intent` | 1 |
| `ctadl-index/src/index_engine/dump.rs` | `DumpCol` skips `staged`; `dump_outputs`; staged dump routing | 5, 10 |
| `ctadl-index/src/index_engine/profile.rs` | per-stage entries | 10 |
| `ctadl-index/src/lib.rs`, `ctadl-index/Cargo.toml` | `pub mod staged`; `sha2` | 3 |
| `ctadl-index/src/staged/{mod,store}.rs` | stage directory, atomic writes, markers, key | 3, 5, 6, 9 |
| `ctadl-index/src/staged/plan.rs` | call graph, SCCs, estimate, packing, split | 4, 6, 7, 13 |
| `ctadl-index/src/staged/shuffle.rs` | seeded shuffle | 4 |
| `ctadl-index/src/facts/parquet.rs` | streaming, filtered reader | 3 |
| `ctadl-index/src/facts/schema.rs` | stage-file schemas | 3 |
| `ctadl-ascent/src/lib.rs` | `pub mod staged` | 5 |
| `ctadl-ascent/src/staged/{mod,run,batch,executor,compare,tests}.rs` | staged run, rounds, executors, comparison | 5, 6, 7, 9, 10 |
| `ctadl-ascent/src/staged/footprint.rs` | worker footprint sampler | 7 |
| `ctadl-ascent/src/index_engine.rs` | dispatch on `staged`; check mode | 5, 8 |
| `ctadl-ascent/src/index_engine_tests.rs` | intent test; `canonical` gains tables; reference configs | 1, 5, 8, 11 |
| `ctadl-ascent/src/cli/mod.rs` | `IndexOptions.worker_exe`; staged `index`; notes from tables | 5, 7, 10, 11 |
| `ctadl-ascent/src/main.rs` | `index-worker`; `--monolithic`, `--index-budget`; `--lsm` requires `--monolithic` | 7, 10 |
| `ctadl-ascent/tests/integration/{main,common,staged_workers,cli,jni_summary,determinism}.rs` | worker tests; `index_options()` helper | 7, 9, 10 |
| `ctadl-import/src/project.rs` | `EngineRecord.staged` | 10 |
| `scripts/fact-dump-diff.sh` | `--outputs` | 10 |
| `xtask/src/regression.rs` | `XTASK_INDEX_MONOLITHIC` | 11 |
| `.github/workflows/test.yml`, `.gitlab-ci.yml` | check-mode test step | 8 |
| `docs/debugging.md` | dump examples; benchmark procedure | 10, 13 |
| `docs/spec.md` | §4.1, §4.2, §4.6 | 12 |

Not touched:
- The rules' meaning (R4).
- Codegen, models, JNI and intent fact generation (`ctadl-ascent/src/codegen/`,
  `languages/`), and `load_and_map_summaries` (`cli/mod.rs:1131`) (R5).
- `INDEX_FORMAT_VERSION` (R3).
- `ctadl-ir/src/graph/scc/`, which is used as is.
- `lsm/` and the LSM engine's storage. The LSM engine only gains an include.

## Acceptance

- `cargo fmt --all -- --check`, `cargo clippy -- -Dwarnings`, and `cargo test --workspace` pass
  with staged as the default.
- `CTADL_INDEX_CHECK_STAGED=1 cargo test --workspace` passes (R1, R2, R14).
- `cargo xtask regression` passes with the same outcomes as Phase 0, staged, and with
  `XTASK_INDEX_MONOLITHIC=1`.
- `scripts/fact-dump-diff.sh compare --outputs` of staged against `--monolithic` is identical
  for one project per front end and for both Phase 0 large targets (R1).
- Phase 13's measurements, against Phase 0 (R7, R15):
  - the staged peak on the large targets tracks the largest batch's peak plus the scheduler's,
    not the monolithic peak;
  - the staged peak stays under `--index-budget`, except for a logged single-function batch;
  - the resumed run's tables equal the uninterrupted run's.
- Greps:
  - `rg -n 'R[0-9]+\b|D[0-9]+\b' --type rust ctadl-ascent ctadl-index` finds no citation of the
    spec in code or help text;
  - `rg -n 'intent_pair|const_reaches' ctadl-index/src/index_engine/mod.rs` finds those rules
    only in `intent_rules`.

## Deferred

- `global_read` is derived in every stage-1 batch, though only `intent_frame` functions use it.
  An `intent_frame(f)` guard would save that memory, but it changes the relation's dump.
- Each stage-1 batch closes its own frame edges in the frame's `locals`
  (`global_mention(GLOBAL_FRAME, _)`, `ctadl-index/src/index_engine/mod.rs:1022`), which feeds
  no output. A config guard would skip that in batches, but it changes a rule's text.
  Decided: Phase 13 measures the frame's `locals` rows per batch first, and a guard comes only
  if the numbers call for one.
