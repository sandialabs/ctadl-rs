# Step properties written by `ctadl query`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This orders the work of writing `taintVertex`, `callees` and
`summarized` on each step and of reading them in `diff-query`. R*/D* numbers are the spec's;
the spec wins where the two disagree, except for the corrections below. Each phase builds,
passes the gate named in it, and is one commit.

No code, comment, help text or document may cite a number from the spec or from this plan.

## Corrections to the spec

- P1. **The renamed field is in the theorems' definitions, and has no new name.** D3 renames
  the canonical step's `callee` field; R16 says the theorems' statements do not change. The
  field is read by two definitions the statements are built from, `step_permuted`
  (`sarif-canon/src/theorems.rs:91`) and `step_renumbered` (`theorems.rs:162`), and by the
  smoke tests (`sarif-canon/src/smoke.rs:896`). D3 does not give the new name. This plan
  assumes the name `call_info`, and that changing that one identifier in those two definitions
  is not a change of statement.
- P2. **Which name a site's callees are sorted by.** D2 sorts by `facts.id_to_name`. The name
  that is written is `source_data.id_to_name`, which starts as a copy of it
  (`ctadl-ascent/src/query_engine/formatter.rs:2762`) and which source info then overwrites
  for functions it knows (`formatter.rs:2274`). If the two ever differ, "the callee whose name
  sorts first" (R7, R9) is not the first of the names written. This plan assumes the sort is
  by the name that is written, so the map is built after `formatter.rs:2765`; nothing reads
  it earlier.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1, 2 | 1 renames a field in `sarif-canon` and `ctadl-query-diff`; 2 changes the callees of a site in the formatter; disjoint files |
| 2 | 3, 4 | 3 needs 1's field; 4 needs 2's map; disjoint files |
| 3 | 5 | needs the properties 4 writes and the reader 3 changes |
| 4 | 6 | documents and acceptance |

Phase 1 is a rename with no behavior change, so a Verus failure after it has one cause.
Phase 2 changes results only at call sites with several callees, and removes two result
properties; phase 4 only adds step properties and changes a call step's message at such a
site. So a regression delta after 2 is about which callee is named, and after 4 about steps.
Phase 3 changes what `diff-query` reads while the tests still add step properties themselves;
phase 5 then moves those tests onto the real output. Phases 3 and 5 both edit
`ctadl-ascent/tests/integration/diff_query.rs`; they are in different waves. In wave 2, phase
3 owns `diff_query.rs` and phase 4 does not touch it.

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

- `cargo test --workspace` and `scripts/verus.sh .` pass. Record the Verus summary lines.
- `cargo xtask regression` passes. It prints `Scratch root: <dir>`; copy every `*.sarif`
  under that root, keeping relative paths, to a directory outside the repo. This is the
  baseline SARIF. Record the pass, fail and xfail counts.
- Save `git diff --stat` (empty) and the commit id with the baseline.

**Gate.** The three commands pass; the baseline directory holds the SARIF files. No commit.

## Phase 1: rename the canonical step's field (D3, R16)

Depends on: 0.

**What.** Rename the field `callee` of the raw and canonical step to `call_info` (P1), and
restate its comment: it holds one optional atom for what the step says about callees. No
behavior changes: the parser still fills it from `callee`. No theorem statement changes; the
only edits in `theorems.rs` are the identifier in two definitions.

**Files.**
- `sarif-canon/src/types.rs:133` — the comment and the field of `RawStep`; the field at
  `:186` (`CanonStep`), `:267` (`RawStepV`), `:296` (`CanonStepV`); the views at `:376`,
  `:421`.
- `sarif-canon/src/prims.rs:523`, `:837`; `sarif-canon/src/order.rs:175`, `:525`, `:1258`,
  `:1436`, `:1548`, `:1632`; `sarif-canon/src/canon.rs:67`, `:76`, `:773` — the identifier.
- `sarif-canon/src/theorems.rs:91`, `:162` — the identifier, nothing else.
- `sarif-canon/src/smoke.rs` — the identifier (16 uses, from `:90`); comments at `:893`,
  `:905`, `:1197`, `:1249`, `:1274` say what the atom holds without naming a SARIF property.
- `ctadl-query-diff/src/parse.rs:229` and its tests (`:725`, `:744`, `:751`);
  `ctadl-query-diff/src/render.rs:99` — the identifier.

**Tests.**
- Existing, still passing unchanged: `cargo test -p sarif-canon -p ctadl-query-diff`, and the
  whole workspace.
- Verus: `scripts/verus.sh .` verifies with the same counts as phase 0.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `scripts/verus.sh .`;
`git diff <phase 0 commit> -- sarif-canon/src/theorems.rs` shows two changed lines, each the
identifier only; `grep -n 'callee' sarif-canon/src/*.rs` finds only comments that name the
SARIF properties older files hold (`types.rs:68`).

**Review.** The controller reads the `theorems.rs` diff before wave 2 starts.

## Phase 2: every callee of a call site (R6, R9, R10, D2)

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
  `source_data.id_to_name` (P2). One entry per row of `facts.call`; no other copy of the
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

## Phase 3: `diff-query` reads `callees` and `summarized` (R12, R13, R14, D3)

Depends on: 1.

**What.** `step` reads `callees` and `summarized` in place of `callee`, and puts in the
step's `call_info` atom the text of a JSON object with the keys the step has: `callees`
sorted with duplicates dropped, `summarized` as written; no atom when the step has neither.
The renderer prints the object's keys back under `properties`. `sarif-canon` does not change.
This changes behavior for files with step `callee`, `callees` or `summarized`; `ctadl query`
writes none of them before phase 4, so on its own output nothing changes.

**Files.**
- `ctadl-query-diff/src/parse.rs:228` — `step` builds the atom. `callees` must be an array
  of strings and `summarized` a boolean, else `Error::Property` with `on_step: true` and
  `expected` `"a list of strings"` or `"a boolean"`. Build the text with `serde_json` from a
  map with sorted keys, so equal content gives equal text. `step_prop` (`:235`) stays for
  `taintVertex`.
- `ctadl-query-diff/src/render.rs:99` — parse the atom's text and insert its keys into
  `props`; update the module comment at `:5`.
- `ctadl-query-diff/src/parse.rs` tests (`:540`, `:558`, `:885`, `:926`, `:1123`) and
  `ctadl-query-diff/src/tests.rs` (`:97`, `:290`, `:530`, `:590`–`:612`, `:674`) — write
  `callees` lists where they wrote `callee`.
- `ctadl-ascent/tests/integration/diff_query.rs:199` — `with_step_props` writes
  `"callees": ["callee_<k>"]`; `changed_step_property` (`:1160`) edits `callees[0]`, and its
  renumbering loop (`:1196`) rewrites only string values. The module comment at `:5` names
  `callees`. The helper itself stays until phase 5.

**Tests.**
- New, `ctadl-query-diff/src/tests.rs`:
  - `callees_compared_as_a_set` — `["a","b"]` against `["b","a"]` and against
    `["a","b","a"]` is `IDENTICAL`; against `["a"]` and against `["a","c"]` is `DIFFERENT`
    with one row each side.
  - `summarized_compared` — `summarized: true` against absent, and against
    `summarized: false`, is `DIFFERENT`; against itself `IDENTICAL`.
  - `absent_is_not_empty` — `callees: []` against no `callees` is `DIFFERENT`.
  - `callee_not_read` — two files that differ only in a step's `callee` are `IDENTICAL`; a
    step `callee` that is a number is accepted.
  - `endpoints_only_reads_first_and_last` — a changed `callees` or `summarized` on an
    interior step is `IDENTICAL` with `--endpoints-only` and `DIFFERENT` without; on the last
    step, `DIFFERENT` with it.
  - Malformed, extending the test at `:674`: `callees` as `"snk"`, `[1]`, `{}`, `null`, and
    `summarized` as `"true"`, `1`, `null`, each exit 2 with the property named.
  - Printed form, extending `:290`: a row shows
    `"properties":{"callees":["a","b"],"summarized":true,"taintVertex":...}` with only the
    keys present.
- New, `parse.rs` tests: the atom's text for `{callees:["b","a","b"]}` is the text for
  `{callees:["a","b"]}`; for `summarized` only; for both; absent for neither.
- Existing, still passing: the rest of `cargo test --workspace`; `scripts/verus.sh .`
  (no `sarif-canon` change).

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `git diff --stat <phase 1 commit> -- sarif-canon` is empty.

## Phase 4: `ctadl query` writes step properties (R1–R5, R7, R8, D1)

Depends on: 2.

**What.** One small value describes a step before it is written: its vertex, what it says
about callees (one name, `summarized`, or nothing), and the function its message names.
`push_site_step` takes it in place of the message string and renders the message and the
property bag from it. This adds `properties` to every step and changes the message of a call
step at a site with several callees. Which steps are written, their order, locations, `kinds`
and `executionOrder` do not change: the dedup test does not read the new value.

**Files.**
- `ctadl-ascent/src/query_engine/formatter.rs:3325` — move the vertex text
  (`<variable><path>`) into one function; use it there, at `:3178`, and for every step.
- `formatter.rs:2801` — `push_site_step` takes the step value and the message's leading word;
  builds the message `<word> <vertex> in <function>` and a `PropertyBag` with `taintVertex`,
  and `callees` (a one-element list) or `summarized: true` when the value has them; sets it
  on the `ThreadFlowLocation` (`:2857`). The test at `:2845`–`:2850` is not edited.
- `formatter.rs:2869`, `:2987` — source and sink steps: vertex only; the message names the
  site's first callee, as after phase 2.
- `formatter.rs:2920` — the edge's step. A call edge: `callees` is the destination's
  function. A return edge: `callees` is the origin's function (`id_to_node[src_id].0`); the
  message keeps naming the destination's. `site_by_edge` holds call and return edges only
  (`ctadl-index/src/facts.rs:1745`); if the third arm of the `match` at `:2924` can be
  reached, stop and report.
- `formatter.rs:2959` — the interior `call-arg` step. Walk with the index of the window so
  the next window is in hand. When the edge leaving `dst_id` (`edge_by_edge` at
  `(dst_id, path[i + 2])`) is a call edge, the step is a call step: `callees` and the message
  name `id_to_node[path[i + 2]].0`. Otherwise, or when there is no next window, it is a
  summarized step: `summarized: true`, and the message is what it was after phase 2.
- New fixtures under `ctadl-ascent/tests/tnt/` (see Tests).
- New `ctadl-ascent/tests/integration/step_props.rs`, and its `mod` line in
  `ctadl-ascent/tests/integration/main.rs:14`.

**Tests.** In `step_props.rs`, each on SARIF from `cli::query` in the `human` profile, the
way `cli.rs:223` runs a `.tnt` fixture:
- `call_step_names_the_function_entered` — fixture `step_props_two_callees.tnt`: one
  indirect call that resolves to `get_a` and `get_b` (as
  `ctadl-ascent/tests/tnt/funcptr_two_targets.tnt` does), with the sink one call further
  down in each. The flow into `sink_a` has a step at the indirect call with
  `callees == ["get_a"]` whose message ends ` in get_a`; the flow into `sink_b` has
  `["get_b"]`. Neither has `summarized`.
- `return_step_names_the_function_returned_from` — a source inside a callee whose value
  returns to the caller and reaches a sink there: the `return` step's `callees` is the
  callee, and its message ends with the caller.
- `summarized_step_between_arguments` — a call that copies one argument to another, crossed
  by a summary (C's `xfer.c` shape, in `.tnt`): the step has `summarized: true`, no
  `callees`.
- `summarized_step_into_global` — a callee that writes its argument to a global a sink reads:
  the step at the call has `summarized: true`, no `callees`.
- `endpoint_step_at_two_callees` — in `step_props_two_callees.tnt`, a sink directly on the
  formal of both targets is anchored at the indirect call: its step has `taintVertex` only,
  its message names `get_a`, and the result's `sinkFunctions` is `["get_a", "get_b"]`.
- `every_step_has_a_vertex_and_interior_steps_say_callees` — over every flow of every fixture
  above and of `ctadl-ascent/tests/c/ties.c`: each step's `taintVertex` is the vertex in its
  message; each step whose message does not start with `source ` or `sink ` has exactly one
  of `callees` (one name) and `summarized` (`true`); each source or sink step has neither.
- `machine_profile_has_no_steps` — the `machine` profile of the two-callee fixture has no
  `codeFlows`.
- Existing, still passing: `cargo test --workspace`, including `diff_query.rs` unchanged
  (the old reader ignores `callees` and `summarized`; `taintVertex` is a string).
- Regression: counts as in phase 0. Then compare with the baseline SARIF: for each file,
  `runs[].results` with each step's `properties` and each result's `sourceCallee` and
  `sinkCallee` deleted (`jq`) is equal to the baseline's, except in results at a call site
  with several callees. List every file that differs and why.

**Gate.** `cargo fmt --all -- --check`; `cargo clippy --workspace -- -Dwarnings`;
`cargo test --workspace`; `cargo xtask regression`; the comparison with the baseline SARIF.

## Phase 5: the metamorphic tests run on real properties (R15, R17, D4)

Depends on: 3, 4.

**What.** `with_step_props` and the "with step properties" base go; the SARIF `ctadl query`
wrote is the only base. A second fixture, the two-callee program of phase 4, is indexed twice
with the two callees' function ids in opposite orders. Tests only.

**Files.**
- `ctadl-ascent/tests/integration/diff_query.rs:199`–`:246` — delete `with_step_props`,
  `Base::step_props` and the second base; `bases` returns the real file. Restate the module
  comment (`:9`–`:13`).
- `diff_query.rs:958` (`endpoints_only`) — the `taintVertex` checks at `:1019`–`:1036` run on
  the real file in the profiles that have flows; add a changed `callees` and a removed
  `summarized` at an interior step (reported without `--endpoints-only`, not with it), on a
  flow that has such a step.
- `diff_query.rs:1160` (`changed_step_property`) — runs on the real file: the first step's
  `taintVertex`; the first step that has `callees`, its name changed; a `summarized` removed.
  A `callees` list reversed, and one with an entry repeated, report nothing. Renumbered
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
nothing.

## Phase 6: documents and acceptance (R19)

Depends on: 5.

**What.** The documents say what is written and compared. No code changes but help text and
comments.

**Files.**
- `docs/debugging.md:257`–`:260` — a step is compared by `kinds`, location, `taintVertex`,
  `callees` (as a set) and `summarized`; drop "does not write these yet". Add which step has
  which property; that a return step's `callees` is the function returned from while its
  message names the caller; that a summarized step names no callee; and when a flow's first
  or last step is not its source or sink step (the sink step dropped at the instruction of
  the step before it; an endpoint with no call site has no step), so `--endpoints-only`
  compares the step that is there.
- `docs/debugging.md:271` — `sourceCallee` and `sinkCallee` are properties older files hold.
  `:281`, `:293`, `:347` — `callees` and `summarized` where they say `callee`; the error list
  names a `callees` that is not a list of strings and a `summarized` that is not a boolean.
- `ctadl-ascent/src/main.rs:92`–`:93` — the help text names `callees` and `summarized` and
  drops the parenthesis.
- `docs/spec.md:261`–`:263` (§5) — the tests run on the properties `ctadl query` writes; name
  `callees` and `summarized`. `docs/spec.md:219` (§4.5) — a step carries `taintVertex`, and
  `callees` or `summarized`.
- `sarif-canon/src/types.rs:68` — unchanged if it still reads true; check.

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
| `sarif-canon/src/types.rs`, `prims.rs`, `order.rs`, `canon.rs`, `theorems.rs`, `smoke.rs` | field renamed, comment restated | 1 |
| `ctadl-query-diff/src/parse.rs` | field renamed; then `callees` and `summarized` read | 1, 3 |
| `ctadl-query-diff/src/render.rs` | field renamed; then the object's keys printed | 1, 3 |
| `ctadl-query-diff/src/tests.rs` | `callees` and `summarized` tests | 3 |
| `ctadl-ascent/src/query_engine/formatter.rs` | site callees; then step properties | 2, 4 |
| `ctadl-ascent/tests/integration/cli.rs` | reads `sinkFunctions` | 2 |
| `ctadl-ascent/tests/integration/diff_query.rs` | writes `callees`; then real properties | 3, 5 |
| `ctadl-ascent/tests/integration/step_props.rs`, `main.rs` | new tests | 4 |
| `ctadl-ascent/tests/tnt/step_props_*.tnt` | new fixtures | 4 |
| `docs/debugging.md`, `docs/spec.md`, `ctadl-ascent/src/main.rs` | documents, help text | 6 |

Not touched: `xtask/src/assertions.rs` (its readers of message text stay);
`ctadl-ascent/src/query_engine/search.rs`; `ctadl-index`; `multiset-diff`, `order-laws`,
`index-canon`; `sarif-canon`'s theorem statements and `scripts/verus.sh`.

## Acceptance

- `cargo fmt --all -- --check`, `cargo clippy --workspace -- -Dwarnings`,
  `cargo test --workspace`, `cargo xtask regression` and `scripts/verus.sh .` pass on the
  last commit, with the regression and Verus counts of phase 0.
- R8, R11: the comparison of phase 4 against the baseline SARIF, run again on the last
  commit, differs only at call sites with several callees.
- R16: `git diff <phase 0 commit> -- sarif-canon/src/theorems.rs` is the two identifier
  lines of phase 1.
- Names that must be gone: `grep -rn 'sourceCallee\|sinkCallee' ctadl-ascent/src` and
  `grep -rn 'with_step_props' ctadl-ascent` find nothing; `"callee"` is read nowhere in
  `ctadl-query-diff/src` outside tests of older files.
