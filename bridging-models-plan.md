# Bridging models: connecting callsites in one language to implementations in another  - DO-NOT-MERGE

## Context

CTADL can index several artifacts into one project (`AnalysisProject::iter_imports`,
`project.rs:449`), and `cli::index` (`cli/mod.rs:52-139`) already codegens all of them into a
*single* fact base with a *single* `IdMap`. What it cannot do is connect them. An Android app's
`native` method is a bodyless extern in the Dex program; its implementation is a `Java_…` symbol
in the pcode program. Both are functions in `facts.call`'s universe, but no edge joins them, so
taint entering the native method vanishes and taint produced by the implementation never returns.

A **bridging model** is the model-generator construct that adds that edge: it pins a set of
*callsites/callees* on one side, a set of *implementations* on the other, and describes how
arguments and returns correspond.

Three things about the current code shape the design:

1. **The hook already exists, and it is hardcoded.** `models/codegen.rs::load_models` is
   documented as the place for "models that bridge languages" (`codegen.rs:6-8`) and today
   contains exactly one, hand-written in Ascent: `AsyncTask.execute` → `doInBackground`. It
   receives an immutable `&IdMap` and runs per import (`codegen/mod.rs:221-224`), which is
   why nothing declarative can live there yet.
2. **The schema already reserves the slot.** `call-model` /
   `forward_call` (schema `:294-311`, `:371-373`) is "models a callsite by forwarding to another
   function", with a `where` selecting the callee — a bridge is exactly this plus a port map and
   a second program. It is unimplemented (`docs/model-generators.md:378-384`); the loader has no
   `visit_forward_call` at all (`json.rs:2007-2029`).
3. **The matcher is single-program.** `ModelGeneratorIngest::new` (`json.rs:218-328`) builds its
   four match indexes from one `ProgramInfo`, and `cli::index` loads every model file once *per
   import* (`cli/mod.rs:72-76`). A construct that must pin two sets of matches in two different
   programs does not fit inside that loop. This is the central architectural problem, and most of
   the work below is making room for it.

---

## 1. The fact-level mechanics: what an edge has to be

Worth establishing first, because it constrains the syntax.

The index engine turns a call into dataflow with two rules:

```
// index_engine/mod.rs:1088-1093 — actual params bind caller vertices to call-arg pseudo-vars
assign_like(f, v, p, call_arg(insn,n), ∅),
assign_like(f, call_arg(insn,n), ∅, v, p)   <-- actual_param(site, n, FlowVertex(v,p))

// index_engine/mod.rs:1096-1102 — the callee's summary is instantiated at the site
assign_like(f, call_arg(insn,n1), p1, call_arg(insn,n2), p2) <--
    summary(tgt, n1, p1, n2, p2), call(f, insn, tgt)
```

So a bridge is fully expressible in existing facts: **one `call` row plus one `actual_param` row
per mapped port.** No new Ascent relation, no new rule. Two consequences:

- **A bare `call` edge is not enough.** The two rules meet only at the *index* `n`. JNI shifts
  every argument by two (`JNIEnv*`, `jobject`), so `call(site, Java_…)` alone connects the Dex
  receiver to `JNIEnv*` and drops every real argument. It fails silently — no error, no flow.
  **The port map is not a refinement, it is the feature.**
- **Caller-side access paths are free; callee-side paths are not.** `actual_param`'s vertex
  carries a `Path`, and those paths reach `program_paths` automatically
  (`index_engine/mod.rs:1039`, `:851-861`). The call-arg side is pinned to `∅` by the rule above,
  so a callee-side path (`"to": "Argument(0).stack.[1]"`) needs a different emission — a pair of
  `facts.assign` rows plus registering the path in `facts.paths`. See §5.

**Where to attach.** Two modes, and the choice is the generator's `find`:

- `find: methods` — synthesize the edge *inside the matched (bodyless) method*. The stub gets a
  real summary via the existing summary rule (`:1105-1112`), and every callsite of it anywhere in
  the program composes with that summary for free. This is the JNI case and it is strictly better
  than touching callsites: one edge per native method, not one per call.
- `find: callsites` — synthesize the edge at each matched call site, mapping from the caller's
  actual vertices. Needed when there is no stub to hang the summary on (a call through a table
  field, a `dlsym`'d pointer, the Lua case).

In both modes, mint a **fresh** `InsnSiteId` via `IndexSourceInfo::add_insn_site`
(`source_info.rs:32`). Do not reuse an existing call site's `insn_id`: `call_arg!(insn, n)` is
keyed on it, so a second `actual_param` at index `n` would alias the bridge's argument to the
original call's argument `n` — a spurious bidirectional flow between two unrelated arguments.

---

## 2. Proposed syntax

Two additions to the generator object and one to `model`.

### 2a. `in` — scope a generator to one program (independently useful)

```jsonc
{ "find": "methods", "in": { "language": "dex" }, "where": [ … ], "model": { … } }
```

`in` takes `language` (an `ArtifactLanguage`, `project.rs:572-588`) and/or `import` (the import's
`name`, `project.rs:104`); omitted means every program, which is today's behavior. It is worth
having on its own — a model file is currently re-matched against every import with no way to say
"these are libc models, only apply them to the binary" — and it is what makes the bridge read
symmetrically.

### 2b. `model.bridge` — the bridging model

```jsonc
{
  "find": "methods",                  // or "callsites"  — side A (the call side)
  "in":    { "language": "dex" },
  "where": [ … ],                     // side A match: the existing constraint language, unchanged
  "model": {
    "bridge": {
      "to": {                         // side B (the implementation side)
        "in":    { "language": "pcode" },
        "where": [ … ]                // same constraint language, matched against side B's program
      },
      "arguments": [                  // the port map; `to` is the callee port, `from` the caller port
        { "from": "Argument(0)", "to": "Argument(1)" },
        { "from": "Argument(1)", "to": "Argument(2)" },
        { "from": "Return",      "to": "Return", "direction": "out" }
      ],
      "convention":  "jni-instance",  // optional shorthand; expands to `arguments` (and a pairing rule)
      "cardinality": "one-to-one",    // default; how many B's each A may bind
      "on-unmatched": "error"         // default; "ignore" for family bridges
    }
  }
}
```

Design notes on each key:

- **`to` is a match block, not a scope.** It mirrors `forward_call`'s existing shape (a `where`
  inside the model, `schema:303-309`), which is the precedent for "the second set of matches lives
  in the model". Side A stays in the generator's own `find`/`where` so that everything already
  built keeps working on it verbatim: `in_function` for callsite mode, `any_of`/`not`,
  `qualified-id`, and the unknown-field/unknown-constraint hard errors.
- **`arguments`** entries use the existing `port-spec` grammar (`Argument(n)`, `Return`, plus an
  access path). Omitted entirely ⇒ identity mapping over the arity the two sides share, plus
  `Return`. The globals pseudo-parameter (`GLOBALS_INDEX`) is *always* mapped and is not
  user-visible; without it heap flows do not cross the bridge.
  `direction` is `in` | `out` | `both` (default `both`, matching how the engine treats ordinary
  calls — see the bidirectional rule in §1).
- **`convention`** is the answer to "an APK has 200 native methods and you cannot hand-write 200
  bridges". `jni-static` / `jni-instance` expand to the standard shift; bare `jni` additionally
  supplies a *pairing rule* — derive the JNI symbol from the Dex method id
  (`Lcom/example/Crypto;->encrypt(…)` → `Java_com_example_Crypto_encrypt`, both short and long
  overload-mangled forms) — so `to.where` can be omitted entirely and the two sides pair by
  derived name rather than by cross product. This cannot be expressed as a template (the `/`→`_`
  and `_`→`_1` mangling is not a substitution), which is why it is a named built-in rather than
  syntax.
- **`cardinality`** (`one-to-one` default, plus `one-to-many` / `many-to-one` / `many-to-many`)
  and **`on-unmatched`** (`error` default, `ignore`) exist because the failure mode here is
  invisible: a bridge that matches nothing produces an analysis with zero cross-language flows,
  which is indistinguishable from a clean app. Erroring by default matches the loader's existing
  policy on unusable constraints (`json.rs:335-395`, `docs:123-136`). `on-unmatched: "ignore"` is
  what a family bridge needs, since most bodyless Dex methods are framework methods with no
  native implementation.

### 2c. Worked examples

**One JNI method, explicit.**

```jsonc
{
  "find": "methods",
  "in": { "language": "dex" },
  "where": [{ "constraint": "signature_match",
              "qualified-id": "Lcom/example/Crypto;->encrypt(Ljava/lang/String;)Ljava/lang/String;" }],
  "model": { "bridge": {
    "to": { "in": { "language": "pcode" },
            "where": [{ "constraint": "signature_match",
                        "qualified-id": "Java_com_example_Crypto_encrypt" }] },
    "convention": "jni-instance"
  }}
}
```

`jni-instance` expands to exactly this, which the user may also write by hand:

```jsonc
"arguments": [
  { "from": "Argument(0)", "to": "Argument(1)" },   // Dex receiver  -> jobject thiz
  { "from": "Argument(1)", "to": "Argument(2)" },   // first real argument
  { "from": "Return",      "to": "Return", "direction": "out" }
]
// Argument(0) on the callee side is JNIEnv*: deliberately unmapped.
```

**Every JNI method in the app, by convention.**

```jsonc
{
  "find": "methods",
  "in": { "language": "dex" },
  "where": [{ "constraint": "has_code", "value": false }],
  "model": { "bridge": {
    "to": { "in": { "language": "pcode" } },
    "convention": "jni",
    "on-unmatched": "ignore"
  }}
}
```

**A callsite bridge with callee-side paths (the Lua shape).** No stub exists to attach a summary
to, and the callee takes its arguments off an interpreter stack rather than positionally:

```jsonc
{
  "find": "callsites",
  "in": { "language": "lua" },
  "where": [{ "constraint": "signature_match", "name": "mylib.add" }],
  "model": { "bridge": {
    "to": { "in": { "language": "pcode" },
            "where": [{ "constraint": "signature_match", "name": "l_add" }] },
    "arguments": [
      { "from": "Argument(0)", "to": "Argument(0).stack.[1]",  "direction": "in"  },
      { "from": "Argument(1)", "to": "Argument(0).stack.[2]",  "direction": "in"  },
      { "from": "Return",      "to": "Argument(0).stack.[-1]", "direction": "out" }
    ]
  }}
}
```

> There is **no Lua frontend in the tree** — `languages/tree_sitter` is C-only
> (`tree_sitter/mod.rs:593`). This example specifies the shape the syntax must support; it is not
> end-to-end testable today. The testable configurations are dex/jvm + pcode, and flowy + flowy.

---

## 3. Architecture: four changes

### Change 1 — parse bridge specs independently of any program

A bridge is the one model that cannot be resolved against a single `ProgramInfo`, so it must not
be resolved inside `ModelGeneratorIngest`.

- New `models/bridge.rs` with `BridgeSpec { source: PathBuf, index: usize, from: SideSpec, to:
  SideSpec, ports: PortMap, convention, cardinality, on_unmatched }`, parsed from JSON with no
  `ProgramInfo` in scope. `SideSpec` holds `{ scope: ProgramScope, find: FindMethod, where:
  Vec<serde_json::Value> }` — the constraints stay as raw JSON, to be handed back to the existing
  evaluator in Change 3.
- `ModelGeneratorIngest::visit_model` (`json.rs:2007-2029`) learns `bridge` and **skips** it: a
  bridge emits no endpoint and no summary, so `endpoint_stats` must not record it (otherwise
  every bridge trips `CTADL0004`). It still *shape-validates* it, so a typo fails at load.
- `ModelsBatch` gains `bridges: Vec<BridgeSpec>`. Because `cli::index` loads each model file once
  per import (`cli/mod.rs:72-76`), the same spec arrives N times; dedup by `(source, index)`.
- **Better: hoist the parse out of the loop.** Since bridge parsing needs no program, scan the
  `--models` files once *before* the import loop. That both removes the dedup and lets `index`
  know up front whether any bridge exists — which Change 2 needs.

### Change 2 — make the match index a reusable, retained value

Today `ModelGeneratorIngest::new` (`json.rs:218-328`) builds `program_method_names`,
`…_parents`, `…_signatures`, `…_qualified_ids`, `program_functions` and `universe` from a
borrowed `ProgramInfo`, and throws them away when the import's IR is dropped by `codegen_program`
(`cli/mod.rs:93`).

- Extract them into an owned `ProgramMatchIndex { scope: ProgramScope, vmt: VirtualMethodTable,
  names, parents, signatures, qualified_ids, … }` and have `ModelGeneratorIngest` *borrow* one
  instead of constructing its own. One struct, one construction path, two users — this is what
  keeps bridge matching and ordinary matching from drifting apart. It also stops rebuilding those
  maps once per model file per import, which is a small free win.
- `cli::index` retains a `Vec<ProgramMatchIndex>` across the import loop, built before
  `codegen_program` consumes the IR, **only when at least one bridge spec was loaded**.
- *Memory.* The maps own their strings, so this costs roughly one copy of the VMT's name data per
  import for the duration of indexing. That is small next to `assign`/`locals`, but this codebase
  measures (`[mem cp]` logs, `index_engine::phys_footprint_mb`) — add a checkpoint after the loop
  and quote a real number for an APK + `.so` before calling it settled.

### Change 3 — evaluate bridges after the import loop

- New `models::bridge::apply_bridges(&[BridgeSpec], &[ProgramMatchIndex], &mut IndexFacts, &mut
  IndexSourceInfo) -> Result<BridgeReport, Error>`, called in `cli::index` after the loop and
  before `facts.try_save` (`cli/mod.rs:101-109`). This is the timing fix: at that point every
  program's functions are in `source_info.sites`, so both sides resolve. Running per-import
  cannot work — the second program's functions do not exist in the `IdMap` yet, and the failure
  is a silent `continue`, exactly as `codegen_summary` already does for unknown functions
  (`codegen/models.rs:37-42`).
- Each side is matched by **reusing the existing evaluator**: build a synthetic one-generator
  value `{"find": …, "where": …}` and run `ModelGeneratorIngest` over the `ProgramMatchIndex`es
  whose scope the side's `in` admits. Do not write a second implementation of `where` — that is
  how `signature_match` would end up meaning two different things.
- Pair the two result sets per `cardinality` / `convention`, then emit (Change 4). Report
  per-spec counts; violations of `cardinality`, and empty matches under `on-unmatched: "error"`,
  are hard errors carrying the `(file, generator index)` the loader already uses for messages.

### Change 4 — emit the facts

For each pair `(a: FunctionId, b: FunctionId)` and mode:

**`find: methods` (attach in the stub).**

```rust
let site = source_info.add_insn_site(a);           // fresh insn id — never reuse
facts.call.push((site.into(), b));
for (from, to) in ports {                          // plus the implicit globals pair
    facts.actual_param.push((site.into(), to.index, FlowVertex(formal(from.index), from.path)));
    facts.formal_param.push((a, formal(from.index), ByRef));   // as codegen_summary does
    facts.formal_param.push((b, formal(to.index),   ByRef));   // (models.rs:78-92)
    if !from.path.is_empty() { facts.paths.push(from.path); }  // -> program_paths (:851-861)
}
```

The `formal_param` rows matter: the stub may declare fewer parameters than the map names, and
`locals` is seeded from `formal_param` (`:1056-1059`). This mirrors what summary codegen already
does for the same reason.

**`find: callsites` (attach at each call).** Same, except the from-vertices come from the
original site's existing `actual_param` rows rather than from formals, and the site set is
computed from `facts.call` (site → callee) filtered by the `in_function` caller match.

**Callee-side access paths** (`"to": "Argument(0).stack.[1]"`) cannot go through `actual_param`,
whose call-arg side is pinned to `∅`. Emit the pair directly instead —
`facts.assign.push((site, FlowVertex(call_arg(insn,n), to_path), FlowVertex(from_var, from_path)))`
and its converse for `direction: both` — and push `to_path` into `facts.paths`. Keep this to
literal model paths: `program_paths` feeds a one-level concat with `model_paths`
(`:1052-1053`), and the comment at `:915-918` records what happens when that set is inflated.

**Source attribution.** A synthetic site has no `source_map` entry
(`source_info.rs:37-39`). Confirm the SARIF formatter renders a step with no span rather than
panicking; if it does not, map the synthetic site to the span of the stub or of the original call.

---

## 4. Schema changes

In `ctadl-model-generator.schema.json`:

1. New `$defs/program-scope`: `{ "language": enum(ArtifactLanguage), "import": string }`,
   `additionalProperties: false`.
2. New `$defs/port-map`: `{ "from": port-spec, "to": port-spec, "direction": enum }`, both ports
   required.
3. New `$defs/bridge-model`: `to` (required: `{ in?, where }`), `arguments`, `convention`,
   `cardinality`, `on-unmatched`; `additionalProperties: false`.
4. `model.properties` gains `"bridge": { "$ref": "#/$defs/bridge-model" }`.
5. The top-level generator object gains `"in": { "$ref": "#/$defs/program-scope" }`.
6. Leave `forward_call` and `forward_self` alone for now, and note in the docs that
   `forward_call` is the same-program special case of `bridge` — once `bridge` lands, folding it
   in is a one-line desugaring and `forward_self` (which selects its target per *receiver class*,
   not per program) is the only genuinely separate construct left.

Every branch already sets `additionalProperties: false`, and the loader hard-errors on unknown
fields (`json.rs:353-395`), so mis-spelled bridge keys fail in the editor and at load without
extra work.

---

## 5. What this makes possible, and what it does not

**Retires a hack.** `models/codegen.rs`'s hand-written `AsyncTask.execute` → `doInBackground`
Ascent rule is the only bridge in the tree. It is `forward_self`-shaped, so it is not
*directly* expressible as a two-program `bridge`, but once the machinery exists the hardcoded
rule should be re-expressed declaratively and the hook reduced to running models. Track it; do
not do it in the same change.

**Index-time only.** Bridges create `call` facts, which are consumed by the index fixpoint;
`ctadl query --models` cannot act on them. This matches `propagation`, which is likewise
index-time and likewise silently inert at query time. Document it in `docs:36-48` and the §9
table rather than hard-erroring, since users pass one file to both phases — a deliberate
exception to the fail-loud policy, for the same reason propagation already is one.

**Argument-0 convention must be verified before writing any JNI expansion.** For a Dex *extern*
(which is what a `native` method is — `parser.rs:1106-1108` returns no code, so `dex/mod.rs:190`
never adds it as a defined method and it arrives via the extern path at `:383-421`), the
generated `FunctionData.params` does **not** include a receiver, while the *callsite* inserts the
receiver as actual argument 0 (`codegen/mod.rs:456-458`). If that asymmetry is real, `jni-instance`'s
shift is off by one, and it may be a pre-existing bug affecting propagation models on extern
instance methods too. Check it before encoding a convention on top of it:

```bash
ctadl index <apk-project> && ctadl inspect …   # or CTADL_* trace on facts.actual_param
# for a known instance-method call: does the site have an actual_param at index 0 = receiver,
# and does the callee have formal_param 0 = first declared parameter?
```

---

## 6. Verification

**Unit — `models/bridge.rs` tests.** Parse/validate: unknown key, missing `to`, `Argument(*)` in
a port map (reject: a wildcard has no correspondent), `cardinality` violation, empty match under
each `on-unmatched`. These need no program.

**Unit — matching.** `tests/json_error_handling.rs` already has hand-built `native_program()`
(`:262`) and `java_program()` (`:285`) fixtures; a bridge test needs two of them at once, so add
a two-`ProgramMatchIndex` helper and assert the side-A/side-B sets and the resulting pairs
directly, without touching `IndexFacts`.

**Unit — emission.** Given a pair and a port map, assert the exact `call` / `actual_param` /
`formal_param` / `paths` rows, including the implicit globals pair and that the site id is
fresh. This is the layer where the shift bug would show up.

**End-to-end — flowy.** The cheapest real test: two flowy imports in one project (flowy is
already a first-class `ArtifactLanguage`, `cli/mod.rs:42`), a bridge model connecting a stub in
artifact 1 to an implementation in artifact 2, and flowy's own `where flows [...]` /
`where summaries [...]` assertions to check the flow arrives — including a `</-` negative case
asserting no flow *without* the model. This exercises the whole path with no Android or Ghidra
toolchain.

**End-to-end — the real thing.** A tiny JNI app: `.java` + `.c`, built to an APK/dex and an `.so`,
imported as two artifacts, with a source in Java reaching a sink in C and back. This needs a new
regression-case kind: `xtask/src/discovery.rs:20-27` models a case as exactly one source file
plus a config (`Kind::Dex { java, config }`), so add `Kind::Bridged { java, c, config }` and the
matching build/import steps. **This is the single largest piece of test work in the plan** and
should be scoped explicitly rather than discovered late.

**Regression.** `cargo test --workspace`, plus `cargo xtask regression` and `cargo xtask
regression --frontend pcode` — Changes 1 and 2 touch the loader and the model-index construction
that every shipped `default-index.jsonl` goes through, so the existing suites are the guard that
no bridge-free run changed behavior. Confirm with the `[mem cp]` logs that retaining the match
indexes did not move the indexing peak on a real APK.

## 7. Alternatives considered

- **Alias the two functions to one `FunctionId`** (make the Dex stub and the native symbol the
  same node). Simplest possible bridge — no synthetic site, no port map — but it cannot express
  the JNI ABI shift, it destroys per-language attribution in SARIF, and `FunctionId` identity is
  baked into saved facts, so it is not reversible after indexing.
- **Model the stub as an indirect call** (`callee_info` + `callee_resolvents`, the machinery at
  `:1250-1263`). Heavier, needs a receiver vertex that does not exist, and still offers nowhere
  to put the port map.
- **A new Ascent relation and rule for bridges** (`bridge_call(site, tgt, mapping)` with a
  mapping-aware summary-instantiation rule). Cleanest conceptually, and it would give callee-side
  paths for free — but the existing `call` + `actual_param` pair already expresses everything
  except callee-side paths, and adding a rule to the main fixpoint has a cost this does not
  justify. Revisit if callee-side paths (the Lua case) become the common case rather than the
  exception.
