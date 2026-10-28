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
| `FunctionId`, `DecisionId` | 4 B; already ids |

On the glxy benchmark, peak RSS is 3.7 GB. The `locals` store alone holds 83.1M
rows in 2.04 GB. Each row's leaf is `(Path, FormalIndex, Path)`, 24 B, so
`Path`'s width dominates.

## Requirements

**R1. Handle width.** `Path` and `Str` are 4 bytes. `Option<Path>` and
`Option<Str>` are 4 bytes. A tuple of `(Path, FormalIndex, Path)` is at most
12 B.

**R2. Memory.** On glxy, with the serial and par engines, the `locals` store uses
at least 0.8 GB less than on `main`, and peak RSS falls by about as much.

**R3. Time.** Wall time on the regression suite and on glxy rises by no more than
5% on any engine.

**R4. Same results.** Every derived relation is the same set as on `main`
(`scripts/fact-dump-diff.sh`), and `cargo xtask regression` results are unchanged.

**R5. Concurrency.**
- Interning is safe under `par` at any `-j`.
- Interning is not serialized by a single global lock.

**R6. No locks during the fixpoint.**
- While the fixpoint runs, no `Path` is interned.
- Getting a path from its handle, and finding a path's handle, take no lock.

**R7. Ordering.** This is a decision already made.
- `Ord` on `Path` compares handles, not contents.
- A content comparison and a way to iterate paths in content order exist, and
  any output that must be reproducible uses them. That covers graphviz, dumps
  and debugging output.

**R8. Formats unchanged.** These encode paths and strings by content,
byte-for-byte as on `main`:
- parquet
- `Display` and `Path::parse`
- serde

**R9. No growth at output.** Writing results does not intern anything new. On
`main` today it does: `parquet.rs:555` interns every output path's string.

**R10. Safe handles.**
- Safe code cannot build a handle that was never handed out.
- Any conversion from raw bits back to a handle (for example in LSM run files) is
  checked.

**R11. Limits fail loudly.** Running out of handle space panics with a message
naming the type. Handles are never truncated silently.

**R12. All engines.** serial, par and lsm build and pass every test. lsm must not
regress; it need not shrink (C6).

**R13. Several indexes per process.** More than one index can be built, in
parallel, in one process (as the tests do), and each meets R4–R6.

**R14. Workspace policy.** Nothing committed names a machine-specific path. That
includes build directories, benchmark directories and data directories, in code,
config, scripts and docs.

**Non-goals.**
- 4-byte handles for `FlowVariable`, `CallTargetObject`, `CallDispatchKey` or
  `DecisionSet`
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

Its properties are R5, R10 and R11. Handles of different types do not mix.

**D2. Phases.** An index has two phases:
- **Seeding.** Inputs are converted to relations. Interning is allowed here,
  including concurrent interning.
- **Fixpoint.** Rules run to fixpoint. Lookups only (R6).

Every `Path` value that a rule can produce or probe must already have a handle
when the fixpoint starts. When a rule's lookup misses, that must mean "this path
is not in the index's path set". Whether a miss is an ordinary answer or a bug
depends on the call site, and each call site must state which.

**D3. `Path` and `Str` are handles.**
- `Eq`, `Hash` and `Ord` work on the handle alone and never read the value.
- Every operation the rules perform on a path (split, concat, tail, offsets,
  prefixes) works within D2.

**D4. Boundaries.**
- **Content to handle.** Values become handles at seeding: codegen, parquet
  decode and `EngineInput::new`.
- **Handle to content.** Handles become content only at output: `IndexResult`,
  parquet encode and rendering.
- **Opaque storage.** The LSM provider may store raw handle bits, under R10.

## Concerns and conflicts

**C1. Rule bodies have no context (R6 vs. R13).**
- **Constraint.** Rules can only resolve handles through something global.
- **Conflict.** One process may hold several indexes that are each in a
  different phase, so one global "frozen" state can't express D2.
- **What the plan must do.** Find a way to scope lock-free lookup to each index
  that satisfies both requirements. R6 may then have to be enforced by tests
  rather than at runtime.

**C2. Values are never freed vs. the memory goal (R2).**
- **Constraint.** Interned values live for the whole process, across every index
  in it.
- **Consequence.** R2 is measured for one index per process. A long-lived process
  that builds many indexes keeps growing, and this spec accepts that.
- **Effect of D2.** It removes today's interning, during the fixpoint, of
  candidate paths that are then rejected.

**C3. Handle order vs. reproducibility (R7).**
- **Constraint.** Under `par`, handles depend on the order in which threads first
  intern a value, so anything sorted by `Ord` and printed changes between runs.
- **What the plan must do.** Audit every sort that feeds printed output.
- **Risk.** A sort that was missed shows up as flaky output, not as a wrong
  answer, so tests may not catch it.

**C4. Opaque handles vs. LSM storage (R10).**
- **Constraint.** LSM run files hold raw bits, which must become handles again.
- **What the check must do.** Reject any value that was never handed out, without
  breaking R3.

**C5. Changing `Str`'s width (R1 vs. R8).**
- **Risk.** `Str` derives `Serialize` over its raw id. If anything persists `Str`
  through serde, its format changes.
- **Action.** Confirm that nothing does before `Str` changes.

**C6. LSM gains nothing (R2 vs. R12).**
- **Constraint.** The provider stores every column in a 64-bit slot.
- **Consequence.** R2 applies to serial and par only.

**C7. D2 depends on two path filters being the same.**
- **The rules.** Rules `mod.rs:1573` and `mod.rs:2030` filter candidate paths
  with `ps.contains` and `paths(&p)`, respectively.
- **Risk.** D2 treats a miss as "not in the set". That is only correct if the
  `paths` relation and the `PathSet` contain exactly the same paths.
- **Action.** Confirm that before relying on it.

**C8. Benchmark baseline vs. workspace policy (R2 vs. R14).**
- **Constraint.** The glxy and udpbus inputs live in machine-local directories,
  and R14 forbids committing those paths.
- **Consequence.** Each engineer reproduces the baseline locally.
