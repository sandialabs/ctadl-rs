# Call-graph discovery ideas

Design proposal based on the code at `3e3f09ab`. The performance observations
come from recorded runs; the proposed analysis has not been implemented or
benchmarked.

## Recommendation

Establish a new call-discovery baseline built for scale, starting with a separate,
small analysis over SSA using symbolic return origins and parameterized dispatch
requirements. It should avoid computing the general taint-flow closure.

Changing call results during this redesign is acceptable. There is no requirement
to reproduce the existing engine's edges or retain its expensive mechanisms for
compatibility. In particular, remove speculative CHA dispatch and allow flows
through unsupported heap patterns, out-parameters, globals, or conditional
summaries to remain unresolved.

The experiment should establish whether this restricted discovery kernel is
useful and structurally cheaper. The proposed relations still have potentially
large products; their scaling must be measured before adopting the design.
Once a useful baseline is established, prefer additive extensions that preserve
its discoveries. Treat intentional edge removal as an explicit semantic change,
not as a reason to retain an unsuitable architecture indefinitely.

## Objectives and constraints

The objective is call-graph discovery. Leaving a call unresolved is acceptable.
There should be no CHA or other fallback that enumerates speculative callees.
Avoiding speculative target enumeration does not guarantee execution feasibility.

Discovery should have declarative, evaluation-order-independent semantics.
Stopping after a global worklist budget is not a semantic definition.

The current redesign may change previously discovered call edges. After the new
baseline is established, pursue a policy of generally preserving discoveries for
the same input and modeling assumptions while adding supported patterns. This
is a development policy, not an unconditional compatibility guarantee. Better
overall coverage alone does not demonstrate preservation; removed edges should
be detected and explained.

Structural efficiency means avoiding call strings, eager points-to closure, and
expensive computations hidden in preprocessing. Remaining products and
worst-case growth must be explicit. Small reusable procedure summaries should
compose without repeatedly expanding procedure bodies.

## Representation and current analysis

The [IR semantics](docs/ir.md) defines how function and class labels flow through
assignments and calls. The discovery kernel uses a restricted subset of those
flows. The relevant representation is:

- `CallAssign` distinguishes direct, function-pointer, Java, CIL, and Lua calls.
  Arguments and return slots are explicit.
- `Exp::ObjectRef` carries object-class tags and function addresses. JVM/Dex
  allocations, CIL allocations/boxing, and recognized native function addresses
  produce these tags. JVM/Dex lower supported method-handle constants
  to function references and `MethodHandle.invoke`/`invokeExact` to indirect
  calls. For method handles, runtime lookup, adaptation, constructor invocation,
  and virtual overrides are not generally modeled.
- `Assign` has a full `AccessPath` destination and `Exp::Path` sources.
  `StatementKind::is_store` classifies a non-empty destination path as a store;
  an empty destination path is a copy and defines an SSA variable. A load
  `x = y.f` and pointer arithmetic `x = y.[8]` are both copies in this terminology.
  Neither is a pathless transfer admitted by the initial kernel.
- Memory stores do not define a new SSA base or kill earlier stored values.
- Paths use typed `Field`, `Offset`, `Deref`, and `Value` segments. Use
  [cir::path](ctadl-ir/src/cir/path.rs) for normalization when recognizing
  supported flows; do not infer a path's meaning from its printed spelling.
- Return types describe **return arity**, not concrete runtime classes.
  `ParameterType` describes sharing behavior, not a receiver's class. Neither
  justifies `ReturnsType(T)`.
- CIR branches are nondeterministic `Goto` successors. Phi operands retain
  predecessors in CIR, but codegen projects them into ordinary assignments,
  discarding that association. S16 itself defines a phi as its set of copies.
  There are no branch predicates to prove general execution feasibility.

See [CIR statements and expressions](ctadl-ir/src/cir/mod.rs),
[call styles](ctadl-ir/src/cir/call.rs),
[terminators](ctadl-ir/src/cir/terminator.rs), and
[phi lowering](ctadl-ascent/src/codegen/mod.rs).

The current analysis is compositional and does **not** enumerate call
strings. Its `critical_summary(function, formal, path)` and
`resolvent(function, formal, path, target, decision)` describe parameter
dependencies; `establishes_*` retains single-hop relationships. Decisions identify
a formal/path/target, not a calling history. The `NeedsReceiver` proposal is
therefore a simplification and refocusing of existing ideas, rather than the
introduction of compositionality. See the
[hybrid rules](ctadl-index/src/index_engine/mod.rs) and
[decision representation](ctadl-index/src/index_engine/decision.rs).

The expensive substrate is broader:

- `locals` relates local access paths to formal access paths.
- Summaries relate pairs of formal access paths.
- Summary application produces more assignment edges.
- Field extension, alias summaries, tag propagation, and conditional flows
  participate in the fixpoint.

Preprocessing also performs analysis: formal-alias closure, program-path ×
model-path concatenation, and class-hierarchy closure. Moving work there would
not satisfy the efficiency objective. See `compute_alias_of_formal` and
`compute_paths` in the [index engine](ctadl-index/src/index_engine/mod.rs), and
`run_cha` in [codegen](ctadl-ascent/src/codegen/mod.rs).

## Performance evidence

The evidence points toward reducing those products, but does not establish a
speedup for the proposed replacement:

| Evidence inspected | What it establishes |
| --- | --- |
| Commit `12f50391` | Changing the tag/edge join reduced its reported time from 1,091 to 7 seconds in one configuration; total fixpoint fell from 1,388 to 296 seconds, while peak memory rose from 13.3 to 16.2 GiB. Intermediate materialization remains a substantial tradeoff. |
| Commit `e1ad45c2` | Consolidating conditional and unconditional rows reportedly reduced peak memory across six firmware binaries from 0.87–17.7 GiB to 0.27–3.02 GiB. “Identical findings” is weaker evidence than call-edge inclusion. |
| [Path-set implementation notes](ctadl-index/src/index_engine/path_set.rs) | A historical dense-binary run constructed four billion candidate paths and retained six percent. Finite domains alone do not make propagation cheap. |
| Saved `cajino_baidu` profile | A completed 0.892-second fixpoint had 302,393 `locals`, 600,948 `assign_like`, 640,587 `edge_split`, and 97,452 propagated tag rows. It uses mixed CHA and the older conditional layout; it is not a current no-CHA baseline. |

The saved profile inspected was
`~/.local/state/ctadl/projects/cajino_baidu/index/profile.json`, with its adjacent
`index_config.json`. It is a local artifact, not a repository fixture.

These are historical measurements, not a baseline for the current code. Collect
fresh profiles with the same imports, models, and pass configuration when
comparing the discovery kernel with the existing engine.

## Proposed design

The following ideas develop the Recommendation into one candidate design for
building a call graph. They are proposals to test, not additional project
constraints or decisions already made. The objectives above motivate the design;
the historical performance evidence does not establish that it will be faster.

The core proposal combines two ideas:

1. **Resolve dispatch from specific type or function evidence.** Replace the
   current CHA resolvent table with lookup for the evidenced receiver class or
   function address.
2. **Propagate only the information needed for call discovery.** Use a separate
   SSA analysis with symbolic return origins and dispatch requirements, so
   wrappers and factories can contribute edges without general taint closure.

Demand restriction is a possible optimization of that core. Narrow field
tracking and guarded return effects are possible later extensions. The sections
below describe how these mechanisms could work, where they would lose coverage,
and what an experiment would need to measure. Restrictions inside a mechanism
define the proposed experiment's scope or explain what makes its rules valid;
they are not requirements to adopt that mechanism.

## Idea 1: resolve dispatch from specific evidence

I propose replacing the existing dispatch lookup as part of the new baseline.
This addresses the objective of avoiding speculative callee enumeration.
Currently, `emit_callee_resolvents` exports `cha.resolvents`, whose construction
includes:

```prolog
cha_resolve(supertype, signature, implementation) :-
    inherited_method(subtype, signature, implementation),
    subtype_of(subtype, supertype).
```

Consequently, even when discovery supplies the tag for `new A`, lookup under `A`
can produce overriding methods in subclasses of `A`. Merely requiring a tag—or
selecting `--strategy hi`—does not eliminate speculative target enumeration.
See [resolvent emission and construction](ctadl-ascent/src/codegen/mod.rs).

The replacement could expose a different relation:

```text
Dispatch(Type(T), signature, method)
Dispatch(Function(f), function_call_key, f)
```

`Dispatch(Type(T), …)` means lookup for the **specific evidenced runtime class
`T`**, including applicable inherited implementations. Under this definition,
lookup does not enumerate descendants of `T`. For example, evidence from
`new A` would allow the implementation selected for an `A` receiver, but would
not add an override belonging only to a subclass of `A`.

A possible implementation is demand-driven, memoized lookup for encountered
`(T, signature)` pairs. To meet the evaluation-order-independence objective, its
hierarchy walk, override handling, and interface-default resolution would need
deterministic semantics over fixed metadata. The first experiment could leave
unsupported or ambiguous language cases unresolved. Lookup would also need
declaring-type and dispatch-kind information where relevant; the existing
deferred key of method name and descriptor is not the whole invocation contract.

This uses hierarchy metadata to implement dispatch, without using hierarchy
membership to invent receiver possibilities. It still does not prove that the
receiver reaches that instruction in an execution.

## Idea 2: build a small symbolic discovery kernel

I propose a separate analysis over post-SSA CIR that follows pathless copies,
summarizes where return values come from, and records which parameters must
supply evidence for an indirect call. The intended benefit is to discover calls
through common wrappers and factories without computing the general taint-flow
closure. This kernel would use Idea 1's lookup when concrete evidence becomes
available.

For a first experiment, the symbolic vocabulary could be:

```text
Arg(i)          value comes from this procedure's input i
Type(T)         concrete-class evidence from an admitted producer
Function(f)     evidence of this function address
```

These are finite atoms, not recursively nested terms. Multiple atoms mean
alternatives. The constructor notation in the sketches is shorthand for tagged
finite-domain values, not unrestricted term construction.

The proposed discovery fact base is:

```text
Copy(p, dst, src)
Input(p, i, value)
Return(p, slot, value)
Actual(callsite, i, value)
Result(callsite, slot, value)
Direct(callsite, procedure)
Indirect(callsite, receiver, dispatch_key)
Seed(p, value, concrete_evidence)
Owner(callsite, procedure)
```

The initial extractor would admit the following flows. These boundaries keep
the experiment small and avoid treating unrelated IR operations as copies:

- Emit `Copy` for an empty destination path and an `Exp::Path` source with an
  empty path, and for each phi operand. Several admitted sources give separate
  alternatives; they do not make the sources aliases of one another.
- Do not emit `Copy` merely because `is_store()` is false. `x = y.f`,
  `x = y.[8]`, and `x = value(y)` need different treatment. Leave their
  unsupported contributions unresolved in the initial kernel.
- Seed pathless `ObjectRef` assignments. For literal arguments or literal return
  operands, use finite synthetic operand slots keyed by their syntactic sites,
  so `return ptr<f>` and `invoke(ptr<f>)` do not lose their evidence simply
  because the literal has no variable. This is linear extraction, not a closure.
- Bind `Input` to formal entry variables and `Return` to explicit return
  operands. Do not import the taint codegen's return-time formal/global
  write-backs as ordinary parameter-rebinding copies.
- Preserve the frontend's call ABI: Java/CIL receiver arguments need the same
  slot-zero insertion as codegen; Lua already supplies its receiver. Map result
  slots consistently, including the final exception slot where present. The
  sketches number results from zero; engine facts use negative formal indices.

Under S15, models desugar to assignments or calls. An explicitly admitted
pathless model copy can therefore support target-label propagation under that
modeling assumption. A generic taint summary row, however, does not certify a
pathless copy or source-language object identity: it can encode stores, suffix
flows, and engine approximations. Keep admitted model semantics explicit rather
than interpreting every taint flow as a `ReturnsArg` discovery.

### Relationship to target-label semantics

The proposed `Origin` relation tracks selected IR target origins. It does not
prove that a source-language runtime value equals the origin. Frontends lower
arithmetic operands as `value(p)`, while copying the base of an address
computation. The extractor must distinguish those contributions.

There is a deliberate limit here: S11 allows a function-pointer call through
`e` to consume target labels at both `e` and `e.#value`. Thus a target label can
justify a call in the IR even after value-only arithmetic, without proving the
computed address is the function's actual address.

Keep `Exp::Value`, non-empty paths, and value-only target recovery outside the
initial kernel. This deliberately omits discoveries supported by S11. If a
later extension admits value-only targets, use a separate finite evidence kind
or channel; do not silently upgrade them to exact receiver types or introduce
general suffix closure.

### Summarize return origins and substitute at each callsite

The first mechanism is a symbolic return summary. Keeping parameter origins
symbolic until each callsite applies the summary avoids mixing different
callers' arguments. Represent it as `Ret(p, slot, origin)`, accommodating the
IR's multiple returns:

```prolog
Origin(p, v, Arg(i)) :-
    Input(p, i, v).

Origin(p, v, e) :-
    Seed(p, v, e).

Origin(p, dst, o) :-
    Copy(p, dst, src),
    Origin(p, src, o).

Ret(p, r, o) :-
    Return(p, r, v),
    Origin(p, v, o).
```

Let `Apply(c,q)` mean that callee effects may be applied at `c` without an
outstanding caller-parameter requirement:

```prolog
Apply(c, q) :-
    Direct(c, q).

Apply(c, q) :-
    Owner(c, p),
    Indirect(c, v, k),
    Origin(p, v, e),
    Concrete(e),
    Dispatch(e, k, q).
```

`Concrete` holds for admitted `Type` and `Function` atoms. Then instantiate
summaries:

```prolog
Origin(p, dst, o) :-
    Owner(c, p), Apply(c, q), Result(c, r, dst),
    Ret(q, r, Arg(i)),
    Actual(c, i, a), Origin(p, a, o).

Origin(p, dst, e) :-
    Owner(c, p), Apply(c, q), Result(c, r, dst),
    Ret(q, r, e), Concrete(e).
```

For example:

```text
id(x)        { return x; }
makeA()      { return new A; }
getHandler() { return &handler; }

a = id(new A);
b = id(new B);
a.run();
b.run();
```

`Ret(id,0,Arg(0))` is instantiated separately at the two calls. The analysis never
inserts both callers' concrete types into `id`'s formal and then exports their
union back to both callers.

These rules discover identity wrappers, direct factory chains, returned function
addresses, and local dispatch using their results. They do not establish that a
factory returns on every execution, or that each alternative is compatible with
a particular caller.

For example:

```text
choose(flag) {
    if (flag) return new A;
    else      return new B;
}
choose(true).run();
```

The branch-insensitive summary contains both types. `B.run` can be spurious
despite having an allocation-backed derivation. Callsite instantiation preserves
argument substitution; it does not restore branch correlation.

### Carry unresolved dispatch requirements through wrappers

The second mechanism is a dispatch requirement: record that a call inside a
procedure needs evidence from one of its parameters. Such a requirement can
pass through wrappers until an actual argument supplies concrete evidence,
without importing callers' values into reusable summaries. The proposed rules
for the pathless experiment are:

```prolog
Need(p, i, original_site, k) :-
    Owner(original_site, p),
    Indirect(original_site, v, k),
    Origin(p, v, Arg(i)).

Need(p, j, original_site, k) :-
    Owner(c, p), Apply(c, q),
    Need(q, i, original_site, k),
    Actual(c, i, a),
    Origin(p, a, Arg(j)).

Edge(original_site, target) :-
    Owner(c, p), Apply(c, q),
    Need(q, i, original_site, k),
    Actual(c, i, a),
    Origin(p, a, e), Concrete(e),
    Dispatch(e, k, target).

Edge(c, q) :-
    Apply(c, q).
```

For example:

```text
invoke(x) { x.run(); }        // site s
forward(y) { invoke(y); }

forward(new A);
forward(new B);
```

This derives:

```text
Need(invoke, 0, s, run)
Need(forward, 0, s, run)
Edge(s, A.run)
Edge(s, B.run)
```

The two edges belong to the original syntactic site inside `invoke`. No caller
sequence is stored.

The crucial restriction is that **`Edge` is not automatically fed back into
`Apply`**. Consider:

```text
apply(f, x) { return f(x); }

a = apply(&identity, new A);
b = apply(&makeB, new A);
```

Discovering both targets inside `apply` does not justify applying both targets'
return summaries for every caller of `apply`. Doing so would lose the distinction
the current conditional machinery exists to preserve; the
[hybrid-inlining fixture](ctadl-ascent/tests/tnt/hybrid_inlining.tnt) exercises that
issue.

In this example, the proposed kernel would discover the internal call edges and
leave their caller-dependent return effects unresolved. That is a deliberate
coverage limitation relative to S10/S14. The
[guarded-summary fixture](frontends/ctadl-flowy/tests/semantics/s14_guarded.flowy)
provides another concrete example of the effects this first kernel omits.

### Possible later extension: guarded return effects

To recover some of that coverage, a later extension could represent a guarded
return effect:

```text
if Arg(0) = Function(identity), return Arg(1)
```

But this should be treated as a separate abstraction. Composing effects
requiring two independent parameter facts needs conjunctions or an explicit
approximation. The current
[`Cond::and`](ctadl-index/src/index_engine/decision.rs) explicitly declines
compositions needing two distinct decisions together. Replacing that behavior
with unrestricted condition formulas could introduce combinatorial growth.

### Possible optimization: restrict propagation to demanded values

One way to reduce unnecessary work is demand restriction. This would focus
propagation on values relevant to discovery without defining semantics by a
budget. The proposed scheme seeds `Live` at indirect-call operands and return
operands, propagates it backward through admitted copies, and requests actual
arguments when a return summary or dispatch requirement uses them:

```prolog
Live(p, src) :-
    Live(p, dst), Copy(p, dst, src).

Live(p, a) :-
    Owner(c, p), Apply(c, q), Result(c, r, dst),
    Live(p, dst), Ret(q, r, Arg(i)), Actual(c, i, a).

Live(p, a) :-
    Owner(c, p), Apply(c, q),
    Need(q, i, s, k), Actual(c, i, a).
```

Gating origin propagation by this demand relation computes a positive least
fixpoint over relevant vertices. It does not eagerly compute variable-to-variable
reachability or general heap points-to closure. In the worst case every vertex
is relevant, so demand restriction is an optimization with an explicit worst
case, not a complexity guarantee.

## Optional extension: track fields of fresh local objects

Once the pathless kernel has been evaluated, a possible extension is to track a
value placed in a fresh object's field by a constructor and recovered by a
getter. This could discover calls such as `h.get().run()` without general heap
closure. I would keep it out of the first experiment because **the existing CIR
cannot certify field immutability**. A field is represented by its symbol/path;
the generic representation does not carry a sufficient contract covering
final/readonly status, all writes, constructor initialization, and
reflective/native interference.

A narrow extension could expose:

```text
InitArg(constructor, field, argument)
ReturnsField(getter, result_slot, receiver_argument, field)
```

For:

```text
Holder(x) { this.target = x; }
Holder.get() { return this.target; }

h = new Holder(new A);
h.get().run();
```

At the constructor call, retain a field binding associated with that particular
fresh object:

```prolog
FieldOrigin(p, object, field, o) :-
    ConstructorCall(c, p, object, ctor),
    CertifiedInitArg(ctor, field, i),
    Actual(c, i, a),
    Origin(p, a, o).
```

A getter application can consume that binding using the same receiver object.

Recognize initialization as an `Assign` to the constructor
receiver's field path and a getter as an assignment/return reading that path.
Normalize first and respect frontend conventions: C's `p->f` is `p.deref.f`,
unlike a managed object's field path. Stores accumulate possible values rather
than killing earlier values.

Heap propagation also needs direction and scope. In the IR, labels arriving by
copies have scope `Fixed`; stores introduce scope `All`, permitting backward
propagation of non-empty suffixes except `#value` itself (S6–S8). Two stores to
the same destination can merge suffix labels (S9). A heap extension must account
for these rules or explicitly restrict them; making every copy bidirectional
would mix receiver evidence from otherwise separate values.

Do **not** store `FieldType(Holder,target,A)` globally. Two `Holder` instances
initialized with different targets would contaminate one another.

An object-class tag identifies a class, not an individual allocation. Preserve
the allocation/constructor-site relationship explicitly for this experiment;
even a site represents multiple instances inside loops or recursion. Do not
claim per-instance field correlation from `Type(T)` or a shared allocation site.

The initial scope of this extension could be a fresh local allocation, a
recognized constructor, explicit field initialization, and an audited alias/copy
pattern. Constructor chains, escaping objects, arbitrary mutation, containers, and returned object
structures can remain unresolved. If certification requires scanning writes or
analyzing escapes, charge that work to the analysis. “No write has been
discovered yet” is not a positive immutability certificate.

Returning parameterized objects would require a richer summary than
`ReturnsType(Holder)`: the field bindings must survive the return. Unbounded
nested object-shape terms would defeat the proposed finite domain.

## Expected costs and scaling risks of the proposed kernel

The finite vocabulary gives the pathless kernel a termination argument, but not
a guarantee that it will be cheap. The following bounds identify products the
experiment would need to measure. Let:

- `P`: procedures; `S`: callsites.
- `a`: maximum input arity; `r`: maximum return arity.
- `K`: distinct admitted type/function evidence atoms.
- `V`: demanded SSA values; `G`: admitted copy edges.
- `D`: discovered `Apply(callsite,procedure)` pairs.

Loose relation-size bounds are:

| Relation | Worst-case rows |
| --- | --- |
| `Origin` | `O(V(a + K))` |
| `Ret` | `O(Pr(a + K))` |
| `Need` | `O(PaS)`, with the key determined by its original site |
| `Apply` / `Edge` | `O(SP)` each |

Remaining work includes copying origins across `G` edges; pairing callee return
alternatives with actual origins at `D` applications; and pairing requirements
with actual origins. A loose requirement-composition/discharge bound is
`O(DaS(a + K))`. These are real products even with exact indexes.

Carrying `original_site` is not free: a chain of wrappers containing distinct
dispatch sites can produce quadratically many propagated requirements. If that
dominates, factor requirements into a graph of formal-to-formal forwarding links
and propagate evidence through it, retaining original sites at the leaves. Keep
such evidence separate from reusable return origins. The existing
`critical_summary`/`resolvent` design already suggests this factorization.

Exact dispatch lookup has a cost too: uncached lookup walks relevant ancestry,
and cached results can approach the product of observed classes and signatures.
Avoid materializing all subtype pairs or all class/signature combinations
beforehand.

All these domains are finite. Recursion adds facts until saturation:

- A recursive identity wrapper with a base return can derive `ReturnsArg`.
- Pure recursive forwarding with no evidence seed invents no evidence.
- Recursive requirements revisit existing tuples rather than extend a call
  string.
- A recursive factory's allocation evidence may circulate without proving
  termination of an execution.

If fields are added, use an explicitly finite vocabulary—such as selected
one-field projections. Do not recursively generate arbitrary access paths or
nested summary terms. A timeout may report an incomplete computation; it must
not define the analysis result.

## Proposed policy after establishing a baseline

This is a development policy for evolving the design, not a call-discovery
mechanism or a prerequisite to trying it. It follows the Recommendation's
distinction between the initial reset and subsequent extensions. The initial
reset may discard any existing mechanism and its discoveries to establish a useful,
scalable baseline. Comparing with the current engine is valuable for understanding
coverage changes, but edge inclusion is not a release gate for this redesign.

After that reset, default to additive discovery rules and stable evidence
contracts. For a change intended to preserve results, the target for supported
input `I` and fixed modeling assumptions is:

```text
Edges_old(I) ⊆ Edges_new(I)
```

Positivity ensures monotonic growth **within one fixed rule system**. Adding
positive rules also preserves its old derivations when the input facts, domains,
and existing rules keep their meanings. Deleting rules, changing extraction,
narrowing admissible evidence, or replacing an abstraction can lose edges even
if the resulting analysis is still positive Datalog.

I propose the following policy for future changes:

- Prefer independent rules for newly supported patterns. Avoid making a new
  mechanism disable an older discovery merely because it now handles that site.
- For representation or algorithm replacements claimed to preserve results,
  explain how old derivations translate. Map old intermediate facts into the new
  representation and show how their rule applications can be reproduced. Use
  edge-set regression checks as evidence, not as a universal proof.
- Permit intentional losses for corrected modeling, precision changes, or a
  justified scalability tradeoff. Identify the affected derivations and record
  the reason and measured impact. Do not describe such a change as preserving
  results merely because total coverage increased.
- Do not keep a costly compatibility engine solely to maintain the edge set.
  The policy should support a scalable analysis, not prevent structural changes.

For example, replacing `Ret(p,r,Arg(i))` with a richer summary should retain its
callsite-substitution derivation. Adding immutable-field support should add
discoveries without disabling existing pathless returns. Conversely, removing
the current CHA-derived subclass targets is an intentional part of the initial
reset and requires no compatibility rule.

Better total coverage, a more precise abstraction, or unchanged taint findings
does not establish edge inclusion. Inclusion is the default policy, with
explicit exceptions for intentional changes.

## How to evaluate the proposal: the smallest useful experiment

I suggest testing Ideas 1 and 2 together before adding heap support or guarded
return effects. The experiment would answer whether this restricted kernel
discovers useful call edges with substantially less intermediate work:

1. **Build the pathless kernel separately**, over post-SSA CIR, with direct calls,
   admitted copies/phis, explicit type/function evidence, return origins, and
   dispatch requirements. Use the current `Assign`/`Exp::Path` extractor above.
   No heap extension or guarded return effects initially.
2. **Use exact evidenced-class lookup**, independently of the existing CHA
   preprocessing. Start with a clearly supported Java dispatch subset and
   function-address calls.
3. **Exercise focused fixtures:** two identity callers with different types;
   factory chains; forwarding wrappers; recursive wrappers with and without
   seeds; branch-dependent factories; parameter-selected callees with different
   return effects; and `new Base` with an overriding subclass. Add literal
   return/argument evidence, copies versus `value(..)`, a field read that is an
   IR copy but unsupported by the kernel, and a supported method-handle constant.
   Assert both discoveries and intended unresolved cases.
4. **Measure on a real import**, plus synthetic sweeps of wrapper depth,
   dispatch-site count, parameter count, and evidence alternatives. The synthetic
   sweeps are necessary to expose the `Need` and origin products.

Use the [semantics corpus](frontends/ctadl-flowy/tests/semantics) for small
extraction and callsite-substitution fixtures. The
[reference evaluator](ctadl-ir/src/semantics/mod.rs) can help validate these,
subject to three limits:

- It unfolds call instances with depth and path-length bounds. Use it only on
  small fixtures and inspect `Outcome::truncated`; a bounded absence does not
  establish that an edge is absent, especially for recursion.
- It handles explicit direct and function-pointer calls, but does not accept a
  class hierarchy and reports virtual call styles as unsupported. Exact class
  dispatch needs dedicated tests against the intended lookup contract.
- It reports taint observations rather than a resolved call-edge set.
  Call-graph validation needs expected-edge assertions or a test-only edge
  recorder; taint parity alone is insufficient.

Fix and record the input-lowering and pass configuration for each comparison.
Start semantic fixtures with SSA conversion alone; separately test the
production pipeline. The
[approximations register](docs/limitations.md#approximations-register) names
`copy_prop` additions and `mem2reg` losses, so the pass configuration can change
target evidence before discovery runs. Include that work in end-to-end timing
and record frontend conventions, models, and pass options with each baseline.

Record end-to-end analysis time, extraction/lookup time, peak RSS, relation and
index sizes, join candidates versus inserted facts, fixpoint iterations, and
unresolved-site counts. Use the existing
[profile machinery](ctadl-index/src/index_engine/profile.rs) where applicable.
Preserve the distinction between an indirect site with no discovered target and
a discovered target with no body/model. The current
[index diagnostics](ctadl-ascent/src/cli/mod.rs) already report these separately;
the kernel should retain that visibility and identify unsupported evidence
patterns.

Compare actual edge sets using stable procedure/callsite identities:

```text
old_only = old_edges − new_edges
new_only = new_edges − old_edges
```

For the initial reset, classify old-only edges by derivation mechanism to
understand lost coverage; zero old-only edges is not required. Give the new
kernel its own expected-edge fixtures and record its baseline results. For later
changes intended to preserve results, require zero old-only edges in regression
tests; intentional exceptions need an explanation and updated expectations.
Also vary fact order and serial/parallel evaluation to check that results are
invariant.

The experiment succeeds if it demonstrates useful discoveries with substantially
smaller intermediate relations, acceptable growth on the synthetic sweeps, and
no hidden closure in extraction or lookup. Document what it discovers and leaves
unresolved, then use the validated kernel as the baseline for generally additive
extensions. Adoption does not depend on recovering the existing engine's edge
set.
