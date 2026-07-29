# Bridging models - DO-NOT-MERGE

**A model-generator construct that connects callsites in one language to implementations in
another.**

## 1. Problem

CTADL indexes several artifacts into one project and one fact base. Within that fact base,
functions are interned by name, so two imports that spell a function identically already share a
node and taint already crosses between them.

What has no representation is a *name mismatch* across a language boundary. An Android app's
`native` method is a bodyless extern in the Dex program; its implementation is a `Java_…` symbol in
the pcode program. Both are functions in the same universe, but nothing joins them: taint entering
the native method vanishes, and taint produced by the implementation never returns.

Name coincidence would not be enough even if it happened. The JNI ABI shifts every argument by two
(`JNIEnv*`, `jobject`), so a bare edge between the two functions would connect the Dex receiver to
`JNIEnv*` and drop every real argument. A cross-language edge must carry an argument
correspondence.

**Goal.** A declarative construct that pins a set of callsites/callees on one side, a set of
implementations on the other, and describes how arguments and returns correspond — expressed in a
model-generator file, matched with the same constraint language everything else uses.

## 2. Semantics: what a bridge is, at the fact level

A bridge needs no new relation and no new inference rule. The index engine already turns a call
into dataflow via two rules:

- **Actual parameters** bind caller vertices to per-site call-argument pseudo-variables, in both
  directions (`actual_param(site, n, FlowVertex(v, p))` ⇒ flow between `v.p` and `call_arg(site,
  n)`).
- **Summary instantiation** replays the callee's summary between the call-argument pseudo-variables
  of any site that targets it (`summary(tgt, n1, p1, n2, p2)` ∧ `call(f, site, tgt)`).

The two rules meet only at the argument *index* `n`. So:

> A bridge is **one `call` row plus one `actual_param` row per mapped port.**

Three consequences that drive the rest of the design:

1. **The port map is the feature, not a refinement.** A bare `call` edge silently mis-wires any
   pair of functions whose ABIs differ — no error, no flow, no diagnostic.
2. **Caller-side access paths are free; callee-side paths are not.** An `actual_param` vertex
   carries a `Path`, and such paths reach the engine's path universe automatically. The
   call-argument side is pinned to the empty path by the rule above, so a callee-side path
   (`"to": "Argument(0).stack.[1]"`) must be emitted as an explicit assignment pair instead, with
   the path registered separately.
3. **Sites must be fresh.** Call-argument pseudo-variables are keyed on the site id, so a bridge
   that reused an existing site's id would alias its argument *n* to that call's argument *n* — a
   spurious bidirectional flow between two unrelated arguments. Every bridge mints a new site id.

### 2.1 Where the edge attaches

Two modes, selected by the generator's `find`:

- **`find: methods`** — synthesize the edge *inside the matched (bodyless) method*. The stub thereby
  acquires a real summary, and every callsite of it anywhere in the program composes with that
  summary for free. This is the JNI case, and it is strictly better than touching callsites: one
  edge per native method, not one per call.
- **`find: callsites`** — synthesize the edge at each matched call site, mapping from the caller's
  actual vertices. Needed when there is no stub to hang a summary on: a call through a table field,
  a `dlsym`'d pointer, the Lua case.

### 2.2 Access paths

Port paths use the canonical access-path grammar shared by model ports, on-disk paths, IR display
and the test DSLs. One distinction matters more here than anywhere else:

- `.[1]` is a real offset — what a native/pcode frontend emits.
- `.\[1]` is a symbol named `[1]` — what the Lua, dex, jvm and tree-sitter C frontends emit for a
  container element.

Getting them backwards matches nothing rather than failing loudly, which is why bridges lean on
`on-unmatched` (§3.2) to make an empty match an error by default.

## 3. Syntax

Two additions to the generator object and one to `model`.

### 3.1 `in` — scope a generator to one program

```jsonc
{ "find": "methods", "in": { "language": "dex" }, "where": [ … ], "model": { … } }
```

`in` takes `language` (an `ArtifactLanguage`: `jvm`, `jar`, `dex`, `apk`, `c`, `lua`, `pcode`,
`flowy`) and/or `import` (the import's name). Omitted means every program.

This is independently useful — a `--models` file otherwise has no way to say "these are libc
models, only apply them to the binary" — and it is what lets a bridge read symmetrically, with the
same key naming the program on each side.

It is deliberately distinct from how *built-in* default model files are selected (by virtual method
table, per import). The VMT is the right key for "which shipped file"; `in` is the right key for
"which import did the user mean".

### 3.2 `model.bridge`

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

**`to` is a match block, not a scope.** It mirrors the existing shape of `forward_call`, whose
`where` lives inside the model — the precedent for "the second set of matches lives in the model".
Side A stays in the generator's own `find`/`where`, so every existing matching feature applies to it
verbatim: `in_function` for callsite mode, `any_of`/`not`, `qualified-id`, and the
unknown-field/unknown-constraint hard errors.

**`arguments`** entries use the existing `port-spec` grammar (`Argument(n)`, `Return`, plus an
optional access path). Omitted entirely ⇒ identity mapping over the arity the two sides share, plus
`Return`. The globals pseudo-parameter is *always* mapped and is not user-visible; without it, heap
flows do not cross the bridge. `direction` is `in` | `out` | `both`, defaulting to `both` — matching
how the engine treats ordinary calls (§2). `Argument(*)` is rejected in a port map: a wildcard has
no correspondent.

**`convention`** answers "an APK has 200 native methods and you cannot hand-write 200 bridges".
`jni-static` / `jni-instance` expand to the standard argument shift. Bare `jni` additionally
supplies a *pairing rule* — derive the JNI symbol from the Dex method id
(`Lcom/example/Crypto;->encrypt(…)` → `Java_com_example_Crypto_encrypt`, both short and long
overload-mangled forms) — so `to.where` may be omitted entirely and the two sides pair by derived
name rather than by cross product. This cannot be expressed as a template (the `/`→`_` and `_`→`_1`
mangling is not a substitution), which is why it is a named built-in rather than syntax.

**`cardinality`** (`one-to-one` default, plus `one-to-many` / `many-to-one` / `many-to-many`) and
**`on-unmatched`** (`error` default, `ignore`) exist because the failure mode here is invisible: a
bridge that matches nothing produces an analysis with zero cross-language flows, which is
indistinguishable from a clean app. Erroring by default matches the loader's existing policy on
unusable constraints. `on-unmatched: "ignore"` is what a *family* bridge needs, since most bodyless
Dex methods are framework methods with no native implementation.

### 3.3 Worked examples

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

**A callsite bridge with callee-side paths (the Lua shape).** No stub exists to attach a summary to,
and the callee takes its arguments off an interpreter stack rather than positionally:

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

The `.stack.[1]` here is an *unescaped* offset on purpose — that is what a native frontend emits. A
Lua-side `t[1]` would be the escaped `.\[1]` (§2.2).

## 4. Architecture

A bridge pins two sets of matches in two different programs, and can only be resolved once *both*
programs' functions exist in the shared id map. That single constraint dictates the whole
structure: parse without a program, retain what matching needs, evaluate after all imports are
codegen'd, then emit.

### 4.1 Program-independent bridge specs

A bridge is the one model that cannot be resolved against a single program, so it is not resolved
during per-program model ingest.

```rust
struct BridgeSpec {
    source: PathBuf, index: usize,      // provenance, for error messages
    from: SideSpec, to: SideSpec,
    ports: PortMap,
    convention: Option<Convention>,
    cardinality: Cardinality,
    on_unmatched: OnUnmatched,
}

struct SideSpec {
    scope: ProgramScope,                // the `in` block
    find:  FindMethod,
    where_: Vec<serde_json::Value>,     // raw JSON — handed back to the existing evaluator in §4.3
}
```

Constraints stay as raw JSON deliberately: they are evaluated later by the *existing* evaluator
against the *existing* match indexes, so nothing here needs to understand them.

Bridge specs are scanned out of the `--models` files **once, before the import loop**. Parsing needs
no program, so hoisting it out both avoids per-import duplicates and lets indexing know up front
whether any bridge exists at all — which §4.2 depends on. Per-program model ingest recognizes
`bridge`, shape-validates it, and skips it: a bridge emits no endpoint and no summary, so it must
not be counted in endpoint statistics.

**Unknown keys must be checked explicitly.** The JSON schema is editor-time only; it is not
evaluated at load. Existing unknown-field checking covers constraints and ports, not the generator
object or the `model` object, so `in`, `bridge`, and every key inside `bridge` need their own key
checks in the same style, with tests that a misspelling is a hard error. (A generic
generator-level key check would be strictly better, but it would reject files that are accepted
today — a separate decision.)

### 4.2 A retained, reusable match index

Matching is a function of a program's name/parent/signature/qualified-id tables plus its function
universe. Those are extracted into an owned value:

```rust
struct ProgramMatchIndex {
    scope: ProgramScope, vmt: VirtualMethodTable,
    names, parents, signatures, qualified_ids, functions, universe,
}
```

Per-program model ingest *borrows* one instead of constructing its own. One struct, one construction
path, two users — this is what keeps bridge matching and ordinary matching from drifting apart. The
alternative, a second matching implementation, would get the per-VMT keying rules (bare name vs
qualified id, plus the externals column) subtly wrong.

Indexing retains a `Vec<ProgramMatchIndex>` across the import loop, built before each program's IR
is consumed, **only when at least one bridge spec was loaded**. Reuse also stops rebuilding the maps
once per model file per import.

*Memory.* The maps own their strings, so retention costs roughly one copy of each program's name
data for the duration of indexing. That should be small next to the assignment and locals relations,
but this is a codebase that measures: add a footprint checkpoint after the import loop and quote a
real number for an APK + `.so` before calling it settled.

### 4.3 Evaluation, after the import loop

```rust
fn apply_bridges(
    &[BridgeSpec], &[ProgramMatchIndex],
    &mut IndexFacts, &mut IndexSourceInfo,
) -> Result<BridgeReport, Error>
```

Called after every import has been codegen'd and before the fact base is saved. At that point every
program's functions are present, so both sides resolve. Evaluating per import cannot work — the
second program's functions do not exist yet, and the failure mode is a silent skip.

Each side is matched by **reusing the existing evaluator**: build a synthetic one-generator value
`{"find": …, "where": …}` and run it over the `ProgramMatchIndex`es whose scope the side's `in`
admits. There must not be a second implementation of `where`; that is how `signature_match` ends up
meaning two different things in two places.

The two result sets are then paired per `cardinality` / `convention`. The report carries per-spec
match and pair counts. Cardinality violations, and empty matches under `on-unmatched: "error"`, are
hard errors carrying the `(file, generator index)` used by every other loader message.

### 4.4 Emission

For each pair `(a, b)` of function ids:

**`find: methods` — attach in the stub.**

```rust
let site = source_info.add_insn_site(a);           // fresh site id — never reuse
facts.call.push((site.into(), b));
for (from, to) in ports {                          // plus the implicit globals pair
    facts.actual_param.push((site.into(), to.index, FlowVertex(formal(from.index), from.path)));
    facts.formal_param.push((a, formal(from.index), ByRef));
    facts.formal_param.push((b, formal(to.index),   ByRef));
    if !from.path.is_empty() { facts.paths.push(from.path); }
}
```

The `formal_param` rows matter: a stub may declare fewer parameters than the port map names, and the
engine seeds its locals from formals. This mirrors what summary emission already does, for the same
reason.

**`find: callsites` — attach at each call.** Identical, except the from-vertices come from the
original site's existing actual parameters rather than from formals, and the site set is derived
from the call relation filtered by the caller match.

**Callee-side access paths** cannot go through `actual_param`, whose call-argument side is pinned to
the empty path (§2). Emit the assignment pair directly —
`facts.assign.push((site, FlowVertex(call_arg(site, n), to_path), FlowVertex(from_var, from_path)))`
and its converse when `direction: both` — and push `to_path` into the fact base's paths. Restrict
this to *literal* model paths: the program path set feeds a one-level concatenation with model
paths, and inflating it is costly.

**Source attribution.** A synthetic site has no source-map entry. Confirm the SARIF formatter
renders a step with no span rather than panicking; if it does not, map the synthetic site to the
span of the stub or of the originating call.

## 5. Schema and docs

In `ctadl-model-generator.schema.json`:

1. `$defs/program-scope`: `{ "language": enum(ArtifactLanguage), "import": string }`,
   `additionalProperties: false`.
2. `$defs/port-map`: `{ "from": port-spec, "to": port-spec, "direction": enum }`, both ports
   required.
3. `$defs/bridge-model`: `to` (required: `{ in?, where }`), `arguments`, `convention`,
   `cardinality`, `on-unmatched`; `additionalProperties: false`.
4. `model.properties` gains `"bridge": { "$ref": "#/$defs/bridge-model" }`.
5. The top-level generator object gains `"in": { "$ref": "#/$defs/program-scope" }`.

Every branch sets `additionalProperties: false`, so a misspelled bridge key is flagged in an editor
wired to the `$schema` URL. That is the *only* place it is caught unless §4.1's explicit key checks
land, since the schema is not evaluated at load time.

`forward_call` and `forward_self` are left alone. Document that `forward_call` is the same-program
special case of `bridge`: once `bridge` exists, folding it in is a one-line desugaring, and
`forward_self` — which selects its target per *receiver class*, not per program — is the only
genuinely separate construct left.

In `docs/model-generators.md`, `bridge` needs its own subsection alongside `forward_call`, a row in
the summary table, and an update to the prose enumerating what the loader actually consumes.

## 6. Scope and limits

**Index-time only.** Bridges create `call` facts, which are consumed by the index fixpoint.
`ctadl query --models` cannot act on them, because query-time models are loaded after the index is
fixed. This matches `propagation`, which is likewise index-time and likewise silently inert at query
time. Document it rather than hard-erroring, since users pass one file to both phases — a deliberate
exception to the fail-loud policy, for the same reason propagation already is one.

**Retires a hack.** The one hand-written, hardcoded cross-language rule in the tree
(`AsyncTask.execute` → `doInBackground`) should be re-expressed declaratively once this machinery
exists, reducing that hook to "run models". It is `forward_self`-shaped rather than a two-program
bridge, so it is not *directly* expressible as one. Track it; do not do it in the same change.

**Not addressed.** A native implementation must currently arrive through pcode/Ghidra; direct C
import is out of scope here. Bridges also do not attempt any type-based or signature-based automatic
pairing beyond the named `convention` built-ins.

## 7. Risks and open questions

**Argument-0 convention must be verified before any JNI expansion is written.** For a Dex extern —
which is what a `native` method is — the generated parameter list is built from the declared method
parameters, which do not include a receiver, while the *callsite* inserts the receiver as actual
argument 0. If that asymmetry is real, `jni-instance`'s shift is off by one, and it is likely a
pre-existing bug affecting propagation models on extern instance methods too. Resolve this before
encoding a convention on top of it: for a known instance-method call, check whether the site has an
`actual_param` at index 0 equal to the receiver, and whether the callee's `formal_param` 0 is the
first *declared* parameter.

**Silent failure is the dominant risk.** Every failure mode of a bridge — wrong path escaping, wrong
argument shift, a `where` that matches nothing, a program scope that admits no import — produces an
analysis with fewer flows, not an error. `on-unmatched: "error"` by default, cardinality checking,
and per-spec match counts in the report are the mitigations; none of them catch a *wrong* pairing,
only an absent one.

**Memory of retained match indexes** (§4.2) is unquantified until measured on a real APK + `.so`.

## 8. Verification approach

- **Parse/validate, no program needed.** Unknown key at generator level, at `model` level, and
  inside `bridge`; missing `to`; `Argument(*)` in a port map; cardinality violation; empty match
  under each `on-unmatched` setting.
- **Matching.** A two-`ProgramMatchIndex` fixture asserting the side-A and side-B match sets and the
  resulting pairs directly, without touching the fact base.
- **Emission.** Given a pair and a port map, assert the exact `call` / `actual_param` /
  `formal_param` / `paths` rows, including the implicit globals pair and that the site id is fresh.
  This is the layer where an argument-shift bug shows up.
- **End-to-end, two flowy imports.** The cheapest real test, and it needs no Android or Ghidra
  toolchain. Give the two artifacts *deliberately different* function names — same-named functions
  already unify, so a name collision would make the test pass without the bridge — and assert both a
  positive flow and a negative case with the model removed.
- **End-to-end, two real frontends.** A tiny JNI app (`.java` + `.c`) built to a dex and an `.so`,
  imported as two artifacts, with a source in Java reaching a sink in C and back. **This is the
  single largest piece of test work** and should be scoped explicitly: it needs a new
  regression-case kind threaded through case discovery, the frontend enum, toolchain gating and
  dispatch, plus the first *two-import* regression runner.

## 9. Alternatives considered

- **Alias the two functions to one id** (make the Dex stub and the native symbol the same node).
  Nearly free, since functions already unify by name — it amounts to renaming one side. But it
  cannot express the ABI shift, which is the actual problem (§1); it destroys per-language
  attribution in SARIF; and function identity is baked into saved facts, so it is not reversible
  after indexing.
- **Model the stub as an indirect call** (callee info + resolvents). Heavier, needs a receiver
  vertex that does not exist, and still offers nowhere to put the port map.
- **A new relation and inference rule for bridges** (`bridge_call(site, tgt, mapping)` plus a
  mapping-aware summary-instantiation rule). Cleanest conceptually, and it would give callee-side
  paths for free — but `call` + `actual_param` already expresses everything except callee-side
  paths, and adding a rule to the main fixpoint carries a cost this does not justify. Revisit if
  callee-side paths (the Lua case) become the common case rather than the exception.
