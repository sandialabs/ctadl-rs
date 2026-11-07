# Call-graph discovery ideas

Design proposal. It is written against the IR semantics in [ir.md](docs/ir.md)
and the framing in [spec.md](docs/spec.md), not against the current engine. The
analysis proposed here has not been implemented or benchmarked.

## Recommendation

Make the call graph its own analysis, computed **before** taint, in positive
Datalog over a representation built for it:

1. **Cells, not access paths.** Every SSA variable is a cell. Memory below a
   variable is a graph of cells reached through fields, offsets and `deref`.
   Flow between variables is directional; the memory structure below them is
   **unified**, as in Steensgaard's analysis. This is Das's "one level flow":
   near-linear structure, Andersen-like precision for the pointer level that
   decides calls.
2. **Only two kinds of label.** Class labels and function labels flow over the
   cell graph. Nothing else is tracked. A label's derivation is a chain of cells
   from a `new C` or `ptr<f>` to the call site, which an analyst can read.
3. **Hierarchy as a filter, never a generator.** A virtual call resolves by
   exact lookup from an evidenced class. Declared types prune labels that cannot
   be there. No class-hierarchy analysis, no fallback that enumerates subclasses.
4. **Context sensitivity only where it is cheap and matters:** pass-through
   summaries (formal `i` reaches return `r`) keep wrappers and identity-like
   utilities from mixing their callers. Everything else is context-insensitive.
5. **Taint runs afterwards on the fixed edge set.** The discovery abstraction
   over-approximates the IR's label flow for class and function labels, so the
   taint engine no longer propagates target labels or resolves calls at all.

Changing call results during this redesign is acceptable. There is no
requirement to reproduce the existing engine's edges or retain its mechanisms.
Unresolved calls are reported, not guessed.

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

## Why a separate analysis, and why this abstraction

The current design makes call resolution part of the taint fixpoint (S11:
labels create assignments). That is elegant and it is the cost center: target
labels ride the same access-path machinery as taint, with suffix extension,
alias summaries and conditional rows, and the tag/edge join has been the single
most expensive relation in recorded profiles. Access paths exist because
analysts want to see `p.f.g` in a finding. Call targets do not need them: a
function pointer in `s.ops.read` needs a cell for `read` in the object `ops`
points to, not a set of path suffixes.

The proposal therefore uses a **different heap abstraction for discovery than
for taint**, chosen so that it is a quotient of the taint abstraction for the
labels discovery cares about:

- At the top level (empty path), the IR's forward rule S6 is directional and
  copies never merge their sources (S7, S16). Discovery keeps this exactly:
  directional `flow` edges between variable cells.
- Below the top level, S6 to S9 together make memory effectively shared across
  copies: a store's labels have scope `All` and move backward through every copy
  of the base, two stores to one location merge, and `#value` is shared with
  every alias of its cell. Discovery over-approximates this by unifying the
  memory structure below variables that are copied, loaded or stored into one
  another. Every location the semantics would relate, discovery relates;
  discovery also relates some the semantics keeps apart (a `Fixed` label does
  not move backward in the IR, but does under unification).

Because discovery is coarser than the semantics wherever they differ, every
target label the semantics places at a call operand, discovery places at the
corresponding cell. The call graph it produces therefore contains every edge
S11 would derive, and the taint analysis can take the edge set as input. Index
construction becomes two strata instead of one fixpoint: discovery, then taint
over a fixed call graph. The taint engine drops target-label propagation, the
tag/edge join and decision-guarded summaries (or keeps the last as an input, see
[Guards for hybrid inlining](#guards-for-hybrid-inlining)).

The precision cost is confined to memory: two objects of the same class whose
pointers ever meet share one set of field cells. The precision gains relative to
today are the removal of speculative CHA edges, and type filtering. Where the
memory merge hurts, a later refinement stratum splits objects
([Refinement](#later-strata)); it is not needed to get a baseline.

## The cell graph

### Cells and objects

```text
cell  := Var(v)                   an SSA variable, formal, return slot, global
       | Mem(obj, f)              the location reached from object obj by one
                                  segment f: a field name, an offset, deref, or
                                  #value
obj   := Alloc(site)              the object created at an allocation site
       | Of(cell)                 the object the pointer held in cell refers to
```

Every cell has one pointee object, `Of(cell)`. Objects are subject to
**unification**: `unify(o1, o2)` says they are the same abstract object, and
`Mem(o, f)` is keyed by the representative of `o`, so unified objects share one
cell per segment. The number of cells is bounded by the number of variables plus
the number of field accesses in the program text, because a `Mem` cell is
created only for a segment the program applies to a pointer, and unification
only merges. Cyclic structures (`x = x.next`) unify the object with its own
`next` pointee rather than growing a chain.

Offsets compose. A pointer cell's pointee is an object plus an offset, and `[k]`
adjusts the offset (S5: `p.[k]` is an address, whose labels are `p`'s). Memory
cells of a native object are keyed by absolute offset. If unification forces
two pointees with inconsistent offsets onto one object, that object collapses
to a single offset-insensitive cell; the collapse is reported.

### Extraction

Extraction is linear. After S5 normalization, a path of length greater than one
is split into a chain through fresh cells, so the rules only ever see one
segment:

```text
copy(d, s)                      d = s, and each phi operand
load(d, b, f)                   d = b.f
store(b, f, s)                  b.f = s
seed(v, Class(C))               v = new C;   also object(Alloc(site)),
                                unify(Of(v), Alloc(site))
seed(v, Fun(h))                 v = ptr<h>
site(c, g, Direct(h) | Indirect(v) | Virtual(v, sig))   call c in function g
actual(c, i, a)   result(c, r, d)   formal(h, i, p)   ret(h, r, v)
static_type(v, S)   subtype(C, S)   declares(C, sig, m)   super(C, D)
```

`y = value(p)` is the store `y.#value = p`. Literal arguments and literal
return operands get synthetic cells so `return ptr<f>` and `invoke(ptr<f>)` keep
their evidence. Globals are ordinary cells shared by every function (G2); nothing
is passed for them at calls (G5).

### Unification in Datalog

Union-find is not a Datalog primitive, but an equivalence closure keyed by a
least representative is a lattice program, which Ascent supports directly. The
least fixpoint is unique, so results do not depend on evaluation order or on
serial versus parallel execution.

```prolog
% rep(o, r): r is the least object id known to be in o's class (lattice: min)
rep(o, o)                                   :- object(o).
rep(o2, r)                                  :- unify(o1, o2), rep(o1, r).
rep(o1, r)                                  :- unify(o1, o2), rep(o2, r).

% unified objects share field cells: identify every Mem(o, f) with Mem(r, f)
unify(Of(Mem(o, f)), Of(Mem(r, f)))         :- has_field(o, f), rep(o, r).
```

`has_field(o, f)` is derived from the loads and stores that apply `f` to a
pointer whose pointee is `o`. As representatives only decrease, facts derived
under an old representative stay (they are harmless) and are re-derived under
the new one; the final representative carries everything.

If the lattice encoding proves slow, a union-find kept in Rust and consulted by
the rules computes the same fixpoint; the semantics is the Datalog program, not
the data structure.

### Structure rules

```prolog
unify(Of(d), Of(s))                         :- copy(d, s).
unify(Of(d), Of(Mem(Of(b), f)))             :- load(d, b, f).
unify(Of(Mem(Of(b), f)), Of(s))             :- store(b, f, s).
unify(Of(a), Of(p))                         :- edge(c, h), actual(c, i, a), formal(h, i, p).
unify(Of(v), Of(d))                         :- edge(c, h), result(c, r, d), ret(h, r, v).
```

`Mem(Of(b), f)` is read as `Mem(r, f)` for the current representative `r` of
`Of(b)`. Every statement contributes a constant number of unifications. Call
edges contribute one per argument and result, and `edge` is itself derived, so
structure and resolution are one fixpoint; adding an edge only adds
unifications and flows, so the system stays monotone.

### Flow rules

Flow edges are directional and carry a mode that says what happens to a label's
provenance when it crosses:

```prolog
flow(s, d, Keep)                            :- copy(d, s).
flow(Mem(Of(b), f), d, Keep)                :- load(d, b, f).
flow(s, Mem(Of(b), f), Erase)               :- store(b, f, s).
flow(a, p, Enter)                           :- edge(c, h), actual(c, i, a), formal(h, i, p).
flow(v, d, Exit)                            :- edge(c, h), result(c, r, d), ret(h, r, v).
```

Labels carry one bit of provenance, `Own` or `Formal`: whether, within the
function that holds the cell, the label arrived through one of that function's
formals.

```prolog
holds(v, T, Own)                            :- seed(v, T).
holds(d, T, b)                              :- flow(s, d, Keep),  holds(s, T, b),   ok(d, T).
holds(d, T, Own)                            :- flow(s, d, Erase), holds(s, T, _),   ok(d, T).
holds(d, T, Formal)                         :- flow(s, d, Enter), holds(s, T, _),   ok(d, T).
holds(d, T, Own)                            :- flow(s, d, Exit),  holds(s, T, Own), ok(d, T).
holds(Mem(r, f), T, b)                      :- holds(Mem(o, f), T, b), rep(o, r).
```

A store erases provenance because the memory cell is shared by every function
and every caller. A return exports only labels the callee produced itself; a
label that entered through a formal leaves through the pass-through mechanism
below, which routes it caller-locally. This is what keeps `a = id(new A);
b = id(new B)` from giving `a` the label `B`.

`ok(d, T)` is the type filter:

```prolog
ok(d, T)                                    :- !static_type(d, _).
ok(d, Class(C))                             :- static_type(d, S), subtype(C, S).
ok(d, Fun(h))                               :- static_type(d, S), prototype_fits(h, S).
```

This uses the hierarchy to remove labels that a declared type rules out, the
dual of CHA. For bytecode the declared types of fields, parameters, locals and
`checkcast` are known and the filter is strong. For binaries it is a prototype
check where a signature is available, and nothing otherwise; field sensitivity
by offset does most of the filtering there.

### Pass-through summaries

A pass-through summary is label-free: it records that a function's formal `i`
reaches its return slot `r` through copies and through callees' pass-throughs.
At a call site it becomes a direct flow edge from actual to result, inside the
caller, so labels never go into the callee and back out.

```prolog
reach(h, p, p)                              :- formal(h, _, p).
reach(h, p, d)                              :- reach(h, p, s), copy(d, s), owner(d, h).
reach(h, p, d)                              :- reach(h, p, a), owner(c, h), edge(c, k),
                                               actual(c, i, a), through(k, i, r), result(c, r, d).
through(h, i, r)                            :- formal(h, i, p), ret(h, r, v), reach(h, p, v).
flow(a, d, Keep)                            :- edge(c, k), through(k, i, r), actual(c, i, a), result(c, r, d).
```

`reach` never crosses memory, because a load reads a cell whose labels are
`Own`. A recursive identity wrapper derives `through` from its base case; a
wrapper that only forwards and never returns a formal derives nothing. The
relation sizes are `Σ_h arity(h) × |vars(h)|` for `reach` and
`O(P × a × r)` for `through`.

What this does not do: `apply(f, x) { return f(x); }` called once with
`identity` and once with `makeB` gives both callers the label `B`, because the
return effect of `makeB` is `Own` inside `apply`. Separating that needs a
guarded effect ("if formal 0 is `identity`, return formal 1"), which is S14's
guarded summary. It is deliberately not part of the kernel; see
[Guards for hybrid inlining](#guards-for-hybrid-inlining).

### Dispatch

```prolog
target(v, T)                                :- holds(v, T, _).
target(v, T)                                :- holds(Mem(Of(v), #value), T, _).        % S11

edge(c, h)                                  :- site(c, _, Direct(h)).
edge(c, h)                                  :- site(c, _, Indirect(v)), target(v, Fun(h)).
edge(c, m)                                  :- site(c, _, Virtual(v, sig)), target(v, Class(C)),
                                               lookup(C, sig, m).

wanted(C, sig)                              :- site(c, _, Virtual(v, sig)), target(v, Class(C)).
wanted(D, sig)                              :- wanted(C, sig), !declares(C, sig, _), super(C, D).
lookup(C, sig, m)                           :- wanted(C, sig), declares(C, sig, m).
lookup(C, sig, m)                           :- wanted(C, sig), !declares(C, sig, _), super(C, D),
                                               lookup(D, sig, m).
```

`lookup` is exact and demand-driven: it walks up from the evidenced class only,
for signatures actually called on it, and never down. Negation is only on input
facts, so the program remains stratified. Interface defaults, bridge methods and
CIL's explicit interface implementations need language-specific `declares`
facts; where a language's rule is ambiguous or unmodeled the site stays
unresolved and says why.

Only Java and CIL need `lookup`. Everywhere else a method table is memory:

- A C `ops` struct, a C++ vtable, a Lua table and a .NET delegate are objects
  whose cells hold function labels. A call through them is a load followed by
  an indirect call, which the rules above already resolve.
- Statically initialized data (`.data`, `.rodata`, vtable symbols, GOT entries)
  is lowered by the front end as load-time stores into global cells:
  `$tbl.[8] = ptr<f>`. There is no special rule for tables.
- A C++ object's class is its vtable address: `new C` is a store of the vtable
  symbol at offset zero, and a virtual call loads through it. No class
  hierarchy is needed for native virtual dispatch.
- A JVM method handle constant is `ptr<m>`; `invoke` is an indirect call.
  Reflection and strings (S12) stay out, as today.

### Reporting

The final stratum, after the fixpoint, classifies what was not resolved and
what was resolved under strain (N3, no silent loss):

```prolog
unresolved(c, NoEvidence)                   :- site(c, _, Indirect(v)), !target(v, _).
unresolved(c, NoEvidence)                   :- site(c, _, Virtual(v, _)), !target(v, _).
unresolved(c, NoLookup(C, sig))             :- site(c, _, Virtual(v, sig)), target(v, Class(C)),
                                               !lookup(C, sig, _).
unresolved(c, NoBody(h))                    :- edge(c, h), !has_body(h), !has_model(h).
collapsed(o)                                :- offset-inconsistent unification of o
hub(cell, n)                                :- n = #{T : target(cell, T)}, n > N
```

`hub` is a diagnostic, not a cutoff: the edges are still produced. It names the
cells where precision is being lost (typically formals of `Object` methods and
container internals) and is the input to the refinement stratum. `N` is
recorded with the index like any other knob (N2, N5).

## Scaling

The domains and the work are:

| Quantity | Bound |
| --- | --- |
| Cells | variables + field-access sites in the program text |
| `unify`, `rep` | linear in statements plus one per argument of each resolved edge; the lattice converges in at most the number of merges |
| `flow` | statements + Σ over resolved edges of arity, + `D × a × r` pass-through instantiations |
| `holds` | cells × labels reaching × 2; the only product that can be large |
| `reach`, `through` | Σ_h arity(h) × vars(h); `P × a × r` |
| `lookup` | wanted (class, signature) pairs × hierarchy depth |
| `edge` | sites × targets evidenced at the site |

There is no suffix extension, no path concatenation, no alias summary and no
conditional row. The one product that can grow is `holds` at hub cells: a cell
reached by many classes. Three things bound it in practice. Type filtering
removes most labels at typed cells. Pass-through keeps utility functions from
becoming hubs for their callers' results. And labels are classes, not
allocation sites, so the ceiling at a hub is the number of classes with a path
to it, not the number of allocations.

The known failure mode of unification is a chain of merges that fuses unrelated
objects through a shared utility (`memcpy`, a generic container, `Object`
formals). Collapsing them does not make the analysis slower, it makes it less
precise, and `hub` and `collapsed` make it visible. The refinement stratum
exists for exactly these cells.

The second experiment to run alongside is the **directional variant**: replace
`unify` with a directional `points_to(cell, obj)` keyed by allocation site
(Andersen's analysis, as in Doop's context-insensitive baseline). Same
extraction, same labels, same dispatch, more precise memory, cubic worst case.
It is the comparison that says whether unification is buying anything on real
inputs.

## Later strata

None of these is needed for the baseline. Each is a separate stratum that takes
the baseline's output as input, so the baseline's semantics do not change.

### Refinement: split hub objects

For each object whose field cell is a `hub`, rerun discovery with that object's
unification suppressed across call boundaries: pointers flowing into a formal
do not unify their pointee with the formal's pointee if the pointee is a hub
object; instead the callee is analyzed per allocation site of the hub object
(one-object sensitivity, for those objects only). The refined result replaces
the baseline's edges for sites whose receiver cell belongs to a refined object:

```prolog
edge(c, m)   :- base_edge(c, m), !refined_site(c).
edge(c, m)   :- refined_edge(c, m).
```

This is stratified: `hub` is computed from the baseline fixpoint, the refined
run is a fresh least fixpoint parameterized by it, and the combination uses
negation only on finished strata. Refinement removes edges, which is why it
cannot live inside the baseline fixpoint.

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

A function's contribution to the structure is a set of unifications among the
pointees of its formals, return slots and the globals it touches, plus the
labels it seeds into them, plus its `through` facts. That is a Steensgaard
summary: a small shape graph that composes by unification in near-linear time.
A separately indexed library (`--summary`, F7) can ship its shape graph and be
linked without re-analysis. The directional variant does not have this
property, which is a reason to prefer unification if the measurements allow.

### Native evidence

Binaries have fewer seeds than bytecode and the front end must produce them:
address-taken functions (relocations and immediates equal to a function entry),
initialized pointer tables, PLT and GOT entries as cells holding labels for
functions without bodies, and vtable symbols as class labels for C++. All of
these are stores into global cells, so the kernel does not change. Type
reconstruction falls out of the unified structure as a side effect: the shape
graph is a struct layout, and it can be reported.

## Relation to the earlier draft

The earlier draft of this document proposed a pathless kernel with symbolic
return origins (`Ret(p, r, Arg(i))`) and dispatch requirements (`Need`) carried
through wrappers. This proposal keeps the first as `through`, phrased so that it
is one flow mode among others. It drops `Need`: a call edge belongs to a
syntactic site, and the edges a site gets are the union over its contexts
whether the context is tracked or not, so routing requirements through wrappers
changes nothing about the edge set. It only mattered for guarded return effects,
which are now the optional `decision` export.

The main difference is memory. The earlier draft excluded every non-empty path
and offered an immutable-field extension that the IR cannot certify. This
proposal includes memory from the start, with an abstraction coarse enough to be
linear and sound with respect to the IR's own rules, and buys precision back
afterwards where it is measured to matter. Factories that return `this.target`,
`ops` tables, vtables and Lua method tables are in the baseline rather than in
an extension.

## Evaluation

The question is whether the kernel discovers useful edges with intermediate
relations that are small and grow predictably.

1. **Implement the kernel in Ascent** with lattice representatives, as its own
   engine over the post-SSA IR, with `edge`, `unresolved`, `hub` and
   `collapsed` as outputs. No taint.
2. **Implement the directional variant** by swapping the structure rules, and
   run both on every input.
3. **Fixtures**, each asserting discovered edges and intended unresolved sites:
   two identity callers with different classes; factory chains; forwarding
   wrappers; recursive wrappers with and without a base case; a
   branch-dependent factory; `apply(f, x)` with two callers; `new Base` with an
   overriding subclass (the override must not appear); a getter returning a
   constructor-stored field, with two instances holding different targets (the
   merge is expected and must be reported as a hub when `N` is small); a C
   `ops` table; a C++ vtable; a Lua method table; a JVM method-handle constant;
   a `value(p)` operand; pointer arithmetic into a table; a `checkcast`-filtered
   hub.
4. **Order invariance:** shuffle input facts and run serial and parallel; the
   outputs must be identical.
5. **Measure** on the regression inputs and on real imports: wall time, peak
   RSS, and the sizes of `cells`, `unify`, `flow`, `holds`, `through`, `edge`;
   join candidates versus inserted facts; number and size of hubs; count of
   collapsed objects. Synthetic sweeps of wrapper depth, hub fan-in, access-path
   depth and class count expose the products in the table above.
6. **Compare edge sets** with the current engine using stable site and
   procedure identities:

   ```text
   old_only = old_edges − new_edges
   new_only = new_edges − old_edges
   ```

   Classify `old_only` by the mechanism that produced the edge (CHA, guarded
   summary, path-sensitive memory). CHA-only edges are intended losses. Then
   run taint on the new call graph and compare findings, as a second signal.
7. **Stratified taint:** run the existing taint engine with target-label
   propagation disabled and the kernel's edges supplied as facts. This measures
   the structural win on the taint side, which is where the recorded cost was.

The experiment succeeds if the kernel finds the edges the fixtures expect, the
real-import comparison loses only CHA and guard-dependent edges, `holds` and
`flow` stay within a small factor of program size, and the stratified taint run
is materially cheaper than the coupled one. Adoption does not depend on
recovering the current engine's edge set.

## Policy after a baseline exists

The initial reset may discard any existing mechanism and its discoveries. After
that, the default is additive: new rules add edges, and a change meant to
preserve results satisfies `Edges_old(I) ⊆ Edges_new(I)` for supported input `I`
under fixed modeling assumptions, checked by regression. Positivity guarantees
this only within one rule system; a change of abstraction, extraction or
evidence must explain how old derivations translate, and an intentional loss
(corrected modeling, a precision or scalability tradeoff) is recorded with its
measured impact. The refinement stratum is such an intentional loss and is
reported as one. Better total coverage or unchanged taint findings do not
establish inclusion.
