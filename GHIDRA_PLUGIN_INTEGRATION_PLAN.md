# Plan: Make the Ghidra taint plugin work with rust/Ascent ctadl

## Context

The Ghidra decompiler taint plugin (in `../ghidra`, package
`ghidra.app.plugin.core.decompiler.taint`) lets a user mark sources/sinks in the
decompiler, runs an external `ctadl` engine over exported pcode, and highlights
the resulting taint in the decompiler window. It was written for the **old
Souffle-based ctadl** (`../ctadl`): it generated raw **Souffle datalog** query
files and called a Souffle-era CLI.

We are moving to the **new rust/Ascent ctadl** (this repo). Two things must be
reconciled so the plugin and the new engine work together:

1. The plugin must emit **JSON5 model-generator format** (this repo's query
   format) instead of Souffle datalog.
2. The new ctadl CLI must accept what the plugin calls: ingest a
   pre-exported facts directory *without re-running Ghidra*, and return SARIF on
   stdout where the plugin reads it.

The user requires **full fidelity**: the old plugin could seed taint at an
arbitrary local variable at a specific instruction address; the new JSON5 format
only expresses function+port (Return / Argument(n)) sources/sinks today. So the
rust model ingest must be **extended** to support per-variable / per-instruction
seeding.

Everything is run through each project's **nix flake**. Verification uses a small
(<300K) binary compiled from `nightly/tests/c/example.c`.

## What already exists (do not rebuild)

- **`legacy-pcode-cli` seam** — `ctadl-ascent/src/main.rs:57-116,482-545`. A
  Ghidra-facing subcommand: `legacy-pcode-cli --directory <store> [-m models]
  index -f <facts>` and `... query <query_file>`. The nix package
  `legacy-pcode-cli` (`flake.nix:156-162`) wraps it as `ctadl-legacy-pcode-cli`.
  The plugin's `buildIndex`/`buildQuery` arg shapes already line up with this
  wrapper (see below) — **no plugin arg-list changes needed**.
- **Byte-identical export scripts** — the plugin's
  `ghidra_scripts/ExportPCodeForCTADL.java` and this repo's
  `pcode-reader/ExportPcode.java` are identical (1237 lines). So facts the
  plugin exports are exactly what `pcode-reader` (`pcode-reader/src/lib.rs`)
  consumes. **No fact-schema work.**
- **SARIF is already the result contract on both sides.** Rust emits SARIF with
  tool name `ctadl`, rule IDs `C0001.tainted-path` / `C0002.tainted-instruction`
  (+ source/sink/data/almost-path), and pcode `physicalLocation.address.
  absoluteAddress` (kind `instruction`) — `ctadl-ascent/src/query_engine/
  formatter.rs`. The plugin parses exactly these in `sarif/
  SarifTaintResultHandler.java` and maps them to Ghidra addresses.
- **JSON5 model ingest** — `ctadl-ascent/src/models/json.rs` +
  `ctadl-ascent/src/models/mod.rs` (`EndpointBuilder`) +
  `ctadl-ascent/src/models/ctadl-model-generator.schema.json`. Today only
  `find: "methods"` is implemented; sources/sinks become
  `EndpointBuilder::append(function, port, access_path, label, direction, …)`.
- **Vertex-level seeding already exists internally.** The analysis is seeded by
  `QueryFacts.endpoints: Vec<(QueryEndpoint,)>` where `QueryEndpoint { infunc,
  vertex, label, direction }` (`ctadl-ascent/src/query_engine/mod.rs:37-141`).
  So seeding an arbitrary vertex is architecturally supported; the model layer
  just needs to resolve (function, var-name, address) → that vertex.

## Part A — rust CLI fixes (make the CLI accept what the plugin calls)

Both changes are in `ctadl-ascent`.

### A1. Ingest a pre-exported facts directory without re-running Ghidra
`legacy-pcode-cli index -f <facts>` imports `<facts>` as
`ImportLanguage::Pcode`, which flows through `import_pcode`
(`ctadl-ascent/src/languages/pcode/mod.rs:24-57`) → **always** calls
`ghidra::run_ghidra_export`. `GhidraSource::detect`
(`.../pcode/ghidra.rs:47-59`) classifies a facts directory (no `.gpr`) as a raw
`Binary` and re-runs `analyzeHeadless` on it — wrong.

**Fix:** detect when the artifact is *already an exported facts directory* (e.g.
it directly contains `CTADLLanguage.facts` / `PROGRAM_FILE.facts` / the `.facts`
tables) and, in that case, read facts straight from it (`ctx.process(&facts_dir,
…)` in `import_pcode`), skipping `run_ghidra_export`. Reuse `PcodeFactsReader`
which already reads that directory. This matches the plugin's contract: it
exports facts to the Facts dir, then calls `index -f <FactsDir>`.

### A2. Return SARIF on stdout
`AbstractTaintState.queryIndex` reads the engine's **stdout**
(`readQueryResultsIntoDataFrame(program, p.getInputStream())`,
`AbstractTaintState.java:338,363`), but `handle_legacy_pcode_cli` sets
`output: "results.sarif"` (a file). All diagnostics already go to stderr and the
query path writes SARIF to stdout when `output == "-"` (`cli/mod.rs:340-356`;
formatter honors `-`).

**Fix:** in the `LegacyPcodeSubcommand::Query` arm
(`ctadl-ascent/src/main.rs:517-541`) set `output: PathBuf::from("-")`. Then the
plugin's existing stdout read gets clean SARIF.

## Part B — plugin emits JSON5 instead of Souffle datalog

All in `../ghidra/.../taint/ctadl/CTADLTaintState.java` (+ one options default).
The datalog is produced by `AbstractTaintState.writeQueryFile` calling the
`writeHeader`/`writeRule`/`writeGate`/`writeFooter` hooks per mark
(`AbstractTaintState.java:199-233`).

- **Change the query file to a single JSON5 document.** JSON5 is structured, not
  line-appended, so override `writeQueryFile` in `CTADLTaintState` (or repurpose
  the hooks to accumulate) to serialize one `{ "model_generators": [ … ] }`
  document from the active `sources`/`sinks`/`gates`. Gson is already imported in
  the plugin.
- **Map each `TaintLabel` → a model generator.** `TaintLabel`
  (`taint/TaintLabel.java`) already carries: function name (`getFunctionName`),
  instruction address (`getAddress().getOffset()`), variable name
  (`TaintState.varName(token,false)`), symbol/global flags, and whether it's
  function-level (`getVarnodeAddress()==null`). Mapping:
  - Function-name token / return → `{find:"methods", where:[{constraint:
    "signature_match", name:<fn>}], model:{sources|sinks:[{kind:<label>,
    port:"Return"}]}}`.
  - Function parameter → `port:"Argument(n)"`.
  - Arbitrary local at an address → **Part C** `find:"variables"`/`"instructions"`
    generator carrying function + var-name + instruction address.
- **Change the query filename** from `taintquery.dl` to a `.json5` extension:
  `TaintOptions.DEFAULT_TAINT_QUERY` and `getTaintQueryDLName()` usage
  (`AbstractTaintState.java:293-296`).
- `buildIndex`/`buildQuery` arg lists are unchanged (they already match
  `legacy-pcode-cli` via the wrapper: `--directory <dir> index -j8 -f <facts>`
  and `--directory <dir> query [--compute-slices …] --no-compile-analysis -j8
  --format=… <queryfile>` — the extra flags rust accepts/ignores).

## Part C — rust: full-fidelity variable/instruction seeding

Extend the JSON5 model ingest so `find:"instructions"` (and `find:"variables"`)
seed taint at an interior vertex identified by (function, instruction-address,
optional variable-name), mirroring the old Souffle rule
(`(PCODE_INPUT|PCODE_OUTPUT)(i,vn) , PCODE_TARGET(i,addr) , CVar_SourceInfo(vn,
NAME,"myvar")`). The plugin's variable marks already carry exactly this:
`TaintLabel` has function name, **instruction address** (`getAddress()
.getOffset()`), and variable name (`varName(token)`).

**The Ascent engine needs NO changes.** The seed rule
(`ctadl-ascent/src/query_engine/mod.rs:256-260`) destructures any endpoint's
`FlowVertex(v,p)` straight into `taint` — a local vertex seeds exactly like a
formal port. Seeding an interior vertex = producing a `QueryEndpoint{infunc,
vertex: FlowVertex(local_or_callarg, path), label, direction}` and adding it to
`facts.endpoints`, which is done in `build_query_endpoints`
(`ctadl-ascent/src/cli/mod.rs:34-158`, the formal-port branch at `:75-124`).

**The only real work is resolution + persistence**, because instruction
addresses survive to query time but the `(address → interior vertex)` and
`(human-name → vertex)` maps are dropped from the persisted index:
- `assign_like` drops the insn id (`index_engine/mod.rs:369`), so arbitrary
  address→vertex is not recoverable today; only call-arg vertices are
  (`actual_param` + `source_map` both persist the site).
- Pcode locals are named by **SSA-versioned varnode-id** (`pcode/mod.rs:1074-
  1103`), and Ghidra HVAR/SYMBOL human names are read at import but never
  attached (`pcode-reader/src/lib.rs:207-224`), so human names aren't queryable.

### C1. Persist an address→vertex table (primary mechanism)
At **codegen** (`ctadl-ascent/src/codegen/mod.rs:259-292`, where each statement's
`site: PackedInsnSiteId` and its `FlowVertex`es coexist) emit a new fact
`vertex_at_address: (FunctionId, address, FlowVariable, Path)` — the def/use
vertices at each instruction, address taken via the existing `source_map`
(`site → FileSpanId → offset`, `index_engine/source_info.rs`). Persist it as a
new parquet table modeled on `index_source_map` / `actual_param` (schema helpers
in `ctadl-ascent/src/facts/schema`; add to `IndexResult` in
`index_engine/mod.rs`). Load it at query time alongside the other query facts.

### C2. (Optional refinement) Persist a name→vertex table
For the plugin's "by symbol" / by-name marks and to disambiguate multiple
vertices at one address, capture `named_var: (FunctionId, human_name,
FlowVariable)` during **pcode import** (where `hvar_name_facts` /
`symbol_hvar_facts` / `hvar_representative_facts` are live,
`pcode-reader/src/lib.rs`), resolving name → representative varnode → the `Local`
`FlowVariable` (map across SSA versions by base-name prefix). Persist as parquet.

### C3. Model ingest + schema + endpoint building
- `ctadl-ascent/src/models/json.rs` — extend `enum FindMethod` (`:58-59`, today
  only `Methods`) with `Instructions`/`Variables`; make `visit_find` (`:156-171`)
  accept them instead of erroring; add a **`address`** `where`-constraint handler
  and reuse the existing `name`/regex machinery for variable names.
- `ctadl-ascent/src/models/ctadl-model-generator.schema.json` — `find` already
  allows `"instructions"`/`"variables"` (`:346`); add an `address` where-
  constraint variant (e.g. `{constraint:"address", value:"0x401000"}`).
- `ctadl-ascent/src/models/mod.rs` — add a `VertexEndpointBuilder`/batch
  (columns: function, address?, var_name?, path, label, direction, wildcard)
  alongside `EndpointBuilder` (`:680-848`) rather than overloading the formal
  `index` column.
- `ctadl-ascent/src/cli/mod.rs` — in/beside `build_query_endpoints` add a branch
  that joins vertex-endpoint rows against the new `vertex_at_address` /
  `named_var` tables and emits `QueryEndpoint`s with `FlowVertex(local, ap)`.
  (These stay function-anchored with `call_site:None` — the call-site fan-out at
  `query_engine/mod.rs:89-118` only applies to formals; fine.)

**Sequencing:** land C1 + C3 first (address-pinned `find:"instructions"`), which
faithfully matches the plugin's address-carrying marks and lets end-to-end GUI
highlighting be verified early; add C2 (name) as a refinement. No Ascent engine
changes in any step.

## Verification (end-to-end, via nix)

1. **Build the small binary:** compile `nightly/tests/c/example.c` (a
   `source()`→`transfer()`→`sink()` program; expected tainted lines 10–12) to a
   tiny ELF, e.g. within `nix develop` here.
2. **Build the rust engine:** `nix build .#legacy-pcode-cli` (this repo) →
   `ctadl-legacy-pcode-cli`.
3. **CLI smoke test (Parts A/C) without Ghidra GUI:** export facts for the
   binary (run `ExportPcode.java` via Ghidra headless, or reuse an existing facts
   dir), then `ctadl-legacy-pcode-cli --directory <store> index -f <facts>` and
   `... query <query.json5>`; confirm SARIF comes out on **stdout** and marks the
   expected instructions. Test a function-level query and a variable-level
   (Part C) query.
4. **Build Ghidra with the plugin:** `cd ../ghidra && nix-shell --run
   ./build-ghidra.sh`.
5. **Drive the GUI** via the agentic-gui harness (`../ghidra/
   run-ghidra-agentic.sh` / `agentic-gui/open-ghidra-gui.sh`, client
   `agentic-gui/ghidra-gui`). Import the small binary, set Decompiler tool
   options (`Taint.Query Engine=ctadl`, engine path = the
   `ctadl-legacy-pcode-cli` wrapper, facts/output dirs), then **export pcode →
   create index → mark a source and sink (both a function-level and a
   variable-level mark) → run query**.
6. **Confirm highlighting:** the expected program points become highlighted in
   the decompiler window. If the rust `Human` SARIF profile's shape (graph
   nodes/edges vs code-flows, `logicalLocation.kind`, `properties.taintLabels`)
   doesn't drive the plugin's highlighters, adjust the SARIF shape /
   legacy-query profile and re-verify — this is the one area to nail down
   empirically.

## Critical files

- Rust CLI: `ctadl-ascent/src/main.rs`, `ctadl-ascent/src/cli/mod.rs`
- Rust pcode import: `ctadl-ascent/src/languages/pcode/mod.rs`, `.../pcode/ghidra.rs`
- Rust model ingest (Part C): `ctadl-ascent/src/models/json.rs`,
  `ctadl-ascent/src/models/mod.rs`,
  `ctadl-ascent/src/models/ctadl-model-generator.schema.json`,
  `ctadl-ascent/src/query_engine/mod.rs`
- Rust SARIF: `ctadl-ascent/src/query_engine/formatter.rs`
- Plugin: `../ghidra/.../taint/ctadl/CTADLTaintState.java`,
  `.../taint/AbstractTaintState.java`, `.../taint/TaintLabel.java`,
  `.../taint/TaintOptions.java`, `.../taint/sarif/SarifTaintResultHandler.java`
- Nix: `flake.nix` (this repo), `../ghidra/{flake.nix,shell.nix,build-ghidra.sh}`
