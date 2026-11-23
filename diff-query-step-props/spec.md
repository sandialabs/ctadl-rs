# Step properties and invocation comparison in `diff-query`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines what `ctadl diff-query` compares. It must stay consistent
with `docs/spec.md` (F12, §5) and `docs/debugging.md` (*Comparing two query outputs*). It does
not sequence the work. Writing the step properties in `ctadl query` is `intent2.md`, a later
change, and is not specified here.

## Background

- A step of a code flow gets a location, `executionOrder`, one `kinds` string, and a message
  `<word> <variable><path> in <function>` (`push_site_step`,
  `ctadl-ascent/src/query_engine/formatter.rs:2801`). No step has a `properties` bag.
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
| **Step** | A `threadFlowLocation` of a code flow. |
| **Step properties** | `taintVertex` and `callee` in a step's `properties` bag, each a string. `taintVertex` is `<variable><path>`, the form the result property of that name has. |
| **Status** | An invocation's `executionSuccessful` and `exitCode`. |

## Requirements

**What `diff-query` compares**

- R1. **Steps by their properties.** A step is compared by its `kinds`, its location, its
  `taintVertex` and its `callee`. An absent property compares equal only to an absent one, so
  files written before `ctadl query` writes them compare as they do today.
- R2. **Endpoints.** Under `--endpoints-only`, the first and last step of each thread flow are
  compared with their `taintVertex` and `callee`; interior steps' properties are not compared.
- R3. **Instruction ids stay out.** The instruction id in a step's
  `call-arg(<id>, <formal>)` vertex is not compared; the formal and the path are.
- R4. **Execution notifications.** The `toolExecutionNotifications` of every invocation of
  every run in a file are compared as one multiset. A notification is compared by everything
  it holds except `message.text` and `message.arguments`, the values the text is rendered
  from; `message.id` is compared. Their order does not matter; equal notifications count
  separately.
- R5. **Status.** The status of every invocation of every run in a file is compared as one
  multiset. An absent `exitCode` compares equal only to an absent one.
- R6. **Flags.** `--endpoints-only` does not change how notifications or statuses are
  compared. `--limit` bounds the rows printed per side for each of results, notifications and
  statuses.
- R7. **Still not compared.** Message text anywhere, a notification's `message.arguments`,
  `toolConfigurationNotifications`, and
  the rest of the invocation (`commandLine`, `arguments`, times, `exitCodeDescription`,
  `ruleConfigurationOverrides`) are not compared.
- R8. **Malformed input.** A step `taintVertex` or `callee` that is not a string is an error
  (exit 2), as a misshapen result property is today.
- R9. **`ctadl query` does not change.** Its SARIF is byte for byte what it was.

**Report**

- R10. **First line and exit code.** The first line is `IDENTICAL`, and the exit code 0, only
  when results, notifications and statuses are all equal as multisets; otherwise `DIFFERENT`
  and 1.
- R11. **Sections.** After the results section, the report gives the count per file and the
  differing rows for notifications, then for statuses, in the form the results section uses
  (`[A n, B m]` and one line of JSON). `--json` carries the same content.
- R12. **A differing step is visible.** A printed result shows each step's `taintVertex` and
  `callee` when present, with the instruction id of a call-argument vertex printed as `_`.

**Tests and documents**

- R13. **Metamorphic tests.** On real `ctadl query` output in every profile: reordering
  notifications and changing a notification's message text report no difference; changing a
  notification's level or a property, `executionSuccessful`, or `exitCode` is reported as
  exactly that. With step properties added to that output by the test: renumbering
  instruction ids reports no difference; changing a step's `taintVertex` or `callee` is
  reported; under `--endpoints-only`, changing an interior step's property reports no
  difference and changing an endpoint's does.
- R14. **Proofs.** The Verus theorems of `sarif-canon` still hold with the step properties in
  the canonical step: the canonical results do not change under reordering or renumbering of
  logical locations, and their order is total.
- R15. **Documents.** `docs/debugging.md` (*Comparing two query outputs*), `docs/spec.md` (F12
  and the `diff-query` bullet of §5) and the `diff-query` help text
  (`ctadl-ascent/src/main.rs:87`) state what is now compared, and that `ctadl query` does not
  write the step properties yet. None of them, and no code comment, cites a number from this
  document.

## Design

### D1. The canonical step (R1–R3, R14)

`RawStep` and `CanonStep` (`sarif-canon/src/types.rs:125`, `:177`) gain two optional atoms,
`vertex` and `callee`, and so do their Verus views. `canon_step`
(`sarif-canon/src/canon.rs:737`) copies them. `ord_step` and `cmp_step`
(`sarif-canon/src/order.rs:169`, `:1220`) compare them after `kinds` and `location`.
`step_permuted` and `step_renumbered` (`sarif-canon/src/theorems.rs:86`, `:151`) require them
equal. They are plain atoms, not bags, so no new sorting is proved.

Each property is a single string. If `intent2.md` settles on a list for either (several
callees at one call site, several endpoints at one instruction), the reader changes with it.

Invariant: `endpoints_result` is not changed. It keeps whole steps, so R2 follows from the
step's new fields.

The parser (`ctadl-query-diff/src/parse.rs:144`) reads the two keys from the step's
`properties` and passes `taintVertex` through `without_call_site` (`parse.rs:311`), as it does
for the result property of the same name. `render.rs:86` prints both, leaving out an absent
one.

### D2. Notifications and statuses (R4–R7)

These do not go through `sarif-canon`. Each notification becomes one string: its JSON with
`message.text` and `message.arguments` removed and object keys sorted, arrays in the order written. Each invocation's
status becomes one string, `{"executionSuccessful":…,"exitCode":…}`. For each file the parser
returns the two lists, pooled over runs and invocations and sorted by string. The driver
(`ctadl-query-diff/src/driver.rs:73`) runs `multiset_diff` on each pair with string order, so
the proved merge is reused and no proof changes. The string is also the printed row.

Invariant: the canonical string is a function of the notification's content only. It has no
atom ids, so the report does not depend on interning order.

### D3. Report (R6, R10, R11)

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

### D4. Existing tests that state the old behavior

`drops_unread_fields` (`ctadl-query-diff/src/parse.rs:743`) asserts that invocations are
ignored; it changes to match R4, R5 and R7. `step_key`
(`ctadl-ascent/tests/integration/diff_query.rs:153`) already treats everything on a step but
`executionOrder` and the message as compared, and needs no change.

## Concerns

None open.
