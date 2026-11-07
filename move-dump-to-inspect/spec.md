# Spec: move `--dump-index-graph` from `index` to `inspect` - DO-NOT-MERGE

Status: decisions made, ready for implementation
Source intent: `intent.md` — "Move the --dump-index-graph option to inspect instead of index."

## 1. Goal

Today you can only get the index (assign-like) graph as a side effect of building an index:

```
ctadl index app --dump-index-graph app.dot
```

If you did not pass the flag, you have to re-run the whole index — minutes to hours — to get the
graph. The graph is already on disk in the index, so this is wasted work.

After this change the graph is dumped from a finished index, on demand:

```
ctadl index app
ctadl inspect app --dump-index-graph app.dot
```

`--dump-index-graph` is removed from both `index` and `go`.

## 2. Background: what exists today

**The flag.** `IndexArgs.dump_index_graph` (`ctadl-ascent/src/main.rs:406`) is passed through
`IndexOptions.dump_index_graph` (`ctadl-ascent/src/cli/mod.rs:71`) to a call at the end of
`cli::index` (`ctadl-ascent/src/cli/mod.rs:372`):

```rust
if let Some(dot_path) = dump_index_graph {
    dump_index_graph_dot(&result.assign_like, &sites, dot_path)?;
}
```

`dump_index_graph_dot` (`ctadl-ascent/src/cli/mod.rs:794`) writes a legend comment, calls
`graphviz::render_index_graph` (`ctadl-ascent/src/graphviz.rs:285`), and logs a legend line. It
needs exactly two things:

1. `assign_like: &[(FunctionId, FlowVariable, Path, FlowVariable, Path)]`
2. an `IdMap` to turn `FunctionId` into a function name

**Both are already persisted.** Immediately after the dump, `cli::index` saves the same vector via
`IndexResult::try_save` → `schema::assign::try_save`, whose record type
(`ctadl-ascent/src/facts/schema.rs:64`) is the identical 5-tuple. The `IdMap` is saved as
`function_id.parquet` by `IndexSourceInfo::try_save`. `cli::query` already reloads both
(`ctadl-ascent/src/cli/mod.rs:515-521`), and `dump_taint_graph_dot` already reloads the `IdMap`
from the index directory to render its graph. So there is no new data to persist — this is a
read-side change. See §8.4 for why the two are genuinely equivalent.

**`inspect` today.** `InspectArgs` (`ctadl-ascent/src/main.rs:216`) takes an optional `name` plus
`--dump-ir` and `--function`. `inspect_artifact` (`ctadl-ascent/src/main.rs:1034`) either:

- dispatches on a store *file path* (`.parquet`, program/vmt bitcode, JNI registry), or
- resolves `name` with `ArtifactImport::load_by_name` and runs `cli::dump_ir` or `cli::inspect`, or
- with no name, runs `cli::list_store_contents`.

Note the mismatch: the help text says "Artifact name, project name, or store path", but the code
only ever loads an **import**. `inspect` cannot currently look at a project at all. This change
introduces the first project-level `inspect` operation, so name resolution has to be settled (§5.2).

## 3. Requirements

### Functional

- **F1.** `ctadl inspect <NAME> --dump-index-graph <FILE>` writes the index graph of the project
  `NAME` to `FILE` in Graphviz DOT format.
- **F2.** The output has the same content as what `ctadl index --dump-index-graph` writes today:
  same legend comment, same node set, same edge set, same labels.
- **F3.** Edges are written in sorted order, so two dumps of the same program's index are
  byte-identical and `diff` between two dumps is meaningful. See §8.1.
- **F4.** `--dump-index-graph` is removed from `ctadl index` **and** from `ctadl go`. Passing it to
  either is a clap error listing the valid flags.
- **F5.** `--dump-index-graph <FILE>` requires `<NAME>`; without one it is a usage error.
- **F6.** `--dump-index-graph` and `--dump-ir` are mutually exclusive (clap `conflicts_with`).
- **F7.** If `NAME` has no index, fail with the existing `MissingIndex` error, which names the
  project and tells the user to run `ctadl index <NAME>`.
- **F8.** If the index exists but was written by an incompatible build, fail with the existing
  `IncompatibleIndex` error via `AnalysisProject::check_index_config()`, same as `query` does.
- **F9.** `ctadl inspect` with no new flag keeps its current behaviour exactly (store listing,
  import summary, `--dump-ir`, file-path dispatch).

### Non-functional

- **N1.** Dumping must not require re-indexing, and must not write anything into the store.
- **N2.** Only the two tables needed are read (`assign.parquet`, `function_id.parquet`). Do not call
  `IndexResult::try_load`, which also pulls `summary` and `paths` for nothing.
- **N3.** No change to the index format version, to what `index` writes, or to any other command's
  output.

### Non-goals

- Filtering the graph (by function, by size). Explicitly deferred — see §9.2.
- Changing the DOT rendering, the node-id scheme, or the legend, beyond the edge sort in F3.
- Touching the `--dump-taint-graph` flag on `query`/`go`.
- Touching `ctadl-ascent/examples/flowy.rs` / `codegen::flowy::check`. Flowy keeps its own copy of
  the dump — see §9.1.

## 4. CLI, before and after

Before:

```
ctadl index NAME [PROGS]... --dump-index-graph <FILE>
ctadl go ARTIFACTS... --dump-index-graph <FILE>
ctadl inspect [NAME] [--dump-ir] [--function <SUBSTR>]
```

After:

```
ctadl index NAME [PROGS]...
ctadl go ARTIFACTS...
ctadl inspect [NAME] [--dump-ir] [--function <SUBSTR>] [--dump-index-graph <FILE>]
```

A `go` user who wants the graph runs `ctadl inspect <name> --dump-index-graph <file>` afterwards;
`go` has already built the index by then.

Proposed help text for the new flag:

```rust
/// Write the index (assign-like) graph of this project to a Graphviz DOT file.
///
/// Reads the finished index, so the project must have been indexed. An edge `A -> B`
/// is the assignment `B = A`. Requires a project name; conflicts with `--dump-ir`.
#[arg(long, value_name = "FILE", conflicts_with = "dump_ir")]
pub dump_index_graph: Option<PathBuf>,
```

## 5. Design

### 5.1 New public API in `cli`

Add to `ctadl-ascent/src/cli/mod.rs`, next to the other `inspect_*` functions:

```rust
/// Renders a finished project's index (assign-like) graph to a Graphviz DOT file.
///
/// Reads `assign.parquet` and `function_id.parquet` out of the project's index directory —
/// the same rows `ctadl index` used to render in memory — so no re-index is needed.
pub fn inspect_index_graph(project: &AnalysisProject, dot_path: &Path) -> Result<(), Error> {
    if !project.has_index() {
        return Err(Error::from(ctadl_import::Error::MissingIndex {
            project: project.name.clone(),
        }));
    }
    // Before touching a table: the parquet decoders panic on an encoding they cannot read.
    project.check_index_config()?;
    let index_path = project.index_path()?;
    let ids = facts::IdMap::try_load(&index_path)
        .err_context(|| format!("loading IdMap from index: {}", index_path.display()))?;
    let mut assign_like = facts::schema::assign::try_load(&index_path)
        .err_context(|| format!("loading assign table from index: {}", index_path.display()))?;
    // The row order of `assign` is not stable across indexes (see docs/debugging.md); sort so
    // two dumps of the same program produce the same file. Free here -- we own the Vec.
    assign_like.sort_unstable();
    dump_index_graph_dot(&assign_like, &ids, dot_path)
}
```

The sort lives here, in the one place that owns the vector, rather than in
`dump_index_graph_dot` or `render_index_graph` — both take a `&[_]` and would have to clone the
table to sort it, doubling peak memory on the largest table in the index. After this change
`inspect_index_graph` is `dump_index_graph_dot`'s only caller, so the invariant holds in practice;
say so in a comment on `dump_index_graph_dot` so a future second caller knows it is expected to sort.

The 5-tuple is `Ord` by construction (`render_index_graph` already puts a prefix of it in a
`BTreeSet`), so `sort_unstable` needs no key function.

### 5.2 Name resolution in `inspect`

`--dump-index-graph` needs an `AnalysisProject`. Imports and projects live in separate store
directories and routinely share a name (`ctadl index app` builds project `app` from import `app`),
so `inspect` must decide which one `NAME` means.

**Rule: the flag picks the namespace.** With `--dump-index-graph`, `NAME` is a project name and is
resolved with `AnalysisProject::try_load_name`. Without it, `inspect` behaves as today and resolves
an import. This keeps F9 exact and needs no new disambiguation flag.

Do **not** reuse `main.rs`'s `load_or_infer_project`: its ephemeral fallback exists so `query` can
model-check without an index, which is meaningless here. A name that is not a project should fail
with the project-load error.

### 5.3 Wiring in `main.rs`

1. Delete `IndexArgs.dump_index_graph` (`main.rs:406-408`) and its use at `main.rs:953`.
2. Delete `GoArgs.dump_index_graph` (`main.rs:511-513`) and its use at `main.rs:676`.
3. Remove the now-dead `dump_index_graph` entry from the two struct literals that build an
   `IndexArgs`: the `go` literal (`main.rs:676`) and the legacy pcode CLI literal (`main.rs:828`).
4. Add `dump_index_graph: Option<PathBuf>` to `InspectArgs`.
5. In `inspect_artifact`, before the store-file-path dispatch:

```rust
if let Some(dot_path) = &args.dump_index_graph {
    let Some(name) = &args.name else {
        anyhow::bail!("--dump-index-graph requires a project name");
    };
    let project = project::AnalysisProject::try_load_name(name)
        .with_context(|| format!("loading project: '{name}'"))?;
    return cli::inspect_index_graph(&project, dot_path).map_err(Into::into);
}
```

It goes **before** the file-path dispatch so `--dump-index-graph` is never shadowed by a project
name that happens to match an existing file.

### 5.4 Wiring in `cli/mod.rs`

1. Remove the `dump_index_graph` field from `IndexOptions` (`cli/mod.rs:71`), its `Default`
   (`cli/mod.rs:88`), and its binding in the destructuring `let` (`cli/mod.rs:114`).
2. Remove the `if let Some(dot_path) = dump_index_graph { … }` block at `cli/mod.rs:372`.
3. Add `inspect_index_graph` as in §5.1.

Removing the field drops the `'a` lifetime's only user in `IndexOptions<'a>`; check whether the
struct still needs the parameter and simplify to `IndexOptions` if not. That is a mechanical churn
across every `IndexOptions` construction site — grep before committing.

## 6. Errors

| Situation | Behaviour |
|---|---|
| `--dump-index-graph` with no name | `anyhow` error: "--dump-index-graph requires a project name" (mirrors the existing `--dump-ir` message) |
| Name is not a project | project-load error, contextualised `loading project: '<name>'` |
| Project exists, never indexed | `ctadl_import::Error::MissingIndex` (existing message points at `ctadl index <name>`) |
| Index written by an older build | `ctadl_import::Error::IncompatibleIndex` from `check_index_config` |
| DOT file cannot be created/written | existing `err_context` messages in `dump_index_graph_dot` |
| `--dump-index-graph` and `--dump-ir` together | clap conflict error |

## 7. Testing

Tests in `ctadl-ascent/tests/cli.rs` call the `cli::` API directly (not the binary) and must run in
milliseconds; follow that. Wrap store tests in `run_store_test` and use distinct import/project
names.

- **T1.** Round-trip: import a tiny `.c` fixture, `cli::index` it, then `cli::inspect_index_graph`
  into a temp path. Assert the file exists, starts with the legend comment, and contains at least
  one `->` edge.
- **T2.** Equivalence: render the graph from the in-memory `IndexResult` via
  `graphviz::render_index_graph` over a sorted copy of `assign_like`, and again through
  `inspect_index_graph`; assert the edge lines match byte for byte.
- **T3.** Determinism (this is what justifies F3): index the same fixture twice into two projects,
  dump both, assert the two files are byte-identical.
- **T4.** No index: `cli::inspect_index_graph` on a project that was created but never indexed
  returns `MissingIndex`.
- **T5.** Stale index: reuse the fixture pattern from
  `index_version_gate_rejects_a_different_version` to assert `IncompatibleIndex`.
- **T6.** Clap: `--dump-index-graph` is rejected on `index` and on `go`; `--dump-index-graph` with
  `--dump-ir` on `inspect` is rejected. (Parse-level; `Cli::try_parse_from`.)
- **T7.** Existing `graphviz.rs` unit tests are untouched and must still pass.

## 8. Behavioural notes

### 8.1 Edge order — resolved by sorting

`docs/debugging.md` states that the row order of `assign` written post-fixpoint is
**nondeterministic**: the table is stable as a set, unstable as a sequence.
`render_index_graph` sorts nodes (a `BTreeSet`) but emits edges in slice order, so without
intervention two dumps taken from two indexes of the same program would differ line by line while
describing the same graph.

F3 settles this: `inspect_index_graph` sorts the table before rendering (§5.1). The cost is one
`sort_unstable` over the assign table, in-place, on a vector we already own. The benefit is that
`diff` between two dumps means something, and T3 can assert it.

### 8.2 Memory — accepted

At index time `assign_like` was already resident, so the dump was free. At inspect time it must be
loaded from parquet, and on large targets this table is the biggest thing in the index, so
`ctadl inspect --dump-index-graph` on a real program has a real memory cost. N2 (loading only
`assign` and `function_id`) keeps it to the minimum, and the sort in F3 is in-place, so it adds
nothing to peak.

This is accepted as the price of the feature: the alternative is re-indexing, which costs strictly
more. There is no streaming path — `schema::assign::try_load` returns a `Vec` and
`render_index_graph` wants a slice — and building one is out of scope.

### 8.3 Graph size

An index graph on a real target has millions of nodes, and Graphviz will not usefully lay that out.
This is already true of the flag today, so the change does not make it worse, but moving the flag to
`inspect` does invite people to run it on indexes they would never have passed the flag for.
Filtering is deferred (§9.2).

### 8.4 Verified: the two data sources are equivalent

Worth stating explicitly because it is the load-bearing assumption of the whole change. The dump at
`cli/mod.rs:372` runs on `result.assign_like` immediately before `result.try_save` hands that exact
same `Vec` to `assign::try_save`, and the parquet record type is the same 5-tuple. Nothing is
pruned, filtered, or transformed on the way to disk. `IdMap` round-trips through
`function_id.parquet` by construction (`facts.rs:1324`/`1336`), and `docs/debugging.md` confirms
function ids are byte-stable across indexes. The one thing `IndexResult::try_load` does *not*
restore is `call_target_assign_like`, which the index graph does not use — and §5.1 does not go
through `IndexResult` anyway.

## 9. Remaining concerns

### 9.1 Flowy keeps its own copy of the dump

`codegen::flowy::check` / `check_with_config` (`ctadl-ascent/src/codegen/flowy.rs:303`) take their
own `dump_index_graph: Option<&Path>` and inline a copy of the rendering (minus the legend comment),
driven by `ctadl-ascent/examples/flowy.rs`. That path compiles a `.tnt` file in memory and never
writes a store index, so `inspect` cannot serve it. It stays as-is.

Consequence to be aware of, not to fix here: after this change there are two places that write an
index graph, and they produce different files — the flowy one has no legend header and its edges are
unsorted. That is already true today. If the divergence ever bites, the cheap fix is to make
`dump_index_graph_dot` `pub(crate)` and have flowy call it.

### 9.2 No filtering, deferred

`inspect --dump-ir` has `--function <SUBSTR>` to narrow its output; `--dump-index-graph` has no
equivalent, so on anything but a toy program the DOT file is too large for Graphviz (§8.3). Reusing
`--function` to keep only rows whose `FunctionId` resolves to a matching name would be a few lines.

Deliberately out of this change: the intent says "move", not "move and extend". Revisit once the
move has landed and there is real usage to aim a filter at.

### 9.3 Breaking change, no deprecation

`ctadl index --dump-index-graph` and `ctadl go --dump-index-graph` stop working, with no deprecation
period. The flag is a debugging aid with no in-tree callers (verified: no hits in `scripts/`,
`nightly/`, `xtask/`, `.gitlab-ci.yml`, `.github/`, `README.md` or `docs/`) and no documentation
pointing at it. If a downstream harness uses it, the clap error will name the available flags but
will not point at `inspect`. A one-release soft landing is possible — keep the flag
`#[arg(long, hide = true)]` on `index` and error with "use `ctadl inspect <name>
--dump-index-graph` instead" — but is not recommended and is not part of this spec.

### 9.4 Three pre-existing defects, written up separately

Each has its own file at the repo root; none is in scope here, and none is caused by this change,
but all three sit in the code this change touches:

- `issue-dot-node-id-collisions.md` — `node_id` maps punctuation to `_`, so distinct vertices can
  share a DOT id and get merged into one node.
- `issue-inspect-function-silently-ignored.md` — `ctadl inspect --function` without `--dump-ir`
  parses fine and is discarded without a word.
- `issue-inspect-name-help-text.md` — `inspect`'s name argument is documented as accepting a project
  name, which no code path supports.

## 10. Work breakdown

| # | Task | Est. |
|---|---|---|
| 1 | Add `cli::inspect_index_graph`, including the edge sort | S |
| 2 | Remove the flag from `IndexArgs`, `GoArgs`, `IndexOptions` and `cli::index`; fix all construction sites and the `IndexOptions<'a>` lifetime | S–M |
| 3 | Add the flag to `InspectArgs` and wire `inspect_artifact` | S |
| 4 | Tests T1–T7 | M |
| 5 | Docs: `docs/debugging.md` gains a short "dumping the index graph" note; amend the `inspect` name help text per `issue-inspect-name-help-text.md` | S |
