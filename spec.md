# Spec: u32 handles for the index engine's interned data - DO-NOT-MERGE

Status: proposed. Branch `intern-everything`.

This spec constrains the **result**. It does not choose the implementation; that
belongs in `plan.md`. Where this spec names a mechanism, it is because the
mechanism is observable or is a decision already made, and it says so.

## Background

The index rules (`index_rules`, `ctadl-index/src/index_engine/mod.rs`) run under
`ascent!` on three engines: serial, par (`ascent_par`, `-j N`) and lsm. Most
column types are already interned, but their handles are 8 bytes or wider:

| Type | Handle today |
|---|---|
| `Path` | 8 B pointer (`tailshare::Seq`) |
| `Str` (and `Function`, `Label`) | 8 B `u64` id |
| `CallTargetObject` | 24 B enum of `FunctionId` or `ArcIntern<str>` |
| `CallDispatchKey` | 40 B enum of up to two `ArcIntern<str>` |
| `DecisionSet` | 24 B (`&'static [DecisionId]` plus a `collapse` flag) |
| `FunctionId`, `DecisionId` | 4 B; already ids |

On the glxy benchmark, peak RSS is 3.7 GB. The `locals` store alone holds 83.1M
rows in 2.04 GB. Each row's leaf is `(Path, FormalIndex, Path)`, 24 B, so
`Path`'s width dominates.

## Requirements

**R1. Handle width.**
- `Path`, `Str`, `CallTargetObject`, `CallDispatchKey` and `DecisionSet` are
  each 4 bytes and `Copy`.
- `Option` of each is 4 bytes.
- A tuple of `(Path, FormalIndex, Path)` is at most 12 B.
- Cloning any of them touches no reference count.

**R2. Memory.** On glxy, with `Storage::Memory` (the default, on the serial and
par engines), the `locals` store uses at least 0.8 GB less than on `main`, and
peak RSS falls by about as much.

**R3. Time.** Wall time on the regression suite and on glxy rises by no more than
5% on any engine.

**R4. Same results.** Every derived relation is the same set as on `main`
(`scripts/fact-dump-diff.sh`), and `cargo xtask regression` results are unchanged.

**R5. Concurrency.**
- Interning is safe under `par` at any `-j`.
- Interning is not serialized by a single global lock.

**R6. No locks during the fixpoint.**
- While the fixpoint runs, no `Path`, `Str`, `CallTargetObject` or
  `CallDispatchKey` is interned.
- Getting any of these from its handle, and finding a path's handle, take no
  lock.

**R7. `DecisionSet` during the fixpoint.**
- Lattice joins create new `DecisionSet`s during the fixpoint, so these may be
  interned concurrently mid-run, as they are on `main`.
- Getting a set from its handle takes no lock.
- The cost of joins and interning counts toward R3.

**R8. Membership tests on candidates.** During the fixpoint, a rule can test
whether a candidate path is in the path set without materializing it. This holds
whenever the candidate is described as a composition of existing handles and
segments: concatenation, prefix substitution, or an offset adjustment to a step.
The test:
- neither interns nor allocates
- takes no lock
- costs time proportional to the part of the candidate that is new, not its full
  length

A miss means the candidate is not in the set, and nothing is left behind.

**R9. Ordering.** This is a decision already made.
- `Ord` on `Path` compares handles, not contents.
- A content comparison and a way to iterate paths in content order exist, and
  any output that must be reproducible uses them. That covers graphviz, dumps
  and debugging output.

**R10. `DecisionSet` order.**
- `DecisionSet`'s `PartialOrd`, and its lattice join, stay the semantic lattice
  order they are on `main` (`decision.rs:455`).
- This covers both the exact-set mode and the collapsing mode (`⊥ < {d} < ⊤`).
- Only `Eq` and `Hash` may act on the handle alone.

**R11. Formats unchanged.** These encode every handle type in R1 by content,
byte-for-byte as on `main`:
- parquet
- `Display` and `Path::parse`
- serde

The exception is `Str`'s serde, which on `main` writes the raw id. It now
writes the string, and with it every type that derives serde from `Str` (C7).

**R12. No growth at output.** Writing results does not intern anything new. On
`main` today it does: `parquet.rs:555` interns every output path's string.

**R13. Safe handles.**
- Safe code cannot build a handle that was never handed out.
- Any conversion from raw bits back to a handle is checked. The exception is
  bits this process wrote itself and never let leave it, which may be trusted
  (C6).

**R14. Limits fail loudly.** Running out of handle space panics with a message
naming the type. Handles are never truncated silently.

**R15. All engines.** serial, par and lsm build and pass every test. lsm must not
regress; it need not shrink (C8).

**R16. Indexes per process.**
- **Production.** Production builds exactly one index per process, and the
  implementation may rely on that.
- **Tests.** `cargo test` may build several indexes in parallel in one process.
  - Each must still meet R4 and R5, and nothing may be unsound.
  - R2, R3 and the lock-free, no-allocation parts of R6–R8 need hold only for
    one index per process.
  - Tests may serialize index construction to get there.

**R17. Workspace policy.** Nothing committed names a machine-specific path. That
includes build directories, benchmark directories and data directories, in code,
config, scripts and docs.

**Non-goals.**
- A 4-byte `FlowVariable`.
  - This is not a capacity problem. About 20 rules build `CallArg` variables
    mid-run by packing bits (`call_arg!`), and a handle would turn each of those
    into a lookup on every firing.
  - It also isn't in the `locals` leaf.
- freeing interned values
- packing LSM columns
- changing the rules' semantics

## Design

These are the contracts the implementation must provide. How it provides them is
left to `plan.md`.

**D1. An interner.** It is a reusable component that maps values of a type to
4-byte handles and back. It must provide:
- `intern(value) -> handle`
- a lookup that never inserts: `value -> Option<handle>`
- `handle -> &'static value`, lock-free
- a checked conversion from raw bits to a handle
- a way to pre-size it

Its properties are R5, R13 and R14. Handles of different types do not mix.

**D2. Phases.** An index has two phases:
- **Seeding.** Inputs are converted to relations. Interning is allowed here,
  including concurrent interning.
- **Fixpoint.** Rules run to fixpoint. Lookups only (R6), except that
  `DecisionSet` may be interned (R7).

Every `Path` value that a rule can produce must already have a handle when the
fixpoint starts. A rule that only tests a candidate (`ps.contains`, `paths(&p)`,
`ps.splits`, `ps.concat`) does so through R8 and needs no handle for it. When a
lookup misses, that must mean "this path is not in the index's path set".
Whether a miss is an ordinary answer or a bug depends on the call site, and each
call site must state which.

**D3. The R1 types are handles.**
- `Eq` and `Hash` work on the handle alone and never read the value.
- Each type has either an `Ord` that works on the handle alone, or, if it is a
  lattice, a `PartialOrd` that is its lattice order (R10).
- Every operation the rules perform on a path (split, concat, tail, offsets,
  prefixes) works within D2.

**D4. Boundaries.**
- **Content to handle.** Values become handles at seeding: codegen, parquet
  decode and `EngineInput::new`.
- **Handle to content.** Handles become content only at output: `IndexResult`,
  parquet encode and rendering.
- **Opaque storage.** The LSM provider may store raw handle bits, under R13.

## Concerns and conflicts

**C1. Rule bodies have no context (R6 vs. R16).**
- **Constraint.** Rules can only resolve handles through something global.
- **In production.** There is one index per process, so a single global phase
  ("seeding" or "frozen") can express D2.
- **Conflict, in tests only.** Parallel tests can hold several indexes at once,
  each in a different phase, and one global phase can't describe them all.
- **Also.** Having a handle is not the same as being in this index's path set:
  the global table also holds prefix keys, rests and other indexes' paths. So
  R8's test must ask this index's set.
- **Decision.** Each index's path set owns its frozen, lock-free table from
  path to handle, and rules reach it through the `path_set(ps)` column. Turning
  a handle back into a path stays global, through an append-only arena that is
  lock-free to read.
- **Consequences.**
  - This is correct for any number of indexes, with no change to tests.
  - A rule that tests a candidate path joins `path_set(ps)`. That is a one-row
    join.

**C2. Values are never freed vs. the memory goal (R2).**
- **Constraint.** Interned values live for the whole process, across every index
  in it.
- **Consequence.** In production there is one index per process (R16), so the pool
  grows across indexes only in test runs.
- **What stays interned that the index doesn't need:**
  - path-set by-products from seeding, such as prefix keys and rests, which grow
    O(depth²) per path
  - intermediate `DecisionSet`s produced by joins
  - arena slack
- **Effect of R6 and R8.** They remove today's interning, during the fixpoint, of
  candidate paths that are then rejected.
- **Decision.** Interning stays append-only and never frees. `profile.json`
  reports each interner's entry count and bytes, so this cost is measured and
  any R2 regression can be traced to it. Growth across indexes in tests is
  accepted.

**C3. Handle order vs. reproducibility (R9).**
- **Constraint.** Under `par`, handles depend on the order in which threads first
  intern a value, so anything sorted by `Ord` and printed changes between runs.
- **Risk.** A sort that was missed shows up as flaky output, not as a wrong
  answer, so tests may not catch it.
- **Not affected.**
  - parquet, which is already unordered
  - the LSM seed sort
  - tests that sort both sides within one process
- **Decision.**
  - Every sort that feeds printed output uses content order. That includes
    graphviz (`graphviz.rs:240`), dumps and debugging output.
  - A test renders graphviz and the dumps for a fixture in two processes, with
    the fixture's values pre-interned in opposite orders, and asserts that the
    outputs are byte-identical. A missed or future sort then fails that test.

**C4. `CallTargetObject` and `CallDispatchKey` change order (R1 vs. R9).**
- **Change.** Both derive `Ord` over their content today. Ordering by handle
  changes how any collection sorted by them iterates.
- **Knock-on.** `Decision` (`decision.rs:33`) derives `Ord` from `Path` and
  `CallTargetObject`, so its order becomes handle order too.
- **Use today.** The rules and stores only hash these types and compare them for
  equality. Tests sort them only within one process.
- **Decision.** This is covered by C3. These two types, and every type that
  derives `Ord` from them (including `Decision`), have a content comparison.
  Printed output sorts by it, and C3's determinism test covers them.

**C5. `DecisionSet` mid-run interning vs. R3.**
- **Change.** The join's union cache is keyed by the set's address today, and has
  to be rekeyed by handle.
- **Constraint.** The two ⊤ values (exact and collapsing) and the empty set must
  still be expressible in 4 bytes.
- **Fact.** The kind of set, exact or collapsing, is fixed once per run, and
  `Eq` and `Hash` already ignore `collapse`. So the handle need not carry a
  `collapse` bit.
- **Cost.** Reading a set's members (`contains`, `is_subset`, `ids()`) gains a
  handle-to-arena load on the contextual closure's hot path.
- **Gain.** Lattice tuples shrink. The union cache can shrink too, from about
  40 MB.
- **Decision.** `DecisionSet` stays in R1. R3's 5% budget also applies to a
  contextual Android benchmark, where the decision lattices are large, and the
  plan must show it is met there as well as on glxy.

**C6. Opaque handles vs. LSM storage (R13).**
- **Constraint.** LSM run files hold raw bits, which must become handles again.
  `LsmCol::ref_from` reinterprets the slot bytes as `&T` without copying, so
  there is no decode step to check.
- **Fact.** Every slot was written by `to_slots` in this process. Run files don't
  outlive the process, and handles are never invalidated (C2).
- **Decision.** LSM storage counts as trusted in-process storage, under
  `LsmCol`'s existing `unsafe` contract. Release builds don't check it. Debug
  builds assert that every handle read back was handed out.

**C7. Changing `Str`'s width (R1 vs. R11).**
- **Fact.** On `main`, `Str` derives serde over its raw id, which only means
  something inside the process that interned it.
- **Checked.** Nothing persists `Str` through serde:
  - bitcode files hold `ctadl-ir` types, which use `ArcIntern<str>`
  - the serde_json outputs (profile, report, model check) contain no `Str`
  - parquet encodes `Str` by content through `EncodeColumn`
- **Decision.** `Str` serializes as its string, and deserializing interns it.
  R11 makes this an explicit exception to "byte-for-byte as on `main`".

**C8. LSM gains nothing (R2 vs. R15).**
- **Constraint.** `Storage::Lsm` is opt-in and serial-only. It keeps the largest
  relations in memory-mapped run files, and stores every column in whole 8-byte
  slots (`LsmCol::Slots = [u64; N]`). A 4-byte handle is padded, so neither RSS
  nor run-file bytes nor merge I/O shrink.
- **Decision.** R2 applies to `Storage::Memory` only. LSM mode must not regress
  (R15). Packing LSM columns stays a non-goal.

**C9. D2 depends on two path filters being the same.**
- **The rules.** Rules `mod.rs:1573` and `mod.rs:2030` filter candidate paths
  with `ps.contains` and `paths(&p)`, respectively.
- **Risk.** D2 treats a miss as "not in the set". That is only correct if the
  `paths` relation and the `PathSet` contain exactly the same paths.
- **Action.** Confirm that before relying on it.

**C10. Benchmark baseline vs. workspace policy (R2 vs. R17).**
- **Constraint.** The glxy and udpbus inputs live in machine-local directories,
  and R17 forbids committing those paths.
- **Consequence.** Each engineer reproduces the baseline locally.
