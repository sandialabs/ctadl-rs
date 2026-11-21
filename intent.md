# Intent: Do the critical-call join once - DO-NOT-MERGE

## Problem

Two hybrid-inlining joins in `ctadl-index/src/index_engine/mod.rs` are computed more than once, and each copy pairs multi-relation scans that a materialized helper would do once. Only the first is in scope (see *Out of scope*).

1. **Critical-summary propagation.** The two rules that lift `critical_summary` from a callee to its callers (over `call` and `fnptr_call`) re-pair calls with argument vertices, split the path, and probe `locals`. That is the same join `critical_arg` and `critical_reach` already compute for rule 2.2, with the same `Always` filter and the same path concatenation. Rule 2.1 (`establishes_direct`) also rebuilds the call/argument pairing that `critical_arg` holds.
2. **Site resolution.** Rule 3.1's `call_site` and the `resolved_call` rule beside it run the same four-way join, `callee_info` × `locals` × `resolvent` × `callee_resolvents`. Every new `locals` row at a receiver or new resolvent pays for that join twice.

### Measured cost

Setup: `ctadl index -j 1 -m ct-bench/bench/suites/android-privacy.json` at eef8b971e, with the per-rule-timing `ascent_macro` from the `datalog-blowups` skill. Inputs are Dex only (`--no-native-libs`). Each pair of apps ran concurrently, and every run reached the fixpoint. Times are rule seconds summed over all semi-naive variants, with each as a share of `fixpoint_secs`. Static numbers come from `ctadl report`.

| app | functions | virtual sites (≥2 targets) | `critical_summary` rows | `resolvent` rows | fixpoint |
| --- | ---: | ---: | ---: | ---: | ---: |
| DuckDuckGo 52921000 | 459 K | 617 K (236 K) | 197 K | 77 K | 183.7 s |
| Nextcloud 350000051 | 371 K | 596 K (210 K) | 128 K | 74 K | 138.0 s |
| Wikipedia 50603 | 84 K | 237 K (65 K) | 30 K | 28 K | 32.5 s |
| NewPipe 1015 | 83 K | 177 K (43 K) | 21 K | 22 K | 28.5 s |

| rule (all variants) | DuckDuckGo | Nextcloud | Wikipedia | NewPipe |
| --- | ---: | ---: | ---: | ---: |
| 1.2 `critical_summary` over `call`/`fnptr_call` | 35.5 s (19.3%) | 9.8 s (7.1%) | 1.0 s (3.1%) | 0.9 s (3.3%) |
| 2.1 `establishes_direct` | 11.9 s (6.5%) | 3.1 s (2.2%) | 0.3 s (0.9%) | 0.3 s (1.2%) |
| 3.1 `call_site` (four-way join) | 2.6 s (1.4%) | 2.3 s (1.6%) | 0.4 s (1.1%) | 0.3 s (1.2%) |
| `resolved_call` twin of 3.1 | 2.7 s (1.5%) | 2.4 s (1.7%) | 0.4 s (1.1%) | 0.3 s (1.2%) |
| **problem joins, total** | **52.6 s (28.7%)** | **17.5 s (12.7%)** | **2.0 s (6.2%)** | **1.9 s (6.8%)** |
| `critical_reach` (kept, for comparison) | 49.0 s (26.7%) | 34.2 s (24.8%) | 9.9 s (30.4%) | 9.2 s (32.4%) |
| `critical_arg` (kept, for comparison) | 0.08 s | 0.06 s | 0.02 s | 0.01 s |

What the profiles show:

- Almost all of 1.2's time goes to a single variant, `call_indices_2_total × critical_summary_indices_0_total × locals_indices_0_1_2_delta`: 35.3 of 35.5 s on DuckDuckGo. Every iteration re-pairs all of `call` with all of `critical_summary` and redoes the splits, only to probe that iteration's `locals` delta. `establishes_direct` has the same shape, with `call_target_assign_like` delta last (11.8 of 11.9 s). `critical_arg` already holds that pairing, and building it costs under 0.1 s.
- The duplicated four-way join in 3.1 costs about the same as the join it duplicates. Sharing it would save at most the `resolved_call` twin's 0.3–2.7 s (about 1–1.7%).
- `critical_reach` is the biggest hybrid-inlining rule on every app (25–32%). Nearly all of that is the variant `locals_indices_none_total × critical_arg_indices_0_1_2_delta`, a full scan of `locals` total on every iteration that has a new `critical_arg` row. Change 1 makes `critical_summary` depend on `critical_reach`, so `critical_arg` gets new rows in more iterations. Each of those iterations pays for another full `locals` scan. That could cost more than the 1.2 time it saves, so measure `critical_reach` as well as the rules being removed.

Logs and profiles: `/Volumes/Shampoo/blowups/critjoins/run-*/t*/{index.log,profile.json}`. Summarize with `summ.py` there.

## Change

Derive `critical_summary` propagation from `critical_reach`, and `establishes_direct` from `critical_arg` (the split with an empty rest), so the call/argument pairing and the `locals` probe happen once.

Measure the rule profiles of the affected rules, `critical_reach` included, before and after on a workload where hybrid inlining is busy. Keep the change only if it reduces work.

## Out of scope

Site resolution (problem 2). A `site_resolution(caller, insn, target, decision)` helper shared by `call_site` and `resolved_call` would save at most the `resolved_call` twin's time, 1.1–1.7% of the fixpoint in the measurements above.

## Constraints

- The index results must not change. Use `index-diff` as a gate that must pass.
- Keep every key that distinguishes call sites, formals, paths and decisions, so that a conditional callback's effect never becomes unconditional for every caller of its builder.
- Do not regress fixpoint termination. `critical_reach` grows with `critical_summary`, so check that feeding one from the other converges.
