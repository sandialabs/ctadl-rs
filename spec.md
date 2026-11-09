# AXML decoder in dex-reader: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This spec moves the AndroidManifest decoder out of the
engine crate and into `readers/dex-reader`, next to the APK reading it depends
on. It must stay consistent with `docs/spec.md`, which it does not change: the
move is internal and alters no behavior in that document. It does not sequence
the work.

## Background

`ctadl-ascent/src/languages/android_manifest.rs` holds four things at once:

- **Decoding.** `parse_manifest` (`android_manifest.rs:180`) turns manifest
  bytes into a flat tree. It handles binary XML (AXML, detected by the magic
  `0x00080003` at `android_manifest.rs:201`, decoded by
  `parse_binary_manifest` at `android_manifest.rs:205`) and plain-text XML
  (`parse_text_manifest` at `android_manifest.rs:473`, through `roxmltree`).
- **The tree types.** `AndroidManifest`, `ManifestNode`, `ManifestNodeChild`,
  `ManifestNodeAttr` (`android_manifest.rs:8-32`). They derive
  `Serialize`/`Deserialize`, but nothing serializes them with serde.
- **Android queries.** `package`, `components`, `AndroidComponent` and
  `normalize_component_name` (`android_manifest.rs:34-46`, `:96-178`). They read only the
  tree.
- **Persistence and import.** `AndroidManifest::save`/`load`
  (`android_manifest.rs:48-93`) write and read three parquet tables defined in
  `ctadl-index/src/facts/schema.rs:238-260`. `import_from_apk`
  (`android_manifest.rs:187`) reads the APK entry with
  `dex_reader::apk::read_bundle_entry` (`readers/dex-reader/src/apk.rs:159`),
  decodes it, and saves it.

Every decoding failure is `ctadl_index::error::Error::AndroidManifest { message }`
(`ctadl-index/src/error.rs:224-225`), displayed as
`android manifest error: <message>`.

Callers:

- `ctadl-ascent/src/cli/mod.rs:51` calls `import_from_apk` and logs a warning
  on failure.
- `ctadl-ascent/src/languages/android_intent.rs:197` loads the saved manifest
  and walks `components()` and `attrs`; its test at
  `android_intent.rs:1023` calls `parse_manifest`.
- `xtask/src/dex.rs:452-459` (`check_apk_manifest`) reads and decodes the
  committed APK's manifest.
- `xtask/src/apk.rs:913-916` (`check_manifest_and_intents`) loads the saved
  manifest and checks its counts.
- The unit test `noto_binary_manifest_has_known_counts`
  (`android_manifest.rs:598`) reads `xtask/tests/dex/com.noto_54.apk`, and
  `xtask/tests/dex/README.md:23-25` names that file as its only reader outside
  xtask.

`dex-reader` already depends on `zip`, `thiserror`, `smallvec`, `log` and
`rayon` (`readers/dex-reader/Cargo.toml:6-11`), not on serde or any ctadl
crate. Its `lib.rs` allows `clippy::all` crate-wide because most of it is
vendored (`readers/dex-reader/src/lib.rs:1-5`).

## Requirements

**What moves**

- R1. **Decoder in dex-reader.** `dex-reader` decodes an AndroidManifest from
  its bytes, both binary (AXML) and plain-text XML, with no dependency on any
  ctadl crate.
- R2. **Tree types in dex-reader.** The decoded tree (nodes, parent/child
  edges, attributes, with the same fields and node numbering as today) is
  defined in `dex-reader`.
- R3. **Android queries in dex-reader.** The package name, the component list
  (`AndroidComponent`), and component-name normalization are defined in
  `dex-reader` beside the tree types.
- R4. **No decoder left behind.** `ctadl-ascent` contains no AXML or XML
  parsing code and does not depend on `roxmltree`.
- R5. **Persistence stays in the engine.** Saving the tree to, and loading it
  from, the three `manifest_node*` parquet tables stays in `ctadl-ascent`,
  through the schemas in `ctadl-index`. `dex-reader` does not learn about
  parquet or the store.

**What must not change**

- R6. **Same decoded output.** For every input, the decoder returns the same
  nodes, edges and attributes, in the same order, as today. The noto APK still
  decodes to 180 nodes, 302 attributes and 35 components, 10 of them
  `activity-alias`.
- R7. **Same stored tables.** The parquet files, their names and columns are
  unchanged, so existing imports load without a re-import.
- R8. **Same messages.** A decoding failure still renders as
  `android manifest error: <message>` with today's message text, and the CLI
  still logs it as a warning and continues the import.
- R9. **No new dependencies for dex-reader beyond `roxmltree`.** In
  particular, `dex-reader` does not gain serde.

**Tests**

- R10. **Decoder tests live with the decoder.** The tests of parsing, of
  component-name normalization, of the truncated-AXML error, and of the noto
  APK's counts run under `cargo test -p dex-reader`. The parquet round-trip
  test stays in `ctadl-ascent`.
- R11. **Fixture ownership is current.** `xtask/tests/dex/README.md` names the
  dex-reader test, not the ctadl-ascent file, as the fixture's reader outside
  xtask.

## Design

### D1. Module `dex_reader::axml`

A new module `readers/dex-reader/src/axml.rs`, declared in
`readers/dex-reader/src/lib.rs`, holds:

- the tree types `AndroidManifest`, `ManifestNode`, `ManifestNodeChild`,
  `ManifestNodeAttr`, deriving `Debug, Clone, PartialEq, Eq` (and `Default` on
  `AndroidManifest`), without serde;
- `pub fn parse_manifest(bytes: &[u8]) -> Result<AndroidManifest, AxmlError>`,
  with the binary and text paths and their helpers moved verbatim apart from
  the error type;
- the inherent methods `package` and `components`, the private helpers they
  use (`attr`, `has_child_tag`, `is_component_tag`, `attr_key_matches`,
  `parse_bool`), `AndroidComponent`, and `normalize_component_name`;
- the unit tests named in R10.

The module is named for the format. Its types keep their manifest names,
since every caller decodes a manifest. (R1-R3, R10.)

Invariants carried over unchanged:

- Node ids are dense, assigned in document (pre-)order starting at 0, and
  equal to the node's index in `nodes`. `has_child_tag` relies on this.
- The root `manifest` element is node 0; `package` reads its attribute.
- Attribute keys in the android namespace are `android:<name>` from AXML and
  `<namespace-uri>:<name>` from text XML; `attr_key_matches` accepts both.

The crate-wide `#![allow(clippy::all)]` exists for vendored code. This module
is not vendored, so it opts back in with a module-level
`#![warn(clippy::all)]`, as the crate comment already promises for code it
holds to workspace standards.

### D2. Error type

`dex_reader::axml::AxmlError` is a `thiserror` struct carrying the message
string, displayed as the bare message. It is separate from
`dex_reader::error::DexError`: a manifest is not a Dex file, and folding it
into `DexError` would route it through `Error::Import` and change the rendered
text.

`ctadl_index::error::Error::AndroidManifest` wraps it with `#[from]` and keeps
the display `android manifest error: {0}`. (R8.)

### D3. What stays in ctadl-ascent

`ctadl-ascent/src/languages/android_manifest.rs` keeps:

- `save_manifest(&AndroidManifest, dir)` and `load_manifest(dir)`, free
  functions over `dex_reader::axml::AndroidManifest`. They can no longer be
  inherent methods, because the type is foreign. Their bodies are today's
  `save`/`load`.
- `import_from_apk`, unchanged in behavior, calling
  `dex_reader::axml::parse_manifest`.
- the parquet round-trip test.

It does not re-export the dex-reader types. Callers name them from
`dex_reader::axml`. (R4, R5, R7.)

### D4. Callers

- `android_intent.rs` takes `AndroidManifest` from `dex_reader::axml` and
  loads it with `load_manifest`.
- `xtask/src/dex.rs` `check_apk_manifest` calls
  `dex_reader::axml::parse_manifest` and no longer needs ctadl-ascent for it.
- `xtask/src/apk.rs` `check_manifest_and_intents` loads with
  `load_manifest`.
- `ctadl-ascent/Cargo.toml` drops `roxmltree`; `readers/dex-reader/Cargo.toml`
  adds it at the same version (`0.20`), so the lock file gains no new version.

### D5. The noto test

The test reads the APK at `../../xtask/tests/dex/com.noto_54.apk` relative to
`CARGO_MANIFEST_DIR` of dex-reader, and skips when it is absent, as today.
(R6, R10, R11.)

## Concerns

- C1. **Does text XML belong in a Dex reader?** The intent says "AXML
  decoder", but `parse_manifest` also accepts plain-text XML, and the
  integration tests build APKs with text manifests
  (`ctadl-ascent/tests/integration/cli.rs:834`). Options: (a) move both paths,
  so `dex-reader` gains `roxmltree`, a small crate with no dependencies; (b)
  move only the AXML path, and leave the text path and the binary/text dispatch
  in `ctadl-ascent`, which keeps `dex-reader` free of an XML library but splits
  one decoder across two crates. The spec assumes (a).
- C2. **Do the Android queries move?** `package`, `components` and
  `normalize_component_name` interpret the manifest rather than decode it.
  Options: (a) move them with the types (R3), keeping them as methods; (b)
  leave them in `ctadl-ascent` as free functions or an extension trait, keeping
  `dex-reader` to decoding alone. They have no ctadl dependencies, and
  splitting a type from its queries costs an extension trait for no gain. The
  spec assumes (a).
