# Step properties written by `ctadl query`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This orders the work of writing `taintVertex`, and `callee` or
`summarized`, on each step and of reading them in `diff-query`. R*/D* numbers are the spec's;
the spec wins where the two disagree. Each phase builds,
passes the gate named in it, and is one commit.

No code, comment, help text or document may cite a number from the spec or from this plan.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1, 3 | 1 states the verified step with `summarized`; 3 changes the callees of a site in the formatter; disjoint files |
| 2 | 2, 4, 5 | 2 proves 1's statements; 4 needs 1's types; 5 needs 3's map; disjoint files |
| 3 | 6 | needs the properties 5 writes and the reader 4 changes |
| 4 | 7 | documents and acceptance; needs 2's proofs complete |

Wave 2 starts after the user has reviewed phase 1's statements. Phase 1 changes the verified
types and what is claimed about them, with no change in what `diff-query` reports; phase 2
only proves. So a Verus failure is about a statement in 1 and about a proof in 2. Phase 3
changes results only at call sites with several callees, and removes two result properties;
phase 5 only adds step properties and changes a call step's message at such a site. So a
regression delta after 3 is about which callee is named, and after 5 about steps. Phase 4
makes `diff-query` read `summarized` while the tests still add step properties themselves;
phase 6
then moves those tests onto the real output. Phases 1 and 4 both edit
`ctadl-query-diff/src/parse.rs` and `render.rs`, and phases 4 and 6 both edit
`ctadl-ascent/tests/integration/diff_query.rs`; each pair is in different waves. In wave 2,
phase 2 owns `sarif-canon`, phase 4 owns `ctadl-query-diff` and `diff_query.rs`, and phase 5
owns the formatter, `xtask` and the rest of `ctadl-ascent/tests/integration`.

Test commands used throughout:

    cargo fmt --all -- --check
    cargo clippy --workspace -- -Dwarnings
    cargo test --workspace
    cargo xtask regression
    scripts/verus.sh .

`scripts/verus.sh` needs `verus` on `PATH`, which the Nix dev shell provides; it is what
`nix build .#checks.<system>.verus` runs. `cargo xtask regression` needs the dev shell's
toolchains.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record what the unchanged tree does, for R8, R11 and R16.

- `cargo test --workspace` and `scripts/verus.sh .` pass. Record the Verus summary lines,
  and the output of `grep -n 'admit()\|assume(\|external_body' sarif-canon/src/*.rs`.
- `cargo xtask regression` passes. It prints `Scratch root: <dir>`; copy every `*.sarif`
  under that root, keeping relative paths, to a directory outside the repo. This is the
  baseline SARIF. Record the pass, fail and xfail counts.
- Save `git diff --stat` (empty) and the commit id with the baseline.

**Gate.** The three commands pass; the baseline directory holds the SARIF files. No commit.

## Phase 1: statements for `summarized` (R16, D3)

Depends on: 0.

**What.** The raw and the canonical step get `summarized`, an optional boolean, beside
`callee`. This phase writes the types, the definitions, the contracts and the theorems'
definitions for it, and smoke tests of those statements. The executable code is the real
code, since `cargo test` runs it: it is a line or two per function. A proof that no longer
goes through is admitted (`admit()`), and each admit is listed in the commit message.
`diff-query` reports what it did: the parser sets `summarized` to `None`.

**Files.**
- `sarif-canon/src/types.rs:134` — `RawStep` gets `summarized: Option<bool>`
  ("`properties.summarized`"); the same in `CanonStep` (`:186`), `RawStepV` (`:267`) and
  `CanonStepV` (`:296`); the views at `:376`, `:421`.
- `sarif-canon/src/canon.rs:55` — `spec_canon_step` copies `summarized` in both arms;
  `canon_step` (`:765`) does the same; its contract is unchanged in form: it returns
  `spec_canon_step`.
- `sarif-canon/src/order.rs:169` — `ord_step` compares `summarized` after `callee`, by an
  order on an optional boolean (absent, `false`, `true`), a new definition. `cmp_step`
  (`:1258`) computes it; the lemma at `:518` that `ord_step` is a strict total order keeps
  its statement. The model in the tests (`:1436`, `:1548`, `:1632`) gets the field.
- `sarif-canon/src/prims.rs:523`, `:837` — `copy_step` copies the field; its contract
  (`r@ == s@`) is unchanged.
- `sarif-canon/src/theorems.rs:91` — `step_permuted` gets `x.summarized == y.summarized`;
  `:162` — `step_renumbered` gets the same. No theorem signature changes
  (`theorem_canon_invariant`, `:1132`; `theorem_diff_empty_iff_equal`, `:1160`).
- `sarif-canon/src/smoke.rs` — the step builders (`:90`, `:221`, `:896`, `:900`) take the
  field; the smoke tests below.
- `ctadl-query-diff/src/parse.rs:229` — `summarized: None`; the expected steps in its tests
  (`:725`, `:744`, `:751`) follow. Replaced in phase 4.

**Tests.**
- Smoke, in `smoke.rs`, using no admitted fact:
  - not vacuous: `smoke_permuted_bags_holds` (`:1249`) and `smoke_renumbered_holds` (`:1274`)
    on steps of which one has `summarized: Some(true)`;
  - passes: the canonical step keeps `Some(true)`, `Some(false)` and `None` as they are;
  - fails: two runs that differ only in a step's `summarized` (`Some(true)` and `None`;
    `Some(true)` and `Some(false)`) have different canonical forms, and are neither
    `step_permuted` nor `step_renumbered`, beside the test at `:1197` for `callee`.
- Existing, still passing unchanged: `cargo test --workspace`.
- Verus: `scripts/verus.sh .` verifies, with the admits listed.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `scripts/verus.sh .`;
`git diff <phase 0 commit> -- sarif-canon/src/theorems.rs` changes no `theorem_` signature.

**Review.** The user checks the statements (the types, `spec_canon_step`, `ord_step`,
`step_permuted`, `step_renumbered` and the smoke tests) against the spec before wave 2.

## Phase 2: proofs for `summarized` (R16)

Depends on: 1.

**What.** Replace every admit of phase 1 with a proof. This phase may add helper
definitions and lemmas, proof blocks and invariants. It does not change a statement of
phase 1: no type, definition, contract, theorem or smoke test. If a statement is false or
cannot be proved, stop and report; the change is the user's call. No behavior changes.

**Files.**
- `sarif-canon/src/order.rs` — the order on an optional boolean is a strict total order;
  `ord_step` is one (`:518`); `cmp_step` computes it.
- `sarif-canon/src/canon.rs`, `prims.rs`, `theorems.rs` — whatever about steps was admitted.

**Tests.**
- Verus: `scripts/verus.sh .` verifies with no new admit.
- Existing, still passing: `cargo test -p sarif-canon`.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `scripts/verus.sh .`;
`grep -n 'admit()\|assume(\|external_body' sarif-canon/src/*.rs` prints what phase 0
recorded. The controller checks that `git diff <phase 1 commit> -- sarif-canon/src` changes
no statement and that `smoke.rs` is unchanged.

## Phase 3: every callee of a call site (R6, R9, R10, D2)

Depends on: 0.

**What.** `call_callee` becomes a map from a site to all its callees, sorted by name and
deduplicated by name. `endpoint_callee` returns the first. A path result's `sourceFunctions`
and `sinkFunctions` take the whole list, and `sourceCallee` and `sinkCallee` are no longer
written. This changes output: the two properties go from every `C0001` result; at a site
with several callees the function lists grow and the one function named may change. Step
properties are not written yet.

**Files.**
- `ctadl-ascent/src/query_engine/formatter.rs:2571` — replace the map by
  `BTreeMap<PackedInsnSiteId, Vec<FunctionId>>`, built by a new free function next to
  `endpoint_callee` from `ctx.facts.call` and the name map. Sort each site's list by name
  (compare borrowed names; a function with no name compares as `func#<id>`), then drop
  entries whose name repeats. Build it after `formatter.rs:2765`, from
  `source_data.id_to_name`. One entry per row of `facts.call`; no other copy of the
  call list.
- `formatter.rs:3286` — `endpoint_callee` takes the new map and returns the first callee,
  else `infunc`. Add `endpoint_callees`, returning the whole list (or `infunc` alone).
- `formatter.rs:2967` — the interior `call-arg` step's message takes the site's first callee.
- `formatter.rs:3205`–`3223` — `sourceFunctions` and `sinkFunctions` collect every callee of
  each endpoint's site.
- `formatter.rs:3232`–`3245` — drop `sourceCallee` and `sinkCallee`; keep the two lists
  written only when not empty. Restate the comment at `:3198`.
- `formatter.rs:3301`, `:3328`, `:3140` — `format_source_sink_results` takes the new map and
  keeps naming one function, the first.
- `ctadl-ascent/tests/integration/cli.rs:297` — read `sinkFunctions` (one entry in that
  fixture) in place of `sinkCallee`.

**Tests.**
- New, `formatter.rs` test module: `site_callees_sorted_by_name` — rows
  `(s, f_b), (s, f_a), (s, f_a), (t, f_c)` in that order and in the reverse order give the
  same map, `s -> [f_a, f_b]`, `t -> [f_c]`; `site_callees_unnamed` — a function with no name
  sorts and prints as `func#<id>`; `site_callees_same_name_once` — two ids with one name give
  one entry.
- Changed, `cli.rs`: `test_cli_query_each_path_result_carries_only_its_flow` reads
  `sinkFunctions[0]`.
- Existing, still passing: `cargo test --workspace`. `ctadl-query-diff/src/parse.rs:567`,
  `:980`, `:1028` keep `sourceCallee` and `sinkCallee` in their inputs: they are the test
  that an older file compares as it did (R14).
- Regression: `cargo xtask regression` has the pass, fail and xfail counts of phase 0. If a
  case changes, it must be at a call site with several callees, where the sink function
  `xtask/src/assertions.rs:223` reads is now the first by name; report it, do not edit the
  expectation without saying so.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `cargo xtask regression`;
`grep -n 'sourceCallee\|sinkCallee' ctadl-ascent/src` finds nothing.

## Phase 4: `diff-query` reads `summarized` (R12, R13, R14, D3)

Depends on: 1.

**What.** `step` reads `summarized` beside `taintVertex` and `callee`, and the renderer
prints it under `properties`. `sarif-canon` does not change. This changes behavior only for
files with a step `summarized`; `ctadl query` writes none before phase 5.

**Files.**
- `ctadl-query-diff/src/parse.rs:228` — `step` reads `summarized`: a boolean, else
  `Error::Property` with `on_step: true` and `expected` `"a boolean"`; absent is `None`.
  `step_prop` (`:235`) is unchanged.
- `ctadl-query-diff/src/render.rs:99` — print `summarized` under `properties` when present;
  update the module comment at `:5`.
- `ctadl-ascent/tests/integration/diff_query.rs:1196` — the renumbering loop of
  `changed_step_property` rewrites only string values, so it keeps working once real steps
  have a boolean (phase 5 lands in the same wave; `with_step_props` still overwrites the
  bag until phase 6).

**Tests.**
- New, `ctadl-query-diff/src/tests.rs`:
  - `summarized_compared` — `summarized: true` against absent, and against
    `summarized: false`, is `DIFFERENT` with one row each side; against itself `IDENTICAL`.
  - `endpoints_only_reads_first_and_last` — a changed `callee` or a removed `summarized` on
    an interior step is `IDENTICAL` with `--endpoints-only` and `DIFFERENT` without; on the
    last step, `DIFFERENT` with it.
  - Malformed, extending the test at `:674`: `summarized` as `"true"`, `1`, `null`, each
    exit 2 with the property named.
  - Printed form, extending `:290`: a row shows `"summarized":true` under `properties`, and
    only the keys present.
- New, `parse.rs` tests: `summarized` parses to `Some(true)` or `Some(false)`; a step
  without it to `None`.
- Existing, still passing unchanged: the rest of `cargo test --workspace`, including every
  test that writes a step `callee`.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `git diff --stat <phase 1 commit> -- sarif-canon` is empty.

## Phase 5: `ctadl query` writes step properties (R1–R5, R7, R8, R18, R19, D1)

Depends on: 3.

**What.** One small value describes a step before it is written: its vertex, what it says
about its callee (a name, `summarized`, or nothing), and the function its message names.
`push_site_step` takes it in place of the message string and renders the message and the
property bag from it. This adds `properties` to every step and changes the message of a call
step at a site with several callees. Which steps are written, their order, locations, `kinds`
and `executionOrder` do not change: the dedup test does not read the new value.

**Files.**
- `ctadl-ascent/src/query_engine/formatter.rs:3325` — move the vertex text
  (`<variable><path>`) into one function; use it there, at `:3178`, and for every step.
- `formatter.rs:2801` — `push_site_step` takes the step value and the message's leading word;
  builds the message `<word> <vertex> in <function>` and a `PropertyBag` with `taintVertex`,
  and `callee` or `summarized: true` when the value has one; sets it
  on the `ThreadFlowLocation` (`:2857`). The test at `:2845`–`:2850` is not edited.
- `formatter.rs:2869`, `:2987` — source and sink steps: vertex only; the message names the
  site's first callee, as after phase 3.
- `formatter.rs:2920` — the edge's step. A call edge: `callee` is the destination's
  function. A return edge: `callee` is the origin's function (`id_to_node[src_id].0`); the
  message keeps naming the destination's. `site_by_edge` holds call and return edges only
  (`ctadl-index/src/facts.rs:1745`); if the third arm of the `match` at `:2924` can be
  reached, stop and report.
- `formatter.rs:2959` — the interior `call-arg` step. Walk with the index of the window so
  the next window is in hand. When the edge leaving `dst_id` (`edge_by_edge` at
  `(dst_id, path[i + 2])`) is a call edge, the step is a call step: `callee` and the message
  name `id_to_node[path[i + 2]].0`. Otherwise, or when there is no next window, it is a
  summarized step: `summarized: true`, and the message is what it was after phase 3.
- Two new fixtures under `ctadl-ascent/tests/tnt/` (see Tests). The Flowy tests run every
  `.tnt` file there (`ctadl-ascent/tests/integration/flowy_tests.rs:89`), so each must pass
  as one. No existing fixture is edited.
- New `ctadl-ascent/tests/integration/step_props.rs`, and its `mod` line in
  `ctadl-ascent/tests/integration/main.rs:14`.
- The check of every flow (R19). For every step of every flow of a SARIF file:
  `properties.taintVertex` is a string, and the message is
  `<word> <taintVertex> in <function>`; when `<word>` is `source` or `sink` the step has
  neither `callee` nor `summarized`; otherwise it has exactly one of `callee`, a string, and
  `summarized`, `true`. It returns what is wrong with the first step that fails, naming the
  result and the step.
  - `xtask/src/assertions.rs` — `check_step_properties`, shaped as `check_unexpected_lines`
    (`:90`) is. `xtask/src/regression.rs:1310`, `:1655`, `:1881`, `:2539`, `:2666` — each
    pass criterion calls it on the human-profile SARIF where it calls
    `codeflow_connects_source_and_sink`, and fails the case with its text.
  - `ctadl-ascent/tests/integration/common.rs` — the same check, and
    `query`, with the signature of `cli::query` (`ctadl-ascent/src/cli/mod.rs:971`): it
    calls `cli::query`, and when that wrote the output file, panics with what the check
    reports. Every `cli::query(` in `ctadl-ascent/tests/integration` becomes
    `common::query(`, in `bridging_end_to_end.rs`, `cli.rs`, `determinism.rs`,
    `global_ports.rs`, `jni_summary.rs`, `multi_import_sarif.rs`, `port_semantics.rs`,
    `sarif_uris.rs` and the new `step_props.rs`. Not `diff_query.rs:50`: phase 4 owns that
    file in this wave, and phase 6 changes it.

**Tests.** In `step_props.rs`, each on SARIF from `cli::query` in the `human` profile, the
way `cli.rs:223` runs a `.tnt` fixture:
- `call_step_names_the_function_entered` — existing fixture
  `ctadl-ascent/tests/tnt/funcptr_two_targets.tnt`: one indirect call (`:45`) that resolves
  to `get_a` and `get_b`, with the sink one call further down in each. The flow into
  `sink_a` has a step at the indirect call with `callee == "get_a"` whose message ends
  ` in get_a`; the flow into `sink_b` has `"get_b"`. Neither has `summarized`. (Before this
  phase both messages name one of the two.)
- `return_step_names_the_function_returned_from` — existing fixture
  `ctadl-ascent/tests/tnt/callsite_paths.tnt`: the source is inside `src`, whose value
  returns to `main` and reaches a sink there. Each of its two flows has a `return` step
  (`:35`, `:37`) with `callee == "src"` whose message ends ` in main`.
- `summarized_step_between_arguments` — existing fixture `ctadl-ascent/tests/tnt/ex4.tnt`:
  `A(x, y)` (`:4`) carries `x.bar` to `y.foo`, and the flow has one step, at that call, and
  none at the calls inside `A`. The step has `summarized: true`, no `callee`, and its
  message ends ` in A`.
- `summarized_step_into_global` — new fixture `step_props_global.tnt`: a callee that writes
  its argument to a global, which a sink in the caller reads after the call. The step at the
  call has `summarized: true`, no `callee`. Give the sink a call site (a sink on a callee's
  formal, as `snk` in `callsite_paths.tnt`): a flow with no step has no `codeFlows`, which is
  what `global_call_boundary.tnt` gives.
- `endpoint_step_at_two_callees` — new fixture `step_props_two_callees.tnt`: the program of
  `funcptr_two_targets.tnt`, and also a sink directly on the formal of both targets, which
  that fixture leaves out on purpose. That sink is anchored at the indirect call: its step
  has `taintVertex` only, its message names `get_a`, and the result's `sinkFunctions` is
  `["get_a", "get_b"]`. The flows into `sink_a` and `sink_b` stay, so phase 6 renumbers a
  call step's `callee` with this fixture too.
- A `.tnt` `source(...)` gives no source step (no existing fixture's flow has one), so the
  endpoint step these tests see is a sink's.
- `check_rejects_a_bad_step` — the check of `common.rs` on the SARIF of
  `step_props_two_callees.tnt`,
  edited as JSON, reports: a call step given `summarized` too; a call step with its `callee`
  removed; a sink step given a `callee`; a step whose `taintVertex` is changed. It reports
  nothing on the file as written. The same four edits, on a small SARIF literal, in a unit
  test of `check_step_properties` in `xtask/src/assertions.rs`.
- Every other test above, and every existing integration test that queries, now runs the
  check through `common::query`.
- `machine_profile_has_no_steps` — the `machine` profile of `step_props_two_callees.tnt` has no
  `codeFlows`.
- Existing, still passing: `cargo test --workspace`, including `diff_query.rs` unchanged
  (`with_step_props` still overwrites each step's bag; `taintVertex` and `callee` are
  strings, and a reader without phase 4 ignores `summarized`).
- Regression: counts as in phase 0. Then compare with the baseline SARIF: for each file,
  `runs[].results` with each step's `properties` and each result's `sourceCallee` and
  `sinkCallee` deleted (`jq`) is equal to the baseline's, except in results at a call site
  with several callees. List every file that differs and why.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `cargo xtask regression`, which now checks every flow of every
case; the comparison with the baseline SARIF;
`grep -rn 'cli::query(' ctadl-ascent/tests/integration` finds `common.rs` and
`diff_query.rs:50` only.

## Phase 6: the metamorphic tests run on real properties (R15, R17, R19, D4)

Depends on: 4, 5.

**What.** `with_step_props` and the "with step properties" base go; the SARIF `ctadl query`
wrote is the only base. A second fixture, phase 5's `step_props_two_callees.tnt`, is indexed twice
with the two callees' function ids in opposite orders. Tests only.

**Files.**
- `diff_query.rs:50` — `query_all` queries through `common::query`, so every profile of
  `ties.c`, of its renumbered index and of the two-callee fixture is checked (R19).
- `ctadl-ascent/tests/integration/diff_query.rs:199`–`:246` — delete `with_step_props`,
  `Base::step_props` and the second base; `bases` returns the real file. Restate the module
  comment (`:9`–`:13`).
- `diff_query.rs:958` (`endpoints_only`) — the `taintVertex` checks at `:1019`–`:1036` run on
  the real file in the profiles that have flows; add a changed `callee` and a removed
  `summarized` at an interior step (reported without `--endpoints-only`, not with it), on a
  flow that has such a step.
- `diff_query.rs:1160` (`changed_step_property`) — runs on the real file: the first step's
  `taintVertex`; the first step that has `callee`, its name changed; a `summarized` removed.
  Renumbered
  instruction ids in `taintVertex` report nothing.
- `diff_query.rs:757` (`renumbered_ids`) — unchanged code; it now covers step properties for
  `ties.c`. Add the same test for the two-callee fixture, with a helper next to `renumbered`
  (`:88`) that indexes it both ways: once alone, and once after a pad import, as
  `renumbered` does. A function id is given to a name the first time any import of the
  project mentions it (`ctadl-index/src/facts.rs:2103`), and imports are read in the order
  the project lists them (`ctadl-import/src/project.rs:893`). So the pad is a `.tnt` program
  that mentions the two callees in the opposite order without defining or calling them
  (`def pad(o) { start: o.x = ptr<get_b>; o.y = ptr<get_a>; return; }`), written to the
  store as `PAD_C` is (`:78`). The fixture file and its import name are the same in both
  projects, so every location and name is too. Do not define the callees in the pad (one
  name is one function), and do not reorder the fixture (lines would move).

**Tests.**
- `renumbered_ids` — passes for `ties.c` in every profile, with and without
  `--endpoints-only`.
- New `renumbered_callee_ids` — the two-callee fixture indexed twice. First assert that the
  two indexes give the callees' ids in opposite orders (read `funcId` of the two callees from
  the `debug` profile, or from the index), so a reorder that did nothing cannot pass. Then
  every profile compares `IDENTICAL`, with and without `--endpoints-only`, and each result's
  `sourceFunctions` and `sinkFunctions` are equal in the two files.
- Changed `endpoints_only`, `changed_step_property` as above; each first asserts the edit
  changed the document.
- Existing, still passing: the rest of `diff_query.rs`, on one base.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`;
`grep -n 'with_step_props\|step_props:' ctadl-ascent/tests/integration/diff_query.rs` finds
nothing; `grep -rn 'cli::query(' ctadl-ascent/tests/integration` finds `common.rs` only.

## Phase 7: documents and acceptance (R20)

Depends on: 2, 6.

**What.** The documents say what is written and compared. No code changes but help text and
comments.

**Files.**
- `docs/debugging.md:257`–`:260` — a step is compared by `kinds`, location, `taintVertex`,
  `callee` and `summarized`; drop "does not write these yet". Add which step has
  which property; that a return step's `callee` is the function returned from while its
  message names the caller; that a summarized step names no callee; and when a flow's first
  or last step is not its source or sink step (the sink step dropped at the instruction of
  the step before it; an endpoint with no call site has no step), so `--endpoints-only`
  compares the step that is there.
- `docs/debugging.md:271` — `sourceCallee` and `sinkCallee` are properties older files hold.
  `:281`, `:293`, `:347` — `summarized` beside `callee`; the error list names a `summarized`
  that is not a boolean.
- `ctadl-ascent/src/main.rs:92`–`:93` — the help text names `summarized` beside `callee` and
  drops the parenthesis.
- `docs/spec.md:261`–`:263` (§5) — the tests run on the properties `ctadl query` writes; name
  `summarized`. `docs/spec.md:219` (§4.5) — a step carries `taintVertex`, and `callee` or
  `summarized`.
- `sarif-canon/src/types.rs:68` — unchanged if it still reads true; check.
- `sarif-canon/src/lib.rs:14`–`:31` — "What is proved" still reads true; check.

**Tests.**
- Existing: `cargo test --workspace` (a test of the help text, if any, still passes).

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`;
`grep -rn 'not write\|does not write' docs/debugging.md docs/spec.md ctadl-ascent/src/main.rs`
finds no sentence about step properties;
`grep -rnE '\b(R|D|P)[0-9]+\b' docs/debugging.md ctadl-ascent/src/main.rs ctadl-ascent/src/query_engine/formatter.rs ctadl-query-diff/src sarif-canon/src/types.rs`
finds no citation of this plan or the spec.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `sarif-canon/src/types.rs`, `canon.rs`, `order.rs`, `prims.rs`, `theorems.rs`, `smoke.rs` | `summarized` in the step: statements, then proofs | 1, 2 |
| `ctadl-query-diff/src/parse.rs`, `render.rs` | `summarized` set to `None`; then read and printed | 1, 4 |
| `ctadl-query-diff/src/tests.rs` | `summarized` tests | 4 |
| `ctadl-ascent/src/query_engine/formatter.rs` | site callees; then step properties | 3, 5 |
| `ctadl-ascent/tests/integration/cli.rs` | reads `sinkFunctions` | 3 |
| `ctadl-ascent/tests/integration/diff_query.rs` | renumbering loop; then real properties | 4, 6 |
| `ctadl-ascent/tests/integration/step_props.rs`, `main.rs` | new tests | 5 |
| `ctadl-ascent/tests/integration/common.rs` | the check of every flow, and `query` | 5 |
| the other files of `ctadl-ascent/tests/integration` that query | query through `common::query` | 5 |
| `xtask/src/assertions.rs`, `regression.rs` | the check of every flow, in every pass criterion | 5 |
| `ctadl-ascent/tests/tnt/step_props_global.tnt`, `step_props_two_callees.tnt` | new fixtures | 5 |
| `docs/debugging.md`, `docs/spec.md`, `ctadl-ascent/src/main.rs` | documents, help text | 7 |

Not touched: the readers of message text in `xtask/src/assertions.rs`;
`ctadl-ascent/src/query_engine/search.rs`; `ctadl-index`; `multiset-diff`, `order-laws`,
`index-canon`; the signatures of `sarif-canon`'s theorems; `scripts/verus.sh`.

## Acceptance

- `cargo fmt --all -- --check`, `cargo clippy --workspace -- -Dwarnings`,
  `cargo test --workspace`, `cargo xtask regression` and `scripts/verus.sh .` pass on the
  last commit, with the regression and Verus counts of phase 0.
- R8, R11: the comparison of phase 5 against the baseline SARIF, run again on the last
  commit, differs only at call sites with several callees.
- R16: `scripts/verus.sh .` verifies with the admits phase 0 recorded and no other, and
  `git diff <phase 1 commit> -- sarif-canon/src` changes no statement.
- Names that must be gone: `grep -rn 'sourceCallee\|sinkCallee' ctadl-ascent/src` and
  `grep -rn 'with_step_props' ctadl-ascent` find nothing.
