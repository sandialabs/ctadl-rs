# Android ICC External Benchmarks

This directory is the Phase 5 home for expanded DroidBench ICC / ICC-Bench-style validation.

Cases are discovered by `cargo xtask regression --frontend android-icc` from `*.json5` specs in
this directory. The runner is intentionally fixture-agnostic: pinned APKs can be vendored here or
produced by the Nix nightly environment, and missing APKs report as `SKIP` rather than silently
vanishing.

Spec format:

```json5
{
  apk: "fixtures/SomeCase.apk",
  model: "models/some-case.json",
  expect_flow: true,
  expected_intent_pairs: 1,
  status: "pass",
}
```

- `apk` is relative to the spec file unless absolute.
- `model` is a normal CTADL model file passed to `ctadl query -m`.
- `expect_flow: true` requires a SARIF code flow connecting a source to a sink.
- `expect_flow: false` is a negative case.
- `expected_intent_pairs` is optional and checks persisted `intent_pair.parquet` rows.
- `status` is `pass`, `xfail`, `unsupported`, or `phase6-plus`; non-`pass` cases must fail until the
  implementation catches up, then the runner asks you to update the status.

Keep the regular regression slice small. Put expanded external-suite growth here and run it from the
nightly check path.

The initial pinned DroidBench fixtures come from `secure-software-engineering/DroidBench` commit
`a57fa6f42f278591695672f1aa8b37c275139370`.
