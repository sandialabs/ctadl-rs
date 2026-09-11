# Android Intent support - DO-NOT-MERGE

A design for teaching CTADL the Intent surface of an Android app: the components declared in
`AndroidManifest.xml`, the intent filters that make them reachable, and the data flows that run
between components through intents.

## 1. The problem

CTADL imports an APK by reading its `classes*.dex` entries and nothing else. It therefore sees an
Android app as a pile of classes with no relationships between them beyond calls. Two things are
invisible:

- **The app's boundary.** Which components another app -- or `adb shell am start` -- can reach is
  declared in the manifest, not in the code. Without it, the components and the routes into them
  are simply not represented.
- **Delivery.** `startActivity(intent)` in one component and `getIntent()` in another are a data
  flow, but no call connects them. The flow stops at the `startActivity` call.

Both gaps are closed by the same artifact, `AndroidManifest.xml`, which CTADL currently never
opens. The only mention of it in the tree is a test fixture's magic bytes
(`dex-reader/src/apk.rs:599`).

## 2. What already exists

Worth stating plainly, because it decides most of the design.

**Getting the bytes is solved.** `dex_reader::apk::read_bundle_entry(path, entry_name)` reads an
arbitrary entry out of an APK by name. `APKParser::new` keeps only the decompressed DEX buffers and
drops the archive, so manifest reading follows the same free-function pattern the native-library
readers already use.

**Cross-boundary joins have one home: bridges.** Mechanically a bridge is one `call` row plus one
`actual_param` per port between two functions no static call connects -- see
`emit_bridge` in `ctadl-ascent/src/languages/jni.rs:868`, which is 25 lines and knows nothing about
JNI. The JNI pass is the built-in instance; `model.bridge`
(`ctadl-ascent/src/models/spec.rs:407`) is the declarative one. The mechanism was built for
cross-language flow, but it is not restricted to it: `docs/model-generators.md:284` records that
`forward_call`, the same-program special case, was removed from the schema in favor of `bridge`.
Intent delivery is a same-language use of the same shape.

**The flow relation already propagates a tag that is not taint.** `call_target_assign` attaches an
opaque object -- a function pointer, a Java allocation class -- to a *vertex*, and
`call_target_assign_like` carries it across `assign_like` with the same path substitution and the
same `paths` gate the taint rules use (`ctadl-ascent/src/index_engine/mod.rs:1342-1356`). It is
what resolves virtual dispatch. A string constant is the same shape of thing with a different
payload, which is what makes Phase 2 an instance of an existing mechanism rather than a new one.
And the engine is open at the other end too: `call` is an ordinary relation
(`:1060`), the block contains no Datalog negation, and `jni::link` already puts synthetic `call`
rows into the fact base -- so a pass can hand the engine a join and let it *derive* call edges
rather than having to decide them first.

**Summaries are a relation, not a model-file artifact.** `summary(FunctionId, FormalIndex, Path,
FormalIndex, Path)` is declared like any other relation (`index_engine/mod.rs:1072`), seeded from
`facts.summary` (`:1429`), and *also* derived inside the fixpoint by two rules (`:1173`, `:1187`). A
`model.propagation` entry is a matcher in front of that relation and nothing more:
`codegen_propagations` (`codegen/model_matches.rs:106-153`) resolves the entry's function name to a
`FunctionId` through the site `IdMap`, expands its ports to formal indices, and pushes one
`formal_param` row per index and one `summary` row per pair. Anything that can name a `FunctionId`
and two ports can push the same rows. Two properties of the relation decide most of Phase 2's shape:

- **The paths a summary names become `model_paths` -- but only if the row exists before the
  fixpoint.** `summary_paths` is collected from `facts.summary` (`:1023`) and seeded into
  `model_paths` (`:1437`), the bucket that concatenates one level with every program path
  (`:1133-1135`); the doc comment on `codegen_propagations` says outright that the documented port
  semantics depend on rows landing there. Nothing *derives* `model_paths`, so a summary row derived
  by a rule contributes no path to that bucket and has to register its own.
- **A summary is keyed by its callee**, so it holds at every call site of that callee (`:1164`).
  Anything that varies per site -- an extras key -- is not a summary at all; it is the `assign_like`
  row that instantiation would have produced, written directly.

**There is no whole-program entrypoint notion.** The analysis is compositional and summary-based;
every function is analyzed. The previous ctadl's `StatementReachableFromOnCreate` rule existed to
work around a whole-program reachability model and has no counterpart here. One thing that rule
bought does need a counterpart: the framework, not the app, calls lifecycle methods, so a flow
delivered into `onCreate` alone is invisible to the rest of the component. Phase 3 handles that
by bridging into every lifecycle method rather than reintroducing whole-program reachability.

## 3. The blocking gap: constants are dropped

The dex frontend builds string constants -- `Instruction::ConstString` becomes `Exp::new_str`
(`ctadl-ascent/src/languages/dex/mod.rs:640`), `ConstClass` becomes `Exp::new_str` of the type
descriptor (`:655`) -- but codegen maps every expression that is not a variable or access path to
no vertex at all (`trans_exp`, `ctadl-ascent/src/codegen/mod.rs:846-853`). No constant reaches the
fact base. A constant *argument* fares worst of all: `trans_exp` returning `None` also suppresses
the `actual_param` row (`codegen/mod.rs:690-692`), so the call-arg pseudo-variable that would have held
it is never created either.

Every part of intent resolution needs them:

| what | comes from |
| --- | --- |
| implicit intent target | the action string on the intent, joined against manifest filters |
| explicit intent target | the class named by `new Intent(ctx, Foo.class)` or `setClassName` |
| which extra was written | the key argument of `putExtra` |

Those are not one question. The first two ask *what value does this intent carry by the time it
reaches this send site* -- a property of a value that flows, and that changes as it flows. The
third asks *what does this instruction name* -- a property of one call site, settled before
anything flows at all. The previous ctadl carried a `_VarIsConst` relation for the first kind.
Phase 2 restores it as a relation over the flow graph the index engine already computes, and
answers the third from the facts those same constants now leave at the call site -- no IR walk, no
def-chase, and no rewrite (see *Key-precise extras* below).

Note also that `const-class` produces an `Exp::Str`, not an
`Exp::ObjectRef(CallObject::JavaObject(_))` -- only `NewInstance` produces the latter
(`dex/mod.rs:664`), and only the latter feeds `call_target_assign`. So explicit intents do not
resolve for free either; they resolve through the same string-constant machinery as implicit ones,
with a type descriptor playing the part of the string.

Manifest parsing on its own therefore buys less than it appears to. Section 5 arranges the work so
that the part which does not depend on constants ships first.

## 4. Design

Three layers, each useful before the next exists.

1. **Manifest to facts.** Decode the binary XML, emit a node/attribute/child triple store, persist
   it with the import.
2. **Constants in the data flow, and the intent API as rows in the engine's own relations.** Emit
   string constants as facts where codegen drops them, propagate them over the flow relation
   intraprocedurally, and give the intent API static access paths -- `.<action>`, `.<component>`,
   `.<extras>` -- so a recovered constant lands somewhere a join can name. Those paths arrive as
   `summary` rows the pass writes, not as `model.propagation` entries in a data file, and
   key-precise extras -- the one shape no data file can express, because its path is minted from a
   recovered constant -- arrive as `assign` rows at the same call sites. Nothing rewrites the IR,
   and nothing has to agree with a JSONL file about the spelling of a synthetic field.
3. **The intent linking pass.** Generate the facts that let the engine do the join: the manifest's
   filters and components as relations over function ids, one bridge site per send site, and the
   delivery edges into it. The pairing itself is two rules in the index fixpoint, so `call` becomes
   a derived relation and the call graph the index saves is the post-fixpoint one.

The shape worth noticing across (2) and (3) is that nothing new is *computed* outside the engine.
Constants, filters, pairings, and the intent API's own summaries are relations; the Rust is a scan
that turns method names into function ids, fact generation, and reporting. That is
what removes the ordering cycle an earlier revision had to design around, and it is why the
constant rules get the real flow relation instead of a rebuilt subset of it.

## 5. Phases

### Phase 1 -- Parse the manifest

**Decoder.** Use a maintained Android binary XML crate rather than vendoring a decoder. Binary XML
is small compared with the bytecode frontends CTADL owns, and owning another format parser is not
worth the maintenance cost unless the crate proves inadequate. The subset needed is narrow: the
header (`0x00080003`), the string pool, the resource-map chunk, and start/end element records with
their attributes. Attribute names arrive as resource IDs -- `android:name` is `0x01010003` -- with
string-pool names alongside; values are string references, integers, booleans, and resource
references. Keep the crate behind a tiny adapter that emits CTADL's own manifest tree, so replacing
it later does not touch the rest of the feature.

Detect plain-text XML by magic bytes and fall back to a text parser; unbuilt manifests are rare but
they exist.

**Fact shape.** Keep the previous ctadl's triple store rather than inventing a typed schema:

```
manifest_node(node_id, tag)
manifest_node_child(parent_id, child_id)
manifest_node_attr(node_id, key, value)
```

It records the manifest verbatim, it is cheap, and it answers questions we have not asked yet --
`android:exported`, `<provider>` authorities, `<permission>`, `android:process` -- without a schema
migration. Derive typed views in Rust at index time, where changing them costs nothing.

**Name normalization.** `android:name=".MainActivity"` is relative to the `package` attribute of
`<manifest>`, and `com.foo.Bar` must become `Lcom/foo/Bar;` before it will join against a dex class
name. Put this in one function with its own tests. Getting it wrong produces zero matches and no
error, which is indistinguishable from an app with no components.

**Persistence.** Write the tables into the import directory alongside the serialized program and
VMT, and bump `IMPORT_FORMAT_VERSION` (`ctadl-ascent/src/project.rs:75`) so a stale import fails
with a clear message rather than a silent absence of manifest facts.

**Split APKs.** The manifest lives in the base APK of a bundle. Check how
`ctadl-ascent/src/languages/xapk.rs` selects artifacts and make sure the base manifest attaches to
the bundle as a whole, not to whichever split happened to carry it.

**The payoff, with no dataflow work at all.** As soon as the facts exist, `ctadl inspect` can
report the app's component surface: every component with `android:exported="true"`, every
component exported by default because it carries an intent filter (below API 31), and the
`android:permission` guard on each -- a component behind a signature-level permission is exported
in name only. The joins Phase 3 runs later are exercised here for free, which is where the
name-normalization function proves itself against a real manifest. Treat the inspect report as
Phase 1's deliverable, not as a side effect.

### Phase 2 -- Constants in the data flow

**The correction this phase encodes.** An earlier revision answered every constant question with
one syntactic helper: a def-chase through SSA that hops single-variable assigns and stops at a
literal. That helper is right for the third row of section 3's table and wrong for the first two.
`putExtra`'s key is a literal operand of the call site itself, and once constants are facts the
call site's own fact carries it -- no walk required. An intent's action is a different thing
entirely: a value written into an object in one place and read in another, possibly across a copy,
a field store, a builder chain, or a framework call whose behaviour is already summarized. Chasing
defs for *that* is re-implementing, worse and separately, the analysis this tool already is. So:
constants become facts, the flow relation carries them, and both questions are answered off the
same facts at different distances from the site.

#### The fact

Where codegen drops a constant, emit one instead:

```
const_str_assign(PackedInsnSiteId, FlowVertex, Symbol)
```

Three emission sites, each of which already has the identical hook one line away, because
`call_target_assign` -- the Java-object / function-pointer tag that drives virtual dispatch -- is
emitted at exactly the same three places:

| where | existing object-ref hook | constant case |
| --- | --- | --- |
| `Assign { dest, sources }` | `codegen/mod.rs:432` | `x = "s"` |
| `Store { dest, field, value }` | `codegen/mod.rs:793` | `o.f = "s"` |
| call arguments | `codegen/mod.rs:676` | `setAction("s")` |

The third row is load-bearing and is the one the def-chase design never needed: an action string is
usually an *argument*, and today a constant argument leaves no trace in the fact base at all.

`Exp::Str` holds an `ArcIntern<str>`, which *is* `ctadl_ir::Symbol` (`ctadl-ir/src/mir/mod.rs:155`)
-- the same type `CallTargetObject::Symbol` already carries. So the fact carries a reference to the
interned string rather than its bytes, and every join downstream, including the join against the
manifest's action names, is a pointer comparison.

#### The propagation

Two rules, transcribed from Call Target Propagation (`index_engine/mod.rs:1342-1356`) with the
payload changed:

```
const_reaches(f, v, p, s) <-- const_str_assign(f, FlowVertex(v, p), s);

const_reaches(f, v1, p_new, s) <--
    const_reaches(f, v2, p_ctx, s),
    assign_like(f, v1, p1, v2, p2),
    if let Some(p_new) = p_ctx.substitute_prefix(p2, p1),
    paths(&p_new);
```

Same `substitute_prefix` step, same `paths` gate, same shape of index. This is not a new mechanism;
it is a second species of one the engine has been running since function pointers were added. Two
deliberate differences from its model:

- **A separate relation, not a fourth `CallTargetObject` variant.** The variants are documented as
  kept apart so that two frontends cannot collide in the `callee_resolvents` key space
  (`facts.rs:1063-1071`); a string tag folded in there would be eligible to *resolve calls*.
- **No interprocedural rules.** `call_target_assign_like` also has a push-down rule through
  `resolvent` (`index_engine/mod.rs:1219-1227`) and a return-direction rule out of callee out-formals
  (`:1385`). Neither is copied. The next section is why that restriction is the load-bearing
  decision of this phase rather than a corner cut.

Because `assign_like` is keyed by `FunctionId` and every rule above stays inside one key, the
relation is intraprocedural by construction -- there is no rule to disable and no flag to get
wrong.

#### Where it runs: in the engine, with the manifest beside it

There is an ordering cycle here, and it is worth naming before dissolving it. If the *pairing* is
computed in Rust between codegen and the fixpoint, then:

- Phase 3 needs recovered constants to decide which components a send site reaches, and it emits
  that decision as a `call` row plus the edges that carry the intent into the callee.
- The `call` row has to exist before the fact base is frozen, because the query engine crosses a
  function boundary **only** on a `call` row (`query_engine/mod.rs:442`, `:455`). A delivery edge
  that exists only inside the index would be a flow the index contains and no report can show.
- So the constants would have to be recovered before the fixpoint that computes the flow relation
  they should be riding -- and the pass would have to reconstruct an approximation of `assign_like`
  outside the engine, duplicating three of its rules in a second place with nothing to keep them in
  step.

Every part of that is an artifact of putting the join outside. Put the manifest *in* -- as input
relations -- and the cycle is gone: constants, filters, and pairing all become relations in one
fixpoint, `call` becomes a derived relation like any other, and the constant rules get the real
`assign_like`, summaries and contextual assigns included, rather than a hand-rebuilt subset.

**`call` can be derived.** It is declared as an ordinary relation
(`index_engine/mod.rs:1060`) and seeded from `facts.call` (`:1411`); nothing stops rules from
adding rows.

The thing that could have stopped them is negation, so it is worth writing down why it does not.
Every rule in the block is monotone -- it only ever adds rows -- which is what makes the least
fixpoint well defined and order-independent. `!rel(..)` is the exception: its truth can flip from
true to false as the fixpoint grows, and Datalog never retracts. Ascent handles that by requiring
**stratification**: a negated relation must be complete before the rule negating it runs, i.e. it
must live in an earlier SCC of the dependency graph. Today `call` is an input, so it is complete
before the fixpoint starts and *any* rule could safely negate it. Deriving it moves it into the
recursive SCC (`call -> assign_like -> const_reaches -> call`), and a single `!call(..)` anywhere
in the block would then be negating a relation still growing beside it.

That failure is loud, not subtle: ascent desugars `!rel(..)` to
`agg () = ascent::aggregators::not() in rel(..)` (`ascent_macro-0.8.0/src/ascent_syntax.rs:988`)
and the MIR builder rejects an aggregated relation in the rule's own SCC with "use of aggregated
relation `call` cannot be stratified" (`ascent_mir.rs:288`) -- a compile error. So the risk was
never a quietly wrong index; it was discovering after the pass was written that the design does not
build. There is no such clause: the whole block has no `!rel(..)` form, and the `!` that a grep
does find are `call_arg!` macro invocations plus two Rust guards on `Path::is_empty` and
`CallString::is_empty`, neither of which is Datalog negation.

Synthetic `call` rows in the saved fact base are already routine: `jni::link` injects one per
bridge (`languages/jni.rs:877-880`), so every downstream consumer already tolerates a call site
with no source span.

**What goes in.** Six input relations, all computable before the fixpoint, none of which needs a
constant -- alongside the `summary` and `formal_param` rows for the intent API itself, which are
ordinary input facts and are the subject of the next section:

| relation | rows | from |
| --- | --- | --- |
| `intent_frame(FunctionId)` | functions containing an intent-API call or an observed send call | the Android call-site observer plus the site `IdMap` |
| `const_str_assign(FunctionId, FlowVertex, Symbol)` | every string literal in an intent frame | codegen's three hooks |
| `intent_send(FunctionId, InsnId, InsnId, FormalIndex, IntentKind)` | one per observed send site: its frame, its fresh bridge site, its own site, which argument is the intent, and what kind of send it is | the Android call-site observer |
| `intent_filter(Symbol, IntentKind, FunctionId)` | (action string, kind, receiving entry method) | Phase 1's manifest, after normalization, alias folding, and the hierarchy walk |
| `intent_component(Symbol, IntentKind, FunctionId)` | (component type descriptor, kind, receiving entry method) | the same |
| `extras_site(FunctionId, InsnId, ExtrasOp, FormalIndex, FormalIndex)` | one per `putExtra` / `get*Extra` / `Bundle` accessor site: which argument is the key, which carries the value | the Android call-site observer |

`intent_filter` and `intent_component` are the whole of "the manifest goes into the engine": a few
hundred rows on a real app, and the only thing Phase 1 has to hand Phase 3. `extras_site` exists
only for the optional keyed-extras rule at the end of this phase; the sites whose key is a literal
need no relation at all, because the pass has already emitted their edges as facts.

**What comes out.** Five rules:

```
const_reaches(f, v, p, s) <--
    const_str_assign(f, vx, s), let FlowVertex(v, p) = vx, intent_frame(f);

const_reaches(f, v1, p_new, s) <--
    const_reaches(f, v2, p_ctx, s),
    assign_like(f, v1, p1, v2, p2),
    if let Some(p_new) = p_ctx.substitute_prefix(p2, p1),
    paths(&p_new);

// explicit: the intent names its target class outright
intent_pair(f, b_insn, recv, PairKind::Explicit) <--
    intent_send(f, b_insn, send_insn, n, kind),
    const_reaches(f, call_arg!(*send_insn, *n), P_COMPONENT, cls),
    intent_component(cls, kind, recv);

// implicit: the intent's action joins a manifest filter of the matching kind
intent_pair(f, b_insn, recv, PairKind::Implicit) <--
    intent_send(f, b_insn, send_insn, n, kind),
    const_reaches(f, call_arg!(*send_insn, *n), P_ACTION, action),
    intent_filter(action, kind, recv);

call(f, b_insn, recv) <-- intent_pair(f, b_insn, recv, _);
```

`intent_pair` exists rather than writing `call` directly so the pass has something to count: the
explicit/implicit split and the unresolved remainder are reads off this relation, and open
decision 2's fan-out budget is a `GROUP BY` on it.

Those five rules are the whole of the pairing. The intent API's own flows are rows, not rules --
the next two sections -- with one optional rule set for extras keys that only `const_reaches`
recovers, which is the only place in the design where a rule mints an access path.

**One bridge site per send site, minted before the fixpoint.** The earlier revision wanted a fresh
site per *pair*, which cannot be minted inside a fixpoint that has not run yet. It is also not
needed: freshness exists so that two unrelated intents cannot alias through a shared call-arg
pseudo-variable, and every pair derived at one send site carries the *same* intent. A send site
delivering to N candidate receivers is an ordinary multi-target call site. So `intent_send` carries
one pre-minted site (`source_info.add_insn_site`, a plain counter -- `index_engine/source_info.rs:52-56`), and
the derived `call` rows all hang off it.

**Delivery is an `assign`, not an `actual_param`, and it is unconditional.** Emitted with the site,
before the fixpoint:

| send kind | edges emitted at the bridge site |
| --- | --- |
| activity | `call_arg(b, 0).<intent> := i`, `call_arg(b, 1) := i` |
| receiver | `call_arg(b, 2) := i` |
| service | `call_arg(b, 1) := i` |

Three things fall out of that shape. The arguments a given receiver does not have are dead ends --
the query's actual-to-formal step probes `formal_param(callee, formal(n), _)`, so `onCreate(this)`
takes delivery at `Argument(0).<intent>` and ignores the `Argument(1)` edge that `onNewIntent`
uses. The edges cost nothing when no pairing is derived, since the site then has no callee. And
using `assign` rather than `actual_param` makes delivery **one-directional**: `actual_param` is
lowered to a symmetric pair of `assign_like` rows (`index_engine/mod.rs:1155-1161`), which would
carry the receiving component's writes back into the sender's intent. Intents cross a process
boundary as parcels; that back-edge would be unsound in the world as well as in the engine.

**The cycle through `call` is monotone, but it is not only the intent rules.** `call` now depends on
`const_reaches`, which depends on `assign_like`, which depends on `call` (summary instantiation).
The intent-specific part of that cycle does not feed constants back to the send-site argument: the
delivery edge points from the sender's intent into the bridge site's call args, and the pairing
rules read the original send site's call arg. But `call` is also an input to the existing hybrid
inlining rules (`critical_summary`, `resolvent`, `context_assign`, contextual summaries, and the
local virtual-call bypass). Deriving intent calls therefore enlarges the engine's SCC, not just the
five rules above. The rules remain monotone, so the least fixpoint is still well defined, but Phase
0 makes this a measured migration: add a fact-level test where a derived call targets a summarized
function and a virtual-call-bearing function, and record the SCC timing before intent pairings are
enabled.

**The plumbing this costs, which is the real price of the design.** `call` stops being purely an
input, so the saved `call.parquet` has to be the *final* relation rather than the pre-fixpoint one.
Today `IndexFacts::try_save` writes it before the index runs (`cli/mod.rs:393`) and query/SARIF
formatting load it through `IndexFacts` (`cli/mod.rs:633`, `:655`). This is not an intent-linking
subtask to do after the rules exist; it is a Phase 0 migration. `IndexResult` gains a `call` column,
`IndexResult::try_save` writes it, every query/inspect/formatter path reads the final table, and
`IndexFacts::try_save` stops writing it. The schema is already the right shape --
`facts/schema.rs:56-62` is `(FunctionId, InsnId, FunctionId)`, exactly the relation -- and
`IMPORT_FORMAT_VERSION` is being bumped for the manifest tables anyway. Move the table in one
change, with the version bump and write ordering together, so a failed index cannot leave a stamped
complete project with a missing final call graph.

**What the intraprocedural restriction still means, now that ordering is not the reason for it.**
The two constant rules stay keyed on a single `FunctionId`: no push-down onto a callee's formals,
no return-direction rule out of a callee's out-formals. But they now run over the *real*
`assign_like`, which contains summary-instantiated edges, so cross-procedure *pass-through* comes
free. Three buckets, and the middle one has changed sides since the previous revision:

- *Constant and send site in one procedure*, however many modeled framework calls sit between
  them -- `startActivity(new Intent(ACTION_VIEW).setData(u))` and its local-variable forms.
  Recovered.
- *Constant in the caller, mutation in an app-defined helper, send in the caller* --
  `configure(i, ACTION); startActivity(i)` where `configure` calls `i.setAction(a)`. This needs
  `configure`'s **computed** summary, which is exactly what running inside the fixpoint provides.
  Recovered, at no extra cost, and this is the concrete payoff for moving in.
- *Constant originating in a callee, or send site in a callee* -- `Intent build() { return new
  Intent(ACTION); }`, a Kotlin companion-object getter, or `void go(Intent i) { startActivity(i); }`
  called from the constant's frame. Still missed: a `summary` row relates a formal to a formal and
  a literal is not a formal, so nothing carries a constant out of the procedure that wrote it. The
  rules that would close it are cheap now that there is no ordering constraint -- see open
  decision 5.

**Scope.** The `intent_frame` premise on the seeding rule is what keeps this from being a
program-wide string analysis. Gate on functions that contain at least one observed call to a method
in the intent-API table or the send-site table. Do not recover this set from `facts.call`: under the
current Java `Mixed` strategy, ambiguous calls are represented as `callee_info`, and `facts.call`
also lacks the declared receiver class needed to exclude `LocalBroadcastManager`. The observer records
the call site's declared class, name, descriptor, site id, and argument count while the IR is still in
hand; link-time code only resolves the recorded method ids through the final `IdMap`. Every intent
construction goes through `Intent.<init>`, so the gate loses nothing the restriction was going to
serve, and it takes the working set from "every `const-string` in 42,000 methods" to a few hundred
frames. Seed *every* string constant inside a gated frame, not only ones the manifest names: keeping
"no constant recovered" distinguishable from "a constant was recovered and matched nothing" is what
makes the unresolved count in Phase 4 mean anything. The manifest's string alphabet is a reporting
aid, not a filter.

**One gate to get right.** Every new path depends on `paths` membership, which fails by producing
zero rows rather than by erroring. `.<intent>.<extras>.<key>` is representable, but only through
the one-level concat rule (`index_engine/mod.rs:1134`), which pairs a *model* path with a
*program* path: `.<intent>` arrives in `model_paths` from the `getIntent` summary row, and
`.<extras>.<key>` arrives in `program_paths` because the pass pushed it into `facts.paths` when it
emitted the keyed store. That is one pass writing both halves, so the composition no longer depends
on a model file having been loaded -- but it does depend on the static rows going in *before* the
fixpoint, which is why they are input rows and not a rule. A key recovered only inside the fixpoint
has neither half and must mint both, which is the second bullet of the rule section below.

#### The intent API in Datalog, not in a model file

With constants riding the flow relation, everything the intent API needs from Phase 2 is a set of
access paths for them to land on. The previous revision bought those with a dozen
`model.propagation` entries in `java-index.jsonl`. This revision writes the `summary` rows
directly, from the pass that writes everything else.

The reason is not that a data file cannot express the *static* half. It can, and the table below is
the same content it was. It is that the data file cannot express the other half, and splitting one
API across two mechanisms costs more than owning both. Two of the things this design reasons about
are known only inside the engine: a key recovered by `const_reaches`, and the manifest's string
alphabet, which is already an input relation. A propagation port is a static path in a JSON file; it
cannot be `.<extras>` followed by a symbol the fixpoint recovered. That is precisely why the
previous revision needed a separate IR rewrite standing beside the model file. Once one mechanism
has to mint paths out of recovered constants, the static half is a few dozen lines of the same
emitter rather than a second artifact with its own failure mode -- and the seam where a pass and a
data file had to agree on the spelling of `.<intent>` disappears with it.

**What replaces the matcher.** One scan of `IdMap::functions()` (`facts.rs:1464`), parsing each
`Lcls;->name(descriptor)ret` and matching it against a small built-in signature table. The table has
two matching modes, because the declared-type problem is real but narrow:

- **Pinned-class rows.** Methods whose receiver is the framework type itself keep the class in the
  key: `Landroid/content/Intent;`, `Landroid/content/ComponentName;`, or `Landroid/os/Bundle;`, plus
  name and descriptor. A call to `Intent.setAction`, `Intent.getData`, `Intent.putExtras`, or a
  `Bundle` accessor names the declared framework class in the dex method id, so dropping the class
  buys nothing and can accidentally give an app method a synthetic `.<action>` or `.<extras>` field.
- **Receiver-varying rows.** `getIntent` and `setIntent` match on name plus descriptor only. These
  are called on the app's `Activity` subclass, so the dex method id at the call site records that
  subclass rather than `Landroid/app/Activity;`. This is the two-row case a `parents` filter cannot
  express without enumerating every app activity.

The scan is manifest-independent and runs for every Java-family import where these method ids are
present, including bare `.dex`, `.jar`, and APK imports whose manifest is absent or fails to decode.
That is what makes it safe to remove the coarse Android `Intent` entries from `java-index.jsonl`:
the static API propagation surface does not depend on Phase 1 succeeding. Manifest-dependent work --
component inventory, filters, and send/receive pairing -- is a later step and can be skipped without
turning off these rows.

The residual imprecision is therefore limited to the receiver-varying rows: an app-defined
`getIntent()Landroid/content/Intent;` or `setIntent(Landroid/content/Intent;)V` method is
indistinguishable at this layer and gets the synthetic `.<intent>` treatment.

**The rows.** Written `source -> destination`; each line is one
`summary(f, dst_index, dst_path, src_index, src_path)` row, for every `f` the scan matched:

| matched method | class matching | summary row(s) |
| --- | --- | --- |
| `Intent.<init>(String)` | pinned to `Landroid/content/Intent;` | `Argument(1) -> Argument(0).<action>` |
| `Intent.<init>(String, Uri)` | pinned to `Landroid/content/Intent;` | `+ Argument(2) -> Argument(0).<data>` |
| `Intent.<init>(Context, Class)` | pinned to `Landroid/content/Intent;` | `Argument(2) -> Argument(0).<component>` |
| `Intent.<init>(String, Uri, Context, Class)` | pinned to `Landroid/content/Intent;` | the union of the three above |
| `Intent.<init>(Intent)` | pinned to `Landroid/content/Intent;` | `Argument(1) -> Argument(0)` |
| `setAction` / `getAction` | pinned to `Landroid/content/Intent;` | `Argument(1) -> Argument(0).<action>` / `Argument(0).<action> -> Return` |
| `setData`, `setDataAndType` / `getData` | pinned to `Landroid/content/Intent;` | the same, on `.<data>` |
| `setClass`, `setClassName`, `setComponent` | pinned to `Landroid/content/Intent;` | the class-name argument `-> Argument(0).<component>` |
| `ComponentName.<init>(Context, String)`, `(String, String)` | pinned to `Landroid/content/ComponentName;` | `Argument(2) -> Argument(0)` |
| `getIntent` / `setIntent` | receiver-varying: name + descriptor only | `Argument(0).<intent> -> Return` / `Argument(1) -> Argument(0).<intent>` |
| `getExtras` / `putExtras` | pinned to `Landroid/content/Intent;` | `Argument(0).<extras> -> Return` / `Argument(1) -> Argument(0).<extras>` |
| `Bundle` accessors and `putAll` | pinned to `Landroid/os/Bundle;` | bundle entries live at the bundle root |
| `Intent.createChooser(Intent, CharSequence)` | pinned to `Landroid/content/Intent;` | `Argument(0) -> Return` |
| every builder setter | pinned to its framework owner unless listed receiver-varying | `+ Argument(0) -> Return` (they return `this`) |

`putExtra` and `get*Extra` are deliberately absent. Their edge varies per call site, and a summary
is keyed by callee; the next section is the whole of that argument.

Six things make this work, and each is a place it could quietly fail instead:

- **The call rows exist and the summaries instantiate.** These are framework methods with no body in the dex, but the frontend
  records a VMT entry for every externally-referenced method (`dex/mod.rs:119-121`), so CHA
  resolves `Landroid/content/Intent;->setAction` to itself and codegen emits a `call` row
  (`codegen/mod.rs:536-545`). Without that row a summary is inert, because instantiation joins
  `summary(tgt, ..)` with `call(f, insn, tgt)` (`index_engine/mod.rs:1164`). This is the chain the
  existing `putExtra` entry already depends on. Before the full emitter lands, add a two-line
  fixture that calls a bodyless framework method with one pass-through summary and assert that the
  expected `assign_like` row exists after indexing. The pass's own match counts check only the first
  link in the chain; this assertion checks the end-to-end behaviour the intent rows rely on.
- **`formal_param` rows are not optional.** Model codegen pushes a `FormalType::ByRef` row for
  every index a summary names and says why: `locals` is seeded from formals, so a modelled function
  without them is "silently lost" (`model_matches.rs:126-137`). The query engine needs them for a
  second reason -- its actual-to-formal step probes `formal_param(callee, formal(n), _)`
  (`query_engine/mod.rs:448-460`) -- so a summary whose formals are undeclared is inert at index
  time *and* unreportable at query time. The pass pushes them exactly as codegen does.
- **The rows go in before the fixpoint, and that is load-bearing.** `.<action>`, `.<data>`,
  `.<component>`, `.<extras>`, `.<intent>` reach `model_paths` only through `summary_paths`, which is collected
  from `facts.summary` before the run (`index_engine/mod.rs:1023`, `:1437`). A pass that derived
  these same rows from an `intent_api(FunctionId, ..)` relation inside the fixpoint would get every
  join right and still lose the one-level concat that composes `.<intent>` with a program path --
  silently, as usual. Static rows are input rows.
- **The two android Intent lines in `java-index.jsonl` come out** (`:120-121`). Not because they become
  redundant but because they are wrong beside the new rows: `getStringExtra`/`getData`/`getAction`
  as `Argument(0) -> Return` drags the whole intent, extras included, onto the value that was read,
  and `putExtra` as `Argument(2) -> Argument(0)` re-conflates every key with every other. Removing
  that whole-object `putExtra` edge is most of the precision this phase is for, independently of
  key recovery. Model files keep working alongside the pass -- both write the same relation -- so
  hand-written overrides and app-specific wrapper entries are unaffected, which is what this route
  gives up nothing on.
- **Counting replaces `CTADL0004`.** A model generator that matches nothing gets a diagnostic; a
  scan that matches nothing is silent. So the pass logs matched functions and matched call sites
  per API row, and treats zero matched functions as an error when the `IdMap` contains any method id
  under `Landroid/content/Intent;->...` but the built-in table matched none of them. That test is
  expressed where the pass actually runs: the dex string pool is gone by link time, but the `IdMap`
  still contains every referenced method function. This is the "app that appears to send no
  intents" failure mode, and this is the cheapest place it can be caught. It also answers what the
  previous revision could only promise to "fail loudly" about. And it does one thing the matcher
  could not: because the scan sees every function, it can report the descriptors it *declined* -- an
  `Landroid/content/Intent;-><init>` overload the table does not list, a `set*`/`get*` on `Intent`
  nobody modelled. A table of API signatures is a checklist and checklists rot; this is the
  mechanism that says which entry is missing rather than leaving a construction path to lose its
  action in silence.
- **Port paths are a level shift, not a filter** (`docs/model-generators.md:567-590`). The
  semantics are the engine's, not the model loader's -- they fall out of `substitute_prefix` -- so
  they hold identically for a row the pass writes. A lumped `Argument(0).<extras> -> Return` read
  does see a keyed write, and the value arrives at `Return.<key>` rather than at `Return`. That is
  the correct behaviour for the two tiers below, and it is why a *source* placed on such a read
  needs `saturating` -- see open decision 4.

`createChooser` earns its row: a share send is usually
`startActivity(Intent.createChooser(target, title))`, the chooser intent is framework-built and
carries no recoverable constants of its own, and aliasing it to the target is what lets pairing
read the target's action. Without the row that send lands in the unresolved bucket, which is
defaulted off -- and `com.noto` contains exactly one `createChooser` call.

#### Key-precise extras, with no IR rewrite

`putExtra(k, v)` / `get*Extra(k)` with a recovered key is the shape a summary cannot carry: a
summary is keyed by callee and holds at every call site of it, so one site's key would become every
site's key -- the conflation keying exists to remove, restated. The edge has to be per site, which
means it is an `assign` on the site's own vertices: exactly the row summary instantiation would
have produced (`index_engine/mod.rs:1164-1170`), written directly.

The path convention is fixed here because every later extras API composes through it: a `Bundle`'s
entries live at the bundle root (`bundle.<k>`), while an `Intent`'s extras live under its
`.<extras>` field (`intent.<extras>.<k>`). `putExtras` and `getExtras` are the bridge between the two
worlds: `putExtras` maps the bundle root to the intent's `.<extras>`, and `getExtras` maps the
intent's `.<extras>` back to the returned bundle root. Methods added later should be checked against
that invariant before they get rows: `replaceExtras(Bundle)`, `Bundle.putAll(Bundle)`,
`getBundleExtra(String)`, and `putExtra(String, Bundle)` are the first cases likely to expose a
level mismatch.

The previous revision reached that row through an IR rewrite. It does not need to. By the time the
pass runs, everything the rewrite read off the statement is in `IndexFacts`:

- `actual_param(site, 0, FlowVertex(recv, ε))` and `actual_param(site, 2, FlowVertex(value, _))` --
  codegen emits one row per argument and one per return value at a negative index
  (`codegen/mod.rs:647-707`), so the *real* IR variables on both sides of the edge are readable
  from the fact base;
- `const_str_assign(site, FlowVertex(call_arg(insn, 1), ε), k)` for a literal key -- Phase 2's
  third emission hook is what makes a constant *argument* leave a trace at all, and this is the
  first thing that reads it.

One gap in that, worth naming because it is the same gap Phase 2 opened this section to close: an
argument that is *itself* a constant has no `actual_param` row at all, since `trans_exp` returning
`None` suppresses it (`codegen/mod.rs:690-692`). `putExtra("k", "literal")` therefore has no
variable to name on the value side. The pass roots that side at `call_arg(insn, 2)` instead --
where `const_str_assign` put the constant -- which is the vertex the propagation rules already
read.

So the rewrite becomes emission:

| call, key a literal at the site | rows the pass pushes |
| --- | --- |
| `putExtra(k, v)` | `assign(site, (recv, .<extras>.<k>), (value, ε))`, `paths(.<extras>.<k>)` |
| `get*Extra(k)` | `assign(site, (retval, ε), (recv, .<extras>.<k>))`, the same path row |
| `Bundle.put*(k, v)` / `Bundle.get*(k)` | the same two shapes on `.<k>`, not on `.<extras>.<k>` |
| key not a literal | the same two shapes on `.<extras>` (Bundle: on the receiver whole) |

Rooting the store at the receiver *variable* rather than at the call-argument pseudo-variable is
the part worth being deliberate about: `prog_store` is built from `facts.assign` before the
fixpoint (`index_engine/mod.rs:992-1000`) and gates the aliasing summary rule (`:1187`), which is
what carries a keyed write through an alias of a formal out of a helper. An edge that exists only
inside the fixpoint can never feed it. This is the one thing the IR rewrite bought that a rule
cannot, and emitting facts keeps it.

Everything else the previous revision had to say about the rewrite stops applying, because no
statement is rewritten: the pipeline slot between `ssa::transform_program` and
`ssa::propagate_copies`, `rets = [retval, throwval]` and SSA preservation, `mir::store_access_path`
and the `cap_path` re-anchoring, copy propagation never aliasing constants, frontend neutrality.
The `call` row survives at every extras site too, which is what dissolves most of open decision 4:
a user model on `getStringExtra` keeps firing exactly where it did.

Reusing the call's own site for these emitted `assign` rows is deliberate, because it preserves the
source span of the extras operation. It is also a fact shape codegen does not emit today: one site
will carry a `call` row, `actual_param` rows, callee metadata, and synthetic `assign` rows. Before
committing the emitter, check every consumer that assumes a site maps to one statement kind -- SARIF
step rendering, `tainted_var_at_insn`, the graphviz dump, and any inspect path that renders calls or
assignments -- and check that `prog_store` and `copy_edge` behave sensibly when the row's site is
also a call. If a consumer really depends on the old shape, the fallback is a fresh synthetic site
and a less precise span; do not silently accept a mis-rendered trace.

Two corrections to the previous revision's table, both about levels rather than syntax:

- **A `Bundle` key is not an extras key.** The old table rewrote `Bundle.put*(k, v)` to
  `recv.<extras>.<k>`, which double-wraps: `putExtras` already carries
  `Argument(1) -> Argument(0).<extras>`, so a bundle written at `.<k>` arrives on the intent at
  `.<extras>.<k>` -- correct -- while a bundle written at `.<extras>.<k>` arrives at
  `.<extras>.<extras>.<k>`, and the two spellings never meet. `getExtras` unwraps the same way.
- **The lumped tier is per site, not per callee.** Because the pass emits it as an `assign` at each
  site rather than as a summary row on `Intent.putExtra`, a site whose key is a literal gets the
  keyed row *only*. The two tiers stop coexisting at the same site, which is what makes keying an
  actual precision gain rather than an addition on top of the conflation it was meant to remove.

**The composition claim that is still an argument rather than an observation.** A keyed write
followed by a lumped read is clean: `substitute_prefix` walks `.<extras>` down to
`.<extras>.<key>` and the value arrives at `Return.<key>`. A *lumped* write followed by a *keyed*
read is not the mirror image -- `.<extras>` is not an extension of `.<extras>.<key>`, so the
forward rule does not fire and the flow arrives only through the source-path rule, one level
deeper than the writer put it. Whether that still reaches a given sink depends on the sink port
materializing over reachable paths. Nothing above settles it, and it is cheap to settle: a nightly
case with a keyed write and a lumped read, and its reverse. Write them before tuning key recovery,
because the answer decides whether an unrecovered key is a precision loss or a missed flow.

#### When the key is not a literal: the one rule that mints a path

R8 inlines `static final String` and Kotlin `const val`, so the overwhelming majority of keys are
literal operands at the site and never leave the emitter above. What is left is a key that arrives
in a variable -- a companion-object getter, an `sget`, a key passed into a helper. `const_reaches`
already knows those values; the rule is the general form of the emitter:

```
extras_key_path(f, insn, val_n, pk) <--
    extras_site(f, insn, ExtrasOp::Put, key_n, val_n),
    const_reaches(f, call_arg!(*insn, *key_n), P_EMPTY, k),
    let pk = Path::from_accesses([Symbol(EXTRAS), Symbol(k.clone())]);

assign_like(f, call_arg!(*insn, 0), pk, call_arg!(*insn, *val_n), Path::empty()) <--
    extras_key_path(f, insn, val_n, pk);

paths(pk)              <-- extras_key_path(_, _, _, pk);
paths(q.concat(pk))    <-- extras_key_path(_, _, _, pk), model_paths(q);
paths(pk.concat(q))    <-- extras_key_path(_, _, _, pk), model_paths(q);
```

(and its mirror for `ExtrasOp::Get`, whose head writes the site's `Return` call-arg from
`call_arg(insn, 0)` at `pk`.) `PathSegment::Symbol` holds a `ctadl_ir::Symbol`
(`ctadl-ir/src/mir/mod.rs:193-198`, `:155`), which is the type `const_str_assign` already carries,
so minting the path is a pointer copy into an existing interned segment -- no allocation, no
re-interning, and the resulting `Path` compares by the same equality every other path uses.

Three things this has to answer, and together they are why it is a *second* mechanism rather than
the only one:

- **Termination.** The comment on the path rules is explicit that this is the hazard: "Paths must
  remain finite so we shouldn't add paths from constructed summaries directly"
  (`index_engine/mod.rs:1119-1120`). The minted set here is bounded before the run starts. Every
  key symbol comes from `const_str_assign`, a fixed input relation; the shape is fixed at two
  segments; growth in `const_reaches` adds (vertex, symbol) pairs and never new symbols; and
  nothing minted feeds back into the constant set. So the minted paths are a subset of
  `{.<extras>.k : k in the input constants}` -- finite, and countable before the fixpoint runs, which
  is what makes it checkable rather than merely argued.
- **The concat rules have to be replayed by hand, against `model_paths` only.** `.<intent>.<extras>.<key>`
  is what the receiving component reads, and today it exists because the one-level concat rule
  pairs a model path with a program path (`:1133-1135`). A minted path cannot join that bucket
  from either side: putting it in `model_paths` crosses it with *every* program path -- the
  `|model| x |program|` blowup the comment at `:1019-1022` exists to prevent -- and `program_paths`
  is seeded, not derived. So the rule mints exactly the compositions the concat rule would have
  produced against `model_paths`, a few dozen rows per key, and nothing else. The fact route needs
  none of this: a path in `facts.paths` *is* a program path and the existing rule composes it.
- **A rule cannot suppress the lumped edge.** "No key was recovered at this site" is negation, and
  `const_reaches` lives in the recursive SCC with `call` and `assign_like`, so `!const_reaches(..)`
  is the case the stratification argument above says will not compile at all ("use of aggregated
  relation ... cannot be stratified", `ascent_mir.rs:288`). At a site whose key only the fixpoint
  recovers, the keyed edge is therefore *added* to a lumped edge that is already there, and that
  site keeps the conflation. Additive precision, not replaced precision -- which is exactly the
  asymmetry the fact route avoids by making the choice in Rust.

Recommendation: ship the emitter, and gate the rule on a count -- extras sites whose key is not a
literal, measured on the Phase 4 fixture. If that number is small, the rule is a precision
extension nobody needs to turn on; if it is not, it is two rules in a fixpoint that already runs.

#### Reporting

Constant recovery fails silently -- it yields fewer links, which reads exactly like an app that
sends no intents -- so the counts are the only thing that catches a regression. At `info`, per
index: intent frames gated, `const_str_assign` rows seeded, `const_reaches` rows derived and the
ratio between them (the same "relation increase" shape `IndexStats::log` already uses,
`index_engine/mod.rs:326-347`), keyed vs. lumped extras sites, and -- the count that used to be a
diagnostic -- API functions matched by the signature scan and call sites they resolve, per row of
the summary table. The lumped share is the imprecision metric; the seeded-to-derived ratio is the
"did the propagation actually propagate" metric, and a ratio near 1.0 means the `paths` gate is
dropping everything. A summary-table row that matched zero functions in an Android import is the
one count that should fail the index rather than log.

### Phase 3 -- The intent linking pass

A built-in index-time pass, structured like `ctadl-ascent/src/languages/jni.rs`: an observer that
collects during the import loop, and a `link` step that runs after it, once every import's
functions are interned in one `IdMap`. What it does *not* do is decide any pairings. Phase 2 moved
that join into the engine, so this pass generates facts and reads results:

| step | when | produces |
| --- | --- | --- |
| observe | during the import loop, IR in hand | the manifest tables, the class hierarchy, and Android-relevant Java call sites for this import |
| link | after the loop, before the fixpoint | the intent-API `summary` + `formal_param` rows and the per-site extras assigns (Phase 2), then `intent_send`, `intent_filter`, `intent_component`, `extras_site`, the bridge sites, the delivery assigns |
| report | after the fixpoint | counts read off `intent_pair` |

The split between the first two is forced: receive-site resolution needs the manifest and the
hierarchy, both per-import and both gone by the end of the loop -- `program_info` is dropped with
each import -- so the observer takes them while the IR is in hand, exactly as
`jni_observer.observe` takes the native-method table (`cli/mod.rs:258`). The send side also needs a
small IR-time observation: a resolved `call` row does not retain the declared receiver class, and
under `CallResolutionStrategy::Mixed` an ambiguous Java call can be represented only as
`callee_info`. The observer therefore records Android-relevant Java calls as compact tuples --
containing the caller function, original instruction site, declared receiver class, method name,
descriptor, and whether codegen inserts an instance receiver -- and drops the IR. The link step uses
those recorded tuples plus the final `IdMap`; it does not rediscover send sites from resolved call
edges.

**Send sites, observed before the IR is dropped.** The observer matches Android framework sends on
the declared receiver class, method name, and descriptor. Matching only the resolved callee name is
not enough: it misses unresolved/ambiguous Java calls and cannot distinguish
`LocalBroadcastManager.sendBroadcast` from manifest broadcasts. The observed call-site table is the
only source of `intent_send` rows.

| method (matched on name + descriptor) | intent argument |
| --- | --- |
| `startActivity`, `startActivityForResult`, `startService`, `startForegroundService`, `sendBroadcast`, `sendOrderedBroadcast`, `bindService` | `Argument(1)` |

Matching on name *plus descriptor* is what answers "which argument is the intent" across the
overloads -- `startActivityForResult(Intent, int)`, `bindService(Intent, ServiceConnection, int)`,
`sendBroadcast(Intent, String)` -- and what rejects an app-defined method that merely shares a
name. It falls out that the intent is `Argument(1)` for every instance-method send: codegen inserts
the receiver as `Argument(0)` (`codegen/mod.rs:503`) and the intent is the first declared parameter
of all of them. `Intent.createChooser` is deliberately absent from this table. It is a framework
factory whose `Argument(0) -> Return` summary preserves the wrapped target intent for a later
`startActivity` call; it is not itself an ICC send and must not mint an `intent_send` row.

The receiver's declared type still varies too much to enumerate as a hard allow-list (`Activity`,
`Context`, `ContextWrapper`, `Fragment`), which is the same problem the `iterator` entry in
`java-index.jsonl` documents at length. It is still used for exclusions and diagnostics.
`LocalBroadcastManager.sendBroadcast` delivers only to receivers registered on that manager, never to
manifest receivers, so pairing it against the manifest fabricates links; skip send sites whose
declared receiver type is `LocalBroadcastManager` (both the `androidx` and `android.support`
descriptors). Add a fixture proving this exclusion survives `Mixed` strategy, where a resolved
`call` row may not exist.

**Receive sites.** The manifest names a component *class*; two resolution steps turn that into
functions to bridge into.

- *Hierarchy.* Components inherit entry methods more often than they override them, and the
  framework's implementations are not in the dex. Walk up from the component class and bridge to
  the nearest app-defined override of each method in the table below; where no override exists in
  app code, there is nothing to bridge into and the method is skipped. This is also what makes
  `IntentService` work at all: a subclass overrides `onHandleIntent`, never `onStartCommand`, so
  framework-mediated indirections get their own rows.
- *Aliases.* An `<activity-alias>` is a component in its own right -- its own name, its own
  exported status, its own filters -- that delivers to its `android:targetActivity`. Fold each
  alias's filters into the target activity's filter set; the previous ctadl's pseudo-filter
  treatment (section 7) extends to this without new machinery.

| tag | methods bridged | where the intent lands |
| --- | --- | --- |
| `<activity>` | every lifecycle override: `onCreate`, `onStart`, `onRestart`, `onResume`, `onPause`, `onStop`, `onDestroy`, `onNewIntent` | `Argument(0).<intent>`; for `onNewIntent`, also `Argument(1)` |
| `<receiver>` | `onReceive` | `Argument(2)` |
| `<service>` | `onStartCommand`, `onBind`; `onHandleIntent` / `onHandleWork` for `IntentService` / `JobIntentService` subclasses | the intent parameter, `Argument(1)` |

**Why every lifecycle override, not just `onCreate`.** The analysis is compositional: nothing in
the app calls `onResume` -- the framework does -- so an intent delivered only into `onCreate`'s
scope is invisible everywhere else in the component. Bridging each lifecycle override delivers
the same intent vertex into each method's `this`, and ordinary summary propagation carries it
into anything those methods call. The cost is a handful of bridge sites per pair instead of one.

`<intent>` is a synthetic field: `Activity`'s real backing field lives in the framework, outside
the dex, so the symbol is invented. Phase 2 writes both accessors as summary rows -- `getIntent`
reads `Argument(0).<intent>`, `setIntent` writes it -- and this pass's bridge writes the same
symbol. Both are now the same pass reading the same Rust constant, so the seam the previous
revision opened (a data file and a pass having to agree on a spelling, with `--no-default-models`
or a scoping `in` block enough to silently break the agreement) is closed by construction. What
replaces "fail loudly if the model file is not loaded" is the scan count: if the `getIntent` row
matched no function, the pass says so and fails, rather than producing an app that appears to read
nothing out of its intents.

**Extras.** Phase 2 has already put extras on `.<extras>`, keyed where the key was recovered and
lumped where it was not. What this phase adds is only the delivery path -- the bridge writes the
intent into `Argument(0).<intent>`, and ordinary access-path composition makes
`this.<intent>.<extras>.<key>` reachable from the receiving component. A lumped read of `.<extras>`
sees a keyed write and delivers it one level down, at `Return.<key>`, because a summary's port pair
is a prefix substitution rather than a filter (`docs/model-generators.md:567-590`); the reverse
pairing is the asymmetric one Phase 2 flags for a nightly case.

One caution survives, and it stays inside the pass: the declared-type trap. Phase 2's signature
scan uses class-pinned matching for framework-owned methods and reserves name-plus-descriptor
matching for the receiver-varying `getIntent` / `setIntent` rows, where a `parents` filter would
have needed `Activity` listed alongside every caller-declared subclass.
The other two cautions stay retired: the jvm frontend emits the same symbolic fields
(`jvm/mod.rs:658`), so synthetic fields are portable across both Java frontends, and dex field
symbols already carry dots inside `<...>` pretty names, so keys embed without the previous ctadl's
`ord()` mangling.

**Pairing.** Three cases, in descending confidence:

- *Explicit.* `new Intent(ctx, Foo.class)`, `setClass`, `setClassName`, `setComponent` name the
  target class outright. Low fan-out, high confidence. This is the default. For app-code sends,
  explicit pairing is allowed regardless of `android:exported`: non-exported components are still
  reachable from inside the same package.
- *Implicit.* A constant action string on the intent joins against a manifest
  `<intent-filter><action android:name=...>`. Correct, but prone to fan-out: every sender of
  `ACTION_VIEW` links to every activity that filters it.
- *Unresolved.* No constant action was recovered. The sound answer links the send to every exported
  component; the useful answer links it to nothing. Put this behind a flag, defaulted off.

Do not confuse this internal ICC pairing with the external attack-surface report. Phase 1 reports
which components another app or `adb shell am start` can reach, using `android:exported`, default
export rules, permissions, and disabled state. Phase 3's initial linker pairs sends observed in the
app's own bytecode, so it does not use `exported="false"` as a pruning rule. External-entry modelling
is a separate source relation: if a later query wants to model an outside app sending an intent into
this APK, it should consume the exported component/filter views directly rather than pretending the
observed app-code send relation contains external callers. Synthetic negative tests for
non-exported components belong to that external-entry surface, not to internal send-site pairing.

**How a constant attaches to a send site.** Through `const_reaches`, in the engine. The intent
argument of a send site is the call-arg pseudo-variable `call_arg!(insn, 1)`, and the
actual-parameter edge carries a constant sitting at `(i, .<action>)` onto
`(call_arg(insn, 1), .<action>)` under the same `substitute_prefix` step everything else uses. The
two pairing rules in Phase 2 read exactly that. No IR is consulted, no def-chase runs, and the
chained-builder case (`new Intent(A).setData(u).putExtra(k, x)`) needs no special handling: each
setter's summary row puts its argument on the receiver's field and returns `this`, so the
constants accumulate on one object whatever order the chain is written in.

Two normalizations sit on the explicit rule, and both happen in Rust when `intent_component` is
built, not in the join. A `const-class` constant is already a type descriptor (`Lcom/foo/Bar;`)
while `setClassName` takes a dotted source name, so the manifest side is normalized to both
spellings rather than normalizing constants at join time. The two key spaces cannot collide -- a
type descriptor is never a manifest action string -- which is why it costs nothing that the fact
base does not distinguish a `const-string` from a `const-class`.

What the join misses is Phase 2's third bucket: a constant that originates in a callee
(`buildIntent()` factories, Kotlin companion-object getters, action strings read back through
`sget`) and a send site sitting in a callee of the frame that built the intent. Both need the
rules in open decision 5, which are now two rules in a fixpoint that already runs. Count
unresolved send sites as their own number rather than assuming the bucket is small.

**Emission.** Almost all of it happens before the fixpoint, and it is smaller than the previous
revision's version because the pass no longer emits one bridge per pair -- it emits one *site* per
send site and lets the engine attach callees to it. Per send site: mint the site
(`source_info.add_insn_site`), push the delivery `assign` rows for its kind, push one `intent_send`
row. Per component entry method: push an `intent_filter` or `intent_component` row. Per matched
intent-API function, from Phase 2's scan: its `summary` rows and the `formal_param` rows they
require. Per extras call site: the keyed or lumped `assign` pair and its `paths` row. That is the
whole emitter, and it needs neither `emit_bridge` (`languages/jni.rs:868`, whole-parameter ports
only) nor the declarative-bridge emitter's sub-path machinery
(`codegen/model_matches.rs:343`), because a sub-path port is just an `assign` with a non-empty
destination path when you are writing facts directly.

The freshness caution from those emitters still applies, one level up: call-argument
pseudo-variables key on the instruction id, so two *send sites* must never share a bridge site or
their intents alias. Two *receivers of one send site* sharing one is not the same thing and is
correct -- it is an ordinary multi-target call site carrying one intent.

**Reporting.** Log a per-pass count line at `info`, the way the JNI bridge does
(`jni.rs:671`). A mis-paired bridge yields fewer flows rather than an error, which reads exactly
like a clean app; the count is the only thing that catches it. The counts now come *back out of*
the fixpoint rather than being tallied as the pass emits, so `intent_pair` has to survive into
`IndexResult` alongside the derived `call` -- one more column, and the one that makes the
explicit / implicit / unresolved split reportable at all.

**Why a built-in pass and not generated `model.bridge` entries.** Generated entries would need no
new pass code and would let users override pairings by hand. But a bridge's two sides are selected
by static `where` constraints, and intent pairing depends on constants recovered from the program.
That is still true -- what has changed is where the dynamic half lives. It is not pass code any
more; it is two rules in the engine, fed by relations the pass generates. Declarative `bridge`
remains available for hand-written overrides of what the join gets wrong, and it is now the *only*
place a human names a specific pairing. The same split answers the matching question for Phase 2's
API rows: the pass generates them because it can count what it matched and fail on zero, and a
user's `model.propagation` entries keep writing the same relation beside them for anything the
built-in table does not name.

### Phase 4 -- Validation against a real app

`xtask/tests/dex/com.noto_54.apk` is already committed, already owned by xtask, and already wired:
`xtask regression` takes it through `--dex-apk`, the Nix `regression` derivation passes it
explicitly, and `ctadl-ascent/tests/cli.rs:37` reads it for an import smoke test. Today it has one
job -- `check_apk` (`xtask/src/dex.rs:93`) parses every `classes*.dex` and proves dex-reader does
not crash on a large multi-dex app. This phase gives it a second job: known-answer validation of
the intent pipeline, as `intent:*` cases emitted next to `dex:apk` from the same `Option<&Path>`
and Skipping the same way when the APK is absent. No new fixture, no flake change, no CI change.

That the fixture is a frozen binary is what makes this worth doing. Every count below is a
legitimate known answer rather than a moving target, so these checks can assert exact equality
where a live app would force a threshold.

**The app, measured.** `com.noto` v2.2.3, minSdk 21, targetSdk 33, two `classes*.dex`, a 509-line
manifest:

| tag | count |
| --- | --- |
| `<activity>` | 4 |
| `<activity-alias>` | 10 |
| `<receiver>` | 13 |
| `<service>` | 7 |
| `<provider>` | 1 |
| `<intent-filter>` | 37 |
| `<action>` | 42 |

**Decoder ground truth, differentially against `aapt`.** This mirrors `dex:baksmali`, which checks
dex-reader's disassembly against the reference disassembler rather than against our own
expectations. The reference here is `aapt dump xmltree <apk> AndroidManifest.xml`, and it costs no
new dependency: the flake already pins `androidenv` build-tools 30.0.2 for `dx`, `aapt` ships in
the same directory, and the `regression` derivation already prepends that directory to `PATH`.
Compare normalized element tree shape, attribute names, and attribute values; Skip on
`exec::which("aapt").is_none()` exactly as the baksmali case does. This is the check that catches
AXML bugs at the byte level, where they are legible, instead of at the join level, where they
present as an app that merely appears to have no components.

Two decoding requirements this manifest forces, both worth a named assertion because both are easy
to get wrong in a way that still parses:

- Booleans are `(type 0x12)` with value `0xffffffff` for true and `0x0` for false, not the strings
  `"true"`/`"false"`. Every filtered component here carries an explicit `android:exported`, as
  targetSdk 33 requires, so a decoder that mis-reads the encoding inverts the entire exported set
  rather than failing.
- `android:enabled` on the three WorkManager services and on `ConstraintProxyUpdateReceiver` is a
  *resource reference* (`@0x7f050002`), not a literal. It cannot be resolved without
  `resources.arsc`, which this design never reads. The Phase 1 triple store keeps values verbatim
  and so accommodates it; the typed view derived from it must be able to say "reference,
  unresolved" rather than defaulting to enabled or to disabled.

**The component surface.** Phase 1's deliverable is the `ctadl inspect` report, so this is where it
gets its known answer. Beyond the counts above, assert the classification by name:

- *Exported with filters:* `AppActivity`, `components.TransparentActivity`, both widget config
  activities, both widget providers, `QuickNoteTileService`, `work.impl.diagnostics.DiagnosticsReceiver`,
  `profileinstaller.ProfileInstallReceiver`.
- *App-owned but `exported="false"`:* `note.NoteReminderReceiver` and `vault.VaultReceiver`. These
  are the negative half of the report -- a component reachable only from inside the app must not be
  listed as attack surface.
- *Exported behind a permission:* `DiagnosticsReceiver` and `ProfileInstallReceiver` behind
  `android.permission.DUMP`, `QuickNoteTileService` behind `BIND_QUICK_SETTINGS_TILE`, both widget
  services behind `BIND_REMOTEVIEWS`. Phase 1 claims a component behind a signature-level
  permission is exported in name only; this is the fixture that makes the claim testable.

**Name normalization, with a built-in negative.** The strongest single assertion the fixture
offers, and it comes free:

> Of the 35 component names in the manifest, exactly 25 resolve to a type descriptor in
> `classes*.dex`, and the 10 that do not are exactly the 10 `<activity-alias>` names.

`Lcom/noto/app/AppActivity;` is in the string table; `Lcom/noto/app/SanguineSun;` and its nine
siblings are not, because an alias is a manifest-only name with no class behind it. One number
therefore does three jobs. A normalization bug pushes the miss count above 10. An alias-handling
bug that treats aliases as classes leaves 10 dangling components instead of 10 folds -- same count,
different set, which is why the check must assert the *identity* of the 10 and not just how many.
A dex-lookup bug pushes it to 35. Phase 1 notes that getting normalization wrong yields zero
matches and no error; this is the check that tells the two apart.

**Alias folding.** All 10 aliases carry `targetActivity="com.noto.app.AppActivity"` and the same
two filters `AppActivity` declares for itself -- `MAIN`/`LAUNCHER` and `SEND` with `text/*`. Phase 3
folds alias filters into the target's filter set, so here folding must be idempotent: `AppActivity`'s
effective filter set is unchanged afterwards, and the 10 aliases contribute zero additional bridge
targets. A fan-out bug shows up as an 11x, which is not a number anyone squints at. Getting a
tenfold duplicate for free is rare and worth spending.

All 10 are also `enabled="false"`. Do not skip disabled-in-manifest components: an app can enable an
alias at runtime through `PackageManager.setComponentEnabledSetting`, so treating them as unreachable
would be a soundness loss. The inspect report should still display the disabled state, but pairing
keeps the component in the candidate set. This fixture pins that choice by asserting the aliases are
reported as disabled but still folded into the target activity's effective filter set.

**Send sites and the fan-out budget.** Both halves of a flow are present in `classes.dex`:
`startActivity`, `startActivityForResult`, `sendBroadcast`, `startService`, `startForegroundService`,
`bindService` on the send side, with `Intent.createChooser` present as a wrapper factory rather than
a send; `getStringExtra`, `getParcelableExtra`
and `getData` on the receive side. So the fixture exercises pairing, not just one end of it.
(`startForegroundService` is absent from Phase 3's send-site list above and has been added there --
finding it here is what this phase is for.)

The single `createChooser` call is worth a named case rather than a line in a count. A chooser
wraps the real intent -- the started intent is framework-built with action `ACTION_CHOOSER` and the
target rides inside it -- so without Phase 2's `Argument(0) -> Return` row for it, the send resolves
to nothing and lands in the unresolved bucket, which is defaulted off. If this call is noto's share
path, it is also the hand-verified flow this phase pins below. Assert that the chooser's target
intent, not the chooser, is what pairing reads.

Two measurement consequences of Phase 2, both simpler than in the revision that rewrote IR. First,
nothing is rewritten out of the call facts: every `putExtra` and `get*Extra` site keeps its `call`
row, so the fixture's total extras call sites is a straight count off `facts.call`. The pass reports
three extras numbers rather than pretending the optional recovered-key rule is a partition:
literal-key sites that got a keyed fact row, no-literal sites that kept the lumped fact row, and
lumped sites that the in-fixpoint rule later keyed additively. With the optional rule off, the first
two numbers partition total extras call sites exactly. With it on, the third number is expected to
overlap the lumped bucket because stratification prevents retracting the fallback edge. Report
Bundle sites separately from Intent sites, since literal detection is syntactic for both but the
in-fixpoint key rule is gated on `intent_frame` and therefore covers a narrower set. The keyed share
is separately assertable as emitted `assign` rows whose destination path has two segments under
`<extras>`, and the minted key set is assertable by name -- `com.noto`'s extra keys are in the
string pool. Send-site numbers are unaffected either way. Second, the intent-API rows are no longer
a generator file, so the check is not "no `CTADL0004`" but the scan's own table. This fixture's
`classes.dex` string pool has been confirmed by hand to contain `setAction`, `getAction`,
`setClassName`, `setComponent`, `createChooser`, `putExtra`, `getStringExtra`, `getIntent`,
`setIntent`, `getExtras`, `putExtras`, `setData`, `getData`, and on the receive side `onNewIntent`
and `onHandleIntent`, so pin a nonzero matched-function count for exactly those rows and let the
rest be zero. A renamed descriptor is then a failing count rather than a quieter result. Assert also
that the two android Intent lines are gone from `java-index.jsonl`, and that no summary row on
`Landroid/content/Intent;` has an empty destination path -- a resurrected whole-object entry is
exactly the regression that would re-conflate every key while every count above stays green.

The constant relation gets its own known answers here, because it is the layer with no other
visible surface: the number of intent frames gated, the number of `const_str_assign` rows seeded
inside them, and the number of send sites at which `const_reaches` yields an action or a component.
The last is the one that moves when Phase 2 regresses, and it is the numerator of the unresolved
count below. One more number belongs here and decides open decision 4 by itself: extras sites whose
key argument is *not* a literal at the site. If R8 leaves that near zero on a real Kotlin app, the
in-fixpoint keyed rule is an extension nobody has to turn on.

Record four numbers per run, all read off `intent_pair` after the fixpoint, and fail on movement
outside a band: send sites matched, explicit pairings derived, implicit pairings derived, and send
sites with no pairing at all. The fourth is the one that decides open decision 5 -- it is the only
measurement that says whether keeping the constant rules intraprocedural is costing real links --
and when it moves, the way to act on it is to hand-classify a sample by disassembly into Phase 2's
three buckets, because the counter alone cannot tell "the constant came from a factory" from
"there was no constant".

One assertion belongs here that could not exist in a design where pairing happened outside the
engine: the *derived* `call` rows. `call.parquet` now holds codegen's calls plus JNI bridges plus
intent bridges, and the intent share is exactly `intent_pair`'s row count. Assert the identity. A
call-graph table that grew by a different number than the pass thinks it emitted is the signature
of a bridge site being shared, or of the fixpoint deriving a pairing the report never saw. A band rather than an equality, because Phase 2's
constant recovery will move the explicit/implicit split as it improves, and a hard equality would
turn every precision gain into a red build. Log them at `info` the way the JNI pass logs its count
(`jni.rs:671`). This is the measurement open decision 2 asks for, on the app it asks for it on.

**Flow assertions cannot use source lines here.** The regression harness asserts `expected_lines`
and maps DEX back to source through a linemap, and that mechanism does not apply to this APK:
`classes.dex` carries 336 `debug_info_item`s against 38,006 `code_item`s, and `classes2.dex` 333
against 4,087. R8 stripped the line tables; roughly 1% of methods retain one. `intent:*` cases
therefore cannot be `Kind::Dex` cases -- `read_expected_lines` requires the key
(`xtask/src/assertions.rs:15`) and there would be nothing truthful to put in it. Assert on
component and method identity instead, read off the index facts or the SARIF logical locations.
Settle this before writing the case runner, not after.

**Flow-semantics validation.** Inventory and budget are what the APK is good at; source-to-sink
semantics need source-built cases with ground truth in the test tree. Phase 4 therefore has three
validation tracks rather than one:

- `com.noto_54.apk` remains the real-app regression for manifest decoding, component inventory,
  API-scan counts, send-site counts, derived pairing counts, and one hand-verified R8-shrunk flow.
- Purpose-built cases under `nightly/tests/` cover the flow matrix directly: explicit activity
  delivery, implicit action delivery, activity alias folding, receiver delivery, service delivery,
  `getIntent`, `onNewIntent`, `putExtra`/`get*Extra` keyed extras, `Bundle` round trips through
  `putExtras`/`getExtras`, and chooser wrapping.
- The first pinned DroidBench ICC / ICC-Bench slice runs here too. The full external suite remains
  Phase 5, but Phase 3 is not considered complete until this initial independent slice exists and is
  either passing or explicitly xfailed with tracked reasons.

Those synthetic cases assert component and method identity rather than source lines when they run
against compiled Android bytecode. The first required cases are small: one explicit intent carrying
a tainted extra into an activity read, one implicit action carrying the same extra through a manifest
filter, one broadcast receiver case, one service case, one chooser case, and one `Bundle` case.
Non-exported-component negatives are external-entry tests, not internal app-code send tests: they
assert the exported attack-surface facts produced from the manifest, or a later explicit external
sender relation, rather than pruning ordinary intra-app ICC.

DroidBench's ICC suite and ICC-Bench expand in their own phase below. The initial slice is a gate for
the main feature; the larger suite is what keeps the feature honest against known ICC patterns we did
not invent for this implementation.

**One hand-verified end-to-end flow.** One real flow through `com.noto` is still worth having,
because no synthetic case shows that the pass survives R8-shrunk Kotlin. Derive it once by
disassembling with baksmali, which the regression environment already provides, and pin the result;
the manifest points at two candidates, the `SEND`/`text/*` share path into `AppActivity` and the
`PROCESS_TEXT` path into `TransparentActivity`. Record in the case file how the answer was derived,
since nobody can re-derive it from source.

**What this fixture cannot validate.** Worth stating so the gaps get synthetic cases rather than
false confidence:

- *Relative component names.* Every `android:name` here is fully qualified, so the `.MainActivity`
  path through the normalizer is never exercised.
- *Cross-dex resolution.* All 25 real component classes live in `classes.dex`; `classes2.dex`
  contributes none.
- *Split APKs.* This is a single APK, so the base-manifest selection Phase 1 describes for bundles
  is untested by it.
- *Programmatic receivers and `PendingIntent`*, which are Phase 6 anyway.

### Phase 5 -- Expanded external ICC validation

DroidBench's ICC suite and ICC-Bench are explicit validation inputs for this feature, not optional
reading. The first pinned smoke slice gates Phase 3/4 completion and runs with the regular
`xtask regression` path. The expanded Phase 5 suite lives under `nightly/tests/android-icc/` and is
run by the nightly GitHub workflow, not by the default fast CI path. The suites cover a broader
matrix of inter-component communication patterns than `com.noto` or a handful of local cases can,
and they give the implementation a regression surface that is not shaped by CTADL's own design.

The testing framework for them should be separate from the small source-built nightly cases in Phase
4:

- **Fixture acquisition.** Add an xtask-managed fixture step for `nightly/tests/android-icc/` that
  either vendors pinned DroidBench ICC / ICC-Bench APKs or downloads/builds them from pinned
  revisions in the Nix nightly environment. Prefer source-built fixtures when feasible, but pin APK
  hashes either way so expected answers are stable.
- **Case manifest.** Keep expected answers in a CTADL-owned manifest file under
  `nightly/tests/android-icc/`, keyed by fixture name.
  Each case records the expected sender component/method, receiver component/method, ICC kind
  (activity, receiver, service, result-back when implemented), action/component match kind, and
  whether extras should flow. Do not assert source line numbers; assert component and method identity
  plus the presence or absence of a taint path.
- **Harness shape.** Add an `xtask` runner namespace for Android ICC cases rather than forcing them
  through the existing `Kind::Dex` `expected_lines` path. The runner imports the APK, indexes it,
  runs the query model for that fixture, and checks SARIF logical locations or index facts against
  the case manifest. It must be filterable locally, and the expanded suite is invoked from the
  nightly workflow; only the small Phase 4 smoke slice belongs in regular regression.
- **Suite slicing.** Start in Phase 4 with the smallest subset that covers explicit activity,
  implicit action, broadcast receiver, started service, bound service if supported, extras,
  alias/filter cases, and negative non-exported external-entry cases. Phase 5 grows that subset.
  Mark unsupported categories in the manifest with a reason and expected status (`xfail`,
  `unsupported`, or `phase-6-plus`) rather than silently omitting them.
- **Reporting.** Summarize pass/fail/xfail counts by ICC kind and by feature bucket. A failure should
  say whether pairing was missing, pairing was excessive, or the taint path through extras/lifecycle
  delivery was missing.

The harness itself and the first external cases are part of the main feature gate; larger coverage
can grow afterward. Phase 5 is therefore not permission to defer all external validation -- it is the
nightly expansion of the already-required harness. Keep the regular regression slice small enough to
run by default, and keep the full DroidBench / ICC-Bench end-to-end suite in the nightly hook.

### Phase 6 -- Precision

In rough order of value:

- **Result-back flows.** `setResult(code, intent)` in the callee pairing with `onActivityResult` in
  the caller. The previous ctadl's rule for this was written and marked untested; treat it as new
  work rather than a port.
- **Programmatic receivers.** `registerReceiver(receiver, new IntentFilter(action))` registers a
  filter that never appears in the manifest.
- **Easy constructed string constants.** Some apps build action strings from literals through
  `String.concat`, `String.valueOf`, or simple `StringBuilder.append(...).toString()` chains instead
  of leaving a single `const-string` at the intent site. A normal propagation model can carry the
  fragments but cannot compute the joined string, so these sends remain unresolved under the Phase 2
  constant rules. If this becomes worth closing, add a small bounded constant-string synthesis pass
  before the main fixpoint: recognize literal-only same-function string-building sites, cap length
  and fan-out, emit the synthesized value as another `const_str_assign`, and let `const_reaches` and
  the existing intent pairing rules consume it. Do not make this a general string analysis: skip
  variables, branches, loops, `String.format`, resource lookups, and interprocedural builders until
  a fixture shows they matter.
- **`PendingIntent`**, and `<provider>` authorities if content-provider flows matter.

## 6. Open decisions

1. **Implicit-intent fan-out tolerance.** This decides whether Phase 3 produces a usable result set
   or drowns a real app in links. Worth measuring on a real APK before committing to the default;
   Phase 4 is where that measurement happens, on the APK the regression suite already carries.
2. **Whether the in-fixpoint keyed-extras rule ships, and what the lumped tier does beside it.**
   The version of this decision that mattered most is gone: nothing is rewritten out of the fact
   base any more, every extras site keeps its `call` row, and a user model that
   `signature_match`es `getStringExtra` -- sourcing on intent data is a common taint configuration
   -- fires exactly where it did before. What is left is narrower and is forced by stratification.
   At a site whose key the pass can see as a literal, it emits the keyed edge *instead of* the
   lumped one, because that choice is made in Rust. At a site whose key only `const_reaches`
   recovers, no rule can withhold the lumped edge -- "no key was recovered here" is negation over a
   relation in the recursive SCC, which will not compile -- so the keyed edge is additive and that
   site keeps its conflation. Decide on the count: how many extras sites in the Phase 4 fixture
   have a non-literal key. If the number is negligible the rule is optional; if it is not, ship it
   knowing it adds precision without removing imprecision. Independently of that, document the
   `saturating` source on `getIntent`'s `Return` as the recommended way to source on intent data:
   a saturating vertex taints anything loaded off it regardless of path
   (`query_engine/search.rs:83-86`), so it covers keyed and lumped extras alike, and it is now an
   addition to the documentation rather than a migration users are forced into.
3. **Whether the constant rules stay intraprocedural.** Running inside the fixpoint already buys
   cross-procedure pass-through through summaries, so what is left open is only the bucket where a
   constant *originates* in a callee. Two extensions, and they differ sharply:
   - *Up-direction only* -- a `const_summary(f, formal, path, symbol)` relation meaning "f writes
     this literal into that out-formal", instantiated at each call site of `f`. Two rules, no new
     inputs, and it closes the factory case (`Intent build() { ... }`, Kotlin companion getters).
     Its fan-out is honest: every caller of a getter really does get that constant.
   - *Down-direction* -- pushing a caller's constants onto a callee's formals, which is what a send
     site inside a helper needs. Context-insensitive, so every caller's constants pollute every
     other caller's view of the callee. This is where the fan-out lives; do not take it without a
     call string, and a call string here means the resolvent machinery for a second payload.
   Both are now cheap to *try*, because they are rules in a fixpoint that already runs rather than
   a new phase. Gate the decision on the unresolved-send-site count, not on taste.
4. **Static-final action strings.** `javac` inlines `static final String` constants and Kotlin
   inlines `const val`, so the common cases arrive as `const-string` at the use site. What does not
   is a Kotlin companion `val` (a getter call) or an action read back through `sget`. The `sget`
   half is cheap to close without touching the interprocedural rules: scan for
   `store globals.<field> := "literal"` across all functions and seed those as available in every
   intent frame. It is a flow-insensitive table lookup rather than propagation, but it *is* a
   deliberate exception to the intraprocedural rule and should be a flag with a count, not a
   silent default.

## 7. Relationship to the previous ctadl

`~/proj/ctadl` carried this in Souffle at `src/ctadl/souffle-logic/jadx/android-intents.dl`, over
manifest facts a jadx plugin emitted. What is worth taking:

- The `_ManifestNode` / `_ManifestNodeChild` / `_ManifestNodeAttr` triple store (section 5, Phase 1).
- `_VarIsConst`, in spirit. That design also answered "which string is in this variable" with a
  relation rather than a syntactic walk; Phase 2's `const_reaches` is the same idea expressed over
  this analyzer's flow relation instead of over Souffle facts.
- The `ActivityHasIntentFilter` rule, including its treatment of a component's own name as a
  pseudo-filter so that explicit intents match through the same join.
- The separation of `IntentSend` / `IntentRecv` by communication type, so an activity send cannot
  pair with a broadcast receive.

What is not worth taking:

- The per-signature enumeration of every `putExtra` and `get*Extra` overload -- about 250 lines of
  near-identical rules. Phase 2 handles the family in two tiers, both of them rows in the engine's
  own relations: one `summary` row per matched method for everything whose path is static, and one
  pair of `assign` rows per call site for the keyed form, whose path is minted from the key. The
  enumeration collapses because a summary is keyed by callee and a name-plus-descriptor scan finds
  the callees, and the keyed form needs no per-overload rule at all.
- `StatementReachableFromOnCreate`. It exists to bridge a whole-program reachability model that
  this analyzer does not have.
- The `substring` special case in `VarReferencesString`. It exists because that design had to
  reconstruct a string syntactically; `const_reaches` propagates over whatever edges the summary
  relation carries, so a string operation that matters is one more summary row rather than a rule.
  (What it is *not* is free: propagating over `StringBuilder.append`'s modeled edges means a
  concatenated action reports its fragments as candidate actions. That is over-approximation in
  the same direction as the rest of the analysis, and Phase 4's fan-out budget is where it shows
  up.)
- The `ord()` key encoding, which is a workaround for Souffle's flat symbol paths. CTADL paths
  are structured and dex field symbols already carry dots inside their `<...>` pretty names, so
  extra keys embed as plain symbols.
