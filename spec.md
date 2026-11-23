# Step properties written by `ctadl query`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines what `ctadl query` writes on each step of a code flow, and how
`ctadl diff-query` reads it. It
must stay consistent with `docs/spec.md` (F9, F12, §4.5, §5) and `docs/debugging.md`
(*Comparing two query outputs*). It does not sequence the work.

## Background

- A step gets a location, `executionOrder`, one `kinds` string, and a message
  `<word> <variable><path> in <function>` (`push_site_step`,
  `ctadl-ascent/src/query_engine/formatter.rs:2801`, built at `:2857`). It has no
  `properties` bag. Code flows are written in the `human`, `agent` and `debug` profiles only
  (`formatter.rs:2554`).
- There are four kinds of step, all written per flow in this order:

  | Step | Written at | Vertex in the message | Function in the message |
  | --- | --- | --- | --- |
  | source | `formatter.rs:2869` | the source endpoint's | the callee at its call site (`endpoint_callee`, `:3286`) |
  | call or return edge | `:2920` | the edge's destination | the destination's function: the callee on a call, **the caller on a return** |
  | summarized call | `:2959` | an interior `call-arg` vertex | the callee at that call site, else the enclosing function |
  | sink | `:2987` | the sink endpoint's | the callee at its call site |

- "The callee at a call site" is looked up in `call_callee` (`formatter.rs:2571`), a map built
  by collecting `facts.call`. A site with several callees keeps the one that comes last in
  `facts.call`, which is the static calls followed by the calls the index resolved
  (`final_call`, `ctadl-index/src/index_engine/mod.rs:802`). That order is not a function of
  the program's content.
- A call or return edge names its one callee exactly: the destination's function on a call,
  the origin's function on a return. Only the source, sink and summarized-call steps go
  through `call_callee`.
- A call-site endpoint does not record which modeled function it came from: `infunc` is the
  caller, and the modeled callee is only the test that selected the site
  (`anchored_at_callsites`, `ctadl-index/src/facts/query_endpoint.rs:96`).
- A flow has exactly one source endpoint and one sink endpoint. Each (source vertex, sink
  vertex) pair is searched once (`tested_pairs`, `formatter.rs:2614`), and a path's first and
  last node fix the pair, so the `details` of a path hold one entry. The intent's third
  question (several endpoints, one step) does not arise in that form. What does arise:
  - A step is not written when it has the location of the step before it; an endpoint step
    only when it also has that step's instruction (`formatter.rs:2847`). So the sink step is
    dropped after a return edge into the sink's own vertex, or when the source and the sink
    are at one call and nothing lies between.
  - An endpoint with no call site (a global, or a function with no callers) gets no step.
  In both cases a flow's first or last step is not its source or sink step.
- `diff-query` reads `taintVertex` and `callee` from a step's `properties`, each a string,
  and drops the instruction id of a `call-arg` vertex (`step`,
  `ctadl-query-diff/src/parse.rs:228`). Its tests add the properties to real output
  themselves (`with_step_props`, `ctadl-ascent/tests/integration/diff_query.rs:199`).
- The regression suite reads steps by message prefix, and the sink's function from the text
  after ` in ` (`xtask/src/assertions.rs:204`, `:223`).

## Terms

| Term | Meaning |
| --- | --- |
| **Step** | A `threadFlowLocation` of a code flow. |
| **Step properties** | In a step's `properties` bag: `taintVertex`, a string, and `callees`, a list of strings. There is no `callee` property. |
| **The site's callees** | Every function the final call graph gives a call site. |
| **Edge step** | A step written for a call or return edge of the path. |
| **Summarized-call step** | A step written for an interior `call-arg` vertex, where the path crosses a call by the callee's summary. |

## Requirements

**What `ctadl query` writes**

- R1. **`taintVertex` on every step.** Every step has a `taintVertex`: the vertex its message
  names, as `<variable><path>`, the form the result property of that name has
  (`formatter.rs:3325`).
- R2. **`callees` on an edge step.** A call step's `callees` is one name, the function
  called. A return step's `callees` is one name, the function returned from, which the
  step's message does not name.
- R3. **`callees` on a source or sink step.** A source or sink step's `callees` is the names
  of the site's callees, each once, sorted.
- R4. **Nothing on a summarized-call step.** A summarized-call step has `taintVertex` only.
  The edge it stands for does not say which callee's summary carried the taint.
- R5. **By content.** No step property depends on function ids or on the order of the call
  list. The instruction id in a `call-arg` vertex is the one id a property holds.
- R6. **Messages.** A call or return step's message is unchanged. Where a source, sink or
  summarized-call step's message names a callee, it names the site's callee whose name sorts
  first.
- R7. **Steps do not change.** Which steps a flow has, their order, locations, `kinds` and
  `executionOrder` are what they were. A step that was not written is still not written, and
  its vertex appears nowhere. (C1.)
- R8. **Results at a site with several callees.** A `C0001` result's `sourceFunctions` and
  `sinkFunctions` hold all of the callees of its endpoints' sites, and `sourceCallee` and
  `sinkCallee` the first of each. A `C0003`/`C0004` result's message and logical location
  name the first by name.
- R9. **Nothing else changes.** At a call site with one callee, results, their properties,
  notifications, invocations and message text are byte for byte what they were. The `machine`
  profile writes no steps, so only R8 can change its output.

**What `diff-query` compares**

- R10. **`callees`, not `callee`.** A step is compared by its `kinds`, its location, its
  `taintVertex` and its `callees`. `callees` is compared as a set: the order written does not
  matter, and absent compares equal only to absent. Under `--endpoints-only` the first and
  last step's `callees` are compared. A printed step shows it as a list. A step's `callee`
  is no longer read; `ctadl query` never wrote one.
- R11. **Malformed input.** A step `callees` that is not a list of strings is an error (exit
  2).
- R12. **Old files.** Files `ctadl query` wrote before this change compare as they did.
- R13. **Renumbering is invisible.** Two queries of one program, indexed so that its function
  and instruction ids differ, compare `IDENTICAL`, with and without `--endpoints-only`, in
  every profile. This holds for a program whose source or sink is at a call site with
  several callees.
- R14. **Proofs.** `sarif-canon` and its Verus theorems do not change.

**Tests and documents**

- R15. **Tests on real properties.** The metamorphic tests of `diff-query` run on the
  properties `ctadl query` wrote, not on ones the test added. A changed `taintVertex`
  or `callees` is reported, at an interior step only without `--endpoints-only`.
  Renumbered instruction ids and a reordered `callees` are not.
- R16. **What each step says is tested.** A test checks, on a flow that has all four kinds of
  step, the properties of each: the return step's `callees`, a source or sink step's `callees`
  at a site with two callees, and that a summarized-call step has none.
- R17. **Documents.** `docs/debugging.md` (`:259`), the `diff-query` help text
  (`ctadl-ascent/src/main.rs:93`) and `docs/spec.md` (§5, `:261`) no longer say the
  properties are not written, and name `callees` where they named `callee`. `docs/spec.md`
  §4.5 says a step carries them. `docs/debugging.md` says which step has which property,
  what `callees` holds on a return step, and when a flow's first or last step is not its
  source or sink. None of them, and no code comment, cites a number from this document.

## Design

### D1. What a step says (R1–R4, R6)

One small value describes a step before it is written: its vertex (`FlowVariable`, `Path`),
what it says about callees (one function, a site's callees, or nothing), and the function
its message names. `push_site_step` (`formatter.rs:2801`) takes it in place of the finished
message string, and renders both the message and the property bag from it. The message and
the properties then cannot disagree about the vertex.

| Step | `taintVertex` | `callees` | Message names |
| --- | --- | --- | --- |
| source, sink | endpoint vertex | the site's callees | their first by name |
| call edge | destination vertex | destination's function | that function |
| return edge | destination vertex | origin's function | destination's function |
| summarized call | the `call-arg` vertex | absent | the site's first callee by name, else the enclosing function |

The vertex is formatted by the one expression the result property uses
(`formatter.rs:3325`), shared, not copied. The bag goes on the `ThreadFlowLocation`, where
`diff-query` reads it.

Invariant: the dedup test (`formatter.rs:2847`) does not read the new value. Whether a step
is written depends on what it did before (R7).

### D2. The site's callees (R3, R5, R6, R8)

`call_callee` (`formatter.rs:2571`) becomes a map from a site to its callees, sorted by name
and then deduplicated by name. Names come from `facts.id_to_name`; a function with no name
takes the `func#<id>` text the message already uses. The map holds one entry per row of
`facts.call`, which is what the vector it is built from holds.

`endpoint_callee` (`formatter.rs:3286`) returns the first of a site's callees. Its users
that want one function keep it: step messages and the source and sink results
(`formatter.rs:3328`). A path result's `sourceFunctions` and `sinkFunctions`
(`formatter.rs:3210`) take the whole list. `diff-query` compares those two already, so today
two indexes that order the calls differently can differ there; with this they do not (R13).
An endpoint step exists only for an endpoint with a call site, and that site was chosen from
the call list, so `callees` is never empty.

### D3. `callees` in `diff-query` (R10–R12, R14)

The canonical step keeps its one optional atom for the callee
(`sarif-canon/src/types.rs:134`, `:186`); only what the parser puts in it changes. `step`
(`ctadl-query-diff/src/parse.rs:228`) reads `callees` in place of `callee`: it sorts the
list, drops duplicates, writes it as the text of a JSON array, and interns that. So
`sarif-canon` is untouched (R14). The set comparison of R10 is the parser's, covered by its
tests, not by the proofs; the execution notifications are canonicalized the same way.
`render.rs:99` prints the atom back as an array under `callees`.

Invariant: the canonical text is a function of the set of names only, so equal sets give
one atom and unequal sets give two.

### D4. Tests (R13, R15, R16)

`with_step_props` and the "with step properties" bases (`diff_query.rs:199`, `:224`) go: the
real output is the base. `renumbered_ids` (`diff_query.rs:757`) then covers R13 for
`ties.c`. A second fixture with a call through a function pointer or a virtual call that
resolves to two functions, whose source or sink is at that call, covers R13 and R16 for
several callees: indexed twice, with the callees' ids in opposite orders.

The regression suite's readers of message text (`xtask/src/assertions.rs:204`, `:223`) do not
change; the sink function they read is the site's first callee by name.

## Concerns

- C1. **A flow whose first or last step is not its source or sink.** See Background. Options:
  accept and document (assumed, R7); always write the sink step, which adds a step to some
  existing flows; give a function-anchored endpoint a step, which needs a location for it.
  Under `--endpoints-only` an accepted case compares an interior step in the endpoint's place.
