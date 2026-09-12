# Criticism of Android Intent Design and Implementation Plan - DO-NOT-MERGE

This review covers the current `android-intent-design.md` and `implementation-plan.md` against the
repository as it stands. The iteration is materially better than a pass that rewrites IR or tries to
precompute intent pairs outside the index, but several assumptions still need to be made explicit or
changed before implementation starts.

## 1. Send-site discovery from `facts.call` is not reliable enough

The design says the intent linker can find send sites after the import loop by scanning `facts.call`
and resolving each callee through the `IdMap`. That is only safe for call sites that codegen already
resolved to a concrete callee. For Java calls under the current `Mixed` strategy, codegen emits a
`call` row only when CHA has exactly one resolvent; ambiguous Java calls go to `callee_info`, and no
static `call` row is emitted (`ctadl-ascent/src/codegen/mod.rs:535-557`). If an Android framework
send has zero or multiple resolvents in the VMT, the proposed scan silently misses the send site.

This is not just a theoretical mismatch in wording. The plan also says to skip
`LocalBroadcastManager.sendBroadcast` by declared receiver type, but `facts.call` does not retain the
declared receiver class; the `call` row is only `(site, target)`, and `callee_info` retains only a
Java dispatch key of simple name plus descriptor. The declared class exists in the IR `CallStyle`,
which the design explicitly avoids retaining.

The linker needs either an observer during import that records Java call sites before the IR is
dropped, or a new codegen fact that preserves the declared class, method name, descriptor, site, and
argument positions for Java calls. Relying on resolved `call` rows makes the feature depend on the
call-resolution strategy and loses exactly the unresolved framework calls an ICC pass should be
finding.

## 2. `Intent.createChooser` is incorrectly listed as a send site

`Intent.createChooser(Intent, CharSequence)` is a factory method, not an ICC send. The design also
correctly models it as `Argument(0) -> Return`, because the real send is normally
`startActivity(Intent.createChooser(target, title))`. But the Phase 3 send-site table lists
`Intent.createChooser` as a send site with `Argument(0)` as the intent argument.

If implemented literally, this mints bridge sites and attempts manifest pairing at the chooser call
itself, before any `startActivity` exists. That creates false call edges from a framework factory to
receivers and double-counts or misclassifies chooser flows. The implementation plan omits
`createChooser` from the Phase 3 bullet list, which is better, but the design and plan need to agree:
`createChooser` belongs only in the static Intent API summary table, never in `intent_send`.

## 3. The intent-filter relation is too small for Android's matching rules

`intent_filter(Symbol, IntentKind, FunctionId)` only keys on action and kind. That will over-pair
real apps because Android intent resolution also considers categories, data URI scheme/host/path,
MIME type, and default-category rules. The design acknowledges data constraints in Phase 1, but the
engine relation that drives pairing has no columns for them.

The most important missing rule is `CATEGORY_DEFAULT` for activities. A manifest filter that lacks
`android.intent.category.DEFAULT` is generally not a target for an implicit `startActivity`, even if
the action matches. Conversely, launcher filters have `MAIN`/`LAUNCHER` and should not become
generic action matches for app sends. Data constraints matter for common actions such as `VIEW` and
`SEND`; joining only on action can fan out to every component that has a popular action string.

The implementation plan should not treat action-only implicit pairing as Phase 3 correctness. At
minimum, it should model and test category-default and MIME/data scheme constraints, or explicitly
label action-only matching as an early, over-approximating mode with fan-out reporting and a default
that keeps it off until measured.

## 4. Exported/non-exported semantics are under-specified for internal versus external sends

The docs mix two different questions: the app's external attack surface and flows between components
inside the same app. `android:exported="false"` blocks other apps, but it does not block an explicit
internal send from the same package. The current `intent_component` and `intent_filter` relations do
not carry enough context to distinguish an internal sender from an external entry.

This matters because Phase 4 asks for a negative case where an external send to a non-exported
component is not paired, while the pass being designed only scans app code send sites. There is no
external sender relation in the plan, no package/UID notion, and no exported flag in the pairing
rules. Either external-entry modelling is out of scope for the initial linker, or the manifest views
need to emit separate exported entry facts and the tests need to distinguish external attack-surface
reports from internal ICC flows.

## 5. Service kinds are conflated

The delivery table bridges services into `onStartCommand`, `onBind`, and IntentService-style handler
methods. That is not right for every send kind. `startService` and `startForegroundService` deliver
to started-service entry points; `bindService` delivers to `onBind`; `IntentService`/`JobIntentService`
handler behavior is framework-specific and should not be treated as a universal service delivery
target.

The `IntentKind` relation needs enough structure to separate started-service from bound-service
delivery. Otherwise a `bindService` call can falsely flow into `onStartCommand`, and a `startService`
call can falsely flow into `onBind`. The implementation plan currently groups them all under
"service argument positions" and should split the test matrix by send kind.

## 6. Derived `call` makes the index SCC larger than the design admits

The design says the cycle through `call` terminates trivially because delivery edges point away from
the sender's intent and nothing carries constants back to the send-site argument. That argument only
considers `const_reaches`. In the current engine, `call` also feeds hybrid inlining:
`critical_summary`, `resolvent`, `context_assign`, contextual summaries, and additional
`assign_like` rows all depend on `call` (`ctadl-ascent/src/index_engine/mod.rs:1206-1329`). Once
intent pairings derive `call`, those new calls participate in more than the five intent rules.

This may still be monotone and correct, but it is not a one-step local cycle. A derived lifecycle
call can instantiate summaries for receiver methods, contribute to hybrid-inlining critical-call
state, and change `assign_like` beyond the delivery edge. The implementation plan should include a
small fact-level test where an intent-derived call targets a function with summaries and/or virtual
calls, then assert both termination and the expected final `call`/`assign_like` shape. It should also
measure SCC time before and after deriving calls.

## 7. Final-call persistence is a cross-cutting migration, not a Phase 3 subtask

Moving `call.parquet` from pre-fixpoint `IndexFacts` to post-fixpoint `IndexResult` touches every
query and formatting path that currently reads `index_facts.call`. The current query builder uses
`index_facts.call` both for taint propagation and SARIF formatting (`ctadl-ascent/src/cli/mod.rs:633`
and `:655`). The design notes this, but the implementation plan buries it inside Phase 3 after the
pairing rules.

This migration should be a standalone precondition with tests before intent pairing is added. A
half-moved implementation can compile and run while querying a call graph without intent bridges.
The plan also needs an atomic-write story: `project.write_index_config()` is deliberately last, but
moving a formerly pre-index table to `IndexResult` creates a period where older consumers or failed
runs can see missing or stale `call.parquet` unless the version bump and write order are handled as
one change.

## 8. Manifest triples need typed values, namespaces, and resource identity

The proposed triple store is a good persistence shape, but the documented schema
`manifest_node_attr(node_id, key, value)` is too stringly for Android manifests. Attribute names may
come from resource IDs, namespaces matter, and values can be strings, booleans, integers, enum/flag
integers, or unresolved resource references. Phase 4 specifically depends on not confusing boolean
`0xffffffff` with a string and not defaulting unresolved `@0x...` references.

If the persisted table only stores normalized strings, the typed view cannot later tell whether
`android:enabled` was literal `false`, string `"false"`, or `@bool/foo`. The plan should define a
minimal typed value representation in the persisted facts, or add parallel columns for value kind,
raw data, namespace URI, and resource ID. Otherwise the triple store is not actually verbatim enough
to support the future typed views the design wants.

## 9. XAPK/base-manifest ownership is not mechanically resolved

The design says the base manifest should attach to the bundle as a whole, not whichever split
carried it. The current XAPK importer extracts and imports each split as its own APK sub-import, and
the parent bundle import is not itself a code-bearing program (`ctadl-ascent/src/languages/xapk.rs`).
The index loop observes per-import `program_info`; it is not obvious where a parent-level manifest
with no program body is loaded, how it is associated with the child DEX functions, or how multiple
split manifests are rejected or merged.

The implementation plan has a test for split selection, but it needs an implementation hook: either
the parent import owns Android manifest sidecar data and the linker explicitly loads parent metadata
for all sub-imports, or the base split owns it and the linker knows which split is base. Without that
mechanical choice, the XAPK requirement is aspirational.

## 10. Built-in Intent API rows are no longer user-controllable

Moving Intent API propagation out of `java-index.jsonl` fixes the spelling seam, but it also removes
the user's current escape hatch. Model-file rows can be omitted with `--no-default-models` or
replaced by custom models. Built-in Rust-emitted summaries are a union with user rows; users can add
more precision but cannot remove a coarse built-in row.

That matters for rows such as `Intent.<init>(Intent) -> Argument(0)` or broad builder-self returns.
If one of these is too imprecise for a user's corpus, adding a narrower model does not retract the
built-in one. The design should add an explicit control, for example `--no-android-intent-models` or
folding these rows under `--no-default-models`, and the implementation plan should test that the
flag actually suppresses the pass-emitted summaries.

## 11. Literal-value call arguments still lack `actual_param`, and the workaround is too narrow

The plan handles `putExtra("k", "literal")` by rooting the value side at `call_arg(insn, n)` when a
constant value has no `actual_param`. That solves extras, but the same missing `actual_param` shape
exists for every constant argument. Static API summaries such as `Intent.<init>(String)` and
`setAction(String)` instantiate over call-arg vertices; they need the constant fact on the
call-arg vertex, not an `actual_param`, so they are fine for `const_reaches`. But any later feature
or user model that expects constant arguments to appear as ordinary actuals will still see a hole.

The design should document this boundary: `const_str_assign` is not a general replacement for
`actual_param`, and only the new constant-propagation rules consume it. If the project wants
constant arguments to be visible to ordinary model summaries, that is a different codegen change.

## 12. Component-name matching misses package-relative string APIs

The design correctly normalizes manifest component names and `const-class` descriptors. It is much
less clear for explicit string APIs: `setClassName(String packageName, String className)`,
`ComponentName(String pkg, String cls)`, and class names beginning with `.` require joining two
arguments and applying package-relative normalization. The current plan only says to treat class-name
arguments as `.<component>` constants and normalize the manifest side to descriptors and dotted
spellings.

That will miss common cases where the component name is split across package and class parameters,
or where the class parameter is relative. It may also over-match a bare class-name suffix across
packages if the manifest side is normalized too generously. The implementation plan should call out
which overloads are supported initially and add explicit tests for `(Context, String)`,
`(String, String)`, relative class names, and package/class mismatch.

## 13. Action-only constants cannot represent categories or data values without more API rows

Phase 2 models `.<action>`, `.<component>`, `.<data>`, and `.<extras>`, but implicit matching only
reads `.<action>`. Real filters often require `addCategory`, `setType`, `setData`,
`setDataAndType`, `Uri.parse`, and `Intent.setPackage`. The design lists some static rows for data
but never connects them to the `intent_filter` join, and the implementation plan has no tests for
category/data-constrained implicit intents.

This means the first version will likely report impressive implicit-pair counts while being too
coarse to trust. If category/data matching is deferred, the reporting should label implicit pairs as
"action-only" and tests should include a negative case where the action matches but category or MIME
does not.

## 14. The lifecycle bridge is intentionally broad but not budgeted

Bridging an activity intent into every lifecycle override is a pragmatic answer to compositional
analysis, but it deliberately over-approximates temporal behavior. `onDestroy` and `onPause` are not
entry points that receive the launch intent in the same sense as `onCreate`/`onNewIntent`; the design
uses them to make data available to component methods that the framework calls later.

That may be acceptable, but it should be reported and tested as an approximation. Otherwise a taint
path that only exists because the same intent was injected into every lifecycle method will look as
concrete as a path through `onCreate`. The plan should include a count of bridge entry methods per
component and a test showing that broad lifecycle delivery does not explode duplicate SARIF paths for
a simple activity.

## 15. Phase 2 is too large for one implementation checkpoint

Phase 2 includes new codegen facts, new index-engine relations, an intent-frame gate, built-in API
summary emission, API diagnostics, removal of default model rows, per-site extras assignment
emission, path invariant checks, synthetic-call-site consumer audits, and an optional recovered-key
rule. That is too many interacting changes for a single phase boundary.

The smallest safer ordering is:

1. Add `const_str_assign` and prove constants reach call-arg vertices inside one function.
2. Add built-in static Intent API summaries and prove bodyless framework summary instantiation.
3. Remove the conflicting `java-index.jsonl` rows only after the built-in rows run for bare DEX/JAR
   as well as APK.
4. Add literal-key extras fact emission with consumer tests.
5. Defer non-literal recovered-key paths until after real-app measurements.

The current plan contains those tasks, but it does not enforce the dependency order strongly enough.

## 16. External ICC validation is both required and placed after "Phase 3 complete"

The design says Phase 3 should not be considered complete until the DroidBench ICC / ICC-Bench
harness exists and the first subset passes or is explicitly xfailed. The implementation plan puts
that harness in Phase 5, after Phase 4. That creates a milestone contradiction: either Phase 3 is
not complete until Phase 5 exists, or Phase 5 is follow-up validation.

For a feature this easy to overfit to `com.noto_54.apk`, the stricter interpretation is better.
Move the initial external ICC harness earlier, or rename the phase gates so the main feature cannot
be called complete before it has been checked against an independent suite.

## 17. Performance acceptance criteria are missing

The design adds relations that can affect the hottest part of the index: `const_reaches` propagates
over `assign_like`, recovered extras keys mint new `paths`, and derived `call` can feed the existing
hybrid-inlining rules. The docs mention counts and `#![measure_rule_times]`, but the implementation
plan does not define acceptable budgets.

Before this lands, the plan should record baseline and target bounds for at least `com.noto_54.apk`:
index wall time, peak memory, `paths` count, `assign_like` count, `const_reaches` count, and per-SCC
rule time. Without those, a correct implementation can still make ordinary APK analysis unusable.

## What Holds Up

Several parts of the iteration are sound and worth keeping:

- Manifest import as a first shippable surface is the right Phase 1 deliverable.
- The triple-store direction is better than prematurely freezing a typed manifest schema, provided
  the stored attributes keep value kind and resource identity.
- Constants as relations over the existing flow graph are better than a local def-chase.
- Per-site extras facts are the right replacement for a keyed `putExtra` summary; a summary keyed by
  callee cannot be key-precise.
- One bridge site per send site is the right freshness granularity; one site per pair is unnecessary
  and cannot be minted inside the fixpoint.
- Delivery as one-way `assign` rows avoids the unsound back-flow that symmetric `actual_param` edges
  would introduce.
- Persisting the final derived call graph is necessary if query-time taint propagation is expected
  to see intent bridges.

The main correction is to stop treating the current relation sketches as implementation-ready. The
feature needs one more tightening pass around call-site observation, Android filter semantics,
exported/internal distinctions, service kinds, and persistence migration before the checklist becomes
safe to execute.
