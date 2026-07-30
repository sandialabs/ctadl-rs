# Criticism of `bridging-models-ir-design.md` - DO-NOT-MERGE

Findings from reading the design against the code it cites. Ordered by severity: §1–§3 are
defects in the design as specified, §4–§6 are places where the argument overclaims, §7 collects
smaller points, §8 concludes. Every claim below was checked against the tree at the cited
locations.

## 1. The inspectability premise is not delivered by the design as written

The document's motivation — stated in its opening paragraph and repeated as the payoff of both
changes — is that models and bridges become "visible to `--dump-ir`". They do not, under this
design, and the document never notices.

`dump_ir` (`ctadl-ascent/src/cli/mod.rs:694-709`) loads the **cached on-disk** `ProgramInfo`
(`load_program_info_without_source_info`) and pretty-prints it. It loads no models, runs no
passes, and takes one import. Meanwhile everything the design adds happens **in memory, inside
`cli::index`**: model loading fills `FunctionData::summaries` during the import loop, and the
bridge pass edits function bodies in phase 2. Nothing writes the edited IR back to the import
cache — and nothing should, because the cache is currently a pure function of the artifact, and
persisting model-dependent edits into it would make `ctadl index --models a.json` poison the
next `ctadl index --models b.json`.

So delivering the headline benefit requires a third, unspecified change with real design
content of its own: either

- `dump_ir` grows `--models` flags and re-runs matching + summary attachment + bridge
  synthesis. For change A this is plausible. For change B it is not small: dumping *import A's*
  bridge body requires *import B's* match index, so `dump-ir` stops being a one-import command
  and inherits the whole phase-1 apparatus; or
- edited IR is persisted somewhere separate from the import cache, i.e. a new artifact with its
  own lifecycle and staleness rules.

Neither is priced anywhere in §4 ("What it costs"), and the §5 comparison table's
"Inspectability: `--dump-ir`" row is, as specified, false. The document's own decision
procedure for change B — "Synthesize the Lua bridge, dump it, and read it" (§8) — cannot be
executed with the tool it names.

## 2. Change A cannot represent the dominant use of summaries

The shape in §2.1 hangs `SummaryEdge`s off `mir::FunctionData`. That requires every function a
summary model can match to *have* a `FunctionData`. That invariant is false today, per
frontend, and the shipped default models depend on it being false.

- Model matching runs over the **VMT tables**, not over `FunctionData`
  (`models/json.rs:227-345`; `program_functions` at `json.rs:70` is consulted only for
  `Variable(name)` ports, `json.rs:747-762`). A generator can and does match names that exist
  in no `FunctionData`.
- `codegen_summary` resolves the matched *name* against the shared interner at fact time and
  explicitly tolerates functions "that don't occur in the facts"
  (`codegen/models.rs:37-43`). Summaries attach to any function that was so much as *called*.
- **Lua externals have no `FunctionData` at all.** The `externals` VMT column is "the set of
  called names minus the names the import defined" (`languages/lua/mod.rs:1018-1029` — "an
  external has no definition site to read it from"; the subjunctive at `lua/mod.rs:2352`, "what
  an externals shim *would have to* spell", confirms no shim exists). And
  `models/defaults/lua-index.jsonl` is a file of `propagation` summaries targeting exactly
  those externals — `string.format`, `table.concat`, `os.execute`, the stdlib. Under change A
  as specified, **the entire Lua default model set has nowhere to go.**
- Dex escapes only by accident of its frontend: an extern-stub loop synthesizes bodyless
  `FunctionData` for every called-but-undefined method (`languages/dex/mod.rs:405-454`).
  Whether pcode's `<EXTERNAL>::` functions all have `FunctionData` is unverified in the
  design and in this criticism.

The available fixes all hurt. Synthesizing stub `FunctionData` at model-load time mutates the
program to record what is really a fact about a name — strictly heavier than the fact row it
replaces. Keeping `codegen_summary` as a fallback for non-resident functions reinstates the
second producer of `facts.summary`, splits one construct across two representations, and loses
`--dump-ir` visibility for precisely the majority case (library models). Either way, change A's
claimed profit — "one producer", "every model becomes dumpable" — does not survive.

## 3. `SummaryPort` cannot express the shipped port space, and "one loop" understates codegen

§2.1 gives `SummaryPort.index: ParameterIdx_or_Return`. The port space `codegen_summary`
actually consumes (`codegen/models.rs:44-73`) is `Index | Return | Global | AnyArgument`:

- **`Global`** (`GLOBALS_INDEX`) has no representation in the proposed type.
- **`AnyArgument`** — `Argument(*)` — appears **34 times** in the shipped default model files
  (4 lua, 22 java, 8 native), and its expansion is *fact-time information*:
  `facts.compute_arg_arity()` takes the max arity over **actual call sites**
  (`index_engine/mod.rs:258`; `native-index.jsonl` documents this property and models rely on
  it). An IR-resident summary written at model-load time cannot expand it — the call sites are
  not codegen'd yet — so `SummaryEdge` must carry the tag symbolically and codegen must do the
  expansion.

Which means §2.3's "codegen_program gains one loop: for each `SummaryEdge`, push a
`facts.summary` row. Nothing else changes" is wrong in three particulars: codegen must
replicate the tag expansion, the `dst_index == src_index` skip, and the `formal_param` pushes
for both ports (`codegen/models.rs:78-98`) — the last of which the design never mentions and
without which `locals` seeding and the summary rule silently lose the modelled function.
None of this is fatal; all of it belongs in a design that claims §2.3 as its low-cost half.

Separately: "one producer of `facts.summary` at fact time instead of two" is miscounted even
granting change A. `load_and_map_summaries` (`cli/mod.rs:630-687`) pushes `facts.summary` rows
directly, post-loop, for cross-*project* summaries resolved through the id map. That producer
can never move into the IR — its input is another project's saved index. Fact-time producers go
2 → 2, not 2 → 1.

## 4. §1's "finding" is representation-independent, so it does not argue for IR-level bridges

§1 is right that emission does not need the post-loop point: `CallEdges::Explicit` names
targets by string and `get_or_add_function` interns on demand (`codegen/mod.rs:441-448`), so a
call row naming side B can be emitted before B is codegen'd. But notice what that observation
is about: **name interning at the facts layer**. It is equally available to the facts-level
design — `facts.call.push((site, get_or_add_function(b_name)))` works per-import today, no IR
change required. What actually forces lateness in *both* designs is **matching**: side B's
`where` runs against side B's match index, which may belong to a later import. The IR design
pays for that with its two-phase loop (§3.1); the facts design pays with a post-loop pass. The
quoted constraint from the facts design's §4 conflates matching and emission, and §1 inherits
the conflation in reverse.

Consequence: there is an unexamined third design — facts-level emission with hoisted matching,
or simply the facts design as-is (whose single post-loop call is not actually hard to place) —
that captures the entire ordering simplification of §1 with none of the IR costs of §4. The
side-by-side table in §5 compares two corners of the space and presents the ordering win as
belonging to the IR column, when it belongs to the *name-resolution* row that both columns
could share. This is the central structural weakness of the document: its strongest argument
does not distinguish its proposal from its rival.

What §1 *does* buy exclusively for the IR route is the §3.4 list (globals for free, a statement
to hang a span on, `verify`). Those are real and are weighed in §6 below — but they, not the
dissolved ordering argument, are the actual case for change B, and they are weaker.

## 5. The synthesis sketch is one-to-one only; cardinality is unaddressed and the natural extensions lose flows

§3.2 synthesizes "per resolved pair `(a, b)`" with `FunctionBuilder::new(a)`. The syntax being
adopted (facts design §3.2) includes `one-to-many` — several pairs sharing one stub `a`. The
sketch does not compose for that case, and both obvious extensions are hazardous:

- **Two disconnected blocks** (run the sketch twice): the second block is unreachable from the
  entry, and `ssa::transform_program(_, prune_unreachable_cfg_nodes)` will, when pruning is on,
  **delete the second bridge silently** — the design's own dominant-failure-mode, minted by the
  design.
- **Sequential calls in one block**: IR statements are ordered and SSA-versioned. Two
  out-direction writes to the same caller port become `x.1 = ret_of_b1; x.2 = ret_of_b2`, and
  only the final version flows out — callee 1's out-flow is shadowed. The fact-level encoding
  has no such hazard: `assign` rows onto `a`'s formals are unordered facts that coexist, and
  the facts design even keys temporaries on `(site, index)` for exactly this case (its §2,
  consequence 3, which this document claims carries over "unchanged" — it does not carry over;
  its motivating scenario is unhandled).

Correct synthesis under `one-to-many` needs branch-per-pair with a join (phi), which is a
materially more complex emitter than §3.2 shows and appears nowhere in the document, including
in §4's cost list and §6's test list. More generally: the move from facts to IR is a move from
an *unordered set of dataflow edges* to a *program*, and programs have ordering, dominance and
liveness. §3.5 treats the pre-SSA cleanups as the interaction surface; the deeper interaction
is that SSA semantics can delete flows the bridge meant to assert, in the basic multi-pair
case, not in an optimization corner.

Two adjacent under-specifications in the same sketch: `bb.create_ret(ret_exps)` glosses the
Java return-arity-2 shape — a synthesized Java-side stub must produce a value for the exception
return it deliberately does not map, and what that value is (undef? fresh temp? and does
`verify` accept it?) is unstated. And the sketch never emits `ParamFlow`
(`builder.rs:create_param_flow`), relying implicitly on SSA insertion to connect post-call
writes back to formals — probably true, stated nowhere, and load-bearing for out-direction
ports.

## 6. The §3.4 "free" list is real but each item is smaller than presented

- **Globals**: genuine. Codegen pushes the globals pair at every lowered call
  (`codegen/mod.rs:657-665`). This is the cleanest point in the document.
- **Source attribution**: overclaimed. "A synthesized call … carries a span like any other" —
  it carries whatever span the pass assigns, and the pass has nothing to assign: it runs on a
  bodyless stub (a dex extern stub has no code item and no debug info; whether the *function*
  has a declaration span is frontend-dependent). What the IR route buys is a **place to put** a
  span, not a span. The remaining work — choosing and wiring an attribution — is the same
  follow-up the facts design already scoped. Table row "spans like any other" should read "a
  statement that *could* carry a span".
- **`verify` as a net**: real but narrow. `Program::verify` checks statement well-formedness
  (offset-only `AccessPath`, symbolic-only `FieldPath` — `mir/mod.rs:379-407`). The dominant
  silent failures the facts design's §7 actually enumerates — wrong slot, wrong path escaping,
  wrong direction, scope matching nothing — all produce *well-formed* IR that verifies
  cleanly. `verify` catches the malformed-path-construction class, which is also the class a
  unit-tested lowering function eliminates once and for all. Weight accordingly.
- **Name resolution, the un-listed anti-item**: the IR route makes the silent-failure surface
  *worse* in one respect the document never flags. `get_or_add_function` on a call edge
  **creates** a node for an unknown name — a bridge whose side-B spelling drifts from B's
  `FunctionData.name` (three namespaces now: VMT match spelling, IR name, interned fact name)
  yields a phantom, summaryless callee and a silently dead bridge, with no point in the
  pipeline at which an error is even expressible. The facts design's post-loop pass can
  hard-error on a name absent from the id map. For a document that treats silent failure as
  the dominant risk, handing emission to an interner that cannot say "no" deserves a line in
  §4, not silence.

## 7. Smaller points

- **Phase 2 cost accounting**: with any bridge present, *every* import is deserialized twice,
  including the N-2 imports no bridge touches. §3.1 says "I/O plus decode, not a frontend
  re-run", which is honest, but the comparison table omits the row; the facts design pays zero
  here.
- **§2.2 is good** — the argument for keeping `ModelPath` distinct from `AccessPath` (a
  summary is a claim about the fixpoint, not a machine step) is the best-reasoned section in
  the document, and the instruction to document the non-unification is right.
- **The serialization break is under-leveraged as sequencing advice**: the document notes a
  version field "is arguably part of this change" (§4.2) and "probably worth doing first
  regardless" (§7). Given §2's finding that change A also needs a story for non-resident
  functions, the version field is the only part of change A that is unambiguously ready to
  build.
- **`bitcode` claim verified** (`ctadl-ir/src/mir/encode.rs:8`): not self-describing, so the
  break is real, and it invalidates every user's import cache, not an edge case.
- **Change A/B interaction**: the document is right that a bridge is not a summary (§2.4),
  but the consequence is that A and B share almost no machinery — a declaration list versus a
  builder-synthesized body — so "designed together for the shared inspectability payoff" buys
  less than it implies, and §1 shows the payoff itself is undelivered.
- **The §7 self-criticisms are correct and should be kept**: the deep-port dump being less
  legible than six fact rows (§3.3), the `GlobalHeap` port question, and the
  gate-on-the-Lua-artifact discipline in §8 are all sound — though per §1, producing that
  artifact requires tooling the design has not specified.

## 8. Conclusions

1. **Change A is not buildable as specified.** It has no representation for summaries on
   functions without `FunctionData` — which includes the entire shipped Lua default model set —
   and its port type omits `Global` and `Argument(*)`, the latter being unexpandable before
   codegen in principle. Any repair either mutates programs to store facts about absent
   functions or keeps the fact-level path alive, forfeiting the "one producer / every model
   dumpable" claims that justify A. The salvageable kernel: the IR version field, and
   possibly `SummaryEdge` for *resident* functions as a strictly additive convenience — if a
   use case survives the fallback path existing anyway.
2. **Change B's decisive argument is not its own.** The ordering simplification of §1 is a
   property of name interning at the facts layer and is available to the facts-level design
   unchanged. What remains exclusive to B — implicit globals, a span-bearing statement,
   `verify` — is real but modest, and is bought with a two-phase import loop, a cross-import
   edit protocol, an unsolved `one-to-many` synthesis with two flow-losing failure modes, a
   worsened silent-failure channel at name resolution, and an inspectability payoff that
   requires further unspecified tooling to observe at all.
3. **The recommendation should invert.** The document says "do A, prototype B behind the Lua
   example". The evidence says: do neither as specified; take the version field; fix the
   globals-port and attribution gaps *inside* the facts-level design (both have fact-level
   fixes: emit the globals pair unconditionally in `apply_bridges`; attribute the synthetic
   site to the stub's declaration where one exists); and re-derive the bridge design's §4.3
   ordering argument knowing emission could be per-import — which may simplify the facts-level
   pass without changing its layer.
4. **Process note.** The document is unusually honest about its costs (§3.3, §4, §7), and its
   verification section is better than its design sections — the §6 test list would have
   caught §5's SSA-shadowing hazard had "one-to-many at the IR level" been on it. The failures
   above are failures of *inventory* (what do summaries attach to today; what ports exist;
   what does `dump-ir` read), not of reasoning. The fix for the next revision is a §0 that
   enumerates the observable facts the design must preserve, checked against the tree, before
   the representation argument starts.
