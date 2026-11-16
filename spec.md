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
- `index_config.json` records the format version that the parquet encoding follows
  (`project.rs:186`, `:207`). This command reads it only to refuse mismatched formats; it
  reads no other non-parquet file.
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

### What is compared

- R4. **Every table.** Every table `schema.rs` declares that exists on either side is
  compared. A table present on one side only is a difference.
- R5. **Nothing skipped silently.** A `.parquet` file in either directory that no schema
  names is listed as not compared. Files that are not parquet are not read, apart from the
  format version (R3).
- R6. **Multisets of content.** Two tables are equal when they hold the same canonical rows
  with the same multiplicities. Row order, row-group layout, function numbering and
  interning order do not affect the result.
- R7. **Ids with no content name are dropped.** Instruction ids and span ids are not
  compared: front ends may assign them in any order, even across two imports of one artifact.
  They are removed from every column, including the instruction inside a call-argument
  variable, whose argument index is kept. Import ids are compared by the import's name. The
  output's first lines say that a fact is compared by its function, not by its instruction.

### Output

- R8. **First line.** The first line of output is `IDENTICAL`, or `DIFFERENT` followed by
  how many tables differ.
- R9. **Per-table summary.** For each table, it prints the row counts on each side, the
  number of canonical rows only in A, only in B, and present on both sides with different
  multiplicity. Equal tables print one line each.
- R10. **Samples.** For each differing table, at most `--limit N` (default 20) differing
  rows per side are printed, by content, in content order. The count of rows not printed
  is always printed.
- R11. **Deterministic.** The output for the same two indexes is byte-identical across
  runs, and swapping A and B swaps the sides in the output and changes nothing else.
- R12. **Filters.** `--table NAME` (repeatable) limits the comparison to those tables;
  `--function SUBSTR` limits rows to those whose function name contains it. A filter that
  is applied is named in the output.
- R13. **JSON.** `--json` prints the same information as one JSON document on stdout, with
  the same exit codes.

### Correctness

- R14. **Proved core.** The function that diffs two sorted sequences as multisets is proved
  with Kani, for sequences up to a stated bound, to satisfy:
  - diffing a sequence with itself reports nothing;
  - swapping the inputs swaps the sides;
  - for every value, its reported counts equal its counts in A and in B;
  - no value is reported twice.
- R15. **Property tests.** The same properties hold under proptest on unbounded inputs in
  ordinary `cargo test`.
- R16. **Kani is pinned in Nix, and the proof is rechecked.** The flake provides one pinned
  Kani version, in the dev shell and in a `checks` derivation that runs the harnesses with
  no network. The nightly workflow builds that check, and a failed proof fails the night.
- R17. **Metamorphic tests.** On real indexes written by the test suite:
  - a copy whose rows are shuffled, whose row groups are split differently, whose
    function ids are renumbered consistently, or whose instruction and span ids are
    renumbered arbitrarily reports `IDENTICAL`;
  - two indexes of one program written by processes that intern in opposite orders report
    `IDENTICAL`;
  - deleting one row reports exactly that row only in A; duplicating one row reports
    exactly that row with multiplicity 1 vs 2.
- R18. **Agrees with the existing comparers.** On the regression corpus, the tables both
  can compare give the same verdict as the duckdb `EXCEPT` recipe, after accounting for
  multiplicity.

### One implementation

- R19. **Shared core.** `assert_same_index` and `xtask regression --compare-paths` use the
  same multiset core. `assert_same_index` covers every `IndexResult` field, including
  `const_reaches`, `intent_pair` and `global_flow`.
- R20. **Docs.** `docs/debugging.md` tells readers to compare two index runs with
  `ctadl diff-index` and not with a hand-written script. The duckdb recipe stays, for ad-hoc
  questions the command does not answer. `docs/spec.md` F12 names the command.
- R21. **Unchanged.** The on-disk index format, `INDEX_FORMAT_VERSION`, the `CTADL_DUMP_RELATIONS`
  dump and `scripts/fact-dump-diff.sh` do not change.

## Design

### D1. The core: `multiset_diff`

A new workspace crate, `multiset-diff`, with no dependencies and no dev-dependencies, so
the Nix check can run Kani on it alone without vendoring the workspace's crates (D7). The
proptest properties live in `ctadl-index`'s tests, which call it.

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
with a stated length bound (R14). The proptest versions are the same properties as ordinary
tests (R15).

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
  that holds only the argument index (R7). `Canon` types cannot contain an `InsnId`,
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
`DiffTable` impl, so a table added to the schema without one fails to compile (R4).

### D3. Sorting: by intern id, then by content for printing

Both sides load into one process, so one interner: two canonical rows are equal exactly when
their contents are equal, whatever order either run interned in. The diff therefore sorts each
side with the derived `Ord` (integer compares on intern ids), runs `multiset_diff`, and sorts
only the differing rows with `content_cmp` before printing. This keeps the full-table sort
cheap on tables of tens of millions of rows and keeps the output independent of load order
(R6, R11).

Invariant: no two canonical rows from different processes are compared by intern id. Every
canonical row the diff compares was interned in the diff's own process.

### D4. Driver and preconditions

- Resolve A and B: an existing directory holding `index_config.json` is used as is;
  otherwise the name is looked up as a project, and its `index/` directory is used (R1).
- Read the format version from both `index_config.json`. Refuse on a mismatch or a missing
  file (R3).
- List the `.parquet` files in both directories. Each is a known table or is reported as not
  compared (R5).
- Compare tables one at a time, freeing each pair before loading the next, so peak memory is
  the largest table on both sides, not the whole index.

### D5. Output

A `Report` struct holds, per table: counts on each side, the three difference counts, and the
sampled rows rendered as strings. The text and the `--json` forms are two printers of the same
`Report` (R13), so they cannot disagree. Tables print in name order. The sample limit
applies after the content sort, so the printed rows are the first N by content, and the
remainder is printed as a count (R10).

### D6. Consumers

- The `Command` enum (`ctadl-ascent/src/main.rs:31`) gains `DiffIndex`; its implementation
  sits next to `inspect_project` in `ctadl-ascent/src/cli/mod.rs`.
- `assert_same_index` keeps its stats checks and replaces `canonical()` with a call to
  `multiset_diff` per `IndexResult` field (all of them), failing with the rendered
  differences rather than a whole-vector `assert_eq!` (R19).
- `compare_paths` loads each side as today and calls `multiset_diff` instead of
  `BTreeSet::difference` (R19).

### D7. Kani in the flake

The pinned nixpkgs has no Kani package, and `cargo kani setup` downloads a bundle and a
nightly toolchain, which the Nix sandbox forbids. The flake instead does what that setup does,
from fixed-hash inputs. What setup does, from Kani 0.68.0's `src/setup.rs`:

1. Unpack the release bundle `kani-<ver>-<target>.tar.gz` into `$KANI_HOME/kani-<ver>/`.
   The bundle holds `bin/` (`kani-driver`, `kani-compiler`, `cbmc`, `goto-cc`,
   `goto-instrument`, `goto-analyzer`, `kissat`, `kani-cov`), Kani's library sources,
   `rust-toolchain-version` and `rustc-version`. Bundles exist for all four flake systems
   (aarch64/x86_64, darwin/linux).
2. Symlink `kani-<ver>/toolchain` to the nightly that `rust-toolchain-version` names, after
   checking its `rustc --version` equals `rustc-version`. Kani 0.68.0 needs
   `nightly-2026-08-21` (`rustc 1.100.0-nightly (8925ea358 2026-08-20)`) with `llvm-tools`,
   `rustc-dev`, `rust-src` and `rustfmt`.
3. On NixOS only, patchelf the ELF binaries' interpreter, and `cbmc`'s and the solvers'
   rpath to the C++ runtime (`src/os_hacks.rs`). Nothing on macOS. No Python is installed.

The derivation, `nix/kani.nix`:

- Fetches the bundle per system with a fixed hash, and builds the nightly from the `fenix`
  input the flake already has (`flake.nix:7`), with those four components.
- Lays out `$out/kani-<ver>/` as setup would: the unpacked bundle plus the `toolchain`
  symlink into the store. On Linux, `autoPatchelfHook` replaces step 3. On both, it checks
  that `kani-compiler` resolves `librustc_driver-*` and `libLLVM` from that toolchain's
  `lib/` (on macOS they are `@rpath` loads), and fails the build otherwise.
- Provides `cargo-kani` and `kani` as wrappers that do what `kani-verifier`'s `exec` does
  (`src/lib.rs`): set `KANI_HOME`, prepend `kani-<ver>/bin` to `PATH`, set
  `RUSTUP_TOOLCHAIN` to the bundle's version, and exec `kani-driver` with `argv[0]` set to
  the wrapper's name. This replaces the `kani-verifier` crate, whose only other job is the
  download.
- Pins the version in one place, `nix/kani.nix`. Bumping it means changing the version, the
  nightly date and its manifest hash, and four bundle hashes.

The flake adds that package to `devShells.default` and adds `checks.kani`, which runs
`cargo kani` on a copy of `multiset-diff` alone (its own trivial lockfile, no dependencies,
so nothing to fetch). The nightly workflow builds `checks.kani` the way it builds
`checks.regression` (`.github/workflows/nightly.yml:35`). The harnesses compile only under
`cfg(kani)`, so the ordinary build and `rust-toolchain.toml` are unaffected.

### D8. Help and docs

Code and `--help` must not cite requirement numbers from this spec.

## Concerns

- C1. **Memory.** Per-table loading keeps both sides of the largest table in memory: `assign`
  at tens of millions of rows is a few GB for both sides. Options: accept it (assumed);
  partition by function name and diff partition by partition, which bounds memory but costs
  a second pass over the files.
- C2. **Multiplicity vs the other comparers.** The dump script and the duckdb recipe use sets.
  If an index legitimately writes duplicate rows, the diff reports a difference they do not.
  The spec assumes duplicates are meaningful and reports them; R18 accounts for it.
