# Call-graph discovery ideas

Design proposal. It is written against the IR semantics in [ir.md](docs/ir.md)
and the framing in [spec.md](docs/spec.md), not against the current engine. The
analysis proposed here has not been implemented or benchmarked.

## Recommendation

Make the call graph its own analysis, computed **before** taint, in positive
Datalog, and start it as a deliberate **under-approximation**: a small kernel
that follows class and function labels only along flows it can follow cheaply
and exactly, resolves calls only from that evidence, and reports everything else
as unresolved. Grow it one admitted flow pattern at a time, each measured before
it stays.

The kernel:

1. **Cells, not access paths.** Every SSA variable, formal, return slot and
   global is a cell. The kernel's flow edges are directional and connect cells
   only through empty-path copies, phis, call bindings and pass-through
   summaries. Memory is not in the kernel.
2. **Only two kinds of label.** Class labels and function labels flow over the
   cell graph. A label's derivation is a chain of cells from a `new C` or
   `ptr<f>` to the call site, which an analyst can read.
3. **Hierarchy as a filter, never a generator.** A virtual call resolves by
   exact, upward-only lookup from an evidenced class. Declared types prune
   labels that cannot be there. No class-hierarchy analysis, no fallback that
   enumerates subclasses.
4. **Context sensitivity only where it is cheap and matters:** pass-through
   summaries (formal `i` reaches return `r`) keep wrappers and identity-like
   utilities from mixing their callers. Everything else is context-insensitive.
5. **Taint runs afterwards on the fixed edge set.** What the kernel does not
   resolve stays unresolved in taint too, and is reported. The taint engine no
   longer propagates target labels or resolves calls.

Memory (loads, stores, `value(p)`, tables, fields) is admitted afterwards, in
stages, from the cheapest and most contained pattern outward. Each stage is a
separate experiment with its own measurement, and an over-approximating memory
abstraction (unification or allocation-site points-to) is the last candidate,
not the first, because its growth on real inputs is the open question.

Changing call results during this redesign is acceptable. There is no
requirement to reproduce the existing engine's edges or retain its mechanisms.

## Objectives and constraints

The objective is call-graph discovery. Leaving a call unresolved is acceptable.
There should be no CHA or other fallback that enumerates speculative callees.
Avoiding speculative enumeration does not guarantee execution feasibility.

Discovery should have declarative, evaluation-order-independent semantics: a
least fixpoint of a monotone system. Stopping after a budget is not a semantic
definition.

Structural efficiency means no call strings, no access-path suffix products, no
eager closure hidden in preprocessing. Remaining products and worst-case growth
must be explicit. Reusable per-function summaries should compose without
re-expanding bodies.

All given code is analyzed ([spec.md](docs/spec.md) §1): no pruning by
reachability from entry points, since hidden payloads must still be found.

## Why a separate, under-approximating analysis

The current design makes call resolution part of the taint fixpoint (S11:
labels create assignments). That is elegant and it is the cost center: target
labels ride the same access-path machinery as taint, with suffix extension,
alias summaries and conditional rows, and the tag/edge join has been the single
most expensive relation in recorded profiles. Access paths exist because
analysts want to see `p.f.g` in a finding. Call targets do not need them.

Two ways out exist. An **over-approximating** discovery (a coarse memory
abstraction such as unification, with every flow admitted) would contain every
edge S11 derives, and could justify taint consuming its edges by soundness. Its
problem is growth: unification fuses unrelated objects through shared utilities
and `Object`-typed hubs, and the label product at those hubs is bounded only by
the number of classes reaching them. On a large app with all code analyzed and
no entry-point pruning, that is the blow-up the redesign is meant to avoid.

An **under-approximating** discovery admits only flows it can follow exactly
and cheaply, and leaves the rest unresolved. Its edge set is smaller than S11's.
That is acceptable under the objectives: an unresolved call is a reported
outcome, not a failure. Taint consumes the kernel's edges by decision, not by a
soundness argument, and the unresolved sites are visible in taint's output as
they are today for calls with no target. This still removes target-label
propagation, the tag/edge join and decision-guarded summaries from the taint
engine, which is where the recorded cost was. Index construction becomes two
strata: discovery, then taint over a fixed call graph.

Starting from the under-approximation has a second advantage: every later
admission of a flow pattern only adds edges, so the additive policy at the end
of this document applies from the first version, and each addition can be
measured against the previous version on the same inputs.

## The kernel

### Cells

```text
cell := Var(v)        an SSA variable, formal, return slot or global
```

Nothing else, in the kernel. Globals are ordinary cells shared by every
function (G2), with nothing passed for them at calls (G5). A global whose
own value (empty path) holds a function label resolves calls through it; its
storage below (`$g.deref`, `$C.f`) is memory and is not in the kernel.

### Extraction

Extraction is linear and admits, after S5 normalization:

```text
copy(d, s)              d = s with empty paths on both sides, and each phi operand
seed(v, Class(C))       v = new C
seed(v, Fun(h))         v = ptr<h>
site(c, g, Direct(h) | Indirect(v) | Virtual(v, sig))      call c in function g
actual(c, i, a)   result(c, r, d)   formal(h, i, p)   ret(h, r, v)
static_type(v, S)   subtype(C, S)   declares(C, sig, m)   super(C, D)
```

Not admitted: loads (`x = y.f`, `x = y.[8]`, `x = y.deref`), stores, `value(p)`,
and anything with a non-empty path. Several phi operands give separate
alternatives; they do not make the sources aliases. Literal arguments and
literal return operands get synthetic cells so `return ptr<f>` and
`invoke(ptr<f>)` keep their evidence. Receiver arguments use the same slot
convention as the front end; result slots include the exception slot where a
language has one.

### Flow

Flow edges are directional and carry a mode that says what happens to a label's
provenance when it crosses:

```prolog
flow(s, d, Keep)      :- copy(d, s).
flow(a, p, Enter)     :- edge(c, h), actual(c, i, a), formal(h, i, p).
flow(v, d, Exit)      :- edge(c, h), result(c, r, d), ret(h, r, v).
```

Labels carry one bit of provenance, `Own` or `Formal`: whether, within the
function that holds the cell, the label arrived through one of that function's
formals.

```prolog
holds(v, T, Own)      :- seed(v, T).
holds(d, T, b)        :- flow(s, d, Keep),  holds(s, T, b),   ok(d, T).
holds(d, T, Formal)   :- flow(s, d, Enter), holds(s, T, _),   ok(d, T).
holds(d, T, Own)      :- flow(s, d, Exit),  holds(s, T, Own), ok(d, T).
```

A return exports only labels the callee produced itself. A label that entered
through a formal leaves through the pass-through mechanism below, which routes
it caller-locally. This is what keeps `a = id(new A); b = id(new B)` from
giving `a` the label `B`. Formals themselves still receive every caller's
labels, which is what calls inside the callee need.

`ok(d, T)` is the type filter:

```prolog
ok(d, T)              :- !static_type(d, _).
ok(d, Class(C))       :- static_type(d, S), subtype(C, S).
ok(d, Fun(h))         :- static_type(d, S), prototype_fits(h, S).
```

This uses the hierarchy to remove labels a declared type rules out, the dual of
CHA. For bytecode the declared types of fields, parameters, locals and
`checkcast` are known and the filter is strong. For binaries it is a prototype
check where a signature is known, and nothing otherwise.

### Pass-through summaries

A pass-through summary is label-free: it records that a function's formal `i`
reaches its return slot `r` through copies and through callees' pass-throughs.
At a call site it becomes a direct flow edge from actual to result inside the
caller, so labels never go into the callee and back out.

```prolog
reach(h, p, p)        :- formal(h, _, p).
reach(h, p, d)        :- reach(h, p, s), copy(d, s), owner(d, h).
reach(h, p, d)        :- reach(h, p, a), owner(c, h), edge(c, k),
                         actual(c, i, a), through(k, i, r), result(c, r, d).
through(h, i, r)      :- formal(h, i, p), ret(h, r, v), reach(h, p, v).
flow(a, d, Keep)      :- edge(c, k), through(k, i, r), actual(c, i, a), result(c, r, d).
```

A recursive identity wrapper derives `through` from its base case; a wrapper
that only forwards and never returns a formal derives nothing. The relation
sizes are `Σ_h arity(h) × |vars(h)|` for `reach` and `O(P × a × r)` for
`through`.

What this does not do: `apply(f, x) { return f(x); }` called once with
`identity` and once with `makeB` gives both callers the label `B`, because the
return effect of `makeB` is `Own` inside `apply`. Separating that needs a
guarded effect ("if formal 0 is `identity`, return formal 1"), which is S14's
guarded summary. It is deliberately not in the kernel; see
[Guards for hybrid inlining](#guards-for-hybrid-inlining).

There is no relation routing *dispatch requirements* through wrappers. A call
edge belongs to a syntactic site, and the edges a site gets are the union over
its contexts whether or not the context is tracked, so such routing never
changes the edge set. Context-insensitive formals give the same edges.

### Dispatch

```prolog
edge(c, h)            :- site(c, _, Direct(h)).
edge(c, h)            :- site(c, _, Indirect(v)), holds(v, Fun(h), _).
edge(c, m)            :- site(c, _, Virtual(v, sig)), holds(v, Class(C), _),
                         lookup(C, sig, m).

wanted(C, sig)        :- site(c, _, Virtual(v, sig)), holds(v, Class(C), _).
wanted(D, sig)        :- wanted(C, sig), !declares(C, sig, _), super(C, D).
lookup(C, sig, m)     :- wanted(C, sig), declares(C, sig, m).
lookup(C, sig, m)     :- wanted(C, sig), !declares(C, sig, _), super(C, D),
                         lookup(D, sig, m).
```

`lookup` is exact and demand-driven: it walks up from the evidenced class only,
for signatures actually called on it, and never down. Negation is only on input
facts, so the program remains stratified. Interface defaults, bridge methods and
CIL's explicit interface implementations need language-specific `declares`
facts; where a language's rule is ambiguous or unmodeled the site stays
unresolved and says why.

S11 also lets a call through `e` consume labels at `e.#value`. That is a memory
read and is not in the kernel; such sites are unresolved until the stage that
admits it.

Only Java and CIL need `lookup`. A C `ops` struct, a C++ vtable, a Lua method
table, a .NET delegate's target and a GOT entry are all memory holding function
labels, so in the kernel calls through them are unresolved. They are the first
thing the memory stages below recover, and they are the reason the stages are
ordered as they are.

### Reporting

The final stratum, after the fixpoint, classifies what was not resolved (N3, no
silent loss), and in particular *why*, so that the unresolved population tells
us which flow pattern to admit next:

```prolog
unresolved(c, NoEvidence)            :- site(c, _, Indirect(v)), !holds(v, _, _).
unresolved(c, NoEvidence)            :- site(c, _, Virtual(v, _)), !holds(v, _, _).
unresolved(c, NoLookup(C, sig))      :- site(c, _, Virtual(v, sig)), holds(v, Class(C), _),
                                        !lookup(C, sig, _).
unresolved(c, NoBody(h))             :- edge(c, h), !has_body(h), !has_model(h).
blocked(v, Load(f))                  :- a call operand v, or a cell flowing to one, is
                                        defined by an unadmitted load through segment f
blocked(v, Store)                    :- ... or by a store-mediated flow
blocked(v, Value)                    :- ... or by value(p)
hub(cell, n)                         :- n = #{T : holds(cell, T, _)}, n > N
```

`blocked` is computed by a backward walk from unresolved operands over the
kernel's own edges to the first unadmitted statement, so it is linear in the
cells it visits. It is the measurement that drives the staging: counts of
unresolved sites by blocking pattern, on real imports, decide what to admit
next. `hub` is a diagnostic, not a cutoff; `N` is recorded with the index like
any other knob (N2, N5).

## Scaling of the kernel

| Quantity | Bound |
| --- | --- |
| Cells | variables, formals, return slots, globals |
| `flow` | copies + Σ over resolved edges of arity + `D × a × r` pass-through instantiations |
| `holds` | cells × labels reaching × 2; the only product that can be large |
| `reach`, `through` | `Σ_h arity(h) × vars(h)`; `P × a × r` |
| `lookup` | wanted (class, signature) pairs × hierarchy depth |
| `edge` | sites × targets evidenced at the site |

There is no suffix extension, no path concatenation, no alias summary, no
conditional row and no memory product. The one product that can grow is
`holds` at hub cells reached by many classes: formals of `Object` methods, and
anything downstream of them. Type filtering removes most labels at typed cells,
pass-through keeps utility functions from becoming hubs for their callers'
results, and labels are classes rather than allocation sites, so the ceiling at
a hub is the number of classes with a copy path to it. Without memory in the
kernel, most of the classic hub formation (through containers) cannot happen
at all. `hub` says whether the remaining hubs matter.

## Admitting memory, in stages

Each stage is a separate experiment: the kernel plus one admitted pattern,
measured on the same inputs as the previous stage, kept if the gain in resolved
sites justifies the measured growth. The order is from most contained to least.

### Stage 1: global tables

A global is already one cell for the whole project (G2, G3), so its storage
needs no aliasing to find: `$tbl.[8]` is one location whatever function names
it. Admit

```text
gstore($g, f, s)        $g.f = s        one segment, including load-time
                                        initialization of .data, vtable symbols,
                                        GOT and PLT entries
gload(d, $g, f)         d = $g.f
```

with a cell `Mem($g, f)` per global and segment, `flow(s, Mem($g, f), Erase)`
and `flow(Mem($g, f), d, Keep)`, where `Erase` sets the provenance to `Own`.
Two stores to one location merge (S9). This has no unification, no products
beyond the kernel's, and it resolves calls through statically initialized
function tables, C++ vtables reached through a global, GOT entries, and
`static` ops structures in C. For binaries this is most of what there is.

A pointer *to* a global's storage (`p = $tbl` then `p.[8]`) is not admitted
here: that is a load through a non-global base. The next stage covers the
common shape of it.

### Stage 2: one-hop loads from a seeded base

Admit `d = b.f` when `b` holds a label whose storage is known: `b` is a global
(stage 1) or `b` was defined by `new C` in the same function with no
intervening escape, and the only writes to `b.f` are in that function. This is
the constructor-initialized-field case (`h = new Holder(x); h.get()`) and the
locally built table case (`ops.read = &f; use(ops)`). The cell is
`Mem(Alloc(site), f)`, keyed by allocation site, with directional flow in and
out. It is under-approximate because it declines any base it cannot pin to a
site. The "no intervening escape" and "only writes" conditions are syntactic
checks over the function body, charged to extraction, and refusing when unsure.

### Stage 3: `#value` and `value(p)`

Admit the `#value` segment as a one-hop load/store on the same terms as stage
2, so a call through `e` also consumes `Mem(Of(e), #value)` where `Of(e)` is
pinned, and `y = value(p)` contributes the store `y.#value = p`. Value-only
arithmetic is kept as a separate evidence kind, not upgraded to an exact
receiver, per S4.

### Stage 4: a general memory abstraction, measured

Only after the stages above have been measured, try an over-approximating
memory model for whatever remains blocked, as a separate stratum whose output
is compared to, not merged with, the kernel's:

- **One-level flow** (directional variables, unified memory structure below
  them, Steensgaard-style). Near-linear structure; the risk is fusing unrelated
  objects through shared utilities and `Object` hubs, and the `holds` product
  at the fused cells. Expressible in Datalog as a lattice program over least
  representatives, so it stays order-independent. Its summaries compose by
  unification, which matters for `--summary` (F7).
- **Allocation-site points-to** (Andersen, as in Doop's context-insensitive
  baseline). More precise, cubic worst case, no compositional summaries.

Either one, if adopted at all, is a *filter-and-report* layer: an edge it finds
that the kernel did not is admitted only through an explicit, named rule, and
the growth of its relations on the same inputs is the acceptance criterion.
The expectation going in is that it blows up on the large inputs; the point of
running it is to know by how much and at which cells, since `hub` and the
collapse diagnostics name them.

## Other later strata

### Guards for hybrid inlining

Hybrid inlining (spec §4.2) makes taint flows conditional on the target that
reached a formal. With the call graph computed separately, taint no longer sees
targets flow. Discovery can export what it knows instead: replace the `Formal`
bit with `Formal(i)` and derive

```prolog
decision(c, m, i)   :- edge(c, m) derived from a target with provenance Formal(i).
```

This costs a factor of `a` on `holds` and gives the taint engine exactly the
facts a guarded summary needs (this edge exists in instances where formal `i`
carries this target) without computing target flow itself. Whether taint should
keep guards at all is a separate decision; the kernel supports both.

### Library summaries

A function's contribution to the kernel is its `through` facts, the labels it
seeds into its return slots and globals, and the edges inside it. That is
small and composes at a call site by substitution. A separately indexed library
(`--summary`, F7) can ship it. The memory stages add `Mem` cells of globals and
pinned allocations, still without aliasing summaries; only stage 4 needs shape
summaries.

### Native evidence

Binaries have fewer seeds than bytecode and the front end must produce them:
address-taken functions (relocations and immediates equal to a function
entry), initialized pointer tables, PLT and GOT entries as cells holding labels
for functions without bodies, and vtable symbols as class labels for C++. All
of these are stage-1 global stores, so the kernel does not change.

## Relation to the earlier draft

The earlier draft proposed a pathless kernel with symbolic return origins
(`Ret(p, r, Arg(i))`) and dispatch requirements (`Need`) carried through
wrappers, and kept memory out. This proposal is that kernel, with four changes:

- `Ret(Arg(i))` becomes `through`, phrased as one flow mode among others, and
  the `Own`/`Formal` provenance bit replaces symbolic origins at cells.
- `Need` is dropped, because it cannot change the edge set (see
  [Pass-through summaries](#pass-through-summaries)).
- A type filter is added. The earlier draft used the hierarchy only for lookup;
  using it to prune is cheap and is the main precision device available
  without context sensitivity.
- Memory has a staged roadmap, ordered by containment, with globals first
  because they need no aliasing at all, and a `blocked` diagnostic that says
  which pattern to admit next. The earlier draft's immutable-field extension is
  stage 2, with the certification problem replaced by a syntactic refusal.

The two drafts agree on the stance: start under-approximate, resolve only from
evidence, report the rest, grow additively.

## Evaluation

The question is whether the kernel discovers useful edges with intermediate
relations that are small and grow predictably, and what the unresolved
population looks like.

1. **Implement the kernel in Ascent** as its own engine over the post-SSA IR,
   with `edge`, `unresolved`, `blocked` and `hub` as outputs. No taint, no
   memory.
2. **Fixtures**, each asserting discovered edges and intended unresolved sites:
   two identity callers with different classes; factory chains; forwarding
   wrappers; recursive wrappers with and without a base case; a
   branch-dependent factory; `apply(f, x)` with two callers; `new Base` with an
   overriding subclass (the override must not appear); a field read that is an
   IR copy but blocked in the kernel; `value(p)` blocked; a JVM method-handle
   constant; a `checkcast`-filtered hub; a global function table (unresolved in
   the kernel, resolved at stage 1).
3. **Order invariance:** shuffle input facts and run serial and parallel; the
   outputs must be identical.
4. **Measure** on the regression inputs and on real imports: wall time, peak
   RSS, sizes of `flow`, `holds`, `through`, `edge`; join candidates versus
   inserted facts; hubs; and the `unresolved` and `blocked` populations by
   reason. Synthetic sweeps of wrapper depth, hub fan-in and class count expose
   the products in the table above.
5. **Compare edge sets** with the current engine using stable site and
   procedure identities:

   ```text
   old_only = old_edges − new_edges
   new_only = new_edges − old_edges
   ```

   Classify `old_only` by the mechanism that produced the edge (CHA, guarded
   summary, memory). CHA-only edges are intended losses; memory losses are
   expected and sized by `blocked`.
6. **Stratified taint:** run the existing taint engine with target-label
   propagation disabled and the kernel's edges supplied as facts. Compare
   findings and cost against the coupled engine. This is the structural win,
   and it does not wait for the memory stages.
7. **Stages 1 to 3**, each repeating steps 4 to 6, kept on measured gain.
8. **Stage 4**, run as a comparison, not merged.

The kernel experiment succeeds if it finds the edges the fixtures expect, the
real-import comparison loses only CHA, guard-dependent and memory edges, `holds`
and `flow` stay within a small factor of program size, and the stratified taint
run is materially cheaper than the coupled one. Adoption does not depend on
recovering the current engine's edge set.

## Policy after a baseline exists

The initial reset may discard any existing mechanism and its discoveries. After
that, the default is additive: new rules add edges, and a change meant to
preserve results satisfies `Edges_old(I) ⊆ Edges_new(I)` for supported input `I`
under fixed modeling assumptions, checked by regression. Starting from the
under-approximation makes this the natural shape of every memory stage.
Positivity guarantees inclusion only within one rule system; a change of
abstraction, extraction or evidence must explain how old derivations translate,
and an intentional loss (corrected modeling, a precision or scalability
tradeoff) is recorded with its measured impact. Better total coverage or
unchanged taint findings do not establish inclusion.
