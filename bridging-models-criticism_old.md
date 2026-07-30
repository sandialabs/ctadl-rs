# Criticism of `bridging-models-design.md` - DO-NOT-MERGE

Findings from reading the design against the code it cites. Ordered by severity: §1–§3 are
defects in the design as specified, §4–§6 are places where the argument overclaims or underprices,
§7 collects smaller points, §8 records what was checked and holds, §9 concludes. Every claim below
was verified against the tree at the cited locations.

## 1. Composition past the seam is exact-match only; the flagship Lua example does not work as written

The design's centerpiece — the temporary of §2 consequence 2 — is mechanically sound (§8 below),
but it solves path *registration* while leaving path *composition* gated, and the gate is stricter
than §7's "may not clear the propagation gate" suggests. Traced through the actual rules:

Every path-extending propagation step is gated on membership in `paths`
(`index_engine/mod.rs:1063-1072`), and `paths` is exactly: `program_paths` ∪ `model_paths` ∪ their
**one-level** concatenations (`mod.rs:1049-1054`). `model_paths` is seeded *only* from the input
`facts.summary` (`mod.rs:920-924`, with a comment explaining why nothing else may be folded in);
derived summary paths never enter either bucket. Consequences for a bridge with non-empty
`to_path`:

- **Callee summary path deeper than the port** (`to_path.f`): the flow survives to the seam and
  needs `paths(from_path.f)`. `from_path` is a program path, so the only concat that produces it is
  program × model with the model operand *literally* `.f` — i.e. some input summary must carry the
  bare suffix as its own path. A program-path `.f` does not help: there is no program × program
  rule. Otherwise the flow drops silently.
- **Callee summary path shallower than the port** (e.g. `summary(b, Arg0, .g, Arg0, ∅)` — a pcode
  function that passes the state pointer around whole and writes one field of it): the derived
  caller path is `dst_path ++ to_path`, whose left operand is a derived-summary path in *neither*
  bucket. Not manufacturable. Dropped.
- **Exact match** (`src_path == to_path`, dst residue empty): works, because the surviving path is
  a literal endpoint of the bridge's own `assign` and so a program path (`mod.rs:852-857`).

So a pathful bridge composes with its callee's summary only where the summary's endpoints land
*exactly* on the port's `to_path` (or extend it by a suffix that is itself an input model path).
The design's §7 presents this as one open question about a `.f` corner; it is the general shape of
every non-empty `to_path`, in both directions.

Now apply that to §3.3's Lua example, "the case to build first". Its port paths mix a **symbolic**
field (`.stack`) with offsets (`.[1]`, `.[-1]`). The pcode frontend derives offset-only paths —
that is the design's own §2.3 distinction — so the *derived* summary of `l_add` can never mention
`.stack.[1]` literally, and `[-1]` (Lua's top-of-stack) has no static byte offset at all. The
example can only produce flow if `l_add`'s behavior is *also modelled* by hand-written `propagation`
summaries in exactly the port map's vocabulary, at exactly the port map's paths. That precondition
appears nowhere in the document, and a reader will assume the analysis of the native side supplies
the composition. Under the precondition, the exact-match case fires and the example works; without
it, the bridge delivers taint to `t.stack.[1]` and nothing ever reads it — the design's own
dominant failure mode, in its flagship example.

Two revisions follow. First, §2 consequence 2's "Nothing is ever concatenated … That is the
decisive advantage" needs demoting: the temporary genuinely wins on aliasing (ports sharing a
callee index stay distinct — that part stands) and on registration of the bridge's own literal
paths, but the rejected alternative's "open family `from_path ++ to_path ++ q`" is not escaped —
the chosen design silently *drops* exactly the flows that family would have admitted. Both designs
express only `q = ∅`; the difference is which failure you get. Second, the §9 rejection of "a new
relation and inference rule" deserves a footnote: a bridge-aware rule inside the fixpoint could
derive the needed `paths` rows *on demand* (rules can head-insert into `paths`), which is the one
approach for which the family is enumerable because it is demand-driven. If deep callee-side paths
ever matter, that alternative comes back.

## 2. Pairing is undefined; `cardinality` is a constraint on a function the design never specifies

§4.3 says the two result sets "are then paired per `cardinality`". Cardinality names how many B's
each A may bind (§3.2) — it is a *check* on a pairing, not a pairing. When both sides match one
function each, there is nothing to decide. In every other case the design is silent on which A
pairs with which B: cross product? error on ambiguity? and under `one-to-one` with |A| = |B| = 3,
which bijection?

This is not a pedantic gap, because the design's own text depends on the multi-match case.
`on-unmatched: "ignore"` exists for "a bridge written against a family of optional symbols — most
matched stubs will have no implementation present" (§3.2). A family bridge has many A's and a
subset of B's present, and *nothing in the syntax correlates them*: one shared `arguments` map and
two independent `where` blocks. The construct that used to carry the correlation was `convention` —
the plan's `bare jni` variant explicitly supplied "a *pairing rule* — derive the JNI symbol from
the Dex method id — so `to.where` can be omitted entirely and the two sides pair by derived name
rather than by cross product" (`bridging-models-plan.md:167-172`). §3.2 dropped `convention` for
sound reasons (§2.2: the expansion is not a fixed shift) but never noticed it did double duty, and
removed the only pairing mechanism with it.

Meanwhile §2.2's own argument — a hand-written `arguments` list is inherently per-method — implies
every *sound* bridge generator is singleton × singleton. Taken together: the many-valued
cardinalities are either dead weight (unreachable by any correct model file) or a trap (a family
bridge whose pairs are wired by cross product). The design should either specify pairing (and
justify a use for many-to-many), or restrict cardinality to what a single shared port map can
support and say the family case is a `languages/` pass by construction.

## 3. `on-unmatched` conflates the two sides, and its supporting precedent is misstated

One knob governs both match sets. The family case needs `ignore` because *side B* may be legitimately
absent; but `ignore` also silences an empty *side A* — a typo'd `where`, a scope admitting no import
— which is precisely the "indistinguishable from a clean app" failure §7 declares dominant. The two
sides have different failure semantics: an empty A means the model is wrong; an empty B may mean the
optional library is absent. A per-side setting, or at minimum distinguishing "matched nothing" from
"matched but unpaired", would preserve the fail-loud default where it is diagnostic. As specified,
every family bridge author disables the design's principal safety mechanism on both sides at once.

The justifying sentence is also wrong on the facts: "Erroring by default matches the loader's
existing policy on unusable constraints" conflates two policies. The loader hard-errors on
*malformed* constraints (`check_constraint_keys`, `json.rs:428-470`); its policy on *empty matches*
is a query-time `warning`-level SARIF notification for sources/sinks only (`CTADL0004`,
`query_engine/formatter.rs:294-347`, level at `:332`) and **total silence** for propagations —
`endpoint_stats` is populated only by `visit_source`/`visit_sink` (`json.rs:1655`, `:1744`) and
`cli::index` never reads it at all. `on-unmatched: "error"` is a defensible *change* of posture; it
is not a continuation of one.

## 4. The emission sketch pushes `assign` rows of a shape the fact base does not have

§4.4 writes `facts.assign.push((a, callee, caller))` with `a` a function id. The in-memory relation
is `Vec<AssignFlow>` = `(PackedInsnSiteId, FlowVertex, FlowVertex)` (`index_engine/mod.rs:60, :77`)
— keyed on a packed **instruction site**, from which the function is derived (`mod.rs:903-910`).
The function-keyed shape the sketch uses is the *persisted parquet* form
(`facts/schema.rs:63-69`), which is presumably where the confusion came from. The fix is easy —
key the bridge's assigns on the same `site` the call uses, which packs `a` — but as written the
sketch's central three lines do not type-check against the relation they cite, in the section whose
job is to be the precise one. (The rest of the sketch's shapes are right: `call` is
`(PackedInsnSiteId, FunctionId)` (`mod.rs:74`), `actual_param` is site-keyed (`mod.rs:71`),
`formal_param` carries the `FormalType` mode column (`mod.rs:68`, `facts.rs:1273-1278`), and
`add_insn_site` exists with exactly the claimed freshness property
(`index_engine/source_info.rs:52-56`).)

One un-flagged side effect of consequence 4's "push rows for every mapped port regardless":
`formal_param` rows past the callee's real arity feed `compute_num_params`/`compute_arg_arity`
(`mod.rs:258`), which expands `Argument(*)` ports at query time (`query_engine/endpoints.rs:57`).
A phantom formal on `b` makes a later `Argument(*)` source/sink model on `b` emit endpoints for
parameters that do not exist. Probably harmless noise, but "a duplicate row is free" is argued only
for duplicates, and these are not duplicates.

## 5. "Reuse the existing evaluator" is a refactor priced as a function call

§4.2–§4.3 present the reuse as: extract a `ProgramMatchIndex`, "build a synthetic one-generator
value and run it". What the code allows today is neither:

- The match state is fused into `ModelGeneratorIngest` (`json.rs:39-88`) together with the
  `&mut ModelBuilders` and `endpoint_stats`; the only public entry points run the full visit,
  which *emits* models as a side effect.
- The matched set is **destroyed at the end of the visit** (`json.rs:881`,
  `self.methods[n] = UniverseSet::empty();`), all four maps and the universe are private, and
  `matched_functions` (`json.rs:2135`) takes a `&UniverseSet` no caller can obtain. There is no way
  to read a match set out; the test suite observes matches by attaching a source model and reading
  endpoint rows (`tests/json_error_handling.rs:310-328`).
- The maps borrow `&'p str` from the `ProgramInfo`. An owned `ProgramMatchIndex` (which §4.2
  correctly requires) means the evaluator must be re-pointed at owned data — a lifetime refactor
  across every constraint visitor, not a new caller.

None of this is an argument against the approach — "one struct, one construction path, two users"
is the right end state, and the plan's Step 2 scoped it honestly. But the design compresses the
largest single piece of loader work into a sentence, while spending paragraphs on cheaper things.
The same underpricing applies to `in`: `ProgramInfo` carries neither the import's language nor its
name (`ctadl-ir/src/mir/mod.rs:703-711`), the loader API takes only `&ProgramInfo`
(`models/mod.rs:105-208`), and `models/mod.rs:73-77` documents that the language is deliberately
*not* threaded down. Enforcing `in` — including `dex` vs `apk` vs `jar`, which the VMT variant
cannot distinguish — requires a signature change through the whole `try_load_models*` chain. The
design never mentions that scoping ordinary generators is plumbing work at all.

## 6. §4.3's ordering rationale still conflates matching with emission

"Evaluating per import cannot work — the second program's functions do not exist yet, and the
failure mode is a silent skip." The second clause describes `codegen_summary`'s lookup-and-skip; but
emission does not need lookup. `get_or_add_function` interns names on demand, and the sibling
criticism (`bridging-models-ir-criticism.md` §4) already established that fact-level emission
naming side B works per-import — what genuinely forces the post-loop point is that *matching* side
B's `where` needs side B's match index, which may belong to a later import. The revision imports
that criticism's conclusions elsewhere (§2.1's removal of callsite mode) but keeps the conflated
sentence here. The placement is still fine — matching alone justifies it — but the stated reason is
the wrong one, and it matters because the wrong reason forecloses a real option: per-import
emission with hoisted matching, which would let a bridge's rows participate in anything else that
happens inside the loop and would shrink `apply_bridges` to pure matching plus pairing.

## 7. Smaller points

- **"Unknown-field checking covers constraints and ports" — half right.** Constraints, yes
  (`json.rs:428-470`). Ports, no: `visit_source`/`visit_sink` never call `check_constraint_keys`,
  and `field`/`fields`/`all_fields` — declared in the schema and documented — are read nowhere in
  `json.rs`; `saturating` on a `propagation` is likewise silently dropped. The status quo is worse
  than §4.1 states, which strengthens its call for explicit key checks. Adjacent hazard for the
  implementer: `super_model_generator` does `.as_array().unwrap()` on `where` (`json.rs:1895`), and
  `super_model` the same on its three keys (`json.rs:2085-2099`) — a non-array **panics** rather
  than erroring, and `bridge`'s parser should not inherit the pattern.
- **The `AsyncTask` hack is not cross-language.** §6 calls it "the one hand-written, hardcoded
  cross-language rule in the tree"; `models/codegen.rs:9-33` is Java-only, same-program, and
  `forward_self`-shaped (as §6 itself then concedes). The actual hand-written cross-language linker
  in the tree is the JNI pass the design's §0 celebrates. Also worth knowing before re-expressing
  it declaratively: the rule binds `execute_cls` and never joins it against `doInBackground`'s
  class, so it forwards every `AsyncTask.execute` to *every* `doInBackground` in the program — do
  not preserve the bug in the translation.
- **The `jni bridge:` log line users are told to check (§3.3) is at debug level.** Per-method
  resolution logs at `log::debug!` (`jni.rs:391`, `:406-410`); the default index log carries only
  the one-line `LinkStats` summary at info (`jni.rs:469`). The advice "check the `jni bridge:` line"
  works only under `-v`; either the doc should say so or the resolution line should be promoted.
- **`program_paths` seeding is stated incompletely.** Besides `assign` endpoints and
  `actual_param` vertices (§2 consequence 2), it is seeded from `callee_info` receiver paths
  (`mod.rs:1048`) and from codegen's explicit `facts.paths` (`mod.rs:862`). Harmless for the
  argument, but the design twice asserts the seeding set as if exhaustive.
- **"The two rules meet only at the argument index `n`"** (§2) — the summary-instantiation rule
  joins only on the *callee* (`mod.rs:1096-1103`); the indices are used to mint `call_arg`
  pseudo-variables, with no premise that an actual exists there. The meeting point is the minted
  `(insn, n)` pseudo-variable, not a join on `n`. The design's inference survives; the sentence
  doesn't.
- **A bridge-only generator is invisible to the existing diagnostics — both ways.** §4.1's "must
  not be counted in endpoint statistics" is satisfied for free (`endpoint_stats` is populated only
  by source/sink visits), but the flip side goes unsaid: no entry also means no `CTADL0004`, so the
  `BridgeReport` line is the *only* surface a bridge ever appears on. That raises the stakes on
  §4.3's report being unconditionally at info from "following LinkStats" to load-bearing.
- **Query-time silence is broader than §6 admits.** Not only are `propagation` and `bridge` inert
  at query time; source/sink models passed to `ctadl index` are also silently discarded
  (`cli::index` consumes only `batch.summary`, `cli/mod.rs:119`, and never reads
  `endpoint_stats`). The "deliberate exception to the fail-loud policy" is already a family of
  three, and the docs' current statement of it is soft (a recommendation in
  `docs/model-generators.md:46-49`; the only explicit sentence lives in `docs/jni.md:167-169`).
- **Lua externals match by name only.** `has_code`, `number_parameters` and `uses_field` cannot
  match an external (`json.rs:325-327` — no `FunctionData`). §3.3's example uses
  `signature_match`, so it is fine, but §2.1's "already appears as a matchable, bodyless callee"
  should carry the caveat, since a natural family-side-A constraint (`has_code: false`) is exactly
  one of the unavailable ones.
- **Wording nit in §3.1:** "Omitting `in` means every program, as does an `in` naming no language
  key" literally says `{"import": "app_dex"}` scopes to every program. The intended meaning (the
  language *dimension* is unconstrained) is contradicted by the sentence after it.
- **Double-bridging with the built-in pass is unstated.** A user bridge over a pair the JNI pass
  also links yields two sites and duplicated flows. §3.3 tells the user not to write one;
  nothing says what happens if they do. The IR counterproposal at least names `--no-jni-bridge` as
  the answer (`bridging-models-ir-design.md` §3.5); this document should too, or the loader should
  detect the overlap.

## 8. What was checked and holds

Recorded because the sibling criticism's process note asked for an inventory, and this revision
largely delivered one. Verified true against the tree:

- **The §0 status table, in full**: `emit_bridge` emits `call` + per-port `actual_param` +
  caller-side `formal_param` only, fresh site, unconditional globals and return pairs, arity
  warning naming both functions and both arities (`jni.rs:520-549`, `:447-463`); callee-side
  formals genuinely pending (no `native_id` formal anywhere, and the test fixture pre-seeds them);
  `port_map(descriptor, is_static, slots)` with the two slot models and receiver-at-0 in both
  (`jni.rs:58-82`, `:197-219`); `--no-jni-bridge`; `INDEX_FORMAT_VERSION` 3 with the `import_id`
  column; the two-import `xtask` regression runner; `JniFlow` and `JniArgShift` testing exactly
  what §2 claims of them.
- **The fact-level model of §2**: `actual_param` expands bidirectionally and unconditionally
  (`mod.rs:1088-1094`); the call-arg pseudo-variable is keyed on instruction id, so the fresh-site
  rule (consequence 3) is exactly right; `call` is EDB — it appears in no rule head, and dispatch
  resolution adds `assign_like`/`context_assign` rows, never `call` rows — so §2.1's argument for
  removing callsite mode is sound as well as well-written.
- **The load-bearing mechanism claim**: there is *no* variable-universe premise anywhere in the
  propagation rules — a fresh temporary with no `formal_param` row propagates through `assign_like`
  and the whole-variable `actual_param` binding transfers every registered sub-path
  (`mod.rs:1056-1072`; `substitute_prefix` at `facts.rs:216-218`). "Temporaries need no
  `formal_param` row" is correct, and the places that *do* require formals (summary derivation,
  `mod.rs:1106-1113`; the query engines' call steps) are exactly the ones consequence 4 emits rows
  for. The design's §2 is, within the exact-match regime of §1 above, a faithful description of the
  engine.
- **The premises of §3.1**: no scoping mechanism exists today, model files are re-matched against
  every import, and `models/mod.rs:73-77` documents the deliberate non-threading of the language —
  the design's motivation for `in` is real, even though §5 above shows its cost is understated.
- **The SARIF early return** for a location-less site (`formatter.rs:2270-2281`), the one-level
  model × program concat with the |paths|² warning comment, and `load_and_map_summaries` as
  existing post-loop precedent (`cli/mod.rs:612-687`) — all as described.

## 9. Conclusions

1. **The mechanism is validated; the composition semantics are not what the document implies.**
   Everything the JNI pass exercised — empty-path ports, both directions, fresh sites, formals —
   checks out at the rule level. What no shipped code exercises, non-empty `to_path`, composes with
   the callee only at exact path equality, and the flagship Lua example additionally requires the
   callee to be hand-modelled in the port map's vocabulary. The design should state that
   precondition, weaken consequence 2's "decisive advantage" to the aliasing claim (which stands),
   and treat §9's rejected fixpoint-rule alternative as the live fallback for deep paths rather
   than a curiosity. "Build the Lua case first" was the right instinct; §1 says it is not optional.
2. **The multi-match story needs a decision, not defaults.** Specify the pairing function or
   restrict `cardinality` to singleton-compatible values; split `on-unmatched` by side or by
   unmatched-vs-unpaired; and reconcile the family use case with §2.2's own conclusion that shared
   `arguments` maps cannot serve families. As written, the three keys interact to make the safest
   configuration unusable for the case they were added for, and the usable configuration unsafe.
3. **Fix the mechanical debts before implementation starts**: the `assign` shape in §4.4 (site-
   keyed, not function-keyed); the evaluator extraction and `in` plumbing priced as real work
   items; the §4.3 rationale restated as a matching constraint; the phantom-formal effect on
   `Argument(*)` at least mentioned.
4. **Process note.** This revision does what the sibling criticism asked of the IR document — its
   §0 pins what shipped, and nearly every checkable claim survives checking, including several
   (slot models, return arity, freshness, EDB-ness of `call`) that would have been easy to get
   subtly wrong. The two real defects live precisely in the parts no shipped code validates
   (pathful ports, multi-match bridges), which the document itself half-knows — §7 flags both areas
   — but half-knowing shows up as "unvalidated, test first" where the evidence now says "does not
   compose / is not specified". The next revision's job is to promote those two from risk
   paragraphs to design sections.
