# Import: read only the bytes a step needs

`ctadl import` holds whole artifacts in memory when it needs only a small part of them, or
needs them only as a stream. Point 1 of `PERFORMANCE.md` reports this problem. This branch
fixes the cases that occur in our code.

## The problem

Three places read a whole artifact with `std::fs::read`:

- **Hashing.** `hash_artifact` (`ctadl-import/src/project.rs:641`) reads a single-file artifact
  whole (`:665`), and reads each file of a directory artifact whole (`:658`), then feeds the
  buffer to SHA-256. SHA-256 can take its input as a stream. `ctadl import` runs the hash on a
  thread of its own while the import runs (`ctadl-ascent/src/main.rs:1113`), so the hash's copy
  is in memory at the same time as the importer's copy. The up-to-date check
  (`ArtifactImport::is_up_to_date`, `ctadl-import/src/project.rs:567`) hashes the whole
  artifact again on every import.
- **APK import.** `ctadl_dex::import_apk` (`frontends/ctadl-dex/src/lib.rs:94`) reads the whole
  APK and passes the bytes to `APKParser::new` (`readers/dex-reader/src/apk.rs:490`). That
  parser uses only the ZIP central directory and the `classes*.dex` entries. Resources, assets
  and native libraries take up most of a large APK, and all of them are loaded, then dropped.
- **.NET detection.** `is_cli_image` (`frontends/ctadl-cil/src/lib.rs:443`) reads a whole
  `.dll`/`.exe` and parses it as a module, only to decide between the CIL and pcode frontends.
  Then it discards the bytes, and whichever frontend is chosen reads the file again.
  `dir_has_cli_image` and the CIL directory import (`frontends/ctadl-cil/src/lib.rs:171`) call
  it for every `.dll`/`.exe` in a directory.

Measured on this branch's base with `ctadl import --no-native-libs`, three runs each. Immich
(`fdroid/app.alextran.immich_3030699.apk` in ct-bench) is a 180 MB APK holding 7.6 MB of Dex.
The second input is the same Dex repacked into an APK with nothing else in it:

| Input | Median peak footprint | Median wall time |
| --- | ---: | ---: |
| Immich, 180 MB APK | 775 MB | 0.79 s |
| Same Dex, 3.6 MB APK | 550 MB | 0.45 s |

The ~177 MB of resources cost ~225 MB of peak memory and ~0.35 s. The benchmark APKs schildi,
loki messenger and osmand are built the same way: 160-177 MB with 14-48 MB of Dex.

## Goals

- `hash_artifact` streams each regular file through a fixed-size buffer. It retries
  interrupted reads and stops at end of file. Its memory no longer grows with the artifact's
  size.
- Every digest stays byte-identical to today's, for single files, empty files and directory
  trees: the same length prefixes, path framing and order. Stored hashes must keep matching,
  or every existing import looks out of date. Do not replace the content hash with a size or a
  timestamp.
- APK import reads the archive from the file (`Read + Seek`), not from a whole-file buffer. It
  decompresses only the `classes*.dex` entries, as `read_bundle_entry` and
  `has_dex_entries_of_file` already do. The parallel per-entry inflation in `APKParser::new`
  stays. Callers that already hold the bytes (tests, `xtask/src/dex.rs`) still work, either
  through a generic reader or by keeping a bytes constructor.
- `is_cli_image` reads only the PE headers and the CLI header data directory it needs to
  decide. It returns the same answer as today for every PE file, including truncated and
  malformed ones.
- Import output does not change: decoded IR, the VMT, source info and artifact digests stay
  identical. Serialized IR stays byte-identical.

## Tests

- A hash test that compares the streamed digest with today's whole-buffer digest at 0, 1, and
  buffer size −1, size and +1 bytes, and at a size that takes several reads. Do the same for a
  directory tree with nested and empty files.
- A digest recorded by today's build still passes `is_up_to_date` after the change.
- `is_cli_image` agrees with today's implementation on: a .NET assembly, a native PE, a
  non-PE file, a truncated PE, and a PE with no CLI data directory.
- The existing dex-reader APK tests and the `ctadl-dex` import tests pass. One test imports an
  APK whose resources are much larger than its Dex.
- Import the ct-bench APKs before and after the change, including the resource-heavy ones
  above, and check that the IR, VMT, source info and digests match. Report median peak
  footprint and wall time over three runs, together with the Immich numbers above. A gain in
  import time is welcome but not required. The memory gain on resource-heavy APKs is the goal.

## Out of scope

- `hash_file_contents` (`ctadl-import/src/project.rs:608`). It sorts the model files by their
  contents before hashing, so it needs every file's bytes at once. Model files are small.
- `jni_registry::scan_import` (`frontends/ctadl-pcode/src/jni_registry.rs:234`). It reads the
  whole ELF to scan its data sections, which is a real use of the bytes. Reading only those
  sections is a separate change.
- The `.dex` and `.class` frontends, which read the whole file because their decoders need all
  of it.
- XAPK splits and the native libraries inside an APK. These are already streamed to disk.
- Detecting the format by content instead of by extension. `PERFORMANCE.md` describes it, but
  our detection goes by extension, and changing that is a behavior change, not a performance
  fix.
