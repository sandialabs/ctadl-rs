# Criticism of `removing-modelbuilders-plan.md` - DO-NOT-MERGE

BLUF: The direction is right and most of the plan's factual claims check out against the code —
nothing persists the columnar layer, `SummaryBatch::dedup` really has zero callers, every use of
the deleted types is inside `ctadl-ascent`, and both ap-id collision bugs are real
(`build_ap_map` is last-writer-wins on duplicate ids, and `flowy.rs:342` /
`cli/mod.rs:339-343` both union without remapping). But the plan contains one wrong
characterization of the bug's trigger conditions, one wrong claim about current flowy behavior,
an internal contradiction in the sequencing, and a query-path performance regression it doesn't
guard against. Each is fixable without changing the plan's shape.

## Major

### 1. §2's "same-file / multi-import is benign" claim is false, and the red test under-covers

§2 says the endpoint-side collision needs two *model files* with differently-shaped paths, and
that same-file / multi-import is benign because "identical appends in identical order produce
identical ids". That is only true when every import produces identical matches — which is the
opposite of the point of per-import matching. `cli::query` unions one batch per **(import ×
file) pair** (`cli/mod.rs:317-344`). The same file matched against two different imports
produces two different match sets (different functions exist in each program), hence different
append sequences, hence the same ids bound to different path tables. One model file whose
generators match pathful ports in both a Java import and a native import — exactly the
bridging use case this branch exists for — collides with itself.

Consequences for the plan:

- The "why this has never been seen" story is wrong, which matters because it is the plan's
  implicit severity assessment.
- Step 0's red test (two files, one program) covers only one of the two triggers. It should
  also cover one file × two imports, or the §6 "repeat with colliding files" SARIF check should.
- The deletion still fixes both triggers — the *fix* is fine; the *characterization and test
  matrix* are not.

### 2. Step 1's flowy behavior-change #2 is (half) wrong — the plan's model of `codegen/models.rs` is stale

The plan claims flowy models "now get `AnyArgument` expanded by phase 2 … because it runs the
same phase 2 the index path does. Previously it ran `codegen_summary`". But `codegen_summary`
*already is* a thin adapter over phase 2: `codegen/models.rs:21-32` builds a
`ProgramModelMatches` and calls `codegen_model_matches` — its own module doc says "The
expansion itself lives in `codegen::model_matches`, which is phase 2 of codegen". `AnyArgument`
expansion on the flowy path is identical before and after this change.

Only the second half of the claim is a real change: flowy currently drops
`batch.access_paths` on the floor (`flowy.rs:339-343` takes only `.summary`), and after step 1
it would honor them. The planned commit message would document a behavior change that doesn't
exist, and — more worrying for a reviewer — it suggests the plan was written against a
pre-refactor memory of `codegen/models.rs` rather than the current file. Worth a re-read pass
over any other claim inherited from earlier design docs.

### 3. Step 0 contradicts §6, and the red test's harness doesn't survive step 2

§6 requires `cargo test --workspace` clean **after each step**, but step 0 lands a test that
"fails today" and stays red until step 2. Those cannot both hold. Pick one mechanism and say
so: `#[ignore]` with a comment naming the plan section, a `#[should_panic]`-style inversion
that gets flipped in step 2, or land step 0 in the same PR as step 2 with the commit history
showing red-then-green.

Separately: a test written "the way `cli::query` loads them" must use
`ModelsBatch::union_with` + `build_query_endpoints(&batch.endpoint, …)` — APIs step 2 deletes.
So the test's harness is necessarily rewritten in the very commit that turns it green, which
weakens "fails before, passes after" as evidence. The plan should state which part survives
verbatim (the model-file fixtures and the final assertions on resolved endpoint paths) and
accept that the loading scaffolding changes.

### 4. The naive step-2 rewrite of `cli::query` removes a load-bearing gate

Today stage 2 runs only under `if let Some(ref batch) = models_batch` (`cli/mod.rs:379`), so a
query with no `-m` files never calls `build_query_endpoints`. That function does real work
before touching a single endpoint: `compute_copy_alias` runs union-find over the whole
`assign_like` relation and builds `vertex_paths` over every row (`endpoints.rs:72-101`). Once
`models_batch: Option<…>` becomes an always-present `ProgramModelMatches`, the natural rewrite
calls stage 2 unconditionally and every model-less or flowy-only query pays the union-find on
a full index. The plan's §3.3/§4 call-site notes don't mention preserving the gate. Add it
explicitly: skip stage 2 when `matches.endpoints.is_empty()`.

## Moderate

### 5. The out-param loader's error contract is unspecified

`try_load_models(index, path, out) -> Result<ModelLoadReport>`: what is in `out` when the
result is `Err`? The current design is transactional per load — a failed load returns no
batch, so the caller's accumulator is untouched. With the out-param, rows appended before the
error (JSON model errors are *collected* during a batch and returned at the end —
`encode_models_from`, `json.rs:750-760` — after rows have already been emitted) now sit in the
caller's accumulator. All production callers `?`-abort, so behavior doesn't change today, but
the contract should be written into the doc comment, because `tests/json_error_handling.rs`
already exercises exactly this state (reads rows out of a builder after collecting errors,
`:171`, `:333`) and someone will eventually want continue-on-error loading across files.

### 6. The plan re-signatures everything but keeps two known provenance gaps

This change touches all five loader entry points and re-derives the endpoint row type from
scratch — and passes up the two places where "no file provenance" is a documented pain point:

- `ModelLoadReport.endpoint_stats` keeps the bare `(generator index, direction)` key, so
  `cli::query` must keep its re-key-by-file dance (`cli/mod.rs:327-338`), preserved by §3.3
  verbatim. The loader *takes the path as a parameter*; keying the report by file (or carrying
  the file in the report) deletes the dance and the footgun comment above it.
- `EndpointMatch` carries no provenance, and `formatter.rs:295` explicitly laments this:
  the CTADL0005 notification cannot point at the model *file* because "`EndpointRow` carries
  no provenance".

"Every `EndpointRow` field survives" is the right fidelity discipline for the mechanical step,
but the decision log should record "no provenance added, deliberately, because X" — otherwise
the one moment where adding a field was cheap passes silently. (A `facts::Str` file name on
`EndpointMatch` costs 8 bytes on a `Copy` struct.)

### 7. The §3.2 "accumulation order is a fidelity requirement" is probably vacuous — check before pinning it

The formatter collects sources and sinks into `BTreeSet<&QueryEndpoint>`
(`formatter.rs:1303-1304`), i.e. it *sorts* them. If endpoint insertion order doesn't reach the
SARIF at all, then promoting "import outer, file inner, visit order within a file" to a
requirement pins an invariant nothing consumes, and it will be cited against future refactors
(it already sits awkwardly next to the deferred dedup change, which necessarily perturbs both
order and counts). Do the §6 SARIF diff first, with a deliberately permuted accumulation
order; if the diff is empty, delete §3.2's requirement and say ordering is unconstrained. If
some path *is* order-sensitive, then one APK with two model files is thin coverage for an
ordering invariant and the test should be named as its permanent guard.

### 8. The §6 memory check has no baseline

"Add a `[mem cp]` checkpoint after the query-side accumulation loop and quote the number"
measures only the new build. The claim being tested — §1's "interning makes the native form
smaller" — is comparative. The same checkpoint has to be added to the pre-change build (a
one-line patch on `main`) and both numbers quoted, or the measurement can't support or refute
the claim. The index-side precedent the plan cites (`cli/mod.rs:159-175`) got this right by
recording the comparison in the comment.

### 9. Step 1's intermediate `ModelsBatch` shape is unstated

Step 1 makes `ModelBuilders.propagations` a `Vec<PropagationMatch>` and deletes
`SummaryBatch` — but `ModelsBatch` still exists until step 2, its `summary` field must become
the `Vec`, and `ModelsBatch::union_with` (still called by `cli::query` in step 1) must switch
from `concat_batches` to `Vec::extend` for that field. None of this is written down. It's
forced by the compiler, so nothing can go silently wrong, but a plan that specifies
`cli/mod.rs:111-117`'s new closure body to the line should also specify the one transient
struct shape both steps depend on.

## Minor

- **§7's option-2 cost argument contradicts §3.3.** Option 2 is rejected because a sibling
  struct "means two return values threaded through all five entry points" — but §3.3 already
  chose out-params over return values. Under the chosen design, option 2 costs one more
  `&mut` parameter, not two return values. The conclusion may stand; the stated reason doesn't.
- **The ~850-line estimate is low.** Deleting everything from `ModelsBatch` (line 260) through
  end-of-file, minus the surviving `FormalIndexTypeTag` block (~45 lines), is closer to 1000
  lines of `mod.rs`. Harmless, but the number is quoted in the BLUF.
- **"Keep the `u8` conversions if the compiler still wants them" is backwards** — the
  compiler never *wants* them; dead `From` impls survive `rustc` silently and clippy
  `--all-targets` (a §6 gate) may flag them. Decide now: they die with `FormalIndexBuilder`
  unless a live use remains (grep says the only non-builder use is the `iter_endpoints`
  decode, which is also deleted).
- **Small line-reference drift**, cosmetic only: `json_error_handling.rs` row-reading sites
  are `:171`/`:333` (plan says `:170`/`:331`); `default_models.rs`'s `build_ap_map` use is at
  `:250` (plan cites `:336`); the endpoint appends are `json.rs:665-676`/`:706-717`.

## What the plan gets right (verified, not just plausible)

- No persistence path touches the columnar types; `load_and_map_summaries` reads parquet
  `facts.summary`, not `ModelsBatch`.
- All uses of the deleted types are in `ctadl-ascent/src` and its tests; no cross-crate break.
- `SummaryBatch::dedup`: zero callers. `iter_summaries` in production: only the
  `extend_from_summaries` decode (the other two callers are test code, as the plan's
  "in production" qualifier implies).
- `facts::Str` is a `Copy` `u64` into a process-global table and `facts::Path` is `Copy`, so
  `EndpointMatch: Copy` works and the interning argument is at least directionally right.
- `local_index` genuinely cannot be re-derived at query time — stage 1 resolves it against
  pre-optimization locals (`json.rs:646-663`) that `eliminate_dead_temps`/`coalesce_copies`
  destroy before the graph exists. Keeping it loud in §3.1 is correct.
- The `observe_import` double-borrow problem (§ step 2) is real, and the
  accumulate-then-fold fix is the right shape for it.
- The endpoint-vs-propagation warning symmetry, the `EndpointStats::merge` max-not-sum
  semantics, and the `CTADL0100` post-fan-out counting are all correctly identified as
  invariants to preserve.
