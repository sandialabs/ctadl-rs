# Overhauling models: complete plan - DO-NOT-MERGE

BLUF: Overhaul model matching so that it is done on demand, as we load IR, against each program's
VirtualMethodTable, and stored into a new in-memory structure — `ProgramModelMatches` — that is
codegen'd into facts during a new second phase inside codegen. Bridging models get a native
representation and are codegen'd directly into facts; no IR is ever synthesized or modified.

This document supersedes `overhauling-models.md`. It incorporates the resolutions from
`overhauling-models-criticism.md`, and adopts `bridging-models-design.md` (the *facts design*) by
reference for the concrete `model_generator` syntax (its §3) and for emission semantics (its §2 and
§4.4) — **as corrected** by `bridging-models-criticism.md` (the *facts criticism*), with the
corrections written out in §6 below so nothing load-bearing lives only in a criticism. Findings from
`bridging-models-ir-criticism.md` that survive independent of the IR proposal (the `dump-ir`/cache
purity argument, the bodyless-function trap, the attribution follow-up) are folded in where they
apply.

## 1. Design principles

- **All model matching is done on the VMT in the IR, irrespective of (and before) codegen.** This is
  deliberate and load-bearing: the VMT carries language-specific metadata (including Lua's
  *externals* column), so models keep working on functions with no IR body — Lua stdlib externals,
  Dex `native` methods, prototype-less binary functions. Any design that attached models to
  `FunctionData` would strand the entire shipped Lua default model set (IR criticism §2); matching on
  the VMT dodges that trap, and this sentence marks the dodge as deliberate.
- **One matcher implementation for `where` clauses.** There are permanently two matching pipelines —
  query's per-import source/sink matching and the new index-time streaming matcher — and both MUST
  share the evaluator, via the `ProgramMatchIndex` extraction (facts design §4.2). A second
  implementation of `where` is how `signature_match` ends up meaning two things in two places; both
  prior designs legislated against it and this plan adopts that rule by reference.
- **Matches are lifted into first-class concepts, not into IR.** We never generate or modify
  functions. Bridges and propagations are represented natively and codegen'd directly to facts. This
  kills the IR-synthesis hazard class (SSA shadowing, dead-temp elimination, phantom interned names)
  wholesale.
- **Streaming memory posture.** As IR streams through the import loop, we match and retain only
  *matches* (in `ProgramModelMatches`), not match indexes and not IR.

## 2. The core structure: `ProgramModelMatches`

As we stream IR, we populate one `ProgramModelMatches` shared across the streaming. It persists
across the import loop (it is **not** persisted to disk — see §8) and is codegen'd after all the
IR. It stores the exact information about matched models:

- **`propagations`** — propagation models, their associated access paths, etc. Codegen uses this
  directly to generate summary facts and record the relevant access paths. The port space is the
  full shipped one: `Index`, `Return`, `Global`, and `AnyArgument`, so codegen can generate the
  appropriate summaries (see §7.1 for why `AnyArgument` must be expanded in phase 2).

- **`access_paths`** — a registry of access paths that do *not* occur syntactically in the IR. This
  is a **human-declared** registry: it can hold composed paths a user knows, for example
  `.next.next.next`, three fields deep into a linked list. It is the escape hatch for deep
  composition across bridges (§6.3): a user who knows a callee's summary shape can pre-declare the
  needed residue paths by hand. The default behavior is stated plainly: **flows needing residue
  paths nobody declared are dropped, silently.** There is no automatic composition of bridge
  `from`/`to` paths — a composite like `.foo.bar` registered from a bridge `Argument(0).foo` →
  `Argument(3).bar` is the gate query for no derivation under the chosen emission and would burn a
  `facts.paths` row to enable nothing (facts criticism traced this through the rules; the composite
  belongs to the *rejected* structural-sharing alternative, not this one).

- **`bridges`** — a native representation of bridging models: how the caller's method's parameters
  (and subfields) should be shuttled into the parameters of a callee. Because this is its own field,
  we never turn bridging models into IR and back; we codegen them directly. Bridges are not
  optimized like real IR code; they faithfully represent the model the user chose. Each entry
  carries provenance `(file, generator index)` for diagnostics.

Matched/instantiated models are represented only here — nothing is stored in the VMT, nothing
modifies the IR. `ProgramModelMatches` is an instantiation of all the model specs, specialized to
the IR being indexed.

## 3. Matching: streaming, shared evaluator, scoping

### 3.1 Parse specs before the import loop

Bridge and model specs are scanned out of the `--models` files once, before the import loop
(facts design §4.1): parsing needs no program, hoisting avoids per-import duplicates, and indexing
knows up front whether any bridge exists. `BridgeSpec` / `SideSpec` / `ProgramScope` are as the
facts design specifies, with `ProgramScope` normalizing `language`/`languages` at parse time and the
mutual-exclusion and non-empty checks living there.

Loader hardening that lands with the parse (facts criticism §7):

- **Unknown keys are checked explicitly** at the generator level, the `model` level, and inside
  `bridge` — the JSON schema is editor-time only. Misspellings are hard errors with tests.
- The new parsers must **not** inherit the `.as_array().unwrap()` panic pattern that
  `super_model_generator` and `super_model` use today; non-array `where` etc. are errors, not
  panics.
- `find` is a constant (`methods`) on a generator carrying a `bridge`; any other value is a hard
  error pointing at the facts design §2.1 (callsite mode was removed and could not have worked).
- `Argument(*)` is rejected in a port map: a wildcard has no correspondent.

### 3.2 The shared evaluator: `ProgramMatchIndex`

Extract the match state out of `ModelGeneratorIngest` into an owned `ProgramMatchIndex` (facts
design §4.2), and make `ModelGeneratorIngest` borrow one instead of constructing its own. One
struct, one construction path, two users.

**Priced honestly** (facts criticism §5) — this is the largest single piece of loader work, not a
function call:

- Today the match state is fused into `ModelGeneratorIngest` together with `&mut ModelBuilders` and
  `endpoint_stats`; the only public entry points run the full visit and emit models as a side
  effect.
- The matched set is destroyed at the end of the visit; all maps are private; `matched_functions`
  takes a `&UniverseSet` no caller can obtain. Making match sets readable is new API surface.
- The maps borrow `&'p str` from the `ProgramInfo`. An owned index means re-pointing the evaluator
  at owned data — a lifetime refactor across every constraint visitor, not a new caller.

### 3.3 `in` scoping is on the critical path

The plan adopts the facts design's `in` syntax, and the streaming matcher must know which import it
is looking at — so the plumbing the facts criticism §5 priced (threading language/import identity
through the `try_load_models*` chain; `ProgramInfo` carries neither the import's language nor its
name, and `models/mod.rs:73-77` documents the deliberate non-threading) is **required work in this
plan**, not an optional refinement. Budget it as a signature change through the whole chain,
including distinguishing `dex` vs `apk` vs `jar`, which the VMT variant alone cannot.

One matching-semantics caveat to document: **Lua externals match by name only** — `has_code`,
`number_parameters`, and `uses_field` cannot match an external (no `FunctionData`), so a family
side-A constraint like `has_code: false` is unavailable there; `signature_match` is the supported
shape.

### 3.4 Streaming evaluation, and the reconciliation with conditional semantics

As each import streams through: build its `ProgramMatchIndex`, evaluate every applicable generator
— **including both sides of every bridge, eagerly** — record matches into `ProgramModelMatches`,
then drop the index and the IR.

This requires one sentence the first implementer otherwise discovers as a design contradiction
(overhauling criticism §3): the bridging rule "if the `from` side doesn't match anything, the `to`
side isn't even attempted" describes **reporting semantics, not evaluation order**. Under
streaming, side B's import may arrive before side A's, so whether `from` matched is unknowable when
B's VMT is in hand. Therefore:

- **Evaluation is eager**: both sides are matched per import as imports stream, and matches
  accumulate in `ProgramModelMatches`.
- **The conditionality is applied once, in phase 2** (after the stream ends): pairing,
  `on-unmatched` classification, and warning emission all happen there. "Isn't attempted" means: no
  `to`-side warning is reported when the `from` side is empty.

## 4. Bridging model semantics

Syntax is the facts design §3 (`in`, `model.bridge`, `to` as a match block, `arguments` in the
port-spec grammar, `direction: in|out|both`, no `convention` key), with the following decisions
replacing/refining its `cardinality`/`on-unmatched` story:

### 4.1 Pairing: full cross product, guarded by `on-ambiguous`

If `from` matches three methods and `to` matches two implementations, we take the **full cross
product** of matches. There is no `cardinality` key.

By default, any bridge match that isn't unique — i.e. not singleton × singleton — gets a warning.
This is `on-ambiguous: warn`, the default; the warning text tells the user they can add
`on-ambiguous: ignore` to silence it, and `on-ambiguous: error` is available when the user knows
ambiguity is bad. The name follows the document family's own distinction (*unmatched*: a side
matched nothing; *ambiguous*: matched, multiply) and names the condition, not the mechanism. The
warning includes the match counts on each side and the `(file, generator index)` provenance every
other loader message carries.

### 4.2 `on-unmatched`: per side, warn/warn, "matched anywhere"

- `on-unmatched: ignore|warn|error` is a key that is **independently** part of the `from` side and
  the `to` side of a bridging model.
- Default is `warn` on both sides.
- If the `from` side matches nothing, reporting for the `to` side is not attempted (see §3.4 —
  this is reporting semantics, applied in phase 2). So with `from: on-unmatched: ignore` and no
  `from` matches, the `to` side won't warn even if it also matched nothing.
- If the `from` side matches but the `to` side matches nothing, a warning is produced by default.
- "Unmatched" means **not matched anywhere across the whole project** — it is not calculated
  independently per import.
- A scope that admits no import in the project (an `in` naming no import present) counts as
  **unmatched**, so the default `warn` fires. We deliberately do not add a distinct
  configuration-error category; warn-by-default is loud enough, but only because the warning fires
  in this case — hence this sentence.

### 4.3 Globals

For bridging models, the globals pseudo-parameter (`GLOBALS_INDEX`) is mapped **unconditionally**
and is not user-visible. Without it, heap flows do not cross the boundary at all; `JniFlow` (taint
in through one native function, held in a native global, out through another) is the regression
that catches its absence.

### 4.4 `forward_call` is deleted; `forward_self` is kept

`forward_call` is the same-program special case of a bridging model — schema-only today, no loader
support — so delete it; folding it into `bridge` is a one-line desugaring and deletion costs
nothing.

`forward_self` is **not** a special case of a bridging model and stays (overhauling criticism §2).
It selects its target per *receiver class* — a correlation between the matched callsite's receiver
and the class hierarchy that a bridge's independent `to`-side `where` cannot express. Attempting to
emulate it with a cross-product bridge (`{from: execute*, to: doInBackground*}`) reproduces exactly
the known bug in the hardcoded `models/codegen.rs` rule (every `AsyncTask.execute` forwarded to
*every* `doInBackground` in the program) — do not canonize it. If a receiver-correlated bridge
pairing mode is ever wanted, that is real design work to be specified separately, not a deletion.

### 4.5 Interaction with the built-in JNI pass

A user bridge over a pair the built-in JNI pass also links double-bridges it: two sites, duplicated
flows. `--no-jni-bridge` is the answer, and the docs for `bridge` say so explicitly (and it is also
what enables the A/B regression in §10).

## 5. Composition limits, stated plainly

The bridge emission (§6) keeps every path **literal**: `from_path` on the caller's formal,
`to_path` on the temporary. The temporary wins decisively on aliasing (ports sharing a callee index
stay distinct), but composition past the seam is **exact-match only** (facts criticism §1): a
pathful bridge composes with the callee's *derived* summary only where the summary's endpoints land
exactly on the port's `to_path` (or extend it by a suffix that is itself an input model path).
Consequences, all deliberate:

- Flows needing residue paths (`from_path.q` / `to_path.q` for residues `q` the callee's derived
  summary produces) that nobody declared are **dropped silently**. The `access_paths` registry
  (§2) is the escape hatch: a user who knows the callee's summary shape pre-declares the residues.
- The Lua-shape example (ports at `Argument(0).stack.[1]` etc.) carries a precondition: the
  callee's behavior must also be modelled (by hand-written `propagation` summaries) in exactly the
  port map's vocabulary at exactly the port map's paths, or the bridge delivers taint that nothing
  reads. Document the precondition wherever that example appears.
- If deep callee-side paths ever matter broadly, the live fallback is the **demand-driven
  fixpoint rule** (a bridge-aware rule that head-inserts needed `paths` rows on demand) — the one
  approach for which the path family is enumerable because it is demand-driven. Recorded here so it
  is revisited as a design extension, not rediscovered as a bug.

There is **no** explicit `facts.paths` push anywhere in a bridge: `program_paths` is seeded from
both endpoints of every `assign` and every `actual_param` vertex, so the bridge's own literal paths
register themselves. (The previous draft's "bridging model paths are individually added to the
indexer paths" described a no-op and is gone.)

## 6. Bridge emission semantics (facts design §2 and §4.4, as corrected)

Adopted by reference and summarized here with the facts criticism's corrections applied, so the
emission section is self-contained:

1. **The port map is the feature, not a refinement.** A bare `call` edge silently mis-wires any
   ABI-shifted pair (`JniArgShift` is the pinning regression).
2. **Ports route through a temporary.** One temporary per distinct *callee* index in the port map,
   standing for that parameter as the callee sees it. The bridge assigns between the caller's port
   and a sub-path of the temporary, and passes the temporary whole via `actual_param`. Everything
   downstream is ordinary summary instantiation; no rule knows a bridge was involved.
3. **Sites are fresh; temporaries are keyed `(site, index)`.** Never reuse an existing site id
   (call-arg pseudo-variables are keyed on it; reuse aliases unrelated arguments). Under this
   plan's cross-product pairing, several bridge sites routinely share one caller — the exact
   scenario the `(site, index)` keying exists for.
4. **Formals are synthesized on both sides**, for every mapped port, trusting the model over the
   recovered prototype. Side B's `formal_param` rows are a **cross-function emission**: the phase-2
   codegen walking side A's bridge entries must be told to produce rows against `b` as well as `a`.
   An arity mismatch on `b` is a *warning* naming both functions and both arities — never a dropped
   fact. Side effect to note in code: `formal_param` rows past `b`'s real arity feed
   `compute_num_params`/`compute_arg_arity`, so a later `Argument(*)` source/sink model on `b` will
   expand over phantom parameters — accepted as noise, documented at the emission site.
5. **Return and globals are ports like any other; return arity is asymmetric.** A Java function's
   exception return (`-2`) is deliberately unmapped; `Return` means the normal return (`-1`). The
   globals pair is emitted unconditionally (§4.3).

The emission per pair `(a, b)`, with the shape corrections:

- `let site = source_info.add_insn_site(a);` — fresh site, then `facts.call.push((site.into(), b))`.
- Per distinct callee index `n`: a temporary `t_n` keyed on `(site, n)`;
  `facts.actual_param.push((site.into(), n, FlowVertex(t_n, empty)))`;
  `facts.formal_param.push((b, formal_index(n), ByRef))`.
- Per port: assigns between `FlowVertex(formal(from.index), from.path)` and
  `FlowVertex(t_{to.index}, to.path)`. **`assign` rows are keyed on the `site`
  (`PackedInsnSiteId`), not on the function id** — the in-memory relation is
  `(PackedInsnSiteId, FlowVertex, FlowVertex)`; the function-keyed shape is the persisted parquet
  form only (facts criticism §4). `direction` is exactly which of the two assigns get pushed
  (`in` = into the temporary, `out` = converse, `both` = both).
  Also `facts.formal_param.push((a, formal(from.index), ByRef))`.
- **Degenerate collapse**: a port with empty `to_path` and `direction: both` (every JNI port) is
  special-cased to emit the direct `actual_param(site, n, FlowVertex(formal_k, from_path))` row —
  keeping the shipped JNI fact shape byte-identical, which `languages/jni/tests.rs` asserts on.
- No `facts.paths` push (§5). Temporaries need no `formal_param` row.
- **Source attribution**: the synthetic site has no `source_map` entry; the SARIF step emitter
  returns early for it (no panic, no bogus location). Attributing the crossing to the stub's own
  declaration span *where one exists* is a scoped follow-up (IR criticism §6 agreed the fact-level
  fix is available), not part of this change.

## 7. Codegen: two phases

Codegen is split in two. Phase 1 is what codegen does today. Phase 2 runs after **all** the IR is
codegen'd and populates facts for all the models out of `ProgramModelMatches`:

### 7.1 Propagations

`ProgramModelMatches::propagations` populates `model_paths` and the initial `summaries` — access
paths from propagation models get all the way into models, preserving the model-path bucket
discipline (summary paths seed `model_paths` and concatenate with every program path).

Phase 2 must **replicate the expansion details of today's `codegen_summary`** (IR criticism §3, by
way of the overhauling criticism §5): the `AnyArgument` tag expansion, the `dst == src` skip, and
the `formal_param` pushes for both ports. Placing `AnyArgument` expansion in phase 2 is not just
necessary but an improvement: expansion consults `compute_arg_arity` over actual parameters and
call sites across *all* imports, where today's per-import expansion under-expands a function whose
call sites span imports.

### 7.2 Access paths

The `access_paths` registry is included in the initial indexer paths. That is its entire mechanism;
its semantics (user-declared residues; dropped-by-default otherwise) are in §5.

### 7.3 Bridges

Bridge entries are codegen'd directly into facts per §6: a fresh callsite inside the `from`
function; each from/to pairing read from the formal parameter into the `(site, index)` temporary;
the temporary passed as the actual to the call site of the callee; formals synthesized on both
sides; globals unconditional.

Phase 2 also applies the reporting semantics deferred from streaming (§3.4): pairing (cross
product), `on-ambiguous` and `on-unmatched` classification, and warnings.

### 7.4 Diagnostics — unconditional

Phase 2 logs **matched/paired counts per generator, unconditionally, at `info`**. This is the
mitigation that actually gets read (the JNI `LinkStats` experience): a bridge-only generator
appears on no other surface — no endpoint stats, no `CTADL0004` — so this line is load-bearing,
not cosmetic. It covers the *mis-paired* case (wrong slot, wrong path, wrong function matched)
that warn-on-empty cannot.

## 8. Query interaction, persistence, memory

- **Query keeps its own source/sink matching.** The query continues to take a `--models` flag; it
  issues a single warning that query ignores propagation/bridging models if any were found in its
  input. Both pipelines share the evaluator (§1, §3.2).
- **The index-side twin of that warning**: source/sink models passed to `ctadl index` are currently
  discarded in total silence. Emit one warning. Symmetry costs one line and closes the
  silent-inertness family.
- **Matches are in-memory only.** The rationale (corrected from the earlier draft): matches are a
  function of (artifact × models files), and the import cache must stay a pure function of the
  artifact — persisting model-dependent state would let `ctadl index --models a.json` poison the
  next `ctadl index --models b.json`. `ProgramModelMatches` persists *across the import loop*
  only; nothing is written to disk, nothing is stored in the VMT, and the IR is never modified.
- **Memory is measured, not assumed.** Streaming matching retains only matches, not match indexes
  — better than the "match indexes resident" posture the prior designs accepted. Still: add a
  footprint checkpoint after the import loop and quote a real number for an APK + `.so` before
  calling it settled.

## 9. Schema and docs

Following the facts design §5, adjusted for this plan's decisions:

- `$defs/program-scope` as specified there (`language` xor `languages`, `import`,
  `additionalProperties: false`).
- `$defs/port-map` as specified there.
- `$defs/bridge-model`: `to` (required), `arguments`, `on-unmatched` (on the `to` block; the
  generator-level `on-unmatched` covers the `from` side), `on-ambiguous`;
  `additionalProperties: false`.
- `model.properties` gains `bridge`; the generator object gains `in` and `on-unmatched`.
- **`forward_call` is removed** from the schema and docs. **`forward_self` remains**, documented as
  the genuinely separate, receiver-correlated construct (§4.4).
- `docs/model-generators.md`: a `bridge` subsection, a summary-table row, the index-time-only
  scoping note (bridges and propagations are inert at query time; sources/sinks are inert at index
  time — now warned, §8), the `--no-jni-bridge` double-bridging note (§4.5), the Lua
  externals name-only matching caveat (§3.3), and the composition-limits statement with the
  `access_paths` escape hatch (§5). `docs/jni.md` cross-references `bridge` for the code the
  built-in pass cannot reach, and notes that the per-method `jni bridge:` resolution line requires
  `-v` (it logs at debug).

## 10. Verification

The facts design §8 applies, adapted:

- **Parse/validate, no program needed**: unknown keys at all three levels; missing `to`;
  `Argument(*)` in a port map; `find: callsites` with a `bridge` as a hard error; `in`
  mutual-exclusion/empty/unknown-language cases; `{"language":"dex"}` ≡ `{"languages":["dex"]}`;
  non-array `where` is an error, not a panic; each `on-unmatched`/`on-ambiguous` setting.
- **Matching**: two-`ProgramMatchIndex` fixture asserting side-A/side-B sets and resulting pairs.
  Add a **streaming-order** case: the `to` side's import arrives before the `from` side's, and the
  pairing/warnings come out identical (§3.4's eager-evaluate/late-classify contract).
- **Reporting semantics**: `from` empty + `from: ignore` ⇒ no `to`-side warning; `from` matched +
  `to` empty ⇒ warning; ambiguous (2×1, 1×2, 2×3) ⇒ `on-ambiguous` warning with counts and
  provenance; scope-admits-no-import ⇒ unmatched warning.
- **Emission**: exact `call`/`actual_param`/`formal_param`/`assign` rows for a pair + port map,
  including the implicit globals pair, fresh site, both-side formals including ports past the
  callee's recovered arity (plus the arity warning), site-keyed assign shape, and **zero
  `facts.paths` rows**. Temporaries: shared-callee-index ports share one temporary and don't alias
  (the Lua map is the fixture; the negative is taint on `Argument(0)` must not reach
  `Argument(1)`); `direction: in` emits one assign and not its converse; two bridge sites in one
  caller (cross-product pairing) get distinct temporaries. A JNI-shape case asserts the degenerate
  collapse leaves `jni::emit_bridge`'s rows byte-identical.
- **Phase-2 propagations**: `AnyArgument` expansion over call sites spanning two imports (the case
  today's per-import expansion under-expands); `dst == src` skip; `formal_param` pushes; model
  paths land in `model_paths`, not `program_paths`.
- **Diagnostics**: the per-generator count line is present unconditionally; the query-side and
  index-side ignore warnings each fire exactly once.
- **End-to-end, two flowy imports**, with deliberately different function names (same-named
  functions already unify, which would fake a pass), positive flow plus a negative with the model
  removed.
- **End-to-end, two real frontends**: reuse `cargo xtask regression --frontend jni` with
  `--no-jni-bridge` plus a declarative bridge doing the join by hand — a direct A/B against the
  built-in pass. Shape cases so no per-function model could fake them (`JniFlow`, `JniArgShift`).
- **Regression**: `cargo test --workspace`, `cargo xtask regression` across frontends, and the
  `[mem cp]` footprint number from §8.

## 11. Sequencing

Ordered so each step has its own tests and the risky refactors come before the features that need
them:

1. **Loader groundwork**: parse bridge specs and `in` before the import loop; explicit key checks;
   panic-pattern fixes; `forward_call` removal from schema/docs. (No program needed; unit tests.)
2. **`ProgramMatchIndex` extraction** — the big loader refactor (§3.2), landed with
   `every_shipped_default_file_parses` and the full default-models suite as the guard.
3. **`in` plumbing** — thread import identity/language through `try_load_models*` (§3.3).
4. **Streaming matching** into `ProgramModelMatches` (§3.4), with the streaming-order test.
5. **Phase-2 codegen**: propagations (with `codegen_summary` parity tests), `access_paths`, then
   bridge emission (§6) with the emission/temporary test batteries.
6. **Reporting semantics + diagnostics** (§4.1–§4.2, §7.4, §8's two warnings).
7. **End-to-end**: flowy two-import, then the JNI A/B regression.
8. **Docs** (§9) and the memory number (§8).

## 12. Decision log

Recorded so resolutions read as deliberate and don't get "simplified" back out:

- Matching on the VMT (not `FunctionData`): keeps models on bodyless functions — the IR
  criticism's Lua-externals trap, dodged on purpose.
- Query keeps source/sink matching (iterate on queries without reindexing); the two-pipeline risk
  is retired by the shared evaluator, adopted as an architecture item, not just a syntax reference.
- In-memory only, with the cache-purity rationale (not the earlier drafts' "IR is modified" — the
  IR is never modified).
- Eager evaluation + phase-2 classification is the streaming reconciliation of the conditional
  `to`-side semantics.
- Full cross product pairing + `on-ambiguous: warn` default; `on-unmatched` per side, warn/warn.
- No automatic path composition; `access_paths` is the human-declared escape hatch; dropped
  residues are the documented default; the demand-driven fixpoint rule is the recorded fallback.
- `forward_call` deleted; `forward_self` kept — it is receiver-correlated and a cross-product
  bridge emulation would canonize a known bug.
- `AnyArgument` expansion in phase 2 is a strict improvement over today's per-import expansion.
