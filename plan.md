# Plan: u32 handles for the index engine's interned data - DO-NOT-MERGE

This plan says how to meet `spec.md` and in what order. R*, D* and C* refer to
`spec.md`, which wins wherever the two disagree.

## Done

- `c7773571`: `immortal` shards on the hash's middle bits, not the top 7 that
  hashbrown takes its tags from.
- Checked against `main`:
  - C7: nothing persists `Str` through serde
  - C9: `paths` and the `PathSet` are the same set

## Step 0. Baseline on `main`

Record each of these on glxy, udpbus and a contextual Android benchmark:

| Configuration | What to record |
|---|---|
| `Storage::Memory`, serial | peak RSS (`/usr/bin/time -l`), per-store sizes from `profile.json`, wall time |
| `Storage::Memory`, par | the same |
| `Storage::Lsm` | the same, as the no-regression reference (R15, C8) |

Also record a `fact-dump-diff` reference dump for each, and the wall time of
`cargo xtask regression`. Keep the results outside the repo (R17, C10).

## Components

### A. `IdInterner<T>` in `immortal` (D1)

The benchmark measured these per entry at 10M keys:

| Interner | Bytes per entry |
|---|---|
| `access-tree`'s `concurrent.rs` | about 110 |
| `immortal` | about 38 |
| this design | about 24, and fastest at 16 threads |

**Layout.**
- **Shards.** `S` shards, where `S` is a power of two, configurable, default 256.
  Each shard is a `parking_lot::RwLock<hashbrown::HashTable<Id>>` plus its own
  append-only arena. The table holds only ids, and hashes and compares by reading
  the value from the arena.
- **Arena.** Buckets of `64 << b` slots. Each bucket is allocated uninitialized
  under that shard's write lock, so there is no shared counter and no race over
  who allocates a bucket.
- **Slots.** No `OnceLock`. The shard lock orders the write before any reader can
  see the id. This is sound only because ids can't be forged (R13).
- **Id.** `((local + 1) << log2 S) | shard`, stored as a `NonZeroU32`, so
  `Option<Id>` is 4 bytes. The shard is picked from hash bits 32 and up, never
  the top 7 bits or the low bits. A shard that runs out of local space panics
  and names the type (R14).

**API.**
- `intern(&T) -> Id`
- `try_get(&T) -> Option<Id>`, which never inserts
- `get(Id) -> &'static T`, lock-free
- `from_raw(u32) -> Option<Id>`, checked against the shard's length (R13)
- `is_valid(Id)`, for the debug check on LSM reads (C6)
- `with_capacity(n)`
- `stats() -> (entries, bytes)` (C2)

**Typed handles.** A macro stamps out one typed handle per kind, so handles of
different types can't be mixed.

**Unsized values.** `str` and `[DecisionId]` are leaked boxes for now. A bump
arena for them is a later improvement.

**Tests.**
- a concurrent stress test
- `try_get` never inserts
- `size_of::<Option<Id>>() == 4`
- `from_raw` rejects ids that were never handed out
- running out of local space panics with the type's name

### B. `Str`

- **Storage.** Replace `STRING_TABLE` (`facts.rs:29-55`, a `Mutex` plus a
  `FrozenVec`) with an `IdInterner<str>`. `Str` becomes a 4-byte `Copy` handle,
  and `Ord` stays by id, as on `main`.
- **`FlowVariable`.** Its `Local` variant holds the `Str` id in its payload,
  which has room for it.
- **serde.** Hand-written serde writes the string and interns it on deserialize
  (C7). `Function` and `Label` inherit that.
- **Parquet.** Encoding `Str` is unchanged: by content, through `EncodeColumn`.

### C. `CallTargetObject` and `CallDispatchKey`

- **Handles.** Intern each whole enum value, which turns it into a 4-byte `Copy`
  handle with no `ArcIntern` reference counting (R1).
- **Where they're created.** Codegen and parquet decode, both during seeding.
  Before starting, confirm that no rule constructs one; the survey found that
  rules only clone them into `Decision` (`mod.rs:1672`, `1678`, `1719`).
- **Comparisons.** Add `content_cmp` for both, and for `Decision`, which now
  derives `Ord` over handles (C4).
- **Formats.** `Display` and the parquet encoding stay by content (R11).

### D. `Path` (D3)

- **Node.** `PathNode { head: mir::PathSegment, tail: Option<PathId> }`. This is
  front-cons, as tailshare is: `tail()` is one arena read, and every suffix of an
  interned path is itself interned.
- **Normalization.** Interning normalizes as `from_accesses` does today: adjacent
  offsets are summed and a zero offset is dropped.
- **Constants.** The empty path gets a reserved, pre-interned id. The constant
  paths the rules build (`<component>`, `<action>`, `<type>`) are interned at
  seeding, never lazily mid-run (R6).
- **Comparison.** `content_cmp` walks both paths through the arena (R9).
- **Formats.** serde, `Display` and `parse` stay by content (R11).
- **Parquet encode.** Write an arrow `StringArray` straight from
  `to_dot_string()`, without going through `Str` (R12).
- **tailshare.** Drop `tailshare` from `ctadl-index` if nothing else uses it.

### E. Frozen path set and candidate tests (D2, R6, R8, C1, C9)

**What seeding interns.** `EngineInput::new` interns everything the rules can
produce:
- every path in the set
- its split keys and rests (`prefix_keys*`)
- its tails
- each path's `split_trailing_offset` result

**Freezing.** The `PathSet` then takes a per-index, read-only table with no locks:
- **Key.** `(head, tail id)`, mapping to the `PathId`.
- **Contents.** Every suffix of every member, each with a flag saying whether
  that suffix is itself a member.
- **Splits.** Each path's trailing-offset split, precomputed as
  `id → (prefix id, offset)`.

**Candidate test (R8).** To test a candidate `prefix · [adjust] · rest`:
1. `rest` is an existing handle.
2. Starting from `rest`, look up each new segment in the table, from the last
   segment of the new part outward, applying the normalization rules where an
   offset adjustment meets an offset.
3. The first miss means the candidate isn't in the set. Otherwise, the last node
   reached is the candidate, and its member flag gives the answer.

That costs O(length of the new part). To walk the new part backwards without
allocating, use recursion or a fixed-size stack buffer. If a fixed buffer is
used, the maximum path depth has to be bounded and asserted. Start from
`path_set.rs:197`, which already describes this operation.

**Rule changes.**

| Today (`mod.rs`) | After |
|---|---|
| `p.split_trailing_offset()` (1484, 1489, 1919) | `ps.split_trailing_offset(p)` |
| `p.substitute_prefix(a, b)`, then `ps.contains` (1573) | `ps.substitute_prefix(p, a, b)`: the R8 test; a miss is an ordinary answer |
| `p.substitute_prefix(a, b)`, then `paths(&p_new)` (2030) | the same `ps` test (C9); the rule joins `path_set(ps)` |
| `Path::from_symbol_names([...])` (1579, 1586, 1594-95) | the seeded constants |
| `ps.splits`, `ps.concat` | answered from the frozen table; a miss is an ordinary answer |

**Checks.**
- **Debug build.** At seeding, assert that the `paths` relation and the path set
  hold the same paths (C9).
- **R6 test.** A single-threaded fixture test asserts that `stats().entries` for
  `Path`, `Str`, `CallTargetObject` and `CallDispatchKey` is the same before and
  after the fixpoint.

### F. `DecisionSet` (R7, R10, C5)

- **Interner.** An `IdInterner<[DecisionId]>`, with concurrent interning during
  the fixpoint.
- **Reserved handles.** Empty, ⊤ (collapsing) and ⊤-exact. `is_top` compares
  against the two ⊤ handles.
- **Kind.** Exact or collapsing becomes a run-wide setting, set before the run as
  `set_widen_bound` is, instead of a field. `Eq` and `Hash` already ignore it.
- **Order.** `PartialOrd` and `join` stay the lattice order. `is_subset` and
  `contains` read the slice through `get`.
- **Union cache.** Rekey it by handle pair. A slot becomes `seq: AtomicU32`,
  `(a, b): AtomicU64` and `result: AtomicU32`, about 16 B, so 2^20 slots take
  about 16 MB instead of about 40 MB. Keep the seqlock protocol.

### G. Ordering and reproducibility (C3, C4)

- **Audit.** Switch every sort that feeds printed output to `content_cmp`. Start
  with:
  - `index_vertex_cmp` (`ctadl-ascent/src/graphviz.rs:240`)
  - the dumps described in `docs/debugging.md`
  - any `BTreeMap` or `BTreeSet` keyed by `Path`, `Str`, `CallTargetObject`,
    `CallDispatchKey` or `Decision` that gets printed
- **Determinism test.** The test re-runs itself as a child process twice, with
  an environment variable picking the intern order. Each child pre-interns the
  fixture's values in that order (forward or reverse), then renders graphviz and
  the dumps. The parent asserts that the two outputs are byte-identical.

### H. LSM (C6, C8)

- **Layout.** Unchanged: each handle sits in its 8-byte slot, and `ref_from`
  stays zero-copy.
- **Debug check.** `ref_from` asserts `is_valid` on every handle read back.
- **New columns.** Add `lsm_col!` for any new handle type that appears in a
  relation stored in LSM mode.
- **Comment.** Update the one at `mod.rs:64-67`, which describes `Path` as a
  pointer.

### I. Profile (C2)

Add each interner's `stats()` (entries and bytes) to `profile.json`.

## Order

Every step ends with a gate:
- `cargo test`
- `fact-dump-diff` against Step 0, on serial, par and lsm (R4)
- `cargo xtask regression` (R4)

| Step | Work | Measure |
|---|---|---|
| 1 | A: interner and its tests | the micro-benchmark, kept outside the repo |
| 2 | B and I: `Str`, serde, profile stats | regression-suite time (R3) |
| 3 | C: `CallTargetObject`, `CallDispatchKey`, `Decision` comparisons | `call_target_assign_like` store size |
| 4 | D, G and H: `Path` handle, ordering audit, determinism test, LSM check | glxy `locals` store and peak RSS under `Storage::Memory`; most of R2 should show here |
| 5 | E: frozen path set, R8 test, rule rewrites, C9 assertion, R6 test | wall time on glxy and udpbus, serial and par (R3) |
| 6 | F: `DecisionSet` and the union cache | wall time and RSS on the Android benchmark (R3, C5) |
| 7 | Final pass | every Step 0 measurement again; check R1–R3 and R15 against the baseline |

Between steps 4 and 5, paths are still interned mid-run (`substitute_prefix`).
R6 and R8 only have to hold after step 5.
