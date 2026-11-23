# Intent: write what a step says as structured step properties, in `ctadl query` - DO-NOT-MERGE

A step of a flow names its tainted vertex and callee only in message text. Write them as
structured step properties, which `diff-query` then compares: `taintVertex`, a string, and
`callees`, always a list of function names. There is no separate `callee` property;
`diff-query` reads `callees` in its place and compares it as a set.

- A call step's `callees` is the one function called.
- A return step's `callees` is the one function returned from. The message keeps naming the
  function returned into, the caller (`ctadl-ascent/src/query_engine/formatter.rs:2920`).
  A call site with several callees does not change either: the edge the step traverses names
  one.
- A source or sink step's `callees` is every callee of its call site. The endpoint does not
  record which of them its model named.
- A step for a call crossed by its summary has no `callees`: nothing records which callee's
  summary carried the taint.

Which steps a flow has does not change. A flow has one source and one sink endpoint, but
its first or last step is not always theirs: the sink step is dropped when the step before
it is at the same instruction (`formatter.rs:2847`), and an endpoint with no call site gets
no step. That is accepted and documented.
