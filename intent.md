# Redesign IR with solid semantics and simplicity

- The IR is a translation target for *real languages* (which do arbitrary things that static
  analysis can't precisely, scalably reason about)
- The IR is the analysis source of a flow-insensitive, *unsound* static taint analysis. It needs to
  have solid semantics so that passes on the IR can obey some sort of correctness/abstraction
  criterion. Also, so that our analysis can approximate appropriately
- It is a data flow language whose semantics is labeling. Beginning with an initial labeling, the
  semantics says how labels propagate across various IR constructs.
- Offsets are inspired by offsets into data structures (in binaries), like how C lays out a struct.
  Offsets are *always* constants. There is no [*], no arbitrary offset or field.
- Fields are symbolic field names and do not have any layout characteristics.

## Paths

An access path `x.s` is a variable `x` followed by a path `s`: a sequence of segments, each a
field `f`, an offset `[k]`, `deref`, or `#value`. Paths are compared and classified only in
normal form, given by these equations:

- Offsets add, and zero does nothing: `p.[i].[j] = p.[i+j]`, `p.[0] = p`.
- Fields and deref obey no equations: `p.f ≠ p`, `p.f.g ≠ p.g.f`. deref is its own segment,
  not a field name, so a source field named `deref` cannot be mistaken for it. Frontends use
  it, and only it, for the step from an address to the memory there.
- #value absorbs itself and adjacent offsets, on both sides: `p.#value.#value = p.#value`,
  `p.[k].#value = p.#value`, `p.#value.[k] = p.#value`. A value plus a constant is still
  computed from the same value.

Every path in normal form is exactly one of the following, checked in this order:

- *void*: a field or deref follows #value (`p.#value.f`). It denotes nothing, holds no
  labels, and an assignment to or from it propagates nothing. This keeps #value terminal while
  every path stays meaningful.
- *address*: ends in an offset. Not a cell; it holds no labels. Its labels are those of the
  location before the trailing offset: `labels(p.[k]) = labels(p)`, so if `q` is
  attacker-controlled and `p = q.[8]`, then `p` is too.
- *location*: every other path (empty, or ending in a field, deref, or #value). A location is a
  cell carrying a set of labels.

Each label at a location has a *scope*, `All` or `Fixed`, saying whether it may move backward
along assignments (below). A location holds a set of (label, scope) pairs; `All` subsumes `Fixed`.

A source or sink on a path `p` labels or observes both `p` and `p.#value`.

## Assignment

`y.d = x.s`, classified after normalizing:

- An address destination is rewritten first: `p.[k] = e` is `p = e.[-k]`, normalized again.
- *store*: `d` is non-empty.
- *copy*: `d` is empty (`y = x`, `y = x.f`, `y = x.[8]`). A load is a copy, and pointer
  arithmetic shares the object, like a whole-variable copy.

Propagation:

- Forward: for every suffix `t`, including the empty one, with `x.s.t` and `y.d.t` both
  locations, `y.d.t` gets `labels(x.s.t)`. With `t` empty and `x.s` an address, that is the
  labels of its base, so assigning from an address is defined.
- Scope: a label placed by a store, at `y.d` or at any suffix of it, or introduced by a
  source, has scope `All`. A label arriving by a copy has scope `Fixed`.
- Backward: for every non-empty suffix `t` not ending in #value, with both locations, labels at
  `y.d.t` of scope `All` also reach `x.s.t`, keeping scope `All`. This makes a write through
  a copy of a pointer reach the original, while a label copied forward (`Fixed`) never moves
  back, so a phi or multi-source assignment does not merge its sources. Two stores to one destination do merge: `y.f = x; y.f = w` sends `x.g` to
  `w.g`, because the first store's labels at `y.f.g` must be `All` for `l = y; l.f = x` to
  reach `y.f.g`. That imprecision is accepted.
- The empty suffix and suffixes ending in #value move forward only.

Consequence: `t.#value = v` gives `t.#value` the labels of `v` and `v.#value` only, since every
other `t.#value.s` is void. So `value(v)` needs no rule of its own: nothing reachable through
`v` becomes reachable through `t`.

## Calls

A call `r = f(a_1, …, a_n)` is the body of `f` with fresh variables, preceded by the copies
`p_i = a_i` into its formals and followed by the copy `r = ret` from its return. Their labels
have scope `Fixed`, like any copy's. A program is its infinite unfolding; only `globals` is
shared between instances, so `r1 = id(a); r2 = id(b)` gives `r2` only `b`. A call to a
function without a body is zero assignments unless a model supplies them, and the analysis
must report that, never hide it.

A function reference is a label: `x = &f` puts the target label `f` at `x`, and `x = new C`
puts the class label `C`. An indirect call `r = (*e)(a_1, …, a_n)` is the call of `f` for
every target label `f` at `e` and `e.#value`; a virtual call is the same with the receiver's
class labels resolved through the class hierarchy. So labels create assignments, and the call graph is part of the same fixpoint,
resolved per call-site instance. A call with no target label is zero assignments, reported
like a call without a body.

Target labels are ordinary labels: they follow the path rules and scopes, and cross
functions like any label, so a function pointer stored in a struct in one function resolves
a call in another.

## Globals

The semantics has one variable `globals`, shared by every function; a global `g` is the
location `globals.g`. The implementation passes it to every function like a formal, which
agrees with the semantics: a copy in, and stores inside the callee reach the caller's
`globals` backward.

## Summaries

The implementation analyzes each function once into a summary: the flows of its body between
paths rooted at its formals, return and `globals`. A flow that exists only because a target
label reaches a formal path (`p ∋ g`, `p.vtable.[8].deref ∋ g`, `globals.handler ∋ g`) is
*guarded* by that requirement; a flow through a direct call is unguarded. Instantiating a
summary at a call site substitutes actuals for formals and fires a guarded flow only when the
actuals carry its targets. Summaries and the call graph are therefore one fixpoint: a target
reaches a formal only through other summaries. "Once" means no per-context copies, not an
ordering of passes. This computes the unfolding exactly for flows between formal paths; cells
inside a callee see every caller's labels merged, where the unfolding keeps them apart. That
over-approximation is accepted.

Resolving a call by other means (a dispatch model, class hierarchy analysis for a small
candidate set) only adds targets.

## Models

Models desugar to the constructs above: a propagation model is a store or copy between
ports, a bridge is a call, a dispatch model adds targets, and a source or sink is an initial
label or an observation. There is no third construct.

## Program

A program is a set of assignments and calls in SSA form, with an initial labeling from
sources. Its semantics is the least labeling closed under the rules above. SSA is part of the
definition, not a pass: it is what makes a variable one cell, so `x1 = src; x2 = clean;
sink(x2)` reports nothing, while memory stays flow-insensitive (`sink(p.f); p.f = src` is
reported). A phi is the set of its copies.

The observable is the labeling at ports: which sources' labels reach which sinks. A reported
path is a witness from the derivation, not part of the semantics.

A pass is correct if the SSA form of its output has the same observable labeling as the SSA
form of its input. A pass that creates or merges cells (`mem2reg`, coalescing) is correct
only when no other path reaches the memory it promotes or merges.

## Front ends

The semantics is exact over the IR. Whatever a source language does that the IR cannot say is
decided at lowering, by convention:

- No address-of. What has its address taken lives behind a `deref` (`frame.[k].deref`), and
  `&x` is the address `frame.[k]`.
- A non-constant offset (`p + i`, `a[i]`) is a copy of `p`.
- Offsets have no width; `p.[0].deref` and `p.[4].deref` are distinct locations, and overlap
  is lost.
