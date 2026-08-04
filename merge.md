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

## A pre-existing bug in the taintbench xtask

`xtask/src/taintbench.rs` shelled out to a `ctadl format` subcommand. **That
subcommand has never existed.** The top-level subcommand list is identical at the
merge base (`73109eab`) and at `origin/main` (`e27e1466`): `import`, `index`,
`query`, `go`, `init-model`, `inspect`, `legacy-pcode-cli`. `ctadl query` has taken
`-o` and `--sarif-profile` directly the whole time.

Every commit of the pre-rebase branch (`1a57772a`..`ac0dc06d`) carries this call, so
`cargo xtask taintbench` never completed a run on that branch — it failed at the
`format` step on every app. This is not API drift introduced by the rebase; it is a
bug that was always there and only became visible once the suite was run.

Fixed by folding the flags into the single `query` call, and updated the stale
references in `taintbench/README.md` and the rule-ID doc comment.

Consequence for the baselines: `expected.json` was not produced by an end-to-end run
of this harness. The values are still trustworthy — building the pre-rebase tip
(`ac0dc06d`) and running `ctadl query` on it directly reproduces exactly
`{1,2,3,4,6,7,8,9,10,12,15}` for `cajino_baidu`, matching the committed baseline.

## Benchmark results

`cargo xtask taintbench` against the three APKs, run from the rebased tree:

| app | pre-rebase | rebased | rebased + model fix |
| --- | --- | --- | --- |
| `beita_com_beita_contact` | 1/3 | 1/3 | 1/3 |
| `cajino_baidu` | 11/12 | 10/12 (#8 lost) | 11/12 |
| `hummingbad_android_samp` | 0/2 | 2/2 | 2/2 |

```
3 passed, 0 skipped, 0 failed of 3 app(s)
```

Both movements were investigated. **Neither is caused by the query-regime switch.**

### The regime switch is verdict-neutral

`taint_analysis` now dispatches to `search::taint_search` unless
`CTADL_QUERY_DATALOG=1` is set, so it is the obvious suspect. It is not the cause.
Running the full model on the rebased tree under both regimes gives identical
finding-level verdicts on all three apps:

| app | search regime | datalog regime |
| --- | --- | --- |
| `cajino_baidu` | 10 source/sink pairs, #8 absent | same 10 pairs, #8 absent |
| `hummingbad_android_samp` | both flows found | both flows found |
| `beita_com_beita_contact` | 1 pair | same pair |

The regimes differ in how many redundant paths they report per pair (the search
emits one breadth-first shortest path per sink vertex; the closure engine emits
many), but not in which pairs exist. The `Arc` and `taint_edge` conflict
resolutions above are representational and likewise change nothing.

### Root cause: `66a24fd6` "Canonicalize access path encoding (#85)"

Bisected over the 39 commits the rebase pulled in (`73109eab..e27e1466`) with a
reproducer that runs in about ten seconds: a model containing only the
`File.listFiles` source and the `FileWriter.write` sink, testing whether the SARIF
reports that pair.

```
good 73109eab (#46)   good c62394b5 (#68)   good 4d901137 (#79)
good 90f7c056 (#81)   good 08298913 (#82)   bad 66a24fd6 (#85)   bad e27e1466 (#90)
```

`#85`'s parent is `#82`, so the range closes on a single commit.

The dex frontend encodes an array access as the field `[]`
(`FieldPath::symbol("[]")`, `ctadl-ascent/src/languages/dex/mod.rs:803` for `AGet`,
`:826` for `APut`). Before `#85`, `Path::to_dot_string` escaped `.` but not `[`, and
the parser read a leading `[` as an offset, so `Symbol("[]")` did not survive the
index -> query round trip. The commit's own test corpus names it first: *"The three
the fact store was corrupting: dex/jvm, lua/C, and lua/C again"*, `vec![sym("[]")]`.

With the element field corrupted away, an array was indistinguishable from its
elements, so empty-path taint on an array flowed into every element read. That
over-tainting is what carried finding #8, whose flow is
`File[] files = ...listFiles(); ... files[i].toString()`. Measured over the same
three `listFiles` sources, the fix removes it: **3120 -> 1584 tainted instructions,
341 -> 185 tainted functions**. The sink side moves for the same reason —
`Argument(*)` on `FileWriter.write` fans out to 4 endpoints before and 8 after, now
correctly enumerating the `.\[]` and `.length` subtree.

`#85` is a correctness fix. What it exposed is a modelling gap.

### Fix: mark the `listFiles` source saturating

Post-`#85`, `File.listFiles`'s `Return` source seeds taint at the **empty path** on
the returned array, and `files[i]` is a load off `.\[]` — a strictly longer sibling
path that precise, path-matched propagation correctly declines to follow.

This is exactly what `a836dbef` "Add saturating sources (#70)" is for;
`docs/model-generators.md` gives C's `argv` as the motivating case (`argv[1]` read
at an offset path that is a sibling of the one the source is modeled at).
`File.listFiles()` returning `File[]` is the Java analogue.

`taintbench/apps/cajino_baidu/model.json`:

```json
{ "kind": "FileData", "port": "Return", "saturating": true }
```

`cajino_baidu` returns to `11/12`, meeting the committed baseline exactly, with no
new false positives — the three negatives `#11`/`#13`/`#14` remain
`MATCH(shadowed-by-positive)`. **`expected.json` is unchanged.**

### `hummingbad_android_samp` 0/2 -> 2/2

Both ground-truth flows (`Cursor.getString` -> `SQLiteDatabase.insert` and
`-> .update`) now connect, under both regimes. The old baseline comment explained
the empty value as flows crossing Intent IPC, JSON serialize/deserialize, and async
network callbacks that the analysis could not track end-to-end; something in the
same block of main commits connects them. Not bisected — it is an improvement, and
the README prescribes folding those in.

**`expected.json` updated to `[1, 2]`**, and its comment corrected: the earlier
version credited the demand-driven search regime, which the two-regime comparison
above rules out.

## Verification

- `cargo check --workspace --all-targets` — clean
- `cargo fmt --all` — applied
- `cargo clippy --workspace --all-targets` — clean (two `unnecessary_cast`
  warnings in `index_engine/locals_trie.rs` are pre-existing on main, untouched here)
- `cargo test --workspace` — 231 + all other suites pass, 0 failed
- `cargo xtask taintbench` — 3 passed, 0 skipped, 0 failed

## Files changed beyond the replayed commits

- `xtask/src/taintbench.rs` — the never-valid `format` subcommand folded into
  `query`; doc comment
- `taintbench/README.md` — same rename
- `taintbench/apps/cajino_baidu/model.json` — `listFiles` source marked
  `saturating`, with the rationale in `description`
- `taintbench/apps/hummingbad_android_samp/expected.json` — baseline `[]` ->
  `[1, 2]`, comment corrected

These are uncommitted working-tree changes.
