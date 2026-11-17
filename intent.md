# Intent: a verified diff of two index runs

Comparing two `ctadl index` runs is something agents do constantly (serial vs
parallel vs LSM, before vs after a change, build vs build). Each time, the
comparison code is written afresh (pyarrow scripts, duckdb `EXCEPT`, `comm`
over sorted dumps), and each fresh copy can be wrong in the same few ways:
comparing rows in file order, comparing run-specific ids,
dropping NULLs in tagged-union columns, leaving tables out, comparing runs
that are not comparable, or truncating output without saying so.

Ship one small, trustworthy tool for it instead: `ctadl diff-index A B`.

- It compares every table an index run writes, as multisets of rows keyed
  by content, not by row order or run-specific ids. Front ends may assign
  instruction ids in any order (in parallel, say), so an instruction is
  named by its facts: its function and the set of input facts about it.
  Instructions with the same facts share a name, and the diff counts how
  many rows collapse onto each name. Span ids are dropped.
- Adding a table to the index without teaching the diff about it fails to
  compile.
- Before comparing rows, it checks the two runs are comparable (index
  format version).
- It diffs the index's parquet tables and nothing else: not `profile.json`,
  not the index configuration, not relation dumps.
- Its output is meant for an agent to read: a first line that says
  `IDENTICAL` or summarizes per-table counts, then bounded, sorted samples
  that always say how many were left out; a `--json` form; filters by table
  and function; exit codes 0 (same), 1 (different), 2 (error).
- The core that compares two sorted row sequences as multisets is small and
  proved correct with Verus, for inputs of any length. Verus is pinned in a
  Nix derivation, and the proof is rerun nightly from it. The rest is checked with metamorphic
  tests (shuffled rows, renumbered function ids, reversed interning order
  must report no difference; one deleted row must report exactly that
  row; swapped call targets at two distinct sites must be reported) and against the existing comparers.
- The existing comparers in Rust (`assert_same_index` in the engine tests,
  `xtask regression --compare-paths`) use the same core instead of their
  own.
- `docs/debugging.md` tells readers, agents included, to use it rather than
  writing a comparison script.

Model-match bridge temporaries are named with function and instruction ids
in the string. They are renamed by content (callee, rule, index), so the
diff never has to parse a name.

Out of scope for now: diffing `CTADL_DUMP_RELATIONS` TSV dumps
(`scripts/fact-dump-diff.sh` stays), though the core should not preclude it.
