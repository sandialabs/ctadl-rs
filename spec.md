# Step properties written by `ctadl query`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines what `ctadl query` writes on each step of a code flow, so
that `ctadl diff-query` (which already reads the two properties) has something to compare. It
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
| **Step properties** | `taintVertex` and `callee` in a step's `properties` bag, each a string. |
| **The site's callee** | Of the functions the final call graph gives a call site, the one whose name sorts first. |

## Requirements

**What `ctadl query` writes**

- R1. **`taintVertex` on every step.** Every step has a `taintVertex`: the vertex its message
  names, as `<variable><path>`, the form the result property of that name has
  (`formatter.rs:3325`).
- R2. **`callee` on a call-site step.** A source, sink or summarized-call step has `callee`,
  the name of the site's callee. When the call graph gives the site no callee, the step has no
  `callee`.
- R3. **`callee` on an edge step.** A call step's `callee` is the function called. A return
  step's `callee` is the function returned from. (C1.)
- R4. **One callee, by name.** `callee` is one function name, never a list. At a site with
  several callees it is the same name whatever ids the index gave the functions. (C2.)
- R5. **Property and message agree.** Where a step's message names a callee (source, sink,
  call, summarized call), it names the function in `callee`. A return step's message still
  names the function returned into.
- R6. **Steps do not change.** Which steps a flow has, their order, locations, `kinds` and
  `executionOrder` are what they were. A step that was not written is still not written, and
  its vertex appears nowhere. (C3.)
- R7. **Nothing else changes.** Results, their properties, notifications, invocations and
  message text are byte for byte what they were, except the function named in a message at a
  call site with several callees (R5). The `machine` profile's output does not change at all.

**What `diff-query` then sees**

- R8. **Renumbering is invisible.** Two queries of one program, indexed so that its function
  and instruction ids differ, compare `IDENTICAL` with the step properties written, with and
  without `--endpoints-only`, in every profile. This holds for a program with a call site
  that has several callees.
- R9. **`diff-query` does not change.** Its reader, canonical form, proofs and report are
  untouched. Files written before this change still compare as they did.

**Tests and documents**

- R10. **Tests on real properties.** The metamorphic tests of `diff-query` run on the
  properties `ctadl query` wrote, not on ones the test added: a changed `taintVertex` or
  `callee` is reported, at an interior step only without `--endpoints-only`, and renumbered
  instruction ids are not.
- R11. **What each step says is tested.** A test checks, on a flow that has all four kinds of
  step, the `taintVertex` and `callee` of each, the return step's included, and that a step
  at a site with no callee has no `callee`.
- R12. **Documents.** `docs/debugging.md` (`:259`), the `diff-query` help text
  (`ctadl-ascent/src/main.rs:93`) and `docs/spec.md` (§5, `:261`) no longer say the
  properties are not written. `docs/spec.md` §4.5 says a step carries them.
  `docs/debugging.md` says what `callee` holds on a return step and at a site with several
  callees, and when a flow's first or last step is not its source or sink. None of them, and
  no code comment, cites a number from this document.

## Design

### D1. What a step says (R1–R3, R5)

One small value describes a step before it is written: its vertex (`FlowVariable`, `Path`),
its `Option<FunctionId>` callee, and the function its message names. `push_site_step`
(`formatter.rs:2801`) takes it in place of the finished message string, and renders both the
message and the property bag from it. The message and the properties then cannot disagree
about the vertex, and agree about the callee wherever the message names one.

| Step | `taintVertex` | `callee` | Message names |
| --- | --- | --- | --- |
| source, sink | endpoint vertex | the site's callee | `callee` |
| call edge | destination vertex | destination's function | `callee` |
| return edge | destination vertex | origin's function | destination's function |
| summarized call | the `call-arg` vertex | the site's callee, or none | `callee`, else the enclosing function |

The vertex is formatted by the one expression the result property uses
(`formatter.rs:3325`), shared, not copied. The bag goes on the `ThreadFlowLocation`, where
`diff-query` reads it, with `callee` left out when there is none.

Invariant: the dedup test (`formatter.rs:2847`) does not read the new value. Whether a step
is written depends on what it did before (R6).

### D2. The site's callee (R2, R4, R8)

`call_callee` (`formatter.rs:2571`) keeps, for each site, the callee whose name sorts first,
by the names in `facts.id_to_name`; a function with no name sorts by the `func#<id>` text the
message already uses. Two callees with one name give one string, so no further tie-break is
observable. The map stays one entry per call site.

`endpoint_callee` (`formatter.rs:3286`) and the source and sink results that use it
(`formatter.rs:3328`) read the same map, so a `C0003`/`C0004` result and the step for the
same endpoint name one function. Its fallback to `infunc` stays for results; a step takes
`None` instead (R2).

### D3. Tests (R8, R10, R11)

`with_step_props` and the "with step properties" bases (`diff_query.rs:199`, `:224`) go: the
real output is the base. `renumbered_ids` (`diff_query.rs:757`) then covers R8 for `ties.c`.
A second fixture with a call through a function pointer or a virtual call that resolves to
two functions, whose source or sink is at that call, covers R8 for several callees: indexed
twice, with the callees' ids in opposite orders.

The regression suite's readers of message text (`xtask/src/assertions.rs:204`, `:223`) do not
change; the sink function they read is now the site's callee.

## Concerns

- C1. **`callee` on a return step.** The message names the caller. Options: the function
  returned from; the caller, as the message; no `callee` on a return. The caller is already
  compared, as the step's logical location. The spec assumes the function returned from (R3).
- C2. **Several callees at one call site.** Options: one name, the first by name (assumed,
  R4); every name, as a list. A list makes a changed callee set visible, but changes
  `diff-query`'s reader and the proved canonical step (an atom becomes a sorted set). One
  name keeps both, but a callee that is added or removed behind the first goes unseen, and a
  source or sink step may name a function other than the one its model named, since the
  endpoint does not record that.
- C3. **A flow whose first or last step is not its source or sink.** See Background. Options:
  accept and document (assumed, R6); always write the sink step, which adds a step to some
  existing flows; give a function-anchored endpoint a step, which needs a location for it.
  Under `--endpoints-only` an accepted case compares an interior step in the endpoint's place.
