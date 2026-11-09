# AXML decoder in dex-reader: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the move of the AndroidManifest
decoder, its tree types and its Android queries from `ctadl-ascent` into
`dex-reader`. R* and D* numbers are the spec's; where this plan and the spec
disagree, the spec wins, except for the corrections listed below. Each phase
builds, passes the gate named in it, and is one commit.

## Corrections to the spec

- P1. **The crate comment does not promise a lint opt-in.** D1 says the new
  module opts back into clippy "as the crate comment already promises for code
  it holds to workspace standards". The comment at
  `readers/dex-reader/src/lib.rs:1-4` says the opposite: code changed since
  the vendoring is held to the workspace's standards "by review and by its own
  tests, not by this lint". Until settled, this plan keeps D1's
  `#![warn(clippy::all)]` in `axml.rs` and has Phase 1 extend that comment to
  say that `axml` is not vendored and opts back in.
- P2. **The noto APK has two readers outside xtask.** R11 has the fixture
  README name "the dex-reader test" as the fixture's reader outside xtask. But
  `frontends/ctadl-dex/src/tests.rs:758` (`a_class_global_is_declared_at_its_class_def_item`)
  also reads it, and does not skip when it is absent (`expect("the APK")`,
  `frontends/ctadl-dex/src/tests.rs:761`). The README's "only" at
  `xtask/tests/dex/README.md:23-25` is already false. Until settled, Phase 3
  has the README name both tests, and say that only the dex-reader one skips.

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1 | additive: `dex_reader::axml` with its tests; no callers yet |
| 2 | 2 | switches every caller and deletes the old decoder; needs 1 |
| 3 | 3 | fixture README; needs 2's test to exist in its final place |

The work is a move, so it is nearly serial. Phase 1 adds the new module beside
the old one, so the two decoders can be compared on the same inputs while both
exist (J1). Phase 2 cannot be split: changing
`ctadl_index::error::Error::AndroidManifest` to wrap `AxmlError` breaks every
`Error::AndroidManifest { message }` in the old decoder, so the error change,
the deletion and all the callers build only together. No two phases share a
wave, so no file has two owners at once.

Test commands used throughout:

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test -p dex-reader
    cargo test -p ctadl-index -p ctadl-ascent
    cargo xtask regression --frontend dex --filter manifest

The last one runs `intent:apk-manifest` (`xtask/src/dex.rs:128`) and
`apk:manifest-and-intents` (`xtask/src/apk.rs:52`), the two checks that
decode or load the noto manifest. Run it in the Nix dev shell: the dex checks
that run first need the Java toolchain, and if they fail as a whole their
results are dropped by `--filter`
(`xtask/src/regression.rs:511-518`, `:580-583`) rather than reported. The
report must list both checks as PASS.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Record that the gates pass on the unchanged tree, so a later
failure is known to be new. No commit.

**Tests.** Run every command above. Save the output to files outside the
repository. Note any test that already fails; later phases are judged against
this list.

**Gate.** None; this phase records.

## Phase 1: add `dex_reader::axml` (R1, R2, R3, R9, R10; D1, D2, D5)

Depends on: 0.

**What.** Add the module `dex_reader::axml`, a copy of the decoder, tree types
and queries, with the decoder tests. Nothing calls it yet, and `ctadl-ascent`
is unchanged apart from one temporary test. No behavior change.

**Files.**
- `readers/dex-reader/src/axml.rs` (new). Starts with `#![warn(clippy::all)]`
  (D1, P1). Holds:
  - `AndroidManifest`, `ManifestNode`, `ManifestNodeChild`, `ManifestNodeAttr`
    as at `ctadl-ascent/src/languages/android_manifest.rs:8-32`, deriving
    `Debug, Clone, PartialEq, Eq` (plus `Default` on `AndroidManifest`), no
    serde (R9);
  - `AndroidComponent` (`android_manifest.rs:34-45`);
  - `impl AndroidManifest` with `package`, `components`, `attr`,
    `has_child_tag` (`android_manifest.rs:96-144`); not `save`/`load`;
  - `normalize_component_name`, `is_component_tag`, `attr_key_matches`,
    `parse_bool` (`android_manifest.rs:147-178`);
  - `pub fn parse_manifest(bytes: &[u8]) -> Result<AndroidManifest, AxmlError>`
    (`android_manifest.rs:180-185`), `is_binary_xml` and everything from
    `parse_binary_manifest` through `append_element`
    (`android_manifest.rs:201-513`), copied verbatim except that each
    `Error::AndroidManifest { message: ... }` becomes `AxmlError { message: ... }`;
  - `AxmlError` (D2):
    `#[derive(Debug, thiserror::Error)] #[error("{message}")] pub struct AxmlError { message: String }`.
    Naming the field `message` keeps the copied bodies otherwise identical.
- `readers/dex-reader/src/lib.rs:18` — add `pub mod axml;` after `pub mod apk;`.
  Extend the comment at `readers/dex-reader/src/lib.rs:1-4` with a sentence
  that `axml` is not vendored and opts back into the lint (P1).
- `readers/dex-reader/Cargo.toml:6-11` — add `roxmltree = "0.20"` (D4).
  `Cargo.lock` gains the edge from `dex-reader` to the existing `roxmltree`
  entry, and no new package version.
- `ctadl-ascent/src/languages/android_manifest.rs:515` (tests module) — the
  temporary equivalence test below (J1).

**Tests.**
- New, `readers/dex-reader/src/axml.rs` `mod tests`, moved from
  `android_manifest.rs:519-626` and adapted only in paths:
  - `text_manifest_becomes_triples` — as at `android_manifest.rs:520`.
  - `component_names_normalize_to_descriptors` — as at `:552`.
  - `binary_xml_is_reported_clearly` — as at `:592`: the error from the four
    magic bytes alone displays text containing `unexpected end of manifest`.
  - `noto_binary_manifest_has_known_counts` — as at `:598`, but the APK path
    is `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../xtask/tests/dex/com.noto_54.apk")`
    and the bytes come from `crate::apk::read_bundle_entry` (D5). Asserts 180
    nodes, 302 attributes, 35 components, 10 of them `activity-alias`, package
    `com.noto`, and the exported `Lcom/noto/app/AppActivity;` activity with an
    intent filter. Skips when the file is absent.
  - `parquet` round-trip test does not move (R10).
- New, temporary, `ctadl-ascent/src/languages/android_manifest.rs`:
  `axml_decoder_matches_engine_decoder` — for the two text manifests in the
  existing tests (`:521`, `:569`), the intent-filter manifest at
  `ctadl-ascent/src/languages/android_intent.rs:1023`, and the noto APK's
  manifest (skipped when absent), both `parse_manifest` and
  `dex_reader::axml::parse_manifest` succeed, and their `nodes`, `children`
  and `attrs`, mapped to tuples, are equal element by element, in order. Their
  `components()` are equal field by field. For the truncated input
  `0x0008_0003u32.to_le_bytes()`, the old error's message equals the new
  error's `to_string()`. This test proves R6 and R8's message text; Phase 2
  deletes it with the old decoder.
- Existing, still passing: everything in `cargo test -p ctadl-ascent`.

**Gate.**

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test -p dex-reader
    cargo test -p ctadl-ascent android_manifest

## Phase 2: switch callers to `dex_reader::axml` and delete the old decoder (R4, R5, R6, R7, R8; D2, D3, D4)

Depends on: 1.

**What.** Make `dex_reader::axml` the only decoder. The engine keeps only
persistence and import. No behavior change: decoded output, stored tables and
rendered messages are as before.

**Files.**
- `ctadl-index/src/error.rs:224-225` — the variant becomes
  `#[error("android manifest error: {0}")] AndroidManifest(#[from] dex_reader::axml::AxmlError)`
  (D2). `ctadl-index` already depends on `dex-reader`
  (`ctadl-index/Cargo.toml:18`). No other code constructs or matches this
  variant.
- `ctadl-ascent/src/languages/android_manifest.rs` — delete the types, queries
  and decoder (`:8-46`, `:96-185`, `:201-513`) and the moved tests. Keep, as
  free functions over `dex_reader::axml::AndroidManifest` (D3):
  - `pub fn save_manifest<P: AsRef<Path>>(manifest: &AndroidManifest, path: P) -> Result<(), Error>`,
    the body of `:48-68`;
  - `pub fn load_manifest<P: AsRef<Path>>(path: P) -> Result<AndroidManifest, Error>`,
    the body of `:70-94`;
  - `import_from_apk` (`:187-199`), calling `dex_reader::axml::parse_manifest`
    (its `?` converts through the new `#[from]`) and `save_manifest`.
  Drop the `serde` import (`:3`). Do not re-export the dex-reader types (D3).
- `ctadl-ascent/src/languages/android_intent.rs:10` — import `AndroidManifest`
  from `dex_reader::axml`; `:197` uses `android_manifest::load_manifest`;
  the test at `:1023` calls `dex_reader::axml::parse_manifest`.
- `xtask/src/dex.rs:456` — call `dex_reader::axml::parse_manifest`. Its error
  type implements `std::error::Error`, so `.with_context` still applies.
- `xtask/src/apk.rs:914-916` — use
  `ctadl_ascent::languages::android_manifest::load_manifest`.
- `ctadl-ascent/Cargo.toml:60` — remove `roxmltree` (R4). `Cargo.lock` loses
  that edge only.
- `ctadl-ascent/src/cli/mod.rs:51` — unchanged; it still calls
  `android_manifest::import_from_apk` and logs failures as warnings (R8).

**Tests.**
- Kept in `ctadl-ascent/src/languages/android_manifest.rs`:
  `manifest_round_trips_through_parquet_tables` (`:568`), now parsing with
  `dex_reader::axml::parse_manifest` and calling `save_manifest` and
  `load_manifest` (R5, R7, R10).
- New, same file: `decoding_error_renders_with_engine_prefix` — converting
  `dex_reader::axml::parse_manifest(&0x0008_0003u32.to_le_bytes())`'s error
  into `ctadl_index::error::Error` gives text that starts with
  `android manifest error: unexpected end of manifest` (R8).
- Deleted: the temporary `axml_decoder_matches_engine_decoder`, and the
  tests that moved in Phase 1.
- Existing, still passing: `activity_filters_require_default_category`
  (`android_intent.rs:1022`); the integration tests that import APKs with
  truncated AXML (`ctadl-ascent/tests/integration/cli.rs:1084`, `:1118`) and
  text XML (`cli.rs:834`), unchanged.
- Regression: `intent:apk-manifest` and `apk:manifest-and-intents` pass, with
  the counts pinned at `xtask/src/apk.rs:917-942` unchanged.

**Gate.**

    cargo fmt --all -- --check
    cargo clippy -- -Dwarnings
    cargo test -p dex-reader
    cargo test -p ctadl-index -p ctadl-ascent
    cargo xtask regression --frontend dex --filter manifest

## Phase 3: fixture README (R11)

Depends on: 2.

**What.** Docs only.

**Files.**
- `xtask/tests/dex/README.md:23-25` — say that outside xtask the APK is read
  by the unit test in `readers/dex-reader/src/axml.rs`, which skips when the
  file is absent, and by the test in `frontends/ctadl-dex/src/tests.rs`, which
  does not (P2). Name files, not the spec's or this plan's numbers.

**Tests.** None.

**Gate.** `git grep -n 'android_manifest.rs' -- xtask/tests/dex/README.md`
prints nothing.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `readers/dex-reader/src/axml.rs` | new: types, queries, decoder, `AxmlError`, tests | 1 |
| `readers/dex-reader/src/lib.rs` | declare `axml`; extend lint comment | 1 |
| `readers/dex-reader/Cargo.toml` | add `roxmltree` | 1 |
| `Cargo.lock` | `roxmltree` edge moves from ctadl-ascent to dex-reader | 1, 2 |
| `ctadl-ascent/src/languages/android_manifest.rs` | temporary equivalence test; then only `save_manifest`, `load_manifest`, `import_from_apk` and their tests | 1, 2 |
| `ctadl-index/src/error.rs` | `AndroidManifest` wraps `AxmlError` | 2 |
| `ctadl-ascent/src/languages/android_intent.rs` | import from `dex_reader::axml`; `load_manifest` | 2 |
| `ctadl-ascent/Cargo.toml` | drop `roxmltree` | 2 |
| `xtask/src/dex.rs` | call `dex_reader::axml::parse_manifest` | 2 |
| `xtask/src/apk.rs` | call `load_manifest` | 2 |
| `xtask/tests/dex/README.md` | name the fixture's readers | 3 |

Not touched: `ctadl-index/src/facts/schema.rs` (the `manifest_node*` tables,
R7), `ctadl-ascent/src/cli/mod.rs`, `docs/spec.md`,
`ctadl-ascent/tests/integration/cli.rs`, `xtask/Cargo.toml` (xtask still needs
ctadl-ascent for `load_manifest` and the intent facts).

## Acceptance

- Every command under "Test commands used throughout" passes, and nothing
  fails that passed in Phase 0.
- Phase 1's equivalence test passed on the commit that added it (R6, R8).
- `git grep -n roxmltree -- ctadl-ascent` prints nothing (R4).
- `git grep -nE 'parse_binary_manifest|parse_text_manifest|read_chunk_header' -- ctadl-ascent`
  prints nothing (R4).
- `git grep -nE 'serde|parquet|ctadl' -- readers/dex-reader/src/axml.rs readers/dex-reader/Cargo.toml`
  prints nothing (R1, R5, R9).
- `cargo tree -p dex-reader -e normal --depth 1` lists `roxmltree` as its only
  new dependency (R9).

## Judgment calls

- J1. **Equivalence test while both decoders exist.** Phase 1 adds a
  temporary test comparing old and new decoder output, and Phase 2 deletes
  it. The alternative is to trust the verbatim copy and the pinned counts.
