# Plan notes: u32 handles (input to the plan) - DO-NOT-MERGE

These are notes from the survey session. They suggest an approach and an order
for meeting `spec.md`, and nothing here is binding. Requirement numbers (R*)
refer to `spec.md`.

## Done

- `c7773571`: `immortal` now shards on the hash's middle bits, not the top 7
  that hashbrown uses for its tags. This sped up `Path` and `DecisionSet`
  interning on `main`'s code.

## Checks to run first

1. **C7.** Grep for anything that persists `Str` through serde.
2. **C9.** Check that the `paths` relation (`mod.rs:1322-1327`, `1366`) and the
   `PathSet` built by `PathSet::from_paths` contain the same set of paths.
3. **Baseline.** On `main`, record for glxy and udpbus, on serial, par and lsm:
   - peak RSS (`/usr/bin/time -l`)
   - per-store sizes (`profile.json`)
   - wall time
   - a `fact-dump-diff` reference dump

## Candidate interner (for D1)

The benchmark crate is in the session scratchpad. Copy it into the repo or
rewrite it if it is needed; nothing committed may point at the scratchpad.

At 10M keys, per entry:

| Interner | Bytes per entry | Notes |
|---|---|---|
| `access-tree` `concurrent.rs` | about 110 | the key is stored twice; filling `OnceLock` slots commits every page; 4.1 GB peak at 16 threads, because all but one of the threads that each allocated a whole bucket throw theirs away |
| `immortal` | about 38 | 8-byte pointer handles; no way back from an id |
| hybrid, below | about 24 | fastest at 16 threads |

The suggested hybrid:

- **Shards.** Each shard holds a `parking_lot::RwLock<HashTable<Id>>` and its own
  append-only arena.
- **Value stored once.** The table holds only ids, and hashes and compares by
  reading the value from the arena.
- **Arena buckets.** Buckets are allocated uninitialized under the shard's write
  lock. That removes the shared counter and the bucket race.
- **No per-slot flag.** The shard lock already orders writes before readers see
  the id, so slots need no `OnceLock`.
- **Ids.** Encode `((local + 1) << log2 S) | shard` as a `NonZeroU32`. The shard
  is chosen from bits 32 and up of the hash.
- **Shard count.** Configurable. 256–1024 for `-j 16`; going from 64 to 1024
  shards halved the time for lookups of existing keys.
- **`from_raw`.** Check against the shard's length, which is one atomic load
  (R12, C6).
- **Later.** A bump arena for `str`.

## Candidate `Path` representation (for D3)

- **Node shape.** `PathNode { head: mir::PathSegment, tail: Option<PathId> }`.
  This is front-cons, as tailshare is, so `tail()` costs nothing. The mirror shape
  used in access-tree (parent plus last step) makes `tail()` expensive instead.
- **Empty path.** A reserved constant id.
- **Comparison.** `content_cmp` walks both paths through the arena (R8).

## Candidate freeze (for D2, C1)

- **The global arena stays.** It is global, append-only and lock-free. Rules
  already resolve paths through it.
- **Each index gets a frozen table.** The `PathSet` takes a read-only
  `(head, tail id) → PathId` table with no locks, which satisfies R6 and R15.
- **What seeding must intern:**
  - every set path, its split keys and rests, and its `tail`s
  - each path's `split_trailing_offset` result, precomputed as `id → (prefix, offset)`
  - the constant paths: empty, `<component>`, `<action>`, `<type>`
- **Rule changes:**

  | Today (`mod.rs`) | After |
  |---|---|
  | `split_trailing_offset` (1484, 1489, 1919) | `ps.split_trailing_offset(p)` |
  | `substitute_prefix` (1573, 2030) | `ps.substitute_prefix(…)`; a miss means "not in the set" (after C9) |
  | `from_symbol_names` constants (1579, 1586, 1594-95) | constants held on `ps` |

- **Enforcing R6.** A single-threaded fixture test asserts that the path
  interner's `len()` is the same before and after the fixpoint.

## Other changes

- **Graphviz.** `index_vertex_cmp` (`ctadl-ascent/src/graphviz.rs:240`) switches
  to `content_cmp`.
- **Sort audit (C3).** Audit sorts in dumps and debugging code
  (`docs/debugging.md`, `scripts/fact-dump-diff.sh`). The LSM seed sort
  (`engines/lsm/src/lib.rs:62`) and the test sorts can stay sorted by handle.
- **Parquet (R11).** Write paths to an arrow `StringArray` straight from
  `to_dot_string()`, without going through `Str`.
- **LSM.** `lsm_col!(Path, 1)` decodes with `from_raw`. Update the comment at
  `mod.rs:64-67`.

## Suggested order

1. Build the interner and its tests:
   - a concurrent stress test
   - `try_get` never inserts
   - `size_of::<Option<Id>>() == 4`
   - `from_raw` rejects ids that were never handed out
2. Move `Str` onto it, and fix parquet path encoding (R11). This is low risk and
   proves the interner on simple data.
3. Make `Path` a u32 handle, add `content_cmp`, and audit sorts. Measure `locals`
   on glxy here; most of R2 should be visible already.
4. Freeze the `PathSet` and rewrite the rules (R6). Measure time (R3).
5. After every step, run `fact-dump-diff` and `cargo xtask regression` on all
   three engines.
