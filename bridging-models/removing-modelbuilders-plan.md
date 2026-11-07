# Removing `ModelBuilders`: one native match structure - DO-NOT-MERGE

BLUF: Delete the columnar model-encoding layer entirely. `ModelGeneratorIngest` emits native
`PropagationMatch` and a new `EndpointMatch` straight into `ProgramModelMatches`, which grows an
`endpoints` field and becomes the single thing a models file matches into, whichever phase consumes
it. `ModelBuilders`, `SummaryBuilder`, `SummaryBatch`, `EndpointBuilder`, `EndpointBatch`,
`ModelsBatch`, `AccessPathBuilder`, `AccessPathFieldBuilder`, `AccessPathBatch` and
`codegen::models` all go — roughly 1000 of `models/mod.rs`'s 1355 lines (everything from
`ModelsBatch` down, minus the surviving `FormalIndexTypeTag`), plus `codegen/models.rs`.

This is option 1 of the two homes considered for endpoints (one struct with honest docs, vs. a
sibling `EndpointMatches` returned alongside). §7 records why.

Two live bugs are deleted by construction rather than fixed, because after this change there are no
access-path ids left to collide: `bridging-models-summary.md` §5.3 (`SummaryBatch::union_with`, still
exposed in `codegen/flowy.rs:342`) and its unreported twin on the endpoint side
(`EndpointBatch::union_with`, `models/mod.rs:1070`, which `cli::query` exercises on every run that
unions more than one (import × model file) batch — see §2).

## 1. Why the layer is removable

- **Nothing persists it.** No `try_save`/parquet path touches `ModelsBatch`
  (`load_and_map_summaries` reads parquet `facts.summary`, not the model encoding). The
  `// TODO load summary parquet models` at `models/mod.rs:89` is about a future *decoder*, which is
  unaffected — and becomes easier, since it would build `PropagationMatch`es directly instead of
  re-deriving them from Arrow.
- **Nothing outside the crate names these types.** Every use of `ModelsBatch` / `EndpointBatch` /
  `ModelBuilders` is in `ctadl-ascent/src` or `ctadl-ascent/tests`. No public API break beyond that
  crate's own test files.
- **The summary half is already a round trip to nowhere.** `json.rs:1582` appends into
  `builder.summary`; `SummaryBuilder::finish` encodes three Arrow batches; `matches.rs:157
  extend_from_summaries` immediately decodes them back into `Vec<PropagationMatch>`, which is what
  phase 2 reads. In production `iter_summaries` has exactly one caller (that decode; the other two
  are test code) and `SummaryBatch::dedup` has **zero**.
- **The endpoint half has one reader doing one linear pass.** `query_engine::endpoints.rs:43-118`
  calls `build_ap_map()` once and walks `iter_endpoints()` once. `EndpointRow` (`models/mod.rs:1218`)
  is already the native row type; making it own its data is the whole change.
- **Interning makes the native form smaller, not larger.** `StringBuilder` stores `function`,
  `label` and `in_function` per row; `facts::Str` is a `Copy` `u64` into a process-global table
  (`facts.rs:57-75`), and `facts::Path` is a `Copy` `tailshare::Seq`. So the row count that worried
  the columnar design argues *for* this change. §6 measures it — against a baseline — rather than
  asserting it.

## 2. The endpoint-side collision, stated once

`AccessPathBuilder::append` (`models/mod.rs:734-745`) numbers paths from 0 in every builder, and
`AccessPathBatch::union_with` (`models/mod.rs:1253`) concatenates rows without remapping. So a
unioned batch has duplicate ids, and `build_ap_map`'s `HashMap<u64, facts::Path>` keeps whichever
row it saw last.

`cli::query` unions one batch per **(import × model file) pair** (`cli/mod.rs:339-343`) and then
resolves every endpoint's `path_id` through the merged map (`endpoints.rs:56,211`). There are two
distinct triggers:

- **Two model files** whose ports carry differently-shaped trailing paths: the first file's
  endpoints silently resolve through the second file's table.
- **One model file matched against two imports.** Each import produces its own match set —
  different functions exist in each program — so the same file yields different append sequences
  and the same ids bind to different path tables. A single file whose generators match pathful
  ports in both a Java import and a native import (the bridging use case this branch exists for)
  collides with itself.

Either way, a source or sink is quietly widened, narrowed, or moved. The only benign case is one
file × one import (a single batch, nothing unioned), which is the common configuration to date —
that, not any structural safety, is why this has not been observed.

This plan does not repair the remapping. It removes the ids: `EndpointMatch` carries a
`facts::Path`, so accumulation is `Vec::extend` and there is nothing to renumber. §6 pins both
triggers with tests that fail before the change and pass after.

## 3. The target shape

### 3.1 `EndpointMatch`

New, in `models/matches.rs` beside `PropagationMatch`. Field-for-field `EndpointRow` with borrows
interned — every field must survive, since each one is load-bearing at query time and a dropped one
changes taint results silently. No field is *added* either; in particular no model-file provenance,
which `formatter.rs:295` wants for CTADL0005 — that is a recorded decision, not an oversight (§7).

```rust
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EndpointMatch {
    pub function: facts::Str,            // callee, for a callsite-scoped endpoint
    pub selector_ty: FormalIndexTypeTag,
    pub index: Option<i16>,
    pub path: facts::Path,               // was `path_id: u64` + the ap tables
    pub label: facts::Str,               // `Label(Str)` at query time, so this is free
    pub direction: TaintDirection,
    pub wildcard: bool,                  // sink-only
    pub saturating: bool,                // source-only
    pub in_function: Option<facts::Str>, // callsite-scoped caller filter; None = any caller
    pub callsite_scoped: bool,
    pub local_index: Option<u32>,        // `Variable(name)`, resolved per function in stage 1
}
```

`local_index` deserves the loudest comment: it is resolved against the matched function's `locals`
during matching (`json.rs:646-663`) and **cannot** be recovered later — stage 2 sees only the
post-`eliminate_dead_temps`/`coalesce_copies` graph, where the name may no longer exist — which is
why the row type exists at all rather than query re-deriving from names.

### 3.2 `ProgramModelMatches` gains `endpoints`

```rust
pub struct ProgramModelMatches {
    pub propagations: Vec<PropagationMatch>,  // index-time: -> facts.summary
    pub endpoints: Vec<EndpointMatch>,        // query-time: -> QueryEndpoint
    pub access_paths: BTreeSet<facts::Path>,  // index-time: -> facts.paths
    pub bridges: BridgeMatches,               // index-time: -> call/actual_param/assign/formal_param
}
```

`Vec`, not a set: union today concatenates duplicates, and `CTADL0100`'s declared-vs-matched
comparison counts post-fan-out endpoints (`cli/mod.rs:391-399`). Deduping is a separate, visible
change and is not part of this one.

**Accumulation order must be deterministic, nothing more.** It is not a fidelity requirement: the
formatter sorts endpoints into `BTreeSet`s before writing anything (`formatter.rs:1303-1304`), so
insertion order does not reach the SARIF. The natural loop order (import outer, model file inner,
visit order within a file) is deterministic and is what the accumulation gets for free; the only
rule is to never route accumulation through unordered iteration (`HashMap` drains and the like).
§6's SARIF diff confirms output is unchanged either way.

### 3.3 The loader accumulates instead of returning a batch

```rust
pub struct ModelLoadReport {
    pub endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats>,
    pub index_time_models: IndexTimeModelCounts,
}

pub fn try_load_models<P: AsRef<Path>>(
    index: &ProgramMatchIndex<'_>,
    path: P,
    out: &mut ProgramModelMatches,
) -> Result<ModelLoadReport, Error>;
```

Same for `try_load_default_models`, `try_load_jsonl_models`, `try_load_json_models`,
`try_load_json5_models`, `try_load_models_from_values`. `ModelsBatch` and its `union_with` are
deleted; `endpoint_stats` and `index_time_models` were never in `ModelBuilders` anyway (they live on
`ModelGeneratorIngest` and are stapled on at `models/mod.rs:250-256`), and the report is their new
home. `cli::query`'s deliberate re-key-by-file *before* merging (`cli/mod.rs:327-338`) stays exactly
as it is, reading from the report; so does `cli::index`'s "did this file declare endpoints" check
(`cli/mod.rs:124`), which reads `report.endpoint_stats.is_empty()`. The report is *not* keyed by
file: `try_load_jsonl_models` and `try_load_models_from_values` take readers and value streams that
have no path, so file identity belongs to the caller — which is exactly what the re-key already
implements.

**Error contract, to be stated in the doc comment:** on `Err`, `out` may hold rows appended before
the error. JSON model errors are collected across a batch and returned after rows have already been
emitted (`json.rs:750-760`), so this is the existing partial-append semantic made visible in the
caller's accumulator. Every production caller propagates with `?` and aborts, so nothing observable
changes; `tests/json_error_handling.rs` already pins the partial state (rows readable alongside
collected errors, `:171`, `:333`) and keeps doing so against the native rows.

Rejected alternative: keep the loader pure, return an owned `ProgramModelMatches` per load, and add
`merge`. It is a smaller call-site diff, but it allocates per (file × import), and `merge` would have
to special-case a `bridges` field that is always empty on that path. The out-param also matches
`observe_import(index, specs, &mut matches)`, which is already the shape of the other matcher entry
point.

## 4. Sequencing

Two library steps, each compiling and passing the suite on its own, then tests and docs. The order
is chosen so the awkward transient state (two sinks threaded through the visitor at once) never
exists.

### Step 0 — the red tests (`tests/models_loading.rs`)

Two tests, one per §2 trigger:

1. **Two files × one program**: both files with pathful source/sink ports of *different* shapes
   (e.g. file A a source at `Argument(0).headers`, file B a sink at `Argument(1)`), loaded and
   unioned the way `cli::query` does, asserting each endpoint resolves to its own declared path.
2. **One file × two programs**: a single file whose generators match different functions (with
   differently-shaped pathful ports) in two different `ProgramMatchIndex`es, unioned across both,
   with the same assertion.

Both fail today for the documented reason. They land **`#[ignore]`d**, each with a comment naming
this section, so `cargo test --workspace` stays green between steps — §6's per-step gate and a
committed red test cannot otherwise coexist. Step 2 removes the `#[ignore]`s.

Honestly stated: the loading *harness* of these tests necessarily uses the API this plan deletes
(`ModelsBatch::union_with` + `build_query_endpoints(&batch.endpoint, …)`), so step 2 rewrites that
harness in the same commit that turns them green. What survives verbatim — and what the red/green
evidence rests on — is the model-file fixtures and the final assertions on which `facts::Path` each
resolved endpoint carries.

### Step 1 — propagations go native

`ModelBuilders` keeps its shape but its first field becomes native:

```rust
pub struct ModelBuilders {
    pub propagations: Vec<PropagationMatch>,   // was SummaryBuilder
    pub endpoint: EndpointBuilder,
    pub access_paths: BTreeSet<facts::Path>,
}
```

- `json.rs:1580-1587`: push a `PropagationMatch { function: facts::Str::from(&func), dst, src }`
  instead of `builder.summary.append(...)`. `parse_port` already yields `(tag, index, ap)`; the ap
  becomes `facts::Path::from_accesses(...)` at the push, which is what `build_ap_map` was
  reconstructing.
- **`ModelsBatch`'s transient step-1 shape, spelled out**: its `summary: SummaryBatch` field
  becomes `propagations: Vec<PropagationMatch>`, and `ModelsBatch::union_with` switches that field
  from `concat_batches` to `Vec::extend` (the endpoint half is untouched until step 2). This is the
  one struct both steps depend on, so it is stated rather than left to the compiler to force.
- Delete `SummaryBuilder`, `SummaryBatch` (with `union_with`, `dedup`, `iter_summaries`,
  `num_rows`), `ProgramModelMatches::extend_from_summaries`, and `FormalIndexBuilder`'s summary use.
- Delete `codegen/models.rs` entirely. Its only caller is `codegen/flowy.rs:339-354`, which now
  collects one `ProgramModelMatches` across its model files and calls
  `codegen::model_matches::codegen_model_matches(&matches, &[], ...)?` directly.
- `cli/mod.rs:111-117`: `record` becomes `model_matches.propagations.extend(...)` plus the existing
  `extend_access_paths`.
- `models/mod.rs:252`'s trace line counts `builder.propagations.len()`.

Two deliberate behavior changes on the flowy path, both improvements, both to be called out in the
commit message:

1. flowy stops unioning summary batches, so §5.3's collision is gone there (that was its last
   exposure).
2. flowy models now get `access_paths` honored: today `flowy.rs:339-343` takes only
   `batch.summary` and drops `batch.access_paths` on the floor.

Explicitly *not* a behavior change: `AnyArgument` expansion. `codegen_summary` is already a thin
adapter over phase 2 — `codegen/models.rs:21-32` builds a `ProgramModelMatches` and calls
`codegen_model_matches` — so the flowy path runs the same expansion before and after this step.
`tests/flowy_tests.rs` and `tests/port_semantics.rs` are the guard for both real changes.

### Step 2 — endpoints go native, `ModelBuilders` disappears

- Add `EndpointMatch` (§3.1) and `ProgramModelMatches::endpoints` (§3.2).
- `json.rs:665-676` and `:706-717`: push `EndpointMatch` instead of `builder.endpoint.append(...)`.
  The two call sites keep their surrounding logic (the `Local`-port `continue`, the
  `UnmatchedReason` bookkeeping, the callsite-scoped caller cross-product) untouched.
- `ModelGeneratorIngest`'s sink field becomes `&'b mut ProgramModelMatches`; `ModelBuilders` and
  `ModelsBatch` are deleted along with `EndpointBuilder`, `EndpointBatch`, `AccessPathBuilder`,
  `AccessPathFieldBuilder`, `AccessPathBatch` and `build_ap_map`. Keep `FormalIndexTypeTag`
  (`models/mod.rs:632`); delete its `u8` conversions along with `FormalIndexBuilder` — their only
  non-builder use is the `iter_endpoints`/`iter_summaries` decode, which dies here too, and clippy
  (`--all-targets`, a §6 gate) would flag them as dead. `EndpointRow` collapses into
  `EndpointMatch`.
- **`observe_import` (`matches.rs:193-231`) must be restructured, not just retyped.** It currently
  fabricates a throwaway `ModelBuilders` purely to satisfy the constructor; with
  `ProgramModelMatches` as the sink it would borrow `matches` mutably twice. Fix: accumulate
  `(spec_index, from_names, to_names)` into a local `Vec` while the ingest holds the borrow, call
  `take_errors()`, drop the ingest, then fold the names into `matches.bridges`. The temporary
  `Vec<String>` per side already exists — `match_where` returns one.
- `query_engine/endpoints.rs`: drop `let ap_map = …` (`:56`), destructure `&EndpointMatch` at
  `:105-118`, use `ep.path` at `:211`, and `Label(ep.label)` at `:222`.
- `cli/mod.rs:307-346`: the `Option<ModelsBatch>` + `union_with` becomes one
  `ProgramModelMatches` accumulated across the loop; `cli/mod.rs:379-389` passes
  `&matches.endpoints`. **Keep the stage-2 gate**: today `build_query_endpoints` runs only under
  `if let Some(ref batch) = models_batch`, and it does real work before touching an endpoint —
  `compute_copy_alias` union-finds the whole `assign_like` relation (`endpoints.rs:72-101`). With
  the `Option` gone, guard on `matches.endpoints.is_empty()` so a model-less or flowy-only query
  does not start paying for it.
- Remove the `#[ignore]`s from step 0's tests and port their harness to the new API; they go green
  here.
- The `arrow` imports leave `models/mod.rs`. The crate still uses Arrow for fact persistence, so
  `Cargo.toml` does not change.

### Step 3 — tests

- `src/models/tests.rs` (185 lines, all `EndpointBuilder`): rewrite as assertions on
  `Vec<EndpointMatch>`. The schema-field-order assertions have no successor and go; the
  round-trip-a-`[8]`-offset-segment assertion becomes trivially true and is kept as a
  `parse_segment`-level test where it belongs (`AccessPathFieldBuilder`'s escaped-segment encoding,
  `models/mod.rs:785-791`, is what disappears — the guarantee it protected now needs no protecting,
  and that reasoning goes in the deletion commit).
- `tests/json_error_handling.rs`: ~17 mechanical `ModelBuilders::new()` → `ProgramModelMatches::default()`
  swaps, plus two row-reading sites (`:171`, `:333`). These also pin §3.3's error contract: rows
  emitted before a collected error stay readable in the accumulator.
- `tests/models_loading.rs:337-344`, `tests/default_models.rs:150,205,250`: assert on native rows
  instead of `num_rows()` / `build_ap_map()`. These tests were reaching through the encoding to
  check what the matcher produced; they get shorter.
- `src/languages/tree_sitter/tests.rs:1063-1230` (the `Variable(name)` / `local_index` cases) and
  `src/languages/lua/mod.rs:2812-2836` (`a_model_can_name_a_lua_external`): same swap.
- `tests/bridging_models.rs`, `codegen/model_matches/tests.rs`, `tests/bridging_end_to_end.rs`:
  call-shape updates only; the bridge assertions are already native.

### Step 4 — docs and comments

- `models/matches.rs:1-19` module docs: today they say the structure is index-time only and that
  "phase 2 of codegen turns this structure into facts". That is now false for one field. Rewrite as
  "everything a models file matched, whichever phase consumes it", keep the in-memory / cache-purity
  paragraph verbatim (it is true of endpoints too), and add the per-field which-phase-consumes-what
  note. Say explicitly that `ctadl index` warns about `endpoints` (`cli/mod.rs:203-216`) and
  `ctadl query` warns about `propagations`/`bridges` (`cli/mod.rs:347-357`) — the two halves each
  phase ignores are documented, not accidental.
- `models/mod.rs:1-4` header ("Defines a `ModelBuilders` in which to express summary and call
  models") and `json.rs:33-37` (stage 1 "→ the name-based columnar `EndpointBatch` intermediate").
- `models/mod.rs:89`'s parquet TODO: keep it, note it now decodes to `PropagationMatch`.
- `docs/model-generators.md`'s which-phase table gains nothing — it already describes the split.
  `docs/debugging.md`'s parquet references are about facts, not models: no change.
- Add a note to `bridging-models-summary.md` §5.3 pointing at this document as the resolution, and
  record the endpoint-side twin there — including the single-file × multi-import trigger — so the
  finding is not half-recorded.

## 5. What must not change

Pinned by the verification in §6, listed here so a reviewer can check intent against diff:

- Every `EndpointRow` field survives into `EndpointMatch`, `local_index` most of all.
- Endpoint accumulation stays deterministic (§3.2) — plain loop order, no unordered iteration.
- Duplicate endpoints stay duplicated (§3.2).
- Stage 2 stays gated: a query with no matched endpoints must not run `compute_copy_alias`.
- `endpoint_stats` re-keyed per file before merging, and `EndpointStats::merge` /
  `IndexTimeModelCounts::merge` semantics (max, not sum) unchanged.
- The two ignore-warnings still fire once each, on the same conditions.
- `codegen_propagations`' expansion semantics, including the `dst == src` skip — this change must not
  quietly resolve `bridging-models-summary.md` §6.2, which is a separate decision.
- The JNI fact shape, including the degenerate collapse.

## 6. Verification

- **Step 0's red tests** (both §2 triggers) go green in step 2 — fixtures and assertions
  unchanged, harness ported — and are the only new *behavioral* claims in the change.
- `cargo test --workspace` (31 targets) and `cargo clippy --workspace --all-targets` clean after
  each step, not only at the end. (Step 0's tests are `#[ignore]`d until step 2, which is what
  makes both halves of this sentence true at once.)
- `cargo xtask regression --frontend lua` → 20/20; `cargo xtask regression --frontend jni` → 4/4
  inside `nix develop .#regression`. The JNI A/B is the end-to-end guard for phase 2.
- `tests/flowy_tests.rs` + `tests/port_semantics.rs` are the guard for step 1's flowy changes, and
  `port_semantics/` is specifically what pins what a model *port* means.
- **SARIF byte-diff, the strongest fidelity check**: index `com.noto_54.apk` once, then run the same
  `ctadl query -m <two model files>` before and after the change and `diff` the SARIF. This is what
  proves duplicate retention and `CTADL0100` counts are untouched (ordering is not among the claims
  — the formatter sorts, §3.2). Use two model files whose paths *don't* collide, so §2's bug doesn't
  make the diff non-empty for the right reason; then repeat with colliding files and confirm the
  diff is non-empty in exactly the way §2 predicts.
- **Memory, with a baseline**: add the same `[mem cp]` checkpoint after the query-side accumulation
  loop to *both* builds — a one-line patch on the pre-change commit — and quote both numbers for
  that APK, the way `cli/mod.rs:159-175` does for the index side. The claim being checked is §1's
  last bullet, and it is comparative: a single post-change number can neither support nor refute
  it. If the native form isn't smaller, that is worth knowing before the columnar code is gone.

## 7. Decision log

- **Endpoints live in `ProgramModelMatches` (option 1), not a sibling struct.** Under §3.3's
  out-param design a sibling costs one more `&mut` parameter through the five loader entry points
  and `ModelGeneratorIngest` — not a large plumbing cost, so the honest deciding reason is
  simpler: one matcher pass produces both halves, and splitting its output across two accumulators
  buys nothing but a second thing to thread and document. The cost of option 1 is that the struct
  spans both phases, paid in module docs (§4). This is the one genuinely reversible decision here —
  if a third consumer with a third lifetime shows up, split then.
- **Loader takes an out-param** rather than returning an owned struct to merge (§3.3), with the
  partial-mutation-on-`Err` contract stated in the doc comment.
- **`Vec`, not a set**, for endpoints and propagations: dedup is visible in `CTADL0100` counts.
- **Interned `facts::Str` / `facts::Path`**, not `String` / `Vec<PathSegment>`: it is what makes the
  native form no larger than the columnar one, and `Label(Str)` makes the query-side conversion free.
- **No provenance field on `EndpointMatch`, deliberately.** `formatter.rs:295` notes CTADL0005
  cannot name the model file because the row carries no provenance, and this change is the cheap
  moment to add one (`facts::Str` is 8 bytes on a `Copy` struct). Not taken: two of the loader
  entry points have no path to attach (readers and value streams, §3.3), file identity already
  lives with the caller via the report re-key, and widening CTADL0005 is a diagnostics feature
  with its own tests — a separate change, now recorded as such rather than left implicit by the
  field-for-field rule.
- **Accumulation ordering is determinism-only, not fidelity.** The formatter sorts endpoints
  (`formatter.rs:1303-1304`), so insertion order does not reach the output; requiring a specific
  order would pin an invariant nothing consumes and would conflict with the deferred dedup change.
  The requirement kept is only "no unordered iteration on the accumulation path".
- **The ap-id bugs are deleted, not fixed.** Remapping ids on union would be ~20 lines and would
  keep an encoding nothing reads. Recorded so the deletion doesn't later read as an oversight.
- **`SummaryBatch::dedup` is deleted with zero callers** rather than ported. If summary dedup is ever
  wanted, it is a few lines over `Vec<PropagationMatch>` once that type derives `Hash` (it already
  derives `Copy + Eq`) — the 55-line Arrow version is not the thing to keep.
- **Two steps, not one commit.** Step 1 alone removes the round trip and the flowy bug; if step 2
  has to be paused for the option-1/option-2 question to be re-litigated, step 1 still stands.
