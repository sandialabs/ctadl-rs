# ctadl-dynamic

A differential testing harness that compares **CTADL's static taint analysis** against
**dynamic taint observed by LLVM DataFlowSanitizer (DFSan)** on small C programs. DFSan
runs the program and watches the taint actually happen, so it's the ground truth: a flow
DFSan observes that CTADL misses is a **soundness gap** (a real analyzer false-negative).

## Run it

```bash
# Curated corpus (cases/ + manifests + allowlists). Exit 0 unless something unexpected.
cargo run -p ctadl-dynamic            # human table + summary
cargo run -p ctadl-dynamic -- --json  # machine-readable JSON (table -> stderr)
```

Requires `clang` with the dataflow-sanitizer runtime (`libclang_rt.dfsan-*`) and `setarch`
(DFSan binaries are run under `setarch -R` to disable ASLR; otherwise they intermittently
abort with "out of application range").

For each case it runs CTADL statically (`ctadl_ascent::taint_compare::analyze_c_flows`) and
DFSan dynamically (compile `prog.c` + the instrumented shim with `-fsanitize=dataflow`, run,
read the observation), then classifies the two.

### Other modes (automated discovery — M7)

```bash
# Scan bare *.c files with DFSan as the AUTO-ORACLE (no manifests). Classifies each as
# agree / soundness-disagree / precision-disagree / frontend-error / dyn-error.
cargo run -p ctadl-dynamic -- scan <dir> [--json]

# Generate N reproducible C programs (source -> random taint transforms -> sink).
cargo run -p ctadl-dynamic -- gen <outdir> --count N [--seed S]

# Interestingness predicate for minimization (cvise): exit 0 iff <file> still shows <kind>.
cargo run -p ctadl-dynamic -- check <file> --interesting <kind>   # kind = soundness-disagree | ...

# Discovery loop core:
cargo run -p ctadl-dynamic -- gen /tmp/g --count 200 --seed 1
cargo run -p ctadl-dynamic -- scan /tmp/g --json | jq '.results[] | select(.class=="soundness-disagree")'
```

`scan`/`gen`/`check` are the M7 automated-generation modes; `scan` over `cases/` reproduces the
curated classifications (it's corpus-mode minus the oracle).

## Layout

```
cases/NN_name/
  prog.c          test program: logic + `int source();` / `void sink(int);` prototypes
  manifest.json   the oracle + metadata (see schema below)
markers.json      source/sink model CTADL reads (static side)
shim/static_markers.c   inert source()/sink() bodies, concatenated for the static run
shim/dfsan_shim.c       instrumented source()/sink() bodies, linked for the dynamic run
```

`source`/`sink` have no bodies in `prog.c`; each side supplies its own (CTADL's model only
matches *defined* functions, and DFSan needs instrumented ones). One `prog.c` serves both.

## manifest.json schema

```json
{
  "name": "funcptr_indirect",
  "description": "...",
  "label": "Test",          // taint label; matches markers.json (default "Test")
  "expect_flow": true,      // ORACLE: does taint truly flow source->sink? (hand-authored)
  "known_gap": "Fn",            // OPTIONAL: known, preserved SOUNDNESS gap (none open today)
  "known_frontend_gap": "<construct>",       // OPTIONAL: known frontend INGESTION gap (none open today)
  "precision_gap": "constant-condition"      // OPTIONAL: label for an EXPECTED precision gap
}
```

> Note: `known_gap` and `known_frontend_gap` are shown for completeness — as of 2026-06-29 **no
> case sets either** (F1 and the `array_declarator`/`switch` frontend gaps are all fixed). The
> only label in active use is `precision_gap` on the three constant-condition cases.

There are **two allowlists**, both naming an entry in [`KNOWN_FINDINGS.md`](KNOWN_FINDINGS.md):

- `known_gap` — a known **soundness** gap (CTADL misses a flow DFSan observes). Allowlisted ⇒
  `known-gap`; un-allowlisted ⇒ `NEW-GAP` (fails the run).
- `known_frontend_gap` — a known **ingestion** gap: the tree-sitter C frontend can't parse the
  program (the frontend is incomplete). Allowlisted ⇒ `known-frontend-gap`; un-allowlisted ⇒
  `FRONTEND-ERROR` (fails the run). Finding "C source the frontend can't ingest" is a goal here,
  so an unexpected ingestion failure is surfaced, not swallowed.

There is also a purely **descriptive** label (not an allowlist — precision gaps never fail the
run):

- `precision_gap` — names the reason a case is *expected* to over-report, surfaced as
  `precision-gap (reason)` in the table/JSON. The only class today is **`constant-condition`**:
  CTADL is path-insensitive and does not fold a compile-time-constant branch guard, so it keeps a
  flow through a branch runtime never takes (dead `if(0)`, always-taken kill `if(1)`, untaken
  `switch` case). This is sound over-approximation, not a bug — the label just makes that explicit.

## Per-case status

| status                          | meaning                                                          |
|---------------------------------|------------------------------------------------------------------|
| `OK`                            | static and dynamic agree                                         |
| `known-gap (Fn)`                | soundness gap, allowlisted via `known_gap` — expected            |
| `NEW-GAP`                       | soundness gap, **not** allowlisted — a regression / new finding  |
| `resolved-known-gap`            | allowlisted soundness gap that stopped failing — drop the entry  |
| `precision-gap [(reason)]`      | CTADL reported a flow runtime never produced (imprecision); optional `reason` from `precision_gap`, e.g. `constant-condition` |
| `known-frontend-gap (id)`       | frontend can't ingest it, allowlisted via `known_frontend_gap`   |
| `FRONTEND-ERROR`                | frontend can't ingest it, **not** allowlisted — a new finding    |
| `resolved-known-frontend-gap`   | frontend now ingests a previously-failing case — re-triage       |
| `dyn-error`                     | DFSan couldn't compile/run the case                              |

An `oracle mismatch` (DFSan disagrees with the hand-authored `expect_flow`) is reported
separately and flags a buggy test or a harness problem.

## Exit code (for automated / AI loops)

`--json` emits a report with a top-level `ok` boolean and a `summary` of counts, plus a
`cases` array. The process exit code is:

- **0** — nothing unexpected. (`resolved-known-gap` / `resolved-known-frontend-gap` are
  surfaced but do not by themselves fail — they're the "your change fixed a known gap" signals.)
- **1** — a `new-gap`, an un-allowlisted `FRONTEND-ERROR`, or an oracle mismatch occurred.
- **2** — the harness itself errored (e.g. couldn't read a case).

So a loop can gate on the exit code and branch on the JSON: watch `summary.new_gap` and
`summary.frontend_error` for regressions/new findings, and `summary.resolved_known_gap` /
`summary.resolved_known_frontend_gap` to detect that a targeted fix (soundness or parser) landed.

```bash
cargo run -q -p ctadl-dynamic -- --json 2>/dev/null > report.json
# exit 0 = clean; jq '.summary' report.json ; jq '.cases[] | select(.status=="new-gap")' report.json
```
