# Bridging models at the IR level - DO-NOT-MERGE

**Models and bridges as IR constructs, synthesized before codegen, rather than as facts
synthesized after it.**

## 0. What this is

A counterproposal to `bridging-models-design.md`, which encodes a bridge as rows pushed directly
into `IndexFacts` after every import has been codegen'd. This document asks what happens if the
same construct is expressed one level up: models become a first-class IR concept, bridges become
synthesized IR, and both are visible to `--dump-ir` (`ctadl-ascent/src/cli/mod.rs:694`).

Two changes, **separable**, in increasing order of blast radius:

| | change | buys |
| --- | --- | --- |
| A | `Summary` as a construct on `mir::FunctionData` | every model becomes dumpable; one producer of `facts.summary` at fact time |
| B | a pre-codegen bridge pass over the full import set | bridges become synthesized IR; §4.3 of the facts design disappears |

A is useful without B. B depends on A only weakly (a bridge is not a summary — §2.4), but the two
share the motivation and the same inspectability payoff, so they are designed together here.

**What carries over unchanged from `bridging-models-design.md`:** the syntax (§3 — `in`,
`model.bridge`, `arguments`, `cardinality`, `on-unmatched`), the matching architecture (§4.1–4.2 —
`BridgeSpec`, `ProgramScope`, `ProgramMatchIndex`, reusing the existing evaluator), the per-method
attachment decision and the removal of `find: callsites` (§2.1), the frontend-specific-port-map
risk (§2.2, §7), and the scope limits (§6). None of that is affected by where emission happens.
What this document replaces is §4.3 (evaluation ordering) and §4.4 (emission).

## 1. The finding that makes this viable

The facts-level design is built around one constraint, stated in its §4:

> A bridge pins two sets of matches in two different programs, and can only be resolved once *both*
> programs' functions exist in the shared id map.

**That constraint does not exist at the IR level.** A direct call in the IR names its targets by
*string*:

```rust
// ctadl-ir/src/mir/call.rs:64
pub enum CallEdges {
    Explicit(ThinVec<String>),
}
```

and codegen resolves each string through the interner as it goes:

```rust
// ctadl-ascent/src/codegen/mod.rs:441
CallStyle::DirectCall { call_edges: CallEdges::Explicit(targets) } => {
    for target in targets {
        let target = fx::Function(target.clone().into());
        let target = self.source_info.sites.get_or_add_function(target);
        self.facts.call.push((site, target));
    }
}
```

Function names are the cross-program namespace — that is the same interning §1 of the facts design
already relies on when it observes that "two imports that spell a function identically already share
a node". A bridge that writes a call statement naming side B's function does not need side B to have
been codegen'd, does not need an `IdMap`, and does not need to run after the import loop. It needs
only to *know the name*, and names are what matching produces.

So the entire `apply_bridges(&[BridgeSpec], &[ProgramMatchIndex], &mut IndexFacts, &mut
IndexSourceInfo)` phase, and the ordering argument that justifies it, dissolves. What remains is a
much smaller cross-program requirement: **matching side B needs side B's match index**, and nothing
else.

This is not a small simplification. It is the difference between "a pass that must run at exactly
one point in `cli::index` for a reason that is easy to get wrong" and "a pass that rewrites one
function's body."

## 2. Summaries as an IR construct

### 2.1 Shape

```rust
// ctadl-ir/src/mir/mod.rs, alongside FunctionData::blocks
pub struct FunctionData {
    pub name: String,
    pub params: Params,
    pub return_type: ReturnType,
    pub blocks: BasicBlocks,
    pub locals: Locals,
    /// Declared dataflow through this function, independent of `blocks`. A frontend leaves this
    /// empty; model loading fills it in. Codegen emits one `facts.summary` row per edge.
    pub summaries: Vec<SummaryEdge>,
}

pub struct SummaryEdge {
    pub dst: SummaryPort,
    pub src: SummaryPort,
    /// Provenance: which model file and which generator produced this. Carried so `--dump-ir`
    /// can attribute an edge, and so a diagnostic can name the generator that wrote it.
    pub origin: ModelOrigin,
}

pub struct SummaryPort {
    pub index: ParameterIdx_or_Return,   // the same port space as a model port spec
    pub path:  ModelPath,
}
```

### 2.2 `ModelPath` is a new path type, and that is the honest cost

The IR deliberately does **not** have a flat access path. `AccessPath` is offset-only
(`x.[50].[4]`), `FieldPath` is a *single* symbol, and a symbolic field is reachable only through a
`Load` or a `Store` (`ctadl-ir/src/mir/mod.rs:379-407`). A model port path such as
`Argument(0).stack.[1]` is a mixed sequence of both and belongs to neither type.

A summary therefore needs its own path representation — structurally the same flat `Path` the fact
base already uses. Adding it to the IR is adding a concept the IR spent effort avoiding.

The justification is that a summary is **not a statement**. Statements describe memory operations,
and the offset/field split exists because a memory operation reads or writes exactly one field at
one address. A summary describes *reachability between two ports*, which is a claim about the
fixpoint's output, not about a machine step. It has no lowering, no address, and no single field, so
the discipline that shapes `AccessPath` has nothing to say about it. Keeping it as a distinct type
next to `AccessPath` — rather than trying to force it into one — is what keeps the invariant
`verify` checks on statements meaningful.

Say so explicitly in the type's documentation, or the next reader will try to unify them.

### 2.3 What codegen does with them

`codegen_program` gains one loop: for each function, for each `SummaryEdge`, push a `facts.summary`
row. Nothing else changes, and specifically **the model-path bucket is preserved**. `model_paths` is
seeded only from `facts.summary` (`index_engine/mod.rs:916-925`), and the one-level concat rules
(`mod.rs:1053-1054`) join it against `program_paths`. A summary path that arrives via the IR is
still a `facts.summary` path, so it still lands in the model bucket and still composes with every
program path. This is the property the fact-level encoding has and an IR-*statement* encoding would
lose; carrying summaries as declarations rather than as synthesized code is exactly what preserves
it.

The comment at `mod.rs:916-919` — that folding `facts.paths` into `model_paths` turns the concat
into a program×program self-join at |facts.paths|² rows — is the reason this matters and should be
cross-referenced from the new codegen loop.

### 2.4 What this replaces, and what it does not

`codegen_summary(models_batch.summary, &mut facts, &mut source_info)` (`cli/mod.rs:119`) stops being
a separate step that runs after `codegen_program` and reaches into the fact base. Model loading
instead writes `SummaryEdge`s onto the matched `FunctionData`, and `codegen_program` emits them
along with everything else. One producer of `facts.summary` at fact time instead of two, and the
model is visible in `--dump-ir` output between matching and codegen — which is the inspectability
the whole idea is for.

**A bridge is not a summary.** Worth stating loudly, because the shape invites the mistake. Side A's
behavior *is* side B's behavior, and side B's behavior is not known until the fixpoint has run — it
is derived, not declared. A bridge that tried to write a `SummaryEdge` on side A would have to know
side B's summary in advance, which is the thing the analysis exists to compute. The bridge must
synthesize a **call**, so that summary instantiation replays whatever side B turns out to do. §3 is
about that call.

## 3. The pre-codegen bridge pass

### 3.1 Where it runs

Today's loop (`cli/mod.rs:73-121`) is: for each import — load `ProgramInfo`, observe for JNI, load
models, dead-temp/coalesce/SSA, `codegen_program`, `codegen_summary`.

The change is to split it in two:

```
Phase 1 (all imports):  load ProgramInfo -> build ProgramMatchIndex -> drop the IR
                        resolve every BridgeSpec against the collected indexes
                        -> a list of per-import IR edits, keyed by import

Phase 2 (all imports):  load ProgramInfo -> apply this import's edits
                        -> models -> dead temps -> coalesce -> SSA -> codegen_program
```

Phase 1 is the same `ProgramMatchIndex` construction the facts design already specifies in its
§4.2, and the same reuse-the-existing-evaluator rule from its §4.3 — only hoisted so that *all*
indexes exist before *any* program is edited. That ordering requirement is real and unavoidable:
side B's `where` constraints have to run against side B's program, which may be a later import.

**Memory.** The same profile the facts design already accepted: match indexes for every import
resident at once, one `ProgramInfo` at a time. Not "hold all IR", which would be much worse. Its
§4.2 asks for a footprint checkpoint after the import loop and a real number for an APK + `.so`;
that measurement is still owed and is unchanged by this proposal.

**Cost.** Each import's `ProgramInfo` is deserialized twice. `load_program_info_without_source_info`
is a `bitcode` decode of the cached per-import IR, so this is I/O plus decode, not re-running a
frontend. Skip phase 1 entirely when no `BridgeSpec` was loaded — the facts design's §4.1 already
scans specs out of the models files before the import loop for exactly this reason, and that
hoisting becomes load-bearing here rather than merely tidy.

### 3.2 What it synthesizes

Per resolved pair `(a, b)`, the pass produces the same structure §4.4 of the facts design arrived at
— temporaries standing for the callee's parameters, ports wired to sub-paths of them, temporaries
passed to the call — but as IR, using the existing builder (`ctadl-ir/src/mir/builder.rs`):

```rust
let mut fb = FunctionBuilder::new(a);        // `a` is the matched (usually bodyless) stub
let block = fb.add_block();                  // a bodyless stub has no blocks at all
let mut bb = fb.at_block(block);

// One temporary per distinct callee index in the port map.
let temps: BTreeMap<Index, VariableRef> = callee_indices
    .map(|n| (n, bb.new_local_var(&format!("$bridge_arg{n}"))))
    .collect();

for port in ports {
    let caller = /* Exp / place for `Param(port.from.index)` at `port.from.path` */;
    let callee = /* place for `temps[port.to.index]` at `port.to.path`  -- see 3.3 */;
    if port.direction.inward()  { /* write caller -> callee */ }
    if port.direction.outward() { /* write callee -> caller */ }
}

bb.create_call(
    CallStyle::DirectCall { call_edges: CallEdges::Explicit(thin_vec![b_name.clone()]) },
    /* rets */ ret_temps,
    /* args */ temps.values().cloned().map(Exp::Variable).collect(),
);
bb.create_ret(ret_exps);
```

Side A's parameters are asserted with `FunctionBuilder::add_param` and its return arity with
`set_return_arity` — the IR-level equivalents of the `formal_param` rows §2 consequence 4 of the
facts design emits by hand.

**Side B's parameters are a cross-import edit.** The pass never touches B's IR from A's phase, so
the "trust the model over the recovered prototype" rule of §2 consequence 4 needs somewhere to live.
It falls out of the phase split cleanly: phase 1 emits edits keyed by *import*, and a pair produces
two of them — a body-and-params edit against A's program and a params-only edit against B's. Phase 2
applies whichever edits belong to the import in hand. The arity warning is computed in phase 1,
where both sides' match indexes are still available, which is a better place for it than the facts
design's emission loop.

### 3.3 Port sub-paths must be lowered, and this is the real cost

A port path like `Argument(0).stack.[1]` cannot be written as one IR operand. `.stack` is symbolic
and so reachable only through a `Load`/`Store`; `.[1]` is an offset and so belongs to an
`AccessPath`. Writing `t.stack.[1] = x` lowers to a hop through a temporary:

```
%h = load t.stack          // Load { dest: %h, source: AccessPath(t, []), field: .stack }
store %h.[1] := x          // Store { dest: AccessPath(%h, [.[1]]), field: none, value: x }
```

— one extra temporary per symbolic field in the path. Two consequences:

1. **The composed path exists on no single edge.** This is a known property of the lowering, not new
   to bridges: `index_engine/mod.rs:858-862` says exactly this about field reads through pointers
   ("lowered to a chain of loads through temporaries, so only the per-hop paths land on edges"), and
   notes that codegen separately pushes composed paths into `facts.paths` so the propagation gate
   still admits them. A synthesized bridge inherits that machinery and needs no special handling.
2. **They land in the program-path bucket**, because `facts.paths` feeds `program_paths`
   (`mod.rs:862`). That is *the same outcome* as the facts-level design, whose §2 consequence 2
   already concedes that a callee-side path written on a bridge composes one level less than the
   same path on a `propagation`.

So on the bucket question the two designs are a wash, and neither is as good as a summary port.
What the IR route loses is directness: the facts design writes `to_path` as one literal path on one
`assign`, whereas here it becomes a chain whose shape depends on how many symbolic fields the port
names. **This is the strongest argument against B**, and it should be weighed honestly against the
inspectability gain — the dumped IR for a deep port is *more* verbose and *less* obviously equal to
what the model said than the corresponding fact row is.

### 3.4 What the IR route gets for free

These are not restatements of the same benefit; each closes a specific hole in the facts-level
design.

- **The globals port disappears.** §2 consequence 5 of the facts design requires `GLOBALS_INDEX` to
  be mapped explicitly or "heap flows do not cross the boundary at all", and cites `JniFlow` as the
  case that fails without it. Codegen already passes globals at *every* call site it lowers
  (`codegen/mod.rs:657-666`, "pass globals"), so a synthesized call gets it unconditionally. The
  implicit-and-not-user-visible globals pair stops being something the bridge emitter has to
  remember.
- **Source attribution stops being a known gap.** §4.4 of the facts design ends with a synthetic
  site that has no `source_map` entry, a SARIF step emitter that returns early, and a flow that
  renders with nothing naming the crossing — flagged there as a worthwhile follow-up. A synthesized
  call is a real `Statement` in a real block and carries a span like any other, so the crossing can
  point at the stub's own declaration site. The multi-import span concern from that section
  (`INDEX_FORMAT_VERSION` 3, spans resolved against the import that numbered them) is *satisfied*
  rather than dodged: the statement lives in A's program and is numbered by A's import.
- **`verify` becomes a net under the synthesis.** `Program::verify` checks the IR's invariants —
  including the offset-only invariant on `AccessPath` and the symbolic-only invariant on
  `FieldPath`. A bridge that lowers a port path wrongly fails verification at the IR boundary. The
  facts-level equivalent is a malformed `Path` in an `assign` row, which nothing checks and which
  manifests as a missing flow. Given that §7 of the facts design names silent failure as the
  dominant risk, moving a class of it into a checked boundary is worth real weight.
- **The degenerate case may optimize itself.** `ssa::coalesce_copies` runs before SSA
  (`cli/mod.rs:105`) and fuses single-use copy temporaries. A JNI-shape port — empty `to_path`,
  `direction: both` — synthesizes as `t_n = param_k` with exactly one use (the call), which is that
  pattern. If it fuses, the bridge reproduces today's shipped `actual_param`-direct fact shape with
  no special case, where §4.4 of the facts design has to hand-write one to keep
  `languages/jni/tests.rs` passing. **Unverified.** Test it before relying on it; if coalescing does
  not fire here, nothing breaks — the extra copy hop is semantically inert — but the claim should
  not go in the doc as settled.

### 3.5 Ordering hazards

- **Before SSA.** The pass must run before `ssa::transform_program` so synthesized statements get
  versioned and `ParamFlow`/phi insertion sees them.
- **Interaction with the pre-SSA cleanups.** `eliminate_dead_temps` and `coalesce_copies` also run
  before SSA. A bridge temporary is never dead — each one is read by the call — so elimination
  should not touch them, and coalescing is *wanted* in the degenerate case (above). A temporary that
  has stores into it is not a single-use copy and must survive. Both directions need a test; this is
  the most likely place for a synthesized bridge to be silently optimized into something else.
- **Relative to the built-in JNI pass.** Unchanged from today: `jni_observer.observe` runs per
  import and `jni::link` after the loop, and a declarative bridge over a pair the built-in pass also
  links would double-bridge it. `--no-jni-bridge` remains the answer, and the A/B story in §8 of the
  facts design still works.

## 4. What it costs

Collected in one place, because these are the reasons to *not* do it:

1. **A new IR construct with a path type the IR avoids** (§2.2).
2. **A breaking IR serialization change.** `encode_program` is `bitcode::serialize`
   (`ctadl-ir/src/mir/encode.rs:8`) — compact and not self-describing. Adding `summaries` to
   `FunctionData` invalidates every cached per-import `ProgramInfo` on disk, so every user re-runs
   `ctadl import`. There is no version field to negotiate on; adding one is arguably part of this
   change.
3. **A two-phase import loop** and a second deserialization per import (§3.1).
4. **Port paths become chains** whose dumped form is less legible than the model that produced them
   (§3.3) — the direct counterweight to the inspectability argument.
5. **SSA and `verify` must accept synthesized bodies in functions that previously had none.** A
   bodyless stub acquiring a block, locals, params and a terminator is a shape those passes have not
   seen from a frontend.

## 5. Side by side

| | facts-level (`bridging-models-design.md`) | IR-level (this) |
| --- | --- | --- |
| Cross-program resolution | needs the shared `IdMap`; forces a post-import-loop pass | by name, via `CallEdges::Explicit`; no id map |
| Where it runs | `apply_bridges`, after every codegen | a `&mut Program` pass, before SSA |
| Globals port | mapped explicitly or heap flows do not cross | implicit — codegen passes globals at every call |
| Callee formals | `formal_param` rows pushed for side B | a params-only IR edit against B's import |
| Source attribution | absent; SARIF renders no crossing step | a real statement, spans like any other |
| Port sub-paths | one literal path on one `assign` | lowered to a load/store chain per symbolic field |
| Path bucket | program | program (no change) |
| Degenerate JNI case | hand-written special case to keep the shipped shape | possibly free via `coalesce_copies` (unverified) |
| Malformed emission | silent wrong/missing flow | caught by `Program::verify` |
| Inspectability | fact rows, no per-generator row dump | `--dump-ir` |
| New concepts | none | `SummaryEdge`, `ModelPath` |
| Serialization impact | none | breaks cached `ProgramInfo`; re-import required |

## 6. Verification

Everything in §8 of the facts design that concerns parsing, scoping and matching applies unchanged.
What differs:

- **Synthesis, at the IR level.** Given a pair and a port map, assert the exact synthesized
  `FunctionData` — blocks, locals, params, statements, call style and edges — rather than fact rows.
  This is a better test than the fact-row assertion it replaces: it compares against something a
  human can read, and `mir::WithLocalNames` already renders it.
- **`verify` passes on every synthesized body**, including deep port paths and one-directional
  ports. Add a deliberately malformed port (a symbolic field in an `AccessPath`) and assert
  verification rejects it.
- **Survival through the pre-SSA passes** (§3.5): a bridge temporary is not eliminated as dead; a
  temporary with stores into it is not coalesced away; the degenerate JNI-shape port is coalesced
  (or, if it is not, that the resulting facts are still correct).
- **Fact-level equivalence with the built-in JNI pass.** Run the `--frontend jni` regression with
  `--no-jni-bridge` plus a declarative bridge and assert the same flows as the built-in. This is the
  A/B the facts design already proposes, and it is the strongest single check that the IR route
  produces the same analysis.
- **Summaries round-trip** (change A, independently testable): a model loaded onto a
  `FunctionData::summaries` produces the same `facts.summary` rows `codegen_summary` produces today,
  and its paths still land in `model_paths` and not `program_paths`.
- **End-to-end, two flowy imports** — unchanged from the facts design, including the
  deliberately-different-function-names requirement.

## 7. Risks and open questions

**Does `coalesce_copies` actually fire on the degenerate port?** (§3.4.) The claim that the IR route
reproduces the shipped JNI fact shape for free rests on it. Cheap to check; check it before this
argument is used to justify the change.

**Is a `Variable::GlobalHeap` port expressible if a user names one?** Codegen passes globals
implicitly at every call, so the common case needs nothing. But `GlobalHeap` "may only be written in
a `Store` instruction" (`mir/mod.rs:362-364`), so a port map that names the globals pseudo-parameter
explicitly may have no legal IR form. Probably the right answer is that the syntax stops accepting
one — §3.2 of the facts design already says globals are "always mapped and not user-visible" — but
it should be a checked error rather than an unrepresentable case discovered during synthesis.

**The serialization break** (§4.2) is the largest non-technical cost and the one most likely to
decide this. Adding a version field to the IR encoding is separable and probably worth doing first
regardless.

**Does the win survive contact with a deep port?** §3.3 is the honest weak point: the Lua example
from §3.3 of the facts design (`Argument(0).stack.[1]`, `.stack.[2]`, `.stack.[-1]`) synthesizes
into three load/store chains through three extra temporaries. Dump that function early. If the
dumped IR is *harder* to check against the model than the six fact rows the other design emits,
the inspectability premise of this whole document is wrong for exactly the case that motivated
bridges, and change B should be dropped while change A is kept.

**Everything §7 of the facts design lists remains true**: silent failure as the dominant mode, the
frontend-specificity of a hand-written port map, unquantified match-index memory, and callee-side
paths being unvalidated end to end.

## 8. Recommendation

**Do change A. Prototype change B behind the Lua example before committing to it.**

A stands on its own: it makes every model dumpable, removes a second producer of `facts.summary`,
costs one new field and one codegen loop, and preserves the model-path bucket that makes the whole
fact-level treatment of models work. Its only real cost is the serialization break, which is
tolerable and which the IR should probably take anyway.

B is more attractive than it looked before §1 — resolution by name genuinely deletes the most
delicate part of the other design, and globals, source attribution and `verify` are three real holes
it closes. But its central claim is inspectability, and §3.3 is the one place where the IR
representation is plainly *worse* than the fact representation. Synthesize the Lua bridge, dump it,
and read it. That single artifact decides B, and it can be produced long before any of the rest is
built.
