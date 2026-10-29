# CTADL: Specification, Requirements and Design

A high-level description of what CTADL is for, what it must do, and how it is built. Details
live in the code and the other documents in `docs/`; where they disagree, this document is
stale.

## 1. Purpose

CTADL (Compositional Taint Analysis in Datalog) is a static taint analyzer. Given a program
and where untrusted data enters it (**sources**) and must not arrive (**sinks**), it reports
the data-flow paths between them.

It is for security analysts examining applications and binaries, often without source and
often mixing languages (an Android app's Java and its native libraries). It is research-grade:
precision, scale and room to try new analysis ideas outrank a stable interface.

### Principles

- **Compositional throughout.** Each function is analyzed once into a summary that is reused
  at every call site, never reanalyzed per calling context. Every other part of the analysis
  follows suit: call resolution (hybrid inlining, §4.2) and separately indexed libraries
  (`--summary`).
- **Simple, explainable rules.** Branch conditions are ignored and all given code is
  analyzed, so an analyst can tell why a flow was or was not reported. Payloads hidden behind
  runtime checks, as in malware, are still reported.
- **Between under- and over-approximation.** CTADL is deliberately unsound. It reports flows
  the program text gives strong evidence for, accepting some false positives and negatives.
- **Language-independent core.** Every language is lowered to one small IR of assignments,
  loads, stores and calls; the analysis knows only that IR.
- **Defined in Datalog,** so the rules read as a specification and can be extended.

### Out of scope

- Dynamic analysis; proving a flow exists; proving no flow exists (soundness).
- Bug finding that is not about data flow (memory safety, type errors).
- Backward compatibility of commands, flags, model files or on-disk formats.

## 2. Concepts

| Term | Meaning |
| --- | --- |
| **Artifact** | An input: `.dex`, `.jar`, `.class`, APK/XAPK, a directory of C files, a native binary (via Ghidra p-code), .NET assemblies, Lua, or Flowy (CTADL's test language). |
| **Store** | Where CTADL keeps imports and projects, with their indexes, between runs. |
| **Import** | An artifact lowered to CTADL IR and saved in the store. An APK yields sub-imports for its native libraries. |
| **Project** | A named set of imports indexed together. |
| **Index** | A project's query-independent analysis result: resolved calls and function summaries. |
| **Summary** | How data flows between a function's formals (and return), as pairs of access paths. |
| **Access path** | A variable followed by fields or offsets (`x.f.g`, `p.[8].deref`); the unit of taint. |
| **Port** | The part of a matched program element that a model attaches to: an argument, the return, or an access path through one. |
| **Model generator** | A declarative rule that finds program elements and attaches sources, sinks, propagations or bridges. See [model-generators.md](model-generators.md), a guide to the current implementation, not part of this spec. |

## 3. Requirements

### 3.1 Functional

**Import**

- F1. Lower each supported artifact kind to one IR, preserving data flow, calls (direct,
  virtual, indirect), constants, control flow and exceptions, with a source location (line or
  address) for every instruction.
- F2. Import an APK or XAPK whole, with its native libraries for one chosen ABI.
- F3. Persist imports, so an artifact is imported once and indexed many times; reject imports
  in an older format with a request to re-import.

**Index**

- F4. Index any set of imports as one project, resolving calls across them, with a
  selectable precision/cost trade-off for virtual and indirect calls (class hierarchy
  analysis, hybrid inlining, or a mix).
- F5. Link Java `native` methods to their implementations (by `Java_…` symbol and
  `RegisterNatives` tables), mapping arguments across the JNI ABI.
- F6. Model Android inter-component communication (intents and their receivers).
- F7. Reuse summaries from a separately indexed project (`--summary`).
- F8. Apply shipped per-language default models, user models on top, or user models alone.

**Query**

- F9. Find flows from sources to sinks given by model generators, and report them as SARIF:
  source-to-sink paths or all tainted instructions, each step located in the artifact.
- F10. Check model files against imported programs before indexing, reporting generators and
  ports that match nothing.
- F11. Make a vacuous run visible: a query that could not be answered, or whose endpoints
  matched nothing, says so in the SARIF and exits non-zero.

**Inspection**

- F12. Inspect the store, print imported IR, report call-graph statistics, and dump the index
  and taint graphs (Graphviz).
- F13. Run import, index and query in one command (`ctadl go`).

### 3.2 Non-functional

- N1. **Scale.** Index real Android apps and large binaries on one workstation: large
  relations may spill to disk, and indexing may use many threads.
- N2. **Precision over volume.** Keep false positives down where it is affordable, and make
  every precision/cost knob explicit and recorded with the index.
- N3. **No silent loss.** Whatever the analysis drops (an unresolved call, an unlinked native
  method, an unmatched model, a possibly stale summary) is reported in the log or SARIF. A
  quiet result must not be mistaken for a clean one.
- N4. **Traceability.** Every finding, and every step of a flow, points back to the artifact
  it came from: a file, line and column for source code, an address for a binary. Locations
  survive every IR pass, and a location that cannot be confirmed against the artifact is
  flagged, not guessed.
- N5. **Reproducibility.** An index records the options it was built with; known
  non-determinism is documented ([debugging.md](debugging.md)).
- N6. **Extensibility.** A new language needs only a front end producing IR (and a VMT); SSA,
  summaries, models and output come for free. New library behaviour or endpoints need only
  model generators; new analysis needs only Datalog rules.
- N7. **Clear errors.** User mistakes (a malformed model, a stale import, a missing
  toolchain) produce a message that says what to do.
- N8. **Portable.** One self-contained `ctadl` binary for macOS and Linux; optional tools
  (Ghidra for binaries) degrade gracefully when absent.

## 4. Design

### 4.1 Pipeline

```
 artifact ──import──▶ IR + source info ──index──▶ facts + summaries ──query──▶ SARIF
            (front end)   (store)       (SSA, codegen,    (store)    (taint search,
                                         Datalog fixpoint)            formatter)
                            ▲                   ▲                         ▲
                            │                   │                         │
                         models: propagation, bridge, dispatch      models: sources, sinks
```

Each stage reads only what the previous one saved, so stages can be rerun and debugged
separately.

1. **Import.** A front end lowers the artifact to CTADL IR (`ctadl-ir`), a control-flow graph
   of statements over access paths, and writes it to the store with a virtual method table
   (class hierarchy, symbols) and a source-info database.
2. **Index.** A fixed pass pipeline (dead temporaries, copy coalescing, SSA, copy
   propagation) runs over the project's imports. Codegen turns IR, call-resolution policy and
   propagation models into Datalog facts; the index engine runs them to fixpoint, producing
   the call graph and summaries, saved as parquet.
3. **Query.** Source and sink models are matched by name and signature against the programs,
   then resolved against the index to call sites and access paths. The query engine computes
   taint, and the formatter maps it to source locations and writes SARIF. Query results are
   not persisted.

### 4.2 Analysis

- **Engines.** Index and query rules are written once as Datalog in Rust with
  [Ascent](https://s-arash.github.io/ascent/) and compiled into engines that differ only in
  execution and storage: serial, parallel (rayon), and serial with large relations in
  memory-mapped LSM stores.
- **Local flow** is the transitive closure of assignments between access paths: flow- and
  path-insensitive, with order kept only as far as SSA keeps it.
- **Field propagation.** `x = y` with an access `y.f` implies `x.f = y.f`, and with `x.f`
  implies the same backward, making implied field flows explicit.
- **Finite access paths.** Propagation builds only paths from the program text, from models,
  and one-level concatenations of the two. This makes the fixpoint terminate (`x = x.next`
  would otherwise build `x.next.next...` forever) and is the main source of unsoundness.
- **Summaries.** Local flow between paths rooted at two formals (the return included) becomes
  a summary, instantiated at each call site as assignments between the actuals by prefix
  substitution. Calls and returns stay matched, and cost grows with the program, not with
  calling contexts.
- **Globals** live in one global-heap variable passed to every function like a formal, so
  they flow through summaries like parameters.
- **Call resolution.** Call targets (receiver types, function pointers) flow as data, and a
  target reaching an indirect call resolves it, so call graph and data flow share one
  fixpoint. Virtual calls go through a ladder: super calls, dispatch models, class hierarchy
  analysis for sites with few targets, hybrid inlining for the rest. Hybrid inlining is the
  compositional answer to virtual calls: a flow that depends on a call target passed in as a
  parameter is kept in the summary, conditional on that target (a *decision*), and applied at
  callers that establish it. The summary is still computed once, without knowing the callers,
  and context sensitivity appears only where needed.
- **Aliasing.** A local Andersen-style analysis finds each formal's copy aliases, so a store
  through an alias still yields a summary (`l = p1; l.f = p2` gives `p2 -> p1.f`). In queries,
  field taint may also move backward along whole-variable copies (they share an object);
  value taint only moves forward.
- **Queries** meet forward from sources and backward from sinks. A reported path leaves
  functions through returns before entering them through calls, so it respects call/return
  matching.
- **Cross-language.** The JNI bridge and Android intent handling (`ctadl-ascent/src/languages`)
  add call edges between a project's programs.

### 4.3 Models

A model generator selects program elements (functions or individual call sites), optionally
limited to some imports and filtered by name, signature and structure, and attaches one or
more of these to a port of each match:

- a **source** or **sink**, labelled with a kind the user chooses;
- a **propagation**: how data flows through a function, standing in for code that is absent
  or too costly to analyze;
- a **bridge**: a call edge the code does not show (a callback, a framework hook, a call into
  another import);
- a **dispatch**: the targets a virtual call resolves to, replacing call resolution there.

Propagation, bridge and dispatch models are consumed at index time; sources and sinks at query
time. Per-language defaults ship in `ctadl-ascent/src/models/defaults`.

### 4.4 Source info

Each IR instruction carries a span id into its import's source-info tables, which map it to
a byte range in a file of an original artifact, recorded by path and content hash. Front ends
record spans as they lower, and IR passes carry them along. The formatter resolves them to
SARIF locations: line and column when the artifact's text is at hand, an absolute and
section-relative address for a binary, and the enclosing function as a logical location. When
the artifact is missing, a location falls back to a byte offset and a warning.

### 4.5 Output

One SARIF run, one rule per kind of result: `C0001` source-to-sink paths (a code flow with
intra-/interprocedural steps), `C0002` tainted instructions, `C0003`/`C0004` source and sink
vertices, `C0005` tainted vertices, `C0006` functions holding both source- and sink-tainted
data (an "almost" path), `C0007` external functions that absorb taint. `CTADL0xxx`
notifications report on the run itself. `--sarif-profile` (`human`, `agent`, `machine`,
`debug`) selects what is emitted.

### 4.6 Storage

The store, under `$XDG_STATE_HOME` by default, holds `imports/` and `projects/`. An import is
IR and VMT (`bitcode`) plus source info (parquet), stamped with a format version. A project
holds its import list, index configuration, and index facts and summaries (parquet,
inspectable with DuckDB; see [debugging.md](debugging.md)).

## 5. Verification

- Per-crate unit and integration tests (`cargo test`), including front-end lowering tests
  that run the engine.
- Golden tests for readers and front ends (`xtask`).
- A nightly regression suite (`cargo xtask regression`, or the `regression` flake check):
  small programs in every front end, each with a known answer (the source lines a flow must
  reach), built with real toolchains (javac, dx, gcc, Ghidra, the Android SDK) from Nix.

## 6. Known limitations

See [limitations.md](limitations.md). Also:

- Only context-free summaries cross a `--summary` boundary.
- A native method that fails to link produces no flow and no error, only a JNI bridge log line.
- Some declared model behaviours are accepted but have no effect yet.
