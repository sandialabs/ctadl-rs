# Android Intent Support Implementation Plan

This is the implementation checklist for the Android Intent support described in
`android-intent-design.md`. It intentionally omits most design reasoning and keeps the work ordered
around shippable increments and their validation gates.

## Phase 0 -- Preconditions and Test Harness Hooks

Goal: add the small testing surfaces needed before the feature-specific code becomes hard to debug.

1. Add a minimal index-engine regression proving summaries on bodyless Java/Dex framework methods
   instantiate into `assign_like`.
   - Construct or compile a tiny Java/Dex fixture that calls an external/framework-like method.
   - Insert a test summary and matching `formal_param` rows.
   - Assert the expected post-index `assign_like` row exists.

2. Add helper assertions for Android intent tests that do not rely on source lines.
   - Assert component and method identity in SARIF logical locations or directly from index facts.
   - Keep ordinary `expected_lines` tests unchanged.

3. Add a place for Android intent/ICC test cases in `xtask`.
   - Add a new case kind rather than forcing APK intent tests through existing line-based `Kind::Dex`.
   - Preserve `--filter` and frontend selection support.
   - Keep `checksarif` validation for any SARIF the new cases emit.

4. Move persisted calls to post-fixpoint output before adding intent-derived calls.
   - Add final `call` output to `IndexResult`.
   - Save `call.parquet` from `IndexResult`, not pre-fixpoint `IndexFacts`.
   - Make query, inspect, SARIF formatting, graphviz, and flowy helper paths load the final call
     table consistently.
   - Bump the relevant format version and make the write ordering atomic enough that a failed index
     cannot leave a stamped complete project with a missing final call table.
   - Add a migration regression where a synthetic derived call is visible to query and formatting.

5. Add an index-engine regression for derived calls feeding the existing SCC.
   - Build a small fact-level case where a derived `call` targets a function with a summary.
   - Add a second case where a derived `call` participates in virtual-call or hybrid-inlining state.
   - Assert termination, final `call` contents, expected `assign_like`, and no duplicate bridge-site
     aliasing.

## Phase 1 -- Manifest Import and Inspect Surface

Goal: read `AndroidManifest.xml`, persist it, and report component surface before any dataflow work.

1. Add manifest extraction for APK imports.
   - Use existing APK/bundle entry-reading helpers where possible.
   - Read `AndroidManifest.xml` from APKs.
   - For XAPK/split APKs, associate the base manifest with the bundle/project, not an arbitrary split.

2. Add an Android XML decoding adapter.
   - Use a maintained Android binary XML crate behind a small CTADL-owned adapter.
   - Support binary XML values needed for manifests: strings, integers, booleans, and resource references.
   - Detect plain-text XML by magic bytes and parse it as text XML.
   - Emit CTADL-owned manifest tree data rather than exposing crate-specific structures.

3. Persist manifest triples with imports.
   - Add persisted tables equivalent to:
     - `manifest_node(node_id, tag)`
     - `manifest_node_child(parent_id, child_id)`
     - `manifest_node_attr(node_id, key, value)`
   - Bump `IMPORT_FORMAT_VERSION`.
   - Ensure stale imports fail clearly rather than silently omitting manifest data.

4. Implement typed manifest views at index/inspect time.
   - Normalize component names to dex descriptors in one tested function.
   - Handle relative names like `.MainActivity` using the manifest package.
   - Preserve unresolved resource references as unresolved values.
   - Do not skip `enabled="false"` components; report disabled state but keep them in pairing inputs.
   - Record `android:exported`, default-exported components with filters where applicable, permissions,
     aliases, filters, actions, categories, and data constraints at least enough for current intent
     matching.

5. Add `ctadl inspect` output for Android component surface.
   - List exported components.
   - List permission-guarded exported components.
   - Show disabled state without pruning disabled components.
   - Show aliases folded to target activities.

Tests for Phase 1:

- Unit tests for component-name normalization.
- Unit tests or fixtures for binary XML adapter output.
- Plain-text XML fallback test.
- Differential test against `aapt dump xmltree` for the committed `com.noto_54.apk` fixture.
- Import/reload persistence round-trip for manifest triples.
- Inspect known-answer test for `com.noto_54.apk` component counts, exported classifications,
  permission guards, disabled aliases, and alias identities.
- XAPK/base-manifest test covering split selection.

## Phase 2 -- String Constants, Intent API Rows, and Extras Facts

Goal: make literal string/class constants visible to the engine, model static Intent API dataflow as
input facts, and emit per-site extras edges without rewriting IR.

1. Add constant facts.
   - Add `const_str_assign(PackedInsnSiteId, FlowVertex, Symbol)` or equivalent fact storage.
   - Emit constants in the same three codegen locations that already handle object-ref tags:
     - assignment, for `x = "s"`;
     - field store, for `o.f = "s"`;
     - call argument, for `setAction("s")`, `putExtra("k", v)`, and similar sites.
   - Treat `const-class` descriptors as string constants for explicit component matching.

2. Add intraprocedural constant propagation to the index engine.
   - Seed `const_reaches` from `const_str_assign` in gated intent frames.
   - Propagate over `assign_like` using the same `substitute_prefix` and `paths` gate as
     `call_target_assign_like`.
   - Keep the initial rules intraprocedural; do not add down-call propagation in this phase.

3. Define and populate `intent_frame`.
   - Populate from the Android call-site observer, not by rediscovering send sites from resolved
     `facts.call` rows.
   - Include functions containing observed intent API calls or observed send-site calls.
   - Seed every string constant in those frames, not only strings present in the manifest.

4. Implement manifest-independent intent API summary emission.
   - Run this scan for every Java-family import where relevant method ids exist, including bare DEX,
     JAR, APK without manifest, and APK with failed manifest decode.
   - Match framework-owned methods with pinned class + method name + descriptor.
   - Match only `getIntent` / `setIntent` by name + descriptor without class pinning.
   - Emit required `formal_param` rows for every summary port.
   - Emit static `summary` rows before the main fixpoint.
   - Remove the conflicting coarse Android Intent rows from `java-index.jsonl`.

5. Implement API scan diagnostics.
   - Log matched functions and matched call sites per API row.
   - Treat zero matched built-in rows as an error when the `IdMap` contains any
     `Landroid/content/Intent;->...` method id but the table matched none.
   - Report known-but-unmodeled overloads or declined descriptors where practical.

6. Implement per-site extras fact emission.
   - Scan `facts.call`, `facts.actual_param`, and `const_str_assign` after codegen.
   - For literal-key `Intent.putExtra`/`get*Extra`, emit keyed `assign` rows on
     `.<extras>.<key>` and a matching `paths` row.
   - For literal-key `Bundle.put*`/`Bundle.get*`, emit keyed rows on `.<key>` at the bundle root.
   - For non-literal keys, emit lumped fallback rows on `.<extras>` for `Intent` and on the whole
     receiver for `Bundle`.
   - If the value argument is itself a constant and has no `actual_param`, root the value side at the
     appropriate `call_arg(insn, n)` vertex.

7. Preserve the extras path invariant.
   - A `Bundle`'s entries live at the bundle root.
   - An `Intent`'s extras live under `.<extras>`.
   - Check `replaceExtras(Bundle)`, `Bundle.putAll(Bundle)`, `getBundleExtra(String)`, and
     `putExtra(String, Bundle)` against this invariant before adding rows for them.

8. Check synthetic assigns at call sites before committing the emitter.
   - Verify SARIF step rendering, `tainted_var_at_insn`, graphviz output, inspect paths, `prog_store`,
     and `copy_edge` tolerate a site with `call`, `actual_param`, callee metadata, and synthetic
     `assign` rows.
   - If a consumer depends on the old one-statement-kind shape, use a fresh synthetic site for extras
     assigns and accept the weaker span.

9. Optional later-in-phase keyed rule.
   - Add only if measurements show non-literal extras keys matter.
   - If added, make reporting explicit that recovered-key rows overlap the lumped fallback bucket.

Tests for Phase 2:

- Codegen tests for constants in assignments, stores, call arguments, and `const-class`.
- Index-engine tests for `const_reaches` over assign/copy/path substitutions.
- Bodyless framework summary-instantiation regression from Phase 0.
- API scan tests for pinned-class matches, receiver-varying matches, accidental app-method rejection,
  and `IdMap`-based zero-match error.
- Extras fact tests for literal keyed `Intent`, literal keyed `Bundle`, non-literal lumped fallback,
  and constant value arguments with no `actual_param`.
- Consumer tests for call-site synthetic `assign` rows.

## Phase 3 -- Intent Linking in the Index Fixpoint

Goal: turn manifest components and send sites into input relations, let the engine derive pairings,
and make derived calls visible to query.

1. Add intent relations to the index engine.
   - Inputs:
     - `intent_send(FunctionId, InsnId, InsnId, FormalIndex, IntentKind)`
     - `intent_filter(Symbol, IntentKind, FunctionId)`
     - `intent_component(Symbol, IntentKind, FunctionId)`
     - any `extras_site` relation needed by the optional recovered-key rule
   - Derived:
     - `intent_pair(FunctionId, InsnId, FunctionId, PairKind)`

2. Add pairing rules.
   - Explicit pairing: send-site intent component constant joins `intent_component`.
   - Implicit pairing: send-site intent action constant joins `intent_filter`.
   - Derive `call(f, bridge_insn, recv)` from `intent_pair`.

3. Implement the intent linking pass.
   - Observe manifest data, class hierarchy, and Android-relevant Java call sites during import while
     `program_info` is available.
   - For observed Java calls, retain the caller function, original site id, declared receiver class,
     simple method name, descriptor, static/instance receiver shape, and argument count.
   - After all imports are interned, derive send sites from the observed call-site table, not from
     resolved `facts.call` rows.
   - Skip `LocalBroadcastManager.sendBroadcast` declared receiver types, including both `androidx`
     and `android.support` descriptors.
   - Do not treat `Intent.createChooser` as a send site; it is only a static API summary that lets a
     later real send read the wrapped target intent.
   - Mint one bridge site per send site.
   - Emit delivery `assign` rows at the bridge site:
     - activity: `call_arg(b, 0).<intent> := i`, `call_arg(b, 1) := i`;
     - receiver: `call_arg(b, 2) := i`;
     - service: `call_arg(b, 1) := i`.
   - Emit `intent_send`, `intent_filter`, and `intent_component` rows.

4. Resolve receive sites.
   - Normalize manifest component names to dex descriptors.
   - Fold `<activity-alias>` filters into their target activities.
   - Walk the class hierarchy to the nearest app-defined lifecycle/entry-method override.
   - Bridge activities into lifecycle overrides and `onNewIntent`.
   - Bridge receivers into `onReceive`.
   - Bridge services into `onStartCommand`, `onBind`, and relevant `IntentService`/`JobIntentService`
     methods when present.
   - Do not prune `enabled="false"` components.

5. Keep exported-state semantics separate from internal app-code pairing.
   - Do not prune `android:exported="false"` components when pairing sends observed in the same app.
   - Keep exported/default-exported/permission/disabled state in the manifest-derived inspect and
     external-entry facts.
   - If an external sender relation is added later, make it consume only exported component/filter
     facts and permissions rather than reusing internal send-site semantics.

6. Reporting.
    - Log send sites, explicit pairings, implicit pairings, unresolved send sites, and bridge-derived
      calls at `info`.
    - Persist or expose `intent_pair` in `IndexResult` so post-fixpoint reporting can count actual
      derived rows.

Tests for Phase 3:

- Fact-level pairing tests for explicit, implicit, wrong-kind, unresolved, and fan-out cases.
- Send-site observer tests proving ambiguous Java calls and `Mixed` strategy do not hide Android
  sends.
- Negative test proving `Intent.createChooser` does not produce an `intent_send` row.
- Bridge-site freshness tests: one bridge site per send site, shared by targets of the same send,
  not shared across sends.
- Delivery tests for activity, `onNewIntent`, receiver, and service argument positions.
- Alias-folding and hierarchy-walk tests.
- Disabled-component test proving disabled entries are retained.
- Internal non-exported component test proving same-app explicit sends are still paired.
- External-entry negative test, if that relation exists, proving non-exported components are not
  reachable from outside the app.
- Negative test for `LocalBroadcastManager` not pairing with manifest receivers.

## Phase 4 -- Real-App and Local Android Validation

Goal: validate the feature on the committed `com.noto_54.apk` fixture and on small source-built
Android cases with explicit known answers.

1. Extend the regression harness with Android intent cases.
   - Add a new Android/intent case kind if Phase 0 did not already do so.
   - Build tiny APKs from source and a manifest, or assemble APK-shaped ZIPs when installability is
     irrelevant.
   - Assert on component/method identity and SARIF/index facts, not source lines.

2. Add `com.noto_54.apk` manifest and inventory checks.
   - Assert component/tag/filter/action counts from the design.
   - Assert exported, non-exported, permission-guarded, and disabled classifications by name.
   - Assert component name normalization and alias identities.
   - Assert alias folding is idempotent for the ten aliases targeting `AppActivity`.

3. Add `com.noto_54.apk` intent-analysis checks.
   - Assert API scan counts for known present methods.
   - Assert extras-site totals and keyed/lumped/recovered-key buckets.
   - Assert send-site counts, explicit pairings, implicit pairings, unresolved sends, and derived
     intent call-row counts.
   - Add one hand-verified R8-shrunk flow through the app, derived by baksmali disassembly and
     documented in the case file.

4. Add local source-built flow cases.
   - Explicit activity carrying tainted extra into an activity read.
   - Implicit activity action carrying tainted extra through a manifest filter.
   - Broadcast receiver delivery.
   - Started service delivery.
   - Bound service delivery if supported by the initial implementation.
   - `getIntent` and `onNewIntent` delivery.
   - `putExtra`/`get*Extra` keyed extras.
   - `Bundle` round trip through `putExtras`/`getExtras`.
   - Chooser wrapping.
   - Activity alias folding.
   - Internal explicit send to a non-exported component.
   - External-entry negative for a non-exported component only if the external sender surface has
     been implemented; otherwise keep this as an inspect/attack-surface assertion.

5. Add the initial external ICC gate to regular regression.
   - Add or reuse the Android ICC runner from Phase 0.
   - Include a small pinned DroidBench ICC / ICC-Bench subset covering explicit activity, implicit
     action, broadcast receiver, started service, extras, alias/filter, and a non-exported
     external-entry negative when supported.
   - Run only this small smoke subset in the regular `xtask regression` path.
   - Mark unsupported cases with tracked `xfail`, `unsupported`, or `phase-6-plus` statuses rather
     than omitting them.

Tests for Phase 4:

- The Phase 4 deliverable is the test suite above, including the initial external ICC slice. It
  should run through Nix like the rest of `xtask regression`, self-skipping only when optional
  external tools such as `aapt` are absent.

## Phase 5 -- Expanded External ICC Validation

Goal: broaden DroidBench ICC and ICC-Bench coverage in the nightly test hook after the initial
external smoke slice gates the main feature.

1. Add nightly fixture acquisition.
   - Store expanded-suite fixtures and expected-answer files under `nightly/tests/android-icc/`.
   - Started: `nightly/tests/android-icc/` now contains the runner README and a scaffold
     DroidBench fixture set with pinned prebuilt APKs from upstream commit
     `a57fa6f42f278591695672f1aa8b37c275139370`.
   - Vendor pinned APKs or download/build from pinned upstream revisions in the Nix nightly
     environment.
   - Record fixture hashes even when built from source.
   - Keep only the Phase 4 smoke subset in regular regression; run the expanded suite in nightly CI.

2. Add an expected-answer manifest.
   - Store it under `nightly/tests/android-icc/` or a similar Android-specific directory.
   - Key entries by suite and fixture name.
   - Record expected sender component/method, receiver component/method, ICC kind, match kind,
     expected flow presence, extras expectation, and expected status.
   - Use statuses such as `pass`, `xfail`, `unsupported`, and `phase-6-plus`.

3. Add an `xtask` Android ICC runner.
   - Add a frontend selector such as `--frontend android-icc` or `--frontend icc`.
   - Started: `xtask regression --frontend android-icc` discovers `*.json5` specs, imports/indexes
     APKs when present, runs query models, validates SARIF, and checks optional `intent_pair` counts.
   - Initial real-suite state: `ComponentNotInManifest1` is enforced passing; `ActivityCommunication2`
     and `ActivityCommunication5` are tracked XFAILs for string-construction and `getIntent()`
     lifecycle delivery gaps.
   - Import the fixture APK, index it, run the fixture query model, validate SARIF, and compare
     logical locations or index facts with the expected-answer manifest.
   - Preserve filtering by fixture name and feature bucket.
   - Wire the expanded DroidBench / ICC-Bench invocation into `.github/workflows/nightly.yml`, not
     the default test workflow.

4. Expand the focused subset from Phase 4.
   - Add more explicit activity and implicit action variants.
   - Add broadcast receiver variants.
   - Add started service variants.
   - Add bound service if implemented.
   - Add extras variants.
   - Add alias/filter cases.
   - Add negative non-exported external-entry cases.
   - Mark result-back, programmatic receiver, and `PendingIntent` cases as future-phase until Phase 6
     implements them.

5. Report failures by cause.
   - Pairing missing.
   - Pairing excessive.
   - Extras flow missing.
   - Lifecycle delivery missing.
   - Query/SARIF location mismatch.

Tests for Phase 5:

- The DroidBench/ICC-Bench runner is the test. The runner and initial subset gate Phase 4 through
  regular regression; Phase 5's expanded end-to-end suite is committed under
  `nightly/tests/android-icc/` and run from the nightly GitHub workflow.

## Phase 6 -- Precision and Future Work

Goal: extend precision after the main manifest, constant, extras, and pairing pipeline is validated.

Candidate features:

- Result-back flows through `setResult` and `onActivityResult`.
- Programmatic receivers registered with `registerReceiver`.
- `PendingIntent` flows.
- Content-provider authorities and provider flows.
- Easy constructed string constants for literal-only `String.concat`, `String.valueOf`, and simple
  same-function `StringBuilder.append(...).toString()` chains.
- Static-final action strings not emitted as local `const-string`, such as selected `sget` cases.

Testing rule for Phase 6:

- No precision feature lands without a source-built synthetic case.
- When an external DroidBench/ICC-Bench case covers the same behavior, move it from `xfail` or
  `phase-6-plus` to enforced passing in the Android ICC runner.
- For easy constructed strings, include positive literal-only cases and a negative non-literal suffix
  case so the feature stays bounded.

## Remaining Open Decisions

These do not block Phase 1 or the harness work, but they should be resolved before the corresponding
implementation step ships.

1. **Implicit-intent fan-out default.** Default unresolved implicit fan-out to off, measure explicit
   and implicit pair counts on `com.noto_54.apk` and the external ICC suites, then decide whether a
   bounded fan-out option should be on by default.

2. **Optional in-fixpoint recovered-key extras rule.** Do not ship initially. First measure how many
   extras sites have non-literal keys in `com.noto_54.apk` and the external ICC subset. If the count
   is meaningful, add the rule with overlapping keyed/lumped reporting.

3. **Constant propagation beyond intraprocedural pass-through.** Keep initial constant rules
   intraprocedural. Use unresolved-send-site counts to decide whether to add up-direction
   `const_summary` rules for factory/getter cases. Do not add down-direction propagation without a
   context-sensitive design.
