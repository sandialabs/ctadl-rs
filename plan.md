# `ctadl diff-index`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order of the work, the files each step
changes and the tests that prove it. R* and D* numbers are the spec's; where this plan and the
spec disagree, the spec wins, except for the corrections listed below. Each phase builds,
passes the gate named in it, and is one commit.

The bridge rename (R23, D9) is already done, in commit `1314bbdc7`: `temp_for` at
`ctadl-ascent/src/codegen/model_matches.rs:420-424` names a temporary
`$bridge[{file}:{label}]{callee}#{index}`. What remains of it is R24's check, which Phase 0 and
the acceptance run carry.

Code, `--help` text and docs must not cite this plan's or the spec's numbers (D10).

## Corrections to the spec

- P3. **R19 has no duckdb to run.** duckdb is in no Cargo dependency, in neither the dev shell
  nor `testEnv` (`flake.nix:96-138`, `:298`), so R19 as written cannot run in the nightly.
  datafusion 55 is already linked (`ctadl-ascent/Cargo.toml:36`) and reads parquet
  (`ctadl-ascent/src/query_engine/formatter.rs:1725-1730`). The corpus also writes one index
  per case, so R19 does not say which two indexes to compare. "No instruction column" must
  also exclude tables whose flow variables can hold a call argument (`assign`). This plan
  assumes: an xtask regression check, run after the cases like `--compare-paths`
  (`xtask/src/regression.rs:350`), diffs pairs of distinct corpus indexes (each case's index
  against the next one's, by name). For every table with no instruction id anywhere in a row
  (`formal_param`, `callee_resolvents`, `summary`, `globals`, `global_flow`, `paths`,
  `external_function`, `function_id`, `import_id`), it checks the command's per-table counts
  against datafusion SQL: `GROUP BY` with `count(*)` on each side, function ids joined to
  names, then a full outer join on the canonical columns.
- P4. **Tables are not always sets.** The Terms entry "multiplicity" says a canonical row's
  multiplicity is above 1 only when rows collapse under canonicalizing. That is false. The
  input tables are written from `Vec`s with no dedup (`ctadl-index/src/index_engine/mod.rs:160-214`):
  bridges push a `formal_param` row per port (`ctadl-ascent/src/codegen/model_matches.rs:340-345`),
  and `synth_call` appends to `call` (`ctadl-ascent/src/models/codegen.rs:34`). Behavior is
  unchanged, since the diff already compares multisets. Only the Terms text is wrong.
- P5. **D5's memory estimate leaves things out.** `read_vec` grows its `Vec` by doubling and
  then shrinks it (`ctadl-index/src/facts/parquet.rs:164-169`), so a load briefly holds about
  3x the table. Mapping a loaded `Vec<Record>` to `Vec<Canon>` holds both. The signature
  interner and both sides' instruction maps also stay loaded throughout (D3). Peak is
  therefore the largest table pair, plus the maps, plus whatever the load holds transiently.
  This plan assumes: tables load through a streaming map into a presized `Vec<Canon>` (Phase
  3), so the transient term is one batch. D5's estimate should be restated as "largest
  canonical table pair plus the instruction maps".
- P6. **`DiffTable`'s comparisons need a context.** D2's `content_cmp(a, b)` and
  `render(row, out)` take no context. But comparing or printing a `CanonInsn` by content means
  reading its signature from the per-diff `SignatureInterner`. Making that interner global
  would never free it, which D3's "Memory" bullet rules out. This plan assumes:
  `content_cmp(cx, a, b)` and `render(cx, row, out)`, where `cx` is the dump `Cx` extended with
  `&SignatureInterner`. D3's memory bullet should also state the cost: about 16 bytes per
  instruction for each side's map, and about 130 bytes per distinct signature.
- P7. **Background citations.**
  - `canonical()` is at `ctadl-ascent/src/index_engine_tests.rs:73` and `assert_same_index` at
    `:188`; the spec swaps them.
  - `function_id`, `import_id` and `index_source_map` are written by `IndexSourceInfo::try_save`
    (`ctadl-index/src/index_engine/source_info.rs:70-105`), not by the two writers the spec
    names.
  - `external_function` is written twice (`mod.rs:213` and `:700`); the second write wins.
  - The tables `index` does not write are at `schema.rs:149-178` and `:238-260`, not all of
    `:149-258`.
  - The nightly's build step is `.github/workflows/nightly.yml:36`.
  - D9's comment change landed in `temp_for`'s doc (`model_matches.rs:411-418`), not at the
    old `:296-298`.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree: the pre-rename indexes for R24 (no commit) |
| 1 | 1 | the Kani statement of the core's properties, as an expected failure, for review |
| 2 | 2 | the core, which makes the proofs pass, plus proptests |
| 3 | 3, 4 | 3 is the diff library on 2; 4 moves the existing comparers onto 2. Disjoint files |
| 4 | 5 | the command, its tests on real indexes, the SQL check and the docs |

The properties come first, stated alone, so they can be reviewed before any code tries to
meet them. **Stop after Phase 1 for the user to review the harnesses.** Phase 2 fills in the
code they constrain. The library (3) is additive: no caller outside its own tests. Phase 4 is
independent of 3 and changes only test and xtask comparers. Phase 5 is the first that a user
sees.

Shared files:
- `Cargo.lock` changes in Phases 1, 2, 4 and 5. Phase 4 is the only one in wave 3 that
  changes it.
- `xtask/src/regression.rs` and `xtask/Cargo.toml` are edited by Phase 4 (`compare_paths`) and
  Phase 5 (the SQL check), which are in different waves.

Test commands used throughout (`test.yml` runs the first three):

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test --workspace
    cargo test -p ctadl-ascent --test integration <filter>
    cargo xtask regression --frontend <list> [--compare-paths <dir>]
    nix build .#checks.<system>.kani

Put `CARGO_TARGET_DIR` and `TMPDIR` outside the repo, as `.envrc.local` already does. Run long
measurements under a memory cap, and capture their output to files outside the repo.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record the "before" side of R24. The bridge rename has landed, so build `ctadl` at
`1314bbdc7^` in a separate worktree and target directory. Then run the cases that use model
bridges with that binary, and keep their indexes:

    git worktree add <scratch>/pre-rename 1314bbdc7^
    (cd <scratch>/pre-rename && cargo build --release -p ctadl-ascent)
    CTADL_BIN=<scratch>/pre-rename/target/release/ctadl cargo xtask regression --frontend jvm,dex,jni

The run root (`$TMPDIR/ctadl_xtask_<pid>`, `xtask/src/regression.rs:2888`) persists; record
its path. Nothing is committed.

**Gate.** The run finishes with its usual pass/xfail counts, and the run root holds an
`index/` directory per case.

## Phase 1: the core's properties, stated in Kani, as an expected failure (R15, R17, D8)

Depends on: nothing.

**What.** State R15's properties as Kani harnesses against the core's signature, with no
implementation yet, and run them from a pinned Kani in Nix.

- **The crate.** `multiset-diff` declares `Delta` and `multiset_diff` exactly as D1 gives
  them, with the doc comment stating the sorted precondition and what is yielded. The body is
  `todo!()`, followed by an unreachable `std::iter::empty()` so that the `impl Iterator`
  return type has a concrete type (`!` does not implement `Iterator`). The crate has no
  dependencies.
- **The harnesses.** They are in `multiset-diff/src/proofs.rs` under `#[cfg(kani)]`. Each
  works over `u8` values with `Ord::cmp`, on two `kani::any` arrays of length at most a bound
  (start at 6, stated in the module doc) that are assumed sorted:
  - `self_diff_is_empty`: diffing a sequence with itself yields nothing.
  - `swap_swaps_sides`: the deltas of `(b, a)` are those of `(a, b)` with the counts
    exchanged.
  - `reports_exactly_count_differences`: a value is yielded exactly when its counts in `a` and
    `b` differ (counted directly in the harness), and with those counts.
  - `no_value_twice`: the yielded values are strictly increasing.

  Each is marked `#[kani::should_panic]` with a comment saying it is an expected failure until
  the core exists. Against the `todo!()` they panic, so the check passes. That proves the
  statement compiles and runs, not that it holds.
- **Kani in Nix.**
  - `nix/kani.nix` builds Kani as D8 describes, with the version pinned there alone.
  - `flake.nix` adds it to the dev shell and adds `checks.kani`. The check copies
    `multiset-diff/` alone and wraps it in a generated one-member workspace `Cargo.toml`,
    which supplies the inherited `version` (from `workspaceVersion`, `flake.nix:81`) and
    lints. It then runs `cargo generate-lockfile --offline` and `cargo kani`.
  - The nightly builds the check.

**Files.**
- `Cargo.toml:3`: add `multiset-diff` to `members`.
- `Cargo.toml:38`: `unexpected_cfgs` with `check-cfg = ['cfg(kani)']`.
- `multiset-diff/Cargo.toml` (new): `version.workspace = true` and `[lints] workspace = true`.
- `multiset-diff/src/lib.rs` (new): the signature and `#[cfg(kani)] mod proofs;`.
- `multiset-diff/src/proofs.rs` (new): the harnesses.
- `nix/kani.nix` (new).
- `flake.nix:203` (`checks`) and `:298` (dev shell).
- `.github/workflows/nightly.yml:36`: add `'.#checks.x86_64-linux.kani'`.
- `Cargo.lock`.

**Tests.** The four harnesses, each an expected failure.

**Gate.**
- `nix build .#checks.aarch64-darwin.kani` passes, and its log shows four harnesses that
  panicked as expected.
- `nix build .#checks.aarch64-linux.kani`, in a `nixos/nix` container on the
  `analysis-tool-wrangling-linux` podman machine, which exercises `autoPatchelfHook`.
- `cargo build --workspace`, `cargo clippy -- -Dwarnings` and `cargo fmt --all -- --check`.
  Nothing calls the stub.

Then stop for review of `multiset-diff/src/proofs.rs`.

## Phase 2: the multiset core (R15, R16, D1)

Depends on: 1.

**What.** Implement `multiset_diff` as D1 describes: one merge pass that allocates nothing,
with a `debug_assert` that both inputs are sorted. Remove `#[kani::should_panic]` from every
harness, so the proofs now have to hold. Add the same four properties as proptests, with no
bound. `ctadl-index` gains the crate as a dependency and proptest as a dev-dependency; proptest
is already in `Cargo.lock` through `lsm/Cargo.toml:15`. Do not change the harnesses' statements;
if one looks wrong, stop and raise it.

**Files.**
- `multiset-diff/src/lib.rs`: the body, and a `#[cfg(test)]` `fixed_examples` test.
- `multiset-diff/src/proofs.rs`: drop the `should_panic` markers and their comments.
- `ctadl-index/Cargo.toml:6`, `:42`: the dependency and the dev-dependency.
- `ctadl-index/tests/multiset_diff_props.rs` (new).
- `Cargo.lock`.

**Tests.**
- proptest, over sorted `Vec<u8>`: `self_diff_is_empty`, `swap_swaps_sides`,
  `reports_exactly_count_differences` (against a `BTreeMap` count) and `no_value_twice`.
- `fixed_examples`: `[1,1,2]` vs `[1,2,2,3]` yields `(1,2,1)`, `(2,1,2)`, `(3,0,1)`.
- The four Kani harnesses, now required to pass.
- Check that a proof can fail: break the merge temporarily (for example, skip a value missing
  from B), confirm that `checks.kani` fails, and revert. Say in the commit message that this
  was done.

**Gate.** `nix build .#checks.aarch64-darwin.kani` and `.#checks.aarch64-linux.kani` pass with
no expected failures. `cargo test -p multiset-diff -p ctadl-index` and
`cargo clippy -- -Dwarnings` pass.

## Phase 3: the diff library (R4-R14, D2-D6)

Depends on: 2.

**What.** `ctadl_index::diff::diff_index(a, b, &Options) -> Result<Report, Error>` over two
resolved, version-checked index directories, and everything under it. No CLI yet. In the order
an agent would build it:

1. **One table list.** Replace the 22 hand-written modules (`ctadl-index/src/facts/schema.rs:39-260`)
   with one `tables!` invocation.
   - It emits each module as today: `Record`, `COLUMNS`, `FILENAME`, and `save_load!`
     (`schema.rs:22-37`).
   - It also emits `pub enum Table` with `ALL`, `filename()` and `from_filename()`.
   - Module paths are unchanged, so external users compile as is: `cli/mod.rs:2285-2320`,
     `formatter.rs`, `android_intent.rs:194`, and in xtask `regression.rs:378` and `:388`,
     `apk.rs`, `android_icc.rs:91` and `cil/taint.rs:370`.
   - `save_load!` gains `try_load_map(dir, f)`, built on a new
     `Reader::read_map(cols, f: FnMut(T) -> Option<U>)`. It presizes from the file's row count
     and maps each batch straight into the output, so no raw `Vec<Record>` exists (P5).
     `read_vec` stays as is.
2. **Names.**
   - `FunctionNames` maps `FunctionId -> Function` by `function_id.parquet`'s `id` column, not
     by row position (unlike `IdMap::try_load`, `facts.rs:2071-2078`). It names `GLOBAL_FRAME`
     (`facts.rs:971`), which is never in the table, as `IdMap::get_function` does
     (`facts.rs:2087-2093`). A repeated or missing id is an error naming the table and the row.
   - `ImportNames` maps `ImportId -> String`.
3. **Canonical instructions (D3).**
   - **Building.** `InsnNames` reads `call`, `actual_param`, `call_target_assign` and
     `callee_info` one table at a time through `try_load_map`. It does not use
     `IndexFacts::try_load` (`ctadl-index/src/index_engine/mod.rs:219`), which loads them all
     at once. Each row goes, with the row's own instruction as `This`, into one flat
     `Vec<(InsnId, Function, SigRow)>`, which is then `sort_unstable`d and `dedup`ed. That is
     O(n log n) in input rows, with no per-instruction `Vec` or map of `Vec`s.
   - **Interning.** Each run of rows with one instruction is interned as a slice in
     `SignatureInterner`. It is a plain struct, not an `immortal` interner, so it is freed with
     the diff (`immortal/src/id.rs:1-25` never frees): one arena `Vec<SigRow>`, a `Vec<u32>`
     of offsets, and a hash table keyed on the slice. `SignatureId` is a `u32` index into it.
     Both sides share one interner. `CanonInsn` is `(Function, SignatureId)`, 8 bytes.
   - **The map.** The per-side map is a sorted `Vec<(InsnId, CanonInsn)>`, searched by binary
     search. It is not a dense `Vec`, since `InsnId` is 36 bits (`facts.rs:1176-1180`). An
     instruction not in the map has the empty signature. The flat buffer is freed once the
     map is built.
   - **Errors.** A row whose variable is a call argument of a different instruction is an error
     that names the table.
4. **Canonical column types.** None of them can hold a `FunctionId`, `InsnId`, `FileSpanId` or
   `ImportId`. They are plain `Copy` values, never `immortal`-interned: one interned value per
   distinct (instruction, argument) pair would grow without bound and never be freed.
   - `CanonVar`: `Local(Str) | Global(..) | Formal(i16) | CallArg(CanonInsn, i16) | Uninit`.
     It is laid out at 4-byte alignment (12 bytes), so a canonical `assign` row is about 36
     bytes.
   - `CanonTarget`: `Fn(Function) | Obj(CallTargetObject)`. It keeps the existing handle for the
     variants that hold no id.
   - `CanonSite`: the `CanonInsn` (which already holds its function), for the
     `PackedInsnSiteId`s inside `taint.endpoint` (`facts/query_endpoint.rs:24-43`) and
     `taint_edge.edge` (`facts.rs:1731-1740`).

   Each derives `Ord`, by intern id and `SignatureId` (D4), and has its own `content_cmp`. It
   must not reuse `FlowVariable::content_cmp` (`facts.rs:1410`, which orders call arguments by
   packed bits) or `CallTargetObject::content_cmp` (`facts.rs:1562-1567`, which orders by
   function id). Comparing two `SignatureId`s by content needs the interner, so `content_cmp`
   and `render` take a context (P6).
5. **`DiffTable`, once per table (D2).**
   - `load` uses `try_load_map` and applies the `--function` filter inside the map, cached per
     `Function`.
   - `diff_table` dispatches with an exhaustive `match` on `Table`, with no wildcard arm.
   - For each table it `sort_unstable`s both sides and runs `multiset_diff`. While iterating,
     it counts all three kinds of difference and keeps the samples in a bounded max-heap of
     `limit` per side, ordered by `content_cmp`. So the cost is O(D log limit), with no
     content sort of every difference.
   - It frees both sides before the next table.
   - `function_id` and `import_id` are compared by name. `index_source_map` drops the span;
     `globals` maps its import to a name.
   - `manifest_*` are compared as raw rows: their `u32` node ids are positional, and they are
     written to import directories, never an index
     (`ctadl-ascent/src/languages/android_manifest.rs:9-31`, `:70`).
6. **Rendering.** Make `Cx` constructible (`ctadl-index/src/index_engine/dump.rs:31`), with an
   optional `&SignatureInterner`. Make `push_col` public (`dump.rs:233`), and add `DumpCol`
   impls for `Function`, `String` and the canonical types. A `CanonInsn` renders as its
   function and a short form of its signature, for example the callee and the argument count.
7. **Driver and report (D5, D6).**
   - Unknown `*.parquet` files are listed as not compared (R5). A table on one side only is a
     difference (R4).
   - `--table` and `--function` are recorded in the report (R13).
   - The text and JSON printers are pure functions of one `Report` (R9-R11, R14). JSON is
     written with `serde_json::to_writer` to a buffered stdout.
   - Each table load runs inside `catch_unwind` (with `AssertUnwindSafe`), so a decoder panic
     (`parquet.rs:166`, `:434-437`, `:575`) becomes an `Err` naming the table and side, with
     the message from the panic payload.

**Files.**
- `ctadl-index/src/facts/schema.rs`.
- `ctadl-index/src/facts/parquet.rs:153`: add `read_map` beside `read_vec`.
- `ctadl-index/src/index_engine/dump.rs:31`, `:233`.
- `ctadl-index/src/lib.rs:5-7`: `pub mod diff;`.
- `ctadl-index/src/diff/` (new): `mod.rs`, `names.rs`, `insn.rs`, `canon.rs`, `table.rs`,
  `driver.rs` and `report.rs`.

**Tests.** All in `ctadl-index`, over hand-built tables written with the schema's `try_save`
into `tempfile` directories.
- **Schema:** the existing round trips (`schema.rs:262-520`) pass unchanged.
  `table_filenames_unique`: 22 tables. `from_filename_round_trips`, where
  `update_sans_path.parquet` gives `None`.
- **`read_map`:** it returns the same rows as `read_vec`, and its output capacity equals the
  row count.
- **Instructions:**
  - `equal_facts_share_a_name`;
  - `different_arguments_differ`;
  - `renumbered_instructions_keep_names`;
  - `renumbered_functions_keep_names`, with `function_id.parquet` written in shuffled order;
  - `two_sides_share_signatures`, building in either order;
  - `second_instruction_in_input_is_an_error`;
  - `instruction_with_no_facts_has_empty_signature`;
  - `global_frame_is_named`;
  - `duplicate_function_id_is_an_error`.
- **Tables:**
  - `every_table_shuffled_is_identical`, for each `Table`;
  - `multiplicity_is_reported`: `call` with a row twice in A and once in B gives `(2, 1)`;
  - `call_arg_inside_assign_is_canonical`;
  - `function_target_inside_cto_is_canonical`;
  - `span_ids_dropped`;
  - `sample_limit_and_remainder`: 30 rows only in A, `limit 20`, gives the first 20 by content
    and 10 left out;
  - `all_rows_differ`: every row of a 10k-row table differs; the counts are exact and the
    samples are the first N by content.
- **Driver:**
  - `identical_first_line`;
  - `unknown_parquet_listed`, which alone does not make the result `DIFFERENT`;
  - `table_on_one_side`;
  - `swap_is_mirror` (text and JSON);
  - `output_is_deterministic`;
  - `samples_independent_of_load_order`;
  - `table_filter_named`, `function_filter_named`;
  - `json_matches_text`;
  - `corrupt_table_is_an_error`: a truncated `call.parquet` gives `Err` naming `call`.

**Gate.** `cargo test -p ctadl-index`, `cargo build --workspace`, `cargo clippy -- -Dwarnings`.

## Phase 4: the existing comparers use the core (R20, D7)

Depends on: 2.

**What.** What passes does not change; only the failure messages change.

- **`assert_same_index`** (`ctadl-ascent/src/index_engine_tests.rs:188-215`) keeps its
  `IndexStats` checks.
  - It replaces `canonical()` (`:73-111`) with one `multiset_diff` per `IndexResult` field,
    including `const_reaches`, `intent_pair` and `global_flow`.
  - It destructures `IndexResult` (`ctadl-index/src/index_engine/mod.rs:651-664`) with no `..`,
    so a new field fails to compile.
  - On a difference it fails with the field name and the first deltas, with their counts.
  - Both runs share one process and one set of facts, so raw ids are comparable here; its doc
    says so.
  - `context_joins_agree` (`:221`, which calls `canonical()` at `:237` and `:259`) uses the
    same helper.
- **`compare_paths`** (`xtask/src/regression.rs:365-422`) loads into sorted `Vec`s instead of
  `BTreeSet`s and prints the first 8 deltas per side. It keeps no format check, since it
  compares against older runs.

**Files.**
- `ctadl-ascent/src/index_engine_tests.rs`.
- `ctadl-ascent/Cargo.toml:80`: dev-dependency `multiset-diff`.
- `xtask/src/regression.rs:365-422`.
- `xtask/Cargo.toml:12`: the `multiset-diff` dependency.
- `Cargo.lock`.

**Tests.**
- Existing, unchanged: `parallel_engine_matches_serial` (`:115`), `lsm_storage_matches_memory`
  (`:146`), `context_joins_agree` (`:221`).
- New: `assert_same_index_reports_const_reaches` (`#[should_panic(expected = "const_reaches")]`),
  which removes one `const_reaches` row from a copy.
- New, in xtask: `compare_paths_reports_multiset_difference`, over two run roots whose
  `paths.parquet` differ by one path.

**Gate.** `cargo test -p ctadl-ascent --lib index_engine_tests`, `cargo test -p xtask`,
`cargo clippy -- -Dwarnings`.

## Phase 5: `ctadl diff-index`, tested on real indexes, and documented (R1-R3, R12, R18, R19, R21)

Depends on: 3.

**What.**
- **The command.**
  - `Command::DiffIndex` (`ctadl-ascent/src/main.rs:31-73`) takes `A`, `B`, `--limit N`
    (default 20), `--table` (repeatable), `--function` and `--json`.
  - Resolution (D5): an existing directory is used as is; otherwise
    `AnalysisProject::try_load_name(..).dir().join("index")`, as at `cli/mod.rs:2001`. It does
    not use `index_path()`, which creates the directory (`ctadl-import/src/project.rs:869-875`).
  - Each side's `IndexConfig` (`project.rs:211`) must have version `INDEX_FORMAT_VERSION`.
  - `main` returns `anyhow::Result` (`main.rs:672`), so errors exit 1 today. The arm therefore
    prints the report, flushes, and calls `std::process::exit` with 0, 1 or 2; any error goes
    to stderr with exit 2.
- **Metamorphic tests (R18).**
  - They run over real indexes from `cli::index`, as `determinism.rs:172` does: the C program
    at `index_engine_tests.rs:56-68`, and a bridging fixture from `bridging_end_to_end.rs`.
  - The perturbations are test helpers that `try_load`, rewrite and `try_save` each table.
  - Intern order: `determinism.rs`'s `forward` and `reverse` children
    (`determinism.rs:225-245`), which pre-intern in opposite orders (`pre_intern`, `:117`),
    have their two `index/` directories diffed by the parent.
- **SQL agreement (R19 as P3 corrects it).** `xtask/src/diff_sql.rs` runs after the cases, as
  `compare_paths` does (`regression.rs:350`).
  - It pairs consecutive corpus indexes by name.
  - For P3's tables, it checks `ctadl diff-index --json --table t` against datafusion
    `GROUP BY` counts with a full outer join, in both directions.
  - A disagreement fails the run. With fewer than two indexes, it reports `SKIP`.
- **Docs (R21).**
  - `docs/debugging.md`:
    - It gains "Comparing two index runs", which says to use `ctadl diff-index` and not a
      script.
    - The duckdb recipe (`:178-185`) stays, with a note that it compares raw ids as sets.
    - `:129-133` stops calling instruction ids byte-stable across indexes. It also says that
      instructions with no input facts (Android intent bridges) share one name per function.
  - `docs/spec.md` F12 (`:88-89`) names the command, and §5 (`:233-240`) adds the Kani check
    and the property and metamorphic tests.
  - `nightly/README.md` lists `checks.kani`.
  - No doc, help text or code cites spec numbers.

**Files.**
- `ctadl-ascent/src/main.rs:31-73`, `:683-799` (dispatch), `:1301` (parse tests).
- `ctadl-ascent/src/cli/diff_index.rs` (new) and `ctadl-ascent/src/cli/mod.rs`.
- `ctadl-ascent/tests/integration/cli.rs`.
- `ctadl-ascent/tests/integration/diff_index.rs` (new).
- `ctadl-ascent/tests/integration/main.rs:4-27`.
- `ctadl-ascent/tests/integration/determinism.rs`.
- `xtask/src/diff_sql.rs` (new), `xtask/src/main.rs` and `xtask/src/regression.rs:350`.
- `xtask/Cargo.toml`: `datafusion = "55"`, already built for `ctadl-ascent`
  (`Cargo.lock:1451`).
- `docs/debugging.md`, `docs/spec.md` and `nightly/README.md`.
- `Cargo.lock`.

**Tests.**
- **Parsing:** `diff_index_parses`, in `main.rs`'s test module.
- **Command,** in `cli.rs` beside the version-gate tests (`:1331-1389`):
  - `same_project_is_identical`, by name and by path, exits 0;
  - `refuses_old_format`: one side at `"7"` exits 2, and two `"7"` sides also exit 2;
  - `refuses_missing_config`: exits 2 and says why;
  - `unreadable_table`: exits 2.
- **Metamorphic,** in `diff_index.rs`; each reports `IDENTICAL` unless stated otherwise:
  - `shuffled_rows`, including `function_id`;
  - `split_row_groups`;
  - `renumbered_functions`, inside `CallTargetObject`, `QueryEndpoint`, `FlowEdge` and
    `PackedInsnSiteId` too;
  - `renumbered_insns_and_spans`;
  - `deleting_a_row_reports_it`: one row per table, with the input tables checked as R18
    says;
  - `swapped_call_targets_reported`: in `wrap`, the `apply(twice, b)` and `through(&o, r)`
    sites. The two original `call` rows are only in A, and the two swapped rows only in B;
  - `swap_sides_mirror`.
- **Intern order:** `output_does_not_depend_on_intern_order` also asserts `IDENTICAL` between
  its two indexes.
- **SQL:** `sql_counts_match_known_tables` in `diff_sql.rs`; then
  `cargo xtask regression --frontend lua,c` passes with the check.

**Gate.** `cargo test --workspace`, `cargo test -p ctadl-ascent --test integration`,
`cargo xtask regression --frontend lua,c`, `cargo clippy -- -Dwarnings`, and
`ctadl diff-index --help` by hand.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `Cargo.toml` | member `multiset-diff`; `cfg(kani)` check-cfg | 1 |
| `Cargo.lock` | new crate, proptest, datafusion for xtask | 1, 2, 4, 5 |
| `multiset-diff/src/lib.rs` | signature and stub; then the merge | 1, 2 |
| `multiset-diff/src/proofs.rs` | harnesses as expected failures; then required | 1, 2 |
| `nix/kani.nix`, `flake.nix`, `.github/workflows/nightly.yml` | pinned Kani, `checks.kani` | 1 |
| `ctadl-index/Cargo.toml`, `ctadl-index/tests/multiset_diff_props.rs` | core dependency, proptests | 2 |
| `ctadl-index/src/facts/schema.rs` | `tables!`, `Table`, `try_load_map` | 3 |
| `ctadl-index/src/facts/parquet.rs` | `Reader::read_map` | 3 |
| `ctadl-index/src/index_engine/dump.rs` | `Cx::new`, `pub push_col` | 3 |
| `ctadl-index/src/lib.rs`, `ctadl-index/src/diff/` | the diff library | 3 |
| `ctadl-ascent/src/index_engine_tests.rs`, `ctadl-ascent/Cargo.toml` | `assert_same_index` on the core | 4 |
| `xtask/src/regression.rs`, `xtask/Cargo.toml` | `compare_paths` on the core; the SQL check | 4, 5 |
| `ctadl-ascent/src/main.rs`, `ctadl-ascent/src/cli/{mod,diff_index}.rs` | the command | 5 |
| `ctadl-ascent/tests/integration/{cli,diff_index,main,determinism}.rs` | command and metamorphic tests | 5 |
| `xtask/src/{diff_sql,main}.rs` | SQL agreement | 5 |
| `docs/debugging.md`, `docs/spec.md`, `nightly/README.md` | docs | 5 |

Not touched:
- `ctadl-import/src/project.rs`: `INDEX_FORMAT_VERSION` and the on-disk format do not change
  (R22).
- `ctadl-index/src/index_engine/mod.rs`: the writers do not change.
- `scripts/fact-dump-diff.sh` and the `CTADL_DUMP_RELATIONS` dump (R22).
- `ctadl-ascent/src/codegen/model_matches.rs`: the rename is done.

## Acceptance

- `cargo fmt --all -- --check`, `cargo clippy -- -Dwarnings` and `cargo test --workspace`
  pass.
- `nix build .#checks.aarch64-darwin.kani` and `.#checks.aarch64-linux.kani` pass.
- `cargo xtask regression`, then `nix build .#checks.aarch64-darwin.regression`, pass,
  including the SQL check.
- **R24.**
  - Rerun `--frontend jvm,dex,jni` with the new `ctadl`.
  - Run `ctadl diff-index` on each case's index from Phase 0 and from the new run.
  - Every difference must be a row naming a `$bridge` local, paired one-to-one with a row that
    differs only in that local's name.
  - Capture the output outside the repo.
- **Memory (D5, P5).**
  - Under a memory cap, run `ctadl diff-index` on the largest corpus index (the `apk:*` index
    of `xtask/tests/dex/com.noto_54.apk`) against a copy of it.
  - Record peak physical footprint, the largest table's row count and wall time.
  - Peak should be about two copies of the largest canonical table plus the instruction maps.
    A peak near the sum of all tables means a pair was not freed.
- **Greps.** Each of these finds nothing:
  - `rg 'fn canonical\(' ctadl-ascent/src/index_engine_tests.rs`;
  - `rg '\.difference\(' xtask/src/regression.rs`;
  - `rg '\$bridge\{' ctadl-ascent`;
  - `rg '\b[RDP][0-9]+\b' ctadl-index/src/diff ctadl-ascent/src/cli/diff_index.rs docs`.

## Deferred

- Generate `inspect_parquet`'s `match_schema!` list (`ctadl-ascent/src/cli/mod.rs:2292-2320`)
  from `Table::ALL`. It already lacks `resolved_call`, `globals`, `global_flow`, `taint_edge`,
  `intent_pair` and `manifest_*`.
- Make `parquet::Reader` return errors instead of panicking, so that `catch_unwind` is no longer
  needed.
- Stop leaking a `String` per batch for each `FlowVariable` column
  (`ctadl-index/src/facts/parquet.rs:1402`); it is bounded, about 1 MB per 30M-row table.
- Diffing `CTADL_DUMP_RELATIONS` dumps on the same core (out of scope in the intent).
