# Step properties and invocation comparison in `diff-query`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines what `ctadl query` writes on each step of a code flow and
what `ctadl diff-query` compares. It must stay consistent with `docs/spec.md` (F12, §4.5, §5)
and `docs/debugging.md` (*Comparing two query outputs*). It does not sequence the work.

## Background

- A step is built by `push_site_step` (`ctadl-ascent/src/query_engine/formatter.rs:2801`). It
  gets a location, `executionOrder`, one `kinds` string, and a message
  `<word> <variable><path> in <function>`, where the word is `source`, `sink`, `call` or
  `return`. No step has a `properties` bag.
- The function in the message is: for a source or sink step, the callee at the endpoint's call
  site, else the endpoint's own function (`endpoint_callee`, `formatter.rs:3286`); for a call
  or return edge, the function of the edge's destination vertex (`formatter.rs:2920`), which on
  a return edge is the caller; for an interior call-argument step, the callee at that call site
  (`formatter.rs:2959`).
- `diff-query` reads a step's `executionOrder`, `kinds` and `location` only
  (`ctadl-query-diff/src/parse.rs:144`), and compares `kinds` and `location`
  (`CanonStep`, `sarif-canon/src/types.rs:177`). Message text is dropped.
- `--endpoints-only` keeps each thread flow's first and last step whole
  (`CanonThreadFlow::Endpoints`, `sarif-canon/src/types.rs:186`; `endpoints_result`,
  `sarif-canon/src/canon.rs:1061`). So whatever a `CanonStep` holds is compared at the
  endpoints.
- `diff-query` reads nothing under `invocations`: `RawRun` holds only logical locations and
  results (`sarif-canon/src/types.rs:93`), and a test asserts that deleting `invocations`
  changes nothing (`ctadl-query-diff/src/parse.rs:743`).
- `ctadl query` writes one invocation per run (`build_invocation`, `formatter.rs:345`).
  `executionSuccessful` is "no error-level notification"; `exitCode` is 0 or 1 to match. An
  execution notification has `descriptor.id`, `level`, `message` (`id`, `arguments`, `text`)
  and an optional `properties` bag of JSON values (`ctadl-import/src/sarif.rs:84`). The index's
  notifications are appended to the query's own (`formatter.rs:842`).

## Terms

| Term | Meaning |
| --- | --- |
| **Step** | A `threadFlowLocation` of a `C0001.tainted-path` code flow. |
| **Step vertex** | The variable and access path a step's message names, as `<variable><path>`, the form `taintVertex` already has on `C0003`/`C0004` results. |
| **Step callee** | The function a step's message names after ` in `. |
| **Status** | An invocation's `executionSuccessful` and `exitCode`. |

## Requirements

**What `ctadl query` writes**

- R1. **Step properties.** Every step has a `properties` bag with `taintVertex`, the step
  vertex as a string, and `callee`, the step callee's name as a string, in every profile that
  writes code flows.
- R2. **Same content as the message.** A step's `taintVertex` and `callee` name the same
  vertex and function as its message text.
- R3. **Nothing else changes in the SARIF.** Step message text, `kinds`, locations,
  `executionOrder`, which steps are written, result properties and the invocation are as
  before.

**What `diff-query` compares**

- R4. **Steps by their properties.** A step is compared by its `kinds`, its location, its
  `taintVertex` and its `callee`. An absent property compares equal only to an absent one.
- R5. **Endpoints.** Under `--endpoints-only`, the first and last step of each thread flow are
  compared with their `taintVertex` and `callee`; interior steps' properties are not compared.
- R6. **Instruction ids stay out.** The instruction id in a step's
  `call-arg(<id>, <formal>)` vertex is not compared; the formal and the path are.
- R7. **Execution notifications.** The `toolExecutionNotifications` of every invocation of
  every run in a file are compared as one multiset. A notification is compared by everything
  it holds except `message.text`. Their order does not matter; equal notifications count
  separately.
- R8. **Status.** The status of every invocation of every run in a file is compared as one
  multiset. An absent `exitCode` compares equal only to an absent one.
- R9. **Flags.** `--endpoints-only` does not change how notifications or statuses are
  compared. `--limit` bounds the rows printed per side for each of results, notifications and
  statuses.
- R10. **Still not compared.** Message text anywhere, `toolConfigurationNotifications`, and
  the rest of the invocation (`commandLine`, `arguments`, times, `exitCodeDescription`,
  `ruleConfigurationOverrides`) are not compared.
- R11. **Malformed input.** A step `taintVertex` or `callee` that is not a string is an error
  (exit 2), as a misshapen result property is today.

**Report**

- R12. **First line and exit code.** The first line is `IDENTICAL`, and the exit code 0, only
  when results, notifications and statuses are all equal as multisets; otherwise `DIFFERENT`
  and 1.
- R13. **Sections.** After the results section, the report gives the count per file and the
  differing rows for notifications, then for statuses, in the form the results section uses
  (`[A n, B m]` and one line of JSON). `--json` carries the same content.
- R14. **A differing step is visible.** A printed result shows each step's `taintVertex` and
  `callee`, with the instruction id of a call-argument vertex printed as `_`.

**Tests and documents**

- R15. **Metamorphic tests.** On real `ctadl query` output in every profile: reordering
  notifications, changing a notification's message text, and renumbering instruction ids
  report no difference; changing a step's `taintVertex` or `callee`, a notification's level
  or a property, `executionSuccessful`, or `exitCode` is reported as exactly that. Under
  `--endpoints-only`, changing an interior step's property reports no difference and changing
  an endpoint's does.
- R16. **Proofs.** The Verus theorems of `sarif-canon` still hold with the step properties in
  the canonical step: the canonical results do not change under reordering or renumbering of
  logical locations, and their order is total.
- R17. **Documents.** `docs/debugging.md` (*Comparing two query outputs*), `docs/spec.md` (F12
  and the `diff-query` bullet of §5) and the `diff-query` help text
  (`ctadl-ascent/src/main.rs:87`) state what is now compared. None of them, and no code
  comment, cites a number from this document.

## Design

### D1. Writing step properties (R1–R3)

`push_site_step` takes the step's vertex (`FlowVariable`, `Path`) and callee (`FunctionId`)
instead of a finished message, and derives both the message and the properties from them.
One source for both keeps R2 true by construction. The four call sites
(`formatter.rs:2869`, `:2920`, `:2959`, `:2987`) pass what they format today. The callee is
rendered by `fname` (`formatter.rs:2785`), so a function with no name is `func#<id>` in both
places.

### D2. The canonical step (R4–R6, R16)

`RawStep` and `CanonStep` (`sarif-canon/src/types.rs:125`, `:177`) gain two optional atoms,
`vertex` and `callee`, and so do their Verus views. `canon_step`
(`sarif-canon/src/canon.rs:737`) copies them. `ord_step` and `cmp_step`
(`sarif-canon/src/order.rs:169`, `:1220`) compare them after `kinds` and `location`.
`step_permuted` and `step_renumbered` (`sarif-canon/src/theorems.rs:86`, `:151`) require them
equal. They are plain atoms, not bags, so no new sorting is proved.

Invariant: `endpoints_result` is not changed. It keeps whole steps, so R5 follows from the
step's new fields.

The parser (`ctadl-query-diff/src/parse.rs:144`) reads the two keys from the step's
`properties` and passes `taintVertex` through `without_call_site` (`parse.rs:311`), as it does
for the result property of the same name. `render.rs:86` prints both.

### D3. Notifications and statuses (R7–R10)

These do not go through `sarif-canon`. Each notification becomes one string: its JSON with
`message.text` removed and object keys sorted, arrays in the order written. Each invocation's
status becomes one string, `{"executionSuccessful":…,"exitCode":…}`. For each file the parser
returns the two lists, pooled over runs and invocations and sorted by string. The driver
(`ctadl-query-diff/src/driver.rs:73`) runs `multiset_diff` on each pair with string order, so
the proved merge is reused and no proof changes. The string is also the printed row.

Invariant: the canonical string is a function of the notification's content only. It has no
atom ids, so the report does not depend on interning order.

### D4. Report (R9, R12, R13)

`Report` (`ctadl-query-diff/src/report.rs:11`) gains counts and samples for notifications and
statuses. `identical()` is true when all three have no differing row. The text report adds,
after the results section:

```
notifications: A n, B m
more in A: N notifications, K not shown
  [A 1, B 0] {"descriptor":{"id":"CTADL0103"},"level":"warning",...}
invocations: A n, B m
more in B: N invocations, K not shown
  [A 0, B 1] {"executionSuccessful":false,"exitCode":1}
```

`write_samples` (`diff-report/src/lib.rs:87`) already takes the unit word. In JSON,
`results_differ` keeps its meaning and `notifications_differ` and `invocations_differ` are
added beside it.

### D5. Existing tests that state the old behavior

`drops_unread_fields` (`ctadl-query-diff/src/parse.rs:743`) asserts that invocations are
ignored, and `step_key` (`ctadl-ascent/tests/integration/diff_query.rs:153`) already treats
everything on a step but `executionOrder` and the message as compared. The first changes to
match R7, R8 and R10; the second needs no change.

## Concerns

- C1. **`callee` on a return step.** The message of a return step names the function returned
  into (the caller), which the step's logical location already gives. Options: (a) `callee`
  holds what the message names, on every step; (b) on a return step `callee` holds the
  function returned from, which is new information but breaks R2 for that step. The spec
  assumes (a).
- C2. **Call sites with several callees.** The callee at a call site comes from a map that
  keeps one callee per site (`formatter.rs:2571`); for a virtual or indirect call with several
  targets it is whichever sorts last by function id. Two indexes that number functions
  differently can then write different `callee` values for the same flow, and `diff-query`
  would report a difference that is not one. The result-level `sourceCallee` is left out of
  the comparison today, and `sourceFunctions` compared, for this kind of reason. Options: (a)
  write `callees`, the sorted names of every callee at the site, and compare it as a bag; (b)
  pick the callee by name, not id; (c) accept it. The spec assumes a single `callee` as the
  intent says, and does not yet meet "renumbering ids reports no difference" at such sites.
- C3. **Several endpoints at one instruction.** A path can carry several source (or sink)
  endpoints. Their steps are written one after another, and a step at the same instruction as
  the previous one is dropped (`formatter.rs:2845`). Two tainted arguments of one source call
  thus give one step, with the first endpoint's vertex only. So "compared down to the
  argument" holds for one argument per call. Options: (a) accept it; (b) make the step's
  vertex a list of every endpoint vertex merged into the step. The spec assumes (a).
- C4. **`message.arguments` of a notification.** R7 leaves out only `message.text`, so
  `message.id` and `message.arguments` are compared. Arguments are the values the text is
  rendered from. Some index notifications put an example in them, such as one vertex of the
  first unresolved indirect call in a function (`ctadl-ascent/src/cli/mod.rs:306`), which may
  depend on iteration order or hold a per-index id. Options: (a) compare arguments, and fix
  any that turn out unstable; (b) treat arguments as message text and leave them out, keeping
  `message.id` and `properties`. The spec assumes (a).
- C5. **Old SARIF files.** A file written before this change has no step properties, so
  against a new file every `C0001` result differs. `docs/spec.md` puts backward compatibility
  out of scope, and the spec assumes that is acceptable here.
