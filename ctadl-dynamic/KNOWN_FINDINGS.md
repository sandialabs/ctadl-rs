# Known findings

This file records **analyzer findings the comparison harness has surfaced and that we are
intentionally leaving in place** — usually as exemplars of what the dynamic-vs-static
approach catches.

> If `cargo run -p ctadl-dynamic` prints a `known-gap (Fn)` for a case listed here, that is
> **expected** — it's a real CTADL behavior we're tracking, not a broken test. Do **not** "fix"
> it by flipping the case's manifest oracle; the oracle is correct and the gap is the point.

Each finding names the case that reproduces it, so the documentation stays executable: run the
harness and the gap re-appears.

**Allowlist linkage.** Each finding's case is allowlisted by a `"known_gap": "Fn"` field in its
`manifest.json` (see [README.md](README.md)). That is what turns a raw soundness gap into an
expected `known-gap` instead of a run-failing `NEW-GAP`. When a finding is fixed, the harness
reports the case as `resolved-known-gap` — at which point, remove the `known_gap` field **and**
the finding entry below.

---

## F1 — Static taint is dropped through indirect (function-pointer) calls

- **Status:** open; intentionally preserved as an exemplar. Allowlisted via
  `"known_gap": "F1"` in [`cases/05_funcptr_indirect/manifest.json`](cases/05_funcptr_indirect/manifest.json).
- **Severity:** soundness false-negative (CTADL misses a flow that genuinely occurs).
- **Reproduces with (all `known-gap (F1)`):** `05_funcptr_indirect` (local fp, initialized),
  `07_funcptr_separate_assign` (local fp, separate assignment), `08_funcptr_param` (fp passed as
  a parameter), `09_funcptr_in_struct` (fp in a struct field). `cargo run -p ctadl-dynamic`.
- **Also covered by:** the (ignored) unit test `taint_flows_through_indirect_call` in
  `ctadl-ascent/src/languages/tree_sitter/tests.rs`.

### What it is

CTADL's static taint analysis does not carry taint through a call made via a function pointer.
The tainted value really does reach the sink at runtime, but the static analysis reports no
flow.

### Why we're confident it's real (controlled comparison)

The corpus isolates the variable. Two cases call the same identity function `int id(int p)
{ return p; }`:

| Case                  | Call form                          | Static result |
|-----------------------|------------------------------------|---------------|
| `03_call_summary`     | `r = id(s);` (direct)              | flow ✓        |
| `05_funcptr_indirect` | `fp = id; r = fp(s);` (indirect)   | **no flow** ✗ |

The only difference is the indirection, so the dropped taint is attributable to indirect-call
handling, not to summaries or field/local propagation (which the other cases confirm work).

### Relationship to the C frontend work

The C frontend was extended to *detect* indirect calls (emit `funcptr-call`, push
`facts.indirect_call`). The gap is that the **taint query doesn't resolve** those calls to
carry data through them.

### Scope — broad (M5 finding)

The gap is **not** specific to one syntactic form. All four function-pointer forms drop taint:

| Case | Function pointer is… | Static |
|------|----------------------|--------|
| `05` | a local, initialized (`int (*fp)(int) = id;`) | no flow ✗ |
| `07` | a local, assigned separately (`fp = id;`)     | no flow ✗ |
| `08` | a parameter (`int apply(int (*f)(int), int x)`) | no flow ✗ |
| `09` | a struct field (`o.op = id; o.op(s)`)         | no flow ✗ |

The param (`08`) and struct-field (`09`) forms are progressively harder sub-cases (they need the
resolution to follow the function value through a formal parameter / through field sensitivity),
so a partial fix may resolve `05`/`07` first. The harness will show that split when it happens.

### Hypothesis for a future fix (revised)

The original guess — that the *declaration-initializer* form specifically fails to emit a
`func_ptr_assign_like` fact — is **refuted by case `07`**: the separate-assignment form
(`int (*fp)(int); fp = id;`) also drops the flow. So the problem is more general: the
indirect-call resolution in the taint query (`resolvent` / `func_ptr_assign_like` rules in
`ctadl-ascent/src/index_engine/mod.rs`) does not carry data through function-pointer calls in
any form. A focused investigation should compare the `assign_like` / `func_ptr_assign_like` /
`resolvent` relations between the working direct case (`03`) and the indirect cases (`05`/`07`).

### Confirmed by DFSan (runtime ground truth)

The DFSan dynamic runner now observes this case directly: `05_funcptr_indirect` reports
`static=none  dynamic=flow` — i.e. DFSan watched the `Test` label reach the sink through the
indirect call while CTADL's static analysis reported no flow. So this is no longer just
"the oracle says so": the gap is confirmed against observed runtime behavior, which is exactly
the kind of soundness violation this harness exists to find.

---

# Frontend ingestion gaps

The tree-sitter C frontend is incomplete: some valid C cannot be parsed/lowered to IR. These are
tracked separately from soundness gaps and allowlisted with `"known_frontend_gap": "<id>"` in a
case manifest (an un-allowlisted ingestion failure is a run-failing `FRONTEND-ERROR`). DFSan
compiles these with clang regardless, so each case already carries the runtime ground truth to
compare against the moment the frontend learns to ingest it (the harness will then report
`resolved-known-frontend-gap`).

## array_declarator — array declarations don't parse

- **Status:** open (tracked externally in a GitLab issue). Allowlisted as
  `"known_frontend_gap": "array_declarator"`.
- **Symptom:** a declaration like `int a[3];` fails with
  `ERR 78: Unsupported expression type: array_declarator` (the `array_declarator` arm in
  `walk_declaration` routes to `flatten_expr`, which has no case for it).
- **Reproduces with:** [`cases/18_array_subscript`](cases/18_array_subscript/) →
  `known-frontend-gap (array_declarator)`. DFSan observes the flow (`a[1] = source(); sink(a[1])`),
  so the expected result once it parses is `flow`.

## switch_statement — `switch` is now ingested (RESOLVED)

- **Status:** **resolved.** `switch`/`case`/`default` (and the `break`/`continue` they need)
  are now lowered by the tree-sitter frontend. The `known_frontend_gap` allowlist has been
  removed from `cases/25_switch_taint`.
- **Was:** a `switch` statement failed with `ERR 78: Unsupported expression type:
  switch_statement` — the statement walker in `mod.rs` had no `switch_statement` arm, so it fell
  through to `flatten_expr`, which had no case for it.
- **Fix:** `walk_switch` in
  [`ctadl-ascent/src/languages/tree_sitter/mod.rs`](../ctadl-ascent/src/languages/tree_sitter/mod.rs)
  lowers a `switch` path-insensitively, the same way `if` is lowered — the entry block branches
  non-deterministically to every `case`/`default` arm, arms fall through to the next unless a
  `break` redirects to the switch continuation. `break`/`continue` resolve against per-construct
  target stacks on `Context` (also enabling `break`/`continue` inside loops). No backend changes.
- **Covered by:** unit tests `switch_case_flows_to_return`, `switch_default_flows_to_return`,
  `switch_fallthrough_flows_to_return`, `break_exits_loop_flows_to_return`,
  `continue_in_loop_flows_to_return` (`ctadl-ascent/.../tree_sitter/tests.rs`); and DFSan cases
  `25_switch_taint`, `26_switch_merge_paths`, `27_switch_untaken_case` (precision-gap, expected),
  `28_switch_default_taint`, `29_switch_fallthrough`.
