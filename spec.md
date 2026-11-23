# Step properties written by `ctadl query`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines what `ctadl query` writes on each step of a code flow, and
how `ctadl diff-query` reads it. It must stay consistent with `docs/spec.md` (F9, F12, §4.5,
§5) and `docs/debugging.md` (*Comparing two query outputs*). It does not sequence the work.

## Background

- A step gets a location, `executionOrder`, one `kinds` string, and a message
  `<word> <variable><path> in <function>` (`push_site_step`,
  `ctadl-ascent/src/query_engine/formatter.rs:2801`, built at `:2857`). It has no
  `properties` bag. Code flows are written in the `human`, `agent` and `debug` profiles only
  (`formatter.rs:2554`).
- Steps are written at calls only. An assignment, alias or global edge of the path has no
  instruction (`FlowEdge::site`, `ctadl-index/src/facts.rs:1745`) and gives no step.
- Four pieces of code write steps, per flow in this order:

  | Written for | At | Vertex in the message | Function in the message |
  | --- | --- | --- | --- |
  | the source endpoint | `formatter.rs:2869` | the endpoint's | a callee of its call site (`endpoint_callee`, `:3286`) |
  | a call or return edge | `:2920` | the edge's destination | the destination's function: the callee on a call, **the caller on a return** |
  | an interior `call-arg` vertex | `:2959` | that vertex | a callee of its call site, else the enclosing function |
  | the sink endpoint | `:2987` | the endpoint's | a callee of its call site |

- A step is not written when it has the location of the step before it; an endpoint step
  only when it also has that step's instruction (`formatter.rs:2847`).
- A call edge starts at a `call-arg` vertex and a return edge ends at one
  (`ctadl-ascent/src/query_engine/search.rs:546`, `:521`). The path usually reaches a call's
  `call-arg` vertex by an assignment, so the interior `call-arg` step is written first and
  the call edge's step, at the same location, is dropped. The step at a call the path enters
  is therefore almost always the `call-arg` one, which does not know which callee is entered.
  A return edge's step is written before the `call-arg` step for its destination, so a return
  step is the edge's.
- The path reaches the `call-arg` vertex whose step is written by a copy from the actual. It
  then leaves by a call edge into a callee, or by an edge that stands for the call's effect
  (a summary or a propagation model): to another `call-arg` vertex of the same call, or to a
  global the callee writes. It cannot go back to the actual: the alias hop is not taken
  after a copy (`search.rs:462`).
- "A callee of a call site" is looked up in `call_callee` (`formatter.rs:2571`), a map built
  by collecting `facts.call`. A site with several callees keeps the one that comes last in
  `facts.call`, which is the static calls followed by the calls the index resolved
  (`final_call`, `ctadl-index/src/index_engine/mod.rs:802`). That order is not a function of
  the program's content. A path result's `sourceFunctions` and `sinkFunctions`
  (`formatter.rs:3210`), which `diff-query` compares, come from the same map.
- A call-site endpoint does not record which modeled function it came from: `infunc` is the
  caller, and the modeled callee is only the test that selected the site
  (`anchored_at_callsites`, `ctadl-index/src/facts/query_endpoint.rs:96`).
- A flow has exactly one source endpoint and one sink endpoint: each (source vertex, sink
  vertex) pair is searched once (`tested_pairs`, `formatter.rs:2614`), and a path's first and
  last node fix the pair. But a flow's first or last step is not always theirs:
  - The sink step is dropped after a return edge into the sink's own vertex, or when the
    source and the sink are at one call and nothing lies between.
  - An endpoint with no call site (a global, or a function with no callers) gets no step.
- `diff-query` reads `taintVertex` and `callee` from a step's `properties`, each a string,
  and drops the instruction id of a `call-arg` vertex (`step`,
  `ctadl-query-diff/src/parse.rs:228`). `ctadl query` has never written either. The tests add
  them to real output themselves (`with_step_props`,
  `ctadl-ascent/tests/integration/diff_query.rs:199`).
- The regression suite reads steps by message prefix, and the sink's function from the text
  after ` in ` (`xtask/src/assertions.rs:204`, `:223`).

## Terms

| Term | Meaning |
| --- | --- |
| **Step** | A `threadFlowLocation` of a code flow. |
| **Step properties** | In a step's `properties` bag: `taintVertex`, a string; `callees`, a list of strings; `summarized`, the boolean `true`. There is no `callee` property. |
| **The site's callees** | Every function the final call graph gives a call site. |
| **Call step** | A step at a call the path enters: a call edge leaves the step's vertex, or the step is the call edge's own. |
| **Return step** | A step written for a return edge. |
| **Summarized step** | A step for an interior `call-arg` vertex that is not a call step: the path crosses the call without entering a callee. |

## Requirements

**What `ctadl query` writes**

- R1. **`taintVertex` on every step.** Every step has a `taintVertex`: the vertex its message
  names, as `<variable><path>`, the form the result property of that name has
  (`formatter.rs:3325`).
- R2. **`callees` on a call or return step.** A call step's `callees` is one name, the
  function the path enters. A return step's `callees` is one name, the function returned
  from, which the step's message does not name. The other callees of the site are not
  listed.
- R3. **`callees` on a source or sink step.** A source or sink step's `callees` is the names
  of the site's callees, each once, sorted.
- R4. **`summarized` on a summarized step.** A summarized step has `summarized: true` and no
  `callees`: nothing records which callee's summary carried the taint. No other step has
  `summarized`.
- R5. **Every step says something about callees.** Every step has `callees` or
  `summarized`, never both.
- R6. **By content.** No step property depends on function ids or on the order of the call
  list. The instruction id in a `call-arg` vertex is the one id a property holds.
- R7. **Messages.** A call step's message names the function the path enters. A return
  step's message is unchanged. Where a source, sink or summarized step's
  message names a callee, it names the site's callee whose name sorts first.
- R8. **Steps do not change.** Which steps a flow has, their order, locations, `kinds` and
  `executionOrder` are what they were. A step that was not written is still not written, and
  its vertex appears nowhere. So a flow's first or last step is not always its source or sink
  step (Background), and `--endpoints-only` then compares the step that is there.
- R9. **Results at a site with several callees.** A `C0001` result's `sourceFunctions` and
  `sinkFunctions` hold all of the callees of its endpoints' sites, and `sourceCallee` and
  `sinkCallee` the first of each. A `C0003`/`C0004` result's message and logical location
  name the first by name.
- R10. **Nothing else changes.** At a call site with one callee, results, their properties,
  notifications, invocations and message text are byte for byte what they were. The `machine`
  profile writes no steps, so only R9 can change its output.

**What `diff-query` compares**

- R11. **`callees` and `summarized`, not `callee`.** A step is compared by its `kinds`, its
  location, its `taintVertex`, its `callees` and its `summarized`. `callees` is compared as a
  set: the order written does not matter. For each, absent compares equal only to absent.
  Under `--endpoints-only` the first and last step's are compared. A printed step shows both.
  A step's `callee` is no longer read.
- R12. **Malformed input.** A step `callees` that is not a list of strings, or a `summarized`
  that is not a boolean, is an error (exit 2).
- R13. **Old files.** Files `ctadl query` wrote before this change compare as they did.
- R14. **Renumbering is invisible.** Two queries of one program, indexed so that its function
  and instruction ids differ, compare `IDENTICAL`, with and without `--endpoints-only`, in
  every profile. This holds for a program whose source or sink is at a call site with
  several callees.
- R15. **Proofs.** `sarif-canon` and its Verus theorems do not change.

**Tests and documents**

- R16. **Tests on real properties.** The metamorphic tests of `diff-query` run on the
  properties `ctadl query` wrote, not on ones the test added. A changed `taintVertex` or
  `callees`, or a removed `summarized`, is reported, at an interior step only without
  `--endpoints-only`. Renumbered instruction ids and a reordered `callees` are not.
- R17. **What each step says is tested.** Tests check the properties of each kind of step: a
  call step at a site with two callees, of which the path enters one; a return step; a
  summarized step, both between two arguments and into a global; a source or sink step at a
  site with two callees. On every flow the tests query, each step has `callees` or
  `summarized`.
- R18. **Documents.** `docs/debugging.md` (`:259`), the `diff-query` help text
  (`ctadl-ascent/src/main.rs:93`) and `docs/spec.md` (§5, `:261`) no longer say the
  properties are not written, and name `callees` and `summarized` where they named `callee`.
  `docs/spec.md` §4.5 says a step carries them. `docs/debugging.md` says which step has
  which property, what `callees` holds on a return step, and when a flow's first or last step
  is not its source or sink. None of them, and no code comment, cites a number from this
  document.

## Design

### D1. What a step says (R1–R5, R7, R8)

One small value describes a step before it is written: its vertex (`FlowVariable`, `Path`),
what it says about callees, and the function its message names. `push_site_step`
(`formatter.rs:2801`) takes it in place of the finished message string, and renders both the
message and the property bag from it. The message and the properties then cannot disagree
about the vertex.

| Step | `taintVertex` | `callees` | `summarized` | Message names |
| --- | --- | --- | --- | --- |
| source, sink | endpoint vertex | the site's callees | | their first by name |
| call | its vertex | the function entered | | that function |
| return | the edge's destination | the edge's origin's function | | the destination's function |
| summarized | the `call-arg` vertex | | `true` | the site's first callee by name, else the enclosing function |

The step for an interior `call-arg` vertex (`formatter.rs:2959`) is classified by the path
edge that leaves the vertex, which the walk has in hand as the next window:

- a call edge: a call step; the function entered is the edge's destination's function;
- anything else: a summarized step.

A step written for a call edge itself (`formatter.rs:2920`), when it is not dropped, is a
call step by the same rule. Its `kinds` stay as they are in every case.

The vertex is formatted by the one expression the result property uses
(`formatter.rs:3325`), shared, not copied. The bag goes on the `ThreadFlowLocation`, where
`diff-query` reads it.

Invariant: the dedup test (`formatter.rs:2847`) does not read the new value. Whether a step
is written depends on what it did before (R8). A dropped step's classification is lost with
it: when a summarized crossing's first `call-arg` step is dropped, the call is not marked.

### D2. The site's callees (R3, R6, R7, R9)

`call_callee` (`formatter.rs:2571`) becomes a map from a site to its callees, sorted by name
and then deduplicated by name. Names come from `facts.id_to_name`; a function with no name
takes the `func#<id>` text the message already uses. The map holds one entry per row of
`facts.call`, which is what the vector it is built from holds.

`endpoint_callee` (`formatter.rs:3286`) returns the first of a site's callees. Its users
that want one function keep it: step messages and the source and sink results
(`formatter.rs:3328`). A path result's `sourceFunctions` and `sinkFunctions`
(`formatter.rs:3210`) take the whole list, so they no longer depend on the order of the call
list (R14). An endpoint step exists only for an endpoint with a call site, and that site was
chosen from the call list, so its `callees` is never empty.

### D3. `callees` and `summarized` in `diff-query` (R11–R13, R15)

The canonical step keeps its one optional atom for the callee
(`sarif-canon/src/types.rs:134`, `:186`); only what the parser puts in it changes. `step`
(`ctadl-query-diff/src/parse.rs:228`) reads `callees` and `summarized` in place of `callee`.
When the step has neither, the atom is absent. Otherwise the atom is the text of a JSON
object with the keys the step has: `callees` as the list sorted with duplicates dropped,
`summarized` as written. So `sarif-canon` is untouched (R15). The set comparison of R11 is
the parser's, covered by its tests, not by the proofs; the execution notifications are
canonicalized the same way. `render.rs:99` prints the object's keys back under `properties`.

Invariant: the canonical text is a function of the set of names and the flag only, so equal
content gives one atom and unequal content gives two.

### D4. Tests (R14, R16, R17)

`with_step_props` and the "with step properties" bases (`diff_query.rs:199`, `:224`) go: the
real output is the base. `renumbered_ids` (`diff_query.rs:757`) then covers R14 for
`ties.c`. A second fixture has a call through a function pointer or a virtual call that
resolves to two functions, with a flow that enters one of them and a source or sink at such
a call; it is indexed twice, with the callees' ids in opposite orders. It covers R14 and R17
for several callees.

The regression suite's readers of message text (`xtask/src/assertions.rs:204`, `:223`) do not
change; the sink function they read is the site's first callee by name.

## Concerns

None open.
