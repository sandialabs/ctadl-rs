# Implementation plan — CHA with a surgical fallback - DO-NOT-MERGE

Builds `spec.md` in ten steps. Each step compiles, passes tests, and can be committed on its
own. Step numbers match spec §16. Capture every measurement run's output under
`/Volumes/Shampoo/ctadl-sweep/`.

Test commands used throughout:

```
cargo test --workspace
cargo xtask regression --frontend c,lua,pcode
cargo xtask regression --frontend jvm,dex,jni
```

## 1. Add `super_start` to the IR and bump the import format

**What.** `CallStyle::JavaCall` gets a `super_start: Option<Symbol>` field. The dex and jvm
frontends fill it for `Super` calls. Format goes to `8`. Nothing resolves differently yet.

**Files.**
- `ctadl-ir/src/mir/call.rs` — the field and its doc comment.
- `ctadl-ir/src/mir/visit/mod.rs` — the visitor rebuilds `JavaCall`; pass the field through.
- `frontends/ctadl-dex/src/lib.rs` — near line 554: interface the class implements ⇒ the named
  class, else the enclosing class's superclass (known at line 169).
- `frontends/ctadl-jvm/src/lib.rs` — near line 651: same rule, plus constructors and private
  calls ⇒ the named class (superclass known at line 163).
- `ctadl-import/src/project.rs:106` — `IMPORT_FORMAT_VERSION = "8"` and the history entry.
- Every other `CallStyle::JavaCall {` constructor gets `super_start: None`:
  `ctadl-ascent/src/cli/mod.rs`, `codegen/mod.rs`, `codegen/tests.rs`, `report/callgraph.rs`.

**Tests.** Workspace tests pass. Both Java regression suites pass unchanged. A loader test in
`ctadl-import` shows a format-7 store is rejected with the re-import message. Then re-import
the 61-app corpus once (`run-one.sh` over `manifest.tsv`) and keep the stores.

## 2. Fix the single-abstract-method test and move it into `ctadl-ir`

**What.** `TypeFacts::from_vmt` (`report/callgraph.rs:748`) moves to `ctadl-ir/src/mir/call.rs`
as a method on `VirtualMethodTable`. It closes over super-interfaces, subtracts default methods,
and ignores `toString`/`equals`/`hashCode`. Report calls the moved version.

**Files.** `ctadl-ir/src/mir/call.rs`, `ctadl-ascent/src/report/callgraph.rs`.

**Tests.** Extend the fixture at `report/callgraph.rs:1545`: a `Provider` extending
`javax.inject.Provider` is detected; one abstract plus one default method is detected; one
abstract plus a redeclared `equals` is detected. Report on DuckDuckGo's store and confirm
`dagger.internal.Provider` now appears in `functional_interfaces`.

## 3. Resolve `invoke-super` (rung 0)

**What.** `ClassHierarchyAnalysis` keeps `declared` and `parents` maps built from the inputs to
`run_cha`, plus a memo. `super_resolvent` walks up from `super_start.unwrap_or(cls)`. In the
`Mixed` arm only, a `Super` site that resolves to exactly one target emits that one `call` row.
Anything else falls through unchanged.

**Files.** `ctadl-ascent/src/codegen/mod.rs` (struct at 958, `run_cha` at 1163, the `Mixed`
arm at 547).

**Tests.** Unit tests in `codegen/tests.rs`: superclass chain; interface default method; class
missing from the hierarchy falls through; ambiguous diamond falls through; method reference
naming the current class does not self-loop. Java regression suites pass. Report on TikTok's
store: super sites resolving exactly go from 311 toward 49,774.

## 4. Count every Java site into four buckets

**What.** `SiteBuckets` with the sub-counts, one per `JavaDispatch` kind, filled where rows
are emitted so it works under every strategy. `codegen_program` returns it. `cli::index` prints
the `calls:` block beside the `models:` line. `debug_assert!` the sum equals the site count.

**Files.** `ctadl-ascent/src/codegen/mod.rs`, `ctadl-ascent/src/cli/mod.rs:252`.

**Tests.** Unit test in `codegen/tests.rs`: a hand-built program with one site of each shape
per dispatch kind; the four counts sum to the site count. Index antennapod under today's
`mixed` and keep the printed line as the baseline for step 5.

## 5. The ladder without models

**What.** `SiteAction` and `classify`. `CallPolicy` struct carrying `K`, the interface `K`, the
order and the model switches, passed to `codegen_program`. `Mixed` becomes the ladder with
rung 1 stubbed out. The old `Mixed` body becomes `LegacyMixed`. Flags on `index` and `go`.
`CallPolicyRecord` written into the on-disk `IndexConfig`; `query` prints it at `info`, or says
it is missing. Warn when a ladder flag is given with a non-`mixed` strategy.

**Files.** `ctadl-ascent/src/codegen/mod.rs`, `ctadl-ascent/src/main.rs` (`IndexArgs`,
`GoArgs`), `ctadl-ascent/src/cli/mod.rs` (index and query), `ctadl-import/src/project.rs:160`.

**Tests.** Table-driven `classify` test over dispatch kind × target count × order.
End-to-end test in `ctadl-ascent/tests/`: the same hand-built program indexed under
`legacy-mixed` and under `cha` produces the fact tables it produced before this step, byte for
byte (build the expected tables at the parent commit and check them in). `check_index_config`
still accepts an index whose `call_policy` is absent. Then the A/B: `ab.sh` on antennapod,
newpipe and schildi under the 24 GiB guard. Newpipe finishing is the result that matters.

## 6. `find: "dispatch"` in the model DSL

**What.** `FindMethod::Dispatch`. The loader accepts exactly one of `propagation` and
`resolve: "inline"` and rejects every other `model` key and every per-function constraint by
name. `ProgramMatchIndex` builds the dispatch universe lazily from the program's `JavaCall`
keys. `CurrentSet::Dispatch` points the existing constraint code at it. Matches land in
`ProgramModelMatches::dispatch` with the `Inline > Model > Skip` precedence. Schema updated.
The `close`/`dispose` inline default can ship here since it needs no synthetic function.

**Files.** `ctadl-ascent/src/models/json.rs` (`FindMethod` at 325, `CurrentSet` at 355,
model-key parsing near 1025 and 1758), `models/match_index.rs`, `models/matches.rs:210`,
`models/ctadl-model-generator.schema.json:398`, `models/defaults/java-index.jsonl`.

**Tests.** In `models/tests`: each rejected constraint and key gives the named error; empty
`propagation` loads; `resolve: "inline"` loads; neither, both, or another `resolve` value
errors. In `codegen/tests.rs`: an inline key with exactly one target takes CHA, with zero or
two or more targets defers, in both orders. Two generators on one key resolve to the higher disposition and both appear in
provenance. A key naming an undeclared interface (`java.util.Iterator`) matches. Extend
`ctadl-ascent/tests/default_models.rs`: the shipped Java file still parses.

## 7. Synthetic functions and the source/sink guard

**What.** Rung 1 goes live. `Model` interns `ctadl$dispatch$…`, emits one `call` row per site
and lets phase 2 write the summary. `Skip` emits nothing. `Inline` takes the rung 3 body. The
R4 guard intersects a key's targets with `ProgramModelMatches::endpoints` and refuses with a
warning. The `index` warning about endpoint models is reworded. `endpoint_model_digest` is
recorded and `query` warns on mismatch.

**Files.** `ctadl-ascent/src/codegen/mod.rs`, `codegen/model_matches.rs` (header comment at
15, formals at 126), `ctadl-ascent/src/cli/mod.rs:264` (warning, digest), `ctadl-import/src/project.rs`.

**Tests.** New `ctadl-ascent/tests/dispatch_models.rs` in the style of `default_models.rs`, a
hand-built Java program with a VMT: one dispatch model ⇒ one `call` row to the synthetic
function, its summary rows, zero CHA rows at the site, taint flows through. Skip ⇒ site has no
callee and no flow. Inline ⇒ `callee_info`, no `call` rows, a sink inside one `close()` body is
reached when the receiver is allocated locally. Refusal ⇒ a sink on a class in the target set
makes the site take CHA and the warning names both. Query with a different endpoint file ⇒ the
digest warning prints.

## 8. Ship the defaults

**What.** The §11 entries in `java-index.jsonl`, with a header comment pointing at
`sigstudy/purity.py`. This is the first step that changes results for a user passing no flags.

**Files.** `ctadl-ascent/src/models/defaults/java-index.jsonl`.

**Tests.** `default_models.rs` still parses the file. Both Java regression suites pass; read
any that move. TaintBench, 38 apps, findings diffed per app against step 5's build; expect
movement from rung 0 and read it before touching `K`.

## 9. `ctadl report --models` and the policy section

**What.** `report` takes `--models`, `--no-default-models` and the three ladder flags. It
builds a `ProgramMatchIndex`, evaluates only dispatch generators, and adds a `policy` section:
buckets, edge counts, top inlined, top modelled, refused, unmodelled closures. The by-name
closure list ships as data in the defaults file. Docs for `find: dispatch` open with what a
model hides.

**Files.** `ctadl-ascent/src/main.rs` (`ReportArgs` at 231), `ctadl-ascent/src/report/mod.rs`
(`ReportOptions` at 78), `report/callgraph.rs`, `models/defaults/java-index.jsonl`,
`docs/model-generators.md`.

**Tests.** Report unit test: the policy section is built from the complete key table, so a
zero-excess signature is counted. Run `report --format json` on the re-imported 61-app corpus
and check median shares against the simulation (about 16% modelled, 2.6% skipped, 1.8% inlined).

## 10. Trim `callee_resolvents`, then the optional items

**What.** Emit `callee_resolvents` only for keys that took `Defer`, from `finish_with_vmt`.
Optional, only if the A/B says import-time peak matters: pre-filter the `call` clone in
`models/codegen.rs:13`. Optional and borderline with intent bullet 3: a count of deferred sites
that ended the fixpoint with no callee, printed beside the bucket line.

**Files.** `ctadl-ascent/src/codegen/mod.rs` (`emit_callee_resolvents` at 1133,
`finish_with_vmt` at 264), optionally `models/codegen.rs`, `index_engine/mod.rs`.

**Tests.** Every regression suite produces the same findings before and after the trim. Re-run
the A/B and record peak memory beside step 5's numbers.
