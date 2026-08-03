# Rebase of `taintbench` onto `origin/main` - DO-NOT-MERGE

Rebased `taintbench` onto `origin/main` (`e27e1466`, "Add human output (#90)") and
re-ran the TaintBench suite (see `taintbench/README.md`).

## Result

Rebase succeeded. Four of the five commits replayed:

```
21566a48 Optimize some more things in the query
b5767a7b Format
0673fa43 Fix taintbench sink
3c25d4a8 Taintbench
e27e1466 Add human output (#90)   <- origin/main
```

The **"Clippy" commit dropped out as empty**. Its only surviving content was a
`.into()` removal in `ctadl-ascent/src/languages/dex/tests.rs`, and main had
already refactored those assertions to go through `local_name(&locals, var)`, so
resolving to main's side left the commit with nothing.

## Conflicts resolved

### `xtask/src/main.rs` — additive, both sides kept

Main added `mod sarif;`, we added `mod taintbench;`. Same story in the help text:
main added the `--models-dir` block, we added the `taintbench` task block. Both
kept in each case.

### `ctadl-ascent/src/languages/dex/tests.rs` — main's side

Four identical conflicts in the `const_wide*_assign` tests. Main replaced
`assert_eq!(*var.variable, Variable::Local(expected_reg))` with
`assert_eq!(local_name(&locals, var), expected_reg)`; our clippy tweak only
dropped an `.into()` from the older form. Took main's.

### `ctadl-ascent/src/query_engine/formatter.rs` — combined

Two conflicts, both genuine two-sided changes:

- `ProjectContext` — main restructured it for multi-import support (`SourceSpan`,
  `SpanKey`, `ImportSource`, `imports: &'a [ImportSource]`, `details_by_span`
  keyed by `SpanKey`). Ours changed `taint_results` to
  `&'a TaintAnalysisResults<'a>`. Kept main's struct and applied our lifetime.
- `results_by_path` — main changed the key from `u32` to `SpanKey`; ours changed
  the endpoints from `QueryEndpoint` to `Arc<QueryEndpoint>`. Combined:
  `(SpanKey, Vec<(Arc<QueryEndpoint>, Option<Arc<QueryEndpoint>>, Label)>)`.

## Semantic conflicts (auto-merged clean, did not compile)

Two places where git merged without complaint but the result was broken — each
side independently touched code the other's change invalidated.

### `ctadl-ascent/src/query_engine/mod.rs:597`

Main added a `CTADL_QUERY_SIZES` diagnostic that reads `engine.taint_edge.len()`.
Our commit **deleted the `taint_edge` relation** from the ascent block — the graph
is now projected from `taint_edge_directed` after the run, precisely so the engine
never materializes and indexes a second copy of the whole taint graph. Dropped the
`taint_edge=` field from the log line; `taint_edge_directed=` was already there and
carries the same information pre-orientation.

### `ctadl-ascent/src/query_engine/search.rs`

Main's new demand-driven search engine is a second producer of `QueryResult`, and
it built its `taint` rows with inline `QueryEndpoint` values — the exact
representation our commit changed to `Arc<QueryEndpoint>`. Rather than convert at
the boundary, applied the same optimization inside the search engine: endpoints are
`Arc`'d once up front (`all_endpoints`) and `source_sets`, `sink_nodes`, `taint`,
and `sink_tags` all hold `Arc<QueryEndpoint>`. `absorbing` still wants owned
values, so that one site derefs: `(**src).clone()`.

## API drift the compiler could not catch

`ctadl format` no longer exists — main merged it into `ctadl query`, which now
takes `-o` and `--sarif-profile` directly. The taintbench xtask shells out to
`ctadl`, so this compiled fine and failed at runtime with
`error: unrecognized subcommand 'format'` on all three apps.

Fixed in `xtask/src/taintbench.rs` by folding the flags into the single `query`
call, and updated the stale references in `taintbench/README.md` and the rule-ID
doc comment.

## Verification

- `cargo check --workspace --all-targets` — clean
- `cargo fmt --all` — applied
- `cargo clippy --workspace --all-targets` — clean (two `unnecessary_cast`
  warnings in `index_engine/locals_trie.rs` are pre-existing on main, untouched here)
- `cargo test --workspace` — 231 + all other suites pass, 0 failed

## Benchmark results

`cargo xtask taintbench` against the three APKs, run from the rebased tree:

| app | before | after | verdict |
| --- | --- | --- | --- |
| `beita_com_beita_contact` | 1/3 | 1/3 | PASS, unchanged |
| `cajino_baidu` | 11/12 | 10/12 | **FAIL — regression, finding #8 lost** |
| `hummingbad_android_samp` | 0/2 | 2/2 | PASS, **improvement** |

```
2 passed, 0 skipped, 1 failed of 3 app(s)
```

Both movements point at the same cause: **main switched the default query regime.**
`taint_analysis` now dispatches to `search::taint_search` (the demand-driven
multi-start realizable-path search) unless `CTADL_QUERY_DATALOG=1` is set. Every
`expected.json` baseline in the suite was recorded against the datalog closure
engine, so the suite is comparing two different analyses.

### Improvement: `hummingbad_android_samp` 0/2 → 2/2

Both ground-truth flows (`Cursor.getString` → `SQLiteDatabase.insert` and
`→ .update`) now connect. The old baseline comment explained the empty baseline as
flows crossing Intent IPC, JSON serialize/deserialize, and async network callbacks
that the analysis could not track end-to-end. The search regime connects them.

**`expected.json` updated to `[1, 2]`** — this is the improvement fold-in the
README prescribes.

### Regression: `cajino_baidu` finding #8

```
#8 pos [BaiduUtils:-1 java.io.File.listFiles -> BaiduUtils:479 java.io.FileWriter.write]
   source:HIT  sink:HIT  path:no  => -
```

Both endpoints are still recognized; the connecting path is gone. The reported
path list confirms it — `File.listFiles` still reaches `BaiduBCS.putObject` and
`File.delete`, but no longer `FileWriter.write`.

Consistent with how the search regime reports: it emits one breadth-first shortest
path per sink *vertex* reached, deduped by the level-agnostic vertex. A second
distinct flow arriving at a sink vertex some other path already reached is not
reported separately, so a genuine source→sink pair can drop out of the SARIF even
though taint does arrive.

**`cajino_baidu/expected.json` was left at `[1, 2, 3, 4, 6, 7, 8, 9, 10, 12, 15]`.**
Lowering it would bury a real capability loss behind a green check. Whether to
accept the trade (the search regime exists because the datalog closure blows up on
firmware-sized copy groups) is a call for you, not something to baseline away
silently. Options:

1. Accept it — drop `8` from the baseline and note the engine switch in the comment.
2. Fix the reporting — have the search emit a path per (source-set, sink vertex)
   pair rather than one per sink vertex, so co-arriving flows stay distinguishable.
3. Re-baseline the whole suite under `CTADL_QUERY_DATALOG=1` and treat the search
   regime as a separate axis.

### Attribution check (in flight)

A confirming run of `cajino_baidu` under `CTADL_QUERY_DATALOG=1` was still
executing when this was written — the datalog engine is much slower on this app,
which is what motivated the switch. If #8 returns under the datalog engine, the
loss is fully attributable to the regime change and not to any conflict resolution
above. Note that the `Arc` and `taint_edge` resolutions are representational only
and cannot change which paths are found.

## Files changed beyond the replayed commits

- `xtask/src/taintbench.rs` — `format` subcommand folded into `query`; doc comment
- `taintbench/README.md` — same rename
- `taintbench/apps/hummingbad_android_samp/expected.json` — baseline `[]` → `[1, 2]`

These are uncommitted working-tree changes.
