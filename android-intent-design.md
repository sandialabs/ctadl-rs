# Android Intent support - DO-NOT-MERGE

A design for teaching CTADL the Intent surface of an Android app: the components declared in
`AndroidManifest.xml`, the intent filters that make them reachable, and the data flows that run
between components through intents.

## 1. The problem

CTADL imports an APK by reading its `classes*.dex` entries and nothing else. It therefore sees an
Android app as a pile of classes with no relationships between them beyond calls. Two things are
invisible:

- **The app's boundary.** Which components another app -- or `adb shell am start` -- can reach is
  declared in the manifest, not in the code. Without it, the components and the routes into them
  are simply not represented.
- **Delivery.** `startActivity(intent)` in one component and `getIntent()` in another are a data
  flow, but no call connects them. The flow stops at the `startActivity` call.

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
work around a whole-program reachability model and has no counterpart here. One thing that rule
bought does need a counterpart: the framework, not the app, calls lifecycle methods, so a flow
delivered into `onCreate` alone is invisible to the rest of the component. Phase 3 handles that
by bridging into every lifecycle method rather than reintroducing whole-program reachability.

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

**The payoff, with no dataflow work at all.** As soon as the facts exist, `ctadl inspect` can
report the app's component surface: every component with `android:exported="true"`, every
component exported by default because it carries an intent filter (below API 31), and the
`android:permission` guard on each -- a component behind a signature-level permission is exported
in name only. The joins Phase 3 runs later are exercised here for free, which is where the
name-normalization function proves itself against a real manifest. Treat the inspect report as
Phase 1's deliverable, not as a side effect.

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
length. The declared type is still good for one thing: exclusion.
`LocalBroadcastManager.sendBroadcast` delivers only to receivers registered on that manager,
never to manifest receivers, so pairing it against the manifest fabricates links; skip send sites
whose declared receiver type is `LocalBroadcastManager` (both the `androidx` and
`android.support` descriptors).

**Receive sites.** The manifest names a component *class*; two resolution steps turn that into
functions to bridge into.

- *Hierarchy.* Components inherit entry methods more often than they override them, and the
  framework's implementations are not in the dex. Walk up from the component class and bridge to
  the nearest app-defined override of each method in the table below; where no override exists in
  app code, there is nothing to bridge into and the method is skipped. This is also what makes
  `IntentService` work at all: a subclass overrides `onHandleIntent`, never `onStartCommand`, so
  framework-mediated indirections get their own rows.
- *Aliases.* An `<activity-alias>` is a component in its own right -- its own name, its own
  exported status, its own filters -- that delivers to its `android:targetActivity`. Fold each
  alias's filters into the target activity's filter set; the previous ctadl's pseudo-filter
  treatment (section 7) extends to this without new machinery.

| tag | methods bridged | where the intent lands |
| --- | --- | --- |
| `<activity>` | every lifecycle override: `onCreate`, `onStart`, `onRestart`, `onResume`, `onPause`, `onStop`, `onDestroy`, `onNewIntent` | `Argument(0).intent`; for `onNewIntent`, also `Argument(1)` |
| `<receiver>` | `onReceive` | `Argument(2)` |
| `<service>` | `onStartCommand`, `onBind`; `onHandleIntent` / `onHandleWork` for `IntentService` / `JobIntentService` subclasses | the intent parameter, `Argument(1)` |

**Why every lifecycle override, not just `onCreate`.** The analysis is compositional: nothing in
the app calls `onResume` -- the framework does -- so an intent delivered only into `onCreate`'s
scope is invisible everywhere else in the component. Bridging each lifecycle override delivers
the same intent vertex into each method's `this`, and ordinary summary propagation carries it
into anything those methods call. The cost is a handful of bridge sites per pair instead of one.

`.intent` is a synthetic field: `Activity`'s real backing field lives in the framework, outside
the dex, so the pass invents the symbol, and a pair of model entries must agree on it --
`getIntent` as `Argument(0).intent -> Return`, `setIntent` as `Argument(1) ->
Argument(0).intent`. Add both to `java-index.jsonl` alongside the extras entries below.

**Extras.** Without model entries for `putExtra`, the value never reaches the intent at all, so a
delivered intent carries nothing interesting; the extras model belongs with this phase rather than
after it. With Phase 2's constants in hand, model the map key-precisely from the start --
`putExtra(key, v)` as `Argument(2) -> Argument(0).extras.<key>` plus `Argument(0) -> Return` for the
builder chain, the `get*Extra` family as `Argument(0).extras.<key> -> Return`, and `Intent.<init>`,
`setAction`, `setData`, `putExtras` and the `Bundle` `put*`/`get*` pairs likewise -- falling back to
a lumped `.extras` field only where the key is not a recovered constant. Do not ship the lumped form
on its own as an interim step: it conflates every key with every other, and the constants arrive
soon enough that those false positives would outlive their usefulness. `java-index.jsonl` already
carries a few Intent and Bundle entries -- `getExtras` as `Argument(0) -> Return`, the Bundle `get*`
family as `Argument(*) -> Return` -- to reconcile against.

Three cautions. `parent` matches the class declared at the invoke site rather than the runtime type,
so list `Intent` alongside anything callers actually write. A named-field port is not portable
between the dex and jvm frontends, so `extras` commits us to dex. And the previous ctadl encoded
keys as `ord()` numbers to avoid `.` characters inside key strings; check how
`facts::Path::from_accesses` handles a `Symbol` containing a dot and escape rather than mangle.

**Pairing.** Three cases, in descending confidence:

- *Explicit.* `new Intent(ctx, Foo.class)`, `setClass`, `setClassName`, `setComponent` name the
  target class outright. Low fan-out, high confidence. This is the default.
- *Implicit.* A constant action string on the intent joins against a manifest
  `<intent-filter><action android:name=...>`. Correct, but prone to fan-out: every sender of
  `ACTION_VIEW` links to every activity that filters it.
- *Unresolved.* No constant action was recovered. The sound answer links the send to every exported
  component; the useful answer links it to nothing. Put this behind a flag, defaulted off.

**Emission.** The JNI pass is an existence proof that an index-time pass can emit bridge facts,
not a template: its `emit_bridge` handles only whole-parameter ports, and `Argument(0).intent` is
a sub-path of a parameter. The general shape already exists in the declarative-bridge emitter
(`ctadl-ascent/src/codegen/model_matches.rs:343`): a fresh site, a `call` row, a direct
`actual_param` where a port names a whole parameter, and -- where a port names a sub-path -- one
temporary per callee index passed whole, with `assign` rows writing the sub-path. The intent pass
should share that emitter rather than grow a third copy. The site must be fresh per pair either
way: call-argument pseudo-variables key on the instruction id, so reusing a site would alias
unrelated intents to each other.

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
2. **Implicit-intent fan-out tolerance.** This decides whether Phase 3 produces a usable result set
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
