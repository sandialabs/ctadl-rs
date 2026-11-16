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
| signature | The set of an instruction's input facts (D3), canonicalized, with each reference to the instruction itself replaced by a marker for "this instruction". |
| canonical instruction | An instruction named by content: its function's name and its signature. Instructions of one function with equal signatures have the same name. |
| canonical row | A table row with every function id replaced by the function's name, every instruction id by its canonical instruction, every import id by the import's name, and every span id removed, so it has the same value in any run that derived the same fact. |
| multiplicity | How many rows of a table map to one canonical row. A table may hold repeated rows, since the input tables are written without removing duplicates (`model_matches.rs:340-345`, `models/codegen.rs:34`), and canonicalizing can collapse further rows into one. |

## Requirements

### Command

- R1. **Invocation.** `ctadl diff-index A B` compares two indexes. Each of A and B is a
  project name in the store or a path to an index directory.
- R2. **Exit codes.** 0 when the indexes are the same, 1 when they differ, 2 on an error
  (unreadable file, incompatible format).
- R3. **Comparability.** If either side's `index_config.json` is missing, or its format version
  is not the version this build reads, the command exits 2 without comparing rows.

### What is compared

- R4. **Every table.** Every table `schema.rs` declares that exists on either side is
  compared. A table present on one side only is a difference.
- R5. **Nothing skipped silently.** A `.parquet` file in either directory that no schema
  names is listed as not compared. Files that are not parquet are not read, apart from the
  format version (R3).
- R6. **Content with multiplicity.** Two tables are equal when they hold the same canonical
  rows, each with the same multiplicity. Row order, row-group layout, function numbering,
  instruction numbering and interning order do not affect the result.
- R7. **Instructions are named by their facts.** Instruction ids are not compared: front
  ends may assign them in any order, even across two imports of one artifact. Every
  instruction id, in a column or inside a call-argument variable, is replaced by its
  canonical instruction. So if `foo` calls `bar` at two sites with the same arguments and B
  loses one, `call` differs in multiplicity; if two sites with different arguments swap
  targets, `call` differs in rows.
- R8. **Other ids.** Span ids are dropped, since they number spans per import. Import ids are
  compared by the import's name.

### Output

- R9. **First line.** The first line of output is `IDENTICAL`, or `DIFFERENT` followed by
  how many tables differ.
- R10. **Per-table summary.** For each table, it prints the row counts on each side, the
  number of canonical rows only in A, only in B, and on both sides with different
  multiplicity. Equal tables print one line each.
- R11. **Samples.** For each differing table, at most `--limit N` (default 20) differing
  rows per side are printed, by content, in content order, each with its multiplicity on
  both sides. The count of rows not printed
  is always printed.
- R12. **Deterministic.** The output for the same two indexes is byte-identical across
  runs, and swapping A and B swaps the sides in the output and changes nothing else.
- R13. **Filters.** `--table NAME` (repeatable) limits the comparison to those tables;
  `--function SUBSTR` limits rows to those whose function name contains it. A filter that
  is applied is named in the output.
- R14. **JSON.** `--json` prints the same information as one JSON document on stdout, with
  the same exit codes.

### Correctness

- R15. **Proved core.** The function that diffs two sorted sequences as multisets is proved
  with Kani, for sequences up to a stated bound, to satisfy:
  - diffing a sequence with itself reports nothing;
  - swapping the inputs swaps the sides;
  - a value is reported exactly when its count in A differs from its count in B, and the
    reported counts are those counts;
  - no value is reported twice.
- R16. **Property tests.** The same properties hold under proptest on unbounded inputs in
  ordinary `cargo test`.
- R17. **Kani is pinned in Nix, and the proof is rechecked.** The flake provides one pinned
  Kani version, in the dev shell and in a `checks` derivation that runs the harnesses with
  no network. The nightly workflow builds that check, and a failed proof fails the night.
- R18. **Metamorphic tests.** On real indexes written by the test suite:
  - a copy whose rows are shuffled, whose row groups are split differently, whose
    function ids are renumbered consistently, or whose instruction and span ids are
    renumbered arbitrarily (consistently across tables) reports `IDENTICAL`;
  - two indexes of one program written by processes that intern in opposite orders report
    `IDENTICAL`;
  - deleting one row of a table that is not an instruction's input (D3), `function_id` or
    `import_id` reports exactly its canonical row, with multiplicities n and n-1;
  - deleting one row of an input table renames its instruction (D3), so that table differs
    and every differing row names the edited instruction;
  - swapping the call targets of two call sites of one function whose arguments differ
    reports the two original `call` rows only in A and the two swapped rows only in B, among
    the other rows of those two renamed instructions.
- R19. **Agrees with an independent query.** After the regression cases run, each corpus
  index is diffed against the next one by name. For every table with no instruction id
  anywhere in a row (`formal_param`, `callee_resolvents`, `summary`, `globals`,
  `global_flow`, `paths`, `external_function`, `function_id`, `import_id`; not `assign`,
  whose variables can hold a call argument), the per-table counts equal duckdb's: a
  `GROUP BY` with `count(*)` on each side over the same canonical columns (function and
  import ids joined to names, spans left out), then a full outer join. This is checked in
  both directions. duckdb comes from the flake, in the dev shell and in the regression
  check's environment.

### One implementation

- R20. **Shared core.** `assert_same_index` and `xtask regression --compare-paths` use the
  same multiset core. `assert_same_index` covers every `IndexResult` field, including
  `const_reaches`, `intent_pair` and `global_flow`.
- R21. **Docs.** `docs/debugging.md` tells readers to compare two index runs with
  `ctadl diff-index` and not with a hand-written script. The duckdb recipe stays, for ad-hoc
  questions the command does not answer. `docs/spec.md` F12 names the command.
- R22. **Unchanged.** The on-disk index format, `INDEX_FORMAT_VERSION`, the `CTADL_DUMP_RELATIONS`
  dump and `scripts/fact-dump-diff.sh` do not change.

### No ids in names

- R23. **Bridge temporaries are named by content.** A model-match bridge's temporary local is
  named from the bridge's callee name, its rule (model file name and rule label) and the
  callee index, with no function or instruction id. Two indexes of one program with the
  same model files give their bridge temporaries the same names however the files' paths
  were spelled.
- R24. **Same flows.** Renaming bridge temporaries changes no derived flow: every index table
  is equal, under this diff, to the one written before the rename, except for the
  temporaries' names.

## Design

### D1. The core: `multiset_diff`

A new workspace crate, `multiset-diff`, with no dependencies and no dev-dependencies, so
the Nix check can run Kani on it alone without vendoring the workspace's crates (D8). The
proptest properties live in `ctadl-index`'s tests, which call it.

```rust
pub struct Delta<'a, T> { pub row: &'a T, pub count_a: usize, pub count_b: usize }

/// Both inputs sorted by `cmp`. Yields each value whose counts differ, once, in `cmp` order.
pub fn multiset_diff<'a, T, F>(a: &'a [T], b: &'a [T], cmp: F) -> impl Iterator<Item = Delta<'a, T>>
where F: Fn(&T, &T) -> Ordering;
```

It is a single merge pass: advance past the run of equal values on each side, and report a
value whose two run lengths differ (a missing run has length 0). It allocates nothing. Its precondition, sorted input, is established by
its only callers, which sort immediately before calling, and is checked with a
`debug_assert`.

The Kani harnesses live beside it under `#[cfg(kani)]`, over `u8` values and a total order,
with a stated length bound (R15). The proptest versions are the same properties as ordinary
tests (R16).

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
- `load` maps each `InsnId` to its canonical instruction (D3), including the instruction
  inside a call-argument `FlowVariable`, which becomes (canonical instruction, argument
  index). It drops every `FileSpanId`, and maps each `ImportId` to its name through that
  side's `import_id.parquet` (R8). `Canon` types cannot contain an `InsnId`, `FileSpanId`
  or `ImportId`.
- `load` returns the canonical rows unsorted and with repeats; the repeats are the
  multiplicities (R6).
- `function_id` itself is compared by name only, since its ids are
  what the other tables have been rewritten away from.
- `render` prints by content, the way `index_engine/dump.rs` does (`dump.rs:1-12`); the
  column printers there are shared, not copied.

The schema modules are declared in one macro invocation in `schema.rs` that emits each
module (as `save_load!` does today, `schema.rs:22`) and a `Table` enum with one variant per
module. The diff dispatches with an exhaustive `match` on `Table` that names each module's
`DiffTable` impl, so a table added to the schema without one fails to compile (R4).

### D3. Canonical instructions

Instruction ids are project-global (`facts.rs:1178`), so one map per side,
`InsnId -> CanonInsn`, renames every instruction reference in that side's tables.

- **Input facts.** An instruction's signature is built from the tables written before the
  fixpoint that have an instruction column: `call`, `actual_param`, `call_target_assign` and
  `callee_info` (`index_engine/mod.rs:160-214`). These state the program; they do not
  depend on what the analysis derived. The result tables (`resolved_call`, `intent_pair`,
  `assign`, ...) are never part of a signature, so a change in results never renames an
  instruction, and "same instruction, different result" shows as a changed result row.
- **Building it.** For each input row that names instruction `i`, canonicalize the row with
  function ids as names, `i` itself (as a column or inside a call-argument variable) as
  the marker `This`, and the instruction column removed. The signature of `i` is the sorted,
  deduplicated set of those rows, tagged with their table. `CanonInsn` is (function name,
  interned signature). The signature is interned in the diff's process and compared by
  content, never by hash, so two instructions get one name only when their fact sets are
  equal.
- **No other instructions inside a signature.** No writer of an input table puts a
  call-argument variable of another instruction in a row: `call` has no variable column;
  `callee_info`'s receiver is a local (`ctadl-ascent/src/codegen/mod.rs:701`);
  `call_target_assign`'s destination is a local or an argument of the row's own call
  (`mod.rs:961-969`, `:1168-1183`); `actual_param`'s variable is a translated expression, a
  return variable, a formal, or a bridge temporary (`mod.rs:1207-1219`,
  `codegen/model_matches.rs:352-380`, `languages/jni.rs:1223`). If an input row ever names
  a second instruction, `load` fails with an error naming the row and the diff exits 2. It
  does not refine names in rounds.
- **No facts.** An instruction in no input table has the empty signature; all of a
  function's such instructions share one name, and their rows differ only by multiplicity.
- **Cost of naming by content.** A change to an instruction's input facts (an added argument,
  say) changes its name, so every row about that instruction shows as removed and added.
  That is accepted: the instruction did change.
- **Memory.** The two maps, one entry per instruction, stay loaded while the tables are
  diffed. The signatures are built from the input tables, one table at a time, before any
  table is diffed. The cost is about 16 bytes per instruction per side, plus about 130 bytes
  per distinct signature, shared by both sides: roughly 2 GB at 20M call sites.

### D4. Sorting: by intern id, then by content for printing

Both sides load into one process, so one interner: two canonical rows are equal exactly when
their contents are equal, whatever order either run interned in. The diff therefore sorts each
side with the derived `Ord` (integer compares on intern ids), runs `multiset_diff`, and sorts
only the differing rows with `content_cmp` before printing. This keeps the full-table sort
cheap on tables of tens of millions of rows and keeps the output independent of load order
(R6, R12).

Invariant: no two canonical rows from different processes are compared by intern id. Every
canonical row the diff compares was interned in the diff's own process.

### D5. Driver and preconditions

- Resolve A and B: an existing directory is used as is; otherwise the name is looked up as a
  project, and its `index/` directory is used (R1).
- Read the format version from both `index_config.json`. Refuse if either is missing or is not
  `INDEX_FORMAT_VERSION` (`project.rs:186`): the decoders read only the current format and
  panic on any other (`project.rs:949-953`), so two equal old versions are refused too (R3).
- List the `.parquet` files in both directories. Each is a known table or is reported as not
  compared (R5).
- Build both sides' instruction maps from the input tables (D3).
- Compare tables one at a time, freeing each pair before loading the next, so peak memory
  tracks the largest table, not the whole index. Each table loads with the schema's
  `try_load`, whose `read_vec` grows its `Vec` by doubling and then shrinks it
  (`parquet.rs:164-169`), and is then mapped to canonical rows. So while B loads, memory holds
  A's canonical rows, plus B's raw rows briefly at up to 3x, plus B's canonical rows. The
  instruction maps (D3) sit on top. A canonical `assign` row is about 36 bytes, so 30M rows
  peaks at several GB. That is accepted: tables are not partitioned and not streamed.

### D6. Output

A `Report` struct holds, per table: row counts on each side, the three difference counts, and the
sampled rows rendered as strings. The text and the `--json` forms are two printers of the same
`Report` (R14), so they cannot disagree. Tables print in name order. The sample limit
applies after the content sort, so the printed rows are the first N by content, and the
remainder is printed as a count (R11).

### D7. Consumers

- The `Command` enum (`ctadl-ascent/src/main.rs:31`) gains `DiffIndex`; its implementation
  sits next to `inspect_project` in `ctadl-ascent/src/cli/mod.rs`.
- `assert_same_index` keeps its stats checks and replaces `canonical()` with a call to
  `multiset_diff` per `IndexResult` field (all of them), failing with the rendered
  differences rather than a whole-vector `assert_eq!` (R20).
- `compare_paths` loads each side as today and calls `multiset_diff` instead of
  `BTreeSet::difference` (R20).

### D8. Kani in the flake

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

### D9. Bridge temporary names

`temp_for` (`ctadl-ascent/src/codegen/model_matches.rs:420-424`) names a bridge temporary
`$bridge{func_id}_{insn_id}#{index}`, ids that differ between runs. It becomes
`$bridge[{file}:{label}]{callee}#{index}`: `file` is the model file's name without its
directory, `label` the rule's label (`portcullis/mod.rs:345-353`), `callee` the bridge's
`to` name, which `codegen_bridges` already holds (`model_matches.rs:240`). `emit_bridge`
takes the callee's name, or the `ResolvedBridge`, instead of the callee's id alone.

Why it stays unique: a local is scoped to its function, so the caller's id adds nothing.
`emit_bridge` makes a fresh site per grounded bridge (`model_matches.rs:305`), so the
instruction only tells bridges of one caller apart. The emitter merges all ports of one rule
for one `(from, to)` into one bridge, across per-import emission
(`portcullis/emit.rs:508-536`, `portcullis/mod.rs:608-619`), so (rule, callee) identifies a
bridge within its caller, and the index identifies its port. The comment at
`model_matches.rs:296-298` changes to say the temporaries are keyed on the bridge's content.

The one way two bridges share a name: model files with the same file name in different
directories, rules with the same label, bridging the same `(caller, callee)`. They then
share a temporary, and the analysis merges their flows: less precise, never missing a flow.
This is accepted. Nothing else creates or parses `$bridge` names.

### D10. Help and docs

Code and `--help` must not cite requirement numbers from this spec.

## Concerns

None open.
