# GitHub release builds — work in progress

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

`nix/release.nix` and `.github/workflows/release.yml` are `git add -N`'d. They
have to be at least intent-to-add or Nix cannot see them: a flake's source is
its *tracked* git tree, so an untracked file does not exist as far as
`nix build` is concerned.

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

- The root `Cargo.toml` keeps `[workspace.package] version = "0.1.0"`. This is
  the only version to edit. Bumping it bumps the tree.
- Every member takes it with `version.workspace = true`: `ctadl-ascent`,
  `ctadl-flowy`, `ctadl-ir`, `dex-reader`, `immortal`, `jvm-reader`,
  `pcode-reader`, `rustc_graphviz`, `source-info`, `tailshare`, `trie`,
  `xtask`. Verified with `cargo metadata`: all twelve resolve to 0.1.0.
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

The tree reads 0.1.0, so the first tag has to be `0.1.0`, or
`[workspace.package] version` needs a bump to whatever you tag.

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
  the version instead of naersk's fallback `unknown`. (`packages.default` still
  says `rust-workspace-unknown`; that predates this branch and changing it would
  force a full rebuild for a cosmetic name.)

`nix/release.nix` needed no change. It selects `.target.name == "ctadl"`, so
examples could not reach the distro even if something did build them.

`cargo test --workspace` now covers the reader crates' unit tests, which
exclusion used to prevent. `dex-reader-tests` is kept anyway — it runs on the
nightly's Nix builder with no toolchain assumptions and costs nothing.
`jvm-reader-tests` is not redundant at all: its `flow.rs` tests are `#[ignore]`d
and need both `JVM_READER_TEST_FIXTURES` and `--include-ignored`.

## Verification status

- **The workspace itself — verified.** `cargo metadata` reports twelve members,
  all 0.1.0, with `dex-reader`, `jvm-reader`, and `show-regions` as `example`
  targets rather than bins. `cargo clippy -p dex-reader -p jvm-reader
  -- -Dwarnings` passes, which is what the per-PR job now runs over them for the
  first time, and `cargo test --workspace --no-run` builds clean — that is the
  job that compiles the examples. `nix flake check --no-build` passes.
- **The naersk example build — verified on this Mac.** Built the reworked
  `dex-reader` derivation for real. It produces `$out/bin/dex-reader`, and the
  binary runs and prints its usage. So naersk does install an example the same
  way it installs a bin; that was the one claim in the `flake.nix` rework that
  reasoning alone could not settle.
- **What that leaves unproven in `flake.nix`.** `jvm-reader` is the same shape as
  `dex-reader` and was not built. Neither `-tests` check was run, so the
  `--package` on `cargoTestOptions` is read, not exercised. All four evaluate.
- **aarch64-apple-darwin — done, verified.** Rebuilt from scratch after the move
  into `nix/release.nix` and again after the first round of Cargo.toml changes.
  Not rebuilt since the workspace rework. That rework forces a recompile (it
  changes `rustc_graphviz`'s version) but should not change the result: the
  reported version is 0.1.0 either way, `cargo build` still ignores examples, and
  the `.target.name == "ctadl"` filter still admits exactly one binary. Worth one
  rebuild to confirm `$out/bin` has not grown. `$out/bin`
  holds only `ctadl` (no `xtask`); `otool -L` shows only CoreFoundation,
  `/usr/lib/libiconv.2.dylib`, `/usr/lib/libSystem.B.dylib`; `nix path-info -r`
  on the output returns exactly one path, its own, so nothing in the store is
  retained at runtime; copied out of the store it runs and reports
  `ctadl 0.1.0`. 146 MB.
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

3. Rebuild the darwin release target and confirm `$out/bin` still holds `ctadl`
   alone — the workspace rework should not have changed that, but it is one
   command and it is the thing the whole examples-not-bins arrangement exists to
   guarantee:

   ```
   nix build '.#legacyPackages.aarch64-darwin.release."aarch64-apple-darwin"'
   ls result/bin
   ```

   Same for the reworked reader derivations, which the nightly depends on:

   ```
   nix build '.#checks.aarch64-darwin.dex-reader-tests' '.#checks.aarch64-darwin.jvm-reader-tests'
   ```

4. Push the branch and run the workflow via `workflow_dispatch` before tagging
   anything. That is the only true test of the CI path, and given (1) it is the
   cheapest way to get one. Nothing is committed yet — that was deliberate.

   Note that a `workflow_dispatch` run does *not* exercise the `release` job
   (it is gated on `refs/tags/`), so the tag check, the checksums, the notes,
   and `gh release create` stay unproven until a real tag. Reading them is the
   only review they get before the first release.

## Open questions / caveats

- **Stripping.** The binary is 146 MB. Three of them per release. Note the
  build log already says `stripping (with command strip and flags -S)`: nixpkgs'
  fixupPhase runs, and `[profile.release] strip = "debuginfo"` is set in
  `Cargo.toml`, so debug info is gone already and the 146 MB is what remains.
  Stripping further (`strip = "symbols"`) would cost symbolized panic
  backtraces. Left as is.
- **Three toolchains.** `rust-toolchain.toml` pins 1.94.1, the release builds
  use fenix's `stable` (pinned by `flake.lock`), and the rest of the flake uses
  nixpkgs' rustc. Pinning fenix to 1.94.1 is possible but needs a hash.
- **Runner disk.** macOS runners have ~14 GB free; the naersk deps output alone
  is a 1 GB closure. It should fit, but a disk-cleanup step may become necessary.
- **Rebuild sensitivity.** `src = ./.` is the whole flake source, so editing
  `flake.nix` invalidates the ctadl build and recompiles the workspace. That is
  pre-existing (`packages.default` does the same), not new here, but it makes
  local iteration on the flake expensive.
