# Call-graph discovery ideas

Analysis of the repository at commit `286e7993`, recorded on 2026-11-06.
This is a design proposal, not an implemented replacement. The performance
observations below come from inspected records, not a new benchmark run.

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

The existing representation already has several useful ingredients:

- `CallAssign` distinguishes direct, function-pointer, Java, CIL, and Lua calls.
  Arguments and return slots are explicit.
- `Exp::ObjectRef` carries object-class tags and function addresses. JVM/Dex
  allocations, CIL allocations/boxing, and recognized native function addresses
  produce these tags.
- SSA preserves some assignment ordering. `Load`, `Store`, and functional
  `Update` remain distinguishable in MIR.
- Return types describe **return arity**, not concrete runtime classes.
  `ParameterType` describes sharing behavior, not a receiver's class. Neither
  justifies `ReturnsType(T)`.
- MIR branches are nondeterministic `Goto` successors. Phi operands retain
  predecessors in MIR, but codegen projects them into ordinary assignments,
  discarding that association. There are no branch predicates to prove general
  execution feasibility.

See [MIR statements and expressions](ctadl-ir/src/mir/mod.rs),
[call styles](ctadl-ir/src/mir/call.rs),
[terminators](ctadl-ir/src/mir/terminator.rs), and
[phi lowering](ctadl-ascent/src/codegen/mod.rs).

The current analysis is already compositional and does **not** enumerate call
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

## Dispatch must use specific evidence

The first semantic change must be dispatch lookup. Currently,
`emit_callee_resolvents` exports `cha.resolvents`, whose construction includes:

```prolog
cha_resolve(supertype, signature, implementation) :-
    inherited_method(subtype, signature, implementation),
    subtype_of(subtype, supertype).
```

Consequently, even when discovery supplies the tag for `new A`, lookup under `A`
can produce overriding methods in subclasses of `A`. Merely requiring a tag—or
selecting `--strategy hi`—does not eliminate speculative target enumeration.
See [resolvent emission and construction](ctadl-ascent/src/codegen/mod.rs).

For the proposed analysis, use a different relation:

```text
Dispatch(Type(T), signature, method)
Dispatch(Function(f), function_call_key, f)
```

`Dispatch(Type(T), …)` means lookup for the **specific evidenced runtime class
`T`**, including applicable inherited implementations. It must not enumerate
descendants of `T`.

Implement this as demand-driven, memoized lookup for encountered
`(T, signature)` pairs. The hierarchy walk, override handling, and
interface-default resolution must have deterministic semantics over fixed
metadata. Unsupported or ambiguous language cases can remain unresolved.
Preserve declaring-type and dispatch-kind information where needed; the existing
deferred key of method name and descriptor is not the whole invocation contract.

This uses hierarchy metadata to implement dispatch, without using hierarchy
membership to invent receiver possibilities. It still does not prove that the
receiver reaches that instruction in an execution.

## A finite discovery kernel

For a first discovery kernel, use the following finite symbolic vocabulary:

```text
Arg(i)          value comes from this procedure's input i
Type(T)         concrete-class evidence from an admitted producer
Function(f)     evidence of this function address
```

These are finite atoms, not recursively nested terms. Multiple atoms mean
alternatives. The constructor notation in the sketches is shorthand for tagged
finite-domain values, not unrestricted term construction.

Extract a separate discovery fact base from post-SSA MIR:

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

Do not take general taint summaries as proof of identity preservation. A
propagation model saying “argument taints return” need not mean the returned
object has the argument's runtime class or function address. Likewise,
`Exp::Value`, arithmetic dependencies, and `ParamFlow` write-backs must not
accidentally become identity copies. Begin with admitted copy/phi operations and
explicit evidence producers; report unsupported lowering patterns.

### Return summaries

Return summaries should remain symbolic until each callsite applies them. Write
them as `Ret(p, slot, origin)`, accommodating the IR's multiple returns:

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

### Dispatch requirements

Dispatch requirements should record obligations without importing callers'
values into reusable summaries. For the pathless experiment:

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

The small kernel should discover the internal call edges and leave their
caller-dependent return effects unresolved. That is a deliberate coverage
limitation.

A later extension could represent a guarded return effect:

```text
if Arg(0) = Function(identity), return Arg(1)
```

But this should be treated as a separate abstraction. Composing effects
requiring two independent parameter facts needs conjunctions or an explicit
approximation. The current
[`Cond::and`](ctadl-index/src/index_engine/decision.rs) explicitly declines
compositions needing two distinct decisions together. Replacing that behavior
with unrestricted condition formulas could introduce combinatorial growth.

### Demand restriction

Demand restriction can keep the kernel focused without defining semantics by a
budget. Seed `Live` at indirect-call operands and return operands, propagate it
backward through admitted copies, and request actual arguments when a return
summary or dispatch requirement uses them:

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

Gate origin propagation by this demand relation. This computes a positive least
fixpoint over relevant vertices. It does not eagerly compute variable-to-variable
reachability or general heap points-to closure. In the worst case every vertex
is relevant, so demand restriction is an optimization with an explicit worst
case, not a complexity guarantee.

## Narrow heap support

The immutable-field idea is useful, but **the existing MIR cannot certify
immutability**. A field is represented by its symbol/path; the generic
representation does not carry a sufficient contract covering final/readonly
status, all writes, constructor initialization, and reflective/native
interference.

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

Do **not** store `FieldType(Holder,target,A)` globally. Two `Holder` instances
initialized with different targets would contaminate one another.

Initially restrict this to a fresh local allocation, a recognized constructor,
explicit field initialization, and an audited alias/copy pattern. Constructor
chains, escaping objects, arbitrary mutation, containers, and returned object
structures can remain unresolved. If certification requires scanning writes or
analyzing escapes, charge that work to the analysis. “No write has been
discovered yet” is not a positive immutability certificate.

Returning parameterized objects would require a richer summary than
`ReturnsType(Holder)`: the field bindings must survive the return. Unbounded
nested object-shape terms would defeat the proposed finite domain.

## Termination and cost

Termination and cost should be stated separately. For the pathless kernel, let:

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

## Preservation across versions

Separate the initial reset from subsequent evolution. The initial reset may
discard any existing mechanism and its discoveries to establish a useful,
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

Use the following policy for future changes:

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
does not establish edge inclusion. Keep that distinction explicit even though
inclusion is now a default policy rather than an absolute requirement.

## Smallest useful experiment

1. **Build the pathless kernel separately**, over post-SSA MIR, with direct calls,
   admitted copies/phis, explicit type/function evidence, return origins, and
   dispatch requirements. No heap extension or guarded return effects initially.
2. **Use exact evidenced-class lookup**, independently of the existing CHA
   preprocessing. Start with a clearly supported Java dispatch subset and
   function-address calls.
3. **Exercise focused fixtures:** two identity callers with different types;
   factory chains; forwarding wrappers; recursive wrappers with and without
   seeds; branch-dependent factories; parameter-selected callees with different
   return effects; and `new Base` with an overriding subclass. Assert both
   discoveries and intended unresolved cases.
4. **Measure on a real import**, plus synthetic sweeps of wrapper depth,
   dispatch-site count, parameter count, and evidence alternatives. The synthetic
   sweeps are necessary to expose the `Need` and origin products.

Record end-to-end analysis time, extraction/lookup time, peak RSS, relation and
index sizes, join candidates versus inserted facts, fixpoint iterations, and
unresolved-site counts. Use the existing
[profile machinery](ctadl-index/src/index_engine/profile.rs) where applicable.
On the headless Mac, use `/usr/bin/time -l` rather than the existing benchmark
script's `footprint -p` polling. Follow the workspace disk-space instructions
when choosing build, output, and temporary directories; keep machine-specific
paths out of project configuration and scripts.

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
