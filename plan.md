# Step properties and invocation comparison in `diff-query`: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This orders the work of making `ctadl diff-query` compare step
properties, execution notifications and invocation statuses. R\* and D\* numbers are the
spec's; the spec wins where the two disagree. Each phase builds, passes the gate named in it,
and is one commit.

No code, comment, help text or document may cite a number from the spec or from this plan.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | the gates are green on the unchanged tree |
| 1 | 1 | the Verus statements with the step properties; every later phase builds on the new step types |
| 2 | 2, 3 | 2 is code and proofs inside `sarif-canon`; 3 is notifications and statuses inside `ctadl-query-diff`; disjoint files |
| 3 | 4 | the parser and printer read the step properties; needs 2's comparison, and 3's `parse.rs` and `tests.rs` |
| 4 | 5, 6 | 5 is the metamorphic tests in `ctadl-ascent/tests`; 6 is documents and help text; disjoint files |
| 5 | 7 | measures the finished branch on TaintBench; changes no code |

Phase 1 changes the step types, so it must also touch every constructor of a step, including
three lines of `ctadl-query-diff/src/parse.rs`; that is why Phase 3 waits for it although it
uses none of its code. Phase 1 changes no behavior: the parser still gives every step no
properties. Phase 2 is the only phase that changes a proof. Phase 3 changes the report for
every pair of files (two new count lines) and is the first phase that can turn `IDENTICAL`
into `DIFFERENT`. Phase 4 changes results only for files whose steps carry the properties,
which no `ctadl query` output does yet. So a changed outcome has one phase to blame.

The phases of one wave run in separate worktrees, each with its own cargo target directory,
so that one phase's build never replaces a test binary the other is running.

Test commands used throughout:

    cargo fmt --all -- --check
    cargo clippy --workspace -- -Dwarnings
    cargo test -p sarif-canon -p ctadl-query-diff -p diff-report -p multiset-diff
    cargo test -p ctadl-ascent --test integration diff_query
    scripts/verus.sh .
    cargo test --workspace
    cargo xtask regression

`scripts/verus.sh .` needs `verus` on `PATH` (the dev shell provides it) and takes about 20
seconds. The `diff_query` filter runs both `tests/integration/diff_query.rs` and the
`diff_query_*` tests of `tests/integration/cli.rs`.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Confirm the gates on the unchanged tree, so a later failure belongs to a phase. No
commit.

**Gate.** All of these pass:

    scripts/verus.sh .
    cargo test -p sarif-canon -p ctadl-query-diff -p diff-report -p multiset-diff
    cargo test -p ctadl-ascent --test integration diff_query
    grep -rn 'admit()\|assume(' sarif-canon/src     # prints nothing

Facts planning already measured on this tree, which later phases rely on:

- `scripts/verus.sh .` verifies with 0 errors.
- `ties.c` queried with `ties.json` has, in every profile, one invocation with
  `executionSuccessful: true` and `exitCode: 0`, and five or six execution notifications,
  each with a `properties` bag and only the keys `descriptor`, `level`, `message` and
  `properties`. At least two are distinct. Three `CTADL0111.call-without-body` notifications
  are equal once `message.text` and `message.arguments` are removed.
- The notifications and status of the renumbered index (`renumbered()`,
  `ctadl-ascent/tests/integration/diff_query.rs:83`) equal those of `ties()` in every
  profile, so `renumbered_ids` (`diff_query.rs:601`) stays identical when notifications are
  compared.

**As built.** All gates passed (Verus 279 verified, 0 errors). Every profile has four
`CTADL0111.call-without-body` notifications: three (`sink_beta`, `sink_omega`, `src_zeta`,
`properties: {calls: 3}`) are equal once `message.text` and `message.arguments` are removed,
and the fourth (`src_alpha`, `{calls: 2}`) differs. The `machine` profile has six
notifications (it adds `CTADL0101.path-generation-disabled`), the others five.

## Phase 1: statements (R14; D1)

Depends on: 0.

**What.** Write what R14 asks to prove, with the step properties in the step, before any of
it is proved. The step types and their views gain the two optional atoms, and the three
definitions that say what a step means gain them too. Proof bodies that stop verifying are
admitted. Smoke tests check the new definitions on concrete steps. No behavior changes: no
caller produces a step with a property yet.

**Files.**
- `sarif-canon/src/types.rs:125`, `:177` — `RawStep` and `CanonStep` gain
  `pub vertex: Option<A>` and `pub callee: Option<A>`, documented as the step's
  `taintVertex` (instruction id already replaced) and `callee`. `:256`, `:284` — the same on
  `RawStepV` and `CanonStepV`. `:356`, `:400` — both `view` functions copy them.
- `sarif-canon/src/canon.rs:56` — `spec_canon_step` copies `vertex` and `callee` in both arms.
- `sarif-canon/src/order.rs:169` — `ord_step` compares `kinds`, then `location`, then
  `vertex`, then `callee`, the last two with `ord_opt_atom` (`order.rs:61`).
- `sarif-canon/src/theorems.rs:86`, `:151` — `step_permuted` and `step_renumbered` each also
  require `x.vertex == y.vertex` and `x.callee == y.callee`.
- Statements that must stay exactly as they are, since they are already stated in terms of
  the definitions above: `lemma_ord_step` (`order.rs:503`), `cmp_step` (`order.rs:1220`),
  `canon_step` (`canon.rs:737`), `copy_step` (`prims.rs:515`), `lemma_step_permuted`
  (`theorems.rs:551`), `lemma_step_renumbered` (`theorems.rs:887`), `theorem_canon_invariant`
  (`theorems.rs:1128`), `theorem_diff_empty_iff_equal` (`theorems.rs:1156`).
- Stub bodies, so the workspace builds: `canon.rs:754` and `prims.rs:523` build the step with
  the two fields copied (the type forces it). `cmp_step`'s body is not changed. Where Verus
  then reports a failure, and only there, put `admit()` in that body (expected: `cmp_step`,
  `lemma_ord_step`). List the admitted functions in the commit message.
- Other constructors of a step, each given `vertex: None, callee: None`:
  `ctadl-query-diff/src/parse.rs:145`, `:563`, `:568`, `:585`; `sarif-canon/src/prims.rs:814`;
  `sarif-canon/src/order.rs:1521`; `sarif-canon/src/smoke.rs:90`, `:221`.

**Tests.** New in `sarif-canon/src/smoke.rs`, each proved with no admitted fact (by
computing the definitions; none may call a lemma admitted in this phase):
- Passes. `smoke_step_props_kept`: `spec_canon_step` of a step with `vertex: Some(40)`,
  `callee: Some(41)` is `Ok` of the canonical step with the same two values.
  `smoke_step_props_ordered`: under `ord_step(ou())`, two steps equal but for `vertex`
  `Some(40)` and `Some(41)` are `Less`; equal but for `vertex` `None` and `Some(40)` are not
  `Equal`; equal but for `callee` are not `Equal`; and kinds `30` and `31` decide before a
  `vertex` that orders the other way.
- Passes, endpoints. `smoke_endpoint_props_compared`: with the endpoints rule, two results
  that differ only in the first step's `vertex` have different `spec_canon_result`; the same
  for the last step's `callee`. `smoke_interior_props_ignored`: with the endpoints rule, two
  results that differ only in an interior step's `vertex` have equal `spec_canon_result`, and
  without the rule they differ.
- Fails (a statement that says too much is caught). `smoke_step_relations_need_props`:
  `!step_permuted(x, y)` and `!step_renumbered(seq![0int, 1], 2, x, y)` for steps `x`, `y`
  equal but for `vertex`; and the canonical runs of two one-result runs that differ only in a
  step's `callee` are not equal, so no rewrite may change a property.
- Not vacuous. `smoke_permuted_bags_holds_props`, `smoke_renumbered_holds_props`: the
  hypotheses of `theorem_canon_invariant` (`permuted_bags`, `renumbered`) hold of the runs of
  `smoke_permuted_bags_holds` and `smoke_renumbered_holds` (`smoke.rs:822`, `:840`) with a
  `vertex` and a `callee` on their steps.
- Existing, still passing unchanged: every other proof in `smoke.rs`;
  `cargo test -p sarif-canon -p ctadl-query-diff`.

**Gate.**

    cargo build --workspace
    cargo test -p sarif-canon -p ctadl-query-diff
    scripts/verus.sh .
    grep -n 'admit()\|assume(' sarif-canon/src/smoke.rs sarif-canon/src/types.rs   # prints nothing
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

**Review.** Stop here. The user checks the changed definitions (`ord_step`,
`spec_canon_step`, `step_permuted`, `step_renumbered`), the list of admitted bodies and the
new smoke tests against R1, R2 and R14 before Wave 2 starts.

**As built** (`6f3dc5968`). Admitted: `lemma_ord_step` (`order.rs:517`) and `cmp_step`, whose
only change is a `proof { admit(); }` at the top (`order.rs:1239`); its comparison code is
untouched. `smoke.rs` gains `calc_*` helpers for steps with properties beside the existing
ones, and `smoke_step_props_kept` also checks a step with a location. Verus: `sarif-canon`
224 verified (was 210), 0 errors.

## Phase 2: comparison and proofs in `sarif-canon` (R1, R2, R14; D1)

Depends on: 1.

**What.** Make the canonical order compare the step properties and finish the proofs. This
phase fills in; it does not restate. It may add helper lemmas, proof blocks and invariants,
and it replaces every stub and `admit()` of Phase 1. It does not change a type, a `spec fn`,
a `requires` or an `ensures`. If a statement turns out false or unprovable, stop and report;
changing it is the user's call. `endpoints_result` (`canon.rs:1061`) is not changed.

**Files.**
- `sarif-canon/src/order.rs:1220` — `cmp_step` compares `vertex`, then `callee`, after
  `kinds` and `location`, as `cmp_props` compares `Props::vertex`.
- `sarif-canon/src/order.rs:503` — `lemma_ord_step`: the key becomes
  `(kinds, location, vertex, callee)`.
- `sarif-canon/src/theorems.rs:551`, `:887` — `lemma_step_permuted`,
  `lemma_step_renumbered`, only if they need a hint.
- `sarif-canon/src/canon.rs`, `sarif-canon/src/prims.rs` — proof hints only, if any body was
  admitted there.
- `sarif-canon/src/order.rs:1369`, `:1413`, `:1521` (unit tests) — the model step `MStep`
  and `m_step` gain the two fields in the same position, and the generator's `step` draws
  each as `self.opt(Self::atom)`, so the model test covers the new comparison.
- Not `types.rs`, not `smoke.rs`.

**Tests.**
- Existing, now covering the properties: the model-against-`cmp_result` tests in
  `sarif-canon/src/order.rs` (the generator now makes steps with properties).
- Verus: every proof of `sarif-canon` verifies with no admitted body.

**Gate.**

    scripts/verus.sh .
    grep -rn 'admit()\|assume(' sarif-canon/src     # prints nothing
    cargo test -p sarif-canon -p ctadl-query-diff
    git diff <phase 1 commit> -- sarif-canon/src/types.rs sarif-canon/src/smoke.rs   # empty
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

The controller also reads `git diff <phase 1 commit> -- sarif-canon/src` and checks that no
`spec fn` body and no `requires` or `ensures` line changed.

**As built** (`22339af01`). Only `order.rs` changed. `lemma_ord_step` is proved through the
key `(kinds, (location, (vertex, callee)))` as `lemma_ord_props` is, with one call to
`lemma_strict_opt`; `cmp_step` needed no hint. The generator change alone did not test the
new comparison (a `cmp_step` that skipped `vertex` or `callee` still passed, since two random
results almost never agree up to a step's properties), so the model test's "change one field
of a copy" case also changes a step's `vertex` or `callee`, through a helper `last_step`;
both broken versions then fail. Verus: `sarif-canon` 224 verified, 0 errors, nothing admitted.

## Phase 3: notifications and statuses (R4–R7, R10, R11; D2, D3, D4)

Depends on: 1 (for `parse.rs` only; it uses none of Phase 1's code).

**What.** `diff-query` reads every invocation's execution notifications and status, compares
each as a multiset of strings, prints two new sections, and counts them in `IDENTICAL` and
the exit code. This changes the report for every input.

Decisions the spec leaves to the plan:
- A notification is taken from `serde-sarif`'s typed `Notification`, as every other field is,
  and turned back into JSON with `serde_json::to_value`. So "everything it holds" is every
  field `serde-sarif` reads; a key SARIF does not define is dropped, as it is on a result.
- Only the notification's own `message.text` and `message.arguments` are removed.
  `ctadl query` writes no other message inside a notification (Phase 0).
- The rows are made during the one parse of the file. The file is not read or parsed twice,
  and the parsed document is still dropped before `read_runs` returns.
- An invocation with no `executionSuccessful` is already a parse error, since `serde-sarif`
  requires the field. That stays.

**Files.**
- `ctadl-query-diff/src/parse.rs:74`, `:79`, `:84` — `read_runs` and `parse_runs` return a
  new `pub struct Parsed { runs: Vec<RawRun<u32>>, notifications: Vec<String>, statuses: Vec<String> }`.
  The two lists are pooled over every invocation of every run and sorted. New private
  functions: `notification_row(&sarif::Notification) -> String` (to JSON, remove
  `message.text` and `message.arguments`, sort object keys at every depth, keep array order,
  print compactly); `status_row(&sarif::Invocation) -> String`
  (`{"executionSuccessful":…,"exitCode":…}`, the `exitCode` key left out when absent). No
  atom is interned for either.
- `ctadl-query-diff/src/driver.rs:44`, `:73`, `:80` — after the results, run `multiset_diff`
  on the two notification lists and on the two status lists with `String`'s order, each
  through its own pair of `Smallest::new(opts.limit)`; the printed row is the string.
  `:19` — `Options::limit`'s comment says it bounds each of the three.
- `ctadl-query-diff/src/report.rs:11` — `Report` gains `notifications_a`, `notifications_b`,
  `notifications_more_in_a`, `notifications_more_in_b`, `invocations_a`, `invocations_b`,
  `invocations_more_in_a`, `invocations_more_in_b`. `:31` — `identical()` is true when
  `results_differ()`, `notifications_differ()` and `invocations_differ()` are all 0. `:50` —
  `write_text` adds, after the results section, `notifications: A n, B m` and its samples
  with unit `notifications`, then `invocations: A n, B m` and its samples with unit
  `invocations`. `:64` — `write_json` adds `notifications_differ` and `invocations_differ`
  beside `results_differ`.
- `ctadl-query-diff/src/lib.rs:1`, `:23` — module comment; export `Parsed`.
- `diff-report/src/lib.rs` — not changed; `write_samples` (`:87`) already takes the unit.

**Tests.**
- Changed, `ctadl-query-diff/src/parse.rs`: `doc()` (`:340`) gains, in its invocation,
  `exitCode`, `exitCodeDescription`, `arguments`, a `toolConfigurationNotifications` entry,
  a `ruleConfigurationOverrides` entry, and two `toolExecutionNotifications` with
  `descriptor.id`, `level`, `message` (`id`, `arguments`, `text`) and `properties`.
  `drops_unread_fields` (`:743`): the removal of `/runs/0/invocations` (`:815`) goes; it
  gains edits that must change nothing: a notification's `message/text` and
  `message/arguments`, the configuration notification, `exitCodeDescription`, `arguments`,
  `ruleConfigurationOverrides` (and `commandLine`, `startTimeUtc`, already there).
  `kept_fields_matter` (`:663`) gains edits that must change the parse:
  `executionSuccessful` to `false`, `exitCode` to `1`, `exitCode` removed, a notification's
  `level`, `descriptor/id`, `message/id`, one property value, and the notification removed.
  `reads_kept_fields` (`:508`) and `reads_file_and_pools_runs` (`:944`) follow the new
  return type; the latter asserts two runs give twice the rows.
- New, `ctadl-query-diff/src/parse.rs`: `notification_row_canonical` — two notifications
  that differ in key order at two depths give one string; two that differ in the order of an
  array inside a property give two. `status_row_absent_exit_code` — absent and `0` differ.
- Existing tests of `ctadl-query-diff/src/tests.rs`, widened: `doc()` (`:70`) takes
  optional invocations.
  `reordered_and_renumbered_is_identical` (`:119`): both files get two notifications; B has
  them reversed, with changed `message.text` and `message.arguments`, and split over two
  invocations with equal status. Still identical.
  `duplicate_counts` (`:151`) also writes a notification twice in B: `[A 1, B 2]` under the
  notifications' `more in B`.
  `small_diff_text` (`:162`): A and B also differ in one notification's `level` and in
  `exitCode`, so the expected text is the whole report in the form of D3, both new sections
  included.
  `limit_bounds_samples` (`:252`): the files also differ in two notifications and two
  statuses per side; each side of each section shows `min(limit, n)` rows, and `limit: 0`
  is still not identical.
  `json_matches_text` (`:292`) also checks `notifications_differ` and `invocations_differ`.
  `endpoints_only` (`:314`): with the flag, a differing notification and a differing status
  are still reported.
- New, `ctadl-query-diff/src/tests.rs`: `status_compared` — `executionSuccessful` flipped,
  `exitCode` changed, `exitCode` absent against `0`, and no invocation against one are each
  `DIFFERENT` with only invocation rows; absent against absent is identical.
- Existing, still passing unchanged: the rest of `tests.rs`, `diff-report`'s tests, and
  `cargo test -p ctadl-ascent --test integration diff_query` (in particular `renumbered_ids`
  and `self_identical`; see Phase 0). If one of them now reports a notification or status
  difference, stop and report the rows; do not weaken the test.

**Gate.**

    cargo test -p ctadl-query-diff -p diff-report
    cargo test -p ctadl-ascent --test integration diff_query
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

**As built** (`bbf499417`). The driver's sampling loop became one generic `differing` helper
used for results, notifications and statuses. `doc()` in `tests.rs` keeps its signature; a
new `invoked(doc, invocations)` adds invocations. In `reordered_and_renumbered_is_identical`,
A also has two invocations (the second with no notifications), since splitting B's
notifications over two invocations would otherwise make the status counts differ. The real
outputs of `renumbered_ids` and `self_identical` show no notification or status difference.

## Phase 4: step properties in the parser and the printer (R1–R3, R8, R12; D1)

Depends on: 2, 3.

**What.** The parser reads `taintVertex` and `callee` from a step's `properties`, and the
printer shows them. Files whose steps have neither compare exactly as before.

**Files.**
- `ctadl-query-diff/src/parse.rs:144` — `step` reads the two keys from
  `s.properties`' `additional_properties` with `string` (`:253`): `taintVertex` through
  `without_call_site` (`:311`), `callee` with `as_is`. An absent key, or no bag, is `None`.
  Only the rewritten string is interned.
- `ctadl-query-diff/src/parse.rs:30`, `:45` — a value that is not a string is
  `Error::Property` with a new field that says the property is a step's, so the message
  reads `step property `taintVertex` is not a string`. The result-property message does not
  change.
- `ctadl-query-diff/src/render.rs:86` — `step` adds, after `location`,
  `"properties":{"taintVertex":…,"callee":…}` with an absent one left out, and no
  `properties` key when both are absent. `:1` — module comment.
- `ctadl-query-diff/src/driver.rs:21` — `Options::endpoints_only`'s comment: the endpoints
  are compared with their properties.

**Tests.**
- Changed, `ctadl-query-diff/src/parse.rs`: in `doc()`, step 0 gains
  `"properties": {"taintVertex": "call-arg(12, 0).f", "callee": "snk", "other": 1}` and
  step 1 `"properties": {"callee": "snk2"}`. `reads_kept_fields` expects step 0's `vertex`
  to be the atom of `call-arg(_, 0).f` and `callee` of `snk`, step 1's `vertex` `None`, and
  no atom for `call-arg(12, 0).f`. `kept_fields_matter` gains: step 0's `taintVertex` to
  `call-arg(12, 1).f` (the formal), to `call-arg(12, 0).g` (the path), its `callee` to
  another name, and each key removed. `drops_unread_fields` gains: step 0's `taintVertex`
  to `call-arg(13, 0).f` (the instruction id), and its `other` property changed.
  `bad_inputs_error` (`:863`) gains a step `taintVertex` of `["x"]` and a step `callee` of
  `3`, each an `Error::Property` naming the key.
- Existing tests of `ctadl-query-diff/src/tests.rs`, widened: `step` (`:10`) takes optional
  step properties.
  `reordered_and_renumbered_is_identical` (`:119`) puts `taintVertex` `call-arg(12, 0)` and a
  `callee` on the steps of A, and `call-arg(99, 0)` in B: still identical.
  `small_diff_text` (`:162`) puts both properties on the differing step, so its expected
  text pins `"properties":{"taintVertex":"call-arg(_, 0)","callee":"snk"}` inside that step
  and no `properties` on the others.
  `endpoints_only` (`:314`) gains: a changed interior `taintVertex` is identical with the
  flag, and a changed first-step `taintVertex` or last-step `callee` is reported with it.
  `errors_are_errors` (`:341`) gains a file whose step `callee` is `3`: `ErrorKind::Parse`.
- New, `ctadl-query-diff/src/tests.rs`: `step_properties_compared` — two files equal but
  for one step's `taintVertex` report one result more in A and one more in B; the same for
  `callee`; a step with the property against the same step without it is reported too.
- Existing, still passing unchanged: the tests of `tests.rs` not named above (their steps
  have no properties), and
  `cargo test -p ctadl-ascent --test integration diff_query` (real output has none either).

**Gate.**

    cargo test -p sarif-canon -p ctadl-query-diff
    cargo test -p ctadl-ascent --test integration diff_query
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

## Phase 5: metamorphic tests on real output (R13, R10)

Depends on: 3, 4.

**What.** Tests only. On the `ties.c` SARIF of every profile, edits that change nothing
compared report nothing, and edits that do are reported as exactly that. The existing tests
are widened to cover the new edits wherever one already makes the same kind of edit; a new
test is written only for an edit no existing test makes.

**Files.**
- `ctadl-ascent/tests/integration/diff_query.rs` — the changes below; the module comment
  (`:1`) lists the new edits. `step_key` (`:153`) is not changed.
- `ctadl-ascent/tests/integration/cli.rs` — one more case in an existing test.

**Tests.** Existing tests of `ctadl-ascent/tests/integration/diff_query.rs`, widened:
- `assert_reported` (`:195`) also asserts that the report has no differing notification and
  no differing status. So every existing "reported as exactly that" test (`deleted_result`,
  `duplicated_result`, `swapped_steps`, `changed_line`, `endpoints_only`) now also checks
  that a result edit touches nothing else. `assert_identical` (`:179`) needs no change:
  `identical()` covers all three.
- `shuffle` (`:296`) also shuffles `runs[0].invocations[0].toolExecutionNotifications`,
  under the key `notifications`; `shuffled_arrays` (`:504`) adds `notifications` to the keys
  it requires to have been reordered. This is the reordered-notifications test.
- `change_messages` (`:628`) also replaces a message's `arguments`, and `changed_messages`
  (`:654`) applies it to the whole run, not only its results, and asserts that it changed at
  least one message under `invocations`. This is the changed-notification-text test.
- A new helper `with_step_props(doc)` gives every step of every flow
  `properties: {"taintVertex": "call-arg(<1000 + k>, <k % 3>).f<k>", "callee": "callee_<k>"}`
  for a running count `k`, so every step differs. `shuffled_arrays`,
  `renumbered_logical_locations` (`:547`), `changed_messages`, `swapped_steps` (`:712`) and
  `endpoints_only` (`:771`) each run over two bases, the real SARIF and
  `with_step_props` of it, written to a temporary file. Their bodies do not change otherwise:
  `step_key` already keeps a step's properties, so `swapped_steps` and the interior-step
  replacement and swap of `endpoints_only` now move step properties too.
- `endpoints_only` gains, on the `with_step_props` base, for each flow of three or more
  steps: the interior step's `taintVertex` changed is reported without the flag and
  identical with it; the last step's `taintVertex` changed is reported with it.

New tests, for edits no existing test makes, each over `PROFILES` and each first asserting
with `assert_changed` that its edit changed the file:
- `changed_notification` — for each execution notification in turn, `level` set to
  `warning`, then one number in its `properties` increased: each is one notification more in
  A and one more in B, and no result or status row. Equal notifications exist (three
  `CTADL0111.call-without-body`), so assert the A row's `count_a == count_b + 1`, not
  `(1, 0)`. Assert every profile has a notification with a property.
- `changed_status` — `executionSuccessful` set to `false`, `exitCode` set to `1`, and
  `exitCode` removed: each is one status more in A and one more in B, and no result or
  notification row.
- `changed_step_property` — on the `with_step_props` base, for each result with a flow: the
  formal of its first step's `taintVertex` changed, then its first step's `callee` changed,
  are each reported through `assert_reported` as 1 and 1; every `call-arg(<id>,` in a step
  property rewritten to `<id> + 7` is identical. Counted against `has_flows` as
  `swapped_steps` does (`:732`). `renumbered_ids` (`:601`) cannot cover this: it compares
  two real indexes, and real output has no step properties yet.

`ctadl-ascent/tests/integration/cli.rs`: `diff_query_exit_independent_of_limit_and_json`
(`:1617`) gains a third file beside `sarif` and `removed`: the fixture with one
notification's `level` changed, expected to exit 1 with `DIFFERENT` under every flag
combination. `sarif_with_a_result_removed` (`:1561`) becomes a helper that takes the edit.

**Gate.**

    cargo test -p ctadl-ascent --test integration diff_query
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

## Phase 6: documents and help text (R15)

Depends on: 3, 4.

**What.** Say what is now compared, in the three places that describe `diff-query`. No
behavior changes. None of them cites a number from the spec or this plan.

**Files.**
- `docs/debugging.md:235` (*Comparing two query outputs*) — steps are compared with
  `taintVertex` and `callee` when present, the instruction id of a `call-arg` vertex aside,
  and `ctadl query` does not write them yet; under `--endpoints-only` the two endpoints keep
  them. Execution notifications (all but `message.text` and `message.arguments`) and each
  invocation's `executionSuccessful` and `exitCode` are compared as multisets over the
  file; what of the invocation is still not compared. Take "invocations" out of the
  not-compared list. The report description, the example and the `--json` example gain the
  `notifications:` and `invocations:` lines and the new JSON keys; regenerate the example
  from a real run rather than editing it by hand. The error list gains a step property that
  is not a string. The exit code is 0 only when all three multisets are equal.
- `docs/spec.md:88` (F12) and `:255` (the `diff-query` bullet) — the comparison covers
  results, execution notifications and invocation status; the metamorphic bullet names the
  new edits (reordered notifications, changed notification text, level and property, changed
  status, step properties).
- `ctadl-ascent/src/main.rs:87` — the `diff-query` help: what is compared now, that
  `ctadl query` does not write the step properties yet, the two new report sections, and
  the exit code. `:153` — `--limit` bounds each of results, notifications and invocations.
  `:161` — `--endpoints-only` leaves notifications and statuses as they are.

**Tests.**
- Existing, still passing: `diff_query_parses` (`ctadl-ascent/src/main.rs:1554`).
- `ctadl diff-query --help` prints the new text.

**Gate.**

    cargo build -p ctadl-ascent --bin ctadl
    cargo test -p ctadl-ascent diff_query_parses
    grep -nE '\b(R|D|P)[0-9]+\b|Phase [0-9]' docs/debugging.md ctadl-ascent/src/main.rs   # no new hit
    cargo fmt --all -- --check && cargo clippy --workspace -- -Dwarnings

## Phase 7: TaintBench, run twice (R10)

Depends on: 5, 6 (it measures the finished branch).

**What.** On real Android apps, two independent runs of `ctadl` on the same inputs give query
outputs that `diff-query` marks `IDENTICAL`, now that notifications, statuses and step
properties are compared. Measure how long `diff-query` takes and how much memory it uses.
No code changes; the results go in this phase's "As built" note, which is the commit.

**Inputs.** The 38 apps of `~/proj/ct-bench/taintbench/apps/<app>/`, each with `app.json`
(the APK's URL and SRI hash) and `model.json`. That checkout is read only: nothing is built
or written in it. Each APK is fetched into the Nix store by its hash
(`nix store prefetch-file --json --hash <sri> <url>`), which finds the copy already there.
`ctadl` is the release build of this branch (`cargo build --release -p ctadl-ascent --bin
ctadl`), in this phase's own target directory.

**Steps.** For each app, twice (runs `r1` and `r2`), each run with its own
`XDG_STATE_HOME` under `$TMPDIR`, the steps `cargo xtask taintbench` uses
(`ct-bench/xtask/src/taintbench.rs:282`):

    ctadl import -l apk -n <app>_tb <apk>
    ctadl index <app>_tb -m model.json
    ctadl query <app>_tb -m model.json -o <profile>.sarif --sarif-profile <profile>

with `<profile>` each of `human`, `agent`, `machine` and `debug` from the same index. Then,
for each app and profile:

    ctadl diff-query r1/<profile>.sarif r2/<profile>.sarif
    ctadl diff-query --endpoints-only r1/<profile>.sarif r2/<profile>.sarif

Apps run one at a time, each `ctadl` command under the `memory-guard` skill's
`memguard.sh` (`xbot_android_samp` peaked at 5.1 GB and `remote_control_smack` took about two
minutes in the 2026-09-22 timings, `ct-bench/taintbench-timing-20260922.tsv`). An app whose
import, index or query fails or is killed is recorded with the failure and skipped; it is not
a `diff-query` finding.

**Measured, per app and profile:** the verdict and exit code of each `diff-query`; the
result, notification and invocation counts it prints; its wall time and peak memory footprint
(`/usr/bin/time -l`); the SARIF size. Kept as a TSV under `$TMPDIR`, summarized in the note.

**Gate.**
- Every app that queried in both runs gives `IDENTICAL` and exit 0 in every profile, with and
  without `--endpoints-only`.
- `git status --short` in `~/proj/ct-bench` is unchanged by the phase.

If an app gives `DIFFERENT`, stop and report its differing rows and which section they are
in. It is either `ctadl` not being deterministic run to run (a finding about `index` or
`query`, outside this change) or `diff-query` comparing something it should not; telling
which is the user's call, and no code changes in this phase.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `sarif-canon/src/types.rs` | step types and views gain `vertex`, `callee` | 1 |
| `sarif-canon/src/canon.rs` | `spec_canon_step` and `canon_step` copy them | 1 (hints: 2) |
| `sarif-canon/src/order.rs` | `ord_step` | 1 |
| `sarif-canon/src/order.rs` | `cmp_step`, `lemma_ord_step`, model test | 2 |
| `sarif-canon/src/theorems.rs` | `step_permuted`, `step_renumbered` | 1 (hints: 2) |
| `sarif-canon/src/prims.rs` | `copy_step`, a test constructor | 1 |
| `sarif-canon/src/smoke.rs` | new smoke tests | 1 |
| `ctadl-query-diff/src/parse.rs` | step constructors get `None` | 1 |
| `ctadl-query-diff/src/parse.rs` | `Parsed`, notification and status rows, tests | 3 |
| `ctadl-query-diff/src/parse.rs` | step properties, error, tests | 4 |
| `ctadl-query-diff/src/driver.rs` | two more multiset diffs | 3 (a comment: 4) |
| `ctadl-query-diff/src/report.rs` | counts, samples, sections, JSON keys | 3 |
| `ctadl-query-diff/src/lib.rs` | module comment, export | 3 |
| `ctadl-query-diff/src/render.rs` | a step's `properties` | 4 |
| `ctadl-query-diff/src/tests.rs` | new and changed tests | 3, 4 |
| `ctadl-ascent/tests/integration/diff_query.rs` | existing metamorphic tests widened, three new | 5 |
| `ctadl-ascent/tests/integration/cli.rs` | one more case in an existing test | 5 |
| `docs/debugging.md`, `docs/spec.md` | what is compared | 6 |
| `ctadl-ascent/src/main.rs` | help text only | 6 |

Not touched: `ctadl-ascent/src/query_engine/formatter.rs` and `ctadl-import/src/sarif.rs`
(R9; writing the properties is `intent2.md`); `sarif-canon/src/sort.rs`,
`sarif-canon/src/u32_atoms.rs` and `endpoints_result` in `canon.rs` (D1); `diff-report` and
`multiset-diff` (D2, D3); `scripts/verus.sh` and `flake.nix`; `nightly/README.md`, which
names the proofs without saying what a step holds.

## Acceptance

- `cargo test --workspace`, `cargo fmt --all -- --check` and
  `cargo clippy --workspace -- -Dwarnings` pass.
- `scripts/verus.sh .` verifies with 0 errors, and
  `grep -rn 'admit()\|assume(' sarif-canon/src` prints nothing (R14).
- `cargo xtask regression` passes with no golden changed, and
  `git diff main -- ctadl-ascent/src/query_engine ctadl-import` is empty (R9).
- `git diff <phase 1 commit> -- sarif-canon/src/types.rs sarif-canon/src/smoke.rs` is empty,
  and no `spec fn`, `requires` or `ensures` of `sarif-canon` changed after Phase 1.
- `ctadl diff-query a.sarif a.sarif` on a real query output prints `IDENTICAL` and the three
  count lines, and exits 0 (R10, R11).
- No code, comment, help text or document cites a number from `spec.md` or this plan (R15).
- On TaintBench, two runs of the branch's `ctadl` give `IDENTICAL` query outputs for every
  app that ran, in every profile, with and without `--endpoints-only` (Phase 7).

## Deferred

- Writing `taintVertex` and `callee` on steps in `ctadl query`: `intent2.md`. If it settles
  on a list for either property, the reader of Phase 4 changes with it (D1).
