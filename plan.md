# `ctadl diff-sarif`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order of the work, the files each step changes
and the tests that prove it. R*, D* and P-* names are the spec's; the spec wins where the two
disagree. Each phase builds, passes the gate named in it, and is one commit. F is
`ctadl-ascent/src/query_engine/formatter.rs`.

Code, comments, help text and docs written by these phases must not cite the spec's or the
plan's numbers (R*, D*, P-*, phase numbers).

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1, 2 | 1 is the Verus plumbing and the `sarif-canon` skeleton with its types; 2 moves the report's sample code into a shared crate. Independent code. |
| 2 | 3, 4, 5 | 3 is the order on canonical types, 4 the sort and the other primitives, 5 the parser. All need only 1's types; disjoint files. |
| 3 | 6a | `canon_run` needs 3's order and 4's primitives |
| 4 | 6b, 7 | 6b proves the theorems about 6a; 7, the driver and report, needs 2's shared code, 5's parser and 6a's `canon_run`. Disjoint files. |
| 5 | 8, 9 | 8 wires the subcommand; 9 adds the metamorphic tests on real SARIF. Both call 7's API. |
| 6 | 10 | docs and comments close, after 6b's proofs and 8, 9 |

The verified core is built bottom-up (types, then order and primitives, then their
composition) so each Verus phase proves one layer against stubs that already exist.
`canon_run`'s code (6a) is split from the theorems about it (6b), so the driver, CLI and
real-SARIF tests (7-9) do not wait on the hardest proofs; 10 closes only after 6b. The
parser (5) is written against the raw types alone, so it runs beside the proofs. The report
refactor (2) changes no output of `diff-index` and lands before anything uses it, so a
`diff-index` regression has one cause. Wiring (8) and real-SARIF tests (9) come after the
library is whole.

Shared files between parallel phases:

- Root `Cargo.toml` and `Cargo.lock` (wave 1): Phase 1 adds `sarif-canon`, Phase 2 adds
  `diff-report`, on different lines of `members`, `default-members` and
  `[workspace.dependencies]`. Phase 1 commits first; Phase 2 rebases onto it and lets cargo
  regenerate `Cargo.lock`.
- `sarif-canon/src/lib.rs` (wave 2): owned by Phase 1, which declares every module. Phases 3
  and 4 edit only their own module files. If either needs a re-export in `lib.rs`, it adds
  one line and the controller merges.
- `ctadl-ascent/tests/integration/main.rs` (wave 5): owned by Phase 9 (adds
  `mod diff_sarif;`). Phase 8 adds its tests to `cli.rs`, which is already listed.

Test commands used throughout (from the dev shell, `use flake`):

    cargo fmt --all -- --check
    cargo clippy --workspace -- -Dwarnings
    cargo test --workspace
    nix build .#checks.aarch64-darwin.verus      # x86_64-linux on Linux
    cargo xtask regression

`nix build` of the flake sees only files git tracks: `git add` new files before running the
`verus` check.

For a quicker Verus loop than the flake check, the two commands it runs (Phase 1), with
`$T` a scratch directory:

    verus --crate-type=lib --edition=2024 --crate-name multiset_diff \
      --export $T/md.vir --compile -o $T/libmultiset_diff.rlib multiset-diff/src/lib.rs
    verus --crate-type=lib --edition=2024 --crate-name sarif_canon \
      --import multiset_diff=$T/md.vir --extern multiset_diff=$T/libmultiset_diff.rlib \
      sarif-canon/src/lib.rs

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record that the unchanged tree is green, and how long the `verus` check takes, so
later phases can tell their own failures and slowdowns from existing ones. R17 needs no
output baseline: Phase 10's acceptance checks that the files that write SARIF are
unchanged.

**Files.** None.

**Tests.** Run the gate below; save the logs and the wall time of the `verus` check outside
the repo.

**Gate.**

    cargo test --workspace
    nix build .#checks.aarch64-darwin.verus
    cargo xtask regression

## Phase 1: Verus plumbing and the `sarif-canon` skeleton (R13, R16, D1, D3, D4)

Depends on: 0.

**What.** Make `multiset-diff`'s specs and theorems usable from another verified crate, and
create `sarif-canon` with its types, stub modules and a cross-crate check. No behavior
changes; nothing calls the new crate yet.

**Files.**
- `multiset-diff/src/lib.rs:24` — `mod proofs;` becomes `pub mod proofs;`. Update the
  module doc (`multiset-diff/src/lib.rs:1-16`) to say the theorems are public for verified
  callers. `multiset_diff_by` (`multiset-diff/src/lib.rs:257-275`) is not changed (D4).
- `sarif-canon/Cargo.toml` (new) — as `multiset-diff/Cargo.toml:1-14`: the same exact
  `vstd`, `verus_builtin`, `verus_builtin_macros` pins, plus `multiset-diff` from the
  workspace. No serde.
- `sarif-canon/src/lib.rs` (new) — crate doc (what is proved, how to run Verus, as
  `multiset-diff/src/lib.rs:1-16`); `use multiset_diff::{total_order, computes, sorted,
  eqv, le, lt};`; declares `pub mod types; pub mod order; pub mod sort; pub mod prims;
  pub mod canon;`. Proof-only modules are `#[cfg(verus_keep_ghost)]` the way
  `name-by-facts/src/lib.rs:19-23` does it.
- `sarif-canon/src/types.rs` (new) — D3's raw and canonical types, generic over `A: Copy`,
  one per row of D4's table, plain data in `verus!`:
  - `RawRun<A> { logical: Vec<RawLogical<A>>, results: Vec<RawResult<A>> }`;
    `RawLogical<A> { fqn: A }` (the parser picks `fullyQualifiedName` else `name`);
    `RawResult`, `RawLocation` (optional physical location, logical references as
    `RawLogicalRef::{Index(usize), Inline(A)}`), `RawCodeFlow`, `RawThreadFlow`,
    `RawStep { order: Option<i64>, kinds: Vec<A>, location: RawLocation }`,
    `RawPhysical { base: Option<A>, uri: Option<A>, region: RawRegion, address: Option<RawAddress> }`,
    and the result properties of D4 (`labels`, `vertex`, `vertices`, `source_fns`,
    `sink_fns`, `tainted_formals`, `import`).
  - The matching `Canon*` types, with no index, id or order-number field (R8 by
    construction). Unordered collections are `Vec`s.
  - `Error { OutOfRange, NotIncreasing, EmptyFlow }`.
  - The `spec fn view` of each canonical type, if the proofs in Phases 3 and 4 need `==` on
    `Seq`s rather than `Vec`s. The phase that needs it may add it.
- `sarif-canon/src/{order,sort,prims,canon}.rs` (new) — empty `verus! {}` stubs with a
  one-line module doc each. Phases 3, 4 and 6 fill them.
- `flake.nix:292-306` — the check verifies `multiset-diff` with `--crate-name
  multiset_diff --export md.vir --compile -o libmultiset_diff.rlib`, then `name-by-facts`
  as today, then `sarif-canon` with `--import multiset_diff=md.vir --extern
  multiset_diff=libmultiset_diff.rlib`. Copy `sarif-canon/src` in as the loop does now.
  Update the comment at `flake.nix:292` to name the three crates.
- `Cargo.toml` — add `"sarif-canon"` to `members` (`Cargo.toml:3-34`), `default-members`
  (from `Cargo.toml:37`) and `[workspace.dependencies]` (near `Cargo.toml:94-95`); update
  the comment at `Cargo.toml:110` to say "the verified crates".

**Tests.**
- New, `sarif-canon/src/lib.rs`: a proof `smoke_imports_multiset_diff` that uses
  `multiset_diff::total_order` and `multiset_diff::proofs::theorem_no_value_twice` on a
  `Seq<u32>`, so the check fails if the import breaks.
- New, `sarif-canon` unit test `types_build`: builds one `RawRun<u32>` under plain cargo,
  so the erased crate compiles and links.
- Existing, still passing: `multiset-diff`'s `fixed_examples`
  (`multiset-diff/src/lib.rs:428-441`), its Verus proofs (34 verified), `name-by-facts`'s
  proofs and tests.

**Gate.**

    cargo test -p multiset-diff -p sarif-canon
    cargo clippy --workspace -- -Dwarnings
    nix build .#checks.aarch64-darwin.verus

## Phase 2: shared report pieces in `diff-report` (D1, D5)

Depends on: 0.

**What.** Move the bounded-sample buffer and the per-side sample block out of
`ctadl-index-diff` into a small new crate `diff-report`, so `ctadl-sarif-diff` can use them
without linking `ctadl-index`. `diff-index`'s text and JSON output do not change. A new crate rather than a `pub mod` in `ctadl-index-diff` was chosen so that
`cargo test -p ctadl-sarif-diff` does not build `ctadl-index` and `name-by-facts`.

**Files.**
- `diff-report/Cargo.toml`, `diff-report/src/lib.rs` (new) — depends on `multiset-diff`
  and `serde`. Holds:
  - `Sample` and `Samples` (moved from `ctadl-index-diff/src/report.rs:61-82`, with
    `total()`), with their `Serialize` derives unchanged;
  - `Smallest` (moved from `ctadl-index-diff/src/table.rs:433-482`), made `pub`;
  - `pub fn write_samples(w, side: &str, samples: &Samples, unit: &str, indent: &str)`, the
    block now inlined at `ctadl-index-diff/src/report.rs:159-172`, with the unit ("rows",
    "results") and indentation as parameters.
- `ctadl-index-diff/src/report.rs:61-82, 159-172` — drop the moved definitions, call
  `diff_report::write_samples(.., "rows", "  ")`.
- `ctadl-index-diff/src/table.rs:24, 433-482` — import `Smallest` instead of defining it.
- `ctadl-index-diff/src/lib.rs:35` — keep re-exporting `Sample` and `Samples` (now from
  `diff_report`): `ctadl-ascent/tests/integration/diff_index.rs:25` imports them from here.
- `ctadl-index-diff/Cargo.toml` — add `diff-report`.
- `Cargo.toml` — `diff-report` in `members`, `default-members`,
  `[workspace.dependencies]` (see the shared-file note above).

**Tests.**
- New, `diff-report/src/lib.rs`: `smallest_keeps_first_limit` (push 30 values in reverse
  order with limit 20; `finish` yields the 20 smallest in order and `not_shown == 10`);
  `smallest_limit_zero` (keeps nothing, counts all); `write_samples_layout` (exact text for
  unit "results" and no indent: `"more in A: 2 results, 0 not shown\n  [A 1, B 0] x\n..."`).
- Existing, unchanged and passing: `ctadl-index-diff/src/tests.rs`
  `sample_limit_and_remainder` (:658, exact text), `all_rows_differ` (:674),
  `swap_is_mirror` (:778), `output_is_deterministic` (:789),
  `samples_independent_of_load_order` (:802), `json_matches_text` (:876); the integration
  tests in `ctadl-ascent/tests/integration/diff_index.rs` and the `diff-index` CLI tests at
  `ctadl-ascent/tests/integration/cli.rs:1411-1525`.
- Regression: the `diff_sql` check (`xtask/src/diff_sql.rs:183-211`), which parses
  `diff-index --json`, must pass unchanged.

**Gate.**

    cargo test -p diff-report -p ctadl-index-diff
    cargo test -p ctadl-ascent --test integration diff_index
    cargo clippy --workspace -- -Dwarnings
    cargo xtask regression

## Phase 3: the order on canonical types (R14, D3, P-ord)

Depends on: 1.

**What.** `cmp_canon` for every canonical type, executable and verified: lexicographic over
fields, over `Option` (`None` first) and over sequences, built from an atom `cmp` that
computes a total order `oa` in which `Equal` means `==`. Proves each is a total order
(multiset-diff's `total_order`) and returns `Equal` exactly when its arguments are equal.
No callers yet.

**Files.**
- `sarif-canon/src/order.rs` — owned by this phase:
  - generic lemmas, stated with `multiset_diff`'s `total_order`, `eqv`, `le`: lex order on
    `Seq<T>` is a total order and is `Equal` iff `==` when the element order is
    (port `name-by-facts/src/order.rs:48-229` and `name-by-facts/src/lib.rs:795-822`,
    which are stated over name-by-facts's own copies of these definitions,
    `name-by-facts/src/order.rs:9-35`); the same for pairs/records (product order) and
    `Option`;
  - one `spec fn ord_*` and one exec `fn cmp_*` per canonical type, with
    `ensures o == ord_*(*x, *y)`, and a lemma per type: `total_order(ord_*)` and
    `ord_*(x, y) == Equal <==> x == y`;
  - integers in regions and addresses (`u64`/`i64`) compared by their own order.
  `name-by-facts` is not changed.

**Tests.**
- Verus: the lemmas above, for the top type `CanonResult` in particular:
  `lemma_result_order_total` and `lemma_result_order_equal_iff_eq`.
- New, plain cargo, `sarif-canon/src/order.rs` `#[cfg(test)]`: `cmp_result` on hand-built
  `CanonResult<u32>`s agrees with a reference `Ord` derived on a mirror type, over a few
  hundred generated pairs (a deterministic generator, no new dependency): antisymmetric,
  `Equal` exactly when equal.

**Gate.**

    cargo test -p sarif-canon
    nix build .#checks.aarch64-darwin.verus

## Phase 4: verified primitives (R13, D3, P-ref, P-bag, P-seq, P-end)

Depends on: 1.

**What.** The primitives of D3, generic over any element type and any `cmp`/`ord` with
`total_order(ord)` and `ord(x, y) == Equal <==> x == y` as preconditions (so this phase
does not wait for Phase 3's concrete orders). No callers yet.

**Files.**
- `sarif-canon/src/sort.rs` — a verified merge sort `sort_by(v: Vec<T>, cmp, Ghost(ord)) ->
  Vec<T>`, ensuring `sorted(ord, r@)` and `r@.to_multiset() == v@.to_multiset()`. Not
  `std`'s sort. Plus `lemma_sorted_unique`: two sequences sorted by such an `ord` with equal
  multisets are equal (what turns P-bag's permutation into equality of outputs).
- `sarif-canon/src/prims.rs`:
  - `resolve(table, i) -> Result<T, Error>` with P-ref (renumbering by a permutation `π`
    and `π⁻¹`, and errs exactly when `i >= table.len()`);
  - `canon_bag(xs, f) -> Vec<C>`: `map` then `sort_by`; P-bag, including the corollary for
    any permutation `ys` of `xs`;
  - `canon_seq(xs, f) -> Vec<C>`: P-seq;
  - `endpoints(xs, f) -> Result<(C, C), Error>`: P-end; errs exactly on an empty `xs`;
  - `check_increasing(orders: &[Option<i64>]) -> Result<(), Error>`: `Ok` exactly when the
    `Some` values are strictly increasing along the array. Absent values are allowed; ctadl
    always writes them (F:2861). The spec does not say what to do with absent values; this
    is the least strict choice that cannot make two different flows compare equal, since
    the order compared is the array order.
  `f` is an exec closure with `f.requires` for all inputs and its result named by a spec
  function, as `computes` does for `cmp`.

**Tests.**
- Verus: one lemma per property above.
- New, plain cargo, `sarif-canon/src/prims.rs` `#[cfg(test)]`: `sort_by` against
  `slice::sort` on generated `u32` vectors with duplicates (lengths 0..=64);
  `check_increasing` on `[Some(1), Some(2)]` (ok), `[Some(2), Some(2)]` (err), `[]` (ok);
  `endpoints` of `[]` errs; `resolve` of index `len` errs.

**Gate.**

    cargo test -p sarif-canon
    nix build .#checks.aarch64-darwin.verus

## Phase 5: the SARIF parser (R15, D2)

Depends on: 1.

**What.** Create `ctadl-sarif-diff` with its parser: SARIF file → `RawRun<u32>`s over an atom
table shared by A and B. Nothing calls it yet.

**Files.**
- `ctadl-sarif-diff/Cargo.toml` (new) — `serde-sarif = "0.8"` (as
  `ctadl-ascent/Cargo.toml:62`), `serde_json`, `sarif-canon`, `rustc-hash`.
- `ctadl-sarif-diff/src/lib.rs` (new) — crate doc; `mod atoms; mod parse;`. Phase 7 adds
  the rest.
- `ctadl-sarif-diff/src/atoms.rs` (new) — `Atoms`: `FxHashMap<Box<str>, u32>` plus
  `Vec<Box<str>>`; `intern`, `get(u32) -> &str`. Injective by construction (D2's one
  assumption).
- `ctadl-sarif-diff/src/parse.rs` (new) — `read_runs(path, &mut Atoms) ->
  Result<Vec<RawRun<u32>>, Error>`:
  - reads with `serde_sarif::sarif::Sarif`; a read or deserialize error is an error (exit 2
    later); unknown `kind`/`level` values already fail to deserialize (serde-sarif's
    `ResultKind`/`ResultLevel` enums);
  - takes only D4's kept fields; fills `kind` `fail` and `level` `warning` when absent;
    reads `properties` keys from `PropertyBag.additional_properties` with the shapes ctadl
    writes (`taintLabels`: array of strings; `taintVertex`: string; `taintVertices`:
    object of label → array; `sourceFunctions`/`sinkFunctions`: array; `taintedFormals`:
    object of formal → array; `import`: string). A kept key of the wrong shape is an error;
  - `logicalLocations[].index` → `RawLogicalRef::Index` (negative is an error); an entry
    with neither `fullyQualifiedName` nor `name` is an error;
  - the `call-arg` rewrite, anchored at the start of the vertex string (a vertex is a
    `FlowVariable` then its path, `ctadl-index/src/facts.rs:1422-1435`; `call-arg` occurs
    only as the prefix): `call-arg(<digits>, <formal>)<path>` → `call-arg(_, <formal>)<path>`.
    Applied to `taintVertex` and every `taintVertices` entry. Other strings are left alone.
  - The parsed `Sarif` is dropped as soon as its runs are built, so one file's document is
    in memory at a time.

**Tests.** New, `ctadl-sarif-diff/src/parse.rs` `#[cfg(test)]`, on small hand-written SARIF
strings:
- `call_arg_rewrite`: `call-arg(123, 0).f` → `call-arg(_, 0).f`; `call-arg(7, -2)` →
  `call-arg(_, -2)`; `local(call-arg(1, 0))` unchanged; `formal(1).x` unchanged.
- `drops_unread_fields`: two documents that differ only in messages, `ruleIndex`,
  `fileSpanId`/`funcId`/`insnId`, `sourceCallee`, invocations, tool version and
  `originalUriBaseIds` values parse to equal `RawRun`s.
- `kind_level_defaults`: absent `kind`/`level` parse equal to `fail`/`warning`.
- `bad_inputs_error`: not JSON; a `taintLabels` that is a string; a negative logical index.
- `atoms_shared`: the same string in A and B gets one atom.

**Gate.**

    cargo test -p ctadl-sarif-diff
    cargo clippy --workspace -- -Dwarnings

## Phase 6a: `canon_run` (R5-R12, D3, D4)

Depends on: 3, 4.

**What.** Compose the primitives into `canon_run`, element by element as D4's table says. The
code verifies against the primitives' contracts and ensures its output is sorted, which is
what `multiset_diff` needs. The invariance and end-to-end theorems are Phase 6b, so the
library and CLI (Phases 7-9) do not wait on them. No callers yet.

**Files.**
- `sarif-canon/src/canon.rs` — owned by this phase:
  - `canon_run(run: &RawRun<A>, endpoints_rule: Option<A>, cmp, Ghost(oa)) ->
    Result<Vec<CanonResult<A>>, Error>`, ensuring `sorted(ord_result, r@)` (Phase 3's
    `ord_result`, from `canon_bag`'s ensures). `endpoints_rule` is the atom of `C0001` when
    `--endpoints-only` is set; a result whose `rule` equals it drops its locations,
    `taintVertex` and `taintVertices`, and keeps the `endpoints` of each thread flow (R11).
    Logical references resolve to the entry's `fqn`. `executionOrder` goes through
    `check_increasing` and is then dropped.
  - `canon_runs(runs: &[RawRun<A>], ..)`: all runs' results pooled, then one `canon_bag`
    (the `sarifLog.runs` row), with the same ensures.
  - A `spec fn` for each, naming its result in terms of the primitives' spec functions, for
    Phase 6b to reason about.
  No `assume`, `admit` or `external_body`: everything this phase ensures is proved here.

**Tests.**
- Verus: `canon_run` and `canon_runs` verify, including the sortedness ensures.
- New, plain cargo, `sarif-canon/src/canon.rs` `#[cfg(test)]`, on hand-built `RawRun<u32>`s:
  `renumbered_table_same`; `shuffled_results_same`; `swapped_steps_differ`;
  `endpoints_only_ignores_interior` and `endpoints_only_counts_twice` (two flows with equal
  endpoints vs one: the outputs have lengths 2 and 1); `out_of_range_errs`;
  `non_increasing_errs`; `empty_flow_with_endpoints_only_errs`.

**Gate.**

    cargo test -p sarif-canon
    nix build .#checks.aarch64-darwin.verus

## Phase 6b: the invariance and end-to-end theorems (R13, R14, D4)

Depends on: 6a.

**What.** Proofs only, about 6a's spec functions; no executable code changes.

**Files.**
- `sarif-canon/src/theorems.rs` (new), `#[cfg(verus_keep_ghost)]`:
  - `theorem_canon_invariant`: renumbering `logical` and every index into it, and permuting
    any P-bag collection, at any depth, leaves `canon_runs` unchanged (P-ref and P-bag
    lifted through every row of D4's table);
  - `theorem_diff_empty_iff_equal`: the end-to-end theorem as D4 states it, using
    `multiset_diff::proofs::theorem_reports_exactly_count_differences` and Phase 3's
    `Equal <==> ==` lemma to restate counts with `==`.
- `sarif-canon/src/lib.rs` — one line, `#[cfg(verus_keep_ghost)] mod theorems;`, and the
  crate doc's "what is proved" names the two theorems.

**Tests.** Verus: the two theorems. Existing `sarif-canon` tests unchanged.

**Gate.**

    cargo test -p sarif-canon
    nix build .#checks.aarch64-darwin.verus
    grep -nE 'assume|admit|external_body' sarif-canon/src   # empty

## Phase 7: driver and report (R1-R4, R12, D1, D5)

Depends on: 2, 5, 6a.

**What.** The `ctadl-sarif-diff` library API: read both files, canonicalize, diff, report.
Adds the dependency to `ctadl-ascent` but no subcommand yet.

**Files.**
- `ctadl-sarif-diff/src/lib.rs` — `pub struct Options { limit, endpoints_only }`;
  `pub fn diff_sarif(a: &Path, b: &Path, &Options) -> Result<Report, Error>`; `Error`
  carries the file and what failed (parse, `OutOfRange`, `NotIncreasing`, `EmptyFlow`).
- `ctadl-sarif-diff/src/driver.rs` (new) — `read_runs` for A then B into one `Atoms`;
  `canon_runs` per file with `endpoints_rule = atoms.get_id("C0001")` when
  `--endpoints-only`; `multiset_diff(&a, &b, |x, y| sarif_canon::order::cmp_result(x, y,
  &u32::cmp))` (the same `cmp` `canon_runs` sorted by, so its proved sortedness is the
  precondition `multiset_diff` needs); each `Delta` to `more_in_a` or `more_in_b`
  (as `ctadl-index-diff/src/table.rs:505-509`) through `diff_report::Smallest`, keyed by the
  rendered string so samples do not depend on atom numbers.
- `ctadl-sarif-diff/src/report.rs` (new) — `Report { a, b, endpoints_only, limit,
  more_in_a: Samples, more_in_b: Samples }`; `identical()`; `write_text`: `IDENTICAL`, or
  `DIFFERENT`, `A: <path>`, `B: <path>`, the flags in force, then
  `diff_report::write_samples(.., "results", "")` for A then B; `write_json`:
  `{result, results_differ, more_in_a, more_in_b, ...}`; a canonical result renders as
  compact JSON with atoms resolved to strings.
- `ctadl-sarif-diff/Cargo.toml` — add `multiset-diff` and `diff-report`.
- `ctadl-ascent/Cargo.toml:28` — add `ctadl-sarif-diff.workspace = true`.
- `Cargo.toml` — `ctadl-sarif-diff` in `members`, `default-members`,
  `[workspace.dependencies]`.

**Tests.** New, `ctadl-sarif-diff/src/tests.rs`, on small SARIF files written to a tempdir:
- `identical_self`: A against A is `IDENTICAL`, `identical()` true.
- `deleted_result`: one result fewer in B → `more in A: 1 results`, `[A 1, B 0]`.
- `duplicate_counts` (R12): a result duplicated in A → `[A 2, B 1]`.
- `swap_is_mirror` (R4): text of B,A is A,B's with sides swapped.
- `output_is_deterministic` (R4): two runs, byte-identical; and interning B first gives the
  same output.
- `limit_bounds_samples` (R3): 30 differing results, `--limit 20` → 20 shown, 10 not shown;
  `identical()` the same for every `limit`, including 0.
- `json_matches_text`: `results_differ` agrees with the text's first line.
- `errors_are_errors` (R2): unreadable file, out-of-range index, non-increasing
  `executionOrder` each return `Err`.

**Gate.**

    cargo test -p ctadl-sarif-diff
    cargo build -p ctadl-ascent
    cargo clippy --workspace -- -Dwarnings

## Phase 8: the `ctadl diff-sarif` subcommand (R1-R3)

Depends on: 7.

**What.** Wire the library into the CLI, beside `diff-index`, with the same exit codes.

**Files.**
- `ctadl-ascent/src/main.rs:68-85` — a `DiffSarif(DiffSarifArgs)` variant after
  `DiffIndex`, its doc stating exits 0/1/2 as `ctadl-ascent/src/main.rs:83-84` does.
- `ctadl-ascent/src/main.rs:94-120` — `DiffSarifArgs` after `DiffIndexArgs`: positional
  `a`, `b` as `PathBuf`; `--limit N` (default 20); `--json`; `--endpoints-only`.
- `ctadl-ascent/src/main.rs:759` — dispatch arm,
  `Command::DiffSarif(args) => std::process::exit(diff_sarif(args))`.
- `ctadl-ascent/src/main.rs:931-959` — `fn diff_sarif(&DiffSarifArgs) -> i32` beside
  `diff_index`, the same shape: error → stderr, 2; write error → 2; else 0 or 1.

**Tests.**
- New, `ctadl-ascent/src/main.rs` beside `diff_index_parses` (:1437): `diff_sarif_parses`
  (all flags).
- New, `ctadl-ascent/tests/integration/cli.rs` after :1525, with a `run_diff_sarif` helper
  copied from `run_diff_index` (:1434): `diff_sarif_same_file_exit_0`;
  `diff_sarif_deleted_result_exit_1` (stdout starts `DIFFERENT`);
  `diff_sarif_malformed_exit_2` (stdout empty, stderr names the file);
  `diff_sarif_exit_independent_of_limit_and_json` (`--limit 0`, `--json`: same exit code).
  The SARIF comes from `ctadl query` on `xfer.c` (as `test_cli_query_c_sources_and_sinks`,
  `cli.rs:107`) and an edited copy.

**Gate.**

    cargo test -p ctadl-ascent --bin ctadl diff_sarif_parses
    cargo test -p ctadl-ascent --test integration diff_sarif
    cargo clippy --workspace -- -Dwarnings

## Phase 9: metamorphic tests on real SARIF (R15)

Depends on: 7.

**What.** The tests that cover what Verus does not, on SARIF from real `ctadl query` runs in
every profile. Test-only.

**Files.**
- `ctadl-ascent/tests/integration/diff_sarif.rs` (new) — owns the tests below. Fixture: the
  pipeline of `ctadl-ascent/tests/integration/determinism.rs:173-196` (import `ties.c`, index
  with `ties.json`), queried in Human, Agent, Machine and Debug (`cli::query(..,
  SarifProfile::*, None)`); once per test binary, in a `OnceLock` as `cli.rs:1418` does.
  `ties.c` gives C0001 with equal-length paths, field paths, C0003/C0004, and (in Agent)
  C0007; Machine gives C0002; Debug adds the debug properties.
- `ctadl-ascent/tests/integration/main.rs:13` — add `mod diff_sarif;`.
- `ctadl-ascent/tests/integration/diff_index.rs:346` — make `remap_index` and the remap
  helpers it needs `pub(super)`, so renumbered ids come from a genuinely renumbered index,
  not from editing SARIF text.

**Tests.** Each in every profile, through `ctadl_sarif_diff::diff_sarif`, asserting
`identical()` or the exact `more_in_a`/`more_in_b` counts:
- Report nothing: `self_identical`; `shuffled_arrays` (shuffle `results`, `locations`,
  `codeFlows`, `threadFlows`, `logicalLocations` of a location, and the set- and map-valued
  properties, with a seeded shuffle); `renumbered_logical_locations` (permute
  `run.logicalLocations` and rewrite every `index`); `renumbered_ids` (query an index copied
  with `remap_index` renumbering instructions, functions and spans, as
  `diff_index.rs:534` `renumbered_insns_and_spans` does: changes `call-arg` ids and
  `fileSpanId`/`funcId`/`insnId`); `changed_messages` (rewrite every `message.text`).
  Each of these first checks that the edit changed the file's bytes, so a no-op edit cannot
  pass.
- Report exactly that: `deleted_result` (`more_in_a` 1, `more_in_b` 0);
  `duplicated_result` (`[A 1, B 2]`); `swapped_steps` (swap two differing steps of one
  thread flow: one result each side); `changed_line` (`startLine` + 1 in one location:
  one result each side).
- `endpoints_only`: `swapped_steps` of two interior steps reports nothing with
  `endpoints_only`, and a deleted C0001 result still reports 1.

**Gate.**

    cargo test -p ctadl-ascent --test integration diff_sarif
    cargo test -p ctadl-ascent --test integration diff_index
    cargo clippy --workspace -- -Dwarnings

## Phase 10: docs and comments (R18), and acceptance

Depends on: 6b, 8, 9.

**What.** Document the command and the new verified crate. No code changes. Docs must not
cite spec or plan numbers.

**Files.**
- `docs/spec.md:88-90` (F12) — name `ctadl diff-sarif`: compare the results of two SARIF
  files by content.
- `docs/spec.md:244-248` (§5) — add `sarif-canon` to the verified cores with
  `multiset-diff` and `name-by-facts`; after :249-251 add a bullet for `diff-sarif`'s
  metamorphic tests.
- `docs/debugging.md` — a section `# Comparing two query outputs` after the `diff-index`
  section (:182-219, before :221): usage, what is compared and ignored, `--endpoints-only`,
  output and exit codes. Mention it where :173-180 says regressions should not compare the
  shape of a rendered flow.
- `nightly/README.md:31-37` — the `verus` check also proves `diff-sarif`'s canonical core.
- `flake.nix:350` — the dev shell comment names the verified crates, not only
  `multiset-diff` (stale today; Phase 1 already fixed `flake.nix:292` and `Cargo.toml:110`).

**Tests.** None new.

**Gate.** The acceptance below.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `multiset-diff/src/lib.rs` | `pub mod proofs`; doc | 1 |
| `sarif-canon/Cargo.toml`, `src/lib.rs`, `src/types.rs` | new crate, types | 1 |
| `sarif-canon/src/order.rs` | order and its proofs | 3 (stub in 1) |
| `sarif-canon/src/sort.rs`, `src/prims.rs` | merge sort, primitives, proofs | 4 (stubs in 1) |
| `sarif-canon/src/canon.rs` | `canon_run` | 6a (stub in 1) |
| `sarif-canon/src/theorems.rs`, `src/lib.rs` | invariance and end-to-end theorems; `mod theorems;` | 6b |
| `flake.nix` | cross-crate `verus` check; comments | 1, 10 |
| `Cargo.toml` | workspace entries; comment at :110 | 1, 2, 7 |
| `diff-report/*` | new crate: `Sample`, `Samples`, `Smallest`, `write_samples` | 2 |
| `ctadl-index-diff/src/{report,table,lib}.rs`, `Cargo.toml` | use `diff-report` | 2 |
| `ctadl-sarif-diff/Cargo.toml`, `src/{lib,atoms,parse}.rs` | new crate, parser | 5 |
| `ctadl-sarif-diff/src/{driver,report,tests}.rs`, `Cargo.toml` | driver, report; depend on `multiset-diff`, `diff-report` | 7 |
| `ctadl-ascent/Cargo.toml` | depend on `ctadl-sarif-diff` | 7 |
| `ctadl-ascent/src/main.rs` | `DiffSarif` subcommand | 8 |
| `ctadl-ascent/tests/integration/cli.rs` | process tests | 8 |
| `ctadl-ascent/tests/integration/{diff_sarif,main}.rs` | metamorphic tests | 9 |
| `ctadl-ascent/tests/integration/diff_index.rs` | `remap_index` `pub(super)` | 9 |
| `docs/spec.md`, `docs/debugging.md`, `nightly/README.md` | docs | 10 |
| `Cargo.lock` | new crates | 1, 2, 5, 7 |

Not touched:
- `ctadl-ascent/src/query_engine/formatter.rs`, `ctadl-import/src/sarif.rs`: `ctadl query`'s
  SARIF does not change (R17).
- `ctadl-index-diff`'s behavior and `ctadl-ascent/src/cli/diff_index.rs` (R17); Phase 2
  only moves code.
- `name-by-facts`: Phase 3 ports its order lemmas into `sarif-canon` rather than sharing
  them.
- `multiset_diff_by` (D4).
- `.github/workflows/nightly.yml`: it already builds `checks.<system>.verus`, which now
  includes `sarif-canon` (R16).

## Acceptance

- `cargo fmt --all -- --check`, `cargo clippy --workspace -- -Dwarnings`,
  `cargo test --workspace` pass.
- `nix build .#checks.aarch64-darwin.verus` passes with `sarif-canon` in it; compare its wall
  time with Phase 0's and report the added time.
- `cargo xtask regression` passes, as in Phase 0.
- R17: `git diff main -- ctadl-ascent/src/query_engine ctadl-import/src ctadl-index-diff/src/canon.rs ctadl-index-diff/src/driver.rs ctadl-ascent/src/cli/diff_index.rs`
  is empty, and `diff-index` output is unchanged (Phase 2's tests).
- No `assume`, `admit` or `external_body` in `sarif-canon`:
  `grep -nE 'assume|admit|external_body' sarif-canon/src` is empty.
- No spec or plan numbers in code or docs:
  `git diff main -- . ':!spec.md' ':!plan.md' ':!intent.md' | grep -nE '\b(R[0-9]+|D[0-9]|P-[a-z]+|Phase [0-9])\b'`
  is empty.
- By hand: `ctadl diff-sarif` on a regression case's SARIF against itself prints `IDENTICAL`
  and exits 0; against the same case after a model change prints the changed results.

### Complexity and memory

No phase touches an existing hot path; the only moved code (`Smallest`, Phase 2) is
unchanged. For the new command, with `n` results and `s` the size of a result:
parsing holds one file's `serde_sarif` document at a time (dropped once its raw runs are
built, Phase 5); the atom table holds each distinct kept string once for both files;
`canon_run` is `O(n log n)` comparisons of `O(s)` each, and the merge sort's extra space is
`O(n)`; `multiset_diff` is linear; samples render each differing result once and keep at
most `2 * limit` (`Smallest`). Nothing grows past the two files' canonical results plus the
atom table.

## Deferred

- Run `ctadl diff-sarif` in `cargo xtask regression`, e.g. each case's SARIF against a run
  on a renumbered index (`xtask/src/sarif.rs` already handles each case's SARIF).
- Share one copy of the order definitions and lemmas among `multiset-diff`,
  `name-by-facts` and `sarif-canon`.
