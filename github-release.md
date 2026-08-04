# GitHub release builds — work in progress - DO-NOT-MERGE

Task: on a new `x.y.z` tag, have CI create a GitHub release carrying macOS
(aarch64), Linux (x86_64), and Windows binaries plus a SHA-256 file for each,
with Nix doing the builds.

## Files changed

- `nix/release.nix` — new; holds the whole release build definition.
- `flake.nix` — imports it as `release`, exposed as
  `legacyPackages.<system>.release.<target>`; new `fenix` flake input. The
  change is 23 added lines and nothing else.
- `flake.lock` — records `fenix` (and its `rust-analyzer-src`).
- `.github/workflows/release.yml` — new workflow.
- `Cargo.toml` and every member manifest — one version in the tree now, set at
  the root and inherited. Three programs became cargo examples and `sarif-util`
  is gone. See "Versioning" below.
- `Cargo.lock` — two lines: `rustc_graphviz` moves off upstream's 0.0.0, and
  `ctadl-ascent` picks up `percent-encoding`.
- `flake.nix` also gains a `workspaceVersion` and rewires the four naersk
  derivations that used to build the reader crates standalone.
- `xtask/src/regression.rs` — a comment that claimed the readers were
  workspace-excluded.

All of this is now committed (`One version to rule them all`). While it was in
progress, `nix/release.nix` and `.github/workflows/release.yml` had to be at
least `git add -N`'d: a flake's source is its *tracked* git tree, so an
untracked file does not exist as far as `nix build` is concerned.

## What `nix/release.nix` does

`legacyPackages.<system>.release.<target>` builds the single `ctadl` binary for
one target. Three targets:

| target | built on | how |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | x86_64 Linux | cross to musl, `-C target-feature=+crt-static` |
| `x86_64-pc-windows-gnu` | x86_64 Linux | cross via `pkgsCross.mingwW64` |
| `aarch64-apple-darwin` | aarch64 macOS | native; dylib refs rewritten to `/usr/lib` |

Build one with:

```
nix build '.#legacyPackages.<system>.release."<target>"'
```

Decisions worth remembering, all of which are commented in `nix/release.nix`:

- **Why not `packages.default`.** Its binary resolves libraries under
  `/nix/store`, so it only runs on a machine that has one. Verified: the stock
  `nix build .#default` binary links `/nix/store/…-libiconv/lib/libiconv.2.dylib`.
- **Why `fenix`.** naersk runs cargo on the builder, so a cross build needs a
  rustc carrying the *target's* std. nixpkgs' rustc ships std only for its own
  host. Nothing outside `release` uses fenix.
- **Why `legacyPackages`, not `packages`.** `nix flake check` builds every
  `packages.<system>.<name>`, which would drag these slow (and, on the wrong
  builder, impossible) cross builds into the per-PR checks. Same reasoning the
  existing `legacyPackages.examples.nginx` comment gives.
- **`copyBinsFilter`, not `cargo build --bin ctadl`.** naersk reuses the build
  options for a first pass over a stub source tree where no bin target of this
  workspace exists yet; `--bin ctadl` fails there with "no bin target named
  `ctadl`". Filtering at the copy step keeps `xtask` out of `$out` instead.
- **Fat LTO, and only here.** The tree builds with `lto = "thin"`. These three
  derivations override that to `"fat"` with `CARGO_PROFILE_RELEASE_LTO`, so
  LLVM optimizes the whole program as one module instead of trading precision
  for parallelism. The override sits in `nix/release.nix` rather than in
  `[profile.release]` so that only a release pays the longer link; a
  developer's `cargo build --release` keeps thin LTO. Measured below: 23% off
  the binary, and one stage of the build about eight times longer.
- **`pkgsCross.musl64`, not `pkgsStatic`.** pkgsStatic follows the *builder's*
  architecture, so it would quietly emit aarch64 code on an aarch64 builder.
- **`depsBuildBuild`, not `nativeBuildInputs`, for the cross cc.** The latter's
  cc-wrapper setup hook would claim the plain `cc`/`CC` that build scripts —
  which must target the builder — rely on. The former still puts the prefixed
  binaries on PATH, which is all `crossToolEnv` needs.
- **winpthreads by `-L native=…`, not a build input.** It is a Windows library;
  any dependency slot of a native derivation rejects it on `meta.platforms`
  ("Refusing to evaluate package 'mingw_w64-pthreads'…").
- **macOS relocatability.** rustc links libiconv out of the store. `postInstall`
  rewrites such load commands to `/usr/lib/<name>` — macOS resolves
  `/usr/lib/libiconv.2.dylib` from the dyld shared cache even though no such
  file exists — then fails the build if any store reference survives.
  `autoSignDarwinBinariesHook` re-signs, because rewriting a load command
  invalidates the signature and arm64 macOS will not run an unsigned binary.
  (Note `otool -L`'s first line is the binary's own path; the check skips it.)

## What the workflow does

Triggers on `push` of a tag matching `[0-9]+.[0-9]+.[0-9]+`, plus
`workflow_dispatch` — a manual run does the builds and uploads artifacts but
skips publishing, so the cross-compiles can be exercised without minting a tag.

`build` job: 3-way matrix (Linux and Windows share one `ubuntu-latest`,
`aarch64-apple-darwin` gets `macos-latest`), each installing Nix +
FlakeHub cache the way `nightly.yml` does, then `nix build` and upload
`ctadl-<tag>-<target>[.exe]`.

`release` job: downloads all three, writes `<file>.sha256` in `sha256sum`
format (so `sha256sum -c` verifies), writes a `notes.md` preamble (what each
asset is, how to check the sum, `chmod +x`, and the macOS quarantine flag), and
runs `gh release create` with `--verify-tag --notes-file notes.md
--generate-notes`. Confirmed against gh's source that a supplied body and
`--generate-notes` combine rather than one overwriting the other: `--notes-file`
lands in the same `opts.Body` that the documented `--notes` prepend path reads.

## Versioning

Decided: one version for the whole workspace, set in one place and inherited
everywhere. A release tag then names exactly one number. Concretely —

- The root `Cargo.toml` carries `[workspace.package] version`, now "0.1.1".
  This is the only version to edit. Bumping it bumps the tree.
- Every member takes it with `version.workspace = true`: `ctadl-ascent`,
  `ctadl-flowy`, `ctadl-ir`, `dex-reader`, `immortal`, `jvm-reader`,
  `pcode-reader`, `rustc_graphviz`, `source-info`, `tailshare`, `trie`,
  `xtask`. Verified with `cargo metadata`: all twelve resolve to 0.1.1.
- Nothing is excluded from the workspace any more. That is what makes "all
  twelve" possible: `exclude` puts a package outside the workspace, and cargo
  then rejects `version.workspace = true` there with "failed to find a
  workspace root" (checked, not assumed). Exclusion was the reason this tree
  had more than one version in the first place.
- `rustc_graphviz` gives up the 0.0.0 it was vendored from rustc with. Keeping
  it would be the one exception to "every crate", and the version of a crate we
  neither wrote nor track upstream is not worth an exception.
- No `publish = false` anywhere, and nothing about publishing in CI. If one of
  these is ever published by hand, it goes out as the workspace version. (That
  would need one more edit first: crates.io rejects a bare `path` dependency, so
  each in-tree dep would need a `version` alongside its `path`. Left undone —
  nothing is published today, and adding it now would create a second place to
  edit on every bump, which is the thing this plan is against.)
- `ctadl --version` still prints the workspace version, by way of
  `#[command(name = "ctadl", version, about)]` in `ctadl-ascent`'s `main.rs`.
- The release workflow checks the tag against `ctadl --version` — run from the
  freshly built musl binary, which is static and runs on the runner as-is — and
  fails the release on a mismatch. Checking the binary rather than a manifest
  means what is checked is what ships.

The tree reads 0.1.1, so the first tag has to be `0.1.1`, or
`[workspace.package] version` needs another bump to whatever you tag.

## Keeping developer programs out of the distro

Dropping `exclude` costs the tree its old mechanism for that, so it needs a new
one. The new one is: **a program that must not ship is a cargo example, not a
bin.** `nix build .#packages.default` runs a bare `cargo build`, which builds
libs and bins but not examples. So an example is in the workspace — inherits the
version, gets tested, gets linted — and still stays out of the distro.

This is not an invention for this branch. `ctadl-ascent` already carries `flowy`,
`ir1`, and `tree-sitter-compile` as examples, and `source-info` carries four
more. Three programs joined them:

| was | is now |
| --- | --- |
| `dex-reader` bin (`src/main.rs`) | `dex-reader/examples/dex-reader.rs` |
| `jvm-reader` bin (`src/main.rs`) | `jvm-reader/examples/jvm-reader.rs` |
| `sarif-util`'s `show-regions` bin | `ctadl-ascent/examples/show-regions.rs` |

Build one with `cargo build --example dex-reader`; they land in
`target/<profile>/examples/`. `cargo test` builds all of them, so they cannot
rot unnoticed.

`sarif-util` is deleted. Its `show-regions` moved to `ctadl-ascent`, which
already had four of the five crates it needs (`clap`, `serde`, `serde_json`,
`url`); `percent-encoding` is new, and sits in `[dev-dependencies]` because
examples can see those and the shipped binary cannot. The rest of `sarif-util`
was a `Cargo.toml`, a `Cargo.lock`, a `.gitignore`, and a `src/main.rs` that
printed "Hello, world!". One cost worth naming: `show-regions` is an example
*of* `ctadl-ascent`, so building it now builds `ctadl-ascent`'s lib and its
whole dependency tree, where before it was a five-dependency crate on its own.

### What this cost in `flake.nix`

Four naersk derivations built the reader crates standalone (`src = ./dex-reader`).
That no longer works twice over: a workspace member cannot be built alone because
`version.workspace = true` needs the root manifest, and the thing being built is
no longer a bin. All four now take `src = ./.` and select with `--package`:

- `dex-reader` / `jvm-reader` (the programs `testEnv` puts on the regression
  suite's PATH) build with `--package <name> --lib --examples`. naersk installs
  whatever cargo reports as an executable and an example qualifies, so they still
  land at `$out/bin/<name>`.
- `--lib` rides along for naersk's benefit, not cargo's. naersk builds
  dependencies in a first pass over a stub source tree that has no `examples/`
  directory; `--examples` alone matches nothing there ("target filter `examples`
  specified, but no targets matched"), so that pass builds nothing and the
  dependency caching it exists for is lost. `--lib` gives it real work. This is
  the same stub-tree trap `nix/release.nix` documents for `--bin ctadl`.
- `dex-reader-tests` / `jvm-reader-tests` take `--package <name>` on both
  `cargoBuildOptions` and `cargoTestOptions`. Without it, `src = ./.` would test
  the entire workspace — a far larger build than either check wants.
- A new `workspaceVersion` reads `[workspace.package] version` out of
  `Cargo.toml` with `builtins.fromTOML`, so these derivations stay named after
  the version instead of naersk's fallback `unknown`. `nix/release.nix` takes
  it too, as a parameter rather than reading the manifest a second time, and
  pairs it with `name = binName`: all three release targets used to build as
  `rust-workspace-unknown` and now build as `ctadl-0.1.1` (and their shared
  dependency derivation as `ctadl-deps-0.1.1`). They share that name across
  targets -- still distinct derivations, and the workflow renames the artifacts
  per target anyway. (`packages.default` is untouched and still says
  `rust-workspace-unknown`; that predates this branch.)

  The name could not come from the root manifest the way the version does.
  Cargo has no name for a virtual workspace, which is why naersk falls back to
  the hardcoded string `rust-workspace`. So `binName = "ctadl"` is stated once
  in `nix/release.nix`'s `let` block and used four times: the derivation name,
  the `copyBinsFilter` that picks one artifact out of the build, and the two
  `$out/bin/<name>` paths the Darwin `postInstall` rewrites.

  Worth recording why naersk needed telling at all, since it looks like it
  should already know. Its fallback chain is
  `toplevelCargotoml.package.version or toplevelCargotoml."workspace.package".version
  or "unknown"`, and that middle term looks up an attribute whose name is the
  literal string `workspace.package`. `fromTOML` of `[workspace.package]`
  produces a *nested* attrset, so the literal key never exists. naersk means to
  read the workspace version and cannot.

`nix/release.nix` needed no change for the examples themselves. It selects
`.target.name == "ctadl"`, so an example could not reach the distro even if
something did build one.

`cargo test --workspace` now covers the reader crates' unit tests, which
exclusion used to prevent. `dex-reader-tests` is kept anyway — it runs on the
nightly's Nix builder with no toolchain assumptions and costs nothing.
`jvm-reader-tests` is not redundant at all: its `flow.rs` tests are `#[ignore]`d
and need both `JVM_READER_TEST_FIXTURES` and `--include-ignored`.

## Verification status

- **The workspace itself — verified.** `cargo metadata` reports twelve members,
  all 0.1.1, with `dex-reader`, `jvm-reader`, and `show-regions` as `example`
  targets rather than bins. `cargo clippy -p dex-reader -p jvm-reader
  -- -Dwarnings` passes, which is what the per-PR job now runs over them for the
  first time, and `cargo test --workspace --no-run` builds clean — that is the
  job that compiles the examples. `nix flake check --no-build` passes.
- **The `flake.nix` rework — all four derivations built on this Mac.**
  - `dex-reader` and `jvm-reader` each produce a `$out/bin/<name>` that runs and
    prints its usage. So naersk does install an example the same way it installs
    a bin — the one claim in the rework that reasoning alone could not settle.
    Both builds produced a `<name>-deps` derivation, which is the `--lib` trick
    working: the dependency pass found something to build.
  - `nix build .#checks.aarch64-darwin.{dex,jvm}-reader-tests` both pass. The
    jvm log shows `cargo test … --package jvm-reader -- --include-ignored` and
    compiles jvm-reader alone, so `--package` is doing its job — without it,
    `src = ./.` would have pulled in the whole workspace.
  - One cosmetic wart in the `-tests` logs: `jq: parse error: Invalid numeric
    literal`. naersk's install step tries to read the test run's output as the
    JSON build log it is not. Harmless, and it predates this branch — the old
    `mode = "test"` checks did the same.
- **aarch64-apple-darwin — done, verified.** Rebuilt from scratch five times
  now: after the move into `nix/release.nix`, after the first round of
  Cargo.toml changes, after the workspace rework and the bump to 0.1.1, after
  the derivation rename, and after the `binName` dedupe. Those last two are
  byte-identical -- same SHA-256, not merely the same size -- which is exactly
  what an edit that changes no build instruction should
  produce, and a pleasant side finding: this build is reproducible across two
  different derivations. The dedupe rebuild also reused `ctadl-deps` rather than
  rebuilding it, confirming the note above.

  Every time: `$out/bin` holds only `ctadl` — no `xtask`, and now no examples
  either; `otool -L` shows only CoreFoundation,
  `/usr/lib/libiconv.2.dylib`, `/usr/lib/libSystem.B.dylib`; `nix path-info -r`
  on the output returns exactly one path, its own, so nothing in the store is
  retained at runtime; copied out of the store it runs and reports
  `ctadl 0.1.1`. 146 MB under thin LTO, 112 MB under fat.

  That last line is the version plan working end to end: `[workspace.package]
  version` was edited, nothing else was, and the shipped binary says 0.1.1.
- **What fat LTO costs and buys — measured, on this Mac (20 cores).** Both
  builds ran on the same machine, one after the other, from the same tree; the
  only difference between them is `CARGO_PROFILE_RELEASE_LTO`. Each stage was
  timed on its own, `ctadl-deps` first and then the workspace build that
  consumes it.

  | | thin | fat | change |
  | --- | --- | --- | --- |
  | `ctadl-deps` | 1m52s | 1m53s | none to speak of |
  | workspace + link | 1m33s | 12m27s | 8.0x |
  | both, cold | 3m26s | 14m19s | 4.2x |
  | binary | 146,151,792 B | 112,229,872 B | -33,921,920 B, -23.2% |

  The dependency stage costing the same either way is the expected result and
  worth stating: cargo compiles dependencies to bitcode under thin LTO too, so
  fat LTO adds no work there. All of it lands in the final link, which is why
  one stage absorbs the whole 11 minutes.

  A caveat on reading those minutes: GitHub's runners have a few cores, not 20,
  and the LTO phase is largely serial, so CI will pay more than 11 minutes
  wall-clock. The `timeout-minutes: 180` the build job already carries leaves
  room for it. The two cross targets are unmeasured for the usual reason --
  this Mac cannot build them.
- **x86_64-unknown-linux-musl / x86_64-pc-windows-gnu — evaluated, not built.**
  Both `.#legacyPackages.x86_64-linux.release."…"` derivations now evaluate
  cleanly *as an x86_64-linux builder would evaluate them* (evaluation is
  cross-platform even though building is not), and their derivations carry the
  right Ubuntu-side wiring:

  | | musl | mingw |
  | --- | --- | --- |
  | `system` | `x86_64-linux` | `x86_64-linux` |
  | linker | `x86_64-unknown-linux-musl-cc` | `x86_64-w64-mingw32-cc` |
  | `CC_<target>` | `x86_64-unknown-linux-musl-cc` | `x86_64-w64-mingw32-cc` |
  | `HOST_CC` | Linux `gcc-wrapper-15.2.0/bin/cc` | same |

  That last row is the point: `HOST_CC` resolves to the *Linux* gcc, so nothing
  here is quietly reaching for this Mac's clang. What is still unproven is that
  the compile and link actually succeed, which needs a Linux builder.
- **`ctadl.exe` — settled by reading naersk.** `build.nix`'s installPhase copies
  each artifact to `$out/bin/$(basename "$bin_path")`, and cargo reports
  `ctadl.exe` for the windows-gnu target. The workflow's `matrix.suffix` is
  right.

## To resume

1. Build the two cross targets on x86_64 Linux (matching CI):

   ```
   nix build '.#legacyPackages.x86_64-linux.release."x86_64-unknown-linux-musl"'
   nix build '.#legacyPackages.x86_64-linux.release."x86_64-pc-windows-gnu"'
   ```

   This Mac cannot: no `/etc/nix/machines`, no Docker/podman/colima, no remote
   builder in `nix.conf`. Cross-compiling them *from* Darwin instead would
   exercise a different derivation than CI builds, so it proves little.

2. Sanity-check the Linux binary is actually static (`file` should say
   "statically linked").

3. Push the branch and run the workflow via `workflow_dispatch` before tagging
   anything. That is the only true test of the CI path, and given (1) it is the
   cheapest way to get one. Nothing is pushed yet.

   Note that a `workflow_dispatch` run does *not* exercise the `release` job
   (it is gated on `refs/tags/`), so the tag check, the checksums, the notes,
   and `gh release create` stay unproven until a real tag. Reading them is the
   only review they get before the first release.

## Open questions / caveats

- **Stripping.** The binary is 112 MB, down from 146 MB now that fat LTO is on.
  Three of them per release. Note the
  build log already says `stripping (with command strip and flags -S)`: nixpkgs'
  fixupPhase runs, and `[profile.release] strip = "debuginfo"` is set in
  `Cargo.toml`, so debug info is gone already and the 112 MB is what remains.
  Stripping further (`strip = "symbols"`) would cost symbolized panic
  backtraces. Left as is. `codegen-units = 1` is the other knob of this kind
  and is not set; it would shrink the binary further and lengthen the build
  again. Not tried.
- **Three toolchains.** `rust-toolchain.toml` pins 1.94.1, the release builds
  use fenix's `stable` (pinned by `flake.lock`), and the rest of the flake uses
  nixpkgs' rustc. Pinning fenix to 1.94.1 is possible but needs a hash.
- **Runner disk.** macOS runners have ~14 GB free; the naersk deps output alone
  is a 1 GB closure. It should fit, but a disk-cleanup step may become necessary.
- **Rebuild sensitivity.** `src = ./.` is the whole flake source, so editing
  *any tracked file* invalidates the ctadl build and recompiles the workspace --
  `flake.nix`, `nix/release.nix`, and this document alike. Measured, not
  assumed: a comment-only edit to `nix/release.nix` changed the derivation, and
  a `nix derivation show` diff of before and after showed exactly two
  differences, `src` and `out`. Every build instruction was byte-identical.
  What is not invalidated is the `ctadl-deps` derivation, which naersk builds
  from a stub tree of manifests and the lockfile; it survives edits to anything
  else, so a rebuild after one of these is the workspace only, not the whole
  dependency graph.

  This is pre-existing (`packages.default` does the same), not new here, but it
  makes local iteration expensive, and it is worth knowing before starting a
  build: finish editing first, because a one-character change while a build runs
  means the finished build is not the tree you have.
