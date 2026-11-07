# Intent - use CHA with expensive, surgical fallback algorithm - DO-NOT-MERGE

Change the call graph analysis to be faster and more scalable by resolving calls in a ladder.

There is an initial viability analysis in `cha-viability.md`.

- Closures and callbacks are resolved with hybrid inlining.
- `invoke-super` in the dex and jvm frontends should be handled precisely by finding the single,
  appropriate resolvent to call. This should work in the case of calling a parent as well as for
  interfaces, when applicable
- The query phase should be configurable to output the calls/signatures that resolve to nothing at
  all (future, do not make this a work item, I'm just noting it here)
- the `report` command should be modified so it can take extra model files, and take into account
  the built-in models, so that if the issues identified in `cha-viability` surface in other apps for
  other method signatures, report helps users identify them
- For every signature in the section "What the carved-out-calls should be handed to", handle them that way
- Add the "find: dispatch" modeling feature (see `cha-viability`)
- Include both structural closure-shaped testing and by name closure-shaped testing. The
  structural single-abstract-method test must be computed over a type's transitive
  super-interface closure rather than only its declared methods, or it misses inherited-method
  cases like `dagger.internal.Provider` and `dagger.internal.Factory` (see `cha-viability`).
- Follow the algorithm in cha-viability: 1. dispatch model, 2. threshold, 3. hybrid inlining
- `K`, the step-2 threshold, is one CLI flag and one field on the index config, defaulting to 32
  (the knee is between 16 and 32). Model-first is the default order; threshold-first is a flag for
  a precision-sensitive run, not the default.
- Leave hybrid inlining's soundness gap open -- unresolved calls are simply unresolved
- Interface-dispatched sites should be configurable separately from class-virtual ones. They are a
  different population -- 8.7-16% of them resolve to a single target against 81-91% for ordinary
  virtual calls -- and they own 31-42% of the excess. `CallStyle::JavaCall` already carries the
  `JavaDispatch` this needs.
- Every call site must land in exactly one of four buckets -- modelled, skipped, CHA, inlined --
  and the counts must be reported on the index's summary line beside the existing `models: N
  summary row(s), ... M function bod(ies) not analyzed`. Without it a mis-scoped dispatch model
  silently swallows a signature and the only symptom is a missing finding.
