# LSM column packing: implementation plan - DO-NOT-MERGE

Source: [spec.md](spec.md) (requirements R1–R16, N1–N5, design §4). This plan orders the work
into phases, each of which builds, passes the tests named in it, and can be its own commit.
Section and requirement numbers refer to spec.md.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `lsm/src/mmap.rs` | Runs of sets: no offsets or values sections, dedup on flush and merge, `RUN_VERSION` 3 (§4.7); new tests | 1 |
| `lsm/Cargo.toml` | `bytemuck = { version = "1", features = ["min_const_generics"] }` | 2 |
| `lsm/src/ascent_provider.rs` | `Layout` and its computation (§4.1); `LsmCol` and `lsm_col!` without slots (§4.2); `LsmTuple` with sizes, alignments, `encode_at`/`decode_at` (§4.3); `LsmStore`, `LsmIndex`, views, iterators and provider macros with `W`/`KW`/`VW` (§4.4); `Cat` removed; module docs; width accessors (§4.6); unit tests | 2, 3, 4 |
| `lsm/tests/ascent_nary.rs` | `lsm_col!(Lab, [u64, u64])`; new mixed-width relations (§4.8) | 3 |
| `lsm/tests/ascent_redeclare.rs`, `lsm/tests/memory_budget.rs`, `lsm/benches/mmap_benchmarks.rs` | Only if they name a slot count or `LsmStore`'s parameters; expected to compile unchanged | 3 |
| `ctadl-index/src/index_engine/mod.rs:79` | Column declarations drop the slot count (§4.5) | 3 |
| `ctadl-index/src/index_engine/mod.rs:1070` | `StoreSizes` gains per-relation bytes per row | 4 |
| `ctadl-index/src/index_engine/mod.rs:2636`, `:2771` | Serial/parallel `store_sizes` report no widths; `run_engine!` passes widths to the profile | 4 |
| `ctadl-index/src/index_engine/profile.rs:23` | `IndexProfile` gains `bytes_per_row: BTreeMap<String, usize>` | 4 |
| `ctadl-index/engines/lsm/src/lib.rs:45` | `store_sizes` reports each LSM relation's tuple width | 4 |

Not touched: `lsm/src/disk.rs`, `lsm/src/bloom.rs`, the `cond_trie` stores, the serial and
parallel engines' rules, `ctadl-index/src/facts.rs` (`FlowVertex` keeps `VertexPad`).

## How the layout is computed in stable Rust

§4.3 asks for a layout that is a `const fn` of the columns' sizes and alignments (associated
constants of `LsmTuple`) and a prefix mask. A trait cannot declare a `const fn` in stable Rust
(1.94), so `layout` is a free function:
`pub const fn layout(sizes: &[usize], aligns: &[usize], prefixes: u32) -> Layout`, over
`LsmTuple`'s `SIZES: &'static [usize]` and `ALIGNS: &'static [usize]`.

- **In the macros**, the width is
  `{ $crate::ascent_provider::layout(<$t as LsmTuple>::SIZES, <$t as LsmTuple>::ALIGNS, mask).words }`.
  This is a const expression over concrete types, so it is legal as a const generic argument.
- **In generic code**, the layout is an associated constant of the instantiation,
  `const LAYOUT: Layout = layout(T::SIZES, T::ALIGNS, P);`, which stable Rust allows.

`min_const_generics` is on from Phase 2 (§4.4): `[u64; W]: Pod` for a generic `W` needs it.

## Phase 0: baseline (no code change)

Record the "before" numbers that N1 and N2 are measured against. Build from this commit with
`CARGO_TARGET_DIR` set outside the repo.

- `cargo bench -p lsm -- --save-baseline before` (both `lsm_benchmarks` and `mmap_benchmarks`).
- On the large target chosen for this work: `ctadl index --lsm` under `/usr/bin/time -l`.
  Keep `profile.json`, the peak RSS, `lsm::memory_peak` (as logged), the fixpoint time, the
  total size of the LSM run files (sum `du` of the temp dir at the end, or log it), and a fact
  dump for `scripts/fact-dump-diff.sh`. Also make a serial-engine dump of the same target.

**Status: done** (2026-11-07, no code change). Results and exact commands are in
`/Volumes/Shampoo/lsm-pack-results/phase0/README.md`, which is machine-local. Deviations:

- **Target.** The large target is glxy (the APK plus `libgdx.so`). It was re-imported into a
  fresh store, because the old stores have an older import format.
- **Bench command.** `cargo bench -p lsm -- …` fails, because the lib's test harness rejects
  criterion's flags. Use `cargo bench -p lsm --bench lsm_benchmarks --bench mmap_benchmarks --
  --save-baseline before`, and likewise `--baseline before` later. The baseline is copied to
  `phase0/criterion-before/`.
- **Run-file bytes.** No log line reports them, and `du` misses them, because run files are
  unlinked once mapped (`lsm/src/mmap.rs:367`). `phase0/scripts/run-lsm.sh` samples the named
  files plus the mapped ones `lsof` reports, once a second.

Baseline, glxy `--lsm`, 3 runs:

| Measure | Value |
| --- | --- |
| Wall | 42.8–46.9 s (±4.5 % noise) |
| Fixpoint | 40.9–44.8 s |
| Peak footprint | 1.11 GiB |
| Max RSS | 1.37 GiB |
| `lsm::memory_peak` | 42.0 MiB |
| Run files | 134,482,432 bytes |

Row counts:

| Relation | Rows |
| --- | --- |
| `locals_key` | 1,658,149 |
| `locals_key_wild` | 515,145 |
| `reach_vp` | 725,788 |
| `prog_store` | 79,405 |
| `alias_of_formal` | 96,973 |
| `actual_param` | 147,087 |

The serial and lsm dumps agree on all 58 relations.

## Phase 1: runs of sets (§4.7; R13–R15)

Independent of column packing and confined to `lsm/src/mmap.rs`, so it goes first. That way
Phase 6 can separate its savings from those of packing.

Changes, all branching on `size_of::<V>() == 0`:

- `RunWriter::create` (`lsm/src/mmap.rs:256`): for a set, `offsets_off = values_off =
  align8(keys_off + max_entries * size_of::<K>())`, with no offsets bytes reserved.
- `RunWriter::start_key` (`:309`): write no offset for a set; `append_values` is not called.
- `RunWriter::finish` (`:342`): for a set, write no sentinel offset, and set `values_len` to
  `keys_written` in the header and in the returned run. Skip the `values_written ==
  values_len` assertion: a merge that collapses keys writes fewer than it reserved.
- `flush` (`:975`): for a set, call only `start_key` per sorted memtable key, and size the
  writer with `values_len = keys_len()`.
- `merge_runs` (`:1037`): for a set, `start_key` once per distinct key from `merge_keys` and
  read no offsets of the inputs (`run.offsets()` would read past a set run's keys).
- `MmapRun::find` (`:197`) and `MmapRun::entry` (`:219`): for a set, return a one-element
  `&[()]` (the dangling-pointer construction `values()` already uses at `:165`) and never
  call `offsets()`.
- `RUN_VERSION` (`:34`) goes from 2 to 3. Update the `load_run` error message (`:401`), which
  says "magic" but now also covers the version.
- Module docs (`:1`) and `MmapRun`/`RunWriter` docs: say that a set run has no offsets or
  values sections.

Tests, in `lsm/src/mmap.rs` `mod tests`:

- `set_run_has_no_offsets`: `MmapLsmMultiMap::<[u64; 2], ()>::open(dir, 1000)`, insert
  distinct keys, `flush`. Load the run's header and assert `offsets_off == values_off ==
  filter_off`, and that the file length is `filter_off + (1 + prefix filters) * filter
  bytes`.
- `set_duplicates_collapse`: a key inserted twice into one memtable, and a key held by
  several runs (small memtable limit and fanout 2, so that merges happen), are each written
  once. `get_refs(k).count() == 1` after flush and after merge, and the merged run's
  `entries_count` equals the number of distinct keys.
- `old_run_version_rejected`: write a run with `open`, overwrite header bytes 4..8 with
  `2u32.to_ne_bytes()`, and assert `open` returns `ErrorKind::InvalidData`.
- Unchanged and still passing: `unit_values_across_runs` (`:1347`, the set test §4.8 names),
  `keys_with_prefix_across_runs_and_memtable`, and every `u64 → u64` multi-map test (R15).

Gate:

- `cargo test -p lsm`; `cargo test -p ctadl-ascent lsm_storage_matches_memory`.
- On the large target, the `lsm` dump must equal Phase 0's. Record the total run-file bytes,
  which 3a is checked against, and their drop from Phase 0, which is Phase 1's saving.

**Status: done** in "lsm: runs of sets store no offsets or values". All the gates pass,
including on the branch combined with Phase 2. Results are in
`/Volumes/Shampoo/lsm-pack-results/phase1/`. On glxy `--lsm`:

- **Dump.** It equals Phase 0's on all 58 relations, and the row counts are unchanged.
- **Run files.** They fall to **111,108,928 bytes**, down 23,373,504 (−17.4 %). 3a is
  checked against this figure.
- **Everything else.** Wall 46.3 s, fixpoint 42.9 s, peak 1.11 GiB, `memory_peak` 42.0 MiB.
  All are within Phase 0's spread, from one run at load 30.

Deviations:

- **`flush`'s writer sizing.** `flush` still passes the insertion count as `values_len`:
  `RunWriter::create` ignores it for a set, and `finish` records the key count. The docs on
  `create` say so.
- **Error message.** It now reads "`<path>`: not an mmap LSM run of version 3".
- **Small additions.** A new `MmapRun::values_of(i)` serves both `find` and `entry`. A
  `debug_assert!` stops `offsets()` from being called on a set run. `set_run_has_no_offsets`
  uses a prefix filter, so its file-length check covers the prefix-filter term.

## Phase 2: layout computation (§4.1; R1, R2, N3), additive

Pure code with no callers yet, so the layout rules get tested in isolation.

- `lsm/Cargo.toml`: enable `min_const_generics`.
- `lsm/src/ascent_provider.rs`: `pub struct Layout { offsets: [usize; 6], seg_ends: [usize;
  6], n_cols: usize, n_segs: usize, pub words: usize }` plus accessors, and
  `pub const fn layout(sizes: &[usize], aligns: &[usize], prefixes: u32) -> Layout`. The
  steps:
  1. The boundaries are the bits `k` of `prefixes` with `0 < k < n_cols`. Bit 0 (scan) and
     bit `n_cols` (full index) do not split.
  2. Within each segment, a stable selection by decreasing alignment assigns offsets (a
     `while` loop, since this is const context).
  3. Each segment starts at a multiple of 8, and `seg_ends[s]` is the segment's end rounded
     up to 8.
  4. `words = max(1, last seg_end / 8)`.

  Assert in the function (a const panic) that `n_cols <= 6` and that every alignment divides
  8.
- Also a helper `const fn prefix_bytes(&self, k) -> usize`: the segment end of boundary `k`,
  which views and `with_prefix_reads` use.

Tests (`#[cfg(test)] mod layout_tests` in `ascent_provider.rs`), as one table of `(sizes,
aligns, mask) → (offsets, seg_ends, words)`:

- §3.3's rows. `locals_key`: `[4,8,4,4,4]`, mask with bit 3, gives 3 words with offsets
  `[8,0,12,16,20]`. `reach_vp`: `[4,8,4]` with no inner boundary gives 2 words. `actual_param`:
  `[8,2,16]` gives 4 words, with `FlowVertex` at an 8-aligned offset.
- A segment that pads: `[4]` + boundary + `[8]` gives 2 words, with the second column at 8.
- One column each of 1, 2, 4, 8 and 16 bytes (the 16 with align 8) in one segment, which
  packs to 32 bytes / 4 words (31 bytes rounded up).
- Several boundaries (a mask with bits 1, 2 and 4 on a 5-column tuple of `u32`s).
- Arity 0: `layout(&[], &[], 0).words == 1`.
- Ties keep declared order: `[4,4,4]` gives offsets `[0,4,8]`.
- A `const _: Layout = layout(...)` item, to prove it evaluates at compile time.

Gate: `cargo test -p lsm layout`.

**Status: done** in "lsm: compute packed tuple layouts". The gates pass: the `layout`
tests, `cargo test -p lsm`, clippy with `-D warnings`, and fmt. Every expected value above
matched the §4.1 rules. Deviations:

- `Layout` has a private `seg_cols` field: for each segment, the index one past its last
  column. `prefix_bytes(k)` uses it to find the segment that ends at `k`.
- `prefix_bytes(k)` also accepts `k == 0` (it returns 0) and `k == n_cols` (the full width). It
  panics for a `k` that falls inside a segment.
- `layout` also panics if `sizes` and `aligns` differ in length, or if a size is not a
  multiple of its alignment. The no-padding argument in step 2 depends on the second check.
- There is an extra accessor, `n_cols()`.
- The tests add a row of six 1-byte columns split at every boundary, a row showing that mask
  bits 0 and *n* split nothing, and should-panic tests.
- At arity 0, `seg_ends()` is `[0]`: one empty segment, while `words` is 1.

## Phase 3: packed encoding through the provider (§4.2–§4.5; R1–R12, N4, N5)

Phase 3 is the riskiest part of the work. An offset or padding bug gives wrong rows, not a
crash, and decoding through offsets could slow the join loops. So it lands as two commits:

- **3a, the new code with today's layout.** Every change below goes in, but the layout puts
  each column at its own 8-aligned offset, so the stored bytes are the same as today's.
- **3b, packing on.** One function changes, to the §4.1 layout.

A failure in 3a is then a bug in the new code, and a failure in 3b is a bug in packing.

The provider gets the layout from one place, a
`const fn store_layout(sizes, aligns, prefixes) -> Layout`. The macros' width expressions and
every `LAYOUT`/`K_LAYOUT`/`V_LAYOUT` constant call it. In 3a it returns `slot_layout`, a
private `const fn` that places the columns in declared order, each at the next multiple of
eight, with width `Σ ceil(size / 8)` (at least one word). In 3b it returns Phase 2's `layout`,
and `slot_layout` is deleted.

### Phase 3a: the new code, with today's layout

This is one commit: `lsm_col!`'s signature and `LsmStore`'s parameters change together, and the
workspace does not build between them.

`lsm/src/ascent_provider.rs`, in file order:

1. **Module docs (`:18`–`:30`).** Describe the natural-width layout. Drop "slots" and the
   little-endian requirement, and keep process-local validity.
2. **Remove `Cat`** (`:74`–`:91`).
3. **`LsmCol` (`:97`).** Replace `Slots`/`to_slots`/`ref_from` with
   `fn write_to(self, dst: &mut [u8])` (exactly `size_of::<Self>()` bytes, all initialized),
   `unsafe fn ref_at(blob: &[u8], offset: usize) -> &Self` (debug-asserts `is_valid`), and
   `is_valid`. Rewrite the safety contract: no padding, alignment ≤ 8, equal values give equal
   bytes, process-local validity. The contract drops endianness.
4. **`lsm_col!` (`:159`).** New form `($t:ty, [$($field:ty),+] $(, valid = $f:expr)?)`. The
   const asserts are fields-sum-equals-size and align ≤ 8, and the slot-count assert goes.
   `write_to` is one `copy_nonoverlapping` of `size_of::<$t>()` bytes. `ref_at` casts
   `blob.as_ptr().add(offset)`, with a debug assertion on alignment and bounds. Update the
   primitive declarations (`:217`–`:224`). Doctests: the passing example becomes
   `lsm_col!(Pair, [u32, u32])`, the padded `compile_fail` stays, and a new `compile_fail`
   uses a `#[repr(align(16))]` type (R11).
5. **Pair column (`:236`).** `write_to` zeroes `size_of::<(A, B)>()` bytes of `dst`, then
   calls `A::write_to` and `B::write_to` at `offset_of!((A, B), 0)` and `offset_of!((A, B),
   1)`. `ref_at` casts to `&(A, B)`.
6. **`LsmTuple` (`:290`).** Replace `Arr`, `encode`, `decode` and `COLUMN_BYTES` with
   `const SIZES`, `const ALIGNS`, `type Ref<'a>`,
   `fn encode_at(&self, blob: &mut [u8], offsets: &[usize])`, and
   `unsafe fn decode_at(blob: &[u8], offsets: &[usize]) -> Self::Ref<'_>`. Implement it for
   `()` and arities 1–6. A small `macro_rules!` inside the module generates the six impls,
   replacing the hand-nested `Cat` code (`:335`–`:513`).
7. **`LsmIndex<K, V, const KW: usize, const VW: usize>` (`:583`).** The map type becomes
   `MmapLsmMultiMap<[u64; KW], [u64; VW]>`. Add associated consts `K_LAYOUT = layout(K::SIZES,
   K::ALIGNS, 0)` and the same for `V`. A `const { assert!(KW == K_LAYOUT.words && VW ==
   V_LAYOUT.words) }` goes in `Default::default`, which every instantiation passes through.
   `index_insert` and `index_get` encode into zeroed `[u64; KW]`/`[u64; VW]` through
   `bytemuck::bytes_of_mut`. `ValIter` and `AllIter` carry the layouts' offsets and decode
   with `decode_at`.
8. **`LsmStore<T, const P: u32, const W: usize>` (`:693`).** Add `const LAYOUT: Layout =
   layout(T::SIZES, T::ALIGNS, P)` and assert `W == LAYOUT.words` in `Default`. The
   `with_prefix_reads` lengths are `LAYOUT.prefix_bytes(k)` for each inner boundary, replacing
   the `COLUMN_BYTES` sum at `:699`. Add `fn encode(&T) -> [u64; W]` (zeroed, then
   `encode_at`) and use it in `contains` and `insert_if_not_present`. `iter` decodes with
   `LAYOUT.offsets()`.
9. **Markers and views (`:774`–`:1133`).** `ToFull<T, P, W>`, `ToPrefix<T, K, V, P, W>`,
   `ToScan<T, P, W>`, and the views and `FullWrite` likewise. In `PrefixView`, the
   `to_rel_index` const assert (`:845`) checks that `K::SIZES ++ V::SIZES == T::SIZES` and the
   same for `ALIGNS`, as a const loop. `index_get` encodes `key` with the first `k` offsets
   into a zeroed `[u64; W]` and passes `&words[..prefix_bytes(k) / 8]` to
   `keys_with_prefix`. `iter_all` calls `prefix_groups(prefix_bytes(k) / 8)`. `SuffixIter`
   and `PrefixGroupsIter` drop `skip` and decode `V` with `LAYOUT.offsets()[k..]` and `K` with
   `[..k]`, from the whole stored blob. `k` is `K::SIZES.len()`.
10. **Macros (`:1288`–`:1339`).** In `lsm_nary_rel_ind_common`, pass `W` computed from the mask
    it already builds. `lsm_nary_rel_full_ind` and `lsm_nary_rel_ind` change `$indices:expr`
    to the token pattern `[$([$($col:literal),*]),*]`, so that they can compute the same
    mask and `W` and forward them to `ToFull` and `lsm_nary_index_for!`. Add `P`/`W`
    parameters to `lsm_nary_index_for!`. Its `LsmIndex` arm computes `KW`/`VW` from `$key` and
    `$val` with mask 0. Factor the mask and width expressions into two `#[doc(hidden)]` helper
    macros, so that each is written once.

CTADL side:

- `ctadl-index/src/index_engine/mod.rs:79`–`:91`: drop the slot counts, e.g.
  `lsm::lsm_col!(FlowVertex, [FlowVariable, Path, crate::facts::VertexPad], valid = ...)`.
  Fix the comment above them, which says nothing about slots but should keep the "4-byte ids"
  reasoning.

Tests added in 3a. All but the first are written against `store_layout`, so they run
unchanged in 3b and check the packed layout there:

- **Same bytes as today** (3a only, deleted in 3b): for tuples of `u8`, `u16`, `u32`, `u64`,
  a 16-byte `lsm_col!` type and `FlowVertex`-shaped columns, the stored blob must equal each
  column's bytes, zero-padded to a multiple of eight, concatenated in column order. That is
  exactly what the slot encoding wrote. Pair columns are the one exception: a pair now takes
  `ceil(size_of::<(A, B)>() / 8)` words, where it took the sum of its fields' slots. No CTADL
  relation has a pair column.
- **Round trips** (`ascent_provider.rs` tests): for arities 1–6 over columns `u8`, `u16`,
  `u32`, `u64`, `(u32, u16)` and a 16-byte `lsm_col!` type, and for every mask over the
  arity's inner boundaries, check `decode_at(encode_at(t)) == t`. Splitting the stored blob at
  each boundary `k` must decode the first `k` and the remaining columns as `K` and `V`. Pad
  bytes must be zero (R2, R7). Equal tuples must give equal blobs, and tuples differing in
  one column must give different blobs.
- **Contiguity** (`ascent_provider.rs` tests, driving `LsmStore` directly through `FullWrite`
  and `PrefixView`): `set_memtable_limit(4)`, 500 shuffled tuples of `(u32, u64, u16, u32)`,
  and masks `{2}` and `{1, 2, 3}`. Every prefix read (`index_get`) and `iter_all` must return
  exactly the matching tuples, and each `iter_all` group's tuples must share its key.
- **Provider equivalence** (`lsm/tests/ascent_nary.rs`): keep `edge`, `reach` and `quint`, and
  add a relation `mix(u8, u32, u16, Lab, u64)` and rules that probe it by prefixes of length 1,
  2 and 3 (several boundaries) and by `mix(_, b, _, _, e)` (an own index with non-prefix key
  columns). Compare with `DefaultProg` as today. Run it once at the default memtable limit
  and once at 4, so that both memtable and runs are read.
- **Compile-time rejections**: the two `compile_fail` doctests above, plus an incomplete field
  list (`lsm_col!(Pair, [u32])`).

Gate for 3a:

- `cargo test -p lsm` (unit, integration, doc), and `cargo test -p lsm --release`, to check
  that release builds, without the debug validity assertions, agree.
- `cargo test -p ctadl-ascent` (`index_engine_tests`, including `lsm_storage_matches_memory`,
  `ctadl-ascent/src/index_engine_tests.rs:153`).
- `cargo clippy --workspace --all-targets`.
- **Time.** `cargo bench -p lsm --bench mmap_benchmarks -- --baseline before` must show no
  regression beyond noise. The bytes and the order are the same as at the baseline, so any
  slowdown comes from decoding through offsets. If there is one, check that `decode_at`
  inlines and that the offsets from `LAYOUT` fold into constants, before going on to 3b.
- **Engine parity on the large target.** The `lsm` engine's dump must equal Phase 0's.
  `profile.json`'s row counts must equal Phase 0's too, and the total run-file bytes must
  equal Phase 1's, since the layout has not changed.

**Status: done** in "lsm: store tuple columns through a layout". Results are in
`/Volumes/Shampoo/lsm-pack-results/phase3a/`.

The gates pass:
- `cargo test -p lsm` in debug and release.
- All of `cargo test -p ctadl-ascent`.
- Workspace clippy with `-D warnings`, and fmt.
- Each `compile_fail` doctest was checked by hand to fail for its intended reason.
- With `store_layout` switched to `layout`, every lib test passes except "same bytes as
  today", as intended.

Benches against `before`, at load 9–10, show no regression in the code 3a changes:
`provider_tc/lsm` was −2 % and −10 % in two runs. `disk_lsm/insert` was +35–39 % with wide
intervals, but it measures `lsm/src/disk.rs`, which no phase touches. Phase 5 should look at
it again on a quiet machine.

On glxy `--lsm`:
- **Dump.** It equals Phase 0's, and the row counts match.
- **Run files.** They end at exactly **111,108,928 bytes**, the same as Phase 1.
- **Run-file samples.** One 1 s sample caught a merge holding its inputs and its output
  (165.2 MB), so the script's maximum overstates the size. Compare the final value.
- **Time and memory.** Wall 45.2 s, fixpoint 41.8 s, peak 1.09 GiB, `memory_peak` 42.0 MiB,
  at load 30.

Deviations:

1. **View markers.** `ToFull<T>`, `ToPrefix<T, K, V>` and `ToScan<T>` don't carry `P`/`W`.
   They implement `ToRelIndex<LsmStore<T, P, W>>`, so the store type fixes both. The views
   and iterators do carry `P` and `W`.
   - So `lsm_nary_rel_ind` and `lsm_nary_rel_full_ind` keep `$indices:expr`, and
     `lsm_nary_index_for!` takes no `P`/`W`.
   - There is one hidden helper macro, `lsm_tuple_words!($t, $mask)`, rather than two.
2. **`slot_layout`** makes each column its own segment and takes no mask, so `prefix_bytes(k)`
   holds for every `k`.
3. **Index iterators.** `ValIter` and `AllIter` carry `K, V, KW, VW`, so that they read the
   index's `K_LAYOUT`/`V_LAYOUT`.
   - Each prefix view's `k` and its prefix bytes are inline `const { }` blocks.
   - So after 3b, a prefix view whose `k` is not a boundary of its store fails to compile.
4. **Private helpers.** `LsmStore::encode`/`decode`, `LsmIndex::encode_key`/`encode_value`,
   and `concat_eq` for the `PrefixView` key/value check. `LsmStore` and `LsmIndex` gained
   `PhantomData` fields.
5. **`write_to`** uses a bounds-checked `copy_from_slice` rather than `copy_nonoverlapping`.
6. **The contiguity test is two tests,** one per mask. A single generic test would instantiate
   prefix reads at boundaries its mask lacks, which no longer compiles.
7. **`ascent_nary` serializes** its tests that set the global memtable limit, with a mutex.

### Phase 3b: packing on

- Make `store_layout` return `layout` and delete `slot_layout` and the "same bytes as today"
  test. Nothing else changes, so any failure is in `layout` or in code that wrongly assumed
  one column per word (look first at the `prefix_bytes(k) / 8` sites).
- Module docs: describe the packed layout. In 3a they described the new code, with the
  layout given as one column per word.

Gate for 3b: the same tests as 3a (the round trips, the contiguity test and `ascent_nary` now
check the packed layout), and `cargo bench -p lsm -- --baseline before`. Packing should be
neutral or faster than the baseline, since keys are shorter. Leave the large target to
Phase 5.

**Status: done** in "lsm: pack tuple columns at their natural width". The change is
`lsm/src/ascent_provider.rs` alone (+9/−78), and no code assumed one column per word. The
`prefix_bytes(k) / 8` sites are correct, since segment ends are multiples of 8.

The gates pass: tests in debug and release, all of `ctadl-ascent`, clippy and fmt. Benches
against `before`, at load about 8:

| Bench | Change |
| --- | --- |
| `provider_tc/lsm` | −6.1 % |
| `mmap_lsm/insert` | −5.4 % |
| `mmap_lsm/get_refs_hit` | −16.9 % |
| `disk_lsm/insert` | +60 %, wide interval, untouched code (see 3a) |
| Others | No change |

Deviations:

- **glxy run.** It ran once here as an early check, before Phase 5. At load 27 the results,
  in `/Volumes/Shampoo/lsm-pack-results/phase3b/`, were:

  | Measure | Phase 3b | Change |
  | --- | --- | --- |
  | Dump | Equals Phase 0's | |
  | Row counts | Equal Phase 0's | |
  | Run files, final | **71,591,552 bytes** | −35.6 % from Phase 1/3a, −46.8 % from Phase 0 |
  | `lsm::memory_peak` | **35.8 MiB** | Phase 0: 42.0 |
  | Wall | 45.1 s | |
  | Fixpoint | 41.6 s | |
  | Peak footprint | 1.09 GiB | |

- **`store_layout`** now only forwards to `layout`. Fold it into `layout` in a later cleanup.

## Phase 4: observability (§4.6; R16)

- `lsm/src/ascent_provider.rs`: `LsmStore::tuple_bytes() -> usize` (`W * 8`), and
  `LsmIndex::key_bytes()` / `value_bytes()` (`KW * 8`, `VW * 8`), as `const fn` associated to
  the instantiation, so that they are readable without a value too.
- `ctadl-index/src/index_engine/mod.rs:1070`: `StoreSizes` gets `bytes_per_row: Vec<(&'static
  str, usize)>`. The serial/parallel macro (`:2636`) sets it empty.
- `ctadl-index/engines/lsm/src/lib.rs:45`: fill it for all six LSM relations from
  `self.__<rel>_ind_common.tuple_bytes()`. A local macro, like `stores!` there, avoids
  repeating the list.
- `ctadl-index/src/index_engine/profile.rs:23`: `IndexProfile.bytes_per_row: BTreeMap<String,
  usize>`, which `run_engine!` (`mod.rs:2771`) fills from `sizes.bytes_per_row`. Add it to the
  `[relsizes]` debug line too.

Tests:

- A unit test in `lsm` that `LsmStore::<(u32, u64, u32, u32, u32), {1 << 3}, 3>::tuple_bytes()
  == 24`, and that `W = 2` does not compile (a `compile_fail` doctest; this also covers the
  "mismatched instantiation fails to compile" bullet of §4.4).
- In `ctadl-ascent/src/index_engine_tests.rs`, extend `lsm_storage_matches_memory` (or add a
  test beside it) to assert that the LSM run's profile reports `locals_key` and
  `locals_key_wild` at 24, `reach_vp`, `prog_store` and `alias_of_formal` at 16, and
  `actual_param` at 32 (§3.3), and that the memory engine reports none. This pins the expected
  widths, so that a change to a column type or to an index list shows up as a test diff.

Gate: as for 3b.

**Status: done** in "Report LSM bytes per row in the index profile". The gates pass: the
`lsm`, `ctadl-index` and `ctadl-ascent` tests, clippy with `-D warnings`, and fmt.

`profile.json` on glxy (`/Volumes/Shampoo/lsm-pack-results/phase4/`) reports bytes per row:

| Relation | Bytes per row |
| --- | --- |
| `locals_key` | 24 |
| `locals_key_wild` | 24 |
| `reach_vp` | 16 |
| `prog_store` | 16 |
| `alias_of_formal` | **24** |
| `actual_param` | 32 |

Deviations:

1. **`alias_of_formal` is 24, not 16.** The aliasing summary rule probes it by its first two
   columns (`ctadl-index/src/index_engine/mod.rs:1906`). That puts a boundary after
   `{FunctionId, FlowVariable}` (12 bytes), which pads to 16, and `Root` takes another word.
   §3.3 had assumed no boundary. **spec.md §3.3 is corrected**, and the test expects 24.
2. **Reading the widths.** `tuple_bytes()`, `key_bytes()` and `value_bytes()` take no `self`.
   The lsm engine reads a store's width through a generic helper,
   `fn tuple_bytes(&LsmStore<T, P, W>)`.
3. **Width check.** The check that `W` matches now lives in associated consts,
   `WIDTH_MATCHES` and `WIDTHS_MATCH`. So a wrong `W` fails to compile even when only the
   width is read.
4. **One relation list.** A single `lsm_stores!` macro in `engines/lsm` lists the six
   relations, for both `store_sizes` and `dump_stores`. The old `stores!` became `dump_rows`.
5. **`profile.json` format.** `bytes_per_row` is always written, as `{}` for the other
   engines, because `IndexProfile` is serialize-only. The widths go on the second
   `[relsizes]` line.
6. **Stale release binary.** The first glxy run used a stale release binary, and was redone
   after a rebuild. Phase 5 uses its own `CARGO_TARGET_DIR` and checks its binary.

## Phase 5: verification on the large target (§4.8 "Engine parity", "Space and time"; R6, N1, N2)

Use the target and commands of Phase 0.

- **R6:** `scripts/fact-dump-diff.sh` of the `lsm` dump against the serial dump finds no
  difference, and neither does a diff against Phase 0's `lsm` dump.
- **N1:** `profile.json`'s `bytes_per_row` matches §3.3. Report `memory_peak`, peak RSS, and
  the total run-file bytes against Phase 0. The run-file total should fall by roughly the
  ratio of §3.3's second table, weighted by row counts from `profile.json`'s `relations`.
- **N2:** `cargo bench -p lsm -- --baseline before`, and fixpoint time against Phase 0.
  Repeat each timing three times, and report the spread as the noise.
- Write the numbers into the PR description, together with the row counts that weight them.

**Status: done.** It measured `d51f1d578` from a fresh target dir. Results and commands are in
`/Volumes/Shampoo/lsm-pack-results/phase5/README.md`.

**R6.** The new `--lsm` dump equals Phase 0's serial and lsm dumps, and the new serial dump
equals Phase 0's serial. All 58 relations are byte-identical once sorted.

**N1.** `bytes_per_row` matches §3.3's "Packed" column (as corrected for `alias_of_formal`).
Means of 3 runs:

| Measure | Before | After | Change |
| --- | --- | --- | --- |
| Run files, final | 134,482,432 bytes | 71,591,552 bytes | −46.8 % (−35.6 % vs Phase 1) |
| `lsm::memory_peak` | 42.0 MiB | 35.8 MiB | −14.8 % |
| Max RSS | 1.372 GiB | 1.292 GiB | −5.9 % |
| Peak footprint | 1.115 GiB | 1.093 GiB | −2.0 % |

The run-file ratio is 0.532 measured, and §3.3 predicts 0.541 weighted by all rows. The gap
comes from which rows reach runs: `prog_store` and `alias_of_formal` never leave the memtable,
and about 125k tail rows (mostly `actual_param`) are still in memtables at the end. Those
relations shrink least. Weighted by the rows actually in the 7 final runs, the prediction
equals the measurement to the byte.

**N2.** No change. Three interleaved before/after pairs at load 6–9 give mean wall 42.95 s
before and 42.92 s after, and mean fixpoint 40.99 s and 40.97 s, against ±3 % noise. Benches
against `before`, median of 3 runs:

| Bench | Change |
| --- | --- |
| `provider_tc/lsm` | −5.7 % |
| `mmap_lsm/get_refs_hit` | −7.3 % |
| Other `mmap_lsm` | Within ±1.5 % |
| `provider_tc/default` | −3.6 % (untouched code: machine drift since Phase 0) |

The `disk_lsm/insert` "regression" seen since 3a is disk noise. The bench mostly times fsync on
the shared USB disk. Run back to back under the same conditions, 72bb87564 and HEAD measure
503.0 ms and 502.7 ms.

Deviation: in N2 every timing ran as a before/after pair at similar load, rather than being
compared with Phase 0's absolute times. The `disk_lsm` question was settled with a same-session
A/B run of the old commit.

## Order and dependencies

```
Phase 0 ──► Phase 1 ──┐
           Phase 2 ──► 3a ──► 3b ──► Phase 4 ──► Phase 5
```

Phases 1 and 2 are independent of each other. 3a needs Phase 2's `Layout` type but not its
packing rules, and it should follow Phase 1, so that its run-file bytes can be checked against
Phase 1's measurement. Phase 5 needs all the others, and Phase 0 must be measured before any of
them.

## Requirement coverage

| Req | Proved by |
| --- | --- |
| R1, R2, R3 | Phase 2 layout table; Phase 3 round trips (pad bytes zero); Phase 4 width test |
| R4 | Phase 3 contiguity test; `ascent_nary` with several prefix indices |
| R5 | By construction (`decode_at` returns `&Column`); `ref_at`'s debug alignment assertion runs in every debug test |
| R6 | `ascent_nary`; `lsm_storage_matches_memory`; Phase 5 dump diff |
| R7 | Phase 3 round trips (equal ⇔ equal bytes) |
| R8 | Round trips over arities 0–6, pair columns and all masks; `ascent_redeclare` (`counting`/`seeded`) |
| R9 | No CLI or parquet code touched; Phase 5 dump diff |
| R10, R11 | New `lsm_col!` form; `compile_fail` doctests |
| R12 | `ref_at`'s `debug_assert!(is_valid)`; debug test runs |
| R13–R15 | Phase 1 tests |
| R16 | Phase 4 tests |
| N1, N2 | Phase 5 |
| N3 | `layout` is a `const fn` of sizes, alignments and mask only |
| N4 | Phase 3 safety-contract rewrite; review of every `unsafe` block's comment |
| N5 | Column declarations stay in `ctadl-index`; `lsm` gains no CTADL dependency |
