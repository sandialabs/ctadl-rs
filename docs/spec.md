# CTADL: Specification, Requirements and Design

This is a high-level description of what CTADL is for, what it must do, and how it is built.
It names the parts and how they connect; the code and the other documents in `docs/` hold the
details. When the two disagree, the code is right and this document is stale.

## 1. Purpose

CTADL (Compositional Taint Analysis in Datalog) is a static taint analyzer. Given a program
and a description of where untrusted data enters it (**sources**) and where that data must not
arrive (**sinks**), it reports the data-flow paths from sources to sinks, without running the
program.

It is for security analysts and researchers who examine applications and binaries, often
without source code, and often apps that mix languages (an Android app's Java and its native
libraries). It is research-grade software: precision, scale and the ability to try out new
analysis ideas count for more than a stable interface.

### Scope

In scope:

- Compositional, hole-program, interprocedural, field-sensitive taint analysis over many input
  languages, bytecode and binaries included.
- Multi-language analysis. Crosses language boundaries (JNI) and Android inter-component
  communication.
- Declarative, user-written models for sources, sinks and code the analyzer cannot see.
- Results in a standard, tool-consumable format (SARIF).

Out of scope:

- Dynamic analysis, and proving that a reported flow is exploitable.
- General-purpose bug finding that is not about data flow (memory safety, type errors).
- Backward compatibility of commands, flags, model files or on-disk formats between versions.

## 2. Concepts

| Term | Meaning |
| --- | --- |
| **Artifact** | An input to analyze: a `.dex`, `.jar`, `.class`, APK/XAPK, a directory of C files, a native binary (via Ghidra p-code), .NET assemblies, Lua, or Flowy (CTADL's test language). |
| **Import** | An artifact translated into CTADL IR and saved in the store. An APK yields sub-imports for its native libraries. |
| **Project** | A named set of imports indexed together. |
| **Index** | The per-project result of the analysis that does not depend on a query: resolved calls, and a summary of each function's data flow. |
| **Summary** | How data flows from a function's inputs to its outputs, in terms of formal parameters and access paths. |
| **Access path** | A variable followed by a sequence of fields or offsets (`x.f.g`, `p.[8].deref`), the unit of taint. |
| **Model generator** | A rule in a JSON/JSON5/JSONL file that finds program elements and attaches sources, sinks, propagations or bridges to them. See [model-generators.md](model-generators.md). |
| **Query** | A taint search over an index, with endpoints taken from model generators. |

## 3. Requirements

### 3.1 Functional

**Input**

- F1. Import each supported artifact kind into one language-independent IR, preserving data
  flow, calls (direct, virtual, indirect), constants, control flow and exceptions.
- F2. Record, for each IR instruction, its source location (file and line, or address for
  binaries), so that results can point back at the artifact.
- F3. Import an APK or XAPK as a whole, including its native libraries for one chosen ABI.
- F4. Store imports persistently, so that an artifact is imported once and indexed or queried
  many times; detect imports written by an older format and ask for a re-import.

**Indexing**

- F5. Index any set of imports together as a project, resolving calls across them.
- F6. Resolve virtual and indirect calls, with a user-selectable trade-off between precision
  and cost (class hierarchy analysis, hybrid inlining, or a mix of the two).
- F7. Compute per-function summaries compositionally, so that a function is analyzed once and
  its summary reused at every call site, with context sensitivity where calls are resolved
  through arguments (call targets passed as parameters).
- F8. Link Java `native` methods to their native implementations automatically (by `Java_…`
  symbol and by `RegisterNatives` tables) and map arguments across the JNI ABI.
- F9. Model Android inter-component communication (intents and their receivers).
- F10. Reuse summaries from a separately indexed project (`--summary`), so that a large app
  and its libraries can be indexed apart.
- F11. Apply shipped default models per language, and user models on top, or user models
  alone.

**Querying and output**

- F12. Find flows from sources to sinks given by model generators, forward from sources and
  backward from sinks.
- F13. Report results as SARIF, either as source-to-sink paths or as the full set of tainted
  instructions, with each step located in the artifact.
- F14. Check model files against the imported programs before indexing: report generators that
  match nothing and ports that resolve to nothing.
- F15. Make a vacuous run visible: a query that could not be answered, or whose endpoints
  matched nothing, says so in the SARIF and exits non-zero.

**Inspection**

- F16. Inspect the store, pretty-print imported IR, report call-graph statistics, and dump the
  index and taint graphs (Graphviz) for debugging.
- F17. Provide a single command (`ctadl go`) that runs import, index and query in one step.

### 3.2 Non-functional

- N1. **Scale.** Index real Android apps and large native binaries on one workstation, with
  bounded memory: large relations may spill to disk, and indexing may use many threads.
- N2. **Precision over volume.** Prefer analyses that keep false positives down (field
  sensitivity, context where it pays, exact call resolution when it is cheap), and make every
  precision/cost knob explicit and recorded with the index.
- N3. **No silent loss.** Whenever the analysis drops something (an unresolved call, an
  unlinked native method, a model that matched nothing, a summary that may be stale), it says so
  in the log or the SARIF. A quiet result must not be mistaken for a clean one.
- N4. **Reproducibility.** An index records the options it was built with, so a query can say
  what it is reading. Known sources of non-determinism are documented
  ([debugging.md](debugging.md)).
- N5. **Extensibility.** Adding a language means writing a front end that produces the IR;
  nothing downstream changes. Modelling a construct means writing a model generator or
  Datalog rules, not changing the front ends.
- N6. **Clear errors.** User mistakes (a malformed model, a stale import, a missing toolchain)
  produce a message that says what to do.
- N7. **Portable builds.** One self-contained `ctadl` binary for macOS and Linux. Optional
  external tools (Ghidra for binaries) degrade gracefully when absent.

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

Each stage reads only what the stage before it saved, so the stages can be run, rerun and
debugged separately.

1. **Import.** A front end parses the artifact and lowers it to CTADL IR (`ctadl-ir`), a
   control-flow graph of statements over access paths. The IR, a virtual method table (class
   hierarchy, symbols) and the source-info database are written to the store.
2. **Index.** The imports of a project are loaded and run through a fixed pass pipeline (dead
   temporaries, copy coalescing, SSA, copy propagation). Code generation (`codegen`) turns the
   IR, the call-resolution policy and the propagation models into Datalog facts. The index
   engine runs the index rules to fixpoint, producing the call graph and function summaries,
   which are saved as parquet.
3. **Query.** Source and sink models are matched against the programs (stage 1, by name and
   signature) and resolved against the index (stage 2, to concrete call sites and access
   paths). The query engine computes taint over the index, meeting forward from sources and
   backward from sinks, and the formatter maps the result to instructions and source locations
   and writes SARIF. Query results are not persisted.

### 4.2 Analysis

- **Datalog.** The index and query algorithms are written as Datalog in Rust with
  [Ascent](https://s-arash.github.io/ascent/), once, and compiled into several engines that
  differ only in execution and storage: serial, parallel (rayon), and serial with large
  relations in memory-mapped LSM stores.
- **Compositional summaries.** A summary relates a formal's access path to another formal's
  (or the return's) access path. A call site instantiates its callee's summary by prefix
  substitution, so analysis cost grows with the program, not with the number of calling
  contexts.
- **Call resolution.** Virtual calls are resolved by a ladder: super calls, then dispatch
  models, then class hierarchy analysis for sites with few targets, then hybrid inlining for
  the rest. Hybrid inlining keeps flows that depend on a call target passed in as a parameter
  conditional on that target (a *decision*), and applies them at the callers that establish
  it, giving context sensitivity only where it is needed.
- **Aliasing.** Field taint may travel backward along whole-variable copies (the copies share
  an object); value taint only moves forward.
- **Cross-language.** The JNI bridge and Android intent handling are passes in
  `ctadl-ascent/src/languages` that add call edges between programs of one project.

### 4.3 Components

| Crate / directory | Role |
| --- | --- |
| `ctadl-ascent` | The `ctadl` binary: CLI, codegen, models, query engine, SARIF formatter, cross-language passes, reports. |
| `ctadl-index` (+ `engines/{serial,par,lsm}`) | Fact schema, parquet encoding, the index Datalog and its engines. |
| `ctadl-ir` | The IR, its verifier and visitors, graph algorithms, and the SSA pass pipeline. |
| `ctadl-import` | The store: on-disk layout, import format versioning, reading and writing imports. |
| `ctadl-frontends` | Picks a front end by artifact language; APK/XAPK unpacking and native sub-imports. |
| `frontends/ctadl-*` | One front end per language: `dex`, `jvm`, `c` (tree-sitter), `pcode` (Ghidra), `cil`, `lua` (tree-sitter), `flowy`. |
| `readers/*` | Format parsers with no CTADL dependency: Dex, class files, p-code, CIL. |
| `source-info` | Compact source spans from instructions to files and lines. |
| `trie`, `tailshare`, `immortal`, `lsm` | Data structures for the engines: prefix tries, shared-suffix sequences, interning, disk-backed relations. |
| `xtask`, `nightly` | Test harnesses: golden tests and the source/sink regression suite. |

### 4.4 Storage

The store lives under `$XDG_STATE_HOME` (overridable) and holds `imports/` and `projects/`.
An import is IR and VMT in a binary encoding (`bitcode`) plus source info in parquet, stamped
with a format version. A project holds its import list, the index configuration and the
index facts and summaries in parquet, which can be inspected with DuckDB
([debugging.md](debugging.md)).

### 4.5 Models

Model generators (schema: `ctadl-ascent/src/models/ctadl-model-generator.schema.json`) have a
`find` (what kind of element), an `in` (which imports), `where` constraints (names,
signatures, structure) and a `model` (sources, sinks, propagation, bridge, dispatch). Each
kind is consumed by one phase: propagation, bridge and dispatch models at index time,
sources and sinks at query time. Default propagation models ship per language in
`ctadl-ascent/src/models/defaults`.

### 4.6 Extension points

- **A new language:** a front end that produces valid IR and a VMT. SSA, summaries, models and
  output come for free.
- **New library behaviour or endpoints:** model generators; no code changes.
- **New analysis rules:** Datalog rules in the index or query engine.

## 5. Verification

- Unit and integration tests per crate (`cargo test`), including per-front-end lowering tests
  that run the engine on small programs.
- Golden tests for readers and front ends (`xtask`).
- A nightly regression suite (`cargo xtask regression`, or the `regression` flake check) of
  small programs in every front end, each with a known answer: the source lines a flow must
  reach. It builds its inputs with real toolchains (javac, dx, gcc, Ghidra, the Android SDK)
  supplied by Nix.

## 6. Known limitations

See [limitations.md](limitations.md). In addition, by design:

- Only context-free summaries cross a `--summary` boundary.
- A flow through a native method that fails to link produces no result and no error, only a
  log line from the JNI bridge.
- Some schema features (`taint`, `forward_self` models) are not yet implemented.
