# Import: read only the bytes a step needs: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This spec refines that intent, and must stay consistent with
`docs/spec.md`, which it does not change. It does not sequence the work.

## Background

Three steps of `ctadl import` read a whole artifact into memory with `std::fs::read`:

- `hash_artifact` (`ctadl-import/src/project.rs:641`) reads a single-file artifact whole
  (`:665`), and reads each file of a directory whole (`:658`), before hashing it.
  - `ctadl import` hashes on a second thread while the import runs
    (`ctadl-ascent/src/main.rs:1113`), so this copy is in memory alongside the importer's.
  - `ArtifactImport::is_up_to_date` (`ctadl-import/src/project.rs:567`) and
    `record_artifact_hash` (`:548`) call it too.
- `ctadl_dex::import_apk` (`frontends/ctadl-dex/src/lib.rs:94`) reads the whole APK and passes
  the bytes to `APKParser::new` (`readers/dex-reader/src/apk.rs:490`). `APKParser::new` uses
  only the ZIP central directory and the `classes*.dex` entries. It inflates the entries in
  parallel, and each task clones the archive (`readers/dex-reader/src/apk.rs:509`). XAPK splits
  reach the same function after they are extracted to disk.
- `is_cli_image` (`frontends/ctadl-cil/src/lib.rs:443`) reads a whole `.dll`/`.exe` and runs
  `Module::read` (`readers/cil-reader/crates/cil-reader/src/model/module.rs:236`) on it. That
  checks:
  - the PE headers and the CLI header (`readers/cil-reader/crates/cil-reader/src/raw/pe.rs:216`);
  - the metadata root, the debug directory, and the metadata table header
    (`readers/cil-reader/crates/cil-reader/src/raw/metadata.rs:52`).

  All of these sit at offsets that the headers give, which can be anywhere in the file.
  Callers: autodetection (`ctadl-ascent/src/main.rs:1391`), `dir_has_cli_image`
  (`frontends/ctadl-cil/src/lib.rs:448`), and the CIL directory import, once per candidate file
  (`frontends/ctadl-cil/src/lib.rs:171`).

Baseline (`ctadl import --no-native-libs`, median of three runs): Immich
(`fdroid/app.alextran.immich_3030699.apk` in ct-bench) is a 180 MB APK with 7.6 MB of Dex. Its
import peaks at 775 MB of physical footprint and takes 0.79 s. The same Dex repacked alone into
a 3.6 MB APK peaks at 550 MB and takes 0.45 s.

## Requirements

**Hashing**

- R1. **Digests unchanged.** `hash_artifact` returns the same digest as today for every
  artifact: an empty file, a single file of any size, and a directory tree, including nested
  and empty files. A digest stored by an earlier build still passes `is_up_to_date`.
- R2. **Bounded hashing memory.** Hashing an artifact holds no more than a fixed-size buffer of
  its contents at a time, independent of the size of the artifact or of any file in it.
- R3. **A file that changes while hashing is an error.** If a file's length differs from the
  number of bytes the hash reads, hashing fails with an error naming the file. It does not
  return a digest.

**APK import**

- R4. **APK read from the file.** `import_apk` does not hold the APK's bytes in memory. It
  decompresses only the `classes*.dex` entries, and inflates them in parallel as it does today.
- R5. **APK memory.** On Immich with `--no-native-libs`, the median peak footprint over three
  runs is at most 600 MB, within 10% of the Dex-only repack.
- R6. **In-memory callers still work.** Code that already holds an APK's bytes, such as the tests,
  `xtask/src/dex.rs:413` and `xtask/src/dex.rs:439`, can still parse them without writing a
  file.
- R7. **APK errors kept.** A file that is not a ZIP, a missing file, and a corrupt Dex entry
  still fail the import, with the file's path in the error.

**.NET detection**

- R8. **Same verdict.** `is_cli_image` returns the same answer as today for every file: a .NET
  assembly, a native PE, a non-PE file, a truncated PE, a PE with no CLI header, a missing file,
  and a directory.
- R9. **Header-sized reads.** `is_cli_image` reads only the parts of the file that the checks in
  `Module::read` touch. It does not copy the whole file into memory.

**Unchanged**

- R10. **Import output unchanged.** For every input in the ct-bench app suites, serialized IR is
  byte-identical, and the VMT, source info and artifact digest match the base build.
- R11. **No CLI or format change.** Flags, the store layout, and the import config are unchanged.

## Design

### D1. Streaming `hash_artifact` (R1-R3)

A private helper `hash_file(hasher, path, framed: bool)` opens the file once and reads its
length from the open handle's metadata. If `framed`, it feeds that length as `u64` little-endian
before the contents. It then reads through a reused 64 KiB buffer:

- it retries `Interrupted`;
- it feeds only the bytes each read returns;
- it stops at end of file.

If the total read differs from the length it took from the metadata, it returns an error (R3).

- A single-file artifact calls it with `framed = false`. Today that hashes exactly the
  contents (`ctadl-import/src/project.rs:668`).
- Each file in a directory calls it with `framed = true`, after the path framing it has today
  (`ctadl-import/src/project.rs:654`).

The directory walk, its sorting and its path framing are unchanged. One buffer serves all files
of an artifact.

Invariant: for a file that does not change while it is read, the bytes fed to the hasher are
exactly the bytes fed today. R1 rests on this.

`hash_file_contents` (`ctadl-import/src/project.rs:608`) is unchanged. It sorts model files by
their contents, so it needs them whole, and model files are small.

### D2. APK archive over a shared file handle (R4, R6, R7)

`APKParser` gets a constructor that takes a path, `APKParser::open(path)`. It and
`APKParser::new(&[u8])` share one private generic constructor over a reader
`R: Read + Seek + Clone + Send + Sync`, which keeps today's logic:

- list and sort the Dex entry names;
- clone the archive per rayon task;
- inflate the entry and validate its header.

`Cursor<&[u8]>` already meets that bound, so `new` keeps its signature (R6).

For `open`, the reader is a small `SharedFile` in `dex-reader`: an `Arc<File>` and its own
position. It reads with positioned reads (`FileExt::read_at` on Unix, `seek_read` on Windows), so clones
share the descriptor, don't share a cursor, and need no lock. Cloning the `ZipArchive` per task
still shares the parsed central directory, as it does today. The OS page cache holds the file,
not the process heap.

`import_apk` (`frontends/ctadl-dex/src/lib.rs:91`) calls `APKParser::open(file)` and drops its
`std::fs::read`. The error contexts "reading APK" and "parsing APK", each with the path, stay.

### D3. `is_cli_image` over a memory map (R8, R9)

`is_cli_image` maps the file read-only (`memmap2`, already in the workspace via `lsm`) and runs
the same `Module::read` on the mapping. The verdict comes from the same code on the same bytes,
which gives R8 by construction. Only the pages that the header and metadata checks touch are
read in, and file-backed clean pages don't count toward the process's footprint (R9).

- A file that cannot be opened or mapped gives `false`, as a failed read does today.
- An empty file gives `false`. It cannot be mapped, and `PeImage::parse` rejects anything under
  128 bytes.

The mapping lives only for the call. `Module` borrows from it and is dropped before the call
returns.

### D4. Measurement (R5, R10)

The before/after comparison imports each app in the ct-bench `apps.json` and `fdroid.json`
suites into separate stores, and compares:

- serialized IR, byte for byte;
- the VMT, source info, and import config digests;
- median peak footprint and wall time over three runs (`/usr/bin/time -l`).

Immich and its Dex-only repack are the R5 pair. Report the resource-heavy APKs too: schildi,
loki messenger and osmand.

## Concerns

- C1. **SIGBUS from the memory map (D3).** Accepted: keep the map. If another process
  truncates the file during the parse, the process gets `SIGBUS`. The window is one small
  parse, and keeping a single PE parser is worth more than a header-only copy of it.
- C2. **Hashing errors on a changing file (R3).** Accepted: error. A digest of a mixture could
  later mark a stale import as up to date. The metadata length is the only one that streams
  without changing digests, and checking it costs nothing.
- C3. **`read_at` is Unix-only (D2).** Accepted: support both. `SharedFile` uses `read_at` on
  Unix and `seek_read` on Windows, so a Windows build still compiles. Nothing tests it there.
