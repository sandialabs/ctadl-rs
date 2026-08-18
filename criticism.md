# Criticism of android-intent-design.md - DO-NOT-MERGE

A review of the design against the code it cites. Every line reference in the design checked out;
the problems below are where the design's claims meet parts of the machinery it read less
closely. Items that have since been fixed in the design were removed from this file; everything
that remains is open.

## 1. The central algorithm of Phase 3 has no home

Phase 2 deliberately delivers a lookup: one literal per vertex, intraprocedural. Phase 3 needs
more: given the intent *argument at a send site*, recover the action string and target class
attached to that intent object — via its constructor, `setAction`, `setClassName` — possibly many
instructions earlier. That is a small intraprocedural object-use analysis (find the intent's
def, find the calls it flows through as receiver, read their constant arguments), not a per-vertex
constant lookup. It is the hardest part of the design and appears in neither phase.

Say where it lives, and what happens when the intent is built in a helper method
(`buildIntent()` factories are common) — intraprocedural resolution will always miss those, so
the `Unresolved` bucket will be bigger than the design's tone suggests. Counting resolved and
unresolved send sites separately in the per-pass report would make the miss rate visible.

Related: "one constant per vertex" silently drops one arm of `cond ? ACTION_A : ACTION_B`. A set
per vertex costs little more, and the miss is invisible by construction.

## 2. Smaller points

- **No validation plan**: DroidBench's ICC suite and ICC-Bench exist to ground-truth
  exactly this feature. The design names three silent-failure modes of its own (name
  normalization, mis-paired bridges, unresolved constants) but proposes no acceptance test beyond
  measuring fan-out on one APK.
- **Access-path depth**: activity delivery makes extras reachable at
  `this.intent.extras.<key>` — three fields deep. Worth checking against any access-path length
  limit before Phase 3 commits to key-precise extras.

## What holds up

The phasing (manifest facts before constants, constants before linking), the triple store over a
typed schema, resolving constants on the IR instead of in the fixpoint, the fresh-site aliasing
caution (confirmed at `model_matches.rs:330`), the name-normalization warning, the format-version
bump, and the whole section on what *not* to port from the previous ctadl are all correct and
well argued.
