# `ctadl diff-index`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines it into requirements and a design consistent with
`docs/spec.md` (F12 inspection, N3 no silent loss, N5 reproducibility, §5 verification). It
does not sequence the work.

## Background

- An index is a directory of parquet tables plus `index_config.json` and `profile.json`
  (`ctadl-import/src/project.rs:869`). Each table is a module in
  `ctadl-index/src/facts/schema.rs` with typed `Record`, `COLUMNS`, `FILENAME` and
  `try_save`/`try_load` (`schema.rs:22-37`). `index` writes the pre-fixpoint tables from
  `IndexFacts::try_save` (`ctadl-index/src/index_engine/mod.rs:160`) and the results from
  `IndexResult::try_save` (`mod.rs:667`). `schema.rs` also declares `taint`, `taint_edge` and
  `manifest_*` (`schema.rs:149-258`), which `index` does not write. Old index directories
  hold files no current schema names (for example `update_sans_path.parquet`).
- Row order of `assign`, `summary`, `paths` and `index_source_map` differs between runs; only
  their sets are stable (`docs/debugging.md:122`).
- Interned values (`Str`, `Path`, `FlowVariable`'s local/global names, `CallTargetObject`)
  are ordered by intern id, not by content (`ctadl-index/src/facts.rs:26-31`, `:192-194`);
  `content_cmp` orders them by what they denote (for example `facts.rs:1404`).
- `FunctionId` is a position in `function_id.parquet` (`facts.rs:2032-2070`). It appears as a
  column and also inside `CallTargetObjectKind::FunctionId` (`facts.rs:1599-1601`).
  `InsnId` is a project-global counter with no content name (`facts.rs:1178`); it appears as
  a column and inside call-argument `FlowVariable`s (`CallArgId`, `facts.rs:1109`).
  `FileSpanId` is meaningful only with its `ImportId` (`schema.rs:180-193`).
- `index_config.json` records the format version, call policy, import hashes and engine
  options (`project.rs:200-246`); `profile.json` records every relation's tuple count
  (`ctadl-index/src/index_engine/profile.rs:32`).
- Comparers today: `scripts/fact-dump-diff.sh` (relation dumps, `sort -u`, so sets);
  `assert_same_index`/`canonical()` (`ctadl-ascent/src/index_engine_tests.rs:73`, `:188`),
  which skips `const_reaches`, `intent_pair` and `global_flow`; `xtask regression
  --compare-paths` (`xtask/src/regression.rs:365`), `paths` only, as sets; and a duckdb
  `EXCEPT` recipe (`docs/debugging.md:178`), also sets.

## Terms

| Term | Meaning |
|---|---|
| side | One of the two index directories, A (left) and B (right). |
| canonical row | A table row with every function id replaced by the function's name, every import id by the import's name, and every instruction and span id removed, so it has the same value in any run that derived the same fact. |
| multiset diff | For each distinct canonical row, its count in A and in B; a row differs when the counts differ. |

## Requirements

### Command

- R1. **Invocation.** `ctadl diff-index A B` compares two indexes. Each of A and B is a
  project name in the store or a path to an index directory.
- R2. **Exit codes.** 0 when the indexes are the same, 1 when they differ, 2 on an error
  (unreadable file, incompatible format).
- R3. **Comparability.** If the two `index_config.json` format versions differ, or either is
  missing, the command exits 2 without comparing rows.
- R4. **Context is reported.** Differences in call policy, engine options and import hashes
  are printed before any row difference. They are reported, not counted: on their own they
  do not make the exit code 1.

### What is compared

- R5. **Every table.** Every table `schema.rs` declares that exists on either side is
  compared. A table present on one side only is a difference.
- R6. **Nothing skipped silently.** A file in either directory that no schema names, other
  than `index_config.json` and `profile.json`, is listed as not compared.
- R7. **Relation counts.** The per-relation tuple counts in `profile.json` are compared, and
  a differing count is a difference. Times and memory figures are not compared.
- R8. **Multisets of content.** Two tables are equal when they hold the same canonical rows
  with the same multiplicities. Row order, row-group layout, function numbering and
  interning order do not affect the result.
- R9. **Ids with no content name are dropped.** Instruction ids and span ids are not
  compared: front ends may assign them in any order, even across two imports of one artifact.
  They are removed from every column, including the instruction inside a call-argument
  variable, whose argument index is kept. Import ids are compared by the import's name. The
  output's first lines say that a fact is compared by its function, not by its instruction.

### Output

- R10. **First line.** The first line of output is `IDENTICAL`, or `DIFFERENT` followed by
  how many tables and relation counts differ.
- R11. **Per-table summary.** For each table, it prints the row counts on each side, the
  number of canonical rows only in A, only in B, and present on both sides with different
  multiplicity. Equal tables print one line each.
- R12. **Samples.** For each differing table, at most `--limit N` (default 20) differing
  rows per side are printed, by content, in content order. The count of rows not printed
  is always printed.
- R13. **Deterministic.** The output for the same two indexes is byte-identical across
  runs, and swapping A and B swaps the sides in the output and changes nothing else.
- R14. **Filters.** `--table NAME` (repeatable) limits the comparison to those tables;
  `--function SUBSTR` limits rows to those whose function name contains it. A filter that
  is applied is named in the output.
- R15. **JSON.** `--json` prints the same information as one JSON document on stdout, with
  the same exit codes.

### Correctness

- R16. **Proved core.** The function that diffs two sorted sequences as multisets is proved
  with Kani, for sequences up to a stated bound, to satisfy:
  - diffing a sequence with itself reports nothing;
  - swapping the inputs swaps the sides;
  - for every value, its reported counts equal its counts in A and in B;
  - no value is reported twice.
- R17. **Property tests.** The same properties hold under proptest on unbounded inputs in
  ordinary `cargo test`.
- R18. **Metamorphic tests.** On real indexes written by the test suite:
  - a copy whose rows are shuffled, whose row groups are split differently, whose
    function ids are renumbered consistently, or whose instruction and span ids are
    renumbered arbitrarily reports `IDENTICAL`;
  - two indexes of one program written by processes that intern in opposite orders report
    `IDENTICAL`;
  - deleting one row reports exactly that row only in A; duplicating one row reports
    exactly that row with multiplicity 1 vs 2.
- R19. **Agrees with the existing comparers.** On the regression corpus, the tables both
  can compare give the same verdict as the duckdb `EXCEPT` recipe, after accounting for
  multiplicity.

### One implementation

- R20. **Shared core.** `assert_same_index` and `xtask regression --compare-paths` use the
  same multiset core. `assert_same_index` covers every `IndexResult` field, including
  `const_reaches`, `intent_pair` and `global_flow`.
- R21. **Docs.** `docs/debugging.md` tells readers to compare two index runs with
  `ctadl diff-index` and not with a hand-written script. The duckdb recipe stays, for ad-hoc
  questions the command does not answer. `docs/spec.md` F12 names the command.
- R22. **Unchanged.** The on-disk index format, `INDEX_FORMAT_VERSION`, the `CTADL_DUMP_RELATIONS`
  dump and `scripts/fact-dump-diff.sh` do not change.

## Design

### D1. The core: `multiset_diff`

A new module in `ctadl-index` (it has no dependency on ctadl types):

```rust
pub enum Side { A, B }
pub struct Delta<T> { pub row: T, pub count_a: usize, pub count_b: usize }

/// Both inputs sorted by `cmp`. Yields each value whose counts differ, once, in `cmp` order.
pub fn multiset_diff<T, F>(a: &[T], b: &[T], cmp: F) -> impl Iterator<Item = Delta<&T>>
where F: Fn(&T, &T) -> Ordering;
```

It is a single merge pass: advance through the run of equal values on each side, compare
the run lengths. It allocates nothing. Its precondition, sorted input, is established by
its only callers, which sort immediately before calling, and is checked with a
`debug_assert`.

The Kani harnesses live beside it under `#[cfg(kani)]`, over `u8` values and a total order,
with a stated length bound (R16). The proptest versions are the same properties as ordinary
tests (R17).

### D2. Canonical rows

A trait implemented once per schema table:

```rust
trait DiffTable {
    const NAME: &'static str;         // the schema module's FILENAME
    type Canon: Ord + Clone;          // the canonical row
    fn load(dir: &Path, names: &FunctionNames) -> Result<Vec<Self::Canon>, Error>;
    fn content_cmp(a: &Self::Canon, b: &Self::Canon) -> Ordering;
    fn render(row: &Self::Canon, out: &mut String);
    fn function(row: &Self::Canon) -> Option<&Function>;   // for --function
}
```

- `load` calls the table's existing `try_load` and maps each `FunctionId` to its `Function`
  through that side's own `function_id.parquet`. This includes the `FunctionId` inside
  `CallTargetObjectKind::FunctionId`. `Canon` types are built so that they cannot contain a
  `FunctionId`: canonical counterparts of `CallTargetObject` hold a `Function` instead.
- `load` drops every `InsnId` and `FileSpanId`, and maps each `ImportId` to its name through
  that side's `import_id.parquet`. A call-argument `FlowVariable` becomes a canonical variable
  that holds only the argument index (R9). `Canon` types cannot contain an `InsnId`,
  `FileSpanId` or `ImportId`.
- Dropping instructions and spans loses no counts: rows that differ only in them become one
  canonical row with a higher multiplicity. What it gives up is seeing facts that move
  between instructions of one function while their counts stay the same.
- `function_id` itself is compared by name only, as a multiset of names, since its ids are
  what the other tables have been rewritten away from.
- `render` prints by content, the way `index_engine/dump.rs` does (`dump.rs:1-12`); the
  column printers there are shared, not copied.

The schema modules are declared in one macro invocation in `schema.rs` that emits each
module (as `save_load!` does today, `schema.rs:22`) and a `Table` enum with one variant per
module. The diff dispatches with an exhaustive `match` on `Table` that names each module's
`DiffTable` impl, so a table added to the schema without one fails to compile (R5).

### D3. Sorting: by intern id, then by content for printing

Both sides load into one process, so one interner: two canonical rows are equal exactly when
their contents are equal, whatever order either run interned in. The diff therefore sorts each
side with the derived `Ord` (integer compares on intern ids), runs `multiset_diff`, and sorts
only the differing rows with `content_cmp` before printing. This keeps the full-table sort
cheap on tables of tens of millions of rows and keeps the output independent of load order
(R8, R13).

Invariant: no two canonical rows from different processes are compared by intern id. Every
canonical row the diff compares was interned in the diff's own process.

### D4. Driver and preconditions

- Resolve A and B: an existing directory holding `index_config.json` is used as is;
  otherwise the name is looked up as a project, and its `index/` directory is used (R1).
- Read both `index_config.json`. Refuse on a version mismatch or a missing file (R3); print
  differences in `call_policy`, `engine` and `import_hashes` (R4).
- List both directories. Every file is a known table, `index_config.json`, `profile.json`,
  or reported as not compared (R6).
- Compare tables one at a time, freeing each pair before loading the next, so peak memory is
  the largest table on both sides, not the whole index.
- Compare `profile.json`'s `relations` maps key by key (R7).

### D5. Output

A `Report` struct holds, per table: counts on each side, the three difference counts, and the
sampled rows rendered as strings. The text and the `--json` forms are two printers of the same
`Report` (R15), so they cannot disagree. Tables print in name order. The sample limit
applies after the content sort, so the printed rows are the first N by content, and the
remainder is printed as a count (R12).

### D6. Consumers

- The `Command` enum (`ctadl-ascent/src/main.rs:31`) gains `DiffIndex`; its implementation
  sits next to `inspect_project` in `ctadl-ascent/src/cli/mod.rs`.
- `assert_same_index` keeps its stats checks and replaces `canonical()` with a call to
  `multiset_diff` per `IndexResult` field (all of them), failing with the rendered
  differences rather than a whole-vector `assert_eq!` (R20).
- `compare_paths` loads each side as today and calls `multiset_diff` instead of
  `BTreeSet::difference` (R20).

### D7. Help and docs

Code and `--help` must not cite requirement numbers from this spec.

## Concerns

- C1. **Kani toolchain.** Kani is not in the flake or `rust-toolchain.toml`, and needs its
  own nightly toolchain. Options: add it to the dev shell and run it in the nightly suite;
  or run it by hand only, with proptest as the gate in `cargo test`. The spec assumes the
  harnesses are checked in and run nightly, and that proptest gates every build.
- C2. **Relation counts as a difference.** R7 makes a changed internal relation count fail
  the diff, as `assert_same_index` does. That is right for engine comparisons; across a rule
  change, internal counts are expected to move while outputs do not. Options: count them
  (assumed), or report them only and add `--outputs-only`.
- C3. **Memory.** Per-table loading keeps both sides of the largest table in memory: `assign`
  at tens of millions of rows is a few GB for both sides. Options: accept it (assumed);
  partition by function name and diff partition by partition, which bounds memory but costs
  a second pass over the files.
- C4. **Multiplicity vs the other comparers.** The dump script and the duckdb recipe use sets.
  If an index legitimately writes duplicate rows, the diff reports a difference they do not.
  The spec assumes duplicates are meaningful and reports them; R19 accounts for it.
