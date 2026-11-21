# Critical-call join once: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the reorder of `critical_reach` (D4), the
derivation of rules 1.2 and 2.1 from `critical_arg`/`critical_reach` (D1, D2), and the
four-configuration measurement (R6, R7). R*/D* numbers are the spec's; the spec wins where the
two disagree. Each phase builds, passes the gate named in it, and is one commit.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline outputs on the unchanged tree: regression indexes, app indexes, relation dumps |
| 1 | 1 | D4 alone: a body reorder, no result change; adds the fixture snapshot test |
| 2 | 2 | D1 + D2 on top of 1; same file as 1, and its snapshot test is the guard |
| 3 | 3 | measures the four configurations; needs the commits of 1 and 2 |
| 4 | 4 | keeps the winning configuration (R7) and closes |

All the rule changes are in one file, `ctadl-index/src/index_engine/mod.rs`, so phases 1 and
2 are sequential. D4 goes first because it is a pure reorder of one rule body (R8 forbids any
other change to `critical_reach`), and the snapshot test it adds is recorded before any rule
changes, so phase 2's result-preservation is checked against numbers taken from the unchanged
rules. D1 and D2 share one phase because the spec's *derive* configuration is the two
together, they edit adjacent rules, and D2's invariant check is part of the same change.

The linear history gives three of the four configurations as commits: *baseline*
(eef8b971e, code-identical to the branch's spec commits), *reorder* (phase 1), *both*
(phase 2). Phase 3 builds *derive* from a throwaway commit: phase 2's diff cherry-picked onto
eef8b971e, never pushed to the branch.

Test commands used throughout:

    cargo test --workspace
    cargo xtask regression --release
    ctadl diff-index <baseline-index-dir> <new-index-dir>     # exit 0 = identical

`ctadl` here is the release binary of the tree under test
(`cargo build --release -p ctadl-ascent --bin ctadl`). Before building, check that no running
process uses the target dir's binary (`/bin/ps -axo pid,command | grep <target-dir>`).

**Regression index-diff loop** (used as a gate). `cargo xtask regression` keeps its scratch
root (`$TMPDIR/ctadl_xtask_<pid>`, printed as `Scratch root:` at the end). Every index
directory under it is a directory holding `index_config.json`. For each one, run
`ctadl diff-index <baseline-root>/<rel> <new-root>/<rel>`; the gate passes when every pair
exits 0 and no index exists on one side only. Phase 0 records the baseline root. Write the
loop as a small script in a scratch directory, not in the repo.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record the "before" outputs on the unchanged tree (HEAD of this branch, whose code
is eef8b971e). No commit: this phase writes nothing in the repo. Its outputs live outside the
repo, under the measurement root on the big disk; note their paths in the session, not in
any committed file.

1. **Regression.** `cargo xtask regression --release`. Record the scratch root. Note the
   pass/fail counts; later phases must match them.
2. **Instrumented baseline binary.** With the `datalog-blowups` skill,
   `build-instrumented.sh <repo> eef8b971e`. Keep the printed binary path.
3. **App indexes and baseline timings.** Index the four apps of the spec's measurement table
   (DuckDuckGo 52921000, Nextcloud 350000051, Wikipedia 50603, NewPipe 1015) with that binary,
   exactly as the spec's *Measured cost* setup: `ctadl index -j 1 -m
   ct-bench/bench/suites/android-privacy.json`, Dex only (`--no-native-libs`), no timeout,
   under the skill's `memguard`/`sweep.sh` with the same cap the spec's runs used, two apps at
   a time (DuckDuckGo with Nextcloud, Wikipedia with NewPipe). Reuse the stores the spec's
   runs prepared. Keep each run's `index.log`, `profile.json`, and the index directory (copy
   it aside: a later index of the same project overwrites it).
4. **Relation dumps.** Re-index Wikipedia and NewPipe once more with
   `CTADL_DUMP_RELATIONS=<dir>` set (a separate dir per app). Keep the dumps of
   `critical_summary`, `critical_arg`, `critical_reach`, `establishes_direct`,
   `establishes_via` and `resolvent`.

**Gate.** All four apps report `reached_fixpoint: true`; the regression scratch root, the four
index directories, the four profiles and the two dumps exist.

## Phase 1: `critical_reach` as a simple join (D4, R9, R8)

Depends on: 0.

**What.** Reorder the body of `critical_reach` so that `critical_arg` and `locals` are its first
two items, adjacent, with every filter after them, exactly as spec D4 writes it. Nothing else
in the rule changes; no other rule changes. No result change. Also add a snapshot test of the
hybrid-inlining relation sizes, recorded on the tree *before* the reorder, that phase 2 relies
on.

**Files.**
- `ctadl-index/src/index_engine/mod.rs:1994` — the `critical_reach` rule: body order only.
- `ctadl-ascent/src/index_engine_tests.rs` — the new test below. The fixtures `SRC`
  (`:56`) and `SRC_NESTED_TARGET` (`:345`), and the helpers `program_from_string` (`:19`) and `index_program` (`:32`) are reused.

**Tests.**
- New, `ctadl-ascent/src/index_engine_tests.rs`: `hybrid_relation_sizes_snapshot`. It indexes
  `SRC`, `SRC_NESTED_TARGET` and a new fixture, `SRC_PREFIX`, and asserts exact values of
  `stats.hybrid_critical_summary`, `stats.hybrid_resolvent`, `stats.hybrid_context_assign`,
  `stats.hybrid_context_locals` and `stats.hybrid_context_summary`
  (`ctadl-index/src/index_engine/mod.rs:2878`) for each. `SRC_PREFIX` adds a forwarder that
  passes a struct pointer whole to a callee that calls through one of its fields, and a
  caller that stores a target in that field before calling the forwarder:

  ```c
  struct ops { int (*f)(int); int tag; };
  int id(int p) { return p; }
  int twice(int p) { return p + p; }
  int through(struct ops *o, int x) { return o->f(x); }
  int fwd(struct ops *o, int x) { return through(o, x); }
  int top(int a, int b) {
      struct ops o; o.f = twice; o.tag = a;
      struct ops q; q.f = id;
      return fwd(&o, a) + fwd(&q, b);
  }
  ```

  so a critical summary crosses a split with a non-empty `rest`, and two callers establish
  different decisions through the same forwarder. Record the expected numbers by running the
  test on the unchanged tree first (the commit's diff shows the test passing before and after
  the reorder). Assert each fixture yields `hybrid_resolvent > 0`, so the snapshot cannot pass
  vacuously.
- Existing, still passing unchanged: `parallel_engine_matches_serial`
  (`ctadl-ascent/src/index_engine_tests.rs:75`) and `lsm_storage_matches_memory` (`:106`):
  the reorder is compiled under all three engines (`ctadl-index/engines/{serial,par,lsm}`).
- Regression: identical to phase 0 (same pass/fail counts; the index-diff loop passes).
- R9: build the instrumented binary of this commit (`build-instrumented.sh <repo> HEAD`),
  index NewPipe with it, and check in `index.log` that the `critical_reach` rule's summary
  line carries `[SIMPLE JOIN]` and that none of its variants names `locals_indices_none`.
- **Cost risk to watch.** A simple join iterates whichever side `len_estimate()` calls
  smaller (`ascent_codegen.rs:1235` in the patched macro). For `locals` that estimate is the
  number of `(g, arg)` groups with `Always` rows (`ctadl-index/src/index_engine/cond_trie.rs:623`),
  which undercounts the rows. When `locals` is the iterated side, its 0_1_2 index iterates
  through `iter_by_path` (`cond_trie.rs:375`), which copies every row of that side into a
  temporary hash map; the parallel store does the same (`c_cond_trie.rs:437`). Against the
  `locals` delta that is linear and fine. Against total `locals` it is no slower
  asymptotically than today's full scan, but allocates in proportion to `locals` (12.3 M rows
  on DuckDuckGo). On the NewPipe run, compare `critical_reach`'s per-variant times with
  phase 0's and record peak footprint from `sweep.tsv`; phase 3 does the same on all apps.
  Decided: measure only. This phase does not change `cond_trie.rs`; the streaming fix
  stays under Deferred.

**Gate.**

    cargo test --workspace
    cargo xtask regression --release      # then the regression index-diff loop against phase 0
    grep 'critical_reach <--' <newpipe-index.log>   # [SIMPLE JOIN]; no locals_indices_none

## Phase 2: 1.2 and 2.1 from `critical_reach` and `critical_arg` (D1, D2, R2, R3, R4)

Depends on: 1.

**What.** Replace the two 1.2 rules (over `call` and `fnptr_call`) with D1's one-clause
projection of `critical_reach`, and the two 2.1 rules with D2's join of `critical_arg` (empty
`rest`) with `call_target_assign_like`, written exactly as the spec does so that Ascent
compiles it as a simple join. Add the debug assertion of D2's invariant. Results must not
change (D3: same least model). `critical_arg`, `critical_reach`, `establishes_direct`,
`establishes_via`, `resolvent`, 1.1 and 3.1 keep their text (R4, R8).

**Files.**
- `ctadl-index/src/index_engine/mod.rs:1923-1945` — the two 1.2 rules become D1's rule.
  Rewrite their comment: 1.2 is now the projection of `critical_reach`; keep the explanation
  of the prefix split, which now lives with `critical_arg`.
- `ctadl-index/src/index_engine/mod.rs:1947-1961` — the two 2.1 rules become D2's rule. Its
  comment says why the empty-`rest` split suffices (`key` is then `p`) and that it relies on
  every `critical_summary` path being a `path_set` member.
- `ctadl-index/src/index_engine/mod.rs:1966-1979` — the comment above `critical_arg`, which
  says "as in 1.2": point it at its new role (it feeds 1.2, 2.1 and 2.2). Comment only.
- `ctadl-index/src/index_engine/mod.rs:2740` (`run_engine!`, right after `run_timeout`) — the
  D2 invariant as the spec states it (a member or ε): under `debug_assertions`, iterate
  `prog.critical_summary.iter()` (a plain relation, so this works for both `Vec` and
  `boxcar::Vec`; don't use the `Rows` trait, whose `StreamingIterator` import is private to
  `ctadl-index` and absent from the engine crates that expand the macro) and assert every
  path `p` satisfies `path_set.contains(p) || p.is_empty()`, naming the first offender.
- `ctadl-index/src/index_engine/path_set.rs` (tests module) — the unit test below.

Code comments must not cite the spec's or this plan's numbers (rule numbers 1.2/2.1 are the
code's own and may stay).

**Tests.**
- New, `ctadl-index/src/index_engine/path_set.rs`: `exact_splits_end_with_whole_path` — for a
  set built from ε, `a`, `a.b` and `a.b.[3]`, every member's `splits(p).exact` contains
  `(p, ε)` exactly once, with `key == p` as `Path` equality, and a non-member's exact splits
  are empty. This is the fact D2 rests on (`Path::prefix_keys`, `ctadl-index/src/facts.rs:420`).
- Existing, still passing **unchanged**: `hybrid_relation_sizes_snapshot` (phase 1); its
  numbers must not be edited in this phase. Also `parallel_engine_matches_serial`,
  `lsm_storage_matches_memory`, and every other `cargo test` test. Debug-build tests now run
  the invariant assertion; a failure there means D2 would lose rows and is a stop-and-report,
  not a test to relax.
- Regression: identical to phase 0 (same counts; the index-diff loop passes).
- Relation dumps: index Wikipedia and NewPipe with this commit's instrumented binary and
  `CTADL_DUMP_RELATIONS`; the six dumped relations of phase 0 step 4 are equal as row sets.
- Memory: D1's one-clause rule binds nothing, so Ascent adds `critical_reach_indices_none`,
  a full copy of `critical_reach` (222 K rows on DuckDuckGo; about +50% on that relation).
  Accepted; dropping old 1.2 frees `critical_summary_indices_0`. Phase 3 records peak
  footprint.
- Greps (R2, R3): in the index program no rule body pairs `call(` or `fnptr_call(` with
  `critical_summary(` other than `critical_arg`'s two rules.

**Gate.**

    cargo test --workspace
    cargo xtask regression --release      # then the regression index-diff loop against phase 0
    # dumps of Wikipedia and NewPipe equal phase 0's for the six relations

## Phase 3: measure four configurations (R1, R5, R6, R9)

Depends on: 2.

**What.** Measure *baseline* (eef8b971e), *derive* (phase 2's diff cherry-picked onto
eef8b971e on a detached throwaway worktree; never committed to the branch), *reorder*
(phase 1's commit) and *both* (phase 2's commit). No code change on the branch; the commit
of this phase adds the results table to `plan.md` under this phase as an "As built" note
(numbers only, no machine paths).

For each configuration, build the instrumented binary with `build-instrumented.sh`. Index the
four apps with it under exactly the phase 0 setup (same flags, cap, pairing, stores). Run each
configuration **twice**, interleaving configurations (baseline, derive, reorder, both, then
again) so that drift over the session spreads evenly; one heavy job pair at a time, nothing
else building. Run-to-run noise for an app is the largest `|run1 − run2|` of its
`fixpoint_secs` across the four configurations.

Per configuration and app, report (from `profile.json` and `index.log`, summarized with the
spec's `summ.py`): `reached_fixpoint`, `fixpoint_secs`, SCC iterations, and the summed time
over all semi-naive variants of the rules for `critical_summary`, `establishes_direct`,
`critical_arg` and `critical_reach`, plus peak footprint (`sweep.tsv`). Also report the rules
whose full scans run once per iteration with a non-empty delta, because D1 spreads the
hybrid deltas over about three times as many rounds (D3): the 3.2 `assign_like` rules under
`ContextJoin::Sets` (`ctadl-index/src/index_engine/mod.rs:2117`, `:2126`, which scan all of
`summary`), and 3.1 `call_site` and `resolved_call` (`mod.rs:2013`, `:2027`). For *reorder* and
*both*, also the `critical_reach` summary line (R9) and its per-variant times, naming any
variant that iterates `locals_indices_0_1_2_total` (phase 1's cost risk).

Check R1 for every non-baseline configuration: `ctadl diff-index` of each app's index against
phase 0's exits 0 (use the release `ctadl` of phase 2; the index format is unchanged).

**Gate.** All 4 × 4 × 2 runs reach the fixpoint (R5); every diff-index exits 0 (R1); R9 holds
for *reorder* and *both*; the table is complete.

## Phase 4: keep the winner (R7)

Depends on: 3.

**What.** Apply R7 to phase 3's table: keep the configuration with the lowest `fixpoint_secs`
on DuckDuckGo and Nextcloud whose `fixpoint_secs` rises on none of the four apps by more than
the phase 3 noise. If the two big apps disagree on the lowest, take the lowest sum over the
two. Then make the branch hold exactly that configuration:

- *both*: nothing to revert.
- *reorder*: `git revert` phase 2's commit.
- *derive*: `git revert` phase 1's commit (the snapshot test it added stays; restore it in the
  revert).
- none beats baseline: revert phases 2 and 1 (keep the snapshot test).

Record the decision and the reason in an "As built" note on this phase.

**Gate.**

    cargo test --workspace
    cargo xtask regression --release      # then the regression index-diff loop against phase 0

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `ctadl-index/src/index_engine/mod.rs` | `critical_reach` body reorder | 1 |
| `ctadl-ascent/src/index_engine_tests.rs` | `hybrid_relation_sizes_snapshot`, `SRC_PREFIX` | 1 |
| `ctadl-index/src/index_engine/mod.rs` | 1.2 and 2.1 replaced; comments; invariant assertion | 2 |
| `ctadl-index/src/index_engine/path_set.rs` | `exact_splits_end_with_whole_path` | 2 |
| `plan.md` | measurement table, decision | 3, 4 |

Not touched: `docs/spec.md` (§4.2 describes hybrid inlining above the rule level, and neither
change alters what it says); `ctadl-index/src/index_engine/dump.rs:409` (every dumped relation
still exists); the engine crates `ctadl-index/engines/*` (they include the same rule source);
rules 1.1, `establishes_via`, `resolvent`, 3.1 (R8); site resolution (out of scope per the
intent: at most 1.1–1.7 % of the fixpoint).

## Deferred

- Stream the `locals` 0_1_2 index's `iter_all` over the trie's groups instead of
  materializing it (`cond_trie.rs:375`, `c_cond_trie.rs:437`), and make its `len_estimate`
  count keys. Only if phase 3 shows `critical_reach` iterating total `locals`.
- Make `critical_summary(g, n2, p2r)` a second head of `critical_reach` instead of D1's
  separate rule: no `critical_reach_indices_none`, and two rounds per hop instead of three.
  It edits `critical_reach`'s text, which R8 forbids.

## Acceptance

- `cargo test --workspace` and `cargo xtask regression --release` pass, with the regression
  index-diff loop identical to phase 0 (R1).
- Phase 3's table covers R6 in full; every run reached the fixpoint (R5); every app index
  diffs identical to baseline (R1); R9's summary line check holds for *reorder* and *both*.
- The branch holds the configuration phase 4 chose, by R7.
- If *derive* or *both* is kept: `grep -n 'call(\|fnptr_call(' ctadl-index/src/index_engine/mod.rs`
  shows no rule that pairs either with `critical_summary(` except `critical_arg`'s two (R2, R3),
  and `critical_arg`, `critical_reach`, `establishes_direct` keep their column lists (R4).
