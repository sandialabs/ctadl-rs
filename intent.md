# Intent: write what a step says as structured step properties, in `ctadl query`

A step of a flow names its tainted vertex and callee only in message text. Write them as
structured step properties, `taintVertex` and `callee`, which `diff-query` then compares.

On a return step `callee` holds the function returned from. The message keeps naming the
function returned into, the caller (`ctadl-ascent/src/query_engine/formatter.rs:2920`).

A call or return step has one `callee`, the function whose edge the step traverses. A call
site with several callees does not change that.

A source or sink step has `callees` instead: every callee of its call site, by name. The
endpoint does not record which of them its model named. `diff-query` compares it as a set.

A step for a call crossed by its summary has neither: nothing records which callee's summary
carried the taint.

Open question, found while writing the spec for those changes:

- A flow has one source and one sink endpoint, but its first or last step is not always
  theirs: the sink step is dropped when the step before it is at the same instruction
  (`formatter.rs:2847`), and an endpoint with no call site gets no step. Accept it, or always
  write the endpoint's step?
