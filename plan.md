# Import: read only the bytes a step needs: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order of the work, the files each step
changes, and the tests that prove it. R* and D* numbers are the spec's. Where this plan and
the spec disagree, the spec wins, except for a correction listed below. Each phase builds,
passes the gate named in it, and is one commit.

## Corrections to the spec

- P1. **One call can't carry both APK error contexts.** D2 has `import_apk` call
  `APKParser::open(path)` and keep both contexts, "reading APK" and "parsing APK". With one
  call, an open failure and a parse failure come out of the same call, so only one context can
  wrap them. `DexError` (`readers/dex-reader/src/error.rs:4`) also has no I/O variant, so
  `open` would flatten the `io::Error` of a missing file into a `&'static str`, as
  `native_lib_entries_of_file` does (`readers/dex-reader/src/apk.rs:101`). This plan assumes,
  until settled: `APKParser::from_file(std::fs::File)` in place of `open(path)`. `import_apk`
  opens the file itself under "reading APK: <path>", which keeps the `io::Error`. It then calls
  `from_file` under "parsing APK: <path>".

## Order

| Wave | Phases | Why |
| --- | --- | --- |
| 0 | 0 | baseline on the unchanged tree |
| 1 | 1, 2, 4 | hashing, the APK reader, and .NET detection; disjoint files |
| 2 | 3 | wires phase 2's constructor into `import_apk` |
| 3 | 5 | acceptance measurements against phase 0 |

The three changes are independent: each touches its own crate. Phase 2 adds the file-backed APK
constructor with no callers, and phase 3 switches `import_apk` to it. So a change in APK import
memory or output has one cause, phase 3, and phase 2 is tested on its own first. Phase 4 alone
changes `Cargo.lock`, since it is the only phase that adds a dependency. Phase 1 changes
the only code that computes the artifact digest, so phase 5 checks the digests against phase 0.

Test commands used throughout:

    cargo test --workspace
    cargo xtask regression [--frontend dex|cil]
    cargo clippy --workspace --all-targets -- -D warnings

Measurement runs go under the memory guard skill's `memguard.sh` at a 16 GiB cap, with
`/usr/bin/time -l` inside it, one run at a time. Their outputs (stores, `time -l` logs, digests)
go under `$TMPDIR`, never in the repo. The ctadl binary they run is a copy, taken out of the
target directory, so no later build replaces it while it runs.

## Phase 0: baseline (no code change)

Depends on: nothing.

**What.** Records the "before" numbers and outputs on the unchanged tree, for R5 and R10.

**Steps.**
1. Build `cargo build --release -p ctadl-ascent` at the base commit. Copy the `ctadl` binary to
   `$TMPDIR/read-bytes/base/ctadl`.
2. Make the Dex-only repack of Immich: extract the `classes*.dex` entries of
   `app.alextran.immich_3030699.apk` (path in ct-bench `bench/suites/fdroid.json`) and zip them
   alone into `$TMPDIR/read-bytes/immich-dex-only.apk`. Check that it is about 3.6 MB.
3. For each of the 25 inputs in ct-bench `bench/suites/apps.json` (14) and
   `bench/suites/fdroid.json` (11), import it once into its own store:
   `ctadl --store <store> import --no-native-libs -n app <path>`. Keep the stores. Native libs
   are off because their extraction already reads the APK from the file
   (`ctadl-frontends/src/apk_native.rs:84`), none of this work touches it, and it needs Ghidra.
4. From each store, record `sha256` of `imports/app/ir-program.bitcode` and
   `imports/app/ir-vmt.bitcode`, the source info file next to them, and the `hash` field of
   `imports/app/import_config.json`.
5. Time Immich, the repack, schildi (`chat-schildi-android`), loki messenger
   (`network-loki-messenger-fdroid`) and osmand (`net-osmand-plus`): three imports each,
   recording `peak memory footprint` and `real` from `time -l`. Report medians.

**Gate.** The baseline table exists: for Immich, about 775 MB and 0.79 s; for the repack, about
550 MB and 0.45 s. If not, report the numbers before going on: R5's threshold rests on them.

## Phase 1: streaming `hash_artifact` (R1, R2, R3, D1)

Depends on: phase 0.

**What.** `hash_artifact` reads each file through one reused 64 KiB buffer instead of
`std::fs::read`. A file whose length differs from the bytes read is an error. Digests do not
change.

**Files.**
- `ctadl-import/src/project.rs:641` — `hash_artifact`. Replace the whole-file reads at `:658`
  (directory file) and `:665` (single file) with calls to a new private
  `hash_file(hasher, path, framed, buf)`. Keep the directory walk, sorting and path framing
  (`:646`–`:657`) as they are. Allocate the 64 KiB buffer once in `hash_artifact` and pass it to
  each call.
- Same file, new private helpers next to `hash_artifact`:
  - `hash_file` opens the file, takes its length from `File::metadata` on the open handle,
    and calls `hash_reader`. It adds the path to any error.
  - `hash_reader(hasher, reader: impl Read, len: u64, framed, buf)` feeds `len` as `u64`
    little-endian when `framed`, then loops: retry `ErrorKind::Interrupted`, feed
    `&buf[..n]`, stop at `n == 0`. If the total differs from `len`, it returns an error. It is
    split from `hash_file` so the test can give it a reader whose length disagrees.
- `ctadl-import/src/project.rs:608` — `hash_file_contents`: unchanged (D1).

**Tests.** New, in the `tests` module of `ctadl-import/src/project.rs` (`:1222`):
- `hash_artifact_digest_unchanged` — a reference function in the test copies today's code
  (whole-file `std::fs::read`, same framing). For each input below, `hash_artifact` equals it:
  - single files of 0, 1, 65 535, 65 536, 65 537 and 200 000 bytes of a non-repeating
    pattern;
  - a directory with nested subdirectories, an empty file, a file of exactly 64 KiB, and a
    file over 64 KiB;
  - an empty directory.
- `hash_artifact_known_digests` — a single file `hello` gives
  `2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824`, and an empty file gives
  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` (plain SHA-256, as
  today). This pins R1 against a stored digest without the reference function.
- `hash_reader_rejects_length_mismatch` — `hash_reader` over a `Cursor` of 10 bytes with
  `len` 11, and with `len` 9, returns an error. A reader that returns `Interrupted` once and
  then the bytes gives the same digest as the bytes alone.
- `hash_artifact_missing_file_errors` — a missing path is an error that names the path.

Existing, still passing: `test_hash_artifact_file_and_dir`
(`ctadl-ascent/tests/integration/cli.rs:1313`), and every test that runs
`is_up_to_date`/`record_artifact_hash`.

Regression: none expected. Digests in phase 5 must match phase 0.

A single-file artifact that is not a regular file (a FIFO, a device) reports length 0, so it
now fails to hash where `std::fs::read` read it to the end. R3 asks for that. Directory members
are unaffected: `collect_files` keeps only regular files (`ctadl-import/src/project.rs:684`).

**Gate.**

    cargo test -p ctadl-import
    cargo test -p ctadl-ascent --test integration hash_artifact
    cargo clippy -p ctadl-import --all-targets -- -D warnings
    grep -n 'fs::read(' ctadl-import/src/project.rs   # only hash_file_contents and unrelated loads

## Phase 2: file-backed `APKParser` (R4, R6, R7, D2; additive)

Depends on: phase 0.

**What.** Adds `APKParser::from_file(File)` (see P1) and the `SharedFile` reader. It has no
callers yet. `APKParser::new(&[u8])` keeps its signature and behavior.

**Files.**
- New `readers/dex-reader/src/shared_file.rs` — `pub(crate) struct SharedFile { file:
  Arc<File>, pos: u64, len: u64 }`, `Clone`. `Read` does a positioned read at `pos`
  (`std::os::unix::fs::FileExt::read_at` under `cfg(unix)`,
  `std::os::windows::fs::FileExt::seek_read` under `cfg(windows)`) and advances `pos`. `Seek`
  updates `pos` only; `SeekFrom::End` uses `len`, taken once from the metadata. Seeking before
  0 is an `InvalidInput` error. No lock.
- `readers/dex-reader/src/lib.rs` — `mod shared_file;`.
- `readers/dex-reader/src/apk.rs:490` — move the body of `new` into a private
  `fn from_reader<R: Read + Seek + Clone + Send + Sync>(reader: R) -> DexResult<Self>`. Keep
  the name listing and sorting, the per-task `archive.clone()` (`:509`), the inflate and
  `parse_dex_header` exactly as they are. `new` calls it with `Cursor::new(apk_bytes)`.
  `from_file(file: File)` calls it with `SharedFile::new(file)`. If the metadata can't be read,
  that is an `InvalidDex("cannot read APK")` error.
- `readers/dex-reader/src/apk.rs:50`–`:51` — the comment says `new` drops the archive. Make it
  say both constructors do.

**Tests.** New, in the `tests` module of `readers/dex-reader/src/apk.rs`:
- `from_file_matches_new` — on `xtask/tests/dex/com.noto_54.apk` (via `CARGO_MANIFEST_DIR`),
  `from_file` and `new` give the same `dex_count`, and the same names and buffers from
  `dex_parsers_with_filenames`, in the same order. It has two Dex entries, so this runs the
  parallel inflate over clones of the shared file. Immich, the R5 app, has only one.
- `from_file_of_an_apk_with_no_dex_is_empty` — the `zip_of` APK of
  `an_apk_with_no_dex_entries_parses_as_empty` (`:721`), written to a temp file, gives
  `dex_count() == 0`.
- `from_file_rejects_a_non_zip` — a temp file holding `b"not a zip at all"` is an error.
- `from_file_rejects_a_corrupt_dex_entry` — `zip_of(&[("classes.dex", b"garbage")])` is an
  error, from both `new` and `from_file`.

New, in `readers/dex-reader/src/shared_file.rs`:
- `clones_keep_their_own_position` — two clones over one temp file read different offsets in
  alternation and each gets its own bytes.
- `seek_and_read_past_end` — `SeekFrom::End(-3)` then reads give the last 3 bytes, then 0.
  Seeking before the start is an error.

Temp files: `dex-reader` already has `tempfile` as a dev-dependency.

Existing, still passing: every `dex-reader` test, including `a_non_zip_is_still_rejected`
(`:734`).

**Gate.**

    cargo test -p dex-reader
    cargo clippy -p dex-reader --all-targets -- -D warnings
    cargo xtask regression --frontend dex

## Phase 3: `import_apk` reads the APK from the file (R4, R5, R7, D2)

Depends on: phase 2.

**What.** `import_apk` stops reading the whole APK. Peak memory drops; output does not change.

**Files.**
- `frontends/ctadl-dex/src/lib.rs:94` — replace `std::fs::read(file)` with
  `std::fs::File::open(file)` under the context "reading APK: <path>". Then call
  `APKParser::from_file` under "parsing APK: <path>". Drop `drop(data)` (`:99`) and its comment.
  The `PhaseTimer` label "read APK" becomes "open APK"; the others stay.

**Tests.** New, in `frontends/ctadl-dex/src/tests.rs`:
- `import_apk_of_a_missing_file_names_it` — the error's chain contains "reading APK" and the
  path.
- `import_apk_of_a_non_zip_names_it` — a temp file holding text: the error contains
  "parsing APK" and the path.

A corrupt Dex entry is not retested here: `ctadl-dex` has no `zip` to build one, phase 2's
`from_file_rejects_a_corrupt_dex_entry` shows `from_file` rejects it, and every `from_file`
error goes through the "parsing APK" context that the non-ZIP test checks.

Temp files go under `std::env::temp_dir()` with a test-unique name, as
`listing_from_a_file_matches_listing_from_bytes` does (`readers/dex-reader/src/apk.rs:706`):
`ctadl-dex` has no `tempfile` dev-dependency, and two tests don't need one. Output identity
is not retested here. Phase 2's `from_file_matches_new` shows both constructors give the same
Dex buffers, and the dex goldens and phase 5 check the IR.

Existing, still passing: `a_class_global_is_declared_at_its_class_def_item`
(`frontends/ctadl-dex/src/tests.rs:758`), and `xtask/src/dex.rs:413` and `:439`, which still use
`APKParser::new` (R6).

Regression: dex goldens unchanged.

**Measure.** One import of Immich with `--no-native-libs` under `memguard.sh 16 /usr/bin/time
-l`. Its peak footprint should be near the phase 0 repack, well under 600 MB. If not, stop
and report: phase 5 would fail R5.

**Gate.**

    cargo test -p ctadl-dex
    cargo clippy -p ctadl-dex --all-targets -- -D warnings
    cargo xtask regression --frontend dex

## Phase 4: `is_cli_image` over a memory map (R8, R9, D3)

Depends on: phase 0.

**What.** `is_cli_image` maps the file read-only and runs `Module::read` on the map. The
verdict is unchanged.

**Files.**
- `frontends/ctadl-cil/Cargo.toml` — add `memmap2 = "0.9"` (the version `lsm/Cargo.toml:7`
  uses; `Cargo.lock` already has 0.9.11). cil-reader's own `mmap` feature
  (`readers/cil-reader/crates/cil-reader/Cargo.toml:10`) is not used: nothing in its source
  reads it.
- `Cargo.lock` — the new edge from `ctadl-cil` to `memmap2`. No other phase touches it.
- `frontends/ctadl-cil/src/lib.rs:443` — open the file; on error, `false`. `unsafe {
  memmap2::Mmap::map(&file) }`, with a `// SAFETY:` comment (the workspace denies
  `undocumented_unsafe_blocks`, `Cargo.toml:127`). The comment says the map is read-only and
  dropped before return, and that truncation by another process during the parse is the
  accepted SIGBUS risk. On a map error, `false`. Then `Module::read(&map).is_ok()`. The
  `Module` is a temporary, dropped before the map.

**Tests.** New, in `frontends/ctadl-cil/src/tests.rs`:
- `is_cli_image_matches_whole_file_read` — a reference function in the test is today's
  `std::fs::read(path).is_ok_and(|d| Module::read(&d).is_ok())`. Check that `is_cli_image`
  agrees with it on each input, and that the verdicts are as expected:
  - every `*.dll` in `$CIL_READER_FIXTURES` (true);
  - one fixture truncated to 0, 64, 127, 128 and 300 bytes, half its length, and its length
    minus 1. Assert `false` for 0, 64 and 127 (under the 128-byte minimum,
    `readers/cil-reader/crates/cil-reader/src/raw/pe.rs:218`). For the rest assert only
    agreement with the reference: heap slices are clamped to the end of the file and no row
    is read, so a long truncation can still be a CLI image;
  - a copy of one fixture with the CLI header data directory zeroed, a PE with no CLI header,
    which takes the same path as a native PE (false). With `lfanew` the `u32` at `0x3C`, zero
    the 8 bytes at `lfanew + 232` for PE32 (optional-header magic `0x10b` at `lfanew + 24`)
    or `lfanew + 248` for PE32+ (`0x20b`). Both the address and the size must be zero
    (`DataDirectory::is_zero`, `raw/pe.rs:99`);
  - a text file (false);
  - a missing path (false);
  - a directory (false).

  If `CIL_READER_FIXTURES` is unset, the fixture cases are skipped with a printed note, as the
  `cil:*` regression checks do. The non-fixture cases always run.

Existing, still passing: `cargo xtask regression --frontend cil`, which imports CIL
directories through `import_directory` (`frontends/ctadl-cil/src/lib.rs:171`).

**Gate.**

    cargo test -p ctadl-cil
    cargo clippy -p ctadl-cil --all-targets -- -D warnings
    cargo xtask regression --frontend cil
    grep -n 'fs::read' frontends/ctadl-cil/src/lib.rs   # none in is_cli_image

## Phase 5: acceptance (R5, R10, R11, D4; no code change)

Depends on: phases 1, 3, 4.

**What.** Runs phase 0's steps 3–5 with a release build of the branch, and compares.

**Steps.**
1. `cargo build --release -p ctadl-ascent`, and copy the binary to
   `$TMPDIR/read-bytes/after/ctadl`.
2. Import all 25 suite inputs into new stores, as in phase 0 step 3.
3. Compare with phase 0 (R10): `ir-program.bitcode` and `ir-vmt.bitcode` byte for byte, the
   source info file, and the `hash` field of `import_config.json`. Any difference fails.
4. Three timed imports of the five apps of phase 0 step 5. Report median peak footprint and
   wall time, before and after.

**Gate.**
- R5: Immich median peak at most 600 MB, and within 10% of the repack's median.
- R10: no output or digest difference.
- R11: `ctadl import --help` and `ctadl --help` print the same for the phase 0 and phase 5
  binaries, and
  `IMPORT_FORMAT_VERSION` (`ctadl-import/src/project.rs:129`) is unchanged.
- `cargo test --workspace` and `cargo xtask regression` pass.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `ctadl-import/src/project.rs` | streaming `hash_artifact`; `hash_file`, `hash_reader`; tests | 1 |
| `readers/dex-reader/src/shared_file.rs` | new: `SharedFile` and its tests | 2 |
| `readers/dex-reader/src/lib.rs` | `mod shared_file` | 2 |
| `readers/dex-reader/src/apk.rs` | `from_reader`, `from_file`; comment; tests | 2 |
| `frontends/ctadl-dex/src/lib.rs` | `import_apk` opens the file | 3 |
| `frontends/ctadl-dex/src/tests.rs` | error and output-identity tests | 3 |
| `frontends/ctadl-cil/Cargo.toml` | `memmap2` | 4 |
| `Cargo.lock` | `memmap2` edge | 4 |
| `frontends/ctadl-cil/src/lib.rs` | `is_cli_image` maps the file | 4 |
| `frontends/ctadl-cil/src/tests.rs` | verdict equivalence test | 4 |

Not touched:
- `ctadl-ascent/src/main.rs:1113`, the hashing thread. Its memory falls with phase 1.
- `xtask/src/dex.rs:413` and `:439`, and `readers/dex-reader/examples/dex-reader.rs:73`: they
  keep `APKParser::new` (R6).
- `ctadl-frontends/src/apk_native.rs`, which already reads native libraries from the file.
- `import_files_with_stats` (`frontends/ctadl-cil/src/lib.rs:188`), which reads the selected
  assemblies whole to import them. The spec does not cover it.
- `hash_file_contents` (`ctadl-import/src/project.rs:608`).

## Acceptance

- Phase 5's gate: R5 and R10 against phase 0, with the before/after table for Immich, the
  repack, schildi, loki messenger and osmand.
- `cargo test --workspace`, `cargo xtask regression`, and `cargo clippy --workspace
  --all-targets -- -D warnings` pass.
- Greps:
  - `grep -n 'fs::read' frontends/ctadl-dex/src/lib.rs` shows no read of the APK in
    `import_apk`;
  - `grep -n 'fs::read' frontends/ctadl-cil/src/lib.rs` shows none in `is_cli_image`;
  - in `hash_artifact` (`ctadl-import/src/project.rs`), none.

## Deferred

- **Ghidra project digests may include lock files.** A local Ghidra project directory can be
  the artifact (`frontends/ctadl-pcode/src/ghidra.rs:86`). Ghidra runs on it
  (`ghidra.rs:261`) while the hash thread walks it, and Ghidra may write lock files there.
  If so, the digest can already vary, or fail when a lock file vanishes during the walk. R3
  adds only a narrow new failure, a lock file written between its metadata read and its read.
  This predates the work; check separately whether that digest is stable.
