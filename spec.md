# `ctadl diff-sarif`: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This refines it into requirements and a design, consistent with
[docs/spec.md](docs/spec.md) (F12, §4.5, §5), which it extends. It does not sequence the work.

## Background

`ctadl query` writes one SARIF 2.1.0 document with one run, built as `serde_sarif` values by
the formatter (`ctadl-ascent/src/query_engine/formatter.rs`, "F" below) and assembled by
`ctadl-import/src/sarif.rs:169`. What the comparison has to see through:

- **Per-run references.** Every location names its function by `logicalLocations[].index`
  into `run.logicalLocations` (F:2324, F:3380, F:3485), a table filled in first-use order
  keyed by `fullyQualifiedName` (F:2309-2322). Thread-flow steps clone those locations, so
  they carry the index too (F:2856-2864). No `run.artifacts`, `artifactLocation.index`,
  `ruleIndex`, `parentIndex` or `threadFlowLocation.index` is emitted.
- **Per-run numbers in content.** `executionOrder` is 1..n per flow (F:2861).
  `taintVertex` and `taintVertices` strings render call-argument vertices as
  `call-arg(<insn id>, <formal>)` (`ctadl-index/src/facts.rs:1431`; F:3043, F:3328). The
  debug profile adds `fileSpanId`, `funcId`, `insnId` (F:3080-3091). Messages embed span,
  function and import ids (`span {n}`, `func#N`, `import#N`).
- **Order.** `results` is ordered by rule, then by numeric span, function or node ids
  (F:3027-3276). It is deterministic for one index, not a content order.
- **Machine and run data.** Times, command line, `parquet_dir(s)`, `originalUriBaseIds`
  values (F:2104-2133), absolute `file:` URIs for files outside an import's root
  (F:981-1007), and tool version.
- **Shapes by rule** (docs/spec.md §4.5). C0001 `fail` results carry one code flow with one
  thread flow and one primary location (F:3253-3266); the C0001 status result has no
  location (F:915). C0002 has one instruction location. C0003/C0004 have a logical location,
  plus a physical one at a call site or one per declaration of a global (F:3365-3419). C0007
  has a logical location only (F:3455-3497). `--sarif-profile` selects which rules appear
  (F:1043).
- **Witness choices.** A C0001 result's primary location is the first span, in
  (import id, span id) order, where its source/sink pair was found (F:2666-2671), and its
  steps are one shortest path, ties broken in a content order (docs/debugging.md:163-171).
  Two indexes that agree on every source/sink pair can still differ here: in span numbering,
  or in an edge elsewhere that changes which path is shortest.

`ctadl diff-index` (`ctadl-index-diff`, wired at `ctadl-ascent/src/main.rs:85` and
`ctadl-ascent/src/main.rs:933`) is the model: canonicalize rows, sort, compare with
`multiset_diff` (`ctadl-index-diff/src/table.rs:485-509`), print bounded samples, exit 0/1/2.
`multiset_diff` (`multiset-diff/src/lib.rs:287`) is proved to report exactly the values whose
counts differ, provided `cmp` computes a total order and both inputs are sorted by it
(`multiset-diff/src/proofs.rs:604`, `multiset-diff/src/proofs.rs:633`). Today neither
precondition is proved at any call site: callers pass a derived `Ord` and sort with
`sort_unstable`, and sortedness is only a `debug_assert` (`multiset-diff/src/lib.rs:421`).

## Terms

| Term | Meaning |
| --- | --- |
| **Raw run** | One SARIF run read into the typed subset of SARIF that ctadl emits, with references still numbers and every field the comparison drops already gone. |
| **Canonical result** | The value a result is compared by: references replaced by what they name, per-run numbers gone, unordered collections sorted. Two results are the same finding exactly when their canonical results are equal. |
| **Atom** | A string the comparison treats as opaque (a URI, a function name, a label), interned to a `u32` shared by both inputs. |
| **Endpoints** | A thread flow's first and last steps. |

## Requirements

### Command

- R1. **Command.** `ctadl diff-sarif A B` compares the results of two SARIF files by
  content and prints `IDENTICAL` or the results that occur more often in A and in B, each
  with its count on both sides.
- R2. **Exit codes.** 0 when identical, 1 when different, 2 on error (unreadable file,
  malformed SARIF, a reference out of range, a per-run reference the design does not
  resolve), as `diff-index` does.
- R3. **Flags.** `--limit N` bounds the results printed per side (default 20); counts are
  always complete. `--json` writes one JSON document instead of text. `--endpoints-only`
  is R11.
- R4. **Deterministic output.** The same pair of files gives byte-identical output, and
  `diff-sarif B A` is `diff-sarif A B` with the sides swapped.

### What is compared

- R5. **Results only.** The results of every run in a file are compared as one multiset.
  Run metadata, tool, rules, invocations, notifications and the document's `properties`
  are not compared.
- R6. **Message text is ignored**, on results and on thread-flow steps.
- R7. **References by content.** A result is compared by what its references name, not by
  their numbers: renumbering `run.logicalLocations` (or `run.artifacts`), with every
  reference updated to match, reports nothing.
- R8. **No per-run numbers.** Canonical results contain no table index, `executionOrder`,
  instruction, function or span id, and no `uriBaseId` value.
- R9. **Order ignored where SARIF gives it none:** results, a result's locations, its code
  flows, a code flow's thread flows, a location's logical locations, and the set- and
  map-valued properties ctadl writes. Shuffling any of them reports nothing.
- R10. **Order kept where it means something:** the steps of a thread flow. Swapping two
  steps that differ is reported.
- R11. **Endpoints only.** With `--endpoints-only`, a C0001 result is compared by its rule,
  kind, level, labels, source and sink functions, and the endpoints of its thread flows; its
  interior steps, primary location and `taintVertices` are ignored. Results with equal
  endpoints still count separately, so two such flows on one side and one on the other is a
  difference. Other rules are unaffected.
- R12. **Duplicates count.** Two equal canonical results in one file count twice; deleting
  one is reported as a count difference.

### Correctness

- R13. **Proved canonical core.** The canonical core (D3) is verified with Verus: for
  inputs of any size, the properties in D4 hold of the code that runs.
- R14. **Proved comparison, end to end.** The order on canonical results is proved a total
  order under which `Equal` means equal, and the core proves its output sorted by it, so
  `multiset_diff`'s theorems apply with their preconditions discharged.
- R15. **Metamorphic tests** cover what Verus does not (the parser and atom rewriting, D2),
  on SARIF from real `ctadl query` runs in every profile: shuffled arrays, a renumbered
  `logicalLocations` table, renumbered `call-arg` instruction ids and debug ids, changed
  message text, and A against itself report nothing; a deleted result, a duplicated
  result, two swapped steps and a changed line are reported as exactly that.
- R16. **Verus check.** The new verified crate is in the `verus` flake check
  (`flake.nix:292-306`) and runs nightly with the others.

### Unchanged and documented

- R17. **`ctadl query`'s SARIF does not change**, and neither does `ctadl diff-index`.
- R18. **Docs.** docs/spec.md F12 names `ctadl diff-sarif`, and §5 lists its verified core
  with `multiset-diff` and `name-by-facts`; docs/debugging.md documents its use and output
  beside `diff-index` (docs/debugging.md:183-215); `nightly/README.md:31-36` and the comments
  at `flake.nix:292` and `Cargo.toml:110` name the new crate.

## Design

### D1. Shape

Two crates and a subcommand.

- **`sarif-canon`** (new, verified like `multiset-diff`: executable code inside `verus!`,
  `vstd` pinned to the same version, no serde). It defines the raw run and canonical result
  types over an atom type, and the functions from one to the other (D3). It knows nothing
  of JSON.
- **`ctadl-sarif-diff`** (new, unverified, the sibling of `ctadl-index-diff`). It parses
  SARIF into raw runs (D2), calls `sarif-canon`, sorts nothing itself, runs
  `multiset_diff`, and renders the report. It reuses `ctadl-index-diff`'s report layout
  and bounded-sample buffer, moved or shared rather than copied.
- **`ctadl diff-sarif`** in `ctadl-ascent/src/main.rs`, beside `DiffIndex`, with the same
  exit-code handling as `ctadl-ascent/src/main.rs:933-960`.

Data flow: JSON → (D2, unverified) raw runs over atoms → (D3, verified) sorted
`Vec<CanonResult>` per file → `multiset_diff` (verified) → report (unverified).

### D2. Parser (unverified)

Reads each file with `serde_json` into a small set of `Deserialize` structs that name only
the fields the table in D4 keeps or resolves, plus the reference fields it must reject.
`serde` ignores other fields. It:

- interns every kept string into one atom table shared by A and B, so equal strings get
  equal atoms in both files (the one assumption the proofs rest on: interning is
  injective);
- rewrites `call-arg(<n>, <f>)` to `call-arg(_, <f>)` inside vertex strings before
  interning, and drops the debug ids. The call site is already named by the result's
  location; the formal and access path stay, since they tell apart two endpoints on one
  variable (F:3322-3328);
- rejects (exit 2) a reference D4 does not resolve: `ruleIndex` without `ruleId`,
  `logicalLocation.parentIndex`, `threadFlowLocation.index`, `result.rule.index`;
- keeps unknown result properties as canonical JSON text (keys sorted, arrays in order), one
  atom each.

The parser holds no ordering, resolution or dropping logic beyond not reading a field, so
what it can get wrong is limited to the field list and the `call-arg` rewrite. The
metamorphic tests (R15) target exactly that.

### D3. Canonical core (verified)

Generic over an atom type `A: Copy` and a `cmp` that computes a total order on atoms in
which `Equal` means `==` (`u32::cmp` in practice), stated with `multiset-diff`'s
`total_order` and `computes` (`multiset-diff/src/lib.rs:55-67`).

- **Raw types** mirror the SARIF the parser reads: `RawRun { logical: Vec<RawLogical>,
  artifacts: Vec<RawArtifactLoc>, results: Vec<RawResult> }`, with references as `usize`.
- **Canonical types** have no index, id or order-number field at all, so R8 holds by
  construction. Unordered collections are `Vec`s the core keeps sorted.
- **Primitives**, each verified once and used at every element:
  - `resolve(table, i) -> Result<T, OutOfRange>`;
  - `canon_bag(xs, f) -> Vec<C>`: maps `f` over `xs` and sorts with a verified merge sort,
    keeping duplicates (not `std`'s sort: its output is not specified to Verus);
  - `canon_seq(xs, f) -> Vec<C>`: maps `f` over `xs` in order;
  - `endpoints(steps) -> Result<(C, C), Empty>`;
  - `check_increasing(orders) -> Result<(), NotIncreasing>` for `executionOrder`;
  - `cmp_canon`: the order on each canonical type, lexicographic over fields and over
    sequences, built from `cmp` on atoms; `name-by-facts`'s order lemmas
    (`name-by-facts/src/order.rs`) are the starting point.
- **`canon_run(run, endpoints_only) -> Result<Vec<CanonResult>, Error>`** applies the
  primitives element by element as the table in D4 says.

### D4. Each SARIF element: canonical form and Verus property

Properties named here:

- **P-ref (renumbering).** For any table `t`, any permutation `π` of its positions, and any
  index `i`: `resolve(t∘π⁻¹, π(i)) == resolve(t, i)`; and `resolve` errs exactly when `i` is
  out of range. Lifted to the run: renumbering a table and every reference into it leaves
  `canon_run` unchanged.
- **P-bag (permutation and count).** `canon_bag(xs, f)` is sorted by `cmp_canon`, and is a
  permutation of `xs.map(f)`: each canonical value occurs in the output as often as `f`
  produces it, none dropped or merged. Hence for any permutation `ys` of `xs`,
  `canon_bag(ys, f) == canon_bag(xs, f)`.
- **P-seq (order kept).** `canon_seq(xs, f)` has `xs`'s length and its `k`th element is
  `f(xs[k])`. Hence swapping steps `j` and `k` with `f(xs[j]) != f(xs[k])` changes the
  output.
- **P-end (endpoints).** `endpoints(xs)` is `(f(xs[0]), f(xs[last]))` and errs exactly on an
  empty flow. Hence it is unchanged by any change to `xs[1..last]`.
- **P-ord (order).** `cmp_canon` computes a total order on each canonical type, and returns
  `Equal` exactly when its arguments are equal.
- **P-drop (by construction).** A field the raw type does not have cannot affect the
  output. This is a fact about the types, not a Verus theorem; R15 checks that the parser
  drops the right fields.

| SARIF element | Canonical form | Verus property |
| --- | --- | --- |
| `sarifLog.runs` | Results of all runs pooled; each run's references resolved against its own tables. | P-bag (pooling is a bag) |
| `run.results` | `canon_bag` of canonical results: the multiset compared. | P-bag; output sorted, discharging `multiset_diff`'s `sorted` precondition (R14) |
| `run.logicalLocations` | Not compared itself; the table references resolve through. | P-ref |
| `run.artifacts` (not emitted today) | Not compared itself; resolves `artifactLocation.index`. | P-ref |
| `run.tool`, `invocations`, `originalUriBaseIds`, `properties`, notifications | Dropped. | P-drop |
| `result.ruleId` | Atom. `ruleIndex` dropped when `ruleId` is present, rejected otherwise. | P-drop |
| `result.kind`, `result.level` | Atoms, with SARIF's defaults filled in when absent (`fail`, `warning`), so absent and default compare equal. | none (total function on atoms) |
| `result.message` | Dropped (R6). | P-drop |
| `result.locations` | `canon_bag` of canonical locations. With `--endpoints-only`, dropped for C0001. | P-bag |
| `result.codeFlows` | `canon_bag` of canonical code flows. | P-bag |
| `codeFlow.threadFlows` | `canon_bag` of canonical thread flows. | P-bag |
| `threadFlow.locations` | `canon_seq` of canonical steps; with `--endpoints-only`, `endpoints` of them instead. | P-seq; P-end |
| `threadFlowLocation.executionOrder` | Checked strictly increasing along the array (exit 2 if not), then dropped: array order is the order. | `check_increasing` returns `Ok` exactly when increasing; P-drop |
| `threadFlowLocation.kinds` | `canon_bag` of atoms (`taint`, `call`, `return`). | P-bag |
| `threadFlowLocation.location` | Canonical location; its `message` dropped. | as `location` |
| `location` | Record of optional canonical physical location and `canon_bag` of logical locations. `location.message` dropped. | P-bag |
| `logicalLocations[]` reference (`index`) | Resolved to the entry's `fullyQualifiedName` (else `name`) atom. `kind` and `name` are dropped: the table entry for a name is whichever use came first (F:2309-2322), so they can change under reordering. | P-ref |
| Inline `logicalLocation` (no index) | Its `fullyQualifiedName` (else `name`) atom. | none |
| `physicalLocation.artifactLocation` | `(uriBaseId name, uri)` atoms; the base's value is not consulted. With `index`, resolved through `run.artifacts`; with both `uri` and `index`, they must agree (exit 2 otherwise). | P-ref |
| `physicalLocation.region` | `startLine`, `startColumn`, `endLine`, `endColumn`, `byteOffset`, `byteLength`, each optional, as numbers. `region.message`/`snippet` dropped. | none |
| `physicalLocation.address` | `absoluteAddress`, `relativeAddress`, `kind`. | none |
| `properties.taintLabels` | `canon_bag` of atoms. | P-bag |
| `properties.taintVertex` | Atom, after the parser's `call-arg` rewrite. Dropped with `--endpoints-only` on C0001. | none (rewrite is D2, tested) |
| `properties.taintVertices` | `canon_bag` of `(label, canon_bag of vertex)`. Dropped with `--endpoints-only` on C0001. | P-bag |
| `properties.sourceFunctions`, `sinkFunctions` | `canon_bag` of atoms. | P-bag |
| `properties.sourceCallee`, `sinkCallee` | Dropped: each is the first of the list above (F:3235-3247). | P-drop |
| `properties.taintedFormals` | `canon_bag` of `(formal, canon_bag of labels)`. | P-bag |
| `properties.import` (debug) | Atom (an import name). | none |
| `properties.fileSpanId`, `funcId`, `insnId` (debug) | Dropped. | P-drop |
| Other `properties` | `canon_bag` of `(key, canonical JSON text)` atoms. | P-bag |

**End-to-end theorem** (R13, R14), stated in `sarif-canon` over raw runs: if `ra'` is
obtained from `ra` by any composition of permuting an unordered collection (rows of the
table marked P-bag) and renumbering a table with its references (P-ref), then
`canon_run(ra') == canon_run(ra)`; and with `multiset_diff`'s
`theorem_reports_exactly_count_differences` (`multiset-diff/src/proofs.rs:604`), the diff of
`canon_run(ra)` and `canon_run(rb)` is empty exactly when the two multisets of canonical
results are equal. `multiset-diff` is called through `multiset_diff_by`
(`multiset-diff/src/lib.rs:258`), which becomes `pub` under `verus_keep_ghost` so a
verified caller can name `ord`.

### D5. Report

Mirrors `diff-index` (`ctadl-index-diff/src/report.rs:119-200`): `IDENTICAL`, or
`DIFFERENT`, the two paths, the flags in force, then `more in A: N results, K not shown`
and `[A n, B m] <result>` lines, then the same for B. A result prints as its canonical form
in compact JSON with atoms resolved to their strings. Samples are chosen and ordered by
that printed form, not by atom numbers, so the output does not depend on interning order
(R4). `--json` writes `{result, results_differ, more_in_a, more_in_b, ...}`.

## Concerns

- C1. **Witness-dependent parts of C0001 are compared by default.** The primary location is
  the first span in numeric order where the pair was found (F:2666-2671), and the steps are
  one shortest path, which an unrelated edge can change. Two indexes that find the same
  flows can differ there, and the default mode reports it. Options: (a) as specified:
  default is exact, `--endpoints-only` drops primary location, interior steps and
  `taintVertices`; (b) also drop the primary location by default. (a) matches the intent's
  "same multiset of source-sink findings" for the flag and keeps the default strict. The
  spec assumes (a), and needs confirming that the flag's key (rule, kind, level, labels,
  source and sink functions, endpoints) is the "source-sink finding" the intent means.
- C2. **Unknown fields.** `serde` ignores fields the parser does not name, so a field ctadl
  adds later would be ignored silently until `diff-sarif` learns it. Options: (a) as
  specified: ignore unknown fields, compare unknown `properties` as canonical JSON;
  (b) compare every unknown field as canonical JSON, which reports new fields but also any
  per-run number they carry; (c) reject unknown fields, which forces `diff-sarif` to be
  updated with the formatter. The spec assumes (a); (c) plus a formatter test that runs
  `diff-sarif` on every profile's output would catch drift at build time.
- C3. **How much is proved.** Verifying the whole canonical core, including the
  end-to-end theorem over nested raw runs and a verified merge sort, is a larger proof than
  `multiset-diff`. Options: (a) as specified: the core and its composition are verified,
  only parsing is not; (b) verify the primitives (P-ref, P-bag, P-seq, P-end, P-ord) and
  leave their composition in `canon_run` to review and tests. (b) is cheaper, and the
  composition is mechanical, but a missed `canon_bag` there is exactly the kind of mistake
  the intent fears. The spec assumes (a).
