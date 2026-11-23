# Intent: write what a step says as structured step properties, in `ctadl query`

A step of a flow names its tainted vertex and callee only in message text. Write them as
structured step properties, `taintVertex` and `callee`, which `diff-query` then compares.

Open questions, found while writing the spec for those changes:

- On a return step the message names the function returned into, the caller
  (`ctadl-ascent/src/query_engine/formatter.rs:2920`). Should `callee` hold that, or the
  function returned from?
- A call site with several callees keeps one, whichever sorts last by function id
  (`formatter.rs:2571`). Two indexes that number functions differently can then write
  different `callee` values for one flow, which `diff-query` would report. Write every callee,
  or pick one by name?
- Several source (or sink) endpoints at one instruction give one step, with the first
  endpoint's vertex only (`formatter.rs:2845`). Accept it, or write every vertex?
