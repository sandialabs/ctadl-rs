# Import: read only the bytes a step needs: requirements and design - DO-NOT-MERGE

Source: `intent.md`. This spec refines that intent, and must stay consistent with
`docs/spec.md`, which it does not change. It does not sequence the work.

## Background

Four steps of `ctadl import` read a whole artifact into memory with `std::fs::read`. This work
changes all four. For the APK and the JAR it changes only where the package's bytes live: the
readers themselves are out of scope (`intent.md`).

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
- `ctadl_jvm::import_jar` (`frontends/ctadl-jvm/src/lib.rs:73`) calls `JarFileParser::open`
  (`readers/jvm-reader/src/jar.rs:24`), which reads the whole JAR and passes the bytes to
  `JarFileParser::from_bytes`. That uses only the ZIP central directory and the `.class`
  entries, inflated in parallel, each task over a clone of the archive.

Baseline (`ctadl import --no-native-libs`, median of three runs, measured in plan phase 0):
Immich (`fdroid/app.alextran.immich_3030699.apk` in ct-bench) is a 180 MB APK with 7.6 MB of
Dex. Its import peaks at 909 MB of physical footprint and takes 0.81 s. The same Dex repacked
alone into a 3.6 MB APK peaks at 543 MB and takes 0.47 s. Of the ~366 MB gap, one APK-sized
copy is the hashing thread's; the other is `import_apk`'s.

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

- R4. **APK mapped, not read.** `import_apk` does not copy the APK into the heap. It maps the
  file read-only and gives the mapping to `APKParser::new`. `APKParser` and the rest of
  `dex-reader` do not change.
- R5. **APK memory.** On Immich with `--no-native-libs`, the median peak footprint over three
  runs is at most 600 MB, within 10% of the Dex-only repack. No other app timed in D4 has a
  higher median peak than at base.
- R6. Withdrawn: `APKParser::new(&[u8])` does not change, so code that holds an APK's bytes is
  untouched.
- R7. **APK errors kept.** A missing file, an empty file, a file that is not a ZIP, and a
  corrupt Dex entry still fail the import, with the file's path in the error.

**JAR import**

- R12. **JAR mapped, not read.** `import_jar` does not copy the JAR into the heap. It maps the
  file read-only and gives the mapping to `JarFileParser::from_bytes`. `jvm-reader` does not
  change.
- R13. **JAR memory.** For a JAR that is a real JAR plus one large stored resource, the median
  peak footprint of its import over three runs is within 10% of the real JAR's alone.
- R14. **JAR errors kept.** A missing file, an empty file, a file that is not a ZIP, and a
  corrupt class entry still fail the import, with the file's path in the error.
- R15. **JAR output unchanged.** For the JARs of D5's measurement, serialized IR, VMT, source
  info and artifact digest match the base build.

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

### D2. `import_apk` over a memory map (R4, R5, R7)

`import_apk` (`frontends/ctadl-dex/src/lib.rs:94`) replaces its `std::fs::read` with:

- open the file, and map it read-only (`memmap2`, as D3), both under the context
  "reading APK: <path>", which keeps the OS error;
- `APKParser::new(&map)` under "parsing APK: <path>", as today;
- drop the map once `APKParser::new` returns, where it drops the bytes today. The parser keeps
  only the inflated Dex buffers.

`APKParser::new` reads the central directory and the `classes*.dex` entries through the map, so
only those pages are read in, and file-backed clean pages don't count toward the footprint.
Its parallel inflate is unchanged: each task's `Cursor` clone borrows the same mapping.

An empty file still fails (R7): either the map fails, under "reading APK", or it gives an
empty slice, which `APKParser::new` rejects as not a ZIP, as it rejects an empty buffer today.

### D3. `is_cli_image` over a memory map (R8, R9)

`is_cli_image` maps the file read-only (`memmap2`, already in the workspace via `lsm`) and runs
the same `Module::read` on the mapping. The verdict comes from the same code on the same bytes,
which gives R8 by construction. Only the pages that the header and metadata checks touch are
read in, and file-backed clean pages don't count toward the process's footprint (R9).

- A file that cannot be opened or mapped gives `false`, as a failed read does today.
- An empty file gives `false`. It maps to an empty slice, which `PeImage::parse` rejects, as it
  does anything under 128 bytes.

The mapping lives only for the call. `Module` borrows from it and is dropped before the call
returns.

### D5. `import_jar` over a memory map (R12–R15)

`import_jar` replaces `JarFileParser::open(file)` with: open the file and map it read-only
(`memmap2`), then `JarFileParser::from_bytes(&map)`, all under today's single context
"reading jar: <path>", so error messages read as today. The map is dropped once `from_bytes`
returns; the parser keeps only the parsed classes. The other callers of `JarFileParser::open`
(xtask, the jvm-reader example and tests) keep it.

Measurement: a real JAR with little besides classes (Ghidra's `Base.jar` or one like it), and
the same JAR with a 150 MB stored resource of random bytes added. The pair plays the role of
Immich and its Dex-only repack. Both are imported with the base and the branch binaries, three
runs each, and their outputs compared as in D4.

### D4. Measurement (R5, R10)

The before/after comparison imports each app in the ct-bench `apps.json` and `fdroid.json`
suites into separate stores, and compares:

- serialized IR, byte for byte;
- the VMT, source info, and import config digests;
- median peak footprint and wall time over three runs (`/usr/bin/time -l`).

Immich is the R5 app; its Dex-only repack shows the floor the APK copy in `import_apk` keeps
it above. Report the resource-heavy APKs too: schildi, loki messenger and osmand.

## Concerns

- C1. **SIGBUS from the memory map (D3).** Accepted: keep the map. If another process
  truncates the file during the parse, the process gets `SIGBUS`. The window is one small
  parse, and keeping a single PE parser is worth more than a header-only copy of it.
- C2. **Hashing errors on a changing file (R3).** Accepted: error. A digest of a mixture could
  later mark a stale import as up to date. The metadata length is the only one that streams
  without changing digests, and checking it costs nothing.
- C4. **SIGBUS from the JAR map (D5).** Accepted, as C3: the window is the inflate of the
  `.class` entries.
- C3. **SIGBUS from the APK map (D2).** Accepted, as C1. If another process truncates the APK
  while `APKParser::new` inflates it, the process gets `SIGBUS`. The window is the inflate of
  the Dex entries, longer than D3's. Reading through a file handle instead
  would avoid it, but changes the APK reader, which is out of scope.
