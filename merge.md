# Rebase of `taintbench` onto `origin/main` - DO-NOT-MERGE

Rebased `taintbench` onto `origin/main` and re-ran the TaintBench suite (see
`taintbench/README.md`). Newest rebase first; the earlier sections are kept as
the record of where each conflict came from.

# Rebase onto `0183c42c` (#122 - #128)

Previous base was `99bbed37` ("Move crate groups into subdirectories (#121)");
seven main commits came in:

```
0183c42c Andersen style aliasing in query (#128)          <- new base
a5f4f557 Fully hybrid call graph (#127)
2ba7e014 Delete CLAUDE md (#126)
51452046 Add `report` subcommand (#125)
44e46bdf Regression fix (#124)
cea3ea6b character encoding fix (#123)
1175dbd2 flake update fix for nightly (#122)
99bbed37 Move crate groups into subdirectories (#121)     <- old base
```

## Result

Twenty of the twenty-two branch commits replayed. Two were dropped (below), one
conflicted, and the runner (`xtask/src/taintbench.rs`) needed no changes.

**The suite no longer passes.** This is the first rebase that changes what ctadl
finds:

```
22 passed, 0 skipped, 16 failed of 38 app(s)
```

Detection drops from 92 of TaintBench's 191 ground-truth flows to 77 - 26 flows
lost, 11 gained. False positives drop from 8 to 3. Indexing the suite takes
about twice as long as before and peak memory is up on 26 of the 38 apps, while
querying is roughly eight times faster, so the suite's total wall-clock actually
falls from 471 s to 389 s.

All of that comes from the two new engine commits, and they split cleanly.
Measured four ways - neither commit, each alone, and both - in
[`taintbench-callgraph-rework.md`](taintbench-callgraph-rework.md):

| | index (all 38) | query (all 38) | worst app's peak | flows found |
| --- | ---: | ---: | ---: | ---: |
| `2ba7e014`, neither | 134 s | 243 s | 3.4 GB | 92/191 |
| `#127` only | 259 s | 297 s | 6.5 GB | 93/191 |
| `#128` only | 134 s | 30 s | 3.4 GB | 76/191 |
| both (this base) | 264 s | 31 s | 6.1 GB | 77/191 |

`#127` costs the time and memory and finds the same things; `#128` loses the
flows and buys the query time. Each holds with or without the other, so the
attribution does not rest on the order they landed in.

The committed `expected.json` baselines were **not** updated. Rebaselining would
make the check green and lose the finding; they stay as they are so the sixteen
failures keep being reported until someone decides the new behaviour is what we
want.

## Commits dropped

`8ee23ca5` "Fix index_engine comments" and `68419a08` "More cleanup in
index_engine rules" both add or edit `facts::counters`, the temporary
instrumentation for the hybrid-inlining investigation (see
`hybrid-inlining-plateau.md`). `#127` deletes the call-string lattice those
counters count - `CallString` and `SmallestCallString` no longer exist anywhere
in `ctadl-ascent` - and rewrites the `locals` / `context_locals` rules they
counted into keyed joins. There is nothing left for the counters to attach to,
so both commits resolve to main's side and drop out.

`#127` also brings its own instrumentation in the same place, logging the sizes
of the new propagation relations (`reach_vp`, `locals_key`, `edge_split`,
`ext_dst`, and friends) next to the existing scc-time summary.

## Conflict resolved

### `768c44bc` "Taintbench" - `xtask/src/main.rs`

`#125` added a `report-eval` subcommand to the same three places this commit
adds `taintbench`: the dispatch `match`, the argument parser, and the help text.
Both were kept, in that order. The module comment at the top of the file said
"Two tasks" and now lists three.

## Verification

- `cargo check --workspace --all-targets` - clean
- `cargo test --workspace` - 1011 passed, 0 failed
- The rebased branch's own code changes are intact: the `Arc<QueryEndpoint>`
  sharing, the post-hoc `taint_edge` projection, and the borrowed taint graph in
  `TaintAnalysisResults` all merged into `#128`'s rewritten `search.rs` without
  conflict.
- Suite run: 22 passed, 16 failed, recorded in
  [`taintbench-run-7e804754.log`](taintbench-run-7e804754.log).
- Before rebasing, the branch was run at its old head `c9280964` and reproduced
  `taintbench-run-8baae049.log` verdict-for-verdict, so the changes above are
  the new base's and not drift in the suite.

# Rebase onto `99bbed37` (#121)

Previous base was `5fcb2c89` ("Extract language frontends into per-language
crates, clean up IR (#119)"); one main commit came in:

```
99bbed37 Move crate groups into subdirectories (#121)  <- new base
5fcb2c89 Extract language frontends into per-language crates (#119)  <- old base
```

## Result

All twenty-one branch commits replayed with **no conflicts**, and the runner
(`xtask/src/taintbench.rs`) needed no changes.

```
38 passed, 0 skipped, 0 failed of 38 app(s)
```

Every per-finding verdict is byte-identical to `taintbench-run-8baae049.log` —
all 236 finding lines, same MATCH / `-` / `FALSE-POSITIVE` on all 38 apps:

```sh
diff <(grep -E '^    #[0-9]' taintbench-run-8baae049.log) \
     <(grep -E '^    #[0-9]' <new run>)     # empty
```

## Why there were no conflicts

`#121` is a pure relocation: the language frontends moved to
`frontends/ctadl-{c,dex,flowy,jvm,lua,pcode}` and the binary readers to
`readers/{dex,jvm,pcode}-reader`, with the only edits being the path strings
that follow them — the workspace `Cargo.toml` members, each moved crate's
`path = ` dependencies, `flake.nix`, and `xtask/src/{main,dex,regression}.rs`.

None of that overlaps this branch. The branch's code changes live in
`ctadl-ascent/src/` (untouched by `#121`, which only rewrote its `Cargo.toml`
dependency paths) and in files it introduced itself: `xtask/src/taintbench.rs`,
`nix/taintbench.nix`, and `taintbench/`. Its one edit to a file `#121` also
touched — `flake.nix`, where it adds the `taintbench` check — landed in a
different region than `#121`'s `readers/`-path line, so git merged both.

Confirmed mechanically: diffing the branch's own files pre- against
post-rebase yields exactly `#121`'s changes and nothing else.

```sh
git diff --stat <old-head> <new-head> -- ctadl-ascent xtask taintbench nix flake.nix merge.md
# ctadl-ascent/Cargo.toml, flake.nix, xtask/Cargo.toml,
# xtask/src/{dex,main,regression}.rs, xtask/tests/dex/README.md
```

## Verification

- `cargo check --workspace --all-targets` — clean, every crate builds at its new
  path
- `cargo clippy --workspace --all-targets` — the same two pre-existing warnings
  as the previous rebase (`items_after_test_module` in
  `readers/jvm-reader/src/flow.rs`, `wrong_self_convention` in the vendored
  `rustc_graphviz`); nothing new
- `cargo test --workspace` — 0 failed
- `nix build .#checks.aarch64-darwin.taintbench` — 38 passed, 0 skipped, 0
  failed; verdicts identical to the committed log

# Rebase onto `5fcb2c89` (#101 - #120)

Previous base was `0038c0b8` ("Indexing tweaks (#100)"); twelve main commits
came in:

```
5fcb2c89 Extract language frontends into per-language crates, clean up IR (#119)  <- new base
6ad1e2e1 Add -j flag to index (#120)
5858c731 Restore Update instruction in IR (#118)
53a0d6d1 Bugfixes to tree-sitter C parsing (#116)
c0509263 Move all expensive tests into nightly runner (#115)
6a58056a Java dex fixes (#114)
6ecbfb45 Environment Variables and nested brackets in initializers (#110)
92d6a996 Fix > 127 param issue (#108)
7dd6c901 Implement modes: ["skip-analysis"] (#107)
b2a1ea5b bump version to 0.1.2 (#105)
de09eec8 Update release workflow (#106)
e41fdc6a Tree-sitter C alpha support (#101)
0038c0b8 Indexing tweaks (#100)                                                   <- old base
```

## Result

All twenty branch commits replayed. Two conflicted; the runner
(`xtask/src/taintbench.rs`) needed no changes, and the suite reproduces every
committed baseline.

```
38 passed, 0 skipped, 0 failed of 38 app(s)
```

Every per-finding verdict is byte-identical to the pre-rebase run recorded in
`taintbench-run-8baae049.log` — same MATCH / `-` / `FALSE-POSITIVE` on all 38
apps, `remote_control_smack` included:

```sh
diff <(grep -E '^    #[0-9]' taintbench-run-8baae049.log) \
     <(grep -E '^    #[0-9]' <new run>)     # empty
```

`remote_control_smack` still indexes in well under a minute and still detects
`[1, 2, 5, 6, 9, 16]`, so the unwinder modelling survives the change of
mechanism described below.

## Conflicts resolved

### `8e2326ae` "Fix index_engine comments" — reapplied by hand

`#119`/`#120` restructured the index engine: the rule bodies moved into an
`ascent_source!`-style `include_source!(index_rules)` block shared by the serial
`ascent!` and the parallel `ascent_par!` program, so git could not align the old
ascent block with the new one and threw the whole region up as one conflict.

Took main's file and reapplied this commit's content onto main's rule text: the
`counters` import, the six trailing `let _d = counters::bump(..)` clauses on the
`locals` / `context_locals` forward-propagation and seeding rules, and the
`log::debug!("index counters: ..")` line beside `scc_times_summary()`. The rules
themselves are unchanged by main, only re-indented and relocated, so each clause
went back on the rule it came from. `facts.rs` (the `counters` module and the two
`join_mut` bumps) auto-merged.

Placing the counters last still holds under the new layout: they are trailing
clauses, so the first-two-clause adjacency the SIMPLE JOIN plan keys on is
untouched, and being shared source they now count both the serial and the
parallel program.

### `8baae049` "Use modeling to address problem" — main's side, wholesale

**Main's `7dd6c901` "Implement modes: [\"skip-analysis\"] (#107)" is the
upstreamed form of this very commit** (branch commit dated Aug 9, main's Aug 12).
Ten files conflicted, all of them the same feature written twice.

Resolved by taking main's side in every conflicted file and reverting
`ctadl-ascent/src/codegen/model_matches.rs` (which auto-merged to the branch's
version) to main's. What survives from the branch commit is only what main did
not take: `hybrid-inlining-plateau.md`, `taintbench/README.md`, and
`remote_control_smack`'s `app.json` (the `excluded` key removed) and
`expected.json` (baseline `[1, 2, 5, 6, 9, 16]`).

**The two implementations differ, and main's is now the one in the tree.**

| | branch's `8baae049` | main's `#107` (kept) |
| --- | --- | --- |
| mechanism | `facts.skip_analysis` input relation; one `!skip_analysis(infunc)` guard on the rule seeding `locals` from formals | codegen lowers the signature and drops the body, so the blocks never become facts |
| the body's own calls | still in the call graph | **gone from the call graph** |
| cost | one relation for the solver to test | none — the fact base is smaller |

Main's is the stronger suppression, and it is a superset for the case the branch
cares about: with no body facts there is no `callee_info` for hybrid inlining's
rule 1.1 to read, which is the multiplication the ARM unwinder models exist to
stop. The suite confirms it empirically — `remote_control_smack` keeps its
baseline and its runtime.

Two smaller consequences of taking main's side:

- The CLI line is main's wording: `"... {} function body(ies) not analyzed"`
  reading a local `skipped_bodies`, not the branch's `model_report.skipped`.
- `models/json.rs`'s `uses_field` constraint keeps main's
  `StatementKind::Update` arm — `#118` restored that instruction, and the branch
  commit predates it.

The three fixtures (`model_adds_to_summary.tnt`, `skip_analysis.tnt`,
`skip_analysis_hybrid.tnt`), `models/tests.rs`, and
`defaults/native-index.jsonl`'s unwinder entry are the same on both sides bar
comment wording, so nothing about the feature's coverage changed.

## Verification

- `cargo check --workspace --all-targets` — clean
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets` — two warnings, both pre-existing and
  in files this branch does not touch (`items_after_test_module` in
  `jvm-reader/src/flow.rs`, `wrong_self_convention` in the vendored
  `rustc_graphviz`)
- `cargo test --workspace` — 0 failed
- `nix eval .#checks.aarch64-darwin.taintbench.drvPath` — evaluates; all 38 APK
  fetches resolve now that no app carries `excluded`
- `xtask taintbench` over all 38 apps with the APKs supplied from the Nix store —
  38 passed, 0 skipped, 0 failed, verdicts identical to the committed log
- `CTADL_QUERY_DATALOG=1 xtask taintbench --filter cajino_baidu` — still fails on
  the same `#8` regression documented under the `0852b052` rebase below, so that
  gap (`saturating` is a search-engine-only feature) is unchanged by `#101`–`#120`.
  Measured only: the datalog query took about twelve minutes of CPU and 8.8 GB
  resident on this one app, against roughly fifteen seconds end-to-end under the
  default search regime. No pre-rebase datalog timing was recorded, so this is an
  absolute figure, not a comparison.

# Rebase onto `0852b052` (#93 - #96)

## Result

Rebase succeeded. All nine commits replayed, one conflict, no runner changes
needed:

```
db2e4324 Move taintbench to its own nix file
cb4409f8 Update merge.md
b034e7b1 Update merge note
2c30e315 updates
89c23602 update branch
70cb8c7a Optimize some more things in the query
25d2bf31 Format
6bd0d01b Fix taintbench sink
6ab8fa96 Taintbench
0852b052 updates to sarif formatting (#96)              <- origin/main
faabe32d Parallel indexing (#95)
228651c6 CI: version the ctadl binaries (#94)
de45c4b0 Implement hybrid locals data structure (#93)
```

(One further commit, "Clippy", dropped out as empty during the earlier rebase
onto `afe3f1f4`. Its only surviving content was a `.into()` removal in
`ctadl-ascent/src/languages/dex/tests.rs`, and main had already refactored those
assertions to go through `local_name(&locals, var)`, so resolving to main's side
left the commit with nothing.)

## Conflicts resolved — onto `0852b052` (#94, #95, #96)

One conflicted commit, one file, one hunk. `cargo check --workspace
--all-targets` was clean straight off the rebase.

### `ctadl-ascent/src/query_engine/formatter.rs` — combined

`format_sarif`'s signature. `#96` dropped the standalone `compact: bool`
parameter (compactness is now a property of `SarifProfile::Machine`); ours
changed `taint_results` to `&TaintAnalysisResults<'_>`. Kept both: main's
parameter list with our lifetime. The body had already auto-merged to main's
side, which no longer threads `compact` through `async_format_sarif`.

Nothing else in `#96` collided. It renamed the `C0005`/`C0006` rule IDs to
`C0005.tainted-data` / `C0006.almost-path-function` and added
`properties.project_name`; `#95`'s parallel indexing is confined to
`index_engine`, which this branch does not touch; `#94` is CI YAML only.

### The test runner needed no changes

`#96` is the one main commit that could have broken `xtask taintbench`, since
the suite is a SARIF consumer. It did not, and the reasons are worth recording
so the next rebase knows where to look:

- **Rule IDs.** The runner reads `C0001.tainted-path`, `C0003.taint-source`, and
  `C0004.taint-sink` (`xtask/src/taintbench.rs:37`). `#96` renamed only `C0005`
  and `C0006`, which the runner never mentions.
- **The dropped `--compact` flag.** The runner never passed it; it calls `ctadl
  query -m <model> -o <sarif> --sarif-profile agent`. Compact output is now what
  `SarifProfile::Machine` means, and the suite asks for `agent`, which still
  pretty-prints.
- **`sourceCallee` / `sinkCallee`.** The match itself hangs on these two
  `properties` of a `tainted-path` result. `#96` added `project_name` alongside
  them and removed nothing, so the parse is unchanged.

Re-running the suite confirms it: same verdicts, same matched IDs, same
baselines as the `de45c4b0` run below.

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

`nix build .#checks.aarch64-darwin.taintbench` against the three APKs, re-run
from the tree rebased onto `0852b052`. **All three apps meet their committed
baseline exactly, and every verdict is unchanged from the `afe3f1f4` and
`de45c4b0` runs** — `#92` through `#96` moved nothing.

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

Re-run on the tree rebased onto `0852b052`:

- `cargo check --workspace --all-targets` — clean, with no fixups needed after the
  rebase
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets` — clean of anything we introduced. Two
  warnings remain, both pre-existing and in files this branch does not touch: an
  `items_after_test_module` in `jvm-reader/src/flow.rs:1792` and a
  `wrong_self_convention` in the vendored `rustc_graphviz`.
- `cargo test --workspace` — 621 passed across 38 suites, 0 failed
- `nix build .#checks.aarch64-darwin.taintbench` — 3 passed, 0 skipped, 0 failed
- `CTADL_QUERY_DATALOG=1 xtask taintbench` — 2 passed, 1 failed, the same
  `cajino_baidu` `#8` regression as before (see above), so the datalog gap is
  unchanged by `#94`–`#96`

## Files changed beyond the replayed commits

Now committed, in `89c23602` ("update branch"), `2c30e315` ("updates"), and
`b034e7b1` ("Update merge note"):

- `xtask/src/taintbench.rs` — the never-valid `format` subcommand folded into
  `query`; doc comment
- `taintbench/README.md` — same rename
- `taintbench/apps/cajino_baidu/model.json` — `listFiles` source marked
  `saturating`, with the rationale in `description`
- `taintbench/apps/hummingbad_android_samp/expected.json` — baseline `[]` ->
  `[1, 2]`, comment corrected
- `merge.md` — this note

The working tree is clean against `origin/taintbench`.
