# Known findings

This file records **analyzer findings the comparison harness has surfaced and that we are
intentionally leaving in place** — usually as exemplars of what the dynamic-vs-static
approach catches.

> If `cargo run -p ctadl-dynamic` prints a `SOUNDNESS-GAP` (or `precision-gap`) for a case
> listed here, that is **expected** — it's a real CTADL behavior we're tracking, not a broken
> test. Do **not** "fix" it by flipping the case's manifest oracle; the oracle is correct and
> the gap is the point.

Each finding names the case that reproduces it, so the documentation stays executable: run the
harness and the gap re-appears.

---

## F1 — Static taint is dropped through indirect (function-pointer) calls

- **Status:** open; intentionally preserved as an exemplar.
- **Severity:** soundness false-negative (CTADL misses a flow that genuinely occurs).
- **Reproduces with:** [`cases/05_funcptr_indirect`](cases/05_funcptr_indirect/) →
  `cargo run -p ctadl-dynamic` prints `05_funcptr_indirect … SOUNDNESS-GAP`.

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

### Hypothesis for a future fix (not yet investigated)

The function-pointer assignment — especially the declaration-initializer form
`int (*fp)(int) = id;` — may not produce the `func_ptr_assign_like` fact that the indirect-call
resolution rules in `ctadl-ascent/src/index_engine/mod.rs` depend on. A focused investigation
would compare the `assign_like` / `func_ptr_assign_like` / `resolvent` relations for the
working direct case (`03`) against the failing indirect case (`05`).

### Confirmed by DFSan (runtime ground truth)

The DFSan dynamic runner now observes this case directly: `05_funcptr_indirect` reports
`static=none  dynamic=flow` — i.e. DFSan watched the `Test` label reach the sink through the
indirect call while CTADL's static analysis reported no flow. So this is no longer just
"the oracle says so": the gap is confirmed against observed runtime behavior, which is exactly
the kind of soundness violation this harness exists to find.
