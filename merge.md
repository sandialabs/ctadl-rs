# Rebase of `taintbench` onto `origin/main` - DO-NOT-MERGE

Rebased `taintbench` onto `origin/main` (`de45c4b0`, "Implement hybrid locals
data structure (#93)") and re-ran the TaintBench suite (see
`taintbench/README.md`).

## Result

Rebase succeeded. Four of the five original commits replayed, plus three commits
carrying this note and the baseline/model edits:

```
0d9a67a8 Update merge note
310cf417 updates
55cbf658 update branch
6c9c2de6 Optimize some more things in the query
1ed52727 Format
4255b8ce Fix taintbench sink
68e28a3b Taintbench
de45c4b0 Implement hybrid locals data structure (#93)   <- origin/main
c8b61936 GitHub release ci (#92)
afe3f1f4 Misc fixes (#91)
```

The **"Clippy" commit dropped out as empty**. Its only surviving content was a
`.into()` removal in `ctadl-ascent/src/languages/dex/tests.rs`, and main had
already refactored those assertions to go through `local_name(&locals, var)`, so
resolving to main's side left the commit with nothing.

## Conflicts resolved — onto `de45c4b0` (#92, #93)

One conflicted commit, one file, and no semantic conflicts: `cargo check
--workspace --all-targets` was clean straight off the rebase.

### `flake.nix` — two conflicts, one additive, one main's side

- `#92` added the `release = import ./nix/release.nix { ... }` binding right where
  our commit added the `taintbenchAppsDir` / `taintbenchApps` block. Purely
  additive on both sides; both kept.
- `jvm-reader-tests` — `#92` added `cargoBuildOptions = ... "--package"
  "jvm-reader"` and put the same `--package` filter on `cargoTestOptions`; our
  side only reflowed the base one-liner across several lines. Main's is the
  semantic change and matches the single-line style of the `dex-reader-tests`
  block above it. Took main's.

`#93` replaced `index_engine/locals_trie.rs` wholesale, and none of our commits
touch that file, so it merged silently and correctly.

## Conflicts resolved — earlier steps (onto `afe3f1f4`)

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

Places where git merged without complaint but the result was broken — each side
independently touched code the other's change invalidated.

### `ctadl-ascent/src/query_engine/mod.rs:593`

Main added a `CTADL_QUERY_SIZES` diagnostic that reads `engine.taint_edge.len()`.
Our commit **deleted the `taint_edge` relation** from the ascent block — the graph
is now projected from `taint_edge_directed` after the run, precisely so the engine
never materializes and indexes a second copy of the whole taint graph. Dropped the
`taint_edge=` field from the log line; `taint_edge_directed=` was already there and
carries the same information pre-orientation.

### `ctadl-ascent/src/query_engine/search.rs`

Main's demand-driven search engine is a second producer of `QueryResult`, and it
built its `taint` rows with inline `QueryEndpoint` values — the exact
representation our commit changed to `Arc<QueryEndpoint>`. Rather than convert at
the boundary, applied the same optimization inside the search engine: endpoints are
`Arc`'d once up front (`all_endpoints`) and `source_sets`, `sink_nodes`, `taint`,
and `sink_tags` all hold `Arc<QueryEndpoint>`. `absorbing` still wants owned
values, so that one site derefs: `(**src).clone()`.

### `#91` added three more consumers needing the same treatment

The rebase onto `afe3f1f4` pulled in `#91`'s `model_check` CLI and its
`codegen::flowy::query_check` path, both of which construct the formatter's
taint-results view. They called `TaintAnalysisResults::from_query_result`, which
our commit replaced with a borrowing `::new(&facts.taint_edge, ..)` so the taint
graph — by far the largest query artifact — is lent out of `FormatFacts` instead
of cloned a second time. That reverses the construction order: build the facts
first, then the view.

- `ctadl-ascent/src/cli/mod.rs:587` — facts built first, moving the query result's
  pieces in; view borrows from `facts.taint_edge`.
- `ctadl-ascent/src/codegen/flowy.rs:462` — same, but `query_result` is only
  borrowed here, so the facts take clones and the view still borrows from them.
  Also `TaintAnalysisResults<'_>` in `check_human_profile_paths`'s signature.
- `ctadl-ascent/src/codegen/tests.rs:123` — `r.4` is now `Arc<QueryEndpoint>`, so
  `(*r.4).clone()` where the old code had `r.4.clone()`.

## A pre-existing bug in the taintbench xtask

`xtask/src/taintbench.rs` shelled out to a `ctadl format` subcommand. **That
subcommand has never existed.** The top-level subcommand list is identical at the
merge base (`73109eab`) and at `origin/main`: `import`, `index`, `query`, `go`,
`init-model`, `inspect`, `legacy-pcode-cli`. `ctadl query` has taken `-o` and
`--sarif-profile` directly the whole time.

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

`cargo xtask taintbench` against the three APKs, re-run from the tree rebased onto
`de45c4b0`. **All three apps meet their committed baseline exactly, and every
verdict is unchanged from the `afe3f1f4` run** — `#92` and `#93` moved nothing.

```
3 passed, 0 skipped, 0 failed of 3 app(s)
```

| app | baseline | detected | matched IDs |
| --- | --- | --- | --- |
| `beita_com_beita_contact` | 1/3 | 1/3 | `[3]` |
| `cajino_baidu` | 11/12 | 11/12 | `[1,2,3,4,6,7,8,9,10,12,15]` |
| `hummingbad_android_samp` | 2/2 | 2/2 | `[1,2]` |

The three `cajino_baidu` negatives `#11`/`#13`/`#14` remain
`MATCH(shadowed-by-positive)`; no false positives anywhere. `beita`'s two misses
(`#1`, `#2`) and `cajino`'s `#5` have both endpoints recognized but no connecting
path, as their baseline comments record.

### The two movements that got us here

Both were investigated when the suite first ran end-to-end, and **neither was
caused by the query-regime switch**:

| app | pre-rebase | first rebased run | with model fix |
| --- | --- | --- | --- |
| `beita_com_beita_contact` | 1/3 | 1/3 | 1/3 |
| `cajino_baidu` | 11/12 | 10/12 (#8 lost) | 11/12 |
| `hummingbad_android_samp` | 0/2 | 2/2 | 2/2 |

#### Root cause of the `cajino_baidu` dip: `66a24fd6` "Canonicalize access path encoding (#85)"

Bisected over the commits the rebase pulled in with a reproducer that runs in about
ten seconds: a model containing only the `File.listFiles` source and the
`FileWriter.write` sink, testing whether the SARIF reports that pair.

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

#### Fix: mark the `listFiles` source saturating

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
new false positives. **`expected.json` is unchanged.**

#### `hummingbad_android_samp` 0/2 -> 2/2

Both ground-truth flows (`Cursor.getString` -> `SQLiteDatabase.insert` and
`-> .update`) now connect. The old baseline comment explained the empty value as
flows crossing Intent IPC, JSON serialize/deserialize, and async network callbacks
that the analysis could not track end-to-end; something in the block of main commits
the rebase pulled in connects them. Not bisected — it is an improvement, and the
README prescribes folding those in. **`expected.json` updated to `[1, 2]`.**

## The datalog regime no longer reproduces the baseline

`taint_analysis` dispatches to `search::taint_search` unless `CTADL_QUERY_DATALOG=1`
is set. Under the fallback datalog engine the suite **fails**, identically on the
`de45c4b0` rebase as on `afe3f1f4`:

```
2 passed, 0 skipped, 1 failed of 3 app(s)
```

| app | search regime (default) | datalog regime |
| --- | --- | --- |
| `beita_com_beita_contact` | 1/3 `[3]` | same |
| `cajino_baidu` | 11/12 | **10/12 — #8 regresses to `path:no`** |
| `hummingbad_android_samp` | 2/2 | same |

**Cause: `saturating` is a search-engine-only feature.** The datalog engine's
source-seeding rule destructures the endpoint and explicitly discards the flag —
`ctadl-ascent/src/query_engine/mod.rs:425`:

```rust
let QueryEndpoint { infunc, vertex, label, direction, call_site: _, saturating: _ } = s,
```

so every source is seeded at the plain level. The search engine instead maps it to
a taint level (`search.rs:461`):

```rust
let level = if ep.saturating { TaintLevel::Saturating } else { TaintLevel::Plain };
```

That is precisely the difference finding #8 depends on: the `saturating` marking on
`File.listFiles` is what reconnects the `.\[]` element read, and only the search
engine acts on it. The two regression tests for the behavior
(`saturating_source_reaches_offset_read`, `saturating_source_reaches_extended_sink`)
live in `search.rs` and exercise only that engine, so nothing caught the gap.

This supersedes an earlier claim in this note that the regime switch was
verdict-neutral. That measurement predated the `saturating` model fix; the two
regimes agreed only while the flag was unused. The `Arc` and `taint_edge` conflict
resolutions above remain representational and change nothing under either regime.

Not a merge blocker — the datalog engine is an opt-in fallback and the default path
meets every baseline — but `saturating` is silently ignored there, which is worth
either implementing or rejecting at model-load time under that flag.

## Verification

Re-run on the tree rebased onto `de45c4b0`:

- `cargo check --workspace --all-targets` — clean, with no fixups needed after the
  rebase
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets` — clean of anything we introduced. Two
  warnings remain, both pre-existing and in files this branch does not touch: an
  `items_after_test_module` in `jvm-reader/src/flow.rs:1792` and a
  `wrong_self_convention` in the vendored `rustc_graphviz`. (The two
  `unnecessary_cast` warnings this note used to list are gone: `#93` replaced
  `index_engine/locals_trie.rs` wholesale.)
- `cargo test --workspace` — 614 passed across 38 suites, 0 failed
- `cargo xtask taintbench` — 3 passed, 0 skipped, 0 failed
- `CTADL_QUERY_DATALOG=1 cargo xtask taintbench` — 2 passed, 1 failed, the same
  `cajino_baidu` `#8` regression as before (see above)

## Files changed beyond the replayed commits

Now committed, in `55cbf658` ("update branch"), `310cf417` ("updates"), and
`0d9a67a8` ("Update merge note"):

- `xtask/src/taintbench.rs` — the never-valid `format` subcommand folded into
  `query`; doc comment
- `taintbench/README.md` — same rename
- `taintbench/apps/cajino_baidu/model.json` — `listFiles` source marked
  `saturating`, with the rationale in `description`
- `taintbench/apps/hummingbad_android_samp/expected.json` — baseline `[]` ->
  `[1, 2]`, comment corrected
- `merge.md` — this note

The working tree is clean against `origin/taintbench`.
