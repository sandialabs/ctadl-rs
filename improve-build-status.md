# Build-time investigation: status - DO-NOT-MERGE

Updated 2026-10-21. Timings are on the 20-core Mac, Rust 1.94.1, quiet machine,
measured from a separate worktree. Edit-loop numbers are the mean of four
builds (edit, revert, edit, revert) of `cargo build --workspace --all-targets`.

## Results

| | before (221fba6a) | now |
|---|---|---|
| clean build | 202s | 145–148s |
| one-line edit, `index_engine/mod.rs` | 38–44s | 12.6s |
| one-line edit, `facts.rs` | ~40s | 14s |
| one-line edit, `query_engine` | ~40s | 9s |
| `touch` (no-op change) | 26s | 8–11s |
| `cargo xtask regression` (all frontends) | 413s | 137s |
| `xtask regression`'s `cargo build` of `ctadl` after a workspace build | 125s (50 crates) | 0s |

## Commits on this branch

1. `82f7795f` Dev profile: line tables for our crates, no debuginfo for deps.
2. `69773bd3` `ctadl-ascent`'s integration tests are one binary (`tests/it`).
3. `6ca449ed` `ctadl-index` crate: `error`, `facts`, `index_engine`. It uses
   `datafusion-common`, not `datafusion`, so it builds alongside datafusion.
4. `e15d8463` One crate per index engine (`ctadl-index/engines/{serial,par,lsm}`).
   An incremental `ctadl-index` compile spent 16 of 20 s in dead-code analysis
   over the three generated `run_timeout` bodies (~2.3 MB each). The dispatcher
   (`taint_index`, `taint_index_with_config`) now lives in `ctadl-ascent`'s
   `index_engine` module.
5. `676d440e` `xtask regression` builds `ctadl` with `--workspace`. Under
   `-p ctadl-ascent`, xtask's `boon` is not selected, so `idna` loses its
   `default` feature and 50 crates get a second copy. `cargo xtask` itself
   resolves like the workspace build.
6. `f9475c2a` `cil-reader`, `ctadl-cil`, `ctadl-ir` at `opt-level = 1` in dev:
   Mono.Android.dll imports in 72 s instead of 226 s; edit loops unchanged.
7. `b3eebc8f` The regression report ends with the slowest tasks.
8. `a644e87a` Long tasks start first; `cil:frontend` and
   `apk:manifest-and-intents` are tasks of their own; the CIL sweep starts with
   the largest assembly.
9. `d5007f27` The apk index check runs `ctadl index -j 0`.

## Decided against, with numbers

- **`opt-level = 1` for `ctadl-index` and the engine crates.** The apk check's
  index goes from 130 s to 78 s, and an ordinary edit to `index_engine` builds a
  little faster (12.8 s vs 15.1 s, smaller objects to link). But an edit to the
  rules themselves (`index_rules`) goes from 30.5 s to 48 s, because O1 has to
  re-optimize the generated bodies. Worth it if rule edits are rare.
- **`opt-level = 1` for all third-party deps.** Mono.Android import 72 → 57 s,
  but every clean build compiles 400 crates with optimizations.
- **lld**: −1.5 s on a 30-link `touch`, less now.
- **`resolver.feature-unification`**: still needs `-Zfeature-unification`.
- Earlier: trimming datafusion features (noise), sccache (0 Rust hits).

## Where the time goes now

- `index_engine` edit (12.6 s): `ctadl-index` incremental, then the three engine
  crates in parallel (~6.5 s each), then `ctadl-ascent` (~2 s) and the links.
- `cargo xtask regression` (137 s): `apk:manifest-and-intents` 118 s (import,
  then a debug-build `ctadl index -j 0`; the engine's fixpoint is most of it),
  `apk` checks 107 s (`ctadl report --format json` alone is 42 s), `cil:frontend`
  88 s (Mono.Android.dll).

## Still open

- Shared target dir across worktrees (needs discussion; unchanged).

## How to reproduce the measurements

Scripts are in the session scratchpad
(`/private/tmp/claude-501/-Users-dbueno-proj-improve-build-times/cacc094a-17d2-46bd-a919-a2184e57e974/scratchpad/`),
which is not permanent: `bench.sh TAG [clean|warm]` (with `PROBE=`, `NAME=`,
`BUILD=`) and `pgtimeout.pl` (kills the whole process group on timeout or
signal). `bench.sh` checks that each step did or did not recompile
`ctadl-ascent` as expected and restores the probe file even on failure. Run it
from a separate worktree, so the tree being edited isn't the one being measured.
