# `ctadl diff-index`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order of the work, the files each step
changes and the tests that prove it. R* and D* numbers are the spec's; where this plan and the
spec disagree, the spec wins. Each phase builds,
passes the gate named in it, and is one commit.

The bridge rename (R23, D9) is already done, in commit `1314bbdc7`: `temp_for` at
`ctadl-ascent/src/codegen/model_matches.rs:420-424` names a temporary
`$bridge[{file}:{label}]{callee}#{index}`. What remains of it is R24's check, which Phase 0 and
the acceptance run carry.

Code, `--help` text and docs must not cite this plan's or the spec's numbers (D10).

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
- `Cargo.lock` changes in Phases 1, 2 and 4. Phase 4 is the only one in wave 3 that
  changes it.
- `flake.nix` is edited by Phase 1 (Kani, replaced by Verus in Phase 2), Phase 2 (Verus) and
  Phase 5 (duckdb), which are in different waves.
- `xtask/src/regression.rs` is edited by Phase 4 (`compare_paths`) and Phase 5 (the SQL
  check), which are in different waves.

Test commands used throughout (`test.yml` runs the first three):

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test --workspace
    cargo test -p ctadl-ascent --test integration <filter>
    cargo xtask regression --frontend <list> [--compare-paths <dir>]
    nix build .#checks.<system>.verus

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

**As built.**
- The pre-rename binary is `/Volumes/Shampoo/cargo-target/ctadl-pre-rename/release/ctadl`
  (built from `6587da638` = `1314bbdc7^`, worktree kept at
  `/Volumes/Shampoo/tmp/ctadl-index-diff/phase0/pre-rename`).
- Run root: `/Volumes/Shampoo/tmp/ctadl-index-diff/nix-shell.1srVhU/ctadl_xtask_34330`.
  `nix develop` sets its own `TMPDIR` inside the exported one, so the run root lands under a
  `nix-shell.*` directory. Result: 156 passed, 1 xfail (`Jvm:ClassIdMergedTryFlow`), 132
  `index/` directories.
- No index in the run has a `$bridge` temporary. The suite's only bridge models
  (`nightly/tests/jni/jni-flow.bridge.ctadl`, `jni-arg-shift.bridge.ctadl`) use path-less
  ports, which collapse to a direct `actual_param` (`model_matches.rs:319`, `:351`) and never
  call `temp_for`. So these indexes alone make R24 hold trivially. The acceptance check for R24
  therefore also runs a bridge model with sub-path ports (for example
  `F::arg(0).f <-> G::arg(2)`) through both binaries.

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

**As built.**
- D8's version facts hold for the downloaded bundles: Kani 0.68.0's `rust-toolchain-version`
  is `nightly-2026-08-21-<target>` and its `rustc-version` is
  `rustc 1.100.0-nightly (8925ea358 2026-08-20)`, which the fenix nightly's `rustc --version`
  matches. `nix/kani.nix` checks both at build time.
- goto-cc preprocesses by running `gcc`, which the macOS stdenv does not provide by that name
  (outside Nix, Xcode's `/usr/bin/gcc` does). The wrappers therefore also prepend
  `$out/libexec/kani`, which holds `gcc` linked to `stdenv.cc`'s `cc`. Beyond that they do
  what `kani-verifier`'s `exec` does, except that they do not filter rustup toolchain paths
  out of `LD_LIBRARY_PATH`/`DYLD_FALLBACK_LIBRARY_PATH` (nothing under Nix sets them).
- The library check runs `kani-compiler` under the loader's own tracing
  (`DYLD_PRINT_LIBRARIES` on macOS, `LD_TRACE_LOADED_OBJECTS` on Linux) and requires the
  loaded `librustc_driver-*` and `libLLVM` to be the toolchain's. The bundle's binaries are
  not stripped (`dontStrip`).
- On aarch64 Linux, cbmc, goto-cc, goto-instrument and goto-analyzer are static;
  autoPatchelfHook patches the others (kani-driver, kani-compiler, kani-cov, kissat).
- The check runs `cargo kani --output-format terse`, to keep the nightly log short. The
  generated workspace takes its lints from the root `Cargo.toml` (read with `fromTOML`), so
  they cannot drift; `cargo` there is `rustToolchain`'s, which only dispatches to
  `cargo-kani`.
- The harnesses use `#[kani::unwind(13)]` (`2 * N + 1` for `N` = 6), and record what the
  core yields in a fixed array of `2 * N` entries, so an implementation that yields more, or
  loops past the bound, fails rather than passing unchecked.
- Kani was replaced by Verus in Phase 2, at the user's decision, after the Kani check of the
  implemented core ran 2h53m at bound 6 without finishing (two of the four harnesses had
  verified, at 17 and 79 minutes). The harnesses, `nix/kani.nix`, `checks.kani` and the
  `cfg(kani)` check-cfg are gone; Phase 2's as-built notes say what replaced them.

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

**As built.**
- Verus replaced Kani, at the user's decision. The four properties are proved for inputs of
  any length, not up to a bound: `multiset-diff/verus/multiset_diff.rs` holds a copy of the
  merge whose `next` is proved to yield exactly a spec-level merge, for any `T` and any `cmp`
  that computes a total order passed as a ghost argument (`multiset_diff_by`); the four
  properties are theorems about that spec, restated for `u8` under `Ord::cmp` with `==`.
  Verus checks the copy because the crate cannot depend on `vstd`;
  `multiset-diff/verus/same_code.py` fails `checks.verus` unless the copy's `next`, less what
  Verus adds, is the crate's token for token. The proof was written on a separate spike
  (`2771a45ad`) and brought in here.
- `nix/verus.nix` pins Verus `0.2026.11.02.8bf68ee` from its release bundle, with the Rust
  1.99.1 it was built against from `fenix` and its bundled Z3 4.16.0, run with
  `VERUS_USE_RUSTUP=0`. `flake.nix` adds `checks.verus` (the token check, with nixpkgs'
  Python, then `verus`; nothing fetched) and `verus` in the dev shell; the nightly builds
  `checks.x86_64-linux.verus`. Removed: `multiset-diff/src/proofs.rs`, `nix/kani.nix`,
  `checks.kani`, Kani in the dev shell, the `rootManifest` binding only the Kani check used,
  and the `cfg(kani)` check-cfg in `Cargo.toml`. spec.md (R15, R17, D1, D8) and intent.md
  were updated to match.
- The merge is the spike's: for each value, it measures the run on each side with a loop of
  its own and yields a `Delta` when the lengths differ. Under Kani that nesting was the
  problem (an element-per-step merge was tried and was faster, but still too slow at bound
  6); Verus does not care, and the crate keeps the shape the proof was written for. The
  `debug_assert`s use `is_sorted_by`, which allocates nothing.
- `checks.aarch64-darwin.verus`: 34 verified, 0 errors, in about 7 seconds.
- The proptests draw inputs of up to 255 elements, half from `0..8` so that repeats and
  shared values are common; "no bound" means no bound on length like a model checker's, but
  proptest needs a size range.
- No aarch64-linux check was run, at the user's request.
- The failed-proof check made both the crate and the Verus copy skip a value whose run in `b`
  is empty (`count_a != count_b && count_b != 0`). The token check passed, as the two still
  agreed, and Verus failed: `invariant not satisfied at end of loop body`, on the invariant
  that what remains to be yielded is `diff_from(...)` (33 verified, 1 error), so
  `checks.aarch64-darwin.verus` failed. The derivation's inputs were checked to hold the
  break; both files were then reverted.

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
     `read_vec` stays as is.
2. **Names.**
   - `FunctionNames` maps `FunctionId -> Function` by `function_id.parquet`'s `id` column, not
     by row position (unlike `IdMap::try_load`, `facts.rs:2071-2078`). It names `GLOBAL_FRAME`
     (`facts.rs:971`), which is never in the table, as `IdMap::get_function` does
     (`facts.rs:2087-2093`). A repeated or missing id is an error naming the table and the row.
   - `ImportNames` maps `ImportId -> String`.
3. **Canonical instructions (D3).**
   - **Building.** `InsnNames` reads `call`, `actual_param`, `call_target_assign` and
     `callee_info` one table at a time through each module's `try_load`, dropping each before the next. It does not use
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
   and `render` take a context (D2).
5. **`DiffTable`, once per table (D2).**
   - `load` calls `try_load` and consumes the result with `into_iter().filter_map(..)`, which
     maps to `Canon` and applies the `--function` filter, cached per
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
- `ctadl-index/src/index_engine/dump.rs:31`, `:233`.
- `ctadl-index/src/lib.rs:5-7`: `pub mod diff;`.
- `ctadl-index/src/diff/` (new): `mod.rs`, `names.rs`, `insn.rs`, `canon.rs`, `table.rs`,
  `driver.rs` and `report.rs`.

**Tests.** All in `ctadl-index`, over hand-built tables written with the schema's `try_save`
into `tempfile` directories.
- **Schema:** the existing round trips (`schema.rs:262-520`) pass unchanged.
  `table_filenames_unique`: 22 tables. `from_filename_round_trips`, where
  `update_sans_path.parquet` gives `None`.
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

**As built.**
- `tables!` derives each module's `FILENAME` from its name (`concat!`), so the file names are
  unchanged and cannot disagree with `Table`. `Table` also has `name()` and `from_name()`, which
  `--table` uses (it takes `call` or `call.parquet`).
- Canonical rows name a function by `FnName(Str)`, not `Function`, which is not `Copy`; the new
  `DumpCol` impls are for `FnName`, `String`, `u32`, `TaintState` and the canonical types. Import
  names are `Str` too.
- `DiffTable` is per row: `canon(&LoadCx, &Record) -> Canon` and `function(&Canon)`, with
  `const TABLE: Table` in place of `NAME`. Loading, filtering, `content_cmp` and `render` are
  generic: `Canon` is a tuple of columns that implement `ContentOrd` (the content order, with the
  interner in `Cx`) and `DumpCol`, and a row prints with the dump's `DumpRow`, columns separated
  by tabs. So `push_col` stays private. `DiffTable::INSNS` says whether a table's rows can name an
  instruction; with `--table` naming none that do, the instruction maps are not built. The impls
  are generated by one `diff_table!` macro, one invocation per table; `diff_one` and
  `needs_insns` match on `Table` with no wildcard.
- A signature is stored sorted by content, so two are equal exactly when their slices are, and
  their content order is a row-by-row comparison. The interner's offsets are `usize`.
- The samples are kept in a buffer of at most `2 * limit`, cut back to the first `limit` by
  content with `select_nth_unstable_by` when it fills, instead of a max-heap: the same bound,
  O(D) amortized plus a final sort of `limit` rows.
- A differing row is sampled on the side that holds it more often ("more in A", "more in B"),
  with both counts. A table on one side only is compared against no rows, so its rows count as
  only on that side, and it differs even when it is empty. Row counts are after `--function`;
  a row about no function (`paths`, `globals`, `global_flow`, `import_id`, the `manifest_*`) is
  left out by it. `callee_resolvents` is filtered by its target, `taint_edge` by its source
  function.
- An instruction that the input tables put in two functions is an error, like a row naming a
  second instruction.
- The error type is `ctadl_index::diff::Error`, a message naming the table, the side and, for a
  row that cannot be made canonical, the row.
- Text output: the first line is `IDENTICAL` or `DIFFERENT: N of M tables differ: t1, t2`; then
  `A:`/`B:` paths, the filters, files not compared, and a line per table (`call: same, 2 rows`,
  or the counts and the samples). JSON is the `Report` with `result` and `tables_differ` first.

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

**As built.**
- `assert_same_index` checks the stats and calls `assert_same_rows`, which destructures one
  `IndexResult` with no `..` and hands each field, with the other result's, to
  `assert_same_relation`. That sorts both, runs `multiset_diff` under `Ord::cmp`, and panics
  with `<relation> differs (<n> vs <m> rows)` and the first 8 deltas as `row: count_a vs
  count_b`, then how many more. `context_joins_agree` calls `assert_same_rows` with the join's
  name as a prefix, so its stats checks stay its own (it checks a different set of them).
- The `const_reaches` fixture is built with `FunctionBuilder`, not C: the C front end emits no
  string constants, and `const_reaches` is derived only in an intent frame, so the test adds
  the function to `intent_frame` itself. Its guard that the fixture derived rows avoids the
  word `const_reaches` in its message, which the expected panic would otherwise match.
- `compare_paths` deduplicates each side after sorting. Parsing normalizes, so two spellings
  in an older run can load as one path; deduplicating keeps the comparison one of sets, so
  what passed still passes. Its test also checks that a path stored twice compares equal.

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
- **SQL agreement (R19).** `xtask/src/diff_sql.rs` runs after the cases, as `compare_paths`
  does (`regression.rs:350`).
  - It pairs consecutive corpus indexes by name.
  - For R19's tables, it checks `ctadl diff-index --json --table t` against SQL that it runs
    through the `duckdb` CLI (`duckdb -json -c ...` over the parquet files), in both
    directions.
  - `pkgs.duckdb` is added to `testEnv` (`flake.nix:96-138`) and to the dev shell
    (`flake.nix:298`).
  - A disagreement fails the run. With fewer than two indexes, or with no `duckdb` on `PATH`
    (as the SARIF check self-skips without `checksarif`), it reports `SKIP` with the reason.
- **Docs (R21).**
  - `docs/debugging.md`:
    - It gains "Comparing two index runs", which says to use `ctadl diff-index` and not a
      script.
    - The duckdb recipe (`:178-185`) stays, with a note that it compares raw ids as sets.
    - `:129-133` stops calling instruction ids byte-stable across indexes. It also says that
      instructions with no input facts share one name per function. Android intent bridges
      (`android_intent.rs:157`) are the common case: a receiver swap between two sends in
      one function is invisible to the diff. This was decided; the spec's D3 accepts it.
  - `docs/spec.md` F12 (`:88-89`) names the command, and §5 (`:233-240`) adds the Verus check
    and the property and metamorphic tests.
  - `nightly/README.md` lists `checks.verus`.
  - No doc, help text or code cites spec numbers.

**Files.**
- `ctadl-ascent/src/main.rs:31-73`, `:683-799` (dispatch), `:1301` (parse tests).
- `ctadl-ascent/src/cli/diff_index.rs` (new) and `ctadl-ascent/src/cli/mod.rs`.
- `ctadl-ascent/tests/integration/cli.rs`.
- `ctadl-ascent/tests/integration/diff_index.rs` (new).
- `ctadl-ascent/tests/integration/main.rs:4-27`.
- `ctadl-ascent/tests/integration/determinism.rs`.
- `xtask/src/diff_sql.rs` (new), `xtask/src/main.rs` and `xtask/src/regression.rs:350`.
- `flake.nix:96-138` (`testEnv`) and `:298` (dev shell): `duckdb`.
- `docs/debugging.md`, `docs/spec.md` and `nightly/README.md`.

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

**As built.**
- `cli::diff_index` returns `anyhow::Result`: `resolve_index` (a directory, else the project's
  `index/`, never created), `check_index_format` (reads `index_config.json` itself, so it
  works on a bare directory, and names both formats) and `diff_indexes`. `main` prints
  `Error: {e:#}` and exits 2; a failed write of the report (a closed pipe) is also exit 2.
- The command tests run the built binary (`CARGO_BIN_EXE_ctadl`, with `--store` set to the
  test store) on one project indexed once from `xfer.c`, and on copies of its index.
- The C fixture is a copy of `index_engine_tests.rs`'s program, not shared with it. The
  bridge fixture is the pathful bridge of `bridging_end_to_end.rs` (two Lua imports); it
  asserts that `actual_param` passes a `$bridge` temporary.
- The perturbations are one macro over every table `index` writes plus `taint` and
  `taint_edge`: a row rewrite through a `Remap` (function, instruction, span and import ids)
  and a row selection (shuffle, delete). `split_row_groups` rewrites each file through arrow
  with one row per row group. `index` writes no `taint` or `taint_edge`, so the renumbering
  tests add a row of each per call site, which puts a `QueryEndpoint`, a `FlowEdge` and the
  `PackedInsnSiteId`s inside them under the renumbering. `renumbered_insns_and_spans` also
  renumbers import ids.
- Renumbered instruction ids stay below 2^28: `FlowVariable::call_actual` panics ("CallArg
  top bits must be 0") for an instruction id with bit 30 or 31 set, though `1 << 32` packs.
  Real ids are far smaller; the packing bug is left alone here.
- `deleting_a_row_reports_it` deletes row 0 of every non-empty table of both fixtures. For an
  input table it checks that the table differs, that every differing row in every table prints
  the edited instruction's function as `caller@`, and that no table has more differing rows on
  a side than rows naming the instruction there. Deleting a `function_id` or `import_id` row
  that another table names is an error ("is not in"), and the test expects it; one nothing
  names is checked like any other row.
- `swap_sides_mirror` checks that the report of B against A, with its sides swapped back, is
  equal to the report of A against B, as a value and as text.
- The SQL check runs one `ctadl diff-index --json --limit 0` per pair and direction with
  every checked table as a `--table`, and one duckdb query per pair and direction with a row
  per table, instead of one of each per table. Indexes are the directories under the run root
  that hold `index_config.json`, sorted by path. A table missing on either side of a pair is
  left out of that pair; a pair without `function_id` and `import_id` on both sides is
  skipped with a `SKIP` line. `<globals>` is joined in for the reserved global-frame id.
- `duckdb` is added to `testEnv` only: the dev shell has `packages = [ testEnv ]`, so it gets
  it from there.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `Cargo.toml` | member `multiset-diff`; `cfg(kani)` check-cfg, then removed | 1, 2 |
| `Cargo.lock` | new crate, proptest | 1, 2, 4 |
| `multiset-diff/src/lib.rs` | signature and stub; then the merge | 1, 2 |
| `multiset-diff/src/proofs.rs` | Kani harnesses as expected failures; then removed | 1, 2 |
| `multiset-diff/verus/{multiset_diff.rs,same_code.py}` | the Verus proof and the token check | 2 |
| `nix/kani.nix`, `flake.nix`, `.github/workflows/nightly.yml` | pinned Kani, `checks.kani`; then removed | 1, 2 |
| `nix/verus.nix`, `flake.nix`, `.github/workflows/nightly.yml` | pinned Verus, `checks.verus` | 2 |
| `flake.nix` | `duckdb` in `testEnv` and the dev shell | 5 |
| `ctadl-index/Cargo.toml`, `ctadl-index/tests/multiset_diff_props.rs` | core dependency, proptests | 2 |
| `ctadl-index/src/facts/schema.rs` | `tables!`, `Table` | 3 |
| `ctadl-index/src/index_engine/dump.rs` | `Cx::new`, `pub push_col` | 3 |
| `ctadl-index/src/lib.rs`, `ctadl-index/src/diff/` | the diff library | 3 |
| `ctadl-ascent/src/index_engine_tests.rs`, `ctadl-ascent/Cargo.toml` | `assert_same_index` on the core | 4 |
| `xtask/src/regression.rs` | `compare_paths` on the core; the SQL check | 4, 5 |
| `xtask/Cargo.toml` | `multiset-diff` | 4 |
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
- `nix build .#checks.aarch64-darwin.verus` passes. No aarch64-linux builds: the user does not
  want Linux runs now. The nightly still builds `checks.x86_64-linux.verus`.
- `cargo xtask regression`, then `nix build .#checks.aarch64-darwin.regression`, pass,
  including the SQL check.
- **R24.**
  - Rerun `--frontend jvm,dex,jni` with the new `ctadl`.
  - Run `ctadl diff-index` on each case's index from Phase 0 and from the new run.
  - Every difference must be a row naming a `$bridge` local, paired one-to-one with a row that
    differs only in that local's name.
  - Capture the output outside the repo.
  - Also run a JNI case with a bridge model whose ports have sub-paths through both the
    pre-rename binary and the new one, and diff those indexes the same way, so that at least
    one comparison has `$bridge` rows (see Phase 0's as-built note).
- **Memory (D5).**
  - Under a memory cap, run `ctadl diff-index` on the largest corpus index (the `apk:*` index
    of `xtask/tests/dex/com.noto_54.apk`) against a copy of it.
  - Record peak physical footprint, the largest table's row count and wall time.
  - Peak should match D5's estimate: the largest table's canonical rows on both sides, its raw
    rows briefly, and the instruction maps. A peak near the sum of all tables means a pair was
    not freed.
- **Greps.** Each of these finds nothing:
  - `rg 'fn canonical\(' ctadl-ascent/src/index_engine_tests.rs`;
  - `rg '\.difference\(' xtask/src/regression.rs`;
  - `rg '\$bridge\{' ctadl-ascent`;
  - `rg '\b[RDP][0-9]+\b' ctadl-index/src/diff ctadl-ascent/src/cli/diff_index.rs docs`.

**Acceptance, as run.** At `4fd3c9fbd`, on aarch64-darwin. The greps, fmt, clippy and the
workspace tests had already passed there and were not rerun. Outputs are under
`$TMPDIR/acceptance/` (`TMPDIR` = the project's scratch directory on the external disk), named
below relative to it.
- **Regression.** `cargo xtask regression` (all frontends, debug `ctadl`): 255 passed,
  0 skipped, 0 failed, 5 xfail (`AndroidIcc:droidbench-activity-communication-2`,
  `C:defaultmodels`, `C:example`, `Cil:delegates`, `Jvm:ClassIdMergedTryFlow`) of 260.
  "Checked diff-index against SQL: 4032 table comparison(s) over 224 pair(s) of 225 indexes,
  both directions: all agree". Scratch root `$TMPDIR/nix-shell.d5SKvH/ctadl_xtask_10818`; log
  `regression.log`.
- `nix build .#checks.aarch64-darwin.regression`: the same 255 passed, 0 failed, the same
  5 xfail of 260. "Checked diff-index against SQL: 3996 table comparison(s) over 222 pair(s)
  of 223 indexes, both directions: all agree" (two indexes fewer than the cargo run). Scratch
  root `/nix/var/nix/builds/nix-28888-1451807179/ctadl_xtask_45102`, removed with the build;
  log `nix-regression.log`.
- **R24, the suite's indexes.** `r24a.sh` ran `ctadl diff-index` on each of Phase 0's 132
  indexes against the index at the same relative path in the new run: all 132 `IDENTICAL`
  (exit 0), the `apk-index` one included. The new run's other 93 indexes are frontends Phase 0
  did not run. Outputs `r24a/`, summary `r24a-summary.txt`.
- **R24, a bridge with sub-path ports.** `r24b/jni-flow.pathful.bridge.ctadl` (outside the
  repo) bridges `nativeStash` with `F::arg(0) <-> G::arg(2).f`, `F::arg(0).g -> G::arg(2)` and
  `F::arg(0) -> G::arg(1).h`, and `nativeFetch` with a path-less return. `r24b/run.sh` imports
  `JniFlow.dex` and `libjniflow.so` (from Phase 0's `Jni_JniFlow+bridge`) and indexes them with
  `--no-jni-bridge -m` that model, once with the pre-rename binary (`r24b/old`) and once with
  the new one (`r24b/new`), each in its own store. The old index names the temporaries
  `$bridge6_119#1` and `$bridge6_119#2`; the new one
  `$bridge[jni-flow.pathful.bridge.ctadl:0]Java_JniFlow_nativeStash#1` and `#2`. `diff-index`
  (`r24b/diff.txt`, `r24b/diff.json`): `DIFFERENT: 3 of 17 tables differ: actual_param,
  assign, call`, with 2, 9 and 1 rows only in A and as many only in B; every row of the 14
  other tables is the same. Each differing row names a `$bridge` local, directly or inside the
  bridge site's call instruction. Renaming the old names to the new ones in the 12 A-only rows
  gives exactly the 12 B-only rows. For example, A's `formal(0) → local($bridge6_119#2).f`
  pairs with B's `formal(0) →
  local($bridge[jni-flow.pathful.bridge.ctadl:0]Java_JniFlow_nativeStash#2).f`. The two
  queries' SARIF results (3 each) are equal.
- **Memory (D5).** Release `ctadl diff-index` on the `apk-index` index (the `app` import of
  `xtask/tests/dex/com.noto_54.apk`) against a copy of it, under `memguard.sh 8` with
  `/usr/bin/time -l`: `IDENTICAL`, 7.6 s wall, peak physical footprint 632 MiB
  (`mem/diff.txt`, `mem/guard.txt`). The largest table is `assign`, 2,280,830 rows, then `paths`
  (1,407,700); the index holds about 5.8M rows in all. With `--table` alone (`mem/per-table.txt`):
  `assign` 445 MiB, `paths` 126, `actual_param` 187, `index_source_map` 178, `call` 149,
  `import_id` 15 (no instruction maps). So the instruction maps are about 130 MiB and `assign`
  on both sides, with its raw rows, about 300 MiB, which D5's estimate fits (2 x 2.28M x
  36 bytes = 164 MB canonical, plus raw rows). The whole run's footprint never falls
  (`mem/trajectory.txt`), but that is the allocator, not a pair kept alive: `vmmap --summary`
  at 4.6 s, in the later tables (`mem/vmmap-4.6s.txt`), shows 139 MiB allocated in the malloc
  zone, about the instruction maps, beside 481 MiB of "Malloc Large (empty)", freed large
  blocks the allocator keeps dirty and that still count in the footprint. The peak is not near
  the sum of the tables but sits about 190 MiB above `assign`'s own.

## Deferred

- Generate `inspect_parquet`'s `match_schema!` list (`ctadl-ascent/src/cli/mod.rs:2292-2320`)
  from `Table::ALL`. It already lacks `resolved_call`, `globals`, `global_flow`, `taint_edge`,
  `intent_pair` and `manifest_*`.
- Make `parquet::Reader` return errors instead of panicking, so that `catch_unwind` is no longer
  needed.
- Stop leaking a `String` per batch for each `FlowVariable` column
  (`ctadl-index/src/facts/parquet.rs:1402`); it is bounded, about 1 MB per 30M-row table.
- Diffing `CTADL_DUMP_RELATIONS` dumps on the same core (out of scope in the intent).
