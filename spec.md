# Critical-call join once: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This spec refines that intent's single change: derive the hybrid-inlining rules 1.2 and 2.1 from `critical_arg` and `critical_reach`. It must stay consistent with `docs/spec.md` §4.2, which describes call resolution. It does not sequence the work.

## Background

All rules below are in the index program in `ctadl-index/src/index_engine/mod.rs`.

- **1.2** (`:1926`, `:1936`) lifts `critical_summary(tgt, n_tgt, p_tgt)` to each caller. For every `call` or `fnptr_call` edge and every exact split `p_tgt = key · rest`, it probes `locals(caller, arg, key, r, p_n, c)`. When `c` is `Always` and `r` is a formal `n`, it derives `critical_summary(caller, n, p_n · rest)`.
- **2.1** (`:1950`, `:1956`) derives `establishes_direct(f, d, caller, insn)` from `critical_summary(f, n, p)`, a call edge into `f`, and `call_target_assign_like(caller, arg, p, cto)`.
- **`critical_arg`** (`:1980`) holds every (call edge, `critical_summary` row, exact split) as `(g, arg, key, rest, p, f, insn, n)`. **`critical_reach`** (`:1994`) joins it with `locals` on `(g, arg, key)`, under the same `Always` and formal filters and the same `ps.concat(p2, None, rest)` as 1.2. So projecting `critical_reach` to `(g, n2, p2r)` gives exactly the rows that 1.2 derives.
- `PathSet::splits` (`ctadl-index/src/index_engine/path_set.rs:185`) yields `(p, ε)` among a member's exact splits (`Path::prefix_keys`, `ctadl-index/src/facts.rs:420`). A non-member has no splits at all.
- **How Ascent plans a join.** Each body clause becomes a nested loop in written order, and a clause with no bound columns is a full scan. Ascent compiles a rule as a *simple join* only when its first two body items are adjacent clauses: plain-variable arguments, no `if let` on the first clause, and no variable repeated within the second. A simple join iterates whichever of the two relations is smaller at run time and probes the other. See `ascent_hir.rs:343-463` and `ascent_codegen.rs:1217` in the patched `ascent_macro` 0.8.0. `critical_reach` puts `if`s between `locals` and `critical_arg`, so it is not a simple join. Its `critical_arg`-delta variant therefore scans all of `locals` total.

### Measured cost

Setup: `ctadl index -j 1 -m ct-bench/bench/suites/android-privacy.json` at eef8b971e, with the per-rule-timing `ascent_macro` from the `datalog-blowups` skill. Inputs are Dex only (`--no-native-libs`). Each pair of apps ran concurrently, and every run reached the fixpoint. Times are rule seconds summed over all semi-naive variants, with each as a share of `fixpoint_secs`. Static numbers come from `ctadl report`.

| app | functions | virtual sites (≥2 targets) | `critical_summary` rows | `resolvent` rows | SCC iterations | fixpoint |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| DuckDuckGo 52921000 | 459 K | 617 K (236 K) | 197 K | 77 K | 1627 | 183.7 s |
| Nextcloud 350000051 | 371 K | 596 K (210 K) | 128 K | 74 K | 787 | 138.0 s |
| Wikipedia 50603 | 84 K | 237 K (65 K) | 30 K | 28 K | 320 | 32.5 s |
| NewPipe 1015 | 83 K | 177 K (43 K) | 21 K | 22 K | 475 | 28.5 s |

| rule (all variants) | DuckDuckGo | Nextcloud | Wikipedia | NewPipe |
| --- | ---: | ---: | ---: | ---: |
| 1.2 `critical_summary` over `call`/`fnptr_call` | 35.5 s (19.3%) | 9.8 s (7.1%) | 1.0 s (3.1%) | 0.9 s (3.3%) |
| 2.1 `establishes_direct` | 11.9 s (6.5%) | 3.1 s (2.2%) | 0.3 s (0.9%) | 0.3 s (1.2%) |
| 3.1 `call_site` (four-way join, out of scope) | 2.6 s (1.4%) | 2.3 s (1.6%) | 0.4 s (1.1%) | 0.3 s (1.2%) |
| `resolved_call` twin of 3.1 (out of scope) | 2.7 s (1.5%) | 2.4 s (1.7%) | 0.4 s (1.1%) | 0.3 s (1.2%) |
| `critical_reach` | 49.0 s (26.7%) | 34.2 s (24.8%) | 9.9 s (30.4%) | 9.2 s (32.4%) |
| `critical_arg` | 0.08 s | 0.06 s | 0.02 s | 0.01 s |

On DuckDuckGo the relations hold: `locals` 12.3 M, `call_target_assign_like` 4.5 M, `call` 1.3 M, `critical_arg` 516 K, `critical_reach` 222 K.

What the profiles show:

- Almost all of 1.2's time goes to a single variant, `call_indices_2_total × critical_summary_indices_0_total × locals_indices_0_1_2_delta`: 35.3 of 35.5 s on DuckDuckGo. Every iteration re-pairs all of `call` with all of `critical_summary` and redoes the splits, only to probe that iteration's `locals` delta. 2.1 has the same shape, with `call_target_assign_like` delta last (11.8 of 11.9 s). `critical_arg` already holds that pairing, and building it costs under 0.1 s.
- Nearly all of `critical_reach`'s time is the variant `locals_indices_none_total × critical_arg_indices_0_1_2_delta` (48.2 of 49.0 s on DuckDuckGo). That is a full scan of `locals` on every iteration that has a new `critical_arg` row.

Logs and profiles: `/Volumes/Shampoo/blowups/critjoins/run-*/t*/{index.log,profile.json}`. Summarize with `summ.py` there.

## Requirements

- R1. **Results unchanged.** On each of the four apps above and on the index test suite, `index-diff` finds no difference between the index written before the change and the one written after.
- R2. **1.2 reads `critical_reach`.** Every `critical_summary` row that is not a base row (1.1) comes from `critical_reach`. No rule pairs `call` or `fnptr_call` with `critical_summary` to derive `critical_summary`.
- R3. **2.1 reads `critical_arg`.** `establishes_direct` comes from the `critical_arg` rows with an empty `rest`, joined with `call_target_assign_like`. No rule pairs `call` or `fnptr_call` with `critical_summary` to derive `establishes_direct`.
- R4. **Keys kept.** `critical_arg`, `critical_reach` and `establishes_direct` keep every column they have now: call site, formal, path and decision.
- R5. **Termination.** All four apps reach the fixpoint (`reached_fixpoint: true`) under the measurement setup.
- R6. **Less work.** Under the measurement setup, the summed time of `critical_summary`, `establishes_direct`, `critical_arg` and `critical_reach` falls on DuckDuckGo and Nextcloud. `fixpoint_secs` rises on none of the four apps by more than run-to-run noise. Otherwise the change is not kept.
- R7. **Scope.** Rules 1.1, 2.2 (`establishes_via`, `resolvent`) and 3.1 (`call_site`, `resolved_call`) keep their text.

## Design

### D1. 1.2 as a projection (R2)

Replace the two 1.2 rules with one:

```
critical_summary(g, n2, p2r) <-- critical_reach(g, n2, p2r, _, _, _, _);
```

`critical_reach` already applies the `Always` filter, the formal test and the concatenation, and it covers both `call` and `fnptr_call` through `critical_arg`. The rule has one body clause, so its only variant is driven by the `critical_reach` delta.

### D2. 2.1 from the empty-rest split (R3)

Replace the two 2.1 rules with one, written so that Ascent compiles it as a simple join:

```
establishes_direct(f, d, g, insn) <--
    critical_arg(g, arg, key, rest, p, f, insn, n),
    call_target_assign_like(g, arg, key, cto),
    if rest.is_empty(),
    let d = DecisionId::of(Decision { formal: *n, path: *p, target: cto.clone() });
```

The join is on `(g, arg, key)`, so it reuses `critical_arg_indices_0_1_2` and `call_target_assign_like_indices_0_1_2`. For the split with an empty rest, `key` is `p`: `prefix_keys` builds `key` from all of `p`'s components, and `p` is already normalized. Each semi-naive variant iterates its delta side and probes the other.

**Invariant D2 relies on:** every `critical_summary` path is a `path_set` member. The current 2.1 does not consult `path_set`, but `critical_arg` has no row for a non-member. 1.2 and `concat` produce only members, and 1.1 produces `locals` paths, which are members or ε. ε is a member whenever some `actual_param` passes a whole variable (`mod.rs:2536` adds those paths before `compute_paths`). A debug assertion over `critical_summary` at the end of the fixpoint states the invariant.

### D3. Termination (R5)

D1 and D2 keep the same least model, because each new rule derives exactly the rows of the rules it replaces. The relations stay finite (they are bounded by functions × formals × `path_set` members), so the fixpoint is still reached. What changes is the iteration count: one hop up the call graph now takes three rounds (`critical_summary` → `critical_arg` → `critical_reach` → `critical_summary`) instead of one. More rounds mean more iterations with a `critical_arg` delta, and each pays `critical_reach`'s full `locals` scan. See C1.

## Concerns

- C1. **`critical_reach`'s full scan of `locals`.** D1 moves all of 1.2's work into `critical_reach`, whose `critical_arg`-delta variant already scans the whole of `locals` (48 s on DuckDuckGo). A hop now takes three rounds instead of one, so that variant may run in more iterations. It could then cost more than the 35 s that D1 saves, and R6 would fail. Options:
  - (a) Leave `critical_reach` as it is and rely on R6's measurement.
  - (b) Also reorder `critical_reach`'s body to `critical_arg(g, arg, key, rest, p, f, insn, n), locals(g, arg, key, r2, p2, c), if …`, which makes it a simple join on `(g, arg, key)`. Each variant then iterates its delta and probes the other through indices both relations already have (`critical_arg_indices_0_1_2`, `locals_indices_0_1_2`). This touches a rule outside the intent's literal change. It derives the same rows, and it likely removes most of `critical_reach`'s 25–32% share regardless of D1.

  The spec assumes (b), with R6 covering the reordered rule, and records it in the intent if accepted.
