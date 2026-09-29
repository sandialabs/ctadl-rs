# Things to improve

## `locals` blowup on native code: `org.vi_server.androidudpbus` (2026-09-29)

This app does not index in 15 minutes. It hit the timeout at 88 GiB, against a budget of about
2.8 GB (100x its 28.2 MB of IR). Nearly all of the IR is `libudphub.so`, a 567 KB Rust/tokio
library. The Java half is trivial (231 call sites).

### What makes `locals` big

- **Size and growth.** `locals` held 10.7 M rows after 20 s of fixpoint, 152 M after 80 s, and
  341 M after 160 s (9.9 GB for the store alone). Rule costs are normal, about 0.2 µs per tuple,
  so the problem is the number of tuples.
- **One function dominates.** `FUN_00144f04`, an async state machine with 803 blocks, holds
  86–92% of `locals`. Almost every variable in it is reached from about the same 1–3k sources:
  paths of formal 0 plus paths into global memory. So rows ≈ (variables in the function) ×
  (all sources).
- **Root cause: stack slots are not in SSA.** Every stack access goes through one SSA variable,
  `%L3_0`, the stack pointer at entry. The function has 55,775 stores into 255 slots and
  105,050 loads from 285 slots. A typical slot receives about 300 distinct values and is read by
  about 590 loads. Each load inherits every value ever stored to its slot, and loaded values are
  stored again into other slots. The stack becomes a function-wide mixing pool, so every
  variable in the function ends up reached from everything.

### Contributing: unpruned SSA

- `ctadl-ir/src/ssa/mod.rs` places phis with minimal Cytron SSA and no liveness pruning.
  `libudphub` gets 5.19 M phis, of which 1.1% are live. The phis take the statement count from
  880k to 5.96 M and the assign edges to 14.6 M.
- An experimental dead-phi pass cut the assign edges to 1.09 M. The index then finished, in
  875 s at 49.8 GB: still about 18x over budget. The stack-slot mixing remains.

### Fixes to try

1. Treat stack slots at constant offsets from the stack pointer as ordinary locals before SSA,
   so they get SSA versions like any other local (as LLVM's mem2reg does). This addresses
   `locals` directly.
2. Build pruned SSA (a liveness check before placing a phi) or run dead-phi elimination after
   SSA.

### Data

All output is under `/Volumes/Shampoo/ct-bigapk/small/sweep-3c85988c/`:

- `prof/ladder.log`: time and peak memory for every run.
- `prof/rank-t*.txt`: rules ranked by time.
- `prof/ssa-census.txt`: phi counts per function.
- `prof/census-{base60,dp60}/census/*.tsv`: breakdowns of `locals` rows.
- `prof/stack-slots-FUN_00144f04.txt`: store and load counts per stack slot.

Experiment switches (uncommitted in this worktree): `CTADL_DEAD_PHIS=1` turns on the dead-phi
pass, and `CTADL_LOCALS_CENSUS=<dir>` writes the `locals` breakdown.

Other probable failures, measured on the older a979c371 binary: `com.kaeruct.glxy` hit the 48 GB
cap and `com.kgurgul.cpuinfo` timed out.
