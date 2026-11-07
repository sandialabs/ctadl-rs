# Plan: move `--dump-index-graph` from `index` to `inspect` - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. Read `spec.md` first; this file says what to change, in what order,
and what proves it. Where this plan departs from the spec, it says so and why.

## 1. Three corrections to the spec

**C1. F3 is not achievable with `assign_like.sort_unstable()`.** The spec assumes the 5-tuple's
derived `Ord` is a content order. It is not. `FlowVariable` is a packed `u64`, and for a local the
payload is a `Str`, which is an index into a process-global string table handed out in
first-intern order (`facts.rs:36`). When `assign.parquet` is read back, locals are interned in the
file's row order — and `docs/debugging.md` says the row order of `assign` is nondeterministic. So
two indexes of the same program give the same rows in a different order, which gives different
`Str` ids, which gives a different sort. The same applies to the node `BTreeSet` in
`render_index_graph`. The plain sort would make the dump depend on nothing but an interning
accident.

The other two components are fine: `FunctionId` is byte-stable across indexes (`docs/debugging.md`),
and `Path` compares by content already (`tailshare::Seq`'s `Ord` is lexicographic over segments, and
`Symbol = ArcIntern<str>` compares by string).

**Decision: order by content.** Add `FlowVariable::content_cmp`, which compares a local by
`Str::as_str()` — a lock-free `FrozenVec` lookup returning `&'static str`, so no allocation in the
sort — and order both the edges and the nodes with it. F3 then actually holds, across processes.

**C2. Flowy calls `render_index_graph` directly** (`codegen/flowy.rs:386`), contrary to spec §9.1.
What flowy inlines is the file creation and the log line, not the rendering. So the node-order
change in C1 also changes flowy's dump. That is harmless — flowy's edges stay in fixpoint order
because flowy does not sort — but say it in the commit message.

**C3. T6 cannot live in `tests/cli.rs`.** `Cli` is defined in `src/main.rs`, which is the `ctadl`
binary; `tests/` links the library only. The clap tests go in a new `#[cfg(test)] mod tests` at the
bottom of `main.rs`.

Two smaller notes, following the spec as written:

- F5 ("no name is a usage error") is a runtime `bail!`, mirroring the existing `--dump-ir` message,
  not a clap `requires = "name"`. Consistency with the neighbouring flag wins.
- §9.3 stands: hard removal from `index` and `go`, no hidden deprecation stub.

## 2. Files that change

| File | Change |
|---|---|
| `ctadl-ascent/src/facts.rs` | add `FlowVariable::content_cmp` + its unit test |
| `ctadl-ascent/src/graphviz.rs` | add `index_vertex_cmp` / `index_edge_cmp`; order nodes with the former |
| `ctadl-ascent/src/cli/mod.rs` | add `inspect_index_graph`; drop `IndexOptions::dump_index_graph` and the dump block in `index`; drop the `'a` lifetime |
| `ctadl-ascent/src/main.rs` | drop the flag from `IndexArgs` and `GoArgs` and their 3 use sites; add it to `InspectArgs`; wire `inspect_artifact`; new `mod tests` |
| `ctadl-ascent/tests/cli.rs` | T1–T5; fix the one `IndexOptions` literal |
| `docs/debugging.md` | short "dumping the index graph" note |

Untouched on purpose: `examples/flowy.rs`, `codegen/flowy.rs`, `--dump-taint-graph`, the index
format version, `IndexResult`.

## 3. Order of work

Each step leaves the tree building and the existing tests passing. Steps 1–2 are additive, so the
old flag keeps working until step 3, which is the only breaking one.

**Step 0 — baseline.** `cargo test -p ctadl-ascent` and keep the output, so a later failure can be
told from a pre-existing one. All build and test output goes to a file (see §6).

**Step 1 — content ordering.** `facts.rs`: `FlowVariable::content_cmp`, with a doc comment saying
why the derived `Ord` is not usable for anything reproducible. `graphviz.rs`: two comparators built
on it,

```rust
pub fn index_vertex_cmp(a: &IndexVertex, b: &IndexVertex) -> Ordering {
    a.0.cmp(&b.0)                                 // FunctionId: stable across indexes
        .then_with(|| a.1.content_cmp(&b.1))      // FlowVariable: by text, not intern id
        .then_with(|| a.2.cmp(&b.2))              // Path: already content-ordered
}
pub fn index_edge_cmp(a: &AssignRow, b: &AssignRow) -> Ordering { /* dst vertex, then src */ }
```

and in `render_index_graph`, keep the `BTreeSet` for dedup but sort the collected `Vec` with
`index_vertex_cmp` before handing it to `dot::render`. Ships with its own tests; nothing else moves.

**Step 2 — `cli::inspect_index_graph`.** As spec §5.1, next to the other `inspect_*` functions, with
`sort_unstable_by(graphviz::index_edge_cmp)` in place of `sort_unstable()`. Order inside it:
`has_index()` → `check_index_config()` → `IdMap::try_load` → `assign::try_load` → sort → render.
Only those two tables are read (N2). Add the comment on `dump_index_graph_dot` saying its caller is
expected to have sorted.

**Step 3 — remove the flag from the index path.** `IndexOptions`: drop the field, its `Default`
entry, and its binding in the destructuring `let`; drop the dump block at `cli/mod.rs:372`. The
field was the struct's only borrow, so `IndexOptions<'a>` becomes `IndexOptions` — 7 sites mention
it, and five of them are `IndexOptions::default()` and need no edit:
`cli/mod.rs:52,77,103`, `main.rs:944`, `tests/cli.rs:135`, plus `tests/{sarif_uris,
bridging_end_to_end, multi_import_sarif, port_semantics}.rs`. Then `main.rs`: delete
`IndexArgs.dump_index_graph` (406–408), `GoArgs.dump_index_graph` (511–513), and the three uses at
676, 828 and 953.

**Step 4 — add the flag to `inspect`.** `InspectArgs.dump_index_graph: Option<PathBuf>` with
`conflicts_with = "dump_ir"` and the help text from spec §4. In `inspect_artifact`, handle it
*before* the store-file-path dispatch, so a project whose name matches a file on disk still works:
bail if there is no name, else `AnalysisProject::try_load_name` (not `load_or_infer_project` — an
ephemeral project has no index to dump) and call `cli::inspect_index_graph`.

**Step 5 — tests.** §4.

**Step 6 — docs.** A short section in `docs/debugging.md`: how to dump the graph from a finished
index, that two dumps of the same program are now comparable (and that this is the one thing in the
post-fixpoint output that is), and the size warning from spec §8.3. Fix `inspect`'s `name` help
text, which claims to accept a project name — as of step 4 it does, but only with this flag.

## 4. Tests

Unit tests (fast, no store):

- **U1** `flow_variable_content_cmp_ignores_intern_order` (`facts.rs`). Intern `"zz_var"` before
  `"aa_var"` so the intern ids and the text disagree, then assert the derived `Ord` puts `zz` first
  and `content_cmp` puts `aa` first. This is the test that fails if anyone swaps the comparator back
  to `sort_unstable()`.
- **U2** `index_graph_nodes_are_ordered_by_label` (`graphviz.rs`). Same trick, through
  `render_index_graph`: assert the node lines come out in label order.
- **T7** the three existing `graphviz.rs` tests still pass, unchanged.

Store tests in `tests/cli.rs`, wrapped in `run_store_test`, distinct import/project names, built on
the existing `xfer.c` fixture so they stay in milliseconds:

- **T1** `inspect_index_graph_writes_a_dot_file` — import, `cli::index`, dump to a temp path; assert
  the file exists, opens with the legend comment, and has at least one `->` line.
- **T2** `inspect_index_graph_matches_the_in_memory_render` — render the in-memory
  `IndexResult.assign_like` (sorted with `index_edge_cmp`) through `render_index_graph`, and compare
  the `->` lines against the dumped file. Proves the parquet round-trip loses nothing.
- **T3** `inspect_index_graph_is_identical_across_two_indexes` — index the same fixture into two
  projects, dump both, assert byte equality. Caveat to write into the test: the process shares one
  string table, so T3 alone would also pass with the spec's naive sort; U1 is what covers the part
  T3 cannot see.
- **T4** `inspect_index_graph_without_an_index_fails` — project created, never indexed →
  `MissingIndex`.
- **T5** `inspect_index_graph_rejects_a_stale_index` — write `{"version":"1"}` into
  `index/<INDEX_CONFIG_FILE>` as `index_version_gate_rejects_a_different_version` does →
  `IncompatibleIndex`. Needs no parquet files, since the gate runs before any table is touched.

CLI tests in `main.rs`'s new `mod tests` (see C3), all `Cli::try_parse_from`:

- **T6a** `index` and `go` reject `--dump-index-graph`.
- **T6b** `inspect --dump-index-graph f.dot --dump-ir` is a clap conflict.
- **T6c** `inspect --dump-index-graph f.dot` parses, and `inspect_artifact` on it errors with
  "requires a project name" — a runtime check, not a parse one, so it is asserted by calling
  `inspect_artifact` directly with a hand-built `InspectArgs`. No store needed: the name check comes
  first.

## 5. What is not in this change

Filtering the graph (spec §9.2), the flowy divergence (§9.1), the three pre-existing defects in
§9.4, and any deprecation stub (§9.3). The memory cost of loading `assign` at inspect time is
accepted as specified (§8.2); `content_cmp` allocates nothing, so the sort adds nothing to peak.

## 6. Checking the work

Per `CLAUDE.md`, every build and test run is captured to a file rather than read off the terminal:

```
LOG=/private/tmp/claude-501/-Users-dbueno-proj-ct-dump-to-inspect/<session>/scratchpad
cargo build -p ctadl-ascent            > $LOG/build.txt 2>&1
cargo test  -p ctadl-ascent            > $LOG/test.txt  2>&1
cargo clippy --workspace --all-targets > $LOG/clippy.txt 2>&1
```

Clippy matters here: the workspace denies `rust-2018-idioms`, and removing the `IndexOptions`
lifetime is exactly the kind of edit that leaves an elided-lifetime warning behind.
