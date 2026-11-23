# Intent: compare what a flow's steps and a run's notifications say, in `diff-query`

`ctadl diff-query` compares a `C0001.tainted-path` result's code flow step by step, but it
reads only each step's `kinds`, its location, and `executionOrder`, which it checks increases
and then drops (`ctadl-query-diff/src/parse.rs:144`, `sarif-canon/src/types.rs:125`). A step's
location is the call site, and its logical location is the *calling* function. What the step
says about the flow — which vertex is tainted, at which access path, and which function is
called — is only in its message text:

```
kinds: ["call"]   logical location: ToggleableKt;->launchTrampolineAction…
message: call call-arg(630121, 0).<…$launchTrampolineAction$1;->$actionIntent:Landroid/content/Intent;>.#value
         in …ActionTrampolineKt$launchTrampolineAction$1;-><init>(…)
```

(org-wikipedia, agent profile.) `diff-query` ignores message text on purpose: a wording change
does not change the structure of a finding. But because this step's vertex, access path and
callee exist only in that text, a formatter change that attributes a step to the wrong vertex
or the wrong callee — same line, same kinds — passes `diff-query` as IDENTICAL.

Result-level messages do not have this problem: every fact in a `C0001`, `C0003`, `C0004` or
`C0007` message is also in a field `diff-query` compares (`taintLabels`, `taintVertex`,
`taintedFormals`, the logical location).

## Goal

Every fact a step's message carries is also in a structured step property that `ctadl query`
writes in every profile with code flows (human, agent, debug), and that `diff-query` reads and
compares. Message text stays out of the comparison.

The step messages are built in `ctadl-ascent/src/query_engine/formatter.rs`: `source …`
(`:2881`), `call …` (`:2978`) and `sink …` (`:3001`). Each is `<kind> <vertex><path> in
<callee>`.

## Scope

- In each step's `location` (or the step's own property bag), write the tainted vertex, with
  its access path, and the function the step is in or calls — the same values the message
  prints. Name them so they line up with the result properties (`taintVertex`, and a callee
  property in the spirit of `sourceCallee`/`sinkCallee`).
- `diff-query` reads them, rewriting the instruction id of a `call-arg(<id>, <formal>)` vertex
  as it already does for `taintVertex`, so renumbered instructions still compare equal.
- `sarif-canon`'s canonical step and its Verus proofs cover the new fields, so reordering and
  renumbering still yield the same canonical results.
- `docs/debugging.md` (what is and is not compared) and the metamorphic tests in
  `ctadl-ascent/tests/integration/diff_query.rs` are updated: a changed step vertex or callee
  is reported; changed step message text is not.

## Also compare `invocation.toolExecutionNotifications`

`diff-query` skips the invocation entirely, so it does not see what a run says about itself.
`toolExecutionNotifications` is where the query keeps the spec's promises that a vacuous or
lossy run is visible (`docs/spec.md` F11 and N3), and where `invocation.executionSuccessful`
comes from: a run is unsuccessful exactly when one of its notifications is at `error` level
(`build_invocation`, `ctadl-ascent/src/query_engine/formatter.rs:345`). It holds:

- what the query writes: `CTADL0100.endpoint-match-summary` (ports declared and unmatched,
  sources and sinks matched, as `properties`), `CTADL0101.path-generation-disabled`,
  `CTADL0102.no-paths-found` and `CTADL0103.path-dropped-no-location`, the last counting paths
  the formatter found but could not locate (`PathStats`);
- what the index recorded and the query passes on (`QueryDiagnostics::index_notifications`):
  `CTADL0104`–`CTADL0111`, such as unresolved indirect calls and calls to functions with no
  body or model, each with counts in `properties`.

A formatter change that drops paths, or loses a notification, changes nothing `diff-query`
reads. Compare these notifications as a multiset, each by its descriptor id, level,
`message.id`, `properties` and locations. As for results, message text is not compared.

## Not in scope

- Comparing message text.
- The other fields `diff-query` does not read: `sourceCallee`/`sinkCallee` on results (each
  is the first of `sourceFunctions`/`sinkFunctions`, which are compared),
  `toolConfigurationNotifications`, `ruleConfigurationOverrides`, `executionSuccessful` and
  `exitCode`.
