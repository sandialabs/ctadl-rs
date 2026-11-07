# Criticism of `overhauling-models.md` (revised) - DO-NOT-MERGE

Findings from reading the revised plan against the code and against the four bridging-models
documents (cited as *facts design*, *facts criticism*, *IR design*, *IR criticism*). The revision
resolved most of the previous round's findings — §7 records which, because an audit trail of
deliberate resolutions is what keeps them from being "simplified" back out. What remains is two new
defects introduced by the revision (§1–§2), one ordering subtlety the new streaming posture creates
(§3), internal inconsistencies left over from the previous draft (§4), and carried-over
under-specification (§5–§6). Every claim below was checked against the tree.

## 1. The from/to path composition registers a path that no rule ever reads

The new codegen bullet: "we can compose the from/to paths and add them to the indexer paths. If a
bridge model goes from `Argument(0).foo` to `Argument(3).bar` then we can add the access path
`.foo.bar`." Under the emission the plan itself specifies two bullets later — formal read into a
temporary, temporary passed as the actual — the composite `.foo.bar` is inert. Traced through the
actual rules:

- The bridge's own facts are `t.bar = formal0.foo` (and its converse) plus
  `actual_param(site, 3, t)`. The propagation rules gate on the *rewritten* path: a flow crossing
  the in-assign carries residue `q` and is gated on `paths(bar.q)`; crossing the out-assign, on
  `paths(foo.q)` (`index_engine/mod.rs:1058-1072`, `substitute_prefix` on the assign's two
  endpoint paths). The concatenation `foo ++ bar` is the gate query for **no** derivation: it
  would only ever be asked for if the *caller's own code* mentioned `formal0.foo.bar`, in which
  case it is already a program path. Registering it burns a `facts.paths` row to enable nothing.
- The composite belongs to the *other* design. `from_path ++ to_path` is the path family of the
  facts design's §9 **rejected** alternative — drop the temporary, let the caller's port *be* the
  callee's parameter with sub-structure visible through it. The plan grafts the rejected
  alternative's registration onto the chosen alternative's emission, where it has no meaning. (And
  the rejection stands on its own terms: even under structural sharing, one composite was not
  enough — the needed family was `from_path ++ to_path ++ q`, open over the callee's summary.)
- The paths that are *actually* missing remain missing. The composition gap (facts criticism §1:
  pathful bridges compose with the callee's summary only at exact path equality) needs
  `paths(foo.q)` and `paths(bar.q)` for every residue `q` the callee's **derived** summary
  produces — fixpoint output, unenumerable when models are matched. `.foo.bar` does not touch
  this; the flows still drop silently. The only complete fix on the table is still the
  demand-driven fixpoint rule the facts criticism §1 revived.

Also in this bullet: "the bridging model paths are individually added to the indexer paths" is
redundant — `program_paths` is seeded from both endpoints of every `assign` and every
`actual_param` vertex (`mod.rs:1038-1049`), which is why the facts design's §4.4 states "no
`facts.paths` push anywhere in a bridge". Harmless, but the sentence suggests the registration is
load-bearing when it is a no-op.

The fix is to delete the composition bullet, or replace it with what it is presumably reaching
for: `access_paths` (§7 — now correctly specified as a human-declared registry) *is* the escape
hatch for deep composition, because a user who knows the callee's summary shape can pre-declare
`foo.q` / `bar.q` by hand. Say that, and say the default behavior: flows needing residues nobody
declared are dropped.

## 2. `forward_self` is not a special case of bridging models, and deleting it loses the construct

The plan: "`forward_self` and `forward_call` are special cases of bridging models, so they should
be deleted in favor of the bridging models." Half of this contradicts a settled finding, and the
settled finding is right.

`forward_call` — yes: the facts design §5 already concluded it is "the same-program special case
of `bridge`: once `bridge` exists, folding it in is a one-line desugaring", and it is schema-only
today (`docs/model-generators.md:475-481`, no loader support), so deletion costs nothing.

`forward_self` — no. It forwards calls on the matched method to *another method on the same
object* (`docs/model-generators.md:483-499`): the target is selected **per receiver class**, a
correlation between the matched callsite's receiver and the class hierarchy. A bridge's `to` side
is an independent `where` evaluated against a program; nothing in the bridge syntax can express
"the `doInBackground` of *this receiver's* class". This is exactly why the facts design §5 called
`forward_self` "the only genuinely separate construct left" after `bridge` lands — a conclusion
both criticisms checked and let stand.

Worse, the plan's own new pairing rule makes the natural attempt at emulation a known bug. Under
"full cross product of matches", `{from: execute*, to: doInBackground*}` pairs every
`AsyncTask.execute` with **every** `doInBackground` in the program — which is precisely the defect
in the hardcoded `models/codegen.rs` rule that facts criticism §7 flagged with "do not preserve
the bug in the translation". Deleting `forward_self` in favor of bridges canonizes it.

Either keep `forward_self` (schema-only today; deleting it is not even a simplification of
shipped behavior), or extend bridge pairing with a receiver-correlated mode — which is real design
work the plan would need to specify, not a deletion.

## 3. Streaming matching cannot literally implement the conditional `to`-side semantics

The revision moves matching to "on demand, as we load IR" — the right memory posture (§7). But
the bridging semantics say: "If the `from` side doesn't match anything, then the `to` side isn't
even attempted a match." Under streaming, side A's program and side B's program arrive in project
order; the `to` side's import may stream *before* the `from` side's. At the moment B's VMT is in
hand, whether `from` matched is unknowable — so the `to` match must be attempted (or B's match
index retained, which reintroduces the retention the streaming posture avoids).

The semantics are fine; the operational phrasing is not. What streaming supports is: attempt both
sides eagerly per import, accumulate matches in `ProgramModelMatches`, and apply the
conditionality — pairing, `on-unmatched` classification, warning emission — once, after the
stream ends (phase 2 is the natural place; it is already defined as "after all the IR"). The plan
should state that "isn't even attempted" describes *reporting* semantics (no `to`-side warning
when `from` is empty), not evaluation order. One sentence; but left unstated, the first
implementer discovers it as a design contradiction mid-build.

Related, one decision short of specified: the revision keeps query-time source/sink matching (the
right call, §7) — so there are now permanently **two matching pipelines** over the same `where`
language: query's per-import `ModelGeneratorIngest` (`cli/mod.rs:218-243`) and the new streaming
index-time matcher feeding `ProgramModelMatches`. Both prior designs made a rule against exactly
this ("there must not be a second implementation of `where`"). The rule survives only if both
pipelines share the evaluator — the `ProgramMatchIndex` extraction of facts design §4.2. The plan
adopts the facts design "as the concrete model_generator syntax"; it should adopt this
architecture item by reference too.

## 4. Leftovers from the previous draft contradict the new core

The revision's core is the new `ProgramModelMatches` struct — matches live in their own
structure, not in the IR. Three sentences from the previous draft survive and contradict it:

- "The matched models **stored in the VMT** are stored in-memory only…" — nothing is stored in
  the VMT under this design; the previous draft's VMT fields are gone. The note answers a
  question the revision made moot, while re-raising it by its wording.
- "…because **the IR is modified** after the index loads the artifacts from disk" — nothing
  modifies the IR under either draft ("we don't have to generate new functions or anything");
  bridges codegen directly to facts. The rationale describes neither design. The *correct*
  rationale for in-memory-only is the one from the previous criticism: matches are a function of
  (artifact × models files), and the import cache must stay a pure function of the artifact or
  `ctadl index --models a.json` poisons the next run.
- "a **persistent** `ProgramModelMatches`" — "persistent" reads as "persisted to disk", the
  opposite of the in-memory note. It means "persists across the import loop"; say that.

Plus the typo `PrograModelMatches`. These are ten minutes of edits, but as written the document
disagrees with itself about where matches live and why.

## 5. The bridge emission is still compressed past the point the last round paid for

The revision improves this — the fresh callsite is now explicit (consequence 3 of the facts
design's §2), and the port space is enumerated (`AnyArgument`, `Index`, `Return`, `Global` — the
"and so on" that hid `Global` last time is gone). Still absent, and each one a place a prior
round found a real defect:

- **The globals pseudo-parameter** mapped unconditionally, or heap flows do not cross
  (consequence 5; `JniFlow` is the regression that catches it).
- **Formals synthesized on both sides** (consequence 4) — and side B's rows are a cross-function
  emission the phase-2 codegen walking side A's bridge entries must be told to produce.
- **Temporaries keyed `(site, index)`** — under the plan's cross-product pairing, several bridge
  sites share one caller, the exact scenario the keying exists for.
- **Return-arity asymmetry** (Java's exception return deliberately unmapped), **`direction`** as
  which of the two assigns get pushed, the **degenerate empty-path collapse** that keeps
  `languages/jni/tests.rs` byte-identical, and the **site-keyed** (not function-keyed) `assign`
  shape — the precise error facts criticism §4 caught in the precise section last time.
- **The expansion details** the new propagations codegen must replicate from
  `codegen_summary`: the `dst == src` skip and the `formal_param` pushes for both ports
  (`codegen/models.rs:73-99`) — the items IR criticism §3 listed against "codegen gains one
  loop".

The plan already incorporates the facts design by reference for *syntax*. Extend the reference to
its §2 (the five consequences) and §4.4-as-corrected for *emission semantics*, and this section
disappears.

## 6. Smaller points

- **`on-multiple-match` naming** (the plan asks): the document family's own distinction is the
  useful precedent — the facts criticism §2/§3 separated *unmatched* (a side matched nothing)
  from *ambiguous* (matched, multiply). `on-ambiguous: warn|ignore|error` pairs naturally with
  `on-unmatched` and names the condition rather than the mechanism; `expect-unique: true` is the
  other defensible shape (cf. SQL's cardinality-violation vocabulary, which the plan is right to
  avoid surfacing). Whatever the name, the warning text should include the match counts and the
  `(file, generator index)` provenance every other loader message carries.
- **The per-spec count line is still missing.** Warn-by-default (§7) now covers the empty-match
  cases, but nothing covers the *mis-paired* case — wrong slot, wrong path, wrong function
  matched — which produces flows-missing silence. The JNI experience (facts design §7) is that
  the unconditional per-spec count at `info` is the mitigation that actually gets read; a
  bridge-only generator still appears on no other surface (no endpoint stats, no `CTADL0004`).
  One line in the plan: phase 2 logs matched/paired counts per generator, unconditionally.
- **The query-side warning has no index-side twin.** Query now warns that it ignores
  propagation/bridging models — good, that shrinks the silent-inertness family the facts
  criticism §7 documented. The converse remains silent: source/sink models passed to `ctadl
  index` are discarded without a word (`cli::index` consumes only summaries). Symmetry costs one
  warning and closes the family.
- **`in` scoping plumbing is still unpriced.** The plan adopts the facts design's syntax, which
  includes `in`; the facts criticism §5 established that enforcing it means threading
  language/import identity through the `try_load_models*` chain (`models/mod.rs:73-77` documents
  the deliberate non-threading). Streaming matching has to build this anyway — the matcher must
  know which import it is looking at — so the cost is real but now on the critical path; note it.
- **"Matched anywhere" vs "matched per import" is unstated for `on-unmatched`.** A `from` side
  that matches in import 1 and not import 2 presumably counts as matched; with `in` scopes in
  play (a generator whose scope admits no import in the project), say whether scope-admits-nothing
  is "unmatched" (warn, under the new default — acceptable) or a distinct configuration error
  (the facts design §3.1 wanted it loud; warn-by-default gets close enough to drop the
  distinction, but only if the warning fires).

## 7. What the revision resolved, and what carries over as right

Recorded so the resolutions read as deliberate. Each item below was a finding in the previous
round; the revision's answer checks out against the tree:

- **The query phase is decided, correctly.** Query keeps its own source/sink matching and warns
  on index-time models in its input — preserving iterate-on-queries-without-reindexing, at the
  cost of the two-pipeline risk now flagged in §3. The unpriced "saved description" artifact is
  gone entirely.
- **Persistence is decided, correctly**: matches are in-memory only (modulo §4's stale
  rationale).
- **Memory posture is decided, correctly**: streaming matching with a persistent match structure
  is the "match indexes resident, one `ProgramInfo` at a time" discipline both prior designs
  demanded — better, since `ProgramModelMatches` retains only *matches*, not match indexes
  (modulo §3's ordering caveat, which may force retaining side-B indexes or eager evaluation).
- **`AnyArgument` expansion is placed correctly, and better than today.** Phase 2 runs after all
  IR is codegen'd and consults actual parameters and call sites — resolving the IR criticism
  §3 objection, and strictly improving on today's *per-import* expansion in `codegen_summary`
  (`codegen/models.rs:50-53`, keyed on `func_num_params` at that import's codegen), which
  under-expands a function whose call sites span imports.
- **`access_paths` is respecified as what it can actually be**: a registry of human-known
  composed paths (`.next.next.next`). The previous round's "redundant or impossible" objection
  is answered — provided §1's confusion between this registry and automatic bridge-path
  composition is resolved in the registry's favor.
- **Pairing is specified** (full cross product) and **guarded** (`on-multiple-match: warn` by
  default) — answering the facts criticism §2's "specify the pairing function" directly, with
  the warn default handling the shared-`arguments`-map-is-per-method concern by nudging toward
  singleton×singleton. §2 above shows the cross product must not be asked to carry
  `forward_self`'s weight, but as bridge semantics it is now a decision, not a gap.
- **`on-unmatched` defaults are warn/warn** — the from-side `ignore` default that silenced the
  dominant failure mode is gone; the conditional `to`-side evaluation is retained and is the
  right structure for optional families.
- **Carried-over strengths**: matching against the VMT preserves models on functions with no IR
  body (Lua externals — the IR criticism §2 trap, still dodged, still worth one sentence marking
  it deliberate); the native `bridges` representation still kills the entire IR-synthesis hazard
  class (IR criticism §5) and the phantom-name hazard (§6); `propagations` feeding `model_paths`
  and initial summaries preserves the model-path bucket discipline (`mod.rs:916-919`).

## 8. Conclusions

1. **The skeleton is now decided and sound**; the two defects the revision *introduced* are both
   in the "notes" tier, and both are deletions-of-a-sentence to fix: drop the inert `.foo.bar`
   composition (or fold it into `access_paths` as user-declared paths with a stated
   dropped-by-default caveat), and drop `forward_self` from the deletion list (keep
   `forward_call` on it).
2. **State the streaming/conditionality reconciliation** (§3) — evaluation is eager, semantics
   are applied at phase 2 — and commit both matching pipelines to the shared evaluator, or the
   two-implementations-of-`where` drift the whole document family legislated against arrives by
   default.
3. **Purge the previous draft's residue** (§4): the VMT-storage note, the IR-is-modified
   rationale, "persistent", the typo. The document currently disagrees with itself about its own
   core structure.
4. **Finish by reference, not by re-derivation**: emission semantics (facts design §2 + §4.4 as
   corrected), the per-spec count line, the index-side discard warning, and the `in` plumbing
   cost are each one line to incorporate. The revision demonstrates the pattern works — its
   best sections are the ones that adopted a criticism's conclusion verbatim.
