# Android Intent support - DO-NOT-MERGE

A design for teaching CTADL the Intent surface of an Android app: the components declared in
`AndroidManifest.xml`, the intent filters that make them reachable, and the data flows that run
between components through intents.

## 1. The problem

CTADL imports an APK by reading its `classes*.dex` entries and nothing else. It therefore sees an
Android app as a pile of classes with no relationships between them beyond calls. Two things are
invisible:

- **The app's boundary.** Which components another app -- or `adb shell am start` -- can reach is
  declared in the manifest, not in the code. Without it, nothing marks attacker-controlled input as
  attacker-controlled.
- **Delivery.** `startActivity(intent)` in one component and `getIntent()` in another are a data
  flow, but no call connects them. Taint stops at the `startActivity` call.

Both gaps are closed by the same artifact, `AndroidManifest.xml`, which CTADL currently never
opens. The only mention of it in the tree is a test fixture's magic bytes
(`dex-reader/src/apk.rs:599`).

## 2. What already exists

Worth stating plainly, because it decides most of the design.

**Getting the bytes is solved.** `dex_reader::apk::read_bundle_entry(path, entry_name)` reads an
arbitrary entry out of an APK by name. `APKParser::new` keeps only the decompressed DEX buffers and
drops the archive, so manifest reading follows the same free-function pattern the native-library
readers already use.

**Cross-boundary joins have one home: bridges.** Mechanically a bridge is one `call` row plus one
`actual_param` per port between two functions no static call connects -- see
`emit_bridge` in `ctadl-ascent/src/languages/jni.rs:868`, which is 25 lines and knows nothing about
JNI. The JNI pass is the built-in instance; `model.bridge`
(`ctadl-ascent/src/models/spec.rs:407`) is the declarative one. The mechanism was built for
cross-language flow, but it is not restricted to it: `docs/model-generators.md:284` records that
`forward_call`, the same-program special case, was removed from the schema in favor of `bridge`.
Intent delivery is a same-language use of the same shape.

**There is no whole-program entrypoint notion.** The analysis is compositional and summary-based;
every function is analyzed. The previous ctadl's `StatementReachableFromOnCreate` rule existed to
work around a whole-program reachability model and has no counterpart here.

## 3. The blocking gap: constants are dropped

The dex frontend builds string constants -- `Instruction::ConstString` becomes `Exp::new_str`
(`ctadl-ascent/src/languages/dex/mod.rs:640`), `ConstClass` becomes `Exp::new_str` of the type
descriptor (`:655`) -- but codegen maps every expression that is not a variable or access path to
no vertex at all (`ctadl-ascent/src/codegen/mod.rs:848-851`). No constant reaches the fact base.

Every part of intent resolution needs them:

| what | comes from |
| --- | --- |
| implicit intent target | the action string on the intent, joined against manifest filters |
| explicit intent target | the class named by `new Intent(ctx, Foo.class)` or `setClassName` |
| which extra was written | the key argument of `putExtra` |

The previous ctadl carried a `_VarIsConst` relation for exactly this. Note also that `const-class`
produces an `Exp::Str`, not an `Exp::ObjectRef(CallObject::JavaObject(_))` -- only `NewInstance`
produces the latter (`dex/mod.rs:664`), and only the latter feeds `call_target_assign`. So explicit
intents do not resolve for free either.

Manifest parsing on its own therefore buys less than it appears to. Section 5 arranges the work so
that the part which does not depend on constants ships first.

## 4. Design

Three layers, each useful before the next exists.

1. **Manifest to facts.** Decode the binary XML, emit a node/attribute/child triple store, persist
   it with the import.
2. **Constants to facts.** A resolved map from vertex to literal, computed on the IR.
3. **The intent linking pass.** Join send sites to receiver entry methods using (1) and (2),
   emitting bridge-shaped facts.

## 5. Phases

### Phase 0 -- Extras as an opaque container

Model files only; no new code. Add `Intent` and `Bundle` entries to
`ctadl-ascent/src/models/defaults/java-index.jsonl`, treating the extras map as a single field:

- `putExtra(key, v)`: `Argument(2) -> Argument(0).extras`, plus `Argument(0) -> Return` for the
  builder chain
- `getStringExtra(key)` and the rest of the `get*Extra` family: `Argument(0).extras -> Return`
- `Intent.<init>`, `setAction`, `setData`, `putExtras`, and the `Bundle` `put*`/`get*` pairs
  likewise

This catches put-then-get on one intent object within a component today, at the cost of conflating
every key with every other. It is a day's work, it needs no new machinery, and it establishes the
path vocabulary that Phase 4 refines. Do it first.

Two cautions carried over from the comments at the top of `java-index.jsonl`: `parent` matches the
class declared at the invoke site rather than the runtime type, so list `Intent` alongside anything
callers actually write; and a named-field port is not portable between the dex and jvm frontends,
so `extras` here commits us to dex.

### Phase 1 -- Parse the manifest

**Decoder.** A new `dex-reader/src/axml.rs`. The subset of Android binary XML needed is narrow:
the header (`0x00080003`), the string pool, the resource-map chunk, and start/end element records
with their attributes. Attribute names arrive as resource IDs -- `android:name` is `0x01010003` --
with string-pool names alongside; values are string references, integers, or booleans. Roughly
300-500 lines.

Vendoring it matches the house style, since the tree already vendors its own dex and JVM readers,
and it avoids taking a dependency for a format that does not change. The cost is owning format
code. Detect plain-text XML by magic bytes and fall back to a text parser; unbuilt manifests are
rare but they exist.

**Fact shape.** Keep the previous ctadl's triple store rather than inventing a typed schema:

```
manifest_node(node_id, tag)
manifest_node_child(parent_id, child_id)
manifest_node_attr(node_id, key, value)
```

It records the manifest verbatim, it is cheap, and it answers questions we have not asked yet --
`android:exported`, `<provider>` authorities, `<permission>`, `android:process` -- without a schema
migration. Derive typed views in Rust at index time, where changing them costs nothing.

**Name normalization.** `android:name=".MainActivity"` is relative to the `package` attribute of
`<manifest>`, and `com.foo.Bar` must become `Lcom/foo/Bar;` before it will join against a dex class
name. Put this in one function with its own tests. Getting it wrong produces zero matches and no
error, which is indistinguishable from an app with no components.

**Persistence.** Write the tables into the import directory alongside the serialized program and
VMT, and bump `IMPORT_FORMAT_VERSION` (`ctadl-ascent/src/project.rs:75`) so a stale import fails
with a clear message rather than a silent absence of manifest facts.

**Split APKs.** The manifest lives in the base APK of a bundle. Check how
`ctadl-ascent/src/languages/xapk.rs` selects artifacts and make sure the base manifest attaches to
the bundle as a whole, not to whichever split happened to carry it.

**The payoff, with no dataflow work at all.** Two things become possible as soon as the facts
exist:

- `ctadl inspect` reports the app's exported surface: every component with
  `android:exported="true"`, plus every component with an intent filter, which is exported by
  default below API 31.
- A shipped model file marks the intent parameter of each exported component's entry method as a
  **taint source** -- the result of `getIntent()` in `onCreate`, `Argument(2)` of `onReceive`, the
  intent argument of `onStartCommand`. That is the standard "attacker-controlled input crosses the
  app boundary" query, and it needs the manifest and nothing else.

This source story is the highest value per unit of work in the whole design. Treat it as Phase 1's
deliverable, not as a side effect.

### Phase 2 -- Constant strings as facts

Add a relation of roughly `const_str(FunctionId, FlowVariable, Path, Symbol)`, emitted where
codegen currently discards `Exp::Str`.

Resolve it on the IR, before codegen, rather than inside the Ascent fixpoint. The IR already
carries SSA and copy propagation (`ctadl-ir/src/ssa/copy_prop`), so an intraprocedural pass
answering "which literal reaches this argument" is direct, and it keeps a large and mostly
uninteresting relation out of the fixpoint. What the linking pass wants is a lookup, not a
dataflow relation.

Scope it deliberately: one constant per vertex, intraprocedural, no concatenation. The previous
ctadl added a `substring` special case to make a single test pass; skip it. If `StringBuilder`
chains turn out to matter, that is a separate decision with its own cost, and it should be made on
evidence from real apps.

Resolve `ConstClass` here too. That is what gives Phase 3 its explicit-intent targets.

### Phase 3 -- The intent linking pass

A built-in index-time pass, structured like `ctadl-ascent/src/languages/jni.rs`.

**Send sites.** Calls to `startActivity`, `startActivityForResult`, `sendBroadcast`,
`sendOrderedBroadcast`, `startService`, `bindService`. Match on the method's simple name. The
receiver's declared type varies too much to enumerate -- `Activity`, `Context`, `ContextWrapper`,
`Fragment` -- which is the same problem the `iterator` entry in `java-index.jsonl` documents at
length.

**Receive sites.** The component's entry method, selected by manifest tag:

| tag | entry method | where the intent is |
| --- | --- | --- |
| `<activity>` | `onCreate` | the result of `getIntent()` |
| `<receiver>` | `onReceive` | `Argument(2)` |
| `<service>` | `onStartCommand`, `onBind` | the intent argument |

**Pairing.** Three cases, in descending confidence:

- *Explicit.* `new Intent(ctx, Foo.class)`, `setClass`, `setClassName`, `setComponent` name the
  target class outright. Low fan-out, high confidence. This is the default.
- *Implicit.* A constant action string on the intent joins against a manifest
  `<intent-filter><action android:name=...>`. Correct, but prone to fan-out: every sender of
  `ACTION_VIEW` links to every activity that filters it.
- *Unresolved.* No constant action was recovered. The sound answer links the send to every exported
  component; the useful answer links it to nothing. Put this behind a flag, defaulted off.

**Emission.** Exactly what `emit_bridge` does: a fresh instruction site in the sending function, a
`call` row targeting the receiver's entry method, and `actual_param` rows carrying the intent
vertex into the receiver's intent parameter. The site must be fresh per pair -- call-argument
pseudo-variables key on the instruction id, so reusing a site would alias unrelated intents to each
other.

**Reporting.** Log a per-pass count line at `info`, the way the JNI bridge does
(`jni.rs:583`). A mis-paired bridge yields fewer flows rather than an error, which reads exactly
like a clean app; the count is the only thing that catches it.

**Why a built-in pass and not generated `model.bridge` entries.** Generated entries would need no
new pass code and would let users override pairings by hand. But a bridge's two sides are selected
by static `where` constraints, and intent pairing depends on constants recovered from the program,
which the model language cannot express. So: a built-in pass, with declarative `bridge` remaining
available for hand-written overrides of what the pass gets wrong.

### Phase 4 -- Precision

In rough order of value:

- **Key-precise extras.** Replace Phase 0's lumped `.extras` with `.extras.<key>` where the key is
  constant, falling back to the lumped field otherwise. The previous ctadl encoded keys as `ord()`
  numbers to avoid `.` characters inside key strings; check how `facts::Path::from_accesses`
  handles a `Symbol` containing a dot and escape rather than mangle.
- **Result-back flows.** `setResult(code, intent)` in the callee pairing with `onActivityResult` in
  the caller. The previous ctadl's rule for this was written and marked untested; treat it as new
  work rather than a port.
- **Programmatic receivers.** `registerReceiver(receiver, new IntentFilter(action))` registers a
  filter that never appears in the manifest.
- **`PendingIntent`**, and `<provider>` authorities if content-provider flows matter.

## 6. Open decisions

1. **Vendored AXML decoder or a crate.** The recommendation is vendored, for consistency with the
   existing readers and to avoid a dependency on a frozen format. The cost is roughly 400 lines of
   format code to own.
2. **Which Phase 1 payoff leads.** The exported-component taint-source story and the
   cross-component linking story are separable, and the source story lands far sooner. Which one
   answers the question you actually have?
3. **Implicit-intent fan-out tolerance.** This decides whether Phase 3 produces a usable result set
   or drowns a real app in links. Worth measuring on a real APK before committing to the default.

## 7. Relationship to the previous ctadl

`~/proj/ctadl` carried this in Souffle at `src/ctadl/souffle-logic/jadx/android-intents.dl`, over
manifest facts a jadx plugin emitted. What is worth taking:

- The `_ManifestNode` / `_ManifestNodeChild` / `_ManifestNodeAttr` triple store (section 5, Phase 1).
- The `ActivityHasIntentFilter` rule, including its treatment of a component's own name as a
  pseudo-filter so that explicit intents match through the same join.
- The separation of `IntentSend` / `IntentRecv` by communication type, so an activity send cannot
  pair with a broadcast receive.

What is not worth taking:

- The per-signature enumeration of every `putExtra` and `get*Extra` overload -- about 250 lines of
  near-identical rules. The model generator system expresses this in a handful of entries.
- `StatementReachableFromOnCreate`. It exists to bridge a whole-program reachability model that
  this analyzer does not have.
- The `substring` special case in `VarReferencesString`.
- The `ord()` key encoding, which is a workaround for Souffle's flat symbol paths.
