# Bridging models - DO-NOT-MERGE

**A model-generator construct that connects callees in one language to implementations in
another.**

## 0. Status

The JNI half of this design shipped as a **built-in pass**, not as syntax
(`ctadl-ascent/src/languages/jni.rs`, `docs/jni.md`, `nightly/tests/jni/`). What that settled, and
what it left, is what this revision is about.

| | state |
| --- | --- |
| Fact-level shape of a bridge (§2) | **shipped and validated** — `call` + `actual_param`, fresh site, synthesized caller formals; callee-side formals (§2, consequence 4) are a pending change to `jni::emit_bridge` |
| JNI linking, mangling, port map | **shipped**, automatic, `--no-jni-bridge` to disable |
| Multi-import SARIF attribution | **shipped** — `index_source_map` gained an `import_id` column (`INDEX_FORMAT_VERSION` 3) |
| Two-import end-to-end regression runner | **shipped** — `cargo xtask regression --frontend jni` |
| `model.bridge`, `in`, callee-side paths | **not started** — this document |

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

**Goal.** A declarative construct that pins a set of callees on one side, a set of implementations
on the other, and describes how arguments and returns correspond — expressed in a model-generator
file, matched with the same constraint language everything else uses.

## 2. Semantics: what a bridge is, at the fact level

A bridge needs no new relation and no new inference rule. The index engine already turns a call
into dataflow via two rules:

- **Actual parameters** bind caller vertices to per-site call-argument pseudo-variables, in both
  directions (`actual_param(site, n, FlowVertex(v, p))` ⇒ flow between `v.p` and `call_arg(site,
  n)`).
- **Summary instantiation** replays the callee's summary between the call-argument pseudo-variables
  of any site that targets it (`summary(tgt, n1, p1, n2, p2)` ∧ `call(f, site, tgt)`).

The two rules meet only at the argument *index* `n`. So:

> A bridge is **one `call` row, one temporary per mapped callee index, and one `assign` row per
> port direction** — the temporaries being what the call passes as its actuals.

The JNI pass validates the degenerate half of this: with every port an empty-path formal it reduces
to `call` plus one `actual_param` per port, which is what `jni::emit_bridge` emits, and taint
crosses a real Java-to-C boundary in both directions on the strength of those two relations alone.
The temporaries are what generalize it to ports that name a sub-path of the callee's parameter
(consequence 2). Five consequences drive the rest of the design.

1. **The port map is the feature, not a refinement.** A bare `call` edge silently mis-wires any
   pair of functions whose ABIs differ — no error, no flow, no diagnostic. The `JniArgShift`
   regression case pins this: one native method called twice, taint in the argument the
   implementation returns in one call and in the argument it drops in the other, so an off-by-one
   flips both assertions at once.
2. **A port map is a relabeling; `actual_param` cannot relabel, so the bridge routes through a
   temporary.** The rule expands one row into a bidirectional pair: for an actual `x.p`,
   `call_arg(site, n) = x.p` together with `x.p = call_arg(site, n)`. The pseudo-variable thereby
   stands for the argument *as a whole* — its empty path is what makes it the whole vertex, not a
   truncation of one — and every later rule that mentions `call_arg(site, n)` relates back to the
   original actual in both directions. A row has one vertex column and no second path, so it cannot
   express what `"to": "Argument(0).stack.[1]"` asserts: that the caller's actual corresponds to a
   *sub-path* of the callee's parameter rather than to the parameter.

   The construct that does express it is a **temporary** — one local in the caller per mapped callee
   index, standing for that parameter as the callee sees it. The bridge assigns between the caller's
   port and a *sub-path of the temporary*, and passes the temporary whole:

   ```
   t_n.to_path      = formal_k.from_path    // direction: in
   formal_k.from_path = t_n.to_path         // direction: out
   actual_param(site, n, FlowVertex(t_n, empty))
   ```

   Everything downstream is then ordinary. `actual_param` binds `t_n` whole to `call_arg(site, n)`,
   so summary instantiation — `assign_like(f, call_arg(i, n1), dst_path, call_arg(i, n2), src_path)`
   — replays `b`'s summary against `t_n`'s sub-paths exactly as it does for any other call, at every
   caller of the stub, including callers hybrid inlining resolves inside the fixpoint (§2.1). No
   rule has to know a bridge was involved.

   Three things fall out that a hand-expanded `actual_param` got wrong or could not say:

   - **Ports sharing a callee index stay distinct.** The Lua map (§3.3) puts three ports on callee
     `Argument(0)`, at `.stack.[1]`, `.stack.[2]` and `.stack.[-1]`. Three `actual_param` rows at
     index 0 would bind all three caller ports to the one `call_arg(site, 0)` pseudo-variable and
     alias them to each other, bidirectionally — the same failure consequence 3 mints fresh sites to
     avoid. One temporary at three sub-paths separates them by field-sensitivity, which is the
     mechanism that was always meant to do this work.
   - **`direction` becomes expressible.** `actual_param` is unconditionally bidirectional. A pair of
     `assign` rows is two independent facts, so `in` / `out` / `both` is just which of them get
     pushed.
   - **Nothing is ever concatenated.** Every path the bridge writes is *literal*: `from_path` on the
     caller's formal, `to_path` on the temporary. The temporary is the seam, so no composite
     `from_path ++ to_path` need exist in the path set. That is the decisive advantage over the
     obvious alternative — drop `to_path` from the row and register the concatenation in
     `facts.paths` so ordinary field propagation carries it. That alternative needs not one
     composite but the whole family `from_path ++ to_path ++ q` for every `q` the callee's summary
     adds, which is not a set a bridge can enumerate.

   Registration stays the engine's job. `program_paths` is seeded from both endpoints of every
   `assign` and from every `actual_param` vertex (`index_engine/mod.rs`), so `from_path` and
   `to_path` each register themselves. There is no `facts.paths` push anywhere in a bridge.

   One asymmetry survives, and it is about which bucket a path lands in rather than whether it
   lands. Summary-carried paths become `model_paths` and so concatenate with every program path; a
   path introduced through `assign` or `actual_param` becomes a `program_path` and concatenates only
   with model paths. Both of a bridge's paths are program paths, so a callee-side path written on a
   `bridge` composes one level less than the same path written on a `propagation`. *Unvalidated:*
   every JNI port is an empty-path formal, so nothing in the tree exercises this (§7).
3. **Sites must be fresh, and temporaries are keyed on the site.** Call-argument pseudo-variables
   are keyed on the site id, so a bridge that reused an existing site's id would alias its argument
   *n* to that call's argument *n* — a spurious bidirectional flow between two unrelated arguments.
   Every bridge mints a new site id. The temporaries of consequence 2 inherit the same requirement
   for the same reason: under `one-to-many` cardinality a single caller hosts several bridge sites,
   and a temporary named only for its callee index would merge their parameters. Name them
   `(site, index)`.
4. **Formals are synthesized on both sides.** A bodyless stub has no `formal_param` rows at all —
   the dex frontend sets parameters up only when it finds a code item — and both the `locals`
   seeding rule and the summary rule join on them, so the bridge emits them itself for every mapped
   port, exactly as `codegen_summary` does for modelled functions.

   The callee side gets the same treatment, and for the same reason. **The port map is the more
   authoritative statement of the callee's ABI**, and the cases where the two disagree are exactly
   the cases the author wrote the bridge for: Ghidra gives a function with no recovered prototype
   zero parameters; a varargs or hand-written stub declares fewer parameters than it takes; a
   symbol-only import has no body to recover anything from. Emitting the row asserts the parameter
   the model names, so an argument that crosses the bridge lands in the callee's `Argument(n)`
   whatever the disassembler recovered. The earlier position — that inventing a row fabricates a
   parameter the disassembler never saw — traded a precise model for an imprecise frontend and lost
   the argument silently, which is the worse of the two failures. A synthesized formal that nothing
   in the body reads is inert: it produces a `locals` seed and no flow beyond it, which is
   precisely the state that withholding the row already produces. Trust the model.

   Only *positive* argument indices are ever actually missing. `codegen_program` emits the globals
   and return formals for every function it visits, so those two are present on any callee the
   frontend saw at all; the bridge pushes rows for every mapped port regardless, since a duplicate
   row is free and the uniform loop is what keeps the two sides symmetric.

   An arity mismatch is still worth reporting — as a *warning*, not as the reason a fact is
   missing. It names both functions and both arities and tells the author that the callee's
   recovered prototype disagrees with the model, which nearly always means something else needs
   fixing too: a missing Ghidra prototype, a wrong port index, or a map written against a different
   build. The taint edge is emitted either way, and the warning is what says how far it will
   actually travel. (What ships in `jni::emit_bridge` today is the caller-side half plus this
   warning; extending it to the callee side is the change this consequence calls for.)
5. **Return and globals are ports like any other, and the return arity is asymmetric.** A Java
   function has return arity 2 (`-1` normal, `-2` exception); a native function has one. Only the
   normal return is mappable — a JNI implementation cannot throw into the second — so a port map's
   `Return` means `-1`, and the exception return is deliberately unmapped. The globals
   pseudo-parameter (`GLOBALS_INDEX`) must be mapped explicitly or heap flows do not cross the
   boundary at all; the JNI pass maps it unconditionally, and `JniFlow` — taint in through one
   native function, out through a different one via a native global — is the case that fails
   without it.

### 2.1 Where the edge attaches

**Inside the matched method, and only there.** `find: methods` mints the site *in* the matched
(bodyless) function. The stub thereby acquires a real summary, and every callsite of it anywhere in
the program composes with that summary for free — one edge per bridged method, not one per call.
This is the JNI case, and it is what shipped.

An earlier draft offered a second mode, `find: callsites`, synthesizing an edge at each matched call
site from the caller's own actuals. **It is removed, and it could not have worked.** `call` is an
EDB relation — it never appears in a rule head — and indirect and virtual dispatch are resolved
*inside* the fixpoint, through `resolvent` and `context_assign` (`index_engine/mod.rs`). A pass
running at `apply_bridges` time therefore sees only the statically emitted call rows. The sites a
callsite bridge would most want are exactly the ones that are not there yet: a call through a table
field, a `dlsym`'d pointer, a virtual dispatch. It would have bridged the easy sites, missed the
motivating ones, and reported success — §7's dominant failure mode with a mode of its own.

Attaching in the method has no such hole, because it does not enumerate callers at all: it produces
a summary, and the fixpoint applies that summary to whatever call sites it later discovers.

Nothing is lost, because the case that motivated callsite mode has a function node to attach to. The
Lua frontend publishes an **externals** column on `VirtualMethodTable::Lua` — the set of called
names minus the names the import defined (`languages/lua/mod.rs`) — so a C function bound through a
`luaL_Reg` table already appears as a matchable, bodyless callee, exactly like a Dex `native`
method. That is the same prerequisite §4.2 states for any side a bridge matches on, and for Lua it
is already met.

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
  "find": "methods",                  // side A (the call side); the only mode — §2.1
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
verbatim: `any_of`/`not`, `qualified-id`, and the unknown-field/unknown-constraint hard errors.

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

**Callee-side paths (the Lua shape).** The callee takes its arguments off an interpreter stack
rather than positionally, so all three ports land on the same callee parameter at different
sub-paths. `mylib.add` is matchable as an *external* of the Lua import (§2.1) — a called name the
import never defined — which is what gives the bridge a method to attach to:

```jsonc
{
  "find": "methods",
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

Emission gives this bridge one temporary — the callee's `Argument(0)`, the `lua_State *` — written
at `.stack.[1]` and `.stack.[2]` on the way in and read at `.stack.[-1]` on the way out. That the
three ports share a callee index and stay unaliased is the whole point of the temporary (§2,
consequence 2); an `actual_param`-only encoding would have collapsed them and connected the two
arguments and the return to each other. This is the case to build first: it is the only mechanism
the JNI pass left untested, and a surprise here should still be able to change the design.

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
`{"find": "methods", "where": …}` and run it over the `ProgramMatchIndex`es whose scope the side's
`in` admits. `find` is a constant here — it is `methods` on both sides (§2.1), so `SideSpec` does
not carry it and the loader rejects any other value on a generator carrying a `bridge`. There must not be a second implementation of `where`; that is how `signature_match` ends up
meaning two different things in two places.

The two result sets are then paired per `cardinality`. The report carries per-spec match and pair
counts, and — following `LinkStats` — is logged at `info` even when nothing went wrong, since a
bridge that did not fire is otherwise indistinguishable from an app with no cross-language flow.
Cardinality violations, and empty matches under `on-unmatched: "error"`, are hard errors carrying
the `(file, generator index)` used by every other loader message.

### 4.4 Emission

One shape, for each pair `(a, b)` of function ids, since `find: methods` is the only mode. Read a
port as *"the caller's `from` vertex is the callee's `to` vertex"*, and the emission as the three
steps that make that true: name the callee's parameter locally, wire the caller's port to it, pass
it.

```rust
let site = source_info.add_insn_site(a);           // fresh site id — never reuse
facts.call.push((site.into(), b));

// 1. One temporary per distinct *callee* index in the port map, plus the implicit globals pair.
//    Keyed on (site, index): `one-to-many` cardinality puts several bridge sites in one caller,
//    and a temporary named only for its index would merge their parameters (§2, consequence 3).
for n in callee_indices {
    let t = FlowVariable::local(intern(&format!("$bridge{site}#{n}")));
    // 3. Pass it. The temporary IS the callee's parameter, so it goes whole.
    facts.actual_param.push((site.into(), n, FlowVertex(t, Path::empty())));
    facts.formal_param.push((b, FlowVariable::formal_index(n), ByRef));
}

// 2. Wire each port to a sub-path of its callee's temporary, one direction per `assign`.
for port in ports {
    let caller = FlowVertex(FlowVariable::formal_index(port.from.index), port.from.path);
    let callee = FlowVertex(temp_for(site, port.to.index), port.to.path);
    if port.direction.inward()  { facts.assign.push((a, callee.clone(), caller.clone())); }
    if port.direction.outward() { facts.assign.push((a, caller, callee)); }
    facts.formal_param.push((a, FlowVariable::formal_index(port.from.index), ByRef));
}
```

No `facts.paths` push anywhere. `program_paths` is seeded from both endpoints of every `assign` and
from every `actual_param` vertex, so `from_path` and `to_path` register themselves (§2, consequence
2), and the temporary means no *composite* of the two ever has to be registered at all.

The temporaries need no `formal_param` row: `locals` seeds from formals only, and a temporary is a
conduit rather than a source. The `formal_param` rows on **both** `a` and `b` do matter (§2,
consequence 4) — either function may declare fewer parameters than the port map names, a bodyless
stub declaring none and a prototype-less binary function declaring none — and the engine seeds
`locals` from formals and joins the summary rule on them. The out-direction assign writing to `a`'s
*formal* is what makes `a` summarizable, which is in turn what lets every caller of `a`, including
the ones hybrid inlining resolves later, pick the bridge up (§2.1).

Synthesizing side B's formals does not make an incomplete prototype harmless, so still check `b`'s
arity against the highest mapped callee index and warn when it falls short, naming both functions
and both numbers. The argument *is* delivered to `b`'s `Argument(n)`; a body the disassembler never
connected to that parameter will not carry it further. The warning points at the prototype that
needs recovering, rather than reporting a fact the bridge declined to emit.

**The degenerate case is what already ships.** A port with an empty `to_path` and `direction: both`
— every JNI port — expands to `t_n = formal_k.from_path`, its converse, and the `actual_param`.
That has the same reachability as the direct `actual_param(site, n, FlowVertex(formal_k,
from_path))` that `jni::emit_bridge` writes today, one copy-hop longer. Special-case it to emit the
direct row: it keeps the shipped JNI fact shape byte-identical, which `languages/jni/tests.rs`
asserts on, and it keeps the common case free of a variable and two rows per port. The general path
is unaffected either way.

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
whose recovered arity is short of the port map. The second of those stays a warning and never
becomes a dropped fact (§2, consequence 4): the bridge asserts the parameters the model names, and
the warning tells the author the callee's prototype needs recovering before taint will travel past
them.

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

**Callee-side paths on a bridge are unvalidated** (§2, consequence 2) — the JNI pass needed none.
Callee-side paths on a *propagation* are routine and well exercised; what is untried is carrying one
across a bridge, where it travels as an `assign` onto a temporary and lands in the program-path
bucket rather than the model-path one. The Lua example in §3.3 is the first thing that exercises it
and should be built early enough that a surprise there can still change the design.

**A deep callee summary path may not clear the propagation gate on the way out.** Suppose `b`'s
summary writes `Argument(n).to_path.f` while the bridge's port reads `t_n.to_path`. Propagation
derives the caller vertex `formal_k.from_path.f` and gates it on `paths(from_path.f)`. `from_path`
is a program path and `to_path.f` is the model path; `f` *alone* is in neither set, so the one-level
concat rules (`model_paths × program_paths`) do not obviously produce `from_path.f`. This is not
bridge-specific — an ordinary modelled call has the same shape — but a non-empty `to_path` is what
makes it reachable in practice, and it is the kind of gap that manifests as a missing flow rather
than an error. Write the test before assuming it works.

## 8. Verification approach

- **Parse/validate, no program needed.** Unknown key at generator level, at `model` level, and
  inside `bridge`; missing `to`; `Argument(*)` in a port map; cardinality violation; empty match
  under each `on-unmatched` setting; and `"find": "callsites"` on a generator carrying a `bridge`,
  which must be a hard error pointing at §2.1 rather than a silently ignored key. For `in`: `language` and `languages` given together, an empty
  `languages`, an unrecognized `ArtifactLanguage`, and — the one positive case — that
  `{"language": "dex"}` and `{"languages": ["dex"]}` parse to the identical `ProgramScope`, so the
  two spellings cannot drift.
- **Matching.** A two-`ProgramMatchIndex` fixture asserting the side-A and side-B match sets and the
  resulting pairs directly, without touching the fact base.
- **Emission.** Given a pair and a port map, assert the exact `call` / `actual_param` /
  `formal_param` / `assign` rows, including the implicit globals pair, that the site id is fresh,
  and that a `formal_param` row is emitted on *both* sides for every mapped port, including ports
  past the callee's recovered arity (§2, consequence 4). Pair that with a case where the callee's
  arity is short: the rows are still emitted and the warning still fires. Assert no `facts.paths`
  rows at all: registration is the engine's job (§2). `languages/jni/tests.rs` is the model for this
  layer; extend it rather than starting a new fixture style.
- **Temporaries, specifically** (§2, consequence 2). Three assertions that no other test implies:
  that two ports sharing a callee index share *one* temporary and are written at their own
  sub-paths, so the caller ports do not alias — the §3.3 Lua map is the fixture, and the negative is
  that taint on `Argument(0)` must not reach `Argument(1)`; that `direction: in` emits one `assign`
  and not its converse; and that two bridge sites in the same caller get distinct temporaries under
  `one-to-many` cardinality (§2, consequence 3). Add a JNI-shape case asserting the degenerate
  collapse leaves `jni::emit_bridge`'s existing rows unchanged.
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
  mapping-aware summary-instantiation rule). Cleanest conceptually, and it was the fallback for
  callee-side paths — but the temporary (§2, consequence 2) gets them out of relations that already
  exist, and adding a rule to the main fixpoint carries a cost nothing now justifies. The
  observation that killed it: a mapping-aware rule would be re-deriving, inside the fixpoint, what
  one local variable and two assignments state directly at fact time.
- **Drop `to_path` from the row and register the concatenation in `facts.paths`.** Tempting, because
  summary instantiation *does* deliver the callee's paths into the caller's context at every site
  without anyone enumerating sites, and the only thing stopping the taint is the propagation gate.
  It fails on two counts: it means structural sharing (the caller's port *is* the callee's whole
  parameter, with sub-structure visible through it) rather than relabeling, so several ports on one
  callee index collapse into one pseudo-variable and alias; and the paths it would have to register
  are not one composite but the open family `from_path ++ to_path ++ q` over everything the callee's
  summary adds. The temporary keeps every path literal and needs no registration at all.
