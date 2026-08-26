# Criticism of android-intent-design.md - DO-NOT-MERGE

A review of the design against the code it cites, re-run after the revision that took the intent
API out of the model file: static ports are `summary` rows the linking pass emits, extras edges are
per-site `assign` rows rooted at the real argument variables, the IR rewrite is gone entirely, and
one optional in-fixpoint rule mints `.<extras>.<k>` from a key only `const_reaches` recovers. Items
the revision closed were removed or moved into the section below; everything numbered is open.

## What the revision closed

- **The data-file / pass seam** (previously item 1). `.<intent>`, `.<action>`, `.<component>`,
  `.<extras>` are now written and read by one pass from one constant. The failure modes that
  motivated the item -- `--no-default-models`, an `in` block scoping the entries to a language that
  does not match, a user file shadowing the defaults -- cannot silently empty the intent surface
  any more, because nothing about it depends on a file being loaded.
- **The IR rewrite, and its whole risk surface.** Checked, and the revision's central claim holds:
  everything the rewrite read off the statement is in `IndexFacts` by the time a link pass runs.
  Codegen emits an `actual_param` row per argument and one per return value at a negative index
  (`codegen/mod.rs:647-707`), so the receiver, the value and the return are all nameable as real IR
  variables; and a constant argument, which leaves *no* `actual_param` row (`trans_exp` returning
  `None` skips the push, `:690-692`), is exactly what Phase 2's third `const_str_assign` hook
  supplies. So the pipeline slot between `ssa::transform_program` and `ssa::propagate_copies`, SSA
  preservation through `rets = [retval, throwval]`, `store_access_path` / `cap_path` re-anchoring,
  and the copy-propagation ordering all stop being things that can go wrong. Old open decision 4 --
  a user model on `getStringExtra` silently ceasing to fire at rewritten sites -- goes with them:
  no site loses its `call` row.
- **The `Bundle` level bug**, which the previous revision shipped without noticing. Walked through
  the documented port semantics (`docs/model-generators.md:567-590`): with bundle entries at `.<k>`,
  `putExtras: Argument(1) -> Argument(0).<extras>` lands them at the intent's `.<extras>.<k>` and
  `getExtras: Argument(0).<extras> -> Return` unwraps them back to `Return.<k>`. The old table's
  `recv.<extras>.<k>` for `Bundle.put*` really would have produced `.<extras>.<extras>.<k>` and
  never met a reader. Item 7 below is about the convention, not the arithmetic.
- **Half of the "nothing verified the model entries fire" item.** The *matching* half is now a
  scan the pass can count, and the design commits to counting it. The *instantiation* half is
  untouched -- see item 2.

## 1. The scan gives up class pinning for the whole table to solve a two-row problem

The design says matching on name plus descriptor rather than on an owning class "is still required,
and for the unchanged reason". That reason is real for exactly two rows. `getIntent` / `setIntent`
are called on the app's own subclass, so the dex method id at the call site never says
`Landroid/app/Activity;`. Every other row in the table is a method *on `Intent` itself*, and a call
to it names `Landroid/content/Intent;` in the id, because that is the declared type of the
receiver -- which is why the existing `java-index.jsonl` entries pin `parents` and work.

Dropping the class for the whole table buys nothing there and costs precision: `getData` with
descriptor `()Landroid/net/Uri;`, `setAction(Ljava/lang/String;)`, `getExtras`,
`putExtras` are all plausible names on app classes and on other framework classes, and each
accidental match gives that class a synthetic `.<action>` / `.<extras>` field and a summary row
nobody asked for. The fix is not a new mechanism -- pin the class where the declared type is
`Intent` (or `ComponentName`, or `Bundle`), and fall back to name-plus-descriptor only for the two
rows where the declared type genuinely varies. Write down which rows are in which bucket, because
"the scan enumerates every function" is currently doing the work of a decision nobody made
deliberately.

## 2. Nothing has verified that a summary on a bodyless framework method instantiates

Unchanged in substance from the previous round, and it is worth restating precisely because the
revision's counting *looks* like it addresses it. The chain is: the dex frontend records a VMT
entry for an externally-referenced method (`dex/mod.rs:119-121`), CHA resolves
`Landroid/content/Intent;->setAction` to itself, codegen emits a `call` row
(`codegen/mod.rs:536-545`), and instantiation joins `summary(tgt, ..)` with `call(f, insn, tgt)`
(`index_engine/mod.rs:1164`) to produce the `assign_like` edge. Each link was read in the source;
none was observed end to end.

What the pass's counts add is visibility into the *first* link only: how many functions the scan
matched, and how many call sites resolve to them. A row can match a function, that function can
have call sites, and the summary can still produce zero `assign_like` rows if any later link is
wrong. So the check that gates this phase is still the two-line one from last round -- index the
fixture, count `assign_like` rows attributable to those `summary` rows -- and it is now cheaper to
write, because the pass knows which function ids they are. Do it before writing the emitter, not
after.

## 3. The built-in rows can be added to, but not overridden or removed

`summary` is a relation and the design's rows are a union with whatever a model file contributes.
That is stated as a virtue -- "model files keep working alongside the pass" -- and it is, for
*adding* an app-specific wrapper. It is not a substitute for what a data file gave a user before:
a coarse built-in row could be edited or deleted. Now it cannot. A user who thinks
`Intent.<init>(Intent)` as `Argument(1) -> Argument(0)` is too coarse for their app has no gesture
that removes it; adding a narrower row leaves the coarse one in place, and the union keeps the
imprecision. `--no-default-models` reaches the JSONL and not the pass.

This is a one-flag problem (`--no-intent-api-rows`, or folding the rows under the existing
gesture), but the design should decide which, because the answer also decides what a support
answer looks like when someone's intent modelling is wrong in a way only they can see.

## 4. The zero-match error names an artifact the pass cannot see

The design says to treat "zero matched functions in an import whose string pool contains
`Landroid/content/Intent;`" as an error. By the time the link step runs, the import loop is over
and every import's IR and dex are dropped -- that is the same constraint the design leans on
elsewhere to justify finding send sites in `facts.call` rather than in the IR. There is no string
pool to consult.

The condition is recoverable, and cheaply: the `IdMap` holds a `Function` entry for every
referenced method, so "the id map contains any `Landroid/content/Intent;->…` function and the table
matched none of them" is the same test, expressible where the pass actually stands. Restate it that
way; as written it is a check that cannot be implemented at the point it is specified.

## 5. Deleting the `java-index` lines couples a data file to a pass that may not run

The two android `Intent` lines come out because they conflict with the new rows. That is right when
the pass runs. The design does not say what happens when it does not: a `--dex` import of a bare
`classes.dex` with no APK around it, a jvm import of Android code, an APK whose manifest fails to
decode, or the feature behind a flag while it stabilises. In each of those the deletion is a
straight recall regression with no diagnostic -- extras and action strings stop propagating at all,
rather than propagating coarsely as they do today.

The likely answer is that the API scan is manifest-independent and runs for every Java import, in
which case say so in the design, because it is not currently stated anywhere: Phase 3 introduces
the scan inside a pass whose other steps all need the manifest. If instead the pass can be off,
the deletion needs to be conditional, and a conditional data file is worse than either end of it.

## 6. The keyed/lumped counts are not a partition once the in-fixpoint rule runs

Phase 4 asserts that the pass's keyed and lumped counts "must partition [total extras call sites]
exactly". They do for the fact route, where the pass chooses one tier per site. They do not once
the optional rule ships: the design is explicit that at a site whose key only `const_reaches`
recovers the keyed edge is *added* to a lumped edge that stratification forbids withdrawing. Such a
site is in both buckets. Either the assertion needs a third number -- lumped sites the rule later
keyed -- or the rule ships off and the assertion is scoped to that.

There is a second, quieter interaction in the same place: `const_reaches` is gated on
`intent_frame`, so a `Bundle.put*` site in a function that never touches an `Intent` is outside the
gate and its key can never be recovered by the rule, however non-literal it is. The fact route has
no such gate, because a literal key is syntactic. So the two routes cover different site sets, and
the counts should say which route produced each number rather than being summed.

## 7. `.<k>` versus `.<extras>.<k>` is a convention with no stated invariant

The revision fixed the level arithmetic (see above) by giving `Bundle` bare `.<k>` while `Intent`
gets `.<extras>.<k>`. That composes, and it is not the only scheme that does: making a bundle carry
its entries at `.<extras>` too, with `putExtras: Argument(1).<extras> -> Argument(0).<extras>` and
`getExtras: Argument(0).<extras> -> Return.<extras>`, composes as well and gives every bag of keyed
values one spelling. The design picks the mixed convention without arguing for it, which matters
because the next methods anyone adds are the ones that cross the two worlds: `replaceExtras(Bundle)`,
`Bundle.putAll(Bundle)`, `getBundleExtra(String)` -- a bundle nested inside extras -- and
`putExtra(String, Bundle)`. Each is a place a level mismatch produces silence.

State the invariant ("a `Bundle`'s entries live at the bundle's root; an `Intent`'s live under
`.<extras>`") next to the table, and check the four methods above against it before the emitter
lands. This is cheap now and archaeology later.

## 8. Reusing the call's own site for emitted `assign` rows is a new fact shape

Every other synthetic emitter in the tree mints a fresh site: `jni::link` (`languages/jni.rs:877`),
the declarative-bridge emitter (`model_matches.rs:338`), and the design's own bridge sites. The
extras emission does not -- it pushes `assign` rows at the *existing* call site, which is what
gives the store a real source span and is a genuine improvement in reporting. But it produces a
site that carries a `call` row, `actual_param` rows, `callee_info` and now `assign` rows, and
nothing in the tree produces that shape today: codegen's `CallAssign` binds its return through
`actual_param`, not through an assign.

Nothing obviously breaks -- the index derives `func_id` from the site and ignores the rest -- but
"obviously" is again doing work. Two things to check before committing: whether any consumer maps a
site to a single statement kind (SARIF step rendering, `tainted_var_at_insn`, the graphviz dump),
and whether `prog_store` and `copy_edge`, both of which read `facts.assign` unconditionally
(`index_engine/mod.rs:975-1000`), behave sensibly when the row's site is also a call. The fallback
is a fresh site and a lost span, which is a small price if the check goes badly.

## 9. The `prog_store` advantage is real, narrow, and flag-gated

The design calls rooting the store at the receiver variable "the one thing the IR rewrite bought
that a rule cannot", and the mechanism is correct: `prog_store` is built from `facts.assign` before
the fixpoint (`:992-1000`) and gates the aliasing summary rule (`:1187`), which no in-fixpoint edge
can reach. Two qualifications the design does not make.

It is narrower than it sounds. A helper that writes an extra onto its *formal* exports a summary
through the ordinary rule, because the call-site edges are symmetric and the destination variable
is the formal itself; the aliasing rule is only needed when the write goes through an alias
(`Intent j = i; j.putExtra(..)`), and `ssa::propagate_copies` fuses most of those before codegen
ever runs. And it is configurable: the rule is gated on `config(c), if c.alias_rule`, default true
but user-flippable (`main.rs:744`). So an argument presented as decisive rests on a narrow case
under a flag. Keep the design decision -- it costs nothing -- but do not let it carry the weight of
justifying the fact route on its own; the stratification argument in item 6 is the real one.

## 10. `paths` is now a cost centre and nobody has measured it

Carried forward from last round's item 5 and made sharper by the revision. The `intent_frame` gate
and the constant rules were already asserted-cheap rather than measured-cheap, inside the fixpoint
that the whole engine's cost is measured against. The revision adds a second, more structural cost:
minted paths land in `paths`, which is the membership gate probed by the hottest rules in the block
(`locals` forward and backward, `context_locals`, `call_target_assign_like`). The rule mints one
path per recovered key plus one composition per `model_paths` entry per key -- a few dozen times the
key count, which the design correctly avoids being a `|model| x |program|` cross, but which nobody
has put a number to on a real app.

`#![measure_rule_times]` is already on the block, and `IndexResult.paths` is already saved, so both
numbers are one run away: `paths` size with and without the pass, and the per-rule time of the two
constant rules. Make that the first measurement, not the last. The same run answers item 2.

## 11. `call` becoming derived is still the change with reach outside this feature

Unchanged, with one detail sharper than last round. `call.parquet` stops being what codegen emitted
and becomes what the fixpoint concluded, which changes the saved call graph for every consumer --
`ctadl inspect`, the dot dump, `flowy::get_endpoints`, SARIF call-site rendering. At query time the
table is read from `IndexFacts` in at least two builder paths (`cli/mod.rs:633` and `:655`), and
both have to move to `IndexResult` together; a half-moved version compiles and silently queries a
call graph without intent bridges.

The two checks from last round stand: whether any consumer distinguishes "a call site with no
source span" from "a span lookup that failed" -- intent bridges will have none, at a rate JNI
bridges never reached -- and whether writing the table from `IndexResult` leaves a window where a
crash mid-index yields an index directory with every other fact table and no call table. The
format-version bump covers the schema; it does not cover a half-written directory.

## 12. Bucket accounting is still an argument, not a finding

Unchanged. The case for keeping the constant rules intraprocedural rests on the claim that the
bucket needing a constant to *originate* in a callee is small. That is a reasonable reading of how
Android code is written, and R8 inlining pushes the same way, but it is a prediction about a corpus
stated with more confidence than the evidence supports. The unresolved-pairing counter is the right
instrument and has never been read.

The revision adds a second prediction of the same kind, and it is load-bearing for the whole
key-precise tier: that R8 leaves nearly every extras key as a literal at the site. If that is true
the in-fixpoint rule is optional and item 6 evaporates; if it is not, the rule ships with a
conflation it cannot remove. The count exists in the design (Phase 4 asks for it) -- read it before
building the rule, not after.

## 13. Lumped write followed by keyed read is still unpinned, and now matters more

The design names the asymmetry and defers it to a nightly case, which is where it belongs, but the
case still does not exist: `.<extras>` is not an extension of `.<extras>.<key>`, so the forward
field rule cannot fire and the flow arrives only through the source-path rule, one level deeper
than the writer put it, reaching a sink only if the sink port materialises over that path.

What changed is the stakes. Under the previous revision the tiering was a property of a data file
plus a rewrite anyone could disable. Now the pass decides per site which tier a site gets, and item
6's coexistence means some sites get both. Two two-line cases (keyed write / lumped read, and the
reverse) settle the semantics; write them before the emitter chooses tiers around an assumption
about them.

## 14. Flow-semantics validation is still thin

Unchanged from both previous rounds. Phase 4 covers the manifest, the inventory, the fan-out budget
and now the scan's match table, but flow correctness still rests on one hand-verified flow plus
synthetic cases that do not exist. DroidBench's ICC suite and ICC-Bench are ready-made ground truth
for the explicit / implicit / extras / result-back matrix; decide whether they are in or out and
record why. The `intent:*` cases cannot use `expected_lines` on this fixture (R8 stripped the line
tables; ~1% of methods retain one), so whatever ground truth is chosen has to be assertable on
component and method identity.

## What holds up

The phasing, the triple store over a typed schema, the fresh-site aliasing caution for *bridge*
sites, the name-normalization warning, the format-version bump, the one-bridge-site-per-send-site
argument, and the section on what not to port from the previous ctadl all remain correct.

Confirmations from this round -- the revision's new claims were checked against the code and hold:

- **`summary` is an ordinary relation, and a `model.propagation` entry is only a matcher in front
  of it.** Declared at `index_engine/mod.rs:1072`, seeded from `facts.summary` at `:1429`, derived
  in-fixpoint at `:1173` and `:1187`; `codegen_propagations` (`model_matches.rs:106-153`) does
  nothing but resolve a name, expand ports, and push `formal_param` + `summary` rows. Writing those
  rows from a pass is the same act minus the matcher.
- **Pre-fixpoint rows are what reach `model_paths`.** `summary_paths` is collected from
  `facts.summary` before the run (`:1023`) and seeded into `model_paths` (`:1437`); no rule derives
  `model_paths`. So the design's insistence that the static rows be *input* rows is not stylistic --
  a rule deriving the same rows would lose the one-level concat at `:1133-1135` and with it
  `.<intent>.<extras>.<key>`, silently. The doc comment at `model_matches.rs:103-105` says the same
  thing from the other side.
- **A summary cannot be per-site.** Instantiation joins `summary(tgt, ..)` with `call(f, insn, tgt)`
  (`:1164`), so a key-bearing summary row would apply one site's key at every site. The design's
  choice of an `assign` / `assign_like` row for the keyed tier is forced, not stylistic.
- **The minted-path termination argument is airtight.** `const_reaches` carries its symbol column
  unchanged from `const_str_assign`, a fixed input relation, so the fixpoint adds (vertex, symbol)
  pairs and never new symbols; minted paths are a subset of `{.<extras>.k}` over that fixed
  alphabet at a fixed depth, and nothing minted feeds back into the constant set. This is the
  hazard the comment at `:1119-1120` warns about, and it does not apply here.
- **Negation really is unavailable.** Confirmed again: the block contains no `!rel(..)`, ascent
  desugars it to an aggregate, and the MIR builder rejects an aggregated relation in the rule's own
  SCC (`ascent_mir.rs:288`). So "suppress the lumped edge where a key was recovered" is a compile
  error, not a design preference -- which is what makes the fact route load-bearing rather than an
  optimisation.
- **The derived rows survive to query time.** `IndexResult` saves `assign_like` and `paths`, and
  the query builder reads exactly those (`cli/mod.rs:634-635`), while `formal_param` and `call`
  come from `IndexFacts` (`:631`, `:633`). So minted paths and rule-derived edges are visible in
  reports, and the pass's `formal_param` rows must be pushed before `facts.try_save` (`:393`) --
  which the proposed link slot, between `jni::link` (`:349`) and `codegen_model_matches` (`:352`),
  satisfies.
- **The qualified id parses unambiguously.** A dex method id is `Lcls;->name(params)ret`
  (`dex-reader/src/parser.rs:796`, with `pretty_signature` contributing `(params)ret` and no second
  `->`), and the jvm frontend builds the same shape (`jvm/mod.rs:518`). One `->` separates class
  from member, so the scan's parse is well defined for both Java frontends.
- **Minting a path from a recovered constant costs nothing.** `PathSegment::Symbol` holds a
  `ctadl_ir::Symbol = ArcIntern<str>` (`ctadl-ir/src/mir/mod.rs:193-198`, `:155`), the same type
  `Exp::Str` and `const_str_assign` carry, so the design's "pointer copy" is literal.
- **The pass's `formal_param` rows do not distort `Argument(*)`.** `compute_arg_arity` takes the
  max over declared formals *and* actual call-site indices (`index_engine/mod.rs:263-283`), and the
  indices the intent rows name are all real arguments, so no phantom parameter is introduced --
  unlike the bridge emitter's cross-function rows, which the code comments already flag.
- Earlier rounds' confirmations that still stand: `call` is the only thing carrying a flow across a
  function boundary at query time (`query_engine/mod.rs:442`, `:455`), which is why delivery must
  derive `call` rows; delivery needs no `actual_param`, and an `assign` keeps it one-directional;
  the constant-propagation shape already exists as `call_target_assign_like` (`:1342-1356`); the
  three codegen hooks sit one line from the object-ref handling (`codegen/mod.rs:432`, `:676`,
  `:793`); a literal cannot cross a procedure boundary through `summary`, so the callee-origin
  bucket needs the extra rules rather than a better placement; and no access-path length limit
  exists, so `this.<intent>.<extras>.<key>` at depth three is representable.
