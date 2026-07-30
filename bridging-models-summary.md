# Implementation summary: overhauling models - DO-NOT-MERGE

BLUF: `overhauling-models-plan.md` is implemented, in full, across all eight of its sequencing
steps. Model matching now runs on demand as IR streams through the import loop, against each
program's VirtualMethodTable, into a new in-memory `ProgramModelMatches`; a second codegen phase
turns that structure into facts after every import is lowered. Bridging models have a native
representation and are codegen'd directly. No IR is synthesized or modified, and nothing is
persisted.

The strongest verification the plan asked for — `cargo xtask regression --frontend jni` running
a declarative bridge under `--no-jni-bridge` as a direct A/B against the built-in pass — **passes
on real DEX plus Ghidra-recovered pcode**, for both `JniFlow` and `JniArgShift`.

Three things need a decision before merge; they are §6 below. The rest of this document is what
was built (§1–§2), how it was verified (§3), where the implementation departed from the plan and
why (§4), and what was learned in the building (§5).

---

## 1. What was built

Six new files, ~1000 lines of new library code, ~1700 lines of new tests.

### `ctadl-ascent/src/models/spec.rs` (+ `spec/tests.rs`)

Plan §3.1. The program-independent half of a models file: `ProgramScope` (the `in` block),
`ImportScope` (the identity of the import being matched), `SideSpec`, `BridgeSpec`, `PortPair`,
`BridgePort`, `Direction`, `Severity`. `scan_model_files` reads every `--models` file once,
before the import loop, and returns the bridges.

`where` constraints are kept as raw `serde_json::Value` and handed back to the existing
evaluator, so nothing here understands them.

Loader hardening landed with the parse, as §3.1 requires:

- Unknown keys are checked explicitly at the generator level, the `model` level, inside
  `bridge`, inside the `to` block, inside `in`, and inside a port-map entry. The JSON schema's
  `additionalProperties: false` is editor-time only; a misspelled `on-unmached` used to be
  dropped in silence.
- The `.as_array().unwrap()` panic pattern is gone. `super_model_generator`, `super_model`,
  `super_any_of_constraint` and `super_all_of_constraint` now route a non-array through a new
  `ModelGeneratorVisitor::report_not_array` hook, which `ModelGeneratorIngest` turns into a hard
  error. A scalar `"where"` in a user-supplied file used to panic.
- `find` must be `methods` on a generator carrying a `bridge`; anything else is a hard error
  naming the reason (`call` is an EDB relation, so a callsite bridge sees only the statically
  emitted rows).
- `Argument(*)` is rejected in a port map. So are `Variable(...)` and any attempt to name the
  globals pseudo-parameter.
- `language` xor `languages`; an empty `languages` list and an unknown language name are errors.

### `ctadl-ascent/src/models/match_index.rs`

Plan §3.2. `ProgramMatchIndex` — the name/parent/signature/qualified-id/functions/universe
tables, lifted out of `ModelGeneratorIngest`, which now borrows one instead of building its own.
One struct, one construction path, two users: the query-time source/sink pipeline and the
index-time streaming matcher.

`ModelGeneratorIngest::match_where(n, constraints)` is the reuse point. It runs the constraints
of a synthetic one-generator value through the ordinary visitor and returns the matched fq-names,
so `any_of`/`not`, `qualified-id` and every unknown-field error apply to a bridge side verbatim.
There is no second implementation of `where`.

Building the index once per import (rather than once per model file per import) also removes a
redundant decode on both the index and the query path.

### `ctadl-ascent/src/models/matches.rs`

Plan §2 and §3.4. `ProgramModelMatches { propagations, access_paths, bridges }`.

- `propagations: Vec<PropagationMatch>` — an interned function name plus two `ModelPort`s.
  `AnyArgument` is deliberately still a *tag* here; expanding it needs arity over call sites
  across every import.
- `access_paths: BTreeSet<facts::Path>` — the human-declared registry.
- `bridges: BridgeMatches` — per spec, the accumulated side-A and side-B match sets, unioned
  across imports.

`observe_import` evaluates both sides of every applicable bridge per import — **eagerly**, since
side B's import may arrive before side A's — and the conditional `to`-side semantics are applied
later, in phase 2. The module docs state that reconciliation explicitly, which is the sentence
the plan (§3.4) says the first implementer otherwise discovers as a contradiction.

### `ctadl-ascent/src/codegen/model_matches.rs` (+ `model_matches/tests.rs`)

Plan §6 and §7. Phase 2 of codegen, run once after all IR.

Both arity snapshots (`compute_arg_arity` for `Argument(*)`, `compute_num_params` for the bridge
arity warning) are taken before this phase pushes a single row, so neither observes its own
output.

- **Propagations** → `facts.summary`, replicating the old `codegen_summary`'s expansion exactly:
  `AnyArgument` fan-out, the `dst == src` skip, and the `formal_param` pushes for both ports.
  `codegen_summary` was a thin adapter onto this and has since been deleted along with the
  columnar model encoding (see §5.3), so there is one implementation and one caller path.
- **Declared access paths** → `facts.paths`. This is the only thing in the whole change that
  pushes a `paths` row.
- **Bridges** → per pair: a fresh site inside the caller, `call`, one temporary per distinct
  callee index keyed `(site, index)`, `actual_param` passing each temporary whole, one `assign`
  per port direction keyed on the **site** (not the function id), and `formal_param` on **both**
  sides for every mapped port. The globals pair is unconditional. A port with an empty `to_path`
  and `direction: both` that is the sole port on its callee index collapses to a direct
  `actual_param`, which keeps the shipped JNI fact shape byte-identical. Zero `facts.paths`
  pushes.

`classify` applies the reporting semantics: `on-unmatched` per side (default `warn`),
`on-ambiguous` (default `warn`), full cross-product pairing, "unmatched" meaning *not matched
anywhere in the project*, and no `to`-side report when side A matched nothing.

### `cli::index` and `cli::query`

Plan §7 and §8. `index` scans specs before the loop, matches each import while its IR is in hand
and drops the index with it, then runs phase 2 after `jni::link`. It logs the per-generator
bridge count line unconditionally at `info`, and warns once, naming the files, when source/sink
models were passed to `index`. `query` warns once when propagation or bridging models were in its
input.

### Schema, docs, regression harness

- `ctadl-model-generator.schema.json`: new `$defs` for `program-scope`, `port-map`,
  `bridge-model`, `report-severity`, `artifact-language`; `model.bridge` and
  `model.access_paths`; generator-level `in` and `on-unmatched`. **`forward_call` and its
  `call-model` definition are removed**; `forward_self` remains.
- `docs/model-generators.md`: an `in` section, a `bridge` section (pairing, diagnostics,
  composition limits, JNI double-bridging, index-time-only, the `find: callsites` rejection), an
  `access_paths` section, a which-phase-consumes-what table, the Lua externals name-only caveat,
  and three new summary-table rows.
- `docs/jni.md`: cross-references `model.bridge` for the boundaries the pass cannot reach, notes
  that `--no-jni-bridge` is what to pair a hand-written bridge with, and records that the
  per-method resolution lines are at `debug` (`RUST_LOG=debug` — there is no `-v` flag; the plan
  said `-v`, the tree uses `env_logger`).
- `xtask`: a sibling `<kebab>.bridge.jsonl` beside a JNI case now yields a second discovered
  case, `Jni:<Stem>+bridge`, which indexes the same two artifacts with `--no-jni-bridge -m
  <bridge>` and makes the same claims.

---

## 2. Sequencing

Every step of plan §11 is done.

| step | state |
| --- | --- |
| 1. Loader groundwork; `forward_call` removal | done |
| 2. `ProgramMatchIndex` extraction | done (see §4.1 for the one deviation) |
| 3. `in` plumbing through `try_load_models*` | done |
| 4. Streaming matching into `ProgramModelMatches` | done, with the streaming-order test |
| 5. Phase-2 codegen | done |
| 6. Reporting semantics + diagnostics | done |
| 7. End-to-end: two imports, then the JNI A/B | done (Lua, not flowy — see §4.2) |
| 8. Docs and the memory number | done (partial number — see §6.1) |

---

## 3. Verification

### Automated

- **`cargo test --workspace`**: 31 test targets, all pass. 23 of the tests are new.
- **`cargo xtask regression --frontend lua`**: 20/20.
- **`cargo xtask regression --frontend jni`** (in `nix develop .#regression`): **4/4** —
  `Jni:JniFlow`, `Jni:JniFlow+bridge`, `Jni:JniArgShift`, `Jni:JniArgShift+bridge`.
- `cargo clippy --workspace --all-targets`: no new warnings.
- `dex`, `jvm` and `pcode` frontends were not run: they need `dex-reader` / `jvm-reader` /
  Ghidra, and only the `jni` frontend was run inside the regression shell.

### What the JNI A/B proves

This is the result worth reading. `Jni:JniFlow+bridge` and `Jni:JniArgShift+bridge` index the
*same* DEX and the *same* Ghidra-recovered shared library as their built-in counterparts, with
the pass switched off and a hand-written `model.bridge` in its place, and satisfy the *same*
`expected_lines` / `unexpected_lines` / `expected_native_lines` claims.

- `JniFlow` exercises the unconditional globals mapping: taint enters one native function, is
  held in a native global, and leaves through a *different* one. Nothing in the Java half
  connects them, and no per-function propagation model could fake it.
- `JniArgShift` exercises the port map: the implementation returns `b` and drops `a`, and the
  Java half calls it twice with the taint in the other argument each time, so an off-by-one
  anywhere in the map flips both assertions at once.

### Coverage against plan §10

Everything in the plan's verification list is covered except two items noted below.

- Parse/validate (no program): unknown keys at all three levels, missing `to`, `Argument(*)`,
  `find: callsites` with a bridge, `in` mutual-exclusion / empty / unknown-language,
  `{"language":"dex"}` ≡ `{"languages":["dex"]}`, non-array `where` as an error rather than a
  panic, each `on-unmatched` / `on-ambiguous` setting. — `models/spec/tests.rs`
- Matching: two-`ProgramMatchIndex` fixture asserting both sides and the resulting pairs; the
  streaming-order case (side B's import first, identical outcome); "matched anywhere";
  scope-admits-no-import. — `tests/bridging_models.rs`
- Reporting semantics: `from` empty + `ignore` ⇒ no `to` report even under `to: error`; `from`
  matched + `to` empty ⇒ report; 2×1, 1×2 and 2×3 ambiguity with counts and provenance. —
  `codegen/model_matches/tests.rs`
- Emission: the degenerate JNI shape collapsing to direct `actual_param` rows with a fresh site
  and both-side formals; the Lua shape sharing one temporary across three sub-paths without
  aliasing; `direction` selecting which assign rows exist; site-keyed assigns; ports past the
  callee's arity still emitted; two bridge sites in one caller getting distinct temporaries;
  zero `facts.paths` rows. — `codegen/model_matches/tests.rs`
- Phase-2 propagations: `Argument(*)` expanding over call sites spanning two imports; the
  `dst == src` skip; `formal_param` for both ports; model paths landing in `facts.summary` and
  not `facts.paths`; declared paths landing in `facts.paths`. — `codegen/model_matches/tests.rs`
- End-to-end, two imports: positive, negative-without-the-model, a pathful bridge composing at
  an exactly-matching path, and the bridge-removed negative for that. —
  `tests/bridging_end_to_end.rs`
- End-to-end, two real frontends: the JNI A/B above.

**Not asserted**: the bridge arity warning fires (the rows it accompanies are asserted; the log
line is not — asserting it needs log capture), and that the two ignore-warnings fire exactly
once (the counts feeding them are asserted).

### Memory

The post-loop checkpoint (`[mem cp] after import loop`) was measured on `com.noto_54.apk`
(6.4 MB, one import, 1224 propagation matches from the shipped Java defaults plus one bridge
spec):

```
[mem cp] after codegen_program (IR dropped, facts built): 406.7 MB
[mem cp] after import loop (1224 propagation match(es), 0 declared path(s), 1 bridge spec(s) retained): 406.7 MB
```

The retained matches are below the resolution of the gauge. Peak physical footprint for the whole
run was 2.27 GB, in `ascent_run` — not in matching. The number is recorded in a comment at the
checkpoint.

---

## 4. Where the implementation departed from the plan

### 4.1 `ProgramMatchIndex` borrows the program rather than owning its strings

Plan §3.2 specifies an *owned* index and prices the lifetime refactor across every constraint
visitor. That cost was not paid, deliberately.

The facts design wanted owned strings because it retained a `Vec<ProgramMatchIndex>` across the
import loop. This plan replaced that with streaming (§3.4): each import builds an index,
everything applicable is evaluated against it, the *matches* are copied out as owned data, and
the index and the IR are dropped together. Under that posture the index never outlives its
`ProgramInfo`, so owning the strings buys nothing.

What the plan actually wanted from the extraction — "one struct, one construction path, two
users", and no second implementation of `where` — is delivered. The reasoning is in the module
docs so it does not read as an oversight.

### 4.2 The two-import end-to-end fixture is Lua, not flowy

Plan §10 asks for "two flowy imports". Driving flowy through `cli::index` / `cli::query` reports
no source→sink path **even with the flow entirely inside a function body and no models
involved** — verified against a fixture with the flow in `alphaStub`'s own body. That is
pre-existing and unrelated to this change: the `.tnt` fixtures run through `flowy::check`, not
the CLI, and no existing test drives flowy through the CLI.

Lua satisfies the plan's stated reason for choosing flowy ("needs no Android or Ghidra
toolchain") identically, and `multi_import_sarif.rs` already proves the two-import query path
works with it. It also gives a *better* fixture: the Lua frontend's externals column makes
`alpha_stub` a genuinely bodyless callee with no `FunctionData` at all, which exercises
end-to-end the "match on the VMT, not `FunctionData`" decision that plan §12 records as
load-bearing.

The flowy CLI gap is worth its own issue; it is not touched here.

### 4.3 `on-unmatched` for side B lives in the `to` block

Plan §9 lists `on-unmatched` under `$defs/bridge-model` with the parenthetical "(on the `to`
block; the generator-level `on-unmatched` covers the `from` side)". The parenthetical is what was
implemented, since §4.2 describes the key as "independently part of the `from` side and the `to`
side" and the `to` block is side B's block. Only one spelling is accepted, so the two cannot
drift.

### 4.4 `access_paths` needed a syntax the plan did not give

Plan §2 specifies `access_paths` as a human-declared registry and §7.2 specifies where it goes,
but neither §9 (schema) nor anywhere else gives it a spelling. It is implemented as
`model.access_paths: [string]`, each entry parsed with the canonical access-path grammar, the
same way a port's trailing path is. It lives under `model` because that is where the other
model constructs live, and it is handled during ingest so it works uniformly for `--models` files
and for the shipped defaults.

### 4.5 The degenerate collapse requires a *sole* port on the callee index

Plan §6 says a port with empty `to_path` and `direction: both` collapses to a direct
`actual_param`. Implemented with one added condition: the callee index must carry exactly that
one port. If an index carried both a collapsed port and a pathful one, the direct row and the
temporary would both bind the same call-argument pseudo-variable whole and re-alias precisely
what the temporary exists to keep apart. Every JNI port is a distinct callee index, so the
shipped shape is unaffected — pinned by both the JNI-shape unit test and the JNI A/B regression.

### 4.6 `RUST_LOG=debug`, not `-v`

Plan §9 asks the docs to note that the per-method `jni bridge:` line "requires `-v`". There is no
`-v` flag; the tree uses `env_logger`. The docs say `RUST_LOG=debug`.

---

## 5. Findings from the build

### 5.1 A same-index, different-path propagation cannot be expressed

`codegen_propagations`' `dst == src` skip (inherited verbatim from `codegen_summary`) compares
**indices**, not (index, path) pairs. So
`{"input": "Argument(0).stack", "output": "Argument(0).out"}` produces no summary row at all —
silently.

This matters directly for the design's flagship Lua example, whose port map puts all three ports
on callee `Argument(0)` at different sub-paths. Plan §5 already states the precondition that such
a callee must be hand-modelled "in exactly the port map's vocabulary at exactly the port map's
paths"; this finding sharpens it into something stronger: **that hand-written model cannot map
`Argument(0).x` to `Argument(0).y` at all.** The end-to-end pathful fixture works around it by
mapping to `Return`, and both the fixture and `docs/model-generators.md` say why.

Plan §7.1 mandates replicating the skip, so it was replicated. Making it path-aware is a real
change to summary semantics and belongs in its own decision.

### 5.2 The composition gap is narrower than §5 implies, in one direction

Plan §5 says composition past the seam is "exact-match only". In practice a callee summary whose
endpoint is a *prefix* of the port path also composes, because prefix substitution carries the
residue: a bridge delivering taint to `t.stack` composes fine with a callee summary of
`Return ← Argument(0)`. The strict statement holds for summaries *deeper* than the port. An
end-to-end negative that assumed otherwise had to be rewritten; the surviving negative removes
the bridge instead.

### 5.3 `SummaryBatch::union_with` collides access-path ids

> **Resolved.** See `removing-modelbuilders-plan.md`, implemented on this branch: the columnar
> model encoding is gone, so there are no access-path ids left to collide. Propagations and
> endpoints both carry a `facts::Path`, accumulation is `Vec::extend`, and the two triggers
> below are pinned by tests in `tests/models_loading.rs`
> (`two_model_files_keep_their_own_endpoint_paths`,
> `one_model_file_across_two_imports_keeps_its_endpoint_paths`).

`AccessPathBuilder` numbers paths from 0 in every builder, so concatenating two batches makes the
second batch's summaries resolve their paths through the first's table. This is pre-existing —
`cli::index` unioned the default batch with each `--models` batch — and the new code sidesteps it
rather than inheriting it: each file's batch is converted into `ProgramModelMatches` against its
own path table, and no summary batches are unioned on the index path. `codegen/flowy.rs` still
unions, and is still exposed for a caller passing two model files.

**The endpoint side has the same bug, unreported until now, and it is the worse half.**
`EndpointBatch::union_with` concatenates rows without remapping and `build_ap_map` is
last-writer-wins on duplicate ids, so a source or sink is silently widened, narrowed, or moved
rather than merely mis-summarized. `cli::query` unions one batch per **(import × model file)
pair**, which gives it two distinct triggers:

- **Two model files** whose ports carry differently-shaped trailing paths: the first file's
  endpoints resolve through the second file's table.
- **One model file matched against two imports.** Each import produces its own match set —
  different functions exist in each program — so the same file yields different append sequences
  and the same ids bind to different path tables. A single file whose generators match pathful
  ports in both a Java import and a native import — the bridging use case — collides with
  itself.

The only benign configuration is one file × one import (a single batch, nothing unioned), which
is what has been run to date; that, and not any structural safety, is why this was not observed.

### 5.4 The bridge warnings work, observably

The APK memory run happened to carry a bridge model that matched nothing, and produced exactly
what the design intends:

```
WARN  bridge …/bridge.jsonl:0: the 'from' side matched no function in this project, so nothing
      is bridged. Check the 'where' constraints and the 'in' scope.
INFO  bridge …/bridge.jsonl:0: 0 from, 0 to, 0 pair(s) bridged
```

---

## 6. Open before merge

1. **The APK + `.so` memory number is not measured.** Only the APK half is (§3). The `.so` half
   needs a Ghidra import; plan §8 says to quote a real number for the pair "before calling it
   settled". The checkpoint and the partial number are in the code.
2. **The `dst == src` skip (§5.1).** Path-aware or not is a decision, and it constrains the Lua
   shape the design leads with.
3. **The owned-index question (§4.1).** If cross-import retention is ever wanted for another
   reason, the lifetime refactor comes back.

Two smaller follow-ups the plan already scoped and this change deliberately did not do: source
attribution for the synthetic bridge site (it has no `source_map` entry; the SARIF step emitter
returns early), and re-expressing the hardcoded `AsyncTask.execute` → `doInBackground` rule in
`models/codegen.rs`, which is `forward_self`-shaped rather than a two-program bridge.
