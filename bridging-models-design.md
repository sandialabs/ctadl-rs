# Bridging models - DO-NOT-MERGE

**A model-generator construct that connects callsites in one language to implementations in
another.**

## 0. Status

The JNI half of this design shipped as a **built-in pass**, not as syntax
(`ctadl-ascent/src/languages/jni.rs`, `docs/jni.md`, `nightly/tests/jni/`). What that settled, and
what it left, is what this revision is about.

| | state |
| --- | --- |
| Fact-level shape of a bridge (§2) | **shipped and validated** — `call` + `actual_param`, fresh site, synthesized caller formals |
| JNI linking, mangling, port map | **shipped**, automatic, `--no-jni-bridge` to disable |
| Multi-import SARIF attribution | **shipped** — `index_source_map` gained an `import_id` column (`INDEX_FORMAT_VERSION` 3) |
| Two-import end-to-end regression runner | **shipped** — `cargo xtask regression --frontend jni` |
| `model.bridge`, `in`, `find: callsites`, callee-side paths | **not started** — this document |

Nothing about the declarative construct was invalidated. Two things about it were: the fixed
`jni-*` argument shifts it proposed are wrong (§2.3), and the `convention` key they justified is
gone (§3.2).

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

**That one boundary is now closed in code.** `languages::jni` observes each import's virtual method
table, mangles every Java `native` method into its JNI symbol, and joins the two halves with a port
map computed from the method descriptor. It runs automatically and needs no models file.

It is also the whole of what CTADL can bridge. The pass is keyed on the `VirtualMethodTable::Java`
/ `::Native` pair and on the JNI mangling rules; it is not parameterizable, and it is not reachable
from a `--models` file. Everything else is still where §1 started:

- a Lua script calling a C function registered in a `luaL_Reg` table;
- a native implementation bound through `RegisterNatives` rather than by symbol name, whose
  correspondence lives in a `JNINativeMethod[]` the analysis would have to constant-propagate;
- a call through a table field, a `dlsym`'d pointer, or any hand-rolled FFI;
- the `JNIEnv` accessor vtable — `(*env)->GetStringUTFChars(env, s, 0)` — which the bridge delivers
  arguments *to* but cannot propagate *through*.

Every one of these is a name mismatch plus an argument correspondence, and every one of them is a
handful of pairs a user knows and the analysis does not.

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

This is no longer a claim: it is what `jni::emit_bridge` does, and taint crosses a real
Java-to-C boundary in both directions on the strength of those two relations alone. Five
consequences drive the rest of the design, the last three of which the implementation discovered.

1. **The port map is the feature, not a refinement.** A bare `call` edge silently mis-wires any
   pair of functions whose ABIs differ — no error, no flow, no diagnostic. The `JniArgShift`
   regression case pins this: one native method called twice, taint in the argument the
   implementation returns in one call and in the argument it drops in the other, so an off-by-one
   flips both assertions at once.
2. **Caller-side access paths are free; callee-side paths are not.** An `actual_param` vertex
   carries a `Path`, and such paths reach the engine's path universe automatically. The
   call-argument side is pinned to the empty path by the rule above, so a callee-side path
   (`"to": "Argument(0).stack.[1]"`) must be emitted as an explicit assignment pair instead, with
   the path registered separately. *Unvalidated:* every JNI port is an empty-path formal, so
   nothing in the tree exercises this yet.
3. **Sites must be fresh.** Call-argument pseudo-variables are keyed on the site id, so a bridge
   that reused an existing site's id would alias its argument *n* to that call's argument *n* — a
   spurious bidirectional flow between two unrelated arguments. Every bridge mints a new site id.
4. **Formals are synthesized on the caller side only.** A bodyless stub has no `formal_param` rows
   at all — the dex frontend sets parameters up only when it finds a code item — and the summary
   rule joins on them, so the bridge emits them itself for every mapped port, exactly as
   `codegen_summary` does for modelled functions. The *callee* side gets no synthesized rows: its
   formals come from its own frontend, and inventing one would fabricate a parameter the
   disassembler never recovered. When the callee's arity is short of what the port map needs —
   Ghidra gives a function with no recovered prototype zero parameters — the right response is a
   warning naming both functions and both arities, which is what ships.
5. **Return and globals are ports like any other, and the return arity is asymmetric.** A Java
   function has return arity 2 (`-1` normal, `-2` exception); a native function has one. Only the
   normal return is mappable — a JNI implementation cannot throw into the second — so a port map's
   `Return` means `-1`, and the exception return is deliberately unmapped. The globals
   pseudo-parameter (`GLOBALS_INDEX`) must be mapped explicitly or heap flows do not cross the
   boundary at all; the JNI pass maps it unconditionally, and `JniFlow` — taint in through one
   native function, out through a different one via a native global — is the case that fails
   without it.

### 2.1 Where the edge attaches

Two modes, selected by the generator's `find`:

- **`find: methods`** — synthesize the edge *inside the matched (bodyless) method*. The stub thereby
  acquires a real summary, and every callsite of it anywhere in the program composes with that
  summary for free. This is the JNI case; it is what shipped, and it is strictly better than
  touching callsites: one edge per native method, not one per call.
- **`find: callsites`** — synthesize the edge at each matched call site, mapping from the caller's
  actual vertices. Needed when there is no stub to hang a summary on: a call through a table field,
  a `dlsym`'d pointer, the Lua case. Unvalidated — nothing in the tree emits this shape yet.

### 2.2 Argument correspondence is per-method, not a fixed shift

The original draft claimed `jni-instance` expands to `Argument(0)→Argument(1)`,
`Argument(1)→Argument(2)`, …, "which the user may also write by hand". That is wrong, and the
reason is worth stating because it constrains what a port map can be.

The native side *is* fixed by the ABI: index 0 is `JNIEnv *`, index 1 is the receiver `jobject` or
the declaring `jclass`, and declared parameter *k* lands at `2 + k`. The **Java side is not**. The
slot of declared parameter *k* is frontend-dependent:

- **Dex** numbers parameters by *register*, and `long`/`double` consume two of them. A static
  `(JI)V` puts the `int` at slot 2.
- **JVM** numbers parameters by *argument position*, one per declared parameter, wide or not. The
  same `(JI)V` puts the `int` at slot 1.

Both put `this` at slot 0 for an instance method. So a correct port map is a function of the method
descriptor *and* the observing frontend — `jni::port_map(descriptor, is_static, slots)` — and the
`+2` shift only looks constant because the two happen to agree until the first wide parameter.

Consequences for the syntax:

- A hand-written `arguments` list is inherently **per method**, and correct only for the frontend
  the method was observed through. That is acceptable for the one-off pairings a declarative bridge
  exists to express, and useless as a family shorthand.
- Anything that wants to map a *family* of methods must compute the map from each method's
  signature, which means it needs signature parsing and frontend knowledge. That is a pass, not
  syntax. §3.2 drops `convention` accordingly.

### 2.3 Access paths

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
{ "find": "methods", "in": { "language":  "dex" },                          "where": [ … ], "model": { … } }
{ "find": "methods", "in": { "languages": ["jvm", "jar", "dex", "apk"] },   "where": [ … ], "model": { … } }
```

`in` takes:

- **`language`** — one `ArtifactLanguage` (`jvm`, `jar`, `dex`, `apk`, `c`, `lua`, `pcode`,
  `flowy`);
- **`languages`** — a non-empty array of them, admitting a program whose language is any one;
- **`import`** — the import's name.

Omitting `in` means every program, as does an `in` naming no language key. Keys *within* one `in`
block are ANDed: `{ "languages": ["dex", "apk"], "import": "app_dex" }` is that import, and only if
it is one of those two languages.

**`language` is exactly the one-element case of `languages`.** Both are accepted because the
one-language scope is the common one and `["dex"]` reads worse than `"dex"`; they normalize to the
same thing (§4.1). Giving both keys in one block is a hard error rather than a union — a reader
cannot tell which was meant — and so is `"languages": []`, which would match nothing quietly.

The plural is not sugar. A language *boundary* has a set on each side, not a language: "the Java
side" is `jvm`/`jar`/`dex`/`apk` and "the native side" is `pcode` and eventually `c`. A bridge
scoped to `"dex"` matches nothing the day the same app is imported from a `.jar` — no error, just an
analysis with no cross-language flow, which is §7's dominant failure mode. Making the natural
scope expressible in one generator is cheaper than asking every author to duplicate it per frontend
and remember to.

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
  "in":    { "language": "lua" },
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
      "cardinality": "one-to-one",    // default; how many B's each A may bind
      "on-unmatched": "error"         // default
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
optional access path), and name *slots*, not declaration order (§2.2). Omitted entirely ⇒ identity
mapping over the arity the two sides share, plus `Return`. The globals pseudo-parameter is *always*
mapped and is not user-visible; without it, heap flows do not cross the bridge. `Return` means the
normal return; a Java side's exception return is never mapped. `direction` is `in` | `out` | `both`,
defaulting to `both` — matching how the engine treats ordinary calls (§2). `Argument(*)` is rejected
in a port map: a wildcard has no correspondent.

**No `convention` key.** The draft proposed `jni` / `jni-static` / `jni-instance` shorthands to
answer "an APK has 200 native methods and you cannot hand-write 200 bridges". That question is
answered — by a built-in pass that fires with no models file at all — and the shorthands could not
have answered it correctly anyway, since the expansion they promised is not a fixed shift (§2.2).
The precedent stands for the next boundary that needs it: a *family* correspondence derived from
signatures belongs in `languages/`, alongside `jni.rs`, where it can see descriptors and staticness;
`bridge` is for the pairings a user names one at a time.

**`cardinality`** (`one-to-one` default, plus `one-to-many` / `many-to-one` / `many-to-many`) and
**`on-unmatched`** (`error` default, `ignore`) exist because the failure mode here is invisible: a
bridge that matches nothing produces an analysis with zero cross-language flows, which is
indistinguishable from a clean app. Erroring by default matches the loader's existing policy on
unusable constraints. `on-unmatched: "ignore"` is what a bridge written against a family of
optional symbols needs — most matched stubs will have no implementation present.

### 3.3 Worked examples

**One `RegisterNatives`-bound method.** The symbol does not follow the mangling rules, so the
built-in pass cannot see it; the correspondence is in a `JNINativeMethod[]` the user can read and
the analysis cannot:

```jsonc
{
  "find": "methods",
  "in": { "languages": ["dex", "apk"] },
  "where": [{ "constraint": "signature_match",
              "qualified-id": "Lcom/example/Crypto;->encrypt(Ljava/lang/String;)Ljava/lang/String;" }],
  "model": { "bridge": {
    "to": { "in": { "language": "pcode" },
            "where": [{ "constraint": "signature_match", "name": "crypto_encrypt_impl" }] },
    "arguments": [
      { "from": "Argument(0)", "to": "Argument(1)" },   // Dex receiver  -> jobject thiz
      { "from": "Argument(1)", "to": "Argument(2)" },   // first real argument
      { "from": "Return",      "to": "Return", "direction": "out" }
    ]
  }}
}
```

`Argument(0)` on the callee side is `JNIEnv*`: deliberately unmapped. `Argument(1)` on the Dex side
is the first declared parameter only because no earlier parameter is wide — with a leading `long`
it would be `Argument(2)` here and `Argument(1)` if the same class were imported as a `.jar`
(§2.2). Write these against the artifact you are actually indexing. The scope spans `dex` and `apk`
safely for exactly that reason: those two are the same frontend and so share a slot model, whereas
`["dex", "jar"]` with this `arguments` map would be right for one of them at most (§7).

Note what this example is *not*: a method whose implementation is named by the JNI rules needs no
generator at all. If you find yourself writing one, check the `jni bridge:` line in the `index` log
first — the method is more likely unresolved or ambiguous than unbridgeable.

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
Lua-side `t[1]` would be the escaped `.\[1]` (§2.3).

This example exercises both of the mechanisms the JNI pass left untested: callsite attachment
(§2.1) and callee-side paths (§2, consequence 2). Expect it to be the harder half of the work.

## 4. Architecture

A bridge pins two sets of matches in two different programs, and can only be resolved once *both*
programs' functions exist in the shared id map. That single constraint dictates the whole
structure: parse without a program, retain what matching needs, evaluate after all imports are
codegen'd, then emit. `jni.rs` is the same structure at one-tenth the generality —
`JniObserver::observe` per import, `jni::link` after the loop — and is worth reading as a
skeleton before starting.

### 4.1 Program-independent bridge specs

A bridge is the one model that cannot be resolved against a single program, so it is not resolved
during per-program model ingest.

```rust
struct BridgeSpec {
    source: PathBuf, index: usize,      // provenance, for error messages
    from: SideSpec, to: SideSpec,
    ports: PortMap,
    cardinality: Cardinality,
    on_unmatched: OnUnmatched,
}

struct SideSpec {
    scope: ProgramScope,                // the `in` block
    find:  FindMethod,
    where_: Vec<serde_json::Value>,     // raw JSON — handed back to the existing evaluator in §4.3
}

struct ProgramScope {
    languages: SmallVec<[ArtifactLanguage; 2]>,   // empty ⇒ any language
    import:    Option<String>,
}
```

`ProgramScope` **normalizes at parse time**: `language` and `languages` both land in the one vector,
so `admits()` has a single implementation and no caller ever asks which spelling the file used. The
mutual-exclusion and non-empty checks (§3.1) belong here too, next to the unknown-key checks below —
the schema catches neither at load time.

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

### 4.2 Observe during the import loop, resolve after it

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

Two ordering facts, both learned the hard way in `jni.rs`:

- The observation must run **before `codegen_program` consumes the `ProgramInfo`**, and it must hold
  *owned* data, because the id map that both sides resolve against does not exist until every
  import has been codegen'd. `JniObserver` holds `String`s for exactly this reason.
- What a side needs to match on has to be **in the VMT to begin with**. A bodyless dex `native`
  method appeared in no column at all until the frontend was changed to push one — it is skipped by
  the code-item branch and by the extern-stub loop alike. Any `find` a bridge relies on should be
  checked against a real bodyless artifact before the matching code is written, not after.

*Memory.* The maps own their strings, so retention costs roughly one copy of each program's name
data for the duration of indexing. That should be small next to the assignment and locals relations,
but this is a codebase that measures: add a footprint checkpoint after the import loop and quote a
real number for an APK + `.so` before calling it settled. (`jni.rs` sidesteps this — it retains only
the VMT's native rows — which is not an option once arbitrary `where` constraints are in play.)

### 4.3 Evaluation, after the import loop

```rust
fn apply_bridges(
    &[BridgeSpec], &[ProgramMatchIndex],
    &mut IndexFacts, &mut IndexSourceInfo,
) -> Result<BridgeReport, Error>
```

Called after every import has been codegen'd and before the fact base is saved — the same point
`cli::index` calls `jni::link` from, and for the same reason. At that point every program's
functions are present, so both sides resolve. Evaluating per import cannot work — the second
program's functions do not exist yet, and the failure mode is a silent skip.

Each side is matched by **reusing the existing evaluator**: build a synthetic one-generator value
`{"find": …, "where": …}` and run it over the `ProgramMatchIndex`es whose scope the side's `in`
admits. There must not be a second implementation of `where`; that is how `signature_match` ends up
meaning two different things in two places.

The two result sets are then paired per `cardinality`. The report carries per-spec match and pair
counts, and — following `LinkStats` — is logged at `info` even when nothing went wrong, since a
bridge that did not fire is otherwise indistinguishable from an app with no cross-language flow.
Cardinality violations, and empty matches under `on-unmatched: "error"`, are hard errors carrying
the `(file, generator index)` used by every other loader message.

### 4.4 Emission

For each pair `(a, b)` of function ids:

**`find: methods` — attach in the stub.** This is `jni::emit_bridge` with a user-supplied port map:

```rust
let site = source_info.add_insn_site(a);           // fresh site id — never reuse
facts.call.push((site.into(), b));
for (from, to) in ports {                          // plus the implicit globals pair
    facts.actual_param.push((site.into(), to.index, FlowVertex(formal(from.index), from.path)));
    facts.formal_param.push((a, formal(from.index), ByRef));
    if !from.path.is_empty() { facts.paths.push(from.path); }
}
```

The `formal_param` rows matter: a stub may declare fewer parameters than the port map names, and the
engine seeds its locals from formals. This mirrors what summary emission already does, for the same
reason. Note there is **no** row for side B (§2, consequence 4) — check `b`'s arity against the
highest mapped callee index instead and warn when it falls short, naming both functions and both
numbers. That warning is the only signal a user gets that a stripped or prototype-less binary is
quietly dropping arguments.

**`find: callsites` — attach at each call.** Identical, except the from-vertices come from the
original site's existing actual parameters rather than from formals, and the site set is derived
from the call relation filtered by the caller match.

**Callee-side access paths** cannot go through `actual_param`, whose call-argument side is pinned to
the empty path (§2). Emit the assignment pair directly —
`facts.assign.push((site, FlowVertex(call_arg(site, n), to_path), FlowVertex(from_var, from_path)))`
and its converse when `direction: both` — and push `to_path` into the fact base's paths. Restrict
this to *literal* model paths: the program path set feeds a one-level concatenation with model
paths, and inflating it is costly.

**Source attribution — resolved, and it costs a step.** A synthetic site has no `source_map` entry,
and the SARIF formatter's step emitter simply returns early for a site with no location: no panic,
and no bogus location either. The flow renders as the caller-side steps followed by the
callee-side steps, with nothing in between naming the crossing. That is what ships for JNI and what
the nightly cases assert against. Leaving the span absent is also the *correct* choice now that
spans are per-import indices resolved against the import that numbered them (`INDEX_FORMAT_VERSION`
3) — a synthetic site borrowing either side's span would be read against the wrong database.
Attributing the crossing to the stub's own span, so the flow shows where it jumped languages, is a
worthwhile follow-up and should be scoped as one.

## 5. Schema and docs

In `ctadl-model-generator.schema.json`:

1. `$defs/program-scope`: `{ "language": enum(ArtifactLanguage), "languages": { "type": "array",
   "items": enum(ArtifactLanguage), "minItems": 1, "uniqueItems": true }, "import": string }`,
   `additionalProperties: false`, plus `"not": { "required": ["language", "languages"] }` so an
   editor flags giving both. That last one is the only rule here a schema can express and a careless
   load-time check would miss, which is why §4.1 repeats it.
2. `$defs/port-map`: `{ "from": port-spec, "to": port-spec, "direction": enum }`, both ports
   required.
3. `$defs/bridge-model`: `to` (required: `{ in?, where }`), `arguments`, `cardinality`,
   `on-unmatched`; `additionalProperties: false`.
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
the summary table, and an update to the prose enumerating what the loader actually consumes. The
file already carries a callout saying a Java `native` method needs no model; that callout should
survive `bridge` landing, and gain a pointer to the `RegisterNatives` case as the exception.
`docs/jni.md` gets a cross-reference the other way, replacing "see model generators for the code
the bridge cannot reach" with the specific construct.

## 6. Scope and limits

**Index-time only.** Bridges create `call` facts, which are consumed by the index fixpoint.
`ctadl query --models` cannot act on them, because query-time models are loaded after the index is
fixed. This matches `propagation`, which is likewise index-time and likewise silently inert at query
time, and it matches the built-in JNI bridge, which for the same reason requires a re-`index` when a
native artifact is added late. Document it rather than hard-erroring, since users pass one file to
both phases — a deliberate exception to the fail-loud policy, for the same reason propagation
already is one.

**Retires a hack.** The one hand-written, hardcoded cross-language rule in the tree
(`AsyncTask.execute` → `doInBackground`, in `models/codegen.rs`) should be re-expressed
declaratively once this machinery exists, reducing that hook to "run models". It is
`forward_self`-shaped rather than a two-program bridge, so it is not *directly* expressible as one.
Track it; do not do it in the same change. Note that JNI went the other way — a second hardcoded
pass — on purpose: it needs descriptor parsing and per-frontend slot models, which is code (§2.2).

**Not addressed.** A native implementation must currently arrive through pcode/Ghidra; direct C
import is out of scope here. Bridges do not attempt any type-based or signature-based automatic
pairing — that is what a `languages/` pass is for. And a bridge delivers taint to a function; it
does not propagate taint *through* code the analysis cannot resolve, so the `JNIEnv` accessor
vtable still needs default models for `JNINativeInterface` plus indirect-call resolution, neither of
which this design provides.

## 7. Risks and open questions

**Silent failure is the dominant risk.** Every failure mode of a bridge — wrong path escaping, wrong
argument slot, a `where` that matches nothing, a program scope that admits no import — produces an
analysis with fewer flows, not an error. `on-unmatched: "error"` by default, cardinality checking,
and an unconditional per-spec count line (§4.3) are the mitigations; none of them catch a *wrong*
pairing, only an absent one. The JNI experience says the count line is the one that actually gets
read, and that the two conditions worth escalating to `warn` are an ambiguous match and a callee
whose recovered arity is short of the port map.

**A hand-written port map is frontend-specific** (§2.2). The same model file is silently wrong for
the `.jar` build of an app whose `.apk` it was written against, wherever a wide parameter precedes a
mapped one. Options: reject nothing and document it; validate the map against the callee's arity
(catches some cases, not this one); or accept declaration-ordinal ports and translate. Undecided,
and worth deciding before the syntax is published rather than after.

`languages` (§3.1) sharpens this rather than causing it: `{ "languages": ["dex", "jar"] }` is
precisely the scope whose two frontends disagree about wide-parameter slots, and it makes that scope
a natural thing to write. The two features want resolving together — if ports stay slot-valued, a
multi-language scope carrying an `arguments` map is arguably worth a warning; if they become
declaration-ordinal, the conflict disappears and the plural is unambiguously the right default.

**Memory of retained match indexes** (§4.2) is unquantified until measured on a real APK + `.so`.

**Callee-side paths and callsite attachment are both unvalidated** (§2, §2.1) — the JNI pass needed
neither. The Lua example in §3.3 is the first thing that exercises them and should be built early
enough that a surprise there can still change the design.

## 8. Verification approach

- **Parse/validate, no program needed.** Unknown key at generator level, at `model` level, and
  inside `bridge`; missing `to`; `Argument(*)` in a port map; cardinality violation; empty match
  under each `on-unmatched` setting. For `in`: `language` and `languages` given together, an empty
  `languages`, an unrecognized `ArtifactLanguage`, and — the one positive case — that
  `{"language": "dex"}` and `{"languages": ["dex"]}` parse to the identical `ProgramScope`, so the
  two spellings cannot drift.
- **Matching.** A two-`ProgramMatchIndex` fixture asserting the side-A and side-B match sets and the
  resulting pairs directly, without touching the fact base.
- **Emission.** Given a pair and a port map, assert the exact `call` / `actual_param` /
  `formal_param` / `paths` rows, including the implicit globals pair, that the site id is fresh, and
  that no `formal_param` row is emitted for side B. `languages/jni/tests.rs` is the model for this
  layer; extend it rather than starting a new fixture style.
- **End-to-end, two flowy imports.** The cheapest real test, and it needs no Android or Ghidra
  toolchain. Give the two artifacts *deliberately different* function names — same-named functions
  already unify, so a name collision would make the test pass without the bridge — and assert both a
  positive flow and a negative case with the model removed.
- **End-to-end, two real frontends.** *The infrastructure exists.* `cargo xtask regression
  --frontend jni` builds both halves of a boundary, imports the `.java` as a DEX and the `.c` as a
  pcode shared library, co-indexes them as one project, and checks Java-side `expected_lines`
  through the dex linemap plus `expected_native_lines` through `addr2line`. Reuse it: a declarative
  bridge case is the same two-import shape with `--no-jni-bridge` and a `model_generators` entry
  doing the join by hand, which also gives a direct A/B against the built-in.
- **Shape the end-to-end cases so no per-function model could fake them.** `JniFlow` is the
  worked example — taint in through one native function, held in a native global, out through a
  different one — and `JniArgShift` is the port-map counterpart, where an off-by-one flips two
  assertions in opposite directions. A bridge test that a single `propagation` model would also
  satisfy proves nothing.

## 9. Alternatives considered

- **A built-in pass per boundary** — what JNI now is. Zero configuration, and it can compute a port
  map from a method descriptor, which no models file can (§2.2). The costs are that it must be
  written in Rust against a specific VMT shape, that every new boundary is a new pass, and that a
  user staring at an unbridged `dlsym` call has no recourse. The two coexist: passes for
  correspondences derivable from signatures, `bridge` for the ones only a human knows.
- **Alias the two functions to one id** (make the Dex stub and the native symbol the same node).
  Nearly free, since functions already unify by name — it amounts to renaming one side. But it
  cannot express the ABI shift, which is the actual problem (§1); it destroys per-language
  attribution in SARIF, which the multi-import work has since made real; and function identity is
  baked into saved facts, so it is not reversible after indexing.
- **Model the stub as an indirect call** (callee info + resolvents). Heavier, needs a receiver
  vertex that does not exist, and still offers nowhere to put the port map.
- **A new relation and inference rule for bridges** (`bridge_call(site, tgt, mapping)` plus a
  mapping-aware summary-instantiation rule). Cleanest conceptually, and it would give callee-side
  paths for free — but `call` + `actual_param` already expresses everything except callee-side
  paths, as JNI now demonstrates end to end, and adding a rule to the main fixpoint carries a cost
  this does not justify. Revisit if callee-side paths (the Lua case) become the common case rather
  than the exception.
