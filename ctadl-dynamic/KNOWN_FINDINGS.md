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
