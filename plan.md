# Plan: u32 handles for the index engine's interned data - DO-NOT-MERGE

This plan says how to meet `spec.md` and in what order. R*, D* and C* refer to
`spec.md`, which wins wherever the two disagree.

## Done

- `c7773571`: `immortal` shards on the hash's middle bits, not the top 7 that
  hashbrown takes its tags from.
- Checked against `main`:
  - C7: nothing persists `Str` through serde
  - C9: `paths` and the `PathSet` are the same set
- Surveyed this plan for complexity and memory against `main`. Nothing grows
  asymptotically:
  - `main`'s `Path` is already a front-cons list, hash-consed into an
    interner that never frees (`facts.rs:131-134`, `tailshare/src/lib.rs:123-143`).
  - Its O(d²) prefix keys (`path_set.rs:150-171`) and its intermediate
    `DecisionSet`s (`decision.rs:134`) already stay interned for the whole
    process.
  - The findings are folded into the components below.
- Steps 0–7 are implemented. The tip passes every gate and meets R1, R2, R3
  and R15 (Step 7 below). Results are kept outside the repo (R17).

## Where the implementation departs from this plan

- **Gate.**
  - `scripts/fact-dump-diff.sh` didn't exist. It now does, along with a
    `CTADL_DUMP_RELATIONS=<dir>` dump that writes every relation by content.
  - `main` has no dump mode, so the references come from a build of the
    commit that added it, which derives the same sets as `main`.
  - udpbus is too large to dump, so the gate runs on glxy and xbot.
- **xbot is not deterministic on `main`.**
  - Each `context_summary` decision set grows, and every intermediate value it
    passes through leaves a row in `context_summary_set`, `set_member` and both
    `set_establishes_*` relations.
  - How many intermediate values appear depends on evaluation order.
  - The rules join those relations against the current `context_summary`, so
    the stale rows never reach output.
  - R4 accepts differences in those four relations only; every other relation
    must match exactly.
- **A.**
  - `with_capacity(n)` is `reserve(n)`, since a `static` can't be built with a
    capacity.
  - `stats()` returns a `Stats` struct.
  - `id_handle!` declares each interner as a `static` branded by its handle
    type.
  - The exhaustion test uses a private small limit, since a small `S` makes
    each shard's limit larger, not smaller.
- **C.** The enums became the content types `CallTargetObjectKind` and
  `CallDispatchKeyKind`. The handle `CallDispatchKey::c()` is a `LazyLock`
  forced before the fixpoint.
- **D.**
  - `PathNode` is 24 B. Packing it to 12 B would save about 2 MB on glxy, and
    offsets are `i64`.
  - `Path::EMPTY` is a value no `PathId` can take, read through
    `from_raw_unchecked`.
  - `tailshare` is deleted.
- **E.**
  - Every path the rules split is a member, except the empty path, whose split
    is answered directly. So no extra precomputation over edge paths is needed.
  - The plan's range `mod.rs:1744-1875` omitted `:1557`, `:1979` and `:1987`.
    `compute_paths`'s input is also built from `program_paths` and
    `summary_paths`.
  - Release builds count and log a split miss, and `profile.json` reports
    `split_misses` and `fixpoint_interned`.
  - The R6 test runs in a child process, so no other test interns alongside it.
- **F.**
  - Bit 31 of a `DecisionSet` marks the collapsing kind and picks its interner.
    Both interners keep ids below 2^31 (`IdInterner::with_id_bits`).
  - The empty set is always exact, and exact ⊤ and collapsing ⊤ are distinct
    handles. Two kinds never meet in one index.
- **G.**
  - `Str` already ordered by id on `main`, so `Function`, `Label`,
    `FlowVariable`, `FlowVertex` and `QueryEndpoint` did too.
  - The biggest leak was in neither list: the search breaks ties by successor
    order, which followed index row order. `TaintSearchGraph::new` now sorts
    every successor list by content.
  - Other sites the audit missed: the taint-graph DOT (`cli/mod.rs`),
    `search.rs:267` and `:544`, and the formatter's endpoint and node-id
    orders.
  - `models/spec.rs:827` was an unused alias and is deleted.
  - The determinism test pre-interns both `Str` and whole `Path`s, and adds
    filler values so that their ids come out in the requested order.
- **H.**
  - `lsm_col!` takes its field types and asserts that their sizes sum to the
    type's size.
  - `edge_split` stays `(FlowVariable, Path)`, whose pair impl writes each
    field into zeroed slots. Switching it to `FlowVertex` would have changed
    the dump format.

## Step 7 results

| Bench, config | Peak, `main` → tip (GiB) | Wall time |
|---|---|---|
| glxy serial | 4.57 → 2.57 | no change |
| glxy par | 4.75 → 2.77 | +1.6%, noise |
| glxy lsm | 0.42 → 0.29 | +4.3% on quiet runs |
| xbot serial, par, lsm | 1.09, 1.28, 0.81 → 0.80, 0.93, 0.56 | faster |
| udpbus serial | 44.9 → 26 | +5.2% |
| udpbus par | 49.2 → 28 | +7.4% |

- **Requirements.**
  - R2: glxy's `locals` fell from 2.61 to 1.41 GB, and peak fell by 2.1 GB.
  - Scaled to the spec's `--mem2reg` figures, the saving is about 0.97 GB.
- **udpbus lsm.** It wasn't run: on `main` it never reaches a fixpoint within
  an hour.
- **Open performance work.**
  - glxy lsm spends 4.2% more cycles than `main`, most of it in `edge_split ⋈
    locals` (+12%), a rule that Step 5 rewrote.
  - udpbus serial retires 8.7% more instructions than `main`, extra work that
    was already there by Step 5.
  - About half of udpbus par's slowdown came with Step 6.
- **Found on `main`, not fixed here.** With debug logging on, `par` panics
  (`CMap::frozen() on an unfrozen map`, `c_locals_trie.rs:148`) whenever
  `context_locals` is non-empty.

## Step 0. Baseline on `main`

Record each of these on glxy, udpbus and a contextual Android benchmark:

| Configuration | What to record |
|---|---|
| `Storage::Memory`, serial | peak RSS (`/usr/bin/time -l`), the heap reports, wall time |
| `Storage::Memory`, par | the same |
| `Storage::Lsm` | the same, as the no-regression reference (R15, C8) |

**Heap reports.** `profile.json` holds row counts only. Byte counts go to the
debug log. From that log, record:
- the `[mem cp]` checkpoint lines (`mod.rs:2268`, `2524`)
- the `locals` `heap_report`: `group_hist`, `large_groups`, `p_entries` and
  element sizes (`locals_trie.rs:284`)
- the number of paths and the number of distinct suffix nodes

**Projecting R2.** Do this before step 1. Code comments disagree about where peak
RSS falls:
- `mod.rs:2346` says `take_assign_like`, while `locals` is still alive
- `mod.rs:607-610` says the parquet writer, after `locals` is freed

The checkpoints settle which one holds on glxy.

Then project the saving from the heap report:
- Most `locals` rows are probably in by-path groups. There the leaf is
  `(FormalIndex, Path)` (`path_group.rs:47`), 16 B on `main` and 8 B after,
  not the 24 B the spec sizes.
- Singleton flat sets drop from 32 B to 24 B, but both fall in malloc's 32 B
  size class, so their RSS doesn't fall.
- `FlowVertex` doesn't shrink (see H).

If the projection falls short of R2's 0.8 GB, or the peak isn't while `locals`
is alive, raise it against the spec before going on.

Also record a `fact-dump-diff` reference dump for each, and the wall time of
`cargo xtask regression`. Keep the results outside the repo (R17, C10).

## Components

### A. `IdInterner<T>` in `immortal` (D1)

The benchmark measured these per entry at 10M fixed-size keys:

| Interner | Bytes per entry |
|---|---|
| `access-tree`'s `concurrent.rs` | about 110 |
| `immortal` | about 38 |
| this design | about 24, and fastest at 16 threads |

**Layout.**
- **Shards.** `S` shards, where `S` is a power of two, configurable, default 256.
  Each shard is a `parking_lot::RwLock<hashbrown::HashTable<u64>>` plus its own
  append-only arena. Pad each lock to 128 B (Apple's line size).
- **Table entries.** Each table entry packs `id | hash32 << 32`, and the hasher
  returns the stored hash, so resizes and tag misses don't read the arena. Only
  a candidate match reads the value to compare it.
- **Arena.** Buckets of `64 << b` slots. Each bucket is allocated uninitialized
  under that shard's write lock, so there is no shared counter and no race over
  who allocates a bucket.
- **Read path.** The bucket pointers live in a plain `static` array, apart from
  the locks. A lock-free `get` then never shares a cache line with a lock word
  that `DecisionSet` interning writes during the fixpoint. `get` is shift,
  bucket pointer, slot, with no `LazyLock` check in front.
- **Shard length.** Each shard's length is an atomic, written with Release after
  its slot is filled. `from_raw` and `is_valid` read it with Acquire and take no
  lock.
- **Slots.** No `OnceLock`. The shard lock orders the write before any reader can
  see the id. This is sound only because ids can't be forged (R13).
- **Id.** `((local + 1) << log2 S) | shard`, stored as a `NonZeroU32`, so
  `Option<Id>` is 4 bytes. The shard is picked from hash bits 32 and up, never
  the top 7 bits or the low bits. A shard that runs out of local space panics
  and names the type (R14). The cap is just under `2^(32 − log2 S)` per shard,
  far above any measured path count.

**API.**
- `intern(&T) -> Id`
- `try_get(&T) -> Option<Id>`, which never inserts
- `get(Id) -> &'static T`, lock-free
- `from_raw(u32) -> Option<Id>`, checked against the shard's length (R13)
- `is_valid(Id)`, for the debug check on LSM reads (C6)
- `with_capacity(n)`, which sizes each shard's table for about `n / S · 1.1`
  entries. It doesn't change bucket sizes, so arena slack stays at most 2×.
- `stats() -> (entries, bytes)` (C2)

**Typed handles.** A macro stamps out one typed handle per kind, so handles of
different types can't be mixed.

**Unsized values.** `str` and `[DecisionId]` go in a per-shard bump arena. The
slot holds a 4 B offset and a length. Leaked boxes would cost a malloc block
each, about 40–70 B per entry, which would cancel the interner's own saving for
`Str` and `DecisionSet`.

**Tests.**
- a concurrent stress test
- `try_get` never inserts
- `size_of::<Option<Id>>() == 4`
- `from_raw` rejects ids that were never handed out
- running out of local space panics with the type's name, using a small `S`

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
  - Rules otherwise only clone them into `Decision` (`mod.rs:1672`, `1678`,
    `1719`).
  - One rule builds a value: `*key == CallDispatchKey::C` (`mod.rs:1770`), on
    every firing. Make `C` a constant seeded before the fixpoint, so this
    compares handles and interns nothing (R6).
- **Comparisons.** Add `content_cmp` for both, and for `Decision`, which now
  derives `Ord` over handles (C4).
- **Formats.** `Display` and the parquet encoding stay by content (R11).

### D. `Path` (D3)

- **Node.** `PathNode { head: mir::PathSegment, tail: Option<PathId>, hash: u32 }`.
  - This is front-cons, as tailshare is: `tail()` is one arena read, and every
    suffix of an interned path is itself interned.
  - `hash` is the suffix hash `mix(head, tail's hash)`, so the hash of
    `segments · rest` folds from `rest`'s hash without walking `rest` (E).
- **Normalization.** Interning normalizes as `from_accesses` does today: adjacent
  offsets are summed and a zero offset is dropped.
- **Constants.**
  - The empty path is a reserved, pre-interned id that is a compile-time
    constant, not a lookup. `main`'s `Path::empty()` interns `Nil` on every call
    (`mod.rs:1437`, `1549`, `1550`).
  - The constant paths the rules build (`<component>`, `<action>`, `<type>`) are
    interned at seeding, never lazily mid-run (R6).
- **Comparison.** `content_cmp` walks both paths through the arena (R9).
- **Formats.** serde, `Display` and `parse` stay by content (R11).
- **Parquet encode.** Write an arrow `StringArray` straight from
  `to_dot_string()`, without going through `Str` (R12).
- **`FlowVertex`.** `FlowVertex(FlowVariable, Path)` (`facts.rs:976`) becomes
  8 + 4 bytes padded to 16. It doesn't shrink, and its padding breaks LSM (H).
  Give it an explicit zeroed `u32` pad field.
- **tailshare.** Drop `tailshare` from `ctadl-index` if nothing else uses it.

### E. Frozen path set and candidate tests (D2, R6, R8, C1, C9)

**What seeding interns.** `EngineInput::new` interns everything the rules can
produce:
- every path in the set
- its split keys and rests (`prefix_keys*`)
- its tails
- each path's `split_trailing_offset` result

That is what `main` interns at seeding today, plus the splits. On `main` the
splits are interned mid-run instead (`facts.rs:298-309`).

**Freezing.** The `PathSet` then takes a per-index, read-only table with no locks.
It stores ids only, so it doesn't copy path data next to the global arena:
- **Table.** A `hashbrown::HashTable<PathId>` over every member. It hashes by the
  node's stored suffix hash and compares by reading the arena.
- **Membership.** A bitset over member ids.
- **Splits.** Each member's trailing-offset split, precomputed as
  `id → (prefix id, offset)`.
- **Depth.** The maximum member depth, recorded at freeze time. No depth bound
  exists anywhere else; depth is at most the longest program path plus the
  longest model path (`mod.rs:1308-1329`).

**Candidate test (R8).** To test a candidate `prefix · [adjust] · rest`:
1. `rest` is an existing handle.
2. Fold the new segments onto `rest`'s stored hash, from the last outward. Where
   an offset adjustment meets an offset, apply the normalization rules.
3. Probe the table once with that hash. A miss means the candidate isn't in the
   set.
4. On a tag match, confirm it: walk the matched node's segments against the new
   part, then check that the tail handle where they meet is `rest`.

That costs O(length of the new part) in hashing and comparing, but only one
probe. A probe per segment would put a dependent cache miss on every segment of
the hottest rules (`mod.rs:1534`, `1546`, `1903`, `1938`), at a cost to R3.

The fold walks the new part backwards without allocating. It uses a stack buffer
sized from the recorded maximum depth, or recursion. Start from
`path_set.rs:197`, which already describes this operation.

**Rule changes.**

| Today (`mod.rs`) | After |
|---|---|
| `p.split_trailing_offset()` (1484, 1489, 1919) | `ps.split_trailing_offset(p)`; a miss is a bug (below) |
| `p.substitute_prefix(a, b)`, then `ps.contains` (1573) | `ps.substitute_prefix(p, a, b)`: the R8 test; a miss is an ordinary answer |
| `p.substitute_prefix(a, b)`, then `paths(&p_new)` (2030) | the same `ps` test (C9); the rule joins `path_set(ps)` |
| `Path::from_symbol_names([...])` (1579, 1586, 1594-95) | the seeded constants |
| `Path::empty()` (1437, 1549, 1550) | the constant empty id |
| `*key == CallDispatchKey::C` (1770) | the seeded constant (C) |
| `ps.splits`, `ps.concat` | answered from the frozen table; a miss is an ordinary answer |

**Splits of non-members.** `main`'s `split_trailing_offset` works on any path.
The precomputed one covers members only. The rules apply it to `reach_vp` paths
and to edge source paths, including `assign_like` paths from summaries
(`mod.rs:1744-1875`).
- Before step 5, confirm that every such path is in `compute_paths`'s input
  (`mod.rs:2243-2266`).
- If one isn't, precompute splits over every distinct seeded edge path as well.
- Either way, a miss is a bug, and a debug assertion catches it. Otherwise
  `locals_wild` and `assign_wild` rows would disappear silently and R4 would
  fail.

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
- **Kind.** Exact or collapsing stays per set, not a run-wide setting.
  - The rules read it per set (`mod.rs:1793`, `1845`).
  - Tests build both kinds in one process (`decision.rs`, the last tests), and
    parallel tests may build indexes with different settings, which R16
    forbids breaking.
  - Keep one interner per kind, so a set's handle names its kind (C5). `Eq` and
    `Hash` still act on the handle alone.
- **Order.** `PartialOrd` and `join` stay the lattice order. `is_subset` and
  `contains` read the slice through `get`.
- **Union cache.** Rekey it by handle pair.
  - A slot becomes `seq: AtomicU32`, `(a, b): AtomicU64` and `result: AtomicU32`.
    That is 16 B only because Rust reorders the fields, so assert
    `size_of == 16`. 2^20 slots then take about 16 MB instead of about 40 MB.
  - Keep the seqlock protocol, but fix its missing fence. On `main` the writer
    takes the slot with an Acquire CAS and then stores Relaxed
    (`decision.rs:226-239`), with no `fence(Release)` to pair with the reader's
    acquire fence (`:213`). A reader can then accept a torn entry. Under handles
    a torn entry is a wrong set, not a wild pointer.
  - Update `union_slot`'s comment about pointer alignment (`decision.rs:190`).

### G. Ordering and reproducibility (C3, C4)

- **Audit.** Switch every sort that feeds printed output to `content_cmp`. Start
  with:
  - `index_vertex_cmp` (`ctadl-ascent/src/graphviz.rs:240`), whose `a.2.cmp`
    on the `Path` (`:242`) becomes `content_cmp`
  - the dumps described in `docs/debugging.md`
  - any `BTreeMap` or `BTreeSet` keyed by `Path`, `Str`, `CallTargetObject`,
    `CallDispatchKey` or `Decision` that gets printed, or whose order picks what
    is printed. Those found so far, in `ctadl-ascent/src`:
    - `query_engine/search.rs:580-592`, `:700`. The breadth-first search keeps
      the first state it finds per node (`:649`), so iteration order can change
      which trace is reported, not just how output is sorted.
    - `query_engine/formatter.rs:140`, `:1998`, `:2626`
    - `query_engine/endpoints.rs:85`, `:261-272`
    - `codegen/mod.rs:457`, `:799`
    - `models/matches.rs:309`
    - `models/spec.rs:827`
- **Determinism test.** The test re-runs itself as a child process twice, with
  an environment variable picking the intern order. Each child pre-interns the
  fixture's values in that order (forward or reverse), then renders graphviz,
  the dumps and a query's output. The parent asserts that the two outputs are
  byte-identical.

### H. LSM (C6, C8)

- **Layout.** Each handle sits in its 8-byte slot, and `ref_from` stays
  zero-copy. The exception is `FlowVertex`:
  - `lsm_col!(FlowVertex, 2)` copies all of a value's bytes, padding included,
    into slots (`lsm/src/ascent_provider.rs:148-157`).
  - LSM sorts and deduplicates by slot bits (`lsm/src/mmap.rs:598`, `:852`), so
    padding bytes left uninitialized would be undefined behaviour, and equal
    vertices could compare unequal.
  - The `(FlowVariable, Path)` column (`engines/lsm/src/lib.rs:37`) wouldn't
    compile, since the pair impl asserts there is no padding
    (`ascent_provider.rs:192`).
  - D's explicit pad field fixes both.
- **Padding assert.** Add an assert to `lsm_col!` that `$t` has no padding, as
  the pair impl has. Types without a fixed field list can't be checked
  generically, so the assert compares `size_of` against the sum of the field
  sizes the macro is given, or `$t` implements a `NoPadding` marker.
- **Debug check.** `ref_from` asserts `is_valid` on every handle read back.
- **New columns.** Add `lsm_col!` for any new handle type that appears in a
  relation stored in LSM mode.
- **Comment.** Update the one at `mod.rs:64-67`, which describes `Path` as a
  pointer.

### I. Profile (C2)

Add to `profile.json`:
- each interner's `stats()` (entries and bytes)
- the frozen path set's bytes
- the `locals` heap report's totals, so R2 is read from `profile.json`, not the
  debug log

## Order

Every step ends with a gate:
- `cargo test`
- `fact-dump-diff` against Step 0, on serial, par and lsm (R4)
- `cargo xtask regression` (R4)

| Step | Work | Measure |
|---|---|---|
| 1 | A: interner and its tests. Also F's seqlock fence, which fixes a bug on `main` and can land on its own. | the micro-benchmark, kept outside the repo |
| 2 | B and I: `Str`, serde, profile stats | regression-suite time (R3) |
| 3 | C: `CallTargetObject`, `CallDispatchKey`, `Decision` comparisons, the seeded `CallDispatchKey::C` | `call_target_assign_like` store size |
| 4 | D, G and H: `Path` handle, `FlowVertex` padding and the `lsm_col!` assert, ordering audit, determinism test, LSM check | glxy `locals` store and peak RSS under `Storage::Memory`, against Step 0's projection; most of R2 should show here |
| 5 | E: frozen path set, R8 test, rule rewrites, split coverage check, C9 assertion, R6 test | wall time on glxy and udpbus, serial and par (R3) |
| 6 | F: `DecisionSet` and the union cache | wall time and RSS on the Android benchmark (R3, C5) |
| 7 | Final pass | every Step 0 measurement again; check R1–R3 and R15 against the baseline |

`FlowVertex` gets its pad field in the same step as the 4-byte `Path`, since LSM
breaks without it. Between steps 4 and 5, paths are still interned mid-run
(`substitute_prefix`, `split_trailing_offset`). R6 and R8 only have to hold
after step 5.
