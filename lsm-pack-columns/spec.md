# LSM column packing: requirements and design - DO-NOT-MERGE

Source: `intent.md` ("pack LSM columns"). This document refines it into requirements and a
design consistent with the overall specification, [docs/spec.md](docs/spec.md). It does not
sequence the work; that is for planning.

## 1. Purpose

The `lsm` engine (`ctadl index --lsm`, [docs/spec.md §4.2](docs/spec.md#42-analysis),
"Engines") keeps the index engine's largest relations in memory-mapped LSM stores so that
indexing scales past RAM (N1). Today every column of a stored tuple takes a whole number of
eight-byte slots (`lsm_col!` in `lsm/src/ascent_provider.rs:159`), so a 4-byte `FunctionId` or
`Path`, a 2-byte `FormalIndex`, or any 4-byte interned handle costs eight bytes in every tuple
of every store and index that holds it. Since interning ("intern everything", #170) made most
of the engine's values 4-byte handles, most of those bytes are zeros.

This change makes the LSM provider store each column at its natural width, so the relations the
engine holds take fewer bytes: in the memtable (counted against the LSM memory budget), in run
files and the page cache, and in every merge that rewrites runs.

It also removes a per-row cost that packing cannot reach. A relation's store is a set: an
`MmapLsmMultiMap` whose value type is the zero-sized `()`. Its run files are nonetheless written
in the multi-map layout, with an 8-byte entry per key in an offsets table that locates the key's
values. For a set those entries carry nothing (each key has exactly one `()`) and nothing in the
provider reads them, yet after packing they would be a third of a `locals_key` row on disk.
Runs of sets drop the table.

Both changes are to storage only; what the index computes is unchanged.

## 2. Concepts

| Term | Meaning |
| --- | --- |
| **Column type** | A Rust type used as a column of an LSM-provided relation, made storable by `lsm_col!` (e.g. `FunctionId`, `Path`, `FlowVertex`). |
| **Natural width** | A column type's `size_of`; its **alignment** is its `align_of`. |
| **Tuple blob** | The fixed-width bytes one tuple is stored as: the key of a relation's store, or the key or value of an index of its own. |
| **Word** | Eight bytes. A tuple blob is a whole number of words, and the stores order blobs by comparing them word by word. |
| **Store** | A relation's one set of full tuples (`LsmStore`, the relation's `ind_common`). |
| **Prefix index** | An index whose key columns are the relation's first *k* columns. It is served by range reads on the store rather than stored separately. |
| **Own index** | An index whose key columns are not a prefix (`LsmIndex`): a separate LSM map from key blob to value blob. |
| **Prefix boundary** | A column position *k* at which some prefix index of the relation splits key from value. |
| **Segment** | The columns between two consecutive prefix boundaries (or the start or end of the tuple). |
| **Layout** | For one tuple type and one set of prefix boundaries: each column's byte offset in the blob, each segment's end, and the blob's width in words. |
| **Run** | One immutable, sorted, memory-mapped file of an `MmapLsmMultiMap`: a key array, an offsets table, a values array and Bloom filters (`lsm/src/mmap.rs`). |
| **Set map** | An `MmapLsmMultiMap` whose value type is zero-sized, such as a store's `MmapLsmMultiMap<[u64; W], ()>`. Only its keys carry information. |

## 3. Requirements

### 3.1 Functional

**Width**

- R1. **Natural width.** Every column of a stored tuple occupies exactly its natural width, at
  an offset that is a multiple of its alignment. No column is widened to a whole word.
- R2. **Least padding.** A tuple blob's width is the least whole number of words that holds
  its columns under R1 and R4. Padding appears only where R1 or R4 forces it, and only as
  whole bytes set to zero.
- R3. **Everywhere the provider stores tuples.** R1 and R2 hold for relation stores and for
  the keys and values of own indices alike.

**Behaviour preserved**

- R4. **Prefix reads.** Every prefix index is still served from the relation's store by a
  range read, with the store's per-prefix Bloom filters and memtable chains, and needs no
  storage of its own. Tuples with equal first *k* columns, for every prefix boundary *k*, are
  contiguous in every run.
- R5. **Zero-copy reads.** Reads still yield references (`&Column`) straight into the memtable
  or the mapped runs, without copying or decoding a tuple into owned values.
- R6. **Same results.** For every input, each LSM-provided relation holds exactly the rows it
  held before this change, and the `lsm` engine's index is identical, row for row, to the
  serial and parallel engines' (as `scripts/fact-dump-diff.sh` compares them,
  [docs/debugging.md](docs/debugging.md)).
- R7. **Same equality.** Two tuples are stored as identical bytes if and only if they are equal
  values, so the stores' bitwise deduplication and membership tests keep meaning tuple
  equality.
- R8. **Same reach.** Every relation shape the provider accepts today is still accepted:
  arities 0 to 6, prefix indices of up to five columns, `counting` and `seeded` relations, and
  pair columns (`(A, B)`) in any position.
- R9. **No visible change.** The CLI, its flags (`--lsm`, `--lsm-memtable-limit`,
  `--lsm-memory-budget`), SARIF output, and the persisted index (parquet) do not change.

**Column types**

- R10. **Declaring a column.** Making a type a column type names the type and its fields, and,
  for a type holding interned handles, its validity check; it does not state a width. The
  width is derived from the type.
- R11. **Rejected at compile time.** A column type that has padding bytes, an incomplete field
  list, or an alignment above eight does not compile.
- R12. **Checked reads.** In debug builds, every value read back from storage is checked to
  hold only handles its interners handed out, as today.

**Runs of sets**

- R13. **No offsets for sets.** A run of a set map stores no offsets table and no values: its
  bytes per key are the key and its Bloom filter bits. This covers every relation store.
- R14. **Set semantics, stated.** A set map records only which keys are present. A run holds
  each key at most once, and reading a present key from a run yields exactly one value. A
  flush or merge that meets a key several times, whether inserted more than once into the
  memtable or held by several input runs, writes it once. Every other read (`get_refs`,
  `contains_key`, `contains_value`, `keys`, `keys_with_prefix`, `prefix_groups`,
  `iter_all_refs`) answers as it does today for a map that never holds a key twice in one
  source, which is how the provider uses it.
- R15. **Other maps unchanged.** Maps whose values are not zero-sized, including own indices,
  keep today's run layout and multi-map semantics. A run file in the previous layout is
  rejected by its version, never misread.

**Observability**

- R16. **Layout is inspectable.** The width in bytes of each LSM relation's tuple blob, and of
  each own index's key and value blobs, can be obtained programmatically, and the index
  profile (`profile.json`) reports each LSM-provided relation's bytes per row alongside its row
  count.

### 3.2 Non-functional

- N1. **Space.** For the engine's LSM relations, bytes per stored tuple and per run-file row
  fall to the figures in §3.3, and the memtable memory and memory-budget accounting
  (`lsm::memory_in_use`, `memory_peak`) fall with the tuple width.
- N2. **Time.** Fixpoint time of the `lsm` engine does not regress beyond run-to-run noise on
  the `lsm` crate's benchmarks and on a large index; reading a column costs a pointer offset,
  as today.
- N3. **Determinism.** A relation's layout depends only on its column types and its set of
  prefix boundaries, so it is the same on every run and every build of the same program
  (supports [docs/spec.md](docs/spec.md) N5).
- N4. **Safety.** The provider's `unsafe` code keeps a stated contract per item, and the
  contract no longer depends on the target being little-endian. Validity of stored handles
  stays process-local: run files are per-process temporaries and interners never free, as
  today.
- N5. **Policy-free crate.** The `lsm` crate stays independent of CTADL types; column types
  are declared where they are defined (`ctadl-index/src/index_engine/mod.rs:79`).

### 3.3 Expected widths

Bytes per tuple for the relations `Storage::Lsm` moves onto the provider
(`ctadl-index/engines/lsm/src/lib.rs:25`), with `FunctionId`, `Path`, `Root` 4 bytes,
`FormalIndex` 2, `FlowVariable` and `PackedInsnSiteId` 8, `FlowVertex` 16:

| Relation | Columns (natural bytes) | Today | Packed |
| --- | --- | --- | --- |
| `locals_key`, `locals_key_wild` | `FunctionId, FlowVariable, Path, Path, Path` (4, 8, 4, 4, 4) | 40 | 24 |
| `reach_vp`, `prog_store` | `FunctionId, FlowVariable, Path` (4, 8, 4) | 24 | 16 |
| `alias_of_formal` | `FunctionId, FlowVariable, Root` (4, 8, 4) | 24 | 24 |
| `actual_param` | `PackedInsnSiteId, FormalIndex, FlowVertex` (8, 2, 16) | 32 | 32 |

"Packed" assumes the prefix boundaries the rules imply: `locals_key` and `locals_key_wild` are
probed by their first three columns (`ctadl-index/src/index_engine/mod.rs:1675`), whose
segment, `{FunctionId, FlowVariable, Path}`, is 16 bytes, leaving 8 for the rest.
`alias_of_formal` is probed by its first two columns (`ctadl-index/src/index_engine/mod.rs:1906`),
so `{FunctionId, FlowVariable}` (12 bytes) pads to 16 and `Root` takes a word of its own: it does
not shrink. A boundary
that splits a segment's bytes off a word boundary costs up to seven bytes of padding (§4.1);
the profile (R16) reports the actual widths. `actual_param` does not shrink: its 26 bytes of
columns round up to four words, because `FlowVertex` must stay 8-aligned for R5.

In a store's run file a row costs its tuple blob, today an 8-byte offset, and 10 bits of Bloom
filter for the whole key plus 10 for each prefix length the store filters. With the same
prefix assumptions (one filtered prefix for `locals_key`, `locals_key_wild` and
`alias_of_formal`, none for the others):

| Relation | Run bytes per row today | Packed | Packed, no offsets (R13) |
| --- | --- | --- | --- |
| `locals_key`, `locals_key_wild` | 50.5 | 34.5 | 26.5 |
| `reach_vp`, `prog_store` | 33.25 | 25.25 | 17.25 |
| `alias_of_formal` | 34.5 | 34.5 | 26.5 |
| `actual_param` | 41.25 | 41.25 | 33.25 |

Own indices still pay the offsets table, since their keys map to values (R15).

### 3.4 Out of scope

- Sub-byte or bit packing (e.g. `FunctionId`'s 28 significant bits), variable-length
  encodings, and compression of run files.
- Shrinking column types themselves, such as removing `FlowVertex`'s `VertexPad`, which still
  fills the bytes Rust would otherwise leave as padding.
- Changing which relations live in LSM stores, the `cond_trie` stores, the parallel or serial
  engines, `DiskLsmMultiMap` and its codec, and the run layout of maps whose values are not
  zero-sized.
- A set map's memtable entries: each still carries a value vector (16 bytes per entry beside
  the key) that holds only `()`s. Dropping it is a further saving, bounded by the memtable
  limit, left for later.
- Persisting LSM stores across processes, or making run files portable across endianness.

## 4. Design

Column packing (§4.1–§4.6) changes how `lsm/src/ascent_provider.rs` lays a tuple out in its
key, and the column declarations in `ctadl-index`. It leaves `MmapLsmMultiMap`
(`lsm/src/mmap.rs`) alone: the map already stores any `Pod` key of any width, orders keys word
by word, and reads prefixes by whole words. Dropping the offsets of sets (§4.7) is the one
change to the map, and the provider does not see it.

### 4.1 Layout

A relation's layout is computed from its column types' sizes and alignments and from its set
of prefix boundaries (the `PREFIXES` mask `prefix_mask` already computes,
`lsm/src/ascent_provider.rs:713`):

1. Split the columns into segments at the prefix boundaries.
2. Within a segment, place the columns in order of decreasing alignment, ties in declared
   order. Each column's offset is then a multiple of its alignment with no padding between
   columns, since every earlier column's size is a multiple of an alignment at least as large.
3. Each segment starts at a word boundary; the bytes from a segment's last column to the next
   word boundary are zero padding.
4. The blob's width is the end of the last segment, at least one word.

Why segments start on word boundaries: the stores order blobs by comparing native `u64` words,
and on a little-endian machine a word's later bytes outrank its earlier ones. If one word held
the end of the prefix and the start of the suffix, tuples sharing a prefix would not be
contiguous, breaking R4. Starting each segment on a word keeps every prefix a whole number of
words, which is what `keys_with_prefix` and `prefix_groups` take, and keeps those reads in
`u64` compares rather than byte-wise ones. Within a segment the order of bytes is irrelevant
to R4: two tuples agree on a segment exactly when they agree on its bytes. This is also why
reordering columns within a segment is free: the order only fixes where a column lives, never
which tuples are grouped.

Why columns keep their alignment: R5 hands out `&Column` into the blob, so each column must
sit at an offset aligned for its type within a blob that is itself 8-aligned. Blobs are
`[u64; W]`, so every blob in the memtable and in a run's key array starts 8-aligned.

Equal tuples give equal blobs (R7): columns are copied at fixed offsets into a zeroed blob, the
column types have no padding (R11), and pair columns zero their own padding (§4.2).

A key or value of an own index (`LsmIndex<K, V>`) is laid out the same way as a tuple with no
prefix boundaries, since the map reads it only whole.

### 4.2 Column types

`LsmCol` no longer declares slot storage. A column type is described by its natural size and
alignment (from `size_of` and `align_of`), a way to write its bytes into a destination, and its
validity check:

- `write_to(self, dst)` writes exactly `size_of::<Self>()` bytes, every one initialized.
- `ref_at(blob, offset) -> &Self` reinterprets the bytes at an offset `write_to` wrote, and
  asserts `is_valid` in debug builds (R12).
- `is_valid(&self) -> bool`, defaulting to true.

`lsm_col!` keeps its field list and `valid = f` argument and drops the slot count:
`lsm_col!(FunctionId, [u32])`. It asserts at compile time that the fields' sizes sum to the
type's size (no padding) and that the alignment is at most eight (R11); the "fits in N slots"
assertion goes away with the slot count. The existing `compile_fail` doctest is kept and
another added for alignment above eight.

The pair column `(A, B)` stays: its natural size and alignment are those of the Rust tuple
`(A, B)`, and `write_to` zeroes the pair's bytes and writes each field at its `offset_of!`
place through the field's own `write_to`, so padding Rust puts inside the pair is zero.

Because each column is written and read at its own aligned offset, in native byte order, no
step assumes little-endianness; the module's "little-endian target" requirement and the
corresponding clause of the `LsmCol` safety contract are removed (N4). Process-local validity
of interned handles remains in the contract.

### 4.3 Tuple types

`LsmTuple` (implemented for arities 0 to 6, as today) exposes its columns' sizes and
alignments as associated constants. A tuple's layout is a `const fn` of those and a prefix
mask, by §4.1: per-column offsets, segment ends (in bytes, multiples of eight) and the width in
words. A `Layout` is a small fixed-capacity value (at most six columns). Because it depends on
nothing else, it is deterministic (N3) and computable at compile time, where the width becomes
a const generic argument (§4.4).

Encoding and decoding take a layout's offsets rather than a type of their own:

- `encode_at(&self, blob, offsets)` writes each column at its offset.
- `decode_at(blob, offsets) -> Self::Ref<'_>` returns a tuple of `&Column`s.

With offsets passed in, a prefix index's key type `K` (the first *k* columns) and value type
`V` (the rest) are encoded and decoded with the store's layout: `K` with the first *k* offsets,
`V` with the remaining ones. A prefix read's key is encoded into a blob of the store's width
and the first *segment end of k* bytes are its prefix words; a group's key and each tuple's
value are decoded from the stored blob in place. The `skip` arithmetic in `SuffixIter` and
`PrefixGroupsIter` gives way to these offsets. A const assertion in each view checks that
`K`'s and `V`'s column sizes and alignments, concatenated, are the store's, replacing today's
check that their slot sizes sum to the tuple's.

`Cat` and the per-column `Slots` arrays are removed; a blob is a flat `[u64; W]`.

### 4.4 Widths as const generics

Stable Rust cannot spell an array length computed from a generic `T`, so the width enters as a
const generic parameter whose value is computed where the types are concrete: in the provider
macros, which Ascent expands at each relation's declaration with its column types and index
list.

- `LsmStore<T, const P: u32, const W: usize>` stores `MmapLsmMultiMap<[u64; W], ()>`.
  `lsm_nary_rel_ind_common` passes `W` as the width of the layout of `$field_types`'s sizes and
  alignments under the `prefix_mask` it already computes.
- `LsmIndex<K, V, const KW: usize, const VW: usize>` stores `MmapLsmMultiMap<[u64; KW],
  [u64; VW]>`. `lsm_nary_rel_ind` passes both from `$key` and `$val`.
- The view markers and views (`ToFull`, `ToPrefix`, `ToScan`, `FullView`, `PrefixView`,
  `ScanView`, `FullWrite`) carry the store's `P` and `W`. To compute them, `lsm_nary_rel_ind`
  and `lsm_nary_rel_full_ind` match the index list as tokens, the way `lsm_nary_rel_ind_common`
  does, rather than as an opaque `expr`.
- Each type checks in a `const` block that its `W` equals the width its `T` and `P` imply, so
  a mismatched instantiation fails to compile rather than misreads.

The store passes the layout's segment ends as the byte lengths of `with_prefix_reads`, so the
Bloom filters and memtable chains cover exactly the prefixes the views read. The provider
caches each store's `Layout` as an associated constant of the instantiation, so no layout is
computed at run time.

The `lsm` crate enables bytemuck's `min_const_generics` feature: the stores and indices are
generic over `W`, so they need `[u64; W]: Pod` for every `W`, which bytemuck provides only with
that feature.

### 4.5 CTADL column declarations

`ctadl-index/src/index_engine/mod.rs:79` declares each column with its field list and no slot
count. Their natural widths are those in §3.3. `FlowVertex` keeps `VertexPad`: without it Rust
pads the type to 16 bytes anyway, with uninitialized bytes that R7 cannot allow. Nothing else
in `ctadl-index` or `ctadl-ascent` changes; `counting` and `seeded` relations, `dump_stores`,
and the memtable-limit and memory-budget settings are unaffected.

### 4.6 Observability

`LsmStore` and `LsmIndex` report their blob widths in bytes (R16). `EngineStores::store_sizes`
grows a bytes-per-row field per LSM relation, which `profile.json` reports beside the row
counts, so a run shows both what the engine holds and what it costs per row. Engines without
LSM stores report none.

### 4.7 Runs of sets

The offsets table exists because `MmapLsmMultiMap` is a multi-map: a run keeps its values
packed in one array, and `offsets[i]..offsets[i + 1]` is key `i`'s slice of it, which `find`,
`entry` and `merge_runs` read (`lsm/src/mmap.rs:197`, `:219`, `:1037`). The provider reuses
the map as a set for its stores, so it inherited the table without needing it.

The map treats a zero-sized `V` as a set throughout its run code, branching on
`size_of::<V>() == 0`, a constant the compiler folds, so a multi-map pays nothing for the
check:

- **Writing.** `RunWriter` lays out no offsets section (`offsets_off` equals `values_off`, and
  both sections are empty) and `start_key` writes no offset. A run's value count is its key
  count, and the header records it so.
- **Flush and merge.** `flush` writes each memtable key once, whatever number of `()`s it
  holds. `merge_runs` writes each distinct key once, rather than appending a slice from every
  run that holds it, so a key several runs hold is collapsed (R14).
- **Reading.** `find` and `entry` return a one-element slice of `()` for a key the run holds,
  built as `values()` already builds zero-sized slices (`lsm/src/mmap.rs:164`), without
  touching the mapping beyond the key array. `get_refs`, `contains_key`, `contains_value`,
  `iter_all_refs` and the key and prefix iterators go through these and need no change of
  their own.
- **Format.** The header layout is unchanged; `RUN_VERSION` rises from 2 to 3, so a map opened
  on a directory of older runs (`MmapLsmMultiMap::open`) rejects them rather than misreading a
  set run's sections (R15).

The map's `len` keeps counting insertions, as today. The provider inserts a tuple only after
checking it is absent (`LsmStore::insert_if_not_present`), and moves `delta` into `total`
only when they share no tuple, so for its stores `len` is the number of distinct tuples, and
collapsing duplicates in a flush or merge never changes it.

A separate `MmapLsmSet<K>` type was considered and rejected: it would duplicate the levels,
filters, prefix reads and merge, which are the bulk of the map, for a difference that is a
handful of branches in the run code.

### 4.8 Verification

- **Layout unit tests** in `lsm`: a table of column size and alignment lists and prefix masks
  against expected offsets, segment ends and widths, including the rows of §3.3, a segment
  that pads, 1-, 2-, 4-, 8- and 16-byte columns, and the arity-0 key.
- **Round trips**: for every arity and every prefix boundary, `decode_at(encode_at(t))` equals
  `t`, and splitting a stored blob into key and value at a boundary gives the tuple's first
  and remaining columns.
- **Contiguity**: inserting shuffled tuples across several runs (a small memtable limit) and
  reading each prefix returns exactly the tuples with that prefix, for masks with one and with
  several boundaries.
- **Provider equivalence**: `lsm/tests/ascent_nary.rs`, which compares the LSM and default
  providers on the same program, gains relations mixing `u8`, `u16`, `u32`, `u64` and 16-byte
  columns, with several prefix indices and an own index.
- **Compile-time rejections**: `compile_fail` doctests for a padded type, an incomplete field
  list, and alignment above eight.
- **Runs of sets** in `lsm/src/mmap.rs`'s tests: a set run's file has no offsets or values
  bytes (its size is the header, keys and filters); a key inserted twice into the memtable, or
  held by several merged runs, is written once and read back with one value; a set read across
  the memtable and several levels answers every read as the existing set test
  (`lsm/src/mmap.rs:1404`) expects; a version-2 run file is rejected by `open`; and the
  existing multi-map tests pass unchanged (R13–R15).
- **Engine parity**: the existing index engine tests, including the run with memtable limit 4
  (`ctadl-ascent/src/index_engine_tests.rs:153`), pass; and `scripts/fact-dump-diff.sh` finds
  the `lsm` engine's dump equal to the serial engine's on a large target (R6).
- **Space and time**: on that target, `profile.json`'s bytes per row match §3.3, and peak
  memory and fixpoint time are compared with the build before the change (N1, N2); the `lsm`
  crate's benchmarks are compared likewise.

### 4.9 Consistency with docs/spec.md

The change sits inside the "Engines" design of [docs/spec.md §4.2](docs/spec.md#42-analysis):
engines that "differ only in execution and storage". It serves N1 (scale on one workstation)
and keeps N5 (reproducibility) by making layouts deterministic. The engine's run files are
per-process temporaries, not part of the store of §4.6 there, so no persisted CTADL format
changes. The run version bump affects only maps a caller opens on a directory of its own, and
backward compatibility of on-disk formats is out of scope (docs/spec.md §1). docs/spec.md
needs no edit.
