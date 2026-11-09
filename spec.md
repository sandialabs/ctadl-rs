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
  skips when it is absent. `xtask/tests/dex/README.md:23-25` names that file
  as the APK's only reader outside xtask, but
  `frontends/ctadl-dex/src/tests.rs:758` reads it too, and fails when it is
  absent.

`dex-reader` already depends on `zip`, `thiserror`, `smallvec`, `log` and
`rayon` (`readers/dex-reader/Cargo.toml:6-11`), not on serde or any ctadl
crate. Its `lib.rs` allows `clippy::all` crate-wide because most of it is
vendored (`readers/dex-reader/src/lib.rs:1-5`), and its `Cargo.toml` has no
`[lints] workspace = true`, unlike the other ctadl crates. Linted like them,
it has 77 findings in eight files: 44 hidden-lifetime errors from
`rust-2018-idioms` and 33 clippy warnings.

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
- R11. **Fixture ownership is current.** `xtask/tests/dex/README.md` names
  the fixture's two readers outside xtask, the dex-reader test and the
  ctadl-dex test, and says which of them skips when the file is absent. It
  no longer names the ctadl-ascent file.

**Lints**

- R12. **dex-reader is linted like the workspace.** It takes the workspace
  lints and has no crate-wide clippy allow, and `cargo clippy -- -Dwarnings`
  passes on it, vendored code included. Fixing the findings changes no
  behavior.

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

Both input forms move together: a manifest may be stored as plain-text XML
(the integration tests build such APKs, `ctadl-ascent/tests/integration/cli.rs:834`),
and one function decodes either, so callers never see the split.

The queries move with the types because they read only the tree and need no
ctadl crate; keeping them as methods avoids an extension trait in
`ctadl-ascent` and keeps `attr` and `has_child_tag` private.

The module is named for the format. Its types keep their manifest names,
since every caller decodes a manifest. (R1-R3, R10.)

Invariants carried over unchanged:

- Node ids are dense, assigned in document (pre-)order starting at 0, and
  equal to the node's index in `nodes`. `has_child_tag` relies on this.
- The root `manifest` element is node 0; `package` reads its attribute.
- Attribute keys in the android namespace are `android:<name>` from AXML and
  `<namespace-uri>:<name>` from text XML; `attr_key_matches` accepts both.

The module needs no lint attribute of its own: the whole crate is linted
(D6).

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

### D6. Lints

`readers/dex-reader/Cargo.toml` gains `[lints] workspace = true`, and
`readers/dex-reader/src/lib.rs:1-5` loses the `#![allow(clippy::all)]` and the
comment that justifies it. The findings are fixed in the code, not allowed;
an `#[allow]` on a single item is acceptable only where the fix would change
behavior, with a comment saying why. (R12.)

## Concerns

None open. Text XML moves with AXML (D1), and the Android queries move with
the tree types (R3).
