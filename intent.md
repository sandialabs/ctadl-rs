# Intent: write what a step says as structured step properties, in `ctadl query` - DO-NOT-MERGE

A step of a flow names its tainted vertex and callee only in message text. Write them as
structured step properties, which `diff-query` then compares: `taintVertex`, a string, and
`callees`, always a list of function names. There is no separate `callee` property;
`diff-query` reads `callees` in its place and compares it as a set.

- A call step's `callees` is the one function the path enters. The step at such a call is
  usually written for the call's argument vertex, not for the call edge
  (`ctadl-ascent/src/query_engine/formatter.rs:2959`), so it is classified by the path edge
  that leaves that vertex.
- A return step's `callees` is the one function returned from. The message keeps naming the
  function returned into, the caller (`formatter.rs:2920`).
  A call site with several callees does not change either: the edge the path traverses names
  one.
- A source or sink step has `taintVertex` only. Every callee of its call site is written
  once, in the result's `sourceFunctions` or `sinkFunctions`, which `diff-query` already
  compares; the step does not repeat the list. The endpoint does not record which of them
  its model named.
- Any other step at a call inside the flow is one the path crosses by a summary, without
  entering a callee. It has `summarized: true` and no `callees`: nothing records which callee's summary carried the
  taint. `diff-query` compares the flag.

A path result no longer has `sourceCallee` and `sinkCallee`: each repeated the first entry of
`sourceFunctions` or `sinkFunctions`, and neither is in the SARIF standard.

Which steps a flow has does not change. A flow has one source and one sink endpoint, but
its first or last step is not always theirs: the sink step is dropped when the step before
it is at the same instruction (`formatter.rs:2847`), and an endpoint with no call site gets
no step. That is accepted and documented.
