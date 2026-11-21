# Import: read only the bytes a step needs: implementation plan - DO-NOT-MERGE

Source: `intent.md`, `spec.md`. This plans the order of the work, the files each step
changes, and the tests that prove it. R* and D* numbers are the spec's. Where this plan and
the spec disagree, the spec wins. Each phase builds,
passes the gate named in it, and is one commit.

## Order

**Scope change.** Phases 2 and 3 were built, then withdrawn with R4, R6 and R7: how the dex
frontend reads an APK is out of scope (`intent.md`). `fc4f38762` reverts both. The intent then took the APK back,
scoped to mapping it for the unchanged reader: phase 6 (R4, R5, R7, D2 as restated). The
branch is phases 1, 4 and 6, and phase 5 is rerun on it.

| Wave | Phases | Why |
| --- | --- | --- |
| 4 | 6 | `import_apk` maps the APK |
| 5 | 5 | acceptance rerun on phases 1, 4 and 6 |

The waves before the scope change:

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

**As built.** Base `b3dd10cf0`. All 25 imports succeeded. Medians of three runs (MB = 10^6 B
of `peak memory footprint`):

| app | peak MB | real s |
| --- | --- | --- |
| Immich | 909.2 | 0.81 |
| Dex-only repack (3.6 MB, one `classes.dex`) | 543.2 | 0.47 |
| schildi | 1463.9 | 1.70 |
| loki messenger | 1287.7 | 1.23 |
| osmand | 2268.2 | 3.06 |

Immich peaks at 909 MB, not the 775 MB in the spec's Background (four runs, 888–911 MB), so
the gap to the repack is ~366 MB, not ~225 MB. R5's threshold (at most 600 MB, within 10% of
the repack) is unchanged and still reachable. Other departures:
- The source info is a directory, `imports/<imp>/source-info/` (five `.parquet` files). Its
  digest is a sha256 over the sorted (path, sha256) list of its files.
- An XAPK's Dex lands in sub-imports `imports/app__<split>`, so digests are kept per import
  directory: 49 rows for 25 inputs.
- `source-info/artifacts.parquet` records the artifact path, which for an XAPK split is inside
  the store. Each import runs in a fixed staging store, then moves, so base and after compare.
- The steps are scripted in `$TMPDIR/read-bytes/run-suite.sh` and `time-five.sh`, which
  phase 5 reruns with the branch binary.

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

**As built.** `584277e48`. Gates pass; the grep shows only `hash_file_contents` and the
test's reference function. Departures:
- The buffer size is a named constant, `HASH_BUF_LEN`. `hash_reader` returns
  `std::io::Result`, and a length mismatch is an `InvalidData` error. `hash_file` adds the path.
- A single-file artifact's read error now says "hashing artifact file: <path>", as a directory
  member's always did, instead of "hashing artifact: <path>".
- `hash_artifact_missing_file_errors` fails at the existing metadata read ("reading artifact
  metadata: <path>"), before `hash_file`. It checks that the path is named, not
  `hash_file`'s message.

## Phase 2: file-backed `APKParser` (R4, R6, R7, D2; additive)

Depends on: phase 0.

**What.** Adds `APKParser::from_file(File)` and the `SharedFile` reader. It has no
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

**As built.** `5c3c2b58e`. Gates pass (dex regression: 71 passed of 71). Departures:
- `apk.rs` has no `tests` module; the four APK tests are in `native_lib_tests`, next to
  `zip_of`. They make temp files with `tempfile::NamedTempFile`.
- `SharedFile::new(File)` returns `io::Result`, since it reads the length.
  `SeekFrom::End`/`Current` that would go below 0 or overflow `u64` is `InvalidInput`.
- `from_file_matches_new` also asserts that `com.noto_54.apk` has two Dex entries.
- The `cfg(windows)` branch is not type-checked: no Windows target is installed here.

**Withdrawn.** Reverted in `fc4f38762` with phase 3; see the scope change under Order.

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

**As built.** `af1776352`. Gates pass (dex regression: 71 passed of 71). Immich, three runs
instead of one because the first was a cold launch: 498.5, 540.3, 487.0 MB peak (base 909 MB);
real 0.88–0.89 s on the warm runs (base 0.81 s). Departures:
- The tests read the whole error chain through a small `error_chain` helper, since the error's
  message shows only the outer context. They use `let ... else`, since `ApkImport` is not
  `Debug`.

**Withdrawn.** Reverted in `fc4f38762`; see the scope change under Order.

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

**As built.** `cdb44113b`. Gates pass; the test ran the fixture cases on all 68 fixture
`.dll`s. Departures:
- The test makes its temp files under `std::env::temp_dir()`, in a directory named with the
  process id: `ctadl-cil` has no `tempfile`, and adding one would change `Cargo.lock` beyond the
  one edge.
- `cargo xtask regression --frontend cil` fails its `cil:models` check with the `model-run` on
  PATH (the nix test env's, which writes paths as lists where `xtask/src/cil/models.rs:107`
  expects strings). That is not this change. It passes (23 of 24, one expected `Cil:delegates`
  xfail) with `CTADL_MODEL_RUN` set to this tree's `frontends/ctadl-cil/tools/ModelRun`, built
  into `$TMPDIR/p4-model-run`. Later full regression runs set it the same way.

## Phase 6: `import_apk` over a memory map (R4, R5, R7, D2)

Depends on: phases 1 and 4 (on the branch; phases 2 and 3 reverted).

**What.** `import_apk` maps the APK instead of reading it, and hands the map to the unchanged
`APKParser::new`. Peak memory drops; output does not change.

**Files.**
- `frontends/ctadl-dex/Cargo.toml` — add `memmap2 = "0.9"`, as phase 4 did for `ctadl-cil`.
- `Cargo.lock` — the new edge from `ctadl-dex` to `memmap2`, and nothing else.
- `frontends/ctadl-dex/src/lib.rs:94` — replace `std::fs::read(file)` with `File::open` and
  `unsafe { memmap2::Mmap::map(&file) }`, both under "reading APK: <path>". The `// SAFETY:`
  comment says the map is read-only, dropped once `APKParser::new` returns, and that
  truncation by another process during the inflate is the accepted SIGBUS risk (spec C3).
  `APKParser::new(&map)` under "parsing APK: <path>" as today. `drop(map)` where `drop(data)`
  is; keep its comment's point. The `PhaseTimer` label "read APK" becomes "map APK".
- `readers/dex-reader`: unchanged.

**Tests.** New, in `frontends/ctadl-dex/src/tests.rs`, with temp files under
`std::env::temp_dir()` and a test-unique name (`ctadl-dex` has no `tempfile`):
- `import_apk_of_a_missing_file_names_it` — the error chain has "reading APK" and the path.
- `import_apk_of_a_non_zip_names_it` — a text file: the chain has "parsing APK" and the path.
- `import_apk_of_an_empty_file_fails` — an empty file is an error naming the path (R7).

Output identity is not retested here: `APKParser::new` gets the same bytes. The dex goldens and
phase 5 check the IR.

**Measure.** Three imports of Immich with `--no-native-libs` under `memguard.sh 16
/usr/bin/time -l`, from a copy of the release binary. Median peak should be near the repack
(543 MB), well under 600 MB. If not, stop and report.

**Gate.**

    cargo test -p ctadl-dex
    cargo clippy -p ctadl-dex --all-targets -- -D warnings
    cargo xtask regression --frontend dex
    git diff b3dd10cf0 -- readers/dex-reader   # empty

**As built.** `ac0ef1f0e`. Gates pass (dex regression: 71 passed of 71; `readers/dex-reader`
unchanged). Immich peak 517.7, 534.4, 534.0 MB, median 534.0 MB, 1.7% under the repack (base
909 MB; phases 1 and 4 alone 724 MB); real 0.86–0.87 s warm. Departures:
- The open and the map share one closure, so one "reading APK" context covers both.
- memmap2 0.9 maps an empty file to an empty slice, so an empty APK fails in `APKParser::new`
  under "parsing APK". The test checks only that the path is named.
- The tests read the whole error chain through an `error_chain` helper, and share an
  `import_apk_of_bytes_fails` helper for the temp file.

## Phase 5: acceptance (R5, R10, R11, D4; no code change)

Depends on: phases 1, 4 and 6 (1, 3 and 4 before the scope change).

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
- R5: Immich median peak at most 600 MB, and within 10% of the repack's median. No timed app's
  median peak above phase 0's.
- R10: no output or digest difference.
- R11: `ctadl import --help` and `ctadl --help` print the same for the phase 0 and phase 5
  binaries, and
  `IMPORT_FORMAT_VERSION` (`ctadl-import/src/project.rs:129`) is unchanged.
- `cargo test --workspace` and `cargo xtask regression` pass.

**As built, with phases 2 and 3 (superseded).** At `c32f59b59`. Every gate passes. Medians of three runs, MB = 10^6 B of peak
footprint. "Base rerun" is the base binary timed again in the same session, to control for
machine load; the plan does not ask for it.

| app | base MB / s | base rerun MB / s | after MB / s |
| --- | --- | --- | --- |
| Immich | 909.2 / 0.81 | 863.8 / 0.84 | 504.3 / 0.87 |
| Dex-only repack | 543.2 / 0.47 | 542.5 / 0.47 | 536.4 / 0.48 |
| schildi | 1463.9 / 1.70 | 1497.6 / 1.72 | 1239.8 / 1.74 |
| loki messenger | 1287.7 / 1.23 | 1284.5 / 1.27 | 887.8 / 1.34 |
| osmand | 2268.2 / 3.06 | 2253.6 / 3.09 | 1881.6 / 3.30 |

- R5: Immich 504.3 MB, at most 600 MB, and 6.0% under the repack's 536.4 MB.
- R10: all 25 imports succeed; the 49 digest rows (IR, VMT, source info, config hash) are
  identical.
- R11: help output identical; `IMPORT_FORMAT_VERSION` unchanged.
- `cargo test --workspace` (2109 passed), `cargo xtask regression` (255 passed, 5 xfail of
  260, with `CTADL_MODEL_RUN` as in phase 4's note), and clippy pass. The greps hold.
- Wall time is a little higher than the base rerun: Immich +0.03 s, loki +0.07 s, osmand
  +0.21 s (about 7%). No requirement covers it.

**As built, phases 1 and 4 only (stopped).** Rerun at `5890686da`, stopped after the imports
and the branch's timing; no control rerun, tests or regression. All 49 digest rows match
phase 0. Median peak MB / real s: Immich 724.4 / 0.83, repack 539.2 / 0.49, schildi
1269.1 / 1.82, loki 1010.9 / 1.27, osmand 1912.1 / 3.07. Streaming the hash alone takes ~185 MB
off Immich, about one APK copy.

## Files that change

| File | Change | Phase |
| --- | --- | --- |
| `ctadl-import/src/project.rs` | streaming `hash_artifact`; `hash_file`, `hash_reader`; tests | 1 |
| `frontends/ctadl-cil/Cargo.toml` | `memmap2` | 4 |
| `Cargo.lock` | `memmap2` edge | 4 |
| `frontends/ctadl-cil/src/lib.rs` | `is_cli_image` maps the file | 4 |
| `frontends/ctadl-cil/src/tests.rs` | verdict equivalence test | 4 |
| `frontends/ctadl-dex/Cargo.toml` | `memmap2` | 6 |
| `Cargo.lock` | `memmap2` edge from `ctadl-dex` | 6 |
| `frontends/ctadl-dex/src/lib.rs` | `import_apk` maps the APK | 6 |
| `frontends/ctadl-dex/src/tests.rs` | error tests | 6 |

Not touched:
- `ctadl-ascent/src/main.rs:1113`, the hashing thread. Its memory falls with phase 1.
- `APKParser` and the rest of `readers/dex-reader`: out of scope since phases 2 and 3 were
  withdrawn. `xtask/src/dex.rs:413` and `:439` keep `APKParser::new`.
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
  - `grep -n 'fs::read' frontends/ctadl-dex/src/lib.rs` shows none in `import_apk`;
  - `grep -n 'fs::read' frontends/ctadl-cil/src/lib.rs` shows none in `is_cli_image`;
  - in `hash_artifact` (`ctadl-import/src/project.rs`), none.

## Deferred

- **Ghidra project digests may include lock files.** A local Ghidra project directory can be
  the artifact (`frontends/ctadl-pcode/src/ghidra.rs:86`). Ghidra runs on it
  (`ghidra.rs:261`) while the hash thread walks it, and Ghidra may write lock files there.
  If so, the digest can already vary, or fail when a lock file vanishes during the walk. R3
  adds only a narrow new failure, a lock file written between its metadata read and its read.
  This predates the work; check separately whether that digest is stable.
