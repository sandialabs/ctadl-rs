# CTADL IR: Requirements and Design

How the IR semantics in `intent.md` becomes part of CTADL. The first part states what must be
true when the work is done; the second says how the code should be shaped to make it true. It
does not order the work; that is planning.

This document refines [docs/spec.md](docs/spec.md) §2 (access paths), §4.1 (the import and
index stages) and §4.2 (local flow, field propagation, finite access paths, summaries,
aliasing). Where the two disagree, this one is current and docs/spec.md is to be updated
(R45). The principles of docs/spec.md §1 stand: compositional throughout, simple explainable
rules, deliberately unsound, one language-independent IR, rules written as Datalog.

Line references are to the tree at the time of writing and will drift.

## 1. The semantics

`intent.md` is normative. This section restates it as numbered rules so requirements can cite
them.

**Paths.**

- S1. An access path is a variable followed by segments: field `f`, offset `[k]` (a constant
  integer), `deref`, or `#value`. `deref` and `#value` are segment kinds, not field names.
- S2. Normal form: `p.[i].[j] = p.[i+j]`, `p.[0] = p`; `p.#value.#value = p.#value`,
  `p.[k].#value = p.#value`, `p.#value.[k] = p.#value`; fields and `deref` obey no equations.
  Paths are compared only in normal form.
- S3. A normal-form path is *void* (a field or `deref` after `#value`), else an *address*
  (ends in an offset), else a *location*. Only locations hold labels. `labels(p.[k]) =
  labels(p)`; void holds nothing and propagates nothing.
- S4. A location holds a set of (label, scope) pairs, scope ∈ {`Fixed`, `All`}, `All ⊒ Fixed`.

**Assignment** `y.d = x.s`, after normalizing and rewriting an address destination
(`p.[k] = e` is `p = e.[-k]`):

- S5. *Store* if `d` is non-empty; otherwise *copy* (a load is a copy; pointer arithmetic is a
  copy).
- S6. Forward: for every suffix `t` (including empty) with `x.s.t` not void and `y.d.t` a
  location, `y.d.t` gets `labels(x.s.t)`.
- S7. Scope: a label placed by a store (at `y.d` or any suffix of it) or by a source has scope
  `All`; a label arriving by a copy has scope `Fixed`.
- S8. Backward: for every non-empty suffix `t`, `t ≠ #value`, with both sides locations, labels
  at `y.d.t` of scope `All` also reach `x.s.t`, keeping `All`. The empty suffix and the suffix
  `#value` move forward only.
- S9. Accepted imprecision: two stores to one destination merge (`y.f = x; y.f = w` sends
  `x.g` to `w.g`).

**Calls.**

- S10. `r = f(a_1..a_n)` is the body of `f` with fresh variables, preceded by the copies
  `p_i = a_i` and followed by `r = ret`. The program is its infinite unfolding; only `globals`
  is shared between instances. A call to a function without a body, or with no target label,
  is zero assignments and must be reported.
- S11. A function reference `x = &f` places target label `f` at `x`; `x = new C` places class
  label `C`. An indirect call is the call of every target labelled at `e ∪ e.#value`; a
  virtual call resolves the receiver's class labels through the class hierarchy. Labels create
  assignments; the call graph is in the same fixpoint, per call-site instance.
- S12. A constant string is a label of a third kind. It propagates like any label and is
  consumed only by bridges that resolve a target from it (Android intents). Other constants
  (`Int`, `Bytes`) carry nothing.

**Globals, summaries, models, program.**

- S13. One shared variable `globals`; a global `g` is `globals.g`. Passing it as a formal
  agrees with S10.
- S14. A summary is the body's flows between paths rooted at formals, return and `globals`,
  each guarded by the target labels it depends on at formal paths. Instantiation substitutes
  actuals and fires a guard only when the actuals carry its targets. Summaries and the call
  graph are one fixpoint. Cells inside a callee see every caller merged (accepted).
- S15. Models desugar: propagation is a store or copy between ports; a bridge is a call; a
  dispatch model adds targets; a source or sink is an initial label or an observation.
- S16. A program is a set of assignments and calls in SSA form plus an initial
  labeling; its semantics is the least labeling closed under S1–S15. SSA is definitional. A
  phi is the set of its copies.
- S17. A source or sink on `p` labels or observes exactly `p` and `p.#value`. The observable
  is which sources reach which sinks, per program point, over all instances. A reported path
  is a witness, not semantics.
- S18. A pass is correct if the SSA form of its output has the same observable labeling as
  the SSA form of its input. A pass that creates or merges cells is correct only when no other
  path reaches the memory it promotes or merges.
- S19. Front-end conventions: no address-of (address-taken storage lives behind `deref`); a
  non-constant offset is a copy of the base; offsets have no width.

## 2. Requirements

### 2.1 The IR

- R1. The IR's path type has the four segment kinds of S1 as distinct variants. `deref` and
  `#value` are no longer reserved field names (`ctadl-ir/src/mir/mod.rs:171,180`), so a
  source field named `deref` cannot collide with them.
- R2. The IR owns normalization (S2) and classification (S3). Every path stored in the IR, in
  facts, in a summary or in a model port is in normal form, and there is exactly one
  implementation of the equations, in `ctadl-ir`. Today the equations are split between
  `load_access_path` (`mir/mod.rs:1665`), `facts::Path::from_accesses`
  (`ctadl-index/src/facts.rs:262`), `prepend_onto` (`facts.rs:1673`) and `PathSet::concat`
  (`index_engine/path_set.rs:271`), and none of them absorbs an offset before `#value`.
- R3. An assignment's destination and source are full access paths. The one-field-per-
  statement forms (`Load`, `Store` over an offset-only `Address` plus one `FieldRef`) go; the
  load-chain recomposition in codegen (`cap_path`, `ctadl-ascent/src/codegen/mod.rs:874`)
  goes with them. Lowering `x.f.[8] = v` must keep the path `x.f.[8]`; the present
  `store_access_path` moves the field behind the offsets (`mir/mod.rs:1707`), which S2 forbids.
- R4. The statement set is: assignment (one destination, one or more sources; several sources
  are sugar for several assignments), call, phi, nop, plus the SSA-internal
  parameter write-back. Expressions are an access path, `value(p)` with `p` an access path,
  a function or class reference (target label), a constant string (string label), and inert
  constants.
- R5. A call names its target as today (`CallStyle`): direct by name, indirect by an access
  path, virtual by receiver and method key. A call has zero or more results; an exception is
  always the last result (R21).
- R6. `verify` (`mir/verify.rs`) rejects: a path not in normal form; `value(p)` anywhere but
  as an assignment source; a void path as a destination or source; a store (non-empty
  destination) that defines its base in SSA; a write to `globals` with an empty path; a
  `deref` or `#value` spelled as a field. The verifier is the executable definition of
  well-formed IR.
- R7. The textual form of the IR (dumps, flowy, Portcullis ports) prints and parses the four
  segment kinds unambiguously, and parsers normalize.
- R8. The import format version is bumped; an older import is rejected with a message to
  re-import (docs/spec.md F3). Nothing is migrated in place.

### 2.2 Semantics and correctness

- R9. The semantics of §1 is the single reference. Every consumer of the IR (passes, codegen,
  index rules, query engine, model expansion) is specified by its relation to it: equal, or an
  approximation named in the register (§4) with its direction.
- R10. An executable reference implementation of §1 exists in the tree, for programs small
  enough to run it, with a path-length bound and an unfolding-depth bound as explicit inputs.
  It is the oracle for R11, R12 and R36.
- R11. Every IR pass states its correctness argument under S18 in its module documentation,
  and the passes that create or merge cells (`mem2reg`, `coalesce`) state and check their side
  condition. A pass with a configuration that is not correct under S18 (today: `--mem2reg
  full`) is an approximation, is listed in §4, and when it may lose a flow
  it reports that function (as `--mem2reg full` already does).
- R12. Passes are tested differentially: on a corpus of flowy programs the oracle's observable
  labeling before and after each pass is equal, except for listed approximations.
- R13. The observable (S17) is what the query reports, and nothing in the query asserts a flow
  the semantics does not derive, except through a listed approximation.

### 2.3 Front ends

- R14. A front end's output is in normal form and passes `verify` (R6). The front-end
  conventions of S19 are documented per front end in its crate docs.
- R15. `deref`, and only `deref`, is the step from an address to the memory there. In C,
  `p->f` and `(*p).f` are `p.deref.f`, not `p.f` (`frontends/ctadl-c/src/lib.rs:4708`), so
  `*p` and `p->f` share a prefix. Address-taken scalars keep the C front end's cell convention
  (`x.deref`), which is S19.
- R16. A non-constant offset is a copy of the base plus the value of the index: `q = p + i`
  lowers to `q = p; q.#value = value(i)`, in C (`lib.rs:21-25`) and pcode (`PTRADD`,
  `PTRSUB` with a non-constant operand, `frontends/ctadl-pcode/src/lib.rs:1232,1149`). `a[i]`
  with unknown `i` is `a.deref` (C already) or `a.[0].deref`, the same path.
- R17. Integer arithmetic and comparisons supply values only: every operand is `value(v)`,
  except the base of an address computation, which is a copy. This is pcode's default
  (`CTADL_EXP_INT_ARITH=value`) and becomes the rule for C, JVM, Dex, CIL and Lua, which
  today merge operands (`ctadl-c/src/lib.rs:3911`, `ctadl-jvm/src/lib.rs:984`,
  `ctadl-dex/src/lib.rs:1064`, `ctadl-cil/src/effects.rs:91`, `ctadl-lua/src/lib.rs:1862`).
  pcode's `merge` mode is kept only as a comparison knob.
- R18. Function references and allocations are target labels (S11): `ptr<F>`, `java<C>`,
  `cil<C>`, `lua<C>` as today. Every front end that has them emits them; a method handle or
  delegate that today becomes an opaque object (JVM `MethodHandle`, `ctadl-jvm/src/lib.rs:1006`)
  is a function reference when the method is known.
- R19. Structs by name and structs by offset coexist (C and Java use fields; pcode uses
  offsets); a field is symbolic and has no layout (S1). A bridge between a by-name and a
  by-offset import names the correspondence explicitly in its model; the IR never infers it.
- R20. A frame slot write in pcode is a store, `__stack_top.[k].deref = v`. The frame is
  memory (S16), so `mem2reg` is the only source of flow-sensitivity for it; the `Update`
  statement, `update_sans_path`, the `ssa/frame` coarsening pass and `--frame` go. An
  update cannot be re-expressed as assignments (`x2 = x1; x2.p = v` is a weak update plus an
  alias), which is why it has no place in the semantics. Dropping a write to a frame
  address (`ctadl-pcode/src/lib.rs:201`) stays, as a documented convention.
- R21. An exception is the last result of every call and the last value of every return. How
  far a front end propagates a callee's escaping exception is its own convention, documented
  in its crate docs (ctadl-cil accumulates them in `excout`; JVM and Dex drop uncaught callee
  exceptions, a limitation recorded in docs/limitations.md and outside this work).
- R22. A direct call to a name with no body stays a call (S10) and is never silently turned
  into nothing; the index reports it (R40).

### 2.4 Passes

- R23. SSA construction versions a variable at an assignment whose destination path
  is empty, and never at a store. It is part of the semantics (S16), not a pass to be checked.
- R24. `copy_prop`, `dead_temps` and load-chain composition are correct under S18 with no
  side condition; `coalesce` and `mem2reg` are correct under their side conditions (S18,
  second sentence). `mem2reg`'s escape policy (`window:N`, `exact`, `above`) is the check of
  that side condition; a policy that can be wrong (`window`) is listed in §4.
- R25. No pass depends on `#value` or `deref` being spelled as a field name, and no pass
  treats `value(p)` as anything but a copy of `p` into `dest.#value`.

### 2.5 Index

- R26. The index rules are the rules of §1 with labels = *paths rooted at formals* (including
  return and `globals`) and *target labels*, plus guards. Forward (S6), scope (S7) and
  backward (S8) are each one rule shape; there is no separate alias analysis
  (`compute_alias_of_formal`, `ctadl-index/src/index_engine/mod.rs:1327`), no `FormalType`
  (`Copied`/`SharedFields`/`Shared`) and no `isout` (`facts.rs:1640`): whether a formal path
  flows out is decided by scope. The bidirectional actual↔call-arg edges (`mod.rs:1686`) go;
  a call site is the copies of S10 and the backward rule.
- R27. A summary row is `out ← in` as today (`summary.parquet` keeps its columns), derived
  as: `out` is a return path with any scope, or a formal or `globals` path that is non-empty,
  not `#value`, and holds the `in` label with scope `All`. Guards stay as `Cond` over
  `Decision {formal, path, target}` (`decision.rs:34`).
- R28. Target labels and string labels run through the same rules as formal-path labels. They
  may be stored in their own relations for cost (as `call_target_assign_like` and
  `const_reaches` are), but the rules are the same text, shared by macro or source
  inclusion, so the three label kinds cannot drift.
- R29. Path finiteness stays the admissible set of program paths ∪ model paths ∪ one-level
  concatenations (`compute_paths`, `mod.rs:1387`), applied through `PathSet` membership. It
  is an approximation that loses flows and is listed in §4; the admissible set is closed
  under S2 (so `p.[k].#value` and `p.#value` are one member) and contains `p.#value` for
  every member `p`.
- R30. The resolution ladder (super, dispatch model, CHA under the threshold, hybrid
  inlining) only adds targets, and its configuration is recorded with the index as today
  (`index_config.json`). The engine `IndexConfig` fields that change results (`alias_rule`
  is gone; `hybrid_context`, `deref_paths`) are recorded too (docs/spec.md N2).
- R31. The three engines (serial, par, lsm) share one rule text and differ only in execution
  and storage, as today.
- R32. The index format version is bumped; an index built by an older build is rejected.

### 2.6 Query

- R33. The query propagates source labels with the rules of §1 over the summarized program:
  the index's `assign_like` edges (program assignments, per-site summary edges, call-site
  copies) within a function, and the call and return copies of S10 between a caller and a
  callee body. Scope is the scope of S7; the query has no scope rule of its own. The present
  search engine's deviations (the object hop at the empty path, `query_engine/search.rs:477`;
  its own `ObjectScope`) are removed or shown equal to S7/S8.
- R34. Sources and sinks are exact (S17): a port `p` labels or observes `p` and `p.#value`.
  Sink `wildcard` and source `saturating` are model expansions over the admissible path set,
  computed before the query runs, and documented as such; nothing in the propagation rules
  implements "taint at `p` implies taint at `p.f`".
- R35. Call/return matching is a property of witness extraction (S17), applied as today by
  the `TaintState` discipline, and is not a propagation rule.
- R36. The search engine is tested differentially against the reference implementation
  (R10) on the flowy corpus, and the Datalog query engine (`CTADL_QUERY_DATALOG`) is either
  retired or kept as the engine the search is checked against; two propagation semantics do
  not survive.
- R37. C0002 (tainted instructions) and C0007 (absorbing functions) are debugging and
  agent views and need only the labels at call-argument vertices, as today.

### 2.7 Models

- R38. A propagation model is a summary row (a store or copy between ports) and nothing else;
  a bridge is a call with explicit port pairs; a dispatch model adds targets; a source or sink
  is an endpoint. No model sets a formal's type (`formal_param(.., Shared)` in
  `models/endpoints.rs:229` and `codegen/model_matches.rs` goes with `FormalType`).
- R39. A port path is in normal form, `#value` may only end it (already enforced,
  `portcullis/parse.rs:816`), and `.*` is rejected (today it parses as a field named `*`,
  docs/portcullis.md:201). `arg(_)` remains an index wildcard.

### 2.8 Reporting, storage, documentation, verification

- R40. No silent loss (docs/spec.md N3): a call with no body and no model, an indirect or
  virtual call with no target, and a function where `mem2reg full` may lose a flow are each
  reported in the index's `notifications.sarif` and carried into the query's SARIF.
- R41. The index records the options of R30; the import records its front-end conventions'
  options (`CTADL_EXP_INT_ARITH`, mem2reg policy) when they are not the
  default.
- R42. The reference implementation (R10), the verifier (R6) and `intent.md` agree; a test
  corpus of flowy programs, one per rule S1–S19, pins each rule with its expected observable.
- R43. The regression suite (`cargo xtask regression`) passes under the new semantics, with
  every changed answer explained by a rule or a listed approximation.
- R44. Front-end golden tests are regenerated and reviewed for the convention changes (R15–R21).
- R45. docs/spec.md §2 (access path), §4.2 (field propagation, finite access paths,
  aliasing, the "value taint only moves forward" sentence) and docs/portcullis.md (ports) are
  rewritten to cite §1 rather than restate it; `intent.md` moves to `docs/ir-semantics.md`
  and becomes the normative semantics.

## 3. Design

### 3.1 One semantics, three consumers

```
                 docs/ir-semantics.md  (S1–S19, normative)
                           │
            ┌──────────────┼──────────────────┐
            ▼              ▼                  ▼
   ctadl-ir::semantics   index rules       query engine
   (reference, R10)      (ctadl-index)     (ctadl-ascent)
            ▲              labels = formal     labels = sources
            │              paths, targets,     over the summarized
   differential tests      strings; guards     program
   (passes, search)
```

The rules are written once as Datalog (docs/spec.md §4.2) in `ctadl-index`, parameterized by
label kind. The index runs them with symbolic labels (formal paths) to produce summaries; the
query runs them with concrete labels (sources) over the program the index summarized. The
reference implementation is a third, naive evaluation of the same rules, written for clarity
and used only in tests.

### 3.2 `ctadl-ir`: data model

- `PathSegment` becomes `Field(Symbol) | Offset(i64) | Deref | Value`. `DEREF_FIELD` and
  `VALUE_FIELD` are deleted; their helper constructors (`PathSegment::deref()`, `::value()`)
  stay as the way to build the variants. `facts::Path` (the interned cons list) stores the
  same variants.
- `AccessPath { base: VariableRef, segments }` becomes an IR operand. `Address` (offsets only)
  and `FieldRef` are deleted. `Exp` becomes `Path(AccessPath) | Value(AccessPath) |
  ObjectRef(CallObject) | Str | Int | Bytes`. `Exp::address`'s rule that an offset-free address
  is a variable becomes the general rule that an empty-path access path prints as its variable.
- `StatementKind` becomes:

  ```rust
  Assign  { dest: AccessPath, sources: SmallVec<[Exp; 2]> }   // store if dest.segments non-empty
  CallAssign { style: CallStyle, rets, args }                  // unchanged
  Phi, ParamFlow, Nop                                           // unchanged
  ```

  `Load`, `Store` and `Update` are `Assign` (an update is a store, R20). The classification
  store/copy is a method, not a variant, so no pass can disagree with S5.
- `CallStyle` is unchanged. `FuncPtrCall.callee` becomes an `AccessPath`. `ParameterType` is
  deleted from `FunctionData` (R26).
- `load_access_path` / `store_access_path` reduce to building one `Assign`. Front ends that
  wanted a temporary per segment (so that SSA could version intermediate pointers) no longer
  need one: the suffix rules make a chain and its composition equivalent (R24), and the
  verifier does not require either form.

### 3.3 `ctadl-ir`: normalization and classification

One module, `mir::path`, owns:

- `normalize(segments) -> segments`: sums adjacent offsets, drops `[0]`, absorbs `#value` with
  adjacent offsets and itself (S2). Idempotent; a property test checks the equations and
  idempotence on random segment lists.
- `classify(segments) -> Void | Address | Location` (S3), in that order.
- `rewrite_address_dest(dest, src) -> (dest, src)` for `p.[k] = e` (S5 preamble).
- `concat(prefix, suffix)` and `strip_prefix(path, prefix) -> Option<suffix>`: the only two
  path operations the engine needs for S6/S8, both normalizing at the junction. `facts::Path`
  and `PathSet` call these and keep their own hashing; `prepend_onto`, `match_prefix_split`
  and the "wild" split tables remain as the indexed forms of the same two operations, with
  tests that they agree with `mir::path` on random inputs.

`AccessPath` construction normalizes, so an un-normalized path cannot exist in a verified
program; `verify` re-checks (R6) because serialized programs and models bypass constructors.

### 3.4 `ctadl-ir`: verification

`MirVerify` gains the checks of R6. It is run by every front end's tests, by `import` before
writing to the store, by `index` after the pass pipeline, and by the reference implementation
before evaluating. A program that fails `verify` is a front-end or pass bug, reported as such,
never analyzed.

### 3.5 Reference implementation (`ctadl-ir::semantics`)

A direct, unoptimized evaluator of S1–S17 over a `ProgramInfo`:

- Input: the program, an initial labeling (locations → labels), a path-length bound `k`, an
  unfolding depth `d`, and the class hierarchy for virtual calls.
- State: `HashMap<(Instance, Variable, Path), HashSet<(Label, Scope)>>`.
- Evaluation: iterate S6, S8, S10, S11 to a fixpoint, enumerating suffixes over the
  paths of length ≤ `k` built from the program's and the labeling's segments, and inlining
  calls to depth `d` with fresh instance ids.
- Output: labels at ports, joined over instances.

It is a few hundred lines, written to read like §1, and is never run on a real artifact. Its
tests are the corpus of R42; the differential tests of R12 and R36 drive it from the pass
pipeline and from the query.

### 3.6 Passes (`ctadl-ir/src/ssa`)

- The pipeline order (dead temps, coalesce, mem2reg, SSA, copy prop) is
  unchanged. Each module's documentation opens with its S18 argument, two or three sentences,
  and its side condition if any.
- `mem2reg`: the escape analysis is the side-condition check. Its "a use other than a direct
  `.deref` access escapes" classification is restated in terms of S3: a slot `k` may be
  promoted if no label can reach `frame.[k].deref` except through the frame variable's own
  direct accesses. `window:N` is a heuristic bound on which slots an escaped address reaches;
  it stays the default and is listed in §4 as a possible loss. `full` is an explicit
  approximation and keeps its per-function notification.
- `coalesce`: merging a single-def, single-use temporary into its use is correct because the
  temporary's cell is reachable by no other path; the pass asserts the single use.
- `copy_prop`: a copy of a copy is a copy (S6 composed); correct without condition.
- `dead_temps`: removing a cell nothing reads changes no observable; correct without
  condition.
- Load-chain composition is no longer a pass because R3 lets a front end emit the composed
  form directly. A front end that prefers chains may keep them; nothing recomposes them.

### 3.7 Front ends

Per front end, what changes; everything else stays.

- **pcode.** Emits `Deref`/`Value` variants instead of the reserved names; `PTRADD`/`PTRSUB`
  with a non-constant operand become copy-plus-value (R16). Frame slot writes, including the
  entry block's binding of stack parameters, become stores (R20), and `push_assign_or_store`
  loses its `Update` branch. `__stack_top.[k].deref` and `$globals.[A].deref` are already
  S19. All parameters were `Shared`; the type is gone and whole-variable write-back through a
  parameter no longer exists (it was not a flow S10 admits).
- **C.** `p->f` becomes `p.deref.f` (R15). Arithmetic becomes value-only (R17); comparisons
  stop merging (`<`, `>`, `!=` at `lib.rs:3935` merge today by oversight). `q = p + i` becomes
  copy-plus-value. `&s.f` remains unnameable and falls back to a copy of `s.f`, listed in
  docs/limitations.md. The same-block `addr_alias` must-alias table is a front-end
  precision device and is kept.
- **JVM / Dex.** Arithmetic becomes value-only; `MethodHandle` constants with a known method
  become `ptr<F>`. Dex's direct
  assignments to `Param` variables are fine (SSA versions them; version 0 is the incoming
  value).
- **CIL.** Arithmetic becomes value-only; `excout` is already R21. Value-type copies remain
  aliasing (docs/limitations.md).
- **Lua.** Arithmetic becomes value-only. Table keys stay symbolic fields (`[3]` is a field,
  not an offset), which S1 permits; `err` results stay empty.
- **flowy.** Gains nothing new in syntax except that `.deref` and `.#value` parse to the
  variants and `.*` is a parse error. It is the language of the R42 corpus and of every
  example in the docs.

### 3.8 Codegen and facts (`ctadl-ascent/src/codegen`, `ctadl-index/src/facts`)

- `assign(site, dst: FlowVertex, src: FlowVertex)` keeps its columns; the kind (store or copy)
  is `dst.path.is_empty()`, computed where needed, so the fact needs no column.
  `update_sans_path` is deleted (R20). `value(p)` lowers to `dst.#value <- p` as today.
- `call_target_assign` and `const_str_assign` stay as the seeds of target and string labels.
- `formal_param` loses its type column. `actual_param` stays; it is now the premise of the
  call-site copies only, never of a reverse edge.
- `cap_path` is deleted (R3). The per-block pre-pass that composed load chains was the one
  place codegen had its own path semantics.
- The admissible path set is computed as today (`compute_paths`), from normal-form paths, and
  `with_value_paths` adds `p.#value` for every member (R29). `deref_paths` stays a config.
- Facts are written with the bumped index format version.

### 3.9 Index rules (`ctadl-index/src/index_engine/mod.rs`)

The rule set keeps its skeleton (`locals`, `assign_like`, `summary`, `call_site`, the
`Decision`/`Cond` machinery, the split tables for joining on path prefixes) and changes the
rules that carry semantics:

- `locals(f, v, p, a, pa, scope, cond)`: one new column, `scope ∈ {Fixed, All}`, a lattice
  column stored the way `Cond` is (`cond_trie`: `Always`/`All` rows in the main trie, the
  rest in the side map, or a second bit in the same store).
- Seed: `locals(f, formal_i, ε, i, ε, Fixed, Always)`. Scope at a bare variable is never read
  (S8 needs a non-empty suffix), so `Fixed` is a placeholder.
- Forward (S6): the existing `ext_dst` rules, extended to carry scope: through a copy the
  derived row is `Fixed`; through a store (destination path non-empty) it is `All`.
- Backward (S8): one rule shape replaces the "formal side" rules (`edge_split` at
  `mod.rs:1664`, `ext_fml`) and the alias summary rule (`mod.rs:1760`):

  ```
  locals(f, v2, p2·t, a, pa, All, c) <--
      assign_like(f, v1, p1, v2, p2, c1), locals(f, v1, p1·t, a, pa, All, c2),
      t ≠ ε, t ≠ #value, ps.concat(p2, t) = Some(p2·t), c = c1.and(c2);
  ```

  with the split tables used to find `t` as they are used today for the forward direction.
- Summary (S14, R27): `summary(f, n1, p1, n2, p2, c) <-- locals(f, dst, p1, n2, p2, s, c),
  formal_param(f, dst), n1 = dst.as_formal(), out(n1, p1, s)` where `out` is: `n1` is a
  return index, or `p1 ≠ ε ∧ p1 ≠ #value ∧ s = All`. `isout` and `FormalType` are deleted.
- Call sites (S10): `assign_like(caller, call_arg(insn, n), ε, v, p, Always) <-- actual_param`
  (one direction), the summary replay rule as today, and `assign_like(caller, v, p,
  call_arg(insn, ret), ε)` for results. The backward rule then carries a callee's `All`
  labels at `p_i.t` to `a_i.t`, which is what the reverse edge used to approximate.
- Target labels: `call_target_assign_like` keeps its relation but its rules are generated
  from the same forward/backward shapes (R28), with the backward direction now included
  (a function pointer stored through an alias reaches the original, S11/S8). `const_reaches`
  likewise.
- Guards: unchanged (`critical_summary`, `resolvent`, rule 3.1/3.2, `ContextJoin`).
- `compute_alias_of_formal` and `prog_store` are deleted from `EngineInput`.
- Reporting (R40): `callee_info` rows that never acquire a `call_site`, and `call` rows to a
  bodyless function with no summary, are collected into notifications at the end of the
  fixpoint.

### 3.10 Query (`ctadl-ascent/src/query_engine`)

- The search engine's successor relation (`labeled_successors`) is restated as S6/S7/S8
  over the query's `assign` edges, with `Call`/`Return` edges as S10's copies. Concretely:
  the direct-assign step keeps carrying the suffix; the destination scope is `All` iff the
  edge is a store or the node is a seed; the object hop is the backward rule and fires only
  for a non-empty suffix that is not `#value` (today it also fires at the empty path,
  `search.rs:477`, which S8 forbids); the "field-alias back-flow" step (`search.rs:459`) is
  the same backward rule and merges with it; the address step (`search.rs:440`) is S3; the
  functional-update step (`search.rs:430`) is deleted.
- `TaintLevel::Saturating` and sink `wildcard` move out of the successor relation into
  endpoint expansion (`endpoints.rs`): a saturating source at `p` becomes one seed per
  admissible extension of `p`; a wildcard sink likewise (as it already is). The
  `saturating_read` and `sink_ext_by_var` steps are deleted.
- `#value`: a source at `p` also seeds `p.#value`; a sink at `p` also matches `p.#value`
  (S17), as today.
- The Datalog query engine is kept as the checked-against engine for R36 or deleted; the
  design prefers keeping it, since it is the rule text run with source labels and is the
  shortest path to "the query is the same rules".
- Witness extraction, `TaintState`, SARIF formatting, C0001–C0007: unchanged in shape.

### 3.11 Models (`ctadl-ascent/src/models`, Portcullis)

- Propagation → `summary` rows, unchanged. Whether a row is a store or copy follows from its
  paths (S5), so docs/portcullis.md's "prefix substitution" paragraph becomes a citation of S6.
- Bridges and JNI → `call` plus `actual_param` in one direction and explicit `assign` rows
  for the outward direction where a port pair is bidirectional; the JNI bridge's reliance on
  `actual_param` being bidirectional (`languages/jni.rs:20-31`) becomes explicit outward
  copies for out-parameters (`.deref` paths), which S8 then carries.
- Endpoints: `formal_param(.., Shared)` is no longer emitted. Call-site anchoring of a source
  on an external function's return stays.
- Dispatch models: unchanged.
- The port parser normalizes and rejects `.*` (R39).

### 3.12 Reporting

`notifications.sarif` gains three message ids: unresolved indirect call (site, callee path),
call to a bodyless function with no model (site, name), and the existing full-promotion
notice. `ctadl query` copies them through as it does now. The SARIF driver is shared and
versioned as today.

### 3.13 Storage and formats

- Import: `bitcode` of the new `ProgramInfo`; format version bumped; `open_import` rejects
  older versions with the re-import message.
- Index: parquet schemas unchanged in column names except `formal_param` (type column
  removed); `paths.parquet` holds normal-form paths; index format version bumped;
  `index_config.json` records `hybrid_context`, `deref_paths` and the import-time convention
  options (R41).
- Summary projects (`--summary`): a summary written by an older index is rejected like an
  older index.

### 3.14 Testing

- `ctadl-ir`: property tests for `mir::path` (equations, idempotence, agreement with
  `facts::Path` and `PathSet`); verifier tests, one per R6 check.
- `ctadl-ir::semantics`: the R42 corpus, one flowy program per rule with its expected
  observable, as the reference's own unit tests.
- Passes: R12 differential tests over the corpus plus the existing pass tests.
- Index: the existing `index_engine_tests.rs` cases, re-baselined with each change explained;
  new cases for backward through a store (`l = y; l.f = x`), two-store merge (S9),
  function pointer through an alias, and summary `out` under scope.
- Query: R36 differential tests of the search against the reference on the corpus; the
  existing `tests/tnt` cases, re-baselined.
- Front ends: golden tests regenerated (R44); nightly regression (R43) with every changed
  answer attributed.

## 4. Approximations register

The implementation deviates from §1 only here. "Loses" means a flow the semantics derives
may be absent; "adds" means a flow the semantics does not derive may be present.

| Where | What | Direction | Reported |
| --- | --- | --- | --- |
| index, query | Admissible path set (R29): suffixes outside program ∪ model ∪ one-level concat are dropped | loses | no (structural) |
| index | Cells inside a callee merge all callers (S14) | adds | no (structural) |
| index | Resolution ladder: dispatch model, CHA under threshold | adds | config recorded |
| index | Hybrid context widening (`collapse`, `bounded`, `spill`) | adds | config recorded |
| passes | `mem2reg window:N`: an escape reaching past the window | loses | no; `exact`/`above` avoid it |
| passes | `mem2reg full` | loses | per function |
| query | Call/return matching by one bit, not a site stack (R35) | adds (witness only) | no |
| front ends | S19 conventions; C `&s.f`; CIL value types; Lua `err` never filled | both | docs/limitations.md |
| front ends | pcode `merge` arithmetic | adds | config recorded |

Anything not in this table that differs from the reference implementation is a bug.

## 5. Decisions required before planning

1. **Fate of the Datalog query engine (R36, §3.10).** Keep as the checked-against engine
   (preferred) or delete and check the search only against the reference.
