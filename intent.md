# Intent: Do each critical-call and site-resolution join once

## Problem

Two hybrid-inlining joins in `ctadl-index/src/index_engine/mod.rs` are computed more than once, and each copy pairs multi-relation scans that a materialized helper would do once.

1. **Critical-summary propagation.** The two rules that lift `critical_summary` from a callee to its callers (over `call` and `fnptr_call`) re-pair calls with argument vertices, split the path, and probe `locals`. That is the same join `critical_arg` and `critical_reach` already compute for rule 2.2, with the same `Always` filter and the same path concatenation. Rule 2.1 (`establishes_direct`) also rebuilds the call/argument pairing that `critical_arg` holds.
2. **Site resolution.** Rule 3.1's `call_site` and the `resolved_call` rule beside it run the same four-way join, `callee_info` × `locals` × `resolvent` × `callee_resolvents`. Every new `locals` row at a receiver or new resolvent pays for that join twice.

## Change

1. Derive `critical_summary` propagation from `critical_reach`, and `establishes_direct` from `critical_arg` (the split with an empty rest), so the call/argument pairing and the `locals` probe happen once.
2. Add a `site_resolution(caller, insn, target, decision)` helper holding the result of the 3.1 join. `call_site` and `resolved_call` both read it.

Measure the rule profiles of the affected rules before and after on a workload where hybrid inlining is busy. Keep a change only if it reduces work.

## Constraints

- The index results must not change. Use `index-diff` as a gate that must pass.
- Keep every key that distinguishes call sites, formals, paths and decisions, so that a conditional callback's effect never becomes unconditional for every caller of its builder.
- Do not regress fixpoint termination. `critical_reach` grows with `critical_summary`, so check that feeding one from the other converges.
