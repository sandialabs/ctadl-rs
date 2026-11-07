# CTADL IR

The CTADL IR is what every front end lowers to and what the index analyzes. This document is its
definition: the syntax a program is written in, and the semantics that says what a program
means. The semantics is the reference every consumer of the IR is measured against; where the
engines deviate from it is listed in the
[approximations register](limitations.md#approximations-register). The rules are numbered S1–S19
so that code and tests can cite them.

The IR is a translation target for real languages, which do things a static analysis cannot
reason about precisely and scalably, and the source of a flow-insensitive, unsound taint
analysis. It is a data-flow language whose semantics is *labeling*: from an initial labeling,
the rules say how labels propagate across the IR's constructs. Front ends decide, by convention,
whatever a source language does that the IR cannot say (see [Front ends](#front-ends)).

Three implementations follow this document:

- `ctadl_ir::semantics`, the reference implementation: a direct evaluation of the rules below,
  used as an oracle on small programs.
- The index (`ctadl-index`) and the query (`ctadl-ascent/src/query_engine`), the engines, which
  compute the same labeling compositionally and at scale, deviating only as the register says.
- `ctadl_ir::cir::path`, the one implementation of the path equations.

## Syntax

### Paths

**S1.** An *access path* `x.s` is a variable `x` followed by a *path* `s`, a sequence of segments:

| segment | spelling | meaning |
| --- | --- | --- |
| field | `.f` | a symbolic field; no layout |
| offset | `.[k]` | address arithmetic by the constant `k` |
| deref | `.deref` | the step from an address to the memory there |
| value | `.#value` | what a variable's value was computed from |

`deref` and `#value` are segment kinds, not field names. A field literally named `deref` or
`#value` is written with its first character escaped, `.\deref`, `.\#value`; a field whose name
contains `.`, or begins with `[`, escapes those characters (`.a\.b`, `.\[3]`). Offsets are
decimal. `ctadl_ir::cir::path_syntax` is the grammar's one printer and parser, used by IR dumps,
the fact store, model ports and flowy alike.

Offsets are always constants; there is no `[*]` and no variable offset. Fields are symbolic
names with no layout: two fields are related only by being equal.

### Normal form

**S2.** Paths are compared and classified only in normal form, given by these equations:

- Offsets add, and zero does nothing: `p.[i].[j] = p.[i+j]`, `p.[0] = p`.
- Fields and `deref` obey no equations: `p.f ≠ p`, `p.f.g ≠ p.g.f`.
- `#value` absorbs itself and adjacent offsets, on both sides: `p.#value.#value = p.#value`,
  `p.[k].#value = p.#value`, `p.#value.[k] = p.#value`. A value plus a constant is still
  computed from the same value.

Every path stored in the IR, in facts, in a summary or in a model port is in normal form.
`AccessPath::new` normalizes; the verifier rejects a path that is not normal.

### Classification

**S3.** A normal-form path is exactly one of, checked in this order:

- **void**: a field or `deref` follows `#value` (`p.#value.f`). It denotes nothing.
- **address**: it ends in an offset. It is not a cell.
- **location**: everything else (empty, or ending in a field, `deref` or `#value`). A location
  is a cell.

### Expressions

An operand is one of:

| expression | spelling | |
| --- | --- | --- |
| access path | `x.s` | read as a whole; an empty path names the variable |
| value | `value(x.s)` | valid only as an assignment's source |
| function reference | `ptr<f>` | a target label |
| class reference | `new C` (front end specific) | a class label |
| string constant | `"s"` | a string label |
| integer, bytes | `3` | carry nothing |

### Statements

| statement | form | |
| --- | --- | --- |
| assignment | `y.d = e_1, ..., e_n` | one destination; several sources are several assignments |
| call | `r_1, ..., r_m = f(a_1, ..., a_n)` | direct by name, indirect through an access path, or virtual by receiver and method |
| phi | `y = phi(x_1, ..., x_n)` | inserted by SSA conversion |
| nop | | |

An assignment is a **store** when its destination path `d` is non-empty and a **copy** when it
is empty. A load `y = x.f` is a copy, and so is pointer arithmetic `y = x.[8]`. The IR has no
other form of read or write. `StatementKind::is_store` is the one place that classifies.

A call's results come back in order, and an exception, where a language has them, is always
the last result of every call and the last value of every return.

### Well-formedness

`MirVerify` is the executable definition of a well-formed program. It rejects, among the
structural errors: a path not in normal form; `value(..)` anywhere but as an assignment's
source; a void path as an operand; an address as an assignment's destination (see below); a
copy into the whole of `$globals`; a field named `#value`; and, in SSA form, a store whose base
is a version nothing defines (a store treated as a definition).

### Textual form

An IR dump prints a copy as `assign y = e, ...` and a store as `store y.d := e, ...`, an
expression as its access path, `value(x.s)`, or `<const: ...>`, and a variable as `%name`
(`%L7` by local index in the fact base), `@p0` for a parameter, and `$globals`, with `_k`
appended for SSA version `k`. [flowy](../frontends/ctadl-flowy) is the IR's own test language:
`.deref` and `.#value` are the segments, `.*` is an error, and an address destination
`p.[k] = e` is accepted and lowered by the rewrite below.

## Semantics

A program is a set of assignments and calls in SSA form, with an initial labeling from its
sources. Its meaning is the least labeling closed under the rules below.

### Labels

**S4.** Each location holds a set of labels. A label is a taint label from a source, a target label
(a function or class reference), or a string label. Each label at a location has a **scope**, `All`
or `Fixed`, saying whether it may move backward along assignments; a location holds a set of (label,
scope) pairs, and `All` subsumes `Fixed`.

An address holds no labels of its own: `labels(p.[k]) = labels(p)`, so if `q` is
attacker-controlled and `p = q.[8]`, then `p` is too. A void path holds nothing, and an
assignment to or from one propagates nothing.

### Assignment

**S5.** `y.d = x.s` is a store or a copy (see [Statements](#statements)), classified after
normalizing. An address destination is rewritten first: `p.[k] = e` is
`p = e.[-k]` (so `p.[8] = e.[8]` is `p = e`), normalized again; a value or constant source is
unchanged, since `p.[k].#value = p.#value` and an address's labels are its base's.

- **S6. Forward.** For every suffix `t`, including the empty one, with `x.s.t` and `y.d.t` both
  locations, `y.d.t` gets `labels(x.s.t)`. With `t` empty and `x.s` an address, that is the
  labels of its base, so assigning from an address is defined.
- **S7. Scope.** A label placed by a store, at `y.d` or at any suffix of it, or introduced by a
  source, has scope `All`. A label arriving by a copy has scope `Fixed`.
- **S8. Backward.** For every non-empty suffix `t` other than `#value` itself, with both sides
  locations, labels at `y.d.t` of scope `All` also reach `x.s.t`, keeping scope `All`.

The backward rule makes a write through a copy of a pointer reach the original
(`l = y; l.f = x` reaches `y.f.g` from `x.g`), while a label copied forward (`Fixed`) never moves
back, so a phi or a multi-source assignment does not merge its sources.

**S9.** Two stores to one destination do merge: `y.f = x; y.f = w` sends `x.g` to `w.g`, because the
first store's labels at `y.f.g` must be `All` for `l = y; l.f = x` to reach `y.f.g`. That
imprecision is accepted.

The empty suffix and the suffix `#value` move forward only: a variable's own value is not shared
with its aliases, but a value held in memory (`p.f.#value`, `p.deref.#value`) is, like the cell
that holds it.

`y = value(p)` is the store `y.#value = p`. It gives `y.#value` the labels of `p` and `p.#value`
only, since every other `y.#value.s` is void, so nothing reachable through `p` becomes reachable
through `y`.

### Calls

**S10.** A call `r = f(a_1, ..., a_n)` is the body of `f` with fresh variables, preceded by the
copies `p_i = a_i` into its formals and followed by the copy `r = ret` from its return. Their labels
have scope `Fixed`, like any copy's. A program is its infinite unfolding; only `$globals` is shared
between instances, so `r1 = id(a); r2 = id(b)` gives `r2` only `b`'s labels. A call to a function
without a body is zero assignments unless a model supplies them, and the analysis reports it, never
hides it.

**S11.** A function reference is a label: `x = ptr<f>` puts the target label `f` at `x`, and
`x = new C` puts the class label `C`. An indirect call `r = (*e)(a_1, ..., a_n)` is the call of
`f` for every target label `f` at `e` and `e.#value`; a virtual call is the same with the receiver's
class labels resolved through the class hierarchy. So labels create assignments, and the call graph
is part of the same fixpoint, resolved per call-site instance. A call with no target label is zero
assignments, reported like a call without a body.

Target labels are ordinary labels: they follow the path rules and scopes, and cross functions
like any label, so a function pointer stored in a struct in one function resolves a call in
another.

**S12.** A constant string is a label of the same standing, consumed only by a bridge that
resolves its target from it (an Android intent's component or action). Other constants carry
nothing.

### Globals

**S13.** There is one variable `$globals`, shared by every function; a global `g` is the location
`$globals.g`. An implementation may pass it to every function like a formal: a copy in, and
stores inside the callee reach the caller's `$globals` backward.

### Summaries

**S14.** An implementation may analyze each function once into a summary: the flows of its body
between paths rooted at its formals, return and `$globals`. A flow that exists only because a target
label reaches a formal path is *guarded* by that requirement; instantiating the summary at a call
site substitutes actuals for formals and fires a guarded flow only when the actuals carry its
targets. Summaries and the call graph are then one fixpoint. This computes the unfolding exactly for
flows between formal paths; cells inside a callee see every caller's labels merged, where the
unfolding keeps them apart, which is accepted. Resolving a call by other means (a dispatch model,
class hierarchy analysis for a small candidate set) only adds targets.

### Models

**S15.** Models desugar to the constructs above: a propagation model is a store or copy between
ports, a bridge is a call, a dispatch model adds targets, and a source or sink is an initial label
or an observation. There is no third construct.

### Ports and the observable

**S17.** A source or sink on a path `p` labels or observes exactly `p` and `p.#value`, nothing under
them: a label at `p` says nothing about `p.f`. A model that means depth names more ports. A
wildcard, where a model language offers one, is a convenience expanded to a finite set of paths
from the program text, never a path in the IR.

The observable is the labeling at ports: which sources' labels reach which sinks, per program
point, as the union over instances of the unfolding. A reported path is a witness from the
derivation, not part of the semantics.

### SSA

**S16.** SSA is part of the definition, not a pass: it is what makes a variable one cell. A variable
is versioned at an assignment whose destination path is empty, and never at a store. So `x1 = src;
x2 = clean; sink(x2)` reports nothing, while memory stays flow-insensitive: `sink(p.f); p.f = src`
is reported. A phi is the set of its copies.

### Passes

**S18.** A pass is correct if the SSA form of its output has the same observable labeling as the SSA
form of its input. A pass that creates or merges cells (`mem2reg`, coalescing) is correct only when
no other path reaches the memory it promotes or merges. Each pass's module documentation states its
argument, and `frontends/ctadl-flowy/tests/passes.rs` checks it against the reference
implementation.

## Front ends

**S19.** The semantics is exact over the IR. Whatever a source language does that the IR cannot say
is decided at lowering, by convention, and each front end documents its conventions in its crate
documentation:

- **No address-of.** What has its address taken lives behind a `deref` (`frame.[k].deref`), and
  `&x` is the address `frame.[k]`. `deref`, and only `deref`, is the step from an address to the
  memory there: C's `p->f` is `p.deref.f`.
- **Non-constant offsets.** `p + i` and `a[i]` with `i` unknown are a copy of `p` plus the value
  of `i`: `q = p, value(i)`.
- **Arithmetic** and comparisons supply values only: every operand is `value(v)`, except the base
  of an address computation, which is a copy.
- **Widths.** Offsets have no width; `p.[0].deref` and `p.[4].deref` are distinct locations, and
  overlap is lost.

## Testing the definition

- `ctadl-ir/src/cir/path.rs`: property tests of the equations and the classification.
- `ctadl-ir/src/cir/tests.rs`: one test per verifier check.
- `frontends/ctadl-flowy/tests/semantics/*.flowy`: one program per rule, run on the reference
  implementation, each pinning its observable in an `// expect:` block.
- `ctadl-ascent/tests/integration/reference_parity.rs`: the engines against the reference on the
  corpus and every `.tnt` program; every difference is a row of the
  [approximations register](limitations.md#approximations-register).
